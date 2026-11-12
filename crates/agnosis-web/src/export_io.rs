//! The exports, from a browser: the recorded parade becomes bytes the
//! page downloads (roadmap `REVIEW/ROADMAP_WASM.md` §5, milestone M2).
//!
//! The export machinery is the core's ([`agnosis_core::export`]) — the
//! recorder drives its **own** scene at the gate's geometry with a
//! fixed timestep, so the export is reproducible and independent of
//! the live parade on the screen. The one platform difference is where
//! the drawn art comes from: the shell hands folders over, this host
//! answers through the in-memory store — the injectable seam
//! (`record_with_loader`), the same seam the page's own parade uses,
//! so the export shows exactly what the page shows.
//!
//! The three artifacts match the native `agnosis export` verb:
//!
//! * the **self-contained page** (`.html`) — [`agnosis_core::export::html`]
//!   renders it; the page downloads the bytes and the visitor's
//!   `<a download>` lands it in their browser;
//! * the **animated GIF** (`.gif`) — [`agnosis_core::export::gif`]
//!   encodes the downscaled frames;
//! * the **art package** (`.agnosis`) — the store-only ZIP through
//!   [`agnosis_core::package::export_bytes`], so a visitor can
//!   round-trip their art: a package imported here can be downloaded
//!   and imported back into the native app.
//!
//! Everything here is **pure**: no clock, no threads, no file system —
//! the same rules the whole crate obeys.

use agnosis_core::config::Config;
use agnosis_core::drawn::{DrawnError, DrawnKind, DrawnSprite};
use agnosis_core::export::frames;
use agnosis_core::export::frames::MAX_FRAMES;
use agnosis_core::export::gif;
use agnosis_core::export::html;
use agnosis_core::manifest::Manifest;
use agnosis_core::package::{self, AssetPayload};

use crate::app::the_app;
use crate::assets_io::AssetStore;

/// The frames a recording holds: one loop of the parade.
///
/// The same count the native `agnosis export` derives
/// (`export_frame_count` in `src/cli.rs`): the config's `loop_seconds`
/// at the recording's fps, at least one frame, never over the core's
/// frame cap.
pub fn frame_count(cfg: &Config, fps: u32) -> u32 {
    let loop_frames = (cfg.loop_seconds * fps.max(1) as f32).round();
    (loop_frames.max(1.0) as u32).min(MAX_FRAMES)
}

/// The GIF's per-frame delay, in hundredths of a second.
///
/// WHY the `max(1)`: a delay of **zero** means "as fast as possible"
/// in most viewers, so a fast frame rate must not produce one (audit
/// 2026-10-21 §F14) — the same rule `export::write_gif` applies.
pub fn delay_cs(fps: u32) -> u16 {
    ((100 / fps.max(1)) as u16).max(1)
}

/// Run `f` with the live app's config and its **store**, borrowed.
///
/// The store is the shared handle the editor saves into too
/// (roadmap WASM §6); the borrow here lives only for this call — the
/// recorder inside `f` answers its loader from the same `&AssetStore`
/// reference, so the store is never copied for a recording.
fn with_live<T>(f: impl FnOnce(&Config, &AssetStore) -> Result<T, String>) -> Result<T, String> {
    let Some(app) = the_app() else {
        return Err("the parade is not running yet".to_string());
    };
    let state = app.borrow();
    let store = state.sim.assets.borrow();
    f(&state.sim.cfg, &store)
}

/// The loader over one copy of the store.
fn store_loader(
    store: &AssetStore,
) -> impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError> + '_ {
    move |kind, name| store.load(kind, name)
}

/// The self-contained page (`.html`), as bytes.
pub fn export_html(name: &str) -> Result<Vec<u8>, String> {
    with_live(|cfg, assets| {
        let fps = frames::DEFAULT_FPS;
        let count = frame_count(cfg, fps);
        let recording = frames::record_with_loader(cfg, count, fps, store_loader(assets))
            .map_err(|e| e.to_string())?;
        Ok(html::render_with_manifest(&recording, cfg, name).into_bytes())
    })
}

/// The animated GIF (`.gif`), as bytes.
pub fn export_gif(scale: u32) -> Result<Vec<u8>, String> {
    with_live(|cfg, assets| {
        let fps = frames::DEFAULT_FPS;
        let count = frame_count(cfg, fps);
        let picture =
            frames::render_pixels_with_loader(cfg, count, fps, scale, store_loader(assets))
                .map_err(|e| e.to_string())?;
        gif::encode(&picture, delay_cs(fps)).map_err(|e| e.to_string())
    })
}

/// The art package (`.agnosis`), as bytes.
///
/// The manifest carries the config's **look** and the store's assets —
/// the same shape the native `agnosis export` writes, so the native
/// app imports the download back.
pub fn export_package(name: &str) -> Result<Vec<u8>, String> {
    with_live(|cfg, assets| {
        let mut manifest = Manifest::from_look(&package::capture_look(cfg), name);
        let mut payloads = Vec::new();
        for (kind, name, sprite) in assets.entries() {
            manifest.add_asset(*kind, name);
            let bytes = serde_json::to_vec(&sprite.to_doc())
                .map_err(|e| format!("{name}: the document does not serialise: {e}"))?;
            payloads.push(AssetPayload {
                kind: *kind,
                name: name.clone(),
                bytes,
            });
        }
        package::export_bytes(&manifest, &payloads).map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The default config records one loop: 20 s at 12 fps.
    #[test]
    fn the_frame_count_is_one_loop_of_the_parade() {
        let cfg = Config::default();
        assert_eq!(frame_count(&cfg, frames::DEFAULT_FPS), 240);
    }

    /// At least one frame, and never over the core's cap.
    #[test]
    fn the_frame_count_is_bounded() {
        let cfg = Config::default();
        assert_eq!(frame_count(&cfg, 1), cfg.loop_seconds.round() as u32);
        assert_eq!(
            frame_count(&cfg, u32::MAX),
            MAX_FRAMES,
            "an enormous fps is capped"
        );
    }

    /// The GIF delay matches the native rule: a zero delay is a fast
    /// viewer, never a written file.
    #[test]
    fn the_gif_delay_is_never_zero() {
        assert_eq!(delay_cs(frames::DEFAULT_FPS), 8, "12 fps delays 8 cs");
        assert_eq!(delay_cs(0), 100, "a zero fps clamps to 1 fps: one second");
        assert_eq!(delay_cs(200), 1, "a delay of zero becomes one");
    }
}
