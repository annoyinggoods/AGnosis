//! Borderless popup window + event loop.
//!
//! `run` shows a borderless window at the resolved [`PopupArgs`] origin/size
//! with the native background painted black before first show, then drives
//! the scene at `cfg.fps_cap` once the background wgpu device arrives. The
//! black window is the frame until the first draw; there is no spinner.
//!
//! The Win32 window class is [`crate::platform::POPUP_WINDOW_CLASS`]
//! (`"AGnosisPopup"`) via winit 0.30's `WindowAttributesExtWindows`, so the
//! daemon's `FindWindowW("AGnosisPopup", NULL)` close keeps working.
//!
//! Neither the init thread nor the surface setup ever blocks the event
//! loop: the thread posts one bundle over a channel and the loop picks it
//! up in `tick`. ESC stays responsive throughout startup. When GPU init
//! fails the black window keeps running degraded (scene still advances)
//! instead of panicking.
//!
//! The per-frame path stays allocation-light and never blocks: the balloon
//! watcher does its scanning, settling and image decoding on its own worker
//! thread and the popup only polls the finished sprites (audit P0-1), and
//! the input line text is built only when its content changes.
//!
//! The typed balloon name is resolved and loaded through
//! [`crate::balloon_resolve`], the same path the watcher uses, so a typed
//! name honours the same radii-stamped `.sprite.json` cache and finds the
//! same files the watcher auto-loads (audit P1-13).

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Fullscreen, Window, WindowButtons, WindowLevel};

#[cfg(target_os = "windows")]
use winit::platform::windows::WindowAttributesExtWindows;

/// Resolved placement for one popup invocation.
#[derive(Debug, Clone, Copy)]
pub struct PopupArgs {
    /// Top-left corner in physical pixels.
    pub origin: (i32, i32),
    /// Window size in physical pixels.
    pub size: (u32, u32),
    /// DPI scale the grid was resolved with.
    pub scale: f64,
}

/// Popup failures.
///
/// GPU failures are not errors: a failed init or a failed draw degrades to
/// the black window that still advances the scene (see `degraded`), so the
/// window/event-loop and the argument check are the only error paths.
#[derive(Debug, thiserror::Error)]
pub enum PopupError {
    /// winit window or event-loop failure.
    #[error("window error: {0}")]
    Window(String),
    /// Config, font, watcher or platform-helper failure.
    #[error("config error: {0}")]
    Config(String),
}

/// Validate popup args before touching winit.
fn validate_args(args: &PopupArgs) -> Result<(), PopupError> {
    if args.size.0 == 0 || args.size.1 == 0 {
        return Err(PopupError::Config(format!(
            "popup size must be nonzero, got {}x{}",
            args.size.0, args.size.1
        )));
    }
    if !args.scale.is_finite() || args.scale <= 0.0 {
        return Err(PopupError::Config(format!(
            "popup scale must be positive, got {}",
            args.scale
        )));
    }
    Ok(())
}

/// Fixed-step frame pacing derived from `fps_cap`.
///
/// The clamp is the core's: the repair pass bounds `fps_cap` with the
/// **same** constants, so the panel and the runtime cannot drift apart
/// (audit 2026-10-21 §G31).
fn frame_interval(fps_cap: u32) -> Duration {
    use agnosis_core::config::bounds::{FPS_CAP_DEFAULT, FPS_CAP_MAX, FPS_CAP_MIN};
    let fps = if (FPS_CAP_MIN..=FPS_CAP_MAX).contains(&fps_cap) {
        fps_cap
    } else {
        FPS_CAP_DEFAULT
    } as u64;
    Duration::from_nanos(1_000_000_000 / fps)
}

/// Snap `window` to borderless fullscreen on the winit monitor containing
/// `origin` (falling back to the primary monitor). No-op when no monitor
/// matches or the backend refuses; the caller still shows the window at the
/// CLI-resolved rect.
fn snap_to_fullscreen(event_loop: &ActiveEventLoop, window: &Window, origin: (i32, i32)) {
    let monitor = event_loop
        .available_monitors()
        .find(|m| {
            let pos = m.position();
            let size = m.size();
            origin.0 >= pos.x
                && origin.0 < pos.x + size.width as i32
                && origin.1 >= pos.y
                && origin.1 < pos.y + size.height as i32
        })
        .or_else(|| event_loop.primary_monitor())
        .or_else(|| event_loop.available_monitors().next());
    match monitor {
        Some(m) => {
            window.set_fullscreen(Some(Fullscreen::Borderless(Some(m))));
            agnosis_core::trace::trace("popup.fullscreen-snapped");
        }
        None => agnosis_core::trace::trace("popup.fullscreen-snapped=no-monitor"),
    }
}

/// What one pressed key means for the Fix 5 input line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputAction {
    /// Ignore (modifier, unhandled key, ...).
    None,
    /// Buffer edited in place.
    Edit,
    /// Enter: commit the buffer.
    Commit,
}

/// Max typed characters kept in the input buffer: the HUD write path clips
/// instead of wrapping, so longer input would render off-line unseen
/// (audit §P1-9).
pub(crate) const INPUT_MAX_CHARS: usize = 64;

/// Usage hint shown on the input line while the buffer is empty.
///
/// Short on purpose: the hint is the first thing on screen and every
/// grid must be able to show at least its **tail** — the newest
/// characters survive a narrow line (`scene::tail_window`), so the
/// HELP chord has to sit at the end, not the middle
/// (audit 2026-10-23 §F-101). The folder is `Ctrl+O`, not a bare `O`
/// (a bare letter made every balloon name starting with "o"
/// untypeable), F12 is the shot, F2 opens the draw-your-own editor and
/// F1 opens the in-app HELP — the full README, scrollable
/// (roadmap HELP_WINDOW §1).
const INPUT_HINT: &str =
    "ADD BALLOON: NAME + ENTER - CTRL O FOLDER - F12 SHOT - F2 EDITOR - F1 HELP_";

/// How long a status message stays on the input line.
const FEEDBACK_LIFETIME: Duration = Duration::from_secs(3);

/// How long the onboarding hint stays: ten times an ordinary status
/// message, because "run this once" needs to survive more than a glance.
const ONBOARDING_LIFETIME: Duration = Duration::from_secs(30);

/// The one-time hint for a parade no daemon is watching: the global
/// hotkey is the **daemon's** keyboard hook, so a bare double-click run
/// has no browser minimization and no chord at all — and nothing else in
/// the user's path ever says so. Block-font charset only (A-Z, 0-9,
/// `. : _ - / \` and space).
const NO_DAEMON_HINT: &str = "NO DAEMON: RUN AGNOSIS INSTALL AND AGNOSIS START FOR THE HOTKEY";

/// How often the demoted parade probes for its overlay windows' classes.
///
/// While the parade has stepped out of always-on-top so an overlay window
/// (the HELP reader, the editor) can be read over it, it owes one
/// `FindWindowW` per interval — cheap, but once per rendered frame would
/// still be waste. The interval only decides how long the parade stays
/// demoted after the last overlay closes, so a quarter of a second is
/// invisible to the user.
const OVERLAY_PROBE_INTERVAL: Duration = Duration::from_millis(250);

/// True when the overlay watch owes a class probe: at least
/// [`OVERLAY_PROBE_INTERVAL`] has passed since `last`.
fn overlay_probe_due(last: Instant, now: Instant) -> bool {
    now.duration_since(last) >= OVERLAY_PROBE_INTERVAL
}

/// One probe round of the overlay watch: an overlay still open keeps its
/// slot, a closed one drops out, and the watch ends when the last slot
/// does — the caller re-promotes then. The probe replaces the belief
/// slot by slot, so a second overlay never resets the first. Pure, so
/// the demote/re-promote machine is testable without a window (audit
/// 101126 §S8).
fn fold_overlay_watch(
    watch: &OverlayWatch,
    help_open: bool,
    editor_open: bool,
) -> Option<OverlayWatch> {
    let help = watch.help && help_open;
    let editor = watch.editor && editor_open;
    if !(help || editor) {
        return None;
    }
    Some(OverlayWatch {
        help,
        editor,
        since: Instant::now(),
    })
}

/// The spawned overlay window the parade steps aside for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Overlay {
    /// The HELP reader (F1).
    Help,
    /// The draw-your-own editor (F2).
    Editor,
}

/// Which overlay windows the parade is watching, and when the probes last
/// ran.
///
/// Both slots are tracked together so HELP and the editor can be open at
/// once: the parade stays demoted until the **last** one closes, never
/// re-promoting under a window the user is still reading.
#[derive(Debug, Clone, Copy)]
struct OverlayWatch {
    /// The HELP reader is (still believed) open.
    help: bool,
    /// The editor is (still believed) open.
    editor: bool,
    /// The instant of the last probe round (the gate).
    since: Instant,
}

/// Edit `buffer` for one pressed key. `named` is the logical key's named
/// variant (`Escape` is handled by the caller and never reaches here);
/// `text` is the platform-typed text, if any. ASCII graphic characters and
/// spaces append up to [`INPUT_MAX_CHARS`]; Backspace deletes. Returns
/// [`InputAction::None`] when the key contributed nothing (unhandled key, or
/// text that filtered down to zero characters), so the caller need not
/// redraw on dead keys (audit §P1-9).
pub(crate) fn handle_input_key(
    buffer: &mut String,
    named: Option<&winit::keyboard::NamedKey>,
    text: Option<&str>,
) -> InputAction {
    if let Some(named) = named {
        return edit_for_named_key(buffer, named);
    }
    edit_for_text(buffer, text)
}

/// Named-key half of [`handle_input_key`]: Enter commits, Backspace deletes,
/// Space appends (capped), everything else is ignored.
fn edit_for_named_key(buffer: &mut String, named: &winit::keyboard::NamedKey) -> InputAction {
    use winit::keyboard::NamedKey;
    match named {
        NamedKey::Enter => InputAction::Commit,
        NamedKey::Backspace => match buffer.pop() {
            Some(_) => InputAction::Edit,
            None => InputAction::None,
        },
        NamedKey::Space => push_capped(buffer, ' '),
        _ => InputAction::None,
    }
}

/// Typed-text half of [`handle_input_key`]: appends ASCII graphics and
/// spaces up to the cap; reports [`InputAction::None`] when nothing
/// survived the filter.
///
/// The space is accepted here too, not only on the `Named(Space)` path:
/// some platforms (and IMEs) deliver a space as **text**, and a balloon
/// name may contain one — dropping it made "red duck.png" typeable only
/// as "redduck.png" there (audit 2026-10-23 §F-118).
fn edit_for_text(buffer: &mut String, text: Option<&str>) -> InputAction {
    let text = match text {
        Some(text) => text,
        None => return InputAction::None,
    };
    let mut added = false;
    for ch in text.chars() {
        let acceptable = ch == ' ' || ch.is_ascii_graphic();
        if acceptable && push_capped(buffer, ch) == InputAction::Edit {
            added = true;
        }
    }
    commit_if_added(added)
}

/// [`InputAction::Edit`] when anything landed in the buffer, else
/// [`InputAction::None`] so dead keys cost no redraw.
fn commit_if_added(added: bool) -> InputAction {
    if added {
        InputAction::Edit
    } else {
        InputAction::None
    }
}

/// Push one char unless the buffer is at [`INPUT_MAX_CHARS`].
fn push_capped(buffer: &mut String, ch: char) -> InputAction {
    if buffer.chars().count() >= INPUT_MAX_CHARS {
        return InputAction::None;
    }
    buffer.push(ch);
    InputAction::Edit
}

/// Spawn the HELP window as a detached child process.
///
/// The spawn mirrors the daemon's popup/editor spawns: same executable,
/// detached with no console window, stdio nulled, and the child reaped
/// by a short-lived thread (`crate::daemon::reap_when_it_exits`), so the
/// popup — which may outlive the reader — never leaves a zombie behind.
///
/// A second reader is **refused, not stacked**: the live-window probe
/// (`help_is_open` — the window class on Windows, the claimed pid file
/// on Unix) answers before the spawn, and the child re-claims the same
/// slot at its own startup, so a race between two spawners still ends
/// with one reader (audit 2026-10-25).
///
/// The parade's monitor geometry rides along (`--origin`/`--size`), so
/// the reader opens on **this** monitor — the same monitor the editor's
/// spawn passes — instead of re-resolving from the config and possibly
/// following the cursor elsewhere.
fn spawn_detached_help(origin: (i32, i32), size: (u32, u32)) -> Result<&'static str, String> {
    if crate::platform::window::help_is_open() {
        return Ok("HELP ALREADY OPEN");
    }
    spawn_detached_child("help", origin, size)?;
    agnosis_core::trace::trace("popup.help_spawned");
    Ok("OPENED HELP")
}

/// F2's spawn: the draw-your-own editor as a detached child.
///
/// The single-instance probe answers before the spawn, exactly like the
/// HELP spawn above, and the argv is the contract
/// [`crate::daemon::editor_spawn_argv`] produces — the editor computes
/// its own 80 %-of-monitor rect from the `--size` monitor pair.
fn spawn_detached_editor(origin: (i32, i32), size: (u32, u32)) -> Result<&'static str, String> {
    if crate::platform::window::editor_is_open() {
        return Ok("EDITOR ALREADY OPEN");
    }
    spawn_detached_child("editor", origin, size)?;
    agnosis_core::trace::trace("popup.editor_spawned");
    Ok("OPENED EDITOR")
}

/// Spawn `<exe> <verb> --origin X,Y --size W,H` as a detached child with
/// nulled stdio — the shared tail of the HELP and the editor spawns.
///
/// Detached and stdio-nulled on purpose: the child outlives the parade
/// (the user can close it whenever) and a GUI-subsystem child must never
/// hold the spawner's pipes open.
fn spawn_detached_child(verb: &str, origin: (i32, i32), size: (u32, u32)) -> Result<(), String> {
    let exe = crate::paths::current_exe().map_err(|e| format!("current exe: {e}"))?;
    let mut command = std::process::Command::new(exe);
    command.arg(verb);
    command
        .arg("--origin")
        .arg(format!("{},{}", origin.0, origin.1))
        .arg("--size")
        .arg(format!("{},{}", size.0, size.1));
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        use windows::Win32::System::Threading::{CREATE_NO_WINDOW, DETACHED_PROCESS};
        command.creation_flags(DETACHED_PROCESS.0 | CREATE_NO_WINDOW.0);
    }
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    let child = command.spawn().map_err(|e| format!("{verb} spawn: {e}"))?;
    crate::daemon::reap_when_it_exits(child);
    Ok(())
}

