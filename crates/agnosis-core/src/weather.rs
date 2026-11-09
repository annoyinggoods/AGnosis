//! Weather: rain, snow, wind and lightning (roadmap Extension 1 §5).
//!
//! The weather is a **pure function of the clock and the seed**. There is
//! no `rand()` in this module, no wall-clock time and no hash-map
//! iteration order in the drawing path: the same config and the same
//! elapsed time always produce the same frame, so the selftest PNG gate
//! stays byte-identical (roadmap §5.3, AC 6).
//!
//! ## The schedule
//!
//! One effect at a time in v1. In `auto` mode the weather follows a
//! **fixed 60 s cycle** (roadmap §5.3):
//!
//! ```text
//!   0.0 –  3.0 s   Lightning   (flash 0.5 s, parade stopped 1.5 s)
//!   3.0 – 24.0 s   Rain
//!  24.0 – 27.0 s   Clear
//!  27.0 – 44.0 s   Snow
//!  44.0 – 48.0 s   Wind
//!  48.0 – 60.0 s   Clear
//! ```
//!
//! A **forced** mode runs its one effect continuously. The forced
//! `lightning` mode is a special case: the strike has to **repeat**,
//! otherwise the effect would freeze the parade forever, so it pulses
//! every [`FORCED_PULSE_SECONDS`]: the flash is on while
//! `phase < FLASH_SECONDS` and the parade is stopped while
//! `phase < STOP_SECONDS` (roadmap §5.5, AC 4).
//!
//! ## The grass rules are per-frame overrides
//!
//! Nothing accumulates and nothing is remembered between frames: the
//! effect ends the instant the weather ends (roadmap §5.5). The band is
//! a **post-process of the grid** — the rules look at the glyphs that are
//! already there, so they work on the built-in band AND on a
//! user-drawn grass tile (roadmap §10.1 AC 2):
//!
//! * the band's **first row** (`ground_row`) is the *tall* row (the
//!   built-in rail row's `/` and `\` blades);
//! * the **remaining rows** (`ground_row + 1 ..= ground_row +
//!   band_rows - 1`) are the *short* row(s) (the built-in wave row's
//!   `~ _ , . -` grass).
//!
//! The `|` separators and the spaces are the **frame** of the band: they
//! are never recoloured, because a band whose frame is lit reads as
//! broken rather than as lit.
//!
//! ## The particles are world-scrolling
//!
//! The rain and the snow are drawn into the scene grid's **parade
//! band**, so they carry the parade's sub-cell scroll: they are part of
//! the world the ducks march through, not chrome, and that is intent
//! (audit 2026-10-23 F-039). The lightning's bolt is drawn into the same
//! band for the same reason.
//!
//! ## The puddles and the splashes
//!
//! The puddles live on the band's **second row** (`ground_row + 1`) —
//! the row under the ducks' feet — and they fill the **gaps** (the
//! spaces between the grass marks) with a muted blue `~`. They never
//! overwrite a grass glyph: they fill the *missing* pieces, which is
//! what the user asked for. A band with no gaps (the built-in wave row
//! is dense) therefore shows the splashes but no puddles.
//!
//! The splashes are **per duck**, phase-shifted by the slot so the train
//! does not splash in lockstep, and they are **dropped when the cell is
//! already occupied** — the grid is one layer deep, so a splash never
//! covers a duck or a grass glyph.
//!
//! ## The duck interaction
//!
//! The snow makes one seeded duck **fall on its bum** for
//! [`FALL_SECONDS`]; the ducks behind it stop (roadmap §5.4, the *hold
//! model* in [`crate::parade`]). This module only reports **which** duck
//! falls and **how far** it has fallen — the scene owns the sprite
//! drawing and drives [`crate::parade::Parade::hold`].
//!
//! ## The sky
//!
//! A storm darkens the sky and the flash brightens it, but the sky
//! belongs to the scene; the weather only exposes [`WeatherState::current`]
//! so the scene can ask what is running.
//!
//! ## Errors
//!
//! The module has no fallible operation, so it carries no `thiserror`
//! enum: a bad config value is repaired (the intensity is clamped) and a
//! non-finite step is ignored.

use crate::config::{WeatherConfig, WeatherMode};
use crate::grid::Grid;
use crate::parade::DUCK_COUNT;
// The ground row is the **scene's** rule (`rows * 4 / 5`): the modules of
// one crate may reference each other — the scene already imports the
// weather — so the weather uses the owner instead of keeping a copy
// that a test has to pin (audit 2026-10-21 §F20).
use crate::scene::ground_row_for;

/// The weather's effect: one at a time in v1 (roadmap §5.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weather {
    /// No effect.
    Clear,
    /// Blue puddles and splashes.
    Rain,
    /// A white band and one duck falling on its bum.
    Snow,
    /// The blades lean the other way.
    Wind,
    /// A yellow/navy flash and a frozen parade.
    Lightning,
}

impl Weather {
    /// The effect's own name, for the trace lines and the CLI.
    pub fn as_str(&self) -> &'static str {
        match self {
            Weather::Clear => "clear",
            Weather::Rain => "rain",
            Weather::Snow => "snow",
            Weather::Wind => "wind",
            Weather::Lightning => "lightning",
        }
    }
}

/// The length of the auto cycle, in seconds (roadmap §5.3).
pub const CYCLE_SECONDS: f32 = 60.0;

/// The auto schedule: the effect and the length of every segment of the
/// cycle. The lengths sum to [`CYCLE_SECONDS`].
///
/// The order is the roadmap's: lightning, rain, clear, snow, wind, clear.
const AUTO_SEGMENTS: [(Weather, f32); 6] = [
    (Weather::Lightning, 3.0),
    (Weather::Rain, 21.0),
    (Weather::Clear, 3.0),
    (Weather::Snow, 17.0),
    (Weather::Wind, 4.0),
    (Weather::Clear, 12.0),
];

/// The lightning's flash: how long the band is lit (roadmap §5.5).
pub const FLASH_SECONDS: f32 = 0.5;

/// The lightning's stop: how long the whole parade stands still. The
/// flash is over before the stop ends (roadmap §5.5, AC 4).
pub const STOP_SECONDS: f32 = 1.5;

/// The forced lightning mode's pulse length: the strike repeats, so the
/// effect cannot freeze the parade forever.
pub const FORCED_PULSE_SECONDS: f32 = 4.0;

/// How long the snow's duck stays on its bum.
pub const FALL_SECONDS: f32 = 1.5;

