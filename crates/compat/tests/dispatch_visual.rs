//! Discriminating tests for the native **receiver-misrouting** bug in the
//! visual families (`Layer`, `Window`, `Bitmap`, `Font`, `Timer`).
//!
//! ## The mechanism under test
//!
//! Native dispatch has exactly one receiver channel: the `objthis` the VM
//! passes to `PropGet`/`PropSet`/`FuncCall`. The VM derives it as
//! `clo.ObjThis ? clo.ObjThis : ra[-1]`
//! (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1440`, identical to
//! `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1430`). Every instance-based
//! class then resolves its payload **only** from that pointer
//! (`crates/tjs/cpp/tjs2_abi.cpp:1136`/`:1327`), so if a member reference's
//! closure has no own `ObjThis`, the *enclosing function's* `this` leaks into
//! the write/call and the wrong-but-same-class object is mutated silently.
//!
//! The `with(obj){ .prop = value; }` shape is the measured reproducer: a
//! script subclass (`Sprite extends Layer`) wrote `.visible` to its parent
//! layer instead of itself when the write ran from a callback whose `this`
//! was the parent.
//!
//! ## What these tests pin
//!
//! Each family is declared as a **script subclass** of the native class, the
//! write target is stored as a member of a *different* same-class instance
//! (the "foreign receiver"), and the write runs from a method of that foreign
//! receiver — so the enclosing function's `this` is a same-class object and a
//! misroute is silent rather than a loud wrong-class error. Every write is
//! cross-checked against the L2 state (where available) as well as the
//! script-visible read.
//!
//! These assertions describe the **reference/correct** behaviour, so they are
//! expected to **fail until the central ABI fix lands** (the fix is owned by
//! another agent and must not be duplicated here). A failure therefore *is*
//! the evidence of misrouting; the tests are written so that, pre-fix, the
//! wrong object is observably mutated.

mod common;

use common::{Env, sleep_ms, vm_lock};
use engine::LayerState;

/// The L2 layer registered under `name` (the classes below set it explicitly).
fn layer_named<'a>(env: &'a Env, name: &str) -> &'a LayerState {
    env.scene
        .layers
        .iter()
        .find(|layer| layer.name == name)
        .unwrap_or_else(|| panic!("no layer named {name:?}"))
}

// ---------------------------------------------------------------------------
// Layer
// ---------------------------------------------------------------------------

/// The measured shape: `Sprite extends Layer` stored on a `Logo extends Layer`,
/// and a `Timer` action invokes `logo.tick()` so the enclosing `this` is the
/// Logo. `with(_logo){ .visible = false; }` must reach the Sprite.
#[test]
fn layer_timer_callback_with_write_targets_the_member_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        var w = new Window();
        class Sprite extends Layer {
            function Sprite(win, parent) { super.Layer(win, parent); }
        }
        class Logo extends Layer {
            var _logo = null;
            function Logo(win, parent) {
                super.Layer(win, parent);
                name = 'logo';
                _logo = new Sprite(win, this);
                _logo.name = 'logo_sprite';
                visible = true;
                _logo.visible = true;
            }
            function tick() { with(_logo) { .visible = false; } }
        }
        var logo = new Logo(w, null);
        var timer = new Timer(logo, 'tick');
        timer.interval = 1;
        timer.enabled = true;
        "#,
    );
    sleep_ms(10);
    env.poll_timer();

    // reference/cpp/core/visual/LayerIntf.cpp:10592 — `visible` is a native
    // instance property; the receiver is the object the write names, so the
    // Sprite's layer must become invisible and the Logo's must stay visible.
    assert!(
        !layer_named(&env, "logo_sprite").visible,
        "`with(_logo){{ .visible = false }}` from the Logo's timer callback must \
         land on the Sprite, not on the enclosing Logo (receiver misrouting)"
    );
    // reference/cpp/core/visual/LayerIntf.cpp:10592 — the enclosing Logo is
    // the leaked receiver; it must be untouched.
    assert!(
        layer_named(&env, "logo").visible,
        "the enclosing Layer must be untouched by the with-write"
    );
}

