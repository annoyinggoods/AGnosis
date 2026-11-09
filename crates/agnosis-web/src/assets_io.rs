//! The drawn art, and the two ways it arrives.
//!
//! The web build is **read-only** (roadmap Extension LATER §1.4): the
//! editor stays native. But the *art* travels: the native app exports a
//! package (`agnosis export out.agnosis`) and the web imports it.
//!
//! A browser has no asset folder, so the assets live in memory and the
//! scene's injectable loader ([`agnosis_core::scene::Scene::build`]) asks
//! this store for the asset by name. That is the same seam the native
//! app uses to read the user's folders, so the two cannot drift.
//!
//! The core APIs this uses, all **read-only**:
//!
//! * [`agnosis_core::manifest::Manifest::from_json`] and
//!   [`agnosis_core::manifest::Manifest::validate`] — the schema and its
//!   checks (an unknown version, an illegal name, a path that leaves the
//!   archive, an unknown slot);
//! * [`agnosis_core::manifest::Manifest::config`] and
//!   [`agnosis_core::manifest::Manifest::assignments_config`] — the config
//!   the manifest describes;
//! * [`agnosis_core::package::ZipArchive::parse`] and
//!   [`agnosis_core::package::ZipArchive::read`] — the store-only ZIP
//!   reader, which is the read half of the core's package support (the
//!   write half, `package::import_from`, writes to a folder, and a
//!   browser has no folder).

use agnosis_core::config::Config;
use agnosis_core::drawn::{DrawnDoc, DrawnError, DrawnKind, DrawnSprite};
use agnosis_core::manifest::{Manifest, MANIFEST_ENTRY};
use agnosis_core::package::ZipArchive;

/// The drawn assets the page carries.
///
/// WHY it is `Clone`: the scene's reload path asks the loader for the
/// assets **while** the scene is being reloaded, and the reload holds
/// the scene mutably — so the loader answers from a copy of the store
/// (audit 2026-10-21 §E8).
#[derive(Debug, Default, Clone)]
pub struct AssetStore {
    assets: Vec<(DrawnKind, String, DrawnSprite)>,
}

impl AssetStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// The asset of that kind and name.
    ///
    /// This is the loader the scene asks for: a missing asset is an
    /// **error**, not a panic, so the built-in art stays.
    pub fn load(&self, kind: DrawnKind, name: &str) -> Result<DrawnSprite, DrawnError> {
        self.find(kind, name)
            .cloned()
            .ok_or_else(|| DrawnError::NotFound {
                name: name.to_string(),
            })
    }

    /// Does the store carry that asset?
    pub fn exists(&self, kind: DrawnKind, name: &str) -> bool {
        self.find(kind, name).is_some()
    }

    /// Put an asset into the store, replacing an older one of the same
    /// name.
    pub fn insert(&mut self, kind: DrawnKind, name: String, sprite: DrawnSprite) {
        let slot = self
            .assets
            .iter()
            .position(|(k, n, _)| *k == kind && *n == name);
        match slot {
            Some(index) => self.assets[index].2 = sprite,
            None => self.assets.push((kind, name, sprite)),
        }
    }

    /// How many assets the store carries.
    pub fn len(&self) -> usize {
        self.assets.len()
    }

    /// Whether the store carries no asset at all.
    ///
    /// WHY it exists: a `len() == 0` check at the call sites reads as a
    /// guess; the pair is the Rust API convention (clippy's `len_without_is_empty`).
    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    /// The asset of that kind and name.
    fn find(&self, kind: DrawnKind, name: &str) -> Option<&DrawnSprite> {
        self.assets
            .iter()
            .find(|(k, n, _)| *k == kind && n == name)
            .map(|(_, _, sprite)| sprite)
    }
}

/// A package read into memory.
pub struct LoadedPackage {
    /// The manifest: the assignments and the config.
    pub manifest: Manifest,
    /// The assets the manifest describes.
    pub assets: Vec<(DrawnKind, String, DrawnSprite)>,
}

/// Read a manifest (the `agnosis config export` output).
pub fn read_manifest(text: &str) -> Result<Manifest, String> {
    let manifest = Manifest::from_json(text).map_err(|e| e.to_string())?;
    manifest.validate().map_err(|e| e.to_string())?;
    Ok(manifest)
}

