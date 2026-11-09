//! The cell-level run-length codec (roadmap Extension 4 §2.2).
//!
//! The exported frames are **ASCII grids**, not pictures, so the codec
//! works on cells: every cell is a palette index, and the palette is a
//! list of `(glyph, colour)` pairs.
//!
//! THE COMPRESSION IDEA — every frame is a **diff**:
//!
//! * the base of a row is either the **previous frame's** row or the
//!   **current frame's row above**;
//! * the encoder picks whichever base gives fewer runs;
//! * the sky (a colour per grid row) is carried only when it changes;
//! * the scroll offset is a few bits.
//!
//! The two bases together are what make the blob small. The previous
//! frame's row is the cheap base while the duck only *moves*; the row
//! above is the cheap base when the duck changes its *pose* (the gait
//! frame changes twice a second) or when the content above it is
//! identical. A flat 256-colour palette would have to re-send the whole
//! picture every frame; this sends the outline of what moved.
//!
//! The format is **little-endian and self-describing**, and the page
//! decodes it with about sixty lines of JavaScript.

// The decoder half of this module is the **test** twin of the page's
// JavaScript decoder. It is public, so in the library crate it is never
// dead code — and the module-level `#[allow(dead_code)]` this file used
// to carry could only mask a real regression (audit 2026-10-21 §F9).

use std::collections::HashMap;

use crate::font::Font;
use crate::grid::Grid;

/// The magic bytes at the head of a blob.
pub const MAGIC: [u8; 4] = *b"DKSF";

/// The blob format version this build writes.
///
/// Version 2 added the **message band**: the `[message]` caption the app
/// and the GIF draw was missing from the page (audit 2026-10-23 F-140).
/// The band travels as one static block between the header and the
/// palette, so a version-1 page refuses a version-2 blob with the
/// version error instead of drawing a parade without its caption.
pub const VERSION: u8 = 2;

/// The largest palette a blob may carry.
///
/// The palette index travels in one byte, so 256 entries is the ceiling.
/// The first entry is reserved for the **space**, which is transparent.
pub const MAX_PALETTE: usize = 256;

/// One palette entry: the glyph a cell draws and the colour it draws it in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteEntry {
    /// The glyph the cell shows.
    pub glyph: char,
    /// The colour the glyph is inked in.
    pub color: [u8; 3],
}

/// One glyph's coverage bitmap, at the export's cell size.
///
/// The page draws the bitmap with the coverage as the alpha, which is
/// the same blend the CPU blitter does — so the page shows the app's
/// picture, not a lookalike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GlyphBitmap {
    /// The glyph the bitmap belongs to.
    pub glyph: char,
    /// The bitmap's width in pixels.
    pub width: u16,
    /// The bitmap's height in pixels.
    pub height: u16,
    /// The coverage per pixel, `width * height` bytes.
    pub coverage: Vec<u8>,
}

/// One backdrop layer in a blob: the tile, the band placement and the
/// scroll rate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobLayer {
    /// The tile's width in cells.
    pub cols: usize,
    /// The tile's height in cells.
    pub rows: usize,
    /// The grid row the band starts at.
    pub row_offset: usize,
    /// How many of the tile's rows are drawn.
    pub drawn_rows: usize,
    /// The scroll rate, in thousandths.
    ///
    /// The wire format carries no floats: a rate of 0.25 is 250.
    pub rate_milli: u16,
    /// The palette index of every tile cell, row-major.
    pub cells: Vec<u16>,
}

/// The message band in a blob: the `[message]` caption, baked as static
/// art (audit 2026-10-23 F-140).
///
/// The cells are **cropped to the ink**: a centred caption on a 960-cell
/// grid would otherwise carry a row of empty cells on every frame.
/// `row` and `col` are the crop's top-left corner **in grid cells**, and
/// the page paints the block once, at offset 0 — the message is chrome,
/// and chrome must not smear.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMessage {
    /// The grid row the band's top row sits at.
    pub row: usize,
    /// The grid column the crop's first column sits at.
    pub col: usize,
    /// The crop's width in cells.
    pub cols: usize,
    /// The crop's height in cells.
    pub rows: usize,
    /// The palette index of every cropped cell, row-major.
    pub cells: Vec<u16>,
}

/// A scroll rate as the wire format carries it.
pub fn rate_to_milli(rate: f32) -> u16 {
    if !rate.is_finite() || rate <= 0.0 {
        return 0;
    }
    ((rate * 1000.0).round() as u16).min(1000)
}

/// Everything that can go wrong while reading or writing a blob.
#[derive(Debug, thiserror::Error)]
pub enum CodecError {
    /// The bytes are not a blob this build understands.
    #[error("the blob is not an agnosis frame blob: {0}")]
    NotABlob(String),
    /// The blob ends in the middle of something.
    #[error("the blob is truncated")]
    Truncated,
    /// A field carries a value this build cannot use.
    #[error("the blob is malformed: {0}")]
    Malformed(String),
}

/// The row's base is the previous frame's row.
const MODE_ACROSS: u8 = 0;

/// The row's base is the row **above**, in the same frame.
///
/// The duck is an **outline**: its body is a contour of `X`s, and the
/// contour drifts sideways as it goes down, so a row and the row above
/// it share most of their ink.
const MODE_ABOVE: u8 = 1;

/// The row's base is the previous frame's row, **shifted**.
///
/// The parade marches: the ducks keep their shape and slide across the
/// screen, so a row of the new frame is the same row of the previous
/// frame, moved sideways. A shifted base turns a whole row of moved art
/// into a handful of runs — the legs, whose pose the gait changes.
///
/// It is the single biggest win in the codec: the duck band is a third
/// of the picture, and without the shift every one of its rows would
/// cost a run per inked cell.
const MODE_SHIFT_ACROSS: u8 = 2;

/// The row's base is the row above, shifted.
const MODE_SHIFT_ABOVE: u8 = 3;

/// The row did not change: it needs no runs at all.
///
/// The flag is what keeps an unchanged row at **one byte**: the static
/// rows (the sky, the grass, the HUD) are the same in every frame, and
/// 360 rows of nothing would otherwise cost 360 bytes a frame.
const MODE_UNCHANGED: u8 = 4;

/// The largest shift the encoder looks for, in cells.
///
/// The parade moves `cols / loop_seconds / fps` cells a frame, which is
/// four cells at the export's 12 fps on a 960-cell screen.
const MAX_SHIFT: i32 = 8;

/// The key a `(glyph, colour)` pair is looked up by.
fn palette_key(glyph: char, color: [u8; 3]) -> u32 {
    let code = glyph as u32;
    (code << 24) | ((color[0] as u32) << 16) | ((color[1] as u32) << 8) | color[2] as u32
}

/// The streaming encoder: it turns the scene's grids into a blob.
///
/// The encoder is **streaming** on purpose: a 20 s loop at 12 fps is 240
/// frames of 345 600 cells, and holding them all as `(glyph, colour)`
/// pairs would cost hundreds of megabytes. The frames are diffed and
/// encoded one at a time, and only the palette and the two working grids
/// are kept.
pub struct Encoder {
    /// The grid's width in cells.
    cols: usize,
    /// The grid's height in cells.
    rows: usize,
    /// The cell's width in pixels.
    cell_w: u32,
    /// The cell's height in pixels.
    cell_h: u32,
    /// The row the grass starts at: the rows above it scroll.
    ground_row: usize,
    /// The frames per second the recording is meant for.
    fps: u32,
    /// The palette. Index 0 is the space.
    palette: Vec<PaletteEntry>,
    /// The palette lookup.
    lookup: HashMap<u32, u16>,
    /// The previous frame's cell indices.
    previous: Vec<u16>,
    /// The frame being encoded.
    current: Vec<u16>,
    /// The encoded frames.
    frames: Vec<u8>,
    /// The sky ramp of the last frame.
    sky: Vec<[u8; 3]>,
    /// Whether the sky is on at all.
    has_sky: bool,
    /// The backdrop layers, encoded once.
    layers: Vec<BlobLayer>,
    /// The message band, encoded once (`None` when there is no message).
    message: Option<BlobMessage>,
    /// The shifts that worked lately, tried first.
    shift_hints: Vec<i32>,
    /// The scratch list of shift candidates.
    candidates: Vec<i32>,
    /// The winning shifted base's runs.
    shifted_best: Vec<(u32, u32, u16)>,
    /// How many frames were pushed (the full count; the blob field is a
    /// `u16` and [`Encoder::finish`] refuses an overflow).
    frame_count: u32,
}

impl Encoder {
    /// A new encoder for a grid of the given shape.
    pub fn new(
        cols: usize,
        rows: usize,
        cell_w: u32,
        cell_h: u32,
        ground_row: usize,
        fps: u32,
    ) -> Self {
        let cells = cols.max(1) * rows.max(1);
        let palette = vec![PaletteEntry {
            glyph: ' ',
            color: [0, 0, 0],
        }];
        Self {
            cols: cols.max(1),
            rows: rows.max(1),
            cell_w,
            cell_h,
            ground_row,
            fps,
            palette,
            lookup: HashMap::new(),
            previous: vec![0; cells],
            current: vec![0; cells],
            frames: Vec::new(),
            sky: Vec::new(),
            has_sky: false,
            frame_count: 0,
            layers: Vec::new(),
            message: None,
            shift_hints: Vec::new(),
            candidates: Vec::new(),
            shifted_best: Vec::new(),
        }
    }

