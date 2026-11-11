//! Duck parade kinematics: one 11-duck train (2 big + 9 small) marching
//! right-to-left with spawn-on-exit re-entry (audit 2026-10-06 §4
//! consequence 6 + addendum note 8).
//!
//! The train is longer than the screen at the audit sprite sizes
//! (~1145 cols of ducks on a 960-col grid), so the old seamless
//! `cols / 11` slot loop is replaced by a fixed train layout with
//! width-driven spacing (audit sketch: big ducks 170 apart, smalls 90).
//! The whole train shares one head coordinate that sweeps left; ducks keep
//! their train offset. When the **tail — its lags included** — fully exits
//! the left edge, the head re-enters at the right edge: the train walks in
//! as one block, and no duck ever wraps while partially on screen
//! (drawing clips, see `WalkSprite::draw` and `draw_rope`).
//!
//! Slot order: slot 0 (big duck 1) heads the train at the smallest x; slot
//! 1 (big duck 2) follows, then slots 2..=10 (smalls) trail right:
//! `unwrapped_x(0) < unwrapped_x(2) < unwrapped_x(10)`.
//!
//! Single-owner layout (audit maintainability): the duck count lives here
//! (`DUCK_COUNT`), the sprite widths come from [`crate::sprites`] (the art
//! owns its own size), and the simulation step belongs to
//! [`crate::pendulum::SUBSTEP_SECONDS`] — this module bakes none of them.
//!
//! ## The hold model (roadmap Extension 1 §5.4)
//!
//! The train is rigid, but a duck can be **held** in place — the snow-fall
//! (roadmap §5.5) or a mouse reaction (roadmap Extension 3 §3.2) need
//! "stop in the spot, stop everything behind, no breaking order or rhythm".
//! The model is a **lag** on top of the train, not a flock model: one
//! `f32` per slot says how far the duck is behind its nominal train
//! position, and the effective position is
//!
//! ```text
//! x(slot) = head_x() + TRAIN_OFFSETS[slot] + lag[slot]
//! ```
//!
//! (the offsets **add** here, because in this module the train marches
//! left and the later slots trail to the right — the roadmap writes the
//! formula for the opposite axis convention).
//!
//! The invariant `lag[slot] >= lag[slot - 1]` is what keeps the order and
//! the minimum gap: the gap between two neighbours is
//! `OFFSET[i+1] - OFFSET[i] + lag[i+1] - lag[i]`, which is at least the
//! nominal gap exactly when the lag never decreases along the train.
//!
//! While a duck is held its lag grows at the march speed (the train keeps
//! moving left, the duck does not) and every duck behind it inherits that
//! lag through the same `max` rule — so the followers stop too, without
//! ever closing the gap. When the hold ends the lag shrinks back to zero
//! at [`CATCHUP_FACTOR`] times the march speed, so the train re-forms in
//! about a second.
//!
//! The lag and the re-entry are one design (user decision 2026-11-10,
//! audit 101126 §11): the re-entry boundary is the tail **with its lag**,
//! so a spam-clicked hold can stretch the cycle but never break it — the
//! held group is simply the last ducks to walk out, and the head waits
//! for them. A clock-derived head with a fixed fold point would teleport
//! on-screen laggards a full cycle instead.
//!
//! The **whole-parade stop** (the lightning) is a different primitive: it
//! freezes the head's *translation* only. The raw clock keeps running, so
//! the gait and the pendulums keep their rhythm — a fully frozen scene
//! reads as a crash, not as a lightning strike.

/// Ducks in the train. Slots 0 and 1 are the big ducks.
pub const DUCK_COUNT: usize = 11;

/// Big-duck slots: the two lead ducks.
pub const BIG_SLOTS: usize = 2;

/// Sprite widths in cells, mirroring [`crate::sprites`] (`BIG_DUCK_COLS` /
/// `SMALL_DUCK_COLS`): the train layout derives from the art it marches, not
/// from baked copies (audit maintainability).
pub const BIG_DUCK_W: usize = crate::sprites::BIG_DUCK_COLS;
pub const SMALL_DUCK_W: usize = crate::sprites::SMALL_DUCK_COLS;

/// Train gaps in cells from the audit sketch: big→big, big→small,
/// small→small.
pub const GAP_BIG_BIG: usize = 20;
pub const GAP_BIG_SMALL: usize = 30;
pub const GAP_SMALL_SMALL: usize = 15;

/// Train-relative duck offsets in cells, derived from the sprite widths and
/// the audit sketch gaps: `[0, 170, 350, 440, ..., 1070]`, train length
/// `1070 + 75 = 1145`. A `const` table (audit performance): the old
/// per-query rebuild ran once per slot per frame.
pub const TRAIN_OFFSETS: [usize; DUCK_COUNT] = train_offsets_const();

/// Default parade loop length in seconds. Used when the caller passes a
/// non-finite or non-positive loop time.
const DEFAULT_LOOP_SECONDS: f32 = 20.0;

/// The stride, in cells: the ground distance the frozen art's gait covers
/// in one 4-frame cycle — the front foot's plant travels from
/// [`crate::sprites::LEG_FRONT_PERCENT`] to
/// [`crate::sprites::LEG_BACK_PERCENT`] of the big duck's frame, so the
/// stride is that span of [`crate::sprites::BIG_DUCK_COLS`] cells
/// (28 % of 150 = 42 cells; audit 2026-10-23 F-031).
pub const STRIDE_CELLS: usize = (crate::sprites::LEG_BACK_PERCENT
    - crate::sprites::LEG_FRONT_PERCENT)
    * crate::sprites::BIG_DUCK_COLS
    / 100;

