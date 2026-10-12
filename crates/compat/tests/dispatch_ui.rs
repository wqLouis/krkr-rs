//! Dispatch tests for the UI/system native families.
//!
//! The audited hypothesis (`docs/audits/object-write-dispatch.md`) was that a
//! `with(obj)` write whose register closure has no own `ObjThis` falls back to
//! the enclosing function's `this` (`clo.ObjThis ? clo.ObjThis : ra[-1]`,
//! `crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1440`), so an **instance-payload**
//! or **objthis** family would silently write the wrong same-class object.
//! These tests exercise that shape: a **script subclass** of the native class,
//! the write target stored as a member of a *different* instance (the
//! `system/Title.tjs` `_logo` shape), and the write / read / call issued
//! through `with(held)` from that instance's `this`. The observed result is
//! that the receiver is routed correctly — the hypothesis is **disproven** for
//! these families at this layer — so the tests now pin the reference
//! behaviour (`with(obj){ .member = v }` writes through to `obj`,
//! `reference/tests/test_files/tjs2/test_with.tjs`), with the instance/objthis
//! and static/no-state families as the contrast.
//!
//! The harness is the shared [`common::Env`] + [`common::vm_lock`] (the
//! engine → context binding is process-wide), which binds a VM, the compat
//! host and the L2 scene / timer / input the app uses.

mod common;

use common::{Env, vm_lock};
use tjs::TjsValue;

/// Read a string member (the shared [`Env`] ships only an integer reader).
fn eval_str(env: &Env, expression: &str) -> String {
    match env.eval_ok(expression) {
        TjsValue::String(value) => value,
        other => panic!("{expression:?} returned {other:?}, expected a string"),
    }
}

// ---------------------------------------------------------------------------
// MenuItem (core, instance-payload family) — `natives/menu_item.rs`
// ---------------------------------------------------------------------------

/// The `system/Title.tjs` shape narrowed to `MenuItem`: the write target is a
/// member of a *different* item, and the `with` write runs from that other
/// item's `this`. Reference: `tTJSNI_BaseMenuItem`'s instance properties
/// (`reference/cpp/core/visual/MenuItemIntf.cpp:444-460` for `visible`) are
/// resolved through `TJS_GET_NATIVE_INSTANCE` on the receiver, and the
/// `with` statement writes through to the with-object
/// (`reference/tests/test_files/tjs2/test_with.tjs`). The port's
/// `bool_property!` reads the `MenuItemInst` payload derived from `objthis`
/// (`crates/compat/src/natives/menu_item.rs:1002`), so a wrong receiver
/// writes the wrong node. The item is attached to a real `Window` so the
/// `fireClick` method-call half is deliverable (reference
/// `MenuItemIntf.cpp:196-213`); a failure is therefore *misrouting*, not the
/// window gate.
#[test]
fn menu_item_instance_write_read_and_call_reach_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var menuWindow = new Window(); \
         class SubItem extends MenuItem { \
             var held = null; \
             function SubItem(owner, win) { \
                 super.MenuItem(owner, win); \
                 this.visible = true; \
             } \
             function leak() { with(held) { .visible = false; } } \
             function readHeld() { with(held) { foreignVisible = .visible; } } \
             function clickHeld() { with(held) { .fireClick(); } } \
         } \
         var targetItem = new SubItem(null, menuWindow); \
         var anchorItem = new SubItem(null, menuWindow); \
         var foreignVisible = -1; \
         var targetClicks = 0; \
         var anchorClicks = 0; \
         anchorItem.held = targetItem; \
         targetItem.onClick = function() { targetClicks++; }; \
         anchorItem.onClick = function() { anchorClicks++; }; \
         anchorItem.leak(); \
         anchorItem.readHeld(); \
         anchorItem.clickHeld();",
    );
    assert_eq!(
        env.eval_int("targetItem.visible"),
        0,
        "with(target){{ .visible = false }} from a foreign `this` must write the target item"
    );
    assert_eq!(
        env.eval_int("anchorItem.visible"),
        1,
        "the foreign `this` (anchorItem) must be untouched"
    );
    assert_eq!(
        env.eval_int("foreignVisible"),
        0,
        "the `with` read must read the target item, not the enclosing `this`"
    );
    assert_eq!(
        env.eval_int("targetClicks"),
        1,
        "with(target){{ .fireClick() }} must dispatch on the target item"
    );
    assert_eq!(
        env.eval_int("anchorClicks"),
        0,
        "the method call must not reach the enclosing `this`"
    );
}

