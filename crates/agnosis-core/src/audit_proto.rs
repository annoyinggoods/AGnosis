//! Audit prototype v2 (test-only): folds the 2026-10-06 user notes into the
//! validated layout. See REVIEW/audit/061026.md addendum. Changes vs v1:
//! longer legs, blunt rounded beak, ropes reaching the duck, 11 ducks
//! (2 big + 9 small, spawn-model: off-screen ducks not drawn, no col wrap),
//! balloons on ducks 1,3,5,7,9, randomized rope heights, ASCII-art HUD text.

use crate::blit::{blit_grid_rows, save_png, FrameBuffer};
use crate::config::Config;
use crate::font::Font;
use crate::grid::Grid;

// ---------------------------------------------------------------------------
// Parametric X-outline duck generator v2 (facing LEFT).
// ---------------------------------------------------------------------------

struct DuckGeom {
    cols: usize,
    rows: usize,
}

impl DuckGeom {
    fn px(&self) -> (f32, f32) {
        (self.cols as f32 * 2.0, self.rows as f32 * 3.0)
    }
    fn head_c(&self) -> (f32, f32) {
        let (w, h) = self.px();
        (w * 0.30, h * 0.24)
    }
    fn head_r(&self) -> f32 {
        self.px().0 * 0.14
    }
    fn body_c(&self) -> (f32, f32) {
        let (w, h) = self.px();
        (w * 0.55, h * 0.62)
    }
    fn body_r(&self) -> (f32, f32) {
        let (w, h) = self.px();
        (w * 0.40, h * 0.24)
    }
    /// Blunt beak: a horizontal capsule (segment + radius), not a `<` wedge.
    fn beak(&self) -> ((f32, f32), (f32, f32), f32) {
        let (w, h) = self.px();
        let hx = self.head_c().0 - self.head_r() * 0.85;
        let y = h * 0.28;
        ((hx, y), (w * 0.01, y), h * 0.055)
    }
    fn tail(&self) -> [(f32, f32); 3] {
        let (w, h) = self.px();
        [
            (w * 0.90, h * 0.46),
            (w * 0.995, h * 0.22),
            (w * 0.96, h * 0.70),
        ]
    }
    fn eye_cell(&self) -> (usize, usize) {
        (
            (self.head_c().0 + self.head_r() * 0.35) as usize / 2,
            (self.head_c().1 - self.head_r() * 0.25) as usize / 3,
        )
    }
    fn wing(&self) -> ((f32, f32), (f32, f32)) {
        let (w, h) = self.px();
        ((w * 0.58, h * 0.60), (w * 0.24, h * 0.13))
    }
    /// First row of visible leg: just below the body belly at the leg column.
    fn leg_top(&self) -> usize {
        self.rows * 86 / 100
    }
}

fn inside_geom(g: &DuckGeom, x: f32, y: f32) -> bool {
    let (hx, hy) = g.head_c();
    if ((x - hx) / g.head_r()).powi(2) + ((y - hy) / g.head_r()).powi(2) <= 1.0 {
        return true;
    }
    let (bx, by) = g.body_c();
    let (brx, bry) = g.body_r();
    if ((x - bx) / brx).powi(2) + ((y - by) / bry).powi(2) <= 1.0 {
        return true;
    }
    let ((ax, ay), (bx2, by2), r) = g.beak();
    let (vx, vy) = (bx2 - ax, by2 - ay);
    let t = (((x - ax) * vx + (y - ay) * vy) / (vx * vx + vy * vy)).clamp(0.0, 1.0);
    let (cx, cy) = (ax + t * vx, ay + t * vy);
    if (x - cx).powi(2) + (y - cy).powi(2) <= r * r {
        return true;
    }
    in_tri(g.tail(), x, y)
}

fn in_tri(t: [(f32, f32); 3], x: f32, y: f32) -> bool {
    let (ax, ay) = t[0];
    let (bx, by) = t[1];
    let (cx, cy) = t[2];
    let d1 = (x - bx) * (ay - by) - (ax - bx) * (y - by);
    let d2 = (x - cx) * (by - cy) - (bx - cx) * (y - cy);
    let d3 = (x - ax) * (cy - ay) - (cx - ax) * (y - ay);
    let has_neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
    let has_pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
    !(has_neg && has_pos)
}

