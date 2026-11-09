//! Daemon stop signalling: `agnosis stop` asks a running daemon to exit.
//!
//! | Platform | Mechanism |
//! | --- | --- |
//! | Windows | A named manual-reset event `Local\AGnosisDaemonStop`. The
//! daemon arms it at startup and [`wait_for_daemon_stop`] waits on it with
//! `WaitForSingleObject(timeout)`; `stop` opens it with `OpenEventW` and
//! signals it with `SetEvent`. |
//! | Unix | A daemon PID file (`<config>/run/daemon.pid`, `"<pid>
//! <starttime>"`) plus `SIGTERM` via libc. The daemon side installs a
//! `SIGTERM` handler that sets a static `AtomicBool` (via `signal` from
//! libc, async-signal-safe — the handler only stores); the wait loop polls
//! the flag in ~10ms slices so a signal arriving during a blocking call
//! wakes the daemon promptly. `request_stop` signals only when the PID
//! file identity verifies (audit P0-2) and unlinks stale/foreign files.
//! `stop` reports honestly via [`StopOutcome`]: signalled, stale-cleaned,
//! or not-running. Old pid-only files are unverifiable and take the
//! stale-cleanup path. |
//! [`arm_daemon_stop`] is called once by daemon startup: it arms the stop
//! mechanism and records the daemon PID for [`request_stop_daemon`].
//!
//! On Unix the arm step also refuses to steal a record that a *live* daemon
//! already owns: overwriting it would leave the running daemon untracked
//! and unstoppable (audit P0-2). The refusal surfaces as
//! [`DaemonCtlError::AlreadyRunning`]. [`disarm_daemon_stop`] undoes the
//! arm step on the way out, so a clean stop leaves no stale PID file.
//!
//! The public error and outcome variants are constructed by this shared
//! layer, not by the per-OS backends: the backends report *facts* (a live
//! owner, a stale record, a signal that went through) and the shared code
//! turns them into [`DaemonCtlError`] / [`StopOutcome`]. That keeps every
//! variant constructible in every build — the CLI matches both enums
//! exhaustively — even though only the Unix PID-file backend can ever find
//! a stale record (audit dead-code sweep).

/// The name of the Windows stop event.
///
/// The single owner of the name: the daemon-control backend and the
/// CLI's "is a daemon running" probe both read it, so the two cannot
/// drift apart (audit 2026-10-20 §3.18 — the name was a literal in
/// two files).
#[cfg(target_os = "windows")]
pub const STOP_EVENT_NAME: &str = r"Local\AGnosisDaemonStop";

/// Failures from [`arm_daemon_stop`].
#[derive(Debug, thiserror::Error)]
pub enum DaemonCtlError {
    /// Platform backend failure with a message.
    #[error("{0}")]
    Platform(String),
    /// A live daemon already owns the platform's single-instance record
    /// (audit P0-2): the claimant refuses to steal it instead of orphaning
    /// the running daemon.
    ///
    /// The pid is the owner's when the platform can report it (the Unix
    /// PID file) and `None` when it cannot (the Windows named mutex, which
    /// carries no owner identity).
    #[error("daemon already running{owner}", owner = match .0 {
        Some(pid) => format!(" (pid {pid})"),
        None => String::new(),
    })]
    AlreadyRunning(Option<i32>),
}

/// Called once by daemon startup: arms the stop mechanism and records the
/// daemon PID.
pub fn arm_daemon_stop() -> Result<(), DaemonCtlError> {
    // The "already running" decision lives in the shared layer: only the
    // Unix backend keeps a PID record, but the refusal is a cross-platform
    // error and must be constructible in every build.
    if let Some(pid) = imp::live_owner_pid() {
        return Err(DaemonCtlError::AlreadyRunning(Some(pid)));
    }
    imp::arm()
}

/// Drop the daemon's own stop-control record (audit P0-2): a clean stop
/// must not leave a stale PID file behind. Only a record that still names
/// this process is removed.
pub fn disarm_daemon_stop() {
    imp::disarm();
}

