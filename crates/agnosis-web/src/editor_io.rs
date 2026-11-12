//! The draw-your-own editor, in a browser tab (roadmap
//! `REVIEW/ROADMAP_WASM.md` §6, milestone M3).
//!
//! The editor's **pure half** is the core's [`agnosis_core::editor`]: the
//! state machine, the layout, the hit test, the stroke-delta undo, the
//! settings rows and the paint helpers. This file is the **application
//! half** the browser needs, and it ports the native one
//! (`src/editor.rs`) line for line where the platform matches and
//! replaces it where the platform differs:
//!
//! | Native | Browser |
//! | --- | --- |
//! | a winit window, 80 % of the monitor | the page's `#editor-canvas`,
//!   never below the layout floor ([`agnosis_core::editor::MIN_WINDOW_W`]
//!   × [`agnosis_core::editor::MIN_WINDOW_H`]) |
//! | the wgpu surface | the plain 2D canvas: one `putImageData` per
//!   frame, no GPU negotiation — the editor is UI, not a parade |
//! | the drawn folders | the page's **shared** store
//!   ([`crate::assets_io::SharedStore`]): a save is in the parade's
//!   store the moment it lands |
//! | `drawn_save` writes the user's folder | the save **downloads** the
//!   document: persistence on the web is download-only for v1 (the
//!   roadmap decision) |
//! | the settings writer edits `config.toml` | the writer accepts, and
//!   the model hands the config back to the parade — the web's version
//!   of the config watcher's live reload |
//! | the draft autosave (`DraftDoc`, worker thread) | **nothing**: there
//!   is no disk and IndexedDB is a v2 follow-up; the download is the
//!   only copy |
//! | the note strip (F3 / TRK) | **inert**, with a toast: the sequencer
//!   is a v2 feature on the web |
//!
//! What survives unchanged is the interaction: the same 19 tools and 8
//! colours, the same stroke model (a press-drag is one undo step), the
//! same ESC ladder, the same dropdowns, name field and status line, the
//! same assign panel and the same settings panel. The keyboard and the
//! mouse are mapped from browser events onto the core's
//! [`EditorState`] through the same [`hit_test`] the native window uses.
//!
//! The model is **host-testable**: everything but the view (canvas,
//! animation-frame loop, DOM) runs in `cargo test`, mirroring the way
//! the parade's [`crate::app`] splits its browser glue from its host
//! tests.

use std::path::PathBuf;
use std::rc::Rc;

use agnosis_core::blit::{blit_grid_rows_between, FrameBuffer};
use agnosis_core::config::Config;
use agnosis_core::drawn::{validate_name, AssetEntry, DrawnError, DrawnKind, DrawnSprite};
use agnosis_core::editor::{
    self, hit_test, layout, paint_base, paint_overlay, visible_slot_rows, EditorHit, EditorLayout,
    EditorMode, EditorState, SettingKind, SettingValue, UndoAction, SETTING_ROWS,
};
use agnosis_core::font::Font;
use agnosis_core::grid::Grid;
use agnosis_core::personality::random_personalities;

use crate::assets_io::{AssetStore, SharedStore};

/// The one platform text the web editor adds: the note strip is a v2
/// feature (the roadmap decision — sequencer deferred). The block font
/// carries `A–Z`, `0–9` and `. : _ - / \`, so the toast reads in it.
const STRIP_INERT: &str = "NOTE STRIP IS V2: DESKTOP ONLY";

/// A download the model produced and the page has not taken yet.
pub struct PendingDownload {
    /// The file name the page saves under (`sprite-1.json`).
    pub name: String,
    /// The MIME type the page gives the blob.
    pub mime: &'static str,
    /// The bytes.
    pub bytes: Vec<u8>,
}

/// One thing the model did that the browser must act on.
///
/// The model never touches the DOM: it queues, the view pumps.
#[derive(Debug, Clone, PartialEq)]
pub enum EditorOutput {
    /// A document to download — the save's persistence (download-only
    /// for v1, the roadmap decision).
    Download {
        /// The file name (`sprite-1.json`).
        name: String,
        /// The MIME type of the blob.
        mime: &'static str,
        /// The bytes.
        bytes: Vec<u8>,
    },
    /// The config changed through the editor; the parade gets it — the
    /// web's version of the config watcher's live reload. **Boxed**:
    /// the `Config` is a wide struct and the download variant is three
    /// words wide, and the output queue carries both.
    ApplyConfig(Box<Config>),
}

/// One browser key event, decoded by the page.
///
/// `key` is the `KeyboardEvent.key` value (`"z"`, `"Escape"`, `"F1"`,
/// `"ArrowLeft"`), the modifiers the plain booleans. The model reads
/// only these — the same routing the native editor builds from winit's
/// `Key` and `ModifiersState`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEvent {
    /// The key, as `KeyboardEvent.key` spells it.
    pub key: String,
    /// Ctrl held.
    pub ctrl: bool,
    /// Shift held.
    pub shift: bool,
    /// Alt held.
    pub alt: bool,
    /// Meta (Cmd) held — the macOS undo alias.
    pub meta: bool,
}

impl KeyEvent {
    /// A key with no modifiers — the tests' shorthand.
    pub fn plain(key: &str) -> Self {
        Self {
            key: key.to_string(),
            ctrl: false,
            shift: false,
            alt: false,
            meta: false,
        }
    }
}

/// The editor's buffer for a CSS box of `css_w × css_h` at `dpr`.
///
/// The editor's layout math runs in **pixels**, and the native window
/// had an OS-enforced floor ([`MIN_WINDOW_W`] × [`MIN_WINDOW_H`]):
/// below it the name field vanishes while the user types into it. The
/// browser has no OS floor, so the floor is applied here — a page area
/// narrower than the floor gets the floor-sized buffer, scaled down by
/// the CSS, and every control stays present.
pub fn buffer_for(css_w: f64, css_h: f64, dpr: f64) -> (u32, u32) {
    let dpr = dpr.max(1.0);
    let width = ((css_w.max(1.0) * dpr).round() as u32)
        .max(editor::MIN_WINDOW_W)
        .max(1);
    let height = ((css_h.max(1.0) * dpr).round() as u32)
        .max(editor::MIN_WINDOW_H)
        .max(1);
    (width, height)
}

/// The first free generated name of `kind`, over a **name list**.
///
/// The core's [`agnosis_core::drawn::next_free_name_in`] walks a folder;
/// the browser's store is memory, so the same bounded scan runs over the
/// names the store carries (with `n` taken names there are `n + 2`
/// candidates, so one is always free).
pub fn next_free_name_from(taken: &[String], kind: DrawnKind) -> String {
    (1..=taken.len() + 2)
        .map(|index| format!("{}-{index}", kind.prefix()))
        .find(|candidate| !taken.iter().any(|name| name == candidate))
        .unwrap_or_else(|| format!("{}-1", kind.prefix()))
}

