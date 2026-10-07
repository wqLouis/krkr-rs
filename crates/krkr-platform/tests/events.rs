//! Integration tests for the platform event value types.

use krkr_api::host::TouchPhase;
use krkr_platform::{InputEvent, MouseButton, PlatformEvent};

#[test]
fn platform_events_are_comparable_values() {
    assert_eq!(
        PlatformEvent::Resized(640, 360),
        PlatformEvent::Resized(640, 360)
    );
    assert_ne!(PlatformEvent::CloseRequested, PlatformEvent::Focused(true));
}

#[test]
fn input_events_carry_their_payload() {
    let key = InputEvent::Key {
        code: 42,
        pressed: true,
    };
    assert_eq!(
        key,
        InputEvent::Key {
            code: 42,
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
