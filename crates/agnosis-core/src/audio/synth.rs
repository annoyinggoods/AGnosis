//! The pure-Rust synth: a note becomes a buffer of samples.
//!
//! WHY pure: the synthesis is the only part of the audio bundle that can
//! be tested at all — a CI runner has no sound card (roadmap Extension 3
//! §2.3), so the tests assert on the *samples*, never on the device. No
//! file is read, no device is opened, and the only allocation is the
//! buffer that is returned.
//!
//! WHY a hand-rolled synth and not a soundfont: shipping a General MIDI
//! soundfont means shipping megabytes of assets and a whole asset
//! pipeline for a novelty app (roadmap §2.1). A sine/triangle/square
//! oscillator with an ADSR envelope is a few dozen lines and sounds
//! enough for a duck parade.

use std::f32::consts::PI;

/// The longest note the synth renders (seconds).
///
/// The duration comes from the sequencer, but the synth is the last line
/// of defence: a note that asked for an hour of audio must not ask the
/// allocator for a gigabyte.
const MAX_NOTE_SECONDS: f32 = 30.0;

/// The waveform a note is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waveform {
    /// A pure sine — the softest of the three.
    Sine,
    /// A triangle — the default: a little body, no harsh edges.
    Triangle,
    /// A square — the harshest of the three.
    Square,
}

impl Waveform {
    /// The waveform's name, as the configuration spells it.
    pub fn as_str(&self) -> &'static str {
        match self {
            Waveform::Sine => "sine",
            Waveform::Triangle => "triangle",
            Waveform::Square => "square",
        }
    }
}

/// The waveform a configuration name means, or `None` when the name is
/// not one of the three.
///
/// The name is the configuration's own spelling (`sine`, `triangle`,
/// `square`), so a typo in `config.toml` is refused rather than guessed.
pub fn waveform_from_name(name: &str) -> Option<Waveform> {
    match name.trim().to_ascii_lowercase().as_str() {
        "sine" => Some(Waveform::Sine),
        "triangle" => Some(Waveform::Triangle),
        "square" => Some(Waveform::Square),
        _ => None,
    }
}

/// An ADSR envelope, in seconds (`sustain` is a level 0.0..=1.0).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Envelope {
    /// The rise from silence to full level, in seconds.
    pub attack: f32,
    /// The fall from full level to the sustain level, in seconds.
    pub decay: f32,
    /// The level the note holds at, 0.0..=1.0.
    pub sustain: f32,
    /// The fall from the sustain level to silence, in seconds.
    pub release: f32,
}

impl Default for Envelope {
    /// A short pluck: 5 ms in, 60 ms down to 60 % of the level, 120 ms out.
    fn default() -> Self {
        Self {
            attack: 0.005,
            decay: 0.06,
            sustain: 0.6,
            release: 0.12,
        }
    }
}

impl Envelope {
    /// The total length of the envelope, in seconds.
    ///
    /// The sustain is a **level**, not a time: it is held for as long as
    /// the note rings. So the envelope's own length is the attack plus the
    /// decay plus the release — the shortest note this envelope fits into.
    pub fn duration(&self) -> f32 {
        self.attack + self.decay + self.release
    }
}

/// The frequency of a MIDI pitch in Hz (A4 = 440 Hz at pitch 69).
///
/// A pitch above the MIDI range is clamped, never wrapped: a wrapped
/// pitch would sound like a different note than the one on the strip.
pub fn midi_to_hz(pitch: u8) -> f32 {
    let clamped = pitch.min(127) as f32;
    440.0 * 2.0f32.powf((clamped - 69.0) / 12.0)
}

/// Render one note into mono samples.
///
/// **Pure**: no device, no I/O, no allocation beyond the returned buffer.
///
/// * a pitch outside 0..=127 is clamped,
/// * a non-finite or non-positive duration is refused (empty buffer),
/// * a zero sample rate is refused (empty buffer).
pub fn render_note(
    pitch: u8,
    velocity: u8,
    duration_seconds: f32,
    waveform: Waveform,
    envelope: &Envelope,
    sample_rate: u32,
) -> Vec<f32> {
    let count = note_sample_count(duration_seconds, sample_rate);
    if count == 0 {
        return Vec::new();
    }
    let duration = duration_seconds.min(MAX_NOTE_SECONDS);
    let hz = midi_to_hz(pitch);
    let amplitude = (velocity as f32 / 127.0).clamp(0.0, 1.0);
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let t = index as f32 / sample_rate as f32;
        let level = envelope_level(envelope, t, duration);
        out.push(wave_value(waveform, hz * t) * level * amplitude);
    }
    out
}

