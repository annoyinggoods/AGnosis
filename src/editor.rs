//! Draw-your-own editor process — the **application** half.
//!
//! The editor is a **separate process**, like the popup: the popup's
//! `PopupApp` owns a single window, a single `wgpu::Surface` and a single
//! renderer, and the roadmap (§4) keeps the parade's frame rate untouched
//! while the editor is open. `run` therefore mirrors `popup::run` almost
//! line for line: one winit event loop, one GPU-init worker thread, one
//! window, one renderer, one PID file.
//!
//! **This file is the platform half.** The editor's *data* — the state,
//! the layout, the hit test, the undo stack, the note strip and the pure
//! paint helpers — lives in [`agnosis_core::editor`], which knows nothing
//! about winit, the GPU or the file system. This file owns the window,
//! the renderer, the workers and the event loop, and it **injects** the
//! platform's services into the state
//! ([`agnosis_core::editor::EditorState::set_platform`]).
//!
//! **Borderless, not decorated.** Roadmap §5.1 says "normal, resizable,
//! decorated window", but §5.8 (and the painting stage, Stage 6) resolves
//! that wording in favour of a borderless window: `CursorMoved` reports
//! physical pixels relative to the *window* origin, and in a decorated
//! window that origin is the title bar, so the client area starts lower and
//! every click would land off by the title-bar height. The parade popup
//! already carries `platform::window::ensure_borderless(&window)` for
//! exactly this reason, and the editor calls its sibling
//! `platform::window::ensure_borderless_resizable(&window)`.
//!
//! **Borderless, but still resizable.** The editor window has no title
//! bar, no system menu and no close button — **ESC closes it** — yet the
//! user must be able to resize it (roadmap Stage 4/5: "the window is
//! resizable and the layout follows the new size"). On Windows a
//! borderless window loses its sizing border together with the caption,
//! so the editor keeps the **invisible** border and the user drags it.
//! The parade popup keeps the plain `ensure_borderless`: it covers the
//! screen and has nothing to resize.
//!
//! The Win32 window class is [`crate::platform::EDITOR_WINDOW_CLASS`]
//! (`"AGnosisEditor"`), never `"AGnosisPopup"`: the parade hotkey closes a
//! popup by looking for `"AGnosisPopup"` and the editor chord closes an
//! editor by looking for `"AGnosisEditor"`, so neither lookup can find the
//! other window (roadmap Stage 4).
//!
//! The window is **not** fullscreen: it is sized to 80 % of the monitor
//! (min [`MIN_WINDOW_W`] × [`MIN_WINDOW_H`], max 1920 × 1200) and
//! centred on it, and it is resizable. The surface is built from the
//! window's *actual* inner size, never from the monitor rect the command
//! line carries, so a resize follows the window.
//!
//! **The window has an OS-level minimum size.** The top bar has a floor
//! width — the three dropdown boxes, the name field, the two mode
//! buttons and the SAVE button do not fit into fewer pixels than that —
//! and the layout used to *drop* the name field when the window was
//! dragged below the floor while the keyboard still typed into it (audit
//! 2026-10-20 § editor, task 1). `with_min_inner_size` makes the
//! invariant "the window is never smaller than the layout needs" true by
//! construction, and [`min_layout_width`] derives the number from the
//! layout itself.
//!
//! The pure half is **re-exported** below, so every `crate::editor::…`
//! reference to a pure item — `EditorState`, `EditorMode`, `EditorHit`,
//! `layout`, `min_layout_width`, … — keeps resolving exactly as before
//! the split.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowLevel};

#[cfg(target_os = "windows")]
use winit::platform::windows::WindowAttributesExtWindows;

/// The editor's **pure** half — the state, the layout, the hit test and
/// the paint helpers.
///
/// The re-export is what keeps `crate::editor::EditorState` and every
/// other pure name resolving after the split.
pub use agnosis_core::editor::*;

/// The exit toast when the drawing had work in it: the draft has it.
const DRAFT_SAVED_CLOSING: &str = "DRAFT SAVED - CLOSING";

/// The exit toast when strip notes were never saved at all.
const TRACK_NOTES_UNSAVED: &str = "TRACK NOTES UNSAVED";

/// The panels the editor can show.
///
/// The keyboard route into a panel names the panel, so the shortcut and
/// the button cannot drift apart (audit 2026-10-22 §UX-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorPanel {
    /// The drawing canvas — the way out of every other panel.
    Draw,
    /// The settings panel.
    Settings,
    /// The assignment picker.
    Assign,
    /// The note strip.
    Sequencer,
}

/// A track job for the worker thread.
///
/// The paths are resolved **once**, when the job is queued, so the
/// worker never has to guess where the user's config folder is.
#[derive(Debug, Clone)]
struct TrackJob {
    /// What to do.
    request: TrackRequest,
    /// The folder the track files live in.
    dir: PathBuf,
    /// The user's config file.
    config: PathBuf,
}

/// The track worker: it writes and reads the track files.
///
/// The thread lives as long as the editor: the channel closes when the
/// editor drops the sender, and the loop ends.
fn spawn_track_worker() -> (
    std::sync::mpsc::Sender<TrackJob>,
    std::sync::mpsc::Receiver<TrackOutcome>,
) {
    let (job_tx, job_rx) = std::sync::mpsc::channel::<TrackJob>();
    let (out_tx, out_rx) = std::sync::mpsc::channel::<TrackOutcome>();
    std::thread::spawn(move || {
        for job in job_rx {
            let outcome = carry_out_track_request(&job);
            if out_tx.send(outcome).is_err() {
                break;
            }
        }
    });
    (job_tx, out_rx)
}

/// Carry out one track request.
///
/// The worker and the tests use the same function, so a track is written
/// the same way every time.
fn carry_out_track_request(job: &TrackJob) -> TrackOutcome {
    match &job.request {
        TrackRequest::Save { name, track } => {
            match crate::audio::sequencer::save_track(&job.dir, name, track) {
                Ok(()) => TrackOutcome::Saved { name: name.clone() },
                Err(e) => TrackOutcome::Failed {
                    action: "SAVE",
                    reason: e.to_string(),
                },
            }
        }
        TrackRequest::Load { name } => match crate::audio::sequencer::load_track(&job.dir, name) {
            Ok(track) => TrackOutcome::Loaded {
                name: name.clone(),
                track,
            },
            Err(e) => TrackOutcome::Failed {
                action: "LOAD",
                reason: e.to_string(),
            },
        },
        TrackRequest::Select { name } => match write_track_selection(&job.config, name) {
            Ok(()) => TrackOutcome::Selected { name: name.clone() },
            Err(reason) => TrackOutcome::Failed {
                action: "SELECT",
                reason,
            },
        },
    }
}

/// The word a request's failure is reported under (`SAVE`, `LOAD`,
/// `SELECT`).
fn track_action(request: &TrackRequest) -> &'static str {
    match request {
        TrackRequest::Save { .. } => "SAVE",
        TrackRequest::Load { .. } => "LOAD",
        TrackRequest::Select { .. } => "SELECT",
    }
}

/// Point the parade at the track: the `[audio] track` key is written
/// into the user's config.
///
/// Only the one key is replaced, so every comment, every unknown key
/// and every other table of the user's `config.toml` survives
/// byte-for-byte (the same discipline the settings panel follows).
fn write_track_selection(config: &std::path::Path, name: &str) -> Result<(), String> {
    if crate::audio::sequencer::validate_name(name).is_err() {
        return Err(TRACK_NAME_INVALID.to_string());
    }
    let value = format!("\"{name}\"");
    agnosis_core::config_edit::write_key(config, "audio", "track", &value)
        .map_err(|e| e.to_string())
}

/// Carry out one draft action.
///
/// Returns the error, when it failed. The worker thread and the clean
/// exit use the same function, so a draft is written the same way
/// whether it is autosaved or flushed.
fn carry_out_draft_action(dir: &std::path::Path, action: &DraftAction) -> Option<String> {
    match action {
        DraftAction::Write { kind, name, doc } => {
            agnosis_core::drawn::save_draft_to(dir, doc, *kind, name)
                .err()
                .map(|e| e.to_string())
        }
        DraftAction::Delete { kind, name } => {
            agnosis_core::drawn::delete_draft_from(dir, *kind, name)
                .err()
                .map(|e| e.to_string())
        }
    }
}

/// Retire the draft the exit path leaves behind, **before** the actions
/// are carried out.
///
/// The decision `flush_draft` used to carry inline — an `if` inside an
/// `else if`, past the nesting rule's depth (audit 2026-10-23 §F-094):
/// a dirty draft is re-queued, and a clean one is deleted **only** when
/// nothing unsaved stands behind it (audit 2026-10-22 §P0-1).
fn retire_draft_on_exit(state: &mut EditorState) {
    if state.draft_dirty {
        state.draft_dirty = false;
        state.draft_due = None;
        state.queue_draft_write();
        return;
    }
    let Some((kind, name)) = state.draft_key.clone() else {
        return;
    };
    // The draft stands behind the drawing only while the drawing is
    // still in the editor. Once the asset is saved, the draft has done
    // its job and may go.
    if state.has_unsaved_work() {
        agnosis_core::trace::trace("editor: draft kept on exit");
        return;
    }
    state.draft_actions.push(DraftAction::Delete { kind, name });
    state.draft_key = None;
}

/// A draft write that goes to the worker thread.
///
/// The write is a JSON serialisation of up to four frames — hundreds of
/// kilobytes — so it must not run on the UI thread (AGENTS.md: never
/// block the UI thread).
#[derive(Debug, Clone)]
struct DraftJob {
    /// The folder the draft belongs in.
    dir: std::path::PathBuf,
    /// What to do.
    action: DraftAction,
}

/// What the draft worker reports back.
#[derive(Debug, Clone)]
struct DraftOutcome {
    /// The asset name the draft belongs to.
    name: String,
    /// The error, when the write failed.
    error: Option<String>,
}

/// The draft worker: it writes the drafts the editor queues.
///
/// The thread lives as long as the editor: the channel closes when the
/// editor drops the sender, and the loop ends.
fn spawn_draft_worker() -> (
    std::sync::mpsc::Sender<DraftJob>,
    std::sync::mpsc::Receiver<DraftOutcome>,
) {
    let (job_tx, job_rx) = std::sync::mpsc::channel::<DraftJob>();
    let (out_tx, out_rx) = std::sync::mpsc::channel::<DraftOutcome>();
    std::thread::spawn(move || {
        for job in job_rx {
            let name = match &job.action {
                DraftAction::Write { name, .. } => name.clone(),
                DraftAction::Delete { name, .. } => name.clone(),
            };
            let error = carry_out_draft_action(&job.dir, &job.action);
            if out_tx.send(DraftOutcome { name, error }).is_err() {
                break;
            }
        }
    });
    (job_tx, out_rx)
}

/// Write one setting into the user's real config.
///
/// The production path: only the one key is replaced, so every comment,
/// every unknown key and every other setting survives byte-for-byte. The
/// write is atomic and keeps a `.bak` (audit 2026-10-20 §3.9).
///
/// The tests inject their own writer, so they never touch the user's
/// `config.toml`.
fn write_setting(section: &str, key: &str, value: &str) -> Result<(), String> {
    let path = crate::paths::config_file().map_err(|e| e.to_string())?;
    agnosis_core::config_edit::write_key(&path, section, key, value).map_err(|e| e.to_string())
}

