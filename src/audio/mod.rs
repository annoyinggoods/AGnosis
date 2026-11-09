//! The audio side: a worker thread that owns the sound device.
//!
//! WHY a worker thread: the device is opened and driven **off** the UI
//! thread (roadmap Extension 3 §2.2). A sound card can block for tens of
//! milliseconds when it is opened, and the parade's fixed step must
//! never wait for it. The scene hands the audio side **events** — a note
//! or a one-shot — through a bounded queue, and the worker renders them.
//!
//! WHY no device is not an error: a CI runner has no sound card
//! (roadmap §2.3), so a failed open is reported through one trace line
//! and the parade runs silent. The exit code is never affected, and the
//! context itself never reports an error to its caller.
//!
//! WHY a bounded queue that drops the oldest: the parade must never
//! wait for the sound card. When the queue is full the **oldest** event
//! is dropped — a stale cluck is worth less than the newest one — and
//! the producer never blocks.
//!
//! The audio side is a **pure consumer** of the simulation: a note is
//! fired by a step index, never by a callback into the scene
//! (roadmap §0.3). Nothing in this module reads the simulation.

// WHY the re-exports: the pure half of the audio side (the event, the
// sequencer's timeline, the one-shots and the oscillator) lives in the
// `agnosis-core` crate, because the scene — a core module — asks for a
// sound and the core must stay platform-free. The **device**, the worker
// thread and the bounded queue stay here, because the sound card is
// native-only. The inline modules re-export the core's submodules so
// `crate::audio::sequencer::…` and friends keep resolving for the shell.
//
// WHY the allow: the shell is a **binary** crate, so a `pub use` has no
// consumer outside it and rustc reports the names the shell does not
// use *today* as unused imports. The re-export list is the shell's
// audio vocabulary — it is what keeps `crate::audio::Note` (and the
// rest) resolving for the shell's own code.
#[allow(unused_imports)]
pub use agnosis_core::audio::{AudioEvent, Note, Sequencer, SfxId, Track};

pub mod sequencer {
    pub use agnosis_core::audio::sequencer::*;
}

pub mod sfx {
    pub use agnosis_core::audio::sfx::*;
}

pub mod synth {
    pub use agnosis_core::audio::synth::*;
}

use std::collections::VecDeque;
use std::num::{NonZeroU16, NonZeroU32};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use rodio::stream::{DeviceSinkBuilder, DeviceSinkError, MixerDeviceSink};

/// How many events the queue holds.
const EVENT_CAPACITY: usize = 256;

/// How long the worker waits for an event before it re-checks the stop
/// flag. Short, so shutting down never waits on a sound that is not
/// coming.
const IDLE_WAIT: Duration = Duration::from_millis(25);

/// How much audio may pile up in the output queue before an event is
/// dropped.
///
/// The output queue is a timeline: an event that waits behind a backlog
/// is late, and a late note breaks the rhythm the strip was written for.
/// The backlog is therefore capped — a burst of events loses the
/// surplus instead of dragging the melody behind it.
const MAX_BACKLOG_SECONDS: f32 = 0.25;

/// How many samples the output queue hands over in one go.
const CHUNK_SAMPLES: usize = 512;

/// The envelope the track's notes are made of.
const NOTE_ENVELOPE: synth::Envelope = synth::Envelope {
    attack: 0.005,
    decay: 0.06,
    sustain: 0.6,
    release: 0.12,
};

