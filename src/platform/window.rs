//! Native black background + focus ladder + cross-process popup close.
//!
//! The popup calls [`paint_black_before_show`] right after creating its
//! winit window and before showing it, so the first presented frame is
//! opaque black instead of garbage or white while wgpu spins up. The
//! handle comes from winit's `raw-window-handle` (`rwh_06`) implementation.
//!
//! [`focus_ladder`] runs the Windows foreground-recovery ladder; on other
//! platforms it is a graceful no-op. It never blocks the caller: the
//! Windows path tries once inline and hands the retries to a short-lived
//! worker, so the popup's first frame is not delayed by the ladder
//! (audit performance). [`close_popup`] lets the hotkey combo
//! double as a toggle: Windows finds `POPUP_WINDOW_CLASS`
//! (`"AGnosisPopup"`, see [`crate::platform::POPUP_WINDOW_CLASS`]) with
//! `FindWindowW` and posts `WM_CLOSE`; Unix reads the runtime PID file
//! ([`crate::paths::popup_pid_file`]) and sends `SIGTERM`. It returns true
//! when a popup was found and signalled, and is a harmless no-op
//! otherwise.
//!
//! [`close_editor`] is the editor's twin (roadmap Stage 4): Windows finds
//! `EDITOR_WINDOW_CLASS` (`"AGnosisEditor"`, see
//! [`crate::platform::EDITOR_WINDOW_CLASS`]), Unix reads the editor's own
//! PID file ([`crate::paths::editor_pid_file`]). The two lookups use
//! different names and different PID files, so neither window can ever be
//! closed by the other's toggle.
//!
//! [`ensure_borderless`] and [`ensure_borderless_resizable`] are the two
//! borderless flavours: the parade popup uses the first (no chrome at
//! all, not even a sizing border), the draw-your-own editor uses the
//! second (no caption, but the **invisible** sizing border the user
//! drags to resize the window — a borderless window without it could
//! never be resized, which would break roadmap Stage 4/5).

use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::Window;

/// Win32 `GWL_STYLE` bits that paint OS chrome (titlebar + caption buttons).
///
/// The `GWL_STYLE` vocabulary is Win32-only: the constants exist on every
/// platform so the style tests run everywhere, but only the Windows
/// backend consumes them in production.
#[cfg(any(target_os = "windows", test))]
pub const CHROME_CAPTION: u32 = 0x00C0_0000;
/// Win32 `GWL_STYLE` thick-frame bit (sizing border).
#[cfg(any(target_os = "windows", test))]
pub const CHROME_THICKFRAME: u32 = 0x0004_0000;
/// Win32 `GWL_STYLE` minimize-button bit.
#[cfg(any(target_os = "windows", test))]
pub const CHROME_MINIMIZEBOX: u32 = 0x0002_0000;
/// Win32 `GWL_STYLE` maximize-button bit.
#[cfg(any(target_os = "windows", test))]
pub const CHROME_MAXIMIZEBOX: u32 = 0x0001_0000;
/// Win32 `GWL_STYLE` system-menu bit.
#[cfg(any(target_os = "windows", test))]
pub const CHROME_SYSMENU: u32 = 0x0008_0000;
/// Win32 `GWL_STYLE` popup bit a clean borderless window carries.
#[cfg(any(target_os = "windows", test))]
pub const STYLE_POPUP: u32 = 0x8000_0000;
/// Win32 `GWL_STYLE` visible bit a shown popup carries.
#[cfg(test)]
pub const STYLE_VISIBLE: u32 = 0x1000_0000;

/// Mask of every `GWL_STYLE` bit that paints OS chrome.
#[cfg(any(target_os = "windows", test))]
pub const CHROME_BITS: u32 =
    CHROME_CAPTION | CHROME_THICKFRAME | CHROME_MINIMIZEBOX | CHROME_MAXIMIZEBOX | CHROME_SYSMENU;

/// Mask of the `GWL_STYLE` chrome bits **except** the sizing border.
///
/// [`CHROME_BITS`] minus `WS_THICKFRAME`. Windows draws the (invisible)
/// sizing border of a `WS_POPUP` window from `WS_THICKFRAME` alone, so
/// stripping every other chrome bit keeps the window borderless *and*
/// user-resizable.
#[cfg(any(target_os = "windows", test))]
pub const CHROME_BITS_KEEP_FRAME: u32 =
    CHROME_CAPTION | CHROME_MINIMIZEBOX | CHROME_MAXIMIZEBOX | CHROME_SYSMENU;

/// True when a raw `GWL_STYLE` value carries any OS chrome bit.
///
/// Documented call order for the popup: create the winit window with
/// decorations disabled and caption buttons emptied, paint the native
/// background black, `set_visible(true)`, then [`ensure_borderless`]
/// re-asserts the borderless style after show, then the focus ladder runs.
///
/// Test-only helper (audit maintainability: sweep test-only accessors with
/// the `dead_code` lint): production reads the style through
/// [`strip_chrome_bits`].
#[cfg(test)]
pub fn style_has_chrome(style: u32) -> bool {
    style & CHROME_BITS != 0
}

/// Return `style` with every OS chrome bit stripped, `WS_POPUP` set.
#[cfg(any(target_os = "windows", test))]
pub fn strip_chrome_bits(style: u32) -> u32 {
    (style & !CHROME_BITS) | STYLE_POPUP
}

/// Return `style` with every OS chrome bit **except the sizing border**
/// stripped, `WS_POPUP` set and `WS_THICKFRAME` kept.
///
/// This is the editor's flavour of "borderless": the window shows no
/// title bar, no system menu and no caption buttons (the ESC key closes
/// it instead), but it keeps the invisible border Windows uses to size a
/// window, so the user can drag the edge and resize it. The border is
/// only *invisible* — the hit test is unchanged.
///
/// The parade popup must **not** use this: it covers the whole screen and
/// has nothing to resize (see [`strip_chrome_bits`]).
#[cfg(any(target_os = "windows", test))]
pub fn strip_chrome_bits_keep_frame(style: u32) -> u32 {
    (style & !CHROME_BITS_KEEP_FRAME) | STYLE_POPUP | CHROME_THICKFRAME
}

/// Failures from [`paint_black_before_show`] and [`write_pid_file`].
/// Focus/close paths never fail hard.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    /// Platform backend failure with a message.
    #[error("{0}")]
    Platform(String),
}

/// Set the native background to opaque black BEFORE `show()`.
///
/// Call from the popup right after creating the winit window, using winit's
/// raw-window-handle (`rwh_06`) to get the native handle:
/// Windows `SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND,
/// CreateSolidBrush(BLACK))` so `WM_ERASEBKGND` fills black; X11
/// `XSetWindowAttributes { background_pixel: 0 }` on the window XID via
/// x11rb, flushed before the first map/show; macOS `backgroundColor =
/// black`, `isOpaque = YES`.
pub fn paint_black_before_show(window: &Window) -> Result<(), WindowError> {
    let handle = window
        .window_handle()
        .map_err(|e| WindowError::Platform(e.to_string()))?;
    imp::paint_black_before_show(handle.as_raw())
}

/// How long the close waits for the popup's window to disappear.
///
/// The close is asynchronous on every platform — a posted `WM_CLOSE` on
/// Windows, a `SIGTERM` on Unix — so the daemon waits a moment for the
/// window to go before it answers the next hotkey press. Without the
/// wait, a double-tap posted a second close instead of opening a new
/// parade (audit 2026-10-20 §3.11).
const POPUP_CLOSE_WAIT: std::time::Duration = std::time::Duration::from_millis(500);

