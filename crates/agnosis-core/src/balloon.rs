//! Balloon data: the converted sprite, the fixed-slot ring buffer, the
//! holder assignment table and the disc rasterizers.
//!
//! The sprite contract (converted picture, ring buffer, holder table) is
//! stable; the geometry helpers below rasterize the balloon circle and are
//! shared by the scene (which caches the disc cells per geometry, see
//! [`crate::scene::Scene`]).
//!
//! This module also owns the **authoring geometry** — the disc the user
//! draws a balloon in ([`balloon_authoring_geometry`]). The runtime disc
//! is elliptical in cells and circular in pixels, so the authoring disc
//! must be elliptical in cells too; a square authoring canvas is a tall
//! ellipse in pixels, and every balloon drawn in one came out 1.5x too
//! wide in the parade (audit 2026-10-20 § 3.18).
//!
//! Duck-count truth lives in [`crate::parade::DUCK_COUNT`]; this module only
//! owns the holder table into it.

/// Balloon holders, by duck slot. Balloon 0 is held by big duck #1, then small
/// #1, #3, #5 and #7. The last two small ducks hold nothing.
pub const BALLOON_HOLDER_DUCK: [usize; 5] = [0, 2, 4, 6, 8];

/// Train slots drawn with the big duck sprite (audit maintainability: one
/// owner for the big/small split, so the scene stops hard-coding `slot < 2`).
pub const BIG_DUCK_SLOTS: [usize; 2] = [0, 1];

/// A converted balloon: an ASCII image with per-cell colour.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct AsciiSprite {
    cols: usize,
    rows: usize,
    chars: Vec<char>,
    colors: Vec<[u8; 3]>,
}

impl AsciiSprite {
    pub fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            chars: vec![' '; cols * rows],
            colors: vec![[255, 255, 255]; cols * rows],
        }
    }

    /// Build from rows of characters, all in one colour. Ragged rows are
    /// padded with spaces in the same colour (test helper: the production
    /// path builds sprites cell by cell through [`AsciiSprite::from_cells`]).
    #[cfg(any(test, feature = "test-support"))]
    pub fn from_rows(rows: &[&str], color: [u8; 3]) -> Self {
        let h = rows.len();
        let w = rows.iter().map(|r| r.chars().count()).max().unwrap_or(0);
        let mut s = Self::new(w, h);
        s.colors.iter_mut().for_each(|c| *c = color);
        for (y, row) in rows.iter().enumerate() {
            stamp_sprite_row(&mut s, y, row, color);
        }
        s
    }

    pub fn from_cells(
        cols: usize,
        rows: usize,
        chars: Vec<char>,
        colors: Vec<[u8; 3]>,
    ) -> Option<Self> {
        if chars.len() != cols * rows || colors.len() != cols * rows || cols * rows == 0 {
            return None;
        }
        Some(Self {
            cols,
            rows,
            chars,
            colors,
        })
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn chars(&self) -> &[char] {
        &self.chars
    }

    pub fn colors(&self) -> &[[u8; 3]] {
        &self.colors
    }

    #[inline]
    pub fn get(&self, col: usize, row: usize) -> Option<(char, [u8; 3])> {
        if col >= self.cols || row >= self.rows {
            return None;
        }
        let i = row * self.cols + col;
        Some((self.chars[i], self.colors[i]))
    }

    #[inline]
    pub fn set(&mut self, col: usize, row: usize, ch: char, color: [u8; 3]) {
        if col >= self.cols || row >= self.rows {
            return;
        }
        let i = row * self.cols + col;
        self.chars[i] = ch;
        self.colors[i] = color;
    }

    /// True when the sprite has no inked cell.
    pub fn is_empty(&self) -> bool {
        self.chars.iter().all(|c| *c == ' ')
    }

    /// Number of inked cells (test helper).
    #[cfg(any(test, feature = "test-support"))]
    pub fn ink_count(&self) -> usize {
        self.chars.iter().filter(|c| **c != ' ').count()
    }

    /// Drop fully transparent border rows and columns. `None` when nothing is inked.
    pub fn trimmed(&self) -> Option<Self> {
        let (min_x, max_x, min_y, max_y) = self.ink_bounds()?;
        let (w, h) = (max_x - min_x + 1, max_y - min_y + 1);
        let mut out = Self::new(w, h);
        for k in 0..w * h {
            self.copy_trimmed_cell(&mut out, min_x, min_y, w, k);
        }
        Some(out)
    }

    /// Tightest inked rectangle as `(min_x, max_x, min_y, max_y)`;
    /// `None` when nothing is inked.
    fn ink_bounds(&self) -> Option<(usize, usize, usize, usize)> {
        let mut bounds: Option<(usize, usize, usize, usize)> = None;
        for y in 0..self.rows {
            self.scan_ink_row(&mut bounds, y);
        }
        bounds
    }

    /// Extend `bounds` with every inked cell of row `y`.
    fn scan_ink_row(&self, bounds: &mut Option<(usize, usize, usize, usize)>, y: usize) {
        for x in 0..self.cols {
            if self.chars[y * self.cols + x] != ' ' {
                grow_bounds(bounds, x, y);
            }
        }
    }

    /// Copy one trimmed cell (`k`-th in row-major order) from
    /// `(min_x + x, min_y + y)` to `(x, y)`. An out-of-range read (bounds
    /// drift) yields a transparent cell instead of panicking — release is
    /// `panic = "abort"` (audit robustness).
    fn copy_trimmed_cell(&self, out: &mut Self, min_x: usize, min_y: usize, w: usize, k: usize) {
        let (x, y) = (k % w, k / w);
        let (ch, c) = self
            .get(min_x + x, min_y + y)
            .unwrap_or((' ', [255, 255, 255]));
        out.set(x, y, ch, c);
    }

    /// Drop isolated single cells not orthogonally adjacent to another inked cell.
    pub fn despeckle(&mut self) {
        if self.cols < 3 || self.rows < 3 {
            return;
        }
        let original = self.chars.clone();
        for r in 0..self.rows {
            self.despeckle_row(&original, r);
        }
    }

    /// Clear isolated cells of row `r`: inked cells with no orthogonal
    /// neighbour in the pre-pass snapshot.
    fn despeckle_row(&mut self, original: &[char], r: usize) {
        for c in 0..self.cols {
            if original[r * self.cols + c] != ' '
                && !has_ink_neighbour(original, self.cols, self.rows, c, r)
            {
                self.chars[r * self.cols + c] = ' ';
            }
        }
    }
}

