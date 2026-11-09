//! The HELP window's **pure** half: the README text, laid out and
//! scrollable, with no window, no GPU and no file system.
//!
//! The window itself lives in the shell (`src/help.rs`); this module owns
//! everything the window shows: the minimal markdown strip, the one-time
//! line wrap, the scroll model and the painting into a
//! [`crate::grid::Grid`]. The shell reads the README at open time and
//! hands the text over — the core never opens a file (roadmap
//! HELP_WINDOW §1: the README is the single source of truth, and the
//! core stays platform-free and file-free).
//!
//! The shape mirrors [`crate::editor`]: pure state and paint helpers
//! here, the window and the event loop there, re-exported under the
//! shell module's name.
//!
//! The markdown strip is **minimal** on purpose (roadmap §1): drop the
//! marker characters — heading `#`s, emphasis `*`s and code-span
//! backticks, and the link brackets — keep headings visible by their
//! text (uppercased, with a rule under them), and let fenced code
//! through verbatim. It is not a markdown engine, and it never parses
//! nested structure.

use crate::grid::Grid;
use crate::hud_font;

/// Body-text cell in pixels — the classic 8 × 16 terminal cell: wide
/// enough that DejaVu Sans Mono reads as prose, small enough that a
/// 1536 px window shows 190 columns.
pub const HELP_CELL_W: u32 = 8;
/// Body-text cell height in pixels — see [`HELP_CELL_W`].
pub const HELP_CELL_H: u32 = 16;

/// Footer cell in pixels — the editor's UI cell (small enough for long
/// block-font labels).
pub const FOOTER_CELL_W: u32 = 6;
/// Footer cell height in pixels — see [`FOOTER_CELL_W`].
pub const FOOTER_CELL_H: u32 = 10;

/// Footer band height in UI rows: a spare row, the rule row, a gap and
/// the five-row block-font text — the spare padding sits **above** the
/// rule.
const FOOTER_UI_ROWS: u32 = 8;

/// Smallest HELP window. Wide enough for 80 body columns — a wrapped
/// README line stays readable below that.
pub const MIN_HELP_W: u32 = 640;
/// Smallest HELP window height: the footer band plus 20 body rows.
pub const MIN_HELP_H: u32 = 400;
/// Largest HELP window: a bigger one is only empty page.
const MAX_HELP_W: u32 = 1920;
/// Largest HELP window height — see [`MAX_HELP_W`].
const MAX_HELP_H: u32 = 1200;
/// Share of the monitor the HELP window covers, in percent — the
/// editor's share.
const MONITOR_SHARE_PERCENT: u64 = 80;

/// Colour of ordinary README prose.
const COLOR_BODY: [u8; 3] = [188, 188, 188];
/// Colour of a heading's text.
const COLOR_HEADING: [u8; 3] = [255, 255, 255];
/// Colour of the rule under a heading.
const COLOR_RULE: [u8; 3] = [96, 96, 96];
/// Colour of fenced code.
const COLOR_CODE: [u8; 3] = [150, 200, 150];
/// Colour of the footer's block-font text — its rule is [`COLOR_RULE`].
const COLOR_FOOTER: [u8; 3] = [200, 200, 200];

/// The footer's left text: how to leave, how to move.
const FOOTER_KEYS: &str = "ESC CLOSE - WHEEL OR PGUP PGDN SCROLLS";

/// The footer's text when the GPU never came up: the honest word, the
/// same shape the popup and the editor carry (audit 2026-10-23 §F-092).
const GPU_DEAD_TEXT: &str = "NO GPU - PRESS ESC TO EXIT";

/// What kind of line a laid-out README line is.
///
/// The kind decides the colour and the wrap rule: prose word-wraps,
/// code hard-splits (word-wrapping would mangle the alignment), and a
/// rule never wraps at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// A heading's text (markers stripped, uppercased).
    Heading,
    /// Ordinary prose or a blank line.
    Body,
    /// A line inside a fenced code block, verbatim.
    Code,
    /// The full-width rule under a heading.
    Rule,
}

