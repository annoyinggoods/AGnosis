//! The core's purity invariants, pinned as **tests**.
//!
//! The crate's whole reason to exist is the rule the module doc of
//! `lib.rs` states: **the core is platform-free**. A convention that
//! only lives in a comment leaks eventually — it did once, with
//! `stats::today()` reading `SystemTime::now` inside the core
//! (audit 2026-10-22 §P1-7). These tests make the rule mechanical:
//! a violation fails the build instead of being noticed in review.
//!
//! The greps run over the crate's own source, so they must not see
//! their own patterns. The patterns are therefore **assembled from
//! pieces**, so this file never contains the literal text it is looking
//! for.
//!
//! **Test code is exempt.** A `#[cfg(test)]` module may use a clock —
//! it only runs on the host, never in the browser build. The rule the
//! tests enforce is about the **shipped** code, so the reader tracks
//! the test modules and skips their lines.
//!
//! The scans themselves follow the nesting rule like any other code:
//! one helper per decision, iterator chains instead of a loop inside a
//! loop (audit 2026-10-23 §F-050).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The attribute that opens a test module.
const TEST_ATTRIBUTE: &str = "#[cfg(test)]";

/// The root of this crate.
fn crate_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_path_buf()
}

/// The crate's `lib.rs`, read **once** for the whole test run.
///
/// The test-only-file check used to re-read it for every source file
/// (audit 2026-10-23 §F-050); the text cannot change under us, so it is
/// cached.
fn lib_rs() -> &'static str {
    static LIB: OnceLock<String> = OnceLock::new();
    LIB.get_or_init(|| std::fs::read_to_string(crate_root().join("src/lib.rs")).unwrap_or_default())
}

/// Every `.rs` file of this crate's `src`, except this one.
fn source_files() -> Vec<PathBuf> {
    let src = crate_root().join("src");
    let mut files = Vec::new();
    collect_rs(&src, &mut files);
    files.retain(|f| f.file_name().is_some_and(|n| n != "purity.rs"));
    files.sort();
    files
}

/// Walk a folder and collect the `.rs` files in it.
fn collect_rs(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Is this file a **whole-file** test module?
///
/// `audit_proto.rs` and `export/tests.rs` are test-only: the crate
/// declares them behind `#[cfg(test)]`, so they never reach the
/// browser build.
fn is_test_only_file(path: &Path) -> bool {
    let Some(name) = path.file_stem().map(|n| n.to_string_lossy().to_string()) else {
        return false;
    };
    if name == "tests" {
        return true;
    }
    declares_test_only(&name)
}

/// Does `lib.rs` declare `mod <name>;` behind the test attribute?
///
/// The attribute and the declaration may sit on separate lines, so the
/// scan walks the whole text once, flat: every line that carries the
/// attribute is followed by its first non-empty line.
fn declares_test_only(name: &str) -> bool {
    let declaration = format!("mod {name};");
    let mut lines = lib_rs().lines().map(str::trim);
    while let Some(line) = lines.next() {
        if !line.starts_with(TEST_ATTRIBUTE) {
            continue;
        }
        let declares = lines
            .find(|l| !l.is_empty())
            .is_some_and(|l| l.contains(&declaration));
        if declares {
            return true;
        }
    }
    false
}

/// One line of source, with where it came from.
struct Line {
    file: PathBuf,
    number: usize,
    text: String,
    /// True when the line sits inside a `#[cfg(test)]` module.
    in_test: bool,
    /// True when the whole file is a test-only module.
    file_is_test_only: bool,
}

/// The lines of every source file, with the file they came from.
///
/// The per-file walk is a helper of its own, so the collection itself is
/// an iterator chain (audit 2026-10-23 §F-050, nesting rule).
fn all_lines() -> Vec<Line> {
    source_files().into_iter().flat_map(lines_of).collect()
}

/// The lines of one file.
///
/// The `in_test` flag is true from a `#[cfg(test)]` attribute to the
/// end of the file: a test module is always the last thing in a file
/// in this crate.
fn lines_of(file: PathBuf) -> Vec<Line> {
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Vec::new();
    };
    let file_is_test_only = is_test_only_file(&file);
    let mut in_test = false;
    text.lines()
        .enumerate()
        .map(|(index, line)| {
            in_test |= line.trim().starts_with(TEST_ATTRIBUTE);
            Line {
                file: file.clone(),
                number: index + 1,
                text: line.to_string(),
                in_test,
                file_is_test_only,
            }
        })
        .collect()
}