/// Does the probed PID-file record belong to a *different* live process?
///
/// `status` and `record` come from the PID-file probe; `own_pid` is the pid
/// of the process trying to claim the record. A record that verifies as
/// [`PidStatus::Owned`] but names another pid belongs to a live daemon, so
/// the claimant must refuse instead of overwriting the record (which would
/// leave the running daemon untracked and unstoppable). The Unix backend
/// turns a `true` into the pid of that owner, which the shared layer
/// reports as [`DaemonCtlError::AlreadyRunning`].
///
/// [`PidStatus`]: crate::platform::pidfile::PidStatus
#[cfg(any(test, unix))]
fn already_running(
    status: crate::platform::pidfile::PidStatus,
    record: Option<crate::platform::pidfile::PidRecord>,
    own_pid: i32,
) -> bool {
    match (status, record) {
        (crate::platform::pidfile::PidStatus::Owned, Some(rec)) => rec.pid != own_pid,
        _ => false,
    }
}

/// Honest outcome of a stop request (audit P0-2): distinguishes a daemon
/// that was actually signalled from a stale file that was cleaned up and
/// from nothing running at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopOutcome {
    /// A verified owner was signalled.
    Signalled,
    /// A stale/foreign PID file was unlinked; nothing was signalled.
    StaleCleaned,
    /// No PID file, or no live process behind it.
    NotRunning,
}

/// What a platform backend found while probing for a running daemon.
///
/// The backends report facts, the shared layer owns the public
/// [`StopOutcome`]: only the Unix PID-file backend can find a stale
/// record, so a backend-local enum would leave that variant dead on
/// Windows (audit dead-code sweep).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StopFacts {
    /// A live owner was signalled.
    signalled: bool,
    /// A stale/foreign record was removed; nothing was signalled.
    stale_removed: bool,
}

/// Request a running daemon to stop. Returns true when a daemon was found
/// and signalled.
pub fn request_stop_daemon() -> bool {
    request_stop_status() == StopOutcome::Signalled
}

/// Request a running daemon to stop, reporting the honest outcome.
pub fn request_stop_status() -> StopOutcome {
    public_outcome(imp::stop_facts())
}

/// Map the backend facts to the public outcome (audit P0-2): a daemon that
/// was actually signalled, a stale file that was cleaned up, or nothing
/// running at all.
fn public_outcome(facts: StopFacts) -> StopOutcome {
    match (facts.signalled, facts.stale_removed) {
        (true, _) => StopOutcome::Signalled,
        (false, true) => StopOutcome::StaleCleaned,
        (false, false) => StopOutcome::NotRunning,
    }
}

/// Block up to `timeout_ms` for a stop request. Returns true when stop was
/// requested.
pub fn wait_for_daemon_stop(timeout_ms: u32) -> bool {
    imp::wait(timeout_ms)
}

/// True when a daemon's stop endpoint is armed and reachable.
///
/// The shared "is a daemon running" probe: the CLI's `start` asks it
/// before spawning a second daemon, and the popup asks it once at startup
/// so it can tell a daemon-less user why the global hotkey does nothing —
/// a parade run from a bare double-click has no keyboard hook anywhere,
/// and nothing else in the user's path ever says so.
///
/// Windows: the named stop event exists (the daemon arms it at startup
/// and the kernel destroys the name when that process dies, so there is
/// no stale state to clean). Unix: the daemon PID file names a live
/// process whose identity matches the record (audit P0-2).
pub fn endpoint_armed() -> bool {
    imp::endpoint_armed()
}

