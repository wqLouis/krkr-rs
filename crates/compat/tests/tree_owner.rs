//! Integration tests for the `BitmapLayerTreeOwner` native class.
//!
//! The tests attach a caller-owned L2 [`Scene`], timer registry and input
//! state to a [`CompatHost`] and drive the class through TJS. Because the host
//! stores raw pointers to those values, they are boxed and declared after
//! `_host` so they outlive it (the fields drop in declaration order).

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
    scene: Box<Scene>,
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
            scene,
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
            .exec_script(script, "tree-owner-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.engine.eval(expression, "tree-owner-test") {
            Ok(TjsValue::Integer(value)) => value,
            Ok(TjsValue::Real(value)) => value as i64,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }
}

#[test]
fn constructor_exposes_the_full_surface_and_identity() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         var owner = new BitmapLayerTreeOwner();",
    );

    // Every `fire*` method and every `on*` hook is present and callable. TJS
    // reports both native methods and functions as `Object`.
    for member in [
        "fireClick",
        "fireDisplayRotate",
        "fireDoubleClick",
        "fireKeyDown",
        "fireKeyPress",
        "fireKeyUp",
        "fireMouseDown",
        "fireMouseMove",
        "fireMouseOutOfWindow",
        "fireMouseUp",
        "fireMouseWheel",
        "fireMultiTouch",
        "fireRecheckInputState",
        "fireReleaseCapture",
        "fireTouchDown",
        "fireTouchMove",
        "fireTouchRotate",
        "fireTouchScaling",
        "fireTouchUp",
        "onChangeLayerImage",
        "onDisableAttentionPoint",
        "onGetCursorPos",
        "onReleaseMouseCapture",
        "onResetImeMode",
        "onResizeLayer",
        "onSetAttentionPoint",
        "onSetCursorPos",
        "onSetHintText",
        "onSetImeMode",
        "onSetMouseCursor",
    ] {
        assert_eq!(
            env.eval_int(&format!("typeof(owner.{member}) == 'Object'")),
            1,
            "{member} is missing"
        );
    }

    // The value properties read as the documented types. `primaryLayer`
    // raises when no primary layer has been attached
    // (`reference/cpp/core/visual/BitmapLayerTreeOwner.cpp:622-624`).
    assert!(
        env.engine
            .eval("owner.primaryLayer", "tree-owner-test")
            .is_err(),
        "a fresh tree owner has no primary layer"
    );
    assert_eq!(env.eval_int("owner.focusedLayer === null"), 1);
    assert_eq!(env.eval_int("owner.width"), 320);
    assert_eq!(env.eval_int("owner.height"), 240);
    assert_eq!(env.eval_int("owner.bitmap != null"), 1);
    // `layerTreeOwnerInterface` is the owner object itself.
    assert_eq!(env.eval_int("owner.layerTreeOwnerInterface === owner"), 1);
}

#[test]
fn focused_layer_setter_mirrors_into_the_scene() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var owner = new BitmapLayerTreeOwner(); \
         owner.focusedLayer = l;",
    );

    assert_eq!(env.eval_int("owner.focusedLayer === l"), 1);

    let layer_id = env.scene.layers[0].id;
    let focused = env.scene.windows[0]
        .focused_layer
        .expect("scene focus was set");
    assert_eq!(focused, layer_id);

    // Clearing with null restores the default and clears the scene focus.
    env.run("owner.focusedLayer = null;");
    assert_eq!(env.eval_int("owner.focusedLayer === null"), 1);
    assert_eq!(env.scene.windows[0].focused_layer, None);
}

#[test]
fn primary_layer_is_read_only_and_the_tree_falls_back_to_the_window() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         l.setSize(100, 77); \
         var owner = new BitmapLayerTreeOwner();",
    );

    // `TJS_DENY_NATIVE_PROP_SETTER` denies writes to `primaryLayer`
    // (`reference/cpp/core/visual/BitmapLayerTreeOwner.cpp:633`).
    assert!(
        env.engine
            .eval("owner.primaryLayer = l", "tree-owner-test")
            .is_err(),
        "primaryLayer must be read-only"
    );
    // The port never attaches a primary layer from script, so the getter
    // raises and the tree bounds fall back to the owner's window.
    assert!(
        env.engine
            .eval("owner.primaryLayer", "tree-owner-test")
            .is_err(),
        "a tree owner with no primary layer raises"
    );
    assert_eq!(env.eval_int("owner.width"), 320);
    assert_eq!(env.eval_int("owner.height"), 240);
}

