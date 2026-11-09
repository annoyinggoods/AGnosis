//! Animated GIF encoder: a global 256-colour palette, Floyd–Steinberg
//! dithering and a hand-rolled GIF LZW writer.
//!
//! WHY a hand-rolled encoder and not a crate: the project ships one static
//! binary and takes **no new dependency** (AGENTS.md), and the GIF format is
//! small enough to own — the same argument that put the store-only ZIP
//! writer in `package.rs`. The roadmap's own note ("the `gif` crate is pure
//! Rust and small", Extension 4 §3) is answered the way this repo answers
//! every other format question: the format is the dependency.
//!
//! WHAT the encoder does, and why each piece is there:
//!
//! * **One global palette for the whole animation.** A per-frame palette
//!   would let every frame pick its own colours, but a GIF89a file with a
//!   local colour table per frame costs 768 bytes a frame and defeats the
//!   "one palette" idea the dithering depends on: the ducks, the grass and
//!   the sky ramp must land on the *same* colours in every frame, or the
//!   parade flickers between palettes as it scrolls.
//! * **Median cut, then Floyd–Steinberg with a dead zone.** The scene
//!   paints per-cell RGB and the sky is a gradient (roadmap Extension 1
//!   §3.1a), so a flat 256-colour palette bands the ramp into stripes.
//!   The dither is the standard left-to-right, top-to-bottom scan (NOT a
//!   serpentine): the error is diffused on the **un-premultiplied** RGB
//!   with the classic 7/16, 3/16, 5/16, 1/16 weights, and the error is
//!   carried in sixteenths so the weights are exact integer divisions
//!   and the result is bit-reproducible.
//!   The dither stops at [`DITHER_DEAD_ZONE`]: a pixel that is already
//!   within a few units of its palette entry is snapped to it and
//!   **nothing** is diffused. The roadmap's DECISION is to dither
//!   because a flat palette turns the sky into stripes — it is not to
//!   dither a one-unit residual, which is invisible but changes the
//!   palette index of a whole row and breaks the long runs the
//!   compressor lives on.
//! * **The nearest-colour search is memoised.** The search is a scan
//!   over the whole palette, which costs a few hundred nanoseconds a
//!   pixel. The parade's art repeats its colours from frame to frame,
//!   so the memo turns every *distinct* colour into one scan: the
//!   The quantisation of 240 frames of 960 × 540 went from about 25 s to
//!   3.5 s. It is a cache and nothing more — the answer it holds is the
//!   answer the scan would give.
//! * **Palette slot 0 is reserved for "no colour".** A frame may carry
//!   fully transparent pixels; they take index 0, the graphic control
//!   extension names index 0 as the transparent index, and the quantiser
//!   never hands a real colour to that slot. The palette the caller sees
//!   therefore has one extra entry at the front, and `palette[index]` is
//!   the colour of that index.
//! * **Every frame is a stream of its own.** A frame's LZW data opens with
//!   a clear code, so a decoder may reset its dictionary between the
//!   frames — which is what every decoder in the wild does. The
//!   `gif` 0.14 crate (the one the `image` crate reads GIFs with) resets
//!   its table at every image descriptor (`reader/decoder.rs`,
//!   `LzwInit` → `LzwReader::reset` → `weezl::Decoder::reset`, which
//!   re-inits the code table), so a file whose second frame continues the
//!   first frame's dictionary is **not** decodable by it: it stops with
//!   "invalid code in LZW stream". The browsers build a fresh LZW
//!   decoder per frame as well, so they stop there too.
//!   [`encode_with_shared_dictionary`] writes that shared-dictionary
//!   variant anyway, because it is much smaller — half the size of the
//!   default file, 0.20 MiB against 0.44 MiB for 240 frames of
//!   960 × 540 of the parade's own art — but it is opt-in, and only this
//!   module's own decoder reads it. The default file is well inside the
//!   roadmap's 2 MB budget (Extension 4 §3.2) either way, so the size
//!   does not buy the compatibility.
//!   A GIF's code stream is a *bit* stream, so each frame's data is
//!   byte-aligned: the trailing bits of a frame are padded and the next
//!   frame starts on a byte boundary.
//!
//! The encoder is **deterministic**: the same frames always produce the
//! same bytes. The palette is built from a `BTreeMap` histogram (so the
//! colour order never depends on a hash seed), the median cut breaks its
//! ties by colour value, and the dither scan is a fixed raster order.

// The decoder half of this module is the **test** twin of the writer.
// It is public, so in the library crate it is never dead code — and the
// module-level `#[allow(dead_code)]` this file used to carry could only
// mask a real regression (audit 2026-10-21 §F9).

use std::collections::{BTreeMap, HashMap};

// ---------------------------------------------------------------------------
// The format constants
// ---------------------------------------------------------------------------

/// The GIF89a signature.
const HEADER: &[u8; 6] = b"GIF89a";

/// The trailer that ends the file.
const TRAILER: u8 = 0x3B;

/// The introducer of an extension block.
const EXTENSION: u8 = 0x21;

/// The introducer of an image descriptor.
const IMAGE_DESCRIPTOR: u8 = 0x2C;

/// The label of the graphic control extension.
const GRAPHIC_CONTROL: u8 = 0xF9;

/// The label of the application extension (the NETSCAPE2.0 loop).
const APPLICATION: u8 = 0xFF;

/// The label of the comment extension.
const COMMENT: u8 = 0xFE;

/// The application id of the looping extension.
const NETSCAPE_APP: &[u8; 11] = b"NETSCAPE2.0";

/// The text of the comment that marks a shared-dictionary file.
///
/// A file whose frames share one LZW dictionary cannot be read by the
/// decoders in the wild, and a decoder cannot tell such a file from a
/// normal one by looking at the codes: the width of a frame's first code
/// is the only hint, and it is ambiguous. The comment settles it — every
/// other decoder ignores a comment, so the file stays a legal GIF.
const SHARED_MARKER: &[u8] = b"agnosis:shared-lzw";

/// The LZW minimum code size: a 256-entry colour table.
const MIN_CODE_SIZE: u8 = 8;

/// The palette slot reserved for "no colour" (the transparent index).
const TRANSPARENT_INDEX: u8 = 0;

/// The colour the reserved slot carries. Nothing is ever drawn in it, so
/// the value is arbitrary; black keeps the file readable in a hex dump.
const TRANSPARENT_RGB: [u8; 3] = [0, 0, 0];

/// The number of real colours a palette may hold: slot 0 is reserved.
const MAX_COLORS: usize = 255;

/// The largest logical screen a GIF can address (the descriptor is u16).
const MAX_DIMENSION: u32 = 0xFFFF;

/// The largest screen the test decoder will allocate: 64 megapixels, or
/// 256 MB of RGBA. The export renders a screen in the tens of thousands
/// of pixels, so a real file is orders of magnitude below this.
const MAX_DECODE_PIXELS: u64 = 64 * 1024 * 1024;

/// The largest dictionary code a GIF's LZW may use (12 bits).
const MAX_LZW_CODE: u32 = 4096;

/// The number of slots in the encoder's LZW dictionary table.
///
/// The dictionary holds at most `MAX_LZW_CODE - 258` entries, so half the
/// table is free and the linear probe never fills up.
const DICT_SLOTS: usize = 8192;

/// The biggest block a GIF sub-block may carry.
const MAX_SUB_BLOCK: usize = 255;

/// The dead zone of the dither: a distance in RGB space.
///
/// A pixel whose nearest palette entry is within this distance is
/// snapped to that entry **exactly**, and no error is diffused for it.
/// The comparison is on the squared distance, so the zone is this many
/// units of colour difference.
///
/// WHY: the sky is a smooth gradient whose ~288 row colours do not all
/// fit in a 255-entry palette. Merging two adjacent ramp colours leaves
/// a one-unit residual, and diffusing even that changes the palette
/// INDEX of the row — which breaks the long horizontal runs the LZW
/// compressor lives on (a dithered 960x540 sky measured 33 MB against a
/// 2 MB budget). Snapping the invisible error keeps the gradient smooth
/// to the eye AND keeps the runs intact.
pub const DITHER_DEAD_ZONE: u32 = 6;

/// The squared dead zone: the threshold the squared distance is tested
/// against.
const DEAD_ZONE_SQUARED: u32 = DITHER_DEAD_ZONE * DITHER_DEAD_ZONE;

// ---------------------------------------------------------------------------
// The public data
// ---------------------------------------------------------------------------

/// One frame of the animation: RGBA8 pixels, row-major.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GifFrame {
    /// The frame width in pixels.
    pub width: u32,
    /// The frame height in pixels.
    pub height: u32,
    /// RGBA8 bytes, `width * height * 4`, row-major.
    pub rgba: Vec<u8>,
}

/// Everything that can go wrong while encoding or decoding a GIF.
#[derive(Debug, thiserror::Error)]
pub enum GifError {
    /// A frame's byte count does not match its dimensions.
    #[error("the frame {index} is {width}x{height} but carries {bytes} bytes")]
    FrameSize {
        /// The index of the offending frame.
        index: usize,
        /// Its width.
        width: u32,
        /// Its height.
        height: u32,
        /// The byte count it actually carries.
        bytes: usize,
    },
    /// The frames are not all the same size.
    #[error("the frames differ in size: frame {index} is {width}x{height}, the first is {first_width}x{first_height}")]
    FrameSizeMismatch {
        /// The index of the offending frame.
        index: usize,
        /// Its width.
        width: u32,
        /// Its height.
        height: u32,
        /// The first frame's width.
        first_width: u32,
        /// The first frame's height.
        first_height: u32,
    },
    /// The picture is larger than a GIF can address.
    #[error("the picture is too large for a GIF: {width}x{height}")]
    TooLarge {
        /// The offending width.
        width: u32,
        /// The offending height.
        height: u32,
    },
    /// There is nothing to encode.
    #[error("there are no frames to encode")]
    NoFrames,
    /// A frame has no pixels at all.
    ///
    /// WHY its own variant: a zero-sized frame is a **size** error, not
    /// a missing frame — the animation has frames, one of them is empty
    /// (audit 2026-10-21 §F16).
    #[error("the frame {index} has no pixels: {width}x{height}")]
    FrameEmpty {
        /// The index of the offending frame.
        index: usize,
        /// Its width.
        width: u32,
        /// Its height.
        height: u32,
    },
    /// The bytes are not a GIF this decoder understands.
    #[error("the file is not a GIF this build understands: {0}")]
    Format(String),
}