/// The `checked` radio-group side effect is a receiver-independent oracle:
/// the native setter clears the target's radio siblings **under the same
/// `Parent`** (`tTJSNI_MenuItem::SetChecked`,
/// `reference/cpp/core/visual/impl/MenuItemImpl.cpp:226-237`: `if(b &&
/// IsRadio && Parent) for(_item : Parent->Children) …`; reached through the
/// `checked` setter at `reference/cpp/core/visual/MenuItemIntf.cpp:352-368`).
/// The port mirrors that parent/children scoping
/// (`crates/compat/src/natives/menu_item.rs` `menu_checked_set`). Checking
/// the target through a foreign `this` must therefore clear the *anchor's*
/// checked state. This distinguishes a real receiver misroute (which would
/// check the anchor and clear the target) from a plain shadowing write
/// (which would leave the native state untouched).
#[test]
fn menu_item_checked_radio_side_effect_follows_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var checkedWindow = new Window(); \
         class SubChecked extends MenuItem { \
             var held = null; \
             function SubChecked(win) { super.MenuItem(null, win); } \
             function checkHeld() { with(held) { .checked = true; } } \
         } \
         var checkedRoot = new SubChecked(checkedWindow); \
         var checkedTarget = new SubChecked(checkedWindow); \
         var checkedAnchor = new SubChecked(checkedWindow); \
         checkedRoot.add(checkedTarget); \
         checkedRoot.add(checkedAnchor); \
         checkedAnchor.held = checkedTarget; \
         checkedTarget.radio = true; checkedTarget.group = 1; \
         checkedAnchor.radio = true; checkedAnchor.group = 1; \
         checkedAnchor.checked = true; \
         checkedAnchor.checkHeld();",
    );
    assert_eq!(
        env.eval_int("checkedTarget.checked"),
        1,
        "the target item must be checked"
    );
    assert_eq!(
        env.eval_int("checkedAnchor.checked"),
        0,
        "checking the target's radio group must clear the enclosing item"
    );
}

/// Static members (`MenuItem.textToKeycode` / `keycodeToText`) are registered
/// with `TJS_MEMBERENSURE | TJS_IGNOREPROP | TJS_STATICMEMBER`
/// (`reference/cpp/core/visual/impl/MenuItemImpl.cpp:363-395`), so the
/// receiver is dropped (`crates/tjs/cpp/tjs2_abi.cpp:718`) and a
/// class-qualified read is receiver-independent. The tables are read-only and
/// hold exactly the three unconditional entries.
#[test]
fn menu_item_static_tables_are_receiver_independent() {
    let _lock = vm_lock();
    let env = Env::new();
    assert_eq!(env.eval_int("MenuItem.textToKeycode['bksp']"), 8);
    assert_eq!(env.eval_int("MenuItem.textToKeycode['pgup']"), 33);
    assert_eq!(eval_str(&env, "MenuItem.keycodeToText[34]"), "PgDn");
    assert!(
        env.eval("MenuItem.textToKeycode = %[]").is_err(),
        "a static read-only property must reject a class-qualified write"
    );
    assert!(
        env.eval("MenuItem.keycodeToText = []").is_err(),
        "a static read-only property must reject a class-qualified write"
    );
}

// ---------------------------------------------------------------------------
// window_ex `Window` (objthis family) — `plugins/window_ex/window.rs`
// ---------------------------------------------------------------------------

