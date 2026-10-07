//! Android platform backend (stub).
//!
//! The Android host is not implemented yet; selecting `platform-android`
//! compiles this stub so the higher layers build unchanged. [`run`] fails
//! fast with [`PlatformError::Unsupported`] (D11: no silent fallback).

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
    /// Returns the raw window handle (always `None` in the stub).
    pub fn raw_window_handle(&self) -> Option<RawWindowHandle> {
        None
    }

    /// Returns the raw display handle (always `None` in the stub).
    pub fn raw_display_handle(&self) -> Option<RawDisplayHandle> {
        None
    }

    /// Returns the surface size (always 1×1 in the stub).
    pub fn size(&self) -> (u32, u32) {
        (1, 1)
    }

    /// Requests a redraw (no-op in the stub).
    pub fn request_redraw(&self) {}
}

/// Runs the Android event loop (stub: always unsupported).
///
/// # Errors
///
/// Always returns [`PlatformError::Unsupported`].
pub fn run<H: EventHandler>(_config: PlatformConfig, _handler: H) -> Result<(), PlatformError> {
    Err(PlatformError::Unsupported)
}
