//! The HELP window — the **application** half.
//!
//! A third winit window (roadmap HELP_WINDOW §1), beside the parade
//! popup and the draw-your-own editor: a borderless, resizable reader
//! that shows the **full text of `README.md`, scrollable**. The text is
//! read **at open time** — the window carries no copy of its own — so a
//! change to the README reaches HELP on the next open with no second
//! document to keep in sync. The shell reads the file
//! ([`crate::host::readme_text`]) and hands the plain text to the core;
//! the **pure** half — the markdown strip, the one-time line wrap, the
//! scroll model and the painting — lives in [`agnosis_core::help`],
//! re-exported below.
//!
//! **Event-driven, not a frame loop.** The parade animates and the
//! editor repaints continuously, but a text page is static: the window
//! polls the GPU init at a slow cadence until the first frame is
//! presented and then sleeps (`ControlFlow::Wait`) until a key, a wheel
//! notch or a resize marks the page dirty and asks for one redraw.
//!
//! **Rendering is TrueType, not the block font** (roadmap §1): README
//! prose is lowercase and the HUD block font carries only `A–Z`,
//! `0–9` and a handful of punctuation. The body renders through the
//! core's embedded DejaVu Sans Mono at the fixed-scale path
//! ([`Renderer::draw_text_layers`]); the footer band keeps the block
//! font (the editor's UI cell), because its labels are uppercase and
//! the footer is chrome.
//!
//! **ESC closes it.** The window is borderless like the editor (the
//! pixels map 1:1 for the layout), but resizable, and the layout
//! follows the new size: a resize re-wraps **once** — never per frame.
//!
//! The Win32 window class is [`crate::platform::HELP_WINDOW_CLASS`]
//! (`"AGnosisHelp"`), never `"AGnosisPopup"` or `"AGnosisEditor"`: the parade
//! hotkey and the editor chord look those classes up to close their own
//! window, and neither lookup may find this one.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowLevel};

#[cfg(target_os = "windows")]
use winit::platform::windows::WindowAttributesExtWindows;

// The GPU-init poll and its outcome are shared with the popup and the
// editor: one definition, three callers (audit 2026-10-21 §G26).
use crate::render::{try_recv_init, Gpu, GpuPoll};

/// The HELP window's **pure** half — the strip, the wrap, the scroll
/// model and the paint helpers.
///
/// The re-export keeps every pure name resolving under `crate::help::*`,
/// the same way the editor re-exports [`agnosis_core::editor`].
pub use agnosis_core::help::*;

/// The HELP window's spawn arguments: the **monitor** geometry.
///
/// The window computes its own rect from the monitor (80 % share,
/// clamped, centred), so the caller only hands over the monitor — the
/// same contract the editor's spawn follows.
#[derive(Debug, Clone, Copy)]
pub struct HelpArgs {
    /// Monitor origin in physical pixels.
    pub origin: (i32, i32),
    /// Monitor size in physical pixels.
    pub size: (u32, u32),
}

/// HELP failures.
#[derive(Debug, thiserror::Error)]
pub enum HelpError {
    /// The event loop could not start.
    #[error("help window: {0}")]
    Window(String),
}

/// How long the GPU-init poll waits between attempts.
///
/// The poll runs only until the first frame is presented; a text page
/// needs no fast cadence, and 100 ms is plenty for an init that normally
/// lands in well under a second.
const GPU_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// The scroll a pressed key asks for, if the key is one.
///
/// Split from [`HelpApp::on_key`] so the routing is testable without an
/// event loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrollAction {
    /// Move by whole lines (the arrows).
    Lines(i32),
    /// Move by pages of the visible height (PgUp/PgDn).
    Page(i32),
    /// Jump to the top of the document (Home).
    Top,
    /// Jump to the last full page (End).
    Bottom,
}