/// `Window.disableResize`/`enableNCMouseEvent` are objthis-only properties:
/// the raw receiver is handed to Rust, which keys a per-object `WindowFlags`
/// map by `objthis as usize` (`window_ex/window.rs` and `util.rs`
/// `sync_object_state`). The reference derives its `WindowEx` instance from
/// the same receiver (`GetInstance(obj)`,
/// `reference/cpp/plugins/windowEx.cpp:496-498,513-515,581`). A wrong
/// receiver therefore flips the wrong window's flags.
#[test]
fn window_ex_window_flags_reach_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubWindow extends Window { \
             var held = null; \
             function SubWindow() { super.Window(); } \
             function leak() { with(held) { .disableResize = false; } } \
             function readHeld() { with(held) { foreignResize = .disableResize; } } \
             function enableNcmHeld() { with(held) { .enableNCMouseEvent = true; } } \
         } \
         var targetWindow = new SubWindow(); \
         var anchorWindow = new SubWindow(); \
         var foreignResize = -1; \
         anchorWindow.held = targetWindow; \
         targetWindow.disableResize = true; \
         anchorWindow.disableResize = true; \
         anchorWindow.leak(); \
         anchorWindow.readHeld(); \
         anchorWindow.enableNcmHeld();",
    );
    assert_eq!(
        env.eval_int("targetWindow.disableResize"),
        0,
        "the `with` write must flip the target window's disableResize"
    );
    assert_eq!(
        env.eval_int("anchorWindow.disableResize"),
        1,
        "the enclosing window must keep its own flag"
    );
    assert_eq!(
        env.eval_int("foreignResize"),
        0,
        "the `with` read must read the target window"
    );
    assert_eq!(
        env.eval_int("targetWindow.enableNCMouseEvent"),
        1,
        "the `with` write must flip the target window's enableNCMouseEvent"
    );
    assert_eq!(
        env.eval_int("anchorWindow.enableNCMouseEvent"),
        0,
        "the enclosing window must not inherit the target's enableNCMouseEvent"
    );
    // The class constants are statics, so they are receiver-independent.
    assert_eq!(env.eval_int("Window.nchtError"), 65534);
    assert_eq!(env.eval_int("Window.nchtCaption"), 2);
}

// ---------------------------------------------------------------------------
// window_ex `MenuItemEx` (objthis family) — `plugins/window_ex/menu_item.rs`
// ---------------------------------------------------------------------------

/// `rightJustify`/`bmpItem`/`bmpChecked`/`bmpUnchecked` are objthis-only
/// properties keyed by `objthis` (`crates/compat/src/natives/plugins/window_ex/
/// menu_item.rs:319`); the reference caches them per object via
/// `ncbInstanceAdaptor<MenuItemEx>::GetNativeInstance(obj)`
/// (`reference/cpp/plugins/windowEx.cpp:1081-1103,1191-1199`). A wrong
/// receiver writes the wrong item. The `bi*` constants are statics
/// (`windowEx.cpp:1220-1229`) and therefore immune.
#[test]
fn window_ex_menu_item_flags_reach_the_target() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var exWindow = new Window(); \
         class SubItemEx extends MenuItem { \
             var held = null; \
             function SubItemEx(win) { super.MenuItem(null, win); } \
             function leak() { with(held) { .rightJustify = false; } } \
             function readHeld() { with(held) { foreignRight = .rightJustify; } } \
             function bmpHeld() { with(held) { .bmpItem = %[ a: 1 ]; } } \
         } \
         var targetEx = new SubItemEx(exWindow); \
         var anchorEx = new SubItemEx(exWindow); \
         var foreignRight = -1; \
         anchorEx.held = targetEx; \
         targetEx.rightJustify = true; \
         anchorEx.rightJustify = true; \
         anchorEx.leak(); \
         anchorEx.readHeld(); \
         anchorEx.bmpHeld();",
    );
    assert_eq!(
        env.eval_int("targetEx.rightJustify"),
        0,
        "the `with` write must clear the target item's rightJustify"
    );
    assert_eq!(
        env.eval_int("anchorEx.rightJustify"),
        1,
        "the enclosing item must keep its own rightJustify"
    );
    assert_eq!(
        env.eval_int("foreignRight"),
        0,
        "the `with` read must read the target item"
    );
    assert_eq!(
        env.eval_int("targetEx.bmpItem"),
        -1,
        "an object write through `with` must store BMT_BMP on the target"
    );
    assert_eq!(
        env.eval_int("anchorEx.bmpItem"),
        0,
        "the enclosing item must keep its own bmpItem state"
    );
    // The `bi*` class constants are statics (receiver dropped).
    assert_eq!(env.eval_int("MenuItem.biSystem"), 1);
    assert_eq!(env.eval_int("MenuItem.biClose"), 5);
}

