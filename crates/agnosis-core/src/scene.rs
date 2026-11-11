//! Scene composition: the pure simulation owning a [`crate::grid::Grid`].
//!
//! Per frame, in order: clear to black, grass band, duck sprites, balloons,
//! ropes, HUD text. Duck and pendulum geometry runs in unwrapped space so a
//! duck stepping off the left edge never stretches its rope across the screen;
//! wrapping happens only at raster time via [`crate::grid::Grid::set`].
//!
//! Fixed-step integration: simulation advances on a 1/120 s timestep
//! accumulator, so gait, parade and pendulum motion are independent of the
//! render rate.
//!
//! Stage 8 adds the **drawn assets**: the config may point a parade slot at
//! a user-drawn sprite or balloon (roadmap §3.8). The documents are loaded
//! **once**, when the scene is built ([`Scene::new_with_assets`]) — never
//! inside `tick`, so the UI thread never touches the filesystem. The scene
//! never resolves a platform folder itself: the shell hands it the
//! directories ([`crate::assets::AssetDirs`]). A drawn sprite keeps the
//! built-in frame size
//! (150 x 70 / 75 x 35), so the train spacing never changes, and its
//! animation frame is taken modulo the **art's own** frame count. A missing
//! or corrupt asset is not fatal: it is traced and the built-in art stays.

use crate::assets::AssetDirs;
use crate::audio::sfx::SfxId;
use crate::audio::AudioEvent;
use crate::audio::{Sequencer, Track};
use crate::balloon::{self, AsciiSprite, RingBuffer, BALLOON_HOLDER_DUCK};
use crate::config::{Config, HintCorner};
use crate::content::{ContentKind, ContentLayers};
use crate::drawn::{DrawnError, DrawnKind, DrawnSize, DrawnSprite};
use crate::grid::Grid;
use crate::parade::{Parade, DUCK_COUNT};
use crate::pendulum::Pendulum;
use crate::reaction::{Reaction, ReactionKind};
use crate::rope::draw_rope_stop_at_ink;
use crate::sky;
use crate::sprites::{big_duck, small_duck, WalkSprite};
use crate::weather::WeatherState;

/// Fixed simulation step in seconds: the single owner of the 1/120 s
/// timestep (audit maintainability) — defined once as the pendulum substep
/// so the scene accumulator and the pendulum integrator can never drift
/// apart; `step_matches_pendulum_substep` pins the spelling below.
///
/// WHY the name: the gait has a step of its own (1/6 s) and the two
/// differ by a factor of twenty — a generic `STEP_SECONDS` in two
/// modules is a 20x error waiting for the next `use` that imports both
/// (audit 2026-10-21 §E6). The gait's is [`crate::gait::GAIT_STEP_SECONDS`].
pub const SIM_STEP_SECONDS: f32 = crate::pendulum::SUBSTEP_SECONDS;

/// Duck body colour: warm yellow.
pub const DUCK_COLOR: [u8; 3] = [255, 220, 80];

/// Grass colour: meadow green.
pub const GRASS_COLOR: [u8; 3] = [90, 200, 90];

/// How dark the daylight tint may floor the grass (audit 2026-10-23
/// F-026).
///
/// The grass follows the time of day: full green at noon, a quarter of it
/// at midnight. The floor keeps a midnight meadow **readable as grass** —
/// a black band would read as a hole in the ground.
const GRASS_DIM_FLOOR: f32 = 0.25;

/// How many rows the built-in grass band covers (audit 2026-10-23 F-024):
/// the rail row, the wave row, a sparse blade row and a soil dot row. A
/// drawn tile is still as tall as the user drew it — this is the built-in
/// band's own height, and the weather's rules read it through
/// [`Scene::band_rows`].
const BUILTIN_GRASS_ROWS: usize = 4;

/// Every how many columns the sparse blade row (third row of the built-in
/// band) carries a blade: one in six reads as tufts without becoming a
/// second wave row.
const BLADE_EVERY: i64 = 6;

/// Rope colour: rope brown.
pub const ROPE_COLOR: [u8; 3] = [180, 140, 90];

/// Empty-balloon disc colour: dim slate.
pub const EMPTY_BALLOON_COLOR: [u8; 3] = [90, 110, 140];

/// The empty balloon's highlight arc (audit 2026-10-23 F-027): the inner
/// edge of the upper-left quadrant — the sky light comes from there — in
/// a pale tone, so the disc reads as shaded rather than as a hoop.
const EMPTY_BALLOON_HIGHLIGHT: [u8; 3] = [170, 190, 220];

/// The empty balloon's interior speckle (audit 2026-10-23 F-027): dimmer
/// than the ring, sparse across the bottom third of the interior.
const EMPTY_BALLOON_SHADE: [u8; 3] = [70, 85, 110];

/// HUD hint colour: dim grey.
pub const HUD_COLOR: [u8; 3] = [140, 140, 140];

/// Walk frames per second of the duck gait.
///
/// Public because it is the **sequencer's clock** as well: one strip slot
/// is one sprite step, and the editor and the popup both need the same
/// number (roadmap Extension 3 §1). The number itself lives in
/// [`crate::gait`], so the scene, the sequencer and the editor cannot
/// drift apart.
pub use crate::gait::GAIT_FPS;

/// The colour a reacting duck flares towards: white.
const FLASH_COLOR: [u8; 3] = [255, 255, 255];

/// Input/status line geometry (audit §5 + P1-10): the block-font line starts
/// `ground + 8` and is 5 rows tall, so a grid needs `rows - ground >= 13`
/// for the full line; anything less skips rendering with a trace.
const INPUT_ROW_OFFSET: i64 = 8;
const INPUT_FONT_ROWS: i64 = 5;
const INPUT_TEXT_COL: i64 = 2;

/// HUD hint geometry: 5-tall block font with one blank margin row below.
const HUD_MARGIN_ROWS: i64 = 6;
const HUD_LEFT_COL: i64 = 1;

/// The custom message's geometry (roadmap Extension 1 §1.1).
///
/// The block font is 5 rows tall. Above the grass the message's **bottom**
/// row lands on `ground - 2`, so the band is `ground - 6 ..= ground - 2`;
/// below the grass the band is `ground + 3 ..= ground + 7`, clear of the
/// input line at `ground + 8` and of the hint at `rows - 6`.
const MESSAGE_FONT_ROWS: i64 = 5;
const MESSAGE_ABOVE_OFFSET: i64 = -6;
const MESSAGE_BELOW_OFFSET: i64 = 3;
const MESSAGE_LEFT_COL: i64 = 1;

/// The message's colour on a dark band.
const MESSAGE_COLOR: [u8; 3] = [255, 255, 255];

/// The message's colour on a **light** band (audit 2026-10-23 F-029):
/// white on a day horizon is ink on paper — unreadable — so the ink
/// flips to a deep navy there. One luminance comparison decides.
const MESSAGE_INK_DARK: [u8; 3] = [25, 35, 60];

/// The luminance above which the message's band counts as light, in
/// `0.0..=1.0` (Rec. 601 luma).
const MESSAGE_LIGHT_BAND: f32 = 0.5;

/// How long the message takes to fade out, in seconds.
const MESSAGE_FADE_SECONDS: f32 = 1.0;

/// The counter's geometry: the "days since the ducks" line sits in the
/// bottom-left corner, on its own row above the hint.
const COUNTER_ROW_OFFSET: i64 = 12;
const COUNTER_LEFT_COL: i64 = 1;

/// The counter's colour.
const COUNTER_COLOR: [u8; 3] = [170, 170, 190];

/// Empty-balloon ring: outline cells with normalized distance in this band
/// (prototype: filled discs read as grey clouds over the parade).
const RING_INNER_D: f32 = 0.86;

/// Empty-balloon highlight: the arc lives in this inner band of the
/// upper-left quadrant (between here and [`RING_INNER_D`]).
const HIGHLIGHT_INNER_D: f32 = 0.62;

/// The contact shadow (audit 2026-10-23 F-032): a one-row speckle under
/// the duck's body centre, shifted toward the march direction (the light
/// comes from ahead of the train). The speckle spans three cells and
/// skips the grass's own glyphs, so it reads as shade in the gaps.
const SHADOW_OFFSET_COLS: i64 = -3;
const SHADOW_SPAN_COLS: usize = 3;

/// The stars (audit 2026-10-23 F-025): roughly one lit sky cell in this
/// many, placed by a column/row hash — pure function of the grid, so the
/// night sky is byte-identical on every render.
const STAR_EVERY: i64 = 120;

/// The hash seed pair for the star placement: `(col * 31 + row * 17) % N`
/// has no visible axis-aligned structure at screen scale.
const STAR_COL_MIX: i64 = 31;
const STAR_ROW_MIX: i64 = 17;

/// Largest simulated span one `advance` call may integrate (audit
/// maintainability: the step guard must be derived from the step constant
/// so the two cannot drift apart).
const MAX_ADVANCE_SECONDS: f32 = 600.0;

/// Fixed-step watchdog: at most this many sim steps per `advance` call.
///
/// Derived from [`SIM_STEP_SECONDS`], so the guard and the timestep can
/// never drift: `600 s / (1/120 s)` = 72000 steps, the old hand-written
/// `3600 * 20`.
const MAX_STEPS_PER_ADVANCE: u32 = (MAX_ADVANCE_SECONDS / SIM_STEP_SECONDS).round() as u32;

/// Deterministic rope-roll hash seed (splitmix64 golden-ratio constant).
const ROPE_SEED_MIX: u64 = 0x9E37_79B9_7F4A_7C15;
const ROPE_SEED_MUL: u64 = 0xFF51_AFD7_ED55_8CCD;

/// The duck's drawn rectangle: where its frame lands on the grid.
///
/// The draw path and the hit test share it, so a click always lands on
/// the duck the user sees (roadmap Extension 3 §5.2).
///
/// The rectangle's fields are read by the hit test, so the struct is
/// **not** dead code — the `#[allow(dead_code)]` it used to carry was
/// a leftover from the binary crate, where a `pub` item could go
/// unused; in a library it can only mask a real regression (audit
/// 2026-10-21 §F9).
#[derive(Debug, Clone, Copy)]
struct DuckBox {
    /// The frame's left column.
    left: i64,
    /// The frame's top row.
    top: i64,
    /// The frame's width in cells.
    cols: i64,
    /// The frame's height in cells.
    rows: i64,
}

/// One parallax backdrop layer (roadmap Extension 4 §1.1).
///
/// The layer is a **grid of its own**, not a part of the scene grid: it
/// scrolls at its own rate, so it cannot share the parade's offset.
struct BackdropLayer {
    /// The tile the layer is built from.
    ///
    /// It is kept so a resize can rebuild the grid: the grid's width
    /// follows the screen, the tile's does not.
    tile: AsciiSprite,
    /// The tile repeated across the screen, one tile width wider than the
    /// screen so a scroll of up to a whole tile still covers it.
    grid: Grid,
    /// The layer's scroll rate: a fraction of the parade's travel.
    rate: f32,
}

/// Pure scene: grid plus fixed-step simulation state.
pub struct Scene {
    cfg: Config,
    grid: Grid,
    ground_row: i64,
    parade: Parade,
    pendulums: Vec<Pendulum>,
    /// Per-balloon-slot rope length in rows, randomized deterministically
    /// (audit addendum note 7): the big-duck carrier keeps head clearance,
    /// the small carriers start at the sketch minimum.
    ropes: Vec<f32>,
    balloons: RingBuffer<AsciiSprite>,
    /// Optional text line drawn in the bottom band below the grass (the
    /// Fix 5 `add balloon` prompt / status message), block font.
    input_line: Option<String>,
    /// How often the short-grid input-line skip was traced. The guard
    /// fires on every redraw, so the trace is emitted once per episode
    /// (audit P0-1: trace once, not per frame).
    input_skip_traces: u32,
    /// Cached balloon disc cells for the current geometry. `circle_cells`
    /// allocates a `Vec` per call and the radii are fixed for the Scene's
    /// lifetime, so the set is built once and reused on every frame (audit
    /// performance).
    disc_cells: Vec<(i64, i64, f32)>,
    /// The `(rx, ry)` the cached disc cells were built for.
    disc_radii: (f32, f32),
    /// The user-drawn sprite of every train slot, loaded once at startup.
    ///
    /// `None` means "no override": the built-in art is drawn. The length is
    /// [`DUCK_COUNT`], one entry per train slot.
    drawn_sprites: Vec<Option<DrawnSprite>>,
    /// The user-drawn balloon of every balloon slot, loaded once at
    /// startup.
    ///
    /// `None` means "no override": the watched balloon (or the empty ring)
    /// is drawn. The length is [`BALLOON_HOLDER_DUCK`].len().
    assigned_balloons: Vec<Option<AsciiSprite>>,
    /// The user-drawn grass tile, loaded once at startup.
    ///
    /// `None` means "no override": the built-in four-row band is drawn.
    grass_tile: Option<AsciiSprite>,
    /// The decoration the **big** ducks wear, loaded once at startup.
    ///
    /// `None` means "no override": the big ducks are bare. A decoration
    /// is drawn **over** the body's cells, and its blank cells are
    /// transparent, so the body shows through (roadmap Extension 2 §4).
    big_decoration: Option<DrawnSprite>,
    /// The decoration the **small** ducks wear, loaded once at startup.
    small_decoration: Option<DrawnSprite>,
    /// The content layers the scene draws (roadmap Extension 1 §7.1).
    content: ContentLayers,
    /// The parallax backdrop layers, back to front (roadmap Extension 4
    /// §1.1).
    ///
    /// The layers are **not** part of the scene grid: each one scrolls at
    /// its own rate, so each one is a grid of its own, blitted under the
    /// parade with its own offset.
    backdrops: Vec<BackdropLayer>,
    /// How far the parade has travelled, in cells.
    ///
    /// The backdrop layers scroll at a **fraction of this**, never on
    /// their own clock: that is what keeps them pixel-locked to the
    /// ducks. A whole-parade stop freezes it with the parade.
    scroll_cells: f64,
    /// The parade's march clock at the last step.
    ///
    /// The scroll is the **delta** of the march clock, so a resize — which
    /// changes the march speed — moves the layers with the parade instead
    /// of jumping them.
    ///
    /// WHY `f64`: the delta is a small number taken from two large ones,
    /// and in `f32` the difference of two hours-old clocks keeps only a
    /// millisecond of the truth — the backdrops jitter by it (audit
    /// 2026-10-21 §F5).
    last_march_clock: f64,
    /// The weather's own state: the effect, the PRNG and the fall event.
    weather: WeatherState,
    /// The sky's colour ramp, one colour per grid row.
    ///
    /// `None` means the sky is off and the background stays black.
    sky_ramp: Option<Vec<[u8; 3]>>,
    /// How long the custom message has been on screen, in seconds.
    ///
    /// WHY `f64`: the message's fade is a function of the age, and an
    /// `f32` clock drifts — the fixed step is not representable in
    /// binary, so an `f32` accumulator loses a little every step
    /// (audit 2026-10-21 §E2).
    message_age: f64,
    /// The message's own band: the five rows the block font takes.
    ///
    /// WHY the message has a band of its own: the message above the
    /// grass shares the **parade's** band, and every row of that band
    /// scrolls with the ducks — so a message in the scene's grid would
    /// jitter with them, although the layer model says the message is
    /// chrome and must not smear. The band carries the message alone,
    /// so the blitter can draw it over the scene, at offset 0 (audit
    /// 2026-10-21 §E7).
    message_band: Grid,
    /// The "days since the ducks" counter, drawn in the bottom-left.
    counter_line: Option<String>,
    /// The duck that is falling on its bum (snow), and the gait phase it
    /// froze at. `None` when no duck is falling.
    ///
    /// This is the **weather's** fall only: the mouse's stop-reaction is
    /// tracked separately (see [`Scene::stop_hold`]), so the two holds
    /// never cancel each other.
    falling: Option<(usize, f32)>,
    /// The slot the mouse's stop-reaction holds, if any.
    ///
    /// The hold lasts exactly as long as the reaction does.
    stop_hold: Option<usize>,
    /// The reaction of every train slot, if it is reacting.
    ///
    /// The length is [`DUCK_COUNT`], one entry per train slot. A `None`
    /// entry is a duck that is not reacting.
    reactions: Vec<Option<Reaction>>,
    /// The note sequencer: the scene's clock drives it (roadmap Extension
    /// 3 §3.3).
    ///
    /// The track is loaded **once**, never inside the frame path, so the
    /// UI thread never touches the filesystem while it draws.
    sequencer: Sequencer,
    /// The audio events the scene produced since the last take.
    ///
    /// The scene never touches a device: the popup forwards the events to
    /// the audio context.
    audio_events: Vec<AudioEvent>,
    /// The folder the MIDI tracks live in.
    ///
    /// Resolved once, and only when the config names a track: a parade
    /// without a track never touches the disk.
    tracks_dir: Option<std::path::PathBuf>,
    /// The simulated time, in seconds.
    ///
    /// WHY `f64`: the clock is advanced by the fixed 1/120 s step, which
    /// is not representable in binary, so an `f32` accumulator drifts —
    /// measured, **-8.4 s after an hour** — and the gait (which reads
    /// this clock) drifts with it (audit 2026-10-21 §E2).
    time: f64,
    /// The time not yet spent on a step.
    ///
    /// WHY `f64`: the same drift, and it is worse here — the carry
    /// decides **how many** steps a frame runs, so an `f32` carry made
    /// the step count depend on how the elapsed time was chunked (audit
    /// 2026-10-21 §E2).
    sim_carry: f64,
    /// The grass colour for the current time of day (audit 2026-10-23
    /// F-026): full green at noon, floored at [`GRASS_DIM_FLOOR`] at
    /// midnight. Recomputed once per configuration change, never per
    /// frame — the tint is a function of the config, not of the weather
    /// the ramp composes with.
    grass_colour: [u8; 3],
    /// The exit hint folded to what the block font can draw (audit
    /// 2026-10-23 F-037 + F-043): `hud_font::sanitize` owns the fold, and
    /// the result is cached because the per-frame `String` build was the
    /// HUD's only allocation.
    hint_text: String,
    /// The night-sky layer (audit 2026-10-23 F-025): the stars and the
    /// moon, in a grid of their own so they sit at scroll rate 0 — inked
    /// into the scene grid they would stream with the parade's sub-cell
    /// offset, and stars that move are not stars. Rebuilt on a
    /// configuration change and on a resize; empty when the sky is off or
    /// the day is lit.
    star_grid: Grid,
    /// Scratch for the weather's per-duck feet list: refilled every
    /// frame, never reallocated (audit 2026-10-23 F-041).
    feet_scratch: Vec<i64>,
}

impl Scene {
    /// The scene with the **built-in art only**: no drawn assets, no track.
    ///
    /// New scene over a `cols x rows` grid whose cells are `cell_w x cell_h`
    /// px. `cfg` is cloned. The cell must match the dims used for the Font
    /// and the framebuffer (the row-budget values from
    /// [`crate::config::Config::grid_for_monitor`]), or the blit lands every
    /// glyph outside the frame.
    ///
    /// This is the pure constructor: it touches no disk and no platform.
    /// The app uses [`Scene::new_with_assets`].
    ///
    /// The web build uses this one — it has no asset folder at all — and
    /// so do the tests. In a **library** crate a `pub` item is never dead
    /// code, so the `#[allow(dead_code)]` this attribute used to carry
    /// was a leftover that could only mask a real regression (audit
    /// 2026-10-21 §F9).
    pub fn new(cfg: &Config, cols: usize, rows: usize, cell_w: u32, cell_h: u32) -> Self {
        Self::build(cfg, cols, rows, cell_w, cell_h, no_drawn_art, None)
    }

    /// The scene with the app's own assets: the drawn art in
    /// `assets.drawn` and the MIDI track in `assets.tracks`.
    ///
    /// The drawn assets the config assigns are loaded **here**, once: the
    /// UI thread must never touch the filesystem inside `tick`.
    pub fn new_with_assets(
        cfg: &Config,
        cols: usize,
        rows: usize,
        cell_w: u32,
        cell_h: u32,
        assets: &AssetDirs,
    ) -> Self {
        Self::build(
            cfg,
            cols,
            rows,
            cell_w,
            cell_h,
            asset_loader(assets),
            assets.tracks.clone(),
        )
    }

    /// The **injectable** constructor: the asset loader and the track
    /// folder are parameters, so the core never reaches for the disk.
    ///
    /// The loader is the testability seam: production reads the user's
    /// drawn-asset folders, the tests inject a stub, so a unit test never
    /// depends on the machine it runs on. `tracks_dir` is the same seam
    /// for the MIDI tracks: the tests point it at a temp folder. The web
    /// build uses this one too, with its own in-memory asset loader.
    pub fn build(
        cfg: &Config,
        cols: usize,
        rows: usize,
        cell_w: u32,
        cell_h: u32,
        load: impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError>,
        tracks_dir: Option<std::path::PathBuf>,
    ) -> Self {
        let cols = cols.max(1);
        let rows = rows.max(1);
        let ground_row = ground_row_for(rows);
        let max = cfg.balloons.max.max(1).min(BALLOON_HOLDER_DUCK.len());
        let mut scene = Self {
            cfg: cfg.clone(),
            grid: Grid::new(cols, rows, cell_w, cell_h),
            ground_row,
            parade: Parade::new(cols, cfg.loop_seconds),
            pendulums: (0..BALLOON_HOLDER_DUCK.len())
                .map(|i| Pendulum::new(0.15 * (i as f32 + 1.0)))
                .collect(),
            ropes: (0..BALLOON_HOLDER_DUCK.len())
                .map(|i| roll_rope(&cfg.balloons, i))
                .collect(),
            balloons: RingBuffer::new(max),
            input_line: None,
            input_skip_traces: 0,
            disc_cells: Vec::new(),
            disc_radii: (0.0, 0.0),
            drawn_sprites: load_assigned_sprites(cfg, &load),
            assigned_balloons: load_assigned_balloons(cfg, &load),
            grass_tile: load_grass_tile(cfg, &load),
            big_decoration: load_decoration(cfg, &load, DrawnSize::Big),
            small_decoration: load_decoration(cfg, &load, DrawnSize::Small),
            content: ContentLayers::from_config(cfg),
            backdrops: Vec::new(),
            scroll_cells: 0.0,
            last_march_clock: 0.0,
            weather: WeatherState::new(&cfg.weather),
            sky_ramp: None,
            message_age: 0.0,
            message_band: Grid::new(cols, MESSAGE_FONT_ROWS as usize, cell_w, cell_h),
            counter_line: None,
            falling: None,
            stop_hold: None,
            reactions: (0..DUCK_COUNT).map(|_| None).collect(),
            sequencer: Sequencer::new(cfg.audio.strip_steps.max(1)),
            audio_events: Vec::new(),
            tracks_dir,
            time: 0.0,
            sim_carry: 0.0,
            grass_colour: grass_colour_for(cfg),
            hint_text: crate::hud_font::sanitize(&cfg.exit_hint),
            star_grid: Grid::new(1, 1, cell_w, cell_h),
            feet_scratch: Vec::new(),
        };
        scene.rebuild_the_sequencer(cfg);
        scene.rebuild_the_stars();
        scene
    }

    /// Set the text drawn on the input line below the grass (Fix 5); the
    /// popup owns the buffer/cursor/status text, the scene only renders it.
    pub fn set_input_line(&mut self, text: Option<String>) {
        self.input_line = text;
    }

    /// Install the parallax backdrop tiles, back to front.
    ///
    /// The tiles come from the **loader worker**: turning a picture into
    /// a tile is an image decode, and it must never run on the UI thread
    /// (the typed-balloon path blocked the parade for a second or two in
    /// exactly this shape). The scene only ever holds finished tiles.
    ///
    /// An empty list removes every layer, so a missing or broken backdrop
    /// leaves the parade exactly as it was.
    pub fn set_backdrops(&mut self, tiles: Vec<AsciiSprite>) {
        // WHY the cap: the layer model has a rate for every backdrop
        // slot, and a stack deeper than that has no rate of its own —
        // the config's repair caps the list, but the **public API** did
        // not (audit 2026-10-21 §E10).
        let capped = tiles.len().min(crate::layer::MAX_BACKDROPS);
        if capped < tiles.len() {
            crate::trace::trace(&format!(
                "scene: {} backdrop layers offered, the model has rates for {} — the rest are dropped",
                tiles.len(),
                crate::layer::MAX_BACKDROPS
            ));
        }
        let built = tiles
            .iter()
            .take(capped)
            .enumerate()
            .map(|(index, tile)| self.build_backdrop(tile, index))
            .collect();
        self.backdrops = built;
        crate::trace::trace(&format!(
            "scene: {} {} layers installed, {}",
            self.backdrops.len(),
            crate::layer::LayerKind::Backdrop.as_str(),
            self.installed_layers()
        ));
    }

    /// The layers the scene actually draws, with their rates.
    ///
    /// WHY: the trace line used to print the **fixed** stack, which
    /// names one backdrop — while the scene installs up to
    /// [`crate::layer::MAX_BACKDROPS`] of them, each at its own rate. A
    /// trace line that does not describe what was installed is a trace
    /// line that lies (audit 2026-10-21 §F19).
    fn installed_layers(&self) -> String {
        let mut parts: Vec<String> = self
            .backdrops
            .iter()
            .map(|layer| {
                format!(
                    "{}@{}",
                    crate::layer::LayerKind::Backdrop.as_str(),
                    layer.rate
                )
            })
            .collect();
        parts.push(format!(
            "{}@{}",
            crate::layer::LayerKind::Grass.as_str(),
            crate::layer::GRASS_RATE
        ));
        parts.push(format!(
            "{}@{}",
            crate::layer::LayerKind::Parade.as_str(),
            crate::layer::PARADE_RATE
        ));
        parts.push(format!(
            "{}@{}",
            crate::layer::LayerKind::Hud.as_str(),
            crate::layer::HUD_RATE
        ));
        parts.join(",")
    }

