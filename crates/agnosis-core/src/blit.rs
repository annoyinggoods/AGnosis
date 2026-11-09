//! CPU glyph blitter: grid cells into an RGBA8 framebuffer.
//!
//! Each non-space [`crate::grid::Grid`] cell is blended through its cached
//! [`crate::font::GlyphMask`] coverage into a [`FrameBuffer`]. Space cells are
//! skipped entirely, so blit cost scales with visible content (~2000 cells at
//! 1080p, ~1ms) rather than with resolution. The whole grid is shifted by one
//! uniform sub-cell pixel offset, which is what makes the parade scroll
//! smoothly: ducks move in floating point and the fractional part becomes a
//! pixel shift of the fully rasterized grid.
//!
//! Row range and scroll offset are the caller's business: [`blit_scene`]
//! is the **one** frame composition — the sky, the parallax layers and the
//! scene grid — and the two-pass offset split lives inside it: the parade
//! rows are blitted with the sub-cell offset, the grass/HUD/input rows at
//! offset 0, so static content never shakes (see [`blit_grid_rows_between`]).
//! The GPU popup, the headless gate and the web front end all call
//! `blit_scene`, so the three cannot drift.
//!
//! Uniform-scale rows (audit 2026-10-17 § Maintainability, "uniform-row
//! blit threading"): the override that used to be threaded through the
//! production call sites is gone. It pointed at a single row below the
//! block-font HUD (which occupies `rows-6 .. rows-2`), so it never took
//! effect, and the block font renders every on-screen glyph as the same
//! `X` cell — there is no mixed-scale body text left to normalize. The
//! uniform-scale raster path itself ([`Font::mask_uniform`]) survives on the
//! test-only [`blit_grid_rows`] wrapper, which the audit prototype and the
//! uniform-scale unit tests still use.

use crate::font::{Font, GlyphMask};
use crate::grid::Grid;

/// What one blit call walked and blitted — the F-165 counters (roadmap
/// HELP window §4.1, "measure first").
///
/// `blit_rows` returns the tally of its own call, `blit_scene` sums the
/// tallies of the frame's blits and reports them in ONE trace line per
/// frame, only when `AGNOSIS_TRACE` is on. No behaviour change and no
/// state: a tally is a plain value, so nothing can race and nothing
/// accumulates between frames. Revisit row-sliced iteration only if a
/// profile ever flags the blit; until then these are the numbers to
/// argue with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlitTally {
    /// The rows the blit walked, inked or not.
    pub rows_walked: usize,
    /// The rows that carried at least one non-space cell.
    pub rows_inked: usize,
    /// The non-space cells in the walked band.
    pub cells: usize,
}

impl BlitTally {
    /// Fold another tally into this one.
    fn merge(&mut self, other: BlitTally) {
        self.rows_walked += other.rows_walked;
        self.rows_inked += other.rows_inked;
        self.cells += other.cells;
    }
}

/// Coverage denominator: a glyph mask pixel is fully inked at 255.
const COVERAGE_MAX: u32 = 255;

/// Opaque alpha written for every inked pixel.
const ALPHA_OPAQUE: u8 = 255;

/// Cell size (in px) at or below which any nonzero coverage blends as full
/// ink. At the audit cell `[2,3]` an `X` covers 2 of 6 px and at the 4K
/// cell `[4,6]` the same ratio — art would otherwise read at ~33%
/// brightness (audit 2026-10-06 §4 consequence 1).
const FLOOR_CELL_MAX: i64 = 6;

/// How many rows at the bottom of the grid are **chrome** (must not scroll
/// with the parade): the counter sits at `rows - 12` and the hint at
/// `rows - 6`, so 13 rows cover both bands with one margin row
/// (audit 2026-10-23 F-022).
///
/// WHY a blit-side constant: the grass row is the split on a tall grid,
/// but a short grid (the web tier draws `rows - ground` of 16..=19) puts
/// the counter **above** the grass — those rows scrolled with the parade
/// and the hint/counter smeared a few pixels on every refresh.
const BOTTOM_CHROME_ROWS: usize = 13;

/// Rounding offset of the coverage blend: half of the coverage range, so
/// the blend rounds to nearest instead of truncating.
const BLEND_ROUND_OFFSET: u32 = 127;

/// RGBA8 pixel buffer, 4 bytes per pixel, alpha always 255.
#[derive(Clone, Debug)]
pub struct FrameBuffer {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Row-major `[R, G, B, A]` bytes, `len == width * height * 4`.
    pub data: Vec<u8>,
}

impl FrameBuffer {
    /// Allocate a black buffer of `width x height` pixels.
    ///
    /// The size math cannot wrap: on a 32-bit host (the wasm tier) a
    /// `width * height` in `usize` wraps silently and the buffer came out
    /// shorter than the documented `len == width * height * 4` invariant —
    /// every later write indexed past the Vec. The bytes are counted in
    /// `u64` with saturating arithmetic (audit 2026-10-23 F-038): a size
    /// the address space cannot hold is traced and yields an **empty**
    /// buffer — a later write then fails loudly at the first
    /// out-of-range pixel instead of scribbling into a too-short Vec —
    /// and a size that fits `usize` but not the machine's memory aborts
    /// in the allocator itself, the way every `vec!` does.
    pub fn new(width: u32, height: u32) -> Self {
        let bytes = (width as u64)
            .saturating_mul(height as u64)
            .saturating_mul(4);
        let n = match usize::try_from(bytes) {
            Ok(n) => n,
            Err(_) => {
                // A size the address space cannot hold: traced, and an
                // empty buffer instead of a wrapped one — a later write
                // fails loudly at the first out-of-range pixel rather
                // than scribbling into a too-short Vec.
                crate::trace::warn(&format!(
                    "blit: the {width}x{height} framebuffer exceeds the address space"
                ));
                0
            }
        };
        Self {
            width,
            height,
            data: vec![0u8; n],
        }
    }