// ---------------------------------------------------------------------------
// WIN32Dialog (instance class, no receiver-derived state) — immune
// ---------------------------------------------------------------------------

/// `WIN32Dialog.messageBox` is an **instance** method, but its body never
/// reads the native payload: it resolves `System` from the engine and calls
/// `System.inform(message, caption, 2)` (`reference/cpp/plugins/win32dialog.cpp:
/// 6-10`; port `crates/compat/src/natives/plugins/win32_dialog/mod.rs`). So
/// even a misrouted receiver cannot redirect the write — the family is
/// **immune** to this bug. The test pins that the call still reaches
/// `System.inform` (with the reference's literal `2`) when issued through a
/// `with` from a foreign `this`.
#[test]
fn win32_dialog_message_box_is_receiver_independent() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var informSeen = ''; \
         System.inform = function(m, c, t) { \
             informSeen = '' + typeof m + '|' + typeof c + '|' + t; \
             return 7; \
         }; \
         class SubDialog extends WIN32Dialog { \
             var held = null; \
             function SubDialog() { super.WIN32Dialog(); } \
             function leak() { with(held) { dialogResult = .messageBox('hello', 'world', 5); } } \
         } \
         var targetDialog = new SubDialog(); \
         var anchorDialog = new SubDialog(); \
         var dialogResult = -1; \
         anchorDialog.held = targetDialog; \
         anchorDialog.leak();",
    );
    // `return !System.inform(...)` with the override returning 7 → 0.
    assert_eq!(env.eval_int("dialogResult"), 0);
    assert_eq!(
        eval_str(&env, "informSeen"),
        "String|String|2",
        "message/caption pass through and `type` is replaced by the literal 2"
    );
}

// ---------------------------------------------------------------------------
// Pad / Mouse / Key — `natives/input.rs`
// ---------------------------------------------------------------------------

/// `Pad` is an instance class whose getters return fixed reference values and
/// whose setters are no-ops (`reference/cpp/core/utils/PadIntf.cpp:62-115,
/// 282-320`). The port mirrors that (`crates/compat/src/natives/input.rs`
/// `pad_noop_set` + the constant `PadState` defaults), so there is **no
/// script-writable receiver state**: a misrouted write or read cannot be
/// observed. The family is immune; the test pins the reference constants
/// through a foreign `this`.
#[test]
fn pad_properties_are_receiver_independent_constants() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubPad extends Pad { \
             var held = null; \
             function SubPad() { super.Pad(); } \
             function leak() { \
                 with(held) { .visible = true; .text = 'leaked'; .width = 42; } \
             } \
             function readHeld() { \
                 with(held) { \
                     padVisible = .visible; padText = .text; padWidth = .width; \
                 } \
             } \
             function noop() { with(held) { .registerExEvent(); } } \
         } \
         var targetPad = new SubPad(); \
         var anchorPad = new SubPad(); \
         var padVisible = -1; var padText = 'init'; var padWidth = -1; \
         anchorPad.held = targetPad; \
         anchorPad.leak(); \
         anchorPad.readHeld(); \
         anchorPad.noop();",
    );
    // Setters are no-ops (`PadIntf.cpp:62-115`), getters are constants.
    assert_eq!(
        env.eval_int("targetPad.visible"),
        0,
        "visible is a constant 0"
    );
    assert_eq!(
        env.eval_int("targetPad.width"),
        100,
        "width is a constant 100"
    );
    assert_eq!(
        eval_str(&env, "targetPad.text"),
        "",
        "text is a constant \"\""
    );
    assert_eq!(env.eval_int("padVisible"), 0);
    assert_eq!(env.eval_int("padWidth"), 100);
    assert_eq!(eval_str(&env, "padText"), "");
}

