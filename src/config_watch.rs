//! The live config watcher (roadmap Extension 1 §1.3).
//!
//! The parade watches `config.toml` and applies the changes live. This
//! is the watcher half: it watches the config file's **parent directory**
//! and reports that the file changed. The reading and the parsing happen
//! on the UI thread, because a config file is a few kB and the parse is
//! microseconds — but the *watching* is here, on a worker thread, so the
//! UI thread never blocks on the file system.
//!
//! WHY the directory and not the file: the config is written atomically
//! (a temp file and a rename), so the file's inode is **replaced**. A
//! watcher on the file itself loses the event on Linux, and a watcher on
//! the directory sees every write of that name.
//!
//! WHY the event filter: reading the file raises an *access* event, and
//! a reload that triggered a reload would be an endless loop. Only the
//! create / modify / remove events are interesting.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

use notify::Watcher;

/// How long the watcher waits for the writes to settle.
///
/// An editor that writes the config in two steps (a temp file and a
/// rename) produces two events; the debounce collapses them into one
/// reload.
pub const DEBOUNCE: Duration = Duration::from_millis(150);

/// How long the worker waits for an event before it looks at the
/// shutdown flag.
const IDLE_POLL: Duration = Duration::from_millis(50);

/// Failures produced while setting the watcher up.
#[derive(Debug, thiserror::Error)]
pub enum ConfigWatchError {
    /// The file system's watcher could not be created.
    #[error("could not create the config watcher: {0}")]
    Notify(#[from] notify::Error),
    /// The worker thread could not be started.
    #[error("could not start the config watcher: {0}")]
    Io(#[from] std::io::Error),
    /// The config file has no parent directory to watch.
    #[error("the config file {0:?} has no parent directory to watch")]
    NoParent(PathBuf),
}

/// A watcher on the config file's directory.
pub struct ConfigWatcher {
    /// The notify handle. It is kept alive for as long as the watcher
    /// lives: dropping it stops the events.
    _watcher: notify::RecommendedWatcher,
    /// The worker thread.
    worker: Option<std::thread::JoinHandle<()>>,
    /// The paths the worker reported.
    results: Receiver<PathBuf>,
    /// The flag the worker checks before it wakes up.
    shutdown: Arc<AtomicBool>,
}

impl ConfigWatcher {
    /// Watch the directory that holds `config_path`.
    ///
    /// The directory is watched non-recursively: only the config file
    /// itself is interesting, and the balloon folder has its own watcher.
    pub fn new(config_path: &Path) -> Result<Self, ConfigWatchError> {
        let parent = config_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| ConfigWatchError::NoParent(config_path.to_path_buf()))?
            .to_path_buf();
        let (events_tx, events) = std::sync::mpsc::channel();
        let mut watcher = notify::recommended_watcher(events_tx)?;
        watcher.watch(&parent, notify::RecursiveMode::NonRecursive)?;
        let (out_tx, out) = std::sync::mpsc::channel();
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let worker_path = config_path.to_path_buf();
        let worker = std::thread::Builder::new()
            .name("agnosis-config-watch".to_string())
            .spawn(move || {
                worker_main(worker_path, events, out_tx, worker_shutdown);
            })?;
        Ok(Self {
            _watcher: watcher,
            worker: Some(worker),
            results: out,
            shutdown,
        })
    }

    /// The config paths that changed since the last drain.
    ///
    /// The drain is non-blocking: the popup calls it once per frame.
    pub fn drain(&mut self) -> Vec<PathBuf> {
        let mut out = Vec::new();
        while let Ok(path) = self.results.try_recv() {
            out.push(path);
        }
        out
    }
}

impl Drop for ConfigWatcher {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// What one step of the worker's loop produced.
#[derive(Debug, PartialEq, Eq)]
enum WatchStep {
    /// Nothing to report.
    Idle,
    /// The config changed: report the path.
    Changed(PathBuf),
    /// The event channel is gone: the worker must stop.
    Stop,
}

/// The worker's loop: filter the events, debounce them, report the path.
fn worker_main(
    config_path: PathBuf,
    events: Receiver<notify::Result<notify::Event>>,
    out: Sender<PathBuf>,
    shutdown: Arc<AtomicBool>,
) {
    let mut pending: Option<(PathBuf, Instant)> = None;
    while !shutdown.load(Ordering::Relaxed) {
        let wait = wait_for(&pending);
        if !report_step(
            receive_event(&events, wait, &config_path, &mut pending),
            &out,
        ) {
            return;
        }
        if !report_step(take_ready(&mut pending), &out) {
            return;
        }
    }
}

/// Report one step of the worker's loop.
///
/// `false` when the worker must stop: the receiver is gone, or the
/// event channel disconnected.
fn report_step(step: WatchStep, out: &Sender<PathBuf>) -> bool {
    match step {
        WatchStep::Idle => true,
        WatchStep::Changed(path) => out.send(path).is_ok(),
        WatchStep::Stop => false,
    }
}

/// Wait for one event and fold it into the pending state.
///
/// [`WatchStep::Stop`] when the channel is gone (the watcher died or the
/// popup has shut down), so the worker returns instead of reporting a
/// change that never happened (audit 2026-10-21 §G28).
fn receive_event(
    events: &Receiver<notify::Result<notify::Event>>,
    wait: Duration,
    config_path: &Path,
    pending: &mut Option<(PathBuf, Instant)>,
) -> WatchStep {
    match events.recv_timeout(wait) {
        Ok(Ok(event)) => {
            if is_interesting(&event) && touches_config(&event, config_path) {
                *pending = Some((config_path.to_path_buf(), Instant::now()));
            }
            WatchStep::Idle
        }
        Ok(Err(_)) => WatchStep::Idle,
        Err(RecvTimeoutError::Timeout) => WatchStep::Idle,
        Err(RecvTimeoutError::Disconnected) => WatchStep::Stop,
    }
}

/// How long the worker waits for the next event.
fn wait_for(pending: &Option<(PathBuf, Instant)>) -> Duration {
    match pending {
        Some((_, at)) => DEBOUNCE
            .saturating_sub(at.elapsed())
            .max(Duration::from_millis(1)),
        None => IDLE_POLL,
    }
}

/// The pending path, once the debounce has elapsed.
fn take_ready(pending: &mut Option<(PathBuf, Instant)>) -> WatchStep {
    let ready = pending
        .as_ref()
        .map(|(_, at)| at.elapsed() >= DEBOUNCE)
        .unwrap_or(false);
    if !ready {
        return WatchStep::Idle;
    }
    match pending.take() {
        Some((path, _)) => WatchStep::Changed(path),
        None => WatchStep::Idle,
    }
}

/// True when the event is one worth reloading for.
///
/// The **access** events are excluded: reading the config raises one, and
/// a reload that triggered a reload would never end.
fn is_interesting(event: &notify::Event) -> bool {
    matches!(
        event.kind,
        notify::EventKind::Create(_) | notify::EventKind::Modify(_) | notify::EventKind::Remove(_)
    )
}

/// True when the event touches the config file.
fn touches_config(event: &notify::Event, config_path: &Path) -> bool {
    let name = config_path.file_name();
    event.paths.iter().any(|path| path.file_name() == name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A temp folder that is not the user's real config folder.
    fn temp_dir(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "agnosis-config-watch-{}-{nanos}-{tag}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("mkdir temp dir");
        dir
    }

    #[test]
    fn a_change_is_reported() {
        let dir = temp_dir("change");
        let path = dir.join("config.toml");
        std::fs::write(&path, "loop_seconds = 20.0").expect("write");
        let mut watcher = ConfigWatcher::new(&path).expect("watch");
        // Give the watcher a moment to register the directory.
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(&path, "loop_seconds = 12.0").expect("write");
        let mut seen = Vec::new();
        for _ in 0..40 {
            seen.extend(watcher.drain());
            if !seen.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!seen.is_empty(), "the change must be reported");
        assert_eq!(seen[0], path);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_untouched_file_is_not_reported() {
        let dir = temp_dir("untouched");
        let path = dir.join("config.toml");
        std::fs::write(&path, "loop_seconds = 20.0").expect("write");
        let mut watcher = ConfigWatcher::new(&path).expect("watch");
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            watcher.drain().is_empty(),
            "nothing must be reported without a change"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn another_file_in_the_folder_is_not_reported() {
        let dir = temp_dir("other");
        let path = dir.join("config.toml");
        std::fs::write(&path, "loop_seconds = 20.0").expect("write");
        let mut watcher = ConfigWatcher::new(&path).expect("watch");
        std::thread::sleep(Duration::from_millis(200));
        std::fs::write(dir.join("something-else.txt"), "x").expect("write");
        std::thread::sleep(Duration::from_millis(400));
        assert!(
            watcher.drain().is_empty(),
            "another file must not trigger a reload"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_debounce_collapses_a_double_write() {
        let dir = temp_dir("debounce");
        let path = dir.join("config.toml");
        std::fs::write(&path, "loop_seconds = 20.0").expect("write");
        let mut watcher = ConfigWatcher::new(&path).expect("watch");
        std::thread::sleep(Duration::from_millis(200));
        // Two writes in quick succession, like a temp file and a rename.
        std::fs::write(&path, "loop_seconds = 12.0").expect("write");
        std::fs::write(&path, "loop_seconds = 14.0").expect("write");
        std::thread::sleep(Duration::from_millis(600));
        let seen = watcher.drain();
        assert!(
            seen.len() <= 1,
            "the double write must collapse into one reload, got {}",
            seen.len()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_disconnected_channel_stops_the_worker() {
        // The watcher is gone: the worker must stop, not report the config
        // as "changed" over and over (audit 2026-10-21 §G28).
        let (tx, rx) = std::sync::mpsc::channel();
        drop(tx);
        let mut pending: Option<(PathBuf, Instant)> = None;
        let step = receive_event(
            &rx,
            Duration::from_millis(10),
            Path::new("config.toml"),
            &mut pending,
        );
        assert_eq!(step, WatchStep::Stop);
        assert!(pending.is_none(), "nothing must be pending");
    }

    #[test]
    fn an_access_event_is_not_interesting() {
        let event = notify::Event {
            kind: notify::EventKind::Access(notify::event::AccessKind::Read),
            paths: vec![PathBuf::from("config.toml")],
            attrs: Default::default(),
        };
        assert!(!is_interesting(&event));
        let event = notify::Event {
            kind: notify::EventKind::Modify(notify::event::ModifyKind::Any),
            paths: vec![PathBuf::from("config.toml")],
            attrs: Default::default(),
        };
        assert!(is_interesting(&event));
    }
}
