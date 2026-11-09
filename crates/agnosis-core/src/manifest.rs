//! The manifest schema: "what is a creation?" (roadmap Extension 1 §7.2).
//!
//! ONE schema serves four features, and that is the whole point of the
//! module:
//!
//! * the **package** (§3.3) is the manifest plus an archive;
//! * the **preset** (§3.4) is the manifest with another extension and
//!   another merge policy;
//! * the **shareable screenshot** (§1.6) is the manifest plus one frame;
//! * **`agnosis config export`** (§1.4) is the manifest with no assets.
//!
//! ```text
//! manifest = { version, name, assets[], assignments, config_fragment }
//! ```
//!
//! WHY the validation is this strict (roadmap §7.2 calls the merge policy
//! "the real design work", because a bug there destroys a user's
//! configuration):
//!
//! * an asset **name** becomes a **file name** on import, so it goes
//!   through [`crate::drawn::validate_name`] — a name that can escape the
//!   asset folder is a path traversal, not a name;
//! * an asset's `file` is a path **inside** the archive, so it must stay
//!   inside it: no absolute path, no `..`, no backslash;
//! * a version this build does not know is refused, never misread.
//!
//! The module is pure data plus JSON: no I/O beyond the strings it is
//! given, so the whole schema is testable without a display and without a
//! filesystem.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::config::{AssignmentSlot, AssignmentsConfig, Config};
use crate::drawn::DrawnKind;

/// The manifest version this build writes and understands.
pub const MANIFEST_VERSION: u32 = 1;

/// The name of the manifest entry inside a package or a preset.
pub const MANIFEST_ENTRY: &str = "manifest.json";

/// The longest accepted manifest name.
const MAX_MANIFEST_NAME_LEN: usize = 64;

/// One asset a manifest carries.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ManifestAsset {
    /// What kind of asset it is: a sprite, a balloon or a grass tile.
    pub kind: DrawnKind,
    /// The asset's name. It becomes a **file name** on import, so it is
    /// validated with [`crate::drawn::validate_name`].
    pub name: String,
    /// The path of the document **inside** the archive or preset.
    ///
    /// It is a path *inside*, never a path on the user's disk: it must
    /// stay under the archive root.
    pub file: String,
}

/// The manifest: the description of a creation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// The manifest format version. A version this build does not know is
    /// refused, never misread.
    pub version: u32,
    /// The name of the creation — a package's or a preset's title.
    pub name: String,
    /// The assets the creation carries. Empty for a preset (a preset is a
    /// *look*, not art) and for `agnosis config export`.
    #[serde(default)]
    pub assets: Vec<ManifestAsset>,
    /// The assignment table: a parade slot key (`small_3`) to an asset
    /// name.
    #[serde(default)]
    pub assignments: BTreeMap<String, String>,
    /// The config the creation describes.
    ///
    /// A TOML value, so the manifest stays a *document* that can be read
    /// and edited without the binary that wrote it.
    #[serde(default = "empty_fragment")]
    pub config_fragment: toml::Value,
}

/// An empty config fragment: a manifest without a config still parses.
fn empty_fragment() -> toml::Value {
    toml::Value::Table(toml::Table::new())
}

impl Manifest {
    /// A new, empty manifest of this build's version.
    pub fn new(name: &str) -> Self {
        Self {
            version: MANIFEST_VERSION,
            name: name.to_string(),
            assets: Vec::new(),
            assignments: BTreeMap::new(),
            config_fragment: empty_fragment(),
        }
    }

