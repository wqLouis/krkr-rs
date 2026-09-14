//! Per-member tests for the native `Window` class ported from
//! `reference/cpp/core/visual/WindowIntf.cpp` and
//! `reference/cpp/core/visual/impl/WindowImpl.cpp`:
//!
//! * event dispatch (`TVP_ACTION_INVOKE` → `objthis.action(event)`),
//! * geometry (`setSize`/`setMinSize`/`setMaxSize`/`setPos`, `width`/`height`,
//!   `min*`/`max*`, `left`/`top`, `onResize`),
//! * persisted state (`focusable`, `useMouseKey`, `trapKey`, `imeMode`,
//!   `mouseCursorState`, `waitVSync`, `enableTouch`, `hintDelay`, zoom,
//!   `fullScreen`, `showModal`, mask regions, `focusedLayer`),
//! * touch tracking (`onTouchDown`/`Move`/`Up`, `touchPointCount`,
//!   `getTouchPoint`),
//! * host read-only members (`HWND`, `layerTreeOwnerInterface`, `drawDevice`,
//!   `displayOrientation`, `displayRotate`, `mainWindow`).
//!
//! Headless: no window/GPU — the natives only mutate the shared
//! [`tvp_visual::scene::Scene`] plus the per-instance state, which the test
//! drives exactly like the game / input bridge does.

use std::sync::{Arc, Mutex, RwLock};

use engine::Storage;
use tempfile::TempDir;
use tjs2_sys::Tjs2Engine;
use tvp_visual::scene::Scene;

/// Fresh engine + scene + temp storage with the visual natives registered.
/// The crate-global VM context is shared, so the process-wide test lock is
/// held for the environment's lifetime (mirrors `tests/layer_color_ops.rs`).
struct Env {
    engine: Arc<Tjs2Engine>,
    scene: Arc<RwLock<Scene>>,
    #[allow(dead_code)]
    storage: Arc<Mutex<Storage>>,
    _dir: TempDir,
    _vm_lock: std::sync::MutexGuard<'static, ()>,
}

impl Env {
    fn new() -> Self {
        let _vm_lock = tvp_visual::natives::vm_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("temp dir");
        let storage = Arc::new(Mutex::new(
            Storage::mount(dir.path()).expect("mount temp game dir"),
        ));
        let scene = Arc::new(RwLock::new(Scene::default()));
        let engine = Arc::new(Tjs2Engine::new().expect("engine"));
        tvp_visual::register_visual(&engine, scene.clone(), storage.clone())
            .expect("register visual natives");
        Env {
            engine,
            scene,
            storage,
            _dir: dir,
            _vm_lock,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "window_test")
            .expect("script must run");
    }

    fn eval_int(&self, expr: &str) -> i64 {
        match self.engine.eval(expr, "window_test") {
            Ok(tjs2_sys::TjsValue::Integer(i)) => i,
            other => panic!("eval {expr:?} -> {other:?}"),
        }
    }

    fn eval_real(&self, expr: &str) -> f64 {
        match self.engine.eval(expr, "window_test") {
            Ok(tjs2_sys::TjsValue::Real(v)) => v,
            Ok(tjs2_sys::TjsValue::Integer(v)) => v as f64,
            other => panic!("eval {expr:?} -> {other:?}"),
        }
    }

    fn eval_string(&self, expr: &str) -> String {
        match self.engine.eval(expr, "window_test") {
            Ok(tjs2_sys::TjsValue::String(s)) => s,
            other => panic!("eval {expr:?} -> {other:?}"),
        }
    }

    fn scene(&self) -> std::sync::RwLockReadGuard<'_, Scene> {
        self.scene.read().expect("scene lock poisoned")
    }
}

