//! The duck reaction state machine (roadmap Extension 3 §5.3).
//!
//! WHY a separate module: the user's spec for the mouse interaction is
//! "jump, change facial expression, make sound — **but still moving forward
//! or stopping**" (roadmap §0.2). A reaction is a **timeline**, not a
//! drawing: every number it produces (the lift, the landing squash, the
//! colour flash) is a pure function of one clock, so the whole thing is
//! testable without a display, a GPU or a pointer, and the scene only has
//! to read four numbers per reacting duck.
//!
//! The module is deliberately **dependency-free**: it imports nothing from
//! the scene, the parade or the weather. The *hold* (the ducks behind a
//! stopping duck stop too — roadmap §5.4, the same primitive the weather
//! uses) belongs to the parade's hold model; this module only says **which**
//! reaction is running and **for how long**, so the scene can wire the two
//! together without either of them owning the other.
//!
//! ## The timeline
//!
//! One reaction lasts [`Reaction::DURATION_SECONDS`] (0.4 s) and every curve
//! is a function of the normalised time `t = age / DURATION`:
//!
//! ```text
//!   lift(t)   = -JUMP_HEIGHT_CELLS * 4 * t * (1 - t)   // the parabola
//!   squash(t) = 0 until the landing window, then the landing curve
//!   flash(t)  = 1 - t
//! ```
//!
//! The lift is a **parabola over the whole reaction**: 0 at the start, the
//! most negative in the middle, 0 again at the end — the duck leaves the
//! ground, peaks and comes back down inside one reaction, and it is back
//! on the ground exactly when the reaction ends. The train does not move:
//! the lift is added to the duck's **draw row** (roadmap §5.3), so only the
//! duck's body rises and falls and the order never changes.
//!
//! ## The face
//!
//! The built-in art is **generated** and drift-guarded, so a reaction
//! cannot add a new frame (roadmap §5.3's decision): the expression is a
//! **held frame** plus a squash plus a flash. The held frame is the gait
//! phase the duck had when the reaction started — [`Reaction::frozen_phase`]
//! — so the duck stops cycling through its walk frames and the held one
//! reads as a grimace at three pixels per cell.
//!
//! ## The lifecycle
//!
//! The state machine is per slot: `Idle -> Reacting { kind, until } -> Idle`.
//! This module owns the `Reacting` half; the `Idle` half is the scene's
//! (a `None` reaction is a duck that is not reacting). The clock is the
//! scene's **fixed 1/120 s step**, so the curve is identical whatever the
//! frame rate (roadmap §5.6: the reaction is integrated into the fixed
//! step).

/// The landing window's start, as a fraction of the reaction: the squash
/// is zero for the whole flight before it.
const LANDING_START: f32 = 0.8;

/// Where the squash peaks inside the landing window, as a fraction of that
/// window: 0.75 puts the peak at 95 % of the reaction, where the duck is
/// about one cell above the ground — a hundredth of its own height, so it
/// reads as grounded.
const SQUASH_PEAK: f32 = 0.75;

/// The age tolerance at the end of the reaction.
///
/// WHY: the fixed 1/120 s step accumulates a hair under the duration (48
/// steps are 0.39999999999999997 s), so a reaction driven by the fixed
/// step would still be "running" on its last step. The tolerance is a
/// hundredth of a millisecond — far below the step — so the reaction still
/// ends on the 48th step, and its curves are exactly back to zero.
const AGE_EPSILON: f32 = 1e-4;

/// What a reacting duck does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionKind {
    /// A vertical hop. The duck stays in the train and keeps marching.
    Jump,
    /// The duck stops in place, and the ducks behind it stop too.
    Stop,
}

/// One duck's reaction: `Idle -> Reacting { kind, until } -> Idle`.
///
/// The `until` of the roadmap's state machine is the age here: the reaction
/// is a relative timeline, so the caller advances it by the same `dt` it
/// advances the scene by and the two never drift.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Reaction {
    /// What the duck is doing.
    kind: ReactionKind,
    /// Seconds since the reaction started, clamped to
    /// [`Reaction::DURATION_SECONDS`].
    age: f32,
    /// The gait phase the duck holds while it reacts.
    frozen_phase: f32,
}