/// The scroll action of one key.
fn scroll_action(key: &Key) -> Option<ScrollAction> {
    let named = match key {
        Key::Named(named) => *named,
        _ => return None,
    };
    match named {
        NamedKey::ArrowUp => Some(ScrollAction::Lines(-1)),
        NamedKey::ArrowDown => Some(ScrollAction::Lines(1)),
        NamedKey::PageUp => Some(ScrollAction::Page(-1)),
        NamedKey::PageDown => Some(ScrollAction::Page(1)),
        NamedKey::Home => Some(ScrollAction::Top),
        NamedKey::End => Some(ScrollAction::Bottom),
        _ => None,
    }
}

/// The HELP window: one winit window, two grids, one renderer.
///
/// The body grid and the footer overlay grid each keep their own cell
/// size — the body's 8 × 16 px and the footer's 6 × 10 px — and each
/// grid is rasterized by the font built for the **same** size: a
/// mismatch puts the glyphs off-frame and the window stays black
/// (AGENTS.md).
pub struct HelpApp {
    args: HelpArgs,
    /// The README text, read at open time by the caller.
    text: String,
    /// The scroll model over the laid-out lines.
    state: HelpState,
    window: Option<Arc<Window>>,
    /// The body layer: the visible page, in body cells.
    grid: Option<agnosis_core::grid::Grid>,
    /// The overlay layer: the footer band, in UI cells.
    overlay_grid: Option<agnosis_core::grid::Grid>,
    /// The rasterizer for the body grid's cell size.
    font: Option<agnosis_core::font::Font>,
    /// The rasterizer for the footer grid's cell size.
    overlay_font: Option<agnosis_core::font::Font>,
    /// The window's inner size in pixels.
    inner: (u32, u32),
    /// Grid geometry `(body_cols, body_rows, footer_cols, footer_rows)`
    /// the current grids were built for.
    geometry: (usize, usize, usize, usize),
    /// The GPU life-cycle ([`Gpu`]): the same machine the popup runs
    /// (audit 101126 §S3).
    gpu: Gpu,
    /// True when something changed and the page needs one redraw.
    dirty: bool,
    /// True after the first successful draw: the window then sleeps
    /// between events instead of polling.
    first_frame: bool,
    exit: bool,
}

impl HelpApp {
    /// Poll the GPU init thread; a failed or vanished init degrades to
    /// the honest dead-GPU title instead of panicking (the popup's and
    /// the editor's shape, audit 2026-10-23 §F-092).
    fn poll_gpu(&mut self) {
        let Gpu::Awaiting(receiver) = &self.gpu else {
            return;
        };
        let init = match try_recv_init(Some(receiver)) {
            GpuPoll::Ready(init) => init,
            GpuPoll::Pending => return,
            GpuPoll::Gone => {
                self.degrade();
                return;
            }
        };
        // The channel is spent: the machine moves on to Ready or Dead
        // and never awaits the same init twice (audit 101126 §S3). A
        // failure below leaves it in `Dead` — the footer then paints the
        // dead-GPU word instead of the hints.
        self.gpu = Gpu::Dead;
        self.mark_dirty();
        agnosis_core::trace::trace("help.device-ready");
        let window = match self.window.as_ref() {
            Some(window) => Arc::clone(window),
            None => return,
        };
        let surface = match init.instance.create_surface(Arc::clone(&window)) {
            Ok(surface) => surface,
            Err(e) => {
                agnosis_core::trace::trace(&format!("help.surface error, degraded: {e}"));
                return;
            }
        };
        let inner = window.inner_size();
        let (width, height) = (inner.width.max(1), inner.height.max(1));
        let config =
            match crate::render::pick_surface_config(&surface, &init.adapter, width, height) {
                Some(config) => config,
                None => {
                    agnosis_core::trace::trace("help.surface no-compatible-format, degraded");
                    return;
                }
            };
        match crate::render::Renderer::new(init.device, init.queue, surface, config, width, height)
        {
            Ok(renderer) => {
                agnosis_core::trace::trace("help.renderer-ready");
                self.gpu = Gpu::Ready(Box::new(renderer));
            }
            Err(e) => {
                agnosis_core::trace::trace(&format!("help.renderer error, degraded: {e}"));
            }
        }
        self.mark_dirty();
    }

