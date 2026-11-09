//! The backdrop loader: a worker thread that turns the configured
//! pictures into ASCII tiles.
//!
//! The conversion is an **image decode**, and it must never run on the
//! UI thread: the typed-balloon path blocked the parade for a second or
//! two in exactly this shape (audit 2026-10-20 §3.2). The worker owns
//! the decode; the UI thread only ever holds finished tiles.
//!
//! A picture that is missing or broken is **skipped**, and the layers
//! that did load still show: a broken backdrop leaves the parade exactly
//! as it was (roadmap Extension 4 §1.3).
//!
//! The split is deliberate: the **conversion** is pure and lives in
//! [`agnosis_core::backdrop`], and only the **thread** lives here. The worker
//! resolves the configured *names* into absolute paths first — the name
//! of a picture is a platform question — and then hands the layers to
//! the core converter.

use std::sync::mpsc::{channel, Receiver, Sender};

use agnosis_core::balloon::AsciiSprite;
use agnosis_core::config::BackdropConfig;

/// The worker that converts the backdrop pictures.
pub struct BackdropLoader {
    /// The layers to build, with the request's number. The channel is
    /// unbounded, so a request never blocks the UI thread.
    requests: Sender<(u64, Vec<BackdropConfig>)>,
    /// The finished tiles, with the request's number.
    results: Receiver<(u64, Vec<AsciiSprite>)>,
    /// The number of the last request.
    ///
    /// A result that carries an **older** number is dropped: the config
    /// changed while the worker was busy, and the older tiles are not
    /// what the user asked for any more.
    generation: u64,
}

impl BackdropLoader {
    /// Start the worker thread.
    ///
    /// A thread that cannot be spawned leaves the loader inert: the
    /// parade simply has no backdrop, which is the same as a missing
    /// picture.
    pub fn start() -> Self {
        let (request_tx, request_rx) = channel::<(u64, Vec<BackdropConfig>)>();
        let (result_tx, result_rx) = channel::<(u64, Vec<AsciiSprite>)>();
        let spawned = std::thread::Builder::new()
            .name("agnosis-backdrop".into())
            .spawn(move || loader_worker(request_rx, result_tx));
        if spawned.is_err() {
            agnosis_core::trace::warn("backdrop: the worker thread could not be started");
        }
        Self {
            requests: request_tx,
            results: result_rx,
            generation: 0,
        }
    }

    /// Ask the worker to build the layers.
    ///
    /// The request **replaces** the previous one: the worker works
    /// through the queue, and only the newest answer is used.
    pub fn request(&mut self, layers: Vec<BackdropConfig>) {
        self.generation += 1;
        let _ = self.requests.send((self.generation, layers));
    }

    /// Take the finished tiles, if the worker has them.
    ///
    /// A stale answer is dropped and `None` is returned: the newer one is
    /// still in the channel and arrives on the next tick.
    pub fn drain(&mut self) -> Option<Vec<AsciiSprite>> {
        let (generation, tiles) = self.results.try_recv().ok()?;
        if !crate::stale_stamp::answer_is_current(&generation, &self.generation) {
            return None;
        }
        Some(tiles)
    }
}

/// The worker loop: convert what is asked for, until the popup is gone.
fn loader_worker(
    requests: Receiver<(u64, Vec<BackdropConfig>)>,
    results: Sender<(u64, Vec<AsciiSprite>)>,
) {
    while let Ok((generation, layers)) = requests.recv() {
        let tiles = agnosis_core::backdrop::convert_layers(&resolve_layers(&layers));
        if results.send((generation, tiles)).is_err() {
            break;
        }
    }
}

/// Resolve the configured picture names into absolute paths.
///
/// The name of a picture is a **platform** question — which folder is it
/// in? — so the shell answers it before the core converter sees the
/// layers. The core only ever reads an absolute path.
///
/// The rule lives in [`crate::host::resolve_backdrop_layer`]: one
/// implementation, one caller set (audit 2026-10-21 §G30).
fn resolve_layers(layers: &[BackdropConfig]) -> Vec<BackdropConfig> {
    layers.iter().map(resolve_layer).collect()
}

