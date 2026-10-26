//! Behaviour tests for the layer hit threshold and the window's screen root.
//!
//! The reference gives the screen root a hit threshold of zero, so it takes
//! every pixel of its area, and every other layer the constructor's
//! threshold of 16, so its hit test reads the image's alpha. The screen root
//! is the last candidate of the hit test, so a click that misses every layer
//! above it lands on the root.

mod common;

use common::{Env, vm_lock};
use platform::{InputEvent, PlatformEvent};

/// The screen root takes every pixel; a layer with a parent keeps the
/// constructor's threshold.
///
/// Reference `tTJSNI_Layer::Construct` gives the layer that has no parent
/// the primary-layer defaults, the hit threshold among them
/// (`reference/cpp/core/visual/LayerIntf.cpp:463-472`): the screen root's
/// `HitThreshold` is 0. Every other layer keeps the constructor's default
/// (`reference/cpp/core/visual/LayerIntf.cpp:344-345`): `HitThreshold = 16`.
#[test]
fn the_screen_root_takes_every_pixel_and_a_child_keeps_the_default_threshold() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window();\
         var primary = new Layer(w, null);\
         var child = new Layer(w, primary);\
         var second = new Layer(w, null);",
    );

    // reference/cpp/core/visual/LayerIntf.cpp:472
    assert_eq!(env.eval_int("primary.hitThreshold"), 0);
    // reference/cpp/core/visual/LayerIntf.cpp:345
    assert_eq!(env.eval_int("child.hitThreshold"), 16);
    // A second parentless layer keeps the constructor's threshold: the
    // reference gives every parentless root its own manager, and the second
    // one raises in the draw device before the threshold is set
    // (`reference/cpp/core/visual/BasicDrawDevice.cpp:478-487`).
    assert_eq!(env.eval_int("second.hitThreshold"), 16);
}

/// A click that misses every layer above the screen root lands on the root.
///
/// Reference `tTJSNI_BaseLayer::GetMostFrontChildAt`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3331-3357`): the children are
/// tried front to back and the node itself last, so the screen root is the
/// last candidate of the search. Its threshold of zero
/// (`reference/cpp/core/visual/LayerIntf.cpp:3239-3240`) makes the hit test
/// accept the point without reading the image's alpha.
#[test]
fn a_click_that_misses_every_layer_lands_on_the_screen_root() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window();\
         w.setSize(320, 240);\
         var primary = new Layer(w, null);\
         primary.setSize(320, 240);\
         var over = new Layer(w, primary);\
         over.setPos(0, 0); over.setSize(50, 50); over.visible = true; over.hitThreshold = 0;\
         var plain = new Layer(w, primary);\
         plain.setPos(100, 100); plain.setSize(50, 50); plain.visible = true;\
         var rootMoves = 0; var overMoves = 0; var plainMoves = 0; var rootX = -1; var rootY = -1;\
         primary.onMouseMove = function(x, y, shift) { rootMoves = rootMoves + 1; rootX = x; rootY = y; };\
         over.onMouseMove = function(x, y, shift) { overMoves = overMoves + 1; };\
         plain.onMouseMove = function(x, y, shift) { plainMoves = plainMoves + 1; };",
    );

    // The layer with the zero threshold takes the point.
    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 10.0,
        y: 10.0,
    })]);
    assert_eq!(env.eval_int("overMoves"), 1);
    assert_eq!(env.eval_int("rootMoves"), 0);

    // The layer with the default threshold reads the alpha of an image it
    // does not have, so it misses and the search continues to the root.
    // reference/cpp/core/visual/LayerIntf.cpp:3250-3261
    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 110.0,
        y: 110.0,
    })]);
    assert_eq!(env.eval_int("plainMoves"), 0);
    assert_eq!(env.eval_int("rootMoves"), 1);

    // An empty part of the screen lands on the root as well, with the
    // window's coordinates (the root sits at the window's origin).
    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 200.0,
        y: 200.0,
    })]);
    assert_eq!(env.eval_int("rootMoves"), 2);
    assert_eq!(env.eval_int("rootX"), 200);
    assert_eq!(env.eval_int("rootY"), 200);
}

/// The hit test finds the screen root where it found nothing.
///
/// Reference `tTJSNI_BaseLayer::GetMostFrontChildAt`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3312-3360`): the root is the last
/// candidate of the search, and its threshold of zero
/// (`reference/cpp/core/visual/LayerIntf.cpp:3239-3240`) makes it accept the
/// point.
#[test]
fn the_hit_test_finds_the_screen_root_where_it_found_nothing() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window();\
         w.setSize(320, 240);\
         var primary = new Layer(w, null);\
         primary.setSize(320, 240);\
         var plain = new Layer(w, primary);\
         plain.setPos(100, 100); plain.setSize(50, 50); plain.visible = true;",
    );

    // The plain layer's default threshold reads the alpha of an image it does
    // not have, so the search continues behind it to the root.
    // reference/cpp/core/visual/LayerIntf.cpp:3250-3261
    assert_eq!(
        env.eval_int("plain.getLayerAt(10, 10) === primary"),
        1,
        "the search ends on the screen root"
    );
}