/// The speed quantum, in cells per second: one stride per 4-frame gait
/// cycle at [`crate::gait::GAIT_FPS`] frames a second
/// (42 cells / (4/6 s) = 63 cells/s).
///
/// WHY a quantum at all: the art's 4-frame cycle moves a foot exactly one
/// stride, so a march speed whose per-cycle distance is not a whole
/// multiple of the stride makes the planted foot **slide** across the
/// ground — the foot-slide finding (audit 2026-10-23 F-031, V-09). Every
/// speed [`Parade::speed`] reports is a whole multiple of this quantum, so
/// one cycle covers exactly `k` strides and the feet stay planted.
pub const SPEED_QUANTUM: f32 = STRIDE_CELLS as f32 * crate::gait::GAIT_FPS / 4.0;

/// How much faster than the march speed the lag shrinks during the
/// recovery after a hold (roadmap §5.4 rule 4).
///
/// The lag is the distance the held duck fell behind, so it shrinks at
/// `CATCHUP_FACTOR * speed` cells per second: the duck covers the gap in
/// `lag / (CATCHUP_FACTOR * speed)` seconds — about a second for a
/// one-and-a-half-second hold.
pub const CATCHUP_FACTOR: f32 = 2.0;

/// Where the head duck starts, as a fraction of the grid width (the audit
/// sketch placement: the lead duck starts just inside the left edge).
const HEAD_START_FRACTION: f32 = 3.0 / 100.0;

/// [`TRAIN_OFFSETS`] as a function (callers that take it by value).
///
/// Test-only: production reads the [`TRAIN_OFFSETS`] table directly
/// (audit dead-code sweep — the function had no production caller).
#[cfg(any(test, feature = "test-support"))]
pub fn train_offsets() -> [usize; DUCK_COUNT] {
    TRAIN_OFFSETS
}

/// Sprite width of the duck in `slot`: the two lead slots carry the big
/// duck, every other slot the small one.
const fn slot_width(slot: usize) -> usize {
    if slot < BIG_SLOTS {
        BIG_DUCK_W
    } else {
        SMALL_DUCK_W
    }
}

/// Train gap that precedes `slot`: big→big, big→small, small→small.
const fn slot_gap(slot: usize) -> usize {
    match slot {
        1 => GAP_BIG_BIG,
        2 => GAP_BIG_SMALL,
        _ => GAP_SMALL_SMALL,
    }
}

/// Build the train offsets as a `const` table.
///
/// Flat by construction (audit nesting rule): the loop body carries no
/// branch at all — the two layout decisions live in [`slot_width`] and
/// [`slot_gap`], one helper per decision.
const fn train_offsets_const() -> [usize; DUCK_COUNT] {
    let mut offsets = [0usize; DUCK_COUNT];
    let mut slot = 1;
    while slot < DUCK_COUNT {
        offsets[slot] = offsets[slot - 1] + slot_width(slot - 1) + slot_gap(slot);
        slot += 1;
    }
    offsets
}

/// Kinematic snapshot of one duck slot.
///
/// Test-only: the production path reads positions straight off
/// [`Parade::unwrapped_x`], so this struct never leaves the test build
/// (audit dead-code sweep).
#[cfg(any(test, feature = "test-support"))]
#[derive(Clone, Debug, PartialEq)]
pub struct DuckPose {
    /// Slot index in `0..DUCK_COUNT`.
    pub slot: usize,
    /// Monotonic unwrapped position in cells. Never wrapped.
    pub unwrapped_x: f32,
    /// Screen position in cells (same as `unwrapped_x`; the parade no
    /// longer wraps — drawing clips at the edges instead).
    pub render_x: f32,
    /// True for slots 0 and 1.
    pub big: bool,
}

/// One 11-duck train marching right to left, re-entering from the right
/// once its tail has fully exited left.
pub struct Parade {
    cols: usize,
    loop_seconds: f32,
    /// The raw simulation clock. It always runs — the gait, the pendulums
    /// and the weather's schedule read the scene's clock, and a stopped
    /// parade must still keep its rhythm.
    ///
    /// WHY `f64`: the fixed step (1/120 s) is **not** representable in
    /// binary, so an `f32` accumulator loses a little every step. The
    /// head's position is `speed * march_time`, so the drift is a drift
    /// of the ducks themselves — measured, **-8.4 s after one hour**,
    /// about 405 cells on a 960-column screen (audit 2026-10-21 §F5).
    time: f64,
    /// The clock the **translation** is measured on. It stops while the
    /// whole parade is stopped, and the parallax layers read it to learn
    /// the parade's travel; the head itself is the [`Parade::head`] state
    /// below, not a product of this clock.
    ///
    /// `f64` for the same reason as [`Parade::time`].
    march_time: f64,
    /// Head (slot 0) coordinate — **explicit state**, not a clock
    /// product. It sweeps from `cols` down to the re-entry boundary
    /// ([`Parade::boundary`]) and re-enters at `cols`.
    ///
    /// WHY state and not `x0 − speed · march_time` folded with
    /// `rem_euclid`: a clock-derived head folds at a **fixed** period,
    /// but the re-entry boundary moves with the holds' lag — a fixed
    /// fold could land while the laggards are still on screen and
    /// teleport them a full cycle (user decision 2026-11-10, audit
    /// 101126 §11). The §F5 f64-drift protection is not needed here: the
    /// head is bounded (`[boundary, cols]`), so `f32` keeps ~1e-4 cells
    /// of precision forever.
    head: f32,
    /// How far each slot is behind its nominal train position, in cells
    /// (roadmap §5.4). The invariant `lag[slot] >= lag[slot - 1]` keeps
    /// the order and the minimum gap.
    lag: [f32; DUCK_COUNT],
    /// The slot that is currently held in place, if any.
    held: Option<usize>,
    /// Whether the whole parade's translation is frozen.
    stopped: bool,
}

