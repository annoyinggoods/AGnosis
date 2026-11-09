//! The parallax layer stack (roadmap Extension 4 §1.1).
//!
//! A layer is `(kind, scroll_rate, asset)`: the scene draws a stack of
//! them, back to front. The scroll rate is a **multiplier on the parade's
//! offset**, not a second clock — a layer at 0.25 scrolls a quarter as
//! fast as the ducks, and because the offset is the same sub-cell value,
//! the layers stay pixel-locked to each other. That lock is what makes
//! the depth read.
//!
//! The rates are **code constants**, not config keys: an exposed rate is
//! a knob nobody can predict, and the art is what the user actually
//! wants to choose.
//!
//! The stack is fixed in v1:
//!
//! | layer | rate | drawn as |
//! | --- | --- | --- |
//! | sky | 0.0 | a background ramp, not a grid layer |
//! | backdrop 1..3 | 0.10, 0.25, 0.40 | a tile repeated across the screen |
//! | grass | 0.0 | rows of the scene grid |
//! | parade | 1.0 | rows of the scene grid |
//! | hud | 0.0 | rows of the scene grid |
//!
//! The grass and the HUD stay at offset 0 — they are chrome, and the
//! text must not smear. That rule already exists in the renderer and it
//! survives the generalization.

use crate::balloon::AsciiSprite;
use crate::grid::Grid;

/// What a layer is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayerKind {
    /// The procedural sky gradient. A background, not a grid layer.
    Sky,
    /// A backdrop band behind the ducks.
    Backdrop,
    /// The ground band.
    Grass,
    /// The ducks, the balloons and the ropes.
    Parade,
    /// The HUD, the counter, the message and the input line.
    Hud,
}

impl LayerKind {
    /// The layer's own name, for the trace lines.
    pub fn as_str(&self) -> &'static str {
        match self {
            LayerKind::Sky => "sky",
            LayerKind::Backdrop => "backdrop",
            LayerKind::Grass => "grass",
            LayerKind::Parade => "parade",
            LayerKind::Hud => "hud",
        }
    }
}

/// The scroll rate of the sky: it does not move.
pub const SKY_RATE: f32 = 0.0;

/// The scroll rate of the grass: it keeps its pixel-stability.
pub const GRASS_RATE: f32 = 0.0;

/// The scroll rate of the parade: the reference every other layer is a
/// fraction of.
pub const PARADE_RATE: f32 = 1.0;

/// The scroll rate of the HUD: chrome, and chrome must not smear.
pub const HUD_RATE: f32 = 0.0;

/// The scroll rates of the backdrop layers, farthest first.
pub const BACKDROP_RATES: [f32; 3] = [0.10, 0.25, 0.40];

/// The most backdrop layers the stack takes.
pub const MAX_BACKDROPS: usize = BACKDROP_RATES.len();

/// The scroll rate of the backdrop layer at `index` in the stack.
///
/// The index is clamped: a stack deeper than [`MAX_BACKDROPS`] has no
/// rate of its own, and the nearest rate is the honest answer.
pub fn backdrop_rate(index: usize) -> f32 {
    BACKDROP_RATES[index.min(BACKDROP_RATES.len() - 1)]
}

/// One backdrop layer: the tile, where its band sits and how fast it
/// scrolls.
///
/// The export needs the **tile** (the art, once) and the **placement**
/// (where the band sits), not the repeated grid the renderer blits: the
/// exported page repeats the tile itself.
#[derive(Debug, Clone, Copy)]
pub struct BackdropInfo<'a> {
    /// The tile the layer is built from.
    pub tile: &'a AsciiSprite,
    /// The grid row the band starts at.
    pub row_offset: usize,
    /// How many of the tile's rows are drawn.
    pub rows: usize,
    /// The layer's scroll rate: a fraction of the parade's travel.
    pub rate: f32,
}

/// One layer's blit: the grid, the grid row it starts at, how many of its
/// rows to draw, and the horizontal scroll offset in pixels.
#[derive(Debug, Clone, Copy)]
pub struct LayerBlit<'a> {
    /// The layer's own grid.
    pub grid: &'a Grid,
    /// The grid row the layer's first row lands on.
    pub row_offset: usize,
    /// How many of the layer's rows to draw.
    pub rows: usize,
    /// The horizontal scroll offset, in pixels. Negative: the layers move
    /// left, with the parade.
    pub offset_px: i32,
}

/// The fixed layer stack, back to front.
///
/// The stack is pinned in v1: the sky, the backdrop layers, the grass,
/// the parade and the HUD.
///
/// WHY this function is gone: it listed **one** backdrop, at
/// `BACKDROP_RATES[0]`, while the model has three backdrop rates and
/// the scene installs up to [`MAX_BACKDROPS`] layers — so the rates were
/// spelled in three places, and one of the three was wrong. The truth
/// is [`BACKDROP_RATES`] (with [`backdrop_rate`] for the lookup) and
/// the constants below it; the scene's trace line now names the layers
/// that were **actually installed** (audit 2026-10-21 §F19).
#[cfg(any(test, feature = "test-support"))]
pub fn stack() -> Vec<(LayerKind, f32)> {
    let mut layers = vec![(LayerKind::Sky, SKY_RATE)];
    layers.extend(
        BACKDROP_RATES
            .iter()
            .map(|rate| (LayerKind::Backdrop, *rate)),
    );
    layers.push((LayerKind::Grass, GRASS_RATE));
    layers.push((LayerKind::Parade, PARADE_RATE));
    layers.push((LayerKind::Hud, HUD_RATE));
    layers
}

