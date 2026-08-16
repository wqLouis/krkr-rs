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

/// Open the default audio device and start streaming the live mixer mix.
///
/// Headless machines (no sound card) get [`OutputError::NoDevice`]; callers
/// should log and continue — the mixer keeps advancing either way.
///
/// The returned guard keeps the stream and sink alive; dropping it stops
/// output.
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

/// Keeps the rodio output stream and sink alive (dropping stops output).
/// The underscore-prefixed fields are held only for their `Drop` side
/// effects, so they are intentionally not read.
pub struct OutputGuard {
    _stream: OutputStream,
    _handle: OutputStreamHandle,
    _sink: Sink,
}
