//! The in-game menu click of a KAG 3.28 title, through the whole adapter
//! chain.
//!
//! The title's menu is not the engine's in-game menu form: it is the KAG's
//! `[button]` tag, which creates a `LinkButtonLayer` (a `ButtonLayer`) inside
//! a `MessageLayer`, and the message layer overrides `onHitTest` to become
//! transparent where no link is under the cursor. The click has to travel
//! platform -> event adapter -> hit test -> `onHitTest` -> mouse capture ->
//! `onMouseUp` -> the script's handler -> the action.
//!
//! Reference:
//!   * `system/messagelayer.tjs:12-41` — `LinkButtonLayer.onMouseUp` runs
//!     the parent's `onButtonClick` for a left-button release.
//!   * `system/messagelayer.tjs:1545-1566` — `findLink` finds the link under
//!     the point.
//!   * `system/messagelayer.tjs:1568-1604` — the `[button]` tag's layer.
//!   * `system/messagelayer.tjs:1944-1987` — the message layer's
//!     `internalHitTest` and `onHitTest`: the layer is transparent where no
//!     link is under the cursor.
//!   * `reference/cpp/core/visual/LayerIntf.cpp:3286-3302` — `onHitTest` is
//!     fired on the layer that hits geometrically and the layer's flag
//!     decides whether the hit stands.
//!   * `reference/cpp/core/visual/LayerIntf.cpp:3312-3360` — the children are
//!     tried front to back and the node itself last.
//!   * `reference/cpp/core/visual/LayerManager.cpp:374-422` — the press sets
//!     the mouse capture and the release follows the capture.
//!
//! The adapter is the harness one, which delivers every event it is given
//! (AGENTS §9.15).

mod common;

use common::{Env, vm_lock};
use platform::{InputEvent, MouseButton, PlatformEvent};

/// A click on the KAG's `[button]` layer runs the button's action, a click on
/// the transparent message layer falls through to the background, and a
/// release after the pointer left the button still follows the capture.
#[test]
fn a_kag_button_click_runs_the_action_and_the_capture_holds_the_release() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        // The KAG's `[button]` tag creates a `LinkButtonLayer` inside the
        // message layer (`system/messagelayer.tjs:1568-1604`); the button
        // takes every pixel of its own area (`system/buttonlayer.tjs:35-36`).
        class LinkButtonLayer extends Layer {
            var linkNum;
            function LinkButtonLayer(win, par) {
                super.Layer(win, par);
                focusable = false;
                hitType = 0;
                hitThreshold = 0;
            }
            function onMouseUp(x, y, button, shift) {
                if(enabled && button == 0) parent.onButtonClick(linkNum);
                super.onMouseUp(x, y, button, shift);
            }
        }
        // The message layer finds the link under the point and is transparent
        // where there is none (`system/messagelayer.tjs:1545-1566`,
        // `:1944-1987`).
        class MessageLayer extends Layer {
            var links;
            var clicks;
            var hitTests;
            function MessageLayer(win, par) {
                super.Layer(win, par);
                links = [];
                clicks = 0;
                hitTests = 0;
                hitType = 0;
                hitThreshold = 0;
            }
            function findLink(x, y) {
                for(var i = 0; i < links.length; i++) {
                    var link = links[i];
                    if(link.left <= x && link.top <= y &&
                       link.left + link.width > x && link.top + link.height > y) {
                        return i;
                    }
                }
                return -1;
            }
            function internalHitTest(x, y) {
                return findLink(x, y) != -1;
            }
            function onHitTest(x, y, hit) {
                hitTests = hitTests + 1;
                var accepted = internalHitTest(x - imageLeft, y - imageTop);
                return super.onHitTest(x, y, accepted);
            }
            function onButtonClick(linkNum) {
                clicks = clicks + 1;
            }
        }
        var w = new Window();
        w.setSize(800, 600);
        var base = new Layer(w, null);
        base.setPos(0, 0);
        base.setSize(800, 600);
        base.visible = true;
        var message = new MessageLayer(w, base);
        message.setPos(0, 0);
        message.setSize(800, 600);
        message.visible = true;
        var button = new LinkButtonLayer(w, message);
        button.setPos(100, 100);
        button.setSize(75, 26);
        button.visible = true;
        button.linkNum = 0;
        message.links = [%[left: 100, top: 100, width: 75, height: 26]];
        var windowDowns = 0;
        w.onMouseDown = function(x, y, button, shift) { windowDowns = windowDowns + 1; };
        var baseDowns = 0;
        base.onMouseDown = function(x, y, button, shift) { baseDowns = baseDowns + 1; };
        var messageDowns = 0;
        message.onMouseDown = function(x, y, button, shift) { messageDowns = messageDowns + 1; };
        "#,
    );

    // The hit test finds the button, and the message layer is transparent
    // where no link is under the cursor.
    // reference/cpp/core/visual/LayerIntf.cpp:3286-3302
    assert_eq!(
        env.eval_int("base.getLayerAt(110, 110) === button"),
        1,
        "the hit test finds the button"
    );
    // reference/cpp/core/visual/LayerIntf.cpp:3312-3360
    assert_eq!(
        env.eval_int("base.getLayerAt(300, 300) === base"),
        1,
        "the transparent message layer lets the search continue to the background"
    );
    // The probe at the transparent point ran the message layer's hit test.
    let hit_tests = env.eval_int("message.hitTests");
    assert!(
        hit_tests > 0,
        "the message layer's onHitTest ran for the transparent point"
    );

    // The press and the release at the button's centre.
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 110.0, y: 110.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    // reference/cpp/core/visual/LayerManager.cpp:374-422
    assert_eq!(
        env.eval_int("message.clicks"),
        1,
        "the button's onMouseUp runs the action"
    );
    assert_eq!(
        env.eval_int("messageDowns"),
        0,
        "the message layer does not see the press"
    );
    assert_eq!(
        env.eval_int("baseDowns"),
        0,
        "the background does not see the press"
    );
    assert_eq!(
        env.eval_int("windowDowns"),
        1,
        "the window sees every press"
    );
    assert_eq!(
        env.eval_int("message.hitTests"),
        hit_tests,
        "the button is in front of the message layer, which is never hit"
    );

    // A click on the transparent message layer reaches the background.
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 300.0, y: 300.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    assert!(
        env.eval_int("message.hitTests") > hit_tests,
        "the message layer's onHitTest ran and cleared the hit"
    );
    assert_eq!(
        env.eval_int("messageDowns"),
        0,
        "the transparent message layer does not take the press"
    );
    assert_eq!(
        env.eval_int("baseDowns"),
        1,
        "the transparent message layer lets the press through to the background"
    );
    assert_eq!(
        env.eval_int("message.clicks"),
        1,
        "the click on the background runs no action"
    );

    // A press on the button and a release after the pointer moved off it
    // still reach the button: the press took the capture.
    // reference/cpp/core/visual/LayerManager.cpp:374-422
    env.dispatch_events(&[
        PlatformEvent::Input(InputEvent::PointerMoved { x: 110.0, y: 110.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: true,
        }),
        PlatformEvent::Input(InputEvent::PointerMoved { x: 300.0, y: 110.0 }),
        PlatformEvent::Input(InputEvent::MouseButton {
            button: MouseButton::Left,
            pressed: false,
        }),
    ]);
    assert_eq!(
        env.eval_int("message.clicks"),
        2,
        "the release follows the capture and runs the action again"
    );
    assert_eq!(
        env.eval_int("baseDowns"),
        1,
        "the background does not see the captured press"
    );
}