/// The lines of the **shipped** code: the test modules are skipped.
fn shipped_lines() -> Vec<Line> {
    all_lines()
        .into_iter()
        .filter(|l| !l.in_test)
        .filter(|l| !l.file_is_test_only)
        // A doc comment **names** the API; it does not call it. The
        // editor's doc comments explain why the clock is injected, and
        // that text must not trip the grep.
        .filter(|l| !l.text.trim_start().starts_with("///"))
        .filter(|l| !l.text.trim_start().starts_with("//"))
        .collect()
}

/// The offending lines among `lines`, as `file:line: text` strings.
///
/// One helper for every scan (audit 2026-10-23 §F-050): the loop-in-a-
/// loop the tests used to carry is now this chain, and a hit is decided
/// by the injected `hits` predicate while the injected `exempt` decides
/// which lines the rule does not reach.
fn offenders_of(
    lines: Vec<Line>,
    hits: impl Fn(&str) -> bool,
    exempt: fn(&Line) -> bool,
) -> Vec<String> {
    lines
        .into_iter()
        .filter(|line| !exempt(line))
        .filter(|line| hits(&line.text))
        .map(|line| {
            format!(
                "{}:{}: {}",
                line.file.display(),
                line.number,
                line.text.trim()
            )
        })
        .collect()
}

/// Does the text carry any of the assembled `patterns`?
fn carries_any(text: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| text.contains(p.as_str()))
}

/// Nothing is exempt.
fn never(_line: &Line) -> bool {
    false
}

/// The one **guarded** clock site in `trace.rs`.
///
/// The exemption used to cover the whole file (audit 2026-10-23 §F-050);
/// it is narrowed to the line that names its own guard — the file's
/// `Instant::now` sits behind `has_system_clock().then(…)`. A second,
/// unguarded `now` anywhere — trace.rs included — fails the gate.
fn is_guarded_trace_clock(line: &Line) -> bool {
    let in_trace = line.file.file_name().is_some_and(|n| n == "trace.rs");
    in_trace && line.text.contains("has_system_clock")
}

/// The clock-reading patterns, assembled so this file never carries the
/// literals it hunts.
fn clock_patterns() -> [String; 3] {
    [
        String::from("Instant") + "::now",
        String::from("SystemTime") + "::now",
        String::from("std::time::Instant") + "::now",
    ]
}

/// The core carries **no** OS-conditional `cfg`.
///
/// WHY: the crate is compiled for `wasm32-unknown-unknown`, where the
/// platform tree does not exist. A `cfg(target_os = …)` in the core
/// would silently exclude a code path from the browser build.
///
/// The matcher is **whitespace-tolerant** since audit 2026-10-23 §F-050:
/// `cfg ( target_os = …)` spaces its brackets exactly like `cfg(…)` does,
/// and a pattern that missed the spaced spelling was a hole in the gate.
#[test]
fn the_core_has_no_os_conditional_cfg() {
    let offenders = offenders_of(shipped_lines(), mentions_target_cfg, never);
    assert!(
        offenders.is_empty(),
        "the core must carry no OS-conditional cfg:\n{}",
        offenders.join("\n")
    );
}

/// Does the text name an OS-conditional `cfg`?
///
/// Matches `cfg` followed — with whitespace anywhere in between — by
/// `(` and then `target`: the `cfg\s*\(\s*target` of the crate's grep,
/// spelled as a function so the spaced forms cannot slip through. The
/// pieces are assembled at runtime, so this file does not carry the
/// literal it hunts.
fn mentions_target_cfg(text: &str) -> bool {
    let cfg = String::from("c") + "fg";
    let target = String::from("tar") + "get";
    let mut rest = text;
    while let Some(at) = rest.find(cfg.as_str()) {
        let after = &rest[at + cfg.len()..];
        if paren_then(after, &target) {
            return true;
        }
        rest = after;
    }
    false
}

/// Does `text` start with `(` — whitespace allowed around it — followed
/// by `name`?
fn paren_then(text: &str, name: &str) -> bool {
    let Some(after_paren) = text.trim_start().strip_prefix('(') else {
        return false;
    };
    after_paren.trim_start().starts_with(name)
}

