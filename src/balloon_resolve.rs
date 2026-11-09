//! Shared balloon resolve + load path (audit P1-13, P0-3, P0-4).
//!
//! Two callers want "the balloon art the user meant": the folder watcher
//! (auto-loads everything under the balloon folder) and the popup's typed
//! name (`add balloon` line). Before this module each had its own resolve,
//! extension probe and cache handling, and they drifted: the typed path
//! ignored the radii stamp the watcher honours, scanned only the top level
//! while the watcher scans recursively, and matched bare stems so `duck.png`
//! also matched `duck.png.png` (audit P1-13).
//!
//! One implementation now serves both:
//!
//! - [`is_balloon_source`] is the single definition of "balloon content"
//!   (source extension, not a dotfile, not an editor temp file). The
//!   watcher gates its poll scan on it and the typed scan lists with it.
//! - [`resolve`] maps a typed name to a path: absolute names pass through,
//!   an exact hit wins, then a deterministic, case-insensitive, *recursive*
//!   scan matches the whole file name (or, for an extensionless name, the
//!   stem of a source file). The scan sees everything the watcher
//!   auto-loads, so a balloon in a subfolder is findable by name.
//! - [`load_stamped_cache`] is the only reader of the `<stem>.sprite.json`
//!   cache and it only accepts a cache whose radii stamp matches the live
//!   geometry (audit P0-3). The typed path honours the same rule, so typing
//!   a name can no longer re-introduce wrong-radii art.
//!
//! Stage 8 adds the **drawn balloon** fallback (roadmap D17): when the
//! balloon folder holds nothing of that name, the drawn balloon of the same
//! name is loaded from `<config>/drawn/balloons/<name>.json` — the frame is
//! used as it is, with its per-cell colours, because no conversion, no disc
//! mask and no repainting may touch the user's art. **Existing sources
//! win**: a file in the balloon folder keeps the current behaviour bit for
//! bit. The drawn folder is *not* watched — a drawn balloon arrives when it
//! is named, which is enough for now.

use std::path::{Path, PathBuf};

/// Source extensions that count as balloon content. The order is the
/// preference order used when several files match the same typed stem.
pub const SOURCE_EXTENSIONS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "bmp", "ans"];

/// Recursion cap for the folder scan; deeper trees are ignored. Mirrors the
/// watcher's cap so the typed path sees the same files the watcher loads.
pub const MAX_SCAN_DEPTH: usize = 8;

/// What kind of source a path is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    /// A raster image (PNG/JPEG/GIF/BMP), decoded by content.
    Image,
    /// Hand-made `.ans` ASCII art.
    Ans,
}

/// The kind of source a path names, by extension (case-insensitive).
pub fn source_kind(path: &Path) -> Option<SourceKind> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" => Some(SourceKind::Image),
        "ans" => Some(SourceKind::Ans),
        _ => None,
    }
}

/// True when the path names balloon content: a source file that is neither a
/// dotfile nor an editor temp file. Dotfiles and `.sprite.json` caches are
/// never balloon content.
pub fn is_balloon_source(path: &Path) -> bool {
    if path.is_dir() {
        return false;
    }
    source_kind(path).is_some_and(|_| !is_temp_name(path))
}

/// True for editor leftovers: dotfiles, `*~` backups and half-downloaded
/// `.tmp`/`.part`/`.crdownload` files.
fn is_temp_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return true;
    };
    if name.starts_with('.') || name.ends_with('~') {
        return true;
    }
    matches!(
        path.extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase()),
        Some(ref e) if e == "tmp" || e == "part" || e == "crdownload"
    )
}

/// True when the path names `.ans` art (used verbatim, not decoded).
pub fn is_ans(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("ans"))
}

/// The `<stem>.sprite.json` cache sibling of a source path.
pub fn sibling_sprite(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem()?.to_str()?;
    Some(path.with_file_name(format!("{stem}.sprite.json")))
}

/// True when the path's sibling cache carries exactly these radii.
///
/// Missing siblings, unreadable stamps and stale stamps all read as "not
/// current": the caller must reconvert (audit P0-3).
pub fn cache_stamp_current(path: &Path, radii: (i64, i64)) -> bool {
    let Some(sibling) = sibling_sprite(path) else {
        return false;
    };
    if !sibling.is_file() {
        return false;
    }
    agnosis_core::image_ascii::stamped_radii(&sibling) == Some(radii)
}

