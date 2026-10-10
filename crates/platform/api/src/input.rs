//! Platform-agnostic input DTOs.
//!
//! These values are the generic input contract between the platform layer
//! (L1) and the middle layer (L2). L1 backends translate native events
//! (winit, Android) into [`InputEvent`]; L2 folds them into explicit state
//! (`engine::input::InputState`); L3 maps them onto the TJS key codes and hit
//! testing. L1 itself assigns no engine meaning.
//!
//! The types live in `api` (not `platform`) because they are pure values, not
//! platform code: the same `InputEvent` describes desktop and Android input
//! and both the L1 backends and the higher layers name it directly.

use crate::host::TouchPhase;

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
/// All coordinates are in **physical window pixels** — the same space as
/// [`crate::host::InputState`] and the platform window size — so a higher
/// layer can hit-test without knowing the display scale factor. The `code`
/// carried by [`InputEvent::Key`] is an opaque, stable-within-a-build platform
/// physical key identifier; L3 owns the mapping to TJS key codes.
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    /// The pointer moved to the given position.
    PointerMoved {
        /// Horizontal physical coordinate.
        x: f32,
        /// Vertical physical coordinate.
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
        /// Physical pixels (or lines) scrolled; positive is down.
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
        /// Horizontal physical coordinate.
        x: f32,
        /// Vertical physical coordinate.
        y: f32,
    },
}
