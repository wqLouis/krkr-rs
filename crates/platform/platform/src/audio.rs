//! L1 audio output: stateless device object + explicit stream state.
//!
//! [`AudioDevice`] is immutable configuration — the requested [`AudioSpec`]
//! and the backend chosen when it was opened. All evolving stream state (the
//! OS stream, the software queue and the lifecycle flags) lives in the
//! caller-owned [`AudioOutputState`]; the middle layer (L2) stores it and
//! drives playback with [`AudioDevice::write`]. The object remembers nothing,
//! so two devices can coexist and no global or static is involved.
//!
//! The queue policy itself (a 100–250 ms pre-fill before the stream starts,
//! oldest-first dropping past the two-second cap, and a shared-gain fade in
//! and out of the per-channel hold on underrun) is shared by every backend and
//! lives in `audio_queue`.
//!
//! # Backends
//!
//! * the real backend is cpal on `platform-linux` and AAudio (via the `ndk`
//!   crate) on `platform-android`;
//! * [`AudioDevice::null`] is a mock that validates and buffers samples
//!   through the same [`AudioQueue`] policy as the real backend, so headless
//!   hosts and tests need no audio hardware.

use api::error::AudioError;
use api::media::AudioSpec;

use crate::PlatformError;
use crate::audio_queue::AudioQueue;

#[cfg(all(feature = "platform-android", not(feature = "platform-linux")))]
use crate::android::audio as backend;
#[cfg(all(feature = "platform-linux", not(feature = "platform-android")))]
use crate::linux::audio as backend;

/// A stateless handle to an audio output backend.
///
/// The object holds only the requested [`AudioSpec`] and the backend selected
/// at open time; the stream and its queue live in the caller-owned
/// [`AudioOutputState`], so the object is immutable and free of interior
/// mutability.
pub struct AudioDevice {
    spec: AudioSpec,
    kind: DeviceKind,
}

/// The stream and queue for one [`AudioDevice`].
///
/// Store this next to the object that opened it (L2 owns it). Dropping it
/// stops and releases the OS stream.
pub struct AudioOutputState {
    kind: StateKind,
}

enum DeviceKind {
    Null,
    Real(backend::Backend),
}

enum StateKind {
    Null(NullState),
    Real(backend::StreamState),
}

/// The mock backend's state: the same bounded queue and lifecycle flags as a
/// real device, but no OS stream.
struct NullState {
    queue: AudioQueue,
    /// Whether the mock stream has reached pre-fill and is "producing".
    playing: bool,
    /// Whether [`AudioDevice::stop`] has closed it (writes are then rejected).
    stopped: bool,
}

impl AudioDevice {
    /// Returns the default output device's preferred format.
    ///
    /// The **device** chooses the rate and channel count: the backend asks the
    /// default output device for its preferred configuration (cpal's
    /// `default_output_config` on Linux, an AAudio probe on Android), restricted
    /// to the 1–2 channels the L2 mixer can render.
    ///
    /// # Errors
    ///
    /// [`PlatformError::Audio`] wrapping [`AudioError::DeviceUnavailable`] when
    /// there is no output device, or the backend's own open error. A device
    /// that *exists* but reports no usable format yields a documented fallback
    /// format (with a logged warning) instead of an error, so the caller can
    /// still try to open it.
    pub fn default_spec() -> Result<AudioSpec, PlatformError> {
        backend::default_spec()
    }

    /// Opens the default real output device for `spec`.
    ///
    /// # Errors
    ///
    /// [`PlatformError::Audio`] wraps the typed [`AudioError`]:
    /// [`AudioError::InvalidSpec`] if the spec is malformed,
    /// [`AudioError::DeviceUnavailable`] when there is no output device,
    /// [`AudioError::Unsupported`] when no matching stream format exists, and
    /// [`AudioError::Open`]/[`AudioError::Stream`] when the backend fails.
    pub fn open(spec: AudioSpec) -> Result<(Self, AudioOutputState), PlatformError> {
        validate_spec(spec)?;
        let (backend, state) = backend::open(spec)?;
        Ok((
            Self {
                spec,
                kind: DeviceKind::Real(backend),
            },
            AudioOutputState {
                kind: StateKind::Real(state),
            },
        ))
    }