    /// The manifest of a configuration: the whole config becomes the
    /// fragment, and the assignment table is lifted out of it.
    ///
    /// The fragment is the **whole** config, so a manifest round trip is
    /// lossless. A *shared* file (a package, a preset) wants less than
    /// that: it owns the look only, so it captures through
    /// [`crate::package::capture_look`] and writes itself through
    /// [`Manifest::from_look`] — the hotkey chords and the browser list
    /// stay on the machine they belong to (audit 2026-10-23 §F-046).
    ///
    /// A capture that cannot happen (the config is a plain struct of plain
    /// values) is reported and leaves an empty fragment, so a manifest is
    /// always returned.
    pub fn from_config(cfg: &Config, name: &str) -> Self {
        match Self::try_from_config(cfg, name) {
            Ok(manifest) => manifest,
            Err(e) => {
                crate::trace::warn(&format!("manifest: could not capture the config: {e}"));
                let mut manifest = Self::new(name);
                manifest.assignments = assignments_of(cfg);
                manifest
            }
        }
    }

    /// The manifest of a **look**: the capture of
    /// [`crate::package::capture_look`], with the two private tables
    /// removed from the fragment.
    ///
    /// WHY the removal: `capture_look` neutralizes the hotkey and the
    /// browser list to the defaults, but a serialized `Config` still
    /// *writes* those tables — and a file that is about to be sent to
    /// somebody else must not carry them at all, not even as defaults
    /// (audit 2026-10-23 §F-046: one capture policy, no exceptions).
    pub fn from_look(cfg: &Config, name: &str) -> Self {
        let mut manifest = Self::from_config(cfg, name);
        remove_private_tables(&mut manifest.config_fragment);
        manifest
    }

    /// The manifest of a configuration, reporting a capture failure.
    pub fn try_from_config(cfg: &Config, name: &str) -> Result<Self, ManifestError> {
        let mut manifest = Self::new(name);
        manifest.config_fragment = toml::Value::try_from(cfg)
            .map_err(|source| ManifestError::FragmentSerialize { source })?;
        manifest.assignments = assignments_of(cfg);
        Ok(manifest)
    }

    /// Add an asset to the manifest, using the canonical in-archive path.
    pub fn add_asset(&mut self, kind: DrawnKind, name: &str) {
        self.assets.push(ManifestAsset {
            kind,
            name: name.to_string(),
            file: Self::asset_file(kind, name),
        });
    }

    /// The canonical path of an asset **inside** an archive.
    pub fn asset_file(kind: DrawnKind, name: &str) -> String {
        format!("assets/{}/{}.json", kind.prefix(), name)
    }

    /// The asset of that kind and name, if the manifest describes one.
    pub fn asset(&self, kind: DrawnKind, name: &str) -> Option<&ManifestAsset> {
        self.assets
            .iter()
            .find(|asset| asset.kind == kind && asset.name == name)
    }

    /// The manifest as JSON.
    pub fn to_json(&self) -> Result<String, ManifestError> {
        serde_json::to_string_pretty(self).map_err(|source| ManifestError::Serialize { source })
    }

    /// A manifest from JSON.
    ///
    /// This **parses**; it does not validate. Call [`Manifest::validate`]
    /// before trusting what came out of it.
    pub fn from_json(text: &str) -> Result<Self, ManifestError> {
        serde_json::from_str(text).map_err(|source| ManifestError::Parse { source })
    }