/// Load one balloon by typed name.
///
/// Thin alias to the shared path: the watcher and the typed name must not
/// drift apart (audit §P1-13). The shared implementation resolves absolute
/// names, exact hits and a deterministic case-insensitive recursive scan,
/// honours the radii-stamped `<stem>.sprite.json` cache, and reports a
/// missing name instead of guessing.
fn load_balloon_by_name(
    dir: &std::path::Path,
    name: &str,
    rx: i64,
    ry: i64,
) -> Result<agnosis_core::balloon::AsciiSprite, String> {
    crate::balloon_resolve::load(dir, name, rx, ry)
}

/// A balloon the user asked for by typed name.
struct LoadRequest {
    /// The name as typed.
    name: String,
    /// The folder the name is resolved against.
    dir: std::path::PathBuf,
    /// The conversion radii.
    rx: i64,
    ry: i64,
}

/// The answer to a [`LoadRequest`].
struct LoadResult {
    /// The name the request carried, for the status line.
    name: String,
    /// The conversion radii the request was made with.
    ///
    /// A geometry change between the request and the answer leaves the
    /// answer **stale** — a sprite converted for the old cell aspect
    /// would be drawn on a disc of another shape. The drain drops it,
    /// the third staleness discipline beside the watcher's settle and
    /// the asset loader's generation (audit 2026-10-23 §F-117).
    radii: (i64, i64),
    /// The loaded sprite, or the reason it could not be loaded.
    sprite: Result<agnosis_core::balloon::AsciiSprite, String>,
}

/// The typed-name loader: a worker thread that resolves and converts a
/// balloon while the UI keeps drawing.
///
/// WHY a worker: the resolve is a recursive folder scan and the conversion
/// is a full image decode — together they can take a second or two, which
/// is far too long to block the UI thread on (audit 2026-10-20 §3.2). The
/// watcher already works this way; the typed path now follows suit.
///
/// The thread ends when the request channel closes, which happens when the
/// popup drops the loader. It is **not** joined on drop: a conversion in
/// flight must not delay the popup's exit, and the process is on its way
/// out anyway.
struct BalloonLoader {
    requests: std::sync::mpsc::Sender<LoadRequest>,
    results: std::sync::mpsc::Receiver<LoadResult>,
    /// The names with a conversion still in flight. This set is the
    /// request identity the channel alone could not carry: two Enters on
    /// one name used to queue two conversions and push two copies of the
    /// same balloon into the five-slot ring (audit 101126 §B6).
    pending: std::collections::HashSet<String>,
}

/// Why a typed balloon was not queued for conversion.
enum LoadReject {
    /// That exact name is already converting on the worker.
    InFlight,
    /// The worker is gone; nothing would answer.
    WorkerGone,
}

impl BalloonLoader {
    /// Start the loader thread.
    fn start() -> Option<Self> {
        let (request_tx, request_rx) = std::sync::mpsc::channel::<LoadRequest>();
        let (result_tx, result_rx) = std::sync::mpsc::channel::<LoadResult>();
        let spawned = std::thread::Builder::new()
            .name("agnosis-balloon-load".to_string())
            .spawn(move || loader_worker(request_rx, result_tx));
        if spawned.is_err() {
            agnosis_core::trace::warn("popup: could not start the balloon loader thread");
            return None;
        }
        Some(Self {
            requests: request_tx,
            results: result_rx,
            pending: std::collections::HashSet::new(),
        })
    }

    /// Ask the worker to load one balloon. `Err` when the request could
    /// not be queued: that exact conversion is already in flight, or the
    /// worker is gone.
    fn request(
        &mut self,
        dir: &std::path::Path,
        name: &str,
        rx: i64,
        ry: i64,
    ) -> Result<(), LoadReject> {
        if !self.pending.insert(name.to_string()) {
            return Err(LoadReject::InFlight);
        }
        let request = LoadRequest {
            name: name.to_string(),
            dir: dir.to_path_buf(),
            rx,
            ry,
        };
        if self.requests.send(request).is_err() {
            self.pending.remove(name);
            return Err(LoadReject::WorkerGone);
        }
        Ok(())
    }

    /// Take the answers that are ready. Never blocks. An answered name
    /// leaves the in-flight set, so the same name can be asked again.
    fn drain(&mut self) -> Vec<LoadResult> {
        let mut out = Vec::new();
        while let Ok(result) = self.results.try_recv() {
            self.pending.remove(&result.name);
            out.push(result);
        }
        out
    }
}

/// The loader thread's loop: load what is asked, report the answer.
fn loader_worker(
    requests: std::sync::mpsc::Receiver<LoadRequest>,
    results: std::sync::mpsc::Sender<LoadResult>,
) {
    for request in requests {
        let sprite = load_balloon_by_name(&request.dir, &request.name, request.rx, request.ry);
        let answer = LoadResult {
            name: request.name,
            radii: (request.rx, request.ry),
            sprite,
        };
        if results.send(answer).is_err() {
            break;
        }
    }
}

/// The balloon folder, or an **empty** path when it cannot be resolved.
///
/// The fallback is deliberately empty and not a relative name: a
/// relative path would be scanned relative to the current working
/// directory, which for the popup is wherever the daemon happened to
/// start (audit 2026-10-21 §G24).
fn balloon_folder() -> std::path::PathBuf {
    match crate::paths::balloon_dir() {
        Ok(dir) => dir,
        Err(e) => {
            agnosis_core::trace::trace(&format!("popup.balloons error: {e}"));
            std::path::PathBuf::new()
        }
    }
}

/// Open the balloon folder in the OS file manager (audit addendum note 5):
/// `explorer` / `open` / `xdg-open`.
fn open_balloon_folder(dir: &std::path::Path) -> Result<(), String> {
    if !dir.is_dir() {
        return Err(format!("FOLDER MISSING: {}", dir.display()));
    }
    #[cfg(target_os = "windows")]
    let spawned = std::process::Command::new("explorer").arg(dir).spawn();
    #[cfg(target_os = "macos")]
    let spawned = std::process::Command::new("open").arg(dir).spawn();
    #[cfg(target_os = "linux")]
    let spawned = std::process::Command::new("xdg-open").arg(dir).spawn();
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    let spawned: Result<std::process::Child, std::io::Error> = Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "no file manager",
    ));
    spawned
        .map(|_| ())
        .map_err(|e| format!("CANNOT OPEN FOLDER: {e}"))
}

/// Balloon conversion radii from the configured radius in rows and the
/// live grid cell aspect (same helpers as the scene).
fn balloon_radii(cfg: &agnosis_core::config::Config, cell_w: u32, cell_h: u32) -> (i64, i64) {
    let (rx, ry) = agnosis_core::balloon::balloon_radii_cells(
        cfg.balloons.balloon_radius_rows,
        cell_w,
        cell_h,
    );
    (rx.round() as i64, ry.round() as i64)
}

struct PopupApp {
    cfg: agnosis_core::config::Config,
    args: PopupArgs,
    window: Option<Arc<Window>>,
    scene: Option<agnosis_core::scene::Scene>,
    font: Option<agnosis_core::font::Font>,
    watcher: Option<crate::watcher::BalloonWatcher>,
    /// The typed-name loader: a worker thread that resolves and converts
    /// a balloon the user asked for by name, so the UI thread never
    /// blocks on a folder scan or an image decode (audit 2026-10-20
    /// §3.2).
    loader: Option<BalloonLoader>,
    /// The GPU life-cycle: see [`Gpu`].
    gpu: Gpu,
    first_frame: bool,
    last_tick: Instant,
    next_frame: Instant,
    exit: bool,
    /// Fix 5: text typed on the `add balloon` line below the grass.
    input: String,
    /// Status message with a show-until instant and a per-message
    /// lifetime (added / not found / ...): an ordinary message lives
    /// [`FEEDBACK_LIFETIME`], the onboarding hint lives
    /// [`ONBOARDING_LIFETIME`].
    feedback: Option<(String, Instant, Duration)>,
    /// The overlay watch: `Some` from the moment the parade has
    /// **demoted** itself from always-on-top so a spawned overlay window
    /// (HELP, the editor) can be read over it, until the last one closes
    /// and the parade re-promotes. The probes run on a slow gate
    /// ([`OVERLAY_PROBE_INTERVAL`]) because a `FindWindowW` every frame
    /// is waste.
    overlay_watch: Option<OverlayWatch>,
    /// Conversion radii the scene/watcher were built with.
    balloon_radii: (i64, i64),
    /// The grid geometry `(cols, rows, cell_w, cell_h)` the scene, the
    /// font and the watcher's radii follow. A resize that changes it
    /// re-lays the scene, rebuilds the font and republishes the radii
    /// (audit P0-4).
    geometry: (usize, usize, u32, u32),
    /// The monitor origin a **pending** fullscreen snap must run around.
    ///
    /// Snapping needs the `ActiveEventLoop` (it enumerates the
    /// monitors), which the reload path does not have — so
    /// [`Self::move_the_window`] leaves the target here and the event
    /// loop applies it at [`Self::apply_pending_fullscreen`]. `None`
    /// when there is nothing pending (audit 2026-10-23 §F-091).
    pending_fullscreen: Option<(i32, i32)>,
    /// The modifier keys held right now, tracked from the
    /// `ModifiersChanged` event (winit delivers the modifiers separately
    /// from the key event). The folder shortcut needs them.
    modifiers: winit::keyboard::ModifiersState,
    /// True when the input line changed and the scene needs the new text.
    /// The text itself is built only then, so the per-frame path does not
    /// allocate (audit P0-1).
    line_dirty: bool,
    /// The live config watcher (roadmap Extension 1 §1.3).
    ///
    /// It watches the config file's **directory** and reports that the
    /// file changed; the read and the parse happen here, on the UI
    /// thread, because a config file is a few kB.
    config_watcher: Option<crate::config_watch::ConfigWatcher>,
    /// The backdrop worker: it turns the configured pictures into ASCII
    /// tiles off the UI thread (roadmap Extension 4 §1.2).
    backdrops: crate::backdrop_loader::BackdropLoader,
    /// The stats counters, read once at startup and refreshed when a
    /// parade is recorded.
    stats: Option<agnosis_core::stats::Stats>,
    /// The sound: a worker thread with the device (roadmap Extension 3
    /// §2.2). The scene hands it events; the popup never renders audio
    /// itself.
    audio: crate::audio::AudioContext,
    /// The pointer's position in window pixels.
    ///
    /// The popup is borderless and fullscreen, so the pointer's position
    /// maps straight onto the grid — no origin correction (roadmap
    /// Extension 3 §5.1).
    pointer: (i64, i64),
    /// The worker that loads the drawn art for a config reload.
    ///
    /// A drawn frame is a JSON document of up to four frames — hundreds
    /// of kilobytes — and the reload used to parse up to nineteen of
    /// them inside `tick`, which froze the parade (audit 2026-10-21 §G7).
    asset_loader: crate::asset_loader::AssetLoader,
    /// Whether the asset worker is gone, so the reload falls back to
    /// loading the drawn art on the calling thread.
    ///
    /// The fallback happens **once** per reload: without the flag the
    /// popup would reload the assets on every frame.
    assets_worker_gone: bool,
    /// The screenshot worker: the PNG encode and the clipboard write run
    /// off the UI thread (audit 2026-10-22 §P2-13).
    shots: crate::shot_worker::ShotWorker,
}

impl PopupApp {
    fn tick(&mut self) {
        if self.exit {
            return;
        }
        let now = Instant::now();
        if now < self.next_frame {
            return;
        }
        self.catch_up_deadline(now);
        self.poll_gpu();
        let dt = self.sim_dt(now);
        self.drain_watcher_into_scene();
        self.drain_loader_into_scene();
        self.drain_backdrops_into_scene();
        self.drain_config_watcher();
        self.drain_assets_into_scene();
        self.drain_shot_outcomes();
        self.poll_overlay_watch();
        self.step_simulation(dt);
        self.pump_the_audio();
        self.draw_frame();
        if let Some(window) = self.window.as_ref() {
            window.request_redraw();
        }
    }

    /// Advance `next_frame` past `now` so one slow frame never spirals.
    fn catch_up_deadline(&mut self, now: Instant) {
        let interval = frame_interval(self.cfg.fps_cap);
        while self.next_frame <= now {
            self.next_frame += interval;
        }
    }

    /// Clamped delta since the last tick for `scene.advance`.
    fn sim_dt(&mut self, now: Instant) -> f32 {
        let dt = now
            .duration_since(self.last_tick)
            .as_secs_f32()
            .clamp(0.0, 0.5);
        self.last_tick = now;
        dt
    }

    /// Drain watcher outcomes into the scene; missing pieces are a no-op.
    ///
    /// The watcher works on its own thread, so this is a channel poll and
    /// nothing more: the folder scan, the settle wait and the image decode
    /// never run on the UI thread (audit §P0-1). Every outcome answers on
    /// the input line, so a dropped image is no longer silent
    /// (audit 2026-10-23 §F-104).
    fn drain_watcher_into_scene(&mut self) {
        let outcomes = match self.watcher.as_mut() {
            Some(watcher) => watcher.drain(),
            None => return,
        };
        let messages: Vec<(Option<agnosis_core::balloon::AsciiSprite>, String)> =
            outcomes.into_iter().map(watcher_message).collect();
        for (sprite, message) in messages {
            self.add_balloon_and_message(sprite, &message);
        }
    }

    /// Hand one finished result to the scene and speak it on the input
    /// line: a sprite (when there is one) goes into the ring, the
    /// message always lands.
    ///
    /// The **one** `push_balloon` road for the watcher and the
    /// typed-name loader, so the two cannot drift (audit §P1-13);
    /// extracted so both drains stay flat (audit 2026-10-23 §F-109).
    fn add_balloon_and_message(
        &mut self,
        sprite: Option<agnosis_core::balloon::AsciiSprite>,
        message: &str,
    ) {
        if let (Some(sprite), Some(scene)) = (sprite, self.scene.as_mut()) {
            scene.push_balloon(sprite);
        }
        self.set_feedback(message);
    }

    /// Take the finished backdrop tiles and install them in the scene.
    ///
    /// The conversion happens on the worker thread, so the parade never
    /// stutters while a picture is decoded; the tiles arrive through the
    /// channel and are installed here, between two frames.
    fn drain_backdrops_into_scene(&mut self) {
        let Some(tiles) = self.backdrops.drain() else {
            return;
        };
        if let Some(scene) = self.scene.as_mut() {
            scene.set_backdrops(tiles);
        }
    }