    /// Fill every pixel with opaque black.
    pub fn clear_black(&mut self) {
        self.data.fill(0);
        for px in self.data.chunks_exact_mut(4) {
            px[3] = ALPHA_OPAQUE;
        }
    }

    /// Read one pixel. Out-of-bounds reads return opaque black.
    ///
    /// Test-only accessor: the production path writes pixels through
    /// [`crate::blit::stamp_mask`] and never reads them back
    /// (audit dead-code sweep — no production caller).
    #[cfg(any(test, feature = "test-support"))]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0, 0, 0, ALPHA_OPAQUE];
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// Write one pixel. Out-of-bounds writes are dropped.
    ///
    /// Test-only accessor: the production path writes pixels through
    /// [`crate::blit::stamp_mask`] (audit dead-code sweep).
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_pixel(&mut self, x: u32, y: u32, px: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = (y as usize * self.width as usize + x as usize) * 4;
        self.data[i..i + 4].copy_from_slice(&px);
    }

    /// Raw RGBA8 bytes, row-major.
    ///
    /// Test-only accessor: the production path uploads [`FrameBuffer::data`]
    /// straight to the GPU (audit dead-code sweep).
    #[cfg(any(test, feature = "test-support"))]
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

/// Blend one channel: `dst = (fg * cov + dst * (255 - cov)) / 255`, rounded.
fn blend_channel(fg: u8, dst: u8, cov: u8) -> u8 {
    let fg = fg as u32;
    let dst = dst as u32;
    let cov = cov as u32;
    ((fg * cov + dst * (COVERAGE_MAX - cov) + BLEND_ROUND_OFFSET) / COVERAGE_MAX) as u8
}

/// Blit every non-space grid cell into `fb`.
///
/// Test-only convenience wrapper: production blits a row range through
/// [`blit_grid_rows_between`] so the static rows keep offset 0 (audit
/// dead-code sweep — no production caller).
#[cfg(any(test, feature = "test-support"))]
pub fn blit_grid(grid: &Grid, font: &mut Font, fb: &mut FrameBuffer, offset_px: i32) -> BlitTally {
    blit_grid_rows(grid, font, fb, offset_px, &[])
}

/// Every row of `grid`, with a per-row uniform-text override.
///
/// Test-only wrapper (audit 2026-10-17 § Maintainability): the override is
/// what the audit prototype (`audit_proto`) and the uniform-scale unit tests
/// need; the production call sites never request one (see the module doc).
#[cfg(any(test, feature = "test-support"))]
pub fn blit_grid_rows(
    grid: &Grid,
    font: &mut Font,
    fb: &mut FrameBuffer,
    offset_px: i32,
    uniform_rows: &[usize],
) -> BlitTally {
    let end = grid.rows();
    blit_rows(grid, font, fb, offset_px, 0, 0..end, uniform_rows)
}

/// Blit the rows of `grid` in `rows` into `fb` — cells outside the range are
/// skipped entirely.
///
/// The cell at `(col, row)` lands at pixel origin
/// `(col * cell_w + offset_px, row * cell_h)`. `offset_px` is the uniform
/// sub-cell horizontal scroll offset shared by the whole blit and may be
/// negative. Cells whose rect lies entirely outside `fb` are skipped;
/// partially visible cells are clipped. Blending follows the coverage
/// formula per channel and leaves alpha at 255.
///
/// The parade scrolls with a uniform sub-cell pixel offset so duck motion is
/// smooth, but that offset must never touch static content (grass, HUD,
/// input line): [`blit_scene`] blits the rows above the grass with the
/// offset and everything from the grass down with offset 0, or the frame
/// shakes a few pixels left/right on every refresh.
///
/// The one caller that matters is [`blit_scene`] — the GPU popup path, the
/// headless PNG gate and the web front end all go through it, so the split
/// lives in exactly one place (audit 2026-10-21 §F6).
pub fn blit_grid_rows_between(
    grid: &Grid,
    font: &mut Font,
    fb: &mut FrameBuffer,
    offset_px: i32,
    rows: std::ops::Range<usize>,
) -> BlitTally {
    blit_grid_rows_at(grid, font, fb, offset_px, 0, rows)
}

/// Blit a **body-text** grid: every alphanumeric glyph takes the
/// fixed-scale [`Font::mask_uniform`] path, so mixed-case prose reads at
/// one visual size instead of every glyph being stretched to fill its
/// cell.
///
/// This is the HELP window's blit (roadmap HELP_WINDOW §1): README prose
/// is lowercase, and the ink-fit path the block font relies on would
/// smear a narrow `i` across the cell. Punctuation and the line-art
/// characters keep the default path, so a heading's `-` rule still
/// connects across cell edges.
pub fn blit_text_between(
    grid: &Grid,
    font: &mut Font,
    fb: &mut FrameBuffer,
    offset_px: i32,
    rows: std::ops::Range<usize>,
) -> BlitTally {
    let uniform: Vec<usize> = rows.clone().collect();
    blit_rows(grid, font, fb, offset_px, 0, rows, &uniform)
}

/// Blit the rows of `grid` in `rows` into `fb`, with the grid's first row
/// landing on the framebuffer's `row_offset`-th row.
///
/// This is the **layer** blit: a parallax layer is a grid of its own that
/// does not start at the top of the screen (a backdrop band sits just
/// above the grass) and that scrolls at its own rate (its own `offset_px`).
/// Everything else about the blit is the same as
/// [`blit_grid_rows_between`]'s.
pub fn blit_grid_rows_at(
    grid: &Grid,
    font: &mut Font,
    fb: &mut FrameBuffer,
    offset_px: i32,
    row_offset: usize,
    rows: std::ops::Range<usize>,
) -> BlitTally {
    blit_rows(grid, font, fb, offset_px, row_offset, rows, &[])
}

/// Shared blit implementation behind every public entry point.
///
/// `uniform_rows` lists the rows whose alphanumeric glyphs render through
/// the fixed-scale [`Font::mask_uniform`] path, so one row reads at one
/// visual size even when glyphs differ in ink width. The scene blits pass
/// an empty slice (the block font already renders one glyph everywhere);
/// the body-text blit ([`blit_text_between`], the HELP window) asks for
/// every row.
fn blit_rows(
    grid: &Grid,
    font: &mut Font,
    fb: &mut FrameBuffer,
    offset_px: i32,
    row_offset: usize,
    rows: std::ops::Range<usize>,
    uniform_rows: &[usize],
) -> BlitTally {
    let cell_w = grid.cell_w() as i64;
    let cell_h = grid.cell_h() as i64;
    if cell_w <= 0 || cell_h <= 0 {
        return BlitTally::default();
    }
    // Brightness floor: at tiny cells glyph coverage leaves art at ~33%
    // brightness — an 'X' covers 2 of 6 px at the audit cell [2,3] and the
    // same ratio at the 4K cell [4,6] — so any nonzero coverage blends as
    // full ink (audit 2026-10-06 §4 consequence 1).
    let floor_coverage = cell_w <= FLOOR_CELL_MAX && cell_h <= FLOOR_CELL_MAX;
    let fb_w = fb.width as i64;
    let fb_h = fb.height as i64;
    let ctx = BlitCtx {
        cell_w,
        cell_h,
        fb_w,
        fb_h,
        offset_px: offset_px as i64,
        row_offset: row_offset as i64,
        rows: rows.clone(),
        floor_coverage,
        uniform_rows,
    };
    // The F-165 tally (§4.1): rows walked, rows carrying ink, cells
    // blitted. Counted per call — a plain value, no state — and
    // reported by `blit_scene` only when tracing is on.
    let mut inked = vec![false; rows.len()];
    let mut cells = 0usize;
    let rows_start = rows.start;
    grid.for_each_cell(|col, row, ch, color| {
        blit_cell(
            &ctx,
            font,
            fb,
            Cell {
                col,
                row,
                ch,
                color,
            },
        );
        if ch == ' ' || !ctx.rows.contains(&row) {
            return;
        }
        cells += 1;
        if let Some(slot) = inked.get_mut(row - rows_start) {
            *slot = true;
        }
    });
    BlitTally {
        rows_walked: rows.len(),
        rows_inked: inked.iter().filter(|seen| **seen).count(),
        cells,
    }
}

/// Shared per-blit constants so `blit_cell` stays a flat guard chain.
struct BlitCtx<'a> {
    cell_w: i64,
    cell_h: i64,
    fb_w: i64,
    fb_h: i64,
    offset_px: i64,
    /// The framebuffer row the grid's first row lands on.
    row_offset: i64,
    rows: std::ops::Range<usize>,
    floor_coverage: bool,
    /// Rows whose alphanumeric glyphs take the fixed-scale raster path.
    uniform_rows: &'a [usize],
}

