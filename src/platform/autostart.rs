//! Register / remove the login entry. Idempotent.
//!
//! | Platform | Mechanism |
//! | --- | --- |
//! | Windows | `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value
//! `ducks` → `"<exe>" --daemon`. Per-user key, no elevation required.
//! Uses the `windows` Registry APIs from the pinned crate. |
//! | macOS | `~/Library/LaunchAgents/com.agnosis.daemon.plist` with
//! `RunAtLoad`, pointing at the binary (no bundled `.app` needed). The
//! plist launches **`agnosis menubar`**, not the daemon: the menu bar is
//! the permission-free trigger, so the icon appears at login and the
//! parade and the editor stay reachable even when the daemon cannot
//! get the *Input Monitoring* grant (roadmap Stage 3B, decision D21). |
//! | Linux | `~/.config/autostart/agnosis.desktop` with `Terminal=false`,
//! `X-GNOME-Autostart-enabled=true`, `Type=Application`. |
//!
//! [`uninstall`] removes the entry, stops the daemon via
//! [`crate::platform::daemon_ctl::request_stop_daemon`], and leaves the
//! balloon folder alone.

/// Failures from [`install`] and [`uninstall`].
#[derive(Debug, thiserror::Error)]
pub enum AutostartError {
    /// Filesystem failure with a message.
    #[error("io: {0}")]
    Io(String),
    /// Platform backend failure with a message.
    ///
    /// Windows-only: the registry backend is the only one that can fail
    /// for a reason other than the filesystem (the macOS and Linux
    /// backends only write a plist / desktop file). Gated so the variant
    /// is not dead in the other builds (audit dead-code sweep).
    #[cfg(target_os = "windows")]
    #[error("autostart: {0}")]
    Platform(String),
}

/// Register `agnosis --daemon` to run at login. Idempotent.
pub fn install() -> Result<(), AutostartError> {
    imp::install()
}

/// What the login entry launches, per platform.
///
/// Windows and Linux register the daemon (`agnosis --daemon`); macOS
/// registers the **menu bar** (`agnosis menubar`), because the menu bar is
/// the permission-free trigger — the icon appears at login and the parade
/// and the editor stay reachable even when the daemon cannot get the
/// *Input Monitoring* grant (roadmap Stage 3B, decision D21).
///
/// Single owner: the CLI's confirmation message reads this, so the message
/// can never claim a different target than the one that was registered.
pub fn autostart_target() -> &'static str {
    if cfg!(target_os = "macos") {
        "agnosis menubar"
    } else {
        "agnosis --daemon"
    }
}

/// Remove the entry and stop the daemon, leaving the balloon folder alone.
/// Idempotent.
pub fn uninstall() -> Result<(), AutostartError> {
    imp::uninstall()
}

/// Escape a string for XML text content (the plist, audit F-133).
///
/// An executable path can contain `&` (`C:\R&D\ducks.exe`) and the same
/// for `<`/`>` in an exotic folder name; an unescaped `&` is not valid
/// plist XML and `launchd` silently refuses to load the agent — the icon
/// then never appears at login and nothing says why. The label is escaped
/// with the same rule for consistency (it is a compile-time constant
/// today, so this is belt and braces).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
    out
}

/// The macOS LaunchAgent plist: the label plus the program arguments.
///
/// The plist launches **`agnosis menubar`**, not `agnosis --daemon`: the
/// menu bar is the permission-free trigger, so the icon appears at
/// login and the parade and the editor stay reachable even when the
/// daemon cannot get the *Input Monitoring* grant (roadmap Stage 3B,
/// decision D21).
///
/// The builder is a pure function of the executable and the label, so
/// the plist text is testable without a Mac. Both strings are XML-escaped
/// (see [`xml_escape`], audit F-133).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn macos_plist_text(exe: &std::path::Path, label: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
         \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>Label</key>\n\t<string>{}</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n\
         \t\t<string>{}</string>\n\t<string>menubar</string>\n\
         \t</array>\n\
         \t<key>RunAtLoad</key>\n\t<true/>\n\
         </dict>\n</plist>\n",
        xml_escape(label),
        xml_escape(&exe.display().to_string()),
    )
}