/// The snow segment's length in the auto cycle (the delay range is
/// derived from it, so the fall always ends inside the segment).
///
/// The value matches the snow entry of [`AUTO_SEGMENTS`]; a pinned test
/// asserts the equality **and** that the segments sum to
/// [`CYCLE_SECONDS`], so a schedule edit that forgets this constant
/// fails the suite rather than cutting a fall short (audit 2026-10-23
/// F-040).
const SNOW_SEGMENT_SECONDS: f32 = 17.0;

/// The slack the snow's fall needs at the end of its segment: the delay
/// is drawn from `0 ..= snow_segment - SNOW_FALL_SLACK`, so the fall
/// always finishes before the segment does.
const SNOW_FALL_SLACK: f32 = 2.0;

/// The muted blue of the puddles (roadmap §5.5).
pub const PUDDLE_COLOR: [u8; 3] = [70, 120, 190];

/// The blue of the splashes around the ducks' feet.
pub const SPLASH_COLOR: [u8; 3] = [110, 160, 220];

/// The near-white of the snow: a pure white on a black background is a
/// blinding rectangle, so the band takes a tint (roadmap §5.5).
pub const SNOW_COLOR: [u8; 3] = [235, 240, 245];

/// The lightning's tall blades: yellow.
pub const LIGHTNING_TALL_COLOR: [u8; 3] = [255, 230, 80];

/// The lightning's short grass: navy blue.
pub const LIGHTNING_SHORT_COLOR: [u8; 3] = [60, 80, 200];

/// The rain drop: a dim blue-grey slash.
pub const RAIN_COLOR: [u8; 3] = [110, 130, 160];

/// The tone the rain lerps toward when it falls across a lit sky (audit
/// 2026-10-23 F-028): the plain rain colour is nearly the day horizon's
/// own blue, and a drop that matches its background is invisible. The
/// darker tone stands out against the day ramp while staying a rain
/// colour against the storm-darkened one.
const RAIN_CONTRAST_COLOR: [u8; 3] = [35, 60, 105];

/// How far the lit-sky rain leans into its contrast tone.
const RAIN_CONTRAST_STRENGTH: f32 = 0.6;

/// Every how many drops a two-cell streak is drawn (audit 2026-10-23
/// F-028): a wall of identical single slashes reads as static, and the
/// occasional longer streak reads as a heavier drop.
const STREAK_EVERY: usize = 4;

/// How fast the streak pattern rotates along the drop list, in shifts
/// per second: the heavier drops travel with the clock instead of
/// sitting on fixed columns forever.
const STREAK_SHIFT_HZ: f32 = 2.0;

/// The lightning's bolt colour: brighter than the flash's own sky, so
/// the strike reads as the source of the light (audit 2026-10-23 F-030).
const BOLT_COLOR: [u8; 3] = [255, 255, 190];

/// How many rows the bolt walks before its next jag (audit 2026-10-23
/// F-030): four rows per step reads as a strike, one row per step reads
/// as a fence.
const BOLT_JAG_ROWS: u32 = 4;

/// The snow flake: white.
pub const SNOW_FLAKE_COLOR: [u8; 3] = [235, 240, 245];

/// The colour the wind's flipped blades fall back to.
///
/// WHY a fallback and not the rule: `flip_blades` keeps the blade's
/// **own** colour (`Grid::color_at` reads it back — audit 2026-10-22
/// §VIS-01), and this constant only covers a cell that carries a glyph
/// but no colour entry, so the write always has a colour to hold.
pub const GRASS_TINT: [u8; 3] = [90, 200, 90];

/// The hard cap on the particle count, whatever the grid width and the
/// intensity are.
pub const MAX_PARTICLES: usize = 2048;

/// How much of the sky a raindrop covers per second: "about a third of
/// the screen per second" (roadmap §5.6).
const RAIN_FALL_FRACTION: f32 = 1.0 / 3.0;

/// How much of the sky a snowflake covers per second: snow is slow.
const SNOW_FALL_FRACTION: f32 = 1.0 / 20.0;

/// The snowflake's horizontal wobble, in cycles per second.
const WOBBLE_HZ: f32 = 0.6;

/// The wobble's amplitude, in columns.
const WOBBLE_COLUMNS: f32 = 1.2;

/// The puddle wave's period, in columns.
const PUDDLE_WAVE_COLUMNS: f32 = 8.0;

/// How fast the puddle wave drifts along the row, in columns per second.
const PUDDLE_DRIFT_COLUMNS: f32 = 6.0;

/// The share of every wave period that is filled with water.
const PUDDLE_FILL: f32 = 0.5;

/// How fast a splash alternates between `~` and `'`.
const SPLASH_HZ: f32 = 2.0;

/// The per-slot phase offset of the splashes, so the train does not
/// splash in lockstep.
const SPLASH_SLOT_PHASE: f32 = 0.37;

/// A tiny deterministic LCG (the Numerical Recipes constants).
///
/// The weather never calls `rand()`: every "random" choice comes from
/// this generator, so the same seed and the same elapsed time always
/// produce the same frame. The generator is advanced only from the fixed
/// simulation step (never from the render path), so the frame is
/// reproducible (roadmap §5.3).
#[derive(Debug, Clone)]
struct Lcg {
    state: u64,
}

impl Lcg {
    /// A generator for the given seed.
    fn new(seed: u64) -> Self {
        Self {
            state: seed.wrapping_add(0x9E37_79B9_7F4A_7C15),
        }
    }

    /// The next value of the sequence.
    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.state
    }

    /// A value in `[0, 1)` (the top 24 bits).
    fn next_f32(&mut self) -> f32 {
        ((self.next_u64() >> 40) as f32) / ((1u64 << 24) as f32)
    }
}

/// One particle: where it starts and how it drifts.
///
/// The particles have **no per-frame state**: a drop's position is
/// `(col0 + wobble, row0 + speed * t) mod sky` — analytic, so it is
/// automatically deterministic and the draw path stays cheap
/// (roadmap §5.6, the analytic-form decision).
#[derive(Debug, Clone, Copy)]
struct Particle {
    /// The column offset, in `[0, 1)` of the grid width.
    col: f32,
    /// The row offset, in `[0, 1)` of the sky height.
    row: f32,
    /// The wobble phase.
    phase: f32,
    /// The flake glyph: snow alternates between `.` and `'`.
    glyph: char,
}

