//! Android platform backend (stub).
//!
//! The Android host is not implemented yet; selecting `platform-android`
//! compiles this stub so the higher layers build unchanged. [`run`] fails
//! fast with [`PlatformError::NotImplemented`] (D11: no silent fallback).

use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

use crate::{EventHandler, PlatformConfig, PlatformError};

/// The platform window handle (Android stub).
///
/// The type exists so [`crate::EventHandler`] signatures are identical across
/// platforms, but it cannot be constructed until the Android backend lands.
#[allow(dead_code)]
pub struct PlatformWindow {
    _private: (),
}

impl PlatformWindow {
    /// Returns the raw window handle (always an error in the stub).
    pub fn raw_window_handle(&self) -> Result<RawWindowHandle, PlatformError> {
        Err(PlatformError::NotImplemented(
            "android raw window handle".to_owned(),
        ))
    }

    /// Returns the raw display handle (always an error in the stub).
    pub fn raw_display_handle(&self) -> Result<RawDisplayHandle, PlatformError> {
        Err(PlatformError::NotImplemented(
            "android raw display handle".to_owned(),
        ))
    }

    /// Returns the surface size (always 1×1 in the stub).
    pub fn size(&self) -> (u32, u32) {
        (1, 1)
    }

    /// Returns the display scale factor (always 1.0 in the stub).
    pub fn scale_factor(&self) -> f64 {
        1.0
    }

    /// Requests a redraw (no-op in the stub).
    pub fn request_redraw(&self) {}
}

/// Runs the Android event loop (stub: always unsupported).
///
/// # Errors
///
/// Always returns [`PlatformError::NotImplemented`].
pub fn run<H: EventHandler>(_config: PlatformConfig, _handler: H) -> Result<(), PlatformError> {
    Err(PlatformError::NotImplemented(
        "android event loop".to_owned(),
    ))
}
/// Audio output backend (Android stub).
///
/// The types exist so [`crate::audio`] compiles identically on every target,
/// but no stream can be opened until the Android backend lands.
#[allow(dead_code)]
pub(crate) mod audio {
    use api::media::AudioSpec;

    use crate::PlatformError;

    /// Immutable stream-description placeholder.
    pub(crate) struct Backend;

    /// Stream-state placeholder.
    pub(crate) struct StreamState;

    /// Always fails: Android audio output is not implemented yet.
    pub(crate) fn open(_spec: AudioSpec) -> Result<(Backend, StreamState), PlatformError> {
        Err(PlatformError::NotImplemented(
            "android audio output".to_owned(),
        ))
    }

    /// Always fails: Android audio output is not implemented yet.
    pub(crate) fn write(
        _dev: &Backend,
        _state: &mut StreamState,
        _pcm: &[f32],
    ) -> Result<(), PlatformError> {
        Err(PlatformError::NotImplemented(
            "android audio output".to_owned(),
        ))
    }

    /// No-op in the stub.
    pub(crate) fn stop(_dev: &Backend, _state: &mut StreamState) {}

    /// Always `false` in the stub.
    pub(crate) fn is_running(_state: &StreamState) -> bool {
        false
    }
}