/// Load the sibling `.sprite.json` cache when its radii stamp matches the
/// live geometry (audit P0-3). Missing, unreadable or stale caches are
/// misses — the caller reconverts. A cache-decode failure traces once and
/// reads as a miss.
pub fn load_stamped_cache(
    path: &Path,
    radii: (i64, i64),
) -> Option<agnosis_core::balloon::AsciiSprite> {
    let sibling = sibling_sprite(path)?;
    if !sibling.is_file() {
        return None;
    }
    if agnosis_core::image_ascii::stamped_radii(&sibling) != Some(radii) {
        return None;
    }
    match agnosis_core::image_ascii::load_sprite(&sibling) {
        Ok(sprite) => Some(sprite),
        Err(e) => {
            agnosis_core::trace::trace(&format!(
                "balloon cache {} ignored: {e}",
                sibling.display()
            ));
            None
        }
    }
}

/// Convert one source path at the given radii: `.ans` art verbatim, raster
/// images decoded by content (the extension is only a hint for the decoder
/// input, not a gate).
pub fn convert_source(
    path: &Path,
    rx: i64,
    ry: i64,
) -> Result<agnosis_core::balloon::AsciiSprite, agnosis_core::image_ascii::ImageAsciiError> {
    if is_ans(path) {
        agnosis_core::image_ascii::convert_ans_file(
            path,
            rx,
            ry,
            agnosis_core::image_ascii::ANS_COLOR,
        )
    } else {
        agnosis_core::image_ascii::convert_image_file(path, rx, ry)
    }
}

/// Resolve a typed balloon name to a path under `dir`.
///
/// Absolute names pass through untouched. Otherwise an exact hit wins, then
/// a deterministic case-insensitive recursive scan matches the whole file
/// name (case-folded, Unicode-aware) or — for an extensionless name — the
/// stem of a source file. A name that matches nothing resolves to the
/// direct join so the caller can report it (audit P1-13).
///
/// A relative name that tries to **escape** the folder — `../secrets.png`
/// — is refused: the direct join would resolve outside it, so the balloon
/// folder would stop being a boundary (audit 2026-10-21 §G17).
pub fn resolve(dir: &Path, name: &str) -> Option<PathBuf> {
    let as_path = Path::new(name);
    if as_path.is_absolute() {
        return Some(as_path.to_path_buf());
    }
    if crate::paths::escapes_its_folder(name) {
        return None;
    }
    let direct = dir.join(name);
    if direct.is_file() {
        return Some(direct);
    }
    Some(find_match(dir, name).unwrap_or(direct))
}

/// First file under `dir` whose name matches `name`, preferring the
/// canonical extension order and then the path order (deterministic).
fn find_match(dir: &Path, name: &str) -> Option<PathBuf> {
    let want = name.to_lowercase();
    let mut matches: Vec<PathBuf> = scan_sources(dir)
        .into_iter()
        .filter(|path| name_matches(path, &want))
        .collect();
    matches.sort_by(|a, b| (ext_rank(a), a).cmp(&(ext_rank(b), b)));
    matches.into_iter().next()
}

/// Every balloon source file under `dir`, recursively, sorted by path.
///
/// The scan skips symlinks (a symlink loop must not hang the caller) and
/// stops at [`MAX_SCAN_DEPTH`], matching the watcher's poll scan.
pub fn scan_sources(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0usize)];
    while let Some((current, depth)) = stack.pop() {
        if depth >= MAX_SCAN_DEPTH {
            continue;
        }
        let (sources, subdirs) = entries_in(&current);
        out.extend(sources);
        stack.extend(subdirs.into_iter().map(|path| (path, depth + 1)));
    }
    out.sort();
    out
}

/// Balloon source files and real subdirectories directly inside `dir`.
///
/// One `read_dir` per level: the directory is listed once and split, so the
/// scan never walks the same level twice.
pub fn entries_in(dir: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    let mut sources = Vec::new();
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        match classify_entry(&entry) {
            Entry::Dir(path) => subdirs.push(path),
            Entry::Source(path) => sources.push(path),
            Entry::Skip => {}
        }
    }
    (sources, subdirs)
}

/// How one directory entry counts for the balloon scan.
enum Entry {
    /// A real subdirectory to descend into.
    Dir(PathBuf),
    /// A candidate balloon source file.
    Source(PathBuf),
    /// Nothing: a symlink, a dot-directory, or a file that is not a source.
    Skip,
}

