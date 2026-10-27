//! Integration tests for the L2 video-overlay playback state
//! (`engine::video`).
//!
//! Headless by construction: the overlay never opens a codec. Every test
//! drives the state machine with `advance(dt)` and supplies decoded frames
//! directly, exactly as the L1 `media` decoder would. The reference behaviour
//! is `main`'s `crates/tvp-natives/src/video_overlay.rs`
//! (`VideoOverlayInst`/`advance_playback`) and the reference `VideoOverlay`.

use std::sync::Arc;

use api::media::{RgbaFrame, VideoSpec};
use engine::video::{PeriodReason, PlayStatus, VideoBounds, VideoError, VideoEvent, VideoOverlay};

fn spec(fps: f32, frames: u64) -> VideoSpec {
    VideoSpec {
        width: 640,
        height: 360,
        fps,
        total_frames: Some(frames),
        sample_rate: 0,
        channels: 0,
        ..VideoSpec::default()
    }
}

fn frame(width: u32, height: u32, pts_ms: u64) -> RgbaFrame {
    RgbaFrame::new(
        width,
        height,
        pts_ms,
        vec![0; (width * height * 4) as usize],
    )
}

#[test]
fn open_validates_the_spec_and_reports_metadata() {
    let mut v = VideoOverlay::new();
    assert!(matches!(
        v.open(spec(0.0, 30)),
        Err(VideoError::InvalidSpec { .. })
    ));
    let mut no_frames = spec(30.0, 1);
    no_frames.total_frames = None;
    assert!(matches!(
        v.open(no_frames),
        Err(VideoError::InvalidSpec { .. })
    ));

    v.open(spec(30.0, 30)).expect("open");
    assert_eq!(v.status(), PlayStatus::Stop);
    assert_eq!(v.total_frames(), 30);
    assert_eq!(v.duration_ms(), 1000);
    assert!((v.fps() - 30.0).abs() < 1e-9);

    v.close();
    assert_eq!(v.status(), PlayStatus::Unload);
    assert!(!v.is_open());
}

#[test]
fn play_advance_pause_stop_emits_status_events() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();

    v.play().unwrap();
    assert_eq!(v.status(), PlayStatus::Play);
    assert!(v.is_playing());
    v.advance(500.0);
    assert_eq!(v.position_ms(), 500);
    assert_eq!(v.frame(), 15);

    v.pause().unwrap();
    assert_eq!(v.status(), PlayStatus::Pause);
    v.advance(500.0);
    assert_eq!(v.position_ms(), 500, "paused position must not move");

    v.stop().unwrap();
    assert_eq!(v.status(), PlayStatus::Stop);
    assert_eq!(v.position_ms(), 0);

    let events = v.poll();
    assert!(events.contains(&VideoEvent::StatusChanged(PlayStatus::Play)));
    assert!(events.contains(&VideoEvent::StatusChanged(PlayStatus::Pause)));
    assert!(events.contains(&VideoEvent::StatusChanged(PlayStatus::Stop)));
}

#[test]
fn status_change_is_not_emitted_twice() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();
    v.play().unwrap();
    let _ = v.poll();
    v.play().unwrap();
    assert!(
        v.poll().is_empty(),
        "already playing must not re-fire onStatusChanged"
    );
}

/// A finished non-looping stream stops and rewinds to frame 0, then reports
/// the stop status.
///
/// Reference: `EC_COMPLETE` calls `VideoOverlay->Stop()`, which rewinds to 0,
/// then `SetStatusAsync(ssStop)` (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:529-550`,
/// `reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:209-215`).
#[test]
fn non_looping_stream_finishes_and_fires_transition() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();
    v.play().unwrap();
    v.poll();
    v.advance(5000.0);

    assert_eq!(v.status(), PlayStatus::Stop);
    assert!(!v.is_playing());
    assert_eq!(v.position_ms(), 0);
    let events = v.poll();
    assert!(events.contains(&VideoEvent::StatusChanged(PlayStatus::Stop)));
    assert!(events.contains(&VideoEvent::TransitionComplete));
}

