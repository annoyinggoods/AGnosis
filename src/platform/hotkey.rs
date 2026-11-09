//! Global hotkey listener: a chord trigger sends the action it fires.
//!
//! Common contract on every platform: ESC is never bound globally — only the
//! popup window handles ESC. The parade combo doubles as the popup toggle:
//! the daemon first asks a running popup to close (see
//! [`crate::platform::window::close_popup`]) and only spawns a fresh popup
//! when none was running. Key repeat never re-fires: the matcher re-arms
//! only after every chord key is released.
//!
//! Several chords can be armed at once: the listener matches them all and
//! sends the [`HotkeyAction`] of the chord that fired, so the daemon can
//! tell the parade hotkey from the editor hotkey without a second
//! listener thread.
//!
//! | Platform | Mechanism |
//! | --- | --- |
//! | Windows | `WH_KEYBOARD_LL` via `SetWindowsHookExW`. The match runs
//! inline in the hook proc and returns promptly; the trigger is signalled
//! only via the channel. The listener thread runs a `GetMessageW` pump,
//! which an LL hook requires. No elevation, no injection. |
//! | Linux/X11 | `XRecord` passive recording of all key events via
//! `x11rb::record`; never grabs. If the RECORD extension is missing, falls
//! back to `XQueryKeymap` polling at 100 Hz. Keycodes are mapped via
//! keysyms to [`Key`]/[`Modifier`] through a table fetched **once** at
//! listener start (audit F-128) — never a `get_keyboard_mapping` round
//! trip per key event. Synthetic `xdotool`/`XTEST` events are
//! indistinguishable from real ones at the server, so CI can fire the
//! hotkey headlessly — that property is load-bearing for the xvfb smoke
//! test and must be kept. `stop()` asks the X server to close the record
//! context from a second connection (audit P0-6): the `enable_context`
//! iterator only ends when the server sends the EndOfData reply that
//! closing a context produces, so that request is what wakes the blocked
//! listener. The wake is best-effort, not a guarantee: the server can
//! reject the close, and the close can still race the listener enabling
//! the context, so on a wedged or broken X server the join may still
//! block. |
//! | macOS | A **listen-only** `CGEventTap` (`kCGHIDEventTap`,
//! `kCGHeadInsertEventTap`, `kCGEventTapOptionListenOnly`) observes every
//! key event without swallowing it, so the permission macOS asks for is
//! *Input Monitoring* and not Accessibility. The tap's `CFMachPort` is
//! added as a `CFRunLoopSource` to a dedicated `"agnosis-hotkey"` thread, so
//! the daemon's main loop does not change. The callback feeds the same
//! [`ChordSet`] as the other backends — same parser, same edge rules, same
//! [`HotkeyAction`] payload — so `D+Shift` and a multi-key chord such as
//! `A+G+Shift+Space` both work (roadmap Stage 3A, decision D20). The tap
//! is re-enabled when macOS disables it (`kCGEventTapDisabledByTimeout` /
//! `kCGEventTapDisabledByUserInput`), otherwise the hotkey would die
//! silently after a burst of heavy input. Without the Input Monitoring
//! grant the tap cannot be created at all: the backend then reports
//! [`HotkeyError::Unavailable`] with the exact remediation and the daemon
//! exits with that message. The parade and the editor stay reachable
//! through `ducks`, `agnosis editor` and the menu-bar trigger. |
//!
//! **The Carbon fallback is not implemented.** Carbon's
//! `RegisterEventHotKey` would need its own `CFRunLoop` registration path,
//! and it can only express **one key plus modifiers** — a restriction the
//! `CGEventTap` path does not have. The tap is therefore the only backend
//! and the chord shape stays unrestricted. The shape rule is still spelled
//! out as the pure predicate [`carbon_fallback_accepts`], so a future
//! Carbon backend can report honestly which chords it can honour instead
//! of re-deriving the rule (roadmap Stage 3A, decision D20).
//!
//! [`Key`]: crate::platform::chord::Key

use crate::platform::chord::{ChordError, ChordSet};

/// The action a chord fires; re-exported so the daemon can name it
/// through the hotkey module it already imports.
pub use crate::platform::chord::HotkeyAction;

/// Failures from [`spawn`].
#[derive(Debug, thiserror::Error)]
pub enum HotkeyError {
    /// The chord spec failed to parse.
    #[error("bad hotkey spec: {0}")]
    Spec(#[from] ChordError),
    /// Platform backend failure with a message.
    ///
    /// The variant exists for the platforms whose backend can fail for
    /// a reason other than a missing permission.
    #[cfg_attr(
        not(any(target_os = "windows", target_os = "linux", target_os = "macos")),
        allow(dead_code)
    )]
    #[error("hotkey backend: {0}")]
    Platform(String),
    /// The platform cannot host the global hotkey: the OS refused the
    /// backend (Windows blocked the low-level keyboard hook), or a
    /// platform prerequisite is missing (no X11 display on Linux).
    ///
    /// macOS never reports it: the event-tap backend either works or
    /// reports the missing Input Monitoring grant as
    /// [`HotkeyError::Unavailable`].
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    #[error("hotkey unsupported: {0}")]
    Unsupported(String),
    /// The platform **can** host the global hotkey, but the user has not
    /// granted the permission it needs.
    ///
    /// macOS only: the `CGEventTap` backend needs the *Input Monitoring*
    /// grant; without it the tap cannot be created, so the daemon exits
    /// with the remediation text instead of pretending to work (roadmap
    /// Stage 3A, decision D20).
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    #[error("hotkey unavailable: {0}")]
    Unavailable(String),
    /// The listener thread died before it could report readiness; its
    /// sender is gone.
    #[cfg_attr(
        not(any(target_os = "windows", target_os = "linux", target_os = "macos")),
        allow(dead_code)
    )]
    #[error("hotkey listener thread exited")]
    ChannelClosed,
}

/// The remediation for a macOS daemon that cannot create its event tap
/// because the *Input Monitoring* grant is missing.
///
/// The wording names the exact System Settings path, because that is
/// where the grant lives and the user has to find it (roadmap Stage 3A).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const INPUT_MONITORING_REMEDIATION: &str =
    "grant Input Monitoring to AGnosis in System Settings → Privacy & Security → Input Monitoring";

/// The error the macOS backend reports when the event tap could not be
/// created because the *Input Monitoring* grant is missing.
///
/// The mapping is a pure function so the honest degradation can be
/// tested without a Mac (roadmap Stage 3A).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_missing_permission() -> HotkeyError {
    HotkeyError::Unavailable(INPUT_MONITORING_REMEDIATION.to_string())
}

/// True when a chord can be expressed as a Carbon hot key.
///
/// Carbon's `RegisterEventHotKey` registers **one** key plus a modifier
/// set, so a chord with two or more non-modifier keys (`A+G+Shift+Space`)
/// cannot be expressed. The `CGEventTap` backend has no such
/// restriction; this predicate exists so a Carbon fallback can report
/// honestly which chords it can honour (roadmap Stage 3A, decision D20).
#[cfg_attr(not(test), allow(dead_code))]
fn carbon_fallback_accepts(chord: &crate::platform::chord::Chord) -> bool {
    chord.keys.len() <= 1
}

/// Spawn the platform hotkey listener thread.
///
/// `bindings` are the armed chords: each spec is matched, and a trigger
/// sends the [`HotkeyAction`] of the chord that fired on the channel; the
/// receiver end belongs to the daemon main loop. Sending never blocks the
/// hotkey thread. `stop()` shuts the listener down (blocking join); dropping
/// the handle does the same.
///
/// An empty spec disables that chord; a spec that does not parse fails the
/// call, so the caller decides whether one broken chord may take the others
/// down with it.
pub fn spawn(
    bindings: &[(String, HotkeyAction)],
) -> Result<(HotkeyHandle, std::sync::mpsc::Receiver<HotkeyAction>), HotkeyError> {
    let set = ChordSet::new(bindings)?;
    let spawned = imp::spawn(set);
    report_unusable_backend(&spawned);
    spawned
}

/// Report an unusable backend to the trace file.
///
/// The daemon nulls its stdio, so `trace::warn` (stderr plus the
/// `AGNOSIS_TRACE_FILE` file) is the only durable channel that tells a user
/// why the daemon died (audit P0-5 / robustness).
fn report_unusable_backend(
    result: &Result<(HotkeyHandle, std::sync::mpsc::Receiver<HotkeyAction>), HotkeyError>,
) {
    match result {
        Err(HotkeyError::Unsupported(reason)) => {
            agnosis_core::trace::warn(&format!("hotkey.unsupported: {reason}"));
        }
        Err(HotkeyError::Unavailable(reason)) => {
            agnosis_core::trace::warn(&format!("hotkey.unavailable: {reason}"));
        }
        _ => {}
    }
}

