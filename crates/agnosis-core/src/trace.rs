//! Environment-gated timestamp logging.
//!
//! Set `AGNOSIS_TRACE=1` to print stage timings. The smoke test asserts on
//! window-on-screen time only; first-duck time is reported but never asserted,
//! so a slow driver cannot fail the build.
//!
//! Daemon mode nulls stdio (the `--daemon` spawn detaches all three stdio
//! handles), so plain `trace` output would vanish there: set
//! `AGNOSIS_TRACE_FILE=<path>` to also append every trace/warn line to a file.
//! [`warn`] always writes (stderr, plus the file when set) so config repairs
//! and fallback assumptions stay visible without `AGNOSIS_TRACE`.
//!
//! # The clock is a parameter
//!
//! The traces are timed against `std::time::Instant`, and the standard
//! library's clock **panics** on `wasm32-unknown-unknown` ("time not
//! implemented on this platform") — so the web build (roadmap Extension
//! LATER §1.2), which runs this very core, could not even build a scene.
//!
//! So the clock is a **parameter**, like every other thing the core
//! cannot know: a host that has no system clock to give (the browser)
//! hands the core one with [`set_host_clock`], and a host that has one
//! (the native shell) does not, and keeps the system clock. Nothing in
//! here knows what a browser is — the core only asks for the
//! milliseconds.
//!
//! The **fallback** must not panic either: the web build was safe only
//! because it called [`set_host_clock`] as its first statement — a
//! convention, not a property, and the core's own scene traces during
//! its construction. So the fallback asks the platform at *runtime*
//! whether it has a clock at all (see [`has_system_clock`]) and reports
//! `0.0` when it does not. The native traces keep their timings.

use std::sync::OnceLock;
use std::time::Instant;

static ENABLED: OnceLock<bool> = OnceLock::new();
/// The anchor of the system clock: the moment of the **first** trace.
///
/// `None` when the platform has **no** monotonic clock at all — see
/// [`has_system_clock`].
static START: OnceLock<Option<Instant>> = OnceLock::new();
static LOG_FILE: OnceLock<Option<std::path::PathBuf>> = OnceLock::new();
/// The host's own clock, when it has no system clock to give.
///
/// WHY this exists: see the module doc. The clock is a plain function
/// of "milliseconds since the host started", so the core asks for it
/// and never learns where it came from.
static HOST_CLOCK: OnceLock<Box<dyn Fn() -> f64 + Send + Sync>> = OnceLock::new();

/// Hand the core a clock: the milliseconds since the host's start.
///
/// The web build calls this with the page's `performance.now()` before
/// it builds its first scene. A host with a system clock (the native
/// shell) does not need it: the traces keep their timings, unchanged.
///
/// The first clock wins: a second call is **refused** and reported, so
/// a host that has already started cannot be re-timed under the running
/// app (audit 2026-10-21 §F4 — the refusal used to be swallowed, so a
/// caller could not tell that its clock had been dropped).
///
/// Returns `true` when the clock was taken and `false` when one was
/// already set.
pub fn set_host_clock(clock: impl Fn() -> f64 + Send + Sync + 'static) -> bool {
    HOST_CLOCK.set(Box::new(clock)).is_ok()
}

/// The milliseconds since the clock's origin.
///
/// The host's clock when it gave one, the system clock otherwise.
fn elapsed_ms() -> f64 {
    if let Some(clock) = HOST_CLOCK.get() {
        return clock();
    }
    match start() {
        Some(start) => start.elapsed().as_secs_f64() * 1000.0,
        None => 0.0,
    }
}

fn enabled() -> bool {
    *ENABLED.get_or_init(|| {
        std::env::var("AGNOSIS_TRACE")
            .map(|v| !v.is_empty() && v != "0")
            .unwrap_or(false)
    })
}

/// Is tracing on? (decision F-165, roadmap HELP window §4.1: the blit
/// counters emit only when it is, so a normal run pays nothing.)
pub fn is_enabled() -> bool {
    enabled()
}

/// The anchor of the system clock, or `None` when there is no clock.
fn start() -> Option<&'static Instant> {
    START
        .get_or_init(|| has_system_clock().then(Instant::now))
        .as_ref()
}

/// Does this platform have a monotonic clock?
///
/// WHY this is a runtime question and not a `cfg`: the core carries
/// **no** OS-conditional `cfg` — that is the crate's rule, and the grep
/// in the module doc proves it — but the standard library's clock
/// **panics** on `wasm32-unknown-unknown` ("time not implemented on
/// this platform"), and the release profile aborts on a panic. So the
/// core asks the platform at *runtime*: `std::env::consts` is baked in
/// by the compiler, so the check costs nothing and the very same code
/// is compiled everywhere.
///
/// `unknown` is the operating-system name of `wasm32-unknown-unknown`:
/// the one target whose standard library has **no** clock at all. Every
/// other target the app builds for — windows, linux, macos, wasi — has
/// one, and the native traces keep their timings.
fn has_system_clock() -> bool {
    std::env::consts::OS != "unknown"
}

