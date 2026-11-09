//! The seam between the pure core and the platform.
//!
//! Everything the app does that is **not** a pure function of its
//! inputs lives here: the platform's config folder, the platform's
//! drawn-asset folders, the platform's default hotkey chord, the
//! platform's track folder and the platform's backdrop folder.
//!
//! The rule this module enforces is one sentence long: **a function
//! here resolves a platform location or a platform-specific default, and
//! nothing else does**. The core modules — the config model, the
//! drawn-asset model, the scene, the package, the exports — stay
//! platform-free, so they can be compiled for a platform the app was
//! never built for (the web build, `wasm32-unknown-unknown`, where
//! there is no config dir and no filesystem at all).
//!
//! The functions here are **thin**: each one resolves a location and
//! then hands the work to a core function that already takes its
//! location as a parameter (`Config::load_from`, `drawn::save_to`,
//! `TargetDirs`, …). That is what keeps the two halves from drifting:
//! there is exactly one implementation of the *work*, and the platform
//! only decides *where*.
//!
//! The one exception is the hotkey chord: the *value* is the user's,
//! but the *key* it comes from depends on the platform, so the choice
//! is made here and nowhere else.

use std::path::{Path, PathBuf};

use agnosis_core::assets::{AssetDirs, DrawnDirs};
use agnosis_core::config::{BackdropConfig, Config, ConfigError, HotkeyConfig};
use agnosis_core::drawn::{DrawnError, DrawnKind, DrawnSprite};
use agnosis_core::package::{PackageError, TargetDirs};

// ---------------------------------------------------------------------------
// The config file
// ---------------------------------------------------------------------------

/// Load the platform's config.
///
/// A missing file yields the defaults and is written out, so the user
/// gets a real file to edit on the first run.
///
/// The load runs the **repair pass**, and the repair pass needs to know
/// whether a drawn asset exists — so this shell hands over its own
/// predicate ([`drawn_exists`]).
pub fn load_config() -> Result<Config, ConfigError> {
    let path = crate::paths::config_file()?;
    if !path.exists() {
        let cfg = Config::default();
        report_save(save_config(&cfg));
        return Ok(cfg);
    }
    Config::load_from(&path, drawn_exists)
}

/// Load the platform's config **as the user wrote it**: no repair pass.
///
/// The editor's ASSIGN panel uses this. The *sanitizing* loader drops
/// every assignment whose drawn asset does not exist (yet) — a soft
/// repair that is right for the runtime and wrong for an editor that
/// writes the table back, because it turned an unrelated assignment
/// into a deletion (audit 2026-10-20 §3.3).
pub fn load_config_for_edit() -> Result<Config, ConfigError> {
    let path = crate::paths::config_file()?;
    if !path.exists() {
        return Ok(Config::default());
    }
    Config::load_unsanitized_from(&path)
}

/// Write the config into the platform's config file.
///
/// The seam's write is **internal**: the only caller is the first-run
/// bootstrap in [`load_config`], so the surface stays as narrow as the
/// callers make it (audit 2026-10-23 §F-120). A verb that must save
/// does it through the core's own writers.
fn save_config(cfg: &Config) -> Result<(), ConfigError> {
    let path = crate::paths::config_file()?;
    cfg.save_to(&path)
}

/// Report a failed config write (audit nesting rule): the report lives
/// in its own helper so the caller stays flat.
///
/// `warn`, not `trace`: a save failure is a real problem for the user,
/// and the daemon nulls its stdio, so only the `AGNOSIS_TRACE_FILE`
/// channel survives (audit §Robustness).
fn report_save(result: Result<(), ConfigError>) {
    if let Err(e) = result {
        agnosis_core::trace::warn(&format!("config.save error: {e}"));
    }
}

// ---------------------------------------------------------------------------
// The hotkey chord
// ---------------------------------------------------------------------------

/// The chord spec for the platform we are running on.
///
/// The *chords* are the user's, but the **key** is the platform's: the
/// hotkey backends differ, so each platform has its own string. The
/// `windows` chord is the fallback for a platform that has no chord of
/// its own.
pub fn hotkey_for_current_platform(cfg: &HotkeyConfig) -> &str {
    if cfg!(target_os = "windows") {
        &cfg.windows
    } else if cfg!(target_os = "linux") {
        &cfg.linux
    } else if cfg!(target_os = "macos") {
        &cfg.macos
    } else {
        &cfg.windows
    }
}

