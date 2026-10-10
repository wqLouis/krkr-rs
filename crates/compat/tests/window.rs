//! Integration tests for the `Window` native class surface added on top of the
//! base scene adapter: the assignable `on*` event handlers, the
//! `getMouseVelocity`/`resetMouseVelocity` input seam, the host-fact
//! properties and the `layerTreeOwnerInterface` handoff.
//!
//! The tests attach a caller-owned L2 [`Scene`], timer registry and input state
//! to a [`CompatHost`]. Because the host stores raw pointers to those values,
//! they are boxed and declared after `_host` so they outlive it (the fields
//! drop in declaration order).

mod common;

use common::vm_lock;
use compat::{CompatHost, EventAdapter};
use engine::Scene;
use engine::input::InputState;
use engine::timer::TimerState;
use platform::{InputEvent, PlatformEvent};
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host + attached scene, timer registry and input state.
struct Env {
    /// Dropped first: releases every retained script object before the scene,
    /// the timer registry, the input state and the engine go away.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// Kept alive for the host's raw pointer; never read directly.
    _scene: Box<Scene>,
    /// Kept alive for the host's raw pointer; never read directly.
    _timer: Box<TimerState>,
    /// Kept alive for the host's raw pointer; never read directly.
    _input: Box<InputState>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut timer = Box::new(TimerState::new());
        let mut input = Box::new(InputState::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed values are declared after `_host`, so they are
        // dropped later and outlive the binding.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
            host.services_mut()
                .attach_input(input.as_mut() as *mut InputState);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _scene: scene,
            _timer: timer,
            _input: input,
        }
    }

    /// Feed one frame of platform events through the L3 adapter into the
    /// attached L2 input state.
    fn feed_input(&self, events: &[PlatformEvent]) {
        let adapter = EventAdapter::new();
        let mut ctx = self._host.ctx().expect("bound host");
        adapter.begin_input_frame(&mut ctx);
        for event in events {
            adapter.apply_input_event(&mut ctx, event);
        }
        adapter.end_input_frame(&mut ctx);
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "window-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "window-test")
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_err(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should raise"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            TjsValue::Integer(value) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }
}

/// The full reference `on*` dispatch surface.
const EVENT_HANDLERS: [&str; 24] = [
    "onActivate",
    "onClick",
    "onCloseQuery",
    "onDeactivate",
    "onDisplayRotate",
    "onDoubleClick",
    "onFileDrop",
    "onKeyDown",
    "onKeyPress",
    "onKeyUp",
    "onMouseDown",
    "onMouseEnter",
    "onMouseLeave",
    "onMouseMove",
    "onMouseUp",
    "onMouseWheel",
    "onMultiTouch",
    "onPopupHide",
    "onResize",
    "onTouchDown",
    "onTouchMove",
    "onTouchRotate",
    "onTouchScaling",
    "onTouchUp",
];

#[test]
fn event_handlers_are_callable_and_script_assignments_shadow_them() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");

    for handler in EVENT_HANDLERS {
        // Calling the default must not raise: an absent member would, so this
        // also proves every reference handler name is registered. (TJS
        // `typeof` reports a function object as `Object`, so a call is the
        // reliable check for "callable".)
        env.run(&format!("w.{handler}();"));
    }

    // A script assignment shadows the native no-op (the `MenuItem.onClick`
    // dispatch contract the L3 event adapter relies on).
    env.run(
        "var hits = 0; var gotX = 0; var gotY = 0; \
         w.onClick = function(x, y) { hits++; gotX = x; gotY = y; }; \
         w.onClick(3, 4);",
    );
    assert_eq!(env.eval_int("hits"), 1);
    assert_eq!(env.eval_int("gotX"), 3);
    assert_eq!(env.eval_int("gotY"), 4);
}

#[test]
fn mouse_velocity_samples_the_l2_pointer_and_resets() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");

    env.feed_input(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 10.0,
        y: 20.0,
    })]);

    // TJS2 passes only *local* variables by reference, so the out-parameter
    // probe lives in a function (the reference `(*param[i]) = value` shape).
    env.run(
        "function probe() { \
             var vx = 0.0; var vy = 0.0; var vs = 0.0; \
             var moved = w.getMouseVelocity(vx, vy, vs); \
             return [moved, vx, vy, vs]; \
         } \
         var r = probe();",
    );
    // The first sample only seeds the tracker: no movement to report yet.
    assert_eq!(env.eval_int("r[0]"), 0);
    assert_eq!(env.eval_real("r[1]"), 0.0);
    assert_eq!(env.eval_real("r[2]"), 0.0);
    assert_eq!(env.eval_real("r[3]"), 0.0);

    env.feed_input(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 13.0,
        y: 24.0,
    })]);
    env.run("r = probe();");
    assert_eq!(env.eval_int("r[0]"), 1);
    assert_eq!(env.eval_real("r[1]"), 3.0);
    assert_eq!(env.eval_real("r[2]"), 4.0);
    assert_eq!(env.eval_real("r[3]"), 5.0);

    // `resetMouseVelocity` drops the sample, so the next read is a fresh seed.
    env.run("w.resetMouseVelocity(); r = probe();");
    assert_eq!(env.eval_int("r[0]"), 0);
    assert_eq!(env.eval_real("r[1]"), 0.0);
    assert_eq!(env.eval_real("r[3]"), 0.0);

    // The reference shape needs all three out-parameters.
    assert!(env.eval("w.getMouseVelocity(1, 2)").is_err());
}