/// The Linux desktop entry: the daemon at login.
///
/// The `Exec` argument is **quoted** (audit F-133): the desktop-file
/// spec parses `Exec` with shell-like quoting, so a path with a space
/// (`/opt/My Ducks/ducks`) broke into two arguments and the session's
/// autostart silently ran nothing.
///
/// The builder is a pure function of the executable, so the text is
/// testable without a Linux desktop.
#[cfg_attr(not(all(unix, not(target_os = "macos"))), allow(dead_code))]
fn desktop_text(exe: &std::path::Path) -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=ducks\n\
         Exec=\"{}\" --daemon\n\
         Terminal=false\n\
         X-GNOME-Autostart-enabled=true\n",
        exe.display(),
    )
}

#[cfg(target_os = "windows")]
mod imp {
    use super::AutostartError;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegOpenKeyW, RegQueryValueExW,
        RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE,
        REG_OPTION_NON_VOLATILE, REG_SZ,
    };

    /// Registry subkey holding the per-user logon entries.
    ///
    /// Single owner: the Run-key location lives here and nowhere else — the
    /// old `paths::autostart_registry_path` helper had no callers and was
    /// removed, so this constant is the one place that names the key
    /// (audit D3, "single owner").
    const RUN_KEY_SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

    /// Name of the value written under [`RUN_KEY_SUBKEY`]: the `ducks`
    /// entry that launches the daemon with `--daemon`.
    const RUN_KEY_VALUE: &str = "AGnosis";

    /// Win32 error code for "the key or value does not exist"
    /// (`ERROR_FILE_NOT_FOUND`): uninstall treats it as success.
    const ERROR_NOT_FOUND: u32 = 2;

    fn command_line() -> Result<String, AutostartError> {
        let exe = crate::paths::current_exe().map_err(|e| AutostartError::Io(e.to_string()))?;
        Ok(format!("\"{}\" --daemon", exe.display()))
    }

    fn run_key_name() -> Vec<u16> {
        RUN_KEY_VALUE
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    /// The wide, NUL-terminated text of one registry string.
    fn wide_bytes(text: &str) -> Vec<u8> {
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        // SAFETY: the slice reinterprets the `wide` buffer as raw bytes
        // for exactly the `wide.len() * 2` bytes it owns, is never
        // misaligned (u8 needs no alignment u16 does not provide), and
        // `to_vec()` copies it while `wide` is still alive, so no
        // reference escapes this statement.
        unsafe { std::slice::from_raw_parts(wide.as_ptr().cast::<u8>(), wide.len() * 2) }.to_vec()
    }

    /// Open (and create when missing) the Run key for writing.
    fn open_key() -> Result<HKEY, AutostartError> {
        let subkey: Vec<u16> = RUN_KEY_SUBKEY
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut key = HKEY::default();
        // SAFETY: `RegCreateKeyExW` writes the opened handle into `key`;
        // the strings are NUL-terminated and the handle is closed by the
        // caller on every path.
        let err = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                windows::core::PCWSTR(subkey.as_ptr()),
                Some(0),
                windows::core::PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_SET_VALUE | KEY_QUERY_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if err.0 != 0 {
            return Err(AutostartError::Platform(format!(
                "could not open Run key: {}",
                err.0
            )));
        }
        Ok(key)
    }

    /// Open the Run key **without creating it**.
    ///
    /// `uninstall` used the install path's `RegCreateKeyExW`, so removing
    /// the entry **created** the Run key first and left an empty key
    /// behind after every uninstall (audit F-133). `None` means the key
    /// does not exist — nothing to remove, which is a successful
    /// uninstall.
    fn open_existing_key() -> Result<Option<HKEY>, AutostartError> {
        let subkey: Vec<u16> = RUN_KEY_SUBKEY
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        let mut key = HKEY::default();
        // SAFETY: `RegOpenKeyW` writes the opened handle into `key` and
        // never creates anything; the string is NUL-terminated.
        let err = unsafe {
            RegOpenKeyW(
                HKEY_CURRENT_USER,
                windows::core::PCWSTR(subkey.as_ptr()),
                &mut key,
            )
        };
        if err.0 == ERROR_NOT_FOUND {
            return Ok(None);
        }
        if err.0 != 0 {
            return Err(AutostartError::Platform(format!(
                "could not open Run key: {}",
                err.0
            )));
        }
        Ok(Some(key))
    }

    /// The Run value's current text, `None` when the key or the value
    /// does not exist or cannot be read.
    fn current_value(key: HKEY) -> Option<String> {
        let name = run_key_name();
        let mut len = 0u32;
        // SAFETY: the first `RegQueryValueExW` asks for the size only
        // (`lpdata` is null); the strings are NUL-terminated.
        let size_err = unsafe {
            RegQueryValueExW(
                key,
                windows::core::PCWSTR(name.as_ptr()),
                None,
                None,
                None,
                Some(&mut len),
            )
        };
        if size_err.0 != 0 || len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: the second query writes at most `len` bytes into the
        // buffer, which is exactly `len` bytes long.
        let err = unsafe {
            RegQueryValueExW(
                key,
                windows::core::PCWSTR(name.as_ptr()),
                None,
                None,
                Some(buf.as_mut_ptr()),
                Some(&mut len),
            )
        };
        if err.0 != 0 {
            return None;
        }
        let words: Vec<u16> = buf[..len as usize]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let text = String::from_utf16_lossy(&words);
        Some(text.trim_end_matches('\0').to_string())
    }

    /// Write the Run value, comparing first (audit F-133): an unchanged
    /// value is not rewritten, mirroring the plist/desktop installs'
    /// compare-before-rewrite so repeated installs do not touch the
    /// registry at all.
    fn write_run_value(key: HKEY, cmd: &str) -> Result<(), AutostartError> {
        if current_value(key).as_deref() == Some(cmd) {
            return Ok(());
        }
        let bytes = wide_bytes(cmd);
        let name = run_key_name();
        // SAFETY: `RegSetValueExW` reads `bytes.len()` bytes from the
        // buffer, which is that long; the strings are NUL-terminated.
        let err = unsafe {
            RegSetValueExW(
                key,
                windows::core::PCWSTR(name.as_ptr()),
                Some(0),
                REG_SZ,
                Some(&bytes),
            )
        };
        if err.0 != 0 {
            return Err(AutostartError::Platform(format!(
                "could not write Run value: {}",
                err.0
            )));
        }
        Ok(())
    }

    pub(super) fn install() -> Result<(), AutostartError> {
        let key = open_key()?;
        let cmd = command_line()?;
        let written = write_run_value(key, &cmd);
        // SAFETY: the handle was opened by `open_key` and is closed on
        // every path.
        unsafe {
            let _ = RegCloseKey(key);
        }
        written
    }

    pub(super) fn uninstall() -> Result<(), AutostartError> {
        let Some(key) = open_existing_key()? else {
            // No Run key at all: nothing was ever installed, which is
            // exactly what uninstall wants to be true (audit F-133 —
            // the old path created the key here).
            crate::platform::daemon_ctl::request_stop_daemon();
            return Ok(());
        };
        let name = run_key_name();
        // SAFETY: `RegDeleteValueW` only reads its string arguments; the
        // handle was opened by `open_existing_key`.
        let err = unsafe { RegDeleteValueW(key, windows::core::PCWSTR(name.as_ptr())) };
        // SAFETY: see above.
        unsafe {
            let _ = RegCloseKey(key);
        }
        if err.0 != 0 && err.0 != ERROR_NOT_FOUND {
            return Err(AutostartError::Platform(format!(
                "could not delete Run value: {}",
                err.0
            )));
        }
        crate::platform::daemon_ctl::request_stop_daemon();
        Ok(())
    }
}