/// Classify one directory entry. The decisions live here so the scan loop
/// stays flat (audit nesting rule).
fn classify_entry(entry: &std::fs::DirEntry) -> Entry {
    if is_symlink(entry) {
        return Entry::Skip;
    }
    let path = entry.path();
    if path.is_dir() {
        return dir_entry(path);
    }
    if is_balloon_source(&path) {
        return Entry::Source(path);
    }
    Entry::Skip
}

/// Classify a directory: dot-directories are skipped, the rest are descended.
fn dir_entry(path: PathBuf) -> Entry {
    if is_dot(&path) {
        return Entry::Skip;
    }
    Entry::Dir(path)
}

/// True for symlinks, checked via `file_type` and never followed.
fn is_symlink(entry: &std::fs::DirEntry) -> bool {
    entry.file_type().is_ok_and(|t| t.is_symlink())
}

/// True for dot-directories (skipped by the scan).
fn is_dot(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with('.'))
}

/// True when the path's file name matches `want` (already case-folded).
///
/// The whole name always matches. The stem of a source file matches only
/// when `want` carries no extension: that is what keeps `duck.png` from
/// matching `duck.png.png` (audit P1-13).
fn name_matches(path: &Path, want: &str) -> bool {
    let Some(file) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    let file = file.to_lowercase();
    file == *want || stem_matches(&file, want)
}

/// True when `file` is a source file whose stem equals `want`.
fn stem_matches(file: &str, want: &str) -> bool {
    if Path::new(want).extension().is_some() {
        return false;
    }
    bare_stem(file).is_some_and(|stem| stem == want)
}

/// Stem of a source file name (already case-folded); `None` when the
/// extension is not a source extension.
fn bare_stem(lower: &str) -> Option<&str> {
    let (stem, ext) = lower.rsplit_once('.')?;
    if SOURCE_EXTENSIONS.contains(&ext) {
        Some(stem)
    } else {
        None
    }
}

/// Preference rank of a path's extension: lower wins, unknown sorts last.
fn ext_rank(path: &Path) -> usize {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    SOURCE_EXTENSIONS
        .iter()
        .position(|candidate| *candidate == ext)
        .unwrap_or(usize::MAX)
}

/// Load one balloon by typed name: resolve the path, honour the stamped
/// `<stem>.sprite.json` cache, else convert at the live radii.
///
/// The error string is user-facing (drawn on the input line).
pub fn load(
    dir: &Path,
    name: &str,
    rx: i64,
    ry: i64,
) -> Result<agnosis_core::balloon::AsciiSprite, String> {
    // The drawn folder is only **read** here, so it is resolved without
    // creating it: a lookup must not materialise a folder in the user's
    // real config tree (audit 2026-10-21 §G23).
    let drawn = crate::paths::drawn_balloons_root()
        .map_err(|e| format!("CANNOT LOAD {}: {e}", name.to_uppercase()))?;
    load_from(&drawn, dir, name, rx, ry)
}

/// [`load`] with an explicit drawn-asset folder.
///
/// The drawn folder is the fallback: when the balloon folder holds nothing
/// of that name, the drawn balloon of the same name is used. **Existing
/// sources win** — a file in the balloon folder keeps the current
/// behaviour, bit for bit (roadmap D17).
pub fn load_from(
    drawn_dir: &Path,
    dir: &Path,
    name: &str,
    rx: i64,
    ry: i64,
) -> Result<agnosis_core::balloon::AsciiSprite, String> {
    if name.is_empty() {
        return Err("TYPE A NAME FIRST".to_string());
    }
    // The NOT FOUND string points at the art folder: the single most
    // common "the app is broken" report is a fresh install typing a name
    // into an **empty** balloon folder — the feature works, there is
    // simply nothing to find yet. `Ctrl+O` opens the folder (and
    // `agnosis add` copies art into it), so the message names the way out.
    // Block-font charset only (A-Z, 0-9, `. : _ - / \` and space).
    let not_found = format!(
        "CANNOT LOAD {}: NOT FOUND - CTRL O FOR THE ART FOLDER",
        name.to_uppercase()
    );
    let Some(path) = resolve(dir, name) else {
        return Err(not_found);
    };
    if !path.is_file() {
        return load_drawn(drawn_dir, name).ok_or(not_found);
    }
    if let Some(sprite) = load_stamped_cache(&path, (rx, ry)) {
        return Ok(sprite);
    }
    convert_source(&path, rx, ry).map_err(|e| format!("CANNOT LOAD {}: {e}", name.to_uppercase()))
}