/// `Window` event methods dispatch to `objthis.action(event)` with the
/// reference member names (`TVP_ACTION_INVOKE`, `WindowIntf.cpp:934`).
#[test]
fn window_events_dispatch_to_action() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var got = ''; \
         var target_ok = false; \
         w.action = function(ev) { \
             got = ev.type; \
             if (ev.x !== void) got += ':' + ev.x + ',' + ev.y; \
             if (ev.button !== void) got += ':' + ev.button + ',' + ev.shift; \
             if (ev.key !== void) got += ':' + ev.key; \
             if (ev.delta !== void) got += ':' + ev.delta; \
             if (ev.ID !== void) got += ':' + ev.ID; \
             if (ev.orientation !== void) got += ':' + ev.orientation + ',' + ev.angle + ',' + ev.bpp; \
             target_ok = (ev.target === w); \
         };",
    );

    env.run("w.onResize();");
    assert_eq!(env.eval_string("got"), "onResize");
    assert_eq!(env.eval_int("target_ok"), 1);

    env.run("w.onMouseEnter(); w.onMouseLeave(); w.onMultiTouch(); w.onPopupHide();");
    assert_eq!(env.eval_string("got"), "onPopupHide");

    env.run("w.onClick(3, 4);");
    assert_eq!(env.eval_string("got"), "onClick:3,4");

    env.run("w.onDoubleClick(5, 6);");
    assert_eq!(env.eval_string("got"), "onDoubleClick:5,6");

    env.run("w.onMouseDown(1, 2, 3, 4);");
    assert_eq!(env.eval_string("got"), "onMouseDown:1,2:3,4");
    env.run("w.onMouseUp(1, 2, 0, 8);");
    assert_eq!(env.eval_string("got"), "onMouseUp:1,2:0,8");
    env.run("w.onMouseMove(7, 8, 9);");
    assert_eq!(env.eval_string("got"), "onMouseMove:7,8");
    env.run("w.onMouseWheel(1, -120, 9, 10);");
    assert_eq!(env.eval_string("got"), "onMouseWheel:9,10:-120");

    env.run("w.onKeyDown(65, 1);");
    assert_eq!(env.eval_string("got"), "onKeyDown:65");
    env.run("w.onKeyUp(65, 0);");
    assert_eq!(env.eval_string("got"), "onKeyUp:65");
    env.run("w.onKeyPress(97);");
    assert_eq!(env.eval_string("got"), "onKeyPress:97");

    env.run("w.onDisplayRotate(1, 2, 3, 4, 5);");
    assert_eq!(env.eval_string("got"), "onDisplayRotate:1,2,3");
}

/// Touch events track their points; `touchPointCount` / `getTouchPoint`
/// expose them and `onTouchUp` removes the point.
#[test]
fn window_touch_points_track_and_dispatch() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var got = ''; \
         w.action = function(ev){ if (ev.type == 'onTouchDown') got = ev.x + ',' + ev.y + ',' + ev.cx + ',' + ev.cy + ',' + ev.id; };",
    );
    env.run("w.onTouchDown(1.5, 2.5, 3.5, 4.5, 7);");
    assert_eq!(env.eval_string("got"), "1.5,2.5,3.5,4.5,7");
    assert_eq!(env.eval_int("w.touchPointCount"), 1);
    assert_eq!(env.eval_int("w.getTouchPoint(0).ID"), 7);
    assert_eq!(env.eval_real("w.getTouchPoint(0).startX"), 1.5);
    assert_eq!(env.eval_real("w.getTouchPoint(0).startY"), 2.5);
    assert_eq!(env.eval_real("w.getTouchPoint(0).x"), 3.5);

    env.run("w.onTouchMove(1.5, 2.5, 30.5, 40.5, 7);");
    assert_eq!(env.eval_real("w.getTouchPoint(0).x"), 30.5);
    // The start position is retained across a move.
    assert_eq!(env.eval_real("w.getTouchPoint(0).startX"), 1.5);

    env.run("w.onTouchUp(1.5, 2.5, 30.5, 40.5, 7);");
    assert_eq!(env.eval_int("w.touchPointCount"), 0);
}

/// `postInputEvent(name, params)` synthesizes a keyboard action event.
#[test]
fn window_post_input_event_dispatches() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var got = ''; \
         w.action = function(ev){ got = ev.type + ':' + ev.key; if (ev.shift !== void) got += ',' + ev.shift; };",
    );
    env.run("w.postInputEvent('onKeyDown', %[key:65, shift:1]);");
    assert_eq!(env.eval_string("got"), "onKeyDown:65,1");
    env.run("w.postInputEvent('onKeyUp', %[key:66, shift:0]);");
    assert_eq!(env.eval_string("got"), "onKeyUp:66,0");
    env.run("w.postInputEvent('onKeyPress', %[key:97]);");
    assert_eq!(env.eval_string("got"), "onKeyPress:97");
}