/// Fixed-capacity ring buffer. Slot `next_slot` is overwritten by the next push.
///
/// WHY `pub(crate)`: the buffer is an internal detail of the balloon
/// pipeline (the scene holds it), not part of the core's public API. In
/// the binary crate it was crate-private by accident of the module tree;
/// naming it here keeps that visibility in the library.
#[derive(Clone, Debug)]
pub(crate) struct RingBuffer<T> {
    items: Vec<Option<T>>,
    next: usize,
    count: usize,
}

impl<T> RingBuffer<T> {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            items: (0..capacity).map(|_| None).collect(),
            next: 0,
            count: 0,
        }
    }

    /// Number of slots in the buffer (test helper).
    #[cfg(test)]
    pub fn capacity(&self) -> usize {
        self.items.len()
    }

    /// Insert at the next slot, return the slot index written.
    pub fn push(&mut self, value: T) -> usize {
        let slot = self.next;
        self.items[slot] = Some(value);
        self.next = (self.next + 1) % self.items.len();
        self.count = (self.count + 1).min(self.items.len());
        slot
    }

    pub fn get(&self, slot: usize) -> Option<&T> {
        self.items.get(slot).and_then(|s| s.as_ref())
    }

    /// Slot the next push overwrites (test helper).
    #[cfg(test)]
    pub fn next_slot(&self) -> usize {
        self.next
    }

    /// Number of filled slots.
    #[cfg(any(test, feature = "test-support"))]
    pub fn len(&self) -> usize {
        self.count
    }

    /// Filled slots in logical order (insertion order), oldest first.
    /// A bookkeeping drift (a `None` where a filled slot is expected) is
    /// skipped with a `debug_assert` instead of panicking — release is
    /// `panic = "abort"` (audit robustness).
    ///
    /// The production path drives the buffer through `push`/`get` only, so
    /// this traversal is the tested primitive that pins the wrap-around
    /// bookkeeping (audit P1-11: keep exactly one accessor with a
    /// justification and a test, not a pile of dead code).
    #[cfg(test)]
    pub fn iter(&self) -> impl Iterator<Item = (usize, &T)> {
        let cap = self.items.len().max(1);
        let count = self.count.min(cap);
        // `count <= cap` and `cap >= 1`, so the subtraction cannot underflow.
        let start = (self.next + cap - count) % cap;
        (0..count).filter_map(move |k| {
            let slot = (start + k) % cap;
            let item = self.items.get(slot).and_then(|s| s.as_ref());
            if item.is_none() {
                debug_assert!(false, "ring buffer drift: empty slot in iter range");
            }
            item.map(|v| (slot, v))
        })
    }
}