/// The quantised picture: one global palette and one index plane per frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quantized {
    /// The global palette, at most 256 entries, each an RGB triple.
    ///
    /// Entry 0 is the reserved "no colour" slot; the real colours start at
    /// entry 1, so a plane's byte is an index into this vector.
    pub palette: Vec<[u8; 3]>,
    /// One index plane per frame, `width * height` bytes each.
    pub planes: Vec<Vec<u8>>,
}

/// One decoded frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedFrame {
    /// The frame's delay in hundredths of a second.
    pub delay_cs: u16,
    /// The reconstructed RGBA8 pixels, `width * height * 4`.
    pub rgba: Vec<u8>,
}

/// A decoded GIF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedGif {
    /// The logical screen width in pixels.
    pub width: u32,
    /// The logical screen height in pixels.
    pub height: u32,
    /// The frames, in file order.
    pub frames: Vec<DecodedFrame>,
}

// ---------------------------------------------------------------------------
// The input check
// ---------------------------------------------------------------------------

/// Check the frames and return the picture's size.
///
/// The order of the checks is the order the errors are reported in: a
/// frame that lies about its size is a lie about everything else, so it is
/// caught before the frames are compared with each other.
fn validate(frames: &[GifFrame]) -> Result<(u32, u32), GifError> {
    let first = frames.first().ok_or(GifError::NoFrames)?;
    let (width, height) = (first.width, first.height);
    for (index, frame) in frames.iter().enumerate() {
        let expected = frame.width as u64 * frame.height as u64 * 4;
        if frame.rgba.len() as u64 != expected {
            return Err(GifError::FrameSize {
                index,
                width: frame.width,
                height: frame.height,
                bytes: frame.rgba.len(),
            });
        }
        if frame.width == 0 || frame.height == 0 {
            return Err(GifError::FrameEmpty {
                index,
                width: frame.width,
                height: frame.height,
            });
        }
        if frame.width > MAX_DIMENSION || frame.height > MAX_DIMENSION {
            return Err(GifError::TooLarge {
                width: frame.width,
                height: frame.height,
            });
        }
        if frame.width != width || frame.height != height {
            return Err(GifError::FrameSizeMismatch {
                index,
                width: frame.width,
                height: frame.height,
                first_width: width,
                first_height: height,
            });
        }
    }
    Ok((width, height))
}

// ---------------------------------------------------------------------------
// The palette: a deterministic median cut
// ---------------------------------------------------------------------------

/// One distinct colour and how often it occurs.
#[derive(Debug, Clone, Copy)]
struct Entry {
    colour: [u8; 3],
    count: u32,
}

/// A box of the median cut: a half-open range of the entry vector.
#[derive(Debug, Clone, Copy)]
struct ColourBox {
    start: usize,
    end: usize,
    min: [u8; 3],
    max: [u8; 3],
    count: u64,
}

impl ColourBox {
    /// Measure the box over `entries[start..end]`.
    ///
    /// The three channels are folded one by one: a loop over the channels
    /// inside a loop over the entries would be a loop inside a loop, which
    /// the nesting rule bans.
    fn measure(start: usize, end: usize, entries: &[Entry]) -> ColourBox {
        let mut min = [255u8; 3];
        let mut max = [0u8; 3];
        let mut count = 0u64;
        for entry in &entries[start..end] {
            min[0] = min[0].min(entry.colour[0]);
            min[1] = min[1].min(entry.colour[1]);
            min[2] = min[2].min(entry.colour[2]);
            max[0] = max[0].max(entry.colour[0]);
            max[1] = max[1].max(entry.colour[1]);
            max[2] = max[2].max(entry.colour[2]);
            count += entry.count as u64;
        }
        ColourBox {
            start,
            end,
            min,
            max,
            count,
        }
    }

    /// The widest channel of the box: the one a cut along it separates best.
    fn channel(&self) -> usize {
        let mut best = 0;
        let mut span = 0u32;
        for channel in 0..3 {
            let width = (self.max[channel] - self.min[channel]) as u32;
            if width > span {
                span = width;
                best = channel;
            }
        }
        best
    }

    /// The box's priority: how much a cut along it would gain.
    ///
    /// The count is in the product because a wide box that only a few
    /// pixels use is worth less than a narrow box that a gradient walks
    /// through.
    fn priority(&self) -> u64 {
        let span = (self.max[self.channel()] - self.min[self.channel()]) as u64;
        self.count * span
    }
}

/// Cut the histogram into at most `max_colors` boxes and average each box.
///
/// The cut is the classic median cut: the box with the greatest priority is
/// split along its widest channel, at the **weighted** median, so a colour
/// that covers half the picture keeps half the box.
fn median_cut(histogram: &BTreeMap<u32, u32>, max_colors: usize) -> Vec<[u8; 3]> {
    let mut entries: Vec<Entry> = histogram
        .iter()
        .map(|(&key, &count)| Entry {
            colour: [(key >> 16) as u8, (key >> 8) as u8, key as u8],
            count,
        })
        .collect();
    if entries.is_empty() {
        return Vec::new();
    }
    let mut boxes = vec![ColourBox::measure(0, entries.len(), &entries)];
    while boxes.len() < max_colors {
        let Some(index) = pick_box(&boxes) else {
            break;
        };
        let Some((left, right)) = split_box(&mut entries, &mut boxes[index]) else {
            break;
        };
        boxes[index] = left;
        boxes.push(right);
    }
    boxes.iter().map(|b| box_colour(&entries, b)).collect()
}

/// The box that is worth cutting next, or `None` when none can be cut.
fn pick_box(boxes: &[ColourBox]) -> Option<usize> {
    let mut best: Option<usize> = None;
    let mut best_priority = 0u64;
    for (index, b) in boxes.iter().enumerate() {
        if b.end - b.start < 2 {
            continue;
        }
        let priority = b.priority();
        if priority > best_priority {
            best_priority = priority;
            best = Some(index);
        }
    }
    best
}

/// Cut one box in two along its widest channel, at the weighted median.
///
/// The entries of the box are sorted by the cut channel first, so the cut
/// is reproducible: the tie between two colours of the same value is
/// broken by their own value.
fn split_box(entries: &mut [Entry], b: &mut ColourBox) -> Option<(ColourBox, ColourBox)> {
    let channel = b.channel();
    let (start, end) = (b.start, b.end);
    entries[start..end].sort_by_key(|entry| (entry.colour[channel], entry.colour));
    let total = b.count;
    let mut seen = 0u64;
    let mut middle = start;
    while middle < end {
        seen += entries[middle].count as u64;
        middle += 1;
        if seen * 2 >= total {
            break;
        }
    }
    // The cut must leave a colour on both sides, or the split is a no-op.
    if middle <= start {
        middle = start + 1;
    }
    if middle >= end {
        middle = end - 1;
    }
    let left = ColourBox::measure(start, middle, entries);
    let right = ColourBox::measure(middle, end, entries);
    *b = left;
    Some((left, right))
}

/// The colour of a box: the average of its colours, weighted by their
/// counts and rounded to nearest.
///
/// The three channels are summed one by one: a loop over the channels
/// inside a loop over the entries would be a loop inside a loop, which
/// the nesting rule bans.
fn box_colour(entries: &[Entry], b: &ColourBox) -> [u8; 3] {
    let mut count = 0u64;
    let mut red = 0u64;
    let mut green = 0u64;
    let mut blue = 0u64;
    for entry in &entries[b.start..b.end] {
        let weight = entry.count as u64;
        red += entry.colour[0] as u64 * weight;
        green += entry.colour[1] as u64 * weight;
        blue += entry.colour[2] as u64 * weight;
        count += weight;
    }
    let half = count / 2;
    [
        ((red + half) / count) as u8,
        ((green + half) / count) as u8,
        ((blue + half) / count) as u8,
    ]
}

/// Count the colours of every frame, ignoring the fully transparent pixels.
///
/// A `BTreeMap` keeps the colours in a fixed order, so the median cut
/// never depends on a hash seed.
fn histogram(frames: &[GifFrame]) -> BTreeMap<u32, u32> {
    let mut map: BTreeMap<u32, u32> = BTreeMap::new();
    for frame in frames {
        count_frame_colours(frame, &mut map);
    }
    map
}

/// Count one frame's colours into the histogram.
///
/// WHY a helper: the nesting rule is two levels at most, and the
/// histogram was a loop in a loop (audit 2026-10-21 §F8).
fn count_frame_colours(frame: &GifFrame, map: &mut BTreeMap<u32, u32>) {
    for pixel in frame.rgba.chunks_exact(4) {
        if pixel[3] == 0 {
            continue;
        }
        let key = ((pixel[0] as u32) << 16) | ((pixel[1] as u32) << 8) | pixel[2] as u32;
        *map.entry(key).or_insert(0) += 1;
    }
}

// ---------------------------------------------------------------------------
// The dither
// ---------------------------------------------------------------------------

/// Quantise the frames to one global palette, with Floyd–Steinberg dithering.
///
/// The palette is built from the pixels of **all** frames, so the colours
/// stay the same from frame to frame, and every frame is dithered against
/// that one palette.
pub fn quantize(frames: &[GifFrame]) -> Result<Quantized, GifError> {
    let (width, height) = validate(frames)?;
    quantize_checked(frames, width, height)
}

/// The palette slots the **sky ramp** is pre-seeded with (audit
/// 2026-10-23 F-148, roadmap R-10).
///
/// The sky is a smooth vertical ramp and the median cut spends its boxes
/// where the pixels are — on the ducks and the grass — so the dusk band
/// came out banded. A fixed set of slots is reserved for the ramp's own
/// colours before the cut runs, and the cut gets the rest.
const RAMP_SEED_SLOTS: usize = 48;