/// The core reads **no** wall clock of its own.
///
/// WHY: `Instant::now` and `SystemTime::now` **panic** on
/// `wasm32-unknown-unknown` ("time not implemented on this platform"),
/// and the release profile aborts on a panic. The clock belongs to the
/// host; the core takes the time as data.
///
/// The one exception is named: the **guarded** clock site in `trace.rs`
/// (see [`is_guarded_trace_clock`]).
#[test]
fn the_core_reads_no_wall_clock() {
    let patterns = clock_patterns();
    let offenders = offenders_of(
        shipped_lines(),
        |text| carries_any(text, &patterns),
        is_guarded_trace_clock,
    );
    assert!(
        offenders.is_empty(),
        "the core must not read a wall clock:\n{}",
        offenders.join("\n")
    );
}

/// The core spawns **no** thread and owns **no** channel.
///
/// WHY: a thread in the core would make the browser build impossible —
/// `wasm32-unknown-unknown` has no threads — and it would put the
/// platform's scheduling into the pure half.
#[test]
fn the_core_spawns_no_thread() {
    let patterns = [
        String::from("std::thread") + "::spawn",
        String::from("thread::spawn"),
        String::from("std::sync::mpsc"),
    ];
    let offenders = offenders_of(shipped_lines(), |text| carries_any(text, &patterns), never);
    assert!(
        offenders.is_empty(),
        "the core must spawn no thread and own no channel:\n{}",
        offenders.join("\n")
    );
}

/// The core **sleeps** and **locks** nothing.
///
/// WHY: a `sleep` or a `park` blocks its caller — on the browser build
/// there is no other thread to run while it waits, so the page freezes;
/// and a `Mutex`/`RwLock`/`Condvar`/`Barrier` is a thread-synchronization
/// primitive the thread-free core has no use for (`OnceLock` is the one
/// lazy static that needs no locking, and it stays legal). The patterns
/// were missing from the gate until audit 2026-10-23 §F-050.
#[test]
fn the_core_sleeps_and_locks_nothing() {
    let patterns = [
        String::from("std::thread") + "::sleep",
        String::from("std::thread") + "::park",
        String::from("thread::sleep"),
        String::from("thread::park"),
        String::from("std::sync") + "::Mutex",
        String::from("std::sync") + "::RwLock",
        String::from("std::sync") + "::Condvar",
        String::from("std::sync") + "::Barrier",
    ];
    let offenders = offenders_of(shipped_lines(), |text| carries_any(text, &patterns), never);
    assert!(
        offenders.is_empty(),
        "the core must not sleep or lock:\n{}",
        offenders.join("\n")
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whitespace-tolerant matcher sees the spaced spellings the old
    /// literal pattern missed (audit 2026-10-23 §F-050).
    #[test]
    fn the_cfg_matcher_is_whitespace_tolerant() {
        assert!(mentions_target_cfg("#[cfg(target_os = \"windows\")]"));
        assert!(mentions_target_cfg("#[cfg ( target_os = \"windows\")]"));
        assert!(mentions_target_cfg("#[cfg(\n    target_os = \"windows\")]"));
        assert!(!mentions_target_cfg(
            "#[cfg(any(test, feature = \"test-support\"))]"
        ));
        assert!(!mentions_target_cfg("// a comment about configuration"));
    }

    /// The exemption reaches exactly the guarded site, not the file.
    #[test]
    fn the_clock_exemption_names_the_guard() {
        let guarded = Line {
            file: PathBuf::from("trace.rs"),
            number: 92,
            text: ".get_or_init(|| has_system_clock().then(Instant::now))".to_string(),
            in_test: false,
            file_is_test_only: false,
        };
        assert!(is_guarded_trace_clock(&guarded));
        let bare = Line {
            text: "let t = Instant::now();".to_string(),
            ..guarded
        };
        assert!(!is_guarded_trace_clock(&bare), "an unguarded now fails");
        let elsewhere = Line {
            file: PathBuf::from("scene.rs"),
            text: guarded.text.clone(),
            ..bare
        };
        assert!(
            !is_guarded_trace_clock(&elsewhere),
            "only trace.rs is asked"
        );
    }
}