    /// The honest dead-GPU state: the title says it at the OS level.
    /// With the renderer gone there is nothing left to paint the
    /// footer's word with, so the OS title — task bar, alt-tab — is the
    /// only surface that can carry it (audit 2026-10-23 §F-092). `Dead`
    /// is terminal: the announcement fires exactly once (audit 101126
    /// §S3).
    fn degrade(&mut self) {
        agnosis_core::trace::trace("help.gpu init unavailable, degraded");
        self.gpu = Gpu::Dead;
        if let Some(window) = self.window.as_ref() {
            window.set_title("AGnosis - GPU UNAVAILABLE");
        }
        self.mark_dirty();
    }

    /// Draw one frame: paint the page and the footer, hand them to the
    /// renderer. A missing piece is a no-op.
    fn draw_frame(&mut self) {
        // The footer's choice is read **before** the renderer borrows
        // `gpu`; with the machine the two states cannot coexist anyway —
        // a `Ready` window is never `Dead` (audit 101126 §S3).
        let dead = matches!(self.gpu, Gpu::Dead);
        let Gpu::Ready(renderer) = &mut self.gpu else {
            return;
        };
        let Some(font) = self.font.as_mut() else {
            return;
        };
        let Some(overlay_font) = self.overlay_font.as_mut() else {
            return;
        };
        let Some(grid) = self.grid.as_mut() else {
            return;
        };
        let Some(overlay) = self.overlay_grid.as_mut() else {
            return;
        };
        let visible = grid.rows();
        paint_body(grid, &self.state, visible);
        if dead {
            paint_footer_gpu_dead(overlay);
        } else {
            paint_footer(overlay, &self.state);
        }
        match renderer.draw_text_layers(grid, font, Some((overlay, overlay_font))) {
            Ok(()) => self.mark_first_frame(),
            Err(e) => agnosis_core::trace::trace(&format!("help.draw error: {e}")),
        }
    }

    /// First successful draw: trace once for the startup smoke log.
    fn mark_first_frame(&mut self) {
        if self.first_frame {
            return;
        }
        self.first_frame = true;
        agnosis_core::trace::trace("help.first-frame");
    }

    /// Mark the page dirty and ask for exactly one redraw.
    fn mark_dirty(&mut self) {
        self.dirty = true;
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// One tick: poll the GPU, draw when dirty, schedule the next wake.
    ///
    /// Before the first presented frame the poll runs at
    /// [`GPU_POLL_INTERVAL`]; afterwards the loop sleeps until an event
    /// (a text page is static — there is nothing to animate).
    fn tick(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit || self.window.is_none() {
            return;
        }
        self.poll_gpu();
        if self.dirty {
            self.dirty = false;
            self.draw_frame();
        }
        self.schedule(event_loop);
    }

    /// Schedule the next wake: poll again while the first frame is
    /// outstanding, sleep once it is in.
    fn schedule(&mut self, event_loop: &ActiveEventLoop) {
        if self.first_frame {
            event_loop.set_control_flow(ControlFlow::Wait);
            return;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(Instant::now() + GPU_POLL_INTERVAL));
    }

    /// One pressed key: ESC closes the window, the scroll keys move the
    /// page. Every other key is ignored — a reader has no input line.
    fn on_key(&mut self, event_loop: &ActiveEventLoop, event: winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            self.quit(event_loop);
            return;
        }
        let Some(action) = scroll_action(&event.logical_key) else {
            return;
        };
        self.apply_scroll(action);
    }

    /// Carry out one scroll action over the page's visible height.
    fn apply_scroll(&mut self, action: ScrollAction) {
        let visible = body_rows_for(self.inner.1);
        match action {
            ScrollAction::Lines(delta) => self.state.scroll_by(delta, visible),
            ScrollAction::Page(pages) => self.state.scroll_page(pages, visible),
            ScrollAction::Top => self.state.scroll_to_top(),
            ScrollAction::Bottom => self.state.scroll_to_bottom(visible),
        }
        self.mark_dirty();
    }

