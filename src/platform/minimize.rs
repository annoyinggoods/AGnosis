//! Minimize every visible browser window, skipping our own popup — and
//! put the windows back when the parade is over.
//!
//! The shared contract: one process-name snapshot per call, matched
//! case-insensitively with `.exe` stripped.
//!
//! | Platform | Hide | Restore |
//! | --- | --- | --- |
//! | Windows | `EnumWindows` → skip non-visible, already-iconic (`IsIconic` — a taskbar-minimized window is already hidden and is **not recorded**, audit F-127), `WS_EX_TOOLWINDOW`, DWM-cloaked and our own windows → `GetWindowThreadProcessId` → match process base name (`GetModuleBaseNameW`, falling back to `QueryFullProcessImageNameW` which the limited query right promises, audit F-130) → `ShowWindow(hwnd, SW_MINIMIZE)`. | `ShowWindow(hwnd, SW_RESTORE)` on every `HWND` that was recorded. |
//! | Linux/X11 | Recursive walk of the root window's children → read `_NET_WM_PID` and the window title → match PID against the process snapshot → send `WM_CHANGE_STATE` with `IconicState`. Skip override-redirect and unmapped windows. | `WM_CHANGE_STATE` with `NormalState` (ICCCM) plus an EWMH `_NET_ACTIVE_WINDOW` request. |
//! | macOS | `NSWorkspace.sharedWorkspace.runningApplications` → match the app's executable name / bundle identifier → `hide`. Never shells out to `osascript`. | `-[NSRunningApplication unhide]` — the exact inverse of `hide:` — with `activateWithOptions:` as a fallback. |
//!
//! Audit 2026-10-17 (P1-7, P1-8):
//!
//! * **P1-7** — macOS ignored `browsers.names`. The effective list is now
//!   split into the built-in defaults and the user-configured names
//!   ([`is_default_browser_list`]): the built-ins are matched EXACTLY,
//!   exactly like the Windows/Linux process names, while only
//!   user-configured names may match loosely (localized name / bundle id).
//!   A short built-in name such as `tor` therefore no longer hides every
//!   app whose localized name happens to contain "tor".
//! * **P1-8** — the Linux kernel truncates `/proc/<pid>/comm` at 15
//!   characters. Every candidate name of a process (comm, `cmdline` argv[0]
//!   basename, `exe` basename) is matched, so a truncated comm no longer
//!   hides the browser.
//!
//! Audit 2026-10-20 (restore, module leak, own-window skip):
//!
//! * **Restore** — the browsers used to be minimized and NEVER brought
//!   back: there was no restore path anywhere in the codebase, so every
//!   parade left the user's windows minimized for the rest of the
//!   session. [`minimize_and_record`] now records what it hid and
//!   [`restore`] puts it back.
//! * **`dwmapi.dll` leak** — the module used to be `LoadLibrary`'d once
//!   per enumerated window and never `FreeLibrary`'d, so a machine with
//!   many windows leaked one module reference per window. It is loaded
//!   once per process now and reused.
//! * **Own-window skip** — [`is_own_window`] only compares against the
//!   pid the caller passes in, and the daemon passes *its own* pid while
//!   the parade popup and the draw-your-own editor are separate child
//!   processes, so it never matched them. The backends now recognize
//!   our own windows by their window class/title (Windows, X11) or by
//!   the executable name (macOS), and additionally skip the windows of
//!   the process that is doing the enumerating.

use agnosis_core::config::DEFAULT_BROWSERS;

/// Browser bundle identifiers minimized on macOS.
///
/// Safari, Chrome, Firefox, Edge-mac, Brave, Opera, Arc and the Tor
/// Browser, per the plan. Matched exactly (case-insensitive) — a bundle id
/// is never a substring match, so unrelated apps are not hidden.
///
/// The Tor Browser is listed here because its executable is named
/// `Tor Browser`, which the built-in name list does not carry: the
/// built-in names are matched exactly, so the bundle id is the only way
/// to recognize it.
#[cfg(any(target_os = "macos", test))]
pub const MACOS_BROWSER_BUNDLE_IDS: &[&str] = &[
    "com.apple.Safari",
    "com.google.Chrome",
    "org.mozilla.firefox",
    "com.microsoft.edgemac",
    "com.brave.Browser",
    "com.operasoftware.Opera",
    "com.operasoftware.Operagx",
    "company.thebrowser.Browser",
    "org.torproject.tor-browser",
    "ru.yandex.desktop.yandex-browser",
    "com.comodo.dragon",
    "com.avast.securebrowser",
    "com.avg.securebrowser",
    "com.apple.SafariTechnologyPreview",
];

/// Executable base name of our own app.
///
/// The daemon, the parade popup, the draw-your-own editor and the menu
/// bar are all the same binary, so this is the one name that means "this
/// is us" on the platforms where the backend identifies an application
/// by its executable (macOS hides whole applications, not windows).
#[cfg(any(target_os = "macos", test))]
pub const OWN_EXE_NAME: &str = "ducks";

/// Failures from [`minimize_browsers`]; the backend never fails hard on a
/// single bad window, so this only carries platform-level errors.
#[derive(Debug, thiserror::Error)]
pub enum MinimizeError {
    /// Platform backend failure with a message.
    ///
    /// The macOS backend has no platform-level failure mode (the AppKit
    /// calls used here cannot fail), so the variant is only constructed by
    /// the backends that can report one.
    #[error("{0}")]
    #[cfg_attr(not(any(target_os = "windows", target_os = "linux")), allow(dead_code))]
    Platform(String),
}

/// What the minimize pass hid, so a restore can put it back.
///
/// The payload is per platform: the `HWND`s that were minimized
/// (Windows), the X11 window ids that were iconified (Linux) or the
/// process ids of the applications that were hidden (macOS). Only the
/// windows the minimize pass really hid are recorded, so a restore
/// never touches a window the user had already put away.
///
/// The value is `Send`/`Sync` on every platform: the Windows payload
/// keeps the raw handle as a `usize` because `HWND` wraps a raw
/// pointer, which is neither.
#[derive(Clone, Debug, Default)]
pub struct Minimized {
    hidden: imp::Hidden,
}

impl Minimized {
    /// The number of windows that were hidden.
    pub fn len(&self) -> usize {
        self.hidden.len()
    }

    /// Was nothing hidden?
    // The restore wiring belongs to the daemon (audit 2026-10-20); until
    // it calls this, the binary crate would warn about the unused entry
    // point.
    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.hidden.is_empty()
    }
}

/// Normalize one executable name for comparison: lowercase, `.exe` stripped.
pub fn normalize_process_name(exe_name: &str) -> String {
    let lower = exe_name.trim().to_ascii_lowercase();
    lower.strip_suffix(".exe").unwrap_or(&lower).to_string()
}

/// True when `exe_name` names one of `browsers` exactly (after
/// normalization). Substrings never match: `mychrome` is not `chrome`.
/// An empty browser list falls back to [`DEFAULT_BROWSERS`].
pub fn process_name_matches(exe_name: &str, browsers: &[String]) -> bool {
    let name = normalize_process_name(exe_name);
    if name.is_empty() {
        return false;
    }
    if browsers.is_empty() {
        return DEFAULT_BROWSERS.contains(&name.as_str());
    }
    browsers.iter().any(|b| normalize_process_name(b) == name)
}

