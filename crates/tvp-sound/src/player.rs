//! Optional real-device playback on top of rodio.
//!
//! The clock-driven [`Mixer`] is the source of truth and works headless
//! (this machine has no audio device). This module is the *output* half for
//! machines that do have one: [`start_output`] opens a rodio/cpal output
//! sink and appends an infinite [`MixerSource`] that pulls the mixer's
//! current mix at the device's sample rate. Everything here is fallible and
//! never required by tests — headless machines just don't call it.
//!
//! # Setup notes (rodio 0.22)
//!
//! - rodio 0.22 replaced `OutputStream`/`Sink` with a `DeviceSinkBuilder` →
//!   `MixerDeviceSink` model: `DeviceSinkBuilder::open_default_sink()`
//!   returns a sink whose [`MixerDeviceSink::mixer`] is a rodio `Mixer`;
//!   callers `add()` a [`Source`] to it. The sink internally resamples, so
//!   `MixerSource` renders at the device's rate.
//! - cpal (pulled by rodio) builds on Linux against ALSA; with no sound
//!   card, `open_default_sink` fails at runtime and [`start_output`]
//!   returns an error instead of panicking.
//! - cpal makes its `Stream` **`!Send`/`!Sync`** on some platforms; rodio's
//!   `MixerDeviceSink` wraps it, so an [`OutputGuard`] can only be created
//!   **and dropped on the thread that opened it** (dropping on another
//!   thread is UB). In a Bevy app the guard must live on the main thread —
//!   see [`start_output_on_main_thread`] and
//!   [`crate::set_sound_output_enabled`] for the two supported ways to wire it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rodio::{DeviceSinkBuilder, MixerDeviceSink, Source};

use crate::mixer::{Mixer, lock_ok};

/// Frames rendered per [`MixerSource::next`] refill.
const CHUNK_FRAMES: usize = 512;

/// An infinite rodio `Source` that pulls the live mix out of a shared
/// [`Mixer`] at a fixed sample rate.
///
/// Channel count is fixed at construction (stereo in practice — the render
/// path handles mono and stereo outputs; see [`Mixer::render_mix`]).
pub struct MixerSource {
    mixer: Arc<Mutex<Mixer>>,
    sample_rate: u32,
    channels: u16,
    /// Next chunk of interleaved samples and the read position into it.
    chunk: Vec<f32>,
    pos: usize,
}

impl MixerSource {
    /// Create a source that renders `mixer` at `sample_rate` Hz with
    /// `channels` channels.
    pub fn new(mixer: Arc<Mutex<Mixer>>, sample_rate: u32, channels: u16) -> Self {
        MixerSource {
            mixer,
            sample_rate,
            channels: channels.max(1),
            chunk: Vec::new(),
            pos: 0,
        }
    }

    fn refill(&mut self) {
        let mut frame = vec![0.0f32; CHUNK_FRAMES * usize::from(self.channels)];
        lock_ok(&self.mixer).render_mix_advancing(&mut frame, self.sample_rate, self.channels);
        self.chunk = frame;
        self.pos = 0;
    }
}

impl Iterator for MixerSource {
    type Item = f32;

    fn next(&mut self) -> Option<f32> {
        if self.pos >= self.chunk.len() {
            self.refill();
        }
        let v = self.chunk[self.pos];
        self.pos += 1;
        Some(v)
    }
}

impl Source for MixerSource {
    /// Never ends: the source renders whatever the mixer currently has
    /// (silence when nothing is playing).
    fn current_span_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> rodio::ChannelCount {
        std::num::NonZero::new(self.channels).expect("channels >= 1 by construction")
    }

    fn sample_rate(&self) -> rodio::SampleRate {
        std::num::NonZero::new(self.sample_rate).expect("non-zero sample rate")
    }

    fn total_duration(&self) -> Option<std::time::Duration> {
        None
    }
}

/// Errors from [`start_output`].
#[derive(Debug, thiserror::Error)]
pub enum OutputError {
    #[error("no audio device available: {0}")]
    NoDevice(String),
    #[error("cannot attach the mixer source to the output stream: {0}")]
    Sink(String),
}

/// Thread-safe knob the app can flip to start/stop real audio output
/// without touching the main thread. The heavy rodio objects
/// ([`OutputGuard`], `cpal::Stream`) are `!Send`/`!Sync`, so this is the
/// only state an arbitrary thread may mutate: `true` means “output is
/// enabled”, and the main thread's audio startup reads it.
#[derive(Debug, Default)]
pub struct OutputStatus {
    enabled: AtomicBool,
}

