//! The backdrop tile: a rectangular ASCII band that scrolls behind the
//! parade.
//!
//! Roadmap Extension 4 §1 (Stage 1, "parallax layers"): a backdrop layer is
//! a *tile* — a band of cells that repeats behind the ducks while it
//! scrolls at its own rate. This module turns a picture into that band.
//!
//! The picture pipeline in [`crate::image_ascii`] is the **balloon's**: it
//! masks the result to a **disc** and despeckles it, because a balloon is
//! round and a lone pixel in the middle of it is noise. A backdrop band is
//! a **rectangle** and its detail is the whole point, so this module does
//! neither — no disc mask, no despeckle. The disc mask belongs to the
//! balloon; a rectangle is what a backdrop band needs. The two conversions
//! are separate on purpose: the balloon's disc must not leak into the
//! backdrop, and the backdrop's rectangle must not leak into the balloon.
//!
//! The **density ramp is not restated here**. The glyph comes from
//! [`crate::image_ascii::glyph_for_luminance`], which owns
//! [`crate::image_ascii::DENSITY_RAMP`], so the balloon and the backdrop
//! cannot drift apart on how a brightness becomes a glyph. A consequence
//! worth knowing: the ramp's darkest glyph is a **space**, so a black
//! region of the picture lands on the sky instead of on the band — the
//! same convention the balloon already uses.
//!
//! Alpha participates through **premultiplied accumulation**, exactly as
//! the balloon pipeline does it: a cell accumulates `rgb * a` weighted by
//! the overlap of the source pixel with the cell's footprint, averages the
//! alpha by that same coverage, and un-premultiplies back to a straight
//! colour. A cell whose average alpha is below 128 is **transparent** (a
//! space), so the sky shows through it.
//!
//! The cell's colour is the un-premultiplied average RGB, so the band keeps
//! the picture's colours: a backdrop reduced to a grey silhouette reads as
//! a hole punched in the scene, a painted one reads as a painting.
//!
//! The conversion is **deterministic** — the same bytes always give the
//! same tile, byte for byte. The exports are reproducible artifacts, and a
//! tile that shifted between two runs of the same picture would make them
//! differ. The accumulation is therefore a fixed row-major reduction with
//! no clock, no randomness and no parallel sum.
//!
//! A source is **capped before the decode** (pixel count and edge length),
//! so a hostile picture cannot balloon the allocator. The release build
//! aborts on panic, so a refusal has to arrive as an error, never as an
//! abort.

use crate::balloon::AsciiSprite;
use crate::config::BackdropConfig;
use crate::image_ascii::{glyph_for_luminance, DENSITY_RAMP};
use std::io::Cursor;
use std::path::Path;

/// Failures while turning a picture into a backdrop tile.
#[derive(Debug, thiserror::Error)]
pub enum BackdropError {
    /// The file could not be read.
    #[error("the backdrop image {path} could not be read: {source}")]
    Io {
        /// The file that could not be read.
        path: String,
        /// The OS error.
        #[source]
        source: std::io::Error,
    },
    /// The bytes did not decode as a supported image.
    #[error("the backdrop image {path} could not be decoded: {source}")]
    Decode {
        /// The file that could not be decoded.
        path: String,
        /// The decoder error.
        #[source]
        source: image::ImageError,
    },
    /// The requested tile size is not usable.
    #[error("a backdrop tile of {cols}x{rows} cells is not usable")]
    BadSize {
        /// The requested width in cells.
        cols: usize,
        /// The requested height in cells.
        rows: usize,
    },
}

/// The largest tile width this build accepts, in cells.
pub const MAX_TILE_COLS: usize = 512;

/// The largest tile height this build accepts, in cells.
pub const MAX_TILE_ROWS: usize = 128;

/// A cell whose coverage-averaged alpha is below this is transparent.
const ALPHA_THRESHOLD: f32 = 128.0;

/// Source images above this pixel count are refused before the decode, so
/// a hostile PNG cannot balloon the allocator (release is `panic = "abort"`).
const MAX_SOURCE_PIXELS: u64 = 4096 * 4096;

/// Longest accepted source edge, in pixels. A backstop for a header that
/// under-reports its size.
const MAX_SOURCE_EDGE: u32 = 16_384;

/// Longest accepted source file, in bytes. Bounds the read **before** it
/// happens: with `panic = "abort"` an over-large allocation is instant
/// process death, not a catchable error.
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// The source name the byte-level entry point reports in its errors: the
/// bytes have no path, but the error still has to name something.
const BYTES_LABEL: &str = "<bytes>";