/// Geometry: size, min/max, position, and the `onResize` fired on a real
/// client-size change.
#[test]
fn window_geometry_roundtrips_and_fires_resize() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var resized = 0; var last = ''; \
         w.action = function(ev){ if (ev.type == 'onResize') resized++; };",
    );
    env.run("w.setInnerSize(640, 480);");
    assert_eq!(env.eval_int("w.width"), 640);
    assert_eq!(env.eval_int("w.height"), 480);
    assert_eq!(env.eval_int("w.innerWidth"), 640);
    assert_eq!(env.eval_int("w.innerHeight"), 480);
    assert_eq!(env.eval_int("resized"), 1);

    // Setting the same size does not re-fire.
    env.run("w.setInnerSize(640, 480);");
    assert_eq!(env.eval_int("resized"), 1);

    // `width`/`height` setters resize too.
    env.run("w.width = 800; w.height = 600;");
    assert_eq!(env.eval_int("resized"), 3);
    assert_eq!(env.eval_int("w.innerWidth"), 800);

    env.run("w.setSize(1024, 768);");
    assert_eq!(env.eval_int("resized"), 4);
    assert_eq!(env.eval_int("w.width"), 1024);
    assert_eq!(env.eval_int("w.height"), 768);

    // A script override of `onResize` shadows the native dispatcher.
    env.run("var direct = 0; w.onResize = function(){ direct++; }; w.setSize(320, 240);");
    assert_eq!(env.eval_int("direct"), 1);
    assert_eq!(env.eval_int("resized"), 4);

    env.run("w.setMinSize(200, 150);");
    assert_eq!(env.eval_int("w.minWidth"), 200);
    assert_eq!(env.eval_int("w.minHeight"), 150);
    env.run("w.minWidth = 210; w.minHeight = 160;");
    assert_eq!(env.eval_int("w.minWidth"), 210);
    assert_eq!(env.eval_int("w.minHeight"), 160);

    env.run("w.setMaxSize(1920, 1080);");
    assert_eq!(env.eval_int("w.maxWidth"), 1920);
    assert_eq!(env.eval_int("w.maxHeight"), 1080);
    env.run("w.maxWidth = 1280; w.maxHeight = 720;");
    assert_eq!(env.eval_int("w.maxWidth"), 1280);
    assert_eq!(env.eval_int("w.maxHeight"), 720);

    env.run("w.setPos(12, 34);");
    assert_eq!(env.eval_int("w.left"), 12);
    assert_eq!(env.eval_int("w.top"), 34);
    env.run("w.left = 56; w.top = 78;");
    assert_eq!(env.eval_int("w.left"), 56);
    assert_eq!(env.eval_int("w.top"), 78);
}

/// Persisted state transitions.
#[test]
fn window_state_roundtrips() {
    let env = Env::new();
    env.run("var w = new Window();");

    // Reference defaults.
    assert_eq!(env.eval_int("w.focusable"), 1);
    assert_eq!(env.eval_int("w.hintDelay"), 500);
    assert_eq!(env.eval_real("w.touchScaleThreshold"), 5.0);
    assert_eq!(env.eval_real("w.touchRotateThreshold"), 5.0);
    assert_eq!(env.eval_int("w.zoomNumer"), 1);
    assert_eq!(env.eval_int("w.zoomDenom"), 1);
    assert_eq!(env.eval_int("w.mouseCursorState"), 0);
    assert_eq!(env.eval_int("w.fullScreen"), 0);

    env.run(
        "w.focusable = false; w.useMouseKey = true; w.trapKey = true; \
         w.imeMode = 3; w.waitVSync = true; w.enableTouch = true; \
         w.hintDelay = 100; w.touchScaleThreshold = 3.5; w.touchRotateThreshold = 4.25; \
         w.setZoom(2, 3); w.fullScreen = true;",
    );
    assert_eq!(env.eval_int("w.focusable"), 0);
    assert_eq!(env.eval_int("w.useMouseKey"), 1);
    assert_eq!(env.eval_int("w.trapKey"), 1);
    assert_eq!(env.eval_int("w.imeMode"), 3);
    assert_eq!(env.eval_int("w.waitVSync"), 1);
    assert_eq!(env.eval_int("w.enableTouch"), 1);
    assert_eq!(env.eval_int("w.hintDelay"), 100);
    assert_eq!(env.eval_real("w.touchScaleThreshold"), 3.5);
    assert_eq!(env.eval_real("w.touchRotateThreshold"), 4.25);
    assert_eq!(env.eval_int("w.zoomNumer"), 2);
    assert_eq!(env.eval_int("w.zoomDenom"), 3);
    assert_eq!(env.eval_int("w.fullScreen"), 1);

    // 3-state cursor: explicit state, then `hideMouseCursor` → temp hidden.
    env.run("w.mouseCursorState = 2;");
    assert_eq!(env.eval_int("w.mouseCursorState"), 2);
    env.run("w.hideMouseCursor();");
    assert_eq!(env.eval_int("w.mouseCursorState"), 1);

    env.run("w.zoomNumer = 4;");
    assert_eq!(env.eval_int("w.zoomNumer"), 4);
    assert_eq!(env.eval_int("w.zoomDenom"), 3);
}