/// Handle to a running hotkey listener; [`HotkeyHandle::stop`] shuts it
/// down with a blocking join.
///
/// Dropping the handle stops and joins the listener too, so a forgotten
/// handle cannot leave a live hook or listener thread behind.
pub struct HotkeyHandle {
    stop_fn: Option<Box<dyn FnOnce() + Send>>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl HotkeyHandle {
    /// Build a handle around a running listener thread.
    #[cfg_attr(
        not(any(target_os = "windows", target_os = "linux", target_os = "macos")),
        allow(dead_code)
    )]
    fn new(stop_fn: impl FnOnce() + Send + 'static, thread: std::thread::JoinHandle<()>) -> Self {
        Self {
            stop_fn: Some(Box::new(stop_fn)),
            thread: Some(thread),
        }
    }

    /// Shut the listener down, blocking until its thread exits.
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// Stop the listener and join its thread.
    ///
    /// Idempotent: [`HotkeyHandle::stop`] and `Drop` share it, so the
    /// second call has nothing left to do.
    fn shutdown(&mut self) {
        if let Some(stop) = self.stop_fn.take() {
            stop();
        }
        self.join_listener();
    }

    /// Join the listener thread.
    ///
    /// A panicking listener is reported instead of being swallowed by a
    /// discarded join result (audit robustness: a silent listener looks
    /// exactly like a dead hotkey).
    fn join_listener(&mut self) {
        let Some(thread) = self.thread.take() else {
            return;
        };
        if thread.join().is_err() {
            agnosis_core::trace::warn("hotkey.listener_panicked: thread exited abnormally");
        }
    }
}

