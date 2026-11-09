//! The frame recorder (roadmap Extension 4 §4).
//!
//! The recorder drives the scene with a **fixed timestep** and a fixed
//! configuration, so the same parade always records the same frames.
//! That is what makes an export reproducible: the same parade exports
//! byte-identical files, twice.
//!
//! The recorder is **not** the app's frame loop: it builds its own
//! scene, at the gate's geometry (1920x1080 -> 960x360 cells at the
//! 2x3 pitch), and it never touches the user's window. The export runs
//! on a worker thread, so the parade keeps marching while an export is
//! in progress (roadmap §4.1).

use std::path::PathBuf;

use crate::assets::AssetDirs;
use crate::config::Config;
use crate::export::codec::{CodecError, Encoder};
use crate::font::Font;
use crate::scene::Scene;

/// The monitor the recording is made at.
///
/// The gate's geometry: 1920x1080 resolves to 960x360 cells at the
/// 2x3 pitch, and that is the geometry the selftest gate pins.
const RECORD_MONITOR_W: u32 = 1920;
/// The monitor's height.
const RECORD_MONITOR_H: u32 = 1080;

/// The frames per second the export records at.
///
/// The roadmap's budget (a 20 s loop under 1 MB) is written for 12 fps:
/// the parade's gait is 6 fps, so 12 fps shows every pose twice.
pub const DEFAULT_FPS: u32 = 12;

/// Install the configured backdrop tiles into the scene.
///
/// The conversion is pure and lives in [`crate::backdrop`]: the export
/// runs off the UI thread, so it converts in place — and it then shows
/// the same picture the app does. The pictures are **absolute** paths:
/// the shell resolved the configured names before the core saw them.
fn install_backdrops(scene: &mut Scene, cfg: &Config) {
    let tiles = crate::backdrop::convert_layers(&cfg.backdrop);
    scene.set_backdrops(tiles);
}

/// The largest number of frames a recording may hold.
///
/// A 20 s loop at 12 fps is 240 frames. The cap is generous but finite:
/// an unbounded frame count is a disk-filling loop (the same lesson as
/// `agnosis selftest --frames`).
pub const MAX_FRAMES: u32 = 3600;

/// Everything that can go wrong while recording or writing an export.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The configuration does not describe a usable grid.
    #[error("config error: {0}")]
    Config(String),
    /// The embedded font failed to load.
    #[error("font error: {0}")]
    Font(String),
    /// The frame codec failed.
    #[error("the frame codec failed: {0}")]
    Codec(#[from] CodecError),
    /// A file could not be read or written.
    #[error("io error at {path}: {source}")]
    Io {
        /// The file that could not be written.
        path: PathBuf,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// The GIF encoder failed.
    #[error("the gif encoder failed: {0}")]
    Gif(String),
}

/// A recorded parade: the geometry and the encoded blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recording {
    /// The grid's width in cells.
    pub cols: usize,
    /// The grid's height in cells.
    pub rows: usize,
    /// The cell's width in pixels.
    pub cell_w: u32,
    /// The cell's height in pixels.
    pub cell_h: u32,
    /// The frames per second.
    pub fps: u32,
    /// How many frames were recorded.
    pub frame_count: u32,
    /// The encoded blob: the header, the palette, the glyphs, the layers
    /// and the frames.
    pub blob: Vec<u8>,
}

/// The config the exports record with.
///
/// The one change is the **exit hint** (audit 2026-10-23 F-154): an
/// exported file is not the app — there is no ESC to press — so the
/// *default* hint ("press ESC to exit", or a blank one, which the
/// config's repair folds back to the default) is replaced with the
/// page's own signature, `MADE WITH DUCKS`. A hint the user wrote is
/// kept verbatim: the override is only for the words the app ships.
pub fn export_config(cfg: &Config) -> Config {
    let mut cfg = cfg.clone();
    let hint = cfg.exit_hint.trim();
    let is_the_default =
        hint.is_empty() || hint.eq_ignore_ascii_case(crate::config::DEFAULT_EXIT_HINT);
    if is_the_default {
        cfg.exit_hint = EXPORT_HINT.to_string();
    }
    cfg
}

