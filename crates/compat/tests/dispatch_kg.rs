//! Receiver-dispatch (misrouting) tests for the KAG / plugin families named
//! in `docs/audits/object-write-dispatch.md`: `KAGParser`, the `layer_ex`
//! `draw_property!` objthis family, `VideoOverlay` and the `motion_player`
//! plugin.
//!
//! The audited mechanism (`docs/audits/object-write-dispatch.md` §1): a
//! `with(obj) { .member = v }` write inside a function whose `this` is a
//! *different* object resolves the native member's receiver from the
//! `with`-object's closure `ObjThis`; when that closure carries no own
//! `ObjThis`, `SetPropertyDirect` falls back to `ra[-1]` — the enclosing
//! function's `this` (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1440`).
//!
//! A script subclass of a native class is the shape the audit singles out:
//! compat's class-name constructor member registers the native instance but
//! does not copy the native members onto the subclass instance and rebind
//! their `ObjThis` (`crates/tjs/cpp/tjs2_abi.cpp:1392-1470`), while the
//! reference does exactly that (`reference/cpp/core/tjs2/tjsNative.cpp:305-375`,
//! the `val.ChangeClosureObjThis(Dest)` at `:353`). The `with`-proxy then
//! hands the enclosing function's `this` to the native member, so the write
//! lands on the wrong object. Instance families resolve the payload *only*
//! from that pointer (`crates/tjs/cpp/tjs2_abi.cpp:1136`/`:1327`); objthis
//! families hand the raw pointer to Rust (`:825`/`:946`).
//!
//! Every test below asserts the **correct** semantics: the `with` write (and
//! the matching read and method call) must reach the `with` object and never
//! the enclosing object. They are **expected to fail until the central ABI
//! fix in `crates/tjs` lands**; they are written to pass afterwards.
//!
//! Each family gets the same shape:
//!   * a script subclass of the native class whose constructor writes an
//!     initial state value;
//!   * an *enclosing* instance of the same class whose method performs the
//!     `with(target)` read, write and method call while its own `this` is the
//!     enclosing object;
//!   * the read runs **before** the write so the target's constructor value
//!     and the enclosing object's value are distinct — a misrouted read is
//!     therefore observable, not self-consistent with a misrouted write.

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use tjs::{Tjs2Engine, TjsValue};

/// Script label for diagnostics.
const SCRIPT: &str = "dispatch-kg-test";

/// A bound VM + host + attached scene.
///
/// Fields drop in declaration order: the host first (releasing every retained
/// script object), then the engine and the scene.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene outlives the host (declared after `_host`).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _scene: scene,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, SCRIPT)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, SCRIPT)
            .map_err(|error| error.to_string())
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            Ok(TjsValue::Integer(value)) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval(expression) {
            Ok(TjsValue::Real(value)) => value,
            Ok(TjsValue::Integer(value)) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }
}

// ---------------------------------------------------------------------------
// KAGParser — instance-payload family
// ---------------------------------------------------------------------------

/// `KAGParser` flags/state are instance members resolved from the `objthis`
/// payload (compat `natives/kag/mod.rs:1045-1099`; reference
/// `TJS_GET_NATIVE_INSTANCE`, `reference/cpp/core/base/KAGParser.cpp:2435-2469`).
/// The `with` write must reach `target`; the enclosing parser must be
/// untouched.
#[test]
fn kagparser_with_write_keeps_the_target_receiver() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        // The constructor writes the initial state (`ignoreCR = false`,
        // `debugLevel = 1`); `enclosing` gets a distinct `debugLevel = 2`,
        // so the read-before-write below discriminates the two receivers.
        "class SubKAG extends KAGParser { \
             var _target = null; \
             var readValue = 0; \
             function SubKAG() { super.KAGParser(); ignoreCR = false; debugLevel = 1; } \
             function poke() { \
                 with(_target) { readValue = .debugLevel; } \
                 with(_target) { .debugLevel = 7; } \
                 with(_target) { .ignoreCR = true; } \
                 with(_target) { .interrupt(); } \
             } \
         } \
         var target = new SubKAG(); \
         var enclosing = new SubKAG(); \
         enclosing._target = target; \
         enclosing.debugLevel = 2; \
         enclosing.poke(); \
         var tag = target.getNextTag(); \
         var tagName = tag ? tag.tagname : '';",
    );

    // The read ran before the write, so target still holds its ctor value
    // (1) while the enclosing parser holds 2.
    assert_eq!(
        env.eval_int("enclosing.readValue"),
        1,
        "`with(target){{ readValue = .debugLevel }}` must read target (1), not enclosing (2)"
    );
    // reference KAGParser.cpp:2453-2469 (`debugLevel`): the write must land
    // on the `with` object (`target`), not the enclosing parser.
    assert_eq!(
        env.eval_int("target.debugLevel"),
        7,
        "`with(target){{ .debugLevel = 7 }}` must write target, not enclosing"
    );
    assert_eq!(
        env.eval_int("enclosing.debugLevel"),
        2,
        "the enclosing parser must keep its own debugLevel"
    );
    // reference KAGParser.cpp:2435-2451 (`ignoreCR`).
    assert_eq!(
        env.eval_int("target.ignoreCR ? 1 : 0"),
        1,
        "`with(target){{ .ignoreCR = true }}` must write target"
    );
    assert_eq!(
        env.eval_int("enclosing.ignoreCR ? 1 : 0"),
        0,
        "the enclosing parser must keep its own ignoreCR"
    );
    // Method contrast: `with(target){{ .interrupt(); }}` must reach target
    // (reference `KAGParser.cpp:2355-2363`); the target then yields the
    // `interrupt` tag while the enclosing parser has none.
    assert_eq!(
        env.eval_int("tagName == 'interrupt' ? 1 : 0"),
        1,
        "`with(target){{ .interrupt(); }}` must call target's method"
    );
}