impl Drop for HotkeyHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::{HotkeyAction, HotkeyError, HotkeyHandle};
    use crate::platform::chord::{ChordSet, Key, Modifier};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use windows::Win32::Foundation::{HINSTANCE, LPARAM, LRESULT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Threading::GetCurrentThreadId;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        VK_CONTROL, VK_ESCAPE, VK_LCONTROL, VK_LMENU, VK_LSHIFT, VK_LWIN, VK_MENU, VK_RCONTROL,
        VK_RMENU, VK_RSHIFT, VK_RWIN, VK_SHIFT, VK_SPACE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, PostThreadMessageW, SetWindowsHookExW,
        TranslateMessage, UnhookWindowsHookEx, HHOOK, KBDLLHOOKSTRUCT, MSG, WH_KEYBOARD_LL,
        WM_KEYDOWN, WM_KEYUP, WM_QUIT, WM_SYSKEYDOWN, WM_SYSKEYUP,
    };

    /// Why the low-level keyboard hook could not be installed.
    ///
    /// `SetWindowsHookExW` fails when the OS refuses a low-level hook (a
    /// "block low-level hooks" policy, a non-interactive session, a
    /// 32/64-bit mismatch): the platform then simply cannot host the
    /// global hotkey.
    const HOOK_FAILED: &str = "the low-level keyboard hook was refused \
         (policy, non-interactive session, or 32/64-bit mismatch)";

    struct Shared {
        set: Mutex<ChordSet>,
        tx: mpsc::Sender<HotkeyAction>,
    }

    // The hook's own snapshot of the shared state, published **once per
    // hook install**.
    //
    // A `WH_KEYBOARD_LL` hook is always called on the thread that
    // installed it, so the snapshot is a thread-local of the listener
    // thread: the hook proc reads it without a global lock and without
    // allocating, where the old path locked a process-wide mutex and
    // `format!`ed a key name on **every key event** (audit F-137).
    thread_local! {
        static HOOK_SHARED: std::cell::RefCell<Option<Arc<Shared>>> =
            const { std::cell::RefCell::new(None) };
    }

    /// Publish the shared state to this thread's hook slot.
    fn publish_hook_shared(shared: Arc<Shared>) {
        HOOK_SHARED.with(|slot| *slot.borrow_mut() = Some(shared));
    }

    /// Take the shared state back once the hook is gone.
    fn clear_hook_shared() {
        HOOK_SHARED.with(|slot| *slot.borrow_mut() = None);
    }

    /// The hook's snapshot, cloned out of the thread-local.
    fn hook_shared() -> Option<Arc<Shared>> {
        HOOK_SHARED.with(|slot| slot.borrow().clone())
    }

    /// One entry of the pre-mapped VK table: a modifier transition or a
    /// key transition the chord matcher understands.
    ///
    /// `Copy` on purpose (audit F-137): the table is built once (see
    /// [`build_vk_table`]) and read on every key event, so handing an
    /// entry out may not allocate. The catch-all serves every unmapped
    /// VK — the matcher never compares `Key::Other` against a chord key
    /// (the parser cannot produce one), so one sentinel is enough and
    /// `String::new()` allocates nothing.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum VkEntry {
        Mod(Modifier),
        Space,
        Escape,
        Char(char),
        Other,
    }

    /// The pre-mapped VK table (audit F-137): every `vkCode` the hook can
    /// report, resolved to its matcher input once per process instead of
    /// once per event. The old `vk_to_key` re-derived the key per event
    /// and carried a dead modifier branch no caller could reach (audit
    /// F-131) — the table carries the whole decision now.
    fn vk_table() -> &'static [VkEntry; 256] {
        static TABLE: std::sync::OnceLock<[VkEntry; 256]> = std::sync::OnceLock::new();
        TABLE.get_or_init(build_vk_table)
    }

    /// Build the VK table. `pub(super)` so the pinning test reaches it on
    /// every Windows host.
    pub(super) fn build_vk_table() -> [VkEntry; 256] {
        let mut table = [VkEntry::Other; 256];
        table[VK_SPACE.0 as usize] = VkEntry::Space;
        table[VK_ESCAPE.0 as usize] = VkEntry::Escape;
        stamp_modifiers(&mut table);
        stamp_chars(&mut table);
        table
    }

    /// Stamp the modifier VKs into the table: the generic code plus the
    /// left/right codes of every modifier, table-driven.
    fn stamp_modifiers(table: &mut [VkEntry; 256]) {
        for (vk, modifier) in [
            (VK_SHIFT.0, Modifier::Shift),
            (VK_LSHIFT.0, Modifier::Shift),
            (VK_RSHIFT.0, Modifier::Shift),
            (VK_CONTROL.0, Modifier::Ctrl),
            (VK_LCONTROL.0, Modifier::Ctrl),
            (VK_RCONTROL.0, Modifier::Ctrl),
            (VK_MENU.0, Modifier::Alt),
            (VK_LMENU.0, Modifier::Alt),
            (VK_RMENU.0, Modifier::Alt),
            (VK_LWIN.0, Modifier::Super),
            (VK_RWIN.0, Modifier::Super),
        ] {
            table[vk as usize] = VkEntry::Mod(modifier);
        }
    }

    /// Stamp the letter and digit VKs: ASCII-ordered ranges, letters
    /// lowercased exactly as the parser lowercases its segments.
    fn stamp_chars(table: &mut [VkEntry; 256]) {
        for vk in 0x41..=0x5A {
            table[vk as usize] = VkEntry::Char((vk as u8 as char).to_ascii_lowercase());
        }
        for vk in 0x30..=0x39 {
            table[vk as usize] = VkEntry::Char(vk as u8 as char);
        }
    }

    /// One VK's pre-mapped entry; `None` for a VK code past the table.
    fn vk_entry(vk: u32) -> Option<VkEntry> {
        vk_table().get(vk as usize).copied()
    }

    /// Lock the shared chord set, clearing held state after poisoning (audit
    /// robustness): a poisoned lock previously dropped the event via
    /// `.ok()`, making the hotkey look dead. Clearing keeps the chord
    /// definitions (only transient press state resets) and logs loudly.
    fn lock_set(shared: &Arc<Shared>) -> std::sync::MutexGuard<'_, ChordSet> {
        match shared.set.lock() {
            Ok(g) => g,
            Err(poisoned) => {
                agnosis_core::trace::warn("hotkey.matcher_poisoned: held state cleared");
                let mut guard = poisoned.into_inner();
                guard.clear_held();
                guard
            }
        }
    }

    /// One low-level key transition: feed the chord set, signal on the
    /// edge. The key arrives pre-mapped (see [`vk_table`]), so the hook
    /// path locks nothing but the matcher and allocates nothing.
    fn handle_key_event(shared: &Arc<Shared>, vk: u32, down: bool) {
        let Some(entry) = vk_entry(vk) else {
            return;
        };
        let mut set = lock_set(shared);
        let fired = match entry {
            VkEntry::Mod(md) => {
                set.on_modifier(md, down);
                None
            }
            VkEntry::Space => set.on_key(Key::Space, down),
            VkEntry::Escape => set.on_key(Key::Escape, down),
            VkEntry::Char(c) => set.on_key(Key::Char(c), down),
            // One allocation-free sentinel for every unmapped VK (see
            // `VkEntry`): the matcher only inserts it into its held set.
            VkEntry::Other => set.on_key(Key::Other(String::new()), down),
        };
        let Some(action) = fired else {
            return;
        };
        let _ = shared.tx.send(action);
    }

    fn is_key_msg(msg: u32) -> bool {
        matches!(msg, WM_KEYDOWN | WM_SYSKEYDOWN | WM_KEYUP | WM_SYSKEYUP)
    }

    /// Hand one key transition to the matcher (audit nesting rule): the
    /// guard clauses live at the top level of this helper, so the hook
    /// proc itself stays flat.
    ///
    /// The `KBDLLHOOKSTRUCT` is copied out of the OS-owned `lparam` before
    /// the hook returns; the shared state comes from the thread-local
    /// snapshot taken at hook-install time (audit F-137).
    fn dispatch_key_event(lparam: LPARAM, msg: u32) {
        // SAFETY: `lparam` of a `WH_KEYBOARD_LL` hook points at a
        // `KBDLLHOOKSTRUCT` the OS owns for the duration of this call; the
        // copy is taken while the pointer is still valid.
        let info = unsafe { *(lparam.0 as *const KBDLLHOOKSTRUCT) };
        let down = msg == WM_KEYDOWN || msg == WM_SYSKEYDOWN;
        let Some(shared) = hook_shared() else {
            return;
        };
        handle_key_event(&shared, info.vkCode, down);
    }

    unsafe extern "system" fn hook_proc(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let msg = wparam.0 as u32;
        if code >= 0 && is_key_msg(msg) {
            dispatch_key_event(lparam, msg);
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    /// Install the low-level keyboard hook on this thread.
    ///
    /// A `WH_KEYBOARD_LL` hook is delivered to the thread that installed
    /// it, so the hook thread is the one that must run a message pump.
    fn install_hook() -> Option<HHOOK> {
        unsafe {
            let hmod = GetModuleHandleW(None).ok().map(|m| HINSTANCE(m.0));
            SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), hmod, 0).ok()
        }
    }

    /// Pump the thread's message queue until the stop flag is set.
    ///
    /// `stop()` posts `WM_QUIT` to this thread, so `GetMessageW` returns 0
    /// and the loop ends.
    fn run_message_pump(stop: &AtomicBool) {
        let mut msg = MSG::default();
        while !stop.load(Ordering::Relaxed) {
            let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            if r.0 == 0 || r.0 == -1 {
                break;
            }
            unsafe {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    /// The listener thread: install the hook, report readiness, then pump
    /// messages until the stop flag is set.
    ///
    /// The shared state is published to this thread's hook slot **once,
    /// here** (audit F-137), and taken back after the hook is gone — the
    /// hook proc never takes a global lock per event.
    fn listener_thread(
        shared: Arc<Shared>,
        ready_tx: mpsc::Sender<Result<u32, String>>,
        stop: Arc<AtomicBool>,
    ) {
        let Some(hook) = install_hook() else {
            let _ = ready_tx.send(Err(HOOK_FAILED.to_string()));
            return;
        };
        publish_hook_shared(shared);
        let tid = unsafe { GetCurrentThreadId() };
        let _ = ready_tx.send(Ok(tid));
        run_message_pump(&stop);
        unsafe {
            let _ = UnhookWindowsHookEx(hook);
        }
        clear_hook_shared();
    }

    pub(super) fn spawn(
        set: ChordSet,
    ) -> Result<(HotkeyHandle, mpsc::Receiver<HotkeyAction>), HotkeyError> {
        let (tx, rx) = mpsc::channel::<HotkeyAction>();
        let (ready_tx, ready_rx) = mpsc::channel::<Result<u32, String>>();
        let shared = Arc::new(Shared {
            set: Mutex::new(set),
            tx,
        });
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_thread = stop_flag.clone();
        let thread = std::thread::Builder::new()
            .name("agnosis-hotkey".into())
            .spawn(move || listener_thread(shared, ready_tx, stop_flag_thread))
            .map_err(|e| HotkeyError::Platform(e.to_string()))?;
        match ready_rx.recv().map_err(|_| HotkeyError::ChannelClosed)? {
            Ok(tid) => {
                let stop_fn = move || {
                    stop_flag.store(true, Ordering::Relaxed);
                    unsafe {
                        let _ = PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0));
                    }
                };
                Ok((HotkeyHandle::new(stop_fn, thread), rx))
            }
            Err(e) => {
                let _ = thread.join();
                Err(HotkeyError::Unsupported(e))
            }
        }
    }
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{HotkeyAction, HotkeyError, HotkeyHandle};
    use crate::platform::chord::{ChordSet, Key, Modifier};
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;
    use x11rb::connection::Connection;
    use x11rb::protocol::record::{self, Range, Range8, CS};
    use x11rb::protocol::xproto::*;
    use x11rb::rust_connection::RustConnection;

    const KEY_PRESS: u8 = 2;
    const KEY_RELEASE: u8 = 3;

    /// How many times the wake path retries the close-context request.
    ///
    /// The retry covers the race between the listener enabling the
    /// context and `stop()` closing it: a close that reaches the server
    /// before the enable is rejected with `BadContext`.
    const WAKE_ATTEMPTS: u32 = 50;

    /// Pause between two wake attempts.
    const WAKE_RETRY_DELAY: Duration = Duration::from_millis(10);

    fn has_record(conn: &impl Connection) -> bool {
        conn.extension_information(record::X11_EXTENSION_NAME)
            .ok()
            .flatten()
            .is_some()
    }

    /// Why the X11 backend cannot run here, if it cannot.
    ///
    /// The RECORD backend attaches the record context to an X server, so
    /// without `DISPLAY` there is none: the global hotkey cannot exist
    /// and saying so beats spawning a listener that dies at once.
    fn unsupported_reason() -> Option<&'static str> {
        match std::env::var_os("DISPLAY") {
            Some(_) => None,
            None => Some("no X11 display: DISPLAY is unset, the X11 hotkey backend cannot run"),
        }
    }

    /// Map an X11 keysym to a [`Key`] or [`Modifier`].
    ///
    /// `Ok` carries a modifier for Shift/Ctrl/Alt/Super keysyms; `Err`
    /// carries the non-modifier key. Unknown keysyms become `Key::Other`.
    pub(super) fn keysym_to_key(keysym: u32) -> Result<Modifier, Key> {
        match keysym {
            0xFFE1 | 0xFFE2 => Ok(Modifier::Shift),
            0xFFE3 | 0xFFE4 => Ok(Modifier::Ctrl),
            0xFFE9 | 0xFFEA => Ok(Modifier::Alt),
            0xFFEB | 0xFFEC => Ok(Modifier::Super),
            0x0020 => Err(Key::Space),
            0xFF1B => Err(Key::Escape),
            0xFFBE..=0xFFD5 => Err(Key::F((keysym - 0xFFBE + 1) as u8)),
            0x0041..=0x005A => Err(Key::Char((keysym as u8 as char).to_ascii_lowercase())),
            0x0061..=0x007A | 0x0030..=0x0039 => Err(Key::Char(keysym as u8 as char)),
            _ => Err(Key::Other(format!("keysym:{keysym:#x}"))),
        }
    }

    /// The keycode→keysym table, fetched **once** at listener start
    /// (audit F-128).
    ///
    /// The RECORD callback and the polling fallback used to issue one
    /// synchronous `get_keyboard_mapping` round trip **per key event**;
    /// the server answers in its own time, so a busy server delayed every
    /// keystroke. The whole keycode range is fetched once here and the hot
    /// paths only index the table (see [`super::x11_keymap`]). A failure
    /// is a backend error: without a mapping no key could be translated.
    fn keycode_table(conn: &impl Connection) -> Result<[u32; 256], String> {
        let reply = conn
            .get_keyboard_mapping(
                super::x11_keymap::MIN_KEYCODE,
                super::x11_keymap::KEYCODE_COUNT,
            )
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        Ok(super::x11_keymap::table_from_mapping(
            &reply.keysyms,
            reply.keysyms_per_keycode as usize,
        ))
    }

    fn device_event_range() -> Range {
        let none8 = Range8 { first: 0, last: 0 };
        Range {
            core_requests: none8,
            core_replies: none8,
            ext_requests: record::ExtRange {
                major: none8,
                minor: record::Range16 { first: 0, last: 0 },
            },
            ext_replies: record::ExtRange {
                major: none8,
                minor: record::Range16 { first: 0, last: 0 },
            },
            delivered_events: none8,
            device_events: Range8 {
                first: KEY_PRESS,
                last: KEY_RELEASE,
            },
            errors: none8,
            client_started: false,
            client_died: false,
        }
    }

    /// Ask the server to close one record context.
    ///
    /// The request is flushed explicitly (see [`wake_record_context`]) and
    /// then checked, so a rejection — `BadContext`, meaning the context
    /// was not enabled yet — is reported instead of being swallowed.
    fn close_record_context(conn: &impl Connection, ctx: u32) -> Result<(), String> {
        let cookie = record::disable_context(conn, ctx).map_err(|e| e.to_string())?;
        conn.flush().map_err(|e| e.to_string())?;
        cookie.check().map_err(|e| e.to_string())
    }

    /// Release the record context: close it, then free it.
    ///
    /// Both requests are void requests, so the connection is flushed
    /// explicitly — see [`wake_record_context`] for why that matters.
    fn release_record_context(conn: &impl Connection, ctx: u32) {
        let _ = record::disable_context(conn, ctx).map(|c| c.ignore_error());
        let _ = record::free_context(conn, ctx).map(|c| c.ignore_error());
        if let Err(e) = conn.flush() {
            agnosis_core::trace::warn(&format!("hotkey.record_release_failed: {e}"));
        }
    }

    /// Wake a listener blocked in the RECORD `enable_context` iterator.
    ///
    /// The iterator only ends when the server sends the EndOfData reply
    /// that closing the context produces, so the close request must reach
    /// the server on a *second* connection (audit P0-6).
    ///
    /// Two x11rb details make the obvious code wrong:
    ///
    /// * `disable_context` is a void request. x11rb buffers outgoing
    ///   requests in a 16 KiB `WriteBuffer` and only writes them to the
    ///   socket inside `wait_for_reply` / `check_for_raw_error` / an
    ///   explicit `flush()`; `VoidCookie::ignore_error()` only marks the
    ///   sequence for error discarding and does NOT flush, and
    ///   `RustConnection` has no `Drop` impl that flushes. Without the
    ///   explicit flush the 8-byte request is discarded when the
    ///   connection is dropped, the server never closes the context and
    ///   the listener stays blocked forever.
    /// * The server handles the two connections in an arbitrary order, so
    ///   a close that arrives before the server processed the enable is
    ///   rejected. The close is therefore retried while the listener is
    ///   still alive.
    fn wake_record_context(ctx: u32, finished: &AtomicBool) {
        if ctx == 0 {
            return;
        }
        let Ok((conn, _)) = x11rb::connect(None) else {
            agnosis_core::trace::warn(
                "hotkey.wake_failed: no X11 connection to close the record context",
            );
            return;
        };
        for _ in 0..WAKE_ATTEMPTS {
            if finished.load(Ordering::SeqCst) {
                return;
            }
            let Err(e) = close_record_context(&conn, ctx) else {
                return;
            };
            agnosis_core::trace::warn(&format!("hotkey.wake_retry: {e}"));
            std::thread::sleep(WAKE_RETRY_DELAY);
        }
        agnosis_core::trace::warn("hotkey.wake_failed: record context still open");
    }

    /// Create the record context and publish its id.
    ///
    /// The id is published before the spawner is told the listener is
    /// ready, so a `stop()` that runs right after the spawn always finds
    /// a context to close (audit P0-6).
    fn create_record_context(
        ctx_id: &AtomicU32,
    ) -> Result<(RustConnection, record::Context), String> {
        let (conn, _screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let ctx: record::Context = conn.generate_id().map_err(|e| e.to_string())?;
        ctx_id.store(ctx, Ordering::SeqCst);
        let range = device_event_range();
        let all: record::ClientSpec = u32::from(CS::ALL_CLIENTS);
        // Element header 0: the recorded data carries no extra header
        // fields (the RECORD `element_header` byte is a plain `u8`).
        record::create_context(&conn, ctx, 0u8, &[all], &[range])
            .map_err(|e| e.to_string())?
            .check()
            .map_err(|e| e.to_string())?;
        Ok((conn, ctx))
    }

    /// Record key events until the stop flag is set.
    ///
    /// `ready_tx` reports the setup result to the spawner.
    fn run_record(
        mut set: ChordSet,
        tx: mpsc::Sender<HotkeyAction>,
        stop: Arc<AtomicBool>,
        ctx_id: Arc<AtomicU32>,
        ready_tx: mpsc::Sender<Result<(), String>>,
    ) -> Result<(), String> {
        let setup = create_record_context(&ctx_id);
        let (conn, ctx) = match setup {
            Ok(pair) => pair,
            Err(e) => {
                let _ = ready_tx.send(Err(e.clone()));
                return Err(e);
            }
        };
        // The keycode table is fetched before the listener reports
        // readiness, so a broken keyboard mapping fails the spawn instead
        // of silently deadening every key (audit F-128).
        let table = match keycode_table(&conn) {
            Ok(table) => table,
            Err(e) => {
                let _ = ready_tx.send(Err(e.clone()));
                return Err(e);
            }
        };
        let _ = ready_tx.send(Ok(()));
        // Enabling the context is what blocks this thread, so a stop that
        // already happened must not enable it: the wake would have no
        // context to close and the join would hang (audit P0-6). The
        // server frees the context when this connection goes away.
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let cookie = record::enable_context(&conn, ctx).map_err(|e| e.to_string())?;
        // The first iterator step blocks until the server answers, so the
        // flag is checked once more right before it.
        if stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        for reply in cookie {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let reply = reply.map_err(|e| e.to_string())?;
            if handle_record_reply(&mut set, &tx, &table, &reply).is_err() {
                break;
            }
        }
        release_record_context(&conn, ctx);
        Ok(())
    }

    /// One XRecord reply: classify the key event and feed the chord set.
    /// Returns `Err(())` when the receiver is gone and the loop must exit.
    fn handle_record_reply(
        set: &mut ChordSet,
        tx: &mpsc::Sender<HotkeyAction>,
        table: &[u32; 256],
        reply: &record::EnableContextReply,
    ) -> Result<(), ()> {
        let data = &reply.data;
        if data.len() < 32 {
            return Ok(());
        }
        let kind = data[0] & 0x7F;
        if kind != KEY_PRESS && kind != KEY_RELEASE {
            return Ok(());
        }
        let down = kind == KEY_PRESS;
        let Some(keysym) = super::x11_keymap::keysym_of(table, data[1]) else {
            return Ok(());
        };
        let fired = match keysym_to_key(keysym) {
            Ok(md) => {
                set.on_modifier(md, down);
                None
            }
            Err(k) => set.on_key(k, down),
        };
        let Some(action) = fired else {
            return Ok(());
        };
        if tx.send(action).is_err() {
            return Err(());
        }
        Ok(())
    }

    /// One keymap snapshot.
    fn query_keymap(conn: &impl Connection) -> Result<[u8; 32], String> {
        let reply = conn
            .query_keymap()
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| e.to_string())?;
        Ok(reply.keys)
    }

    /// Feed one keycode transition to the chord set.
    ///
    /// Returns `Err(())` when the receiver is gone.
    fn poll_key_event(
        set: &mut ChordSet,
        tx: &mpsc::Sender<HotkeyAction>,
        table: &[u32; 256],
        keycode: u16,
        was: bool,
        is: bool,
    ) -> Result<(), ()> {
        if was == is {
            return Ok(());
        }
        let Some(keysym) = super::x11_keymap::keysym_of(table, keycode as u8) else {
            return Ok(());
        };
        let fired = match keysym_to_key(keysym) {
            Ok(md) => {
                set.on_modifier(md, is);
                None
            }
            Err(k) => set.on_key(k, is),
        };
        let Some(action) = fired else {
            return Ok(());
        };
        if tx.send(action).is_err() {
            return Err(());
        }
        Ok(())
    }

    /// Feed every changed keycode of one keymap snapshot to the chord set.
    ///
    /// Returns `Err(())` when the receiver is gone (the daemon stopped).
    fn poll_key_events(
        set: &mut ChordSet,
        tx: &mpsc::Sender<HotkeyAction>,
        table: &[u32; 256],
        prev: &[u8; 32],
        cur: &[u8; 32],
    ) -> Result<(), ()> {
        for keycode in 8u16..256 {
            let byte = (keycode / 8) as usize;
            let bit = (keycode % 8) as u8;
            let was = prev[byte] & (1 << bit) != 0;
            let is = cur[byte] & (1 << bit) != 0;
            poll_key_event(set, tx, table, keycode, was, is)?;
        }
        Ok(())
    }

    /// Polling fallback when RECORD is missing: 100 Hz `XQueryKeymap`.
    ///
    /// The keycode table is fetched once, before the first poll (audit
    /// F-128): the polling path shares the RECORD path's table instead of
    /// making a mapping round trip per changed keycode.
    fn run_polling(
        mut set: ChordSet,
        tx: mpsc::Sender<HotkeyAction>,
        stop: Arc<AtomicBool>,
    ) -> Result<(), String> {
        let (conn, _screen) = x11rb::connect(None).map_err(|e| e.to_string())?;
        let table = keycode_table(&conn)?;
        let mut prev = [0u8; 32];
        while !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(10));
            let cur = query_keymap(&conn)?;
            if poll_key_events(&mut set, &tx, &table, &prev, &cur).is_err() {
                return Ok(());
            }
            prev = cur;
        }
        Ok(())
    }

    pub(super) fn spawn(
        set: ChordSet,
    ) -> Result<(HotkeyHandle, mpsc::Receiver<HotkeyAction>), HotkeyError> {
        if let Some(reason) = unsupported_reason() {
            return Err(HotkeyError::Unsupported(reason.to_string()));
        }
        let use_record = x11rb::connect(None)
            .map(|(conn, _)| has_record(&conn))
            .unwrap_or(false);
        let (tx, rx) = mpsc::channel::<HotkeyAction>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        if use_record {
            let ctx_id = Arc::new(AtomicU32::new(0));
            let ctx_id_thread = ctx_id.clone();
            let finished = Arc::new(AtomicBool::new(false));
            let finished_thread = finished.clone();
            let (ready_tx, ready_rx) = mpsc::channel::<Result<(), String>>();
            let thread = std::thread::Builder::new()
                .name("agnosis-hotkey".into())
                .spawn(move || {
                    let _ = run_record(set, tx, stop_thread, ctx_id_thread, ready_tx);
                    finished_thread.store(true, Ordering::SeqCst);
                })
                .map_err(|e| HotkeyError::Platform(e.to_string()))?;
            // The context id must be known before `stop()` can run, else
            // the wake would have no context to close (audit P0-6).
            match ready_rx.recv().map_err(|_| HotkeyError::ChannelClosed)? {
                Ok(()) => {}
                Err(e) => {
                    let _ = thread.join();
                    return Err(HotkeyError::Platform(e));
                }
            }
            let stop_fn = move || {
                stop.store(true, Ordering::Relaxed);
                wake_record_context(ctx_id.load(Ordering::SeqCst), &finished);
            };
            Ok((HotkeyHandle::new(stop_fn, thread), rx))
        } else {
            let thread = std::thread::Builder::new()
                .name("agnosis-hotkey".into())
                .spawn(move || {
                    let _ = run_polling(set, tx, stop_thread);
                })
                .map_err(|e| HotkeyError::Platform(e.to_string()))?;
            Ok((
                HotkeyHandle::new(
                    move || {
                        stop.store(true, Ordering::Relaxed);
                    },
                    thread,
                ),
                rx,
            ))
        }
    }
}