/// How often the close checks whether the popup has gone.
const POPUP_CLOSE_POLL: std::time::Duration = std::time::Duration::from_millis(10);

/// How long the daemon waits for a **spawned** popup's window to exist.
///
/// The popup is a **detached child**, so its window class (Windows) or
/// its PID file (Unix) does not exist for tens of milliseconds after the
/// spawn. A second hotkey press inside that window therefore took the
/// **spawn** branch again and opened a second parade (audit
/// 2026-10-21 §G21).
///
/// The close path has the mirror-image fix ([`POPUP_CLOSE_WAIT`]); the
/// spawn path had none.
const POPUP_SPAWN_WAIT: std::time::Duration = std::time::Duration::from_millis(1500);

/// Focus ladder: bring the freshly spawned window to the foreground.
///
/// Windows: `SetForegroundWindow` with an `AttachThreadInput` retry over
/// ~150ms, then give up gracefully. The call returns immediately — the
/// retries run on a short-lived worker thread, so the popup's first frame
/// is never delayed by the ladder (audit performance). No-ops where the
/// platform needs none; never fails hard.
///
/// `topmost` asks for the window to be raised above everything else. The
/// parade popup wants that — it is a fullscreen overlay. The **editor**
/// does not: it is an ordinary window, and forcing it topmost made it
/// float above every other application on Windows while the same editor
/// behaved normally on Linux and macOS (audit 2026-10-20 §3.18).
pub fn focus_ladder(window: &Window, topmost: bool) {
    let Ok(handle) = window.window_handle() else {
        return;
    };
    imp::focus_ladder(handle.as_raw(), topmost);
}

/// Re-assert the borderless style after the window is shown.
///
/// winit 0.30 keeps `WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX |
/// WS_MAXIMIZEBOX` in `GWL_STYLE` when caption buttons are left enabled, so
/// the frame can come back with a titlebar. This strips every chrome bit
/// with `SetWindowLongPtrW(GWL_STYLE)` and forces a frame recalc with
/// `SetWindowPos(SWP_FRAMECHANGED)`. No-op off Windows; never fails hard.
pub fn ensure_borderless(window: &Window) {
    let Ok(handle) = window.window_handle() else {
        return;
    };
    imp::ensure_borderless(handle.as_raw());
}

/// Re-assert the **borderless but resizable** style after the window is
/// shown.
///
/// The editor is the window that needs this one: it is borderless (no
/// title bar, no close button — ESC closes it) because `CursorMoved`
/// reports pixels relative to the window origin, but it must stay
/// **resizable** (roadmap Stage 4/5). On Windows a borderless window
/// loses its sizing border with the caption, so the style keeps
/// `WS_THICKFRAME`: the border stays invisible, but the user can drag
/// it and the layout follows the new size.
///
/// It runs **after** `set_resizable(true)`, which re-adds the thick frame,
/// so the final style is the one the user can resize. The parade popup
/// keeps the plain [`ensure_borderless`] — it covers the screen and has
/// nothing to resize.
///
/// No-op off Windows; never fails hard.
pub fn ensure_borderless_resizable(window: &Window) {
    let Ok(handle) = window.window_handle() else {
        return;
    };
    imp::ensure_borderless_resizable(handle.as_raw());
}

/// Raise the process to Per-Monitor-V2 DPI awareness at runtime.
///
/// The build-time manifest (build.rs) is the primary mechanism because it
/// applies before `main()`; this is the belt-and-braces fallback for
/// binaries whose manifest was stripped by an external packer. Calling it
/// when the manifest already applied is a harmless no-op (same level), and
/// it must run before any monitor query or winit init — `cli::run` calls it
/// first thing. No-op off Windows; never fails hard.
pub fn ensure_dpi_awareness() {
    imp::ensure_dpi_awareness();
}

/// Request a running popup to close (the combo doubles as a toggle):
/// Windows `FindWindowW("AGnosisPopup")` + `PostMessageW(WM_CLOSE)`; Unix
/// reads the PID file and sends `SIGTERM`. No-op when no popup runs.
/// Returns true when a popup was found and signalled.
pub fn close_popup() -> bool {
    imp::close_popup()
}

/// Wait, briefly, for a **spawned** popup's window to exist.
///
/// The wait is bounded and it runs on the **daemon's** hotkey thread —
/// never on a UI thread — so it cannot freeze the parade (audit
/// 2026-10-21 §G21).
///
/// It is the mirror image of the close path's wait: the close waits for
/// the window to **go**, this waits for it to **come**.
pub fn wait_for_popup_window() {
    imp::wait_for_popup_window();
}

/// Is the parade popup on screen right now?
///
/// The daemon polls this to notice that the parade has ended — however it
/// ended (the hotkey, `ESC`, the window manager) — so the browsers it hid
/// can be put back (audit 2026-10-20 §3.7).
///
/// The probe posts **no** message and closes nothing.
pub fn popup_is_open() -> bool {
    imp::popup_is_open()
}

/// Unix: write our PID to the runtime pid file
/// ([`crate::paths::popup_pid_file`]). Windows: no-op returning `Ok`.
pub fn write_pid_file() -> Result<(), WindowError> {
    imp::write_pid_file()
}

/// Remove the runtime pid file, ignoring absence.
pub fn clear_pid_file() {
    imp::clear_pid_file();
}

/// Request a running editor to close (the editor chord doubles as a
/// toggle): Windows `FindWindowW("AGnosisEditor")` + `PostMessageW(WM_CLOSE)`;
/// Unix reads the editor's own PID file
/// ([`crate::paths::editor_pid_file`]) and sends `SIGTERM` after the same
/// ownership check the popup path uses. No-op when no editor runs.
/// Returns true when an editor was found and signalled.
///
/// The editor has its own window class and its own PID file, so this never
/// closes the parade popup and [`close_popup`] never closes the editor
/// (roadmap Stage 4).
///
/// The daemon's editor toggle is the caller (roadmap Stage 4b).
pub fn close_editor() -> bool {
    imp::close_editor()
}

/// Wait, briefly, for a spawned editor's window to exist.
///
/// The editor's mirror of [`wait_for_popup_window`]: the editor is a
/// detached child and its window does not exist for tens of
/// milliseconds after the spawn. Without the wait, a double-tap of the
/// editor chord took the spawn branch twice and opened a second editor
/// (audit 2026-10-22 §P1-6).
///
/// The wait runs on the daemon's hotkey thread, never on a UI thread.
pub fn wait_for_editor_window() {
    imp::wait_for_editor_window();
}

/// Read-only lookup of the live window registered under `class`.
///
/// Windows only: the window class is a Win32 concept. The answer is the
/// native window handle as a number, or `None` when no window of that
/// class exists.
///
/// The probe posts **no** message and closes nothing, so a test that
/// runs beside the user's own windows cannot disturb them — unlike
/// [`close_editor`], which really does close the editor.
#[cfg(all(test, target_os = "windows"))]
pub fn find_window_of_class(class: &str) -> Option<isize> {
    imp::find_window_of_class(class)
}

/// Unix: write our PID to the editor's runtime pid file
/// ([`crate::paths::editor_pid_file`]). Windows: no-op returning `Ok`.
pub fn write_editor_pid_file() -> Result<(), WindowError> {
    imp::write_editor_pid_file()
}

