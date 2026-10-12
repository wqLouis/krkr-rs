//! krkr-rs **L1 platform core**.
//!
//! Owns the window/surface, the event loop, input, the clock, audio output,
//! threads and the process lifecycle — i.e. *all* platform-specific code —
//! behind one platform abstraction. Higher layers (L2 engine, L3 compatibility, L4 VM)
//! consume this and contain no `#[cfg(target_os = ...)]`.
//!
//! # Compile-time platform selection
//!
//! The platform is chosen at **compile time**; the other platform's code is
//! not compiled. Exactly one feature must be active and there is no default:
//! the consumer (or CI job) selects it explicitly.
//!
//! * `platform-linux` — the real winit 0.30 backend;
//! * `platform-android` — the real AAudio audio-output backend (via the
//!   `ndk` crate, Android 8.0 / API 26+) plus stubs for the window and event
//!   loop, whose [`run`] returns [`PlatformError::NotImplemented`].
//!
//! Enabling both or neither is a compile error, as is `platform-linux` on an
//! Android target.
//!
//! # `unsafe` and the Android audio backend
//!
//! The crate is `#![deny(unsafe_code)]`, not `forbid`: the `ndk` crate wraps
//! the AAudio C API safely, but its data callback still hands the audio buffer
//! over as a raw `*mut c_void`, so one small private module converts that
//! pointer into a writable slice. That is the only `unsafe` in the crate; it
//! carries a scoped `#[allow(unsafe_code)]` and a `SAFETY:` comment on every
//! block. Every other backend, including `platform-linux`, stays entirely
//! safe.
//!
//! Because there is no default, every consumer must select exactly one
//! feature: `platform = { features = ["platform-linux"] }` for a desktop
//! binary or `features = ["platform-android"]` for an Android frontend. A
//! `cargo build`/`cargo check` of this crate on its own must pass the feature
//! explicitly.
//!
//! # Driving the loop
//!
//! A higher layer implements [`EventHandler`] and calls [`run`] with a
//! [`PlatformConfig`]. The backend creates the window, feeds
//! [`PlatformEvent`]s to the handler, and calls `EventHandler::frame` on the
//! cadence of the internal [`Ticker`]. The handler owns all engine state; L1
//! owns the loop.

#![deny(unsafe_code)]
#![warn(missing_docs)]

mod audio;
mod audio_queue;
mod clock;
mod event;
mod fonts;
mod input;
mod window;

// A feature/target mismatch is the bug this guards against: the winit desktop
// backend must never be compiled for an Android target. The reverse —
// selecting the Android stub on a desktop target — is allowed so the stub can
// be compile-checked anywhere (see the crate docs).
#[cfg(all(feature = "platform-linux", target_os = "android"))]
compile_error!("feature `platform-linux` cannot target Android; enable `platform-android` instead");
#[cfg(all(feature = "platform-linux", feature = "platform-android"))]
compile_error!(
    "features `platform-linux` and `platform-android` are mutually exclusive; enable exactly one"
);
#[cfg(not(any(feature = "platform-linux", feature = "platform-android")))]
compile_error!("enable exactly one platform feature: `platform-linux` or `platform-android`");

// Gate each backend on exactly its own feature so setting both yields the
// single `compile_error!` above rather than an `E0252` duplicate definition.
#[cfg(all(feature = "platform-android", not(feature = "platform-linux")))]
mod android;
#[cfg(all(feature = "platform-linux", not(feature = "platform-android")))]
mod linux;

#[cfg(all(feature = "platform-android", not(feature = "platform-linux")))]
pub use crate::android::{PlatformWindow, run};
#[cfg(all(feature = "platform-linux", not(feature = "platform-android")))]
pub use crate::linux::{PlatformWindow, run};

pub use crate::audio::{AudioDevice, AudioOutputState};
pub use crate::clock::{Clock, Ticker};
pub use crate::event::PlatformEvent;
pub use crate::fonts::{ANDROID_SYSTEM_FONT_DIRS, system_font_dirs};
pub use crate::input::{InputEvent, MouseButton, PhysicalKey};
pub use crate::window::headless_window_handle;

use api::error::AudioError;
use std::time::Duration;

/// Errors from the platform layer.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// The event loop could not be created or failed while running.
    #[error("event loop: {0}")]
    EventLoop(String),
    /// A window could not be created.
    #[error("window: {0}")]
    Window(String),
    /// Audio output failed.
    #[error(transparent)]
    Audio(#[from] AudioError),
    /// The raw window/display handle could not be obtained.
    ///
    /// A higher layer must treat this as fatal rather than substituting a
    /// synthetic handle (no silent fallback).
    #[error("window handle: {0}")]
    Handle(String),
    /// The platform backend is not compiled in.
    #[error("platform backend not compiled in")]
    Unsupported,
    /// The compiled backend does not implement this operation yet.
    #[error("platform backend not implemented: {0}")]
    NotImplemented(String),
}

/// Immutable configuration for [`run`].
///
/// The type is `#[non_exhaustive]`: construct it through [`Default`] and
/// assign the fields you need, so new fields can be added without a breaking
/// change.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct PlatformConfig {
    /// Window title.
    pub title: String,
    /// Initial window size in logical pixels (DPI-scaled by the backend).
    ///
    /// The surface size actually created is reported physically through
    /// [`PlatformEvent::Resized`] and `PlatformWindow::size`.
    pub size: (u32, u32),
    /// Target interval between frames.
    ///
    /// A zero interval is normalized up by [`Ticker::new`] so the loop cannot
    /// busy-spin.
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
#[non_exhaustive]
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
    ///
    /// Returning [`Flow::Exit`] stops the loop. A
    /// [`PlatformEvent::CloseRequested`] always stops the loop, so it cannot
    /// be vetoed.
    fn event(&mut self, event: PlatformEvent) -> Flow;

    /// Called once per due frame with the time since the previous frame.
    ///
    /// `window` is the live platform window. The handler may query its size
    /// and request a new inner size so a script-driven client-area change
    /// reaches the backend; the backend reports the applied size back as
    /// [`PlatformEvent::Resized`].
    fn frame(&mut self, dt_ms: f32, window: &PlatformWindow) -> Flow;

    /// Called when the loop is about to wait for events.
    fn about_to_wait(&mut self) -> Flow {
        Flow::Continue
    }
}