/// One frame of the duck as `rows` strings. `frame` in 0..4 cycles LEG 1..4.
/// Crate-visible so `sprites.rs` can assert its frozen tables match (test
/// builds only; this whole module is `#[cfg(test)]`).
pub fn proto_duck(rows: usize, frame: usize) -> Vec<String> {
    let cols = rows * 150 / 70;
    let g = DuckGeom { cols, rows };
    let (w, _) = g.px();
    let mut art = vec![vec![' '; cols]; rows];
    stamp_body(&g, &mut art, cols, rows);
    stamp_wing(&g, &mut art, cols, rows, w);
    // Eye.
    let (ec, er) = g.eye_cell();
    if er < rows && ec < cols {
        art[er][ec] = 'X';
    }
    stamp_legs(&g, &mut art, cols, rows, frame);
    art.iter().map(|row| row.iter().collect()).collect()
}

/// Multisample cell centres of the head/body/beak/tail geometry: a cell is
/// inked when some (but not all) of its 6 sub-pixel taps land inside.
/// Split from `proto_duck` for nesting depth (audit site #8).
fn sample_cell(g: &DuckGeom, c: usize, r: usize) -> bool {
    // Precomputed sub-pixel tap offsets: 2 across (rows handled per tap).
    const SX: [f32; 2] = [-0.5, 0.5];
    let cx = c as f32 * 2.0 + 1.0;
    let cy = r as f32 * 3.0 + 1.5;
    let mut inside_n = 0;
    for sx in SX {
        inside_n += taps_in_row(g, cx + sx, cy);
    }
    inside_n > 0 && inside_n < 6
}

/// Count of vertical taps at one horizontal offset that land inside the duck
/// geometry. Split from `sample_cell` so no loop nests a loop.
fn taps_in_row(g: &DuckGeom, x: f32, cy: f32) -> usize {
    const SY: [f32; 3] = [-0.75, 0.0, 0.75];
    SY.iter().filter(|&&sy| inside_geom(g, x, cy + sy)).count()
}

/// Stamp the body outline (head + body + beak + tail) into the art grid.
fn stamp_body(g: &DuckGeom, art: &mut [Vec<char>], cols: usize, rows: usize) {
    for r in 0..rows {
        stamp_body_row(g, art, r, cols);
    }
}

/// Stamp one body-outline row. Split from `stamp_body` for nesting depth.
fn stamp_body_row(g: &DuckGeom, art: &mut [Vec<char>], r: usize, cols: usize) {
    for (c, cell) in art[r].iter_mut().enumerate().take(cols) {
        if sample_cell(g, c, r) {
            *cell = 'X';
        }
    }
}

/// Stamp the wing ring: cells near the wing ellipse but outside the body.
fn stamp_wing(g: &DuckGeom, art: &mut [Vec<char>], cols: usize, rows: usize, w: f32) {
    let ((wx, wy), (wrx, wry)) = g.wing();
    for r in 0..rows {
        stamp_wing_row(g, art, r, cols, w, wx, wy, wrx, wry);
    }
}

/// Stamp one wing-ring row. Split from `stamp_wing` for nesting depth.
#[allow(clippy::too_many_arguments)]
fn stamp_wing_row(
    g: &DuckGeom,
    art: &mut [Vec<char>],
    r: usize,
    cols: usize,
    w: f32,
    wx: f32,
    wy: f32,
    wrx: f32,
    wry: f32,
) {
    let _ = g;
    for (c, cell) in art[r].iter_mut().enumerate().take(cols) {
        if *cell != ' ' {
            continue;
        }
        let cx = c as f32 * 2.0 + 1.0;
        let cy = r as f32 * 3.0 + 1.5;
        let v = (((cx - wx) / wrx).powi(2) + ((cy - wy) / wry).powi(2)).sqrt();
        if (v - 1.0).abs() < 0.16 && cx > w * 0.30 && cx < w * 0.86 {
            *cell = 'X';
        }
    }
}

/// Stamp the 4-frame walk-cycle legs: shafts from `leg_top` to each foot plus
/// left-pointing feet on the foot row. Split from `proto_duck` for depth.
fn stamp_legs(g: &DuckGeom, art: &mut [Vec<char>], cols: usize, rows: usize, frame: usize) {
    // Long legs: 4-frame walk cycle, feet pointing left, planted on ground row.
    let front = cols as i64 * 42 / 100;
    let back = cols as i64 * 70 / 100;
    let leg_top = g.leg_top() as i64;
    let ground = rows as i64 - 1;
    for (col, top, foot) in leg_poses(front, back, leg_top, ground, frame) {
        stamp_leg_shaft(art, cols, rows, col, top, foot);
        stamp_leg_foot(art, cols, rows, col, foot);
    }
}