/// The pixel offset of a layer with a **fixed** rate.
///
/// The fixed layers are the ones the stack pins: the sky and the HUD do
/// not move, the grass must keep its pixel-stability, and the parade is
/// the reference every other layer is a fraction of.
///
/// A **backdrop** has no fixed rate: it scrolls at 0.10, 0.25 or 0.40
/// depending on its place in the stack, so the answer is `None`. The
/// old `0` was a trap for a caller that trusted the name (audit
/// 2026-10-21 §F19).
pub fn fixed_offset_px(kind: LayerKind, parade_offset_px: i32) -> Option<i32> {
    let rate = match kind {
        LayerKind::Sky => SKY_RATE,
        LayerKind::Backdrop => return None,
        LayerKind::Grass => GRASS_RATE,
        LayerKind::Parade => PARADE_RATE,
        LayerKind::Hud => HUD_RATE,
    };
    Some((rate * parade_offset_px as f32) as i32)
}

/// The pixel offset of a layer that scrolls at `rate` while the parade has
/// travelled `scroll` cells.
///
/// The layer's own travel is `rate * scroll` cells, taken **modulo the
/// tile width** because the tile repeats: the layer's grid is one tile
/// wider than the screen, so a shift of up to a whole tile still covers
/// every column.
///
/// The offset is **negative**: the content moves left, with the parade.
pub fn layer_offset_px(rate: f32, scroll: f64, cell_w: u32, tile_cols: usize) -> i32 {
    if cell_w == 0 || tile_cols == 0 || !rate.is_finite() {
        return 0;
    }
    let travel = rate as f64 * scroll;
    if !travel.is_finite() {
        return 0;
    }
    let wrapped = travel.rem_euclid(tile_cols as f64);
    -((wrapped * cell_w as f64).round() as i32)
}

/// Build a layer's grid from a tile: the tile repeated across the screen
/// **plus one tile width**, so a scroll of up to a whole tile still covers
/// every column of the screen.
///
/// `rows` is how many rows of the tile to keep — the band is clipped to
/// the sky region, so a tall tile never grows into the grass.
pub fn tile_grid(
    tile: &AsciiSprite,
    screen_cols: usize,
    rows: usize,
    cell_w: u32,
    cell_h: u32,
) -> Grid {
    let tile_cols = tile.cols().max(1);
    let width = screen_cols.saturating_add(tile_cols);
    let height = rows.max(1);
    let mut grid = Grid::new(width, height, cell_w, cell_h);
    let inked = tile_cells(tile, width, height);
    for (col, row, ch, color) in inked {
        grid.set(col as i64, row as i64, ch, color);
    }
    grid
}