/// The assign panel's entries of one kind, from the shared store.
///
/// The size class rides along (the panel offers a big slot only the big
/// sprites — audit 2026-10-21 §E1) and the list is **sorted**, the way
/// the native folder listing is.
pub fn asset_entries_of(store: &AssetStore, kind: DrawnKind) -> Vec<AssetEntry> {
    let mut entries: Vec<AssetEntry> = store
        .entries()
        .iter()
        .filter(|(entry_kind, _, _)| *entry_kind == kind)
        .map(|(_, name, sprite)| AssetEntry {
            name: name.clone(),
            size: sprite.size,
        })
        .collect();
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

/// Save a document into the shared store, and queue the download.
///
/// This is the web's `drawn_save`: the store insert is the "disk" the
/// parade reads, the download is the copy the visitor keeps (the
/// roadmap's download-only persistence for v1). The bytes are the same
/// pretty JSON the native `drawn::save_to` writes, so a downloaded
/// document drops into the native app's drawn folder and parses there.
///
/// WHY the runtime document is not re-validated: the document is the
/// editor state's own output (`EditorState::document`), and the import
/// path validates on read — the native path re-checks because it trusts
/// nothing; the store cannot hold a sprite the editor did not just
/// build.
fn save_into_store(
    store: &SharedStore,
    outputs: &mut Vec<EditorOutput>,
    sprite: &DrawnSprite,
    kind: DrawnKind,
    name: &str,
) -> Result<PathBuf, DrawnError> {
    validate_name(name)?;
    if sprite.kind != kind {
        return Err(DrawnError::KindMismatch {
            expected: kind,
            actual: sprite.kind,
        });
    }
    let bytes = serde_json::to_vec_pretty(&sprite.to_doc()).map_err(|source| DrawnError::Json {
        path: PathBuf::new(),
        source,
    })?;
    store
        .borrow_mut()
        .insert(kind, name.to_string(), sprite.clone());
    outputs.push(EditorOutput::Download {
        name: format!("{name}.json"),
        mime: "application/json",
        bytes,
    });
    Ok(PathBuf::from(format!(
        "<browser>/drawn/{}/{}",
        kind.prefix(),
        name
    )))
}

/// The editor's model: the core's state plus the browser's seams.
///
/// The model owns **no DOM**: the view drives it and pumps its outputs.
/// The platform services the native editor injects as function pointers
/// are here ordinary model methods, because the browser's store is one
/// `Rc` away — the function-pointer seam exists so the core's state
/// stays `Clone`, and the model does not need that.
pub struct EditorModel {
    /// The core's state: the canvas, the dropdowns, the undo, the modes.
    pub state: EditorState,
    /// The store the parade shares. Saves land here and the parade sees
    /// them; the assign panel lists from here.
    store: SharedStore,
    /// The outputs the page has not pumped yet.
    outputs: Vec<EditorOutput>,
    /// The buffer the layout math runs on, in pixels.
    inner: (u32, u32),
    /// The last pointer position, in buffer pixels.
    pointer: (i64, i64),
    /// The settings row the pointer is dragging, if any.
    settings_drag: Option<usize>,
    /// The base layer: the canvas and the tool panel, in big cells.
    base_grid: Option<Grid>,
    /// The overlay layer: the top bar and the status line, in small cells.
    overlay_grid: Option<Grid>,
    /// The rasterizer for the base grid's cell size.
    base_font: Option<Font>,
    /// The rasterizer for the overlay grid's cell size.
    overlay_font: Option<Font>,
    /// The frame buffer both layers composite into.
    fb: FrameBuffer,
    /// The grid geometry the current grids were built for.
    geometry: (usize, usize, usize, usize),
    /// The moment the model was built — the personality randomiser's
    /// seed base (the core takes the seed as a parameter and never
    /// reads the clock).
    born: web_time::Instant,
    /// How many seeds the model has drawn, mixed into the seed.
    seeds: u32,
    /// True when the ESC ladder ran out of rungs: the page swaps the
    /// editor out (the native editor quits its event loop here).
    close_requested: bool,
}

impl EditorModel {
    /// A model over the parade's config and the shared store.
    pub fn new(cfg: &Config, store: SharedStore) -> Result<Self, String> {
        let state = EditorState::new(cfg, web_time::Instant::now);
        let base_font =
            Font::new(editor::CANVAS_CELL_W, editor::CANVAS_CELL_H).map_err(|e| e.to_string())?;
        let overlay_font =
            Font::new(editor::UI_CELL_W, editor::UI_CELL_H).map_err(|e| e.to_string())?;
        let mut model = Self {
            state,
            store,
            outputs: Vec::new(),
            inner: (editor::MIN_WINDOW_W, editor::MIN_WINDOW_H),
            pointer: (0, 0),
            settings_drag: None,
            base_grid: None,
            overlay_grid: None,
            base_font: Some(base_font),
            overlay_font: Some(overlay_font),
            fb: FrameBuffer::new(editor::MIN_WINDOW_W, editor::MIN_WINDOW_H),
            geometry: (0, 0, 0, 0),
            born: web_time::Instant::now(),
            seeds: 0,
            close_requested: false,
        };
        model.rebuild_grids();
        model.refresh_default_name();
        Ok(model)
    }

    /// The buffer the layout runs on, in pixels.
    pub fn buffer(&self) -> (u32, u32) {
        self.inner
    }

    /// Adopt a new buffer size (open and page resize).
    pub fn set_buffer(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if (width, height) == self.inner {
            return;
        }
        self.inner = (width, height);
        self.fb = FrameBuffer::new(width, height);
        self.rebuild_grids();
    }

    /// One editor frame at the native 30 fps: finish the armed save,
    /// refresh the placeholder name, paint and rasterize.
    pub fn tick(&mut self) -> (&[u8], u32, u32) {
        self.run_armed_save();
        self.refresh_default_name();
        self.render()
    }

    /// Paint the two layers and composite them into the frame buffer.
    ///
    /// The composition mirrors the native `Renderer::draw_layers`:
    /// black clear, the base grid, then the overlay grid on top — the
    /// overlay's ink wins, its transparent cells leave the base visible.
    pub fn render(&mut self) -> (&[u8], u32, u32) {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        if let Some(grid) = self.base_grid.as_mut() {
            grid.clear();
            paint_base(grid, &layout, &self.state);
        }
        if let Some(grid) = self.overlay_grid.as_mut() {
            grid.clear();
            paint_overlay(grid, &layout, &self.state);
        }
        self.fb.clear_black();
        if let (Some(base), Some(base_font)) = (&self.base_grid, self.base_font.as_mut()) {
            let rows = base.rows();
            blit_grid_rows_between(base, base_font, &mut self.fb, 0, 0..rows);
        }
        if let (Some(overlay), Some(overlay_font)) =
            (&self.overlay_grid, self.overlay_font.as_mut())
        {
            let rows = overlay.rows();
            blit_grid_rows_between(overlay, overlay_font, &mut self.fb, 0, 0..rows);
        }
        (&self.fb.data, self.fb.width, self.fb.height)
    }

    /// Rebuild the two grids for the current buffer.
    ///
    /// Each grid keeps its layer's cell size (the base the canvas'
    /// 16 × 24 px, the overlay the UI's 6 × 10 px) — a grid and its font
    /// must share the cell size, or the glyph masks no longer match the
    /// cell pitch and the frame goes black (AGENTS.md).
    fn rebuild_grids(&mut self) {
        let (width, height) = self.inner;
        let base_cols = usize::try_from((width / editor::CANVAS_CELL_W).max(1)).unwrap_or(1);
        let base_rows = usize::try_from((height / editor::CANVAS_CELL_H).max(1)).unwrap_or(1);
        let ui_cols = usize::try_from((width / editor::UI_CELL_W).max(1)).unwrap_or(1);
        let ui_rows = usize::try_from((height / editor::UI_CELL_H).max(1)).unwrap_or(1);
        if (base_cols, base_rows, ui_cols, ui_rows) == self.geometry {
            return;
        }
        self.geometry = (base_cols, base_rows, ui_cols, ui_rows);
        self.base_grid = Some(Grid::new(
            base_cols,
            base_rows,
            editor::CANVAS_CELL_W,
            editor::CANVAS_CELL_H,
        ));
        self.overlay_grid = Some(Grid::new(
            ui_cols,
            ui_rows,
            editor::UI_CELL_W,
            editor::UI_CELL_H,
        ));
    }

    /// Drain the outputs the page has not seen.
    pub fn take_outputs(&mut self) -> Vec<EditorOutput> {
        std::mem::take(&mut self.outputs)
    }

    /// Whether the ESC ladder ran out of rungs; the view closes.
    pub fn take_close(&mut self) -> bool {
        std::mem::take(&mut self.close_requested)
    }

    /// The line the page's status region mirrors: the toast when one is
    /// up, the editor's own status line otherwise.
    pub fn status_line(&self) -> String {
        if let Some(toast) = self.state.active_toast_at(web_time::Instant::now()) {
            return toast.to_string();
        }
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        editor::status_text(&self.state, &layout)
    }

    // -- pointer ---------------------------------------------------------

    /// The pointer moved (a drag paints while a stroke is open).
    pub fn pointer_move(&mut self, x: i64, y: i64) {
        self.pointer = (x, y);
        if let Some(row) = self.settings_drag {
            self.drag_setting(row);
            return;
        }
        if !self.state.stroke_is_open() {
            return;
        }
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        let hit = hit_test(&layout, &self.state, x, y);
        if let EditorHit::Canvas { col, row } = hit {
            self.state.paint(col, row);
        }
    }

    /// The left button went down.
    pub fn pointer_down(&mut self, x: i64, y: i64) {
        self.pointer = (x, y);
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        let hit = hit_test(&layout, &self.state, x, y);
        if self.list_swallows(&hit) {
            self.close_or_switch_list(hit);
            return;
        }
        if let EditorHit::Canvas { col, row } = hit {
            self.state.begin_stroke();
            self.state.paint(col, row);
            return;
        }
        // A slider is the drag target until the button comes up.
        if let EditorHit::SettingSlider(row) = hit {
            self.settings_drag = Some(row);
        }
        self.apply_hit(hit);
    }

    /// The left button came up: the stroke is over.
    pub fn pointer_up(&mut self) {
        self.state.end_stroke();
        self.settings_drag = None;
    }

    /// One wheel notch: `columns` sideways (trackpad gesture), `lines`
    /// vertically (positive scrolls down — the page converts the
    /// browser's pixel deltas to line units first).
    pub fn wheel(&mut self, columns: i32, lines: i32) {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        if columns != 0 {
            self.scroll_view_columns(columns);
            return;
        }
        if self.state.mode == EditorMode::Assign {
            self.scroll_assign_lists(lines, &layout);
            return;
        }
        if self.state.mode == EditorMode::Settings {
            self.scroll_settings(lines, &layout);
            return;
        }
        if layout.tools.contains(self.pointer.0, self.pointer.1) {
            self.state.scroll_panel_by(lines, layout.canvas_view_rows);
            return;
        }
        self.state.scroll_by(lines, layout.canvas_view_rows);
    }

    // -- keyboard --------------------------------------------------------

    /// One pressed key, routed the way the native editor routes winit's.
    pub fn key(&mut self, event: KeyEvent) {
        let KeyEvent {
            key,
            ctrl,
            shift,
            alt: _,
            meta,
        } = event;
        if key == "Escape" {
            self.on_escape();
            return;
        }
        if let Some(action) = undo_shortcut(&key, ctrl, meta, shift) {
            self.apply_undo(action);
            return;
        }
        // The note strip is inert on the web (v2), so F3 answers with a
        // toast instead of a mode.
        if key == "F3" {
            self.state.set_toast(STRIP_INERT);
            return;
        }
        if let Some(panel) = panel_shortcut(&key) {
            self.open_panel(panel);
            return;
        }
        if self.state.mode == EditorMode::Settings && self.settings_panel_key(&key) {
            return;
        }
        if let Some(delta) = self.scroll_delta(&key) {
            self.on_scroll_key(delta);
            return;
        }
        if let Some(delta) = scroll_columns_delta(&key) {
            if self.state.mode == EditorMode::Settings {
                self.step_setting_value(delta);
                return;
            }
            self.scroll_view_columns(delta);
            return;
        }
        if key == "Enter" {
            self.on_enter();
            return;
        }
        if self.state.mode == EditorMode::Settings {
            self.settings_text_key(&key);
            return;
        }
        self.on_name_key(&key, ctrl, meta);
    }

    /// The `Escape` key — the native ladder: an open choice list closes
    /// first, a focused text field commits, a panel is left, and a
    /// plain canvas **closes the editor** (the native editor quits its
    /// event loop here).
    fn on_escape(&mut self) {
        if self.escape_ladder() {
            return;
        }
        self.close_requested = true;
    }

    /// The rungs ESC walks before it may close; true when a rung
    /// consumed the key. (The native ladder's draft rung has no web
    /// equivalent — there are no drafts.)
    fn escape_ladder(&mut self) -> bool {
        if self.state.open_dropdown.is_some() {
            self.state.close_dropdown();
            return true;
        }
        if self.state.settings_open.is_some() {
            self.state.settings_open = None;
            return true;
        }
        if self.state.settings_focus.is_some() {
            self.commit_settings_text();
            return true;
        }
        if self.state.mode != EditorMode::Draw {
            self.state.leave_any_mode();
            return true;
        }
        false
    }

    /// Carry out an undo or a redo.
    fn apply_undo(&mut self, action: UndoAction) {
        let available = match action {
            UndoAction::Undo => self.state.undo.can_undo(),
            UndoAction::Redo => self.state.undo.can_redo(),
        };
        let changed = match action {
            UndoAction::Undo => self.state.undo(),
            UndoAction::Redo => self.state.redo(),
        };
        if !available || !changed {
            let toast = match action {
                UndoAction::Undo => "NOTHING TO UNDO",
                UndoAction::Redo => "NOTHING TO REDO",
            };
            self.state.set_toast(toast);
            return;
        }
        let toast = match action {
            UndoAction::Undo => "UNDONE",
            UndoAction::Redo => "REDONE",
        };
        self.state.set_toast(toast);
    }

    /// Open the panel a function key names.
    fn open_panel(&mut self, panel: Panel) {
        match panel {
            Panel::Settings => self.enter_settings_mode(),
            Panel::Assign => self.enter_assign_mode(),
            Panel::Draw => self.state.leave_any_mode(),
        }
    }

    /// One key in the settings panel that is not the cursor's Up/Down.
    fn settings_panel_key(&mut self, key: &str) -> bool {
        if matches!(key, "Enter" | " ") {
            if self.state.settings_focus.is_some() {
                self.commit_settings_text();
            } else if self.cursor_row_is_action() {
                let row = self.state.settings_cursor;
                self.run_settings_action(row);
            } else {
                self.state.settings_activate();
                self.commit_staged_setting();
            }
            return true;
        }
        let delta = match key {
            "ArrowLeft" => -1,
            "ArrowRight" => 1,
            _ => return false,
        };
        self.step_setting_value(delta);
        true
    }

    /// Is the settings cursor on an **action** row?
    fn cursor_row_is_action(&self) -> bool {
        self.state
            .settings_row()
            .is_some_and(|row| matches!(row.kind, SettingKind::Action(_)))
    }

    /// The scroll delta of a scroll key; a page is the visible canvas
    /// rows.
    fn scroll_delta(&self, key: &str) -> Option<i32> {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        let page = i32::try_from(layout.canvas_view_rows.max(1)).unwrap_or(1);
        match key {
            "ArrowUp" => Some(-1),
            "ArrowDown" => Some(1),
            "PageUp" => Some(-page),
            "PageDown" => Some(page),
            _ => None,
        }
    }

    /// One scroll key: the cursor in the settings panel, the canvas
    /// otherwise.
    fn on_scroll_key(&mut self, delta: i32) {
        if self.state.mode == EditorMode::Settings {
            let layout = layout(self.inner.0, self.inner.1, &self.state);
            self.state.move_settings_cursor(delta);
            self.keep_the_cursor_visible(&layout);
            return;
        }
        self.scroll_view(delta);
    }

    /// Scroll the view: the assign lists in assign mode, the settings
    /// panel in settings mode, the canvas in draw mode.
    fn scroll_view(&mut self, delta: i32) {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        if self.state.mode == EditorMode::Assign {
            self.scroll_assign_lists(delta, &layout);
            return;
        }
        if self.state.mode == EditorMode::Settings {
            self.scroll_settings(delta, &layout);
            return;
        }
        self.state.scroll_by(delta, layout.canvas_view_rows);
    }

    /// Scroll the canvas sideways (a grass tile's canvas is 200 columns
    /// wide).
    fn scroll_view_columns(&mut self, delta: i32) {
        if matches!(self.state.mode, EditorMode::Assign | EditorMode::Settings) {
            return;
        }
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        self.state.scroll_by_cols(delta, layout.canvas_view_cols);
    }

    /// Scroll the slot list or the asset list, depending on the pointer.
    fn scroll_assign_lists(&mut self, delta: i32, layout: &EditorLayout) {
        if layout.slot_list.contains(self.pointer.0, self.pointer.1) {
            self.state
                .scroll_slot_list_by(delta, visible_slot_rows(layout));
            return;
        }
        self.state
            .scroll_asset_list_by(delta, layout.canvas_view_rows);
    }

    /// Scroll the settings panel and keep the cursor on screen.
    fn scroll_settings(&mut self, delta: i32, layout: &EditorLayout) {
        self.state
            .scroll_settings_by(delta, layout.settings_visible);
        self.keep_the_cursor_visible(layout);
    }

    /// Scroll the settings panel so the cursor row is on screen.
    fn keep_the_cursor_visible(&mut self, layout: &EditorLayout) {
        let cursor = self.state.settings_cursor;
        let first = layout.settings_scroll;
        let last = first + layout.settings_visible;
        if cursor < first {
            self.state.settings_scroll = cursor;
        } else if cursor >= last {
            self.state.settings_scroll = cursor + 1 - layout.settings_visible;
        }
    }

    /// Route one key to the name field.
    ///
    /// The name only accepts `a-z0-9-_`; the core drops the rest. A
    /// modified key (Ctrl, Meta, Alt) never reaches the name: the
    /// browser fires Ctrl+A and friends constantly, and a combination is
    /// a command, not a name character.
    fn on_name_key(&mut self, key: &str, ctrl: bool, meta: bool) {
        if ctrl || meta {
            return;
        }
        if key == "Backspace" {
            self.state.pop_name_char();
            return;
        }
        if key.chars().count() == 1 {
            if let Some(ch) = key.chars().next() {
                self.state.push_name_char(ch);
            }
        }
    }

    /// One settings-panel key that is not an arrow: a character into the
    /// focused field, Backspace out of it.
    fn settings_text_key(&mut self, key: &str) {
        if key == "Backspace" {
            self.state.pop_settings_text();
            return;
        }
        if key.chars().count() == 1 {
            self.state.push_settings_text(key);
        }
    }

    /// The Enter key: the save in draw mode, the assignment in assign
    /// mode, the text commit in the settings panel (which the panel's
    /// own key router consumed first).
    fn on_enter(&mut self) {
        let now = web_time::Instant::now();
        if self.state.mode == EditorMode::Draw {
            self.state.arm_save(now);
            return;
        }
        if self.state.mode == EditorMode::Assign {
            let before_table = self.state.assignments.clone();
            let before_config = self.state.settings_config.clone();
            let store = Rc::clone(&self.store);
            let outputs = &mut self.outputs;
            self.state.enter_action_with(
                now,
                |kind, name| store.borrow().exists(kind, name),
                |sprite, kind, name| save_into_store(&store, outputs, sprite, kind, name),
                |_write| Ok(()),
            );
            self.push_assign_config(&before_table, &before_config);
        }
    }

    // -- pointer targets -------------------------------------------------

    /// Act on one hit (the native `apply_hit`, minus the note strip).
    fn apply_hit(&mut self, hit: EditorHit) {
        if self.list_swallows(&hit) {
            self.close_or_switch_list(hit);
            return;
        }
        // Clicking somewhere else commits the text field the user was
        // typing in: the value is written, not dropped.
        if self.state.settings_focus.is_some() && !matches!(hit, EditorHit::SettingText(_)) {
            self.commit_settings_text();
        }
        match hit {
            EditorHit::Dropdown(index) => self.state.open_dropdown(index),
            EditorHit::DropdownOption { dropdown, option } => {
                // The element picker loads its slot's document out of the
                // shared store — the browser has no folder to ask
                // (2026-11-12 element picker).
                if dropdown == 3 {
                    let store = Rc::clone(&self.store);
                    self.state.pick_element_with(option, |kind, name| {
                        store.borrow().load(kind, name).ok()
                    });
                } else {
                    self.state.pick_option(dropdown, option)
                }
            }
            EditorHit::Tool(index) => self.state.set_tool(index),
            EditorHit::Color(index) => self.state.set_color(index),
            EditorHit::Canvas { col, row } => self.state.paint(col, row),
            // The name is edited with the keyboard; the field itself has
            // no click behaviour.
            EditorHit::NameField => {}
            EditorHit::PrimaryButton => self.state.arm_save(web_time::Instant::now()),
            EditorHit::AssignButton => self.enter_assign_mode(),
            EditorHit::DrawButton => self.state.leave_any_mode(),
            EditorHit::SettingsButton => self.enter_settings_mode(),
            EditorHit::Slot(index) => self.state.select_target(index),
            EditorHit::Asset(index) => self.assign_asset(index),
            EditorHit::ClearButton => self.clear_assignment(),
            EditorHit::SettingSlider(row) => self.drag_setting(row),
            EditorHit::SettingToggle(row) => self.toggle_setting(row),
            EditorHit::SettingText(row) => self.state.focus_text_row(row),
            EditorHit::SettingChoice(row) => self.state.settings_open = Some(row),
            EditorHit::SettingAction(row) => self.run_settings_action(row),
            EditorHit::SettingOption { row, option } => {
                self.state.settings_open = None;
                self.change_setting(row, SettingValue::Choice(option));
            }
            // The note strip is inert on the web: a toast, not a mode.
            EditorHit::TrackButton => self.state.set_toast(STRIP_INERT),
            // The strip-only hits are unreachable while the strip cannot
            // be entered.
            _ => {}
        }
    }

    /// True when the open option list swallows the click (the native
    /// rule: only an option of the open list is acted on; the mode
    /// buttons are never swallowed).
    fn list_swallows(&self, hit: &EditorHit) -> bool {
        if matches!(
            hit,
            EditorHit::AssignButton
                | EditorHit::DrawButton
                | EditorHit::SettingsButton
                | EditorHit::TrackButton
        ) {
            return false;
        }
        if self.state.mode != EditorMode::Draw {
            return false;
        }
        let Some(open) = self.state.open_dropdown else {
            return false;
        };
        !matches!(hit, EditorHit::DropdownOption { dropdown, .. } if *dropdown == open)
    }

    /// Close the open list; a click on another box opens that one
    /// instead, and a click on the open box closes it.
    fn close_or_switch_list(&mut self, hit: EditorHit) {
        let open = self.state.open_dropdown;
        self.state.close_dropdown();
        let EditorHit::Dropdown(other) = hit else {
            return;
        };
        if Some(other) == open {
            return;
        }
        self.state.open_dropdown(other);
    }

    // -- modes -----------------------------------------------------------

    /// Switch into the settings panel over the current config.
    ///
    /// The native editor re-reads the user's file (its watcher picks the
    /// changes up); the web's `settings_config` **is** the live copy, so
    /// the panel opens on what is already on screen.
    fn enter_settings_mode(&mut self) {
        let config = self.state.settings_config.clone();
        self.state.enter_settings_mode_with(config);
    }

    /// Switch into assign mode over the shared store.
    ///
    /// The lists come from the store (the same read the native editor's
    /// platform lister makes), the assignment table from the live
    /// config.
    fn enter_assign_mode(&mut self) {
        let config = self.state.settings_config.clone();
        let sprites = asset_entries_of(&self.store.borrow(), DrawnKind::Sprite);
        let balloons = asset_entries_of(&self.store.borrow(), DrawnKind::Balloon);
        let grass = asset_entries_of(&self.store.borrow(), DrawnKind::Grass);
        let decorations = asset_entries_of(&self.store.borrow(), DrawnKind::Decoration);
        self.state.enter_assign_mode_with_assets(
            sprites,
            balloons,
            grass,
            decorations,
            config.assignments,
        );
    }

    /// Assign the picked asset to the selected slot.
    fn assign_asset(&mut self, index: usize) {
        self.state.select_asset(index);
        let before_table = self.state.assignments.clone();
        let before_config = self.state.settings_config.clone();
        let now = web_time::Instant::now();
        self.state.assign_action_with(now, |_write| Ok(()));
        self.push_assign_config(&before_table, &before_config);
    }

    /// Remove the assignment of the selected slot.
    fn clear_assignment(&mut self) {
        let before_table = self.state.assignments.clone();
        let before_config = self.state.settings_config.clone();
        let now = web_time::Instant::now();
        self.state.clear_action_with(now, |_write| Ok(()));
        self.push_assign_config(&before_table, &before_config);
    }

    /// Hand the config to the parade when an assign write landed.
    ///
    /// The write itself is a no-op on the web (there is no file): the
    /// core's `record_the_assignment` runs **only when the writer
    /// accepted**, so a changed state is the receipt that the write
    /// landed — the same write-first rule the native editor's file gives
    /// it. The `AssignWrite` payload is ignored on purpose: the core's
    /// record keeps the state authoritative, and here that state is the
    /// only truth.
    ///
    /// The receipt watches **two** states, because the core keeps the
    /// assignment on two shelves: the parade slots go into the panel's
    /// own table ([`EditorState::assignments`]), the look targets (the
    /// grass tile, the decorations) into the config copy. The config the
    /// parade reloads merges both — the table wins, exactly as the
    /// manifest lift does on import.
    fn push_assign_config(
        &mut self,
        before_table: &agnosis_core::config::AssignmentsConfig,
        before_config: &Config,
    ) {
        let wrote =
            self.state.assignments != *before_table || self.state.settings_config != *before_config;
        if !wrote {
            return;
        }
        let mut config = self.state.settings_config.clone();
        config.assignments = self.state.assignments.clone();
        self.outputs
            .push(EditorOutput::ApplyConfig(Box::new(config)));
    }

    // -- settings panel --------------------------------------------------

    /// Write one setting into the live config.
    ///
    /// The writer accepts unconditionally (there is no file to refuse);
    /// the core applies the value to its own copy on success, and the
    /// parade reloads from that copy — the web's live reload.
    fn change_setting(&mut self, row: usize, value: SettingValue) {
        let changed =
            self.state
                .change_setting_with(row, value, web_time::Instant::now(), |_, _, _| Ok(()));
        if changed {
            let config = self.state.settings_config.clone();
            self.outputs
                .push(EditorOutput::ApplyConfig(Box::new(config)));
        }
    }

    /// Write the focused text field into the live config.
    fn commit_settings_text(&mut self) {
        let Some(row) = self.state.settings_focus else {
            return;
        };
        let text = self.state.settings_text.clone();
        self.state.unfocus_text();
        self.change_setting(row, SettingValue::Text(text));
    }

    /// Write the keyboard's staged settings edit through the writer.
    fn commit_staged_setting(&mut self) {
        let Some((row, value)) = self.state.take_staged_setting() else {
            return;
        };
        self.change_setting(row, value);
    }

    /// Step the value of the settings row the cursor is on.
    fn step_setting_value(&mut self, delta: i32) {
        if self.state.settings_step(delta) {
            self.commit_staged_setting();
            return;
        }
        let row = self.state.settings_cursor;
        self.state.focus_text_row(row);
    }

    /// The value of a slider row at the pointer's position.
    fn setting_slider_value(&self, row: usize) -> Option<SettingValue> {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        let visible = row.checked_sub(layout.settings_scroll)?;
        let track = editor::settings_track_rect(layout.settings, visible);
        if track.w == 0 {
            return None;
        }
        let fraction = ((self.pointer.0 - track.x) as f32) / (track.w as f32);
        self.state.slider_value(row, fraction)
    }

    /// Set a slider row from the pointer's position.
    fn drag_setting(&mut self, row: usize) {
        let Some(value) = self.setting_slider_value(row) else {
            return;
        };
        self.change_setting(row, value);
    }

    /// Flip a toggle row.
    fn toggle_setting(&mut self, row: usize) {
        let Some(current) = self.state.settings_value(row) else {
            return;
        };
        let SettingValue::Bool(flag) = current else {
            return;
        };
        self.change_setting(row, SettingValue::Bool(!flag));
    }

    /// Run the action of an action row (the PERSONALITY buttons).
    ///
    /// The native runner writes the `[[personality]]` table and the
    /// watcher reloads the parade; the web runner lands the drawn
    /// personalities **directly in the live config** and hands it to the
    /// parade. The seed is drawn from the model's own elapsed time, so
    /// the core never reads a clock (the core takes the seed as a
    /// parameter).
    fn run_settings_action(&mut self, row: usize) {
        let Some(setting) = SETTING_ROWS.get(row) else {
            return;
        };
        let SettingKind::Action(_) = setting.kind else {
            return;
        };
        let drawn = match setting.key {
            "randomise" => Some(random_personalities(self.next_seed())),
            "reset" => Some(Vec::new()),
            _ => None,
        };
        let Some(drawn) = drawn else {
            return;
        };
        self.state.settings_config.personality = drawn;
        self.state
            .run_settings_action_with(row, web_time::Instant::now(), |_| Ok(()));
        let config = self.state.settings_config.clone();
        self.outputs
            .push(EditorOutput::ApplyConfig(Box::new(config)));
    }

    /// A seed for the personality randomiser.
    fn next_seed(&mut self) -> u64 {
        self.seeds += 1;
        let elapsed = self.born.elapsed().as_nanos() as u64;
        elapsed ^ ((u64::from(self.seeds)) << 47)
    }

    // -- the small shared things ------------------------------------------

    /// Complete the save the SAVE press armed — one frame after the
    /// press, so the `SAVING...` toast paints first (decision F-163).
    fn run_armed_save(&mut self) {
        if !self.state.armed_save {
            return;
        }
        self.state.armed_save = false;
        let now = web_time::Instant::now();
        let store = Rc::clone(&self.store);
        let outputs = &mut self.outputs;
        self.state.primary_action_with(
            now,
            |kind, name| store.borrow().exists(kind, name),
            |sprite, kind, name| save_into_store(&store, outputs, sprite, kind, name),
        );
        // The element picker's save points its slot at the document it
        // wrote. The core recorded the assignment write-first, so the
        // merged config copy **is** the write on the web — push it to the
        // parade (2026-11-12 element picker).
        if self.state.assignment_request.take().is_some() {
            let mut config = self.state.settings_config.clone();
            config.assignments = self.state.assignments.clone();
            self.outputs
                .push(EditorOutput::ApplyConfig(Box::new(config)));
        }
    }

    /// Refresh the name field's placeholder from the shared store.
    ///
    /// The native editor asks the platform only when the kind changes;
    /// the model refreshes every frame instead — after a save the
    /// placeholder moves to the next free name (`sprite-2`), which the
    /// native placeholder learns only on a kind switch. The scan is a
    /// handful of name comparisons.
    fn refresh_default_name(&mut self) {
        // A **loaded** document keeps its own name as the placeholder:
        // the field shows what a save will write back to (2026-11-12
        // element picker), not the next free name.
        if self.state.editing.is_some() {
            return;
        }
        let kind = self.state.kind;
        let taken: Vec<String> = self
            .store
            .borrow()
            .entries()
            .iter()
            .filter(|(entry_kind, _, _)| *entry_kind == kind)
            .map(|(_, name, _)| name.clone())
            .collect();
        self.state.default_name = next_free_name_from(&taken, kind);
    }
}

/// The panel a function key opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Panel {
    /// The settings panel.
    Settings,
    /// The assignment picker.
    Assign,
    /// The drawing canvas — the way out of every other panel.
    Draw,
}