    /// Check the manifest against everything this build knows.
    ///
    /// Rejects: an unknown version, a name that is not a legal asset name,
    /// an asset described twice, a path that leaves the archive root, an
    /// unknown parade slot and an asset name that is not a legal name.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.version != MANIFEST_VERSION {
            return Err(ManifestError::UnsupportedVersion {
                version: self.version,
                supported: MANIFEST_VERSION,
            });
        }
        validate_manifest_name(&self.name)?;
        self.validate_assets()?;
        self.validate_assignments()
    }

    /// Every asset name and file path must be legal, and no asset may be
    /// described twice.
    fn validate_assets(&self) -> Result<(), ManifestError> {
        for (index, asset) in self.assets.iter().enumerate() {
            if crate::drawn::validate_name(&asset.name).is_err() {
                return Err(ManifestError::InvalidAssetName {
                    name: asset.name.clone(),
                });
            }
            if !is_safe_relative_path(&asset.file) {
                return Err(ManifestError::UnsafePath {
                    file: asset.file.clone(),
                });
            }
            let earlier = &self.assets[..index];
            if earlier
                .iter()
                .any(|other| other.kind == asset.kind && other.name == asset.name)
            {
                return Err(ManifestError::DuplicateAsset {
                    name: asset.name.clone(),
                });
            }
            if earlier.iter().any(|other| other.file == asset.file) {
                return Err(ManifestError::DuplicateFile {
                    file: asset.file.clone(),
                });
            }
        }
        Ok(())
    }

    /// Every slot must be a real parade slot and every asset name a legal
    /// one: the table is written straight into the user's config.
    fn validate_assignments(&self) -> Result<(), ManifestError> {
        for (slot, name) in &self.assignments {
            if AssignmentSlot::parse(slot).is_err() {
                return Err(ManifestError::UnknownSlot { slot: slot.clone() });
            }
            if crate::drawn::validate_name(name).is_err() {
                return Err(ManifestError::InvalidAssetName { name: name.clone() });
            }
        }
        Ok(())
    }

    /// The config the fragment describes.
    pub fn config(&self) -> Result<Config, ManifestError> {
        self.config_fragment
            .clone()
            .try_into()
            .map_err(|source| ManifestError::FragmentParse { source })
    }

    /// The assignment table as a config table.
    pub fn assignments_config(&self) -> Result<AssignmentsConfig, ManifestError> {
        let mut table = AssignmentsConfig::default();
        for (slot, name) in &self.assignments {
            let parsed = AssignmentSlot::parse(slot)
                .map_err(|_| ManifestError::UnknownSlot { slot: slot.clone() })?;
            table.set(parsed, Some(name.clone()));
        }
        Ok(table)
    }
}

/// The assignment table of a config: the slots that carry a name.
fn assignments_of(cfg: &Config) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for (slot, value) in cfg.assignments.entries() {
        let Some(value) = value else {
            continue;
        };
        map.insert(slot, value.to_string());
    }
    map
}

/// Remove the tables a shared file must not carry from a fragment.
///
/// The hotkey and the browser list are the **user's machine's** settings,
/// not the creation's look: a package or a preset that carried them would
/// either leak them or reset the recipient's own (audit 2026-10-23
/// §F-046).
fn remove_private_tables(fragment: &mut toml::Value) {
    let toml::Value::Table(table) = fragment else {
        return;
    };
    table.remove("hotkey");
    table.remove("browsers");
}

/// Is `path` a path that stays **inside** an archive?
///
/// The rules, all of them about a path that must never leave the archive
/// root: it is relative, it has no `.` or `..` component, no empty
/// component, no backslash (a Windows separator would split the path on
/// Windows and not on Linux), no drive letter and no control character.
pub fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return false;
    }
    if path.chars().any(|ch| ch.is_control()) {
        return false;
    }
    path.split('/')
        .all(|part| !part.is_empty() && part != "." && part != "..")
}

/// A manifest name must be printable and short: it is a title, and it
/// ends up in a file name.
fn validate_manifest_name(name: &str) -> Result<(), ManifestError> {
    let trimmed = name.trim();
    let legal = !trimmed.is_empty()
        && trimmed.len() <= MAX_MANIFEST_NAME_LEN
        && !trimmed.chars().any(|ch| ch.is_control());
    if !legal {
        return Err(ManifestError::InvalidName {
            name: name.to_string(),
        });
    }
    Ok(())
}

