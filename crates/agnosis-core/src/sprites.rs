//! Duck ASCII sprites: pure X-outline art generated from the validated
//! audit prototype (REVIEW/audit/061026.md §2 + addendum) and frozen in
//! [`crate::sprite_art`]. Both ducks face/move LEFT: the blunt rounded beak
//! capsule sits on the west edge, the tail on the east edge. The four frames
//! of a sprite share identical dimensions (space-padded at generation) and
//! differ ONLY in the leg rows; body rows are byte-identical across frames.
//! "Big" is a larger sprite, never a larger font.

/// A multi-frame walk-cycle sprite over borrowed row slices.
pub struct WalkSprite {
    /// Sprite width in cells; every frame row is padded to this.
    pub cols: usize,
    /// Sprite height in cells; every frame has this many rows.
    pub rows: usize,
    /// `frames[f][r]` is one row of frame `f`.
    pub frames: &'static [&'static [&'static str]],
}

impl WalkSprite {
    /// Build from pre-padded frame rows. Frame dimensions are trusted as
    /// authored; [`big_duck`] and [`small_duck`] guarantee the invariant.
    pub const fn new(cols: usize, rows: usize, frames: &'static [&'static [&'static str]]) -> Self {
        Self { cols, rows, frames }
    }

    /// One frame's rows, wrapping the index over the frame count.
    ///
    /// A sprite with **no** frames has no frame to return, so it returns
    /// an empty list: `WalkSprite::new` is a `pub const fn`, so any
    /// crate (the web one included) can build one, and an empty frame
    /// list must not be an out-of-bounds panic (audit 2026-10-21 §F18).
    pub fn frame(&self, index: usize) -> &'static [&'static str] {
        let count = self.frame_count();
        if count == 0 {
            return &[];
        }
        self.frames[index % count]
    }

    /// Number of walk frames (`>= 3` for both ducks).
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Stamp one frame onto the grid, skipping spaces. Columns and rows
    /// outside the grid are clipped — a partially entered/exited duck must
    /// never wrap to the opposite edge (audit addendum note 8).
    pub fn draw(
        &self,
        grid: &mut crate::grid::Grid,
        col: i64,
        row: i64,
        frame: usize,
        color: [u8; 3],
    ) {
        let cols = grid.cols() as i64;
        for (r, line) in self.frame(frame).iter().enumerate() {
            stamp_sprite_line(grid, line, col, row + r as i64, cols, color);
        }
    }

