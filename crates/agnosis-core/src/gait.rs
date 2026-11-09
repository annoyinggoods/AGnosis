//! The gait: the sprite animation's frame rate, and the step it implies.
//!
//! WHY this module exists: the gait's frame rate is read by **three**
//! owners — the scene (it picks the duck's frame), the note sequencer
//! (one strip slot is one sprite step) and the editor (it shows the
//! ruler). A copy of the number in two of them is a copy that drifts, so
//! the number lives here and everybody reads it from here.
//!
//! The number is the simulation's own: a sprite step is a fixed step of
//! the 1/120 s accumulator, rounded to whole sprite frames.

/// The walk frames per second of the duck gait.
///
/// One sprite step lasts `1 / GAIT_FPS` seconds, so a note on the strip
/// is a sixth of a second long.
pub const GAIT_FPS: f32 = 6.0;

/// The length of one sprite step, in seconds.
///
/// WHY the name: the scene has a step of its own
/// ([`crate::scene::SIM_STEP_SECONDS`], 1/120 s) and the two differ by
/// a factor of twenty — a generic `STEP_SECONDS` in two modules is a
/// 20x error waiting for the next `use` that imports both (audit
/// 2026-10-21 §E6).
pub const GAIT_STEP_SECONDS: f32 = 1.0 / GAIT_FPS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_step_is_a_sixth_of_a_second() {
        assert!((GAIT_STEP_SECONDS - 1.0 / 6.0).abs() < 1e-6);
        assert!((GAIT_FPS * GAIT_STEP_SECONDS - 1.0).abs() < 1e-6);
    }
}