/// Remove the editor's runtime pid file, ignoring absence.
pub fn clear_editor_pid_file() {
    imp::clear_editor_pid_file();
}

/// Is a HELP window open? The parade's F1 asks this before spawning a
/// second reader (audit 2026-10-25).
///
/// Windows: the live `AGnosisHelp` window class (`FindWindowW`). Unix: the
/// HELP runtime pid file with verified ownership — a stale file is not a
/// reader. The probe posts no message and closes nothing.
pub fn help_is_open() -> bool {
    imp::help_is_open()
}

/// Is the draw-your-own editor open? The parade's F2 asks this before
/// spawning a second editor, exactly like F1 and the HELP window
/// (audit 2026-10-25's single-instance shape, one window over).
///
/// Windows: the live editor window class (`FindWindowW`). Unix: the
/// editor's runtime pid file with verified ownership — a stale file is
/// not an editor. The probe posts no message and closes nothing.
pub fn editor_is_open() -> bool {
    imp::editor_is_open()
}

/// Claim the HELP single-instance slot.
///
/// Unix: a **claimed** pid record — a live reader's file is refused, so
/// a race between two spawners still ends with one reader. Windows has
/// nothing to write: the class probe in [`help_is_open`] answers there,
/// and this is always `Ok`.
pub fn write_help_pid_file() -> Result<(), WindowError> {
    imp::write_help_pid_file()
}

/// Release the HELP single-instance slot, ignoring absence.
pub fn clear_help_pid_file() {
    imp::clear_help_pid_file();
}

/// Signal the owner of the PID file at `path`, Unix only.
///
/// The record's identity must verify against the live process before the
/// signal goes out; a stale or foreign file is unlinked instead of
/// signalled (audit P0-2). This is the editor's close path and it mirrors
/// the popup's byte for byte, only with a different file.
#[cfg(unix)]
fn signal_pid_file_owner(path: &std::path::Path) -> bool {
    let (status, record) = crate::platform::pidfile::status_of(path);
    match status {
        crate::platform::pidfile::PidStatus::Owned => {
            let Some(record) = record else {
                return false;
            };
            crate::platform::pidfile::signal_if_owned(record, libc::SIGTERM)
        }
        crate::platform::pidfile::PidStatus::Stale
        | crate::platform::pidfile::PidStatus::Foreign => {
            crate::platform::pidfile::unlink_stale(path);
            false
        }
        crate::platform::pidfile::PidStatus::Missing => false,
    }
}

#[cfg(target_os = "windows")]
mod imp {
    use super::{
        RawWindowHandle, WindowError, POPUP_CLOSE_POLL, POPUP_CLOSE_WAIT, POPUP_SPAWN_WAIT,
    };
    use windows::Win32::Foundation::HWND;
    use windows::Win32::Graphics::Gdi::CreateSolidBrush;
    use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
    use windows::Win32::UI::HiDpi::{
        SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GetForegroundWindow, GetWindowLongPtrW, GetWindowThreadProcessId,
        PeekMessageW, PostMessageW, SetClassLongPtrW, SetForegroundWindow, SetWindowLongPtrW,
        SetWindowPos, GCLP_HBRBACKGROUND, GWL_STYLE, HWND_TOPMOST, MSG, PM_NOREMOVE,
        SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WM_CLOSE,
    };

    /// Total time the foreground ladder keeps retrying before it gives up.
    const FOCUS_LADDER_BUDGET: std::time::Duration = std::time::Duration::from_millis(150);
    /// Pause between two foreground attempts.
    const FOCUS_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(15);

    /// Best-effort runtime DPI awareness; see [`super::ensure_dpi_awareness`].
    pub(super) fn ensure_dpi_awareness() {
        // SAFETY: `SetProcessDpiAwarenessContext` takes a predefined
        // context constant and reads no pointers; the result is discarded
        // on purpose (an error means the manifest already set a level).
        unsafe {
            // ERROR_ACCESS_DENIED just means the manifest (or a prior call)
            // already set it — the desired end state either way.
            let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        }
    }

    fn hwnd_of(raw: RawWindowHandle) -> Option<HWND> {
        match raw {
            RawWindowHandle::Win32(h) => Some(HWND(h.hwnd.get() as *mut std::ffi::c_void)),
            _ => None,
        }
    }