/// The particle pool: the per-drop column and row offsets, drawn once
/// from the seeded generator.
///
/// The pool is built at construction (never in the render path) and its
/// size is the hard cap; the *count* of live particles is the grid width
/// times the intensity, so a narrow grid draws fewer of them.
fn particle_pool(seed: u64) -> Vec<Particle> {
    let mut rng = Lcg::new(seed ^ 0xD1B5_4A32_D192_ED03);
    (0..MAX_PARTICLES)
        .map(|index| {
            let col = rng.next_f32();
            let row = rng.next_f32();
            let phase = rng.next_f32() * std::f32::consts::TAU;
            let glyph = if index % 3 == 0 { '\'' } else { '.' };
            Particle {
                col,
                row,
                phase,
                glyph,
            }
        })
        .collect()
}

/// The weather's own state: the current effect, the PRNG, and the fall
/// event.
///
/// The state is advanced by the **fixed simulation step** (the scene's
/// 1/120 s step) — never in the render path — so the weather's clock and
/// the scene's clock stay in step.
pub struct WeatherState {
    /// Which weather runs.
    mode: WeatherMode,
    /// The particle density, `0.0..=2.0`.
    intensity: f32,
    /// The state's own clock, in seconds.
    ///
    /// WHY `f64`: the clock is advanced by the fixed 1/120 s step, which
    /// is not representable in binary, so an `f32` accumulator drifts —
    /// and the auto schedule reads this clock, so the weather's effects
    /// would drift with it (audit 2026-10-21 §F5).
    time: f64,
    /// The effect that is running right now.
    current: Weather,
    /// The seconds since the current effect started.
    effect_time: f64,
    /// The seeded generator: the only source of "randomness".
    rng: Lcg,
    /// The train slot of the duck that falls on its bum, if any.
    fall_slot: Option<usize>,
    /// The state's clock at the start of the fall.
    fall_start: f64,
    /// The state's clock at the end of the fall.
    fall_end: f64,
    /// The particle pool.
    particles: Vec<Particle>,
}

impl WeatherState {
    /// A state for the given configuration.
    pub fn new(cfg: &WeatherConfig) -> Self {
        let intensity = if cfg.intensity.is_finite() {
            cfg.intensity.clamp(0.0, 2.0)
        } else {
            1.0
        };
        let mut state = Self {
            mode: cfg.mode,
            intensity,
            time: 0.0,
            current: Weather::Clear,
            effect_time: 0.0,
            rng: Lcg::new(cfg.seed),
            fall_slot: None,
            fall_start: 0.0,
            fall_end: 0.0,
            particles: particle_pool(cfg.seed),
        };
        state.sync_effect();
        if state.current == Weather::Snow {
            // A forced snow mode starts its fall right away.
            state.pick_snow_fall(0.0);
        }
        state
    }

    /// Advance the state by one fixed simulation step.
    ///
    /// A non-finite step is ignored: the weather's clock never jumps.
    pub fn advance(&mut self, dt_seconds: f32) {
        if !dt_seconds.is_finite() {
            return;
        }
        self.time += dt_seconds as f64;
        self.sync_effect();
        self.restage_forced_snow();
    }

    /// The effect that is running right now.
    pub fn current(&self) -> Weather {
        self.current
    }

    /// The seconds since the current effect started.
    pub fn effect_time(&self) -> f32 {
        self.effect_time as f32
    }

    /// The train slot of the duck that is falling on its bum (snow), if
    /// any.
    pub fn fallen_duck(&self) -> Option<usize> {
        if self.falling() {
            self.fall_slot
        } else {
            None
        }
    }

    /// True while the whole parade must be stopped (the lightning).
    pub fn parade_stopped(&self) -> bool {
        self.current == Weather::Lightning && self.lightning_phase() < STOP_SECONDS
    }

    /// True while the lightning's flash is on (the first half second of
    /// the strike).
    pub fn flash_active(&self) -> bool {
        self.current == Weather::Lightning && self.lightning_phase() < FLASH_SECONDS
    }

    /// How far the falling duck has fallen, `0.0..=1.0` (the squash
    /// amount).
    pub fn fall_progress(&self) -> f32 {
        if !self.falling() {
            return 0.0;
        }
        ((self.time - self.fall_start) / FALL_SECONDS as f64).clamp(0.0, 1.0) as f32
    }

    /// Draw the weather's particles (rain / snow) BEHIND the parade.
    ///
    /// The particles are drawn in the **sky** — the rows above the grass
    /// band — so a drop never covers a duck and never reaches the ground.
    /// They are drawn behind everything: the scene calls this right after
    /// the sky and before the grass. They scroll with the parade's band
    /// by intent — see the module doc (audit 2026-10-23 F-039).
    ///
    /// `sky_on` is whether the sky ramp is drawn behind the drops: the
    /// rain takes a contrast tone when it is (audit 2026-10-23 F-028), or
    /// it disappears into the day horizon.
    pub fn draw_particles(&self, grid: &mut Grid, time: f32, sky_on: bool) {
        let Some(fall_fraction) = self.particle_fall_fraction() else {
            return;
        };
        let cols = grid.cols();
        let rows = grid.rows();
        if cols == 0 || rows == 0 {
            return;
        }
        let sky = ground_row_for(rows);
        if sky <= 0 {
            return;
        }
        let count = self.particle_count(cols);
        let speed = sky as f32 * fall_fraction;
        for (index, particle) in self.particles.iter().enumerate().take(count) {
            let col = particle_column(particle, cols, self.current, time);
            let row = particle_row(particle, sky, speed, time);
            let Some(glyph) = particle_glyph(particle, self.current) else {
                continue;
            };
            // A drop never overwrites a glyph that is already there.
            if grid.char_at(col, row) != Some(' ') {
                continue;
            }
            grid.set(
                col as i64,
                row as i64,
                glyph,
                particle_color(self.current, sky_on),
            );
            self.draw_streak_tail(grid, index, col, row, time, sky_on);
        }
    }

