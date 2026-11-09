//! The procedural one-shots: clucks, squeaks, chirps and weather noises.
//!
//! WHY procedural: the project ships a single static binary and has no
//! audio asset pipeline (roadmap Extension 3 §4.1). A dozen one-shots
//! that are a few hundred milliseconds of generated waveform cost a few
//! hundred lines of pure Rust, ship nothing, and — because they are
//! deterministic — can be asserted on in the tests.
//!
//! Nothing in this module touches the filesystem: an effect is a
//! function of its id and the sample rate, nothing else.

use std::f32::consts::PI;

/// How many effects there are (the cooldown gate's array size).
const EFFECT_COUNT: usize = 5;

/// How long an effect stays on cooldown, in seconds.
///
/// Ten clicks in a second must produce at most three clucks: at this
/// spacing a second holds 1 / 0.35 = 2.85 clucks.
pub const COOLDOWN_SECONDS: f32 = 0.35;

/// The length of each effect, in seconds.
const DURATION_SECONDS: [f32; EFFECT_COUNT] = [0.12, 0.28, 0.09, 0.25, 0.90];

/// The procedural one-shots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SfxId {
    /// A duck is clicked.
    Cluck,
    /// A balloon is added.
    Squeak,
    /// A duck jumps.
    Chirp,
    /// A duck hits the ground in the rain.
    Splosh,
    /// A lightning strikes.
    Thunder,
}

impl SfxId {
    /// The short name used in traces.
    pub fn as_str(&self) -> &'static str {
        match self {
            SfxId::Cluck => "CLUCK",
            SfxId::Squeak => "SQUEAK",
            SfxId::Chirp => "CHIRP",
            SfxId::Splosh => "SPLOSH",
            SfxId::Thunder => "THUNDER",
        }
    }

    /// How many effects there are (the cooldown gate's table size).
    pub fn count() -> usize {
        EFFECT_COUNT
    }

    /// The effect's own mixer gain, so it can be mixed with the track.
    pub fn gain(&self) -> f32 {
        match self {
            SfxId::Cluck => 0.80,
            SfxId::Squeak => 0.45,
            SfxId::Chirp => 0.55,
            SfxId::Splosh => 0.70,
            SfxId::Thunder => 0.90,
        }
    }

    /// The effect's slot in the cooldown gate's table.
    fn index(&self) -> usize {
        match self {
            SfxId::Cluck => 0,
            SfxId::Squeak => 1,
            SfxId::Chirp => 2,
            SfxId::Splosh => 3,
            SfxId::Thunder => 4,
        }
    }

    /// The effect's own length, in seconds.
    fn duration_seconds(&self) -> f32 {
        DURATION_SECONDS[self.index()]
    }
}

/// Render an effect into mono samples. **Pure** — no file is ever loaded.
///
/// The samples already carry the effect's own gain (see [`SfxId::gain`]),
/// so an effect mixed with the track lands at its intended level.
pub fn render(id: SfxId, sample_rate: u32) -> Vec<f32> {
    if sample_rate == 0 {
        return Vec::new();
    }
    let mut samples = render_raw(id, sample_rate);
    let gain = id.gain();
    for sample in &mut samples {
        *sample *= gain;
    }
    samples
}

/// The effect's waveform, before its gain is applied.
fn render_raw(id: SfxId, sample_rate: u32) -> Vec<f32> {
    let count = (id.duration_seconds() * sample_rate as f32)
        .round()
        .max(0.0) as usize;
    match id {
        SfxId::Cluck => render_cluck(count, sample_rate),
        SfxId::Squeak => render_squeak(count, sample_rate),
        SfxId::Chirp => render_chirp(count, sample_rate),
        SfxId::Splosh => render_splosh(count, sample_rate),
        SfxId::Thunder => render_thunder(count, sample_rate),
    }
}

/// A short pitched-down blip with a noise transient.
fn render_cluck(count: usize, rate: u32) -> Vec<f32> {
    let mut rng = Lcg::new(0x5EED_0001);
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let t = index as f32 / rate as f32;
        // The pitch falls from 720 Hz to 260 Hz: a linear sweep, so the
        // phase is the integral of the frequency.
        let phase = sweep_phase(720.0, 260.0, t, DURATION_SECONDS[0]);
        let body = (2.0 * PI * phase).sin();
        let transient = noise_burst(&mut rng, t, 0.02, 0.004);
        let env = (-t / 0.045).exp();
        out.push((body * 0.7 + transient * 0.6) * env);
    }
    out
}