    /// One wheel notch: scroll the page by lines.
    fn on_wheel(&mut self, delta: MouseScrollDelta) {
        let lines = wheel_lines(delta);
        if lines == 0 {
            return;
        }
        self.apply_scroll(ScrollAction::Lines(lines));
    }

    /// ESC / window close.
    ///
    /// A reader holds no pending work — the text was read at open time
    /// and nothing here writes — so the exit is just the exit.
    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        self.exit = true;
        event_loop.exit();
    }

    /// Resize handler: the renderer follows the window, the grids are
    /// rebuilt and the text re-wraps **once** for the new width (roadmap
    /// §1: line-wrap at open/resize, never per frame).
    ///
    /// The **fonts** are NOT rebuilt: they rasterize the *constant* cell
    /// sizes, so a resize costs no re-rasterization (the editor's audit
    /// 2026-10-23 §F-112 note).
    fn on_resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        self.inner = (width, height);
        if let Gpu::Ready(renderer) = &mut self.gpu {
            renderer.resize(width, height);
        }
        self.rebuild_grids(width, height);
        let cols = body_cols_for(width);
        self.state.relayout(&self.text, cols, body_rows_for(height));
        self.mark_dirty();
    }

    /// Rebuild the two grids for a new window size.
    ///
    /// The body grid holds everything above the footer band; the footer
    /// grid is the full window (its text sits on the bottom rows).
    fn rebuild_grids(&mut self, width: u32, height: u32) {
        let body_cols = body_cols_for(width);
        let body_rows = body_rows_for(height);
        let footer_cols = (width / FOOTER_CELL_W).max(1) as usize;
        let footer_rows = (height / FOOTER_CELL_H).max(1) as usize;
        if (body_cols, body_rows, footer_cols, footer_rows) == self.geometry {
            return;
        }
        self.geometry = (body_cols, body_rows, footer_cols, footer_rows);
        self.grid = Some(agnosis_core::grid::Grid::new(
            body_cols,
            body_rows,
            HELP_CELL_W,
            HELP_CELL_H,
        ));
        self.overlay_grid = Some(agnosis_core::grid::Grid::new(
            footer_cols,
            footer_rows,
            FOOTER_CELL_W,
            FOOTER_CELL_H,
        ));
    }

    /// Build the two fonts, one per layer.
    ///
    /// Both cell sizes are **constants**, so this runs **once**, when the
    /// window is built — the fonts can never drift from the grids, and a
    /// resize costs no re-rasterization (audit 2026-10-23 §F-112).
    fn rebuild_fonts(&mut self) {
        match agnosis_core::font::Font::new(HELP_CELL_W, HELP_CELL_H) {
            Ok(font) => self.font = Some(font),
            Err(e) => agnosis_core::trace::trace(&format!("help.body-font error: {e}")),
        }
        match agnosis_core::font::Font::new(FOOTER_CELL_W, FOOTER_CELL_H) {
            Ok(font) => self.overlay_font = Some(font),
            Err(e) => agnosis_core::trace::trace(&format!("help.footer-font error: {e}")),
        }
    }
}

/// The wheel delta in lines: positive scrolls **down** (the page moves
/// towards the document's end).
fn wheel_lines(delta: MouseScrollDelta) -> i32 {
    match delta {
        MouseScrollDelta::LineDelta(_, dy) => wheel_to_lines(f64::from(dy), 1.0),
        MouseScrollDelta::PixelDelta(pos) => wheel_to_lines(pos.y, f64::from(HELP_CELL_H)),
    }
}

/// `-value / scale` as whole lines, saturating at `i32`'s edges.
///
/// The negation runs in **float** space: a float→int cast saturates on
/// its own, but negating the already-saturated `i32::MIN` afterwards
/// would overflow (the core's scroll math handles the same edge with
/// `try_from().unwrap_or`).
fn wheel_to_lines(value: f64, scale: f64) -> i32 {
    let lines = -value / scale;
    if lines.is_nan() {
        return 0;
    }
    lines
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}

