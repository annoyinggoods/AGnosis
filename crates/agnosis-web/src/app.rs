//! The web front end's loop: the canvas, the clock and the renderer.
//!
//! WHY this file owns the clock and not the core: the core owns the
//! **simulation** ([`agnosis_core::scene::Scene`]), the web owns the
//! **presentation**. The core is the same code the native app runs, so
//! the parade's speed, its art and its physics cannot drift between the
//! two front ends (roadmap Extension LATER §6).
//!
//! The one rule that matters here: **the web simulates nothing**. It
//! advances the scene by a delta and blits what the scene produced.
//!
//! WHAT the browser replaces (roadmap Extension LATER §1.3):
//!
//! | Native | Web |
//! | --- | --- |
//! | the window's size | the canvas' size, in device pixels |
//! | the monitor probe | the canvas' own box |
//! | the config folder | the built-in config, plus what a manifest brings |
//! | the drawn-asset folders | an in-memory store ([`crate::assets_io`]) |
//! | the sound device | **nothing**: the scene's audio events are drained
//!   and dropped every frame, so the parade is silent — on purpose (there
//!   is no device to play on, and an undrained queue would grow without
//!   bound; audit 2026-10-23 F-138) |
//! | the stats file's "days" counter | **nothing**: a browser tab has no
//!   stats file to count from, so `set_counter_line` stays unset and the
//!   corner stays empty (audit 2026-10-23 F-155) |

use std::cell::RefCell;
use std::rc::Rc;

use agnosis_core::blit::{blit_scene, FrameBuffer};
use agnosis_core::config::Config;
use agnosis_core::drawn::DrawnKind;
use agnosis_core::font::Font;
use agnosis_core::scene::Scene;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::HtmlCanvasElement;

use crate::assets_io::AssetStore;
use crate::render::{Backend, Renderer};

/// The longest frame delta the simulation is given.
///
/// WHY: a backgrounded tab comes back with a delta of seconds, and one
/// `advance(3.0)` would teleport the parade across the screen (the
/// fixed 1/120 s step would run 360 sub-steps in a single frame). The
/// clamp is a tenth of a second: long enough for an ordinary frame,
/// short enough that a stall cannot jump the train.
pub const MAX_DT_SECONDS: f32 = 0.1;

/// The hint the HUD shows.
///
/// The native hint is "press ESC to exit"; a browser tab has no ESC to
/// exit (there is nothing to exit), so the web says what the page can
/// actually do.
pub const WEB_EXIT_HINT: &str = "CLICK A DUCK - SHIFT-CLICK STOPS IT";

thread_local! {
    /// The one live app of the page.
    static APP: RefCell<Option<Rc<RefCell<AppState>>>> = const { RefCell::new(None) };

    /// The back end the page asked for **before the app was up**.
    ///
    /// WHY it is remembered here: the wasm boots asynchronously, so a
    /// `set_backend()` call can arrive while the app does not exist yet.
    /// The request was simply lost then, and the page showed the old
    /// back end with no sign of the switch (audit 2026-10-21 §W1).
    static PENDING_BACKEND: RefCell<Option<Backend>> = const { RefCell::new(None) };

    /// The closures the browser must keep alive.
    ///
    /// A dropped [`Closure`] invalidates its JavaScript callback: the
    /// animation-frame loop would stop after one frame and the resize
    /// listener would go silent. So they live here, for the life of the
    /// page.
    static KEEPALIVE: RefCell<Vec<Box<dyn std::any::Any>>> = const { RefCell::new(Vec::new()) };
}

/// The animation-frame callback.
type FrameClosure = Closure<dyn FnMut(f64)>;

/// The loop's handle on its own callback.
///
/// WHY a `JsValue` and not the [`Closure`]: the closure is **running**
/// while it schedules the next frame, and the wasm-bindgen trampoline
/// holds it as `&mut` — a shared borrow of the very closure that is
/// running is an aliasing violation (audit 2026-10-21 §W3). The handle
/// therefore carries the **JavaScript function**, and the closure lives
/// in the keep-alive list.
type FrameHandle = Rc<RefCell<Option<wasm_bindgen::JsValue>>>;

/// Keep `value` alive for the life of the page.
pub fn keep_alive<T: std::any::Any + 'static>(value: T) {
    KEEPALIVE.with(|slot| slot.borrow_mut().push(Box::new(value)));
}

