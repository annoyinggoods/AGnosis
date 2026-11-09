//! The export surface: the artifacts you can send to somebody
//! (roadmap Extension 4 §4).
//!
//! Three artifacts come out of the same place, so the machinery is
//! shared:
//!
//! * [`frames`] — the **frame recorder**: the scene becomes recorded
//!   frames, on a fixed timestep and with a fixed configuration, so the
//!   export is reproducible;
//! * [`codec`] — the cell-level run-length codec the HTML page decodes;
//! * [`html`] — the self-contained page;
//! * [`gif`] — the animated GIF.
//!
//! The exports are **pure functions of the scene state**: they read the
//! scene, they never mutate it, and they never run on the UI thread's
//! critical path (the frame render is a worker).
//!
//! The manifest is **not** repeated here: [`crate::manifest`] owns the
//! schema, and the page carries that very manifest.

pub mod codec;
pub mod frames;
pub mod gif;
pub mod html;

use std::path::Path;

use crate::assets::AssetDirs;
use crate::config::Config;
use crate::export::frames::ExportError;

/// The name the artifacts carry: what the page says it is.
pub const DEFAULT_NAME: &str = "a ducks parade";

/// Write the self-contained HTML page.
///
/// The file is one `.html` with the frames, the glyphs and the manifest
/// inside it: it opens from a `file://` URL, from a chat app, from a
/// USB stick.
///
/// `assets` is where the drawn art and the MIDI tracks live: the export
/// renders the **user's** parade, so the shell hands the folders over.
pub fn write_html(
    path: &Path,
    cfg: &Config,
    name: &str,
    frame_count: u32,
    fps: u32,
    assets: &AssetDirs,
) -> Result<u64, ExportError> {
    let recording = frames::record(cfg, frame_count, fps, assets)?;
    html::write(path, &recording, cfg, name, assets)
}

/// Write the animated GIF.
///
/// The picture is the app's own render, downscaled by `scale`: a factor
/// of 2 halves the cell pitch and keeps the 16:9 shape.
///
/// `assets` is where the drawn art and the MIDI tracks live: the export
/// renders the **user's** parade, so the shell hands the folders over.
pub fn write_gif(
    path: &Path,
    cfg: &Config,
    frame_count: u32,
    fps: u32,
    scale: u32,
    assets: &AssetDirs,
) -> Result<u64, ExportError> {
    let picture = frames::render_pixels(cfg, frame_count, fps, scale, assets)?;
    // WHY the `max(1)`: a delay of **zero** means "as fast as possible"
    // in most viewers, so a fast frame rate (`fps > 100`) must not
    // produce one (audit 2026-10-21 §F14).
    let delay_cs = ((100 / fps.max(1)) as u16).max(1);
    let bytes = gif::encode(&picture, delay_cs).map_err(|e| ExportError::Gif(e.to_string()))?;
    std::fs::write(path, &bytes).map_err(|source| ExportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(bytes.len() as u64)
}

/// The export the file's extension asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// The self-contained HTML page.
    Html,
    /// The animated GIF.
    Gif,
}

impl Format {
    /// The format a file name asks for.
    ///
    /// `None` for anything else: the **package** export (the drawn art,
    /// the assignments and the look) owns every other extension.
    pub fn for_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?.to_ascii_lowercase();
        match extension.as_str() {
            "html" | "htm" => Some(Format::Html),
            "gif" => Some(Format::Gif),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("tests.rs");

    #[test]
    fn the_extension_chooses_the_format() {
        assert_eq!(
            Format::for_path(Path::new("parade.html")),
            Some(Format::Html)
        );
        assert_eq!(
            Format::for_path(Path::new("parade.htm")),
            Some(Format::Html)
        );
        assert_eq!(Format::for_path(Path::new("parade.gif")), Some(Format::Gif));
        assert_eq!(Format::for_path(Path::new("parade.GIF")), Some(Format::Gif));
    }

    #[test]
    fn every_other_extension_is_the_package() {
        assert_eq!(Format::for_path(Path::new("parade.agnosis")), None);
        assert_eq!(Format::for_path(Path::new("parade")), None);
        assert_eq!(Format::for_path(Path::new("parade.png")), None);
    }
}