/// `showModal` + `onCloseQuery`: a modal window is not hidden when the query
/// allows closing; a non-modal one is.
#[test]
fn window_modal_and_close_query() {
    let env = Env::new();
    env.run("var w = new Window(); w.onCloseQuery(true);");
    assert_eq!(env.eval_int("w.visible"), 0, "non-modal close hides");

    env.run("var m = new Window(); m.showModal(); m.onCloseQuery(true);");
    assert_eq!(env.eval_int("m.visible"), 1, "modal close stays visible");
    env.run("m.onCloseQuery(false);");
    assert_eq!(env.eval_int("m.visible"), 1);
}

/// `setMaskRegion` requires a primary layer (reference `TVPWindowHasNoLayer`).
#[test]
fn window_mask_region_requires_layer() {
    let env = Env::new();
    env.run("var w = new Window();");
    // Without a primary layer the call throws (catchable).
    assert_eq!(
        env.eval_int(
            "(function(){ try { w.setMaskRegion(2); return 1; } catch(e) { return 0; } })()"
        ),
        0
    );
    env.run("var l = new Layer(w, null); w.setMaskRegion(2);");
    assert_eq!(env.eval_int("w.primaryLayer === null"), 0);
    // The threshold is persisted and removable; no error after a layer exists.
    env.run("w.removeMaskRegion();");
}

/// `focusedLayer` accepts a Layer object and returns the same object;
/// `mainWindow` returns the first window's object.
#[test]
fn window_focused_layer_and_main_window() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         var focused = w.focusedLayer; \
         w.focusedLayer = l; \
         var again = w.focusedLayer; \
         var mw = w.mainWindow;",
    );
    assert_eq!(env.eval_int("focused === null"), 1, "no focus by default");
    assert_eq!(env.eval_int("again.id"), env.eval_int("l.id"));
    assert_eq!(env.eval_int("mw.id"), env.eval_int("w.id"));
    // The focus holder is shared with the render-facing scene state.
    let win_id = env.eval_int("w.id") as u32;
    let layer_id = env.eval_int("l.id") as u32;
    let scene = env.scene();
    assert_eq!(scene.window(win_id).unwrap().focused_layer, Some(layer_id));
    drop(scene);

    // Clearing focus with null.
    env.run("w.focusedLayer = null; var cleared = w.focusedLayer;");
    assert_eq!(env.eval_int("cleared === null"), 1);
    let scene = env.scene();
    assert_eq!(scene.window(win_id).unwrap().focused_layer, None);
}

/// Host read-only members: `HWND`/`layerTreeOwnerInterface` are opaque
/// non-zero tokens; `drawDevice` is documented inert (`null`);
/// `displayOrientation`/`displayRotate` report the headless defaults.
#[test]
fn window_host_readonly_members() {
    let env = Env::new();
    env.run("var w = new Window();");
    assert_ne!(env.eval_int("w.HWND"), 0, "opaque native-instance token");
    assert_eq!(
        env.eval_int("w.HWND"),
        env.eval_int("w.layerTreeOwnerInterface")
    );
    assert_eq!(env.eval_int("w.drawDevice === null"), 1);
    assert_eq!(env.eval_int("w.displayOrientation"), 0);
    assert_eq!(env.eval_int("w.displayRotate"), -1);
    // The setter is accepted and ignored.
    env.run("w.drawDevice = %[];");
    assert_eq!(env.eval_int("w.drawDevice === null"), 1);
}

/// Velocity getters return `0` (no tracker; the ABI cannot write back the
/// out parameters), and the argument-checked no-ops accept their arguments.
#[test]
fn window_velocity_and_fullscreen_noops() {
    let env = Env::new();
    env.run("var w = new Window();");
    assert_eq!(env.eval_int("w.getMouseVelocity(0, 0, 0)"), 0);
    assert_eq!(env.eval_int("w.getTouchVelocity(0, 0, 0, 0)"), 0);
    env.run(
        "w.resetMouseVelocity(); \
         w.findFullScreenCandidates(1280, 720, 32, 0, 0); \
         w.registerMessageReceiver(0, 0, 0);",
    );
}

/// `onFileDrop(files)` passes the object through to the action.
#[test]
fn window_file_drop_dispatches_files() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var got = ''; \
         w.action = function(ev){ got = ev.type + ':' + ev.files.count; };",
    );
    env.run("w.onFileDrop(%[count:2]);");
    assert_eq!(env.eval_string("got"), "onFileDrop:2");
}