/// Convert an image file into a rectangular ASCII tile of `cols x rows` cells.
pub fn convert_image_file(
    path: &std::path::Path,
    cols: usize,
    rows: usize,
) -> Result<AsciiSprite, BackdropError> {
    let bytes = read_source_file(path)?;
    convert_source(&bytes, cols, rows, &path.display().to_string())
}

/// Convert image bytes into a rectangular ASCII tile of `cols x rows` cells.
pub fn convert_image_bytes(
    bytes: &[u8],
    cols: usize,
    rows: usize,
) -> Result<AsciiSprite, BackdropError> {
    convert_source(bytes, cols, rows, BYTES_LABEL)
}

/// Convert every configured picture into a tile.
///
/// A picture that is missing or broken is skipped: the layers that did
/// load still show, and the parade is exactly what it was (roadmap
/// Extension 4 §1.3). The order is the configuration's order, so the
/// farthest layer stays the farthest.
///
/// Only an **absolute** path is read. The *name* of a picture is a
/// platform question — which folder is it in? — so the shell resolves it
/// into an absolute path before the core sees the configuration. A
/// relative name is skipped with a warning.
pub fn convert_layers(layers: &[BackdropConfig]) -> Vec<AsciiSprite> {
    layers.iter().filter_map(convert_one).collect()
}

/// Convert one picture into a tile, or `None` when it cannot be read.
fn convert_one(layer: &BackdropConfig) -> Option<AsciiSprite> {
    let path = absolute_path(&layer.image)?;
    match convert_image_file(&path, layer.cols, layer.rows) {
        Ok(tile) => Some(tile),
        Err(e) => {
            crate::trace::warn(&format!("backdrop: {}: {e}", path.display()));
            None
        }
    }
}

/// The absolute path of a configured picture, or `None` when the
/// configuration does not name one the core can read.
fn absolute_path(image: &str) -> Option<std::path::PathBuf> {
    let trimmed = image.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = std::path::Path::new(trimmed);
    if !path.is_absolute() {
        crate::trace::warn(&format!(
            "backdrop: {trimmed} is not an absolute path, so the layer is skipped"
        ));
        return None;
    }
    Some(path.to_path_buf())
}

/// The shared conversion: check the tile size, decode the picture, rasterize
/// the band. `label` is the name the errors report.
fn convert_source(
    bytes: &[u8],
    cols: usize,
    rows: usize,
    label: &str,
) -> Result<AsciiSprite, BackdropError> {
    check_tile_size(cols, rows)?;
    let picture = decode_rgba(bytes, label)?;
    rasterize(&picture, cols, rows, label)
}

/// Refuse a tile size this build will not produce. The bounds are on the
/// *output* grid: the band is blitted into a screen-sized grid, so a tile
/// wider than the widest screen can never be shown.
fn check_tile_size(cols: usize, rows: usize) -> Result<(), BackdropError> {
    let usable = cols > 0 && rows > 0 && cols <= MAX_TILE_COLS && rows <= MAX_TILE_ROWS;
    if !usable {
        return Err(BackdropError::BadSize { cols, rows });
    }
    Ok(())
}

/// Read a source image, refusing a file that is too big **before** the read.
fn read_source_file(path: &Path) -> Result<Vec<u8>, BackdropError> {
    let name = path.display().to_string();
    let size = std::fs::metadata(path)
        .map_err(|source| BackdropError::Io {
            path: name.clone(),
            source,
        })?
        .len();
    if size > MAX_SOURCE_BYTES {
        return Err(BackdropError::Io {
            path: name,
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("{size} bytes exceeds the {MAX_SOURCE_BYTES} byte limit"),
            ),
        });
    }
    std::fs::read(path).map_err(|source| BackdropError::Io { path: name, source })
}

/// Decode the source into straight RGBA.
///
/// The dimension probe runs **before** the decode, so an oversize picture
/// never reaches the allocator. The probe only reads the header: if it
/// cannot name the dimensions, the decode below reports the real error.
fn decode_rgba(bytes: &[u8], label: &str) -> Result<image::RgbaImage, BackdropError> {
    if let Some((w, h)) = probe_dimensions(bytes) {
        check_source_size(w, h, label)?;
    }
    let picture = image::load_from_memory(bytes).map_err(|source| BackdropError::Decode {
        path: label.to_string(),
        source,
    })?;
    Ok(picture.into_rgba8())
}

/// The picture's dimensions from its header, or `None` when the header
/// cannot be read or the format does not lead with its size.
fn probe_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    reader.into_dimensions().ok()
}

