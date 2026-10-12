//! Integration tests for the platform event value types.

use std::time::Duration;

use api::host::TouchPhase;
use platform::{InputEvent, MouseButton, PhysicalKey, PlatformEvent};
use raw_window_handle::RawWindowHandle;

#[test]
fn platform_events_are_comparable_values() {
    assert_eq!(
        PlatformEvent::Resized(640, 360),
        PlatformEvent::Resized(640, 360)
    );
    assert_ne!(PlatformEvent::CloseRequested, PlatformEvent::Focused(true));
}

#[test]
fn headless_handle_is_an_explicit_synthetic_canvas() {
    let handle = platform::headless_window_handle();
    assert!(matches!(handle, RawWindowHandle::Web(_)));
}

#[test]
fn platform_config_default_has_a_sane_frame_interval() {
    let config = platform::PlatformConfig::default();
    assert_eq!(config.size, (1280, 720));
    assert!(config.frame_interval > Duration::ZERO);
}

#[test]
fn input_events_carry_their_payload() {
    let key = InputEvent::Key {
        code: PhysicalKey::Native(42),
        pressed: true,
    };
    assert_eq!(
        key,
        InputEvent::Key {
            code: PhysicalKey::Native(42),
            pressed: true
        }
    );

    let touch = InputEvent::Touch {
        id: 7,
        phase: TouchPhase::Started,
        x: 1.5,
        y: 2.5,
    };
    assert_ne!(touch, InputEvent::PointerLeft);
    assert_eq!(MouseButton::Other(9), MouseButton::Other(9));
}

#[test]
fn input_event_is_reachable_through_platform_event() {
    let event = PlatformEvent::Input(InputEvent::PointerMoved { x: 1.0, y: 2.0 });
    match event {
        PlatformEvent::Input(InputEvent::PointerMoved { x, y }) => {
            assert_eq!((x, y), (1.0, 2.0));
        }
        other => panic!("unexpected event: {other:?}"),
    }
}