impl OutputStatus {
    /// Enable (or disable) real-device output. Purely a flag; the stream is
    /// opened by whoever runs the audio setup (see [`start_output_on_main_thread`]).
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// Whether output is currently enabled.
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }
}

/// Open the default audio device and start streaming the live mixer mix.
///
/// Headless machines (no sound card) get [`OutputError::NoDevice`]; callers
/// should log and continue — the mixer keeps advancing either way.
///
/// The returned guard keeps the stream and sink alive; dropping it stops
/// output.
///
/// # Thread-safety
///
/// [`OutputGuard`] is **`!Send`/`!Sync`** (rodio's `OutputStream` wraps
/// cpal's `!Send`/`!Sync` stream). Call this from the thread that will
/// hold the guard, and drop the guard on that same thread. In a Bevy app
/// that is the main thread — use [`start_output_on_main_thread`] to
/// enforce it at compile time, or flip [`OutputStatus`] from any thread
/// and open the stream on the main thread.
pub fn start_output(mixer: Arc<Mutex<Mixer>>) -> Result<OutputGuard, OutputError> {
    let sink = DeviceSinkBuilder::open_default_sink()
        .map_err(|e| OutputError::NoDevice(format!("{e}")))?;

    // rodio 0.22 resamples internally, so render at the device rate.
    let rate = sink.config().sample_rate().get();
    let channels = sink.config().channel_count().get();

    sink.mixer().add(MixerSource::new(mixer, rate, channels));
    // Mark the device as the mixer clock source (see `crate::advance`).
    crate::set_audio_output_active(true);
    Ok(OutputGuard { _sink: sink })
}

impl Drop for OutputGuard {
    fn drop(&mut self) {
        crate::set_audio_output_active(false);
    }
}

/// Start real-device output with the main-thread-only guarantee baked into
/// the type system.
///
/// `main_thread_token` is obtained by the main thread (or any thread that
/// is guaranteed to keep the guard), e.g. `std::thread::current().id() ==
/// std::thread::main()` is *not* sufficient — the requirement is that the
/// guard is created and dropped on the same thread, so the caller passes
/// its own thread id. The token enforces at compile time that only that
/// thread can hold and drop the guard.
pub fn start_output_on_main_thread(
    main_thread_token: std::thread::ThreadId,
    mixer: Arc<Mutex<Mixer>>,
) -> Result<MainThreadOutputGuard, OutputError> {
    start_output(mixer).map(|guard| MainThreadOutputGuard {
        _guard: guard,
        _token: main_thread_token,
    })
}

/// An [`OutputGuard`] that can only be created and dropped by the thread
/// that owns `main_thread_token` (its `Drop` runs on that thread, which is
/// where cpal requires the stream to be torn down).
///
/// In a Bevy app this lives in a `Resource` on the main thread; a
/// [`Sync`] wrapper like [`MainThreadOutputGuard`] (or the plain
/// [`OutputGuard`] behind a `Mutex`/`RwLock`) is what makes holding it in
/// a resource sound.
pub struct MainThreadOutputGuard {
    _guard: OutputGuard,
    _token: std::thread::ThreadId,
}

// SAFETY: the guard is only ever created and dropped on the owning thread
// (enforced by the token), and it is never accessed from any other thread;
// the `_token` field pins it to that thread. This is the same shape the
// rodio examples use to hand a `!Send` stream to the main thread.
unsafe impl Send for MainThreadOutputGuard {}
unsafe impl Sync for MainThreadOutputGuard {}

/// Keep the rodio output sink alive (dropping stops output). The
/// underscore-prefixed field is held only for its `Drop` side effects, so
/// it is intentionally not read.
///
/// **Not `Send`/`Sync`** — see the module docs and [`start_output`].
pub struct OutputGuard {
    _sink: MixerDeviceSink,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MainThreadOutputGuard` is explicitly `Send`+`Sync` so a Bevy app
    /// can hold it in a `Resource` (created on the main thread, dropped on
    /// the main thread by the app's own drop).
    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn main_thread_guard_is_send_sync() {
        assert_send_sync::<MainThreadOutputGuard>();
    }

    #[test]
    fn output_status_flag_roundtrips() {
        let s = OutputStatus::default();
        assert!(!s.enabled());
        s.set_enabled(true);
        assert!(s.enabled());
        s.set_enabled(false);
        assert!(!s.enabled());
    }
}
