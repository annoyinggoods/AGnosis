//! The step-indexed timeline: what plays when.
//!
//! WHY the scene's clock: one slot on the strip is one **sprite step**,
//! and a step is 1/6 s (the gait's frame rate). The strip is as long as
//! the crossing window — the time the train needs to cross the screen
//! and for the trailing duck to clear the left edge — so the strip loops
//! with the parade (roadmap Extension 3 §1, §3.3). A note in slot *k*
//! fires on the step where the scene's clock reads *k*: the sequencer
//! cannot drift away from the animation, because it *is* the animation's
//! clock.
//!
//! WHY a sparse map: the strip is ~263 slots long and a hand-made track
//! holds a couple of dozen notes, so the track is a map from the step
//! index to the note rather than a dense array of empty slots.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The gait's frames per second: one strip slot is one sprite step, so a
/// step lasts 1/6 s. The number lives in [`crate::gait`], so the scene
/// and the sequencer cannot drift apart.
use crate::gait::GAIT_FPS;

/// The longest legal track name.
pub const MAX_NAME_LEN: usize = 24;

/// The document version this build writes and reads.
const TRACK_VERSION: u32 = 1;

/// One MIDI note on the strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Note {
    /// MIDI pitch, 0..=127.
    pub pitch: u8,
    /// How many sprite steps the note rings for (>= 1).
    pub duration_steps: u8,
    /// MIDI velocity, 0..=127.
    pub velocity: u8,
}

/// A sparse track: step index -> note. The strip is ~263 slots long, so a
/// sparse map keeps a 20-note track at 20 entries.
pub type Track = BTreeMap<usize, Note>;

/// The step-indexed timeline: what plays when.
pub struct Sequencer {
    track: Track,
    strip_steps: usize,
    last_step: Option<usize>,
}

impl Sequencer {
    /// A sequencer over a strip of `strip_steps` steps.
    pub fn new(strip_steps: usize) -> Self {
        Self {
            track: Track::new(),
            strip_steps: strip_steps.max(1),
            last_step: None,
        }
    }

    /// Replace the track.
    ///
    /// The clock is **not** reset: the notes of the new track fire on
    /// the steps they sit on, and the parade does not replay the strip
    /// from the top when the user swaps the track.
    ///
    /// A note whose step lies **past the strip** (a track saved against
    /// a longer strip) is folded to `step % strip_steps` — the same slot
    /// it would fire on when the strip loops, instead of never firing
    /// at all (audit 2026-10-23 §prior-55).
    pub fn set_track(&mut self, track: Track) {
        self.track = normalise_track(&track, self.strip_steps);
    }

    /// The current track.
    pub fn track(&self) -> &Track {
        &self.track
    }

    /// The strip's length in steps.
    pub fn strip_steps(&self) -> usize {
        self.strip_steps
    }

    /// The notes to fire for every step after the last one, up to and
    /// including `step`. The strip loops: step `k` of every loop fires
    /// once.
    ///
    /// The first call fires the notes from the top of the strip up to
    /// `step` — the sequencer has no history before it was created, so it
    /// treats the whole strip as pending once.
    pub fn advance_to(&mut self, step: usize) -> Vec<Note> {
        let strip = self.strip_steps;
        let start = match self.last_step {
            Some(last) => last.saturating_add(1),
            None => 0,
        };
        self.last_step = Some(step);
        if step < start {
            // The clock went backwards: rebase and fire nothing.
            return Vec::new();
        }
        let span = step - start + 1;
        if span >= strip {
            // The range covers the whole strip, so every note fires once.
            return self.track.values().copied().collect();
        }
        let mut fired = Vec::new();
        for current in start..=step {
            if let Some(note) = self.track.get(&(current % strip)) {
                fired.push(*note);
            }
        }
        fired
    }
}

/// The number of sprite steps in one crossing window: the time the train
/// needs to cross the screen and for the trailing duck to clear the left
/// edge, measured in the scene's gait steps (6 per second).
///
/// The train's length is a **parameter**, not a number baked in here: the
/// train's layout belongs to [`crate::parade`], and a copy of its length
/// in the audio module is a copy that drifts.
pub fn crossing_window_steps(cols: usize, loop_seconds: f32, train_len_cells: f32) -> usize {
    if cols == 0 || !loop_seconds.is_finite() || loop_seconds <= 0.0 {
        return 0;
    }
    if !train_len_cells.is_finite() || train_len_cells < 0.0 {
        return 0;
    }
    let speed = cols as f32 / loop_seconds;
    let window = cols as f32 + train_len_cells;
    (window / speed * GAIT_FPS) as usize
}

