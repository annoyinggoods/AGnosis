//! Rope rasterization: a Bresenham line from hand cell to balloon, drawn in
//! unwrapped space and clipped at the grid edges (a rope on a duck that is
//! still entering or exiting must never wrap to the opposite edge — audit
//! addendum note 8).
//!
//! Coordinates are unwrapped floating point; each plotted cell outside the
//! grid is dropped. Note from render-core: line-art glyphs are guaranteed
//! edge-connecting, so `/`, `\` and `|` join visually across cells without
//! any extra work here.

/// Classify one rope step by its local slope.
pub fn line_char(dx: f32, dy: f32) -> char {
    if !dx.is_finite() || !dy.is_finite() {
        return '|';
    }
    if dx.abs() < f32::EPSILON {
        return '|';
    }
    if dy.abs() < f32::EPSILON {
        return '-';
    }
    if (dx > 0.0) == (dy > 0.0) {
        '\\'
    } else {
        '/'
    }
}

/// Draw a Bresenham rope from `from` to `to` (unwrapped cell coordinates).
/// Each cell takes the glyph of the step that LED to it ([`step_char`]), so
/// long diagonal runs render as straight `|`/`-` spans joined by `/`/`\`
/// kinks instead of one smeared slope glyph (audit P1-12), and the endpoint
/// keeps the direction it was approached from instead of falling back to a
/// degenerate zero-length slope.
/// Cells outside the grid are dropped: a rope is fully invisible when both
/// endpoints sit beyond one edge, and never wraps.
///
/// Test-only (audit maintainability: prod draws ropes with
/// [`draw_rope_stop_at_ink`]); the non-stop walk is the reference the rope
/// tests pin (endpoint glyphs, clipping, NaN), so it stays.
#[cfg(any(test, feature = "test-support"))]
pub fn draw_rope(grid: &mut crate::grid::Grid, from: (f32, f32), to: (f32, f32), color: [u8; 3]) {
    walk_rope(grid, from, to, color, false);
}

/// Draw a rope that stops at the first already-inked cell walking from
/// `from` to `to` — the rope lands on the duck's back instead of
/// overwriting the body (audit addendum note 1).
pub fn draw_rope_stop_at_ink(
    grid: &mut crate::grid::Grid,
    from: (f32, f32),
    to: (f32, f32),
    color: [u8; 3],
) {
    walk_rope(grid, from, to, color, true);
}

fn walk_rope(
    grid: &mut crate::grid::Grid,
    from: (f32, f32),
    to: (f32, f32),
    color: [u8; 3],
    stop_at_ink: bool,
) {
    if grid.cols() == 0 || grid.rows() == 0 {
        return;
    }
    if !from.0.is_finite() || !from.1.is_finite() || !to.0.is_finite() || !to.1.is_finite() {
        return;
    }
    let span = Span::new(from, to);
    let mut err = span.dx_abs + -span.dy_abs;
    let (mut x, mut y) = (span.x0, span.y0);
    // Direction of the step that LED to the current cell. `None` on the
    // first cell, which has no incoming step (audit D1).
    let mut incoming: Option<(i64, i64)> = None;
    loop {
        let (nx, ny) = span.peek_next(x, y, err);
        let ch = step_char(incoming, (nx - x, ny - y));
        if plot_rope_cell(grid, x, y, ch, stop_at_ink, color) {
            break;
        }
        if x == span.x1 && y == span.y1 {
            break;
        }
        incoming = Some((nx - x, ny - y));
        (x, y) = span.advance_step(x, y, &mut err);
    }
}

/// Constant span of one rope segment: the Bresenham error term and the step
/// predicates derive from these, never from the moving cursor. Bundled into
/// one struct so the step helpers stay readable (audit site #18).
struct Span {
    /// Start cell (rounded from the unwrapped start point).
    x0: i64,
    /// Start row.
    y0: i64,
    /// End cell.
    x1: i64,
    /// End row.
    y1: i64,
    /// Horizontal step direction, `+1` or `-1`.
    sx: i64,
    /// Vertical step direction, `+1` or `-1`.
    sy: i64,
    /// Horizontal distance, always nonnegative.
    dx_abs: i64,
    /// Vertical distance, always nonnegative.
    dy_abs: i64,
}