impl Parade {
    /// New parade over a `cols`-wide grid completing one crossing per
    /// `loop_seconds`. The train starts with its head just inside the left
    /// edge (`cols * 3 / 100`, the audit sketch placement), tail trailing
    /// off the right edge. Degenerate inputs fall back to `cols >= 1` and a
    /// 20 s loop.
    pub fn new(cols: usize, loop_seconds: f32) -> Self {
        let cols = cols.max(1);
        let loop_seconds = if loop_seconds.is_finite() && loop_seconds > 0.0 {
            loop_seconds
        } else {
            DEFAULT_LOOP_SECONDS
        };
        Self {
            cols,
            loop_seconds,
            time: 0.0,
            march_time: 0.0,
            head: Self::head_start(cols),
            lag: [0.0; DUCK_COUNT],
            held: None,
            stopped: false,
        }
    }

    /// Head start position on a `cols`-wide grid (the audit sketch
    /// placement: the lead duck starts just inside the left edge).
    fn head_start(cols: usize) -> f32 {
        ((cols as f32) * HEAD_START_FRACTION).min(cols as f32)
    }

    /// Grid width in cells.
    ///
    /// Test-only accessor: production reads the grid width from the scene
    /// (audit dead-code sweep — no production caller).
    #[cfg(any(test, feature = "test-support"))]
    pub fn cols(&self) -> usize {
        self.cols
    }

    /// Simulation time in seconds since construction.
    ///
    /// The `f32` view of the `f64` accumulator — the view the public API
    /// has always had.
    ///
    /// Test-only accessor: production drives the clock through
    /// [`Parade::advance`] only (audit dead-code sweep).
    #[cfg(any(test, feature = "test-support"))]
    pub fn time(&self) -> f32 {
        self.time as f32
    }

    /// The clock the translation is driven from.
    ///
    /// It differs from [`Parade::time`] only while the whole parade is
    /// stopped: the raw clock always advances, the march clock does not.
    ///
    /// The parallax layers read it to learn how far the parade has moved
    /// (roadmap Extension 4 §1.1): a layer scrolls at a fraction of the
    /// parade's travel, never on a clock of its own.
    ///
    /// This is the `f32` view; the scene's parallax delta needs the
    /// exact one ([`Parade::march_clock_f64`]), because the difference of
    /// two large `f32` values loses the very digits the delta is made of.
    pub fn march_clock(&self) -> f32 {
        self.march_time as f32
    }

    /// The march clock in full precision.
    ///
    /// WHY this exists: the parallax accumulator takes the **delta** of
    /// the march clock, and that delta is a small number taken from two
    /// large ones. In `f32` the difference of two hours-old clocks keeps
    /// only about a millisecond of the truth, and the backdrops jitter by
    /// it (audit 2026-10-21 §F5).
    pub fn march_clock_f64(&self) -> f64 {
        self.march_time
    }

    /// Advance simulation time; ducks move left at the stride-locked
    /// [`Parade::speed`] cells/s.
    ///
    /// The clocks are accumulated in `f64` so that they do **not** drift
    /// — the fixed step (1/120 s) is not representable in binary, and an
    /// `f32` accumulator lost 8.4 s over an hour (audit 2026-10-21 §F5).
    /// The head itself is bounded state ([`Parade::head`]), so it cannot
    /// drift at all.
    ///
    /// The raw clock always advances; the **march** clock (the one the
    /// parallax reads) advances only while the parade is not stopped, so
    /// a whole-parade stop freezes the translation and nothing else
    /// (roadmap §5.4 rule 5). Non-finite and **non-positive** steps are
    /// both ignored (the scene's own `advance` guard mirrors this one,
    /// audit 2026-10-23 F-035): a negative delta would run the parade
    /// backwards through its clock.
    pub fn advance(&mut self, dt_seconds: f32) {
        if !dt_seconds.is_finite() || dt_seconds <= 0.0 {
            return;
        }
        self.time += dt_seconds as f64;
        if self.stopped {
            return;
        }
        self.march_time += dt_seconds as f64;
        self.update_lags(dt_seconds);
        self.march_the_head(self.speed() * dt_seconds);
    }

    /// Hold a duck in place: it stops, and every duck behind it stops too.
    ///
    /// The held duck's lag grows at the march speed (the train keeps
    /// moving, the duck does not) and the ducks behind it inherit that lag
    /// through the `max` rule, so the order and the minimum gap survive
    /// (roadmap §5.4 rules 1 and 2).
    ///
    /// Holding a slot that is already held simply moves the hold.
    pub fn hold(&mut self, slot: usize) {
        self.held = Some(slot.min(DUCK_COUNT - 1));
    }

    /// Release the held duck: the train catches up again.
    ///
    /// The lag then shrinks back toward zero at [`CATCHUP_FACTOR`] times
    /// the march speed, so the group re-forms in about a second
    /// (roadmap §5.4 rule 4).
    pub fn release(&mut self) {
        self.held = None;
    }

    /// The slot that is currently held, if any.
    pub fn held_slot(&self) -> Option<usize> {
        self.held
    }

    /// True while a duck is held in place (the snow-fall, or a mouse
    /// reaction).
    pub fn is_holding(&self) -> bool {
        self.held.is_some()
    }

    /// Freeze the whole parade's translation (the lightning) or resume it.
    ///
    /// The stop freezes the **translation** only: the raw clock keeps
    /// running, so the gait and the pendulums keep their rhythm
    /// (roadmap §5.4 rule 5).
    pub fn set_stopped(&mut self, stopped: bool) {
        self.stopped = stopped;
    }

    /// True while the parade's translation is frozen.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// How far the slot lags behind its nominal train position, in cells.
    ///
    /// Test-only accessor: production reads the lag through the hold
    /// model itself (audit dead-code sweep — no production caller).
    #[cfg(any(test, feature = "test-support"))]
    pub fn lag(&self, slot: usize) -> f32 {
        self.lag[slot.min(DUCK_COUNT - 1)]
    }

    /// Update the lags for one marched step.
    ///
    /// While a duck is held its lag grows at the march speed; the ducks
    /// behind it inherit it. With no hold the lag shrinks back toward zero
    /// at the catch-up rate, and every slot shrinks by the same amount, so
    /// the trailing group closes as a block and the internal gaps survive.
    fn update_lags(&mut self, march_seconds: f32) {
        let step = self.speed() * march_seconds;
        match self.held {
            Some(slot) => {
                // The held duck falls behind by exactly the marched
                // distance, so it does not advance.
                self.lag[slot] = (self.lag[slot] + step).max(self.lag[slot.saturating_sub(1)]);
                self.propagate_lag(slot);
            }
            None => self.recover_lags(step),
        }
    }