    /// Draw the frame compressed vertically by `squash` (0.0 = normal,
    /// 1.0 = fully squashed). The duck's **feet stay on the ground row**:
    /// the frame's bottom row keeps its place and the frame's top comes
    /// down.
    ///
    /// WHY the feet stay: a duck that lands must not sink into the grass
    /// — the squash reads as a compression of the body, not of the whole
    /// duck (roadmap Extension 3 §5.3).
    ///
    /// A `squash` of `0.0` draws **exactly** what [`WalkSprite::draw`]
    /// draws; that is the invariant the landing curve leans on.
    pub fn draw_squashed(
        &self,
        grid: &mut crate::grid::Grid,
        col: i64,
        row: i64,
        frame: usize,
        color: [u8; 3],
        squash: f32,
    ) {
        let rows = self.frame(frame);
        let height = rows.len();
        let cols = grid.cols() as i64;
        for (r, line) in rows.iter().enumerate() {
            let y = row + r as i64 + squash_row_shift(height, r, squash);
            stamp_sprite_line(grid, line, col, y, cols, color);
        }
    }
}

/// The biggest vertical compression a landing squash may apply: a duck
/// squashes to three quarters of its height at the full squash.
pub const MAX_SQUASH: f32 = 0.25;

/// How far the source row `row` of a frame of `frame_rows` rows moves
/// **down** when the frame is drawn squashed.
///
/// The frame's bottom row never moves (the feet stay planted) and the
/// top row comes down by [`MAX_SQUASH`] of the frame's height at the
/// full squash. A squash of `0.0` moves nothing at all, so the squashed
/// draw is the plain draw.
pub fn squash_row_shift(frame_rows: usize, row: usize, squash: f32) -> i64 {
    let squash = squash.clamp(0.0, 1.0);
    let height = frame_rows.saturating_sub(1) as f32;
    let drop = height - row as f32;
    (drop * MAX_SQUASH * squash).round() as i64
}

/// Sprite dimensions in cells: the single owner of the art sizes
/// (audit maintainability). [`crate::parade`] derives its train layout from
/// these instead of baked copies.
pub const BIG_DUCK_COLS: usize = 150;
pub const BIG_DUCK_ROWS: usize = 70;
pub const SMALL_DUCK_COLS: usize = 75;
pub const SMALL_DUCK_ROWS: usize = 35;

/// Column fraction of the sprite width carrying the front leg (percent).
///
/// The frozen v2 art, not a tunable: the generator in
/// [`crate::audit_proto`] places the front foot at this fraction, the back
/// foot at [`LEG_BACK_PERCENT`], and starts the legs at
/// [`LEG_TOP_PERCENT`] of the sprite height. Public beyond the tests
/// because [`crate::parade`] derives its stride-locked speed quantum from
/// the pair (audit 2026-10-23 F-031) — the art itself is frozen in
/// [`crate::sprite_art`].
pub const LEG_FRONT_PERCENT: usize = 42;

/// Column fraction of the sprite width carrying the back leg (percent).
///
/// See [`LEG_FRONT_PERCENT`]: [`crate::parade`] reads the pair.
pub const LEG_BACK_PERCENT: usize = 70;

/// Row fraction of the sprite height where the legs start (percent).
pub const LEG_TOP_PERCENT: usize = 86;

/// Stamp one sprite frame row at grid row `y`, clipping columns to
/// `0..cols` (never wrapping) and skipping spaces (audit nesting: the cell
/// loop lives here so [`WalkSprite::draw`] reads flat).
fn stamp_sprite_line(
    grid: &mut crate::grid::Grid,
    line: &str,
    col: i64,
    y: i64,
    cols: i64,
    color: [u8; 3],
) {
    for (c, ch) in line.chars().enumerate() {
        plot_sprite_cell(grid, col + c as i64, y, cols, ch, color);
    }
}

/// Stamp one sprite cell: spaces stay transparent, off-grid columns clip.
fn plot_sprite_cell(
    grid: &mut crate::grid::Grid,
    x: i64,
    y: i64,
    cols: i64,
    ch: char,
    color: [u8; 3],
) {
    if ch != ' ' && x >= 0 && x < cols {
        grid.set(x, y, ch, color);
    }
}

/// Big duck: 150 cols x 70 rows, 4 walk frames. X-outline art with a blunt
/// rounded beak, long legs (~10 visible rows, from 86% of sprite height down
/// to the ground row) and a 4-stage leftward stride (LEG 1 -> LEG 4).
pub fn big_duck() -> WalkSprite {
    WalkSprite::new(
        BIG_DUCK_COLS,
        BIG_DUCK_ROWS,
        crate::sprite_art::BIG_DUCK_FRAMES,
    )
}

/// Small duck: 75 cols x 35 rows, 4 walk frames. Same X-outline art as
/// [`big_duck`] at half scale; legs ~5 visible rows.
pub fn small_duck() -> WalkSprite {
    WalkSprite::new(
        SMALL_DUCK_COLS,
        SMALL_DUCK_ROWS,
        crate::sprite_art::SMALL_DUCK_FRAMES,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Grid;

    /// Fraction of the sprite width the beak must reach on the west edge.
    const BEAK_EDGE_PERCENT: usize = 5;
    /// Fraction of the sprite width the tail must reach on the east edge.
    const TAIL_EDGE_PERCENT: usize = 95;
    /// Sprite height above which the blunt-beak rule applies.
    const BIG_SPRITE_ROWS: usize = 50;
    /// Blunt-beak span in rows for the big duck.
    const BIG_BEAK_SPAN: usize = 3;
    /// Blunt-beak span in rows for the small duck: at 35 rows the capsule
    /// collapses to a flat 1-row beak line (still blunt, per the v2 art).
    const SMALL_BEAK_SPAN: usize = 1;

    fn assert_dimensions(sprite: &WalkSprite, cols: usize, rows: usize) {
        assert!(sprite.frame_count() >= 3);
        for (f, frame) in sprite.frames.iter().enumerate() {
            assert_frame_dimensions(frame, cols, rows, f);
        }
    }

    /// One frame must have `rows` rows of `cols` ASCII cells each (audit
    /// nesting: the row loop lives here so `assert_dimensions` reads flat).
    fn assert_frame_dimensions(frame: &[&str], cols: usize, rows: usize, f: usize) {
        assert_eq!(frame.len(), rows, "frame {f} height");
        for (r, line) in frame.iter().enumerate() {
            let w = line.chars().count();
            assert_eq!(w, cols, "frame {f} row {r} width {w}");
            assert!(line.is_ascii(), "frame {f} row {r} non-ASCII");
        }
    }

    #[test]
    fn big_duck_frames_share_dimensions() {
        let d = big_duck();
        assert_eq!((d.cols, d.rows), (150, 70));
        assert_dimensions(&d, 150, 70);
        assert_eq!(d.frame_count(), 4);
    }

    #[test]
    fn small_duck_frames_share_dimensions() {
        let d = small_duck();
        assert_eq!((d.cols, d.rows), (75, 35));
        assert_dimensions(&d, 75, 35);
        assert_eq!(d.frame_count(), 4);
    }

    #[test]
    fn frames_differ_only_in_leg_rows() {
        // Legs run from 86% of sprite height down to the ground row; the
        // body above is byte-identical across frames.
        for (name, sprite, body_rows) in [
            ("big", big_duck(), 0..60usize),
            ("small", small_duck(), 0..30usize),
        ] {
            assert_frames_differ_only_in_legs(name, &sprite, body_rows);
        }
    }

    /// The body rows of `sprite` are byte-identical across frames and the
    /// leg rows below `body_rows.end` vary (audit nesting: the frame and row
    /// loops live in the helpers so the test body reads flat).
    fn assert_frames_differ_only_in_legs(
        name: &str,
        sprite: &WalkSprite,
        body_rows: std::ops::Range<usize>,
    ) {
        assert_ne!(sprite.frame(0), sprite.frame(1), "{name} legs must move");
        assert_eq!(sprite.frame(4), sprite.frame(0), "{name} index wraps");
        for f in 1..sprite.frame_count() {
            assert_frame_body_identical(name, sprite, f, &body_rows);
        }
        assert_leg_rows_vary(name, sprite, body_rows.end);
    }

    /// One frame's body rows match frame 0 byte for byte.
    fn assert_frame_body_identical(
        name: &str,
        sprite: &WalkSprite,
        f: usize,
        body_rows: &std::ops::Range<usize>,
    ) {
        for r in body_rows.clone() {
            assert_eq!(
                sprite.frame(0)[r],
                sprite.frame(f)[r],
                "{name} body row {r} must be identical across frames"
            );
        }
    }

    /// At least one leg row (below `leg_start`) differs between frames.
    fn assert_leg_rows_vary(name: &str, sprite: &WalkSprite, leg_start: usize) {
        let varies = (leg_start..sprite.rows)
            .any(|r| (1..sprite.frame_count()).any(|f| sprite.frame(0)[r] != sprite.frame(f)[r]));
        assert!(varies, "{name} leg rows must vary");
    }

    #[test]
    fn ducks_face_left_with_blunt_beak_west_tail_east() {
        for (name, sprite) in [("big", big_duck()), ("small", small_duck())] {
            assert_frames_face_left(name, &sprite);
        }
    }

    /// Every frame of `sprite` faces left with a blunt beak.
    fn assert_frames_face_left(name: &str, sprite: &WalkSprite) {
        for f in 0..sprite.frame_count() {
            assert_frame_faces_left(name, sprite, f);
        }
    }

    /// One frame: inked cells exist, the beak sits west, the tail east and
    /// the beak tip is blunt.
    fn assert_frame_faces_left(name: &str, sprite: &WalkSprite, f: usize) {
        let inked = inked_cells(sprite.frame(f));
        assert!(!inked.is_empty(), "{name} frame {f} must have ink");
        assert_beak_west(name, sprite, &inked, f);
        assert_tail_east(name, sprite, &inked, f);
        assert_beak_blunt(name, sprite, &inked, f);
    }

    /// `(row, col)` of every inked cell in a frame (audit site #7 helper:
    /// the pixel loops live here so the test bodies read flat).
    fn inked_cells(frame: &[&str]) -> Vec<(usize, usize)> {
        let mut inked = Vec::new();
        for (r, line) in frame.iter().enumerate() {
            collect_ink_row(&mut inked, r, line);
        }
        inked
    }

    /// Push `(row, col)` of every non-space char of one frame row.
    fn collect_ink_row(inked: &mut Vec<(usize, usize)>, r: usize, line: &str) {
        for (c, ch) in line.chars().enumerate() {
            if ch != ' ' {
                inked.push((r, c));
            }
        }
    }

    /// West-edge beak placement: ink must reach the first 5% of columns.
    fn assert_beak_west(name: &str, sprite: &WalkSprite, inked: &[(usize, usize)], f: usize) {
        let min_c = inked.iter().map(|(_, c)| *c).min().unwrap_or(usize::MAX);
        assert!(
            min_c <= sprite.cols * BEAK_EDGE_PERCENT / 100,
            "{name} frame {f}: beak must sit on the west edge (min col {min_c})"
        );
    }

    /// East-edge tail placement: ink must reach the last 5% of columns.
    fn assert_tail_east(name: &str, sprite: &WalkSprite, inked: &[(usize, usize)], f: usize) {
        let max_c = inked.iter().map(|(_, c)| *c).max().unwrap_or(0);
        assert!(
            max_c >= sprite.cols * TAIL_EDGE_PERCENT / 100,
            "{name} frame {f}: tail must reach the east edge (max col {max_c})"
        );
    }

    /// Blunt (non-wedge) beak tip: ink at the tip columns spans rows.
    /// At 35 rows the small duck's capsule collapses to a flat 1-row beak
    /// line — still blunt, per the approved v2 art.
    fn assert_beak_blunt(name: &str, sprite: &WalkSprite, inked: &[(usize, usize)], f: usize) {
        let min_c = inked.iter().map(|(_, c)| *c).min().unwrap_or(usize::MAX);
        let mut tip_rows: Vec<usize> = inked
            .iter()
            .filter(|(_, c)| *c <= min_c + 2)
            .map(|(r, _)| *r)
            .collect();
        tip_rows.sort_unstable();
        let span = tip_rows.last().unwrap_or(&0) - tip_rows.first().unwrap_or(&0) + 1;
        let min_span = if sprite.rows > BIG_SPRITE_ROWS {
            BIG_BEAK_SPAN
        } else {
            SMALL_BEAK_SPAN
        };
        assert!(
            span >= min_span,
            "{name} frame {f}: beak tip must be blunt (span {span})"
        );
    }

    #[test]
    fn legs_are_long_and_planted_on_ground() {
        for (name, sprite) in [("big", big_duck()), ("small", small_duck())] {
            assert_legs_planted(name, &sprite);
        }
    }

    /// Every frame plants a foot on the ground row and draws the leg at that
    /// column up to the leg-top row (audit nesting: the frame loop lives in
    /// the helper so the test body reads flat).
    fn assert_legs_planted(name: &str, sprite: &WalkSprite) {
        for f in 0..sprite.frame_count() {
            assert_frame_leg_planted(name, sprite, f);
        }
    }

    /// One frame: a foot on the ground row and a leg spanning the leg rows.
    fn assert_frame_leg_planted(name: &str, sprite: &WalkSprite, f: usize) {
        let cols = sprite.cols;
        let front = cols * LEG_FRONT_PERCENT / 100;
        let back = cols * LEG_BACK_PERCENT / 100;
        let ground = sprite.rows - 1;
        let leg_top = sprite.rows * LEG_TOP_PERCENT / 100;
        let min_leg_rows = sprite.rows - leg_top;
        let frame = sprite.frame(f);
        let inked_at = |c: usize, r: usize| frame[r].as_bytes()[c] == b'X';
        let candidates = [front, front.saturating_sub(2), back, back + 2];
        let planted = candidates.iter().copied().find(|&c| inked_at(c, ground));
        assert!(
            planted.is_some(),
            "{name} frame {f}: no foot on the ground row"
        );
        let col = planted.unwrap();
        let span = (leg_top..=ground).filter(|&r| inked_at(col, r)).count();
        assert!(
            span >= min_leg_rows,
            "{name} frame {f}: leg at col {col} spans {span}, want >= {min_leg_rows}"
        );
    }

    /// A 4 x 5 sprite with its top and its bottom row inked: the rows in
    /// between are blank, so the squashed draw is easy to read.
    fn marker_sprite() -> WalkSprite {
        WalkSprite::new(4, 5, &[&["XXXX", "    ", "    ", "    ", "XXXX"]])
    }

    #[test]
    fn the_squash_keeps_the_feet_on_the_ground() {
        let sprite = marker_sprite();
        let mut plain = Grid::new(10, 10, 1, 1);
        sprite.draw(&mut plain, 2, 1, 0, [255, 255, 255]);
        let mut squashed = Grid::new(10, 10, 1, 1);
        sprite.draw_squashed(&mut squashed, 2, 1, 0, [255, 255, 255], 0.0);
        assert_eq!(
            plain, squashed,
            "a squash of zero must draw exactly what draw draws"
        );
        // The feet do not move: the bottom row keeps its place.
        let mut full = Grid::new(10, 10, 1, 1);
        sprite.draw_squashed(&mut full, 2, 1, 0, [255, 255, 255], 1.0);
        assert_eq!(full.char_at(2, 5), Some('X'), "the feet stay planted");
        assert_eq!(plain.char_at(2, 5), Some('X'));
        // The top comes down: the frame is three quarters of its height.
        assert_eq!(plain.char_at(2, 1), Some('X'));
        assert_eq!(full.char_at(2, 1), Some(' '), "the top row must come down");
        assert_eq!(full.char_at(2, 2), Some('X'), "the top row lands lower");
    }

    #[test]
    fn the_squash_is_clamped() {
        // A squash outside 0.0..=1.0 is clamped, so a broken reaction can
        // never fold the duck into itself.
        let sprite = marker_sprite();
        let mut over = Grid::new(10, 10, 1, 1);
        sprite.draw_squashed(&mut over, 2, 1, 0, [255, 255, 255], 5.0);
        let mut full = Grid::new(10, 10, 1, 1);
        sprite.draw_squashed(&mut full, 2, 1, 0, [255, 255, 255], 1.0);
        assert_eq!(over, full, "a squash above one is a full squash");
        let mut under = Grid::new(10, 10, 1, 1);
        sprite.draw_squashed(&mut under, 2, 1, 0, [255, 255, 255], -3.0);
        let mut plain = Grid::new(10, 10, 1, 1);
        sprite.draw(&mut plain, 2, 1, 0, [255, 255, 255]);
        assert_eq!(under, plain, "a squash below zero is no squash");
    }

    #[test]
    fn the_squash_shift_moves_only_the_top() {
        // The bottom row never moves and the top row moves the most.
        assert_eq!(squash_row_shift(5, 4, 1.0), 0, "the feet stay");
        assert_eq!(squash_row_shift(5, 0, 1.0), 1, "the top comes down");
        assert_eq!(squash_row_shift(5, 0, 0.0), 0, "no squash, no move");
        assert_eq!(squash_row_shift(5, 2, 1.0), 1);
        assert_eq!(
            squash_row_shift(1, 0, 1.0),
            0,
            "a one-row frame cannot squash"
        );
    }

    #[test]
    fn drawing_at_edge_clips_instead_of_wrapping() {
        let mut g = Grid::new(10, 40, 1, 1);
        let s = small_duck();
        // Only the westmost 2 columns of the sprite fit; the rest clips.
        s.draw(&mut g, 8, 0, 0, [255, 255, 255]);
        assert!(g.ink_count() > 0, "partial duck must ink its visible part");
        // Wrapped columns would reappear on the left edge — they must not.
        for r in 0..s.rows {
            assert_eq!(g.char_at(0, r), Some(' '), "no wrap at col 0, row {r}");
        }
        for r in s.rows..40 {
            assert_row_empty(&g, r, 10);
        }
        // Fully off-screen draw is a no-op.
        let mut h = Grid::new(10, 40, 1, 1);
        s.draw(&mut h, -80, 0, 0, [255, 255, 255]);
        assert_eq!(h.ink_count(), 0, "fully exited duck must vanish");
    }

    /// Every cell of grid row `r` must be blank (audit nesting: the column
    /// loop lives here so the caller reads flat).
    fn assert_row_empty(grid: &Grid, r: usize, cols: usize) {
        for c in 0..cols {
            assert_eq!(grid.char_at(c, r), Some(' '), "row {r} must stay empty");
        }
    }

    #[test]
    fn frozen_art_matches_validated_generator() {
        // The frozen tables must stay byte-identical to the validated audit
        // prototype so hand-edited art cannot silently drift (regenerate via
        // `write_frozen_sprite_art_source`).
        let big = big_duck();
        let small = small_duck();
        for f in 0..4 {
            assert_frame_matches_generator("big", &big, 70, f);
            assert_frame_matches_generator("small", &small, 35, f);
        }
    }

    /// One frozen frame must equal the prototype's frame row for row (audit
    /// nesting: the row loop lives here so the test body reads flat).
    fn assert_frame_matches_generator(name: &str, sprite: &WalkSprite, rows_n: usize, f: usize) {
        let gen = crate::audit_proto::proto_duck(rows_n, f);
        for (r, line) in sprite.frame(f).iter().enumerate() {
            assert_eq!(line, &gen[r], "{name} frame {f} row {r}");
        }
    }

    /// A sprite with no frames has no frame to return — and it must not
    /// be an out-of-bounds panic (audit 2026-10-21 §F18).
    #[test]
    fn a_sprite_without_frames_returns_an_empty_frame() {
        let sprite = WalkSprite::new(2, 2, &[]);
        assert_eq!(sprite.frame_count(), 0);
        assert!(sprite.frame(0).is_empty(), "the frame is empty");
        assert!(sprite.frame(7).is_empty(), "the frame is empty");
    }
}