#[cfg(target_os = "macos")]
mod imp {
    use super::AutostartError;

    const LABEL: &str = crate::platform::APP_BUNDLE_ID;

    pub(super) fn install() -> Result<(), AutostartError> {
        let path =
            crate::paths::autostart_plist_path().map_err(|e| AutostartError::Io(e.to_string()))?;
        let exe = crate::paths::current_exe().map_err(|e| AutostartError::Io(e.to_string()))?;
        let text = super::macos_plist_text(&exe, LABEL);
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        if old == text {
            return Ok(());
        }
        std::fs::write(&path, text).map_err(|e| AutostartError::Io(e.to_string()))
    }

    pub(super) fn uninstall() -> Result<(), AutostartError> {
        let path =
            crate::paths::autostart_plist_path().map_err(|e| AutostartError::Io(e.to_string()))?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(AutostartError::Io(e.to_string())),
        }
        crate::platform::daemon_ctl::request_stop_daemon();
        Ok(())
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod imp {
    use super::AutostartError;

    pub(super) fn install() -> Result<(), AutostartError> {
        let path = crate::paths::autostart_desktop_path()
            .map_err(|e| AutostartError::Io(e.to_string()))?;
        let exe = crate::paths::current_exe().map_err(|e| AutostartError::Io(e.to_string()))?;
        let text = super::desktop_text(&exe);
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        if old == text {
            return Ok(());
        }
        std::fs::write(&path, text).map_err(|e| AutostartError::Io(e.to_string()))
    }

    pub(super) fn uninstall() -> Result<(), AutostartError> {
        let path = crate::paths::autostart_desktop_path()
            .map_err(|e| AutostartError::Io(e.to_string()))?;
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(AutostartError::Io(e.to_string())),
        }
        crate::platform::daemon_ctl::request_stop_daemon();
        Ok(())
    }
}