    /// Push the held slot's lag to every duck behind it: the followers
    /// inherit it, so they stop too and never close the gap.
    fn propagate_lag(&mut self, from: usize) {
        for slot in (from + 1)..DUCK_COUNT {
            self.lag[slot] = self.lag[slot].max(self.lag[slot - 1]);
        }
    }

    /// Shrink every lag back toward zero at the catch-up rate.
    fn recover_lags(&mut self, marched: f32) {
        let recover = CATCHUP_FACTOR * marched;
        for slot in 0..DUCK_COUNT {
            self.lag[slot] = (self.lag[slot] - recover).max(0.0);
        }
        self.propagate_lag(0);
    }

    /// The re-entry boundary: the head coordinate at which the **tail
    /// duck** — its lag included — has fully walked out on the left.
    ///
    /// The lag is monotone along the train (`lag[slot] >= lag[slot - 1]`),
    /// so the last duck's lag is the deepest and this one number covers
    /// the whole train: `head - boundary` is exactly the tail's right
    /// edge, and when the head reaches the boundary every duck is
    /// off-screen left. The re-entry jump is invisible by construction —
    /// the train walks out as one block and walks back in as one block,
    /// however far a spam-clicked hold has stretched it (user decision
    /// 2026-11-10, audit 101126 §11).
    fn boundary(&self) -> f32 {
        -(self.train_len() + self.lag[DUCK_COUNT - 1])
    }

    /// The length of one crossing cycle: the nominal sweep plus whatever
    /// the laggards added. Always positive (`cols >= 1`, `train_len > 0`,
    /// lags are non-negative).
    fn cycle_len(&self) -> f32 {
        self.cols as f32 + self.train_len() + self.lag[DUCK_COUNT - 1]
    }

    /// March the head one step and re-enter when the whole train — the
    /// laggards included — has walked out.
    ///
    /// The re-entry lands the head at `cols` minus the overshoot, so the
    /// walk-in continues exactly where the walk-out left off; the loop
    /// covers a step long enough to cross more than one cycle. The
    /// boundary cannot move past the head while the tail is still on
    /// screen: `head - boundary` *is* the tail's right edge, so the
    /// condition fires only when that edge has reached zero.
    fn march_the_head(&mut self, step: f32) {
        self.head -= step;
        while self.head < self.boundary() {
            self.head = self.cols as f32 + (self.head - self.boundary());
        }
    }

    /// Re-base the parade on a new grid width, keeping the phase.
    ///
    /// WHY: the grid carries the cell pitch, so a window resize needs a
    /// new grid — but the SIMULATION must survive it. Restarting the
    /// train at the left edge on every resize reads as a bug: the parade
    /// is the main content of the window and the user only resized it.
    ///
    /// "Phase" here is the head's fraction through the current crossing
    /// cycle, `frac = (head - boundary) / cycle_len` in `[0, 1)`. Holding
    /// that fraction fixed keeps the head at the same point of its sweep
    /// in the NEW cycle, so the train keeps marching instead of jumping
    /// back to the start.
    ///
    /// The **lags** survive the resize unchanged: they are distances in
    /// cells, not a function of the width, and the phase survives with
    /// them (a hold in progress keeps its hold across a resize).
    pub fn resize(&mut self, cols: usize) {
        let cols = cols.max(1);
        if cols == self.cols {
            return;
        }
        // Keep the head's fraction through the current cycle: the train
        // keeps marching instead of jumping. The boundary does not move
        // (the offsets and the lags are width-independent), so only the
        // cycle's width term grows.
        let frac = self.phase_fraction();
        self.cols = cols;
        self.head = self.boundary() + frac * self.cycle_len();
    }

    /// Train length in cells: last offset + small-duck width.
    pub fn train_len(&self) -> f32 {
        (TRAIN_OFFSETS[DUCK_COUNT - 1] + SMALL_DUCK_W) as f32
    }

    /// Change the crossing time, keeping the train where it is.
    ///
    /// This is the live-reload path for `loop_seconds`: the parade's speed
    /// changes without the train moving at all — the head is state, so
    /// there is nothing to re-base.
    ///
    /// A non-finite or non-positive value is refused: the old speed stays.
    pub fn set_loop_seconds(&mut self, loop_seconds: f32) {
        if !(loop_seconds.is_finite() && loop_seconds > 0.0) {
            return;
        }
        if (loop_seconds - self.loop_seconds).abs() < f32::EPSILON {
            return;
        }
        self.loop_seconds = loop_seconds;
        let new_speed = Self::speed_for(self.cols, loop_seconds);
        // F-031: `loop_seconds` is the request; the stride-locked speed is
        // the actual, and the trace says how far the quantum moved it.
        let actual_loop = self.cols as f32 / new_speed;
        crate::trace::trace(&format!(
            "parade: loop {loop_seconds:.2} s requested, stride-locked to {new_speed:.0} cells/s (a {actual_loop:.2} s loop)"
        ));
    }

    /// March speed in cells per second: `cols / loop_seconds`, quantized
    /// to a whole multiple of [`SPEED_QUANTUM`].
    ///
    /// `loop_seconds` stays the **request**: the quantum is what keeps the
    /// art's 4-frame gait cycle covering a whole number of strides, so the
    /// planted feet never slide (audit 2026-10-23 F-031). The nearest
    /// multiple is taken, floored at one quantum — a parade slower than
    /// 63 cells/s would slide its feet on every cycle, so the slowest
    /// parade is one stride per cycle. On the 960-column default
    /// (`loop_seconds = 20`, requested 48 cells/s) the actual is
    /// **63 cells/s**; `set_loop_seconds` traces the difference when it
    /// changes the value.
    pub fn speed(&self) -> f32 {
        Self::speed_for(self.cols, self.loop_seconds)
    }