/// The exit hint the exports replace the default one with.
pub const EXPORT_HINT: &str = "MADE WITH DUCKS";

/// Record the parade into a blob.
///
/// The recording is a **pure function** of the configuration: the same
/// config records the same blob, every time.
///
/// `assets` is where the drawn art and the MIDI tracks live: the export
/// renders the **user's** parade, so the shell hands the folders over.
pub fn record(
    cfg: &Config,
    frames: u32,
    fps: u32,
    assets: &AssetDirs,
) -> Result<Recording, ExportError> {
    record_with_hud(cfg, frames, fps, assets, None)
}

/// Record the parade into a blob, with the HUD the app shows.
///
/// `counter_line` is the "days since the ducks" line the popup draws in
/// its bottom-left corner; `None` records without it. The parameter is
/// **additive** — [`record`] is this function with `None` — so a caller
/// that does not care about the counter keeps compiling (audit
/// 2026-10-23 F-142).
///
/// The rest of the HUD travels on its own: the exit hint is part of the
/// config ([`export_config`]), and the `[message]` caption is baked into
/// the blob as a static band (audit F-140).
pub fn record_with_hud(
    cfg: &Config,
    frames: u32,
    fps: u32,
    assets: &AssetDirs,
    counter_line: Option<&str>,
) -> Result<Recording, ExportError> {
    if frames == 0 {
        return Err(ExportError::Config(
            "a recording needs at least one frame".into(),
        ));
    }
    if frames > MAX_FRAMES {
        return Err(ExportError::Config(format!(
            "{frames} frames is over the {MAX_FRAMES} frame limit"
        )));
    }
    let fps = fps.max(1);
    let cfg = export_config(cfg);
    let (cols, rows, cell_w, cell_h) =
        cfg.grid_for_monitor(RECORD_MONITOR_W, RECORD_MONITOR_H, 1.0);
    let mut font = Font::new(cell_w, cell_h).map_err(|e| ExportError::Font(e.to_string()))?;
    let mut scene = Scene::new_with_assets(&cfg, cols, rows, cell_w, cell_h, assets);
    // The backdrop pictures are converted **here**, on the export's own
    // thread: the conversion is an image decode, and the export already
    // runs off the UI thread. The pictures are **absolute** paths: the
    // shell resolved the configured names before the core saw them.
    install_backdrops(&mut scene, &cfg);
    scene.set_counter_line(counter_line.map(|line| line.to_string()));
    let ground_row = scene.ground_row().max(0) as usize;
    let mut encoder = Encoder::new(cols, rows, cell_w, cell_h, ground_row, fps);
    let layers = scene.backdrop_layers();
    encoder.set_layers(&layers);
    // The message band is **static art** like the tiles: it is captured
    // once, before the first frame, and every frame shows the same
    // caption at offset 0 (audit 2026-10-23 F-140). The band is drawn
    // by the scene's redraw, which `advance` runs after its step — a
    // **zero** step is a redraw and nothing else, so this draws the
    // caption without moving the parade.
    scene.advance(0.0);
    if let Some(band) = scene.message_blit() {
        encoder.set_message(&band);
    }
    let step = 1.0 / fps as f32;
    for _ in 0..frames {
        scene.advance(step);
        let offsets = scene.backdrop_offsets();
        encoder.push_frame(
            scene.grid(),
            scene.sky_ramp(),
            scene.sub_cell_offset_px(),
            &offsets,
        )?;
    }
    let blob = encoder.finish(&mut font)?;
    Ok(Recording {
        cols,
        rows,
        cell_w,
        cell_h,
        fps,
        frame_count: frames,
        blob,
    })
}