    /// Build one layer's grid from its tile.
    fn build_backdrop(&self, tile: &AsciiSprite, index: usize) -> BackdropLayer {
        let rows = tile.rows().min(self.ground_row.max(1) as usize);
        let grid = crate::layer::tile_grid(
            tile,
            self.grid.cols(),
            rows,
            self.grid.cell_w(),
            self.grid.cell_h(),
        );
        BackdropLayer {
            tile: tile.clone(),
            grid,
            rate: crate::layer::backdrop_rate(index),
        }
    }

    /// The backdrop layers: the tile, where its band sits and how fast it
    /// scrolls.
    ///
    /// The bands are **stacked**: the farthest layer sits highest and the
    /// nearest one just above the grass, so every layer is visible and
    /// the depth reads. A band that does not fit the sky region is
    /// clipped, never drawn over the grass.
    pub fn backdrop_layers(&self) -> Vec<crate::layer::BackdropInfo<'_>> {
        let ground = self.ground_row.max(0) as usize;
        let heights: Vec<usize> = self.backdrops.iter().map(|l| l.grid.rows()).collect();
        self.backdrops
            .iter()
            .enumerate()
            .map(|(index, layer)| {
                let rows = heights[index].min(ground);
                let below: usize = heights[index + 1..].iter().sum();
                let bottom = ground.saturating_sub(below);
                crate::layer::BackdropInfo {
                    tile: &layer.tile,
                    row_offset: bottom.saturating_sub(rows),
                    rows,
                    rate: layer.rate,
                }
            })
            .collect()
    }

    /// The backdrop layers' scroll offsets, in pixels.
    pub fn backdrop_offsets(&self) -> Vec<i32> {
        self.backdrops
            .iter()
            .map(|layer| {
                crate::layer::layer_offset_px(
                    layer.rate,
                    self.scroll_cells,
                    self.grid.cell_w(),
                    layer.tile.cols().max(1),
                )
            })
            .collect()
    }

    /// The backdrop layers' blits, back to front.
    ///
    /// The list opens with the night-sky layer when it is active (audit
    /// 2026-10-23 F-025): the stars and the moon are the farthest thing
    /// back, and their layer scrolls at rate 0 — inked into the scene
    /// grid they would stream with the parade's sub-cell offset, and
    /// stars that move are not stars.
    ///
    /// The returned Vec is one allocation per frame, on purpose (audit
    /// 2026-10-23 F-041): the list is bounded at
    /// [`crate::layer::MAX_BACKDROPS`] + 1 pointer-wide entries, and the
    /// caller holds it across the blit, so a scene-side scratch buffer
    /// could not be handed out without a `&mut` the callers do not have.
    pub fn backdrop_blits(&self) -> Vec<crate::layer::LayerBlit<'_>> {
        let mut blits = self.star_layer_blit();
        let offsets = self.backdrop_offsets();
        let infos = self.backdrop_layers();
        blits.extend(
            self.backdrops
                .iter()
                .zip(infos.iter())
                .zip(offsets.iter())
                .map(|((layer, info), offset)| crate::layer::LayerBlit {
                    grid: &layer.grid,
                    row_offset: info.row_offset,
                    rows: info.rows,
                    offset_px: *offset,
                }),
        );
        blits
    }

    /// The night-sky layer's blit, if the night sky is active.
    fn star_layer_blit(&self) -> Vec<crate::layer::LayerBlit<'_>> {
        if !self.star_field_active() {
            return Vec::new();
        }
        vec![crate::layer::LayerBlit {
            grid: &self.star_grid,
            row_offset: 0,
            rows: self.star_grid.rows(),
            offset_px: 0,
        }]
    }

    /// Apply a reloaded configuration.
    ///
    /// The scalars take effect at once; the content (the grass tile, the
    /// drawn assignments) is re-resolved; the geometry is **not** touched
    /// here — a monitor change goes through the popup's resize path, so
    /// the scene never has to re-derive the grid itself.
    ///
    /// A reload must never take the parade down: the caller only calls
    /// this with a configuration that already passed the repair pass, so
    /// every value here is clamped.
    ///
    /// `assets` is where the drawn art and the MIDI tracks live. It is a
    /// **parameter**, like in [`Scene::new_with_assets`]: the scene never
    /// looks a folder up, so the shell resolves the platform's folders
    /// and hands them over.
    pub fn apply_config(&mut self, cfg: &Config, assets: &AssetDirs) {
        self.apply_config_with(cfg, asset_loader(assets), assets.tracks.clone());
    }

    /// [`Scene::apply_config`] with an injectable asset loader.
    ///
    /// WHY it is public: it is the **in-place** reload path — the scene
    /// keeps its parade phase, its balloons and its clock, so a reload
    /// does not restart the parade. A host that only has an asset
    /// loader (the web, with its in-memory store) would otherwise have
    /// to build a whole new scene, and the parade would jump back to
    /// the left edge (audit 2026-10-21 §E8).
    pub fn apply_config_with(
        &mut self,
        cfg: &Config,
        load: impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError>,
        tracks_dir: Option<std::path::PathBuf>,
    ) {
        let old_message = self.cfg.message.text.clone();
        let ropes_changed = self.rope_bounds_changed(cfg);
        self.cfg = cfg.clone();
        // The per-config look caches (audit 2026-10-23 F-026/F-037/F-043):
        // the grass tint and the folded hint are functions of the
        // configuration alone, so they are recomputed here and never
        // inside the frame path. (The star field is rebuilt below, after
        // the content registry it reads has been refreshed.)
        self.grass_colour = grass_colour_for(cfg);
        self.hint_text = crate::hud_font::sanitize(&cfg.exit_hint);
        self.parade.set_loop_seconds(cfg.loop_seconds);
        if ropes_changed {
            self.ropes = (0..BALLOON_HOLDER_DUCK.len())
                .map(|i| roll_rope(&cfg.balloons, i))
                .collect();
        }
        // The disc radii follow the config, so the cached cells are stale.
        self.disc_cells = Vec::new();
        self.disc_radii = (0.0, 0.0);
        self.drawn_sprites = load_assigned_sprites(cfg, &load);
        self.assigned_balloons = load_assigned_balloons(cfg, &load);
        self.grass_tile = load_grass_tile(cfg, &load);
        self.big_decoration = load_decoration(cfg, &load, DrawnSize::Big);
        self.small_decoration = load_decoration(cfg, &load, DrawnSize::Small);
        self.content = ContentLayers::from_config(cfg);
        // The star field reads the content registry, so it is rebuilt
        // once the registry reflects the new configuration.
        self.rebuild_the_stars();
        self.weather = WeatherState::new(&cfg.weather);
        // The track folder is a parameter: the scene never looks it up.
        if cfg.audio.track.is_some() && self.tracks_dir.is_none() {
            self.tracks_dir = tracks_dir;
        }
        // The track is read here, never in the frame path.
        self.rebuild_the_sequencer(cfg);
        // A new message starts its own clock.
        if cfg.message.text != old_message {
            self.message_age = 0.0;
        }
        self.trace_the_layers();
    }

    /// Rebuild the sequencer for a (re)loaded configuration.
    ///
    /// The strip's length is the config's and the track is the file the
    /// config names. A missing or malformed track is **not fatal**: it is
    /// traced and the incumbent track keeps playing (roadmap Extension 3
    /// §3.4).
    fn rebuild_the_sequencer(&mut self, cfg: &Config) {
        let strip = cfg.audio.strip_steps.max(1);
        if self.sequencer.strip_steps() != strip {
            // The old track travels with the strip rebuild (audit
            // 2026-10-23 F-023): the old sequencer used to be dropped
            // wholesale, and a reload that changed `strip_steps` while the
            // track file was broken then left the new strip **silent** —
            // the music died on a strip resize, not on the file.
            let old_track = self.sequencer.track().clone();
            let mut sequencer = Sequencer::new(strip);
            // A reload must not replay the strip from the top: the pending
            // range is swallowed before the new track is set.
            let _ = sequencer.advance_to(self.gait_step());
            sequencer.set_track(old_track);
            self.sequencer = sequencer;
        }
        self.trace_the_strip_length(strip);
        let Some(name) = cfg.audio.track.as_deref() else {
            self.sequencer.set_track(Track::new());
            return;
        };
        // The load failed and was traced: the incumbent track (the one
        // carried across a strip rebuild, or the previously loaded
        // one) keeps playing — a broken file must not stop the music.
        if let Some(track) = self.load_the_track(name) {
            self.sequencer.set_track(track);
            let notes = self.sequencer.track().len();
            crate::trace::trace(&format!(
                "scene.track: {name:?} loaded, {notes} note(s) on a {strip}-step strip"
            ));
        }
    }

    /// Tell the user when the strip is not the length of the crossing
    /// window.
    ///
    /// The strip's last slot is the moment the trailing duck clears the
    /// left edge, and that moment depends on the monitor's width: a strip
    /// that is longer than the window repeats before the train is out, and
    /// a shorter one is cut short. The window is computed from the live
    /// grid, so the warning is per monitor.
    fn trace_the_strip_length(&self, strip: usize) {
        let window = crate::audio::sequencer::crossing_window_steps(
            self.grid.cols(),
            self.cfg.loop_seconds,
            self.parade.train_len(),
        );
        if window == 0 || window == strip {
            return;
        }
        crate::trace::trace(&format!(
            "scene.strip: {strip} steps configured, the crossing window is {window} on this monitor"
        ));
    }

    /// Read the track the config names.
    ///
    /// The read happens **here** — at construction or at a reload — and
    /// never inside the frame path: the UI thread must never touch the
    /// filesystem while it draws.
    fn load_the_track(&self, name: &str) -> Option<Track> {
        let Some(dir) = self.tracks_dir.as_deref() else {
            crate::trace::trace(&format!(
                "scene: the track {name:?} is ignored: no track folder"
            ));
            return None;
        };
        match crate::audio::sequencer::load_track(dir, name) {
            Ok(track) => Some(track),
            Err(e) => {
                crate::trace::trace(&format!("scene: the track {name:?} is ignored: {e}"));
                None
            }
        }
    }

    /// Trace the content layers and the weather after a reload.
    ///
    /// The trace is how the user sees what a reload actually did: the
    /// layers that are on and the effect that runs.
    fn trace_the_layers(&self) {
        let layers: Vec<&str> = self
            .content
            .visible()
            .iter()
            .map(|kind| kind.as_str())
            .collect();
        crate::trace::trace(&format!(
            "scene.layers: [{}] weather={} effect_time={:.2}",
            layers.join(", "),
            self.weather.current().as_str(),
            self.weather.effect_time()
        ));
    }

    /// True when the reload changes the rope length bounds.
    fn rope_bounds_changed(&self, cfg: &Config) -> bool {
        let old = &self.cfg.balloons;
        let new = &cfg.balloons;
        old.rope_min_small != new.rope_min_small
            || old.rope_min_big != new.rope_min_big
            || old.rope_max != new.rope_max
    }

    /// The sky's colour ramp, one colour per grid row.
    ///
    /// `None` when the sky is off: the background stays black.
    pub fn sky_ramp(&self) -> Option<&[[u8; 3]]> {
        self.sky_ramp.as_deref()
    }

    /// Re-lay the scene on a new grid, keeping the simulation.
    ///
    /// WHY the split: the GRID carries the cell pitch, so a new cell size
    /// genuinely needs a new grid; the SIMULATION does not depend on the
    /// cell pitch and must survive, or a window resize restarts the whole
    /// scene — the train jumps back to the left edge, the balloons vanish
    /// and the clock restarts. So the grid is rebuilt here and the
    /// simulation is preserved: the parade is re-based (its phase), the
    /// pendulums keep their phases, the rope lengths and the balloon ring
    /// stay, and the clock (`time` / `sim_carry`) does not jump.
    ///
    /// The disc-cell cache is dropped: the balloon radii are derived from
    /// the cell pitch (`balloon_radii_cells`), so the cached cells are
    /// stale after a cell-size change and the next frame rebuilds them.
    pub fn resize(&mut self, cols: usize, rows: usize, cell_w: u32, cell_h: u32) {
        let cols = cols.max(1);
        let rows = rows.max(1);
        self.grid = Grid::new(cols, rows, cell_w, cell_h);
        // The message's band follows the new width.
        self.message_band = Grid::new(cols, MESSAGE_FONT_ROWS as usize, cell_w, cell_h);
        // The grass band is a fraction of the row count, so it moves with
        // the new height.
        self.ground_row = ground_row_for(rows);
        // The parade is re-based, not restarted: its phase survives.
        self.parade.resize(cols);
        // The disc radii follow the cell pitch, so the cached disc cells
        // are stale: drop them and let the next frame rebuild the set.
        self.disc_cells = Vec::new();
        self.disc_radii = (0.0, 0.0);
        // The new grid may again be too short for the input line: that
        // deserves its own one-shot trace.
        self.input_skip_traces = 0;
        // The star field follows the new width and the new sky height.
        self.rebuild_the_stars();
        // The backdrop grids follow the screen's width, so they are stale
        // now: rebuild them from the tiles they were built from.
        self.rebuild_the_backdrops();
    }

    /// Rebuild the backdrop layers' grids for the current geometry.
    fn rebuild_the_backdrops(&mut self) {
        let tiles: Vec<AsciiSprite> = self.backdrops.iter().map(|l| l.tile.clone()).collect();
        self.backdrops = tiles
            .iter()
            .enumerate()
            .map(|(index, tile)| self.build_backdrop(tile, index))
            .collect();
    }

    /// Grid width in cells (test helper).
    #[cfg(any(test, feature = "test-support"))]
    pub fn cols(&self) -> usize {
        self.grid.cols()
    }

    /// Grid height in cells.
    pub fn rows(&self) -> usize {
        self.grid.rows()
    }

    /// Grass line: `rows * 4 / 5` (the band's first row; the wave, blade
    /// and soil rows sit below it, see [`BUILTIN_GRASS_ROWS`]).
    pub fn ground_row(&self) -> i64 {
        self.ground_row
    }

    /// Advance the simulation on a fixed timestep and redraw the grid.
    ///
    /// A `dt` larger than [`MAX_ADVANCE_SECONDS`] (600 s) is **capped**:
    /// at most that much is simulated and the leftover carry is dropped,
    /// so a clock jump cannot pin the UI for minutes of catch-up (audit
    /// 2026-10-23 F-033 — the old doc claimed the span was "simulated in
    /// full"). Non-finite or non-positive input is ignored; the grid is
    /// still redrawn.
    pub fn advance(&mut self, dt_seconds: f32) {
        if !dt_seconds.is_finite() || dt_seconds <= 0.0 {
            self.redraw();
            return;
        }
        self.sim_carry += dt_seconds as f64;
        let mut guard = 0u32;
        while self.sim_carry >= SIM_STEP_SECONDS as f64 && guard < MAX_STEPS_PER_ADVANCE {
            self.sim_carry -= SIM_STEP_SECONDS as f64;
            guard += 1;
            self.step(SIM_STEP_SECONDS);
        }
        if self.sim_carry >= SIM_STEP_SECONDS as f64 {
            self.sim_carry = 0.0;
        }
        self.redraw();
    }

    /// Simulated seconds since construction (test helper).
    #[cfg(any(test, feature = "test-support"))]
    pub fn elapsed(&self) -> f32 {
        self.time as f32
    }

    /// The parade's phase: the head's fraction through the crossing cycle
    /// in `[0, 1)` (test helper).
    ///
    /// Test-only accessor: production reads the parade through
    /// [`Scene::sub_cell_offset_px`] and the draw path (audit dead-code
    /// sweep — no production caller).
    #[cfg(any(test, feature = "test-support"))]
    pub fn parade_phase(&self) -> f32 {
        self.parade.phase()
    }

    /// The parallax accumulator in cells (test helper) — the exact f64
    /// the scroll math maintains (audit 101126 §C10).
    #[cfg(any(test, feature = "test-support"))]
    pub fn scroll_cells(&self) -> f64 {
        self.scroll_cells
    }

    /// The march speed in cells per second (test helper) — the same
    /// number the scroll accumulator multiplies (audit 101126 §C10).
    #[cfg(any(test, feature = "test-support"))]
    pub fn march_speed(&self) -> f32 {
        self.parade.speed()
    }

    /// Insert an animal into the fixed-slot ring buffer; returns the slot
    /// written.
    pub fn push_balloon(&mut self, sprite: AsciiSprite) -> usize {
        let slot = self.balloons.push(sprite);
        // A new balloon squeaks (roadmap Extension 3 §4.1).
        self.audio_events.push(AudioEvent::Sfx(SfxId::Squeak));
        self.redraw();
        slot
    }

    /// Number of filled balloon slots (animals only; empty discs excluded).
    #[cfg(any(test, feature = "test-support"))]
    pub fn balloon_count(&self) -> usize {
        self.balloons.len()
    }

    /// The composed grid, redrawn by the last [`Scene::advance`].
    pub fn grid(&self) -> &Grid {
        &self.grid
    }

    /// Whole-scene sub-cell scroll offset in px, from the parade.
    pub fn sub_cell_offset_px(&self) -> i32 {
        self.parade.sub_cell_offset_px(self.grid.cell_w())
    }

    /// The train slot of the duck under the given grid cell, if any.
    ///
    /// The test is the duck's **body box** (the frame width x the frame
    /// height at the duck's ground row), so the user clicks the duck they
    /// see. `None` when the cell is empty ground.
    pub fn duck_at(&self, col: i64, row: i64) -> Option<usize> {
        (0..DUCK_COUNT).find(|&slot| self.duck_covers(slot, col, row))
    }

    /// True when the cell is inside the duck's body box.
    fn duck_covers(&self, slot: usize, col: i64, row: i64) -> bool {
        let box_ = self.duck_box(slot);
        let inside_columns = col >= box_.left && col < box_.left + box_.cols;
        let inside_rows = row >= box_.top && row < box_.top + box_.rows;
        inside_columns && inside_rows
    }

    /// Start a reaction on a duck: it jumps (or stops), holds its frame,
    /// flashes and clucks.
    ///
    /// A slot outside the train is ignored: a click on empty ground must
    /// never touch the parade.
    ///
    /// The incumbent reaction is **ended** before the new one starts: a
    /// Stop that a Jump replaces used to leave the scene's `stop_hold`
    /// pointing at the slot forever — the replacement reaction expired,
    /// the hold never did, and the train stood still for the rest of the
    /// parade (audit 2026-10-23 F-021).
    pub fn react(&mut self, slot: usize, kind: ReactionKind) {
        if slot >= DUCK_COUNT {
            return;
        }
        if let Some(old) = self.reactions[slot] {
            self.end_the_reaction(slot, old.kind());
        }
        let phase = self.current_gait_phase(slot);
        self.reactions[slot] = Some(Reaction::start(kind, phase));
        // The duck's own voice (roadmap Extension 3 §5.3), and the chirp
        // of the hop itself (§4.1: "Chirp | a duck jumps"). Both go
        // through the effect's own cooldown, so a burst of clicks cannot
        // machine-gun the device.
        self.audio_events.push(AudioEvent::Sfx(SfxId::Cluck));
        if kind == ReactionKind::Jump {
            self.audio_events.push(AudioEvent::Sfx(SfxId::Chirp));
        }
        if kind == ReactionKind::Stop {
            self.stop_hold = Some(slot);
        }
    }

    /// The audio events the scene produced since the last call. The popup
    /// forwards them to the audio context; the scene never touches a
    /// device.
    pub fn take_audio_events(&mut self) -> Vec<AudioEvent> {
        std::mem::take(&mut self.audio_events)
    }

    /// The sprite-animation step index — the sequencer's clock.
    ///
    /// One step is one sprite frame (the gait runs at 6 frames a second),
    /// so a note on the strip fires on the step it sits on.
    pub fn gait_step(&self) -> usize {
        let step = self.time * GAIT_FPS as f64;
        if step.is_finite() && step > 0.0 {
            step as usize
        } else {
            0
        }
    }

    /// Advance every reaction by the fixed step.
    ///
    /// The reaction lives on the scene's fixed 1/120 s step, so the curve
    /// is the same whatever the frame rate (roadmap Extension 3 §5.6). A
    /// reaction that ends is cleared, and a stop-reaction releases the
    /// hold it was holding.
    fn advance_the_reactions(&mut self, h: f32) {
        for slot in 0..DUCK_COUNT {
            let Some(mut reaction) = self.reactions[slot] else {
                continue;
            };
            reaction.advance(h);
            if reaction.is_active() {
                self.reactions[slot] = Some(reaction);
                continue;
            }
            let kind = reaction.kind();
            self.reactions[slot] = None;
            self.end_the_reaction(slot, kind);
        }
    }

    /// Release what a finished reaction was holding.
    fn end_the_reaction(&mut self, slot: usize, kind: ReactionKind) {
        if kind == ReactionKind::Stop && self.stop_hold == Some(slot) {
            self.stop_hold = None;
        }
    }

    /// The reaction of a slot, if it is reacting.
    fn reaction_of(&self, slot: usize) -> Option<&Reaction> {
        self.reactions.get(slot).and_then(|entry| entry.as_ref())
    }

    /// The gait phase a slot's **frame** follows: the reaction's frozen
    /// phase while it reacts, the live phase otherwise.
    ///
    /// The frozen phase is what reads as a changed face: the duck stops
    /// cycling through its walk frames.
    fn frame_phase(&self, slot: usize) -> f32 {
        let frozen = self
            .reaction_of(slot)
            .map(|reaction| reaction.frozen_phase());
        frozen.unwrap_or_else(|| self.current_gait_phase(slot))
    }

    /// The gait phase a slot shows right now: the live phase, or the
    /// frozen one while the duck is falling on its bum.
    fn current_gait_phase(&self, slot: usize) -> f32 {
        let frozen = self
            .falling
            .filter(|(falling, _)| *falling == slot)
            .map(|(_, phase)| phase);
        frozen.unwrap_or_else(|| self.live_gait_phase(slot))
    }

    /// The live gait phase of a slot, personality included.
    fn live_gait_phase(&self, slot: usize) -> f32 {
        let personality = self.cfg.personality_for(slot);
        let gait_speed = personality.map_or(1.0, |p| p.gait_speed);
        let phase_offset = personality.map_or(0, |p| p.phase_offset);
        self.gait_phase(slot, gait_speed, phase_offset)
    }

    /// The reaction's vertical offset in cells: negative lifts the duck.
    fn reaction_offset_cells(&self, slot: usize) -> f32 {
        self.reaction_of(slot)
            .map_or(0.0, |reaction| reaction.offset_cells())
    }

    /// The reaction's vertical offset in rows: negative lifts the duck.
    fn reaction_offset(&self, slot: usize) -> i64 {
        self.reaction_offset_cells(slot).round() as i64
    }

    /// The landing squash of a slot, `0.0..=1.0`.
    fn reaction_squash(&self, slot: usize) -> f32 {
        self.reaction_of(slot)
            .map_or(0.0, |reaction| reaction.squash())
    }

    /// The colour flash of a slot, `0.0..=1.0`.
    fn reaction_flash(&self, slot: usize) -> f32 {
        self.reaction_of(slot)
            .map_or(0.0, |reaction| reaction.flash())
    }

    /// The parallax layers' scroll: the distance the parade has travelled.
    ///
    /// The accumulator is the parade's **own** travel, never a second
    /// clock: a layer at a quarter of the parade's rate therefore scrolls
    /// a quarter as fast and stays pixel-locked to the ducks, which is
    /// what makes the depth read. A whole-parade stop freezes it with
    /// the parade, and so does the weather's lightning.
    ///
    /// The distance is taken from the **march** clock, so a stopped
    /// parade does not advance it — and the *delta* is used rather than
    /// the total, so a resize (which changes the march speed) does not
    /// jump the layers.
    fn advance_the_scroll(&mut self) {
        let now = self.parade.march_clock_f64();
        let marched = now - self.last_march_clock;
        self.last_march_clock = now;
        if marched <= 0.0 || !marched.is_finite() {
            return;
        }
        self.scroll_cells += self.parade.speed() as f64 * marched;
    }

    fn step(&mut self, h: f32) {
        self.time += h as f64;
        self.message_age += h as f64;
        // The reactions run on the fixed step, so a reaction is the same
        // whatever the frame rate (roadmap Extension 3 §5.6). The hold is
        // resolved **before** the parade moves, so a duck that stops is
        // stopped on the very step it stops.
        self.advance_the_reactions(h);
        self.sync_the_hold();
        self.parade.advance(h);
        self.advance_the_scroll();
        self.weather.advance(h);
        self.sync_the_weather();
        self.advance_the_sequencer();
        for (i, p) in self.pendulums.iter_mut().enumerate() {
            p.advance(h, &self.cfg.balloons, self.ropes[i]);
        }
    }

    /// The sequencer's clock is the scene's clock (roadmap Extension 3
    /// §1): one strip slot is one sprite step, so the notes cannot drift
    /// away from the animation.
    fn advance_the_sequencer(&mut self) {
        let notes = self.sequencer.advance_to(self.gait_step());
        for note in notes {
            self.audio_events.push(AudioEvent::Note {
                pitch: note.pitch,
                velocity: note.velocity,
                duration_seconds: note.duration_steps as f32 * crate::gait::GAIT_STEP_SECONDS,
            });
        }
    }

    /// Keep the parade in step with the weather.
    ///
    /// The snow's falling duck is **held**: it stops, and the ducks behind
    /// it stop too (the hold model in [`crate::parade`]). The lightning
    /// freezes the whole parade's translation — and its strike is the
    /// thunder the audio side plays.
    fn sync_the_weather(&mut self) {
        let fallen = self.weather.fallen_duck();
        self.sync_the_fall(fallen);
        let stopped = self.weather.parade_stopped();
        let was_stopped = self.parade.is_stopped();
        if stopped != was_stopped {
            crate::trace::trace(&format!(
                "scene.parade {}",
                if stopped { "stopped" } else { "moving" }
            ));
        }
        // The strike is the edge **into** the stop: a lightning that
        // pulses every four seconds thunders once per pulse.
        if stopped && !was_stopped {
            self.audio_events.push(AudioEvent::Sfx(SfxId::Thunder));
        }
        self.parade.set_stopped(stopped);
    }

    /// Keep the scene's view of the weather's falling duck.
    ///
    /// The **hold** is not decided here: [`Scene::sync_the_hold`] owns it,
    /// so the weather's fall and the mouse's stop-reaction can never
    /// cancel each other.
    fn sync_the_fall(&mut self, fallen: Option<usize>) {
        let was_falling = self.falling.is_some();
        self.falling = self.next_fall(fallen);
        // The duck lands: the fall ends, and the splosh is the sound of it.
        if was_falling && fallen.is_none() {
            self.audio_events.push(AudioEvent::Sfx(SfxId::Splosh));
        }
    }

    /// The weather's falling duck and the gait phase it froze at.
    ///
    /// The phase is kept while the same duck keeps falling and taken
    /// fresh when the weather picks another one.
    fn next_fall(&self, fallen: Option<usize>) -> Option<(usize, f32)> {
        let slot = fallen?;
        let frozen = self
            .falling
            .filter(|(held, _)| *held == slot)
            .map(|(_, phase)| phase);
        Some((
            slot,
            frozen.unwrap_or_else(|| self.gait_phase(slot, 1.0, 0)),
        ))
    }

    /// The effective hold of the parade.
    ///
    /// Two things can hold the train: the weather's falling duck and the
    /// mouse's stop-reaction. The weather wins — it is the one the user
    /// did not ask for — and the mouse's hold is released the moment its
    /// reaction ends. The hold is **not** decided by the weather's own
    /// sync: a weather that stops reporting a fall must not cancel a
    /// mouse hold.
    fn sync_the_hold(&mut self) {
        let target = self.falling.map(|(slot, _)| slot).or(self.stop_hold);
        match (target, self.parade.held_slot()) {
            (Some(slot), Some(held)) if held == slot => {}
            (Some(slot), _) => self.parade.hold(slot),
            (None, Some(_)) => self.parade.release(),
            (None, None) => {}
        }
    }

    /// The draw order.
    ///
    /// The weather inserts itself in three places: the particles behind
    /// everything, the grass overrides inside the grass band, and the
    /// splashes on the ground under the ducks (roadmap Extension 1 §5.7).
    ///
    /// The ramp is refreshed **first** (audit 2026-10-23 F-034): the
    /// message's ink picks its colour from the row it sits on, and a ramp
    /// refreshed at the end of the draw left that choice one frame stale.
    fn redraw(&mut self) {
        self.refresh_sky_ramp();
        self.grid.clear();
        self.draw_weather_particles();
        self.draw_grass();
        self.draw_ducks();
        self.draw_balloons();
        self.draw_ropes();
        self.draw_message();
        self.draw_hud();
        self.draw_counter();
        self.draw_input_line();
    }

    /// The sky's colour ramp for this frame.
    ///
    /// The ramp is a pure function of the time of day, and the weather
    /// composes with it: a storm darkens it and the lightning flash
    /// brightens it.
    fn refresh_sky_ramp(&mut self) {
        if !self.content.is_visible(ContentKind::Sky) {
            self.sky_ramp = None;
            return;
        }
        let mut ramp = sky::ramp(self.cfg.sky.time_of_day, self.grid.rows(), self.ground_row);
        let ground = self.ground_row.max(0) as usize;
        sky::apply_weather(
            &mut ramp,
            self.weather.current(),
            self.weather.flash_active(),
            ground,
        );
        self.sky_ramp = Some(ramp);
    }

    /// Rebuild the night-sky layer: the stars and the moon (audit
    /// 2026-10-23 F-025).
    ///
    /// The field is a pure function of the grid's geometry and the sky's
    /// configuration, so the same config and the same size always produce
    /// the same layer — byte-identical renders, and the selftest gate
    /// stays green. Called on a configuration change and on a resize,
    /// never inside the frame path.
    fn rebuild_the_stars(&mut self) {
        let cols = self.grid.cols();
        let sky_rows = (self.ground_row.max(0) as usize).max(1);
        self.star_grid = Grid::new(cols, sky_rows, self.grid.cell_w(), self.grid.cell_h());
        if !self.star_field_active() {
            return;
        }
        let nightness = sky::nightness(self.cfg.sky.time_of_day);
        draw_star_field(&mut self.star_grid, nightness);
        draw_the_moon(&mut self.star_grid, nightness);
    }

    /// True when the night sky draws at all: the sky layer is on and the
    /// time of day is inside a night segment (some starlight to show).
    fn star_field_active(&self) -> bool {
        self.content.is_visible(ContentKind::Sky) && sky::nightness(self.cfg.sky.time_of_day) > 0.0
    }

    /// The Fix 5 input/status line in the empty bottom band, block font
    /// (audit §5: prompt row at `ground + 8`). On short grids the 5-tall
    /// font would run past the bottom edge, so the line is skipped with a
    /// trace when `rows - ground` cannot fit it (audit P1-10) — the trace
    /// fires once per episode, not once per frame (audit P0-1).
    ///
    /// On a narrow grid the line is wider than the screen, so the visible
    /// window follows the cursor: the tail of the line is shown and the
    /// head (the prompt) scrolls off (audit P1-9 nit). The text is
    /// borrowed, never cloned per frame (audit performance).
    fn draw_input_line(&mut self) {
        let Some(text) = self.input_line.as_deref() else {
            return;
        };
        let rows = self.grid.rows() as i64;
        let ground = self.ground_row;
        if !input_line_fits(rows, ground) {
            self.trace_input_line_skipped(rows, ground);
            return;
        }
        let available = input_line_available_cells(self.grid.cols());
        let window = tail_window(text, available);
        crate::hud_font::draw_text(
            &mut self.grid,
            INPUT_TEXT_COL,
            ground + INPUT_ROW_OFFSET,
            window,
            HUD_COLOR,
        );
    }

    /// Trace the short-grid input skip once per episode (audit P0-1: trace
    /// once, not per frame — the row guard fires on every redraw).
    fn trace_input_line_skipped(&mut self, rows: i64, ground: i64) {
        if self.input_skip_traces > 0 {
            return;
        }
        self.input_skip_traces += 1;
        crate::trace::trace(&format!(
            "scene.input skipped: rows {} ground {} need {}",
            rows,
            ground,
            INPUT_ROW_OFFSET + INPUT_FONT_ROWS
        ));
    }

    /// The height of the grass band, in rows.
    ///
    /// The built-in band is [`BUILTIN_GRASS_ROWS`] rows (four: the rail,
    /// the wave, the sparse blades and the soil — audit 2026-10-23
    /// F-024); a drawn tile is as tall as the user drew it. The weather's
    /// rules are written against this height, so they work on both.
    pub fn band_rows(&self) -> usize {
        self.grass_tile
            .as_ref()
            .map_or(BUILTIN_GRASS_ROWS, |tile| tile.rows().max(1))
    }

    /// The grass band, and then the weather's override of it.
    ///
    /// The override is applied **after** the band is drawn, so it works on
    /// the built-in band and on a drawn tile alike (roadmap Extension 1
    /// §10.1, AC 2). The built-in band carries the daylight tint (audit
    /// 2026-10-23 F-026); a drawn tile keeps the palette the user drew.
    fn draw_grass(&mut self) {
        let cols = self.grid.cols() as i64;
        let ground = self.ground_row;
        let band_rows = self.band_rows();
        let tile = self.grass_tile.as_ref();
        match tile {
            Some(tile) => draw_grass_tile(&mut self.grid, tile, cols, ground),
            None => draw_builtin_grass(&mut self.grid, cols, ground, self.grass_colour),
        }
        self.weather.draw_grass(&mut self.grid, ground, band_rows);
    }

    /// The weather's particles and, during a flash, the lightning bolt —
    /// both behind everything.
    fn draw_weather_particles(&mut self) {
        if !self.content.is_visible(ContentKind::Weather) {
            return;
        }
        // The ramp is refreshed before the draw (audit 2026-10-23 F-034),
        // so the drops know whether they fall across a lit sky: the rain
        // colour takes its contrast tone from it (F-028).
        let sky_on = self.sky_ramp.is_some();
        self.weather
            .draw_particles(&mut self.grid, self.time as f32, sky_on);
        // The bolt (audit 2026-10-23 F-030): behind the parade, so the
        // strike reads as sky, not as a wall in front of the ducks.
        self.weather.draw_bolt(&mut self.grid);
    }

    /// The puddles and the per-duck splashes on the ground band.
    ///
    /// The feet are collected into the scene's scratch Vec (audit
    /// 2026-10-23 F-041): the weather borrows the grid and the feet at
    /// once, so the fields are split once here instead of building a
    /// fresh `Vec` every frame.
    fn draw_weather_ground(&mut self) {
        if !self.content.is_visible(ContentKind::Weather) {
            return;
        }
        let ground = self.ground_row;
        let band_rows = self.band_rows();
        let time = self.time as f32;
        let Scene {
            grid,
            weather,
            parade,
            feet_scratch,
            ..
        } = self;
        feet_scratch.clear();
        feet_scratch.extend((0..DUCK_COUNT).map(|slot| parade.unwrapped_x(slot).floor() as i64));
        weather.draw_ground(grid, ground, band_rows, feet_scratch, time);
    }

    fn draw_ducks(&mut self) {
        for slot in 0..DUCK_COUNT {
            let sprite = self.duck_sprite(slot);
            self.draw_duck_at(slot, &sprite);
        }
        self.draw_contact_shadows();
        // The splashes are drawn after the ducks, so a splash never covers
        // a duck: the grid is one layer deep.
        self.draw_weather_ground();
    }

    /// The contact shadows (audit 2026-10-23 F-032): a one-row speckle
    /// under each duck's body centre, shifted [`SHADOW_OFFSET_COLS`]
    /// toward the march direction (the light comes from ahead of the
    /// train). The speckle skips the grass's own glyphs — it shades the
    /// gaps — and its colour is the band's own colour darkened, so it
    /// reads as shade rather than as dirt from the sky.
    fn draw_contact_shadows(&mut self) {
        let ground = self.ground_row;
        let colour = scale_colour(self.grass_colour, 0.45);
        for slot in 0..DUCK_COUNT {
            let box_ = self.duck_box(slot);
            let centre = box_.left + box_.cols / 2;
            draw_one_shadow(&mut self.grid, centre + SHADOW_OFFSET_COLS, ground, colour);
        }
    }

    /// The built-in art of a slot: the big duck for the lead slots, the
    /// small one for the rest.
    ///
    /// The big/small split has a single owner: the holder table in
    /// `crate::balloon` (audit maintainability), and the art owns its own
    /// size ([`crate::sprites`]).
    fn duck_sprite(&self, slot: usize) -> WalkSprite {
        if balloon::BIG_DUCK_SLOTS.contains(&slot) {
            big_duck()
        } else {
            small_duck()
        }
    }

    /// The duck's drawn rectangle: where its frame lands on the grid.
    ///
    /// The draw path and the hit test share it, so a click always lands on
    /// the duck the user sees — a jumping duck is hit in the air.
    fn duck_box(&self, slot: usize) -> DuckBox {
        let sprite = self.duck_sprite(slot);
        let cols = sprite.cols as i64;
        let rows = sprite.rows as i64;
        let ux = self.parade.unwrapped_x(slot);
        let left = (ux.floor() as i64) - cols / 2;
        DuckBox {
            left,
            top: self.duck_top_row(slot, rows),
            cols,
            rows,
        }
    }

    /// The row the duck's frame starts at.
    ///
    /// The bob and the hop are the personality's, the falling offset is
    /// the weather's and the reaction offset is the mouse's: the duck's
    /// body rises and falls, the train never moves.
    fn duck_top_row(&self, slot: usize, rows: i64) -> i64 {
        let personality = self.cfg.personality_for(slot);
        let bob = personality.map_or(0.0, |p| p.bob_cells());
        let hop = personality.map_or(0.0, |p| p.hop);
        let phase = self.current_gait_phase(slot);
        self.ground_row - rows
            + duck_row_offset(bob, hop, phase)
            + self.falling_offset(slot)
            + self.reaction_offset(slot)
    }

    /// Draw one duck: the assigned drawn art when there is one, the built-in
    /// art otherwise.
    ///
    /// The position comes from the **built-in** frame size (150 x 70 /
    /// 75 x 35) in every case, so a drawn sprite is a drop-in and the
    /// train spacing never changes (roadmap D2).
    ///
    /// The animation frame is taken modulo the **art's own** frame count: a
    /// drawn sprite with one frame stands still, a sprite with three frames
    /// cycles through all three (the old hard-coded `% 4` would have
    /// clamped a three-frame sprite).
    fn draw_duck_at(&mut self, slot: usize, sprite: &WalkSprite) {
        let personality = self.cfg.personality_for(slot);
        let box_ = self.duck_box(slot);
        let col = box_.left;
        let row = box_.top;
        // A reacting duck **holds its frame**: the frozen phase is the
        // gait phase it had when the reaction started.
        let phase = self.frame_phase(slot);
        let squash = self.reaction_squash(slot);
        let drawn = self.drawn_sprites.get(slot).and_then(|art| art.as_ref());
        // The body's frame: the decoration steps in step with it, so
        // the two are drawn with the **same** frame index (roadmap
        // Extension 2 §4.1).
        let frame = match drawn {
            Some(art) => {
                let frame = frame_of(phase, art.frame_count());
                // A drawn asset may bring its own face: the reaction frame
                // replaces the walking frame (roadmap Extension 3 §5.3).
                if !art.draw_reaction(&mut self.grid, col, row) {
                    draw_drawn_frame(art, &mut self.grid, col, row, frame, squash);
                }
                frame
            }
            None => {
                let frame = frame_of(phase, sprite.frame_count());
                let base = personality.and_then(|p| p.color).unwrap_or(DUCK_COLOR);
                // The reaction's flash: the duck flares towards white.
                let color = lerp_colour(base, FLASH_COLOR, self.reaction_flash(slot));
                draw_walk_frame(sprite, &mut self.grid, col, row, frame, color, squash);
                frame
            }
        };
        self.draw_decoration(slot, col, row, frame);
    }

    /// Draw the decoration of the duck's size class **over** its body.
    ///
    /// The decoration is a full-size frame — the duck's own frame — so
    /// it lands on the duck's own cells. Its blank cells are
    /// transparent, so the body shows through everywhere the decoration
    /// does not cover (roadmap Extension 2 §4.1).
    fn draw_decoration(&mut self, slot: usize, col: i64, row: i64, frame: usize) {
        let big = balloon::BIG_DUCK_SLOTS.contains(&slot);
        let decoration = if big {
            self.big_decoration.as_ref()
        } else {
            self.small_decoration.as_ref()
        };
        let Some(decoration) = decoration else {
            return;
        };
        decoration.draw(&mut self.grid, col, row, frame);
    }

    /// The gait phase of a slot, in frames.
    ///
    /// The phase is a function of the simulation clock only, so a held
    /// duck keeps walking in place and the rhythm is never broken
    /// (roadmap Extension 1 §5.4, rule 3). The personality changes the
    /// **phase**, never the train's spacing.
    fn gait_phase(&self, slot: usize, gait_speed: f32, phase_offset: i32) -> f32 {
        (self.time * GAIT_FPS as f64 * gait_speed as f64 + slot as f64 + phase_offset as f64) as f32
    }

    /// The extra row a falling duck is drawn lower by.
    ///
    /// The duck **sinks** as it falls: the weather's fall progress drives
    /// the offset, so the fall reads as a fall and not as a jump.
    fn falling_offset(&self, slot: usize) -> i64 {
        if !self.parade.is_holding() {
            return 0;
        }
        match self.falling {
            Some((falling, _)) if falling == slot => self.weather.fall_progress().round() as i64,
            _ => 0,
        }
    }

    fn draw_balloons(&mut self) {
        let (rx, ry) = balloon::balloon_radii_cells(
            self.cfg.balloons.balloon_radius_rows,
            self.grid.cell_w(),
            self.grid.cell_h(),
        );
        // The disc-cell set is built once per geometry, not once per frame
        // (audit performance): `circle_cells` allocates a `Vec` per call and
        // the radii are fixed for the Scene's lifetime, so the cached set is
        // reused by all five balloons on every frame.
        let mut cells = std::mem::take(&mut self.disc_cells);
        if self.disc_radii != (rx, ry) {
            cells = balloon::circle_cells(rx, ry);
            self.disc_radii = (rx, ry);
        }
        // Sprites are borrowed, not cloned (audit performance): the helpers
        // below take `&mut Grid` instead of `&mut self`, so the sprite borrow
        // (`self.balloons`) and the grid borrow (`self.grid`) coexist on
        // disjoint fields.
        for slot in 0..BALLOON_HOLDER_DUCK.len() {
            let centre = self.balloon_centre(slot, rx, ry);
            let grid = &mut self.grid;
            // The assigned balloon wins over the watched one; the ring
            // buffer push path (the `ADD BALLOON` line and the watcher)
            // keeps working unchanged.
            let assigned = self
                .assigned_balloons
                .get(slot)
                .and_then(|art| art.as_ref());
            let watched = self.balloons.get(slot);
            match assigned.or(watched) {
                Some(sprite) => Self::draw_photo_balloon(grid, centre, rx, ry, &cells, sprite),
                None => Self::draw_empty_balloon(grid, centre, ry, &cells),
            }
        }
        self.disc_cells = cells;
    }

    fn draw_ropes(&mut self) {
        // Rope from the duck's hand/back up to the balloon: the randomized
        // rope length sets the balloon height and the whole segment between
        // balloon and pivot is filled (audit addendum note 1). Every carrier
        // slot gets its rope — placeholder discs included, as in the sketch.
        for (slot, &holder) in BALLOON_HOLDER_DUCK.iter().enumerate() {
            // The rope's lower end is the duck's **hand**, with the
            // reaction's offset: a jumping duck takes its hand with it.
            let hand = self.duck_hand(holder);
            let (rx, ry) = balloon::balloon_radii_cells(
                self.cfg.balloons.balloon_radius_rows,
                self.grid.cell_w(),
                self.grid.cell_h(),
            );
            let balloon = self.balloon_centre(slot, rx, ry);
            // Start one row below the ring (cy+ry is itself a ring cell, and
            // stop-at-ink would break on it); walk down to the duck's back.
            let below = (balloon.0, balloon.1 + ry + 1.0);
            draw_rope_stop_at_ink(&mut self.grid, below, hand, ROPE_COLOR);
        }
    }

    fn draw_hud(&mut self) {
        // Block-font hint 5 cells tall, one blank margin row below, so it
        // stays readable at the row-budget cell (audit addendum note 6).
        // The text is the cached `hud_font::sanitize` fold (audit
        // 2026-10-23 F-037 + F-043): no per-frame `String`, and no glyph
        // the block font silently drops.
        let text = self.hint_text.as_str();
        let row = self.grid.rows() as i64 - HUD_MARGIN_ROWS;
        let width = crate::hud_font::text_width(text);
        let cols = self.grid.cols() as i64;
        let start = match self.cfg.hint_corner {
            HintCorner::BottomLeft => HUD_LEFT_COL,
            HintCorner::BottomRight => (cols - width - 1).max(0),
        };
        crate::hud_font::draw_text(&mut self.grid, start, row, text, HUD_COLOR);
    }

    /// The custom message, drawn in the block font.
    ///
    /// The message is drawn **after** the ducks, so it reads as a caption
    /// over the parade rather than something behind it. The exit hint and
    /// the input line are untouched (roadmap Extension 1 §3.2, AC 4).
    fn draw_message(&mut self) {
        self.message_band.clear();
        if !self.content.is_visible(ContentKind::Message) {
            return;
        }
        let text = self.cfg.message.text.trim();
        if text.is_empty() {
            return;
        }
        let fade = self.message_fade();
        if fade <= 0.0 {
            return;
        }
        let Some((row, col)) = self.message_position(text) else {
            return;
        };
        let color = self.message_color(row, fade);
        // The message goes into its **own band**, not into the scene's
        // grid: the band above the grass scrolls with the ducks, and the
        // message is chrome (audit 2026-10-21 §E7).
        crate::hud_font::draw_text(&mut self.message_band, col, 0, text, color);
    }

    /// The message's band, for the blitter's second pass.
    ///
    /// The band is drawn **over** the scene, at offset 0: the message is
    /// chrome, and chrome must not smear (audit 2026-10-21 §E7).
    ///
    /// `None` when there is no message to draw.
    pub fn message_blit(&self) -> Option<crate::layer::LayerBlit<'_>> {
        let text = self.cfg.message.text.trim();
        if !self.content.is_visible(ContentKind::Message) || text.is_empty() {
            return None;
        }
        let (row, _) = self.message_position(text)?;
        if row < 0 {
            return None;
        }
        Some(crate::layer::LayerBlit {
            grid: &self.message_band,
            row_offset: row as usize,
            rows: self.message_band.rows(),
            offset_px: 0,
        })
    }

    /// Where the message is drawn: the font's top row and the first
    /// column. `None` when the grid cannot hold the band.
    fn message_position(&self, text: &str) -> Option<(i64, i64)> {
        let cols = self.grid.cols() as i64;
        let width = crate::hud_font::text_width(text);
        let ground = self.ground_row;
        match self.cfg.message.position {
            crate::config::MessagePosition::AboveGrass => {
                let row = ground + MESSAGE_ABOVE_OFFSET;
                if row < 0 {
                    return None;
                }
                Some((row, ((cols - width) / 2).max(0)))
            }
            crate::config::MessagePosition::BottomLeft => {
                if !message_below_fits(self.rows() as i64, ground, self.counter_line.is_some()) {
                    return None;
                }
                Some((ground + MESSAGE_BELOW_OFFSET, MESSAGE_LEFT_COL))
            }
            crate::config::MessagePosition::BottomRight => {
                if !message_below_fits(self.rows() as i64, ground, self.counter_line.is_some()) {
                    return None;
                }
                let col = (cols - width - 1).max(0);
                Some((ground + MESSAGE_BELOW_OFFSET, col))
            }
        }
    }

    /// The message's opacity, `0.0..=1.0`.
    ///
    /// The message is shown for `fade_seconds` and then fades out over one
    /// second: a message that never leaves is a message you stop seeing.
    /// `fade_seconds = 0` means "always".
    fn message_fade(&self) -> f32 {
        let fade_seconds = self.cfg.message.fade_seconds;
        if fade_seconds <= 0.0 {
            return 1.0;
        }
        let remaining = (fade_seconds + MESSAGE_FADE_SECONDS) as f64 - self.message_age;
        (remaining / MESSAGE_FADE_SECONDS as f64).clamp(0.0, 1.0) as f32
    }

    /// The message's colour, faded towards the background it sits on.
    ///
    /// The ink picks its own tone first (audit 2026-10-23 F-029): white
    /// on a dark band, deep navy on a light one — white on the day
    /// horizon was ink on paper. One luminance comparison decides, and
    /// the ramp is refreshed before the draw (F-034), so the comparison
    /// reads this frame's sky, not the previous one's.
    fn message_color(&self, row: i64, fade: f32) -> [u8; 3] {
        let background = sky::row_colour(self.sky_ramp.as_deref(), row.max(0) as usize);
        let luminance = (0.299 * background[0] as f32
            + 0.587 * background[1] as f32
            + 0.114 * background[2] as f32)
            / 255.0;
        let ink = if luminance > MESSAGE_LIGHT_BAND {
            MESSAGE_INK_DARK
        } else {
            MESSAGE_COLOR
        };
        lerp_colour(background, ink, fade)
    }

    /// The "days since the ducks" counter, in the bottom-left corner.
    ///
    /// It is off by default and it sits on its own row above the hint, so
    /// it never fights the hint or the message.
    fn draw_counter(&mut self) {
        let Some(text) = self.counter_line.as_deref() else {
            return;
        };
        let row = self.grid.rows() as i64 - COUNTER_ROW_OFFSET;
        if row < 0 {
            return;
        }
        crate::hud_font::draw_text(&mut self.grid, COUNTER_LEFT_COL, row, text, COUNTER_COLOR);
    }

    /// Set the "days since the ducks" counter, drawn bottom-left.
    ///
    /// The popup owns the stats file; the scene only renders the line.
    pub fn set_counter_line(&mut self, text: Option<String>) {
        self.counter_line = text;
    }

    /// The pendulum's pivot: the hand's **rest** position.
    ///
    /// The pivot is static by construction — the integrator assumes it —
    /// so the reaction's offset is *not* part of it (roadmap Extension 3
    /// §5.5).
    fn hand_cell(&self, duck_slot: usize) -> (f32, f32) {
        let ux = self.parade.unwrapped_x(duck_slot);
        (ux, (self.ground_row - 2) as f32)
    }

    /// The duck's hand, with the reaction's offset.
    ///
    /// A jumping duck's hand rises with it: the rope's lower end follows
    /// the duck, so the rope keeps its length (roadmap Extension 3 §5.5).
    fn duck_hand(&self, duck_slot: usize) -> (f32, f32) {
        let (x, y) = self.hand_cell(duck_slot);
        (x, y + self.reaction_offset_cells(duck_slot))
    }

    /// Balloon centre for `slot`: the rope-ruled attach point, then the
    /// grid-top guard.
    ///
    /// Precedence (audit P1-15, pinned by
    /// `top_guard_wins_over_rope_invariant`): the rope invariant rules —
    /// the RING BOTTOM hangs one rope length above the duck's hand/back
    /// (prototype semantics: `bottom = pivot - rope`), so the lowest rope
    /// still clears the duck's head by the full radius margin, and the
    /// extra `ry` lifts the centre accordingly. Only the grid-top guard
    /// overrides: when the rope math would push the disc above row 0, the
    /// centre is clamped down to `ry + 1` and the rope is allowed to go
    /// slack (shorter than `ropes[slot]`) rather than clipping the balloon.
    fn balloon_centre(&self, slot: usize, _rx: f32, ry: f32) -> (f32, f32) {
        let attach = self.balloon_attach(slot, ry);
        (attach.0, clamp_centre_top(attach.1, ry))
    }

    /// Unclamped rope attach point for `slot`: the ring-bottom invariant
    /// with no grid-top guard applied, so the tests can tell whether the
    /// guard actually fired (audit P1-15).
    ///
    /// The attach point is the pendulum's, **plus the holder's reaction
    /// offset**: a jumping duck's balloon rises with it, so the rope keeps
    /// its length.
    fn balloon_attach(&self, slot: usize, ry: f32) -> (f32, f32) {
        let holder = BALLOON_HOLDER_DUCK[slot];
        let pivot = self.hand_cell(holder);
        let (x, y) = self.pendulums[slot].attach_point(pivot, self.ropes[slot] + ry);
        (x, y + self.reaction_offset_cells(holder))
    }

    fn draw_photo_balloon(
        grid: &mut Grid,
        centre: (f32, f32),
        rx: f32,
        ry: f32,
        cells: &[(i64, i64, f32)],
        sprite: &AsciiSprite,
    ) {
        let cx = centre.0.round() as i64;
        let cy = centre.1.round() as i64;
        // One flat closure: the sprite sampling lives in `photo_pixel`, so
        // there is no closure inside a closure (audit nesting rule).
        let mut pixel_at = |dx: i64, dy: i64, _d: f32| {
            let (sx, sy) = photo_source_cell(dx, dy, rx, ry, sprite);
            photo_pixel(sprite, sx, sy)
        };
        balloon::draw_balloon_cells(grid, cx, cy, cells, &mut pixel_at);
    }

    /// The placeholder balloon, when a slot has no sprite: a three-band
    /// shaded disc (audit 2026-10-23 F-027) — the outline ring, a pale
    /// highlight arc on the upper-left and a sparse speckle across the
    /// bottom third of the interior. The old ring-only disc read as a
    /// faint dotted hoop at the row-budget cell.
    fn draw_empty_balloon(grid: &mut Grid, centre: (f32, f32), ry: f32, cells: &[(i64, i64, f32)]) {
        let cx = centre.0.round() as i64;
        let cy = centre.1.round() as i64;
        // One flat closure: the shading decision lives in
        // `empty_balloon_cell`, so there is no branch inside the closure
        // (audit nesting rule).
        let mut pixel_at = |dx: i64, dy: i64, d: f32| empty_balloon_cell(dx, dy, d, ry);
        balloon::draw_balloon_cells(grid, cx, cy, cells, &mut pixel_at);
    }
}