#[cfg(target_os = "windows")]
mod imp {
    use super::{DaemonCtlError, StopFacts};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::OnceLock;
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        CreateEventW, CreateMutexW, OpenEventW, ReleaseMutex, SetEvent, WaitForSingleObject,
        EVENT_MODIFY_STATE, SYNCHRONIZATION_SYNCHRONIZE,
    };

    const EVENT_NAME: &str = super::STOP_EVENT_NAME;

    /// The name of the single-instance mutex (audit 2026-10-20 §3.5).
    ///
    /// A named mutex is the canonical Windows single-instance primitive:
    /// `CreateMutexW` reports `ERROR_ALREADY_EXISTS` when another process
    /// already holds it, and the kernel releases the name when that
    /// process dies — so there is no stale state to clean up and no way
    /// to get a false "already running".
    const SINGLETON_NAME: &str = "Local\\AGnosisDaemonSingleton";

    static ARMED: AtomicBool = AtomicBool::new(false);

    /// Cached stop-event handle as a raw value (audit robustness): stored
    /// once at `arm`, reused by every `wait` slice instead of open/close
    /// per poll slice. Stored as `isize` (not `HANDLE`) because raw
    /// pointers are `!Sync` and a `static` must be `Sync`.
    static CACHED_EVENT: OnceLock<isize> = OnceLock::new();

    /// The single-instance mutex handle, held for the process lifetime.
    ///
    /// `isize` rather than `HANDLE` for the same reason as the event
    /// above: a `static` must be `Sync` and a raw pointer is not.
    static SINGLETON: OnceLock<isize> = OnceLock::new();

    fn wide_name() -> Vec<u16> {
        EVENT_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    fn wide_singleton_name() -> Vec<u16> {
        SINGLETON_NAME
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect()
    }

    /// Rebuild a `HANDLE` from the cached raw value.
    fn cached_handle() -> Option<HANDLE> {
        let raw = *CACHED_EVENT.get()?;
        if raw == 0 {
            return None;
        }
        let handle = HANDLE(raw as *mut _);
        if handle.is_invalid() {
            return None;
        }
        Some(handle)
    }

    pub(super) fn arm() -> Result<(), DaemonCtlError> {
        arm_singleton()?;
        let name = wide_name();
        unsafe {
            // Safety: CreateEventW needs only a valid name pointer, held above.
            let handle = CreateEventW(None, true, false, windows::core::PCWSTR(name.as_ptr()))
                .map_err(|e| DaemonCtlError::Platform(e.to_string()))?;
            let _ = CACHED_EVENT.set(handle.0 as isize);
            // The event handle stays open for the process lifetime on
            // purpose: `HANDLE` is a `Copy` raw pointer without a `Drop`
            // impl, so there is nothing to leak — the OS closes the handle
            // when the process exits. (`std::mem::forget` on a `Copy` value
            // was a no-op that only tripped the `forgetting_copy_types` lint.)
        }
        ARMED.store(true, Ordering::SeqCst);
        Ok(())
    }

    /// Take the single-instance mutex, or refuse when a daemon already
    /// holds it (audit 2026-10-20 §3.5).
    ///
    /// The mutex is created with initial ownership, so the claim is
    /// exclusive. `CreateMutexW` succeeds on an existing name **and**
    /// reports `ERROR_ALREADY_EXISTS` — that error is the "another daemon
    /// is running" signal, and the daemon refuses to start rather than
    /// installing a second global keyboard hook (which made one press
    /// spawn two parade windows).
    fn arm_singleton() -> Result<(), DaemonCtlError> {
        let name = wide_singleton_name();
        let created = unsafe {
            // Safety: CreateMutexW needs only a valid name pointer, held
            // above for the call duration.
            match CreateMutexW(None, true, windows::core::PCWSTR(name.as_ptr())) {
                Ok(handle) => {
                    let already = windows::Win32::Foundation::GetLastError()
                        == windows::Win32::Foundation::ERROR_ALREADY_EXISTS;
                    if already {
                        // The handle still has to be closed: the *other*
                        // process owns the mutex, not this one.
                        let _ = windows::Win32::Foundation::CloseHandle(handle);
                    } else {
                        let _ = SINGLETON.set(handle.0 as isize);
                    }
                    !already
                }
                Err(e) => {
                    return Err(DaemonCtlError::Platform(format!(
                        "could not create the single-instance mutex: {e}"
                    )));
                }
            }
        };
        if !created {
            return Err(DaemonCtlError::AlreadyRunning(None));
        }
        Ok(())
    }

    /// Handle for signalling: the cached armed handle when present, else a
    /// one-shot open (the `stop` side never armed the event itself).
    fn signal_handle() -> (Option<HANDLE>, bool) {
        if let Some(h) = cached_handle() {
            return (Some(h), false);
        }
        let name = wide_name();
        unsafe {
            match OpenEventW(
                EVENT_MODIFY_STATE,
                false,
                windows::core::PCWSTR(name.as_ptr()),
            ) {
                Ok(h) => (Some(h), true),
                Err(_) => (None, false),
            }
        }
    }

    /// Windows keeps no PID record for the daemon (the named stop event is
    /// the record), so there is no live owner to report.
    pub(super) fn live_owner_pid() -> Option<i32> {
        None
    }

    pub(super) fn stop_facts() -> StopFacts {
        unsafe {
            // Safety: OpenEventW/SetEvent need only a valid name pointer.
            let (handle, opened) = signal_handle();
            let Some(handle) = handle else {
                return StopFacts {
                    signalled: false,
                    stale_removed: false,
                };
            };
            let signalled = SetEvent(handle).is_ok();
            if opened {
                let _ = windows::Win32::Foundation::CloseHandle(handle);
            }
            StopFacts {
                signalled,
                stale_removed: false,
            }
        }
    }

    pub(super) fn wait(timeout_ms: u32) -> bool {
        if !ARMED.load(Ordering::SeqCst) {
            return false;
        }
        let Some(handle) = cached_handle() else {
            return open_and_wait_once(timeout_ms);
        };
        unsafe {
            // Safety: cached handle from arm, valid for process lifetime.
            WaitForSingleObject(handle, timeout_ms) == WAIT_OBJECT_0
        }
    }

    /// The daemon is reachable when its stop event already exists.
    ///
    /// The same name the daemon creates — the constant, so the two can
    /// never drift apart. The constant carries the `Local\` prefix
    /// (audit 2026-10-22 §P2-31).
    pub(super) fn endpoint_armed() -> bool {
        use windows::Win32::System::Threading::{OpenEventW, EVENT_MODIFY_STATE};
        let name = wide_name();
        // Safety: `OpenEventW` takes one NUL-terminated name pointer and
        // returns a handle it owns the lifetime of.
        unsafe {
            OpenEventW(
                EVENT_MODIFY_STATE,
                false,
                windows::core::PCWSTR(name.as_ptr()),
            )
            .is_ok()
        }
    }

    /// Drop the daemon's own records.
    ///
    /// The stop event needs no cleanup (the OS closes the handle when the
    /// process exits), but the single-instance mutex is released here so
    /// the name is free for the next daemon while this process is still
    /// winding down.
    pub(super) fn disarm() {
        let Some(raw) = SINGLETON.get().copied() else {
            return;
        };
        if raw == 0 {
            return;
        }
        let handle = HANDLE(raw as *mut _);
        if handle.is_invalid() {
            return;
        }
        unsafe {
            // Safety: the handle came from our own CreateMutexW call and
            // is still owned by this process.
            let _ = ReleaseMutex(handle);
            let _ = windows::Win32::Foundation::CloseHandle(handle);
        }
    }

    /// Fallback when `arm` never cached (e.g. `wait` before `arm`): one
    /// one-shot open instead of per-slice churn in the caller loop.
    fn open_and_wait_once(timeout_ms: u32) -> bool {
        let name = wide_name();
        unsafe {
            // Safety: name pointer held above for the call duration.
            let Ok(handle) = OpenEventW(
                SYNCHRONIZATION_SYNCHRONIZE,
                false,
                windows::core::PCWSTR(name.as_ptr()),
            ) else {
                return false;
            };
            let r = WaitForSingleObject(handle, timeout_ms);
            let _ = windows::Win32::Foundation::CloseHandle(handle);
            r == WAIT_OBJECT_0
        }
    }
}

