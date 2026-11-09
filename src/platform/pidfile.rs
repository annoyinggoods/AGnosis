//! Shared Unix PID-file ownership helpers (audit P0-2).
//!
//! Format is `"<pid> <starttime>"`, tolerant of old pid-only files which
//! parse as `(pid, None)`. The start time is the process's identity
//! fingerprint and it is read from the kernel on both supported Unixes:
//!
//! * Linux — `/proc/<pid>/stat` field 22 (clock ticks since boot).
//! * macOS — `proc_pidinfo(PROC_PIDTBSDINFO)`, whose
//!   `pbi_start_tvsec`/`pbi_start_tvusec` are the process's absolute start
//!   time (wall clock, `timeval` shape).
//!
//! Identity holds only when the recorded value equals the live value, so a
//! recycled PID never matches. Old pid-only files are unverifiable and take
//! the stale-cleanup path (never signalled, file unlinked); so is the
//! `"<pid> 0"` record the pre-start-time macOS build wrote, because 0 is
//! the "no start time" sentinel and a real start time is never 0.
//!
//! The macOS identity check used to be a `ps -p <pid> -o command=`
//! containment search for the string `ducks`, and that was wrong twice
//! over: any process whose command line merely *mentioned* ducks
//! (`vim ~/agnosis/config.toml`, `tail -f /var/log/agnosis.log`) was
//! classified as owned and SIGTERMed, and a binary installed under another
//! name (`quack`) never matched, so every PID file read as stale while the
//! daemon was alive. The start-time comparison has neither problem: it is
//! exact, and it does not depend on the binary's name.
//!
//! The whole module is Unix-only: Windows keeps no PID file (the daemon is
//! found through a named stop event, the popup through its window class),
//! so in a Windows build this file compiles to nothing.
//!
//! Field-index arithmetic (Linux, audit P0-2 follow-up): `/proc/<pid>/stat`
//! is `"<pid> (<comm>) <state> ..."` and `<comm>` may contain spaces, so
//! the line is sliced after the closing `)`. From there token 0 is field 3
//! (`state`), i.e. **field N lives at index N-3** — `starttime` is field 22
//! and therefore index **19**. Reading index 20 picked `vsize` (field 23),
//! which changes whenever the memory map changes: live daemons were then
//! misclassified as recycled PIDs and their PID files unlinked instead of
//! signalled.

/// The import is Unix/test-only: every `Path` user in this module is a
/// PID-file helper, and Windows has no PID file (audit dead-code sweep).
#[cfg(any(test, unix))]
use std::path::Path;

/// Parsed PID file: pid plus optional recorded starttime. Old pid-only
/// files parse as `(pid, None)` and are treated as unverifiable.
/// Unix production + cross-platform tests only (Windows non-test builds
/// would flag it dead, hence the gate).
#[cfg(any(test, unix))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PidRecord {
    /// Recorded process id.
    pub pid: i32,
    /// Recorded start time — Linux `/proc` field 22 (clock ticks since
    /// boot), macOS `pbi_start_tvsec`/`pbi_start_tvusec` (microseconds
    /// since the epoch) — if present.
    pub starttime: Option<u64>,
}

/// Outcome of checking one PID file against the live process table.
#[cfg(any(test, unix))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidStatus {
    /// Live process whose identity matches the record.
    Owned,
    /// Live PID but identity does not match (recycled) — never signal.
    Foreign,
    /// No live process, or an unverifiable old-format record.
    Stale,
    /// File missing or unreadable.
    Missing,
}

/// The "no start time" sentinel.
///
/// The pre-start-time macOS build wrote `"<pid> 0"`, and 0 can never be a
/// real start time (Linux counts clock ticks since boot, macOS counts
/// microseconds since the epoch). A record carrying 0 therefore says
/// "unknown", not "started at 0", and must stay unverifiable.
#[cfg(any(test, unix))]
const NO_START_TIME: u64 = 0;

/// Format one PID-file body: `"<pid> <starttime>"`.
#[cfg(any(test, unix))]
pub fn format_record(pid: i32, starttime: u64) -> String {
    format!("{pid} {starttime}")
}