    pub(super) fn paint_black_before_show(raw: RawWindowHandle) -> Result<(), WindowError> {
        let Some(hwnd) = hwnd_of(raw) else {
            return Err(WindowError::Platform("not a Win32 window handle".into()));
        };
        // SAFETY: `CreateSolidBrush` takes a plain colour value and
        // returns a GDI handle; `SetClassLongPtrW` stores the handle in
        // the window class, which owns it from here on (see the note
        // below on why it is not deleted).
        unsafe {
            let brush = CreateSolidBrush(windows::Win32::Foundation::COLORREF(0x0000_0000));
            if brush.is_invalid() {
                return Err(WindowError::Platform("CreateSolidBrush failed".into()));
            }
            SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, brush.0 as isize);
            // The window class **stores** the brush handle, it does not
            // copy the GDI object, so deleting it here would leave the
            // class pointing at a dangling handle for the life of the
            // process (audit 2026-10-20 §3.18). The brush is deleted when
            // the process exits, with the rest of the GDI state.
            let _ = brush;
        }
        Ok(())
    }

    /// Owning thread of the current foreground window, 0 when unknown.
    fn foreground_tid(fg: HWND) -> u32 {
        if fg.is_invalid() {
            return 0;
        }
        // SAFETY: `GetWindowThreadProcessId` reads the window's thread id;
        // a null out-pointer asks for the thread only, so nothing is
        // written through a pointer.
        unsafe { GetWindowThreadProcessId(fg, None) }
    }

    /// One foreground attempt: direct set, then an `AttachThreadInput`
    /// retry against the owning thread. True when `hwnd` is foreground.
    fn try_foreground_once(hwnd: HWND, self_tid: u32) -> bool {
        // SAFETY: `SetForegroundWindow` takes a window handle and no
        // pointers; failure is an error code we discard.
        unsafe {
            let _ = SetForegroundWindow(hwnd);
        }
        // SAFETY: `GetForegroundWindow` returns a handle and takes none.
        if unsafe { GetForegroundWindow() } == hwnd {
            return true;
        }
        // SAFETY: see above.
        let fg_tid = foreground_tid(unsafe { GetForegroundWindow() });
        if fg_tid == 0 || fg_tid == self_tid {
            return false;
        }
        // SAFETY: the thread ids come from `GetCurrentThreadId` /
        // `GetWindowThreadProcessId`, and both threads own a message
        // queue (this one pumps, the foreground one owns a window); the
        // attach is undone on the spot.
        unsafe {
            let _ = AttachThreadInput(self_tid, fg_tid, true);
            let _ = SetForegroundWindow(hwnd);
            let _ = AttachThreadInput(self_tid, fg_tid, false);
            GetForegroundWindow() == hwnd
        }
    }

    /// Give the calling thread a message queue.
    ///
    /// `AttachThreadInput` refuses to attach a thread that has none, and a
    /// plain Rust worker thread has none until it pumps messages. A
    /// `PeekMessage` with `PM_NOREMOVE` creates the queue without
    /// consuming anything.
    fn ensure_message_queue() {
        let mut msg = MSG::default();
        // SAFETY: `PeekMessageW` writes one `MSG` into the buffer, which
        // is exactly that big, and `PM_NOREMOVE` leaves the queue intact.
        unsafe {
            let _ = PeekMessageW(&mut msg, None, 0, 0, PM_NOREMOVE);
        }
    }

    /// The foreground ladder, run on a worker thread so the show path never
    /// blocks (audit performance: the ladder used to sleep up to 150 ms
    /// inline, before the popup drew its first frame).
    ///
    /// The worker owns the whole `AttachThreadInput` dance: it gives itself
    /// a message queue and uses its own thread id, which is what the trick
    /// needs (the attached threads must own a message queue each).
    fn foreground_ladder(hwnd_addr: usize) {
        let hwnd = HWND(hwnd_addr as *mut std::ffi::c_void);
        ensure_message_queue();
        // SAFETY: `GetCurrentThreadId` takes no arguments and cannot fail.
        let self_tid = unsafe { GetCurrentThreadId() };
        let deadline = std::time::Instant::now() + FOCUS_LADDER_BUDGET;
        while std::time::Instant::now() < deadline {
            std::thread::sleep(FOCUS_RETRY_DELAY);
            if try_foreground_once(hwnd, self_tid) {
                return;
            }
        }
    }

    pub(super) fn focus_ladder(raw: RawWindowHandle, topmost: bool) {
        let Some(hwnd) = hwnd_of(raw) else {
            return;
        };
        if topmost {
            // SAFETY: `SetWindowPos` takes the window handle and the
            // predefined `HWND_TOPMOST` placement; the flags move and size
            // nothing, so there are no sizes to validate.
            unsafe {
                let _ = SetWindowPos(
                    hwnd,
                    Some(HWND_TOPMOST),
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE,
                );
            }
        }
        // SAFETY: see `foreground_ladder`.
        let self_tid = unsafe { GetCurrentThreadId() };
        // First attempt on the calling thread: it owns the window and its
        // message queue, so the foreground steal has the best chance here.
        if try_foreground_once(hwnd, self_tid) {
            return;
        }
        // Retries run on a short-lived worker so the caller returns at once
        // (audit performance). `HWND` is a raw pointer and not `Send`, so
        // the handle travels as an address and is rebuilt in the worker.
        let hwnd_addr = hwnd.0 as usize;
        let _ = std::thread::Builder::new()
            .name("ducks-focus".to_string())
            .spawn(move || foreground_ladder(hwnd_addr));
    }

    pub(super) fn ensure_borderless(raw: RawWindowHandle) {
        let Some(hwnd) = hwnd_of(raw) else {
            return;
        };
        // SAFETY: `GetWindowLongPtrW` reads the style, `SetWindowLongPtrW`
        // writes a style value derived from it (same width), and
        // `SetWindowPos` with these flags only recalculates the frame.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let clean = super::strip_chrome_bits(style);
            if clean != style {
                SetWindowLongPtrW(hwnd, GWL_STYLE, clean as isize);
            }
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
            );
        }
    }

    /// The editor's flavour: no caption, but the sizing border stays.
    ///
    /// `WS_THICKFRAME` is what makes the window drag-resizable, so it is
    /// kept while every other chrome bit goes (see
    /// [`super::strip_chrome_bits_keep_frame`]).
    pub(super) fn ensure_borderless_resizable(raw: RawWindowHandle) {
        let Some(hwnd) = hwnd_of(raw) else {
            return;
        };
        // SAFETY: the same calls as `ensure_borderless`, with the
        // keep-the-frame style mask; see there for the per-call argument.
        unsafe {
            let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
            let clean = super::strip_chrome_bits_keep_frame(style);
            if clean != style {
                SetWindowLongPtrW(hwnd, GWL_STYLE, clean as isize);
            }
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
            );
        }
    }

    pub(super) fn close_popup() -> bool {
        let closed = post_close(crate::platform::POPUP_WINDOW_CLASS);
        if closed {
            wait_until_popup_is_gone();
        }
        closed
    }

    /// Windows: wait for the spawned popup's window class to appear.
    pub(super) fn wait_for_popup_window() {
        let deadline = std::time::Instant::now() + POPUP_SPAWN_WAIT;
        while std::time::Instant::now() < deadline {
            if popup_is_open() {
                return;
            }
            std::thread::sleep(POPUP_CLOSE_POLL);
        }
    }

    pub(super) fn write_pid_file() -> Result<(), WindowError> {
        Ok(())
    }

    pub(super) fn clear_pid_file() {}

    /// Windows: find the editor's window by its class and ask it to close.
    ///
    /// The class is `"AGnosisEditor"` — never `"AGnosisPopup"` — so the parade
    /// popup is out of reach (roadmap Stage 4).
    pub(super) fn close_editor() -> bool {
        let closed = post_close(crate::platform::EDITOR_WINDOW_CLASS);
        if closed {
            wait_until_editor_is_gone();
        }
        closed
    }

    /// Is the draw-your-own editor on screen? A read-only probe.
    pub(super) fn editor_is_open() -> bool {
        window_of_class(crate::platform::EDITOR_WINDOW_CLASS).is_some()
    }

    /// Wait, briefly, for the spawned editor's window class to appear.
    ///
    /// The editor toggle needs the same guard the parade popup has: the
    /// editor is a **detached child**, so its window does not exist for
    /// tens of milliseconds after the spawn, and a double-tap of the
    /// editor chord took the spawn branch twice and opened a second
    /// editor (audit 2026-10-22 §P1-6).
    pub(super) fn wait_for_editor_window() {
        let deadline = std::time::Instant::now() + POPUP_SPAWN_WAIT;
        while std::time::Instant::now() < deadline {
            if editor_is_open() {
                return;
            }
            std::thread::sleep(POPUP_CLOSE_POLL);
        }
    }

    /// Wait, briefly, for the editor's window to disappear.
    ///
    /// The mirror image of [`wait_for_editor_window`], for the same
    /// reason the popup's close path waits.
    fn wait_until_editor_is_gone() {
        let deadline = std::time::Instant::now() + POPUP_CLOSE_WAIT;
        while std::time::Instant::now() < deadline {
            if !editor_is_open() {
                return;
            }
            std::thread::sleep(POPUP_CLOSE_POLL);
        }
    }

    /// Is the parade popup on screen? Windows: the window class lookup
    /// is a read-only probe — it posts nothing and closes nothing.
    pub(super) fn popup_is_open() -> bool {
        window_of_class(crate::platform::POPUP_WINDOW_CLASS).is_some()
    }

    /// Wait, briefly, for the popup's window to disappear.
    ///
    /// WHY: `PostMessageW(WM_CLOSE)` only *asks* the window to close. If
    /// the daemon answered the next hotkey press while the window was
    /// still there, it would post a second close instead of opening a new
    /// parade — the "off" state was missed on a double-tap (audit
    /// 2026-10-20 §3.11). Waiting for the window to go makes the toggle
    /// state exact.
    fn wait_until_popup_is_gone() {
        let deadline = std::time::Instant::now() + POPUP_CLOSE_WAIT;
        while std::time::Instant::now() < deadline {
            if !popup_is_open() {
                return;
            }
            std::thread::sleep(POPUP_CLOSE_POLL);
        }
    }

    /// Ask the window registered under `class` to close.
    ///
    /// `WM_CLOSE` is posted, never sent, so the receiving window closes
    /// itself — the editor's ESC handler and the popup's own teardown
    /// still run.
    fn post_close(class: &str) -> bool {
        let Some(hwnd) = window_of_class(class) else {
            return false;
        };
        // SAFETY: `PostMessageW` takes the found handle and plain
        // zeroed parameters; the message is queued, nothing is written
        // through a pointer.
        unsafe {
            PostMessageW(
                Some(hwnd),
                WM_CLOSE,
                windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(0),
            )
            .is_ok()
        }
    }

    /// The window registered under `class`, read-only.
    ///
    /// The lookup posts nothing, so a test may probe the desktop without
    /// closing anything the user has open.
    fn window_of_class(class: &str) -> Option<HWND> {
        let name = wide_null(class);
        // SAFETY: `FindWindowW` takes two NUL-terminated strings — the
        // class name is one and the window name is explicitly null — and
        // returns a handle it owns the lifetime of.
        let hwnd = unsafe {
            FindWindowW(
                windows::core::PCWSTR(name.as_ptr()),
                windows::core::PCWSTR::null(),
            )
        }
        .ok()?;
        if hwnd.is_invalid() {
            return None;
        }
        Some(hwnd)
    }

    /// `class` as a NUL-terminated UTF-16 string, as Win32 wants it.
    fn wide_null(class: &str) -> Vec<u16> {
        class.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// The read-only class lookup, as a plain number (see
    /// [`super::find_window_of_class`]).
    #[cfg(test)]
    pub(super) fn find_window_of_class(class: &str) -> Option<isize> {
        window_of_class(class).map(|hwnd| hwnd.0 as isize)
    }

    /// Windows keeps no PID file for the editor: it is found by class.
    pub(super) fn write_editor_pid_file() -> Result<(), WindowError> {
        Ok(())
    }

    pub(super) fn clear_editor_pid_file() {}

    /// Windows: is a HELP window on screen? Found by class, like the
    /// popup and the editor.
    pub(super) fn help_is_open() -> bool {
        window_of_class(crate::platform::HELP_WINDOW_CLASS).is_some()
    }

    /// Windows keeps no PID file for the HELP window either: the class
    /// probe in [`help_is_open`](super::help_is_open) answers.
    pub(super) fn write_help_pid_file() -> Result<(), WindowError> {
        Ok(())
    }

    pub(super) fn clear_help_pid_file() {}
}