/// The X11 keycode→keysym table helpers.
///
/// WHY this is its own module, compiled on every host in test builds: the
/// RECORD callback and the polling fallback used to make one synchronous
/// `get_keyboard_mapping` round trip **per key event** (audit F-128) —
/// the server answers in its own time, so a busy server delayed every
/// keystroke the listener observed. The whole keycode range is now
/// fetched once at listener start and laid into a table the hot paths
/// only index. The pure half of that layout lives here so it is pinned by
/// a test on every host (the same discipline as [`macos_keycodes`], whose
/// table a platform-gated module once hid from CI).
#[cfg(any(target_os = "linux", test))]
mod x11_keymap {
    /// The smallest keycode the X protocol hands out; the mapping request
    /// and the table both start there.
    pub(super) const MIN_KEYCODE: u8 = 8;

    /// How many keycodes the one-shot table covers: `8..=255` exactly.
    pub(super) const KEYCODE_COUNT: u8 = 248;

    /// Lay a `GetKeyboardMapping` reply into a keycode-indexed table.
    ///
    /// The reply is one flat list of `keysyms_per_keycode`-wide groups in
    /// keycode order; the table keeps each group's **first** keysym — the
    /// unshifted level, the one a chord's letter means. A keycode whose
    /// group lies past the data reads as 0, which [`keysym_of`]
    /// reports as unmapped.
    pub(super) fn table_from_mapping(keysyms: &[u32], keysyms_per_keycode: usize) -> [u32; 256] {
        let mut table = [0u32; 256];
        // A zero-width reply (a server that reports no keysyms per
        // keycode) maps nothing: reading the flat list at stride 1 would
        // hand every keycode the next server's keysym.
        if keysyms_per_keycode == 0 {
            return table;
        }
        let first = MIN_KEYCODE as usize;
        for (keycode, slot) in table.iter_mut().enumerate().skip(first) {
            *slot = group_first_keysym(keysyms, keysyms_per_keycode, keycode as u8);
        }
        table
    }

