//! Image to ASCII pipeline for balloon discs.
//!
//! Decodes PNG, JPEG, GIF and BMP bytes with the `image` crate, downscales
//! with a hand-rolled box filter to `(2 * rx + 1) x (2 * ry + 1)` cells, maps
//! Rec. 601 luminance through [`DENSITY_RAMP`], masks to the balloon circle
//! and despeckles.
//!
//! Alpha participates through premultiplied accumulation: each target cell
//! accumulates source `rgb * a` weighted by footprint overlap, averages alpha
//! by coverage, then un-premultiplies back to straight colour. Cells whose
//! average alpha falls below 128 become transparent.
//!
//! The circular mask reuses [`crate::balloon::circle_cells`] as the single
//! definition of the disc so conversion and rendering agree exactly. Corners
//! are cropped, so subjects must be centred.
//!
//! Hand-made `.ans` art is used verbatim: trailing blank lines are dropped,
//! blank borders are trimmed, the art is centred on the disc canvas, re-masked
//! to the circle and repainted in one fixed colour. No luminance mapping runs.
//! Oversize art errors with a message rather than clipping silently.
//!
//! Converted sprites persist as JSON `{cols, rows, chars, colors}` so the
//! running app loads them directly without re-converting.
//!
//! Cached sprites carry an optional radii stamp: `save_sprite_stamped`
//! records the `(rx, ry)` the art was built for, and `stamped_radii` reads it
//! back. `None` (older caches, hand-written JSON) means "unstamped" —
//! callers treat that as a miss and reconvert. The stamp is advisory only; a
//! stamped cache loads verbatim and only the stamp decides reuse (audit P0-3:
//! stamp, not gate).
//!
//! Robustness (audit 2026-10-17 §Robustness): every buffer this module
//! allocates is reserved fallibly, so an impossible allocation is reported as
//! a normal error instead of aborting the process — release builds use
//! `panic = "abort"`. Source images are size-capped *before* the decode, and
//! the copy of hand-made art is bounded and fallible too.

use crate::balloon::{circle_cells, AsciiSprite};
use std::collections::HashSet;
use std::io::Cursor;
use std::path::Path;

/// Density ramp, darkest to lightest, exactly as the plan specifies it.
pub const DENSITY_RAMP: &str =
    " .'\\`^\",:;Il!i~+_-?][}{1)(|\\/tfjrxnuvczXYUJCLQ0OZmwqpdbkhao*#MW&8%B@$";

/// Fixed palette colour for hand-made `.ans` balloons: warm amber. Single
/// owner for the CLI, watcher, and popup convert call sites (audit §P0-3).
pub const ANS_COLOR: [u8; 3] = [255, 220, 150];

const ALPHA_THRESHOLD: f32 = 128.0;
const MAX_RADIUS: i64 = 1024;
/// Scene-reasonable radii ceiling: `balloon_radii_cells` derives from a row
/// budget of tens of rows, so anything above this is caller error, reported
/// early rather than as a late allocation failure (audit robustness).
const REASONABLE_RADIUS: i64 = 256;
/// Source images above this pixel count are rejected before decode so a
/// hostile PNG cannot balloon the UI-thread allocator (audit robustness).
const MAX_SOURCE_PIXELS: u64 = 4096 * 4096;
/// Longest accepted source edge, in pixels. Backstop for a header that
/// under-reports its size: the decoder refuses before it allocates a frame.
const MAX_SOURCE_EDGE: u32 = 16_384;
/// Longest accepted `.ans` art, in bytes. Bounds the copy of the art and the
/// sprite built from it; a disc is at most 513x513 cells, so a mebibyte of
/// art is already far more than any disc can show.
const MAX_ANS_BYTES: usize = 1 << 20;
/// Longest accepted source file, in bytes. Bounds the read **before** it
/// happens: the pixel cap is applied to the decoded frame, so a huge file
/// was read whole into memory first (audit 2026-10-20 §3.13). With
/// `panic = "abort"` in the release profile an over-large allocation is
/// instant process death, not a catchable error — so the file is refused
/// by its size, up front.
const MAX_SOURCE_BYTES: u64 = 64 * 1024 * 1024;