/// Render the parade's frames as pixel frames, for the GIF.
///
/// The frames are rendered at the app's **own** cell pitch and then
/// downscaled by `scale` with a box filter. Half the cell pitch (one
/// and a half pixels per cell) keeps the app's 16:9 shape and its
/// glyphs; a one-pixel cell would squash the ducks and turn every
/// glyph into a dot.
///
/// `assets` is where the drawn art and the MIDI tracks live: the export
/// renders the **user's** parade, so the shell hands the folders over.
pub fn render_pixels(
    cfg: &Config,
    frames: u32,
    fps: u32,
    scale: u32,
    assets: &AssetDirs,
) -> Result<Vec<crate::export::gif::GifFrame>, ExportError> {
    if frames == 0 {
        return Err(ExportError::Config(
            "a recording needs at least one frame".into(),
        ));
    }
    if frames > MAX_FRAMES {
        return Err(ExportError::Config(format!(
            "{frames} frames is over the {MAX_FRAMES} frame limit"
        )));
    }
    let fps = fps.max(1);
    let scale = scale.max(1);
    // The GIF carries the same export hint as the page (audit F-154).
    let cfg = export_config(cfg);
    let (cols, rows, cell_w, cell_h) =
        cfg.grid_for_monitor(RECORD_MONITOR_W, RECORD_MONITOR_H, 1.0);
    let mut font = Font::new(cell_w, cell_h).map_err(|e| ExportError::Font(e.to_string()))?;
    let mut scene = Scene::new_with_assets(&cfg, cols, rows, cell_w, cell_h, assets);
    install_backdrops(&mut scene, &cfg);
    let width = cols as u32 * cell_w;
    let height = rows as u32 * cell_h;
    let mut fb = crate::blit::FrameBuffer::new(width, height);
    let step = 1.0 / fps as f32;
    let mut out = Vec::with_capacity(frames as usize);
    for _ in 0..frames {
        scene.advance(step);
        crate::selftest::blit_scene_into(&scene, &mut font, &mut fb);
        let (w, h, rgba) = downscale(&fb, scale);
        out.push(crate::export::gif::GifFrame {
            width: w,
            height: h,
            rgba,
        });
    }
    Ok(out)
}

/// Downscale a framebuffer by an integer factor, with a box filter.
///
/// The average is over the RGB channels; the alpha stays opaque.
///
/// WHY the helpers: the nesting rule is two levels at most, and the box
/// filter was four levels deep — one helper per level (audit 2026-10-21
/// §F8).
fn downscale(fb: &crate::blit::FrameBuffer, factor: u32) -> (u32, u32, Vec<u8>) {
    let factor = factor.max(1) as usize;
    if factor == 1 {
        return (fb.width, fb.height, fb.data.clone());
    }
    let width = (fb.width as usize / factor).max(1);
    let height = (fb.height as usize / factor).max(1);
    let mut out = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            write_box_filter_cell(fb, factor, x, y, width, &mut out);
        }
    }
    (width as u32, height as u32, out)
}

/// The box filter of one output cell: the average of the `factor x
/// factor` source pixels it covers.
fn write_box_filter_cell(
    fb: &crate::blit::FrameBuffer,
    factor: usize,
    x: usize,
    y: usize,
    width: usize,
    out: &mut [u8],
) {
    let mut sums = [0u32; 3];
    let mut count = 0u32;
    for dy in 0..factor {
        let sy = y * factor + dy;
        if sy >= fb.height as usize {
            break;
        }
        sum_source_row(fb, factor, x, sy, &mut sums, &mut count);
    }
    let at = (y * width + x) * 4;
    for channel in 0..3 {
        out[at + channel] = (sums[channel] / count.max(1)) as u8;
    }
    out[at + 3] = 255;
}