/// The simulation and the buffer it is drawn into.
pub struct Sim {
    /// The configuration the scene runs on.
    pub cfg: Config,
    /// The scene: the simulation and the composed grid.
    pub scene: Scene,
    /// The glyph rasterizer, at the grid's cell pitch.
    pub font: Font,
    /// The frame buffer the blit writes into.
    pub fb: FrameBuffer,
    /// The grid's columns.
    pub cols: usize,
    /// The grid's rows.
    pub rows: usize,
    /// The cell width, in pixels.
    pub cell_w: u32,
    /// The cell height, in pixels.
    pub cell_h: u32,
    /// The drawn assets the page carries.
    ///
    /// A browser has no asset folder, so the art lives in memory and the
    /// scene's injectable loader asks this store for it by name. The
    /// store is **shared** (`Rc<RefCell>`) with the draw-your-own editor
    /// (roadmap WASM §6): a sprite the editor saves is in the parade's
    /// store the moment it lands, and a package the page imports is in
    /// the editor's assign panel without a restart — one store, one
    /// truth, no sync pass between the two.
    pub assets: Rc<RefCell<AssetStore>>,
}

/// The backend-swap machine (audit 101126 §W1): what the page asked for
/// while it is not live yet. The transitions are reduced by
/// [`reduce_swap`], which the host tests pin — the futures themselves
/// only run in the browser.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Swap {
    /// Nothing is in flight.
    Idle,
    /// A swap to that backend is requested and running.
    InFlight(Backend),
}

/// One event of the swap machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwapEvent {
    /// The page asked for that backend.
    Requested(Backend),
    /// The running swap built its renderer and went live.
    Committed,
    /// The running swap failed; the automatic choice took over (or
    /// failed too).
    FellBack,
}

/// Reduce the swap machine (audit 101126 §W1): a request while one is
/// in flight **replaces** the target, and exactly one terminal event
/// per swap returns to Idle. A terminal event belonging to a replaced
/// swap is a late arrival — [`settle_swap`]'s generation check keeps it
/// away from this function.
fn reduce_swap(swap: Swap, event: SwapEvent) -> Swap {
    match (swap, event) {
        (_, SwapEvent::Requested(backend)) => Swap::InFlight(backend),
        (Swap::InFlight(_), SwapEvent::Committed) => Swap::Idle,
        (Swap::InFlight(_), SwapEvent::FellBack) => Swap::Idle,
        (Swap::Idle, _) => Swap::Idle,
    }
}

/// Apply a terminal swap event if the swap that produced it is still
/// the live one (`swap_generation == generation`): a replaced request's
/// late arrival is ignored. Pure, so the host tests pin the whole
/// machine even though the futures only run in the browser (audit
/// 101126 §W1).
fn settle_swap(swap: Swap, swap_generation: u64, generation: u64, event: SwapEvent) -> Swap {
    if swap_generation != generation {
        return swap;
    }
    reduce_swap(swap, event)
}

/// The whole live app: the simulation, the renderer and the page's state.
pub struct AppState {
    /// The simulation.
    pub sim: Sim,
    /// The drawing back end. `None` until the first one is built.
    pub renderer: Option<Box<dyn Renderer>>,
    /// The canvas the page shows.
    pub canvas: HtmlCanvasElement,
    /// Is the parade paused?
    pub paused: bool,
    /// The timestamp of the last frame, in milliseconds.
    pub last_ts_ms: Option<f64>,
    /// The time not yet spent on a frame.
    ///
    /// The frame rate is capped (the config's `fps_cap`, the same number
    /// the native app honours), so the deltas are **accumulated** and
    /// spent in whole frames: the simulation's step is the cap's
    /// interval, not the display's.
    pub accumulator: f32,
    /// The backend-swap machine: what was asked for and is not live yet
    /// ([`Swap`]).
    pub swap: Swap,
    /// Bumped on every accepted request, so a replaced swap's late
    /// commit cannot clear the newer one (audit 101126 §W1).
    pub swap_generation: u64,
    /// A note for the status line (a back end that was not available, a
    /// repair the config made).
    pub note: Option<String>,
}

/// The live app, if it is up.
pub fn the_app() -> Option<Rc<RefCell<AppState>>> {
    APP.with(|cell| cell.borrow().clone())
}

