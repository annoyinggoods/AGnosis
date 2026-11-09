//! Scene presets: the whole **look** under a name (roadmap Extension 1
//! §3.4, §9.2).
//!
//! A preset **is** a package with another extension (`.agnosispreset`) and
//! another merge policy: a package **merges**, a preset **replaces**.
//! Everything else is shared — the same manifest schema, the same
//! store-only ZIP, the same "what is a creation?" question.
//!
//! A preset carries **no assets**: it is a *look*, not art. The manifest
//! schema allows an asset array, so the array is simply empty.
//!
//! WHAT a preset owns, and what it must never touch (roadmap §9.2, AC:
//! "Applying a preset replaces the look and does not touch the hotkey or
//! the browser list"):
//!
//! | replaced                                   | never touched            |
//! |--------------------------------------------|--------------------------|
//! | `sky`, `weather`, `message`, `grass`        | `hotkey`                 |
//! | `assignments`, `personality`                | `browsers`               |
//! |                                            | `monitor`, `fps_cap`     |
//! |                                            | `max_cols`, `loop_seconds` |
//!
//! The copy is **field by field**, never a struct replace: a struct
//! replace would silently reset the user's hotkey and browser list, which
//! is the exact bug the roadmap's AC names.
//!
//! The **capture** side is one policy with the package (audit
//! 2026-10-23 §F-046): a preset file is built from
//! [`crate::package::capture_look`], which adds the backdrops and the
//! decorations so a shared look is complete. They ride in the file for
//! the web front end and for inspection; `apply_preset` does not write
//! them back — a backdrop names a **picture** the preset does not carry,
//! and a decoration names an asset the recipient may not have, so
//! applying either would write a dangling reference into the config.

use std::path::{Path, PathBuf};

use crate::config::{Config, ConfigError};
use crate::manifest::{Manifest, ManifestError, MANIFEST_ENTRY};
use crate::package::{ZipArchive, ZipError, ZipWriter};

/// The extension of a preset file.
pub const PRESET_EXTENSION: &str = "agnosispreset";

/// The name of the config entry inside a preset.
///
/// The manifest carries the config as well; this entry is the same config
/// as a plain TOML document, so a preset can be inspected with any text
/// editor.
pub const PRESET_CONFIG_ENTRY: &str = "config.toml";

/// The longest accepted preset name.
pub const MAX_PRESET_NAME_LEN: usize = 48;

/// The look of a configuration: the fields a preset owns.
///
/// One capture policy with the package since audit 2026-10-23 §F-046:
/// [`crate::package::capture_look`] — the preset's look plus the
/// backdrops and the decorations, minus the hotkey and the browser list.
fn look_of(cfg: &Config) -> Config {
    crate::package::capture_look(cfg)
}

/// Save a preset: `<dir>/<name>.agnosispreset`.
///
/// An existing preset of the same name is replaced — the user is naming
/// the look they want, not collecting versions. The name is **trimmed**
/// before it becomes a file name, so a trailing space can never produce a
/// preset `list` cannot show (audit 2026-10-23 §F-064).
///
/// The manifest's config fragment is written through
/// [`Manifest::from_look`]: no hotkey, no browsers (audit 2026-10-23
/// §F-046).
pub fn save_preset(dir: &Path, name: &str, cfg: &Config) -> Result<PathBuf, PresetError> {
    let name = name.trim();
    validate_preset_name(name)?;
    let look = look_of(cfg);
    let manifest = Manifest::from_look(&look, name);
    manifest.validate()?;
    let mut writer = ZipWriter::new();
    writer.add_file(MANIFEST_ENTRY, manifest.to_json()?.as_bytes())?;
    writer.add_file(
        PRESET_CONFIG_ENTRY,
        toml::to_string_pretty(&look)?.as_bytes(),
    )?;
    let bytes = writer.finish()?;
    std::fs::create_dir_all(dir).map_err(|source| PresetError::Io {
        path: dir.to_path_buf(),
        source,
    })?;
    let path = dir.join(format!("{name}.{PRESET_EXTENSION}"));
    write_bytes(&path, &bytes)?;
    Ok(path)
}