    /// The stride-locked speed for a `(cols, loop_seconds)` request.
    fn speed_for(cols: usize, loop_seconds: f32) -> f32 {
        let raw = cols as f32 / loop_seconds;
        let strides = (raw / SPEED_QUANTUM).round().max(1.0);
        strides * SPEED_QUANTUM
    }

    /// Head position at the current time: explicit state, marched and
    /// re-entered by [`Parade::advance`] ([`Parade::head`]).
    ///
    /// The head freezes while the whole parade is stopped because
    /// [`Parade::advance`] returns before marching it (roadmap §5.4
    /// rule 5).
    fn head_x(&self) -> f32 {
        self.head
    }

    /// The head's fraction through the current crossing cycle, in
    /// `[0, 1)`: `0` is the head at the re-entry boundary (the tail has
    /// just walked out), `1` is the head entering at the right edge.
    ///
    /// The phase is width-independent, so it is the quantity a resize has
    /// to preserve (see [`Parade::resize`]).
    fn phase_fraction(&self) -> f32 {
        (self.head - self.boundary()) / self.cycle_len()
    }

    /// The head's fraction through the crossing cycle (test helper).
    ///
    /// Test-only accessor: production reads the phase through the
    /// re-basing math in [`Parade::resize`] (audit dead-code sweep — no
    /// production caller).
    #[cfg(any(test, feature = "test-support"))]
    pub fn phase(&self) -> f32 {
        self.phase_fraction()
    }

    /// Train-layout x of one slot (head + fixed offset), unwrapped relative
    /// to the current re-entry cycle. Slot 0 is the on-screen leader with
    /// the smallest x. Values may exceed `cols` — those ducks have not
    /// entered yet and drawing must clip them, never wrap.
    ///
    /// The **lag** is added: a held duck (and the ducks behind it) sit
    /// behind their nominal train position, so the train keeps its order
    /// and its minimum gap (roadmap §5.4).
    pub fn unwrapped_x(&self, slot: usize) -> f32 {
        let slot = slot.min(DUCK_COUNT - 1);
        self.head_x() + TRAIN_OFFSETS[slot] as f32 + self.lag[slot]
    }

    /// Screen column: identical to `unwrapped_x` (kept for API
    /// compatibility; callers clip at the grid edges).
    ///
    /// Test-only accessor: production reads `unwrapped_x` directly
    /// (audit dead-code sweep — no production caller).
    #[cfg(any(test, feature = "test-support"))]
    pub fn render_x(&self, slot: usize) -> f32 {
        self.unwrapped_x(slot)
    }

    /// Poses for slots `0..11`; `big` is true for slots 0 and 1.
    ///
    /// Test-only: the production path asks for one slot at a time through
    /// [`Parade::unwrapped_x`] (audit dead-code sweep).
    #[cfg(any(test, feature = "test-support"))]
    pub fn poses(&self) -> Vec<DuckPose> {
        (0..DUCK_COUNT)
            .map(|slot| DuckPose {
                slot,
                unwrapped_x: self.unwrapped_x(slot),
                render_x: self.render_x(slot),
                big: slot < BIG_SLOTS,
            })
            .collect()
    }

    /// Whole-scene sub-cell scroll offset in px: fractional part of the lead
    /// position times `cell_w`. Always in `[0, cell_w)`.
    pub fn sub_cell_offset_px(&self, cell_w: u32) -> i32 {
        if cell_w == 0 {
            return 0;
        }
        let ux = self.unwrapped_x(0);
        let frac = ux - ux.floor();
        (frac * cell_w as f32) as i32
    }