impl Span {
    /// Round the unwrapped endpoints into the integer cell span.
    fn new(from: (f32, f32), to: (f32, f32)) -> Self {
        let x0 = from.0.round() as i64;
        let y0 = from.1.round() as i64;
        let x1 = to.0.round() as i64;
        let y1 = to.1.round() as i64;
        Self {
            x0,
            y0,
            x1,
            y1,
            sx: if x0 < x1 { 1 } else { -1 },
            sy: if y0 < y1 { 1 } else { -1 },
            dx_abs: (x1 - x0).abs(),
            dy_abs: (y1 - y0).abs(),
        }
    }

    /// Pure lookahead at the next Bresenham cell: the glyph decision in the
    /// walk reads this, and [`Span::advance_step`] commits the same step, so
    /// the plotted glyph and the motion always agree (audit P1-12).
    fn peek_next(&self, x: i64, y: i64, err: i64) -> (i64, i64) {
        if x == self.x1 && y == self.y1 {
            return (x, y);
        }
        let mut nx = x;
        let mut ny = y;
        let e2 = 2 * err;
        if should_step_x(e2, x, self.x1, self.dy_abs) {
            nx += self.sx;
        }
        if should_step_y(e2, y, self.y1, self.dx_abs) {
            ny += self.sy;
        }
        (nx, ny)
    }

    /// Commit one Bresenham step: update the error term for each fired leg
    /// and return the next cell. The fired legs match [`Span::peek_next`] by
    /// construction.
    fn advance_step(&self, x: i64, y: i64, err: &mut i64) -> (i64, i64) {
        let (nx, ny) = self.peek_next(x, y, *err);
        if nx != x {
            *err += -self.dy_abs;
        }
        if ny != y {
            *err += self.dx_abs;
        }
        (nx, ny)
    }
}

/// Plot one walk cell. Out-of-grid cells are dropped. Returns true when the
/// walk ends here: an inked cell under `stop_at_ink`, decided in one flat
/// guard so the walk loop never nests (audit site #18).
fn plot_rope_cell(
    grid: &mut crate::grid::Grid,
    x: i64,
    y: i64,
    ch: char,
    stop_at_ink: bool,
    color: [u8; 3],
) -> bool {
    if x < 0 || x >= grid.cols() as i64 || y < 0 || y >= grid.rows() as i64 {
        return false;
    }
    if stop_at_ink && grid.char_at(x as usize, y as usize) != Some(' ') {
        return true;
    }
    grid.set(x, y, ch, color);
    false
}

/// True when the x-leg of the Bresenham step fires: the error term says so
/// and there is still horizontal distance to cover.
fn should_step_x(e2: i64, x: i64, x1: i64, dy_abs: i64) -> bool {
    e2 >= -dy_abs && x != x1
}

/// True when the y-leg of the Bresenham step fires.
fn should_step_y(e2: i64, y: i64, y1: i64, dx_abs: i64) -> bool {
    e2 <= dx_abs && y != y1
}