// ---------------------------------------------------------------------------
// The clock
// ---------------------------------------------------------------------------

/// Today's date as `YYYY-MM-DD`, in UTC.
///
/// The **core has no clock**: the standard library's `SystemTime::now`
/// **panics** on `wasm32-unknown-unknown` ("time not implemented on
/// this platform") and the release profile aborts on a panic, so the
/// core must not read one (audit 2026-10-22 §P1-7). The clock belongs
/// to the shell; the **calendar** — the date arithmetic — belongs to
/// the core, which is what `stats::date_from_unix_seconds` is for.
///
/// The `unix` fallback is the epoch itself, so a host with no clock
/// still produces a well-formed date instead of panicking.
pub fn today() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    agnosis_core::stats::date_from_unix_seconds(seconds)
}

// ---------------------------------------------------------------------------
// The drawn assets
// ---------------------------------------------------------------------------

/// The platform folder that holds the assets of `kind`.
///
/// The folder names live in [`crate::paths`] — one owner for every
/// location (audit maintainability) — so this module never spells a
/// path itself. **Internal**: every caller is in this module
/// (audit 2026-10-23 §F-120).
fn drawn_dir_for(kind: DrawnKind) -> Result<PathBuf, DrawnError> {
    match kind {
        DrawnKind::Sprite => crate::paths::drawn_sprites_dir(),
        DrawnKind::Balloon => crate::paths::drawn_balloons_dir(),
        DrawnKind::Grass => crate::paths::drawn_grass_dir(),
        DrawnKind::Decoration => crate::paths::drawn_decorations_dir(),
    }
    .map_err(|source| DrawnError::Path { source })
}

/// Save an asset into the platform's drawn-asset folder.
///
/// The name is validated first (inside `drawn::save_to`), so a name can
/// never escape the asset folder.
pub fn drawn_save(
    sprite: &DrawnSprite,
    kind: DrawnKind,
    name: &str,
) -> Result<PathBuf, DrawnError> {
    let dir = drawn_dir_for(kind)?;
    agnosis_core::drawn::save_to(&dir, sprite, kind, name)
}

/// The assets of `kind`, **from the cache**: the name and the size
/// class of every file in the folder.
///
/// The same list the existence predicate keeps, so a caller on the UI
/// thread (the editor's key handler) does not walk the user's asset
/// folder on every keystroke (audit 2026-10-21 §G18).
///
/// The cache is invalidated by the folder's modification time, so a
/// newly drawn asset shows up right away. A folder that cannot be
/// resolved yields an empty list.
pub fn drawn_list_cached(kind: DrawnKind) -> Vec<agnosis_core::drawn::AssetEntry> {
    let Ok(dir) = drawn_dir_for(kind) else {
        return Vec::new();
    };
    let index = kind_index(kind);
    DRAWN_NAME_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        refresh_names(&mut cache[index], &dir);
        match cache[index].as_ref() {
            Some(entry) => entry.names.clone(),
            None => Vec::new(),
        }
    })
}

/// Does a drawn asset of `kind` exist in the user's folders?
///
/// This is the **existence predicate** the config's repair pass asks: it
/// drops an assignment whose asset is gone (a *soft* repair — the file
/// may appear later). The core cannot answer the question itself, so it
/// is injected: the shell owns the folders, the core owns the rule.
///
/// A folder that cannot be read yields `false`: the same answer an empty
/// list gives.
///
/// The folders are **not** walked once per question. The repair pass asks
/// once per assignment, so the list of names of a kind is read **once**
/// and kept until the folder changes (see the name cache below).
pub fn drawn_exists(kind: DrawnKind, name: &str) -> bool {
    let Ok(dir) = drawn_dir_for(kind) else {
        return false;
    };
    let index = kind_index(kind);
    DRAWN_NAME_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        list_carries(&mut cache[index], &dir, name)
    })
}