#[test]
fn fire_dispatches_to_the_focused_layer_and_reports_consumption() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var owner = new BitmapLayerTreeOwner(); \
         owner.focusedLayer = l; \
         var calls = 0; var gotX = 0; var gotY = 0; \
         l.onClick = function(x, y) { calls++; gotX = x; gotY = y; return true; };",
    );

    // A handler that returns true consumes the event.
    assert_eq!(env.eval_int("owner.fireClick(11, 22)"), 1);
    assert_eq!(env.eval_int("calls"), 1);
    assert_eq!(env.eval_int("gotX"), 11);
    assert_eq!(env.eval_int("gotY"), 22);

    // The no-op default returns void: the event is not consumed.
    assert_eq!(env.eval_int("owner.fireDoubleClick(1, 2)"), 0);

    // An explicit `false` declines the event.
    env.run("l.onMouseDown = function(x, y, b, s) { return false; };");
    assert_eq!(env.eval_int("owner.fireMouseDown(1, 2, 0, 0)"), 0);

    // The payload carries the button and shift.
    env.run(
        "var gotButton = -1; var gotShift = -1; \
         l.onMouseDown = function(x, y, b, s) { gotButton = b; gotShift = s; return true; };",
    );
    assert_eq!(env.eval_int("owner.fireMouseDown(3, 4, 2, 8)"), 1);
    assert_eq!(env.eval_int("gotButton"), 2);
    assert_eq!(env.eval_int("gotShift"), 8);
}

#[test]
fn fire_arguments_default_to_the_l2_input_state() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var owner = new BitmapLayerTreeOwner(); \
         owner.focusedLayer = l; \
         var gotX = -1; var gotY = -1; \
         l.onClick = function(x, y) { gotX = x; gotY = y; return true; }; \
         var key = -1; \
         l.onKeyPress = function(k) { key = k; return true; };",
    );

    env.feed_input(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 100.0,
        y: 50.0,
    })]);
    // No explicit coordinates: the pointer is read from `InputState`.
    assert_eq!(env.eval_int("owner.fireClick()"), 1);
    assert_eq!(env.eval_int("gotX"), 100);
    assert_eq!(env.eval_int("gotY"), 50);

    env.feed_input(&[PlatformEvent::Input(InputEvent::Text("A".to_string()))]);
    assert_eq!(env.eval_int("owner.fireKeyPress()"), 1);
    assert_eq!(env.eval_int("key"), 65);
}

#[test]
fn owner_hooks_are_assignable_with_a_callable_noop_default() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var owner = new BitmapLayerTreeOwner(); \
         var last = -1; \
         owner.onSetMouseCursor = function(cursor) { last = cursor; }; \
         owner.onSetMouseCursor(7);",
    );

    assert_eq!(env.eval_int("last"), 7);
    // The default is a callable no-op, so a direct call never raises.
    env.run("owner.onResetImeMode(); owner.onChangeLayerImage();");
    assert_eq!(env.eval_int("typeof(owner.onGetCursorPos) == 'Object'"), 1);
}

#[test]
fn touch_and_wheel_payloads_reach_the_handlers() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var owner = new BitmapLayerTreeOwner(); \
         owner.focusedLayer = l; \
         var shift = -1; var delta = 0; \
         l.onMouseWheel = function(s, d, x, y) { shift = s; delta = d; return true; }; \
         var touchId = -1; var tx = 0.0; \
         l.onTouchDown = function(x, y, cx, cy, id) { tx = x; touchId = id; return true; };",
    );

    assert_eq!(env.eval_int("owner.fireMouseWheel(4, -1, 9, 9)"), 1);
    assert_eq!(env.eval_int("shift"), 4);
    assert_eq!(env.eval_int("delta"), -1);

    assert_eq!(
        env.eval_int("owner.fireTouchDown(1.5, 2.5, 0.0, 0.0, 42)"),
        1
    );
    assert_eq!(env.eval_int("tx == 1.5"), 1);
    assert_eq!(env.eval_int("touchId"), 42);
}

#[test]
fn release_capture_consults_the_owner_hook() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var owner = new BitmapLayerTreeOwner(); \
         owner.focusedLayer = l; \
         var released = 0; \
         owner.onReleaseMouseCapture = function() { released++; }; \
         owner.fireReleaseCapture();",
    );

    assert_eq!(env.eval_int("released"), 1);
}