    /// The streak tail of one drop: every [`STREAK_EVERY`]-th drop draws
    /// a second cell one down-and-right, so the rain has heavier drops
    /// (audit 2026-10-23 F-028). Snow has no streaks, and the tail stays
    /// inside the sky — a drop never reaches the ground band.
    fn draw_streak_tail(
        &self,
        grid: &mut Grid,
        index: usize,
        col: usize,
        row: usize,
        time: f32,
        sky_on: bool,
    ) {
        if self.current != Weather::Rain || !rain_streak(index, time) {
            return;
        }
        let sky = ground_row_for(grid.rows());
        if col + 1 >= grid.cols() || row + 1 >= sky.max(0) as usize {
            return;
        }
        if grid.char_at(col + 1, row + 1) != Some(' ') {
            return;
        }
        grid.set(
            col as i64 + 1,
            row as i64 + 1,
            '/',
            particle_color(Weather::Rain, sky_on),
        );
    }

    /// Draw the lightning's bolt (audit 2026-10-23 F-030): one jagged
    /// column from the top of the sky to the grass, while the flash is
    /// on.
    ///
    /// The strike used to be a strobe with no source; the bolt is the
    /// source. Its column and jags are a hash of the flash's index —
    /// which pulse of the repeating strike this is — so the render stays
    /// deterministic and every pulse of a forced lightning looks
    /// different.
    pub fn draw_bolt(&self, grid: &mut Grid) {
        if !self.flash_active() {
            return;
        }
        let ground = ground_row_for(grid.rows());
        if grid.cols() == 0 || ground <= 0 {
            return;
        }
        let index = self.flash_index();
        let start = bolt_column(index, grid.cols() as i64);
        draw_bolt_path(grid, start, ground, index);
    }

    /// Which pulse of the repeating strike this is (0 inside the auto
    /// cycle's single 3 s segment).
    fn flash_index(&self) -> u64 {
        (self.effect_time / FORCED_PULSE_SECONDS as f64).max(0.0) as u64
    }

    /// Draw the weather's override of the grass band.
    ///
    /// The band's content is whatever is already in the grid — the
    /// built-in band or a user-drawn grass tile — so the rules work on
    /// both (roadmap §10.1 AC 2). The rules are per-frame overrides of
    /// the glyphs that are already there; they need no clock of their
    /// own (the old `_time` parameter was dead — audit F-062).
    pub fn draw_grass(&self, grid: &mut Grid, ground_row: i64, band_rows: usize) {
        match self.current {
            Weather::Snow => tint_band(grid, ground_row, band_rows, SNOW_COLOR),
            Weather::Wind => flip_blades(grid, ground_row),
            Weather::Lightning if self.flash_active() => {
                lightning_band(grid, ground_row, band_rows)
            }
            _ => {}
        }
    }

    /// Draw the puddles and the per-duck splashes on the ground band.
    ///
    /// `feet` holds the foot column of every duck, in slot order.
    pub fn draw_ground(
        &self,
        grid: &mut Grid,
        ground_row: i64,
        band_rows: usize,
        feet: &[i64],
        time: f32,
    ) {
        if self.current != Weather::Rain {
            return;
        }
        draw_puddles(grid, ground_row, band_rows, time);
        draw_splashes(grid, ground_row, feet, time);
    }

    /// The number of live particles: the grid width times the intensity,
    /// capped.
    fn particle_count(&self, cols: usize) -> usize {
        let wanted = (cols as f32 * self.intensity) as usize;
        wanted.min(MAX_PARTICLES).min(self.particles.len())
    }

    /// How fast the particles fall, as a share of the sky per second, or
    /// `None` when the effect has no particles.
    fn particle_fall_fraction(&self) -> Option<f32> {
        match self.current {
            Weather::Rain => Some(RAIN_FALL_FRACTION),
            Weather::Snow => Some(SNOW_FALL_FRACTION),
            _ => None,
        }
    }

    /// The lightning's phase inside its pulse.
    ///
    /// The auto segment is 3 s long, so the modulo is a no-op there; in a
    /// forced mode it turns the strike into a repeating 4 s pulse.
    fn lightning_phase(&self) -> f32 {
        (self.effect_time.rem_euclid(FORCED_PULSE_SECONDS as f64)) as f32
    }

    /// True while the fall is running.
    fn falling(&self) -> bool {
        self.fall_slot.is_some() && self.time >= self.fall_start && self.time < self.fall_end
    }

    /// The state's clock at the start of the current effect.
    ///
    /// In the auto mode that is the start of the segment; in a forced mode
    /// the effect never changes, so it is the start of the state.
    fn effect_start(&self) -> f64 {
        self.time - self.effect_time
    }

    /// Recompute the effect and the time inside it, and stage the snow's
    /// fall when the snow segment starts.
    fn sync_effect(&mut self) {
        let (effect, effect_time) = self.schedule();
        let changed = effect != self.current;
        self.current = effect;
        self.effect_time = effect_time;
        if changed && effect == Weather::Snow {
            // One duck falls per snow segment, and the delay keeps the
            // fall inside the segment.
            self.pick_snow_fall((SNOW_SEGMENT_SECONDS - SNOW_FALL_SLACK) as f64);
        }
    }

    /// The effect and the time inside it, for the current clock.
    fn schedule(&self) -> (Weather, f64) {
        match self.mode {
            WeatherMode::Off => (Weather::Clear, self.time),
            WeatherMode::Auto => auto_segment(self.time),
            WeatherMode::Rain => (Weather::Rain, self.time),
            WeatherMode::Snow => (Weather::Snow, self.time),
            WeatherMode::Wind => (Weather::Wind, self.time),
            WeatherMode::Lightning => (Weather::Lightning, self.time),
        }
    }

    /// Pick the duck that falls and the delay before it falls.
    ///
    /// The delay is drawn from the seeded generator, so the choice is
    /// reproducible (roadmap §5.3, "a random duck is reproducible
    /// randomness"). A zero delay range skips the delay **draw**
    /// entirely (audit 2026-10-23 F-040): the forced-snow start used to
    /// draw a value it then multiplied by zero — a dead draw that shifted
    /// the seeded sequence for nothing.
    fn pick_snow_fall(&mut self, max_delay: f64) {
        let delay = if max_delay > 0.0 {
            self.rng.next_f32() as f64 * max_delay
        } else {
            0.0
        };
        let slot = self.rng.next_u64() as usize % DUCK_COUNT;
        self.stage_fall(slot, self.effect_start() + delay);
    }

    /// Restage the fall in a forced snow mode: the fall repeats, so the
    /// effect keeps showing a duck falling.
    fn restage_forced_snow(&mut self) {
        if self.mode != WeatherMode::Snow {
            return;
        }
        if self.falling() {
            return;
        }
        let slot = self.rng.next_u64() as usize % DUCK_COUNT;
        self.stage_fall(slot, self.time);
    }