    /// Ask the backdrop worker to build the configured layers.
    ///
    /// The request is cheap: the worker does the decoding. An empty list
    /// clears the layers, so removing the `[[backdrop]]` table takes the
    /// backdrop away.
    fn request_backdrops(&mut self) {
        let layers = self.cfg.backdrop.clone();
        self.backdrops.request(layers);
    }

    /// Reload the config when the watcher says the file changed.
    ///
    /// The read and the parse happen here, on the UI thread: a config
    /// file is a few kB and the parse is microseconds, so it is bounded
    /// work — the same discipline the balloon path uses.
    ///
    /// A **malformed** config must never take the parade down: the old
    /// values stay, the error is traced, and the file is left alone.
    fn drain_config_watcher(&mut self) {
        let changed = match self.config_watcher.as_mut() {
            Some(watcher) => !watcher.drain().is_empty(),
            None => false,
        };
        if !changed {
            return;
        }
        self.reload_config();
    }

    /// Read, parse and apply the config file.
    ///
    /// A **rejected** edit is not silent any more: the input line says
    /// so for the feedback lifetime, while the old values keep the
    /// parade running and the file is left alone
    /// (audit 2026-10-23 §F-097 — `agnosis config validate` is the
    /// terminal's way to ask the same question).
    fn reload_config(&mut self) {
        let path = match crate::paths::config_file() {
            Ok(path) => path,
            Err(e) => {
                agnosis_core::trace::trace(&format!("config.reload: cannot locate the file: {e}"));
                return;
            }
        };
        let Some(cfg) = read_reload_config(&path) else {
            agnosis_core::trace::trace("config.reload: broken config, keeping the old values");
            self.set_feedback("CONFIG ERROR - OLD VALUES KEPT");
            return;
        };
        self.apply_new_config(cfg);
    }

    /// Apply a reloaded configuration to the popup and the scene.
    ///
    /// The **scene** is updated when the worker has loaded the drawn art
    /// (the assets are hundreds of kilobytes of JSON each, and parsing
    /// them inside `tick` froze the parade — audit 2026-10-21 §G7).
    /// Everything else — the sound, the radii, the counter, the
    /// backdrops, the monitor — is applied at once.
    fn apply_new_config(&mut self, cfg: agnosis_core::config::Config) {
        let monitor_changed = cfg.monitor != self.cfg.monitor;
        self.cfg = cfg.clone();
        self.request_the_assets(&cfg);
        if monitor_changed {
            self.re_resolve_monitor();
        }
        self.refresh_radii();
        self.refresh_counter();
        self.refresh_the_sound();
        self.request_backdrops();
        agnosis_core::trace::trace("config.reload applied");
    }

    /// Ask the worker to load the assets the configuration names.
    ///
    /// The drawn art and the track folder are parameters: the scene
    /// never looks a folder up, so the shell resolves the platform's
    /// folders and hands them over.
    ///
    /// A worker that is gone (or a thread that could not be spawned)
    /// falls back to loading on the calling thread: slow, but the reload
    /// still happens.
    fn request_the_assets(&mut self, cfg: &agnosis_core::config::Config) {
        let assets = crate::host::asset_dirs(cfg);
        if self.asset_loader.request(cfg, assets.clone()) {
            return;
        }
        agnosis_core::trace::warn("assets: the worker is gone, loading on this thread");
        self.assets_worker_gone = true;
        self.apply_the_config_to_the_scene(cfg, &assets);
    }

    /// Apply the configuration to the scene, loading the drawn art here.
    ///
    /// The fallback path: the assets are read on the calling thread, so
    /// it must not be the UI thread's normal road.
    fn apply_the_config_to_the_scene(
        &mut self,
        cfg: &agnosis_core::config::Config,
        assets: &agnosis_core::assets::AssetDirs,
    ) {
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        scene.apply_config(cfg, assets);
    }

    /// Apply the pending configuration to the scene, with the assets the
    /// worker loaded.
    ///
    /// The scene asks for the assets it needs through a **loader
    /// closure**, and the closure answers from the finished set — the
    /// disk is never touched here.
    fn drain_assets_into_scene(&mut self) {
        use crate::asset_loader::Drain;
        if self.assets_worker_gone {
            return;
        }
        match self.asset_loader.drain() {
            Drain::Pending => {}
            Drain::Gone => {
                // The worker died: load on this thread, so the reload is
                // not lost — and only once, or the assets would be read
                // on every frame.
                self.assets_worker_gone = true;
                let cfg = self.cfg.clone();
                let assets = crate::host::asset_dirs(&cfg);
                self.apply_the_config_to_the_scene(&cfg, &assets);
            }
            Drain::Ready(loaded) => {
                let Some(scene) = self.scene.as_mut() else {
                    return;
                };
                let cfg = &loaded.cfg;
                let tracks = crate::host::asset_dirs(cfg).tracks;
                scene.apply_config_with(cfg, loaded.assets.loader(), tracks);
                agnosis_core::trace::trace(&format!(
                    "config.reload applied with {} drawn assets",
                    loaded.assets.len()
                ));
            }
        }
    }

    /// Apply the sound settings to the audio worker.
    ///
    /// The volume, the mute flag **and the voice** live in the config,
    /// and the config reloads live, so the sound follows the file.
    fn refresh_the_sound(&mut self) {
        self.audio.set_volume(self.cfg.audio.volume);
        self.audio.set_muted(self.cfg.audio.muted);
        let waveform = crate::audio::synth::waveform_from_name(&self.cfg.audio.waveform)
            .unwrap_or(crate::audio::synth::Waveform::Triangle);
        self.audio.set_waveform(waveform);
    }

    /// Re-resolve the monitor after the user changed the monitor choice.
    fn re_resolve_monitor(&mut self) {
        let target = match crate::cli::resolve_popup_target(&self.cfg) {
            Ok(target) => target,
            Err(e) => {
                agnosis_core::trace::trace(&format!(
                    "config.reload: cannot resolve the monitor: {e}"
                ));
                return;
            }
        };
        self.args = target;
        let geometry =
            self.cfg
                .grid_for_monitor(self.args.size.0, self.args.size.1, self.args.scale);
        self.apply_geometry(geometry);
        self.move_the_window();
    }

    /// Move and resize the window for the monitor the user chose.
    ///
    /// The scene is re-laid out for the new monitor's cell size. The
    /// **fullscreen state** comes off first: the window used to stay
    /// borderless-fullscreen on the *old* monitor — winit keeps the
    /// monitor it snapped to, and a resize cannot move a fullscreen
    /// window — so a live `[monitor]` change moved the scene's geometry
    /// but the parade never left the old screen
    /// (audit 2026-10-23 §F-091). The re-snap needs the
    /// `ActiveEventLoop` (it enumerates the monitors), which this method
    /// does not have, so the target is parked in
    /// [`Self::pending_fullscreen`] and the event loop applies it.
    fn move_the_window(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        window.set_fullscreen(None);
        let size = PhysicalSize::new(self.args.size.0, self.args.size.1);
        let position = PhysicalPosition::new(self.args.origin.0, self.args.origin.1);
        // The window is borderless, so the inner size is the monitor's
        // size: `request_inner_size` asks the window manager for it (it
        // is a hint, and it is answered asynchronously).
        let _ = window.request_inner_size(size);
        window.set_outer_position(position);
        self.pending_fullscreen = Some(self.args.origin);
    }

    /// Re-snap the window borderless-fullscreen around a parked target
    /// ([`Self::move_the_window`]).
    fn apply_pending_fullscreen(&mut self, event_loop: &ActiveEventLoop) {
        let Some(origin) = self.pending_fullscreen.take() else {
            return;
        };
        let Some(window) = self.window.as_ref() else {
            return;
        };
        snap_to_fullscreen(event_loop, window, origin);
    }

    /// Recompute the conversion radii after a reload.
    ///
    /// The publish itself is [`Self::apply_radii`]'s — one implementation
    /// of "new radii reach the watcher" (audit 2026-10-23 §F-113).
    fn refresh_radii(&mut self) {
        let (_, _, cell_w, cell_h) = self.geometry;
        let radii = balloon_radii(&self.cfg, cell_w, cell_h);
        self.apply_radii(radii);
    }

    /// Refresh the "days since the ducks" counter.
    fn refresh_counter(&mut self) {
        if !self.cfg.stats.show_days {
            self.set_counter(None);
            return;
        }
        let text = self.counter_text();
        self.set_counter(Some(text));
    }

    /// The counter's text: the days since the first parade.
    fn counter_text(&self) -> String {
        let days = self
            .stats
            .as_ref()
            .map(|stats| stats.days_since_first(&crate::host::today()))
            .unwrap_or(0);
        if days == 1 {
            "DAY 1".to_string()
        } else {
            format!("DAYS {days}")
        }
    }

    /// Hand the counter line to the scene.
    fn set_counter(&mut self, text: Option<String>) {
        if let Some(scene) = self.scene.as_mut() {
            scene.set_counter_line(text);
        }
    }

    /// Copy the current frame to the clipboard.
    ///
    /// The frame is the app's **own** framebuffer, not a screen grab: the
    /// window is borderless and fullscreen, so a grab would be the same
    /// picture, but the app's buffer is exact and needs no
    /// screen-capture permission.
    ///
    /// The PNG is also written next to the config as `last_shot.png`, so
    /// the user has the file as well as the clipboard.
    fn copy_the_frame(&mut self) {
        // The frame is the renderer's **own staging buffer** — the frame
        // the user is looking at, never a fresh render — and the encode
        // and the clipboard write happen on the worker, so the parade
        // never hiccups on F12 (audit 2026-10-23 §F-095). The
        // confirmation comes back through [`Self::drain_shot_outcomes`].
        let frame = match self.shot_frame() {
            Ok(fb) => fb,
            Err(e) => {
                self.set_feedback(&format!("COPY FAILED: {e}"));
                return;
            }
        };
        let path = match crate::paths::config_dir() {
            Ok(dir) => dir.join("last_shot.png"),
            Err(e) => {
                self.set_feedback(&format!("COPY FAILED: {e}"));
                return;
            }
        };
        if self
            .shots
            .send(crate::shot_worker::ShotJob { fb: frame, path })
        {
            self.set_feedback("COPYING");
        } else {
            self.set_feedback("COPY FAILED: the shot worker is gone");
        }
    }

    /// The frame F12 copies.
    ///
    /// With a renderer this is [`crate::render::Renderer::last_frame`] —
    /// the staging buffer of the last presented frame, so a skipped
    /// frame (a timeout, an occluded surface) copies what is on screen
    /// and a half-blit is impossible. Without one (the degraded,
    /// GPU-less window) the frame is blitted from the scene's grid, the
    /// same headless path the selftest gate uses.
    fn shot_frame(&mut self) -> Result<agnosis_core::blit::FrameBuffer, String> {
        if let Gpu::Ready(renderer) = &self.gpu {
            return Ok(renderer.last_frame());
        }
        self.render_the_frame()
    }

    /// Report the shots that finished: one confirmation each.
    fn drain_shot_outcomes(&mut self) {
        // The outcomes are collected first: the channel borrow must be
        // over before the feedback can take `&mut self`.
        let outcomes: Vec<Result<(), String>> = self.shots.drain().collect();
        for outcome in outcomes {
            match outcome {
                Ok(()) => self.set_feedback("COPIED"),
                Err(e) => self.set_feedback(&format!("COPY FAILED: {e}")),
            }
        }
    }

    /// Render the scene into a framebuffer and copy it to the clipboard.
    fn render_the_frame(&mut self) -> Result<agnosis_core::blit::FrameBuffer, String> {
        let scene = self.scene.as_ref().ok_or("no scene")?;
        let font = self.font.as_mut().ok_or("no font")?;
        let grid = scene.grid();
        let mut fb = agnosis_core::blit::FrameBuffer::new(
            grid.cols() as u32 * grid.cell_w(),
            grid.rows() as u32 * grid.cell_h(),
        );
        agnosis_core::selftest::blit_scene_into(scene, font, &mut fb);
        Ok(fb)
    }

    /// Show a status message on the input line for the ordinary lifetime.
    fn set_feedback(&mut self, message: &str) {
        self.set_feedback_for(message, FEEDBACK_LIFETIME);
    }

    /// Show a status message on the input line for `lifetime`.
    ///
    /// The line is **dirty**: the message is drawn on it, so the line
    /// must be redrawn. Without this the F12 "COPIED" confirmation
    /// was set and then never shown (audit 2026-10-22 §UX-16).
    fn set_feedback_for(&mut self, message: &str, lifetime: Duration) {
        self.feedback = Some((message.to_string(), Instant::now(), lifetime));
        self.line_dirty = true;
    }

    /// Set the input line text and advance the simulation one step.
    fn step_simulation(&mut self, dt: f32) {
        // The line is built (and allocated) only when it changed.
        let pending = self.pending_line();
        let scene = match self.scene.as_mut() {
            Some(scene) => scene,
            None => return,
        };
        if let Some(text) = pending {
            scene.set_input_line(Some(text));
        }
        scene.advance(dt);
    }

    /// Hand the scene's audio events to the sound worker.
    ///
    /// The scene is the single owner of the simulation and it emits the
    /// events (the strip's notes, the cluck, the thunder, the splosh); the
    /// audio side is a pure consumer of them. The queue is bounded and
    /// drops its oldest event when it overflows, so a slow sound card can
    /// never stall the parade (roadmap Extension 3 §2.2).
    fn pump_the_audio(&mut self) {
        let events = match self.scene.as_mut() {
            Some(scene) => scene.take_audio_events(),
            None => return,
        };
        for event in events {
            self.audio.push(event);
        }
    }

    /// A click on the parade: the duck under the pointer reacts.
    ///
    /// A plain click makes it **jump** — it stays in the train and keeps
    /// marching. A **Shift-click** holds it in place, and the ducks behind
    /// it stop too (the hold model in `parade.rs`). Clicking empty ground
    /// does nothing at all (roadmap Extension 3 §5.4).
    fn on_click(&mut self) {
        let Some((col, row)) = self.pointer_cell() else {
            return;
        };
        let kind = self.click_reaction_kind();
        let Some(scene) = self.scene.as_mut() else {
            return;
        };
        let Some(slot) = scene.duck_at(col, row) else {
            return;
        };
        scene.react(slot, kind);
    }

    /// The reaction a click asks for: a jump, or a stop when Shift is held.
    ///
    /// The **default** does not disturb the parade: a train that stopped
    /// every time the user moved the mouse would be a broken parade
    /// (roadmap Extension 3 §5.4).
    fn click_reaction_kind(&self) -> agnosis_core::reaction::ReactionKind {
        let stopping = self
            .modifiers
            .contains(winit::keyboard::ModifiersState::SHIFT);
        if stopping {
            agnosis_core::reaction::ReactionKind::Stop
        } else {
            agnosis_core::reaction::ReactionKind::Jump
        }
    }

