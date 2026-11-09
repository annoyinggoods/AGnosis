//! Balloon folder watcher: turns dropped-in files into balloon sprites.
//!
//! A `notify` 8.x [`notify::RecommendedWatcher`] feeds an mpsc channel. A
//! dedicated **worker thread** owns that channel, the poll scan, the settle
//! waits and the PNG/JPEG decode, and pushes finished sprites back over a
//! second channel. The UI thread only ever *polls* that result channel
//! ([`BalloonWatcher::drain`]): it never sleeps, never decodes an image and
//! never walks the directory tree (audit P0-1 — the settle used to sleep
//! 3x50 ms and decode on the thread that runs `tick`).
//!
//! The worker is joined when the watcher is dropped, so no thread outlives
//! the popup.
//!
//! A `<stem>.sprite.json` sibling wins over re-conversion only when its
//! radii stamp matches the live geometry; unstamped or stale caches are
//! treated as misses and reconverted (audit P0-3). The watcher **writes**
//! that cache itself after every successful conversion, exactly like the
//! `agnosis add` CLI does (`cli.rs` `cache_sprite_in`): the cache is the
//! durable record of the art that is in the balloon ring, so a geometry
//! change keeps the `done` entry instead of re-converting the file and
//! pushing a **second** copy of the same art onto another duck (audit
//! §3.8). The write happens on the worker thread, never on the UI thread.
//!
//! Unreadable or corrupt files are skipped with a single
//! [`agnosis_core::trace::trace`] per failure episode, never fatal, and the failure
//! backs off exponentially instead of re-queuing every frame. Dotfiles,
//! editor temp files and `.sprite.json` caches are never treated as balloon
//! content — the cache the watcher writes lands in the watched folder, and
//! [`crate::balloon_resolve::is_balloon_source`] excludes it by its
//! extension. Directory scans skip symlinks and stop at
//! [`crate::balloon_resolve::MAX_SCAN_DEPTH`] so a symlink loop cannot hang
//! the worker (audit robustness).
//!
//! The resolve + cache rules live in [`crate::balloon_resolve`] so the typed
//! name path in the popup and this watcher cannot drift (audit P1-13).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use notify::Watcher as _;

/// Settle delay per scan round: three 50 ms stability reads, taken in
/// **one shared window** for the whole pending batch.
const SETTLE_ROUNDS: u32 = 3;
const SETTLE_STEP_MS: u64 = 50;

/// Minimum gap between recursive poll scans: the worker scans at most once
/// per [`SCAN_INTERVAL`] (audit P0-1 / performance).
const SCAN_INTERVAL: Duration = Duration::from_millis(1000);
/// Failure backoff: first retry after 5 s, doubling to a 5 min ceiling.
const BACKOFF_FIRST: Duration = Duration::from_secs(5);
const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// How long the worker blocks waiting for work before it re-checks the stop
/// flag and the command queue. Keeps shutdown latency bounded without
/// spinning.
const WORKER_POLL: Duration = Duration::from_millis(25);

/// Watcher failures. Conversion failures never surface here: bad files are
/// skipped inside the worker with a trace instead.
#[derive(Debug, thiserror::Error)]
pub enum WatcherError {
    /// The platform watcher could not start or watch the directory.
    #[error("notify error: {0}")]
    Notify(#[from] notify::Error),
    /// The watched directory could not be created.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// A message from the UI thread to the worker.
enum Command {
    /// New conversion radii (audit P0-4).
    SetRadii(i64, i64),
    /// Test-only: answer once every earlier command has been applied.
    #[cfg(test)]
    Sync(std::sync::mpsc::Sender<()>),
}

/// Shared control block: stop flag, command queue and a wakeup.
struct Control {
    inner: Mutex<ControlInner>,
    wake: Condvar,
}

struct ControlInner {
    stopped: bool,
    commands: VecDeque<Command>,
}

impl Control {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(ControlInner {
                stopped: false,
                commands: VecDeque::new(),
            }),
            wake: Condvar::new(),
        })
    }

    /// Queue a command and wake the worker.
    fn push(&self, command: Command) {
        let mut inner = self.lock();
        inner.commands.push_back(command);
        self.wake.notify_one();
    }

    /// Ask the worker to stop and wake it so it notices immediately.
    fn shutdown(&self) {
        let mut inner = self.lock();
        inner.stopped = true;
        self.wake.notify_one();
    }

    /// True once [`Control::shutdown`] was called.
    fn is_stopped(&self) -> bool {
        self.lock().stopped
    }

    /// Wait for a command (or the poll timeout) and take the whole queue.
    fn wait(&self, timeout: Duration) -> Vec<Command> {
        let inner = self.lock();
        let (mut inner, _) = self
            .wake
            .wait_timeout(inner, timeout)
            .unwrap_or_else(|e| e.into_inner());
        inner.commands.drain(..).collect()
    }

    /// Lock without poisoning the worker: a panicking thread must not take
    /// the watcher down with it.
    fn lock(&self) -> std::sync::MutexGuard<'_, ControlInner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// What the worker finished: a sprite for the ring, or a skip the user