#[test]
fn play_restarts_a_finished_non_looping_stream() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();
    v.play().unwrap();
    v.advance(5000.0);
    v.poll();
    v.play().unwrap();
    assert_eq!(v.position_ms(), 0);
    assert!(v.is_playing());
}

#[test]
fn looping_wraps_to_the_start() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();
    v.looping = true;
    v.play().unwrap();
    v.poll();
    v.advance(1500.0);

    assert_eq!(v.position_ms(), 0);
    assert!(v.is_playing());
    assert_eq!(v.poll(), vec![VideoEvent::Period(PeriodReason::Loop)]);
}

/// A configured segment loop wraps at its end frame even when whole-stream
/// looping is off, without a frame update or a completion event.
///
/// Reference: `EC_UPDATE` wraps when `SegLoopEndFrame > 0` and
/// `curFrame >= SegLoopEndFrame`, then `return`s before the frame update
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:567-575`).
#[test]
fn segment_loop_wraps_inside_the_segment() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 300)).unwrap();
    v.set_segment_loop(10, 19);
    assert_eq!(v.segment_loop(), Some((10, 19)));
    v.play().unwrap();
    v.poll();
    v.advance(700.0); // frame 21 is past the segment end 19

    // frame 10 at 30 fps is ~333 ms
    assert_eq!(v.position_ms(), 333);
    assert_eq!(
        v.poll(),
        vec![VideoEvent::Period(PeriodReason::SegmentLoop)]
    );

    v.cancel_segment_loop();
    assert_eq!(v.segment_loop(), None);
}

/// A period event set at or before the current frame is already passed, is
/// cleared to `-1` when it fires, and is re-armed by a `frame` seek only while
/// still armed.
///
/// Reference: `SetPeriodEvent` sets `IsEventPast = eventFrame <= GetFrame()`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:363-370`); the
/// `EC_UPDATE` handler clears `EventFrame` to `-1` when it fires (`:628-631`);
/// `SetFrame` re-arms only when `EventFrame >= f && IsEventPast` (`:730-738`).
#[test]
fn period_event_semantics_match_the_reference() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 300)).unwrap();
    v.set_period_event(3);
    assert_eq!(v.period_event(), Some(3));
    v.play().unwrap();
    v.poll();

    v.advance(120.0);
    assert_eq!(v.poll(), vec![VideoEvent::Period(PeriodReason::Period)]);
    // Firing clears the event.
    assert_eq!(v.period_event(), None);

    // A seek does not re-arm a fired event.
    v.seek_frame(0);
    v.advance(120.0);
    assert!(v.poll().is_empty(), "a fired period event stays cleared");

    // An event set behind the play head is treated as passed.
    v.seek_frame(10);
    v.set_period_event(5);
    v.advance(200.0);
    assert!(
        !v.poll().contains(&VideoEvent::Period(PeriodReason::Period)),
        "an event set at or before the current frame must not fire"
    );

    // A frame seek re-arms a passed-but-still-armed event.
    v.seek_frame(0);
    v.advance(200.0);
    assert!(v.poll().contains(&VideoEvent::Period(PeriodReason::Period)));

    v.cancel_period_event();
    assert_eq!(v.period_event(), None);
}

