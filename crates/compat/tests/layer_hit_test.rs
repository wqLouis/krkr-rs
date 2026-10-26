//! Integration tests for the `Layer.onHitTest` hook.
//!
//! The reference hit test posts `onHitTest` to the layer that hits
//! geometrically and takes the layer's hit flag as the result
//! (`reference/cpp/core/visual/LayerIntf.cpp:3286-3309`). The event runs
//! synchronously, inside the hit test, and the flag the native member sets
//! from its third argument is the hit test's verdict
//! (`reference/cpp/core/visual/LayerIntf.cpp:9944-9968`).

mod common;

use common::{Env, vm_lock};
use platform::{InputEvent, MouseButton, PlatformEvent};

/// The hit test posts `onHitTest` to the layer that hits, with the point in
/// the layer's own coordinates and `true` as the third argument.
///
/// Reference `tTJSNI_BaseLayer::HitTestNoVisibleCheck`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3286-3309`): the flag starts
/// accepted, the event is posted with `(x, y, true)`, and the flag after the
/// event is the result.
#[test]
fn the_hit_test_calls_the_on_hit_test_handler() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window();\
         w.setSize(320, 240);\
         var base = new Layer(w, null);\
         var l = new Layer(w, base);\
         l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.hitThreshold = 0;\
         var hits = 0; var hitX = -1; var hitY = -1; var hitFlag = -1;\
         l.onHitTest = function(x, y, hit) { hits = hits + 1; hitX = x; hitY = y; hitFlag = hit; };",
    );

    assert_eq!(
        env.eval_int("l.getLayerAt(10, 10) === l"),
        1,
        "the hit test finds the layer"
    );
    assert_eq!(
        env.eval_int("hits"),
        1,
        "the handler runs once per hit test"
    );
    assert_eq!(env.eval_int("hitX"), 10, "the point is the layer's own");
    assert_eq!(env.eval_int("hitY"), 10, "the point is the layer's own");
    assert_eq!(env.eval_int("hitFlag"), 1, "the third argument is true");
}

/// The native member sets the layer's hit flag from its third argument, and
/// the hit test's result is the flag after the event.
///
/// Reference `tTJSNI_BaseLayer::HitTestNoVisibleCheck`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3286-3309`): the flag starts
/// accepted, the event may clear it, and `res = OnHitTest_Work` is the hit
/// test's result. The native member stores its third argument's truthiness
/// as the flag (`reference/cpp/core/visual/LayerIntf.cpp:9944-9968`), so a
/// handler that calls it with `false` rejects the hit and the search
/// continues behind the layer.
#[test]
fn the_hit_test_result_is_the_flag_after_the_event() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window();\
         w.setSize(320, 240);\
         var base = new Layer(w, null);\
         var back = new Layer(w, base);\
         back.setPos(0, 0); back.setSize(100, 50); back.visible = true; back.hitThreshold = 0;\
         class VetoLayer extends Layer {\
             function VetoLayer(w, parent) { super.Layer(w, parent); }\
             function onHitTest(x, y, hit) { super.onHitTest(x, y, false); }\
         }\
         var front = new VetoLayer(w, base);\
         front.setPos(0, 0); front.setSize(100, 50); front.visible = true; front.hitThreshold = 0;",
    );

    assert_eq!(
        env.eval_int("front.getLayerAt(10, 10) === back"),
        1,
        "a layer whose hit flag was cleared is skipped, and the layer behind it is found"
    );
    assert_eq!(
        env.eval_int("back.getLayerAt(10, 10) === back"),
        1,
        "a layer without a handler keeps its hit: the native member sets the flag from its third argument"
    );
    // The native member keeps its three-argument contract.
    assert!(
        env.error("back.onHitTest(1, 2)")
            .contains("requires 3 arguments"),
        "the native member rejects a call with fewer than three arguments"
    );
}

/// A mouse event on a layer with an `onHitTest` handler still reaches that
/// layer, and the handler ran.
///
/// Reference: the manager hit-tests the layer tree before it fires the
/// pointer event (`reference/cpp/core/visual/LayerManager.cpp:374-422`), and
/// the hit test posts `onHitTest` to the hit layer
/// (`reference/cpp/core/visual/LayerIntf.cpp:3286-3309`).
#[test]
fn the_input_path_runs_the_on_hit_test_handler() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window();\
         w.setSize(320, 240);\
         var base = new Layer(w, null);\
         var l = new Layer(w, base);\
         l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.focusable = true; l.hitThreshold = 0;\
         var hits = 0; var down = 0;\
         l.onHitTest = function(x, y, hit) { hits = hits + 1; };\
         l.onMouseDown = function(x, y, btn, shift) { down = down + 1; };",
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 10.0,
        y: 10.0,
    })]);
    assert_eq!(env.eval_int("hits"), 1, "the hit test runs the handler");

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: true,
    })]);
    assert_eq!(env.eval_int("down"), 1, "the press still reaches the layer");
}

/// A handler that throws does not break the hit test.
///
/// The reference delivers the immediate event inside a handler that shows the
/// script exception and continues
/// (`reference/cpp/core/base/EventIntf.cpp:215-233`); the event adapter's
/// rule is the same: a throwing handler is logged and never aborts the
/// caller.
#[test]
fn a_throwing_on_hit_test_handler_does_not_break_the_hit_test() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window();\
         w.setSize(320, 240);\
         var base = new Layer(w, null);\
         var l = new Layer(w, base);\
         l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.hitThreshold = 0;\
         l.onHitTest = function(x, y, hit) { throw 'boom'; };",
    );

    assert_eq!(
        env.eval_int("l.getLayerAt(10, 10) === l"),
        1,
        "the hit test survives a throwing handler"
    );
}