/// Conversion failures for the image-to-ASCII pipeline.
#[derive(Debug, thiserror::Error)]
pub enum ImageAsciiError {
    /// The bytes on disk did not decode as a supported image.
    #[error("image decode failed: {0}")]
    Decode(#[from] image::ImageError),
    /// A file read or write failed.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// Radii were non-positive or absurdly large.
    #[error("bad balloon size: {0}")]
    BadSize(String),
    /// Hand-made art or a sprite cache was empty or malformed.
    #[error("ascii art error: {0}")]
    Ans(String),
    /// A buffer could not be reserved. Release builds abort on panic, so an
    /// allocation failure has to be reported instead of being fatal
    /// (audit 2026-10-17 §Robustness).
    #[error("cannot allocate {what} ({bytes} bytes): {reason}")]
    Alloc {
        what: &'static str,
        bytes: u64,
        reason: String,
    },
}

/// Decode bytes (PNG/JPEG/GIF/BMP), convert, circular-mask, despeckle; ready for a ring-buffer slot.
///
/// Animated GIF input yields its first frame only. Every cell whose
/// coverage-averaged alpha is below 128 becomes transparent.
pub fn convert_image_bytes(bytes: &[u8], rx: i64, ry: i64) -> Result<AsciiSprite, ImageAsciiError> {
    let (cols, rows) = target_size(rx, ry)?;
    let (sw, sh) = check_source_dimensions(bytes)?;
    let raw = decode_rgba(bytes, sw, sh)?;
    let inside = inside_set(rx, ry);
    let view = SourceView {
        raw: &raw,
        sw,
        sh,
        cols,
        rows,
        rx,
        ry,
        inside: &inside,
    };
    let (chars, colors) = sample_cells(&view)?;
    let mut sprite = AsciiSprite::from_cells(cols, rows, chars, colors)
        .ok_or_else(|| ImageAsciiError::BadSize(format!("grid {cols}x{rows}")))?;
    sprite.despeckle();
    Ok(sprite)
}

/// Same, from a file path (extension decides nothing — content sniffed by `image`).
pub fn convert_image_file(path: &Path, rx: i64, ry: i64) -> Result<AsciiSprite, ImageAsciiError> {
    let bytes = read_source_file(path)?;
    convert_image_bytes(&bytes, rx, ry)
}

/// Read a source image, refusing a file that is too big **before** the
/// read.
///
/// The size is checked from the file metadata, so an over-large file is
/// never loaded into memory (audit 2026-10-20 §3.13).
fn read_source_file(path: &Path) -> Result<Vec<u8>, ImageAsciiError> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_SOURCE_BYTES {
        return Err(ImageAsciiError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is {size} bytes, which exceeds the {MAX_SOURCE_BYTES} byte limit",
                path.display()
            ),
        )));
    }
    Ok(std::fs::read(path)?)
}

/// Hand-made ASCII art file used verbatim: trim blank borders, re-mask to the circle, fixed palette colour.
///
/// Trailing blank lines are dropped, the art is trimmed and centred on a
/// `(2 * rx + 1) x (2 * ry + 1)` canvas, cells outside the circle become
/// transparent, and every inked cell is repainted with `color`.
pub fn convert_ans_text(
    text: &str,
    rx: i64,
    ry: i64,
    color: [u8; 3],
) -> Result<AsciiSprite, ImageAsciiError> {
    let (cols, rows) = target_size(rx, ry)?;
    check_ans_size(text)?;
    let trimmed = raw_sprite(text)?;
    check_ans_fits(&trimmed, cols, rows, rx, ry)?;
    let mut canvas = center_on_canvas(&trimmed, cols, rows, color);
    apply_circle_mask(&mut canvas, rx, ry);
    Ok(canvas)
}

/// Reject art that is too large to copy into a sprite. The bound is in bytes
/// because every cell needs at least one byte, so it caps the cell count too
/// (audit 2026-10-17 §Robustness: the copy of the art must be fallible).
fn check_ans_size(text: &str) -> Result<(), ImageAsciiError> {
    if text.len() > MAX_ANS_BYTES {
        return Err(ImageAsciiError::Ans(format!(
            "ascii art {} bytes exceeds {MAX_ANS_BYTES}",
            text.len()
        )));
    }
    Ok(())
}

/// Parse `.ans` text verbatim: drop trailing blank lines, trim blank borders.
/// Errors when nothing inked remains. Flat helper for `convert_ans_text`.
fn raw_sprite(text: &str) -> Result<AsciiSprite, ImageAsciiError> {
    let mut lines = reserve_vec::<Vec<char>>(text.lines().count(), "ascii art lines")?;
    for line in text.lines() {
        lines.push(art_line(line)?);
    }
    while lines.last().is_some_and(|l| l.iter().all(|c| *c == ' ')) {
        lines.pop();
    }
    let empty = || ImageAsciiError::Ans("empty ascii art".to_string());
    let w = lines.iter().map(|l| l.len()).max().unwrap_or(0);
    if w == 0 {
        return Err(empty());
    }
    let cells = w.saturating_mul(lines.len());
    let mut chars = reserve_vec::<char>(cells, "ascii art chars")?;
    chars.resize(cells, ' ');
    let mut colors = reserve_vec::<[u8; 3]>(cells, "ascii art colors")?;
    colors.resize(cells, [255, 255, 255]);
    for (y, line) in lines.iter().enumerate() {
        copy_ink_line(&mut chars, &mut colors, w, y, line);
    }
    let raw = AsciiSprite::from_cells(w, lines.len(), chars, colors).ok_or_else(empty)?;
    raw.trimmed().ok_or_else(empty)
}

/// Copy one art line into a fallibly reserved character row. Split from
/// `raw_sprite` so no loop nests a loop.
fn art_line(line: &str) -> Result<Vec<char>, ImageAsciiError> {
    let mut chars = reserve_vec::<char>(line.chars().count(), "ascii art row")?;
    chars.extend(line.chars());
    Ok(chars)
}

/// Copy one text line's inked cells into the raw sprite's buffers. Split from
/// `raw_sprite` so no loop nests a loop.
fn copy_ink_line(chars: &mut [char], colors: &mut [[u8; 3]], w: usize, y: usize, line: &[char]) {
    for (x, ch) in line.iter().enumerate().take(w) {
        if *ch == ' ' {
            continue;
        }
        let i = y * w + x;
        chars[i] = *ch;
        colors[i] = [255, 255, 255];
    }
}