/// Run `f` on the live app, if it is up.
pub fn with_app<T>(f: impl FnOnce(&mut AppState) -> T) -> Option<T> {
    let app = the_app()?;
    let mut state = app.borrow_mut();
    Some(f(&mut state))
}

/// Build the app and start the loop.
pub async fn boot() -> Result<(), String> {
    // The core's clock is a parameter (see `agnosis_core::trace`): the
    // standard library's clock **panics** on `wasm32-unknown-unknown`,
    // and the core traces while it builds the scene. So the page hands
    // the core its own clock BEFORE anything else.
    //
    // WHY the return value: the core now **reports** a refused clock,
    // and the page is the only host that can have one.
    if !agnosis_core::trace::set_host_clock(browser_millis) {
        crate::render::warn("the page's clock was refused: another clock was set first");
    }
    let canvas = the_canvas().await?;
    let cfg = boot_config();
    let (renderer, canvas) = build_the_first_renderer(&canvas).await?;
    let state = new_state(&cfg, &canvas, renderer)?;
    let app = Rc::new(RefCell::new(state));
    APP.with(|cell| *cell.borrow_mut() = Some(Rc::clone(&app)));
    install_the_listeners(&app)?;
    start_the_loop(Rc::clone(&app))?;
    apply_the_pending_backend();
    Ok(())
}

/// Build the first renderer, on a canvas that is still free.
///
/// WHY the retry: in the `Auto` chain the WebGL2 back end has to take
/// the `webgl2` context **before** it can ask for an adapter, so a
/// browser whose adapter request fails leaves the canvas **spent** — and
/// the chain's error says so. Without the retry the page's `#canvas`
/// stayed permanently unusable: no parade at all (audit 2026-10-21 §W2).
async fn build_the_first_renderer(
    canvas: &HtmlCanvasElement,
) -> Result<(Box<dyn Renderer>, HtmlCanvasElement), String> {
    match crate::render::create(canvas, Backend::Auto).await {
        Ok(renderer) => Ok((renderer, canvas.clone())),
        Err(e) if is_a_spent_canvas(&e) => {
            let fresh = a_fresh_canvas(canvas)?;
            let renderer = crate::render::create(&fresh, Backend::Auto).await?;
            put_the_canvas_in_the_page(canvas, &fresh)?;
            Ok((renderer, fresh))
        }
        Err(e) => Err(e),
    }
}

/// True when the error says the canvas already carries a context.
fn is_a_spent_canvas(error: &str) -> bool {
    error.contains("already carries a context")
}

/// Apply the back end the page asked for before the app was up.
///
/// WHY: the wasm boots asynchronously, so a `set_backend()` call can
/// arrive before the app exists. The request is remembered and applied
/// now (audit 2026-10-21 §W1).
fn apply_the_pending_backend() {
    let pending = PENDING_BACKEND.with(|cell| cell.borrow_mut().take());
    let Some(backend) = pending else {
        return;
    };
    request_backend(backend);
}

/// The page's clock: the milliseconds since the page started.
///
/// The browser's `performance.now()` is the monotonic clock of the web
/// platform, and it is the only clock a tab has.
fn browser_millis() -> f64 {
    web_sys::window()
        .and_then(|w| w.performance())
        .map(|performance| performance.now())
        .unwrap_or(0.0)
}

/// The configuration the page starts with.
///
/// The built-in config, with the web's own exit hint: the native one
/// says "press ESC to exit", and a tab has nothing to exit.
fn boot_config() -> Config {
    Config {
        exit_hint: WEB_EXIT_HINT.to_string(),
        ..Config::default()
    }
}