#[cfg(target_os = "linux")]
mod imp {
    use super::{
        RawWindowHandle, WindowError, POPUP_CLOSE_POLL, POPUP_CLOSE_WAIT, POPUP_SPAWN_WAIT,
    };
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::*;

    fn xid_of(raw: RawWindowHandle) -> Option<u32> {
        match raw {
            RawWindowHandle::Xlib(h) => Some(h.window as u32),
            RawWindowHandle::Xcb(h) => Some(h.window.get()),
            _ => None,
        }
    }

    /// X11: `XSetWindowAttributes { background_pixel: 0 }` on the window
    /// XID via x11rb, flushed before the first map/show.
    pub(super) fn paint_black_before_show(raw: RawWindowHandle) -> Result<(), WindowError> {
        let Some(xid) = xid_of(raw) else {
            return Err(WindowError::Platform("not an X11 window handle".into()));
        };
        let (conn, _screen) =
            x11rb::connect(None).map_err(|e| WindowError::Platform(e.to_string()))?;
        conn.change_window_attributes(xid, &ChangeWindowAttributesAux::new().background_pixel(0))
            .map_err(|e| WindowError::Platform(e.to_string()))?
            .check()
            .map_err(|e| WindowError::Platform(e.to_string()))?;
        conn.flush()
            .map_err(|e| WindowError::Platform(e.to_string()))?;
        Ok(())
    }

    /// X11 needs no focus ladder: the window manager maps the override /
    /// topmost window on show.
    pub(super) fn focus_ladder(_raw: RawWindowHandle, _topmost: bool) {}

    /// X11 needs no post-show style fix: decorations are a window-manager
    /// hint applied at map time.
    pub(super) fn ensure_borderless(_raw: RawWindowHandle) {}

    /// X11 needs no post-show style fix either: the editor's invisible
    /// resize border is the window manager's own resize handle, and the
    /// editor asks for a resizable window when it creates it.
    pub(super) fn ensure_borderless_resizable(_raw: RawWindowHandle) {}

    /// Unix: signal the PID file owner only when identity verifies
    /// (audit P0-2). Stale/foreign files are unlinked, never signalled.
    pub(super) fn close_popup() -> bool {
        let path = match crate::paths::popup_pid_file() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let (status, record) = crate::platform::pidfile::status_of(&path);
        match status {
            crate::platform::pidfile::PidStatus::Owned => {
                let Some(record) = record else {
                    return false;
                };
                let signalled = crate::platform::pidfile::signal_if_owned(record, libc::SIGTERM);
                if signalled {
                    wait_until_popup_is_gone();
                }
                signalled
            }
            crate::platform::pidfile::PidStatus::Stale
            | crate::platform::pidfile::PidStatus::Foreign => {
                crate::platform::pidfile::unlink_stale(&path);
                false
            }
            crate::platform::pidfile::PidStatus::Missing => false,
        }
    }

    /// Unix: is the parade popup on screen? The popup's PID file names
    /// its owner, so "open" means the file is there and its owner is
    /// alive (the identity must verify — a stale file is not a popup).
    pub(super) fn popup_is_open() -> bool {
        let Ok(path) = crate::paths::popup_pid_file() else {
            return false;
        };
        let (status, _) = crate::platform::pidfile::status_of(&path);
        status == crate::platform::pidfile::PidStatus::Owned
    }

    /// Unix: wait for the spawned popup's PID file to appear.
    pub(super) fn wait_for_popup_window() {
        let deadline = std::time::Instant::now() + POPUP_SPAWN_WAIT;
        while std::time::Instant::now() < deadline {
            if popup_is_open() {
                return;
            }
            std::thread::sleep(POPUP_CLOSE_POLL);
        }
    }

    /// Wait, briefly, for the popup to go away (audit 2026-10-20 §3.11):
    /// `SIGTERM` is asynchronous, so the next hotkey press could arrive
    /// while the popup was still on screen and be answered with a second
    /// close instead of a new parade.
    fn wait_until_popup_is_gone() {
        let deadline = std::time::Instant::now() + POPUP_CLOSE_WAIT;
        while std::time::Instant::now() < deadline {
            if !popup_is_open() {
                return;
            }
            std::thread::sleep(POPUP_CLOSE_POLL);
        }
    }

    /// Unix: write our pid+starttime identity record.
    pub(super) fn write_pid_file() -> Result<(), WindowError> {
        let path =
            crate::paths::popup_pid_file().map_err(|e| WindowError::Platform(e.to_string()))?;
        crate::platform::pidfile::write_own_record(&path).map_err(WindowError::Platform)
    }

    /// Remove the runtime pid file, ignoring absence.
    pub(super) fn clear_pid_file() {
        if let Ok(path) = crate::paths::popup_pid_file() {
            // Only our own record goes: two instances must not unlink
            // each other's (audit 2026-10-22 §P2-12).
            let _ = crate::platform::pidfile::unlink_if_owned(&path, std::process::id() as i32);
        }
    }

    /// Unix: signal the editor's PID file owner only when identity
    /// verifies (audit P0-2). Stale/foreign files are unlinked, never
    /// signalled.
    pub(super) fn close_editor() -> bool {
        let path = match crate::paths::editor_pid_file() {
            Ok(p) => p,
            Err(_) => return false,
        };
        super::signal_pid_file_owner(&path)
    }