    /// Stage the fall of one duck.
    fn stage_fall(&mut self, slot: usize, start: f64) {
        self.fall_slot = Some(slot);
        self.fall_start = start;
        self.fall_end = start + FALL_SECONDS as f64;
    }
}

/// The auto schedule: the effect and the seconds into it at `time`.
fn auto_segment(time: f64) -> (Weather, f64) {
    let position = time.rem_euclid(CYCLE_SECONDS as f64);
    let mut start = 0.0f64;
    for (effect, length) in AUTO_SEGMENTS {
        let length = length as f64;
        if position < start + length {
            return (effect, position - start);
        }
        start += length;
    }
    (Weather::Clear, 0.0)
}

/// True for a glyph that belongs to the grass (not the frame).
fn is_grass(ch: char) -> bool {
    ch != ' ' && ch != '|'
}

/// Tint the whole band with one colour (the snow).
///
/// Flat by construction: one helper per level — the band loop calls the
/// row helper, the row helper walks the columns (audit nesting rule: no
/// loop inside a loop).
fn tint_band(grid: &mut Grid, ground_row: i64, band_rows: usize, color: [u8; 3]) {
    (0..band_rows as i64).for_each(|offset| tint_row(grid, ground_row + offset, color));
}

/// Tint one row of the band, keeping the glyphs.
fn tint_row(grid: &mut Grid, row: i64, color: [u8; 3]) {
    let cols = grid.cols();
    if row < 0 || row as usize >= grid.rows() {
        return;
    }
    for col in 0..cols {
        let Some(ch) = grid.char_at(col, row as usize) else {
            continue;
        };
        if !is_grass(ch) {
            continue;
        }
        grid.set(col as i64, row, ch, color);
    }
}

/// The lightning: the tall blades turn yellow, the short ones navy.
fn lightning_band(grid: &mut Grid, ground_row: i64, band_rows: usize) {
    if band_rows == 0 {
        return;
    }
    tint_row(grid, ground_row, LIGHTNING_TALL_COLOR);
    (1..band_rows as i64)
        .for_each(|offset| tint_row(grid, ground_row + offset, LIGHTNING_SHORT_COLOR));
}

/// The wind: the `\` blades of the tall row lean the other way.
fn flip_blades(grid: &mut Grid, ground_row: i64) {
    let cols = grid.cols();
    if ground_row < 0 || ground_row as usize >= grid.rows() {
        return;
    }
    for col in 0..cols {
        let Some(ch) = grid.char_at(col, ground_row as usize) else {
            continue;
        };
        if ch != '\\' {
            continue;
        }
        // The blade keeps **its own** colour: a drawn grass tile carries
        // the user's palette, and the built-in green painted over it left
        // a patchwork when the wind blew (audit 2026-10-22 §VIS-01).
        let color = grid
            .color_at(col, ground_row as usize)
            .unwrap_or(GRASS_TINT);
        grid.set(col as i64, ground_row, '/', color);
    }
}

/// The puddles: the gaps of the wave row filled with a moving blue `~`.
fn draw_puddles(grid: &mut Grid, ground_row: i64, band_rows: usize, time: f32) {
    if band_rows < 2 {
        // The puddles live on the band's second row; a one-row band has
        // no second row.
        return;
    }
    let row = ground_row + 1;
    let cols = grid.cols();
    if row < 0 || row as usize >= grid.rows() {
        return;
    }
    for col in 0..cols {
        let Some(ch) = grid.char_at(col, row as usize) else {
            continue;
        };
        if ch != ' ' {
            // The puddles never overwrite a grass glyph: they fill the
            // missing pieces.
            continue;
        }
        if !puddle_wave(col as f32, time) {
            continue;
        }
        grid.set(col as i64, row, '~', PUDDLE_COLOR);
    }
}

/// The moving fill of the puddles: the wave drifts along the row.
fn puddle_wave(col: f32, time: f32) -> bool {
    let phase = (col + time * PUDDLE_DRIFT_COLUMNS) / PUDDLE_WAVE_COLUMNS;
    phase - phase.floor() < PUDDLE_FILL
}

/// The splashes: one per duck, phase-shifted by the slot.
fn draw_splashes(grid: &mut Grid, ground_row: i64, feet: &[i64], time: f32) {
    if ground_row < 0 || ground_row as usize >= grid.rows() {
        return;
    }
    for (slot, foot) in feet.iter().enumerate() {
        draw_splash(grid, ground_row, *foot, slot, time);
    }
}

/// One duck's splash: the foot column and the column beside it.
fn draw_splash(grid: &mut Grid, row: i64, foot: i64, slot: usize, time: f32) {
    let phase = time * SPLASH_HZ + slot as f32 * SPLASH_SLOT_PHASE;
    let glyph = if phase - phase.floor() < 0.5 {
        '~'
    } else {
        '\''
    };
    splash_cell(grid, row, foot, glyph);
    splash_cell(grid, row, foot + 1, glyph);
}

/// One splash cell, dropped when the cell is already occupied.
fn splash_cell(grid: &mut Grid, row: i64, col: i64, glyph: char) {
    if col < 0 || col as usize >= grid.cols() {
        return;
    }
    let Some(ch) = grid.char_at(col as usize, row as usize) else {
        return;
    };
    if ch != ' ' {
        // The grid is one layer deep: a splash never covers a duck or a
        // grass glyph.
        return;
    }
    grid.set(col, row, glyph, SPLASH_COLOR);
}

/// The column of a particle.
fn particle_column(particle: &Particle, cols: usize, effect: Weather, time: f32) -> usize {
    let wobble = particle_wobble(particle, effect, time);
    let col = particle.col * cols as f32 + wobble;
    let cols_f = cols as f32;
    col.rem_euclid(cols_f) as usize
}

/// The row of a particle: the analytic fall.
fn particle_row(particle: &Particle, sky: i64, speed: f32, time: f32) -> usize {
    let sky_f = sky as f32;
    let row = particle.row * sky_f + speed * time;
    row.rem_euclid(sky_f) as usize
}

/// The horizontal wobble of a snowflake (the rain falls straight).
fn particle_wobble(particle: &Particle, effect: Weather, time: f32) -> f32 {
    if effect != Weather::Snow {
        return 0.0;
    }
    let wave = (time * WOBBLE_HZ * std::f32::consts::TAU + particle.phase).sin();
    wave * WOBBLE_COLUMNS
}

