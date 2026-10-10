//! Integration tests for the L2 generic input state (`engine::input`).
//!
//! The state is pure and clock-free: events are folded in explicitly and the
//! `begin_frame`/`end_frame` protocol advances the held counters. No VM or
//! platform backend is involved.

use api::host::TouchPhase;
use api::input::{InputEvent, MouseButton};
use engine::input::{DEFAULT_CLICK_SEQUENCE_FRAMES, InputState};

#[test]
fn pointer_wheel_and_visibility_track_events() {
    let mut state = InputState::new();
    state.begin_frame();
    state.apply(&InputEvent::PointerMoved { x: 12.0, y: -4.0 });
    state.apply(&InputEvent::Wheel { delta: 120.0 });
    state.apply(&InputEvent::Wheel { delta: -20.0 });
    assert_eq!(state.pointer(), (12.0, -4.0));
    assert!(state.pointer_inside());
    assert_eq!(state.wheel(), 100.0);

    // The wheel delta is per-frame.
    state.end_frame();
    state.begin_frame();
    assert_eq!(state.wheel(), 0.0);

    assert!(state.pointer_visible);
    state.pointer_visible = false;
    assert!(!state.pointer_visible);
    state.apply(&InputEvent::PointerLeft);
    assert!(!state.pointer_inside());
}

#[test]
fn mouse_button_edges_hold_and_release_flags() {
    let mut state = InputState::new();
    state.begin_frame();
    state.apply(&InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: true,
    });
    assert!(state.is_button_down(MouseButton::Left));
    assert!(!state.is_button_released(MouseButton::Left));
    state.end_frame();
    assert_eq!(state.button_hold_frames(MouseButton::Left), 1);

    // A repeated down within a frame is ignored.
    state.begin_frame();
    state.apply(&InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: true,
    });
    state.end_frame();
    assert_eq!(state.button_hold_frames(MouseButton::Left), 2);

    state.begin_frame();
    state.apply(&InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: false,
    });
    assert!(state.is_button_released(MouseButton::Left));
    assert_eq!(state.button_hold_frames(MouseButton::Left), 0);
    state.end_frame();

    // The release flag lasts exactly one frame.
    state.begin_frame();
    state.end_frame();
    assert!(!state.is_button_released(MouseButton::Left));
}

#[test]
fn click_sequence_counts_and_times_out() {
    let mut state = InputState::new();
    state.set_click_sequence(DEFAULT_CLICK_SEQUENCE_FRAMES, 4.0);
    state.begin_frame();
    state.set_pointer(10.0, 10.0);
    state.set_button(MouseButton::Left, true);
    state.end_frame();
    assert_eq!(state.button_click_count(MouseButton::Left), 1);
    assert_eq!(
        state.button_last_click_pos(MouseButton::Left),
        Some((10.0, 10.0))
    );

    state.begin_frame();
    state.set_button(MouseButton::Left, false);
    state.set_button(MouseButton::Left, true);
    state.end_frame();
    assert_eq!(state.button_click_count(MouseButton::Left), 2);

    // Moving far starts a new sequence.
    state.begin_frame();
    state.set_button(MouseButton::Left, false);
    state.set_pointer(500.0, 500.0);
    state.set_button(MouseButton::Left, true);
    state.end_frame();
    assert_eq!(state.button_click_count(MouseButton::Left), 1);

    for _ in 0..=DEFAULT_CLICK_SEQUENCE_FRAMES {
        state.begin_frame();
        state.end_frame();
    }
    assert_eq!(state.button_click_count(MouseButton::Left), 0);
}

#[test]
fn keyboard_edges_repeat_and_last_pressed() {
    let mut state = InputState::new();
    state.begin_frame();
    state.apply(&InputEvent::Key {
        code: 0x0D,
        pressed: true,
    });
    assert!(state.is_key_down(0x0D));
    assert_eq!(state.last_pressed(), &[0x0D]);
    assert_eq!(state.held_keys().collect::<Vec<_>>(), vec![0x0D]);
    state.end_frame();
    assert_eq!(state.key_hold_frames(0x0D), 1);

    // A repeated down in a later frame is not a new edge.
    state.begin_frame();
    state.apply(&InputEvent::Key {
        code: 0x0D,
        pressed: true,
    });
    assert!(state.last_pressed().is_empty());
    state.end_frame();
    assert_eq!(state.key_hold_frames(0x0D), 2);

    state.begin_frame();
    state.apply(&InputEvent::Key {
        code: 0x0D,
        pressed: false,
    });
    assert!(state.is_key_released(0x0D));
    assert_eq!(state.released_keys().collect::<Vec<_>>(), vec![0x0D]);
    assert_eq!(state.key_hold_frames(0x0D), 0);
    state.end_frame();

    state.begin_frame();
    state.end_frame();
    assert!(!state.is_key_released(0x0D));
}