/// Draw a built-in frame, squashed when the duck is landing.
///
/// The plain draw is kept for the frames that do not squash, so a duck
/// that is not reacting is drawn exactly as it always was.
fn draw_walk_frame(
    sprite: &WalkSprite,
    grid: &mut Grid,
    col: i64,
    row: i64,
    frame: usize,
    color: [u8; 3],
    squash: f32,
) {
    if squash > 0.0 {
        sprite.draw_squashed(grid, col, row, frame, color, squash);
        return;
    }
    sprite.draw(grid, col, row, frame, color);
}

/// Draw a drawn frame, squashed when the duck is landing.
///
/// The plain draw is kept for the frames that do not squash, so a duck
/// that is not reacting is drawn exactly as it always was.
fn draw_drawn_frame(
    art: &DrawnSprite,
    grid: &mut Grid,
    col: i64,
    row: i64,
    frame: usize,
    squash: f32,
) {
    if squash > 0.0 {
        art.draw_squashed(grid, col, row, frame, squash);
        return;
    }
    art.draw(grid, col, row, frame);
}

/// Draw the built-in grass band: four rows (audit 2026-10-23 F-024).
///
/// The rail row and the wave row are the original pair; the third row is
/// a sparse blade row (one tuft in [`BLADE_EVERY`] columns) and the
/// fourth a soil dot row at half the band's colour, so the band reads as
/// ground rather than as a divider line. The band's bottom row is
/// `ground + 3`, still clear of the input line at `ground + 8`.
fn draw_builtin_grass(grid: &mut Grid, cols: i64, ground: i64, colour: [u8; 3]) {
    let soil = scale_colour(colour, 0.5);
    let wave_row = ground + 1;
    let blade_row = ground + 2;
    let soil_row = ground + 3;
    for c in 0..cols {
        grid.set(c, ground, grass_rail_char(c), colour);
        grid.set(c, wave_row, grass_wave_char(c), colour);
        draw_band_blade(grid, c, blade_row, colour);
        draw_soil_dot(grid, c, soil_row, soil);
    }
}

