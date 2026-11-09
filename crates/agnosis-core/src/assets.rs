//! Where the app's own assets live.
//!
//! The core never resolves a platform location: the shell fills these
//! structures in and hands them over. An empty [`AssetDirs`] means "the
//! built-in art only" — which is what a headless render of a default
//! config shows.

use std::path::{Path, PathBuf};

use crate::drawn::{DrawnError, DrawnKind, DrawnSprite};

/// The folders the drawn assets live in.
///
/// The default value is **empty on purpose**: an empty path means "there
/// is no folder of that kind", and [`DrawnDirs::load`] then reports
/// [`DrawnError::NotFound`] instead of looking the asset up in the
/// process's working directory (audit 2026-10-21 §F3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrawnDirs {
    /// The user-drawn walking sprites.
    pub sprites: PathBuf,
    /// The user-drawn balloons.
    pub balloons: PathBuf,
    /// The user-drawn grass tiles.
    pub grass: PathBuf,
    /// The user-drawn duck decorations.
    pub decorations: PathBuf,
}

impl DrawnDirs {
    /// The folder that holds the assets of `kind`.
    pub fn dir_for(&self, kind: DrawnKind) -> &Path {
        match kind {
            DrawnKind::Sprite => &self.sprites,
            DrawnKind::Balloon => &self.balloons,
            DrawnKind::Grass => &self.grass,
            DrawnKind::Decoration => &self.decorations,
        }
    }

    /// Load an asset of `kind` by name.
    ///
    /// An **empty** folder path means "there is no folder of that kind":
    /// the load is refused with [`DrawnError::NotFound`]. It must not
    /// fall back to the working directory — `Path::new("").join("cat.json")`
    /// is the *relative* path `cat.json`, so the lookup would land in
    /// whatever directory the process happens to run in (audit 2026-10-21
    /// §F3).
    pub fn load(&self, kind: DrawnKind, name: &str) -> Result<DrawnSprite, DrawnError> {
        let dir = self.dir_for(kind);
        if dir.as_os_str().is_empty() {
            return Err(DrawnError::NotFound {
                name: name.to_string(),
            });
        }
        crate::drawn::load_from(dir, kind, name)
    }
}

/// Everything the scene loads from disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AssetDirs {
    /// The drawn-asset folders. `None` means "no drawn art at all".
    pub drawn: Option<DrawnDirs>,
    /// The folder the MIDI tracks live in. `None` means "no track".
    pub tracks: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty folder path means "no folder of that kind": the load is
    /// refused instead of looking the asset up in the working directory
    /// (audit 2026-10-21 §F3).
    #[test]
    fn an_empty_drawn_folder_is_not_the_working_directory() {
        let dirs = DrawnDirs::default();
        for kind in [
            DrawnKind::Sprite,
            DrawnKind::Balloon,
            DrawnKind::Grass,
            DrawnKind::Decoration,
        ] {
            let err = dirs
                .load(kind, "cat")
                .expect_err("an empty folder must not find anything");
            assert!(
                matches!(err, DrawnError::NotFound { .. }),
                "the load must report a missing asset: {err}"
            );
        }
    }
}
