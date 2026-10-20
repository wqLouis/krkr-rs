//! L2 video-overlay playback state.
//!
//! Models the TVP `VideoOverlay` playback machine (open/play/stop/pause/seek,
//! position/duration, visible, loop, segment loops, overlay binding) as
//! explicit L2 state. It consumes frames and positions supplied by the
//! caller — L1 `media` owns decoding — and never opens a codec itself.
//!
//! Behavioral reference: `main`'s `crates/tvp-natives/src/video_overlay.rs`
//! (the `VideoOverlayInst` state machine) and the reference
//! `tTJSNI_VideoOverlay` (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp`).
//!
//! # What this module is
//!
//! [`VideoOverlay`] is a plain, caller-owned value: there is no process
//! global, no active-instance registry and no clock. The application's update
//! loop calls [`VideoOverlay::advance`] with the elapsed milliseconds each
//! frame; the caller asks L1 `media` for the frame at
//! [`VideoOverlay::position_ms`] and hands it back with
//! [`VideoOverlay::present_frame`]. Nothing here links `media`, FFmpeg or any
//! other decoder.
//!
//! # The clock
//!
//! The reference stores an absolute `play_anchor_ms` and derives the position
//! from the engine tick. To stay free of a global clock, this model advances
//! a relative position by `dt * play_rate`: the two are equivalent for a
//! monotonically increasing clock, and a relative step is directly testable.
//!
//! The position is kept with sub-millisecond resolution
//! ([`VideoOverlay::position_exact_ms`]) and never rounded per step: a
//! step of `1000/60` ms is `16.666…` ms, and rounding it to `17` ms would
//! make the clock run two percent fast and skip a frame's worth of audio
//! on every step. [`VideoOverlay::position_ms`] rounds the accumulated
//! value for the script surface.
//!
//! # Events (explicit state)
//!
//! Play/pause/stop, loop wraps, period events and stream completion are
//! reported as [`VideoEvent`]s queued on the overlay and drained by
//! [`VideoOverlay::poll`]. L3 dispatches them to the script `on*` members;
//! this layer stops at the event list so it is testable without a VM.
//!
//! # Errors
//!
//! An unusable [`VideoSpec`] (non-positive/NaN `fps`, missing frame count),
//! a playback command on a closed overlay and an inconsistent decoded frame
//! are typed [`VideoError`]s instead of silent no-ops. Script-visible values
//! are not validated here: the reference stores whatever it is given.

use std::sync::Arc;

use api::media::{RgbaFrame, VideoSpec};

use crate::layer_ops::{BlitOptions, BlitSource, blit_copy_clipped, blit_over, clip_rect};
use crate::scene::{BitmapState, Scene};

/// Playback status (`tTVPStatus` for the video overlay).
///
/// `Pause` is a distinct status here (unlike the audio `Status`), matching
/// the reference `ssPlay`/`ssStop`/`ssPause`/`ssUnload`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum PlayStatus {
    /// No media is open.
    #[default]
    Unload,
    /// Media is open but not playing.
    Stop,
    /// Playback is running.
    Play,
    /// Playback is frozen at the current position.
    Pause,
}

impl PlayStatus {
    /// The reference status string (`onStatusChanged` argument).
    pub const fn as_str(self) -> &'static str {
        match self {
            PlayStatus::Unload => "unload",
            PlayStatus::Stop => "stop",
            PlayStatus::Play => "play",
            PlayStatus::Pause => "pause",
        }
    }
}

/// Why an `onPeriod` event fired (`perLoop`/`perPeriod`/`perPrepare`/
/// `perSegLoop` in `VideoOvlIntf.cpp`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PeriodReason {
    /// A whole-stream loop wrapped (`perLoop`).
    Loop,
    /// The configured `setPeriodEvent` frame was reached (`perPeriod`).
    Period,
    /// `prepare()` was called (`perPrepare`).
    Prepare,
    /// A configured segment loop wrapped (`perSegLoop`).
    SegmentLoop,
}

impl PeriodReason {
    /// The TVP integer code the script `onPeriod(reason)` receives.
    pub const fn code(self) -> i64 {
        match self {
            PeriodReason::Loop => 0,
            PeriodReason::Period => 1,
            PeriodReason::Prepare => 2,
            PeriodReason::SegmentLoop => 3,
        }
    }
}