/// The size policy for a source picture: at most [`MAX_SOURCE_PIXELS`]
/// pixels and at most [`MAX_SOURCE_EDGE`] pixels per edge.
fn check_source_size(w: u32, h: u32, label: &str) -> Result<(), BackdropError> {
    let pixels = u64::from(w) * u64::from(h);
    let fits =
        pixels > 0 && pixels <= MAX_SOURCE_PIXELS && w <= MAX_SOURCE_EDGE && h <= MAX_SOURCE_EDGE;
    if fits {
        return Ok(());
    }
    Err(BackdropError::Decode {
        path: label.to_string(),
        source: image::ImageError::Limits(image::error::LimitError::from_kind(
            image::error::LimitErrorKind::DimensionError,
        )),
    })
}

/// The source rectangle covered by one target cell, in source pixels.
struct Footprint {
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
    sx0: usize,
    sx1: usize,
    sy0: usize,
    sy1: usize,
}

/// Running premultiplied sums for one target cell.
#[derive(Default)]
struct Sums {
    r: f64,
    g: f64,
    b: f64,
    a: f64,
    w: f64,
}

/// Sample every target cell of the band into a character/colour buffer.
///
/// The buffers are reserved fallibly: a refused allocation is an error,
/// not an abort (release is `panic = "abort"`).
fn rasterize(
    picture: &image::RgbaImage,
    cols: usize,
    rows: usize,
    label: &str,
) -> Result<AsciiSprite, BackdropError> {
    let cells = cols * rows;
    let mut chars = reserve::<char>(cells, label)?;
    let mut colors = reserve::<[u8; 3]>(cells, label)?;
    for (tx, ty) in (0..rows).flat_map(|ty| (0..cols).map(move |tx| (tx, ty))) {
        let (ch, color) = sample_cell(picture, cols, rows, tx, ty);
        chars.push(ch);
        colors.push(color);
    }
    AsciiSprite::from_cells(cols, rows, chars, colors).ok_or(BackdropError::BadSize { cols, rows })
}

/// Sample one target cell: box-average the source footprint, drop the
/// transparent ones, else map the luminance through the density ramp.
///
/// A backdrop band is a **rectangle**: the ramp's space-at-black (right
/// for a balloon, whose empty cells must be transparent) would punch
/// sky-coloured holes in a dark picture's darkest regions, leaving a
/// ragged band. The floor maps "almost black" to the ramp's first
/// visible glyph, so the band stays whole
/// (audit 2026-10-22 §VIS-02).
fn sample_cell(
    picture: &image::RgbaImage,
    cols: usize,
    rows: usize,
    tx: usize,
    ty: usize,
) -> (char, [u8; 3]) {
    /// Below this luminance a backdrop cell reads as the ramp's first
    /// visible glyph instead of a hole.
    const BLACK_FLOOR: f32 = 0.06;
    let (r, g, b, a) = box_average(picture, cols, rows, tx, ty);
    if a < ALPHA_THRESHOLD {
        return (' ', [0, 0, 0]);
    }
    let lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
    let glyph = if lum < BLACK_FLOOR {
        DENSITY_RAMP
            .chars()
            .nth(1)
            .unwrap_or_else(|| glyph_for_luminance(lum))
    } else {
        glyph_for_luminance(lum)
    };
    (glyph, [channel(r), channel(g), channel(b)])
}

/// Box-average one target cell over its source footprint with premultiplied
/// alpha. All index math is `usize`/`u64`: a `u32` multiply could wrap on
/// a large source.
///
/// Twin pipeline note (decision F-167, roadmap HELP window §4.3):
/// `image_ascii.rs` carries the **disc** mask's box filter — same
/// premultiplied accumulation, plus the balloon pipeline's disc mask and
/// despeckle, both wrong for a backdrop. The two stay separate until one
/// of them next changes for a real reason (the piggyback rule).
fn box_average(
    picture: &image::RgbaImage,
    cols: usize,
    rows: usize,
    tx: usize,
    ty: usize,
) -> (f32, f32, f32, f32) {
    let raw = picture.as_raw();
    let fp = footprint(picture.width(), picture.height(), cols, rows, tx, ty);
    let mut sums = Sums::default();
    for sy in fp.sy0..fp.sy1 {
        accumulate_scanline(raw, picture.width(), sy, &fp, &mut sums);
    }
    if sums.w <= 0.0 || sums.a <= 0.0 {
        return (0.0, 0.0, 0.0, 0.0);
    }
    let avg_a = (sums.a / sums.w * 255.0) as f32;
    (
        (sums.r / sums.a) as f32,
        (sums.g / sums.a) as f32,
        (sums.b / sums.a) as f32,
        avg_a,
    )
}