/// One laid-out line: its text and the kind that decides its colour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpLine {
    /// What the line is.
    pub kind: LineKind,
    /// The stripped text; empty for a blank line and for a rule.
    pub text: String,
}

/// Strip the README's markdown down to plain lines (roadmap §1).
///
/// Fenced blocks toggle on a line that starts with ```` ``` ````; the
/// fence markers themselves are dropped and the content passes through
/// verbatim as [`LineKind::Code`]. Outside a fence, a heading line
/// loses its `#`s, is uppercased and is followed by a full-width
/// [`LineKind::Rule`]; every other line loses the inline markers
/// (`*`, `` ` ``, `[`, `]`) and keeps its text. Bullets keep their
/// leading `-`: it is the list's structure, not a formatting marker.
fn strip_markdown(src: &str) -> Vec<HelpLine> {
    let mut lines = Vec::new();
    let mut in_fence = false;
    for raw in src.lines() {
        let Some(line) = fence_or_text(raw, &mut in_fence) else {
            continue;
        };
        push_with_rule(&mut lines, line);
    }
    lines
}

/// Classify one raw source line: `None` when the line is a fence marker
/// (it toggles `in_fence` and renders nothing), otherwise the stripped
/// [`HelpLine`].
fn fence_or_text(raw: &str, in_fence: &mut bool) -> Option<HelpLine> {
    if raw.trim_start().starts_with("```") {
        *in_fence = !*in_fence;
        return None;
    }
    if *in_fence {
        return Some(HelpLine {
            kind: LineKind::Code,
            text: raw.to_string(),
        });
    }
    Some(strip_line(raw))
}

/// One non-fenced line: a heading (markers gone, uppercased) or body
/// text with the inline markers removed.
fn strip_line(raw: &str) -> HelpLine {
    let trimmed = raw.trim_start();
    if let Some(rest) = trimmed.strip_prefix('#') {
        let rest = rest.trim_start().trim_start_matches('#').trim_start();
        return HelpLine {
            kind: LineKind::Heading,
            text: strip_inline(rest).to_uppercase(),
        };
    }
    HelpLine {
        kind: LineKind::Body,
        text: strip_inline(raw).trim_end().to_string(),
    }
}

/// Remove the inline marker characters: emphasis `*`, code-span
/// backticks and link brackets. Everything else is kept as it stands.
fn strip_inline(text: &str) -> String {
    text.chars()
        .filter(|ch| !matches!(ch, '*' | '`' | '[' | ']'))
        .collect()
}

/// Push one stripped line; a heading is followed by a full-width rule.
fn push_with_rule(lines: &mut Vec<HelpLine>, line: HelpLine) {
    let is_heading = line.kind == LineKind::Heading;
    lines.push(line);
    if is_heading {
        lines.push(HelpLine {
            kind: LineKind::Rule,
            text: String::new(),
        });
    }
}

/// Wrap the stripped lines to `cols` cells. Runs **once**, at open and
/// at resize — the scroll is an offset into the result, never a
/// re-layout per frame (roadmap §1).
fn wrap_lines(lines: &[HelpLine], cols: usize) -> Vec<HelpLine> {
    let cols = cols.max(1);
    lines.iter().flat_map(|line| wrap_one(line, cols)).collect()
}

/// Wrap one line by its kind.
fn wrap_one(line: &HelpLine, cols: usize) -> Vec<HelpLine> {
    match line.kind {
        LineKind::Rule => vec![line.clone()],
        LineKind::Code => wrap_code(line, cols),
        _ => wrap_prose(line, cols),
    }
}

