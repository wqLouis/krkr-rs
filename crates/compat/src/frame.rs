//! The ordered per-frame poll body.
//!
//! L1 drives the tick; L4 runs the frame body. The order is fixed:
//!
//! ```text
//! drain the onTimer posts L2 already fired
//!            → sound_poll → video_poll → async_trigger_poll
//!            → continuous_handler_poll → idle compaction → frame_tail
//!            → L2 projects the scene → L1 RenderPipeline::render
//! ```
//!
//! L2 advances its timer registry and frame clock before this body runs, so the
//! body only delivers what that advance posted. This module defines the
//! L3-side seam ([`FrameBody`]) and the single function that enforces the order
//! ([`run_frame_body`]); the natives implement the individual polls.

use std::time::{Duration, Instant};

use crate::{CompatCtx, CompatError};

/// The ordered polls of one VM frame body.
///
/// Every method defaults to a no-op so a host can implement only the polls
/// its natives need; the order is enforced by [`run_frame_body`], never by
/// the implementor. The timer posts are drained by [`run_frame_body`] itself,
/// because L2 already advanced the registry before the body runs.
///
/// Each poll has a matching `*_completed` hook that [`run_frame_body`] calls
/// with the poll's wall-clock duration. A host that wants per-frame cost
/// attribution overrides the hooks it cares about; the default discards the
/// measurement. A poll that returns an error does not report a duration.
///
/// Every poll reports its failure as a [`CompatError`](crate::CompatError);
/// the frame body stops at the first error and the remaining polls do not
/// run. A poll never unwinds into the VM: a script handler that raises is
/// caught by the native that dispatched it and logged.
pub trait FrameBody {
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

    /// The tail of the body: movie playback and layer transitions advance
    /// here.
    ///
    /// L2 projects the scene and L1 renders it *after* this hook returns; L3
    /// owns neither, so this step only advances the per-frame playback the
    /// projection reads. [`run_frame_body`] remains the one place the
    /// documented order is enforced.
    fn frame_tail(&mut self, _ctx: &mut CompatCtx) -> Result<(), CompatError> {
        Ok(())
    }

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

    /// Wall-clock duration of [`frame_tail`](Self::frame_tail).
    fn frame_tail_completed(&mut self, _elapsed: Duration) {}

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
///
/// # Errors
///
/// Returns the first error a poll reports. The remaining polls do not run
/// after it, and the frame's remaining work is skipped.
pub fn run_frame_body(body: &mut dyn FrameBody, ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // Request the 1500 ms TJS object rehash before any script poll. An object
    // that grew past its initial bucket count rebuilds its member table on
    // the next property access, so the frame's handlers walk the rebuilt
    // table instead of the old chain.
    crate::natives::system::rehash_poll(ctx);

    // L2 advanced the timer registry before the hook ran, and the registry's
    // callbacks only post, so the frame's `onTimer` events are complete now.
    // Deliver them before any other poll can disable or reschedule a timer.
    crate::natives::visual::timer_posts_poll(ctx)?;

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
    body.frame_tail(ctx)?;
    body.frame_tail_completed(started.elapsed());

    Ok(())
}