/// A rising sine with a vibrato.
fn render_squeak(count: usize, rate: u32) -> Vec<f32> {
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let t = index as f32 / rate as f32;
        let base = 480.0 + (1500.0 - 480.0) * (t / DURATION_SECONDS[1]);
        let vibrato = 1.0 + 0.06 * (2.0 * PI * 17.0 * t).sin();
        let env = attack_then_decay(t, 0.02, 0.12);
        out.push((2.0 * PI * base * vibrato * t).sin() * env);
    }
    out
}

/// A fast up-sweep.
fn render_chirp(count: usize, rate: u32) -> Vec<f32> {
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let t = index as f32 / rate as f32;
        let phase = sweep_phase(1100.0, 2800.0, t, DURATION_SECONDS[2]);
        let env = attack_then_decay(t, 0.005, 0.03);
        out.push((2.0 * PI * phase).sin() * env);
    }
    out
}

/// Filtered noise with a fast decay.
fn render_splosh(count: usize, rate: u32) -> Vec<f32> {
    let mut rng = Lcg::new(0x5EED_0002);
    let mut out = Vec::with_capacity(count);
    let mut low = 0.0f32;
    for index in 0..count {
        let t = index as f32 / rate as f32;
        // A one-pole lowpass over the white noise: the body of the water.
        low += (rng.next_signed() - low) * 0.25;
        let env = (-t / 0.06).exp();
        out.push(low * 2.2 * env);
    }
    out
}

/// A low rumble: noise through a slow envelope.
///
/// The rumble is soft-clipped with `tanh` (audit 2026-10-23 §prior-55):
/// the raw body is lowpassed noise times six, which swings past ±1.0,
/// and a sample past full scale is **distortion** on every device —
/// the clipper keeps the shape while bounding every sample to ±1.0.
fn render_thunder(count: usize, rate: u32) -> Vec<f32> {
    let mut rng = Lcg::new(0x5EED_0003);
    let mut out = Vec::with_capacity(count);
    let mut low = 0.0f32;
    for index in 0..count {
        let t = index as f32 / rate as f32;
        // A heavy lowpass: the rumble has no top end at all.
        low += (rng.next_signed() - low) * 0.03;
        let env = attack_then_decay(t, 0.06, 0.35);
        out.push((low * 6.0 * env).tanh());
    }
    out
}

/// The phase of a linear frequency sweep, in turns.
///
/// The frequency runs from `start` to `end` over `seconds`, so the phase
/// is the integral of the frequency: a quadratic, not a straight line.
fn sweep_phase(start: f32, end: f32, t: f32, seconds: f32) -> f32 {
    start * t + (end - start) * t * t / (2.0 * seconds)
}

/// A quick rise and a slow fall: the shape of a squeak and of a chirp.
fn attack_then_decay(t: f32, attack: f32, decay: f32) -> f32 {
    let rise = (t / attack).min(1.0);
    let fall = (-(t.max(attack) - attack) / decay).exp();
    rise * fall
}

/// A noise transient: white noise that fades fast, for the first
/// `window` seconds only.
fn noise_burst(rng: &mut Lcg, t: f32, window: f32, decay: f32) -> f32 {
    if t >= window {
        return 0.0;
    }
    rng.next_signed() * (-t / decay).exp()
}

/// A fixed-seed linear congruential generator.
///
/// The noise must be **identical on every run**: a random source would
/// make the samples untestable, and the whole point of the procedural
/// effects is that they are pure functions.
struct Lcg(u32);

impl Lcg {
    fn new(seed: u32) -> Self {
        Lcg(seed)
    }

    fn next_u32(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0
    }