/// Same, from a `.ans` file path.
pub fn convert_ans_file(
    path: &Path,
    rx: i64,
    ry: i64,
    color: [u8; 3],
) -> Result<AsciiSprite, ImageAsciiError> {
    let text = read_source_text(path)?;
    convert_ans_text(&text, rx, ry, color)
}

/// Read a hand-made art file, refusing a file that is too big **before**
/// the read (audit 2026-10-20 §3.13).
fn read_source_text(path: &Path) -> Result<String, ImageAsciiError> {
    let size = std::fs::metadata(path)?.len();
    if size > MAX_ANS_BYTES as u64 {
        return Err(ImageAsciiError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "{} is {size} bytes, which exceeds the {MAX_ANS_BYTES} byte limit",
                path.display()
            ),
        )));
    }
    Ok(std::fs::read_to_string(path)?)
}

/// Reject oversize `.ans` art with a message instead of casting negative
/// centering offsets to `usize` (silent mis-clip before; audit robustness).
///
/// The check also makes the centering offsets provably non-negative: the art
/// is at most as wide as the disc, so `cols / 2 - art.cols() / 2 >= 0` and
/// likewise for the rows (see [`center_on_canvas`]).
fn check_ans_fits(
    trimmed: &AsciiSprite,
    cols: usize,
    rows: usize,
    rx: i64,
    ry: i64,
) -> Result<(), ImageAsciiError> {
    let fits = trimmed.cols() <= cols && trimmed.rows() <= rows;
    match fits {
        true => Ok(()),
        false => Err(ImageAsciiError::Ans(format!(
            "art {}x{} exceeds disc {}x{} (rx={rx} ry={ry})",
            trimmed.cols(),
            trimmed.rows(),
            cols,
            rows
        ))),
    }
}

/// Centre the trimmed art on a disc-sized canvas, repainting every inked cell
/// with the single palette colour. Offsets are non-negative: the fit check ran.
fn center_on_canvas(
    trimmed: &AsciiSprite,
    cols: usize,
    rows: usize,
    color: [u8; 3],
) -> AsciiSprite {
    let ox = cols / 2 - trimmed.cols() / 2;
    let oy = rows / 2 - trimmed.rows() / 2;
    let mut canvas = AsciiSprite::new(cols, rows);
    for y in 0..trimmed.rows() {
        center_ink_row(&mut canvas, trimmed, y, ox, oy, color);
    }
    canvas
}

/// Centre one trimmed row's inked cells onto the canvas. Split from
/// `center_on_canvas` so no loop nests a loop.
fn center_ink_row(
    canvas: &mut AsciiSprite,
    trimmed: &AsciiSprite,
    y: usize,
    ox: usize,
    oy: usize,
    color: [u8; 3],
) {
    for x in 0..trimmed.cols() {
        let Some((ch, _)) = trimmed.get(x, y) else {
            continue;
        };
        if ch != ' ' {
            canvas.set(ox + x, oy + y, ch, color);
        }
    }
}

/// Clear every canvas cell outside the disc. Flat helper for `convert_ans_text`.
fn apply_circle_mask(canvas: &mut AsciiSprite, rx: i64, ry: i64) {
    let inside = inside_set(rx, ry);
    let (cols, rows) = (canvas.cols(), canvas.rows());
    for y in 0..rows {
        mask_circle_row(canvas, y, cols, rx, ry, &inside);
    }
}

/// Clear one canvas row's cells that lie outside the disc. Split from
/// `apply_circle_mask` so no loop nests a loop.
fn mask_circle_row(
    canvas: &mut AsciiSprite,
    y: usize,
    cols: usize,
    rx: i64,
    ry: i64,
    inside: &HashSet<(i64, i64)>,
) {
    for x in 0..cols {
        let outside = !inside.contains(&(x as i64 - rx, y as i64 - ry));
        let inked = canvas.get(x, y).is_some_and(|(ch, _)| ch != ' ');
        if outside && inked {
            canvas.set(x, y, ' ', [0, 0, 0]);
        }
    }
}

/// The disc as a centre-relative cell set: the single definition callers share
/// so conversion and rendering agree exactly.
fn inside_set(rx: i64, ry: i64) -> HashSet<(i64, i64)> {
    circle_cells(rx as f32, ry as f32)
        .into_iter()
        .map(|(dx, dy, _)| (dx, dy))
        .collect()
}

/// Everything one target cell needs to sample the source: the decoded RGBA
/// buffer, the target grid geometry and the disc mask. Bundled into one
/// value so the sampling helpers stay flat (audit §Nesting).
struct SourceView<'a> {
    raw: &'a [u8],
    sw: u32,
    sh: u32,
    cols: usize,
    rows: usize,
    rx: i64,
    ry: i64,
    inside: &'a HashSet<(i64, i64)>,
}

/// The source rectangle covered by one target cell, in source pixels.
struct Footprint {
    x0: f64,
    x1: f64,
    y0: f64,
    y1: f64,
}