/// Word-wrap a prose or heading line; a blank line stays one blank line.
fn wrap_prose(line: &HelpLine, cols: usize) -> Vec<HelpLine> {
    if line.text.trim().is_empty() {
        return vec![HelpLine {
            kind: line.kind,
            text: String::new(),
        }];
    }
    let mut rows = Vec::new();
    let mut current = String::new();
    for word in line.text.split_whitespace() {
        fold_word(&mut rows, &mut current, word, cols);
    }
    if !current.is_empty() {
        rows.push(current);
    }
    to_lines(line.kind, rows)
}

/// Fold one word into the row accumulator, starting a new row when the
/// word no longer fits.
fn fold_word(rows: &mut Vec<String>, current: &mut String, word: &str, cols: usize) {
    if word.chars().count() > cols {
        split_long_word(rows, current, word, cols);
        return;
    }
    if current.is_empty() {
        *current = word.to_string();
        return;
    }
    if current.chars().count() + 1 + word.chars().count() <= cols {
        current.push(' ');
        current.push_str(word);
        return;
    }
    rows.push(std::mem::take(current));
    *current = word.to_string();
}

/// Flush the open row and hard-split a word that is longer than a whole
/// row; the word's last chunk stays open for the words that follow.
fn split_long_word(rows: &mut Vec<String>, current: &mut String, word: &str, cols: usize) {
    if !current.is_empty() {
        rows.push(std::mem::take(current));
    }
    let chunks = hard_chunks(word, cols);
    let Some((last, head)) = chunks.split_last() else {
        return;
    };
    rows.extend(head.iter().cloned());
    *current = last.clone();
}

/// Split `text` into `cols`-wide character chunks (a word longer than a
/// row, or a code line, cannot word-wrap without losing its shape).
fn hard_chunks(text: &str, cols: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    chars
        .chunks(cols.max(1))
        .map(|chunk| chunk.iter().collect())
        .collect()
}

/// Hard-split a code line to `cols`; the internal spacing survives.
fn wrap_code(line: &HelpLine, cols: usize) -> Vec<HelpLine> {
    if line.text.chars().count() <= cols {
        return vec![line.clone()];
    }
    to_lines(LineKind::Code, hard_chunks(&line.text, cols))
}

/// Re-attach a kind to wrapped rows.
fn to_lines(kind: LineKind, rows: Vec<String>) -> Vec<HelpLine> {
    rows.into_iter()
        .map(|text| HelpLine { kind, text })
        .collect()
}

/// The HELP window's scroll model: the laid-out lines plus the offset of
/// the first visible one. Pure data — the shell owns the window, the
/// keys and the wheel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpState {
    lines: Vec<HelpLine>,
    /// The column count the current `lines` were wrapped for.
    wrap_cols: usize,
    scroll: usize,
}

impl HelpState {
    /// Lay `text` out for a `cols`-cell-wide page, top of document.
    pub fn new(text: &str, cols: usize) -> Self {
        let cols = cols.max(1);
        Self {
            lines: wrap_lines(&strip_markdown(text), cols),
            wrap_cols: cols,
            scroll: 0,
        }
    }

    /// Re-wrap for a new page width, then clamp the offset to the
    /// `visible`-line page.
    ///
    /// The strip+wrap runs **only when the width changed**: an
    /// interactive border drag fires `Resized` for every intermediate
    /// size, and only the width reaches the wrap — a height-only resize
    /// costs a scroll clamp, not a re-layout. The text is assumed
    /// unchanged between calls: the shell reads it once, at open time.
    pub fn relayout(&mut self, text: &str, cols: usize, visible: usize) {
        let cols = cols.max(1);
        if self.wrap_cols != cols {
            self.lines = wrap_lines(&strip_markdown(text), cols);
            self.wrap_cols = cols;
        }
        self.scroll = self.scroll.min(self.max_scroll(visible));
    }

    /// The laid-out line count.
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The first visible line's index.
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// The largest first-visible index for a `visible`-line page.
    pub fn max_scroll(&self, visible: usize) -> usize {
        self.lines.len().saturating_sub(visible.max(1))
    }

