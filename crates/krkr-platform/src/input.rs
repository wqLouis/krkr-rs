//! Raw input events produced by the platform.
//!
//! These values are platform-agnostic: the backend translates native events
//! (winit, Android) into [`InputEvent`] and the higher layers never see a
//! platform type. The L3 compatibility surface maps them onto the TJS key
//! codes and hit-testing; L1 deliberately assigns no engine meaning.

use krkr_api::host::TouchPhase;

/// A mouse button, independent of the platform event type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MouseButton {
    /// The primary (usually left) button.
    Left,
    /// The secondary (usually right) button.
    Right,
    /// The middle button.
    Middle,
    /// The first extra (back) button.
    Back,
    /// The second extra (forward) button.
    Forward,
    /// A button the platform reported as a raw code.
    Other(u16),
}

/// A single raw input event.
///
/// Coordinates are in logical window coordinates. The `code` carried by
/// [`InputEvent::Key`] is an opaque, stable-within-a-build platform physical
/// key identifier; L3 owns the mapping to TJS key codes.
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// The pointer moved to the given position.
    PointerMoved {
        /// Horizontal logical coordinate.
        x: f32,
        /// Vertical logical coordinate.
        y: f32,
    },
    /// The pointer left the window.
    PointerLeft,
    /// A mouse button changed state.
    MouseButton {
        /// Which button changed.
        button: MouseButton,
        /// `true` for press, `false` for release.
        pressed: bool,
    },
    /// A vertical wheel scroll.
    Wheel {
        /// Lines (or pixels) scrolled; positive is down.
        delta: f32,
    },
    /// A physical key changed state.
    Key {
        /// Opaque platform physical-key identifier.
        code: u32,
        /// `true` for press, `false` for release.
        pressed: bool,
    },
    /// Committed text input (IME).
    Text(String),
    /// A touch contact changed phase.
    Touch {
        /// Stable identifier of the contact.
        id: u64,
        /// Current phase of the contact.
        phase: TouchPhase,
        /// Horizontal logical coordinate.
        x: f32,
        /// Vertical logical coordinate.
        y: f32,
    },
}