#[cfg(unix)]
mod imp {
    use super::{DaemonCtlError, StopFacts};
    use std::sync::atomic::{AtomicBool, Ordering};

    static STOP_FLAG: AtomicBool = AtomicBool::new(false);

    unsafe extern "C" fn on_sigterm(_: libc::c_int) {
        STOP_FLAG.store(true, Ordering::SeqCst);
    }

    fn daemon_pid_file() -> Option<std::path::PathBuf> {
        crate::paths::runtime_dir()
            .map(|d| d.join("daemon.pid"))
            .ok()
    }

    pub(super) fn arm() -> Result<(), DaemonCtlError> {
        unsafe {
            // Safety: libc::signal handler install; handler only stores a flag.
            // The handler is cast through a raw pointer first: `sighandler_t`
            // is an integer type, and casting a function item straight into
            // it trips the `function_casts_as_integer` lint.
            let handler = on_sigterm as *const () as libc::sighandler_t;
            if libc::signal(libc::SIGTERM, handler) == libc::SIG_ERR {
                return Err(DaemonCtlError::Platform(
                    "could not install SIGTERM handler".into(),
                ));
            }
        }
        STOP_FLAG.store(false, Ordering::SeqCst);
        let path =
            daemon_pid_file().ok_or_else(|| DaemonCtlError::Platform("no runtime dir".into()))?;
        claim_the_record(&path)
    }

