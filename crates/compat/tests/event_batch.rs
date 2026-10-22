//! Integration tests for the L3 event batch path
//! ([`compat::EventAdapter::dispatch_frame`]).
//!
//! The batch path is what the VM host's frame body runs: it applies every
//! platform event to the L2 input state, maps it and delivers it to the
//! script handler that owns it, in the order the platform produced the
//! events. A frame that collects a flood of events stops dispatching when
//! its dispatch budget is exhausted; the events that did not fit stay queued
//! and the next frame delivers them first. No event is merged, dropped or
//! reordered.

mod common;

use std::time::Duration;

use common::{Env, vm_lock};
use platform::{InputEvent, MouseButton, PhysicalKey, PlatformEvent};

thread_local! {
    static CAPTURED_LOGS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A `log` sink that records every record on the calling thread.
struct CaptureLogger;

impl log::Log for CaptureLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        CAPTURED_LOGS.with(|logs| {
            logs.borrow_mut()
                .push(format!("{} {}", record.level(), record.args()));
        });
    }

    fn flush(&self) {}
}

/// Install the capturing sink once per test binary, clear this thread's
/// buffer, run `f`, and return its result plus the thread's log lines.
fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = log::set_boxed_logger(Box::new(CaptureLogger));
        log::set_max_level(log::LevelFilter::Debug);
    });
    CAPTURED_LOGS.with(|logs| logs.borrow_mut().clear());
    let result = f();
    let logs = CAPTURED_LOGS.with(|logs| logs.borrow().clone());
    (result, logs)
}

/// A burst of pointer moves is delivered completely, one frame at a time.
///
/// The frame stops dispatching when its budget is exhausted and keeps the
/// events that did not fit queued, so the next frame delivers them first.
/// The moves are never merged and never dropped: every move reaches the
/// script, in the order the platform produced it.
///
/// The move is an ordinary event for the script
/// (`reference/cpp/core/visual/LayerIntf.cpp:3438-3447`), so its count and
/// its position are observable.
#[test]
fn a_burst_of_pointer_moves_is_delivered_across_frames_in_order() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.hitThreshold = 0; \
         var moves = 0; var lastX = -1; \
         l.onMouseMove = function(x, y, shift) { moves = moves + 1; lastX = x; };",
    );

    // A zero budget dispatches one event per frame and defers the rest.
    let mut adapter = compat::EventAdapter::with_dispatch_budget(Duration::ZERO);
    env.dispatch_with(
        &mut adapter,
        &[
            PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
            PlatformEvent::Input(InputEvent::PointerMoved { x: 20.0, y: 20.0 }),
        ],
    );
    // The budget is checked after the dispatch, so the frame still delivers
    // one event: the first move.
    // reference/cpp/core/visual/LayerIntf.cpp:3438-3447
    assert_eq!(env.eval_int("moves"), 1);
    assert_eq!(env.eval_int("lastX"), 10);

    // The next frame delivers the deferred move first, then the move the
    // platform brought with it.
    env.dispatch_with(
        &mut adapter,
        &[PlatformEvent::Input(InputEvent::PointerMoved {
            x: 30.0,
            y: 30.0,
        })],
    );
    assert_eq!(env.eval_int("moves"), 2);
    assert_eq!(env.eval_int("lastX"), 20);

    env.dispatch_with(&mut adapter, &[]);
    assert_eq!(env.eval_int("moves"), 3);
    assert_eq!(env.eval_int("lastX"), 30);

    // The queue is empty: no event was lost and none is left over.
    env.dispatch_with(&mut adapter, &[]);
    assert_eq!(env.eval_int("moves"), 3);
}

/// A wheel a frame deferred reaches the focused layer in the next frame.
///
/// The wheel is posted as an ordinary event
/// (`reference/cpp/core/visual/LayerIntf.cpp:3882-3889`), so it is neither
/// merged with another event nor dropped.
#[test]
fn a_deferred_wheel_reaches_the_focused_layer_in_the_next_frame() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.focusable = true; l.hitThreshold = 0; \
         w.focusedLayer = l; \
         var wheels = 0; var delta = 0; \
         l.onMouseWheel = function(shift, d, x, y) { wheels = wheels + 1; delta = d; };",
    );

    let mut adapter = compat::EventAdapter::with_dispatch_budget(Duration::ZERO);
    env.dispatch_with(
        &mut adapter,
        &[
            PlatformEvent::Input(InputEvent::Wheel { delta: 1.0 }),
            PlatformEvent::Input(InputEvent::Wheel { delta: -1.0 }),
        ],
    );
    // reference/cpp/core/visual/LayerIntf.cpp:3882-3889
    assert_eq!(env.eval_int("wheels"), 1);
    assert_eq!(env.eval_int("delta"), 120);

    env.dispatch_with(&mut adapter, &[]);
    assert_eq!(env.eval_int("wheels"), 2);
    assert_eq!(env.eval_int("delta"), -120);
}