/// The state of a fresh app.
fn new_state(
    cfg: &Config,
    canvas: &HtmlCanvasElement,
    renderer: Box<dyn Renderer>,
) -> Result<AppState, String> {
    let (cols, rows, cell_w, cell_h) = geometry(cfg, canvas);
    let font = Font::new(cell_w, cell_h).map_err(|e| format!("cannot open the font: {e}"))?;
    let assets = Rc::new(RefCell::new(AssetStore::new()));
    let scene = build_scene(cfg, &assets, cols, rows, cell_w, cell_h);
    let fb = FrameBuffer::new(cols as u32 * cell_w, rows as u32 * cell_h);
    // The canvas' drawing buffer IS the frame: the page scales the
    // canvas element with CSS, so the buffer holds exactly the grid's
    // pixels and nothing is stretched by the blit.
    canvas.set_width(fb.width);
    canvas.set_height(fb.height);
    Ok(AppState {
        sim: Sim {
            cfg: cfg.clone(),
            scene,
            font,
            fb,
            cols,
            rows,
            cell_w,
            cell_h,
            assets,
        },
        renderer: Some(renderer),
        canvas: canvas.clone(),
        paused: false,
        last_ts_ms: None,
        accumulator: 0.0,
        swap: Swap::Idle,
        swap_generation: 0,
        note: None,
    })
}

/// The grid for the canvas, in device pixels.
///
/// The row-budget grid is the core's own ([`Config::grid_for_monitor`]),
/// so the web's cell pitch is the native's cell pitch for a screen of
/// the same size — the duck is the same fraction of the screen height
/// on every monitor and every DPI.
fn geometry(cfg: &Config, canvas: &HtmlCanvasElement) -> (usize, usize, u32, u32) {
    let (css_w, css_h) = canvas_size(canvas);
    let dpr = device_pixel_ratio();
    let buffer_w = (css_w as f64 * dpr).round().max(1.0) as u32;
    let buffer_h = (css_h as f64 * dpr).round().max(1.0) as u32;
    cfg.grid_for_monitor(buffer_w, buffer_h, 1.0)
}

/// The canvas' CSS size, in pixels.
fn canvas_size(canvas: &HtmlCanvasElement) -> (u32, u32) {
    let width = canvas.client_width();
    let height = canvas.client_height();
    let fallback = document_size();
    (
        to_pixels(width.max(fallback.0)),
        to_pixels(height.max(fallback.1)),
    )
}

/// The document's size, in CSS pixels: the fallback for a canvas that
/// has no layout yet (the page's own box).
fn document_size() -> (i32, i32) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else {
        return (0, 0);
    };
    let Some(root) = document.document_element() else {
        return (0, 0);
    };
    (root.client_width(), root.client_height())
}

/// A CSS size as a pixel count: never zero, never negative.
fn to_pixels(value: i32) -> u32 {
    u32::try_from(value.max(1)).unwrap_or(1)
}

/// The device pixel ratio: the CSS pixels the page uses times this is
/// the physical pixels the canvas draws into.
pub(crate) fn device_pixel_ratio() -> f64 {
    web_sys::window()
        .map(|w| w.device_pixel_ratio())
        .unwrap_or(1.0)
        .max(1.0)
}

/// The scene over the in-memory assets.
///
/// The **injectable** constructor is the one the web uses: the core
/// never reaches for a folder, the page's own store answers. The store
/// is shared with the editor, so the loader borrows it **per call** —
/// a short-lived [`RefCell`] borrow inside the closure, never a borrow
/// held across the scene build (audit 2026-10-21 §E8's shape).
pub fn build_scene(
    cfg: &Config,
    assets: &Rc<RefCell<AssetStore>>,
    cols: usize,
    rows: usize,
    cell_w: u32,
    cell_h: u32,
) -> Scene {
    let loader_store = Rc::clone(assets);
    let loader = move |kind: DrawnKind, name: &str| loader_store.borrow().load(kind, name);
    Scene::build(cfg, cols, rows, cell_w, cell_h, loader, None)
}

/// Install the window's listeners: the resize and the click.
fn install_the_listeners(app: &Rc<RefCell<AppState>>) -> Result<(), String> {
    let window = web_sys::window().ok_or_else(|| "the page has no window".to_string())?;
    let resize_app = Rc::clone(app);
    let resize = Closure::<dyn FnMut(web_sys::Event)>::new(move |_event: web_sys::Event| {
        let mut state = resize_app.borrow_mut();
        state.on_resize();
    });
    window
        .add_event_listener_with_callback("resize", resize.as_ref().unchecked_ref())
        .map_err(|e| format!("cannot listen for resizes: {e:?}"))?;
    let canvas = app.borrow().canvas.clone();
    crate::input::install(app, &canvas)?;
    keep_alive(resize);
    Ok(())
}