/// `prepare` pauses, rewinds and plays; `perPrepare` fires on the first
/// decoded frame, after the caller has shown that picture, and the overlay is
/// then paused at frame 0 again.
///
/// Reference: `Prepare()` does `Pause(); Rewind(); IsPrepare = true; Play()`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:349-355`), and
/// `EC_UPDATE` assigns the front buffer, fires `onFrameUpdate` and then fires
/// `perPrepare` before `Pause(); Rewind(); IsPrepare = false` (`:583-640`).
#[test]
fn prepare_fires_the_prepare_period_after_the_frame_is_shown() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();
    v.present_frame(Arc::new(frame(1, 1, 0))).unwrap();
    v.prepare().unwrap();
    assert!(v.is_playing(), "prepare restarts playback");
    assert_eq!(v.position_ms(), 0);
    let started = v.poll();
    assert!(started.contains(&VideoEvent::StatusChanged(PlayStatus::Pause)));
    assert!(started.contains(&VideoEvent::StatusChanged(PlayStatus::Play)));

    v.advance(100.0);
    assert!(v.poll().is_empty(), "the prepare waits for the next frame");
    v.present_frame(Arc::new(frame(1, 1, 0))).unwrap();
    assert!(
        v.is_playing() && v.current_frame().is_some(),
        "the caller shows the prepared frame before prepare finishes"
    );
    assert_eq!(v.poll(), vec![VideoEvent::FrameUpdate(0)]);

    v.complete_prepare();
    assert_eq!(
        v.poll(),
        vec![
            VideoEvent::Period(PeriodReason::Prepare),
            VideoEvent::StatusChanged(PlayStatus::Pause)
        ]
    );
    assert!(!v.is_playing());
    assert_eq!(v.position_ms(), 0);
}

#[test]
fn seek_clamps_and_rewind_keeps_the_play_state() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();
    v.seek_ms(-5);
    assert_eq!(v.position_ms(), 0);
    v.seek_frame(15);
    assert_eq!(v.frame(), 15);
    assert_eq!(v.position_ms(), 500);

    v.play().unwrap();
    v.advance(100.0);
    v.rewind();
    assert_eq!(v.position_ms(), 0);
    assert!(v.is_playing(), "rewind keeps playing");
}

#[test]
fn present_frame_stores_and_validates_decoded_frames() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();

    let bad = RgbaFrame::new(2, 2, 0, vec![0; 3]);
    assert!(matches!(
        v.present_frame(Arc::new(bad)),
        Err(VideoError::InconsistentFrame { .. })
    ));

    v.present_frame(Arc::new(frame(2, 2, 33))).unwrap();
    assert_eq!(v.current_frame_pts_ms(), Some(33));
    assert_eq!(v.current_frame().unwrap().width, 2);
    v.clear_frame();
    assert!(v.current_frame().is_none());
}

#[test]
fn frame_update_fires_once_a_frame_is_presented() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 300)).unwrap();
    v.play().unwrap();
    v.poll();

    v.advance(100.0); // frame 3, but no decoded frame yet
    assert!(v.poll().is_empty());

    // The event belongs to the presented frame: the reference fires it when
    // a frame has been decoded
    // (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:621`).
    v.present_frame(Arc::new(frame(1, 1, 0))).unwrap();
    assert_eq!(v.poll(), vec![VideoEvent::FrameUpdate(3)]);

    v.advance(100.0); // frame 6
    assert!(
        v.poll().is_empty(),
        "the frame update belongs to a presented frame"
    );
    v.present_frame(Arc::new(frame(1, 1, 0))).unwrap();
    assert_eq!(v.poll(), vec![VideoEvent::FrameUpdate(6)]);

    // The same picture again: the reference fires the event once per decoded
    // picture, so a picture that is already on screen fires nothing.
    v.present_frame(Arc::new(frame(1, 1, 0))).unwrap();
    assert!(v.poll().is_empty(), "the picture on screen did not change");
}

#[test]
fn overlay_binding_and_rectangle() {
    let mut v = VideoOverlay::new();
    v.set_visible(true);
    v.bind_layer(4);
    v.bind_window(2);
    v.set_binding_alpha(2.0); // clamped
    let binding = v.binding();
    assert_eq!(binding.layer, Some(4));
    assert_eq!(binding.window, Some(2));
    assert_eq!(binding.alpha, 1.0);
    assert!(binding.visible);

    v.bounds = VideoBounds::new(1, 2, 3, 4);
    assert_eq!(v.bounds, VideoBounds::new(1, 2, 3, 4));
    v.set_position(9, 8);
    v.set_size(7, 6);
    assert_eq!(v.bounds, VideoBounds::new(9, 8, 7, 6));

    v.set_visible(false);
    assert!(!v.visible());
    v.unbind();
    assert_eq!(v.binding().layer, None);
    assert_eq!(v.binding().window, None);
}