/// True when `(c, r)` has an orthogonally adjacent inked cell in `chars`.
fn has_ink_neighbour(chars: &[char], cols: usize, rows: usize, c: usize, r: usize) -> bool {
    has_ink_at(chars, cols, rows, c.wrapping_sub(1), r)
        || has_ink_at(chars, cols, rows, c + 1, r)
        || has_ink_at(chars, cols, rows, c, r.wrapping_sub(1))
        || has_ink_at(chars, cols, rows, c, r + 1)
}

/// True when `(c, r)` is in bounds and inked in `chars` (`wrapping_sub`
/// underflows to `usize::MAX`, which fails the bounds check by design).
fn has_ink_at(chars: &[char], cols: usize, rows: usize, c: usize, r: usize) -> bool {
    r < rows && c < cols && chars[r * cols + c] != ' '
}

/// Grow an ink-bounds rectangle to include `(x, y)`; `None` starts the box.
fn grow_bounds(bounds: &mut Option<(usize, usize, usize, usize)>, x: usize, y: usize) {
    let Some((min_x, max_x, min_y, max_y)) = bounds else {
        *bounds = Some((x, x, y, y));
        return;
    };
    *min_x = (*min_x).min(x);
    *max_x = (*max_x).max(x);
    *min_y = (*min_y).min(y);
    *max_y = (*max_y).max(y);
}
/// Stamp one sprite row into `s` at row `y` (audit nesting: the cell loop
/// lives here so `from_rows` reads flat).
#[cfg(any(test, feature = "test-support"))]
fn stamp_sprite_row(s: &mut AsciiSprite, y: usize, row: &str, color: [u8; 3]) {
    for (x, ch) in row.chars().enumerate() {
        s.set(x, y, ch, color);
    }
}

/// Duck slot that holds the given balloon (test helper: the scene reads the
/// holder table directly).
#[cfg(any(test, feature = "test-support"))]
pub fn holder_duck_index(balloon_slot: usize) -> usize {
    BALLOON_HOLDER_DUCK[balloon_slot % BALLOON_HOLDER_DUCK.len()]
}

/// Balloon cell radii from the configured radius in rows and the grid cell
/// aspect: `ry = radius_rows`, `rx = ry * cell_h / cell_w` so the disc reads
/// circular (54 x 36 at the audit cell [2,3]).
pub fn balloon_radii_cells(radius_rows: u32, cell_w: u32, cell_h: u32) -> (f32, f32) {
    let ry = (radius_rows.max(1)) as f32;
    let aspect = if cell_w == 0 {
        1.0
    } else {
        cell_h as f32 / cell_w as f32
    };
    let rx = (ry * aspect).round().max(2.0);
    (rx, ry.max(1.0))
}

// ---------------------------------------------------------------------------
// The authoring geometry: the disc the user draws the balloon in
// ---------------------------------------------------------------------------

/// The authoring aspect of the balloon disc: `rx = ry * 3 / 2` **cells**.
///
/// The grid cell is taller than wide — the runtime grid derives
/// `cell_w = 2/3 * cell_h` from the row budget, and the editor's canvas
/// cells are 16 x 24 px, the same 2:3. So a disc that is 3:2 *in cells*
/// is a **circle in pixels**, on every monitor and in the editor alike.
///
/// This is the whole point of the authoring geometry: the runtime disc
/// (`balloon_radii_cells`) is elliptical in cells and circular in pixels,
/// so the authoring disc must be elliptical in cells too. A square
/// authoring canvas is a tall ellipse in pixels, and every balloon drawn
/// in it came out 1.5x too wide in the parade (audit 2026-10-20 § 3.18).
const AUTHORING_RX_PER_RY: f32 = 1.5;