    /// Scroll by `delta` lines; positive moves towards the document's
    /// end. The offset clamps to the page.
    pub fn scroll_by(&mut self, delta: i32, visible: usize) {
        let max = self.max_scroll(visible);
        if delta >= 0 {
            self.scroll = (self.scroll + delta as usize).min(max);
        } else {
            self.scroll = self.scroll.saturating_sub(delta.unsigned_abs() as usize);
        }
    }

    /// Scroll by `pages` pages of `visible` lines.
    pub fn scroll_page(&mut self, pages: i32, visible: usize) {
        let page = i32::try_from(visible.max(1)).unwrap_or(i32::MAX);
        let delta = page.saturating_mul(pages);
        self.scroll_by(delta, visible);
    }

    /// Jump to the top of the document.
    pub fn scroll_to_top(&mut self) {
        self.scroll = 0;
    }

    /// Jump to the end of the document: the last full page.
    pub fn scroll_to_bottom(&mut self, visible: usize) {
        self.scroll = self.max_scroll(visible);
    }

    /// The lines of the `visible`-line page at the current offset.
    pub fn visible(&self, visible: usize) -> &[HelpLine] {
        let start = self.scroll.min(self.lines.len());
        let end = (start + visible).min(self.lines.len());
        &self.lines[start..end]
    }

    /// The footer's `LINE x/y` numbers: the top visible line (1-based)
    /// and the document length.
    pub fn position(&self) -> (usize, usize) {
        (self.scroll.min(self.lines.len()) + 1, self.lines.len())
    }
}

/// Body rows a window of `height` px shows: everything above the footer
/// band, in [`HELP_CELL_H`] rows.
pub fn body_rows_for(height: u32) -> usize {
    let band = FOOTER_UI_ROWS * FOOTER_CELL_H;
    (height.saturating_sub(band) / HELP_CELL_H) as usize
}

/// Body columns a window of `width` px shows, in [`HELP_CELL_W`] cells.
pub fn body_cols_for(width: u32) -> usize {
    (width / HELP_CELL_W).max(1) as usize
}

/// The HELP window's size: 80 % of the monitor, clamped into the min/max
/// window, in pixels.
pub fn help_window_rect(monitor_w: u32, monitor_h: u32) -> (u32, u32) {
    (
        scaled_side(monitor_w, MIN_HELP_W, MAX_HELP_W),
        scaled_side(monitor_h, MIN_HELP_H, MAX_HELP_H),
    )
}

/// One clamped 80 %-of-monitor side.
fn scaled_side(monitor: u32, min: u32, max: u32) -> u32 {
    let scaled = u64::from(monitor) * MONITOR_SHARE_PERCENT / 100;
    scaled.clamp(u64::from(min), u64::from(max)) as u32
}

/// The centred top-left corner of the HELP window on its monitor.
///
/// The rect is centred on the monitor origin, so a monitor smaller than
/// the minimum window keeps the window at its minimum (the same rule
/// the editor's origin follows).
pub fn help_window_origin(
    monitor_origin: (i32, i32),
    monitor_w: u32,
    monitor_h: u32,
) -> (i32, i32) {
    let (w, h) = help_window_rect(monitor_w, monitor_h);
    let x = i64::from(monitor_origin.0) + (i64::from(monitor_w) - i64::from(w)) / 2;
    let y = i64::from(monitor_origin.1) + (i64::from(monitor_h) - i64::from(h)) / 2;
    (x as i32, y as i32)
}

/// Colour of one line kind.
fn line_color(kind: LineKind) -> [u8; 3] {
    match kind {
        LineKind::Heading => COLOR_HEADING,
        LineKind::Code => COLOR_CODE,
        LineKind::Rule => COLOR_RULE,
        LineKind::Body => COLOR_BODY,
    }
}

/// Paint the visible page into the body grid.
pub fn paint_body(grid: &mut Grid, state: &HelpState, visible: usize) {
    grid.clear();
    let page = state.visible(visible);
    for (row, line) in page.iter().enumerate() {
        paint_line(grid, row as i64, line);
    }
}

