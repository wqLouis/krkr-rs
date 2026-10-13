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
        let mut adapter = EventAdapter::new();
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

/// The full reference `on*` dispatch surface with each handler's reference
/// `TVP_ACTION_INVOKE_BEGIN` argument count
/// (`reference/cpp/core/visual/WindowIntf.cpp:934-1250`).
const EVENT_HANDLERS: [(&str, usize); 24] = [
    ("onActivate", 0),
    ("onClick", 2),
    ("onCloseQuery", 1),
    ("onDeactivate", 0),
    ("onDisplayRotate", 5),
    ("onDoubleClick", 2),
    ("onFileDrop", 1),
    ("onKeyDown", 2),
    ("onKeyPress", 1),
    ("onKeyUp", 2),
    ("onMouseDown", 4),
    ("onMouseEnter", 0),
    ("onMouseLeave", 0),
    ("onMouseMove", 3),
    ("onMouseUp", 4),
    ("onMouseWheel", 4),
    ("onMultiTouch", 0),
    ("onPopupHide", 0),
    ("onResize", 0),
    ("onTouchDown", 5),
    ("onTouchMove", 5),
    ("onTouchRotate", 6),
    ("onTouchScaling", 5),
    ("onTouchUp", 5),
];

#[test]
fn event_handlers_are_callable_and_script_assignments_shadow_them() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");

    for (handler, arity) in EVENT_HANDLERS {
        // Calling the default with the reference argument count must not
        // raise: an absent member would, so this also proves every reference
        // handler name is registered. (TJS `typeof` reports a function object
        // as `Object`, so a call is the reliable check for "callable".)
        let args = vec!["0"; arity].join(", ");
        env.run(&format!("w.{handler}({args});"));
    }

    // A script assignment shadows the native default, which is what the L3
    // event adapter resolves by name.
    // reference/cpp/core/visual/WindowIntf.cpp:967-978
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
fn win32_host_facts_return_plausible_values() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window(); var h1 = w.HWND; var h2 = w.HWND;");

    // Reference `WindowImpl.cpp:2288-2298`: `*result =
    // (tTVInteger)(tjs_intptr_t)_this;` — the native instance pointer, which
    // is non-zero and stable across reads (the getter does not mutate it).
    let hwnd = env.eval_int("w.HWND");
    assert_ne!(hwnd, 0, "Window.HWND must be a non-zero instance pointer");
    assert_eq!(env.eval_int("h1"), hwnd);
    assert_eq!(env.eval_int("h2"), hwnd);

    // Reference `TVPWindow.h:138` returns `orientLandscape`, which is `2`
    // (`TVPWindow.h:25-28`: `orientUnknown = 0, orientPortrait = 1,
    // orientLandscape = 2`).
    assert_eq!(env.eval_int("w.displayOrientation"), 2);

    // Reference `TVPWindow.h:137`: `GetDisplayRotate()` returns `0`.
    assert_eq!(env.eval_int("w.displayRotate"), 0);

    // Reference `WindowImpl.cpp:2129-2148`: the body is `#if 0`, so a
    // five-argument call returns `TJS_S_OK` with no result (void).
    assert_eq!(
        env.eval_ok("w.findFullScreenCandidates(0, 0, 0, 0, 0)"),
        TjsValue::Void
    );

    // Reference `WindowImpl.cpp:2130-2131`: fewer than five arguments is
    // `TJS_E_BADPARAMCOUNT`, which stays a typed arity error.
    assert!(env.eval("w.findFullScreenCandidates(1, 2, 3, 4)").is_err());
    // Reference `WindowImpl.cpp:2150-2154`: fewer than three
    // `registerMessageReceiver` arguments is `TJS_E_BADPARAMCOUNT`.
    assert!(env.eval("w.registerMessageReceiver(0, 1)").is_err());
}