/// The glyph of a particle.
fn particle_glyph(particle: &Particle, effect: Weather) -> Option<char> {
    match effect {
        Weather::Rain => Some('/'),
        Weather::Snow => Some(particle.glyph),
        _ => None,
    }
}

/// The colour of a particle.
///
/// `sky_on` routes the rain through its contrast tone (audit 2026-10-23
/// F-028); the snow is near-white either way — a night sky is dark enough
/// to carry it.
fn particle_color(effect: Weather, sky_on: bool) -> [u8; 3] {
    match effect {
        Weather::Snow => SNOW_FLAKE_COLOR,
        Weather::Rain if sky_on => {
            lerp_colour(RAIN_COLOR, RAIN_CONTRAST_COLOR, RAIN_CONTRAST_STRENGTH)
        }
        _ => RAIN_COLOR,
    }
}

/// True for the drops that carry a streak tail: every
/// [`STREAK_EVERY`]-th drop, rotating along the list with the clock — a
/// pure function of the index and the time (audit 2026-10-23 F-028), so
/// the heavier drops travel without any randomness.
fn rain_streak(index: usize, time: f32) -> bool {
    (index + (time * STREAK_SHIFT_HZ) as usize).is_multiple_of(STREAK_EVERY)
}

/// The bolt's top column for a flash index: a hash of the index, wrapped
/// into the grid's columns.
fn bolt_column(index: u64, cols: i64) -> i64 {
    let mixed = index
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0xD1B5_4A32_D192_ED03);
    (mixed % cols as u64) as i64
}

/// Walk the bolt from the top of the sky to the grass row: every
/// [`BOLT_JAG_ROWS`] rows the column shifts by a hash-derived step, so
/// the column reads as a strike and not as a post (audit 2026-10-23
/// F-030).
fn draw_bolt_path(grid: &mut Grid, start: i64, ground: i64, index: u64) {
    let mut col = start;
    let mut until_jag = 0u32;
    for row in 0..ground {
        if until_jag == 0 {
            col += bolt_step(index, row);
            until_jag = BOLT_JAG_ROWS;
        }
        until_jag -= 1;
        bolt_cell(grid, col, row);
    }
}

/// The bolt's horizontal step at one row: -1, 0 or +1 by the hash.
fn bolt_step(index: u64, row: i64) -> i64 {
    let mixed = index
        .wrapping_mul(2654435761)
        .wrapping_add(row.max(0) as u64);
    (mixed % 3) as i64 - 1
}

