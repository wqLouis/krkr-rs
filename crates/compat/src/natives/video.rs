//! `VideoOverlay` native class.
//!
//! Behavioral reference: `main`'s `crates/tvp-natives/src/video_overlay.rs`.
//! Composes the L1 media engine (video decode). Not implemented yet.

use crate::{CompatCtx, CompatError};

/// Register the `VideoOverlay` class.
pub fn register(_ctx: &mut CompatCtx) -> Result<(), CompatError> {
    Ok(())
}
