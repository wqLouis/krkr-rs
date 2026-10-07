//! Cross-platform window helpers.

use raw_window_handle::{RawWindowHandle, WebWindowHandle};

/// Returns a synthetic window handle for **explicitly requested** headless
/// surface creation.
///
/// The null render backend accepts and ignores any handle, so an offscreen
/// frontend needs no display or native window. The returned value describes a
/// zero-sized Web canvas, a variant that exists on every target in
/// `raw-window-handle` 0.6.
///
/// Call this only when the caller has explicitly asked for a headless host and
/// the backend is the null backend; a real backend with no handle must be a
/// hard error rather than silently binding to this fabricated canvas.
pub fn headless_window_handle() -> RawWindowHandle {
    RawWindowHandle::Web(WebWindowHandle::new(0))
}