/// Glyph for one walk cell.
///
/// The glyph follows the step that LED to the cell, so the endpoint keeps
/// the direction it was approached from (a 45° rope ends in `\`, a
/// horizontal one in `-`). The first cell has no incoming step and uses the
/// outgoing one — the segment's own direction — so the rope starts with the
/// slope it actually runs at. A degenerate zero-length segment has no
/// direction at all and draws a pipe (audit D1: the old fallback used the
/// whole-segment slope, which at the endpoint degenerated to `line_char(0,
/// 0)` and painted every rope's last cell as a pipe).
fn step_char(incoming: Option<(i64, i64)>, outgoing: (i64, i64)) -> char {
    let (dx, dy) = incoming.unwrap_or(outgoing);
    line_char(dx as f32, dy as f32)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Grid;

    /// Count the rope's step glyphs in one flat pass: the `-` spans and the
    /// `/`/`\` kinks that join them. A nested loop would break the nesting
    /// rule (audit §Nesting), so the cells arrive as one flat iterator.
    fn count_step_glyphs(g: &Grid) -> (usize, usize) {
        let (cols, rows) = (g.cols(), g.rows());
        let mut dash = 0usize;
        let mut kink = 0usize;
        for (c, r) in (0..cols).flat_map(move |c| (0..rows).map(move |r| (c, r))) {
            match g.char_at(c, r) {
                Some('-') => dash += 1,
                Some('/') | Some('\\') => kink += 1,
                _ => {}
            }
        }
        (dash, kink)
    }

    #[test]
    fn vertical_run_uses_pipe() {
        let mut g = Grid::new(20, 20, 1, 1);
        draw_rope(&mut g, (5.0, 2.0), (5.0, 8.0), [255, 255, 255]);
        for r in 2..=8 {
            assert_eq!(g.char_at(5, r), Some('|'), "row {r}");
        }
        assert_eq!(g.ink_count(), 7);
    }

    #[test]
    fn steep_runs_use_slashes() {
        let mut down = Grid::new(20, 20, 1, 1);
        draw_rope(&mut down, (2.0, 2.0), (8.0, 8.0), [255, 255, 255]);
        assert_eq!(down.char_at(2, 2), Some('\\'));
        let mut up = Grid::new(20, 20, 1, 1);
        draw_rope(&mut up, (2.0, 8.0), (8.0, 2.0), [255, 255, 255]);
        assert_eq!(up.char_at(2, 8), Some('/'));
        assert_eq!(line_char(1.0, 1.0), '\\');
        assert_eq!(line_char(-1.0, -1.0), '\\');
        assert_eq!(line_char(1.0, -1.0), '/');
        assert_eq!(line_char(-1.0, 1.0), '/');
        assert_eq!(line_char(0.0, 5.0), '|');
        assert_eq!(line_char(5.0, 0.0), '-');
    }

    #[test]
    fn endpoint_glyph_follows_the_incoming_step() {
        // Audit D1: the endpoint has no outgoing step, so it must reuse the
        // direction it was approached from — never a degenerate
        // `line_char(0, 0)` pipe at the end of every rope.
        let mut diag = Grid::new(20, 20, 1, 1);
        draw_rope(&mut diag, (2.0, 2.0), (8.0, 8.0), [255, 255, 255]);
        assert_eq!(diag.char_at(8, 8), Some('\\'), "diagonal endpoint");
        for c in 2..=8 {
            assert_eq!(diag.char_at(c, c), Some('\\'), "diagonal cell {c}");
        }

        let mut horiz = Grid::new(20, 20, 1, 1);
        draw_rope(&mut horiz, (2.0, 5.0), (9.0, 5.0), [255, 255, 255]);
        assert_eq!(horiz.char_at(9, 5), Some('-'), "horizontal endpoint");
        for c in 2..=9 {
            assert_eq!(horiz.char_at(c, 5), Some('-'), "horizontal cell {c}");
        }

        let mut vert = Grid::new(20, 20, 1, 1);
        draw_rope(&mut vert, (5.0, 2.0), (5.0, 9.0), [255, 255, 255]);
        assert_eq!(vert.char_at(5, 9), Some('|'), "vertical endpoint");
        for r in 2..=9 {
            assert_eq!(vert.char_at(5, r), Some('|'), "vertical cell {r}");
        }
    }

    #[test]
    fn rope_crossing_left_edge_clips_instead_of_wrapping() {
        let mut g = Grid::new(10, 10, 1, 1);
        draw_rope(&mut g, (1.0, 5.0), (-2.0, 5.0), [255, 255, 255]);
        assert_eq!(g.char_at(0, 5), Some('-'), "visible part draws");
        assert_eq!(g.char_at(1, 5), Some('-'), "visible part draws");
        for c in [8, 9] {
            assert_eq!(g.char_at(c, 5), Some(' '), "no wrap at col {c}");
        }
    }

    #[test]
    fn long_diagonal_uses_step_glyphs_not_one_slope() {
        // Audit P1-12: a shallow run is mostly `-` with `/` or `\` only at
        // the kink rows — never a single smeared slope glyph wall-to-wall.
        let mut g = Grid::new(40, 10, 1, 1);
        draw_rope(&mut g, (2.0, 2.0), (37.0, 7.0), [255, 255, 255]);
        let (dash, kink) = count_step_glyphs(&g);
        assert!(dash > 0, "shallow run must contain `-` spans");
        assert!(kink > 0, "slope changes must kink through `/` or `\\`");
        // Endpoints still ink: the start with the segment direction, the
        // end with the direction it was approached from (audit D1).
        assert_ne!(g.char_at(2, 2), Some(' '), "start endpoint must ink");
        assert_ne!(g.char_at(37, 7), Some(' '), "end endpoint must ink");
    }

    #[test]
    fn off_screen_rope_draws_nothing() {
        let mut g = Grid::new(10, 10, 1, 1);
        draw_rope(&mut g, (3.0, -20.0), (3.0, -12.0), [255, 255, 255]);
        draw_rope(&mut g, (3.0, 30.0), (3.0, 40.0), [255, 255, 255]);
        assert_eq!(g.ink_count(), 0);
        let mut nan = Grid::new(10, 10, 1, 1);
        draw_rope(&mut nan, (f32::NAN, 0.0), (1.0, 1.0), [255, 255, 255]);
        assert_eq!(nan.ink_count(), 0);
    }
}
