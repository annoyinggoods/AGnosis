//! Character grid: the only scene representation.
//!
//! One grid per screen. `' '` means transparent and is skipped by every blitter.
//! Column coordinates wrap (the duck parade crosses the left edge); row
//! coordinates are absolute and writes outside the grid are dropped.
//! [`Grid::set_if_visible`] drops out-of-range cells instead of wrapping, for
//! HUD/input text that must never teleport to column 0 (audit P1-9/P1-11).

pub const DEFAULT_FG: [u8; 3] = [255, 255, 255];

#[derive(Clone, Debug, PartialEq)]
pub struct Grid {
    cols: usize,
    rows: usize,
    cell_w: u32,
    cell_h: u32,
    chars: Vec<char>,
    colors: Vec<[u8; 3]>,
}

impl Grid {
    pub fn new(cols: usize, rows: usize, cell_w: u32, cell_h: u32) -> Self {
        let n = cols * rows;
        Self {
            cols,
            rows,
            cell_w: cell_w.max(1),
            cell_h: cell_h.max(1),
            chars: vec![' '; n],
            colors: vec![DEFAULT_FG; n],
        }
    }

    pub fn cols(&self) -> usize {
        self.cols
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn cell_w(&self) -> u32 {
        self.cell_w
    }

    pub fn cell_h(&self) -> u32 {
        self.cell_h
    }

    /// Pixel width of the grid (test helper: the renderer owns the real
    /// framebuffer size).
    #[cfg(any(test, feature = "test-support"))]
    pub fn px_width(&self) -> u32 {
        self.cols as u32 * self.cell_w
    }

    /// Pixel height of the grid (test helper: the renderer owns the real
    /// framebuffer size).
    #[cfg(any(test, feature = "test-support"))]
    pub fn px_height(&self) -> u32 {
        self.rows as u32 * self.cell_h
    }

    pub fn clear(&mut self) {
        self.chars.iter_mut().for_each(|c| *c = ' ');
        self.colors.iter_mut().for_each(|c| *c = DEFAULT_FG);
    }

    #[inline]
    fn wrap_col(&self, col: i64) -> Option<usize> {
        if self.cols == 0 {
            return None;
        }
        Some(col.rem_euclid(self.cols as i64) as usize)
    }

    #[inline]
    fn row_index(&self, row: i64) -> Option<usize> {
        if row < 0 || row as usize >= self.rows {
            None
        } else {
            Some(row as usize)
        }
    }

    /// Wrapping write. Rows outside the grid are dropped.
    #[inline]
    pub fn set(&mut self, col: i64, row: i64, ch: char, color: [u8; 3]) {
        let (Some(c), Some(r)) = (self.wrap_col(col), self.row_index(row)) else {
            return;
        };
        let i = r * self.cols + c;
        self.chars[i] = ch;
        self.colors[i] = color;
    }

    /// Dropping write, for HUD/input text that must not wrap across the
    /// screen edge (audit P1-11): out-of-range cells are dropped, on both
    /// axes. Nothing is clamped onto the edge cell — the old `set_clamped`
    /// smeared off-screen columns onto the edge, and the name now states the
    /// behaviour instead of promising a clamp. This is the non-wrapping
    /// primitive [`crate::hud_font`] builds its text rendering on (audit
    /// P1-9). For parade sprites use [`Grid::set`], which wraps columns by
    /// design.
    #[inline]
    pub fn set_if_visible(&mut self, col: i64, row: i64, ch: char, color: [u8; 3]) {
        let (Some(c), Some(r)) = (self.drop_col(col), self.row_index(row)) else {
            return;
        };
        let i = r * self.cols + c;
        self.chars[i] = ch;
        self.colors[i] = color;
    }

    /// Non-wrapping column index: `None` outside `0..cols` (rows reuse
    /// [`Grid::row_index`], which already drops).
    #[inline]
    fn drop_col(&self, col: i64) -> Option<usize> {
        if col < 0 || col as usize >= self.cols {
            None
        } else {
            Some(col as usize)
        }
    }

    /// Write a string left to right, wrapping across the grid edge (test
    /// helper: the HUD and input line use the dropping
    /// [`Grid::set_if_visible`] path instead).
    #[cfg(any(test, feature = "test-support"))]
    pub fn put_str(&mut self, col: i64, row: i64, s: &str, color: [u8; 3]) {
        for (k, ch) in s.chars().enumerate() {
            self.set(col + k as i64, row, ch, color);
        }
    }

    pub fn char_at(&self, col: usize, row: usize) -> Option<char> {
        if col >= self.cols || row >= self.rows {
            None
        } else {
            Some(self.chars[row * self.cols + col])
        }
    }

    /// Colour of a cell.
    pub fn color_at(&self, col: usize, row: usize) -> Option<[u8; 3]> {
        if col >= self.cols || row >= self.rows {
            None
        } else {
            Some(self.colors[row * self.cols + col])
        }
    }

    /// Row-major iteration over every cell.
    ///
    /// The flat `chars` buffer is walked once and the row/column are
    /// derived from the index (audit nesting rule: no loop inside a loop).
    /// The visit order stays row-major and the callback still receives
    /// `(col, row, char, color)`.
    pub fn for_each_cell(&self, mut f: impl FnMut(usize, usize, char, [u8; 3])) {
        // The index is split with `cols` as the divisor, so a zero-width
        // grid must not reach the loop. `Grid::new` keeps `chars` empty in
        // that case, but the guard states the precondition outright.
        if self.cols == 0 {
            return;
        }
        for i in 0..self.chars.len() {
            let r = i / self.cols;
            let c = i % self.cols;
            f(c, r, self.chars[i], self.colors[i]);
        }
    }

    /// Render as text, one line per row. Test and debug helper.
    #[cfg(any(test, feature = "test-support"))]
    pub fn to_text(&self) -> String {
        let mut out = String::with_capacity(self.rows * (self.cols + 1));
        for r in 0..self.rows {
            // One row is one contiguous slice of the flat buffer, so the
            // row needs no inner loop (audit nesting rule).
            let start = r * self.cols;
            out.extend(self.chars[start..start + self.cols].iter().copied());
            out.push('\n');
        }
        out
    }

    /// Count of non-space cells, i.e. the actual blit workload (test helper).
    #[cfg(any(test, feature = "test-support"))]
    pub fn ink_count(&self) -> usize {
        self.chars.iter().filter(|c| **c != ' ').count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_all_spaces() {
        let g = Grid::new(10, 3, 12, 24);
        assert_eq!(g.cols(), 10);
        assert_eq!(g.rows(), 3);
        assert_eq!(g.px_width(), 120);
        assert_eq!(g.px_height(), 72);
        assert_eq!(g.ink_count(), 0);
    }

    #[test]
    fn set_writes_and_reads_back() {
        let mut g = Grid::new(10, 3, 12, 24);
        g.set(3, 1, '#', [10, 20, 30]);
        assert_eq!(g.char_at(3, 1), Some('#'));
        assert_eq!(g.color_at(3, 1), Some([10, 20, 30]));
        assert_eq!(g.ink_count(), 1);
    }

    #[test]
    fn set_wraps_columns_both_directions() {
        let mut g = Grid::new(4, 1, 1, 1);
        g.set(4, 0, 'a', DEFAULT_FG);
        assert_eq!(g.char_at(0, 0), Some('a'));
        g.set(-1, 0, 'b', DEFAULT_FG);
        assert_eq!(g.char_at(3, 0), Some('b'));
        g.set(-5, 0, 'c', DEFAULT_FG);
        assert_eq!(g.char_at(3, 0), Some('c'));
        g.set(-6, 0, 'd', DEFAULT_FG);
        assert_eq!(g.char_at(2, 0), Some('d'));
    }

    #[test]
    fn set_drops_out_of_range_rows() {
        let mut g = Grid::new(4, 2, 1, 1);
        g.set(0, -1, 'x', DEFAULT_FG);
        g.set(0, 2, 'y', DEFAULT_FG);
        assert_eq!(g.ink_count(), 0);
    }

    #[test]
    fn set_if_visible_drops_instead_of_wrapping_or_smearing() {
        let mut g = Grid::new(4, 2, 1, 1);
        g.set_if_visible(-3, 0, 'x', DEFAULT_FG);
        g.set_if_visible(9, 0, 'y', DEFAULT_FG);
        g.set_if_visible(0, -1, 'z', DEFAULT_FG);
        g.set_if_visible(0, 9, 'w', DEFAULT_FG);
        assert_eq!(g.ink_count(), 0, "out-of-range writes must drop");
        g.set_if_visible(1, 1, 'q', DEFAULT_FG);
        assert_eq!(g.char_at(1, 1), Some('q'));
        assert_eq!(g.ink_count(), 1);
    }

    #[test]
    fn set_if_visible_does_not_smear_onto_the_edge_column() {
        // Audit P1-11: the old `set_clamped` collapsed off-screen columns
        // onto the edge cell; a dropping write must leave the edge alone.
        let mut g = Grid::new(4, 1, 1, 1);
        g.set_if_visible(9, 0, 'y', DEFAULT_FG);
        assert_eq!(g.char_at(3, 0), Some(' '), "edge cell must stay clean");
        assert_eq!(g.ink_count(), 0);
    }

    #[test]
    fn put_str_wraps_across_edge() {
        let mut g = Grid::new(5, 1, 1, 1);
        g.put_str(3, 0, "abc", DEFAULT_FG);
        assert_eq!(g.char_at(3, 0), Some('a'));
        assert_eq!(g.char_at(4, 0), Some('b'));
        assert_eq!(g.char_at(0, 0), Some('c'));
    }

    #[test]
    fn clear_resets_everything() {
        let mut g = Grid::new(4, 4, 1, 1);
        g.put_str(0, 0, "abcd", [1, 2, 3]);
        g.clear();
        assert_eq!(g.ink_count(), 0);
        assert_eq!(g.color_at(0, 0), Some(DEFAULT_FG));
    }

    #[test]
    fn zero_cell_size_is_clamped_to_one() {
        let g = Grid::new(2, 2, 0, 0);
        assert_eq!(g.cell_w(), 1);
        assert_eq!(g.cell_h(), 1);
    }

    #[test]
    fn to_text_has_one_line_per_row() {
        let mut g = Grid::new(3, 2, 1, 1);
        g.set(0, 0, 'a', DEFAULT_FG);
        g.set(2, 1, 'z', DEFAULT_FG);
        assert_eq!(g.to_text(), "a  \n  z\n");
    }
}