/// Start the animation-frame loop.
fn start_the_loop(app: Rc<RefCell<AppState>>) -> Result<(), String> {
    let window = web_sys::window().ok_or_else(|| "the page has no window".to_string())?;
    let handle: FrameHandle = Rc::new(RefCell::new(None));
    let handle_in_loop = Rc::clone(&handle);
    let app_in_loop = Rc::clone(&app);
    let window_in_loop = window.clone();
    let closure: FrameClosure = Closure::<dyn FnMut(f64)>::new(move |ts_ms: f64| {
        {
            let mut state = app_in_loop.borrow_mut();
            state.frame(ts_ms);
        }
        schedule_the_next_frame(&window_in_loop, &handle_in_loop);
    });
    // The handle carries the **JavaScript function**, not the closure:
    // the closure is running while it schedules the next frame, and the
    // wasm-bindgen trampoline holds it as `&mut` (audit 2026-10-21 §W3).
    let callback: wasm_bindgen::JsValue = closure.as_ref().clone();
    *handle.borrow_mut() = Some(callback);
    // The closure itself must outlive the page: a dropped closure
    // invalidates its JavaScript callback.
    keep_alive(closure);
    keep_alive(Rc::clone(&handle));
    let guard = handle.borrow();
    let Some(callback) = guard.as_ref() else {
        return Err("the frame callback is gone".to_string());
    };
    window
        .request_animation_frame(callback.unchecked_ref())
        .map_err(|e| format!("requestAnimationFrame failed: {e:?}"))?;
    Ok(())
}

/// Ask for the next frame.
fn schedule_the_next_frame(window: &web_sys::Window, handle: &FrameHandle) {
    let guard = handle.borrow();
    let Some(callback) = guard.as_ref() else {
        return;
    };
    let _ = window.request_animation_frame(callback.unchecked_ref());
}

impl AppState {
    /// One frame: advance the simulation and draw it.
    ///
    /// The accumulator **paces** the frames at the config's `fps_cap` —
    /// the same cadence the native app runs at, not the display's. A
    /// paced frame then advances the scene by the whole accumulated
    /// delta (clamped, see [`MAX_DT_SECONDS`]); the scene itself runs
    /// its fixed 1/120 s substeps inside that delta. The doc used to
    /// claim the step *was* the cap's interval, which the code never
    /// did (audit 2026-10-23 F-156).
    fn frame(&mut self, ts_ms: f64) {
        let delta = self.raw_delta(ts_ms);
        self.accumulator += delta;
        // The scene emits audio events for the native sound device. The
        // browser has none, so the events are **drained and dropped**
        // every frame: an undrained queue grows without bound over a
        // long session (audit 2026-10-23 F-138). Draining is safe when
        // paused too — the scene emits nothing while it is not advanced.
        let _dropped = self.sim.scene.take_audio_events();
        let cap = self.sim.cfg.fps_cap.max(1);
        if self.accumulator < 1.0 / cap as f32 {
            return;
        }
        // The clamp is the important part: a backgrounded tab comes back
        // with seconds of delta, and the parade must not jump.
        let dt = self.accumulator.min(MAX_DT_SECONDS);
        self.accumulator = 0.0;
        if !self.paused {
            self.sim.scene.advance(dt);
        }
        self.present();
    }

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