/// True when a window owned by `window_pid` belongs to the process the
/// caller named as "ours".
///
/// **What this deliberately does not do:** it does not recognize the
/// parade popup or the draw-your-own editor. Those are *separate
/// processes* — the daemon spawns them as children — so comparing their
/// pid against the daemon's own pid can never match (audit 2026-10-20).
/// The per-OS backends therefore recognize our own windows by their
/// window class/title instead, and [`is_our_window`] adds the pid of the
/// process that is doing the enumerating.
pub fn is_own_window(window_pid: u32, own_pid: u32) -> bool {
    window_pid == own_pid
}

/// True when a window belongs to us: either to the process the caller
/// named (`own_pid`) or to the process that is enumerating right now.
///
/// The second test is the one that actually skips a window of the
/// *calling* process (audit 2026-10-20). The daemon passes its own pid,
/// so both tests agree there; a caller that enumerates from inside the
/// popup would otherwise hide the parade it is showing.
pub(crate) fn is_our_window(window_pid: u32, own_pid: u32) -> bool {
    is_own_window(window_pid, own_pid) || window_pid == std::process::id()
}

/// True when the executable name is our own binary.
///
/// The macOS backend works per *application*, so the executable name is
/// the only identity that can tell "a browser we must not hide" from
/// "our own parade".
#[cfg(any(target_os = "macos", test))]
pub fn is_own_exe_name(exe_name: &str) -> bool {
    exe_name.trim().eq_ignore_ascii_case(OWN_EXE_NAME)
}

/// True when a window is **ours to hide and ours to restore**: the pure
/// predicate behind the Windows pass's guard clauses.
///
/// A window the user already put away — `IsIconic`, taskbar-minimized —
/// is not recordable: it is already hidden, and a later restore would
/// drag it back out of the taskbar against the user's wishes (audit
/// F-127: the restore invariant "never touch a window the user had
/// already put away" is only as good as what the pass records). Tool
/// windows and DWM-cloaked windows are not ours either, and neither is
/// an invisible one.
#[cfg(any(target_os = "windows", test))]
pub fn recordable_browser_window(
    visible: bool,
    iconic: bool,
    tool_window: bool,
    cloaked: bool,
) -> bool {
    visible && !iconic && !tool_window && !cloaked
}

/// Basename of a `/`-separated path (Linux argv[0]/exe fallback, audit
/// P1-8; macOS app executable path). Pure, so it is unit-tested on every
/// host; the Linux and macOS backends are the callers.
#[cfg(any(target_os = "linux", target_os = "macos", test))]
pub fn file_basename(path: &str) -> Option<String> {
    let base = path.rsplit('/').next()?.trim();
    if base.is_empty() {
        return None;
    }
    Some(base.to_string())
}

/// True when a custom macOS browser name hits an app (audit P1-7): exact
/// localized-name or bundle-id match, localized-name containment (e.g.
/// `chrome` hits `Google Chrome`), or bundle-id tail match (e.g. `chrome`
/// hits `com.google.Chrome`). Pure, so it is unit-tested on every host;
/// the macOS backend is the caller.
#[cfg(any(target_os = "macos", test))]
pub fn custom_browser_name_matches(want: &str, localized: &str, bundle_id: &str) -> bool {
    let w = normalize_process_name(want);
    if w.is_empty() {
        return false;
    }
    let local = localized.trim().to_ascii_lowercase();
    let bundle = bundle_id.trim().to_ascii_lowercase();
    if local == w || bundle == w {
        return true;
    }
    if local.contains(&w) {
        return true;
    }
    bundle.rsplit('.').next().is_some_and(|tail| tail == w)
}

/// Candidate process names of one process, in priority order (audit P1-8).
///
/// The kernel truncates `/proc/<pid>/comm` at 15 characters, so a long
/// name (`com.google.Chrom`, `org.mozilla.firef`) never matches the browser
/// list on its own. The `cmdline` argv[0] and the `exe` link target carry
/// the full name, so a process counts as a browser when ANY of its
/// candidates matches.
///
/// `comm` is a bare name; `cmdline` and `exe` are paths and are reduced to
/// their basename.
#[cfg(any(target_os = "linux", test))]
pub fn process_name_candidates(
    comm: Option<&str>,
    cmdline: Option<&str>,
    exe: Option<&str>,
) -> Vec<String> {
    let comm_name = comm.map(str::trim).filter(|name| !name.is_empty());
    let path_names = [cmdline, exe]
        .into_iter()
        .flatten()
        .filter_map(file_basename);
    comm_name
        .map(str::to_string)
        .into_iter()
        .chain(path_names)
        .collect()
}

/// True when any candidate process name matches the browser list (audit
/// P1-8): the list is not short-circuited on the first name that exists.
#[cfg(any(target_os = "linux", test))]
pub fn any_candidate_matches(candidates: &[String], browsers: &[String]) -> bool {
    candidates
        .iter()
        .any(|name| process_name_matches(name, browsers))
}

/// True when the effective browser list is the untouched built-in default
/// list, i.e. the user configured no browser names of their own.
///
/// `BrowsersConfig::effective()` hands the built-in defaults to the
/// backend when the user configured nothing, so "the list is the defaults"
/// is the only signal available here that a name is built-in rather than
/// user configured (audit P1-7). An empty list means the built-in
/// defaults, matching [`process_name_matches`].
#[cfg(any(target_os = "macos", test))]
pub fn is_default_browser_list(names: &[String]) -> bool {
    if names.is_empty() {
        return true;
    }
    names.len() == DEFAULT_BROWSERS.len()
        && DEFAULT_BROWSERS
            .iter()
            .all(|d| names.iter().any(|n| n == d))
}

/// True when a macOS application is one of the configured browsers
/// (audit P1-7).
///
/// * the executable base name — the macOS analogue of the Windows/Linux
///   process name — is matched EXACTLY against the effective list, exactly
///   like [`process_name_matches`];
/// * the built-in bundle ids always match (exact, case-insensitive);
/// * the loose localized-name / bundle-id rules apply to USER-CONFIGURED
///   names only. A short built-in name such as `tor` would otherwise
///   match every app whose localized name merely contains "tor"
///   ("…Editor"), which is why the built-in names are never matched
///   loosely.
#[cfg(any(target_os = "macos", test))]
pub fn macos_app_matches(
    browsers: &[String],
    built_in_bundle_ids: &[&str],
    exe_name: &str,
    localized: &str,
    bundle_id: &str,
) -> bool {
    if process_name_matches(exe_name, browsers) {
        return true;
    }
    if built_in_bundle_ids
        .iter()
        .any(|id| id.eq_ignore_ascii_case(bundle_id.trim()))
    {
        return true;
    }
    if is_default_browser_list(browsers) {
        return false;
    }
    browsers
        .iter()
        .any(|want| custom_browser_name_matches(want, localized, bundle_id))
}