/// [`quantize`]'s body, once the input has been validated.
///
/// WHY the split: [`crate::export::gif::encode`] validated the frames a
/// second time through [`quantize`] — the same walk twice per encode
/// (audit 2026-10-23 F-158). The check now runs once and the result is
/// passed through.
fn quantize_checked(frames: &[GifFrame], width: u32, height: u32) -> Result<Quantized, GifError> {
    let seeds = ramp_seeds(frames, height);
    let budget = MAX_COLORS.saturating_sub(seeds.len());
    let mut real = median_cut(&histogram(frames), budget);
    if real.is_empty() {
        // Every pixel is transparent, so there is no colour to quantise —
        // but the palette still needs one real entry beside the reserved
        // slot, or an index could point at nothing.
        real.push(TRANSPARENT_RGB);
    }
    // The ramp slots come first, and they are **permanent**: the dither
    // can always find the ramp colour a pixel needs, band or no band.
    let mut palette = Vec::with_capacity(1 + seeds.len() + real.len());
    palette.push(TRANSPARENT_RGB);
    palette.extend_from_slice(&seeds);
    palette.extend_from_slice(&real);
    // One memo for the whole animation: the parade's art repeats its
    // colours from frame to frame, so a colour is searched once and not
    // once per pixel.
    let mut quantiser = Quantiser::new(&palette);
    let mut planes = Vec::with_capacity(frames.len());
    for frame in frames {
        planes.push(dither_frame(frame, &mut quantiser, width, height));
    }
    Ok(Quantized { palette, planes })
}

/// The sky ramp's own colours, sampled from **one column of frame 0**.
///
/// The ramp is vertical, so a single column carries it; the column is
/// walked top to bottom at even steps. A pure function of the frames, so
/// the export stays reproducible.
fn ramp_seeds(frames: &[GifFrame], height: u32) -> Vec<[u8; 3]> {
    let Some(first) = frames.first() else {
        return Vec::new();
    };
    let mut seeds: Vec<[u8; 3]> = Vec::new();
    let rows = RAMP_SEED_SLOTS.min(height as usize).max(1);
    for step in 0..rows {
        let y = step * height as usize / rows;
        let offset = (y * first.width as usize) * 4;
        let Some(pixel) = first.rgba.get(offset..offset + 4) else {
            continue;
        };
        if pixel[3] == 0 {
            continue;
        }
        let colour = [pixel[0], pixel[1], pixel[2]];
        if !seeds.contains(&colour) {
            seeds.push(colour);
        }
    }
    seeds
}

/// The index plane of one frame.
///
/// The error of the current row and of the next one are carried in
/// sixteenths of a unit, so the 7/16, 3/16, 5/16 and 1/16 weights of the
/// Floyd–Steinberg kernel are exact.
fn dither_frame(frame: &GifFrame, quantiser: &mut Quantiser, width: u32, height: u32) -> Vec<u8> {
    let w = width as usize;
    let h = height as usize;
    let mut plane = vec![0u8; w * h];
    let mut current = vec![[0i32; 3]; w];
    let mut next = vec![[0i32; 3]; w];
    for y in 0..h {
        dither_row(frame, quantiser, y, w, &mut current, &mut next, &mut plane);
        std::mem::swap(&mut current, &mut next);
        clear_errors(&mut next);
    }
    plane
}

/// Zero the error buffer of a row.
fn clear_errors(buffer: &mut [[i32; 3]]) {
    for slot in buffer.iter_mut() {
        *slot = [0; 3];
    }
}

/// Dither one row of a frame, left to right.
///
/// A fully transparent pixel takes the reserved index and diffuses no
/// error: it has no colour, so there is nothing to spread.
///
/// A pixel that is already within [`DITHER_DEAD_ZONE`] of its palette
/// entry is **snapped**: it takes that entry and diffuses no error
/// either. The residual is too small to see, and diffusing it would
/// change the palette index of the row — which is what breaks the long
/// runs the compressor lives on.
fn dither_row(
    frame: &GifFrame,
    quantiser: &mut Quantiser,
    y: usize,
    w: usize,
    current: &mut [[i32; 3]],
    next: &mut [[i32; 3]],
    plane: &mut [u8],
) {
    for x in 0..w {
        let offset = (y * w + x) * 4;
        if frame.rgba[offset + 3] == 0 {
            plane[y * w + x] = TRANSPARENT_INDEX;
            continue;
        }
        let source = [
            frame.rgba[offset] as i32,
            frame.rgba[offset + 1] as i32,
            frame.rgba[offset + 2] as i32,
        ];
        let value = apply_error(source, current[x]);
        let (index, distance) = quantiser.nearest(value);
        plane[y * w + x] = index;
        if distance <= DEAD_ZONE_SQUARED {
            continue;
        }
        let entry = quantiser.palette[index as usize];
        let error = [
            (value[0] - entry[0] as i32) * 16,
            (value[1] - entry[1] as i32) * 16,
            (value[2] - entry[2] as i32) * 16,
        ];
        diffuse(current, next, x, w, error);
    }
}

/// Add the carried error to a colour and clamp it into a byte.
fn apply_error(source: [i32; 3], error: [i32; 3]) -> [i32; 3] {
    let mut out = [0i32; 3];
    for channel in 0..3 {
        let scaled = source[channel] * 16 + error[channel];
        out[channel] = ((scaled + 8) >> 4).clamp(0, 255);
    }
    out
}

/// Spread the error of one pixel over its neighbours.
fn diffuse(current: &mut [[i32; 3]], next: &mut [[i32; 3]], x: usize, w: usize, error: [i32; 3]) {
    if x + 1 < w {
        add_error(&mut current[x + 1], error, 7);
    }
    if x > 0 {
        add_error(&mut next[x - 1], error, 3);
    }
    add_error(&mut next[x], error, 5);
    if x + 1 < w {
        add_error(&mut next[x + 1], error, 1);
    }
}

/// Add a weighted part of an error to one slot.
///
/// The error is in sixteenths and the weight is a sixteenth, so the
/// division is exact and the dither never loses a fraction.
fn add_error(slot: &mut [i32; 3], error: [i32; 3], weight: i32) {
    for channel in 0..3 {
        slot[channel] += error[channel] * weight / 16;
    }
}

/// The palette entry that is closest to a colour, and the squared
/// distance to it.
///
/// The reserved slot is never a candidate, so the palette must hold it
/// plus at least one real colour — [`quantize`] always builds that.
/// The scan stops at an exact match, which is the common case in the
/// flat parts of the art.
fn nearest_entry(palette: &[[u8; 3]], value: [i32; 3]) -> (u8, u32) {
    let mut best = 1u8;
    let mut best_distance = u32::MAX;
    for (index, entry) in palette.iter().enumerate().skip(1) {
        let distance = distance_to(entry, value);
        if distance == 0 {
            return (index as u8, 0);
        }
        if distance < best_distance {
            best_distance = distance;
            best = index as u8;
        }
    }
    (best, best_distance)
}

/// The squared distance between a palette entry and a colour.
fn distance_to(entry: &[u8; 3], value: [i32; 3]) -> u32 {
    let dr = value[0] - entry[0] as i32;
    let dg = value[1] - entry[1] as i32;
    let db = value[2] - entry[2] as i32;
    (dr * dr + dg * dg + db * db) as u32
}

/// The quantiser's working state: the global palette and the memo of the
/// nearest-colour search.
///
/// The search is a scan over the whole palette, which costs a few
/// hundred nanoseconds per pixel — a minute for a 240-frame animation
/// with a full palette. The parade's art repeats its colours from frame
/// to frame, and the dead zone leaves the error at zero over the flat
/// parts, so the same colour is looked up again and again. The memo
/// turns every *distinct* colour into one scan.
///
/// It is a cache and nothing more: the answer it holds is the answer the
/// scan would give, so the quantisation is unchanged.
struct Quantiser<'a> {
    /// The global palette, the reserved slot first.
    palette: &'a [[u8; 3]],
    /// The memo: a packed colour and the entry it snapped to.
    entries: HashMap<u32, u8>,
}

impl<'a> Quantiser<'a> {
    /// A quantiser for one palette.
    fn new(palette: &'a [[u8; 3]]) -> Self {
        Self {
            palette,
            entries: HashMap::new(),
        }
    }

    /// The palette entry closest to a colour, and the squared distance to
    /// it.
    fn nearest(&mut self, value: [i32; 3]) -> (u8, u32) {
        let key = pack_colour(value);
        let index = match self.entries.get(&key) {
            Some(&index) => index,
            None => {
                let (index, _) = nearest_entry(self.palette, value);
                self.entries.insert(key, index);
                index
            }
        };
        let distance = distance_to(&self.palette[index as usize], value);
        (index, distance)
    }
}

/// Pack a colour into a key.
fn pack_colour(value: [i32; 3]) -> u32 {
    ((value[0] as u32) << 16) | ((value[1] as u32) << 8) | value[2] as u32
}

// ---------------------------------------------------------------------------
// The LZW writer
// ---------------------------------------------------------------------------

/// The LZW dictionary: the map from a (prefix, byte) pair to a code.
///
/// The table is a hash table with linear probing. It is an implementation
/// detail — the codes themselves are what the format defines — and it is
/// rebuilt from scratch when the dictionary is cleared.
struct Dictionary {
    /// The key of each slot, plus one; zero means the slot is free.
    keys: Vec<u32>,
    /// The code of each occupied slot.
    codes: Vec<u16>,
}

impl Dictionary {
    /// An empty dictionary.
    fn new() -> Self {
        Self {
            keys: vec![0; DICT_SLOTS],
            codes: vec![0; DICT_SLOTS],
        }
    }

    /// Forget every entry.
    fn clear(&mut self) {
        self.keys.fill(0);
    }

    /// The slot a key starts its probe at.
    fn slot(&self, key: u32) -> usize {
        (key.wrapping_mul(2_654_435_761) >> 16) as usize & (DICT_SLOTS - 1)
    }