/// Control: a **direct** native instance is not a script subclass, so the
/// class-chain member copy is not involved. The same `with` write must also
/// reach it (the reference path is member-copied, `tjsNative.cpp:305-375`).
#[test]
fn kagparser_direct_instance_with_write_keeps_the_receiver() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var target = new KAGParser(); \
         var enclosing = new KAGParser(); \
         enclosing.debugLevel = 2; \
         function poke(t) { with(t) { .debugLevel = 7; } } \
         poke(target);",
    );
    assert_eq!(
        env.eval_int("target.debugLevel"),
        7,
        "a direct KAGParser instance must receive the with write"
    );
    assert_eq!(
        env.eval_int("enclosing.debugLevel"),
        2,
        "the enclosing instance must be untouched"
    );
}

// ---------------------------------------------------------------------------
// layer_ex — `draw_property!` objthis family
// ---------------------------------------------------------------------------

/// The `LayerExDraw` `draw_property!` members (`smoothingMode`,
/// `textRenderingHint`, `updateWhenDraw`, compat
/// `plugins/layer_ex/draw.rs:2181-2229`) are registered on `Layer` as
/// objthis properties (`layer_property_defs`, `draw.rs:2446-2480`) and
/// resolve the receiver from the raw `objthis`
/// (`layer_from_objthis`, `plugins/layer_ex/mod.rs:121-140`). Reference:
/// `NCB_ATTACH_CLASS_WITH_HOOK(LayerExDraw, Layer)` +
/// `NCB_PROPERTY(smoothingMode, …)`
/// (`reference/cpp/plugins/layerex_draw/windows/main.cpp:872-875`), whose
/// getters/setters read the native instance from `objthis`
/// (`reference/cpp/core/plugin/ncbind.hpp:171`).
#[test]
fn layer_ex_draw_property_with_write_keeps_the_target_receiver() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubLayer extends Layer { \
             var _target = null; \
             var readValue = 0; \
             function SubLayer(w) { super.Layer(w, null); smoothingMode = 4; } \
             function poke() { \
                 with(_target) { readValue = .smoothingMode; } \
                 with(_target) { .smoothingMode = 8; } \
                 with(_target) { .resetTransform(); } \
             } \
         } \
         var w = new Window(); \
         var target = new SubLayer(w); \
         var enclosing = new SubLayer(w); \
         enclosing._target = target; \
         enclosing.smoothingMode = 2; \
         enclosing.poke();",
    );
    assert_eq!(
        env.eval_int("enclosing.readValue == 4 ? 1 : 0"),
        1,
        "`with(target){{ readValue = .smoothingMode }}` must read target (4), not enclosing (2)"
    );
    // reference windows/LayerExDraw.hpp:214-215 (`smoothingMode`).
    assert_eq!(
        env.eval_int("target.smoothingMode == 8 ? 1 : 0"),
        1,
        "`with(target){{ .smoothingMode = 8 }}` must write the target layer"
    );
    assert_eq!(
        env.eval_int("enclosing.smoothingMode == 2 ? 1 : 0"),
        1,
        "the enclosing layer must keep its own smoothingMode"
    );
}