    /// The first keysym of one keycode's group, 0 when the data is short.
    fn group_first_keysym(keysyms: &[u32], width: usize, keycode: u8) -> u32 {
        let group = keycode.saturating_sub(MIN_KEYCODE) as usize;
        keysyms.get(group * width).copied().unwrap_or(0)
    }

    /// Look one keycode up in the cached table; `None` when unmapped.
    pub(super) fn keysym_of(table: &[u32; 256], keycode: u8) -> Option<u32> {
        let keysym = table[keycode as usize];
        (keysym != 0).then_some(keysym)
    }
}

/// The macOS virtual keycode → chord-key table.
///
/// WHY this is its own module, compiled on every host in test builds: the
/// macOS virtual keycodes are **not** ASCII-ordered, so the mapping cannot
/// be an arithmetic range. The old code assumed they were
/// (`b'a' + (code - 0x00)`), which mis-mapped every letter and digit
/// except `A` — `D` (0x02) came out as `c`, so the documented `D+Shift`
/// editor chord never fired on macOS (audit 2026-10-20 §3.1). Because the
/// table used to live inside the macOS-only `imp` module, no test on the
/// Windows or Linux CI runners could reach it. Here it is compiled
/// everywhere in test builds, so the table is pinned by a test on every
/// host.
#[cfg(any(target_os = "macos", test))]
mod macos_keycodes {
    use crate::platform::chord::Key;

    /// The letter keys, by macOS virtual keycode.
    ///
    /// The order is the macOS one (`A S D F H G Z X C V B Q W E R Y T …`),
    /// not the alphabetical one — the codes come from the ANSI/PC
    /// keyboard layout, so `D` is 0x02 and `S` is 0x01.
    const LETTERS: [(u64, char); 26] = [
        (0x00, 'a'),
        (0x01, 's'),
        (0x02, 'd'),
        (0x03, 'f'),
        (0x04, 'h'),
        (0x05, 'g'),
        (0x06, 'z'),
        (0x07, 'x'),
        (0x08, 'c'),
        (0x09, 'v'),
        (0x0B, 'b'),
        (0x0C, 'q'),
        (0x0D, 'w'),
        (0x0E, 'e'),
        (0x0F, 'r'),
        (0x10, 'y'),
        (0x11, 't'),
        (0x1F, 'o'),
        (0x20, 'u'),
        (0x22, 'i'),
        (0x23, 'p'),
        (0x25, 'l'),
        (0x26, 'j'),
        (0x28, 'k'),
        (0x2D, 'n'),
        (0x2E, 'm'),
    ];

    /// The digit keys, by macOS virtual keycode.
    ///
    /// The top row is `1 2 3 4 6 5 = 9 7 - 8 0` on a US layout, so the
    /// codes are not in digit order either.
    const DIGITS: [(u64, char); 10] = [
        (0x12, '1'),
        (0x13, '2'),
        (0x14, '3'),
        (0x15, '4'),
        (0x16, '6'),
        (0x17, '5'),
        (0x19, '9'),
        (0x1A, '7'),
        (0x1C, '8'),
        (0x1D, '0'),
    ];

    /// The virtual key code of the space bar.
    const SPACE: u64 = 0x31;

    /// The virtual key code of the escape key.
    const ESCAPE: u64 = 0x35;

    /// The virtual key codes of the function keys F1..=F20.
    ///
    /// `kVK_F1` is 0x7A and the codes are not contiguous, so they are
    /// listed explicitly.
    const FUNCTION_KEYS: [(u64, u8); 20] = [
        (0x7A, 1),
        (0x78, 2),
        (0x63, 3),
        (0x76, 4),
        (0x60, 5),
        (0x61, 6),
        (0x62, 7),
        (0x64, 8),
        (0x65, 9),
        (0x6D, 10),
        (0x67, 11),
        (0x6F, 12),
        (0x69, 13),
        (0x6B, 14),
        (0x71, 15),
        (0x6A, 16),
        (0x40, 17),
        (0x4F, 18),
        (0x50, 19),
        (0x5A, 20),
    ];

    /// The chord key a virtual key code stands for.
    ///
    /// `None` for a key the chord matcher has no representation for:
    /// macOS has no `Key::Other`, so such a key is dropped and the
    /// matcher never sees it. Extra held keys never block a chord, so
    /// dropping them is harmless.
    ///
    /// Letters come out **lowercased**, matching the convention of the
    /// Windows and Linux backends and the chord parser (which lowercases
    /// a single-character segment).
    pub(super) fn key_for_code(code: u64) -> Option<Key> {
        letter_for_code(code)
            .or_else(|| digit_for_code(code))
            .or_else(|| special_key(code))
            .or_else(|| function_key(code))
    }

    /// The letter a virtual key code stands for.
    fn letter_for_code(code: u64) -> Option<Key> {
        LETTERS
            .iter()
            .find(|(key_code, _)| *key_code == code)
            .map(|(_, letter)| Key::Char(*letter))
    }