    /// The code of a (prefix, byte) pair, if the dictionary holds one.
    fn get(&self, prefix: u32, byte: u8) -> Option<u16> {
        let key = (prefix << 8) | byte as u32;
        let mut index = self.slot(key);
        loop {
            let stored = self.keys[index];
            if stored == 0 {
                return None;
            }
            if stored == key + 1 {
                return Some(self.codes[index]);
            }
            index = (index + 1) & (DICT_SLOTS - 1);
        }
    }

    /// Remember a (prefix, byte) pair under a code.
    fn insert(&mut self, prefix: u32, byte: u8, code: u16) {
        let key = (prefix << 8) | byte as u32;
        let mut index = self.slot(key);
        loop {
            let stored = self.keys[index];
            if stored == 0 {
                self.keys[index] = key + 1;
                self.codes[index] = code;
                return;
            }
            if stored == key + 1 {
                return;
            }
            index = (index + 1) & (DICT_SLOTS - 1);
        }
    }
}

/// The GIF-flavoured LZW encoder.
///
/// The codes are packed **LSB first** into bytes. The clear code is
/// `1 << min_code_size`, the end code is one above it, and the first free
/// dictionary entry is one above that. The code width grows when the next
/// free code no longer fits in it, and it stops at 12 bits.
struct LzwWriter {
    /// The packed bytes of the frame being written.
    bytes: Vec<u8>,
    /// The bits that did not fill a byte yet.
    accumulator: u32,
    /// How many bits the accumulator holds.
    bits: u32,
    /// The width of the codes being emitted.
    code_size: u32,
    /// The next free dictionary code.
    next_code: u32,
    /// The clear code.
    clear_code: u32,
    /// The end-of-information code.
    end_code: u32,
    /// The dictionary itself.
    dictionary: Dictionary,
    /// Whether the dictionary carries from one frame to the next.
    share_dictionary: bool,
    /// Whether the first frame has been written.
    started: bool,
}

impl LzwWriter {
    /// A writer for a 256-colour table.
    ///
    /// `share_dictionary` decides whether a frame continues the code
    /// stream of the one before it.
    fn new(share_dictionary: bool) -> Self {
        let clear_code = 1 << MIN_CODE_SIZE;
        Self {
            bytes: Vec::new(),
            accumulator: 0,
            bits: 0,
            code_size: MIN_CODE_SIZE as u32 + 1,
            next_code: clear_code + 2,
            clear_code,
            end_code: clear_code + 1,
            dictionary: Dictionary::new(),
            share_dictionary,
            started: false,
        }
    }

    /// Write one code, least significant bit first.
    fn emit(&mut self, code: u32) {
        self.accumulator |= code << self.bits;
        self.bits += self.code_size;
        while self.bits >= 8 {
            self.bytes.push((self.accumulator & 0xFF) as u8);
            self.accumulator >>= 8;
            self.bits -= 8;
        }
    }

    /// Write the codes of one frame's index plane.
    ///
    /// With a shared dictionary the frame continues the code stream of the
    /// one before it, so the strings the first frame taught the encoder are
    /// still known. Without one, the frame is a stream of its own: the
    /// dictionary is emptied and a clear code opens the frame, which is
    /// what every decoder in the wild expects.
    fn write_plane(&mut self, plane: &[u8]) {
        if plane.is_empty() {
            return;
        }
        self.open_frame();
        let mut prefix = plane[0] as u32;
        for &byte in &plane[1..] {
            if let Some(code) = self.dictionary.get(prefix, byte) {
                prefix = code as u32;
                continue;
            }
            self.emit(prefix);
            self.add_entry(prefix, byte);
            prefix = byte as u32;
        }
        self.emit(prefix);
        self.emit(self.end_code);
        self.flush();
    }

    /// Open a frame's code stream.
    fn open_frame(&mut self) {
        if !self.share_dictionary {
            // The clear code goes out at the initial width, so the
            // decoder reads it with the width it starts a frame with.
            self.reset_dictionary();
            self.emit(self.clear_code);
            self.started = true;
            return;
        }
        if self.started {
            return;
        }
        self.emit(self.clear_code);
        self.started = true;
    }

    /// Teach the dictionary one more string.
    fn add_entry(&mut self, prefix: u32, byte: u8) {
        if self.next_code >= MAX_LZW_CODE {
            // The dictionary is full, so it is cleared. The clear code
            // goes out at the **current** width: the decoder is still
            // reading that width when it sees the clear.
            self.emit(self.clear_code);
            self.reset_dictionary();
            return;
        }
        // The width grows before the code is used, so the code that would
        // no longer fit is never emitted at the old width.
        if self.next_code >= (1 << self.code_size) && self.code_size < 12 {
            self.code_size += 1;
        }
        self.dictionary.insert(prefix, byte, self.next_code as u16);
        self.next_code += 1;
    }

    /// Forget the dictionary and go back to the initial code width.
    fn reset_dictionary(&mut self) {
        self.dictionary.clear();
        self.next_code = self.clear_code + 2;
        self.code_size = MIN_CODE_SIZE as u32 + 1;
    }

    /// Pad the frame's last byte: the next frame starts on a byte.
    fn flush(&mut self) {
        if self.bits > 0 {
            self.bytes.push((self.accumulator & 0xFF) as u8);
            self.accumulator = 0;
            self.bits = 0;
        }
    }

    /// The bytes of the frame that was written.
    fn take_bytes(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.bytes)
    }
}

// ---------------------------------------------------------------------------
// The writer
// ---------------------------------------------------------------------------

/// Encode an animated GIF (GIF89a) from the RGBA frames.
///
/// `delay_cs` is the per-frame delay in hundredths of a second.
///
/// Every frame is written as a code stream of its own, so the file is
/// readable by any GIF decoder — see the module doc for why the
/// dictionary is not shared between the frames.
pub fn encode(frames: &[GifFrame], delay_cs: u16) -> Result<Vec<u8>, GifError> {
    encode_with(frames, delay_cs, false)
}

/// Encode an animated GIF whose frames **share one LZW dictionary**.
///
/// The dictionary is not emptied between the frames, so the second frame
/// of a loop re-uses the strings the first one taught the encoder: the
/// file is roughly a third of the size of [`encode`]'s on the parade's
/// own art. The price is that the file is **not** readable by the decoders
/// in the wild: the `gif` 0.14 crate and the browsers reset their code
/// table at every frame, so they stop at the second one. Only
/// [`decode`], which keeps the table across the frames as well, reads it.
pub fn encode_with_shared_dictionary(
    frames: &[GifFrame],
    delay_cs: u16,
) -> Result<Vec<u8>, GifError> {
    encode_with(frames, delay_cs, true)
}

/// The shared body of the two encoders.
///
/// The frames are validated **once** and the size passed through — the
/// walk used to run again inside [`quantize`] (audit 2026-10-23 F-158).
fn encode_with(
    frames: &[GifFrame],
    delay_cs: u16,
    share_dictionary: bool,
) -> Result<Vec<u8>, GifError> {
    let (width, height) = validate(frames)?;
    let quantized = quantize_checked(frames, width, height)?;
    let mut out = Vec::new();
    write_header(&mut out, width, height, &quantized.palette);
    write_loop_extension(&mut out);
    if share_dictionary {
        write_comment(&mut out, SHARED_MARKER);
    }
    let mut lzw = LzwWriter::new(share_dictionary);
    for plane in &quantized.planes {
        write_graphic_control(&mut out, delay_cs);
        write_image_descriptor(&mut out, width, height);
        lzw.write_plane(plane);
        write_sub_blocks(&mut out, &lzw.take_bytes());
    }
    out.push(TRAILER);
    Ok(out)
}

/// Write the header, the logical screen descriptor and the global table.
fn write_header(out: &mut Vec<u8>, width: u32, height: u32, palette: &[[u8; 3]]) {
    out.extend_from_slice(HEADER);
    out.extend_from_slice(&(width as u16).to_le_bytes());
    out.extend_from_slice(&(height as u16).to_le_bytes());
    let size_bits = table_size_bits(palette.len());
    // The global table flag, eight bits per primary, and the table size.
    out.push(0x80 | 0x70 | size_bits);
    out.push(0); // the background colour index
    out.push(0); // the pixel aspect ratio
    for entry in palette {
        out.extend_from_slice(entry);
    }
    // The table is padded to its power of two with the reserved colour.
    for _ in palette.len()..(1usize << (size_bits + 1)) {
        out.extend_from_slice(&TRANSPARENT_RGB);
    }
}

/// The size field of a colour table: the table holds `2^(bits+1)` entries.
fn table_size_bits(entries: usize) -> u8 {
    let mut bits = 0u8;
    while (1usize << (bits + 1)) < entries {
        bits += 1;
    }
    bits
}

/// Write the NETSCAPE2.0 extension: the animation loops forever.
fn write_loop_extension(out: &mut Vec<u8>) {
    out.push(EXTENSION);
    out.push(APPLICATION);
    out.push(NETSCAPE_APP.len() as u8);
    out.extend_from_slice(NETSCAPE_APP);
    out.push(3);
    out.push(1);
    out.extend_from_slice(&0u16.to_le_bytes());
    out.push(0);
}

/// Write a comment extension.
///
/// A comment is a legal GIF block that every decoder skips, so it can
/// carry this module's private marker without making the file unusual.
fn write_comment(out: &mut Vec<u8>, text: &[u8]) {
    out.push(EXTENSION);
    out.push(COMMENT);
    for chunk in text.chunks(MAX_SUB_BLOCK) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out.push(0);
}

/// Write the graphic control extension: the delay and the transparent
/// index.
fn write_graphic_control(out: &mut Vec<u8>, delay_cs: u16) {
    out.push(EXTENSION);
    out.push(GRAPHIC_CONTROL);
    out.push(4);
    // The transparency flag and the "do not dispose" disposal method.
    out.push(0x01 | 0x04);
    out.extend_from_slice(&delay_cs.to_le_bytes());
    out.push(TRANSPARENT_INDEX);
    out.push(0);
}

