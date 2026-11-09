//! The drawn-asset loader: a worker thread that loads the assets a
//! configuration names.
//!
//! WHY a worker: a drawn frame is a JSON document of up to four frames —
//! hundreds of kilobytes — and the **live config reload** used to parse
//! up to nineteen of them on the UI thread, inside `tick`. The rule is
//! "never block the UI thread" (AGENTS.md), so the load happens here and
//! the popup only ever holds the **finished** sprites.
//!
//! The shape is the one the backdrop loader already uses: a request
//! channel, a result channel and a generation counter, so a stale answer
//! is dropped when the config changed while the worker was busy.
//!
//! The seam on the other side is the core's
//! [`agnosis_core::scene::Scene::apply_config_with`]: the scene asks for the
//! assets it needs through a **loader closure**, and the closure answers
//! from the finished set — the disk is never touched on the UI thread.

use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};

use agnosis_core::assets::AssetDirs;
use agnosis_core::config::Config;
use agnosis_core::drawn::{DrawnError, DrawnKind, DrawnSize, DrawnSprite};

/// The assets a configuration names, loaded.
pub struct LoadedAssets {
    /// The loaded sprites, by kind and name.
    entries: Vec<(DrawnKind, String, DrawnSprite)>,
}

impl LoadedAssets {
    /// The loader the scene asks an asset by.
    ///
    /// The closure is what [`agnosis_core::scene::Scene::apply_config_with`]
    /// takes: the scene asks for the assets it needs, and the answer
    /// comes from the **finished** set.
    pub fn loader(&self) -> impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError> + '_ {
        move |kind, name| {
            let found = self
                .entries
                .iter()
                .find(|(k, n, _)| *k == kind && n == name);
            match found {
                Some((_, _, sprite)) => Ok(sprite.clone()),
                None => Err(DrawnError::NotFound {
                    name: name.to_string(),
                }),
            }
        }
    }

    /// How many assets were loaded.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Were no assets loaded at all?
    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The finished assets and the configuration they belong to.
///
/// The pair is **boxed** where it travels: the configuration alone is
/// several hundred bytes, and the enum would otherwise be as big as the
/// whole configuration.
pub struct Loaded {
    /// The configuration the assets were loaded for.
    pub cfg: Config,
    /// The assets themselves.
    pub assets: LoadedAssets,
}

/// What one drain of the loader found.
pub enum Drain {
    /// The worker has not answered yet.
    Pending,
    /// The assets, with the configuration they belong to.
    Ready(Box<Loaded>),
    /// The worker is gone: the caller must load the assets itself.
    Gone,
}

/// The worker that loads the drawn assets.
pub struct AssetLoader {
    /// The loads to do, with the request's number.
    requests: Sender<(u64, Config, AssetDirs)>,
    /// The finished assets, with the request's number and the
    /// configuration they belong to.
    results: Receiver<(u64, Config, LoadedAssets)>,
    /// The number of the last request.
    ///
    /// An answer that carries an **older** number is dropped: the config
    /// changed while the worker was busy, and those assets are not what
    /// the user asked for any more.
    generation: u64,
}

impl AssetLoader {
    /// Start the worker thread.
    ///
    /// A thread that cannot be spawned leaves the loader inert: the
    /// reload falls back to loading on the calling thread, which is the
    /// old behaviour — slow, but never a broken parade.
    pub fn start() -> Self {
        let (request_tx, request_rx) = channel::<(u64, Config, AssetDirs)>();
        let (result_tx, result_rx) = channel::<(u64, Config, LoadedAssets)>();
        let spawned = std::thread::Builder::new()
            .name("agnosis-assets".into())
            .spawn(move || loader_worker(request_rx, result_tx));
        if spawned.is_err() {
            agnosis_core::trace::warn("assets: the worker thread could not be started");
        }
        Self {
            requests: request_tx,
            results: result_rx,
            generation: 0,
        }
    }

    /// Ask the worker to load the assets `cfg` names.
    ///
    /// `false` when the request could not be queued: the worker is gone,
    /// and the caller has to load the assets itself.
    pub fn request(&mut self, cfg: &Config, assets: AssetDirs) -> bool {
        self.generation += 1;
        self.requests
            .send((self.generation, cfg.clone(), assets))
            .is_ok()
    }

    /// Take the finished assets, if the worker has them.
    pub fn drain(&mut self) -> Drain {
        match self.results.try_recv() {
            Ok((generation, cfg, loaded)) => {
                if !crate::stale_stamp::answer_is_current(&generation, &self.generation) {
                    // A stale answer: the newer one is on its way.
                    return Drain::Pending;
                }
                Drain::Ready(Box::new(Loaded {
                    cfg,
                    assets: loaded,
                }))
            }
            Err(TryRecvError::Empty) => Drain::Pending,
            Err(TryRecvError::Disconnected) => Drain::Gone,
        }
    }
}