/// One grid cell queued for blitting.
struct Cell {
    col: usize,
    row: usize,
    ch: char,
    color: [u8; 3],
}

/// Blit one grid cell: skip spaces, out-of-range rows, and off-frame cells,
/// then stamp the glyph mask row by row. Extracted from
/// `blit_grid_rows_between` for nesting depth (audit site #10); every early
/// exit is a flat guard, never a nested branch.
fn blit_cell(ctx: &BlitCtx, font: &mut Font, fb: &mut FrameBuffer, cell: Cell) {
    if cell.ch == ' ' {
        return;
    }
    if cell.row < ctx.rows.start || cell.row >= ctx.rows.end {
        return;
    }
    let uniform = ctx.uniform_rows.contains(&cell.row) && cell.ch.is_ascii_alphanumeric();
    let mask = match uniform {
        true => font.mask_uniform(cell.ch),
        false => font.mask(cell.ch),
    };
    let Some(mask) = mask else {
        return;
    };
    let x0 = cell.col as i64 * ctx.cell_w + ctx.offset_px;
    let y0 = (cell.row as i64 + ctx.row_offset) * ctx.cell_h;
    if x0 + ctx.cell_w <= 0 || x0 >= ctx.fb_w {
        return;
    }
    if y0 + ctx.cell_h <= 0 || y0 >= ctx.fb_h {
        return;
    }
    stamp_mask(fb, &mask, cell.color, x0, y0, ctx);
}

/// Stamp one glyph mask into the framebuffer with per-channel coverage
/// blending. Rows and columns outside the frame are clipped, not wrapped.
fn stamp_mask(
    fb: &mut FrameBuffer,
    mask: &GlyphMask,
    color: [u8; 3],
    x0: i64,
    y0: i64,
    ctx: &BlitCtx,
) {
    let mh = mask.h as i64;
    for my in 0..mh {
        stamp_mask_row(fb, mask, color, x0, y0 + my, my, ctx);
    }
}

/// Stamp one mask row. Split from `stamp_mask` so no loop nests a loop.
fn stamp_mask_row(
    fb: &mut FrameBuffer,
    mask: &GlyphMask,
    color: [u8; 3],
    x0: i64,
    py: i64,
    my: i64,
    ctx: &BlitCtx,
) {
    if py < 0 || py >= ctx.fb_h {
        return;
    }
    let mw = mask.w as i64;
    for mx in 0..mw {
        let mut cov = mask.coverage[my as usize * mask.w as usize + mx as usize];
        if cov == 0 {
            continue;
        }
        if ctx.floor_coverage {
            cov = COVERAGE_MAX as u8;
        }
        let px = x0 + mx;
        if px < 0 || px >= ctx.fb_w {
            continue;
        }
        let i = (py as usize * fb.width as usize + px as usize) * 4;
        let d = &mut fb.data;
        d[i] = blend_channel(color[0], d[i], cov);
        d[i + 1] = blend_channel(color[1], d[i + 1], cov);
        d[i + 2] = blend_channel(color[2], d[i + 2], cov);
        d[i + 3] = ALPHA_OPAQUE;
    }
}

