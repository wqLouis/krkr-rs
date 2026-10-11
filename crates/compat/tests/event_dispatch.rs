//! Integration tests for the L3 [`compat::EventAdapter`] delivery seam.
//!
//! A bound VM + host + attached L2 scene is driven through the same
//! `begin_input_frame` → `apply_input_event`/`map`/`dispatch` →
//! `end_input_frame` sequence the VM host's frame body uses, and the script's
//! `on*` handlers record what ran.

mod common;

use api::host::TouchPhase;
use common::{Env, vm_lock};
use platform::{InputEvent, MouseButton, PhysicalKey, PlatformEvent};

/// A pointer press is delivered to the layer under the cursor with the
/// reference positional arguments `(x, y, button, shift)`, and the release
/// follows the mouse capture.
///
/// Reference: `FireMouseDown`/`FireMouseUp` post
/// `onMouseDown`/`onMouseUp` with `(x, y, mb, flags)`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3410-3437`); the manager
/// hit-tests the layer tree and captures the hit layer
/// (`reference/cpp/core/visual/LayerManager.cpp:374-422`).
#[test]
fn dispatch_delivers_pointer_events_to_the_hit_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.focusable = true; l.hitThreshold = 0; \
         var down = 0; var up = 0; var button = -1; var downX = -1; var downY = -1; var downShift = -1; \
         l.onMouseDown = function(x, y, btn, shift) { down = down + 1; button = btn; downX = x; downY = y; downShift = shift; }; \
         l.onMouseUp = function(x, y, btn, shift) { up = up + 1; };",
    );

    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
    ]);
    // reference/cpp/core/visual/LayerIntf.cpp:3410-3423
    assert_eq!(env.eval_int("down"), 1);
    assert_eq!(env.eval_int("button"), 0);
    assert_eq!(env.eval_int("downX"), 10);
    assert_eq!(env.eval_int("downY"), 10);
    // The reference ORs the held mouse button into the shift mask
    // (`ssLeft` = `TVP_SS_LEFT` = 0x08):
    // reference/cpp/core/environ/impl/TVPWindow.cpp:531-556
    assert_eq!(env.eval_int("downShift"), 8);

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: false,
    })]);
    // reference/cpp/core/visual/LayerManager.cpp:401-422
    assert_eq!(env.eval_int("up"), 1);
}

/// A layer handler receives the pointer position in the layer's own
/// coordinate space; the window handler keeps the window position.
///
/// The manager converts the primary (window) point with
/// `FromPrimaryCoordinates` before firing the layer event
/// (`reference/cpp/core/visual/LayerManager.cpp:380-382`,
/// `reference/cpp/core/visual/LayerManager.cpp:508-509`), which subtracts
/// each non-primary ancestor's rect offset
/// (`reference/cpp/core/visual/LayerIntf.cpp:2282-2291`); the window event
/// is fired with the untouched window point
/// (`reference/cpp/core/visual/WindowIntf.cpp:335-345`).
///
/// A drag-based slider computes its value from this local x, so a
/// window-space x makes every drag clamp to the maximum.
#[test]
fn dispatch_delivers_layer_local_pointer_coordinates() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var base = new Layer(w, null); \
         var outer = new Layer(w, base); \
         outer.setPos(40, 30); outer.setSize(200, 100); outer.visible = true; outer.hitThreshold = 0; \
         var inner = new Layer(w, outer); \
         inner.setPos(60, 20); inner.setSize(100, 50); inner.visible = true; inner.hitThreshold = 0; \
         var downX = -1; var downY = -1; var moveX = -1; var moveY = -1; \
         var clickX = -1; var clickY = -1; var windowX = -1; var windowY = -1; \
         inner.onMouseDown = function(x, y, btn, shift) { downX = x; downY = y; }; \
         inner.onMouseMove = function(x, y, shift) { moveX = x; moveY = y; }; \
         inner.onClick = function(x, y) { clickX = x; clickY = y; }; \
         w.onMouseDown = function(x, y, btn, shift) { windowX = x; windowY = y; };",
    );

    // The window point (110, 70) is (110 - 40 - 60, 70 - 30 - 20) = (10, 20)
    // in `inner`.
    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 110.0,
        y: 70.0,
    })]);
    // reference/cpp/core/visual/LayerManager.cpp:508-509
    assert_eq!(env.eval_int("moveX"), 10);
    assert_eq!(env.eval_int("moveY"), 20);

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: true,
    })]);
    // reference/cpp/core/visual/LayerManager.cpp:380-382
    assert_eq!(env.eval_int("downX"), 10);
    assert_eq!(env.eval_int("downY"), 20);
    // reference/cpp/core/visual/WindowIntf.cpp:335-345
    assert_eq!(env.eval_int("windowX"), 110);
    assert_eq!(env.eval_int("windowY"), 70);

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::MouseButton {
        button: MouseButton::Left,
        pressed: false,
    })]);
    // reference/cpp/core/visual/LayerManager.cpp:361-362
    assert_eq!(env.eval_int("clickX"), 10);
    assert_eq!(env.eval_int("clickY"), 20);
}

