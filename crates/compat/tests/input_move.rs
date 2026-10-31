//! The mouse-move path: the two rules that bound it.
//!
//! The tests drive the real L3 event adapter (`compat::EventAdapter::dispatch_frame`)
//! through the shared harness, so they assert what a script sees.
//!
//! The reference behaviour:
//!
//! * the window's move event is posted discardable, so a pending move event is
//!   dropped when a newer one arrives and only the newest reaches the window
//!   (`reference/cpp/core/visual/WindowIntf.cpp:361-372`);
//! * the layer's move event fires only when the pointer position changed
//!   (`reference/cpp/core/visual/LayerManager.cpp:426` computes `poschanged`,
//!   `reference/cpp/core/visual/LayerManager.cpp:507` guards the layer's
//!   `FireMouseMove` with it) — and the hit test itself runs on every move
//!   (`reference/cpp/core/visual/LayerManager.cpp:433`).

mod common;

use common::{Env, vm_lock};
use platform::{InputEvent, PlatformEvent};

/// A pointer move at `(x, y)`.
fn moved(x: f32, y: f32) -> PlatformEvent {
    PlatformEvent::Input(InputEvent::PointerMoved { x, y })
}

/// A window and a full-screen layer, each counting its own move deliveries and
/// recording the last position the window saw.
fn scene() -> Env {
    let env = Env::new();
    env.run(
        "var windowMoves = 0; var windowX = -1; var layerMoves = 0; \
         var w = new Window(); \
         w.setSize(640, 480); \
         w.onMouseMove = function(x, y, shift) { \
           windowMoves = windowMoves + 1; windowX = x; }; \
         var l = new Layer(w, null); \
         l.setPos(0, 0); l.setSize(640, 480); l.visible = true; l.hitThreshold = 0; \
         l.onMouseMove = function(x, y, shift) { layerMoves = layerMoves + 1; };",
    );
    env
}

/// A frame of moves at three positions runs the window's handler once, at the
/// newest position.
///
/// The reference posts the window's move event with `TVP_EPT_DISCARDABLE`, so
/// the three platform moves of one frame leave one pending event — the last —
/// and the window's handler sees that position
/// (`reference/cpp/core/visual/WindowIntf.cpp:361-372`).
#[test]
fn one_frame_of_moves_runs_the_window_handler_once_at_the_newest_position() {
    let _lock = vm_lock();
    let env = scene();

    // One frame, three positions: the harness adapter delivers the whole batch
    // the way one frame would.
    env.dispatch_events(&[moved(40.0, 10.0), moved(80.0, 10.0), moved(120.0, 10.0)]);

    assert_eq!(
        env.eval_int("windowMoves"),
        1,
        "the window's discardable move event must deliver once per frame"
    );
    assert_eq!(
        env.eval_int("windowX"),
        120,
        "the window's handler must see the newest position of the frame"
    );
}

/// The layer's handler runs once per changed position, and not at all for a
/// repeated one.
///
/// The reference computes `poschanged` from `LastMouseMoveX/Y` on every move and
/// gates the layer's `FireMouseMove` with it, while the hit test runs either
/// way (`reference/cpp/core/visual/LayerManager.cpp:426`, `:433`, `:507`).
#[test]
fn the_layer_handler_runs_once_per_changed_position() {
    let _lock = vm_lock();
    let env = scene();

    // The first move is a change from "no previous position".
    env.dispatch_events(&[moved(10.0, 10.0)]);
    assert_eq!(env.eval_int("layerMoves"), 1);

    // The same position again delivers no layer event.
    env.dispatch_events(&[moved(10.0, 10.0)]);
    assert_eq!(
        env.eval_int("layerMoves"),
        1,
        "a move that repeats the position must not fire the layer's handler"
    );

    // The hit test still ran for the repeated move, and a new position fires
    // the handler again.
    env.dispatch_events(&[moved(11.0, 10.0)]);
    assert_eq!(
        env.eval_int("layerMoves"),
        2,
        "a move to a new position must fire the layer's handler"
    );
}