/// Parse one PID-file body; old pid-only files yield `starttime: None`.
#[cfg(any(test, unix))]
pub fn parse_record(text: &str) -> Option<PidRecord> {
    let mut parts = text.split_whitespace();
    let pid: i32 = parts.next()?.trim().parse().ok()?;
    if pid <= 0 {
        return None;
    }
    let starttime = match parts.next() {
        Some(s) => Some(s.parse().ok()?),
        None => None,
    };
    Some(PidRecord { pid, starttime })
}

/// The start time `record` actually carries, or `None` when the record
/// holds no usable one.
///
/// `None` means *unverifiable*: the record says nothing about which
/// process wrote it, so the caller must never signal the PID. Two forms
/// land here — a pid-only file from an old build, and the `"<pid> 0"`
/// sentinel the pre-start-time macOS build wrote.
#[cfg(any(test, unix))]
fn recorded_starttime(record: PidRecord) -> Option<u64> {
    record.starttime.filter(|t| *t != NO_START_TIME)
}

/// Live starttime of `pid` on Linux (`/proc/<pid>/stat` field 22).
///
/// `stat_after_comm` slices the line past the `(comm)` field, where token 0
/// is field 3 (`state`); field N therefore sits at index N-3 and
/// `starttime` (field 22) at index **19**. Index 20 is field 23 (`vsize`),
/// which drifts with the memory map and made live daemons look recycled.
#[cfg(target_os = "linux")]
pub fn live_starttime(pid: i32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat_after_comm(&stat)?
        .split_whitespace()
        .nth(19)?
        .parse()
        .ok()
}

/// Slice of `/proc/<pid>/stat` after the `(comm)` field, so spaces in the
/// process name cannot shift the field indices.
#[cfg(target_os = "linux")]
fn stat_after_comm(stat: &str) -> Option<&str> {
    let close = stat.rfind(')')?;
    Some(stat[close + 1..].trim_start())
}

/// Darwin's `struct proc_bsdinfo` (`<sys/proc_info.h>`), mirrored here.
///
/// The `libc` crate declares the same struct but **without**
/// `#[repr(C)]`, so Rust's default layout reorders its fields and the
/// `libc` copy does not match the C ABI: there `pbi_start_tvsec` lands at
/// offset 48 instead of 120, and reading the start time out of it returns
/// garbage. This `repr(C)` mirror follows the header's field order and
/// types; its size (136 bytes) is exactly what `proc_pidinfo` writes for
/// the `PROC_PIDTBSDINFO` flavor.
#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct BsdInfo {
    pbi_flags: u32,
    pbi_status: u32,
    pbi_xstatus: u32,
    pbi_pid: u32,
    pbi_ppid: u32,
    pbi_uid: libc::uid_t,
    pbi_gid: libc::gid_t,
    pbi_ruid: libc::uid_t,
    pbi_rgid: libc::gid_t,
    pbi_svuid: libc::uid_t,
    pbi_svgid: libc::gid_t,
    rfu_1: u32,
    pbi_comm: [libc::c_char; 16],
    pbi_name: [libc::c_char; 32],
    pbi_nfiles: u32,
    pbi_pgid: u32,
    pbi_pjobc: u32,
    e_tdev: u32,
    e_tpgid: u32,
    pbi_nice: i32,
    pbi_start_tvsec: u64,
    pbi_start_tvusec: u64,
}

/// The C layout the kernel writes, asserted at compile time so a wrong
/// mirror fails the build instead of silently reading garbage: XNU's
/// `bsd/sys/proc_info.h` puts `pbi_start_tvsec` at offset 120 and
/// `pbi_start_tvusec` at 128, and the whole struct is 136 bytes — the
/// size `proc_pidinfo` reports for the `PROC_PIDTBSDINFO` flavor.
#[cfg(target_os = "macos")]
const _: () = {
    assert!(std::mem::size_of::<BsdInfo>() == 136);
    assert!(std::mem::offset_of!(BsdInfo, pbi_pid) == 12);
    assert!(std::mem::offset_of!(BsdInfo, pbi_comm) == 48);
    assert!(std::mem::offset_of!(BsdInfo, pbi_name) == 64);
    assert!(std::mem::offset_of!(BsdInfo, pbi_start_tvsec) == 120);
    assert!(std::mem::offset_of!(BsdInfo, pbi_start_tvusec) == 128);
};