/// Key events go to the window's focused layer with the reference positional
/// arguments `(key, shift, process)`; a window activation event goes to the
/// window object.
///
/// Reference: `FireKeyDown` posts `onKeyDown` with `(key, shift, true)`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3844-3854`) and
/// `PrimaryKeyDown` delivers to `FocusedLayer`
/// (`reference/cpp/core/visual/LayerManager.cpp:1010-1022`); the OS
/// activation event is posted to the window owner as `onActivate`
/// (`reference/cpp/core/visual/WindowIntf.cpp:556-570`).
#[test]
fn dispatch_delivers_keys_to_the_focused_layer_and_focus_to_the_window() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.focusable = true; l.visible = true; \
         var keyDown = 0; var keyCode = -1; var keyShift = -1; var keyProcess = -1; \
         l.onKeyDown = function(key, shift, process) { keyDown = keyDown + 1; keyCode = key; keyShift = shift; keyProcess = process; }; \
         var activated = 0; \
         w.onActivate = function() { activated = activated + 1; }; \
         w.focusedLayer = l;",
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::Key {
        code: PhysicalKey::Enter,
        pressed: true,
    })]);
    // reference/cpp/core/visual/LayerIntf.cpp:3844-3854
    assert_eq!(env.eval_int("keyDown"), 1);
    assert_eq!(env.eval_int("keyCode"), 13);
    assert_eq!(env.eval_int("keyShift"), 0);
    assert_eq!(env.eval_int("keyProcess"), 1);

    env.dispatch_events(&[PlatformEvent::Focused(true)]);
    // reference/cpp/core/visual/WindowIntf.cpp:556-570
    assert_eq!(env.eval_int("activated"), 1);
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
         a.onMouseMove = function(x, y, shift) { aMoves = aMoves + 1; }; \
         b.onMouseMove = function(x, y, shift) { bMoves = bMoves + 1; }; \
         a.onMouseUp = function(x, y, btn, shift) { aUps = aUps + 1; };",
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
         l.onMouseMove = function(x, y, shift) { moves = moves + 1; };",
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
         l.onMouseDown = function(x, y, btn, shift) { entered = entered + 1; throw 'boom'; };",
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

/// An invalidated layer is detached from the layer tree, so it can neither
/// receive an event nor shadow the live layer beneath it.
///
/// Reference: `tTJSNI_BaseLayer::Invalidate` sets `Shutdown` and parts the
/// layer from its manager (`reference/cpp/core/visual/LayerIntf.cpp:515-556`),
/// and `FireMouseMove` refuses to dispatch while `Shutdown` is set
/// (`reference/cpp/core/visual/LayerIntf.cpp:3438-3444`); the manager's
/// hit-test then finds the layer under the cursor
/// (`reference/cpp/core/visual/LayerManager.cpp:345-372`).
#[test]
fn dispatch_skips_an_invalidated_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var lower = new Layer(w, null); \
         lower.setPos(0, 0); lower.setSize(100, 100); lower.visible = true; lower.hitThreshold = 0; \
         var upper = new Layer(w, null); \
         upper.setPos(0, 0); upper.setSize(100, 100); upper.visible = true; upper.hitThreshold = 0; \
         var lowerMoves = 0; var upperMoves = 0; \
         lower.onMouseMove = function(x, y, shift) { lowerMoves = lowerMoves + 1; }; \
         upper.onMouseMove = function(x, y, shift) { upperMoves = upperMoves + 1; }; \
         invalidate upper;",
    );

    // `invalidate` detaches the layer from the L2 scene entirely.
    // reference/cpp/core/visual/LayerIntf.cpp:515-556
    assert_eq!(
        env.scene.layers.len(),
        1,
        "the invalidated layer is detached"
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 10.0,
        y: 10.0,
    })]);
    // reference/cpp/core/visual/LayerIntf.cpp:3438-3444
    assert_eq!(env.eval_int("upperMoves"), 0);
    // reference/cpp/core/visual/LayerManager.cpp:345-372
    assert_eq!(env.eval_int("lowerMoves"), 1);
}

