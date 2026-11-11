//! Trailing-balloon pendulum: a damped, periodically driven swing integrated
//! at a fixed 1/120 s substep.
//!
//! The drive term keeps the motion alive indefinitely; a bare damped pendulum
//! would decay to rest and the ropes would hang dead still.

/// Fixed integration substep in seconds.
pub const SUBSTEP_SECONDS: f32 = 1.0 / 120.0;

/// Hang guard: the accumulator never holds more than this many seconds, so a
/// pathological `dt` (a hours-long stall, `inf`) cannot trap the substep loop.
/// Ordinary spans — including a single 60 s call, which must match 3600 fine
/// calls — are simulated in full. Stability needs no time-dropping clamp: the
/// fixed 1/120 s substep keeps semi-implicit Euler far inside its stable
/// region for these constants.
const MAX_CARRY_SECONDS: f32 = 8192.0;

/// Driven pendulum state `(theta, omega)` in radians.
pub struct Pendulum {
    theta: f32,
    omega: f32,
    time: f64,
    carry: f32,
}

impl Pendulum {
    /// New pendulum displaced by `theta` radians, at rest. Non-finite input
    /// starts at rest at the pivot.
    pub fn new(theta: f32) -> Self {
        let theta = if theta.is_finite() { theta } else { 0.0 };
        Self {
            theta,
            omega: 0.0,
            time: 0.0,
            carry: 0.0,
        }
    }

    /// Current angular displacement in radians.
    ///
    /// Test-only: production reads the swing through
    /// [`Pendulum::attach_point`], so the accessor is kept behind
    /// `cfg(any(test, feature = "test-support"))` for the trajectory
    /// assertions (audit dead-code sweep). The **feature** is what makes
    /// it reachable from the shell's own tests: a `cfg(test)` item is
    /// invisible to *another* crate (audit 2026-10-21 §F22).
    #[cfg(any(test, feature = "test-support"))]
    pub fn theta(&self) -> f32 {
        self.theta
    }

    /// Current angular velocity in radians per second.
    ///
    /// Test-only, like [`Pendulum::theta`]: the drive's wrap-continuity
    /// test (audit 101126 §C6) asserts on the per-step velocity jump.
    #[cfg(any(test, feature = "test-support"))]
    pub fn omega(&self) -> f32 {
        self.omega
    }

    /// Advance with fixed 1/120 s substeps and an accumulator; leftover time
    /// carries over, so any chunking of the same span integrates (nearly) the
    /// same trajectory. `rope_length` (cells) sets the pendulum's physics
    /// length for this slot — randomized per balloon slot since Fix 4.
    /// Non-finite or non-positive `dt` is ignored; absurdly large `dt` is
    /// capped by the accumulator hang guard instead of being simulated step
    /// by step forever.
    pub fn advance(
        &mut self,
        dt_seconds: f32,
        cfg: &crate::config::BalloonsConfig,
        rope_length: f32,
    ) {
        if !dt_seconds.is_finite() || dt_seconds <= 0.0 {
            return;
        }
        self.carry = (self.carry + dt_seconds).min(MAX_CARRY_SECONDS);
        while self.carry >= SUBSTEP_SECONDS {
            self.carry -= SUBSTEP_SECONDS;
            self.step(SUBSTEP_SECONDS, cfg, rope_length);
        }
    }

    /// Balloon attach point in cell space: `pivot + L*(sin(alpha), -cos(alpha))`.
    /// Trailing is `+x` for positive `theta`.
    pub fn attach_point(&self, pivot: (f32, f32), rope_length: f32) -> (f32, f32) {
        let l = if rope_length.is_finite() && rope_length >= 0.0 {
            rope_length
        } else {
            0.0
        };
        (
            pivot.0 + l * self.theta.sin(),
            pivot.1 - l * self.theta.cos(),
        )
    }