/// Walk-cycle leg poses as `(col, top_row, foot_row)`: the front and back
/// legs stride against each other across the 4 frames.
fn leg_poses(front: i64, back: i64, top: i64, ground: i64, frame: usize) -> Vec<(i64, i64, i64)> {
    match frame % 4 {
        0 => vec![(front, top, ground), (back, top, ground)],
        1 => vec![(front - 2, top, ground), (back + 2, top, ground)],
        2 => vec![(front - 2, top, ground - 3), (back, top, ground)],
        _ => vec![(front, top, ground), (back + 2, top, ground - 3)],
    }
}

/// Stamp one leg shaft: a 2-wide column from `top` to `foot`.
fn stamp_leg_shaft(art: &mut [Vec<char>], cols: usize, rows: usize, col: i64, top: i64, foot: i64) {
    for r in top..=foot {
        stamp_shaft_cell(art, cols, rows, col, r);
    }
}

/// Stamp one shaft row, dropping out-of-range cells. The column guard also
/// hardens degenerate sizes where `front - 2` goes negative: the old direct
/// `col as usize` index would have wrapped and panicked.
fn stamp_shaft_cell(art: &mut [Vec<char>], cols: usize, rows: usize, col: i64, r: i64) {
    if r < 0 || r as usize >= rows {
        return;
    }
    if col < 0 || col as usize >= cols {
        return;
    }
    art[r as usize][col as usize] = 'X';
    if col as usize + 1 < cols {
        art[r as usize][col as usize + 1] = 'X';
    }
}

/// Stamp one left-pointing foot: 3 cells ending at the leg column.
fn stamp_leg_foot(art: &mut [Vec<char>], cols: usize, rows: usize, col: i64, foot: i64) {
    if foot < 0 || foot as usize >= rows {
        return;
    }
    for fc in (col - 2)..=col {
        if fc < 0 || fc as usize >= cols {
            continue;
        }
        art[foot as usize][fc as usize] = 'X';
    }
}

// ---------------------------------------------------------------------------
// 5-row ASCII-art block font (user note 6): production module `hud_font`;
// the prototype draws through it so text rendering cannot diverge.
// ---------------------------------------------------------------------------

use crate::hud_font::draw_text as draw_ascii_text;

// ---------------------------------------------------------------------------

const DUCK: [u8; 3] = [255, 220, 80];
const GRASS: [u8; 3] = [90, 200, 90];
const ROPE: [u8; 3] = [180, 140, 90];
const BALLOON: [u8; 3] = [90, 110, 140];
const HUD: [u8; 3] = [160, 160, 160];

fn draw_art(grid: &mut Grid, art: &[String], col: i64, row: i64, color: [u8; 3]) {
    for (r, line) in art.iter().enumerate() {
        draw_art_line(grid, line, col, row + r as i64, color);
    }
}

/// Draw one art line: every non-blank glyph becomes a grid cell. Split
/// from `draw_art` so no loop nests a loop (audit nesting rule).
fn draw_art_line(grid: &mut Grid, line: &str, col: i64, row: i64, color: [u8; 3]) {
    for (c, ch) in line.chars().enumerate() {
        if ch != ' ' {
            draw_art_cell(grid, col + c as i64, row, ch, color);
        }
    }
}

/// One art cell. No column wrap: with the spawn-on-exit train model a
/// partially-entered duck must not teleport to the right edge.
fn draw_art_cell(grid: &mut Grid, x: i64, row: i64, ch: char, color: [u8; 3]) {
    if x < 0 || x >= grid.cols() as i64 {
        return;
    }
    grid.set(x, row, ch, color);
}

fn inked_cells(art: &[String], col: i64, row: i64, cols_limit: i64) -> Vec<(i64, i64)> {
    let mut v = Vec::new();
    for (r, line) in art.iter().enumerate() {
        v.extend(inked_line_cells(line, col, row + r as i64, cols_limit));
    }
    v
}

/// Inked cells of one art line, clipped to the frame. Split from
/// `inked_cells` so no loop nests a loop (audit nesting rule).
fn inked_line_cells(line: &str, col: i64, row: i64, cols_limit: i64) -> Vec<(i64, i64)> {
    let mut cells = Vec::new();
    for (c, ch) in line.chars().enumerate() {
        if ch != ' ' {
            cells.extend(inked_cell(col + c as i64, row, cols_limit));
        }
    }
    cells
}

