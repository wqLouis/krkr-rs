//! The ordered per-frame poll body.
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
//! function that enforces the order ([`run_frame_body`]); the natives
//! implement the individual polls.

use std::time::{Duration, Instant};

use crate::{CompatCtx, CompatError};

/// The five ordered polls of one VM frame body.
///
/// Every method defaults to a no-op so a host can implement only the polls
/// its natives need; the order is enforced by [`run_frame_body`], never by
/// the implementor.
///
/// Each poll has a matching `*_completed` hook that [`run_frame_body`] calls
/// with the poll's wall-clock duration. A host that wants per-frame cost
/// attribution overrides the hooks it cares about; the default discards the
/// measurement. A poll that returns an error does not report a duration.
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

    /// The tail of the body: L2 projects the scene and L1 renders it.
    ///
    /// It is a single step here because L3 owns neither the projection (L2)
    /// nor the render pipeline (L1); the host delegates both from
    /// this hook so [`run_frame_body`] remains the one place the documented
    /// order is enforced.
    fn project_and_render(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }

    /// Wall-clock duration of [`timer_poll`](Self::timer_poll).
    fn timer_completed(&mut self, _elapsed: Duration) {}

    /// Wall-clock duration of [`sound_poll`](Self::sound_poll).
    fn sound_completed(&mut self, _elapsed: Duration) {}

    /// Wall-clock duration of [`video_poll`](Self::video_poll).
    fn video_completed(&mut self, _elapsed: Duration) {}

    /// Wall-clock duration of
    /// [`async_trigger_poll`](Self::async_trigger_poll).
    fn async_trigger_completed(&mut self, _elapsed: Duration) {}

    /// Wall-clock duration of
    /// [`continuous_handler_poll`](Self::continuous_handler_poll).
    fn continuous_handler_completed(&mut self, _elapsed: Duration) {}

    /// Wall-clock duration of
    /// [`project_and_render`](Self::project_and_render).
    fn project_and_render_completed(&mut self, _elapsed: Duration) {}

    /// Wall-clock duration of the event adapter's per-frame setup, reported by
    /// [`crate::EventAdapter`]'s host (`VmHost::tick`) before the events are
    /// drained.
    fn input_begin_completed(&mut self, _elapsed: Duration) {}

    /// Wall-clock duration of adapting and dispatching the frame's platform
    /// events to script, reported by the VM host after the event loop.
    fn input_events_completed(&mut self, _elapsed: Duration) {}

    /// Wall-clock duration of the event adapter's per-frame commit, reported
    /// by the VM host after [`input_events_completed`](Self::input_events_completed).
    fn input_end_completed(&mut self, _elapsed: Duration) {}
}

/// Run the frame body in the documented order, stopping at the first error.
///
/// Each poll's wall-clock duration is reported to the matching `*_completed`
/// hook; the remaining polls do not run after an error.
pub fn run_frame_body(body: &mut dyn FrameBody, ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let started = Instant::now();
    body.timer_poll(ctx)?;
    body.timer_completed(started.elapsed());

    let started = Instant::now();
    body.sound_poll(ctx)?;
    body.sound_completed(started.elapsed());

    let started = Instant::now();
    body.video_poll(ctx)?;
    body.video_completed(started.elapsed());

    let started = Instant::now();
    body.async_trigger_poll(ctx)?;
    body.async_trigger_completed(started.elapsed());

    let started = Instant::now();
    body.continuous_handler_poll(ctx)?;
    body.continuous_handler_completed(started.elapsed());

    // Idle compaction: the reference `tTVPSystemControl::SystemWatchTimerTimer`
    // delivers `TVP_COMPACT_LEVEL_IDLE` from its per-loop idle check
    // (`reference/cpp/core/environ/impl/SystemControl.cpp:174-179`). It runs
    // after the events and before the frame's draw, so the collector never
    // runs inside a native callback.
    crate::natives::system::compact_poll(ctx);

    let started = Instant::now();
    body.project_and_render(ctx)?;
    body.project_and_render_completed(started.elapsed());

    Ok(())
}