/// Write the image descriptor of a full-screen frame.
fn write_image_descriptor(out: &mut Vec<u8>, width: u32, height: u32) {
    out.push(IMAGE_DESCRIPTOR);
    out.extend_from_slice(&0u16.to_le_bytes()); // left
    out.extend_from_slice(&0u16.to_le_bytes()); // top
    out.extend_from_slice(&(width as u16).to_le_bytes());
    out.extend_from_slice(&(height as u16).to_le_bytes());
    out.push(0); // no local table, no interlace
}

/// Write the LZW data of a frame as sub-blocks.
fn write_sub_blocks(out: &mut Vec<u8>, data: &[u8]) {
    out.push(MIN_CODE_SIZE);
    for chunk in data.chunks(MAX_SUB_BLOCK) {
        out.push(chunk.len() as u8);
        out.extend_from_slice(chunk);
    }
    out.push(0);
}

// ---------------------------------------------------------------------------
// The reader
// ---------------------------------------------------------------------------

/// A cursor over the bytes of a GIF.
struct Reader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Reader<'a> {
    /// A reader over `bytes`.
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    /// Take the next `count` bytes.
    fn take(&mut self, count: usize) -> Result<&'a [u8], GifError> {
        let end = self.position.checked_add(count).ok_or_else(truncated)?;
        let slice = self.bytes.get(self.position..end).ok_or_else(truncated)?;
        self.position = end;
        Ok(slice)
    }

    /// Read one byte.
    fn u8(&mut self) -> Result<u8, GifError> {
        Ok(self.take(1)?[0])
    }

    /// Read a little-endian `u16`.
    fn u16(&mut self) -> Result<u16, GifError> {
        let slice = self.take(2)?;
        Ok(u16::from_le_bytes([slice[0], slice[1]]))
    }

    /// Check the signature and step over it.
    fn expect_header(&mut self) -> Result<(), GifError> {
        let header = self.bytes.get(..6).ok_or_else(truncated)?;
        if header != &HEADER[..] {
            return Err(GifError::Format(
                "the file does not start with GIF89a".to_string(),
            ));
        }
        self.position = HEADER.len();
        Ok(())
    }
}

/// The error of a file that ends in the middle of something.
fn truncated() -> GifError {
    GifError::Format("the file ends in the middle of a block".to_string())
}

/// The bit reader of an LZW stream: the codes are packed least
/// significant bit first.
struct BitReader<'a> {
    bytes: &'a [u8],
    position: usize,
    accumulator: u32,
    bits: u32,
}

impl<'a> BitReader<'a> {
    /// A reader over `bytes`.
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            position: 0,
            accumulator: 0,
            bits: 0,
        }
    }

    /// Read `width` bits, or `None` when the data runs out.
    fn read(&mut self, width: u32) -> Option<u32> {
        while self.bits < width {
            let byte = *self.bytes.get(self.position)?;
            self.position += 1;
            self.accumulator |= (byte as u32) << self.bits;
            self.bits += 8;
        }
        let code = self.accumulator & ((1 << width) - 1);
        self.accumulator >>= width;
        self.bits -= width;
        Some(code)
    }
}

// ---------------------------------------------------------------------------
// The LZW decoder
// ---------------------------------------------------------------------------

/// The GIF-flavoured LZW decoder.
///
/// The dictionary may **persist across the frames**: a frame's data can
/// continue the code stream of the one before it, and then only the
/// pending code is dropped at the start of a frame. A file whose frames
/// are streams of their own is read the way the decoders in the wild read
/// it: the table and the code width are reset at every frame.
struct LzwDecoder {
    min_code_size: u8,
    code_size: u32,
    next_code: u32,
    clear_code: u32,
    end_code: u32,
    /// The prefix of every dictionary entry.
    prefix: Vec<u16>,
    /// The first byte of every dictionary entry.
    first: Vec<u8>,
    /// The code that was read before the one being read.
    previous: Option<u32>,
}

impl LzwDecoder {
    /// A decoder for a 256-colour table.
    fn new(min_code_size: u8) -> Self {
        let clear_code = 1u32 << min_code_size;
        Self {
            min_code_size,
            code_size: min_code_size as u32 + 1,
            next_code: clear_code + 2,
            clear_code,
            end_code: clear_code + 1,
            prefix: vec![0; MAX_LZW_CODE as usize],
            first: vec![0; MAX_LZW_CODE as usize],
            previous: None,
        }
    }

    /// Decode one frame's data into the indices of its pixels.
    ///
    /// `share_dictionary` says whether the frame continues the code stream
    /// of the one before it.
    fn decode_frame(
        &mut self,
        data: &[u8],
        min_code_size: u8,
        expected: usize,
        share_dictionary: bool,
    ) -> Result<Vec<u8>, GifError> {
        // The width of the clear code is `1 << min_code_size`; a byte
        // outside 2..=8 is a corrupt file, not a table this decoder can
        // build (a shift of 32 or more panics in debug).
        if !(2..=8).contains(&min_code_size) {
            return Err(GifError::Format(format!(
                "the frame's minimum code size is {min_code_size}, not 2..=8"
            )));
        }
        if min_code_size != self.min_code_size {
            self.reset(min_code_size);
        }
        // The pending code does not survive a frame boundary; the
        // dictionary does — unless the file says every frame is a stream
        // of its own, and then the table and the width start over too.
        self.previous = None;
        if !share_dictionary {
            self.reset_table();
        }
        let mut reader = BitReader::new(data);
        let mut out = Vec::with_capacity(expected);
        while let Some(code) = reader.read(self.code_size) {
            if code == self.clear_code {
                self.clear_dictionary();
                continue;
            }
            if code == self.end_code {
                break;
            }
            self.emit(code, &mut out)?;
        }
        // The picture's size is the descriptor's, not the data's: a short
        // frame is padded and a long one is cut.
        out.resize(expected, TRANSPARENT_INDEX);
        Ok(out)
    }

    /// Expand one code into the output and teach the dictionary.
    fn emit(&mut self, code: u32, out: &mut Vec<u8>) -> Result<(), GifError> {
        let mut stack: Vec<u8> = Vec::new();
        let first = self.expand(code, &mut stack)?;
        for &byte in stack.iter().rev() {
            out.push(byte);
        }
        self.remember(code, first);
        Ok(())
    }

    /// Teach the dictionary the string that ends with `code`.
    ///
    /// The first code of a frame has no predecessor, so it starts the
    /// dictionary instead of extending it.
    fn remember(&mut self, code: u32, first: u8) {
        let Some(previous) = self.previous else {
            self.previous = Some(code);
            return;
        };
        // A stream that keeps coding past a full table (a deferred-clear
        // encoder, or one flipped bit) must not index `prefix[4096]`: the
        // table simply stops growing, as a decoder is allowed to do.
        if self.next_code as usize >= self.prefix.len() {
            self.previous = Some(code);
            return;
        }
        self.prefix[self.next_code as usize] = previous as u16;
        self.first[self.next_code as usize] = first;
        self.next_code += 1;
        // The width grows when the next code no longer fits in it.
        if self.next_code == (1 << self.code_size) && self.code_size < 12 {
            self.code_size += 1;
        }
        self.previous = Some(code);
    }

    /// Expand a code onto `stack` (last byte first) and return its first
    /// byte.
    fn expand(&self, code: u32, stack: &mut Vec<u8>) -> Result<u8, GifError> {
        if code < 256 {
            stack.push(code as u8);
            return Ok(code as u8);
        }
        if code < self.next_code {
            let first = self.first[code as usize];
            self.push_chain(code, stack);
            return Ok(first);
        }
        // The code is the entry that is about to be defined: its string is
        // the previous string plus that string's own first byte.
        let previous = self.previous.ok_or_else(|| {
            GifError::Format("the code stream starts with an undefined code".to_string())
        })?;
        let first = self.first_byte(previous);
        self.push_chain(previous, stack);
        stack.push(first);
        Ok(first)
    }

    /// The first byte of a code's string.
    fn first_byte(&self, code: u32) -> u8 {
        if code < 256 {
            code as u8
        } else {
            self.first[code as usize]
        }
    }

    /// Push the bytes of a code's string onto the stack, last byte first.
    fn push_chain(&self, code: u32, stack: &mut Vec<u8>) {
        let mut current = code;
        while current >= 256 {
            stack.push(self.first[current as usize]);
            current = self.prefix[current as usize] as u32;
        }
        stack.push(current as u8);
    }

    /// Forget the dictionary and go back to the initial code width.
    fn clear_dictionary(&mut self) {
        self.next_code = self.clear_code + 2;
        self.code_size = self.min_code_size as u32 + 1;
        self.previous = None;
    }

    /// Forget the dictionary without touching the pending code.
    fn reset_table(&mut self) {
        self.next_code = self.clear_code + 2;
        self.code_size = self.min_code_size as u32 + 1;
    }

    /// Start again with another minimum code size.
    fn reset(&mut self, min_code_size: u8) {
        self.min_code_size = min_code_size;
        self.clear_code = 1u32 << min_code_size;
        self.end_code = self.clear_code + 1;
        self.clear_dictionary();
    }
}

// ---------------------------------------------------------------------------
// The frame reader
// ---------------------------------------------------------------------------

/// The graphic control extension that governs the frame that follows it.
#[derive(Debug, Clone, Copy, Default)]
struct Control {
    /// The frame's delay in hundredths of a second.
    delay_cs: u16,
    /// The palette index that is not drawn, when there is one.
    transparent_index: Option<u8>,
    /// The disposal method: 0 and 1 keep, 2 clears, 3 restores.
    disposal: u8,
}

