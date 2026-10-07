//! Integration tests for the input native classes (`Mouse`, `Key`, `Pad`).
//!
//! The input state lives in the per-binding `CompatServices` dispatch state,
//! so every test builds its own VM + [`CompatHost`](compat::CompatHost) under
//! the shared [`common::vm_lock`] (the engine → context binding is
//! process-wide). Events are fed through the input seam on
//! [`compat::EventAdapter`].

mod common;

use common::vm_lock;
use compat::{CompatHost, EventAdapter};
use platform::{InputEvent, MouseButton, PlatformEvent};
use tjs::{Tjs2Engine, TjsValue};

/// Build a VM with the full compat surface installed and run `f` against it.
///
/// `input` is declared before `host`, so `host` (and its binding) drops first
/// while the L2 input state is still alive.
fn with_vm(f: impl FnOnce(&Tjs2Engine, &mut CompatHost)) {
    let engine = Tjs2Engine::new().expect("create engine");
    let mut input = Box::new(engine::input::InputState::new());
    let mut host = CompatHost::new();
    // SAFETY: `input` outlives the host binding (it is declared after `host`,
    // so it is dropped later), matching `attach_input`'s contract.
    unsafe {
        host.services_mut()
            .attach_input(input.as_mut() as *mut engine::input::InputState);
    }
    host.install(&engine).expect("install compat surface");
    f(&engine, &mut host);
}

fn eval_i(e: &Tjs2Engine, expr: &str) -> i64 {
    match e.eval(expr, "test").expect("eval succeeds") {
        TjsValue::Integer(v) => v,
        other => panic!("expected Integer for {expr:?}, got {other:?}"),
    }
}

/// Feed one frame of input events through the adapter seam.
fn frame(
    adapter: &EventAdapter,
    host: &mut CompatHost,
    events: impl IntoIterator<Item = PlatformEvent>,
) {
    let mut ctx = host.ctx().expect("bound context");
    adapter.begin_input_frame(&mut ctx);
    for event in events {
        adapter.apply_input_event(&mut ctx, &event);
    }
    adapter.end_input_frame(&mut ctx);
}

#[test]
fn mouse_reads_state_fed_from_events() {
    let _lock = vm_lock();
    with_vm(|e, host| {
        let adapter = EventAdapter::new();
        frame(
            &adapter,
            host,
            [
                PlatformEvent::Input(InputEvent::PointerMoved { x: 300.0, y: 200.0 }),
                PlatformEvent::Input(InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                }),
                PlatformEvent::Input(InputEvent::Wheel { delta: 120.0 }),
            ],
        );

        assert_eq!(eval_i(e, "Mouse.getCursorX()"), 300);
        assert_eq!(eval_i(e, "Mouse.getCursorY()"), 200);
        assert_eq!(
            e.eval("Mouse.getCursorPos()", "test").unwrap(),
            TjsValue::String("300,200".into())
        );
        assert_eq!(eval_i(e, "Mouse.getPressed(0)"), 1); // mbLeft
        assert_eq!(eval_i(e, "Mouse.getRepeat(0)"), 1);
        assert_eq!(eval_i(e, "Mouse.getWheelRot()"), 120);
        assert_eq!(eval_i(e, "Mouse.getWheelRotY()"), 120);
        assert_eq!(eval_i(e, "Mouse.getClickCount(0)"), 1);

        // Release: visible for exactly one frame, then gone.
        frame(
            &adapter,
            host,
            [PlatformEvent::Input(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            })],
        );
        assert_eq!(eval_i(e, "Mouse.getPressed(0)"), 0);
        assert_eq!(eval_i(e, "Mouse.getReleased(0)"), 1);
        assert_eq!(eval_i(e, "Mouse.getRepeat(0)"), 0);

        frame(&adapter, host, []);
        assert_eq!(eval_i(e, "Mouse.getReleased(0)"), 0);
        // begin_frame cleared the wheel accumulation.
        assert_eq!(eval_i(e, "Mouse.getWheelRot()"), 0);
    });
}

