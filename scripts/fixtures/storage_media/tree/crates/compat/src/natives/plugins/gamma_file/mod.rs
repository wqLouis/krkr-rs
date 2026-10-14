//! Fixture port for the `gamma` domain.
//!
//! # Missing seam
//!
//! The L2 storage namespace has no media-provider seam, so a `gamma://` media
//! cannot be plugged in. `register` is a documented no-op and the missing seam
//! is reported here rather than fabricated.

pub fn register() {}