#[test]
fn end_frame_advances_held_counters_exactly_once() {
    let mut state = InputState::new();
    state.begin_frame();
    state.set_button(MouseButton::Left, true);
    state.set_key(4, true);
    state.end_frame();
    assert_eq!(state.frame(), 1);
    assert_eq!(state.button_hold_frames(MouseButton::Left), 1);
    assert_eq!(state.key_hold_frames(4), 1);

    // Two begin/end cycles without events still advance the held counters,
    // and only once per cycle.
    state.begin_frame();
    state.end_frame();
    state.begin_frame();
    state.end_frame();
    assert_eq!(state.frame(), 3);
    assert_eq!(state.button_hold_frames(MouseButton::Left), 3);
    assert_eq!(state.key_hold_frames(4), 3);
}

#[test]
fn text_input_accumulates_and_clears_per_frame() {
    let mut state = InputState::new();
    state.begin_frame();
    state.apply(&InputEvent::Text("he".into()));
    state.apply(&InputEvent::Text("llo".into()));
    assert_eq!(state.text_input(), "hello");
    state.end_frame();
    state.begin_frame();
    assert_eq!(state.text_input(), "");
}

#[test]
fn multi_touch_tracks_contacts_by_id_and_velocity() {
    let mut state = InputState::new();
    state.apply(&InputEvent::Touch {
        id: 1,
        phase: TouchPhase::Started,
        x: 10.0,
        y: 20.0,
    });
    state.apply(&InputEvent::Touch {
        id: 2,
        phase: TouchPhase::Started,
        x: 30.0,
        y: 40.0,
    });
    assert_eq!(state.touch_point_count(), 2);
    assert_eq!(state.touch_point(1).unwrap().pos, (10.0, 20.0));
    assert_eq!(state.touch_point(2).unwrap().pos, (30.0, 40.0));
    assert_eq!(state.touch_point(1).unwrap().phase, TouchPhase::Started);

    state.apply(&InputEvent::Touch {
        id: 1,
        phase: TouchPhase::Moved,
        x: 14.0,
        y: 26.0,
    });
    assert_eq!(state.touch_point(1).unwrap().pos, (14.0, 26.0));
    assert_eq!(state.touch_point(1).unwrap().phase, TouchPhase::Moved);
    assert_eq!(state.touch_velocity(1), Some((4.0, 6.0)));
    assert!((state.touch_speed(1).unwrap() - 52.0_f32.sqrt()).abs() < 1e-6);

    state.apply(&InputEvent::Touch {
        id: 2,
        phase: TouchPhase::Ended,
        x: 30.0,
        y: 40.0,
    });
    assert_eq!(state.touch_point_count(), 1);
    assert!(state.touch_point(2).is_none());

    state.apply(&InputEvent::Touch {
        id: 1,
        phase: TouchPhase::Cancelled,
        x: 14.0,
        y: 26.0,
    });
    assert_eq!(state.touch_point_count(), 0);
    assert!(state.touch_velocity(1).is_none());
    assert!(state.touch_speed(1).is_none());
}

#[test]
fn moved_without_start_still_creates_a_contact() {
    let mut state = InputState::new();
    state.apply(&InputEvent::Touch {
        id: 7,
        phase: TouchPhase::Moved,
        x: 1.0,
        y: 2.0,
    });
    assert_eq!(state.touch_point_count(), 1);
    assert_eq!(state.touch_point(7).unwrap().pos, (1.0, 2.0));
}

#[test]
fn warp_request_is_taken_once() {
    let mut state = InputState::new();
    state.warp_pointer(5.0, 6.0);
    assert_eq!(state.pointer(), (5.0, 6.0));
    assert_eq!(state.take_warp_request(), Some((5.0, 6.0)));
    assert_eq!(state.take_warp_request(), None);
}

#[test]
fn unknown_mouse_button_is_ignored_safely() {
    let mut state = InputState::new();
    state.begin_frame();
    state.apply(&InputEvent::MouseButton {
        button: MouseButton::Other(42),
        pressed: true,
    });
    state.end_frame();
    assert!(state.is_button_down(MouseButton::Other(42)));
    assert!(!state.is_button_down(MouseButton::Left));
}