/// Decode a GIF back into RGBA frames (the test decoder; it also proves the
/// writer and the reader agree).
pub fn decode(bytes: &[u8]) -> Result<DecodedGif, GifError> {
    let mut reader = Reader::new(bytes);
    reader.expect_header()?;
    let (width, height, palette) = read_screen(&mut reader)?;
    // The canvas is allocated from the header, so an absurd size must be
    // refused **before** the allocator sees it: two u16 dimensions allow
    // a ~17 GB request.
    let pixels = u64::from(width) * u64::from(height);
    if pixels == 0 || pixels > MAX_DECODE_PIXELS {
        return Err(GifError::Format(format!(
            "the screen is {width}x{height}, over the decode limit"
        )));
    }
    let screen = Screen { width, height };
    let mut canvas = vec![0u8; (width as usize) * (height as usize) * 4];
    let mut lzw = LzwDecoder::new(MIN_CODE_SIZE);
    let mut frames = Vec::new();
    let mut control = Control::default();
    let mut share_dictionary = false;
    loop {
        let marker = reader.u8()?;
        match marker {
            TRAILER => break,
            EXTENSION => {
                if let Some(found) = read_extension(&mut reader, &mut share_dictionary)? {
                    control = found;
                }
            }
            IMAGE_DESCRIPTOR => {
                let frame = read_frame(
                    &mut reader,
                    &mut lzw,
                    &palette,
                    &mut canvas,
                    &screen,
                    &control,
                    share_dictionary,
                )?;
                frames.push(frame);
            }
            other => {
                return Err(GifError::Format(format!(
                    "unexpected block 0x{other:02X} in the file"
                )))
            }
        }
    }
    Ok(DecodedGif {
        width,
        height,
        frames,
    })
}

/// Read the logical screen descriptor and the global colour table.
fn read_screen(reader: &mut Reader) -> Result<(u32, u32, Vec<[u8; 3]>), GifError> {
    let width = reader.u16()? as u32;
    let height = reader.u16()? as u32;
    let packed = reader.u8()?;
    reader.u8()?; // the background colour index
    reader.u8()?; // the pixel aspect ratio
    let palette = if packed & 0x80 != 0 {
        read_table(reader, packed & 0x07)?
    } else {
        Vec::new()
    };
    Ok((width, height, palette))
}

/// Read a colour table of `2^(bits+1)` entries.
fn read_table(reader: &mut Reader, size_bits: u8) -> Result<Vec<[u8; 3]>, GifError> {
    let count = 1usize << (size_bits + 1);
    let bytes = reader.take(count * 3)?;
    Ok(bytes
        .chunks_exact(3)
        .map(|chunk| [chunk[0], chunk[1], chunk[2]])
        .collect())
}

/// Read one extension.
///
/// Only the graphic control one carries frame information; a comment may
/// carry the marker of a shared-dictionary file.
fn read_extension(
    reader: &mut Reader,
    share_dictionary: &mut bool,
) -> Result<Option<Control>, GifError> {
    let label = reader.u8()?;
    if label == GRAPHIC_CONTROL {
        return Ok(Some(read_control(reader)?));
    }
    if label == COMMENT {
        let text = read_sub_blocks(reader)?;
        *share_dictionary |= text == SHARED_MARKER;
        return Ok(None);
    }
    skip_sub_blocks(reader)?;
    Ok(None)
}

/// Read the graphic control extension.
fn read_control(reader: &mut Reader) -> Result<Control, GifError> {
    let size = reader.u8()?;
    if size != 4 {
        return Err(GifError::Format(
            "the graphic control extension is not four bytes long".to_string(),
        ));
    }
    let packed = reader.u8()?;
    let delay_cs = reader.u16()?;
    let transparent = reader.u8()?;
    let terminator = reader.u8()?;
    if terminator != 0 {
        return Err(GifError::Format(
            "the graphic control extension is not terminated".to_string(),
        ));
    }
    let transparent_index = if packed & 0x01 != 0 {
        Some(transparent)
    } else {
        None
    };
    Ok(Control {
        delay_cs,
        transparent_index,
        disposal: (packed >> 2) & 0x07,
    })
}

/// Read the data of a frame's sub-blocks.
fn read_sub_blocks(reader: &mut Reader) -> Result<Vec<u8>, GifError> {
    let mut data = Vec::new();
    loop {
        let length = reader.u8()?;
        if length == 0 {
            return Ok(data);
        }
        data.extend_from_slice(reader.take(length as usize)?);
    }
}

/// Skip the data of an extension.
fn skip_sub_blocks(reader: &mut Reader) -> Result<(), GifError> {
    loop {
        let length = reader.u8()?;
        if length == 0 {
            return Ok(());
        }
        reader.take(length as usize)?;
    }
}

/// The logical screen of the file.
#[derive(Debug, Clone, Copy)]
struct Screen {
    width: u32,
    height: u32,
}

/// Where a frame sits on the screen and how big it is.
#[derive(Debug, Clone, Copy)]
struct Placement {
    left: u32,
    top: u32,
    width: u32,
    height: u32,
}

/// Everything one frame's drawing needs.
///
/// The parameters are grouped because the drawing helpers would otherwise
/// carry nine of them around.
struct FrameView<'a> {
    /// The colour table the frame's indices point into.
    table: &'a [[u8; 3]],
    /// The frame's indices, row-major.
    indices: &'a [u8],
    /// Where the frame sits.
    placement: Placement,
    /// The index that is not drawn.
    transparent: Option<u8>,
    /// Whether the rows are stored interlaced.
    interlaced: bool,
}

/// Read one frame: its descriptor, its data and its place on the canvas.
fn read_frame(
    reader: &mut Reader,
    lzw: &mut LzwDecoder,
    palette: &[[u8; 3]],
    canvas: &mut [u8],
    screen: &Screen,
    control: &Control,
    share_dictionary: bool,
) -> Result<DecodedFrame, GifError> {
    let left = reader.u16()? as u32;
    let top = reader.u16()? as u32;
    let width = reader.u16()? as u32;
    let height = reader.u16()? as u32;
    let packed = reader.u8()?;
    // The frame's colour table, or the global one borrowed: a local
    // table **cloned the global palette on every frame** before, which
    // is one 768-byte copy per frame for nothing (audit 2026-10-23
    // F-158). The local table, when there is one, is owned; the global
    // one is borrowed.
    let local_table = if packed & 0x80 != 0 {
        Some(read_table(reader, packed & 0x07)?)
    } else {
        None
    };
    let table: &[[u8; 3]] = match &local_table {
        Some(local) => local,
        None => palette,
    };
    let min_code_size = reader.u8()?;
    let data = read_sub_blocks(reader)?;
    let indices = lzw.decode_frame(
        &data,
        min_code_size,
        (width as usize) * (height as usize),
        share_dictionary,
    )?;
    let placement = Placement {
        left,
        top,
        width,
        height,
    };
    let view = FrameView {
        table,
        indices: &indices,
        placement,
        transparent: control.transparent_index,
        interlaced: packed & 0x40 != 0,
    };
    // "Restore to previous" needs the canvas as it was before the frame.
    let saved = match control.disposal {
        3 => Some(canvas.to_vec()),
        _ => None,
    };
    compose(canvas, screen, &view);
    apply_disposal(canvas, screen, control.disposal, &placement, saved);
    Ok(DecodedFrame {
        delay_cs: control.delay_cs,
        rgba: canvas.to_vec(),
    })
}

/// Draw one frame's indices onto the canvas.
fn compose(canvas: &mut [u8], screen: &Screen, view: &FrameView) {
    for stored in 0..view.placement.height {
        let target = if view.interlaced {
            interlaced_row(stored, view.placement.height)
        } else {
            stored
        };
        compose_row(canvas, screen, view, stored, view.placement.top + target);
    }
}

/// Draw one row of a frame onto the canvas.
fn compose_row(canvas: &mut [u8], screen: &Screen, view: &FrameView, stored: u32, target: u32) {
    let width = view.placement.width;
    for column in 0..width {
        let offset = stored as usize * width as usize + column as usize;
        let Some(index) = view.indices.get(offset) else {
            continue;
        };
        if Some(*index) == view.transparent {
            continue;
        }
        let Some(colour) = view.table.get(*index as usize) else {
            continue;
        };
        let x = view.placement.left + column;
        if x >= screen.width {
            continue;
        }
        let pixel = (target as usize * screen.width as usize + x as usize) * 4;
        let Some(slot) = canvas.get_mut(pixel..pixel + 4) else {
            continue;
        };
        slot.copy_from_slice(&[colour[0], colour[1], colour[2], 255]);
    }
}

/// The row a stored row maps to in an interlaced image.
fn interlaced_row(mut index: u32, height: u32) -> u32 {
    for (start, step) in [(0u32, 8u32), (4, 8), (2, 4), (1, 2)] {
        if height <= start {
            continue;
        }
        let count = (height - start).div_ceil(step);
        if index < count {
            return start + index * step;
        }
        index -= count;
    }
    index
}

/// Apply the disposal method of the frame that was just drawn.
fn apply_disposal(
    canvas: &mut [u8],
    screen: &Screen,
    disposal: u8,
    placement: &Placement,
    saved: Option<Vec<u8>>,
) {
    match disposal {
        2 => clear_rect(canvas, screen, placement),
        3 => {
            if let Some(previous) = saved {
                canvas.copy_from_slice(&previous);
            }
        }
        _ => {}
    }
}

