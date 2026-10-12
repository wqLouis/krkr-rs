//! Headless render helpers shared by the `render` integration tests.

use raw_window_handle::{RawWindowHandle, XlibWindowHandle};

/// Builds a headless raw window handle for the null backend.
///
/// The null pipeline ignores the handle's contents, so a zeroed Xlib window
/// identifier exercises every public entry point without a display server.
pub fn headless_window() -> RawWindowHandle {
    RawWindowHandle::Xlib(XlibWindowHandle::new(0))
}