/// Running premultiplied sums for one target cell.
///
/// Twin pipeline note (decision F-167, roadmap HELP window §4.3):
/// `backdrop.rs` carries the **rectangle** mask's box filter — same
/// premultiplied accumulation, no disc mask, no despeckle. The two stay
/// separate until one of them next changes for a real reason (the
/// piggyback rule); do not unify them for refactoring's sake.
#[derive(Default)]
struct Sums {
    r: f64,
    g: f64,
    b: f64,
    a: f64,
    w: f64,
}

/// Sample every target cell of the disc canvas into char/color buffers.
/// Output-identical to the old inline loop; extracted for nesting depth.
/// The output buffers are reserved fallibly: a refused allocation is an
/// error, not an abort.
fn sample_cells(view: &SourceView<'_>) -> Result<(Vec<char>, Vec<[u8; 3]>), ImageAsciiError> {
    let cells = view.cols.saturating_mul(view.rows);
    let mut chars = reserve_vec::<char>(cells, "sprite chars")?;
    let mut colors = reserve_vec::<[u8; 3]>(cells, "sprite colors")?;
    for (tx, ty) in (0..view.rows).flat_map(|ty| (0..view.cols).map(move |tx| (tx, ty))) {
        let (ch, color) = sample_cell(view, tx, ty);
        chars.push(ch);
        colors.push(color);
    }
    Ok((chars, colors))
}

/// Sample one target cell: box-average the source footprint, drop transparent
/// or out-of-disc cells, else map luminance through the density ramp.
fn sample_cell(view: &SourceView<'_>, tx: usize, ty: usize) -> (char, [u8; 3]) {
    let (r, g, b, a) = box_average(view, tx, ty);
    let blank = (' ', [0, 0, 0]);
    if a < ALPHA_THRESHOLD {
        return blank;
    }
    if !view
        .inside
        .contains(&(tx as i64 - view.rx, ty as i64 - view.ry))
    {
        return blank;
    }
    let lum = (0.299 * r + 0.587 * g + 0.114 * b) / 255.0;
    let channel = |v: f32| v.round().clamp(0.0, 255.0) as u8;
    (
        glyph_for_luminance(lum),
        [channel(r), channel(g), channel(b)],
    )
}