    /// The grid cell under the pointer.
    ///
    /// The popup's window is borderless and fullscreen on the parade's
    /// monitor, so the pointer's pixel position maps straight onto the
    /// grid with no origin correction — the same mapping the editor uses.
    fn pointer_cell(&self) -> Option<(i64, i64)> {
        let (_, _, cell_w, cell_h) = self.geometry;
        if cell_w == 0 || cell_h == 0 {
            return None;
        }
        let col = self.pointer.0 / i64::from(cell_w);
        let row = self.pointer.1 / i64::from(cell_h);
        Some((col, row))
    }

    /// Blit the scene through the renderer; missing pieces are a no-op.
    fn draw_frame(&mut self) {
        let Gpu::Ready(renderer) = &mut self.gpu else {
            return;
        };
        let font = match self.font.as_mut() {
            Some(font) => font,
            None => return,
        };
        let scene = match self.scene.as_ref() {
            Some(scene) => scene,
            None => return,
        };
        let offset = scene.sub_cell_offset_px();
        let ground = scene.ground_row() as usize;
        let sky = scene.sky_ramp();
        let layers = scene.backdrop_blits();
        // The message's band is the **overlay**: it is chrome, so it is
        // drawn over the scene, at offset 0 (audit 2026-10-21 §E7).
        let overlay = scene.message_blit();
        match renderer.draw(
            scene.grid(),
            font,
            offset,
            ground,
            sky,
            &layers,
            overlay.as_ref(),
        ) {
            Ok(()) => self.mark_first_frame(),
            Err(e) => agnosis_core::trace::trace(&format!("popup.draw error: {e}")),
        }
    }

    /// First successful draw: trace once for the startup smoke log.
    fn mark_first_frame(&mut self) {
        if self.first_frame {
            return;
        }
        self.first_frame = true;
        agnosis_core::trace::trace("popup.first-frame");
    }

    /// The text rendered on the input line: the status message while fresh,
    /// the usage hint while the buffer is empty, otherwise the prompt with
    /// the live buffer and cursor.
    ///
    /// The text is built only when it changed (typed text, a new status
    /// message, or the status message expiring) and handed to the scene by
    /// value, so the per-frame path neither formats nor allocates a line
    /// (audit P0-1).
    fn pending_line(&mut self) -> Option<String> {
        self.drop_expired_feedback();
        if !self.line_dirty {
            return None;
        }
        self.line_dirty = false;
        Some(self.compose_input_line())
    }

    /// Forget a status message that has shown for its own lifetime: the
    /// prompt takes over again.
    fn drop_expired_feedback(&mut self) {
        let expired = match &self.feedback {
            Some((_, at, lifetime)) => at.elapsed() >= *lifetime,
            None => false,
        };
        if !expired {
            return;
        }
        self.feedback = None;
        self.line_dirty = true;
    }

    /// Compose the input line text: the live buffer first, then the
    /// status message while fresh, otherwise the usage hint.
    ///
    /// The buffer wins over fresh feedback: during "LOADING X" the
    /// user's next typing used to be invisible, and Enter committed a
    /// buffer they never saw (audit 101126 §B7).
    fn compose_input_line(&self) -> String {
        if !self.input.is_empty() {
            return format!("ADD BALLOON: {}_", self.input);
        }
        if let Some((msg, _, _)) = &self.feedback {
            return msg.clone();
        }
        INPUT_HINT.to_string()
    }

    /// Enter on the input line: hand the typed name to the loader and let
    /// the worker resolve, convert and report back.
    ///
    /// The resolution is a recursive folder scan plus a full image decode,
    /// which is far too slow for the UI thread (audit 2026-10-20 §3.2):
    /// doing it inline froze the parade and left ESC unprocessed. The
    /// worker does the work; [`Self::drain_loader_into_scene`] picks the
    /// result up.
    fn commit_input(&mut self) {
        let name = self.input.trim().to_string();
        self.input.clear();
        self.line_dirty = true;
        if name.is_empty() {
            self.set_feedback("TYPE A NAME FIRST");
            return;
        }
        let dir = balloon_folder();
        let (rx, ry) = self.balloon_radii;
        let Some(loader) = self.loader.as_mut() else {
            self.set_feedback("THE BALLOON LOADER IS NOT RUNNING");
            return;
        };
        match loader.request(&dir, &name, rx, ry) {
            Ok(()) => self.set_feedback(&format!("LOADING {}", name.to_uppercase())),
            Err(LoadReject::InFlight) => {
                self.set_feedback(&format!("ALREADY LOADING {}", name.to_uppercase()))
            }
            Err(LoadReject::WorkerGone) => self.set_feedback("THE BALLOON LOADER IS NOT RUNNING"),
        }
    }

    /// Drain the loader's results into the scene's ring.
    ///
    /// A channel poll and nothing more: the resolve and the decode happen
    /// on the loader's worker thread, so the UI thread never blocks
    /// (audit 2026-10-20 §3.2). An answer made for **other radii** is
    /// stale — a geometry change happened between the request and the
    /// answer — and is dropped instead of drawn distorted
    /// (audit 2026-10-23 §F-117).
    fn drain_loader_into_scene(&mut self) {
        let results = match self.loader.as_mut() {
            Some(loader) => loader.drain(),
            None => return,
        };
        let messages: Vec<(Option<agnosis_core::balloon::AsciiSprite>, String)> = results
            .into_iter()
            .filter(|result| {
                crate::stale_stamp::answer_is_current(&result.radii, &self.balloon_radii)
            })
            .map(|result| match result.sprite {
                Ok(sprite) => (
                    Some(sprite),
                    format!("ADDED {}", result.name.to_uppercase()),
                ),
                Err(e) => (None, e),
            })
            .collect();
        for (sprite, message) in messages {
            self.add_balloon_and_message(sprite, &message);
        }
    }

    /// Resize handler (audit §P0-4): the renderer follows the window, and
    /// so does the *geometry*: the grid cell size comes from the row-budget
    /// rule, so a different window size means a different cell size. The
    /// scene (which owns the grid) is resized onto the new grid and the
    /// font is rebuilt from the same cell dims — a mismatch puts the
    /// glyphs off-frame (AGENTS.md) — and the watcher's conversion radii
    /// follow the same geometry, so a balloon converted after the change
    /// matches the disc it is drawn on.
    fn on_resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if let Gpu::Ready(renderer) = &mut self.gpu {
            renderer.resize(width, height);
        }
        let geometry = self.cfg.grid_for_monitor(width, height, self.args.scale);
        self.apply_geometry(geometry);
        let (_, _, cell_w, cell_h) = geometry;
        let radii = balloon_radii(&self.cfg, cell_w, cell_h);
        self.apply_radii(radii);
    }

    /// Rebuild the grid-dependent state for a new geometry.
    ///
    /// The scene owns the grid, so a new cell size means a new GRID — but
    /// not a new SIMULATION: an existing scene is resized in place, so the
    /// parade phase, the pendulums, the ropes, the balloon ring and the
    /// clock survive the resize (a rebuild restarted the train at the left
    /// edge and dropped the balloons). The font is rebuilt with the *same*
    /// cell dims, otherwise the glyph masks no longer match the cell pitch
    /// and the frame goes black (AGENTS.md). The input line is re-applied
    /// (the resized scene keeps it, but the popup owns the text).
    fn apply_geometry(&mut self, geometry: (usize, usize, u32, u32)) {
        if geometry == self.geometry {
            return;
        }
        self.geometry = geometry;
        let (cols, rows, cell_w, cell_h) = geometry;
        let mut fresh = false;
        if let Some(scene) = self.scene.as_mut() {
            scene.resize(cols, rows, cell_w, cell_h);
        } else {
            // Same discipline as the startup scene: empty folders first,
            // the drawn art arrives through the worker (§F-096).
            self.scene = Some(agnosis_core::scene::Scene::new_with_assets(
                &self.cfg,
                cols,
                rows,
                cell_w,
                cell_h,
                &agnosis_core::assets::AssetDirs::default(),
            ));
            let cfg = self.cfg.clone();
            self.request_the_assets(&cfg);
            fresh = true;
        }
        self.font = match agnosis_core::font::Font::new(cell_w, cell_h) {
            Ok(font) => Some(font),
            Err(e) => {
                agnosis_core::trace::trace(&format!("popup.font error: {e}"));
                None
            }
        };
        self.line_dirty = true;
        if fresh {
            // The backdrop tiles are built by the worker; the scene keeps
            // the ones it already has across a resize.
            self.request_backdrops();
        }
    }

    /// Publish new conversion radii to the watcher (and the typed-name
    /// convert path); unchanged values stay silent.
    fn apply_radii(&mut self, radii: (i64, i64)) {
        if radii == self.balloon_radii {
            return;
        }
        self.balloon_radii = radii;
        if let Some(watcher) = self.watcher.as_mut() {
            watcher.set_radii(radii.0, radii.1);
        }
        agnosis_core::trace::trace(&format!("popup.radii rx={} ry={}", radii.0, radii.1));
    }

    /// Poll the GPU init thread; a failed or vanished init degrades to the
    /// black-window scene-advance loop instead of panicking (audit §poll_gpu).
    fn poll_gpu(&mut self) {
        let Gpu::Awaiting(receiver) = &self.gpu else {
            return;
        };
        let init = match try_recv_init(Some(receiver)) {
            GpuPoll::Ready(init) => init,
            GpuPoll::Pending => return,
            GpuPoll::Gone => {
                self.degraded("popup.gpu init unavailable, degraded");
                return;
            }
        };
        // The channel is spent: the machine moves on to Ready or Dead
        // and never awaits the same init twice (audit 101126 §S3).
        self.gpu = Gpu::Dead;
        agnosis_core::trace::trace("popup.device-ready");
        let window = match self.window.as_ref() {
            Some(w) => Arc::clone(w),
            None => {
                self.degraded("popup.gpu window gone, degraded");
                return;
            }
        };
        let surface = match init.instance.create_surface(Arc::clone(&window)) {
            Ok(s) => s,
            Err(e) => {
                self.degraded(&format!("popup.surface error, degraded: {e}"));
                return;
            }
        };
        // The surface follows the **window's live size**, not the rect
        // the command line carried: a resize that landed between the
        // spawn and the device arriving used to build the surface at the
        // stale size, and the first frames were letterboxed
        // (audit 2026-10-23 §F-110).
        let inner = window.inner_size();
        let (width, height) = (inner.width.max(1), inner.height.max(1));
        let config =
            match crate::render::pick_surface_config(&surface, &init.adapter, width, height) {
                Some(c) => c,
                None => {
                    self.degraded("popup.surface no-compatible-format, degraded");
                    return;
                }
            };
        match crate::render::Renderer::new(init.device, init.queue, surface, config, width, height)
        {
            Ok(renderer) => {
                agnosis_core::trace::trace("popup.renderer-ready");
                self.gpu = Gpu::Ready(Box::new(renderer));
            }
            Err(e) => self.degraded(&format!("popup.renderer error, degraded: {e}")),
        }
    }

    /// Drop GPU init and keep the black window advancing the scene.
    ///
    /// The degradation is **honest** (audit 2026-10-23 §F-092): the
    /// window's title says so (the OS-level escape route: Alt+F4, the
    /// task bar), the input line carries the same word for the user who
    /// is looking at the window, and the message box (or the stderr
    /// line) spells the whole story out.
    fn degraded(&mut self, msg: &str) {
        agnosis_core::trace::trace(msg);
        let first = !matches!(self.gpu, Gpu::Dead);
        self.gpu = Gpu::Dead;
        if let Some(window) = self.window.as_ref() {
            window.set_title("AGnosis - GPU UNAVAILABLE");
        }
        self.set_feedback("NO GPU - PRESS ESC TO EXIT");
        // A modal box inside a test would hang the whole suite — the
        // tests assert on the machine, never on the box.
        if first && !cfg!(test) {
            announce_gpu_failure(msg);
        }
    }

    /// ESC / window close: mark exit and leave the event loop.
    fn quit(&mut self, event_loop: &ActiveEventLoop) {
        // The sound's state is reported once, at the end: the device may
        // have opened long after the parade started.
        agnosis_core::trace::trace(&format!(
            "popup.audio: device={}",
            if self.audio.has_device() { "yes" } else { "no" }
        ));
        self.exit = true;
        event_loop.exit();
    }

    /// One pressed key for the Fix-5 input line: ESC exits, `Ctrl+O`
    /// opens the folder, everything else edits/commits.
    fn on_keyboard_input(&mut self, event_loop: &ActiveEventLoop, event: winit::event::KeyEvent) {
        if event.state != ElementState::Pressed {
            return;
        }
        // Esc always exits first (audit §5).
        if matches!(event.logical_key, Key::Named(NamedKey::Escape)) {
            self.quit(event_loop);
            return;
        }
        if self.maybe_open_folder(&event) {
            return;
        }
        // The screenshot chord: it copies the live frame to the
        // clipboard. It is a **key**, not a button: the popup has no
        // mouse handling, and the input line is its only chrome.
        if matches!(event.logical_key, Key::Named(NamedKey::F12)) {
            self.copy_the_frame();
            return;
        }
        // The HELP chord: the full README, scrollable, in a window of
        // its own (roadmap HELP_WINDOW §1). It is a **spawn**, not a
        // layer over the parade — the parade keeps marching, frame rate
        // untouched, while the user reads.
        if matches!(event.logical_key, Key::Named(NamedKey::F1)) {
            self.open_help();
            return;
        }
        // The EDITOR chord: the draw-your-own editor, the same detached
        // spawn shape as HELP. The daemon suppresses its own editor chord
        // while the parade is open, so this is the parade's own door.
        if matches!(event.logical_key, Key::Named(NamedKey::F2)) {
            self.open_editor();
            return;
        }
        let named = match &event.logical_key {
            Key::Named(named) => Some(named),
            _ => None,
        };
        // `Edit` means the buffer changed, `None` means the key was dead:
        // only an edit invalidates the cached input line (audit D6).
        match handle_input_key(&mut self.input, named, event.text.as_deref()) {
            InputAction::Edit => self.line_dirty = true,
            InputAction::Commit => self.commit_input(),
            InputAction::None => {}
        }
    }

    /// F1 opens the in-app HELP window: the full README, scrollable
    /// (roadmap HELP_WINDOW §1).
    ///
    /// The window is a **separate process** with its own window class,
    /// exactly like the editor: the parade keeps marching — frame rate
    /// untouched — while the user reads, and the help window closes with
    /// its own ESC. The feedback line confirms the spawn; the read of
    /// the README happens inside the spawned process, at its own open
    /// time. A reader that is already open is reported, not duplicated.
    fn open_help(&mut self) {
        let msg = match spawn_detached_help(self.args.origin, self.args.size) {
            Ok(msg) => {
                // The reader is an ordinary window and the parade is
                // always-on-top, so the reader would open entirely behind
                // it — and the next F1 would answer HELP ALREADY OPEN for
                // a window the user has never seen. The parade therefore
                // steps aside for as long as the reader lives.
                self.step_aside_for(Overlay::Help);
                msg.to_string()
            }
            Err(e) => e,
        };
        self.set_feedback(&msg);
    }

    /// F2 opens the draw-your-own editor: a separate process with its own
    /// window class, exactly like F1 and the HELP window.
    ///
    /// WHY the parade spawns it even though the daemon owns an editor
    /// chord: the daemon **suppresses** that chord while the parade is
    /// open (audit 2026-10-23 §F-093), so a parade running on its own
    /// (a double-click, no daemon) had no door to the editor at all. The
    /// parade steps out of always-on-top for the editor — the same dance
    /// as the HELP window — and re-promotes when it closes.
    fn open_editor(&mut self) {
        let msg = match spawn_detached_editor(self.args.origin, self.args.size) {
            Ok(msg) => {
                self.step_aside_for(Overlay::Editor);
                msg.to_string()
            }
            Err(e) => e,
        };
        self.set_feedback(&msg);
    }

    /// Leave always-on-top so a spawned overlay window (HELP, the editor)
    /// can be read over the parade, and start watching for its close.
    ///
    /// The parade keeps marching — only the z-order changes — and
    /// [`Self::poll_overlay_watch`] restores the level when the last
    /// overlay closes.
    fn step_aside_for(&mut self, which: Overlay) {
        let already_watching = self.overlay_watch.is_some();
        let watch = self.overlay_watch.get_or_insert(OverlayWatch {
            help: false,
            editor: false,
            since: Instant::now(),
        });
        match which {
            Overlay::Help => watch.help = true,
            Overlay::Editor => watch.editor = true,
        }
        if already_watching {
            return;
        }
        let Some(window) = self.window.as_ref() else {
            return;
        };
        window.set_window_level(WindowLevel::Normal);
    }

    /// Re-assert always-on-top once the last overlay window is gone.
    ///
    /// No focus ladder here: the parade must never steal the foreground
    /// back from whatever the user went on to read.
    fn restore_always_on_top(&mut self) {
        let Some(window) = self.window.as_ref() else {
            return;
        };
        window.set_window_level(WindowLevel::AlwaysOnTop);
    }

    /// The overlay watch: one probe round per [`OVERLAY_PROBE_INTERVAL`]
    /// while demoted; the first round that finds no overlay re-promotes.
    ///
    /// A spawn whose child died before it built its window self-heals
    /// here: the probe simply finds nothing and restores the level.
    fn poll_overlay_watch(&mut self) {
        let Some(watch) = self.overlay_watch else {
            return;
        };
        let now = Instant::now();
        if !overlay_probe_due(watch.since, now) {
            return;
        }
        // The fold is a pure function, so the demote/re-promote machine
        // is testable without a window (audit 101126 §S8).
        self.overlay_watch = fold_overlay_watch(
            &watch,
            crate::platform::window::help_is_open(),
            crate::platform::window::editor_is_open(),
        );
        if self.overlay_watch.is_none() {
            self.restore_always_on_top();
        }
    }

    /// `Ctrl+O` opens the balloons folder instead of typing the letter.
    ///
    /// WHY a chord and not a bare `O`: a bare letter shortcut makes every
    /// name that starts with "o" untypeable — the key was swallowed
    /// before it reached the buffer, so "orange" opened the folder and
    /// left the buffer empty (audit 2026-10-20 §3.4). A modifier can
    /// never collide with a name.
    ///
    /// The modifier state comes from the `ModifiersChanged` event, not
    /// from the key event: winit delivers the two separately.
    ///
    /// True when the folder was opened.
    fn maybe_open_folder(&mut self, event: &winit::event::KeyEvent) -> bool {
        if !self
            .modifiers
            .contains(winit::keyboard::ModifiersState::CONTROL)
        {
            return false;
        }
        let Key::Character(ch) = &event.logical_key else {
            return false;
        };
        if !ch.eq_ignore_ascii_case("o") {
            return false;
        }
        let dir = balloon_folder();
        let msg = open_balloon_folder(&dir)
            .map_or_else(|e| e, |_| "OPENED THE BALLOONS FOLDER".to_string());
        self.set_feedback(&msg);
        true
    }
    /// Start the live config watcher.
    ///
    /// The watcher is a nicety: when it cannot be created the parade
    /// simply does not reload, and everything else keeps working.
    fn start_config_watcher(&mut self) {
        let path = match crate::paths::config_file() {
            Ok(path) => path,
            Err(e) => {
                agnosis_core::trace::trace(&format!("popup.config-watch error: {e}"));
                return;
            }
        };
        match crate::config_watch::ConfigWatcher::new(&path) {
            Ok(watcher) => self.config_watcher = Some(watcher),
            Err(e) => agnosis_core::trace::trace(&format!("popup.config-watch error: {e}")),
        }
    }

    /// Read the counters, record this parade, and show the counter.
    ///
    /// The stat file is read **once** per parade, never on the UI
    /// thread's per-frame path.
    fn start_stats(&mut self) {
        let dir = match crate::paths::config_dir() {
            Ok(dir) => dir,
            Err(e) => {
                agnosis_core::trace::trace(&format!("popup.stats error: {e}"));
                return;
            }
        };
        let path = dir.join(agnosis_core::stats::STATS_FILE);
        let defs =
            agnosis_core::stats::definitions(&dir.join(agnosis_core::stats::ACHIEVEMENTS_FILE));
        let mut stats = agnosis_core::stats::Stats::load(&path);
        let earned = stats.record_parade(&crate::host::today(), &defs);
        for name in earned {
            agnosis_core::trace::trace(&format!("popup.stats earned {name}"));
            // The counter is off by default and the trace file is not
            // something a user reads, so an achievement earned **in
            // silence** was an achievement that never happened
            // (audit 2026-10-23 §F-122). The input line says so; when
            // several land in one parade the last one is the one shown.
            self.set_feedback(&format!("ACHIEVEMENT - {}", name.to_uppercase()));
        }
        if let Err(e) = stats.save(&path) {
            agnosis_core::trace::trace(&format!("popup.stats error: {e}"));
        }
        self.stats = Some(stats);
        self.refresh_counter();
    }
}