#[cfg(not(any(target_os = "windows", target_os = "macos", unix)))]
mod imp {
    use super::AutostartError;

    pub(super) fn install() -> Result<(), AutostartError> {
        Err(AutostartError::Io("unsupported platform".into()))
    }

    pub(super) fn uninstall() -> Result<(), AutostartError> {
        Err(AutostartError::Io("unsupported platform".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::{desktop_text, macos_plist_text};
    use std::path::Path;

    /// The macOS LaunchAgent must start the **menu bar**, not the
    /// daemon: the menu bar needs no permission, so the icon appears at
    /// login and the parade stays reachable (roadmap Stage 3B).
    #[test]
    fn the_macos_launch_agent_starts_the_menubar() {
        let text = macos_plist_text(Path::new("/usr/local/bin/ducks"), "com.agnosis.daemon");
        assert!(
            text.contains("<string>menubar</string>"),
            "the plist must launch the menu bar: {text}"
        );
        assert!(
            !text.contains("--daemon"),
            "the plist must not launch the daemon: {text}"
        );
        assert!(
            text.contains("<string>/usr/local/bin/ducks</string>"),
            "the plist must point at the binary: {text}"
        );
        assert!(
            text.contains("<string>com.agnosis.daemon</string>"),
            "the plist must keep the bundle id as its label: {text}"
        );
        assert!(
            text.contains("<key>RunAtLoad</key>"),
            "the plist must run at load: {text}"
        );
    }

    /// An `&` in the executable path must be XML-escaped (audit F-133):
    /// an unescaped ampersand is not valid plist XML, `launchd` refuses
    /// to load the agent, and the icon silently never appears at login.
    #[test]
    fn the_plist_escapes_an_ampersand_in_the_path() {
        let text = macos_plist_text(Path::new("/opt/R&D/ducks"), "com.agnosis.daemon");
        assert!(
            text.contains("<string>/opt/R&amp;D/ducks</string>"),
            "the ampersand must be escaped: {text}"
        );
        assert!(
            !text.contains("R&D"),
            "no raw ampersand may survive into the XML: {text}"
        );
        // The other two XML-special characters escape too.
        let text = macos_plist_text(Path::new("/opt/a<b>c/ducks"), "com.agnosis.daemon");
        assert!(text.contains("&lt;b&gt;"));
    }

    /// The Linux desktop entry quotes the `Exec` argument (audit F-133):
    /// a path with a space is one argument, not two.
    #[test]
    fn the_desktop_entry_quotes_the_exec_path() {
        let text = desktop_text(Path::new("/opt/My Ducks/ducks"));
        assert!(
            text.contains("Exec=\"/opt/My Ducks/ducks\" --daemon"),
            "the Exec path must be quoted: {text}"
        );
        // The unquoted regression: `Exec=/opt/My Ducks/ducks --daemon`.
        let plain = desktop_text(Path::new("/usr/bin/ducks"));
        assert!(plain.contains("Exec=\"/usr/bin/ducks\" --daemon"));
    }
}