/// One inked cell inside the frame, or `None` when it is off-screen.
fn inked_cell(x: i64, row: i64, cols_limit: i64) -> Option<(i64, i64)> {
    if x < 0 || x >= cols_limit {
        return None;
    }
    Some((x, row))
}

/// Stamp one balloon-disc ring (`0.86 <= d <= 1.0`) centred at `(hc, cy)`
/// and return the inked cells. Shared by the layout pass and the walk-frame
/// pass so the two identical copies collapse into one (audit site #9).
fn stamp_disc(
    grid: &mut Grid,
    hc: i64,
    cy: i64,
    rx: i64,
    ry: i64,
    cols_limit: i64,
) -> Vec<(i64, i64)> {
    let mut cells = Vec::new();
    for dy in -ry..=ry {
        stamp_disc_row(grid, hc, cy + dy, dy, rx, ry, cols_limit, &mut cells);
    }
    cells
}

/// Stamp one disc row. Split from `stamp_disc` so no loop nests a loop.
/// The argument list mirrors the geometry it needs; the sibling
/// `stamp_wing_row` carries the same exemption.
#[allow(clippy::too_many_arguments)]
fn stamp_disc_row(
    grid: &mut Grid,
    hc: i64,
    y: i64,
    dy: i64,
    rx: i64,
    ry: i64,
    cols_limit: i64,
    cells: &mut Vec<(i64, i64)>,
) {
    for dx in -rx..=rx {
        let d = (((dx * dx) as f32) / ((rx * rx) as f32) + ((dy * dy) as f32) / ((ry * ry) as f32))
            .sqrt();
        try_stamp_disc_cell(grid, hc + dx, y, d, cols_limit, cells);
    }
}

/// Stamp one disc-ring cell when it is on the ring and inside the frame.
/// Clamped, never wrapped: entering balloons must not teleport (audit site #9).
fn try_stamp_disc_cell(
    grid: &mut Grid,
    x: i64,
    y: i64,
    d: f32,
    cols_limit: i64,
    cells: &mut Vec<(i64, i64)>,
) {
    if !(0.86..=1.0).contains(&d) {
        return;
    }
    if x < 0 || x >= cols_limit {
        return;
    }
    grid.set(x, y, '.', BALLOON);
    cells.push((x, y));
}

/// True when `(hc, r)` already carries ink (out-of-range reads as empty).
/// Names the rope's stop-at-ink decision (audit site #9).
fn grid_reaches_ink(grid: &Grid, hc: i64, r: i64) -> bool {
    let (c, row) = (u32::try_from(hc).ok(), u32::try_from(r).ok());
    let Some((c, row)) = c.zip(row) else {
        return false;
    };
    grid.char_at(c as usize, row as usize)
        .is_some_and(|ch| ch != ' ')
}

/// What one balloon stamp produced: the disc cells, the rope cells and
/// whether the rope reached the duck's back.
struct BalloonStamp {
    /// Cells of the balloon disc ring.
    disc: Vec<(i64, i64)>,
    /// Cells of the rope.
    rope: Vec<(i64, i64)>,
    /// True when the rope stopped on the duck's back.
    reached_duck: bool,
}

/// Stamp one balloon disc and the rope that hangs from it.
///
/// The rope reaches the duck: it extends down from the balloon until the
/// first already-inked cell (the duck's back), never past the ground.
/// Shared by the layout pass and the walk-frame pass so the two copies
/// collapse into one and no loop nests a loop (audit site #9).
fn stamp_balloon_with_rope(
    grid: &mut Grid,
    hc: i64,
    bottom: i64,
    rx: i64,
    ry: i64,
    cols_limit: i64,
    ground: i64,
) -> BalloonStamp {
    let disc = stamp_disc(grid, hc, bottom - ry, rx, ry, cols_limit);
    let mut rope = Vec::new();
    let mut reached_duck = false;
    for r in bottom..ground {
        if grid_reaches_ink(grid, hc, r) {
            reached_duck = true;
            break;
        }
        grid.set(hc, r, '|', ROPE);
        rope.push((hc, r));
    }
    BalloonStamp {
        disc,
        rope,
        reached_duck,
    }
}

/// Draw the grass band: the rail row plus the wave row below it.
fn draw_grass_band(grid: &mut Grid, cols: i64, ground: i64) {
    for c in 0..cols {
        grid.set(c, ground, '~', GRASS);
        grid.set(c, ground + 1, '_', GRASS);
    }
}