#[test]
fn host_fact_properties_round_trip() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");

    // Reference defaults (`TVPWindow.h`: `bsNone`, no stay-on-top, `5`/`5`).
    assert_eq!(env.eval_int("w.borderStyle"), 0);
    assert_eq!(env.eval_int("w.stayOnTop"), 0);
    assert_eq!(env.eval_real("w.touchScaleThreshold"), 5.0);
    assert_eq!(env.eval_real("w.touchRotateThreshold"), 5.0);

    env.run(
        "w.borderStyle = 2; \
         w.stayOnTop = true; \
         w.touchScaleThreshold = 1.5; \
         w.touchRotateThreshold = 2.25;",
    );
    assert_eq!(env.eval_int("w.borderStyle"), 2);
    assert_eq!(env.eval_int("w.stayOnTop"), 1);
    assert_eq!(env.eval_real("w.touchScaleThreshold"), 1.5);
    assert_eq!(env.eval_real("w.touchRotateThreshold"), 2.25);
}

#[test]
fn absent_host_facts_raise_typed_errors() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");

    for (expression, needle) in [
        ("w.HWND", "no Win32 window handle"),
        ("w.displayOrientation", "no display is attached"),
        ("w.displayRotate", "no display is attached"),
        (
            "w.findFullScreenCandidates(1, 2, 3, 4, 5)",
            "no platform display-list service",
        ),
    ] {
        let message = env.eval_err(expression);
        assert!(
            message.contains(needle),
            "{expression:?} should report {needle:?}, got: {message}"
        );
    }

    // Argument-count validation stays a typed error too.
    assert!(env.eval("w.findFullScreenCandidates(1, 2, 3, 4)").is_err());
    // Reference `WindowImpl.cpp:2150-2154`: fewer than three
    // `registerMessageReceiver` arguments is `TJS_E_BADPARAMCOUNT`.
    assert!(env.eval("w.registerMessageReceiver(0, 1)").is_err());
}

#[test]
fn draw_device_installs_a_default_and_round_trips() {
    let _lock = vm_lock();
    let env = Env::new();
    // Reference `WindowIntf.cpp:150-166`: constructing a window installs a
    // default `BasicDrawDevice` object.
    env.run("var w = new Window();");

    // Reference `WindowImpl.cpp:2300-2316`: the getter returns the stored
    // draw-device object; the default is a non-null object whose `interface`
    // is the non-zero native payload pointer (`BasicDrawDevice.cpp:886-896`).
    env.run("var d = w.drawDevice; var iface = d.interface;");
    assert_eq!(env.eval_int("d !== null"), 1);
    assert_ne!(env.eval_int("iface"), 0);
    assert_eq!(
        env.eval_int("w.drawDevice.interface"),
        env.eval_int("iface")
    );

    // Reference `WindowIntf.cpp:255-292` via `WindowImpl.cpp:2300-2316`: a
    // replacement object whose `interface` is non-zero is stored, and the
    // getter returns it.
    env.run(
        "var pd = new Window.PassThroughDrawDevice(); \
         var pdIface = pd.interface; \
         w.drawDevice = pd;",
    );
    assert_eq!(env.eval_int("w.drawDevice === pd"), 1);
    assert_eq!(
        env.eval_int("w.drawDevice.interface"),
        env.eval_int("pdIface")
    );
    assert_ne!(env.eval_int("pdIface"), 0);
}

#[test]
fn register_message_receiver_registers_and_unregisters() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");

    // Reference `WindowImpl.cpp:2150-2163`: mode `0` (`wrmRegister`,
    // `WindowImpl.h:20`) and mode `1` (`wrmUnregister`) both return void.
    assert_eq!(
        env.eval_ok("w.registerMessageReceiver(0, 1, 2)"),
        TjsValue::Void
    );
    assert_eq!(
        env.eval_ok("w.registerMessageReceiver(1, 1, 2)"),
        TjsValue::Void
    );
    // An unknown mode is accepted silently (the reference cast
    // `(tTVPWMRRegMode)((tjs_int)*param[0])` has no validation,
    // `WindowImpl.cpp:2156-2159`).
    assert_eq!(
        env.eval_ok("w.registerMessageReceiver(7, 1, 2)"),
        TjsValue::Void
    );
}

#[test]
fn layer_tree_owner_interface_is_the_window_object() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");

    // The reference returns the `iTVPLayerTreeOwner` implemented by the
    // window; L3 hands back the owner object itself (no raw address).
    assert_eq!(env.eval_int("w.layerTreeOwnerInterface === w"), 1);
    // The reference `Window` member list registers no `id`
    // (`reference/cpp/core/visual/WindowIntf.cpp:731`), so reading it raises
    // on both aliases of the same object.
    assert!(
        env.eval("w.layerTreeOwnerInterface.id").is_err(),
        "layerTreeOwnerInterface has no `id` member"
    );
    assert!(env.eval("w.id").is_err(), "Window has no `id` member");
    // It is read-only, matching the reference `TJS_DENY_NATIVE_PROP_SETTER`.
    assert!(env.eval("w.layerTreeOwnerInterface = null").is_err());
}