/// The wheel, key and text events are delivered one by one, in one frame.
///
/// The key and the text events are ordinary events
/// (`reference/cpp/core/visual/LayerIntf.cpp:3844-3877`), so their count and
/// their order are observable.
#[test]
fn wheel_key_and_text_events_are_all_delivered() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.focusable = true; l.hitThreshold = 0; \
         w.focusedLayer = l; \
         var wheels = 0; var downs = 0; var ups = 0; var presses = 0; \
         l.onMouseWheel = function(shift, delta, x, y) { wheels = wheels + 1; }; \
         l.onKeyDown = function(key, shift, process) { downs = downs + 1; }; \
         l.onKeyUp = function(key, shift, process) { ups = ups + 1; }; \
         l.onKeyPress = function(key, process) { presses = presses + 1; };",
    );

    // A budget that no handful of events reaches: every one of them is
    // delivered in this frame.
    let mut adapter = compat::EventAdapter::with_dispatch_budget(Duration::from_secs(1));
    env.dispatch_with(
        &mut adapter,
        &[
            PlatformEvent::Input(InputEvent::Wheel { delta: 1.0 }),
            PlatformEvent::Input(InputEvent::Wheel { delta: -1.0 }),
            PlatformEvent::Input(InputEvent::Key {
                code: PhysicalKey::KeyA,
                pressed: true,
            }),
            PlatformEvent::Input(InputEvent::Key {
                code: PhysicalKey::KeyA,
                pressed: false,
            }),
            PlatformEvent::Input(InputEvent::Text("a".to_string())),
        ],
    );
    // reference/cpp/core/visual/LayerIntf.cpp:3882-3889, :3844-3877
    assert_eq!(env.eval_int("wheels"), 2);
    assert_eq!(env.eval_int("downs"), 1);
    assert_eq!(env.eval_int("ups"), 1);
    assert_eq!(env.eval_int("presses"), 1);
}

/// A target without a script handler is a no-op: the delivery runs the
/// native default, nothing happens, and the adapter reports nothing.
///
/// The reference delivers the event to the member the owner declares
/// (`reference/cpp/core/base/EventIntf.cpp:97-118`); the compat classes
/// declare every event member, so a layer without a script override runs
/// the native default and nothing else.
#[test]
fn a_target_without_a_handler_is_a_no_op() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.focusable = true; l.hitThreshold = 0; \
         w.focusedLayer = l; \
         var downs = 0; \
         l.onMouseDown = function(x, y, btn, shift) { downs = downs + 1; }; \
         w.action = function(ev) {};",
    );

    let (_, logs) = capture_logs(|| {
        env.dispatch_events(&[
            PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
            PlatformEvent::Input(InputEvent::Wheel { delta: 1.0 }),
            PlatformEvent::Input(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            }),
        ]);
    });
    // The layer has no `onMouseMove` and no `onMouseWheel`: the native
    // defaults run and nothing happens, while the press that follows still
    // reaches the layer.
    assert_eq!(env.eval_int("downs"), 1);
    assert!(
        !logs.iter().any(|line| line.contains("event adapter")),
        "a target without a handler is not reported: {logs:?}"
    );
}

/// A throwing handler is reported and the frame continues.
///
/// The reference propagates the `FuncCall` error out of the event delivery
/// (`reference/cpp/core/base/EventIntf.cpp:97-118`); the port logs the
/// failure and keeps the frame alive.
#[test]
fn a_throwing_handler_is_still_reported() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.hitThreshold = 0; \
         var entered = 0; \
         l.onMouseDown = function(x, y, btn, shift) { entered = entered + 1; throw 'boom'; };",
    );

    let (_, logs) = capture_logs(|| {
        env.dispatch_events(&[
            PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
            PlatformEvent::Input(InputEvent::MouseButton {
                button: MouseButton::Left,
                pressed: true,
            }),
        ]);
    });
    // reference/cpp/core/base/EventIntf.cpp:97-118
    assert_eq!(env.eval_int("entered"), 1);
    assert!(
        logs.iter()
            .any(|line| line.contains("event adapter: onMouseDown dispatch failed")),
        "the throwing handler is reported: {logs:?}"
    );
}
