//! macOS menu-bar trigger: the permission-free way to open the parade
//! and the editor (roadmap Stage 3B, decision D21).
//!
//! The menu bar is a `NSStatusItem` with four entries — **Show Parade**,
//! **Draw Your Own**, **Stop Parade** and **Quit**. It needs **no**
//! *Input Monitoring* grant, so it keeps working when the daemon
//! refuses to start because the grant is missing. That is the whole
//! point of it: the hotkey backend is a convenience, the menu bar is
//! the guarantee.
//!
//! It runs as its own process (`agnosis menubar`) because `NSApplication`
//! has to live on the **main thread**. The app never owns the parade:
//! every parade and editor entry goes through the daemon's action
//! handler ([`crate::daemon::handle_action`]), which owns the toggles
//! and the browser minimization and spawns the child processes. The
//! parade itself stays a separate window process, so its frame rate is
//! untouched by the menu bar.
//!
//! **"Show Parade" is the hotkey, not a spawn.** The entry hands the
//! parade action to the daemon's action handler
//! ([`crate::daemon::handle_action`]), which owns the close-or-spawn
//! **toggle** *and* the browser minimization. So clicking the entry
//! twice closes the parade instead of opening a second window, and the
//! parade still hides the browsers — the product's headline behaviour.
//! The menu-bar process still only spawns and stops child processes; it
//! never owns a parade window.
//!
//! **No menu action blocks the AppKit main thread** (audit F-129): the
//! handler's waits — the 1.5 s spawn wait, the 0.5 s close wait, the
//! recursive minimize scans — run on a short-lived worker thread, and
//! the main thread keeps only the wait-free `NSScreen` probe (AppKit
//! requires the main thread for it), so the status item never freezes
//! and the process never beachballs.
//!
//! Every other platform has no menu bar: [`run`] fails with a clear
//! message there instead of pretending.
//!
//! The four entries and their titles live in [`MENU_ENTRIES`], a pure
//! table, so the menu's contents are testable without a Mac.

use crate::daemon::DaemonError;
use crate::platform::chord::HotkeyAction;
use agnosis_core::config::Config;

/// Failures from [`run`].
#[derive(Debug, thiserror::Error)]
pub enum MenuBarError {
    /// The menu bar exists only on macOS.
    #[cfg_attr(target_os = "macos", allow(dead_code))]
    #[error("the menu bar is macOS only")]
    Unsupported,
    /// AppKit refused to build the menu.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    #[error("menu bar: {0}")]
    Platform(String),
}

/// What one menu-bar entry does.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuAction {
    /// Open the parade popup.
    ShowParade,
    /// Open the draw-your-own editor.
    DrawYourOwn,
    /// Stop the parade and the daemon that owns the hotkey.
    StopParade,
    /// Quit the menu-bar app.
    Quit,
}

/// The menu-bar entries, in the order they appear.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const MENU_ENTRIES: [(MenuAction, &str); 4] = [
    (MenuAction::ShowParade, "Show Parade"),
    (MenuAction::DrawYourOwn, "Draw Your Own"),
    (MenuAction::StopParade, "Stop Parade"),
    (MenuAction::Quit, "Quit"),
];

/// What the "Show Parade" entry did.
// The menu bar exists on macOS only, so off macOS the entry and its
// outcome are unreachable — the allow says exactly that.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParadeOutcome {
    /// A running parade was closed — the entry is a toggle.
    Closed,
    /// The parade was spawned and the browsers were minimized.
    Spawned,
    /// The action failed; the reason was written to the trace file.
    Failed,
}

/// Run the parade action through the daemon's action handler.
///
/// This is the shared, platform-independent part of the "Show Parade"
/// entry: the macOS menu bar passes the daemon's action handler, and the
/// handler owns the close-or-spawn toggle *and* the browser minimization
/// — so the entry behaves exactly like the parade hotkey instead of
/// opening a second parade window.
///
/// The handler is **injected** so this path is testable without a
/// desktop: the test passes a recorder and asserts on the action it
/// received.
///
/// A failure is traced, never panicked on: the menu-bar process must
/// survive a broken config or a failed spawn.
// The menu bar exists on macOS only, so off macOS this helper is only
// reachable from its unit test — the allow says exactly that.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn show_parade_with(
    cfg: &Config,
    run: impl FnOnce(&Config, HotkeyAction) -> Result<bool, DaemonError>,
) -> ParadeOutcome {
    match run(cfg, HotkeyAction::Popup) {
        Ok(true) => ParadeOutcome::Closed,
        Ok(false) => ParadeOutcome::Spawned,
        Err(e) => {
            agnosis_core::trace::warn(&format!("menubar.parade_failed: {e}"));
            ParadeOutcome::Failed
        }
    }
}