    /// Reset the lead-duck phase. Non-finite input is ignored.
    ///
    /// Test-only: the phase is fixed at construction in production
    /// (audit dead-code sweep — no production caller).
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_phase(&mut self, x0: f32) {
        if x0.is_finite() {
            self.head = x0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Grid width the parade unit tests march over (the 1080p grid is 960
    /// cells wide).
    const TEST_COLS: usize = 960;
    /// Parade loop length the unit tests use.
    const TEST_LOOP_SECONDS: f32 = 20.0;
    /// Grid width of the small-grid pose test.
    const NARROW_COLS: usize = 160;

    #[test]
    fn train_layout_matches_audit_sketch() {
        let offsets = train_offsets();
        assert_eq!(
            offsets,
            [0, 170, 350, 440, 530, 620, 710, 800, 890, 980, 1070]
        );
        let p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        let want_len = (TRAIN_OFFSETS[DUCK_COUNT - 1] + SMALL_DUCK_W) as f32;
        assert!((p.train_len() - want_len).abs() < 1e-4);
    }

    /// The fixed simulation step, the one the scene drives the parade
    /// with.
    const FIXED_STEP: f32 = 1.0 / 120.0;

    /// The clock must not drift over a simulated hour.
    ///
    /// WHY: the fixed step is not representable in binary, so an `f32`
    /// accumulator lost a little every step — measured, **-8.4 s after
    /// one hour**, which is 405 cells of duck drift on a 960-column
    /// screen (audit 2026-10-21 §F5).
    #[test]
    fn the_clock_does_not_drift_over_an_hour() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        for _ in 0..(3600 * 120) {
            p.advance(FIXED_STEP);
        }
        let expected = 3600.0 * 120.0 * FIXED_STEP as f64;
        let got = p.march_clock_f64();
        assert!(
            (got - expected).abs() < 1e-3,
            "the clock drifted to {got} instead of {expected}"
        );
        assert!(
            (p.time() - 3600.0).abs() < 0.01,
            "the public clock drifted to {}",
            p.time()
        );
    }

    /// Chunking the step must not change the clock.
    ///
    /// The chunked clock is the `f64` sum of the very same steps, so it
    /// differs from a single one-second step by the step's own
    /// quantisation — and by nothing else.
    #[test]
    fn chunking_the_step_does_not_change_the_clock() {
        let mut chunked = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        for _ in 0..120 {
            chunked.advance(FIXED_STEP);
        }
        let mut whole = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        whole.advance(1.0);

        let reference: f64 = (0..120).map(|_| FIXED_STEP as f64).sum();
        assert!(
            (chunked.march_clock_f64() - reference).abs() < 1e-12,
            "the chunked clock drifted to {} instead of {reference}",
            chunked.march_clock_f64()
        );
        assert!(
            (whole.march_clock_f64() - 1.0).abs() < 1e-12,
            "the whole-step clock drifted to {}",
            whole.march_clock_f64()
        );
        assert!(
            (chunked.march_clock_f64() - whole.march_clock_f64()).abs() < 1e-6,
            "chunking changed the clock"
        );
    }

    #[test]
    fn slot_order_heads_left_with_smalls_trailing() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(7.25);
        assert!(
            p.unwrapped_x(0) < p.unwrapped_x(1),
            "slot 0 must head the train"
        );
        assert!(
            p.unwrapped_x(1) < p.unwrapped_x(2),
            "big 2 must sit right of big 1"
        );
        assert!(
            p.unwrapped_x(2) < p.unwrapped_x(10),
            "smalls must trail right of the bigs"
        );
    }

    #[test]
    fn off_screen_ducks_keep_unwrapped_positions() {
        // The train is longer than the screen: trailing ducks sit beyond
        // `cols` and must NOT wrap into view.
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(1.0);
        let last = p.unwrapped_x(10);
        assert!(
            last > p.cols() as f32,
            "tail duck must still be off-screen right, got {last}"
        );
    }

    #[test]
    fn tail_exits_left_then_head_reenters_right() {
        let p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        // Head starts at cols*3/100; the tail (head + 1070) fully exits
        // after (x0 + train_len) / speed seconds, at which moment the head
        // respawns at the right edge.
        let x0 = TEST_COLS as f32 * HEAD_START_FRACTION;
        let t_entry = (x0 + p.train_len()) / p.speed();
        let mut p2 = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p2.advance(t_entry - 0.05);
        let tail = p2.unwrapped_x(10);
        assert!(
            tail + SMALL_DUCK_W as f32 > 0.0 && tail < 0.0,
            "tail must be straddling the left edge, got {tail}"
        );
        p2.advance(0.1);
        let head = p2.unwrapped_x(0);
        assert!(
            (head - TEST_COLS as f32).abs() < 5.0,
            "head must re-enter at the right edge, got {head}"
        );
    }

    #[test]
    fn full_period_returns_every_duck_to_start() {
        let cols = TEST_COLS + 1;
        let mut p = Parade::new(cols, TEST_LOOP_SECONDS);
        let before: Vec<f32> = (0..DUCK_COUNT).map(|s| p.render_x(s)).collect();
        // The period is derived from the stride-locked speed (audit
        // 2026-10-23 F-031): `loop_seconds` is only the request, and the
        // request-based period this test used to compute no longer returns
        // the train to its start once the quantum moves the speed.
        let period = (cols as f32 + p.train_len()) / p.speed();
        p.advance(period);
        for (s, start) in before.iter().enumerate() {
            assert!((p.render_x(s) - start).abs() < 1e-2, "slot {s}");
        }
    }

    #[test]
    fn speed_is_stride_locked() {
        // F-031: every speed is a whole multiple of the quantum, so one
        // 4-frame gait cycle covers a whole number of strides and the
        // planted feet never slide. The default (960 cols, 20 s request)
        // lands on one stride per cycle: 63 cells/s.
        let p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        assert_eq!(p.speed(), 63.0, "the default must be one stride per cycle");
        assert_eq!(STRIDE_CELLS, 42, "28 % of the 150-cell big duck frame");
        for (cols, loop_seconds) in [
            (TEST_COLS, TEST_LOOP_SECONDS),
            (1400usize, 20.0f32),
            (160, 20.0),
            (960, 7.5),
            (333, 13.0),
        ] {
            let p = Parade::new(cols, loop_seconds);
            assert!(
                p.speed() >= SPEED_QUANTUM,
                "({cols}, {loop_seconds}): never slower than one stride per cycle"
            );
            let cycle_cells = p.speed() * 4.0 / crate::gait::GAIT_FPS;
            let strides = cycle_cells / STRIDE_CELLS as f32;
            assert!(
                (strides - strides.round()).abs() < 1e-3,
                "({cols}, {loop_seconds}): {cycle_cells:.1} cells per cycle is {strides} strides"
            );
        }
    }

    #[test]
    fn a_non_positive_step_is_ignored() {
        // F-035: the scene's own guard mirrors this one — a negative delta
        // must not run the clock (or the train) backwards.
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(1.0);
        let before = (p.time(), p.march_clock(), p.unwrapped_x(0));
        p.advance(-0.5);
        p.advance(0.0);
        assert_eq!(
            (p.time(), p.march_clock(), p.unwrapped_x(0)),
            before,
            "a non-positive step must change nothing"
        );
    }

    #[test]
    fn unwrapped_decreases_while_marching() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.set_phase(500.0);
        let a = p.unwrapped_x(0);
        p.advance(1.0);
        let b = p.unwrapped_x(0);
        assert!(b < a, "{b} not left of {a}");
    }