    /// Draw the scene into the buffer and hand the buffer to the
    /// renderer.
    ///
    /// The blit is the core's own ([`blit_scene`]): the sky, the
    /// parallax layers, the parade at its sub-cell offset and the grass
    /// at offset zero — the same composition the native app and the
    /// headless gate draw.
    fn present(&mut self) {
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };
        let sim = &mut self.sim;
        {
            let Sim {
                fb, font, scene, ..
            } = sim;
            let layers = scene.backdrop_blits();
            let overlay = scene.message_blit();
            blit_scene(
                fb,
                font,
                scene.grid(),
                scene.sub_cell_offset_px(),
                scene.ground_row() as usize,
                scene.sky_ramp(),
                &layers,
                overlay.as_ref(),
            );
        }
        renderer.present(&sim.fb.data, sim.fb.width, sim.fb.height);
    }

    /// Re-lay the scene on the canvas' new size.
    ///
    /// The scene keeps its **simulation** and gets a new **grid**: the
    /// parade's phase, the pendulums, the ropes and the clock survive
    /// the resize (a rebuild restarted the train at the left edge and
    /// dropped the balloons). The font is rebuilt with the same cell
    /// dims, otherwise the glyph masks no longer match the cell pitch
    /// and the frame goes black.
    fn on_resize(&mut self) {
        let grid = geometry(&self.sim.cfg, &self.canvas);
        let (cols, rows, cell_w, cell_h) = grid;
        let unchanged = (cols, rows, cell_w, cell_h)
            == (
                self.sim.cols,
                self.sim.rows,
                self.sim.cell_w,
                self.sim.cell_h,
            );
        if unchanged {
            // The grid did not move, but the canvas' backing store may
            // still have: a DPR change (the window moved between
            // monitors) alters the device-pixel box under the same CSS
            // size, and the buffer must follow **even when the grid
            // rounds to the same cells** (audit 2026-10-23 F-144).
            self.canvas.set_width(self.sim.fb.width);
            self.canvas.set_height(self.sim.fb.height);
            return;
        }
        let Ok(font) = Font::new(cell_w, cell_h) else {
            return;
        };
        self.sim.scene.resize(cols, rows, cell_w, cell_h);
        self.sim.font = font;
        self.sim.fb = FrameBuffer::new(cols as u32 * cell_w, rows as u32 * cell_h);
        self.sim.cols = cols;
        self.sim.rows = rows;
        self.sim.cell_w = cell_w;
        self.sim.cell_h = cell_h;
        self.canvas.set_width(self.sim.fb.width);
        self.canvas.set_height(self.sim.fb.height);
    }
}

/// The label of a back end, for the status line.
///
/// WHY the render module owns it: the name is the **contract** between
/// the page and the back ends, and there must be exactly one spelling
/// of it (audit 2026-10-21 §W6).
pub fn backend_label(backend: Backend) -> &'static str {
    crate::render::label(backend)
}

/// Parse the page's back-end name.
///
/// The four names are the page's selector values — "auto", "webgpu",
/// "webgl2" and "canvas2d" — and a few obvious aliases. An unknown name
/// is **refused** (`None`), never guessed at: the caller turns it into
/// an error the page shows.
pub fn parse_backend(name: &str) -> Option<Backend> {
    match name.trim().to_ascii_lowercase().as_str() {
        "auto" => Some(Backend::Auto),
        "webgpu" => Some(Backend::WebGpu),
        "webgl2" | "webgl" => Some(Backend::WebGl2),
        "canvas2d" | "canvas" | "2d" => Some(Backend::Canvas2d),
        _ => None,
    }
}

/// Ask for a back end and build it.
///
/// The request is **remembered** even when the app is not up yet: the
/// wasm boots asynchronously, so a `set_backend()` call can arrive
/// before the app exists — and it used to be silently lost then, with
/// the page showing the old back end and no sign of the switch (audit
/// 2026-10-21 §W1).
pub fn request_backend(backend: Backend) {
    PENDING_BACKEND.with(|cell| *cell.borrow_mut() = Some(backend));
    let Some(app) = the_app() else {
        // The app is not up yet: `boot` applies the request when it is.
        return;
    };
    // The machine takes the request and stamps the swap with a fresh
    // generation, so the future it spawns can prove the swap is still
    // the live one when it settles (audit 101126 §W1).
    let generation = {
        let mut state = app.borrow_mut();
        state.swap = reduce_swap(state.swap, SwapEvent::Requested(backend));
        state.swap_generation += 1;
        state.swap_generation
    };
    wasm_bindgen_futures::spawn_local(async move {
        swap_the_backend(backend, generation).await;
    });
}

/// Build the back end and put it live.
async fn swap_the_backend(backend: Backend, generation: u64) {
    match build_and_swap(backend).await {
        Ok(()) => {
            with_app(|state| {
                state.swap = settle_swap(
                    state.swap,
                    state.swap_generation,
                    generation,
                    SwapEvent::Committed,
                );
            });
        }
        Err(e) => fall_back_after_failure(backend, generation, e).await,
    }
}