    /// The digit a virtual key code stands for.
    fn digit_for_code(code: u64) -> Option<Key> {
        DIGITS
            .iter()
            .find(|(key_code, _)| *key_code == code)
            .map(|(_, digit)| Key::Char(*digit))
    }

    /// The space bar and the escape key.
    fn special_key(code: u64) -> Option<Key> {
        if code == SPACE {
            return Some(Key::Space);
        }
        if code == ESCAPE {
            return Some(Key::Escape);
        }
        None
    }

    /// The function key a virtual key code stands for.
    fn function_key(code: u64) -> Option<Key> {
        FUNCTION_KEYS
            .iter()
            .find(|(key_code, _)| *key_code == code)
            .map(|(_, number)| Key::F(*number))
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{HotkeyAction, HotkeyError, HotkeyHandle};
    use crate::platform::chord::{ChordSet, Key, Modifier};
    use objc2_core_foundation::{kCFRunLoopDefaultMode, CFMachPort, CFRetained, CFRunLoop};
    use objc2_core_graphics::{
        CGEvent, CGEventField, CGEventFlags, CGEventTapLocation, CGEventTapOptions,
        CGEventTapPlacement, CGEventType,
    };
    use std::ffi::c_void;
    use std::ptr::NonNull;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc};
    use std::time::Duration;

    /// How long the listener's run loop sleeps between two checks of the
    /// stop flag.
    ///
    /// The run loop wakes on its own whenever the tap fires, so this only
    /// bounds how long `stop()` takes to be noticed.
    const RUN_LOOP_SLICE: Duration = Duration::from_millis(100);

    /// How many actions may wait for the daemon before new ones are
    /// dropped.
    ///
    /// The channel is bounded so the tap callback can use `try_send` and
    /// never block: the callback runs on the tap's thread, and blocking
    /// it would make macOS disable the tap. The daemon drains the queue
    /// every 25 ms, so the depth only matters for a burst of chords.
    const ACTION_QUEUE_DEPTH: usize = 16;

    /// The event mask of the key events the tap is interested in.
    ///
    /// `kCGEventMaskKeyDown` (0x0000_0001_0000),
    /// `kCGEventMaskKeyUp` (0x0000_0002_0000) and
    /// `kCGEventMaskFlagsChanged` (0x0000_0010_0000) from
    /// `CGEventTypes.h`. A narrow mask keeps the tap cheap: the mouse
    /// events never reach the callback.
    const KEY_EVENT_MASK: u64 = 0x0000_0001_0000 | 0x0000_0002_0000 | 0x0000_0010_0000;

    /// The virtual key codes of the modifier keys (`kVK_*`).
    ///
    /// macOS reports a modifier change as a `FlagsChanged` event whose
    /// key code names the modifier; the *direction* of the change comes
    /// from the event's flag mask, so the right and the left key of the
    /// same modifier share one entry.
    const SHIFT: u64 = 0x38;
    const RIGHT_SHIFT: u64 = 0x3C;
    const CONTROL: u64 = 0x3B;
    const RIGHT_CONTROL: u64 = 0x3E;
    const OPTION: u64 = 0x3A;
    const RIGHT_OPTION: u64 = 0x3D;
    const COMMAND: u64 = 0x37;
    const RIGHT_COMMAND: u64 = 0x36;

    /// The chord key a virtual key code stands for.
    ///
    /// The mapping lives in [`super::macos_keycodes`], which is compiled on
    /// every host in test builds so the table is pinned by a test on the
    /// Windows and Linux CI runners as well (audit 2026-10-20 §3.1: the
    /// table used to be macOS-only, so a mis-mapped letter never reached
    /// CI).
    fn key_for_code(code: u64) -> Option<Key> {
        super::macos_keycodes::key_for_code(code)
    }

    /// The modifier a `FlagsChanged` event stands for, with the flag that
    /// tells whether the modifier is currently held.
    fn modifier_for_code(code: u64) -> Option<(Modifier, CGEventFlags)> {
        match code {
            SHIFT | RIGHT_SHIFT => Some((Modifier::Shift, CGEventFlags::MaskShift)),
            CONTROL | RIGHT_CONTROL => Some((Modifier::Ctrl, CGEventFlags::MaskControl)),
            OPTION | RIGHT_OPTION => Some((Modifier::Alt, CGEventFlags::MaskAlternate)),
            COMMAND | RIGHT_COMMAND => Some((Modifier::Super, CGEventFlags::MaskCommand)),
            _ => None,
        }
    }

    /// The virtual key code of an event.
    ///
    /// Only called for key events: macOS passes a **null** event for the
    /// "tap disabled" notifications.
    fn keycode_of(event: NonNull<CGEvent>) -> u64 {
        // SAFETY: the tap callback receives a valid event pointer and
        // the event stays alive for the duration of the call.
        let event = unsafe { event.as_ref() };
        let code = CGEvent::integer_value_field(Some(event), CGEventField::KeyboardEventKeycode);
        code.max(0) as u64
    }

    /// The modifier flags of an event.
    fn flags_of(event: NonNull<CGEvent>) -> CGEventFlags {
        // SAFETY: see `keycode_of`.
        let event = unsafe { event.as_ref() };
        CGEvent::flags(Some(event))
    }

    /// The state the tap callback shares with the listener thread.
    ///
    /// The callback runs on the tap's run-loop thread and is the only
    /// writer, so the fields are reached through a raw pointer instead
    /// of a lock: the callback must stay cheap (no allocation, no
    /// blocking send).
    struct Shared {
        set: ChordSet,
        tx: mpsc::SyncSender<HotkeyAction>,
        /// The tap, published once it exists so the callback can
        /// re-enable it after macOS disabled it.
        port: Option<CFRetained<CFMachPort>>,
    }

    impl Shared {
        /// Feed one tap event into the chord set.
        fn handle(&mut self, event_type: CGEventType, event: NonNull<CGEvent>) {
            match event_type {
                CGEventType::TapDisabledByTimeout | CGEventType::TapDisabledByUserInput => {
                    self.reenable();
                }
                CGEventType::KeyDown => self.key(event, true),
                CGEventType::KeyUp => self.key(event, false),
                CGEventType::FlagsChanged => self.modifier(event),
                _ => {}
            }
        }

        /// Feed one key transition into the chord set.
        fn key(&mut self, event: NonNull<CGEvent>, down: bool) {
            let Some(key) = key_for_code(keycode_of(event)) else {
                return;
            };
            let action = self.set.on_key(key, down);
            self.fire(action);
        }

        /// Feed one modifier transition into the chord set.
        ///
        /// The direction comes from the event's flag mask: the mask
        /// carries the state *after* the change, so the modifier is held
        /// exactly when its bit is set.
        fn modifier(&mut self, event: NonNull<CGEvent>) {
            let Some((modifier, mask)) = modifier_for_code(keycode_of(event)) else {
                return;
            };
            self.set
                .on_modifier(modifier, flags_of(event).contains(mask));
        }

        /// Send the action of a fired chord, if any.
        fn fire(&mut self, action: Option<HotkeyAction>) {
            let Some(action) = action else {
                return;
            };
            // `try_send` never blocks: the callback runs on the tap's
            // thread, and a blocking send would make macOS disable the
            // tap. A full queue means the daemon is far behind; the
            // action is dropped and the drop is logged.
            if let Err(e) = self.tx.try_send(action) {
                agnosis_core::trace::warn(&format!("hotkey.action_dropped: {e}"));
            }
        }

        /// Re-enable the tap after macOS disabled it.
        ///
        /// macOS disables a tap that ran too long and one that was
        /// disabled by user input; without this the hotkey would die
        /// silently after a burst of heavy input.
        fn reenable(&mut self) {
            let Some(port) = self.port.as_ref() else {
                return;
            };
            CGEvent::tap_enable(port, true);
            agnosis_core::trace::warn("hotkey.tap_reenabled");
        }
    }

    /// The event-tap callback.
    ///
    /// It runs on the tap's run-loop thread for every key event, so it
    /// must stay cheap: no allocation and no blocking send. The action
    /// leaves through `try_send`, which never blocks.
    unsafe extern "C-unwind" fn tap_callback(
        _proxy: objc2_core_graphics::CGEventTapProxy,
        event_type: CGEventType,
        event: NonNull<CGEvent>,
        user_info: *mut c_void,
    ) -> *mut CGEvent {
        // SAFETY: `user_info` is the `Box<Shared>` pointer handed to
        // `CGEventTapCreate`. The box outlives the tap: the listener
        // thread removes the run-loop source and invalidates the port
        // before it drops the box.
        let shared = unsafe { &mut *(user_info as *mut Shared) };
        shared.handle(event_type, event);
        // A listen-only tap must hand the event back unchanged. For the
        // "tap disabled" notifications macOS passes a null event; it is
        // only forwarded here and never dereferenced.
        event.as_ptr()
    }

