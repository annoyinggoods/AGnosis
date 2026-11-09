//! The pure side of the sound: the timeline, the one-shots and the
//! oscillator. The **device** lives in the binary crate.
//!
//! WHY the split: the sound card is native-only (it needs `rodio`), and
//! the *event* the scene asks for is plain data. The core owns the
//! event, the sequencer's clock, the one-shot synthesis and the
//! oscillator; the binary crate owns the device, the worker thread and
//! the bounded queue that drains the events.
//!
//! WHY the audio side is a **pure consumer** of the simulation: a note is
//! fired by a step index, never by a callback into the scene (roadmap
//! Extension 3 §0.3). Nothing in this module reads the simulation.

pub mod sequencer;
pub mod sfx;
pub mod synth;

pub use sequencer::{Note, Sequencer, Track};
pub use sfx::SfxId;

/// True when the audio side never reads the simulation.
///
/// The check reads **this crate's own** `audio/` sources, so it cannot
/// drift with the file layout: the shell's guard used to reach into the
/// core's source tree with a path of its own, and a move of a file in
/// the core broke the **shell's** build (audit 2026-10-21).
///
/// The needle is assembled at run time: written out literally it would
/// sit in this very file and the search would find itself.
pub fn audio_side_is_pure() -> bool {
    let needle = ["crate", "::", "scene"].join("");
    let sources = [
        include_str!("mod.rs"),
        include_str!("sequencer.rs"),
        include_str!("sfx.rs"),
        include_str!("synth.rs"),
    ];
    sources.iter().all(|source| !source.contains(&needle))
}

/// An event the scene hands to the audio side. Pure data — no device.
#[derive(Debug, Clone, PartialEq)]
pub enum AudioEvent {
    /// A sequencer note. `pitch` and `velocity` are MIDI numbers 0..=127,
    /// `duration_seconds` is the note's sounding length.
    Note {
        pitch: u8,
        velocity: u8,
        duration_seconds: f32,
    },
    /// A procedural one-shot.
    Sfx(SfxId),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The audio side must never read the simulation (roadmap Extension
    /// 3 §0.3).
    #[test]
    fn the_audio_side_never_reads_the_simulation() {
        assert!(
            audio_side_is_pure(),
            "the audio side must never read the simulation"
        );
    }
}