/// Run a settings-panel **action** (the PERSONALITY buttons, roadmap
/// HELP window §2).
///
/// RANDOMISE draws the eleven duck personalities from a seed the host
/// clock provides (the core takes the seed as a parameter and never
/// reads the clock) and lands them with the surgical array-table write;
/// RESET writes an empty section, which **removes** the user's
/// `[[personality]]` block. Both write only the user's `config.toml` and
/// keep the write atomic with its `.bak`.
///
/// The tests inject their own runner, so they never touch the user's
/// `config.toml`.
fn run_personality_action(action: &str) -> Result<(), String> {
    let path = crate::paths::config_file().map_err(|e| e.to_string())?;
    match action {
        "randomise" => {
            let seed = host_clock_seed();
            let drawn = agnosis_core::personality::random_personalities(seed);
            let entries = agnosis_core::personality::array_entries(&drawn);
            agnosis_core::config_edit::write_array_table(&path, "personality", &entries)
                .map_err(|e| e.to_string())
        }
        "reset" => agnosis_core::config_edit::write_array_table(&path, "personality", &[])
            .map_err(|e| e.to_string()),
        other => Err(format!("unknown action {other}")),
    }
}

/// A seed for the personality randomiser, from the host clock.
fn host_clock_seed() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(std::process::id() as u64, |d| d.as_nanos() as u64)
}

/// Write what an assign-panel action asks for.
///
/// The parade slots replace the `[assignments]` table; the grass tile
/// and the decorations write one scalar key each — the same surgical
/// single-key write the settings panel uses (decision F-162).
fn write_assign_write(write: agnosis_core::editor::AssignWrite) -> Result<(), String> {
    match write {
        agnosis_core::editor::AssignWrite::Assignments(table) => write_assignments(&table),
        agnosis_core::editor::AssignWrite::Key {
            section,
            key,
            value,
        } => write_setting(section, key, &value),
    }
}

/// Read the user's config **as the user wrote it** (no repair pass).
///
/// The loader the state is given: the sanitizing loader drops every
/// assignment whose drawn asset does not exist (yet), and the editor
/// writes the table back — so the editor needs the unsanitized copy
/// (audit 2026-10-20 §3.3).
fn load_config_for_edit() -> Result<agnosis_core::config::Config, agnosis_core::config::ConfigError>
{
    crate::host::load_config_for_edit()
}

/// Write the assignment table into the user's real config.
///
/// The production path: only the `[assignments]` table of the user's
/// `config.toml` is replaced, so every comment, every unknown key and
/// every other table survives byte-for-byte. The write is atomic and
/// keeps a `.bak` (audit 2026-10-20 §3.3 and §3.9).
///
/// The table is written as it stands in memory, and the memory copy is
/// loaded **unsanitized** — the sanitizing loader drops the assignments
/// whose drawn asset does not exist (yet), and writing that back would
/// delete the user's pending work.
///
/// The tests inject their own writer, so they never touch the user's
/// `config.toml`.
fn write_assignments(table: &agnosis_core::config::AssignmentsConfig) -> Result<(), String> {
    let path = crate::paths::config_file().map_err(|e| e.to_string())?;
    agnosis_core::config_edit::write_assignments(&path, table).map_err(|e| e.to_string())
}

/// The editor window: one winit window, one grid, one renderer.
///
/// The grid and the [`agnosis_core::font::Font`] always use the *same* cell size
/// (the UI layer's 6 × 10 px): a mismatch puts the glyphs off-frame and the
/// window stays black (AGENTS.md).
pub struct EditorApp {
    args: EditorArgs,
    window: Option<Arc<Window>>,
    /// The base layer: the canvas and the tool panel, in big cells.
    base_grid: Option<agnosis_core::grid::Grid>,
    /// The overlay layer: the dropdowns, the name field and the status
    /// line, in small cells.
    overlay_grid: Option<agnosis_core::grid::Grid>,
    /// The rasterizer for the base grid's cell size.
    base_font: Option<agnosis_core::font::Font>,
    /// The rasterizer for the overlay grid's cell size.
    overlay_font: Option<agnosis_core::font::Font>,
    /// The editor's state: the dropdowns, the canvas and the name.
    state: EditorState,
    /// The last pointer position in window pixels. The window is
    /// borderless, so the window origin is the surface origin and the
    /// pixels map 1:1 onto the canvas (roadmap §5.8).
    pointer: (i64, i64),
    /// The modifier keys that are down.
    ///
    /// winit reports the modifiers **separately** from the key event, so
    /// the editor has to remember them: `Ctrl+Z` is a `Key::Character`
    /// and would otherwise land in the name field (roadmap Extension 2
    /// §2.1).
    modifiers: winit::keyboard::ModifiersState,
    /// The folder the drafts live in; `None` when it could not be
    /// resolved, in which case the editor runs without an autosave.
    drafts_dir: Option<std::path::PathBuf>,
    /// The channel the draft writes go down.
    draft_tx: Option<std::sync::mpsc::Sender<DraftJob>>,
    /// The channel the draft writes come back on.
    draft_rx: std::sync::mpsc::Receiver<DraftOutcome>,
    /// The folder the track files live in; `None` when it could not be
    /// resolved, in which case the strip runs without a track folder.
    tracks_dir: Option<std::path::PathBuf>,
    /// The user's config file, where the `[audio] track` key is written.
    config_path: Option<std::path::PathBuf>,
    /// The channel the track jobs go down.
    track_tx: Option<std::sync::mpsc::Sender<TrackJob>>,
    /// The channel the track outcomes come back on.
    track_rx: std::sync::mpsc::Receiver<TrackOutcome>,
    /// The settings row the pointer is dragging, if any.
    settings_drag: Option<usize>,
    /// The writer the settings panel puts its changes through.
    settings_writer: SettingsWriter,
    /// The runner the settings panel's **actions** go through (the
    /// PERSONALITY buttons).
    settings_action: SettingsAction,
    /// The window's inner size in pixels.
    inner: (u32, u32),
    /// The GPU life-cycle ([`Gpu`]): the same machine the popup runs
    /// (audit 101126 §S3).
    gpu: Gpu,
    /// Grid geometry `(base_cols, base_rows, ui_cols, ui_rows)` the
    /// current grids were built for.
    geometry: (usize, usize, usize, usize),
    /// True until the first successful draw (one trace line, not one per
    /// frame).
    first_frame: bool,
    /// When the next frame is due.
    next_frame: Instant,
    exit: bool,
}

