//! L1 audio output: stateless device object + explicit stream state.
//!
//! [`AudioDevice`] is immutable configuration — the requested [`AudioSpec`]
//! and the backend chosen when it was opened. All evolving stream state (the
//! OS stream, the software queue and the running flag) lives in the
//! caller-owned [`AudioOutputState`]; the middle layer (L2) stores it and
//! drives playback with [`AudioDevice::write`]. The object remembers nothing,
//! so two devices can coexist and no global or static is involved (D9).
//!
//! # Backends
//!
//! * the real backend is cpal on `platform-linux` and AAudio (via the `ndk`
//!   crate) on `platform-android`;
//! * [`AudioDevice::null`] is a mock that validates and discards samples, so
//!   headless hosts and tests need no audio hardware.

use api::error::AudioError;
use api::media::AudioSpec;

use crate::PlatformError;

#[cfg(all(feature = "platform-android", not(feature = "platform-linux")))]
use crate::android::audio as backend;
#[cfg(all(feature = "platform-linux", not(feature = "platform-android")))]
use crate::linux::audio as backend;

/// A stateless handle to an audio output backend.
///
/// The object holds only the requested [`AudioSpec`] and the backend selected
/// at open time; the stream and its queue live in the caller-owned
/// [`AudioOutputState`], so the object is immutable and free of interior
/// mutability (D9).
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

/// The mock backend's state: a running flag and nothing else.
struct NullState {
    running: bool,
}

impl AudioDevice {
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
    /// The mock validates the spec and frame alignment exactly like the real
    /// device but discards the samples, so headless hosts and tests can drive
    /// the same call path.
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
                kind: StateKind::Null(NullState { running: true }),
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
    /// count).
    ///
    /// # Errors
    ///
    /// [`PlatformError::Audio`] wrapping [`AudioError::InvalidSpec`] for a
    /// misaligned buffer or [`AudioError::NotRunning`] after
    /// [`AudioDevice::stop`].
    pub fn write(&self, state: &mut AudioOutputState, pcm: &[f32]) -> Result<(), PlatformError> {
        let channels = usize::from(self.spec.channels);
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
                if !null.running {
                    return Err(AudioError::NotRunning.into());
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
            (DeviceKind::Null, StateKind::Null(null)) => null.running = false,
            (DeviceKind::Real(device), StateKind::Real(stream)) => backend::stop(device, stream),
            _ => {}
        }
    }

    /// Returns whether the stream is currently running.
    pub fn is_running(&self, state: &AudioOutputState) -> bool {
        match (&self.kind, &state.kind) {
            (DeviceKind::Null, StateKind::Null(null)) => null.running,
            (DeviceKind::Real(_), StateKind::Real(stream)) => backend::is_running(stream),
            _ => false,
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
