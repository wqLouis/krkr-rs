//! Reference-behaviour tests for the layer focus events: `onBlur`/`onFocus`
//! fire synchronously on every focus transition, with the argument objects
//! and directions the reference passes, and never on a mouse click.

mod common;

use common::{Env, vm_lock};
use platform::{InputEvent, MouseButton, PlatformEvent};
use tjs::TjsValue;

/// `layer.focus()` with no previous holder fires only `onFocus(null, true)`:
/// `org` is null so `FireBlur` is skipped
/// (`reference/cpp/core/visual/LayerManager.cpp:782-790`), and the event
/// carries the previously focused layer or `null` then the direction
/// (`reference/cpp/core/visual/LayerIntf.cpp:3715-3727`).
#[test]
fn focus_from_nothing_fires_only_on_focus_with_null_and_direction() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; a.focusable = true; a.visible = true; \
         var evlog = ''; \
         a.onBlur = function(focused) { evlog += 'blur:' + (focused === null ? 'null' : focused.name) + ';'; }; \
         a.onFocus = function(blurred, direction) { evlog += 'focus:' + (blurred === null ? 'null' : blurred.name) + ':' + direction + ';'; }; \
         a.focus();",
    );
    assert_eq!(
        env.eval_ok("evlog"),
        TjsValue::String("focus:null:1;".into())
    );
    assert_eq!(env.eval_int("a.focused"), 1);
}

/// Moving focus from A to B fires A's `onBlur` **before** B's `onFocus`
/// (`reference/cpp/core/visual/LayerManager.cpp:786-790`); A's `onBlur`
/// argument is the **incoming** layer (`:787`, member `focused` at
/// `reference/cpp/core/visual/LayerIntf.cpp:10199-10200`) and B's `onFocus`
/// first argument is the **outgoing** layer (member `blurred` at `:10215`).
#[test]
fn moving_focus_fires_blur_on_the_outgoing_then_focus_on_the_incoming() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; a.focusable = true; a.visible = true; \
         var b = new Layer(w, null); b.name = 'b'; b.focusable = true; b.visible = true; \
         var evlog = ''; \
         a.onBlur = function(focused) { evlog += 'a.blur:' + (focused === b ? 'b' : 'other') + ';'; }; \
         a.onFocus = function(blurred, direction) { evlog += 'a.focus:' + (blurred === null ? 'null' : blurred.name) + ':' + direction + ';'; }; \
         b.onBlur = function(focused) { evlog += 'b.blur:' + (focused === null ? 'null' : focused.name) + ';'; }; \
         b.onFocus = function(blurred, direction) { evlog += 'b.focus:' + (blurred === a ? 'a' : 'other') + ':' + direction + ';'; }; \
         a.focus(); \
         evlog = ''; \
         b.focus();",
    );
    assert_eq!(
        env.eval_ok("evlog"),
        TjsValue::String("a.blur:b;b.focus:a:1;".into())
    );
}

/// A layer that is not node-focusable cannot become focused by `focus()`:
/// `SetFocusTo` returns false before any state change or event
/// (`reference/cpp/core/visual/LayerManager.cpp:766-767`); `focusable`
/// defaults to false (`reference/cpp/core/visual/LayerIntf.cpp:355`) and
/// `GetNodeFocusable` requires it plus visibility and enablement
/// (`reference/cpp/core/visual/LayerIntf.h:663-667`).
#[test]
fn focus_on_a_non_focusable_layer_fires_nothing_and_moves_nothing() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); \
         var hits = 0; \
         a.onFocus = function() { hits++; }; \
         a.onBlur = function() { hits++; }; \
         a.focus();",
    );
    assert_eq!(env.eval_int("hits"), 0);
    assert_eq!(env.eval_int("a.focused"), 0);
    assert_eq!(env.eval_int("w.focusedLayer === null"), 1);
}

/// `focusNext()` moves focus with direction `true`, `focusPrev()` with
/// `false` (`reference/cpp/core/visual/LayerManager.cpp:723-747`), and
/// `focus(false)` passes its argument through
/// (`reference/cpp/core/visual/LayerIntf.cpp:9726-9742`); both natives
/// return the layer object (`LayerIntf.cpp:9744-9778`).
#[test]
fn focus_directions_reach_the_event_as_reference_booleans() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; a.focusable = true; a.visible = true; \
         var b = new Layer(w, null); b.name = 'b'; b.focusable = true; b.visible = true; \
         var evlog = ''; \
         a.onFocus = function(blurred, direction) { evlog += 'a.focus:' + direction + ';'; }; \
         b.onFocus = function(blurred, direction) { evlog += 'b.focus:' + direction + ';'; }; \
         a.focus(); \
         var next = a.focusNext(); \
         var prev = b.focusPrev(); \
         var stage1 = evlog; \
         evlog = ''; \
         b.focus(false); \
         var stage2 = evlog;",
    );
    assert_eq!(
        env.eval_ok("stage1"),
        TjsValue::String("a.focus:1;b.focus:1;a.focus:0;".into())
    );
    assert_eq!(env.eval_ok("stage2"), TjsValue::String("b.focus:0;".into()));
    assert_eq!(env.eval_int("next === b"), 1);
    assert_eq!(env.eval_int("prev === a"), 1);
}

