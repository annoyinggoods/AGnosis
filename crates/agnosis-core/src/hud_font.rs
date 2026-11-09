//! ASCII-art block font for on-screen text (audit 2026-10-06 addendum
//! note 6): one grid cell per font pixel, 5 rows tall. At the row-budget
//! cell (2x3 px on 1080p) a TrueType glyph is an unreadable smudge, while a
//! 5-cell-tall block glyph reads clearly. All on-screen text (HUD hint, the
//! Fix 5 input line) renders through this module.
//!
//! Glyphs are `&'static [&'static str]` rows of `X`/`.`, top to bottom;
//! widths vary per glyph (1-5). Unknown characters are skipped; lowercase
//! input folds to uppercase.

/// Glyph rows for one character, or `None` when unsupported.
///
/// The block font carries uppercase glyphs only; callers fold case before
/// the lookup ([`char_width`], [`draw_text`]), so this table stays a plain
/// uppercase map.
fn glyph(ch: char) -> Option<&'static [&'static str]> {
    let g: &[&str] = match ch {
        'A' => &[".XXX.", "X...X", "XXXXX", "X...X", "X...X"],
        'B' => &["XXXX.", "X...X", "XXXX.", "X...X", "XXXX."],
        'C' => &["XXXXX", "X....", "X....", "X....", "XXXXX"],
        'D' => &["XXX..", "X..X.", "X..X.", "X..X.", "XXX.."],
        'E' => &["XXXXX", "X....", "XXXX.", "X....", "XXXXX"],
        'F' => &["XXXXX", "X....", "XXXX.", "X....", "X...."],
        'G' => &[".XXX.", "X....", "X..XX", "X...X", ".XXX."],
        'H' => &["X...X", "X...X", "XXXXX", "X...X", "X...X"],
        'I' => &["..X..", "..X..", "..X..", "..X..", "..X.."],
        'J' => &["..XXX", "...X.", "...X.", "X..X.", ".XX.."],
        'K' => &["X...X", "X..X.", "XXX..", "X..X.", "X...X"],
        'L' => &["X....", "X....", "X....", "X....", "XXXXX"],
        'M' => &["X...X", "XX.XX", "X.X.X", "X.X.X", "X...X"],
        'N' => &["X...X", "XX..X", "X.X.X", "X..XX", "X...X"],
        'O' => &[".XXX.", "X...X", "X...X", "X...X", ".XXX."],
        'P' => &["XXXX.", "X...X", "XXXX.", "X....", "X...."],
        'Q' => &[".XXX.", "X...X", "X.X.X", "X..X.", ".XXX."],
        'R' => &["XXXX.", "X...X", "XXXX.", "X.X..", "X..X."],
        'S' => &[".XXXX", "X....", ".XXX.", "....X", "XXXX."],
        'T' => &["XXXXX", "..X..", "..X..", "..X..", "..X.."],
        'U' => &["X...X", "X...X", "X...X", "X...X", ".XXX."],
        'V' => &["X...X", "X...X", "X...X", ".X.X.", "..X.."],
        'W' => &["X...X", "X...X", "X.X.X", "XX.XX", "X...X"],
        'X' => &["X...X", ".X.X.", "..X..", ".X.X.", "X...X"],
        'Y' => &["X...X", ".X.X.", "..X..", "..X..", "..X.."],
        'Z' => &["XXXXX", "...X.", "..X..", ".X...", "XXXXX"],
        '0' => &[".XXX.", "X..XX", "X.X.X", "XX..X", ".XXX."],
        '1' => &["..X..", ".XX..", "..X..", "..X..", "..X.."],
        '2' => &["XXXXX", "....X", "XXXXX", "X....", "XXXXX"],
        '3' => &["XXXXX", "....X", "XXXXX", "....X", "XXXXX"],
        '4' => &["X...X", "X...X", "XXXXX", "....X", "....X"],
        '5' => &["XXXXX", "X....", "XXXXX", "....X", "XXXXX"],
        '6' => &["XXXXX", "X....", "XXXXX", "X...X", "XXXXX"],
        '7' => &["XXXXX", "....X", "....X", "....X", "....X"],
        '8' => &["XXXXX", "X...X", "XXXXX", "X...X", "XXXXX"],
        '9' => &["XXXXX", "X...X", "XXXXX", "....X", "XXXXX"],
        '.' => &[".", ".", ".", ".", "X"],
        ':' => &[".", "X", ".", "X", "."],
        '_' => &["....", "....", "....", "....", "XXXX"],
        '-' => &["...", "...", "XXX", "...", "..."],
        '/' => &["...X", "..X.", "..X.", ".X..", "X..."],
        '\\' => &["X...", ".X..", ".X..", "..X.", "...X"],
        ' ' => &["..", "..", "..", "..", ".."],
        _ => return None,
    };
    Some(g)
}