    /// Unix: wait, briefly, for the editor's PID file to appear.
    ///
    /// The editor writes its PID file before its window exists, and the
    /// double-tap guard needs *some* observable "the editor is coming"
    /// signal (audit 2026-10-22 §P1-6).
    pub(super) fn wait_for_editor_window() {
        let Ok(path) = crate::paths::editor_pid_file() else {
            return;
        };
        let deadline = std::time::Instant::now() + POPUP_SPAWN_WAIT;
        while std::time::Instant::now() < deadline {
            if path.exists() {
                return;
            }
            std::thread::sleep(POPUP_CLOSE_POLL);
        }
    }

    /// Unix: write our pid+starttime identity record for the editor.
    ///
    /// The record is **claimed**, not overwritten: a live editor's record
    /// is refused, so two editors cannot each end up believing they own
    /// the file (audit 2026-10-22 §P2-12).
    pub(super) fn write_editor_pid_file() -> Result<(), WindowError> {
        let path =
            crate::paths::editor_pid_file().map_err(|e| WindowError::Platform(e.to_string()))?;
        crate::platform::pidfile::write_own_record_if_free(&path).map_err(WindowError::Platform)
    }

    /// Remove the editor's runtime pid file, ignoring absence.
    pub(super) fn clear_editor_pid_file() {
        if let Ok(path) = crate::paths::editor_pid_file() {
            // Only our own record goes: two instances must not unlink
            // each other's (audit 2026-10-22 §P2-12).
            let _ = crate::platform::pidfile::unlink_if_owned(&path, std::process::id() as i32);
        }
    }

    /// Unix: is a HELP window open? The help process's pid file names
    /// its owner, so "open" means the file is there and its owner is
    /// alive (the identity must verify — a stale file is not a reader).
    pub(super) fn help_is_open() -> bool {
        let Ok(path) = crate::paths::help_pid_file() else {
            return false;
        };
        let (status, _) = crate::platform::pidfile::status_of(&path);
        status == crate::platform::pidfile::PidStatus::Owned
    }

    /// Unix: is the editor open? The editor's pid file names its owner,
    /// the same verified-ownership shape as [`help_is_open`].
    pub(super) fn editor_is_open() -> bool {
        let Ok(path) = crate::paths::editor_pid_file() else {
            return false;
        };
        let (status, _) = crate::platform::pidfile::status_of(&path);
        status == crate::platform::pidfile::PidStatus::Owned
    }

    /// Unix: claim the HELP single-instance slot.
    ///
    /// The record is **claimed**, not overwritten: a live reader's
    /// record is refused, so a race between two spawners still ends with
    /// one reader (the editor's rule, audit 2026-10-22 §P2-12).
    pub(super) fn write_help_pid_file() -> Result<(), WindowError> {
        let path =
            crate::paths::help_pid_file().map_err(|e| WindowError::Platform(e.to_string()))?;
        crate::platform::pidfile::write_own_record_if_free(&path).map_err(WindowError::Platform)
    }

    /// Remove the help runtime pid file, ignoring absence.
    pub(super) fn clear_help_pid_file() {
        if let Ok(path) = crate::paths::help_pid_file() {
            // Only our own record goes: two instances must not unlink
            // each other's (audit 2026-10-22 §P2-12).
            let _ = crate::platform::pidfile::unlink_if_owned(&path, std::process::id() as i32);
        }
    }