/// The authoring geometry of a balloon: the disc radii in cells and the
/// frame that holds the disc.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BalloonGeometry {
    /// The horizontal radius in cells (54 at the default radius 36).
    pub rx: f32,
    /// The vertical radius in cells (36 at the default radius 36).
    pub ry: f32,
    /// The frame width in cells (`2 * rx + 1`).
    pub cols: usize,
    /// The frame height in cells (`2 * ry + 1`).
    pub rows: usize,
}

/// The authoring geometry of a balloon of `radius_rows` rows.
///
/// The disc is `rx = 1.5 * ry` cells wide, so the frame that holds it is
/// `2 * rx + 1` by `2 * ry + 1` cells — **109 x 73** at the default
/// radius 36, not the 73 x 73 square the editor used before. The frame
/// is the disc's own cell grid, so the art the user paints maps 1:1 onto
/// the runtime disc: the drawn balloon is the shape it was drawn in.
pub fn balloon_authoring_geometry(radius_rows: u32) -> BalloonGeometry {
    let ry = radius_rows.max(1);
    let rx = (ry as f32 * AUTHORING_RX_PER_RY).round().max(2.0);
    BalloonGeometry {
        rx,
        ry: ry as f32,
        cols: (2.0 * rx).round() as usize + 1,
        rows: 2 * ry as usize + 1,
    }
}

/// The active cells of a balloon's authoring frame: the authoring disc,
/// row-major over `cols * rows`.
///
/// The mask comes from [`circle_cells`] — the **same** rasterizer the
/// scene draws the balloon with — so the editor's paintable area and
/// the rendered balloon are one shape, not two that merely look alike.
pub fn balloon_authoring_mask(geometry: &BalloonGeometry) -> Vec<bool> {
    let mut mask = vec![false; geometry.cols.saturating_mul(geometry.rows)];
    for (dx, dy, _) in circle_cells(geometry.rx, geometry.ry) {
        mark_mask_cell(&mut mask, geometry.cols, geometry.rows, dx, dy);
    }
    mask
}

/// Mark the cell at the frame's centre plus `(dx, dy)`. Offsets that
/// leave the frame are dropped.
fn mark_mask_cell(mask: &mut [bool], cols: usize, rows: usize, dx: i64, dy: i64) {
    let x = (cols / 2) as i64 + dx;
    let y = (rows / 2) as i64 + dy;
    if x < 0 || y < 0 || x as usize >= cols || y as usize >= rows {
        return;
    }
    mask[y as usize * cols + x as usize] = true;
}

/// True when `cols x rows` is an authoring disc frame: an odd-by-odd
/// frame whose radii satisfy `rx = 1.5 * ry`.
///
/// A balloon document must have such a frame. The old square frame
/// (73 x 73) is the shape that stretched the art 1.5x, so it is refused
/// rather than quietly drawn distorted.
pub fn is_authoring_disc(cols: usize, rows: usize) -> bool {
    if cols == 0 || rows == 0 || cols.is_multiple_of(2) || rows.is_multiple_of(2) {
        return false;
    }
    let rx = ((cols - 1) / 2) as f32;
    let ry = ((rows - 1) / 2) as f32;
    rx == (ry * AUTHORING_RX_PER_RY).round()
}

// ---------------------------------------------------------------------------
// Circle rasterization: the disc cells are cached per geometry by the scene,
// so `circle_cells` runs once per (rx, ry), not once per frame.
// ---------------------------------------------------------------------------

/// Cells of a circle in a 2:1 character grid as `(dx, dy, d)`: integer offsets
/// from the centre plus normalized elliptical distance `d` in `0..=1` with
/// `d = sqrt((dx/rx)^2 + (dy/ry)^2)`. Only cells with `d <= 1` are returned,
/// so callers can iterate without re-checking. Non-positive radii yield an
/// empty set.
pub fn circle_cells(rx: f32, ry: f32) -> Vec<(i64, i64, f32)> {
    if !rx.is_finite() || !ry.is_finite() || rx <= 0.0 || ry <= 0.0 {
        return Vec::new();
    }
    let mut out = Vec::new();
    for dy in (-(ry.ceil() as i64))..=(ry.ceil() as i64) {
        scan_disc_row(&mut out, rx, ry, dy);
    }
    out
}

