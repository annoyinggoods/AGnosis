//! Resident hotkey daemon: toggle-shut a running popup or spawn a fresh one.
//!
//! [`run`] arms [`crate::platform::daemon_ctl`], spawns the platform hotkey
//! listener for the configured chords and handles triggers until a stop is
//! requested via `agnosis stop`. [`handle_action`] implements one trigger:
//! the parade hotkey closes a running popup if any (toggle), else spawns
//! `<self> popup ...` for the target monitor and minimizes browsers.
//! Spawning first wins the window race even if enumeration is slow; a spawn
//! failure is an error while a minimize failure is reported as an error and
//! written to `AGNOSIS_TRACE_FILE` — the daemon's stdio is nulled, so the
//! trace file is the only durable channel (audit robustness).
//!
//! Two chords are armed: the parade hotkey (platform specific) and the
//! editor hotkey (`[hotkey].editor`, one string for every platform). The
//! editor chord is optional — a broken editor spec is reported and dropped,
//! never allowed to take the parade down. The editor chord **toggles** the
//! draw-your-own editor: it closes a running editor or spawns `<self>
//! editor ...` for the target monitor. It never touches the parade popup
//! and never minimizes the browsers — that is the parade hotkey's job
//! (roadmap decision D10).
//!
//! The daemon refuses to start when another daemon already owns the
//! stop-control record (audit P0-2) and drops its own record on the way
//! out, so a clean stop leaves no stale PID file behind.

use crate::platform::chord::parse_chord;
use crate::platform::daemon_ctl;
use crate::platform::hotkey::{self, HotkeyAction, HotkeyError};
use crate::platform::minimize;
use crate::platform::window;
use agnosis_core::config::Config;

/// Poll slice for the stop wait so triggers stay responsive.
const STOP_SLICE_MS: u32 = 25;