/// The sparse blade row's tuft: blank unless the column is a tuft column,
/// then one of three blade glyphs by the tuft's index.
fn draw_band_blade(grid: &mut Grid, col: i64, row: i64, colour: [u8; 3]) {
    if col.rem_euclid(BLADE_EVERY) != 0 {
        return;
    }
    let glyph = match (col / BLADE_EVERY).rem_euclid(3) {
        0 => '"',
        1 => ',',
        _ => '\'',
    };
    grid.set_if_visible(col, row, glyph, colour);
}

/// The soil row's dot: a `.` every third column, at half the band's
/// colour — dark enough to read as soil under the grass.
fn draw_soil_dot(grid: &mut Grid, col: i64, row: i64, soil: [u8; 3]) {
    if col.rem_euclid(3) != 0 {
        return;
    }
    grid.set_if_visible(col, row, '.', soil);
}

/// One duck's contact shadow (audit 2026-10-23 F-032): a three-cell
/// speckle of the rail row's gaps.
///
/// WHY the bounds guard and not the grid's own `set`: the grid **wraps**
/// columns, so the shadow of a duck that has just left the left edge
/// would otherwise reappear on the far right. An off-screen duck casts no
/// shadow.
fn draw_one_shadow(grid: &mut Grid, col: i64, row: i64, colour: [u8; 3]) {
    let cols = grid.cols() as i64;
    for step in 0..SHADOW_SPAN_COLS as i64 {
        let at = col + step;
        if at < 0 || at >= cols {
            continue;
        }
        if grid.char_at(at as usize, row as usize) != Some(' ') {
            continue;
        }
        grid.set(at, row, '`', colour);
    }
}

/// One cell of the shaded empty balloon (audit 2026-10-23 F-027): the
/// outline ring, the upper-left highlight arc and the sparse bottom-third
/// speckle. `d` is the cell's normalized distance from the disc's centre,
/// `dx`/`dy` its cell offsets.
fn empty_balloon_cell(dx: i64, dy: i64, d: f32, ry: f32) -> Option<(char, [u8; 3])> {
    if d >= RING_INNER_D {
        return Some(('.', EMPTY_BALLOON_COLOR));
    }
    if d >= HIGHLIGHT_INNER_D && dx < 0 && dy < 0 {
        return Some(('o', EMPTY_BALLOON_HIGHLIGHT));
    }
    if dy as f32 > ry / 3.0 && (dx + dy).rem_euclid(2) == 0 {
        return Some(('.', EMPTY_BALLOON_SHADE));
    }
    None
}

/// The grass colour for the configuration's time of day (audit
/// 2026-10-23 F-026).
///
/// The tint is keyed on the ramp's **presence**: a sky that is off leaves
/// the meadow its plain green, noon is unchanged (factor 1.0) and midnight
/// floors at [`GRASS_DIM_FLOOR`].
fn grass_colour_for(cfg: &Config) -> [u8; 3] {
    if !cfg.sky.enabled {
        return GRASS_COLOR;
    }
    let daylight = 1.0 - sky::nightness(cfg.sky.time_of_day);
    let factor = GRASS_DIM_FLOOR + (1.0 - GRASS_DIM_FLOOR) * daylight;
    scale_colour(GRASS_COLOR, factor)
}

/// A colour scaled by `factor` (the same rule the sky's earth row uses).
fn scale_colour(colour: [u8; 3], factor: f32) -> [u8; 3] {
    let scale = |c: u8| -> u8 { (c as f32 * factor).round().clamp(0.0, 255.0) as u8 };
    [scale(colour[0]), scale(colour[1]), scale(colour[2])]
}

/// The stars of the night sky (audit 2026-10-23 F-025): one cell in
/// [`STAR_EVERY`], placed by the `(col, row)` hash and dimmed by the
/// nightness. A pure function of the grid — no clock, no seed state — so
/// two renders of the same config are byte-identical.
fn draw_star_field(grid: &mut Grid, nightness: f32) {
    (0..grid.rows() as i64).for_each(|row| draw_star_row(grid, row, nightness));
}

/// One row of the star field.
fn draw_star_row(grid: &mut Grid, row: i64, nightness: f32) {
    (0..grid.cols() as i64).for_each(|col| {
        let hash = col * STAR_COL_MIX + row * STAR_ROW_MIX;
        if hash.rem_euclid(STAR_EVERY) != 0 {
            return;
        }
        let (glyph, bright) = star_cell(hash, nightness);
        grid.set_if_visible(col, row, glyph, scale_colour([255, 255, 255], bright));
    });
}

/// One star's glyph and brightness from its hash: a four-step brightness
/// ladder and, for one star in seven, the heavier `*` glyph.
fn star_cell(hash: i64, nightness: f32) -> (char, f32) {
    let variant = hash / STAR_EVERY;
    let step = variant.rem_euclid(4);
    let bright = nightness * (0.4 + 0.2 * step as f32);
    let glyph = if variant.rem_euclid(7) == 0 { '*' } else { '.' };
    (glyph, bright)
}

/// The moon: one fixed-position disc, dimmed by the nightness (audit
/// 2026-10-23 F-025). Fixed **relative to the grid**: a wandering moon
/// would be a second parade.
const MOON_PATTERN: [&str; 5] = [".oOo.", "oOOOo", "OOOOO", "oOOOo", ".oOo."];

/// Where the moon sits: seven tenths across, one sixth down the sky.
const MOON_COL_PART: usize = 7;
const MOON_COL_DENOM: usize = 10;
const MOON_ROW_PART: usize = 1;
const MOON_ROW_DENOM: usize = 6;

/// Draw the moon disc at its fixed position.
fn draw_the_moon(grid: &mut Grid, nightness: f32) {
    let colour = scale_colour([235, 235, 220], 0.4 + 0.6 * nightness);
    let left = grid.cols() as i64 * MOON_COL_PART as i64 / MOON_COL_DENOM as i64;
    let top = (grid.rows() / MOON_ROW_DENOM * MOON_ROW_PART) as i64;
    MOON_PATTERN
        .iter()
        .enumerate()
        .for_each(|(dy, line)| draw_moon_row(grid, line, left, top + dy as i64, colour));
}

/// One row of the moon disc.
fn draw_moon_row(grid: &mut Grid, line: &str, left: i64, row: i64, colour: [u8; 3]) {
    line.chars().enumerate().for_each(|(dx, ch)| {
        grid.set_if_visible(left + dx as i64, row, ch, colour);
    });
}

/// Draw a drawn grass tile, tiled across the full width.
///
/// The tile is **static** — the grass does not scroll with the parade —
/// so a repeating tile is exactly the right model (roadmap Extension 1
/// §10.1).
fn draw_grass_tile(grid: &mut Grid, tile: &AsciiSprite, cols: i64, ground: i64) {
    let tile_cols = tile.cols().max(1) as i64;
    for col in 0..cols {
        let source = col.rem_euclid(tile_cols);
        draw_grass_column(grid, tile, source, col, ground);
    }
}

/// Draw one column of a grass tile.
fn draw_grass_column(grid: &mut Grid, tile: &AsciiSprite, source: i64, col: i64, ground: i64) {
    for row in 0..tile.rows() {
        let Some((ch, color)) = tile.get(source as usize, row) else {
            continue;
        };
        if ch == ' ' {
            continue;
        }
        grid.set_if_visible(col, ground + row as i64, ch, color);
    }
}

/// The vertical offset of a duck: the bob and the hop.
///
/// A negative offset lifts the duck. The default (no personality) is
/// zero, so the built-in art is unchanged.
fn duck_row_offset(bob: f32, hop: f32, phase: f32) -> i64 {
    let wave = (phase * std::f32::consts::TAU / 4.0).sin();
    let hop_wave = (phase * std::f32::consts::TAU / 8.0).sin().max(0.0);
    let lift = bob * wave + hop * hop_wave;
    -(lift.round() as i64)
}

/// The animation frame of a gait phase.
fn frame_of(phase: f32, count: usize) -> usize {
    let count = count.max(1) as i64;
    let frame = phase.floor() as i64;
    frame.rem_euclid(count) as usize
}

/// True when a message below the grass fits without fighting the input
/// line, the exit hint **or the days counter**.
///
/// The band is `ground + 3 ..= ground + 7`; the input line starts at
/// `ground + 8`, the hint sits at `rows - 6`, and the counter sits at
/// `rows - 12`. On the short grids the web tier draws (rows-ground
/// 16..=19) the counter row is inside the message band, so the counter
/// is part of the check — the message used to paint straight over it
/// (audit 2026-10-22 §VIS-03). The counter's clause applies only when
/// the counter is actually shown: it is off by default, and a short grid
/// must not lose its bottom message over a row nothing occupies.
fn message_below_fits(rows: i64, ground: i64, counter_shown: bool) -> bool {
    let bottom = ground + MESSAGE_BELOW_OFFSET + MESSAGE_FONT_ROWS;
    let clear_of_chrome =
        bottom <= rows - HUD_MARGIN_ROWS && (!counter_shown || bottom <= rows - COUNTER_ROW_OFFSET);
    rows - ground > MESSAGE_BELOW_OFFSET + MESSAGE_FONT_ROWS && clear_of_chrome
}

/// Linear interpolation between two colours.
fn lerp_colour(from: [u8; 3], to: [u8; 3], t: f32) -> [u8; 3] {
    let t = t.clamp(0.0, 1.0);
    let mix = |a: u8, b: u8| -> u8 {
        let value = a as f32 + (b as f32 - a as f32) * t;
        value.round().clamp(0.0, 255.0) as u8
    };
    [
        mix(from[0], to[0]),
        mix(from[1], to[1]),
        mix(from[2], to[2]),
    ]
}

/// The loader of a scene that has **no** drawn art at all.
///
/// Every request fails with "not found": the built-in art stays, and the
/// core never touches the disk for an asset it was not given.
// The web build uses this; the binary never builds an asset-free
// scene — and in a **library** crate a `pub` item is never dead code,
// so the `#[allow(dead_code)]` this attribute used to carry could only
// mask a real regression (audit 2026-10-21 §F9).
fn no_drawn_art(_kind: DrawnKind, name: &str) -> Result<DrawnSprite, DrawnError> {
    Err(DrawnError::NotFound {
        name: name.to_string(),
    })
}

/// The loader of the drawn art in `assets`.
///
/// No drawn folder at all means every request fails: the scene shows the
/// built-in art, exactly as [`Scene::new`] does.
fn asset_loader(assets: &AssetDirs) -> impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError> {
    let drawn = assets.drawn.clone();
    move |kind, name| match &drawn {
        Some(dirs) => dirs.load(kind, name),
        None => Err(DrawnError::NotFound {
            name: name.to_string(),
        }),
    }
}

/// Load the drawn grass tile of the configuration, if it names one.
///
/// A missing or corrupt tile is **not fatal**: it is traced and the
/// built-in band stays.
fn load_grass_tile(
    cfg: &Config,
    load: &impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError>,
) -> Option<AsciiSprite> {
    let name = cfg.grass.tile.as_deref()?;
    let sprite = match load(DrawnKind::Grass, name) {
        Ok(sprite) => sprite,
        Err(e) => {
            crate::trace::trace(&format!("scene: grass tile {name:?} ignored: {e}"));
            return None;
        }
    };
    sprite.frame(0).cloned()
}

/// Load the decoration of one size class, if the config names one.
///
/// A missing or corrupt decoration is **not fatal**: it is traced and
/// the ducks stay bare (roadmap Extension 2 §4.4).
///
/// A decoration of the **wrong size** is refused as well: the big
/// decoration is stamped over the big duck's frame, and a small one
/// would sit in its top-left corner (audit 2026-10-21 §E1).
fn load_decoration(
    cfg: &Config,
    load: &impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError>,
    size: DrawnSize,
) -> Option<DrawnSprite> {
    let name = cfg.decorations.get(size)?;
    let sprite = match load(DrawnKind::Decoration, name) {
        Ok(sprite) => sprite,
        Err(e) => {
            crate::trace::trace(&format!("scene: decoration {name:?} ignored: {e}"));
            return None;
        }
    };
    if sprite.size != Some(size) {
        crate::trace::trace(&format!(
            "scene: decoration {name:?} ignored: it is a {} decoration, the slot wants a {size} one",
            size_name(sprite.size),
        ));
        return None;
    }
    Some(sprite)
}

/// The name of a size class, for the trace lines.
fn size_name(size: Option<DrawnSize>) -> &'static str {
    match size {
        Some(DrawnSize::Big) => "big",
        Some(DrawnSize::Small) => "small",
        None => "sizeless",
    }
}

/// Sprite pixel for a photo balloon: transparent cells — and out-of-range
/// reads — stay transparent, so the masked parts of the picture show the
/// background through the disc.
fn photo_pixel(sprite: &AsciiSprite, sx: usize, sy: usize) -> Option<(char, [u8; 3])> {
    let (ch, color) = sprite.get(sx, sy)?;
    (ch != ' ').then_some((ch, color))
}

/// The art cell that feeds the disc cell at `(dx, dy)`.
///
/// The art is mapped onto the disc **by normalized position**: the disc's
/// cell at `dx / rx` of the disc's width takes the art's cell at the same
/// fraction of the art's width. The art therefore keeps the shape it was
/// drawn in — a drawn balloon on the editor's disc grid (3:2 cells) stays
/// 3:2 in pixels on every monitor, instead of being stretched to the
/// disc (audit 2026-10-20 § 3.18: the square authoring canvas stretched
/// every drawn balloon 1.5x horizontally).
fn photo_source_cell(dx: i64, dy: i64, rx: f32, ry: f32, sprite: &AsciiSprite) -> (usize, usize) {
    // The `max(1)` matters twice: the **scale** must not be zero, and the
    // **clamp's upper bound** must not be below its lower bound — an
    // `i64::clamp` with `min > max` panics, and a zero-wide sprite is
    // constructible (`AsciiSprite::new(0, n)`) (audit 2026-10-21 §F9).
    let cols = sprite.cols().max(1) as f32;
    let rows = sprite.rows().max(1) as f32;
    let sx =
        (((dx as f32 / rx * 0.5 + 0.5) * cols).floor() as i64).clamp(0, cols as i64 - 1) as usize;
    let sy =
        (((dy as f32 / ry * 0.5 + 0.5) * rows).floor() as i64).clamp(0, rows as i64 - 1) as usize;
    (sx, sy)
}

/// Load the drawn sprite of every assigned train slot (once, at startup).
///
/// The user numbering is mapped onto the train slots by
/// [`crate::drawn::big_slot`] and [`crate::drawn::small_slot`], so the
/// mapping cannot drift from the parade layout.
fn load_assigned_sprites(
    cfg: &Config,
    load: &impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError>,
) -> Vec<Option<DrawnSprite>> {
    let mut out: Vec<Option<DrawnSprite>> = (0..DUCK_COUNT).map(|_| None).collect();
    for number in 1..=balloon::BIG_DUCK_SLOTS.len() {
        put_assigned_sprite(
            &mut out,
            crate::drawn::big_slot(number),
            cfg.assignments.big(number),
            DrawnSize::Big,
            load,
        );
    }
    let small_count = DUCK_COUNT - crate::parade::BIG_SLOTS;
    for number in 1..=small_count {
        put_assigned_sprite(
            &mut out,
            crate::drawn::small_slot(number),
            cfg.assignments.small(number),
            DrawnSize::Small,
            load,
        );
    }
    out
}

/// Put one assigned sprite into its train slot.
///
/// A missing or corrupt asset is **not fatal**: it is traced and the slot
/// keeps the built-in art.
///
/// An asset of the **wrong size** is refused as well: the slot's box is
/// the built-in duck's frame (150 x 70 for the big ducks, 75 x 35 for
/// the small ones), so a small sprite in a big slot would be drawn in
/// the top-left corner of the big duck's box — a broken picture, and a
/// hit test that does not match what the user sees (audit 2026-10-21
/// §E1).
fn put_assigned_sprite(
    out: &mut [Option<DrawnSprite>],
    slot: Option<usize>,
    name: Option<&str>,
    size: DrawnSize,
    load: &impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError>,
) {
    let (Some(slot), Some(name)) = (slot, name) else {
        return;
    };
    let sprite = match load(DrawnKind::Sprite, name) {
        Ok(sprite) => sprite,
        Err(e) => {
            crate::trace::trace(&format!("scene: assigned sprite {name:?} ignored: {e}"));
            return;
        }
    };
    if sprite.size != Some(size) {
        crate::trace::trace(&format!(
            "scene: assigned sprite {name:?} ignored: it is a {} sprite, the slot wants a {size} one",
            size_name(sprite.size),
        ));
        return;
    }
    let Some(entry) = out.get_mut(slot) else {
        return;
    };
    *entry = Some(sprite);
}

/// Load the drawn balloon of every assigned balloon slot (once, at
/// startup).
///
/// The drawn frame is used as it is — no conversion, no disc mask, no
/// repainting — so the user's per-cell colours survive (roadmap D16).
fn load_assigned_balloons(
    cfg: &Config,
    load: &impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError>,
) -> Vec<Option<AsciiSprite>> {
    let mut out: Vec<Option<AsciiSprite>> = (0..BALLOON_HOLDER_DUCK.len()).map(|_| None).collect();
    for number in 1..=BALLOON_HOLDER_DUCK.len() {
        put_assigned_balloon(
            &mut out,
            crate::drawn::balloon_slot(number),
            cfg.assignments.balloon(number),
            load,
        );
    }
    out
}

/// Put one assigned balloon into its balloon slot.
///
/// A missing or corrupt asset is **not fatal**: it is traced and the slot
/// keeps the watched balloon.
fn put_assigned_balloon(
    out: &mut [Option<AsciiSprite>],
    slot: Option<usize>,
    name: Option<&str>,
    load: &impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError>,
) {
    let (Some(slot), Some(name)) = (slot, name) else {
        return;
    };
    let sprite = match load(DrawnKind::Balloon, name) {
        Ok(sprite) => sprite,
        Err(e) => {
            crate::trace::trace(&format!("scene: assigned balloon {name:?} ignored: {e}"));
            return;
        }
    };
    let Some(frame) = sprite.frame(0) else {
        return;
    };
    let Some(entry) = out.get_mut(slot) else {
        return;
    };
    *entry = Some(frame.clone());
}

/// Deterministic per-slot rope length in rows (audit addendum note 7):
/// the big-duck carrier (slot 0) draws from `rope_min_big` so the balloon
/// clears its head, small carriers from `rope_min_small`; both run to
/// `rope_max`. A fixed per-slot seed mix keeps renders byte-reproducible.
fn roll_rope(cfg: &crate::config::BalloonsConfig, slot: usize) -> f32 {
    let min = if slot == 0 {
        cfg.rope_min_big
    } else {
        cfg.rope_min_small
    } as u64;
    let max = (cfg.rope_max as u64).max(min);
    let mut s = (slot as u64 + 1).wrapping_mul(ROPE_SEED_MIX);
    s ^= s >> 33;
    s = s.wrapping_mul(ROPE_SEED_MUL);
    s ^= s >> 33;
    let span = max - min + 1;
    (min + s % span) as f32
}

/// Grid-top guard for balloon centres (audit P1-15): the rope invariant
/// decides the height, but a centre above `ry + 1` would clip the disc, so
/// the guard wins and the rope goes slack instead.
fn clamp_centre_top(centre_y: f32, ry: f32) -> f32 {
    centre_y.max(ry + 1.0)
}