/// Rendered width of one character in cells (advance + 1 gap), 0 when the
/// character has no glyph.
///
/// Case-insensitive: the lookup folds to uppercase first, so a lowercase
/// caller can never silently measure 0 for text that *does* render (audit
/// P1-9 nit — the measuring helper must not be the thing that breaks).
pub fn char_width(ch: char) -> i64 {
    glyph(ch.to_ascii_uppercase()).map_or(0, |g| g[0].len() as i64 + 1)
}

/// True when the block font can draw `ch`.
///
/// The font carries only `A-Z`, `0-9`, `. : _ - / \` and space. A caller
/// that owns user text (the custom message) asks this **before** it draws,
/// so an unsupported character is reported instead of silently vanishing
/// (roadmap Extension 1 §3.2).
pub fn supports(ch: char) -> bool {
    glyph(ch.to_ascii_uppercase()).is_some()
}

/// Fold `text` to exactly what the block font can draw.
///
/// Lowercase folds to uppercase and every glyph the font cannot draw is
/// **dropped**, while spaces survive (the gap between words is the point
/// of them). This is the one shared sanitizer for on-screen text (audit
/// 2026-10-23 F-043 / U-05): the callers that used to fold case by hand
/// still silently lost `(`, `)`, `=`, `+` and `!` — `draw_text` skips an
/// unknown character without a trace, so a hint like `ESC (OR Q) = QUIT`
/// rendered as `ESC OR Q  QUIT` and nobody noticed (U-19).
///
/// The measured width of the result is what [`text_width`] reports for it,
/// so a caller that sanitizes first can never mis-centre the text.
pub fn sanitize(text: &str) -> String {
    text.chars()
        .map(|ch| ch.to_ascii_uppercase())
        .filter(|ch| glyph(*ch).is_some())
        .collect()
}

/// Rendered width of a whole string in cells (trailing gap included).
/// Case-insensitive, like [`char_width`].
pub fn text_width(text: &str) -> i64 {
    text.chars().map(char_width).sum()
}

/// Stamp `text` onto the grid with one cell per font pixel, top-left at
/// `(col, row)`. Lowercase folds to uppercase; unsupported characters are
/// skipped without advancing. Cells outside the grid are dropped — long input
/// clips at the right edge instead of wrapping onto column 0 (audit P1-9,
/// via [`crate::grid::Grid::set_if_visible`]).
pub fn draw_text(grid: &mut crate::grid::Grid, col: i64, row: i64, text: &str, color: [u8; 3]) {
    let mut x = col;
    for ch in text.chars().map(|c| c.to_ascii_uppercase()) {
        let Some(g) = glyph(ch) else {
            continue;
        };
        stamp_glyph(grid, x, row, g, color);
        x += g[0].len() as i64 + 1;
    }
}

/// Stamp one 5-row glyph with its top-left at `(x, row)`, dropping
/// out-of-grid cells instead of wrapping (audit P1-9).
fn stamp_glyph(grid: &mut crate::grid::Grid, x: i64, row: i64, g: &[&str], color: [u8; 3]) {
    for (r, line) in g.iter().enumerate() {
        stamp_glyph_row(grid, x, row + r as i64, line, color);
    }
}