/// Read, parse and repair the config file for a live reload.
///
/// `None` when the file cannot be read or does not parse: the caller
/// keeps the old values and the parade carries on — a malformed config
/// must **never** take the parade down.
///
/// Every applied value goes through the same clamping the repair pass
/// applies at load time, so a hand-edited `balloon_radius_rows = 0`
/// cannot divide anything by zero.
///
/// The drawn-asset existence check is skipped: it would scan the user's
/// folders on the UI thread. A missing asset is not fatal — the scene
/// falls back to the built-in art.
fn read_reload_config(path: &std::path::Path) -> Option<agnosis_core::config::Config> {
    let text = std::fs::read_to_string(path).ok()?;
    let parsed: agnosis_core::config::Config = toml::from_str(&text).ok()?;
    let mut repairs = Vec::new();
    Some(parsed.sanitized_with_reporting(|_, _| true, &mut repairs))
}

/// What one watcher outcome becomes: the sprite to push (if any) and
/// the input-line message. Free so the drain stays flat and the message
/// shape is pinned by a test — a dropped image is no longer silent
/// (audit 2026-10-23 §F-104).
fn watcher_message(
    outcome: crate::watcher::WatchOutcome,
) -> (Option<agnosis_core::balloon::AsciiSprite>, String) {
    match outcome {
        crate::watcher::WatchOutcome::Added { name, sprite } => {
            (Some(sprite), format!("ADDED {}", name.to_uppercase()))
        }
        crate::watcher::WatchOutcome::Skipped { file, reason } => (
            None,
            format!("CANNOT LOAD {}: {reason}", file.to_uppercase()),
        ),
    }
}

/// Outcome of one non-blocking GPU-init poll.
///
/// The outcome and the poll live in [`crate::render`], where both
/// windows can reach them: one definition, two callers (audit
/// 2026-10-21 §G26).
use crate::render::{try_recv_init, Gpu, GpuPoll};

impl ApplicationHandler for PopupApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let size = PhysicalSize::new(self.args.size.0, self.args.size.1);
        let position = PhysicalPosition::new(self.args.origin.0, self.args.origin.1);
        let attrs = Window::default_attributes()
            // The title is the **user's** escape route (audit 2026-10-23
            // §F-103): a borderless window has no title bar, but the task
            // bar, the alt-tab list and the OS all show the name — the
            // OS-level way to know which black window is what.
            .with_title(crate::platform::POPUP_WINDOW_TITLE)
            .with_inner_size(size)
            .with_position(position)
            .with_decorations(false)
            .with_resizable(false)
            .with_enabled_buttons(WindowButtons::empty())
            .with_transparent(false)
            .with_visible(false)
            .with_active(true)
            .with_window_level(WindowLevel::AlwaysOnTop);
        #[cfg(target_os = "windows")]
        let attrs = attrs.with_class_name(crate::platform::POPUP_WINDOW_CLASS);
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                agnosis_core::trace::trace(&format!("popup.window error: {e}"));
                self.exit = true;
                event_loop.exit();
                return;
            }
        };
        agnosis_core::trace::trace("popup.window-created");
        if let Err(e) = crate::platform::window::paint_black_before_show(&window) {
            agnosis_core::trace::trace(&format!("popup.paint-black error: {e}"));
        }
        // Belt-and-braces with the CLI-resolved rect: snap to borderless
        // fullscreen on the monitor winit itself reports at our origin, so
        // the window covers the display edge-to-edge even if the resolved
        // rect was off by a few pixels (audit 2026-10-06 §1).
        snap_to_fullscreen(event_loop, &window, self.args.origin);
        window.set_visible(true);
        agnosis_core::trace::trace("popup.shown-black");
        window.set_decorations(false);
        window.set_resizable(false);
        crate::platform::window::ensure_borderless(&window);
        agnosis_core::trace::trace("popup.borderless-enforced");
        crate::platform::window::focus_ladder(&window, true);
        if let Err(e) = crate::platform::window::write_pid_file() {
            agnosis_core::trace::trace(&format!("popup.pid error: {e}"));
        }
        let (cols, rows, cell_w, cell_h) =
            self.cfg
                .grid_for_monitor(self.args.size.0, self.args.size.1, self.args.scale);
        self.geometry = (cols, rows, cell_w, cell_h);
        // The first scene is built with **empty** asset folders: it shows
        // the built-in art, and the drawn assets the config names are
        // parsed by the **worker** and applied exactly like a live
        // reload. Startup used to parse up to nineteen JSON documents of
        // hundreds of kilobytes each on the UI thread, before the first
        // frame (audit 2026-10-23 §F-096).
        self.scene = Some(agnosis_core::scene::Scene::new_with_assets(
            &self.cfg,
            cols,
            rows,
            cell_w,
            cell_h,
            &agnosis_core::assets::AssetDirs::default(),
        ));
        let cfg = self.cfg.clone();
        self.request_the_assets(&cfg);
        self.request_backdrops();
        match agnosis_core::font::Font::new(cell_w, cell_h) {
            Ok(font) => self.font = Some(font),
            Err(e) => {
                agnosis_core::trace::trace(&format!("popup.font error: {e}"));
            }
        }
        let balloon_dir = balloon_folder();
        let radii = balloon_radii(&self.cfg, cell_w, cell_h);
        self.balloon_radii = radii;
        match crate::watcher::BalloonWatcher::new(&balloon_dir, radii.0, radii.1) {
            Ok(watcher) => self.watcher = Some(watcher),
            Err(e) => {
                agnosis_core::trace::trace(&format!("popup.watcher error: {e}"));
            }
        }
        self.start_config_watcher();
        self.start_stats();
        // Onboarding: the global hotkey is the **daemon's** keyboard hook.
        // When the daemon's stop endpoint is not armed, this parade was
        // started on its own — a double-click — and the browser
        // minimization (the app's whole premise) silently never happens.
        // Say so once, for the onboarding lifetime. A daemon-spawned
        // parade finds the endpoint armed and stays quiet.
        if !crate::platform::daemon_ctl::endpoint_armed() {
            self.set_feedback_for(NO_DAEMON_HINT, ONBOARDING_LIFETIME);
        }
        self.window = Some(window);
        self.last_tick = Instant::now();
        self.next_frame = self.last_tick;
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
            WindowEvent::ModifiersChanged(modifiers) => {
                self.modifiers = modifiers.state();
            }
            WindowEvent::KeyboardInput { event, .. } => self.on_keyboard_input(event_loop, event),
            WindowEvent::CursorMoved { position, .. } => {
                self.pointer = (position.x as i64, position.y as i64);
            }
            WindowEvent::MouseInput { state, button, .. } => {
                if button == MouseButton::Left && state == ElementState::Pressed {
                    self.on_click();
                }
            }
            WindowEvent::RedrawRequested => {
                self.apply_pending_fullscreen(event_loop);
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
    /// ever sits inside another `if`. The order is unchanged — the tick
    /// runs first, and the next frame is only scheduled when the tick did
    /// not raise `exit`.
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.exit {
            event_loop.exit();
            return;
        }
        if self.window.is_none() {
            return;
        }
        self.apply_pending_fullscreen(event_loop);
        self.tick();
        if self.exit {
            return;
        }
        event_loop.set_control_flow(ControlFlow::WaitUntil(self.next_frame));
    }
}