/// Push every in-disc cell of row `dy` (`d <= 1`) into `out`, so
/// [`circle_cells`] reads flat (audit nesting).
fn scan_disc_row(out: &mut Vec<(i64, i64, f32)>, rx: f32, ry: f32, dy: i64) {
    for dx in (-(rx.ceil() as i64))..=(rx.ceil() as i64) {
        let nx = dx as f32 / rx;
        let ny = dy as f32 / ry;
        let d = (nx * nx + ny * ny).sqrt();
        if d <= 1.0 {
            out.push((dx, dy, d));
        }
    }
}

/// Stamp a balloon disc onto the grid. `pixel_at(dx, dy, d)` decides each
/// in-circle cell; returning `None` leaves that cell transparent (outside the
/// circle, or a masked photo cell). Cells outside the grid are dropped — a
/// balloon on a duck still entering or exiting must never wrap to the
/// opposite edge (audit addendum note 8).
///
/// The disc cells come from [`circle_cells`]; for per-frame rendering prefer
/// [`draw_balloon_cells`] with scene-cached cells to avoid rebuilding the
/// `Vec` per balloon per frame (audit performance).
#[cfg(any(test, feature = "test-support"))]
pub fn draw_balloon<F>(
    grid: &mut crate::grid::Grid,
    centre_col: f32,
    centre_row: f32,
    rx: f32,
    ry: f32,
    mut pixel_at: F,
) where
    F: FnMut(i64, i64, f32) -> Option<(char, [u8; 3])>,
{
    if !centre_col.is_finite() || !centre_row.is_finite() {
        return;
    }
    let cx = centre_col.round() as i64;
    let cy = centre_row.round() as i64;
    draw_balloon_cells(grid, cx, cy, &circle_cells(rx, ry), &mut pixel_at);
}

/// Stamp a balloon disc from precomputed [`circle_cells`] output (same
/// semantics as [`draw_balloon`): renders byte-identical output while letting
/// callers cache the cell `Vec` per `(rx, ry)` across frames.
pub fn draw_balloon_cells<F>(
    grid: &mut crate::grid::Grid,
    cx: i64,
    cy: i64,
    cells: &[(i64, i64, f32)],
    pixel_at: &mut F,
) where
    F: FnMut(i64, i64, f32) -> Option<(char, [u8; 3])>,
{
    let (cols, rows) = (grid.cols() as i64, grid.rows() as i64);
    for &(dx, dy, d) in cells {
        plot_balloon_cell(grid, cx + dx, cy + dy, cols, rows, pixel_at(dx, dy, d));
    }
}