/// Live starttime of `pid` on macOS.
///
/// `proc_pidinfo(PROC_PIDTBSDINFO)` hands back the kernel's
/// `struct proc_bsdinfo`, whose `pbi_start_tvsec`/`pbi_start_tvusec` are
/// the process's absolute start time (`timeval` shape: seconds and
/// microseconds since the epoch, set by the kernel from `microtime` at
/// fork time and kept across `exec`). The pair is folded into microseconds
/// since the epoch, which is unique per process *and* per start, so a
/// recycled PID never matches — the same guarantee `/proc/<pid>/stat` gives
/// on Linux.
///
/// `sysctl(KERN_PROC_PID)` would answer the same question, but the `libc`
/// crate exposes no `kinfo_proc` layout, and hand-rolling that much
/// struct for one field is strictly worse than libproc's 136-byte one.
#[cfg(target_os = "macos")]
pub fn live_starttime(pid: i32) -> Option<u64> {
    // Two slots: the kernel writes `sizeof(struct proc_bsdinfo)` bytes and
    // reports that size. A buffer of exactly one slot would be refused
    // once the kernel struct grows a trailing field; two slots absorb
    // that, and the first one still holds the header's layout.
    let mut buf = [BsdInfo::default(); 2];
    let want = std::mem::size_of::<BsdInfo>() as libc::c_int;
    let room = (buf.len() * std::mem::size_of::<BsdInfo>()) as libc::c_int;
    // SAFETY: `buf` is a live, correctly aligned buffer of `room` bytes;
    // the kernel writes at most `sizeof(struct proc_bsdinfo)` bytes into
    // it, which is smaller than `room`.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            buf.as_mut_ptr() as *mut libc::c_void,
            room,
        )
    };
    if written < want {
        return None;
    }
    start_micros(buf[0].pbi_start_tvsec, buf[0].pbi_start_tvusec)
}

/// The start times the macOS check accepts: 2020-01-01 to 2100-01-01, in
/// microseconds since the epoch.
///
/// A misaligned read — a kernel struct that grew in the middle instead of
/// at the end — would hand back a constant, implausible value, and a
/// constant identity would match *every* process. Rejecting implausible
/// values keeps such a record unverifiable, so it is never signalled.
#[cfg(target_os = "macos")]
const START_MICROS_RANGE: std::ops::RangeInclusive<u64> =
    1_577_836_800_000_000..=4_102_444_800_000_000;

/// Fold a `timeval`-shaped start time into microseconds since the epoch.
///
/// `tvsec` is ~1.8e9 today, so the product is ~1.8e15 — comfortably inside
/// a `u64` and inside the PID file's text field, which is why the record
/// format did not have to change. A value outside [`START_MICROS_RANGE`] is
/// not a start time at all and is refused.
#[cfg(target_os = "macos")]
fn start_micros(tvsec: u64, tvusec: u64) -> Option<u64> {
    let micros = tvsec.saturating_mul(1_000_000).saturating_add(tvusec);
    if !START_MICROS_RANGE.contains(&micros) {
        return None;
    }
    Some(micros)
}

/// Live starttime of `pid` on a Unix with no start-time source.
///
/// There is no portable way to read a process's start time, so the
/// platform stays *unverifiable*: a record written here carries no usable
/// identity, and an unverifiable record is never signalled.
#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub fn live_starttime(_pid: i32) -> Option<u64> {
    None
}