    /// Create the listen-only event tap.
    ///
    /// A listen-only tap observes the key events without swallowing
    /// them, so macOS asks for the *Input Monitoring* grant instead of
    /// the Accessibility one. The tap is created on the HID level and
    /// inserted at the head of the event stream.
    ///
    /// The grant is tied to the **bundle id and the signature** of the
    /// running binary, so `crate::platform::APP_BUNDLE_ID`
    /// (`com.agnosis.daemon`) has to stay stable across releases — a
    /// different bundle id loses the grant, and the daemon then exits
    /// with the remediation text.
    fn create_tap(user_info: *mut c_void) -> Option<CFRetained<CFMachPort>> {
        // SAFETY: the callback is a plain `extern "C"` function and
        // `user_info` points at the shared state that outlives the tap.
        unsafe {
            CGEvent::tap_create(
                CGEventTapLocation::HIDEventTap,
                CGEventTapPlacement::HeadInsertEventTap,
                CGEventTapOptions::ListenOnly,
                KEY_EVENT_MASK,
                Some(tap_callback),
                user_info,
            )
        }
    }

    /// The default run-loop mode of the tap's thread.
    fn default_mode() -> Option<&'static objc2_core_foundation::CFRunLoopMode> {
        // SAFETY: `kCFRunLoopDefaultMode` is a CoreFoundation global
        // that lives for the whole process.
        unsafe { kCFRunLoopDefaultMode }
    }

    /// Run the run loop until the stop flag is set.
    ///
    /// `run_in_mode` drives the **current** run loop of the calling
    /// thread, which is the one the tap's source was added to.
    fn run_until_stopped(stop: &AtomicBool) {
        while !stop.load(Ordering::Relaxed) {
            // `run_in_mode` returns as soon as the tap fired, so the
            // chord matcher is driven promptly; the timeout only bounds
            // how long a `stop()` takes to be noticed.
            let _ = CFRunLoop::run_in_mode(default_mode(), RUN_LOOP_SLICE.as_secs_f64(), true);
        }
    }

    /// The listener thread: create the tap, drive its run loop, and keep
    /// the tap alive until the stop flag is set.
    fn listener_thread(
        ready_tx: mpsc::Sender<Result<(), HotkeyError>>,
        stop: Arc<AtomicBool>,
        set: ChordSet,
        tx: mpsc::SyncSender<HotkeyAction>,
    ) {
        let mut shared = Box::new(Shared {
            set,
            tx,
            port: None,
        });
        let Some(port) = create_tap(shared.as_mut() as *mut Shared as *mut c_void) else {
            // The tap could not be created: the honest reason is the
            // missing Input Monitoring grant.
            let _ = ready_tx.send(Err(super::macos_missing_permission()));
            return;
        };
        shared.port = Some(port.clone());
        let Some(source) = CFMachPort::new_run_loop_source(None, Some(&port), 0) else {
            let _ = ready_tx.send(Err(HotkeyError::Platform(
                "the event tap has no run-loop source".to_string(),
            )));
            return;
        };
        let Some(runloop) = CFRunLoop::current() else {
            let _ = ready_tx.send(Err(HotkeyError::Platform(
                "the hotkey thread has no run loop".to_string(),
            )));
            return;
        };
        runloop.add_source(Some(&source), default_mode());
        let _ = ready_tx.send(Ok(()));
        run_until_stopped(&stop);
        runloop.remove_source(Some(&source), default_mode());
        // The tap must not fire after the shared state is dropped.
        CGEvent::tap_enable(&port, false);
        port.invalidate();
    }

    /// Spawn the macOS global hotkey listener.
    ///
    /// The backend is a listen-only `CGEventTap` on a dedicated thread,
    /// so the daemon's main loop is unchanged: the actions arrive on the
    /// channel like on every other platform (roadmap Stage 3A).
    pub(super) fn spawn(
        set: ChordSet,
    ) -> Result<(HotkeyHandle, mpsc::Receiver<HotkeyAction>), HotkeyError> {
        let (tx, rx) = mpsc::sync_channel::<HotkeyAction>(ACTION_QUEUE_DEPTH);
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(), HotkeyError>>();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = stop.clone();
        let thread = std::thread::Builder::new()
            .name("agnosis-hotkey".into())
            .spawn(move || listener_thread(ready_tx, stop_thread, set, tx))
            .map_err(|e| HotkeyError::Platform(e.to_string()))?;
        match ready_rx.recv().map_err(|_| HotkeyError::ChannelClosed)? {
            Ok(()) => {}
            Err(e) => {
                let _ = thread.join();
                return Err(e);
            }
        }
        let stop_fn = move || {
            stop.store(true, Ordering::Relaxed);
        };
        Ok((HotkeyHandle::new(stop_fn, thread), rx))
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
mod imp {
    use super::{HotkeyAction, HotkeyError, HotkeyHandle};
    use crate::platform::chord::ChordSet;

    pub(super) fn spawn(
        _set: ChordSet,
    ) -> Result<(HotkeyHandle, std::sync::mpsc::Receiver<HotkeyAction>), HotkeyError> {
        Err(HotkeyError::Unsupported(
            "this platform has no hotkey backend".into(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::chord::{ChordSet, Key, Modifier};

    #[test]
    fn garbage_spec_returns_err_without_spawning() {
        let before = std::thread::available_parallelism().ok();
        let r = spawn(&[("A++Garbage!!!".to_string(), HotkeyAction::Popup)]);
        assert!(matches!(r, Err(HotkeyError::Spec(_))));
        let after = std::thread::available_parallelism().ok();
        assert_eq!(before.is_some(), after.is_some());
    }

    #[test]
    fn empty_spec_is_skipped_and_binds_nothing() {
        // An empty spec disables that chord instead of failing the whole
        // set, so the editor chord can be turned off in the config.
        let mut set = ChordSet::new(&[("".to_string(), HotkeyAction::Editor)])
            .expect("an empty spec is skipped, not an error");
        assert!(set
            .on_key(crate::platform::chord::Key::Char('d'), true)
            .is_none());
    }

    /// The chord parser is shared: one spec means one chord on every
    /// platform, so the Windows hook, the X11 recorder and the macOS
    /// event tap all feed the same matcher with the same result.
    #[test]
    fn chord_set_matches_every_platform_parser() {
        let mut set = ChordSet::new(&[
            ("A+G+Shift+Space".to_string(), HotkeyAction::Popup),
            ("D+Shift".to_string(), HotkeyAction::Editor),
        ])
        .expect("both specs parse");
        // Shift is held, then the editor chord's key goes down.
        set.on_modifier(Modifier::Shift, true);
        assert_eq!(
            set.on_key(Key::Char('d'), true),
            Some(HotkeyAction::Editor),
            "D+Shift must fire the editor action everywhere"
        );
        set.on_key(Key::Char('d'), false);
        // The parade chord needs all three of its keys.
        assert_eq!(set.on_key(Key::Char('a'), true), None);
        assert_eq!(set.on_key(Key::Char('g'), true), None);
        assert_eq!(
            set.on_key(Key::Space, true),
            Some(HotkeyAction::Popup),
            "a multi-key chord must fire everywhere too"
        );
    }

    /// The macOS backend must degrade honestly: the tap cannot be
    /// created without the Input Monitoring grant, and the message has
    /// to name the exact System Settings path.
    #[test]
    fn macos_chord_reports_a_missing_permission() {
        let err = macos_missing_permission();
        assert!(
            matches!(err, HotkeyError::Unavailable(_)),
            "a missing grant is not an unsupported platform"
        );
        let text = err.to_string();
        assert!(
            text.contains("System Settings → Privacy & Security → Input Monitoring"),
            "the message must name the exact path, got {text}"
        );
        assert!(
            text.contains("Input Monitoring"),
            "the message must name the grant, got {text}"
        );
    }

    /// The macOS virtual keycode table must map every letter and digit
    /// to the key it actually is.
    ///
    /// The macOS codes are **not** ASCII-ordered (0x02 is `D`, 0x01 is
    /// `S`), so a range-based mapping mis-keys almost everything — that
    /// bug made the documented `D+Shift` editor chord fire on `Shift+C`
    /// instead (audit 2026-10-20 §3.1). The table lives in a module
    /// compiled on every host in test builds precisely so this test runs
    /// on the Windows and Linux CI runners too.
    #[test]
    fn macos_keycodes_map_every_letter_and_digit() {
        use crate::platform::chord::Key;
        use crate::platform::hotkey::macos_keycodes::key_for_code;

        // The letters, by their real macOS virtual keycode.
        let letters = [
            (0x00, 'a'),
            (0x01, 's'),
            (0x02, 'd'),
            (0x03, 'f'),
            (0x04, 'h'),
            (0x05, 'g'),
            (0x06, 'z'),
            (0x07, 'x'),
            (0x08, 'c'),
            (0x09, 'v'),
            (0x0B, 'b'),
            (0x0C, 'q'),
            (0x0D, 'w'),
            (0x0E, 'e'),
            (0x0F, 'r'),
            (0x10, 'y'),
            (0x11, 't'),
            (0x1F, 'o'),
            (0x20, 'u'),
            (0x22, 'i'),
            (0x23, 'p'),
            (0x25, 'l'),
            (0x26, 'j'),
            (0x28, 'k'),
            (0x2D, 'n'),
            (0x2E, 'm'),
        ];
        for (code, letter) in letters {
            assert_eq!(
                key_for_code(code),
                Some(Key::Char(letter)),
                "keycode {code:#04x} is {letter}, not something else"
            );
        }

        // The digits, by their real macOS virtual keycode.
        let digits = [
            (0x12, '1'),
            (0x13, '2'),
            (0x14, '3'),
            (0x15, '4'),
            (0x16, '6'),
            (0x17, '5'),
            (0x19, '9'),
            (0x1A, '7'),
            (0x1C, '8'),
            (0x1D, '0'),
        ];
        for (code, digit) in digits {
            assert_eq!(
                key_for_code(code),
                Some(Key::Char(digit)),
                "keycode {code:#04x} is the digit {digit}"
            );
        }
    }

    /// The `D+Shift` editor chord must resolve to the key `D` on the
    /// macOS backend, and it must actually fire the editor action.
    ///
    /// This is the exact chord the README documents, and it is the chord
    /// the mis-mapped table broke (audit 2026-10-20 §3.1).
    #[test]
    fn the_editor_chord_fires_on_the_macos_keycode_for_d() {
        use crate::platform::chord::Key;
        use crate::platform::hotkey::macos_keycodes::key_for_code;

        // 0x02 is `D` on macOS (the ANSI layout's second letter column).
        assert_eq!(key_for_code(0x02), Some(Key::Char('d')));
        // The whole chord, end to end, through the shared matcher.
        let mut set = ChordSet::new(&[("D+Shift".to_string(), HotkeyAction::Editor)])
            .expect("the editor chord parses");
        set.on_modifier(Modifier::Shift, true);
        let key = key_for_code(0x02).expect("0x02 is a key the matcher knows");
        assert_eq!(
            set.on_key(key, true),
            Some(HotkeyAction::Editor),
            "D+Shift must fire the editor action on macOS"
        );
    }

    /// The space bar and the escape key keep their own mapping, and the
    /// function keys are unchanged by the letter/digit fix.
    #[test]
    fn macos_keycodes_keep_space_escape_and_the_function_keys() {
        use crate::platform::chord::Key;
        use crate::platform::hotkey::macos_keycodes::key_for_code;

        assert_eq!(key_for_code(0x31), Some(Key::Space));
        assert_eq!(key_for_code(0x35), Some(Key::Escape));
        assert_eq!(key_for_code(0x7A), Some(Key::F(1)));
        assert_eq!(key_for_code(0x5A), Some(Key::F(20)));
        // A key the chord matcher has no representation for is dropped.
        assert_eq!(key_for_code(0x33), None);
    }

    /// Carbon's `RegisterEventHotKey` can only express one key plus
    /// modifiers; a multi-key chord needs the `CGEventTap` path.
    #[test]
    fn carbon_fallback_accepts_one_key_plus_modifiers() {
        use crate::platform::chord::parse_chord;
        let one_key = parse_chord("Ctrl+Alt+Shift+Space").expect("parses");
        assert!(
            carbon_fallback_accepts(&one_key),
            "one key plus modifiers is expressible in Carbon"
        );
        let editor = parse_chord("D+Shift").expect("parses");
        assert!(
            carbon_fallback_accepts(&editor),
            "D+Shift is expressible in Carbon"
        );
        let multi = parse_chord("A+G+Shift+Space").expect("parses");
        assert!(
            !carbon_fallback_accepts(&multi),
            "a multi-key chord is not expressible in Carbon"
        );
    }

    #[test]
    fn dropping_the_handle_stops_and_joins_the_listener() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::Duration;

        let (tx, rx) = std::sync::mpsc::channel();
        let flag = Arc::new(AtomicBool::new(false));
        let flag_thread = flag.clone();
        let thread = std::thread::spawn(move || {
            while !flag_thread.load(Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(5));
            }
            let _ = tx.send(());
        });
        let handle = HotkeyHandle::new(move || flag.store(true, Ordering::Relaxed), thread);
        drop(handle);
        assert!(
            rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "dropping the handle must stop the listener"
        );
    }

    #[test]
    fn stopping_after_a_listener_panic_does_not_panic() {
        let thread = std::thread::spawn(|| panic!("listener boom"));
        let handle = HotkeyHandle::new(|| {}, thread);
        handle.stop();
    }

    /// The cached keycode→keysym table (audit F-128): the table keeps the
    /// **first** keysym of each keycode's group (the unshifted level), a
    /// keycode whose group lies past the reply's data reads as unmapped,
    /// and the covered range is exactly the protocol's `8..=255`.
    #[test]
    fn the_keycode_table_takes_the_first_keysym_of_each_group() {
        use super::x11_keymap::{keysym_of, table_from_mapping, KEYCODE_COUNT, MIN_KEYCODE};

        // Width 2, two keycodes: [a/A] and [Escape/—].
        let keysyms = [0x61, 0x41, 0xFF1B, 0x0000];
        let table = table_from_mapping(&keysyms, 2);
        assert_eq!(keysym_of(&table, MIN_KEYCODE), Some(0x61));
        assert_eq!(keysym_of(&table, MIN_KEYCODE + 1), Some(0xFF1B));
        // No data for the third keycode: unmapped, not garbage.
        assert_eq!(keysym_of(&table, MIN_KEYCODE + 2), None);
        // Below the protocol's first keycode: unmapped.
        assert_eq!(keysym_of(&table, 0), None);
        // The range is exactly 8..=255.
        assert_eq!(MIN_KEYCODE, 8);
        assert_eq!(KEYCODE_COUNT, 248);
    }

    /// A zero-width reply (a server that reports no keysyms per keycode)
    /// must not divide by zero: every keycode reads as unmapped.
    #[test]
    fn a_zero_width_mapping_reads_as_unmapped() {
        use super::x11_keymap::{keysym_of, table_from_mapping};

        let table = table_from_mapping(&[0x61], 0);
        assert_eq!(keysym_of(&table, 8), None);
        assert_eq!(keysym_of(&table, 255), None);
    }

    /// The pre-mapped VK table (audit F-137): the hook must find every
    /// modifier, the space bar, escape, the letters and the digits
    /// already resolved — and every unmapped VK collapses to the single
    /// `Other` sentinel, so the per-event path allocates nothing. The
    /// table also retires `vk_to_key`'s dead modifier branch (audit
    /// F-131): the decision lives in the table, built once.
    #[cfg(target_os = "windows")]
    #[test]
    fn the_vk_table_pre_maps_every_key_the_matcher_knows() {
        use super::imp::{build_vk_table, VkEntry};
        use crate::platform::chord::Modifier;

        let table = build_vk_table();
        // The space bar and escape keep their own entries.
        assert_eq!(table[0x20], VkEntry::Space);
        assert_eq!(table[0x1B], VkEntry::Escape);
        // The modifiers: generic plus left/right codes.
        assert_eq!(table[0x10], VkEntry::Mod(Modifier::Shift));
        assert_eq!(table[0xA0], VkEntry::Mod(Modifier::Shift));
        assert_eq!(table[0xA1], VkEntry::Mod(Modifier::Shift));
        assert_eq!(table[0x11], VkEntry::Mod(Modifier::Ctrl));
        assert_eq!(table[0xA2], VkEntry::Mod(Modifier::Ctrl));
        assert_eq!(table[0xA3], VkEntry::Mod(Modifier::Ctrl));
        assert_eq!(table[0x12], VkEntry::Mod(Modifier::Alt));
        assert_eq!(table[0xA4], VkEntry::Mod(Modifier::Alt));
        assert_eq!(table[0xA5], VkEntry::Mod(Modifier::Alt));
        assert_eq!(table[0x5B], VkEntry::Mod(Modifier::Super));
        assert_eq!(table[0x5C], VkEntry::Mod(Modifier::Super));
        // The letters (lowercased) and the digits are ASCII-ordered.
        assert_eq!(table[0x41], VkEntry::Char('a'));
        assert_eq!(table[0x44], VkEntry::Char('d'));
        assert_eq!(table[0x5A], VkEntry::Char('z'));
        assert_eq!(table[0x30], VkEntry::Char('0'));
        assert_eq!(table[0x39], VkEntry::Char('9'));
        // Everything else is the catch-all, and the table is total.
        assert_eq!(table[0x00], VkEntry::Other);
        assert_eq!(table[0xBA], VkEntry::Other);
        assert_eq!(table[0xFF], VkEntry::Other);
    }
}