/// The audio side: a worker thread that owns the sound device.
pub struct AudioContext {
    shared: Arc<Shared>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl AudioContext {
    /// Start the audio worker. **Never fails**: with no device the app
    /// runs silent and one trace line says so.
    pub fn start(cfg: &agnosis_core::config::AudioConfig) -> Self {
        let waveform =
            synth::waveform_from_name(&cfg.waveform).unwrap_or(synth::Waveform::Triangle);
        Self::spawn(cfg.volume, cfg.muted, waveform)
    }

    /// Queue one event. **Never blocks** and never fails: a full queue
    /// drops the OLDEST event rather than making the producer wait.
    pub fn push(&self, event: AudioEvent) {
        let mut queue = lock_events(&self.shared);
        if queue.len() >= EVENT_CAPACITY {
            queue.pop_front();
        }
        queue.push_back(event);
        drop(queue);
        self.shared.signal.notify_one();
    }

    /// The master gain, clamped into 0.0..=1.0.
    pub fn set_volume(&mut self, volume: f32) {
        self.shared
            .control
            .volume
            .store(clamped_volume(volume).to_bits(), Ordering::Relaxed);
    }

    /// Mute or unmute the whole output.
    pub fn set_muted(&mut self, muted: bool) {
        self.shared.control.muted.store(muted, Ordering::Relaxed);
    }

    /// Change the voice the notes are played in.
    ///
    /// The voice is a **live** knob: the editor's settings panel offers
    /// it, the repair pass validates it and the config reloads live, so
    /// the new voice must take effect while the parade runs — not only
    /// on the next start (audit 2026-10-21 §G14).
    pub fn set_waveform(&mut self, waveform: synth::Waveform) {
        self.shared
            .control
            .waveform
            .store(waveform_bits(waveform), Ordering::Relaxed);
    }

    /// True when a sound device is open (diagnostics only).
    pub fn has_device(&self) -> bool {
        self.shared.control.has_device.load(Ordering::Relaxed)
    }

    /// Start the worker with an explicit volume and mute flag.
    fn spawn(volume: f32, muted: bool, waveform: synth::Waveform) -> Self {
        Self::spawn_with_device(volume, muted, waveform, true)
    }

    /// Start the worker, optionally without ever opening a device.
    fn spawn_with_device(
        volume: f32,
        muted: bool,
        waveform: synth::Waveform,
        open_device: bool,
    ) -> Self {
        let shared = Arc::new(Shared::new(volume, muted, waveform, open_device));
        let worker_shared = Arc::clone(&shared);
        // A thread that cannot be spawned is not an error either: the app
        // runs silent.
        let worker = std::thread::Builder::new()
            .name("agnosis-audio".to_string())
            .spawn(move || worker_loop(worker_shared))
            .ok();
        Self { shared, worker }
    }

    /// A context that never opens a sound device.
    ///
    /// The test suite must not open the machine's sound card: a test that
    /// plays a note is a test that makes noise on the machine it runs on,
    /// and a test that opens a device is a test that fails on a CI runner
    /// with no audio hardware.
    #[cfg(test)]
    pub fn silent() -> Self {
        Self::spawn_with_device(0.0, true, synth::Waveform::Triangle, false)
    }
}

impl Drop for AudioContext {
    fn drop(&mut self) {
        self.shared.control.stop.store(true, Ordering::Relaxed);
        self.shared.signal.notify_all();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The state the context and the worker share.
struct Shared {
    /// The bounded event queue.
    events: Mutex<VecDeque<AudioEvent>>,
    /// Woken when the producer pushes an event.
    signal: Condvar,
    /// The rendered audio waiting for the device.
    samples: Arc<Mutex<VecDeque<f32>>>,
    /// The knobs the caller can turn while the worker runs.
    control: Arc<Control>,
}

impl Shared {
    fn new(volume: f32, muted: bool, waveform: synth::Waveform, open_device: bool) -> Self {
        Self {
            events: Mutex::new(VecDeque::with_capacity(EVENT_CAPACITY)),
            signal: Condvar::new(),
            samples: Arc::new(Mutex::new(VecDeque::new())),
            control: Arc::new(Control {
                volume: AtomicU32::new(clamped_volume(volume).to_bits()),
                muted: AtomicBool::new(muted),
                waveform: AtomicU32::new(waveform_bits(waveform)),
                has_device: AtomicBool::new(false),
                stop: AtomicBool::new(false),
                open_device: AtomicBool::new(open_device),
            }),
        }
    }

    fn is_stopping(&self) -> bool {
        self.control.stop.load(Ordering::Relaxed)
    }
}

/// The knobs the caller can turn while the worker runs.
struct Control {
    /// The master gain, as the bits of an `f32`.
    volume: AtomicU32,
    /// The mute flag.
    muted: AtomicBool,
    /// The voice the notes are played in, as the waveform's number.
    ///
    /// The waveform is a **knob**, not a constant: the config reloads
    /// live, so the voice must be able to change while the worker runs
    /// (audit 2026-10-21 §G14).
    waveform: AtomicU32,
    /// Whether a device is open right now.
    has_device: AtomicBool,
    /// Whether the worker should stop.
    stop: AtomicBool,
    /// Whether the worker may open a sound device at all.
    open_device: AtomicBool,
}

/// The audio worker's loop: open the device, play, and survive its
/// disappearance.
fn worker_loop(shared: Arc<Shared>) {
    let mut sessions = 0usize;
    if !shared.control.open_device.load(Ordering::Relaxed) {
        silent_loop(&shared);
        return;
    }
    loop {
        if shared.is_stopping() {
            return;
        }
        let failed = Arc::new(AtomicBool::new(false));
        let sink = match open_sink(&failed) {
            Ok(sink) => sink,
            Err(err) => {
                report_no_device(sessions > 0, &err);
                silent_loop(&shared);
                return;
            }
        };
        sessions += 1;
        let died = run_session(&shared, sink, &failed);
        if !died || shared.is_stopping() {
            return;
        }
        if sessions > 1 {
            agnosis_core::trace::warn("audio: the device died again, giving up (silent)");
            silent_loop(&shared);
            return;
        }
        agnosis_core::trace::warn("audio: the device died, restarting it once");
    }
}

/// Open the default output device, with an error callback that flags the
/// device's death.
fn open_sink(failed: &Arc<AtomicBool>) -> Result<MixerDeviceSink, DeviceSinkError> {
    let builder = DeviceSinkBuilder::from_default_device()?.with_error_callback({
        let failed = Arc::clone(failed);
        move |err: rodio::CpalError| {
            agnosis_core::trace::warn(&format!("audio: device error ({err})"));
            failed.store(true, Ordering::Relaxed);
        }
    });
    // The fallback opens the same device with another configuration; the
    // last resort is any other output device the host knows about.
    builder
        .open_sink_or_fallback()
        .or_else(|_| DeviceSinkBuilder::open_default_sink())
}

/// Report that there is no device: one line, and the parade goes on.
fn report_no_device(reopening: bool, err: &DeviceSinkError) {
    if reopening {
        agnosis_core::trace::warn(&format!("audio: no device ({err}), running silent"));
        return;
    }
    agnosis_core::trace::trace(&format!("audio: no device, running silent ({err})"));
}

/// Play until the device dies or the worker is asked to stop.
///
/// Returns true when the device died.
fn run_session(shared: &Arc<Shared>, sink: MixerDeviceSink, failed: &Arc<AtomicBool>) -> bool {
    let rate = sink.config().sample_rate().get();
    let source = QueueSource::new(
        Arc::clone(&shared.samples),
        Arc::clone(&shared.control),
        rate,
    );
    // The mixer owns the source from here on; the sink owns the device.
    sink.mixer().add(source);
    shared.control.has_device.store(true, Ordering::Relaxed);
    agnosis_core::trace::trace(&format!(
        "audio: the device is open, the voice is {}",
        current_waveform(shared).as_str()
    ));
    let mut gate = sfx::CooldownGate::new();
    let started = Instant::now();
    loop {
        if shared.is_stopping() {
            break;
        }
        if failed.load(Ordering::Relaxed) {
            break;
        }
        let now = started.elapsed().as_secs_f32();
        pump_gated(shared, rate, &mut gate, now);
    }
    shared.control.has_device.store(false, Ordering::Relaxed);
    failed.load(Ordering::Relaxed)
}

/// Render the queued events into the output queue, honouring the effects'
/// cooldowns.
///
/// The cooldown is the effect's own: a train of events must not
/// machine-gun the device (roadmap Extension 3 §4.1). Ten clucks in a
/// second produce at most three.
fn pump_gated(shared: &Arc<Shared>, rate: u32, gate: &mut sfx::CooldownGate, now: f32) {
    // The voice is read from the **control**, so a change made while the
    // parade runs is heard on the next pump (audit 2026-10-21 §G14).
    let waveform = current_waveform(shared);
    for event in take_events(shared) {
        let AudioEvent::Sfx(id) = event else {
            let samples = render_event(event, rate, waveform);
            push_samples(shared, &samples, rate);
            continue;
        };
        if !gate.allow(id, now) {
            agnosis_core::trace::trace(&format!("audio: the {} is on cooldown", id.as_str()));
            continue;
        }
        let samples = render_event(AudioEvent::Sfx(id), rate, waveform);
        push_samples(shared, &samples, rate);
    }
}

/// Render one event into samples. The per-effect gain is already part of
/// the effect's own samples.
fn render_event(event: AudioEvent, rate: u32, waveform: synth::Waveform) -> Vec<f32> {
    match event {
        AudioEvent::Note {
            pitch,
            velocity,
            duration_seconds,
        } => synth::render_note(
            pitch,
            velocity,
            duration_seconds,
            waveform,
            &NOTE_ENVELOPE,
            rate,
        ),
        AudioEvent::Sfx(id) => sfx::render(id, rate),
    }
}

/// Hand the rendered samples to the device — **as much of them as the
/// backlog budget allows**.
///
/// The whole event used to be dropped when it did not fit: a Squeak or
/// a Thunder is longer than the whole backlog (0.25 s), so those two
/// could **never** sound — the queue was never empty enough for them
/// (audit 2026-10-23 §F-090). A partial fill starts the sound instead:
/// the head is played, the surplus is dropped. An event that arrives
/// onto an already-full queue still lands nothing (the timeline would
/// be late, not truncated).
fn push_samples(shared: &Arc<Shared>, samples: &[f32], rate: u32) {
    if samples.is_empty() {
        return;
    }
    let mut queue = lock_samples(shared);
    let limit = (rate as f32 * MAX_BACKLOG_SECONDS) as usize;
    let room = limit.saturating_sub(queue.len());
    queue.extend(samples.iter().copied().take(room));
}

/// Take the queued events, waiting briefly for the first one.
fn take_events(shared: &Arc<Shared>) -> Vec<AudioEvent> {
    let mut queue = lock_events(shared);
    if queue.is_empty() {
        let (guard, _timeout) = shared
            .signal
            .wait_timeout(queue, IDLE_WAIT)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        queue = guard;
    }
    queue.drain(..).collect()
}

/// Run without a device: the events are dropped, the parade is silent.
fn silent_loop(shared: &Arc<Shared>) {
    loop {
        if shared.is_stopping() {
            return;
        }
        discard_events(shared);
    }
}

/// Wait for events and throw them away.
fn discard_events(shared: &Arc<Shared>) {
    let mut queue = lock_events(shared);
    if queue.is_empty() {
        let (guard, _timeout) = shared
            .signal
            .wait_timeout(queue, IDLE_WAIT)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        queue = guard;
    }
    queue.clear();
}

/// The event queue's lock, recovered from a poisoned mutex: a panic in
/// the worker must not take the parade down with it.
fn lock_events(shared: &Shared) -> MutexGuard<'_, VecDeque<AudioEvent>> {
    match shared.events.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The output queue's lock, recovered from a poisoned mutex.
fn lock_samples(shared: &Shared) -> MutexGuard<'_, VecDeque<f32>> {
    match shared.samples.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The waveform's number, as the control stores it.
fn waveform_bits(waveform: synth::Waveform) -> u32 {
    match waveform {
        synth::Waveform::Sine => 0,
        synth::Waveform::Triangle => 1,
        synth::Waveform::Square => 2,
    }
}

/// The waveform a stored number means.
///
/// An unknown number is the default: a knob must never hand the
/// synthesizer a voice it does not know.
fn waveform_from_bits(bits: u32) -> synth::Waveform {
    match bits {
        0 => synth::Waveform::Sine,
        2 => synth::Waveform::Square,
        _ => synth::Waveform::Triangle,
    }
}

/// The voice the control currently names.
fn current_waveform(shared: &Shared) -> synth::Waveform {
    waveform_from_bits(shared.control.waveform.load(Ordering::Relaxed))
}

/// The master gain, clamped into 0.0..=1.0.
fn clamped_volume(volume: f32) -> f32 {
    if !volume.is_finite() {
        return 0.0;
    }
    volume.clamp(0.0, 1.0)
}

/// The source the mixer plays: the rendered audio, with the master gain
/// and the mute applied on the way out.
///
/// The source is **infinite**: it returns silence when the queue is
/// empty instead of ending, because a source that ends takes the whole
/// mix down with it.
struct QueueSource {
    samples: Arc<Mutex<VecDeque<f32>>>,
    control: Arc<Control>,
    /// A small window of samples taken from the queue, so the device's
    /// callback does not lock once per sample.
    cache: Vec<f32>,
    cache_pos: usize,
    sample_rate: rodio::SampleRate,
}

impl QueueSource {
    fn new(samples: Arc<Mutex<VecDeque<f32>>>, control: Arc<Control>, sample_rate: u32) -> Self {
        let rate = NonZeroU32::new(sample_rate.max(1)).unwrap_or(NonZeroU32::MIN);
        Self {
            samples,
            control,
            cache: Vec::with_capacity(CHUNK_SAMPLES),
            cache_pos: 0,
            sample_rate: rate,
        }
    }

    /// The next sample of the queue, refilling the window when it runs
    /// dry.
    fn next_sample(&mut self) -> f32 {
        if self.cache_pos >= self.cache.len() {
            self.refill();
        }
        let sample = self.cache.get(self.cache_pos).copied().unwrap_or(0.0);
        self.cache_pos += 1;
        sample
    }

    /// Take the next window of samples out of the queue.
    fn refill(&mut self) {
        self.cache.clear();
        self.cache_pos = 0;
        let mut queue = match self.samples.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let take = queue.len().min(CHUNK_SAMPLES);
        self.cache.extend(queue.drain(..take));
    }
}

impl Iterator for QueueSource {
    type Item = f32;

    fn next(&mut self) -> Option<Self::Item> {
        let sample = self.next_sample();
        if self.control.muted.load(Ordering::Relaxed) {
            return Some(0.0);
        }
        let volume = f32::from_bits(self.control.volume.load(Ordering::Relaxed));
        Some(sample * volume)
    }
}

impl rodio::Source for QueueSource {
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        // Mono: the mixer spreads it over the device's channels.
        NonZeroU16::MIN
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        self.sample_rate
    }

    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A context with no worker: the queue is never drained, so the
    /// drop-oldest rule can be asserted without racing the worker.
    fn detached() -> AudioContext {
        AudioContext {
            shared: Arc::new(Shared::new(0.7, false, synth::Waveform::Triangle, true)),
            worker: None,
        }
    }

    /// A note event that carries its index in the pitch, so the order of
    /// the queue can be read back.
    fn event(index: usize) -> AudioEvent {
        AudioEvent::Note {
            pitch: (index % 128) as u8,
            velocity: 100,
            duration_seconds: 0.1,
        }
    }

    #[test]
    fn no_device_is_not_an_error() {
        // The machine may or may not have a sound card: the context must
        // come up either way, and must never report an error.
        let mut context = AudioContext::spawn(0.0, true, synth::Waveform::Triangle);
        context.push(event(1));
        context.push(AudioEvent::Sfx(SfxId::Cluck));
        context.set_volume(0.5);
        context.set_muted(true);
        let _ = context.has_device();
    }

    #[test]
    fn a_full_channel_drops_the_oldest_event() {
        let context = detached();
        let overflow = 10;
        for index in 0..(EVENT_CAPACITY + overflow) {
            context.push(event(index));
        }
        let queue = lock_events(&context.shared);
        assert_eq!(queue.len(), EVENT_CAPACITY, "the queue grew past its cap");
        assert_eq!(
            queue.front(),
            Some(&event(overflow)),
            "the oldest events must be the ones dropped"
        );
        assert_eq!(
            queue.back(),
            Some(&event(EVENT_CAPACITY + overflow - 1)),
            "the newest event must survive"
        );
    }

    #[test]
    fn the_audio_thread_never_touches_the_scene() {
        // The needle is assembled at run time: written out literally it
        // would sit in this very file and the search would find itself.
        let needle = ["crate", "::", "scene"].join("");
        // The shell's own half of the audio side — the device, the worker
        // thread and the bounded queue — is checked here.
        assert!(
            !include_str!("mod.rs").contains(&needle),
            "the audio side must never read the simulation"
        );
        // The core's half (the sequencer, the one-shots and the
        // oscillator) is checked **by the core**, against its own
        // sources. The shell must not hard-code the core's file layout:
        // a rename inside the core broke the **shell's** build with a
        // cryptic `include_str!` error (audit 2026-10-21 §G29).
        assert!(
            agnosis_core::audio::audio_side_is_pure(),
            "the core's audio side must never read the simulation"
        );
    }

    #[test]
    fn the_waveform_is_a_live_knob() {
        // The voice must follow the config while the parade runs: the
        // editor's settings panel offers it, the repair pass validates
        // it and the config reloads live — so the old voice must not
        // survive a change (audit 2026-10-21 §G14).
        let shared = Arc::new(Shared::new(1.0, false, synth::Waveform::Sine, false));
        let mut gate = sfx::CooldownGate::new();
        shared.events.lock().expect("events").push_back(event(1));
        pump_gated(&shared, 44_100, &mut gate, 0.0);
        let sine = rendered(&shared);
        assert!(!sine.is_empty(), "the pump must render the note");

        // The knob is turned while the worker runs.
        shared
            .control
            .waveform
            .store(waveform_bits(synth::Waveform::Square), Ordering::Relaxed);
        shared.events.lock().expect("events").push_back(event(1));
        pump_gated(&shared, 44_100, &mut gate, 0.0);
        let square = rendered(&shared);
        assert!(!square.is_empty(), "the pump must render the note");
        assert_ne!(
            sine, square,
            "the rendered samples must change shape with the voice"
        );
    }

    /// The samples the pump rendered.
    fn rendered(shared: &Shared) -> Vec<f32> {
        let mut queue = lock_samples(shared);
        queue.drain(..).collect()
    }

    #[test]
    fn the_volume_is_clamped() {
        assert_eq!(clamped_volume(0.5), 0.5);
        assert_eq!(clamped_volume(2.0), 1.0);
        assert_eq!(clamped_volume(-1.0), 0.0);
        assert_eq!(clamped_volume(f32::NAN), 0.0);
    }

    #[test]
    fn the_source_plays_silence_when_the_queue_is_empty() {
        let samples = Arc::new(Mutex::new(VecDeque::new()));
        let control = Arc::new(Control {
            volume: AtomicU32::new(clamped_volume(1.0).to_bits()),
            muted: AtomicBool::new(false),
            waveform: AtomicU32::new(waveform_bits(synth::Waveform::Triangle)),
            has_device: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            open_device: AtomicBool::new(true),
        });
        let mut source = QueueSource::new(Arc::clone(&samples), control, 44_100);
        assert_eq!(source.next(), Some(0.0));
        samples.lock().expect("queue").push_back(0.5);
        assert_eq!(source.next(), Some(0.5));
    }

    #[test]
    fn the_source_is_muted() {
        let samples = Arc::new(Mutex::new(VecDeque::new()));
        samples.lock().expect("queue").push_back(0.5);
        let control = Arc::new(Control {
            volume: AtomicU32::new(clamped_volume(1.0).to_bits()),
            muted: AtomicBool::new(true),
            waveform: AtomicU32::new(waveform_bits(synth::Waveform::Triangle)),
            has_device: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            open_device: AtomicBool::new(true),
        });
        let mut source = QueueSource::new(samples, control, 44_100);
        assert_eq!(source.next(), Some(0.0));
    }

    /// Every one-shot makes **some** sound — a partial fill, not a drop.
    ///
    /// A Squeak and a Thunder are longer than the whole backlog budget
    /// (0.25 s), so the old all-or-nothing rule could never fit them:
    /// they were silent forever (audit 2026-10-23 §F-090). The pump now
    /// fills the queue up to the limit, so every `SfxId` starts.
    #[test]
    fn every_sfx_starts_playing_at_device_rate() {
        let ids = [
            SfxId::Cluck,
            SfxId::Squeak,
            SfxId::Chirp,
            SfxId::Splosh,
            SfxId::Thunder,
        ];
        for id in ids {
            let shared = Arc::new(Shared::new(1.0, false, synth::Waveform::Triangle, false));
            let mut gate = sfx::CooldownGate::new();
            shared
                .events
                .lock()
                .expect("events")
                .push_back(AudioEvent::Sfx(id));
            pump_gated(&shared, 48_000, &mut gate, 0.0);
            let samples = rendered(&shared);
            assert!(
                !samples.is_empty(),
                "the {id:?} must start playing, got nothing"
            );
        }
    }

    /// The partial fill: a sample longer than the backlog is truncated,
    /// not dropped, and an already-full queue takes nothing more.
    #[test]
    fn push_samples_fills_partially_and_respects_the_limit() {
        let shared = Arc::new(Shared::new(1.0, false, synth::Waveform::Triangle, false));
        // 48 kHz * 0.25 s = 12 000 samples of budget.
        let long: Vec<f32> = vec![0.5; 20_000];
        push_samples(&shared, &long, 48_000);
        assert_eq!(
            lock_samples(&shared).len(),
            12_000,
            "a too-long event is truncated to the budget, not dropped"
        );
        // The queue is at its limit: a second event lands nothing.
        let short: Vec<f32> = vec![0.25; 100];
        push_samples(&shared, &short, 48_000);
        assert_eq!(
            lock_samples(&shared).len(),
            12_000,
            "a full queue accepts nothing more"
        );
    }
}