/// The slot of `kind` in the cache.
///
/// The match is **exhaustive**, so a new kind cannot slip past the
/// cache and overwrite another kind's list.
fn kind_index(kind: DrawnKind) -> usize {
    match kind {
        DrawnKind::Sprite => 0,
        DrawnKind::Balloon => 1,
        DrawnKind::Grass => 2,
        DrawnKind::Decoration => 3,
    }
}

/// The names of one kind's folder, and the moment they were read.
struct DrawnNames {
    /// The names the folder held, with their size class.
    names: Vec<agnosis_core::drawn::AssetEntry>,
    /// The folder's modification time when the names were read.
    ///
    /// Adding or removing a file bumps the folder's timestamp, and
    /// nothing else changes the list of names — so the timestamp is
    /// exactly the signal that the list is stale.
    stamp: Option<std::time::SystemTime>,
}

// The per-thread cache of the drawn-asset name lists.
//
// The cache exists so the repair pass walks a folder **once** and not
// once per assignment. It is invalidated by the folder's modification
// time, so a newly drawn asset is picked up right away and the walk
// still happens only once per repair pass.
thread_local! {
    static DRAWN_NAME_CACHE: std::cell::RefCell<[Option<DrawnNames>; 4]> =
        const { std::cell::RefCell::new([None, None, None, None]) };
}

/// Does the cached list of `slot` carry `name`?
///
/// The list is re-read when the folder changed since the last read.
fn list_carries(slot: &mut Option<DrawnNames>, dir: &Path, name: &str) -> bool {
    refresh_names(slot, dir);
    let Some(entry) = slot.as_ref() else {
        return false;
    };
    entry.names.iter().any(|candidate| candidate.name == name)
}

/// Re-read the names in `dir` into `slot` when the folder changed.
fn refresh_names(slot: &mut Option<DrawnNames>, dir: &Path) {
    let stamp = folder_stamp(dir);
    if slot.as_ref().is_some_and(|entry| entry.stamp == stamp) {
        return;
    }
    *slot = Some(DrawnNames {
        names: agnosis_core::drawn::list_with_sizes_in(dir),
        stamp,
    });
}

/// The folder's modification time, or `None` when it cannot be read.
fn folder_stamp(dir: &Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(dir).ok()?.modified().ok()
}

/// The first free generated name of `kind` (`sprite-1`, `sprite-2`, …).
pub fn drawn_next_free_name(kind: DrawnKind) -> String {
    match drawn_dir_for(kind) {
        Ok(dir) => agnosis_core::drawn::next_free_name_in(&dir, kind),
        Err(_) => format!("{}-1", kind.prefix()),
    }
}

// ---------------------------------------------------------------------------
// The package's target folders
// ---------------------------------------------------------------------------

/// The folders of the running platform.
pub fn target_dirs() -> Result<TargetDirs, PackageError> {
    Ok(TargetDirs {
        config_file: crate::paths::config_file().map_err(package_path_error)?,
        sprites: crate::paths::drawn_sprites_dir().map_err(package_path_error)?,
        balloons: crate::paths::drawn_balloons_dir().map_err(package_path_error)?,
        grass: crate::paths::drawn_grass_dir().map_err(package_path_error)?,
        decorations: crate::paths::drawn_decorations_dir().map_err(package_path_error)?,
    })
}

/// Turn a path error into a package error.
fn package_path_error(source: agnosis_core::path_error::PathError) -> PackageError {
    PackageError::Path { source }
}

// ---------------------------------------------------------------------------
// The app's own assets
// ---------------------------------------------------------------------------

/// The folders the app's own assets live in.
///
/// The core never resolves a platform location: this is where the
/// platform's folders are turned into the core's plain data.
pub fn asset_dirs(cfg: &Config) -> AssetDirs {
    AssetDirs {
        drawn: drawn_dirs(),
        tracks: tracks_dir_for(cfg),
    }
}

/// The four drawn-asset folders, or `None` when the platform cannot say
/// where one of them is.
///
/// A folder that cannot be resolved is **not** fatal: the parade shows
/// the built-in art and the failure is written to the trace once.
fn drawn_dirs() -> Option<DrawnDirs> {
    let sprites = drawn_dir(crate::paths::drawn_sprites_dir(), "drawn sprite")?;
    let balloons = drawn_dir(crate::paths::drawn_balloons_dir(), "drawn balloon")?;
    let grass = drawn_dir(crate::paths::drawn_grass_dir(), "drawn grass")?;
    let decorations = drawn_dir(crate::paths::drawn_decorations_dir(), "drawn decoration")?;
    Some(DrawnDirs {
        sprites,
        balloons,
        grass,
        decorations,
    })
}