/// `SetVisible(false)` blurs the focused subtree through `BlurTree`
/// (`reference/cpp/core/visual/LayerIntf.cpp:1183-1206`), which moves focus
/// to `root->GetNextFocusable()` with direction forward
/// (`reference/cpp/core/visual/LayerManager.cpp:690-711`).
#[test]
fn hiding_the_focused_layer_blurs_to_the_next_focusable() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; a.focusable = true; a.visible = true; \
         var b = new Layer(w, null); b.name = 'b'; b.focusable = true; b.visible = true; \
         var evlog = ''; \
         a.onBlur = function(focused) { evlog += 'a.blur:' + (focused === null ? 'null' : focused.name) + ';'; }; \
         b.onFocus = function(blurred, direction) { evlog += 'b.focus:' + (blurred === null ? 'null' : blurred.name) + ':' + direction + ';'; }; \
         a.focus(); \
         evlog = ''; \
         a.visible = false;",
    );
    assert_eq!(
        env.eval_ok("evlog"),
        TjsValue::String("a.blur:b;b.focus:a:1;".into())
    );
    assert_eq!(env.eval_int("w.focusedLayer === b"), 1);
}

/// `SetEnabled(false)` blurs the same way
/// (`reference/cpp/core/visual/LayerIntf.cpp:3801-3832`,
/// `LayerManager.cpp:690-711`).
#[test]
fn disabling_the_focused_layer_blurs_to_the_next_focusable() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; a.focusable = true; a.visible = true; \
         var b = new Layer(w, null); b.name = 'b'; b.focusable = true; b.visible = true; \
         var evlog = ''; \
         a.onBlur = function(focused) { evlog += 'a.blur:' + (focused === null ? 'null' : focused.name) + ';'; }; \
         b.onFocus = function(blurred, direction) { evlog += 'b.focus:' + (blurred === null ? 'null' : blurred.name) + ':' + direction + ';'; }; \
         a.focus(); \
         evlog = ''; \
         a.enabled = false;",
    );
    assert_eq!(
        env.eval_ok("evlog"),
        TjsValue::String("a.blur:b;b.focus:a:1;".into())
    );
    assert_eq!(env.eval_int("w.focusedLayer === b"), 1);
}

/// `focusable = false` while focused blurs to `GetNextFocusable()`
/// (`reference/cpp/core/visual/LayerIntf.cpp:3683-3697`).
#[test]
fn unfocusing_the_focused_layer_blurs_to_the_next_focusable() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; a.focusable = true; a.visible = true; \
         var b = new Layer(w, null); b.name = 'b'; b.focusable = true; b.visible = true; \
         var evlog = ''; \
         a.onBlur = function(focused) { evlog += 'a.blur:' + (focused === null ? 'null' : focused.name) + ';'; }; \
         b.onFocus = function(blurred, direction) { evlog += 'b.focus:' + (blurred === null ? 'null' : blurred.name) + ':' + direction + ';'; }; \
         a.focus(); \
         evlog = ''; \
         a.focusable = false;",
    );
    assert_eq!(
        env.eval_ok("evlog"),
        TjsValue::String("a.blur:b;b.focus:a:1;".into())
    );
    assert_eq!(env.eval_int("w.focusedLayer === b"), 1);
}

/// Hiding an **ancestor** of the focused layer blurs it too:
/// `BlurTree(root)` fires when the focused layer is a descendant of `root`
/// (`reference/cpp/core/visual/LayerManager.cpp:697-700`) and, with no
/// other focusable layer, clears focus via `SetFocusTo(nullptr, true)`
/// (`:704-708`), so only `onBlur` fires
/// (`reference/cpp/core/visual/LayerIntf.cpp:3702-3713`).
#[test]
fn hiding_an_ancestor_blurs_the_focused_descendant() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var root = new Layer(w, null); \
         var p = new Layer(w, null); p.name = 'p'; p.visible = true; \
         var a = new Layer(w, p); a.name = 'a'; a.focusable = true; a.visible = true; \
         var hits = 0; \
         a.onBlur = function(focused) { hits++; }; \
         a.onFocus = function() { hits++; }; \
         a.focus(); \
         hits = 0; \
         p.visible = false;",
    );
    assert_eq!(env.eval_int("hits"), 1);
    assert_eq!(env.eval_int("w.focusedLayer === null"), 1);
}