/// One layer, with its picture resolved.
fn resolve_layer(layer: &BackdropConfig) -> BackdropConfig {
    let mut resolved = layer.clone();
    crate::host::resolve_backdrop_layer(&mut resolved);
    resolved
}

/// Load the configured backdrops into a scene, **on the calling thread**.
///
/// The popup does **not** use this: it converts on the worker, so a
/// big picture never blocks the parade. The headless paths (the selftest
/// gate, `agnosis shot` and the export) are not the UI thread, so they
/// convert in place — and they then show the same picture the app does.
pub fn load_into(scene: &mut agnosis_core::scene::Scene, cfg: &agnosis_core::config::Config) {
    let mut resolved = cfg.clone();
    crate::host::resolve_backdrop_paths(&mut resolved);
    let tiles = agnosis_core::backdrop::convert_layers(&resolved.backdrop);
    scene.set_backdrops(tiles);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A picture that does not exist is skipped, not fatal.
    #[test]
    fn a_missing_picture_is_skipped() {
        let layers = vec![BackdropConfig {
            image: "no-such-picture.png".to_string(),
            ..BackdropConfig::default()
        }];
        assert!(agnosis_core::backdrop::convert_layers(&layers).is_empty());
    }

    /// An empty name is not a picture.
    #[test]
    fn an_empty_name_is_skipped() {
        let layers = vec![BackdropConfig {
            image: "   ".to_string(),
            ..BackdropConfig::default()
        }];
        assert!(agnosis_core::backdrop::convert_layers(&layers).is_empty());
    }

    /// An absolute path is taken as it is; a name is looked up in the
    /// `backdrops` folder.
    ///
    /// The path comes from the **temp** folder: it is absolute on every
    /// platform, and it does not depend on the current directory, which
    /// may be gone (audit 2026-10-21 §G13).
    #[test]
    fn an_absolute_path_is_kept() {
        let picture = std::env::temp_dir().join("hills.png");
        let resolved = crate::host::resolve_backdrop_image(&picture.to_string_lossy())
            .expect("an absolute path");
        assert_eq!(resolved, picture);
    }

    /// The worker answers a missing picture with an **empty** tile list,
    /// and it answers at all: the loader never hangs on a picture it
    /// cannot read (audit 2026-10-21 §G22).
    ///
    /// The old body asserted nothing — "the worker may or may not have
    /// answered by now; both outcomes are fine" — so the test could
    /// never fail.
    #[test]
    fn the_loader_survives_a_request_it_cannot_serve() {
        let mut loader = BackdropLoader::start();
        loader.request(vec![BackdropConfig {
            image: "no-such-picture.png".to_string(),
            ..BackdropConfig::default()
        }]);
        let answer = drain_within(&mut loader, Duration::from_secs(5));
        let tiles = answer.expect("the worker must answer");
        assert!(tiles.is_empty(), "a missing picture yields no tile");
    }

    /// A stale answer is dropped: only the newest request counts.
    #[test]
    fn a_stale_answer_is_dropped() {
        let mut loader = BackdropLoader::start();
        loader.request(Vec::new());
        loader.request(Vec::new());
        // The first answer carries generation 1 and the loader is at 2,
        // so it is stale and dropped; the second one is taken.
        let mut taken = 0;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if loader.drain().is_some() {
                taken += 1;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(taken, 1, "exactly the newest answer may be used");
        // And nothing is left behind: the stale answer was **dropped**,
        // not queued up behind the newest one.
        assert!(
            loader.drain().is_none(),
            "the stale answer must be dropped, not kept"
        );
    }

    /// Drain until the worker answers, or the budget runs out.
    fn drain_within(loader: &mut BackdropLoader, budget: Duration) -> Option<Vec<AsciiSprite>> {
        let deadline = std::time::Instant::now() + budget;
        while std::time::Instant::now() < deadline {
            if let Some(tiles) = loader.drain() {
                return Some(tiles);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        None
    }
}