/// Run the menu-bar app until the user quits it.
///
/// The call blocks: the app owns the main thread and its run loop.
pub fn run() -> Result<(), MenuBarError> {
    imp::run()
}

#[cfg(target_os = "macos")]
mod imp {
    use super::{MenuAction, MenuBarError, ParadeOutcome, MENU_ENTRIES};
    use crate::daemon::DaemonError;
    use agnosis_core::config::Config;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, Sel};
    use objc2::{class, define_class, msg_send, sel, AnyThread, MainThreadMarker, MainThreadOnly};
    use objc2_app_kit::{
        NSApplication, NSApplicationActivationPolicy, NSMenu, NSMenuItem, NSStatusBar, NSStatusItem,
    };
    use std::sync::OnceLock;

    /// The duck shown in the menu bar.
    const ICON: &str = "🦆";

    /// The title of the status item's menu.
    const MENU_TITLE: &str = "AGnosis";

    /// The length of a status item that sizes itself to its title.
    ///
    /// `NSVariableStatusItemLength` from AppKit. The constant is not
    /// reachable without the `objc2-core-foundation` feature of
    /// `objc2-app-kit`, so the value is spelled out here.
    const VARIABLE_STATUS_ITEM_LENGTH: f64 = -1.0;

    /// The configuration the menu-bar app was started with.
    ///
    /// It is loaded **once**, when the app starts: the menu bar lives for
    /// as long as the user keeps it, and re-reading `config.toml` on every
    /// click would let a half-written file break the menu. A failed load
    /// stops the app before the menu is built (see [`run`]).
    static CONFIG: OnceLock<Config> = OnceLock::new();

    /// The configuration the menu bar runs with.
    ///
    /// `None` only before [`run`] loaded it.
    fn config() -> Option<&'static Config> {
        CONFIG.get()
    }

    /// The selector of one menu action.
    ///
    /// Every entry has its own selector, so the target class needs no
    /// state and no dispatch table: AppKit sends the selector and the
    /// matching method does the work.
    fn selector_for(action: MenuAction) -> Sel {
        match action {
            MenuAction::ShowParade => sel!(showParade:),
            MenuAction::DrawYourOwn => sel!(drawYourOwn:),
            MenuAction::StopParade => sel!(stopParade:),
            MenuAction::Quit => sel!(quitDucks:),
        }
    }

    define_class!(
        /// The target of the four menu items.
        ///
        /// The methods are the menu actions; each one is a thin shell
        /// around the free function that does the work, so the
        /// Objective-C part stays empty and the behaviour is testable
        /// Rust.
        #[unsafe(super(objc2::runtime::NSObject))]
        struct MenuTarget;

        impl MenuTarget {
            #[unsafe(method(showParade:))]
            fn show_parade(&self, _sender: &AnyObject) {
                show_parade();
            }

            #[unsafe(method(drawYourOwn:))]
            fn draw_your_own(&self, _sender: &AnyObject) {
                draw_your_own();
            }

            #[unsafe(method(stopParade:))]
            fn stop_parade(&self, _sender: &AnyObject) {
                stop_parade();
            }

            #[unsafe(method(quitDucks:))]
            fn quit_ducks(&self, _sender: &AnyObject) {
                quit_ducks();
            }
        }
    );

    /// An `NSString` with the given text.
    ///
    /// `NSString` lives in `objc2-foundation`, which this crate does not
    /// depend on directly, so the string is built through the
    /// Objective-C runtime instead.
    fn ns_string(text: &str) -> Option<Retained<AnyObject>> {
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        // SAFETY: `stringWithUTF8String:` copies the NUL-terminated
        // UTF-8 bytes into a new string; the buffer outlives the call.
        // The result is autoreleased, so the message send retains it.
        unsafe {
            msg_send![
                class!(NSString),
                stringWithUTF8String: bytes.as_ptr().cast::<core::ffi::c_char>()
            ]
        }
    }

    /// One menu item, wired to its action.
    fn menu_item(
        mtm: MainThreadMarker,
        target: &Retained<AnyObject>,
        action: MenuAction,
        title: &str,
    ) -> Result<Retained<NSMenuItem>, MenuBarError> {
        let Some(title) = ns_string(title) else {
            return Err(MenuBarError::Platform("cannot build the menu title".into()));
        };
        let Some(empty) = ns_string("") else {
            return Err(MenuBarError::Platform("cannot build the menu title".into()));
        };
        // SAFETY: `initWithTitle:action:keyEquivalent:` is the
        // designated initializer of `NSMenuItem`; the strings and the
        // selector are valid for the duration of the call.
        let item: Retained<NSMenuItem> = unsafe {
            msg_send![
                NSMenuItem::alloc(mtm),
                initWithTitle: &*title,
                action: Some(selector_for(action)),
                keyEquivalent: &*empty
            ]
        };
        // SAFETY: the target outlives the menu, which the status item
        // keeps alive for as long as the app runs.
        unsafe { item.setTarget(Some(target)) };
        Ok(item)
    }

    /// The menu with the four entries.
    fn build_menu(
        mtm: MainThreadMarker,
        target: &Retained<AnyObject>,
    ) -> Result<Retained<NSMenu>, MenuBarError> {
        let Some(title) = ns_string(MENU_TITLE) else {
            return Err(MenuBarError::Platform("cannot build the menu title".into()));
        };
        // SAFETY: `initWithTitle:` is the designated initializer of
        // `NSMenu`.
        let menu: Retained<NSMenu> =
            unsafe { msg_send![NSMenu::alloc(mtm), initWithTitle: &*title] };
        for (action, title) in MENU_ENTRIES {
            let item = menu_item(mtm, target, action, title)?;
            menu.addItem(&item);
        }
        Ok(menu)
    }

    /// Run one menu action's body on a short-lived worker thread
    /// (audit F-129).
    ///
    /// The action's waits — the 1.5 s spawn wait, the 0.5 s close wait,
    /// the recursive minimize scans — used to run on the AppKit main
    /// thread, so one *Show Parade* click froze the run loop (and every
    /// status item on the bar) for as long as the daemon's handler
    /// waited. The worker owns them now; the main thread keeps only the
    /// wait-free `NSScreen` probe (see [`parade_target_on_main_thread`]).
    fn run_on_a_worker(job: impl FnOnce() + Send + 'static) {
        let spawned = std::thread::Builder::new()
            .name("agnosis-menubar-action".into())
            .spawn(job);
        if let Err(e) = spawned {
            agnosis_core::trace::warn(&format!("menubar.worker_failed: {e}"));
        }
    }

    /// The target monitor, resolved **on the main thread** (audit F-129).
    ///
    /// The probe behind `resolve_popup_target` is `NSScreen`, which
    /// AppKit keeps main-thread-only — so the resolve happens here,
    /// before the worker starts, while the menu callback still owns the
    /// main thread. It is wait-free, so the run loop is not blocked.
    /// The `Result` travels into the worker: a failed probe is the
    /// action's error, decided where the action runs.
    fn parade_target_on_main_thread(cfg: &Config) -> Result<crate::popup::PopupArgs, String> {
        crate::cli::resolve_popup_target(cfg)
    }

    /// The menu bar's own [`crate::daemon::ActionSteps`]: the daemon's
    /// real side effects, with the monitor target **pre-resolved on the
    /// main thread** (audit F-129).
    ///
    /// Every other step delegates to [`crate::daemon::PlatformSteps`],
    /// so the spawn/close/minimize behaviour cannot drift from the
    /// hotkey's; only `resolve_target` differs, because the worker must
    /// not touch `NSScreen`.
    struct MenubarSteps<'a> {
        steps: &'a mut crate::daemon::PlatformSteps,
        /// `None` once consumed: the handler resolves the target **once**
        /// per action, so the value is taken, not cloned.
        target: Option<Result<crate::popup::PopupArgs, String>>,
    }

    impl crate::daemon::ActionSteps for MenubarSteps<'_> {
        fn close_popup(&mut self) -> bool {
            self.steps.close_popup()
        }

        fn spawn_popup(&mut self, args: &crate::popup::PopupArgs) -> Result<(), DaemonError> {
            self.steps.spawn_popup(args)
        }

        fn wait_for_popup_window(&mut self) {
            // Delegated on purpose: the default is a no-op, and losing
            // this wait would reopen the double-tap bug (audit
            // 2026-10-21 §G21). It runs on this worker thread — never
            // on a UI thread — which is exactly what the trait asks.
            self.steps.wait_for_popup_window();
        }

        fn minimize_browsers(&mut self, names: &[String], own_pid: u32) -> Result<usize, String> {
            self.steps.minimize_browsers(names, own_pid)
        }

        fn restore_browsers(&mut self) {
            self.steps.restore_browsers();
        }

        fn parade_open(&mut self) -> bool {
            self.steps.parade_open()
        }

        fn close_editor(&mut self) -> bool {
            self.steps.close_editor()
        }

        fn wait_for_editor_window(&mut self) {
            // The editor's mirror of `wait_for_popup_window` above (audit
            // 2026-10-22 §P1-6).
            self.steps.wait_for_editor_window();
        }

        fn resolve_target(&mut self, _cfg: &Config) -> Result<crate::popup::PopupArgs, String> {
            self.target.take().unwrap_or_else(|| {
                Err("the monitor target was already resolved for this action".to_string())
            })
        }

        fn spawn_editor(&mut self, args: &crate::editor::EditorArgs) -> Result<(), DaemonError> {
            self.steps.spawn_editor(args)
        }
    }

    /// Lock the parade steps, recovering from a **poisoned** mutex
    /// (audit F-135).
    ///
    /// A panic in one menu action used to poison the mutex, and every
    /// later entry then took the `Err` arm and returned silently — a
    /// dead menu with no trace. The guard is recovered with
    /// `into_inner` (the hidden-browsers record keeps its own
    /// invariants), and the poisoning is traced loudly so the dead menu
    /// is at least visible in `AGNOSIS_TRACE_FILE`.
    fn lock_parade_steps() -> Option<std::sync::MutexGuard<'static, crate::daemon::PlatformSteps>> {
        match parade_steps().lock() {
            Ok(guard) => Some(guard),
            Err(poisoned) => {
                agnosis_core::trace::warn("menubar.steps_poisoned: guard recovered");
                Some(poisoned.into_inner())
            }
        }
    }

    /// Open the parade: hand the action to the daemon's handler, on a
    /// worker.
    ///
    /// The handler owns the close-or-spawn **toggle** and the browser
    /// minimization, so clicking "Show Parade" twice closes the parade
    /// instead of opening a second window, and the parade still hides
    /// the browsers — the same behaviour as the parade hotkey. The
    /// waits inside run off the main thread (audit F-129).
    fn show_parade() {
        let Some(cfg) = config() else {
            return;
        };
        let cfg = cfg.clone();
        let target = parade_target_on_main_thread(&cfg);
        run_on_a_worker(move || run_parade_action(&cfg, target));
    }

    /// The parade action's body, off the main thread (audit F-129).
    fn run_parade_action(cfg: &Config, target: Result<crate::popup::PopupArgs, String>) {
        let outcome = {
            let Some(mut steps) = lock_parade_steps() else {
                return;
            };
            // The steps are kept, not thrown away: they hold the record of
            // the browsers this action hid, so *Stop Parade* can put them back
            // (audit 2026-10-20 §3.7).
            //
            // The guard is **dropped** before the match: the `Closed` arm
            // calls `restore_browsers`, which locks the same mutex. Holding
            // it across that call deadlocked the worker the same way it
            // once deadlocked the AppKit main thread
            // (audit 2026-10-22 §P1-5).
            let mut menubar_steps = MenubarSteps {
                steps: &mut steps,
                target: Some(target),
            };
            super::show_parade_with(cfg, |cfg, action| {
                crate::daemon::handle_action_with(cfg, action, &mut menubar_steps)
            })
        };
        match outcome {
            ParadeOutcome::Closed => {
                agnosis_core::trace::trace("menubar.popup_closed");
                // The toggle **shut** the parade, so the browsers the
                // last *Show Parade* hid go back — exactly as they do
                // when the hotkey closes the parade. The
                // `restore_on_close` flag is honoured on this path too
                // (audit 2026-10-21 §G20).
                restore_browsers();
            }
            ParadeOutcome::Spawned => agnosis_core::trace::trace("menubar.popup_spawned"),
            // The failure was already traced by the shared helper.
            ParadeOutcome::Failed => {}
        }
    }

    /// Open the draw-your-own editor — or close it when it is already
    /// open: the entry **toggles**, like the editor chord.
    ///
    /// The entry spawned unconditionally, so clicking it twice opened a
    /// second editor over the first (audit 2026-10-22 §P1-6). The
    /// daemon's `handle_action_with` owns the same toggle for the chord;
    /// the menu goes through it with the **editor** action, so the two
    /// cannot drift apart. Its waits run off the main thread (audit
    /// F-129).
    fn draw_your_own() {
        let Some(cfg) = config() else {
            return;
        };
        let cfg = cfg.clone();
        let target = parade_target_on_main_thread(&cfg);
        run_on_a_worker(move || run_editor_action(&cfg, target));
    }

    /// The editor action's body, off the main thread (audit F-129).
    fn run_editor_action(cfg: &Config, target: Result<crate::popup::PopupArgs, String>) {
        let Some(mut steps) = lock_parade_steps() else {
            return;
        };
        let mut menubar_steps = MenubarSteps {
            steps: &mut steps,
            target: Some(target),
        };
        let outcome = crate::daemon::handle_action_with(
            cfg,
            crate::platform::chord::HotkeyAction::Editor,
            &mut menubar_steps,
        );
        match outcome {
            Ok(true) => agnosis_core::trace::trace("menubar.editor_closed"),
            Ok(false) => agnosis_core::trace::trace("menubar.editor_spawned"),
            Err(e) => agnosis_core::trace::warn(&format!("menubar.editor_failed: {e}")),
        }
    }

    /// Stop the parade: close a running popup and put the browsers back
    /// — the latter only if the user asked for it.
    ///
    /// The menu entry says *Stop Parade*, so it stops the parade — not
    /// the daemon that owns the global hotkey. Killing the daemon here
    /// took the hotkey down with it, which the title never promised
    /// (audit 2026-10-20 §3.18). `agnosis stop` still stops the daemon.
    /// The close's wait runs off the main thread (audit F-129).
    fn stop_parade() {
        run_on_a_worker(|| {
            if crate::platform::window::close_popup() {
                agnosis_core::trace::trace("menubar.popup_closed");
            }
            restore_browsers();
        });
    }

    /// Put back the browsers the last *Show Parade* hid.
    ///
    /// **Off by default** (`[browsers] restore_on_close`): the joke is
    /// that the browser is *still hidden* when the parade ends, so the
    /// menu bar's *Stop Parade* leaves it hidden too. Nothing is closed,
    /// so no work is lost either way.
    fn restore_browsers() {
        if !config().is_some_and(|cfg| cfg.browsers.restore_on_close) {
            return;
        }
        let Some(mut steps) = lock_parade_steps() else {
            return;
        };
        steps.restore_hidden_browsers();
    }

    /// The parade state the menu bar keeps between the entries: what the
    /// last *Show Parade* hid, so *Stop Parade* can put it back.
    fn parade_steps() -> &'static std::sync::Mutex<crate::daemon::PlatformSteps> {
        static STEPS: std::sync::OnceLock<std::sync::Mutex<crate::daemon::PlatformSteps>> =
            std::sync::OnceLock::new();
        STEPS.get_or_init(|| std::sync::Mutex::new(crate::daemon::PlatformSteps::new()))
    }

    /// Quit the menu-bar app.
    fn quit_ducks() {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        NSApplication::sharedApplication(mtm).terminate(None);
    }

    /// Run the menu-bar app: build the status item and hand the main
    /// thread to AppKit.
    pub(super) fn run() -> Result<(), MenuBarError> {
        let mtm = MainThreadMarker::new()
            .ok_or_else(|| MenuBarError::Platform("not on the main thread".into()))?;
        // The config is loaded **once**, here: every entry works from the
        // same snapshot, and a broken config stops the app before a menu
        // that could not act exists.
        let cfg = match crate::host::load_config() {
            Ok(cfg) => cfg,
            Err(e) => {
                return Err(MenuBarError::Platform(format!(
                    "cannot load the config: {e}"
                )));
            }
        };
        // The menu bar runs one config per process; a second set cannot
        // happen, so the error is ignored on purpose.
        let _ = CONFIG.set(cfg);
        let app = NSApplication::sharedApplication(mtm);
        // An accessory app has no Dock icon and no menu bar of its own:
        // only the status item is visible.
        app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        // SAFETY: `alloc`+`init` is the documented way to create an
        // `NSObject`; the target is kept alive by the menu.
        let target: Retained<MenuTarget> = unsafe { msg_send![MenuTarget::alloc(), init] };
        let target: Retained<AnyObject> = Retained::from(target);
        let menu = build_menu(mtm, &target)?;
        let bar = NSStatusBar::systemStatusBar();
        // SAFETY: `statusItemWithLength:` returns an autoreleased status
        // item, so the message send retains it.
        let status: Retained<NSStatusItem> =
            unsafe { msg_send![&bar, statusItemWithLength: VARIABLE_STATUS_ITEM_LENGTH] };
        let Some(icon) = ns_string(ICON) else {
            return Err(MenuBarError::Platform("cannot build the menu title".into()));
        };
        // SAFETY: `setTitle:` copies the string into the status item.
        let _: () = unsafe { msg_send![&status, setTitle: &*icon] };
        status.setMenu(Some(&menu));
        app.run();
        Ok(())
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use super::MenuBarError;

    /// No menu bar outside macOS: the command says so instead of
    /// pretending.
    pub(super) fn run() -> Result<(), MenuBarError> {
        Err(MenuBarError::Unsupported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_menu_has_the_four_entries() {
        let titles: Vec<&str> = MENU_ENTRIES.iter().map(|(_, title)| *title).collect();
        assert_eq!(
            titles,
            vec!["Show Parade", "Draw Your Own", "Stop Parade", "Quit"]
        );
    }

    #[test]
    fn the_menu_entries_are_in_a_fixed_order() {
        let actions: Vec<MenuAction> = MENU_ENTRIES.iter().map(|(action, _)| *action).collect();
        assert_eq!(
            actions,
            vec![
                MenuAction::ShowParade,
                MenuAction::DrawYourOwn,
                MenuAction::StopParade,
                MenuAction::Quit,
            ]
        );
    }

    #[test]
    fn the_show_parade_entry_routes_through_the_daemon_popup_action() {
        // The macOS menu bar's "Show Parade" must go through the
        // daemon's action handler, so it inherits the close-or-spawn
        // toggle *and* the browser minimization — and never opens a
        // second parade window.
        let mut seen: Option<HotkeyAction> = None;
        let cfg = Config::default();
        let outcome = show_parade_with(&cfg, |_cfg, action| {
            seen = Some(action);
            Ok(true)
        });
        assert_eq!(
            seen,
            Some(HotkeyAction::Popup),
            "the entry must ask for the popup action"
        );
        assert_eq!(outcome, ParadeOutcome::Closed);
    }

    #[test]
    fn a_spawned_parade_is_reported_as_spawned() {
        // The daemon's handler returns false after it spawned a fresh
        // window, and true after it toggled a running one shut.
        let cfg = Config::default();
        let outcome = show_parade_with(&cfg, |_cfg, _action| Ok(false));
        assert_eq!(outcome, ParadeOutcome::Spawned);
    }

    #[test]
    fn a_failed_parade_action_is_traced_not_panicked_on() {
        // The menu-bar process must never die because the action failed.
        let cfg = Config::default();
        let outcome = show_parade_with(&cfg, |_cfg, _action| {
            Err(DaemonError::Spawn("no popup target".into()))
        });
        assert_eq!(outcome, ParadeOutcome::Failed);
    }
}
