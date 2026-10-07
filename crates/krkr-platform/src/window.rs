//! Cross-platform window helpers.

use raw_window_handle::{RawWindowHandle, WebWindowHandle};

/// Returns a synthetic window handle for headless surface creation.
///
/// The null render backend accepts and ignores any handle, so a headless
/// frontend needs no display or native window. The returned value describes a
/// zero-sized Web canvas, a variant that exists on every target in
/// `raw-window-handle` 0.6.
pub fn headless_window_handle() -> RawWindowHandle {
    RawWindowHandle::Web(WebWindowHandle::new(0))
}