    /// Opens a silent mock device that needs no audio hardware.
    ///
    /// The mock validates the spec and frame alignment and applies exactly
    /// the same pre-fill/queue policy as the real device, but never touches a
    /// device, so headless hosts and tests can drive the same call path.
    ///
    /// # Errors
    ///
    /// [`PlatformError::Audio`] wrapping [`AudioError::InvalidSpec`] when the
    /// spec is malformed.
    pub fn null(spec: AudioSpec) -> Result<(Self, AudioOutputState), PlatformError> {
        validate_spec(spec)?;
        Ok((
            Self {
                spec,
                kind: DeviceKind::Null,
            },
            AudioOutputState {
                kind: StateKind::Null(NullState {
                    queue: AudioQueue::new(spec.sample_rate, spec.channels)?,
                    playing: false,
                    stopped: false,
                }),
            },
        ))
    }

    /// Returns the format this device was opened with.
    pub fn spec(&self) -> AudioSpec {
        self.spec
    }

    /// Queues interleaved `f32` PCM for playback.
    ///
    /// `pcm.len()` must be a whole number of frames (a multiple of the channel
    /// count). The real (and mock) stream is started by the first write that
    /// brings the queue up to its pre-fill target, so the device never drains
    /// an empty queue.
    ///
    /// # Errors
    ///
    /// [`PlatformError::Audio`] wrapping [`AudioError::InvalidSpec`] for a
    /// misaligned buffer or [`AudioError::NotRunning`] after
    /// [`AudioDevice::stop`].
    pub fn write(&self, state: &mut AudioOutputState, pcm: &[f32]) -> Result<(), PlatformError> {
        let channels =
            usize::try_from(self.spec.channels).map_err(|_| AudioError::InvalidSpec {
                message: format!(
                    "channel count {} does not fit in memory",
                    self.spec.channels
                ),
            })?;
        if channels == 0 || !pcm.len().is_multiple_of(channels) {
            return Err(AudioError::InvalidSpec {
                message: format!(
                    "a buffer of {} samples is not frame-aligned to {} channel(s)",
                    pcm.len(),
                    channels
                ),
            }
            .into());
        }
        if pcm.is_empty() {
            return Ok(());
        }
        match (&self.kind, &mut state.kind) {
            (DeviceKind::Null, StateKind::Null(null)) => {
                if null.stopped {
                    return Err(AudioError::NotRunning.into());
                }
                if null.queue.push(pcm) {
                    null.playing = true;
                }
                Ok(())
            }
            (DeviceKind::Real(device), StateKind::Real(stream)) => {
                backend::write(device, stream, pcm)
            }
            _ => Err(AudioError::InvalidSpec {
                message: "the audio state does not belong to this device".to_owned(),
            }
            .into()),
        }
    }

    /// Stops the stream and releases its OS resources. Idempotent.
    pub fn stop(&self, state: &mut AudioOutputState) {
        match (&self.kind, &mut state.kind) {
            (DeviceKind::Null, StateKind::Null(null)) => {
                null.stopped = true;
                null.playing = false;
            }
            (DeviceKind::Real(device), StateKind::Real(stream)) => backend::stop(device, stream),
            _ => {}
        }
    }

    /// Returns whether the stream is currently running.
    pub fn is_running(&self, state: &AudioOutputState) -> bool {
        match (&self.kind, &state.kind) {
            (DeviceKind::Null, StateKind::Null(null)) => null.playing && !null.stopped,
            (DeviceKind::Real(_), StateKind::Real(stream)) => backend::is_running(stream),
            _ => false,
        }
    }

    /// How much audio the device has buffered, in seconds.
    ///
    /// This is the delay between the audio the producer wrote and the audio
    /// the listener hears: a voice's rendered position runs that much ahead of
    /// the position the sound has reached. A caller that keeps a picture in
    /// step with a sound reads it here and hands it to the mixer.
    ///
    /// The mock device reports `0.0`: it discards what it is given, so it
    /// adds no playback delay, and its queue's depth is an artefact of the
    /// mock rather than a delay the listener would hear.
    pub fn queued_seconds(&self, state: &AudioOutputState) -> f64 {
        match (&self.kind, &state.kind) {
            (DeviceKind::Null, StateKind::Null(_)) => 0.0,
            (DeviceKind::Real(_), StateKind::Real(stream)) => backend::queued_seconds(stream),
            _ => 0.0,
        }
    }
}

/// Rejects a spec that cannot describe a stream.
fn validate_spec(spec: AudioSpec) -> Result<(), AudioError> {
    if spec.sample_rate == 0 {
        return Err(AudioError::InvalidSpec {
            message: "sample rate is zero".to_owned(),
        });
    }
    if spec.channels == 0 {
        return Err(AudioError::InvalidSpec {
            message: "channel count is zero".to_owned(),
        });
    }
    Ok(())
}