#[test]
fn key_reads_state_fed_from_events_and_repeats() {
    let _lock = vm_lock();
    with_vm(|e, host| {
        let adapter = EventAdapter::new();
        frame(
            &adapter,
            host,
            [PlatformEvent::Input(InputEvent::Key {
                code: 0x0D, // VK_RETURN
                pressed: true,
            })],
        );
        assert_eq!(eval_i(e, "Key.getPressed(Key.kReturn)"), 1);
        assert_eq!(eval_i(e, "Key.getRepeat(Key.kReturn)"), 1);

        frame(&adapter, host, []);
        assert_eq!(eval_i(e, "Key.getRepeat(Key.kReturn)"), 2);

        frame(
            &adapter,
            host,
            [PlatformEvent::Input(InputEvent::Key {
                code: 0x0D,
                pressed: false,
            })],
        );
        assert_eq!(eval_i(e, "Key.getPressed(Key.kReturn)"), 0);
        assert_eq!(eval_i(e, "Key.getReleased(Key.kReturn)"), 1);

        frame(&adapter, host, []);
        assert_eq!(eval_i(e, "Key.getReleased(Key.kReturn)"), 0);
        assert_eq!(eval_i(e, "Key.getRepeat(Key.kReturn)"), 0);
    });
}

#[test]
fn key_resolves_mouse_button_vks_and_pad_any() {
    let _lock = vm_lock();
    with_vm(|e, host| {
        let adapter = EventAdapter::new();
        frame(
            &adapter,
            host,
            [
                PlatformEvent::Input(InputEvent::MouseButton {
                    button: MouseButton::Left,
                    pressed: true,
                }),
                PlatformEvent::Input(InputEvent::Key {
                    code: 0x1C0, // VK_PAD1
                    pressed: true,
                }),
                PlatformEvent::Input(InputEvent::Key {
                    code: 0x1B6, // VK_PADUP
                    pressed: true,
                }),
            ],
        );
        assert_eq!(eval_i(e, "Key.getPressed(Key.kLButton)"), 1);
        assert_eq!(eval_i(e, "Key.getPressed(Key.kRButton)"), 0);
        assert_eq!(eval_i(e, "Key.getPressed(Key.kPad1)"), 1);
        assert_eq!(eval_i(e, "Key.getPressed(Key.kPadAny)"), 1);

        frame(
            &adapter,
            host,
            [PlatformEvent::Input(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: false,
            })],
        );
        assert_eq!(eval_i(e, "Key.getReleased(Key.kLButton)"), 1);
    });
}

#[test]
fn key_constants_match_the_reference_values() {
    let _lock = vm_lock();
    with_vm(|e, _host| {
        for (name, expected) in [
            ("kBack", 0x08),
            ("kReturn", 0x0D),
            ("kShift", 0x10),
            ("kSpace", 0x20),
            ("kA", 0x41),
            ("kZ", 0x5A),
            ("k0", 0x30),
            ("kNumPad0", 0x60),
            ("kF1", 0x70),
            ("kF24", 0x87),
            ("kLShift", 0xA0),
            ("kPadLeft", 0x1B5),
            ("kPadAny", 0x1DF),
            ("kOEMClear", 0xFE),
        ] {
            assert_eq!(
                eval_i(e, &format!("Key.{name}")),
                expected as i64,
                "Key.{name}"
            );
        }
    });
}

#[test]
fn unknown_codes_return_false_not_an_error() {
    let _lock = vm_lock();
    with_vm(|e, _host| {
        assert_eq!(eval_i(e, "Key.getPressed(0x9999)"), 0);
        assert_eq!(eval_i(e, "Key.getReleased(0x9999)"), 0);
        assert_eq!(eval_i(e, "Key.getRepeat(0x9999)"), 0);
        assert_eq!(eval_i(e, "Key.getPressed(-1)"), 0);
        assert_eq!(eval_i(e, "Mouse.getPressed(5)"), 0);
        assert_eq!(eval_i(e, "Mouse.getPressed(-1)"), 0);
        assert_eq!(eval_i(e, "Mouse.getReleased(99)"), 0);
        assert_eq!(eval_i(e, "Mouse.getClickCount(99)"), 0);
    });
}