    /// The pid of a *different* live daemon that owns the PID-file record, if
    /// any (audit P0-2): the claimant must refuse instead of overwriting the
    /// record, which would leave the running daemon untracked and
    /// unstoppable.
    pub(super) fn live_owner_pid() -> Option<i32> {
        let path = daemon_pid_file()?;
        let (status, record) = crate::platform::pidfile::status_of(&path);
        let owner_pid = record.map(|r| r.pid).unwrap_or(0);
        if super::already_running(status, record, own_pid()) {
            return Some(owner_pid);
        }
        None
    }

    /// This process's pid.
    fn own_pid() -> i32 {
        unsafe { libc::getpid() }
    }

    /// Drop our own PID record (audit P0-2): a clean stop must not leave a
    /// stale file behind. A record another process owns now is left alone.
    pub(super) fn disarm() {
        let Some(path) = daemon_pid_file() else {
            return;
        };
        crate::platform::pidfile::unlink_if_owned(&path, own_pid());
    }

    /// Unlink a stale/foreign PID file left by a crash (audit P0-2), so a
    /// fresh `arm` — and hence `start` — never refuses after a crash.
    fn cleanup_stale_at_startup(path: &std::path::Path) {
        use crate::platform::pidfile::PidStatus;
        // An **empty** file is a record that is being written right now
        // (the claim creates the file before it writes the body), so it
        // is never unlinked: unlinking it would hand the record to the
        // next daemon while the first one believes it owns it (audit
        // 2026-10-21 §G27).
        if matches!(std::fs::read_to_string(path), Ok(text) if text.trim().is_empty()) {
            return;
        }
        if let (PidStatus::Stale | PidStatus::Foreign, _) =
            crate::platform::pidfile::status_of(path)
        {
            crate::platform::pidfile::unlink_stale(path);
        }
    }

    /// Claim the daemon's PID record for this process.
    ///
    /// The claim is an **atomic create**, so two daemons that start at
    /// the same instant cannot both win: the loser is told that a daemon
    /// is already running instead of overwriting the winner's record,
    /// which would leave the running daemon untracked and unstoppable
    /// (audit 2026-10-21 §G27).
    ///
    /// A record that is **stale** (its owner is gone) is cleaned up and
    /// the claim is retried once — that is the ordinary "the last daemon
    /// died" case.
    fn claim_the_record(path: &std::path::Path) -> Result<(), DaemonCtlError> {
        use crate::platform::pidfile::ClaimError;
        match crate::platform::pidfile::claim_own_record(path) {
            ClaimError::Claimed => Ok(()),
            ClaimError::Taken => {
                cleanup_stale_at_startup(path);
                match crate::platform::pidfile::claim_own_record(path) {
                    ClaimError::Claimed => Ok(()),
                    ClaimError::Taken => Err(DaemonCtlError::AlreadyRunning(None)),
                    ClaimError::Io(e) => Err(DaemonCtlError::Platform(e)),
                }
            }
            ClaimError::Io(e) => Err(DaemonCtlError::Platform(e)),
        }
    }