    #[test]
    fn sub_cell_offset_stays_in_range() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        for _ in 0..400 {
            p.advance(0.037);
            let off = p.sub_cell_offset_px(2);
            assert!((0..2).contains(&off), "offset {off}");
        }
        assert_eq!(
            Parade::new(TEST_COLS, TEST_LOOP_SECONDS).sub_cell_offset_px(0),
            0
        );
    }

    #[test]
    fn poses_flag_big_slots() {
        let p = Parade::new(NARROW_COLS, TEST_LOOP_SECONDS);
        let poses = p.poses();
        assert!(poses[0].big && poses[1].big);
        assert!(poses.iter().skip(2).all(|d| !d.big));
        assert_eq!(p.time(), 0.0);
        assert_eq!(p.cols(), NARROW_COLS);
    }

    #[test]
    fn resize_keeps_the_phase_and_does_not_restart_the_train() {
        // A resize must re-base the train, not restart it: the head keeps
        // its place in the crossing cycle, so the parade keeps marching.
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(3.0);
        let phase = p.phase();
        p.resize(640);
        assert_eq!(p.cols(), 640);
        assert!(
            (p.phase() - phase).abs() < 1e-4,
            "phase {phase} must survive the resize, got {}",
            p.phase()
        );
        // A rebuild would restart the train at the left edge.
        let fresh = Parade::new(640, TEST_LOOP_SECONDS);
        assert!(
            (fresh.phase() - phase).abs() > 1e-3,
            "the resize must not restart the train (fresh phase {})",
            fresh.phase()
        );
        // The train keeps marching after the re-basing.
        let head = p.unwrapped_x(0);
        p.advance(1.0);
        assert!(
            p.unwrapped_x(0) < head,
            "the train must keep marching after the resize"
        );
    }

    #[test]
    fn resize_keeps_the_phase_for_a_wider_grid_too() {
        let mut p = Parade::new(NARROW_COLS, TEST_LOOP_SECONDS);
        p.advance(1.25);
        let phase = p.phase();
        p.resize(TEST_COLS);
        assert!(
            (p.phase() - phase).abs() < 1e-4,
            "phase {phase} must survive the resize, got {}",
            p.phase()
        );
        assert_eq!(p.cols(), TEST_COLS);
    }

    #[test]
    fn resize_to_the_same_width_is_a_no_op() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(2.0);
        let head = p.unwrapped_x(0);
        let phase = p.phase();
        p.resize(TEST_COLS);
        assert_eq!(
            p.unwrapped_x(0),
            head,
            "a same-width resize must not move the train"
        );
        assert_eq!(p.phase(), phase);
        assert_eq!(p.cols(), TEST_COLS);
    }

    #[test]
    fn resize_clamps_the_width_to_one() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(1.0);
        let phase = p.phase();
        p.resize(0);
        assert_eq!(p.cols(), 1, "the width clamps like `Parade::new`");
        assert!(p.phase().is_finite());
        assert!((p.phase() - phase).abs() < 1e-4, "the phase still survives");
    }

    /// The position tolerance of the hold tests: the head's analytic
    /// position goes through a `rem_euclid` over a value around a thousand,
    /// so a few ten-thousandths of a cell are the float noise floor.
    const HOLD_EPS: f32 = 1e-2;

    #[test]
    fn a_held_duck_does_not_advance_while_the_train_moves() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(1.0);
        p.hold(4);
        let held_x = p.unwrapped_x(4);
        let head_x = p.unwrapped_x(0);
        p.advance(0.5);
        assert!(
            (p.unwrapped_x(4) - held_x).abs() < HOLD_EPS,
            "the held duck must not advance: {} -> {}",
            held_x,
            p.unwrapped_x(4)
        );
        assert!(
            p.unwrapped_x(0) < head_x,
            "the train keeps marching while a duck is held"
        );
    }

    #[test]
    fn the_held_lag_grows_at_the_march_speed() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.hold(3);
        p.advance(0.5);
        let want = p.speed() * 0.5;
        assert!(
            (p.lag(3) - want).abs() < HOLD_EPS,
            "lag {} must be the marched distance {want}",
            p.lag(3)
        );
        assert_eq!(p.lag(0), 0.0, "the ducks in front keep their lag");
    }

    #[test]
    fn the_ducks_behind_a_held_duck_stop_too() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(1.0);
        p.hold(4);
        let before: Vec<f32> = (0..DUCK_COUNT).map(|s| p.unwrapped_x(s)).collect();
        p.advance(0.5);
        for (slot, x) in before.iter().enumerate().skip(4) {
            assert!(
                (p.unwrapped_x(slot) - x).abs() < HOLD_EPS,
                "slot {slot} must stop too"
            );
        }
        for (slot, x) in before.iter().enumerate().take(4) {
            assert!(p.unwrapped_x(slot) < *x, "slot {slot} must keep marching");
        }
    }

    #[test]
    fn the_order_never_changes_during_a_hold() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.hold(3);
        for _ in 0..200 {
            p.advance(1.0 / 60.0);
            assert_the_train_order(&p);
        }
    }

    /// Every gap in the train is at least the nominal gap.
    ///
    /// WHY a helper: the nesting rule is two levels at most, and the
    /// rule is not scoped to production (audit 2026-10-21 §F23).
    fn assert_the_train_order(p: &Parade) {
        for slot in 1..DUCK_COUNT {
            let gap = p.unwrapped_x(slot) - p.unwrapped_x(slot - 1);
            let min = (TRAIN_OFFSETS[slot] - TRAIN_OFFSETS[slot - 1]) as f32;
            assert!(
                gap >= min - HOLD_EPS,
                "slot {slot}: the gap {gap} must never be eaten (min {min})"
            );
        }
    }

    /// Every duck's position, front to back.
    fn positions(p: &Parade) -> Vec<f32> {
        (0..DUCK_COUNT).map(|slot| p.unwrapped_x(slot)).collect()
    }

    /// True when any duck's position grew since the last step — the train
    /// re-entered.
    fn jumped_up(before: &[f32], after: &[f32]) -> bool {
        (0..DUCK_COUNT).any(|slot| after[slot] > before[slot] + HOLD_EPS)
    }

    /// True when no duck was on screen beyond the step margin: every art
    /// run is fully left of the grid (within `margin` of having walked
    /// out — the wrap is decided per step, and a duck that close to the
    /// edge finishes exiting inside the very step the wrap fires, so it
    /// is never *rendered* on the far side) or fully right of it.
    fn nothing_on_screen(before: &[f32], cols: usize, margin: f32) -> bool {
        (0..DUCK_COUNT).all(|slot| {
            let right = before[slot] + slot_width(slot) as f32;
            right <= margin || before[slot] >= cols as f32
        })
    }

    #[test]
    fn the_train_re_enters_only_after_its_laggards_walk_out() {
        // The user's rule (2026-11-10, audit 101126 §11): the head walks
        // in only after the last duck — the laggards a hold created
        // included — has walked out. A spam-clicked hold must never
        // teleport a visible duck: a re-entry may happen only while
        // every duck is off screen (a duck within one step of the exit
        // finishes exiting inside the same step).
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(1.0);
        let mut before = positions(&p);
        // 100 s of spam: hold five frames out of six, like a fast
        // clicking user — the lag grows on net and stretches the cycle.
        for frame in 0..6000 {
            if frame % 6 != 5 {
                p.hold(5);
            } else {
                p.release();
            }
            p.advance(1.0 / 60.0);
            let after = positions(&p);
            if jumped_up(&before, &after) {
                // The tail walks out at up to (1 + CATCHUP) x speed when
                // the group is recovering.
                let margin = (1.0 + CATCHUP_FACTOR) * p.speed() / 60.0;
                assert!(
                    nothing_on_screen(&before, TEST_COLS, margin),
                    "the train re-entered under a visible duck: before {before:?} frame {frame}"
                );
            }
            assert_the_train_order(&p);
            before = after;
        }
    }

    #[test]
    fn a_hold_waits_out_the_re_entry_instead_of_teleporting() {
        // A hold that outlives one nominal cycle must not span a re-entry
        // at all: the head waits until the held group has walked out, so
        // nothing moves right by even a cell while the user holds
        // (supersedes the documented +period jump of audit 101126 §C7).
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(1.0);
        p.hold(5);
        let mut before = positions(&p);
        for _ in 0..((TEST_LOOP_SECONDS * 60.0) as usize + 120) {
            p.advance(1.0 / 60.0);
            let after = positions(&p);
            assert!(
                !jumped_up(&before, &after),
                "the held group must not jump, whatever the cycle does"
            );
            assert_the_train_order(&p);
            before = after;
        }
        assert!(p.is_holding(), "the hold outlived one nominal cycle");
    }

    #[test]
    fn the_held_ducks_keep_walking_in_place() {
        // The gait lives in the scene and is driven by the scene's clock,
        // so the parade's own contribution is: the position is frozen and
        // the clock keeps running — the duck walks in place.
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(1.0);
        p.hold(5);
        let x = p.unwrapped_x(5);
        let t = p.time();
        p.advance(0.25);
        assert!(
            (p.unwrapped_x(5) - x).abs() < HOLD_EPS,
            "the held duck must not advance"
        );
        assert!(
            p.time() > t,
            "the clock keeps running: the gait keeps its rhythm"
        );
        assert!(p.is_holding());
        assert_eq!(p.held_slot(), Some(5));
    }

    #[test]
    fn the_lag_recovers_to_zero_after_the_release() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.hold(2);
        p.advance(1.0);
        assert!(p.lag(2) > 0.0, "the hold must leave a lag behind");
        p.release();
        assert!(!p.is_holding());
        p.advance(2.0);
        for slot in 0..DUCK_COUNT {
            assert!(
                p.lag(slot).abs() < HOLD_EPS,
                "slot {slot}: the lag must be gone, got {}",
                p.lag(slot)
            );
        }
    }

    #[test]
    fn a_hold_that_spans_the_re_entry_wrap_keeps_the_order() {
        // The re-entry wrap is a discontinuity in the head's x, and a
        // hold that outlives one loop has no state for it (audit 101126
        // §C7 — latent, documented). Whatever the wrap does to the
        // absolute positions, the order and the gaps must survive it.
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.hold(5);
        let steps = (TEST_LOOP_SECONDS * 60.0) as usize + 120;
        for _ in 0..steps {
            p.advance(1.0 / 60.0);
            assert_the_train_order(&p);
        }
        assert!(p.is_holding(), "the hold outlived one full loop");
    }

    #[test]
    fn the_recovery_is_faster_than_the_march() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.hold(2);
        p.advance(1.0);
        let lag = p.lag(2);
        p.release();
        p.advance(0.1);
        let shrunk = lag - p.lag(2);
        assert!(
            shrunk > p.speed() * 0.1,
            "the catch-up must be faster than the march: {shrunk}"
        );
    }

    #[test]
    fn a_stopped_parade_freezes_the_head_but_not_the_clock() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(1.0);
        let head = p.unwrapped_x(0);
        let t = p.time();
        p.set_stopped(true);
        assert!(p.is_stopped());
        p.advance(0.5);
        assert!(
            (p.unwrapped_x(0) - head).abs() < HOLD_EPS,
            "the head must be frozen"
        );
        assert!(
            p.time() > t,
            "the raw clock keeps running: the rhythm survives"
        );
        p.set_stopped(false);
        assert!(!p.is_stopped());
        p.advance(0.5);
        assert!(p.unwrapped_x(0) < head, "the train must resume");
    }

    #[test]
    fn a_stopped_parade_does_not_grow_the_lag() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.hold(1);
        p.set_stopped(true);
        p.advance(0.5);
        assert_eq!(p.lag(1), 0.0, "a frozen train does not fall behind");
    }

    #[test]
    fn resize_preserves_the_lags_and_the_phase() {
        let mut p = Parade::new(TEST_COLS, TEST_LOOP_SECONDS);
        p.advance(2.0);
        p.hold(3);
        p.advance(0.5);
        let lag = p.lag(3);
        let phase = p.phase();
        p.resize(640);
        assert!(
            (p.lag(3) - lag).abs() < HOLD_EPS,
            "the lag survives the resize"
        );
        assert!(
            (p.phase() - phase).abs() < 1e-4,
            "the phase survives the resize too"
        );
        assert_eq!(p.held_slot(), Some(3), "the hold survives the resize");
    }
}