/// Stamp the balloons and their ropes of one walk frame. Split out of the
/// frame loop so no loop nests a loop (audit nesting rule).
fn draw_balloons(
    grid: &mut Grid,
    carriers: &[i64],
    rope: &[i64],
    rx: i64,
    ry: i64,
    cols_limit: i64,
    ground: i64,
) {
    for (k, &hc) in carriers.iter().enumerate() {
        let bottom = ground - 8 - rope[k];
        stamp_balloon_with_rope(grid, hc, bottom, rx, ry, cols_limit, ground);
    }
}

/// Columns of the ducks that have entered the screen, with their train
/// index: ducks beyond the right edge are not drawn yet.
fn visible_ducks(cols: &[i64], cols_limit: i64) -> Vec<(i64, usize)> {
    cols.iter()
        .enumerate()
        .filter(|(_, &c)| c < cols_limit)
        .map(|(i, &c)| (c, i))
        .collect()
}

/// What one duck-drawing pass needs besides the frames themselves.
struct DrawPass {
    /// Row of the ducks' top edge.
    row: i64,
    /// Right edge of the frame.
    cols_limit: i64,
    /// Walk frame index of this pass.
    frame: usize,
    /// Frame stride: the big ducks stride by two frames, the smalls by one.
    stride: usize,
}

/// Draw the visible ducks of one pass; returns the inked cells per duck.
fn draw_visible_ducks(
    grid: &mut Grid,
    visible: &[(i64, usize)],
    frames: &[Vec<String>],
    pass: &DrawPass,
) -> Vec<Vec<(i64, i64)>> {
    let mut cells = Vec::new();
    for &(c, i) in visible {
        let art = &frames[(pass.frame + i * pass.stride) % 4];
        draw_art(grid, art, c, pass.row, DUCK);
        cells.push(inked_cells(art, c, pass.row, pass.cols_limit));
    }
    cells
}

/// True when the two cell sets share a cell.
fn cells_overlap(a: &[(i64, i64)], b: &[(i64, i64)]) -> bool {
    a.iter().any(|cell| b.contains(cell))
}

/// First cell of `cells` that is inked by `duck`, if any.
fn first_cell_in(cells: &[(i64, i64)], duck: &[(i64, i64)]) -> Option<(i64, i64)> {
    cells.iter().copied().find(|cell| duck.contains(cell))
}

/// First (cell, duck index) collision between `cells` and the ducks.
fn first_overlap(cells: &[(i64, i64)], ducks: &[Vec<(i64, i64)>]) -> Option<((i64, i64), usize)> {
    for (j, duck) in ducks.iter().enumerate() {
        if let Some(cell) = first_cell_in(cells, duck) {
            return Some((cell, j));
        }
    }
    None
}

/// Assert that duck `i` overlaps none of the ducks behind it.
fn assert_no_duck_overlap(ducks: &[Vec<(i64, i64)>], i: usize) {
    for j in i + 1..ducks.len() {
        assert!(
            !cells_overlap(&ducks[i], &ducks[j]),
            "duck {i} overlaps duck {j}"
        );
    }
}