/// Box-average one target cell over its source footprint with premultiplied
/// alpha. All index math is `usize`/`u64`: the old `u32` multiply could wrap
/// on large sources (audit robustness).
fn box_average(view: &SourceView<'_>, tx: usize, ty: usize) -> (f32, f32, f32, f32) {
    let (sw_f, sh_f) = (f64::from(view.sw), f64::from(view.sh));
    let x0 = tx as f64 * sw_f / view.cols as f64;
    let x1 = (tx + 1) as f64 * sw_f / view.cols as f64;
    let y0 = ty as f64 * sh_f / view.rows as f64;
    let y1 = (ty + 1) as f64 * sh_f / view.rows as f64;
    let sw_i = i64::from(view.sw);
    let sh_i = i64::from(view.sh);
    let sx0 = (x0.floor() as i64).clamp(0, sw_i) as usize;
    let sx1 = (x1.ceil() as i64).clamp(0, sw_i) as usize;
    let sy0 = (y0.floor() as i64).clamp(0, sh_i) as usize;
    let sy1 = (y1.ceil() as i64).clamp(0, sh_i) as usize;
    let fp = Footprint { x0, x1, y0, y1 };
    let mut sums = Sums::default();
    for sy in sy0..sy1 {
        accumulate_scanline(view.raw, view.sw, sy, sx0, sx1, &fp, &mut sums);
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

/// Accumulate one source scanline's overlap with the target footprint into the
/// running premultiplied sums. Split from `box_average` for nesting depth;
/// the `u64` index product cannot wrap on capped sources.
fn accumulate_scanline(
    raw: &[u8],
    sw: u32,
    sy: usize,
    sx0: usize,
    sx1: usize,
    fp: &Footprint,
    sums: &mut Sums,
) {
    let row_base = sy as u64 * u64::from(sw);
    for sx in sx0..sx1 {
        let ox = (sx as f64 + 1.0).min(fp.x1) - (sx as f64).max(fp.x0);
        let oy = (sy as f64 + 1.0).min(fp.y1) - (sy as f64).max(fp.y0);
        accumulate_pixel(raw, row_base, sx, ox.max(0.0) * oy.max(0.0), sums);
    }
}

/// Fold one overlapped source pixel into the running sums; zero overlap is a
/// no-op. The byte index is bounds-checked: a short buffer yields no
/// contribution instead of a panic (release is `panic = "abort"`).
fn accumulate_pixel(raw: &[u8], row_base: u64, sx: usize, w: f64, sums: &mut Sums) {
    if w <= 0.0 {
        return;
    }
    let i = (row_base + sx as u64) * 4;
    let (b0, b1, b2, b3) = (i as usize, i as usize + 1, i as usize + 2, i as usize + 3);
    let bytes = [raw.get(b0), raw.get(b1), raw.get(b2), raw.get(b3)];
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

/// Luminance (0.0..1.0) to density-ramp glyph; exposed for tests.
///
/// Rec. 601 luma is the caller-side convention. Values outside 0..1 are
/// clamped; non-finite input maps to the darkest glyph.
///
/// The lookup walks [`DENSITY_RAMP`]'s own bytes — no per-call `Vec<char>`
/// (audit 2026-10-23 F-036): this runs once per converted cell, and the
/// ramp is pure ASCII, so `as_bytes()[i] as char` is exact. A second
/// `const` array of the ramp would only be a copy that can drift.
pub fn glyph_for_luminance(lum: f32) -> char {
    let bytes = DENSITY_RAMP.as_bytes();
    let last = bytes.len() - 1;
    let l = if lum.is_finite() {
        lum.clamp(0.0, 1.0)
    } else {
        0.0
    };
    bytes[(l * last as f32).round() as usize] as char
}

/// Persist a converted sprite next to the source file as `<stem>.sprite.json`;
/// load it back.
///
/// The JSON shape is `{cols, rows, chars: [...], colors: [[r,g,b], ...]}`
/// plus optional `radii: [rx, ry]` recording what the art was built for.
/// The stamp-less form only exists for backward compatibility: caches written
/// by older builds (or by hand) load fine but read back as unstamped, and
/// callers treat unstamped entries as misses (reconvert, audit P0-3).
#[cfg(any(test, feature = "test-support"))]
pub fn save_sprite(sprite: &AsciiSprite, path: &Path) -> Result<(), ImageAsciiError> {
    write_sprite_doc(sprite, path, None)
}

/// Write a sprite with the radii stamp recorded. New writers (e.g. the CLI
/// `add` path) should prefer this so readers can reuse the cache without
/// reconverting when the radii match.
pub fn save_sprite_stamped(
    sprite: &AsciiSprite,
    path: &Path,
    rx: i64,
    ry: i64,
) -> Result<(), ImageAsciiError> {
    write_sprite_doc(sprite, path, Some([rx, ry]))
}

fn write_sprite_doc(
    sprite: &AsciiSprite,
    path: &Path,
    radii: Option<[i64; 2]>,
) -> Result<(), ImageAsciiError> {
    let doc = SpriteDoc {
        cols: sprite.cols(),
        rows: sprite.rows(),
        chars: sprite.chars().to_vec(),
        colors: sprite.colors().to_vec(),
        radii,
    };
    let text = serde_json::to_string(&doc)
        .map_err(|e| ImageAsciiError::Ans(format!("sprite encode failed: {e}")))?;
    // Temp **then rename**: the watcher worker writes this cache while
    // the typed loader may be reading it, and a plain write left a torn
    // half-JSON file visible to that reader (audit 2026-10-22 §P2-30) —
    // the same discipline every other JSON write in the core follows.
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "sprite.json".to_string());
    let temp = match path.parent() {
        Some(parent) => parent.join(format!("{file_name}.{}.tmp", std::process::id())),
        None => std::path::PathBuf::from(format!("{file_name}.{}.tmp", std::process::id())),
    };
    std::fs::write(&temp, text)?;
    if let Err(source) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(source.into());
    }
    Ok(())
}

/// Load a sprite previously written by the save helpers.
pub fn load_sprite(path: &Path) -> Result<AsciiSprite, ImageAsciiError> {
    let text = std::fs::read_to_string(path)?;
    let doc: SpriteDoc = serde_json::from_str(&text)
        .map_err(|e| ImageAsciiError::Ans(format!("sprite decode failed: {e}")))?;
    AsciiSprite::from_cells(doc.cols, doc.rows, doc.chars, doc.colors)
        .ok_or_else(|| ImageAsciiError::Ans("sprite dimensions mismatch".to_string()))
}

fn target_size(rx: i64, ry: i64) -> Result<(usize, usize), ImageAsciiError> {
    if rx <= 0 || ry <= 0 || rx > MAX_RADIUS || ry > MAX_RADIUS {
        return Err(ImageAsciiError::BadSize(format!("rx={rx} ry={ry}")));
    }
    if rx > REASONABLE_RADIUS || ry > REASONABLE_RADIUS {
        return Err(ImageAsciiError::BadSize(format!(
            "rx={rx} ry={ry} exceeds reasonable {REASONABLE_RADIUS}"
        )));
    }
    Ok(((2 * rx + 1) as usize, (2 * ry + 1) as usize))
}

/// Probe image dimensions from the header without a full decode and reject
/// sources above the megapixel cap. The probe runs BEFORE the decode, so an
/// oversize source never reaches the allocator (audit 2026-10-17 §Robustness).
fn check_source_dimensions(bytes: &[u8]) -> Result<(u32, u32), ImageAsciiError> {
    let reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    let (w, h) = reader.into_dimensions()?;
    check_source_size(w, h)?;
    Ok((w, h))
}

/// The size policy for a decoded source: at most [`MAX_SOURCE_PIXELS`]
/// pixels and at most [`MAX_SOURCE_EDGE`] pixels per edge.
fn check_source_size(w: u32, h: u32) -> Result<(), ImageAsciiError> {
    let pixels = u64::from(w) * u64::from(h);
    if pixels == 0 {
        return Err(ImageAsciiError::BadSize(format!("empty image {w}x{h}")));
    }
    if pixels > MAX_SOURCE_PIXELS {
        return Err(ImageAsciiError::BadSize(format!(
            "image {w}x{h} exceeds {MAX_SOURCE_PIXELS} pixels"
        )));
    }
    if w > MAX_SOURCE_EDGE || h > MAX_SOURCE_EDGE {
        return Err(ImageAsciiError::BadSize(format!(
            "image {w}x{h} exceeds the {MAX_SOURCE_EDGE} px edge limit"
        )));
    }
    Ok(())
}

/// Decode the source into a flat RGBA buffer.
///
/// The destination is reserved fallibly before the decode: it is exactly the
/// byte count the RGBA conversion needs, so a refused reservation means the
/// conversion could not have succeeded either. The failure is reported as an
/// error instead of aborting the process — release builds use
/// `panic = "abort"` (audit 2026-10-17 §Robustness).
fn decode_rgba(bytes: &[u8], sw: u32, sh: u32) -> Result<Vec<u8>, ImageAsciiError> {
    let len = rgba_len(u64::from(sw) * u64::from(sh))?;
    let mut raw = reserve_vec::<u8>(len, "source rgba buffer")?;
    let mut reader = image::ImageReader::new(Cursor::new(bytes)).with_guessed_format()?;
    reader.limits(source_limits());
    let img = reader.decode()?;
    let rgba = img.into_rgba8();
    if rgba.dimensions() != (sw, sh) {
        return Err(ImageAsciiError::BadSize(format!(
            "image decoded as {}x{}",
            rgba.width(),
            rgba.height()
        )));
    }
    raw.extend_from_slice(rgba.as_raw());
    Ok(raw)
}

/// Decoder limits: the source edge is bounded so a header that under-reports
/// its size cannot make the decoder allocate an unbounded frame.
fn source_limits() -> image::Limits {
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_EDGE);
    limits.max_image_height = Some(MAX_SOURCE_EDGE);
    limits
}