/// Build a renderer on a **fresh** canvas and put it live.
///
/// WHY a fresh canvas: a canvas element carries one context type for its
/// whole life, so a back end switch cannot reuse the element the old
/// back end holds a context on (the render module says so in its own
/// docs). The new element only enters the page once its renderer is
/// built, so a failed switch leaves the parade running on the old back
/// end instead of on a black rectangle.
async fn build_and_swap(backend: Backend) -> Result<(), String> {
    let Some(app) = the_app() else {
        return Err("the parade is not running yet".to_string());
    };
    let Some(old_canvas) = with_app(|state| state.canvas.clone()) else {
        return Err("the parade is not running yet".to_string());
    };
    let canvas = a_fresh_canvas(&old_canvas)?;
    let Some((width, height)) = with_app(|state| (state.sim.fb.width, state.sim.fb.height)) else {
        return Err("the parade is not running yet".to_string());
    };
    canvas.set_width(width);
    canvas.set_height(height);
    let renderer = crate::render::create(&canvas, backend).await?;
    put_the_canvas_in_the_page(&old_canvas, &canvas)?;
    crate::input::install(&app, &canvas)?;
    with_app(|state| {
        state.canvas = canvas.clone();
        state.renderer = Some(renderer);
        state.note = None;
    });
    Ok(())
}

/// A fresh canvas element, with the old one's id.
///
/// The id matters: the page's CSS picks the canvas up by it, and the
/// page's own `#canvas` lookup has to keep finding the picture.
fn a_fresh_canvas(old: &HtmlCanvasElement) -> Result<HtmlCanvasElement, String> {
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| "the page has no document".to_string())?;
    let element = document
        .create_element("canvas")
        .map_err(|e| format!("cannot create a canvas: {e:?}"))?;
    let canvas: HtmlCanvasElement = element
        .dyn_into::<HtmlCanvasElement>()
        .map_err(|_| "the new element is not a canvas".to_string())?;
    canvas.set_id(&old.id());
    Ok(canvas)
}

/// Put the fresh canvas where the old one was.
fn put_the_canvas_in_the_page(
    old: &HtmlCanvasElement,
    new: &HtmlCanvasElement,
) -> Result<(), String> {
    let old_node: &web_sys::Node = old.unchecked_ref();
    let new_node: &web_sys::Node = new.unchecked_ref();
    let Some(parent) = old_node.parent_node() else {
        return Err("the canvas has no parent".to_string());
    };
    parent
        .replace_child(new_node, old_node)
        .map(|_| ())
        .map_err(|e| format!("cannot replace the canvas: {e:?}"))
}

/// A forced back end that is not available falls back to the automatic
/// choice: the parade keeps running and the status line says what
/// happened.
async fn fall_back_after_failure(backend: Backend, generation: u64, error: String) {
    let note = format!("{} unavailable ({error})", backend_label(backend));
    match build_and_swap(Backend::Auto).await {
        Ok(()) => {
            with_app(|state| {
                state.swap = settle_swap(
                    state.swap,
                    state.swap_generation,
                    generation,
                    SwapEvent::FellBack,
                );
                state.note = Some(note);
            });
        }
        Err(e) => {
            with_app(|state| {
                state.swap = settle_swap(
                    state.swap,
                    state.swap_generation,
                    generation,
                    SwapEvent::FellBack,
                );
                state.note = Some(format!("{note}; auto failed too ({e})"));
            });
        }
    }
}

/// The one-line summary the page shows.
pub fn status_line(state: &AppState) -> String {
    let backend = state
        .renderer
        .as_ref()
        .map(|renderer| renderer.name())
        .unwrap_or("none");
    let running = if state.paused { "paused" } else { "running" };
    let mut line = format!(
        "{backend} | {}x{} cells | {running} | {} drawn assets",
        state.sim.cols,
        state.sim.rows,
        state.sim.assets.borrow().len()
    );
    // The back end that is on its way in: the page must say that a
    // switch is in flight, or it shows the **old** back end with no
    // sign of the switch (audit 2026-10-21 §W1).
    if let Swap::InFlight(backend) = state.swap {
        line.push_str(&format!(" | switching to {}", backend_label(backend)));
    }
    if let Some(note) = &state.note {
        line.push_str(" | ");
        line.push_str(note);
    }
    line
}