/// An event produced by the video-overlay state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum VideoEvent {
    /// The script-visible status changed.
    StatusChanged(PlayStatus),
    /// An `onPeriod(reason)` event fired.
    Period(PeriodReason),
    /// Playback crossed into a new integer frame while a frame was presented.
    FrameUpdate(i64),
    /// A non-looping stream finished, or a segment loop wrapped and its
    /// completion callback should run.
    TransitionComplete,
}

/// The overlay rectangle in window coordinates (reference `tTVPRect`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct VideoBounds {
    /// Left edge in window coordinates.
    pub left: i32,
    /// Top edge in window coordinates.
    pub top: i32,
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
}

impl VideoBounds {
    /// Build a rectangle from its components.
    pub const fn new(left: i32, top: i32, width: u32, height: u32) -> Self {
        Self {
            left,
            top,
            width,
            height,
        }
    }
}

/// How the overlay is bound into the scene (reference `layer1`/`layer2` and
/// `setMixingLayer`).
///
/// The binding is plain data: L3 resolves the script `Layer`/`Window` objects
/// to engine ids, and the render side later consumes the ids. `alpha` is the
/// mixing-layer opacity in `0.0..=1.0` (reference `opacity / 255`).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub struct OverlayBinding {
    /// Layer id the video is drawn on, if any (`layer1`).
    pub layer: Option<u32>,
    /// Window id that owns the overlay, if any.
    pub window: Option<u32>,
    /// Mixing-layer opacity in `0.0..=1.0`.
    pub alpha: f32,
    /// Whether the bound layer mirrors the overlay's `visible` flag.
    pub visible: bool,
}

impl Default for OverlayBinding {
    fn default() -> Self {
        Self {
            layer: None,
            window: None,
            alpha: 1.0,
            visible: true,
        }
    }
}

/// Errors produced by the video-overlay state machine.
#[derive(Debug, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum VideoError {
    /// The supplied [`VideoSpec`] cannot drive the playback machine.
    #[error("invalid video spec: {message}")]
    InvalidSpec {
        /// Human-readable description of the problem.
        message: String,
    },
    /// A playback command was issued before a stream was opened.
    #[error("video overlay has no open stream")]
    NotOpen,
    /// A decoded frame's buffer length disagreed with its dimensions.
    #[error("inconsistent decoded frame: {width}x{height} needs {expected} bytes, got {len}")]
    InconsistentFrame {
        /// Frame width in pixels.
        width: u32,
        /// Frame height in pixels.
        height: u32,
        /// Expected `width * height * 4` byte count.
        expected: usize,
        /// Actual buffer length.
        len: usize,
    },
}

/// Frame-to-milliseconds conversion at `fps` (reference `frame_to_ms`).
fn frame_to_ms(fps: f64, frame: i64) -> i64 {
    if fps > 0.0 {
        (frame as f64 * 1000.0 / fps).round() as i64
    } else {
        0
    }
}

/// Milliseconds-to-frame conversion at `fps` (reference `ms_to_frame`).
fn ms_to_frame(fps: f64, ms: i64) -> i64 {
    if fps > 0.0 {
        (ms.max(0) as f64 * fps / 1000.0) as i64
    } else {
        0
    }
}

/// A caller-owned TVP `VideoOverlay` playback machine.
///
/// See the [module docs](self) for the clock and event model.
#[derive(Debug)]
#[non_exhaustive]
pub struct VideoOverlay {
    spec: Option<VideoSpec>,
    status: PlayStatus,
    playing: bool,
    /// The playback position in milliseconds, with sub-millisecond
    /// resolution. The script-visible position is this value rounded.
    position_exact_ms: f64,
    /// The playback-rate multiplier (`1.0` is native speed). It stores any
    /// value it is given.
    pub play_rate: f64,
    /// Whether whole-stream looping is enabled.
    pub looping: bool,
    segment_start: i64,
    segment_end: i64,
    period_event_frame: i64,
    period_past: bool,
    preparing: bool,
    visible: bool,
    /// The overlay rectangle in window coordinates.
    pub bounds: VideoBounds,
    binding: OverlayBinding,
    /// The frame that is on screen. The frame is shared: the decoder hands
    /// the same buffer to every holder, so a presented frame is not copied
    /// again.
    frame: Option<Arc<RgbaFrame>>,
    last_frame_update: i64,
    /// Whether the last [`Self::advance`] put the clock back to a loop
    /// boundary.
    restarted_at_loop: bool,
    pending: Vec<VideoEvent>,
}