impl EditorApp {
    /// One editor frame: advance the clock, poll the GPU, draw.
    fn tick(&mut self) {
        if self.exit {
            return;
        }
        let now = Instant::now();
        if now < self.next_frame {
            return;
        }
        self.catch_up_deadline(now);
        self.pump_drafts(now);
        self.pump_track_jobs();
        self.poll_gpu();
        self.draw_frame();
        // The armed save runs **after** the frame painted, so the
        // `SAVING...` toast is on screen while the write blocks the
        // thread, and the next tick paints `SAVED` (decision F-163).
        self.run_armed_save(now);
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Complete the save the SAVE press armed (decision F-163).
    ///
    /// The write itself stays synchronous and atomic inside this tick —
    /// no worker, no channel — but it runs one frame after the press, so
    /// the `SAVING...` toast can paint first and a slow write on an old
    /// machine reads as deliberate work, not as a hang.
    fn run_armed_save(&mut self, now: Instant) {
        if !self.state.armed_save {
            return;
        }
        self.state.armed_save = false;
        self.state.primary_action(now);
    }

    /// Carry out the draft actions the state queued.
    ///
    /// The write itself happens on the **worker thread**: it is a JSON
    /// serialisation of up to four frames, and the UI thread must never
    /// block on it (AGENTS.md).
    fn pump_drafts(&mut self, now: Instant) {
        if self.state.draft_is_due(now) {
            self.state.draft_dirty = false;
            self.state.draft_due = None;
            self.state.queue_draft_write();
        }
        let actions = self.state.take_draft_actions();
        for action in actions {
            self.queue_draft_job(action);
        }
        self.drain_draft_outcomes();
    }

    /// Hand one draft action to the worker thread.
    fn queue_draft_job(&mut self, action: DraftAction) {
        let Some(dir) = self.drafts_dir.clone() else {
            return;
        };
        let Some(tx) = self.draft_tx.as_ref() else {
            return;
        };
        if tx.send(DraftJob { dir, action }).is_err() {
            agnosis_core::trace::warn("editor: the draft worker is gone");
            self.draft_tx = None;
        }
    }

    /// Report the draft writes that finished.
    fn drain_draft_outcomes(&mut self) {
        while let Ok(outcome) = self.draft_rx.try_recv() {
            let Some(error) = outcome.error else {
                continue;
            };
            agnosis_core::trace::warn(&format!("editor: draft {} failed: {error}", outcome.name));
            self.state.set_toast(&format!("DRAFT FAILED: {error}"));
        }
    }

    /// Carry out the track request the state queued, if there is one.
    ///
    /// The **write** happens on the worker thread: a track file is a
    /// JSON document and an atomic rename, and the UI thread must never
    /// wait for the disk (AGENTS.md).
    fn pump_track_jobs(&mut self) {
        if let Some(request) = self.state.take_track_request() {
            self.queue_track_request(request);
        }
        self.drain_track_outcomes();
    }

    /// Hand one track request to the worker thread.
    ///
    /// The paths are resolved **here**, on the UI thread, so the worker
    /// never has to look for the user's config folder.
    fn queue_track_request(&mut self, request: TrackRequest) {
        let action = track_action(&request);
        let Some(dir) = self.tracks_dir.clone() else {
            self.report_track_failure(action, "NO TRACK FOLDER");
            return;
        };
        let Some(config) = self.config_path.clone() else {
            self.report_track_failure(action, "NO CONFIG FILE");
            return;
        };
        let Some(tx) = self.track_tx.as_ref() else {
            self.report_track_failure(action, "NO TRACK WORKER");
            return;
        };
        if tx
            .send(TrackJob {
                request,
                dir,
                config,
            })
            .is_err()
        {
            agnosis_core::trace::warn("editor: the track worker is gone");
            self.track_tx = None;
            self.report_track_failure(action, "NO TRACK WORKER");
        }
    }

    /// Report a track write that never started.
    fn report_track_failure(&mut self, action: &'static str, reason: &str) {
        let outcome = TrackOutcome::Failed {
            action,
            reason: reason.to_string(),
        };
        let now = Instant::now();
        self.state.apply_track_outcome(&outcome, now);
    }

    /// Report the track writes that finished.
    fn drain_track_outcomes(&mut self) {
        while let Ok(outcome) = self.track_rx.try_recv() {
            let now = Instant::now();
            self.state.apply_track_outcome(&outcome, now);
        }
    }

    /// Offer the newest draft of the current kind, if there is one.
    ///
    /// The offer is a **question**, not a silent load: a surprise is
    /// worse than a prompt (roadmap Extension 2 §2.2). The folder is
    /// read **once**, here — the editor must not scan it per frame.
    fn offer_the_newest_draft(&mut self) {
        let Some(dir) = self.drafts_dir.clone() else {
            return;
        };
        let kind = self.state.kind;
        let Some(entry) = agnosis_core::drawn::list_drafts(&dir, kind)
            .into_iter()
            .next()
        else {
            return;
        };
        self.state.offer_draft(entry.name.as_str());
    }

    /// Write the draft one last time, before the editor goes away.
    ///
    /// A clean exit must not lose the work: the draft is written
    /// **synchronously** here, because the worker thread may not get the
    /// job before the process ends (roadmap Extension 2 §2.2).
    ///
    /// An editor that leaves nothing behind takes its draft with it: a
    /// clean exit with no unsaved changes removes the draft it wrote.
    ///
    /// **The draft is only removed when there is nothing behind it**
    /// (audit 2026-10-22 §P0-1). The debounce clears `draft_dirty` when
    /// the autosave fires, so "not dirty" means "the autosave already
    /// ran" — it does **not** mean "the user saved the asset". Deleting
    /// on that reading loses the drawing: draw → wait more than the
    /// debounce → ESC, and the work is gone.
    fn flush_draft(&mut self) {
        retire_draft_on_exit(&mut self.state);
        let Some(dir) = self.drafts_dir.clone() else {
            return;
        };
        for action in self.state.take_draft_actions() {
            if let Some(error) = carry_out_draft_action(&dir, &action) {
                agnosis_core::trace::warn(&format!("editor: draft action failed: {error}"));
            }
        }
    }

    /// Advance `next_frame` past `now` so one slow frame never spirals.
    fn catch_up_deadline(&mut self, now: Instant) {
        let interval = frame_interval();
        while self.next_frame <= now {
            self.next_frame += interval;
        }
    }

    /// Draw one frame: paint the two layers and hand them to the
    /// renderer. A missing piece is a no-op.
    fn draw_frame(&mut self) {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        self.paint_layers(&layout);
        let Gpu::Ready(renderer) = &mut self.gpu else {
            return;
        };
        let Some(base_font) = self.base_font.as_mut() else {
            return;
        };
        let Some(overlay_font) = self.overlay_font.as_mut() else {
            return;
        };
        let Some(base) = self.base_grid.as_ref() else {
            return;
        };
        let Some(overlay) = self.overlay_grid.as_ref() else {
            return;
        };
        // The overlay is blitted on top of the base, so a dropdown list
        // can overlap the canvas (roadmap §4).
        match renderer.draw_layers(base, base_font, Some((overlay, overlay_font))) {
            Ok(()) => self.mark_first_frame(),
            Err(e) => agnosis_core::trace::trace(&format!("editor.draw error: {e}")),
        }
    }

    /// Paint the base layer (canvas + tool panel) and the overlay layer
    /// (dropdowns, name field, status line).
    fn paint_layers(&mut self, layout: &EditorLayout) {
        if let Some(grid) = self.base_grid.as_mut() {
            grid.clear();
            paint_base(grid, layout, &self.state);
        }
        if let Some(grid) = self.overlay_grid.as_mut() {
            grid.clear();
            paint_overlay(grid, layout, &self.state);
        }
    }

    /// First successful draw: trace once for the startup smoke log.
    fn mark_first_frame(&mut self) {
        if self.first_frame {
            return;
        }
        self.first_frame = true;
        agnosis_core::trace::trace("editor.first-frame");
    }

    /// Rebuild the two grids for a new window size.
    ///
    /// Each grid keeps its own layer's cell size (the base grid the
    /// canvas' 16 × 24 px, the overlay grid the UI's 6 × 10 px) — a grid
    /// and its font must share the cell size, or the glyph masks no
    /// longer match the cell pitch and the frame goes black (AGENTS.md).
    fn rebuild_grids(&mut self, width: u32, height: u32) {
        let base_cols = usize::try_from((width / CANVAS_CELL_W).max(1)).unwrap_or(1);
        let base_rows = usize::try_from((height / CANVAS_CELL_H).max(1)).unwrap_or(1);
        let ui_cols = usize::try_from((width / UI_CELL_W).max(1)).unwrap_or(1);
        let ui_rows = usize::try_from((height / UI_CELL_H).max(1)).unwrap_or(1);
        if (base_cols, base_rows, ui_cols, ui_rows) == self.geometry {
            return;
        }
        self.geometry = (base_cols, base_rows, ui_cols, ui_rows);
        self.base_grid = Some(agnosis_core::grid::Grid::new(
            base_cols,
            base_rows,
            CANVAS_CELL_W,
            CANVAS_CELL_H,
        ));
        self.overlay_grid = Some(agnosis_core::grid::Grid::new(
            ui_cols, ui_rows, UI_CELL_W, UI_CELL_H,
        ));
    }

    /// Build the two fonts, one per layer.
    ///
    /// A font is rasterized for exactly one cell size, so the base layer
    /// needs the canvas' 16 × 24 px font and the overlay layer the UI's
    /// 6 × 10 px one. Both cell sizes are **constants**, so this runs
    /// **once**, when the window is built — the fonts can never drift
    /// from the grids, and a resize costs no re-rasterization
    /// (audit 2026-10-23 §F-112).
    fn rebuild_fonts(&mut self) {
        match agnosis_core::font::Font::new(CANVAS_CELL_W, CANVAS_CELL_H) {
            Ok(font) => self.base_font = Some(font),
            Err(e) => agnosis_core::trace::trace(&format!("editor.base-font error: {e}")),
        }
        match agnosis_core::font::Font::new(UI_CELL_W, UI_CELL_H) {
            Ok(font) => self.overlay_font = Some(font),
            Err(e) => agnosis_core::trace::trace(&format!("editor.overlay-font error: {e}")),
        }
    }

    /// Poll the GPU init thread; a failed or vanished init degrades to the
    /// black window instead of panicking (audit §poll_gpu).
    fn poll_gpu(&mut self) {
        let Gpu::Awaiting(receiver) = &self.gpu else {
            return;
        };
        let init = match try_recv_init(Some(receiver)) {
            GpuPoll::Ready(init) => init,
            GpuPoll::Pending => return,
            GpuPoll::Gone => {
                self.degraded("editor.gpu init unavailable, degraded");
                return;
            }
        };
        // The channel is spent: the machine moves on to Ready or Dead
        // and never awaits the same init twice (audit 101126 §S3).
        self.gpu = Gpu::Dead;
        agnosis_core::trace::trace("editor.device-ready");
        let window = match self.window.as_ref() {
            Some(window) => Arc::clone(window),
            None => {
                self.degraded("editor.gpu window gone, degraded");
                return;
            }
        };
        let surface = match init.instance.create_surface(Arc::clone(&window)) {
            Ok(surface) => surface,
            Err(e) => {
                self.degraded(&format!("editor.surface error, degraded: {e}"));
                return;
            }
        };
        // The surface follows the window, not the monitor rect the command
        // line carries (roadmap §5.1).
        let inner = window.inner_size();
        let (width, height) = (inner.width.max(1), inner.height.max(1));
        let config =
            match crate::render::pick_surface_config(&surface, &init.adapter, width, height) {
                Some(config) => config,
                None => {
                    self.degraded("editor.surface no-compatible-format, degraded");
                    return;
                }
            };
        match crate::render::Renderer::new(init.device, init.queue, surface, config, width, height)
        {
            Ok(renderer) => {
                agnosis_core::trace::trace("editor.renderer-ready");
                self.gpu = Gpu::Ready(Box::new(renderer));
            }
            Err(e) => self.degraded(&format!("editor.renderer error, degraded: {e}")),
        }
    }

    /// Drop GPU init and keep the window alive (black, but running).
    ///
    /// The degradation is **honest** (audit 2026-10-23 §F-092): the
    /// window's title says so — the OS-level escape route, the same
    /// shape the popup uses — and the status line carries the word, so a
    /// black editor window is never mute. `Dead` is terminal: the
    /// announcement fires exactly once (audit 101126 §S3).
    fn degraded(&mut self, msg: &str) {
        agnosis_core::trace::trace(msg);
        self.gpu = Gpu::Dead;
        if let Some(window) = self.window.as_ref() {
            window.set_title("AGnosis - GPU UNAVAILABLE");
        }
        self.state.set_toast("NO GPU - PRESS ESC TO EXIT");
    }

    /// ESC / window close: mark exit and leave the event loop.
    ///
    /// The exit carries the pending work out first, so a clean exit never
    /// loses it: the draft (synchronously — the worker may not get the
    /// job before the process ends) and a **pending track save** — a SAVE
    /// pressed a moment before ESC must not die with the window
    /// (audit 2026-10-23 §F-108). Notes that were never saved at all are
    /// announced instead of eaten in silence (§F-105).
    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        self.flush_draft();
        // An armed save is still owed: the one-frame window between the
        // press and the tick must never eat a save (decision F-163).
        if self.state.armed_save {
            self.state.armed_save = false;
            self.state.primary_action(Instant::now());
        }
        self.state.flush_track_request();
        if self.state.has_unsaved_work() {
            self.state.set_toast(DRAFT_SAVED_CLOSING);
        }
        if self.state.track_unsaved() {
            // The closing window may never paint the toast; the trace
            // line is the copy that survives (the editor has no
            // exit-reason file to write to).
            self.state.set_toast(TRACK_NOTES_UNSAVED);
            agnosis_core::trace::warn("editor: track notes were never saved");
        }
        self.exit = true;
        event_loop.exit();
    }