/// Clear a rectangle of the canvas: the "restore to background" disposal.
fn clear_rect(canvas: &mut [u8], screen: &Screen, placement: &Placement) {
    if placement.left >= screen.width {
        return;
    }
    for row in 0..placement.height {
        let y = placement.top + row;
        if y >= screen.height {
            continue;
        }
        let start = (y as usize * screen.width as usize + placement.left as usize) * 4;
        let count = placement.width.min(screen.width - placement.left) as usize * 4;
        if let Some(pixel) = canvas.get_mut(start..start + count) {
            pixel.fill(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame with a smooth three-way ramp: every pixel is its own
    /// colour, so the quantiser has real work to do.
    fn ramp_frame(width: u32, height: u32) -> GifFrame {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            ramp_row(&mut rgba, y, width);
        }
        GifFrame {
            width,
            height,
            rgba,
        }
    }

    /// Append one row of the ramp.
    fn ramp_row(rgba: &mut Vec<u8>, y: u32, width: u32) {
        for x in 0..width {
            let colour = [(x * 8) as u8, (y * 10) as u8, (x * 5 + y * 7) as u8];
            rgba.extend_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
    }

    #[test]
    fn the_ramp_is_pre_seeded_into_the_palette() {
        // The sky's smooth vertical ramp used to band, because the median
        // cut spent its boxes on the art (audit 2026-10-23 F-148). The
        // ramp's own colours are permanent slots now: the palette starts
        // with them, in ramp order, and the exact column colours are in.
        let frame = ramp_frame(32, 96);
        let quantized = quantize(std::slice::from_ref(&frame)).expect("quantize");
        // The reserved slot first, then up to RAMP_SEED_SLOTS seeds.
        assert!(quantized.palette.len() <= 256, "the palette must fit");
        assert!(
            quantized.palette.len() > 48,
            "the seeds must leave room for the cut"
        );
        // The first seed is the frame's own first pixel (column 0, row 0),
        // exactly, and the seeds come out in ramp order. The last seed is
        // step 47 of 48 over 96 rows, so row 94 — and the helper's
        // channel math wraps at the byte (94*10 = 940 -> 172).
        assert_eq!(quantized.palette[1], [0, 0, 0], "the top seed is exact");
        let last_seed = quantized.palette[RAMP_SEED_SLOTS];
        assert_eq!(last_seed, [0, 172, 146], "the seeds walk the ramp in order");
        // And the choice is reproducible.
        let again = quantize(std::slice::from_ref(&frame)).expect("quantize");
        assert_eq!(quantized, again, "the palette must not move between runs");
    }

    #[test]
    fn a_dusk_gradient_lands_on_the_ramp_slots() {
        // A narrow vertical gradient between two nearby blues — the dusk
        // band that used to band — must find enough distinct palette
        // entries that consecutive rows do not all collapse onto one.
        let mut rgba = Vec::new();
        let width = 4u32;
        let height = 64u32;
        for y in 0..height {
            let colour = [20u8, 30, (30 + y) as u8];
            for _ in 0..width {
                rgba.extend_from_slice(&[colour[0], colour[1], colour[2], 255]);
            }
        }
        let frame = GifFrame {
            width,
            height,
            rgba,
        };
        let quantized = quantize(std::slice::from_ref(&frame)).expect("quantize");
        let plane = &quantized.planes[0];
        let mut used = plane.clone();
        used.sort_unstable();
        used.dedup();
        assert!(
            used.len() >= 8,
            "the gradient must spread over the ramp slots, got {} entries",
            used.len()
        );
    }

    /// A frame with the sky's own shape: one colour per row, and more
    /// rows than the palette has entries.
    fn sky_frame(width: u32, height: u32) -> GifFrame {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            sky_row(&mut rgba, y, width, height);
        }
        GifFrame {
            width,
            height,
            rgba,
        }
    }

    /// Append one row of the sky: a smooth ramp with a fractional step,
    /// so the row's colour is its own.
    fn sky_row(rgba: &mut Vec<u8>, y: u32, width: u32, height: u32) {
        let colour = [
            (y * 200 / height.max(1)) as u8,
            (y * 150 / height.max(1)) as u8,
            (y * 100 / height.max(1)) as u8,
        ];
        for _ in 0..width {
            rgba.extend_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
    }

    /// The number of index changes **inside** the rows of a plane.
    ///
    /// A dithered row changes its index on every other pixel; a snapped
    /// one does not change it at all.
    fn count_row_changes(plane: &[u8], width: u32, height: u32) -> usize {
        let mut changes = 0;
        for y in 0..height as usize {
            let row = &plane[y * width as usize..(y + 1) * width as usize];
            changes += row.windows(2).filter(|pair| pair[0] != pair[1]).count();
        }
        changes
    }

    /// A frame of one solid colour.
    fn flat_frame(width: u32, height: u32, colour: [u8; 3]) -> GifFrame {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for _ in 0..(width * height) {
            rgba.extend_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
        GifFrame {
            width,
            height,
            rgba,
        }
    }

    /// A frame of noise: the worst case for the compressor.
    fn noisy_frame(width: u32, height: u32, seed: u32) -> GifFrame {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        let mut state = seed | 1;
        for _ in 0..(width * height) {
            state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
            let colour = [(state >> 16) as u8, (state >> 8) as u8, state as u8];
            rgba.extend_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
        GifFrame {
            width,
            height,
            rgba,
        }
    }

    /// The worst and the mean error between two RGBA buffers, per channel.
    fn channel_errors(original: &[u8], decoded: &[u8]) -> (i32, f64) {
        let differences: Vec<i32> = original
            .chunks_exact(4)
            .zip(decoded.chunks_exact(4))
            .flat_map(|(a, b)| {
                (0..3).map(move |channel| (a[channel] as i32 - b[channel] as i32).abs())
            })
            .collect();
        let worst = differences.iter().copied().max().unwrap_or(0);
        let mean = if differences.is_empty() {
            0.0
        } else {
            differences.iter().sum::<i32>() as f64 / differences.len() as f64
        };
        (worst, mean)
    }

    /// The worst difference between one colour and another, per channel.
    fn colour_difference(one: [u8; 3], other: [u8; 3]) -> i32 {
        let mut worst = 0i32;
        for channel in 0..3 {
            worst = worst.max((one[channel] as i32 - other[channel] as i32).abs());
        }
        worst
    }

    #[test]
    fn a_single_frame_round_trips() {
        let frame = ramp_frame(32, 24);
        let bytes = encode(std::slice::from_ref(&frame), 8).expect("encode");
        let decoded = decode(&bytes).expect("decode");
        assert_eq!(decoded.width, 32);
        assert_eq!(decoded.height, 24);
        assert_eq!(decoded.frames.len(), 1);
        assert_eq!(decoded.frames[0].delay_cs, 8);
        assert_eq!(decoded.frames[0].rgba.len(), frame.rgba.len());
        let (worst, mean) = channel_errors(&frame.rgba, &decoded.frames[0].rgba);
        assert!(worst <= 48, "the worst channel error is {worst}");
        assert!(mean <= 12.0, "the mean channel error is {mean:.1}");
    }

    #[test]
    fn a_colour_within_the_dead_zone_keeps_its_exact_index() {
        // A hand-made palette: the first pixel is three units from the
        // first entry — inside the dead zone — and the second pixel sits
        // exactly between the two entries. Without the dead zone the
        // first pixel would diffuse its residual and drag the second one
        // across the tie; with it, the second pixel keeps its own side.
        let palette = vec![[0, 0, 0], [100, 100, 100], [200, 200, 200]];
        let mut rgba = Vec::new();
        rgba.extend_from_slice(&[103, 100, 100, 255]);
        rgba.extend_from_slice(&[150, 150, 150, 255]);
        let frame = GifFrame {
            width: 2,
            height: 1,
            rgba,
        };
        let mut quantiser = Quantiser::new(&palette);
        let plane = dither_frame(&frame, &mut quantiser, 2, 1);
        assert_eq!(plane[0], 1, "the pixel snaps to the nearest entry");
        assert_eq!(
            plane[1], 1,
            "the dead zone must stop the error from spreading"
        );
    }

    #[test]
    fn a_dithered_gradient_keeps_its_runs() {
        // A palette with two entries a few units apart, and a row that
        // sits exactly on the tie between them. The dead zone keeps the
        // row on one index; without it the residual would make the row
        // alternate between the two, and the runs the compressor lives on
        // would be gone.
        let palette = vec![[0, 0, 0], [100, 100, 100], [106, 100, 100]];
        let width = 64u32;
        let mut rgba = Vec::new();
        for _ in 0..width {
            rgba.extend_from_slice(&[103, 100, 100, 255]);
        }
        let frame = GifFrame {
            width,
            height: 1,
            rgba,
        };
        let mut quantiser = Quantiser::new(&palette);
        let plane = dither_frame(&frame, &mut quantiser, width, 1);
        let changes = plane.windows(2).filter(|pair| pair[0] != pair[1]).count();
        assert_eq!(changes, 0, "the row must keep its run");
    }

    #[test]
    fn a_smooth_gradient_compresses() {
        // The sky: 300 rows of their own colour, and a palette of 255.
        // The colours that do not fit are merged, and the residual of a
        // merge is invisible — so the dead zone snaps them and the rows
        // keep their runs.
        let (width, height) = (64u32, 300u32);
        let frame = sky_frame(width, height);
        let quantized = quantize(std::slice::from_ref(&frame)).expect("quantize");
        let plane = &quantized.planes[0];
        let changes = count_row_changes(plane, width, height);
        assert_eq!(
            changes, 0,
            "the rows must keep their runs, {changes} index changes"
        );
        // And the file must be small: the sky is a handful of codes per
        // row, not a picture per pixel.
        let bytes = encode(&[frame], 5).expect("encode");
        assert!(
            bytes.len() < 8192,
            "the sky must compress, the file is {} bytes",
            bytes.len()
        );
    }

    #[test]
    fn a_flat_colour_survives_exactly() {
        let colour = [64, 128, 192];
        let frame = flat_frame(16, 16, colour);
        let bytes = encode(&[frame], 4).expect("encode");
        let decoded = decode(&bytes).expect("decode");
        let rgba = &decoded.frames[0].rgba;
        assert_eq!(rgba.len(), 16 * 16 * 4);
        for pixel in rgba.chunks_exact(4) {
            assert_eq!(
                colour_difference([pixel[0], pixel[1], pixel[2]], colour),
                0,
                "the flat colour must survive exactly, got {:?}",
                pixel
            );
            assert_eq!(pixel[3], 255, "the picture must stay opaque");
        }
    }

    #[test]
    fn the_palette_holds_at_most_256_colours() {
        let frames = vec![noisy_frame(48, 32, 7), noisy_frame(48, 32, 99)];
        let quantized = quantize(&frames).expect("quantize");
        assert!(
            quantized.palette.len() <= 256,
            "the palette holds {} colours",
            quantized.palette.len()
        );
        assert_eq!(quantized.planes.len(), 2);
        assert_eq!(quantized.planes[0].len(), (48 * 32) as usize);
        assert_eq!(quantized.planes[1].len(), (48 * 32) as usize);
        for plane in &quantized.planes {
            assert!(
                plane.iter().all(|&index| index != TRANSPARENT_INDEX),
                "the reserved slot must stay free"
            );
        }
    }

    #[test]
    fn the_output_is_byte_identical_twice() {
        let frames = vec![
            ramp_frame(24, 18),
            ramp_frame(24, 18),
            flat_frame(24, 18, [9, 8, 7]),
        ];
        let first = encode(&frames, 7).expect("encode");
        let second = encode(&frames, 7).expect("encode");
        assert_eq!(first, second, "the export must be reproducible");
    }

    #[test]
    fn the_animation_loops_forever() {
        let frames = vec![
            flat_frame(8, 8, [10, 20, 30]),
            flat_frame(8, 8, [40, 50, 60]),
        ];
        let bytes = encode(&frames, 5).expect("encode");
        let at = bytes
            .windows(11)
            .position(|window| window == NETSCAPE_APP)
            .expect("the NETSCAPE2.0 extension must be there");
        // The sub-block that follows carries the loop count: one, then
        // the count itself, which is zero — loop forever.
        assert_eq!(bytes[at + 11], 3, "the sub-block holds three bytes");
        assert_eq!(bytes[at + 12], 1, "the sub-block is the loop count");
        assert_eq!(bytes[at + 13], 0, "the loop count is zero");
        assert_eq!(bytes[at + 14], 0, "the loop count is zero");
    }

    #[test]
    fn the_shared_dictionary_file_round_trips() {
        let frames = vec![ramp_frame(24, 18), ramp_frame(24, 18)];
        let bytes = encode_with_shared_dictionary(&frames, 6).expect("encode");
        let decoded = decode(&bytes).expect("the shared file must decode");
        assert_eq!(decoded.frames.len(), 2);
        assert_eq!(decoded.frames[0].delay_cs, 6);
        let (worst, mean) = channel_errors(&frames[0].rgba, &decoded.frames[0].rgba);
        assert!(worst <= 48, "the worst channel error is {worst}");
        assert!(mean <= 12.0, "the mean channel error is {mean:.1}");
    }

    #[test]
    fn the_shared_marker_marks_the_shared_file() {
        let frames = vec![flat_frame(8, 8, [1, 2, 3])];
        let standard = encode(&frames, 5).expect("encode");
        let shared = encode_with_shared_dictionary(&frames, 5).expect("encode");
        let marked = |bytes: &[u8]| {
            bytes
                .windows(SHARED_MARKER.len())
                .any(|w| w == SHARED_MARKER)
        };
        assert!(!marked(&standard), "a normal file carries no marker");
        assert!(marked(&shared), "a shared file carries the marker");
    }

    #[test]
    fn the_dictionary_carries_across_frames() {
        let flat = flat_frame(32, 32, [20, 40, 60]);
        let bytes = encode_with_shared_dictionary(&[flat.clone(), flat.clone(), flat.clone()], 5)
            .expect("encode");
        let payloads = frame_payloads(&bytes);
        assert_eq!(payloads.len(), 3, "the file must carry three frames");
        // Every repetition of the same picture is cheaper than the one
        // before it: the strings the first frame taught the encoder are
        // still known, so the next frame needs fewer codes. Without a
        // dictionary that carried over, the three would cost the same.
        assert!(
            payloads[1] < payloads[0],
            "the second frame carries {} bytes, the first {}",
            payloads[1],
            payloads[0]
        );
        assert!(
            payloads[2] < payloads[1],
            "the third frame carries {} bytes, the second {}",
            payloads[2],
            payloads[1]
        );
    }

    #[test]
    fn every_frame_is_a_stream_of_its_own() {
        // The decoders in the wild reset their code table at every frame,
        // so each frame's data must decode on its own. A fresh decoder
        // per frame is exactly what they do.
        let frames = vec![ramp_frame(24, 18), ramp_frame(24, 18), ramp_frame(24, 18)];
        let bytes = encode(&frames, 5).expect("encode");
        let payloads = frame_payloads_of(&bytes);
        let quantized = quantize(&frames).expect("quantize");
        assert_eq!(payloads.len(), 3);
        for (index, payload) in payloads.iter().enumerate() {
            let mut decoder = LzwDecoder::new(MIN_CODE_SIZE);
            let indices = decoder
                .decode_frame(payload, MIN_CODE_SIZE, quantized.planes[index].len(), false)
                .expect("the frame must decode on its own");
            assert_eq!(indices, quantized.planes[index], "frame {index} differs");
        }
    }

    #[test]
    fn the_shared_dictionary_is_smaller_than_the_own_one() {
        let frames = vec![
            flat_frame(32, 32, [20, 40, 60]),
            flat_frame(32, 32, [20, 40, 60]),
        ];
        let own = encode(&frames, 5).expect("encode");
        let shared = encode_with_shared_dictionary(&frames, 5).expect("encode");
        assert!(
            shared.len() < own.len(),
            "the shared file is {} bytes, the own one {}",
            shared.len(),
            own.len()
        );
    }

    #[test]
    fn a_repeated_frame_is_cheaper_than_a_different_one() {
        let flat = flat_frame(32, 32, [20, 40, 60]);
        let noise = noisy_frame(32, 32, 3);
        let repeated = encode(&[flat.clone(), flat.clone()], 5).expect("encode");
        let mixed = encode(&[flat, noise], 5).expect("encode");
        assert!(
            repeated.len() < mixed.len(),
            "the repeated pair is {} bytes, the mixed pair {}",
            repeated.len(),
            mixed.len()
        );
    }

    #[test]
    fn the_frames_come_back_in_file_order() {
        let first = flat_frame(8, 8, [10, 20, 30]);
        let second = flat_frame(8, 8, [200, 100, 50]);
        let bytes = encode(&[first, second], 3).expect("encode");
        let decoded = decode(&bytes).expect("decode");
        assert_eq!(decoded.frames.len(), 2);
        let head = &decoded.frames[0].rgba[0..3];
        let tail = &decoded.frames[1].rgba[0..3];
        assert_eq!(head, &[10, 20, 30], "the first frame comes first");
        assert_eq!(tail, &[200, 100, 50], "the second frame comes second");
    }

    #[test]
    fn a_broken_file_is_refused() {
        assert!(decode(b"").is_err(), "an empty file is refused");
        assert!(decode(b"not a gif at all").is_err(), "garbage is refused");
        assert!(decode(b"GIF89a").is_err(), "a cut file is refused");
        let mut bytes = encode(&[flat_frame(8, 8, [1, 2, 3])], 5).expect("encode");
        let last = bytes.len() - 1;
        bytes[last] = 0x00;
        assert!(decode(&bytes).is_err(), "a broken trailer is refused");
    }

    #[test]
    fn a_frame_with_the_wrong_byte_count_is_refused() {
        let frame = GifFrame {
            width: 4,
            height: 4,
            rgba: vec![0; 10],
        };
        let err = encode(&[frame], 5).expect_err("the wrong byte count is refused");
        assert!(
            matches!(
                err,
                GifError::FrameSize {
                    index: 0,
                    bytes: 10,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn frames_of_different_sizes_are_refused() {
        let frames = vec![flat_frame(8, 8, [1, 2, 3]), flat_frame(8, 4, [1, 2, 3])];
        let err = encode(&frames, 5).expect_err("the mismatch is refused");
        assert!(
            matches!(err, GifError::FrameSizeMismatch { index: 1, .. }),
            "{err:?}"
        );
    }

    #[test]
    fn an_empty_animation_is_refused() {
        let err = encode(&[], 5).expect_err("there is nothing to encode");
        assert!(matches!(err, GifError::NoFrames), "{err:?}");
    }

    /// A zero-sized frame is a **size** error, not a missing animation.
    #[test]
    fn a_zero_sized_frame_is_refused_as_a_size_error() {
        let frames = vec![flat_frame(0, 0, [1, 2, 3])];
        let err = encode(&frames, 5).expect_err("the empty frame is refused");
        assert!(
            matches!(err, GifError::FrameEmpty { index: 0, .. }),
            "{err:?}"
        );
    }

    /// The LZW payload length of every frame of an encoded GIF.
    ///
    /// The file's own structure is walked, because the size of a frame's
    /// data on its own is what the dictionary's persistence shows up in.
    fn frame_payloads(bytes: &[u8]) -> Vec<usize> {
        frame_payloads_of(bytes)
            .iter()
            .map(|payload| payload.len())
            .collect()
    }

    /// The LZW payload of every frame of an encoded GIF.
    fn frame_payloads_of(bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut at = 13usize;
        if bytes[10] & 0x80 != 0 {
            at += 3 * (1usize << ((bytes[10] & 0x07) + 1));
        }
        let mut payloads = Vec::new();
        while at < bytes.len() {
            let marker = bytes[at];
            at += 1;
            if marker == TRAILER {
                break;
            }
            if marker == EXTENSION {
                at = skip_extension(bytes, at);
                continue;
            }
            assert_eq!(marker, IMAGE_DESCRIPTOR, "unexpected block {marker:#04x}");
            let packed = bytes[at + 8];
            at += 9;
            if packed & 0x80 != 0 {
                at += 3 * (1usize << ((packed & 0x07) + 1));
            }
            at += 1; // the minimum code size
            let (next, payload) = take_data_blocks(bytes, at);
            at = next;
            payloads.push(payload);
        }
        payloads
    }

    /// Skip an extension and return the position behind it.
    fn skip_extension(bytes: &[u8], at: usize) -> usize {
        let label = bytes[at];
        let mut cursor = at + 1;
        if label == GRAPHIC_CONTROL {
            let size = bytes[cursor] as usize;
            return cursor + 1 + size + 1;
        }
        loop {
            let block = bytes[cursor] as usize;
            cursor += 1;
            if block == 0 {
                break;
            }
            cursor += block;
        }
        cursor
    }

    /// Skip the data blocks of a frame; return the position behind them
    /// and the data itself.
    fn take_data_blocks(bytes: &[u8], at: usize) -> (usize, Vec<u8>) {
        let mut cursor = at;
        let mut payload = Vec::new();
        while cursor < bytes.len() {
            let block = bytes[cursor] as usize;
            cursor += 1;
            if block == 0 {
                break;
            }
            payload.extend_from_slice(&bytes[cursor..cursor + block]);
            cursor += block;
        }
        (cursor, payload)
    }
}