/// The names of the presets in `dir`, sorted.
///
/// A folder that cannot be read yields an empty list: "no presets yet" is
/// not an error.
pub fn list_presets(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| preset_stem(&entry.path()))
        .collect();
    names.sort();
    names
}

/// The file stem of a preset, or `None` for anything else.
fn preset_stem(path: &Path) -> Option<String> {
    if path.extension().and_then(|ext| ext.to_str()) != Some(PRESET_EXTENSION) {
        return None;
    }
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .map(|stem| stem.to_string())
}

/// The config a preset describes.
///
/// The preset's own `config.toml` entry is the config; the manifest's
/// fragment is the fallback, and the manifest's assignment table wins
/// over the entry's — the manifest is the schema, the entry is a copy.
pub fn load_preset(path: &Path) -> Result<Config, PresetError> {
    let bytes = std::fs::read(path).map_err(|source| PresetError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let archive = ZipArchive::parse(bytes)?;
    let manifest = read_manifest(&archive)?;
    let mut cfg = read_config(&archive, &manifest)?;
    apply_manifest_assignments(&mut cfg, &manifest);
    Ok(cfg)
}

/// The manifest of a preset.
fn read_manifest(archive: &ZipArchive) -> Result<Manifest, PresetError> {
    let bytes = archive.read(MANIFEST_ENTRY)?;
    let text = std::str::from_utf8(bytes).map_err(|_| PresetError::NotUtf8)?;
    let manifest = Manifest::from_json(text)?;
    manifest.validate()?;
    Ok(manifest)
}

/// The config of a preset: its own entry, or the manifest's fragment.
fn read_config(archive: &ZipArchive, manifest: &Manifest) -> Result<Config, PresetError> {
    if !archive.has(PRESET_CONFIG_ENTRY) {
        return manifest.config().map_err(PresetError::Manifest);
    }
    let bytes = archive.read(PRESET_CONFIG_ENTRY)?;
    let text = std::str::from_utf8(bytes).map_err(|_| PresetError::NotUtf8)?;
    toml::from_str(text).map_err(|source| PresetError::Config { source })
}

/// The manifest's assignment table wins over the config entry's.
fn apply_manifest_assignments(cfg: &mut Config, manifest: &Manifest) {
    if manifest.assignments.is_empty() {
        return;
    }
    if let Ok(table) = manifest.assignments_config() {
        cfg.assignments = table;
    }
}

/// Apply a preset to a configuration: the look is **replaced**.
///
/// The copy is field by field. The hotkey, the browser list, the monitor
/// choice, `fps_cap`, `max_cols` and `loop_seconds` are **not** touched:
/// a preset is a look, and switching looks must never disarm the user's
/// hotkey or change which browsers are hidden.
pub fn apply_preset(cfg: &mut Config, preset: &Config) {
    cfg.sky = preset.sky.clone();
    cfg.weather = preset.weather.clone();
    cfg.message = preset.message.clone();
    cfg.grass = preset.grass.clone();
    cfg.assignments = preset.assignments.clone();
    cfg.personality = preset.personality.clone();
}

/// Apply a preset to the user's config **file**, surgically and in **one
/// write**.
///
/// Every table the preset owns is replaced in place, so the user's
/// comments and the keys the preset does not own survive — the same
/// discipline the `[assignments]` table uses (audit 2026-10-20 §3.9).
/// The hotkey, the browser list and the monitor choice are not touched at
/// all: a preset is a *look*.
///
/// All six edits (sky, weather, message, grass, assignments,
/// personality) are spliced **in memory** and land in one atomic write:
/// the preset used to be six sequential writes, so a failure mid-way left
/// half the look applied (audit 2026-10-23 §F-051). A splice the document
/// cannot carry now refuses the **whole** preset and the file keeps its
/// old values; the `.bak` the write keeps is the pre-preset state.
///
/// A file that does not exist yet is created from the whole default
/// config, the same fresh path the single-table writers have.
pub fn apply_preset_to_file(path: &Path, preset: &Config) -> Result<(), PresetError> {
    let original = crate::config_edit::read_or_default(path)?;
    let Some(spliced) = splice_preset(&original, preset) else {
        return Err(PresetError::ConfigWrite(ConfigError::TableWrite {
            section: "preset".to_string(),
            path: path.to_path_buf(),
        }));
    };
    crate::config_edit::write_atomic(path, &spliced)?;
    Ok(())
}

/// All of a preset's edits, composed into ONE text.
///
/// `None` when any splice cannot be carried or the finished text would
/// not parse: the caller then refuses the whole preset.
fn splice_preset(text: &str, preset: &Config) -> Option<String> {
    let out = crate::package::splice_look(text, preset)?;
    let out = crate::config_edit::splice_assignments_checked(&out, &preset.assignments)?;
    let entries = crate::personality::array_entries(&preset.personality);
    crate::config_edit::splice_array_table(&out, "personality", &entries)
        .and_then(crate::config_edit::checked_config_text)
}

// The `[[personality]]` entries are rendered by
// `crate::personality::array_entries`, which the settings panel's
// PERSONALITY actions share.

/// Reject a preset name that could escape the preset folder or confuse
/// the shell.
fn validate_preset_name(name: &str) -> Result<(), PresetError> {
    let trimmed = name.trim();
    let illegal = trimmed.is_empty()
        || trimmed.len() > MAX_PRESET_NAME_LEN
        || trimmed == "."
        || trimmed == ".."
        || trimmed.starts_with('.')
        || trimmed.starts_with('-')
        || trimmed.chars().any(|ch| {
            ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
        });
    if illegal {
        return Err(PresetError::InvalidName {
            name: name.to_string(),
        });
    }
    Ok(())
}

/// Write a file atomically, creating the parent folder.
///
/// WHY atomic: `std::fs::write` truncates the target first, so a crash
/// (or the release profile's `panic = "abort"`) mid-write left a
/// truncated `.agnosispreset` behind — and a truncated preset is not a
/// preset at all (audit 2026-10-21 §F11).
fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), PresetError> {
    ensure_parent(path)?;
    let temp = temp_path(path);
    std::fs::write(&temp, bytes).map_err(|source| PresetError::Io {
        path: temp.clone(),
        source,
    })?;
    match std::fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(source) => {
            let _ = std::fs::remove_file(&temp);
            Err(PresetError::Io {
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
        .unwrap_or_else(|| "preset.agnosispreset".to_string());
    match path.parent() {
        Some(parent) => parent.join(format!("{name}.{}.tmp", std::process::id())),
        None => PathBuf::from(format!("{name}.{}.tmp", std::process::id())),
    }
}

/// Create the parent folder of `path`, when it has one.
fn ensure_parent(path: &Path) -> Result<(), PresetError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    if parent.as_os_str().is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(parent).map_err(|source| PresetError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Everything that can go wrong while saving, listing or applying a
/// preset.
#[derive(Debug, thiserror::Error)]
pub enum PresetError {
    /// The name is not a legal preset name.
    #[error("illegal preset name {name:?}")]
    InvalidName { name: String },
    /// A file could not be read or written.
    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The preset's archive is broken.
    #[error(transparent)]
    Zip(#[from] ZipError),
    /// The preset's manifest is broken.
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    /// The preset's config is not a config.
    #[error("could not parse the preset's config: {source}")]
    Config {
        #[source]
        source: toml::de::Error,
    },
    /// The config could not be serialized into the preset.
    #[error("could not serialize the preset's config: {0}")]
    ConfigSerialize(#[from] toml::ser::Error),
    /// The user's config could not be written.
    #[error(transparent)]
    ConfigWrite(#[from] crate::config::ConfigError),
    /// An entry's name is not valid UTF-8.
    #[error("the preset carries an entry whose name is not valid utf-8")]
    NotUtf8,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AssignmentSlot;

    /// A temp folder that is not the user's real config folder.
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ducks-preset-{}-{}-{tag}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("mkdir temp dir");
        dir
    }

    /// A config with a distinctive look and a distinctive hotkey.
    fn sample_config() -> Config {
        let mut cfg = Config::default();
        cfg.sky.enabled = true;
        cfg.sky.time_of_day = 0.8;
        cfg.weather.mode = crate::config::WeatherMode::Rain;
        cfg.weather.intensity = 1.5;
        cfg.message.text = "STORM".to_string();
        cfg.grass.tile = Some("my-grass".to_string());
        cfg.hotkey.windows = "Ctrl+Alt+F9".to_string();
        cfg.hotkey.editor = "Ctrl+Alt+F10".to_string();
        cfg.browsers.names = vec!["mybrowser".to_string()];
        cfg.assignments.set(
            AssignmentSlot::parse("small_3").expect("small_3"),
            Some("cat".to_string()),
        );
        cfg
    }

    #[test]
    fn a_preset_round_trips() {
        let dir = temp_dir("round-trip");
        let cfg = sample_config();
        let path = save_preset(&dir, "stormy", &cfg).expect("save");
        assert!(path.is_file());
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("stormy.agnosispreset")
        );

        let back = load_preset(&path).expect("load");
        assert!(back.sky.enabled);
        assert_eq!(back.sky.time_of_day, 0.8);
        assert_eq!(back.weather.mode, crate::config::WeatherMode::Rain);
        assert_eq!(back.message.text, "STORM");
        assert_eq!(back.grass.tile.as_deref(), Some("my-grass"));
        assert_eq!(back.assignments.small(3), Some("cat"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_presets_are_listed_and_sorted() {
        let dir = temp_dir("list");
        for name in ["stormy", "calm", "sunny"] {
            save_preset(&dir, name, &sample_config()).expect("save");
        }
        std::fs::write(dir.join("notes.txt"), b"x").expect("write a stray file");
        assert_eq!(
            list_presets(&dir),
            vec![
                "calm".to_string(),
                "stormy".to_string(),
                "sunny".to_string()
            ]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_folder_lists_no_presets() {
        let dir = temp_dir("missing");
        assert!(list_presets(&dir.join("nope")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn applying_a_preset_replaces_the_look() {
        let mut cfg = Config::default();
        cfg.sky.enabled = false;
        cfg.message.text = "BEFORE".to_string();
        let preset = load_of(&sample_config());
        apply_preset(&mut cfg, &preset);
        assert!(cfg.sky.enabled, "the look is replaced");
        assert_eq!(cfg.message.text, "STORM");
        assert_eq!(cfg.weather.mode, crate::config::WeatherMode::Rain);
        assert_eq!(cfg.grass.tile.as_deref(), Some("my-grass"));
        assert_eq!(cfg.assignments.small(3), Some("cat"));
    }

    #[test]
    fn applying_a_preset_never_touches_the_hotkey_or_the_browsers() {
        let mut cfg = Config::default();
        cfg.hotkey.windows = "Ctrl+Alt+F9".to_string();
        cfg.hotkey.editor = "Ctrl+Alt+F10".to_string();
        cfg.hotkey.macos = "Ctrl+Alt+F11".to_string();
        cfg.hotkey.linux = "Ctrl+Alt+F12".to_string();
        cfg.browsers.names = vec!["mybrowser".to_string()];
        cfg.browsers.restore_on_close = true;
        cfg.monitor = crate::config::MonitorChoice::Primary;
        cfg.fps_cap = 60;
        cfg.max_cols = 1234;
        cfg.loop_seconds = 42.0;

        let mut preset = sample_config();
        // A preset that carries *other* values for the protected fields:
        // they must not reach the user's config either.
        preset.hotkey.windows = "X+Y".to_string();
        preset.hotkey.editor = "X+Z".to_string();
        preset.browsers.names = vec!["otherbrowser".to_string()];
        preset.monitor = crate::config::MonitorChoice::Cursor;
        preset.fps_cap = 15;
        preset.max_cols = 999;
        preset.loop_seconds = 7.0;

        apply_preset(&mut cfg, &preset);

        assert_eq!(cfg.hotkey.windows, "Ctrl+Alt+F9", "the hotkey survives");
        assert_eq!(cfg.hotkey.editor, "Ctrl+Alt+F10");
        assert_eq!(cfg.hotkey.macos, "Ctrl+Alt+F11");
        assert_eq!(cfg.hotkey.linux, "Ctrl+Alt+F12");
        assert_eq!(cfg.browsers.names, vec!["mybrowser".to_string()]);
        assert!(cfg.browsers.restore_on_close, "the browser list survives");
        assert_eq!(cfg.monitor, crate::config::MonitorChoice::Primary);
        assert_eq!(cfg.fps_cap, 60);
        assert_eq!(cfg.max_cols, 1234);
        assert_eq!(cfg.loop_seconds, 42.0);
        // …and the look did change.
        assert_eq!(cfg.message.text, "STORM");
    }

    #[test]
    fn a_preset_file_carries_no_assets() {
        let dir = temp_dir("no-assets");
        let path = save_preset(&dir, "calm", &sample_config()).expect("save");
        let bytes = std::fs::read(&path).expect("read");
        let archive = ZipArchive::parse(bytes).expect("parse");
        let manifest_bytes = archive.read(MANIFEST_ENTRY).expect("the manifest");
        let manifest = Manifest::from_json(std::str::from_utf8(manifest_bytes).expect("utf-8"))
            .expect("the manifest");
        assert!(manifest.assets.is_empty(), "a preset carries no art");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_illegal_preset_name_is_refused() {
        let dir = temp_dir("illegal");
        for name in ["", "  ", "..", "../escape", "a/b", "a\\b", "-x", ".hidden"] {
            let err = save_preset(&dir, name, &Config::default())
                .expect_err("an illegal name must be refused");
            assert!(
                matches!(err, PresetError::InvalidName { .. }),
                "{name}: {err:?}"
            );
        }
        assert!(list_presets(&dir).is_empty(), "nothing may be written");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_preset_can_be_applied_to_a_config_file() {
        let dir = temp_dir("apply");
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.message.text = "BEFORE".to_string();
        cfg.hotkey.windows = "Ctrl+Alt+F9".to_string();
        cfg.save_to(&path).expect("write the config");

        let preset = load_of(&sample_config());
        apply_preset_to_file(&path, &preset).expect("apply");

        let after = Config::load_unsanitized_from(&path).expect("read back");
        assert_eq!(after.message.text, "STORM");
        assert_eq!(after.hotkey.windows, "Ctrl+Alt+F9", "the hotkey survives");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A preset that cannot be spliced is refused **whole**: the six
    /// writes used to be sequential, so a mid-way failure left half the
    /// look applied (audit 2026-10-23 §F-051).
    #[test]
    fn a_preset_that_cannot_splice_applies_nothing() {
        let dir = temp_dir("apply-refused");
        let path = dir.join("config.toml");
        // A top-level inline `grass` key: the `[grass]` section splice is
        // impossible, and the tables spliced before it must not land.
        std::fs::write(
            &path,
            "grass = { tile = \"x\" }\n\n[message]\ntext = \"BEFORE\"\n",
        )
        .expect("write the config");
        let before = std::fs::read_to_string(&path).expect("read back");

        let preset = load_of(&sample_config());
        let err = apply_preset_to_file(&path, &preset).expect_err("refused whole");
        assert!(
            matches!(
                err,
                PresetError::ConfigWrite(ConfigError::TableWrite { .. })
            ),
            "{err:?}"
        );

        let after = std::fs::read_to_string(&path).expect("read back");
        assert_eq!(
            before, after,
            "not even the sky may land when the grass splice fails: {after}"
        );
        let cfg = Config::load_unsanitized_from(&path).expect("read back");
        assert_eq!(cfg.message.text, "BEFORE", "nothing was applied");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The one write of a preset apply keeps a `.bak` of the pre-preset
    /// state (audit 2026-10-23 §F-051).
    #[test]
    fn a_preset_apply_keeps_the_pre_preset_backup() {
        let dir = temp_dir("apply-backup");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[message]\ntext = \"BEFORE\"\n").expect("write the config");

        let preset = load_of(&sample_config());
        apply_preset_to_file(&path, &preset).expect("apply");

        let backup = std::fs::read_to_string(dir.join("config.toml.bak")).expect("the backup");
        assert!(
            backup.contains("text = \"BEFORE\""),
            "the backup is the pre-preset state: {backup}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The preset name is trimmed before it becomes a file name, so a
    /// trailing space can never produce a preset `list` cannot show
    /// (audit 2026-10-23 §F-064).
    #[test]
    fn a_preset_name_is_trimmed_before_it_becomes_a_file_name() {
        let dir = temp_dir("trim");
        let path = save_preset(&dir, "  stormy  ", &sample_config()).expect("save");
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("stormy.agnosispreset"),
            "the trimmed name is the file name"
        );
        assert_eq!(list_presets(&dir), vec!["stormy".to_string()]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The preset's manifest fragment carries no hotkey and no browsers —
    /// one capture policy with the package (audit 2026-10-23 §F-046).
    #[test]
    fn a_preset_manifest_carries_no_hotkey_or_browsers() {
        let dir = temp_dir("no-hotkey");
        let path = save_preset(&dir, "calm", &sample_config()).expect("save");
        let bytes = std::fs::read(&path).expect("read");
        let archive = ZipArchive::parse(bytes).expect("parse");
        let manifest_bytes = archive.read(MANIFEST_ENTRY).expect("the manifest");
        let text = std::str::from_utf8(manifest_bytes).expect("utf-8");
        assert!(
            !text.contains("hotkey"),
            "the preset's fragment must not carry the hotkey: {text}"
        );
        assert!(
            !text.contains("browsers"),
            "the preset's fragment must not carry the browser list: {text}"
        );
        assert!(
            !text.contains("Ctrl+Alt+F9") && !text.contains("mybrowser"),
            "no private value leaks into the preset: {text}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_preset_keeps_the_comments_inside_the_tables_it_writes() {
        // The table writer used to replace each section's whole body, so
        // a comment or an unknown key INSIDE [sky] or [message] was
        // thrown away when a preset applied (audit 2026-10-22
        // §P2-20).
        let dir = temp_dir("apply-comments");
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "[sky]
enabled = false # dusk for me
my_own_sky_key = 1

[message]
text = \"BEFORE\"
# my note about the message
",
        )
        .expect("write the config");

        let preset = load_of(&sample_config());
        apply_preset_to_file(&path, &preset).expect("apply");

        let after = std::fs::read_to_string(&path).expect("read back");
        assert!(
            after.contains("# dusk for me"),
            "the in-section comment survives: {after}"
        );
        assert!(
            after.contains("my_own_sky_key = 1"),
            "the in-section unknown key survives: {after}"
        );
        assert!(
            after.contains("# my note about the message"),
            "the comment under the written key survives: {after}"
        );
        let cfg = Config::load_unsanitized_from(&path).expect("read back");
        assert_eq!(cfg.message.text, "STORM", "the preset still lands");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_preset_is_reported() {
        let dir = temp_dir("broken");
        let path = dir.join("broken.agnosispreset");
        std::fs::write(&path, b"not a preset").expect("write");
        let err = load_preset(&path).expect_err("a broken preset is refused");
        assert!(matches!(err, PresetError::Zip(_)), "{err:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The config a preset describes, without touching the disk.
    fn load_of(cfg: &Config) -> Config {
        look_of(cfg)
    }
}