/// Removing the focused layer blurs it while it is still in the tree:
/// `invalidate` → `Part()` → `NotifyPart` → `BlurTree`
/// (`reference/cpp/core/visual/LayerIntf.cpp:624-631`,
/// `reference/cpp/core/visual/LayerManager.cpp:197-201`, `:690-711`), so
/// the outgoing layer's object is still valid inside the handlers.
#[test]
fn removing_the_focused_layer_blurs_to_the_next_focusable() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; a.focusable = true; a.visible = true; \
         var b = new Layer(w, null); b.name = 'b'; b.focusable = true; b.visible = true; \
         var evlog = ''; \
         a.onBlur = function(focused) { evlog += 'a.blur:' + (focused === null ? 'null' : focused.name) + ';'; }; \
         b.onFocus = function(blurred, direction) { evlog += 'b.focus:' + (blurred === null ? 'null' : blurred.name) + ':' + direction + ';'; }; \
         a.focus(); \
         evlog = ''; \
         invalidate a;",
    );
    assert_eq!(
        env.eval_ok("evlog"),
        TjsValue::String("a.blur:b;b.focus:a:1;".into())
    );
    assert_eq!(env.eval_int("w.focusedLayer === b"), 1);
}

/// Re-parenting a focused layer parts it from its old parent
/// (`reference/cpp/core/visual/LayerIntf.cpp:611-631`), and the part blurs
/// it out of the tree (`reference/cpp/core/visual/LayerManager.cpp:197-201`,
/// `:690-711`); with no other focusable layer, focus clears.
#[test]
fn reparenting_the_focused_layer_blurs_it_out_of_the_tree() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var root = new Layer(w, null); \
         var p = new Layer(w, null); p.name = 'p'; p.visible = true; \
         var a = new Layer(w, p); a.name = 'a'; a.focusable = true; a.visible = true; \
         var q = new Layer(w, null); q.name = 'q'; \
         var evlog = ''; \
         a.onBlur = function(focused) { evlog += 'a.blur:' + (focused === null ? 'null' : focused.name) + ';'; }; \
         a.focus(); \
         evlog = ''; \
         a.parent = q;",
    );
    assert_eq!(
        env.eval_ok("evlog"),
        TjsValue::String("a.blur:null;".into())
    );
    assert_eq!(env.eval_int("w.focusedLayer === null"), 1);
}

/// A mouse click never moves layer focus: the reference press path sets the
/// mouse capture and fires `onMouseDown`/`onClick` only
/// (`reference/cpp/core/visual/LayerManager.cpp:358-399`).
#[test]
fn a_mouse_click_never_moves_layer_focus() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var a = new Layer(w, null); a.name = 'a'; a.focusable = true; a.visible = true; \
         a.setSize(320, 240); \
         var b = new Layer(w, null); b.name = 'b'; b.focusable = true; b.visible = true; \
         b.setSize(320, 240); b.hitThreshold = 0; \
         var hits = 0; \
         a.onFocus = function() { hits++; }; a.onBlur = function() { hits++; }; \
         b.onFocus = function() { hits++; }; b.onBlur = function() { hits++; }; \
         var bClicks = 0; \
         b.onClick = function() { bClicks++; }; \
         a.focus(); \
         hits = 0;",
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
    // The click reached b…
    assert_eq!(env.eval_int("bClicks"), 1);
    // …but focus did not move and no focus event fired.
    assert_eq!(env.eval_int("hits"), 0);
    assert_eq!(env.eval_int("w.focusedLayer === a"), 1);
}

/// Creating layers and turning `focusable`/`visible`/`enabled` on never
/// auto-focuses: `CheckTreeFocusableState`'s body is commented out
/// (`reference/cpp/core/visual/LayerManager.cpp:713-721`) and
/// `SetFocusable(true)` does nothing
/// (`reference/cpp/core/visual/LayerIntf.cpp:3683-3697`).
#[test]
fn enabling_and_showing_layers_never_auto_focuses() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var root = new Layer(w, null); \
         var a = new Layer(w, null); a.name = 'a'; \
         var hits = 0; \
         a.onFocus = function() { hits++; }; \
         a.onBlur = function() { hits++; }; \
         a.focusable = true; \
         a.visible = true; \
         a.enabled = true;",
    );
    assert_eq!(env.eval_int("hits"), 0);
    assert_eq!(env.eval_int("w.focusedLayer === null"), 1);
}