/// The page's canvas, once the document is parsed.
async fn the_canvas() -> Result<HtmlCanvasElement, String> {
    wait_for_the_document().await;
    let document = web_sys::window()
        .and_then(|w| w.document())
        .ok_or_else(|| "the page has no document".to_string())?;
    let element = document
        .get_element_by_id("canvas")
        .ok_or_else(|| "the page has no #canvas element".to_string())?;
    element
        .dyn_into::<HtmlCanvasElement>()
        .map_err(|_| "the #canvas element is not a canvas".to_string())
}

/// Wait for the document to be parsed.
///
/// The page's script is a module at the end of the body, so the DOM is
/// normally ready already; this is the belt to that braces.
async fn wait_for_the_document() {
    let Some(window) = web_sys::window() else {
        return;
    };
    let Some(document) = window.document() else {
        return;
    };
    if document.ready_state() != "loading" {
        return;
    }
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        let resolver = resolve.clone();
        let doc = document.clone();
        let closure = Closure::<dyn FnMut(web_sys::Event)>::new({
            let resolver = resolver.clone();
            move |_event: web_sys::Event| {
                let _ = resolver.call0(&wasm_bindgen::JsValue::NULL);
            }
        });
        let listening = doc
            .add_event_listener_with_callback("DOMContentLoaded", closure.as_ref().unchecked_ref())
            .is_ok();
        if listening {
            // The closure must outlive this call: a dropped closure
            // invalidates its JavaScript callback.
            closure.forget();
        } else {
            let _ = resolver.call0(&wasm_bindgen::JsValue::NULL);
        }
    });
    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The page's selector offers exactly four values, and the parser
    /// must accept every one of them — the contract between the page
    /// and the back ends (audit 2026-10-21 §W6).
    #[test]
    fn the_page_selector_values_all_parse() {
        for name in ["auto", "webgpu", "webgl2", "canvas2d"] {
            assert!(
                parse_backend(name).is_some(),
                "the selector value {name:?} must parse"
            );
        }
        assert!(
            parse_backend("WEBGL2").is_some(),
            "the name is case-insensitive"
        );
        assert!(
            parse_backend("nonsense").is_none(),
            "an unknown name is refused"
        );
    }

    /// The label of a back end is the name the page knows it by.
    #[test]
    fn the_label_is_the_selectors_value() {
        assert_eq!(backend_label(Backend::Auto), "auto");
        assert_eq!(backend_label(Backend::WebGpu), "webgpu");
        assert_eq!(backend_label(Backend::WebGl2), "webgl2");
        assert_eq!(backend_label(Backend::Canvas2d), "canvas2d");
    }

    /// The swap machine (audit 101126 §W1): a request arms it, a
    /// request while one is in flight **replaces** the target, and
    /// exactly one terminal event per swap returns to Idle.
    #[test]
    fn the_swap_machine_replaces_and_settles() {
        let mut swap = Swap::Idle;
        swap = reduce_swap(swap, SwapEvent::Requested(Backend::WebGpu));
        assert_eq!(swap, Swap::InFlight(Backend::WebGpu));
        swap = reduce_swap(swap, SwapEvent::Requested(Backend::Canvas2d));
        assert_eq!(swap, Swap::InFlight(Backend::Canvas2d));
        swap = reduce_swap(swap, SwapEvent::Committed);
        assert_eq!(swap, Swap::Idle);
        // The failure path settles once, too.
        swap = reduce_swap(swap, SwapEvent::Requested(Backend::WebGl2));
        swap = reduce_swap(swap, SwapEvent::FellBack);
        assert_eq!(swap, Swap::Idle);
        // A terminal event for nothing is a no-op.
        assert_eq!(reduce_swap(Swap::Idle, SwapEvent::Committed), Swap::Idle);
    }

    /// The generation check (audit 101126 §W1): a replaced swap's late
    /// arrival is ignored; the live generation's is applied.
    #[test]
    fn a_late_arrival_from_a_replaced_swap_is_ignored() {
        let in_flight = Swap::InFlight(Backend::WebGpu);
        // A stale generation changes nothing.
        assert_eq!(
            settle_swap(in_flight, 7, 1, SwapEvent::Committed),
            in_flight
        );
        // The live generation settles exactly once.
        assert_eq!(
            settle_swap(in_flight, 7, 7, SwapEvent::FellBack),
            Swap::Idle
        );
        assert_eq!(
            settle_swap(Swap::Idle, 7, 7, SwapEvent::FellBack),
            Swap::Idle
        );
    }
}