/// How many samples a note of that length is, or zero when it is refused.
fn note_sample_count(duration_seconds: f32, sample_rate: u32) -> usize {
    if sample_rate == 0 || !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return 0;
    }
    let seconds = duration_seconds.min(MAX_NOTE_SECONDS);
    (seconds * sample_rate as f32).round().max(0.0) as usize
}

/// The envelope's level at `t` seconds into a note of `duration` seconds.
///
/// The envelope is laid out *inside* the note: attack, decay, then the
/// sustain plateau, then the release down to silence. A note shorter
/// than the envelope squashes it proportionally, so the note always ends
/// in silence instead of being cut off mid-plateau.
fn envelope_level(envelope: &Envelope, t: f32, duration: f32) -> f32 {
    let attack = finite(envelope.attack).max(0.0);
    let decay = finite(envelope.decay).max(0.0);
    let release = finite(envelope.release).max(0.0);
    let sustain = finite(envelope.sustain).clamp(0.0, 1.0);
    // WHY the total is computed here and not by `envelope.duration()`:
    // the duration sums the **raw** fields, so a non-finite one made the
    // total a NaN — and a NaN total silenced the note, which defeated
    // the whole point of the `finite()` helper (audit 2026-10-21 §F17).
    let total = attack + decay + release;
    if total <= 0.0 {
        return sustain;
    }
    let scale = if duration >= total {
        1.0
    } else {
        duration / total
    };
    let (attack_s, decay_s, release_s) = (attack * scale, decay * scale, release * scale);
    let sustain_start = attack_s + decay_s;
    let release_start = (duration - release_s).max(sustain_start);
    if t < attack_s {
        return scaled(t / attack_s);
    }
    if t < sustain_start {
        return scaled(1.0 - (1.0 - sustain) * (t - attack_s) / decay_s);
    }
    if t < release_start {
        return sustain;
    }
    sustain * scaled(1.0 - (t - release_start) / (duration - release_start))
}

/// A division that cannot divide by zero: a zero-length phase is a step.
fn scaled(ratio: f32) -> f32 {
    if ratio.is_finite() {
        ratio.clamp(0.0, 1.0)
    } else {
        0.0
    }
}

/// The oscillator's value at `phase` turns.
fn wave_value(waveform: Waveform, phase: f32) -> f32 {
    let cycle = phase - phase.floor();
    match waveform {
        Waveform::Sine => (2.0 * PI * cycle).sin(),
        Waveform::Triangle => triangle_value(cycle),
        Waveform::Square => square_value(cycle),
    }
}

/// The triangle's level over one cycle: it rises to `1.0` at the half
/// cycle and falls back to `-1.0`.
fn triangle_value(cycle: f32) -> f32 {
    if cycle < 0.5 {
        4.0 * cycle - 1.0
    } else {
        3.0 - 4.0 * cycle
    }
}

/// The square's level over one cycle: `1.0` for the first half, `-1.0`
/// for the second.
fn square_value(cycle: f32) -> f32 {
    if cycle < 0.5 {
        1.0
    } else {
        -1.0
    }
}