/// One bolt cell: the column is wrapped into the grid (the grid's own
/// `set` wraps columns too, but the wrap is explicit here so the jag
/// arithmetic never sees a column that has left the grid).
fn bolt_cell(grid: &mut Grid, col: i64, row: i64) {
    let wrapped = col.rem_euclid(grid.cols() as i64);
    grid.set(wrapped, row, '|', BOLT_COLOR);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::grid::DEFAULT_FG;

    /// A configuration for the given mode.
    fn cfg(mode: WeatherMode) -> WeatherConfig {
        WeatherConfig {
            mode,
            intensity: 1.0,
            seed: 7,
        }
    }

    /// A grid of the given size, with a sparse grass band: the tall row
    /// carries the blades, the wave row the short grass and its gaps.
    fn banded_grid(cols: usize, rows: usize) -> Grid {
        let mut grid = Grid::new(cols, rows, 1, 1);
        let ground = ground_row_for(rows) as usize;
        for col in 0..cols {
            let ch = match col % 6 {
                0 => '|',
                1 => '/',
                2 => '\\',
                _ => ' ',
            };
            grid.set(col as i64, ground as i64, ch, DEFAULT_FG);
            let wave = match col % 6 {
                0 => '|',
                1 => '~',
                2 => '.',
                _ => ' ',
            };
            grid.set(col as i64, (ground + 1) as i64, wave, DEFAULT_FG);
        }
        grid
    }

    /// The state's clock advanced to `time`, in one-second steps.
    fn advanced(mode: WeatherMode, time: f32) -> WeatherState {
        let mut state = WeatherState::new(&cfg(mode));
        let mut left = time;
        while left > 0.0 {
            let step = left.min(0.1);
            state.advance(step);
            left -= step;
        }
        state
    }

    #[test]
    fn rain_fills_the_gaps_with_blue_waves() {
        let mut grid = banded_grid(40, 20);
        let before: Vec<Option<char>> = (0..40)
            .map(|col| grid.char_at(col, (ground_row_for(20) + 1) as usize))
            .collect();
        let state = WeatherState::new(&cfg(WeatherMode::Rain));
        let ground = ground_row_for(20);
        state.draw_ground(&mut grid, ground, 2, &[], 0.0);
        let mut puddles = 0;
        for (col, was) in before.iter().enumerate() {
            let row = (ground + 1) as usize;
            let ch = grid.char_at(col, row);
            if grid.color_at(col, row) == Some(PUDDLE_COLOR) {
                assert_eq!(ch, Some('~'), "a puddle is a wave");
                assert_eq!(*was, Some(' '), "a puddle fills a gap only");
                puddles += 1;
                continue;
            }
            assert_eq!(ch, *was, "the grass glyphs themselves are unchanged");
        }
        assert!(puddles > 0, "the puddles must fill the gaps");
    }

    #[test]
    fn the_puddles_move() {
        let mut first = banded_grid(40, 20);
        let mut second = banded_grid(40, 20);
        let state = WeatherState::new(&cfg(WeatherMode::Rain));
        let ground = ground_row_for(20);
        state.draw_ground(&mut first, ground, 2, &[], 0.0);
        state.draw_ground(&mut second, ground, 2, &[], 1.5);
        assert_ne!(
            first.to_text(),
            second.to_text(),
            "the puddles must ripple with the clock"
        );
    }

    #[test]
    fn snow_turns_the_grass_white() {
        let mut grid = banded_grid(40, 20);
        let state = WeatherState::new(&cfg(WeatherMode::Snow));
        let ground = ground_row_for(20);
        state.draw_grass(&mut grid, ground, 2);
        assert_eq!(
            grid.color_at(1, ground as usize),
            Some(SNOW_COLOR),
            "the tall blade must be white"
        );
        assert_eq!(
            grid.color_at(1, (ground + 1) as usize),
            Some(SNOW_COLOR),
            "the short grass must be white"
        );
        assert_eq!(
            grid.color_at(0, ground as usize),
            Some(DEFAULT_FG),
            "the separator is the frame of the band: it is never recoloured"
        );
        assert_eq!(
            grid.char_at(1, ground as usize),
            Some('/'),
            "the glyphs stay"
        );
    }

    #[test]
    fn the_wind_flips_the_blades() {
        let mut grid = banded_grid(40, 20);
        let state = WeatherState::new(&cfg(WeatherMode::Wind));
        let ground = ground_row_for(20);
        state.draw_grass(&mut grid, ground, 2);
        assert_eq!(
            grid.char_at(2, ground as usize),
            Some('/'),
            "the lean flips"
        );
        assert_eq!(grid.char_at(1, ground as usize), Some('/'), "the other way");
    }

    #[test]
    fn lightning_recolours_for_half_a_second() {
        let mut grid = banded_grid(40, 20);
        let mut state = WeatherState::new(&cfg(WeatherMode::Lightning));
        let ground = ground_row_for(20);
        state.advance(0.25);
        assert!(state.flash_active(), "the flash is on for half a second");
        state.draw_grass(&mut grid, ground, 2);
        assert_eq!(
            grid.color_at(1, ground as usize),
            Some(LIGHTNING_TALL_COLOR),
            "the tall blades turn yellow"
        );
        assert_eq!(
            grid.color_at(1, (ground + 1) as usize),
            Some(LIGHTNING_SHORT_COLOR),
            "the short grass turns navy"
        );
        // The flash is over: the band is its own colour again.
        let mut plain = banded_grid(40, 20);
        state.advance(0.5);
        assert!(!state.flash_active(), "the flash lasts half a second");
        state.draw_grass(&mut plain, ground, 2);
        assert_eq!(
            plain.color_at(1, ground as usize),
            Some(DEFAULT_FG),
            "the band keeps its own colour after the flash"
        );
    }

    #[test]
    fn the_parade_stops_for_one_and_a_half_seconds() {
        let mut state = WeatherState::new(&cfg(WeatherMode::Lightning));
        state.advance(1.0);
        assert!(state.parade_stopped(), "the parade stands still");
        state.advance(0.75);
        assert!(
            !state.parade_stopped(),
            "the stop is over after one and a half seconds"
        );
        // The forced strike pulses: the parade comes back after four
        // seconds, so the effect cannot freeze it forever.
        state.advance(3.0);
        assert!(state.parade_stopped(), "the strike pulses");
    }

    #[test]
    fn the_weather_is_deterministic_for_a_seed() {
        let mut first = WeatherState::new(&cfg(WeatherMode::Auto));
        let mut second = WeatherState::new(&cfg(WeatherMode::Auto));
        let mut grid_a = banded_grid(40, 20);
        let mut grid_b = banded_grid(40, 20);
        for _ in 0..120 {
            first.advance(0.1);
            second.advance(0.1);
        }
        let ground = ground_row_for(20);
        first.draw_particles(&mut grid_a, 12.0, false);
        second.draw_particles(&mut grid_b, 12.0, false);
        first.draw_grass(&mut grid_a, ground, 2);
        second.draw_grass(&mut grid_b, ground, 2);
        first.draw_ground(&mut grid_a, ground, 2, &[3, 9, 15], 12.0);
        second.draw_ground(&mut grid_b, ground, 2, &[3, 9, 15], 12.0);
        assert_eq!(grid_a.to_text(), grid_b.to_text(), "the frames must match");
    }

    #[test]
    fn the_particles_fall() {
        let mut first = Grid::new(40, 20, 1, 1);
        let mut second = Grid::new(40, 20, 1, 1);
        let state = WeatherState::new(&cfg(WeatherMode::Rain));
        state.draw_particles(&mut first, 0.0, false);
        state.draw_particles(&mut second, 0.5, false);
        assert_ne!(first.to_text(), second.to_text(), "the drops must fall");
        // The drops stay in the sky: they never reach the ground band.
        let ground = ground_row_for(20) as usize;
        assert_no_drops_below(&first, ground, 20, 40);
        assert!(first.ink_count() > 0, "the rain must draw drops");
    }

    /// No drop landed in the ground band.
    ///
    /// WHY two helpers: the nesting rule is two levels at most, and the
    /// rule is not scoped to production (audit 2026-10-21 §F23).
    fn assert_no_drops_below(grid: &Grid, ground: usize, rows: usize, cols: usize) {
        for row in ground..rows {
            assert_no_drop_in_row(grid, row, cols);
        }
    }

    /// No drop landed in one row.
    fn assert_no_drop_in_row(grid: &Grid, row: usize, cols: usize) {
        for col in 0..cols {
            assert_ne!(grid.char_at(col, row), Some('/'), "a drop below the band");
        }
    }

    #[test]
    fn the_effect_follows_the_fixed_cycle() {
        assert_eq!(
            advanced(WeatherMode::Auto, 1.0).current(),
            Weather::Lightning
        );
        assert_eq!(advanced(WeatherMode::Auto, 10.0).current(), Weather::Rain);
        assert_eq!(advanced(WeatherMode::Auto, 25.0).current(), Weather::Clear);
        assert_eq!(advanced(WeatherMode::Auto, 30.0).current(), Weather::Snow);
        assert_eq!(advanced(WeatherMode::Auto, 45.0).current(), Weather::Wind);
        assert_eq!(advanced(WeatherMode::Auto, 55.0).current(), Weather::Clear);
        // The cycle repeats.
        assert_eq!(
            advanced(WeatherMode::Auto, 61.0).current(),
            Weather::Lightning
        );
    }

    #[test]
    fn a_forced_mode_runs_continuously() {
        let state = advanced(WeatherMode::Rain, 300.0);
        assert_eq!(state.current(), Weather::Rain);
        assert!(state.effect_time() > 250.0, "the effect never ends");
        let off = advanced(WeatherMode::Off, 100.0);
        assert_eq!(off.current(), Weather::Clear);
    }

    #[test]
    fn the_snow_fall_picks_a_seeded_duck() {
        let mut first = WeatherState::new(&cfg(WeatherMode::Snow));
        let mut second = WeatherState::new(&cfg(WeatherMode::Snow));
        // The fall lasts a moment and then repeats.
        first.advance(0.1);
        second.advance(0.1);
        let duck = first.fallen_duck();
        assert!(duck.is_some(), "a duck falls on its bum");
        assert_eq!(duck, second.fallen_duck(), "the seed decides the duck");
        assert!(duck.unwrap() < DUCK_COUNT, "the duck is a train slot");
        assert!(first.fall_progress() > 0.0, "the duck is falling");
        first.advance(2.0);
        assert!(first.fall_progress() <= 1.0, "the progress is a fraction");
    }

    #[test]
    fn the_snow_in_the_auto_cycle_falls_once_per_segment() {
        let mut state = WeatherState::new(&cfg(WeatherMode::Auto));
        // Into the snow segment (27 s – 44 s).
        state.advance(28.0);
        assert_eq!(state.current(), Weather::Snow);
        assert!(state.fallen_duck().is_none(), "the fall is delayed");
        // The delay is drawn from the seed, so the fall is watched for
        // rather than waited for.
        let mut fallen = None;
        for _ in 0..160 {
            state.advance(0.1);
            if let Some(slot) = state.fallen_duck() {
                fallen = Some(slot);
                break;
            }
        }
        let slot = fallen.expect("the duck falls inside the snow segment");
        assert!(slot < DUCK_COUNT, "the duck is a train slot");
        assert!(state.fall_progress() > 0.0, "the duck is falling");
    }

    #[test]
    fn a_splash_is_dropped_on_an_occupied_cell() {
        let mut grid = banded_grid(40, 20);
        let state = WeatherState::new(&cfg(WeatherMode::Rain));
        let ground = ground_row_for(20);
        // The foot column 1 holds a blade: the splash must not cover it.
        state.draw_ground(&mut grid, ground, 2, &[1], 0.0);
        assert_eq!(
            grid.char_at(1, ground as usize),
            Some('/'),
            "the blade stays"
        );
        assert_ne!(
            grid.char_at(2, ground as usize),
            Some(' '),
            "the splash lands"
        );
    }

    #[test]
    fn the_ground_row_rule_matches_the_scene() {
        // The scene owns the band, so the weather reads the scene's rule
        // — and this test pins what that rule is (audit 2026-10-21 §F20).
        assert_eq!(ground_row_for(45), 36);
        assert_eq!(ground_row_for(360), 288);
        assert_eq!(
            ground_row_for(45),
            crate::scene::ground_row_for(45),
            "the weather and the scene agree"
        );
    }

    #[test]
    fn the_rain_takes_a_contrast_tone_on_a_lit_sky() {
        // Audit 2026-10-23 F-028: the plain rain colour is nearly the day
        // horizon's own blue, so a lit sky must shift it darker.
        let plain = particle_color(Weather::Rain, false);
        let contrasted = particle_color(Weather::Rain, true);
        assert_ne!(plain, contrasted, "the lit sky must shift the rain");
        assert!(
            contrasted[2] < plain[2],
            "the contrast tone is darker: {contrasted:?} vs {plain:?}"
        );
        assert_eq!(
            particle_color(Weather::Snow, true),
            SNOW_FLAKE_COLOR,
            "the snow keeps its colour either way"
        );
    }

    #[test]
    fn every_fourth_drop_carries_a_streak() {
        // Audit 2026-10-23 F-028: the streak pattern is a pure function
        // of the index and the time — a quarter of the drops at any
        // moment, and the pattern rotates with the clock.
        let at_zero = (0..40).filter(|index| rain_streak(*index, 0.0)).count();
        assert_eq!(at_zero, 10, "a quarter of the drops at time zero");
        let at_half = (0..40).filter(|index| rain_streak(*index, 0.5)).count();
        assert_eq!(at_half, 10, "a quarter of the drops at any time");
        let zero_pattern = (0..4)
            .map(|index| rain_streak(index, 0.0))
            .collect::<Vec<_>>();
        let shifted_pattern = (0..4)
            .map(|index| rain_streak(index, 0.5))
            .collect::<Vec<_>>();
        assert_ne!(
            zero_pattern, shifted_pattern,
            "the heavier drops travel with the clock"
        );
    }

    #[test]
    fn the_flash_draws_a_bolt_from_the_top_of_the_sky() {
        // Audit 2026-10-23 F-030: while the flash is on, one jagged
        // column reaches from row 0 to the grass — the strike has a
        // source, not just a strobe.
        let mut state = WeatherState::new(&cfg(WeatherMode::Lightning));
        state.advance(0.25);
        assert!(state.flash_active());
        let mut grid = banded_grid(40, 20);
        state.draw_bolt(&mut grid);
        let ground = ground_row_for(20) as usize;
        for row in 0..ground {
            let bolts = (0..40)
                .filter(|col| grid.char_at(*col, row) == Some('|'))
                .count();
            assert_eq!(
                bolts, 1,
                "row {row}: the bolt must have exactly one cell, from the top of the sky to the grass"
            );
        }
        // The bolt is over once the flash is: no strobe leftovers.
        let mut later = banded_grid(40, 20);
        state.advance(0.5);
        assert!(!state.flash_active());
        state.draw_bolt(&mut later);
        assert_eq!(
            count_bolt_cells(&later, ground),
            0,
            "no bolt after the flash"
        );
    }

    /// The bolt's cells on the sky rows of a grid, counted.
    ///
    /// WHY the `ground` bound: the banded test grid's own tall row uses
    /// `|` for its separators, and those are not bolt cells.
    fn count_bolt_cells(grid: &Grid, ground: usize) -> usize {
        (0..ground)
            .map(|row| count_bolt_cells_in_row(grid, row))
            .sum()
    }

    /// The bolt's cells on one row.
    fn count_bolt_cells_in_row(grid: &Grid, row: usize) -> usize {
        (0..grid.cols())
            .filter(|col| grid.char_at(*col, row) == Some('|'))
            .count()
    }

    #[test]
    fn the_snow_segment_constant_matches_the_schedule() {
        // Audit 2026-10-23 F-040: SNOW_SEGMENT_SECONDS hand-duplicates
        // the schedule's snow entry — pin the equality, and pin that the
        // segments still sum to the cycle, so a schedule edit that
        // forgets either constant fails here rather than cutting a fall
        // short.
        let snow: f32 = AUTO_SEGMENTS
            .iter()
            .filter(|(effect, _)| matches!(effect, Weather::Snow))
            .map(|(_, length)| *length)
            .sum();
        assert_eq!(snow, SNOW_SEGMENT_SECONDS);
        let total: f32 = AUTO_SEGMENTS.iter().map(|(_, length)| *length).sum();
        assert_eq!(
            total, CYCLE_SECONDS,
            "the auto segments must sum to the cycle"
        );
    }
}
