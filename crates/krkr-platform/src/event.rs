//! Platform events delivered to higher layers.
//!
//! The L1 backend normalizes native window/lifecycle events into
//! [`PlatformEvent`]; a higher layer receives them through
//! [`crate::EventHandler`]. The per-frame tick is not an event: the loop
//! calls `EventHandler::frame` when the [`crate::Ticker`] says a frame is
//! due.

use crate::input::InputEvent;

/// A normalized platform window/lifecycle event.
#[derive(Clone, Debug, PartialEq)]
pub enum PlatformEvent {
    /// The window was resized to the given physical size in pixels.
    Resized(u32, u32),
    /// The window's scale factor (DPI) changed.
    ScaleFactorChanged(f64),
    /// The user asked to close the window.
    CloseRequested,
    /// The window gained (`true`) or lost (`false`) focus.
    Focused(bool),
    /// A raw input event.
    Input(InputEvent),
}