/// sees on the input line.
///
/// The watcher used to be **silent** — a dropped image either appeared
/// without a word or failed with nothing but a trace — while the typed
/// name path reported everything. The outcome carries the name so the
/// popup can say the same things on both paths
/// (audit 2026-10-23 §F-104).
#[derive(Debug)]
pub enum WatchOutcome {
    /// A converted sprite, with the file it came from.
    Added {
        /// The source file's stem, for the status line.
        name: String,
        /// The sprite, ready for `Scene::push_balloon`.
        sprite: agnosis_core::balloon::AsciiSprite,
    },
    /// A file that could not become a balloon, and why.
    Skipped {
        /// The source file's name, for the status line.
        file: String,
        /// The reason the file was refused.
        reason: String,
    },
}

/// The worker's own label for a file, or the whole path when it has no
/// file name.
fn file_label(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    name
}

/// Live balloon folder watcher.
///
/// The watcher owns the `notify` handle (so events keep flowing) and the
/// worker thread that does the work; dropping it stops delivery and joins
/// the worker.
pub struct BalloonWatcher {
    rx: i64,
    ry: i64,
    _watcher: notify::RecommendedWatcher,
    /// Worker thread; joined on drop.
    worker: Option<std::thread::JoinHandle<()>>,
    /// Stop flag + command queue for the worker.
    control: Arc<Control>,
    /// Finished outcomes from the worker.
    results: std::sync::mpsc::Receiver<WatchOutcome>,
}

impl BalloonWatcher {
    /// Watch `dir` (recursive, creates it if missing). `rx`/`ry` come from
    /// the live scene geometry. Files already present are picked up by the
    /// worker's first scan.
    pub fn new(dir: &Path, rx: i64, ry: i64) -> Result<Self, WatcherError> {
        std::fs::create_dir_all(dir)?;
        let (tx, events) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(tx)?;
        watcher.watch(dir, notify::RecursiveMode::Recursive)?;
        let control = Control::new();
        let (results_tx, results) = std::sync::mpsc::channel();
        let worker_control = Arc::clone(&control);
        let dir = dir.to_path_buf();
        let worker = std::thread::Builder::new()
            .name("ducks-balloon-watch".to_string())
            .spawn(move || {
                let state = WorkerState::new(dir, rx, ry, events);
                worker_main(state, worker_control, results_tx);
            })?;
        Ok(Self {
            rx,
            ry,
            _watcher: watcher,
            worker: Some(worker),
            control,
            results,
        })
    }

    /// Non-blocking drain: the outcomes the worker finished since the
    /// last call — sprites ready for `Scene::push_balloon`, and the
    /// skips the input line reports (audit 2026-10-23 §F-104).
    ///
    /// The per-frame cost is one channel poll and nothing else: no sleep, no
    /// image decode, no directory walk (audit P0-1). Failed paths back off
    /// inside the worker instead of retrying every frame, and each failure
    /// episode traces once.
    pub fn drain(&mut self) -> Vec<WatchOutcome> {
        let mut out = Vec::new();
        while let Ok(outcome) = self.results.try_recv() {
            out.push(outcome);
        }
        out
    }

    /// Update the scene geometry used for subsequent conversions (audit P0-4).
    ///
    /// The worker applies the new radii on its next turn. The art that is
    /// already in the balloon ring is **not** converted and pushed again:
    /// `Scene::resize` keeps the ring, so a second push of the same source
    /// would leave the same art on two ducks (audit §3.8). The worker keeps
    /// the `done` entry of every file whose sibling cache still holds the
    /// art that is in the ring, and only the files converted from now on
    /// use the new radii.
    pub fn set_radii(&mut self, rx: i64, ry: i64) {
        if (rx, ry) == (self.rx, self.ry) {
            return;
        }
        self.rx = rx;
        self.ry = ry;
        self.control.push(Command::SetRadii(rx, ry));
    }

    /// Block until the worker has applied every command sent so far.
    ///
    /// Test seam: the worker is asynchronous, so a test that changes the
    /// radii and then expects the new geometry needs an explicit
    /// completion signal instead of a sleep.
    #[cfg(test)]
    fn sync(&mut self) {
        let (tx, rx) = std::sync::mpsc::channel();
        self.control.push(Command::Sync(tx));
        let _ = rx.recv();
    }
}