/// Failures from [`run`] and [`handle_action`].
#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    /// Configuration load failure.
    #[error(transparent)]
    Config(#[from] agnosis_core::config::ConfigError),
    /// Hotkey listener failure.
    #[error(transparent)]
    Hotkey(#[from] crate::platform::hotkey::HotkeyError),
    /// Popup spawn failure with a message.
    #[error("spawn failed: {0}")]
    Spawn(String),
    /// Browser-minimize failure with a message (audit robustness: surfaced
    /// instead of trace-only, so the visible-browsers failure is actionable).
    #[error("minimize failed: {0}")]
    Minimize(String),
    /// Stop-signalling failure with a message.
    #[error("control error: {0}")]
    Control(String),
    /// Another daemon already owns the stop-control record (audit P0-2):
    /// the message is deliberately distinct so the CLI can say so plainly
    /// instead of reporting a generic daemon failure.
    #[error("{0}")]
    AlreadyRunning(String),
    /// A chord spec that **parsed** but cannot ever fire: modifiers, no
    /// key. Its own variant — `UnknownToken` was a lie for a spec that
    /// parsed fine, and the CLI printed `chord spec has unknown token`
    /// next to an accurate trace line (audit 2026-10-23 §F-106).
    #[error("hotkey spec: {0}")]
    HotkeySpec(String),
}

/// Resident loop: arm stop signalling, spawn the hotkey listener, handle
/// triggers until stopped.
///
/// Never panics on a poisoned or missing hotkey thread: a dead listener
/// channel is logged and reported as an error instead. On the way out the
/// daemon's own stop-control record is dropped (audit P0-2) so a clean stop
/// leaves no stale PID file behind, and the exit reason lands in the
/// runtime folder — the only channel a detached, stdio-nulled process has,
/// and the one `agnosis start` prints when the daemon it just spawned is
/// gone a second later (audit 2026-10-23 §F-100).
pub fn run(cfg: &Config) -> Result<(), DaemonError> {
    agnosis_core::trace::trace("daemon.start");
    let outcome = run_armed(cfg);
    match &outcome {
        Ok(()) => write_exit_reason("stopped"),
        Err(e) => write_exit_reason(&e.to_string()),
    }
    outcome
}

/// Arm the stop control, serve, and drop the record on the way out.
fn run_armed(cfg: &Config) -> Result<(), DaemonError> {
    arm_control()?;
    let outcome = serve(cfg);
    daemon_ctl::disarm_daemon_stop();
    outcome
}

/// The file the daemon's exit reason lives in.
fn exit_reason_file() -> Result<std::path::PathBuf, crate::paths::PathError> {
    crate::paths::exit_reason_file()
}

/// Drop any exit reason an earlier daemon left: `agnosis start` clears the
/// file before it spawns, so whatever it reads later is this daemon's own
/// words (audit 2026-10-23 §F-100).
pub fn clear_exit_reason() {
    if let Ok(path) = exit_reason_file() {
        clear_exit_reason_at(&path);
    }
}

/// The clear, at an explicit path (the test seam).
fn clear_exit_reason_at(path: &std::path::Path) {
    let _ = std::fs::remove_file(path);
}

/// The last daemon's exit reason, when it left one.
pub fn read_exit_reason() -> Option<String> {
    let path = exit_reason_file().ok()?;
    read_exit_reason_from(&path)
}

/// The read, at an explicit path (the test seam): a missing file and a
/// blank one are both "no reason".
fn read_exit_reason_from(path: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let trimmed = text.trim().to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

/// Record the exit reason (best effort): the trace file stays the
/// diagnostic channel, this one is what the CLI prints.
fn write_exit_reason(reason: &str) {
    let path = match exit_reason_file() {
        Ok(path) => path,
        Err(_) => return,
    };
    write_exit_reason_to(&path, reason);
}

/// The write, at an explicit path (the test seam): the file's parent is
/// created, the write is best effort.
fn write_exit_reason_to(path: &std::path::Path, reason: &str) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(path, reason);
}

/// Arm the stop-control mechanism, mapping the "already running" case to
/// its own error so the CLI can say so plainly (audit P0-2).
///
/// The daemon's stdio is nulled when it is spawned, so the warning also
/// goes to `AGNOSIS_TRACE_FILE` — the only durable channel left.
fn arm_control() -> Result<(), DaemonError> {
    daemon_ctl::arm_daemon_stop().map_err(|e| match e {
        daemon_ctl::DaemonCtlError::AlreadyRunning(pid) => {
            let owner = pid.map(|p| format!(" (pid {p})")).unwrap_or_default();
            let msg = format!("daemon already running{owner}");
            agnosis_core::trace::warn(&format!("daemon.already_running: {msg}"));
            DaemonError::AlreadyRunning(msg)
        }
        other => DaemonError::Control(other.to_string()),
    })?;
    agnosis_core::trace::trace("daemon.stop_armed");
    Ok(())
}

/// Spawn the hotkey listener and serve triggers until the loop ends.
fn serve(cfg: &Config) -> Result<(), DaemonError> {
    let bindings = hotkey_bindings(cfg).inspect_err(report_hotkey_unavailable)?;
    let (handle, triggers) = hotkey::spawn(&bindings).map_err(|e| {
        report_hotkey_unavailable(&e);
        DaemonError::Hotkey(e)
    })?;
    agnosis_core::trace::trace("daemon.hotkey_spawned");
    let outcome = pump(cfg, &triggers);
    handle.stop();
    outcome
}

/// Report an unusable hotkey configuration to the trace file.
///
/// Audit P0-5: the Unsupported backend (macOS) must be loud — the daemon
/// exits here, and stdio may be nulled, so warn (stderr +
/// `AGNOSIS_TRACE_FILE`) as well as trace.
fn report_hotkey_unavailable<E: std::fmt::Display>(e: &E) {
    agnosis_core::trace::warn(&format!("daemon.hotkey_unavailable: {e}"));
}

/// The chords the daemon arms: the parade hotkey and the editor hotkey.
///
/// The parade chord keeps today's behaviour: a spec that does not parse is
/// a hard error, so the daemon reports it and exits. The editor chord is
/// optional: a spec that does not parse is reported and dropped, so a typo
/// in the editor chord can never take the parade down.
fn hotkey_bindings(cfg: &Config) -> Result<Vec<(String, HotkeyAction)>, DaemonError> {
    let parade_spec = crate::host::hotkey_for_current_platform(&cfg.hotkey);
    let parade = parse_chord(parade_spec).map_err(|e| DaemonError::Hotkey(HotkeyError::Spec(e)))?;
    // A spec with modifiers but no key parses fine and can never fire,
    // so the daemon would run with a dead hotkey and say nothing
    // (audit 2026-10-20 §3.18). It is a configuration error, and it is
    // reported as one.
    if parade.keys.is_empty() {
        let msg = format!("the hotkey {parade_spec:?} has no key, only modifiers");
        agnosis_core::trace::warn(&format!("daemon.parade_hotkey_invalid: {msg}"));
        return Err(DaemonError::HotkeySpec(msg));
    }
    let mut bindings = vec![(parade_spec.to_string(), HotkeyAction::Popup)];
    bindings.extend(editor_binding(cfg.hotkey.editor_spec()));
    Ok(bindings)
}

/// The editor binding, or none when the spec is empty or broken.
///
/// An empty spec disables the editor chord (the roadmap allows that); a
/// spec that does not parse is reported and dropped — the parade chord
/// keeps working without it.
fn editor_binding(spec: &str) -> Option<(String, HotkeyAction)> {
    if spec.trim().is_empty() {
        agnosis_core::trace::trace("daemon.editor_hotkey_disabled");
        return None;
    }
    let chord = match parse_chord(spec) {
        Ok(chord) => chord,
        Err(e) => {
            agnosis_core::trace::warn(&format!("daemon.editor_hotkey_invalid: {e}"));
            return None;
        }
    };
    // A spec with modifiers but no key can never fire, so it is dropped
    // like a broken one (audit 2026-10-20 §3.18).
    if chord.keys.is_empty() {
        agnosis_core::trace::warn(&format!(
            "daemon.editor_hotkey_invalid: the hotkey {spec:?} has no key, only modifiers"
        ));
        return None;
    }
    Some((spec.to_string(), HotkeyAction::Editor))
}

/// Trigger loop: wait for stop requests and fire queued triggers.
fn pump(
    cfg: &Config,
    triggers: &std::sync::mpsc::Receiver<HotkeyAction>,
) -> Result<(), DaemonError> {
    let mut steps = PlatformSteps::new();
    // Whether the parade was on screen the last time we looked. The
    // transition from "open" to "closed" is what puts the browsers back
    // (audit 2026-10-20 §3.7) — and it catches the parade ending by
    // `ESC` or the window manager, not only by the hotkey.
    let mut parade_was_open = false;
    loop {
        if daemon_ctl::wait_for_daemon_stop(STOP_SLICE_MS) {
            agnosis_core::trace::trace("daemon.stopped");
            restore_on_graceful_stop(cfg, &mut steps);
            return Ok(());
        }
        let fired = match drain_triggers(triggers) {
            Drain::Fired(actions) => actions,
            Drain::Disconnected => {
                agnosis_core::trace::trace("daemon.hotkey_disconnected");
                return Err(DaemonError::Control(
                    "hotkey listener thread exited".to_string(),
                ));
            }
        };
        fire_actions(cfg, fired, &mut steps);
        restore_browsers_when_the_parade_ends(cfg, &mut steps, &mut parade_was_open);
    }
}

/// Put the browsers back when the parade is gone — **only if the user
/// asked for it**.
///
/// The parade ends in several ways — the hotkey, `ESC`, the window
/// manager, a crash — and only the first is something the daemon did
/// itself, so the daemon watches for the window instead of assuming
/// (audit 2026-10-20 §3.7).
///
/// The restore is **off by default** (`[browsers] restore_on_close`): the
/// app's premise is the old Windows XP "hide your browser behind a fake
/// hacker terminal", and putting the browser back is the "parent walked
/// in on you" moment that ends the joke. Nothing is ever closed, only
/// minimized, so no work is lost either way.
fn restore_browsers_when_the_parade_ends(
    cfg: &Config,
    steps: &mut impl ActionSteps,
    parade_was_open: &mut bool,
) {
    if !cfg.browsers.restore_on_close {
        return;
    }
    let open = steps.parade_open();
    if *parade_was_open && !open {
        steps.restore_browsers();
    }
    *parade_was_open = open;
}

/// The daemon's last act on a **graceful** stop (`agnosis stop`): put the
/// browsers back when the parade is still open.
///
/// With the daemon gone there is nobody left to watch for the parade's
/// end, so the usual open→closed restore would never fire and the user
/// un-minimized by hand despite `[browsers] restore_on_close`
/// (audit 2026-10-23 §F-107). The browsers come back **now**, while the
/// record of what was hidden still exists. A daemon **crash** stays out
/// of scope: a dead process cannot run its own last act.
fn restore_on_graceful_stop(cfg: &Config, steps: &mut impl ActionSteps) {
    if !cfg.browsers.restore_on_close {
        return;
    }
    if !steps.parade_open() {
        return;
    }
    steps.restore_browsers();
}

/// Outcome of draining pending hotkey triggers (audit nesting site #13).
enum Drain {
    /// Actions fired this slice.
    Fired(Vec<HotkeyAction>),
    /// Listener channel died.
    Disconnected,
}

/// Drain pending hotkey triggers (audit nesting site #13): collects one
/// slice without touching shutdown, so `run` stays flat.
fn drain_triggers(triggers: &std::sync::mpsc::Receiver<HotkeyAction>) -> Drain {
    let mut fired: Vec<HotkeyAction> = Vec::new();
    loop {
        match triggers.try_recv() {
            Ok(action) => fired.push(action),
            Err(std::sync::mpsc::TryRecvError::Empty) => return Drain::Fired(fired),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => return Drain::Disconnected,
        }
    }
}

/// Fire one action (audit nesting site #13): failures are loud but
/// non-fatal to the loop.
fn fire_once(cfg: &Config, action: HotkeyAction, steps: &mut impl ActionSteps) {
    if let Err(e) = handle_action_with(cfg, action, steps) {
        agnosis_core::trace::warn(&format!("daemon.trigger_failed: {e}"));
        eprintln!("agnosis: hotkey trigger failed: {e}");
    }
}

/// Fire every action of one slice, in order (audit nesting site #13): the
/// repetition lives here so the trigger loop carries no inner loop.
fn fire_actions(cfg: &Config, actions: Vec<HotkeyAction>, steps: &mut impl ActionSteps) {
    for action in actions {
        fire_once(cfg, action, steps);
    }
}

/// The side effects one hotkey action can perform.
///
/// The daemon runs them against the live desktop; the tests run them
/// against a recorder, so every branch is exercised without a window, a
/// monitor or a child process (AGENTS.md: tests must be deterministic).
pub(crate) trait ActionSteps {
    /// Close a running parade popup; true when one was closed.
    fn close_popup(&mut self) -> bool;
    /// Spawn a parade popup for the target monitor.
    fn spawn_popup(&mut self, args: &crate::popup::PopupArgs) -> Result<(), DaemonError>;
    /// Wait, briefly, for a spawned popup's window to exist.
    ///
    /// WHY: the popup is a **detached child**, so its window does not
    /// exist for tens of milliseconds after the spawn. A second hotkey
    /// press inside that window took the spawn branch again and opened a
    /// **second** parade (audit 2026-10-21 §G21). The wait runs on the
    /// daemon's hotkey thread, never on a UI thread.
    ///
    /// The default is a no-op: a caller that has no window system (the
    /// recorder in the tests) has nothing to wait for.
    fn wait_for_popup_window(&mut self) {}
    /// Minimize the configured browsers, excluding our own process.
    fn minimize_browsers(&mut self, names: &[String], own_pid: u32) -> Result<usize, String>;
    /// Put back the browsers the last minimize pass hid.
    fn restore_browsers(&mut self);
    /// Is the parade popup on screen right now?
    fn parade_open(&mut self) -> bool;
    /// Close a running editor; true when one was closed.
    fn close_editor(&mut self) -> bool;
    /// Wait, briefly, for a spawned editor's window to exist.
    ///
    /// The editor's mirror of [`ActionSteps::wait_for_popup_window`]:
    /// the editor is a detached child, so a double-tap of the editor
    /// chord took the spawn branch twice and opened a second editor
    /// (audit 2026-10-22 §P1-6).
    ///
    /// The default is a no-op: a caller that has no window system (the
    /// recorder in the tests) has nothing to wait for.
    fn wait_for_editor_window(&mut self) {}
    /// Resolve the target monitor: its origin, its **monitor** size and
    /// the DPI scale. The editor derives its own window rect from the
    /// monitor size, so it gets the monitor geometry, not a window size.
    fn resolve_target(&mut self, cfg: &Config) -> Result<crate::popup::PopupArgs, String>;
    /// Spawn the editor process for the resolved target.
    fn spawn_editor(&mut self, args: &crate::editor::EditorArgs) -> Result<(), DaemonError>;
}

/// The real side effects: the window system, the detached spawns and the
/// browser minimizer.
///
/// It also **remembers what it hid**, so the browsers can be put back when
/// the parade ends (audit 2026-10-20 §3.7): the app's headline behaviour
/// is "hides your browser behind the parade", and leaving the browser
/// minimized after the parade closed was a bug, not a design.
pub(crate) struct PlatformSteps {
    /// What the last minimize pass hid.
    hidden: Option<minimize::Minimized>,
}

impl PlatformSteps {
    /// Fresh steps with nothing hidden yet.
    pub(crate) fn new() -> Self {
        Self { hidden: None }
    }

    /// Put back the browsers the last minimize pass hid.
    ///
    /// The public face of the trait method, so a holder of the steps
    /// (the daemon loop, the menu bar) can restore without the trait.
    pub(crate) fn restore_hidden_browsers(&mut self) {
        let Some(record) = self.hidden.take() else {
            return;
        };
        if record.is_empty() {
            return;
        }
        minimize::restore(&record);
        agnosis_core::trace::trace("daemon.browsers_restored");
    }
}

impl Default for PlatformSteps {
    fn default() -> Self {
        Self::new()
    }
}

impl ActionSteps for PlatformSteps {
    fn close_popup(&mut self) -> bool {
        window::close_popup()
    }

    fn spawn_popup(&mut self, args: &crate::popup::PopupArgs) -> Result<(), DaemonError> {
        spawn_popup(args)
    }

    fn wait_for_popup_window(&mut self) {
        window::wait_for_popup_window();
    }

    fn wait_for_editor_window(&mut self) {
        window::wait_for_editor_window();
    }

    fn minimize_browsers(&mut self, names: &[String], own_pid: u32) -> Result<usize, String> {
        let record = minimize::minimize_and_record(names, own_pid).map_err(|e| e.to_string())?;
        let count = record.len();
        // The record is **merged**, not replaced: a pass that ran between
        // the parade's close and the restore used to orphan the previous
        // pass's windows — hidden and never put back (audit 101126 §B5).
        match self.hidden.take() {
            Some(mut held) => {
                held.merge(record);
                self.hidden = Some(held);
            }
            None => self.hidden = Some(record),
        }
        Ok(count)
    }

    fn restore_browsers(&mut self) {
        self.restore_hidden_browsers();
    }

    fn parade_open(&mut self) -> bool {
        window::popup_is_open()
    }

    fn close_editor(&mut self) -> bool {
        window::close_editor()
    }

    fn resolve_target(&mut self, cfg: &Config) -> Result<crate::popup::PopupArgs, String> {
        crate::cli::resolve_popup_target(cfg)
    }

    fn spawn_editor(&mut self, args: &crate::editor::EditorArgs) -> Result<(), DaemonError> {
        spawn_editor(args)
    }
}

/// React to one hotkey action.
///
/// `Popup` is the parade hotkey: close a running popup if any (toggle),
/// else spawn `<self> popup ...` for the target monitor and minimize the
/// browsers. `Editor` is the draw-your-own chord: close a running editor
/// if any (toggle), else spawn `<self> editor ...` for the target
/// monitor. The editor action never touches the parade popup and never
/// minimizes the browsers (roadmap decision D10).
///
/// One hotkey action with every side effect injected, so every branch is
/// testable without a live desktop.
pub(crate) fn handle_action_with(
    cfg: &Config,
    action: HotkeyAction,
    steps: &mut impl ActionSteps,
) -> Result<bool, DaemonError> {
    match action {
        HotkeyAction::Popup => handle_popup_with(cfg, steps),
        HotkeyAction::Editor => handle_editor_with(cfg, steps),
    }
}

/// The parade action: close a running popup if any (toggle), else spawn
/// `<self> popup ...` for the target monitor and minimize browsers.
fn handle_popup_with(cfg: &Config, steps: &mut impl ActionSteps) -> Result<bool, DaemonError> {
    if steps.close_popup() {
        agnosis_core::trace::trace("daemon.toggle_shut");
        return Ok(true);
    }
    let target = steps
        .resolve_target(cfg)
        .map_err(|e| DaemonError::Spawn(format!("no popup target: {e}")))?;
    steps.spawn_popup(&target)?;
    // The popup's window must exist before the next press is answered,
    // or a double-tap takes the spawn branch again and opens a second
    // parade (audit 2026-10-21 §G21).
    steps.wait_for_popup_window();
    let names = cfg.browsers.effective();
    let outcome = steps.minimize_browsers(&names, std::process::id());
    report_minimize(outcome)?;
    Ok(false)
}

/// The editor action: close a running editor if any (toggle), else spawn
/// `<self> editor ...` for the target monitor.
///
/// The editor is a separate process with its own window class and its
/// own PID file, so this never closes the parade popup — and it never
/// minimizes the browsers: that is the parade hotkey's job (roadmap
/// decision D10).
///
/// The spawn is **suppressed while the parade popup is open**
/// (audit 2026-10-23 §F-093): the popup holds the keyboard and its input
/// line is the app's own typing surface, so a capital `D` typed into
/// `Dog.png` fired the `D+Shift`-shaped chord and opened a borderless
/// editor over the parade. The toggle-close above is untouched: an
/// editor that IS running is still closed by the chord.
fn handle_editor_with(cfg: &Config, steps: &mut impl ActionSteps) -> Result<bool, DaemonError> {
    if steps.close_editor() {
        agnosis_core::trace::trace("daemon.editor_toggle_shut");
        return Ok(true);
    }
    if steps.parade_open() {
        agnosis_core::trace::trace("daemon.editor_chord_suppressed: the parade is open");
        return Ok(false);
    }
    let target = steps
        .resolve_target(cfg)
        .map_err(|e| DaemonError::Spawn(format!("no editor target: {e}")))?;
    let args = editor_args_for(&target);
    steps.spawn_editor(&args)?;
    // The editor's window must exist before the next press is answered,
    // or a double-tap takes the spawn branch again and opens a second
    // editor (audit 2026-10-22 §P1-6) — the same guard the parade popup
    // got after its own double-tap bug (audit 2026-10-21 §G21).
    steps.wait_for_editor_window();
    Ok(false)
}

/// The editor's placement for a resolved monitor.
///
/// The editor computes its own window rect (80 % of the monitor,
/// clamped and centred), so the daemon hands over the **monitor**
/// geometry — never a window size.
fn editor_args_for(target: &crate::popup::PopupArgs) -> crate::editor::EditorArgs {
    crate::editor::EditorArgs {
        origin: target.origin,
        size: target.size,
    }
}

/// Interpret a browser-minimize outcome.
///
/// A failure is a real error (the browsers stay visible, so the user must
/// know) and is *also* written to `AGNOSIS_TRACE_FILE` through
/// [`agnosis_core::trace::warn`]: the daemon's stdio is nulled, so the trace file
/// is the only durable channel (audit robustness).
fn report_minimize(outcome: Result<usize, String>) -> Result<(), DaemonError> {
    match outcome {
        Ok(0) => {
            agnosis_core::trace::warn("daemon.minimized_none: browsers still visible?");
            Ok(())
        }
        Ok(n) => {
            agnosis_core::trace::trace(&format!("daemon.minimized {n}"));
            Ok(())
        }
        Err(e) => {
            agnosis_core::trace::warn(&format!("daemon.minimize_failed: {e}"));
            Err(DaemonError::Minimize(e))
        }
    }
}

/// Exact argv tail for the popup spawn, after the executable: `popup
/// --origin X,Y --size W,H --scale F`.
pub fn popup_spawn_argv(args: &crate::popup::PopupArgs) -> Vec<String> {
    vec![
        "popup".to_string(),
        "--origin".to_string(),
        format!("{},{}", args.origin.0, args.origin.1),
        "--size".to_string(),
        format!("{},{}", args.size.0, args.size.1),
        "--scale".to_string(),
        format!("{}", args.scale),
    ]
}

/// Spawn type shared by [`run`] shutdown and detached CLI launches.
fn spawn_popup(args: &crate::popup::PopupArgs) -> Result<(), DaemonError> {
    let exe =
        crate::paths::current_exe().map_err(|e| DaemonError::Spawn(format!("current exe: {e}")))?;
    let mut command = std::process::Command::new(exe);
    command.args(popup_spawn_argv(args));
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
    let child = command
        .spawn()
        .map_err(|e| DaemonError::Spawn(format!("popup spawn: {e}")))?;
    reap_when_it_exits(child);
    agnosis_core::trace::trace("daemon.popup_spawned");
    Ok(())
}

/// Reap a detached child when it exits.
///
/// WHY: the `Child` was dropped without `wait()`, so on Unix every spawn
/// left a `<defunct>` entry behind — one zombie per hotkey press, for as
/// long as the daemon lived (audit 2026-10-20 §3.18). The child is
/// detached and outlives the daemon, so it cannot be waited for in the
/// trigger loop; a short-lived thread waits for it instead.
///
/// The macOS menu bar spawns the editor the same way, so it reaps it
/// the same way (audit 2026-10-21 §G19).
pub(crate) fn reap_when_it_exits(child: std::process::Child) {
    let _ = std::thread::Builder::new()
        .name("agnosis-reap".to_string())
        .spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });
}