/// True when `pid` currently names a live process (signal 0 probe).
///
/// `kill(pid, 0)` fails with `EPERM` when the process exists but belongs
/// to another user. The process IS alive in that case — only the signal
/// would be refused — so `EPERM` must not be read as "gone"; a process
/// that really is gone answers `ESRCH`. Reading `EPERM` as "gone" let a
/// live daemon's PID file be unlinked as stale.
///
/// `pid` 0 and negative values are rejected: `kill(0, 0)` and `kill(-1, 0)`
/// are broadcast probes, not "is this pid alive" questions.
#[cfg(unix)]
pub fn is_alive(pid: i32) -> bool {
    if pid <= 0 {
        return false;
    }
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Classify `record` against the live process table.
///
/// The live process's start time is compared with the recorded one; equal
/// means the very process that wrote the record is still there
/// ([`PidStatus::Owned`]), different means the PID was recycled
/// ([`PidStatus::Foreign`]). A record without a usable start time is
/// unverifiable and reports [`PidStatus::Stale`] so the caller takes the
/// stale-cleanup path instead of signalling.
#[cfg(unix)]
pub fn check_record(record: PidRecord) -> PidStatus {
    if !is_alive(record.pid) {
        return PidStatus::Stale;
    }
    let Some(want) = recorded_starttime(record) else {
        return PidStatus::Stale;
    };
    match live_identity_matches(record.pid, want) {
        Some(true) => PidStatus::Owned,
        Some(false) => PidStatus::Foreign,
        None => PidStatus::Stale,
    }
}

/// Platform identity comparison: the live start time must equal the
/// recorded one exactly.
#[cfg(unix)]
fn live_identity_matches(pid: i32, want: u64) -> Option<bool> {
    live_starttime(pid).map(|live| live == want)
}

/// Classify the PID file at `path` without signalling anything.
#[cfg(unix)]
pub fn status_of(path: &Path) -> (PidStatus, Option<PidRecord>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (PidStatus::Missing, None),
    };
    let Some(record) = parse_record(&text) else {
        return (PidStatus::Stale, None);
    };
    let status = check_record(record);
    (status, Some(record))
}

/// Signal `record` with `sig` only when identity verifies as [`PidStatus::Owned`].
#[cfg(unix)]
pub fn signal_if_owned(record: PidRecord, sig: libc::c_int) -> bool {
    if check_record(record) != PidStatus::Owned {
        return false;
    }
    unsafe { libc::kill(record.pid, sig) == 0 }
}

/// Best-effort unlink of a stale/foreign PID file.
#[cfg(any(test, unix))]
pub fn unlink_stale(path: &Path) {
    let _ = std::fs::remove_file(path);
}

/// Does the probed PID-file record still belong to `own_pid`?
///
/// `status` and `record` come from [`status_of`]; `own_pid` is the process
/// asking to remove the record. Only a record that verifies as
/// [`PidStatus::Owned`] *and* names this process may be removed: a record
/// another live process owns now (a newer daemon) must survive.
#[cfg(any(test, unix))]
pub fn owns_record(status: PidStatus, record: Option<PidRecord>, own_pid: i32) -> bool {
    match (status, record) {
        (PidStatus::Owned, Some(rec)) => rec.pid == own_pid,
        _ => false,
    }
}

/// Unlink the PID file at `path` only while it still records `own_pid`'s
/// identity; returns whether the file was removed.
#[cfg(unix)]
pub fn unlink_if_owned(path: &Path, own_pid: i32) -> bool {
    let (status, record) = status_of(path);
    if !owns_record(status, record, own_pid) {
        return false;
    }
    unlink_stale(path);
    true
}

/// Record our own identity at `path`, but **only when the file is free**.
///
/// A `Missing`, `Stale` or `Foreign` record is overwritten (a recycled
/// PID is not the owner it names); a record a **live** process owns is
/// refused with an error that names its PID.
///
/// WHY the editor needs this and not [`claim_own_record`]: the claim is
/// an atomic create, so a record a crashed editor left behind would
/// block every future editor for good. This check reads the record
/// *first* and takes the file only when nobody live owns it
/// (audit 2026-10-22 §P2-12).
#[cfg(unix)]
pub fn write_own_record_if_free(path: &Path) -> Result<(), String> {
    let (status, _) = status_of(path);
    if let PidStatus::Owned = status {
        return Err("another agnosis editor is already running".into());
    }
    write_own_record(path)
}

/// Record our own identity for the PID file at `path`.
///
/// A record without a usable starttime is unverifiable: writing one would
/// make the daemon look like a recycled PID to the next reader, so the
/// write fails loudly instead.
#[cfg(unix)]
pub fn write_own_record(path: &Path) -> Result<(), String> {
    let pid = unsafe { libc::getpid() };
    let starttime =
        live_starttime(pid).ok_or_else(|| format!("cannot read starttime for pid {pid}"))?;
    std::fs::write(path, format_record(pid, starttime)).map_err(|e| e.to_string())
}