    fn step(&mut self, h: f32, cfg: &crate::config::BalloonsConfig, rope_length: f32) {
        let l = if rope_length.is_finite() && rope_length > 0.0 {
            rope_length as f64
        } else {
            7.0
        };
        let g = if cfg.pendulum_g.is_finite() && cfg.pendulum_g > 0.0 {
            cfg.pendulum_g as f64
        } else {
            9.8
        };
        let c = if cfg.pendulum_damp.is_finite() && cfg.pendulum_damp >= 0.0 {
            cfg.pendulum_damp as f64
        } else {
            0.35
        };
        let a = if cfg.pendulum_drive.is_finite() {
            cfg.pendulum_drive as f64
        } else {
            0.9
        };
        let period = if cfg.pendulum_period.is_finite() && cfg.pendulum_period > 0.0 {
            cfg.pendulum_period as f64
        } else {
            1.6
        };
        self.time += h as f64;
        // The drive **phase** is wrapped, not the absolute clock: a wrap
        // at a fixed second count is phase-continuous only for periods
        // that divide it, and any other configured `pendulum_period` took
        // a one-frame drive jump at the wrap (audit 101126 §B2). `sin`
        // still never sees an absurd argument — the phase is folded to
        // one turn, however long the parade runs.
        let phase = (std::f64::consts::TAU * self.time / period).rem_euclid(std::f64::consts::TAU);
        let drive = a * phase.sin();
        let accel = -(g / l) * self.theta as f64 - c * self.omega as f64 + drive;
        self.omega = (self.omega as f64 + accel * h as f64) as f32;
        self.theta = (self.theta as f64 + self.omega as f64 * h as f64) as f32;
        if !self.theta.is_finite() {
            self.theta = 0.0;
        }
        if !self.omega.is_finite() {
            self.omega = 0.0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> crate::config::BalloonsConfig {
        crate::config::BalloonsConfig::default()
    }

    #[test]
    fn chunked_advance_matches_one_big_call() {
        let mut a = Pendulum::new(0.5);
        let mut b = Pendulum::new(0.5);
        a.advance(60.0, &cfg(), 110.0);
        for _ in 0..3600 {
            b.advance(1.0 / 60.0, &cfg(), 110.0);
        }
        assert!(
            (a.theta() - b.theta()).abs() < 0.05,
            "coarse {} vs fine {}",
            a.theta(),
            b.theta()
        );
    }

    #[test]
    fn angle_stays_bounded_over_sixty_seconds() {
        let mut p = Pendulum::new(0.5);
        p.advance(60.0, &cfg(), 40.0);
        assert!(p.theta().is_finite(), "theta not finite");
        assert!(p.theta().abs() < 1.5, "theta {}", p.theta());
    }

    #[test]
    fn drive_keeps_amplitude_alive() {
        let mut p = Pendulum::new(0.5);
        let mut peak = 0.0f32;
        for _ in 0..3600 {
            p.advance(1.0 / 60.0, &cfg(), 65.0);
            peak = peak.max(p.theta().abs());
        }
        assert!(peak > 0.05, "wobble died, peak {peak}");
        let mut q = Pendulum::new(0.5);
        q.advance(59.0, &cfg(), 65.0);
        let mut late_peak = 0.0f32;
        for _ in 0..60 {
            q.advance(1.0 / 60.0, &cfg(), 65.0);
            late_peak = late_peak.max(q.theta().abs());
        }
        assert!(late_peak > 0.01, "settled, late peak {late_peak}");
    }

    #[test]
    fn attach_point_is_rope_length_from_pivot() {
        let cfg = cfg();
        let mut p = Pendulum::new(0.4);
        p.advance(3.0, &cfg, 110.0);
        let pivot = (10.0, 20.0);
        let (x, y) = p.attach_point(pivot, 110.0);
        let d = ((x - pivot.0).powi(2) + (y - pivot.1).powi(2)).sqrt();
        assert!((d - 110.0).abs() < 1e-3, "dist {d}");
        let hanging = Pendulum::new(0.0).attach_point(pivot, 7.0);
        assert!((hanging.0 - 10.0).abs() < 1e-6);
        assert!((hanging.1 - 13.0).abs() < 1e-6);
    }

    #[test]
    fn absurd_dt_cannot_hang_or_explode() {
        let mut p = Pendulum::new(0.3);
        p.advance(100.0, &cfg(), 40.0);
        assert!(p.theta().is_finite());
        assert!(p.theta().abs() < 1.5);
        let mut q = Pendulum::new(0.3);
        q.advance(f32::INFINITY, &cfg(), 40.0);
        assert!(q.theta().is_finite());
        let mut r = Pendulum::new(0.3);
        r.advance(f32::NAN, &cfg(), 40.0);
        assert_eq!(r.theta(), 0.3);
    }

    #[test]
    fn the_drive_is_continuous_across_the_old_clock_wrap() {
        // The drive used to wrap the absolute clock at a fixed second
        // count; any `pendulum_period` that does not divide it took a
        // one-frame drive jump there (audit 101126 §C6). March past that
        // count with a deliberately non-dividing period: the per-step
        // velocity change across it must stay inside the ordinary
        // pace, twice over.
        let mut cfg = cfg();
        cfg.pendulum_period = 1.55;
        let mut p = Pendulum::new(0.3);
        // 166 whole minutes put the clock at 9960 s — under the old
        // wrap point of 10 000 s and under the carry cap per call.
        for _ in 0..166 {
            p.advance(60.0, &cfg, 65.0);
        }
        // The two seconds before the old wrap set the ordinary pace.
        let mut ordinary = 0.0f32;
        for _ in 0..240 {
            let before = p.omega();
            p.advance(SUBSTEP_SECONDS, &cfg, 65.0);
            ordinary = ordinary.max((p.omega() - before).abs());
        }
        // Ten more seconds cross the old wrap (1.67 s in) and keep
        // going: every step stays ordinary.
        for _ in 0..1200 {
            let before = p.omega();
            p.advance(SUBSTEP_SECONDS, &cfg, 65.0);
            let jump = (p.omega() - before).abs();
            assert!(
                jump <= ordinary * 2.0 + 1e-9,
                "the drive jumped: {jump} vs the ordinary {ordinary}"
            );
        }
    }
}