/// The track file: `<dir>/<name>.json`.
pub fn track_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{}.json", safe_stem(name)))
}

/// Save a track atomically (temp file + rename), keeping a `.bak`.
pub fn save_track(dir: &Path, name: &str, track: &Track) -> Result<(), TrackError> {
    validate_name(name)?;
    std::fs::create_dir_all(dir).map_err(|source| TrackError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let doc = TrackDoc {
        version: TRACK_VERSION,
        notes: track
            .iter()
            .map(|(step, note)| TrackNote {
                step: *step,
                pitch: note.pitch,
                duration_steps: note.duration_steps,
                velocity: note.velocity,
            })
            .collect(),
    };
    let path = track_path(dir, name);
    let text = serde_json::to_string_pretty(&doc).map_err(|source| TrackError::Json {
        path: path.clone(),
        source,
    })?;
    write_atomic(&path, &text)
}

/// Load a track. A malformed file is an `Err`, never a panic.
///
/// The file is returned **verbatim**: a note whose step lies past the
/// caller's strip is folded where the strip is known —
/// [`normalise_track`] (or [`Sequencer::set_track`], which folds on its
/// own). The editor's strip and the scene's sequencer both know their
/// length; the file alone does not.
pub fn load_track(dir: &Path, name: &str) -> Result<Track, TrackError> {
    validate_name(name)?;
    let path = track_path(dir, name);
    let text =
        std::fs::read_to_string(&path).map_err(|source| io_or_missing(&path, name, source))?;
    let doc: TrackDoc = serde_json::from_str(&text).map_err(|source| TrackError::Json {
        path: path.clone(),
        source,
    })?;
    if doc.version != TRACK_VERSION {
        return Err(TrackError::UnsupportedVersion {
            version: doc.version,
            supported: TRACK_VERSION,
        });
    }
    Ok(build_track(doc.notes, 0))
}

/// The names of the tracks in `dir`, sorted.
///
/// A missing folder is not an error: it simply holds no tracks.
pub fn list_tracks(dir: &Path) -> Vec<String> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return Vec::new(),
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| track_stem(&entry.path()))
        .collect();
    names.sort();
    names
}

/// A track-file failure.
#[derive(Debug, thiserror::Error)]
pub enum TrackError {
    /// The name is not a legal track name (no path traversal, ever).
    #[error(
        "illegal track name {name:?} (use a-z, 0-9, '-' or '_', max {MAX_NAME_LEN} characters)"
    )]
    InvalidName { name: String },
    /// A file could not be read or written.
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The file is not a valid track document.
    #[error("invalid json at {path}: {source}")]
    Json {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    /// The document was written by a newer build.
    #[error("unsupported track version {version} (this build supports version {supported})")]
    UnsupportedVersion { version: u32, supported: u32 },
    /// No track of that name exists.
    #[error("no track named {name:?}")]
    NotFound { name: String },
}

/// The on-disk document: the version and the notes as a list.
///
/// The notes are a list and not a map because the JSON keys of a map are
/// strings, and a step index is a number.
#[derive(Debug, Serialize, Deserialize)]
struct TrackDoc {
    version: u32,
    notes: Vec<TrackNote>,
}

/// One note on the strip, as it is written to disk.
#[derive(Debug, Serialize, Deserialize)]
struct TrackNote {
    step: usize,
    pitch: u8,
    duration_steps: u8,
    velocity: u8,
}

/// Rebuild the track from the document's notes.
///
/// A duplicate step keeps the **last** note: the map is the model, and a
/// step carries exactly one note. When `strip_steps` is known, a step
/// past the strip is folded to `step % strip_steps` (audit 2026-10-23
/// §prior-55): a note saved against a longer strip fires on the slot it
/// owns in this strip's loop instead of never firing at all.
fn build_track(notes: Vec<TrackNote>, strip_steps: usize) -> Track {
    let mut track = Track::new();
    for note in notes {
        let step = if strip_steps == 0 {
            note.step
        } else {
            note.step % strip_steps
        };
        track.insert(
            step,
            Note {
                pitch: note.pitch,
                duration_steps: note.duration_steps,
                velocity: note.velocity,
            },
        );
    }
    track
}