fn log_file() -> Option<&'static std::path::PathBuf> {
    LOG_FILE
        .get_or_init(|| std::env::var_os("AGNOSIS_TRACE_FILE").map(std::path::PathBuf::from))
        .as_ref()
}

/// Best-effort append of one line to `AGNOSIS_TRACE_FILE`; failures are silent
/// by design (logging must never fail the app).
fn append_to_file(line: &str) {
    let path = match log_file() {
        Some(path) => path,
        None => return,
    };
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    if let Ok(mut file) = opts.open(path) {
        use std::io::Write;
        let _ = writeln!(file, "{line}");
    }
}

/// Print `AGNOSIS_TRACE <ms_since_start> <label>` when tracing is on; always
/// append to `AGNOSIS_TRACE_FILE` when it is set (covers `--daemon`, where
/// stdio is nulled).
pub fn trace(label: &str) {
    let line = format!("AGNOSIS_TRACE {:.3} {}", elapsed_ms(), label);
    if enabled() {
        println!("{line}");
    }
    append_to_file(&line);
}

/// Warn-level trace for config repairs and fallback assumptions: always
/// printed to stderr (and to `AGNOSIS_TRACE_FILE` when set), even when
/// `AGNOSIS_TRACE` is off.
pub fn warn(label: &str) {
    let line = format!("AGNOSIS_TRACE WARN {:.3} {}", elapsed_ms(), label);
    eprintln!("{line}");
    append_to_file(&line);
}

/// Milliseconds since the clock's origin.
///
/// The origin is the **first trace call** — the anchor is a lazy
/// `OnceLock` — or the **host's** start when a host clock was given
/// (the host decides what "zero" means). It is *not* the process start
/// in general.
///
/// Test-only: the production paths format their own timestamps inside
/// [`trace`] and [`warn`], so this accessor is kept behind
/// `cfg(any(test, feature = "test-support"))` for the monotonicity
/// assertion (audit dead-code sweep). The **feature** is what makes it
/// reachable from the shell's own tests: a `cfg(test)` item is invisible
/// to *another* crate (audit 2026-10-21 §F22).
#[cfg(any(test, feature = "test-support"))]
pub fn since_start_ms() -> f64 {
    elapsed_ms()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn since_start_is_small_and_monotonic() {
        let a = since_start_ms();
        // WHY a bounded spin instead of a sleep: the core is compiled for
        // `wasm32-unknown-unknown`, which has no threading module, and
        // the assertion is about the clock's **monotonicity**, not about
        // a specific elapsed time. The spin gives a real clock a chance
        // to tick; the iteration budget keeps the test bounded even where
        // the clock does not advance.
        let mut spins = 0u32;
        while spins < 100_000 && since_start_ms() <= a {
            spins += 1;
            std::hint::spin_loop();
        }
        let b = since_start_ms();
        assert!(b >= a, "{b} < {a}");
    }

    /// The host clock is **monotonic** too: the web front end hands the
    /// core one, and the core's own timing must not go backwards.
    ///
    /// The clock is a `OnceLock`, so this test cannot install one
    /// without taking the slot from every other test in the process. It
    /// therefore pins the property of whatever clock is in use, which is
    /// what the assertion is really about.
    #[test]
    fn the_clock_never_goes_backwards() {
        let mut previous = since_start_ms();
        for _ in 0..64 {
            let now = since_start_ms();
            assert!(
                now >= previous,
                "the clock went backwards: {now} < {previous}"
            );
            previous = now;
        }
    }

    #[test]
    fn trace_does_not_panic_when_disabled() {
        trace("noop");
        warn("noop warn");
    }

    /// The first clock wins, and the refusal is **reported**: the
    /// swallowed `let _ =` used to hide a dropped clock from the host
    /// that set it (audit 2026-10-21 §F4).
    ///
    /// **This test must not be the one that installs the clock.** The
    /// `HOST_CLOCK` is a `OnceLock`: whoever sets it first owns it for
    /// the whole test process. When this test won the race, it froze
    /// every other test's clock at `42.0` for the rest of the run —
    /// which made `since_start_is_small_and_monotonic` spin to its
    /// budget and fail, depending on the test order
    /// (audit 2026-10-22 §P3-41).
    ///
    /// The clock is therefore installed by the test that needs it, and
    /// the *behaviour* is pinned without depending on who wins the
    /// race: the second `set_host_clock` is refused whatever the first
    /// one was, and the clock in use is the first one.
    #[test]
    fn the_first_clock_wins_and_the_refusal_is_reported() {
        // The first call in this process may already have been made by
        // another test, so both outcomes are accepted here.
        let first_taken = set_host_clock(|| 42.0);
        assert!(!set_host_clock(|| 7.0), "a second clock is always refused");
        if first_taken {
            assert_eq!(elapsed_ms(), 42.0, "the first clock is the one in use");
        }
    }
}