    /// No manifest equivalent needed: winit handles platform scaling
    /// natively once the window is created (audit 2026-10-06 note 4).
    pub(super) fn ensure_dpi_awareness() {}
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{
        RawWindowHandle, WindowError, POPUP_CLOSE_POLL, POPUP_CLOSE_WAIT, POPUP_SPAWN_WAIT,
    };
    use objc2::rc::Retained;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSColor, NSView};

    /// macOS: `backgroundColor = black`, `isOpaque = YES` on the `NSWindow`
    /// behind winit's `NSView` (rwh gives the view; `view.window()` is the
    /// window). Must run on the main thread.
    pub(super) fn paint_black_before_show(raw: RawWindowHandle) -> Result<(), WindowError> {
        let RawWindowHandle::AppKit(h) = raw else {
            return Err(WindowError::Platform("not an AppKit window handle".into()));
        };
        let _mtm = MainThreadMarker::new()
            .ok_or_else(|| WindowError::Platform("not on the main thread".into()))?;
        // SAFETY: `ns_view` is the AppKit view winit created and still
        // owns; `Retained::retain` takes our own strong reference for the
        // duration of this call, and the `NSWindow` it yields is
        // AppKit's own, only read and configured here on the main thread.
        unsafe {
            let view_ptr = h.ns_view.as_ptr().cast::<NSView>();
            let view: Retained<NSView> = Retained::retain(view_ptr)
                .ok_or_else(|| WindowError::Platform("NSView pointer is null".into()))?;
            let Some(window) = view.window() else {
                return Err(WindowError::Platform("NSView has no window yet".into()));
            };
            window.setBackgroundColor(Some(&NSColor::blackColor()));
            window.setOpaque(true);
        }
        Ok(())
    }

    /// macOS needs no focus ladder: activation policy + topmost level is
    /// handled at window-creation time.
    pub(super) fn focus_ladder(_raw: RawWindowHandle, _topmost: bool) {}

    /// macOS needs no post-show style fix: the borderless style is set on
    /// the `NSWindow` at creation time.
    pub(super) fn ensure_borderless(_raw: RawWindowHandle) {}

    /// macOS needs no post-show style fix either: AppKit's borderless
    /// `NSWindow` is resizable as long as the window was created
    /// resizable, and the editor's invisible resize border is AppKit's
    /// own.
    pub(super) fn ensure_borderless_resizable(_raw: RawWindowHandle) {}

    /// Unix: is the parade popup on screen? The popup's PID file names
    /// its owner, so "open" means the file is there and its owner is
    /// alive (the identity must verify — a stale file is not a popup).
    pub(super) fn popup_is_open() -> bool {
        let Ok(path) = crate::paths::popup_pid_file() else {
            return false;
        };
        let (status, _) = crate::platform::pidfile::status_of(&path);
        status == crate::platform::pidfile::PidStatus::Owned
    }

    /// Unix: wait for the spawned popup's PID file to appear.
    pub(super) fn wait_for_popup_window() {
        let deadline = std::time::Instant::now() + POPUP_SPAWN_WAIT;
        while std::time::Instant::now() < deadline {
            if popup_is_open() {
                return;
            }
            std::thread::sleep(POPUP_CLOSE_POLL);
        }
    }

    /// Wait, briefly, for the popup to go away (audit 2026-10-20 §3.11):
    /// `SIGTERM` is asynchronous, so the next hotkey press could arrive
    /// while the popup was still on screen and be answered with a second
    /// close instead of a new parade.
    fn wait_until_popup_is_gone() {
        let deadline = std::time::Instant::now() + POPUP_CLOSE_WAIT;
        while std::time::Instant::now() < deadline {
            if !popup_is_open() {
                return;
            }
            std::thread::sleep(POPUP_CLOSE_POLL);
        }
    }

    /// Unix: signal the PID file owner only when identity verifies
    /// (audit P0-2). Stale/foreign files are unlinked, never signalled.
    pub(super) fn close_popup() -> bool {
        let path = match crate::paths::popup_pid_file() {
            Ok(p) => p,
            Err(_) => return false,
        };
        let (status, record) = crate::platform::pidfile::status_of(&path);
        match status {
            crate::platform::pidfile::PidStatus::Owned => {
                let Some(record) = record else {
                    return false;
                };
                let signalled = crate::platform::pidfile::signal_if_owned(record, libc::SIGTERM);
                if signalled {
                    wait_until_popup_is_gone();
                }
                signalled
            }
            crate::platform::pidfile::PidStatus::Stale
            | crate::platform::pidfile::PidStatus::Foreign => {
                crate::platform::pidfile::unlink_stale(&path);
                false
            }
            crate::platform::pidfile::PidStatus::Missing => false,
        }
    }

    /// Unix: write our pid identity record.
    pub(super) fn write_pid_file() -> Result<(), WindowError> {
        let path =
            crate::paths::popup_pid_file().map_err(|e| WindowError::Platform(e.to_string()))?;
        crate::platform::pidfile::write_own_record(&path).map_err(WindowError::Platform)
    }

    /// Remove the runtime pid file, ignoring absence.
    pub(super) fn clear_pid_file() {
        if let Ok(path) = crate::paths::popup_pid_file() {
            // Only our own record goes: two instances must not unlink
            // each other's (audit 2026-10-22 §P2-12).
            let _ = crate::platform::pidfile::unlink_if_owned(&path, std::process::id() as i32);
        }
    }

    /// Unix: signal the editor's PID file owner only when identity
    /// verifies (audit P0-2). Stale/foreign files are unlinked, never
    /// signalled.
    pub(super) fn close_editor() -> bool {
        let path = match crate::paths::editor_pid_file() {
            Ok(p) => p,
            Err(_) => return false,
        };
        super::signal_pid_file_owner(&path)
    }

    /// Unix: wait, briefly, for the editor's PID file to appear.
    ///
    /// The editor writes its PID file before its window exists, and the
    /// double-tap guard needs *some* observable "the editor is coming"
    /// signal (audit 2026-10-22 §P1-6).
    pub(super) fn wait_for_editor_window() {
        let Ok(path) = crate::paths::editor_pid_file() else {
            return;
        };
        let deadline = std::time::Instant::now() + POPUP_SPAWN_WAIT;
        while std::time::Instant::now() < deadline {
            if path.exists() {
                return;
            }
            std::thread::sleep(POPUP_CLOSE_POLL);
        }
    }

    /// Unix: write our pid identity record for the editor.
    ///
    /// The record is **claimed**, not overwritten: a live editor's record
    /// is refused, so two editors cannot each end up believing they own
    /// the file (audit 2026-10-22 §P2-12).
    pub(super) fn write_editor_pid_file() -> Result<(), WindowError> {
        let path =
            crate::paths::editor_pid_file().map_err(|e| WindowError::Platform(e.to_string()))?;
        crate::platform::pidfile::write_own_record_if_free(&path).map_err(WindowError::Platform)
    }

    /// Remove the editor's runtime pid file, ignoring absence.
    pub(super) fn clear_editor_pid_file() {
        if let Ok(path) = crate::paths::editor_pid_file() {
            // Only our own record goes: two instances must not unlink
            // each other's (audit 2026-10-22 §P2-12).
            let _ = crate::platform::pidfile::unlink_if_owned(&path, std::process::id() as i32);
        }
    }

    /// Unix: is a HELP window open? The help process's pid file names
    /// its owner, so "open" means the file is there and its owner is
    /// alive (the identity must verify — a stale file is not a reader).
    pub(super) fn help_is_open() -> bool {
        let Ok(path) = crate::paths::help_pid_file() else {
            return false;
        };
        let (status, _) = crate::platform::pidfile::status_of(&path);
        status == crate::platform::pidfile::PidStatus::Owned
    }

    /// Unix: is the editor open? The editor's pid file names its owner,
    /// the same verified-ownership shape as [`help_is_open`].
    pub(super) fn editor_is_open() -> bool {
        let Ok(path) = crate::paths::editor_pid_file() else {
            return false;
        };
        let (status, _) = crate::platform::pidfile::status_of(&path);
        status == crate::platform::pidfile::PidStatus::Owned
    }

    /// Unix: claim the HELP single-instance slot.
    ///
    /// The record is **claimed**, not overwritten: a live reader's
    /// record is refused, so a race between two spawners still ends with
    /// one reader (the editor's rule, audit 2026-10-22 §P2-12).
    pub(super) fn write_help_pid_file() -> Result<(), WindowError> {
        let path =
            crate::paths::help_pid_file().map_err(|e| WindowError::Platform(e.to_string()))?;
        crate::platform::pidfile::write_own_record_if_free(&path).map_err(WindowError::Platform)
    }

    /// Remove the help runtime pid file, ignoring absence.
    pub(super) fn clear_help_pid_file() {
        if let Ok(path) = crate::paths::help_pid_file() {
            // Only our own record goes: two instances must not unlink
            // each other's (audit 2026-10-22 §P2-12).
            let _ = crate::platform::pidfile::unlink_if_owned(&path, std::process::id() as i32);
        }
    }

    /// No manifest equivalent needed: winit handles platform scaling
    /// natively once the window is created (audit 2026-10-06 note 4).
    pub(super) fn ensure_dpi_awareness() {}
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
mod imp {
    use super::{
        RawWindowHandle, WindowError, POPUP_CLOSE_POLL, POPUP_CLOSE_WAIT, POPUP_SPAWN_WAIT,
    };

    pub(super) fn paint_black_before_show(_raw: RawWindowHandle) -> Result<(), WindowError> {
        Ok(())
    }

    pub(super) fn focus_ladder(_raw: RawWindowHandle, _topmost: bool) {}

    pub(super) fn ensure_borderless(_raw: RawWindowHandle) {}

    pub(super) fn ensure_borderless_resizable(_raw: RawWindowHandle) {}

    pub(super) fn close_popup() -> bool {
        false
    }

    /// No window system to wait for.
    pub(super) fn wait_for_popup_window() {}

    pub(super) fn write_pid_file() -> Result<(), WindowError> {
        Ok(())
    }

    pub(super) fn clear_pid_file() {}

    /// No window system to find an editor in.
    pub(super) fn close_editor() -> bool {
        false
    }

    /// No window system, so there is nothing to wait for.
    pub(super) fn wait_for_editor_window() {}

    pub(super) fn write_editor_pid_file() -> Result<(), WindowError> {
        Ok(())
    }

    pub(super) fn clear_editor_pid_file() {}

    /// No window system to find an editor in.
    pub(super) fn editor_is_open() -> bool {
        false
    }

    /// No window system to find a HELP window in.
    pub(super) fn help_is_open() -> bool {
        false
    }

    pub(super) fn write_help_pid_file() -> Result<(), WindowError> {
        Ok(())
    }

    pub(super) fn clear_help_pid_file() {}