/// Byte count of the RGBA buffer for `pixels` pixels.
fn rgba_len(pixels: u64) -> Result<usize, ImageAsciiError> {
    let bytes = pixels
        .checked_mul(4)
        .ok_or_else(|| ImageAsciiError::Alloc {
            what: "source rgba buffer",
            bytes: u64::MAX,
            reason: "pixel count overflows".to_string(),
        })?;
    usize::try_from(bytes).map_err(|_| ImageAsciiError::Alloc {
        what: "source rgba buffer",
        bytes,
        reason: "size exceeds the address space".to_string(),
    })
}

/// Reserve `count` elements fallibly and report a clean error when the
/// allocator refuses. Release builds abort on panic, so an allocation
/// failure has to be an `Err`, never a fatal abort (audit §Robustness).
fn reserve_vec<T>(count: usize, what: &'static str) -> Result<Vec<T>, ImageAsciiError> {
    let mut buf: Vec<T> = Vec::new();
    buf.try_reserve_exact(count)
        .map_err(|source| ImageAsciiError::Alloc {
            what,
            bytes: (count as u64).saturating_mul(std::mem::size_of::<T>().max(1) as u64),
            reason: source.to_string(),
        })?;
    Ok(buf)
}

#[derive(serde::Serialize, serde::Deserialize)]
struct SpriteDoc {
    cols: usize,
    rows: usize,
    chars: Vec<char>,
    colors: Vec<[u8; 3]>,
    /// Radii the art was converted for. `None` on caches written by older
    /// builds or by hand; callers treat those as misses (reconvert).
    #[serde(default)]
    radii: Option<[i64; 2]>,
}

