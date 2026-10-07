//! Integration tests for the L2 video-overlay playback state
//! (`engine::video`).
//!
//! Headless by construction: the overlay never opens a codec. Every test
//! drives the state machine with `advance(dt)` and supplies decoded frames
//! directly, exactly as the L1 `media` decoder would. The reference behaviour
//! is `main`'s `crates/tvp-natives/src/video_overlay.rs`
//! (`VideoOverlayInst`/`advance_playback`) and the reference `VideoOverlay`.

use api::media::{RgbaFrame, VideoSpec};
use engine::video::{
    OverlayBinding, PeriodReason, PlayStatus, VideoBounds, VideoError, VideoEvent, VideoOverlay,
};

fn spec(fps: f32, frames: u64) -> VideoSpec {
    VideoSpec {
        width: 640,
        height: 360,
        fps,
        total_frames: Some(frames),
        sample_rate: 0,
        channels: 0,
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

#[test]
fn non_looping_stream_finishes_and_fires_transition() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();
    v.play().unwrap();
    v.poll();
    v.advance(5000.0);

    assert_eq!(v.status(), PlayStatus::Stop);
    assert!(!v.is_playing());
    assert_eq!(v.position_ms(), 1000);
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
    v.set_looping(true);
    v.play().unwrap();
    v.poll();
    v.advance(1500.0);

    assert_eq!(v.position_ms(), 0);
    assert!(v.is_playing());
    assert_eq!(v.poll(), vec![VideoEvent::Period(PeriodReason::Loop)]);
}

#[test]
fn segment_loop_wraps_inside_the_segment() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 300)).unwrap();
    v.set_segment_loop(10, 19).unwrap();
    assert_eq!(v.segment_loop(), Some((10, 19)));
    v.set_looping(true);
    v.play().unwrap();
    v.poll();
    v.advance(700.0); // frame 21 is past the segment end 19

    // frame 10 at 30 fps is ~333 ms
    assert_eq!(v.position_ms(), 333);
    let events = v.poll();
    assert!(events.contains(&VideoEvent::Period(PeriodReason::SegmentLoop)));
    assert!(events.contains(&VideoEvent::TransitionComplete));

    v.cancel_segment_loop();
    assert_eq!(v.segment_loop(), None);
}

#[test]
fn period_event_fires_once_per_arm() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 300)).unwrap();
    v.set_period_event(3);
    assert_eq!(v.period_event(), Some(3));
    v.play().unwrap();
    v.poll();

    v.advance(120.0);
    assert_eq!(v.poll(), vec![VideoEvent::Period(PeriodReason::Period)]);
    v.advance(100.0);
    assert!(v.poll().is_empty(), "period event fires once");

    // A seek re-arms it.
    v.seek_frame(0);
    v.advance(120.0);
    assert_eq!(v.poll(), vec![VideoEvent::Period(PeriodReason::Period)]);

    v.cancel_period_event();
    assert_eq!(v.period_event(), None);
}

#[test]
fn prepare_fires_the_prepare_period() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();
    v.prepare().unwrap();
    assert_eq!(v.poll(), vec![VideoEvent::Period(PeriodReason::Prepare)]);
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
        v.present_frame(bad),
        Err(VideoError::InconsistentFrame { .. })
    ));

    v.present_frame(frame(2, 2, 33)).unwrap();
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

    v.present_frame(frame(1, 1, 0)).unwrap();
    v.advance(100.0); // frame 6
    assert_eq!(v.poll(), vec![VideoEvent::FrameUpdate(6)]);
}

#[test]
fn overlay_binding_and_rectangle() {
    let mut v = VideoOverlay::new();
    v.set_visible(true);
    v.bind_layer(4);
    v.bind_window(2);
    v.set_binding_alpha(2.0); // clamped
    assert_eq!(
        v.binding(),
        OverlayBinding::new(Some(4), Some(2), 1.0, true)
    );

    v.set_bounds(VideoBounds::new(1, 2, 3, 4));
    assert_eq!(v.bounds(), VideoBounds::new(1, 2, 3, 4));
    v.set_position(9, 8);
    v.set_size(7, 6);
    assert_eq!(v.bounds(), VideoBounds::new(9, 8, 7, 6));

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

#[test]
fn invalid_segment_and_rate_are_typed_errors() {
    let mut v = VideoOverlay::new();
    v.open(spec(30.0, 30)).unwrap();
    assert!(matches!(
        v.set_segment_loop(-1, 5),
        Err(VideoError::InvalidSegment { .. })
    ));
    assert!(matches!(
        v.set_segment_loop(5, 2),
        Err(VideoError::InvalidSegment { .. })
    ));
    assert!(matches!(
        v.set_segment_loop(5, 30),
        Err(VideoError::InvalidSegment { .. })
    ));
    assert!(matches!(
        v.set_play_rate(0.0),
        Err(VideoError::InvalidPlayRate { .. })
    ));

    v.set_play_rate(2.0).unwrap();
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