/// The source rectangle one target cell covers, in fractional source
/// pixels and in the whole pixels that overlap it.
fn footprint(sw: u32, sh: u32, cols: usize, rows: usize, tx: usize, ty: usize) -> Footprint {
    let (sw_f, sh_f) = (f64::from(sw), f64::from(sh));
    let x0 = tx as f64 * sw_f / cols as f64;
    let x1 = (tx + 1) as f64 * sw_f / cols as f64;
    let y0 = ty as f64 * sh_f / rows as f64;
    let y1 = (ty + 1) as f64 * sh_f / rows as f64;
    let (sw_i, sh_i) = (i64::from(sw), i64::from(sh));
    Footprint {
        x0,
        x1,
        y0,
        y1,
        sx0: (x0.floor() as i64).clamp(0, sw_i) as usize,
        sx1: (x1.ceil() as i64).clamp(0, sw_i) as usize,
        sy0: (y0.floor() as i64).clamp(0, sh_i) as usize,
        sy1: (y1.ceil() as i64).clamp(0, sh_i) as usize,
    }
}

/// Accumulate one source scanline's overlap with the target footprint into
/// the running premultiplied sums.
fn accumulate_scanline(raw: &[u8], sw: u32, sy: usize, fp: &Footprint, sums: &mut Sums) {
    let row_base = sy as u64 * u64::from(sw);
    for sx in fp.sx0..fp.sx1 {
        let ox = (sx as f64 + 1.0).min(fp.x1) - (sx as f64).max(fp.x0);
        let oy = (sy as f64 + 1.0).min(fp.y1) - (sy as f64).max(fp.y0);
        accumulate_pixel(raw, row_base, sx, ox.max(0.0) * oy.max(0.0), sums);
    }
}

/// Fold one overlapped source pixel into the running sums; zero overlap is
/// a no-op. The byte index is bounds-checked: a short buffer yields no
/// contribution instead of a panic (release is `panic = "abort"`).
fn accumulate_pixel(raw: &[u8], row_base: u64, sx: usize, w: f64, sums: &mut Sums) {
    if w <= 0.0 {
        return;
    }
    let Ok(i) = usize::try_from((row_base + sx as u64) * 4) else {
        return;
    };
    let bytes = [raw.get(i), raw.get(i + 1), raw.get(i + 2), raw.get(i + 3)];
    let [r, g, b, a] = match bytes {
        [Some(&r), Some(&g), Some(&b), Some(&a)] => [r, g, b, a],
        _ => return,
    };
    let alpha = f64::from(a) / 255.0;
    sums.r += f64::from(r) * alpha * w;
    sums.g += f64::from(g) * alpha * w;
    sums.b += f64::from(b) * alpha * w;
    sums.a += alpha * w;
    sums.w += w;
}