#[test]
fn mouse_set_cursor_pos_writes_state_and_queues_a_warp() {
    let _lock = vm_lock();
    with_vm(|e, _host| {
        assert_eq!(
            e.eval("Mouse.setCursorPos(50, 60)", "test").unwrap(),
            TjsValue::Void
        );
        assert_eq!(eval_i(e, "Mouse.getCursorX()"), 50);
        assert_eq!(eval_i(e, "Mouse.getCursorY()"), 60);
        assert_eq!(
            e.eval("Mouse.getCursorPos()", "test").unwrap(),
            TjsValue::String("50,60".into())
        );
        assert!(e.eval("Mouse.setCursorPos(1)", "test").is_err());
    });
}

#[test]
fn mouse_get_cursor_pos_fills_an_object_argument() {
    let _lock = vm_lock();
    with_vm(|e, host| {
        let adapter = EventAdapter::new();
        frame(
            &adapter,
            host,
            [PlatformEvent::Input(InputEvent::PointerMoved {
                x: 7.0,
                y: 9.0,
            })],
        );

        e.exec_script("global.__pos = %[x:0, y:0];", "test")
            .expect("define pos");
        assert_eq!(
            e.eval("Mouse.getCursorPos(global.__pos)", "test").unwrap(),
            TjsValue::Void
        );
        assert_eq!(eval_i(e, "global.__pos.x"), 7);
        assert_eq!(eval_i(e, "global.__pos.y"), 9);

        // A missing member is created (TJS_MEMBERENSURE semantics).
        e.exec_script("global.__empty = %[];", "test").unwrap();
        e.eval("Mouse.getCursorPos(global.__empty)", "test")
            .unwrap();
        assert_eq!(eval_i(e, "global.__empty.x"), 7);
        assert_eq!(eval_i(e, "global.__empty.y"), 9);

        // A non-object argument falls back to the string form.
        assert_eq!(
            e.eval("Mouse.getCursorPos(123)", "test").unwrap(),
            TjsValue::String("7,9".into())
        );
    });
}

#[test]
fn mouse_visible_toggles_and_defaults_to_true() {
    let _lock = vm_lock();
    with_vm(|e, _host| {
        assert_eq!(eval_i(e, "Mouse.isVisible()"), 1);
        assert_eq!(
            e.eval("Mouse.setVisible(false)", "test").unwrap(),
            TjsValue::Void
        );
        assert_eq!(eval_i(e, "Mouse.isVisible()"), 0);
        e.eval("Mouse.setVisible(true)", "test").unwrap();
        assert_eq!(eval_i(e, "Mouse.isVisible()"), 1);
    });
}

#[test]
fn pad_is_a_read_only_property_stub() {
    let _lock = vm_lock();
    with_vm(|e, _host| {
        e.exec_script("global.__pad = new Pad();", "test")
            .expect("construct Pad");
        assert_eq!(
            e.eval("global.__pad.text", "test").unwrap(),
            TjsValue::String(String::new())
        );
        assert_eq!(
            e.eval("global.__pad.fileName", "test").unwrap(),
            TjsValue::String(String::new())
        );
        assert_eq!(eval_i(e, "global.__pad.width"), 100);
        assert_eq!(eval_i(e, "global.__pad.height"), 100);
        assert_eq!(eval_i(e, "global.__pad.fontSize"), 10);
        assert_eq!(eval_i(e, "global.__pad.readOnly"), 1);
        assert_eq!(eval_i(e, "global.__pad.opacity"), 255);
        assert_eq!(eval_i(e, "global.__pad.visible"), 0);
        // The reference setters are accepted and ignored (no TJS error).
        e.exec_script("global.__pad.width = 5;", "test").unwrap();
        assert_eq!(eval_i(e, "global.__pad.width"), 100);
    });
}