/// Add one source row of the box filter to the running sums.
fn sum_source_row(
    fb: &crate::blit::FrameBuffer,
    factor: usize,
    x: usize,
    sy: usize,
    sums: &mut [u32; 3],
    count: &mut u32,
) {
    for dx in 0..factor {
        let sx = x * factor + dx;
        if sx >= fb.width as usize {
            break;
        }
        let at = (sy * fb.width as usize + sx) * 4;
        sums[0] += fb.data[at] as u32;
        sums[1] += fb.data[at + 1] as u32;
        sums[2] += fb.data[at + 2] as u32;
        *count += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::export::codec::decode;
    use crate::grid::Grid;

    /// A short recording of the built-in look.
    fn short_recording() -> Recording {
        record(&Config::default(), 3, DEFAULT_FPS, &AssetDirs::default()).expect("recording")
    }

    /// The inked cells of one decoded band, **relative to its top-left
    /// corner**, in a sorted canonical order.
    ///
    /// WHY a shape and not letters: the block font stamps `'X'` cells,
    /// so a band reads back as blocks — the text is compared by the
    /// pattern it stamps, rendered through [`crate::hud_font::draw_text`]
    /// into a scratch grid.
    fn band_shape(
        blob: &crate::export::codec::DecodedBlob,
        frame: &crate::export::codec::DecodedFrame,
        top: usize,
    ) -> Vec<(usize, usize)> {
        let mut cells: Vec<(usize, usize)> = (0..blob.cols)
            .flat_map(move |col| {
                (0..5).filter_map(move |r| {
                    let cell = frame.cells[(top + r) * blob.cols + col];
                    (blob.palette[cell as usize].glyph != ' ').then_some((col, r))
                })
            })
            .collect();
        let left = cells.iter().map(|(col, _)| *col).min().unwrap_or(0);
        cells.sort_unstable();
        cells.iter().map(|(col, r)| (col - left, *r)).collect()
    }

    /// The inked cells of a rendered text, relative to its top-left
    /// corner, in the same canonical order as [`band_shape`].
    fn text_shape(grid: &Grid) -> Vec<(usize, usize)> {
        let mut bounds: Option<(usize, usize, usize, usize)> = None;
        grid.for_each_cell(|col, row, ch, _| {
            if ch == ' ' {
                return;
            }
            bounds = Some(match bounds {
                None => (col, row, col, row),
                Some((left, top, right, bottom)) => {
                    (left.min(col), top.min(row), right.max(col), bottom.max(row))
                }
            });
        });
        let Some((left, top, _right, _bottom)) = bounds else {
            return Vec::new();
        };
        let mut cells: Vec<(usize, usize)> = Vec::new();
        grid.for_each_cell(|col, row, ch, _| {
            if ch != ' ' {
                cells.push((col - left, row - top));
            }
        });
        cells.sort_unstable();
        cells
    }

    /// The text the block font stamps, in a scratch grid of its own.
    fn rendered(text: &str) -> Grid {
        let mut grid = Grid::new(120, 5, 2, 3);
        crate::hud_font::draw_text(&mut grid, 0, 0, text, [1, 2, 3]);
        grid
    }

    #[test]
    fn a_recording_holds_the_gate_geometry() {
        let recording = short_recording();
        assert_eq!(recording.cols, 960);
        assert_eq!(recording.rows, 360);
        assert_eq!(recording.cell_w, 2);
        assert_eq!(recording.cell_h, 3);
        assert_eq!(recording.frame_count, 3);
    }

    #[test]
    fn the_blob_decodes_back_into_the_frames() {
        let recording = short_recording();
        let blob = decode(&recording.blob).expect("the blob decodes");
        assert_eq!(blob.frames.len(), 3);
        assert_eq!(blob.cols, 960);
        assert_eq!(blob.rows, 360);
        assert_eq!(blob.frames[0].cells.len(), 960 * 360);
    }

    #[test]
    fn a_recording_is_reproducible() {
        let a =
            record(&Config::default(), 3, DEFAULT_FPS, &AssetDirs::default()).expect("recording");
        let b =
            record(&Config::default(), 3, DEFAULT_FPS, &AssetDirs::default()).expect("recording");
        assert_eq!(a.blob, b.blob, "the same parade records the same bytes");
    }

    #[test]
    fn the_frames_differ_from_each_other() {
        // The parade moves, so the frames must not be identical: a
        // recorder that wrote the same frame twice would export a still
        // picture.
        let recording = short_recording();
        let blob = decode(&recording.blob).expect("the blob decodes");
        assert_ne!(blob.frames[0].cells, blob.frames[1].cells);
    }

    #[test]
    fn an_empty_recording_is_refused() {
        let err = record(&Config::default(), 0, DEFAULT_FPS, &AssetDirs::default())
            .expect_err("no frames is refused");
        assert!(matches!(err, ExportError::Config(_)), "{err:?}");
    }

    #[test]
    fn an_oversize_recording_is_refused() {
        let err = record(
            &Config::default(),
            MAX_FRAMES + 1,
            DEFAULT_FPS,
            &AssetDirs::default(),
        )
        .expect_err("too many frames is refused");
        assert!(matches!(err, ExportError::Config(_)), "{err:?}");
    }

    #[test]
    fn the_default_exit_hint_does_not_travel() {
        // An exported file has no ESC to press, so the *default* hint is
        // replaced (audit 2026-10-23 F-154); a hint the user wrote stays.
        let cfg = export_config(&Config::default());
        assert_eq!(cfg.exit_hint, EXPORT_HINT, "the default hint is replaced");
        let custom = Config {
            exit_hint: "MY OWN HINT".to_string(),
            ..Config::default()
        };
        assert_eq!(export_config(&custom).exit_hint, "MY OWN HINT");
        let blank = Config {
            exit_hint: "  ".to_string(),
            ..Config::default()
        };
        assert_eq!(export_config(&blank).exit_hint, EXPORT_HINT);
        // And the recording really carries it: the hint band (five rows
        // starting at `rows - 6`) stamps exactly what the export hint
        // stamps.
        let recording = short_recording();
        let blob = decode(&recording.blob).expect("the blob decodes");
        let frame = blob.frames.last().expect("a frame");
        let band = band_shape(&blob, frame, blob.rows - 6);
        assert!(!band.is_empty(), "the hint must be on the grid");
        assert_eq!(
            band,
            text_shape(&rendered(EXPORT_HINT)),
            "the recorded hint is the export one"
        );
    }

    #[test]
    fn the_counter_line_travels_when_asked() {
        // The additive HUD parameter (audit 2026-10-23 F-142): the line
        // the popup draws bottom-left lands in the exported grid too.
        let cfg = Config::default();
        let assets = AssetDirs::default();
        let with_counter =
            record_with_hud(&cfg, 2, DEFAULT_FPS, &assets, Some("DAYS 42")).expect("recording");
        let blob = decode(&with_counter.blob).expect("the blob decodes");
        let frame = blob.frames.last().expect("a frame");
        let band = band_shape(&blob, frame, blob.rows - 12);
        assert!(!band.is_empty(), "the counter row must carry the line");
        assert_eq!(
            band,
            text_shape(&rendered("DAYS 42")),
            "the counter row stamps the requested line"
        );
        // And `None` (the plain `record`) carries none.
        let without = short_recording();
        let blob = decode(&without.blob).expect("the blob decodes");
        let frame = blob.frames.last().expect("a frame");
        let band = band_shape(&blob, frame, blob.rows - 12);
        assert!(band.is_empty(), "no counter without the line");
    }

    #[test]
    fn the_message_band_travels_with_the_recording() {
        // The caption the app and the GIF draw lands in the blob too
        // (audit 2026-10-23 F-140), at the row the scene chose.
        let mut cfg = Config::default();
        cfg.message.text = "HELLO DUCKS".to_string();
        let assets = AssetDirs::default();
        let recording = record(&cfg, 2, DEFAULT_FPS, &assets).expect("recording");
        let blob = decode(&recording.blob).expect("the blob decodes");
        let message = blob.message.expect("the band must travel");
        // The band is the caption's crop, row-major: the inked offsets
        // must be exactly the caption's glyph cells, with the gaps
        // between the glyphs carried as zero (the page paints the block).
        let expected = text_shape(&rendered("HELLO DUCKS"));
        let ink: std::collections::BTreeSet<(usize, usize)> = message
            .cells
            .iter()
            .enumerate()
            .filter(|(_, &cell)| cell != 0)
            .map(|(index, _)| (index % message.cols, index / message.cols))
            .collect();
        let expected_set: std::collections::BTreeSet<(usize, usize)> =
            expected.into_iter().collect();
        assert_eq!(ink, expected_set, "the band is exactly the caption");
        // The band sits where the scene's own blit puts it — the twin
        // scene answers the same query the recorder asked.
        let cfg = export_config(&cfg);
        let (cols, rows, cell_w, cell_h) =
            cfg.grid_for_monitor(RECORD_MONITOR_W, RECORD_MONITOR_H, 1.0);
        let scene = Scene::new_with_assets(&cfg, cols, rows, cell_w, cell_h, &assets);
        let blit = scene.message_blit().expect("the scene draws the band");
        assert_eq!(message.row, blit.row_offset, "the band's row travels");
        assert!(
            message.cols < cols,
            "the band is cropped to the ink, not the whole grid"
        );
    }
}
