//! The editor's **pure** half: the state, the layout and the paint
//! helpers, with no window, no GPU and no platform.
//!
//! This is the part of the draw-your-own editor that can become a
//! library: it owns the editor's **data** (the dropdowns, the canvas,
//! the name field, the undo stack, the note strip) and the **pure**
//! functions that work on that data — the layout, the hit test and the
//! painting into a [`crate::grid::Grid`].
//!
//! Nothing here touches a window, the GPU, the file system or a platform
//! location. The two places where the state needs the outside world —
//! the platform's asset folder and the user's `config.toml` — are
//! **injected**: [`EditorState::set_platform`] hands over the
//! platform's functions and [`EditorState::set_tracks_dir`] hands over
//! the folder the MIDI tracks live in. The application half does the
//! injecting, so the core never has to know where the platform keeps its
//! files.
//!
//! The other half lives in the application's editor module: the window,
//! the renderer, the workers and the event loop. It re-exports this
//! module, so every pure item keeps resolving under its old name.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Canvas/tool cell size in pixels — big enough to click comfortably.
pub const CANVAS_CELL_W: u32 = 16;
/// Canvas/tool cell height in pixels — see [`CANVAS_CELL_W`].
pub const CANVAS_CELL_H: u32 = 24;
/// UI cell size in pixels — small enough for long block-font labels.
pub const UI_CELL_W: u32 = 6;
/// UI cell height in pixels — see [`UI_CELL_W`].
pub const UI_CELL_H: u32 = 10;

/// Smallest editor window.
///
/// The width is **derived from the top bar** (see [`min_layout_width`]):
/// the three dropdown boxes, the name field at its minimum width, the
/// gaps and the three buttons. The OS enforces it
/// (`with_min_inner_size`), so the layout never has to render a control
/// it cannot show — in particular the name field can no longer vanish
/// while the user types into it (audit 2026-10-20 § editor, task 1).
pub const MIN_WINDOW_W: u32 = min_layout_width();
/// Smallest editor window height.
pub const MIN_WINDOW_H: u32 = 600;
/// Largest editor window: a bigger one is only empty canvas.
pub const MAX_WINDOW_W: u32 = 1920;
/// Largest editor window height.
pub const MAX_WINDOW_H: u32 = 1200;
/// Share of the monitor the editor covers, in percent.
pub const MONITOR_SHARE_PERCENT: u64 = 80;

/// Frame rate of the editor loop: 30 fps, the parade's `fps_cap` default.
/// The editor is a drawing tool, not an animation, so it does not follow
/// the configured parade rate.
pub const EDITOR_FPS: u32 = 30;

/// Base rows the top bar occupies, **derived** from the dropdown box's
/// own height: the smallest whole number of base (24 px) rows that fits
/// the box's 8 UI (10 px) rows — `ceil(80 / 24) = 4` today. Two
/// independent constants here were a 16 px dead strip waiting to drift
/// (audit 2026-10-23 §U-01); the agreement test
/// `the_top_bar_fits_the_dropdown_boxes` pins the derivation.
const TOP_BAR_BASE_ROWS: u32 = (BOX_UI_ROWS * UI_CELL_H).div_ceil(CANVAS_CELL_H);

/// UI rows of the status strip at the bottom of the window.
///
/// The strip holds **two** text rows — the status line and, below it,
/// the toast (audit 2026-10-23 §U-04): a toast that replaced the status
/// line for 2.5 s hid the very state the user had just changed, and on
/// a narrow window the strip status echo (§U-25) vanished with it. The
/// block font is five rows tall, so two lines plus a one-row gap take
/// 5 + 1 + 5 = 11 UI rows.
const STATUS_UI_ROWS: u32 = 11;

/// The UI row (of the status strip) the status line is drawn on.
const STATUS_TEXT_UI_ROW: i64 = 0;

/// The UI row (of the status strip) the toast is drawn on: below the
/// status line, with the block font's five rows between them.
const TOAST_UI_ROW: i64 = 6;

/// UI rows of a dropdown box: five for the label and three for the rule
/// below it. The rule is drawn as a `-` glyph, whose dash sits two rows
/// into its own cell, so the dash lands three rows under the label.
const BOX_UI_ROWS: u32 = 8;

/// UI cells between two dropdown boxes.
const BOX_GAP_UI_CELLS: u32 = 2;

/// UI cells of a dropdown box. The widest label (`BAL`, `SPR`, `BIG`,
/// `SML`) is 18 block-font cells wide; two cells of padding make 20.
const BOX_UI_CELLS: u32 = 20;

/// UI cells between the last dropdown box and the name field.
const NAME_FIELD_GAP_UI_CELLS: u32 = 4;

/// UI cells the name field needs before it is worth drawing.
///
/// The field shows the **tail** of the name and a caret, so the minimum
/// is a cell of padding on each side, three characters and the caret.
/// Below that the field would show a single letter, and a name the user
/// cannot read is a name they cannot check (audit 2026-10-20 § editor,
/// task 1) — so the window is never allowed to get that narrow.
pub const NAME_FIELD_MIN_UI_CELLS: u32 = 2 + (NAME_FIELD_MIN_CHARS + 1) * UI_CHAR_CELLS;

/// Characters the name field shows at its minimum width.
const NAME_FIELD_MIN_CHARS: u32 = 3;

/// UI cells the three dropdown boxes and the gap after them take.
const TOP_BAR_LEFT_CELLS: u32 = 3 * BOX_UI_CELLS + 2 * BOX_GAP_UI_CELLS + NAME_FIELD_GAP_UI_CELLS;

/// UI cells the **five** top-bar buttons take: `CFG`, `TRK`, `ASSIGN`,
/// `DRAW` and `SAVE` — the labels the bar actually draws, not their
/// long forms (audit 2026-10-23 §R6-08).
const TOP_BAR_BUTTON_CELLS: u32 = SETTINGS_BUTTON_UI_CELLS
    + TRACK_BUTTON_UI_CELLS
    + ASSIGN_BUTTON_UI_CELLS
    + DRAW_BUTTON_UI_CELLS
    + SAVE_BUTTON_UI_CELLS;

/// UI cells the top bar needs at its narrowest: the dropdown boxes, the
/// name field at its minimum, the gap and the **five** buttons (`CFG`,
/// `TRK`, `ASSIGN`, `DRAW`, `SAVE` — audit 2026-10-23 §U-15).
///
/// This is the floor of the editor's layout — the width below which a
/// control would have to disappear.
pub const TOP_BAR_MIN_CELLS: u32 =
    TOP_BAR_LEFT_CELLS + NAME_FIELD_MIN_UI_CELLS + NAME_FIELD_GAP_UI_CELLS + TOP_BAR_BUTTON_CELLS;

/// UI cells one character of the block font takes.
///
/// The glyphs are five cells wide, plus the one-cell gap the font
/// leaves between two characters. The width of the *widest* glyph is
/// used, so a budget computed with it always fits.
const UI_CHAR_CELLS: u32 = 6;

/// UI cells of the SAVE button in the top bar. The label `SAVE` is four
/// block-font characters (23 cells) plus a cell of padding on each side.
const SAVE_BUTTON_UI_CELLS: u32 = 26;

/// UI cells of the ASSIGN button in the top bar. The label `ASSIGN` is six
/// block-font characters (35 cells) plus a cell of padding on each side.
const ASSIGN_BUTTON_UI_CELLS: u32 = 37;

/// UI cells of the DRAW button in the top bar — four block-font characters
/// (23 cells) plus a cell of padding on each side.
const DRAW_BUTTON_UI_CELLS: u32 = 26;

/// UI cells of the SETTINGS button in the top bar — three block-font
/// characters (18 cells) plus a cell of padding on each side.
///
/// The label is short on purpose, like the kind dropdown's `BAL` / `SPR`:
/// the block font is six cells per character, so a long label would push
/// the top bar's floor past the width of a 1366 px screen. The status
/// line spells the word out.
const SETTINGS_BUTTON_UI_CELLS: u32 = 20;

/// UI cells of the TRACK button in the top bar — three block-font
/// characters (18 cells) plus a cell of padding on each side.
///
/// The label is `TRK`, like `CFG`: the block font is six cells per
/// character, so a longer label would push the top bar's floor past the
/// width of a 1366 px screen. The status line spells the word out.
const TRACK_BUTTON_UI_CELLS: u32 = 20;

/// UI rows one dropdown option occupies in the open list. The block font
/// is five rows tall, so the options stack without overlapping.
const OPTION_UI_ROWS: i64 = 5;

/// UI row of the rule under a box's label.
pub const RULE_UI_ROW: i64 = 5;

/// Width of the tool/colour panel in base cells: a marker column plus the
/// glyph column.
const TOOL_PANEL_CELLS: u32 = 2;

/// Width of the **pitch** panel in base cells, in strip mode: a marker
/// column plus seven label cells.
///
/// The panel is wider in strip mode because a pitch has a **name**
/// (`C#3`), not a glyph: the tool panel draws one character per row, the
/// pitch panel draws a word.
const PITCH_PANEL_CELLS: u32 = 8;

/// Rows of the tool/colour panel: the 19 tools, a separator and the 8
/// colours (roadmap §5.5).
pub const PANEL_ROWS: usize = crate::drawn::TOOLS.len() + 1 + crate::drawn::PALETTE.len();

/// Rows of the note strip's canvas: the caption, the note lane and the
/// ruler (audit 2026-10-23 §U-16).
///
/// The lane sat directly under the top bar with no header, and the
/// caption lived in the top bar, far from the lane — a caption row on
/// the canvas itself names the strip where the strip is: row 0 is the
/// caption, row 1 the note lane and row 2 the ruler. The caption is
/// chrome: it does not scroll with the steps and it takes no clicks
/// ([`strip_hit`] answers row 1 only).
pub const STRIP_ROWS: usize = 3;

/// The canvas row of the strip's caption — the header that names the
/// lane (audit 2026-10-23 §U-16). Chrome: fixed, not scrolled, not
/// clickable.
pub const STRIP_CAPTION_ROW: usize = 0;

/// The canvas row of the note lane — the row a click places a note on.
pub const STRIP_NOTE_ROW: usize = 1;

/// The canvas row of the ruler — the loop's start and end marks.
pub const STRIP_RULER_ROW: usize = 2;

/// The twelve pitches of the palette: one chromatic octave, C3 to B3
/// (roadmap Extension 3 §3.1 — "a chromatic run of the basic MIDI
/// notes").
///
/// The palette is a `const` array, so the pitch a note gets is a compile
/// time fact and the panel and the strip cannot drift apart.
pub const PITCH_NAMES: [&str; 12] = [
    "C3", "C#3", "D3", "D#3", "E3", "F3", "F#3", "G3", "G#3", "A3", "A#3", "B3",
];

/// The MIDI pitch of every palette entry: C3 is 48, so the run is 48..=59.
pub const PITCH_MIDI: [u8; 12] = [48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59];

/// The block-font alias of every palette entry: `#` has no glyph, so a
/// sharp is written as `-` (`C#3` → `C-3`). Seven of the twelve pitches
/// are sharps, and each of them rendered as its natural neighbour
/// (audit 2026-10-23 §U-24).
pub const PITCH_DISPLAYS: [&str; 12] = [
    "C3", "C-3", "D3", "D-3", "E3", "F3", "F-3", "G3", "G-3", "A3", "A-3", "B3",
];

/// The colour of every palette entry.
///
/// The eight drawn colours are cycled so that no pitch is drawn in the
/// grey the muted labels use.
const PITCH_COLORS: [[u8; 3]; 12] = [
    crate::drawn::PALETTE[1],
    crate::drawn::PALETTE[2],
    crate::drawn::PALETTE[3],
    crate::drawn::PALETTE[4],
    crate::drawn::PALETTE[5],
    crate::drawn::PALETTE[6],
    crate::drawn::PALETTE[0],
    crate::drawn::PALETTE[1],
    crate::drawn::PALETTE[2],
    crate::drawn::PALETTE[3],
    crate::drawn::PALETTE[4],
    crate::drawn::PALETTE[5],
];

/// How long a toast stays on the status line (roadmap §5.2).
const TOAST_MILLIS: u64 = 2500;

/// The toast that asks for the next pose of an animated sprite.
const NEXT_FRAME_TOAST: &str = "PLEASE DRAW SECOND POSE FOR THE SPRITE";

/// The toast that refuses to overwrite an existing asset.
const NAME_IN_USE: &str = "NAME IN USE";

/// The toast the SAVE press shows **before** the write runs (decision
/// F-163, roadmap HELP window §3.1): the write blocks the UI thread, so
/// a toast set in the same handler run could never paint — the press
/// arms the save instead, the next frame paints this, and the save
/// completes on the tick after it.
const SAVING_TOAST: &str = "SAVING...";

/// The toast that reports an assignment (`ASSIGNED SMALL 3 = CAT`).
pub const ASSIGNED: &str = "ASSIGNED";

/// The toast that reports a removed assignment (`CLEARED SMALL 3`).
pub const CLEARED: &str = "CLEARED";

/// The toast that says the assign panel has nothing to assign yet.
pub const NO_ASSET_PICKED: &str = "PICK AN ASSET FIRST";

/// The label of the asset list when the user has drawn nothing yet.
const NO_ASSETS: &str = "NO ASSETS YET";

/// The toast that refuses a track name the track folder cannot hold.
pub const TRACK_NAME_INVALID: &str = "TRACK NAME INVALID";

/// The placeholder of the name field in strip mode: the name of the
/// track the file is written under.
const TRACK_NAME_PLACEHOLDER: &str = "TRACK-NAME";

/// The caption the top bar carries in strip mode, where the dropdown
/// boxes of the drawing would be.
const STRIP_CAPTION: &str = "NOTE STRIP";

/// The caption of the note lane itself, on the strip's own canvas
/// (row 0 — audit 2026-10-23 §U-16).
const STRIP_LANE_CAPTION: &str = "NOTES";

/// The glyph a note is drawn with on the strip.
pub const NOTE_GLYPH: char = '#';

/// The glyph that marks the **start** of the loop on the ruler.
pub const RULER_START: char = '[';

/// The glyph that marks the **end** of the loop on the ruler: the moment
/// the trailing duck clears the left edge (roadmap Extension 3 §3.4).
pub const RULER_END: char = ']';

/// The glyph of an ordinary step of the ruler.
pub const RULER_TICK: char = '-';

/// The velocity a placed note carries.
///
/// A hand-made track is a sketch: the note is there or it is not, and
/// the velocity is the one the synth plays by default.
const DEFAULT_VELOCITY: u8 = 100;

/// The number of assignment slots: two big ducks, nine small ducks and
/// five balloons (roadmap §3.8, decision D11).
pub const SLOT_COUNT: usize = 16;

/// The number of rows the assign panel's list holds: the sixteen parade
/// slots plus the three look targets (roadmap HELP window §3, decision
/// F-162) — the grass tile and the two decorations.
pub const ASSIGN_TARGET_COUNT: usize = SLOT_COUNT + 3;

/// What one row of the assign panel's list assigns.
///
/// The sixteen [`crate::config::AssignmentSlot`] rows write the
/// `[assignments]` table; the three **look** targets write one scalar
/// key each — the grass tile (`[grass] tile`) and the decorations the
/// big and the small ducks wear (`[decorations] big` / `small`). The
/// same two-column panel serves both: targets on the left, the target's
/// kind of asset on the right, one surgical write per click.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssignTarget {
    /// One of the sixteen parade slots.
    Slot(crate::config::AssignmentSlot),
    /// The grass tile the parade tiles its ground with.
    GrassTile,
    /// The decoration the big ducks wear.
    DecorationBig,
    /// The decoration the small ducks wear.
    DecorationSmall,
}

/// The assign panel's rows, in display order: the sixteen parade slots,
/// then the three look targets (decision F-162).
pub fn assign_targets() -> Vec<AssignTarget> {
    let mut targets: Vec<AssignTarget> = assignment_slots()
        .into_iter()
        .map(AssignTarget::Slot)
        .collect();
    targets.push(AssignTarget::GrassTile);
    targets.push(AssignTarget::DecorationBig);
    targets.push(AssignTarget::DecorationSmall);
    targets
}

/// The label of an assign target: `BIG 1`, `GRASS`, `DECO BIG`.
///
/// The labels stay short — the slot list column is
/// [`SLOT_LIST_CELLS`] wide and the block font is six cells per
/// character.
pub fn target_label(target: &AssignTarget) -> String {
    match target {
        AssignTarget::Slot(slot) => slot_label(slot),
        AssignTarget::GrassTile => "GRASS".to_string(),
        AssignTarget::DecorationBig => "DECO BIG".to_string(),
        AssignTarget::DecorationSmall => "DECO SMALL".to_string(),
    }
}

/// The kind of drawn asset an assign target takes.
fn target_drawn_kind(target: &AssignTarget) -> crate::drawn::DrawnKind {
    match target {
        AssignTarget::Slot(slot) => slot_drawn_kind(slot.kind),
        AssignTarget::GrassTile => crate::drawn::DrawnKind::Grass,
        AssignTarget::DecorationBig | AssignTarget::DecorationSmall => {
            crate::drawn::DrawnKind::Decoration
        }
    }
}

/// The duck size an assign target's art must carry, when it matters.
fn target_drawn_size(target: &AssignTarget) -> Option<crate::drawn::DrawnSize> {
    match target {
        AssignTarget::Slot(slot) => slot_drawn_size(slot.kind),
        AssignTarget::GrassTile => None,
        AssignTarget::DecorationBig => Some(crate::drawn::DrawnSize::Big),
        AssignTarget::DecorationSmall => Some(crate::drawn::DrawnSize::Small),
    }
}

/// Base cells of the slot list column in assign mode. The longest label
/// (`BALLOON 5`) is eight characters, plus the marker column and a gutter.
const SLOT_LIST_CELLS: u32 = 14;

/// The colour of an active label.
pub const UI_TEXT: [u8; 3] = [255, 255, 255];
/// The colour of an inactive (greyed) label — the palette's grey.
pub const UI_MUTED: [u8; 3] = crate::drawn::PALETTE[7];
/// The colour of the toast message.
const UI_TOAST: [u8; 3] = crate::drawn::PALETTE[3];

/// The short labels of the frame-count dropdown (decision D15).
const FRAME_LABELS: [&str; 4] = ["1", "2", "3", "4"];

/// The long forms of the frame-count dropdown, spelled out in the status
/// line (decision D15).
const FRAME_LONGS: [&str; 4] = ["STATIC", "2 FRAMES", "3 FRAMES", "4 FRAMES"];

/// How many strokes the undo stack keeps (roadmap Extension 2 §2.1).
///
/// 200 steps is far more than a human uses between two saves, and the
/// bound keeps the memory flat: the oldest stroke is dropped when the
/// stack overflows.
pub const UNDO_LIMIT: usize = 200;

/// How long the editor waits for quiet before it writes the draft
/// (roadmap Extension 2 §2.2).
///
/// The draft is written after a **stroke**, not after a cell: a fast
/// drag would otherwise write the whole drawing a dozen times a second.
pub const DRAFT_DEBOUNCE_MILLIS: u64 = 1000;

/// The status line's offer of a draft the editor found.
///
/// The offer says **both** answers: `ENTER` loads the draft, `ESC`
/// declines it and keeps the file for next time (audit 2026-10-23
/// §U-27 — the old wording offered no way out and held the status line
/// hostage). The toast raised with the offer and the status line share
/// this one constant; a second copy of the same text was deleted
/// (audit 2026-10-23 §prior-54).
const DRAFT_OFFER: &str = "DRAFT FOUND - ENTER LOADS ESC SKIPS";

/// The toast that reports a loaded draft.
pub const DRAFT_LOADED: &str = "DRAFT LOADED";

/// One painted cell, before and after the paint.
///
/// A stroke is recorded as a **delta**, not as a snapshot: a stroke that
/// paints 200 cells is 200 small records, while a snapshot of the whole
/// canvas would be 22 500 cells *per stroke* (roadmap Extension 2 §2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellDelta {
    /// The column of the cell in the canvas.
    pub col: usize,
    /// The row of the cell in the canvas.
    pub row: usize,
    /// The glyph and colour the cell had before the paint.
    pub before: (char, [u8; 3]),
    /// The glyph and colour the cell has after the paint.
    pub after: (char, [u8; 3]),
}

/// One stroke: the cells one press-and-drag changed.
///
/// A stroke is the **unit of undo** (roadmap Extension 2 §2.1): the
/// pointer goes down, paints a run of cells and comes up. Undoing a
/// 200-cell stroke is one action, not two hundred.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stroke {
    /// The changed cells, in the order they were painted.
    pub cells: Vec<CellDelta>,
}

impl Stroke {
    /// True when the stroke changed nothing.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// The undo/redo stack: a bounded list of strokes plus the redo side.
///
/// The stack lives in the **pure** state, so the whole feature is
/// testable without a window: paint, undo, redo, assert the canvas
/// (roadmap Extension 2 §2.1).
#[derive(Debug, Clone, Default)]
pub struct UndoStack {
    /// The strokes that can be undone, oldest first.
    ///
    /// A [`VecDeque`], because the overflow drops the **oldest** entry:
    /// a `Vec` had to shift the whole stack with `remove(0)` every time
    /// the bound was reached (audit 2026-10-23 §R6-07), while the deque
    /// pops the front in O(1).
    ///
    /// Public so the **shell's** tests can read the stack from outside
    /// the core: the pure half moves into `agnosis-core`, and the window's
    /// own tests live on this side of the seam.
    pub done: VecDeque<Stroke>,
    /// The strokes that can be redone, most recently undone last.
    ///
    /// Public for the same reason as [`UndoStack::done`].
    pub undone: VecDeque<Stroke>,
}

impl UndoStack {
    /// A new, empty stack.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a stroke onto the undo stack.
    ///
    /// A stroke that changed nothing is dropped: there is nothing to
    /// undo. Pushing a stroke clears the redo side, because a new edit
    /// makes the undone strokes unreachable.
    ///
    /// The stack is bounded: the oldest stroke is dropped when it
    /// overflows, so the memory never grows without limit.
    pub fn push(&mut self, stroke: Stroke) {
        if stroke.is_empty() {
            return;
        }
        self.undone.clear();
        self.done.push_back(stroke);
        while self.done.len() > UNDO_LIMIT {
            self.done.pop_front();
        }
    }

    /// Undo the last stroke, returning it.
    ///
    /// `None` when there is nothing to undo.
    pub fn pop_undo(&mut self) -> Option<Stroke> {
        let stroke = self.done.pop_back()?;
        self.undone.push_back(stroke.clone());
        Some(stroke)
    }

    /// Redo the last undone stroke, returning it.
    ///
    /// `None` when there is nothing to redo.
    pub fn pop_redo(&mut self) -> Option<Stroke> {
        let stroke = self.undone.pop_back()?;
        self.done.push_back(stroke.clone());
        Some(stroke)
    }

    /// True when a stroke can be undone.
    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    /// True when a stroke can be redone.
    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Drop every stroke — the drawing itself changed shape, so the old
    /// steps no longer describe anything.
    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }
}

/// What an undo shortcut asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoAction {
    /// Undo the last stroke.
    Undo,
    /// Redo the last undone stroke.
    Redo,
}

/// One change on the note strip: the note a step held before and after.
///
/// The strip's history is a **delta** list, like the canvas's: a click
/// that places a note is one record, not a snapshot of the whole track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NoteDelta {
    /// The step the change happened on.
    pub step: usize,
    /// The note the step held before the change.
    pub before: Option<crate::audio::Note>,
    /// The note the step holds after the change.
    pub after: Option<crate::audio::Note>,
}

/// The strip's undo history: a bounded list of note changes.
///
/// The canvas's [`UndoStack`] cannot be reused here: it stores **cell**
/// deltas, and a note is not a cell. The bound is the same
/// ([`UNDO_LIMIT`]), so the memory stays flat.
#[derive(Debug, Clone, Default)]
pub struct StripUndo {
    /// The changes that can be undone, oldest first — a deque, so the
    /// overflow drops the oldest entry without shifting the list.
    done: VecDeque<NoteDelta>,
    /// The changes that can be redone, most recently undone last.
    undone: VecDeque<NoteDelta>,
}

impl StripUndo {
    /// A new, empty history.
    pub fn new() -> Self {
        Self::default()
    }

    /// Push a change onto the undo history.
    ///
    /// A change that did nothing is dropped, and a new change clears the
    /// redo side: the undone changes no longer describe the track.
    pub fn push(&mut self, delta: NoteDelta) {
        self.undone.clear();
        self.done.push_back(delta);
        while self.done.len() > UNDO_LIMIT {
            self.done.pop_front();
        }
    }

    /// Undo the last change, returning it.
    pub fn pop_undo(&mut self) -> Option<NoteDelta> {
        let delta = self.done.pop_back()?;
        self.undone.push_back(delta);
        Some(delta)
    }

    /// Redo the last undone change, returning it.
    pub fn pop_redo(&mut self) -> Option<NoteDelta> {
        let delta = self.undone.pop_back()?;
        self.done.push_back(delta);
        Some(delta)
    }

    /// True when a change can be undone.
    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    /// True when a change can be redone.
    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    /// Drop every change — the track changed shape, so the old steps no
    /// longer describe anything.
    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }
}

/// What the user asked the track worker to do.
///
/// The request is **pure data**: the state never touches the file
/// system, and the app turns the request into a job for the worker
/// thread (roadmap Extension 3 §3.2).
#[derive(Debug, Clone, PartialEq)]
pub enum TrackRequest {
    /// Write the track file.
    Save {
        /// The name the file is written under.
        name: String,
        /// The notes to write.
        track: crate::audio::Track,
    },
    /// Read the track file into the strip.
    Load {
        /// The name of the file to read.
        name: String,
    },
    /// Point the parade at the track (`[audio] track`).
    Select {
        /// The name of the track the parade should play.
        name: String,
    },
}

/// Where the strip stands relative to its track file.
///
/// This is the **declared** form of what used to be two independent
/// bools (`track_saved` + `track_loaded`): the pair encoded six states of
/// which only three were reachable, and an impossible one — "saved but
/// never read" — was expressible (audit 101126 §C3). The pending
/// save/load/select job stays a typed [`TrackRequest`] in
/// [`EditorState::track_request`]; this enum owns exactly the sync state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrackSync {
    /// No track file has been read into the strip yet: the configured
    /// track is read the first time the strip mode opens.
    Fresh,
    /// The strip holds exactly what the track file holds — or holds
    /// nothing, matching a file that is not there (a failed first read
    /// lands here too: the empty strip is the truth).
    Synced,
    /// The strip holds notes the file does not: unsaved work the exit
    /// path must ask about.
    Dirty,
}

/// What a track write did.
///
/// The outcome is the **only** thing the strip learns from the worker:
/// the state is never told that a write succeeded before it did
/// (roadmap Extension 3 §3.4 — a broken track is reported, never
/// guessed).
#[derive(Debug, Clone, PartialEq)]
pub enum TrackOutcome {
    /// The track file was written.
    Saved {
        /// The name the file was written under.
        name: String,
    },
    /// The track file was read into the strip.
    Loaded {
        /// The name the file was read from.
        name: String,
        /// The notes it holds.
        track: crate::audio::Track,
    },
    /// The parade now plays the track.
    Selected {
        /// The name of the track the parade plays.
        name: String,
    },
    /// The write failed.
    Failed {
        /// What was being done: `SAVE`, `LOAD` or `SELECT`.
        action: &'static str,
        /// Why it failed.
        reason: String,
    },
}

/// A disk action the editor wants done after the current frame.
///
/// The actions are queued in the **pure** state and carried out by the
/// app, so the state never touches the file system (roadmap Extension 2
/// §2.2).
#[derive(Debug, Clone, PartialEq)]
pub enum DraftAction {
    /// Write the draft of the named asset.
    Write {
        /// The kind of asset the draft belongs to.
        kind: crate::drawn::DrawnKind,
        /// The asset name the draft belongs to.
        name: String,
        /// The payload.
        doc: crate::drawn::DraftDoc,
    },
    /// Delete the draft of the named asset.
    Delete {
        /// The kind of asset the draft belongs to.
        kind: crate::drawn::DrawnKind,
        /// The asset name the draft belongs to.
        name: String,
    },
}

/// The writer the settings panel uses: it puts **one key** into the
/// user's `config.toml`.
///
/// It is a boxed closure so the tests can install their own and never
/// touch the user's real config file.
pub type SettingsWriter = Box<dyn Fn(&str, &str, &str) -> Result<(), String>>;

/// The runner a settings-panel **action** goes through (the PERSONALITY
/// buttons, roadmap HELP window §2): the row's key in, a success or the
/// reason it did not land out.
///
/// It is a boxed closure so the tests can install their own and never
/// touch the user's real config file.
pub type SettingsAction = Box<dyn Fn(&str) -> Result<(), String>>;

/// The write an assign-panel action asks for (decision F-162).
///
/// The sixteen parade slots write the **whole** `[assignments]` table;
/// the look targets (grass, decorations) write **one scalar key** each —
/// the same surgical single-key write the settings panel uses. The
/// writer is the app's seam either way; the enum just says which shape
/// the write has.
#[derive(Debug, Clone, PartialEq)]
pub enum AssignWrite {
    /// The whole `[assignments]` table, as it should read after the
    /// write. Boxed: the table is by far the larger payload of the two
    /// variants, and an assign carries at most one.
    Assignments(Box<crate::config::AssignmentsConfig>),
    /// One scalar key: `section`, `key`, the value already rendered as
    /// TOML — an empty string clears the key.
    Key {
        /// The table the key lives in.
        section: &'static str,
        /// The key inside the table.
        key: &'static str,
        /// The value, rendered as TOML (a quoted string).
        value: String,
    },
}

/// Resolved placement for one editor invocation.
#[derive(Debug, Clone, Copy)]
pub struct EditorArgs {
    /// Top-left corner of the **monitor** in physical pixels.
    pub origin: (i32, i32),
    /// Size of the **monitor** in physical pixels. The editor computes its
    /// own window rect from it (80 %, clamped, centred).
    pub size: (u32, u32),
}

/// Editor failures.
///
/// GPU failures are not errors: a failed init or a failed draw degrades to
/// the black window (see `degraded`), so the window/event-loop and the
/// argument check are the only error paths.
#[derive(Debug, thiserror::Error)]
pub enum EditorError {
    /// The window or the event loop failed.
    #[error("window error: {0}")]
    Window(String),
    /// Bad arguments or an unusable config.
    #[error("config error: {0}")]
    Config(String),
}

/// Validate the editor args before the window layer is touched.
pub fn validate_args(args: &EditorArgs) -> Result<(), EditorError> {
    if args.size.0 == 0 || args.size.1 == 0 {
        return Err(EditorError::Config(format!(
            "monitor size must be nonzero, got {}x{}",
            args.size.0, args.size.1
        )));
    }
    Ok(())
}

/// The editor window size for a monitor: 80 % of it, clamped to
/// [`MIN_WINDOW_W`] × [`MIN_WINDOW_H`] … 1920 × 1200.
///
/// A small monitor still gets the minimum (the editor stays usable), a
/// huge one is capped (a bigger window is only empty canvas).
pub fn editor_window_rect(monitor_w: u32, monitor_h: u32) -> (u32, u32) {
    (
        scaled_side(monitor_w, MIN_WINDOW_W, MAX_WINDOW_W),
        scaled_side(monitor_h, MIN_WINDOW_H, MAX_WINDOW_H),
    )
}

/// The width the editor's window needs so the **balloon's disc** fits on
/// screen in full: the disc's cells plus the tool panel.
///
/// The balloon's canvas is the widest element the editor draws in full
/// (the sprite's 150-cell canvas is wider still and has always been
/// clipped at the window edge). The window opens wide enough for the
/// disc, so the user can paint the whole circle; a window the user
/// narrows below that clips the canvas, as it always did.
pub fn balloon_canvas_width(state: &EditorState) -> u32 {
    let cells = state.balloon_geo.cols as u32 + TOOL_PANEL_CELLS;
    cells * CANVAS_CELL_W
}

/// The editor window size for a monitor, wide enough for the canvas.
///
/// The width is 80 % of the monitor (clamped), but never narrower than
/// [`balloon_canvas_width`] and never wider than the monitor itself.
pub fn editor_window_rect_for(monitor_w: u32, monitor_h: u32, min_width: u32) -> (u32, u32) {
    let (_, height) = editor_window_rect(monitor_w, monitor_h);
    let share = scaled_side(monitor_w, MIN_WINDOW_W, MAX_WINDOW_W);
    let width = share.max(min_width).min(monitor_w.max(MIN_WINDOW_W));
    (width, height)
}

/// 80 % of `monitor`, clamped into `min..=max`.
fn scaled_side(monitor: u32, min: u32, max: u32) -> u32 {
    let scaled = u64::from(monitor) * MONITOR_SHARE_PERCENT / 100;
    scaled.clamp(u64::from(min), u64::from(max)) as u32
}

/// The centred top-left corner of the editor window on its monitor.
///
/// The rect is centred on the monitor origin, so a monitor that is smaller
/// than the minimum window puts the overflow half off the screen — the
/// window keeps its minimum size rather than shrinking below it.
pub fn editor_window_origin(
    monitor_origin: (i32, i32),
    monitor_w: u32,
    monitor_h: u32,
    min_width: u32,
) -> (i32, i32) {
    let (w, h) = editor_window_rect_for(monitor_w, monitor_h, min_width);
    let x = i64::from(monitor_origin.0) + (i64::from(monitor_w) - i64::from(w)) / 2;
    let y = i64::from(monitor_origin.1) + (i64::from(monitor_h) - i64::from(h)) / 2;
    (x as i32, y as i32)
}

/// A pixel rectangle in window coordinates.
///
/// The editor has no `Rect` type of its own yet, so Stage 5 introduces
/// this minimal one: the origin, the size and a `contains` hit test. The
/// hit test is what Stage 6 (clicking a canvas cell, a dropdown, a tool)
/// builds on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    /// Left edge in pixels, from the window's left border.
    pub x: i64,
    /// Top edge in pixels, from the window's top border.
    pub y: i64,
    /// Width in pixels.
    pub w: u32,
    /// Height in pixels.
    pub h: u32,
}

impl Rect {
    /// A rectangle at `(x, y)` with the given size.
    pub fn new(x: i64, y: i64, w: u32, h: u32) -> Self {
        Self { x, y, w, h }
    }

    /// True when the pixel `(x, y)` lies inside the rectangle.
    ///
    /// The right and bottom edges are exclusive, so two adjacent
    /// rectangles never claim the same pixel.
    pub fn contains(&self, x: i64, y: i64) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    /// The right edge in pixels (exclusive).
    pub fn right(&self) -> i64 {
        self.x + i64::from(self.w)
    }

    /// The bottom edge in pixels (exclusive).
    pub fn bottom(&self) -> i64 {
        self.y + i64::from(self.h)
    }
}

/// The editor's geometry: one pixel rectangle per area, computed by the
/// pure [`layout`] function.
///
/// The canvas and the tool panel live in the **base** layer (big cells),
/// the top bar and the status line in the **overlay** layer (small cells).
/// A dropdown's open list is drawn in the overlay layer too, so it can
/// overlap the canvas (roadmap §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorLayout {
    /// The visible part of the drawing canvas (base layer).
    pub canvas: Rect,
    /// The tool/colour panel on the right of the canvas (base layer).
    pub tools: Rect,
    /// The dropdown row at the top (overlay layer).
    pub top_bar: Rect,
    /// The status line at the bottom (overlay layer).
    pub status: Rect,
    /// Where the canvas' cell `(0, 0)` lands in window pixels.
    ///
    /// The x is negative when the canvas is scrolled sideways: the
    /// viewport shows the columns from [`EditorLayout::canvas_scroll_cols`]
    /// on, so the cell `(0, 0)` sits that many cells to the left of the
    /// window.
    pub canvas_origin: (i64, i64),
    /// The first canvas row that is visible.
    pub canvas_scroll_rows: usize,
    /// The first canvas column that is visible.
    pub canvas_scroll_cols: usize,
    /// The three dropdown boxes, left to right (overlay layer).
    pub dropdowns: [Rect; 3],
    /// The name field in the top bar (overlay layer).
    pub name_field: Rect,
    /// The primary (SAVE) button in the top bar (overlay layer).
    pub primary_button: Rect,
    /// The ASSIGN button in the top bar (overlay layer). It switches the
    /// editor into assign mode.
    pub assign_button: Rect,
    /// The DRAW button in the top bar (overlay layer). It switches the
    /// editor back into draw mode.
    pub draw_button: Rect,
    /// The slot list of the assign panel (base layer).
    pub slot_list: Rect,
    /// The asset list of the assign panel (base layer).
    pub asset_list: Rect,
    /// The CLEAR button of the assign panel (base layer): it removes the
    /// assignment of the selected slot.
    pub clear_button: Rect,
    /// The SETTINGS button in the top bar (overlay layer). It switches
    /// the editor into the settings panel.
    pub settings_button: Rect,
    /// The TRACK button in the top bar (overlay layer). It switches the
    /// editor into the note strip.
    pub track_button: Rect,
    /// The settings panel (base layer): the canvas area in settings mode.
    pub settings: Rect,
    /// The first visible settings row.
    pub settings_scroll: usize,
    /// How many settings rows are visible.
    pub settings_visible: usize,
    /// The canvas' width in cells (150 big / 75 small / 109 balloon /
    /// 200 grass).
    pub canvas_cols: usize,
    /// The canvas' height in cells (150 big / 75 small / 73 balloon /
    /// 4 grass).
    pub canvas_rows: usize,
    /// How many canvas rows the canvas viewport shows.
    pub canvas_view_rows: usize,
    /// How many canvas columns the canvas viewport shows.
    pub canvas_view_cols: usize,
    /// The largest scroll offset: the active region minus the visible
    /// rows. The inactive rows below the frame are never scrolled into
    /// view (roadmap §5.4).
    pub max_scroll: usize,
    /// The largest horizontal scroll offset: the canvas' width minus the
    /// visible columns. A grass tile's canvas is 200 columns wide, so it
    /// scrolls sideways (roadmap Extension 1 §10.1).
    pub max_scroll_cols: usize,
}

/// Compute the editor's layout for a window of `window_w × window_h`.
///
/// The function is **pure**: it reads nothing but its arguments and
/// returns the same result for the same input, so the geometry is
/// testable without a window (roadmap Stage 5).
///
/// The geometry (roadmap §5.2):
///
/// * the top bar is a strip at the top, the status line a strip at the
///   bottom — both in overlay pixels;
/// * the canvas fills the middle-left, the tool panel is a vertical strip
///   on its right;
/// * the canvas is the element's own grid: a **square** for a sprite
///   (150 / 75 cells) and the **disc** for a balloon (109 x 73 cells at
///   the default radius 36). The canvas is wider than any window, so it
///   scrolls vertically: `canvas_scroll_rows` is the first visible canvas
///   row and the scroll is clamped to the active region, so the inactive
///   rows below the frame are never scrolled into view.
///
/// The canvas scrolls **horizontally** too (`canvas_scroll_cols`): a
/// grass tile's canvas is 200 columns wide, so the window can only ever
/// show a part of it. The scroll is clamped to the columns that are
/// actually there, and the cell `(0, 0)` moves left by that many cells
/// (`canvas_origin`).
pub fn layout(window_w: u32, window_h: u32, state: &EditorState) -> EditorLayout {
    let base_rows = i64::from(window_h / CANVAS_CELL_H);
    let ui_rows = i64::from(window_h / UI_CELL_H);
    let top_h = i64::from(TOP_BAR_BASE_ROWS) * i64::from(CANVAS_CELL_H);
    // The status strip sits at the bottom of the window; the canvas ends
    // where it starts, so the status never overlaps the canvas.
    let status_top = (ui_rows - i64::from(STATUS_UI_ROWS)).max(0) * i64::from(UI_CELL_H);
    let canvas_top_row = i64::from(TOP_BAR_BASE_ROWS).min(base_rows);
    let canvas_bottom_row =
        (status_top / i64::from(CANVAS_CELL_H)).clamp(canvas_top_row, base_rows);
    let canvas_y = canvas_top_row * i64::from(CANVAS_CELL_H);
    let canvas_h = ((canvas_bottom_row - canvas_top_row) * i64::from(CANVAS_CELL_H)).max(0) as u32;
    let tools_w = (panel_cells(state.mode) * CANVAS_CELL_W).min(window_w);
    // The canvas' width is rounded down to a whole number of base cells,
    // so the tool panel always starts on a cell boundary and its marker
    // column never bleeds into the canvas.
    let canvas_w = (window_w.saturating_sub(tools_w) / CANVAS_CELL_W) * CANVAS_CELL_W;
    let view_rows = (canvas_h / CANVAS_CELL_H) as usize;
    let view_cols = (canvas_w / CANVAS_CELL_W) as usize;
    // The scroll never leaves the active region (roadmap §5.4).
    let max_scroll = state.active_rows().saturating_sub(view_rows);
    let scroll = state.scroll_rows.min(max_scroll);
    // The horizontal scroll never leaves the canvas' columns.
    let max_scroll_cols = state.canvas_cols().saturating_sub(view_cols);
    let scroll_cols = state.scroll_cols.min(max_scroll_cols);
    let canvas_origin = (
        -(scroll_cols as i64) * i64::from(CANVAS_CELL_W),
        canvas_y - (scroll as i64) * i64::from(CANVAS_CELL_H),
    );
    let canvas = Rect::new(0, canvas_y, canvas_w, canvas_h);
    let slots = slot_list_rect(canvas);
    // The settings panel replaces the canvas area: the first row is the
    // header, the rest are the settings.
    let settings_visible = view_rows.saturating_sub(1);
    let settings_scroll = state
        .settings_scroll
        .min(SETTING_ROWS.len().saturating_sub(settings_visible));
    EditorLayout {
        canvas,
        tools: Rect::new(i64::from(canvas_w), canvas_y, tools_w, canvas_h),
        top_bar: Rect::new(0, 0, window_w, top_h as u32),
        status: Rect::new(0, status_top, window_w, STATUS_UI_ROWS * UI_CELL_H),
        canvas_origin,
        canvas_scroll_rows: scroll,
        canvas_scroll_cols: scroll_cols,
        dropdowns: dropdown_boxes(),
        name_field: name_field_rect(window_w),
        primary_button: primary_button_rect(window_w),
        assign_button: assign_button_rect(window_w),
        draw_button: draw_button_rect(window_w),
        settings_button: settings_button_rect(window_w),
        track_button: track_button_rect(window_w),
        slot_list: slots,
        asset_list: asset_list_rect(canvas, slots),
        clear_button: clear_button_rect(slots, view_rows),
        settings: canvas,
        settings_scroll,
        settings_visible,
        canvas_cols: state.canvas_cols(),
        canvas_rows: state.canvas_size().1,
        canvas_view_rows: view_rows,
        canvas_view_cols: view_cols,
        max_scroll,
        max_scroll_cols,
    }
}

/// The three dropdown boxes of the top bar, in pixels.
fn dropdown_boxes() -> [Rect; 3] {
    let w = BOX_UI_CELLS * UI_CELL_W;
    let gap = BOX_GAP_UI_CELLS * UI_CELL_W;
    let h = BOX_UI_ROWS * UI_CELL_H;
    let mut boxes = [Rect::new(0, 0, w, h); 3];
    for (index, rect) in boxes.iter_mut().enumerate() {
        let x = i64::from(index as u32) * (i64::from(w) + i64::from(gap));
        *rect = Rect::new(x, 0, w, h);
    }
    boxes
}

/// The narrowest window the editor's top bar can lay out.
///
/// The number is derived from the layout itself: the three dropdown
/// boxes, the gap, the name field at its minimum width, the gap and the
/// three buttons. The OS enforces it (`with_min_inner_size`), so the
/// name field can never disappear while the user types into it (audit
/// 2026-10-20 § editor, task 1).
pub const fn min_layout_width() -> u32 {
    TOP_BAR_MIN_CELLS * UI_CELL_W
}

/// The name field of the top bar, in pixels.
///
/// It starts after the three dropdown boxes and ends before the **mode
/// buttons** (roadmap Stage 9), so it never overlaps them. The field is
/// the elastic part of the top bar: on a narrow window the two mode
/// buttons keep their width and the field gives way, because the buttons
/// carry the mode switch and the field only holds a name.
///
/// The width is clamped to [`NAME_FIELD_MIN_UI_CELLS`]: a field the user
/// cannot see is a field they type into blindly. The window's minimum
/// size keeps the clamp from ever reaching the mode buttons — a window
/// narrower than [`min_layout_width`] cannot exist.
fn name_field_rect(window_w: u32) -> Rect {
    let boxes = dropdown_boxes();
    let last = boxes[2];
    let gap = NAME_FIELD_GAP_UI_CELLS * UI_CELL_W;
    let x = last.right() + i64::from(gap);
    let button = settings_button_rect(window_w);
    let right = button.x - i64::from(gap);
    let avail = (right - x).max(0) as u32;
    let room = window_w.saturating_sub(x.max(0) as u32);
    let min = NAME_FIELD_MIN_UI_CELLS * UI_CELL_W;
    Rect::new(x, 0, avail.min(room).max(min), BOX_UI_ROWS * UI_CELL_H)
}

/// The SAVE button of the top bar, in pixels.
///
/// It is the right-most control of the top bar and triggers the same
/// primary action as the Enter key (roadmap §5.6).
fn primary_button_rect(window_w: u32) -> Rect {
    let w = (SAVE_BUTTON_UI_CELLS * UI_CELL_W).min(window_w);
    Rect::new(
        i64::from(window_w) - i64::from(w),
        0,
        w,
        BOX_UI_ROWS * UI_CELL_H,
    )
}

/// The DRAW button of the top bar, in pixels.
///
/// It sits directly left of the SAVE button and switches the editor back
/// into draw mode.
fn draw_button_rect(window_w: u32) -> Rect {
    mode_buttons(window_w).draw
}

/// The ASSIGN button of the top bar, in pixels.
///
/// It sits directly left of the DRAW button and switches the editor into
/// assign mode.
fn assign_button_rect(window_w: u32) -> Rect {
    mode_buttons(window_w).assign
}

/// The SETTINGS button of the top bar, in pixels.
///
/// It sits left of the ASSIGN button and switches the editor into the
/// settings panel (roadmap Extension 2 §3.1).
fn settings_button_rect(window_w: u32) -> Rect {
    mode_buttons(window_w).settings
}

/// The TRACK button of the top bar, in pixels.
///
/// It sits between the SETTINGS and the ASSIGN button and switches the
/// editor into the note strip (roadmap Extension 3 §3.2).
fn track_button_rect(window_w: u32) -> Rect {
    mode_buttons(window_w).sequencer
}

/// The mode buttons of the top bar, in pixels.
///
/// They sit left of the SAVE button and keep their **full** width: a
/// squeezed button drops its label, and two labels that overlap are
/// worse than a name field that is a little short (roadmap Stage 9). The
/// name field is the elastic part of the top bar; the window's minimum
/// size keeps it from reaching the buttons.
fn mode_buttons(window_w: u32) -> ModeButtons {
    let h = BOX_UI_ROWS * UI_CELL_H;
    let primary = primary_button_rect(window_w);
    let draw_w = (DRAW_BUTTON_UI_CELLS * UI_CELL_W).min(window_w);
    let assign_w = (ASSIGN_BUTTON_UI_CELLS * UI_CELL_W).min(window_w);
    let track_w = (TRACK_BUTTON_UI_CELLS * UI_CELL_W).min(window_w);
    let settings_w = (SETTINGS_BUTTON_UI_CELLS * UI_CELL_W).min(window_w);
    let draw_x = primary.x - i64::from(draw_w);
    let assign_x = draw_x - i64::from(assign_w);
    let track_x = assign_x - i64::from(track_w);
    let settings_x = track_x - i64::from(settings_w);
    ModeButtons {
        assign: Rect::new(assign_x, 0, assign_w, h),
        draw: Rect::new(draw_x, 0, draw_w, h),
        settings: Rect::new(settings_x, 0, settings_w, h),
        sequencer: Rect::new(track_x, 0, track_w, h),
    }
}

/// The mode buttons of the top bar, in pixels.
///
/// A struct and not a tuple: four rects in a tuple are easy to read in
/// the wrong order, and a mis-ordered button is a button that switches
/// to the wrong mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModeButtons {
    /// The ASSIGN button: the slot and asset picker.
    pub assign: Rect,
    /// The DRAW button: the drawing canvas.
    pub draw: Rect,
    /// The SETTINGS button: the settings panel.
    pub settings: Rect,
    /// The TRACK button: the note strip.
    pub sequencer: Rect,
}

/// The slot list of the assign panel: the left strip of the canvas area.
fn slot_list_rect(canvas: Rect) -> Rect {
    let w = (SLOT_LIST_CELLS * CANVAS_CELL_W).min(canvas.w);
    Rect::new(canvas.x, canvas.y, w, canvas.h)
}

/// The asset list of the assign panel: the rest of the canvas area.
fn asset_list_rect(canvas: Rect, slots: Rect) -> Rect {
    let x = slots.right();
    Rect::new(x, canvas.y, (canvas.right() - x).max(0) as u32, canvas.h)
}

/// The CLEAR button of the assign panel, in pixels.
///
/// It sits on the row **below** the target list's visible rows (the
/// sixteen parade slots and the three look targets, decision F-162), so
/// the slot list keeps exactly that many rows. The row is clamped to the
/// last visible row of the panel, so the button is always on screen.
fn clear_button_rect(slots: Rect, view_rows: usize) -> Rect {
    let row = ASSIGN_TARGET_COUNT.min(view_rows.saturating_sub(1));
    Rect::new(
        slots.x,
        slots.y + row as i64 * i64::from(CANVAS_CELL_H),
        slots.w,
        CANVAS_CELL_H,
    )
}

/// The row of the settings panel that `visible` names, in pixels.
///
/// `visible` is the index **inside the panel** — the first visible row
/// is zero, whatever the scroll offset is.
fn settings_row_rect(area: Rect, visible: usize) -> Rect {
    Rect::new(
        area.x,
        area.y + (visible as i64 + 1) * i64::from(CANVAS_CELL_H),
        area.w,
        CANVAS_CELL_H,
    )
}

/// The label column of one settings row, in pixels.
pub fn settings_label_rect(area: Rect, visible: usize) -> Rect {
    let row = settings_row_rect(area, visible);
    Rect::new(
        row.x + i64::from(CANVAS_CELL_W),
        row.y,
        (SETTINGS_LABEL_CELLS * i64::from(CANVAS_CELL_W)) as u32,
        row.h,
    )
}

/// The value column of one settings row, in pixels.
///
/// The column is where the widget lives: the slider track, the toggle,
/// the text field and the dropdown all sit in it.
pub fn settings_value_rect(area: Rect, visible: usize) -> Rect {
    let row = settings_row_rect(area, visible);
    let x = row.x + (SETTINGS_LABEL_CELLS + SETTINGS_VALUE_GAP_CELLS) * i64::from(CANVAS_CELL_W);
    Rect::new(
        x,
        row.y,
        (SETTINGS_VALUE_CELLS * i64::from(CANVAS_CELL_W)) as u32,
        row.h,
    )
}

/// The unit column of one settings row, in pixels.
fn settings_unit_rect(area: Rect, visible: usize) -> Rect {
    let row = settings_row_rect(area, visible);
    let x = row.x
        + (SETTINGS_LABEL_CELLS
            + SETTINGS_VALUE_GAP_CELLS
            + SETTINGS_VALUE_CELLS
            + SETTINGS_UNIT_GAP_CELLS)
            * i64::from(CANVAS_CELL_W);
    Rect::new(
        x,
        row.y,
        (SETTINGS_UNIT_CELLS * i64::from(CANVAS_CELL_W)) as u32,
        row.h,
    )
}

/// The slider track of one settings row, in pixels.
///
/// The track is the left part of the value column; the value itself is
/// shown to its right.
pub fn settings_track_rect(area: Rect, visible: usize) -> Rect {
    let value = settings_value_rect(area, visible);
    Rect::new(
        value.x,
        value.y,
        (SETTINGS_TRACK_CELLS * i64::from(CANVAS_CELL_W)) as u32,
        value.h,
    )
}

/// What the editor is doing: painting a sprite/balloon, pointing a
/// parade slot at a drawn asset (roadmap Stage 9), editing the
/// settings (roadmap Extension 2 §3.1), or writing a note track
/// (roadmap Extension 3 §3.2).
///
/// The mode is part of the **pure** state, so the layout, the hit test and
/// the painting can all ask for it without touching the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EditorMode {
    /// The drawing canvas (the default).
    #[default]
    Draw,
    /// The assignment picker: the slot list and the asset list.
    Assign,
    /// The settings panel: a list of rows, each with a value widget.
    Settings,
    /// The note strip: the sequencer's lane, its ruler and the pitch
    /// palette.
    Sequencer,
}

/// The save of a drawn asset: the platform's own function.
///
/// A named type, because the signature is long enough to be worth a
/// name of its own — and because the core half of the editor hands it
/// over as a plain function pointer.
pub type SaveAsset = fn(
    &crate::drawn::DrawnSprite,
    crate::drawn::DrawnKind,
    &str,
) -> Result<PathBuf, crate::drawn::DrawnError>;

/// The platform services the editor state needs.
///
/// The **core** cannot list the platform's asset folder, read the
/// user's `config.toml` or write a file: those are platform questions,
/// and the core that becomes a library must not know where the platform
/// keeps its files.
///
/// So every platform call is a **parameter**: the application half hands
/// its own functions over when it builds the state
/// ([`EditorState::set_platform`]). A `None` field means "the core does
/// it on its own" — the kind's own prefix for the name, no assets, no
/// config — which is what the tests and the scene's own use of the state
/// rely on.
///
/// The fields are **function pointers**, not closures: they are `Copy`,
/// so the state stays `Clone`, and they carry no captured state, so the
/// platform's own error types can be used verbatim.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditorPlatform {
    /// The first free asset name of a kind (`sprite-1`, `balloon-1`, …).
    pub next_free_name: Option<fn(crate::drawn::DrawnKind) -> String>,
    /// The assets of a kind, sorted, **with their size class**.
    ///
    /// WHY the size: the assign panel must offer a big slot only the
    /// big sprites — a small sprite in a big slot is drawn in the
    /// top-left corner of the big duck's box, which is a broken picture
    /// (audit 2026-10-21 §E1). The size is in each document, so the
    /// platform reads it when it lists (see
    /// [`crate::drawn::list_with_sizes_in`]).
    pub list_assets: Option<fn(crate::drawn::DrawnKind) -> Vec<crate::drawn::AssetEntry>>,
    /// The save of a drawn asset.
    pub save_asset: Option<SaveAsset>,
    /// The user's config, **as the user wrote it** (no repair pass).
    pub load_config: Option<fn() -> Result<crate::config::Config, crate::config::ConfigError>>,
}

/// The editor's state: what is being drawn, which dropdown is open and
/// what the status line says.
///
/// The state is **pure data** — no window, no GPU, no file handle — so
/// every dropdown and name-field rule is unit-testable without a display
/// (roadmap Stage 5).
#[derive(Debug, Clone)]
pub struct EditorState {
    /// Sprite or balloon.
    pub kind: crate::drawn::DrawnKind,
    /// The duck size of a sprite; meaningless for a balloon.
    pub size: crate::drawn::DrawnSize,
    /// How many frames will be saved (1..=4).
    pub frames_wanted: usize,
    /// The frame that is being drawn, 0-based.
    pub frame_index: usize,
    /// The frame that is being drawn: the element's canvas — square for a
    /// sprite, the balloon's disc for a balloon.
    pub canvas: crate::balloon::AsciiSprite,
    /// The previous frame, drawn faint behind the current one.
    pub ghost: Option<crate::balloon::AsciiSprite>,
    /// The frames that are already finished.
    pub frames: Vec<crate::balloon::AsciiSprite>,
    /// The selected tool — an index into [`crate::drawn::TOOLS`].
    pub tool: usize,
    /// The selected colour — an index into [`crate::drawn::PALETTE`].
    pub color: usize,
    /// The asset name. **Empty** means the field shows the placeholder
    /// ([`EditorState::default_name`]) and nothing has been typed yet.
    pub name: String,
    /// The name used when the name field is empty: the first free asset
    /// name of the current kind.
    ///
    /// It is the **placeholder** of the field: it is shown while the
    /// name is empty, the first character the user types replaces it
    /// and clearing the field brings it back (audit 2026-10-20 § editor,
    /// task 3). It is also the name the document is stored under when
    /// the user typed nothing.
    pub default_name: String,
    /// The open dropdown (0, 1 or 2); `None` when every list is closed.
    pub open_dropdown: Option<usize>,
    /// The first visible canvas row.
    pub scroll_rows: usize,
    /// The first visible canvas column.
    ///
    /// A grass tile's canvas is 200 columns wide, so the canvas scrolls
    /// sideways as well as down (roadmap Extension 1 §10.1). The note
    /// strip borrows the same scroll: it is the editor's canvas again
    /// (roadmap Extension 3 §3.2).
    pub scroll_cols: usize,
    /// The horizontal scroll the **drawing** had before the strip took
    /// the canvas over.
    ///
    /// The strip and the drawing share one scroll, so the drawing's
    /// position is parked here while the strip is on screen and put back
    /// when the mode is left.
    pub draw_scroll_cols: usize,
    /// The first visible row of the tool/colour panel. The panel is 28
    /// rows tall, so a short window can only show part of it; the panel
    /// scrolls instead of overflowing the window.
    pub panel_scroll: usize,
    /// A transient message and the moment it was raised.
    pub toast: Option<(String, Instant)>,
    /// The status line: the long form of every dropdown value.
    pub status: String,
    /// The balloon's authoring geometry: the disc radii and the frame
    /// that holds the disc (`2rx+1` by `2ry+1` cells). The sprite's
    /// canvas follows [`EditorState::size`].
    pub balloon_geo: crate::balloon::BalloonGeometry,
    /// The balloon's active-cell mask; empty for a sprite.
    pub mask: Vec<bool>,
    /// What the editor is doing: drawing or assigning (roadmap Stage 9).
    pub mode: EditorMode,
    /// True when the SAVE press has armed a save that the next tick
    /// still owes (decision F-163). The shell completes an armed save
    /// through [`EditorState::primary_action`] and clears the flag —
    /// and the exit flush drains it too, so the one-frame window can
    /// never eat a save.
    pub armed_save: bool,
    /// What the assign panel's list assigns, in display order: the
    /// sixteen parade slots plus the three look targets (decision
    /// F-162). See [`assign_targets`].
    pub targets: Vec<AssignTarget>,
    /// The target the user has selected in assign mode — an index into
    /// [`EditorState::targets`].
    pub assign_target: usize,
    /// The asset the user picked for the selected target — an index into
    /// the selected target's asset list.
    pub selected_asset: Option<usize>,
    /// The drawn sprites, read when assign mode opens.
    pub sprites: Vec<crate::drawn::AssetEntry>,
    /// The drawn balloons, read when assign mode opens.
    pub balloons: Vec<crate::drawn::AssetEntry>,
    /// The drawn grass tiles, read when assign mode opens (F-162).
    pub grass: Vec<crate::drawn::AssetEntry>,
    /// The drawn decorations, read when assign mode opens (F-162).
    pub decorations: Vec<crate::drawn::AssetEntry>,
    /// The assets the **selected slot** can take, filtered by its size
    /// class.
    ///
    /// WHY it is stored and not computed: the panel asks for the list
    /// every frame, and a per-frame allocation is a waste. It is rebuilt
    /// when the selected slot changes and when the lists are read.
    pub assign_assets: Vec<String>,
    /// The current `[assignments]` table, read when assign mode opens.
    pub assignments: crate::config::AssignmentsConfig,
    /// The clock the toast and the draft debounce are timed with.
    ///
    /// WHY it is a parameter: `Instant::now()` **panics** on
    /// `wasm32-unknown-unknown`, so the core must not call it on its
    /// own (audit 2026-10-21 §E3). The host hands the state one.
    clock: fn() -> Instant,
    /// The first visible row of the slot list.
    pub slot_scroll: usize,
    /// The first visible row of the asset list.
    pub asset_scroll: usize,
    /// The undo/redo stack of drawing strokes (roadmap Extension 2 §2.1).
    ///
    /// It is **pure data**, so the whole undo feature is testable
    /// without a window: paint, undo, redo, assert the canvas.
    pub undo: UndoStack,
    /// The stroke that is being painted, while the pointer is down.
    ///
    /// `None` between strokes. A click is a stroke of its own: the
    /// pointer goes down and comes up without moving.
    pub stroke: Option<Stroke>,
    /// True when the drawing has changed since the last draft write.
    pub draft_dirty: bool,
    /// When the draft is due, if it is dirty.
    ///
    /// The write is **debounced**: one second of quiet, not one write
    /// per cell (roadmap Extension 2 §2.2).
    pub draft_due: Option<Instant>,
    /// The name of the draft the editor found and offers, if any.
    ///
    /// The offer is a **prompt**, not a silent load: a surprise is worse
    /// than a question (roadmap Extension 2 §2.2).
    pub draft_offer: Option<String>,
    /// The kind and the name of the draft this editor wrote or loaded.
    ///
    /// It is what a clean exit cleans up: an editor that leaves nothing
    /// behind must not take the user's work with it. It also tells the
    /// editor which draft to remove when the asset is renamed.
    pub draft_key: Option<(crate::drawn::DrawnKind, String)>,
    /// The disk actions the app still has to carry out.
    pub draft_actions: Vec<DraftAction>,
    /// The config the settings panel shows and edits.
    ///
    /// It is a **copy** of the user's file, read when the panel opens.
    /// The panel has no private source of truth: every change is
    /// written into the user's file first, and the copy follows
    /// (roadmap Extension 2 §3.3).
    pub settings_config: crate::config::Config,
    /// The first visible row of the settings panel.
    pub settings_scroll: usize,
    /// The row the keyboard cursor is on.
    pub settings_cursor: usize,
    /// The text field the user is typing into — the index of the row.
    pub settings_focus: Option<usize>,
    /// The text the user is typing.
    pub settings_text: String,
    /// True once the user has typed into the focused text field.
    ///
    /// Until then the field shows the current value as a placeholder.
    pub settings_typed: bool,
    /// The choice row whose option list is open.
    pub settings_open: Option<usize>,
    /// The keyboard's staged edit: the row and the value the app still
    /// has to write.
    ///
    /// [`EditorState::settings_step`] and
    /// [`EditorState::settings_activate`] stage a value here instead of
    /// applying it: the state never writes the file, and the panel must
    /// never show a value that failed to store (roadmap Extension 2
    /// §3.2). The app drains the pair with
    /// [`EditorState::take_staged_setting`] and writes it through its
    /// own writer.
    pub settings_staged: Option<(usize, SettingValue)>,
    /// The notes on the strip: a step index -> note map.
    ///
    /// It is the **sparse** map of the sequencer's data model (roadmap
    /// Extension 3 §3.1): a 263-step strip with a dozen notes on it is a
    /// dozen entries.
    pub track: crate::audio::Track,
    /// The name of the track file the strip is written to.
    ///
    /// It is the name field of the strip mode — the drawing keeps its own
    /// name, so switching the mode never renames anything.
    pub track_name: String,
    /// The length of the strip in steps, from `[audio] strip_steps`.
    pub strip_steps: usize,
    /// The selected pitch of the palette — an index into [`PITCH_NAMES`].
    pub pitch: usize,
    /// The strip's own undo history (the canvas's stack stores cell
    /// deltas, and a note is not a cell).
    pub strip_undo: StripUndo,
    /// The names of the track files that exist, read when the mode opens.
    pub tracks: Vec<String>,
    /// Where the strip stands relative to its track file
    /// ([`TrackSync`]). The strip keeps its notes when the mode is left
    /// and entered again: switching the mode never throws the user's
    /// work away.
    pub track_sync: TrackSync,
    /// The track job the app still has to carry out.
    ///
    /// The state never touches the file system: it asks, and the worker
    /// does it (roadmap Extension 3 §3.2).
    pub track_request: Option<TrackRequest>,
    /// The platform services the state uses.
    ///
    /// They are **injected** (see [`EditorPlatform`]): the core cannot
    /// list the platform's asset folder or read the user's config, so
    /// the application half hands its own functions over.
    pub platform: EditorPlatform,
    /// The folder the MIDI tracks live in.
    ///
    /// The core cannot resolve a platform location, so the folder is
    /// **injected** as well ([`EditorState::set_tracks_dir`]). `None`
    /// means "no track folder": the strip runs and simply lists no
    /// track.
    pub tracks_dir: Option<PathBuf>,
}

impl EditorState {
    /// A fresh editor state for the configured balloon radius.
    ///
    /// The balloon's canvas is the **authoring disc** and follows the
    /// configured `balloon_radius_rows` (roadmap §3.3); the sprite's side
    /// follows the duck size (150 cells big, 75 small). The name starts as
    /// the first free asset name, so the user can save without typing
    /// (roadmap §3.7).
    ///
    /// The **core** cannot know the platform's first free name, so the
    /// state starts with the kind's own prefix and the application half
    /// hands the real one over ([`EditorState::set_platform`]).
    ///
    /// The **clock** is the host's, for the same reason the core's
    /// trace clock is: the standard library's `Instant::now` **panics**
    /// on `wasm32-unknown-unknown` ("time not implemented on this
    /// platform"), so the core must not call it on its own (audit
    /// 2026-10-21 §E3). The clock times the toast and the draft
    /// debounce; it is a plain function pointer, so the state stays
    /// `Copy`-able and `Clone`.
    ///
    pub fn new(cfg: &crate::config::Config, clock: fn() -> Instant) -> Self {
        let kind = crate::drawn::DrawnKind::Sprite;
        let size = crate::drawn::DrawnSize::Big;
        let balloon_geo =
            crate::balloon::balloon_authoring_geometry(cfg.balloons.balloon_radius_rows);
        let side = crate::drawn::sprite_canvas_side(size);
        let mut state = Self {
            kind,
            size,
            frames_wanted: 1,
            frame_index: 0,
            canvas: crate::balloon::AsciiSprite::new(side, side),
            ghost: None,
            frames: Vec::new(),
            tool: crate::drawn::DEFAULT_TOOL,
            color: crate::drawn::DEFAULT_COLOR,
            name: String::new(),
            default_name: String::new(),
            open_dropdown: None,
            clock,
            scroll_rows: 0,
            scroll_cols: 0,
            draw_scroll_cols: 0,
            panel_scroll: 0,
            toast: None,
            status: String::new(),
            balloon_geo,
            mask: Vec::new(),
            mode: EditorMode::Draw,
            armed_save: false,
            targets: assign_targets(),
            assign_target: 0,
            selected_asset: None,
            sprites: Vec::new(),
            balloons: Vec::new(),
            grass: Vec::new(),
            decorations: Vec::new(),
            assign_assets: Vec::new(),
            assignments: crate::config::AssignmentsConfig::default(),
            slot_scroll: 0,
            asset_scroll: 0,
            undo: UndoStack::new(),
            stroke: None,
            draft_dirty: false,
            draft_due: None,
            draft_offer: None,
            draft_key: None,
            draft_actions: Vec::new(),
            settings_config: cfg.clone(),
            settings_scroll: 0,
            settings_cursor: 0,
            settings_focus: None,
            settings_text: String::new(),
            settings_typed: false,
            settings_open: None,
            settings_staged: None,
            track: crate::audio::Track::new(),
            track_name: cfg.audio.track.clone().unwrap_or_default(),
            strip_steps: cfg.audio.strip_steps.clamp(
                crate::config::bounds::STRIP_STEPS_MIN,
                crate::config::bounds::STRIP_STEPS_MAX,
            ),
            pitch: 0,
            strip_undo: StripUndo::new(),
            tracks: Vec::new(),
            track_sync: TrackSync::Fresh,
            track_request: None,
            platform: EditorPlatform::default(),
            tracks_dir: None,
        };
        let default_name = state.next_free_name(kind);
        state.default_name = default_name;
        state.refresh_status();
        state
    }

    /// Hand the state the platform's services.
    ///
    /// The core cannot list the platform's asset folder nor read the
    /// user's `config.toml`, so the application half injects its own
    /// functions here (see [`EditorPlatform`]).
    ///
    /// The default name is refreshed, because the placeholder of the name
    /// field is the platform's first free name of the current kind — and
    /// the platform only knows it once it has been handed over.
    pub fn set_platform(&mut self, platform: EditorPlatform) {
        self.platform = platform;
        let kind = self.kind;
        let default_name = self.next_free_name(kind);
        self.default_name = default_name;
    }

    /// Hand the state the folder the MIDI tracks live in.
    ///
    /// The core cannot resolve a platform location, so the folder is
    /// **injected**. `None` means "no track folder": the strip runs and
    /// lists no track.
    pub fn set_tracks_dir(&mut self, dir: Option<PathBuf>) {
        self.tracks_dir = dir;
    }

    /// The first free asset name of `kind`.
    ///
    /// The generator is the platform's (see [`EditorPlatform`]); without
    /// one the kind's own prefix is used: `sprite-1`, `balloon-1`, …
    pub fn next_free_name(&self, kind: crate::drawn::DrawnKind) -> String {
        match self.platform.next_free_name {
            Some(next) => next(kind),
            None => default_asset_name(kind),
        }
    }

    /// The names of the assets of `kind`.
    ///
    /// Without the platform's lister there are no assets: the core never
    /// touches a folder on its own.
    pub fn list_assets(&self, kind: crate::drawn::DrawnKind) -> Vec<crate::drawn::AssetEntry> {
        match self.platform.list_assets {
            Some(list) => list(kind),
            None => Vec::new(),
        }
    }

    /// The user's config, as the user wrote it.
    ///
    /// The loader is the platform's (see [`EditorPlatform`]). A config
    /// that cannot be read yields the defaults, so the panel always shows
    /// something.
    fn user_config(&self, tag: &str) -> crate::config::Config {
        let Some(load) = self.platform.load_config else {
            return crate::config::Config::default();
        };
        match load() {
            Ok(cfg) => cfg,
            Err(e) => {
                crate::trace::warn(&format!("{tag} error: {e}"));
                crate::config::Config::default()
            }
        }
    }

    /// Read the user's config for the settings panel.
    ///
    /// The file is read **once**, when the panel opens: it is a few kB and
    /// the panel must not read it per frame (AGENTS.md: never block the
    /// UI thread). A file that cannot be read yields the defaults, so the
    /// panel always shows something.
    fn load_settings_config(&self) -> crate::config::Config {
        self.user_config("editor.settings")
    }

    /// The canvas size in cells for the current element: `(cols, rows)`.
    ///
    /// A sprite's canvas is square (150 x 150 big, 75 x 75 small); a
    /// balloon's is the **disc** — 109 x 73 cells at the default radius
    /// 36. The canvas is not square for a balloon because the grid cell
    /// is taller than wide: a 3:2 cell grid is a circle in pixels, and
    /// the art maps 1:1 onto the runtime disc (audit 2026-10-20 § 3.18).
    ///
    /// In **strip mode** the canvas is the note strip: `strip_steps`
    /// columns and three rows — the caption, the note lane and the
    /// ruler (roadmap Extension 3 §3.2, audit 2026-10-23 §U-16). The
    /// strip is the editor's canvas again, so the layout, the hit test
    /// and the horizontal scroll work unchanged.
    pub fn canvas_size(&self) -> (usize, usize) {
        if self.mode == EditorMode::Sequencer {
            return (self.strip_steps, STRIP_ROWS);
        }
        self.drawing_canvas_size()
    }

    /// The canvas size of the **drawing**, whatever the mode is.
    ///
    /// The strip mode borrows the canvas for the note strip, but the
    /// drawing behind it keeps its own grid: switching the mode must
    /// never resize the canvas the user is drawing on.
    pub fn drawing_canvas_size(&self) -> (usize, usize) {
        match self.kind {
            crate::drawn::DrawnKind::Sprite | crate::drawn::DrawnKind::Decoration => {
                let side = crate::drawn::sprite_canvas_side(self.size);
                (side, side)
            }
            crate::drawn::DrawnKind::Balloon => (self.balloon_geo.cols, self.balloon_geo.rows),
            crate::drawn::DrawnKind::Grass => (
                crate::drawn::GRASS_TILE_MAX_COLS,
                crate::drawn::GRASS_TILE_MAX_ROWS,
            ),
        }
    }

    /// The canvas width in cells for the current element.
    pub fn canvas_cols(&self) -> usize {
        self.canvas_size().0
    }

    /// How many canvas rows can be painted — the height of the active
    /// region (the frame's height for a sprite, the whole disc for a
    /// balloon).
    pub fn active_rows(&self) -> usize {
        if self.mode == EditorMode::Sequencer {
            return STRIP_ROWS;
        }
        match self.kind {
            // A decoration is authored on the duck's frame, so its active
            // region is the duck's frame height.
            crate::drawn::DrawnKind::Sprite | crate::drawn::DrawnKind::Decoration => {
                crate::drawn::sprite_active_rows(self.size)
            }
            crate::drawn::DrawnKind::Balloon => self.balloon_geo.rows,
            crate::drawn::DrawnKind::Grass => crate::drawn::GRASS_TILE_MAX_ROWS,
        }
    }

    /// True when the canvas cell `(col, row)` may be painted.
    ///
    /// A sprite's active region is the frame rectangle at the canvas
    /// origin; a balloon's is the disc (decision D14). The note strip
    /// has three rows — caption, lane, ruler — and only the lane takes
    /// a click ([`strip_hit`]).
    pub fn cell_is_active(&self, col: usize, row: usize) -> bool {
        if self.mode == EditorMode::Sequencer {
            return row < STRIP_ROWS;
        }
        match self.kind {
            crate::drawn::DrawnKind::Sprite | crate::drawn::DrawnKind::Decoration => {
                row < self.active_rows()
            }
            crate::drawn::DrawnKind::Balloon => {
                let cols = self.canvas_cols();
                self.mask.get(row * cols + col).copied().unwrap_or(false)
            }
            crate::drawn::DrawnKind::Grass => true,
        }
    }

    /// True when the dropdown at `index` reacts to input.
    ///
    /// Dropdowns 2 and 3 are inactive while a balloon is selected
    /// (roadmap §5.3): a balloon has one frame and no duck size. In
    /// **strip mode** every dropdown is inert: the kind, the size and the
    /// frame count belong to the drawing, and the strip is not a drawing.
    pub fn dropdown_enabled(&self, index: usize) -> bool {
        if self.mode == EditorMode::Sequencer {
            return false;
        }
        match index {
            0 => true,
            // A decoration is drawn over a duck: it has the duck's size
            // and the duck's frame count.
            1 | 2 => matches!(
                self.kind,
                crate::drawn::DrawnKind::Sprite | crate::drawn::DrawnKind::Decoration
            ),
            _ => false,
        }
    }

    /// The short label of the dropdown's current value — what the box
    /// shows (decision D15).
    pub fn dropdown_label(&self, index: usize) -> &'static str {
        match index {
            0 => match self.kind {
                crate::drawn::DrawnKind::Balloon => "BAL",
                crate::drawn::DrawnKind::Sprite => "SPR",
                crate::drawn::DrawnKind::Grass => "GRS",
                crate::drawn::DrawnKind::Decoration => "DEC",
            },
            1 => match self.size {
                crate::drawn::DrawnSize::Big => "BIG",
                crate::drawn::DrawnSize::Small => "SML",
            },
            2 => FRAME_LABELS[self.frames_wanted.clamp(1, 4) - 1],
            _ => "",
        }
    }

    /// The long form of the dropdown's current value — what the status
    /// line spells out (decision D15).
    pub fn dropdown_long(&self, index: usize) -> &'static str {
        match index {
            0 => match self.kind {
                crate::drawn::DrawnKind::Balloon => "BALLOON",
                crate::drawn::DrawnKind::Sprite => "SPRITE",
                crate::drawn::DrawnKind::Grass => "GRASS",
                crate::drawn::DrawnKind::Decoration => "DECORATION",
            },
            1 => match self.size {
                crate::drawn::DrawnSize::Big => "BIG",
                crate::drawn::DrawnSize::Small => "SMALL",
            },
            2 => FRAME_LONGS[self.frames_wanted.clamp(1, 4) - 1],
            _ => "",
        }
    }

    /// The options of the dropdown at `index`, in list order.
    pub fn dropdown_options(&self, index: usize) -> &'static [&'static str] {
        match index {
            0 => &["BAL", "SPR", "GRS", "DEC"],
            1 => &["BIG", "SML"],
            2 => &["1", "2", "3", "4"],
            _ => &[],
        }
    }

    /// Which option of the dropdown at `index` is the current value.
    pub fn dropdown_option_index(&self, index: usize) -> usize {
        match index {
            0 => match self.kind {
                crate::drawn::DrawnKind::Balloon => 0,
                crate::drawn::DrawnKind::Sprite => 1,
                crate::drawn::DrawnKind::Grass => 2,
                crate::drawn::DrawnKind::Decoration => 3,
            },
            1 => match self.size {
                crate::drawn::DrawnSize::Big => 0,
                crate::drawn::DrawnSize::Small => 1,
            },
            2 => self.frames_wanted.clamp(1, 4) - 1,
            _ => 0,
        }
    }

    /// Switch the element kind.
    ///
    /// The canvas is reset — the new element has a different grid — and
    /// the size and frame dropdowns follow the kind: a balloon has no
    /// duck size and a tile has neither — a tile is **one** frame that
    /// is tiled across the ground band. The status line says that the
    /// canvas was cleared.
    pub fn set_kind(&mut self, kind: crate::drawn::DrawnKind) {
        self.kind = kind;
        if kind != crate::drawn::DrawnKind::Sprite {
            self.frames_wanted = 1;
        }
        // The asset name follows the kind: a balloon is not a sprite, and
        // a grass tile is neither (`grass-1`, `grass-2`, …).
        let default_name = self.next_free_name(kind);
        self.default_name = default_name;
        self.name.clear();
        self.reset_canvas();
        self.set_toast("CANVAS CLEARED - NEW GRID");
        self.refresh_status();
    }

    /// Switch the sprite size.
    ///
    /// The canvas is reset — the new element has a different grid — and
    /// the status line says so.
    pub fn set_size(&mut self, size: crate::drawn::DrawnSize) {
        self.size = size;
        self.reset_canvas();
        self.set_toast("CANVAS CLEARED - NEW GRID");
        self.refresh_status();
    }

    /// Set how many frames will be saved.
    ///
    /// The count is clamped to 1..=4. A canvas that already has content
    /// is **not** cleared (roadmap §5.3): only the number of frames
    /// changes. Frames beyond the new count are dropped and the status
    /// line says so.
    pub fn set_frames(&mut self, n: usize) {
        let wanted = n.clamp(1, 4);
        let changed = wanted != self.frames_wanted || self.frames.len() > wanted;
        self.frames_wanted = wanted;
        let surplus = self.frames.len().saturating_sub(wanted);
        if surplus > 0 {
            self.frames.truncate(wanted);
            let plural = if surplus == 1 { "" } else { "S" };
            self.set_toast(&format!("DROPPED {surplus} FRAME{plural}"));
        }
        // The count and the finished frames are draft-relevant content
        // (`DraftDoc` carries both), so the autosave must hear about the
        // change now and not with the next stroke — a stale draft used to
        // resurrect dropped frames on reload (audit 101126 §B4).
        if changed {
            self.mark_dirty();
        }
        self.refresh_status();
    }

    /// Open the dropdown at `index` — its option list drops down.
    ///
    /// An inactive dropdown does not open.
    pub fn open_dropdown(&mut self, index: usize) {
        if !self.dropdown_enabled(index) {
            return;
        }
        self.open_dropdown = Some(index);
    }

    /// Close the open dropdown (if any).
    pub fn close_dropdown(&mut self) {
        self.open_dropdown = None;
    }

    /// Pick the option `option` of the dropdown at `index`; the list
    /// closes.
    ///
    /// Picking the value that is already selected changes nothing — in
    /// particular it does not clear the canvas.
    pub fn pick_option(&mut self, index: usize, option: usize) {
        if !self.dropdown_enabled(index) {
            return;
        }
        if option >= self.dropdown_options(index).len() {
            return;
        }
        self.close_dropdown();
        match index {
            0 => self.pick_kind(option),
            1 => self.pick_size(option),
            2 => self.set_frames(option + 1),
            _ => {}
        }
    }

    /// Apply the dropdown-1 option `option`.
    ///
    /// Picking the value that is already selected changes nothing — in
    /// particular it does not clear the canvas.
    fn pick_kind(&mut self, option: usize) {
        let kind = kind_of_option(option);
        if kind == self.kind {
            return;
        }
        self.set_kind(kind);
    }

    /// Apply the dropdown-2 option `option`.
    fn pick_size(&mut self, option: usize) {
        let size = size_of_option(option);
        if size == self.size {
            return;
        }
        self.set_size(size);
    }

    /// Append a character to the name field.
    ///
    /// Only `a-z`, `0-9`, `-` and `_` are accepted, and the name is
    /// capped at [`crate::drawn::MAX_NAME_LEN`] characters (roadmap
    /// §3.7). Returns `false` when the character is rejected, so the
    /// caller can tell the user why nothing happened.
    ///
    /// The first character **replaces the placeholder** (audit
    /// 2026-10-20 § editor, task 3): the pre-filled name is a hint, not
    /// content, so typing `cat` gives `cat` and not `sprite-1cat`. The
    /// field is empty while the placeholder is shown, so appending to it
    /// *is* replacing it.
    pub fn push_name_char(&mut self, c: char) -> bool {
        if !is_legal_name_char(c) {
            return false;
        }
        if self.name.chars().count() >= crate::drawn::MAX_NAME_LEN {
            return false;
        }
        self.name.push(c);
        true
    }

    /// Delete the last character of the name field.
    ///
    /// Clearing the field brings the **placeholder** back: the name is
    /// never empty in the field, so a save always has a name to write
    /// and the field never shows nothing at all.
    pub fn pop_name_char(&mut self) {
        self.name.pop();
    }

    /// Select the tool at `index` (an index into [`crate::drawn::TOOLS`]).
    ///
    /// An out-of-range index is ignored, so a click can never select a
    /// tool that does not exist.
    pub fn set_tool(&mut self, index: usize) {
        if index >= crate::drawn::TOOLS.len() {
            return;
        }
        self.tool = index;
        self.refresh_status();
    }

    /// Select the colour at `index` (an index into
    /// [`crate::drawn::PALETTE`]).
    ///
    /// An out-of-range index is ignored.
    pub fn set_color(&mut self, index: usize) {
        if index >= crate::drawn::PALETTE.len() {
            return;
        }
        self.color = index;
        self.refresh_status();
    }

    /// Paint the canvas cell `(col, row)` with the active tool glyph in
    /// the active colour (roadmap §5.4).
    ///
    /// The paint is a no-op when the cell is outside the canvas or
    /// outside the **active region**: a sprite's rows below the frame and
    /// a balloon's four corners can never be painted (decision D18 — the
    /// inactive area looks exactly like the empty part of the active one).
    ///
    /// The status line is rebuilt afterwards, because a grass tile's size
    /// in the line is the size of the **inked** part of the canvas.
    ///
    /// The paint is recorded as an **undoable stroke**: a click with no
    /// drag is a stroke of its own (roadmap Extension 2 §2.1).
    pub fn paint(&mut self, col: usize, row: usize) {
        let own_stroke = self.stroke.is_none();
        if own_stroke {
            self.stroke = Some(Stroke::default());
        }
        self.paint_cell(col, row);
        if own_stroke {
            self.end_stroke();
        }
    }

    /// Paint one cell and record the change in the open stroke.
    ///
    /// The stroke is left open: a drag paints many cells and they are
    /// all **one** undo step (roadmap Extension 2 §2.1).
    fn paint_cell(&mut self, col: usize, row: usize) {
        if col >= self.canvas.cols() || row >= self.canvas.rows() {
            return;
        }
        if !self.cell_is_active(col, row) {
            return;
        }
        let Some(&glyph) = crate::drawn::TOOLS.get(self.tool) else {
            return;
        };
        let Some(&color) = crate::drawn::PALETTE.get(self.color) else {
            return;
        };
        let before = self.canvas.get(col, row);
        let after = (glyph, color);
        if before == Some(after) {
            return;
        }
        self.canvas.set(col, row, glyph, color);
        if let Some(stroke) = self.stroke.as_mut() {
            record_cell(stroke, col, row, before.unwrap_or((' ', [0, 0, 0])), after);
        }
        self.refresh_status();
    }

    /// Open a stroke: the pointer went down on the canvas.
    ///
    /// Every paint until [`EditorState::end_stroke`] belongs to this one
    /// stroke, so a drag is a single undo step.
    pub fn begin_stroke(&mut self) {
        self.stroke = Some(Stroke::default());
    }

    /// Close the stroke: the pointer came up.
    ///
    /// The stroke goes onto the undo stack, and the drawing is marked
    /// dirty, so the draft is written after a second of quiet.
    pub fn end_stroke(&mut self) {
        let Some(stroke) = self.stroke.take() else {
            return;
        };
        if stroke.is_empty() {
            return;
        }
        self.undo.push(stroke);
        self.mark_dirty();
    }

    /// True while a stroke is open.
    pub fn stroke_is_open(&self) -> bool {
        self.stroke.is_some()
    }

    /// Undo the last stroke.
    ///
    /// Returns `true` when a stroke was undone. The undo never touches
    /// the name field: only the canvas changes.
    pub fn undo(&mut self) -> bool {
        // A stroke still open (a lost pointer-up) belongs to the undo
        // being asked for: closing it first puts its wet cells on the
        // stack instead of leaving them un-undoable on the canvas
        // (audit 101126 §C1).
        self.end_stroke();
        let Some(stroke) = self.undo.pop_undo() else {
            return false;
        };
        apply_stroke(&mut self.canvas, &stroke, StrokeSide::Before);
        self.mark_dirty();
        self.refresh_status();
        true
    }

    /// Redo the last undone stroke.
    pub fn redo(&mut self) -> bool {
        let Some(stroke) = self.undo.pop_redo() else {
            return false;
        };
        apply_stroke(&mut self.canvas, &stroke, StrokeSide::After);
        self.mark_dirty();
        self.refresh_status();
        true
    }

    /// Mark the drawing as changed, so the draft is written after a
    /// second of quiet.
    pub fn mark_dirty(&mut self) {
        self.mark_dirty_at((self.clock)());
    }

    /// Mark the drawing as changed at `now` — the test seam, so the
    /// debounce can be tested without a clock.
    pub fn mark_dirty_at(&mut self, now: Instant) {
        self.draft_dirty = true;
        self.draft_due = Some(now + Duration::from_millis(DRAFT_DEBOUNCE_MILLIS));
    }

    /// True when the draft is due at `now`.
    pub fn draft_is_due(&self, now: Instant) -> bool {
        if !self.draft_dirty {
            return false;
        }
        self.draft_due.is_some_and(|due| now >= due)
    }

    /// The draft of the drawing as it stands.
    ///
    /// The payload is the whole drawing: the kind, the size, the frame
    /// count, the frame being drawn, the finished frames and the canvas
    /// **uncropped**, so the drawing resumes on the very same grid.
    pub fn draft_doc(&self) -> crate::drawn::DraftDoc {
        crate::drawn::DraftDoc {
            version: crate::drawn::DRAFT_VERSION,
            kind: self.kind,
            size: match self.kind {
                crate::drawn::DrawnKind::Sprite => Some(self.size),
                _ => None,
            },
            frames: self.frames.iter().map(crate::drawn::frame_doc).collect(),
            canvas: crate::drawn::frame_doc(&self.canvas),
            frames_wanted: self.frames_wanted,
            frame_index: self.frame_index,
            name: self.save_name(),
        }
    }

    /// Load a draft into the editor.
    ///
    /// The undo stack is cleared: the strokes that described the old
    /// canvas do not describe this one. The drawing is marked dirty: it
    /// is unsaved work, and it must survive a crash exactly as it was.
    ///
    /// A draft that carries more frames than the kind holds is **mended
    /// on load**: a balloon or a grass tile is one frame, always
    /// (mirroring [`EditorState::set_kind`]), and a sprite is truncated
    /// to its own frame count — the surplus used to reload into
    /// `frames.len() > frames_wanted` and was saved back with the
    /// surplus (audit 101126 §B3).
    pub fn load_draft(&mut self, doc: &crate::drawn::DraftDoc) {
        self.kind = doc.kind;
        self.size = doc.size.unwrap_or(crate::drawn::DrawnSize::Big);
        if doc.kind != crate::drawn::DrawnKind::Sprite
            && doc.kind != crate::drawn::DrawnKind::Decoration
        {
            self.frames_wanted = 1;
        } else {
            self.frames_wanted = doc.frames_wanted.clamp(1, 4);
        }
        self.frame_index = doc.frame_index.min(self.frames_wanted.saturating_sub(1));
        self.frames = doc.frames.iter().filter_map(frame_from_doc).collect();
        // One rule for every kind: the frames a draft carries beyond the
        // count the kind holds are dropped here, never re-saved.
        self.frames.truncate(self.frames_wanted);
        self.canvas =
            frame_from_doc(&doc.canvas).unwrap_or_else(|| crate::balloon::AsciiSprite::new(0, 0));
        if !doc.name.is_empty() {
            self.name = doc.name.clone();
        }
        self.undo.clear();
        self.stroke = None;
        self.draft_offer = None;
        self.draft_key = Some((self.kind, self.save_name()));
        self.mask = self.build_mask();
        self.mark_dirty();
        self.refresh_status();
    }

    /// Queue the draft write for the current drawing.
    ///
    /// The action is queued, not carried out: the state never touches
    /// the file system.
    ///
    /// A **renamed** asset leaves its old draft behind, so the old one is
    /// deleted first: the drafts folder must not fill up with the
    /// intermediate names of one drawing.
    pub fn queue_draft_write(&mut self) {
        let name = self.save_name();
        if let Some((old_kind, old_name)) = self.draft_key.clone() {
            if old_kind != self.kind || old_name != name {
                self.draft_actions.push(DraftAction::Delete {
                    kind: old_kind,
                    name: old_name,
                });
            }
        }
        self.draft_key = Some((self.kind, name.clone()));
        self.draft_actions.push(DraftAction::Write {
            kind: self.kind,
            name,
            doc: self.draft_doc(),
        });
    }

    /// Queue the deletion of the draft of the named asset.
    pub fn queue_draft_delete(&mut self, name: &str) {
        self.draft_actions.push(DraftAction::Delete {
            kind: self.kind,
            name: name.to_string(),
        });
    }

    /// Take the queued disk actions, leaving the queue empty.
    pub fn take_draft_actions(&mut self) -> Vec<DraftAction> {
        std::mem::take(&mut self.draft_actions)
    }

    /// True when the editor still holds work the user has not saved.
    ///
    /// This is what decides whether a clean exit may remove the draft.
    /// The `draft_dirty` flag is **not** the answer: the debounce clears
    /// it when the autosave fires, so it says "the autosave ran", not
    /// "the user saved the asset". Asking that flag alone made a clean
    /// exit delete the draft of a drawing that was never saved
    /// (audit 2026-10-22 §P0-1).
    ///
    /// The answer is the drawing itself: an inked canvas, or a finished
    /// frame, is work the user drew and has not saved. A blank editor
    /// has nothing behind the draft, so its draft may go.
    ///
    /// The answer stays `true` **after** a save: the ink is still on the
    /// canvas, only the file has caught up. The exit path does not read
    /// this flag to decide whether a *save* happened — the save cleared
    /// `draft_key` and queued the draft's deletion — it reads it to
    /// decide whether the **draft** must outlive the window (audit
    /// 2026-10-23 §R6-08).
    pub fn has_unsaved_work(&self) -> bool {
        !self.canvas.is_empty() || self.frames.iter().any(|f| !f.is_empty())
    }

    /// Offer the draft named `name` to the user.
    ///
    /// The draft is **not** loaded: the offer is a question, and the
    /// user answers it with the Enter key.
    pub fn offer_draft(&mut self, name: &str) {
        self.draft_offer = Some(name.to_string());
        self.set_toast(DRAFT_OFFER);
        self.refresh_status();
    }

    /// Decline the offered draft: the offer goes away, the file stays.
    ///
    /// The offer used to be unanswerable in the negative — `ENTER`
    /// loaded it and nothing else existed, so the status line was held
    /// hostage until the user gave in (audit 2026-10-23 §U-27). `ESC`
    /// now declines: the offer is cleared and the draft **file is
    /// kept**, so declining now and loading later is still possible.
    pub fn decline_draft(&mut self) {
        self.draft_offer = None;
        self.refresh_status();
    }

    /// The largest scroll offset: the active region minus the visible
    /// rows. The inactive rows below the frame are never scrolled into
    /// view (roadmap §5.4).
    pub fn max_scroll(&self, view_rows: usize) -> usize {
        self.active_rows().saturating_sub(view_rows)
    }

    /// Scroll the canvas by `delta` rows, clamped to the active region.
    pub fn scroll_by(&mut self, delta: i32, view_rows: usize) {
        let max = self.max_scroll(view_rows) as i64;
        let next = i64::try_from(self.scroll_rows).unwrap_or(i64::MAX) + i64::from(delta);
        self.scroll_rows = next.clamp(0, max) as usize;
    }

    /// The largest horizontal scroll offset: the canvas' width minus the
    /// visible columns.
    pub fn max_scroll_cols(&self, view_cols: usize) -> usize {
        self.canvas_cols().saturating_sub(view_cols)
    }

    /// Scroll the canvas by `delta` columns, clamped to the canvas.
    ///
    /// A grass tile's canvas is 200 columns wide — far wider than any
    /// window — so the canvas scrolls sideways as well as down (roadmap
    /// Extension 1 §10.1).
    pub fn scroll_by_cols(&mut self, delta: i32, view_cols: usize) {
        let max = self.max_scroll_cols(view_cols) as i64;
        let next = i64::try_from(self.scroll_cols).unwrap_or(i64::MAX) + i64::from(delta);
        self.scroll_cols = next.clamp(0, max) as usize;
    }

    /// Scroll the tool/colour panel by `delta` rows, clamped to the
    /// panel. The panel is 28 rows tall, so a short window shows only
    /// part of it — the panel scrolls instead of overflowing the window.
    pub fn scroll_panel_by(&mut self, delta: i32, visible_rows: usize) {
        let max = self.panel_rows().saturating_sub(visible_rows) as i64;
        let next = i64::try_from(self.panel_scroll).unwrap_or(i64::MAX) + i64::from(delta);
        self.panel_scroll = next.clamp(0, max) as usize;
    }

    /// How many rows the side panel has in the current mode.
    ///
    /// The strip panel holds the twelve pitches **and** the track files
    /// that exist, so its height follows the track list.
    pub fn panel_rows(&self) -> usize {
        match self.mode {
            EditorMode::Sequencer => PITCH_NAMES.len() + 1 + self.tracks.len(),
            _ => PANEL_ROWS,
        }
    }

    /// Switch into assign mode (roadmap Stage 9).
    ///
    /// The asset lists and the current assignments are read **once** here:
    /// the lists do not change while the mode is on, so they are never
    /// read per frame (AGENTS.md: never block the UI thread).
    ///
    /// The reads are the platform's (see [`EditorPlatform`]): the core
    /// never lists a folder or reads a file on its own.
    pub fn enter_assign_mode(&mut self) {
        let sprites = self.list_assets(crate::drawn::DrawnKind::Sprite);
        let balloons = self.list_assets(crate::drawn::DrawnKind::Balloon);
        let grass = self.list_assets(crate::drawn::DrawnKind::Grass);
        let decorations = self.list_assets(crate::drawn::DrawnKind::Decoration);
        // One read serves both tables: the panel's current-value display
        // for the look targets reads the same config the assignments
        // table comes from.
        let config = self.user_config("editor.assign");
        self.settings_config = config.clone();
        self.enter_assign_mode_with_assets(
            sprites,
            balloons,
            grass,
            decorations,
            config.assignments,
        );
    }

    /// Switch into assign mode with an explicit sprite and balloon list
    /// and assignment table — the **test seam**, so the tests never read
    /// the user's folders or the user's config.
    ///
    /// The grass and decoration lists start empty; the full seam
    /// ([`EditorState::enter_assign_mode_with_assets`]) is the one that
    /// carries them.
    pub fn enter_assign_mode_with(
        &mut self,
        sprites: Vec<crate::drawn::AssetEntry>,
        balloons: Vec<crate::drawn::AssetEntry>,
        assignments: crate::config::AssignmentsConfig,
    ) {
        let grass = Vec::new();
        let decorations = Vec::new();
        self.enter_assign_mode_with_assets(sprites, balloons, grass, decorations, assignments);
    }

    /// Switch into assign mode with every list explicit — the full test
    /// seam, for the grass and decoration targets (decision F-162).
    pub fn enter_assign_mode_with_assets(
        &mut self,
        sprites: Vec<crate::drawn::AssetEntry>,
        balloons: Vec<crate::drawn::AssetEntry>,
        grass: Vec<crate::drawn::AssetEntry>,
        decorations: Vec<crate::drawn::AssetEntry>,
        assignments: crate::config::AssignmentsConfig,
    ) {
        self.mode = EditorMode::Assign;
        self.close_dropdown();
        self.sprites = sprites;
        self.balloons = balloons;
        self.grass = grass;
        self.decorations = decorations;
        self.assignments = assignments;
        self.selected_asset = None;
        self.slot_scroll = 0;
        self.asset_scroll = 0;
        self.rebuild_the_asset_list();
    }

    /// Leave assign mode and go back to the drawing canvas.
    ///
    /// The drawing is never touched: the canvas, the finished frames, the
    /// ghost and the name stay exactly as they were (roadmap Stage 9).
    pub fn leave_assign_mode(&mut self) {
        self.mode = EditorMode::Draw;
        self.close_dropdown();
    }

    /// Select the target at `index` of the assign panel's list.
    ///
    /// An out-of-range index is ignored, so a click can never select a
    /// row that does not exist. The asset list follows the target's kind,
    /// so the selection changes the assets on screen without any disk
    /// access.
    pub fn select_target(&mut self, index: usize) {
        if index >= self.targets.len() {
            return;
        }
        self.assign_target = index;
        self.selected_asset = None;
        self.asset_scroll = 0;
        self.rebuild_the_asset_list();
    }

    /// Pick the asset at `index` of the selected target's asset list.
    ///
    /// An out-of-range index is ignored, so a click can never name an
    /// asset that does not exist.
    pub fn select_asset(&mut self, index: usize) {
        if index >= self.assign_assets().len() {
            return;
        }
        self.selected_asset = Some(index);
    }

    /// The target the user has selected, if any.
    pub fn selected_target(&self) -> Option<AssignTarget> {
        self.targets.get(self.assign_target).copied()
    }

    /// The assets the selected target can take: the art of the target's
    /// kind, filtered by the size class it must carry.
    ///
    /// WHY the size filter: a small sprite in a big slot is drawn in the
    /// top-left corner of the big duck's box — a broken picture, and a
    /// hit test that does not match what the user sees (audit
    /// 2026-10-21 §E1). An asset whose size the platform does not
    /// report is **kept**: the panel cannot rule out what it does not
    /// know, and the load refuses a mismatched asset anyway.
    pub fn assign_assets(&self) -> &[String] {
        &self.assign_assets
    }

    /// Rebuild the asset list of the selected target.
    fn rebuild_the_asset_list(&mut self) {
        let Some(target) = self.selected_target() else {
            self.assign_assets.clear();
            return;
        };
        let wanted = target_drawn_size(&target);
        let source: &[crate::drawn::AssetEntry] = match target_drawn_kind(&target) {
            crate::drawn::DrawnKind::Balloon => &self.balloons,
            crate::drawn::DrawnKind::Sprite => &self.sprites,
            crate::drawn::DrawnKind::Grass => &self.grass,
            crate::drawn::DrawnKind::Decoration => &self.decorations,
        };
        self.assign_assets = source
            .iter()
            .filter(|entry| wanted.is_none_or(|size| entry.size.is_none_or(|own| own == size)))
            .map(|entry| entry.name.clone())
            .collect();
    }

    /// True when the assign panel has everything it needs: a selected
    /// target **and** a picked asset.
    pub fn can_assign(&self) -> bool {
        self.selected_target().is_some() && self.selected_asset.is_some()
    }

    /// Scroll the target list by `delta` rows, clamped to the list.
    pub fn scroll_slot_list_by(&mut self, delta: i32, visible_rows: usize) {
        self.slot_scroll = scroll_offset(self.slot_scroll, delta, self.targets.len(), visible_rows);
    }

    /// Scroll the asset list by `delta` rows, clamped to the list.
    pub fn scroll_asset_list_by(&mut self, delta: i32, visible_rows: usize) {
        let len = self.assign_assets().len();
        self.asset_scroll = scroll_offset(self.asset_scroll, delta, len, visible_rows);
    }

    /// Assign the picked asset to the selected target and write the
    /// config.
    ///
    /// The write is **injected**: the production path loads, mutates and
    /// saves the real config, the tests write into a temp dir and never
    /// touch the user's `config.toml`. The write's *shape* follows the
    /// target ([`AssignWrite`]): the parade slots replace the
    /// `[assignments]` table, the look targets write one key.
    pub fn assign_action_with(
        &mut self,
        now: Instant,
        writer: impl FnOnce(AssignWrite) -> Result<(), String>,
    ) {
        let Some(target) = self.selected_target() else {
            return;
        };
        let Some(index) = self.selected_asset else {
            return;
        };
        let Some(name) = self.assign_assets().get(index).cloned() else {
            return;
        };
        let label = target_label(&target);
        let write = match self.assign_write_for(&target, &name) {
            Some(write) => write,
            None => return,
        };
        match writer(write) {
            Ok(()) => {
                self.record_the_assignment(&target, Some(name.clone()));
                self.set_toast_at(
                    &format!("{ASSIGNED} {label} = {}", name.to_uppercase()),
                    now,
                );
            }
            Err(e) => self.set_toast_at(&format!("ASSIGN FAILED: {e}"), now),
        }
    }
    /// Remove the assignment of the selected target and write the
    /// config.
    ///
    /// The write is injected, like [`EditorState::assign_action_with`].
    pub fn clear_action_with(
        &mut self,
        now: Instant,
        writer: impl FnOnce(AssignWrite) -> Result<(), String>,
    ) {
        let Some(target) = self.selected_target() else {
            return;
        };
        let label = target_label(&target);
        let write = self.clear_write_for(&target);
        match writer(write) {
            Ok(()) => {
                self.record_the_assignment(&target, None);
                self.set_toast_at(&format!("{CLEARED} {label}"), now);
            }
            Err(e) => self.set_toast_at(&format!("CLEAR FAILED: {e}"), now),
        }
    }

    /// The write an **assign** of `name` onto `target` asks for.
    ///
    /// `None` when the target does not exist — a caller that kept a
    /// stale index has nothing to write.
    fn assign_write_for(&self, target: &AssignTarget, name: &str) -> Option<AssignWrite> {
        match target {
            AssignTarget::Slot(slot) => {
                let mut table = self.assignments.clone();
                table.set(*slot, Some(name.to_string()));
                Some(AssignWrite::Assignments(Box::new(table)))
            }
            AssignTarget::GrassTile => Some(AssignWrite::Key {
                section: "grass",
                key: "tile",
                value: toml::Value::String(name.to_string()).to_string(),
            }),
            AssignTarget::DecorationBig => Some(AssignWrite::Key {
                section: "decorations",
                key: "big",
                value: toml::Value::String(name.to_string()).to_string(),
            }),
            AssignTarget::DecorationSmall => Some(AssignWrite::Key {
                section: "decorations",
                key: "small",
                value: toml::Value::String(name.to_string()).to_string(),
            }),
        }
    }

    /// The write a **clear** of `target` asks for.
    fn clear_write_for(&self, target: &AssignTarget) -> AssignWrite {
        match target {
            AssignTarget::Slot(slot) => {
                let mut table = self.assignments.clone();
                table.set(*slot, None);
                AssignWrite::Assignments(Box::new(table))
            }
            AssignTarget::GrassTile => AssignWrite::Key {
                section: "grass",
                key: "tile",
                value: toml::Value::String(String::new()).to_string(),
            },
            AssignTarget::DecorationBig => AssignWrite::Key {
                section: "decorations",
                key: "big",
                value: toml::Value::String(String::new()).to_string(),
            },
            AssignTarget::DecorationSmall => AssignWrite::Key {
                section: "decorations",
                key: "small",
                value: toml::Value::String(String::new()).to_string(),
            },
        }
    }

    /// Record what the write did in the state's copy of the config.
    ///
    /// Only a write that **landed** is recorded: a failed write leaves
    /// the state on the old values, exactly like the settings panel's
    /// write-first rule.
    fn record_the_assignment(&mut self, target: &AssignTarget, name: Option<String>) {
        match target {
            AssignTarget::Slot(slot) => self.assignments.set(*slot, name),
            AssignTarget::GrassTile => self.settings_config.grass.tile = none_if_empty(name),
            AssignTarget::DecorationBig => {
                self.settings_config.decorations.big = none_if_empty(name);
            }
            AssignTarget::DecorationSmall => {
                self.settings_config.decorations.small = none_if_empty(name);
            }
        }
    }

    /// The primary action: the next frame, or the save (roadmap §5.6).
    ///
    /// The production path writes into the platform's drawn-asset folder.
    /// The lister and the writer are the platform's (see
    /// [`EditorPlatform`]), so the core never touches a folder.
    /// The SAVE press **arms** the save (decision F-163, roadmap HELP
    /// window §3.1).
    ///
    /// The write blocks the UI thread, so a toast set in the same
    /// handler run as the save can never paint — the next frame exists
    /// only after the save returns. The press therefore only sets the
    /// `SAVING...` toast and the armed flag and returns; the shell's
    /// tick paints the toast, then completes the save through
    /// [`EditorState::primary_action`], and that flips the toast to
    /// `SAVED <NAME>`. The deferral costs one frame.
    ///
    /// A press that means **next frame** (the sprite still has poses
    /// missing) is not a save: it advances as it always did and never
    /// arms.
    pub fn arm_save(&mut self, now: Instant) {
        self.armed_save = false;
        if self.frame_index + 1 < self.frames_wanted {
            self.advance_frame(now);
            return;
        }
        self.armed_save = true;
        self.set_toast_at(SAVING_TOAST, now);
    }

    pub fn primary_action(&mut self, now: Instant) {
        let platform = self.platform;
        let exists = |kind: crate::drawn::DrawnKind, name: &str| asset_exists(platform, kind, name);
        let save =
            |sprite: &crate::drawn::DrawnSprite, kind: crate::drawn::DrawnKind, name: &str| {
                save_asset_document(platform, sprite, kind, name)
            };
        self.primary_action_with(now, exists, save);
    }

    /// The **Enter key**: the primary action in draw mode, the
    /// assignment in assign mode, the track selection in strip mode.
    ///
    /// In assign mode the SAVE button is inert, so Enter must never save
    /// the drawing behind the user's back (audit 2026-10-20 § editor,
    /// task 2): it assigns the picked asset instead, and the toast says
    /// what happened.
    ///
    /// In **strip mode** Enter points the parade at the track: the
    /// `[audio] track` key is written into the user's config. The write
    /// is **queued**, not carried out: the file system is the worker's
    /// business (roadmap Extension 3 §3.2).
    ///
    /// Every side effect is injected, like
    /// [`EditorState::primary_action_with`], so the tests never write to
    /// the user's asset folder or config.
    pub fn enter_action_with<E, S, W>(&mut self, now: Instant, exists: E, save: S, writer: W)
    where
        E: Fn(crate::drawn::DrawnKind, &str) -> bool,
        S: FnOnce(
            &crate::drawn::DrawnSprite,
            crate::drawn::DrawnKind,
            &str,
        ) -> Result<PathBuf, crate::drawn::DrawnError>,
        W: FnOnce(AssignWrite) -> Result<(), String>,
    {
        match self.mode {
            EditorMode::Draw => self.primary_action_with(now, exists, save),
            EditorMode::Assign => self.assign_entered(now, writer),
            // The settings panel writes its changes as they happen, so
            // there is nothing to commit here (roadmap Extension 2 §3.2).
            EditorMode::Settings => {}
            // The strip points the parade at the track.
            EditorMode::Sequencer => self.sequencer_entered(),
        }
    }

    /// The strip-mode Enter: point the parade at this track.
    ///
    /// A name the track folder cannot hold is refused, and the toast
    /// says so.
    fn sequencer_entered(&mut self) {
        if !self.request_track_select() {
            self.set_toast(TRACK_NAME_INVALID);
        }
    }

    /// The assign-mode Enter: assign the picked asset to the selected
    /// slot.
    ///
    /// Nothing is written when no asset is picked — the toast says what
    /// is missing instead.
    fn assign_entered(
        &mut self,
        now: Instant,
        writer: impl FnOnce(AssignWrite) -> Result<(), String>,
    ) {
        if !self.can_assign() {
            self.set_toast_at(NO_ASSET_PICKED, now);
            return;
        }
        self.assign_action_with(now, writer);
    }

    /// The primary action with an **injectable** name check and save
    /// path.
    ///
    /// `exists` reports whether a document of that name already exists —
    /// the editor refuses to overwrite one (roadmap §5.6). `save` writes
    /// the document. Both are injected so the tests never touch the
    /// user's real asset folder.
    pub fn primary_action_with<E, S>(&mut self, now: Instant, exists: E, save: S)
    where
        E: Fn(crate::drawn::DrawnKind, &str) -> bool,
        S: FnOnce(
            &crate::drawn::DrawnSprite,
            crate::drawn::DrawnKind,
            &str,
        ) -> Result<PathBuf, crate::drawn::DrawnError>,
    {
        if self.frame_index + 1 < self.frames_wanted {
            self.advance_frame(now);
            return;
        }
        self.save_document(now, exists, save);
    }

    /// Keep the current frame, clear the canvas and show the previous
    /// frame as the ghost (roadmap §5.6).
    ///
    /// The undo stack is cleared: the strokes that described the old
    /// canvas do not describe the new one, and an undo must never write
    /// into a frame the user did not draw.
    fn advance_frame(&mut self, now: Instant) {
        let frame = crate::drawn::crop_to_active(&self.canvas, self.kind, self.size);
        self.ghost = Some(frame.clone());
        self.frames.push(frame);
        let (cols, rows) = self.drawing_canvas_size();
        self.canvas = crate::balloon::AsciiSprite::new(cols, rows);
        self.frame_index += 1;
        self.undo.clear();
        self.stroke = None;
        // The draft carries the finished frames and the index, so the
        // frame split is draft-relevant content: the autosave used to
        // lag until the next stroke (audit 101126 §B4).
        self.mark_dirty_at(now);
        self.set_toast_at(NEXT_FRAME_TOAST, now);
        self.refresh_status();
    }

    /// Write the document: the finished frames plus the canvas, each one
    /// cropped to the active region (the inactive rows of the square
    /// canvas are never written to disk).
    fn save_document<E, S>(&mut self, now: Instant, exists: E, save: S)
    where
        E: Fn(crate::drawn::DrawnKind, &str) -> bool,
        S: FnOnce(
            &crate::drawn::DrawnSprite,
            crate::drawn::DrawnKind,
            &str,
        ) -> Result<PathBuf, crate::drawn::DrawnError>,
    {
        let kind = self.kind;
        let name = self.save_name();
        if exists(kind, name.as_str()) {
            self.set_toast_at(NAME_IN_USE, now);
            return;
        }
        let document = self.document();
        match save(&document, kind, name.as_str()) {
            Ok(_) => {
                // The asset is on disk, so the draft has done its job:
                // saving the asset deletes the draft (roadmap Extension 2
                // §2.2). A draft written under a **renamed** asset leaves
                // its old file behind — nulling `draft_key` alone
                // orphaned it and later sessions re-offered it forever
                // (audit 101126 §B1).
                let stale = self
                    .draft_key
                    .take()
                    .filter(|(k, n)| *k != kind || *n != name);
                if let Some((old_kind, old_name)) = stale {
                    self.draft_actions.push(DraftAction::Delete {
                        kind: old_kind,
                        name: old_name,
                    });
                }
                self.queue_draft_delete(name.as_str());
                self.draft_offer = None;
                self.draft_dirty = false;
                self.draft_due = None;
                self.set_toast_at(&format!("SAVED {}", name.to_uppercase()), now);
            }
            Err(e) => self.set_toast_at(&format!("SAVE FAILED: {e}"), now),
        }
        self.refresh_status();
    }

    /// The document the current drawing is stored as.
    ///
    /// The frames are the finished ones plus the canvas, in order; a
    /// balloon always has exactly one frame, and so does a grass tile.
    pub fn document(&self) -> crate::drawn::DrawnSprite {
        let kind = self.kind;
        let size = match kind {
            // A decoration is drawn over a duck, so it carries the duck's
            // size: its frame is the big frame or the small one.
            crate::drawn::DrawnKind::Sprite | crate::drawn::DrawnKind::Decoration => {
                Some(self.size)
            }
            crate::drawn::DrawnKind::Balloon | crate::drawn::DrawnKind::Grass => None,
        };
        let mut frames: Vec<crate::balloon::AsciiSprite> = self
            .frames
            .iter()
            .map(|frame| self.cropped_frame(frame))
            .collect();
        frames.push(self.cropped_frame(&self.canvas));
        // A balloon and a grass tile are one frame; a sprite and a
        // decoration are animated, and a decoration's frames step in
        // step with the body's.
        if matches!(
            kind,
            crate::drawn::DrawnKind::Balloon | crate::drawn::DrawnKind::Grass
        ) {
            frames.truncate(1);
        }
        crate::drawn::DrawnSprite {
            kind,
            size,
            frames,
            reaction: None,
        }
    }

    /// One frame of the document: the active region for a sprite or a
    /// balloon, the inked rectangle for a grass tile.
    ///
    /// A tile is **tiled** across the ground band, so a 20-column
    /// drawing must be stored as a 20-column tile: the blank columns
    /// around it would repeat across the whole screen.
    fn cropped_frame(&self, frame: &crate::balloon::AsciiSprite) -> crate::balloon::AsciiSprite {
        match self.kind {
            crate::drawn::DrawnKind::Grass => crop_to_ink(frame),
            _ => crate::drawn::crop_to_active(frame, self.kind, self.size),
        }
    }

    /// The grass tile the drawing is saved as: the canvas cropped to its
    /// ink.
    ///
    /// The status line shows this tile's size, so the user sees the
    /// asset they are about to write — the same crop the save uses.
    pub fn grass_tile(&self) -> crate::balloon::AsciiSprite {
        crop_to_ink(&self.canvas)
    }

    /// The name the document is stored under: the name field, or the
    /// first free name of the kind when the field is empty.
    ///
    /// The **placeholder** is what gets written when the user typed
    /// nothing: it is the name the field showed, so the file on disk
    /// always carries the name the user could see (audit 2026-10-20 §
    /// editor, task 3).
    fn save_name(&self) -> String {
        if self.name.is_empty() {
            return self.default_name.clone();
        }
        self.name.clone()
    }

    /// The toast message, while it is still on screen.
    pub fn active_toast(&self) -> Option<&str> {
        self.active_toast_at((self.clock)())
    }

    /// The toast message at the moment `now` (deterministic — the state
    /// never reads the clock itself).
    pub fn active_toast_at(&self, now: Instant) -> Option<&str> {
        if !toast_visible(&self.toast, now) {
            return None;
        }
        self.toast.as_ref().map(|(text, _)| text.as_str())
    }

    /// Raise a transient message on the status line.
    pub fn set_toast(&mut self, text: &str) {
        self.set_toast_at(text, (self.clock)());
    }

    /// Raise a transient message that was raised at `now`.
    ///
    /// Public so the assign panel (Stage 9) can raise its own toasts
    /// with the same lifetime.
    pub fn set_toast_at(&mut self, text: &str, now: Instant) {
        self.toast = Some((text.to_string(), now));
    }

    /// Rebuild the status line from the current dropdown values.
    ///
    /// The line is short on purpose: the block font is six cells per
    /// character, so a long line would not fit a 1536 px window. The
    /// values that do not fit fall back to shorter forms in
    /// [`status_text`].
    fn refresh_status(&mut self) {
        self.status = match self.mode {
            // The strip is not a drawing: the line names the track, the
            // pitch and the length of the strip.
            EditorMode::Sequencer => self.strip_status(),
            _ => match self.kind {
                // A grass tile has no frame count and no duck size: the line
                // names the tile, its size and the caps it must respect.
                crate::drawn::DrawnKind::Grass => grass_status_full(&self.grass_tile()),
                _ => format!(
                    "TYPE: {}  {}  {}  FRAME {}/{}  {}  {}",
                    self.dropdown_long(0),
                    self.size_label(),
                    self.dropdown_long(2),
                    self.frame_index + 1,
                    self.frames_wanted,
                    tool_label(self.tool),
                    colour_label(self.color),
                ),
            },
        };
    }

    /// The size shown in the status line: the duck size of a sprite, a
    /// dash for a balloon (a balloon has no size) and `TILE` for a grass
    /// tile (a tile is a band, not a duck).
    pub fn size_label(&self) -> &'static str {
        match self.kind {
            crate::drawn::DrawnKind::Sprite | crate::drawn::DrawnKind::Decoration => {
                self.dropdown_long(1)
            }
            crate::drawn::DrawnKind::Balloon => "-",
            crate::drawn::DrawnKind::Grass => "TILE",
        }
    }

    /// Blank the canvas and everything that belongs to the old element.
    ///
    /// The undo stack is cleared with it: the strokes described the old
    /// grid, and the new element has a grid of its own.
    fn reset_canvas(&mut self) {
        let (cols, rows) = self.drawing_canvas_size();
        self.canvas = crate::balloon::AsciiSprite::new(cols, rows);
        self.frames.clear();
        self.ghost = None;
        self.frame_index = 0;
        self.scroll_rows = 0;
        self.scroll_cols = 0;
        self.panel_scroll = 0;
        self.mask = self.build_mask();
        self.undo.clear();
        self.stroke = None;
    }

    /// The active-cell mask of the current element (empty for a sprite).
    fn build_mask(&self) -> Vec<bool> {
        match self.kind {
            crate::drawn::DrawnKind::Balloon => {
                crate::balloon::balloon_authoring_mask(&self.balloon_geo)
            }
            crate::drawn::DrawnKind::Sprite
            | crate::drawn::DrawnKind::Grass
            | crate::drawn::DrawnKind::Decoration => Vec::new(),
        }
    }

    // -----------------------------------------------------------------
    // The settings panel (roadmap Extension 2 §3)
    // -----------------------------------------------------------------

    /// Switch into the settings panel.
    ///
    /// The panel reads the user's `config.toml` **once**, here: the file
    /// is a few kB and the panel must not read it per frame
    /// (AGENTS.md: never block the UI thread).
    ///
    /// The read is the platform's (see [`EditorPlatform`]).
    pub fn enter_settings_mode(&mut self) {
        let config = self.load_settings_config();
        self.enter_settings_mode_with(config);
    }

    /// Switch into the settings panel with an explicit config — the
    /// **test seam**, so the tests never read the user's file.
    pub fn enter_settings_mode_with(&mut self, config: crate::config::Config) {
        self.settings_config = config;
        self.settings_scroll = 0;
        self.settings_cursor = 0;
        self.settings_focus = None;
        self.settings_text.clear();
        self.settings_typed = false;
        self.settings_open = None;
        self.settings_staged = None;
        self.mode = EditorMode::Settings;
    }

    /// Switch back into draw mode.
    ///
    /// The settings panel is left the way the ASSIGN panel is: the mode
    /// switch never touches the drawing.
    pub fn leave_settings_mode(&mut self) {
        self.settings_focus = None;
        self.settings_text.clear();
        self.settings_typed = false;
        self.settings_open = None;
        self.settings_staged = None;
        self.mode = EditorMode::Draw;
    }

    /// Leave the panel the editor is in and go back to drawing.
    ///
    /// The DRAW button works from every mode: it is the way out of the
    /// assign panel, of the settings panel and of the note strip alike.
    pub fn leave_any_mode(&mut self) {
        match self.mode {
            EditorMode::Settings => self.leave_settings_mode(),
            EditorMode::Sequencer => self.leave_sequencer_mode(),
            _ => self.leave_assign_mode(),
        }
    }

    /// The setting row the cursor is on.
    pub fn settings_row(&self) -> Option<&'static SettingRow> {
        SETTING_ROWS.get(self.settings_cursor)
    }

    /// The value the panel shows for one row.
    pub fn settings_value(&self, row: usize) -> Option<SettingValue> {
        let row = SETTING_ROWS.get(row)?;
        read_setting(&self.settings_config, row)
    }

    /// Change one setting and write it into the user's config.
    ///
    /// The value is written **first**: a value that cannot be stored
    /// leaves the panel showing the old one and the toast explaining why
    /// (roadmap Extension 2 §3.2). The editor never shows a value it
    /// failed to store.
    ///
    /// The writer is injected, so the tests never touch the user's real
    /// config file.
    pub fn change_setting_with<W>(
        &mut self,
        row: usize,
        value: SettingValue,
        now: Instant,
        writer: W,
    ) -> bool
    where
        W: FnOnce(&str, &str, &str) -> Result<(), String>,
    {
        let Some(setting) = SETTING_ROWS.get(row) else {
            return false;
        };
        let value = clamp_setting(setting, value, &self.settings_config);
        let rendered = render_setting_value(setting, &value);
        if let Err(reason) = writer(setting.section, setting.key, rendered.as_str()) {
            self.set_toast_at(&format!("SAVE FAILED: {reason}"), now);
            return false;
        }
        apply_setting(&mut self.settings_config, setting, &value);
        self.set_toast_at(&format!("SAVED {}", setting.label), now);
        true
    }

    /// Run the action of an action row (the PERSONALITY buttons, roadmap
    /// HELP window §2).
    ///
    /// An action row writes a whole array-of-tables section, not one key,
    /// so it does not go through the settings writer: the **runner** is
    /// the app's own seam (the shell seeds the randomiser from the host
    /// clock and lands the write; the tests record instead). The toast
    /// discipline is [`EditorState::change_setting_with`]'s: a failed run
    /// leaves everything as it was and the toast explains why.
    ///
    /// Returns `true` when the row had an action and the action ran.
    pub fn run_settings_action_with<W>(&mut self, row: usize, now: Instant, runner: W) -> bool
    where
        W: FnOnce(&str) -> Result<(), String>,
    {
        let Some(setting) = SETTING_ROWS.get(row) else {
            return false;
        };
        let SettingKind::Action(_) = setting.kind else {
            return false;
        };
        match runner(setting.key) {
            Ok(()) => {
                self.set_toast_at(&action_toast(setting.key), now);
                true
            }
            Err(reason) => {
                self.set_toast_at(&format!("SAVE FAILED: {reason}"), now);
                false
            }
        }
    }

    /// The value of a slider row at a fraction of its range.
    ///
    /// The **effective** range is used, so the slider and the clamp
    /// agree: a row whose bound is relative to another row moves with
    /// it (audit 2026-10-21 §F7).
    pub fn slider_value(&self, row: usize, fraction: f32) -> Option<SettingValue> {
        let setting = SETTING_ROWS.get(row)?;
        let fraction = fraction.clamp(0.0, 1.0);
        let value = match setting.kind {
            SettingKind::Int { min, max } => {
                let (min, max) =
                    effective_int_range(setting, &self.settings_config).unwrap_or((min, max));
                let span = (max - min) as f32;
                SettingValue::Int(min + (span * fraction).round() as i64)
            }
            SettingKind::Float { min, max } => {
                let span = max - min;
                SettingValue::Float(min + span * fraction)
            }
            _ => return None,
        };
        Some(value)
    }

    /// The fraction of a slider row's range the value sits at.
    ///
    /// The **effective** range is used, so the slider and the clamp
    /// agree (audit 2026-10-21 §F7).
    pub fn slider_fraction(&self, row: usize) -> Option<f32> {
        let setting = SETTING_ROWS.get(row)?;
        let value = read_setting(&self.settings_config, setting)?;
        let fraction = match (setting.kind, value) {
            (SettingKind::Int { min, max }, SettingValue::Int(number)) => {
                let (min, max) =
                    effective_int_range(setting, &self.settings_config).unwrap_or((min, max));
                let span = (max - min).max(1) as f32;
                (number - min) as f32 / span
            }
            (SettingKind::Float { min, max }, SettingValue::Float(number)) => {
                let span = (max - min).max(f32::MIN_POSITIVE);
                (number - min) / span
            }
            _ => return None,
        };
        Some(fraction.clamp(0.0, 1.0))
    }

    /// Focus a text row: the keyboard edits it from now on.
    ///
    /// The field starts **empty** and shows the current value as a
    /// placeholder: the first character the user types replaces it, and
    /// Backspace clears it (the name field's rule, audit 2026-10-20 §
    /// editor, task 3).
    pub fn focus_text_row(&mut self, row: usize) {
        let Some(setting) = SETTING_ROWS.get(row) else {
            return;
        };
        if setting.kind != SettingKind::Text {
            return;
        }
        self.settings_text.clear();
        self.settings_typed = false;
        self.settings_focus = Some(row);
    }

    /// Stop editing the text field.
    pub fn unfocus_text(&mut self) {
        self.settings_focus = None;
        self.settings_text.clear();
    }

    /// Append a character to the focused text field.
    ///
    /// The first character **replaces** the placeholder: the field shows
    /// the current value until the user types, and typing starts a new
    /// value.
    pub fn push_settings_text(&mut self, text: &str) {
        if self.settings_focus.is_none() {
            return;
        }
        if self.settings_typed {
            self.settings_text.push_str(text);
            return;
        }
        self.settings_text = text.to_string();
        self.settings_typed = true;
    }

    /// Delete the last character of the focused text field.
    ///
    /// Backspace on an untouched field **clears** it: the placeholder
    /// goes away and the field is empty.
    pub fn pop_settings_text(&mut self) {
        if self.settings_focus.is_none() {
            return;
        }
        if !self.settings_typed {
            self.settings_text.clear();
            self.settings_typed = true;
            return;
        }
        self.settings_text.pop();
    }

    /// Move the settings cursor by `delta` rows, keeping it on a row.
    pub fn move_settings_cursor(&mut self, delta: i32) {
        let last = SETTING_ROWS.len().saturating_sub(1) as i64;
        let next = i64::try_from(self.settings_cursor).unwrap_or(0) + i64::from(delta);
        self.settings_cursor = next.clamp(0, last) as usize;
    }

    /// The value a row takes when the user steps it by `delta`.
    ///
    /// `delta` is `-1` or `1`: one step left or one step right.
    ///
    /// A **slider** steps by a twentieth of its range (at least one
    /// unit), so the whole range is reachable in twenty presses — the
    /// same granularity a drag of a fifth of the track has. A **toggle**
    /// flips. A **choice** steps through its options and wraps. A **text**
    /// row has nothing to step: the answer is `None`, and the caller
    /// focuses the field instead.
    ///
    /// An integer slider steps inside its **effective** range — the
    /// cross-row clamp the mouse path honours (see
    /// [`effective_int_range`]) — so the keyboard and the pointer can
    /// never disagree about where a rope bound may go (audit
    /// 2026-10-23 §X-01).
    ///
    /// The keyboard route into the panels was missing, so every
    /// slider/toggle/dropdown was reachable only with the mouse
    /// (audit 2026-10-22 §UX-01).
    pub fn stepped_setting_value(&self, row: usize, delta: i32) -> Option<SettingValue> {
        let setting = SETTING_ROWS.get(row)?;
        let current = read_setting(&self.settings_config, setting)?;
        let step = i32::from(delta >= 0) - i32::from(delta < 0);
        match (setting.kind, current) {
            (SettingKind::Int { min, max }, SettingValue::Int(value)) => {
                let (min, max) =
                    effective_int_range(setting, &self.settings_config).unwrap_or((min, max));
                let span = (max - min).max(1);
                let by = (span / 20).max(1) * i64::from(step);
                Some(SettingValue::Int((value + by).clamp(min, max)))
            }
            (SettingKind::Float { min, max }, SettingValue::Float(value)) => {
                let span = (max - min).max(0.0);
                let by = (span / 20.0).max(0.01) * step as f32;
                Some(SettingValue::Float((value + by).clamp(min, max)))
            }
            (SettingKind::Bool, SettingValue::Bool(flag)) => Some(SettingValue::Bool(!flag)),
            (SettingKind::Choice(options), SettingValue::Choice(index)) => {
                let last = options.len().checked_sub(1)?;
                let next = (index as i64 + i64::from(step)).rem_euclid(options.len() as i64);
                Some(SettingValue::Choice(next.min(last as i64) as usize))
            }
            _ => None,
        }
    }

    /// Step the value of the row the keyboard cursor is on.
    ///
    /// The keyboard's Left/Right in the settings panel (audit
    /// 2026-10-23 §X-01): a choice moves to its neighbour, a slider
    /// nudges by its step, a toggle flips. The new value is **staged**
    /// (see [`EditorState::settings_staged`]) — the state never writes
    /// the file, and the app commits the staged pair through
    /// [`EditorState::change_setting_with`], so a failed write still
    /// leaves the panel on the old value.
    ///
    /// Returns `true` when the row had a value to step. A text row does
    /// not: the answer is `false`, and the caller focuses the field
    /// (the same thing a click on the field does).
    pub fn settings_step(&mut self, direction: i32) -> bool {
        let row = self.settings_cursor;
        let Some(value) = self.stepped_setting_value(row, direction) else {
            return false;
        };
        self.settings_staged = Some((row, value));
        true
    }

    /// The keyboard's Enter/Space on the settings panel's cursor row.
    ///
    /// What "activate" means depends on the row (audit 2026-10-23
    /// §X-01): a **toggle** flips (staged, like [`EditorState::settings_step`]),
    /// a **choice** opens its option list — or closes it, when it is
    /// already open, so the key is also the way out — and a **text**
    /// row takes the focus, the same thing a click on the field does.
    pub fn settings_activate(&mut self) {
        let row = self.settings_cursor;
        let Some(setting) = SETTING_ROWS.get(row) else {
            return;
        };
        match setting.kind {
            SettingKind::Bool => {
                let flipped = self
                    .stepped_setting_value(row, 1)
                    .unwrap_or(SettingValue::Bool(true));
                self.settings_staged = Some((row, flipped));
            }
            SettingKind::Choice(_) => self.settings_open = toggle_open_row(self.settings_open, row),
            SettingKind::Text => self.focus_text_row(row),
            // A slider has nothing to stage on a bare press, and an
            // **action** runs through the app's runner
            // ([`crate::editor::EditorState::run_settings_action_with`]),
            // which the caller owns — the core has no writer to lend it.
            SettingKind::Int { .. } | SettingKind::Float { .. } | SettingKind::Action(_) => {}
        }
    }

    /// Take the keyboard's staged edit, if any.
    ///
    /// The app writes the pair with its own writer (through
    /// [`EditorState::change_setting_with`]) — writing first and
    /// applying only on success, exactly like the mouse path — and the
    /// panel only then shows the new value.
    pub fn take_staged_setting(&mut self) -> Option<(usize, SettingValue)> {
        self.settings_staged.take()
    }

    /// Scroll the settings panel by `delta` rows.
    pub fn scroll_settings_by(&mut self, delta: i32, visible: usize) {
        let max = SETTING_ROWS.len().saturating_sub(visible) as i64;
        let next = i64::try_from(self.settings_scroll).unwrap_or(0) + i64::from(delta);
        self.settings_scroll = next.clamp(0, max.max(0)) as usize;
    }

    // -----------------------------------------------------------------
    // The note strip (roadmap Extension 3 §3.2)
    // -----------------------------------------------------------------

    /// Switch into the strip editor.
    ///
    /// The user's config is read **once**, here: the strip's length and
    /// the name of the track the parade plays are config values, and the
    /// track folder is a directory scan — neither may happen per frame
    /// (AGENTS.md: never block the UI thread).
    ///
    /// The config is the platform's and the track folder is the
    /// **injected** one (see [`EditorPlatform`] and
    /// [`EditorState::set_tracks_dir`]).
    pub fn enter_sequencer_mode(&mut self) {
        let config = self.load_settings_config();
        let dir = self.tracks_dir.clone();
        let tracks = load_track_list(dir.as_deref());
        self.enter_sequencer_mode_with(config, dir, tracks);
    }

    /// Switch into the strip editor with an explicit config, track folder
    /// and track list — the **test seam**, so the tests never read the
    /// user's folders.
    pub fn enter_sequencer_mode_with(
        &mut self,
        config: crate::config::Config,
        dir: Option<std::path::PathBuf>,
        tracks: Vec<String>,
    ) {
        let was_strip = self.mode == EditorMode::Sequencer;
        self.mode = EditorMode::Sequencer;
        self.close_dropdown();
        self.strip_steps = config.audio.strip_steps.clamp(
            crate::config::bounds::STRIP_STEPS_MIN,
            crate::config::bounds::STRIP_STEPS_MAX,
        );
        self.tracks = tracks;
        // The notes and the name are **kept**: switching the mode never
        // throws the user's work away (roadmap Stage 9). The configured
        // track is read the first time the strip is opened.
        if self.track_sync == TrackSync::Fresh {
            self.track_name = config.audio.track.clone().unwrap_or_default();
            self.load_the_configured_track(dir);
        }
        // The strip starts at the loop's beginning, and the drawing keeps
        // the horizontal scroll it had (roadmap Stage 9: the mode switch
        // never touches the drawing). Re-entering the strip keeps the
        // drawing's scroll where it was.
        if !was_strip {
            self.draw_scroll_cols = self.scroll_cols;
        }
        self.scroll_cols = 0;
        self.scroll_rows = 0;
        self.panel_scroll = 0;
    }

    /// Read the track the user's config names, if there is one.
    ///
    /// A missing track is not an error: the strip starts empty and the
    /// user writes the first note.
    fn load_the_configured_track(&mut self, dir: Option<std::path::PathBuf>) {
        let name = self.track_name.clone();
        if name.is_empty() {
            return;
        }
        // The configured-track read is attempted exactly once: whatever
        // it found (the file, or nothing) is what the strip now holds.
        self.track_sync = TrackSync::Synced;
        let Some(dir) = dir else {
            return;
        };
        match crate::audio::sequencer::load_track(&dir, &name) {
            Ok(track) => {
                self.track = crate::audio::sequencer::normalise_track(&track, self.strip_steps);
            }
            Err(e) => {
                crate::trace::warn(&format!("editor.track error: {e}"));
                self.set_toast(&format!("LOAD FAILED: {e}"));
            }
        }
    }

    /// Leave the strip editor and go back to the drawing canvas.
    ///
    /// The drawing is never touched: the canvas, the finished frames, the
    /// ghost and the name stay exactly as they were.
    pub fn leave_sequencer_mode(&mut self) {
        self.mode = EditorMode::Draw;
        self.close_dropdown();
        self.scroll_cols = self.draw_scroll_cols;
        self.scroll_rows = 0;
    }

    /// Select the pitch of the palette at `index`.
    ///
    /// An out-of-range index is ignored, so a click can never select a
    /// pitch that does not exist.
    pub fn select_pitch(&mut self, index: usize) {
        if index >= PITCH_NAMES.len() {
            return;
        }
        self.pitch = index;
        self.refresh_status();
    }

    /// The name of the selected pitch (`C#3`).
    pub fn pitch_name(&self) -> &'static str {
        PITCH_NAMES
            .get(self.pitch)
            .copied()
            .unwrap_or(PITCH_NAMES[0])
    }

    /// The selected pitch as the **status line** shows it: `#` becomes
    /// `-`, so `C#3` reads `C-3`.
    ///
    /// The block font carries no `#` glyph, so the true name rendered as
    /// `C3` — the **wrong pitch**, and seven of the twelve palette
    /// entries were mislabelled (audit 2026-10-23 §U-24). The alias is
    /// used by the strip's status echo only: the palette panel is drawn
    /// in the base layer with the TrueType font, which carries the real
    /// `#`, so the panel keeps the true names.
    pub fn pitch_display(&self) -> &'static str {
        PITCH_DISPLAYS
            .get(self.pitch)
            .copied()
            .unwrap_or(PITCH_DISPLAYS[0])
    }

    /// The MIDI pitch the next placed note gets.
    pub fn pitch_midi(&self) -> u8 {
        PITCH_MIDI.get(self.pitch).copied().unwrap_or(PITCH_MIDI[0])
    }

    /// Place a note on the step — or remove the note that is there.
    ///
    /// The note rings for one step: the duration is a v2 nicety
    /// (roadmap Extension 3 §3.2), so the strip holds one note per step.
    ///
    /// The change is **undoable**: it goes onto the strip's own history,
    /// which is separate from the canvas's (a note is not a cell).
    pub fn place_note(&mut self, step: usize) {
        if step >= self.strip_steps {
            return;
        }
        let before = self.track.get(&step).copied();
        let after = match before {
            Some(_) => None,
            None => Some(crate::audio::Note {
                pitch: self.pitch_midi(),
                duration_steps: 1,
                velocity: DEFAULT_VELOCITY,
            }),
        };
        self.write_note(step, before, after);
    }

    /// Write one note change onto the strip and record it.
    fn write_note(
        &mut self,
        step: usize,
        before: Option<crate::audio::Note>,
        after: Option<crate::audio::Note>,
    ) {
        match after {
            Some(note) => {
                self.track.insert(step, note);
            }
            None => {
                self.track.remove(&step);
            }
        }
        self.strip_undo.push(NoteDelta {
            step,
            before,
            after,
        });
        self.track_sync = TrackSync::Dirty;
        self.refresh_status();
    }

    /// Undo the last note change.
    ///
    /// Returns `true` when a change was undone.
    pub fn undo_note(&mut self) -> bool {
        let Some(delta) = self.strip_undo.pop_undo() else {
            return false;
        };
        self.restore_note(&delta, true);
        true
    }

    /// Redo the last undone note change.
    pub fn redo_note(&mut self) -> bool {
        let Some(delta) = self.strip_undo.pop_redo() else {
            return false;
        };
        self.restore_note(&delta, false);
        true
    }

    /// Put a note back the way it was (or the way it became).
    fn restore_note(&mut self, delta: &NoteDelta, before: bool) {
        let note = if before { delta.before } else { delta.after };
        match note {
            Some(note) => {
                self.track.insert(delta.step, note);
            }
            None => {
                self.track.remove(&delta.step);
            }
        }
        self.track_sync = TrackSync::Dirty;
        self.refresh_status();
    }

    /// Append a character to the track name.
    ///
    /// Only `a-z`, `0-9`, `-` and `_` are accepted — the same set the
    /// track file's name may carry, so a name can never leave the track
    /// folder. Returns `false` when the character is rejected.
    pub fn push_track_name_char(&mut self, c: char) -> bool {
        if !is_legal_name_char(c) {
            return false;
        }
        if self.track_name.chars().count() >= crate::audio::sequencer::MAX_NAME_LEN {
            return false;
        }
        self.track_name.push(c);
        true
    }

    /// Delete the last character of the track name.
    pub fn pop_track_name_char(&mut self) {
        self.track_name.pop();
    }

    /// The track the SAVE button would write: the name and the notes.
    ///
    /// `None` when the name is not a legal track name.
    pub fn track_save_request(&self) -> Option<(String, crate::audio::Track)> {
        let name = self.track_name.trim();
        if crate::audio::sequencer::validate_name(name).is_err() {
            return None;
        }
        Some((name.to_string(), self.track.clone()))
    }

    /// The track the parade should play.
    ///
    /// `None` when the name is not a legal track name.
    pub fn track_select_request(&self) -> Option<String> {
        let name = self.track_name.trim();
        if crate::audio::sequencer::validate_name(name).is_err() {
            return None;
        }
        Some(name.to_string())
    }

    /// Ask for the track file to be written.
    ///
    /// The request is **queued**, not carried out: the file system is
    /// the worker's business, not the state's (AGENTS.md: never block the
    /// UI thread).
    ///
    /// Returns `false` when the name is not a legal track name — the
    /// caller says so in the toast.
    pub fn request_track_save(&mut self) -> bool {
        let Some((name, track)) = self.track_save_request() else {
            return false;
        };
        self.track_request = Some(TrackRequest::Save { name, track });
        true
    }

    /// Ask for a track file to be read into the strip.
    pub fn request_track_load(&mut self, name: &str) -> bool {
        if crate::audio::sequencer::validate_name(name).is_err() {
            return false;
        }
        self.track_request = Some(TrackRequest::Load {
            name: name.to_string(),
        });
        true
    }

    /// Ask for the parade to play this track.
    pub fn request_track_select(&mut self) -> bool {
        let Some(name) = self.track_select_request() else {
            return false;
        };
        self.track_request = Some(TrackRequest::Select { name });
        true
    }

    /// Take the queued track request, leaving the queue empty.
    pub fn take_track_request(&mut self) -> Option<TrackRequest> {
        self.track_request.take()
    }

    /// True when the strip holds notes the track file does not.
    ///
    /// This is the exit path's question: a strip whose notes were never
    /// saved (or were changed after a load) is work that dies with the
    /// window, and nothing anywhere said so (audit 2026-10-23 §X-04).
    /// An empty strip is not unsaved work — there is nothing to lose.
    pub fn track_unsaved(&self) -> bool {
        self.track_sync == TrackSync::Dirty && !self.track.is_empty()
    }

    /// Carry a pending track save out **now**, on the caller's thread.
    ///
    /// The strip's SAVE queues a [`TrackRequest::Save`] for the worker,
    /// so the UI thread never waits on the disk. The exit path is the
    /// one exception (audit 2026-10-23 §X-04): a SAVE pressed a moment
    /// before ESC must not die with the window, so the app calls this
    /// from `quit` — one small atomic JSON write, the same one the
    /// worker performs, through the injected tracks dir (the state
    /// still resolves no location of its own).
    ///
    /// `true` when a save was carried out. A `Load` or `Select` request
    /// is left on the queue untouched: their moment left with the
    /// window, and the parade reads the file on its own.
    pub fn flush_track_request(&mut self) -> bool {
        let Some(TrackRequest::Save { name, track }) = self.track_request.as_ref().cloned() else {
            return false;
        };
        let Some(dir) = self.tracks_dir.clone() else {
            return false;
        };
        self.track_request = None;
        let now = (self.clock)();
        let outcome = match crate::audio::sequencer::save_track(&dir, &name, &track) {
            Ok(()) => TrackOutcome::Saved { name },
            Err(e) => TrackOutcome::Failed {
                action: "SAVE",
                reason: e.to_string(),
            },
        };
        self.apply_track_outcome(&outcome, now);
        true
    }

    /// Apply what the track worker did.
    ///
    /// The toast says exactly what happened: a saved track, a loaded
    /// track, a track the parade plays — or the reason the write
    /// failed. A failed write changes nothing, so the strip never shows
    /// a track that is not on the disk (roadmap Extension 3 §3.4).
    pub fn apply_track_outcome(&mut self, outcome: &TrackOutcome, now: Instant) {
        match outcome {
            TrackOutcome::Saved { name } => {
                self.track_sync = TrackSync::Synced;
                self.set_toast_at(&format!("SAVED {}", name.to_uppercase()), now);
            }
            TrackOutcome::Loaded { name, track } => {
                self.track = crate::audio::sequencer::normalise_track(track, self.strip_steps);
                self.track_name = name.clone();
                self.strip_undo.clear();
                self.track_sync = TrackSync::Synced;
                self.set_toast_at(&format!("LOADED {}", name.to_uppercase()), now);
            }
            TrackOutcome::Selected { name } => {
                self.set_toast_at(&format!("PARADE PLAYS {}", name.to_uppercase()), now);
            }
            TrackOutcome::Failed { action, reason } => {
                self.set_toast_at(&format!("{action} FAILED: {reason}"), now);
            }
        }
        self.refresh_status();
    }

    /// The status line of the strip mode: the track, the pitch, the
    /// number of notes and the length of the strip.
    ///
    /// The pitch is shown through [`EditorState::pitch_display`] — the
    /// block font has no `#`, so the true name would read as the wrong
    /// natural pitch (audit 2026-10-23 §U-24).
    pub fn strip_status(&self) -> String {
        let name = if self.track_name.is_empty() {
            "-"
        } else {
            self.track_name.as_str()
        };
        format!(
            "TRACK: {}  PITCH {}  NOTES {}  STEPS {}",
            name.to_uppercase(),
            self.pitch_display(),
            self.track.len(),
            self.strip_steps
        )
    }
}

// ---------------------------------------------------------------------------
// The settings panel (roadmap Extension 2 §3)
// ---------------------------------------------------------------------------

/// Base cells of the settings panel's label column.
const SETTINGS_LABEL_CELLS: i64 = 17;

/// Base cells between the label column and the value column.
const SETTINGS_VALUE_GAP_CELLS: i64 = 1;

/// Base cells of the settings panel's value column.
///
/// The column is wide enough for the longest value the panel shows (a
/// 150-column rope bound) and for a slider track.
const SETTINGS_VALUE_CELLS: i64 = 30;

/// Base cells of a slider's track — the left part of the value column.
///
/// The rest of the column shows the value itself.
const SETTINGS_TRACK_CELLS: i64 = 18;

/// Base cells between the value column and the unit column.
const SETTINGS_UNIT_GAP_CELLS: i64 = 1;

/// Base cells of the settings panel's unit column.
const SETTINGS_UNIT_CELLS: i64 = 10;

/// The kind of value a settings row edits.
///
/// The kind carries the **validation**: a slider's range is the
/// validation, so an out-of-range value is not reachable by clicking
/// (roadmap Extension 2 §3.2).
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SettingKind {
    /// An integer in a range — a slider.
    Int {
        /// The smallest value the row accepts.
        min: i64,
        /// The largest value the row accepts.
        max: i64,
    },
    /// A float in a range — a slider.
    Float {
        /// The smallest value the row accepts.
        min: f32,
        /// The largest value the row accepts.
        max: f32,
    },
    /// A boolean — a toggle.
    Bool,
    /// A free string — a text field.
    Text,
    /// One of a fixed list of values — a dropdown.
    ///
    /// The values are the ones the **config file** carries, so the panel
    /// writes exactly what the file expects.
    Choice(&'static [&'static str]),
    /// An action, not a value — a button.
    ///
    /// The payload is the verb the value cell shows (`RANDOMISE`,
    /// `RESET`). An action row has no value to read or write: activating
    /// it runs the action through the app's own runner
    /// ([`EditorState::run_settings_action_with`]) instead of the
    /// settings writer, because what it writes is a whole
    /// array-of-tables section, not one key — the PERSONALITY actions
    /// (roadmap HELP window §2) are the first of these.
    Action(&'static str),
}

impl SettingKind {
    /// The options of a choice row; empty for every other kind.
    pub fn options(&self) -> &'static [&'static str] {
        match self {
            SettingKind::Choice(options) => options,
            _ => &[],
        }
    }
}

/// One row of the settings panel: a label, a value and a unit.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SettingRow {
    /// The label the panel shows.
    pub label: &'static str,
    /// The table the key lives in; `""` is the top level.
    pub section: &'static str,
    /// The key inside the table.
    pub key: &'static str,
    /// The unit shown after the value, where it helps.
    pub unit: &'static str,
    /// The kind of value the row edits.
    pub kind: SettingKind,
}

/// The value of a settings row.
#[derive(Debug, Clone, PartialEq)]
pub enum SettingValue {
    /// An integer.
    Int(i64),
    /// A float.
    Float(f32),
    /// A boolean.
    Bool(bool),
    /// A string.
    Text(String),
    /// The index of the chosen option.
    Choice(usize),
}

impl SettingValue {
    /// The value as it is shown in the panel and the status line.
    ///
    /// The row is a parameter because a **choice** is drawn as its
    /// option's name, not its index — the index is a table detail the
    /// user never chose to see (audit 2026-10-23 §U-03). A **toggle**
    /// is drawn as `ON`/`OFF`: the old `[X]`/`[ ]` went through the
    /// block font, which carries no brackets — the echo rendered blank
    /// (audit 2026-10-23 §U-25).
    pub fn display(&self, row: &SettingRow) -> String {
        match self {
            SettingValue::Int(value) => value.to_string(),
            SettingValue::Float(value) => format_float(*value),
            SettingValue::Bool(value) => match *value {
                true => "ON".to_string(),
                false => "OFF".to_string(),
            },
            SettingValue::Text(value) => value.clone(),
            SettingValue::Choice(index) => choice_name(row, *index).unwrap_or_default().to_string(),
        }
    }
}

/// Every documented setting the settings panel offers.
///
/// The rows are a **static table**, not a reflection of the config
/// struct: a reflection would auto-expose the keys the user must not
/// touch (`cell_size` is legacy and ignored) and the ones a widget cannot
/// express (the hotkey chord is a string with a grammar, not a slider)
/// — roadmap Extension 2 §3.1.
///
/// The `cell_size` key is deliberately **absent**: it is legacy and
/// ignored, and the panel refuses to offer it.
pub const SETTING_ROWS: &[SettingRow] = &[
    SettingRow {
        label: "LOOP SECONDS",
        section: "",
        key: "loop_seconds",
        unit: "S",
        kind: SettingKind::Float {
            // The panel's own floor is above the repair's **exclusive**
            // minimum (`> 0.1`): a loop of exactly the bound would be
            // replaced by the default, so the row never offers it. The
            // ceiling is the repair's own ([`crate::config::bounds`]).
            min: 0.5,
            max: crate::config::bounds::LOOP_SECONDS_MAX,
        },
    },
    SettingRow {
        label: "FPS CAP",
        section: "",
        key: "fps_cap",
        unit: "FPS",
        kind: SettingKind::Int { min: 5, max: 120 },
    },
    SettingRow {
        label: "MAX COLS",
        section: "",
        key: "max_cols",
        unit: "COLS",
        kind: SettingKind::Int {
            min: crate::config::bounds::MAX_COLS_MIN as i64,
            max: crate::config::bounds::MAX_COLS_MAX as i64,
        },
    },
    SettingRow {
        label: "EXIT HINT",
        section: "",
        key: "exit_hint",
        unit: "",
        kind: SettingKind::Text,
    },
    SettingRow {
        label: "HINT CORNER",
        section: "",
        key: "hint_corner",
        unit: "",
        kind: SettingKind::Choice(&["bottom-left", "bottom-right"]),
    },
    SettingRow {
        label: "MONITOR",
        section: "",
        key: "monitor",
        unit: "",
        kind: SettingKind::Choice(&["cursor", "primary"]),
    },
    SettingRow {
        label: "BALLOON MAX",
        section: "balloons",
        key: "max",
        unit: "",
        kind: SettingKind::Int {
            min: crate::config::bounds::BALLOONS_MAX_MIN as i64,
            max: crate::config::bounds::BALLOONS_MAX_MAX as i64,
        },
    },
    SettingRow {
        label: "BALLOON RADIUS",
        section: "balloons",
        key: "balloon_radius_rows",
        unit: "ROWS",
        kind: SettingKind::Int {
            min: 4,
            max: crate::config::bounds::BALLOON_RADIUS_ROWS_MAX as i64,
        },
    },
    SettingRow {
        label: "ROPE MIN SMALL",
        section: "balloons",
        key: "rope_min_small",
        unit: "ROWS",
        kind: SettingKind::Int {
            min: crate::config::bounds::ROPE_MIN_SMALL_MIN as i64,
            max: crate::config::bounds::ROPE_MIN_SMALL_MAX as i64,
        },
    },
    SettingRow {
        label: "ROPE MIN BIG",
        section: "balloons",
        key: "rope_min_big",
        unit: "ROWS",
        kind: SettingKind::Int {
            // The **relative** floor (`rope_min_big` at least
            // `rope_min_small`) is a relation between two values, so it
            // cannot be a constant: the cross-row clamp applies it
            // against the config's current value (see
            // [`clamp_setting`]).
            min: crate::config::bounds::ROPE_MIN_SMALL_MIN as i64,
            max: crate::config::bounds::ROPE_MIN_BIG_MAX as i64,
        },
    },
    SettingRow {
        label: "ROPE MAX",
        section: "balloons",
        key: "rope_max",
        unit: "ROWS",
        kind: SettingKind::Int {
            // The **relative** floor (`rope_max` at least `rope_min_big`)
            // is applied by the cross-row clamp.
            min: crate::config::bounds::ROPE_MIN_SMALL_MIN as i64,
            max: crate::config::bounds::ROPE_MAX_MAX as i64,
        },
    },
    SettingRow {
        label: "PENDULUM G",
        section: "balloons",
        key: "pendulum_g",
        unit: "M/S2",
        kind: SettingKind::Float {
            min: crate::config::bounds::PENDULUM_G_MIN,
            max: 40.0,
        },
    },
    SettingRow {
        label: "PENDULUM DAMP",
        section: "balloons",
        key: "pendulum_damp",
        unit: "",
        kind: SettingKind::Float { min: 0.0, max: 2.0 },
    },
    SettingRow {
        label: "PENDULUM DRIVE",
        section: "balloons",
        key: "pendulum_drive",
        unit: "",
        kind: SettingKind::Float {
            min: crate::config::bounds::PENDULUM_DRIVE_MIN,
            max: crate::config::bounds::PENDULUM_DRIVE_MAX,
        },
    },
    SettingRow {
        label: "PENDULUM PERIOD",
        section: "balloons",
        key: "pendulum_period",
        unit: "S",
        kind: SettingKind::Float {
            min: 0.1,
            max: 10.0,
        },
    },
    SettingRow {
        label: "MESSAGE",
        section: "message",
        key: "text",
        unit: "",
        kind: SettingKind::Text,
    },
    SettingRow {
        label: "MESSAGE POS",
        section: "message",
        key: "position",
        unit: "",
        kind: SettingKind::Choice(&["above_grass", "bottom_left", "bottom_right"]),
    },
    SettingRow {
        label: "MESSAGE FADE",
        section: "message",
        key: "fade_seconds",
        unit: "S",
        kind: SettingKind::Float {
            min: 0.0,
            max: 120.0,
        },
    },
    SettingRow {
        label: "SKY",
        section: "sky",
        key: "enabled",
        unit: "",
        kind: SettingKind::Bool,
    },
    SettingRow {
        label: "SKY TIME",
        section: "sky",
        key: "time_of_day",
        unit: "DAY",
        kind: SettingKind::Float { min: 0.0, max: 1.0 },
    },
    SettingRow {
        label: "WEATHER",
        section: "weather",
        key: "mode",
        unit: "",
        kind: SettingKind::Choice(&["off", "auto", "rain", "snow", "wind", "lightning"]),
    },
    SettingRow {
        label: "WEATHER INT",
        section: "weather",
        key: "intensity",
        unit: "",
        kind: SettingKind::Float { min: 0.0, max: 2.0 },
    },
    SettingRow {
        label: "WEATHER SEED",
        section: "weather",
        key: "seed",
        unit: "",
        kind: SettingKind::Int { min: 0, max: 9999 },
    },
    SettingRow {
        label: "GRASS TILE",
        section: "grass",
        key: "tile",
        unit: "",
        kind: SettingKind::Text,
    },
    SettingRow {
        label: "SHOW DAYS",
        section: "stats",
        key: "show_days",
        unit: "",
        kind: SettingKind::Bool,
    },
    SettingRow {
        label: "RESTORE BROWSERS",
        section: "browsers",
        key: "restore_on_close",
        unit: "",
        kind: SettingKind::Bool,
    },
    SettingRow {
        label: "DECORATION BIG",
        section: "decorations",
        key: "big",
        unit: "",
        kind: SettingKind::Text,
    },
    SettingRow {
        label: "DECORATION SMALL",
        section: "decorations",
        key: "small",
        unit: "",
        kind: SettingKind::Text,
    },
    SettingRow {
        label: "AUDIO VOLUME",
        section: "audio",
        key: "volume",
        unit: "",
        kind: SettingKind::Float { min: 0.0, max: 1.0 },
    },
    SettingRow {
        label: "AUDIO MUTE",
        section: "audio",
        key: "muted",
        unit: "",
        kind: SettingKind::Bool,
    },
    SettingRow {
        label: "AUDIO VOICE",
        section: "audio",
        key: "waveform",
        unit: "",
        kind: SettingKind::Choice(&["sine", "triangle", "square"]),
    },
    SettingRow {
        label: "HOTKEY WINDOWS",
        section: "hotkey",
        key: "windows",
        unit: "",
        kind: SettingKind::Text,
    },
    SettingRow {
        label: "HOTKEY MACOS",
        section: "hotkey",
        key: "macos",
        unit: "",
        kind: SettingKind::Text,
    },
    SettingRow {
        label: "HOTKEY LINUX",
        section: "hotkey",
        key: "linux",
        unit: "",
        kind: SettingKind::Text,
    },
    SettingRow {
        label: "EDITOR KEY",
        section: "hotkey",
        key: "editor",
        unit: "",
        kind: SettingKind::Text,
    },
    // The PERSONALITY actions (roadmap HELP window §2, decision F-161):
    // two buttons, no picker — the full sub-panel stays the honest
    // roadmap amendment to ROADMAP_EXTENSION2.md §2.5. An action row
    // writes a whole `[[personality]]` section, not one key, so its
    // work goes through the app's action runner, never the settings
    // writer.
    SettingRow {
        label: "PERSONALITY",
        section: "personality",
        key: "randomise",
        unit: "",
        kind: SettingKind::Action("RANDOMISE"),
    },
    SettingRow {
        label: "PERSONALITY",
        section: "personality",
        key: "reset",
        unit: "",
        kind: SettingKind::Action("RESET"),
    },
];

/// The value of one setting in the config.
///
/// `None` when the key is not one the panel offers — a row the table
/// lists but the config does not carry would be a bug, and the test
/// `the_settings_rows_are_data` catches it.
pub fn read_setting(cfg: &crate::config::Config, row: &SettingRow) -> Option<SettingValue> {
    let value = match row.key {
        "loop_seconds" => SettingValue::Float(cfg.loop_seconds),
        "fps_cap" => SettingValue::Int(i64::from(cfg.fps_cap)),
        "max_cols" => SettingValue::Int(cfg.max_cols as i64),
        "exit_hint" => SettingValue::Text(cfg.exit_hint.clone()),
        "hint_corner" => SettingValue::Choice(choice_index(
            row,
            match cfg.hint_corner {
                crate::config::HintCorner::BottomLeft => "bottom-left",
                crate::config::HintCorner::BottomRight => "bottom-right",
            },
        )),
        "monitor" => SettingValue::Choice(choice_index(
            row,
            match cfg.monitor {
                crate::config::MonitorChoice::Cursor => "cursor",
                crate::config::MonitorChoice::Primary => "primary",
            },
        )),
        "max" => SettingValue::Int(cfg.balloons.max as i64),
        "balloon_radius_rows" => SettingValue::Int(i64::from(cfg.balloons.balloon_radius_rows)),
        "rope_min_small" => SettingValue::Int(i64::from(cfg.balloons.rope_min_small)),
        "rope_min_big" => SettingValue::Int(i64::from(cfg.balloons.rope_min_big)),
        "rope_max" => SettingValue::Int(i64::from(cfg.balloons.rope_max)),
        "pendulum_g" => SettingValue::Float(cfg.balloons.pendulum_g),
        "pendulum_damp" => SettingValue::Float(cfg.balloons.pendulum_damp),
        "pendulum_drive" => SettingValue::Float(cfg.balloons.pendulum_drive),
        "pendulum_period" => SettingValue::Float(cfg.balloons.pendulum_period),
        "text" => SettingValue::Text(cfg.message.text.clone()),
        "position" => SettingValue::Choice(choice_index(
            row,
            match cfg.message.position {
                crate::config::MessagePosition::AboveGrass => "above_grass",
                crate::config::MessagePosition::BottomLeft => "bottom_left",
                crate::config::MessagePosition::BottomRight => "bottom_right",
            },
        )),
        "fade_seconds" => SettingValue::Float(cfg.message.fade_seconds),
        "enabled" => SettingValue::Bool(cfg.sky.enabled),
        "time_of_day" => SettingValue::Float(cfg.sky.time_of_day),
        "mode" => SettingValue::Choice(choice_index(row, cfg.weather.mode.as_str())),
        "intensity" => SettingValue::Float(cfg.weather.intensity),
        "seed" => SettingValue::Int(cfg.weather.seed as i64),
        "tile" => SettingValue::Text(cfg.grass.tile.clone().unwrap_or_default()),
        "show_days" => SettingValue::Bool(cfg.stats.show_days),
        "restore_on_close" => SettingValue::Bool(cfg.browsers.restore_on_close),
        "windows" => SettingValue::Text(cfg.hotkey.windows.clone()),
        "macos" => SettingValue::Text(cfg.hotkey.macos.clone()),
        "linux" => SettingValue::Text(cfg.hotkey.linux.clone()),
        "editor" => SettingValue::Text(cfg.hotkey.editor.clone()),
        "big" => SettingValue::Text(cfg.decorations.big.clone().unwrap_or_default()),
        "small" => SettingValue::Text(cfg.decorations.small.clone().unwrap_or_default()),
        "volume" => SettingValue::Float(cfg.audio.volume),
        "muted" => SettingValue::Bool(cfg.audio.muted),
        "waveform" => SettingValue::Choice(choice_index(row, &cfg.audio.waveform)),
        _ => return None,
    };
    Some(value)
}

/// Write one setting into the config.
///
/// The value is written into the **in-memory** copy only; the file is
/// written separately, and a file that cannot be written leaves the
/// in-memory copy alone (roadmap Extension 2 §3.2).
pub fn apply_setting(cfg: &mut crate::config::Config, row: &SettingRow, value: &SettingValue) {
    match (row.key, value) {
        ("loop_seconds", SettingValue::Float(v)) => cfg.loop_seconds = *v,
        ("fps_cap", SettingValue::Int(v)) => cfg.fps_cap = *v as u32,
        ("max_cols", SettingValue::Int(v)) => cfg.max_cols = *v as usize,
        ("exit_hint", SettingValue::Text(v)) => cfg.exit_hint = v.clone(),
        ("hint_corner", SettingValue::Choice(index)) => {
            if let Some(corner) = choice_value::<crate::config::HintCorner>(row, *index) {
                cfg.hint_corner = corner;
            }
        }
        ("monitor", SettingValue::Choice(index)) => {
            if let Some(monitor) = choice_value::<crate::config::MonitorChoice>(row, *index) {
                cfg.monitor = monitor;
            }
        }
        ("max", SettingValue::Int(v)) => cfg.balloons.max = *v as usize,
        ("balloon_radius_rows", SettingValue::Int(v)) => {
            cfg.balloons.balloon_radius_rows = *v as u32
        }
        ("rope_min_small", SettingValue::Int(v)) => cfg.balloons.rope_min_small = *v as u32,
        ("rope_min_big", SettingValue::Int(v)) => cfg.balloons.rope_min_big = *v as u32,
        ("rope_max", SettingValue::Int(v)) => cfg.balloons.rope_max = *v as u32,
        ("pendulum_g", SettingValue::Float(v)) => cfg.balloons.pendulum_g = *v,
        ("pendulum_damp", SettingValue::Float(v)) => cfg.balloons.pendulum_damp = *v,
        ("pendulum_drive", SettingValue::Float(v)) => cfg.balloons.pendulum_drive = *v,
        ("pendulum_period", SettingValue::Float(v)) => cfg.balloons.pendulum_period = *v,
        ("text", SettingValue::Text(v)) => cfg.message.text = v.clone(),
        ("position", SettingValue::Choice(index)) => {
            if let Some(position) = choice_value::<crate::config::MessagePosition>(row, *index) {
                cfg.message.position = position;
            }
        }
        ("fade_seconds", SettingValue::Float(v)) => cfg.message.fade_seconds = *v,
        ("enabled", SettingValue::Bool(v)) => cfg.sky.enabled = *v,
        ("time_of_day", SettingValue::Float(v)) => cfg.sky.time_of_day = *v,
        ("mode", SettingValue::Choice(index)) => {
            if let Some(mode) = choice_value::<crate::config::WeatherMode>(row, *index) {
                cfg.weather.mode = mode;
            }
        }
        ("intensity", SettingValue::Float(v)) => cfg.weather.intensity = *v,
        ("seed", SettingValue::Int(v)) => cfg.weather.seed = *v as u64,
        ("tile", SettingValue::Text(v)) => {
            let trimmed = v.trim();
            cfg.grass.tile = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        ("show_days", SettingValue::Bool(v)) => cfg.stats.show_days = *v,
        ("restore_on_close", SettingValue::Bool(v)) => cfg.browsers.restore_on_close = *v,
        ("windows", SettingValue::Text(v)) => cfg.hotkey.windows = v.clone(),
        ("macos", SettingValue::Text(v)) => cfg.hotkey.macos = v.clone(),
        ("linux", SettingValue::Text(v)) => cfg.hotkey.linux = v.clone(),
        ("editor", SettingValue::Text(v)) => cfg.hotkey.editor = v.clone(),
        ("big", SettingValue::Text(v)) => {
            let trimmed = v.trim();
            cfg.decorations.big = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        ("small", SettingValue::Text(v)) => {
            let trimmed = v.trim();
            cfg.decorations.small = if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            };
        }
        ("volume", SettingValue::Float(v)) => cfg.audio.volume = *v,
        ("muted", SettingValue::Bool(v)) => cfg.audio.muted = *v,
        ("waveform", SettingValue::Choice(index)) => {
            if let Some(name) = choice_name(row, *index) {
                cfg.audio.waveform = name.to_string();
            }
        }
        _ => {}
    }
}

/// The index of a choice's value in the row's option list.
///
/// `0` when the value is not in the list: a config that carries an
/// unknown value must still show **something** in the panel.
fn choice_index(row: &SettingRow, name: &str) -> usize {
    row.kind
        .options()
        .iter()
        .position(|option| *option == name)
        .unwrap_or(0)
}

/// The name of a choice's option, if the index is one.
fn choice_name(row: &SettingRow, index: usize) -> Option<&'static str> {
    row.kind.options().get(index).copied()
}

/// The enum a choice names, if the choice names one.
///
/// The option strings are the enums' serde names (`bottom-left`,
/// `cursor`, `above_grass`, `lightning`), so the option is parsed as a
/// one-key TOML document — a **bare** value is not a TOML document, and
/// a quoted option alone fails with "key with no value"
/// (audit 2026-10-23 §R6-02).
///
/// The helper exists to keep [`apply_setting`] **flat**: the choice
/// arms used to nest an `if let` inside an `if let` inside the match
/// arm, which the nesting rule forbids (audit 2026-10-22 §P1-10).
fn choice_value<T: serde::de::DeserializeOwned>(row: &SettingRow, index: usize) -> Option<T> {
    #[derive(serde::Deserialize)]
    struct Wrapped<T> {
        value: T,
    }
    let name = choice_name(row, index)?;
    let document = format!("value = {name:?}");
    toml::from_str::<Wrapped<T>>(&document)
        .ok()
        .map(|w| w.value)
}

/// The open choice row after an activate on `row`: open when it was
/// shut, shut when it was open. The keyboard's Enter/Space is both the
/// way in and the way out of the list (audit 2026-10-23 §X-01).
fn toggle_open_row(open: Option<usize>, row: usize) -> Option<usize> {
    match open {
        Some(open_row) if open_row == row => None,
        _ => Some(row),
    }
}

/// The toast a finished action reads, from the row's key.
///
/// The known actions read as the past tense of what the user pressed;
/// an action added later without its line here still answers honestly,
/// just plainly.
fn action_toast(key: &str) -> String {
    match key {
        "randomise" => "PERSONALITY RANDOMISED".to_string(),
        "reset" => "PERSONALITY RESET".to_string(),
        other => format!("{other} DONE"),
    }
}

/// The TOML value of a setting, as it is written into the file.
pub fn render_setting_value(row: &SettingRow, value: &SettingValue) -> String {
    match value {
        SettingValue::Int(number) => number.to_string(),
        SettingValue::Float(number) => format_float(*number),
        SettingValue::Bool(flag) => flag.to_string(),
        SettingValue::Text(text) => toml::Value::String(text.clone()).to_string(),
        SettingValue::Choice(index) => {
            let name = choice_name(row, *index).unwrap_or_default();
            toml::Value::String(name.to_string()).to_string()
        }
    }
}

/// A float as TOML wants it: with a decimal point.
///
/// `format!("{}", 20.0)` is `20`, and a TOML integer does not parse
/// into an `f32` — so the value would be refused by the config's own
/// parser.
pub fn format_float(value: f32) -> String {
    if !value.is_finite() {
        return "0.0".to_string();
    }
    let text = format!("{value}");
    if text.contains('.') || text.contains('e') || text.contains("inf") {
        return text;
    }
    format!("{text}.0")
}

/// Clamp a value into the range its row accepts.
///
/// The slider's range **is** the validation, so an out-of-range value
/// is not reachable by clicking (roadmap Extension 2 §3.2).
///
/// WHY the config is a parameter: three of the bounds are **relative**
/// — `rope_min_big` at least `rope_min_small`, `rope_max` at least
/// `rope_min_big` — and a relation between two values of the same
/// config cannot be a constant. The clamp reads the **current** value of
/// the related row, so the value the panel writes always survives the
/// repair pass (audit 2026-10-21 §F7).
///
/// The rows with a relative bound, and what the clamp does:
/// * `rope_min_small` — at most the current `rope_min_big` (raising it
///   above would leave `rope_min_big` invalid, and the repair pass
///   would rewrite *that* row behind the user's back);
/// * `rope_min_big` — at least the current `rope_min_small`;
/// * `rope_max` — at least the current `rope_min_big`.
///
/// Every other row is bounded by constants alone, and the config is not
/// read for it.
pub fn clamp_setting(
    row: &SettingRow,
    value: SettingValue,
    config: &crate::config::Config,
) -> SettingValue {
    match (row.kind, value) {
        (SettingKind::Int { min, max }, SettingValue::Int(number)) => {
            let (min, max) = effective_int_range(row, config).unwrap_or((min, max));
            SettingValue::Int(number.clamp(min, max))
        }
        (SettingKind::Float { min, max }, SettingValue::Float(number)) => {
            SettingValue::Float(number.clamp(min, max))
        }
        (SettingKind::Bool, SettingValue::Bool(flag)) => SettingValue::Bool(flag),
        (SettingKind::Text, SettingValue::Text(text)) => SettingValue::Text(text),
        (SettingKind::Choice(_), SettingValue::Choice(index)) => {
            let max = row.kind.options().len().saturating_sub(1);
            SettingValue::Choice(index.min(max))
        }
        (_, value) => value,
    }
}

/// The **effective** integer range of a row: its own bounds, tightened by
/// the relative bounds of the repair pass.
///
/// `None` for a row that is not an integer row.
///
/// The bounds are ordered (`min <= max`) in every case: a config whose
/// own values are already out of order must not make the clamp panic
/// (`i64::clamp` panics when `min > max`).
pub fn effective_int_range(row: &SettingRow, config: &crate::config::Config) -> Option<(i64, i64)> {
    let (min, max) = match row.kind {
        SettingKind::Int { min, max } => (min, max),
        _ => return None,
    };
    let (min, max) = match row.key {
        "rope_min_small" => (min, max.min(config.balloons.rope_min_big as i64).max(min)),
        "rope_min_big" => (min.max(config.balloons.rope_min_small as i64).min(max), max),
        "rope_max" => (min.max(config.balloons.rope_min_big as i64).min(max), max),
        _ => (min, max),
    };
    Some((min.min(max), max))
}

/// The sixteen assignment slots, in the user's order: the two big ducks,
/// the nine small ducks and the five balloons (roadmap §3.8, decision
/// D11).
///
/// The counts come from the parade's own tables
/// ([`crate::config::SlotKind::count`]), so a slot can never exist that
/// the parade does not draw.
pub fn assignment_slots() -> Vec<crate::config::AssignmentSlot> {
    let kinds = [
        crate::config::SlotKind::Big,
        crate::config::SlotKind::Small,
        crate::config::SlotKind::Balloon,
    ];
    kinds
        .iter()
        .flat_map(|kind| {
            (1..=kind.count()).map(move |number| crate::config::AssignmentSlot {
                kind: *kind,
                number,
            })
        })
        .collect()
}

/// The label of a slot: `BIG 1`, `SMALL 3`, `BALLOON 5`.
pub fn slot_label(slot: &crate::config::AssignmentSlot) -> String {
    format!("{} {}", slot_kind_label(slot.kind), slot.number)
}

/// The first free asset name of `kind`, without the platform.
///
/// The core cannot list the platform's asset folder, so the **fallback**
/// is the kind's own prefix: `sprite-1`, `balloon-1`, `grass-1`,
/// `decoration-1`. The application half replaces it with the platform's
/// real free name ([`EditorState::set_platform`]).
pub fn default_asset_name(kind: crate::drawn::DrawnKind) -> String {
    format!("{}-1", kind.prefix())
}

/// Whether a document of that name already exists.
///
/// The lister is the platform's (see [`EditorPlatform`]); without one
/// nothing exists, so the save goes ahead.
fn asset_exists(platform: EditorPlatform, kind: crate::drawn::DrawnKind, name: &str) -> bool {
    match platform.list_assets {
        Some(list) => list(kind).iter().any(|taken| taken.name == name),
        None => false,
    }
}

/// The save of a document, through the platform's writer.
///
/// Without a writer there is nowhere to save: the error says so.
fn save_asset_document(
    platform: EditorPlatform,
    sprite: &crate::drawn::DrawnSprite,
    kind: crate::drawn::DrawnKind,
    name: &str,
) -> Result<PathBuf, crate::drawn::DrawnError> {
    match platform.save_asset {
        Some(save) => save(sprite, kind, name),
        None => Err(crate::drawn::DrawnError::Io {
            path: PathBuf::new(),
            source: std::io::Error::other("no asset folder was injected"),
        }),
    }
}

/// The label of a slot kind: `BIG`, `SMALL` or `BALLOON`.
fn slot_kind_label(kind: crate::config::SlotKind) -> &'static str {
    match kind {
        crate::config::SlotKind::Big => "BIG",
        crate::config::SlotKind::Small => "SMALL",
        crate::config::SlotKind::Balloon => "BALLOON",
    }
}

/// An empty asset name means "no override" — the same rule the config's
/// own sanitizing loader applies to the assignments.
fn none_if_empty(name: Option<String>) -> Option<String> {
    match name {
        Some(text) if text.trim().is_empty() => None,
        other => other,
    }
}

/// The kind of drawn asset a slot takes: sprites for the ducks, balloons
/// for the balloons.
fn slot_drawn_kind(kind: crate::config::SlotKind) -> crate::drawn::DrawnKind {
    match kind {
        crate::config::SlotKind::Balloon => crate::drawn::DrawnKind::Balloon,
        _ => crate::drawn::DrawnKind::Sprite,
    }
}

/// The duck size a slot takes: the big ducks take the big art, the small
/// ones the small art, and a balloon slot no size at all.
///
/// WHY it matters: the slot's box is the built-in duck's frame, so art
/// of the wrong size is drawn in the corner of a box that does not fit
/// it (audit 2026-10-21 §E1).
fn slot_drawn_size(kind: crate::config::SlotKind) -> Option<crate::drawn::DrawnSize> {
    match kind {
        crate::config::SlotKind::Big => Some(crate::drawn::DrawnSize::Big),
        crate::config::SlotKind::Small => Some(crate::drawn::DrawnSize::Small),
        crate::config::SlotKind::Balloon => None,
    }
}

/// The new scroll offset of a list of `len` rows that shows `visible`
/// rows, moved by `delta`.
fn scroll_offset(current: usize, delta: i32, len: usize, visible: usize) -> usize {
    let max = len.saturating_sub(visible) as i64;
    let next = i64::try_from(current).unwrap_or(i64::MAX) + i64::from(delta);
    next.clamp(0, max) as usize
}

/// The track files that exist in `dir`.
///
/// The folder is **injected** (see [`EditorState::set_tracks_dir`]): the
/// core cannot resolve a platform location, so it only ever lists a
/// folder it was handed. The folder is read **once**, when the strip
/// mode opens: a directory scan must never happen per frame (AGENTS.md:
/// never block the UI thread). A missing folder yields no tracks, so the
/// editor still runs.
fn load_track_list(dir: Option<&std::path::Path>) -> Vec<String> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    crate::audio::sequencer::list_tracks(dir)
}

/// True when `c` may appear in an asset name (`a-z`, `0-9`, `-`, `_`).
fn is_legal_name_char(c: char) -> bool {
    c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_'
}

/// True when the toast is still on screen at the moment `now`.
///
/// The expiry is a pure function of the toast and the current time, so
/// the 2.5 s lifetime is testable without a clock inside the state
/// (roadmap §5.2).
pub fn toast_visible(toast: &Option<(String, Instant)>, now: Instant) -> bool {
    let Some((_, raised)) = toast.as_ref() else {
        return false;
    };
    now.saturating_duration_since(*raised) < Duration::from_millis(TOAST_MILLIS)
}

/// The ghost colour: the frame colour at 20 % (roadmap §5.6).
///
/// The ghost is the previous frame, so the user sees the pose they just
/// drew as a faint underlay of the pose they are drawing now.
pub fn dim_color(color: [u8; 3]) -> [u8; 3] {
    [
        dim_channel(color[0]),
        dim_channel(color[1]),
        dim_channel(color[2]),
    ]
}

/// One colour channel at 20 %.
fn dim_channel(channel: u8) -> u8 {
    (channel as f32 * 0.2).round() as u8
}

/// The status line's name of the selected tool.
///
/// The block font has no lowercase, so the glyph is folded to upper
/// case — a tool the font cannot draw simply does not appear in the
/// status line (the panel shows the real glyph).
fn tool_label(tool: usize) -> String {
    crate::drawn::TOOLS
        .get(tool)
        .map(|glyph| glyph.to_ascii_uppercase().to_string())
        .unwrap_or_default()
}

/// The status line's name of the selected colour.
fn colour_label(color: usize) -> &'static str {
    crate::drawn::PALETTE_NAMES
        .get(color)
        .copied()
        .unwrap_or(crate::drawn::PALETTE_NAMES[0])
}

/// One row of the side panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelEntry {
    /// A tool glyph.
    Tool(usize),
    /// The separator between two sections of the panel.
    Separator,
    /// A colour swatch.
    Color(usize),
    /// A pitch of the note palette.
    Pitch(usize),
    /// A track file that exists.
    Track(usize),
}

impl PanelEntry {
    /// True when the row carries the `>` marker.
    fn is_selected(&self, state: &EditorState) -> bool {
        match self {
            PanelEntry::Tool(index) => state.tool == *index,
            PanelEntry::Color(index) => state.color == *index,
            PanelEntry::Pitch(index) => state.pitch == *index,
            PanelEntry::Track(index) => state
                .tracks
                .get(*index)
                .is_some_and(|name| *name == state.track_name),
            PanelEntry::Separator => false,
        }
    }
}

/// The rows of the side panel.
///
/// The panel is **mode-dependent**: the drawing mode shows the 19 tools,
/// a separator and the 8 colours (roadmap §5.5), the strip mode shows the
/// twelve pitches, a separator and the track files that exist (roadmap
/// Extension 3 §3.2).
fn panel_entries(state: &EditorState) -> Vec<PanelEntry> {
    if state.mode == EditorMode::Sequencer {
        return strip_panel_entries(state);
    }
    let tools: Vec<PanelEntry> = (0..crate::drawn::TOOLS.len())
        .map(PanelEntry::Tool)
        .collect();
    let colors: Vec<PanelEntry> = (0..crate::drawn::PALETTE.len())
        .map(PanelEntry::Color)
        .collect();
    let mut entries = tools;
    entries.push(PanelEntry::Separator);
    entries.extend(colors);
    entries
}

/// The rows of the strip panel: the twelve pitches, a separator and the
/// track files that exist.
fn strip_panel_entries(state: &EditorState) -> Vec<PanelEntry> {
    let pitches: Vec<PanelEntry> = (0..PITCH_NAMES.len()).map(PanelEntry::Pitch).collect();
    let tracks: Vec<PanelEntry> = (0..state.tracks.len()).map(PanelEntry::Track).collect();
    let mut entries = pitches;
    entries.push(PanelEntry::Separator);
    entries.extend(tracks);
    entries
}

/// The width of the side panel in base cells, for the mode.
///
/// The pitch palette needs a **label** per row (`C#3`), not a glyph, so
/// the panel is wider in strip mode (roadmap Extension 3 §3.2).
fn panel_cells(mode: EditorMode) -> u32 {
    match mode {
        EditorMode::Sequencer => PITCH_PANEL_CELLS,
        _ => TOOL_PANEL_CELLS,
    }
}

// ---------------------------------------------------------------------------
// Hit testing (roadmap Stage 6)
// ---------------------------------------------------------------------------

/// What a click at a pixel lands on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorHit {
    /// A dropdown box (0 = kind, 1 = size, 2 = frames).
    Dropdown(usize),
    /// An option of the open dropdown's list.
    DropdownOption { dropdown: usize, option: usize },
    /// A tool glyph in the tool panel.
    Tool(usize),
    /// A colour swatch in the colour panel.
    Color(usize),
    /// A paintable cell of the canvas.
    Canvas { col: usize, row: usize },
    /// The name field.
    NameField,
    /// The primary (SAVE) button.
    PrimaryButton,
    /// The ASSIGN button of the top bar (roadmap Stage 9).
    AssignButton,
    /// The DRAW button of the top bar (roadmap Stage 9).
    DrawButton,
    /// A slot of the slot list — an index into the sixteen slots.
    Slot(usize),
    /// An asset of the asset list — an index into the selected slot's
    /// asset list.
    Asset(usize),
    /// The CLEAR button of the assign panel.
    ClearButton,
    /// The SETTINGS button of the top bar (roadmap Extension 2 §3.1).
    SettingsButton,
    /// A slider of the settings panel — the index of the row.
    SettingSlider(usize),
    /// A toggle of the settings panel — the index of the row.
    SettingToggle(usize),
    /// A text field of the settings panel — the index of the row.
    SettingText(usize),
    /// A dropdown of the settings panel — the index of the row.
    SettingChoice(usize),
    /// An action button of the settings panel — the index of the row
    /// (the PERSONALITY actions, roadmap HELP window §2).
    SettingAction(usize),
    /// An option of an open settings dropdown.
    SettingOption {
        /// The row whose list is open.
        row: usize,
        /// The option in the list.
        option: usize,
    },
    /// The TRACK button of the top bar (roadmap Extension 3 §3.2).
    TrackButton,
    /// A pitch of the note palette — an index into [`PITCH_NAMES`].
    Pitch(usize),
    /// A cell of the note lane — the step it stands for.
    NoteCell(usize),
    /// A track file of the strip panel — an index into the track list.
    Track(usize),
    /// Nothing.
    None,
}

/// What is at the pixel `(x, y)` (roadmap Stage 6).
///
/// The function is **pure**: it reads the layout and the state and
/// returns the hit, so the whole pointer behaviour is testable without a
/// display.
///
/// The rules it implements:
///
/// * the two **mode buttons** of the top bar are hit first — they are
///   the way in and out of assign mode (roadmap Stage 9);
/// * in **assign mode** the canvas area carries the two lists, so the
///   hit is a slot, an asset or the CLEAR button — the dropdown boxes
///   are inert there;
/// * an open dropdown's **option list** is hit first — it is drawn in the
///   overlay layer, so it lies on top of the canvas;
/// * a dropdown box opens its list, but an **inactive** box (dropdowns 2
///   and 3 while a balloon is selected) changes nothing — the state
///   refuses to open it;
/// * the tool/colour panel maps a row to a tool or a colour;
/// * the canvas maps a pixel to a cell, **minus the scroll offset**, and
///   only an **active** cell is a hit: a cell outside the canvas or inside
///   the inactive region is [`EditorHit::None`];
/// * in **strip mode** the canvas is the note strip: the lane row is a
///   note cell and the ruler row is not interactive (roadmap Extension 3
///   §3.2).
pub fn hit_test(layout: &EditorLayout, state: &EditorState, x: i64, y: i64) -> EditorHit {
    if layout.assign_button.contains(x, y) {
        return EditorHit::AssignButton;
    }
    if layout.draw_button.contains(x, y) {
        return EditorHit::DrawButton;
    }
    if layout.settings_button.contains(x, y) {
        return EditorHit::SettingsButton;
    }
    if layout.track_button.contains(x, y) {
        return EditorHit::TrackButton;
    }
    if state.mode == EditorMode::Assign {
        return assign_hit(layout, state, x, y);
    }
    if state.mode == EditorMode::Settings {
        return settings_hit(layout, state, x, y);
    }
    if state.mode == EditorMode::Sequencer {
        return sequencer_hit(layout, state, x, y);
    }
    if let Some(hit) = dropdown_option_hit(layout, state, x, y) {
        return hit;
    }
    if let Some(hit) = dropdown_box_hit(layout, x, y) {
        return hit;
    }
    if layout.primary_button.contains(x, y) {
        return EditorHit::PrimaryButton;
    }
    if layout.name_field.contains(x, y) {
        return EditorHit::NameField;
    }
    if let Some(hit) = panel_hit(layout, state, x, y) {
        return hit;
    }
    canvas_hit(layout, state, x, y)
}

/// The hit in the strip mode: the SAVE button, the name field, the
/// pitch palette, the track list and the note lane.
///
/// The dropdowns are inert here: the kind, the size and the frame count
/// belong to the drawing, and the strip is not a drawing.
fn sequencer_hit(layout: &EditorLayout, state: &EditorState, x: i64, y: i64) -> EditorHit {
    if layout.primary_button.contains(x, y) {
        return EditorHit::PrimaryButton;
    }
    if layout.name_field.contains(x, y) {
        return EditorHit::NameField;
    }
    if let Some(hit) = panel_hit(layout, state, x, y) {
        return hit;
    }
    strip_hit(layout, state, x, y)
}

/// The note lane cell under the pixel, if any.
///
/// The scroll offset is added back, so a click on a cell that is visible
/// lands on that **step** of the strip. The caption row (0) and the
/// ruler row (2) are not hits: the caption is chrome and the ruler only
/// shows where the loop starts and ends (audit 2026-10-23 §U-16) — the
/// lane at row 1 is the only row that takes clicks.
fn strip_hit(layout: &EditorLayout, state: &EditorState, x: i64, y: i64) -> EditorHit {
    if !layout.canvas.contains(x, y) {
        return EditorHit::None;
    }
    let col =
        ((x - layout.canvas.x) / i64::from(CANVAS_CELL_W)) as usize + layout.canvas_scroll_cols;
    let row = ((y - layout.canvas.y) / i64::from(CANVAS_CELL_H)) as usize;
    if row != STRIP_NOTE_ROW || col >= state.strip_steps {
        return EditorHit::None;
    }
    EditorHit::NoteCell(col)
}

/// The hit inside the assign panel: a slot, an asset or the CLEAR button.
fn assign_hit(layout: &EditorLayout, state: &EditorState, x: i64, y: i64) -> EditorHit {
    if let Some(hit) = slot_hit(layout, state, x, y) {
        return hit;
    }
    asset_hit(layout, state, x, y)
}

/// What is at the pixel in the settings panel.
///
/// The **value column** is the interactive part: a slider, a toggle, a
/// text field or a dropdown. The label and the unit are not — a click
/// on the label must not move a slider (roadmap Extension 2 §3.1).
fn settings_hit(layout: &EditorLayout, state: &EditorState, x: i64, y: i64) -> EditorHit {
    if let Some(hit) = settings_option_hit(layout, state, x, y) {
        return hit;
    }
    let Some(visible) = settings_visible_row(layout, y) else {
        return EditorHit::None;
    };
    let row = layout.settings_scroll + visible;
    let Some(setting) = SETTING_ROWS.get(row) else {
        return EditorHit::None;
    };
    if !settings_value_rect(layout.settings, visible).contains(x, y) {
        return EditorHit::None;
    }
    match setting.kind {
        SettingKind::Int { .. } | SettingKind::Float { .. } => EditorHit::SettingSlider(row),
        SettingKind::Bool => EditorHit::SettingToggle(row),
        SettingKind::Text => EditorHit::SettingText(row),
        SettingKind::Choice(_) => EditorHit::SettingChoice(row),
        SettingKind::Action(_) => EditorHit::SettingAction(row),
    }
}

/// The visible settings row the pixel `y` is on.
///
/// `None` outside the panel's rows: the header row and the area below
/// the last row are not rows.
fn settings_visible_row(layout: &EditorLayout, y: i64) -> Option<usize> {
    let top = layout.settings.y + i64::from(CANVAS_CELL_H);
    if y < top {
        return None;
    }
    let row = (y - top) / i64::from(CANVAS_CELL_H);
    if row < 0 || row >= layout.settings_visible as i64 {
        return None;
    }
    Some(row as usize)
}

/// The option of an open settings dropdown under the pixel, if any.
fn settings_option_hit(
    layout: &EditorLayout,
    state: &EditorState,
    x: i64,
    y: i64,
) -> Option<EditorHit> {
    let row = state.settings_open?;
    let options = SETTING_ROWS.get(row)?.kind.options();
    options.iter().enumerate().find_map(|(index, _)| {
        let rect = settings_option_rect(layout, state, index)?;
        if rect.contains(x, y) {
            Some(EditorHit::SettingOption { row, option: index })
        } else {
            None
        }
    })
}

/// The rect of one option of the open settings dropdown, in pixels.
///
/// The list drops **below** the row's value cell, one base row per
/// option — and it is **clamped to the panel** (audit 2026-10-23
/// §U-26): an option whose row would run past the panel's bottom edge
/// (the canvas area, which ends at the status strip) has no rect, so
/// neither the painter nor the hit test can put it on screen. A row
/// that is itself scrolled out of view has no list at all. Paint and
/// hit share this one function, so the two can never disagree.
fn settings_option_rect(layout: &EditorLayout, state: &EditorState, option: usize) -> Option<Rect> {
    let row = state.settings_open?;
    let visible = row.checked_sub(layout.settings_scroll)?;
    if visible >= layout.settings_visible {
        return None;
    }
    let value = settings_value_rect(layout.settings, visible);
    let index = i64::try_from(option).ok()?;
    let top = value.bottom() + index * i64::from(CANVAS_CELL_H);
    if top + i64::from(CANVAS_CELL_H) > layout.settings.bottom() {
        return None;
    }
    Some(Rect::new(value.x, top, value.w, CANVAS_CELL_H))
}

/// The slot (or the CLEAR button) under the pixel, if any.
fn slot_hit(layout: &EditorLayout, state: &EditorState, x: i64, y: i64) -> Option<EditorHit> {
    let rect = &layout.slot_list;
    if !rect.contains(x, y) {
        return None;
    }
    if layout.clear_button.contains(x, y) {
        return Some(EditorHit::ClearButton);
    }
    let row = ((y - rect.y) / i64::from(CANVAS_CELL_H)) as usize;
    let index = state.slot_scroll + row;
    if index >= state.targets.len() {
        return None;
    }
    Some(EditorHit::Slot(index))
}

/// The asset under the pixel, if any.
fn asset_hit(layout: &EditorLayout, state: &EditorState, x: i64, y: i64) -> EditorHit {
    let rect = &layout.asset_list;
    if !rect.contains(x, y) {
        return EditorHit::None;
    }
    let row = ((y - rect.y) / i64::from(CANVAS_CELL_H)) as usize;
    let index = state.asset_scroll + row;
    if index >= state.assign_assets().len() {
        return EditorHit::None;
    }
    EditorHit::Asset(index)
}

/// The open dropdown's option under the pixel, if any.
fn dropdown_option_hit(
    layout: &EditorLayout,
    state: &EditorState,
    x: i64,
    y: i64,
) -> Option<EditorHit> {
    let open = state.open_dropdown?;
    let options = state.dropdown_options(open);
    options.iter().enumerate().find_map(|(option, _)| {
        let rect = option_rect(layout, open, option)?;
        if rect.contains(x, y) {
            return Some(EditorHit::DropdownOption {
                dropdown: open,
                option,
            });
        }
        None
    })
}

/// The dropdown box under the pixel, if any.
fn dropdown_box_hit(layout: &EditorLayout, x: i64, y: i64) -> Option<EditorHit> {
    layout
        .dropdowns
        .iter()
        .enumerate()
        .find(|(_, rect)| rect.contains(x, y))
        .map(|(index, _)| EditorHit::Dropdown(index))
}

/// The pixel rect of the option `option` of the dropdown at `index`.
///
/// The list drops down under the box, one option every
/// [`OPTION_UI_ROWS`] UI rows (the block font is five rows tall, so the
/// options stack without overlapping).
pub fn option_rect(layout: &EditorLayout, index: usize, option: usize) -> Option<Rect> {
    let rect = layout.dropdowns.get(index)?;
    let first_row = ui_row(rect.bottom()) + 1;
    let top = first_row + option as i64 * OPTION_UI_ROWS;
    Some(Rect::new(
        rect.x,
        top * i64::from(UI_CELL_H),
        rect.w,
        (OPTION_UI_ROWS as u32) * UI_CELL_H,
    ))
}

/// The tool, the colour, the pitch or the track under the pixel, if any.
fn panel_hit(layout: &EditorLayout, state: &EditorState, x: i64, y: i64) -> Option<EditorHit> {
    if !layout.tools.contains(x, y) {
        return None;
    }
    let row = ((y - layout.tools.y) / i64::from(CANVAS_CELL_H)) as usize;
    panel_hit_for_row(state, state.panel_scroll + row)
}

/// The hit of the panel row `index` (0 = the first tool or pitch).
///
/// The panel's contents follow the mode: the drawing mode offers the
/// tools and the colours, the strip mode the pitches and the track
/// files.
fn panel_hit_for_row(state: &EditorState, index: usize) -> Option<EditorHit> {
    if state.mode == EditorMode::Sequencer {
        return strip_panel_hit(state, index);
    }
    let tools = crate::drawn::TOOLS.len();
    if index < tools {
        return Some(EditorHit::Tool(index));
    }
    if index == tools {
        return None;
    }
    let color = index - tools - 1;
    if color < crate::drawn::PALETTE.len() {
        return Some(EditorHit::Color(color));
    }
    None
}

/// The hit of a strip panel row: a pitch, the separator, or a track file.
fn strip_panel_hit(state: &EditorState, index: usize) -> Option<EditorHit> {
    let pitches = PITCH_NAMES.len();
    if index < pitches {
        return Some(EditorHit::Pitch(index));
    }
    if index == pitches {
        return None;
    }
    let track = index - pitches - 1;
    if track < state.tracks.len() {
        return Some(EditorHit::Track(track));
    }
    None
}

/// The canvas cell under the pixel, if it may be painted.
///
/// The scroll offsets are added back, so a click on the cell that is
/// visible lands on that cell of the canvas — not on the one that would
/// be there without the scroll.
fn canvas_hit(layout: &EditorLayout, state: &EditorState, x: i64, y: i64) -> EditorHit {
    if !layout.canvas.contains(x, y) {
        return EditorHit::None;
    }
    let col =
        ((x - layout.canvas.x) / i64::from(CANVAS_CELL_W)) as usize + layout.canvas_scroll_cols;
    let row =
        ((y - layout.canvas.y) / i64::from(CANVAS_CELL_H)) as usize + layout.canvas_scroll_rows;
    if col >= state.canvas_cols() {
        return EditorHit::None;
    }
    if !state.cell_is_active(col, row) {
        return EditorHit::None;
    }
    EditorHit::Canvas { col, row }
}

/// The element kind of the dropdown-1 option `option`.
///
/// The list is `BAL` / `SPR` / `GRS` (decision D15, extended by roadmap
/// Extension 1 §10.1 with the grass tile).
pub fn kind_of_option(option: usize) -> crate::drawn::DrawnKind {
    match option {
        0 => crate::drawn::DrawnKind::Balloon,
        1 => crate::drawn::DrawnKind::Sprite,
        2 => crate::drawn::DrawnKind::Grass,
        _ => crate::drawn::DrawnKind::Decoration,
    }
}

/// The canvas cropped to its **inked** bounding box.
///
/// A grass tile is tiled across the ground band, so the blank columns
/// around the drawing would repeat across the whole screen: the tile is
/// stored as the tightest rectangle that carries ink. A canvas without
/// any ink becomes a 1x1 blank tile, so a document is never empty.
pub fn crop_to_ink(canvas: &crate::balloon::AsciiSprite) -> crate::balloon::AsciiSprite {
    canvas
        .trimmed()
        .unwrap_or_else(|| crate::balloon::AsciiSprite::new(1, 1))
}

/// Which side of a stroke's cells is written back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrokeSide {
    /// The state before the stroke — what an undo restores.
    Before,
    /// The state after the stroke — what a redo restores.
    After,
}

/// Record one painted cell in the stroke.
///
/// A cell that the stroke already painted keeps its **original**
/// `before` value and only its `after` value is updated: undoing the
/// stroke must land on the cell as it was before the stroke started, not
/// on an intermediate value.
///
/// The find is **linear in the stroke's own cells**, and that bound is
/// the point (audit 2026-10-23 §R6-07): a stroke is one press-and-drag,
/// the drag spans at most one pass over the canvas, and the cells it
/// touches are a few hundred at the wildest. A stroke-local index would
/// cost a map per stroke to save a scan that is already bounded by the
/// interaction itself.
fn record_cell(
    stroke: &mut Stroke,
    col: usize,
    row: usize,
    before: (char, [u8; 3]),
    after: (char, [u8; 3]),
) {
    if let Some(cell) = stroke
        .cells
        .iter_mut()
        .find(|cell| cell.col == col && cell.row == row)
    {
        cell.after = after;
        return;
    }
    stroke.cells.push(CellDelta {
        col,
        row,
        before,
        after,
    });
}

/// Write one side of a stroke back onto the canvas.
///
/// Every cell appears **once** in a stroke (see [`record_cell`]), so an
/// undo restores the original value and a redo restores the last one.
fn apply_stroke(canvas: &mut crate::balloon::AsciiSprite, stroke: &Stroke, side: StrokeSide) {
    for cell in &stroke.cells {
        let (ch, color) = match side {
            StrokeSide::Before => cell.before,
            StrokeSide::After => cell.after,
        };
        canvas.set(cell.col, cell.row, ch, color);
    }
}

/// A runtime frame from a document frame.
fn frame_from_doc(doc: &crate::drawn::FrameDoc) -> Option<crate::balloon::AsciiSprite> {
    crate::balloon::AsciiSprite::from_cells(
        doc.cols,
        doc.rows,
        doc.chars.clone(),
        doc.colors.clone(),
    )
}

/// The sprite size of the dropdown-2 option `option`.
fn size_of_option(option: usize) -> crate::drawn::DrawnSize {
    match option {
        0 => crate::drawn::DrawnSize::Big,
        _ => crate::drawn::DrawnSize::Small,
    }
}

// ---------------------------------------------------------------------------
// Painting (roadmap Stage 5)
// ---------------------------------------------------------------------------

/// The UI cell column of a pixel column.
pub fn ui_col(px: i64) -> i64 {
    px / i64::from(UI_CELL_W)
}

/// The UI cell row of a pixel row.
pub fn ui_row(py: i64) -> i64 {
    py / i64::from(UI_CELL_H)
}

/// Paint the base layer: the canvas and the tool panel, the assign panel
/// (roadmap Stage 9), the settings panel (roadmap Extension 2 §3) or the
/// note strip (roadmap Extension 3 §3.2).
pub fn paint_base(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    if state.mode == EditorMode::Assign {
        paint_assign_panel(grid, layout, state);
        return;
    }
    if state.mode == EditorMode::Settings {
        paint_settings_panel(grid, layout, state);
        return;
    }
    if state.mode == EditorMode::Sequencer {
        paint_strip(grid, layout, state);
        paint_tool_panel(grid, layout, state);
        return;
    }
    paint_canvas(grid, layout, state);
    paint_tool_panel(grid, layout, state);
}

/// Paint the overlay layer: the dropdowns, the name field, the buttons
/// and the status line.
///
/// In **settings mode** only the mode buttons and the status line are
/// drawn: the dropdowns, the name field and the SAVE button belong to
/// the drawing, and the drawing is not what the panel edits.
///
/// In **strip mode** the name field and the SAVE button are the track's
/// name and the track's save, so they are drawn — but the dropdowns are
/// not: the kind, the size and the frame count belong to the drawing.
pub fn paint_overlay(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    if state.mode == EditorMode::Settings {
        paint_mode_buttons(grid, layout, state);
        paint_status_line(grid, layout, state);
        return;
    }
    if state.mode != EditorMode::Sequencer {
        paint_dropdowns(grid, layout, state);
    } else {
        paint_strip_caption(grid, layout);
    }
    paint_name_field(grid, layout, state);
    paint_primary_button(grid, layout);
    paint_mode_buttons(grid, layout, state);
    paint_status_line(grid, layout, state);
}

/// Paint the caption that stands in for the dropdown boxes in strip mode.
///
/// The three dropdown boxes belong to the drawing; the strip has no
/// kind, no size and no frame count, so the top bar carries the name of
/// the mode instead of an empty gap.
fn paint_strip_caption(grid: &mut crate::grid::Grid, layout: &EditorLayout) {
    let first = layout.dropdowns[0];
    let last = layout.dropdowns[2];
    let area = Rect::new(first.x, first.y, (last.right() - first.x) as u32, first.h);
    let width = crate::hud_font::text_width(STRIP_CAPTION);
    let box_width = i64::from(area.w) / i64::from(UI_CELL_W);
    let col = ui_col(area.x) + ((box_width - width) / 2).max(0);
    crate::hud_font::draw_text(grid, col, ui_row(area.y), STRIP_CAPTION, UI_MUTED);
}

/// Paint the settings panel: a header and one row per setting.
///
/// The panel lives in the **base** layer, like the assign panel: the
/// values it shows are the user's own text, and the big cell font
/// carries every character the block font does not.
fn paint_settings_panel(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    let header = Rect::new(
        layout.settings.x + i64::from(CANVAS_CELL_W),
        layout.settings.y,
        layout.settings.w,
        CANVAS_CELL_H,
    );
    draw_base_text(
        grid,
        base_col(header.x),
        base_row(header.y),
        &settings_header_text(layout),
        UI_TEXT,
    );
    for visible in 0..layout.settings_visible {
        let Some(setting) = SETTING_ROWS.get(layout.settings_scroll + visible) else {
            continue;
        };
        paint_settings_row(grid, layout, state, visible, setting);
    }
    paint_settings_options(grid, layout, state);
}

/// The panel's header: the invitation, plus the scroll position.
///
/// The list holds 35 rows and a short window shows fewer, so the header
/// says which slice is on screen (`ROWS 12-31 OF 35`) exactly when there
/// is scrolling — a list that silently continues past the window bottom
/// looks like it ends there (audit 2026-10-23 §U-13).
fn settings_header_text(layout: &EditorLayout) -> String {
    let scroll_affordance =
        if layout.settings_scroll > 0 || layout.settings_visible < SETTING_ROWS.len() {
            format!(
                "  ROWS {}-{} OF {}",
                layout.settings_scroll + 1,
                (layout.settings_scroll + layout.settings_visible).min(SETTING_ROWS.len()),
                SETTING_ROWS.len()
            )
        } else {
            String::new()
        };
    format!("SETTINGS - CLICK A VALUE TO CHANGE IT{scroll_affordance}")
}

/// The base-grid column of a pixel position.
///
/// The base grid is indexed in **cells**; the layout is in pixels.
fn base_col(px: i64) -> i64 {
    px / i64::from(CANVAS_CELL_W)
}

/// The base-grid row of a pixel position.
fn base_row(py: i64) -> i64 {
    py / i64::from(CANVAS_CELL_H)
}

/// Paint one row of the settings panel: the label, the widget and the
/// unit.
///
/// The cursor's row carries a `>` in the marker column left of the
/// label (audit 2026-10-23 §U-14) — colour alone signalled the keyboard
/// cursor, and colour is invisible to a colour-blind user and to a
/// screenshot. The value and the unit are **clamped to the panel's
/// width** (audit 2026-10-23 §U-02): the value text is truncated with a
/// trailing `-` when it would run past the panel, and the unit column
/// is dropped entirely when there is no room left for it.
fn paint_settings_row(
    grid: &mut crate::grid::Grid,
    layout: &EditorLayout,
    state: &EditorState,
    visible: usize,
    setting: &SettingRow,
) {
    let label = settings_label_rect(layout.settings, visible);
    let value = settings_value_rect(layout.settings, visible);
    let unit = settings_unit_rect(layout.settings, visible);
    let row = layout.settings_scroll + visible;
    let selected = state.settings_cursor == row;
    let label_color = if selected { UI_TEXT } else { UI_MUTED };
    let marker_col = base_col(layout.settings.x);
    if selected {
        grid.set_if_visible(marker_col, base_row(label.y), '>', UI_TEXT);
    }
    draw_base_text(
        grid,
        base_col(label.x),
        base_row(label.y),
        setting.label,
        label_color,
    );
    let panel_right = base_col(layout.settings.right());
    let text = settings_value_text(state, setting, row);
    let value_cells = (panel_right - base_col(value.x)).max(0);
    match setting.kind {
        SettingKind::Int { .. } | SettingKind::Float { .. } => {
            paint_settings_slider(grid, layout, state, visible, setting);
        }
        _ => draw_base_text(
            grid,
            base_col(value.x),
            base_row(value.y),
            fit_base_text(text.as_str(), value_cells).as_str(),
            label_color,
        ),
    }
    let unit_cells = (panel_right - base_col(unit.x)).max(0);
    if unit_cells >= setting.unit.chars().count() as i64 {
        draw_base_text(
            grid,
            base_col(unit.x),
            base_row(unit.y),
            setting.unit,
            UI_MUTED,
        );
    }
}

/// The text a settings row shows in its value cell.
fn settings_value_text(state: &EditorState, setting: &SettingRow, row: usize) -> String {
    if state.settings_focus == Some(row) && state.settings_typed {
        return format!("{}_", state.settings_text);
    }
    // An action row has no value to read: the verb *is* the display.
    if let SettingKind::Action(verb) = setting.kind {
        return verb.to_string();
    }
    read_setting(&state.settings_config, setting)
        .map_or_else(String::new, |value| value.display(setting))
}

/// Paint a slider: a track with a knob.
fn paint_settings_slider(
    grid: &mut crate::grid::Grid,
    layout: &EditorLayout,
    state: &EditorState,
    visible: usize,
    setting: &SettingRow,
) {
    let track = settings_track_rect(layout.settings, visible);
    let row = layout.settings_scroll + visible;
    let fraction = state.slider_fraction(row).unwrap_or(0.0);
    let cells = (track.w / CANVAS_CELL_W) as usize;
    let text = slider_track(fraction, cells);
    draw_base_text(
        grid,
        base_col(track.x),
        base_row(track.y),
        text.as_str(),
        UI_TEXT,
    );
    let value = settings_value_rect(layout.settings, visible);
    let shown = settings_value_text(state, setting, row);
    let text_x = value.x + SETTINGS_TRACK_CELLS * i64::from(CANVAS_CELL_W);
    let panel_right = base_col(layout.settings.right());
    let value_cells = (panel_right - base_col(text_x)).max(0);
    draw_base_text(
        grid,
        base_col(text_x),
        base_row(value.y),
        fit_base_text(shown.as_str(), value_cells).as_str(),
        UI_TEXT,
    );
}

/// The slider track of one row: a run of dashes with a knob on it.
fn slider_track(fraction: f32, cells: usize) -> String {
    if cells == 0 {
        return String::new();
    }
    let knob = ((fraction.clamp(0.0, 1.0) * (cells - 1) as f32).round() as usize).min(cells - 1);
    let mut out = String::with_capacity(cells);
    for index in 0..cells {
        out.push(if index == knob { '|' } else { '-' });
    }
    out
}

/// Paint the option list of an open settings dropdown.
fn paint_settings_options(
    grid: &mut crate::grid::Grid,
    layout: &EditorLayout,
    state: &EditorState,
) {
    let Some(row) = state.settings_open else {
        return;
    };
    let Some(setting) = SETTING_ROWS.get(row) else {
        return;
    };
    let selected = state
        .settings_value(row)
        .and_then(|value| match value {
            SettingValue::Choice(index) => Some(index),
            _ => None,
        })
        .unwrap_or(0);
    for (index, option) in setting.kind.options().iter().enumerate() {
        let Some(rect) = settings_option_rect(layout, state, index) else {
            continue;
        };
        let marker = if index == selected { '>' } else { ' ' };
        let text = format!("{marker} {option}");
        draw_base_text(
            grid,
            base_col(rect.x),
            base_row(rect.y),
            text.as_str(),
            UI_TEXT,
        );
    }
}

/// Paint the assign panel: the slot list on the left and the asset list
/// on the right (roadmap Stage 9).
///
/// The lists live in the **base** layer, so their labels are drawn with
/// the big cell font: they are as readable and as clickable as the
/// canvas cells.
fn paint_assign_panel(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    paint_slot_list(grid, layout, state);
    paint_asset_list(grid, layout, state);
}

/// Draw the targets of the assign panel's list: the sixteen parade
/// slots and the three look targets (decision F-162).
///
/// The selected target carries a `>` marker in the column left of its
/// label; the other labels are muted.
fn paint_slot_list(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    let rect = &layout.slot_list;
    let col0 = rect.x / i64::from(CANVAS_CELL_W);
    let row0 = rect.y / i64::from(CANVAS_CELL_H);
    let visible = visible_slot_rows(layout);
    state
        .targets
        .iter()
        .enumerate()
        .skip(state.slot_scroll)
        .take(visible)
        .for_each(|(index, target)| {
            let row = row0 + (index - state.slot_scroll) as i64;
            let selected = index == state.assign_target;
            let color = if selected { UI_TEXT } else { UI_MUTED };
            let label = fit_base_text(&target_label(target), SLOT_LIST_CELLS as i64 - 1);
            draw_base_text(grid, col0 + 1, row, label.as_str(), color);
            if selected {
                grid.set_if_visible(col0, row, '>', UI_TEXT);
            }
        });
    paint_clear_button(grid, layout);
}

/// Draw the CLEAR button of the assign panel.
fn paint_clear_button(grid: &mut crate::grid::Grid, layout: &EditorLayout) {
    let rect = &layout.clear_button;
    let col = rect.x / i64::from(CANVAS_CELL_W);
    let row = rect.y / i64::from(CANVAS_CELL_H);
    draw_base_text(grid, col + 1, row, "CLEAR", UI_TEXT);
}

/// Draw the asset list of the assign panel.
///
/// An empty list shows `NO ASSETS YET` instead of a blank panel.
fn paint_asset_list(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    let rect = &layout.asset_list;
    let col0 = rect.x / i64::from(CANVAS_CELL_W);
    let row0 = rect.y / i64::from(CANVAS_CELL_H);
    let assets = state.assign_assets();
    if assets.is_empty() {
        draw_base_text(grid, col0 + 1, row0, NO_ASSETS, UI_MUTED);
        return;
    }
    assets
        .iter()
        .enumerate()
        .skip(state.asset_scroll)
        .take(layout.canvas_view_rows)
        .for_each(|(index, name)| {
            let row = row0 + (index - state.asset_scroll) as i64;
            let cells = i64::from(rect.w / CANVAS_CELL_W) - 1;
            let label = fit_base_text(name, cells);
            draw_base_text(grid, col0 + 1, row, label.as_str(), UI_TEXT);
        });
}

/// How many rows the assign panel's list shows: the whole target list
/// when it fits, minus the row the CLEAR button takes when it does not.
pub fn visible_slot_rows(layout: &EditorLayout) -> usize {
    ASSIGN_TARGET_COUNT.min(layout.canvas_view_rows.saturating_sub(1))
}

/// Draw a string in the base layer, one character per big cell.
///
/// The base layer uses the TrueType font, so the labels carry their real
/// characters (lower case included).
fn draw_base_text(grid: &mut crate::grid::Grid, col: i64, row: i64, text: &str, color: [u8; 3]) {
    for (offset, ch) in text.chars().enumerate() {
        grid.set_if_visible(col + offset as i64, row, ch, color);
    }
}

/// `text` cut to `cells` characters, with a trailing `-` when it was
/// cut.
///
/// The base layer's labels used to run to the grid's edge and beyond —
/// the grid clipped them silently, so a long asset name and the list
/// next to it became one smear (audit 2026-10-23 §U-17). The `-` says
/// "there is more", the same ellipsis the block font's dash can spell.
fn fit_base_text(text: &str, cells: i64) -> String {
    if cells <= 0 {
        return String::new();
    }
    let count = text.chars().count() as i64;
    if count <= cells {
        return text.to_string();
    }
    let keep = (cells - 1).max(0) as usize;
    let cut: String = text.chars().take(keep).collect();
    format!("{cut}-")
}

/// Paint the canvas' visible cells into the base layer.
///
/// The **ghost** goes down first and the current frame on top of it, so
/// a cell painted in the current frame hides the ghost underneath it
/// (roadmap §5.6). Only the **active** region is drawn: an inactive
/// cell stays transparent, so the canvas reads as one clean shape with
/// no border and no dimming (decision D18). The rows above the scroll
/// and the columns left of the horizontal scroll are skipped; the cells
/// below and right of the viewport are dropped by the grid itself.
fn paint_canvas(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    paint_ghost(grid, layout, state);
    paint_current_frame(grid, layout, state);
}

/// Paint the ghost — the previous frame at 20 % brightness.
fn paint_ghost(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    let Some(ghost) = state.ghost.as_ref() else {
        return;
    };
    let cols = ghost.cols();
    if cols == 0 {
        return;
    }
    let col0 = layout.canvas.x / i64::from(CANVAS_CELL_W);
    let row0 = layout.canvas.y / i64::from(CANVAS_CELL_H);
    let scroll = layout.canvas_scroll_rows;
    let scroll_cols = layout.canvas_scroll_cols;
    for (index, (&ch, &color)) in ghost.chars().iter().zip(ghost.colors()).enumerate() {
        if ch == ' ' {
            continue;
        }
        let col = index % cols;
        let row = index / cols;
        if !column_in_view(col, scroll_cols, layout.canvas_view_cols) {
            continue;
        }
        if row < scroll {
            continue;
        }
        let target_row = row0 + (row - scroll) as i64;
        let target_col = col0 + (col - scroll_cols) as i64;
        grid.set_if_visible(target_col, target_row, ch, dim_color(color));
    }
}

/// Paint the frame that is being drawn.
fn paint_current_frame(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    let cols = state.canvas_cols();
    if cols == 0 {
        return;
    }
    // A decoration is authored on the duck's own frame, so the duck's
    // art is shown **underneath** it, faint (roadmap Extension 2 §4.2).
    paint_decoration_underlay(grid, layout, state);
    let col0 = layout.canvas.x / i64::from(CANVAS_CELL_W);
    let row0 = layout.canvas.y / i64::from(CANVAS_CELL_H);
    let scroll = layout.canvas_scroll_rows;
    let scroll_cols = layout.canvas_scroll_cols;
    let chars = state.canvas.chars();
    let colors = state.canvas.colors();
    for (index, (&ch, &color)) in chars.iter().zip(colors.iter()).enumerate() {
        if ch == ' ' {
            continue;
        }
        let col = index % cols;
        let row = index / cols;
        if !column_in_view(col, scroll_cols, layout.canvas_view_cols) {
            continue;
        }
        if row < scroll || !state.cell_is_active(col, row) {
            continue;
        }
        let target_row = row0 + (row - scroll) as i64;
        let target_col = col0 + (col - scroll_cols) as i64;
        grid.set_if_visible(target_col, target_row, ch, color);
    }
}

/// Paint the built-in duck art as a faint underlay of a decoration.
///
/// The underlay is the built-in art at 20 % brightness — the same
/// treatment the frame ghost uses — so the user can see where the
/// duck's head is. It is **not** saved with the decoration: it is a
/// guide, not part of the art (roadmap Extension 2 §4.2).
fn paint_decoration_underlay(
    grid: &mut crate::grid::Grid,
    layout: &EditorLayout,
    state: &EditorState,
) {
    if state.kind != crate::drawn::DrawnKind::Decoration {
        return;
    }
    let art = match state.size {
        crate::drawn::DrawnSize::Big => crate::sprites::big_duck(),
        crate::drawn::DrawnSize::Small => crate::sprites::small_duck(),
    };
    let color = dim_color(crate::scene::DUCK_COLOR);
    for (row, line) in art.frame(0).iter().enumerate() {
        if row < layout.canvas_scroll_rows {
            continue;
        }
        paint_underlay_row(grid, layout, line, row, color);
    }
}

/// Paint one row of the duck's faint underlay.
///
/// WHY a helper: the nesting rule is two levels at most, and the painter
/// was a loop in a loop (audit 2026-10-21 §F8).
fn paint_underlay_row(
    grid: &mut crate::grid::Grid,
    layout: &EditorLayout,
    line: &str,
    row: usize,
    color: [u8; 3],
) {
    let col0 = layout.canvas.x / i64::from(CANVAS_CELL_W);
    let row0 = layout.canvas.y / i64::from(CANVAS_CELL_H);
    let scroll = layout.canvas_scroll_rows;
    let scroll_cols = layout.canvas_scroll_cols;
    for (offset, ch) in line.chars().enumerate() {
        if ch == ' ' {
            continue;
        }
        let col = offset;
        if !column_in_view(col, scroll_cols, layout.canvas_view_cols) {
            continue;
        }
        let target_col = col0 + (col - scroll_cols) as i64;
        let target_row = row0 + (row - scroll) as i64;
        grid.set_if_visible(target_col, target_row, ch, color);
    }
}

/// True when the canvas column `col` is inside the visible window.
///
/// A grass tile's canvas is 200 columns wide and a sprite's 150, so the
/// painting skips the columns that are off screen instead of doing the
/// work for them (the grid would drop them anyway).
fn column_in_view(col: usize, scroll: usize, view: usize) -> bool {
    col >= scroll && col < scroll + view
}

/// Paint the note strip: the caption, the note lane and the ruler
/// (roadmap Extension 3 §3.2, audit 2026-10-23 §U-16).
///
/// The caption (row 0) names the lane where the lane is — the top bar's
/// own caption is far from the strip once it scrolls. The lane is row 1
/// — one cell per step, a note drawn where there is one. The ruler is
/// row 2: a tick per step, with the **start of the loop** and the
/// **end of the loop** marked, so the user can see where the music
/// begins and where the trailing duck clears the left edge.
fn paint_strip(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    let col0 = layout.canvas.x / i64::from(CANVAS_CELL_W);
    let row0 = layout.canvas.y / i64::from(CANVAS_CELL_H);
    let scroll = layout.canvas_scroll_cols;
    draw_base_text(
        grid,
        col0,
        row0 + STRIP_CAPTION_ROW as i64,
        STRIP_LANE_CAPTION,
        UI_MUTED,
    );
    let last = state.strip_steps.saturating_sub(1);
    for step in scroll..(scroll + layout.canvas_view_cols).min(state.strip_steps) {
        let col = col0 + (step - scroll) as i64;
        let (glyph, color) = ruler_mark(step, last);
        grid.set_if_visible(col, row0 + STRIP_RULER_ROW as i64, glyph, color);
    }
    state.track.iter().for_each(|(step, note)| {
        if *step >= state.strip_steps {
            return;
        }
        if !column_in_view(*step, scroll, layout.canvas_view_cols) {
            return;
        }
        let col = col0 + (step - scroll) as i64;
        let color = note_color(note.pitch);
        grid.set_if_visible(col, row0 + STRIP_NOTE_ROW as i64, NOTE_GLYPH, color);
    });
}

/// The mark the ruler shows on a step.
///
/// The first slot is the start of the loop and the last slot is the
/// moment the trailing duck clears the left edge (roadmap Extension 3
/// §3.4): both carry their own glyph, the rest of the ruler is a tick.
fn ruler_mark(step: usize, last: usize) -> (char, [u8; 3]) {
    if step == 0 {
        return (RULER_START, UI_TEXT);
    }
    if step == last {
        return (RULER_END, UI_TOAST);
    }
    (RULER_TICK, UI_MUTED)
}

/// The colour a note is drawn with: the colour of its pitch.
fn note_color(pitch: u8) -> [u8; 3] {
    let index = PITCH_MIDI
        .iter()
        .position(|value| *value == pitch)
        .unwrap_or(0);
    PITCH_COLORS.get(index).copied().unwrap_or(UI_TEXT)
}

/// Paint the tool/colour panel (roadmap §5.5) or the pitch palette and
/// the track list (roadmap Extension 3 §3.2).
///
/// The panel is a vertical strip in the base layer: the 19 tool glyphs,
/// a separator and the 8 colour swatches, with a `>` marker in the
/// column to the left of the selected tool and the selected colour. The
/// panel is 28 rows tall; when the window is too short for that, the
/// panel scrolls instead of overflowing the window.
fn paint_tool_panel(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    let visible = layout.canvas_view_rows;
    panel_entries(state)
        .iter()
        .enumerate()
        .skip(state.panel_scroll)
        .take(visible)
        .for_each(|(index, entry)| {
            draw_panel_entry(grid, layout, state, index - state.panel_scroll, entry);
        });
}

/// Draw one row of the side panel.
fn draw_panel_entry(
    grid: &mut crate::grid::Grid,
    layout: &EditorLayout,
    state: &EditorState,
    row_offset: usize,
    entry: &PanelEntry,
) {
    let row = layout.tools.y / i64::from(CANVAS_CELL_H) + row_offset as i64;
    let marker_col = layout.tools.x / i64::from(CANVAS_CELL_W);
    let glyph_col = marker_col + 1;
    match entry {
        PanelEntry::Tool(index) => {
            let Some(&ch) = crate::drawn::TOOLS.get(*index) else {
                return;
            };
            grid.set_if_visible(glyph_col, row, ch, UI_TEXT);
        }
        PanelEntry::Color(index) => {
            let Some(&color) = crate::drawn::PALETTE.get(*index) else {
                return;
            };
            grid.set_if_visible(glyph_col, row, 'X', color);
        }
        PanelEntry::Separator => {
            grid.set_if_visible(glyph_col, row, RULER_TICK, UI_MUTED);
        }
        PanelEntry::Pitch(index) => {
            let Some(name) = PITCH_NAMES.get(*index) else {
                return;
            };
            let color = PITCH_COLORS.get(*index).copied().unwrap_or(UI_TEXT);
            draw_base_text(grid, glyph_col, row, name, color);
        }
        PanelEntry::Track(index) => {
            let Some(name) = state.tracks.get(*index) else {
                return;
            };
            let color = if state.track_name == *name {
                UI_TEXT
            } else {
                UI_MUTED
            };
            let label = fit_base_text(name, PITCH_PANEL_CELLS as i64 - 1);
            draw_base_text(grid, glyph_col, row, label.as_str(), color);
        }
    }
    if entry.is_selected(state) {
        grid.set_if_visible(marker_col, row, '>', UI_TEXT);
    }
}

/// Paint the three dropdown boxes and the open option list.
fn paint_dropdowns(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    for (index, rect) in layout.dropdowns.iter().enumerate() {
        let enabled = state.dropdown_enabled(index);
        let color = if enabled { UI_TEXT } else { UI_MUTED };
        draw_field_rule(grid, rect, color);
        draw_field_label(grid, rect, state.dropdown_label(index), color);
    }
    paint_open_list(grid, layout, state);
}

/// Draw the dashed rule under a field's label.
fn draw_field_rule(grid: &mut crate::grid::Grid, rect: &Rect, color: [u8; 3]) {
    let row = ui_row(rect.y) + RULE_UI_ROW;
    let first = ui_col(rect.x);
    let last = ui_col(rect.right());
    for col in first..last {
        grid.set_if_visible(col, row, '-', color);
    }
}

/// Draw a field's label, centred in its box.
fn draw_field_label(grid: &mut crate::grid::Grid, rect: &Rect, text: &str, color: [u8; 3]) {
    let width = crate::hud_font::text_width(text);
    let box_width = i64::from(rect.w) / i64::from(UI_CELL_W);
    let col = ui_col(rect.x) + ((box_width - width) / 2).max(0);
    crate::hud_font::draw_text(grid, col, ui_row(rect.y), text, color);
}

/// Draw a field's text, left-aligned inside its box.
fn draw_field_text(grid: &mut crate::grid::Grid, rect: &Rect, text: &str, color: [u8; 3]) {
    crate::hud_font::draw_text(grid, ui_col(rect.x) + 1, ui_row(rect.y), text, color);
}

/// Draw the open dropdown's option list under its box.
///
/// The list lives in the overlay layer, so it overlaps the canvas
/// (roadmap §4). The selected option is drawn in the bright colour, the
/// others in the muted one.
fn paint_open_list(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    let Some(index) = state.open_dropdown else {
        return;
    };
    let Some(rect) = layout.dropdowns.get(index) else {
        return;
    };
    let options = state.dropdown_options(index);
    let selected = state.dropdown_option_index(index);
    let first_row = ui_row(rect.bottom()) + 1;
    for (option_index, option) in options.iter().enumerate() {
        let row = first_row + (option_index as i64) * OPTION_UI_ROWS;
        let color = if option_index == selected {
            UI_TEXT
        } else {
            UI_MUTED
        };
        crate::hud_font::draw_text(grid, ui_col(rect.x), row, option, color);
    }
}

/// Draw the name field: the name, a trailing caret and the rule.
///
/// The caret is an underscore, because the block font has no cursor
/// glyph. A name that does not fit the field shows its tail, so the
/// caret always stays visible.
///
/// The field is **never dropped**: the window's minimum size keeps it
/// wide enough, and a name the user cannot see is a name they type into
/// blindly (audit 2026-10-20 § editor, task 1). An empty name shows the
/// **placeholder** in the muted colour, so it reads as a hint and not as
/// the content the user typed.
fn paint_name_field(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    let rect = &layout.name_field;
    draw_field_rule(grid, rect, UI_TEXT);
    let inner = i64::from(rect.w) / i64::from(UI_CELL_W) - 2;
    let (shown, color) = if state.mode == EditorMode::Sequencer {
        strip_name_display(state)
    } else if state.name.is_empty() {
        (state.default_name.as_str(), UI_MUTED)
    } else {
        (state.name.as_str(), UI_TEXT)
    };
    let mut text = name_tail(shown, inner);
    text.push('_');
    draw_field_text(grid, rect, &text, color);
}

/// The name the field shows in strip mode: the track's name, or the
/// placeholder when the user has typed nothing.
fn strip_name_display(state: &EditorState) -> (&str, [u8; 3]) {
    if state.track_name.is_empty() {
        return (TRACK_NAME_PLACEHOLDER, UI_MUTED);
    }
    (state.track_name.as_str(), UI_TEXT)
}

/// The tail of `name` that fits in `cells` UI cells, caret included.
///
/// The name is taken from the **end**, so the caret always stays
/// visible. The block font's glyphs have different widths, so the
/// characters are measured one by one.
fn name_tail(name: &str, cells: i64) -> String {
    let caret = crate::hud_font::char_width('_');
    let mut tail = String::new();
    let mut width = caret;
    for ch in name.chars().rev() {
        let char_width = crate::hud_font::char_width(ch);
        if width + char_width > cells {
            break;
        }
        tail.insert(0, ch);
        width += char_width;
    }
    tail
}

/// The status line with the drawing's name echoed, when the name field
/// truncates it (audit 2026-10-23 §U-06).
///
/// The field shows the **tail** of a long name — the caret must stay
/// visible while the user types — so the head is exactly the part the
/// field cannot show. The echo is the **whole** name, upper case (the
/// block font draws upper case only), and only when the field really
/// truncates: a name that fits is already readable where it is.
fn name_echo(state: &EditorState, layout: &EditorLayout, full: &str) -> String {
    if state.name.is_empty() {
        return full.to_string();
    }
    let inner = i64::from(layout.name_field.w / UI_CELL_W) - 2;
    let shown = name_tail(&state.name, inner);
    if shown.chars().count() == state.name.chars().count() {
        return full.to_string();
    }
    format!("{full}  NAME {}", state.name.to_uppercase())
}

/// Draw the primary (SAVE) button of the top bar.
///
/// It looks like a dropdown box — a label over a rule — and triggers the
/// same primary action as the Enter key (roadmap §5.6).
fn paint_primary_button(grid: &mut crate::grid::Grid, layout: &EditorLayout) {
    let rect = &layout.primary_button;
    draw_field_rule(grid, rect, UI_TEXT);
    draw_field_label(grid, rect, "SAVE", UI_TEXT);
}

/// Draw the mode buttons of the top bar (roadmap Stage 9).
///
/// `ASSIGN` switches the editor into assign mode, `TRK` into the note
/// strip and `DRAW` switches it back. The button of the *current* mode
/// is **bright** and the others are muted (audit 2026-10-23 §U-29 —
/// this doc used to say the opposite): the bright label answers "where
/// am I?", and the muted ones still say where a click would go.
fn paint_mode_buttons(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    let drawing = state.mode == EditorMode::Draw;
    let assigning = state.mode == EditorMode::Assign;
    let settings = state.mode == EditorMode::Settings;
    let tracking = state.mode == EditorMode::Sequencer;
    let assign_color = if assigning { UI_TEXT } else { UI_MUTED };
    let draw_color = if drawing { UI_TEXT } else { UI_MUTED };
    let settings_color = if settings { UI_TEXT } else { UI_MUTED };
    let track_color = if tracking { UI_TEXT } else { UI_MUTED };
    draw_field_rule(grid, &layout.assign_button, assign_color);
    draw_field_label(grid, &layout.assign_button, "ASSIGN", assign_color);
    draw_field_rule(grid, &layout.draw_button, draw_color);
    draw_field_label(grid, &layout.draw_button, "DRAW", draw_color);
    draw_field_rule(grid, &layout.settings_button, settings_color);
    draw_field_label(grid, &layout.settings_button, "CFG", settings_color);
    draw_field_rule(grid, &layout.track_button, track_color);
    draw_field_label(grid, &layout.track_button, "TRK", track_color);
}

/// Draw the status line — and the toast, on the strip's second row.
///
/// The toast no longer replaces the status line (audit 2026-10-23
/// §U-04): for the 2.5 s it is on screen the user has usually just
/// *changed* the thing the status line names, so hiding it hid exactly
/// what they needed to see. The strip holds both rows — the status on
/// top, the toast below — and both are drawn every frame.
fn paint_status_line(grid: &mut crate::grid::Grid, layout: &EditorLayout, state: &EditorState) {
    let rect = &layout.status;
    let row = ui_row(rect.y) + STATUS_TEXT_UI_ROW;
    let text = status_text(state, layout);
    crate::hud_font::draw_text(grid, ui_col(rect.x), row, &block_font_text(&text), UI_TEXT);
    draw_grass_range(grid, layout, state, row, &text);
    if let Some(toast) = state.active_toast() {
        crate::hud_font::draw_text(
            grid,
            ui_col(rect.x),
            ui_row(rect.y) + TOAST_UI_ROW,
            &block_font_text(toast),
            UI_TOAST,
        );
    }
}

/// Draw the grass tile's column range at the right of the status line.
///
/// The tile's canvas is 200 columns wide and scrolls, so the line says
/// which columns the window is looking at — but only when there is room
/// for it next to the text.
fn draw_grass_range(
    grid: &mut crate::grid::Grid,
    layout: &EditorLayout,
    state: &EditorState,
    row: i64,
    text: &str,
) {
    if state.kind != crate::drawn::DrawnKind::Grass {
        return;
    }
    if state.mode == EditorMode::Sequencer {
        return;
    }
    let range = visible_column_range(state, layout);
    let width = block_font_width(&range);
    let cols = i64::from(layout.status.w) / i64::from(UI_CELL_W);
    if block_font_width(text) + width + 2 > cols {
        return;
    }
    crate::hud_font::draw_text(grid, cols - width, row, &range, UI_MUTED);
}

/// The text as the block font can draw it.
///
/// The whole status line, the toast and the strip echo go through here,
/// and here is where they are folded to exactly the glyphs the font
/// carries: the font has no `#`, `=`, `(`, `)` or `!`, and a caller
/// that drew its text raw would silently lose them (audit 2026-10-23
/// F-043/U-05 — one shared sanitizer, `hud_font::sanitize`). The `=`
/// is special-cased **first**, as the dash the font does carry: an
/// assignment reads `A - B` in every medium that shows it.
pub fn block_font_text(text: &str) -> String {
    crate::hud_font::sanitize(&text.replace('=', "-"))
}

/// The width of the text in UI cells, as the block font draws it.
///
/// The measurement runs over the **sanitized** text, so it counts the
/// dash an `=` becomes and drops the glyphs the font never draws — the
/// number always matches what [`block_font_text`] puts on screen.
pub fn block_font_width(text: &str) -> i64 {
    crate::hud_font::text_width(&block_font_text(text))
}

/// The status line that fits the window.
///
/// The long form names every dropdown (decision D15); a narrower window
/// gets a shorter form, and a very narrow one only the kind. Every form
/// starts with `TYPE:` so the line always has ink in its first cell.
///
/// The forms are a **ladder**: each step drops the least useful piece,
/// so a mid-size window keeps the duck size and the active tool instead
/// of losing both (audit 2026-10-20 § editor, task 4). In assign mode
/// the line names the selected slot instead, and for a grass tile it
/// names the tile, its caps and the columns the window is looking at.
///
/// A **name the field truncates** is echoed in full on a wide window
/// (audit 2026-10-23 §U-06): the field shows the tail so the caret
/// stays visible, but a tail alone cannot be checked against the file
/// on disk. The echo is the ladder's top form, so a narrower window
/// drops it first — the field still shows the tail.
pub fn status_text(state: &EditorState, layout: &EditorLayout) -> String {
    let cols = i64::from(layout.status.w) / i64::from(UI_CELL_W);
    if state.mode == EditorMode::Assign {
        return assign_status_text(state, cols);
    }
    if state.mode == EditorMode::Settings {
        return settings_status_text(state, cols);
    }
    if state.mode == EditorMode::Sequencer {
        return strip_status_text(state, cols);
    }
    // A pending draft offer outranks the ordinary status line: the
    // question has to be answered before anything else (roadmap
    // Extension 2 §2.2).
    if state.draft_offer.is_some() {
        return DRAFT_OFFER.to_string();
    }
    if state.kind == crate::drawn::DrawnKind::Grass {
        return grass_status_text(state, cols);
    }
    let full = state.status.clone();
    let with_name = name_echo(state, layout, &full);
    if block_font_width(&with_name) <= cols {
        return with_name;
    }
    if block_font_width(&full) <= cols {
        return full;
    }
    let medium = format!(
        "TYPE: {}  {}  FRAME {}/{}  {}  {}",
        state.dropdown_long(0),
        state.size_label(),
        state.frame_index + 1,
        state.frames_wanted,
        tool_label(state.tool),
        colour_label(state.color),
    );
    if block_font_width(&medium) <= cols {
        return medium;
    }
    let compact = format!(
        "TYPE: {}  FRAME {}/{}  {}  {}",
        state.dropdown_long(0),
        state.frame_index + 1,
        state.frames_wanted,
        tool_label(state.tool),
        colour_label(state.color),
    );
    if block_font_width(&compact) <= cols {
        return compact;
    }
    format!("TYPE: {}", state.dropdown_long(0))
}

/// The full grass status line: the kind, the tile, its size and the caps.
///
/// The size is the size of the **inked** part of the canvas — the tile
/// is saved cropped to it, so that is the tile the user will get.
fn grass_status_full(tile: &crate::balloon::AsciiSprite) -> String {
    format!(
        "TYPE: GRASS  TILE {}X{}  MAX {} ROWS {} COLS",
        tile.cols(),
        tile.rows(),
        crate::drawn::GRASS_TILE_MAX_ROWS,
        crate::drawn::GRASS_TILE_MAX_COLS,
    )
}

/// The grass status line that fits `cols` UI cells.
///
/// A grass tile says three things: how big the tile is, the caps it must
/// respect (at most 4 rows and 200 columns) and — on the right of the
/// line, when there is room — which columns the window is looking at,
/// because the canvas is 200 columns wide and scrolls.
fn grass_status_text(state: &EditorState, cols: i64) -> String {
    let tile = state.grass_tile();
    let size = format!("{}X{}", tile.cols(), tile.rows());
    let max_rows = crate::drawn::GRASS_TILE_MAX_ROWS;
    let max_cols = crate::drawn::GRASS_TILE_MAX_COLS;
    let full = format!(
        "TYPE: GRASS  {}  {size}  MAX {max_rows} ROWS {max_cols} COLS",
        state.size_label(),
    );
    if block_font_width(&full) <= cols {
        return full;
    }
    let caps = format!("TYPE: GRASS  {size}  MAX {max_rows} ROWS {max_cols} COLS");
    if block_font_width(&caps) <= cols {
        return caps;
    }
    let compact = format!("TYPE: GRASS  {size}  MAX {max_rows}X{max_cols}");
    if block_font_width(&compact) <= cols {
        return compact;
    }
    let sized = format!("TYPE: GRASS  {size}");
    if block_font_width(&sized) <= cols {
        return sized;
    }
    "TYPE: GRASS".to_string()
}

/// The columns the canvas viewport shows, as `first-last`.
///
/// The numbers are 1-based, so they read like the column numbers of a
/// sheet: the first visible column is 1 when the canvas is not scrolled.
pub fn visible_column_range(state: &EditorState, layout: &EditorLayout) -> String {
    let first = layout.canvas_scroll_cols + 1;
    let last = (layout.canvas_scroll_cols + layout.canvas_view_cols).min(state.canvas_cols());
    format!("{first}-{last}")
}

/// The status line of assign mode: the selected target and the asset it
/// currently points at.
pub fn assign_status_text(state: &EditorState, cols: i64) -> String {
    let Some(target) = state.selected_target() else {
        return "ASSIGN".to_string();
    };
    let label = target_label(&target);
    let text = match target_assignment(state, &target) {
        Some(name) => format!("ASSIGN: {label} = {}", name.to_uppercase()),
        None => format!("ASSIGN: {label}"),
    };
    if block_font_width(&text) <= cols {
        return text;
    }
    "ASSIGN".to_string()
}

/// The status line of the strip mode: the track, the pitch, the number
/// of notes and the length of the strip.
///
/// The line is a **ladder**, like the drawing's: the fullest form names
/// everything, and a narrow window keeps what matters most.
fn strip_status_text(state: &EditorState, cols: i64) -> String {
    let full = state.strip_status();
    if block_font_width(&full) <= cols {
        return full;
    }
    let name = if state.track_name.is_empty() {
        "-"
    } else {
        state.track_name.as_str()
    };
    let medium = format!(
        "TRACK: {}  {}  NOTES {}",
        name.to_uppercase(),
        state.pitch_display(),
        state.track.len()
    );
    if block_font_width(&medium) <= cols {
        return medium;
    }
    let compact = format!(
        "TRACK: {}  NOTES {}",
        name.to_uppercase(),
        state.track.len()
    );
    if block_font_width(&compact) <= cols {
        return compact;
    }
    "TRACK".to_string()
}

/// The status line of the settings panel: the panel's name, the row the
/// cursor is on and its value.
///
/// The line is prefixed with `CFG - ` (audit 2026-10-23 §X-21): the
/// panel replaced the canvas, and without the prefix the line looked
/// like the drawing's own status. A focused text field shows what the
/// user is typing, so the line is the field's own echo.
fn settings_status_text(state: &EditorState, cols: i64) -> String {
    if state.settings_focus.is_some() {
        let shown = if state.settings_typed {
            state.settings_text.clone()
        } else {
            String::new()
        };
        let text = format!("CFG - EDIT: {shown}");
        if block_font_width(&text) <= cols {
            return text;
        }
        return "CFG - EDIT".to_string();
    }
    let Some(setting) = state.settings_row() else {
        return "CFG".to_string();
    };
    let value = state
        .settings_value(state.settings_cursor)
        .map(|value| value.display(setting))
        .unwrap_or_default();
    let text = format!("CFG - {} = {}", setting.label, value);
    if block_font_width(&text) <= cols {
        return text;
    }
    format!("CFG - {}", setting.label)
}

/// The asset a slot currently points at, if any.
/// What the selected target currently points at, from the state's own
/// copies of the config tables: the `[assignments]` entry for a parade
/// slot, the grass tile or the decoration for a look target.
fn target_assignment<'a>(state: &'a EditorState, target: &AssignTarget) -> Option<&'a str> {
    match target {
        AssignTarget::Slot(slot) => slot_assignment(state, slot),
        AssignTarget::GrassTile => state.settings_config.grass.tile.as_deref(),
        AssignTarget::DecorationBig => state.settings_config.decorations.big.as_deref(),
        AssignTarget::DecorationSmall => state.settings_config.decorations.small.as_deref(),
    }
}

fn slot_assignment<'a>(
    state: &'a EditorState,
    slot: &crate::config::AssignmentSlot,
) -> Option<&'a str> {
    match slot.kind {
        crate::config::SlotKind::Big => state.assignments.big(slot.number),
        crate::config::SlotKind::Small => state.assignments.small(slot.number),
        crate::config::SlotKind::Balloon => state.assignments.balloon(slot.number),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drawn::DrawnSize;

    #[test]
    fn editor_window_rect_is_clamped_and_centred() {
        // 80 % of a 1920x1080 monitor.
        assert_eq!(editor_window_rect(1920, 1080), (1536, 864));
        // The origin centres the rect on the monitor.
        assert_eq!(editor_window_origin((0, 0), 1920, 1080, 0), (192, 108));
        assert_eq!(
            editor_window_origin((1920, 0), 1920, 1080, 0),
            (2112, 108),
            "a second monitor offsets the origin"
        );
        // A monitor bigger than the maximum clamps to the maximum.
        assert_eq!(editor_window_rect(4000, 3000), (1920, 1200));
        // The rect never leaves the min..=max band.
        for (mw, mh) in [(800, 600), (1280, 720), (2560, 1440), (3840, 2160)] {
            let (w, h) = editor_window_rect(mw, mh);
            assert!((MIN_WINDOW_W..=MAX_WINDOW_W).contains(&w), "width {w}");
            assert!((MIN_WINDOW_H..=MAX_WINDOW_H).contains(&h), "height {h}");
        }
    }

    #[test]
    fn the_window_opens_wide_enough_for_the_balloon_disc() {
        let state = state();
        let min_width = balloon_canvas_width(&state);
        // The disc (109 cells) plus the tool panel, in big cells.
        assert_eq!(min_width, 111 * CANVAS_CELL_W);
        // A 1920-wide monitor: the window grows past the 80 % share so
        // the whole disc fits, but never past the monitor.
        let (w, _) = editor_window_rect_for(1920, 1080, min_width);
        assert_eq!(w, min_width, "the disc must fit in full");
        assert!(w <= 1920, "the window never leaves the monitor");
        // A 4K monitor: the 1920 cap wins.
        let (w, _) = editor_window_rect_for(3840, 2160, min_width);
        assert_eq!(w, MAX_WINDOW_W);
        // A small monitor: the window never grows past the monitor.
        let (w, _) = editor_window_rect_for(1366, 768, min_width);
        assert_eq!(w, 1366, "a small monitor clips the canvas");
    }

    #[test]
    fn editor_window_rect_respects_the_minimum() {
        // A small monitor still gets the minimum usable window.
        assert_eq!(editor_window_rect(640, 480), (MIN_WINDOW_W, 600));
        assert_eq!(editor_window_rect(0, 0), (MIN_WINDOW_W, 600));
        // The minimum window is centred on the small monitor, so half of it
        // hangs over the edge — the size never drops below the minimum.
        let origin = editor_window_origin((0, 0), 640, 480, 0);
        let expected = -((i64::from(MIN_WINDOW_W) - 640) / 2) as i32;
        assert_eq!(origin.0, expected);
        assert_eq!(origin.1, -60);
    }

    /// A fresh editor state for the default config.
    fn state() -> EditorState {
        EditorState::new(&crate::config::Config::default(), Instant::now)
    }

    /// A temp folder that is not the user's real config folder.
    fn temp_root(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "agnosis-editor-{}-{}-{tag}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("mkdir temp dir");
        dir
    }

    /// True when any of the `cols` cells of `row` starting at `col` carries
    /// ink. The block font leaves some cells of a glyph empty (the `S`
    /// starts with a dot), so a label is checked as a run, not as one cell.
    fn row_has_ink(grid: &crate::grid::Grid, col: usize, row: usize, cols: usize) -> bool {
        (col..col + cols).any(|c| grid.char_at(c, row) == Some('X'))
    }

    /// The colour of the first cell of `row` at or after `col` that
    /// carries ink.
    fn first_ink_color(
        grid: &crate::grid::Grid,
        col: usize,
        row: usize,
        count: usize,
    ) -> Option<[u8; 3]> {
        (col..col + count).find_map(|cell| {
            if grid.char_at(cell, row) == Some('X') {
                grid.color_at(cell, row)
            } else {
                None
            }
        })
    }

    #[test]
    fn layout_fits_the_canvas_and_panel() {
        let state = state();
        let layout = layout(1536, 864, &state);
        // The top bar is a strip at the top, the status line one at the
        // bottom, and neither reaches into the canvas.
        assert_eq!(layout.top_bar.y, 0);
        assert!(
            layout.top_bar.bottom() <= layout.canvas.y,
            "the top bar must not reach the canvas"
        );
        assert!(
            layout.status.y >= layout.canvas.bottom(),
            "the status line must not reach the canvas"
        );
        assert!(
            layout.status.bottom() <= 864,
            "the status line must stay inside the window"
        );
        // The tool panel is a vertical strip on the right of the canvas,
        // and the two together fill the window's width.
        assert_eq!(layout.tools.x, layout.canvas.right());
        assert_eq!(layout.tools.y, layout.canvas.y);
        assert_eq!(layout.tools.h, layout.canvas.h);
        assert_eq!(layout.canvas.right() + i64::from(layout.tools.w), 1536);
        // The canvas is a square with a side of the element's frame width.
        assert_eq!(layout.canvas_cols, 150);
        assert_eq!(layout.canvas_rows, 150);
        assert_eq!(
            layout.canvas_cols,
            crate::drawn::sprite_canvas_side(crate::drawn::DrawnSize::Big)
        );
        // The scroll never leaves the active region.
        assert!(
            layout.canvas_scroll_rows + layout.canvas_view_rows <= state.active_rows(),
            "the scroll must stay inside the active region"
        );
    }

    #[test]
    fn layout_is_pure_for_the_same_input() {
        let state = state();
        let first = layout(1536, 864, &state);
        let second = layout(1536, 864, &state);
        assert_eq!(first, second, "the same input must give the same layout");
        // A different window size gives a different layout.
        let smaller = layout(1024, 768, &state);
        assert_ne!(
            first.canvas, smaller.canvas,
            "the layout must follow the window"
        );
        assert_ne!(first.status, smaller.status);
    }

    #[test]
    fn set_kind_resets_the_canvas() {
        let mut state = state();
        state.canvas.set(3, 4, 'x', crate::drawn::PALETTE[1]);
        assert!(state.canvas.ink_count() > 0, "the canvas must start dirty");
        state.set_kind(crate::drawn::DrawnKind::Balloon);
        assert_eq!(state.canvas.ink_count(), 0, "the canvas must be cleared");
        assert_eq!(
            (state.canvas.cols(), state.canvas.rows()),
            (state.balloon_geo.cols, state.balloon_geo.rows),
            "the balloon's canvas is the disc"
        );
        // The sprite's canvas is the frame width wide.
        state.set_kind(crate::drawn::DrawnKind::Sprite);
        assert_eq!(state.canvas.cols(), 150);
    }

    #[test]
    fn the_balloon_canvas_is_the_disc_not_a_square() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Balloon);
        assert_eq!(
            state.canvas_size(),
            (109, 73),
            "the balloon's canvas is the disc's own cell grid"
        );
        assert_eq!(
            (state.canvas.cols(), state.canvas.rows()),
            (109, 73),
            "and the canvas holds it"
        );
        // The active cells are the disc: the corners are outside it, the
        // edge midpoints are inside.
        assert!(!state.cell_is_active(0, 0), "the corner is outside");
        assert!(!state.cell_is_active(108, 72), "and so is the far corner");
        assert!(state.cell_is_active(0, 36), "the left edge is inside");
        assert!(state.cell_is_active(54, 0), "the top edge is inside");
        assert!(state.cell_is_active(54, 36), "the centre is inside");
        // The sprite's canvas stays square.
        state.set_kind(crate::drawn::DrawnKind::Sprite);
        assert_eq!(state.canvas_size(), (150, 150));
    }

    #[test]
    fn the_canvas_cells_keep_the_authoring_aspect() {
        // The disc is a circle on screen only because the canvas cell is
        // 2:3 (taller than wide) and the disc is 3:2 in cells. If the
        // cell ever changes its aspect, the balloon's authoring surface
        // stops being a circle and the art comes out stretched again.
        assert_eq!(
            CANVAS_CELL_W * 3,
            CANVAS_CELL_H * 2,
            "the canvas cell must stay 2:3"
        );
        let geometry = crate::balloon::balloon_authoring_geometry(36);
        let width = geometry.cols as u32 * CANVAS_CELL_W;
        let height = geometry.rows as u32 * CANVAS_CELL_H;
        let skew = (width as i64 - height as i64).abs();
        assert!(
            skew * 100 < width as i64,
            "the disc's frame is square in pixels (skew {skew})"
        );
    }

    #[test]
    fn set_kind_disables_dropdown_two_and_three() {
        let mut state = state();
        assert!(
            state.dropdown_enabled(1) && state.dropdown_enabled(2),
            "a sprite enables both"
        );
        state.set_kind(crate::drawn::DrawnKind::Balloon);
        assert!(
            !state.dropdown_enabled(1),
            "the size is inactive for a balloon"
        );
        assert!(
            !state.dropdown_enabled(2),
            "the frame count is inactive for a balloon"
        );
        // An inactive dropdown neither opens nor changes its value.
        state.open_dropdown(1);
        assert_eq!(state.open_dropdown, None, "an inactive dropdown stays shut");
        state.pick_option(1, 1);
        assert_eq!(
            state.size,
            crate::drawn::DrawnSize::Big,
            "the size must not change"
        );
        // Back to a sprite: both are live again.
        state.set_kind(crate::drawn::DrawnKind::Sprite);
        assert!(state.dropdown_enabled(1) && state.dropdown_enabled(2));
    }

    #[test]
    fn set_frames_clamps_to_one_through_four() {
        let mut state = state();
        state.set_frames(0);
        assert_eq!(state.frames_wanted, 1, "the count never drops below one");
        state.set_frames(9);
        assert_eq!(state.frames_wanted, 4, "the count never rises above four");
        state.set_frames(3);
        assert_eq!(state.frames_wanted, 3);
        // The frame count never clears a canvas that has content.
        state.canvas.set(0, 0, 'x', crate::drawn::PALETTE[0]);
        state.set_frames(1);
        assert!(
            state.canvas.ink_count() > 0,
            "the canvas must keep its content"
        );
    }

    #[test]
    fn set_frames_drops_surplus_frames() {
        let mut state = state();
        let (cols, rows) = state.canvas_size();
        for _ in 0..3 {
            state
                .frames
                .push(crate::balloon::AsciiSprite::new(cols, rows));
        }
        state.set_frames(2);
        assert_eq!(state.frames.len(), 2, "the surplus frame must be dropped");
        let toast = state.active_toast().expect("the status must say so");
        assert!(toast.contains("DROPPED 1 FRAME"), "{toast}");
        // One frame too many: the surplus goes as well.
        state
            .frames
            .push(crate::balloon::AsciiSprite::new(cols, rows));
        state.set_frames(1);
        assert_eq!(state.frames.len(), 1);
    }

    #[test]
    fn dropdown_open_and_pick_updates_the_value() {
        let mut state = state();
        state.open_dropdown(0);
        assert_eq!(state.open_dropdown, Some(0));
        assert_eq!(state.dropdown_options(0), &["BAL", "SPR", "GRS", "DEC"]);
        // Picking the sprite selects it and closes the list.
        state.pick_option(0, 1);
        assert_eq!(state.kind, crate::drawn::DrawnKind::Sprite);
        assert_eq!(state.open_dropdown, None, "the list must close");
        // Picking the balloon selects it.
        state.open_dropdown(0);
        state.pick_option(0, 0);
        assert_eq!(state.kind, crate::drawn::DrawnKind::Balloon);
        assert_eq!(state.open_dropdown, None);
        // Picking the grass tile selects it.
        state.open_dropdown(0);
        state.pick_option(0, 2);
        assert_eq!(state.kind, crate::drawn::DrawnKind::Grass);
        assert_eq!(state.open_dropdown, None);
        // The second dropdown offers the two sprite sizes.
        state.set_kind(crate::drawn::DrawnKind::Sprite);
        assert_eq!(state.dropdown_options(1), &["BIG", "SML"]);
        state.pick_option(1, 1);
        assert_eq!(state.size, crate::drawn::DrawnSize::Small);
        // The third offers the four frame counts.
        assert_eq!(state.dropdown_options(2), &["1", "2", "3", "4"]);
        state.pick_option(2, 2);
        assert_eq!(state.frames_wanted, 3);
    }

    #[test]
    fn name_field_rejects_illegal_characters() {
        let mut state = state();
        state.name.clear();
        for illegal in ['A', 'Z', ' ', '/', '.', '\\', ':', 'ä'] {
            assert!(
                !state.push_name_char(illegal),
                "{illegal:?} must be refused"
            );
        }
        assert_eq!(state.name, "", "nothing illegal may be stored");
        for legal in ['a', 'z', '0', '9', '-', '_'] {
            assert!(state.push_name_char(legal), "{legal:?} must be accepted");
        }
        assert_eq!(state.name, "az09-_");
        state.pop_name_char();
        assert_eq!(state.name, "az09-");
    }

    #[test]
    fn name_field_caps_at_24_characters() {
        let mut state = state();
        state.name.clear();
        for _ in 0..30 {
            state.push_name_char('a');
        }
        assert_eq!(state.name.chars().count(), 24, "the name is capped");
        assert_eq!(state.name.chars().count(), crate::drawn::MAX_NAME_LEN);
        assert!(!state.push_name_char('b'), "a full name must refuse");
        state.pop_name_char();
        assert_eq!(state.name.chars().count(), 23);
        assert!(state.push_name_char('b'), "a freed slot takes a character");
    }

    #[test]
    fn the_name_field_is_a_placeholder() {
        let mut state = state();
        assert_eq!(state.name, "", "the field starts empty");
        assert!(
            !state.default_name.is_empty(),
            "the placeholder carries a name"
        );
        // The first character replaces the placeholder instead of being
        // appended to it.
        state.push_name_char('c');
        state.push_name_char('a');
        state.push_name_char('t');
        assert_eq!(state.name, "cat", "the placeholder must not be appended to");
        // Clearing the field brings the placeholder back: the name is
        // never empty in the field.
        state.pop_name_char();
        state.pop_name_char();
        state.pop_name_char();
        assert_eq!(state.name, "", "the field is empty again");
        // And the next character replaces the placeholder again.
        state.push_name_char('d');
        assert_eq!(state.name, "d");
    }

    #[test]
    fn the_placeholder_is_the_name_that_gets_saved() {
        let mut state = state();
        let saved: std::cell::RefCell<Option<String>> = std::cell::RefCell::new(None);
        state.primary_action_with(
            Instant::now(),
            |_, _| false,
            |_, _, name| {
                *saved.borrow_mut() = Some(name.to_string());
                Ok(std::path::PathBuf::from("x"))
            },
        );
        assert_eq!(
            saved.into_inner().expect("the document was saved"),
            state.default_name,
            "an untouched field saves the name it showed"
        );
    }

    #[test]
    fn the_name_field_shows_the_placeholder_muted() {
        let mut state = state();
        let layout = layout(1536, 864, &state);
        let cols = 1536 / UI_CELL_W as usize;
        let rows = 864 / UI_CELL_H as usize;
        let mut grid = crate::grid::Grid::new(cols, rows, UI_CELL_W, UI_CELL_H);
        paint_overlay(&mut grid, &layout, &state);
        let field_col = ui_col(layout.name_field.x) as usize + 1;
        let color = first_ink_color(&grid, field_col, 0, 20).expect("the field carries text");
        assert_eq!(color, UI_MUTED, "the placeholder reads as a hint");
        // A typed name is the content, so it is drawn bright.
        state.push_name_char('c');
        let mut grid = crate::grid::Grid::new(cols, rows, UI_CELL_W, UI_CELL_H);
        paint_overlay(&mut grid, &layout, &state);
        let color = first_ink_color(&grid, field_col, 0, 20).expect("the field carries text");
        assert_eq!(color, UI_TEXT, "the typed name is the content");
    }

    #[test]
    fn status_line_uses_the_long_labels() {
        let mut state = state();
        assert!(state.status.contains("SPRITE"), "{}", state.status);
        assert!(state.status.contains("BIG"), "{}", state.status);
        assert!(state.status.contains("STATIC"), "{}", state.status);
        state.set_kind(crate::drawn::DrawnKind::Balloon);
        assert!(state.status.contains("BALLOON"), "{}", state.status);
        state.set_kind(crate::drawn::DrawnKind::Sprite);
        state.set_size(crate::drawn::DrawnSize::Small);
        assert!(state.status.contains("SMALL"), "{}", state.status);
        state.set_frames(3);
        assert!(state.status.contains("3 FRAMES"), "{}", state.status);
    }

    #[test]
    fn the_status_line_keeps_the_size_and_the_tool_when_it_fits() {
        let state = state();
        // The long form fits the default window and names everything.
        let full = status_text(&state, &layout(1536, 864, &state));
        assert!(full.contains("BIG"), "{full}");
        assert!(full.contains("STATIC"), "{full}");
        assert!(full.contains("WHITE"), "{full}");
        // The widths are **measured**, not guessed: the status line
        // falls back one form at a time, and the window that shows each
        // form is the width that form needs.
        let medium_form = "TYPE: SPRITE  BIG  FRAME 1/1  X  WHITE";
        let compact_form = "TYPE: SPRITE  FRAME 1/1  X  WHITE";
        // A window that fits the medium form but not the long one.
        let cols = block_font_width(medium_form);
        let medium = status_text(&state, &layout(window_of(cols), 864, &state));
        assert!(medium.contains("BIG"), "{medium}");
        assert!(medium.contains("X"), "{medium}");
        assert!(!medium.contains("STATIC"), "{medium}");
        // A window that fits the compact form but not the medium one.
        let cols = block_font_width(compact_form);
        let compact = status_text(&state, &layout(window_of(cols), MIN_WINDOW_H, &state));
        assert!(compact.contains("X"), "{compact}");
        assert!(compact.contains("WHITE"), "{compact}");
        assert!(!compact.contains("BIG"), "{compact}");
        // A very narrow window keeps only the kind.
        assert_eq!(
            status_text(&state, &layout(600, 864, &state)),
            "TYPE: SPRITE"
        );
    }

    /// A window width that shows exactly `cols` overlay cells.
    fn window_of(cols: i64) -> u32 {
        (cols * i64::from(UI_CELL_W)) as u32
    }

    #[test]
    fn a_truncated_name_is_echoed_in_the_status_line() {
        // The field shows the tail of a long name (the caret must stay
        // visible); a wide window echoes the whole name in the status
        // line so it can be checked against the file on disk (audit
        // 2026-10-23 §U-06).
        let mut state = state();
        let long = "a-very-long-sprite-name";
        state.name = long.to_string();
        // The echo shares the status line with the ordinary status: with
        // the full "TYPE: …" status the line is already full, and the
        // ladder falls back to the medium form — the echo only engages
        // when the status leaves it room (a short status, as after a
        // save).
        state.status.clear();
        let layout = layout(1536, 864, &state);
        let text = status_text(&state, &layout);
        assert!(
            text.contains(&long.to_uppercase()),
            "the status line echoes the full name: {text}"
        );
        // A name that fits the field is not echoed — it is already
        // readable where it is.
        state.name = "cat".to_string();
        let text = status_text(&state, &layout);
        assert!(
            !text.contains("NAME "),
            "a fitting name needs no echo: {text}"
        );
    }

    #[test]
    fn the_assign_status_line_and_the_toast_use_the_same_dash() {
        let mut state = state();
        let mut table = no_assignments();
        let slot = crate::config::AssignmentSlot {
            kind: crate::config::SlotKind::Small,
            number: 3,
        };
        table.set(slot, Some("cat".to_string()));
        state.enter_assign_mode_with(vec![asset("cat", DrawnSize::Small)], vec![], table);
        state.select_target(slot_index(&state, "SMALL 3"));
        let text = assign_status_text(&state, 256);
        assert_eq!(text, "ASSIGN: SMALL 3 = CAT");
        // The block font carries no `=`, so the line draws the dash the
        // font does carry — exactly like the toast does.
        assert_eq!(block_font_text(&text), "ASSIGN: SMALL 3 - CAT");
        assert_eq!(
            block_font_text("ASSIGNED SMALL 3 = CAT"),
            "ASSIGNED SMALL 3 - CAT"
        );
        // The width is measured with the dash, so the line still fits.
        assert!(block_font_width(&text) <= 256);
    }

    #[test]
    fn the_overlay_carries_the_chrome() {
        let mut state = state();
        state.open_dropdown(0);
        let layout = layout(1536, 864, &state);
        let cols = 1536 / UI_CELL_W as usize;
        let rows = 864 / UI_CELL_H as usize;
        let mut grid = crate::grid::Grid::new(cols, rows, UI_CELL_W, UI_CELL_H);
        paint_overlay(&mut grid, &layout, &state);
        assert!(grid.ink_count() > 0, "the overlay must carry the chrome");
        // The first dropdown's box has its rule and its label.
        let rect = layout.dropdowns[0];
        let rule_row = (ui_row(rect.y) + RULE_UI_ROW) as usize;
        assert_eq!(
            grid.char_at(ui_col(rect.x) as usize, rule_row),
            Some('-'),
            "the dropdown box must have its rule"
        );
        let label_col = ui_col(rect.x)
            + (i64::from(rect.w) / i64::from(UI_CELL_W) - crate::hud_font::text_width("SPR")) / 2;
        assert!(
            row_has_ink(
                &grid,
                label_col as usize,
                ui_row(rect.y) as usize,
                crate::hud_font::text_width("SPR") as usize
            ),
            "the dropdown box must show its label"
        );
        // The open list is drawn under the box.
        let list_row = (ui_row(rect.bottom()) + 1) as usize;
        assert_eq!(
            grid.char_at(ui_col(rect.x) as usize, list_row),
            Some('X'),
            "the open list must be drawn"
        );
        // The status line carries the long labels.
        let status_row = ui_row(layout.status.y) as usize;
        assert_eq!(
            grid.char_at(0, status_row),
            Some('X'),
            "the status line must be visible"
        );
    }

    /// True when any pixel of the region carries RGB ink. Split from the
    /// chrome test so no loop nests a loop.
    fn region_has_ink(fb: &crate::blit::FrameBuffer, x: u32, y: u32, w: u32, h: u32) -> bool {
        (y..y + h).any(|row| {
            (x..x + w).any(|col| {
                let px = fb.pixel(col, row);
                px[0] != 0 || px[1] != 0 || px[2] != 0
            })
        })
    }

    #[test]
    fn the_chrome_png_shows_the_top_bar_and_the_status_line() {
        let state = state();
        let (w, h) = (1536u32, 864u32);
        let layout = layout(w, h, &state);
        let mut base = crate::grid::Grid::new(
            w as usize / CANVAS_CELL_W as usize,
            h as usize / CANVAS_CELL_H as usize,
            CANVAS_CELL_W,
            CANVAS_CELL_H,
        );
        let mut overlay = crate::grid::Grid::new(
            w as usize / UI_CELL_W as usize,
            h as usize / UI_CELL_H as usize,
            UI_CELL_W,
            UI_CELL_H,
        );
        paint_base(&mut base, &layout, &state);
        paint_overlay(&mut overlay, &layout, &state);
        let mut base_font = crate::font::Font::new(CANVAS_CELL_W, CANVAS_CELL_H).expect("font");
        let mut overlay_font = crate::font::Font::new(UI_CELL_W, UI_CELL_H).expect("font");
        let mut fb = crate::blit::FrameBuffer::new(w, h);
        fb.clear_black();
        // The core has no layer wrapper: the base layer goes into the
        // framebuffer first, the overlay second. (The app's renderer
        // wraps these two calls in `render::blit_layers`; the core
        // does not need the wrapper.)
        crate::blit::blit_grid_rows_between(&base, &mut base_font, &mut fb, 0, 0..base.rows());
        crate::blit::blit_grid_rows_between(
            &overlay,
            &mut overlay_font,
            &mut fb,
            0,
            0..overlay.rows(),
        );
        // The top bar and the status line carry the chrome.
        assert!(
            region_has_ink(&fb, 0, 0, w, layout.top_bar.h),
            "the top bar must be drawn"
        );
        assert!(
            region_has_ink(&fb, 0, layout.status.y as u32, w, layout.status.h),
            "the status line must be drawn"
        );
        // The empty canvas stays black: the inactive area is invisible
        // (decision D18) and an empty canvas has no ink at all. The
        // region is the canvas only — the tool panel to its right
        // carries the 19 tools and the 8 colours.
        assert!(
            !region_has_ink(&fb, 0, layout.canvas.y as u32 + 200, layout.canvas.w, 200),
            "the empty canvas must stay black"
        );
        // A PNG proof for the eye. It goes into this test's own temp
        // folder (never a fixed name in the shared temp dir) and is
        // removed again, so the suite leaves nothing behind.
        let root = temp_root("chrome-png");
        let path = root.join("agnosis-editor-chrome.png");
        crate::blit::save_png(&fb, &path).expect("png");
        assert!(path.is_file(), "the proof PNG must be written");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_base_layer_carries_the_canvas_cells() {
        let mut state = state();
        state.canvas.set(2, 1, 'x', crate::drawn::PALETTE[0]);
        // A cell below the active region must stay invisible (D18).
        state.canvas.set(2, 80, 'o', crate::drawn::PALETTE[0]);
        let layout = layout(1536, 864, &state);
        let cols = 1536 / CANVAS_CELL_W as usize;
        let rows = 864 / CANVAS_CELL_H as usize;
        let mut grid = crate::grid::Grid::new(cols, rows, CANVAS_CELL_W, CANVAS_CELL_H);
        paint_base(&mut grid, &layout, &state);
        let col = (layout.canvas.x / i64::from(CANVAS_CELL_W) + 2) as usize;
        let row = (layout.canvas.y / i64::from(CANVAS_CELL_H) + 1) as usize;
        assert_eq!(
            grid.char_at(col, row),
            Some('x'),
            "the canvas must be drawn"
        );
        let inactive = (layout.canvas.y / i64::from(CANVAS_CELL_H) + 80) as usize;
        assert_eq!(
            grid.char_at(col, inactive),
            None,
            "an inactive cell must stay invisible"
        );
    }

    #[test]
    fn the_balloon_canvas_is_drawn_on_the_disc() {
        // The paint path follows the new `(cols, rows)` canvas: a cell of
        // the disc lands on the grid, and a cell outside the disc (a
        // corner) is never painted — it is not part of the artwork.
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Balloon);
        let (cols, rows) = state.canvas_size();
        state
            .canvas
            .set(cols / 2, rows / 2, 'x', crate::drawn::PALETTE[0]);
        state.canvas.set(0, 0, 'o', crate::drawn::PALETTE[0]);
        let layout = layout(1920, 1200, &state);
        let grid_cols = 1920 / CANVAS_CELL_W as usize;
        let grid_rows = 1200 / CANVAS_CELL_H as usize;
        let mut grid = crate::grid::Grid::new(grid_cols, grid_rows, CANVAS_CELL_W, CANVAS_CELL_H);
        paint_base(&mut grid, &layout, &state);
        let col = (layout.canvas.x / i64::from(CANVAS_CELL_W) + cols as i64 / 2) as usize;
        let row = (layout.canvas.y / i64::from(CANVAS_CELL_H) + rows as i64 / 2) as usize;
        assert_eq!(
            grid.char_at(col, row),
            Some('x'),
            "the disc's centre must be drawn"
        );
        assert_ne!(
            grid.char_at(
                (layout.canvas.x / i64::from(CANVAS_CELL_W)) as usize,
                (layout.canvas.y / i64::from(CANVAS_CELL_H)) as usize
            ),
            Some('o'),
            "the corner is outside the disc and stays invisible"
        );
        // The hit test follows the same shape: the disc's centre is a hit,
        // the corner is not.
        let centre_x = layout.canvas.x + i64::from(CANVAS_CELL_W) * cols as i64 / 2;
        let centre_y = layout.canvas.y + i64::from(CANVAS_CELL_H) * rows as i64 / 2;
        assert!(
            matches!(
                hit_test(&layout, &state, centre_x, centre_y),
                EditorHit::Canvas { .. }
            ),
            "the disc's centre is paintable"
        );
        assert_eq!(
            hit_test(&layout, &state, layout.canvas.x, layout.canvas.y),
            EditorHit::None,
            "the corner is not"
        );
    }

    #[test]
    fn zero_monitor_size_is_rejected_without_opening_a_window() {
        let args = EditorArgs {
            origin: (0, 0),
            size: (0, 1080),
        };
        assert!(matches!(validate_args(&args), Err(EditorError::Config(_))));
    }

    // -----------------------------------------------------------------
    // Stage 6 — painting, hit testing, the panels and the scrolling
    // -----------------------------------------------------------------
    /// A base-layer grid for a window of `w × h` pixels.
    fn base_grid(w: u32, h: u32) -> crate::grid::Grid {
        crate::grid::Grid::new(
            w as usize / CANVAS_CELL_W as usize,
            h as usize / CANVAS_CELL_H as usize,
            CANVAS_CELL_W,
            CANVAS_CELL_H,
        )
    }

    /// A save error for the failure paths.
    fn failed() -> crate::drawn::DrawnError {
        crate::drawn::DrawnError::Io {
            path: std::path::PathBuf::from("test"),
            source: std::io::Error::other("boom"),
        }
    }

    #[test]
    fn paint_writes_the_active_tool_and_colour() {
        let mut state = state();
        state.set_tool(0);
        state.set_color(1);
        state.paint(3, 4);
        assert_eq!(
            state.canvas.get(3, 4),
            Some(('X', crate::drawn::PALETTE[1])),
            "the click must use the active tool and the active colour"
        );
        // Another tool paints another glyph — with the same colour.
        state.set_tool(2);
        state.paint(3, 5);
        assert_eq!(
            state.canvas.get(3, 5),
            Some(('.', crate::drawn::PALETTE[1]))
        );
        // Another colour paints the same glyph in another colour.
        state.set_color(4);
        state.paint(3, 6);
        assert_eq!(
            state.canvas.get(3, 6),
            Some(('.', crate::drawn::PALETTE[4]))
        );
    }

    #[test]
    fn paint_ignores_cells_outside_the_canvas() {
        let mut state = state();
        let (cols, rows) = state.canvas_size();
        state.paint(cols + 1, 0);
        state.paint(0, rows + 1);
        state.paint(usize::MAX, 0);
        assert_eq!(
            state.canvas.ink_count(),
            0,
            "nothing outside the canvas is painted"
        );
    }

    #[test]
    fn paint_ignores_inactive_balloon_cells() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Balloon);
        let (cols, rows) = state.canvas_size();
        state.paint(0, 0);
        assert_eq!(
            state.canvas.ink_count(),
            0,
            "the corner is outside the disc"
        );
        state.paint(cols - 1, rows - 1);
        assert_eq!(state.canvas.ink_count(), 0, "and so is the far corner");
        state.paint(cols / 2, rows / 2);
        assert_eq!(
            state.canvas.ink_count(),
            1,
            "the centre of the disc is paintable"
        );
    }

    #[test]
    fn paint_ignores_rows_below_the_sprite_frame() {
        let mut state = state();
        let active = state.active_rows();
        assert_eq!(active, 70, "the big duck's frame is 70 rows tall");
        state.paint(1, active);
        assert_eq!(
            state.canvas.ink_count(),
            0,
            "row 70 is below the frame and inactive"
        );
        state.paint(1, active - 1);
        assert_eq!(state.canvas.ink_count(), 1, "row 69 is the last active row");
    }

    #[test]
    fn hit_test_finds_the_tool_panel() {
        let state = state();
        let layout = layout(1536, 864, &state);
        let x = layout.tools.x + i64::from(CANVAS_CELL_W) + 4;
        let y = layout.tools.y + 4;
        assert_eq!(hit_test(&layout, &state, x, y), EditorHit::Tool(0));
        let y = layout.tools.y + i64::from(CANVAS_CELL_H) + 4;
        assert_eq!(hit_test(&layout, &state, x, y), EditorHit::Tool(1));
    }

    #[test]
    fn hit_test_finds_the_colour_panel() {
        let state = state();
        let layout = layout(1536, 864, &state);
        let row = crate::drawn::TOOLS.len() + 1;
        let x = layout.tools.x + i64::from(CANVAS_CELL_W) + 4;
        let y = layout.tools.y + (row as i64) * i64::from(CANVAS_CELL_H) + 4;
        assert_eq!(hit_test(&layout, &state, x, y), EditorHit::Color(0));
        let y = layout.tools.y + ((row + 3) as i64) * i64::from(CANVAS_CELL_H) + 4;
        assert_eq!(hit_test(&layout, &state, x, y), EditorHit::Color(3));
    }

    #[test]
    fn hit_test_finds_canvas_cells() {
        let state = state();
        let layout = layout(1536, 864, &state);
        let x = layout.canvas.x + 3 * i64::from(CANVAS_CELL_W) + 2;
        let y = layout.canvas.y + 2 * i64::from(CANVAS_CELL_H) + 3;
        assert_eq!(
            hit_test(&layout, &state, x, y),
            EditorHit::Canvas { col: 3, row: 2 }
        );
        // Below the canvas: nothing (the strip under it is the status
        // line, which is not a canvas cell).
        let below = layout.canvas.bottom() + 5;
        assert_eq!(hit_test(&layout, &state, x, below), EditorHit::None);
    }

    #[test]
    fn hit_test_respects_the_scroll_offset() {
        let mut state = state();
        state.scroll_by(5, 20);
        let layout = layout(1536, 864, &state);
        let x = layout.canvas.x + 3 * i64::from(CANVAS_CELL_W) + 2;
        let y = layout.canvas.y + 2 * i64::from(CANVAS_CELL_H) + 3;
        assert_eq!(
            hit_test(&layout, &state, x, y),
            EditorHit::Canvas { col: 3, row: 7 },
            "the scroll offset is subtracted from the row"
        );
    }

    #[test]
    fn scroll_is_clamped() {
        let mut state = state();
        let view = 10;
        state.scroll_by(-5, view);
        assert_eq!(state.scroll_rows, 0, "the scroll never goes above the top");
        state.scroll_by(1000, view);
        assert_eq!(state.scroll_rows, state.max_scroll(view));
        assert_eq!(state.max_scroll(view), state.active_rows() - view);
        assert!(
            state.scroll_rows + view <= state.active_rows(),
            "the inactive rows are never scrolled into view"
        );
    }

    #[test]
    fn default_tool_is_x_and_colour_is_white() {
        let state = state();
        assert_eq!(state.tool, crate::drawn::DEFAULT_TOOL);
        assert_eq!(crate::drawn::TOOLS[state.tool], 'x');
        assert_eq!(state.color, crate::drawn::DEFAULT_COLOR);
        assert_eq!(crate::drawn::PALETTE[state.color], [255, 255, 255]);
        assert_eq!(crate::drawn::PALETTE_NAMES[state.color], "WHITE");
    }

    #[test]
    fn the_panel_scrolls_when_the_window_is_too_short() {
        // A short window cannot show the 28 panel rows: the panel scrolls
        // instead of overflowing the window.
        let mut state = state();
        let short = layout(900, 600, &state);
        assert!(
            short.canvas_view_rows < PANEL_ROWS,
            "the short window must not fit the whole panel"
        );
        state.scroll_panel_by(100, short.canvas_view_rows);
        assert_eq!(
            state.panel_scroll,
            PANEL_ROWS - short.canvas_view_rows,
            "the panel scroll is clamped to the panel"
        );
        state.scroll_panel_by(-100, short.canvas_view_rows);
        assert_eq!(
            state.panel_scroll, 0,
            "the panel never scrolls above the top"
        );
        // A tall window fits the whole panel: there is nothing to scroll.
        // (900 px: the status strip now holds two text rows — the toast's
        // own row, audit 2026-10-23 §U-04 — so the panel needs a little
        // more height than it used to.)
        let tall = layout(1536, 900, &state);
        assert!(tall.canvas_view_rows >= PANEL_ROWS);
        state.scroll_panel_by(10, tall.canvas_view_rows);
        assert_eq!(state.panel_scroll, 0);
    }

    #[test]
    fn the_tool_panel_shows_the_tools_and_the_colours() {
        let state = state();
        let layout = layout(1536, 900, &state);
        let mut grid = base_grid(1536, 900);
        paint_base(&mut grid, &layout, &state);
        let row0 = (layout.tools.y / i64::from(CANVAS_CELL_H)) as usize;
        let marker_col = (layout.tools.x / i64::from(CANVAS_CELL_W)) as usize;
        let glyph_col = marker_col + 1;
        for (index, glyph) in crate::drawn::TOOLS.iter().enumerate() {
            assert_eq!(
                grid.char_at(glyph_col, row0 + index),
                Some(*glyph),
                "tool {index} must be in the panel"
            );
        }
        let separator = row0 + crate::drawn::TOOLS.len();
        assert_eq!(grid.char_at(glyph_col, separator), Some('-'));
        for (index, color) in crate::drawn::PALETTE.iter().enumerate() {
            let row = separator + 1 + index;
            assert_eq!(grid.char_at(glyph_col, row), Some('X'));
            assert_eq!(grid.color_at(glyph_col, row), Some(*color));
        }
        // The selected tool and the selected colour carry the marker.
        assert_eq!(grid.char_at(marker_col, row0 + state.tool), Some('>'));
        assert_eq!(
            grid.char_at(marker_col, separator + 1 + state.color),
            Some('>')
        );
    }

    #[test]
    fn clicking_a_dropdown_option_changes_the_value() {
        let mut state = state();
        let layout = layout(1536, 864, &state);
        state.open_dropdown(0);
        let rect = option_rect(&layout, 0, 1).expect("the option rect");
        assert_eq!(
            hit_test(&layout, &state, rect.x + 4, rect.y + 4),
            EditorHit::DropdownOption {
                dropdown: 0,
                option: 1
            }
        );
        // The list is only hit-testable while it is open.
        state.close_dropdown();
        assert_ne!(
            hit_test(&layout, &state, rect.x + 4, rect.y + 4),
            EditorHit::DropdownOption {
                dropdown: 0,
                option: 1
            },
            "a closed list is not a hit"
        );
    }

    // -----------------------------------------------------------------
    // Stage 7 — frames, ghost, toast and saving
    // -----------------------------------------------------------------
    #[test]
    fn primary_action_advances_to_the_next_frame() {
        let mut state = state();
        state.set_frames(3);
        state.paint(1, 1);
        let mut saved = false;
        state.primary_action_with(
            Instant::now(),
            |_, _| false,
            |_, _, _| {
                saved = true;
                Ok(std::path::PathBuf::from("x"))
            },
        );
        assert!(!saved, "the first frame is not saved yet");
        assert_eq!(state.frame_index, 1);
        assert_eq!(state.frames.len(), 1, "the finished frame is kept");
        assert!(state.ghost.is_some(), "the previous frame is the ghost");
        assert_eq!(state.canvas.ink_count(), 0, "the canvas is cleared");
        let toast = state.active_toast().expect("a toast");
        assert!(toast.contains("PLEASE DRAW SECOND POSE"), "{toast}");
    }

    #[test]
    fn primary_action_saves_the_last_frame() {
        let mut state = state();
        state.set_frames(1);
        state.paint(2, 3);
        let saved: std::cell::RefCell<Option<crate::drawn::DrawnSprite>> =
            std::cell::RefCell::new(None);
        state.primary_action_with(
            Instant::now(),
            |_, _| false,
            |doc, _, _| {
                *saved.borrow_mut() = Some(doc.clone());
                Ok(std::path::PathBuf::from("x"))
            },
        );
        let doc = saved.into_inner().expect("the document was saved");
        assert_eq!(doc.frames.len(), 1, "a static sprite has one frame");
        assert_eq!((doc.frames[0].cols(), doc.frames[0].rows()), (150, 70));
        assert_eq!(
            doc.frames[0].get(2, 3),
            Some(('x', crate::drawn::PALETTE[0]))
        );
        let toast = state.active_toast().expect("a toast");
        assert!(toast.starts_with("SAVED"), "{toast}");
    }

    /// The SAVE press **arms** the save: the `SAVING...` toast is set
    /// and the write is still owed (decision F-163).
    #[test]
    fn the_save_press_arms_the_save() {
        let mut state = state();
        state.set_frames(1);
        state.paint(2, 3);
        state.arm_save(Instant::now());
        assert!(state.armed_save, "the save is armed");
        let toast = state.active_toast().expect("a toast");
        assert_eq!(toast, "SAVING...");
        // Nothing was written yet: the document is still owed.
    }

    /// The tick completes an armed save and the toast flips to `SAVED`
    /// (decision F-163).
    #[test]
    fn the_tick_completes_an_armed_save() {
        let mut state = state();
        state.set_frames(1);
        state.paint(2, 3);
        state.arm_save(Instant::now());
        let saved: std::cell::RefCell<bool> = std::cell::RefCell::new(false);
        state.primary_action_with(
            Instant::now(),
            |_, _| false,
            |_, _, _| {
                *saved.borrow_mut() = true;
                Ok(std::path::PathBuf::from("x"))
            },
        );
        assert!(*saved.borrow(), "the save landed");
        let toast = state.active_toast().expect("a toast");
        assert!(toast.starts_with("SAVED"), "{toast}");
    }

    /// A press that means **next frame** advances and never arms.
    #[test]
    fn the_next_frame_press_never_arms_the_save() {
        let mut state = state();
        state.set_frames(2);
        state.paint(2, 3);
        state.arm_save(Instant::now());
        assert!(!state.armed_save, "next frame is not a save");
        assert_eq!(state.frame_index, 1, "the pose advanced");
    }

    #[test]
    fn ghost_is_the_previous_frame_at_twenty_percent() {
        let mut state = state();
        state.set_frames(2);
        state.set_color(1);
        state.paint(4, 5);
        state.primary_action_with(Instant::now(), |_, _| false, |_, _, _| Err(failed()));
        let ghost = state.ghost.as_ref().expect("the ghost");
        assert_eq!(ghost.get(4, 5), Some(('x', crate::drawn::PALETTE[1])));
        // 20 % of the frame colour, rounded.
        assert_eq!(dim_color(crate::drawn::PALETTE[1]), [51, 16, 16]);
        // The base layer carries the dimmed colour.
        let layout = layout(1536, 864, &state);
        let mut grid = base_grid(1536, 864);
        paint_base(&mut grid, &layout, &state);
        let col = (layout.canvas.x / i64::from(CANVAS_CELL_W) + 4) as usize;
        let row = (layout.canvas.y / i64::from(CANVAS_CELL_H) + 5) as usize;
        assert_eq!(grid.char_at(col, row), Some('x'));
        assert_eq!(grid.color_at(col, row), Some([51, 16, 16]));
    }

    #[test]
    fn painted_cells_hide_the_ghost() {
        let mut state = state();
        state.set_frames(2);
        state.set_color(1);
        state.paint(4, 5);
        state.primary_action_with(Instant::now(), |_, _| false, |_, _, _| Err(failed()));
        // The same cell, painted again in the second frame.
        state.set_color(2);
        state.paint(4, 5);
        let layout = layout(1536, 864, &state);
        let mut grid = base_grid(1536, 864);
        paint_base(&mut grid, &layout, &state);
        let col = (layout.canvas.x / i64::from(CANVAS_CELL_W) + 4) as usize;
        let row = (layout.canvas.y / i64::from(CANVAS_CELL_H) + 5) as usize;
        assert_eq!(
            grid.color_at(col, row),
            Some(crate::drawn::PALETTE[2]),
            "the painted cell wins over the ghost underneath it"
        );
    }

    #[test]
    fn toast_expires_after_the_lifetime() {
        let mut state = state();
        let now = Instant::now();
        state.set_toast_at("HELLO", now);
        assert!(toast_visible(&state.toast, now));
        assert!(toast_visible(
            &state.toast,
            now + Duration::from_millis(2499)
        ));
        assert!(
            !toast_visible(&state.toast, now + Duration::from_millis(2500)),
            "the toast is gone after 2.5 s"
        );
        assert_eq!(
            state.active_toast_at(now + Duration::from_millis(2499)),
            Some("HELLO")
        );
        assert_eq!(
            state.active_toast_at(now + Duration::from_millis(2500)),
            None
        );
    }

    #[test]
    fn save_refuses_an_existing_name() {
        let mut state = state();
        state.name = "taken".to_string();
        state.paint(3, 3);
        let mut called = false;
        state.primary_action_with(
            Instant::now(),
            |_, name| name == "taken",
            |_, _, _| {
                called = true;
                Ok(std::path::PathBuf::from("x"))
            },
        );
        assert!(!called, "an existing name is never overwritten");
        let toast = state.active_toast().expect("a toast");
        assert_eq!(toast, "NAME IN USE");
        assert_eq!(state.canvas.ink_count(), 1, "the drawing is kept");
    }

    #[test]
    fn save_keeps_the_drawing_on_failure() {
        let mut state = state();
        state.paint(3, 3);
        state.primary_action_with(Instant::now(), |_, _| false, |_, _, _| Err(failed()));
        assert!(
            state.canvas.ink_count() > 0,
            "nothing is lost when the save fails"
        );
        let toast = state.active_toast().expect("a toast");
        assert!(toast.contains("boom"), "the error is shown: {toast}");
    }

    #[test]
    fn primary_action_saves_three_frames_in_order() {
        let mut state = state();
        state.set_frames(3);
        let saved: std::cell::RefCell<Option<crate::drawn::DrawnSprite>> =
            std::cell::RefCell::new(None);
        let save = |doc: &crate::drawn::DrawnSprite, _: crate::drawn::DrawnKind, _: &str| {
            *saved.borrow_mut() = Some(doc.clone());
            Ok(std::path::PathBuf::from("x"))
        };
        state.paint(1, 1);
        state.primary_action_with(Instant::now(), |_, _| false, save);
        state.paint(2, 2);
        state.primary_action_with(Instant::now(), |_, _| false, save);
        state.paint(3, 3);
        state.primary_action_with(Instant::now(), |_, _| false, save);
        let doc = saved.into_inner().expect("the document was saved");
        assert_eq!(doc.frames.len(), 3, "all three frames are stored");
        assert_eq!(
            doc.frames[0].get(1, 1),
            Some(('x', crate::drawn::PALETTE[0]))
        );
        assert_eq!(
            doc.frames[1].get(2, 2),
            Some(('x', crate::drawn::PALETTE[0]))
        );
        assert_eq!(
            doc.frames[2].get(3, 3),
            Some(('x', crate::drawn::PALETTE[0]))
        );
    }

    #[test]
    fn a_balloon_saves_with_exactly_one_frame() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Balloon);
        state.paint(1, 1);
        let saved: std::cell::RefCell<Option<crate::drawn::DrawnSprite>> =
            std::cell::RefCell::new(None);
        state.primary_action_with(
            Instant::now(),
            |_, _| false,
            |doc, _, _| {
                *saved.borrow_mut() = Some(doc.clone());
                Ok(std::path::PathBuf::from("x"))
            },
        );
        let doc = saved.into_inner().expect("the document was saved");
        assert_eq!(doc.kind, crate::drawn::DrawnKind::Balloon);
        assert_eq!(doc.frames.len(), 1, "a balloon has exactly one frame");
        let (cols, rows) = (state.balloon_geo.cols, state.balloon_geo.rows);
        assert_eq!(
            (doc.frames[0].cols(), doc.frames[0].rows()),
            (cols, rows),
            "the saved frame is the disc"
        );
    }

    #[test]
    fn save_round_trips_through_a_temp_dir() {
        let root = temp_root("editor-save");
        let mut state = state();
        state.name = "sprite-x".to_string();
        state.paint(1, 1);
        state.primary_action_with(
            Instant::now(),
            |_, _| false,
            |doc, kind, name| crate::drawn::save_to(&root, doc, kind, name),
        );
        let path = root.join("sprite-x.json");
        assert!(path.is_file(), "the document is written");
        let loaded = crate::drawn::load_from(&root, crate::drawn::DrawnKind::Sprite, "sprite-x")
            .expect("the document loads again");
        assert_eq!(loaded.frames.len(), 1);
        assert_eq!(
            (loaded.frames[0].cols(), loaded.frames[0].rows()),
            (150, 70)
        );
        assert_eq!(
            loaded.frames[0].get(1, 1),
            Some(('x', crate::drawn::PALETTE[0]))
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_balloon_round_trips_through_a_temp_dir() {
        // The whole chain for a balloon: the editor draws it on the disc
        // canvas, saves it, and the loader reads it back. The frame that
        // comes back must be the disc — the shape the runtime draws.
        let root = temp_root("editor-balloon-save");
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Balloon);
        state.name = "balloon-x".to_string();
        let (cols, rows) = state.canvas_size();
        state.paint(cols / 2, rows / 2);
        state.primary_action_with(
            Instant::now(),
            |_, _| false,
            |doc, kind, name| crate::drawn::save_to(&root, doc, kind, name),
        );
        let path = root.join("balloon-x.json");
        assert!(path.is_file(), "the document is written");
        let loaded = crate::drawn::load_from(&root, crate::drawn::DrawnKind::Balloon, "balloon-x")
            .expect("the document loads again");
        assert_eq!(loaded.frames.len(), 1);
        assert_eq!(
            (loaded.frames[0].cols(), loaded.frames[0].rows()),
            (109, 73),
            "the frame is the disc"
        );
        assert_eq!(
            loaded.frames[0].get(cols / 2, rows / 2),
            Some(('x', crate::drawn::PALETTE[0])),
            "the painted cell survives"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------
    // Stage 9 — the ASSIGN panel
    // -----------------------------------------------------------------
    /// An empty assignment table.
    fn no_assignments() -> crate::config::AssignmentsConfig {
        crate::config::AssignmentsConfig::default()
    }

    /// An asset of the assign panel's list, with its size class.
    fn asset(name: &str, size: crate::drawn::DrawnSize) -> crate::drawn::AssetEntry {
        crate::drawn::AssetEntry {
            name: name.to_string(),
            size: Some(size),
        }
    }

    /// A sizeless asset (a balloon) of the assign panel's list.
    fn sizeless_asset(name: &str) -> crate::drawn::AssetEntry {
        crate::drawn::AssetEntry {
            name: name.to_string(),
            size: None,
        }
    }

    /// The index of the row `label` names, in the target list.
    fn slot_index(state: &EditorState, label: &str) -> usize {
        state
            .targets
            .iter()
            .position(|target| target_label(target) == label)
            .expect("the target exists")
    }

    #[test]
    fn assign_mode_lists_the_targets() {
        let mut state = state();
        state.enter_assign_mode_with(vec![], vec![], no_assignments());
        assert_eq!(state.mode, EditorMode::Assign);
        assert_eq!(
            state.targets.len(),
            ASSIGN_TARGET_COUNT,
            "the sixteen parade slots plus the three look targets"
        );
        let labels: Vec<String> = state.targets.iter().map(target_label).collect();
        assert_eq!(labels[0], "BIG 1");
        assert_eq!(labels[1], "BIG 2");
        assert_eq!(labels[2], "SMALL 1");
        assert_eq!(labels[10], "SMALL 9");
        assert_eq!(labels[11], "BALLOON 1");
        assert_eq!(labels[15], "BALLOON 5");
        assert_eq!(labels[16], "GRASS");
        assert_eq!(labels[17], "DECO BIG");
        assert_eq!(labels[18], "DECO SMALL");
    }

    #[test]
    fn assign_mode_filters_assets_by_kind() {
        let mut state = state();
        state.enter_assign_mode_with(
            vec![
                asset("cat", DrawnSize::Big),
                asset("dog", DrawnSize::Big),
                asset("pup", DrawnSize::Small),
            ],
            vec![sizeless_asset("party")],
            no_assignments(),
        );
        // A big duck takes the big sprites.
        assert_eq!(state.assign_assets().to_vec(), vec!["cat", "dog"]);
        // A balloon slot takes the balloons.
        state.select_target(slot_index(&state, "BALLOON 1"));
        assert_eq!(state.assign_assets().to_vec(), vec!["party"]);
        // A small duck takes the **small** sprites: the size class is
        // the filter, so a small sprite can never land in a big slot
        // (audit 2026-10-21 §E1).
        state.select_target(slot_index(&state, "SMALL 1"));
        assert_eq!(state.assign_assets().to_vec(), vec!["pup"]);
        // An unknown slot is refused.
        state.select_target(999);
        assert_eq!(state.assign_target, slot_index(&state, "SMALL 1"));
    }

    /// The assign panel offers a slot only the art of **its own size**.
    #[test]
    fn the_assign_panel_offers_only_the_art_of_the_slots_size() {
        let mut state = state();
        state.enter_assign_mode_with(
            vec![
                asset("cat", DrawnSize::Big),
                asset("pup", DrawnSize::Small),
                asset("kit", DrawnSize::Small),
            ],
            vec![sizeless_asset("party")],
            no_assignments(),
        );
        // BIG 1: the big sprites only.
        state.select_target(slot_index(&state, "BIG 1"));
        assert_eq!(state.assign_assets().to_vec(), vec!["cat"]);
        // SMALL 1: the small sprites only.
        state.select_target(slot_index(&state, "SMALL 1"));
        assert_eq!(state.assign_assets().to_vec(), vec!["pup", "kit"]);
    }

    #[test]
    fn assigning_writes_the_config() {
        let root = temp_root("editor-assign");
        let path = root.join("config.toml");
        let mut state = state();
        state.enter_assign_mode_with(
            vec![
                asset("cat", DrawnSize::Small),
                asset("dog", DrawnSize::Small),
            ],
            vec![],
            no_assignments(),
        );
        state.select_target(slot_index(&state, "SMALL 3"));
        state.select_asset(0);
        state.assign_action_with(Instant::now(), |write| {
            let crate::editor::AssignWrite::Assignments(table) = write else {
                return Err("not an assignment write".to_string());
            };
            let cfg = crate::config::Config {
                assignments: *table,
                ..crate::config::Config::default()
            };
            cfg.save_to(&path).map_err(|e| e.to_string())
        });
        // The config in the temp dir carries the assignment.
        let back = crate::config::Config::load_unsanitized_from(&path).expect("the config loads");
        assert_eq!(back.assignments.small(3), Some("cat"));
        assert_eq!(back.assignments.small(2), None, "the other slots stay");
        // The toast names the slot and the asset.
        let toast = state.active_toast().expect("a toast");
        assert_eq!(toast, "ASSIGNED SMALL 3 = CAT");
        // The state keeps the new table.
        assert_eq!(state.assignments.small(3), Some("cat"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn clearing_removes_the_assignment() {
        let root = temp_root("editor-clear");
        let path = root.join("config.toml");
        let slot = crate::config::AssignmentSlot {
            kind: crate::config::SlotKind::Small,
            number: 3,
        };
        let mut table = no_assignments();
        table.set(slot, Some("cat".to_string()));
        let mut state = state();
        state.enter_assign_mode_with(vec![asset("cat", DrawnSize::Small)], vec![], table);
        state.select_target(slot_index(&state, "SMALL 3"));
        state.clear_action_with(Instant::now(), |write| {
            let crate::editor::AssignWrite::Assignments(table) = write else {
                return Err("not an assignment write".to_string());
            };
            let cfg = crate::config::Config {
                assignments: *table,
                ..crate::config::Config::default()
            };
            cfg.save_to(&path).map_err(|e| e.to_string())
        });
        let back = crate::config::Config::load_unsanitized_from(&path).expect("the config loads");
        assert_eq!(back.assignments.small(3), None, "the assignment is gone");
        assert_eq!(state.assignments.small(3), None);
        let toast = state.active_toast().expect("a toast");
        assert_eq!(toast, "CLEARED SMALL 3");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The GRASS target lists the grass tiles and its assign is a
    /// single-key write into `[grass]` (decision F-162).
    #[test]
    fn the_grass_target_assigns_the_grass_tile() {
        let mut state = state();
        state.enter_assign_mode_with_assets(
            vec![],
            vec![],
            vec![sizeless_asset("lawn"), sizeless_asset("moss")],
            vec![],
            no_assignments(),
        );
        state.select_target(slot_index(&state, "GRASS"));
        assert_eq!(state.assign_assets(), ["lawn", "moss"].map(String::from));
        state.select_asset(1);
        let mut written: Option<AssignWrite> = None;
        state.assign_action_with(Instant::now(), |write| {
            written = Some(write);
            Ok(())
        });
        match written {
            Some(AssignWrite::Key {
                section,
                key,
                value,
            }) => {
                assert_eq!(section, "grass");
                assert_eq!(key, "tile");
                assert_eq!(value, "\"moss\"");
            }
            other => panic!("a grass assign is a key write, got {other:?}"),
        }
        let toast = state.active_toast().expect("a toast");
        assert_eq!(toast, "ASSIGNED GRASS = MOSS");
        // The state's own copy shows the tile as the current one.
        assert_eq!(state.settings_config.grass.tile.as_deref(), Some("moss"));
    }

    /// The DECO targets list only the decorations of their size class,
    /// and their assign is a single-key write into `[decorations]`
    /// (decision F-162).
    #[test]
    fn the_deco_targets_filter_by_size_and_write_one_key() {
        let mut state = state();
        state.enter_assign_mode_with_assets(
            vec![],
            vec![],
            vec![],
            vec![
                asset("tophat", DrawnSize::Big),
                asset("bowler", DrawnSize::Small),
            ],
            no_assignments(),
        );
        state.select_target(slot_index(&state, "DECO BIG"));
        assert_eq!(state.assign_assets(), ["tophat"].map(String::from));
        state.select_target(slot_index(&state, "DECO SMALL"));
        assert_eq!(state.assign_assets(), ["bowler"].map(String::from));
        state.select_asset(0);
        let mut written: Option<AssignWrite> = None;
        state.assign_action_with(Instant::now(), |write| {
            written = Some(write);
            Ok(())
        });
        match written {
            Some(AssignWrite::Key {
                section,
                key,
                value,
            }) => {
                assert_eq!(section, "decorations");
                assert_eq!(key, "small");
                assert_eq!(value, "\"bowler\"");
            }
            other => panic!("a deco assign is a key write, got {other:?}"),
        }
        let toast = state.active_toast().expect("a toast");
        assert_eq!(toast, "ASSIGNED DECO SMALL = BOWLER");
        assert_eq!(
            state.settings_config.decorations.small.as_deref(),
            Some("bowler")
        );
    }

    /// Clearing a look target writes the **empty** key — the same "no
    /// override" meaning an unset key has.
    #[test]
    fn clearing_a_look_target_writes_an_empty_key() {
        let mut state = state();
        state.enter_assign_mode_with_assets(
            vec![],
            vec![],
            vec![sizeless_asset("lawn")],
            vec![],
            no_assignments(),
        );
        state.select_target(slot_index(&state, "GRASS"));
        let mut written: Option<AssignWrite> = None;
        state.clear_action_with(Instant::now(), |write| {
            written = Some(write);
            Ok(())
        });
        match written {
            Some(AssignWrite::Key { section, key, .. }) => {
                assert_eq!(section, "grass");
                assert_eq!(key, "tile");
            }
            other => panic!("a grass clear is a key write, got {other:?}"),
        }
        let toast = state.active_toast().expect("a toast");
        assert_eq!(toast, "CLEARED GRASS");
        assert_eq!(state.settings_config.grass.tile, None);
    }

    #[test]
    fn switching_modes_keeps_the_drawing() {
        let mut state = state();
        state.paint(3, 4);
        state.set_frames(2);
        state.name = "sprite-x".to_string();
        let cell = state.canvas.get(3, 4);
        let ink = state.canvas.ink_count();
        state.enter_assign_mode_with(vec![], vec![], no_assignments());
        assert_eq!(state.mode, EditorMode::Assign);
        assert_eq!(state.canvas.get(3, 4), cell, "the drawing survives");
        assert_eq!(state.canvas.ink_count(), ink);
        assert_eq!(state.name, "sprite-x", "the name survives");
        state.leave_assign_mode();
        assert_eq!(state.mode, EditorMode::Draw);
        assert_eq!(state.canvas.get(3, 4), cell, "the drawing survives");
        assert_eq!(state.canvas.ink_count(), ink);
        assert_eq!(state.name, "sprite-x", "the name survives");
        assert_eq!(state.frames_wanted, 2, "the frame count survives");
    }

    #[test]
    fn the_mode_buttons_never_overlap_the_name_field() {
        let state = state();
        // The window is never narrower than the layout's floor, so the
        // test starts there.
        for width in [MIN_WINDOW_W, 1200, 1280, 1536, 1920] {
            let layout = layout(width, 864, &state);
            assert!(
                layout.name_field.right() <= layout.assign_button.x,
                "the name field must stop before ASSIGN (width {width})"
            );
            assert!(
                layout.assign_button.right() <= layout.draw_button.x,
                "ASSIGN must stop before DRAW (width {width})"
            );
            assert!(
                layout.draw_button.right() <= layout.primary_button.x,
                "DRAW must stop before SAVE (width {width})"
            );
            assert!(
                layout.name_field.x >= layout.dropdowns[2].right(),
                "the name field must start after the dropdowns (width {width})"
            );
        }
    }

    #[test]
    fn the_minimum_window_matches_the_top_bar() {
        // The floor of the layout is derived from the top bar itself, and
        // the OS enforces exactly that width.
        assert_eq!(MIN_WINDOW_W, min_layout_width());
        assert_eq!(MIN_WINDOW_W, TOP_BAR_MIN_CELLS * UI_CELL_W);
        let state = state();
        let layout = layout(MIN_WINDOW_W, MIN_WINDOW_H, &state);
        // The name field still has its minimum width at the floor.
        assert_eq!(
            layout.name_field.w,
            NAME_FIELD_MIN_UI_CELLS * UI_CELL_W,
            "the field keeps its minimum width at the floor"
        );
        // And it still ends before the mode buttons.
        assert!(
            layout.name_field.right() <= layout.assign_button.x,
            "the name field must stop before ASSIGN"
        );
    }

    #[test]
    fn the_top_bar_fits_the_dropdown_boxes() {
        // The top bar's height is derived from the dropdown box's own
        // height: the smallest whole number of base rows that covers the
        // box's UI rows. Two independent constants were a 16 px dead
        // strip waiting to drift (audit 2026-10-23 §U-01).
        let box_px = BOX_UI_ROWS * UI_CELL_H;
        let bar_px = TOP_BAR_BASE_ROWS * CANVAS_CELL_H;
        assert!(
            bar_px >= box_px,
            "the top bar ({bar_px} px) must cover a dropdown box ({box_px} px)"
        );
        assert!(
            bar_px - CANVAS_CELL_H < box_px,
            "and it is the smallest such count of base rows, not a rounded-up guess"
        );
    }

    #[test]
    fn the_toast_has_its_own_row() {
        // The toast no longer replaces the status line (audit
        // 2026-10-23 §U-04): both are drawn, on two rows of the status
        // strip, and the strip is tall enough for both — even on the
        // minimum window.
        let mut state = state();
        state.set_toast("HELLO");
        for (w, h) in [(MIN_WINDOW_W, MIN_WINDOW_H), (1536, 864)] {
            let layout = layout(w, h, &state);
            assert!(
                layout.status.h >= 11 * UI_CELL_H,
                "the strip holds two text rows ({w}x{h})"
            );
            let mut grid = crate::grid::Grid::new(
                w as usize / UI_CELL_W as usize,
                h as usize / UI_CELL_H as usize,
                UI_CELL_W,
                UI_CELL_H,
            );
            paint_overlay(&mut grid, &layout, &state);
            let status_row = (ui_row(layout.status.y) + STATUS_TEXT_UI_ROW) as usize;
            let toast_row = (ui_row(layout.status.y) + TOAST_UI_ROW) as usize;
            assert!(
                row_has_ink(&grid, 0, status_row, 8),
                "the status line is still drawn under a toast ({w}x{h})"
            );
            assert!(
                row_has_ink(&grid, 0, toast_row, 8),
                "the toast is drawn on its own row ({w}x{h})"
            );
        }
    }

    #[test]
    fn the_mode_button_labels_fit_their_buttons() {
        let state = state();
        for width in [MIN_WINDOW_W, 1200, 1280, 1536, 1920] {
            let layout = layout(width, 864, &state);
            let assign = crate::hud_font::text_width("ASSIGN") as u32;
            let draw = crate::hud_font::text_width("DRAW") as u32;
            assert!(
                layout.assign_button.w >= assign,
                "the ASSIGN label must fit its button (width {width})"
            );
            assert!(
                layout.draw_button.w >= draw,
                "the DRAW label must fit its button (width {width})"
            );
        }
    }

    #[test]
    fn the_name_field_is_never_dropped() {
        let state = state();
        let layout = layout(MIN_WINDOW_W, MIN_WINDOW_H, &state);
        assert!(
            layout.name_field.w >= NAME_FIELD_MIN_UI_CELLS * UI_CELL_W,
            "the field keeps its minimum width"
        );
        let cols = MIN_WINDOW_W as usize / UI_CELL_W as usize;
        let rows = MIN_WINDOW_H as usize / UI_CELL_H as usize;
        let mut grid = crate::grid::Grid::new(cols, rows, UI_CELL_W, UI_CELL_H);
        paint_overlay(&mut grid, &layout, &state);
        let field_col = ui_col(layout.name_field.x) as usize;
        let rule_row = (ui_row(layout.name_field.y) + RULE_UI_ROW) as usize;
        assert_eq!(
            grid.char_at(field_col, rule_row),
            Some('-'),
            "the name field must be drawn, not dropped"
        );
        assert!(
            row_has_ink(&grid, field_col, ui_row(layout.name_field.y) as usize, 8),
            "the name field must carry its text"
        );
    }

    #[test]
    fn the_top_bar_carries_the_mode_buttons() {
        let state = state();
        let layout = layout(1536, 864, &state);
        let cols = 1536 / UI_CELL_W as usize;
        let rows = 864 / UI_CELL_H as usize;
        let mut grid = crate::grid::Grid::new(cols, rows, UI_CELL_W, UI_CELL_H);
        paint_overlay(&mut grid, &layout, &state);
        assert!(
            field_has_label(&grid, &layout.assign_button, "ASSIGN"),
            "the ASSIGN button must show its label"
        );
        assert!(
            field_has_label(&grid, &layout.draw_button, "DRAW"),
            "the DRAW button must show its label"
        );
    }

    /// True when the field's label is drawn in the overlay grid.
    fn field_has_label(grid: &crate::grid::Grid, rect: &Rect, text: &str) -> bool {
        let width = crate::hud_font::text_width(text);
        let box_width = i64::from(rect.w) / i64::from(UI_CELL_W);
        let col = (ui_col(rect.x) + (box_width - width) / 2) as usize;
        let row = ui_row(rect.y) as usize;
        row_has_ink(grid, col, row, width as usize)
    }

    #[test]
    fn the_assign_panel_draws_the_slots_and_the_assets() {
        let mut state = state();
        state.enter_assign_mode_with(vec![asset("cat", DrawnSize::Big)], vec![], no_assignments());
        let layout = layout(1536, 864, &state);
        let mut grid = base_grid(1536, 864);
        paint_base(&mut grid, &layout, &state);
        let row = (layout.slot_list.y / i64::from(CANVAS_CELL_H)) as usize;
        let col = (layout.slot_list.x / i64::from(CANVAS_CELL_W) + 1) as usize;
        assert_eq!(grid.char_at(col, row), Some('B'), "the first slot is BIG 1");
        // The selected slot carries the marker.
        assert_eq!(grid.char_at(col - 1, row), Some('>'));
        // The asset name is in the asset list.
        let acol = (layout.asset_list.x / i64::from(CANVAS_CELL_W) + 1) as usize;
        assert_eq!(grid.char_at(acol, row), Some('c'), "the asset is listed");
        // The CLEAR button sits below the sixteen slots.
        let clear_row = (layout.clear_button.y / i64::from(CANVAS_CELL_H)) as usize;
        assert_eq!(grid.char_at(col, clear_row), Some('C'), "CLEAR is drawn");
    }

    #[test]
    fn an_empty_asset_list_says_no_assets_yet() {
        let mut state = state();
        state.enter_assign_mode_with(vec![], vec![], no_assignments());
        let layout = layout(1536, 864, &state);
        let mut grid = base_grid(1536, 864);
        paint_base(&mut grid, &layout, &state);
        let row = (layout.asset_list.y / i64::from(CANVAS_CELL_H)) as usize;
        let col = (layout.asset_list.x / i64::from(CANVAS_CELL_W) + 1) as usize;
        assert_eq!(grid.char_at(col, row), Some('N'), "the panel is not blank");
    }

    #[test]
    fn hit_test_finds_the_slots_the_assets_and_clear() {
        let mut state = state();
        state.enter_assign_mode_with(vec![asset("cat", DrawnSize::Big)], vec![], no_assignments());
        let layout = layout(1536, 864, &state);
        assert_eq!(
            hit_test(
                &layout,
                &state,
                layout.slot_list.x + 4,
                layout.slot_list.y + 4
            ),
            EditorHit::Slot(0)
        );
        assert_eq!(
            hit_test(
                &layout,
                &state,
                layout.asset_list.x + 4,
                layout.asset_list.y + 4
            ),
            EditorHit::Asset(0)
        );
        assert_eq!(
            hit_test(
                &layout,
                &state,
                layout.clear_button.x + 4,
                layout.clear_button.y + 4
            ),
            EditorHit::ClearButton
        );
        // The mode buttons are hits in assign mode too.
        assert_eq!(
            hit_test(
                &layout,
                &state,
                layout.assign_button.x + 4,
                layout.assign_button.y + 4
            ),
            EditorHit::AssignButton
        );
        assert_eq!(
            hit_test(
                &layout,
                &state,
                layout.draw_button.x + 4,
                layout.draw_button.y + 4
            ),
            EditorHit::DrawButton
        );
    }

    // -----------------------------------------------------------------
    // The Enter key (audit 2026-10-20 § editor, tasks 2 and 3)
    // -----------------------------------------------------------------
    #[test]
    fn enter_in_draw_mode_saves_the_drawing() {
        let mut state = state();
        state.paint(2, 3);
        let mut saved = false;
        state.enter_action_with(
            Instant::now(),
            |_, _| false,
            |_, _, _| {
                saved = true;
                Ok(std::path::PathBuf::from("x"))
            },
            |_| Ok(()),
        );
        assert!(saved, "Enter in draw mode saves the drawing");
    }

    #[test]
    fn enter_in_assign_mode_assigns_and_never_saves() {
        let mut state = state();
        state.enter_assign_mode_with(
            vec![asset("cat", DrawnSize::Small)],
            vec![],
            no_assignments(),
        );
        state.select_target(slot_index(&state, "SMALL 3"));
        let mut saved = false;
        let mut wrote = false;
        // Nothing is picked yet: Enter says so, and saves nothing.
        state.enter_action_with(
            Instant::now(),
            |_, _| false,
            |_, _, _| {
                saved = true;
                Ok(std::path::PathBuf::from("x"))
            },
            |_| {
                wrote = true;
                Ok(())
            },
        );
        assert!(!saved, "Enter in assign mode never saves the drawing");
        assert!(!wrote, "nothing is assigned without a picked asset");
        assert_eq!(
            state.active_toast().expect("a toast"),
            NO_ASSET_PICKED,
            "the status line says what is missing"
        );
        // With an asset picked, Enter assigns it — and still no save.
        state.select_asset(0);
        state.enter_action_with(
            Instant::now(),
            |_, _| false,
            |_, _, _| {
                saved = true;
                Ok(std::path::PathBuf::from("x"))
            },
            |write| {
                wrote = true;
                let crate::editor::AssignWrite::Assignments(table) = write else {
                    return Err("not an assignment write".to_string());
                };
                assert_eq!(table.small(3), Some("cat"));
                Ok(())
            },
        );
        assert!(wrote, "the assignment is written");
        assert!(!saved, "the drawing is never saved");
        let toast = state.active_toast().expect("a toast");
        assert_eq!(toast, "ASSIGNED SMALL 3 = CAT");
    }

    // -----------------------------------------------------------------
    // Extension 1 §10.1 — the grass tile
    // -----------------------------------------------------------------
    /// A grass state with a 3x2 drawing in the middle of the canvas.
    fn grass_state() -> EditorState {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Grass);
        state.paint(5, 1);
        state.paint(6, 1);
        state.paint(7, 1);
        state.paint(5, 2);
        state
    }

    #[test]
    fn the_kind_dropdown_offers_the_grass_tile() {
        let mut state = state();
        assert_eq!(state.dropdown_options(0), &["BAL", "SPR", "GRS", "DEC"]);
        assert_eq!(kind_of_option(2), crate::drawn::DrawnKind::Grass);
        assert_eq!(kind_of_option(0), crate::drawn::DrawnKind::Balloon);
        assert_eq!(kind_of_option(1), crate::drawn::DrawnKind::Sprite);
        // The box shows the short label of the current kind.
        state.set_kind(crate::drawn::DrawnKind::Grass);
        assert_eq!(state.dropdown_label(0), "GRS");
        assert_eq!(state.dropdown_long(0), "GRASS");
        assert_eq!(state.dropdown_option_index(0), 2);
    }

    #[test]
    fn the_grass_option_is_visible_and_clickable() {
        let mut state = state();
        state.open_dropdown(0);
        let layout = layout(1536, 864, &state);
        let rect = option_rect(&layout, 0, 2).expect("the third option");
        // The list hangs under the box and stays above the status line.
        assert!(
            rect.y >= layout.top_bar.bottom(),
            "the list hangs under the box"
        );
        assert!(
            rect.bottom() < layout.status.y,
            "the third option must stay above the status line"
        );
        // The click lands on the option, not on the canvas below it.
        assert_eq!(
            hit_test(&layout, &state, rect.x + 4, rect.y + 4),
            EditorHit::DropdownOption {
                dropdown: 0,
                option: 2
            },
            "the third option must be clickable"
        );
        // And it is drawn: the overlay carries the label.
        let cols = 1536 / UI_CELL_W as usize;
        let rows = 864 / UI_CELL_H as usize;
        let mut grid = crate::grid::Grid::new(cols, rows, UI_CELL_W, UI_CELL_H);
        paint_overlay(&mut grid, &layout, &state);
        let width = crate::hud_font::text_width("GRS") as usize;
        assert!(
            row_has_ink(
                &grid,
                ui_col(rect.x) as usize,
                ui_row(rect.y) as usize,
                width
            ),
            "the GRS option must be drawn"
        );
    }

    #[test]
    fn the_grass_canvas_is_two_hundred_by_four() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Grass);
        assert_eq!(state.canvas_size(), (200, 4), "the tile's canvas");
        assert_eq!(
            (state.canvas.cols(), state.canvas.rows()),
            (200, 4),
            "and the canvas holds it"
        );
        assert_eq!(state.active_rows(), 4, "every row of the tile is active");
        assert!(state.cell_is_active(0, 0), "the first cell is paintable");
        assert!(
            state.cell_is_active(199, 3),
            "and so is the last one — the tile is a band, not a disc"
        );
        // A cell outside the canvas is not paintable.
        state.paint(200, 0);
        state.paint(0, 4);
        assert_eq!(state.canvas.ink_count(), 0, "nothing outside is painted");
    }

    #[test]
    fn the_grass_size_and_frame_dropdowns_are_disabled() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Grass);
        assert!(!state.dropdown_enabled(1), "a tile has no duck size");
        assert!(!state.dropdown_enabled(2), "a tile has exactly one frame");
        // An inactive dropdown neither opens nor changes its value.
        state.open_dropdown(1);
        assert_eq!(state.open_dropdown, None, "an inactive dropdown stays shut");
        state.pick_option(2, 2);
        assert_eq!(
            state.frames_wanted, 1,
            "a tile never has more than one frame"
        );
        // The kind dropdown itself is always live.
        assert!(state.dropdown_enabled(0));
    }

    #[test]
    fn the_ink_bounding_box_crops_the_tile() {
        // A 3x2 drawing in the 200x4 canvas crops to 3x2.
        let mut canvas = crate::balloon::AsciiSprite::new(200, 4);
        canvas.set(5, 1, 'x', crate::drawn::PALETTE[0]);
        canvas.set(6, 1, 'x', crate::drawn::PALETTE[0]);
        canvas.set(7, 1, 'x', crate::drawn::PALETTE[0]);
        canvas.set(5, 2, 'x', crate::drawn::PALETTE[0]);
        let tile = crop_to_ink(&canvas);
        assert_eq!((tile.cols(), tile.rows()), (3, 2), "the tile is the ink");
        assert_eq!(tile.get(0, 0), Some(('x', crate::drawn::PALETTE[0])));
        assert_eq!(tile.get(2, 0), Some(('x', crate::drawn::PALETTE[0])));
        assert_eq!(tile.get(0, 1), Some(('x', crate::drawn::PALETTE[0])));
        assert_eq!(tile.get(2, 1), Some((' ', [255, 255, 255])));
        // An empty canvas becomes a 1x1 blank tile, never an empty one.
        let empty = crate::balloon::AsciiSprite::new(200, 4);
        let blank = crop_to_ink(&empty);
        assert_eq!((blank.cols(), blank.rows()), (1, 1));
        assert_eq!(blank.get(0, 0), Some((' ', [255, 255, 255])));
    }

    #[test]
    fn a_grass_tile_is_saved_cropped_and_alone() {
        let mut state = grass_state();
        let saved: std::cell::RefCell<Option<crate::drawn::DrawnSprite>> =
            std::cell::RefCell::new(None);
        state.primary_action_with(
            Instant::now(),
            |_, _| false,
            |doc, _, _| {
                *saved.borrow_mut() = Some(doc.clone());
                Ok(std::path::PathBuf::from("x"))
            },
        );
        let doc = saved.into_inner().expect("the document was saved");
        assert_eq!(doc.kind, crate::drawn::DrawnKind::Grass);
        assert_eq!(doc.size, None, "a tile has no duck size");
        assert_eq!(doc.frames.len(), 1, "a tile is exactly one frame");
        assert_eq!(
            (doc.frames[0].cols(), doc.frames[0].rows()),
            (3, 2),
            "the tile is the inked rectangle, not the whole canvas"
        );
        assert_eq!(
            doc.frames[0].get(2, 0),
            Some(('x', crate::drawn::PALETTE[0])),
            "the drawn cells survive"
        );
    }

    #[test]
    fn a_grass_tile_round_trips_through_a_temp_dir() {
        let root = temp_root("editor-grass-save");
        let mut state = grass_state();
        state.name = "grass-x".to_string();
        state.primary_action_with(
            Instant::now(),
            |_, _| false,
            |doc, kind, name| crate::drawn::save_to(&root, doc, kind, name),
        );
        let path = root.join("grass-x.json");
        assert!(path.is_file(), "the document is written");
        let loaded = crate::drawn::load_from(&root, crate::drawn::DrawnKind::Grass, "grass-x")
            .expect("the document loads again");
        assert_eq!(loaded.frames.len(), 1);
        assert_eq!(
            (loaded.frames[0].cols(), loaded.frames[0].rows()),
            (3, 2),
            "the tile keeps its size"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_grass_default_name_starts_with_grass() {
        // The generated name follows the kind's prefix.
        let root = temp_root("editor-grass-name");
        let name = crate::drawn::next_free_name_in(&root, crate::drawn::DrawnKind::Grass);
        assert!(name.starts_with("grass-"), "{name}");
        assert_eq!(name, "grass-1", "the first free name of the kind");
        let _ = std::fs::remove_dir_all(&root);
        // And the state offers it as the placeholder of the name field.
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Grass);
        assert!(
            state.default_name.starts_with("grass-"),
            "{}",
            state.default_name
        );
    }

    #[test]
    fn the_grass_status_line_says_the_caps_and_where_you_are() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Grass);
        state.paint(5, 1);
        state.paint(6, 1);
        state.paint(7, 1);
        state.paint(5, 2);
        state.toast = None;
        let view = layout(1536, 864, &state);
        let text = status_text(&state, &view);
        assert!(text.contains("GRASS"), "{text}");
        assert!(
            text.contains("MAX 4X200") || text.contains("MAX 4 ROWS 200 COLS"),
            "the line must name the caps: {text}"
        );
        assert!(text.contains("3X2"), "the line must name the tile: {text}");
        assert!(
            block_font_width(&text) <= i64::from(view.status.w) / i64::from(UI_CELL_W),
            "the line must fit: {text}"
        );
        // The full form spells the caps out for a wide enough window.
        let wide = layout(1920, 864, &state);
        let full = status_text(&state, &wide);
        assert!(full.contains("MAX 4 ROWS 200 COLS"), "{full}");
        assert!(full.contains("TILE"), "{full}");
        // The column range is drawn at the right of the line: the tile's
        // canvas is 200 columns wide, so the user must see where they are.
        let cols = 1536 / UI_CELL_W as usize;
        let rows = 864 / UI_CELL_H as usize;
        let mut grid = crate::grid::Grid::new(cols, rows, UI_CELL_W, UI_CELL_H);
        paint_overlay(&mut grid, &view, &state);
        let row = ui_row(view.status.y) as usize;
        let range = visible_column_range(&state, &view);
        let width = crate::hud_font::text_width(&range) as usize;
        let first = cols - width;
        assert!(
            row_has_ink(&grid, first, row, width),
            "the column range must be drawn: {range}"
        );
    }

    #[test]
    fn the_grass_size_label_is_a_tile() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Grass);
        assert_eq!(state.size_label(), "TILE");
        assert!(state.status.contains("TILE"), "{}", state.status);
    }

    #[test]
    fn the_canvas_scrolls_sideways() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Grass);
        let view = layout(1536, 864, &state);
        assert!(
            view.canvas_view_cols < 200,
            "the tile is wider than any window"
        );
        assert_eq!(
            view.max_scroll_cols,
            200 - view.canvas_view_cols,
            "the scroll reaches the last column"
        );
        // The scroll is clamped to the canvas.
        state.scroll_by_cols(1000, view.canvas_view_cols);
        assert_eq!(state.scroll_cols, view.max_scroll_cols);
        state.scroll_by_cols(-1000, view.canvas_view_cols);
        assert_eq!(state.scroll_cols, 0, "the scroll never goes past the top");
        // The layout clamps the offset too, so a stale value cannot push
        // the canvas off the screen.
        state.scroll_cols = 500;
        let clamped = layout(1536, 864, &state);
        assert_eq!(clamped.canvas_scroll_cols, clamped.max_scroll_cols);
    }

    #[test]
    fn the_horizontal_scroll_reaches_the_click_and_the_painting() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Grass);
        let view = layout(1536, 864, &state).canvas_view_cols;
        state.scroll_by_cols(10, view);
        let layout = layout(1536, 864, &state);
        assert_eq!(layout.canvas_scroll_cols, 10);
        // The click lands on the canvas column that is visible.
        let x = layout.canvas.x + 3 * i64::from(CANVAS_CELL_W) + 2;
        let y = layout.canvas.y + 2 * i64::from(CANVAS_CELL_H) + 3;
        assert_eq!(
            hit_test(&layout, &state, x, y),
            EditorHit::Canvas { col: 13, row: 2 },
            "the scroll offset is added back to the column"
        );
        // The painting only draws the visible columns.
        state.canvas.set(10, 0, 'x', crate::drawn::PALETTE[0]);
        state.canvas.set(9, 0, 'o', crate::drawn::PALETTE[0]);
        let mut grid = base_grid(1536, 864);
        paint_base(&mut grid, &layout, &state);
        let row = (layout.canvas.y / i64::from(CANVAS_CELL_H)) as usize;
        assert_eq!(
            grid.char_at(0, row),
            Some('x'),
            "the first visible column is drawn at the canvas' left edge"
        );
        assert_eq!(
            grid.char_at(usize::MAX, row),
            None,
            "a column left of the view is not drawn"
        );
    }

    #[test]
    fn a_parade_slot_never_takes_a_grass_tile() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Grass);
        state.enter_assign_mode_with(
            vec![asset("cat", DrawnSize::Small)],
            vec![sizeless_asset("party")],
            no_assignments(),
        );
        // The parade slots carry sprites and balloons only; the grass
        // tile has its own target row now (decision F-162), and its
        // list starts empty on this seam.
        assert_eq!(state.targets.len(), ASSIGN_TARGET_COUNT);
        state.select_target(slot_index(&state, "SMALL 1"));
        assert_eq!(state.assign_assets().to_vec(), vec!["cat"]);
        state.select_target(slot_index(&state, "BALLOON 1"));
        assert_eq!(state.assign_assets().to_vec(), vec!["party"]);
        state.select_target(slot_index(&state, "GRASS"));
        assert!(state.assign_assets().is_empty());
    }

    // -----------------------------------------------------------------
    // Extension 2 §2.1 — undo / redo
    // -----------------------------------------------------------------
    /// A stroke of `count` cells, painted one after the other.
    fn paint_run(state: &mut EditorState, count: usize) {
        state.begin_stroke();
        for index in 0..count {
            state.paint(index, 0);
        }
        state.end_stroke();
    }

    #[test]
    fn a_stroke_is_one_undo_step() {
        let mut state = state();
        paint_run(&mut state, 200);
        assert_eq!(
            state.undo.done.len(),
            1,
            "a 200-cell stroke is one undo step, not 200"
        );
        assert_eq!(state.undo.undone.len(), 0);
    }

    #[test]
    fn undo_and_redo_restore_the_canvas() {
        let mut state = state();
        state.set_color(1);
        paint_run(&mut state, 5);
        assert_eq!(state.canvas.get(3, 0).map(|cell| cell.0), Some('x'));
        assert!(state.undo(), "the stroke is undone");
        assert_eq!(
            state.canvas.get(3, 0),
            Some((' ', [255, 255, 255])),
            "the cell is back to blank"
        );
        assert!(state.redo(), "the stroke is redone");
        assert_eq!(
            state.canvas.get(3, 0),
            Some(('x', crate::drawn::PALETTE[1]))
        );
    }

    #[test]
    fn the_undo_restores_the_original_colour_of_a_repainted_cell() {
        let mut state = state();
        state.set_color(1);
        state.begin_stroke();
        state.paint(2, 2);
        state.set_color(2);
        state.paint(2, 2);
        state.end_stroke();
        assert_eq!(
            state.canvas.get(2, 2),
            Some(('x', crate::drawn::PALETTE[2]))
        );
        state.undo();
        assert_eq!(
            state.canvas.get(2, 2),
            Some((' ', [255, 255, 255])),
            "the cell goes back to the value it had before the stroke"
        );
        state.redo();
        assert_eq!(
            state.canvas.get(2, 2),
            Some(('x', crate::drawn::PALETTE[2]))
        );
    }

    #[test]
    fn the_stack_is_bounded() {
        let mut state = state();
        let rows = crate::drawn::sprite_active_rows(crate::drawn::DrawnSize::Big);
        for index in 0..(UNDO_LIMIT + 20) {
            state.begin_stroke();
            state.paint(index % 100, index % rows);
            state.end_stroke();
        }
        assert_eq!(
            state.undo.done.len(),
            UNDO_LIMIT,
            "the stack never grows past its bound"
        );
        // The oldest strokes are the ones that are dropped.
        let mut undone = 0;
        while state.undo() {
            undone += 1;
        }
        assert_eq!(undone, UNDO_LIMIT, "exactly the bound can be undone");
    }

    #[test]
    fn a_new_stroke_clears_the_redo_side() {
        let mut state = state();
        paint_run(&mut state, 3);
        state.undo();
        assert!(state.undo.can_redo());
        paint_run(&mut state, 2);
        assert!(
            !state.undo.can_redo(),
            "a new edit makes the undone strokes unreachable"
        );
    }

    #[test]
    fn an_undo_does_not_touch_the_name_field() {
        let mut state = state();
        state.push_name_char('c');
        state.push_name_char('a');
        state.push_name_char('t');
        paint_run(&mut state, 2);
        state.undo();
        assert_eq!(state.name, "cat", "the name field is untouched");
        state.redo();
        assert_eq!(state.name, "cat");
    }

    #[test]
    fn an_empty_undo_is_refused() {
        let mut state = state();
        assert!(!state.undo(), "there is nothing to undo");
        assert!(!state.redo(), "there is nothing to redo");
    }

    // -----------------------------------------------------------------
    // Extension 2 §2.2 — the autosave draft
    // -----------------------------------------------------------------
    #[test]
    fn the_draft_is_written_after_a_second_of_quiet() {
        let mut state = state();
        let now = Instant::now();
        state.mark_dirty_at(now);
        assert!(
            !state.draft_is_due(now),
            "the draft is not written the moment the stroke lands"
        );
        assert!(
            !state.draft_is_due(now + Duration::from_millis(500)),
            "the draft is not written while the user is still drawing"
        );
        assert!(state.draft_is_due(now + Duration::from_millis(1000)));
    }

    #[test]
    fn the_draft_carries_the_whole_drawing() {
        let mut state = state();
        state.set_frames(2);
        state.paint(4, 5);
        state.primary_action_with(Instant::now(), |_, _| false, |_, _, _| Err(failed()));
        state.paint(6, 7);
        let doc = state.draft_doc();
        assert_eq!(doc.kind, state.kind);
        assert_eq!(doc.size, Some(state.size));
        assert_eq!(doc.frames_wanted, 2);
        assert_eq!(doc.frame_index, 1);
        assert_eq!(doc.frames.len(), 1, "the finished frame is in the draft");
        assert_eq!(
            (doc.canvas.cols, doc.canvas.rows),
            (150, 150),
            "the canvas is not cropped, so the drawing resumes on the same grid"
        );
    }

    #[test]
    fn an_inked_canvas_is_unsaved_work() {
        let mut state = state();
        assert!(
            !state.has_unsaved_work(),
            "a fresh editor holds nothing worth keeping"
        );
        state.paint(4, 5);
        assert!(
            state.has_unsaved_work(),
            "the stroke is work the user has not saved"
        );
    }

    #[test]
    fn a_finished_frame_alone_is_unsaved_work() {
        let mut state = state();
        state.set_frames(2);
        state.paint(4, 5);
        state.primary_action_with(Instant::now(), |_, _| false, |_, _, _| Err(failed()));
        state.paint(6, 7);
        // The second frame is drawn; blank the canvas so only the
        // finished frame carries ink.
        state.canvas = crate::balloon::AsciiSprite::new(state.canvas.cols(), state.canvas.rows());
        assert!(
            state.has_unsaved_work(),
            "the finished frame is still work the user has not saved"
        );
    }

    #[test]
    fn the_autosave_does_not_make_the_work_saved() {
        // The debounce clears `draft_dirty` when it fires, so "not dirty"
        // means "the autosave ran" — not "the user saved the asset". The
        // exit path must not read the flag as permission to delete the
        // draft (audit 2026-10-22 §P0-1).
        let mut state = state();
        state.paint(4, 5);
        let now = Instant::now();
        state.mark_dirty_at(now);
        assert!(state.draft_is_due(now + Duration::from_millis(1000)));
        state.draft_dirty = false;
        state.draft_due = None;
        assert!(
            state.has_unsaved_work(),
            "the drawing is still unsaved after the autosave ran"
        );
    }

    #[test]
    fn the_debounce_fires_after_a_second_of_quiet() {
        // One write per second of quiet, not per cell (roadmap Extension
        // 2 §2.2): a moment short of the second is not due yet.
        let mut state = state();
        let now = Instant::now();
        assert!(!state.draft_is_due(now), "a clean state is never due");
        state.mark_dirty_at(now);
        assert!(!state.draft_is_due(now + Duration::from_millis(999)));
        assert!(state.draft_is_due(now + Duration::from_millis(1000)));
    }

    #[test]
    fn a_renamed_draft_deletes_the_old_file_before_writing_the_new() {
        // The drafts folder must not fill up with one drawing's
        // intermediate names, and the delete goes first: a kill between
        // the two actions leaves the old checkpoint, never nothing
        // (audit 101126 §C2).
        let mut state = state();
        state.name = "old".to_string();
        state.queue_draft_write();
        state.name = "new".to_string();
        state.queue_draft_write();
        let actions = state.take_draft_actions();
        assert_eq!(actions.len(), 3, "the old write, the delete, the new write");
        assert!(
            matches!(&actions[1], DraftAction::Delete { name, .. } if name.as_str() == "old"),
            "the old name is deleted"
        );
        assert!(
            matches!(&actions[2], DraftAction::Write { name, .. } if name.as_str() == "new"),
            "the new name is written last"
        );
    }

    #[test]
    fn asking_twice_for_the_same_draft_queues_no_delete() {
        let mut state = state();
        state.name = "same".to_string();
        state.queue_draft_write();
        state.queue_draft_write();
        let actions = state.take_draft_actions();
        assert!(
            actions.iter().all(|action| {
                matches!(action, DraftAction::Write { name, .. } if name.as_str() == "same")
            }),
            "the same name never queues a delete: {actions:?}"
        );
    }

    #[test]
    fn saving_under_a_new_name_deletes_the_old_draft_too() {
        // A draft written under the pre-rename name used to be orphaned
        // forever by a save under the new one — every later session
        // re-offered it (audit 101126 §B1).
        let root = temp_root("draft-rename-save");
        let mut state = state();
        state.name = "old".to_string();
        state.queue_draft_write();
        state.name = "new".to_string();
        state.paint(1, 1);
        state.primary_action_with(
            Instant::now(),
            |_, _| false,
            |sprite, kind, name| crate::drawn::save_to(&root, sprite, kind, name),
        );
        let actions = state.take_draft_actions();
        let deletes_old = actions.iter().any(
            |action| matches!(action, DraftAction::Delete { name, .. } if name.as_str() == "old"),
        );
        assert!(deletes_old, "the pre-rename draft must go with the save");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_sprite_draft_with_surplus_frames_is_truncated() {
        // Truncation used to be a balloon/grass-only mend: a sprite draft
        // with more frames than it wanted reloaded inconsistent and was
        // saved back with the surplus (audit 101126 §B3).
        let mut state = state();
        state.set_frames(2);
        state.paint(2, 2);
        let mut doc = state.draft_doc();
        let frame = crate::drawn::frame_doc(&state.canvas);
        doc.frames = vec![frame.clone(), frame.clone(), frame.clone(), frame];
        doc.frames_wanted = 2;
        state.load_draft(&doc);
        assert_eq!(state.frames_wanted, 2);
        assert_eq!(state.frames.len(), 2, "the surplus frames are dropped");
    }

    #[test]
    fn set_frames_marks_the_draft_dirty() {
        // The count and the finished frames are draft-relevant content:
        // a drop must reach the autosave now, not with the next stroke —
        // the stale draft resurrected the dropped frames on reload
        // (audit 101126 §B4).
        let mut state = state();
        state.set_frames(2);
        state.paint(4, 5);
        state.primary_action_with(Instant::now(), |_, _| false, |_, _, _| Err(failed()));
        state.set_frames(1);
        // `set_frames` marks dirty with the state's own clock, so the
        // probe is taken after the mark.
        let probe = Instant::now() + Duration::from_millis(1000);
        assert!(
            state.draft_is_due(probe),
            "the frame drop must reach the autosave"
        );
        assert_eq!(state.frames.len(), 1);
    }

    #[test]
    fn the_frame_split_marks_the_draft_dirty() {
        let mut state = state();
        state.set_frames(2);
        state.paint(3, 3);
        let now = Instant::now();
        state.primary_action_with(now, |_, _| false, |_, _, _| Err(failed()));
        assert!(
            state.draft_is_due(now + Duration::from_millis(1000)),
            "the frame split must reach the autosave"
        );
    }

    #[test]
    fn a_second_end_stroke_is_a_no_op() {
        // The pointer-up can be lost (a focus change, a capture grab); a
        // second close must not turn two gestures into one undo step
        // (audit 101126 §C1).
        let mut state = state();
        state.begin_stroke();
        state.paint(1, 1);
        state.end_stroke();
        state.end_stroke();
        assert!(!state.stroke_is_open());
        assert!(state.undo(), "the first flush put the stroke on the stack");
        assert!(!state.undo(), "and only one stroke landed");
        let now = Instant::now();
        assert!(
            state.draft_is_due(now + Duration::from_millis(1000)),
            "the flush marked the draft dirty"
        );
    }

    #[test]
    fn undo_closes_an_open_stroke_first() {
        // A stroke still open when undo is asked for belongs to that
        // undo: its wet cells go onto the stack instead of staying on
        // the canvas un-undoable (audit 101126 §C1).
        let mut state = state();
        state.begin_stroke();
        state.paint(2, 2);
        let ink = state.canvas.get(2, 2);
        assert!(ink.is_some_and(|(ch, _)| ch != ' '), "the paint wet ink");
        assert!(state.undo(), "the wet ink is what undo removes");
        assert!(!state.stroke_is_open(), "the stroke closed with the undo");
        assert!(
            state.canvas.get(2, 2).is_none_or(|(ch, _)| ch == ' '),
            "the wet ink is gone"
        );
        assert!(state.redo(), "the wet stroke can come back");
        assert_eq!(state.canvas.get(2, 2), ink);
    }

    #[test]
    fn loading_a_draft_drops_the_open_stroke() {
        // The draft replaces the canvas wholesale, so the open stroke's
        // deltas describe a canvas that no longer exists: they die with
        // the old one, and no half-stroke survives the load (audit
        // 101126 §C1).
        let mut state = state();
        state.begin_stroke();
        state.paint(1, 1);
        let doc = state.draft_doc();
        state.load_draft(&doc);
        assert!(!state.stroke_is_open());
        assert!(
            !state.undo(),
            "the old strokes do not describe the new canvas"
        );
    }

    #[test]
    fn the_draft_survives_a_restart() {
        let root = temp_root("draft-restart");
        let mut state = state();
        state.paint(3, 3);
        state.paint(4, 4);
        let doc = state.draft_doc();
        let name = "sprite-restart";
        crate::drawn::save_draft_to(&root, &doc, state.kind, name).expect("the draft is written");

        // A new editor picks the draft up.
        let loaded = crate::drawn::load_draft_from(&root, state.kind, name)
            .expect("the draft loads")
            .expect("the draft is there");
        let mut fresh = self::state();
        fresh.load_draft(&loaded);
        assert_eq!(fresh.canvas.get(3, 3), state.canvas.get(3, 3));
        assert_eq!(fresh.canvas.get(4, 4), state.canvas.get(4, 4));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_balloon_draft_loads_as_a_single_frame() {
        // A balloon is one frame, always — a draft that claims otherwise
        // (a hand-edited file, an older build) is mended on load instead
        // of putting the editor in a state the kind cannot save (audit
        // 2026-10-23 §R6-05).
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Balloon);
        state.paint(5, 5);
        let mut doc = state.draft_doc();
        doc.kind = crate::drawn::DrawnKind::Balloon;
        doc.frames_wanted = 4;
        doc.frame_index = 3;
        let frame = crate::drawn::frame_doc(&state.canvas);
        doc.frames = vec![frame.clone(), frame.clone()];
        state.load_draft(&doc);
        assert_eq!(state.frames_wanted, 1, "a balloon has one frame");
        assert_eq!(state.frame_index, 0, "and the index follows");
        assert!(state.frames.len() <= 1, "the surplus frames are dropped");
        // A grass tile is mended the same way.
        doc.kind = crate::drawn::DrawnKind::Grass;
        doc.frames_wanted = 4;
        state.load_draft(&doc);
        assert_eq!(state.frames_wanted, 1, "a tile has one frame");
        // A sprite's frame count is the draft's own — sprites animate.
        doc.kind = crate::drawn::DrawnKind::Sprite;
        doc.size = Some(crate::drawn::DrawnSize::Big);
        doc.frames_wanted = 3;
        state.load_draft(&doc);
        assert_eq!(state.frames_wanted, 3, "a sprite keeps its frames");
    }

    #[test]
    fn the_draft_offer_can_be_declined() {
        // The offer used to be unanswerable in the negative (audit
        // 2026-10-23 §U-27): ESC declines it, the file stays.
        let mut state = state();
        state.offer_draft("sprite-x");
        assert_eq!(state.draft_offer.as_deref(), Some("sprite-x"));
        state.decline_draft();
        assert_eq!(state.draft_offer, None, "the offer is gone");
        let status = status_text(&state, &layout(1536, 864, &state));
        assert!(
            !status.contains("DRAFT FOUND"),
            "the status line is freed: {status}"
        );
    }

    #[test]
    fn the_draft_is_atomic() {
        let root = temp_root("draft-atomic");
        let state = state();
        let doc = state.draft_doc();
        crate::drawn::save_draft_to(&root, &doc, state.kind, "sprite-atomic").expect("the write");
        // No temp file may survive: the write went through a rename.
        let leftovers: Vec<String> = std::fs::read_dir(&root)
            .expect("read the dir")
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no temp file may survive: {leftovers:?}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn saving_the_asset_removes_the_draft() {
        let root = temp_root("draft-save");
        let mut state = state();
        // The name field decides the file name, so the draft and the
        // asset are written under the same name.
        state.push_name_char('s');
        state.push_name_char('a');
        state.push_name_char('v');
        state.push_name_char('e');
        state.push_name_char('d');
        state.paint(1, 1);
        let doc = state.draft_doc();
        let name = "saved";
        crate::drawn::save_draft_to(&root, &doc, state.kind, name).expect("the draft is written");
        assert!(root.join("sprite-saved.draft.json").is_file());

        state.primary_action_with(
            Instant::now(),
            |_, _| false,
            |sprite, kind, name| crate::drawn::save_to(&root, sprite, kind, name),
        );
        let actions = state.take_draft_actions();
        let deletes: Vec<&DraftAction> = actions
            .iter()
            .filter(|action| matches!(action, DraftAction::Delete { .. }))
            .collect();
        assert_eq!(deletes.len(), 1, "the save queues the draft's deletion");

        // The app carries the action out; the app's
        // `carry_out_draft_action` is a thin wrapper around these two
        // core calls, so the core test calls them directly.
        for action in &actions {
            let DraftAction::Delete { kind, name } = action else {
                continue;
            };
            crate::drawn::delete_draft_from(&root, *kind, name).expect("the draft is deleted");
        }
        assert!(
            !root.join("sprite-saved.draft.json").is_file(),
            "saving the asset removes the draft"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_drafts_are_listed_newest_first() {
        let root = temp_root("draft-newest");
        let state = state();
        let doc = state.draft_doc();
        for name in ["old", "new"] {
            crate::drawn::save_draft_to(&root, &doc, state.kind, name).expect("the write");
        }
        let entries = crate::drawn::list_drafts(&root, state.kind);
        assert_eq!(entries.len(), 2, "both drafts are found");
        // The order is total: the newest first, and the name breaks a tie
        // on the file time, so the offer is never ambiguous.
        let sorted = entries.windows(2).all(|pair| {
            pair[0].modified > pair[1].modified
                || (pair[0].modified == pair[1].modified && pair[0].name < pair[1].name)
        });
        assert!(sorted, "the drafts are ordered newest first: {entries:?}");
        let _ = std::fs::remove_dir_all(&root);
    }

    // -----------------------------------------------------------------
    // Extension 2 §4 — the duck decorations
    // -----------------------------------------------------------------
    #[test]
    fn the_kind_dropdown_offers_the_decoration() {
        let mut state = state();
        assert_eq!(state.dropdown_options(0), &["BAL", "SPR", "GRS", "DEC"]);
        assert_eq!(kind_of_option(3), crate::drawn::DrawnKind::Decoration);
        state.set_kind(crate::drawn::DrawnKind::Decoration);
        assert_eq!(state.dropdown_label(0), "DEC");
        assert_eq!(state.dropdown_long(0), "DECORATION");
        assert_eq!(state.dropdown_option_index(0), 3);
    }

    #[test]
    fn a_decoration_is_authored_on_the_ducks_frame() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Decoration);
        // The canvas is the duck's own square canvas and the active
        // region is the duck's frame.
        assert_eq!(state.canvas_size(), (150, 150));
        assert_eq!(state.active_rows(), 70);
        assert!(state.cell_is_active(10, 69), "the last frame row is active");
        assert!(
            !state.cell_is_active(10, 70),
            "the rows below the frame are not"
        );
        // A decoration has the duck's size and the duck's frame count.
        assert!(state.dropdown_enabled(1), "the size dropdown is live");
        assert!(state.dropdown_enabled(2), "the frame count is live");
    }

    #[test]
    fn the_decoration_underlay_shows_the_built_in_duck() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Decoration);
        let layout = layout(1536, 864, &state);
        let mut grid = base_grid(1536, 864);
        paint_base(&mut grid, &layout, &state);
        // The underlay is the built-in art at 20 % brightness: the duck's
        // own glyph is on the grid, dimmed.
        let col = (layout.canvas.x / i64::from(CANVAS_CELL_W)) as usize;
        let row = (layout.canvas.y / i64::from(CANVAS_CELL_H)) as usize;
        let mut found = false;
        for offset in 0..crate::sprites::BIG_DUCK_ROWS {
            if grid.char_at(col + 10, row + offset).unwrap_or(' ') == ' ' {
                continue;
            }
            assert_eq!(
                grid.color_at(col + 10, row + offset),
                Some(dim_color(crate::scene::DUCK_COLOR)),
                "the underlay is the dimmed built-in art"
            );
            found = true;
            break;
        }
        assert!(found, "the built-in duck is shown under the decoration");
    }

    #[test]
    fn a_decoration_keeps_its_frames() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Decoration);
        state.set_frames(2);
        state.paint(4, 5);
        state.primary_action_with(Instant::now(), |_, _| false, |_, _, _| Err(failed()));
        state.paint(6, 7);
        let document = state.document();
        assert_eq!(document.kind, crate::drawn::DrawnKind::Decoration);
        assert_eq!(document.size, Some(crate::drawn::DrawnSize::Big));
        assert_eq!(
            document.frames.len(),
            2,
            "a decoration's frames step in step with the body's"
        );
        assert_eq!(
            (document.frames[0].cols(), document.frames[0].rows()),
            (150, 70),
            "a decoration's frame is the duck's frame"
        );
    }

    #[test]
    fn a_parade_slot_never_takes_a_decoration() {
        let mut state = state();
        state.set_kind(crate::drawn::DrawnKind::Decoration);
        state.enter_assign_mode_with(
            vec![asset("cat", DrawnSize::Small)],
            vec![sizeless_asset("party")],
            no_assignments(),
        );
        // The parade slots carry sprites and balloons; a decoration has
        // its own target rows now (decision F-162), and their lists
        // start empty on this seam.
        assert_eq!(state.targets.len(), ASSIGN_TARGET_COUNT);
        state.select_target(slot_index(&state, "SMALL 1"));
        assert_eq!(state.assign_assets().to_vec(), vec!["cat"]);
        state.select_target(slot_index(&state, "DECO BIG"));
        assert!(state.assign_assets().is_empty());
    }

    // -----------------------------------------------------------------
    // Extension 2 §3 — the settings panel
    // -----------------------------------------------------------------
    /// The setting row with the given key, if the panel offers it.
    fn setting_row(key: &str) -> Option<&'static SettingRow> {
        SETTING_ROWS.iter().find(|row| row.key == key)
    }

    /// A state in settings mode, with the default config.
    fn settings_state() -> EditorState {
        let mut state = state();
        state.enter_settings_mode_with(crate::config::Config::default());
        state
    }

    #[test]
    fn the_settings_panel_is_a_third_mode() {
        let mut state = state();
        assert_eq!(state.mode, EditorMode::Draw);
        state.enter_settings_mode_with(crate::config::Config::default());
        assert_eq!(state.mode, EditorMode::Settings);
        // The mode switch never touches the drawing.
        state.paint(4, 4);
        state.leave_settings_mode();
        assert_eq!(state.mode, EditorMode::Draw);
        assert_eq!(state.canvas.get(4, 4).map(|cell| cell.0), Some('x'));
    }

    #[test]
    fn the_settings_rows_are_data() {
        // Every row is a plain table entry: a label, a section, a key, a
        // unit and a kind. The panel is built from the table, not from
        // the config struct (roadmap Extension 2 §3.1).
        for row in SETTING_ROWS {
            assert!(!row.label.is_empty(), "a row needs a label");
            assert!(!row.key.is_empty(), "a row needs a key");
            // An action row has no value to read — the verb is the
            // display — and its key names the action, not a config key.
            if let SettingKind::Action(verb) = row.kind {
                assert!(!verb.is_empty(), "an action row needs its verb");
                continue;
            }
            let config = crate::config::Config::default();
            assert!(
                read_setting(&config, row).is_some(),
                "the config must carry {}",
                row.key
            );
        }
        // The keys are unique: two rows writing the same key would
        // fight over it.
        let mut keys: Vec<&str> = SETTING_ROWS.iter().map(|row| row.key).collect();
        keys.sort_unstable();
        let count = keys.len();
        keys.dedup();
        assert_eq!(keys.len(), count, "the setting keys are unique");
    }

    #[test]
    fn the_settings_panel_lists_the_documented_keys() {
        // Every key the README documents is on the panel.
        for key in [
            "loop_seconds",
            "fps_cap",
            "max_cols",
            "exit_hint",
            "hint_corner",
            "monitor",
            "max",
            "balloon_radius_rows",
            "rope_min_small",
            "rope_min_big",
            "rope_max",
            "pendulum_g",
            "pendulum_damp",
            "pendulum_drive",
            "pendulum_period",
            "text",
            "position",
            "fade_seconds",
            "enabled",
            "time_of_day",
            "mode",
            "intensity",
            "seed",
            "tile",
            "show_days",
            "restore_on_close",
            "windows",
            "macos",
            "linux",
            "editor",
            "volume",
            "muted",
            "waveform",
        ] {
            assert!(setting_row(key).is_some(), "the panel must offer {key}");
        }
    }

    /// The PERSONALITY actions are on the panel as two action rows
    /// (roadmap HELP window §2, decision F-161).
    #[test]
    fn the_personality_actions_are_on_the_panel() {
        let randomise = setting_row("randomise").expect("a randomise row");
        let reset = setting_row("reset").expect("a reset row");
        assert_eq!(randomise.label, "PERSONALITY");
        assert_eq!(reset.label, "PERSONALITY");
        assert!(matches!(randomise.kind, SettingKind::Action("RANDOMISE")));
        assert!(matches!(reset.kind, SettingKind::Action("RESET")));
    }

    /// Enter on an action row runs the action, not a staged value.
    #[test]
    fn activating_an_action_row_runs_the_action() {
        let mut state = state();
        let row = SETTING_ROWS
            .iter()
            .position(|r| r.key == "randomise")
            .expect("the row");
        state.settings_cursor = row;
        let ran = state.run_settings_action_with(row, Instant::now(), |key| {
            assert_eq!(key, "randomise");
            Ok(())
        });
        assert!(ran, "the action ran");
        assert_eq!(
            state.toast.as_ref().map(|(text, _)| text.as_str()),
            Some("PERSONALITY RANDOMISED")
        );
    }

    /// A failed action leaves everything as it was and the toast
    /// explains why — the change_setting_with discipline.
    #[test]
    fn a_failed_action_toasts_the_reason() {
        let mut state = state();
        let row = SETTING_ROWS
            .iter()
            .position(|r| r.key == "reset")
            .expect("the row");
        state.settings_cursor = row;
        let ran = state
            .run_settings_action_with(row, Instant::now(), |_| Err("disk is full".to_string()));
        assert!(!ran, "the action did not land");
        assert_eq!(
            state.toast.as_ref().map(|(text, _)| text.as_str()),
            Some("SAVE FAILED: disk is full")
        );
    }

    /// A value row is not an action: the runner is never called.
    #[test]
    fn a_value_row_has_no_action_to_run() {
        let mut state = state();
        let ran = state.run_settings_action_with(0, Instant::now(), |_| {
            panic!("a value row must not reach the runner");
        });
        assert!(!ran, "a value row has no action");
    }

    #[test]
    fn the_audio_settings_are_reachable() {
        // The sound's settings are on the panel, so the user never has to
        // hand-edit the config for the volume, the mute or the voice.
        let mut config = crate::config::Config::default();
        let volume = setting_row("volume").expect("a volume row");
        let muted = setting_row("muted").expect("a mute row");
        let voice = setting_row("waveform").expect("a voice row");
        assert_eq!(
            read_setting(&config, volume),
            Some(SettingValue::Float(0.7))
        );
        assert_eq!(
            read_setting(&config, muted),
            Some(SettingValue::Bool(false))
        );
        assert_eq!(
            read_setting(&config, voice),
            Some(SettingValue::Choice(1)),
            "the default voice is the triangle"
        );
        apply_setting(&mut config, volume, &SettingValue::Float(0.25));
        apply_setting(&mut config, muted, &SettingValue::Bool(true));
        apply_setting(&mut config, voice, &SettingValue::Choice(2));
        assert_eq!(config.audio.volume, 0.25);
        assert!(config.audio.muted);
        assert_eq!(config.audio.waveform, "square");
    }

    #[test]
    fn the_legacy_key_is_not_offered() {
        // `cell_size` is legacy and ignored: the panel must not offer it
        // (roadmap Extension 2 §3.4).
        assert!(setting_row("cell_size").is_none());
        assert!(!SETTING_ROWS.iter().any(|row| row.key == "cell_size"));
    }

    #[test]
    fn writing_one_setting_only_touches_that_key() {
        let mut state = settings_state();
        let mut written: Vec<(String, String, String)> = Vec::new();
        let changed = state.change_setting_with(
            0,
            SettingValue::Float(12.5),
            Instant::now(),
            |section, key, value| {
                written.push((section.to_string(), key.to_string(), value.to_string()));
                Ok(())
            },
        );
        assert!(changed, "the value is written");
        assert_eq!(written.len(), 1, "only one key is written");
        assert_eq!(written[0].0, "", "the top level is the empty section");
        assert_eq!(written[0].1, "loop_seconds");
        assert_eq!(written[0].2, "12.5", "the value is rendered as TOML");
        // The panel shows the new value.
        assert_eq!(
            state.settings_value(0),
            Some(SettingValue::Float(12.5)),
            "the panel shows what was written"
        );
    }

    #[test]
    fn a_failed_write_keeps_the_old_value() {
        let mut state = settings_state();
        let before = state.settings_value(0);
        let changed =
            state.change_setting_with(0, SettingValue::Float(12.5), Instant::now(), |_, _, _| {
                Err("the disk is full".to_string())
            });
        assert!(!changed, "the write failed");
        assert_eq!(
            state.settings_value(0),
            before,
            "the panel keeps the old value"
        );
        let toast = state.active_toast().expect("a toast");
        assert!(toast.starts_with("SAVE FAILED"), "{toast}");
        assert!(toast.contains("the disk is full"), "{toast}");
    }

    #[test]
    fn a_slider_value_is_clamped_to_its_range() {
        let state = settings_state();
        // The fps cap row: 5 ..= 120.
        let row = SETTING_ROWS
            .iter()
            .position(|row| row.key == "fps_cap")
            .expect("the fps row");
        let value = state.slider_value(row, 2.0).expect("a value");
        assert_eq!(value, SettingValue::Int(120), "the value is clamped");
        let value = state.slider_value(row, -1.0).expect("a value");
        assert_eq!(value, SettingValue::Int(5), "the value is clamped");
        let value = state.slider_value(row, 0.0).expect("a value");
        assert_eq!(value, SettingValue::Int(5));
    }

    #[test]
    fn a_float_is_written_with_a_decimal_point() {
        // A TOML integer does not parse into an f32, so the value must
        // keep its point.
        let row = setting_row("loop_seconds").expect("the row");
        let value = SettingValue::Float(20.0);
        assert_eq!(render_setting_value(row, &value), "20.0");
        assert_eq!(
            render_setting_value(row, &SettingValue::Float(0.35)),
            "0.35"
        );
    }

    #[test]
    fn a_choice_is_written_as_its_name() {
        let row = setting_row("hint_corner").expect("the row");
        let value = SettingValue::Choice(1);
        assert_eq!(render_setting_value(row, &value), "\"bottom-right\"");
    }

    /// The smallest value of every settings row must survive the repair
    /// pass.
    ///
    /// WHY: the panel and the repair pass used to have **independent**
    /// bounds, so six rows could write a value the repair immediately
    /// rewrote — the setting was lost, the slider jumped back under the
    /// user and the panel showed a number the app refused to honour
    /// (audit 2026-10-21 §F7).
    #[test]
    fn every_settings_row_minimum_survives_the_repair_pass() {
        let config = crate::config::Config::default();
        for row in SETTING_ROWS {
            let Some(minimum) = row_minimum(row) else {
                continue;
            };
            let clamped = clamp_setting(row, minimum, &config);
            let mut written = config.clone();
            apply_setting(&mut written, row, &clamped);
            let before = read_setting(&written, row);
            let repaired = written.sanitized_with_reporting(|_, _| true, &mut Vec::new());
            assert_eq!(
                read_setting(&repaired, row),
                before,
                "the repair pass rewrote the {} row ({})",
                row.label,
                row.key
            );
        }
    }

    /// The smallest value a row can take, or `None` for a row that has
    /// no numeric range.
    fn row_minimum(row: &SettingRow) -> Option<SettingValue> {
        match row.kind {
            SettingKind::Int { min, .. } => Some(SettingValue::Int(min)),
            SettingKind::Float { min, .. } => Some(SettingValue::Float(min)),
            _ => None,
        }
    }

    /// The range the **repair pass** accepts for a row, or `None` when
    /// the repair pass does not touch the key.
    ///
    /// WHY: the panel's range and the repair's range are two rules, and
    /// the panel must not offer a value the repair rewrites — so the
    /// panel's range must lie **inside** the repair's (audit
    /// 2026-10-21 §F7, §E).
    fn repair_range(row: &SettingRow) -> Option<(f64, f64)> {
        use crate::config::bounds;
        match (row.section, row.key) {
            ("", "max_cols") => Some((bounds::MAX_COLS_MIN as f64, bounds::MAX_COLS_MAX as f64)),
            ("", "fps_cap") => Some((bounds::FPS_CAP_MIN as f64, bounds::FPS_CAP_MAX as f64)),
            ("", "loop_seconds") => Some((
                bounds::LOOP_SECONDS_MIN as f64,
                bounds::LOOP_SECONDS_MAX as f64,
            )),
            ("balloons", "max") => Some((
                bounds::BALLOONS_MAX_MIN as f64,
                bounds::BALLOONS_MAX_MAX as f64,
            )),
            ("balloons", "balloon_radius_rows") => {
                Some((1.0, bounds::BALLOON_RADIUS_ROWS_MAX as f64))
            }
            ("balloons", "rope_min_small") => Some((
                bounds::ROPE_MIN_SMALL_MIN as f64,
                bounds::ROPE_MIN_SMALL_MAX as f64,
            )),
            ("balloons", "rope_min_big") => Some((
                bounds::ROPE_MIN_SMALL_MIN as f64,
                bounds::ROPE_MIN_BIG_MAX as f64,
            )),
            ("balloons", "rope_max") => Some((
                bounds::ROPE_MIN_SMALL_MIN as f64,
                bounds::ROPE_MAX_MAX as f64,
            )),
            ("balloons", "pendulum_g") => Some((bounds::PENDULUM_G_MIN as f64, f64::INFINITY)),
            ("balloons", "pendulum_damp") => Some((0.0, f64::INFINITY)),
            ("balloons", "pendulum_drive") => Some((
                bounds::PENDULUM_DRIVE_MIN as f64,
                bounds::PENDULUM_DRIVE_MAX as f64,
            )),
            ("balloons", "pendulum_period") => Some((0.0, f64::INFINITY)),
            ("message", "fade_seconds") => Some((0.0, f64::INFINITY)),
            ("sky", "time_of_day") => Some((0.0, 1.0)),
            ("weather", "intensity") => Some((0.0, 2.0)),
            ("audio", "volume") => Some((0.0, 1.0)),
            _ => None,
        }
    }

    /// Every row's range lies **inside** the range the repair pass
    /// accepts.
    #[test]
    fn every_settings_row_range_is_inside_the_repair_range() {
        for row in SETTING_ROWS {
            let (min, max) = match row.kind {
                SettingKind::Int { min, max } => (min as f64, max as f64),
                SettingKind::Float { min, max } => (min as f64, max as f64),
                _ => continue,
            };
            let Some((repair_min, repair_max)) = repair_range(row) else {
                continue;
            };
            assert!(
                min >= repair_min && max <= repair_max,
                "the {} row's range {min}..={max} is outside the repair's {repair_min}..={repair_max}",
                row.label
            );
        }
    }

    #[test]
    fn the_settings_panel_is_hit_testable() {
        let state = settings_state();
        let layout = layout(1536, 864, &state);
        // The first row's value cell is a slider.
        let value = settings_value_rect(layout.settings, 0);
        let x = value.x + i64::from(CANVAS_CELL_W);
        let y = value.y + 1;
        assert_eq!(
            hit_test(&layout, &state, x, y),
            EditorHit::SettingSlider(0),
            "the slider is hit"
        );
        // A toggle row is a toggle.
        let row = SETTING_ROWS
            .iter()
            .position(|row| row.key == "show_days")
            .expect("the row");
        let visible = row - layout.settings_scroll;
        let value = settings_value_rect(layout.settings, visible);
        let x = value.x + i64::from(CANVAS_CELL_W);
        let y = value.y + 1;
        assert_eq!(
            hit_test(&layout, &state, x, y),
            EditorHit::SettingToggle(row)
        );
        // The label column is not interactive: a click there must not
        // move a slider.
        let label = settings_label_rect(layout.settings, 0);
        assert_eq!(
            hit_test(&layout, &state, label.x + 1, label.y + 1),
            EditorHit::None
        );
    }

    #[test]
    fn the_settings_panel_paints_its_rows() {
        let state = settings_state();
        let layout = layout(1536, 864, &state);
        let mut grid = base_grid(1536, 864);
        paint_base(&mut grid, &layout, &state);
        // The header is there.
        let header = Rect::new(
            layout.settings.x + i64::from(CANVAS_CELL_W),
            layout.settings.y,
            layout.settings.w,
            CANVAS_CELL_H,
        );
        assert!(
            grid_region_has_ink(&grid, header),
            "the settings header is drawn"
        );
        // The first row's label is there.
        let label = settings_label_rect(layout.settings, 0);
        assert!(
            grid_region_has_ink(&grid, label),
            "the first label is drawn"
        );
    }

    /// True when any cell of the rect carries ink.
    ///
    /// The rect is in **pixels**; the grid is indexed in cells, so the
    /// pixel position is converted first.
    fn grid_region_has_ink(grid: &crate::grid::Grid, rect: Rect) -> bool {
        let col = (rect.x.max(0) / i64::from(CANVAS_CELL_W)) as usize;
        let row = (rect.y.max(0) / i64::from(CANVAS_CELL_H)) as usize;
        let cols = (rect.w / CANVAS_CELL_W).max(1) as usize;
        (0..2).any(|offset| {
            (col..col + cols).any(|c| grid.char_at(c, row + offset).unwrap_or(' ') != ' ')
        })
    }

    #[test]
    fn a_text_field_is_focused_and_committed() {
        let mut state = settings_state();
        let row = SETTING_ROWS
            .iter()
            .position(|row| row.key == "exit_hint")
            .expect("the row");
        state.focus_text_row(row);
        assert_eq!(state.settings_focus, Some(row));
        // The field shows the current value as a placeholder until the
        // user types; the first character replaces it.
        assert_eq!(state.settings_text, "");
        state.push_settings_text("BYE");
        assert_eq!(state.settings_text, "BYE");
        state.pop_settings_text();
        assert_eq!(state.settings_text, "BY");
        // The commit writes the field's value.
        let mut written: Vec<String> = Vec::new();
        state.change_setting_with(
            row,
            SettingValue::Text(state.settings_text.clone()),
            Instant::now(),
            |_, key, value| {
                written.push(format!("{key}={value}"));
                Ok(())
            },
        );
        assert_eq!(written, vec!["exit_hint=\"BY\"".to_string()]);
    }

    #[test]
    fn the_settings_cursor_stays_on_a_row() {
        let mut state = settings_state();
        state.move_settings_cursor(-5);
        assert_eq!(state.settings_cursor, 0, "the cursor stops at the top");
        state.move_settings_cursor(1000);
        assert_eq!(
            state.settings_cursor,
            SETTING_ROWS.len() - 1,
            "the cursor stops at the bottom"
        );
    }

    #[test]
    fn a_toggle_row_steps_by_flipping() {
        let state = settings_state();
        let row = SETTING_ROWS
            .iter()
            .position(|row| matches!(row.kind, SettingKind::Bool))
            .expect("the panel has a toggle");
        let SettingValue::Bool(before) = state.settings_value(row).expect("the value") else {
            panic!("the row is a toggle");
        };
        let stepped = state.stepped_setting_value(row, 1).expect("a toggle steps");
        assert_eq!(stepped, SettingValue::Bool(!before), "one step flips it");
        let back = state
            .stepped_setting_value(row, -1)
            .expect("a toggle steps both ways");
        assert_eq!(
            back,
            SettingValue::Bool(!before),
            "a flip does not care about the direction"
        );
    }

    #[test]
    fn a_choice_row_steps_and_wraps() {
        let state = settings_state();
        let row = SETTING_ROWS
            .iter()
            .position(|row| matches!(row.kind, SettingKind::Choice(_)))
            .expect("the panel has a choice");
        let SettingValue::Choice(index) = state.settings_value(row).expect("the value") else {
            panic!("the row is a choice");
        };
        let stepped = state.stepped_setting_value(row, 1).expect("a choice steps");
        assert_eq!(
            stepped,
            SettingValue::Choice((index + 1) % SETTING_ROWS[row].kind.options().len()),
            "one step, the next option"
        );
        let wrapped = state
            .stepped_setting_value(row, -1)
            .expect("a choice steps both ways");
        let len = SETTING_ROWS[row].kind.options().len();
        let expected = (index as i64 - 1).rem_euclid(len as i64) as usize;
        assert_eq!(
            wrapped,
            SettingValue::Choice(expected),
            "stepping left wraps around the option list"
        );
    }

    #[test]
    fn a_text_row_has_nothing_to_step() {
        let state = settings_state();
        let row = SETTING_ROWS
            .iter()
            .position(|row| matches!(row.kind, SettingKind::Text))
            .expect("the panel has a text field");
        assert!(
            state.stepped_setting_value(row, 1).is_none(),
            "a text row is stepped by typing, not by the arrows"
        );
    }

    #[test]
    fn a_slider_row_steps_inside_its_range() {
        let state = settings_state();
        let row = SETTING_ROWS
            .iter()
            .position(|row| matches!(row.kind, SettingKind::Int { .. }))
            .expect("the panel has a slider");
        let SettingValue::Int(before) = state.settings_value(row).expect("the value") else {
            panic!("the row is an int slider");
        };
        let SettingValue::Int(after) = state.stepped_setting_value(row, 1).expect("a slider steps")
        else {
            panic!("the stepped value is an int");
        };
        assert!(
            after > before,
            "one step right increases: {before} -> {after}"
        );
        let (_, max) = match SETTING_ROWS[row].kind {
            SettingKind::Int { min: _, max } => (before, max),
            _ => unreachable!(),
        };
        assert!(after <= max, "the step stays inside the range");
    }

    #[test]
    fn settings_step_stages_the_value_of_the_cursor_row() {
        // The keyboard's step (audit 2026-10-23 §X-01): the value is
        // staged, not applied — the app writes it, and a failed write
        // leaves the panel on the old value.
        let mut state = settings_state();
        let row = SETTING_ROWS
            .iter()
            .position(|row| matches!(row.kind, SettingKind::Choice(_)))
            .expect("the panel has a choice");
        state.settings_cursor = row;
        assert!(state.settings_step(1), "a choice row steps");
        let (staged_row, staged_value) = state
            .take_staged_setting()
            .expect("the step staged a value");
        assert_eq!(staged_row, row);
        assert_eq!(
            staged_value,
            state
                .stepped_setting_value(row, 1)
                .expect("the stepped value")
        );
        assert!(
            state.take_staged_setting().is_none(),
            "taking the staged value clears it"
        );
        // A text row has nothing to step.
        let text_row = SETTING_ROWS
            .iter()
            .position(|row| matches!(row.kind, SettingKind::Text))
            .expect("the panel has a text field");
        state.settings_cursor = text_row;
        assert!(!state.settings_step(1), "a text row does not step");
    }

    #[test]
    fn settings_activate_toggles_flips_and_opens() {
        let mut state = settings_state();
        // A Bool row's activate stages the flip — whatever the default
        // is (the sky is on by default, the day counter is off).
        let toggle = SETTING_ROWS
            .iter()
            .position(|row| matches!(row.kind, SettingKind::Bool))
            .expect("the panel has a toggle");
        state.settings_cursor = toggle;
        let before = state.settings_value(toggle).expect("the value");
        state.settings_activate();
        let (_, staged) = state.take_staged_setting().expect("the flip is staged");
        assert_eq!(
            staged,
            state
                .stepped_setting_value(toggle, 1)
                .expect("the flip of the current value"),
            "activate on a toggle flips it"
        );
        assert_ne!(staged, before);
        // A Choice row's activate opens the list, and opens it again to
        // close it.
        let choice = SETTING_ROWS
            .iter()
            .position(|row| matches!(row.kind, SettingKind::Choice(_)))
            .expect("the panel has a choice");
        state.settings_cursor = choice;
        state.settings_activate();
        assert_eq!(state.settings_open, Some(choice), "the list opens");
        state.settings_activate();
        assert_eq!(state.settings_open, None, "and the second press closes it");
        // A Text row's activate focuses the field.
        let text_row = SETTING_ROWS
            .iter()
            .position(|row| matches!(row.kind, SettingKind::Text))
            .expect("the panel has a text field");
        state.settings_cursor = text_row;
        state.settings_activate();
        assert_eq!(
            state.settings_focus,
            Some(text_row),
            "the field takes the focus"
        );
    }

    #[test]
    fn every_choice_row_round_trips_every_option() {
        // Pins the option list against the enum's serde rename (audit
        // 2026-10-23 §R6-02): a label that does not parse is an option
        // the panel can never write, and the four enum arms
        // (hint_corner, monitor, position, weather mode) compile here.
        for setting in SETTING_ROWS {
            let options = setting.kind.options();
            if options.is_empty() {
                continue;
            }
            for (picked, option) in options.iter().enumerate() {
                let mut config = crate::config::Config::default();
                apply_setting(&mut config, setting, &SettingValue::Choice(picked));
                assert_eq!(
                    read_setting(&config, setting),
                    Some(SettingValue::Choice(picked)),
                    "{} option {} did not round-trip",
                    setting.key,
                    option
                );
            }
        }
    }

    #[test]
    fn a_toggle_is_displayed_as_on_and_off() {
        // The old echo was `[X]`/`[ ]`, whose brackets the block font
        // cannot draw — the echo rendered blank (audit 2026-10-23
        // §U-25).
        let row = setting_row("show_days").expect("the row");
        assert_eq!(
            SettingValue::Bool(true).display(row),
            "ON",
            "the toggle's echo is spelled out"
        );
        assert_eq!(SettingValue::Bool(false).display(row), "OFF");
        // The status line carries the echo through the block font.
        let mut state = settings_state();
        let toggle = SETTING_ROWS
            .iter()
            .position(|row| row.key == "show_days")
            .expect("the row");
        state.settings_cursor = toggle;
        let text = settings_status_text(&state, 256);
        assert!(text.contains("OFF"), "the echo is readable: {text}");
        assert!(block_font_width(&text) <= 256, "and it fits");
    }

    #[test]
    fn a_choice_is_displayed_as_its_option_name() {
        // The value is the option's name, not the index — the index is
        // a table detail the user never chose to see (audit 2026-10-23
        // §U-03).
        let row = setting_row("mode").expect("the weather row");
        assert_eq!(
            SettingValue::Choice(0).display(row),
            "off",
            "the choice's display is its option"
        );
    }

    #[test]
    fn the_settings_status_line_names_the_panel() {
        // The panel replaced the canvas, so the line says which of the
        // four screens it describes (audit 2026-10-23 §X-21).
        let state = settings_state();
        let text = settings_status_text(&state, 256);
        assert!(
            text.starts_with("CFG - "),
            "the line names the panel: {text}"
        );
    }

    #[test]
    fn the_settings_header_names_the_visible_slice() {
        // A short window shows a slice of the 35 rows; the header says
        // which one, so a list that continues past the bottom does not
        // look like it ends there (audit 2026-10-23 §U-13).
        let state = settings_state();
        let short = layout(1536, MIN_WINDOW_H, &state);
        assert!(
            short.settings_visible < SETTING_ROWS.len(),
            "the short window must scroll"
        );
        let header = settings_header_text(&short);
        assert!(
            header.contains(&format!(
                "ROWS 1-{} OF {}",
                short.settings_visible,
                SETTING_ROWS.len()
            )),
            "the header names the slice: {header}"
        );
        // A window that fits every row does not need the affordance.
        let tall = layout(1536, 1200, &state);
        if tall.settings_visible >= SETTING_ROWS.len() {
            assert!(
                !settings_header_text(&tall).contains("ROWS"),
                "no scrolling, no affordance: {}",
                settings_header_text(&tall)
            );
        }
    }

    #[test]
    fn the_settings_cursor_carries_a_marker() {
        // Colour alone signalled the cursor; the `>` in the marker
        // column is the colour-blind-safe signal (audit 2026-10-23
        // §U-14).
        let state = settings_state();
        let layout = layout(1536, 864, &state);
        let mut grid = base_grid(1536, 864);
        paint_base(&mut grid, &layout, &state);
        let marker_col = (layout.settings.x / i64::from(CANVAS_CELL_W)) as usize;
        let marker_row = (layout.settings.y / i64::from(CANVAS_CELL_H) + 1) as usize;
        assert_eq!(
            grid.char_at(marker_col, marker_row),
            Some('>'),
            "the cursor's row carries the marker"
        );
    }

    #[test]
    fn the_settings_option_list_is_clamped_to_the_panel() {
        // The weather row's six options must not run past the panel's
        // bottom into the status strip (audit 2026-10-23 §U-26): with
        // the row near the bottom of a short window, some options are
        // clamped away — and paint and hit agree, because they share one
        // rect function.
        let mut state = settings_state();
        let weather = SETTING_ROWS
            .iter()
            .position(|row| row.key == "mode")
            .expect("the weather row");
        state.settings_open = Some(weather);
        // Scroll so the weather row sits near the bottom of the window:
        // the list has room for a few options, not all six.
        state.settings_scroll = 10;
        let short = layout(1536, MIN_WINDOW_H, &state);
        let mut rected = 0;
        for option in 0..6 {
            let Some(rect) = settings_option_rect(&short, &state, option) else {
                continue;
            };
            rected += 1;
            assert!(
                rect.bottom() <= short.settings.bottom(),
                "option {option} runs past the panel"
            );
            let hit = settings_option_hit(&short, &state, rect.x + 1, rect.y + 1);
            assert_eq!(
                hit,
                Some(EditorHit::SettingOption {
                    row: weather,
                    option
                }),
                "paint and hit agree on option {option}"
            );
        }
        assert!(
            rected < 6,
            "a row near the panel's bottom cannot hold all six options"
        );
        // A row scrolled out of view has no list at all.
        state.settings_open = Some(SETTING_ROWS.len() - 1);
        state.settings_scroll = 0;
        let visible = SETTING_ROWS.len() - 1 - short.settings_scroll;
        if visible >= short.settings_visible {
            assert_eq!(
                settings_option_rect(&short, &state, 0),
                None,
                "an off-screen row's list has no rects"
            );
        }
    }

    #[test]
    fn fit_base_text_truncates_with_a_dash() {
        assert_eq!(fit_base_text("BALLOON 5", 13), "BALLOON 5");
        assert_eq!(fit_base_text("a-long-track-name", 7), "a-long-");
        assert_eq!(fit_base_text("cat", 1), "-");
        assert_eq!(fit_base_text("cat", 0), "", "zero room draws nothing");
    }

    // -----------------------------------------------------------------
    // Extension 3 §3.2 — the note strip
    // -----------------------------------------------------------------
    /// A state in strip mode, with a strip of `steps` steps.
    fn strip_state(steps: usize) -> EditorState {
        let mut config = crate::config::Config::default();
        config.audio.strip_steps = steps;
        let mut state = state();
        state.enter_sequencer_mode_with(config, None, Vec::new());
        state
    }

    /// The default strip: 263 steps, the length of the crossing window.
    fn default_strip_state() -> EditorState {
        strip_state(263)
    }

    #[test]
    fn the_strip_is_three_rows_tall_and_long() {
        let state = strip_state(300);
        assert_eq!(
            state.canvas_size(),
            (300, 3),
            "the strip is as long as the config says and three rows tall: the caption, the lane and the ruler (audit 2026-10-23 §U-16)"
        );
        assert_eq!(state.active_rows(), 3, "all three rows belong to the strip");
        assert_eq!(STRIP_ROWS, 3, "caption, lane and ruler");
        // The drawing keeps its own grid behind the strip.
        assert_eq!(state.drawing_canvas_size().0, 150);
    }

    #[test]
    fn the_strip_scrolls_horizontally() {
        let mut state = default_strip_state();
        let view = 40;
        let max = state.max_scroll_cols(view);
        assert_eq!(max, 263 - view, "the scroll reaches the end of the strip");
        state.scroll_by_cols(5, view);
        assert_eq!(state.scroll_cols, 5, "the strip moves sideways");
        state.scroll_by_cols(-10, view);
        assert_eq!(state.scroll_cols, 0, "the scroll clamps at the start");
        state.scroll_by_cols(10_000, view);
        assert_eq!(state.scroll_cols, max, "the scroll clamps at the end");
    }

    #[test]
    fn a_click_places_a_note_and_a_second_click_removes_it() {
        let mut state = default_strip_state();
        state.place_note(7);
        assert_eq!(state.track.len(), 1, "the note is on the strip");
        let note = state.track.get(&7).expect("the note");
        assert_eq!(note.pitch, PITCH_MIDI[0], "the note has the selected pitch");
        assert_eq!(note.duration_steps, 1, "a note rings for one step");
        state.place_note(7);
        assert!(
            state.track.is_empty(),
            "a second click removes the note again"
        );
    }

    #[test]
    fn a_note_can_be_undone() {
        let mut state = default_strip_state();
        state.place_note(3);
        assert!(state.undo_note(), "the note is undone");
        assert!(state.track.is_empty(), "the strip is empty again");
        assert!(state.redo_note(), "the note comes back");
        assert_eq!(state.track.len(), 1);
        // The history is bounded like the canvas's.
        for step in 0..UNDO_LIMIT + 10 {
            state.place_note(step);
        }
        assert!(state.strip_undo.can_undo());
    }

    #[test]
    fn a_track_is_saved_and_loaded() {
        let root = temp_root("editor-track");
        let mut state = default_strip_state();
        state.track_name = "march".to_string();
        state.select_pitch(4);
        state.place_note(1);
        state.place_note(9);
        let (name, track) = state.track_save_request().expect("a save request");
        crate::audio::sequencer::save_track(&root, &name, &track).expect("the track is written");
        let loaded = crate::audio::sequencer::load_track(&root, &name).expect("the track loads");
        assert_eq!(loaded, track, "the notes survive the round trip");
        assert_eq!(loaded.get(&9).map(|note| note.pitch), Some(PITCH_MIDI[4]));

        // The editor loads the file back into the strip.
        let mut other = default_strip_state();
        let outcome = TrackOutcome::Loaded {
            name: name.clone(),
            track: loaded,
        };
        other.apply_track_outcome(&outcome, Instant::now());
        assert_eq!(other.track.len(), 2, "the strip holds the loaded notes");
        assert_eq!(other.track_name, "march");
        assert_eq!(
            other.track_sync,
            TrackSync::Synced,
            "the strip holds what the file holds"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_ruler_marks_the_loop_start_and_end() {
        let mut state = default_strip_state();
        let first = layout(1536, 864, &state);
        let mut grid = base_grid(1536, 864);
        paint_base(&mut grid, &first, &state);
        let row = (first.canvas.y / i64::from(CANVAS_CELL_H)) as usize;
        let col = (first.canvas.x / i64::from(CANVAS_CELL_W)) as usize;
        let ruler = row + STRIP_RULER_ROW;
        assert_eq!(
            grid.char_at(col, ruler),
            Some(RULER_START),
            "the first slot is the start of the loop"
        );
        assert_eq!(
            grid.char_at(col + 1, ruler),
            Some(RULER_TICK),
            "the other slots are ticks"
        );
        // The end of the loop is at the far right: the strip scrolls.
        state.scroll_by_cols(1000, first.canvas_view_cols);
        let scrolled = layout(1536, 864, &state);
        let mut grid = base_grid(1536, 864);
        paint_base(&mut grid, &scrolled, &state);
        let last = state.strip_steps - 1 - scrolled.canvas_scroll_cols;
        assert_eq!(
            grid.char_at(col + last, ruler),
            Some(RULER_END),
            "the last slot is the end of the loop"
        );
    }

    #[test]
    fn the_palette_selects_the_pitch() {
        let mut state = default_strip_state();
        state.select_pitch(3);
        assert_eq!(state.pitch_name(), "D#3");
        assert_eq!(state.pitch_midi(), PITCH_MIDI[3]);
        state.place_note(2);
        assert_eq!(
            state.track.get(&2).map(|note| note.pitch),
            Some(PITCH_MIDI[3]),
            "the placed note has the selected pitch"
        );
        // The palette is hit testable, and the third row is the fourth
        // pitch.
        let layout = layout(1536, 864, &state);
        let row = layout.tools.y + 3 * i64::from(CANVAS_CELL_H) + 1;
        let hit = hit_test(&layout, &state, layout.tools.x + 8, row);
        assert_eq!(hit, EditorHit::Pitch(3));
    }

    #[test]
    fn a_sharp_pitch_is_never_shown_as_its_natural_neighbour() {
        // The block font has no `#`, so the true name `C#3` rendered as
        // `C3` — the wrong pitch. The status echo uses the `-` alias
        // (audit 2026-10-23 §U-24); the palette keeps the true name.
        let mut state = default_strip_state();
        state.select_pitch(1);
        assert_eq!(state.pitch_name(), "C#3", "the true name keeps the sharp");
        assert_eq!(state.pitch_display(), "C-3", "the display alias spells it");
        let status = state.strip_status();
        assert!(
            status.contains("C-3"),
            "the strip echo uses the alias: {status}"
        );
        assert!(
            !status.contains("C3 ") && !status.ends_with(" C3"),
            "the sharp never renders as the plain natural: {status}"
        );
        // Every sharp of the octave is aliased, and every natural keeps
        // its own name.
        for (index, alias) in PITCH_DISPLAYS.iter().enumerate() {
            state.select_pitch(index);
            assert_eq!(state.pitch_display(), *alias);
            assert!(
                !state.pitch_display().contains('#'),
                "the block font cannot draw the sharp"
            );
        }
    }

    #[test]
    fn the_fourth_mode_button_grows_the_floor() {
        // The floor carries the fourth button.
        assert!(
            min_layout_width() >= TOP_BAR_MIN_CELLS * UI_CELL_W,
            "the floor is derived from the top bar"
        );
        assert!(
            min_layout_width() < 1366,
            "the floor must still fit a 1366 px screen ({} px)",
            min_layout_width()
        );
        let state = state();
        let layout = layout(MIN_WINDOW_W, MIN_WINDOW_H, &state);
        assert!(
            layout.name_field.right() <= layout.settings_button.x,
            "the name field must stop before CFG"
        );
        assert!(
            layout.settings_button.right() <= layout.track_button.x,
            "CFG must stop before TRK"
        );
        assert!(
            layout.track_button.right() <= layout.assign_button.x,
            "TRK must stop before ASSIGN"
        );
        assert!(
            layout.assign_button.right() <= layout.draw_button.x,
            "ASSIGN must stop before DRAW"
        );
        // The label fits its button.
        assert!(layout.track_button.w as i64 >= crate::hud_font::text_width("TRK"));
    }

    #[test]
    fn the_strip_keeps_the_drawing() {
        let mut state = state();
        state.paint(2, 3);
        let before = state.canvas.ink_count();
        state.enter_sequencer_mode_with(crate::config::Config::default(), None, Vec::new());
        assert_eq!(state.canvas.ink_count(), before, "the drawing survives");
        state.leave_sequencer_mode();
        assert_eq!(state.canvas.ink_count(), before, "and it is still there");
    }

    #[test]
    fn the_track_name_is_typed_in_the_strip_mode() {
        let mut state = default_strip_state();
        assert!(state.push_track_name_char('m'));
        assert!(state.push_track_name_char('a'));
        assert!(
            !state.push_track_name_char('!'),
            "an illegal name is refused"
        );
        assert_eq!(state.track_name, "ma");
        state.pop_track_name_char();
        assert_eq!(state.track_name, "m");
        // A name the track folder cannot hold never reaches the disk.
        state.track_name = "../evil".to_string();
        assert!(state.track_save_request().is_none());
    }

    #[test]
    fn the_enter_key_points_the_parade_at_the_track() {
        let mut state = default_strip_state();
        state.track_name = "march".to_string();
        state.enter_action_with(
            Instant::now(),
            |_, _| false,
            |_, _, _| Ok(std::path::PathBuf::from("x")),
            |_| Ok(()),
        );
        let request = state.take_track_request().expect("a request");
        assert_eq!(
            request,
            TrackRequest::Select {
                name: "march".to_string()
            }
        );
    }

    #[test]
    fn a_failed_track_write_keeps_the_strip() {
        let mut state = default_strip_state();
        state.place_note(4);
        let outcome = TrackOutcome::Failed {
            action: "SAVE",
            reason: "disk on fire".to_string(),
        };
        state.apply_track_outcome(&outcome, Instant::now());
        assert_eq!(state.track.len(), 1, "the notes are still there");
        assert_eq!(
            state.track_sync,
            TrackSync::Dirty,
            "the strip does not claim to be saved"
        );
        let toast = state.active_toast().expect("a toast");
        assert_eq!(toast, "SAVE FAILED: disk on fire");
    }

    #[test]
    fn unsaved_notes_are_visible_to_the_exit_path() {
        // An empty strip is not unsaved work; notes without a file are
        // (audit 2026-10-23 §X-04).
        let mut state = default_strip_state();
        assert!(!state.track_unsaved(), "a fresh strip holds nothing");
        state.place_note(3);
        assert!(state.track_unsaved(), "the note is unsaved work");
        // A saved strip is saved, even with notes on it.
        state.apply_track_outcome(
            &TrackOutcome::Saved {
                name: "march".to_string(),
            },
            Instant::now(),
        );
        assert!(
            !state.track_unsaved(),
            "the file holds what the strip holds"
        );
        // Editing again makes it unsaved once more.
        state.place_note(5);
        assert!(state.track_unsaved());
    }

    #[test]
    fn the_track_sync_machine_moves_fresh_dirty_synced() {
        // The declared machine (audit 101126 §C3): the strip starts
        // Fresh, editing makes it Dirty, a save or a load makes it
        // Synced, and another edit makes it Dirty again. A failed write
        // leaves the machine where it was.
        let mut state = default_strip_state();
        assert_eq!(state.track_sync, TrackSync::Fresh);
        state.place_note(1);
        assert_eq!(state.track_sync, TrackSync::Dirty);
        assert!(state.track_unsaved(), "dirty notes are unsaved work");
        state.apply_track_outcome(
            &TrackOutcome::Saved {
                name: "march".to_string(),
            },
            Instant::now(),
        );
        assert_eq!(state.track_sync, TrackSync::Synced);
        state.place_note(2);
        assert_eq!(state.track_sync, TrackSync::Dirty);
        state.apply_track_outcome(
            &TrackOutcome::Loaded {
                name: "march".to_string(),
                track: crate::audio::Track::new(),
            },
            Instant::now(),
        );
        assert_eq!(state.track_sync, TrackSync::Synced);
        state.place_note(3);
        state.apply_track_outcome(
            &TrackOutcome::Failed {
                action: "SAVE",
                reason: "disk on fire".to_string(),
            },
            Instant::now(),
        );
        assert_eq!(state.track_sync, TrackSync::Dirty);
    }

    #[test]
    fn a_pending_track_save_is_flushed_on_the_caller() {
        // The exit path's guarantee (audit 2026-10-23 §X-04): a SAVE
        // pressed a moment before ESC is carried out synchronously, and
        // the strip no longer claims unsaved notes.
        let root = temp_root("editor-track-flush");
        let mut state = default_strip_state();
        state.set_tracks_dir(Some(root.clone()));
        state.track_name = "march".to_string();
        state.place_note(2);
        assert!(state.request_track_save(), "the save is queued");
        assert!(state.track_unsaved(), "and the strip knows it");
        assert!(state.flush_track_request(), "the flush carried it out");
        assert!(
            crate::audio::sequencer::load_track(&root, "march").is_ok(),
            "the file is on disk"
        );
        assert!(!state.track_unsaved(), "the flush marked it saved");
        assert!(
            state
                .active_toast()
                .is_some_and(|toast| toast.contains("SAVED")),
            "the outcome toast says what happened"
        );
        // A second flush finds no pending request and does nothing.
        assert!(!state.flush_track_request(), "nothing left to flush");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_configured_track_is_loaded_when_the_strip_opens() {
        let root = temp_root("editor-track-load");
        let mut track = crate::audio::Track::new();
        track.insert(
            4,
            crate::audio::Note {
                pitch: 60,
                duration_steps: 1,
                velocity: 100,
            },
        );
        crate::audio::sequencer::save_track(&root, "march", &track).expect("the track is written");
        let mut config = crate::config::Config::default();
        config.audio.track = Some("march".to_string());
        let mut state = state();
        state.enter_sequencer_mode_with(config, Some(root.clone()), vec!["march".to_string()]);
        assert_eq!(state.track.len(), 1, "the configured track is loaded");
        assert_eq!(state.track_name, "march", "and it names the field");
        assert_eq!(state.tracks, vec!["march".to_string()]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_strip_keeps_its_notes_when_the_mode_is_entered_again() {
        let mut state = default_strip_state();
        state.place_note(6);
        state.leave_sequencer_mode();
        state.enter_sequencer_mode_with(crate::config::Config::default(), None, Vec::new());
        assert_eq!(state.track.len(), 1, "the notes survive the mode switch");
    }

    #[test]
    fn the_strip_paints_its_caption_lane_and_palette() {
        let mut state = default_strip_state();
        state.select_pitch(2);
        state.place_note(5);
        let layout = layout(1536, 864, &state);
        let mut grid = base_grid(1536, 864);
        paint_base(&mut grid, &layout, &state);
        let row = (layout.canvas.y / i64::from(CANVAS_CELL_H)) as usize;
        let col = (layout.canvas.x / i64::from(CANVAS_CELL_W)) as usize;
        // The caption names the lane on the canvas itself (§U-16).
        assert_eq!(
            grid.char_at(col, row + STRIP_CAPTION_ROW),
            Some('N'),
            "the caption row is drawn"
        );
        assert_eq!(
            grid.char_at(col + 5, row + STRIP_NOTE_ROW),
            Some(NOTE_GLYPH),
            "the note is drawn on the lane"
        );
        // Only the lane takes clicks: the caption and the ruler do not.
        let lane_y = layout.canvas.y + i64::from(CANVAS_CELL_H) * STRIP_NOTE_ROW as i64 + 1;
        let caption_y = layout.canvas.y + 1;
        let x = layout.canvas.x + 5 * i64::from(CANVAS_CELL_W) + 1;
        assert_eq!(hit_test(&layout, &state, x, lane_y), EditorHit::NoteCell(5));
        assert_eq!(
            hit_test(&layout, &state, x, caption_y),
            EditorHit::None,
            "the caption row is chrome, not a click"
        );
        // The palette shows the pitch names.
        let palette_col = (layout.tools.x / i64::from(CANVAS_CELL_W) + 1) as usize;
        let palette_row = (layout.tools.y / i64::from(CANVAS_CELL_H)) as usize;
        assert_eq!(grid.char_at(palette_col, palette_row), Some('C'));
        assert_eq!(grid.char_at(palette_col + 1, palette_row), Some('3'));
    }
}
