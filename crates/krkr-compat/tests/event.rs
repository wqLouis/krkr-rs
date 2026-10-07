//! Integration tests for the L1 → TJS event adapter skeleton.

use krkr_compat::{EventAdapter, EventValue, PlatformEvent, TjsEvent};

#[test]
fn maps_keys_pointer_and_lifecycle_events() {
    let adapter = EventAdapter::new();

    assert_eq!(
        adapter.map(&PlatformEvent::KeyDown {
            code: 13,
            key: "Enter".into(),
        }),
        Some(TjsEvent {
            name: "onKeyDown",
            members: vec![
                ("code", EventValue::Int(13)),
                ("key", EventValue::Str("Enter".into())),
            ],
        })
    );

    assert_eq!(
        adapter.map(&PlatformEvent::PointerMove { x: 3, y: 4 }),
        Some(TjsEvent {
            name: "onMouseMove",
            members: vec![("x", EventValue::Int(3)), ("y", EventValue::Int(4))],
        })
    );

    assert_eq!(
        adapter.map(&PlatformEvent::Resize {
            width: 1280,
            height: 720,
        }),
        Some(TjsEvent {
            name: "onResize",
            members: vec![
                ("width", EventValue::Int(1280)),
                ("height", EventValue::Int(720)),
            ],
        })
    );

    assert_eq!(
        adapter.map(&PlatformEvent::FocusGained),
        Some(TjsEvent {
            name: "onFocus",
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