/// Committed text posts `onKeyPress(key, process)` to the focused layer and
/// the window, and the window native forwards the event dictionary to
/// `action`.
///
/// Reference: `FireKeyPress` posts `(char, true)`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3868-3877`) and `OnKeyPress`
/// posts `onKeyPress(key)` to the window (`reference/cpp/core/visual/WindowIntf.cpp:509-525`),
/// whose native calls `objthis.action(event)`
/// (`reference/cpp/core/base/EventIntf.h:208-238`).
#[test]
fn dispatch_delivers_text_as_on_key_press_to_layer_and_window() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.focusable = true; l.visible = true; \
         w.focusedLayer = l; \
         var layerPress = 0; var layerKey = ''; var layerProcess = -1; \
         l.onKeyPress = function(key, process) { layerPress = layerPress + 1; layerKey = '' + key; layerProcess = process; }; \
         var actionCalls = 0; var actionType = ''; var actionKey = ''; var actionTarget = 0; \
         w.action = function(ev) { actionCalls = actionCalls + 1; actionType = '' + ev.type; actionKey = '' + ev.key; if (ev.target === w) actionTarget = 1; };",
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::Text("a".into()))]);

    // reference/cpp/core/visual/LayerIntf.cpp:3868-3877
    assert_eq!(env.eval_int("layerPress"), 1);
    assert_eq!(env.eval_int("layerKey === 'a'"), 1);
    assert_eq!(env.eval_int("layerProcess"), 1);
    // reference/cpp/core/visual/WindowIntf.cpp:509-525
    assert_eq!(env.eval_int("actionCalls"), 1);
    assert_eq!(env.eval_int("actionType === 'onKeyPress'"), 1);
    assert_eq!(env.eval_int("actionKey === 'a'"), 1);
    assert_eq!(env.eval_int("actionTarget"), 1);
}

/// A mouse release inside the click sequence posts `onClick`, and the second
/// release of the sequence posts `onDoubleClick`, to the hit layer and to the
/// window's `action`.
///
/// Reference: `PrimaryClick`/`PrimaryDoubleClick` fire the layer events
/// (`reference/cpp/core/visual/LayerManager.cpp:358-372`) and `OnClick`
/// posts the window event (`reference/cpp/core/visual/WindowIntf.cpp:311-322`).
#[test]
fn dispatch_synthesises_click_and_double_click_on_release() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); w.setSize(320, 240); \
         var l = new Layer(w, null); l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.hitThreshold = 0; \
         var clicks = 0; var doubles = 0; \
         l.onClick = function(x, y) { clicks = clicks + 1; }; \
         l.onDoubleClick = function(x, y) { doubles = doubles + 1; }; \
         var actionClicks = 0; var actionDoubles = 0; \
         w.action = function(ev) { if (ev.type === 'onClick') actionClicks = actionClicks + 1; if (ev.type === 'onDoubleClick') actionDoubles = actionDoubles + 1; };",
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
    // reference/cpp/core/visual/LayerManager.cpp:358-364
    assert_eq!(env.eval_int("clicks"), 1);
    assert_eq!(env.eval_int("doubles"), 0);
    assert_eq!(env.eval_int("actionClicks"), 1);
    assert_eq!(env.eval_int("actionDoubles"), 0);

    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    // reference/cpp/core/visual/LayerManager.cpp:366-372
    assert_eq!(env.eval_int("clicks"), 1);
    assert_eq!(env.eval_int("doubles"), 1);
    assert_eq!(env.eval_int("actionDoubles"), 1);
}