impl ApplicationHandler for HelpApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let (width, height) = help_window_rect(self.args.size.0, self.args.size.1);
        let origin = help_window_origin(self.args.origin, self.args.size.0, self.args.size.1);
        let attrs = Window::default_attributes()
            // The title is the OS-level escape route (audit 2026-10-23
            // §F-103): the window is borderless, but the task bar, the
            // alt-tab list and the OS all show the name. The class —
            // what the daemon would look the window up by — is its own.
            .with_title(crate::platform::HELP_WINDOW_TITLE)
            .with_inner_size(PhysicalSize::new(width, height))
            .with_min_inner_size(PhysicalSize::new(MIN_HELP_W, MIN_HELP_H))
            .with_position(PhysicalPosition::new(origin.0, origin.1))
            .with_decorations(false)
            .with_resizable(true)
            .with_transparent(false)
            .with_visible(false)
            .with_active(true)
            .with_window_level(WindowLevel::Normal);
        #[cfg(target_os = "windows")]
        let attrs = attrs.with_class_name(crate::platform::HELP_WINDOW_CLASS);
        let window = match event_loop.create_window(attrs) {
            Ok(window) => Arc::new(window),
            Err(e) => {
                agnosis_core::trace::trace(&format!("help.window error: {e}"));
                self.exit = true;
                event_loop.exit();
                return;
            }
        };
        agnosis_core::trace::trace("help.window-created");
        // Black background before the first show: no white flash.
        if let Err(e) = crate::platform::window::paint_black_before_show(&window) {
            agnosis_core::trace::trace(&format!("help.paint-black error: {e}"));
        }
        window.set_visible(true);
        window.set_decorations(false);
        window.set_resizable(true);
        // Borderless, because the layout maps window pixels 1:1 — but
        // *resizable*, so the user can drag the invisible border and the
        // page re-wraps (the editor's flavour, see
        // `platform::window::ensure_borderless_resizable`).
        crate::platform::window::ensure_borderless_resizable(&window);
        // The window owns the keyboard (ESC closes it), so it takes the
        // foreground the same way the popup and the editor do. Not
        // topmost: it is an ordinary window.
        crate::platform::window::focus_ladder(&window, false);
        let inner = window.inner_size();
        self.inner = (inner.width, inner.height);
        self.rebuild_grids(inner.width, inner.height);
        self.rebuild_fonts();
        let cols = body_cols_for(inner.width);
        self.state = HelpState::new(&self.text, cols);
        self.dirty = true;
        self.window = Some(window);
        self.schedule(event_loop);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => self.quit(event_loop),
            WindowEvent::KeyboardInput { event, .. } => self.on_key(event_loop, event),
            WindowEvent::MouseWheel { delta, .. } => self.on_wheel(delta),
            WindowEvent::RedrawRequested => self.tick(event_loop),
            WindowEvent::Resized(size) => self.on_resize(size.width, size.height),
            _ => {}
        }
    }

    /// Poll one tick while the window exists.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit {
            event_loop.exit();
            return;
        }
        self.tick(event_loop);
    }
}

/// Run the HELP window until ESC / window close. Returns Ok on clean exit.
///
/// The window is **single-instance** (audit 2026-10-25): the process
/// claims the help slot before the window exists and releases it on the
/// way out — a claimed slot means another reader is live, so this one
/// exits quietly instead of stacking a second page over the first. The
/// parade's F1 probe asks [`crate::platform::window::help_is_open`]
/// before it even spawns; the claim here is the belt to that probe's
/// braces, covering a race between two spawners.
///
/// The GPU init runs on its own thread and the event loop only polls the
/// result — the same single-flight pattern the popup and the editor use.
pub fn run(text: &str, args: &HelpArgs) -> Result<(), HelpError> {
    agnosis_core::trace::trace(&format!("help.spawn readme_bytes={}", text.len()));
    if let Err(e) = crate::platform::window::write_help_pid_file() {
        agnosis_core::trace::trace(&format!("help.already-open, not reopened: {e}"));
        return Ok(());
    }
    let result = run_window(text, args);
    crate::platform::window::clear_help_pid_file();
    result
}