#[test]
fn commands_on_a_closed_overlay_are_typed_errors() {
    let mut v = VideoOverlay::new();
    assert_eq!(v.play(), Err(VideoError::NotOpen));
    assert_eq!(v.pause(), Err(VideoError::NotOpen));
    assert_eq!(v.stop(), Err(VideoError::NotOpen));
    assert_eq!(v.prepare(), Err(VideoError::NotOpen));
}

/// Segment-loop bounds and the play rate are stored verbatim; the reference
/// validates neither.
///
/// Reference: `SetSegmentLoop` stores both frames and calls the backend with
/// no range check (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:357-361`);
/// `SetLoop(false)` itself uses `(-1, -1)` (`:787-789`); `SetPlayRate` is
/// unvalidated (`reference/cpp/core/movie/ffmpeg/KRMoviePlayer.cpp:75-79`).
#[test]
fn segment_loop_and_play_rate_store_what_they_are_given() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();

    v.set_segment_loop(-1, 5);
    assert_eq!(v.segment_loop(), Some((-1, 5)));
    v.set_segment_loop(5, 2);
    assert_eq!(v.segment_loop(), Some((5, 2)));
    v.set_segment_loop(5, 30);
    assert_eq!(v.segment_loop(), Some((5, 30)));

    v.play_rate = 0.0;
    assert_eq!(v.play_rate, 0.0);
    v.play_rate = -2.0;
    assert_eq!(v.play_rate, -2.0);

    v.play_rate = 2.0;
    v.play().unwrap();
    v.advance(250.0);
    assert_eq!(v.position_ms(), 500, "rate multiplies elapsed time");
}

#[test]
fn two_overlays_are_independent() {
    let mut a = VideoOverlay::new();
    let mut b = VideoOverlay::new();
    a.open(spec(30.0, 30)).unwrap();
    b.open(spec(60.0, 60)).unwrap();
    a.play().unwrap();
    b.play().unwrap();
    a.advance(500.0);

    assert_eq!(a.position_ms(), 500);
    assert_eq!(b.position_ms(), 0);
    assert_eq!(a.frame(), 15);
    assert_eq!(b.frame(), 0);
}

/// The clock keeps the exact elapsed time: a step is never rounded, so 600
/// frames of a 60 Hz display add up to exactly ten seconds.
///
/// The reference player clock is a `double` in microsecond units
/// (`reference/cpp/core/movie/ffmpeg/Clock.cpp:30-52`). A rounded integer
/// step would advance the clock by 17 ms per 16.667 ms frame, run the movie
/// two percent fast and skip a frame's worth of audio on every step.
#[test]
fn the_clock_keeps_the_exact_elapsed_time() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 3000)).unwrap();
    v.play().unwrap();
    for _ in 0..600 {
        v.advance(1000.0 / 60.0);
    }

    assert!(
        (v.position_exact_ms() - 10_000.0).abs() < 0.01,
        "expected 10000 ms, got {}",
        v.position_exact_ms()
    );
    assert_eq!(v.position_ms(), 10_000);
    assert_eq!(v.frame(), 300);
}

/// A play rate multiplies the elapsed time without rounding it away.
#[test]
fn the_play_rate_scales_the_exact_clock() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 3000)).unwrap();
    v.play_rate = 2.0;
    v.play().unwrap();
    for _ in 0..600 {
        v.advance(1000.0 / 60.0);
    }
    assert!((v.position_exact_ms() - 20_000.0).abs() < 0.01);
}

/// A loop restart puts the clock back to the start and says so, so a caller
/// that keeps a sound in step with the picture can place the sound back too.
#[test]
fn a_loop_restart_is_reported() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();
    v.looping = true;
    v.play().unwrap();
    v.poll();

    v.advance(100.0);
    assert!(
        !v.restarted_at_loop(),
        "an ordinary frame is no loop restart"
    );

    v.advance(1000.0);
    assert!(v.restarted_at_loop(), "the loop restart is reported");
    assert_eq!(v.position_ms(), 0);
}
