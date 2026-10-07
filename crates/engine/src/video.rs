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
//! # Events (D9: explicit state)
//!
//! Play/pause/stop, loop wraps, period events and stream completion are
//! reported as [`VideoEvent`]s queued on the overlay and drained by
//! [`VideoOverlay::poll`]. L3 dispatches them to the script `on*` members;
//! this layer stops at the event list so it is testable without a VM.
//!
//! # Errors (D11)
//!
//! An unusable [`VideoSpec`] (non-positive/NaN `fps`, missing frame count),
//! a playback command on a closed overlay, an invalid segment range, an
//! invalid play rate and an inconsistent decoded frame are typed
//! [`VideoError`]s instead of silent no-ops.

use api::media::{RgbaFrame, VideoSpec};

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
#[non_exhaustive]
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

impl OverlayBinding {
    /// Create an overlay binding.
    pub fn new(layer: Option<u32>, window: Option<u32>, alpha: f32, visible: bool) -> Self {
        Self {
            layer,
            window,
            alpha,
            visible,
        }
    }
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
    /// A segment loop range was outside `0..total_frames`.
    #[error("invalid segment loop {start}..={end} (total frames {total})")]
    InvalidSegment {
        /// Requested loop start frame.
        start: i64,
        /// Requested loop end frame.
        end: i64,
        /// Known total frame count (0 when the overlay is closed).
        total: i64,
    },
    /// A playback rate was not finite and positive.
    #[error("invalid play rate {rate}")]
    InvalidPlayRate {
        /// The rejected rate.
        rate: f64,
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
    position_ms: i64,
    play_rate: f64,
    looping: bool,
    segment_start: i64,
    segment_end: i64,
    period_event_frame: i64,
    period_fired: bool,
    visible: bool,
    bounds: VideoBounds,
    binding: OverlayBinding,
    frame: Option<RgbaFrame>,
    last_frame_update: i64,
    pending: Vec<VideoEvent>,
}

impl Default for VideoOverlay {
    fn default() -> Self {
        Self {
            spec: None,
            status: PlayStatus::Unload,
            playing: false,
            position_ms: 0,
            play_rate: 1.0,
            looping: false,
            segment_start: -1,
            segment_end: -1,
            period_event_frame: -1,
            period_fired: false,
            visible: false,
            bounds: VideoBounds::default(),
            binding: OverlayBinding {
                visible: false,
                ..OverlayBinding::default()
            },
            frame: None,
            last_frame_update: -1,
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
        self.spec = Some(spec);
        self.playing = false;
        self.position_ms = 0;
        self.period_fired = false;
        self.segment_start = -1;
        self.segment_end = -1;
        self.period_event_frame = -1;
        self.frame = None;
        self.last_frame_update = -1;
        // `apply_open` assigns the status directly rather than firing an
        // `onStatusChanged`.
        self.status = PlayStatus::Stop;
        Ok(())
    }

    /// Drop the media and return to `unload`, like the reference
    /// `apply_close`. The rectangle and binding are retained.
    pub fn close(&mut self) {
        self.spec = None;
        self.playing = false;
        self.position_ms = 0;
        self.period_fired = false;
        self.frame = None;
        self.last_frame_update = -1;
        self.status = PlayStatus::Unload;
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
    /// real status change. A non-looping stream that is already at the end
    /// restarts from frame 0 (reference `vo_play`).
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::NotOpen`] when no stream is open.
    pub fn play(&mut self) -> Result<(), VideoError> {
        self.require_open()?;
        let end_ms = self.duration_ms();
        if !self.looping && self.position_ms >= end_ms {
            self.position_ms = 0;
        }
        self.playing = true;
        self.period_fired = false;
        self.set_status(PlayStatus::Play);
        Ok(())
    }

    /// Freeze playback at the current position and enter the `pause` state
    /// (reference `vo_pause`).
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::NotOpen`] when no stream is open.
    pub fn pause(&mut self) -> Result<(), VideoError> {
        self.require_open()?;
        self.playing = false;
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
        self.position_ms = 0;
        self.period_fired = false;
        self.set_status(PlayStatus::Stop);
        Ok(())
    }

    /// Rewind to frame 0 without changing the playing/paused state
    /// (reference `vo_rewind`).
    pub fn rewind(&mut self) {
        self.position_ms = 0;
        self.period_fired = false;
    }

    /// Fire the `perPrepare` `onPeriod` event (reference `vo_prepare`).
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::NotOpen`] when no stream is open.
    pub fn prepare(&mut self) -> Result<(), VideoError> {
        self.require_open()?;
        self.pending.push(VideoEvent::Period(PeriodReason::Prepare));
        Ok(())
    }

    // -- position / duration ----------------------------------------------

    /// The current playback position in milliseconds.
    pub fn position_ms(&self) -> i64 {
        self.position_ms
    }

    /// The current integer frame index.
    pub fn frame(&self) -> i64 {
        ms_to_frame(self.fps(), self.position_ms)
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

    /// The playback-rate multiplier (`1.0` is native speed).
    pub fn play_rate(&self) -> f64 {
        self.play_rate
    }

    /// Set the playback-rate multiplier.
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::InvalidPlayRate`] when `rate` is not finite and
    /// strictly positive.
    pub fn set_play_rate(&mut self, rate: f64) -> Result<(), VideoError> {
        if !rate.is_finite() || rate <= 0.0 {
            return Err(VideoError::InvalidPlayRate { rate });
        }
        self.play_rate = rate;
        Ok(())
    }