/// Reference `reference/cpp/core/base/ScriptMgnIntf.cpp:538-542`: the core
/// script manager attaches a `menu` property to the `Window` class. Its
/// value is a `WindowMenuProperty` (`impl/MenuItemImpl.cpp:65-91`): the
/// getter returns the per-window root `MenuItem`, created on first read
/// through `TVPCreateMenuItemObject(objthis)` (`MenuItemIntf.cpp:559-573`),
/// and the setter returns `TJS_E_ACCESSDENYED`.
#[test]
fn window_menu_is_a_cached_root_menu_item_and_read_only() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window(); var m = w.menu; var again = w.menu;");

    // The property exists on the window and is a `MenuItem` object.
    assert_eq!(env.eval_int("m instanceof 'MenuItem'"), 1);
    // `TVPCreateMenuItemObject(objthis)` builds `new MenuItem(window, window)`
    // (`MenuItemIntf.cpp:559-573`), so the root carries its window and is its
    // own root with no parent.
    assert_eq!(env.eval_int("m.window === w"), 1);
    assert_eq!(env.eval_int("m.root === m"), 1);
    assert_eq!(env.eval_int("m.parent === null"), 1);

    // Repeated reads return the same cached object
    // (`impl/MenuItemImpl.cpp:65-82`).
    assert_eq!(env.eval_int("m === again"), 1);

    // The cache is per window: a second window has its own root.
    env.run("var w2 = new Window(); var m2 = w2.menu;");
    assert_eq!(env.eval_int("m2 === m"), 0);
    assert_eq!(env.eval_int("m2.window === w2"), 1);

    // The setter is access-denied (`impl/MenuItemImpl.cpp:84-91`), surfacing
    // the reference `TJS_E_ACCESSDENYED` message.
    let error = env
        .eval("w.menu = 1")
        .expect_err("Window.menu must be read-only");
    assert!(
        error.contains("Read-only"),
        "Window.menu write must raise TJS_E_ACCESSDENYED, got {error:?}"
    );
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

/// `Window.postInputEvent` posts a synthetic key event through the same
/// dispatch a real input event uses.
///
/// Reference: `tTJSNI_Window::PostInputEvent` accepts `onKeyDown`/`onKeyUp`
/// (reading `key` and `shift`) and `onKeyPress` (reading `key`), then hands
/// the values to the form, which posts the `on*` event
/// (`reference/cpp/core/visual/WindowIntf.cpp:913-930`,
/// `reference/cpp/core/visual/impl/WindowImpl.cpp:1190-1264`).
#[test]
fn post_input_event_delivers_key_events_to_window_and_focused_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.focusable = true; l.visible = true; w.focusedLayer = l; \
         var windowKey = -1; var windowShift = -1; \
         var layerKey = -1; var layerShift = -1; var layerProcess = -1; \
         w.onKeyDown = function(key, shift) { windowKey = key; windowShift = shift; }; \
         l.onKeyDown = function(key, shift, process) { layerKey = key; layerShift = shift; layerProcess = process; };",
    );

    assert_eq!(
        env.eval_ok("w.postInputEvent('onKeyDown', %[key: 13, shift: 0])"),
        TjsValue::Void
    );
    // The window gets `(key, shift)`; the focused layer gets
    // `(key, shift, true)` (`reference/cpp/core/visual/WindowIntf.cpp:485-496`,
    // `reference/cpp/core/visual/LayerIntf.cpp:3844-3854`).
    assert_eq!(env.eval_int("windowKey"), 13);
    assert_eq!(env.eval_int("windowShift"), 0);
    assert_eq!(env.eval_int("layerKey"), 13);
    assert_eq!(env.eval_int("layerShift"), 0);
    assert_eq!(env.eval_int("layerProcess"), 1);
}

/// An unknown event name and a missing `params` object are the reference
/// `TVPSpecifiedEventNameIsUnknown` / `TVPSpecifiedEventNeedsParameter`
/// (`reference/cpp/core/visual/impl/WindowImpl.cpp:1208-1216`).
#[test]
fn post_input_event_rejects_unknown_names_and_missing_parameters() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");

    assert!(env.eval("w.postInputEvent('onMouseDown')").is_err());
    assert!(env.eval("w.postInputEvent('onKeyDown')").is_err());
    assert!(env.eval("w.postInputEvent('onKeyPress', 1)").is_err());
}

/// `onKeyPress` posts the low 8 bits of `key` as a one-character string
/// (`reference/cpp/core/visual/impl/WindowImpl.cpp:1243-1261`).
#[test]
fn post_input_event_key_press_delivers_the_character() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var key = ''; \
         w.onKeyPress = function(k) { key = k; };",
    );

    env.run("w.postInputEvent('onKeyPress', %[key: 65]);");
    assert_eq!(env.eval_int("key == 'A'"), 1);
}