/// Run the popup until ESC / window close / OS signal. Returns Ok on clean exit.
pub fn run(cfg: &agnosis_core::config::Config, args: &PopupArgs) -> Result<(), PopupError> {
    agnosis_core::trace::trace("popup.spawn");
    validate_args(args)?;
    let event_loop =
        EventLoop::new().map_err(|e| PopupError::Window(format!("event loop: {e}")))?;
    let gpu_rx = crate::render::spawn_device_thread();
    let now = Instant::now();
    let mut app = PopupApp {
        cfg: cfg.clone(),
        args: *args,
        window: None,
        scene: None,
        font: None,
        watcher: None,
        loader: BalloonLoader::start(),
        gpu: Gpu::Awaiting(gpu_rx),
        first_frame: false,
        last_tick: now,
        next_frame: now,
        exit: false,
        input: String::new(),
        feedback: None,
        overlay_watch: None,
        balloon_radii: (2, 1),
        geometry: (0, 0, 0, 0),
        pending_fullscreen: None,
        line_dirty: true,
        modifiers: winit::keyboard::ModifiersState::empty(),
        config_watcher: None,
        backdrops: crate::backdrop_loader::BackdropLoader::start(),
        asset_loader: crate::asset_loader::AssetLoader::start(),
        assets_worker_gone: false,
        shots: crate::shot_worker::ShotWorker::start(),
        stats: None,
        audio: crate::audio::AudioContext::start(&cfg.audio),
        pointer: (0, 0),
    };
    let ran = event_loop.run_app(&mut app);
    // The pid file goes even when the event loop failed: the `?` used to
    // return first and leave a record naming a dead process behind
    // (audit 2026-10-22 §P2-14). The clear only takes our own record.
    crate::platform::window::clear_pid_file();
    ran.map_err(|e| PopupError::Window(format!("event loop: {e}")))?;
    Ok(())
}

/// Tell the user the parade cannot render, and how to leave.
///
/// A GPU failure used to leave a **black fullscreen window with no
/// message**: the exit hint is drawn in the very framebuffer the dead
/// GPU cannot show, and the user's browsers were already minimised
/// behind it (audit 2026-10-22 §UX-13). The native message box needs no
/// GPU, no surface and no font rasteriser.
///
/// The box blocks the event loop until it is dismissed — the window is
/// unusable anyway, and ESC works again the moment it is gone.
#[cfg(target_os = "windows")]
fn announce_gpu_failure(msg: &str) {
    use windows::core::PCWSTR;
    use windows::Win32::UI::WindowsAndMessaging::{
        MessageBoxW, MB_OK, MB_SETFOREGROUND, MB_TOPMOST,
    };
    let text = format!(
        "The parade could not start its renderer:\n{msg}\n\n\
         The window stays black. Press ESC (or Alt+F4) to close it — \
         the browsers it hid come back."
    );
    let title = "AGnosis — the parade cannot render";
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let text = wide(&text);
    let title = wide(title);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_SETFOREGROUND | MB_TOPMOST,
        );
    }
}

