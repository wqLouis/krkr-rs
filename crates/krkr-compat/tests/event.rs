//! Integration tests for the L1 → TJS event adapter.

use krkr_compat::{EventAdapter, EventValue, TjsEvent};
use krkr_platform::{InputEvent, MouseButton, PlatformEvent};

#[test]
fn maps_keys_pointer_and_lifecycle_events() {
    let adapter = EventAdapter::new();

    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::Key {
            code: 13,
            pressed: true,
        })),
        Some(TjsEvent {
            name: "onKeyDown",
            members: vec![("code", EventValue::Int(13))],
        })
    );
    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::Key {
            code: 13,
            pressed: false,
        })),
        Some(TjsEvent {
            name: "onKeyUp",
            members: vec![("code", EventValue::Int(13))],
        })
    );

    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::PointerMoved {
            x: 3.4,
            y: 4.6,
        })),
        Some(TjsEvent {
            name: "onMouseMove",
            members: vec![("x", EventValue::Int(3)), ("y", EventValue::Int(5))],
        })
    );

    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        })),
        Some(TjsEvent {
            name: "onMouseDown",
            members: vec![("button", EventValue::Int(0))],
        })
    );

    assert_eq!(
        adapter.map(&PlatformEvent::Resized(1280, 720)),
        Some(TjsEvent {
            name: "onResize",
            members: vec![
                ("width", EventValue::Int(1280)),
                ("height", EventValue::Int(720)),
            ],
        })
    );

    assert_eq!(
        adapter.map(&PlatformEvent::Focused(true)),
        Some(TjsEvent {
            name: "onFocus",
            members: Vec::new(),
        })
    );
    assert_eq!(
        adapter.map(&PlatformEvent::Focused(false)),
        Some(TjsEvent {
            name: "onBlur",
            members: Vec::new(),
        })
    );
    assert_eq!(
        adapter.map(&PlatformEvent::CloseRequested),
        Some(TjsEvent {
            name: "onClose",
            members: Vec::new(),
        })
    );
}

#[test]
fn ignores_events_without_a_script_handler() {
    let adapter = EventAdapter::new();
    assert_eq!(adapter.map(&PlatformEvent::ScaleFactorChanged(2.0)), None);
    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::PointerLeft)),
        None
    );
}