/// Fill the framebuffer with a per-cell-row colour ramp.
///
/// The ramp carries one colour per **grid row**; the cell height maps the
/// grid rows onto the pixel rows. This is the sky (roadmap Extension 1
/// §3.1a): it is drawn **instead of** the black clear, not over it, so
/// the grid's transparent cells show the sky through them.
///
/// The fill is a straight pixel write — no glyph is rasterized — so the
/// sky costs one pass over the framebuffer, and the two-pass offset split
/// (the parade rows scroll, the rest does not) cannot make the sky drift:
/// a ramp has no horizontal structure to scroll.
pub fn fill_row_ramp(fb: &mut FrameBuffer, cell_h: u32, ramp: &[[u8; 3]]) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    let cell_h = cell_h.max(1) as usize;
    for (row, colour) in ramp.iter().enumerate() {
        let y0 = row * cell_h;
        if y0 >= height {
            break;
        }
        let rows = cell_h.min(height - y0);
        fill_band(fb, y0, rows, *colour, width);
    }
}

/// Fill `rows` pixel rows with one colour.
fn fill_band(fb: &mut FrameBuffer, y0: usize, rows: usize, colour: [u8; 3], width: usize) {
    let mut pixel = [0u8; 4];
    pixel[0] = colour[0];
    pixel[1] = colour[1];
    pixel[2] = colour[2];
    pixel[3] = ALPHA_OPAQUE;
    for y in y0..y0 + rows {
        fill_band_row(fb, y, width, &pixel);
    }
}

/// Fill one pixel row with one colour.
///
/// WHY a helper of its own: the nesting rule is two levels at most, and
/// the band fill is a loop in a loop — the same shape the sibling
/// `stamp_mask_row` was split out for (audit 2026-10-21 §F6).
fn fill_band_row(fb: &mut FrameBuffer, y: usize, width: usize, pixel: &[u8; 4]) {
    let start = y * width * 4;
    for chunk in fb.data[start..start + width * 4].chunks_exact_mut(4) {
        chunk.copy_from_slice(pixel);
    }
}