impl Reaction {
    /// How long a reaction lasts, in seconds.
    pub const DURATION_SECONDS: f32 = 0.4;
    /// How high the duck hops, in cells.
    pub const JUMP_HEIGHT_CELLS: f32 = 6.0;

    /// Start a reaction. `frozen_phase` is the duck's gait phase at the
    /// moment the reaction starts: the duck **holds that frame** while it
    /// reacts, which is what reads as a changed face.
    ///
    /// A non-finite phase is stored as `0.0`: the phase is a number the
    /// scene computes, and a broken one must not poison the draw path.
    pub fn start(kind: ReactionKind, frozen_phase: f32) -> Self {
        Self {
            kind,
            age: 0.0,
            frozen_phase: if frozen_phase.is_finite() {
                frozen_phase
            } else {
                0.0
            },
        }
    }

    /// The kind of reaction.
    pub fn kind(&self) -> ReactionKind {
        self.kind
    }

    /// How long the reaction has been running, in seconds.
    ///
    /// Clamped to [`Reaction::DURATION_SECONDS`]: a reaction that has run
    /// its course keeps its age, so the caller can tell "just finished"
    /// from "running".
    pub fn age(&self) -> f32 {
        self.age
    }

    /// Advance the reaction's clock.
    ///
    /// A non-finite or negative step is **ignored**: the age never goes
    /// backwards, so a bad step can never rewind a duck that is already
    /// reacting. A step past the end clamps the age at the duration.
    pub fn advance(&mut self, dt_seconds: f32) {
        if !dt_seconds.is_finite() || dt_seconds < 0.0 {
            return;
        }
        self.age = (self.age + dt_seconds).min(Self::DURATION_SECONDS);
    }

    /// True while the reaction is still running.
    ///
    /// The reaction is over on the step that reaches the duration: the
    /// fixed 1/120 s step accumulates a hair under it, so the end is
    /// compared with a tolerance far below the step (see [`AGE_EPSILON`]).
    pub fn is_active(&self) -> bool {
        self.age() < Self::DURATION_SECONDS - AGE_EPSILON
    }

    /// The vertical offset the duck is drawn at, in cells. **Negative lifts
    /// the duck** (the scene adds it to the duck's draw row). Zero when the
    /// duck is not jumping.
    ///
    /// The curve is the parabola `-JUMP_HEIGHT_CELLS * 4 * t * (1 - t)`
    /// over the normalised time `t`: 0 at the start, `-JUMP_HEIGHT_CELLS` at
    /// the halfway point, 0 again at the end, and never positive (the duck
    /// is never pushed **into** the ground).
    pub fn offset_cells(&self) -> f32 {
        if !self.is_active() || self.kind != ReactionKind::Jump {
            return 0.0;
        }
        let t = self.progress();
        -Self::JUMP_HEIGHT_CELLS * 4.0 * t * (1.0 - t)
    }

    /// The landing squash, 0.0..=1.0: how much the duck is compressed
    /// vertically as it lands. Zero while it is in the air.
    ///
    /// The curve is zero for the whole flight and lives in the **landing
    /// window** (the last fifth of the reaction): it rises to 1.0 as the
    /// duck touches down and then relaxes back to 0.0 by the end of the
    /// reaction. The squash therefore never outlives the reaction — a duck
    /// that stays squashed after it lands looks broken, so the release is
    /// part of the reaction, not an afterthought.
    ///
    /// The same curve serves both kinds: for a jump it is the landing, for
    /// a stop it is the halt.
    pub fn squash(&self) -> f32 {
        if !self.is_active() {
            return 0.0;
        }
        let t = self.progress();
        if t <= LANDING_START {
            return 0.0;
        }
        let u = (t - LANDING_START) / (1.0 - LANDING_START);
        if u <= SQUASH_PEAK {
            return smoothstep(u / SQUASH_PEAK);
        }
        1.0 - smoothstep((u - SQUASH_PEAK) / (1.0 - SQUASH_PEAK))
    }