/// The worker's loop: load what is asked for, until the popup is gone.
fn loader_worker(
    requests: Receiver<(u64, Config, AssetDirs)>,
    results: Sender<(u64, Config, LoadedAssets)>,
) {
    while let Ok((generation, cfg, assets)) = requests.recv() {
        let loaded = load_the_assets(&cfg, &assets);
        if results.send((generation, cfg, loaded)).is_err() {
            break;
        }
    }
}

/// Load every asset the configuration names.
///
/// A missing or broken asset is **not fatal**: it is traced and the
/// built-in art stays — the same rule the core's own loader follows.
fn load_the_assets(cfg: &Config, assets: &AssetDirs) -> LoadedAssets {
    let mut entries = Vec::new();
    let Some(dirs) = assets.drawn.as_ref() else {
        return LoadedAssets { entries };
    };
    for (kind, name) in wanted_assets(cfg) {
        match dirs.load(kind, &name) {
            Ok(sprite) => entries.push((kind, name, sprite)),
            Err(e) => {
                agnosis_core::trace::trace(&format!("assets: {kind:?} {name:?} ignored: {e}"))
            }
        }
    }
    LoadedAssets { entries }
}

/// Every asset the configuration names, as a kind and a name.
///
/// The names come from the configuration's **own** accessors, so the
/// list cannot drift from what the scene asks for.
fn wanted_assets(cfg: &Config) -> Vec<(DrawnKind, String)> {
    let mut out = Vec::new();
    for number in 1..=agnosis_core::parade::BIG_SLOTS {
        push_name(&mut out, DrawnKind::Sprite, cfg.assignments.big(number));
    }
    let small_count = agnosis_core::parade::DUCK_COUNT - agnosis_core::parade::BIG_SLOTS;
    for number in 1..=small_count {
        push_name(&mut out, DrawnKind::Sprite, cfg.assignments.small(number));
    }
    for number in 1..=agnosis_core::balloon::BALLOON_HOLDER_DUCK.len() {
        push_name(
            &mut out,
            DrawnKind::Balloon,
            cfg.assignments.balloon(number),
        );
    }
    push_name(&mut out, DrawnKind::Grass, cfg.grass.tile.as_deref());
    push_name(
        &mut out,
        DrawnKind::Decoration,
        cfg.decorations.get(DrawnSize::Big),
    );
    push_name(
        &mut out,
        DrawnKind::Decoration,
        cfg.decorations.get(DrawnSize::Small),
    );
    out
}

/// Add one name to the list, when the configuration names one.
fn push_name(out: &mut Vec<(DrawnKind, String)>, kind: DrawnKind, name: Option<&str>) {
    let Some(name) = name else {
        return;
    };
    out.push((kind, name.to_string()));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A config with no assets names nothing at all.
    #[test]
    fn a_default_config_names_no_asset() {
        let cfg = Config::default();
        assert!(wanted_assets(&cfg).is_empty());
    }

    /// The names come from the configuration's own accessors.
    #[test]
    fn the_named_assets_are_listed() {
        let mut cfg = Config::default();
        let slot = agnosis_core::config::AssignmentSlot::parse("big_1").expect("big_1");
        cfg.assignments.set(slot, Some("cat".to_string()));
        cfg.grass.tile = Some("meadow".to_string());
        let named = wanted_assets(&cfg);
        assert!(
            named.contains(&(DrawnKind::Sprite, "cat".to_string())),
            "got {named:?}"
        );
        assert!(
            named.contains(&(DrawnKind::Grass, "meadow".to_string())),
            "got {named:?}"
        );
    }

    /// The loader answers from the finished set, and a name it does not
    /// carry is a **not found** — never a disk read.
    #[test]
    fn the_loader_answers_from_the_finished_set() {
        let sprite = DrawnSprite {
            kind: DrawnKind::Sprite,
            size: Some(DrawnSize::Big),
            frames: Vec::new(),
            reaction: None,
        };
        let loaded = LoadedAssets {
            entries: vec![(DrawnKind::Sprite, "cat".to_string(), sprite.clone())],
        };
        let loader = loaded.loader();
        assert_eq!(
            loader(DrawnKind::Sprite, "cat").expect("the sprite"),
            sprite
        );
        assert!(
            loader(DrawnKind::Sprite, "dog").is_err(),
            "a name the set does not carry must be a not-found"
        );
        assert_eq!(loaded.len(), 1);
        assert!(!loaded.is_empty());
    }
}