/// Stamp one balloon cell: `None` (masked/transparent) and `' '` stay
/// transparent; anything outside the grid is dropped, never wrapped.
fn plot_balloon_cell(
    grid: &mut crate::grid::Grid,
    x: i64,
    y: i64,
    cols: i64,
    rows: i64,
    pixel: Option<(char, [u8; 3])>,
) {
    let Some((ch, color)) = pixel else { return };
    if ch != ' ' && x >= 0 && x < cols && y >= 0 && y < rows {
        grid.set(x, y, ch, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn holder_table_matches_plan() {
        assert_eq!(holder_duck_index(0), 0, "big duck #1");
        assert_eq!(holder_duck_index(1), 2, "small #1");
        assert_eq!(holder_duck_index(2), 4, "small #3");
        assert_eq!(holder_duck_index(3), 6, "small #5");
        assert_eq!(holder_duck_index(4), 8, "small #7");
    }

    #[test]
    fn radii_derive_from_rows_and_cell_aspect() {
        let (rx, ry) = balloon_radii_cells(36, 2, 3);
        assert_eq!(ry, 36.0);
        assert!((rx - 54.0).abs() < 1e-4, "audit radii rx {rx}");
        let (rx, ry) = balloon_radii_cells(36, 12, 24);
        assert_eq!(ry, 36.0);
        assert!((rx - 72.0).abs() < 1e-4, "2:1 cell rx {rx}");
        let (rx, ry) = balloon_radii_cells(0, 2, 3);
        assert!(rx >= 2.0 && ry >= 1.0, "degenerate radius clamps");
    }

    #[test]
    fn authoring_geometry_is_the_disc_frame() {
        let geo = balloon_authoring_geometry(36);
        assert_eq!((geo.rx, geo.ry), (54.0, 36.0), "the disc radii");
        assert_eq!(
            (geo.cols, geo.rows),
            (109, 73),
            "the frame holds the whole disc"
        );
        let small = balloon_authoring_geometry(18);
        assert_eq!((small.cols, small.rows), (55, 37), "half the radius");
    }

    #[test]
    fn authoring_frame_keeps_the_pixel_circle() {
        // The frame is 3:2 in cells and the cell is 2:3 in pixels, so the
        // frame is a square in pixels and the disc inside it a circle.
        let geo = balloon_authoring_geometry(36);
        let cell_w = 16.0;
        let cell_h = 24.0;
        let width = geo.cols as f32 * cell_w;
        let height = geo.rows as f32 * cell_h;
        let skew = (width / height - 1.0).abs();
        assert!(skew < 0.01, "the frame is square in pixels (skew {skew})");
        let disc_w = 2.0 * geo.rx * cell_w;
        let disc_h = 2.0 * geo.ry * cell_h;
        assert!(
            (disc_w / disc_h - 1.0).abs() < 0.01,
            "the disc is a circle in pixels"
        );
    }

    #[test]
    fn authoring_mask_is_the_disc() {
        let geo = balloon_authoring_geometry(36);
        let mask = balloon_authoring_mask(&geo);
        assert_eq!(mask.len(), geo.cols * geo.rows);
        let at = |col: usize, row: usize| mask[row * geo.cols + col];
        assert!(!at(0, 0), "the corner is outside the disc");
        assert!(!at(geo.cols - 1, 0), "and so is the far corner");
        assert!(at(geo.cols / 2, 0), "the top edge is inside");
        assert!(at(0, geo.rows / 2), "the left edge is inside");
        assert!(at(geo.cols / 2, geo.rows / 2), "the centre is inside");
        let inked = mask.iter().filter(|m| **m).count();
        assert_eq!(
            inked,
            circle_cells(geo.rx, geo.ry).len(),
            "the mask is the disc, cell for cell"
        );
    }

    #[test]
    fn authoring_disc_recognises_only_disc_frames() {
        assert!(is_authoring_disc(109, 73), "the default frame");
        assert!(is_authoring_disc(55, 37), "half the radius");
        assert!(!is_authoring_disc(73, 73), "the old square frame");
        assert!(!is_authoring_disc(108, 72), "an even frame");
        assert!(!is_authoring_disc(0, 0), "an empty frame");
    }

    #[test]
    fn ring_buffer_overwrites_oldest_after_capacity() {
        let mut r: RingBuffer<u32> = RingBuffer::new(5);
        assert_eq!(r.capacity(), 5);
        for i in 0..5usize {
            assert_eq!(r.push(i as u32), i);
        }
        assert_eq!(r.len(), 5);
        assert_eq!(r.next_slot(), 0);
        assert_eq!(r.push(99), 0, "sixth animal overwrites balloon 0");
        assert_eq!(r.get(0), Some(&99));
        assert_eq!(r.get(1), Some(&1));
        assert_eq!(r.len(), 5, "no growth");
    }

    #[test]
    fn ring_buffer_iter_is_insertion_ordered() {
        // The one kept traversal accessor (audit P1-11): it pins the
        // wrap-around bookkeeping the production `get` path relies on.
        let mut r: RingBuffer<u32> = RingBuffer::new(3);
        for i in 0..5 {
            r.push(i);
        }
        let order: Vec<(usize, u32)> = r.iter().map(|(s, v)| (s, *v)).collect();
        assert_eq!(order, vec![(2, 2), (0, 3), (1, 4)], "wrap-around ordering");
    }

    #[test]
    fn ring_buffer_capacity_is_at_least_one() {
        let mut r: RingBuffer<u8> = RingBuffer::new(0);
        assert_eq!(r.capacity(), 1);
        assert_eq!(r.push(7), 0);
    }

    #[test]
    fn sprite_from_rows_and_get() {
        let s = AsciiSprite::from_rows(&["ab", "cd"], [1, 2, 3]);
        assert_eq!((s.cols(), s.rows()), (2, 2));
        assert_eq!(s.get(0, 0), Some(('a', [1, 2, 3])));
        assert_eq!(s.get(2, 0), None);
        assert_eq!(s.ink_count(), 4);
    }

    #[test]
    fn sprite_from_rows_pads_ragged_input() {
        let s = AsciiSprite::from_rows(&["a", "bcd"], [0, 0, 0]);
        assert_eq!((s.cols(), s.rows()), (3, 2));
        assert_eq!(s.get(0, 0), Some(('a', [0, 0, 0])));
        assert_eq!(s.get(1, 0), Some((' ', [0, 0, 0])));
    }

    #[test]
    fn trimmed_drops_blank_border() {
        let mut s = AsciiSprite::new(5, 4);
        s.set(2, 1, '#', [9, 9, 9]);
        s.set(3, 2, '#', [9, 9, 9]);
        let t = s.trimmed().expect("not empty");
        assert_eq!((t.cols(), t.rows()), (2, 2));
        assert_eq!(t.get(0, 0), Some(('#', [9, 9, 9])));
        assert_eq!(AsciiSprite::new(3, 3).trimmed(), None);
    }

    #[test]
    fn from_cells_validates_lengths() {
        assert!(AsciiSprite::from_cells(2, 2, vec!['a'; 3], vec![[0, 0, 0]; 4]).is_none());
        assert!(AsciiSprite::from_cells(2, 2, vec!['a'; 4], vec![[0, 0, 0]; 4]).is_some());
    }

    #[test]
    fn despeckle_removes_isolated_cell() {
        let mut s = AsciiSprite::new(7, 3);
        s.set(1, 1, '.', [0, 0, 0]);
        s.set(3, 1, '.', [0, 0, 0]);
        s.set(4, 1, '#', [0, 0, 0]);
        s.set(5, 1, '#', [0, 0, 0]);
        s.set(4, 0, '#', [0, 0, 0]);
        s.despeckle();
        assert_eq!(s.get(1, 1), Some((' ', [0, 0, 0])), "isolated cell dropped");
        assert_ne!(
            s.get(3, 1).map(|(ch, _)| ch),
            Some(' '),
            "orthogonally adjacent cell kept"
        );
    }

    #[test]
    fn circle_cells_is_symmetric_and_bounded() {
        let cells = circle_cells(8.0, 4.0);
        assert!(!cells.is_empty(), "disc must cover cells");
        assert_eq!(cells.len(), circle_cells(8.0, 4.0).len());
        for (dx, dy, d) in &cells {
            assert_disc_cell(&cells, *dx, *dy, *d);
        }
        assert!(cells.iter().any(|(dx, _, _)| *dx == 8 || *dx == -8));
        assert!(cells.iter().any(|(_, dy, _)| *dy == 4 || *dy == -4));
    }

    /// One disc cell stays mirrored, boxed, and normalized.
    fn assert_disc_cell(cells: &[(i64, i64, f32)], dx: i64, dy: i64, d: f32) {
        assert!(
            circle_cells_contains(cells, -dx, -dy),
            "missing mirror of ({dx}, {dy})"
        );
        assert!((-8..=8).contains(&dx), "dx {dx} escapes 17-wide box");
        assert!((-4..=4).contains(&dy), "dy {dy} escapes 9-tall box");
        assert!((0.0..=1.0).contains(&d), "d {d} outside 0..=1");
    }

    #[test]
    fn draw_balloon_writes_nothing_for_none() {
        let mut g = crate::grid::Grid::new(30, 20, 1, 1);
        draw_balloon(&mut g, 15.0, 10.0, 8.0, 4.0, |_, _, _| None);
        assert_eq!(g.ink_count(), 0);
    }

    #[test]
    fn photo_disc_inhabits_only_the_circle() {
        let mut g = crate::grid::Grid::new(40, 30, 1, 1);
        draw_balloon(&mut g, 20.0, 15.0, 8.0, 4.0, |_, _, _| {
            Some(('#', [200, 100, 50]))
        });
        assert!(g.ink_count() > 0, "disc must ink cells");
        let mut ink_outside = 0usize;
        g.for_each_cell(|c, r, ch, _| {
            if ch == ' ' {
                return;
            }
            let nx = (c as f32 - 20.0) / 8.0;
            let ny = (r as f32 - 15.0) / 4.0;
            if nx * nx + ny * ny > 1.0 + 1e-4 {
                ink_outside += 1;
            }
        });
        assert_eq!(ink_outside, 0, "ink escaped the circle");
    }

    fn circle_cells_contains(cells: &[(i64, i64, f32)], dx: i64, dy: i64) -> bool {
        cells.iter().any(|(x, y, _)| *x == dx && *y == dy)
    }
}
