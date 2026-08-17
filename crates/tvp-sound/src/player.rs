//! Optional real-device playback on top of rodio.
//!
//! The clock-driven [`Mixer`] is the source of truth and works headless
//! (this machine has no audio device). This module is the *output* half for
//! machines that do have one: [`start_output`] opens a rodio/cpal output
//! stream and appends an infinite [`MixerSource`] that pulls the mixer's
//! current mix at the device's sample rate. Everything here is fallible and
//! never required by tests — headless machines just don't call it.
//!
//! # Setup notes (rodio 0.20)
//!
//! - `rodio = { version = "0.20", default-features = false, features =
//!   ["symphonia-wav", "symphonia-vorbis", "symphonia-flac",
//!   "symphonia-mp3"] }`. rodio's decode features map onto `symphonia`
//!   features; the direct `symphonia` dependency in `Cargo.toml` enables
//!   the same set (plus `ogg`/`pcm`) and Cargo unifies them, so rodio's
//!   internal symphonia is the same build as ours.
//! - rodio 0.20 does **not** resample: a source must deliver the device's
//!   sample rate. `MixerSource` is constructed with the device rate by
//!   [`start_output`] and renders every channel at that rate.
//! - cpal 0.15 (pulled by rodio 0.20) builds on Linux against ALSA; with no
//!   sound card, `OutputStream::try_default()` fails at runtime and
//!   [`start_output`] returns an error instead of panicking.
//! - cpal 0.15 deliberately makes `cpal::Stream` **`!Send`/`!Sync`** (the
//!   `NotSendSyncAcrossAllPlatforms` marker, kept for Android AAudio
//!   support). rodio's [`OutputStream`] wraps that stream, so an
//!   [`OutputGuard`] can only be created **and dropped on the thread that
//!   opened it** (dropping on another thread is UB). In a Bevy app the
//!   guard must live on the main thread — see [`start_output_on_main_thread`]
//!   and [`crate::set_sound_output_enabled`] for the two supported ways to wire it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use rodio::{OutputStream, OutputStreamHandle, Sink, Source, cpal};

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
        lock_ok(&self.mixer).render_mix(&mut frame, self.sample_rate, self.channels);
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
    fn current_frame_len(&self) -> Option<usize> {
        None
    }

    fn channels(&self) -> u16 {
        self.channels
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
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
    let (stream, handle) =
        OutputStream::try_default().map_err(|e| OutputError::NoDevice(format!("{e}")))?;
    let sink = Sink::try_new(&handle).map_err(|e| OutputError::Sink(format!("{e}")))?;

    // Render at the device's rate; rodio does not resample.
    let (rate, channels) = device_config();

    sink.append(MixerSource::new(mixer, rate, channels));
    Ok(OutputGuard {
        _stream: stream,
        _handle: handle,
        _sink: sink,
    })
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

/// Keep the rodio output stream and sink alive (dropping stops output).
/// The underscore-prefixed fields are held only for their `Drop` side
/// effects, so they are intentionally not read.
///
/// **Not `Send`/`Sync`** — see the module docs and [`start_output`].
pub struct OutputGuard {
    _stream: OutputStream,
    _handle: OutputStreamHandle,
    _sink: Sink,
}

/// Best-effort device sample rate / channel count (defaults 44100/2 when the
/// device cannot be queried).
fn device_config() -> (u32, u16) {
    use rodio::cpal::traits::{DeviceTrait, HostTrait};
    let Some(device) = cpal::default_host().default_output_device() else {
        return (44_100, 2);
    };
    match device.default_output_config() {
        Ok(config) => (config.sample_rate().0, config.channels()),
        Err(_) => (44_100, 2),
    }
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