/// Radii stamp recorded in a `.sprite.json` cache: `None` when the cache
/// predates stamps (or is hand-written). The watcher treats `None` or a
/// radii mismatch as a miss and reconverts (audit P0-3); tolerant of corrupt
/// files (returns `None`, never errors).
pub fn stamped_radii(path: &Path) -> Option<(i64, i64)> {
    let text = std::fs::read_to_string(path).ok()?;
    let doc: SpriteDoc = serde_json::from_str(&text).ok()?;
    let [rx, ry] = doc.radii?;
    Some((rx, ry))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn png_bytes(pixel: image::Rgba<u8>, w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, pixel);
        let mut cur = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut cur, image::ImageFormat::Png)
            .expect("encode png");
        cur.into_inner()
    }

    /// Every cell of a `cols x rows` grid in row-major order, as `(x, y)`.
    /// One flat iterator instead of a nested loop (audit §Nesting).
    fn cells_of(cols: usize, rows: usize) -> impl Iterator<Item = (usize, usize)> {
        (0..rows).flat_map(move |y| (0..cols).map(move |x| (x, y)))
    }

    /// A BMP header that claims `w x h`; enough for the dimension probe and
    /// deliberately not a decodable image.
    fn bmp_header(w: u32, h: u32) -> Vec<u8> {
        let mut out = Vec::with_capacity(54);
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(w as i32).to_le_bytes());
        out.extend_from_slice(&(h as i32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out
    }

    #[test]
    fn ramp_endpoints_and_monotonic() {
        let ramp: Vec<char> = DENSITY_RAMP.chars().collect();
        assert_eq!(glyph_for_luminance(0.0), ramp[0]);
        assert_eq!(glyph_for_luminance(1.0), ramp[ramp.len() - 1]);
        let last = ramp.len() - 1;
        let mut prev = 0usize;
        for k in 0..=200 {
            let lum = k as f32 / 200.0;
            let idx = (lum.clamp(0.0, 1.0) * last as f32).round() as usize;
            assert_eq!(
                glyph_for_luminance(lum),
                ramp[idx],
                "mapping drift at {lum}"
            );
            assert!(idx >= prev, "index dipped at {lum}");
            prev = idx;
        }
    }

    #[test]
    fn transparent_png_converts_to_empty() {
        let bytes = png_bytes(image::Rgba([0, 0, 0, 0]), 16, 16);
        let s = convert_image_bytes(&bytes, 6, 3).expect("convert");
        assert_eq!((s.cols(), s.rows()), (13, 7));
        assert!(s.is_empty());
        assert_eq!(s.ink_count(), 0);
    }

    #[test]
    fn white_png_inks_exactly_the_disc() {
        let (rx, ry) = (6, 3);
        let bytes = png_bytes(image::Rgba([255, 255, 255, 255]), 32, 32);
        let s = convert_image_bytes(&bytes, rx, ry).expect("convert");
        assert_eq!((s.cols(), s.rows()), (13, 7));
        let inside = inside_set(rx, ry);
        assert_eq!(s.ink_count(), inside.len());
        for (x, y) in cells_of(s.cols(), s.rows()) {
            let (ch, _) = s.get(x, y).unwrap();
            let dx = x as i64 - rx;
            let dy = y as i64 - ry;
            if inside.contains(&(dx, dy)) {
                assert_ne!(ch, ' ', "hole at {x},{y}");
            } else {
                assert_eq!(ch, ' ', "ink escaped at {x},{y}");
            }
        }
        assert_eq!(s.get(0, 0).unwrap().0, ' ');
        assert_eq!(s.get(12, 0).unwrap().0, ' ');
        assert_eq!(s.get(0, 6).unwrap().0, ' ');
        assert_eq!(s.get(12, 6).unwrap().0, ' ');
    }

    #[test]
    fn black_png_uses_darkest_glyph() {
        let (rx, ry) = (6, 3);
        let bytes = png_bytes(image::Rgba([0, 0, 0, 255]), 32, 32);
        let s = convert_image_bytes(&bytes, rx, ry).expect("convert");
        let dark = glyph_for_luminance(0.0);
        assert_eq!(dark, DENSITY_RAMP.chars().next().unwrap());
        let inside = inside_set(rx, ry);
        for (x, y) in cells_of(s.cols(), s.rows()) {
            let (ch, _) = s.get(x, y).unwrap();
            let dx = x as i64 - rx;
            let dy = y as i64 - ry;
            if inside.contains(&(dx, dy)) {
                assert_eq!(ch, dark, "disc cell {x},{y} not darkest");
            } else {
                assert_eq!(ch, ' ', "corner {x},{y} not masked");
            }
        }
    }

    #[test]
    fn alpha_threshold_blanks_thin_cells() {
        let mut img = image::RgbaImage::new(32, 16);
        for (x, y) in cells_of(32, 16) {
            let a = if x < 16 { 255u8 } else { 0u8 };
            img.put_pixel(x as u32, y as u32, image::Rgba([200, 200, 200, a]));
        }
        let mut cur = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut cur, image::ImageFormat::Png)
            .expect("encode");
        let s = convert_image_bytes(&cur.into_inner(), 7, 3).expect("convert");
        assert_eq!((s.cols(), s.rows()), (15, 7));
        let (mut left_ink, mut right_ink) = (0, 0);
        for (x, y) in cells_of(s.cols(), s.rows()) {
            let (ch, _) = s.get(x, y).unwrap();
            if ch == ' ' {
                continue;
            }
            // The seam column itself counts as neither half: left of it is
            // the opaque half, right of it the transparent one.
            match x.cmp(&7) {
                std::cmp::Ordering::Less => left_ink += 1,
                std::cmp::Ordering::Greater => right_ink += 1,
                std::cmp::Ordering::Equal => {}
            }
        }
        assert!(left_ink > 0, "opaque half must ink");
        assert_eq!(right_ink, 0, "transparent half leaked {right_ink} cells");
    }

    #[test]
    fn sizing_matches_radii() {
        let bytes = png_bytes(image::Rgba([255, 0, 0, 255]), 8, 8);
        let s = convert_image_bytes(&bytes, 5, 2).expect("convert");
        assert_eq!((s.cols(), s.rows()), (11, 5));
        let t = convert_image_bytes(&bytes, 9, 4).expect("convert");
        assert_eq!((t.cols(), t.rows()), (19, 9));
    }

    #[test]
    fn bad_radii_rejected() {
        let bytes = png_bytes(image::Rgba([255, 0, 0, 255]), 4, 4);
        assert!(convert_image_bytes(&bytes, 0, 3).is_err());
        assert!(convert_image_bytes(&bytes, 4, 0).is_err());
        assert!(convert_image_bytes(&bytes, -2, 3).is_err());
        assert!(convert_image_bytes(&bytes, 500, 500).is_err());
    }

    #[test]
    fn oversize_source_rejected_before_decode() {
        // 5000x5000 is 25 Mpx, over the 16.7 Mpx cap. The error must be the
        // size cap, not a decode failure: the probe runs before the decode.
        let bytes = bmp_header(5000, 5000);
        let err = convert_image_bytes(&bytes, 4, 2).expect_err("oversize source rejected");
        assert!(
            matches!(err, ImageAsciiError::BadSize(_)),
            "expected the size cap, got {err:?}"
        );
        assert!(format!("{err}").contains("exceeds"));
    }

    #[test]
    fn over_wide_source_rejected() {
        // 20000x2 is under the pixel cap but over the edge limit.
        let bytes = bmp_header(20000, 2);
        let err = convert_image_bytes(&bytes, 4, 2).expect_err("over-wide source rejected");
        assert!(
            matches!(err, ImageAsciiError::BadSize(_)),
            "expected the edge cap, got {err:?}"
        );
    }

    #[test]
    fn undecodable_bytes_report_a_decode_error() {
        let empty: Vec<u8> = Vec::new();
        let err = convert_image_bytes(&empty, 4, 2).expect_err("empty bytes fail");
        assert!(matches!(
            err,
            ImageAsciiError::Decode(_) | ImageAsciiError::Io(_)
        ));
    }

    #[test]
    fn allocation_failure_is_an_error_not_a_panic() {
        // A refused reservation must surface as Err, never as an abort:
        // release builds use `panic = "abort"`.
        let err = reserve_vec::<u8>(usize::MAX, "test buffer")
            .expect_err("an impossible reservation fails");
        assert!(matches!(err, ImageAsciiError::Alloc { .. }));
        assert!(format!("{err}").contains("cannot allocate"));
        let err = reserve_vec::<char>(usize::MAX, "test buffer")
            .expect_err("an impossible reservation fails");
        assert!(matches!(err, ImageAsciiError::Alloc { .. }));
    }

    #[test]
    fn ans_kept_verbatim_trimmed_masked_repainted() {
        let text = "\n\n   ##   \n   ##   \n\n\n";
        let s = convert_ans_text(text, 4, 2, [10, 20, 30]).expect("ans");
        assert_eq!((s.cols(), s.rows()), (9, 5));
        for (x, y) in cells_of(s.cols(), s.rows()) {
            let (ch, c) = s.get(x, y).unwrap();
            if ch != ' ' {
                assert_eq!(ch, '#', "glyph altered at {x},{y}");
                assert_eq!(c, [10, 20, 30], "colour not repainted at {x},{y}");
            }
        }
        assert!(s.ink_count() > 0);
        assert_eq!(s.get(0, 0).unwrap().0, ' ');
        assert_eq!(s.get(8, 0).unwrap().0, ' ');
        assert_eq!(s.get(0, 4).unwrap().0, ' ');
        assert_eq!(s.get(8, 4).unwrap().0, ' ');
        assert!(convert_ans_text("   \n  \n", 4, 2, [1, 2, 3]).is_err());
    }

    #[test]
    fn oversize_ans_text_is_rejected() {
        let text = "#".repeat(MAX_ANS_BYTES + 1);
        let err = convert_ans_text(&text, 4, 2, [1, 2, 3]).expect_err("oversize art rejected");
        assert!(matches!(err, ImageAsciiError::Ans(_)));
        assert!(format!("{err}").contains("exceeds"));
    }

    #[test]
    fn sprite_round_trips_exactly() {
        let bytes = png_bytes(image::Rgba([255, 255, 255, 255]), 24, 24);
        let s = convert_image_bytes(&bytes, 5, 2).expect("convert");
        let path = std::env::temp_dir().join(format!(
            "ducks-sprite-{}-{}.sprite.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        save_sprite(&s, &path).expect("save");
        assert_eq!(
            stamped_radii(&path),
            None,
            "unstamped writer stays unstamped"
        );
        let _ = std::fs::remove_file(&path);
        save_sprite_stamped(&s, &path, 5, 2).expect("stamped save");
        let back = load_sprite(&path).expect("load");
        assert_eq!(s, back);
        assert_eq!(stamped_radii(&path), Some((5, 2)));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn unstamped_legacy_cache_reads_none() {
        let path = std::env::temp_dir().join(format!(
            "ducks-legacy-{}-{}.sprite.json",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(
            &path,
            r#"{"cols":2,"rows":1,"chars":["@","@"],"colors":[[1,2,3],[1,2,3]]}"#,
        )
        .expect("write legacy cache");
        assert_eq!(stamped_radii(&path), None);
        let back = load_sprite(&path).expect("legacy cache still loads");
        assert_eq!((back.cols(), back.rows()), (2, 1));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn oversize_ans_errors_with_message() {
        let text = "##########\n##########\n##########\n";
        let err = convert_ans_text(text, 2, 1, [1, 2, 3]).expect_err("oversize art errors");
        assert!(matches!(err, ImageAsciiError::Ans(_)));
        assert!(format!("{err}").contains("exceeds disc"));
    }

    #[test]
    fn file_entry_points_sniff_content() {
        let bytes = png_bytes(image::Rgba([0, 255, 0, 255]), 16, 16);
        let dir = std::env::temp_dir().join(format!(
            "ducks-img-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir");
        let img_path = dir.join("mislabeled.bmp");
        std::fs::write(&img_path, &bytes).expect("write png bytes");
        let s = convert_image_file(&img_path, 4, 2).expect("convert file");
        assert_eq!((s.cols(), s.rows()), (9, 5));
        let ans_path = dir.join("art.ans");
        std::fs::write(&ans_path, "##\n##\n").expect("write ans");
        let a = convert_ans_file(&ans_path, 4, 2, [7, 8, 9]).expect("convert ans");
        assert_eq!((a.cols(), a.rows()), (9, 5));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
