//! Integration tests for the owner of the pointer: the window on top.
//!
//! A scene holds several windows and draws them in the order it holds them,
//! so the last visible window is the one on top. The pointer belongs to that
//! window: the hit test starts there and only looks further down when the
//! window on top has no layer at the point.

mod common;

use common::{Env, vm_lock};
use platform::{InputEvent, MouseButton, PlatformEvent};

/// The click reaches the layer of the window on top, and the window handler
/// of that same window.
///
/// Reference: every window registers its own mouse listener and starts hidden
/// (`reference/cpp/core/environ/impl/TVPWindow.cpp:58-63`); showing a
/// window brings it to the front and moves the window that was in front off
/// the screen (`reference/cpp/core/environ/impl/TVPWindow.cpp:366-415`), so
/// the pointer belongs to the front-most window and to no other.
#[test]
fn a_click_reaches_the_window_on_top() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var under = new Window(); \
         under.visible = true; \
         var underLayer = new Layer(under, null); \
         underLayer.setImageSize(64, 64); \
         underLayer.fillRect(0, 0, 64, 64, 0xff000000); \
         var over = new Window(); \
         over.visible = true; \
         var overLayer = new Layer(over, null); \
         overLayer.setImageSize(64, 64); \
         var button = new Layer(over, overLayer); \
         button.setPos(0, 0); button.setSize(32, 32); \
         button.visible = true; button.hitThreshold = 0; \
         var buttonClicks = 0; var overClicks = 0; var underClicks = 0; \
         button.onMouseUp = function(x, y, btn, shift) { buttonClicks = buttonClicks + 1; }; \
         over.onMouseUp = function(x, y, btn, shift) { overClicks = overClicks + 1; }; \
         under.onMouseUp = function(x, y, btn, shift) { underClicks = underClicks + 1; };",
    );

    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);

    assert_eq!(env.eval_int("buttonClicks"), 1);
    assert_eq!(env.eval_int("overClicks"), 1);
    assert_eq!(env.eval_int("underClicks"), 0);
}

/// A window that is not shown takes no input.
///
/// Reference: a window that is not visible delivers no event
/// (`reference/cpp/core/visual/impl/WindowImpl.cpp:1154-1158`,
/// `CanDeliverEvents` is the visibility and the enabled state), and the
/// hit test only looks at the windows that are drawn.
#[test]
fn a_hidden_window_takes_no_input() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); w.visible = false; \
         var l = new Layer(w, null); \
         l.setImageSize(64, 64); \
         l.setPos(0, 0); l.setSize(32, 32); l.visible = true; l.hitThreshold = 0; \
         var clicks = 0; \
         l.onMouseUp = function(x, y, btn, shift) { clicks = clicks + 1; };",
    );

    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    assert_eq!(env.eval_int("clicks"), 0);

    // Showing the window hands it the pointer.
    env.run("w.visible = true;");
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    assert_eq!(env.eval_int("clicks"), 1);
}

/// Showing a window brings it to the front.
///
/// Reference: `SetVisible(true)` calls `BringToFront`, which makes the window
/// the one the pointer belongs to
/// (`reference/cpp/core/environ/impl/TVPWindow.cpp:405-414`), and a window
/// that is not shown delivers nothing
/// (`reference/cpp/core/visual/impl/WindowImpl.cpp:1154-1158`).
///
/// The front window is the last one the scene holds, so re-showing a window
/// has to move it there; creation order alone is not the reference's rule.
#[test]
fn showing_a_window_again_brings_it_to_the_front() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var first = new Window(); \
         first.visible = true; \
         var firstLayer = new Layer(first, null); \
         firstLayer.setImageSize(64, 64); \
         var firstClicks = 0; \
         first.onMouseUp = function(x, y, btn, shift) { firstClicks = firstClicks + 1; }; \
         var second = new Window(); \
         second.visible = true; \
         var secondLayer = new Layer(second, null); \
         secondLayer.setImageSize(64, 64); \
         var secondClicks = 0; \
         second.onMouseUp = function(x, y, btn, shift) { secondClicks = secondClicks + 1; };",
    );

    // The second window was shown last, so it is on top.
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    assert_eq!(env.eval_int("secondClicks"), 1);
    assert_eq!(env.eval_int("firstClicks"), 0);

    // Showing the first window again brings it to the front.
    env.run("first.visible = false; first.visible = true;");
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    assert_eq!(env.eval_int("firstClicks"), 1);
    assert_eq!(env.eval_int("secondClicks"), 1);
}