/// `with(_target){ .prop = value; }` from a method whose `this` is a foreign
/// same-class instance must write the member object, not the foreign receiver.
#[test]
fn layer_with_write_from_a_foreign_receiver_targets_the_member_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        var w = new Window();
        class SubLayer extends Layer {
            var _target = null;
            function SubLayer(win, parent) { super.Layer(win, parent); }
            function writeIt() { with(_target) { .visible = false; } }
        }
        var target = new SubLayer(w, null);
        var foreign = new SubLayer(w, null);
        target.name = 'target';
        foreign.name = 'foreign';
        target.visible = true;
        foreign.visible = true;
        foreign._target = target;
        foreign.writeIt();
        "#,
    );

    // reference/cpp/core/visual/LayerIntf.cpp:10592 — the with-object is the
    // receiver, so `target` must be the layer that changes.
    assert!(
        !layer_named(&env, "target").visible,
        "the with-write must reach the member object (`target`), not `foreign`"
    );
    // reference/cpp/core/visual/LayerIntf.cpp:10592 — the foreign `this` must
    // not receive the write.
    assert!(
        layer_named(&env, "foreign").visible,
        "the foreign receiver must be untouched"
    );
}

/// The read counterpart: `with(_target){ .prop }` must read the member object.
#[test]
fn layer_with_read_from_a_foreign_receiver_reads_the_member_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        var w = new Window();
        class SubLayer extends Layer {
            var _target = null;
            var _observed = -1;
            function SubLayer(win, parent) { super.Layer(win, parent); }
            function readIt() { with(_target) { _observed = .visible; } }
        }
        var target = new SubLayer(w, null);
        var foreign = new SubLayer(w, null);
        target.visible = true;
        foreign.visible = false;
        foreign._target = target;
        foreign.readIt();
        "#,
    );

    // reference/cpp/core/visual/LayerIntf.cpp:10592 — reading through the
    // with-object must yield the member object's value (1), not the foreign
    // receiver's (0).
    assert_eq!(
        env.eval_int("foreign._observed"),
        1,
        "the with-read must yield the member object's `visible`, not the \
         foreign receiver's"
    );
}

/// Method-call contrast: a native method invoked through `with(_target)` must
/// operate on the member object (the upstream report is that calls, unlike
/// writes, kept the right receiver).
#[test]
fn layer_with_method_call_from_a_foreign_receiver_lands_on_the_member_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        var w = new Window();
        class SubLayer extends Layer {
            var _target = null;
            function SubLayer(win, parent) { super.Layer(win, parent); }
            function callIt() { with(_target) { .setSize(33, 44); } }
        }
        var target = new SubLayer(w, null);
        var foreign = new SubLayer(w, null);
        target.name = 'target';
        foreign.name = 'foreign';
        target.setSize(11, 12);
        foreign.setSize(21, 22);
        foreign._target = target;
        foreign.callIt();
        "#,
    );

    // reference/cpp/core/visual/LayerIntf.cpp:8574 — `setSize` operates on the
    // receiver named by the with-object; `target` must be resized.
    let target = layer_named(&env, "target");
    assert_eq!(
        (target.rect.w, target.rect.h),
        (33, 44),
        "the with-method-call must resize the member object (`target`)"
    );
    // reference/cpp/core/visual/LayerIntf.cpp:8574 — the foreign receiver must
    // keep its own size.
    let foreign = layer_named(&env, "foreign");
    assert_eq!(
        (foreign.rect.w, foreign.rect.h),
        (21, 22),
        "the foreign receiver must not be resized by the with-method-call"
    );
}