/// Minimize every visible browser window, skipping our own popup/pid.
///
/// Returns the count minimized. One process-name snapshot per call, matched
/// case-insensitively with `.exe` stripped.
///
/// This is a thin wrapper over [`minimize_and_record`]: the count is the
/// number of recorded windows. The daemon keeps calling this function, so
/// the wire-up of the restore lives in [`minimize_and_record`].
#[cfg(test)]
pub fn minimize_browsers(browsers: &[String], own_pid: u32) -> Result<usize, MinimizeError> {
    Ok(minimize_and_record(browsers, own_pid)?.len())
}

/// Hide the browser windows AND record what was hidden, so [`restore`]
/// can put it back.
///
/// The record holds only what was really hidden, so a restore never
/// touches a window the user had already put away, and a restore of an
/// empty record is a no-op.
pub fn minimize_and_record(browsers: &[String], own_pid: u32) -> Result<Minimized, MinimizeError> {
    Ok(Minimized {
        hidden: imp::minimize_and_record(browsers, own_pid)?,
    })
}

/// Put back the windows that [`minimize_and_record`] recorded.
///
/// Never fails and never panics: a window that died in the meantime, a
/// missing window manager or an app that quit are all "nothing to
/// restore" for that one window, and the rest of the record is still
/// restored.
///
/// Platform notes:
///
/// * Windows — `ShowWindow(hwnd, SW_RESTORE)` on every recorded handle.
/// * Linux/X11 — `WM_CHANGE_STATE` with `NormalState` (the ICCCM way to
///   ask for a de-iconify) plus an EWMH `_NET_ACTIVE_WINDOW` request,
///   because not every window manager honours the ICCCM request.
/// * macOS — `-[NSRunningApplication unhide]`, the exact inverse of the
///   `hide:` the minimize pass sent. It is an *application* level
///   operation: macOS has no per-window "unhide", so the whole app comes
///   back (its own windows, in the state AppKit kept them) and the app is
///   not re-activated — it returns behind the other windows. An app that
///   cannot be unhidden is activated as the last resort.
// The restore wiring belongs to the daemon (audit 2026-10-20): the
// daemon holds the record between the parade opening and closing. Until
// it calls this, the binary crate would warn about the unused entry
// point.
#[allow(dead_code)]
pub fn restore(minimized: &Minimized) {
    imp::restore(&minimized.hidden);
}