/// One drawn-asset folder, or `None` when the platform cannot say where it
/// is.
fn drawn_dir(
    dir: Result<PathBuf, agnosis_core::path_error::PathError>,
    what: &str,
) -> Option<PathBuf> {
    match dir {
        Ok(path) => Some(path),
        Err(e) => {
            agnosis_core::trace::warn(&format!("the {what} folder is unavailable: {e}"));
            None
        }
    }
}

/// The folder the MIDI tracks live in — but only when the config names a
/// track.
///
/// A parade without a track never touches the disk, so the folder is
/// never even looked up.
fn tracks_dir_for(cfg: &Config) -> Option<PathBuf> {
    // Only a config that names a track needs the folder.
    let _ = cfg.audio.track.as_ref()?;
    match crate::paths::tracks_dir() {
        Ok(dir) => Some(dir),
        Err(e) => {
            agnosis_core::trace::warn(&format!("the track folder is unavailable: {e}"));
            None
        }
    }
}

// ---------------------------------------------------------------------------
// The backdrops
// ---------------------------------------------------------------------------

/// Resolve a picture name: an absolute path is taken as it is, a name is
/// looked up in the `backdrops` folder beside the config.
///
/// A blank name is no picture at all, and a relative name that tries to
/// escape the folder (`../../secrets.png`) is refused — an absolute name
/// is deliberately allowed, but a relative one may not leave the folder
/// it is looked up in (audit 2026-10-21 §G17).
pub fn resolve_backdrop_image(image: &str) -> Option<PathBuf> {
    let trimmed = image.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Some(path.to_path_buf());
    }
    if crate::paths::escapes_its_folder(trimmed) {
        return None;
    }
    let dir = crate::paths::backdrops_root().ok()?;
    let resolved = dir.join(trimmed);
    // The core only ever reads an **absolute** path, so a resolution
    // that is not absolute is refused (audit 2026-10-21 §G30).
    resolved.is_absolute().then_some(resolved)
}

/// Rewrite every configured backdrop picture into an absolute path.
///
/// The core converter only reads **absolute** paths: the name of a
/// picture is a platform question (which folder is it in?), so the
/// shell answers it before the core sees the config. A name that
/// cannot be resolved is left as it is, and the core skips it.
pub fn resolve_backdrop_paths(cfg: &mut Config) {
    for layer in cfg.backdrop.iter_mut() {
        resolve_backdrop_layer(layer);
    }
}

/// One layer, with its picture resolved — or untouched, when it cannot
/// be resolved.
///
/// This is **the** implementation of the rule: the config rewriter and
/// the backdrop loader both come here, so they cannot disagree about
/// where a picture lives (audit 2026-10-21 §G30).
pub fn resolve_backdrop_layer(layer: &mut BackdropConfig) {
    if Path::new(layer.image.trim()).is_absolute() {
        return;
    }
    let Some(path) = resolve_backdrop_image(&layer.image) else {
        return;
    };
    layer.image = path.to_string_lossy().into_owned();
}

// ---------------------------------------------------------------------------
// The HELP window's README
// ---------------------------------------------------------------------------

/// The README the HELP window shows, **read at open time**.
///
/// The README is the single source of truth (roadmap HELP_WINDOW §1): the
/// window carries no copy of its own, so a change to `README.md` reaches
/// the app's HELP on the next open. The **on-disk file wins** — a user
/// (or a packager) who edits or replaces it sees that edit — and the
/// shell's embedded copy is only the fallback for a bare exe run from
/// anywhere. The embed lives in the **shell**: the core stays
/// platform-free and file-free. When no on-disk copy answers,
/// [`restore_readme_if_missing`] writes the embedded one back beside the
/// binary — the help verb calls it at open time.
pub fn readme_text() -> String {
    readme_text_from(&readme_candidates(), EMBEDDED_README)
}