    /// The colour flash, 0.0..=1.0: how strongly the duck's colour flares
    /// towards white. It fades out over the reaction.
    ///
    /// The curve is linear, `1 - t`: the flash is at its strongest the
    /// instant the reaction starts and is gone when the reaction ends.
    pub fn flash(&self) -> f32 {
        if !self.is_active() {
            return 0.0;
        }
        1.0 - self.progress()
    }

    /// The gait phase the duck holds while it reacts.
    pub fn frozen_phase(&self) -> f32 {
        self.frozen_phase
    }

    /// The normalised time of the reaction, in `0.0..=1.0`.
    fn progress(&self) -> f32 {
        (self.age / Self::DURATION_SECONDS).clamp(0.0, 1.0)
    }
}

/// The smoothstep curve: 0 at 0, 1 at 1 and flat at both ends, so the
/// squash starts and ends without a step.
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    x * x * (3.0 - 2.0 * x)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The scene's fixed simulation step.
    const STEP: f32 = 1.0 / 120.0;

    /// A reaction advanced by `steps` fixed steps.
    fn stepped(kind: ReactionKind, steps: u32) -> Reaction {
        let mut reaction = Reaction::start(kind, 0.0);
        for _ in 0..steps {
            reaction.advance(STEP);
        }
        reaction
    }

    #[test]
    fn a_jump_is_a_parabola() {
        let mut reaction = Reaction::start(ReactionKind::Jump, 0.0);
        assert_eq!(
            reaction.offset_cells(),
            0.0,
            "the duck starts on the ground"
        );

        let mut offsets = Vec::new();
        for _ in 0..48 {
            reaction.advance(STEP);
            offsets.push(reaction.offset_cells());
        }
        assert!(
            offsets.iter().all(|offset| *offset <= 0.0),
            "the duck is never pushed into the ground"
        );

        // The halfway point of the 0.4 s reaction is the peak.
        let peak = offsets[23];
        assert!(
            (peak + Reaction::JUMP_HEIGHT_CELLS).abs() < 1e-3,
            "the peak must be the jump height, got {peak}"
        );
        assert!(
            offsets.iter().all(|offset| *offset >= peak),
            "the halfway point must be the highest point"
        );

        let mut ended = Reaction::start(ReactionKind::Jump, 0.0);
        ended.advance(Reaction::DURATION_SECONDS);
        assert_eq!(
            ended.offset_cells(),
            0.0,
            "the duck lands with the reaction"
        );
    }

    #[test]
    fn the_jump_is_frame_rate_independent() {
        let mut stepped_reaction = Reaction::start(ReactionKind::Jump, 0.0);
        let mut whole_reaction = Reaction::start(ReactionKind::Jump, 0.0);
        for _ in 0..12 {
            stepped_reaction.advance(STEP);
        }
        whole_reaction.advance(12.0 * STEP);
        assert!(
            (stepped_reaction.age() - whole_reaction.age()).abs() < 1e-6,
            "the same elapsed time must give the same age"
        );
        assert!(
            (stepped_reaction.offset_cells() - whole_reaction.offset_cells()).abs() < 1e-4,
            "the same elapsed time must give the same lift"
        );
        assert!(
            (stepped_reaction.squash() - whole_reaction.squash()).abs() < 1e-4,
            "the same elapsed time must give the same squash"
        );
    }

    #[test]
    fn the_reaction_ends() {
        let mut reaction = Reaction::start(ReactionKind::Jump, 0.0);
        reaction.advance(Reaction::DURATION_SECONDS);
        assert!(!reaction.is_active(), "the reaction is over");
        assert_eq!(
            reaction.offset_cells(),
            0.0,
            "the duck is back on the ground"
        );
        assert_eq!(reaction.squash(), 0.0, "the duck is not squashed any more");
        assert_eq!(reaction.age(), Reaction::DURATION_SECONDS);

        // The age stays clamped at the duration.
        reaction.advance(5.0);
        assert_eq!(reaction.age(), Reaction::DURATION_SECONDS);
        assert!(!reaction.is_active());
        assert_eq!(reaction.offset_cells(), 0.0);
        assert_eq!(reaction.squash(), 0.0);
    }

    #[test]
    fn the_landing_squash_happens_at_the_end() {
        let mut reaction = Reaction::start(ReactionKind::Jump, 0.0);
        // The flight: the duck is in the air, so it is not squashed.
        for _ in 0..30 {
            reaction.advance(STEP);
            assert_eq!(reaction.squash(), 0.0, "the duck is in the air");
        }

        // The landing: the squash rises to 1 as the duck touches down.
        let mut peak: f32 = 0.0;
        for _ in 0..30 {
            reaction.advance(STEP);
            peak = peak.max(reaction.squash());
        }
        assert!(peak > 0.95, "the squash must peak at 1, got {peak}");

        let mut landing = Reaction::start(ReactionKind::Jump, 0.0);
        landing.advance(0.38);
        assert!(
            (landing.squash() - 1.0).abs() < 1e-3,
            "the squash peaks at 1, got {}",
            landing.squash()
        );

        // And it is gone once the reaction is over.
        reaction.advance(Reaction::DURATION_SECONDS);
        assert!(!reaction.is_active());
        assert_eq!(reaction.squash(), 0.0, "a landed duck is not squashed");
    }

    #[test]
    fn the_flash_fades_out() {
        let mut reaction = Reaction::start(ReactionKind::Jump, 0.0);
        assert!(
            (reaction.flash() - 1.0).abs() < 1e-6,
            "the flash starts at its strongest"
        );
        reaction.advance(Reaction::DURATION_SECONDS / 2.0);
        assert!(
            (reaction.flash() - 0.5).abs() < 1e-3,
            "the flash fades over the reaction, got {}",
            reaction.flash()
        );
        reaction.advance(Reaction::DURATION_SECONDS);
        assert_eq!(reaction.flash(), 0.0, "the flash is gone with the reaction");
    }

    #[test]
    fn the_frame_is_held() {
        let mut reaction = Reaction::start(ReactionKind::Jump, 7.25);
        for _ in 0..48 {
            reaction.advance(STEP);
            assert_eq!(reaction.frozen_phase(), 7.25, "the frame must not move");
        }
        assert_eq!(reaction.frozen_phase(), 7.25);
    }

    #[test]
    fn a_negative_or_non_finite_step_is_ignored() {
        let mut reaction = Reaction::start(ReactionKind::Jump, 0.0);
        reaction.advance(0.2);
        let age = reaction.age();
        reaction.advance(-1.0);
        assert_eq!(reaction.age(), age, "the age must not go backwards");
        reaction.advance(f32::NAN);
        assert_eq!(reaction.age(), age, "a NaN step must be ignored");
        reaction.advance(f32::INFINITY);
        assert_eq!(reaction.age(), age, "an infinite step must be ignored");
        reaction.advance(0.0);
        assert_eq!(reaction.age(), age, "a zero step changes nothing");
    }

    #[test]
    fn a_stop_reaction_has_no_jump() {
        let mut reaction = Reaction::start(ReactionKind::Stop, 0.0);
        assert_eq!(reaction.kind(), ReactionKind::Stop);
        for _ in 0..48 {
            reaction.advance(STEP);
            assert_eq!(
                reaction.offset_cells(),
                0.0,
                "a stopping duck does not lift"
            );
        }
    }

    #[test]
    fn the_reaction_keeps_its_kind_and_survives_a_copy() {
        let reaction = Reaction::start(ReactionKind::Stop, 3.5);
        let copy = reaction;
        assert_eq!(copy.kind(), ReactionKind::Stop);
        assert_eq!(copy.frozen_phase(), 3.5);
        assert_eq!(copy.age(), 0.0);
        assert!(copy.is_active());
    }

    #[test]
    fn a_non_finite_phase_is_stored_as_zero() {
        let reaction = Reaction::start(ReactionKind::Jump, f32::NAN);
        assert_eq!(reaction.frozen_phase(), 0.0);
        assert!(reaction.frozen_phase().is_finite());
    }

    #[test]
    fn the_stepped_reaction_reaches_the_end_in_48_steps() {
        let reaction = stepped(ReactionKind::Jump, 48);
        assert!(!reaction.is_active());
        assert_eq!(reaction.offset_cells(), 0.0);
        assert_eq!(reaction.squash(), 0.0);
        assert_eq!(reaction.flash(), 0.0);
    }
}