/// One colour channel of a cell, rounded and clamped into a byte.
fn channel(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

/// Reserve `count` elements fallibly and report a clean error when the
/// allocator refuses. Release builds abort on panic, so an allocation
/// failure has to be an `Err`, never a fatal abort.
fn reserve<T>(count: usize, label: &str) -> Result<Vec<T>, BackdropError> {
    let mut buf: Vec<T> = Vec::new();
    buf.try_reserve_exact(count)
        .map_err(|_| BackdropError::Io {
            path: label.to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::OutOfMemory,
                format!("cannot allocate {count} backdrop cells"),
            ),
        })?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::image_ascii::DENSITY_RAMP;
    use std::io::Cursor;

    /// A PNG of one solid colour, built in memory.
    fn png_bytes(pixel: image::Rgba<u8>, w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_fn(w, h, |_x, _y| pixel);
        encode_png(&img)
    }

    /// Encode an image as PNG bytes; the format is chosen by the bytes,
    /// not by a file name.
    fn encode_png(img: &image::RgbaImage) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img.clone())
            .write_to(&mut out, image::ImageFormat::Png)
            .expect("encode png");
        out.into_inner()
    }

    /// Every cell of a `cols x rows` grid in row-major order, as `(x, y)`.
    /// One flat iterator instead of a nested loop (audit §Nesting).
    fn cells_of(cols: usize, rows: usize) -> impl Iterator<Item = (usize, usize)> {
        (0..rows).flat_map(move |y| (0..cols).map(move |x| (x, y)))
    }

    #[test]
    fn a_flat_image_becomes_a_uniform_tile() {
        let bytes = png_bytes(image::Rgba([255, 0, 0, 255]), 8, 8);
        let tile = convert_image_bytes(&bytes, 4, 4).expect("convert");
        for (x, y) in cells_of(4, 4) {
            let (ch, color) = tile.get(x, y).expect("cell");
            assert_ne!(ch, ' ', "cell {x},{y} is blank");
            assert!(
                color[0] >= 254 && color[1] <= 1 && color[2] <= 1,
                "cell {x},{y} is not red: {color:?}"
            );
        }
    }

    #[test]
    fn a_transparent_image_becomes_an_empty_tile() {
        let bytes = png_bytes(image::Rgba([0, 0, 0, 0]), 8, 8);
        let tile = convert_image_bytes(&bytes, 4, 4).expect("convert");
        for (x, y) in cells_of(4, 4) {
            let (ch, _color) = tile.get(x, y).expect("cell");
            assert_eq!(ch, ' ', "cell {x},{y} is not transparent");
        }
    }

    #[test]
    fn the_tile_has_the_requested_size() {
        let bytes = png_bytes(image::Rgba([0, 255, 0, 255]), 16, 16);
        let tile = convert_image_bytes(&bytes, 7, 3).expect("convert");
        assert_eq!((tile.cols(), tile.rows()), (7, 3));
    }

    #[test]
    fn a_bright_image_uses_a_dense_glyph() {
        let bytes = png_bytes(image::Rgba([255, 255, 255, 255]), 8, 8);
        let tile = convert_image_bytes(&bytes, 4, 4).expect("convert");
        let dense = DENSITY_RAMP.chars().last().expect("ramp is not empty");
        for (x, y) in cells_of(4, 4) {
            let (ch, _color) = tile.get(x, y).expect("cell");
            assert_eq!(ch, dense, "cell {x},{y} is not the dense glyph");
            assert_ne!(ch, ' ', "cell {x},{y} is blank");
        }
    }

    #[test]
    fn an_oversize_request_is_refused() {
        let bytes = png_bytes(image::Rgba([255, 0, 0, 255]), 4, 4);
        let zero = convert_image_bytes(&bytes, 0, 4).expect_err("no columns");
        assert!(matches!(zero, BackdropError::BadSize { cols: 0, rows: 4 }));
        let zero = convert_image_bytes(&bytes, 4, 0).expect_err("no rows");
        assert!(matches!(zero, BackdropError::BadSize { cols: 4, rows: 0 }));
        let wide = convert_image_bytes(&bytes, MAX_TILE_COLS + 1, 4).expect_err("too wide");
        assert!(matches!(
            wide,
            BackdropError::BadSize {
                cols,
                rows: 4
            } if cols == MAX_TILE_COLS + 1
        ));
        let tall = convert_image_bytes(&bytes, 4, MAX_TILE_ROWS + 1).expect_err("too tall");
        assert!(matches!(
            tall,
            BackdropError::BadSize {
                cols: 4,
                rows
            } if rows == MAX_TILE_ROWS + 1
        ));
    }

    #[test]
    fn the_same_bytes_give_the_same_tile() {
        let bytes = png_bytes(image::Rgba([12, 200, 90, 255]), 20, 12);
        let first = convert_image_bytes(&bytes, 9, 5).expect("convert");
        let second = convert_image_bytes(&bytes, 9, 5).expect("convert");
        assert_eq!(first, second);
    }

    #[test]
    fn garbage_bytes_are_refused() {
        let err = convert_image_bytes(b"not an image at all", 4, 4).expect_err("garbage");
        assert!(matches!(err, BackdropError::Decode { .. }));
        let empty: Vec<u8> = Vec::new();
        let err = convert_image_bytes(&empty, 4, 4).expect_err("empty");
        assert!(matches!(err, BackdropError::Decode { .. }));
    }

    #[test]
    fn a_file_converts_like_its_bytes() {
        let bytes = png_bytes(image::Rgba([255, 255, 0, 255]), 8, 8);
        let dir = std::env::temp_dir().join(format!(
            "agnosis-backdrop-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let path = dir.join("backdrop.png");
        std::fs::write(&path, &bytes).expect("write");
        let from_file = convert_image_file(&path, 4, 4).expect("convert file");
        let from_bytes = convert_image_bytes(&bytes, 4, 4).expect("convert bytes");
        assert_eq!(from_file, from_bytes);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_missing_file_is_an_io_error() {
        let err = convert_image_file(std::path::Path::new("no-such-backdrop-image.png"), 4, 4)
            .expect_err("missing file");
        assert!(matches!(err, BackdropError::Io { .. }));
    }
}