/// The outcome of claiming a PID file for this process.
#[cfg(unix)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    /// The record was written: this process owns it.
    Claimed,
    /// The record was already there: another daemon owns it.
    Taken,
    /// The record could not be written.
    Io(String),
}

/// Claim the PID file at `path` for this process.
///
/// The claim is an **atomic create** (`create_new(true)`): two daemons
/// that start at the same instant both see "no owner", and only the one
/// that wins the create owns the record. The loser is told so instead
/// of overwriting the winner's PID file, which would leave the running
/// daemon untracked and unstoppable (audit 2026-10-21 §G27).
#[cfg(unix)]
pub fn claim_own_record(path: &Path) -> ClaimError {
    use std::io::Write;

    let pid = unsafe { libc::getpid() };
    let Some(starttime) = live_starttime(pid) else {
        return ClaimError::Io(format!("cannot read starttime for pid {pid}"));
    };
    let body = format_record(pid, starttime);
    let mut file = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return ClaimError::Taken,
        Err(e) => return ClaimError::Io(e.to_string()),
    };
    match file.write_all(body.as_bytes()) {
        Ok(()) => ClaimError::Claimed,
        Err(e) => {
            // The record is unusable: remove it, so the next daemon does
            // not read a half-written body.
            let _ = std::fs::remove_file(path);
            ClaimError::Io(e.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fresh temp directory for one test; the caller removes it again.
    #[cfg(unix)]
    fn temp_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("agnosis-pidfile-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir must be creatable");
        dir
    }

    #[test]
    fn parses_new_format() {
        let r = parse_record("1234 567890\n").expect("must parse");
        assert_eq!(r.pid, 1234);
        assert_eq!(r.starttime, Some(567890));
    }

    #[test]
    fn parses_legacy_pid_only() {
        let r = parse_record("1234\n").expect("must parse");
        assert_eq!(r.pid, 1234);
        assert_eq!(r.starttime, None);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_record("").is_none());
        assert!(parse_record("abc").is_none());
        assert!(parse_record("0").is_none());
        assert!(parse_record("-5 10").is_none());
    }

    #[test]
    fn formats_round_trip() {
        let body = format_record(42, 777);
        assert_eq!(
            parse_record(&body),
            Some(PidRecord {
                pid: 42,
                starttime: Some(777)
            })
        );
    }

    #[test]
    fn format_holds_a_macos_magnitude_starttime() {
        // The macOS start time is microseconds since the epoch (~1.8e15);
        // the record format carries it without a change.
        let body = format_record(42, 1_780_000_000_123_456);
        assert_eq!(
            parse_record(&body),
            Some(PidRecord {
                pid: 42,
                starttime: Some(1_780_000_000_123_456)
            })
        );
    }

    #[test]
    fn pid_only_record_carries_no_starttime() {
        let rec = PidRecord {
            pid: 42,
            starttime: None,
        };
        assert_eq!(recorded_starttime(rec), None);
    }

    #[test]
    fn zero_starttime_is_the_no_start_time_sentinel() {
        // The pre-start-time macOS build wrote "<pid> 0".
        let rec = PidRecord {
            pid: 42,
            starttime: Some(NO_START_TIME),
        };
        assert_eq!(recorded_starttime(rec), None);
    }

    #[test]
    fn real_starttime_is_kept() {
        let rec = PidRecord {
            pid: 42,
            starttime: Some(1_780_000_000_123_456),
        };
        assert_eq!(recorded_starttime(rec), Some(1_780_000_000_123_456));
    }

    #[test]
    fn stale_file_unlink_in_tempdir() {
        let dir = std::env::temp_dir().join("agnosis-pidfile-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("x.pid");
        let _ = std::fs::write(&path, "1");
        unlink_stale(&path);
        assert!(!path.exists());
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stat_after_comm_skips_spaced_comm() {
        // After the `(comm)` field token 0 is field 3 (`state`), so field 22
        // (`starttime`) is token 19 — 18 filler tokens sit in between.
        let stat = "12 (my proc) R 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 4242 20";
        assert_eq!(
            stat_after_comm(stat).and_then(|s| s.split_whitespace().nth(19)),
            Some("4242")
        );
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn stat_starttime_index_is_field_22() {
        // A realistic /proc/<pid>/stat head: pid, (comm), state, ppid, pgrp,
        // session, tty_nr, tpgid, flags, minflt, cminflt, majflt, cmajflt,
        // utime, stime, cutime, cstime, priority, nice, num_threads,
        // itrealvalue, starttime (=987654), vsize (=12345678), ...
        let stat = "4242 (agnosis --daemon) S 1 4242 4242 0 -1 4194304 1234 0 0 0 11 22 0 0 20 0 1 0 987654 12345678 456 18446744073709551615";
        let tail = stat_after_comm(stat).expect("comm must be sliced off");
        assert_eq!(tail.split_whitespace().nth(19), Some("987654"));
        assert_eq!(
            tail.split_whitespace().nth(20),
            Some("12345678"),
            "index 20 is vsize, not starttime"
        );
    }

    #[test]
    fn owns_record_only_for_our_own_live_record() {
        let mine = Some(PidRecord {
            pid: 42,
            starttime: Some(7),
        });
        let theirs = Some(PidRecord {
            pid: 43,
            starttime: Some(7),
        });
        assert!(owns_record(PidStatus::Owned, mine, 42));
        assert!(!owns_record(PidStatus::Owned, theirs, 42));
        assert!(!owns_record(PidStatus::Foreign, mine, 42));
        assert!(!owns_record(PidStatus::Stale, mine, 42));
        assert!(!owns_record(PidStatus::Owned, None, 42));
    }

    #[test]
    #[cfg(unix)]
    fn old_pid_only_record_is_never_owned() {
        // An old build's pid-only file cannot be verified, so it must not
        // be signalled — not even when the pid is our own, live process.
        let dir = temp_dir("legacy-pid-only");
        let path = dir.join("legacy.pid");
        std::fs::write(&path, format!("{}\n", std::process::id())).expect("write must work");
        let (status, record) = status_of(&path);
        assert_eq!(status, PidStatus::Stale);
        assert_eq!(record.map(|r| r.starttime), Some(None));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(unix)]
    fn old_zero_sentinel_record_is_never_owned() {
        // The pre-start-time macOS build wrote "<pid> 0"; the same pid with
        // a real start time is owned, so only the sentinel can be the
        // difference.
        let dir = temp_dir("legacy-zero-sentinel");
        let path = dir.join("legacy.pid");
        std::fs::write(&path, format!("{} 0\n", std::process::id())).expect("write must work");
        let (status, record) = status_of(&path);
        assert_eq!(status, PidStatus::Stale);
        assert_eq!(record.map(|r| r.starttime), Some(Some(NO_START_TIME)));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn mismatching_starttime_is_foreign_and_never_owned() {
        let pid = std::process::id() as i32;
        let live = live_starttime(pid).expect("live starttime must be readable");
        let rec = PidRecord {
            pid,
            starttime: Some(live.saturating_add(1)),
        };
        assert_eq!(check_record(rec), PidStatus::Foreign);
    }

    #[test]
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn own_record_round_trips_as_owned() {
        let dir = temp_dir("own-record");
        let path = dir.join("own.pid");
        write_own_record(&path).expect("write must work");
        let (status, record) = status_of(&path);
        assert_eq!(status, PidStatus::Owned);
        assert_eq!(record.map(|r| r.pid), Some(std::process::id() as i32));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_starttime_is_stable_and_nonzero() {
        let pid = std::process::id() as i32;
        let first = live_starttime(pid).expect("live starttime must be readable");
        let second = live_starttime(pid).expect("live starttime must be readable");
        assert_eq!(first, second);
        assert_ne!(first, NO_START_TIME);
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn macos_starttime_rejects_implausible_values() {
        // A misaligned read would yield a constant; such a value must not
        // be able to stand in for an identity.
        assert_eq!(start_micros(0, 0), None);
        assert_eq!(start_micros(1, 0), None);
        assert_eq!(start_micros(u64::MAX, u64::MAX), None);
        assert_eq!(
            start_micros(1_700_000_000, 123_456),
            Some(1_700_000_000_123_456)
        );
    }
}