/// `Mouse` and `Key` are **static-only** classes (registered via
/// `register_class`; `crates/compat/src/natives/input.rs:1376,1439`), so
/// their callbacks receive no receiver at all. Class-qualified calls are
/// receiver-independent; the default cursor/button state reads back as the
/// documented defaults.
#[test]
fn mouse_and_key_statics_are_receiver_independent() {
    let _lock = vm_lock();
    let env = Env::new();
    assert_eq!(eval_str(&env, "Mouse.getCursorPos()"), "0,0");
    assert_eq!(env.eval_int("Mouse.getCursorX()"), 0);
    assert_eq!(env.eval_int("Mouse.getPressed(0)"), 0);
    assert_eq!(env.eval_int("Key.getPressed(1)"), 0);
    assert_eq!(env.eval_int("Key.getRepeat(1)"), 0);
    env.eval_ok("Mouse.setCursorPos(10, 20)");
}

// ---------------------------------------------------------------------------
// System and the dialog-driven notify path — static-only class
// ---------------------------------------------------------------------------

/// `System` is a static-only class (`register_class`;
/// `crates/compat/src/natives/system.rs:2222`), so its members receive no
/// receiver (`crates/tjs/cpp/tjs2_abi.cpp:569`) and a `with(System)` write
/// from a foreign `this` is receiver-independent. The reference `System.title`
/// is a plain data property on the class object
/// (`reference/cpp/core/base/SystemIntf.cpp`), and `getDoubleClickTime` is the
/// `return -1` literal implemented by the `windowEx` plugin
/// (`reference/cpp/plugins/windowEx.cpp:1422`).
#[test]
fn system_statics_and_dialog_notify_are_receiver_independent() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var systemWindow = new Window(); \
         class SubProbe extends MenuItem { \
             function SubProbe(win) { super.MenuItem(null, win); } \
             function writeTitle() { with(System) { .title = 'LeakedTitle'; } } \
             function readTick() { with(System) { tickRead = .getDoubleClickTime(); } } \
         } \
         var systemProbe = new SubProbe(systemWindow); \
         var tickRead = 0; \
         systemProbe.writeTitle(); \
         systemProbe.readTick();",
    );
    assert_eq!(
        eval_str(&env, "System.title"),
        "LeakedTitle",
        "a `with(System)` write is class-level, not receiver-scoped"
    );
    assert_eq!(
        env.eval_int("tickRead"),
        -1,
        "`System.getDoubleClickTime()` is the static `return -1` literal"
    );
}

// ---------------------------------------------------------------------------
// The motivating frame-body shape: a Timer action writing through `_logo`
// ---------------------------------------------------------------------------

/// The exact `system/Title.tjs` shape scaled to `MenuItem`: the target is a
/// member of the Timer's action owner, and the write runs inside the timer
/// dispatch where `this` is the owner. Reference: the timer invokes
/// `owner.action(event)` with the owner as receiver
/// (`reference/cpp/core/utils/TimerIntf.cpp:156-171`,
/// `reference/cpp/core/base/EventIntf.h:208-234`); the `with` write must
/// therefore reach the stored item, not the owner. The port's timer dispatch
/// calls the owner's member with the owner as `this`
/// (`crates/compat/src/natives/visual/timer.rs` `invoke_action`).
#[test]
fn timer_action_write_from_the_owner_this_reaches_the_held_item() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var timerWindow = new Window(); \
         class SubTimerItem extends MenuItem { \
             var held = null; \
             function SubTimerItem(win) { \
                 super.MenuItem(null, win); \
                 this.visible = true; \
             } \
             function action(ev) { with(held) { .visible = false; } } \
         } \
         var timerTarget = new SubTimerItem(timerWindow); \
         var timerOwner = new SubTimerItem(timerWindow); \
         timerOwner.held = timerTarget; \
         var timer = new Timer(timerOwner, 'action'); \
         timer.interval = 1; \
         timer.enabled = true; \
         timer.onTimer();",
    );
    assert_eq!(
        env.eval_int("timerTarget.visible"),
        0,
        "the timer action's `with(held){{ .visible = false }}` must reach the held item"
    );
    assert_eq!(
        env.eval_int("timerOwner.visible"),
        1,
        "the action owner (the foreign `this`) must be untouched"
    );
}