/// The event loop itself: one window, one reader, until ESC / close.
fn run_window(text: &str, args: &HelpArgs) -> Result<(), HelpError> {
    let event_loop = EventLoop::new().map_err(|e| HelpError::Window(format!("event loop: {e}")))?;
    let gpu_rx = crate::render::spawn_device_thread();
    let state = HelpState::new(
        text,
        body_cols_for(help_window_rect(args.size.0, args.size.1).0),
    );
    let mut app = HelpApp {
        args: *args,
        text: text.to_string(),
        state,
        window: None,
        grid: None,
        overlay_grid: None,
        font: None,
        overlay_font: None,
        inner: (0, 0),
        geometry: (0, 0, 0, 0),
        gpu: Gpu::Awaiting(gpu_rx),
        dirty: false,
        first_frame: false,
        exit: false,
    };
    event_loop
        .run_app(&mut app)
        .map_err(|e| HelpError::Window(format!("event loop: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A README page for the tests.
    fn sample_text() -> String {
        "# TITLE\n\nfirst paragraph\n\n```toml\nkey = 1\n```\n".to_string()
    }

    /// A page long enough to scroll and long enough per line that a
    /// narrower page wraps it into more lines.
    fn long_text() -> String {
        (0..200)
            .map(|i| format!("line {i} {}", "the quick brown fox ".repeat(14)))
            .collect()
    }

    /// A help app without a window, for the grid/state tests.
    fn app_for_tests() -> HelpApp {
        let text = sample_text();
        HelpApp {
            args: HelpArgs {
                origin: (0, 0),
                size: (1920, 1080),
            },
            state: HelpState::new(&text, body_cols_for(1536)),
            text,
            window: None,
            grid: None,
            overlay_grid: None,
            font: None,
            overlay_font: None,
            inner: (1536, 864),
            geometry: (0, 0, 0, 0),
            gpu: Gpu::Dead,
            dirty: false,
            first_frame: false,
            exit: false,
        }
    }

    #[test]
    fn the_grids_and_the_fonts_share_their_cell_size() {
        // A mismatched font size puts the glyphs off-frame and the window
        // stays black (AGENTS.md): each grid and its font use one cell size.
        let mut app = app_for_tests();
        app.rebuild_grids(1536, 864);
        app.rebuild_fonts();
        let grid = app.grid.as_ref().expect("body grid");
        assert_eq!(grid.cell_w(), HELP_CELL_W);
        assert_eq!(grid.cell_h(), HELP_CELL_H);
        assert_eq!(grid.cols(), body_cols_for(1536));
        assert_eq!(grid.rows(), body_rows_for(864));
        let overlay = app.overlay_grid.as_ref().expect("footer grid");
        assert_eq!(overlay.cell_w(), FOOTER_CELL_W);
        assert_eq!(overlay.cell_h(), FOOTER_CELL_H);
        let font = app.font.as_ref().expect("body font");
        assert_eq!(font.cell_w(), HELP_CELL_W);
        assert_eq!(font.cell_h(), HELP_CELL_H);
        let overlay_font = app.overlay_font.as_ref().expect("footer font");
        assert_eq!(overlay_font.cell_w(), FOOTER_CELL_W);
        assert_eq!(overlay_font.cell_h(), FOOTER_CELL_H);
    }

    #[test]
    fn a_resize_rewraps_and_rebuilds_both_layers() {
        let mut app = app_for_tests();
        app.text = long_text();
        app.state = HelpState::new(&app.text, body_cols_for(1536));
        app.rebuild_grids(1536, 864);
        let before = app.state.line_count();
        app.on_resize(1024, 768);
        let grid = app.grid.as_ref().expect("body grid");
        assert_eq!(grid.cols(), body_cols_for(1024));
        assert_eq!(grid.rows(), body_rows_for(768));
        let after = app.state.line_count();
        assert!(
            after > before,
            "a narrower page wraps into more lines ({before} -> {after})"
        );
        assert!(app.dirty, "the resize asks for one redraw");
    }

    #[test]
    fn the_scroll_keys_reach_the_state() {
        let text: String = (0..200)
            .map(|i| format!("line {i} the quick brown fox\n"))
            .collect();
        let mut app = app_for_tests();
        app.text = text.clone();
        app.state = HelpState::new(&text, 80);
        let top = app.state.scroll();
        app.apply_scroll(ScrollAction::Page(1));
        assert!(app.state.scroll() > top, "PageDown moves the page");
        assert!(app.dirty);
        app.apply_scroll(ScrollAction::Top);
        assert_eq!(app.state.scroll(), top);
        app.apply_scroll(ScrollAction::Bottom);
        let visible = body_rows_for(app.inner.1);
        assert_eq!(app.state.scroll(), app.state.max_scroll(visible));
    }

    #[test]
    fn the_wheel_scrolls_by_lines() {
        let mut app = app_for_tests();
        app.text = long_text();
        app.state = HelpState::new(&app.text, body_cols_for(1536));
        let top = app.state.scroll();
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, -2.0));
        assert_eq!(app.state.scroll(), top + 2, "a wheel-down scrolls down");
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, 2.0));
        assert_eq!(app.state.scroll(), top);
        app.on_wheel(MouseScrollDelta::LineDelta(0.0, 5.0));
        assert_eq!(app.state.scroll(), top, "clamped at the top");
    }

    #[test]
    fn the_scroll_action_table_names_its_keys() {
        assert_eq!(
            scroll_action(&Key::Named(NamedKey::PageUp)),
            Some(ScrollAction::Page(-1))
        );
        assert_eq!(
            scroll_action(&Key::Named(NamedKey::PageDown)),
            Some(ScrollAction::Page(1))
        );
        assert_eq!(
            scroll_action(&Key::Named(NamedKey::ArrowUp)),
            Some(ScrollAction::Lines(-1))
        );
        assert_eq!(
            scroll_action(&Key::Named(NamedKey::ArrowDown)),
            Some(ScrollAction::Lines(1))
        );
        assert_eq!(
            scroll_action(&Key::Named(NamedKey::Home)),
            Some(ScrollAction::Top)
        );
        assert_eq!(
            scroll_action(&Key::Named(NamedKey::End)),
            Some(ScrollAction::Bottom)
        );
        assert_eq!(scroll_action(&Key::Named(NamedKey::Escape)), None);
        assert_eq!(scroll_action(&Key::Character("a".into())), None);
    }

    #[test]
    fn the_page_paint_shows_the_top_of_the_sample() {
        let mut app = app_for_tests();
        app.rebuild_grids(1536, 864);
        let grid = app.grid.as_mut().expect("body grid");
        paint_body(grid, &app.state, grid.rows());
        let first: String = (0..10).filter_map(|col| grid.char_at(col, 0)).collect();
        assert_eq!(first.trim_end(), "TITLE", "the heading leads the page");
    }

    #[test]
    fn a_dead_gpu_swaps_the_footer_for_the_word() {
        let mut app = app_for_tests();
        app.gpu = Gpu::Dead;
        app.rebuild_grids(1536, 864);
        app.rebuild_fonts();
        // The footer paints the dead-GPU text through the same path the
        // draw uses; the page itself is untouched.
        let overlay = app.overlay_grid.as_mut().expect("footer grid");
        paint_footer_gpu_dead(overlay);
        let text_row = overlay.rows() as i64 - FOOTER_TEXT_FROM_BOTTOM;
        let ink = (0..overlay.cols())
            .filter(|col| overlay.char_at(*col, text_row as usize) == Some('X'))
            .count();
        assert!(ink > 0, "the dead-GPU word is stamped");
    }
}