/// Fold a track's steps into `0..strip_steps`.
///
/// The strip **loops**, so a step past its end is the slot it wraps
/// onto — and the fold is what the looping lookup in
/// [`Sequencer::advance_to`] already assumes: a note keyed past the
/// strip is a note the lookup can never reach. A collision (two old
/// steps wrapping onto one slot) keeps the **later** note, like a
/// duplicate in one file does. `strip_steps == 0` returns the track
/// unchanged — there is nothing to fold onto.
pub fn normalise_track(track: &Track, strip_steps: usize) -> Track {
    if strip_steps == 0 {
        return track.clone();
    }
    let mut folded = Track::new();
    for (step, note) in track {
        folded.insert(step % strip_steps, *note);
    }
    folded
}

/// The name of a track file without its extension, when it is one.
fn track_stem(path: &Path) -> Option<String> {
    if !path.is_file() {
        return None;
    }
    if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
        return None;
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
}

/// A track name is a-z, 0-9, '-' or '_' — so a name can never escape the
/// track folder.
pub fn validate_name(name: &str) -> Result<(), TrackError> {
    let legal_char = |c: char| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_';
    let legal = !name.is_empty() && name.len() <= MAX_NAME_LEN && name.chars().all(legal_char);
    if !legal {
        return Err(TrackError::InvalidName {
            name: name.to_string(),
        });
    }
    Ok(())
}

/// The file name a track path is built from: the name without any
/// directory part, so a path can never leave the track folder.
fn safe_stem(name: &str) -> &str {
    Path::new(name)
        .file_name()
        .and_then(|stem| stem.to_str())
        .unwrap_or("track")
}

/// Turn a missing file into "not found" and everything else into an I/O
/// error.
fn io_or_missing(path: &Path, name: &str, source: std::io::Error) -> TrackError {
    if source.kind() == std::io::ErrorKind::NotFound {
        return TrackError::NotFound {
            name: name.to_string(),
        };
    }
    TrackError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Write `text` to `path` atomically, keeping a backup of the old file.
///
/// WHY atomic: a plain write truncates the file first, so a crash
/// mid-write leaves a half-written track behind. Writing a temp file and
/// renaming it over the target is atomic on every platform the app
/// supports: the old file is either fully there or fully replaced.
fn write_atomic(path: &Path, text: &str) -> Result<(), TrackError> {
    let temp = temp_path(path);
    std::fs::write(&temp, text).map_err(|source| TrackError::Io {
        path: temp.clone(),
        source,
    })?;
    if path.exists() {
        keep_backup(path);
    }
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(source) => {
            let _ = std::fs::remove_file(&temp);
            Err(TrackError::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    }
}

/// The temp file a write goes through: a sibling of the target, named
/// after it and the writing process, so two writers do not collide.
fn temp_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "track.json".to_string());
    sibling(path, &format!("{name}.{}.tmp", std::process::id()))
}

/// The backup file the write keeps next to the target.
fn backup_path(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "track.json".to_string());
    sibling(path, &format!("{name}.bak"))
}

/// Copy the old file aside. Best effort: a failure to copy must not stop
/// the write.
fn keep_backup(path: &Path) {
    let _ = std::fs::copy(path, backup_path(path));
}