/// Everything that can go wrong while building, reading or checking a
/// manifest.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The manifest was written by a build this one does not know.
    #[error("unsupported manifest version {version} (this build supports version {supported})")]
    UnsupportedVersion { version: u32, supported: u32 },
    /// The manifest's own name is not usable.
    #[error("illegal manifest name {name:?}")]
    InvalidName { name: String },
    /// An asset name is not a legal asset name (path traversal proof).
    #[error("illegal asset name {name:?} in the manifest")]
    InvalidAssetName { name: String },
    /// The same asset is described twice.
    #[error("the manifest describes the asset {name:?} twice")]
    DuplicateAsset { name: String },
    /// Two assets point at the same file inside the archive.
    #[error("the manifest points two assets at {file:?}")]
    DuplicateFile { file: String },
    /// An asset's path leaves the archive root.
    #[error("unsafe archive path {file:?} in the manifest")]
    UnsafePath { file: String },
    /// An assignment names a slot the parade does not have.
    #[error("unknown parade slot {slot:?} in the manifest")]
    UnknownSlot { slot: String },
    /// The manifest could not be written as JSON.
    #[error("could not serialize the manifest: {source}")]
    Serialize {
        #[source]
        source: serde_json::Error,
    },
    /// The manifest is not valid JSON.
    #[error("could not parse the manifest: {source}")]
    Parse {
        #[source]
        source: serde_json::Error,
    },
    /// The config fragment could not be captured.
    #[error("could not serialize the config fragment: {source}")]
    FragmentSerialize {
        #[source]
        source: toml::ser::Error,
    },
    /// The config fragment is not a config.
    #[error("could not parse the config fragment: {source}")]
    FragmentParse {
        #[source]
        source: toml::de::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config with one assignment and a distinctive look.
    fn sample_config() -> Config {
        let mut cfg = Config::default();
        cfg.sky.enabled = true;
        cfg.sky.time_of_day = 0.75;
        cfg.message.text = "HELLO".to_string();
        cfg.assignments.set(
            AssignmentSlot::parse("small_3").expect("small_3"),
            Some("cat".to_string()),
        );
        cfg
    }

    #[test]
    fn the_version_is_the_one_this_build_writes() {
        let manifest = Manifest::new("x");
        assert_eq!(manifest.version, MANIFEST_VERSION);
        assert_eq!(manifest.assets.len(), 0);
        assert!(manifest.assignments.is_empty());
    }

    #[test]
    fn from_config_captures_the_fragment_and_the_assignments() {
        let cfg = sample_config();
        let manifest = Manifest::from_config(&cfg, "my pack");
        assert_eq!(manifest.name, "my pack");
        assert_eq!(manifest.assignments.len(), 1);
        assert_eq!(
            manifest.assignments.get("small_3").map(String::as_str),
            Some("cat")
        );
        // The fragment is the whole config, so it round trips.
        let back = manifest.config().expect("the fragment is a config");
        assert_eq!(back, cfg, "the config must survive the manifest");
    }

    #[test]
    fn the_json_round_trip_is_lossless() {
        let mut manifest = Manifest::from_config(&sample_config(), "my pack");
        manifest.add_asset(DrawnKind::Sprite, "sprite-1");
        manifest.add_asset(DrawnKind::Balloon, "balloon-1");
        let json = manifest.to_json().expect("serialize");
        let back = Manifest::from_json(&json).expect("deserialize");
        assert_eq!(back, manifest, "the manifest must survive JSON unchanged");
        assert!(json.contains("\"version\""), "{json}");
        assert!(json.contains("\"config_fragment\""), "{json}");
    }

    #[test]
    fn a_valid_manifest_passes() {
        let mut manifest = Manifest::from_config(&sample_config(), "my pack");
        manifest.add_asset(DrawnKind::Sprite, "sprite-1");
        manifest.validate().expect("a good manifest validates");
    }

    #[test]
    fn a_future_version_is_refused() {
        let mut manifest = Manifest::new("x");
        manifest.version = MANIFEST_VERSION + 1;
        let err = manifest
            .validate()
            .expect_err("a future version is refused");
        assert!(
            matches!(err, ManifestError::UnsupportedVersion { .. }),
            "{err:?}"
        );
        assert!(err
            .to_string()
            .contains(&(MANIFEST_VERSION + 1).to_string()));
    }

    #[test]
    fn an_illegal_asset_name_is_refused() {
        for name in ["../escape", "a/b", "a\\b", "..", "", "Cat", "dot.name"] {
            let mut manifest = Manifest::new("x");
            manifest.add_asset(DrawnKind::Sprite, name);
            let err = manifest.validate().expect_err("an illegal name is refused");
            assert!(
                matches!(err, ManifestError::InvalidAssetName { .. }),
                "{name}: {err:?}"
            );
        }
    }

    #[test]
    fn a_duplicate_asset_is_refused() {
        let mut manifest = Manifest::new("x");
        manifest.add_asset(DrawnKind::Sprite, "sprite-1");
        manifest.add_asset(DrawnKind::Sprite, "sprite-1");
        let err = manifest.validate().expect_err("a duplicate is refused");
        assert!(
            matches!(err, ManifestError::DuplicateAsset { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn the_same_name_in_two_kinds_is_not_a_duplicate() {
        let mut manifest = Manifest::new("x");
        manifest.add_asset(DrawnKind::Sprite, "thing");
        manifest.add_asset(DrawnKind::Balloon, "thing");
        manifest.validate().expect("two kinds may share a name");
    }

    #[test]
    fn an_unsafe_archive_path_is_refused() {
        for file in [
            "/etc/passwd",
            "../escape.json",
            "assets/../../escape.json",
            "C:\\windows\\evil.json",
            "assets\\sprite\\x.json",
            "",
        ] {
            let manifest = Manifest {
                version: MANIFEST_VERSION,
                name: "x".to_string(),
                assets: vec![ManifestAsset {
                    kind: DrawnKind::Sprite,
                    name: "sprite-1".to_string(),
                    file: file.to_string(),
                }],
                assignments: BTreeMap::new(),
                config_fragment: empty_fragment(),
            };
            let err = manifest.validate().expect_err("an unsafe path is refused");
            assert!(
                matches!(err, ManifestError::UnsafePath { .. }),
                "{file}: {err:?}"
            );
        }
    }

    #[test]
    fn an_unknown_slot_is_refused() {
        let mut manifest = Manifest::new("x");
        manifest
            .assignments
            .insert("small_99".to_string(), "cat".to_string());
        let err = manifest.validate().expect_err("an unknown slot is refused");
        assert!(matches!(err, ManifestError::UnknownSlot { .. }), "{err:?}");

        let mut manifest = Manifest::new("x");
        manifest
            .assignments
            .insert("small_3".to_string(), "../escape".to_string());
        let err = manifest.validate().expect_err("an illegal name is refused");
        assert!(
            matches!(err, ManifestError::InvalidAssetName { .. }),
            "{err:?}"
        );
    }

    #[test]
    fn the_assignments_become_a_config_table() {
        let mut manifest = Manifest::new("x");
        manifest
            .assignments
            .insert("small_3".to_string(), "cat".to_string());
        manifest
            .assignments
            .insert("big_1".to_string(), "dog".to_string());
        let table = manifest.assignments_config().expect("the table");
        assert_eq!(table.small(3), Some("cat"));
        assert_eq!(table.big(1), Some("dog"));
        assert_eq!(table.small(1), None);
    }

    #[test]
    fn the_canonical_asset_path_stays_inside_the_archive() {
        let file = Manifest::asset_file(DrawnKind::Sprite, "sprite-1");
        assert_eq!(file, "assets/sprite/sprite-1.json");
        assert!(is_safe_relative_path(&file));
        assert!(!is_safe_relative_path("../x"));
        assert!(!is_safe_relative_path("/x"));
        assert!(!is_safe_relative_path("a\\b"));
        assert!(!is_safe_relative_path("a//b"));
        assert!(!is_safe_relative_path("a/./b"));
    }

    #[test]
    fn a_manifest_without_a_config_still_parses() {
        let json = r#"{"version":1,"name":"bare"}"#;
        let manifest = Manifest::from_json(json).expect("a bare manifest parses");
        assert!(manifest.config_fragment.is_table());
        assert!(manifest.assets.is_empty());
    }
}