/// True when the 5-tall input line starting at `ground + 8` fits inside a
/// `rows`-tall grid (audit P1-10).
fn input_line_fits(rows: i64, ground: i64) -> bool {
    rows - ground >= INPUT_ROW_OFFSET + INPUT_FONT_ROWS
}

/// Cells the input line may use, measured from its start column: the line
/// runs from [`INPUT_TEXT_COL`] to the right edge of the grid.
fn input_line_available_cells(cols: usize) -> i64 {
    (cols as i64 - INPUT_TEXT_COL).max(0)
}

/// Longest suffix of `text` that fits in `available` cells (audit P1-9
/// nit): the visible window follows the cursor, so on a narrow grid the
/// newest characters stay on screen and the prompt scrolls off instead of
/// being clipped away at the right edge.
///
/// The width measure includes the trailing gap of the last glyph, so the
/// window is at most one cell short of the true extent — never wider.
fn tail_window(text: &str, available: i64) -> &str {
    if available <= 0 {
        return "";
    }
    if crate::hud_font::text_width(text) <= available {
        return text;
    }
    // Accumulate glyph widths from the newest character backwards; the
    // longest suffix that still fits is the one we show.
    let mut width = 0i64;
    let mut start = text.len();
    for (idx, ch) in text.char_indices().rev() {
        let w = crate::hud_font::char_width(ch);
        if width + w > available {
            break;
        }
        width += w;
        start = idx;
    }
    if start == text.len() {
        // Not even one glyph fits: keep the newest character and let the
        // dropping write clip it.
        return match text.chars().next_back() {
            Some(c) => &text[text.len() - c.len_utf8()..],
            None => "",
        };
    }
    &text[start..]
}

/// Grass line at 80% of the grid height (audit 2026-10-06 §4 sketch layout:
/// `360 * 4 / 5 = 288` on the 960x360 row-budget grid; bottom 20% stays
/// empty below the grass band for the Fix 5 input line — the band itself
/// is [`BUILTIN_GRASS_ROWS`] rows tall, well clear of `ground + 8`).
pub fn ground_row_for(rows: usize) -> i64 {
    (rows as i64) * 4 / 5
}

pub const GRASS_RAIL_LINE: &str =
    "| /             \\ | /                             \\ | /                            \\ | /                    \\ |";

pub const GRASS_WAVE_LINE: &str =
    "|/...~_,.-~-.,\\|/..-~-'-~-..~~._,.-~-.,\\|/..-~-'-~-..~~._,.-~-.,\\|/..-~-'-~-..~~._,.-~-.,\\|/..-~-'-~-..~~..\\|";

/// Rail character (top row of grass divider) at column `col`.
pub fn grass_rail_char(col: i64) -> char {
    let bytes = GRASS_RAIL_LINE.as_bytes();
    let idx = col.rem_euclid(bytes.len() as i64) as usize;
    bytes[idx] as char
}

