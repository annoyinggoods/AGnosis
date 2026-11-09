//! Platform backends.
//!
//! Contract module: it only declares submodules and holds cross-platform
//! constants shared by them. Submodule contents are owned by individual agents.

/// Window class the popup registers on Windows. The daemon finds it to close a
/// running popup with `PostMessage(hwnd, WM_CLOSE, 0, 0)`.
#[cfg_attr(not(windows), allow(dead_code))]
pub const POPUP_WINDOW_CLASS: &str = "AGnosisPopup";

/// Window class the draw-your-own editor registers on Windows.
///
/// The class name is what keeps the two windows apart: the parade hotkey
/// closes a running popup by looking for [`POPUP_WINDOW_CLASS`], and the
/// editor chord closes a running editor by looking for this class. Two
/// different names mean neither lookup can ever find the other window, so
/// opening the editor never closes the parade and toggling the parade never
/// closes the editor (roadmap Stage 4).
#[cfg_attr(not(windows), allow(dead_code))]
pub const EDITOR_WINDOW_CLASS: &str = "AGnosisEditor";

/// Window class the HELP window registers on Windows.
///
/// The third window class (roadmap HELP_WINDOW §1), beside the popup and
/// the editor: the parade hotkey closes only `POPUP_WINDOW_CLASS` and the
/// editor chord only `EDITOR_WINDOW_CLASS`, so opening HELP can never be
/// mistaken for either toggle — and the minimizer's own-window check
/// ([`crate::platform::minimize`]) can skip the HELP window by class.
#[cfg_attr(not(windows), allow(dead_code))]
pub const HELP_WINDOW_CLASS: &str = "AGnosisHelp";

/// The OS-level title of the parade popup window.
///
/// The title is the one window identity that exists on **every**
/// platform: the class is a Win32 concept, so the X11 minimizer matches
/// our own windows by title (audit 2026-10-25 — the check used to
/// compare against the class-name constants, which no title ever
/// carried). The titles live here so the windows that set them and the
/// minimizer that matches them cannot drift apart.
pub const POPUP_WINDOW_TITLE: &str = "AGnosis - parade";

/// The OS-level title of the draw-your-own editor window.
///
/// See [`POPUP_WINDOW_TITLE`] for why the titles are constants.
pub const EDITOR_WINDOW_TITLE: &str = "AGnosis - editor";

/// The OS-level title of the HELP window.
///
/// See [`POPUP_WINDOW_TITLE`] for why the titles are constants.
pub const HELP_WINDOW_TITLE: &str = "AGnosis - help";

/// Process/base name used for the LaunchAgent label and the plist filename.
///
/// macOS only: the label is the `Label` key of the LaunchAgent plist and the
/// plist file name is the label plus `.plist`. Keeping the label in one
/// place stops the two from drifting.
#[cfg(target_os = "macos")]
pub const APP_BUNDLE_ID: &str = "com.agnosis.daemon";

/// Name of the desktop entry file on Linux.
///
/// Linux only: the `.desktop` file the session reads at login. The path
/// helper in [`crate::paths`] owns where the file lives; this constant owns
/// its name.
#[cfg(all(unix, not(target_os = "macos")))]
pub const AUTOSTART_DESKTOP_NAME: &str = "agnosis.desktop";

pub mod autostart;
pub mod chord;
pub mod clipboard;
pub mod daemon_ctl;
pub mod hotkey;
pub mod menubar;
pub mod minimize;
pub mod pidfile;
pub mod window;