    /// A uniform sample in -1.0..=1.0.
    fn next_signed(&mut self) -> f32 {
        (self.next_u32() as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
}

/// The cooldown gate: at most one of the same effect per
/// [`COOLDOWN_SECONDS`].
#[derive(Debug, Clone)]
pub struct CooldownGate {
    /// When each effect last played, in seconds.
    last: Vec<Option<f32>>,
}

impl Default for CooldownGate {
    /// WHY this is hand-written: the derived `Default` would leave the
    /// table **empty**, and the first `allow` would index past its end
    /// — an out-of-bounds panic, and the release profile aborts on a
    /// panic. `Default` is the constructor a caller reaches for by
    /// habit, so it has to be the *working* one (audit 2026-10-21 §F2).
    fn default() -> Self {
        Self::new()
    }
}

impl CooldownGate {
    /// A gate that has never allowed anything.
    pub fn new() -> Self {
        Self {
            last: vec![None; SfxId::count()],
        }
    }

    /// True when the effect may play at `now_seconds` (and records it).
    pub fn allow(&mut self, id: SfxId, now_seconds: f32) -> bool {
        let index = id.index();
        let cooling = matches!(
            self.last[index],
            Some(at) if now_seconds >= at && now_seconds - at < COOLDOWN_SECONDS
        );
        if cooling {
            return false;
        }
        self.last[index] = Some(now_seconds);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every effect of the enum, in the cooldown table's order.
    const ALL: [SfxId; EFFECT_COUNT] = [
        SfxId::Cluck,
        SfxId::Squeak,
        SfxId::Chirp,
        SfxId::Splosh,
        SfxId::Thunder,
    ];

    #[test]
    fn each_effect_renders_samples() {
        for id in ALL {
            let samples = render(id, 44_100);
            let expected = (id.duration_seconds() * 44_100.0).round() as usize;
            assert!(!samples.is_empty(), "{} rendered nothing", id.as_str());
            assert_eq!(
                samples.len(),
                expected,
                "{} has the wrong length",
                id.as_str()
            );
            assert!(
                samples.iter().all(|s| s.is_finite()),
                "{} produced a non-finite sample",
                id.as_str()
            );
            assert!(peak(&samples) > 0.01, "{} is silent", id.as_str());
        }
    }

    #[test]
    fn the_effects_have_a_cooldown() {
        let mut gate = CooldownGate::new();
        let mut played = 0;
        for click in 0..10 {
            if gate.allow(SfxId::Cluck, click as f32 * 0.1) {
                played += 1;
            }
        }
        assert!(played <= 3, "ten clicks produced {played} clucks");
    }

    #[test]
    fn the_gate_is_per_effect() {
        let mut gate = CooldownGate::new();
        assert!(gate.allow(SfxId::Cluck, 0.0));
        assert!(gate.allow(SfxId::Squeak, 0.0));
        assert!(!gate.allow(SfxId::Cluck, 0.1));
        assert!(gate.allow(SfxId::Cluck, COOLDOWN_SECONDS));
    }

    /// The **default** gate is a working gate: the derived `Default`
    /// left the table empty and the first `allow` panicked (audit
    /// 2026-10-21 §F2).
    #[test]
    fn the_default_gate_works() {
        let mut gate = CooldownGate::default();
        assert!(gate.allow(SfxId::Cluck, 0.0), "the first click plays");
        assert!(
            !gate.allow(SfxId::Cluck, 0.1),
            "a click inside the cooldown is muted"
        );
    }

    #[test]
    fn the_effects_are_deterministic() {
        let first = render(SfxId::Splosh, 8_000);
        let second = render(SfxId::Splosh, 8_000);
        assert_eq!(first, second);
    }

    #[test]
    fn the_thunder_never_leaves_full_scale() {
        // The rumble's raw body swings past ±1.0; the soft clipper
        // bounds every sample (audit 2026-10-23 §prior-55).
        let samples = render(SfxId::Thunder, 44_100);
        assert!(
            peak(&samples) <= 1.0,
            "thunder exceeds full scale: {}",
            peak(&samples)
        );
    }

    #[test]
    fn no_effect_loads_a_file() {
        // The needles are assembled at run time: written out literally they
        // would appear in this very file and the search would find itself.
        let fs = ["std", "::fs"].join("");
        let open = ["File", "::open"].join("");
        let read = ["read_to", "_string"].join("");
        let bytes = ["include", "_bytes"].join("");
        let path = ["std", "::path"].join("");
        let crate_path = ["crate", "::"].join("");
        let source = include_str!("sfx.rs");
        for needle in [fs, open, read, bytes, path, crate_path] {
            assert!(
                !source.contains(&needle),
                "the effects must not touch the filesystem ({needle})"
            );
        }
    }

    /// The loudest sample of a window.
    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
    }
}