    /// Seek to `milliseconds` (clamped to `>= 0`), re-arming any period
    /// event (reference `position` setter).
    pub fn seek_ms(&mut self, milliseconds: i64) {
        self.position_ms = milliseconds.max(0);
        self.period_fired = false;
    }

    /// Seek to `frame` (clamped to `>= 0`), re-arming any period event
    /// (reference `frame` setter).
    pub fn seek_frame(&mut self, frame: i64) {
        self.position_ms = frame_to_ms(self.fps(), frame.max(0));
        self.period_fired = false;
    }

    // -- loop / segment loop ----------------------------------------------

    /// Whether whole-stream looping is enabled.
    pub fn looping(&self) -> bool {
        self.looping
    }

    /// Enable or disable whole-stream looping.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    /// The configured segment loop as `(start, end)` frames, when set.
    pub fn segment_loop(&self) -> Option<(i64, i64)> {
        if self.segment_start >= 0 || self.segment_end >= 0 {
            Some((self.segment_start.max(0), self.segment_end))
        } else {
            None
        }
    }

    /// Configure a segment loop (`setSegmentLoop`). The loop is only honoured
    /// while [`Self::set_looping`] is also enabled, matching the reference.
    ///
    /// # Errors
    ///
    /// Returns [`VideoError::InvalidSegment`] when `start < 0`, `end < start`,
    /// or (once a stream is open) `end >= total_frames`.
    pub fn set_segment_loop(&mut self, start: i64, end: i64) -> Result<(), VideoError> {
        let total = self.total_frames();
        if start < 0 || end < start || (total > 0 && end >= total) {
            return Err(VideoError::InvalidSegment { start, end, total });
        }
        self.segment_start = start;
        self.segment_end = end;
        self.period_fired = false;
        Ok(())
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

    /// Configure the frame at which `onPeriod(perPeriod)` fires
    /// (`setPeriodEvent`).
    pub fn set_period_event(&mut self, frame: i64) {
        self.period_event_frame = frame;
        self.period_fired = false;
    }

    /// Clear the period event (`cancelPeriodEvent`).
    pub fn cancel_period_event(&mut self) {
        self.period_event_frame = -1;
        self.period_fired = false;
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

    /// The overlay rectangle.
    pub fn bounds(&self) -> VideoBounds {
        self.bounds
    }

    /// Replace the overlay rectangle (`setBounds`).
    pub fn set_bounds(&mut self, bounds: VideoBounds) {
        self.bounds = bounds;
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
    /// # Errors
    ///
    /// Returns [`VideoError::InconsistentFrame`] when `frame.rgba` is not
    /// exactly `width * height * 4` bytes.
    pub fn present_frame(&mut self, frame: RgbaFrame) -> Result<(), VideoError> {
        if !frame.is_consistent() {
            return Err(VideoError::InconsistentFrame {
                width: frame.width,
                height: frame.height,
                expected: (u64::from(frame.width) * u64::from(frame.height) * 4) as usize,
                len: frame.rgba.len(),
            });
        }
        self.frame = Some(frame);
        Ok(())
    }

    /// The most recently presented frame, if any.
    pub fn current_frame(&self) -> Option<&RgbaFrame> {
        self.frame.as_ref()
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

    /// Advance the clock by `dt_ms` milliseconds, updating the position and
    /// queuing the resulting events.
    ///
    /// A non-looping stream that crosses its end stops (`stop` status) and
    /// queues [`VideoEvent::TransitionComplete`]. A looping stream wraps to
    /// the segment start (or frame 0) and queues a `perLoop`/`perSegLoop`
    /// period event; a segment wrap also queues [`VideoEvent::TransitionComplete`].
    /// A configured period event fires once per arming.
    pub fn advance(&mut self, dt_ms: f64) {
        if !self.playing || !dt_ms.is_finite() || dt_ms < 0.0 {
            return;
        }
        let fps = self.fps();
        let total = self.total_frames();
        if fps <= 0.0 || total <= 0 {
            return;
        }
        let rate = self.play_rate;
        self.position_ms = self
            .position_ms
            .saturating_add((dt_ms * rate).round() as i64);
        self.position_ms = self.position_ms.max(0);

        let (start, end) = self.segment_bounds();
        let end_ms = frame_to_ms(fps, end + 1);
        let frame = ms_to_frame(fps, self.position_ms);

        if frame > end || self.position_ms >= end_ms {
            if self.looping {
                self.position_ms = frame_to_ms(fps, start);
                self.period_fired = false;
                let segmented = self.segment_start >= 0 || self.segment_end >= 0;
                self.pending.push(VideoEvent::Period(if segmented {
                    PeriodReason::SegmentLoop
                } else {
                    PeriodReason::Loop
                }));
                if segmented {
                    self.pending.push(VideoEvent::TransitionComplete);
                }
                self.emit_frame_update();
            } else {
                self.position_ms = end_ms;
                self.playing = false;
                self.set_status(PlayStatus::Stop);
                self.pending.push(VideoEvent::TransitionComplete);
            }
            return;
        }

        if self.period_event_frame >= 0 && !self.period_fired && frame >= self.period_event_frame {
            self.period_fired = true;
            self.pending.push(VideoEvent::Period(PeriodReason::Period));
        }
        self.emit_frame_update();
    }

    // -- internals ---------------------------------------------------------

    /// The active `(start, end)` inclusive frame range (reference
    /// `segment_bounds`).
    fn segment_bounds(&self) -> (i64, i64) {
        let start = if self.segment_start >= 0 {
            self.segment_start
        } else {
            0
        };
        let end = if self.segment_end >= start {
            self.segment_end
        } else {
            (self.total_frames() - 1).max(start)
        };
        (start, end)
    }

    /// Set the status, queueing `StatusChanged` only on a real change
    /// (reference `set_status`).
    fn set_status(&mut self, status: PlayStatus) {
        if self.status != status {
            self.status = status;
            self.pending.push(VideoEvent::StatusChanged(status));
        }
    }

    /// Queue a `FrameUpdate` when the frame index moved and a frame has been
    /// presented (the caller-driven analogue of the reference's
    /// `onFrameUpdate` per decoded frame).
    fn emit_frame_update(&mut self) {
        if self.frame.is_none() {
            return;
        }
        let frame = self.frame();
        if frame != self.last_frame_update {
            self.last_frame_update = frame;
            self.pending.push(VideoEvent::FrameUpdate(frame));
        }
    }

    /// Borrow the open spec or fail with [`VideoError::NotOpen`].
    fn require_open(&self) -> Result<&VideoSpec, VideoError> {
        self.spec.as_ref().ok_or(VideoError::NotOpen)
    }
}