    /// Resize handler: the renderer follows the window, and so do the two
    /// grids (roadmap §5.1: the window is resizable and the layout
    /// follows the new size).
    ///
    /// The **fonts** are NOT rebuilt here: they rasterize the *constant*
    /// cell sizes (the canvas' 16 × 24 px and the UI's 6 × 10 px), so a
    /// window resize cannot change them — a rebuild per `Resized` event
    /// re-rasterized both fonts for nothing, tens of milliseconds on the
    /// UI thread (audit 2026-10-23 §F-112).
    fn on_resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        self.inner = (width, height);
        if let Gpu::Ready(renderer) = &mut self.gpu {
            renderer.resize(width, height);
        }
        self.rebuild_grids(width, height);
    }

    /// One pressed key: ESC closes the editor, Enter advances the frame
    /// or saves, the arrow and page keys scroll the canvas and a printable
    /// character goes to the name field (roadmap §5.7).
    ///
    /// The **left and right** arrows scroll the canvas sideways: a grass
    /// tile's canvas is 200 columns wide, so the horizontal scroll is the
    /// only way to reach the far end of it (roadmap Extension 1 §10.1).
    fn on_keyboard_input(&mut self, event_loop: &ActiveEventLoop, event: winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        let key = event.logical_key;
        if matches!(key, Key::Named(NamedKey::Escape)) {
            self.on_escape(event_loop);
            return;
        }
        // The undo shortcut comes **before** the name field: `Ctrl+Z` is
        // a plain character key, and it must never be typed into the
        // name (roadmap Extension 2 §2.1).
        if let Some(action) = self.undo_shortcut(&key) {
            self.apply_undo_action(action);
            return;
        }
        // The panel shortcuts come **before** the name field: they are
        // function keys, so they never collide with a typed character.
        //
        // WHY they exist: the panels were reachable only with the mouse
        // (audit 2026-10-22 §UX-10). A user who does not know where the
        // buttons are could not reach the settings, the assignment
        // picker or the note strip at all.
        if let Some(panel) = self.panel_shortcut(&key) {
            self.open_panel(panel);
            return;
        }
        // The settings panel's own keys come **before** the scroll keys
        // (audit 2026-10-23 §F-067): `Left`/`Right` step the cursor row's
        // value, `Enter`/`Space` activate it. Without this branch the
        // arrows were consumed by the canvas scroll and `Enter` only ever
        // committed a focused text field — the panel was keyboard-dead.
        if self.state.mode == EditorMode::Settings && self.settings_panel_key(&key) {
            return;
        }
        if let Some(delta) = self.scroll_delta(&key) {
            self.on_scroll_key(delta);
            return;
        }
        if let Some(delta) = scroll_columns_delta(&key) {
            // In the settings panel Left/Right step the **value** of the
            // row the cursor is on — they do not scroll the canvas. The
            // arrows were consumed by the scroll and discarded, so every
            // slider, toggle and dropdown was mouse-only
            // (audit 2026-10-22 §UX-01).
            if self.state.mode == EditorMode::Settings {
                self.step_setting_value(delta);
                return;
            }
            self.scroll_view_columns(delta);
            return;
        }
        if matches!(key, Key::Named(NamedKey::Enter)) {
            self.on_enter();
            return;
        }
        if self.state.mode == EditorMode::Settings {
            self.settings_text_key(&key);
            return;
        }
        self.on_name_key(key);
    }

    /// The `Escape` key — a **ladder**, one rung per state
    /// (audit 2026-10-23 §F-116): an open choice list closes first, a
    /// focused text field commits, a pending draft offer is declined,
    /// a panel is left, and only a plain canvas closes the editor.
    ///
    /// The middle rungs are the fixes: `Escape` with an open dropdown
    /// used to quit the whole editor (§F-116), and a draft offer was
    /// unanswerable in the negative — `ENTER` loaded it and `Escape`
    /// threw the window away, so the status line was held hostage until
    /// the user gave in (§F-080, the core's `decline_draft` wired here;
    /// the draft **file** is kept, so declining now and loading later is
    /// still possible).
    fn on_escape(&mut self, event_loop: &ActiveEventLoop) {
        if self.escape_ladder() {
            return;
        }
        self.quit(event_loop);
    }

    /// The rungs ESC walks before it may close the window; true when a
    /// rung consumed the key.
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
        if self.state.draft_offer.is_some() {
            self.state.decline_draft();
            return true;
        }
        if self.state.mode != crate::editor::EditorMode::Draw {
            self.state.leave_any_mode();
            return true;
        }
        false
    }

    /// A key that scrolls the canvas.
    ///
    /// In the settings panel the arrow keys move the cursor instead, so
    /// the two never fight over the same key.
    fn on_scroll_key(&mut self, delta: i32) {
        if self.state.mode == EditorMode::Settings {
            self.settings_key(delta);
            return;
        }
        self.scroll_view(delta);
    }

    /// One key in the settings panel.
    ///
    /// The **up and down** arrows move the cursor, the **left and right**
    /// arrows change the value of a slider or step through a choice, and
    /// a printable character goes into the focused text field.
    fn settings_key(&mut self, delta: i32) {
        if self.state.mode != EditorMode::Settings {
            return;
        }
        self.state.move_settings_cursor(delta);
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        self.keep_the_cursor_visible(&layout);
    }

    /// One key in the settings panel that is not an arrow.
    fn settings_text_key(&mut self, key: &Key) {
        if self.state.mode != EditorMode::Settings {
            return;
        }
        match key {
            Key::Character(text) => self.state.push_settings_text(text.as_str()),
            Key::Named(NamedKey::Backspace) => self.state.pop_settings_text(),
            _ => {}
        }
    }

    /// The undo action a key press asks for, if it asks for one.
    ///
    /// `Ctrl+Z` undoes and `Ctrl+Shift+Z` redoes on every platform; the
    /// macOS `Cmd+Z` spelling is an alias there, and `Ctrl+Y` redoes
    /// everywhere (roadmap Extension 2 §2.1).
    fn undo_shortcut(&self, key: &Key) -> Option<UndoAction> {
        let Key::Character(text) = key else {
            return None;
        };
        let ch = text.chars().next()?;
        if !ch.eq_ignore_ascii_case(&'z') && !ch.eq_ignore_ascii_case(&'y') {
            return None;
        }
        let ctrl = self
            .modifiers
            .contains(winit::keyboard::ModifiersState::CONTROL);
        let cmd = cfg!(target_os = "macos")
            && self
                .modifiers
                .contains(winit::keyboard::ModifiersState::SUPER);
        if !ctrl && !cmd {
            return None;
        }
        if ch.eq_ignore_ascii_case(&'y') {
            return Some(UndoAction::Redo);
        }
        let shift = self
            .modifiers
            .contains(winit::keyboard::ModifiersState::SHIFT);
        if shift {
            Some(UndoAction::Redo)
        } else {
            Some(UndoAction::Undo)
        }
    }

    /// Carry out an undo or a redo.
    ///
    /// The strip has its **own** history: a note is not a cell, so the
    /// canvas's stack cannot hold it (roadmap Extension 3 §3.2).
    fn apply_undo_action(&mut self, action: UndoAction) {
        if self.state.mode == EditorMode::Sequencer {
            self.apply_strip_undo(action);
            return;
        }
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

    /// Undo or redo a note change on the strip.
    fn apply_strip_undo(&mut self, action: UndoAction) {
        let available = match action {
            UndoAction::Undo => self.state.strip_undo.can_undo(),
            UndoAction::Redo => self.state.strip_undo.can_redo(),
        };
        let changed = match action {
            UndoAction::Undo => self.state.undo_note(),
            UndoAction::Redo => self.state.redo_note(),
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

    /// The scroll delta of a scroll key, if the key is one.
    ///
    /// A page is the number of visible canvas rows (roadmap §5.7).
    fn scroll_delta(&self, key: &Key) -> Option<i32> {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        let page = i32::try_from(layout.canvas_view_rows.max(1)).unwrap_or(1);
        match key {
            Key::Named(NamedKey::ArrowUp) => Some(-1),
            Key::Named(NamedKey::ArrowDown) => Some(1),
            Key::Named(NamedKey::PageUp) => Some(-page),
            Key::Named(NamedKey::PageDown) => Some(page),
            _ => None,
        }
    }

    /// Scroll the canvas by `delta` rows.
    fn scroll_canvas(&mut self, delta: i32) {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        self.state.scroll_by(delta, layout.canvas_view_rows);
    }

    /// Scroll the view: the canvas in draw mode, the assign panel's
    /// lists in assign mode, the settings panel in settings mode, the
    /// panel in strip mode.
    ///
    /// The strip itself is **horizontal**, so the vertical scroll has
    /// nothing to move: it scrolls the side panel instead.
    fn scroll_view(&mut self, delta: i32) {
        if self.state.mode == EditorMode::Assign {
            self.scroll_assign_lists(delta);
            return;
        }
        if self.state.mode == EditorMode::Settings {
            self.scroll_settings(delta);
            return;
        }
        if self.state.mode == EditorMode::Sequencer {
            self.scroll_strip_panel(delta);
            return;
        }
        self.scroll_canvas(delta);
    }

    /// Scroll the pitch palette (and the track list) of the strip mode.
    fn scroll_strip_panel(&mut self, delta: i32) {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        self.state.scroll_panel_by(delta, layout.canvas_view_rows);
    }

    /// Scroll the canvas sideways.
    ///
    /// The assign panel's lists and the settings panel are vertical, so
    /// the horizontal scroll does nothing there. The strip is a
    /// **horizontal** strip, so the sideways scroll is the one that
    /// moves it (roadmap Extension 3 §3.2).
    fn scroll_view_columns(&mut self, delta: i32) {
        if matches!(self.state.mode, EditorMode::Assign | EditorMode::Settings) {
            return;
        }
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        self.state.scroll_by_cols(delta, layout.canvas_view_cols);
    }

    /// Scroll the settings panel and keep the cursor on screen.
    fn scroll_settings(&mut self, delta: i32) {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        self.state
            .scroll_settings_by(delta, layout.settings_visible);
        self.keep_the_cursor_visible(&layout);
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

    /// Scroll the slot list or the asset list, depending on where the
    /// pointer is.
    fn scroll_assign_lists(&mut self, delta: i32) {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        if layout.slot_list.contains(self.pointer.0, self.pointer.1) {
            self.state
                .scroll_slot_list_by(delta, visible_slot_rows(&layout));
            return;
        }
        self.state
            .scroll_asset_list_by(delta, layout.canvas_view_rows);
    }

    /// Route one key to the name field.
    ///
    /// The name only accepts `a-z0-9-_` (roadmap §3.7); every other
    /// character is dropped by [`EditorState::push_name_char`]. In assign
    /// mode the name belongs to the drawing, so the field is not edited
    /// there. In **strip mode** the field is the **track's** name, and
    /// the drawing's name is left alone.
    fn on_name_key(&mut self, key: Key) {
        if self.state.mode == EditorMode::Assign {
            return;
        }
        if self.state.mode == EditorMode::Sequencer {
            self.track_name_key(key);
            return;
        }
        match key {
            Key::Character(text) => self.push_name_text(&text),
            Key::Named(NamedKey::Backspace) => self.state.pop_name_char(),
            _ => {}
        }
    }

    /// One key in the track name field.
    fn track_name_key(&mut self, key: Key) {
        match key {
            Key::Character(text) => {
                let Some(ch) = text.chars().next() else {
                    return;
                };
                self.state.push_track_name_char(ch);
            }
            Key::Named(NamedKey::Backspace) => self.state.pop_track_name_char(),
            _ => {}
        }
    }

    /// Append the first character of `text` to the name field.
    fn push_name_text(&mut self, text: &str) {
        let Some(ch) = text.chars().next() else {
            return;
        };
        self.state.push_name_char(ch);
    }

    /// The primary action: the next frame, or the save.
    ///
    /// In **strip mode** the primary action is the track's save — the
    /// drawing is never saved from the strip (roadmap Extension 3 §3.2).
    fn primary_action(&mut self) {
        if self.state.mode == EditorMode::Sequencer {
            self.save_track();
            return;
        }
        // The press **arms** the save (decision F-163): the next frame
        // paints `SAVING...`, and the tick after it completes the write
        // through `run_armed_save`.
        self.state.arm_save(Instant::now());
    }

    /// The Enter key.
    ///
    /// In draw mode it advances the frame or saves (roadmap §5.6); in
    /// **assign mode** it assigns the picked asset — never the drawing,
    /// because the SAVE button is inert there (audit 2026-10-20 § editor,
    /// task 2); in **strip mode** it points the parade at the track.
    ///
    /// A pending draft offer comes first: the question "there is a draft,
    /// shall I load it?" is answered before the editor does anything
    /// else (roadmap Extension 2 §2.2).
    fn on_enter(&mut self) {
        if self.state.mode == EditorMode::Settings {
            self.commit_settings_text();
            return;
        }
        if self.state.draft_offer.is_some() && self.state.mode == EditorMode::Draw {
            self.load_offered_draft();
            return;
        }
        let now = Instant::now();
        // The draw-mode Enter is the SAVE press: it **arms** the save
        // (decision F-163), exactly the way the button does.
        if self.state.mode == EditorMode::Draw {
            self.state.arm_save(now);
            return;
        }
        // The listing comes from the **cache** the existence predicate
        // keeps: a key handler must not walk the user's asset folder
        // (audit 2026-10-21 §G18).
        let exists = |kind: agnosis_core::drawn::DrawnKind, name: &str| {
            crate::host::drawn_list_cached(kind)
                .iter()
                .any(|taken| taken.name == name)
        };
        let save = |sprite: &agnosis_core::drawn::DrawnSprite,
                    kind: agnosis_core::drawn::DrawnKind,
                    name: &str| crate::host::drawn_save(sprite, kind, name);
        self.state
            .enter_action_with(now, exists, save, write_assign_write);
    }

    /// Load the draft the editor offered.
    ///
    /// The draft is **not** loaded silently: the user is asked first, and
    /// the Enter key is the answer (roadmap Extension 2 §2.2).
    fn load_offered_draft(&mut self) {
        let Some(name) = self.state.draft_offer.clone() else {
            return;
        };
        let Some(dir) = self.drafts_dir.clone() else {
            self.state.draft_offer = None;
            self.state.set_toast("NO DRAFT FOLDER");
            return;
        };
        let kind = self.state.kind;
        match agnosis_core::drawn::load_draft_from(&dir, kind, name.as_str()) {
            Ok(Some(doc)) => {
                self.state.load_draft(&doc);
                self.state.set_toast(DRAFT_LOADED);
            }
            Ok(None) => {
                self.state.draft_offer = None;
                self.state.set_toast("NO DRAFT");
            }
            Err(e) => {
                self.state.draft_offer = None;
                self.state.set_toast(&format!("DRAFT FAILED: {e}"));
            }
        }
    }

    /// The left button went down.
    ///
    /// A press on the canvas opens a **stroke**: every cell painted until
    /// the button comes up is one undo step (roadmap Extension 2 §2.1).
    /// A press anywhere else is an ordinary click.
    fn on_press(&mut self) {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        let hit = hit_test(&layout, &self.state, self.pointer.0, self.pointer.1);
        if self.list_swallows(hit) {
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
    fn on_release(&mut self) {
        self.state.end_stroke();
        self.settings_drag = None;
    }

    /// The pointer moved.
    ///
    /// While a stroke is open the pointer paints: a drag is **one**
    /// stroke, not one stroke per cell (roadmap Extension 2 §2.1).
    fn on_cursor_moved(&mut self, x: i64, y: i64) {
        self.pointer = (x, y);
        if let Some(row) = self.settings_drag {
            self.drag_setting(row);
            return;
        }
        if !self.state.stroke_is_open() {
            return;
        }
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        let hit = hit_test(&layout, &self.state, self.pointer.0, self.pointer.1);
        if let EditorHit::Canvas { col, row } = hit {
            self.state.paint(col, row);
        }
    }

    /// Act on one hit.
    fn apply_hit(&mut self, hit: EditorHit) {
        if self.list_swallows(hit) {
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
                self.state.pick_option(dropdown, option)
            }
            EditorHit::Tool(index) => self.state.set_tool(index),
            EditorHit::Color(index) => self.state.set_color(index),
            EditorHit::Canvas { col, row } => self.state.paint(col, row),
            // The name is edited with the keyboard; the field itself has
            // no click behaviour.
            EditorHit::NameField => {}
            EditorHit::PrimaryButton => self.primary_action(),
            EditorHit::AssignButton => self.enter_assign_mode(),
            EditorHit::DrawButton => self.leave_any_mode(),
            EditorHit::SettingsButton => self.enter_settings_mode(),
            EditorHit::Slot(index) => self.state.select_target(index),
            EditorHit::Asset(index) => self.assign_asset(index),
            EditorHit::ClearButton => self.clear_assignment(),
            EditorHit::SettingSlider(row) => self.drag_setting(row),
            EditorHit::SettingToggle(row) => self.toggle_setting(row),
            EditorHit::SettingText(row) => self.focus_setting_text(row),
            EditorHit::SettingChoice(row) => self.open_setting_choice(row),
            EditorHit::SettingAction(row) => self.run_settings_action(row),
            EditorHit::SettingOption { row, option } => self.pick_setting_option(row, option),
            EditorHit::TrackButton => self.enter_sequencer_mode(),
            EditorHit::Pitch(index) => self.state.select_pitch(index),
            EditorHit::NoteCell(step) => self.state.place_note(step),
            EditorHit::Track(index) => self.load_track(index),
            EditorHit::None => {}
        }
    }

    /// Switch the editor into the settings panel.
    fn enter_settings_mode(&mut self) {
        self.state.enter_settings_mode();
    }

    /// The panel a function key opens, if any.
    ///
    /// `F1` opens the settings, `F2` the assignment picker and `F3` the
    /// note strip. `F4` goes back to the canvas — the same way out as
    /// the DRAW button.
    ///
    /// WHY function keys: they cannot be typed into the name field, they
    /// are free of every other shortcut, and they are the same on every
    /// platform (audit 2026-10-22 §UX-10).
    fn panel_shortcut(&self, key: &winit::keyboard::Key) -> Option<EditorPanel> {
        let named = match key {
            winit::keyboard::Key::Named(named) => *named,
            _ => return None,
        };
        match named {
            winit::keyboard::NamedKey::F1 => Some(EditorPanel::Settings),
            winit::keyboard::NamedKey::F2 => Some(EditorPanel::Assign),
            winit::keyboard::NamedKey::F3 => Some(EditorPanel::Sequencer),
            winit::keyboard::NamedKey::F4 => Some(EditorPanel::Draw),
            _ => None,
        }
    }

    /// Open the panel a shortcut names.
    fn open_panel(&mut self, panel: EditorPanel) {
        match panel {
            EditorPanel::Settings => self.enter_settings_mode(),
            EditorPanel::Assign => self.enter_assign_mode(),
            EditorPanel::Sequencer => self.enter_sequencer_mode(),
            EditorPanel::Draw => self.state.leave_any_mode(),
        }
    }

    /// The value of a slider row at the pointer's position.
    ///
    /// The fraction is measured on the **track**, so a click on the
    /// value text at the right of the cell lands on the end of the
    /// range instead of somewhere in the middle of the column.
    fn setting_slider_value(&self, row: usize) -> Option<SettingValue> {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        let visible = row.checked_sub(layout.settings_scroll)?;
        let track = settings_track_rect(layout.settings, visible);
        if track.w == 0 {
            return None;
        }
        let fraction = ((self.pointer.0 - track.x) as f32) / (track.w as f32);
        self.state.slider_value(row, fraction)
    }

    /// Set a slider row from the pointer's position.
    ///
    /// The row stays the **drag target** until the button comes up, so
    /// the slider follows the pointer.
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

    /// Step the value of the settings row the cursor is on.
    ///
    /// Left/Right in the settings panel: the keyboard's way to work a
    /// slider, a toggle or a dropdown (audit 2026-10-22 §UX-01). The
    /// core **stages** the new value ([`EditorState::settings_step`]) and
    /// the app commits it through its own writer — a text row has no
    /// value to step, so the step **focuses the field** instead, the
    /// same thing a click on the field does.
    fn step_setting_value(&mut self, delta: i32) {
        if self.state.settings_step(delta) {
            self.commit_staged_setting();
            return;
        }
        let row = self.state.settings_cursor;
        self.focus_setting_text(row);
    }

    /// Write the keyboard's staged settings edit through the writer.
    ///
    /// The core stages, the app writes — the same write-first,
    /// apply-on-success rule the mouse path follows, so a failed write
    /// leaves the panel showing the old value (contract C→D).
    fn commit_staged_setting(&mut self) {
        let Some((row, value)) = self.state.take_staged_setting() else {
            return;
        };
        self.change_setting(row, value);
    }

    /// One settings-panel key that is not the cursor's Up/Down.
    ///
    /// `Enter` and `Space` **activate** the cursor row (contract C→D,
    /// the core's [`EditorState::settings_activate`]): a toggle flips, a
    /// choice opens — or closes — its option list, a text row takes the
    /// focus; a focused field is committed instead. `Left`/`Right` step
    /// the value. True when the key belonged to the panel.
    fn settings_panel_key(&mut self, key: &Key) -> bool {
        if matches!(
            key,
            Key::Named(NamedKey::Enter) | Key::Named(NamedKey::Space)
        ) {
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
            Key::Named(NamedKey::ArrowLeft) => -1,
            Key::Named(NamedKey::ArrowRight) => 1,
            _ => return false,
        };
        self.step_setting_value(delta);
        true
    }

    /// Is the settings cursor on an **action** row?
    ///
    /// An action row answers Enter/Space with the action itself, not
    /// with a staged value (the PERSONALITY buttons).
    fn cursor_row_is_action(&self) -> bool {
        self.state
            .settings_row()
            .is_some_and(|row| matches!(row.kind, SettingKind::Action(_)))
    }

    /// Run the action of an action row through the app's runner.
    fn run_settings_action(&mut self, row: usize) {
        let now = Instant::now();
        let EditorApp {
            state,
            settings_action,
            ..
        } = self;
        state.run_settings_action_with(row, now, |action| settings_action(action));
    }

    /// Focus a text row so the keyboard edits it.
    fn focus_setting_text(&mut self, row: usize) {
        self.state.focus_text_row(row);
    }

    /// Open the option list of a choice row.
    fn open_setting_choice(&mut self, row: usize) {
        self.state.settings_open = Some(row);
    }

    /// Pick an option of an open choice row and write it.
    fn pick_setting_option(&mut self, row: usize, option: usize) {
        self.state.settings_open = None;
        self.change_setting(row, SettingValue::Choice(option));
    }

    /// Write one setting into the user's config.
    fn change_setting(&mut self, row: usize, value: SettingValue) {
        let now = Instant::now();
        let EditorApp {
            state,
            settings_writer,
            ..
        } = self;
        state.change_setting_with(row, value, now, |section, key, value| {
            settings_writer(section, key, value)
        });
    }

    /// Write the focused text field into the user's config.
    ///
    /// The Enter key commits the field; the text is written even when it
    /// is empty, so a field can be cleared.
    fn commit_settings_text(&mut self) {
        let Some(row) = self.state.settings_focus else {
            return;
        };
        let text = self.state.settings_text.clone();
        self.state.unfocus_text();
        self.change_setting(row, SettingValue::Text(text));
    }

    /// Switch the editor into assign mode (roadmap Stage 9).
    fn enter_assign_mode(&mut self) {
        self.state.enter_assign_mode();
    }

    /// Switch the editor into the note strip (roadmap Extension 3 §3.2).
    ///
    /// The button is inert while the strip is already open: re-entering
    /// the mode must never throw the notes away.
    fn enter_sequencer_mode(&mut self) {
        if self.state.mode == EditorMode::Sequencer {
            return;
        }
        self.state.enter_sequencer_mode();
    }

    /// Save the track: the file is written by the **worker thread**.
    ///
    /// The request is queued, so the UI thread never waits for the disk;
    /// the outcome comes back as a toast (roadmap Extension 3 §3.4).
    fn save_track(&mut self) {
        if !self.state.request_track_save() {
            self.state
                .set_toast(&format!("SAVE FAILED: {TRACK_NAME_INVALID}"));
        }
    }

    /// Read a track file into the strip.
    fn load_track(&mut self, index: usize) {
        let Some(name) = self.state.tracks.get(index).cloned() else {
            return;
        };
        if !self.state.request_track_load(name.as_str()) {
            self.state
                .set_toast(&format!("LOAD FAILED: {TRACK_NAME_INVALID}"));
        }
    }

    /// Switch the editor back into draw mode.
    fn leave_any_mode(&mut self) {
        self.state.leave_any_mode();
    }

    /// Assign the picked asset to the selected slot and write the config.
    fn assign_asset(&mut self, index: usize) {
        self.state.select_asset(index);
        let now = Instant::now();
        self.state.assign_action_with(now, write_assign_write);
    }

    /// Remove the assignment of the selected slot and write the config.
    fn clear_assignment(&mut self) {
        let now = Instant::now();
        self.state.clear_action_with(now, write_assign_write);
    }

    /// True when the open option list swallows the click.
    ///
    /// Only an option of the open list is acted on; every other click
    /// closes the list (roadmap §5.3) without changing the dropdown's
    /// value. Two exceptions keep the assign panel reachable
    /// (roadmap Stage 9): the mode buttons are never swallowed (they
    /// switch the mode, and the switch closes the list anyway), and in
    /// assign mode no list is open at all.
    fn list_swallows(&self, hit: EditorHit) -> bool {
        if matches!(
            hit,
            EditorHit::AssignButton
                | EditorHit::DrawButton
                | EditorHit::SettingsButton
                | EditorHit::TrackButton
        ) {
            return false;
        }
        if self.state.mode == EditorMode::Assign {
            return false;
        }
        if self.state.mode == EditorMode::Settings {
            return false;
        }
        if self.state.mode == EditorMode::Sequencer {
            return false;
        }
        let Some(open) = self.state.open_dropdown else {
            return false;
        };
        !matches!(hit, EditorHit::DropdownOption { dropdown, .. } if dropdown == open)
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

    /// One wheel notch: scroll the canvas, the tool panel, the assign
    /// panel's lists, or — in strip mode — the strip **sideways**.
    ///
    /// A **sideways** wheel (a trackpad's horizontal gesture) scrolls the
    /// canvas sideways — the grass tile's canvas is 200 columns wide, so
    /// the horizontal scroll has to be reachable by the mouse too.
    ///
    /// The strip is a horizontal strip, so a plain wheel over it moves it
    /// sideways; over the side panel the panel scrolls, as it always
    /// did (roadmap Extension 3 §3.2).
    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let layout = layout(self.inner.0, self.inner.1, &self.state);
        let columns = wheel_columns(delta);
        if columns != 0 {
            self.scroll_view_columns(columns);
            return;
        }
        let lines = wheel_lines(delta);
        if self.state.mode == EditorMode::Assign {
            self.scroll_assign_lists(lines);
            return;
        }
        if self.state.mode == EditorMode::Settings {
            self.scroll_settings(lines);
            return;
        }
        if layout.tools.contains(self.pointer.0, self.pointer.1) {
            self.state.scroll_panel_by(lines, layout.canvas_view_rows);
            return;
        }
        if self.state.mode == EditorMode::Sequencer {
            self.scroll_view_columns(lines);
            return;
        }
        self.state.scroll_by(lines, layout.canvas_view_rows);
    }
}

/// The horizontal scroll delta of a wheel notch, in columns.
///
/// A vertical wheel reports no horizontal movement, so the sideways
/// gesture never fights the vertical scroll.
fn wheel_columns(delta: MouseScrollDelta) -> i32 {
    match delta {
        MouseScrollDelta::LineDelta(dx, _) => dx as i32,
        MouseScrollDelta::PixelDelta(pos) => (pos.x as f32 / CANVAS_CELL_W as f32).round() as i32,
    }
}

/// The wheel delta in lines: positive scrolls **down** (the view moves
/// towards the canvas' bottom).
fn wheel_lines(delta: MouseScrollDelta) -> i32 {
    match delta {
        MouseScrollDelta::LineDelta(_, dy) => -(dy as i32),
        MouseScrollDelta::PixelDelta(pos) => {
            -((pos.y as f32 / CANVAS_CELL_H as f32).round() as i32)
        }
    }
}

/// The horizontal scroll delta of a scroll key, if the key is one.
///
/// The left and right arrows move the canvas sideways (roadmap
/// Extension 1 §10.1): a grass tile's canvas is 200 columns wide and
/// has to be scrollable without a scrollbar.
fn scroll_columns_delta(key: &Key) -> Option<i32> {
    match key {
        Key::Named(NamedKey::ArrowLeft) => Some(-1),
        Key::Named(NamedKey::ArrowRight) => Some(1),
        _ => None,
    }
}

/// Outcome of one non-blocking GPU-init poll.
///
/// The outcome and the poll live in [`crate::render`], where both
/// windows can reach them: one definition, two callers (audit
/// 2026-10-21 §G26).
use crate::render::{try_recv_init, Gpu, GpuPoll};

/// The editor's frame interval.
fn frame_interval() -> Duration {
    Duration::from_nanos(1_000_000_000 / u64::from(EDITOR_FPS))
}

impl ApplicationHandler for EditorApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // The PID file goes first, before the window is built: the
        // daemon's editor toggle looks for it to decide whether an editor
        // is running, and a window that takes a while to appear left a
        // gap in which a second editor could be spawned (audit
        // 2026-10-20 §3.18).
        if let Err(e) = crate::platform::window::write_editor_pid_file() {
            agnosis_core::trace::trace(&format!("editor.pid error: {e}"));
        }
        let min_width = balloon_canvas_width(&self.state);
        let (width, height) = editor_window_rect_for(self.args.size.0, self.args.size.1, min_width);
        let origin = editor_window_origin(
            self.args.origin,
            self.args.size.0,
            self.args.size.1,
            min_width,
        );
        let attrs = Window::default_attributes()
            // The title is the OS-level escape route (audit 2026-10-23
            // §F-103): the window is borderless, but the task bar, the
            // alt-tab list and the OS all show the name. The class —
            // what the daemon looks the window up by — is unchanged.
            .with_title(crate::platform::EDITOR_WINDOW_TITLE)
            .with_inner_size(PhysicalSize::new(width, height))
            // The layout has a floor: the top bar's controls do not fit
            // into fewer pixels than `min_layout_width`, and a window
            // below that floor would hide the name field the user is
            // typing into (audit 2026-10-20 § editor, task 1). The OS
            // enforces the floor, so the invariant holds by
            // construction.
            .with_min_inner_size(PhysicalSize::new(MIN_WINDOW_W, MIN_WINDOW_H))
            .with_position(PhysicalPosition::new(origin.0, origin.1))
            .with_decorations(false)
            .with_resizable(true)
            .with_transparent(false)
            .with_visible(false)
            .with_active(true)
            .with_window_level(WindowLevel::Normal);
        #[cfg(target_os = "windows")]
        let attrs = attrs.with_class_name(crate::platform::EDITOR_WINDOW_CLASS);
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                agnosis_core::trace::trace(&format!("editor.window error: {e}"));
                self.exit = true;
                event_loop.exit();
                return;
            }
        };
        agnosis_core::trace::trace("editor.window-created");
        // Black background before the first show: no white flash.
        if let Err(e) = crate::platform::window::paint_black_before_show(&window) {
            agnosis_core::trace::trace(&format!("editor.paint-black error: {e}"));
        }
        window.set_visible(true);
        agnosis_core::trace::trace("editor.shown-black");
        window.set_decorations(false);
        window.set_resizable(true);
        // Borderless, because `CursorMoved` reports pixels relative to the
        // window origin (roadmap §5.8) — but *resizable*, so the user
        // can drag the invisible border and the layout follows the new
        // size (roadmap Stage 4/5). This runs last: `set_resizable(true)`
        // re-adds the thick frame, and only this helper keeps it.
        crate::platform::window::ensure_borderless_resizable(&window);
        agnosis_core::trace::trace("editor.borderless-enforced");
        // The editor owns the keyboard (ESC closes it), so it takes the
        // foreground the same way the popup does.
        crate::platform::window::focus_ladder(&window, false);
        let inner = window.inner_size();
        self.inner = (inner.width, inner.height);
        self.rebuild_grids(inner.width, inner.height);
        self.rebuild_fonts();
        self.window = Some(window);
        self.next_frame = Instant::now();
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => self.quit(event_loop),
            WindowEvent::KeyboardInput { event, .. } => self.on_keyboard_input(event_loop, event),
            // The pointer position is remembered so the click that
            // follows can be routed; the window is borderless, so the
            // pixels map 1:1 onto the surface (roadmap §5.8).
            WindowEvent::CursorMoved { position, .. } => {
                self.on_cursor_moved(position.x as i64, position.y as i64);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button != MouseButton::Left {
                    return;
                }
                if state == ElementState::Pressed {
                    self.on_press();
                } else {
                    self.on_release();
                }
            }
            // The modifiers arrive on their own event, so the editor
            // remembers them: `Ctrl+Z` is a plain character key and would
            // otherwise be typed into the name field (roadmap Extension 2
            // §2.1).
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::RedrawRequested => {
                self.tick();
                if !self.exit {
                    event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
                }
            }
            WindowEvent::Resized(size) => {
                self.on_resize(size.width, size.height);
            }
            _ => {}
        }
    }

    /// Pump one frame when the window exists.
    ///
    /// Flattened with guard clauses (audit nesting rule): the "no window"
    /// and the "the tick asked us to quit" cases return early, so no `if`
    /// ever sits inside another `if`.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit {
            event_loop.exit();
            return;
        }
        if self.window.is_none() {
            return;
        }
        self.tick();
        if self.exit {
            return;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

/// Run the editor until ESC / window close. Returns Ok on clean exit.
///
/// The GPU init runs on its own thread and the event loop only polls the
/// result, so the window is never blocked waiting for an adapter (the same
/// single-flight pattern the popup uses).
pub fn run(cfg: &agnosis_core::config::Config, args: &EditorArgs) -> Result<(), EditorError> {
    // The config drives the balloon's canvas geometry (the disc), so it is
    // read here and handed to the state.
    agnosis_core::trace::trace(&format!("editor.spawn fps_cap={}", cfg.fps_cap));
    validate_args(args)?;
    let event_loop =
        EventLoop::new().map_err(|e| EditorError::Window(format!("event loop: {e}")))?;
    let gpu_rx = crate::render::spawn_device_thread();
    // The draft worker runs for as long as the editor does: the channel
    // closes when the editor drops the sender.
    let (draft_tx, draft_rx) = spawn_draft_worker();
    let drafts_dir = crate::paths::drafts_dir().ok();
    // The track worker does the same for the track files: a track is
    // written on its own thread, so the UI thread never waits for the
    // disk.
    let (track_tx, track_rx) = spawn_track_worker();
    let tracks_dir = crate::paths::tracks_dir().ok();
    let config_path = crate::paths::config_file().ok();
    // The pure state cannot reach the platform, so the platform's
    // services are **injected**: the asset folder's lister, the saver
    // and the user's config loader. The state stays a library that
    // knows nothing about where the platform keeps its files.
    // The clock is a **parameter**: the core cannot call `Instant::now`
    // on its own, because the standard library panics on
    // `wasm32-unknown-unknown` (audit 2026-10-21 §E3). The shell hands
    // the real clock over.
    let mut state = EditorState::new(cfg, Instant::now);
    state.set_platform(EditorPlatform {
        next_free_name: Some(crate::host::drawn_next_free_name),
        list_assets: Some(crate::host::drawn_list_cached),
        save_asset: Some(crate::host::drawn_save),
        load_config: Some(load_config_for_edit),
    });
    // The folder the MIDI tracks live in, for the same reason.
    state.set_tracks_dir(tracks_dir.clone());
    let mut app = EditorApp {
        args: *args,
        window: None,
        base_grid: None,
        overlay_grid: None,
        base_font: None,
        overlay_font: None,
        state,
        pointer: (0, 0),
        modifiers: winit::keyboard::ModifiersState::empty(),
        drafts_dir,
        draft_tx: Some(draft_tx),
        draft_rx,
        tracks_dir,
        config_path,
        track_tx: Some(track_tx),
        track_rx,
        settings_drag: None,
        settings_writer: Box::new(write_setting),
        settings_action: Box::new(run_personality_action),
        inner: (0, 0),
        gpu: Gpu::Awaiting(gpu_rx),
        geometry: (0, 0, 0, 0),
        first_frame: false,
        next_frame: Instant::now(),
        exit: false,
    };
    app.offer_the_newest_draft();
    let ran = event_loop.run_app(&mut app);
    // The pid file goes even when the event loop failed: the `?` used to
    // return first and leave a record naming a dead process behind
    // (audit 2026-10-22 §P2-14). The clear only takes our own record.
    crate::platform::window::clear_editor_pid_file();
    ran.map_err(|e| EditorError::Window(format!("event loop: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh editor state for the default config.
    fn state() -> EditorState {
        EditorState::new(&agnosis_core::config::Config::default(), Instant::now)
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

    /// An editor app without a window, for the grid/font tests.
    fn app_for_tests() -> EditorApp {
        EditorApp {
            args: EditorArgs {
                origin: (0, 0),
                size: (1920, 1080),
            },
            window: None,
            base_grid: None,
            overlay_grid: None,
            base_font: None,
            overlay_font: None,
            state: state(),
            pointer: (0, 0),
            modifiers: winit::keyboard::ModifiersState::empty(),
            drafts_dir: None,
            draft_tx: None,
            draft_rx: std::sync::mpsc::channel().1,
            tracks_dir: None,
            config_path: None,
            track_tx: None,
            track_rx: std::sync::mpsc::channel().1,
            settings_drag: None,
            // The tests never touch the user's real config file.
            settings_writer: Box::new(|_, _, _| Ok(())),
            settings_action: Box::new(|_| Ok(())),
            inner: (0, 0),
            gpu: Gpu::Dead,
            geometry: (0, 0, 0, 0),
            first_frame: false,
            next_frame: Instant::now(),
            exit: false,
        }
    }

    #[test]
    fn a_clean_exit_keeps_the_draft_of_an_unsaved_drawing() {
        // The regression test for the P0 (audit 2026-10-22 §P0-1): draw,
        // wait out the autosave debounce, then ESC. The debounce clears
        // `draft_dirty`, so the exit path must not read that as "the user
        // saved the asset" — the draft has to survive.
        let mut app = app_for_tests();
        app.state.paint(4, 5);
        // The autosave has run: the flag is cleared and the draft key is
        // set, exactly as `pump_drafts` leaves it.
        app.state.draft_dirty = false;
        app.state.draft_due = None;
        app.state.queue_draft_write();
        let actions = app.state.take_draft_actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, DraftAction::Write { .. })),
            "the autosave wrote the draft"
        );
        app.flush_draft();
        let actions = app.state.take_draft_actions();
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, DraftAction::Delete { .. })),
            "the exit must not delete the draft of an unsaved drawing"
        );
    }

    #[test]
    fn a_clean_exit_removes_the_draft_of_a_blank_editor() {
        // The other half of the contract: an editor that leaves nothing
        // behind takes its draft with it.
        let mut app = app_for_tests();
        app.state.queue_draft_write();
        app.state.take_draft_actions();
        app.flush_draft();
        let actions = app.state.take_draft_actions();
        assert!(
            actions
                .iter()
                .any(|a| matches!(a, DraftAction::Delete { .. })),
            "a blank editor removes the draft it wrote"
        );
    }

    #[test]
    fn the_grids_and_the_fonts_share_their_cell_size() {
        // A mismatched font size puts the glyphs off-frame and the window
        // stays black (AGENTS.md): each grid and its font use one cell
        // size.
        let mut app = app_for_tests();
        app.rebuild_grids(1536, 864);
        app.rebuild_fonts();
        let base = app.base_grid.as_ref().expect("base grid");
        assert_eq!(base.cell_w(), CANVAS_CELL_W);
        assert_eq!(base.cell_h(), CANVAS_CELL_H);
        assert_eq!(base.cols(), 1536 / CANVAS_CELL_W as usize);
        let overlay = app.overlay_grid.as_ref().expect("overlay grid");
        assert_eq!(overlay.cell_w(), UI_CELL_W);
        assert_eq!(overlay.cell_h(), UI_CELL_H);
        assert_eq!(overlay.cols(), 1536 / UI_CELL_W as usize);
        let base_font = app.base_font.as_ref().expect("base font");
        assert_eq!(base_font.cell_w(), CANVAS_CELL_W);
        assert_eq!(base_font.cell_h(), CANVAS_CELL_H);
        let overlay_font = app.overlay_font.as_ref().expect("overlay font");
        assert_eq!(overlay_font.cell_w(), UI_CELL_W);
        assert_eq!(overlay_font.cell_h(), UI_CELL_H);
    }

    #[test]
    fn a_resize_rebuilds_both_layers() {
        let mut app = app_for_tests();
        app.rebuild_grids(1536, 864);
        app.rebuild_grids(1024, 768);
        let base = app.base_grid.as_ref().expect("base grid");
        assert_eq!(base.cols(), 1024 / CANVAS_CELL_W as usize);
        assert_eq!(base.rows(), 768 / CANVAS_CELL_H as usize);
        let overlay = app.overlay_grid.as_ref().expect("overlay grid");
        assert_eq!(overlay.cols(), 1024 / UI_CELL_W as usize);
        assert_eq!(overlay.rows(), 768 / UI_CELL_H as usize);
    }

    #[test]
    fn frame_interval_is_thirty_fps() {
        assert_eq!(
            frame_interval(),
            Duration::from_nanos(1_000_000_000 / 30),
            "the editor runs at the parade's default rate"
        );
    }

    #[test]
    fn a_click_paints_the_canvas() {
        let mut app = app_for_tests();
        app.state.set_tool(0);
        app.state.set_color(1);
        app.apply_hit(EditorHit::Canvas { col: 2, row: 3 });
        assert_eq!(
            app.state.canvas.get(2, 3),
            Some(('X', agnosis_core::drawn::PALETTE[1])),
            "a click on a cell paints it"
        );
    }

    #[test]
    fn clicking_outside_the_open_list_closes_it_without_changing_the_value() {
        let mut app = app_for_tests();
        app.state.open_dropdown(0);
        let kind = app.state.kind;
        app.apply_hit(EditorHit::Canvas { col: 1, row: 1 });
        assert_eq!(app.state.open_dropdown, None, "the list closes");
        assert_eq!(app.state.kind, kind, "the value does not change");
    }

    // -----------------------------------------------------------------
    // Stage 9 — the ASSIGN panel
    // -----------------------------------------------------------------
    /// An empty assignment table.
    fn no_assignments() -> agnosis_core::config::AssignmentsConfig {
        agnosis_core::config::AssignmentsConfig::default()
    }

    #[test]
    fn the_assign_panel_is_not_blocked_by_an_open_list() {
        let mut app = app_for_tests();
        app.state.open_dropdown(0);
        // In draw mode the open list still swallows the clicks.
        assert!(app.list_swallows(EditorHit::Canvas { col: 1, row: 1 }));
        // The mode buttons are never swallowed: they switch the mode.
        assert!(!app.list_swallows(EditorHit::AssignButton));
        assert!(!app.list_swallows(EditorHit::DrawButton));
        // In assign mode nothing is swallowed at all.
        app.state.mode = EditorMode::Assign;
        assert!(!app.list_swallows(EditorHit::Slot(0)));
        assert!(!app.list_swallows(EditorHit::Asset(0)));
    }

    #[test]
    fn a_slot_click_selects_the_slot_and_the_mode_switches_back() {
        let mut app = app_for_tests();
        app.state.mode = EditorMode::Assign;
        app.state
            .enter_assign_mode_with(vec![], vec![], no_assignments());
        app.apply_hit(EditorHit::Slot(3));
        assert_eq!(app.state.assign_target, 3, "the slot is selected");
        app.apply_hit(EditorHit::DrawButton);
        assert_eq!(
            app.state.mode,
            EditorMode::Draw,
            "the DRAW button switches back"
        );
    }

    #[test]
    fn the_arrow_keys_and_the_wheel_move_the_canvas_sideways() {
        assert_eq!(
            scroll_columns_delta(&Key::Named(NamedKey::ArrowLeft)),
            Some(-1)
        );
        assert_eq!(
            scroll_columns_delta(&Key::Named(NamedKey::ArrowRight)),
            Some(1)
        );
        assert_eq!(scroll_columns_delta(&Key::Named(NamedKey::ArrowUp)), None);
        // A sideways wheel notch moves the canvas sideways.
        assert_eq!(
            wheel_columns(MouseScrollDelta::LineDelta(2.0, 0.0)),
            2,
            "a sideways wheel scrolls the columns"
        );
        assert_eq!(
            wheel_columns(MouseScrollDelta::LineDelta(0.0, 3.0)),
            0,
            "a vertical wheel does not"
        );
        // The app routes the horizontal scroll to the state.
        let mut app = app_for_tests();
        app.state.set_kind(agnosis_core::drawn::DrawnKind::Grass);
        app.scroll_view_columns(4);
        assert_eq!(app.state.scroll_cols, 4);
    }

    #[test]
    fn the_undo_shortcut_is_the_control_combination() {
        let mut app = app_for_tests();
        // A plain `z` is a name character, not an undo.
        assert_eq!(app.undo_shortcut(&Key::Character("z".into())), None);
        app.modifiers = winit::keyboard::ModifiersState::CONTROL;
        assert_eq!(
            app.undo_shortcut(&Key::Character("z".into())),
            Some(UndoAction::Undo)
        );
        app.modifiers =
            winit::keyboard::ModifiersState::CONTROL | winit::keyboard::ModifiersState::SHIFT;
        assert_eq!(
            app.undo_shortcut(&Key::Character("z".into())),
            Some(UndoAction::Redo)
        );
        app.modifiers = winit::keyboard::ModifiersState::CONTROL;
        assert_eq!(
            app.undo_shortcut(&Key::Character("y".into())),
            Some(UndoAction::Redo)
        );
        assert_eq!(app.undo_shortcut(&Key::Character("a".into())), None);
    }

    #[test]
    fn the_undo_shortcut_never_reaches_the_name_field() {
        let mut app = app_for_tests();
        app.modifiers = winit::keyboard::ModifiersState::CONTROL;
        let action = app
            .undo_shortcut(&Key::Character("z".into()))
            .expect("the shortcut is recognised");
        app.apply_undo_action(action);
        assert_eq!(app.state.name, "", "the name field is untouched");
    }

    #[test]
    fn the_editor_offers_the_draft_it_finds() {
        let root = temp_root("draft-offer");
        let mut state = state();
        state.paint(9, 9);
        let doc = state.draft_doc();
        agnosis_core::drawn::save_draft_to(&root, &doc, state.kind, "sprite-offer")
            .expect("the write");

        let mut app = app_for_tests();
        app.drafts_dir = Some(root.clone());
        app.offer_the_newest_draft();
        assert_eq!(app.state.draft_offer.as_deref(), Some("sprite-offer"));
        // The offer is a question, not a silent load: the canvas is still
        // empty.
        assert_eq!(app.state.canvas.get(9, 9), Some((' ', [255, 255, 255])));

        // The user answers with the Enter key.
        app.load_offered_draft();
        assert_eq!(app.state.canvas.get(9, 9), Some(('x', [255, 255, 255])));
        assert!(app.state.draft_offer.is_none(), "the offer is answered");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_draft_of_another_kind_is_not_offered() {
        let root = temp_root("draft-kind");
        let state = state();
        let doc = state.draft_doc();
        agnosis_core::drawn::save_draft_to(
            &root,
            &doc,
            agnosis_core::drawn::DrawnKind::Balloon,
            "balloon-1",
        )
        .expect("the write");

        let mut app = app_for_tests();
        app.drafts_dir = Some(root.clone());
        app.offer_the_newest_draft();
        assert!(
            app.state.draft_offer.is_none(),
            "a balloon draft is not offered in the sprite editor"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_settings_button_switches_the_mode() {
        let state = state();
        let layout = layout(1536, 864, &state);
        let button = layout.settings_button;
        let hit = hit_test(&layout, &state, button.x + 1, button.y + 1);
        assert_eq!(hit, EditorHit::SettingsButton);
        // The mode buttons are never swallowed by an open list.
        let mut app = app_for_tests();
        app.state.open_dropdown(0);
        assert!(!app.list_swallows(EditorHit::SettingsButton));
    }

    #[test]
    fn clicking_away_commits_the_text_field() {
        let mut app = app_for_tests();
        app.state
            .enter_settings_mode_with(agnosis_core::config::Config::default());
        let row = SETTING_ROWS
            .iter()
            .position(|row| row.key == "exit_hint")
            .expect("the row");
        app.state.focus_text_row(row);
        app.state.push_settings_text("BYE");
        // A click on another row commits the value: the text is
        // written, not dropped.
        app.apply_hit(EditorHit::SettingToggle(0));
        assert!(
            app.state.settings_focus.is_none(),
            "the field is not focused any more"
        );
        assert_eq!(
            app.state.settings_value(row),
            Some(SettingValue::Text("BYE".to_string())),
            "the value was written"
        );
    }

    #[test]
    fn the_settings_panel_writes_through_the_injected_writer() {
        let mut app = app_for_tests();
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = written.clone();
        app.settings_writer = Box::new(move |section, key, value| {
            let mut sink = sink.lock().expect("the lock");
            sink.push((section.to_string(), key.to_string(), value.to_string()));
            Ok(())
        });
        app.state
            .enter_settings_mode_with(agnosis_core::config::Config::default());
        let row = SETTING_ROWS
            .iter()
            .position(|row| row.key == "loop_seconds")
            .expect("the row");
        app.apply_hit(EditorHit::SettingSlider(row));
        let written = written.lock().expect("the lock");
        assert_eq!(written.len(), 1, "one key is written");
        assert_eq!(written[0].1, "loop_seconds");
        assert!(
            written[0].2.parse::<f32>().is_ok(),
            "the value is rendered as TOML: {:?}",
            written[0]
        );
    }

    #[test]
    fn the_track_button_switches_the_mode() {
        let mut app = app_for_tests();
        let layout = layout(1536, 864, &app.state);
        let hit = hit_test(
            &layout,
            &app.state,
            layout.track_button.x + 4,
            layout.track_button.y + 4,
        );
        assert_eq!(hit, EditorHit::TrackButton);
        app.apply_hit(hit);
        assert_eq!(app.state.mode, EditorMode::Sequencer);
        // And the DRAW button leaves it again.
        app.apply_hit(EditorHit::DrawButton);
        assert_eq!(app.state.mode, EditorMode::Draw);
    }

    // -----------------------------------------------------------------
    // The ESC ladder and the settings keyboard (audit 2026-10-23)
    // -----------------------------------------------------------------

    /// `Escape` closes an open choice list **first** — it used to quit
    /// the whole editor instead (audit 2026-10-23 §F-116).
    #[test]
    fn the_escape_ladder_closes_a_dropdown_first() {
        let mut app = app_for_tests();
        app.state.open_dropdown(0);
        assert!(app.escape_ladder(), "the dropdown rung consumes the key");
        assert_eq!(app.state.open_dropdown, None, "the list is closed");
    }

    /// `Escape` with a pending draft offer **declines** it — the offer
    /// used to be unanswerable in the negative, and `Escape` threw the
    /// window away instead (audit 2026-10-23 §F-080). The file stays.
    #[test]
    fn the_escape_ladder_declines_the_draft_offer() {
        let mut app = app_for_tests();
        app.state.offer_draft("sprite-1");
        assert!(app.escape_ladder(), "the offer rung consumes the key");
        assert!(app.state.draft_offer.is_none(), "the offer is declined");
    }

    /// `Escape` leaves a panel before it may quit the editor.
    #[test]
    fn the_escape_ladder_leaves_a_panel_before_it_quits() {
        let mut app = app_for_tests();
        app.state
            .enter_settings_mode_with(agnosis_core::config::Config::default());
        assert!(app.escape_ladder(), "the panel rung consumes the key");
        assert_eq!(app.state.mode, EditorMode::Draw, "back to the canvas");
    }

    /// What the recording writer wrote: `(section, key, value)` triples.
    type RecordedWrites = std::sync::Arc<std::sync::Mutex<Vec<(String, String, String)>>>;

    /// A writer that records what the panel wrote.
    fn recording_writer() -> (SettingsWriter, RecordedWrites) {
        let written = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = written.clone();
        let writer = Box::new(move |section: &str, key: &str, value: &str| {
            let mut sink = sink.lock().expect("the lock");
            sink.push((section.to_string(), key.to_string(), value.to_string()));
            Ok(())
        });
        (writer, written)
    }

    /// The settings panel answers the keyboard: `Enter` on a Bool row
    /// flips it and the flipped value is written through the app's
    /// writer (audit 2026-10-23 §F-067 — the panel was keyboard-dead).
    #[test]
    fn the_settings_panel_answers_enter_on_a_toggle() {
        let mut app = app_for_tests();
        app.state
            .enter_settings_mode_with(agnosis_core::config::Config::default());
        let (writer, written) = recording_writer();
        app.settings_writer = writer;
        let row = SETTING_ROWS
            .iter()
            .position(|row| matches!(row.kind, SettingKind::Bool))
            .expect("a bool row");
        app.state.settings_cursor = row;
        assert!(
            app.settings_panel_key(&Key::Named(NamedKey::Enter)),
            "Enter belongs to the panel"
        );
        let written = written.lock().expect("the lock");
        assert_eq!(written.len(), 1, "the flipped value is written");
        assert_eq!(written[0].1, SETTING_ROWS[row].key);
    }

    /// `Space` activates the panel's cursor row instead of typing into
    /// the name field (audit 2026-10-23 §F-067).
    #[test]
    fn space_in_the_settings_panel_activates_instead_of_typing() {
        let mut app = app_for_tests();
        app.state
            .enter_settings_mode_with(agnosis_core::config::Config::default());
        let row = SETTING_ROWS
            .iter()
            .position(|row| matches!(row.kind, SettingKind::Bool))
            .expect("a bool row");
        app.state.settings_cursor = row;
        app.settings_panel_key(&Key::Named(NamedKey::Space));
        assert_eq!(app.state.name, "", "Space never reaches the name field");
    }

    /// `Left`/`Right` step a numeric row's value through the staged
    /// setting (contract C→D), and the step is written.
    #[test]
    fn the_arrows_step_a_numeric_row() {
        let mut app = app_for_tests();
        app.state
            .enter_settings_mode_with(agnosis_core::config::Config::default());
        let (writer, written) = recording_writer();
        app.settings_writer = writer;
        let row = SETTING_ROWS
            .iter()
            .position(|row| {
                matches!(
                    row.kind,
                    SettingKind::Int { .. } | SettingKind::Float { .. }
                )
            })
            .expect("a numeric row");
        app.state.settings_cursor = row;
        assert!(
            app.settings_panel_key(&Key::Named(NamedKey::ArrowRight)),
            "the arrow belongs to the panel"
        );
        let written = written.lock().expect("the lock");
        assert_eq!(written.len(), 1, "the stepped value is written");
        assert_eq!(written[0].1, SETTING_ROWS[row].key);
    }

    /// Enter on the PERSONALITY action row runs the action through the
    /// app's action runner, not the settings writer (roadmap HELP window
    /// §2, decision F-161).
    #[test]
    fn enter_on_the_personality_row_runs_the_action() {
        let mut app = app_for_tests();
        app.state
            .enter_settings_mode_with(agnosis_core::config::Config::default());
        let run = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let sink = run.clone();
        app.settings_action = Box::new(move |action: &str| {
            sink.lock().expect("the lock").push(action.to_string());
            Ok(())
        });
        let row = SETTING_ROWS
            .iter()
            .position(|r| r.key == "randomise")
            .expect("the row");
        app.state.settings_cursor = row;
        assert!(
            app.settings_panel_key(&Key::Named(NamedKey::Enter)),
            "Enter belongs to the panel"
        );
        assert_eq!(
            *run.lock().expect("the lock"),
            vec!["randomise".to_string()],
            "the action runner saw the row's key"
        );
    }
}