impl Default for VideoOverlay {
    fn default() -> Self {
        Self {
            spec: None,
            status: PlayStatus::Unload,
            playing: false,
            position_exact_ms: 0.0,
            play_rate: 1.0,
            looping: false,
            segment_start: -1,
            segment_end: -1,
            period_event_frame: -1,
            period_past: false,
            preparing: false,
            visible: false,
            bounds: VideoBounds::new(0, 0, 320, 240),
            binding: OverlayBinding {
                visible: false,
                ..OverlayBinding::default()
            },
            frame: None,
            last_frame_update: -1,
            restarted_at_loop: false,
            pending: Vec::new(),
        }
    }
}

impl VideoOverlay {
    /// A fresh, unloaded overlay.
    pub fn new() -> Self {
        Self::default()
    }

    // -- open / close ------------------------------------------------------

    /// Open a stream from its decoded [`VideoSpec`] and enter the `stop`
    /// state. Playback state (position, period event, segment loop, current
    /// frame) is reset; the rectangle and binding are retained, mirroring the
    /// reference `apply_open`.
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::InvalidSpec`] when `fps` is not finite and
    /// positive, or when the total frame count is absent or zero (the
    /// playback machine needs a finite end).
    pub fn open(&mut self, spec: VideoSpec) -> Result<(), VideoError> {
        if !spec.fps.is_finite() || spec.fps <= 0.0 {
            return Err(VideoError::InvalidSpec {
                message: format!("fps must be finite and > 0, got {}", spec.fps),
            });
        }
        match spec.total_frames {
            Some(0) => {
                return Err(VideoError::InvalidSpec {
                    message: "total frame count must be > 0".to_string(),
                });
            }
            None => {
                return Err(VideoError::InvalidSpec {
                    message: "total frame count is required".to_string(),
                });
            }
            Some(_) => {}
        }
        let was_open = self.spec.is_some();
        self.spec = Some(spec);
        self.playing = false;
        self.position_exact_ms = 0.0;
        self.play_rate = 0.0;
        self.preparing = false;
        self.period_past = false;
        self.segment_start = -1;
        self.segment_end = -1;
        self.period_event_frame = -1;
        self.frame = None;
        self.rearm_frame_update();
        // The reference `Open` closes any previous backend (firing `unload`)
        // and then enters the `stop` state.
        if was_open {
            self.set_status(PlayStatus::Unload);
        }
        self.set_status(PlayStatus::Stop);
        Ok(())
    }

    /// Drop the media and return to `unload`, like the reference
    /// `apply_close`. The rectangle and binding are retained.
    pub fn close(&mut self) {
        self.spec = None;
        self.playing = false;
        self.position_exact_ms = 0.0;
        self.preparing = false;
        self.frame = None;
        self.rearm_frame_update();
        self.set_status(PlayStatus::Unload);
    }

    /// Whether a stream is open.
    pub fn is_open(&self) -> bool {
        self.spec.is_some()
    }

    /// The decoded stream description, when open.
    pub fn spec(&self) -> Option<VideoSpec> {
        self.spec
    }

    // -- status ------------------------------------------------------------

    /// The script-visible status.
    pub fn status(&self) -> PlayStatus {
        self.status
    }

    /// Whether the clock is currently running.
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    // -- playback ----------------------------------------------------------

    /// Start (or resume) playback, firing `onStatusChanged("play")` only on a
    /// real status change. A paused backend resumes at normal speed
    /// (reference `vo_play`).
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::NotOpen`] when no stream is open.
    pub fn play(&mut self) -> Result<(), VideoError> {
        self.require_open()?;
        if self.play_rate == 0.0 {
            self.play_rate = 1.0;
        }
        self.playing = true;
        self.set_status(PlayStatus::Play);
        Ok(())
    }

    /// Freeze playback at the current position and enter the `pause` state
    /// (reference `vo_pause`). A paused backend reports a playback rate of
    /// `0`.
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::NotOpen`] when no stream is open.
    pub fn pause(&mut self) -> Result<(), VideoError> {
        self.require_open()?;
        self.playing = false;
        self.play_rate = 0.0;
        self.set_status(PlayStatus::Pause);
        Ok(())
    }

    /// Stop playback, rewind to frame 0 and enter the `stop` state
    /// (reference `vo_stop`).
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::NotOpen`] when no stream is open.
    pub fn stop(&mut self) -> Result<(), VideoError> {
        self.require_open()?;
        self.playing = false;
        self.position_exact_ms = 0.0;
        self.play_rate = 0.0;
        self.set_status(PlayStatus::Stop);
        Ok(())
    }

    /// Rewind to frame 0 without changing the playing/paused state, re-arming
    /// a period event that had already passed (reference `vo_rewind`).
    pub fn rewind(&mut self) {
        self.position_exact_ms = 0.0;
        if self.period_event_frame >= 0 && self.period_past {
            self.period_past = false;
        }
        self.rearm_frame_update();
    }

    /// Enter prepare mode: pause, rewind and play; the `perPrepare`
    /// `onPeriod` event fires on the first decoded frame (reference
    /// `vo_prepare`).
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::NotOpen`] when no stream is open.
    pub fn prepare(&mut self) -> Result<(), VideoError> {
        self.require_open()?;
        self.pause()?;
        self.rewind();
        self.preparing = true;
        self.play()?;
        Ok(())
    }

    // -- position / duration ----------------------------------------------

    /// The current playback position in milliseconds, rounded to the nearest
    /// millisecond.
    pub fn position_ms(&self) -> i64 {
        self.position_exact_ms.round() as i64
    }

    /// The current playback position in milliseconds, with sub-millisecond
    /// resolution.
    ///
    /// This is the clock the picture and the sound are compared against; it
    /// is never rounded per step, so a step of `1000/60` ms accumulates to
    /// exactly the elapsed time.
    pub fn position_exact_ms(&self) -> f64 {
        self.position_exact_ms
    }

    /// The current integer frame index.
    pub fn frame(&self) -> i64 {
        ms_to_frame(self.fps(), self.position_ms())
    }

    /// Put the clock on an absolute position without a seek.
    ///
    /// The position moves, but no seek and no period event is produced: the
    /// picture follows the sound, and the sound's clock moves on its own.
    ///
    /// The value is clamped at `>= 0`.
    pub fn sync_clock(&mut self, milliseconds: f64) {
        if !milliseconds.is_finite() {
            return;
        }
        self.position_exact_ms = milliseconds.max(0.0);
    }

    /// Whether the last [`Self::advance`] put the clock back to a loop
    /// boundary.
    ///
    /// A whole-stream loop and a segment loop both restart the clock at
    /// their start frame. A caller that keeps a sound in step with the
    /// picture reads this to place the sound back onto the picture's
    /// position.
    pub fn restarted_at_loop(&self) -> bool {
        self.restarted_at_loop
    }

    /// The stream duration in milliseconds (0 when closed).
    pub fn duration_ms(&self) -> i64 {
        frame_to_ms(self.fps(), self.total_frames())
    }

    /// The total frame count (0 when closed).
    pub fn total_frames(&self) -> i64 {
        self.spec
            .and_then(|s| s.total_frames)
            .map(|n| n as i64)
            .unwrap_or(0)
    }

    /// The stream frame rate in frames per second (0.0 when closed).
    pub fn fps(&self) -> f64 {
        self.spec.map(|s| f64::from(s.fps)).unwrap_or(0.0)
    }

    /// Seek to `milliseconds` (clamped to `>= 0`). The reference `position`
    /// setter does not touch the period event.
    pub fn seek_ms(&mut self, milliseconds: i64) {
        self.position_exact_ms = milliseconds.max(0) as f64;
        self.rearm_frame_update();
    }

    /// Seek to `frame` (clamped to `>= 0`). A period event whose frame is at
    /// or after the new position is re-armed when it had already passed
    /// (reference `frame` setter).
    pub fn seek_frame(&mut self, frame: i64) {
        self.position_exact_ms = frame_to_ms(self.fps(), frame.max(0)) as f64;
        if self.period_event_frame >= frame && self.period_past {
            self.period_past = false;
        }
        self.rearm_frame_update();
    }

    // -- loop / segment loop ----------------------------------------------

    /// The configured segment loop as `(start, end)` frames, when set. The
    /// stored bounds are returned verbatim.
    pub fn segment_loop(&self) -> Option<(i64, i64)> {
        if self.segment_start >= 0 || self.segment_end >= 0 {
            Some((self.segment_start, self.segment_end))
        } else {
            None
        }
    }

    /// Configure a segment loop (`setSegmentLoop`). The reference stores both
    /// frames verbatim, without validating the range; `-1` is a valid value
    /// that cancels the segment.
    pub fn set_segment_loop(&mut self, start: i64, end: i64) {
        self.segment_start = start;
        self.segment_end = end;
    }

    /// Clear the segment loop (`cancelSegmentLoop`).
    pub fn cancel_segment_loop(&mut self) {
        self.segment_start = -1;
        self.segment_end = -1;
    }

    // -- period event ------------------------------------------------------

    /// The configured `setPeriodEvent` frame, when set.
    pub fn period_event(&self) -> Option<i64> {
        (self.period_event_frame >= 0).then_some(self.period_event_frame)
    }

    /// Configure the frame at which `onPeriod(perPeriod)` fires. A frame at
    /// or before the current position is marked as already passed
    /// (`setPeriodEvent`).
    pub fn set_period_event(&mut self, frame: i64) {
        self.period_event_frame = frame;
        self.period_past = frame <= self.frame();
    }

    /// Clear the period event (`cancelPeriodEvent`).
    pub fn cancel_period_event(&mut self) {
        self.set_period_event(-1);
    }

    // -- rectangle / visibility -------------------------------------------

    /// Whether the overlay is visible.
    pub fn visible(&self) -> bool {
        self.visible
    }

    /// Show or hide the overlay (reference `SetVisible`).
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
        self.binding.visible = visible;
    }

    /// Move the overlay rectangle (`setPos`).
    pub fn set_position(&mut self, left: i32, top: i32) {
        self.bounds.left = left;
        self.bounds.top = top;
    }

    /// Resize the overlay rectangle (`setSize`).
    pub fn set_size(&mut self, width: u32, height: u32) {
        self.bounds.width = width;
        self.bounds.height = height;
    }

    // -- overlay binding ---------------------------------------------------

    /// The current layer/window binding.
    pub fn binding(&self) -> OverlayBinding {
        self.binding
    }

    /// Bind the overlay to a layer (reference `setMixingLayer`/`layer1`).
    pub fn bind_layer(&mut self, layer: u32) {
        self.binding.layer = Some(layer);
    }

    /// Bind the overlay to a window.
    pub fn bind_window(&mut self, window: u32) {
        self.binding.window = Some(window);
    }

    /// Clear the layer/window binding (`resetMixingLayer`).
    pub fn unbind(&mut self) {
        self.binding.layer = None;
        self.binding.window = None;
    }

    /// Set the mixing-layer opacity, clamped to `0.0..=1.0`.
    pub fn set_binding_alpha(&mut self, alpha: f32) {
        self.binding.alpha = alpha.clamp(0.0, 1.0);
    }

    // -- decoded frames ----------------------------------------------------

    /// Hand a decoded frame (from L1 `media`) to the overlay, replacing the
    /// current one.
    ///
    /// The frame is shared: the decoder hands out the same buffer to every
    /// holder, so presenting a frame copies no pixels.
    ///
    /// Presenting a frame fires the frame-update event: the event belongs to
    /// a decoded frame, not to the clock.
    ///
    /// A frame also ends prepare mode: the prepare event, the pause and the
    /// rewind belong to a delivered frame, so the prepare ends on the first
    /// frame that arrives.
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::InconsistentFrame`] when `frame.rgba` is not
    /// exactly `width * height * 4` bytes.
    pub fn present_frame(&mut self, frame: Arc<RgbaFrame>) -> Result<(), VideoError> {
        if !frame.is_consistent() {
            return Err(VideoError::InconsistentFrame {
                width: frame.width,
                height: frame.height,
                expected: (u64::from(frame.width) * u64::from(frame.height) * 4) as usize,
                len: frame.rgba.len(),
            });
        }
        self.frame = Some(frame);
        self.emit_frame_update();
        self.complete_prepare();
        Ok(())
    }

    /// End prepare mode now that a frame arrived.
    ///
    /// The period event comes first, then the pause and the rewind, so the
    /// script sees the event before the status change.
    fn complete_prepare(&mut self) {
        if !self.preparing {
            return;
        }
        self.pending.push(VideoEvent::Period(PeriodReason::Prepare));
        self.playing = false;
        self.play_rate = 0.0;
        self.set_status(PlayStatus::Pause);
        self.rewind();
        self.preparing = false;
    }

    /// The most recently presented frame, if any.
    ///
    /// The frame is shared with the decoder, so reading it copies no pixels.
    pub fn current_frame(&self) -> Option<&RgbaFrame> {
        self.frame.as_deref()
    }

    /// Drop the currently presented frame.
    pub fn clear_frame(&mut self) {
        self.frame = None;
    }

    /// The presentation timestamp of the current frame, in milliseconds.
    pub fn current_frame_pts_ms(&self) -> Option<u64> {
        self.frame.as_ref().map(|f| f.pts_ms)
    }

    // -- clock / events ----------------------------------------------------

    /// The events queued since the last [`Self::poll`], without draining.
    pub fn pending_events(&self) -> &[VideoEvent] {
        &self.pending
    }

    /// Drain and return the queued events.
    pub fn poll(&mut self) -> Vec<VideoEvent> {
        std::mem::take(&mut self.pending)
    }

    /// Remove and return the queued status changes, leaving every other event
    /// queued. Callers that must fire `onStatusChanged` synchronously (the
    /// reference posts it with `TVP_EPT_IMMEDIATE`) use this instead of
    /// [`Self::poll`].
    pub fn take_status_events(&mut self) -> Vec<PlayStatus> {
        let mut statuses = Vec::new();
        self.pending.retain(|event| match event {
            VideoEvent::StatusChanged(status) => {
                statuses.push(*status);
                false
            }
            _ => true,
        });
        statuses
    }

    /// Advance the clock by `dt_ms` milliseconds, updating the position and
    /// queuing the resulting events.
    ///
    /// A non-looping stream that crosses its end stops (`stop` status) and
    /// rewinds to frame 0, queueing [`VideoEvent::TransitionComplete`]. A
    /// configured segment loop wraps to its start whenever its end frame is
    /// reached, regardless of [`Self::looping`]. A whole-stream loop wraps to
    /// frame 0 and queues a `perLoop` period event. A configured period event
    /// fires once per arming.
    pub fn advance(&mut self, dt_ms: f64) {
        self.restarted_at_loop = false;
        if !self.playing || !dt_ms.is_finite() || dt_ms < 0.0 {
            return;
        }
        let fps = self.fps();
        let total = self.total_frames();
        if fps <= 0.0 || total <= 0 {
            return;
        }

        // Prepare mode waits for the next delivered frame: the clock stays
        // where it is until the frame arrives, so the prepared picture is
        // the one the stream starts with.
        if self.preparing {
            return;
        }

        let rate = self.play_rate;
        // The step is not rounded: a rounded step accumulates a systematic
        // error (a `1000/60` ms step rounds up by a third of a millisecond,
        // two percent over a second). A step that is not a number advances
        // nothing; an infinite step runs the clock to the end of the stream.
        let step = dt_ms * rate;
        let step = if step.is_nan() { 0.0 } else { step };
        self.position_exact_ms = (self.position_exact_ms + step).max(0.0);

        let frame = ms_to_frame(fps, self.position_ms());

        // A configured segment loops back before the whole-stream end,
        // whether or not whole-stream looping is enabled.
        if self.segment_end > 0 && frame >= self.segment_end {
            let start = if self.segment_start > 0 {
                self.segment_start
            } else {
                0
            };
            self.seek_frame(start);
            self.restarted_at_loop = true;
            self.pending
                .push(VideoEvent::Period(PeriodReason::SegmentLoop));
            return;
        }

        let end_ms = frame_to_ms(fps, total);
        if frame >= total || self.position_ms() >= end_ms {
            if self.looping {
                self.rewind();
                self.restarted_at_loop = true;
                self.pending.push(VideoEvent::Period(PeriodReason::Loop));
            } else {
                // The reference `EC_COMPLETE` handler calls `Stop()`, which
                // rewinds to the start, before reporting the stop status.
                self.position_exact_ms = 0.0;
                self.playing = false;
                self.set_status(PlayStatus::Stop);
                self.pending.push(VideoEvent::TransitionComplete);
            }
            return;
        }

        if self.period_event_frame >= 0 && !self.period_past && frame >= self.period_event_frame {
            self.period_event_frame = -1;
            self.pending.push(VideoEvent::Period(PeriodReason::Period));
        }
    }

    // -- internals ---------------------------------------------------------

    /// Set the status, queueing `StatusChanged` only on a real change
    /// (reference `set_status`).
    fn set_status(&mut self, status: PlayStatus) {
        if self.status != status {
            self.status = status;
            self.pending.push(VideoEvent::StatusChanged(status));
        }
    }

    /// Forget the picture that is on screen, so the next delivered picture
    /// is reported as a new one.
    ///
    /// A transport jump makes the stream decode a new picture, so the frame
    /// update fires again even when the picture index does not change.
    fn rearm_frame_update(&mut self) {
        self.last_frame_update = -1;
    }

    /// Queue a `FrameUpdate` when the presented picture is a new one.
    ///
    /// The event belongs to a picture: it fires once per decoded picture, so
    /// a picture that is already on screen fires nothing. A picture that
    /// arrives while the overlay is not playing fires nothing either: a
    /// stopped or paused stream decodes no picture.
    ///
    /// The event goes in front of the others: a frame update is reported
    /// before the period events of the same update.
    fn emit_frame_update(&mut self) {
        if !self.playing {
            return;
        }
        let frame = self.frame();
        if frame == self.last_frame_update {
            return;
        }
        self.last_frame_update = frame;
        self.pending.insert(0, VideoEvent::FrameUpdate(frame));
    }

    /// Borrow the open spec or fail with [`VideoError::NotOpen`].
    fn require_open(&self) -> Result<&VideoSpec, VideoError> {
        self.spec.as_ref().ok_or(VideoError::NotOpen)
    }
}