    /// The palette as it stands.
    pub fn palette(&self) -> &[PaletteEntry] {
        &self.palette
    }

    /// Set the backdrop layers.
    ///
    /// The tiles are **static art**: they are encoded once, here, and
    /// every frame only carries the layer's scroll offset.
    pub fn set_layers(&mut self, layers: &[crate::layer::BackdropInfo<'_>]) {
        let mut encoded = Vec::with_capacity(layers.len());
        for layer in layers {
            encoded.push(self.encode_tile(layer));
        }
        self.layers = encoded;
    }

    /// Encode one layer's tile: its cells, against an empty base.
    ///
    /// WHY the helper: the nesting rule is two levels at most, and the
    /// tile scan was a loop in a loop (audit 2026-10-21 §F8).
    fn encode_tile(&mut self, layer: &crate::layer::BackdropInfo<'_>) -> BlobLayer {
        let cols = layer.tile.cols().max(1);
        let rows = layer.tile.rows().max(1);
        let mut cells = vec![0u16; cols * rows];
        for (row, col, ch, color) in inked_tile_cells(layer.tile, cols, rows) {
            let index = intern_entry(&mut self.palette, &mut self.lookup, ch, color);
            cells[row * cols + col] = index;
        }
        BlobLayer {
            cols,
            rows,
            row_offset: layer.row_offset,
            drawn_rows: layer.rows,
            rate_milli: crate::export::codec::rate_to_milli(layer.rate),
            cells,
        }
    }

    /// Bake the message band into the blob, **once** (audit F-140).
    ///
    /// The band is static art like the backdrop tiles: the recorder
    /// captures the scene's `message_blit()` before the frames are pushed
    /// and every frame then shows the same caption at offset 0. The block
    /// is **cropped to the ink**, so a centred caption does not carry a
    /// row of empty cells.
    pub fn set_message(&mut self, band: &crate::layer::LayerBlit<'_>) {
        self.message = self.encode_the_message(band);
    }

    /// Encode one message band: its inked cells, cropped to the ink.
    ///
    /// WHY the helper: the nesting rule is two levels at most, and the
    /// crop is a loop in a loop.
    fn encode_the_message(&mut self, band: &crate::layer::LayerBlit<'_>) -> Option<BlobMessage> {
        let grid = band.grid;
        let crop = ink_bounds(grid)?;
        let cells = self.intern_the_cells(grid, crop);
        Some(BlobMessage {
            row: band.row_offset + crop.0,
            col: crop.1,
            cols: crop.3 - crop.1 + 1,
            rows: crop.2 - crop.0 + 1,
            cells,
        })
    }

    /// Intern one crop's inked cells into the palette.
    fn intern_the_cells(&mut self, grid: &Grid, crop: (usize, usize, usize, usize)) -> Vec<u16> {
        let (top, left, bottom, right) = crop;
        let cols = right - left + 1;
        let mut cells = vec![0u16; cols * (bottom - top + 1)];
        for row in top..=bottom {
            for col in left..=right {
                let Some(ch) = grid.char_at(col, row) else {
                    continue;
                };
                if ch == ' ' {
                    continue;
                }
                let Some(color) = grid.color_at(col, row) else {
                    continue;
                };
                let index = intern_entry(&mut self.palette, &mut self.lookup, ch, color);
                cells[(row - top) * cols + (col - left)] = index;
            }
        }
        cells
    }

    /// Push one frame: the scene's grid, the sky ramp, the scroll offset
    /// and the backdrop layers' scroll offsets.
    ///
    /// The frame is **validated** before it is encoded:
    ///
    /// - the grid must be the encoder's shape. A smaller grid would be
    ///   written as a partly-blank frame, a larger one would be cut
    ///   short — both silent corruption (audit 2026-10-22 §P2-25).
    /// - the sky ramp must have exactly one colour per row, because the
    ///   decoder reads exactly that many. A ramp of the wrong length
    ///   desynchronises the decoder, which then reads the layer offsets
    ///   as sky colours (audit 2026-10-22 §P1-4).
    ///
    /// A refused frame is an **error**, not a silent drop: the export
    /// fails loudly instead of writing a corrupt blob.
    pub fn push_frame(
        &mut self,
        grid: &Grid,
        sky: Option<&[[u8; 3]]>,
        offset_px: i32,
        layer_offsets: &[i32],
    ) -> Result<(), CodecError> {
        self.check_the_frame(grid, sky)?;
        self.fill_the_frame(grid);
        self.encode_the_frame(sky, offset_px, layer_offsets);
        self.previous.copy_from_slice(&self.current);
        self.frame_count += 1;
        Ok(())
    }

    /// Refuse a frame the encoder cannot represent.
    ///
    /// The sky rule (audit 2026-10-23 F-139): the wire format carries the
    /// sky as "same as before" or a full ramp — there is no "the sky went
    /// away". A `None` after a `Some` would be written as *same*, and the
    /// page would keep painting the old ramp over a skyless grid, so the
    /// encoder refuses the desync instead.
    fn check_the_frame(&self, grid: &Grid, sky: Option<&[[u8; 3]]>) -> Result<(), CodecError> {
        if grid.cols() != self.cols || grid.rows() != self.rows {
            return Err(CodecError::Malformed(format!(
                "the frame is {}x{}, the blob is {}x{}",
                grid.cols(),
                grid.rows(),
                self.cols,
                self.rows
            )));
        }
        let wrong_ramp = sky.is_some_and(|ramp| ramp.len() != self.rows);
        if wrong_ramp {
            let len = sky.map(|ramp| ramp.len()).unwrap_or_default();
            return Err(CodecError::Malformed(format!(
                "the sky ramp has {len} colours, the blob has {} rows",
                self.rows
            )));
        }
        if sky.is_none() && self.has_sky {
            return Err(CodecError::Malformed(
                "the sky cannot be switched off mid-recording".into(),
            ));
        }
        Ok(())
    }

    /// Turn the grid's cells into palette indices.
    fn fill_the_frame(&mut self, grid: &Grid) {
        let mut current = std::mem::take(&mut self.current);
        current.fill(0);
        let cols = self.cols;
        let palette = &mut self.palette;
        let lookup = &mut self.lookup;
        let mut intern =
            |glyph: char, color: [u8; 3]| -> u16 { intern_entry(palette, lookup, glyph, color) };
        grid.for_each_cell(|col, row, ch, color| {
            if ch == ' ' {
                return;
            }
            let index = intern(ch, color);
            let at = row * cols + col;
            if at < current.len() {
                current[at] = index;
            }
        });
        self.current = current;
    }

    /// Encode the frame: the offset, the sky, the layers' offsets and the
    /// row diffs.
    fn encode_the_frame(&mut self, sky: Option<&[[u8; 3]]>, offset_px: i32, layer_offsets: &[i32]) {
        let mut head = Vec::with_capacity(8);
        put_varint(&mut head, zigzag(offset_px));
        let same_sky = self.sky_is(sky);
        head.push(u8::from(same_sky));
        if !same_sky {
            let ramp = sky.unwrap_or(&[]);
            for color in ramp {
                head.extend_from_slice(color);
            }
            self.sky = ramp.to_vec();
            self.has_sky = sky.is_some();
        }
        for offset in layer_offsets {
            put_varint(&mut head, zigzag(*offset));
        }
        self.frames.extend_from_slice(&head);
        self.encode_the_rows();
    }

    /// True when the sky ramp is the same as the one already sent.
    fn sky_is(&self, sky: Option<&[[u8; 3]]>) -> bool {
        match sky {
            None => !self.has_sky && self.sky.is_empty(),
            Some(ramp) => self.has_sky && ramp.len() == self.sky.len() && ramp == &self.sky[..],
        }
    }

    /// Encode every row as a diff against its base.
    ///
    /// A row that did not change at all costs **one byte**: the static
    /// rows are the same in every frame, and there are a lot of them.
    /// A row that only **moved** costs a shifted base and the few runs
    /// the gait changed.
    fn encode_the_rows(&mut self) {
        let cols = self.cols;
        let mut across: Vec<(u32, u32, u16)> = Vec::new();
        let mut above: Vec<(u32, u32, u16)> = Vec::new();
        for row in 0..self.rows {
            let start = row * cols;
            diff_runs(
                &self.current,
                &self.previous,
                start,
                start,
                cols,
                &mut across,
            );
            if across.is_empty() {
                self.frames.push(MODE_UNCHANGED);
                continue;
            }
            let mut plan = RowPlan {
                mode: MODE_ACROSS,
                count: across.len(),
                shift: 0,
            };
            if row > 0 {
                above_plan(&mut plan, &self.current, start, cols, &mut above);
            }
            shift_plan(&mut plan, self.plan_the_shift(start, cols));
            self.write_the_row(&plan, &across, &above);
        }
    }

    /// Write one planned row: the mode byte, the shift, the run count
    /// and the winning plan's runs.
    fn write_the_row(
        &mut self,
        plan: &RowPlan,
        across: &[(u32, u32, u16)],
        above: &[(u32, u32, u16)],
    ) {
        self.frames.push(plan.mode);
        if plan.mode == MODE_SHIFT_ACROSS || plan.mode == MODE_SHIFT_ABOVE {
            put_varint(&mut self.frames, zigzag(plan.shift));
            // The hint is the field itself, so the borrow of the
            // winner's runs stays alive.
            self.shift_hints.retain(|hint| *hint != plan.shift);
            self.shift_hints.insert(0, plan.shift);
            self.shift_hints.truncate(4);
        }
        put_varint(&mut self.frames, plan.count as u32);
        let shifted = &self.shifted_best;
        match plan.mode {
            MODE_ABOVE => write_runs(&mut self.frames, above),
            MODE_SHIFT_ACROSS | MODE_SHIFT_ABOVE => write_runs(&mut self.frames, shifted),
            _ => write_runs(&mut self.frames, across),
        }
    }

    /// The best shifted base for a row.
    ///
    /// The method form keeps the planner's eight inputs in one place:
    /// the encoder's own buffers are what it works on.
    fn plan_the_shift(&mut self, current_start: usize, cols: usize) -> Option<(u8, i32, usize)> {
        let mut scratch = Vec::new();
        let mut best = Vec::new();
        let plan = {
            let mut work = ShiftWork {
                hints: &mut self.shift_hints,
                candidates: &mut self.candidates,
                scratch: &mut scratch,
                best: &mut best,
            };
            plan_the_shift(
                &self.current,
                &self.previous,
                current_start,
                cols,
                &mut work,
            )
        };
        self.shifted_best = best;
        plan
    }

    /// The finished blob: the header, the palette, the glyphs and the
    /// frames.
    ///
    /// The palette and the glyphs are only known once every frame is in,
    /// so they are written **after** the frames were encoded — the header
    /// comes first in the file and the frames last.
    pub fn finish(self, font: &mut Font) -> Result<Vec<u8>, CodecError> {
        self.check_the_counts()?;
        self.check_the_glyphs()?;
        let glyphs = self.glyph_bitmaps(font)?;
        let mut out = Vec::with_capacity(self.frames.len() + 4096);
        out.extend_from_slice(&MAGIC);
        out.push(VERSION);
        out.push(u8::from(self.has_sky));
        out.extend_from_slice(&(self.cols as u16).to_le_bytes());
        out.extend_from_slice(&(self.rows as u16).to_le_bytes());
        out.extend_from_slice(&(self.cell_w as u16).to_le_bytes());
        out.extend_from_slice(&(self.cell_h as u16).to_le_bytes());
        out.extend_from_slice(&(self.ground_row as u16).to_le_bytes());
        out.extend_from_slice(&(self.fps as u16).to_le_bytes());
        out.extend_from_slice(&(self.palette.len() as u16).to_le_bytes());
        out.extend_from_slice(&(self.frame_count as u16).to_le_bytes());
        out.extend_from_slice(&(glyphs.len() as u16).to_le_bytes());
        out.extend_from_slice(&(self.layers.len() as u16).to_le_bytes());
        write_the_message(&mut out, self.message.as_ref());
        for entry in &self.palette {
            out.push(glyph_byte(entry.glyph)?);
            out.extend_from_slice(&entry.color);
        }
        for bitmap in &glyphs {
            out.push(glyph_byte(bitmap.glyph)?);
            out.extend_from_slice(&bitmap.width.to_le_bytes());
            out.extend_from_slice(&bitmap.height.to_le_bytes());
            out.extend_from_slice(&bitmap.coverage);
        }
        for layer in &self.layers {
            out.extend_from_slice(&(layer.cols as u16).to_le_bytes());
            out.extend_from_slice(&(layer.rows as u16).to_le_bytes());
            out.extend_from_slice(&(layer.row_offset as u16).to_le_bytes());
            out.extend_from_slice(&(layer.drawn_rows as u16).to_le_bytes());
            out.extend_from_slice(&layer.rate_milli.to_le_bytes());
            for cell in &layer.cells {
                out.push(*cell as u8);
            }
        }
        out.extend_from_slice(&self.frames);
        Ok(out)
    }

    /// Every palette glyph must fit in the blob's **one-byte** glyph
    /// field.
    ///
    /// WHY: a glyph above U+00FF would be truncated on the way out and
    /// read back as a different character, so the page would draw the
    /// wrong glyph. The writer **refuses** it instead: the blob claims
    /// to be self-describing, and a silently mangled glyph is not
    /// (audit 2026-10-21 §F12).
    fn check_the_glyphs(&self) -> Result<(), CodecError> {
        for entry in &self.palette {
            glyph_byte(entry.glyph)?;
        }
        Ok(())
    }

    /// The counts the header writes as a `u16` must actually fit in one.
    ///
    /// WHY: `cols`, `rows` and the frame count were cast with `as u16`,
    /// which **wraps** — a 70 000-column grid would arrive as a 4 464
    /// column blob and every frame would be cut short. The writer
    /// refuses the truncation instead (audit 2026-10-23 F-151).
    fn check_the_counts(&self) -> Result<(), CodecError> {
        let limit = u16::MAX as usize;
        if self.cols > limit || self.rows > limit {
            return Err(CodecError::Malformed(format!(
                "the grid is {}x{}, over the blob's {limit} per side",
                self.cols, self.rows
            )));
        }
        if self.frame_count as usize > limit {
            return Err(CodecError::Malformed(format!(
                "{} frames is over the blob's {limit} frame field",
                self.frame_count
            )));
        }
        Ok(())
    }

    /// The coverage bitmap of every glyph the palette uses.
    fn glyph_bitmaps(&self, font: &mut Font) -> Result<Vec<GlyphBitmap>, CodecError> {
        let mut bitmaps = Vec::new();
        for entry in &self.palette {
            if entry.glyph == ' ' {
                continue;
            }
            let mask = font.mask(entry.glyph).ok_or_else(|| {
                CodecError::Malformed(format!("the glyph {:?} has no mask", entry.glyph))
            })?;
            bitmaps.push(GlyphBitmap {
                glyph: entry.glyph,
                width: mask.w as u16,
                height: mask.h as u16,
                coverage: mask.coverage.clone(),
            });
        }
        Ok(bitmaps)
    }
}

/// The tile's inked cells, in row-major order.
///
/// A cell with no ink (a space) is not in the list at all: the blob
/// stores a palette index per cell and the empty cell is the index
/// zero.
fn inked_tile_cells(
    tile: &crate::balloon::AsciiSprite,
    cols: usize,
    rows: usize,
) -> Vec<(usize, usize, char, [u8; 3])> {
    (0..rows)
        .flat_map(|row| (0..cols).filter_map(move |col| inked_tile_cell(tile, row, col)))
        .collect()
}

/// One inked cell of the tile, or `None` when it carries no ink.
fn inked_tile_cell(
    tile: &crate::balloon::AsciiSprite,
    row: usize,
    col: usize,
) -> Option<(usize, usize, char, [u8; 3])> {
    let (ch, color) = tile.get(col, row)?;
    if ch == ' ' {
        return None;
    }
    Some((row, col, ch, color))
}

/// The ink's bounding box of a grid: `(top, left, bottom, right)`, or
/// `None` when the grid carries no ink at all.
///
/// WHY a helper: the nesting rule is two levels at most, and the bounds
/// scan is a loop in a loop.
fn ink_bounds(grid: &Grid) -> Option<(usize, usize, usize, usize)> {
    let mut bounds: Option<(usize, usize, usize, usize)> = None;
    grid.for_each_cell(|col, row, ch, _color| {
        if ch == ' ' {
            return;
        }
        bounds = Some(match bounds {
            None => (row, col, row, col),
            Some((top, left, bottom, right)) => {
                (top.min(row), left.min(col), bottom.max(row), right.max(col))
            }
        });
    });
    bounds
}

/// The byte a glyph is written as in the blob.
///
/// WHY: the blob's glyph field is **one byte**, so a glyph above
/// U+00FF would be truncated and read back as a different character.
/// The writer refuses it rather than corrupting the blob silently
/// (audit 2026-10-21 §F12).
fn glyph_byte(glyph: char) -> Result<u8, CodecError> {
    if !glyph.is_ascii() {
        return Err(CodecError::Malformed(format!(
            "the glyph {glyph:?} is not ASCII and does not fit in the blob's one-byte glyph field"
        )));
    }
    Ok(glyph as u8)
}

/// Write the message block: a flag byte, then the band when it is there.
///
/// The flag sits **between** the header counts and the palette, so both
/// readers (this file and the page's JavaScript) skip one byte when no
/// message travels (audit 2026-10-23 F-140, blob version 2).
fn write_the_message(out: &mut Vec<u8>, message: Option<&BlobMessage>) {
    let Some(message) = message else {
        out.push(0);
        return;
    };
    out.push(1);
    out.extend_from_slice(&(message.row as u16).to_le_bytes());
    out.extend_from_slice(&(message.col as u16).to_le_bytes());
    out.extend_from_slice(&(message.cols as u16).to_le_bytes());
    out.extend_from_slice(&(message.rows as u16).to_le_bytes());
    for cell in &message.cells {
        out.push(*cell as u8);
    }
}

/// Read the message block [`write_the_message`] wrote.
fn take_message(bytes: &[u8], at: &mut usize) -> Result<Option<BlobMessage>, CodecError> {
    if take_u8(bytes, at)? == 0 {
        return Ok(None);
    }
    let row = take_u16(bytes, at)? as usize;
    let col = take_u16(bytes, at)? as usize;
    let cols = take_u16(bytes, at)? as usize;
    let rows = take_u16(bytes, at)? as usize;
    let count = cols.saturating_mul(rows);
    let end = at.saturating_add(count);
    let raw = bytes.get(*at..end).ok_or(CodecError::Truncated)?;
    let cells = raw.iter().map(|b| *b as u16).collect();
    *at = end;
    Ok(Some(BlobMessage {
        row,
        col,
        cols,
        rows,
        cells,
    }))
}

/// Look a `(glyph, colour)` pair up, adding it to the palette when it is
/// new.
///
/// When the palette is full the pair **reuses** the closest entry: the
/// colour that is nearest, preferring an entry that draws the same
/// glyph. The result is still a valid blob, and the choice is
/// deterministic.
fn intern_entry(
    palette: &mut Vec<PaletteEntry>,
    lookup: &mut HashMap<u32, u16>,
    glyph: char,
    color: [u8; 3],
) -> u16 {
    let key = palette_key(glyph, color);
    if let Some(index) = lookup.get(&key) {
        return *index;
    }
    if palette.len() < MAX_PALETTE {
        let index = palette.len() as u16;
        palette.push(PaletteEntry { glyph, color });
        lookup.insert(key, index);
        return index;
    }
    closest_entry(palette, glyph, color)
}

/// The palette entry that is closest to a `(glyph, colour)` pair.
///
/// An entry that draws the **same glyph** wins over a nearer colour:
/// the shape of a cell matters more than its tint, and the alternative
/// would draw a duck's body in the shape of a letter.
fn closest_entry(palette: &[PaletteEntry], glyph: char, color: [u8; 3]) -> u16 {
    let mut best = 0usize;
    let mut best_distance = u32::MAX;
    for (index, entry) in palette.iter().enumerate() {
        if entry.glyph == ' ' || entry.glyph != glyph {
            continue;
        }
        let distance = colour_distance(entry.color, color);
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    if best_distance != u32::MAX {
        return best as u16;
    }
    for (index, entry) in palette.iter().enumerate() {
        if entry.glyph == ' ' {
            continue;
        }
        let distance = colour_distance(entry.color, color);
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    best as u16
}

/// The squared distance between two colours.
fn colour_distance(a: [u8; 3], b: [u8; 3]) -> u32 {
    let dr = a[0] as i32 - b[0] as i32;
    let dg = a[1] as i32 - b[1] as i32;
    let db = a[2] as i32 - b[2] as i32;
    (dr * dr + dg * dg + db * db) as u32
}

/// The runs of a row that differ from a base row.
///
/// A run is a maximal stretch of cells that differ from the base **and**
/// carry the same value: a cell that matches the base splits the run,
/// because the decoder copies the base into the row before it writes the
/// runs.
///
/// The two rows start at their **own** offsets: the base is the previous
/// frame's row (the same index) or the row above (one row earlier).
fn diff_runs(
    current: &[u16],
    base: &[u16],
    current_start: usize,
    base_start: usize,
    len: usize,
    out: &mut Vec<(u32, u32, u16)>,
) {
    out.clear();
    let mut col = 0usize;
    while col < len {
        let here = current[current_start + col];
        if here == base[base_start + col] {
            col += 1;
            continue;
        }
        let run_start = col;
        col = run_end(current, current_start, col, len, here);
        out.push((run_start as u32, (col - run_start) as u32, here));
    }
}

/// The end of the run of `value` that starts at `col`.
///
/// WHY a helper: the nesting rule is two levels at most, and the run
/// scan was a loop in a loop (audit 2026-10-21 §F8).
fn run_end(current: &[u16], current_start: usize, col: usize, len: usize, value: u16) -> usize {
    let mut end = col + 1;
    while end < len && current[current_start + end] == value {
        end += 1;
    }
    end
}

/// The best base plan for one row: the mode, its run count and the shift
/// a shifted mode won with.
struct RowPlan {
    /// The row's mode byte.
    mode: u8,
    /// The winning base's run count.
    count: usize,
    /// The shift a shifted mode won with (0 otherwise).
    shift: i32,
}

/// Consider the row **above** as the base (audit 2026-10-23 F-141).
///
/// WHY the helper: the base choice was an if inside an if in the row
/// loop, which the nesting rule bans — the decision gets a name of its
/// own instead.
fn above_plan(
    plan: &mut RowPlan,
    current: &[u16],
    start: usize,
    cols: usize,
    out: &mut Vec<(u32, u32, u16)>,
) {
    diff_runs(current, current, start, start - cols, cols, out);
    if out.len() < plan.count {
        plan.mode = MODE_ABOVE;
        plan.count = out.len();
    }
}

/// Consider a **shifted** base (audit 2026-10-23 F-141).
///
/// The planner already ran; this only compares its verdict with the plan
/// the plain bases produced.
fn shift_plan(plan: &mut RowPlan, found: Option<(u8, i32, usize)>) {
    let Some((mode, shift, count)) = found else {
        return;
    };
    if count < plan.count {
        plan.mode = mode;
        plan.count = count;
        plan.shift = shift;
    }
}

/// The planner's scratch space.
///
/// One bundle, so the planner takes five arguments instead of eight.
struct ShiftWork<'a> {
    /// The shifts that worked lately.
    hints: &'a mut Vec<i32>,
    /// The candidates to try, in order.
    candidates: &'a mut Vec<i32>,
    /// The last candidate's runs.
    scratch: &'a mut Vec<(u32, u32, u16)>,
    /// The winner's runs.
    best: &'a mut Vec<(u32, u32, u16)>,
}

/// The best shift for a row, if a shifted base beats the plain one.
///
/// The shifts that worked lately are tried first: the parade moves the
/// same way in every frame, so the hint is almost always the answer
/// and the expensive search happens once per frame.
fn plan_the_shift(
    current: &[u16],
    previous: &[u16],
    current_start: usize,
    cols: usize,
    work: &mut ShiftWork,
) -> Option<(u8, i32, usize)> {
    let ShiftWork {
        hints,
        candidates,
        scratch,
        best,
    } = work;
    candidates.clear();
    for hint in hints.iter() {
        candidates.push(*hint);
    }
    for shift in -MAX_SHIFT..=MAX_SHIFT {
        if !candidates.contains(&shift) {
            candidates.push(shift);
        }
    }
    let mut winner: Option<(u8, i32, usize)> = None;
    for shift in candidates.iter() {
        // Two shifted bases: the previous frame's row (the ducks are
        // rigid in **time**) and the row above (the contour is rigid in
        // the **vertical** direction).
        let across_count = try_the_shift(
            previous,
            current_start,
            current,
            current_start,
            cols,
            *shift,
            scratch,
        );
        let above_count = if current_start >= cols {
            try_the_shift(
                current,
                current_start - cols,
                current,
                current_start,
                cols,
                *shift,
                scratch,
            )
        } else {
            usize::MAX
        };
        let (mode, count) = if across_count <= above_count {
            (MODE_SHIFT_ACROSS, across_count)
        } else {
            (MODE_SHIFT_ABOVE, above_count)
        };
        let better = match winner {
            Some((_, _, best_count)) => count < best_count,
            None => true,
        };
        if better {
            // The winner's runs are **recomputed and copied**: the
            // scratch buffer holds the last candidate tried, which is
            // not necessarily the winner.
            let base_start = match mode {
                MODE_SHIFT_ACROSS => current_start,
                _ => current_start.saturating_sub(cols),
            };
            let base = match mode {
                MODE_SHIFT_ACROSS => previous,
                _ => current,
            };
            try_the_shift(
                base,
                base_start,
                current,
                current_start,
                cols,
                *shift,
                scratch,
            );
            best.clear();
            best.extend_from_slice(scratch);
            winner = Some((mode, *shift, count));
        }
        if count == 0 {
            break;
        }
    }
    winner
}

/// The number of runs a row has against a **shifted** base.
fn try_the_shift(
    base: &[u16],
    base_start: usize,
    current: &[u16],
    current_start: usize,
    cols: usize,
    shift: i32,
    out: &mut Vec<(u32, u32, u16)>,
) -> usize {
    diff_runs_shifted(current, base, current_start, base_start, cols, shift, out);
    out.len()
}

/// The runs of a row that differ from a **shifted** base row.
///
/// The content moved left by `shift` cells, so the cell at column `col`
/// holds what the base held at `col + shift`; outside the base it is
/// the space.
fn diff_runs_shifted(
    current: &[u16],
    base: &[u16],
    current_start: usize,
    base_start: usize,
    len: usize,
    shift: i32,
    out: &mut Vec<(u32, u32, u16)>,
) {
    out.clear();
    let mut col = 0usize;
    while col < len {
        let here = current[current_start + col];
        if here == shifted_value(base, base_start, col, shift, len) {
            col += 1;
            continue;
        }
        let run_start = col;
        col = run_end(current, current_start, col, len, here);
        out.push((run_start as u32, (col - run_start) as u32, here));
    }
}

/// The value of the shifted base at column `col`.
fn shifted_value(base: &[u16], base_start: usize, col: usize, shift: i32, len: usize) -> u16 {
    let source = col as i64 + shift as i64;
    if source < 0 || source >= len as i64 {
        return 0;
    }
    base[base_start + source as usize]
}

/// Write the runs of one row into `out`.
///
/// The start is a **delta** from the previous run's end, so a row of
/// many short runs costs two bytes per run.
fn write_runs(out: &mut Vec<u8>, runs: &[(u32, u32, u16)]) {
    let mut previous_end = 0u32;
    for (start, len, value) in runs {
        put_varint(out, start.saturating_sub(previous_end));
        put_varint(out, *len);
        out.push(*value as u8);
        previous_end = start.saturating_add(*len);
    }
}

/// Write a number as a variable-length integer.
fn put_varint(out: &mut Vec<u8>, mut value: u32) {
    loop {
        let byte = (value & 0x7F) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

/// Read a variable-length integer.
fn get_varint(bytes: &[u8], at: &mut usize) -> Result<u32, CodecError> {
    let mut value = 0u32;
    let mut shift = 0u32;
    loop {
        let byte = *bytes.get(*at).ok_or(CodecError::Truncated)?;
        *at += 1;
        value |= ((byte & 0x7F) as u32) << shift;
        if byte & 0x80 == 0 {
            return Ok(value);
        }
        shift += 7;
        if shift > 28 {
            return Err(CodecError::Malformed("a varint is too long".into()));
        }
    }
}

/// Turn a signed number into one that grows with its magnitude.
fn zigzag(value: i32) -> u32 {
    ((value << 1) ^ (value >> 31)) as u32
}

/// Turn a zigzagged number back into a signed one.
fn unzigzag(value: u32) -> i32 {
    ((value >> 1) as i32) ^ -((value & 1) as i32)
}

/// A decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    /// The palette index of every cell, row-major.
    pub cells: Vec<u16>,
    /// The sky colour of every grid row.
    pub sky: Vec<[u8; 3]>,
    /// The scroll offset in pixels.
    pub offset_px: i32,
    /// The backdrop layers' scroll offsets, in pixels.
    pub layer_offsets: Vec<i32>,
}

/// A decoded blob.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedBlob {
    /// The grid's width in cells.
    pub cols: usize,
    /// The grid's height in cells.
    pub rows: usize,
    /// The cell's width in pixels.
    pub cell_w: u32,
    /// The cell's height in pixels.
    pub cell_h: u32,
    /// The row the grass starts at.
    pub ground_row: usize,
    /// The frames per second.
    pub fps: u32,
    /// The palette.
    pub palette: Vec<PaletteEntry>,
    /// The glyph bitmaps.
    pub glyphs: Vec<GlyphBitmap>,
    /// The backdrop layers.
    pub layers: Vec<BlobLayer>,
    /// The message band, baked as static art (`None` when the recording
    /// has no message).
    pub message: Option<BlobMessage>,
    /// The frames.
    pub frames: Vec<DecodedFrame>,
}

/// The ceiling on the cells of **one** frame.
///
/// WHY a constant and not the blob's size: the cells are decoded from
/// **diffs**, so a 17 kB blob decodes into 345 600 cells per frame. The
/// ceiling is generous enough for any grid the app draws (the row-budget
/// grid is about 960 x 360) and small enough that a hostile blob cannot
/// ask for gigabytes (audit 2026-10-21 §F13).
const MAX_CELLS_PER_FRAME: usize = 4 * 1024 * 1024;

/// The ceiling on the cells of a whole blob.
///
/// A real export is 240 frames of a 960 x 360 grid — 83 M cells — so the
/// ceiling is well above it, and far below the 17 GB a hostile
/// `65535 x 65535` blob would ask for.
const MAX_DECODED_CELLS: usize = 128 * 1024 * 1024;

/// Decode a blob back into frames.
///
/// This is the Rust twin of the page's decoder: the two must agree, and
/// this one is what the tests check the blob against.
pub fn decode(bytes: &[u8]) -> Result<DecodedBlob, CodecError> {
    let mut at = 0usize;
    if bytes.len() < MAGIC.len() || bytes[..MAGIC.len()] != MAGIC {
        return Err(CodecError::NotABlob("the magic bytes are missing".into()));
    }
    at += MAGIC.len();
    let version = take_u8(bytes, &mut at)?;
    if version != VERSION {
        return Err(CodecError::NotABlob(format!(
            "the blob is version {version}, this build speaks {VERSION}"
        )));
    }
    let has_sky = take_u8(bytes, &mut at)? != 0;
    let cols = take_u16(bytes, &mut at)? as usize;
    let rows = take_u16(bytes, &mut at)? as usize;
    let cell_w = take_u16(bytes, &mut at)? as u32;
    let cell_h = take_u16(bytes, &mut at)? as u32;
    let ground_row = take_u16(bytes, &mut at)? as usize;
    let fps = take_u16(bytes, &mut at)? as u32;
    let palette_len = take_u16(bytes, &mut at)? as usize;
    let frame_count = take_u16(bytes, &mut at)? as usize;
    let glyph_count = take_u16(bytes, &mut at)? as usize;
    let layer_count = take_u16(bytes, &mut at)? as usize;
    check_the_geometry(cols, rows, frame_count, bytes.len())?;
    let message = take_message(bytes, &mut at)?;
    let palette = take_palette(bytes, &mut at, palette_len)?;
    let glyphs = take_glyphs(bytes, &mut at, glyph_count)?;
    let layers = take_layers(bytes, &mut at, layer_count)?;
    let mut decoder = FrameDecoder {
        cols,
        rows,
        layer_count,
        previous: vec![0; cols * rows],
        current: vec![0; cols * rows],
        sky: Vec::new(),
        has_sky,
    };
    let mut frames = Vec::with_capacity(frame_count);
    for _ in 0..frame_count {
        frames.push(decoder.take_frame(bytes, &mut at)?);
    }
    Ok(DecodedBlob {
        cols,
        rows,
        cell_w,
        cell_h,
        ground_row,
        fps,
        palette,
        glyphs,
        layers,
        message,
        frames,
    })
}

/// The geometry a blob may claim.
///
/// WHY this exists: `cols` and `rows` are 16-bit numbers taken from the
/// blob, and a hostile blob could claim 65535 x 65535 — an allocation
/// of about 17 GB (audit 2026-10-21 §F13).
///
/// The bounds are **three**, and each of them is a property of the
/// format:
///
/// * the **cells of one frame** are bounded by a constant — the cells
///   are decoded from diffs, so the blob's size does not bound them
///   (a 17 kB blob decodes into 345 600 cells per frame), so the bound
///   has to be a number the format itself keeps small;
/// * the **cells of the whole blob** are bounded by a constant too — the
///   decoder keeps every frame, so 240 frames of a 960 x 360 grid (83 M
///   cells) is a real export and a hostile blob must not ask for more;
/// * the **frames** must fit in the blob: every row of every frame
///   takes at least one byte, so a blob that claims more rows than it
///   has bytes is malformed.
fn check_the_geometry(
    cols: usize,
    rows: usize,
    frame_count: usize,
    blob_bytes: usize,
) -> Result<(), CodecError> {
    let cells = cols
        .checked_mul(rows)
        .ok_or_else(|| CodecError::Malformed("the geometry overflows".into()))?;
    if cells > MAX_CELLS_PER_FRAME {
        return Err(CodecError::Malformed(format!(
            "the blob claims a {cols} x {rows} grid, which is {cells} cells"
        )));
    }
    let total = cells
        .checked_mul(frame_count)
        .ok_or_else(|| CodecError::Malformed("the frame count overflows".into()))?;
    if total > MAX_DECODED_CELLS {
        return Err(CodecError::Malformed(format!(
            "the blob decodes into {total} cells"
        )));
    }
    let rows_in_the_blob = rows
        .checked_mul(frame_count)
        .ok_or_else(|| CodecError::Malformed("the frame count overflows".into()))?;
    if rows_in_the_blob > blob_bytes {
        return Err(CodecError::Malformed(format!(
            "the blob claims {frame_count} frames of {rows} rows but is only {blob_bytes} bytes"
        )));
    }
    Ok(())
}

/// The backdrop layer reader.
fn take_layers(bytes: &[u8], at: &mut usize, len: usize) -> Result<Vec<BlobLayer>, CodecError> {
    let mut layers = Vec::with_capacity(len);
    for _ in 0..len {
        let cols = take_u16(bytes, at)? as usize;
        let rows = take_u16(bytes, at)? as usize;
        let row_offset = take_u16(bytes, at)? as usize;
        let drawn_rows = take_u16(bytes, at)? as usize;
        let rate_milli = take_u16(bytes, at)?;
        let count = cols.saturating_mul(rows);
        let end = at.saturating_add(count);
        let raw = bytes.get(*at..end).ok_or(CodecError::Truncated)?;
        let cells = raw.iter().map(|b| *b as u16).collect();
        *at = end;
        layers.push(BlobLayer {
            cols,
            rows,
            row_offset,
            drawn_rows,
            rate_milli,
            cells,
        });
    }
    Ok(layers)
}

/// The palette reader.
fn take_palette(bytes: &[u8], at: &mut usize, len: usize) -> Result<Vec<PaletteEntry>, CodecError> {
    let mut palette = Vec::with_capacity(len);
    for _ in 0..len {
        let glyph = take_u8(bytes, at)? as char;
        let color = [
            take_u8(bytes, at)?,
            take_u8(bytes, at)?,
            take_u8(bytes, at)?,
        ];
        palette.push(PaletteEntry { glyph, color });
    }
    Ok(palette)
}

/// The glyph bitmap reader.
fn take_glyphs(bytes: &[u8], at: &mut usize, len: usize) -> Result<Vec<GlyphBitmap>, CodecError> {
    let mut glyphs = Vec::with_capacity(len);
    for _ in 0..len {
        let glyph = take_u8(bytes, at)? as char;
        let width = take_u16(bytes, at)?;
        let height = take_u16(bytes, at)?;
        let count = width as usize * height as usize;
        let end = at.saturating_add(count);
        let coverage = bytes.get(*at..end).ok_or(CodecError::Truncated)?.to_vec();
        *at = end;
        glyphs.push(GlyphBitmap {
            glyph,
            width,
            height,
            coverage,
        });
    }
    Ok(glyphs)
}

/// The frame reader: it keeps the two working grids.
struct FrameDecoder {
    cols: usize,
    rows: usize,
    layer_count: usize,
    previous: Vec<u16>,
    current: Vec<u16>,
    sky: Vec<[u8; 3]>,
    has_sky: bool,
}

impl FrameDecoder {
    /// Read one frame and rebuild its cells.
    fn take_frame(&mut self, bytes: &[u8], at: &mut usize) -> Result<DecodedFrame, CodecError> {
        let offset_px = unzigzag(get_varint(bytes, at)?);
        let same_sky = take_u8(bytes, at)? != 0;
        if !same_sky {
            let mut ramp = Vec::with_capacity(self.rows);
            for _ in 0..self.rows {
                ramp.push([
                    take_u8(bytes, at)?,
                    take_u8(bytes, at)?,
                    take_u8(bytes, at)?,
                ]);
            }
            self.sky = ramp;
        } else if self.has_sky && self.sky.is_empty() {
            // The header says the animation has a sky and this frame
            // repeats the previous one — but no previous frame carried a
            // sky. The blob is malformed, not merely odd.
            return Err(CodecError::Malformed(
                "the frame repeats a sky the decoder does not have".into(),
            ));
        }
        let mut layer_offsets = Vec::with_capacity(self.layer_count);
        for _ in 0..self.layer_count {
            let offset = unzigzag(get_varint(bytes, at)?);
            layer_offsets.push(offset);
        }
        self.current.copy_from_slice(&self.previous);
        for row in 0..self.rows {
            self.take_row(bytes, at, row)?;
        }
        let frame = DecodedFrame {
            cells: self.current.clone(),
            sky: self.sky.clone(),
            offset_px,
            layer_offsets,
        };
        self.previous.copy_from_slice(&self.current);
        Ok(frame)
    }

    /// Read one row's diff and apply it.
    fn take_row(&mut self, bytes: &[u8], at: &mut usize, row: usize) -> Result<(), CodecError> {
        let mode = take_u8(bytes, at)?;
        if mode == MODE_UNCHANGED {
            // The row kept the previous frame's cells.
            return Ok(());
        }
        let start = row * self.cols;
        match mode {
            MODE_ACROSS => {}
            MODE_ABOVE => {
                if row == 0 {
                    return Err(CodecError::Malformed(
                        "the first row has no row above".into(),
                    ));
                }
                // The base is the row above: the decoder copies it first.
                let (above, here) = self.current.split_at_mut(start);
                here[..self.cols].copy_from_slice(&above[start - self.cols..start]);
            }
            MODE_SHIFT_ACROSS => {
                // The base is the previous frame's row, moved sideways.
                let shift = unzigzag(get_varint(bytes, at)?) as i64;
                for col in 0..self.cols {
                    let source = col as i64 + shift;
                    let value = if source >= 0 && source < self.cols as i64 {
                        self.previous[start + source as usize]
                    } else {
                        0
                    };
                    self.current[start + col] = value;
                }
            }
            MODE_SHIFT_ABOVE => {
                if row == 0 {
                    return Err(CodecError::Malformed(
                        "the first row has no row above".into(),
                    ));
                }
                // The base is the row above, moved sideways.
                let shift = unzigzag(get_varint(bytes, at)?) as i64;
                for col in 0..self.cols {
                    let source = col as i64 + shift;
                    let value = if source >= 0 && source < self.cols as i64 {
                        self.current[start - self.cols + source as usize]
                    } else {
                        0
                    };
                    self.current[start + col] = value;
                }
            }
            _ => return Err(CodecError::Malformed("an unknown row mode".into())),
        }
        let run_count = get_varint(bytes, at)? as usize;
        let mut previous_end = 0u32;
        for _ in 0..run_count {
            let delta = get_varint(bytes, at)?;
            let len = get_varint(bytes, at)?;
            let value = take_u8(bytes, at)? as u16;
            let from = previous_end.saturating_add(delta) as usize;
            previous_end = previous_end.saturating_add(delta).saturating_add(len);
            let to = from.saturating_add(len as usize);
            if to > self.cols {
                return Err(CodecError::Malformed("a run runs past the row".into()));
            }
            for cell in self.current[start + from..start + to].iter_mut() {
                *cell = value;
            }
        }
        Ok(())
    }
}

/// Read one byte.
fn take_u8(bytes: &[u8], at: &mut usize) -> Result<u8, CodecError> {
    let byte = *bytes.get(*at).ok_or(CodecError::Truncated)?;
    *at += 1;
    Ok(byte)
}

/// Read one little-endian `u16`.
fn take_u16(bytes: &[u8], at: &mut usize) -> Result<u16, CodecError> {
    let end = at.saturating_add(2);
    let slice = bytes.get(*at..end).ok_or(CodecError::Truncated)?;
    *at = end;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::DEFAULT_FG;

    /// A grid with a few inked cells.
    fn sample_grid(cols: usize, rows: usize) -> Grid {
        let mut grid = Grid::new(cols, rows, 2, 3);
        grid.set(1, 0, 'X', DEFAULT_FG);
        grid.set(2, 0, 'X', DEFAULT_FG);
        grid.set(3, 1, '#', [10, 20, 30]);
        grid
    }

    /// A font at the export's cell size.
    fn font() -> Font {
        Font::new(2, 3).expect("the embedded font must parse")
    }

    /// The page's decoder and this one must know the **same**
    /// constants.
    ///
    /// WHY: the page's JavaScript decoder is a twin of this one, and
    /// nothing ran it against a Rust-produced blob — so a change to a
    /// constant on one side would only show up in the browser (audit
    /// 2026-10-21 §E11).
    #[test]
    fn the_page_decoder_knows_the_same_constants() {
        let page = include_str!("page.js");
        let magic = String::from_utf8_lossy(&MAGIC).to_string();
        assert!(
            page.contains(&format!("\"{magic}\"")),
            "the magic must match: {magic}"
        );
        assert!(
            page.contains(&format!("version !== {VERSION}")),
            "the version must match: {VERSION}"
        );
        for mode in [
            MODE_ABOVE,
            MODE_SHIFT_ACROSS,
            MODE_SHIFT_ABOVE,
            MODE_UNCHANGED,
        ] {
            assert!(
                page.contains(&format!("mode === {mode}")),
                "the mode {mode} must match"
            );
        }
        // The `across` mode is the page's **default** branch: it names
        // no number, so the page must not compare against zero.
        assert_eq!(MODE_ACROSS, 0, "the across mode is the default branch");
        assert!(
            !page.contains("mode === 0"),
            "the across mode is the page's default branch"
        );
        // The message band (blob version 2, audit F-140): the page must
        // read the flag and paint the band.
        assert!(
            page.contains("var messageFlag = reader.u8()"),
            "the page must read the message flag byte"
        );
        assert!(
            page.contains("head.message"),
            "the page must carry the message block in its head"
        );
        // The coverage **floor** is blit.rs's rule, mirrored (audit
        // F-146): both sides must name the same ceiling, and the page's
        // condition must have the same *shape* as the blit's — a cell
        // qualifies only when BOTH dims are at or under the ceiling.
        let blit = include_str!("../blit.rs");
        assert!(
            blit.contains("const FLOOR_CELL_MAX: i64 = 6"),
            "the blit's floor is the number this test pins"
        );
        assert!(
            page.contains("var floorCellMax = 6"),
            "the page's floor must mirror blit.rs FLOOR_CELL_MAX"
        );
        assert!(
            page.contains("cellW <= floorCellMax && head.cellH <= floorCellMax"),
            "the page's floor condition must match blit.rs's `&&` shape"
        );
        // The cell ceilings (audit F-150): one frame's cells and a whole
        // blob's cells, the same numbers on both sides.
        assert_eq!(MAX_CELLS_PER_FRAME, 4 * 1024 * 1024);
        assert_eq!(MAX_DECODED_CELLS, 128 * 1024 * 1024);
        assert!(
            page.contains("var MAX_CELLS_PER_FRAME = 4 * 1024 * 1024"),
            "the page's per-frame ceiling must match codec.rs"
        );
        assert!(
            page.contains("var MAX_DECODED_CELLS = 128 * 1024 * 1024"),
            "the page's whole-blob ceiling must match codec.rs"
        );
    }

    #[test]
    fn a_sky_cannot_be_switched_off_mid_recording() {
        // The wire format has no "the sky went away": a None after a
        // Some would be read as "same as before" and the page would keep
        // painting the old ramp (audit 2026-10-23 F-139).
        let mut encoder = Encoder::new(4, 3, 2, 3, 2, 12);
        let grid = sample_grid(4, 3);
        let ramp = vec![[1, 2, 3], [4, 5, 6], [7, 8, 9]];
        encoder
            .push_frame(&grid, Some(&ramp), 0, &[])
            .expect("the frame fits");
        let err = encoder
            .push_frame(&grid, None, 0, &[])
            .expect_err("sky-off after sky-on is refused");
        assert!(
            matches!(err, CodecError::Malformed(_)),
            "the refusal is a malformed blob: {err:?}"
        );
        assert!(
            format!("{err}").contains("sky"),
            "the message names the sky: {err}"
        );
    }

    #[test]
    fn a_non_ascii_glyph_is_refused() {
        // WHY: the blob's glyph field is **one byte**, so a glyph above
        // U+00FF was truncated on the way out and read back as a
        // different character — the page would have drawn the wrong
        // glyph (audit 2026-10-21 §F12).
        let mut encoder = Encoder::new(8, 4, 2, 3, 3, 12);
        let mut grid = Grid::new(8, 4, 2, 3);
        grid.set(1, 0, 'é', DEFAULT_FG);
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        let err = encoder
            .finish(&mut font())
            .expect_err("a non-ASCII glyph is refused");
        assert!(
            matches!(err, CodecError::Malformed(_)),
            "the refusal is a malformed blob: {err}"
        );
        assert!(
            format!("{err}").contains("not ASCII"),
            "the message names the problem: {err}"
        );
    }

    /// A hostile blob must not be able to ask for a huge allocation.
    ///
    /// WHY: `cols` and `rows` are 16-bit numbers from the blob, and
    /// `65535 x 65535` cells is about 17 GB (audit 2026-10-21 §F13).
    #[test]
    fn a_hostile_geometry_is_refused() {
        let mut encoder = Encoder::new(8, 4, 2, 3, 3, 12);
        let grid = sample_grid(8, 4);
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        let mut bytes = encoder.finish(&mut font()).expect("finish");
        // The geometry sits right after the magic, the version and the
        // sky flag.
        bytes[6..8].copy_from_slice(&u16::MAX.to_le_bytes());
        bytes[8..10].copy_from_slice(&u16::MAX.to_le_bytes());
        let err = decode(&bytes).expect_err("a hostile geometry is refused");
        assert!(
            matches!(err, CodecError::Malformed(_)),
            "the refusal is a malformed blob: {err}"
        );
    }

    #[test]
    fn a_blob_round_trips_through_the_decoder() {
        let mut encoder = Encoder::new(8, 4, 2, 3, 3, 12);
        let grid = sample_grid(8, 4);
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        encoder
            .push_frame(&grid, None, 1, &[])
            .expect("the frame fits");
        let bytes = encoder.finish(&mut font()).expect("finish");

        let blob = decode(&bytes).expect("decode");
        assert_eq!(blob.cols, 8);
        assert_eq!(blob.rows, 4);
        assert_eq!(blob.frames.len(), 2);
        assert_eq!(blob.frames[0].cells, blob.frames[1].cells);
        assert_eq!(blob.frames[0].offset_px, 0);
        assert_eq!(blob.frames[1].offset_px, 1);
    }

    #[test]
    fn the_decoded_cells_match_the_grid() {
        let mut encoder = Encoder::new(8, 4, 2, 3, 3, 12);
        let grid = sample_grid(8, 4);
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        let bytes = encoder.finish(&mut font()).expect("finish");
        let blob = decode(&bytes).expect("decode");

        // The space is index 0, so an inked cell must not be.
        let cells = &blob.frames[0].cells;
        assert_eq!(cells[0], 0, "the empty cell is the space");
        assert_ne!(cells[1], 0, "the inked cell carries a palette entry");
        let entry = &blob.palette[cells[1] as usize];
        assert_eq!(entry.glyph, 'X');
        assert_eq!(entry.color, DEFAULT_FG);
    }

    #[test]
    fn an_unchanged_frame_costs_almost_nothing() {
        let mut encoder = Encoder::new(64, 16, 2, 3, 12, 12);
        let grid = sample_grid(64, 16);
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        let first = encoder.frames.len();
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        let second = encoder.frames.len() - first;
        // The second frame changes nothing: one byte per row, no runs,
        // plus the two bytes of the frame's own header.
        assert!(
            second <= 18,
            "an unchanged frame must be tiny, got {second}"
        );
    }

    #[test]
    fn a_frame_of_the_wrong_shape_is_refused() {
        // A grid that does not match the blob was silently truncated or
        // padded before; it is an error now (audit 2026-10-22 §P2-25).
        let mut encoder = Encoder::new(4, 3, 2, 3, 2, 12);
        let too_small = sample_grid(3, 3);
        let err = encoder
            .push_frame(&too_small, None, 0, &[])
            .expect_err("the wrong shape is refused");
        assert!(
            matches!(err, CodecError::Malformed(_)),
            "the refusal names the shape: {err:?}"
        );
        let too_big = sample_grid(4, 4);
        assert!(
            encoder.push_frame(&too_big, None, 0, &[]).is_err(),
            "a taller grid is refused too"
        );
    }

    #[test]
    fn a_sky_ramp_of_the_wrong_length_is_refused() {
        // The decoder reads exactly one colour per row. A ramp of
        // another length desynchronises it, and the decoder then reads
        // the layer offsets as sky colours (audit 2026-10-22 §P1-4).
        let mut encoder = Encoder::new(4, 3, 2, 3, 2, 12);
        let grid = sample_grid(4, 3);
        let short = vec![[1, 2, 3], [4, 5, 6]];
        let err = encoder
            .push_frame(&grid, Some(&short), 0, &[])
            .expect_err("the wrong ramp length is refused");
        assert!(
            matches!(err, CodecError::Malformed(_)),
            "the refusal names the ramp: {err:?}"
        );
    }

    #[test]
    fn the_sky_travels_only_when_it_changes() {
        let mut encoder = Encoder::new(4, 3, 2, 3, 2, 12);
        let grid = sample_grid(4, 3);
        let ramp = vec![[1, 2, 3], [4, 5, 6], [7, 8, 9]];
        encoder
            .push_frame(&grid, Some(&ramp), 0, &[])
            .expect("the frame fits");
        let with_sky = encoder.frames.len();
        encoder
            .push_frame(&grid, Some(&ramp), 0, &[])
            .expect("the frame fits");
        let same = encoder.frames.len() - with_sky;
        // Nine bytes of ramp the second time would show up here.
        assert!(same < 9, "the sky must not be sent twice, got {same}");
    }

    #[test]
    fn a_blob_is_byte_identical_twice() {
        let build = || {
            let mut encoder = Encoder::new(16, 8, 2, 3, 6, 12);
            let grid = sample_grid(16, 8);
            encoder
                .push_frame(&grid, None, 0, &[])
                .expect("the frame fits");
            encoder
                .push_frame(&grid, None, 1, &[])
                .expect("the frame fits");
            encoder.finish(&mut font()).expect("finish")
        };
        assert_eq!(build(), build());
    }

    #[test]
    fn garbage_is_refused() {
        let err = decode(b"not a blob at all").expect_err("garbage is refused");
        assert!(matches!(err, CodecError::NotABlob(_)), "{err:?}");
    }

    #[test]
    fn a_truncated_blob_is_refused() {
        let mut encoder = Encoder::new(8, 4, 2, 3, 3, 12);
        let grid = sample_grid(8, 4);
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        let bytes = encoder.finish(&mut font()).expect("finish");
        let cut = &bytes[..bytes.len() - 4];
        assert!(decode(cut).is_err(), "a truncated blob is refused");
    }

    #[test]
    fn the_palette_never_exceeds_its_ceiling() {
        let mut encoder = Encoder::new(4, 1, 2, 3, 1, 12);
        let mut grid = Grid::new(4, 1, 2, 3);
        // More distinct colours than the palette can hold.
        for col in 0..4 {
            let color = [col as u8 * 60, 10, 20];
            grid.set(col, 0, 'X', color);
        }
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        assert!(encoder.palette().len() <= MAX_PALETTE);
    }

    /// A backdrop layer: a tile of inked cells and a band placement.
    fn a_layer(cols: usize, rows: usize) -> crate::layer::BackdropInfo<'static> {
        let mut tile = crate::balloon::AsciiSprite::new(cols, rows);
        for col in 0..cols {
            tile.set(col, 0, '#', [10, 20, 30]);
        }
        let tile = Box::leak(Box::new(tile));
        crate::layer::BackdropInfo {
            tile,
            row_offset: 4,
            rows,
            rate: crate::layer::backdrop_rate(0),
        }
    }

    #[test]
    fn the_blob_carries_the_backdrop_layers() {
        let mut encoder = Encoder::new(16, 8, 2, 3, 6, 12);
        let layer = a_layer(4, 2);
        encoder.set_layers(std::slice::from_ref(&layer));
        let grid = sample_grid(16, 8);
        encoder
            .push_frame(&grid, None, 0, &[-3])
            .expect("the frame fits");
        let bytes = encoder.finish(&mut font()).expect("finish");
        let blob = decode(&bytes).expect("decode");
        assert_eq!(blob.layers.len(), 1, "the layer must travel");
        let decoded = &blob.layers[0];
        assert_eq!(decoded.cols, 4);
        assert_eq!(decoded.rows, 2);
        assert_eq!(decoded.row_offset, 4);
        assert_eq!(decoded.drawn_rows, 2);
        assert_eq!(decoded.rate_milli, 100, "the rate travels in thousandths");
        // The tile's ink is in the palette and in the layer's cells.
        assert!(decoded.cells.iter().any(|cell| *cell != 0));
        assert_eq!(blob.frames[0].layer_offsets, vec![-3]);
    }

    #[test]
    fn the_space_is_always_the_first_palette_entry() {
        let mut encoder = Encoder::new(4, 1, 2, 3, 1, 12);
        let grid = sample_grid(4, 1);
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        assert_eq!(encoder.palette()[0].glyph, ' ');
    }

    /// A block of ink on one row.
    fn block_row(grid: &mut Grid, row: usize, from: usize, to: usize) {
        for col in from..to {
            grid.set(col as i64, row as i64, 'X', DEFAULT_FG);
        }
    }

    /// The shifted bases survive a round trip **with real runs**.
    ///
    /// WHY: the shift is the codec's biggest win and it had no test that
    /// moved actual content (audit 2026-10-23 F-159). Frame one parks a
    /// block on row 1; frame two slides it two cells right (the shifted
    /// *across* base wins with zero runs) and grows a staircase on row 2
    /// (the shifted *above* base wins the same way).
    #[test]
    fn the_shifted_bases_round_trip_real_runs() {
        let mut encoder = Encoder::new(24, 4, 2, 3, 2, 12);
        let mut first = Grid::new(24, 4, 2, 3);
        block_row(&mut first, 1, 8, 12);
        encoder
            .push_frame(&first, None, 0, &[])
            .expect("the frame fits");
        let after_first = encoder.frames.len();
        let mut second = Grid::new(24, 4, 2, 3);
        block_row(&mut second, 1, 10, 14);
        block_row(&mut second, 2, 12, 16);
        encoder
            .push_frame(&second, None, 1, &[])
            .expect("the frame fits");
        // The mode bytes of the two changed rows of the second frame.
        // The frame's own header is the offset varint (zigzag(1) = 2, a
        // single byte) plus the sky flag; row 0 did not change, so its
        // UNCHANGED byte sits right after.
        let frame_two = &encoder.frames[after_first..];
        assert_eq!(
            frame_two[0], 2,
            "the offset varint is one byte (zigzag(1) = 2)"
        );
        assert_eq!(frame_two[1], 1, "the sky flag: same as before (none)");
        assert_eq!(frame_two[2], MODE_UNCHANGED, "row 0 did not change");
        assert_eq!(
            frame_two[3], MODE_SHIFT_ACROSS,
            "the slid block must use the shifted across base"
        );
        // The row's record is the mode, the shift varint and the count.
        assert_eq!(frame_two[3 + 3], MODE_SHIFT_ABOVE, "the staircase row");
        // And the whole thing round trips cell for cell.
        let bytes = encoder.finish(&mut font()).expect("finish");
        let blob = decode(&bytes).expect("decode");
        let cells = &blob.frames[1].cells;
        for row in 0..4 {
            for col in 0..24 {
                let expected = second.char_at(col, row).unwrap_or(' ');
                let glyph = blob.palette[cells[row * 24 + col] as usize].glyph;
                assert_eq!(glyph, expected, "the cell {col},{row} moved");
            }
        }
    }

    /// The header's `u16` fields refuse a value that does not fit.
    ///
    /// WHY: the counts were cast with `as u16`, which wraps — a wide
    /// grid arrived as a narrow blob and every frame was cut short
    /// (audit 2026-10-23 F-151).
    #[test]
    fn an_oversized_count_is_refused_not_truncated() {
        let mut wide = Encoder::new(70_000, 4, 2, 3, 2, 12);
        let grid = Grid::new(70_000, 4, 2, 3);
        wide.push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        let err = wide
            .finish(&mut font())
            .expect_err("a 70 000-column grid is refused");
        assert!(
            matches!(err, CodecError::Malformed(_)),
            "the refusal is a malformed blob: {err:?}"
        );
        let mut tall = Encoder::new(4, 70_000, 2, 3, 2, 12);
        let grid = Grid::new(4, 70_000, 2, 3);
        tall.push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        assert!(tall.finish(&mut font()).is_err(), "a tall grid too");
    }

    /// The message band travels in the blob and lands where the scene
    /// put it (audit 2026-10-23 F-140).
    #[test]
    fn the_message_band_round_trips() {
        let mut encoder = Encoder::new(16, 8, 2, 3, 6, 12);
        let mut band = Grid::new(16, 5, 2, 3);
        band.set(7, 0, 'H', [255, 255, 255]);
        band.set(8, 0, 'I', [255, 255, 255]);
        let blit = crate::layer::LayerBlit {
            grid: &band,
            row_offset: 3,
            rows: band.rows(),
            offset_px: 0,
        };
        encoder.set_message(&blit);
        let grid = sample_grid(16, 8);
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        let bytes = encoder.finish(&mut font()).expect("finish");
        let blob = decode(&bytes).expect("decode");
        let message = blob.message.expect("the band must travel");
        // The band is cropped to its ink: two cells, one row, at the
        // band's own row offset.
        assert_eq!(message.row, 3);
        assert_eq!(message.col, 7);
        assert_eq!(message.cols, 2);
        assert_eq!(message.rows, 1);
        let glyphs: Vec<char> = message
            .cells
            .iter()
            .map(|cell| blob.palette[*cell as usize].glyph)
            .collect();
        assert_eq!(glyphs, vec!['H', 'I'], "the caption must survive");
    }

    /// A band with no ink carries no message block at all.
    #[test]
    fn an_empty_message_band_is_dropped() {
        let mut encoder = Encoder::new(16, 8, 2, 3, 6, 12);
        let band = Grid::new(16, 5, 2, 3);
        let blit = crate::layer::LayerBlit {
            grid: &band,
            row_offset: 3,
            rows: band.rows(),
            offset_px: 0,
        };
        encoder.set_message(&blit);
        let grid = sample_grid(16, 8);
        encoder
            .push_frame(&grid, None, 0, &[])
            .expect("the frame fits");
        let bytes = encoder.finish(&mut font()).expect("finish");
        let blob = decode(&bytes).expect("decode");
        assert!(blob.message.is_none(), "an empty band is no band");
    }

    /// The **golden blob**: a version-2 blob written byte by byte, read
    /// back by the decoder alone.
    ///
    /// WHY the encoder is not allowed to write it (audit 2026-10-23
    /// recommendation 31): the round-trip tests share the encoder with
    /// the decoder, so an encoder bug could quietly redefine the wire
    /// format and the tests would stay green. This blob is the format's
    /// second witness — and the message flag's slot, right after the
    /// header counts, is pinned by its very byte. The page's JavaScript
    /// decoder reads the same layout (the sync test pins the constants).
    #[test]
    fn a_golden_version_two_blob_decodes() {
        let bytes: Vec<u8> = [
            b'D', b'K', b'S', b'F', // the magic
            2,    // the version: the message band is in the format
            0,    // has_sky
            2, 0, // cols = 2
            1, 0, // rows = 1
            2, 0, // cell_w = 2
            3, 0, // cell_h = 3
            0, 0, // ground_row = 0
            12, 0, // fps = 12
            2, 0, // the palette holds two entries
            1, 0, // one frame
            1, 0, // one glyph bitmap
            0, 0, // no backdrop layers
            0, // the message flag: no band
            b' ', 0, 0, 0, // palette[0]: the transparent space
            b'X', 0xff, 0x40, 0x00, // palette[1]: an X in duck orange
            b'X', 2, 0, 3, 0, // the X bitmap, 2 x 3
            10, 20, 30, 40, 50, 60, // its six coverage bytes
            0,  // the frame's offset varint (zigzag(0) = 0)
            1,  // the sky flag: same as before (there is none)
            4,  // row 0: MODE_UNCHANGED, the blank row it starts with
        ]
        .to_vec();
        let blob = decode(&bytes).expect("the golden blob decodes");
        assert_eq!(blob.cols, 2);
        assert_eq!(blob.rows, 1);
        assert_eq!(blob.message, None, "the flag byte says no band");
        assert_eq!(blob.palette.len(), 2);
        assert_eq!(blob.palette[1].glyph, 'X');
        assert_eq!(blob.glyphs.len(), 1);
        assert_eq!(blob.glyphs[0].coverage, vec![10, 20, 30, 40, 50, 60]);
        assert_eq!(blob.frames.len(), 1);
        assert_eq!(
            blob.frames[0].cells,
            vec![0, 0],
            "the unchanged row is blank"
        );
    }
}
