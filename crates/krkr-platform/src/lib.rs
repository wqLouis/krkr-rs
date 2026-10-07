//! krkr-rs **L1 platform core**.
//!
//! Owns the window/surface, the event loop, input, the clock, threads and
//! the process lifecycle — i.e. *all* platform-specific code — behind one
//! platform abstraction. Higher layers (L2 engine, L3 compatibility, L4 VM)
//! consume this and contain no `#[cfg(target_os = ...)]`.
//!
//! The platform is selected at **compile time** (`platform-linux` |
//! `platform-android`, D13); the other platform's code is not compiled.
//!
//! See `docs/migration/02-target-architecture.md` §2.1, §3, §4.

/// Errors from the platform layer.
#[derive(Debug, thiserror::Error)]
pub enum PlatformError {
    /// The event loop could not be created.
    #[error("event loop: {0}")]
    EventLoop(String),
    /// A window could not be created.
    #[error("window: {0}")]
    Window(String),
    /// The platform backend is not compiled in.
    #[error("platform backend not compiled in")]
    Unsupported,
}
