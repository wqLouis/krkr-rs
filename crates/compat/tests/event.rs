//! Integration tests for the L1 → TJS event adapter.

use api::host::TouchPhase;
use compat::{EventAdapter, EventValue, TjsEvent};
use platform::{InputEvent, MouseButton, PlatformEvent};

#[test]
fn maps_keys_pointer_and_lifecycle_events() {
    let adapter = EventAdapter::new();

    // `FireKeyDown`/`FireKeyUp` pass `(key, shift, process = true)`
    // (`reference/cpp/core/visual/LayerIntf.cpp:3844-3867`).
    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::Key {
            code: 13,
            pressed: true,
        })),
        Some(TjsEvent {
            name: "onKeyDown",
            args: vec![EventValue::Int(13), EventValue::Int(0), EventValue::Int(1)],
        })
    );
    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::Key {
            code: 13,
            pressed: false,
        })),
        Some(TjsEvent {
            name: "onKeyUp",
            args: vec![EventValue::Int(13), EventValue::Int(0), EventValue::Int(1)],
        })
    );

    // `FireMouseMove` passes `(x, y, flags)`
    // (`reference/cpp/core/visual/LayerIntf.cpp:3438-3444`).
    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::PointerMoved {
            x: 3.4,
            y: 4.6,
        })),
        Some(TjsEvent {
            name: "onMouseMove",
            args: vec![EventValue::Int(3), EventValue::Int(5), EventValue::Int(0)],
        })
    );

    // `FireMouseDown` passes `(x, y, button, flags)`
    // (`reference/cpp/core/visual/LayerIntf.cpp:3410-3423`).
    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        })),
        Some(TjsEvent {
            name: "onMouseDown",
            args: vec![
                EventValue::Int(0),
                EventValue::Int(0),
                EventValue::Int(0),
                EventValue::Int(0),
            ],
        })
    );

    // `tTVPOnResizeInputEvent::Deliver` calls `OnResize`, and the reference
    // `OnResize` posts `onResize` with no arguments
    // (`reference/cpp/core/visual/WindowIntf.cpp:302-310`).
    assert_eq!(
        adapter.map(&PlatformEvent::Resized(1280, 720)),
        Some(TjsEvent {
            name: "onResize",
            args: Vec::new(),
        })
    );

    // `OnActivate` posts `onActivate`/`onDeactivate` with no arguments
    // (`reference/cpp/core/visual/WindowIntf.cpp:556-570`).
    assert_eq!(
        adapter.map(&PlatformEvent::Focused(true)),
        Some(TjsEvent {
            name: "onActivate",
            args: Vec::new(),
        })
    );
    assert_eq!(
        adapter.map(&PlatformEvent::Focused(false)),
        Some(TjsEvent {
            name: "onDeactivate",
            args: Vec::new(),
        })
    );
    // `OnClose` posts `onCloseQuery(true)`
    // (`reference/cpp/core/visual/WindowIntf.cpp:292-300`).
    assert_eq!(
        adapter.map(&PlatformEvent::CloseRequested),
        Some(TjsEvent {
            name: "onCloseQuery",
            args: vec![EventValue::Int(1)],
        })
    );
}

#[test]
fn maps_text_touch_and_pointer_leave() {
    let adapter = EventAdapter::new();

    // Committed text posts `onKeyPress(key, process)` with `process = true`:
    // `FireKeyPress` posts `(char, true)`
    // (`reference/cpp/core/visual/LayerIntf.cpp:3868-3877`).
    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::Text("a".into()))),
        Some(TjsEvent {
            name: "onKeyPress",
            args: vec![EventValue::Str("a".into()), EventValue::Int(1)],
        })
    );

    // A touch contact posts the layer touch event with
    // `(x, y, cx, cy, id)`: `FireTouchDown`
    // (`reference/cpp/core/visual/LayerIntf.cpp:3479-3487`).
    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::Touch {
            id: 7,
            phase: TouchPhase::Started,
            x: 3.0,
            y: 4.0,
        })),
        Some(TjsEvent {
            name: "onTouchDown",
            args: vec![
                EventValue::Real(3.0),
                EventValue::Real(4.0),
                EventValue::Real(3.0),
                EventValue::Real(4.0),
                EventValue::Int(7),
            ],
        })
    );
    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::Touch {
            id: 7,
            phase: TouchPhase::Moved,
            x: 5.0,
            y: 6.0,
        })),
        Some(TjsEvent {
            name: "onTouchMove",
            args: vec![
                EventValue::Real(5.0),
                EventValue::Real(6.0),
                EventValue::Real(5.0),
                EventValue::Real(6.0),
                EventValue::Int(7),
            ],
        })
    );
    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::Touch {
            id: 7,
            phase: TouchPhase::Ended,
            x: 5.0,
            y: 6.0,
        })),
        Some(TjsEvent {
            name: "onTouchUp",
            args: vec![
                EventValue::Real(5.0),
                EventValue::Real(6.0),
                EventValue::Real(5.0),
                EventValue::Real(6.0),
                EventValue::Int(7),
            ],
        })
    );

    // The pointer leaving the window posts the window-level `onMouseLeave`
    // (`tTVPOnMouseLeaveInputEvent`,
    // `reference/cpp/core/visual/WindowIntf.h:479-485`).
    assert_eq!(
        adapter.map(&PlatformEvent::Input(InputEvent::PointerLeft)),
        Some(TjsEvent {
            name: "onMouseLeave",
            args: Vec::new(),
        })
    );
}

#[test]
fn ignores_events_without_a_script_handler() {
    let adapter = EventAdapter::new();
    // The scale factor is an L1 DPI concept; TVP exposes no window handler for
    // it (the reference window handlers are
    // `reference/cpp/core/visual/WindowIntf.cpp:934-1250`).
    assert_eq!(adapter.map(&PlatformEvent::ScaleFactorChanged(2.0)), None);
}