/// Control: a direct (non-`with`) member write already resolves the receiver
/// from the object expression, so it must land on the named object. This
/// separates "the `with` proxy loses the receiver" from "the family is wholly
/// broken".
#[test]
fn layer_direct_member_write_is_unaffected() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        var w = new Window();
        class SubLayer extends Layer {
            function SubLayer(win, parent) { super.Layer(win, parent); }
        }
        var target = new SubLayer(w, null);
        var foreign = new SubLayer(w, null);
        target.name = 'target';
        foreign.name = 'foreign';
        target.visible = false;
        foreign.visible = true;
        "#,
    );

    // reference/cpp/core/visual/LayerIntf.cpp:10592 — direct member access
    // names its receiver explicitly.
    assert!(!layer_named(&env, "target").visible);
    assert!(layer_named(&env, "foreign").visible);
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

/// `Window` instance properties (`WindowInst` fields, e.g. `left`) resolved
/// from `objthis`.
#[test]
fn window_with_write_from_a_foreign_receiver_targets_the_member_window() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        class SubWindow extends Window {
            var _target = null;
            var _observed = -1;
            function SubWindow() { super.Window(); }
            function writeIt() { with(_target) { .left = 99; } }
            function readIt() { with(_target) { _observed = .left; } }
        }
        var target = new SubWindow();
        var foreign = new SubWindow();
        target.left = 1;
        foreign.left = 2;
        foreign._target = target;
        foreign.writeIt();
        "#,
    );

    // reference/cpp/core/visual/WindowIntf.cpp:1408 — `left` is a native
    // instance property, so the with-write must reach `target`.
    assert_eq!(
        env.eval_int("target.left"),
        99,
        "the Window with-write must reach the member window (`target`)"
    );
    // reference/cpp/core/visual/WindowIntf.cpp:1408 — the foreign receiver
    // must be untouched.
    assert_eq!(
        env.eval_int("foreign.left"),
        2,
        "the foreign Window receiver must be untouched"
    );

    // Read contrast.
    env.run("target.left = 7; foreign.left = 8; foreign.readIt();");
    // reference/cpp/core/visual/WindowIntf.cpp:1408 — the read must yield the
    // member window's value.
    assert_eq!(
        env.eval_int("foreign._observed"),
        7,
        "the Window with-read must yield the member window's `left`"
    );
}

// ---------------------------------------------------------------------------
// Bitmap
// ---------------------------------------------------------------------------

/// `Bitmap` dimension properties resolved from the `BitmapInst` payload.
#[test]
fn bitmap_with_write_from_a_foreign_receiver_targets_the_member_bitmap() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        class SubBitmap extends Bitmap {
            var _target = null;
            var _observed = -1;
            function SubBitmap(w, h) { super.Bitmap(w, h); }
            function writeIt() { with(_target) { .width = 7; } }
            function readIt() { with(_target) { _observed = .height; } }
        }
        var target = new SubBitmap(2, 2);
        var foreign = new SubBitmap(3, 3);
        foreign._target = target;
        foreign.writeIt();
        "#,
    );

    // reference/cpp/core/visual/BitmapIntf.cpp:454 — `width` is a native
    // instance property; the with-write must resize `target`.
    assert_eq!(
        env.eval_int("target.width"),
        7,
        "the Bitmap with-write must reach the member bitmap (`target`)"
    );
    // reference/cpp/core/visual/BitmapIntf.cpp:454 — the foreign receiver's
    // dimensions must be untouched.
    assert_eq!(
        env.eval_int("foreign.width"),
        3,
        "the foreign Bitmap receiver must be untouched"
    );

    // Read contrast, reference/cpp/core/visual/BitmapIntf.cpp:472 (`height`).
    env.run("target.height = 5; foreign.height = 6; foreign.readIt();");
    assert_eq!(
        env.eval_int("foreign._observed"),
        5,
        "the Bitmap with-read must yield the member bitmap's `height`"
    );
}

// ---------------------------------------------------------------------------
// Font
// ---------------------------------------------------------------------------

