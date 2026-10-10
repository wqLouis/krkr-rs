//! Integration tests for the L3 [`compat::EventAdapter`] delivery seam.
//!
//! A bound VM + host + attached L2 scene is driven through the same
//! `begin_input_frame` → `apply_input_event`/`map`/`dispatch` →
//! `end_input_frame` sequence the VM host's frame body uses, and the script's
//! `on*` handlers record what ran.

mod common;

use common::{Env, vm_lock};
use platform::{InputEvent, MouseButton, PlatformEvent};

/// A pointer press is delivered to the layer under the cursor and the release
/// follows the mouse capture.
///
/// Reference: `tTVPLayerManager::PrimaryMouseDown` hit-tests the layer tree
/// and captures the hit layer, `PrimaryMouseUp` delivers to the capture owner
/// and releases it when no button remains held
/// (`reference/cpp/core/visual/LayerManager.cpp:374-422`); the events are
/// `onMouseDown`/`onMouseUp` on the layer object
/// (`reference/cpp/core/visual/LayerIntf.cpp:3410-3437`).
#[test]
fn dispatch_delivers_pointer_events_to_the_hit_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.focusable = true; l.hitThreshold = 0; \
         var down = 0; var up = 0; var button = -1; \
         l.onMouseDown = function(e) { down = down + 1; button = e.button; }; \
         l.onMouseUp = function(e) { up = up + 1; };",
    );

    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
    ]);
    // reference/cpp/core/visual/LayerManager.cpp:374-399
    assert_eq!(env.eval_int("down"), 1);
    assert_eq!(env.eval_int("button"), 0);

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: false,
    })]);
    // reference/cpp/core/visual/LayerManager.cpp:401-422
    assert_eq!(env.eval_int("up"), 1);
}

/// Key events go to the window's focused layer; a window focus event goes to
/// the window object.
///
/// Reference: `tTVPLayerManager::PrimaryKeyDown`/`PrimaryKeyUp` deliver to
/// `FocusedLayer` (`reference/cpp/core/visual/LayerManager.cpp:1010-1022`);
/// the OS activation event is posted to the window owner
/// (`reference/cpp/core/visual/WindowIntf.cpp:485-506`).
#[test]
fn dispatch_delivers_keys_to_the_focused_layer_and_focus_to_the_window() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.focusable = true; l.visible = true; \
         var keyDown = 0; var keyCode = -1; \
         l.onKeyDown = function(e) { keyDown = keyDown + 1; keyCode = e.code; }; \
         var focused = 0; \
         w.onFocus = function() { focused = focused + 1; }; \
         w.focusedLayer = l;",
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::Key {
        code: 13,
        pressed: true,
    })]);
    // reference/cpp/core/visual/LayerManager.cpp:1010-1015
    assert_eq!(env.eval_int("keyDown"), 1);
    assert_eq!(env.eval_int("keyCode"), 13);

    env.dispatch_events(&[PlatformEvent::Focused(true)]);
    // reference/cpp/core/visual/WindowIntf.cpp:485-506
    assert_eq!(env.eval_int("focused"), 1);
}

/// Once a layer has captured the mouse, the following moves and the release
/// stay on it even when the cursor is over another layer.
///
/// Reference: `PrimaryMouseMove`, `PrimaryMouseDown` and `PrimaryMouseUp`
/// all prefer `CaptureOwner` over the layer under the cursor
/// (`reference/cpp/core/visual/LayerManager.cpp:374-461`).
#[test]
fn dispatch_honours_mouse_capture() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var a = new Layer(w, null); a.setPos(0, 0); a.setSize(50, 50); a.visible = true; a.focusable = true; a.hitThreshold = 0; \
         var b = new Layer(w, null); b.setPos(60, 0); b.setSize(50, 50); b.visible = true; b.hitThreshold = 0; \
         var aMoves = 0; var bMoves = 0; var aUps = 0; \
         a.onMouseMove = function(e) { aMoves = aMoves + 1; }; \
         b.onMouseMove = function(e) { bMoves = bMoves + 1; }; \
         a.onMouseUp = function(e) { aUps = aUps + 1; };",
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 10.0,
        y: 10.0,
    })]);
    assert_eq!(env.eval_int("aMoves"), 1);
    assert_eq!(env.eval_int("bMoves"), 0);

    // Press on `a` (capture), then move over `b`: the capture keeps the move.
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::PointerMoved { x: 70.0, y: 10.0 }),
    ]);
    // reference/cpp/core/visual/LayerManager.cpp:424-461
    assert_eq!(env.eval_int("aMoves"), 2);
    assert_eq!(env.eval_int("bMoves"), 0);

    // Release clears the capture; the next move hits `b`.
    env.dispatch_events(&[PlatformEvent::Input(InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: false,
    })]);
    assert_eq!(env.eval_int("aUps"), 1);
    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 70.0,
        y: 10.0,
    })]);
    assert_eq!(env.eval_int("bMoves"), 1);
}

/// A pointer event that hits nothing is a no-op.
///
/// Reference: `GetMostFrontChildAt` returns `nullptr` and the manager fires
/// nothing (`reference/cpp/core/visual/LayerManager.cpp:345-372`,
/// `:424-461`).
#[test]
fn dispatch_is_a_no_op_when_nothing_is_hit() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         l.setPos(0, 0); l.setSize(50, 50); l.visible = true; l.hitThreshold = 0; \
         var moves = 0; \
         l.onMouseMove = function(e) { moves = moves + 1; };",
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 200.0,
        y: 200.0,
    })]);
    // reference/cpp/core/visual/LayerIntf.cpp:3312-3361
    assert_eq!(env.eval_int("moves"), 0);
}

/// A throwing handler is reported and does not abort the frame.
///
/// Reference: `tTVPEvent::Deliver` propagates the `FuncCall` error
/// (`reference/cpp/core/base/EventIntf.cpp:97-118`); the port logs and keeps
/// the frame alive like the other layer handlers (`onPaint`,
/// `onTransitionCompleted`).
#[test]
fn dispatch_survives_a_throwing_handler() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         l.setPos(0, 0); l.setSize(50, 50); l.visible = true; l.hitThreshold = 0; \
         var entered = 0; \
         l.onMouseDown = function(e) { entered = entered + 1; throw 'boom'; };",
    );

    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
    ]);
    // reference/cpp/core/base/EventIntf.cpp:97-118
    assert_eq!(env.eval_int("entered"), 1);
    assert_eq!(env.eval_int("1"), 1);
}