/// Read a package (the `agnosis export out.agnosis` output).
pub fn read_package(bytes: Vec<u8>) -> Result<LoadedPackage, String> {
    let archive = ZipArchive::parse(bytes).map_err(|e| format!("not an agnosis package: {e}"))?;
    let manifest_bytes = archive
        .read(MANIFEST_ENTRY)
        .map_err(|e| format!("the package carries no manifest: {e}"))?;
    let text = String::from_utf8_lossy(manifest_bytes);
    let manifest = read_manifest(&text)?;
    let mut assets = Vec::new();
    for asset in &manifest.assets {
        let bytes = archive
            .read(&asset.file)
            .map_err(|e| format!("{}: {e}", asset.file))?;
        assets.push((
            asset.kind,
            asset.name.clone(),
            read_asset(asset.kind, bytes)?,
        ));
    }
    Ok(LoadedPackage { manifest, assets })
}

/// The runtime asset of one document.
fn read_asset(kind: DrawnKind, bytes: &[u8]) -> Result<DrawnSprite, String> {
    let text = String::from_utf8_lossy(bytes);
    let doc: DrawnDoc = serde_json::from_str(&text)
        .map_err(|e| format!("the {kind} document is not valid: {e}"))?;
    DrawnSprite::from_doc(&doc).map_err(|e| e.to_string())
}

/// Apply a manifest to the live app.
pub fn apply_manifest(json: &str) -> Result<String, String> {
    let manifest = read_manifest(json)?;
    apply(manifest, Vec::new())
}

/// Read a package and apply it to the live app.
pub fn load_package(bytes: &[u8]) -> Result<String, String> {
    let package = read_package(bytes.to_vec())?;
    apply(package.manifest, package.assets)
}

/// Apply a manifest and its assets to the live app.
fn apply(
    manifest: Manifest,
    assets: Vec<(DrawnKind, String, DrawnSprite)>,
) -> Result<String, String> {
    let Some(app) = crate::app::the_app() else {
        return Err("the parade is not running yet".to_string());
    };
    let mut state = app.borrow_mut();
    for (kind, name, sprite) in assets {
        state.sim.assets.insert(kind, name, sprite);
    }
    let cfg = config_of(&manifest, &state.sim.assets)?;
    // The scene is **reloaded in place**: the core's injectable reload
    // path (`Scene::apply_config_with`) reads the assets through the
    // loader, so the web's in-memory store answers it. The parade keeps
    // its phase, its balloons and its clock — a rebuilt scene would
    // restart the train at the left edge (audit 2026-10-21 §E8).
    let assets_store = state.sim.assets.clone();
    state
        .sim
        .scene
        .apply_config_with(&cfg, |kind, name| assets_store.load(kind, name), None);
    state.sim.cfg = cfg;
    Ok(summary(&manifest, &state.sim.assets))
}

/// The config a manifest describes.
///
/// The manifest's **fragment** is the config (the whole one: the
/// manifest is a lossless round trip), and its lifted **assignments**
/// table wins when it carries one. The repair pass then drops an
/// assignment whose asset the page does not carry — the same soft
/// repair the native app makes.
fn config_of(manifest: &Manifest, assets: &AssetStore) -> Result<Config, String> {
    let mut cfg = manifest.config().map_err(|e| e.to_string())?;
    if !manifest.assignments.is_empty() {
        cfg.assignments = manifest.assignments_config().map_err(|e| e.to_string())?;
    }
    let (mut cfg, _repairs) = cfg.sanitized_with_repairs(|kind, name| assets.exists(kind, name));
    // The native exit hint ("press ESC to exit") means nothing in a tab.
    cfg.exit_hint = crate::app::WEB_EXIT_HINT.to_string();
    Ok(cfg)
}

/// The one-line summary of an import.
fn summary(manifest: &Manifest, assets: &AssetStore) -> String {
    if assets.is_empty() {
        return format!(
            "applied {} ({} assignments, no assets)",
            manifest.name,
            manifest.assignments.len()
        );
    }
    format!(
        "applied {} ({} assignments, {} assets)",
        manifest.name,
        manifest.assignments.len(),
        assets.len()
    )
}