/// Copy a decoded RGBA [`RgbaFrame`] into `layer_id`'s MainImage at full
/// opacity. See [`blit_frame_to_layer_opacity`].
pub fn blit_frame_to_layer(
    scene: &mut Scene,
    layer_id: u32,
    frame: &RgbaFrame,
    source_over: bool,
) -> bool {
    blit_frame_to_layer_opacity(scene, layer_id, frame, source_over, 255)
}

/// Copy a decoded RGBA [`RgbaFrame`] into `layer_id`'s MainImage, clipped to
/// the layer's clip rectangle and to the smaller of the layer and frame
/// dimensions.
///
/// `source_over` selects the alpha-compositing copy (a half-width alpha
/// stream) over the straight replacement (an opaque stream); `opacity` is the
/// compositing opacity applied in that path (reference `SetMixingBitmap(dest,
/// alpha)`). The layer must already own a MainImage; `false` is returned when
/// it has none or the frame has no pixels. This is the shared primitive behind
/// the movie layer copy and the layer-mode video overlay composite.
pub fn blit_frame_to_layer_opacity(
    scene: &mut Scene,
    layer_id: u32,
    frame: &RgbaFrame,
    source_over: bool,
    opacity: u8,
) -> bool {
    if frame.width == 0 || frame.height == 0 {
        return false;
    }
    let Some(dst_bitmap_id) = scene.layer(layer_id).and_then(|layer| layer.bitmap) else {
        return false;
    };
    let clip = clip_rect(scene.layer(layer_id).and_then(|layer| layer.clip));
    let (layer_w, layer_h) = scene
        .layer(layer_id)
        .map(|layer| (layer.rect.w, layer.rect.h))
        .unwrap_or((0, 0));
    let sw = layer_w.min(frame.width);
    let sh = layer_h.min(frame.height);
    if sw == 0 || sh == 0 {
        return false;
    }
    let source = BitmapState::new(frame.width, frame.height, frame.rgba.clone());
    let src_rect = (0, 0, sw as i32, sh as i32);
    let dx = clip.0.max(0);
    let dy = clip.1.max(0);
    let Some(dst) = scene.bitmap_mut(dst_bitmap_id) else {
        return false;
    };
    if source_over {
        blit_over(
            dst,
            dx,
            dy,
            BlitSource {
                bitmap: &source,
                rect: src_rect,
            },
            clip,
            BlitOptions {
                mode: 0,
                opa: opacity,
                treat_as_opaque: false,
                hold_alpha: false,
            },
        );
    } else {
        blit_copy_clipped(dst, dx, dy, &source, src_rect, clip);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The play-rate field stores any value it is given; the reference never
    /// validates it (`reference/cpp/core/movie/ffmpeg/KRMoviePlayer.cpp:78`).
    #[test]
    fn play_rate_stores_any_value() {
        let mut overlay = VideoOverlay::new();
        overlay.play_rate = 0.0;
        assert_eq!(overlay.play_rate, 0.0);
        overlay.play_rate = -2.0;
        assert_eq!(overlay.play_rate, -2.0);
        overlay.play_rate = 2.0;
        assert_eq!(overlay.play_rate, 2.0);
    }
}