/// A sibling path of `path` with the given file name.
fn sibling(path: &Path, name: &str) -> PathBuf {
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temp folder that cleans itself up.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let mut path = std::env::temp_dir();
            path.push(format!("ducks-track-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A track with one note on step 5.
    fn one_note() -> Track {
        let mut track = Track::new();
        track.insert(
            5,
            Note {
                pitch: 60,
                duration_steps: 2,
                velocity: 100,
            },
        );
        track
    }

    #[test]
    fn a_note_fires_on_its_step() {
        let mut sequencer = Sequencer::new(263);
        sequencer.set_track(one_note());

        let fired = sequencer.advance_to(5);
        assert_eq!(fired.len(), 1, "the note must fire on its own step");
        assert_eq!(fired[0].pitch, 60);

        // Nothing fires on the steps in between.
        let mut quiet = Sequencer::new(263);
        quiet.set_track(one_note());
        assert!(quiet.advance_to(4).is_empty());

        // The strip loops: the same slot fires again one loop later.
        let fired = sequencer.advance_to(5 + 263);
        assert_eq!(fired.len(), 1, "the note must fire again one loop later");
        assert_eq!(fired[0].pitch, 60);
    }

    #[test]
    fn the_strip_length_is_the_crossing_window() {
        // 960 columns, a 20 s loop, 6 gait steps per second and the
        // train's own length (1145 cells).
        assert_eq!(crossing_window_steps(960, 20.0, 1145.0), 263);
        // A broken input is refused, not turned into a huge strip.
        assert_eq!(crossing_window_steps(0, 20.0, 1145.0), 0);
        assert_eq!(crossing_window_steps(960, 0.0, 1145.0), 0);
        assert_eq!(crossing_window_steps(960, f32::NAN, 1145.0), 0);
        assert_eq!(crossing_window_steps(960, 20.0, f32::NAN), 0);
    }

    #[test]
    fn a_track_round_trips_through_json() {
        let dir = TempDir::new("roundtrip");
        let mut track = Track::new();
        track.insert(
            0,
            Note {
                pitch: 48,
                duration_steps: 1,
                velocity: 64,
            },
        );
        track.insert(
            130,
            Note {
                pitch: 72,
                duration_steps: 3,
                velocity: 127,
            },
        );

        save_track(dir.path(), "demo", &track).expect("save");
        let loaded = load_track(dir.path(), "demo").expect("load");
        assert_eq!(loaded, track);
        assert_eq!(list_tracks(dir.path()), vec!["demo".to_string()]);
    }

    #[test]
    fn a_broken_track_is_ignored() {
        let dir = TempDir::new("broken");
        std::fs::write(dir.path().join("bad.json"), "{ this is not json").expect("write");
        assert!(load_track(dir.path(), "bad").is_err());
        // A missing track is an error, not a panic.
        assert!(load_track(dir.path(), "missing").is_err());
        // A version this build does not know is refused.
        std::fs::write(
            dir.path().join("future.json"),
            "{\"version\": 99, \"notes\": []}",
        )
        .expect("write");
        assert!(matches!(
            load_track(dir.path(), "future"),
            Err(TrackError::UnsupportedVersion { .. })
        ));
    }

    #[test]
    fn a_track_name_cannot_escape_its_folder() {
        let dir = TempDir::new("escape");
        assert!(save_track(dir.path(), "../evil", &Track::new()).is_err());
        assert!(load_track(dir.path(), "..\\evil").is_err());
        // The path helper never leaves the folder either.
        let path = track_path(dir.path(), "../evil");
        assert_eq!(path.parent(), Some(dir.path()));
    }

    #[test]
    fn a_note_beyond_the_strip_fires_on_its_wrapped_step() {
        // The pick, pinned (audit 2026-10-23 §prior-55): a note saved
        // against a longer strip fires at `step % strip_steps` — the
        // slot it owns in this strip's loop — instead of never firing.
        let mut wide = Track::new();
        wide.insert(
            300,
            Note {
                pitch: 60,
                duration_steps: 1,
                velocity: 100,
            },
        );
        let folded = normalise_track(&wide, 263);
        assert_eq!(
            folded.keys().copied().collect::<Vec<_>>(),
            vec![300 % 263],
            "the step folds onto its wrapped slot"
        );
        // The sequencer folds on its own when the track is set.
        let mut sequencer = Sequencer::new(263);
        sequencer.set_track(wide.clone());
        assert_eq!(
            sequencer.advance_to(37).len(),
            1,
            "the wrapped note fires on its slot"
        );
        // A zero-length strip leaves the track alone.
        assert_eq!(normalise_track(&wide, 0), wide);
    }

    #[test]
    fn the_saving_keeps_a_backup() {
        let dir = TempDir::new("backup");
        save_track(dir.path(), "demo", &one_note()).expect("save");
        save_track(dir.path(), "demo", &one_note()).expect("save again");
        assert!(dir.path().join("demo.json.bak").exists());
    }
}