/// Stamp one glyph row: `X` cells ink, everything else stays transparent.
fn stamp_glyph_row(grid: &mut crate::grid::Grid, x: i64, y: i64, line: &str, color: [u8; 3]) {
    for (c, px) in line.chars().enumerate() {
        if px == 'X' {
            grid.set_if_visible(x + c as i64, y, 'X', color);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::Grid;

    /// Every character the block font claims to carry.
    fn glyph_chars() -> impl Iterator<Item = char> {
        ('A'..='Z').chain('0'..='9').chain("._-/\\ :".chars())
    }

    /// The font's glyph for `ch`, panicking when the font carries none.
    fn glyph_of(ch: char) -> &'static [&'static str] {
        match glyph(ch) {
            Some(g) => g,
            None => panic!("glyph {ch:?} missing"),
        }
    }

    /// One glyph's rows, paired with the character they belong to. The art
    /// check needs every row of every glyph, and a nested loop would break
    /// the nesting rule (audit §Nesting), so the rows arrive as one flat
    /// iterator.
    fn glyph_rows(ch: char) -> impl Iterator<Item = (char, &'static str)> {
        glyph_of(ch).iter().map(move |line| (ch, *line))
    }

    #[test]
    fn every_glyph_is_five_rows_and_ascii() {
        for ch in glyph_chars() {
            assert_eq!(glyph_of(ch).len(), 5, "glyph {ch:?} rows");
        }
        for (ch, line) in glyph_chars().flat_map(glyph_rows) {
            assert!(
                line.chars().all(|c| c == 'X' || c == '.'),
                "glyph {ch:?} non-art char"
            );
        }
    }

    #[test]
    fn uppercase_folds_and_unknown_skips() {
        let mut g = Grid::new(40, 5, 1, 1);
        draw_text(&mut g, 0, 0, "e!s", [255, 255, 255]);
        // 'E' at col 0, '!' skipped without advancing, 'S' at col 6
        // (curved S: top row ".XXXX", so the full bars start at col 7).
        let text = g.to_text();
        assert!(text.contains("XXXXX"), "E top row must render");
        let top = text.lines().next().unwrap();
        assert!(
            top[7..].starts_with("XXXX"),
            "S must follow E at col 6, got {top:?}"
        );
    }

    #[test]
    fn confusable_glyphs_stay_distinct() {
        // I must not carry a T-style top bar (it read as T on screen).
        assert_eq!(glyph('I').unwrap()[0], "..X..");
        // S must break B's straight left spine.
        assert_ne!(glyph('S').unwrap()[0], glyph('B').unwrap()[0]);
        assert_eq!(glyph('S').unwrap()[0], ".XXXX");
        // M keeps its centre leg below the V so it cannot read as N.
        assert_eq!(glyph('M').unwrap()[3], "X.X.X");
        assert_eq!(glyph('N').unwrap()[3], "X..XX");
    }

    #[test]
    fn long_text_clips_instead_of_wrapping() {
        // Audit P1-9: text running past the right edge must drop, never
        // reappear at column 0.
        let mut g = Grid::new(10, 5, 1, 1);
        draw_text(&mut g, 8, 0, "EEEE", [255, 255, 255]);
        assert_eq!(g.char_at(0, 0), Some(' '), "column 0 must stay empty");
        assert_eq!(g.char_at(1, 0), Some(' '), "column 1 must stay empty");
        assert!(g.ink_count() > 0, "visible prefix must still ink");
    }

    #[test]
    fn sanitize_drops_what_the_font_cannot_draw() {
        // Audit F-043 / U-19: `(`, `)`, `=` and `!` have no glyph, and the
        // old hand-rolled folds lost them silently.
        assert_eq!(sanitize("ESC (OR Q) = QUIT!"), "ESC OR Q  QUIT");
        // Spaces survive: the gap between words is the point of them.
        assert_eq!(sanitize("a b  c"), "A B  C");
        // Everything the font carries survives, case-folded.
        assert_eq!(sanitize("x1.y:2-z/3\\_4"), "X1.Y:2-Z/3\\_4");
        // A text with nothing drawable is empty, not garbage.
        assert_eq!(sanitize("(=+!)"), "");
    }

    #[test]
    fn sanitize_width_matches_text_width() {
        // A caller that sanitizes first can never mis-centre: the measured
        // width of the sanitized text is the width the draw advances.
        let raw = "press (ESC) to exit!";
        let clean = sanitize(raw);
        assert_eq!(
            text_width(&clean),
            clean.chars().map(char_width).sum::<i64>()
        );
        // Sanitizing is width-transparent: an unsupported glyph measures
        // zero before and is gone after, so both orders centre the same
        // text the same way.
        assert_eq!(
            text_width(raw),
            text_width(&clean),
            "dropping unsupported glyphs never moves the text"
        );
        assert_ne!(sanitize(raw), raw, "the pin must use a text that changes");
    }

    #[test]
    fn text_width_is_case_insensitive() {
        // Audit P1-9 nit: the block font has no lowercase glyphs, so a
        // measuring helper that forgot to fold would report 0 for
        // "press esc" and silently mis-place the HUD.
        assert_eq!(text_width("press esc"), text_width("PRESS ESC"));
        assert!(text_width("press esc") > 0, "lowercase must still measure");
        assert_eq!(char_width('a'), char_width('A'));
        assert_eq!(char_width('a'), 6, "'A' is 5 cells wide plus the gap");
    }

    #[test]
    fn text_width_matches_drawn_extent() {
        let text = "PRESS ESC TO EXIT";
        let w = text_width(text);
        let mut g = Grid::new(w as usize + 4, 5, 1, 1);
        draw_text(&mut g, 0, 0, text, [255, 255, 255]);
        let text = g.to_text();
        let lines: Vec<&str> = text.lines().collect();
        let max_x = lines
            .iter()
            .flat_map(|l| {
                l.char_indices()
                    .filter(|(_, c)| *c == 'X')
                    .map(|(i, _)| i)
                    .max()
            })
            .max()
            .unwrap();
        assert!((max_x as i64) < w, "ink {max_x} must fit width {w}");
        assert!(
            (max_x as i64) >= w - 3,
            "ink {max_x} must reach the text end"
        );
    }
}