/// Composite one frame into `fb`: the sky, the parallax layers, the
/// scene grid and the message band.
///
/// This is the **one** frame composition. The GPU popup, the headless
/// gate and the web front end all call it, so the three can never drift —
/// and the exported frames are the app's frames, not a lookalike.
///
/// The order is the layer stack, back to front:
///
/// 1. the sky ramp **replaces** the black clear (it is a background, not
///    a layer over it);
/// 2. the parallax layers, each with its own scroll offset;
/// 3. the scene grid: the parade rows with the sub-cell offset, the
///    grass/HUD/input rows at offset 0;
/// 4. the **overlay** — the message's band, at offset 0.
///
/// WHY the overlay is separate: the message above the grass shares the
/// parade's band, and every row of that band scrolls with the ducks —
/// so a message in the scene's grid would jitter with them, although it
/// is chrome (audit 2026-10-21 §E7).
#[allow(clippy::too_many_arguments)]
pub fn blit_scene(
    fb: &mut FrameBuffer,
    font: &mut Font,
    grid: &Grid,
    offset_px: i32,
    ground_row: usize,
    sky_ramp: Option<&[[u8; 3]]>,
    layers: &[crate::layer::LayerBlit<'_>],
    overlay: Option<&crate::layer::LayerBlit<'_>>,
) {
    let rows = grid.rows();
    let ground = ground_row.min(rows);
    fb.clear_black();
    if let Some(ramp) = sky_ramp {
        fill_row_ramp(fb, grid.cell_h(), ramp);
    }
    let mut tally = BlitTally::default();
    for layer in layers {
        let layer_tally = blit_grid_rows_at(
            layer.grid,
            font,
            fb,
            layer.offset_px,
            layer.row_offset,
            0..layer.rows,
        );
        tally.merge(layer_tally);
    }
    // The parade rows scroll at the parade's rate; the grass and the HUD
    // sit at rate 0.0, so the text never smears.
    //
    // The `unwrap_or(0)` covers the layers with **no** fixed rate (a
    // backdrop scrolls at its own rate, which the layer blits carry).
    let parade =
        crate::layer::fixed_offset_px(crate::layer::LayerKind::Parade, offset_px).unwrap_or(0);
    let still =
        crate::layer::fixed_offset_px(crate::layer::LayerKind::Grass, offset_px).unwrap_or(0);
    // F-022: on a short grid the chrome bands (counter at `rows - 12`,
    // hint at `rows - 6`) reach **above** the grass row, so the split is
    // clamped down to the chrome's top — the rows between the split and
    // the grass are chrome that happens to live above the grass line, and
    // chrome never scrolls.
    let split = ground.min(rows.saturating_sub(BOTTOM_CHROME_ROWS));
    tally.merge(blit_grid_rows_between(grid, font, fb, parade, 0..split));
    tally.merge(blit_grid_rows_between(grid, font, fb, still, split..rows));
    // The message's band, over the scene, at offset 0.
    if let Some(overlay) = overlay {
        let overlay_tally = blit_grid_rows_at(
            overlay.grid,
            font,
            fb,
            overlay.offset_px,
            overlay.row_offset,
            0..overlay.rows,
        );
        tally.merge(overlay_tally);
    }
    report_blit_counters(&tally);
}

/// Emit the frame's F-165 tally as one trace line — one line per frame,
/// only when `AGNOSIS_TRACE` is on (decision F-165, "measure first").
fn report_blit_counters(tally: &BlitTally) {
    if !crate::trace::is_enabled() {
        return;
    }
    crate::trace::trace(&format!(
        "blit rows_walked={} rows_inked={} cells={}",
        tally.rows_walked, tally.rows_inked, tally.cells
    ));
}

/// Failures produced while writing a PNG screenshot.
#[derive(Debug, thiserror::Error)]
pub enum BlitError {
    /// Underlying file write failed.
    #[error("io error writing {path}: {source}")]
    Io {
        /// File that could not be written.
        path: std::path::PathBuf,
        /// OS error.
        #[source]
        source: std::io::Error,
    },
    /// Pixel data could not be encoded as PNG.
    #[error("failed to encode png for {path}: {source}")]
    Encode {
        /// File that could not be encoded.
        path: std::path::PathBuf,
        /// Encoder error.
        #[source]
        source: image::ImageError,
    },
}

/// Write `fb` to `path` as RGBA8 PNG through the `image` crate.
pub fn save_png(fb: &FrameBuffer, path: &std::path::Path) -> Result<(), BlitError> {
    image::save_buffer(path, &fb.data, fb.width, fb.height, image::ColorType::Rgba8).map_err(
        |source| match source {
            image::ImageError::IoError(e) => BlitError::Io {
                path: path.to_path_buf(),
                source: e,
            },
            other => BlitError::Encode {
                path: path.to_path_buf(),
                source: other,
            },
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::DEFAULT_FG;

    fn font12x24() -> Font {
        Font::new(12, 24).expect("embedded font must parse")
    }

    fn inked_columns(fb: &FrameBuffer) -> Vec<u32> {
        let mut cols = Vec::new();
        for x in 0..fb.width {
            if column_has_ink(fb, x) {
                cols.push(x);
            }
        }
        cols
    }

    /// True when any pixel in framebuffer column `x` carries RGB ink.
    /// Split from `inked_columns` so the test reads flat (audit site #20).
    fn column_has_ink(fb: &FrameBuffer, x: u32) -> bool {
        (0..fb.height).any(|y| {
            let p = fb.pixel(x, y);
            p[0] != 0 || p[1] != 0 || p[2] != 0
        })
    }

    #[test]
    fn clear_black_zeroes_rgb_and_sets_alpha() {
        let mut fb = FrameBuffer::new(7, 5);
        fb.data.fill(0xAB);
        fb.clear_black();
        assert_eq!(fb.data.len(), 7 * 5 * 4);
        for px in fb.data.chunks_exact(4) {
            assert_eq!(px, &[0, 0, 0, 255]);
        }
        assert_eq!(fb.as_bytes().len(), 7 * 5 * 4);
    }

    #[test]
    fn single_hash_cell_inks_only_its_own_rect() {
        let mut grid = Grid::new(4, 3, 12, 24);
        grid.set(2, 1, '#', [255, 255, 255]);
        let mut font = font12x24();
        let mut fb = FrameBuffer::new(4 * 12, 3 * 24);
        fb.clear_black();
        blit_grid(&grid, &mut font, &mut fb, 0);
        let (x0, x1, y0, y1) = (2 * 12, 3 * 12, 24, 2 * 24);
        let mut any_ink = false;
        for y in 0..fb.height {
            any_ink |= assert_scanline_ink_inside(&fb, y, x0, x1, y0, y1);
        }
        assert!(any_ink, "'#' must leave visible ink");
    }

    /// Assert every inked pixel on framebuffer row `y` lies inside the cell
    /// rect; returns true when the row carries ink. Split from the single-cell
    /// test so no loop nests a loop (audit site #20).
    fn assert_scanline_ink_inside(
        fb: &FrameBuffer,
        y: u32,
        x0: u32,
        x1: u32,
        y0: u32,
        y1: u32,
    ) -> bool {
        let mut inked = false;
        for x in 0..fb.width {
            let p = fb.pixel(x, y);
            if p[0] == 0 && p[1] == 0 && p[2] == 0 {
                continue;
            }
            inked = true;
            assert!(
                x >= x0 && x < x1 && y >= y0 && y < y1,
                "ink at ({x},{y}) outside cell rect"
            );
            assert_eq!(p[3], 255);
        }
        inked
    }

    #[test]
    fn mid_coverage_blending_matches_formula_within_one() {
        let mut font = font12x24();
        let mask = font.mask('#').expect("'#' must rasterize");
        let mut target = None;
        for (i, &cov) in mask.coverage.iter().enumerate() {
            if cov > 32 && cov < 224 {
                target = Some((i, cov));
                break;
            }
        }
        let (i, cov) = target.expect("'#' should have a mid-coverage pixel");
        let mx = (i % mask.w as usize) as u32;
        let my = (i / mask.w as usize) as u32;
        let mut grid = Grid::new(1, 1, 12, 24);
        grid.set(0, 0, '#', [200, 100, 50]);
        let mut fb = FrameBuffer::new(12, 24);
        fb.clear_black();
        fb.set_pixel(mx, my, [10, 20, 30, 255]);
        blit_grid(&grid, &mut font, &mut fb, 0);
        let got = fb.pixel(mx, my);
        for (c, fg, dst) in [(0, 200u8, 10u8), (1, 100, 20), (2, 50, 30)] {
            let want = (fg as u32 * cov as u32 + dst as u32 * (255 - cov as u32) + 127) / 255;
            assert!(
                (got[c] as i32 - want as i32).abs() <= 1,
                "channel {c}: got {} want {want} (cov {cov})",
                got[c]
            );
        }
        assert_eq!(got[3], 255);
    }

    #[test]
    fn space_cells_leave_background_untouched() {
        let grid = Grid::new(6, 4, 12, 24);
        let mut font = font12x24();
        let mut fb = FrameBuffer::new(6 * 12, 4 * 24);
        fb.clear_black();
        fb.set_pixel(3, 3, [40, 50, 60, 255]);
        let before = fb.data.clone();
        blit_grid(&grid, &mut font, &mut fb, 0);
        assert_eq!(fb.data, before);
        assert_eq!(font.cached_count(), 0, "spaces must not touch the cache");
    }

    #[test]
    fn offset_shifts_ink_by_exact_pixels_and_clips_both_edges() {
        let mut grid = Grid::new(3, 1, 12, 24);
        grid.set(1, 0, '#', DEFAULT_FG);
        let (lm, rm) = mask_ink_x_extent('#');
        let show = |offset: i32| -> Vec<u32> {
            let mut font = font12x24();
            let mut fb = FrameBuffer::new(3 * 12, 24);
            fb.clear_black();
            blit_grid(&grid, &mut font, &mut fb, offset);
            inked_columns(&fb)
        };
        let base = show(0);
        assert!(!base.is_empty(), "'#' must be visible at offset 0");
        assert_eq!(base[0], 12 + lm);
        assert_eq!(*base.last().unwrap(), 12 + rm);
        for k in [1i32, 3, 7, 11] {
            let fwd: Vec<u32> = base.iter().map(|x| x + k as u32).collect();
            assert_eq!(show(k), fwd, "offset +{k}");
            let back: Vec<u32> = base.iter().map(|x| x - k as u32).collect();
            assert_eq!(show(-k), back, "offset -{k}");
        }
        assert!(
            show(-(12 + rm as i32) - 1).is_empty(),
            "cell fully left of the frame must vanish"
        );
        assert_eq!(
            show(-(12 + rm as i32)),
            vec![0],
            "rightmost ink pixel clips to column 0"
        );
        assert!(
            show(24 - lm as i32).is_empty(),
            "cell fully right of the frame must vanish"
        );
        assert_eq!(
            show(23 - lm as i32),
            vec![35],
            "leftmost ink pixel clips to the last column"
        );
    }

    fn mask_ink_x_extent(ch: char) -> (u32, u32) {
        let mut font = font12x24();
        let mask = font.mask(ch).expect("glyph must rasterize");
        let w = mask.w as usize;
        let mut min = w as u32;
        let mut max = 0u32;
        for (i, &v) in mask.coverage.iter().enumerate() {
            if v > 0 {
                min = min.min((i % w) as u32);
                max = max.max((i % w) as u32);
            }
        }
        (min, max)
    }

    /// True when any pixel of the framebuffer differs from opaque black.
    /// Split from the perf-gate assertion so the test reads flat.
    fn framebuffer_has_ink(fb: &FrameBuffer) -> bool {
        fb.data
            .chunks_exact(4)
            .any(|px| px[0] != 0 || px[1] != 0 || px[2] != 0)
    }

    /// Every non-background pixel of the framebuffer in scan order. Split
    /// from the brightness-floor test so no loop nests a loop.
    fn collect_ink(fb: &FrameBuffer) -> Vec<[u8; 4]> {
        let mut hits = Vec::new();
        for y in 0..fb.height {
            collect_scanline_ink(fb, y, &mut hits);
        }
        hits
    }

    /// Append the inked pixels of framebuffer row `y` to `hits`.
    fn collect_scanline_ink(fb: &FrameBuffer, y: u32, hits: &mut Vec<[u8; 4]>) {
        for x in 0..fb.width {
            let p = fb.pixel(x, y);
            if p != [0, 0, 0, 255] {
                hits.push(p);
            }
        }
    }

    /// Fill one row of the perf-gate grid with the repeating art pattern,
    /// skipping every third diagonal cell. Split from the perf test so no
    /// loop nests a loop (audit site #20).
    fn fill_perf_row(grid: &mut Grid, art: &[char; 9], r: i32) {
        for c in 0..160 {
            if (c + r) % 3 != 0 {
                grid.set(
                    c as i64,
                    r as i64,
                    art[(c + r) as usize % art.len()],
                    [200, 200, 200],
                );
            }
        }
    }

    /// The F-165 counters count what the blit actually blitted: rows
    /// walked, rows carrying ink, cells blitted (decision F-165, "measure
    /// first").
    #[test]
    fn the_counters_count_rows_and_cells() {
        let mut grid = Grid::new(4, 3, 12, 24);
        // Row 0: two inked cells. Row 1: one. Row 2: empty — walked, not
        // inked.
        grid.set(0, 0, 'X', [255, 255, 255]);
        grid.set(1, 0, 'X', [255, 255, 255]);
        grid.set(2, 1, 'X', [255, 255, 255]);
        let mut font = font12x24();
        let _ = font.mask('X');
        let mut fb = FrameBuffer::new(4 * 12, 3 * 24);
        fb.clear_black();
        let tally = blit_grid(&grid, &mut font, &mut fb, 0);
        assert_eq!(
            tally,
            BlitTally {
                rows_walked: 3,
                rows_inked: 2,
                cells: 3,
            },
            "every row is walked, only inked rows count as inked"
        );
    }

    #[test]
    fn full_hd_grid_blits_within_budget() {
        let mut grid = Grid::new(160, 45, 12, 24);
        let art = ['#', '@', '-', '|', '/', '\\', 'o', '+', 'x'];
        for r in 0..45 {
            fill_perf_row(&mut grid, &art, r);
        }
        let mut font = font12x24();
        for ch in art {
            let _ = font.mask(ch);
        }
        let mut fb = FrameBuffer::new(160 * 12, 45 * 24);
        fb.clear_black();
        let t = std::time::Instant::now();
        blit_grid(&grid, &mut font, &mut fb, 4);
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        assert!(ms < 250.0, "full blit took {ms:.1}ms");
        assert!(framebuffer_has_ink(&fb), "blit must produce ink");
    }

    #[test]
    fn tiny_cells_get_full_brightness_floor() {
        let color = [200, 160, 40];
        let render = |cw: u32, ch: u32| -> Vec<[u8; 4]> {
            let mut grid = Grid::new(1, 1, cw, ch);
            grid.set(0, 0, 'X', color);
            let mut font = Font::new(cw, ch).expect("font");
            let mut fb = FrameBuffer::new(cw, ch);
            fb.clear_black();
            blit_grid(&grid, &mut font, &mut fb, 0);
            collect_ink(&fb)
        };
        // At the audit cell [2,3] the floor applies: every inked pixel is
        // the pure glyph color.
        let tiny = render(2, 3);
        assert!(!tiny.is_empty(), "X must ink at 2x3");
        assert!(
            tiny.iter()
                .all(|p| *p == [color[0], color[1], color[2], 255]),
            "2x3 cells must render at full brightness, got {tiny:?}"
        );
        // The 4K row-budget cell [4,6] gets the floor too.
        let four_k = render(4, 6);
        assert!(!four_k.is_empty(), "X must ink at 4x6");
        assert!(
            four_k
                .iter()
                .all(|p| *p == [color[0], color[1], color[2], 255]),
            "4x6 cells must render at full brightness, got {four_k:?}"
        );
        // At a normal cell the glyph keeps its anti-aliased partial coverage.
        let big = render(12, 24);
        assert!(!big.is_empty(), "X must ink at 12x24");
        assert!(
            big.iter()
                .any(|p| *p != [color[0], color[1], color[2], 255]),
            "12x24 cells must keep partial coverage"
        );
    }

    #[test]
    fn parade_offset_leaves_static_rows_pixel_stable() {
        let color = [255, 255, 255];
        let mut grid = Grid::new(6, 6, 4, 4);
        grid.set(1, 1, '#', color); // parade row (rows 0..3)
        grid.set(1, 4, '#', color); // static row (rows 3..6)
        let render = |offset: i32| -> FrameBuffer {
            let mut f = Font::new(4, 4).expect("font 4x4");
            let mut fb = FrameBuffer::new(6 * 4, 6 * 4);
            fb.clear_black();
            blit_grid_rows_between(&grid, &mut f, &mut fb, offset, 0..3);
            blit_grid_rows_between(&grid, &mut f, &mut fb, 0, 3..6);
            fb
        };
        let a = render(0);
        let b = render(3);
        // Static region: byte-identical regardless of the parade offset.
        let static_a = &a.data[(3 * 4 * a.width as usize) * 4..];
        let static_b = &b.data[(3 * 4 * b.width as usize) * 4..];
        assert_eq!(static_a, static_b, "static rows must not move");
        // Parade region: the offset shifts the ink.
        assert_ne!(
            &a.data[..(3 * 4 * a.width as usize) * 4],
            &b.data[..(3 * 4 * b.width as usize) * 4],
            "parade rows must follow the offset"
        );
    }

    /// A probe grid with ink in the parade rows and in both chrome bands:
    /// the counter at `rows - 12` and the hint at `rows - 6`
    /// (audit 2026-10-23 F-022).
    fn chrome_probe_grid(rows: usize) -> Grid {
        let mut grid = Grid::new(30, rows, 4, 4);
        grid.set(2, 2, '#', [255, 255, 255]);
        grid.set(5, rows as i64 - 12, '#', [255, 255, 255]);
        grid.set(5, rows as i64 - 6, '#', [255, 255, 255]);
        grid
    }

    /// One `blit_scene` pass over the probe grid.
    fn chrome_render(grid: &Grid, rows: usize, offset: i32) -> FrameBuffer {
        let mut font = Font::new(4, 4).expect("font 4x4");
        let mut fb = FrameBuffer::new(30 * 4, (rows * 4) as u32);
        blit_scene(
            &mut fb,
            &mut font,
            grid,
            offset,
            rows * 4 / 5,
            None,
            &[],
            None,
        );
        fb
    }

    /// The chrome band (from `rows - BOTTOM_CHROME_ROWS` down) must be
    /// byte-identical whatever the parade offset is. Split from the test
    /// so no loop nests a loop (audit nesting rule).
    fn assert_chrome_band_stable(a: &FrameBuffer, b: &FrameBuffer, rows: usize) {
        let from = (rows - BOTTOM_CHROME_ROWS) * 4 * a.width as usize * 4;
        assert_eq!(
            &a.data[from..],
            &b.data[from..],
            "rows {rows}: the counter and hint bands must not scroll"
        );
    }

    #[test]
    fn chrome_rows_above_the_grass_do_not_scroll_on_short_grids() {
        // F-022: the counter (rows-12) and the hint (rows-6) are chrome;
        // on a short grid both sit ABOVE the grass row and used to take
        // the parade's offset, smearing with every refresh. The native
        // grid (>= 360 rows) never hits this; the web canvas is a window
        // and does.
        for rows in [24usize, 50] {
            let ground = rows * 4 / 5;
            assert!(
                ground > rows - BOTTOM_CHROME_ROWS,
                "rows {rows}: the pin is vacuous unless the chrome band reaches above the grass"
            );
            let grid = chrome_probe_grid(rows);
            let still = chrome_render(&grid, rows, 0);
            let moved = chrome_render(&grid, rows, 5);
            assert_chrome_band_stable(&still, &moved, rows);
            // The parade rows still follow the offset: the probe cell at
            // cell row 2 covers pixel rows 8..12 (cell height 4), and
            // that band must move once the offset moves it.
            let stride = still.width as usize * 4;
            assert_ne!(
                &still.data[8 * stride..12 * stride],
                &moved.data[8 * stride..12 * stride],
                "rows {rows}: the parade rows must keep scrolling"
            );
        }
    }

    #[test]
    fn save_png_round_trips_dimensions() {
        let mut grid = Grid::new(8, 4, 12, 24);
        grid.put_str(1, 1, "Hi!", [255, 200, 100]);
        let mut font = font12x24();
        let mut fb = FrameBuffer::new(8 * 12, 4 * 24);
        fb.clear_black();
        blit_grid(&grid, &mut font, &mut fb, 0);
        let path = std::env::temp_dir().join("agnosis-blit-roundtrip.png");
        save_png(&fb, &path).expect("png save");
        let img = image::open(&path).expect("png reload").to_rgba8();
        assert_eq!(img.width(), fb.width);
        assert_eq!(img.height(), fb.height);
        assert_eq!(img.as_raw(), &fb.data);
        let _ = std::fs::remove_file(&path);
    }

    fn uniform_row_heights(text: &str) -> Vec<usize> {
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        let mut grid = Grid::new(n + 2, 2, 12, 24);
        grid.put_str(1, 1, text, [255, 255, 255]);
        let mut font = font12x24();
        let mut fb = FrameBuffer::new((n as u32 + 2) * 12, 2 * 24);
        fb.clear_black();
        blit_grid_rows(&grid, &mut font, &mut fb, 0, &[1]);
        let y0 = 24;
        chars
            .iter()
            .enumerate()
            .map(|(i, _)| cell_ink_rows(&fb, (1 + i) as u32 * 12, y0))
            .collect()
    }

    /// Count of inked pixel rows inside one 12-wide cell column starting at
    /// `(x0, y0)`. Split from `uniform_row_heights` so no loop nests a loop.
    fn cell_ink_rows(fb: &FrameBuffer, x0: u32, y0: u32) -> usize {
        (0..24)
            .filter(|&y| cell_row_has_ink(fb, x0, y0 + y as u32))
            .count()
    }

    /// True when any of the 12 pixels at row `y` starting at column `x0`
    /// carries RGB ink.
    fn cell_row_has_ink(fb: &FrameBuffer, x0: u32, y: u32) -> bool {
        (0..12).any(|x| {
            let p = fb.pixel(x0 + x as u32, y);
            p[0] != 0 || p[1] != 0 || p[2] != 0
        })
    }

    #[test]
    fn hud_uniform_row_keeps_one_scale() {
        let text = "press ESC to exit";
        let heights = uniform_row_heights(text);
        let chars: Vec<char> = text.chars().collect();
        assert_eq!(heights.len(), chars.len());
        let ink: Vec<usize> = heights
            .iter()
            .enumerate()
            .filter(|(i, _)| chars[*i] != ' ')
            .map(|(_, &h)| h)
            .collect();
        let mut sorted = ink.clone();
        sorted.sort_unstable();
        let median = sorted[sorted.len() / 2] as i32;
        for h in &ink {
            assert!(
                (*h as i32 - median).abs() <= 3,
                "HUD cell ink height {h} strays from median {median} ({ink:?})"
            );
        }
        let lower_mean = ["p", "r", "e", "s", "s", "t", "o", "e", "x", "i", "t"]
            .iter()
            .map(|s| {
                let ch = s.chars().next().unwrap();
                let idx = chars.iter().position(|&c| c == ch).unwrap();
                ink[chars
                    .iter()
                    .enumerate()
                    .filter(|(_, &c)| c != ' ')
                    .position(|(i, _)| i == idx)
                    .unwrap()] as f64
            })
            .sum::<f64>()
            / 11.0;
        let upper_mean = (ink[5] + ink[6] + ink[7]) as f64 / 3.0;
        assert!(
            (lower_mean - upper_mean).abs() <= 3.0,
            "lower mean {lower_mean} vs upper mean {upper_mean} ({ink:?})"
        );
        assert!(ink.iter().any(|&h| h > 0), "HUD must leave ink");
    }

    #[test]
    fn uniform_rows_leave_line_art_on_fitted_path() {
        let mut grid = Grid::new(4, 2, 12, 24);
        grid.set(1, 1, '-', [255, 255, 255]);
        let mut fitted = font12x24();
        let m_fit = fitted.mask('-').expect("dash");
        let mut uni = font12x24();
        let mut fb = FrameBuffer::new(4 * 12, 2 * 24);
        fb.clear_black();
        blit_grid_rows(&grid, &mut uni, &mut fb, 0, &[1]);
        let (min_x, max_x) = blitted_ink_x_extent(&fb, 12, 24);
        let (fmin, fmax) = mask_ink_x_extent_mask(&m_fit);
        assert_eq!((min_x as usize, max_x as usize), (fmin, fmax));
    }

    /// Horizontal ink extent of the 12-wide cell at framebuffer column `x0`,
    /// rows `y0..y0+24`. Split from the line-art test so no loop nests a loop.
    fn blitted_ink_x_extent(fb: &FrameBuffer, x0: u32, y0: u32) -> (u32, u32) {
        let mut min_x = 12u32;
        let mut max_x = 0u32;
        for y in 0..24 {
            update_ink_x_extent(fb, x0, y0 + y as u32, &mut min_x, &mut max_x);
        }
        (min_x, max_x)
    }

    /// Fold one framebuffer row into the running `[min_x, max_x]` extent.
    fn update_ink_x_extent(fb: &FrameBuffer, x0: u32, y: u32, min_x: &mut u32, max_x: &mut u32) {
        for x in 0..12 {
            let p = fb.pixel(x0 + x as u32, y);
            if p[0] == 0 && p[1] == 0 && p[2] == 0 {
                continue;
            }
            *min_x = (*min_x).min(x as u32);
            *max_x = (*max_x).max(x as u32);
        }
    }

    /// Horizontal ink extent of a glyph mask in mask-local columns.
    fn mask_ink_x_extent_mask(m: &crate::font::GlyphMask) -> (usize, usize) {
        let mut fmin = 12usize;
        let mut fmax = 0usize;
        for (i, &v) in m.coverage.iter().enumerate() {
            if v > 0 {
                fmin = fmin.min(i % 12);
                fmax = fmax.max(i % 12);
            }
        }
        (fmin, fmax)
    }
}