/// The tile's inked cells, repeated across `width` columns.
///
/// Flat on purpose: a loop inside a loop is banned (AGENTS.md nesting
/// rule), so the repetition is an iterator chain and the write is a
/// single loop over the result.
fn tile_cells(
    tile: &AsciiSprite,
    width: usize,
    height: usize,
) -> Vec<(usize, usize, char, [u8; 3])> {
    let tile_cols = tile.cols().max(1);
    (0..height.min(tile.rows()))
        .flat_map(|row| (0..width).map(move |col| (row, col)))
        .filter_map(|(row, col)| {
            let (ch, color) = tile.get(col % tile_cols, row)?;
            Some((col, row, ch, color))
        })
        .filter(|(_, _, ch, _)| *ch != ' ')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tile of one inked cell per column, so the scroll is measurable.
    fn band(cols: usize) -> AsciiSprite {
        let mut tile = AsciiSprite::new(cols, 1);
        for col in 0..cols {
            tile.set(col, 0, '#', [255, 255, 255]);
        }
        tile
    }

    #[test]
    fn a_static_layer_does_not_move() {
        assert_eq!(layer_offset_px(SKY_RATE, 1234.5, 2, 160), 0);
        assert_eq!(layer_offset_px(GRASS_RATE, 987.25, 3, 160), 0);
        assert_eq!(layer_offset_px(HUD_RATE, 55.0, 2, 160), 0);
    }

    /// A backdrop has **no** fixed rate: it scrolls at 0.10, 0.25 or
    /// 0.40 depending on its place in the stack, so the fixed-offset
    /// lookup must say so instead of answering `0` (audit 2026-10-21
    /// §F19).
    #[test]
    fn a_backdrop_has_no_fixed_offset() {
        assert_eq!(
            fixed_offset_px(LayerKind::Backdrop, 100),
            None,
            "the backdrop's rate depends on its index"
        );
        assert_eq!(fixed_offset_px(LayerKind::Parade, 100), Some(100));
        assert_eq!(fixed_offset_px(LayerKind::Grass, 100), Some(0));
        assert_eq!(fixed_offset_px(LayerKind::Sky, 100), Some(0));
        assert_eq!(fixed_offset_px(LayerKind::Hud, 100), Some(0));
    }

    #[test]
    fn the_parade_rate_moves_a_cell_per_scrolled_cell() {
        // The parade's own offset is a sub-cell shift of its grid, and the
        // layer that scrolls with it must carry the same distance.
        let tile_cols = 160;
        let cell_w = 2;
        let moved = layer_offset_px(PARADE_RATE, 10.0, cell_w, tile_cols);
        assert_eq!(moved, -20, "ten cells at two pixels each");
    }

    #[test]
    fn a_backdrop_scrolls_slower_than_the_parade() {
        let tile_cols = 160;
        let cell_w = 2;
        let scroll = 40.0;
        let parade = layer_offset_px(PARADE_RATE, scroll, cell_w, tile_cols);
        let backdrop = layer_offset_px(backdrop_rate(0), scroll, cell_w, tile_cols);
        assert!(
            backdrop.abs() < parade.abs(),
            "the backdrop ({backdrop}) must move less than the parade ({parade})"
        );
        // The first backdrop scrolls at a tenth of the parade's speed.
        assert_eq!(backdrop, -8, "40 cells at a tenth, two pixels each");
    }

    #[test]
    fn the_farther_backdrops_scroll_faster() {
        let far = backdrop_rate(0);
        let mid = backdrop_rate(1);
        let near = backdrop_rate(2);
        assert!(far < mid && mid < near, "{far} < {mid} < {near}");
    }

    #[test]
    fn the_scroll_wraps_at_the_tile_width() {
        let tile_cols = 10;
        let cell_w = 2;
        let one_tile = layer_offset_px(PARADE_RATE, 10.0, cell_w, tile_cols);
        let two_tiles = layer_offset_px(PARADE_RATE, 20.0, cell_w, tile_cols);
        assert_eq!(one_tile, two_tiles, "the tile repeats");
    }

    #[test]
    fn a_degenerate_layer_does_not_move() {
        assert_eq!(layer_offset_px(PARADE_RATE, 10.0, 0, 160), 0);
        assert_eq!(layer_offset_px(PARADE_RATE, 10.0, 2, 0), 0);
        assert_eq!(layer_offset_px(f32::NAN, 10.0, 2, 160), 0);
    }

    #[test]
    fn the_tile_grid_repeats_the_tile_across_the_screen() {
        let tile = band(4);
        let grid = tile_grid(&tile, 10, 1, 2, 3);
        // Ten screen columns plus one tile width.
        assert_eq!(grid.cols(), 14);
        assert_eq!(grid.rows(), 1);
        // The first cell of every tile is the tile's first cell.
        assert_eq!(grid.char_at(0, 0), Some('#'));
        assert_eq!(grid.char_at(4, 0), Some('#'));
        assert_eq!(grid.char_at(8, 0), Some('#'));
        assert_eq!(grid.char_at(12, 0), Some('#'));
    }

    #[test]
    fn the_tile_grid_keeps_the_tile_colours() {
        let mut tile = AsciiSprite::new(2, 1);
        tile.set(0, 0, '#', [10, 20, 30]);
        tile.set(1, 0, '@', [40, 50, 60]);
        let grid = tile_grid(&tile, 4, 1, 2, 3);
        assert_eq!(grid.color_at(0, 0), Some([10, 20, 30]));
        assert_eq!(grid.color_at(1, 0), Some([40, 50, 60]));
        assert_eq!(grid.color_at(2, 0), Some([10, 20, 30]));
    }

    #[test]
    fn the_stack_is_pinned() {
        let stack = stack();
        // The sky, one backdrop **per rate**, the grass, the parade and
        // the HUD: the old stack listed a single backdrop, which was the
        // third spelling of the rates (audit 2026-10-21 §F19).
        assert_eq!(stack.len(), 4 + MAX_BACKDROPS);
        assert_eq!(stack[0].0, LayerKind::Sky);
        assert_eq!(stack[0].1, SKY_RATE);
        for (index, rate) in BACKDROP_RATES.iter().enumerate() {
            assert_eq!(stack[1 + index].0, LayerKind::Backdrop);
            assert_eq!(stack[1 + index].1, *rate);
        }
        let grass = 1 + MAX_BACKDROPS;
        assert_eq!(stack[grass].0, LayerKind::Grass);
        assert_eq!(stack[grass].1, GRASS_RATE);
        assert_eq!(stack[grass + 1].0, LayerKind::Parade);
        assert_eq!(stack[grass + 1].1, PARADE_RATE);
        assert_eq!(stack[grass + 2].0, LayerKind::Hud);
        assert_eq!(stack[grass + 2].1, HUD_RATE);
    }

    #[test]
    fn every_kind_has_a_name() {
        for kind in [
            LayerKind::Sky,
            LayerKind::Backdrop,
            LayerKind::Grass,
            LayerKind::Parade,
            LayerKind::Hud,
        ] {
            assert!(!kind.as_str().is_empty());
        }
    }
}
