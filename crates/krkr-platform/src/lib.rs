//! krkr-rs **L1 platform core**.
//!
//! Owns the window/surface, the event loop, input, the clock, threads and
//! the process lifecycle — i.e. *all* platform-specific code — behind one
//! platform abstraction. Higher layers (L2 engine, L3 compatibility, L4 VM)
//! consume this and contain no `#[cfg(target_os = ...)]`.
//!
//! # Compile-time platform selection (D13)
//!
//! The platform is chosen at **compile time**; the other platform's code is
//! not compiled. Exactly one feature must be active:
//!
//! * `platform-linux` — the real winit 0.30 backend (the default);
//! * `platform-android` — a stub whose [`run`] returns
//!   [`PlatformError::Unsupported`] until the Android backend lands.
//!
//! Enabling both or neither is a compile error.
//!
//! # Driving the loop
//!
//! A higher layer implements [`EventHandler`] and calls [`run`] with a
//! [`PlatformConfig`]. The backend creates the window, feeds
//! [`PlatformEvent`]s to the handler, and calls `EventHandler::frame` on the
//! cadence of the internal [`Ticker`]. The handler owns all engine state; L1
//! owns the loop. See `docs/migration/02-target-architecture.md` §2.1, §3, §4.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod clock;
mod event;
mod input;
mod window;

#[cfg(all(feature = "platform-linux", feature = "platform-android"))]
compile_error!(
    "features `platform-linux` and `platform-android` are mutually exclusive; enable exactly one"
);
#[cfg(not(any(feature = "platform-linux", feature = "platform-android")))]
compile_error!("enable exactly one platform feature: `platform-linux` or `platform-android`");

#[cfg(feature = "platform-android")]
mod android;
#[cfg(feature = "platform-linux")]
mod linux;

#[cfg(feature = "platform-android")]
pub use crate::android::{PlatformWindow, run};
#[cfg(feature = "platform-linux")]
pub use crate::linux::{PlatformWindow, run};

pub use crate::clock::{Clock, Ticker};
pub use crate::event::PlatformEvent;
pub use crate::input::{InputEvent, MouseButton};
pub use crate::window::headless_window_handle;

use std::time::Duration;

/// Errors from the platform layer.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// The event loop could not be created or failed while running.
    #[error("event loop: {0}")]
    EventLoop(String),
    /// A window could not be created.
    #[error("window: {0}")]
    Window(String),
    /// The platform backend is not compiled in.
    #[error("platform backend not compiled in")]
    Unsupported,
}

/// Immutable configuration for [`run`].
#[derive(Clone, Debug)]
pub struct PlatformConfig {
    /// Window title.
    pub title: String,
    /// Initial window/surface size in pixels.
    pub size: (u32, u32),
    /// Target interval between frames.
    pub frame_interval: Duration,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        Self {
            title: "krkr-rs".to_owned(),
            size: (1280, 720),
            frame_interval: Duration::from_millis(16),
        }
    }
}

/// Whether the [`EventHandler`] wants the loop to continue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Flow {
    /// Keep running.
    #[default]
    Continue,
    /// Stop the event loop after this callback.
    Exit,
}

/// The higher layer's receiver of platform events.
///
/// The platform loop owns the window and the ticker; the handler owns the
/// engine and returns [`Flow::Exit`] to stop the loop (for example after a
/// fatal frame error). Implementations must not block for long: they run on
/// the single loop thread.
pub trait EventHandler {
    /// Called once the window (and its display connection) exists.
    ///
    /// On platforms with a suspend/resume lifecycle this may be called again
    /// after a resume; the handler must be idempotent.
    fn resumed(&mut self, window: &PlatformWindow) -> Flow;

    /// Called for every window/lifecycle event.
    fn event(&mut self, event: PlatformEvent) -> Flow;

    /// Called once per due frame with the time since the previous frame.
    fn frame(&mut self, dt_ms: f32) -> Flow;

    /// Called when the loop is about to wait for events.
    fn about_to_wait(&mut self) -> Flow {
        Flow::Continue
    }
}