/// The non-Windows announcement: a line on stderr.
#[cfg(not(target_os = "windows"))]
fn announce_gpu_failure(msg: &str) {
    eprintln!("agnosis: the parade could not start its renderer: {msg}");
    eprintln!("agnosis: the window stays black. Press ESC to close it.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_overlay_probe_gates_on_its_interval() {
        let now = Instant::now();
        // Freshly armed: no probe is owed yet.
        assert!(!overlay_probe_due(now, now));
        // A full interval later: the probe is due.
        assert!(overlay_probe_due(now - OVERLAY_PROBE_INTERVAL, now));
        // Well past it: still just "due", never a panic or a wrap.
        assert!(overlay_probe_due(now - OVERLAY_PROBE_INTERVAL * 100, now));
    }

    #[test]
    fn the_overlay_watch_folds_one_probe_round() {
        let watch = OverlayWatch {
            help: true,
            editor: false,
            since: Instant::now(),
        };
        // A still-open reader keeps its slot.
        let kept = fold_overlay_watch(&watch, true, false).expect("still watching");
        assert!(kept.help && !kept.editor);
        // The probe never **adds** a slot: the watch holds only the
        // overlays the parade demoted for (audit 101126 §S8).
        let kept_too = fold_overlay_watch(&watch, true, true).expect("still watching");
        assert!(kept_too.help && !kept_too.editor);
        // The only watched reader closed: the watch ends, the caller
        // re-promotes.
        assert!(fold_overlay_watch(&watch, false, false).is_none());
        // Two watched overlays: a closed one drops out of its slot
        // alone, and the last close ends the watch.
        let both = OverlayWatch {
            help: true,
            editor: true,
            since: Instant::now(),
        };
        let alone = fold_overlay_watch(&both, false, true).expect("the editor holds it");
        assert!(!alone.help && alone.editor);
        assert!(fold_overlay_watch(&both, false, false).is_none());
    }

    #[test]
    fn the_onboarding_hint_fits_the_block_font() {
        // The HUD block font carries only A-Z, 0-9, `. : _ - / \` and
        // space — a character outside it would draw as a hole.
        let in_font = NO_DAEMON_HINT.chars().all(|c| {
            matches!(
                c,
                'A'..='Z' | '0'..='9' | ' ' | '.' | ':' | '_' | '-' | '/' | '\\'
            )
        });
        assert!(in_font, "{NO_DAEMON_HINT}");
        // And it names both the problem and the way out.
        assert!(NO_DAEMON_HINT.contains("DAEMON"));
        assert!(NO_DAEMON_HINT.contains("START"));
    }

    /// A temp folder that is not the user's real config folder.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "agnosis-popup-{}-{nanos}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir temp dir");
        dir
    }

    #[test]
    fn a_broken_config_keeps_the_old_values() {
        let dir = temp_dir("broken-config");
        let path = dir.join("config.toml");
        std::fs::write(&path, "loop_seconds = 12.0\n").expect("write");
        let good = read_reload_config(&path).expect("the valid config must load");
        assert_eq!(good.loop_seconds, 12.0);
        // A broken file yields nothing: the caller keeps the old values.
        std::fs::write(&path, "this is not = = toml").expect("write");
        assert!(
            read_reload_config(&path).is_none(),
            "a broken config must be refused"
        );
        // Fixing the file applies it again.
        std::fs::write(&path, "loop_seconds = 20.0\n").expect("write");
        let fixed = read_reload_config(&path).expect("the fixed config must load");
        assert_eq!(fixed.loop_seconds, 20.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_reload_never_panics_on_a_zero_radius() {
        let dir = temp_dir("zero-radius");
        let path = dir.join("config.toml");
        std::fs::write(&path, "[balloons]\nballoon_radius_rows = 0\n").expect("write");
        let cfg = read_reload_config(&path).expect("the config must load");
        assert_eq!(
            cfg.balloons.balloon_radius_rows, 36,
            "the zero radius must be clamped"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn args(w: u32, h: u32) -> PopupArgs {
        PopupArgs {
            origin: (0, 0),
            size: (w, h),
            scale: 1.0,
        }
    }

    #[test]
    fn popup_args_construction_sanity() {
        let a = PopupArgs {
            origin: (10, 20),
            size: (1920, 1080),
            scale: 1.5,
        };
        assert_eq!(a.origin, (10, 20));
        assert_eq!(a.size, (1920, 1080));
        assert_eq!(a.scale, 1.5);
        assert!(validate_args(&a).is_ok());
    }

    #[test]
    fn zero_size_returns_err_without_opening_window() {
        let t = Instant::now();
        let err = run(&agnosis_core::config::Config::default(), &args(0, 1080)).unwrap_err();
        assert!(matches!(err, PopupError::Config(_)), "got {err:?}");
        let err = run(&agnosis_core::config::Config::default(), &args(1920, 0)).unwrap_err();
        assert!(matches!(err, PopupError::Config(_)), "got {err:?}");
        assert!(
            t.elapsed() < Duration::from_secs(5),
            "arg validation must be immediate"
        );
    }

    #[test]
    fn bad_scale_returns_err_without_opening_window() {
        let mut a = args(1920, 1080);
        a.scale = 0.0;
        assert!(matches!(
            run(&agnosis_core::config::Config::default(), &a).unwrap_err(),
            PopupError::Config(_)
        ));
        a.scale = f64::NAN;
        assert!(matches!(
            run(&agnosis_core::config::Config::default(), &a).unwrap_err(),
            PopupError::Config(_)
        ));
    }

    #[test]
    fn frame_interval_matches_fps_cap() {
        assert_eq!(frame_interval(30), Duration::from_nanos(33_333_333));
        assert_eq!(frame_interval(60), Duration::from_nanos(16_666_666));
        assert_eq!(frame_interval(0), Duration::from_nanos(33_333_333));
    }

    #[test]
    fn balloon_radii_are_positive() {
        let (rx, ry) = balloon_radii(&agnosis_core::config::Config::default(), 2, 3);
        assert!(rx >= 2 && ry >= 1);
        assert_eq!((rx, ry), (54, 36), "audit radii at the [2,3] cell");
    }

    #[test]
    fn single_gpu_thread_resolves_without_panic() {
        let rx = crate::render::spawn_device_thread();
        let result = rx.recv_timeout(std::time::Duration::from_secs(25));
        if let Ok(init) = result {
            assert!(!format!("{:?}", init.adapter.get_info().backend).is_empty());
        }
    }

    #[test]
    fn degraded_poll_gpu_marks_dead_without_window() {
        // The machine (audit 101126 §S3): a vanished init degrades the
        // window once — Dead is terminal, so a second poll is a no-op.
        let now = Instant::now();
        let (sender, receiver) = std::sync::mpsc::channel::<crate::render::GpuInit>();
        drop(sender);
        let mut app = PopupApp {
            cfg: agnosis_core::config::Config::default(),
            args: args(640, 480),
            window: None,
            scene: None,
            font: None,
            watcher: None,
            loader: None,
            gpu: Gpu::Awaiting(receiver),
            first_frame: false,
            last_tick: now,
            next_frame: now,
            exit: false,
            input: String::new(),
            feedback: None,
            overlay_watch: None,
            balloon_radii: (2, 1),
            geometry: (0, 0, 0, 0),
            pending_fullscreen: None,
            line_dirty: true,
            modifiers: winit::keyboard::ModifiersState::empty(),
            config_watcher: None,
            backdrops: crate::backdrop_loader::BackdropLoader::start(),
            asset_loader: crate::asset_loader::AssetLoader::start(),
            assets_worker_gone: false,
            shots: crate::shot_worker::ShotWorker::start(),
            stats: None,
            audio: crate::audio::AudioContext::silent(),
            pointer: (0, 0),
        };
        app.poll_gpu();
        assert!(matches!(app.gpu, Gpu::Dead), "a vanished init degrades");
        // Degraded is terminal: a second poll changes nothing.
        app.poll_gpu();
        assert!(matches!(app.gpu, Gpu::Dead));
    }

    #[test]
    fn input_keys_edit_and_commit() {
        let mut buf = String::new();
        assert_eq!(
            handle_input_key(&mut buf, None, Some("d")),
            InputAction::Edit
        );
        assert_eq!(
            handle_input_key(&mut buf, None, Some("u")),
            InputAction::Edit
        );
        assert_eq!(buf, "du");
        // Backspace removes the last character.
        assert_eq!(
            handle_input_key(&mut buf, Some(&winit::keyboard::NamedKey::Backspace), None),
            InputAction::Edit
        );
        assert_eq!(buf, "d");
        // Space appends (filenames may contain spaces).
        handle_input_key(&mut buf, Some(&winit::keyboard::NamedKey::Space), None);
        assert_eq!(buf, "d ");
        // Enter commits without editing.
        assert_eq!(
            handle_input_key(&mut buf, Some(&winit::keyboard::NamedKey::Enter), None),
            InputAction::Commit
        );
        assert_eq!(buf, "d ");
        // Non-ASCII text and control chars are ignored: nothing added
        // means None, so dead keys cost no redraw.
        assert_eq!(
            handle_input_key(&mut buf, None, Some("ä")),
            InputAction::None
        );
        assert_eq!(buf, "d ");
        // Backspace on an empty buffer adds nothing: None.
        let mut empty = String::new();
        assert_eq!(
            handle_input_key(
                &mut empty,
                Some(&winit::keyboard::NamedKey::Backspace),
                None
            ),
            InputAction::None
        );
        // Unhandled named keys and missing text are None.
        assert_eq!(
            handle_input_key(&mut buf, Some(&winit::keyboard::NamedKey::Tab), None),
            InputAction::None
        );
        assert_eq!(handle_input_key(&mut buf, None, None), InputAction::None);
    }

    #[test]
    fn input_buffer_caps_at_64_chars() {
        let mut buf = String::new();
        for _ in 0..INPUT_MAX_CHARS {
            assert_eq!(
                handle_input_key(&mut buf, None, Some("x")),
                InputAction::Edit
            );
        }
        assert_eq!(buf.chars().count(), INPUT_MAX_CHARS);
        // One more char lands nowhere and reports None.
        assert_eq!(
            handle_input_key(&mut buf, None, Some("y")),
            InputAction::None
        );
        assert_eq!(buf.chars().count(), INPUT_MAX_CHARS);
        assert!(!buf.contains('y'));
        // Space is capped the same way.
        assert_eq!(
            handle_input_key(&mut buf, Some(&winit::keyboard::NamedKey::Space), None),
            InputAction::None
        );
    }

    #[test]
    fn balloon_name_resolves_absolute_ext_and_folder() {
        let dir = std::env::temp_dir().join(format!("agnosis-fix5-resolve-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Absolute path passes through untouched.
        let abs = dir.join("abs.png");
        assert_eq!(
            crate::balloon_resolve::resolve(&dir, abs.to_str().unwrap()).expect("absolute"),
            abs
        );
        // Extension present: used as-is (even when the file is missing).
        assert_eq!(
            crate::balloon_resolve::resolve(&dir, "missing.png").expect("the direct join"),
            dir.join("missing.png")
        );
        // No extension: an existing .png sibling wins.
        std::fs::write(dir.join("duck.png"), b"x").unwrap();
        assert_eq!(
            crate::balloon_resolve::resolve(&dir, "duck").expect("the sibling"),
            dir.join("duck.png")
        );
        // Exact existing file with an extension resolves even when the
        // lowercase probe would also match (existence check first).
        std::fs::write(dir.join("exact.jpg"), b"x").unwrap();
        assert_eq!(
            crate::balloon_resolve::resolve(&dir, "exact.jpg").expect("the exact hit"),
            dir.join("exact.jpg")
        );
        // Case-insensitive: lowercase finds the uppercase sibling.
        // (`SHOUTY.PNG` is written uppercase; on case-insensitive filesystems
        // the exact hit above already returns, on case-sensitive ones the
        // scan below does — either way the name resolves.)
        std::fs::write(dir.join("SHOUTY.PNG"), b"x").unwrap();
        let shouty = crate::balloon_resolve::resolve(&dir, "shouty").expect("the sibling");
        assert_eq!(
            shouty
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_ascii_lowercase()),
            Some("shouty.png".to_string())
        );
        // Stem probe: lowercase name with extension resolves.
        let shouty_ext = crate::balloon_resolve::resolve(&dir, "shouty.png").expect("the stem");
        assert_eq!(
            shouty_ext
                .file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.to_ascii_lowercase()),
            Some("shouty.png".to_string())
        );
        // .ans participates in the extension probe.
        std::fs::write(dir.join("note.ans"), b"hi").unwrap();
        assert_eq!(
            crate::balloon_resolve::resolve(&dir, "note").expect("the stem"),
            dir.join("note.ans")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_balloon_by_name_converts_or_reports() {
        let dir = std::env::temp_dir().join(format!("agnosis-fix5-load-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A real tiny PNG converts into a sprite.
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([200, 40, 40, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::BufWriter::new(std::fs::File::create(dir.join("tiny.png")).unwrap()),
                image::ImageFormat::Png,
            )
            .unwrap();
        let sprite = load_balloon_by_name(&dir, "tiny", 4, 2).expect("converts");
        assert!(sprite.cols() > 0 && sprite.rows() > 0);
        // Missing name reports a user-facing error.
        let err = load_balloon_by_name(&dir, "nope", 4, 2).unwrap_err();
        assert!(err.contains("CANNOT LOAD NOPE"), "got {err}");
        // Empty name asks for input.
        assert_eq!(
            load_balloon_by_name(&dir, "", 4, 2).unwrap_err(),
            "TYPE A NAME FIRST"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_balloon_by_name_honors_sprite_cache() {
        let dir = std::env::temp_dir().join(format!("agnosis-fix5-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A decoy source plus a cache sibling stamped for the live radii:
        // the cache must win over re-conversion (the radii here would
        // convert to 9x5, the cached sprite is 2x2).
        std::fs::write(dir.join("art.png"), b"not really a png").unwrap();
        let cached = agnosis_core::balloon::AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]);
        agnosis_core::image_ascii::save_sprite_stamped(&cached, &dir.join("art.sprite.json"), 4, 2)
            .unwrap();
        let sprite = load_balloon_by_name(&dir, "art", 4, 2).expect("cache hit");
        assert_eq!(sprite, cached);
        // Typing the cache name directly resolves the same way.
        let sprite = load_balloon_by_name(&dir, "art.png", 4, 2).expect("cache hit");
        assert_eq!(sprite, cached);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_balloon_by_name_ignores_unstamped_and_stale_cache() {
        let dir = std::env::temp_dir().join(format!("agnosis-fix5-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A real PNG plus a 2x2 cache: the typed path must ignore a cache
        // that does not carry the live radii, exactly like the watcher
        // (audit P0-3 / P1-13).
        let img = image::RgbaImage::from_pixel(16, 16, image::Rgba([10, 200, 10, 255]));
        image::DynamicImage::ImageRgba8(img)
            .write_to(
                &mut std::io::BufWriter::new(std::fs::File::create(dir.join("art.png")).unwrap()),
                image::ImageFormat::Png,
            )
            .unwrap();
        let cached = agnosis_core::balloon::AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]);
        // Unstamped cache: a miss.
        agnosis_core::image_ascii::save_sprite(&cached, &dir.join("art.sprite.json")).unwrap();
        let sprite = load_balloon_by_name(&dir, "art", 4, 2).expect("converts");
        assert_ne!(sprite, cached, "an unstamped cache must not be used");
        // Cache stamped for other radii: also a miss.
        agnosis_core::image_ascii::save_sprite_stamped(&cached, &dir.join("art.sprite.json"), 9, 4)
            .unwrap();
        let sprite = load_balloon_by_name(&dir, "art", 4, 2).expect("converts");
        assert_ne!(sprite, cached, "a stale-stamp cache must not be used");
        // Cache stamped for the live radii: used.
        agnosis_core::image_ascii::save_sprite_stamped(&cached, &dir.join("art.sprite.json"), 4, 2)
            .unwrap();
        let sprite = load_balloon_by_name(&dir, "art", 4, 2).expect("cache hit");
        assert_eq!(sprite, cached);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A status message must make the input line **dirty**, or it is
    /// never drawn.
    ///
    /// The F12 "COPIED" confirmation was set and then silently dropped:
    /// `set_feedback` did not mark the line dirty, so the redraw never
    /// happened and the user saw nothing (audit 2026-10-22 §UX-16).
    #[test]
    fn a_status_message_marks_the_input_line_dirty() {
        let now = Instant::now();
        let mut app = PopupApp {
            cfg: agnosis_core::config::Config::default(),
            args: args(640, 480),
            window: None,
            scene: None,
            font: None,
            watcher: None,
            loader: None,
            gpu: Gpu::Dead,
            first_frame: false,
            last_tick: now,
            next_frame: now,
            exit: false,
            input: String::new(),
            feedback: None,
            overlay_watch: None,
            balloon_radii: (2, 1),
            geometry: (0, 0, 0, 0),
            pending_fullscreen: None,
            line_dirty: false,
            modifiers: winit::keyboard::ModifiersState::empty(),
            config_watcher: None,
            backdrops: crate::backdrop_loader::BackdropLoader::start(),
            asset_loader: crate::asset_loader::AssetLoader::start(),
            assets_worker_gone: false,
            shots: crate::shot_worker::ShotWorker::start(),
            stats: None,
            audio: crate::audio::AudioContext::silent(),
            pointer: (0, 0),
        };
        app.set_feedback("COPIED");
        assert!(
            app.line_dirty,
            "the message is on the line, so the line must be redrawn"
        );
        // The line is rebuilt exactly once, and it carries the message.
        let line = app.pending_line().expect("the line is rebuilt");
        assert_eq!(line, "COPIED");
        assert!(
            app.pending_line().is_none(),
            "the line is not rebuilt again until it changes"
        );
    }

    #[test]
    fn input_line_text_shows_prompt_then_feedback() {
        let now = Instant::now();
        let mut app = PopupApp {
            cfg: agnosis_core::config::Config::default(),
            args: args(640, 480),
            window: None,
            scene: None,
            font: None,
            watcher: None,
            loader: None,
            gpu: Gpu::Dead,
            first_frame: false,
            last_tick: now,
            next_frame: now,
            exit: false,
            input: "ducks".to_string(),
            feedback: None,
            overlay_watch: None,
            balloon_radii: (2, 1),
            geometry: (0, 0, 0, 0),
            pending_fullscreen: None,
            line_dirty: true,
            modifiers: winit::keyboard::ModifiersState::empty(),
            config_watcher: None,
            backdrops: crate::backdrop_loader::BackdropLoader::start(),
            asset_loader: crate::asset_loader::AssetLoader::start(),
            assets_worker_gone: false,
            shots: crate::shot_worker::ShotWorker::start(),
            stats: None,
            audio: crate::audio::AudioContext::silent(),
            pointer: (0, 0),
        };
        assert_eq!(app.compose_input_line(), "ADD BALLOON: ducks_");
        app.feedback = Some(("ADDED DUCKS".to_string(), now, FEEDBACK_LIFETIME));
        // The buffer wins over fresh feedback: during "LOADING X" the
        // user's next typing used to be invisible, and Enter committed
        // text they never saw (audit 101126 §B7).
        assert_eq!(app.compose_input_line(), "ADD BALLOON: ducks_");
        app.input.clear();
        assert_eq!(app.compose_input_line(), "ADDED DUCKS");
        // Empty buffer: the short usage hint (name, folder, shot, editor,
        // help).
        app.input.clear();
        app.feedback = None;
        let hint = app.compose_input_line();
        assert_eq!(
            hint, "ADD BALLOON: NAME + ENTER - CTRL O FOLDER - F12 SHOT - F2 EDITOR - F1 HELP_",
            "{hint}"
        );
    }

    /// The hint's **tail** survives the narrowest grid.
    ///
    /// The input line clips to the grid's width and keeps the newest
    /// characters (`scene::tail_window`), so the HELP chord must sit at
    /// the end of the hint — on a 2 × 3-cell floor grid the beginning is
    /// cut, and a hint that ended with the folder note would leave F1
    /// undiscoverable (audit 2026-10-23 §F-101).
    #[test]
    fn the_hint_tail_carries_the_help_chord() {
        // The narrowest line the core's own test grid stands for.
        let available = 16usize;
        let tail = &INPUT_HINT[INPUT_HINT.len() - available..];
        assert!(tail.ends_with("F1 HELP_"), "{tail}");
    }

    #[test]
    fn geometry_rebuild_keeps_scene_and_font_on_the_same_cell_dims() {
        let now = Instant::now();
        let mut app = PopupApp {
            cfg: agnosis_core::config::Config::default(),
            args: args(640, 480),
            window: None,
            scene: None,
            font: None,
            watcher: None,
            loader: None,
            gpu: Gpu::Dead,
            first_frame: false,
            last_tick: now,
            next_frame: now,
            exit: false,
            input: String::new(),
            feedback: None,
            overlay_watch: None,
            balloon_radii: (2, 1),
            geometry: (0, 0, 0, 0),
            pending_fullscreen: None,
            line_dirty: true,
            modifiers: winit::keyboard::ModifiersState::empty(),
            config_watcher: None,
            backdrops: crate::backdrop_loader::BackdropLoader::start(),
            asset_loader: crate::asset_loader::AssetLoader::start(),
            assets_worker_gone: false,
            shots: crate::shot_worker::ShotWorker::start(),
            stats: None,
            audio: crate::audio::AudioContext::silent(),
            pointer: (0, 0),
        };
        // A resize rebuilds the grid-dependent state: the scene owns the
        // grid and the font must use the same cell dims, or the glyphs land
        // off-frame (AGENTS.md).
        app.apply_geometry((80, 45, 4, 6));
        let scene = app.scene.as_ref().expect("scene rebuilt");
        assert_eq!(scene.grid().cell_w(), 4);
        assert_eq!(scene.grid().cell_h(), 6);
        let font = app.font.as_ref().expect("font rebuilt");
        assert_eq!(font.cell_w(), 4);
        assert_eq!(font.cell_h(), 6);
        assert_eq!(app.geometry, (80, 45, 4, 6));
        // The same geometry again must not rebuild (a spurious resize event
        // would otherwise reset the parade).
        let before = app.geometry;
        app.apply_geometry(before);
        assert_eq!(app.geometry, before);
    }

    #[test]
    fn geometry_rebuild_keeps_the_balloons() {
        let mut app = test_app();
        let sprite = agnosis_core::balloon::AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]);
        app.apply_geometry((80, 45, 4, 6));
        app.add_balloon_and_message(Some(sprite.clone()), "ADDED SPRITE");
        app.scene
            .as_mut()
            .expect("scene")
            .push_balloon(sprite.clone());
        // The ring lives in the scene and survives the geometry change: the
        // scene is resized, not rebuilt, so nothing is dropped (and nothing
        // is restored on top of it). The popup keeps no mirror of the ring
        // any more — the scene is the one owner (audit 2026-10-23 §F-111).
        app.apply_geometry((120, 60, 4, 6));
        assert_eq!(app.scene.as_ref().expect("scene").balloon_count(), 2);
    }

    #[test]
    fn geometry_change_resizes_the_scene_instead_of_rebuilding_it() {
        let now = Instant::now();
        let mut app = PopupApp {
            cfg: agnosis_core::config::Config::default(),
            args: args(640, 480),
            window: None,
            scene: None,
            font: None,
            watcher: None,
            loader: None,
            gpu: Gpu::Dead,
            first_frame: false,
            last_tick: now,
            next_frame: now,
            exit: false,
            input: String::new(),
            feedback: None,
            overlay_watch: None,
            balloon_radii: (2, 1),
            geometry: (0, 0, 0, 0),
            pending_fullscreen: None,
            line_dirty: true,
            modifiers: winit::keyboard::ModifiersState::empty(),
            config_watcher: None,
            backdrops: crate::backdrop_loader::BackdropLoader::start(),
            asset_loader: crate::asset_loader::AssetLoader::start(),
            assets_worker_gone: false,
            shots: crate::shot_worker::ShotWorker::start(),
            stats: None,
            audio: crate::audio::AudioContext::silent(),
            pointer: (0, 0),
        };
        app.apply_geometry((80, 45, 4, 6));
        let scene = app.scene.as_mut().expect("scene");
        scene.advance(2.0);
        let elapsed = scene.elapsed();
        let phase = scene.parade_phase();
        // A new geometry re-lays the scene: the grid follows the new dims
        // while the simulation (clock, parade phase) keeps running.
        app.apply_geometry((120, 60, 4, 6));
        let scene = app.scene.as_ref().expect("scene");
        assert_eq!(scene.cols(), 120);
        assert_eq!(scene.rows(), 60);
        assert_eq!(scene.elapsed(), elapsed, "the clock must not restart");
        assert!(
            (scene.parade_phase() - phase).abs() < 1e-4,
            "the parade phase must survive, got {} vs {phase}",
            scene.parade_phase()
        );
        // The font still follows the grid's cell dims.
        let font = app.font.as_ref().expect("font");
        assert_eq!(font.cell_w(), 4);
        assert_eq!(font.cell_h(), 6);
    }

    #[test]
    fn input_line_is_built_only_when_it_changed() {
        let now = Instant::now();
        let mut app = PopupApp {
            cfg: agnosis_core::config::Config::default(),
            args: args(640, 480),
            window: None,
            scene: None,
            font: None,
            watcher: None,
            loader: None,
            gpu: Gpu::Dead,
            first_frame: false,
            last_tick: now,
            next_frame: now,
            exit: false,
            input: String::new(),
            feedback: None,
            overlay_watch: None,
            balloon_radii: (2, 1),
            geometry: (0, 0, 0, 0),
            pending_fullscreen: None,
            line_dirty: true,
            modifiers: winit::keyboard::ModifiersState::empty(),
            config_watcher: None,
            backdrops: crate::backdrop_loader::BackdropLoader::start(),
            asset_loader: crate::asset_loader::AssetLoader::start(),
            assets_worker_gone: false,
            shots: crate::shot_worker::ShotWorker::start(),
            stats: None,
            audio: crate::audio::AudioContext::silent(),
            pointer: (0, 0),
        };
        // The first frame builds the line, the next ones do not: the
        // per-frame path must not allocate (audit P0-1).
        let first = app.pending_line().expect("first frame builds the line");
        assert!(first.starts_with("ADD BALLOON: NAME + ENTER"));
        assert!(
            app.pending_line().is_none(),
            "unchanged line must not rebuild"
        );
        // A typed character invalidates the line again.
        app.line_dirty = true;
        assert!(app.pending_line().is_some());
        // An expired status message falls back to the prompt.
        app.feedback = Some(("ADDED DUCKS".to_string(), now, FEEDBACK_LIFETIME));
        app.line_dirty = true;
        assert_eq!(app.pending_line().as_deref(), Some("ADDED DUCKS"));
        app.feedback = Some((
            "ADDED DUCKS".to_string(),
            now - FEEDBACK_LIFETIME - Duration::from_millis(1),
            FEEDBACK_LIFETIME,
        ));
        let after = app
            .pending_line()
            .expect("expired feedback rebuilds the line");
        assert!(after.starts_with("ADD BALLOON: NAME + ENTER"), "{after}");
    }

    #[test]
    fn the_o_key_is_always_typeable() {
        // The folder shortcut is `Ctrl+O` now, so a bare `o` is an
        // ordinary character: a balloon name that starts with "o" must be
        // typeable (audit 2026-10-20 §3.4 — the bare `O` shortcut made
        // every such name untypeable).
        let mut buf = String::new();
        let named = None;
        handle_input_key(&mut buf, named, Some("o"));
        assert_eq!(buf, "o", "O must be typeable as the first character");
        handle_input_key(&mut buf, named, Some("r"));
        handle_input_key(&mut buf, named, Some("a"));
        handle_input_key(&mut buf, named, Some("n"));
        handle_input_key(&mut buf, named, Some("g"));
        handle_input_key(&mut buf, named, Some("e"));
        assert_eq!(buf, "orange", "a name starting with o must be typeable");
    }

    #[test]
    fn the_folder_shortcut_needs_the_control_modifier() {
        // The shortcut is `Ctrl+O`; the modifier state arrives on its own
        // event, so the check reads the tracked state.
        let mut buf = String::new();
        let named = None;
        handle_input_key(&mut buf, named, Some("o"));
        assert_eq!(buf, "o", "without Ctrl the letter is typed");
    }

    #[test]
    fn the_balloon_loader_loads_on_a_worker_thread() {
        // The typed path must not resolve on the UI thread: the resolve is
        // a recursive folder scan and the conversion a full image decode
        // (audit 2026-10-20 §3.2). The loader hands the work to a thread
        // and reports the answer through a channel.
        //
        // The folder is an **empty** temp folder of our own: the resolve
        // walks it recursively, so the whole `%TEMP%` (which on a
        // developer machine is a big tree) made the test race its own
        // budget (audit 2026-10-21 §G11).
        let dir = temp_dir("balloon-loader");
        let mut loader = BalloonLoader::start().expect("the loader thread starts");
        assert!(
            loader
                .request(&dir, "definitely-not-a-balloon", 4, 2)
                .is_ok(),
            "the request is queued"
        );
        // The answer arrives on the channel, not inline.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut seen = false;
        while std::time::Instant::now() < deadline {
            let results = loader.drain();
            if let Some(result) = results.first() {
                assert_eq!(result.name, "definitely-not-a-balloon");
                assert!(
                    result.sprite.is_err(),
                    "a missing name must come back as an error, not a sprite"
                );
                seen = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(seen, "the loader must answer the request");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_name_asked_twice_converts_once() {
        // The in-flight set is the request identity the channel alone
        // could not carry: two Enters on one name used to queue two
        // conversions and push two copies of the same balloon into the
        // ring (audit 101126 §B6).
        let dir = temp_dir("balloon-loader-dupe");
        let mut loader = BalloonLoader::start().expect("the loader thread starts");
        assert!(loader.request(&dir, "no-such-balloon", 4, 2).is_ok());
        assert!(
            matches!(
                loader.request(&dir, "no-such-balloon", 4, 2),
                Err(LoadReject::InFlight)
            ),
            "the duplicate is refused, not queued"
        );
        // The answer frees the name: it can be asked again.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut seen = false;
        while Instant::now() < deadline {
            if !loader.drain().is_empty() {
                seen = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(seen, "the loader must answer the request");
        assert!(
            loader.request(&dir, "no-such-balloon", 4, 2).is_ok(),
            "the answered name can be asked again"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The live reload must not read the drawn art on the UI thread.
    ///
    /// The reload goes through the **worker**: the config reaches the
    /// scene when the worker has answered, and the scene asks for the
    /// assets through a loader closure (audit 2026-10-21 §G7).
    #[test]
    fn the_config_reload_reaches_the_scene_through_the_worker() {
        let mut app = test_app();
        app.scene = Some(agnosis_core::scene::Scene::new(&app.cfg, 40, 20, 4, 8));
        let mut cfg = app.cfg.clone();
        cfg.message.text = "HELLO".to_string();
        app.apply_new_config(cfg);
        // The worker has not answered yet: the scene still carries the
        // old config, so the message is not there.
        assert!(
            app.scene.as_ref().expect("scene").message_blit().is_none(),
            "the scene must wait for the worker"
        );
        // The worker answers and the drain applies the config.
        let deadline = Instant::now() + Duration::from_secs(5);
        while app.scene.as_ref().expect("scene").message_blit().is_none()
            && Instant::now() < deadline
        {
            app.drain_assets_into_scene();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            app.scene.as_ref().expect("scene").message_blit().is_some(),
            "the reload must reach the scene"
        );
    }

    /// A popup with no window, for the tests that only exercise its logic.
    fn test_app() -> PopupApp {
        let now = Instant::now();
        PopupApp {
            cfg: agnosis_core::config::Config::default(),
            args: args(640, 480),
            window: None,
            scene: None,
            font: None,
            watcher: None,
            loader: None,
            gpu: Gpu::Dead,
            first_frame: false,
            last_tick: now,
            next_frame: now,
            exit: false,
            input: String::new(),
            feedback: None,
            overlay_watch: None,
            balloon_radii: (2, 1),
            geometry: (0, 0, 0, 0),
            pending_fullscreen: None,
            line_dirty: true,
            modifiers: winit::keyboard::ModifiersState::empty(),
            config_watcher: None,
            backdrops: crate::backdrop_loader::BackdropLoader::start(),
            asset_loader: crate::asset_loader::AssetLoader::start(),
            assets_worker_gone: false,
            shots: crate::shot_worker::ShotWorker::start(),
            stats: None,
            audio: crate::audio::AudioContext::silent(),
            pointer: (0, 0),
        }
    }

    /// Every grid cell of the popup, row by row.
    ///
    /// The cells come from one iterator chain, so the helpers below have
    /// no nested loops (audit 2026-10-21 §G6).
    fn grid_cells(app: &PopupApp) -> impl Iterator<Item = (i64, i64)> {
        let (cols, rows) = (app.geometry.0, app.geometry.1);
        (0..rows).flat_map(move |row| (0..cols).map(move |col| (col as i64, row as i64)))
    }

    /// The first grid cell that holds a duck, if the parade has one on
    /// screen.
    fn a_duck_cell(app: &PopupApp) -> Option<(i64, i64)> {
        let scene = app.scene.as_ref()?;
        grid_cells(app).find(|&(col, row)| scene.duck_at(col, row).is_some())
    }

    /// The first grid cell that holds no duck.
    fn an_empty_cell(app: &PopupApp) -> Option<(i64, i64)> {
        let scene = app.scene.as_ref()?;
        grid_cells(app).find(|&(col, row)| scene.duck_at(col, row).is_none())
    }

    /// The audio events the scene has queued.
    fn take_events(app: &mut PopupApp) -> Vec<crate::audio::AudioEvent> {
        app.scene.as_mut().expect("the scene").take_audio_events()
    }

    /// A click on a duck makes it react — and cluck.
    ///
    /// The click's position is given in **pixels**: the popup is
    /// borderless and fullscreen, so the pointer maps straight onto the
    /// grid (roadmap Extension 3 §5.1).
    #[test]
    fn a_click_on_a_duck_makes_it_react() {
        let mut app = test_app();
        app.apply_geometry((160, 120, 12, 24));
        app.scene.as_mut().expect("scene").advance(2.0);
        let (col, row) = a_duck_cell(&app).expect("a duck must be on screen");
        app.pointer = (col * 12, row * 24);
        app.on_click();
        let events = take_events(&mut app);
        assert!(
            events.iter().any(|e| matches!(
                e,
                crate::audio::AudioEvent::Sfx(crate::audio::sfx::SfxId::Cluck)
            )),
            "the duck must cluck, got {events:?}"
        );
    }

    /// A click on empty ground does nothing at all.
    #[test]
    fn a_click_on_empty_ground_does_nothing() {
        let mut app = test_app();
        app.apply_geometry((160, 120, 12, 24));
        app.scene.as_mut().expect("scene").advance(2.0);
        let (col, row) = an_empty_cell(&app).expect("empty ground must exist");
        app.pointer = (col * 12, row * 24);
        app.on_click();
        assert!(
            take_events(&mut app).is_empty(),
            "a click on empty ground must not touch the parade"
        );
    }

    /// A plain click jumps; a Shift-click holds the duck in place.
    ///
    /// The default must not disturb the parade: a train that stopped every
    /// time the user moved the mouse would be a broken parade (roadmap
    /// Extension 3 §5.4).
    #[test]
    fn a_shift_click_asks_for_the_stop_reaction() {
        let mut app = test_app();
        assert_eq!(
            app.click_reaction_kind(),
            agnosis_core::reaction::ReactionKind::Jump
        );
        app.modifiers = winit::keyboard::ModifiersState::SHIFT;
        assert_eq!(
            app.click_reaction_kind(),
            agnosis_core::reaction::ReactionKind::Stop
        );
    }

    /// The pointer's pixel position maps onto the grid's cell.
    #[test]
    fn the_pointer_maps_onto_the_grid() {
        let mut app = test_app();
        app.apply_geometry((160, 120, 12, 24));
        assert_eq!(app.pointer_cell(), Some((0, 0)));
        app.pointer = (12 * 7 + 5, 24 * 3 + 1);
        assert_eq!(app.pointer_cell(), Some((7, 3)));
    }

    /// A space delivered as **text** is typed, not dropped.
    ///
    /// Some platforms and IMEs deliver the space bar as `text: " "` and
    /// never as the named key; a balloon name with a space in it stayed
    /// untypeable there (audit 2026-10-23 §F-118).
    #[test]
    fn a_space_delivered_as_text_is_typed() {
        let mut buf = String::new();
        assert_eq!(
            handle_input_key(&mut buf, None, Some("red")),
            InputAction::Edit
        );
        assert_eq!(
            handle_input_key(&mut buf, None, Some(" ")),
            InputAction::Edit
        );
        assert_eq!(
            handle_input_key(&mut buf, None, Some("duck")),
            InputAction::Edit
        );
        assert_eq!(buf, "red duck", "the space survives in the buffer");
        // A control character delivered as text is still dropped.
        assert_eq!(
            handle_input_key(&mut buf, None, Some("\t")),
            InputAction::None
        );
        assert_eq!(buf, "red duck");
    }

    /// A loader answer made for **other radii** is stale and is dropped.
    ///
    /// A geometry change between the request and the answer leaves a
    /// sprite converted for a disc of another shape; drawing it would
    /// distort the balloon. The watcher settles and the asset loader
    /// carries generations — the typed loader now carries its radii
    /// (audit 2026-10-23 §F-117).
    #[test]
    fn a_stale_loader_answer_is_dropped() {
        let mut app = test_app();
        app.apply_geometry((160, 120, 12, 24));
        let sprite = Ok(agnosis_core::balloon::AsciiSprite::from_rows(
            &["@@", "@@"],
            [9, 8, 7],
        ));
        let stale = LoadResult {
            name: "stale".to_string(),
            radii: (app.balloon_radii.0 + 1, app.balloon_radii.1),
            sprite: sprite.clone(),
        };
        let fresh = LoadResult {
            name: "fresh".to_string(),
            radii: app.balloon_radii,
            sprite,
        };
        // A loader whose answer channel already carries one stale and one
        // fresh answer, in that order.
        let (req_tx, _req_rx) = std::sync::mpsc::channel::<LoadRequest>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<LoadResult>();
        res_tx.send(stale).expect("send the stale answer");
        res_tx.send(fresh).expect("send the fresh answer");
        drop(res_tx);
        app.loader = Some(BalloonLoader {
            requests: req_tx,
            results: res_rx,
            pending: std::collections::HashSet::new(),
        });
        app.drain_loader_into_scene();
        let scene = app.scene.as_ref().expect("scene");
        assert_eq!(
            scene.balloon_count(),
            1,
            "only the fresh answer may land in the ring"
        );
        assert_eq!(app.compose_input_line(), "ADDED FRESH");
    }

    /// A watcher outcome lands on the input line with its name or its
    /// reason: a dropped image is reported, never silent
    /// (audit 2026-10-23 §F-104).
    #[test]
    fn a_watcher_outcome_speaks_on_the_input_line() {
        let mut app = test_app();
        app.apply_geometry((160, 120, 12, 24));
        let sprite = agnosis_core::balloon::AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]);
        let (pushed, msg) = watcher_message(crate::watcher::WatchOutcome::Added {
            name: "cat.png".to_string(),
            sprite: sprite.clone(),
        });
        app.add_balloon_and_message(pushed, &msg);
        assert_eq!(app.compose_input_line(), "ADDED CAT.PNG");
        assert_eq!(app.scene.as_ref().expect("scene").balloon_count(), 1);
        let (pushed, msg) = watcher_message(crate::watcher::WatchOutcome::Skipped {
            file: "broken.png".to_string(),
            reason: "unsupported".to_string(),
        });
        assert!(pushed.is_none(), "a skipped file pushes no sprite");
        app.add_balloon_and_message(pushed, &msg);
        assert_eq!(
            app.compose_input_line(),
            "CANNOT LOAD BROKEN.PNG: unsupported",
            "the reason is the resolver's own words, like the typed path"
        );
        assert_eq!(
            app.scene.as_ref().expect("scene").balloon_count(),
            1,
            "the skip changed nothing in the ring"
        );
    }

    /// A rejected live-reload edit says so on the input line.
    ///
    /// The old values keep the parade running, but the user also has to
    /// know the edit did not land (audit 2026-10-23 §F-097) —
    /// `agnosis config validate` is the terminal's way to see the detail.
    #[test]
    fn a_rejected_reload_edit_reports_on_the_input_line() {
        let dir = temp_dir("reload-feedback");
        let path = dir.join("config.toml");
        std::fs::write(&path, "this is not = = toml").expect("write");
        let parsed: Option<agnosis_core::config::Config> = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str(&t).ok());
        assert!(parsed.is_none(), "the broken file must not parse");
        // The reload path's reaction: the feedback the user sees.
        let mut app = test_app();
        app.set_feedback("CONFIG ERROR - OLD VALUES KEPT");
        let line = app.pending_line().expect("the line is rebuilt");
        assert_eq!(line, "CONFIG ERROR - OLD VALUES KEPT");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