/// A non-finite number is zero: a NaN in an envelope must not poison the
/// whole buffer.
fn finite(value: f32) -> f32 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The mean absolute value of a window — the envelope's level under a
    /// symmetric waveform, without the waveform's own zero crossings.
    fn mean_abs(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum: f32 = samples.iter().map(|s| s.abs()).sum();
        sum / samples.len() as f32
    }

    /// The loudest sample of a window.
    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |acc, s| acc.max(s.abs()))
    }

    /// A note is exactly `duration * rate` samples long.
    ///
    /// WHY: the device's rate is what the caller hands the synth, so a
    /// note that came out at another length would play at the wrong
    /// speed (audit 2026-10-21 §E12).
    #[test]
    fn a_note_has_duration_times_rate_samples() {
        for rate in [8_000u32, 44_100, 48_000] {
            for duration in [0.25f32, 0.5, 1.0] {
                let samples = render_note(
                    69,
                    127,
                    duration,
                    Waveform::Sine,
                    &Envelope::default(),
                    rate,
                );
                assert_eq!(
                    samples.len(),
                    (duration * rate as f32).round() as usize,
                    "{duration} s at {rate} Hz"
                );
            }
        }
    }

    /// A non-finite envelope must not poison the buffer.
    ///
    /// WHY: the envelope's total was computed from the **raw** fields,
    /// so a NaN attack made the total a NaN and the note came out
    /// **silent** — the very thing the `finite()` helper exists to
    /// prevent (audit 2026-10-21 §F17).
    #[test]
    fn a_non_finite_envelope_does_not_poison_the_note() {
        let envelope = Envelope {
            attack: f32::NAN,
            decay: 0.05,
            sustain: 0.5,
            release: 0.1,
        };
        let samples = render_note(69, 127, 0.5, Waveform::Sine, &envelope, 44_100);
        assert!(!samples.is_empty(), "the note is rendered");
        assert!(
            samples.iter().all(|s| s.is_finite()),
            "every sample is finite"
        );
        assert!(
            peak(&samples) > 0.01,
            "the note is not silenced by the NaN envelope"
        );
    }

    #[test]
    fn the_synth_renders_an_envelope() {
        let envelope = Envelope {
            attack: 0.02,
            decay: 0.05,
            sustain: 0.5,
            release: 0.1,
        };
        let rate = 44_100usize;
        let duration = 0.5;
        let samples = render_note(69, 127, duration, Waveform::Sine, &envelope, rate as u32);
        assert_eq!(samples.len(), (duration * rate as f32).round() as usize);

        // The attack starts from silence.
        assert!(
            samples[0].abs() < 0.01,
            "the attack starts at {}",
            samples[0]
        );

        // The attack is quieter than the plateau: the envelope rises.
        let attack = mean_abs(&samples[..rate / 100]);
        let plateau = &samples[rate * 3 / 10..rate * 3 / 10 + rate / 100];
        let sustain = mean_abs(plateau);
        assert!(
            attack < sustain * 0.75,
            "the attack ({attack}) is not below the plateau ({sustain})"
        );

        // The plateau sits at the sustain level.
        assert!(
            (peak(plateau) - envelope.sustain).abs() < 0.05,
            "the plateau peaks at {}, not at {}",
            peak(plateau),
            envelope.sustain
        );

        // The release falls back to silence.
        let tail = &samples[samples.len() - rate / 100..];
        assert!(
            mean_abs(tail) < sustain * 0.75,
            "the release ({}) is not below the plateau ({sustain})",
            mean_abs(tail)
        );
        assert!(
            samples[samples.len() - 1].abs() < 0.01,
            "the note ends at {}",
            samples[samples.len() - 1]
        );
    }

    #[test]
    fn a_broken_note_is_refused() {
        let envelope = Envelope::default();
        assert!(render_note(60, 100, f32::NAN, Waveform::Sine, &envelope, 44_100).is_empty());
        assert!(render_note(60, 100, -1.0, Waveform::Sine, &envelope, 44_100).is_empty());
        assert!(render_note(60, 100, 0.5, Waveform::Sine, &envelope, 0).is_empty());
    }

    #[test]
    fn midi_pitches_map_to_frequencies() {
        assert!((midi_to_hz(69) - 440.0).abs() < 0.001);
        assert!((midi_to_hz(81) - 880.0).abs() < 0.001);
        assert!((midi_to_hz(57) - 220.0).abs() < 0.001);
        // A pitch above the MIDI range is clamped, not wrapped.
        assert!((midi_to_hz(255) - midi_to_hz(127)).abs() < 0.001);
    }

    #[test]
    fn the_envelope_knows_its_own_length() {
        let envelope = Envelope {
            attack: 0.01,
            decay: 0.02,
            sustain: 0.5,
            release: 0.03,
        };
        assert!((envelope.duration() - 0.06).abs() < 1e-6);
        assert!((Envelope::default().duration() - 0.185).abs() < 1e-6);
    }

    #[test]
    fn every_waveform_stays_in_range() {
        let envelope = Envelope::default();
        for waveform in [Waveform::Sine, Waveform::Triangle, Waveform::Square] {
            let samples = render_note(60, 127, 0.2, waveform, &envelope, 8_000);
            assert!(!samples.is_empty());
            assert!(samples.iter().all(|s| s.abs() <= 1.0001));
        }
    }
}