/// `Font` metric properties resolved from the `FontInst` payload / scene font.
#[test]
fn font_with_write_from_a_foreign_receiver_targets_the_member_font() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        class SubFont extends Font {
            var _target = null;
            var _observed = -1;
            function SubFont(face, h, color) { super.Font(face, h, color); }
            function writeIt() { with(_target) { .height = 44; } }
            function readIt() { with(_target) { _observed = .height; } }
        }
        var target = new SubFont('', 12, 0);
        var foreign = new SubFont('', 13, 0);
        foreign._target = target;
        foreign.writeIt();
        "#,
    );

    // reference/cpp/core/visual/LayerIntf.cpp:12034 — `height` is a native
    // instance property; the with-write must reach `target`.
    assert_eq!(
        env.eval_int("target.height"),
        44,
        "the Font with-write must reach the member font (`target`)"
    );
    // reference/cpp/core/visual/LayerIntf.cpp:12034 — the foreign receiver
    // must be untouched.
    assert_eq!(
        env.eval_int("foreign.height"),
        13,
        "the foreign Font receiver must be untouched"
    );

    // Read contrast.
    env.run("target.height = 21; foreign.height = 22; foreign.readIt();");
    assert_eq!(
        env.eval_int("foreign._observed"),
        21,
        "the Font with-read must yield the member font's `height`"
    );
}

// ---------------------------------------------------------------------------
// Timer
// ---------------------------------------------------------------------------

/// `Timer` properties resolved from the `TimerInst` payload / timer binding.
#[test]
fn timer_with_write_from_a_foreign_receiver_targets_the_member_timer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        class SubTimer extends Timer {
            var _target = null;
            function SubTimer(cb) { super.Timer(cb, ''); }
            function writeIt() { with(_target) { .interval = 555; } }
        }
        var target = new SubTimer(function(){});
        var foreign = new SubTimer(function(){});
        target.interval = 100;
        foreign.interval = 200;
        foreign._target = target;
        foreign.writeIt();
        "#,
    );

    // reference/cpp/core/utils/TimerIntf.cpp:177 — `interval` is a native
    // instance property; the with-write must reach `target`.
    assert_eq!(
        env.eval_int("target.interval == 555"),
        1,
        "the Timer with-write must reach the member timer (`target`)"
    );
    // reference/cpp/core/utils/TimerIntf.cpp:177 — the foreign receiver's
    // interval must be untouched.
    assert_eq!(
        env.eval_int("foreign.interval == 200"),
        1,
        "the foreign Timer receiver must be untouched"
    );
}

// ---------------------------------------------------------------------------
// Static member (receiver is discarded)
// ---------------------------------------------------------------------------

/// A static member is reached through the class object; the dispatcher drops
/// `objthis` (`crates/tjs/cpp/tjs2_abi.cpp:671`/`:718`), so a static read/write
/// is immune to the receiver bug. Writing a *static* through an instance
/// creates an own shadow member instead of touching engine state.
#[test]
fn font_static_member_discards_the_receiver() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        class SubFont extends Font {
            function SubFont() { super.Font(); }
        }
        var before = Font.rasterizer;
        Font.rasterizer = 0;
        var after = Font.rasterizer;
        var f = new SubFont();
        with(f) { .rasterizer = 0; }
        "#,
    );

    // reference/cpp/core/visual/LayerIntf.cpp:12162 — `rasterizer` is a static
    // property: it is engine state, not per-instance state, so the receiver is
    // irrelevant.
    assert_eq!(env.eval_int("before"), 0, "static read before write");
    assert_eq!(env.eval_int("after"), 0, "static read after write");
    // reference/cpp/core/visual/LayerIntf.cpp:12162 — the instance write must
    // not have been routed to the static setter's engine state (it creates a
    // shadow member on the instance).
    assert_eq!(
        env.eval_int("Font.rasterizer"),
        0,
        "a static member read through the class object stays receiver-free"
    );
}