/// Deterministic LCG so the randomized balloon heights are reproducible.
struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Prototype scene v2 at 1920x1080, cell [2,3] -> 960x360 grid:
/// - ground at 80%, big ducks 70 rows / smalls 35 rows with LONG legs
/// - 11 ducks (2 big + 9 small), train longer than the screen (spawn model)
/// - 5 balloons on ducks 1,3,5,7,9, radius r=(54,36), randomized rope heights
/// - ropes extend down until they touch the duck
/// - ASCII-art text for HUD + input line
#[test]
fn proto_render_1080p_layout_png() {
    let cols: usize = 960;
    let rows: usize = 360;
    let ground: i64 = rows as i64 * 80 / 100; // 288
    let big_rows = 70usize;
    let small_rows = 35usize;
    let mut grid = Grid::new(cols, rows, 2, 3);

    draw_grass_band(&mut grid, cols as i64, ground);

    let big_frames: Vec<Vec<String>> = (0..4).map(|f| proto_duck(big_rows, f)).collect();
    let small_frames: Vec<Vec<String>> = (0..4).map(|f| proto_duck(small_rows, f)).collect();

    // Train: 2 big + 9 small; total ~1125 cols > 960 screen. Ducks beyond the
    // right edge are NOT drawn (they have not entered yet).
    let big_cols: Vec<i64> = vec![30, 200];
    let small_cols: Vec<i64> = (0..9usize).map(|i| 380 + 90 * i as i64).collect();
    let duck_bottom = ground - 1; // feet touch the rail row
    let big_row = duck_bottom - big_rows as i64 + 1;
    let small_row = duck_bottom - small_rows as i64 + 1;

    let frame = 0usize;
    let mut all_cells: Vec<Vec<(i64, i64)>> = Vec::new();
    let visible_big = visible_ducks(&big_cols, cols as i64);
    let visible_small = visible_ducks(&small_cols, cols as i64);
    all_cells.extend(draw_visible_ducks(
        &mut grid,
        &visible_big,
        &big_frames,
        &DrawPass {
            row: big_row,
            cols_limit: cols as i64,
            frame,
            stride: 2,
        },
    ));
    all_cells.extend(draw_visible_ducks(
        &mut grid,
        &visible_small,
        &small_frames,
        &DrawPass {
            row: small_row,
            cols_limit: cols as i64,
            frame,
            stride: 1,
        },
    ));

    // Balloons on ducks 1,3,5,7,9 (slots 0,2,4,6,8) per user note 8. Carriers:
    // big duck 1 + smalls #1,#3,#5,#7. Balloon radius r=(54 cols, 36 rows).
    let (rx, ry) = (54i64, 36i64);
    let carriers: Vec<i64> = vec![
        big_cols[0] + 75,
        small_cols[0] + 37,
        small_cols[2] + 37,
        small_cols[4] + 37,
        small_cols[6] + 37,
    ];
    let carrier_is_big: Vec<bool> = vec![true, false, false, false, false];
    // Randomized rope lengths (user note 7): big carriers need >= 65 rows so
    // the balloon stays clear of the head; smalls >= 40 (sketch H3).
    let mut rng = Lcg(42);
    let rope: Vec<i64> = carrier_is_big
        .iter()
        .map(|&big| {
            let min = if big { 65 } else { 40 };
            min + (rng.next() % (150 - min + 1) as u64) as i64
        })
        .collect();
    let mut balloon_cells: Vec<(i64, i64)> = Vec::new();
    let mut rope_cells: Vec<(i64, i64)> = Vec::new();
    for (k, &hc) in carriers.iter().enumerate() {
        let bottom = ground - 8 - rope[k];
        let stamp = stamp_balloon_with_rope(&mut grid, hc, bottom, rx, ry, cols as i64, ground);
        balloon_cells.extend(stamp.disc);
        rope_cells.extend(stamp.rope);
        assert!(stamp.reached_duck, "rope {k} must reach its duck");
    }

    // Input line + HUD as ASCII-art text (user notes 5/6), bottom 20% band.
    let prompt = "ADD BALLOON: DUCKS.PNG_";
    draw_ascii_text(&mut grid, 2, ground + 8, prompt, HUD);
    draw_ascii_text(&mut grid, 2, rows as i64 - 6, "PRESS ESC TO EXIT", HUD);

    // --- assertions ---
    for i in 0..all_cells.len() {
        assert_no_duck_overlap(&all_cells, i);
    }
    if let Some((cell, j)) = first_overlap(&balloon_cells, &all_cells) {
        panic!("balloon {cell:?} overlaps duck {j}");
    }
    if let Some((cell, j)) = first_overlap(&rope_cells, &all_cells) {
        panic!("rope {cell:?} crosses duck {j}");
    }
    // Randomized ropes stay inside the vertical budget.
    for (k, &r) in rope.iter().enumerate() {
        assert!(ground - 8 - r - ry * 2 >= 0, "balloon {k} pokes above top");
    }
    // Long legs: the front leg column must be inked from ~86% down to ground.
    let big0 = &big_frames[0];
    let leg_col = 150usize * 42 / 100;
    let leg_col2 = leg_col + 1;
    let mut leg_ink = 0;
    for row in big0.iter().take(big_rows).skip(big_rows * 86 / 100) {
        let bytes = row.as_bytes();
        if bytes[leg_col] == b'X' || bytes[leg_col2] == b'X' {
            leg_ink += 1;
        }
    }
    assert!(
        leg_ink >= 10,
        "big duck legs must span >=10 rows, got {leg_ink}"
    );
    // Blunt beak: outline is an outline, so measure the vertical span between
    // the top and bottom boundary lines at the tip column (a `<` wedge would
    // converge to a single cell; the capsule stays several rows apart).
    let beak_col = 6usize;
    let ink_rows: Vec<usize> = (0..big_rows)
        .filter(|&r| big0[r].as_bytes().get(beak_col) == Some(&b'X'))
        .collect();
    let beak_span = ink_rows.last().map_or(0, |last| last - ink_rows[0] + 1);
    assert!(
        ink_rows.len() >= 2 && beak_span >= 3,
        "beak tip must be blunt (span >=3 rows), got span {beak_span}"
    );

    // --- render walk frames + timing ---
    //
    // All prototype PNG/txt outputs land in a per-process temp dir (tests must
    // not write into the repo; audit maintainability). The pipe stays the
    // same: Grid -> Font -> blit -> PNG, so the frames still exercise the
    // production blit path.
    let mut font = Font::new(2, 3).expect("font");
    let mut fb = FrameBuffer::new(1920, 1080);
    let out_dir = std::env::temp_dir().join(format!(
        "ducks-proto-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&out_dir).unwrap();
    let t0 = std::time::Instant::now();
    let frames_n = 30usize;
    for f in 0..frames_n {
        let mut g2 = Grid::new(cols, rows, 2, 3);
        draw_grass_band(&mut g2, cols as i64, ground);
        draw_visible_ducks(
            &mut g2,
            &visible_big,
            &big_frames,
            &DrawPass {
                row: big_row,
                cols_limit: cols as i64,
                frame: f,
                stride: 2,
            },
        );
        draw_visible_ducks(
            &mut g2,
            &visible_small,
            &small_frames,
            &DrawPass {
                row: small_row,
                cols_limit: cols as i64,
                frame: f,
                stride: 1,
            },
        );
        draw_balloons(&mut g2, &carriers, &rope, rx, ry, cols as i64, ground);
        draw_ascii_text(&mut g2, 2, ground + 8, prompt, HUD);
        draw_ascii_text(&mut g2, 2, rows as i64 - 6, "PRESS ESC TO EXIT", HUD);
        fb.clear_black();
        blit_grid_rows(&g2, &mut font, &mut fb, 0, &[]);
        if f < 2 {
            let path = out_dir.join(format!("proto_1080p_frame_{f}.png"));
            save_png(&fb, &path).expect("write proto png");
        }
    }
    println!(
        "PROTO v2 per-frame (rebuild + blit 960x360 @ cell 2x3): {:?}",
        t0.elapsed() / frames_n as u32
    );
    println!("PROTO v2 ropes: {rope:?}");

    for (name, art) in [
        ("proto_big_duck_70rows.txt", &big_frames[0]),
        ("proto_small_duck_35rows.txt", &small_frames[0]),
    ] {
        let txt: String = art.iter().map(|l| format!("{l}\n")).collect();
        std::fs::write(out_dir.join(name), txt).unwrap();
    }
}

/// Default config vs audit target cell math on 1080p. The audit target
/// became the production default in Fix 4 (row-budget grid).
#[test]
fn proto_default_config_math() {
    let cfg = Config::default();
    let (cols, rows, cw, ch) = cfg.grid_for_monitor(1920, 1080, 1.0);
    println!("default 1080p grid: {cols}x{rows} cell {cw}x{ch}");
    assert_eq!((cols, rows, cw, ch), (960, 360, 2, 3));
    let (cols, rows, cw, ch) = cfg.grid_for_monitor(3840, 2160, 1.25);
    println!("default 4K@125% grid: {cols}x{rows} cell {cw}x{ch}");
    assert_eq!((cols, rows, cw, ch), (960, 360, 4, 6));
}

/// Ink coverage of the 'X' glyph at one cell size: (lit pixels, mask
/// pixels), or `None` when the glyph has no mask at all (invisible).
/// Split from the test so no closure nests inside a match arm in a loop.
fn x_ink_at(cw: u32, ch: u32) -> Option<(usize, usize)> {
    let mut f = Font::new(cw, ch).expect("font loads");
    let m = f.mask('X')?;
    let ink = m.coverage.iter().filter(|&&v| v > 0).count();
    Some((ink, m.coverage.len()))
}

/// Rasterize 'X' at candidate cell sizes and report ink coverage.
#[test]
fn proto_font_ink_at_tiny_cells() {
    for (cw, ch) in [(2u32, 3u32), (3, 3), (3, 4), (4, 6), (6, 8), (12, 24)] {
        match x_ink_at(cw, ch) {
            Some((ink, total)) => {
                println!("CELL {cw}x{ch}: 'X' ink px = {ink}/{total}");
                assert!(ink > 0, "X must leave ink at {cw}x{ch}");
            }
            None => println!("CELL {cw}x{ch}: 'X' mask is NONE (invisible!)"),
        }
    }
}

/// Issue 5 feasibility: a picture from a folder converts to an ASCII balloon
/// at the audit radii (rx=54 cols, ry=36 rows) and lands in the scene ring.
#[test]
fn proto_image_to_balloon_pipeline_at_audit_radii() {
    use crate::image_ascii::convert_image_bytes;
    let img = image::RgbaImage::from_pixel(64, 64, image::Rgba([200, 40, 40, 255]));
    let mut tmp = std::env::temp_dir();
    tmp.push(format!("agnosis_audit_balloon_{}.png", std::process::id()));
    image::DynamicImage::ImageRgba8(img)
        .write_to(
            &mut std::io::BufWriter::new(std::fs::File::create(&tmp).unwrap()),
            image::ImageFormat::Png,
        )
        .unwrap();
    let bytes = std::fs::read(&tmp).unwrap();
    let sprite = convert_image_bytes(&bytes, 54, 36).expect("convert at audit radii");
    assert_eq!(sprite.cols(), 109);
    assert_eq!(sprite.rows(), 73);
    let mut scene = crate::scene::Scene::new(&Config::default(), 960, 360, 2, 3);
    let slot = scene.push_balloon(sprite);
    assert_eq!(slot, 0);
    assert_eq!(scene.balloon_count(), 1);
    scene.advance(1.0 / 30.0);
    assert!(scene.grid().ink_count() > 0);
    let _ = std::fs::remove_file(&tmp);
}

/// One-time codegen (Fix 2): freeze the validated prototype art as Rust
/// source for `crates/agnosis-core/src/sprite_art.rs`. Writes
/// `sprite_art.gen.rs` into a per-process temp dir and **prints the
/// path**; copy that file over `crates/agnosis-core/src/sprite_art.rs` for
/// an explicit regen. `sprites.rs` keeps a sync test asserting the frozen
/// tables still match [`proto_duck`], so hand-edited art fails the build.
/// Never writes into the repo (tests must use temp dirs only).
///
/// The command is `cargo test -p agnosis-core write_frozen_sprite_art_source
/// -- --nocapture`: the test lives in the **core** package, so the
/// un-scoped command matches nothing at the workspace root and exits 0 —
/// which looks like a successful regeneration that never happened
/// (audit 2026-10-21 §F21).
#[test]
fn write_frozen_sprite_art_source() {
    fn frames(rows: usize) -> Vec<Vec<String>> {
        (0..4).map(|f| proto_duck(rows, f)).collect()
    }
    fn table(name: &str, frames: &[Vec<String>]) -> String {
        let body: String = frames
            .iter()
            .map(|frame| {
                let rows: String = frame
                    .iter()
                    .map(|row| format!("        {row:?},\n"))
                    .collect();
                format!("    &[\n{rows}    ],\n")
            })
            .collect();
        format!("pub static {name}: &[&[&str]] = &[\n{body}];\n")
    }
    let big = frames(70);
    let small = frames(35);
    let mut src = String::from(
        "//! Frozen X-art duck frames, generated from the validated audit\n\
         //! prototype (`audit_proto::proto_duck`, REVIEW/audit/061026.md §2 and\n\
         //! addendum). Big: 150 cols x 70 rows; small: 75 cols x 35 rows; four\n\
         //! walk frames each differing only in the leg rows.\n\
         //! Regenerate with `cargo test -p agnosis-core write_frozen_sprite_art_source\n\
         //! -- --nocapture`, then copy the printed temp path over\n\
         //! `crates/agnosis-core/src/sprite_art.rs`. Do not hand-edit: sprites.rs\n\
         //! asserts this art matches the generator.\n\n",
    );
    src.push_str(&table("BIG_DUCK_FRAMES", &big));
    src.push('\n');
    src.push_str(&table("SMALL_DUCK_FRAMES", &small));
    let out_dir = std::env::temp_dir().join(format!(
        "ducks-codegen-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&out_dir).unwrap();
    let path = out_dir.join("sprite_art.gen.rs");
    std::fs::write(&path, &src).unwrap();
    println!("wrote {} ({} bytes)", path.display(), src.len());
}