/// The hit-test transition fires `onMouseLeave` on the previous layer and
/// `onMouseEnter` on the new one.
///
/// Reference: `LastMouseMoveSent->FireMouseLeave()` then `FireMouseEnter`
/// (`reference/cpp/core/visual/LayerManager.cpp:437-457`).
#[test]
fn dispatch_tracks_layer_enter_and_leave() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); w.setSize(320, 240); \
         var a = new Layer(w, null); a.setPos(0, 0); a.setSize(50, 50); a.visible = true; a.hitThreshold = 0; \
         var b = new Layer(w, null); b.setPos(60, 0); b.setSize(50, 50); b.visible = true; b.hitThreshold = 0; \
         var aEnter = 0; var aLeave = 0; var bEnter = 0; \
         a.onMouseEnter = function() { aEnter = aEnter + 1; }; \
         a.onMouseLeave = function() { aLeave = aLeave + 1; }; \
         b.onMouseEnter = function() { bEnter = bEnter + 1; };",
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 10.0,
        y: 10.0,
    })]);
    // reference/cpp/core/visual/LayerManager.cpp:440-457
    assert_eq!(env.eval_int("aEnter"), 1);
    assert_eq!(env.eval_int("aLeave"), 0);

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 70.0,
        y: 10.0,
    })]);
    // reference/cpp/core/visual/LayerManager.cpp:437-457
    assert_eq!(env.eval_int("aLeave"), 1);
    assert_eq!(env.eval_int("bEnter"), 1);
}

/// A touch contact posts the layer touch handler with
/// `(x, y, cx, cy, id)`.
///
/// Reference: `FireTouchDown` posts `onTouchDown` with `(x, y, cx, cy, id)`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3479-3487`), and the manager
/// hit-tests the layer at the touch point
/// (`reference/cpp/core/visual/LayerManager.cpp:516-524`).
#[test]
fn dispatch_delivers_touch_to_the_hit_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); w.setSize(320, 240); \
         var l = new Layer(w, null); l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.hitThreshold = 0; \
         var downs = 0; var tx = -1.0; var ty = -1.0; var tid = -1; \
         l.onTouchDown = function(x, y, cx, cy, id) { downs = downs + 1; tx = x; ty = y; tid = id; };",
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::Touch {
        id: 3,
        phase: TouchPhase::Started,
        x: 10.0,
        y: 20.0,
    })]);
    // reference/cpp/core/visual/LayerIntf.cpp:3479-3487
    assert_eq!(env.eval_int("downs"), 1);
    assert_eq!(env.eval_real("tx"), 10.0);
    assert_eq!(env.eval_real("ty"), 20.0);
    assert_eq!(env.eval_int("tid"), 3);
}

/// The window native handlers forward to `action` with the reference event
/// dictionary: `type` (the handler name), `target` (the window object) and the
/// named members taken from the positional arguments.
///
/// Reference: `OnMouseDown` posts `onMouseDown` to the window
/// (`reference/cpp/core/visual/WindowIntf.cpp:331-345`), whose native builds
/// `TVPCreateEventObject` + `TVP_ACTION_INVOKE_MEMBER` and calls
/// `objthis.action(event)` (`reference/cpp/core/base/EventIntf.h:208-238`).
#[test]
fn dispatch_forwards_window_input_to_action_with_the_event_dictionary() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); w.setSize(320, 240); \
         var l = new Layer(w, null); l.setPos(0, 0); l.setSize(100, 50); l.visible = true; l.hitThreshold = 0; \
         var type = ''; var x = -1; var y = -1; var button = -1; var shift = -1; var target = 0; \
         w.action = function(ev) { type = '' + ev.type; x = ev.x; y = ev.y; button = ev.button; shift = ev.shift; if (ev.target === w) target = 1; };",
    );

    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 10.0, y: 10.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
    ]);
    // reference/cpp/core/visual/WindowIntf.cpp:331-345
    assert_eq!(env.eval_int("type === 'onMouseDown'"), 1);
    assert_eq!(env.eval_int("x"), 10);
    assert_eq!(env.eval_int("y"), 10);
    assert_eq!(env.eval_int("button"), 0);
    // `TVP_SS_LEFT` (0x08) from the held left button
    // reference/cpp/core/environ/impl/TVPWindow.cpp:531-556
    assert_eq!(env.eval_int("shift"), 8);
    assert_eq!(env.eval_int("target"), 1);
}