/// The README beside the binary and, for development builds, beside the
/// repo checkout — in the order they are tried.
fn readme_candidates() -> Vec<PathBuf> {
    let Ok(exe) = crate::paths::current_exe() else {
        return Vec::new();
    };
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    let mut candidates = vec![dir.join("README.md")];
    if let Some(repo) = repo_checkout_from(dir) {
        candidates.push(repo.join("README.md"));
    }
    candidates
}

/// The repo checkout's root, when `dir` is a Cargo build-output folder.
///
/// A development build's exe sits in `target/<profile>` (or its `deps`
/// subfolder), so the checkout root is a fixed number of levels up. A
/// release exe installed beside its README never reaches this: the first
/// candidate already answers.
fn repo_checkout_from(dir: &Path) -> Option<PathBuf> {
    let name = dir.file_name()?.to_str()?;
    match name {
        "debug" | "release" => dir.parent()?.parent().map(Path::to_path_buf),
        "deps" | "examples" => dir.parent()?.parent()?.parent().map(Path::to_path_buf),
        _ => None,
    }
}

/// The fallback text: the README as it stood when the binary was built.
const EMBEDDED_README: &str = include_str!("../README.md");

/// The first readable **non-empty** candidate wins; the embedded copy
/// answers when no candidate does — or when a candidate reads as an
/// empty file, which would blank the page where the fallback has a
/// whole document (audit 2026-10-25). Split from [`readme_text`] so the
/// rule is testable without touching the real install layout.
fn readme_text_from(candidates: &[PathBuf], fallback: &str) -> String {
    first_readable(candidates).unwrap_or_else(|| fallback.to_string())
}

/// The first candidate that reads as a non-empty file, if any.
fn first_readable(candidates: &[PathBuf]) -> Option<String> {
    candidates.iter().find_map(|path| {
        std::fs::read_to_string(path)
            .ok()
            .filter(|text| !text.is_empty())
    })
}

/// Restore the README beside the binary when it is missing or emptied:
/// the embedded copy is written back to the **first** candidate
/// location, so the documentation never stays deleted (the installer
/// ships one file today; a user who deletes it gets it back at the next
/// HELP open, and an update that replaces the exe bundles the README
/// again — audit 2026-10-25 request).
///
/// Best effort by contract: a folder that refuses the write (a
/// read-only install dir, say) leaves the window showing the embedded
/// copy without a file beside it, and nothing else changes. Never
/// touches a live file — the restore fires only when no candidate
/// reads as non-empty.
pub fn restore_readme_if_missing() -> bool {
    restore_readme_at(&readme_candidates(), EMBEDDED_README)
}

/// The testable split of [`restore_readme_if_missing`].
fn restore_readme_at(candidates: &[PathBuf], embedded: &str) -> bool {
    if first_readable(candidates).is_some() {
        return false;
    }
    let Some(path) = candidates.first() else {
        return false;
    };
    write_readme_atomically(path, embedded)
}