/// The drawn balloon of that name, if there is one.
///
/// The frame is used **as it is**: no conversion, no disc mask, no
/// repainting — the user's per-cell colours survive (roadmap D16).
fn load_drawn(drawn_dir: &Path, name: &str) -> Option<agnosis_core::balloon::AsciiSprite> {
    load_drawn_doc(drawn_dir, name).and_then(|sprite| sprite.frame(0).cloned())
}

/// The drawn balloon document of that name.
///
/// The exact name wins; a name that differs only in case or in an illegal
/// character is looked up in the folder, so the typed name behaves like it
/// does for the balloon folder.
fn load_drawn_doc(drawn_dir: &Path, name: &str) -> Option<agnosis_core::drawn::DrawnSprite> {
    match agnosis_core::drawn::load_from(drawn_dir, agnosis_core::drawn::DrawnKind::Balloon, name) {
        Ok(sprite) => Some(sprite),
        Err(e) => load_drawn_doc_folded(drawn_dir, name, e),
    }
}

/// The drawn balloon document of that name, looked up case-insensitively.
///
/// `first` is the error of the exact-name attempt; it is traced when the
/// folder holds nothing of that name either.
fn load_drawn_doc_folded(
    drawn_dir: &Path,
    name: &str,
    first: agnosis_core::drawn::DrawnError,
) -> Option<agnosis_core::drawn::DrawnSprite> {
    let canonical = agnosis_core::drawn::list_in(drawn_dir)
        .into_iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(name));
    let Some(canonical) = canonical else {
        agnosis_core::trace::trace(&format!(
            "balloon_resolve: no drawn balloon {name:?}: {first}"
        ));
        return None;
    };
    match agnosis_core::drawn::load_from(
        drawn_dir,
        agnosis_core::drawn::DrawnKind::Balloon,
        &canonical,
    ) {
        Ok(sprite) => Some(sprite),
        Err(e) => {
            agnosis_core::trace::trace(&format!(
                "balloon_resolve: drawn balloon {canonical:?} ignored: {e}"
            ));
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "agnosis-resolve-{}-{}-{tag}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        dir
    }

    #[test]
    fn resolves_recursive_case_insensitive_stem() {
        let dir = temp_dir("stem");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("DUCK.PNG"), b"x").unwrap();
        let found = resolve(&dir, "duck").expect("a resolved name");
        assert_eq!(
            found.file_name().and_then(|n| n.to_str()),
            Some("DUCK.PNG"),
            "the on-disk casing must be returned"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extension_present_name_does_not_match_double_extension() {
        let dir = temp_dir("double");
        std::fs::write(dir.join("duck.png.png"), b"x").unwrap();
        // "duck.png" must not resolve to "duck.png.png".
        let found = resolve(&dir, "duck.png").expect("a resolved name");
        assert_ne!(
            found.file_name().and_then(|n| n.to_str()),
            Some("duck.png.png"),
            "bare-stem matching must not fire for an extensioned name"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_is_sorted_and_skips_temp_files() {
        let dir = temp_dir("sorted");
        std::fs::write(dir.join("b.png"), b"x").unwrap();
        std::fs::write(dir.join("a.png"), b"x").unwrap();
        std::fs::write(dir.join("a.tmp"), b"x").unwrap();
        std::fs::write(dir.join(".hidden.png"), b"x").unwrap();
        let found = scan_sources(&dir);
        let names: Vec<_> = found
            .iter()
            .map(|p| p.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        assert_eq!(names, vec!["a.png".to_string(), "b.png".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_ascii_names_resolve() {
        let dir = temp_dir("unicode");
        std::fs::write(dir.join("entchen.png"), b"x").unwrap();
        let found = resolve(&dir, "ENTCHEN").expect("a resolved name");
        assert_eq!(
            found.file_name().and_then(|n| n.to_str()),
            Some("entchen.png")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_name_is_reported_not_loaded() {
        let dir = temp_dir("missing");
        let err = load(&dir, "nope", 4, 2).unwrap_err();
        assert!(err.contains("CANNOT LOAD NOPE"), "got {err}");
        let err = load(&dir, "", 4, 2).unwrap_err();
        assert_eq!(err, "TYPE A NAME FIRST");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Stage 8: the drawn balloons are summoned by name ------------------

    /// The colour of the drawn balloon of the tests: the `.ans` path would
    /// have repainted it amber (roadmap D16).
    const DRAWN_COLOR: [u8; 3] = [200, 40, 240];

    /// Write a drawn balloon document (one disc frame, one inked cell).
    fn write_drawn_balloon(dir: &Path, name: &str) {
        let geometry = agnosis_core::balloon::balloon_authoring_geometry(4);
        let mut frame = agnosis_core::balloon::AsciiSprite::new(geometry.cols, geometry.rows);
        frame.set(geometry.cols / 2, geometry.rows / 2, 'o', DRAWN_COLOR);
        let sprite = agnosis_core::drawn::DrawnSprite {
            kind: agnosis_core::drawn::DrawnKind::Balloon,
            size: None,
            frames: vec![frame],
            reaction: None,
        };
        let text = serde_json::to_string(&sprite.to_doc()).expect("serialize");
        std::fs::write(dir.join(format!("{name}.json")), text).expect("write");
    }

    #[test]
    fn load_finds_a_drawn_balloon_by_name() {
        let drawn = temp_dir("drawn");
        let balloons = temp_dir("balloons");
        write_drawn_balloon(&drawn, "party");
        let sprite = load_from(&drawn, &balloons, "party", 4, 2).expect("load");
        let geometry = agnosis_core::balloon::balloon_authoring_geometry(4);
        assert_eq!(
            sprite.get(geometry.cols / 2, geometry.rows / 2),
            Some(('o', DRAWN_COLOR)),
            "the drawn balloon must be loaded"
        );
        let _ = std::fs::remove_dir_all(&drawn);
        let _ = std::fs::remove_dir_all(&balloons);
    }

    #[test]
    fn drawn_balloon_keeps_its_cell_colours() {
        let drawn = temp_dir("drawn-colours");
        let balloons = temp_dir("balloons-colours");
        write_drawn_balloon(&drawn, "party");
        let sprite = load_from(&drawn, &balloons, "party", 4, 2).expect("load");
        // The colour must survive: no conversion, no disc mask, no
        // repainting (the `.ans` path would have used ANS_COLOR).
        let geometry = agnosis_core::balloon::balloon_authoring_geometry(4);
        let cell = sprite
            .get(geometry.cols / 2, geometry.rows / 2)
            .expect("the inked cell");
        assert_eq!(cell.1, DRAWN_COLOR);
        assert_ne!(
            cell.1,
            agnosis_core::image_ascii::ANS_COLOR,
            "the drawn colour must not be repainted"
        );
        let _ = std::fs::remove_dir_all(&drawn);
        let _ = std::fs::remove_dir_all(&balloons);
    }

    #[test]
    fn load_prefers_an_existing_source_over_a_drawn_balloon() {
        let drawn = temp_dir("drawn-prefer");
        let balloons = temp_dir("balloons-prefer");
        write_drawn_balloon(&drawn, "party");
        // A file of the same name in the balloon folder wins: the existing
        // behaviour is kept bit for bit.
        std::fs::write(balloons.join("party.ans"), b"##\n##").expect("write ans");
        let sprite = load_from(&drawn, &balloons, "party", 4, 2).expect("load");
        let cell = sprite.get(4, 4).expect("the inked cell");
        assert_ne!(
            cell.1, DRAWN_COLOR,
            "the balloon folder wins over the drawn folder"
        );
        let inked = sprite.chars().iter().filter(|ch| **ch != ' ').count();
        assert!(inked > 0, "the `.ans` art must be loaded");
        let _ = std::fs::remove_dir_all(&drawn);
        let _ = std::fs::remove_dir_all(&balloons);
    }

    #[test]
    fn load_reports_not_found_when_neither_exists() {
        let drawn = temp_dir("drawn-none");
        let balloons = temp_dir("balloons-none");
        let err = load_from(&drawn, &balloons, "nope", 4, 2).expect_err("no such balloon");
        assert!(err.contains("CANNOT LOAD NOPE"), "got {err}");
        assert!(err.contains("NOT FOUND"), "got {err}");
        // An empty name is still the "type a name" hint.
        let err = load_from(&drawn, &balloons, "", 4, 2).expect_err("no name");
        assert_eq!(err, "TYPE A NAME FIRST");
        let _ = std::fs::remove_dir_all(&drawn);
        let _ = std::fs::remove_dir_all(&balloons);
    }
}