// ---------------------------------------------------------------------------
// VideoOverlay — instance-payload family
// ---------------------------------------------------------------------------

/// `VideoOverlay` properties are instance members resolved from the
/// `objthis` payload (compat `natives/video.rs:370-449`; reference
/// `TJS_GET_NATIVE_INSTANCE`, `reference/cpp/core/visual/VideoOvlIntf.cpp:510`
/// `left`, `:626` `visible`).
#[test]
fn video_overlay_with_write_keeps_the_target_receiver() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubVideo extends VideoOverlay { \
             var _target = null; \
             var readValue = 0; \
             function SubVideo() { super.VideoOverlay(new global.Window()); left = 100; visible = false; } \
             function poke() { \
                 with(_target) { readValue = .left; } \
                 with(_target) { .left = 30; } \
                 with(_target) { .visible = true; } \
                 with(_target) { .setPos(5, 6); } \
             } \
         } \
         var target = new SubVideo(); \
         var enclosing = new SubVideo(); \
         enclosing._target = target; \
         enclosing.left = 10; \
         enclosing.poke();",
    );
    // The read ran before the write: target still holds its ctor value (100)
    // while the enclosing overlay holds 10.
    assert_eq!(
        env.eval_int("enclosing.readValue"),
        100,
        "`with(target){{ readValue = .left }}` must read target (100), not enclosing (10)"
    );
    // reference VideoOvlIntf.cpp:510 (left) and :626 (visible).
    assert_eq!(
        env.eval_int("target.visible ? 1 : 0"),
        1,
        "`with(target){{ .visible = true }}` must write the target"
    );
    // Method contrast: `setPos` is `TJS_BEGIN_NATIVE_METHOD_DECL(setPos)`
    // (`reference/cpp/core/visual/VideoOvlIntf.cpp:249-261`) and resolves
    // `_this` from `objthis`; it must set the target's left to 5.
    assert_eq!(
        env.eval_int("target.left"),
        5,
        "`with(target){{ .setPos(5,6); }}` must call the target, setting left = 5"
    );
    assert_eq!(
        env.eval_int("enclosing.left"),
        10,
        "the enclosing overlay must keep its own left"
    );
    assert_eq!(
        env.eval_int("enclosing.visible ? 1 : 0"),
        0,
        "the enclosing overlay must keep its own visible (false)"
    );
}

// ---------------------------------------------------------------------------
// motion_player — instance-payload family
// ---------------------------------------------------------------------------

/// `Player` properties/methods are instance members resolved from the
/// `objthis` payload (compat `plugins/motion_player/player.rs:1010-1012`,
/// `player_members.rs`; reference `NCB_REGISTER_CLASS(Player)` +
/// `NCB_PROPERTY(x, getX, setX)`,
/// `reference/cpp/plugins/motionplayer/main.cpp:128-166`).
#[test]
fn motion_player_with_write_keeps_the_target_receiver() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "class SubPlayer extends Player { \
             var _target = null; \
             var readValue = 0.0; \
             function SubPlayer() { super.Player(null); x = 1.5; } \
             function poke() { \
                 with(_target) { readValue = .x; } \
                 with(_target) { .x = 42.5; } \
                 with(_target) { .setVariable('k', 3.5); } \
             } \
         } \
         var target = new SubPlayer(); \
         var enclosing = new SubPlayer(); \
         enclosing._target = target; \
         enclosing.x = 2.5; \
         enclosing.poke();",
    );
    assert!(
        (env.eval_real("enclosing.readValue") - 1.5).abs() < 1e-9,
        "`with(target){{ readValue = .x }}` must read target (1.5), not enclosing (2.5)"
    );
    // reference motionplayer/main.cpp:133 (`NCB_PROPERTY(x, getX, setX)`).
    assert!(
        (env.eval_real("target.x") - 42.5).abs() < 1e-9,
        "`with(target){{ .x = 42.5 }}` must write the target player"
    );
    assert!(
        (env.eval_real("enclosing.x") - 2.5).abs() < 1e-9,
        "the enclosing player must keep its own x"
    );
    // Method contrast: `setVariable` is an instance-payload method
    // (reference main.cpp:253-254, `Player.h:344`); the variable must land
    // on the target, not the enclosing shell.
    assert!(
        (env.eval_real("target.getVariable('k')") - 3.5).abs() < 1e-9,
        "`with(target){{ .setVariable('k', 3.5); }}` must call the target"
    );
    assert!(
        (env.eval_real("enclosing.getVariable('k')") - 0.0).abs() < 1e-9,
        "the enclosing player must not receive the variable"
    );
}