impl Drop for BalloonWatcher {
    /// Stop the worker and join it: no thread may outlive the popup
    /// (audit P0-1).
    fn drop(&mut self) {
        self.control.shutdown();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// What the worker remembers about a file it converted.
///
/// The entry is the record "the art of this file is in the balloon ring",
/// so it carries the geometry the art was built for: the sibling
/// `.sprite.json` cache is stamped with the same radii, and a geometry
/// change compares the cache against **these** radii — not against the
/// new ones — so the art that is already on a duck is never pushed a
/// second time (audit §3.8).
struct Converted {
    /// The file's modification time when it was converted.
    mtime: SystemTime,
    /// The radii the art in the ring was built for.
    radii: (i64, i64),
}

/// The worker's own state: the event channel, the candidate set and the
/// bookkeeping maps. Lives entirely on the worker thread.
struct WorkerState {
    dir: PathBuf,
    rx: i64,
    ry: i64,
    events: std::sync::mpsc::Receiver<Result<notify::Event, notify::Error>>,
    pending: HashSet<PathBuf>,
    done: HashMap<PathBuf, Converted>,
    /// Consecutive conversion failures per path (drives backoff).
    failures: HashMap<PathBuf, u32>,
    /// When a failed path may be retried.
    retry_at: HashMap<PathBuf, Instant>,
    /// Last poll scan; scans run at most once per [`SCAN_INTERVAL`].
    last_scan: Instant,
}

impl WorkerState {
    fn new(
        dir: PathBuf,
        rx: i64,
        ry: i64,
        events: std::sync::mpsc::Receiver<Result<notify::Event, notify::Error>>,
    ) -> Self {
        Self {
            dir,
            rx,
            ry,
            events,
            pending: HashSet::new(),
            done: HashMap::new(),
            failures: HashMap::new(),
            retry_at: HashMap::new(),
            // The first scan runs immediately so startup files are picked up.
            last_scan: Instant::now()
                .checked_sub(SCAN_INTERVAL)
                .unwrap_or_else(Instant::now),
        }
    }

    /// Apply the commands the UI thread queued.
    fn apply_commands(&mut self, commands: Vec<Command>) {
        for command in commands {
            match command {
                Command::SetRadii(rx, ry) => self.set_radii(rx, ry),
                #[cfg(test)]
                Command::Sync(reply) => {
                    let _ = reply.send(());
                }
            }
        }
    }

    /// New conversion radii: the files converted from now on use them
    /// (audit P0-4).
    ///
    /// The art that is already in the balloon ring stays there. A
    /// `done` entry is therefore kept when the sibling cache still holds
    /// the art that is in the ring — the watcher writes that cache after
    /// every conversion, so the stamp matches the radii the art was built
    /// for. Forgetting the entry would re-convert the file and push a
    /// **second** copy of the same art into the ring, and `Scene::resize`
    /// keeps the ring, so the old copy would stay (audit §3.8).
    ///
    /// Only an entry whose cache is gone (a failed write, or a file from a
    /// build that never wrote the cache) is forgotten; that file is
    /// derived again at the new radii and the cache is written then.
    fn set_radii(&mut self, rx: i64, ry: i64) {
        self.rx = rx;
        self.ry = ry;
        self.done
            .retain(|path, done| crate::balloon_resolve::cache_stamp_current(path, done.radii));
    }

    /// Drain the `notify` channel into `pending`, coalescing repeat events.
    fn drain_events(&mut self) {
        while let Ok(res) = self.events.try_recv() {
            self.queue_event(res);
        }
    }

    /// Queue one watcher result: event paths merge into `pending`, channel
    /// errors trace.
    fn queue_event(&mut self, res: Result<notify::Event, notify::Error>) {
        match res {
            Ok(event) => {
                self.pending.extend(event.paths);
            }
            Err(e) => {
                agnosis_core::trace::trace(&format!("balloon watch error: {e}"));
            }
        }
    }

    /// Run the throttled poll scan when it is due.
    fn scan_if_due(&mut self) {
        if self.last_scan.elapsed() < SCAN_INTERVAL {
            return;
        }
        self.last_scan = Instant::now();
        self.scan();
    }

    /// Poll scan of the balloon tree. Skips symlinks, stops at
    /// [`crate::balloon_resolve::MAX_SCAN_DEPTH`], and queues only sources
    /// whose mtime differs from `done`.
    fn scan(&mut self) {
        let mut stack = vec![(self.dir.clone(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            if depth >= crate::balloon_resolve::MAX_SCAN_DEPTH {
                continue;
            }
            let (sources, subdirs) = crate::balloon_resolve::entries_in(&dir);
            self.queue_sources(&sources);
            stack.extend(subdirs.into_iter().map(|path| (path, depth + 1)));
        }
        self.prune_missing();
    }

    /// Queue the fresh sources of one directory level.
    fn queue_sources(&mut self, sources: &[PathBuf]) {
        for source in sources {
            self.visit_entry(source);
        }
    }

    /// Queue a fresh source: one whose mtime differs from `done`.
    fn visit_entry(&mut self, path: &Path) {
        let fresh = modified(path);
        let known = self.done.get(path).map(|done| done.mtime);
        if known != fresh {
            self.pending.insert(path.to_path_buf());
        }
    }

    /// Drop bookkeeping for files that no longer exist, so the maps cannot
    /// grow without bound while a folder is watched for a long time.
    fn prune_missing(&mut self) {
        self.done.retain(|path, _| path.is_file());
        self.failures.retain(|path, _| path.is_file());
        self.retry_at.retain(|path, _| path.is_file());
    }

    /// Convert every pending path and hand the finished outcomes to the
    /// UI thread.
    ///
    /// The pending set is a hash set, so its iteration order is random:
    /// the paths are sorted, so the same folder always fills the ring
    /// slots in the same order and a run is reproducible. The settle
    /// window is shared by the whole batch (audit 2026-10-23 §F-115).
    fn process_pending(&mut self, control: &Control, out: &std::sync::mpsc::Sender<WatchOutcome>) {
        let mut paths: Vec<PathBuf> = std::mem::take(&mut self.pending).into_iter().collect();
        paths.sort();
        let marks = settle_batch(&paths, control);
        for (path, mark) in paths.into_iter().zip(marks) {
            self.convert_settled(&path, mark, out);
        }
    }

    /// One settled path: freshness, backoff, cache, convert.
    fn convert_settled(
        &mut self,
        path: &Path,
        mark: Settle,
        out: &std::sync::mpsc::Sender<WatchOutcome>,
    ) {
        if !crate::balloon_resolve::is_balloon_source(path) {
            return;
        }
        match mark {
            Settle::Gone => self.forget(path),
            Settle::Unstable => {
                self.pending.insert(path.to_path_buf());
            }
            Settle::Ready => self.convert_if_fresh(path, out),
        }
    }

    /// Freshness + cache + convert for one settled path. Failed paths
    /// back off; vanished paths are forgotten.
    fn convert_if_fresh(&mut self, path: &Path, out: &std::sync::mpsc::Sender<WatchOutcome>) {
        let Some(mtime) = modified(path) else {
            self.forget(path);
            return;
        };
        if self.done.get(path).is_some_and(|done| done.mtime == mtime) {
            return;
        }
        if self.backed_off(path) {
            self.pending.insert(path.to_path_buf());
            return;
        }
        self.convert_and_send(path, mtime, out);
    }

    /// Convert one settled path and publish the outcome, or record the
    /// failure with a backoff.
    fn convert_and_send(
        &mut self,
        path: &Path,
        mtime: SystemTime,
        out: &std::sync::mpsc::Sender<WatchOutcome>,
    ) {
        let radii = (self.rx, self.ry);
        let loaded = match load_or_convert(path, radii) {
            Ok(loaded) => loaded,
            Err(reason) => {
                self.record_failure(path);
                let _ = out.send(WatchOutcome::Skipped {
                    file: file_label(path),
                    reason,
                });
                return;
            }
        };
        let sprite = cache_and_sprite(path, loaded, radii);
        self.done
            .insert(path.to_path_buf(), Converted { mtime, radii });
        self.failures.remove(path);
        self.retry_at.remove(path);
        let _ = out.send(WatchOutcome::Added {
            name: file_label(path),
            sprite,
        });
    }

    /// True when the path failed recently and its backoff has not expired.
    /// Expired entries are cleared so the retry proceeds.
    fn backed_off(&mut self, path: &Path) -> bool {
        let Some(deadline) = self.retry_at.get(path).copied() else {
            return false;
        };
        if Instant::now() >= deadline {
            self.retry_at.remove(path);
            return false;
        }
        true
    }

    /// Record a conversion failure: bump the streak, arm the backoff window,
    /// keep the path queued for its retry, and trace once per episode.
    fn record_failure(&mut self, path: &Path) {
        let streak = self
            .failures
            .get(path)
            .copied()
            .unwrap_or(0)
            .saturating_add(1);
        self.failures.insert(path.to_path_buf(), streak);
        let shift = streak.saturating_sub(1).min(6);
        let wait = (BACKOFF_FIRST * (1 << shift)).min(BACKOFF_MAX);
        self.retry_at
            .insert(path.to_path_buf(), Instant::now() + wait);
        self.pending.insert(path.to_path_buf());
        if streak == 1 {
            agnosis_core::trace::trace(&format!(
                "balloon {} failed, retrying with backoff",
                path.display()
            ));
        }
    }

    /// Forget a path that vanished: no entry may outlive its file, or the
    /// maps would grow without bound.
    fn forget(&mut self, path: &Path) {
        self.done.remove(path);
        self.failures.remove(path);
        self.retry_at.remove(path);
    }
}

/// Where a balloon sprite came from.
enum Loaded {
    /// The sibling `.sprite.json` cache already carried these radii, so
    /// the art is reused as it is and the cache is left alone.
    Cached(agnosis_core::balloon::AsciiSprite),
    /// The source was converted at these radii, so the sibling cache
    /// still has to be written.
    Converted(agnosis_core::balloon::AsciiSprite),
}

/// Load the radii-stamped cache of `path`, else convert the source at the
/// same radii. The error is the reason the file was refused — the caller
/// reports it on the input line (audit 2026-10-23 §F-104).
fn load_or_convert(path: &Path, radii: (i64, i64)) -> Result<Loaded, String> {
    if let Some(sprite) = crate::balloon_resolve::load_stamped_cache(path, radii) {
        return Ok(Loaded::Cached(sprite));
    }
    crate::balloon_resolve::convert_source(path, radii.0, radii.1)
        .map(Loaded::Converted)
        .map_err(|e| e.to_string())
}

/// The art of a conversion, with the sibling cache written for it.
///
/// The cache is written for a fresh conversion only: art that came from
/// the cache already has one, stamped with the same radii.
fn cache_and_sprite(
    path: &Path,
    loaded: Loaded,
    radii: (i64, i64),
) -> agnosis_core::balloon::AsciiSprite {
    match loaded {
        Loaded::Cached(sprite) => sprite,
        Loaded::Converted(sprite) => {
            write_sibling_cache(path, &sprite, radii);
            sprite
        }
    }
}

/// Write the `<stem>.sprite.json` sibling of a converted source, stamped
/// with the radii the art was built for (audit §3.8).
///
/// The cache is the durable record of the art that is in the balloon ring:
/// `set_radii` keeps the `done` entry of a file only while the cache
/// still holds that art, so a geometry change does not re-convert the
/// file and push a second copy of it onto another duck.
///
/// The write happens **only** after a successful conversion and **only**
/// with the radii the art was built for: a failed conversion must not
/// leave a cache behind, and a cache must never claim a geometry its art
/// does not have (a reader would reuse the wrong resolution).
///
/// The write runs on the worker thread — the UI thread only polls the
/// result channel — so it never blocks a frame (audit P0-1). A failed
/// write is traced and ignored: the art is good, and the file is derived
/// again at the next geometry change.
fn write_sibling_cache(
    path: &Path,
    sprite: &agnosis_core::balloon::AsciiSprite,
    radii: (i64, i64),
) {
    let Some(sibling) = crate::balloon_resolve::sibling_sprite(path) else {
        return;
    };
    if let Err(e) =
        agnosis_core::image_ascii::save_sprite_stamped(sprite, &sibling, radii.0, radii.1)
    {
        agnosis_core::trace::trace(&format!(
            "balloon cache {} not written: {e}",
            sibling.display()
        ));
    }
}

/// The worker loop: apply commands, drain events, scan when due, convert.
fn worker_main(
    mut state: WorkerState,
    control: Arc<Control>,
    results: std::sync::mpsc::Sender<WatchOutcome>,
) {
    while !control.is_stopped() {
        let commands = control.wait(WORKER_POLL);
        if control.is_stopped() {
            return;
        }
        state.apply_commands(commands);
        state.drain_events();
        state.scan_if_due();
        state.process_pending(&control, &results);
    }
}

/// Outcome of the settle window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Settle {
    /// The file is present and its size did not change across the window.
    Ready,
    /// The file vanished (or became unreadable).
    Gone,
    /// The file is still being written: its size changed.
    Unstable,
}

/// Settle the **whole batch** in one stability window
/// (audit 2026-10-23 §F-115).
///
/// The old settle slept 3 × 50 ms **per path, serially**, so a batch of
/// N fresh files cost N × 150 ms of worker time and `Drop` waited the
/// whole batch. Every path is now read once per round and the window is
/// shared: a batch costs the same 150 ms as a single file.
///
/// A stop request **breaks the window**: everything comes back
/// `Unstable` (re-queued), so a watcher being dropped never waits for
/// the batch. Sleeps happen on the *worker* thread by design — the UI
/// thread never waits (audit P0-1).
fn settle_batch(paths: &[PathBuf], control: &Control) -> Vec<Settle> {
    let first: Vec<Option<u64>> = paths.iter().map(|p| file_size(p)).collect();
    let mut last = first.clone();
    for _ in 0..SETTLE_ROUNDS {
        if control.is_stopped() {
            return unstable_all(paths.len());
        }
        std::thread::sleep(Duration::from_millis(SETTLE_STEP_MS));
        last = paths.iter().map(|p| file_size(p)).collect();
    }
    first
        .iter()
        .zip(&last)
        .map(|(a, b)| settle_of(*a, *b))
        .collect()
}

/// The whole batch re-queued: used when a stop cuts the window short.
fn unstable_all(len: usize) -> Vec<Settle> {
    vec![Settle::Unstable; len]
}

/// One path's settle verdict from its first and last size.
fn settle_of(first: Option<u64>, last: Option<u64>) -> Settle {
    match (first, last) {
        (None, _) | (_, None) => Settle::Gone,
        (Some(a), Some(b)) if a == b => Settle::Ready,
        _ => Settle::Unstable,
    }
}

/// The file's current size, `None` when it vanished or cannot be read.
fn file_size(path: &Path) -> Option<u64> {
    std::fs::metadata(path).ok().map(|meta| meta.len())
}

/// The file's modification time, `None` when it vanished.
fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    use std::time::{Duration, Instant};

    fn unique_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ducks-watch-{}-{}-{tag}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("mkdir temp watch dir");
        dir
    }

    fn white_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbaImage::from_pixel(w, h, image::Rgba([255, 255, 255, 255]));
        let mut cur = Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut cur, image::ImageFormat::Png)
            .expect("encode png");
        cur.into_inner()
    }

    /// The sprites inside a batch of outcomes (audit 2026-10-23 §F-104).
    fn sprites_of(outcomes: Vec<WatchOutcome>) -> Vec<agnosis_core::balloon::AsciiSprite> {
        outcomes
            .into_iter()
            .filter_map(|outcome| match outcome {
                WatchOutcome::Added { sprite, .. } => Some(sprite),
                WatchOutcome::Skipped { .. } => None,
            })
            .collect()
    }

    fn poll_drain(watcher: &mut BalloonWatcher) -> Vec<agnosis_core::balloon::AsciiSprite> {
        let deadline = Instant::now() + Duration::from_secs(9);
        loop {
            let sprites = sprites_of(watcher.drain());
            if !sprites.is_empty() || Instant::now() > deadline {
                return sprites;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// The first outcome the worker reports, whatever it is.
    fn poll_outcome(watcher: &mut BalloonWatcher) -> WatchOutcome {
        let deadline = Instant::now() + Duration::from_secs(9);
        loop {
            if let Some(outcome) = watcher.drain().into_iter().next() {
                return outcome;
            }
            assert!(Instant::now() <= deadline, "the watcher never reported");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Collect every sprite that arrives within `window`.
    ///
    /// Unlike [`poll_drain`] this does not stop at the first sprite: a
    /// test that counts the pushed sprites needs the whole window.
    fn drain_for(
        watcher: &mut BalloonWatcher,
        window: Duration,
    ) -> Vec<agnosis_core::balloon::AsciiSprite> {
        let deadline = Instant::now() + window;
        let mut out = Vec::new();
        while Instant::now() < deadline {
            out.extend(sprites_of(watcher.drain()));
            std::thread::sleep(Duration::from_millis(50));
        }
        out
    }

    #[test]
    fn dropped_png_appears_as_sprite() {
        let dir = unique_dir("png");
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        std::fs::write(dir.join("duck.png"), white_png(32, 32)).expect("write png");
        let sprites = poll_drain(&mut watcher);
        assert_eq!(sprites.len(), 1);
        assert_eq!((sprites[0].cols(), sprites[0].rows()), (13, 7));
        assert!(sprites[0].ink_count() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sibling_sprite_cache_wins() {
        let dir = unique_dir("sibling");
        std::fs::write(dir.join("art.png"), white_png(32, 32)).expect("write png");
        let cached = agnosis_core::balloon::AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]);
        agnosis_core::image_ascii::save_sprite_stamped(&cached, &dir.join("art.sprite.json"), 6, 3)
            .expect("save cache");
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        let sprites = poll_drain(&mut watcher);
        assert_eq!(sprites.len(), 1);
        assert_eq!(sprites[0], cached);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stale_cache_stamp_reconverts() {
        let dir = unique_dir("stalestamp");
        std::fs::write(dir.join("art.png"), white_png(32, 32)).expect("write png");
        let cached = agnosis_core::balloon::AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]);
        agnosis_core::image_ascii::save_sprite_stamped(&cached, &dir.join("art.sprite.json"), 4, 2)
            .expect("save cache");
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        let sprites = poll_drain(&mut watcher);
        assert_eq!(sprites.len(), 1);
        assert_ne!(sprites[0], cached, "stale stamp must reconvert");
        assert_eq!((sprites[0].cols(), sprites[0].rows()), (13, 7));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn unstamped_cache_reconverts() {
        let dir = unique_dir("nostamp");
        std::fs::write(dir.join("art.png"), white_png(32, 32)).expect("write png");
        let cached = agnosis_core::balloon::AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]);
        agnosis_core::image_ascii::save_sprite(&cached, &dir.join("art.sprite.json"))
            .expect("save cache");
        assert_eq!(
            agnosis_core::image_ascii::stamped_radii(&dir.join("art.sprite.json")),
            None
        );
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        let sprites = poll_drain(&mut watcher);
        assert_eq!(sprites.len(), 1);
        assert_ne!(sprites[0], cached, "unstamped cache must reconvert");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The conversion writes the sibling cache, stamped with the radii the
    /// art was built for (audit §3.8).
    #[test]
    fn conversion_writes_the_sibling_cache() {
        let dir = unique_dir("cachewrite");
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        std::fs::write(dir.join("duck.png"), white_png(32, 32)).expect("write png");
        let sprites = poll_drain(&mut watcher);
        assert_eq!(sprites.len(), 1);
        let cache = dir.join("duck.sprite.json");
        assert!(cache.is_file(), "the watcher must write the sibling cache");
        assert_eq!(
            agnosis_core::image_ascii::stamped_radii(&cache),
            Some((6, 3))
        );
        // The cache holds exactly the art that was pushed into the ring.
        let cached = agnosis_core::image_ascii::load_sprite(&cache).expect("load cache");
        assert_eq!(cached, sprites[0]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A geometry change must not push the same source a second time:
    /// `Scene::resize` keeps the balloon ring, so a second push would put
    /// the same art on two ducks (audit §3.8).
    #[test]
    fn radii_change_does_not_push_a_duplicate() {
        let dir = unique_dir("nodup");
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        std::fs::write(dir.join("a.png"), white_png(24, 24)).expect("write a");
        let first = poll_drain(&mut watcher);
        assert_eq!(first.len(), 1);
        assert_eq!((first[0].cols(), first[0].rows()), (13, 7));
        // The cache of the first conversion is what keeps the `done`
        // entry alive across the geometry change.
        let cache = dir.join("a.sprite.json");
        assert!(cache.is_file(), "the watcher must write the sibling cache");
        assert_eq!(
            agnosis_core::image_ascii::stamped_radii(&cache),
            Some((6, 3))
        );
        watcher.set_radii(7, 3);
        // The worker applies the radii asynchronously: wait for it, so the
        // pushes below are deterministic.
        watcher.sync();
        // A new file proves the worker is alive and uses the new radii.
        std::fs::write(dir.join("b.png"), white_png(24, 24)).expect("write b");
        let second = drain_for(&mut watcher, Duration::from_secs(4));
        assert_eq!(
            second.len(),
            1,
            "the radii change must not re-push the art that is in the ring"
        );
        assert_eq!((second[0].cols(), second[0].rows()), (15, 7));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed conversion must not leave a cache behind: the cache is
    /// the record of the art in the ring, and there is no art.
    #[test]
    fn failed_conversion_leaves_no_cache() {
        let dir = unique_dir("nocache");
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        std::fs::write(dir.join("bad.png"), b"not an image at all").expect("write corrupt");
        std::thread::sleep(Duration::from_millis(1200));
        assert!(
            sprites_of(watcher.drain()).is_empty(),
            "a corrupt file must not become a balloon"
        );
        assert!(
            !dir.join("bad.sprite.json").exists(),
            "a failed conversion must not leave a cache behind"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The cache lands in the watched folder, so it must never be picked
    /// up as balloon content (a converted cache would loop forever).
    #[test]
    fn the_sprite_cache_is_not_a_balloon_source() {
        let dir = unique_dir("cachenotsource");
        let cache = dir.join("a.sprite.json");
        assert!(
            !crate::balloon_resolve::is_balloon_source(&cache),
            "the cache must never be a balloon source"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The balloon→duck assignment must be reproducible: the pending set
    /// is a hash set, so its order is random unless the paths are sorted
    /// before they are converted.
    #[test]
    fn pending_paths_are_converted_in_sorted_order() {
        let dir = unique_dir("sorted");
        // Two `.ans` sources with a different number of inked cells, so
        // the sprite that arrives first names the file that was converted
        // first. Both are written before the watcher starts, so the
        // first poll scan queues them in one round.
        std::fs::write(dir.join("b.ans"), "##\n##\n##\n##").expect("write b");
        std::fs::write(dir.join("a.ans"), "####").expect("write a");
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        let sprites = drain_for(&mut watcher, Duration::from_secs(4));
        assert_eq!(sprites.len(), 2);
        assert_eq!(sprites[0].ink_count(), 4, "a.ans must be converted first");
        assert_eq!(sprites[1].ink_count(), 8, "b.ans must be converted second");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_files_skipped_without_error() {
        let dir = unique_dir("corrupt");
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        std::fs::write(dir.join("bad.png"), b"not an image at all").expect("write corrupt");
        std::thread::sleep(Duration::from_millis(400));
        for _ in 0..3 {
            assert!(watcher
                .drain()
                .iter()
                .all(|o| matches!(o, WatchOutcome::Skipped { .. })));
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A failed conversion is **reported**, not just traced: the input
    /// line says which file was skipped and why, exactly like the typed
    /// name path (audit 2026-10-23 §F-104).
    #[test]
    fn a_failed_conversion_reports_the_skip() {
        let dir = unique_dir("skipreason");
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        std::fs::write(dir.join("bad.png"), b"not an image at all").expect("write corrupt");
        match poll_outcome(&mut watcher) {
            WatchOutcome::Skipped { file, reason } => {
                assert_eq!(file, "bad.png", "the skip names the file");
                assert!(!reason.is_empty(), "the skip carries a reason");
            }
            WatchOutcome::Added { .. } => {
                panic!("a corrupt file must not become a balloon")
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_image_files_ignored() {
        let dir = unique_dir("nonimage");
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        std::fs::write(dir.join("notes.txt"), b"hello").expect("write txt");
        std::fs::write(dir.join("data.json"), b"{}").expect("write json");
        std::fs::write(dir.join(".hidden.png"), white_png(8, 8)).expect("write dotfile");
        std::fs::write(dir.join("half.tmp"), white_png(8, 8)).expect("write tmp");
        std::thread::sleep(Duration::from_millis(400));
        assert!(watcher.drain().is_empty());
        assert!(watcher.drain().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The batch settle's verdicts (audit 2026-10-23 §F-115): the sizes
    /// compared across the shared window decide, per path.
    #[test]
    fn settle_verdicts_come_from_the_shared_window() {
        use super::Settle;
        assert_eq!(settle_of(Some(10), Some(10)), Settle::Ready);
        assert_eq!(settle_of(None, Some(10)), Settle::Gone);
        assert_eq!(settle_of(Some(10), None), Settle::Gone);
        assert_eq!(settle_of(Some(10), Some(11)), Settle::Unstable);
    }

    #[test]
    fn set_radii_changes_output_size() {
        let dir = unique_dir("radii");
        let mut watcher = BalloonWatcher::new(&dir, 4, 2).expect("watch");
        std::fs::write(dir.join("a.png"), white_png(24, 24)).expect("write a");
        let first = poll_drain(&mut watcher);
        assert_eq!((first[0].cols(), first[0].rows()), (9, 5));
        watcher.set_radii(7, 3);
        // Wait for the worker to apply the new radii before the next file
        // shows up, so the conversion uses them.
        watcher.sync();
        std::fs::write(dir.join("b.png"), white_png(24, 24)).expect("write b");
        let second = poll_drain(&mut watcher);
        assert_eq!((second[0].cols(), second[0].rows()), (15, 7));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn vanished_file_is_forgotten_instead_of_requeued() {
        let dir = unique_dir("vanish");
        let mut watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        let gone = dir.join("gone.png");
        std::fs::write(&gone, white_png(16, 16)).expect("write");
        std::fs::remove_file(&gone).expect("remove");
        std::thread::sleep(Duration::from_millis(300));
        assert!(watcher.drain().is_empty());
        // The worker must not spin on the vanished file: the pending set is
        // empty and no sprite shows up.
        std::thread::sleep(Duration::from_millis(300));
        assert!(watcher.drain().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn drop_joins_the_worker() {
        let dir = unique_dir("drop");
        let watcher = BalloonWatcher::new(&dir, 6, 3).expect("watch");
        std::fs::write(dir.join("duck.png"), white_png(16, 16)).expect("write");
        drop(watcher);
        // The join happened in `drop`: the thread is gone, not detached.
        let _ = std::fs::remove_dir_all(&dir);
    }
}