/// Write `embedded` to `path` atomically: the temp file first, then the
/// rename, so a killed process never leaves a half README behind.
///
/// The rename refuses an existing target on Windows, so the empty file
/// (the "deleted the contents" case) is removed first — it holds
/// nothing, and this point is only reached when no candidate reads as
/// non-empty.
fn write_readme_atomically(path: &Path, embedded: &str) -> bool {
    let tmp = path.with_extension("agnosis-tmp");
    if let Err(e) = std::fs::write(&tmp, embedded) {
        agnosis_core::trace::trace(&format!("readme.restore write failed: {e}"));
        return false;
    }
    let _ = std::fs::remove_file(path);
    match std::fs::rename(&tmp, path) {
        Ok(()) => {
            agnosis_core::trace::trace("readme.restored");
            true
        }
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            agnosis_core::trace::trace(&format!("readme.restore rename failed: {e}"));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The core only reads absolute paths, so a resolved name must be
    /// absolute (audit 2026-10-21 §G30).
    #[test]
    fn a_resolved_backdrop_name_is_absolute() {
        let resolved = resolve_backdrop_image("hills.png").expect("a resolved name");
        assert!(resolved.is_absolute(), "got {}", resolved.display());
    }

    /// An absolute name is taken as it is.
    #[test]
    fn an_absolute_backdrop_name_is_kept() {
        let picture = std::env::temp_dir().join("hills.png");
        let resolved =
            resolve_backdrop_image(&picture.to_string_lossy()).expect("an absolute path");
        assert_eq!(resolved, picture);
    }

    /// A relative name may not escape the folder it is looked up in.
    #[test]
    fn a_relative_backdrop_name_may_not_escape() {
        assert!(resolve_backdrop_image("../../secrets.png").is_none());
        assert!(resolve_backdrop_image("..\\..\\secrets.png").is_none());
    }

    // -----------------------------------------------------------------
    // The HELP window's README resolution (roadmap HELP_WINDOW §1)
    // -----------------------------------------------------------------

    /// A temp folder that is not the user's real config folder.
    fn temp_root(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "agnosis-host-{}-{}-{tag}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("mkdir temp dir");
        dir
    }

    /// The first readable candidate wins: an on-disk README beats the
    /// embedded copy, so an edited or replaced README reaches HELP.
    #[test]
    fn a_readable_candidate_wins_over_the_embedded_copy() {
        let dir = temp_root("readme-wins");
        let path = dir.join("README.md");
        std::fs::write(&path, "ON DISK WINS").expect("write the candidate");
        let text = readme_text_from(&[path], "EMBEDDED");
        assert_eq!(text, "ON DISK WINS");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A candidate that cannot be read is skipped, not fatal: the next
    /// candidate — or the embedded copy — answers.
    #[test]
    fn an_unreadable_candidate_falls_through_to_the_next() {
        let text = readme_text_from(
            &[PathBuf::from("Z:/definitely/missing/README.md")],
            "EMBEDDED",
        );
        assert_eq!(text, "EMBEDDED");
    }

    /// An **empty** candidate falls through too (audit 2026-10-25): a
    /// zero-byte README.md would blank the page where the embedded copy
    /// has a whole document.
    #[test]
    fn an_empty_candidate_falls_through_to_the_next() {
        let dir = temp_root("readme-empty");
        let path = dir.join("README.md");
        std::fs::write(&path, "").expect("write the empty candidate");
        let text = readme_text_from(&[path], "EMBEDDED");
        assert_eq!(text, "EMBEDDED");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A missing README is restored beside the binary: the embedded
    /// copy is written back, so a user who deletes the file gets it
    /// again at the next HELP open (audit 2026-10-25 request).
    #[test]
    fn a_missing_readme_is_restored() {
        let dir = temp_root("readme-restored");
        let path = dir.join("README.md");
        assert!(restore_readme_at(std::slice::from_ref(&path), "RESTORED"));
        let text = std::fs::read_to_string(&path).expect("the restored file");
        assert_eq!(text, "RESTORED");
        // The atomic write leaves no temp file behind.
        assert!(!dir.join("README.agnosis-tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An **emptied** README is rewritten: the empty file holds nothing
    /// and the rename needs the target gone on Windows.
    #[test]
    fn an_emptied_readme_is_rewritten() {
        let dir = temp_root("readme-rewritten");
        let path = dir.join("README.md");
        std::fs::write(&path, "").expect("write the empty candidate");
        assert!(restore_readme_at(std::slice::from_ref(&path), "RESTORED"));
        let text = std::fs::read_to_string(&path).expect("the rewritten file");
        assert_eq!(text, "RESTORED");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A **live** README is never touched: the restore fires only when
    /// no candidate reads as non-empty, so a user's (or packager's)
    /// edit survives.
    #[test]
    fn a_live_readme_is_never_touched() {
        let dir = temp_root("readme-live");
        let path = dir.join("README.md");
        std::fs::write(&path, "USER EDIT").expect("write the live candidate");
        assert!(!restore_readme_at(std::slice::from_ref(&path), "RESTORED"));
        let text = std::fs::read_to_string(&path).expect("the untouched file");
        assert_eq!(text, "USER EDIT");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The embedded fallback is the real README, so a bare exe run from
    /// anywhere still shows the documentation.
    #[test]
    fn the_embedded_fallback_is_the_real_readme() {
        assert!(EMBEDDED_README.contains("# AGnosis"));
        assert!(
            EMBEDDED_README.len() > 10_000,
            "the full README, not a stub"
        );
        let text = readme_text();
        assert!(text.contains("AGnosis"), "the production resolve answers");
    }
}