/// A physical Enter key reaches the script's `onKeyDown` as `VK_RETURN`
/// (`0x0D`), the code the shipped `system/window.tjs` compares against.
///
/// Reference: the platform key conversion maps the enter key to `VK_RETURN`
/// (`reference/cpp/core/environ/cocos2d/CCKeyCodeConv.cpp:142`) and
/// `FireKeyDown` posts the code with `(key, shift, process)`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3844-3854`).
#[test]
fn dispatch_delivers_vk_return_to_on_key_down() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.focusable = true; l.visible = true; \
         w.focusedLayer = l; \
         var key = -1; \
         l.onKeyDown = function(k, shift, process) { key = k; };",
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::Key {
        code: PhysicalKey::Enter,
        pressed: true,
    })]);
    // reference/cpp/core/base/ScriptMgnIntf.cpp:180-198
    assert_eq!(env.eval_int("key"), 0x0D);
}

/// Holding a modifier sets its `TVP_SS_*` bit in the `shift` argument the
/// input handlers receive.
///
/// Reference: `TVPGetCurrentShiftKeyState` probes `VK_SHIFT`/`VK_CONTROL`/
/// `VK_MENU` (`reference/cpp/core/environ/impl/TVPWindow.cpp:573`) and the
/// bits are `ssShift`/`ssAlt`/`ssCtrl`
/// (`reference/cpp/core/base/ScriptMgnIntf.cpp:180-198`).
#[test]
fn dispatch_reports_the_shift_mask_while_modifiers_are_held() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.focusable = true; l.visible = true; \
         w.focusedLayer = l; \
         var shift = -1; \
         l.onKeyDown = function(k, s, process) { shift = s; };",
    );

    // `TVP_SS_SHIFT` (0x01) while shift is held.
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::Key {
            code: PhysicalKey::ShiftLeft,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::Key {
            code: PhysicalKey::KeyA,
            pressed: true,
        }),
    ]);
    // reference/cpp/core/base/ScriptMgnIntf.cpp:180-198
    assert_ne!(env.eval_int("shift") & 0x01, 0);

    // `TVP_SS_CTRL` (0x04) while control is held.
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::Key {
            code: PhysicalKey::ShiftLeft,
            pressed: false,
        }),
        PlatformEvent::Input(InputEvent::Key {
            code: PhysicalKey::ControlLeft,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::Key {
            code: PhysicalKey::KeyA,
            pressed: true,
        }),
    ]);
    // reference/cpp/core/base/ScriptMgnIntf.cpp:180-198
    assert_ne!(env.eval_int("shift") & 0x04, 0);

    // `TVP_SS_ALT` (0x02) while alt is held.
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::Key {
            code: PhysicalKey::ControlLeft,
            pressed: false,
        }),
        PlatformEvent::Input(InputEvent::Key {
            code: PhysicalKey::AltLeft,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::Key {
            code: PhysicalKey::KeyA,
            pressed: true,
        }),
    ]);
    // reference/cpp/core/base/ScriptMgnIntf.cpp:180-198
    assert_ne!(env.eval_int("shift") & 0x02, 0);
}

/// One wheel notch is posted as the reference `WHEEL_DELTA` (`±120`).
///
/// Reference: `TVPWindowLayer::onMouseScroll` converts a scroll to `±120`
/// (`reference/cpp/core/environ/impl/TVPWindow.cpp:109-115`).
#[test]
fn dispatch_scales_the_wheel_delta_to_the_reference_notches() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.focusable = true; l.visible = true; \
         w.focusedLayer = l; \
         var delta = 0; \
         l.onMouseWheel = function(shift, d, x, y) { delta = d; };",
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::Wheel { delta: 1.0 })]);
    // reference/cpp/core/environ/impl/TVPWindow.cpp:109-115
    assert_eq!(env.eval_int("delta"), 120);

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::Wheel { delta: -1.0 })]);
    // reference/cpp/core/environ/impl/TVPWindow.cpp:109-115
    assert_eq!(env.eval_int("delta"), -120);
}