    /// No window system to be DPI-aware of.
    pub(super) fn ensure_dpi_awareness() {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_popup_with_no_popup_running_returns_false() {
        // The body is READ-ONLY and hermetic: it never clears the user's
        // PID file and never calls the live-desktop closer, so
        // `cargo test` can never shut the user's parade down (audit
        // 2026-10-21 §G9). The old body did both: `clear_pid_file()`
        // deleted the user's real `<config>/run/popup.pid`, and
        // `close_popup()` was `FindWindowW("AGnosisPopup")` +
        // `PostMessage(WM_CLOSE)` on Windows — it closed the live
        // parade, and the assertion then failed.
        //
        // What is under test now is the part that is safe to test: the
        // read-only probe never closes the parade it reports on, so
        // "no popup running" stays "no popup running".
        let first = popup_is_open();
        let second = popup_is_open();
        assert_eq!(
            first, second,
            "the probe must not close the parade it reports on"
        );
    }

    #[test]
    fn the_popup_probe_is_read_only() {
        // The probe posts no message and closes nothing, so a test that
        // runs beside the user's own windows cannot disturb them.
        let open = popup_is_open();
        assert_eq!(open, popup_is_open());
    }

    #[test]
    fn editor_window_class_is_not_the_popup_class() {
        // The class name is the whole separation between the two windows:
        // the parade toggle looks for "AGnosisPopup", the editor toggle for
        // "AGnosisEditor", so neither can find the other (roadmap Stage 4).
        assert_eq!(crate::platform::EDITOR_WINDOW_CLASS, "AGnosisEditor");
        assert_ne!(
            crate::platform::EDITOR_WINDOW_CLASS,
            crate::platform::POPUP_WINDOW_CLASS
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn editor_class_lookup_is_distinct_from_the_popup_class() {
        // The class name is the whole separation between the two windows:
        // the parade toggle looks for "AGnosisPopup", the editor toggle for
        // "AGnosisEditor", so neither can find the other (roadmap Stage 4).
        //
        // The lookup is READ-ONLY: it posts no message and closes
        // nothing, so this test cannot shut the user's editor while the
        // suite runs (the old `assert!(!close_editor())` test did exactly
        // that, and it also failed whenever an editor happened to be
        // open).
        let editor = find_window_of_class(crate::platform::EDITOR_WINDOW_CLASS);
        let popup = find_window_of_class(crate::platform::POPUP_WINDOW_CLASS);
        // Only compare when both windows exist — a missing window is a
        // legitimate state, not a failure.
        let (Some(editor), Some(popup)) = (editor, popup) else {
            return;
        };
        assert_ne!(
            editor, popup,
            "the editor and the parade popup must never be the same window"
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn the_class_lookup_never_posts_a_message() {
        // The read-only probe is what the tests use; it must not close
        // anything. Asking for a class that never exists proves the
        // lookup path itself is harmless.
        assert!(find_window_of_class("AGnosisNoSuchWindowClass").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn editor_pid_file_is_a_different_file_than_the_popup_one() {
        let editor = crate::paths::editor_pid_file().expect("editor pid file");
        let popup = crate::paths::popup_pid_file().expect("popup pid file");
        assert_ne!(editor, popup);
        assert!(editor.ends_with("editor.pid"));
    }

    #[test]
    fn chrome_detector_flags_caption_sysmenu_and_boxes() {
        assert!(style_has_chrome(CHROME_CAPTION));
        assert!(style_has_chrome(CHROME_SYSMENU));
        assert!(style_has_chrome(CHROME_MINIMIZEBOX));
        assert!(style_has_chrome(CHROME_MAXIMIZEBOX));
        assert!(style_has_chrome(CHROME_THICKFRAME));
        assert!(style_has_chrome(
            CHROME_CAPTION | CHROME_SYSMENU | CHROME_MINIMIZEBOX | CHROME_MAXIMIZEBOX
        ));
    }

    #[test]
    fn chrome_detector_passes_clean_borderless_style() {
        assert!(!style_has_chrome(STYLE_POPUP));
        assert!(!style_has_chrome(STYLE_POPUP | STYLE_VISIBLE));
        assert!(!style_has_chrome(0));
    }

    #[test]
    fn strip_removes_every_chrome_bit_and_sets_popup() {
        let dirty = CHROME_BITS | STYLE_VISIBLE;
        let clean = strip_chrome_bits(dirty);
        assert_eq!(clean & CHROME_BITS, 0);
        assert_ne!(clean & STYLE_POPUP, 0);
        assert_ne!(clean & STYLE_VISIBLE, 0);
        assert!(!style_has_chrome(clean));
    }

    #[test]
    fn strip_preserves_individual_chrome_bits() {
        for bit in [
            CHROME_CAPTION,
            CHROME_THICKFRAME,
            CHROME_MINIMIZEBOX,
            CHROME_MAXIMIZEBOX,
            CHROME_SYSMENU,
        ] {
            assert_eq!(strip_chrome_bits(bit) & bit, 0);
        }
    }

    #[test]
    fn strip_is_idempotent_on_clean_style() {
        let clean = STYLE_POPUP | STYLE_VISIBLE;
        assert_eq!(strip_chrome_bits(clean), clean);
    }

    #[test]
    fn live_captioned_style_strips_to_borderless() {
        let live: u32 = 0x16CB_0000;
        assert!(style_has_chrome(live));
        let clean = strip_chrome_bits(live);
        assert_eq!(clean & CHROME_BITS, 0);
        assert_ne!(clean & STYLE_POPUP, 0);
    }

    #[test]
    fn the_editor_style_keeps_the_sizing_border() {
        // The editor is borderless but must stay drag-resizable: the
        // sizing border (`WS_THICKFRAME`) survives, the caption does not.
        let dirty = CHROME_BITS | STYLE_VISIBLE;
        let clean = strip_chrome_bits_keep_frame(dirty);
        assert_eq!(
            clean & CHROME_THICKFRAME,
            CHROME_THICKFRAME,
            "the editor must keep the sizing border"
        );
        assert_eq!(
            clean & (CHROME_CAPTION | CHROME_SYSMENU | CHROME_MINIMIZEBOX | CHROME_MAXIMIZEBOX),
            0,
            "the editor must not keep any caption or button"
        );
        assert_ne!(clean & STYLE_POPUP, 0, "a borderless window is a popup");
        assert_ne!(clean & STYLE_VISIBLE, 0, "the visible bit is not chrome");
    }

    #[test]
    fn the_editor_style_gains_the_sizing_border_when_it_is_missing() {
        // `set_resizable(true)` adds the thick frame, but a style that
        // never had one must still end up resizable.
        let clean = strip_chrome_bits_keep_frame(STYLE_POPUP | STYLE_VISIBLE);
        assert_eq!(clean & CHROME_THICKFRAME, CHROME_THICKFRAME);
        assert_eq!(clean & CHROME_CAPTION, 0);
    }

    #[test]
    fn the_popup_style_still_loses_the_sizing_border() {
        // The parade popup covers the screen: it must not keep a sizing
        // border, or the user could resize it away from the monitor.
        let clean = strip_chrome_bits(CHROME_BITS | STYLE_VISIBLE);
        assert_eq!(
            clean & CHROME_THICKFRAME,
            0,
            "the popup must not keep the sizing border"
        );
        assert_eq!(clean & CHROME_BITS, 0);
    }

    #[test]
    fn the_editor_style_is_idempotent_on_clean_style() {
        let clean = strip_chrome_bits_keep_frame(CHROME_BITS | STYLE_VISIBLE);
        assert_eq!(strip_chrome_bits_keep_frame(clean), clean);
    }

    #[test]
    fn the_editor_style_keeps_the_sizing_border_of_a_live_captioned_style() {
        // The style a live Win32 window really carries: caption, thick
        // frame, sysmenu and the two boxes.
        let live: u32 = 0x16CB_0000;
        assert!(style_has_chrome(live));
        let clean = strip_chrome_bits_keep_frame(live);
        assert_eq!(clean & CHROME_THICKFRAME, CHROME_THICKFRAME);
        assert_eq!(clean & CHROME_CAPTION, 0);
        assert_ne!(clean & STYLE_POPUP, 0);
    }
}