/// The panel a function key opens, if any: `F1` settings, `F2` assign,
/// `F4` the canvas. (`F3` — the native note strip — is answered with an
/// inert toast before this runs: the strip is v2 on the web.)
fn panel_shortcut(key: &str) -> Option<Panel> {
    match key {
        "F1" => Some(Panel::Settings),
        "F2" => Some(Panel::Assign),
        "F4" => Some(Panel::Draw),
        _ => None,
    }
}

/// The undo action a combination asks for: `Ctrl+Z` / `Cmd+Z` undoes,
/// `Ctrl+Shift+Z` and `Ctrl+Y` (and `Cmd+Shift+Z` / `Cmd+Y`) redo.
fn undo_shortcut(key: &str, ctrl: bool, meta: bool, shift: bool) -> Option<UndoAction> {
    if !ctrl && !meta {
        return None;
    }
    let ch = key.chars().next()?;
    if ch.eq_ignore_ascii_case(&'y') {
        return Some(UndoAction::Redo);
    }
    if ch.eq_ignore_ascii_case(&'z') {
        if shift {
            return Some(UndoAction::Redo);
        }
        return Some(UndoAction::Undo);
    }
    None
}

/// The horizontal scroll delta of a scroll key, if the key is one.
fn scroll_columns_delta(key: &str) -> Option<i32> {
    match key {
        "ArrowLeft" => Some(-1),
        "ArrowRight" => Some(1),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The view: the canvas, the animation-frame loop, the DOM seams.
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
pub(crate) mod view {
    use super::*;
    use std::cell::RefCell;

    use wasm_bindgen::closure::Closure;
    use wasm_bindgen::JsCast;
    use web_sys::HtmlCanvasElement;

    use crate::app::{keep_alive, the_app, with_app};
    use crate::render::{canvas2d, Renderer};

    thread_local! {
        /// The one live editor of the page, if it is open.
        static EDITOR: RefCell<Option<Rc<RefCell<EditorView>>>> = const { RefCell::new(None) };

        /// The downloads the model produced and the page has not taken.
        static DOWNLOADS: RefCell<Vec<PendingDownload>> = const { RefCell::new(Vec::new()) };
    }

    /// The loop's handle on its own callback (the parade's audit §W3
    /// shape: the handle carries the JavaScript function, the closure
    /// lives in the keep-alive list).
    type FrameHandle = Rc<RefCell<Option<wasm_bindgen::JsValue>>>;

    /// The live editor: the model, its canvas and the 2D renderer.
    pub struct EditorView {
        /// The model (state + inputs + outputs).
        pub model: EditorModel,
        /// The page's editor canvas.
        canvas: HtmlCanvasElement,
        /// The plain 2D back end (no GPU negotiation — see the module doc).
        renderer: Box<dyn Renderer>,
        /// The parade's pause state before the editor opened; restored
        /// on close.
        resume_paused: bool,
        /// The timestamp of the last frame, in milliseconds.
        last_ts_ms: Option<f64>,
        /// The time not yet spent on a frame (the 30 fps cap).
        accumulator: f32,
    }

    /// Run `f` on the live editor, if it is open.
    fn with_editor<T>(f: impl FnOnce(&mut EditorView) -> T) -> Option<T> {
        let view = EDITOR.with(|cell| cell.borrow().clone())?;
        let mut view = view.borrow_mut();
        Some(f(&mut view))
    }

    /// Open the editor on the page's `#editor-canvas`.
    ///
    /// The parade is paused for the session (and its prior pause state
    /// remembered): the parade keeps running hidden otherwise, spending
    /// frames on a canvas nobody watches. Opening is idempotent — a
    /// second call while the editor is up changes nothing.
    pub fn open() -> Result<(), String> {
        if EDITOR.with(|cell| cell.borrow().is_some()) {
            return Ok(());
        }
        let Some(app) = the_app() else {
            return Err("the parade is not running yet".to_string());
        };
        let (cfg, store, resume_paused) = {
            let mut state = app.borrow_mut();
            let resume = state.paused;
            state.paused = true;
            (state.sim.cfg.clone(), Rc::clone(&state.sim.assets), resume)
        };
        let canvas = the_editor_canvas()?;
        let built = build_the_editor(&canvas, &cfg, store, resume_paused);
        let view = match built {
            Ok(view) => view,
            Err(error) => {
                // A failed open restores the parade's pause state: the
                // editor never took over.
                with_app(|state| state.paused = resume_paused);
                return Err(error);
            }
        };
        EDITOR.with(|cell| *cell.borrow_mut() = Some(Rc::clone(&view)));
        start_the_loop()
    }

    /// Find the page's editor canvas.
    fn the_editor_canvas() -> Result<HtmlCanvasElement, String> {
        let document = web_sys::window()
            .and_then(|window| window.document())
            .ok_or_else(|| "the page has no document".to_string())?;
        let element = document
            .get_element_by_id("editor-canvas")
            .ok_or_else(|| "the page has no #editor-canvas element".to_string())?;
        element
            .dyn_into::<HtmlCanvasElement>()
            .map_err(|_| "the #editor-canvas element is not a canvas".to_string())
    }

    /// Build the model and the renderer, size the canvas, return the
    /// view.
    fn build_the_editor(
        canvas: &HtmlCanvasElement,
        cfg: &Config,
        store: SharedStore,
        resume_paused: bool,
    ) -> Result<Rc<RefCell<EditorView>>, String> {
        let renderer: Box<dyn Renderer> = match canvas2d::create(canvas) {
            Ok(backend) => Box::new(backend),
            Err(attempt) => return Err(attempt.message),
        };
        let mut model = EditorModel::new(cfg, store)?;
        let buffer = editor_buffer_for(canvas);
        model.set_buffer(buffer.0, buffer.1);
        canvas.set_width(buffer.0);
        canvas.set_height(buffer.1);
        Ok(Rc::new(RefCell::new(EditorView {
            model,
            canvas: canvas.clone(),
            renderer,
            resume_paused,
            last_ts_ms: None,
            accumulator: 0.0,
        })))
    }

    /// The editor buffer for the canvas' CSS box.
    ///
    /// The canvas' CSS aspect is fixed by the page's stylesheet
    /// (the layout floor's ratio), so the **width** decides and the
    /// height follows — a box that is not laid out yet falls back to the
    /// document's width.
    fn editor_buffer_for(canvas: &HtmlCanvasElement) -> (u32, u32) {
        let css_w = canvas.client_width().max(fallback_width()) as f64;
        let css_h = css_w * f64::from(editor::MIN_WINDOW_H) / f64::from(editor::MIN_WINDOW_W);
        let dpr = crate::app::device_pixel_ratio();
        buffer_for(css_w, css_h, dpr)
    }

    /// The document's width, for a canvas that is not laid out yet.
    fn fallback_width() -> i32 {
        web_sys::window()
            .and_then(|window| window.document())
            .and_then(|document| document.document_element())
            .map(|root| root.client_width())
            .unwrap_or(editor::MIN_WINDOW_W as i32)
            .max(1)
    }

    /// Close the editor and hand the parade back.
    ///
    /// The loop stops by itself: the next animation frame finds no
    /// editor and does not schedule another.
    pub fn close() {
        let Some(view) = EDITOR.with(|cell| cell.borrow_mut().take()) else {
            return;
        };
        let resume = view.borrow().resume_paused;
        with_app(|state| state.paused = resume);
    }

    /// Is the editor open?
    pub fn is_open() -> bool {
        EDITOR.with(|cell| cell.borrow().is_some())
    }

    /// Forward one input to the model and pump what it produced.
    ///
    /// The pump runs **after** the input, inside the same borrow: a
    /// save's download and a config change are handled before the page
    /// can look at anything. A close request is honoured after the
    /// borrow drops (closing takes the editor out of its own slot).
    fn forward(mutate: impl FnOnce(&mut EditorView)) {
        let Some(view) = EDITOR.with(|cell| cell.borrow().clone()) else {
            return;
        };
        let close_requested = {
            let mut view = view.borrow_mut();
            mutate(&mut view);
            pump_outputs(&mut view);
            view.model.take_close()
        };
        if close_requested {
            close();
        }
    }

    /// Carry out the outputs the model queued.
    fn pump_outputs(view: &mut EditorView) {
        for output in view.model.take_outputs() {
            match output {
                EditorOutput::Download { name, mime, bytes } => {
                    DOWNLOADS.with(|cell| {
                        cell.borrow_mut()
                            .push(PendingDownload { name, mime, bytes });
                    });
                }
                EditorOutput::ApplyConfig(config) => apply_live_config(&config),
            }
        }
    }

    /// Reload the parade in place with the editor's config — the web's
    /// version of the config watcher's live reload. The loader borrows
    /// the shared store per call (the reload holds the scene mutably).
    fn apply_live_config(config: &Config) {
        let Some(app) = the_app() else {
            return;
        };
        let mut state = app.borrow_mut();
        let loader_store = Rc::clone(&state.sim.assets);
        state.sim.scene.apply_config_with(
            config,
            |kind, name| loader_store.borrow().load(kind, name),
            None,
        );
        state.sim.cfg = config.clone();
    }

    /// One animation frame: advance the 30 fps cap, tick the model and
    /// present. True when the loop should continue.
    fn frame(view: &mut EditorView, ts_ms: f64) -> bool {
        let delta = view.raw_delta(ts_ms);
        view.accumulator += delta;
        let cap = editor::EDITOR_FPS.max(1) as f32;
        if view.accumulator < 1.0 / cap {
            return true;
        }
        view.accumulator = 0.0;
        let (bytes, width, height) = view.model.tick();
        view.renderer.present(bytes, width, height);
        // The tick can produce outputs of its own — the armed save
        // completes here, one frame after the press — so the frame
        // pumps too, not only the forwarded events (a download that
        // waited for the next click was a download the page never saw).
        pump_outputs(view);
        true
    }

    impl EditorView {
        /// The time since the last frame, in seconds.
        fn raw_delta(&mut self, ts_ms: f64) -> f32 {
            let Some(last) = self.last_ts_ms else {
                self.last_ts_ms = Some(ts_ms);
                return 0.0;
            };
            self.last_ts_ms = Some(ts_ms);
            let raw = (ts_ms - last) / 1000.0;
            if !raw.is_finite() {
                return 0.0;
            }
            (raw as f32).max(0.0)
        }
    }

    /// Start the editor's animation-frame loop.
    fn start_the_loop() -> Result<(), String> {
        let window = web_sys::window().ok_or_else(|| "the page has no window".to_string())?;
        let handle: FrameHandle = Rc::new(RefCell::new(None));
        let handle_in_loop = Rc::clone(&handle);
        let window_in_loop = window.clone();
        let closure: Closure<dyn FnMut(f64)> = Closure::new(move |ts_ms: f64| {
            let again = with_editor(|editor| frame(editor, ts_ms));
            if again == Some(true) {
                schedule_the_next_frame(&window_in_loop, &handle_in_loop);
            }
        });
        // The handle carries the JavaScript function, the closure the
        // keep-alive list (the parade's audit §W3 shape).
        let callback: wasm_bindgen::JsValue = closure.as_ref().clone();
        *handle.borrow_mut() = Some(callback);
        keep_alive(closure);
        keep_alive(Rc::clone(&handle));
        let guard = handle.borrow();
        let Some(callback) = guard.as_ref() else {
            return Err("the editor frame callback is gone".to_string());
        };
        window
            .request_animation_frame(callback.unchecked_ref())
            .map(|_: i32| ())
            .map_err(|error| format!("requestAnimationFrame failed: {error:?}"))
    }

    /// Ask for the next frame.
    fn schedule_the_next_frame(window: &web_sys::Window, handle: &FrameHandle) {
        let guard = handle.borrow();
        let Some(callback) = guard.as_ref() else {
            return;
        };
        let _ = window.request_animation_frame(callback.unchecked_ref());
    }

    /// Re-lay the editor on the page's resize.
    pub fn resize() {
        forward(|view| {
            let buffer = editor_buffer_for(&view.canvas);
            view.model.set_buffer(buffer.0, buffer.1);
            view.canvas.set_width(buffer.0);
            view.canvas.set_height(buffer.1);
        });
    }

    /// The page's forwards, one per browser event kind.
    pub fn pointer_down(x: f64, y: f64) {
        forward(|view| view.model.pointer_down(x as i64, y as i64));
    }

    pub fn pointer_move(x: f64, y: f64) {
        forward(|view| view.model.pointer_move(x as i64, y as i64));
    }

    pub fn pointer_up() {
        forward(EditorView::release);
    }

    pub fn wheel(dx: f64, dy: f64) {
        forward(|view| view.model.wheel(dx.round() as i32, dy.round() as i32));
    }

    pub fn key(event: KeyEvent) {
        forward(|view| view.model.key(event));
    }

    /// The line the page's status region mirrors.
    pub fn status_line() -> String {
        with_editor(|view| view.model.status_line()).unwrap_or_default()
    }

    /// Take one download the model produced.
    pub fn take_download() -> Option<PendingDownload> {
        DOWNLOADS.with(|cell| {
            let mut queue = cell.borrow_mut();
            if queue.is_empty() {
                None
            } else {
                Some(queue.remove(0))
            }
        })
    }

    impl EditorView {
        /// The left button came up (the model's name keeps the parity
        /// with the native `on_release`).
        fn release(&mut self) {
            self.model.pointer_up();
        }
    }
}

/// The host's stubs: the editor's model runs in `cargo test`, the view
/// needs a browser — the same shape `render::create`'s host branch has
/// (an honest refusal, not a panic).
#[cfg(not(target_arch = "wasm32"))]
pub(crate) mod view {
    use super::*;

    pub fn open() -> Result<(), String> {
        Err("the editor's canvas needs a browser".to_string())
    }

    pub fn close() {}

    pub fn is_open() -> bool {
        false
    }

    pub fn pointer_down(_x: f64, _y: f64) {}

    pub fn pointer_move(_x: f64, _y: f64) {}

    pub fn pointer_up() {}

    pub fn wheel(_dx: f64, _dy: f64) {}

    pub fn key(_event: KeyEvent) {}

    pub fn resize() {}

    pub fn status_line() -> String {
        String::new()
    }

    pub fn take_download() -> Option<PendingDownload> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use agnosis_core::balloon::AsciiSprite;
    use agnosis_core::drawn::DrawnSize;

    /// A model at the layout floor's buffer.
    fn model() -> EditorModel {
        model_at_store(test_store())
    }

    /// A model over a given store.
    fn model_at_store(store: SharedStore) -> EditorModel {
        let mut model = EditorModel::new(&Config::default(), store).expect("the model builds");
        model.set_buffer(editor::MIN_WINDOW_W, editor::MIN_WINDOW_H);
        model
    }

    /// An empty shared store.
    fn test_store() -> SharedStore {
        Rc::new(RefCell::new(AssetStore::new()))
    }

    /// A store carrying one big sprite under `name`.
    fn store_with_sprite(name: &str) -> SharedStore {
        let store = test_store();
        store.borrow_mut().insert(
            DrawnKind::Sprite,
            name.to_string(),
            sprite_of(DrawnKind::Sprite, Some(DrawnSize::Big)),
        );
        store
    }

    /// A minimal valid sprite of `kind`.
    fn sprite_of(kind: DrawnKind, size: Option<DrawnSize>) -> DrawnSprite {
        DrawnSprite {
            kind,
            size,
            frames: vec![AsciiSprite::new(8, 8)],
            reaction: None,
        }
    }

    /// One click on the canvas' first paintable cell.
    fn canvas_click(model: &mut EditorModel) {
        let layout = layout(model.buffer().0, model.buffer().1, &model.state);
        let cell = layout.canvas_origin;
        model.pointer_down(cell.0 + 2, cell.1 + 2);
    }

    /// The buffer floors at the layout's minimum.
    #[test]
    fn the_buffer_floors_at_the_layout_minimum() {
        let floor = (editor::MIN_WINDOW_W, editor::MIN_WINDOW_H);
        assert_eq!(buffer_for(900.0, 400.0, 1.0), floor);
        assert_eq!(buffer_for(1.0, 1.0, 1.0), floor);
        // Above the floor the CSS box wins, the DPR folds in.
        assert_eq!(buffer_for(1600.0, 704.0, 1.0), (1600, 704));
        assert_eq!(buffer_for(1600.0, 704.0, 2.0), (3200, 1408));
    }

    /// The generated name skips the names the store carries.
    #[test]
    fn the_next_free_name_skips_used_names() {
        let taken = vec!["sprite-1".to_string(), "sprite-3".to_string()];
        assert_eq!(next_free_name_from(&taken, DrawnKind::Sprite), "sprite-2");
        let taken = vec!["sprite-1".to_string(), "sprite-2".to_string()];
        assert_eq!(next_free_name_from(&taken, DrawnKind::Sprite), "sprite-3");
        assert_eq!(next_free_name_from(&[], DrawnKind::Balloon), "balloon-1");
    }

    /// The store's entries carry the size class and are sorted.
    #[test]
    fn the_asset_entries_carry_sizes_and_sort() {
        let store = test_store();
        let mut big = sprite_of(DrawnKind::Sprite, Some(DrawnSize::Big));
        big.size = Some(DrawnSize::Big);
        store
            .borrow_mut()
            .insert(DrawnKind::Sprite, "zeta".to_string(), big.clone());
        let mut small = sprite_of(DrawnKind::Sprite, Some(DrawnSize::Small));
        small.size = Some(DrawnSize::Small);
        store
            .borrow_mut()
            .insert(DrawnKind::Sprite, "alpha".to_string(), small);
        store.borrow_mut().insert(
            DrawnKind::Balloon,
            "balloon-1".to_string(),
            sprite_of(DrawnKind::Balloon, None),
        );
        let sprites = asset_entries_of(&store.borrow(), DrawnKind::Sprite);
        assert_eq!(
            sprites
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["alpha", "zeta"],
            "the list is sorted"
        );
        assert_eq!(sprites[0].size, Some(DrawnSize::Small));
        assert!(asset_entries_of(&store.borrow(), DrawnKind::Grass).is_empty());
    }

    /// A save lands in the store, downloads the document and parses
    /// back.
    #[test]
    fn a_save_lands_in_the_store_and_downloads_the_document() {
        let store = test_store();
        let mut model = model_at_store(Rc::clone(&store));
        model.state.name.push_str("myduck");
        model.state.arm_save(web_time::Instant::now());
        model.tick();
        let outputs = model.take_outputs();
        let download = outputs.iter().find_map(|output| match output {
            EditorOutput::Download { name, bytes, .. } => Some((name.clone(), bytes.clone())),
            _ => None,
        });
        let (name, bytes) = download.expect("the save queued a download");
        assert_eq!(name, "myduck.json");
        let doc: agnosis_core::drawn::DrawnDoc =
            serde_json::from_slice(&bytes).expect("the download is the drawn document");
        assert_eq!(doc.kind, DrawnKind::Sprite);
        assert!(
            store.borrow().exists(DrawnKind::Sprite, "myduck"),
            "the save is in the parade's store"
        );
    }

    /// A save into a taken name is refused.
    #[test]
    fn a_save_into_a_taken_name_is_refused() {
        let store = store_with_sprite("sprite-1");
        let mut model = model_at_store(store);
        // The placeholder name is `sprite-2`: the store carries sprite-1.
        assert_eq!(model.state.default_name, "sprite-2");
        // The user types the taken name back.
        for ch in "sprite-1".chars() {
            model.state.push_name_char(ch);
        }
        model.state.arm_save(web_time::Instant::now());
        model.tick();
        assert!(
            model
                .take_outputs()
                .iter()
                .all(|output| !matches!(output, EditorOutput::Download { .. })),
            "the refused save downloads nothing"
        );
    }

    /// A click on the canvas paints, and the drag is one stroke.
    #[test]
    fn a_click_paints_and_a_drag_is_one_stroke() {
        let mut model = model();
        let layout = layout(model.buffer().0, model.buffer().1, &model.state);
        let (ox, oy) = layout.canvas_origin;
        model.pointer_down(ox + 2, oy + 2);
        assert!(model.state.stroke_is_open(), "the stroke opened");
        model.pointer_move(ox + editor::CANVAS_CELL_W as i64 * 2 + 2, oy + 2);
        model.pointer_up();
        assert!(
            model.state.canvas.get(0, 0).is_some(),
            "the first cell was painted"
        );
        assert!(model.state.undo.can_undo(), "one undo step");
    }

    /// Ctrl+Z undoes and never types into the name field.
    #[test]
    fn the_undo_shortcut_never_reaches_the_name_field() {
        let mut model = model();
        canvas_click(&mut model);
        model.pointer_up();
        model.key(KeyEvent {
            key: "z".to_string(),
            ctrl: true,
            shift: false,
            alt: false,
            meta: false,
        });
        assert_eq!(model.state.name, "", "the name field is untouched");
        assert!(!model.state.undo.can_undo(), "the stroke was undone");
    }

    /// A plain z types into the name field.
    #[test]
    fn a_plain_letter_types_into_the_name() {
        let mut model = model();
        model.key(KeyEvent::plain("z"));
        assert_eq!(model.state.name, "z");
    }

    /// The ESC ladder: a dropdown first, then the mode, then close.
    #[test]
    fn the_escape_ladder_runs_before_it_closes() {
        let mut model = model();
        model.state.open_dropdown(0);
        model.key(KeyEvent::plain("Escape"));
        assert_eq!(model.state.open_dropdown, None, "the list closed first");
        assert!(!model.take_close(), "the editor stayed open");
        model.state.enter_settings_mode_with(Config::default());
        model.key(KeyEvent::plain("Escape"));
        assert_eq!(model.state.mode, EditorMode::Draw, "the panel was left");
        assert!(!model.take_close(), "the editor stayed open");
        model.key(KeyEvent::plain("Escape"));
        assert!(model.take_close(), "a plain canvas closes the editor");
    }

    /// F2 opens the assign panel; F3 answers inert; F4 returns.
    #[test]
    fn the_function_keys_open_their_panels() {
        let mut model = model();
        model.key(KeyEvent::plain("F2"));
        assert_eq!(model.state.mode, EditorMode::Assign);
        model.key(KeyEvent::plain("F3"));
        assert_eq!(
            model.state.mode,
            EditorMode::Assign,
            "F3 never enters the strip"
        );
        assert_eq!(
            model.state.active_toast_at(web_time::Instant::now()),
            Some(STRIP_INERT),
            "F3 says the strip is v2"
        );
        model.key(KeyEvent::plain("F4"));
        assert_eq!(model.state.mode, EditorMode::Draw);
    }

    /// An assign write hands the config to the parade.
    #[test]
    fn an_assignment_hands_the_config_to_the_parade() {
        let store = store_with_sprite("sprite-1");
        let mut model = model_at_store(store);
        model.key(KeyEvent::plain("F2"));
        model.state.select_target(0);
        model.assign_asset(0);
        assert!(
            model.take_outputs().iter().any(|output| matches!(
                output,
                EditorOutput::ApplyConfig(config) if config.assignments.big_1.as_deref() == Some("sprite-1")
            )),
            "the parade got the assignment"
        );
    }

    /// The TRK button answers with the v2 toast.
    #[test]
    fn the_track_button_answers_inert() {
        let mut model = model();
        let layout = layout(model.buffer().0, model.buffer().1, &model.state);
        let button = layout.track_button;
        let hit = hit_test(&layout, &model.state, button.x + 4, button.y + 4);
        assert_eq!(hit, EditorHit::TrackButton);
        model.apply_hit(hit);
        assert_eq!(
            model.state.active_toast_at(web_time::Instant::now()),
            Some(STRIP_INERT)
        );
    }

    /// A settings change hands the config to the parade.
    #[test]
    fn a_settings_change_hands_the_config_to_the_parade() {
        let mut model = model();
        model.key(KeyEvent::plain("F1"));
        assert_eq!(model.state.mode, EditorMode::Settings);
        let row = SETTING_ROWS
            .iter()
            .position(|row| row.key == "loop_seconds")
            .expect("the row");
        model.state.settings_cursor = row;
        model.state.settings_step(1);
        model.commit_staged_setting();
        assert!(
            model
                .take_outputs()
                .iter()
                .any(|output| matches!(output, EditorOutput::ApplyConfig(_))),
            "the parade got the changed config"
        );
    }

    /// The personality action lands eleven ducks in the config.
    #[test]
    fn the_personality_action_lands_the_config() {
        let mut model = model();
        model.key(KeyEvent::plain("F1"));
        let row = SETTING_ROWS
            .iter()
            .position(|row| row.key == "randomise")
            .expect("the row");
        model.state.settings_cursor = row;
        model.key(KeyEvent::plain("Enter"));
        assert!(
            model.take_outputs().iter().any(|output| matches!(
                output,
                EditorOutput::ApplyConfig(config) if config.personality.len() == 11
            )),
            "the randomise landed eleven personalities"
        );
    }

    /// The wheel scrolls the canvas down.
    #[test]
    fn the_wheel_scrolls_the_canvas() {
        let mut model = model();
        model.wheel(0, 3);
        assert!(model.state.scroll_rows > 0, "the canvas scrolled");
    }

    /// The status line mirrors the toast first.
    #[test]
    fn the_status_line_mirrors_the_toast() {
        let mut model = model();
        model.state.set_toast("SAVED DUCK");
        assert_eq!(model.status_line(), "SAVED DUCK");
    }

    /// The render produces a full-size buffer of opaque pixels.
    #[test]
    fn the_render_fills_the_buffer() {
        let mut model = model();
        let (bytes, width, height) = model.render();
        assert_eq!(width, editor::MIN_WINDOW_W);
        assert_eq!(height, editor::MIN_WINDOW_H);
        assert_eq!(bytes.len(), (width as usize) * (height as usize) * 4);
    }
}
