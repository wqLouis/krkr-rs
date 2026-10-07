//! The ordered per-frame poll body (doc 08 §6).
//!
//! L1 drives the tick; L4 runs the frame body. The order is fixed:
//!
//! ```text
//! timer_poll → sound_poll → video_poll
//!            → async_trigger_poll → continuous_handler_poll
//!            → L2 projects the scene → L1 RenderPipeline::render
//! ```
//!
//! This module defines the L3-side seam ([`FrameBody`]) and the single
//! function that enforces the order ([`run_frame_body`]); the migrated
//! natives implement the individual polls.

use crate::{CompatCtx, CompatError};

/// The five ordered polls of one VM frame body.
///
/// Every method defaults to a no-op so a host can implement only the polls
/// its natives need; the order is enforced by [`run_frame_body`], never by
/// the implementor.
pub trait FrameBody {
    /// Timers due this frame.
    fn timer_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }

    /// Audio playback progress.
    fn sound_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }

    /// Video playback progress.
    fn video_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }

    /// Async triggers ready to fire.
    fn async_trigger_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }

    /// Continuous/periodic script handlers.
    fn continuous_handler_poll(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }
}

/// Run the frame body in the documented order, stopping at the first error.
pub fn run_frame_body(body: &mut dyn FrameBody, ctx: &mut CompatCtx) -> Result<(), CompatError> {
    body.timer_poll(ctx)?;
    body.sound_poll(ctx)?;
    body.video_poll(ctx)?;
    body.async_trigger_poll(ctx)?;
    body.continuous_handler_poll(ctx)?;
    Ok(())
}