/// Paint one laid-out line at `row`.
fn paint_line(grid: &mut Grid, row: i64, line: &HelpLine) {
    if line.kind == LineKind::Rule {
        paint_rule_row(grid, row);
        return;
    }
    let color = line_color(line.kind);
    for (col, ch) in line.text.chars().enumerate() {
        if ch == ' ' {
            continue;
        }
        grid.set_if_visible(col as i64, row, ch, color);
    }
}

/// Paint the footer band into the overlay grid: the key hints on the
/// left, the `LINE x/y` position on the right, a rule above.
pub fn paint_footer(grid: &mut Grid, state: &HelpState) {
    grid.clear();
    let rows = grid.rows() as i64;
    if rows < FOOTER_UI_ROWS as i64 {
        return;
    }
    paint_rule_row(grid, rows - FOOTER_RULE_FROM_BOTTOM);
    let (left, right) = footer_texts(state, grid.cols());
    paint_footer_text(grid, rows - FOOTER_TEXT_FROM_BOTTOM, &left, &right);
}

/// Rows from the overlay grid's bottom to the rule row.
const FOOTER_RULE_FROM_BOTTOM: i64 = 7;
/// Rows from the overlay grid's bottom to the top of the 5-row
/// block-font text.
pub const FOOTER_TEXT_FROM_BOTTOM: i64 = 5;

/// The rule row above the footer text: one full-width row of dashes —
/// the same stroke a heading's rule paints.
fn paint_rule_row(grid: &mut Grid, row: i64) {
    (0..grid.cols() as i64).for_each(|col| grid.set_if_visible(col, row, '-', COLOR_RULE));
}

/// The footer's two texts: the key hints (dropped when the window is too
/// narrow to hold both) and the `LINE x/y` position.
fn footer_texts(state: &HelpState, cols: usize) -> (String, String) {
    let (top, total) = state.position();
    let right = format!("LINE {top}/{total}");
    let left_width = hud_font::text_width(FOOTER_KEYS);
    let right_width = hud_font::text_width(&right);
    let gap = 4;
    // The left text starts at col 1 and the right text ends one col
    // short of the edge (`paint_footer_text`), so the margins ride in
    // the fit check — the gap alone would let the hints overflow.
    let left_fits = 1 + left_width + gap + right_width < cols as i64;
    let left = match left_fits {
        true => FOOTER_KEYS.to_string(),
        false => String::new(),
    };
    (left, right)
}

/// Paint the footer's text row: the position right-aligned, the key
/// hints left-aligned.
fn paint_footer_text(grid: &mut Grid, row: i64, left: &str, right: &str) {
    let cols = grid.cols() as i64;
    let right_width = hud_font::text_width(right);
    hud_font::draw_text(grid, cols - right_width - 1, row, right, COLOR_FOOTER);
    if left.is_empty() {
        return;
    }
    hud_font::draw_text(grid, 1, row, left, COLOR_FOOTER);
}