    pub(super) fn stop_facts() -> StopFacts {
        use crate::platform::pidfile::PidStatus;
        let Some(path) = daemon_pid_file() else {
            return StopFacts {
                signalled: false,
                stale_removed: false,
            };
        };
        let (status, record) = crate::platform::pidfile::status_of(&path);
        match status {
            PidStatus::Owned => {
                let Some(record) = record else {
                    return StopFacts {
                        signalled: false,
                        stale_removed: false,
                    };
                };
                let signalled = crate::platform::pidfile::signal_if_owned(record, libc::SIGTERM);
                StopFacts {
                    signalled,
                    stale_removed: false,
                }
            }
            PidStatus::Stale | PidStatus::Foreign => {
                crate::platform::pidfile::unlink_stale(&path);
                StopFacts {
                    signalled: false,
                    stale_removed: true,
                }
            }
            PidStatus::Missing => StopFacts {
                signalled: false,
                stale_removed: false,
            },
        }
    }

    /// Poll the `SIGTERM` flag in ~10ms slices so a signal arriving during
    /// a blocking hotkey call still wakes the daemon promptly.
    pub(super) fn wait(timeout_ms: u32) -> bool {
        let deadline =
            std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64);
        loop {
            if STOP_FLAG.load(Ordering::SeqCst) {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return STOP_FLAG.load(Ordering::SeqCst);
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }

    /// The daemon is reachable when its PID file names a live process
    /// whose identity matches the record (audit P0-2).
    pub(super) fn endpoint_armed() -> bool {
        use crate::platform::pidfile::PidStatus;
        let Some(path) = daemon_pid_file() else {
            return false;
        };
        matches!(
            crate::platform::pidfile::status_of(&path).0,
            PidStatus::Owned
        )
    }
}

#[cfg(not(any(target_os = "windows", unix)))]
mod imp {
    use super::{DaemonCtlError, StopFacts};

    pub(super) fn arm() -> Result<(), DaemonCtlError> {
        Err(DaemonCtlError::Platform("unsupported platform".into()))
    }

    /// No PID record exists on this platform.
    pub(super) fn live_owner_pid() -> Option<i32> {
        None
    }

    pub(super) fn stop_facts() -> StopFacts {
        StopFacts {
            signalled: false,
            stale_removed: false,
        }
    }

    pub(super) fn wait(_timeout_ms: u32) -> bool {
        false
    }

    /// No daemon endpoint exists on this platform.
    pub(super) fn endpoint_armed() -> bool {
        false
    }

    /// No PID file exists on this platform.
    pub(super) fn disarm() {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::pidfile::{PidRecord, PidStatus};

    fn record(pid: i32) -> Option<PidRecord> {
        Some(PidRecord {
            pid,
            starttime: Some(7),
        })
    }

    #[test]
    fn live_owner_blocks_a_second_daemon() {
        assert!(already_running(PidStatus::Owned, record(4242), 1234));
    }

    #[test]
    fn own_record_does_not_block() {
        assert!(!already_running(PidStatus::Owned, record(1234), 1234));
    }

    #[test]
    fn stale_or_foreign_record_does_not_block() {
        assert!(!already_running(PidStatus::Stale, record(4242), 1234));
        assert!(!already_running(PidStatus::Foreign, record(4242), 1234));
        assert!(!already_running(PidStatus::Missing, None, 1234));
    }

    #[test]
    fn already_running_error_names_the_owner() {
        let e = DaemonCtlError::AlreadyRunning(Some(4242));
        assert_eq!(e.to_string(), "daemon already running (pid 4242)");
    }

    #[test]
    fn stop_outcomes_are_distinct() {
        assert_ne!(StopOutcome::Signalled, StopOutcome::StaleCleaned);
        assert_ne!(StopOutcome::StaleCleaned, StopOutcome::NotRunning);
        assert_ne!(StopOutcome::Signalled, StopOutcome::NotRunning);
    }

    #[test]
    fn public_outcome_maps_the_backend_facts() {
        // The shared layer owns the public outcome, so the mapping is
        // pinned here: a signalled owner wins over everything else, a
        // removed stale record is its own outcome, and "nothing there" is
        // the third.
        assert_eq!(
            public_outcome(StopFacts {
                signalled: true,
                stale_removed: false
            }),
            StopOutcome::Signalled
        );
        assert_eq!(
            public_outcome(StopFacts {
                signalled: false,
                stale_removed: true
            }),
            StopOutcome::StaleCleaned
        );
        assert_eq!(
            public_outcome(StopFacts {
                signalled: false,
                stale_removed: false
            }),
            StopOutcome::NotRunning
        );
    }
}