/// Wave character (bottom row of grass divider) at column `col`.
pub fn grass_wave_char(col: i64) -> char {
    let bytes = GRASS_WAVE_LINE.as_bytes();
    let idx = col.rem_euclid(bytes.len() as i64) as usize;
    bytes[idx] as char
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::balloon::AsciiSprite;

    fn scene160() -> Scene {
        Scene::new(&Config::default(), 160, 45, 12, 24)
    }

    /// The rendered frame of a scene at a given parade offset.
    ///
    /// The scene is rendered through the **same** composite the popup
    /// and the headless gate use, so the test checks what the user
    /// sees.
    fn render_at(scene: &Scene, offset_px: i32) -> crate::blit::FrameBuffer {
        let mut font = crate::font::Font::new(12, 24).expect("font");
        let layers = scene.backdrop_blits();
        let overlay = scene.message_blit();
        let mut fb = crate::blit::FrameBuffer::new(160 * 12, 45 * 24);
        crate::blit::blit_scene(
            &mut fb,
            &mut font,
            scene.grid(),
            offset_px,
            scene.ground_row() as usize,
            scene.sky_ramp(),
            &layers,
            overlay.as_ref(),
        );
        fb
    }

    /// The pixel bytes of the framebuffer from a grid row down.
    fn bytes_from_row(fb: &crate::blit::FrameBuffer, row: usize) -> Vec<u8> {
        let from = row * 24 * fb.width as usize * 4;
        fb.data[from..].to_vec()
    }

    #[test]
    fn the_grass_is_pixel_stable() {
        // The grass must not shake when the parade scrolls: it is the
        // one thing on screen that has to stay put (roadmap Extension 4
        // §1.3).
        let scene = scene160();
        let ground = scene.ground_row() as usize;
        let still = render_at(&scene, 0);
        let moved = render_at(&scene, 7);
        assert_eq!(
            bytes_from_row(&still, ground),
            bytes_from_row(&moved, ground),
            "the grass and everything under it must not move"
        );
    }

    #[test]
    fn the_hud_does_not_scroll() {
        // The HUD is chrome: the text must not smear.
        let scene = scene160();
        let rows = 45;
        let still = render_at(&scene, 0);
        let moved = render_at(&scene, 7);
        assert_eq!(
            bytes_from_row(&still, rows - 8),
            bytes_from_row(&moved, rows - 8),
            "the HUD must not move"
        );
    }

    #[test]
    fn a_backdrop_becomes_a_layer_with_its_own_offset() {
        // A tile becomes a layer: the band sits just above the grass and
        // the layer scrolls at its own rate.
        let mut scene = scene160();
        let mut tile = AsciiSprite::new(8, 3);
        for col in 0..8 {
            tile.set(col, 0, '#', [10, 20, 30]);
        }
        scene.set_backdrops(vec![tile]);
        let ground = scene.ground_row() as usize;
        let layers = scene.backdrop_layers();
        assert_eq!(layers.len(), 1);
        assert_eq!(layers[0].rows, 3, "the band is the tile's height");
        assert_eq!(
            layers[0].row_offset,
            ground - 3,
            "the band sits just above the grass"
        );
        let blits = scene.backdrop_blits();
        assert_eq!(blits.len(), 1);
        assert_eq!(blits[0].rows, 3);
        // The layer's grid is one tile wider than the screen, so a shift
        // of up to a whole tile still covers it.
        assert_eq!(blits[0].grid.cols(), 160 + 8);
    }

    #[test]
    fn the_night_sky_is_deterministic() {
        // Audit 2026-10-23 F-025: the star field and the moon are pure
        // functions of the config and the geometry — two scenes built the
        // same way must agree cell for cell, or the selftest gate could
        // not be byte-identical either.
        let mut night = Config::default();
        night.sky.time_of_day = 0.0;
        let a = Scene::new(&night, 200, 60, 4, 6);
        let b = Scene::new(&night, 200, 60, 4, 6);
        let layer_a = a.backdrop_blits();
        let layer_b = b.backdrop_blits();
        assert_eq!(layer_a.len(), 1, "midnight carries the night-sky layer");
        assert_eq!(
            layer_a[0].offset_px, 0,
            "the stars sit at rate 0: they must not stream with the parade"
        );
        assert_eq!(
            layer_a[0].grid.to_text(),
            layer_b[0].grid.to_text(),
            "the night sky must be identical on every render"
        );
        assert!(
            layer_a[0].grid.ink_count() > 20,
            "the night sky must carry stars and a moon, got {}",
            layer_a[0].grid.ink_count()
        );
        // The day half carries no star layer at all.
        let day = Scene::new(&Config::default(), 200, 60, 4, 6);
        assert_eq!(
            day.backdrop_blits().len(),
            0,
            "the default day has no star layer"
        );
    }

    #[test]
    fn a_broken_backdrop_leaves_the_parade_alone() {
        // A picture that cannot be read is skipped, and the parade is
        // exactly what it was (roadmap Extension 4 §1.3).
        let cfg = Config {
            backdrop: vec![crate::config::BackdropConfig {
                image: "no-such-picture.png".to_string(),
                ..crate::config::BackdropConfig::default()
            }],
            ..Config::default()
        };
        let tiles = crate::backdrop::convert_layers(&cfg.backdrop);
        assert!(tiles.is_empty(), "a broken picture must be skipped");
        let mut broken = Scene::new(&cfg, 160, 45, 12, 24);
        broken.set_backdrops(tiles);
        broken.advance(0.1);
        let mut plain = scene160();
        plain.advance(0.1);
        assert_eq!(
            broken.grid(),
            plain.grid(),
            "a broken backdrop must leave the parade alone"
        );
    }

    #[test]
    fn first_advance_renders_nonempty_grid() {
        let mut s = scene160();
        assert_eq!(s.grid().ink_count(), 0, "fresh grid is empty");
        s.advance(1.0 / 30.0);
        assert!(s.grid().ink_count() > 0, "ducks, grass and HUD must ink");
    }

    #[test]
    fn ground_row_is_four_fifths() {
        let s = scene160();
        assert_eq!(s.ground_row(), 36);
        assert_eq!(ground_row_for(45), 36);
        assert_eq!(ground_row_for(360), 288);
        assert_eq!(s.cols(), 160);
        assert_eq!(s.rows(), 45);
    }

    #[test]
    fn grass_covers_ground_row() {
        let mut s = scene160();
        s.advance(0.1);
        assert!(
            count_ink_rows(s.grid(), 36, 37) > 10,
            "grass rail must ink ground row (36)"
        );
        assert!(
            count_ink_rows(s.grid(), 37, 38) > 100,
            "grass wave must ink wave row (37)"
        );
    }

    #[test]
    fn the_builtin_band_is_four_rows_tall() {
        // Audit 2026-10-23 F-024: the rail, the wave, the sparse blade row
        // and the soil dot row — and the blade row stays sparse, so the
        // band reads as ground and not as a third dense line.
        let mut s = scene160();
        s.advance(0.1);
        let ground = s.ground_row() as usize;
        assert_eq!(s.band_rows(), 4, "the built-in band is four rows");
        assert!(
            count_ink_rows(s.grid(), ground + 2, ground + 3) > 5,
            "the blade row must ink a tuft every few columns"
        );
        assert!(
            count_ink_rows(s.grid(), ground + 3, ground + 4) > 5,
            "the soil row must ink its dots"
        );
        let blades = count_ink_rows(s.grid(), ground + 2, ground + 3);
        let wave = count_ink_rows(s.grid(), ground + 1, ground + 2);
        assert!(
            blades * 3 < wave,
            "the blade row ({blades}) must be far sparser than the wave row ({wave})"
        );
    }

    #[test]
    fn the_grass_follows_the_time_of_day() {
        // Audit 2026-10-23 F-026: noon keeps the meadow green, midnight
        // dims it to the floor, and a sky that is off leaves it alone.
        // The wave row's second column is an inked cell, so its colour is
        // the band's.
        let mut noon = Config::default();
        noon.sky.time_of_day = 0.5;
        let mut midnight = noon.clone();
        midnight.sky.time_of_day = 0.0;
        let mut off = midnight.clone();
        off.sky.enabled = false;
        let s_noon = scene_with(&noon, no_drawn_art);
        let s_night = scene_with(&midnight, no_drawn_art);
        let s_off = scene_with(&off, no_drawn_art);
        let wave = |s: &Scene| (s.ground_row() + 1) as usize;
        assert_eq!(
            s_noon.grid().color_at(1, wave(&s_noon)),
            Some(GRASS_COLOR),
            "noon must keep the plain meadow green"
        );
        assert_eq!(
            s_night.grid().color_at(1, wave(&s_night)),
            Some(scale_colour(GRASS_COLOR, GRASS_DIM_FLOOR)),
            "midnight must dim the meadow to the floor"
        );
        assert_eq!(
            s_off.grid().color_at(1, wave(&s_off)),
            Some(GRASS_COLOR),
            "a sky that is off leaves the band alone"
        );
    }

    #[test]
    fn the_ducks_cast_contact_shadows() {
        // Audit 2026-10-23 F-032: a one-row speckle under each duck's
        // body centre — a speckle, never a solid band, and never over a
        // grass glyph (the shadow writes the rail row's gaps only).
        let mut s = wide_scene();
        s.advance(0.1);
        let ground = s.ground_row() as usize;
        let shadows = (0..s.cols())
            .filter(|&c| s.grid().char_at(c, ground) == Some('`'))
            .count();
        assert!(shadows > 0, "the train must cast shadows");
        assert!(
            shadows <= DUCK_COUNT * SHADOW_SPAN_COLS,
            "the shadow is a speckle per duck, not a band: {shadows}"
        );
    }

    /// Non-space cells in grid rows `r0..r1` across all columns.
    fn count_ink_rows(grid: &Grid, r0: usize, r1: usize) -> usize {
        let mut n = 0;
        grid.for_each_cell(|_, r, ch, _| {
            if ch != ' ' && r >= r0 && r < r1 {
                n += 1;
            }
        });
        n
    }

    /// `'X'` cells (block-font ink) in rows `r0..r1`, cols `c0..c1`.
    fn count_x_in(grid: &Grid, r0: usize, r1: usize, c0: usize, c1: usize) -> usize {
        let mut n = 0;
        grid.for_each_cell(|c, r, ch, _| {
            if ch == 'X' && r >= r0 && r < r1 && c >= c0 && c < c1 {
                n += 1;
            }
        });
        n
    }

    #[test]
    fn eleven_ducks_are_inked() {
        // Production grid: the train head starts near the left edge, so by
        // 6 s the leader is deep in exit and several ducks are on screen.
        let mut s = Scene::new(&Config::default(), 960, 360, 2, 3);
        s.advance(6.0);
        let ground = s.ground_row() as usize;
        let duck_rows_ink = count_ink_rows(s.grid(), ground - 70, ground);
        assert!(
            duck_rows_ink >= 2000,
            "on-screen ducks must ink cells, got {duck_rows_ink}"
        );
    }

    #[test]
    fn lowest_balloon_clears_duck_heads() {
        // Ring bottom = pivot - rope; the shortest rope must leave several
        // rows between the ring and the carrier duck's head.
        let cfg = Config::default();
        let s = Scene::new(&cfg, 960, 360, 2, 3);
        let ground = s.ground_row() as f32;
        for (slot, &rope) in s.ropes.iter().enumerate() {
            let duck_rows = if slot == 0 { 70.0 } else { 35.0 };
            let head_top = ground - duck_rows;
            let ring_bottom = ground - 2.0 - rope;
            assert!(
                ring_bottom < head_top - 5.0,
                "balloon {slot} covers the duck: ring bottom {ring_bottom:.0} vs head top {head_top:.0}"
            );
        }
    }

    #[test]
    fn sketch_balloons_stay_below_the_top_and_clear_each_other() {
        // Audit §4: balloon diameter 108 cols -> carrier spacing must exceed
        // it pairwise, and randomized ropes must keep every disc below the
        // top edge. The invariants hold wherever the train happens to be, so
        // the test sweeps a whole re-entry period instead of standing on one
        // snapshot — the stride-locked march moved the old 6 s position
        // (audit 2026-10-23 §F-031). A disc near a horizontal edge clips,
        // which is the balloon rule, not a bug.
        let cfg = Config::default();
        let mut s = Scene::new(&cfg, 960, 360, 2, 3);
        let (rx, ry) = balloon::balloon_radii_cells(cfg.balloons.balloon_radius_rows, 2, 3);
        let mut swept = 0.0;
        while swept < 20.0 {
            s.advance(0.5);
            swept += 0.5;
            assert_all_balloons(&s, rx, ry);
        }
    }

    /// The per-step balloon invariants over every carrier slot.
    fn assert_all_balloons(s: &Scene, rx: f32, ry: f32) {
        for slot in 0..BALLOON_HOLDER_DUCK.len() {
            assert_balloon_below_the_top(s, slot, ry);
            assert_balloon_clear_of_earlier(s, slot, rx, ry);
        }
    }

    /// Balloon `slot`'s disc stays below the grid top: rope and pendulum
    /// geometry, not a snapshot position.
    fn assert_balloon_below_the_top(s: &Scene, slot: usize, ry: f32) {
        let (_, cy) = s.balloon_centre(slot, 0.0, ry);
        assert!(cy - ry >= 0.0, "balloon {slot} pokes above the grid top");
    }

    /// Balloon `slot` overlaps none of the earlier balloons (discs clear
    /// each other in x or in y).
    fn assert_balloon_clear_of_earlier(s: &Scene, slot: usize, rx: f32, ry: f32) {
        let (cx, cy) = s.balloon_centre(slot, rx, ry);
        for prev in 0..slot {
            let (px, py) = s.balloon_centre(prev, rx, ry);
            let (dx, dy) = ((cx - px).abs(), (cy - py).abs());
            assert!(
                dx >= 2.0 * rx || dy >= 2.0 * ry,
                "balloons {prev} and {slot} overlap: dx {dx:.0} dy {dy:.0}"
            );
        }
    }

    #[test]
    fn input_line_renders_block_text_below_grass() {
        let mut s = Scene::new(&Config::default(), 960, 360, 2, 3);
        s.advance(0.1);
        let ground = s.ground_row() as usize;
        let line = |sc: &Scene, r0: usize, r1: usize| count_x_in(sc.grid(), r0, r1, 0, 200);
        assert_eq!(line(&s, ground + 8, ground + 13), 0, "no line without text");
        s.set_input_line(Some("ADD BALLOON: DUCKS.PNG_".to_string()));
        s.advance(0.1);
        let ink = line(&s, ground + 8, ground + 13);
        assert!(ink > 40, "input line must render block text, got {ink}");
        // The line sits below the grass, above the bottom margin.
        assert_eq!(line(&s, ground + 2, ground + 7), 0, "gap below grass");
    }

    #[test]
    fn input_line_fits_needs_offset_plus_font() {
        assert!(input_line_fits(49, 36), "band 13 fits exactly");
        assert!(input_line_fits(360, 288));
        assert!(!input_line_fits(45, 36), "scene160 band 9 skips the line");
        assert!(
            !input_line_fits(10, 8),
            "rows-ground 2 cannot fit a 13-row line"
        );
        assert!(!input_line_fits(8, 8), "zero band fits nothing");
    }

    #[test]
    fn input_line_skips_cleanly_on_short_grids() {
        // Audit P1-10: a grid too short for `ground + 8` plus 5 font rows
        // renders grass/ducks/HUD but no input line — and no panic. The two
        // scenes differ only by the input text, so identical grids prove the
        // line was skipped rather than drawn somewhere wrong.
        let mut with = Scene::new(&Config::default(), 60, 10, 12, 24);
        with.set_input_line(Some("ADD BALLOON: DUCKS.PNG_".to_string()));
        with.advance(0.1);
        let mut without = Scene::new(&Config::default(), 60, 10, 12, 24);
        without.advance(0.1);
        let band = with.rows() as i64 - with.ground_row();
        assert!(band < 13, "test grid must be too short, band {band}");
        assert_eq!(
            with.grid(),
            without.grid(),
            "short grid must skip the input line"
        );
        assert!(
            with.grid().ink_count() > 0,
            "grass and ducks must still render"
        );
    }

    #[test]
    fn short_grid_input_skip_is_traced_once() {
        // Audit P0-1: the row guard fires on every redraw, so the trace
        // must be emitted once per episode, not once per frame.
        let mut s = Scene::new(&Config::default(), 60, 10, 12, 24);
        s.set_input_line(Some("ADD BALLOON: DUCKS.PNG_".to_string()));
        s.advance(0.1);
        assert_eq!(s.input_skip_traces, 1, "the skip must be traced");
        for _ in 0..30 {
            s.advance(0.1);
        }
        assert_eq!(
            s.input_skip_traces, 1,
            "the trace must not repeat per frame"
        );
    }

    #[test]
    fn tail_window_keeps_the_newest_characters() {
        // Audit P1-9 nit: the visible window follows the cursor, so the
        // tail of the line is kept and the head scrolls off.
        let text = "ADD BALLOON: DUCK.PNG_";
        let window = tail_window(text, 38);
        assert!(
            window.ends_with("PNG_"),
            "the newest characters must stay, got {window:?}"
        );
        assert!(
            !window.contains("ADD"),
            "the prompt must scroll off, got {window:?}"
        );
        assert!(
            crate::hud_font::text_width(window) <= 38,
            "the window must fit the grid"
        );
        // A line that already fits is shown whole.
        assert_eq!(tail_window("HI", 38), "HI");
        // A grid narrower than one glyph still shows the newest character.
        assert_eq!(tail_window("ABC", 1), "C");
    }

    #[test]
    fn input_line_scrolls_to_the_cursor_on_a_narrow_grid() {
        // Audit P1-9 nit: a line wider than the screen used to be clipped
        // at the right edge, so the prompt and the first typed characters
        // were off-screen. The visible window must follow the cursor.
        let cols = 40usize;
        let rows = 120usize;
        let text = "ADD BALLOON: DUCK.PNG_";
        let mut s = Scene::new(&Config::default(), cols, rows, 12, 24);
        s.set_input_line(Some(text.to_string()));
        s.advance(0.1);
        let ground = s.ground_row() as usize;
        let band = (ground + 8)..(ground + 13);
        let tail = tail_window(text, input_line_available_cells(cols));
        assert_eq!(tail, "CK.PNG_", "the window must follow the cursor");
        assert!(!tail.contains("ADD"), "the prompt must scroll off");
        // The rendered band is exactly the tail, drawn at the input column.
        let mut reference = Grid::new(cols, rows, 12, 24);
        crate::hud_font::draw_text(
            &mut reference,
            INPUT_TEXT_COL,
            band.start as i64,
            tail,
            HUD_COLOR,
        );
        for (c, r) in band
            .clone()
            .flat_map(move |r| (0..cols).map(move |c| (c, r)))
        {
            assert_eq!(
                s.grid().char_at(c, r),
                reference.char_at(c, r),
                "cell ({c},{r}) differs from the tail render"
            );
        }
        // The last typed character (the cursor) is visible.
        let cursor_x = INPUT_TEXT_COL + crate::hud_font::text_width(tail) - 5;
        assert_eq!(
            s.grid().char_at(cursor_x as usize, band.end - 1),
            Some('X'),
            "the cursor glyph must be visible"
        );
    }

    #[test]
    fn disc_cache_matches_a_fresh_circle_geometry() {
        // Audit performance: the disc cells are cached per geometry, so the
        // cache must hold exactly what a fresh `circle_cells` call yields,
        // and it must be reused on later frames instead of rebuilt.
        let mut s = Scene::new(&Config::default(), 160, 45, 12, 24);
        s.advance(0.1);
        let (rx, ry) = balloon::balloon_radii_cells(s.cfg.balloons.balloon_radius_rows, 12, 24);
        assert_eq!(s.disc_radii, (rx, ry), "the cache is keyed by the radii");
        assert_eq!(s.disc_cells, balloon::circle_cells(rx, ry));
        let cached = s.disc_cells.clone();
        s.advance(0.1);
        assert_eq!(s.disc_cells, cached, "the cache must survive a redraw");
    }

    #[test]
    fn top_guard_wins_over_rope_invariant() {
        // Audit P1-15: on a grid so short the rope math would push the disc
        // above row 0, the centre clamps to `ry + 1` (rope goes slack) and
        // the rendered disc still clears the top edge. Discs render on a
        // fresh grid (no ducks/ropes) so every ink cell is balloon ink.
        //
        // The pin is not vacuous: the test asserts that the *unclamped*
        // attach point really was above `ry + 1` for at least one slot, so
        // the guard has to fire for the invariant to hold.
        let mut cfg = Config::default();
        cfg.balloons.balloon_radius_rows = 4;
        cfg.balloons.rope_min_small = 10;
        cfg.balloons.rope_min_big = 10;
        cfg.balloons.rope_max = 20;
        let s = Scene::new(&cfg, 160, 30, 12, 24);
        let (rx, ry) = balloon::balloon_radii_cells(cfg.balloons.balloon_radius_rows, 12, 24);
        let mut g = Grid::new(160, 30, 12, 24);
        let mut clamped_slots = 0;
        for slot in 0..BALLOON_HOLDER_DUCK.len() {
            let attach = s.balloon_attach(slot, ry);
            let (cx, cy) = s.balloon_centre(slot, rx, ry);
            assert!(
                cy >= ry + 1.0,
                "slot {slot}: centre {cy:.1} must respect the top guard ry+1 = {:.1}",
                ry + 1.0
            );
            if attach.1 < ry + 1.0 {
                clamped_slots += 1;
                assert!(
                    (cy - (ry + 1.0)).abs() < 1e-4,
                    "slot {slot}: the guard must clamp the centre onto ry+1, got {cy:.2}"
                );
            }
            balloon::draw_balloon(&mut g, cx, cy, rx, ry, empty_ring_pixel);
        }
        assert!(
            clamped_slots > 0,
            "the top guard must fire for at least one slot, else the pin is vacuous"
        );
        assert_eq!(count_ink_rows(&g, 0, 1), 0, "disc tops must clear row 0");
        assert!(g.ink_count() > 0, "clamped discs must still render");
    }

    /// Empty-balloon ring pixel: outline band inks, interior stays
    /// transparent (mirrors `draw_empty_balloon` for the top-guard pin).
    fn empty_ring_pixel(_dx: i64, _dy: i64, d: f32) -> Option<(char, [u8; 3])> {
        if d >= RING_INNER_D {
            Some(('.', EMPTY_BALLOON_COLOR))
        } else {
            None
        }
    }

    #[test]
    fn the_empty_balloon_is_shaded() {
        // Audit 2026-10-23 F-027: the placeholder disc carries the
        // highlight arc and the interior speckle beside the ring, so it
        // reads as a shaded balloon instead of a faint dotted hoop. The
        // grid is the production one (960 x 360, 2x3 cells): the disc's
        // bottom third needs the full row budget to land on screen.
        let mut s = Scene::new(&Config::default(), 960, 360, 2, 3);
        s.advance(0.1);
        let ring = count_color(s.grid(), EMPTY_BALLOON_COLOR);
        let highlight = count_color(s.grid(), EMPTY_BALLOON_HIGHLIGHT);
        let shade = count_color(s.grid(), EMPTY_BALLOON_SHADE);
        assert!(ring > 0, "the outline ring stays");
        assert!(highlight > 0, "the top-left highlight arc must ink");
        assert!(shade > 0, "the bottom-third speckle must ink");
    }

    #[test]
    fn step_matches_pendulum_substep() {
        assert_eq!(SIM_STEP_SECONDS, crate::pendulum::SUBSTEP_SECONDS);
    }

    #[test]
    fn advance_guard_is_derived_from_the_step() {
        // Audit magic number: the watchdog used to be the literal
        // `3600 * 20`; it is now derived from `SIM_STEP_SECONDS`, so this pins
        // the effective step count (72000 steps of 1/120 s = 600 s).
        assert_eq!(MAX_STEPS_PER_ADVANCE, 72_000);
        let simulated = MAX_STEPS_PER_ADVANCE as f32 * SIM_STEP_SECONDS;
        assert!(
            (simulated - MAX_ADVANCE_SECONDS).abs() < 0.01,
            "the guard must cover {MAX_ADVANCE_SECONDS} s, got {simulated}"
        );
    }

    #[test]
    fn the_parallax_scroll_is_the_marched_distance_wherever_it_is_chunked() {
        // The scroll accumulator consumes the march-clock delta exactly
        // once per advance; a second consumer (or an update without
        // accumulation) made the backdrops jump (audit 2026-10-21 §F5,
        // its f32 ancestor). Chunked and single-call marching land on
        // the same scroll within f32 chunking ulps, and the scroll
        // continues from where it was — never resets (audit 101126
        // §C10).
        let cfg = crate::config::Config::default();
        let mut one = Scene::new(&cfg, 160, 90, 12, 24);
        let mut many = Scene::new(&cfg, 160, 90, 12, 24);
        one.advance(2.0);
        for _ in 0..120 {
            many.advance(1.0 / 60.0);
        }
        // The march clock is a per-substep accumulator, so a chunked
        // march can finish the substep the one-call march rounds away:
        // the invariant is that the scroll is the marched distance
        // within **one march substep**, whichever way the frames were
        // chunked — never a jump, never a reset.
        let one_substep = f64::from(many.march_speed()) * f64::from(SIM_STEP_SECONDS);
        let single = one.scroll_cells();
        let chunked = many.scroll_cells();
        assert!(
            (single - chunked).abs() <= one_substep + 1e-6,
            "one call: {single} vs chunked {chunked}"
        );
        assert!(
            (single - f64::from(many.march_speed()) * f64::from(many.elapsed())).abs()
                <= one_substep + 1e-6,
            "the scroll is the marched distance: {single}"
        );
        one.advance(1.0);
        assert!(
            one.scroll_cells() > single,
            "the scroll continues from where it was"
        );
    }

    #[test]
    fn centre_top_guard_clamps_only_upward() {
        assert_eq!(
            clamp_centre_top(-3.0, 4.0),
            5.0,
            "above-grid centre clamps to ry+1"
        );
        assert_eq!(clamp_centre_top(5.0, 4.0), 5.0, "guard boundary holds");
        assert_eq!(
            clamp_centre_top(20.0, 4.0),
            20.0,
            "rope-ruled centre passes through"
        );
    }

    #[test]
    fn ring_slots_wrap_and_count_saturates() {
        let mut s = scene160();
        let mk = |n: u8| {
            let mut sp = AsciiSprite::new(3, 3);
            sp.set(1, 1, (b'0' + n) as char, [255, 255, 255]);
            sp
        };
        let mut slots = Vec::new();
        for n in 0..6 {
            slots.push(s.push_balloon(mk(n)));
        }
        assert_eq!(slots, vec![0, 1, 2, 3, 4, 0]);
        assert_eq!(s.balloon_count(), 5);
    }

    #[test]
    fn big_ducks_lead_left_with_smalls_trailing_right() {
        let mut s = scene160();
        s.advance(1.0 / 30.0);
        let rx = |slot: usize| -> (f32, f32) {
            let p = &s.parade;
            (p.unwrapped_x(slot), p.render_x(slot))
        };
        let (u0, _) = rx(0);
        let (u2, _) = rx(2);
        let (u4, _) = rx(4);
        let (u10, _) = rx(10);
        assert!(u0 < u2, "slot 0 must head the train, got {u0} vs {u2}");
        assert!(u2 < u4, "slot 2 must head slot 4, got {u2} vs {u4}");
        assert!(u4 < u10, "slot 4 must head slot 10, got {u4} vs {u10}");
        let gap = s.parade.unwrapped_x(10) - s.parade.unwrapped_x(0);
        assert!(
            (gap - 1070.0).abs() < 1e-3,
            "train offsets must hold (tail at +1070), gap {gap}"
        );
    }

    #[test]
    fn row_zero_untouched_after_sixty_seconds() {
        // 70-row ducks need a tall grid to clear row 0; the balloon radius
        // and rope bounds are pinned small so this tracks the balloon-top
        // guard rather than the full sketch sizes.
        let mut cfg = Config::default();
        cfg.balloons.balloon_radius_rows = 4;
        cfg.balloons.rope_min_small = 10;
        cfg.balloons.rope_min_big = 10;
        cfg.balloons.rope_max = 20;
        let mut s = Scene::new(&cfg, 160, 140, 12, 24);
        s.advance(60.0);
        assert_eq!(count_ink_rows(s.grid(), 0, 1), 0, "row 0 must stay empty");
        assert_eq!(
            count_ink_rows(s.grid(), 0, 4),
            0,
            "balloon tops must clear the grid top"
        );
    }

    #[test]
    fn hud_sits_in_configured_corner() {
        // Block-font HUD occupies the 5 rows above the bottom margin; the
        // only X ink below the grass belongs to it.
        let hud_count = |sc: &Scene, c0: usize, c1: usize| count_x_in(sc.grid(), 39, 44, c0, c1);
        let mut s = scene160();
        s.advance(1.0);
        // "PRESS ESC TO EXIT" is ~93 cells wide; right-aligned it starts
        // near col 66, so the empty-side window must stop before that.
        assert!(hud_count(&s, 100, 160) > 20, "HUD must render bottom-right");
        assert_eq!(hud_count(&s, 0, 60), 0, "bottom-left must stay empty");
        let left = Config {
            hint_corner: HintCorner::BottomLeft,
            ..Config::default()
        };
        let mut l = Scene::new(&left, 160, 45, 12, 24);
        l.advance(1.0);
        assert!(hud_count(&l, 0, 60) > 20, "HUD must render bottom-left");
        assert_eq!(hud_count(&l, 100, 160), 0, "bottom-right must stay empty");
    }

    #[test]
    fn sub_cell_offset_in_range() {
        let mut s = scene160();
        for _ in 0..200 {
            s.advance(0.037);
            let off = s.sub_cell_offset_px();
            assert!((0..12).contains(&off), "offset {off}");
        }
    }

    #[test]
    fn identical_advance_gives_identical_grid() {
        let mut a = scene160();
        let mut b = scene160();
        a.push_balloon(AsciiSprite::from_rows(&["@@", "@@"], [255, 0, 0]));
        b.push_balloon(AsciiSprite::from_rows(&["@@", "@@"], [255, 0, 0]));
        for _ in 0..90 {
            a.advance(1.0 / 30.0);
            b.advance(1.0 / 30.0);
        }
        assert_eq!(a.elapsed(), b.elapsed());
        assert_eq!(a.grid(), b.grid());
    }

    #[test]
    fn ropes_render_for_every_carrier_slot() {
        // Placeholder balloons included: every carrier draws its rope down
        // to its duck, as in the sketch.
        let mut s = Scene::new(&Config::default(), 960, 360, 2, 3);
        s.advance(6.0);
        let rope_ink = s
            .grid()
            .to_text()
            .chars()
            .filter(|c| *c == '|' || *c == '/' || *c == '\\')
            .count();
        assert!(
            rope_ink > 0,
            "carrier slots must draw ropes, got {rope_ink}"
        );
    }

    #[test]
    fn print_rendered_scene_for_debug() {
        let mut s = scene160();
        s.advance(0.1);
        let txt = s.grid().to_text();
        for (i, line) in txt.lines().enumerate() {
            if !line.trim().is_empty() {
                println!("ROW {:02}: {}", i, line);
            }
        }
    }

    #[test]
    fn grass_pattern_is_periodic() {
        let rail_len = GRASS_RAIL_LINE.len();
        let wave_len = GRASS_WAVE_LINE.len();
        for c in 0..200 {
            assert_eq!(grass_rail_char(c), grass_rail_char(c + rail_len as i64));
            assert_eq!(grass_wave_char(c), grass_wave_char(c + wave_len as i64));
        }
    }

    #[test]
    fn resize_preserves_the_simulation_state() {
        // A resize re-lays the grid but keeps the simulation: the parade
        // phase, the clock and the balloon ring all survive.
        let mut s = Scene::new(&Config::default(), 160, 45, 12, 24);
        s.advance(1.5);
        s.push_balloon(AsciiSprite::from_rows(&["@@", "@@"], [9, 8, 7]));
        let phase = s.parade_phase();
        let elapsed = s.elapsed();
        let balloons = s.balloon_count();
        s.resize(200, 60, 12, 24);
        assert_eq!(s.cols(), 200, "the grid follows the new width");
        assert_eq!(s.rows(), 60, "the grid follows the new height");
        assert_eq!(s.ground_row(), ground_row_for(60), "the grass band moves");
        assert_eq!(s.elapsed(), elapsed, "the clock must not jump");
        assert_eq!(s.balloon_count(), balloons, "the balloon ring must survive");
        assert!(
            (s.parade_phase() - phase).abs() < 1e-4,
            "the parade phase must survive, got {} vs {phase}",
            s.parade_phase()
        );
    }

    #[test]
    fn resize_keeps_the_pendulum_phases_and_the_rope_lengths() {
        let mut s = Scene::new(&Config::default(), 160, 45, 12, 24);
        s.advance(1.0);
        let thetas: Vec<f32> = s.pendulums.iter().map(|p| p.theta()).collect();
        let ropes = s.ropes.clone();
        s.resize(200, 60, 12, 24);
        let after: Vec<f32> = s.pendulums.iter().map(|p| p.theta()).collect();
        assert_eq!(after, thetas, "the pendulum phases must survive");
        assert_eq!(s.ropes, ropes, "the rope lengths must survive");
    }

    #[test]
    fn resize_drops_the_stale_disc_cache() {
        // The balloon radii follow the cell pitch, so the cached disc cells
        // are stale after a cell-size change: the next frame rebuilds them.
        let mut s = Scene::new(&Config::default(), 160, 45, 12, 24);
        s.advance(0.1);
        assert!(!s.disc_cells.is_empty(), "the disc cache must be built");
        s.resize(160, 45, 12, 12);
        assert!(s.disc_cells.is_empty(), "the stale cache must be dropped");
        assert_eq!(s.disc_radii, (0.0, 0.0), "the cache key must be dropped");
        s.advance(0.1);
        let (rx, ry) = balloon::balloon_radii_cells(s.cfg.balloons.balloon_radius_rows, 12, 12);
        assert_eq!(s.disc_radii, (rx, ry), "the next frame rebuilds the cache");
        assert_eq!(s.disc_cells, balloon::circle_cells(rx, ry));
    }

    #[test]
    fn resize_resets_the_input_line_skip_trace() {
        // The new grid may again be too short for the input line: that
        // deserves its own one-shot trace.
        let mut s = Scene::new(&Config::default(), 60, 10, 12, 24);
        s.set_input_line(Some("ADD BALLOON: DUCKS.PNG_".to_string()));
        s.advance(0.1);
        assert_eq!(s.input_skip_traces, 1, "the skip must be traced");
        s.resize(60, 10, 12, 24);
        assert_eq!(
            s.input_skip_traces, 0,
            "the resized grid gets its own one-shot trace"
        );
    }

    // --- Stage 8: the drawn assets replace the built-in art ----------------

    /// The colour every drawn sprite of the tests carries, so the
    /// per-cell colour (D16) can be told apart from the built-in
    /// `DUCK_COLOR`.
    const DRAWN_COLOR: [u8; 3] = [10, 20, 30];

    /// A temp folder that is not the user's real asset folder.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "ducks-scene-{}-{}-{tag}",
            std::process::id(),
            nanos
        ));
        std::fs::create_dir_all(&dir).expect("mkdir temp dir");
        dir
    }

    /// A drawn big sprite of the built-in frame size, filled with `ch`.
    fn drawn_big(ch: char) -> DrawnSprite {
        drawn_big_frames(&[ch])
    }

    /// A drawn **small** sprite, one frame filled with `ch`.
    fn drawn_small(ch: char) -> DrawnSprite {
        let (cols, rows) = crate::drawn::sprite_frame_size(crate::drawn::DrawnSize::Small);
        let mut frame = AsciiSprite::new(cols, rows);
        for index in 0..cols * rows {
            frame.set(index % cols, index / cols, ch, DRAWN_COLOR);
        }
        DrawnSprite {
            kind: DrawnKind::Sprite,
            size: Some(crate::drawn::DrawnSize::Small),
            frames: vec![frame],
            reaction: None,
        }
    }
    /// A drawn big sprite with one frame per glyph.
    fn drawn_big_frames(chars: &[char]) -> DrawnSprite {
        let (cols, rows) = crate::drawn::sprite_frame_size(crate::drawn::DrawnSize::Big);
        let frames = chars
            .iter()
            .map(|ch| {
                let mut frame = AsciiSprite::new(cols, rows);
                for index in 0..cols * rows {
                    frame.set(index % cols, index / cols, *ch, DRAWN_COLOR);
                }
                frame
            })
            .collect();
        DrawnSprite {
            kind: DrawnKind::Sprite,
            size: Some(crate::drawn::DrawnSize::Big),
            frames,
            reaction: None,
        }
    }

    /// A drawn balloon of the default authoring disc, filled with `ch`.
    ///
    /// The frame is the disc's own cell grid (109 x 73 at the default
    /// radius 36) and only the disc's cells are inked — the same shape
    /// the editor paints in.
    fn drawn_balloon(ch: char) -> DrawnSprite {
        let geometry = crate::balloon::balloon_authoring_geometry(36);
        let mut frame = AsciiSprite::new(geometry.cols, geometry.rows);
        for (dx, dy, _) in crate::balloon::circle_cells(geometry.rx, geometry.ry) {
            let col = geometry.cols as i64 / 2 + dx;
            let row = geometry.rows as i64 / 2 + dy;
            if col < 0 || row < 0 {
                continue;
            }
            frame.set(col as usize, row as usize, ch, DRAWN_COLOR);
        }
        DrawnSprite {
            kind: DrawnKind::Balloon,
            size: None,
            frames: vec![frame],
            reaction: None,
        }
    }

    /// A scene over a wide grid with the train parked so that the first
    /// and the last duck are both fully on screen (the train is 1145 cells
    /// long, so a 1400-wide grid is needed to see both ends at once).
    fn scene_with(
        cfg: &Config,
        load: impl Fn(DrawnKind, &str) -> Result<DrawnSprite, DrawnError>,
    ) -> Scene {
        let mut s = Scene::build(cfg, 1400, 360, 2, 3, load, None);
        s.parade.advance(34.0);
        s.advance(0.1);
        s
    }

    /// The grid cell of the frame cell `(fx, fy)` of the duck in `slot`.
    fn duck_cell(s: &Scene, slot: usize, fx: usize, fy: usize) -> (usize, usize) {
        let big = balloon::BIG_DUCK_SLOTS.contains(&slot);
        let (cols, rows) = if big {
            (crate::sprites::BIG_DUCK_COLS, crate::sprites::BIG_DUCK_ROWS)
        } else {
            (
                crate::sprites::SMALL_DUCK_COLS,
                crate::sprites::SMALL_DUCK_ROWS,
            )
        };
        let ux = s.parade.unwrapped_x(slot);
        let col = ux.floor() as i64 - cols as i64 / 2;
        let row = s.ground_row() - rows as i64;
        ((col + fx as i64) as usize, (row + fy as i64) as usize)
    }

    /// Number of cells carrying `ch`.
    fn count_char(grid: &Grid, ch: char) -> usize {
        let mut n = 0;
        grid.for_each_cell(|_, _, cell, _| {
            if cell == ch {
                n += 1;
            }
        });
        n
    }

    /// Cells carrying exactly one colour, anywhere on the grid.
    fn count_color(grid: &Grid, color: [u8; 3]) -> usize {
        let mut n = 0;
        grid.for_each_cell(|_, _, _, cell| {
            if cell == color {
                n += 1;
            }
        });
        n
    }

    /// The colour of a drawn decoration in the tests.
    const DECORATION_COLOR: [u8; 3] = [1, 2, 3];

    /// The rows of a decoration's frame that carry its glyph.
    ///
    /// A decoration is a hat: it covers the **top** of the duck's frame,
    /// so the body's own glyph stays visible below it and the two frames
    /// can be compared.
    const DECORATION_ROWS: usize = 5;

    /// A drawn decoration of the big frame, filled with `ch`.
    ///
    /// The decoration is a **full-size** frame: it is drawn over the
    /// duck's own cells (roadmap Extension 2 §4.1).
    fn drawn_decoration(ch: char) -> DrawnSprite {
        drawn_decoration_frames(&[ch])
    }

    /// A drawn **small** decoration, one frame filled with `ch`.
    fn drawn_small_decoration(ch: char) -> DrawnSprite {
        let (cols, rows) = crate::drawn::sprite_frame_size(crate::drawn::DrawnSize::Small);
        let mut frame = AsciiSprite::new(cols, rows);
        for index in 0..cols * rows {
            frame.set(index % cols, index / cols, ch, DECORATION_COLOR);
        }
        DrawnSprite {
            kind: DrawnKind::Decoration,
            size: Some(crate::drawn::DrawnSize::Small),
            frames: vec![frame],
            reaction: None,
        }
    }

    /// A decoration with one frame per glyph.
    fn drawn_decoration_frames(chars: &[char]) -> DrawnSprite {
        let (cols, rows) = crate::drawn::sprite_frame_size(crate::drawn::DrawnSize::Big);
        let frames = chars
            .iter()
            .map(|ch| {
                let mut frame = AsciiSprite::new(cols, rows);
                for row in 0..DECORATION_ROWS {
                    fill_decoration_row(&mut frame, cols, row, *ch);
                }
                frame
            })
            .collect();
        DrawnSprite {
            kind: DrawnKind::Decoration,
            size: Some(crate::drawn::DrawnSize::Big),
            frames,
            reaction: None,
        }
    }

    /// Fill one row of a decoration frame.
    ///
    /// WHY a helper: the nesting rule is two levels at most (audit
    /// 2026-10-21 §F23).
    fn fill_decoration_row(frame: &mut AsciiSprite, cols: usize, row: usize, ch: char) {
        for col in 0..cols {
            frame.set(col, row, ch, DECORATION_COLOR);
        }
    }

    #[test]
    fn a_decoration_on_disk_reaches_the_parade() {
        // The whole chain: a document on disk, the loader, the scene.
        let root = temp_dir("decoration-chain");
        let sprite = drawn_decoration('H');
        crate::drawn::save_to(&root, &sprite, DrawnKind::Decoration, "crown").expect("save");
        let mut cfg = Config::default();
        cfg.decorations.big = Some("crown".to_string());
        let s = scene_with(&cfg, |kind, name| {
            crate::drawn::load_from(&root, kind, name)
        });
        let (x, y) = duck_cell(&s, 0, 10, 2);
        assert_eq!(
            s.grid().char_at(x, y),
            Some('H'),
            "the decoration on disk reaches the parade"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A **small** sprite in a **big** slot is refused: the slot's box is
    /// the big duck's frame, so the small art would sit in its top-left
    /// corner — a broken picture, and a hit test that does not match what
    /// the user sees (audit 2026-10-21 §E1).
    #[test]
    fn a_sprite_of_the_wrong_size_is_refused() {
        let mut cfg = Config::default();
        cfg.assignments.big_1 = Some("pup".to_string());
        let s = scene_with(&cfg, |kind, name| {
            assert_eq!(kind, DrawnKind::Sprite);
            assert_eq!(name, "pup");
            Ok(drawn_small('Z'))
        });
        // The built-in big duck is drawn: the picture is the one without
        // the assignment.
        let plain = scene_with(&Config::default(), |_, _| {
            Err(DrawnError::NotFound {
                name: "none".to_string(),
            })
        });
        assert_eq!(
            s.grid().to_text(),
            plain.grid().to_text(),
            "the small sprite is not drawn: the built-in duck is"
        );
    }

    /// A **small** decoration in the big duck's slot is refused as well.
    #[test]
    fn a_decoration_of_the_wrong_size_is_refused() {
        let mut cfg = Config::default();
        cfg.decorations.big = Some("crown".to_string());
        let s = scene_with(&cfg, |kind, name| {
            assert_eq!(kind, DrawnKind::Decoration);
            assert_eq!(name, "crown");
            Ok(drawn_small_decoration('H'))
        });
        // The duck is bare: the small decoration is not drawn over the
        // big duck's frame.
        let (x, y) = duck_cell(&s, 0, 10, 2);
        assert_ne!(
            s.grid().char_at(x, y),
            Some('H'),
            "the small decoration is not drawn"
        );
    }

    #[test]
    fn a_decoration_covers_the_body() {
        let mut cfg = Config::default();
        cfg.decorations.big = Some("crown".to_string());
        let s = scene_with(&cfg, |kind, name| {
            assert_eq!(kind, DrawnKind::Decoration);
            assert_eq!(name, "crown");
            Ok(drawn_decoration('H'))
        });
        // The decoration is drawn over the duck's own cells.
        let (x, y) = duck_cell(&s, 0, 10, 2);
        assert_eq!(
            s.grid().char_at(x, y),
            Some('H'),
            "the decoration covers the body"
        );
        assert_eq!(
            s.grid().color_at(x, y),
            Some(DECORATION_COLOR),
            "the decoration keeps its own colour"
        );
    }

    #[test]
    fn the_decoration_is_drawn_over_the_body() {
        let mut cfg = Config::default();
        cfg.decorations.big = Some("crown".to_string());
        let s = scene_with(&cfg, |_, _| Ok(drawn_decoration('H')));
        // The body's own glyph is **not** visible where the decoration
        // covers it: the decoration is the last thing drawn on the duck.
        let (x, y) = duck_cell(&s, 0, 10, 2);
        assert_ne!(s.grid().char_at(x, y), Some('1'), "the body is covered");
    }

    #[test]
    fn a_missing_decoration_leaves_the_ducks_bare() {
        let missing = |_: DrawnKind, name: &str| {
            Err(DrawnError::NotFound {
                name: name.to_string(),
            })
        };
        let mut cfg = Config::default();
        cfg.decorations.big = Some("ghost".to_string());
        let with = scene_with(&cfg, missing);
        let without = scene_with(&Config::default(), missing);
        assert_eq!(
            with.grid(),
            without.grid(),
            "a missing decoration must leave the ducks bare"
        );
        assert_eq!(count_char(with.grid(), 'H'), 0, "no decoration may appear");
    }

    #[test]
    fn the_decoration_frames_step_in_step_with_the_body() {
        let mut cfg = Config::default();
        cfg.assignments.big_1 = Some("cat".to_string());
        cfg.decorations.big = Some("crown".to_string());
        let mut s = scene_with(&cfg, |kind, _| {
            if kind == DrawnKind::Sprite {
                return Ok(drawn_big_frames(&['1', '2', '3']));
            }
            Ok(drawn_decoration_frames(&['a', 'b', 'c']))
        });
        // The body shows '1', '2' or '3' and the decoration shows 'a',
        // 'b' or 'c' — the same frame index, so the hat walks with the
        // duck.
        let mut pairs: Vec<(Option<char>, Option<char>)> = Vec::new();
        for _ in 0..9 {
            let (x, y) = duck_cell(&s, 0, 10, 2);
            let decoration = s.grid().char_at(x, y);
            let (x, y) = duck_cell(&s, 0, 10, 30);
            let body = s.grid().char_at(x, y);
            pairs.push((body, decoration));
            s.advance(0.2);
        }
        for (body, decoration) in &pairs {
            let expected = match body {
                Some('1') => Some('a'),
                Some('2') => Some('b'),
                Some('3') => Some('c'),
                other => *other,
            };
            assert_eq!(
                *decoration, expected,
                "the decoration must play the body's frame: {pairs:?}"
            );
        }
        assert!(
            pairs.len() > 1 && pairs[0].0 != pairs[1].0,
            "the body must actually walk: {pairs:?}"
        );
    }

    #[test]
    fn a_decoration_does_not_reach_the_small_ducks() {
        let mut cfg = Config::default();
        cfg.decorations.big = Some("crown".to_string());
        let s = scene_with(&cfg, |_, _| Ok(drawn_decoration('H')));
        // The small ducks are bare: the decoration is a big-duck one.
        let (x, y) = duck_cell(&s, 2, 10, 2);
        assert_ne!(
            s.grid().char_at(x, y),
            Some('H'),
            "a big decoration must not reach the small ducks"
        );
    }

    #[test]
    fn scene_uses_a_drawn_sprite_for_an_assigned_slot() {
        let mut cfg = Config::default();
        cfg.assignments.big_1 = Some("cat".to_string());
        let s = scene_with(&cfg, |kind, name| {
            assert_eq!(kind, DrawnKind::Sprite);
            assert_eq!(name, "cat");
            Ok(drawn_big('C'))
        });
        // The first duck of the parade is the drawn art.
        let (x, y) = duck_cell(&s, 0, 10, 10);
        assert_eq!(
            s.grid().char_at(x, y),
            Some('C'),
            "the first duck must carry the drawn art"
        );
        assert_eq!(
            s.grid().color_at(x, y),
            Some(DRAWN_COLOR),
            "the drawn art keeps its per-cell colour (D16)"
        );
        // The second duck keeps the built-in art.
        let (x2, y2) = duck_cell(&s, 1, 10, 10);
        assert_ne!(
            s.grid().char_at(x2, y2),
            Some('C'),
            "the second duck must stay built-in"
        );
        // The ninth small duck is the last train slot — and it takes the
        // **small** art: a small slot's box is the small duck's frame
        // (audit 2026-10-21 §E1). The stride-locked march moved the old
        // 34 s snapshot, so walk the parade until the last duck stands on
        // the grid (audit 2026-10-23 §F-031).
        let mut cfg = Config::default();
        cfg.assignments.small_9 = Some("bird".to_string());
        let mut s = scene_with(&cfg, |_, _| Ok(drawn_small('B')));
        wait_for_slot_on_screen(&mut s, crate::parade::DUCK_COUNT - 1);
        let (x, y) = duck_cell(&s, crate::parade::DUCK_COUNT - 1, 10, 10);
        assert_eq!(
            s.grid().char_at(x, y),
            Some('B'),
            "the ninth small duck must carry the drawn art"
        );
    }

    /// Advance until the duck in `slot` stands fully on the grid.
    ///
    /// Bounded at two re-entry periods: a slot that never lands is a test
    /// failure, not a hang.
    fn wait_for_slot_on_screen(s: &mut Scene, slot: usize) {
        let (duck_cols, _) = if balloon::BIG_DUCK_SLOTS.contains(&slot) {
            (crate::sprites::BIG_DUCK_COLS, crate::sprites::BIG_DUCK_ROWS)
        } else {
            (
                crate::sprites::SMALL_DUCK_COLS,
                crate::sprites::SMALL_DUCK_ROWS,
            )
        };
        let cols = s.cols() as f32;
        let duck_cols = duck_cols as f32;
        let mut waited = 0.0;
        while s.parade.unwrapped_x(slot) - duck_cols / 2.0 < 0.0
            || s.parade.unwrapped_x(slot) + duck_cols / 2.0 > cols
        {
            s.advance(0.5);
            waited += 0.5;
            assert!(waited < 120.0, "slot {slot} never lands on the grid");
        }
    }

    #[test]
    fn scene_falls_back_to_the_builtin_when_the_asset_is_missing() {
        let missing = |_: DrawnKind, name: &str| {
            Err(DrawnError::NotFound {
                name: name.to_string(),
            })
        };
        let mut cfg = Config::default();
        cfg.assignments.big_1 = Some("ghost".to_string());
        let with = scene_with(&cfg, missing);
        let without = scene_with(&Config::default(), missing);
        assert_eq!(
            with.grid(),
            without.grid(),
            "a missing asset must leave the built-in art in place"
        );
        assert!(
            with.grid().ink_count() > 0,
            "the built-in duck must still be drawn"
        );
        assert_eq!(count_char(with.grid(), 'C'), 0, "no drawn art may appear");
    }

    #[test]
    fn scene_uses_the_assignment_frame_count() {
        let mut cfg = Config::default();
        cfg.assignments.big_1 = Some("cat".to_string());
        // Three frames: the gait must cycle through all of them.
        let mut s = scene_with(&cfg, |_, _| Ok(drawn_big_frames(&['1', '2', '3'])));
        let mut seen: Vec<Option<char>> = Vec::new();
        for _ in 0..6 {
            s.advance(0.2);
            let (x, y) = duck_cell(&s, 0, 10, 10);
            let cell = s.grid().char_at(x, y);
            if !seen.contains(&cell) {
                seen.push(cell);
            }
        }
        assert!(
            seen.contains(&Some('1')) && seen.contains(&Some('2')) && seen.contains(&Some('3')),
            "the three frames must all be shown, got {seen:?}"
        );
        // One frame: the art stands still, the frame index is always 0.
        let mut s = scene_with(&cfg, |_, _| Ok(drawn_big('1')));
        let mut seen: Vec<Option<char>> = Vec::new();
        for _ in 0..6 {
            s.advance(0.2);
            let (x, y) = duck_cell(&s, 0, 10, 10);
            let cell = s.grid().char_at(x, y);
            if !seen.contains(&cell) {
                seen.push(cell);
            }
        }
        assert_eq!(
            seen,
            vec![Some('1')],
            "a one-frame sprite must not change, got {seen:?}"
        );
    }

    #[test]
    fn scene_prefers_an_assigned_balloon_over_the_watched_one() {
        let mut cfg = Config::default();
        cfg.assignments.balloon_1 = Some("party".to_string());
        let mut s = scene_with(&cfg, |kind, name| {
            assert_eq!(kind, DrawnKind::Balloon);
            assert_eq!(name, "party");
            Ok(drawn_balloon('P'))
        });
        // A watched balloon in the same slot must lose against the
        // assignment.
        let mut watched = AsciiSprite::new(9, 9);
        for index in 0..81 {
            watched.set(index % 9, index / 9, 'W', DRAWN_COLOR);
        }
        s.push_balloon(watched.clone());
        s.advance(0.1);
        assert!(
            count_char(s.grid(), 'P') > 0,
            "the assigned balloon must be drawn"
        );
        assert_eq!(
            count_char(s.grid(), 'W'),
            0,
            "the watched balloon must lose against the assignment"
        );
        // Without an assignment the watched balloon is drawn.
        let mut s = scene_with(&Config::default(), |_, _| Ok(drawn_balloon('P')));
        s.push_balloon(watched);
        s.advance(0.1);
        assert!(
            count_char(s.grid(), 'W') > 0,
            "without an assignment the watched balloon must be drawn"
        );
    }

    #[test]
    fn a_balloon_drawn_in_the_editor_reaches_the_parade_unstretched() {
        // The whole chain, and the regression this guards: the balloon is
        // drawn on the editor's disc canvas, saved as a document and drawn
        // by the parade. A 3x3 block at the centre of the disc must reach
        // the grid as a 3x3 block — one art cell per disc cell, with no
        // horizontal stretch. On the old square canvas the same block
        // came out 1.5x wider than it was drawn (audit 2026-10-20 § 3.18).
        let mut cfg = Config::default();
        cfg.assignments.balloon_1 = Some("party".to_string());
        let sprite = editor_balloon_with_a_centre_block(&cfg);
        let s = scene_with(&cfg, |_, _| Ok(sprite.clone()));
        let block = inked_bounds(s.grid(), 'x');
        assert_eq!(
            block,
            Some((3, 3)),
            "the drawn block must reach the grid 3x3, got {block:?}"
        );
    }

    #[test]
    fn a_drawn_balloon_keeps_its_aspect_on_every_monitor() {
        // The art is authored on a 3:2 cell grid (the editor's 16x24 px
        // cells), so its cells must stay as tall on the screen as they
        // were drawn — on every monitor, whatever the cell pitch is.
        let geometry = crate::balloon::balloon_authoring_geometry(36);
        for (cell_w, cell_h) in [(2u32, 3u32), (3, 4), (4, 6), (7, 10)] {
            let (rx, ry) = balloon::balloon_radii_cells(36, cell_w, cell_h);
            let art_px_w = 2.0 * rx * cell_w as f32 / geometry.cols as f32;
            let art_px_h = 2.0 * ry * cell_h as f32 / geometry.rows as f32;
            let skew = (art_px_h / art_px_w - 1.5).abs();
            assert!(
                skew < 0.01,
                "cell {cell_w}x{cell_h}: the art cell is {art_px_w:.2}x{art_px_h:.2} px, \
                 not 2:3 (skew {skew})"
            );
        }
    }

    /// The bounding box `(width, height)` of the cells inked with `ch`, or
    /// `None` when the glyph is not on the grid.
    fn inked_bounds(grid: &Grid, ch: char) -> Option<(usize, usize)> {
        let mut min = (usize::MAX, usize::MAX);
        let mut max = (0usize, 0usize);
        let mut count = 0usize;
        grid.for_each_cell(|c, r, cell, _| {
            if cell != ch {
                return;
            }
            count += 1;
            min = (min.0.min(c), min.1.min(r));
            max = (max.0.max(c), max.1.max(r));
        });
        if count == 0 {
            return None;
        }
        Some((max.0 - min.0 + 1, max.1 - min.1 + 1))
    }

    /// Paint one row of the 3x3 block at the balloon canvas' centre.
    ///
    /// WHY a helper: the nesting rule is two levels at most (audit
    /// 2026-10-21 §F23).
    fn paint_centre_block_row(state: &mut crate::editor::EditorState, row: usize, col0: usize) {
        for col in col0 - 1..=col0 + 1 {
            state.paint(col, row);
        }
    }

    /// A balloon drawn in the editor — a 3x3 block at the centre of the
    /// disc canvas — saved through the editor's own save path.
    fn editor_balloon_with_a_centre_block(cfg: &Config) -> DrawnSprite {
        let mut state = crate::editor::EditorState::new(cfg, std::time::Instant::now);
        state.set_kind(DrawnKind::Balloon);
        let (cols, rows) = state.canvas_size();
        let col0 = cols / 2;
        let row0 = rows / 2;
        for row in row0 - 1..=row0 + 1 {
            paint_centre_block_row(&mut state, row, col0);
        }
        let saved: std::cell::RefCell<Option<DrawnSprite>> = std::cell::RefCell::new(None);
        state.primary_action_with(
            std::time::Instant::now(),
            |_, _| false,
            |doc, _, _| {
                *saved.borrow_mut() = Some(doc.clone());
                Ok(std::path::PathBuf::from("drawn-balloon.json"))
            },
        );
        saved.into_inner().expect("the editor saved the document")
    }

    // --- Extension 1: the content layers ---------------------------------

    /// A scene over a wide grid with a message configured.
    fn scene_with_message(cfg: &Config) -> Scene {
        let mut s = Scene::new(cfg, 400, 120, 4, 6);
        s.advance(0.1);
        s
    }

    /// The number of cells carrying `ch` in the **message's band**.
    ///
    /// The message lives in a band of its own, so the blitter can draw
    /// it over the scene at offset 0 (audit 2026-10-21 §E7).
    fn count_message_char(s: &Scene, ch: char) -> usize {
        let mut n = 0;
        s.message_band.for_each_cell(|_, _, cell, _| {
            if cell == ch {
                n += 1;
            }
        });
        n
    }

    /// The number of cells carrying `ch` in the rows `r0..r1`.
    fn count_char_in(grid: &Grid, ch: char, r0: usize, r1: usize) -> usize {
        let mut n = 0;
        grid.for_each_cell(|_, r, cell, _| {
            if cell == ch && r >= r0 && r < r1 {
                n += 1;
            }
        });
        n
    }

    #[test]
    fn the_message_is_drawn_above_the_grass() {
        let mut cfg = Config::default();
        cfg.message.text = "BRING YOUR OWN DUCKS".to_string();
        let s = scene_with_message(&cfg);
        let ground = s.ground_row() as usize;
        // The band is `ground - 6 ..= ground - 2`.
        let ink = count_message_char(&s, 'X');
        assert!(ink > 40, "the message must render, got {ink} cells");
        // The band lands above the grass.
        let row = s.message_blit().expect("the message band").row_offset;
        assert_eq!(row, ground - 6, "the message must stay above the grass");
        // Nothing of the message lands on the grass itself.
        assert_eq!(
            count_char_in(s.grid(), 'X', ground, ground + 2),
            0,
            "the message must stay above the grass"
        );
    }

    #[test]
    fn the_message_ink_adapts_to_its_band() {
        // Audit 2026-10-23 F-029: white on a dark band, deep navy on a
        // light one — one luminance comparison at the message's own row.
        let mut day = Config::default();
        day.sky.time_of_day = 0.5;
        day.message.text = "HELLO".to_string();
        let mut night = day.clone();
        night.sky.time_of_day = 0.0;
        let day_scene = scene_with_message(&day);
        let night_scene = scene_with_message(&night);
        let row = day_scene.ground_row() + MESSAGE_ABOVE_OFFSET;
        assert_eq!(
            day_scene.message_color(row, 1.0),
            MESSAGE_INK_DARK,
            "a light band must take the dark ink"
        );
        assert_eq!(
            night_scene.message_color(row, 1.0),
            MESSAGE_COLOR,
            "a dark band keeps the white ink"
        );
    }

    #[test]
    fn the_message_is_centred() {
        let mut cfg = Config::default();
        cfg.message.text = "HELLO".to_string();
        let s = scene_with_message(&cfg);
        let mut min = usize::MAX;
        let mut max = 0usize;
        s.message_band.for_each_cell(|c, _, cell, _| {
            if cell == 'X' {
                min = min.min(c);
                max = max.max(c);
            }
        });
        let centre = (min + max) / 2;
        assert!(
            (centre as i64 - s.cols() as i64 / 2).abs() <= 8,
            "the message must be centred: {min}..{max} on {cols}",
            cols = s.cols()
        );
    }

    #[test]
    fn the_message_fades_out() {
        let mut cfg = Config::default();
        cfg.message.text = "GONE SOON".to_string();
        cfg.message.fade_seconds = 1.0;
        let mut s = Scene::new(&cfg, 400, 120, 4, 6);
        let mut plain = Scene::new(&Config::default(), 400, 120, 4, 6);
        s.advance(0.1);
        plain.advance(0.1);
        assert_ne!(
            s.message_band, plain.message_band,
            "the message must show at first"
        );
        // Past the fade the message is gone: the band is the one
        // without a message again.
        s.advance(3.0);
        plain.advance(3.0);
        assert_eq!(
            s.message_band, plain.message_band,
            "the message must not linger"
        );
    }

    #[test]
    fn a_fade_seconds_of_zero_means_always() {
        let mut cfg = Config::default();
        cfg.message.text = "FOREVER".to_string();
        cfg.message.fade_seconds = 0.0;
        let mut s = Scene::new(&cfg, 400, 120, 4, 6);
        s.advance(0.1);
        s.advance(60.0);
        assert!(
            count_message_char(&s, 'X') > 10,
            "a zero fade means the message stays"
        );
    }

    /// A reload keeps the scene's clock.
    ///
    /// WHY: the reload path (`Scene::apply_config_with`) is the
    /// **in-place** one — a host that rebuilt the scene instead would
    /// restart the parade: the train's phase, the balloons' ring and
    /// the clock all reset (audit 2026-10-21 §E8).
    #[test]
    fn a_reload_keeps_the_clock() {
        let mut s = scene160();
        s.advance(1.5);
        let before = s.elapsed();
        assert!(before > 0.0, "the clock must be running");
        let cfg = Config::default();
        s.apply_config_with(
            &cfg,
            |_, _| {
                Err(crate::drawn::DrawnError::NotFound {
                    name: "x".to_string(),
                })
            },
            None,
        );
        assert_eq!(s.elapsed(), before, "the clock must not go backwards");
    }

    /// The message does not scroll with the parade.
    ///
    /// WHY: the message above the grass shares the parade's band, and
    /// every row of that band scrolls with the ducks — so the message
    /// jittered with them, although it is chrome and the layer model
    /// says the HUD must not smear (audit 2026-10-21 §E7).
    #[test]
    fn the_message_does_not_scroll_with_the_parade() {
        let mut cfg = Config::default();
        cfg.message.text = "BRING YOUR OWN DUCKS".to_string();
        let mut s = scene160();
        s.apply_config_with(
            &cfg,
            |_, _| {
                Err(crate::drawn::DrawnError::NotFound {
                    name: "x".to_string(),
                })
            },
            None,
        );
        s.advance(0.1);
        let ground = s.ground_row() as usize;
        let first = render_at(&s, 0);
        let second = render_at(&s, 3);
        // The message is the only **white** thing in its band, so its
        // pixels can be told from the ducks' art, which does scroll.
        let cell = 24usize;
        let white_columns = |fb: &crate::blit::FrameBuffer| -> Vec<usize> {
            let mut columns = Vec::new();
            for (index, pixel) in fb.data.chunks_exact(4).enumerate() {
                let row = index / fb.width as usize;
                if row < (ground - 6) * cell || row >= (ground - 1) * cell {
                    continue;
                }
                if pixel[0] == 255 && pixel[1] == 255 && pixel[2] == 255 {
                    columns.push(index % fb.width as usize);
                }
            }
            columns
        };
        assert!(
            !white_columns(&first).is_empty(),
            "the message must be in the frame"
        );
        assert_eq!(
            white_columns(&first),
            white_columns(&second),
            "the message must not move with the parade"
        );
    }

    #[test]
    fn the_hint_and_the_message_do_not_collide() {
        // The hint lives at `rows - 6 ..= rows - 2`; a message below the
        // grass lives at `ground + 3 ..= ground + 7`. The two bands must
        // not overlap on a grid that is tall enough for both.
        let mut cfg = Config::default();
        cfg.message.text = "CORNER".to_string();
        cfg.message.position = crate::config::MessagePosition::BottomLeft;
        let mut s = Scene::new(&cfg, 400, 120, 4, 6);
        s.advance(0.1);
        let ground = s.ground_row() as usize;
        let rows = s.rows();
        // The message's band is above the hint's band.
        assert!(
            ground as i64 + MESSAGE_BELOW_OFFSET + MESSAGE_FONT_ROWS
                <= rows as i64 - HUD_MARGIN_ROWS,
            "the message band must end before the hint band starts"
        );
        // The hint is still drawn.
        let hint_ink = count_char_in(s.grid(), 'X', rows - 6, rows - 1);
        assert!(hint_ink > 10, "the hint must survive the message");
    }

    #[test]
    fn a_message_below_the_grass_is_skipped_on_a_short_grid() {
        let mut cfg = Config::default();
        cfg.message.text = "CORNER".to_string();
        cfg.message.position = crate::config::MessagePosition::BottomLeft;
        let mut with = Scene::new(&cfg, 160, 45, 12, 24);
        let mut without = Scene::new(&Config::default(), 160, 45, 12, 24);
        with.advance(0.1);
        without.advance(0.1);
        assert!(
            !message_below_fits(
                with.rows() as i64,
                with.ground_row(),
                with.counter_line.is_some()
            ),
            "the test grid must be too short for the bottom band"
        );
        assert_eq!(
            with.message_band, without.message_band,
            "the message must be skipped, not drawn over the hint"
        );
        assert_eq!(
            with.grid(),
            without.grid(),
            "the message must be skipped, not drawn over the hint"
        );
    }

    #[test]
    fn the_sky_is_a_row_ramp() {
        let mut cfg = Config::default();
        cfg.sky.enabled = true;
        cfg.sky.time_of_day = 0.5;
        let mut s = Scene::new(&cfg, 200, 60, 4, 6);
        s.advance(0.1);
        let ramp = s.sky_ramp().expect("the sky must be on");
        assert_eq!(ramp.len(), s.rows(), "one colour per grid row");
        // The rows differ: it is a ramp, not a flat colour.
        assert_ne!(ramp[0], ramp[10], "the ramp must change with the row");
    }

    #[test]
    fn the_sky_is_deterministic_for_a_time_of_day() {
        let mut cfg = Config::default();
        cfg.sky.enabled = true;
        cfg.sky.time_of_day = 0.42;
        let mut a = Scene::new(&cfg, 200, 60, 4, 6);
        let mut b = Scene::new(&cfg, 200, 60, 4, 6);
        a.advance(0.1);
        b.advance(0.1);
        assert_eq!(a.sky_ramp(), b.sky_ramp(), "the sky must be reproducible");
        assert_eq!(a.grid(), b.grid(), "the frame must be reproducible");
    }

    #[test]
    fn the_sky_can_be_turned_off() {
        let mut cfg = Config::default();
        cfg.sky.enabled = true;
        let mut on = Scene::new(&cfg, 200, 60, 4, 6);
        on.advance(0.1);
        assert!(on.sky_ramp().is_some());
        // The built-in default ships the sky ON (audit 2026-10-23
        // §F-049), so "off" is an explicit choice, not the default.
        let mut off_cfg = Config::default();
        off_cfg.sky.enabled = false;
        let mut off = Scene::new(&off_cfg, 200, 60, 4, 6);
        off.advance(0.1);
        assert!(off.sky_ramp().is_none(), "the sky must be off");
        // The grid is identical: the sky is a background, not a layer.
        assert_eq!(on.grid(), off.grid());
    }

    #[test]
    fn the_ducks_stay_readable_over_the_sky() {
        let mut cfg = Config::default();
        cfg.sky.enabled = true;
        cfg.sky.time_of_day = 0.5;
        let mut s = Scene::new(&cfg, 960, 360, 2, 3);
        s.advance(6.0);
        let ground = s.ground_row() as usize;
        // The duck's own colour survives over the sky.
        let mut ducks = 0;
        s.grid().for_each_cell(|_, _, _, color| {
            if color == DUCK_COLOR {
                ducks += 1;
            }
        });
        assert!(
            ducks > 1000,
            "the ducks must stay readable over the sky, got {ducks} cells"
        );
        let _ = ground;
    }

    // --- Extension 1: the custom grass tile -------------------------------

    /// A drawn grass tile of `cols x rows` filled with `ch`.
    fn drawn_grass(cols: usize, rows: usize, ch: char) -> DrawnSprite {
        let mut frame = AsciiSprite::new(cols, rows);
        for index in 0..cols * rows {
            frame.set(index % cols, index / cols, ch, DRAWN_COLOR);
        }
        DrawnSprite {
            kind: DrawnKind::Grass,
            size: None,
            frames: vec![frame],
            reaction: None,
        }
    }

    #[test]
    fn a_drawn_grass_tile_replaces_the_built_in_band() {
        let mut cfg = Config::default();
        cfg.grass.tile = Some("meadow".to_string());
        let mut s = Scene::build(
            &cfg,
            200,
            60,
            4,
            6,
            |kind, name| {
                assert_eq!(kind, DrawnKind::Grass);
                assert_eq!(name, "meadow");
                Ok(drawn_grass(10, 2, 'G'))
            },
            None,
        );
        s.advance(0.1);
        let ground = s.ground_row() as usize;
        // The tile's glyph is on the band.
        assert!(
            count_char(s.grid(), 'G') > 100,
            "the tile must cover the band, got {} cells",
            count_char(s.grid(), 'G')
        );
        // The band's first row is the tile's glyph: the built-in rail is
        // gone. (A balloon may hang over a few cells of it.)
        let tiled = (0..s.cols())
            .filter(|col| s.grid().char_at(*col, ground) == Some('G'))
            .count();
        assert!(
            tiled > s.cols() * 3 / 4,
            "the band must be the tile's, not the built-in rail: {tiled} cells"
        );
    }

    #[test]
    fn a_grass_tile_tiles_across_the_whole_width() {
        let mut cfg = Config::default();
        cfg.grass.tile = Some("meadow".to_string());
        let mut s = Scene::build(
            &cfg,
            200,
            60,
            4,
            6,
            |_, _| Ok(drawn_grass(10, 2, 'G')),
            None,
        );
        s.advance(0.1);
        let ground = s.ground_row() as usize;
        // The first and the last column of the band carry the tile.
        let first = s.grid().char_at(0, ground);
        let last = s.grid().char_at(199, ground);
        assert_eq!(first, Some('G'), "the band must start with the tile");
        assert_eq!(last, Some('G'), "the band must tile to the edge");
    }

    #[test]
    fn a_missing_grass_tile_keeps_the_built_in_band() {
        let missing = |_: DrawnKind, name: &str| {
            Err(DrawnError::NotFound {
                name: name.to_string(),
            })
        };
        let mut cfg = Config::default();
        cfg.grass.tile = Some("ghost".to_string());
        let mut with = Scene::build(&cfg, 200, 60, 4, 6, missing, None);
        let mut without = Scene::build(&Config::default(), 200, 60, 4, 6, missing, None);
        with.advance(0.1);
        without.advance(0.1);
        assert_eq!(with.grid(), without.grid(), "the built-in band must stay");
    }

    // --- Extension 1: the per-duck personality ----------------------------

    #[test]
    fn a_personality_changes_the_bob_but_not_the_train() {
        let mut cfg = Config::default();
        cfg.personality.push(crate::config::PersonalityConfig {
            slot: "small_3".to_string(),
            bob: 3.0,
            gait_speed: 1.0,
            phase_offset: 0,
            hop: 0.0,
            color: None,
        });
        // The grid is wide enough for the whole train: the personal duck
        // (`small_3`, the fifth slot) must be on screen.
        let mut plain = Scene::new(&Config::default(), 1400, 120, 4, 6);
        let mut personal = Scene::new(&cfg, 1400, 120, 4, 6);
        // The bob is a wave: it is zero twice per cycle, so the two
        // scenes are compared over a whole cycle.
        let mut differed = false;
        for step in 0..12 {
            plain.advance(0.05);
            personal.advance(0.05);
            if plain.grid() != personal.grid() {
                differed = true;
            }
            // The train's spacing is untouched at every step.
            let gap = |s: &Scene| {
                s.parade.unwrapped_x(crate::parade::DUCK_COUNT - 1) - s.parade.unwrapped_x(0)
            };
            assert!(
                (gap(&plain) - gap(&personal)).abs() < 1e-3,
                "the train's spacing must not change: {} vs {}",
                gap(&plain),
                gap(&personal)
            );
            let _ = step;
        }
        assert!(differed, "the bob must show");
    }

    #[test]
    fn a_personality_colours_the_duck() {
        let mut cfg = Config::default();
        cfg.personality.push(crate::config::PersonalityConfig {
            slot: "small_1".to_string(),
            bob: 0.0,
            gait_speed: 1.0,
            phase_offset: 0,
            hop: 0.0,
            color: Some([1, 2, 3]),
        });
        let mut s = Scene::new(&cfg, 960, 360, 2, 3);
        s.advance(6.0);
        let mut painted = 0;
        s.grid().for_each_cell(|_, _, _, color| {
            if color == [1, 2, 3] {
                painted += 1;
            }
        });
        assert!(painted > 0, "the personality's colour must be used");
    }

    #[test]
    fn a_personality_for_a_slot_that_does_not_exist_is_dropped() {
        let cfg = Config {
            personality: vec![crate::config::PersonalityConfig {
                slot: "small_99".to_string(),
                bob: 1.0,
                gait_speed: 1.0,
                phase_offset: 0,
                hop: 0.0,
                color: None,
            }],
            ..Config::default()
        };
        // The existence predicate names every asset as present: this test
        // is about the personality, not about a missing assignment.
        let cfg = cfg.sanitized(|_, _| true);
        assert!(
            cfg.personality.is_empty(),
            "the invalid personality must be dropped"
        );
    }

    // --- Extension 1: the live reload ------------------------------------

    #[test]
    fn a_reload_applies_a_scalar_change() {
        let mut s = Scene::new(&Config::default(), 400, 120, 4, 6);
        s.advance(1.0);
        let before = s.parade.speed();
        // 3 s, not the old 10 s: the stride-lock (audit 2026-10-23 F-031)
        // quantizes the speed, and on this 400-column grid both 20 s and
        // 10 s land on the same one-stride-per-cycle 63 cells/s — the
        // reload must cross a quantum boundary to be visible.
        let cfg = Config {
            loop_seconds: 3.0,
            ..Config::default()
        };
        s.apply_config(&cfg, &AssetDirs::default());
        let after = s.parade.speed();
        assert!(
            after > before,
            "the parade must speed up: {before} -> {after}"
        );
    }

    #[test]
    fn a_reload_keeps_the_simulation() {
        let mut s = Scene::new(&Config::default(), 400, 120, 4, 6);
        s.advance(1.5);
        let elapsed = s.elapsed();
        let phase = s.parade_phase();
        s.apply_config(&Config::default(), &AssetDirs::default());
        assert_eq!(s.elapsed(), elapsed, "the clock must not jump");
        assert!(
            (s.parade_phase() - phase).abs() < 1e-3,
            "the parade's phase must not jump"
        );
    }

    #[test]
    fn a_reload_never_panics_on_a_zero_radius() {
        let mut s = Scene::new(&Config::default(), 400, 120, 4, 6);
        let mut cfg = Config::default();
        cfg.balloons.balloon_radius_rows = 0;
        // The repair pass clamps it, exactly as the load does. The
        // existence predicate names every asset as present: this test is
        // about the radius, not about a missing assignment.
        let cfg = cfg.sanitized(|_, _| true);
        assert_eq!(cfg.balloons.balloon_radius_rows, 36);
        s.apply_config(&cfg, &AssetDirs::default());
        s.advance(1.0);
        assert!(s.grid().ink_count() > 0, "the parade must still draw");
    }

    // --- Extension 1: the weather ---------------------------------------

    #[test]
    fn the_falling_duck_stops_the_ducks_behind_it() {
        let mut cfg = Config::default();
        cfg.weather.mode = crate::config::WeatherMode::Snow;
        let mut s = Scene::new(&cfg, 1400, 360, 2, 3);
        s.parade.advance(34.0);
        s.advance(0.1);
        // The snow picks a seeded duck; the parade must hold it.
        s.advance(1.0);
        let held = s.parade.held_slot();
        assert!(held.is_some(), "the snow must hold a duck");
        let held = held.expect("a held duck");
        // The ducks behind the held one do not advance: their lag is at
        // least the held duck's.
        for slot in held..crate::parade::DUCK_COUNT {
            assert!(
                s.parade.lag(slot) >= s.parade.lag(held) - 1e-3,
                "the duck behind must not overtake: slot {slot}"
            );
        }
    }

    #[test]
    fn the_weather_is_drawn_on_the_parade() {
        let mut cfg = Config::default();
        cfg.weather.mode = crate::config::WeatherMode::Rain;
        let mut s = Scene::new(&cfg, 400, 120, 4, 6);
        s.advance(0.5);
        assert!(
            s.grid().ink_count() > 0,
            "the weather must not break the parade"
        );
    }

    #[test]
    fn the_weather_is_deterministic() {
        let mut cfg = Config::default();
        cfg.weather.mode = crate::config::WeatherMode::Auto;
        cfg.weather.seed = 7;
        let mut a = Scene::new(&cfg, 400, 120, 4, 6);
        let mut b = Scene::new(&cfg, 400, 120, 4, 6);
        a.advance(2.0);
        b.advance(2.0);
        assert_eq!(a.grid(), b.grid(), "the weather must be reproducible");
    }

    #[test]
    fn a_reload_re_resolves_the_drawn_assets() {
        let mut cfg = Config::default();
        cfg.assignments.big_1 = Some("cat".to_string());
        let mut s = Scene::build(
            &cfg,
            1400,
            360,
            2,
            3,
            |_, name| {
                if name == "cat" {
                    Ok(drawn_big('C'))
                } else {
                    Err(DrawnError::NotFound {
                        name: name.to_string(),
                    })
                }
            },
            None,
        );
        s.parade.advance(34.0);
        s.advance(0.1);
        let (x, y) = duck_cell(&s, 0, 10, 10);
        assert_eq!(s.grid().char_at(x, y), Some('C'));
        // A reload that drops the assignment falls back to the built-in art.
        s.apply_config(&Config::default(), &AssetDirs::default());
        s.advance(0.1);
        assert_ne!(
            s.grid().char_at(x, y),
            Some('C'),
            "the drawn art must be released"
        );
    }

    #[test]
    fn scene_loads_an_assigned_asset_from_disk() {
        // The whole chain: the config names an asset, the document is read
        // from the drawn folder, the parade draws it. The folder is a temp
        // dir, so the user's real asset folder is never touched.
        let dir = temp_dir("drawn-on-disk");
        let text = serde_json::to_string(&drawn_big('C').to_doc()).expect("serialize");
        std::fs::write(dir.join("cat.json"), text).expect("write document");
        let mut cfg = Config::default();
        cfg.assignments.big_1 = Some("cat".to_string());
        let mut s = Scene::build(
            &cfg,
            1400,
            360,
            2,
            3,
            |kind, name| crate::drawn::load_from(&dir, kind, name),
            None,
        );
        s.parade.advance(34.0);
        s.advance(0.1);
        let (x, y) = duck_cell(&s, 0, 10, 10);
        assert_eq!(
            s.grid().char_at(x, y),
            Some('C'),
            "the document on disk must reach the parade"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    // --- Extension 3: the mouse reactions and the sequencer's clock -------

    /// A wide scene with the whole train on screen.
    ///
    /// The train is 1145 cells long, so a 1400-wide grid is needed to see
    /// both ends at once — and the ducks behind a reacting duck are only
    /// visible there.
    fn wide_scene() -> Scene {
        let mut s = Scene::new(&Config::default(), 1400, 360, 2, 3);
        s.parade.advance(34.0);
        s.advance(0.1);
        s
    }

    /// The same elapsed time runs the same number of steps, however it
    /// is chunked.
    ///
    /// WHY: the `f32` carry made the step count depend on the chunking
    /// — ten advances of 1/30 s ran 40 steps, one advance of 1/3 s ran
    /// 39 and left a whole step in the carry (audit 2026-10-21 §E2).
    /// The parade's phase is the witness: the parade advances **only**
    /// in steps, so the same phase means the same number of steps.
    #[test]
    fn the_step_count_does_not_depend_on_the_chunking() {
        let mut chunked = wide_scene();
        for _ in 0..10 {
            chunked.advance(1.0 / 30.0);
        }
        let mut whole = wide_scene();
        whole.advance(10.0 / 30.0);
        assert_eq!(
            chunked.parade_phase(),
            whole.parade_phase(),
            "the same time runs the same steps"
        );
    }

    /// The rows the rope glyphs occupy in the columns `left..=right`,
    /// above the grass.
    ///
    /// The grass band is written with `|`, `/` and `\` as well, so the
    /// band is excluded: only the ropes are counted.
    fn rope_rows(grid: &Grid, ground: i64, left: i64, right: i64) -> Vec<usize> {
        let mut rows = Vec::new();
        grid.for_each_cell(|c, r, ch, _| {
            let col = c as i64;
            let row = r as i64;
            let in_window = col >= left && col <= right;
            if in_window && row < ground && matches!(ch, '|' | '/' | '\\') {
                rows.push(r);
            }
        });
        rows
    }

    /// The note events of a list of audio events.
    fn note_events(events: &[AudioEvent]) -> Vec<&AudioEvent> {
        events
            .iter()
            .filter(|event| matches!(event, AudioEvent::Note { .. }))
            .collect()
    }

    #[test]
    fn clicking_a_duck_makes_it_jump() {
        let mut s = wide_scene();
        let slot = 0usize;
        let box_ = s.duck_box(slot);
        let click = (box_.left + box_.cols / 2, box_.top + box_.rows / 2);
        assert_eq!(
            s.duck_at(click.0, click.1),
            Some(slot),
            "the duck under the cursor must be found"
        );
        let resting = s.duck_box(slot).top;
        let behind_row = s.duck_box(slot + 1).top;
        let behind_x = s.parade.unwrapped_x(slot + 1);

        s.react(slot, ReactionKind::Jump);
        s.advance(0.2);

        let jumped = s.duck_box(slot).top;
        assert!(
            jumped <= resting - 5,
            "the duck must rise: {jumped} vs {resting}"
        );
        assert_eq!(
            s.duck_box(slot + 1).top,
            behind_row,
            "the duck behind must not move"
        );
        assert!(
            s.parade.unwrapped_x(slot + 1) < behind_x,
            "the duck behind must keep marching"
        );
    }

    #[test]
    fn clicking_empty_ground_does_nothing() {
        let mut s = wide_scene();
        let ground = s.ground_row();
        assert_eq!(s.duck_at(600, ground), None, "the grass is not a duck");
        assert_eq!(s.duck_at(600, 0), None, "the sky is not a duck");
        assert_eq!(s.duck_at(-1, 10), None, "off the grid is not a duck");
        // A click on a slot that does not exist must not touch the train.
        s.react(99, ReactionKind::Jump);
        s.advance(0.1);
        assert!(
            s.take_audio_events().is_empty(),
            "a click on nothing must be silent"
        );
        assert!(
            s.reactions.iter().all(|entry| entry.is_none()),
            "no duck may react to a click on nothing"
        );
    }

    #[test]
    fn the_weather_does_not_cancel_the_mouse_hold() {
        // The weather's own sync used to release the parade as soon as it
        // stopped reporting a fall — which cancelled the mouse's hold.
        let mut s = wide_scene();
        s.react(3, ReactionKind::Stop);
        s.advance(0.1);
        assert_eq!(
            s.parade.held_slot(),
            Some(3),
            "the mouse's hold must be in place"
        );
        s.advance(0.1);
        assert_eq!(
            s.parade.held_slot(),
            Some(3),
            "the weather's sync must not release the mouse's hold"
        );
    }

    #[test]
    fn the_weather_fall_wins_over_the_mouse_hold() {
        let mut cfg = Config::default();
        cfg.weather.mode = crate::config::WeatherMode::Snow;
        let mut s = Scene::new(&cfg, 1400, 360, 2, 3);
        s.parade.advance(34.0);
        s.advance(1.0);
        let falling = s.parade.held_slot().expect("the snow must hold a duck");
        // The mouse holds another duck: the weather's fall wins, so the
        // two holds never fight.
        let other = (falling + 1) % DUCK_COUNT;
        s.react(other, ReactionKind::Stop);
        s.advance(0.1);
        assert_eq!(
            s.parade.held_slot(),
            Some(falling),
            "the weather's falling duck must win the hold"
        );
    }

    #[test]
    fn a_replaced_reaction_ends_the_incumbent_one() {
        // Audit 2026-10-23 F-021: the four ordered reaction pairs on one
        // slot. The Stop-then-Jump pair is the regression — the Stop's
        // hold used to outlive the Jump that replaced it, and the train
        // stood still for the rest of the parade.
        for (first, second) in [
            (ReactionKind::Stop, ReactionKind::Stop),
            (ReactionKind::Stop, ReactionKind::Jump),
            (ReactionKind::Jump, ReactionKind::Stop),
            (ReactionKind::Jump, ReactionKind::Jump),
        ] {
            let mut s = wide_scene();
            let slot = 4usize;
            s.react(slot, first);
            s.advance(0.05);
            s.react(slot, second);
            s.advance(2.0);
            assert_eq!(
                s.parade.held_slot(),
                None,
                "{first:?} then {second:?}: no hold may outlive its reaction"
            );
        }
    }

    #[test]
    fn a_stopped_duck_stops_the_ducks_behind_it() {
        let mut s = wide_scene();
        let slot = 3usize;
        let before: Vec<f32> = (0..DUCK_COUNT)
            .map(|slot| s.parade.unwrapped_x(slot))
            .collect();

        s.react(slot, ReactionKind::Stop);
        s.advance(0.2);

        let after: Vec<f32> = (0..DUCK_COUNT)
            .map(|slot| s.parade.unwrapped_x(slot))
            .collect();
        assert!(
            (after[slot] - before[slot]).abs() < 1e-2,
            "the stopped duck must not advance: {} -> {}",
            before[slot],
            after[slot]
        );
        for other in slot..DUCK_COUNT {
            assert!(
                (after[other] - before[other]).abs() < 1e-2,
                "the duck behind (slot {other}) must stop too"
            );
        }
        for other in 0..slot {
            assert!(
                after[other] < before[other] - 0.01,
                "the duck in front (slot {other}) must keep marching"
            );
        }
    }

    #[test]
    fn the_order_never_changes_during_a_reaction() {
        let mut s = wide_scene();
        s.react(3, ReactionKind::Stop);
        for _ in 0..120 {
            s.advance(1.0 / 60.0);
            assert_the_train_order(&s);
        }
    }

    /// Every gap in the train is at least the nominal gap.
    ///
    /// WHY a helper: the nesting rule is two levels at most, and the
    /// rule is not scoped to production (audit 2026-10-21 §F23).
    fn assert_the_train_order(s: &Scene) {
        for slot in 1..DUCK_COUNT {
            let gap = s.parade.unwrapped_x(slot) - s.parade.unwrapped_x(slot - 1);
            let min = (crate::parade::TRAIN_OFFSETS[slot] - crate::parade::TRAIN_OFFSETS[slot - 1])
                as f32;
            assert!(
                gap >= min - 1e-2,
                "slot {slot}: the gap {gap} must never be eaten (min {min})"
            );
        }
    }

    #[test]
    fn a_reaction_is_frame_rate_independent() {
        // One big advance and many small ones must reach the same state:
        // the reaction lives on the fixed 1/120 s step, so the frame rate
        // cannot change it.
        let mut big = wide_scene();
        let mut small = wide_scene();
        big.react(1, ReactionKind::Jump);
        small.react(1, ReactionKind::Jump);

        let before = big.elapsed();
        big.advance(0.2);
        let steps = ((big.elapsed() - before) / SIM_STEP_SECONDS).round() as u32;
        assert!(steps > 4, "the test must advance a few steps, got {steps}");
        for _ in 0..steps {
            small.advance(SIM_STEP_SECONDS);
        }

        assert_eq!(big.elapsed(), small.elapsed(), "the clocks must match");
        assert_eq!(
            big.duck_box(1).top,
            small.duck_box(1).top,
            "the duck must be at the same height"
        );
    }

    #[test]
    fn the_balloon_follows_a_jumping_duck() {
        let mut with = wide_scene();
        let mut without = wide_scene();
        let (rx, ry) = balloon::balloon_radii_cells(with.cfg.balloons.balloon_radius_rows, 2, 3);
        let slot = 0usize;
        let holder = BALLOON_HOLDER_DUCK[slot];

        with.react(holder, ReactionKind::Jump);
        with.advance(0.2);
        without.advance(0.2);

        let jumped = with.balloon_centre(slot, rx, ry);
        let resting = without.balloon_centre(slot, rx, ry);
        let offset = with.reaction_offset_cells(holder);
        assert!(offset < -1.0, "the duck must be in the air, got {offset}");
        assert!(
            (jumped.1 - resting.1 - offset).abs() < 1e-3,
            "the balloon must rise with the hand: {} vs {offset}",
            jumped.1 - resting.1
        );
        assert!(
            (jumped.0 - resting.0).abs() < 1e-3,
            "the balloon must not move sideways"
        );
    }

    #[test]
    fn the_rope_reaches_the_jumping_hand() {
        let mut with = wide_scene();
        let mut without = wide_scene();
        let holder = BALLOON_HOLDER_DUCK[0];

        with.react(holder, ReactionKind::Jump);
        with.advance(0.2);
        without.advance(0.2);

        // The resting hand of the same moment: the two scenes differ only
        // in the reaction.
        let hand = with.duck_hand(holder);
        let rest = without.duck_hand(holder);
        let offset = with.reaction_offset_cells(holder);
        assert!(offset < -1.0, "the duck must be in the air, got {offset}");
        assert!(
            (hand.1 - rest.1 - offset).abs() < 1e-4,
            "the hand must carry the jump offset: {} vs {offset}",
            hand.1 - rest.1
        );
        assert!(
            (hand.0 - rest.0).abs() < 1e-4,
            "the hand must not move sideways"
        );

        // The rope is drawn from the balloon down to that hand, so the
        // rope's lower end follows the jumping duck.
        let ground = with.ground_row();
        let x = with.duck_hand(holder).0 as i64;
        let jumped = rope_rows(with.grid(), ground, x - 10, x + 10);
        let resting = rope_rows(without.grid(), ground, x - 10, x + 10);
        assert_eq!(jumped.len(), resting.len(), "the rope must keep its length");
        let jumped_low = jumped.iter().min().copied();
        let resting_low = resting.iter().min().copied();
        match (jumped_low, resting_low) {
            (Some(jumped), Some(resting)) => assert_eq!(
                jumped as i64,
                resting as i64 + with.reaction_offset(holder),
                "the rope's lower end must follow the hand"
            ),
            // The rope stops at the duck's own ink before it reaches the
            // hand on this frame: there is no rope to compare.
            _ => assert!(jumped.is_empty() && resting.is_empty()),
        }
    }

    #[test]
    fn a_note_fires_on_its_step() {
        let dir = temp_dir("track");
        let mut track = Track::new();
        track.insert(
            5,
            crate::audio::sequencer::Note {
                pitch: 60,
                duration_steps: 2,
                velocity: 100,
            },
        );
        crate::audio::sequencer::save_track(&dir, "demo", &track).expect("save the track");
        let mut cfg = Config::default();
        cfg.audio.track = Some("demo".to_string());
        cfg.audio.strip_steps = 16;
        let mut s = Scene::build(&cfg, 160, 45, 12, 24, no_drawn_art, Some(dir.clone()));

        s.advance(1.0);
        let events = s.take_audio_events();
        let notes = note_events(&events);
        assert_eq!(
            notes.len(),
            1,
            "the note must fire once per loop: {events:?}"
        );
        match notes[0] {
            AudioEvent::Note {
                pitch,
                velocity,
                duration_seconds,
            } => {
                assert_eq!(*pitch, 60, "the note must keep its pitch");
                assert_eq!(*velocity, 100, "the note must keep its velocity");
                assert!(
                    (duration_seconds - 2.0 / GAIT_FPS).abs() < 1e-6,
                    "the note must ring for its steps, got {duration_seconds}"
                );
            }
            other => panic!("a note event was expected, got {other:?}"),
        }

        // The strip loops: the same slot fires again one loop later.
        s.advance(16.0 / GAIT_FPS);
        let events = s.take_audio_events();
        assert_eq!(
            note_events(&events).len(),
            1,
            "the note must fire again one loop later: {events:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_strip_rebuild_keeps_the_old_track_when_the_file_is_broken() {
        // Audit 2026-10-23 F-023: the strip's **length** changes while the
        // track file cannot be read back — the old track must travel with
        // the rebuild, not vanish with the sequencer it lived in.
        let dir = temp_dir("strip-keep-track");
        let mut track = Track::new();
        track.insert(
            5,
            crate::audio::sequencer::Note {
                pitch: 60,
                duration_steps: 1,
                velocity: 100,
            },
        );
        crate::audio::sequencer::save_track(&dir, "demo", &track).expect("save the track");
        let mut cfg = Config::default();
        cfg.audio.track = Some("demo".to_string());
        cfg.audio.strip_steps = 16;
        let mut s = Scene::build(&cfg, 160, 45, 12, 24, no_drawn_art, Some(dir.clone()));
        assert_eq!(s.sequencer.track().len(), 1, "the track must be loaded");

        // The reload changes the strip's length and breaks the file.
        std::fs::write(dir.join("demo.json"), "{ broken").expect("break the track");
        cfg.audio.strip_steps = 40;
        s.apply_config(&cfg, &AssetDirs::default());
        assert_eq!(s.sequencer.strip_steps(), 40, "the strip must change");
        assert_eq!(
            s.sequencer.track().len(),
            1,
            "the old track must travel with the strip rebuild"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_malformed_track_is_ignored() {
        let dir = temp_dir("bad-track");
        std::fs::write(dir.join("bad.json"), "{ this is not json").expect("write");
        let mut cfg = Config::default();
        cfg.audio.track = Some("bad".to_string());
        let mut s = Scene::build(&cfg, 160, 45, 12, 24, no_drawn_art, Some(dir.clone()));
        s.advance(1.0);
        assert!(s.grid().ink_count() > 0, "the parade must play on");
        assert!(
            s.take_audio_events().is_empty(),
            "a broken track must be silent"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_track_keeps_the_old_one_playing() {
        let dir = temp_dir("keep-track");
        let mut track = Track::new();
        track.insert(
            5,
            crate::audio::sequencer::Note {
                pitch: 60,
                duration_steps: 1,
                velocity: 100,
            },
        );
        crate::audio::sequencer::save_track(&dir, "demo", &track).expect("save the track");
        let mut cfg = Config::default();
        cfg.audio.track = Some("demo".to_string());
        cfg.audio.strip_steps = 16;
        let mut s = Scene::build(&cfg, 160, 45, 12, 24, no_drawn_art, Some(dir.clone()));
        assert_eq!(s.sequencer.track().len(), 1, "the track must be loaded");

        // The track file breaks, and the config is reloaded.
        std::fs::write(dir.join("demo.json"), "{ broken").expect("break the track");
        s.apply_config(&cfg, &AssetDirs::default());
        assert_eq!(
            s.sequencer.track().len(),
            1,
            "the old track must keep playing"
        );
        s.advance(3.0);
        assert!(
            !note_events(&s.take_audio_events()).is_empty(),
            "the old track must still fire"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_gait_step_is_the_sequencers_clock() {
        let mut s = wide_scene();
        assert_eq!(s.gait_step(), 0, "the clock starts at zero");
        s.advance(1.0 / GAIT_FPS);
        assert_eq!(s.gait_step(), 1, "one step is one sprite frame");
        s.advance(1.0 / GAIT_FPS);
        assert_eq!(s.gait_step(), 2, "the clock follows the gait");
    }

    #[test]
    fn a_reaction_clucks_once() {
        let mut s = wide_scene();
        s.react(0, ReactionKind::Jump);
        let events = s.take_audio_events();
        assert_eq!(
            events,
            vec![AudioEvent::Sfx(SfxId::Cluck), AudioEvent::Sfx(SfxId::Chirp)],
            "the reaction clucks (the duck's voice) and chirps (the hop)"
        );
        assert!(
            s.take_audio_events().is_empty(),
            "the events must be taken, not copied"
        );
    }

    #[test]
    fn a_stopping_reaction_clucks_but_does_not_chirp() {
        // The chirp is the hop's sound; a duck that stops does not hop.
        let mut s = wide_scene();
        s.react(0, ReactionKind::Stop);
        assert_eq!(
            s.take_audio_events(),
            vec![AudioEvent::Sfx(SfxId::Cluck)],
            "a stopping duck clucks once"
        );
    }

    #[test]
    fn the_weather_thunders_on_the_strike() {
        // The lightning is the weather's own event: the sound is the audio
        // bundle's, the edge is the weather's.
        let mut cfg = Config::default();
        cfg.weather.mode = crate::config::WeatherMode::Lightning;
        let mut s = Scene::new(&cfg, 400, 120, 4, 6);
        let mut thunders = 0;
        for _ in 0..600 {
            s.advance(1.0 / 60.0);
            thunders += s
                .take_audio_events()
                .iter()
                .filter(|event| **event == AudioEvent::Sfx(SfxId::Thunder))
                .count();
        }
        assert!(thunders > 0, "the lightning must thunder");
    }
}