/// Paint the footer's dead-GPU variant: the honest word instead of the
/// scroll position. The window still closes with ESC, and the title says
/// the rest (the shell sets it).
pub fn paint_footer_gpu_dead(grid: &mut Grid) {
    grid.clear();
    let rows = grid.rows() as i64;
    if rows < FOOTER_UI_ROWS as i64 {
        return;
    }
    paint_rule_row(grid, rows - FOOTER_RULE_FROM_BOTTOM);
    let row = rows - FOOTER_TEXT_FROM_BOTTOM;
    hud_font::draw_text(grid, 1, row, GPU_DEAD_TEXT, COLOR_FOOTER);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(text: &str) -> HelpLine {
        HelpLine {
            kind: LineKind::Body,
            text: text.to_string(),
        }
    }

    fn heading(text: &str) -> HelpLine {
        HelpLine {
            kind: LineKind::Heading,
            text: text.to_string(),
        }
    }

    fn code(text: &str) -> HelpLine {
        HelpLine {
            kind: LineKind::Code,
            text: text.to_string(),
        }
    }

    /// A scroll model over hand-built lines, skipping the strip+wrap.
    fn state_at(lines: Vec<HelpLine>, scroll: usize) -> HelpState {
        HelpState {
            lines,
            wrap_cols: 1,
            scroll,
        }
    }

    #[test]
    fn a_heading_loses_its_markers_and_gains_a_rule() {
        let lines = strip_markdown("## About ducks\n\nprose");
        assert_eq!(lines[0], heading("ABOUT DUCKS"));
        assert_eq!(lines[1].kind, LineKind::Rule);
        assert_eq!(lines[2], body(""));
        assert_eq!(lines[3], body("prose"));
    }

    #[test]
    fn inline_markers_are_dropped_but_text_survives() {
        let lines = strip_markdown("a **bold** `code` [link](url) stays");
        assert_eq!(lines[0], body("a bold code link(url) stays"));
    }

    #[test]
    fn fenced_code_passes_through_verbatim() {
        let src = "before\n```toml\n[audio]  # kept\nx = 1\n```\nafter";
        let lines = strip_markdown(src);
        assert_eq!(lines[0], body("before"));
        assert_eq!(lines[1], code("[audio]  # kept"));
        assert_eq!(lines[2], code("x = 1"));
        assert_eq!(lines[3], body("after"));
    }

    #[test]
    fn bullets_keep_their_dash() {
        let lines = strip_markdown("- one item");
        assert_eq!(lines[0], body("- one item"));
    }

    #[test]
    fn prose_word_wraps_at_the_column_budget() {
        let lines = wrap_lines(&[body("one two three four")], 8);
        assert_eq!(lines, vec![body("one two"), body("three"), body("four")]);
    }

    #[test]
    fn a_word_longer_than_a_row_hard_splits() {
        let lines = wrap_lines(&[body("abcdefghij")], 4);
        assert_eq!(lines, vec![body("abcd"), body("efgh"), body("ij")]);
    }

    #[test]
    fn blank_lines_survive_the_wrap() {
        let lines = wrap_lines(&[body("a"), body(""), body("b")], 10);
        assert_eq!(lines, vec![body("a"), body(""), body("b")]);
    }

    #[test]
    fn code_hard_splits_without_losing_spacing() {
        let lines = wrap_lines(&[code("    x = 12345678")], 8);
        assert_eq!(lines, vec![code("    x = "), code("12345678")]);
    }

    #[test]
    fn rules_never_wrap() {
        let lines = wrap_lines(
            &[HelpLine {
                kind: LineKind::Rule,
                text: String::new(),
            }],
            4,
        );
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].kind, LineKind::Rule);
    }

    #[test]
    fn the_scroll_clamps_to_the_page() {
        let lines: Vec<HelpLine> = (0..50).map(|i| body(&format!("line {i}"))).collect();
        let mut state = state_at(lines, 0);
        state.scroll_by(10, 20);
        assert_eq!(state.scroll(), 10);
        state.scroll_by(1000, 20);
        assert_eq!(state.scroll(), 30, "50 lines on a 20-line page");
        state.scroll_by(-1, 20);
        assert_eq!(state.scroll(), 29);
        state.scroll_by(-1000, 20);
        assert_eq!(state.scroll(), 0);
        state.scroll_page(1, 20);
        assert_eq!(state.scroll(), 20);
    }

    #[test]
    fn home_and_end_reach_the_document_edges() {
        let lines: Vec<HelpLine> = (0..50).map(|i| body(&format!("line {i}"))).collect();
        let mut state = state_at(lines, 0);
        state.scroll_to_bottom(20);
        assert_eq!(state.scroll(), 30);
        state.scroll_to_top();
        assert_eq!(state.scroll(), 0);
    }

    #[test]
    fn the_visible_page_is_the_scroll_window() {
        let lines: Vec<HelpLine> = (0..10).map(|i| body(&format!("l{i}"))).collect();
        let state = state_at(lines, 3);
        let page = state.visible(4);
        assert_eq!(page.len(), 4);
        assert_eq!(page[0].text, "l3");
        assert_eq!(page[3].text, "l6");
    }

    #[test]
    fn a_resize_keeps_the_scroll_inside_the_new_document() {
        let mut state = HelpState::new("word word word word word", 10);
        let long = state.line_count();
        state.scroll_by(100, 5);
        state.relayout("one line", 10, 5);
        assert!(
            state.scroll() < long,
            "the clamp drops the scroll back into the shrunken document"
        );
    }

    #[test]
    fn a_height_only_resize_clamps_to_the_last_full_page() {
        // The audit 2026-10-25 case: the clamp used to be `len - 1`,
        // which leaves a partial page (blank rows above) once the
        // document is shorter than the window. The clamp is
        // `max_scroll(visible)` now.
        let text = "a\nb\nc\nd\ne";
        let mut state = HelpState::new(text, 20);
        state.scroll_to_bottom(1);
        assert_eq!(state.scroll(), 4);
        state.relayout(text, 20, 2);
        assert_eq!(state.scroll(), 3, "the last full page, not len-1");
    }

    #[test]
    fn a_same_width_resize_skips_the_rewrap() {
        let mut state = HelpState::new("first", 20);
        let before = state.line_count();
        state.relayout("a different document with other words", 20, 5);
        assert_eq!(
            state.line_count(),
            before,
            "same width: the wrap is kept, only the scroll clamps"
        );
    }

    #[test]
    fn a_width_change_rewraps_and_keeps_the_scroll_inside() {
        let mut state = HelpState::new("one two three four five six", 12);
        let narrow = state.line_count();
        state.scroll_to_bottom(1);
        state.relayout("one two three four five six", 400, narrow);
        assert!(
            state.line_count() < narrow,
            "the wider page wraps into fewer lines"
        );
        assert_eq!(state.scroll(), state.max_scroll(narrow));
    }

    #[test]
    fn the_position_reports_the_top_line_one_based() {
        let lines: Vec<HelpLine> = (0..10).map(|i| body(&format!("l{i}"))).collect();
        let state = state_at(lines, 3);
        assert_eq!(state.position(), (4, 10));
    }

    #[test]
    fn the_window_rect_shares_the_monitor() {
        let (w, h) = help_window_rect(1920, 1080);
        assert_eq!(w, 1536);
        assert_eq!(h, 864);
        let (small_w, small_h) = help_window_rect(800, 500);
        assert_eq!(small_w, MIN_HELP_W, "the floor wins on a small monitor");
        assert_eq!(small_h, MIN_HELP_H);
        let (big_w, big_h) = help_window_rect(3840, 2160);
        assert_eq!(big_w, MAX_HELP_W, "the ceiling wins on a 4K monitor");
        assert_eq!(big_h, MAX_HELP_H);
    }

    #[test]
    fn the_window_origin_centres_on_the_monitor() {
        let origin = help_window_origin((0, 0), 1920, 1080);
        assert_eq!(origin, (192, 108));
        let offset = help_window_origin((-1920, -40), 1920, 1080);
        assert_eq!(offset, (-1728, -40 + 108));
    }

    #[test]
    fn body_geometry_follows_the_window() {
        assert_eq!(body_cols_for(1536), 192);
        assert_eq!(body_rows_for(864), 49);
        assert_eq!(body_rows_for(80), 0, "the footer band alone leaves no page");
    }

    #[test]
    fn the_body_paint_shows_the_visible_page() {
        let lines: Vec<HelpLine> = (0..30).map(|i| body(&format!("row{i}"))).collect();
        let state = state_at(lines, 2);
        let mut grid = Grid::new(20, 5, HELP_CELL_W, HELP_CELL_H);
        paint_body(&mut grid, &state, 5);
        assert_eq!(grid.char_at(0, 0), Some('r'));
        assert_eq!(grid.char_at(3, 0), Some('2'), "the top row is `row2`");
        assert_eq!(grid.char_at(0, 4), Some('r'));
        assert_eq!(grid.char_at(3, 4), Some('6'));
        assert_eq!(grid.char_at(0, 5), None, "the page ends at row 4");
    }

    #[test]
    fn a_heading_paints_bright_and_its_rule_spans_the_grid() {
        let state = HelpState::new("# TITLE", 10);
        let mut grid = Grid::new(10, 3, HELP_CELL_W, HELP_CELL_H);
        paint_body(&mut grid, &state, 3);
        assert_eq!(grid.color_at(0, 0), Some(COLOR_HEADING));
        for col in 0..10 {
            assert_eq!(grid.char_at(col, 1), Some('-'), "col {col}");
        }
    }

    #[test]
    fn the_footer_paints_the_rule_and_both_texts() {
        let lines: Vec<HelpLine> = (0..5).map(|i| body(&format!("l{i}"))).collect();
        let state = state_at(lines, 0);
        let (left, right) = footer_texts(&state, 300);
        assert_eq!(left, FOOTER_KEYS);
        assert_eq!(right, "LINE 1/5");
        let mut grid = Grid::new(120, 8, FOOTER_CELL_W, FOOTER_CELL_H);
        paint_footer(&mut grid, &state);
        assert_eq!(grid.char_at(0, 1), Some('-'), "the rule row");
        let text_row = grid.rows() as i64 - FOOTER_TEXT_FROM_BOTTOM;
        let ink = (0..grid.cols())
            .filter(|col| {
                grid.char_at(*col, text_row as usize)
                    .is_some_and(|c| c == 'X')
            })
            .count();
        assert!(ink > 0, "the text row carries block glyphs");
    }

    #[test]
    fn the_footer_drops_the_key_hints_on_a_narrow_grid() {
        let lines: Vec<HelpLine> = (0..5).map(|i| body(&format!("l{i}"))).collect();
        let state = state_at(lines, 0);
        let (left, right) = footer_texts(&state, 30);
        assert_eq!(left, "", "the hints are gone on a narrow window");
        assert_eq!(right, "LINE 1/5", "the position always stays");
    }

    #[test]
    fn the_dead_gpu_footer_says_so_instead_of_the_position() {
        let mut grid = Grid::new(120, 8, FOOTER_CELL_W, FOOTER_CELL_H);
        paint_footer_gpu_dead(&mut grid);
        assert_eq!(grid.char_at(0, 1), Some('-'), "the rule row");
        let text_row = grid.rows() as i64 - FOOTER_TEXT_FROM_BOTTOM;
        let mut any_ink = false;
        for col in 0..grid.cols() {
            if grid.char_at(col, text_row as usize) == Some('X') {
                any_ink = true;
            }
        }
        assert!(any_ink, "the dead-GPU word is stamped");
    }

    /// The real README strips and wraps without panicking, keeps its
    /// headings, and stays inside the column budget (the shell's `ducks
    /// help` runs this exact pipeline on the embedded fallback text).
    #[test]
    fn the_embedded_readme_lays_out_cleanly() {
        let readme = include_str!("../../../README.md");
        let state = HelpState::new(readme, 100);
        assert!(state.line_count() > 500, "{} lines", state.line_count());
        let headings: Vec<&HelpLine> = state
            .visible(state.line_count())
            .iter()
            .filter(|l| l.kind == LineKind::Heading)
            .collect();
        assert!(
            headings.iter().any(|l| l.text.contains("ABOUT")),
            "a heading survived: {headings:?}"
        );
        // Every laid-out line fits the page.
        let too_long = state
            .visible(state.line_count())
            .iter()
            .filter(|l| l.kind != LineKind::Rule && l.text.chars().count() > 100)
            .count();
        assert_eq!(too_long, 0, "no line overruns the page width");
    }
}