/// Exact argv tail for the editor spawn, after the executable: `editor
/// --origin X,Y --size W,H`.
///
/// `--size` carries the **monitor** size: the editor computes its own
/// window rect (80 % of the monitor, clamped and centred) from it, so
/// the daemon never has to know the window size (roadmap §5.1).
pub fn editor_spawn_argv(args: &crate::editor::EditorArgs) -> Vec<String> {
    vec![
        "editor".to_string(),
        "--origin".to_string(),
        format!("{},{}", args.origin.0, args.origin.1),
        "--size".to_string(),
        format!("{},{}", args.size.0, args.size.1),
    ]
}

/// Spawn the editor as a detached child process.
///
/// The editor is a separate process with its own window class and its
/// own PID file, exactly like the popup (roadmap D7), so the parade's
/// frame rate is untouched while the editor is open.
fn spawn_editor(args: &crate::editor::EditorArgs) -> Result<(), DaemonError> {
    let exe =
        crate::paths::current_exe().map_err(|e| DaemonError::Spawn(format!("current exe: {e}")))?;
    let mut command = std::process::Command::new(exe);
    command.args(editor_spawn_argv(args));
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
    let child = command
        .spawn()
        .map_err(|e| DaemonError::Spawn(format!("editor spawn: {e}")))?;
    reap_when_it_exits(child);
    agnosis_core::trace::trace("daemon.editor_spawned");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_args() -> crate::popup::PopupArgs {
        crate::popup::PopupArgs {
            origin: (10, 20),
            size: (1920, 1080),
            scale: 1.5,
        }
    }

    fn sample_editor_args() -> crate::editor::EditorArgs {
        crate::editor::EditorArgs {
            origin: (10, 20),
            size: (1920, 1080),
        }
    }

    /// Records what the daemon asked the platform to do.
    ///
    /// The recorder stands in for the live desktop: the tests drive the
    /// daemon's branching through it, so no test needs a window, a
    /// monitor or a child process (AGENTS.md: deterministic tests).
    #[derive(Default)]
    struct Recorder {
        /// What the popup close reports: true when a popup was running.
        popup_running: bool,
        /// What the editor close reports: true when an editor was running.
        editor_running: bool,
        popup_closed: bool,
        popup_spawned: bool,
        /// Whether the spawn path waited for the popup's window.
        popup_waited: bool,
        minimized: bool,
        editor_closed: bool,
        editor_spawned: bool,
        /// The target the editor spawn was handed.
        editor_args: Option<crate::editor::EditorArgs>,
        /// The error the minimize step reports, when there is one.
        minimize_error: Option<String>,
        /// The browsers the restore put back.
        restored: bool,
        /// Whether the parade is on screen, as the probe sees it.
        parade_running: bool,
    }

    impl ActionSteps for Recorder {
        fn close_popup(&mut self) -> bool {
            self.popup_closed = true;
            self.popup_running
        }

        fn spawn_popup(&mut self, _args: &crate::popup::PopupArgs) -> Result<(), DaemonError> {
            self.popup_spawned = true;
            Ok(())
        }

        fn wait_for_popup_window(&mut self) {
            self.popup_waited = true;
        }

        fn minimize_browsers(&mut self, _names: &[String], _own_pid: u32) -> Result<usize, String> {
            self.minimized = true;
            match &self.minimize_error {
                Some(msg) => Err(msg.clone()),
                None => Ok(1),
            }
        }

        fn restore_browsers(&mut self) {
            self.restored = true;
        }

        fn parade_open(&mut self) -> bool {
            self.parade_running
        }

        fn close_editor(&mut self) -> bool {
            self.editor_closed = true;
            self.editor_running
        }

        fn resolve_target(&mut self, _cfg: &Config) -> Result<crate::popup::PopupArgs, String> {
            Ok(sample_args())
        }

        fn spawn_editor(&mut self, args: &crate::editor::EditorArgs) -> Result<(), DaemonError> {
            self.editor_spawned = true;
            self.editor_args = Some(*args);
            Ok(())
        }
    }

    /// The browsers come back when the parade ends — however it ended
    /// (audit 2026-10-20 §3.7) — **when the user asked for it**
    /// (`[browsers] restore_on_close`). The daemon watches for the window,
    /// so a parade closed with `ESC` restores just like one closed by the
    /// hotkey.
    #[test]
    fn the_browsers_come_back_when_the_parade_ends() {
        let cfg = restoring_config();
        let mut steps = Recorder::default();
        let mut was_open = false;
        // The parade is not on screen yet: nothing to restore.
        restore_browsers_when_the_parade_ends(&cfg, &mut steps, &mut was_open);
        assert!(!steps.restored, "nothing was hidden, nothing to restore");
        assert!(!was_open);

        // The parade is up: still nothing to restore, the browsers are
        // hidden for as long as it lasts.
        steps.parade_running = true;
        restore_browsers_when_the_parade_ends(&cfg, &mut steps, &mut was_open);
        assert!(was_open, "the daemon notices the parade is up");
        assert!(
            !steps.restored,
            "the browsers stay hidden during the parade"
        );

        // The parade ends: the browsers come back.
        steps.parade_running = false;
        restore_browsers_when_the_parade_ends(&cfg, &mut steps, &mut was_open);
        assert!(steps.restored, "the browsers are put back");
        assert!(!was_open);
    }

    /// **The default is the joke**: the browsers stay minimized when the
    /// parade ends, and nothing is put back. The user un-minimizes them
    /// themselves — the old Windows XP "hide your browser" prank only
    /// works if the browser is still hidden.
    #[test]
    fn the_browsers_stay_hidden_by_default() {
        let cfg = Config::default();
        assert!(
            !cfg.browsers.restore_on_close,
            "the restore is off by default"
        );
        let mut steps = Recorder::default();
        let mut was_open = false;
        steps.parade_running = true;
        restore_browsers_when_the_parade_ends(&cfg, &mut steps, &mut was_open);
        steps.parade_running = false;
        restore_browsers_when_the_parade_ends(&cfg, &mut steps, &mut was_open);
        assert!(
            !steps.restored,
            "the browser stays hidden — the joke is the point"
        );
    }

    /// The restore happens exactly once per parade, not on every poll.
    #[test]
    fn the_browsers_are_restored_once() {
        let cfg = restoring_config();
        let mut steps = Recorder::default();
        let mut was_open = false;
        steps.parade_running = true;
        restore_browsers_when_the_parade_ends(&cfg, &mut steps, &mut was_open);
        steps.parade_running = false;
        restore_browsers_when_the_parade_ends(&cfg, &mut steps, &mut was_open);
        assert!(steps.restored);
        // A second poll with the parade still gone must not restore again.
        let mut steps = Recorder {
            restored: false,
            ..Recorder::default()
        };
        restore_browsers_when_the_parade_ends(&cfg, &mut steps, &mut was_open);
        assert!(!steps.restored, "the restore is not repeated");
    }

    /// A config that asks for the restore (`[browsers] restore_on_close`).
    fn restoring_config() -> Config {
        let mut cfg = Config::default();
        cfg.browsers.restore_on_close = true;
        cfg
    }

    #[test]
    fn popup_argv_has_exact_order() {
        assert_eq!(
            popup_spawn_argv(&sample_args()),
            vec![
                "popup",
                "--origin",
                "10,20",
                "--size",
                "1920,1080",
                "--scale",
                "1.5",
            ]
        );
    }

    #[test]
    fn popup_argv_round_trips_through_cli_parser() {
        let argv = popup_spawn_argv(&sample_args());
        let mut full = vec!["agnosis".to_string()];
        full.extend(argv);
        let parsed = crate::cli::parse_args(full).expect("spawned argv must parse");
        let back = parsed.popup_args_for_test();
        assert_eq!(back.origin, (10, 20));
        assert_eq!(back.size, (1920, 1080));
        assert!((back.scale - 1.5).abs() < 1e-9);
    }

    #[test]
    fn popup_argv_supports_negative_origin() {
        let args = crate::popup::PopupArgs {
            origin: (-1920, 0),
            size: (1920, 1080),
            scale: 1.0,
        };
        let argv = popup_spawn_argv(&args);
        assert_eq!(argv[2], "-1920,0");
        let mut full = vec!["agnosis".to_string()];
        full.extend(argv);
        let parsed = crate::cli::parse_args(full).expect("negative origin must parse");
        assert_eq!(parsed.popup_args_for_test().origin, (-1920, 0));
    }

    #[test]
    fn editor_spawn_argv_matches_the_cli_contract() {
        // The daemon's editor argv must be exactly what the hidden
        // `editor` subcommand parses — the same contract the popup argv
        // has (roadmap Stage 4, step 5).
        let argv = editor_spawn_argv(&sample_editor_args());
        assert_eq!(
            argv,
            vec!["editor", "--origin", "10,20", "--size", "1920,1080",]
        );
        let mut full = vec!["agnosis".to_string()];
        full.extend(argv);
        let parsed = crate::cli::parse_args(full).expect("spawned argv must parse");
        match parsed.kind() {
            crate::cli::ParsedKind::Editor(back) => {
                assert_eq!(back.origin, (10, 20));
                assert_eq!(back.size, (1920, 1080));
            }
            other => panic!("expected an editor invocation, got {other:?}"),
        }
    }

    #[test]
    fn editor_argv_supports_negative_origin() {
        // A monitor left of the primary has a negative origin.
        let args = crate::editor::EditorArgs {
            origin: (-1920, -40),
            size: (1920, 1080),
        };
        let argv = editor_spawn_argv(&args);
        assert_eq!(argv[2], "-1920,-40");
        let mut full = vec!["agnosis".to_string()];
        full.extend(argv);
        let parsed = crate::cli::parse_args(full).expect("negative origin must parse");
        match parsed.kind() {
            crate::cli::ParsedKind::Editor(back) => assert_eq!(back.origin, (-1920, -40)),
            other => panic!("expected an editor invocation, got {other:?}"),
        }
    }

    #[test]
    fn error_displays_carry_messages() {
        assert_eq!(
            DaemonError::Spawn("boom".to_string()).to_string(),
            "spawn failed: boom"
        );
        assert_eq!(
            DaemonError::Minimize("no wm".to_string()).to_string(),
            "minimize failed: no wm"
        );
        assert_eq!(
            DaemonError::Control("gone".to_string()).to_string(),
            "control error: gone"
        );
    }

    #[test]
    fn popup_action_toggle_shut_path_returns_true() {
        let cfg = Config::default();
        let mut rec = Recorder {
            popup_running: true,
            ..Recorder::default()
        };
        let toggled = handle_action_with(&cfg, HotkeyAction::Popup, &mut rec)
            .expect("toggle shut must not fail");
        assert!(toggled, "closing a popup must report a toggle-shut trigger");
        assert!(
            rec.popup_closed,
            "the popup action must ask for a popup close"
        );
        assert!(!rec.popup_spawned, "a closed popup must not be replaced");
        assert!(
            !rec.minimized,
            "the toggle-shut path must not minimize browsers"
        );
    }

    #[test]
    fn popup_action_spawn_path_reports_minimize_failure() {
        let cfg = Config::default();
        let mut rec = Recorder {
            minimize_error: Some("no window manager".to_string()),
            ..Recorder::default()
        };
        let err = handle_action_with(&cfg, HotkeyAction::Popup, &mut rec)
            .expect_err("a minimize failure must be a real error");
        match err {
            DaemonError::Minimize(msg) => assert_eq!(msg, "no window manager"),
            other => panic!("expected a minimize error, got {other:?}"),
        }
    }

    #[test]
    fn the_popup_action_is_unchanged() {
        // The regression gate of the multi-chord change: the parade hotkey
        // must still close-or-spawn the popup and minimize the browsers —
        // and must not touch the editor.
        let cfg = Config::default();
        let mut rec = Recorder::default();
        let toggled = handle_action_with(&cfg, HotkeyAction::Popup, &mut rec)
            .expect("the popup action must not fail");
        assert!(!toggled);
        assert!(
            rec.popup_closed,
            "the popup action must ask for a popup close first"
        );
        assert!(rec.popup_spawned, "the popup action must spawn a popup");
        assert!(
            rec.popup_waited,
            "the spawn path must wait for the popup's window, or a double-tap opens a second parade"
        );
        assert!(rec.minimized, "the popup action must minimize the browsers");
        assert!(
            !rec.editor_closed,
            "the popup action must not close the editor"
        );
        assert!(
            !rec.editor_spawned,
            "the popup action must not spawn an editor"
        );
    }

    #[test]
    fn editor_action_closes_a_running_editor() {
        // The toggle: an editor is running, so the chord closes it and
        // spawns nothing.
        let cfg = Config::default();
        let mut rec = Recorder {
            editor_running: true,
            ..Recorder::default()
        };
        let toggled = handle_action_with(&cfg, HotkeyAction::Editor, &mut rec)
            .expect("the editor action must not fail");
        assert!(
            toggled,
            "closing the editor must report a toggle-shut trigger"
        );
        assert!(
            rec.editor_closed,
            "the editor action must ask for an editor close"
        );
        assert!(!rec.editor_spawned, "a closed editor must not be replaced");
    }

    #[test]
    fn editor_action_spawns_when_none_is_running() {
        // The other half of the toggle: no editor runs, so the chord
        // spawns one for the target monitor.
        let cfg = Config::default();
        let mut rec = Recorder::default();
        let toggled = handle_action_with(&cfg, HotkeyAction::Editor, &mut rec)
            .expect("the editor action must not fail");
        assert!(!toggled, "spawning an editor is not a toggle-shut trigger");
        assert!(
            rec.editor_closed,
            "the editor action must ask for an editor close first"
        );
        assert!(rec.editor_spawned, "the editor action must spawn an editor");
        let args = rec.editor_args.expect("the editor spawn must get a target");
        assert_eq!(args.origin, (10, 20));
        assert_eq!(
            args.size,
            (1920, 1080),
            "the editor gets the monitor size, not a window size"
        );
    }

    #[test]
    fn editor_action_never_minimizes_browsers() {
        // Decision D10: minimizing the browsers is the parade hotkey's
        // job. Neither editor branch may do it.
        let cfg = Config::default();
        let mut closing = Recorder {
            editor_running: true,
            ..Recorder::default()
        };
        handle_action_with(&cfg, HotkeyAction::Editor, &mut closing)
            .expect("the editor action must not fail");
        assert!(
            !closing.minimized,
            "closing an editor must not minimize browsers"
        );

        let mut spawning = Recorder::default();
        handle_action_with(&cfg, HotkeyAction::Editor, &mut spawning)
            .expect("the editor action must not fail");
        assert!(
            !spawning.minimized,
            "spawning an editor must not minimize browsers"
        );
    }

    #[test]
    fn editor_action_never_touches_the_parade_popup() {
        // The editor is a separate window class with its own PID file:
        // the editor chord must never close or spawn the parade popup.
        let cfg = Config::default();
        let mut rec = Recorder::default();
        handle_action_with(&cfg, HotkeyAction::Editor, &mut rec)
            .expect("the editor action must not fail");
        assert!(
            !rec.popup_closed,
            "the editor action must not close the popup"
        );
        assert!(
            !rec.popup_spawned,
            "the editor action must not spawn a popup"
        );
    }

    /// A capital `D` typed into the parade's own input line must not
    /// open the editor (audit 2026-10-23 §F-093): the popup holds the
    /// keyboard, so the chord's spawn branch is suppressed while the
    /// popup is open. The toggle-close of a running editor is not
    /// affected.
    #[test]
    fn the_editor_chord_does_not_spawn_over_an_open_parade() {
        let cfg = Config::default();
        let mut rec = Recorder {
            parade_running: true,
            ..Recorder::default()
        };
        let outcome = handle_action_with(&cfg, HotkeyAction::Editor, &mut rec)
            .expect("the suppression must not fail");
        assert!(!outcome, "the suppression is not a toggle-shut trigger");
        assert!(
            rec.editor_closed,
            "the toggle probe still runs (no editor was running)"
        );
        assert!(
            !rec.editor_spawned,
            "an editor must not be spawned over the parade"
        );
    }

    /// A modifiers-only parade spec is reported through its **own**
    /// honest variant, not `UnknownToken` — the spec parsed fine
    /// (audit 2026-10-23 §F-106).
    #[test]
    fn a_keyless_parade_spec_is_a_hotkey_spec_error() {
        let mut cfg = Config::default();
        cfg.hotkey.windows = "Ctrl+Shift".to_string();
        cfg.hotkey.linux = "Ctrl+Shift".to_string();
        cfg.hotkey.macos = "Ctrl+Shift".to_string();
        let err = hotkey_bindings(&cfg).expect_err("a keyless spec must fail");
        match err {
            DaemonError::HotkeySpec(msg) => {
                assert!(
                    msg.contains("has no key, only modifiers"),
                    "the message names the real problem: {msg}"
                );
            }
            other => panic!("expected a keyless-spec error, got {other:?}"),
        }
    }

    /// A graceful stop (`agnosis stop`) with the parade still up puts the
    /// browsers back when the user asked for the restore: with the
    /// daemon gone, nobody would watch for the parade's end
    /// (audit 2026-10-23 §F-107).
    #[test]
    fn a_graceful_stop_restores_the_browsers_while_the_parade_is_open() {
        let cfg = restoring_config();
        let mut steps = Recorder {
            parade_running: true,
            ..Recorder::default()
        };
        restore_on_graceful_stop(&cfg, &mut steps);
        assert!(steps.restored, "the last act restores the browsers");
        // The default (the joke) keeps them hidden: no flag, no restore.
        let mut steps = Recorder {
            parade_running: true,
            ..Recorder::default()
        };
        restore_on_graceful_stop(&Config::default(), &mut steps);
        assert!(!steps.restored);
        // No parade on screen: nothing was hidden with it up, nothing to
        // restore.
        let mut steps = Recorder::default();
        restore_on_graceful_stop(&cfg, &mut steps);
        assert!(!steps.restored);
    }

    /// The exit reason survives from the dying daemon to the starter
    /// that reads it, and the starter's **clear** takes the whole file —
    /// so a stale reason can never answer for a daemon that never wrote
    /// one (audit 2026-10-23 §F-100).
    #[test]
    fn the_exit_reason_round_trips_and_clears() {
        let dir =
            std::env::temp_dir().join(format!("agnosis-daemon-reason-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("exit-reason.txt");
        clear_exit_reason_at(&path);
        assert_eq!(read_exit_reason_from(&path), None, "nothing written yet");
        write_exit_reason_to(&path, "hotkey unavailable: grant Input Monitoring");
        assert_eq!(
            read_exit_reason_from(&path).as_deref(),
            Some("hotkey unavailable: grant Input Monitoring")
        );
        // A blank file is no reason at all.
        std::fs::write(&path, "   \n").expect("write a blank file");
        assert_eq!(read_exit_reason_from(&path), None);
        clear_exit_reason_at(&path);
        assert!(!path.exists(), "the clear removes the file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hotkey_bindings_arm_the_parade_and_the_editor_chord() {
        let cfg = Config::default();
        let bindings = hotkey_bindings(&cfg).expect("the default chords must parse");
        assert_eq!(
            bindings,
            vec![
                (
                    crate::host::hotkey_for_current_platform(&cfg.hotkey).to_string(),
                    HotkeyAction::Popup
                ),
                ("Ctrl+Shift+D".to_string(), HotkeyAction::Editor),
            ]
        );
    }

    #[test]
    fn a_broken_editor_spec_does_not_break_the_parade_chord() {
        let mut cfg = Config::default();
        cfg.hotkey.editor = "D+".to_string();
        let bindings = hotkey_bindings(&cfg).expect("the parade chord must still parse");
        assert_eq!(bindings.len(), 1, "the broken editor chord must be dropped");
        assert_eq!(bindings[0].1, HotkeyAction::Popup);
    }

    #[test]
    fn an_empty_editor_spec_disables_the_editor_chord() {
        let mut cfg = Config::default();
        cfg.hotkey.editor = String::new();
        let bindings = hotkey_bindings(&cfg).expect("the parade chord must still parse");
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].1, HotkeyAction::Popup);
    }

    #[test]
    fn a_broken_parade_spec_is_an_error() {
        let mut cfg = Config::default();
        cfg.hotkey.windows = "A+".to_string();
        cfg.hotkey.linux = "A+".to_string();
        cfg.hotkey.macos = "A+".to_string();
        let err = hotkey_bindings(&cfg).expect_err("a broken parade chord must fail");
        assert!(matches!(err, DaemonError::Hotkey(HotkeyError::Spec(_))));
    }

    #[test]
    fn minimize_failure_is_warned_and_reported() {
        let err = report_minimize(Err("boom".to_string())).expect_err("failure must surface");
        assert!(matches!(err, DaemonError::Minimize(ref m) if m == "boom"));
        // Zero browsers minimized is a warning, not an error.
        assert!(report_minimize(Ok(0)).is_ok());
        assert!(report_minimize(Ok(3)).is_ok());
    }

    #[test]
    fn popup_action_spawn_path_minimizes_without_failing() {
        // The test drives the **pure** path only — the recorder that
        // builds the popup's argv and the browser matcher with a name
        // that matches nothing. It never calls `window::close_popup()`:
        // that closed the user's live parade, and the early return it
        // guarded left the test with **no assertion at all** (audit
        // 2026-10-21 §G10).
        let mut cfg = Config::default();
        cfg.browsers.names = vec!["no-such-browser-xyz".to_string()];
        let target = crate::cli::resolve_popup_target(&cfg).expect("resolve on Windows");
        assert!(target.size.0 > 0 && target.size.1 > 0);
        let argv = popup_spawn_argv(&target);
        assert_eq!(argv[0], "popup");
        let parsed =
            crate::cli::parse_args(std::iter::once("agnosis".to_string()).chain(argv).collect())
                .expect("spawned argv must parse");
        assert_eq!(parsed.popup_args_for_test().origin, target.origin);
        let own = std::process::id();
        let minimized =
            crate::platform::minimize::minimize_browsers(&cfg.browsers.effective(), own)
                .expect("minimize with no browsers must not fail");
        assert_eq!(minimized, 0, "unknown browser minimizes nothing");
    }

    /// Confirm the minimize-failure warning really reaches
    /// `AGNOSIS_TRACE_FILE` (audit robustness): the daemon's stdio is nulled,
    /// so the trace file is the only durable channel.
    ///
    /// The trace module caches the log path on first use, so the check runs
    /// in a child test process started with the variable already set.
    #[test]
    fn minimize_failure_reaches_trace_file() {
        if std::env::var_os("AGNOSIS_TRACE_CHILD").is_some() {
            let _ = report_minimize(Err("no window manager".to_string()));
            return;
        }
        let dir = std::env::temp_dir().join(format!("agnosis-daemon-trace-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let log = dir.join("trace.log");
        let _ = std::fs::remove_file(&log);
        let exe = std::env::current_exe().expect("test binary path");
        let status = std::process::Command::new(exe)
            .args([
                "--exact",
                "daemon::tests::minimize_failure_reaches_trace_file",
                "--test-threads=1",
            ])
            .env("AGNOSIS_TRACE_CHILD", "1")
            .env("AGNOSIS_TRACE_FILE", &log)
            .status()
            .expect("child test process must start");
        assert!(status.success(), "child test process failed: {status}");
        let text = std::fs::read_to_string(&log).unwrap_or_default();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(
            text.contains("daemon.minimize_failed"),
            "trace file must record the minimize failure, got {text:?}"
        );
    }
}