#[cfg(target_os = "windows")]
// The restore half of the backend is called by the daemon once it owns
// the record (audit 2026-10-20); until then it is unused.
#[allow(dead_code)]
mod imp {
    use super::{is_our_window, process_name_matches, MinimizeError};
    use std::collections::HashMap;
    use std::sync::OnceLock;
    use windows::Win32::Foundation::{HANDLE, HMODULE, HWND, LPARAM};
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    use windows::Win32::System::ProcessStatus::GetModuleBaseNameW;
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindowLongW, GetWindowThreadProcessId, IsIconic, IsWindow,
        IsWindowVisible, ShowWindow, GWL_EXSTYLE, SW_MINIMIZE, SW_RESTORE, WS_EX_TOOLWINDOW,
    };

    /// The windows this process hid, as raw `HWND` values.
    ///
    /// The handle is kept as a `usize` and not as an `HWND`, because
    /// `HWND` wraps a raw pointer and is therefore neither `Send` nor
    /// `Sync`, while the daemon holds this record between the minimize
    /// and the restore (audit 2026-10-20).
    #[derive(Clone, Debug, Default)]
    pub(super) struct Hidden {
        windows: Vec<usize>,
    }

    impl Hidden {
        pub(super) fn len(&self) -> usize {
            self.windows.len()
        }

        pub(super) fn is_empty(&self) -> bool {
            self.windows.is_empty()
        }
    }

    struct Ctx<'a> {
        browsers: &'a [String],
        own_pid: u32,
        names: HashMap<u32, String>,
        hidden: Vec<usize>,
    }

    /// `DWMWA_CLOAKED` attribute id; `DwmGetWindowAttribute` lives in
    /// `dwmapi.dll`, which is not in the pinned `windows` feature set, so it
    /// is loaded dynamically and a missing DLL simply means "not cloaked".
    const DWMWA_CLOAKED: u32 = 14;

    /// Full `DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &cloaked, size)`.
    type DwmGetter =
        unsafe extern "system" fn(HWND, u32, *mut std::ffi::c_void, u32) -> windows::core::HRESULT;

    /// The `dwmapi.dll` module handle, loaded once per process.
    ///
    /// The module used to be `LoadLibrary`'d once per enumerated window
    /// and never `FreeLibrary`'d, so every window on the desktop leaked
    /// one module reference (audit 2026-10-20). It is loaded once here
    /// and kept for the lifetime of the process: the Windows loader
    /// unmaps it when the process exits, so no explicit `FreeLibrary` is
    /// needed.
    ///
    /// The handle is stored as a `usize` because `HMODULE` wraps a raw
    /// pointer and is therefore not `Sync`, which a `OnceLock` requires.
    static DWMAPI_MODULE: OnceLock<usize> = OnceLock::new();

    /// The `DwmGetWindowAttribute` function pointer, or `None` when
    /// `dwmapi.dll` (or the export) is unavailable — a missing DLL simply
    /// means "not cloaked".
    fn dwm_getter() -> Option<DwmGetter> {
        let raw = *DWMAPI_MODULE.get_or_init(|| {
            // SAFETY: `LoadLibraryW` takes a NUL-terminated wide string and
            // returns the module handle; the name is a compile-time
            // literal, so it is well-formed.
            match unsafe { LoadLibraryW(windows::core::w!("dwmapi.dll")) } {
                Ok(module) => module.0 as usize,
                Err(_) => 0,
            }
        });
        if raw == 0 {
            return None;
        }
        // SAFETY: `GetProcAddress` takes the module handle that
        // `LoadLibraryW` returned and a NUL-terminated ASCII name; it
        // yields `None` when the export does not exist.
        let proc = unsafe {
            GetProcAddress(
                HMODULE(raw as *mut std::ffi::c_void),
                windows::core::s!("DwmGetWindowAttribute"),
            )
        }?;
        // SAFETY: the export is documented to have the signature of
        // `DwmGetter`; this is the standard FARPROC-to-typed-pointer
        // cast that `GetProcAddress` requires.
        let get: DwmGetter = unsafe { std::mem::transmute(proc) };
        Some(get)
    }

    fn is_cloaked(hwnd: HWND) -> bool {
        let Some(get) = dwm_getter() else {
            return false;
        };
        let mut cloaked: u32 = 0;
        // SAFETY: `DwmGetWindowAttribute` writes at most
        // `size_of::<u32>()` bytes into the buffer that is passed, and the
        // buffer really is that big.
        let r = unsafe {
            get(
                hwnd,
                DWMWA_CLOAKED,
                &mut cloaked as *mut u32 as *mut std::ffi::c_void,
                std::mem::size_of::<u32>() as u32,
            )
        };
        r.is_ok() && cloaked != 0
    }

    /// True when the window is one of ours: the parade popup, the
    /// draw-your-own editor or the HELP window.
    ///
    /// All three are child processes of the daemon, so a pid comparison
    /// can never name them; the Win32 window *class* is the one identity
    /// they carry across the process boundary (audit 2026-10-20).
    fn is_our_class(hwnd: HWND) -> bool {
        let mut buf = [0u16; 64];
        // SAFETY: `GetClassNameW` writes at most `buf.len()` wide
        // characters into the buffer and returns how many it wrote.
        let len = unsafe { GetClassNameW(hwnd, &mut buf) };
        if len <= 0 {
            return false;
        }
        let class = String::from_utf16_lossy(&buf[..len as usize]);
        class == crate::platform::POPUP_WINDOW_CLASS
            || class == crate::platform::EDITOR_WINDOW_CLASS
            || class == crate::platform::HELP_WINDOW_CLASS
    }

    /// The base name of one process's image, with a per-call cache.
    fn process_name(pid: u32, cache: &mut HashMap<u32, String>) -> Option<String> {
        if let Some(name) = cache.get(&pid) {
            return Some(name.clone());
        }
        let name = query_process_name(pid)?;
        cache.insert(pid, name.clone());
        Some(name)
    }

    /// Query one process's base name with the limited right.
    ///
    /// `GetModuleBaseNameW` is only documented for a handle opened with
    /// `PROCESS_QUERY_INFORMATION | PROCESS_VM_READ`; this pass asks for
    /// the weaker `PROCESS_QUERY_LIMITED_INFORMATION`, because elevated
    /// and protected processes refuse the stronger rights — and then the
    /// documented-for-limited `QueryFullProcessImageNameW` is the
    /// fallback. No silent skip: a process neither call can read is
    /// genuinely unreadable (audit F-130).
    fn query_process_name(pid: u32) -> Option<String> {
        // SAFETY: `OpenProcess` returns a handle the two queries accept,
        // or an error that becomes `None`; the handle is closed on every
        // path, and nothing outlives the call.
        unsafe {
            let handle: HANDLE = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let name = module_base_name(handle).or_else(|| image_base_name(handle));
            let _ = windows::Win32::Foundation::CloseHandle(handle);
            name
        }
    }

    /// The process's module base name (`chrome.exe`), the way the taskbar
    /// shows it.
    fn module_base_name(handle: HANDLE) -> Option<String> {
        // SAFETY: `GetModuleBaseNameW` writes at most `buf.len()` wide
        // characters into the buffer and returns how many it wrote; the
        // handle comes from `OpenProcess` above.
        unsafe {
            let mut buf = [0u16; 260];
            let len = GetModuleBaseNameW(handle, None, &mut buf);
            (len != 0).then(|| String::from_utf16_lossy(&buf[..len as usize]))
        }
    }

    /// The basename of the process's full image path.
    ///
    /// `QueryFullProcessImageNameW` is documented to work with
    /// `PROCESS_QUERY_LIMITED_INFORMATION` (audit F-130), so it is the
    /// fallback when the module-base query returns 0.
    fn image_base_name(handle: HANDLE) -> Option<String> {
        // SAFETY: `QueryFullProcessImageNameW` writes at most `buf.len()`
        // wide characters into the buffer and reports the real length in
        // `len`; the handle comes from `OpenProcess` above.
        let path = unsafe {
            let mut buf = [0u16; 1024];
            let mut len = buf.len() as u32;
            QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_WIN32,
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut len,
            )
            .ok()?;
            String::from_utf16_lossy(&buf[..len as usize])
        };
        windows_path_basename(&path)
    }

    /// Basename of a Windows path, either separator (testable pure half
    /// of the F-130 fallback).
    pub(super) fn windows_path_basename(path: &str) -> Option<String> {
        let base = path.rsplit(['\\', '/']).next()?.trim();
        (!base.is_empty()).then(|| base.to_string())
    }

    /// Per-window decision for the Windows backend: every check is a guard
    /// clause, so the body stays at nesting depth 1 (audit nesting rule).
    ///
    /// The recordable predicate (see
    /// [`super::recordable_browser_window`]) carries the F-127 policy: a
    /// window that is **already iconic** — taskbar-minimized by the user —
    /// is skipped AND kept out of the record, so the restore can never
    /// drag it back out of the taskbar.
    unsafe fn minimize_window(hwnd: HWND, ctx: &mut Ctx<'_>) {
        unsafe {
            let ex = GetWindowLongW(hwnd, GWL_EXSTYLE);
            let recordable = super::recordable_browser_window(
                IsWindowVisible(hwnd).as_bool(),
                IsIconic(hwnd).as_bool(),
                (ex as u32 & WS_EX_TOOLWINDOW.0) != 0,
                is_cloaked(hwnd),
            );
            if !recordable {
                return;
            }
            if is_our_class(hwnd) {
                return;
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if is_our_window(pid, ctx.own_pid) {
                return;
            }
            let Some(name) = process_name(pid, &mut ctx.names) else {
                return;
            };
            if !process_name_matches(&name, ctx.browsers) {
                return;
            }
            let _ = ShowWindow(hwnd, SW_MINIMIZE);
            ctx.hidden.push(hwnd.0 as usize);
        }
    }

    unsafe extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> windows::core::BOOL {
        // SAFETY: `lparam` is the `Ctx` pointer `EnumWindows` was handed;
        // the enumeration is synchronous, so the context outlives every
        // callback invocation and no two callbacks run at once.
        let ctx = unsafe { &mut *(lparam.0 as *mut Ctx<'_>) };
        unsafe {
            minimize_window(hwnd, ctx);
        }
        true.into()
    }

    pub(super) fn minimize_and_record(
        browsers: &[String],
        own_pid: u32,
    ) -> Result<Hidden, MinimizeError> {
        let mut ctx = Ctx {
            browsers,
            own_pid,
            names: HashMap::new(),
            hidden: Vec::new(),
        };
        unsafe {
            EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut Ctx<'_> as isize))
                .map_err(|e| MinimizeError::Platform(e.to_string()))?;
        }
        Ok(Hidden {
            windows: ctx.hidden,
        })
    }

    /// Restore one recorded window, if it is still alive.
    fn restore_one(raw: usize) {
        let hwnd = HWND(raw as *mut std::ffi::c_void);
        unsafe {
            if !IsWindow(Some(hwnd)).as_bool() {
                return;
            }
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
    }

    pub(super) fn restore(hidden: &Hidden) {
        for raw in &hidden.windows {
            restore_one(*raw);
        }
    }
}

#[cfg(target_os = "linux")]
// The X11 window walk passes the connection, the window, the root and the
// atoms to every step. Bundling them into a context struct would only move
// the same fields around, so the lint is silenced here rather than paid for
// at every call site.
#[allow(clippy::too_many_arguments)]
// The restore half of the backend is called by the daemon once it owns
// the record (audit 2026-10-20); until then it is unused.
#[allow(dead_code)]
mod imp {
    use super::{any_candidate_matches, is_our_window, process_name_candidates, MinimizeError};
    use std::collections::HashMap;
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::*;
    use x11rb::rust_connection::RustConnection;

    /// The windows this process iconified, as X11 window ids.
    #[derive(Clone, Debug, Default)]
    pub(super) struct Hidden {
        windows: Vec<Window>,
    }

    impl Hidden {
        pub(super) fn len(&self) -> usize {
            self.windows.len()
        }

        pub(super) fn is_empty(&self) -> bool {
            self.windows.is_empty()
        }
    }

    /// Window-manager iconify request: `WM_CHANGE_STATE` with `IconicState`
    /// (format-32 client message). `_NET_WM_STATE_MINIMIZED` is the state the
    /// window manager sets afterwards, not the request we send.
    const ICONIC_STATE: u32 = 3;

    /// Window-manager de-iconify request: `WM_CHANGE_STATE` with
    /// `NormalState` (ICCCM 4.1.4) — the inverse of [`ICONIC_STATE`].
    const NORMAL_STATE: u32 = 0;

    /// EWMH `_NET_ACTIVE_WINDOW` source indication "application".
    ///
    /// The ICCCM de-iconify request is not honoured by every window
    /// manager, so the EWMH request goes out as well. Source indication
    /// `2` is the one a real application would use, so the window manager
    /// raises the window and gives it the focus — the same visible result
    /// as Windows' `SW_RESTORE`.
    const NET_ACTIVE_WINDOW_SOURCE_APPLICATION: u32 = 2;

    fn atom(conn: &RustConnection, name: &[u8]) -> Option<Atom> {
        conn.intern_atom(false, name)
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
    }

    fn window_pid(conn: &RustConnection, win: Window, pid_atom: Atom) -> Option<u32> {
        let reply = conn
            .get_property(false, win, pid_atom, AtomEnum::CARDINAL, 0, 1)
            .ok()?
            .reply()
            .ok()?;
        // `value32` yields an iterator over the 32-bit property words; the
        // pid is the first one. The iterator borrows the reply, so it is
        // bound to a local that is dropped before the reply itself.
        let mut words = reply.value32()?;
        words.next()
    }

    /// The `WM_NAME` of a window, used to recognize our own popup/editor
    /// windows: they are titled with their title constants (see
    /// `platform::POPUP_WINDOW_TITLE` — the class is a Win32 concept and
    /// never appears in an X11 title).
    fn window_title(conn: &RustConnection, win: Window) -> Option<String> {
        let reply = conn
            .get_property(false, win, AtomEnum::WM_NAME, AtomEnum::STRING, 0, 256)
            .ok()?
            .reply()
            .ok()?;
        Some(String::from_utf8_lossy(&reply.value).into_owned())
    }

    /// True when the window is one of ours: the parade popup, the
    /// draw-your-own editor or the HELP window.
    ///
    /// All three are child processes of the daemon, so a pid comparison
    /// can never name them; the window **title** is the one identity
    /// they carry across the process boundary (audit 2026-10-20). The
    /// comparison is against the title constants
    /// (`platform::POPUP_WINDOW_TITLE` and friends) — audit 2026-10-25:
    /// it used to compare against the *class-name* constants, which no
    /// X11 title ever carried, so the arm could never match.
    fn is_our_title(title: &str) -> bool {
        title == crate::platform::POPUP_WINDOW_TITLE
            || title == crate::platform::EDITOR_WINDOW_TITLE
            || title == crate::platform::HELP_WINDOW_TITLE
    }

    /// All candidate names of one process (audit P1-8).
    ///
    /// `/proc/<pid>/comm` is truncated at 15 chars by the kernel, so the
    /// comm alone never matches a long browser name. The `cmdline` argv[0]
    /// and the `exe` link target carry the full name, so all three are
    /// kept and every one of them is matched.
    fn process_names(pid: u32, cache: &mut HashMap<u32, Vec<String>>) -> Vec<String> {
        if let Some(names) = cache.get(&pid) {
            return names.clone();
        }
        let names = process_name_candidates(
            read_comm(pid).as_deref(),
            read_cmdline_argv0(pid).as_deref(),
            read_exe_target(pid).as_deref(),
        );
        cache.insert(pid, names.clone());
        names
    }

    /// `/proc/<pid>/comm` (truncated at 15 chars).
    fn read_comm(pid: u32) -> Option<String> {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .ok()?
            .trim()
            .to_string();
        if comm.is_empty() {
            return None;
        }
        Some(comm)
    }

    /// `/proc/<pid>/cmdline` argv[0] (NUL-separated) — a path, reduced to
    /// its basename by [`process_name_candidates`].
    fn read_cmdline_argv0(pid: u32) -> Option<String> {
        let bytes = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let argv0 = bytes.split(|b| *b == 0).next()?;
        if argv0.is_empty() {
            return None;
        }
        std::str::from_utf8(argv0).ok().map(str::to_string)
    }

    /// Target of the `/proc/<pid>/exe` symlink — a path, reduced to its
    /// basename by [`process_name_candidates`].
    fn read_exe_target(pid: u32) -> Option<String> {
        let target = std::fs::read_link(format!("/proc/{pid}/exe")).ok()?;
        Some(target.to_string_lossy().into_owned())
    }

    fn iconify(conn: &RustConnection, root: Window, win: Window, wm_change_state: Atom) {
        let msg = ClientMessageEvent::new(32, win, wm_change_state, [ICONIC_STATE, 0, 0, 0, 0]);
        let _ = conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            msg,
        );
    }

    /// Ask the window manager to de-iconify one window: the ICCCM
    /// `WM_CHANGE_STATE`/`NormalState` request plus the EWMH
    /// `_NET_ACTIVE_WINDOW` request.
    fn deiconify(
        conn: &RustConnection,
        root: Window,
        win: Window,
        wm_change_state: Atom,
        net_active_window: Option<Atom>,
    ) {
        let msg = ClientMessageEvent::new(32, win, wm_change_state, [NORMAL_STATE, 0, 0, 0, 0]);
        let _ = conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            msg,
        );
        let Some(active) = net_active_window else {
            return;
        };
        let active_msg = ClientMessageEvent::new(
            32,
            win,
            active,
            [NET_ACTIVE_WINDOW_SOURCE_APPLICATION, 0, 0, 0, 0],
        );
        let _ = conn.send_event(
            false,
            root,
            EventMask::SUBSTRUCTURE_NOTIFY | EventMask::SUBSTRUCTURE_REDIRECT,
            active_msg,
        );
    }

    /// Guard for one child window (audit nesting site #1): true only when
    /// its attributes exist, it is not override-redirect, and it is mapped
    /// viewable. Keeps the `walk` body flat.
    fn child_actionable(conn: &RustConnection, child: Window) -> bool {
        let Some(attrs) = conn
            .get_window_attributes(child)
            .ok()
            .and_then(|c| c.reply().ok())
        else {
            return false;
        };
        !attrs.override_redirect && attrs.map_state == MapState::VIEWABLE
    }

    /// Iconify one child when it is a non-own browser window (audit nesting
    /// site #1): all `walk` per-window decisions live here as guard
    /// clauses, so the loop body stays at depth 2.
    #[allow(clippy::too_many_arguments)]
    fn iconify_if_browser(
        conn: &RustConnection,
        root: Window,
        child: Window,
        pid_atom: Atom,
        wm_change_state: Atom,
        ctx_browsers: &[String],
        own_pid: u32,
        cache: &mut HashMap<u32, Vec<String>>,
        hidden: &mut Vec<Window>,
    ) {
        let Some(pid) = window_pid(conn, child, pid_atom) else {
            return;
        };
        if is_our_window(pid, own_pid) {
            return;
        }
        if window_title(conn, child).is_some_and(|title| is_our_title(&title)) {
            return;
        }
        let names = process_names(pid, cache);
        if any_candidate_matches(&names, ctx_browsers) {
            iconify(conn, root, child, wm_change_state);
            hidden.push(child);
        }
    }

    /// Visit one child window: iconify it when it is a browser window, then
    /// recurse into its children (audit nesting site #1). The recursion
    /// lives here, outside the `walk` loop, so no loop sits in a loop.
    #[allow(clippy::too_many_arguments)]
    fn visit_child(
        conn: &RustConnection,
        child: Window,
        root: Window,
        pid_atom: Atom,
        wm_change_state: Atom,
        ctx_browsers: &[String],
        own_pid: u32,
        cache: &mut HashMap<u32, Vec<String>>,
        hidden: &mut Vec<Window>,
    ) {
        if !child_actionable(conn, child) {
            return;
        }
        iconify_if_browser(
            conn,
            root,
            child,
            pid_atom,
            wm_change_state,
            ctx_browsers,
            own_pid,
            cache,
            hidden,
        );
        walk(
            conn,
            child,
            root,
            pid_atom,
            wm_change_state,
            ctx_browsers,
            own_pid,
            cache,
            hidden,
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(
        conn: &RustConnection,
        win: Window,
        root: Window,
        pid_atom: Atom,
        wm_change_state: Atom,
        ctx_browsers: &[String],
        own_pid: u32,
        cache: &mut HashMap<u32, Vec<String>>,
        hidden: &mut Vec<Window>,
    ) {
        let tree = match conn.query_tree(win).ok().and_then(|c| c.reply().ok()) {
            Some(t) => t,
            None => return,
        };
        for child in tree.children.iter().copied() {
            visit_child(
                conn,
                child,
                root,
                pid_atom,
                wm_change_state,
                ctx_browsers,
                own_pid,
                cache,
                hidden,
            );
        }
    }

    pub(super) fn minimize_and_record(
        browsers: &[String],
        own_pid: u32,
    ) -> Result<Hidden, MinimizeError> {
        let (conn, screen) =
            x11rb::connect(None).map_err(|e| MinimizeError::Platform(e.to_string()))?;
        let pid_atom = atom(&conn, b"_NET_WM_PID")
            .ok_or_else(|| MinimizeError::Platform("no _NET_WM_PID atom".into()))?;
        let wm_change_state = atom(&conn, b"WM_CHANGE_STATE")
            .ok_or_else(|| MinimizeError::Platform("no WM_CHANGE_STATE atom".into()))?;
        let root = conn.setup().roots[screen].root;
        let mut cache = HashMap::new();
        let mut hidden: Vec<Window> = Vec::new();
        walk(
            &conn,
            root,
            root,
            pid_atom,
            wm_change_state,
            browsers,
            own_pid,
            &mut cache,
            &mut hidden,
        );
        conn.flush()
            .map_err(|e| MinimizeError::Platform(e.to_string()))?;
        Ok(Hidden { windows: hidden })
    }

    pub(super) fn restore(hidden: &Hidden) {
        if hidden.windows.is_empty() {
            return;
        }
        let (conn, screen) = match x11rb::connect(None) {
            Ok(pair) => pair,
            Err(e) => {
                agnosis_core::trace::warn(&format!("minimize.restore: no X11 connection: {e}"));
                return;
            }
        };
        let Some(wm_change_state) = atom(&conn, b"WM_CHANGE_STATE") else {
            agnosis_core::trace::warn("minimize.restore: no WM_CHANGE_STATE atom");
            return;
        };
        let root = conn.setup().roots[screen].root;
        let net_active_window = atom(&conn, b"_NET_ACTIVE_WINDOW");
        for win in &hidden.windows {
            deiconify(&conn, root, *win, wm_change_state, net_active_window);
        }
        let _ = conn.flush();
    }
}

#[cfg(target_os = "macos")]
// The restore half of the backend is called by the daemon once it owns
// the record (audit 2026-10-20); until then it is unused.
#[allow(dead_code)]
mod imp {
    use super::{
        file_basename, is_default_browser_list, is_our_window, is_own_exe_name, macos_app_matches,
        MinimizeError,
    };
    use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication, NSWorkspace};

    /// Bundle identifiers minimized on macOS. Re-export of the module item.
    pub(super) use super::MACOS_BROWSER_BUNDLE_IDS;

    /// The applications this process hid, by process id.
    ///
    /// macOS hides whole *applications*, not windows, so the pid of the
    /// hidden application is the only handle a restore can use. A pid is
    /// also the honest identity here: an app that quit and came back has
    /// a new pid and new windows, so there is nothing to restore.
    #[derive(Clone, Debug, Default)]
    pub(super) struct Hidden {
        pids: Vec<u32>,
    }

    impl Hidden {
        pub(super) fn len(&self) -> usize {
            self.pids.len()
        }

        pub(super) fn is_empty(&self) -> bool {
            self.pids.is_empty()
        }
    }

    /// Process id of a running application.
    ///
    /// `-[NSRunningApplication processIdentifier]` sits behind the crate's
    /// `libc` feature in objc2-app-kit 0.3.2, and enabling that feature
    /// would add a `libc` dependency edge to `Cargo.lock`. The selector is
    /// stable, so it is sent directly instead; the return value is
    /// `pid_t`, which is an `i32` on every macOS target.
    fn app_pid(app: &NSRunningApplication) -> i32 {
        // SAFETY: `processIdentifier` is a getter on `NSRunningApplication`:
        // it takes no argument and returns the application's `pid_t`.
        let pid: i32 = unsafe { objc2::msg_send![app, processIdentifier] };
        pid
    }

    /// Identity of one running application, as the macOS backend sees it.
    struct AppInfo {
        /// Process id; `u32::MAX` for an application that reports no pid
        /// (`-1`), which never equals a real process id.
        pid: u32,
        bundle_id: String,
        localized: String,
        /// Executable base name, e.g. "Google Chrome" — the macOS
        /// analogue of the Windows/Linux process name.
        exe_name: String,
    }

    fn app_info(app: &NSRunningApplication) -> AppInfo {
        let bundle_id = app
            .bundleIdentifier()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let localized = app
            .localizedName()
            .map(|s| s.to_string())
            .unwrap_or_default();
        let exe_name = app
            .executableURL()
            .and_then(|url| url.path())
            .map(|path| path.to_string())
            .and_then(|path| file_basename(&path))
            .unwrap_or_default();
        AppInfo {
            pid: app_pid(app) as u32,
            bundle_id,
            localized,
            exe_name,
        }
    }

    /// Hide one application when it is a browser we do not own; returns its
    /// pid when the app was actually hidden.
    fn hide_if_browser(
        browsers: &[String],
        app: &NSRunningApplication,
        own_pid: u32,
    ) -> Option<u32> {
        let info = app_info(app);
        if is_our_window(info.pid, own_pid) {
            return None;
        }
        // Our own app (daemon, parade popup, editor, menu bar) shares the
        // executable name: hiding it would hide the parade itself.
        if is_own_exe_name(&info.exe_name) {
            return None;
        }
        let is_browser = macos_app_matches(
            browsers,
            MACOS_BROWSER_BUNDLE_IDS,
            &info.exe_name,
            &info.localized,
            &info.bundle_id,
        );
        if !is_browser {
            return None;
        }
        app.hide().then_some(info.pid)
    }

    pub(super) fn minimize_and_record(
        browsers: &[String],
        own_pid: u32,
    ) -> Result<Hidden, MinimizeError> {
        // Custom names on macOS use fuzzy matching (localized name /
        // bundle id), unlike the exact process-name match on
        // Windows/Linux. Warn only when the user actually configured
        // names: the built-in defaults are matched exactly, so the
        // warning must not fire for them.
        if !is_default_browser_list(browsers) {
            agnosis_core::trace::warn(
                "minimize.macos_custom_names: matching by localized name / bundle id",
            );
        }
        let mut pids: Vec<u32> = Vec::new();
        let workspace = NSWorkspace::sharedWorkspace();
        let apps = NSWorkspace::runningApplications(&workspace);
        for i in 0..apps.count() {
            let app = apps.objectAtIndex(i);
            if let Some(pid) = hide_if_browser(browsers, &app, own_pid) {
                pids.push(pid);
            }
        }
        Ok(Hidden { pids })
    }

    /// Unhide one application that we hid, if it is still hidden.
    ///
    /// `-[NSRunningApplication unhide]` is the exact inverse of the
    /// `hide` the minimize pass sent. It is an *application* level
    /// operation — macOS has no per-window "unhide" — so the whole app
    /// comes back in the state AppKit kept it, and the app is not
    /// re-activated: it returns behind the other windows.
    fn unhide_if_hidden(app: &NSRunningApplication, pid: u32, pids: &[u32]) {
        if !pids.contains(&pid) {
            return;
        }
        if !app.isHidden() {
            return;
        }
        if app.unhide() {
            return;
        }
        // `unhide` refuses for an app that cannot be unhidden (or that
        // quit in the meantime). Activating it is the last honest
        // attempt: it brings the app forward, which is what the user sees
        // as "the browser is back".
        let _ = app.activateWithOptions(NSApplicationActivationOptions::ActivateAllWindows);
    }

    pub(super) fn restore(hidden: &Hidden) {
        if hidden.pids.is_empty() {
            return;
        }
        let workspace = NSWorkspace::sharedWorkspace();
        let apps = NSWorkspace::runningApplications(&workspace);
        for i in 0..apps.count() {
            let app = apps.objectAtIndex(i);
            unhide_if_hidden(&app, app_pid(&app) as u32, &hidden.pids);
        }
    }
}

#[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
// The restore half of the backend is called by the daemon once it owns
// the record (audit 2026-10-20); until then it is unused.
#[allow(dead_code)]
mod imp {
    use super::MinimizeError;

    /// Nothing is ever hidden on an unsupported platform.
    #[derive(Clone, Debug, Default)]
    pub(super) struct Hidden;

    impl Hidden {
        pub(super) fn len(&self) -> usize {
            0
        }

        pub(super) fn is_empty(&self) -> bool {
            true
        }
    }

    pub(super) fn minimize_and_record(
        _browsers: &[String],
        _own_pid: u32,
    ) -> Result<Hidden, MinimizeError> {
        Ok(Hidden)
    }

    pub(super) fn restore(_hidden: &Hidden) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    fn defaults() -> Vec<String> {
        DEFAULT_BROWSERS.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn matches_case_insensitively_and_strips_exe() {
        let browsers = list(&["chrome"]);
        assert!(process_name_matches("chrome", &browsers));
        assert!(process_name_matches("Chrome", &browsers));
        assert!(process_name_matches("CHROME.EXE", &browsers));
        assert!(process_name_matches("chrome.exe", &browsers));
    }

    #[test]
    fn rejects_substrings() {
        let browsers = list(&["chrome"]);
        assert!(!process_name_matches("mychrome", &browsers));
        assert!(!process_name_matches("chrome2", &browsers));
        assert!(!process_name_matches("", &browsers));
    }

    #[test]
    fn empty_browser_list_uses_builtin_defaults() {
        assert!(process_name_matches("chrome", &[]));
        assert!(process_name_matches("firefox.EXE", &[]));
        assert!(!process_name_matches("notepad", &[]));
    }

    #[test]
    fn own_pid_skip_logic() {
        assert!(is_own_window(1234, 1234));
        assert!(!is_own_window(1234, 4321));
    }

    /// The current process is always "ours", whatever pid the caller
    /// named: the popup and the editor are separate processes, so the
    /// caller's pid is the only thing that can name them (audit
    /// 2026-10-20).
    #[test]
    fn current_process_is_always_ours() {
        let me = std::process::id();
        // The window belongs to the process that is enumerating.
        assert!(is_our_window(me, 1));
        // The window belongs to the process the caller named.
        assert!(is_our_window(4321, 4321));
        // Neither: a foreign window is not ours.
        assert!(!is_our_window(1, 2));
    }

    /// Our own executable is never a browser to hide: hiding it would
    /// hide the parade itself.
    #[test]
    fn own_exe_name_is_recognized() {
        assert!(is_own_exe_name("ducks"));
        assert!(is_own_exe_name("Ducks"));
        assert!(is_own_exe_name(" ducks "));
        assert!(!is_own_exe_name("chrome"));
    }

    /// An empty record is a valid record: restoring it must be a no-op
    /// and must not panic.
    #[test]
    fn empty_record_restores_without_panic() {
        let empty = Minimized::default();
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        restore(&empty);
    }

    /// A browser nobody has installed hides nothing, so the record stays
    /// empty and the restore stays a no-op.
    #[test]
    fn unknown_browser_records_nothing() {
        let recorded = minimize_and_record(&list(&["no-such-browser-xyz"]), std::process::id())
            .expect("an unknown browser must not fail the pass");
        assert!(recorded.is_empty());
        assert_eq!(recorded.len(), 0);
        restore(&recorded);
    }

    /// The count the daemon reports and the number of recorded windows
    /// are the same thing: the wrapper is a thin projection of the
    /// record.
    #[test]
    fn minimize_browsers_count_matches_the_record() {
        let browsers = list(&["no-such-browser-xyz"]);
        let counted = minimize_browsers(&browsers, std::process::id())
            .expect("an unknown browser must not fail the pass");
        let recorded = minimize_and_record(&browsers, std::process::id())
            .expect("an unknown browser must not fail the pass");
        assert_eq!(counted, recorded.len());
        assert_eq!(counted, 0);
    }

    #[test]
    fn normalize_trims_and_lowercases() {
        assert_eq!(normalize_process_name("  MsEdge.EXE "), "msedge");
        assert_eq!(normalize_process_name("FIREFOX"), "firefox");
    }

    #[test]
    fn basename_takes_last_segment() {
        assert_eq!(
            file_basename("/usr/bin/google-chrome"),
            Some("google-chrome".to_string())
        );
        assert_eq!(file_basename("firefox"), Some("firefox".to_string()));
        assert_eq!(file_basename("/"), None);
        assert_eq!(file_basename(""), None);
    }

    #[test]
    fn custom_name_hits_localized_containment_and_bundle_tail() {
        assert!(custom_browser_name_matches(
            "chrome",
            "Google Chrome",
            "com.google.Chrome"
        ));
        assert!(custom_browser_name_matches(
            "firefox",
            "Firefox",
            "org.mozilla.firefox"
        ));
        assert!(custom_browser_name_matches(
            "com.google.Chrome",
            "Google Chrome",
            "com.google.Chrome"
        ));
        assert!(!custom_browser_name_matches(
            "chrome",
            "Safari",
            "com.apple.Safari"
        ));
        assert!(!custom_browser_name_matches(
            "",
            "Chrome",
            "com.google.Chrome"
        ));
    }

    /// The Linux backend used to stop at the first name that existed, so a
    /// kernel-truncated `comm` hid the match (audit P1-8).
    #[test]
    fn truncated_comm_still_matches_via_cmdline() {
        let candidates = process_name_candidates(
            Some("com.google.Chrom"),
            Some("/usr/lib/firefox/firefox"),
            Some("/usr/lib/firefox/firefox"),
        );
        assert!(candidates.contains(&"com.google.Chrom".to_string()));
        assert!(candidates.contains(&"firefox".to_string()));
        assert!(any_candidate_matches(&candidates, &list(&["firefox"])));
    }

    /// The comm alone is still not enough: the match stays exact.
    #[test]
    fn truncated_comm_alone_does_not_match() {
        let candidates = process_name_candidates(Some("com.google.Chrom"), None, None);
        assert!(!any_candidate_matches(&candidates, &list(&["firefox"])));
        assert!(!any_candidate_matches(&candidates, &list(&["chrom"])));
    }

    /// A process with no readable name at all never matches.
    #[test]
    fn missing_candidates_never_match() {
        let candidates = process_name_candidates(None, None, None);
        assert!(candidates.is_empty());
        assert!(!any_candidate_matches(&candidates, &list(&["firefox"])));
    }

    #[test]
    fn default_list_is_recognized() {
        assert!(is_default_browser_list(&defaults()));
        assert!(is_default_browser_list(&[]));
        assert!(!is_default_browser_list(&list(&["chrome"])));
        assert!(!is_default_browser_list(&list(&["chrome", "mybrowser"])));
    }

    /// The built-in names are matched exactly on macOS, exactly like the
    /// process names on Windows/Linux: a short default such as "tor" must
    /// not match an unrelated app whose localized name merely contains
    /// "tor" (audit P1-7). The Tor Browser is therefore recognized by its
    /// bundle id, never by a name — see
    /// `macos_tor_browser_matches_by_bundle_id`.
    #[test]
    fn macos_builtin_names_match_exactly() {
        let browsers = defaults();
        let bundle_ids = ["com.apple.Safari"];
        assert!(macos_app_matches(
            &browsers,
            &bundle_ids,
            "Safari",
            "Safari",
            "com.apple.Safari"
        ));
        assert!(macos_app_matches(
            &browsers,
            &bundle_ids,
            "firefox",
            "Firefox",
            "org.mozilla.firefox"
        ));
        assert!(!macos_app_matches(
            &browsers,
            &bundle_ids,
            "Tor Browser",
            "Tor Browser",
            "org.torproject.tor-browser"
        ));
        assert!(!macos_app_matches(
            &browsers,
            &bundle_ids,
            "Vector Editor",
            "Vector Editor",
            "com.vector.Editor"
        ));
    }

    /// The Tor Browser is a built-in browser, so the macOS bundle-id list
    /// must name it: its executable is `Tor Browser`, which no built-in
    /// name matches exactly, so the bundle id is the only way to spot it.
    #[test]
    fn macos_tor_browser_matches_by_bundle_id() {
        let browsers = defaults();
        assert!(MACOS_BROWSER_BUNDLE_IDS.contains(&"org.torproject.tor-browser"));
        assert!(macos_app_matches(
            &browsers,
            MACOS_BROWSER_BUNDLE_IDS,
            "Tor Browser",
            "Tor Browser",
            "org.torproject.tor-browser",
        ));
    }

    /// The Tor Browser's executable is `torbrowser.exe` on Windows and
    /// `torbrowser` on Linux — never `tor` — so the built-in list carries
    /// the real executable name.
    #[test]
    fn tor_browser_executable_is_a_builtin_name() {
        let browsers = defaults();
        assert!(process_name_matches("torbrowser", &browsers));
        assert!(process_name_matches("torbrowser.exe", &browsers));
        assert!(!process_name_matches("Tor Browser", &browsers));
    }

    /// User-configured names keep the loose matching (a name the user typed
    /// may be a localized name or a bundle id), and a name that happens to
    /// equal a default is still treated as user-configured.
    #[test]
    fn macos_user_names_match_loosely() {
        let browsers = list(&["chrome"]);
        assert!(macos_app_matches(
            &browsers,
            &[],
            "Google Chrome",
            "Google Chrome",
            "com.google.Chrome"
        ));
        assert!(!macos_app_matches(
            &browsers,
            &[],
            "Safari",
            "Safari",
            "com.apple.Safari"
        ));
    }

    /// The recordable predicate (audit F-127): a taskbar-minimized
    /// (`iconic`) window is NOT ours to hide or to restore — the user put
    /// it away, and a restore must never drag it back out. The same goes
    /// for invisible, tool and cloaked windows; only a plain visible
    /// window is recordable.
    #[test]
    fn an_iconic_window_is_not_recordable() {
        assert!(recordable_browser_window(true, false, false, false));
        assert!(
            !recordable_browser_window(true, true, false, false),
            "a taskbar-minimized window is already hidden"
        );
        assert!(!recordable_browser_window(false, false, false, false));
        assert!(!recordable_browser_window(true, false, true, false));
        assert!(!recordable_browser_window(true, false, false, true));
    }

    /// The F-130 fallback reduces a full image path to its basename,
    /// either separator, and an empty result is a failure, not a name.
    #[cfg(target_os = "windows")]
    #[test]
    fn the_image_path_fallback_takes_the_basename() {
        use super::imp::windows_path_basename;

        assert_eq!(
            windows_path_basename(r"C:\Program Files\ducks\ducks.exe"),
            Some("ducks.exe".to_string())
        );
        assert_eq!(
            windows_path_basename("/usr/bin/ducks"),
            Some("ducks".to_string())
        );
        assert_eq!(
            windows_path_basename("chrome.exe"),
            Some("chrome.exe".to_string())
        );
        assert_eq!(windows_path_basename(r"C:\dir\"), None);
        assert_eq!(windows_path_basename(""), None);
    }
}
