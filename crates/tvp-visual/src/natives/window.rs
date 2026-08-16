//! `__TvpWindow` — the native implementation class behind the script
//! `Window` wrapper (see `natives/mod.rs` for the architecture).
//!
//! Every instance payload holds the scene window id plus a constructed
//! flag. The class-name method (`__TvpWindow`) is the constructor hook:
//! `new Window()` runs the script wrapper's constructor, which calls
//! `super.__TvpWindow()`; the native adds a window to the scene and returns
//! its id. `destroy` removes the window and its layers from the scene.
//!
//! Surface (mirrors `reference/cpp/core/visual/WindowIntf.cpp` for the
//! subset the game uses; the rest are documented no-ops — input handling,
//! OS-level window chrome and fullscreen are beyond milestone 3A):
//!
//! | member | behavior |
//! |---|---|
//! | `__TvpWindow()` | add a window to the scene; return its id |
//! | `__setInnerSize(w, h)` | set the logical client size |
//! | `__setSize(w, h)` / `__setPos(x, y)` / `__setZoom(zx, zy)` | no-ops (OS chrome) |
//! | `__bringToFront()` | no-op (single-window milestone) |
//! | `__close()` | remove the window (and its layers) from the scene |
//! | `__visible(v?)` / `__caption(v?)` | window visibility / title |
//! | `__width(v?)` / `__height(v?)` | alias of the inner size |
//! | `__fullScreen(v?)` / `__stayOnTop(v?)` | stubs (no-op, stored nothing) |
//! | `__getPrimaryLayerId()` | the primary layer's scene id (-1 if none) |
//! | `__addLayer(layer_id)` / `__removeLayer(layer_id)` | attach/detach a layer |
//! | `__update()` / `__hideMouseCursor()` | no-ops |

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine, Value,
};

use crate::scene::Scene;

use super::ffi::{arg_bool, arg_i64, error_out, instance_ref, set_int_out, set_void_out};
use super::{context_scene_mut, context_scene_read};

/// Payload of one script-visible `Window` object.
#[derive(Default)]
pub(crate) struct WindowInst {
    #[cfg_attr(not(test), allow(dead_code))]
    style: WindowStyleState,
    /// Scene window id, assigned by the constructor.
    pub id: u32,
    /// Whether the native constructor has run.
    pub constructed: bool,
}

/// `new Window()` payload factory.
extern "C" fn window_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::<WindowInst>::default()) as *mut c_void
}

/// Release a `Window` payload; removes the window (and its layers) from the
/// scene.
extern "C" fn window_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: the trampoline passes the payload from window_create.
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    if inst.constructed {
        let mut scene = context_scene_mut();
        remove_window(&mut scene, inst.id);
    }
    // SAFETY: instance came from Box::into_raw.
    unsafe { drop(Box::from_raw(instance as *mut WindowInst)) };
}

/// Remove a window and every layer attached to it from the scene.
fn remove_window(scene: &mut Scene, id: u32) {
    let layer_ids: Vec<u32> = scene
        .layers
        .iter()
        .filter(|l| l.window == id)
        .map(|l| l.id)
        .collect();
    for layer in layer_ids {
        scene.remove_layer(layer);
    }
    scene.windows.retain(|w| w.id != id);
}

/// `__TvpWindow()` — the constructor hook. Returns the new window's scene
/// id so the script wrapper can record it as `this.__id`.
extern "C" fn window_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a WindowInst payload; out/out_error are valid.
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    if inst.constructed {
        return error_out(out_error, "Window: this window is already constructed");
    }
    let mut scene = context_scene_mut();
    // Default client size for a fresh window (the reference opens with a
    // default size; the game immediately calls setInnerSize, but the
    // default keeps `win.width`/`win.height` non-zero for scripts that read
    // them before sizing).
    let id = scene.add_window(String::new(), (1280, 720));
    inst.id = id;
    inst.constructed = true;
    if !_objthis.is_null() {
        super::set_window_tjs_object(id, _objthis);
    }
    set_int_out(out, i64::from(id));
    0
}

/// `__close()` — remove the window from the scene (its layers go with it).
extern "C" fn window_close(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    if inst.constructed {
        let mut scene = context_scene_mut();
        remove_window(&mut scene, inst.id);
        inst.constructed = false;
    }
    set_void_out(out);
    0
}

/// `__setInnerSize(w, h)` — set the game-logical client size.
extern "C" fn window_set_inner_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Window.setInnerSize requires 2 arguments");
    }
    let w = arg_i64(&args[0]).max(0) as u32;
    let h = arg_i64(&args[1]).max(0) as u32;
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    if let Some(win) = context_scene_mut().window_mut(inst.id) {
        win.inner_size = (w, h);
    }
    set_void_out(out);
    0
}

/// `visible` — window visibility (get/set).
extern "C" fn window_visible_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    let scene = context_scene_read();
    let Some(win) = scene.window(inst.id) else {
        return error_out(out_error, "Window: window no longer exists");
    };
    set_int_out(out, win.visible as i64);
    0
}

extern "C" fn window_visible_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(win) = scene.window_mut(inst.id) else {
        return 1;
    };
    win.visible = arg_bool(v);
    0
}

/// `width` — game-logical client width (get).
extern "C" fn window_width_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    let scene = context_scene_read();
    let Some(win) = scene.window(inst.id) else {
        return error_out(out_error, "Window: window no longer exists");
    };
    set_int_out(out, i64::from(win.inner_size.0));
    0
}

/// `height` — game-logical client height (get).
extern "C" fn window_height_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    let scene = context_scene_read();
    let Some(win) = scene.window(inst.id) else {
        return error_out(out_error, "Window: window no longer exists");
    };
    set_int_out(out, i64::from(win.inner_size.1));
    0
}

/// `caption` — window title (get/set).
extern "C" fn window_caption_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    let scene = context_scene_read();
    let Some(win) = scene.window(inst.id) else {
        return error_out(out_error, "Window: window no longer exists");
    };
    super::ffi::set_string_out(out, &win.title);
    0
}

extern "C" fn window_caption_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is valid for the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    let mut scene = context_scene_mut();
    let Some(win) = scene.window_mut(inst.id) else {
        return 1;
    };
    win.title = super::ffi::arg_string(v);
    0
}

/// `primaryLayer` — the scene id of the window's primary layer, or -1 when
/// none is attached (object returns are pending; see the builder).
extern "C" fn window_primary_layer_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    // Return the primary Layer's TJS object (retained), so scripts can do
    // `with(primaryLayer){ .setSize(...) ... }` like the real engine.
    let layer_id = context_scene_read()
        .window(inst.id)
        .and_then(|w| w.primary_layer);
    match layer_id.and_then(|id| {
        let obj = super::layer_tjs_object(id);
        if obj.is_null() { None } else { Some(obj) }
    }) {
        Some(obj) => {
            let engine = crate::natives::context_engine();
            // SAFETY: engine is the registered engine; obj is a live TJS
            // object for the duration of the process.
            let rid = unsafe { tjs2_sys::tjs2_retain_object(engine.raw(), obj) };
            if !rid.is_null() {
                // SAFETY: out is a valid result slot.
                unsafe {
                    (*out).ty = tjs2_sys::VAL_RETAINED;
                    (*out).integer = 0;
                    (*out).real = 0.0;
                    (*out).string = std::ptr::null();
                    (*out).array = std::ptr::null();
                    (*out).array_count = 0;
                    (*out).retained = rid as usize;
                }
                return 0;
            }
            set_int_out(out, -1);
            0
        }
        None => {
            set_int_out(out, -1);
            0
        }
    }
}

/// `add(layer)` / `remove(layer)` — no-ops: the real TVP methods take Layer
/// **objects**, which the FFI cannot resolve yet (object arguments are
/// pending). Layers are attached to their window at construction and
/// removed with `close`, so the scene stays consistent without them.
extern "C" fn window_add_remove_noop(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `id` — the window's scene id (read-only; the FFI cannot return object
/// handles, so scripts address scene objects by their integer id).
extern "C" fn window_id_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    set_int_out(out, i64::from(inst.id));
    0
}

/// `__update()` — no-op (the render loop syncs every frame).
extern "C" fn window_update(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `__bringToFront()` — no-op (single-window milestone).
extern "C" fn window_bring_to_front(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `__hideMouseCursor()` — no-op (input is beyond milestone 3A).
extern "C" fn window_hide_mouse_cursor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// `__setSize(w, h)` / `__setPos(x, y)` / `__setZoom(numer, denom)` — OS
/// window-chrome no-ops; the game-logical size is `__setInnerSize`.
extern "C" fn window_noop2(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// Register the `__TvpWindow` native class.
pub(crate) fn register_window(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "Window",
        create: window_create,
        destroy: window_destroy,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Window",
                f: window_ctor,
            },
            NativeInstanceMethodDef {
                name: "setInnerSize",
                f: window_set_inner_size,
            },
            NativeInstanceMethodDef {
                name: "setSize",
                f: window_noop2,
            },
            NativeInstanceMethodDef {
                name: "setPos",
                f: window_noop2,
            },
            NativeInstanceMethodDef {
                name: "setZoom",
                f: window_noop2,
            },
            NativeInstanceMethodDef {
                name: "bringToFront",
                f: window_bring_to_front,
            },
            NativeInstanceMethodDef {
                name: "close",
                f: window_close,
            },
            NativeInstanceMethodDef {
                name: "update",
                f: window_update,
            },
            NativeInstanceMethodDef {
                name: "hideMouseCursor",
                f: window_hide_mouse_cursor,
            },
            NativeInstanceMethodDef {
                name: "addInputNotify",
                f: window_noop2,
            },
            NativeInstanceMethodDef {
                name: "add",
                f: window_add_remove_noop,
            },
            NativeInstanceMethodDef {
                name: "changeScreenMode",
                f: window_noop2,
            },
            NativeInstanceMethodDef {
                name: "registerExEvent",
                f: window_noop2,
            },
            NativeInstanceMethodDef {
                name: "remove",
                f: window_add_remove_noop,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "id",
                get: Some(window_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "visible",
                get: Some(window_visible_get),
                set: Some(window_visible_set),
            },
            NativeInstancePropertyDef {
                name: "caption",
                get: Some(window_caption_get),
                set: Some(window_caption_set),
            },
            NativeInstancePropertyDef {
                name: "width",
                get: Some(window_width_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(window_height_get),
                set: None,
            },
            // Object returns are not supported by the FFI yet; expose the
            // primary layer's scene id (the game's init path passes it as a
            // parent id, which the Layer ctor accepts as an int).
            NativeInstancePropertyDef {
                name: "primaryLayer",
                get: Some(window_primary_layer_get),
                set: None,
            },
            // Window style/config properties the game's MainWindow sets.
            // They are accepted and stored as no-ops (styling is not
            // relevant to the headless load path).
            NativeInstancePropertyDef {
                name: "borderStyle",
                get: Some(window_int_prop_get_bs),
                set: Some(window_int_prop_set_bs),
            },
            NativeInstancePropertyDef {
                name: "innerSunken",
                get: Some(window_bool_prop_get_is),
                set: Some(window_bool_prop_set_is),
            },
            NativeInstancePropertyDef {
                name: "showScrollBars",
                get: Some(window_bool_prop_get_ssb),
                set: Some(window_bool_prop_set_ssb),
            },
            NativeInstancePropertyDef {
                name: "zoom",
                get: Some(window_real_prop_get_zoom),
                set: Some(window_real_prop_set_zoom),
            },
            NativeInstancePropertyDef {
                name: "stayOnTop",
                get: Some(window_bool_prop_get_sot),
                set: Some(window_bool_prop_set_sot),
            },
            NativeInstancePropertyDef {
                name: "fullScreen",
                get: Some(window_bool_prop_get_fs),
                set: Some(window_bool_prop_set_fs),
            },
        ],
    })
}

// ---------------------------------------------------------------------------
// window style property no-ops (stored per-instance so reads return what
// was written, matching the reference's persistent members).
// ---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct WindowStyleState {
    border_style: i64,
    inner_sunken: bool,
    show_scroll_bars: bool,
    zoom: f64,
    stay_on_top: bool,
}

// ---------------------------------------------------------------------------
// window style property callbacks
// ---------------------------------------------------------------------------

fn style(instance: *mut c_void) -> &'static mut WindowStyleState {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    &mut inst.style
}

extern "C" fn window_int_prop_get_bs(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, style(instance).border_style);
    0
}
extern "C" fn window_int_prop_set_bs(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let v = unsafe { &*value };
    style(instance).border_style = v.integer;
    0
}
extern "C" fn window_bool_prop_get_is(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, i64::from(style(instance).inner_sunken));
    0
}
extern "C" fn window_bool_prop_set_is(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let v = unsafe { &*value };
    style(instance).inner_sunken = v.integer != 0;
    0
}
extern "C" fn window_bool_prop_get_ssb(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, i64::from(style(instance).show_scroll_bars));
    0
}
extern "C" fn window_bool_prop_set_ssb(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let v = unsafe { &*value };
    style(instance).show_scroll_bars = v.integer != 0;
    0
}
extern "C" fn window_real_prop_get_zoom(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: out is a valid result slot.
    unsafe {
        (*out).ty = tjs2_sys::VAL_REAL;
        (*out).integer = 0;
        (*out).real = style(instance).zoom;
        (*out).string = std::ptr::null();
    }
    0
}
extern "C" fn window_real_prop_set_zoom(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let v = unsafe { &*value };
    style(instance).zoom = v.real;
    0
}
extern "C" fn window_bool_prop_get_sot(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, i64::from(style(instance).stay_on_top));
    0
}
extern "C" fn window_bool_prop_get_fs(
    _e: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, 0);
    0
}
extern "C" fn window_bool_prop_set_fs(
    _e: *mut c_void,
    _instance: *mut c_void,
    _value: *const Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    0
}

extern "C" fn window_bool_prop_set_sot(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let v = unsafe { &*value };
    style(instance).stay_on_top = v.integer != 0;
    0
}

#[cfg(test)]
mod tests {
    use crate::natives::tests::TestEnv;

    #[test]
    fn window_construct_and_size() {
        let env = TestEnv::new("window-ctor");
        env.run("var w = new Window(); w.setInnerSize(1280, 720);")
            .unwrap();
        let scene = env.scene();
        assert_eq!(scene.windows.len(), 1);
        assert_eq!(scene.windows[0].inner_size, (1280, 720));
        // the native object exposes its scene id
        assert_eq!(env.eval_int("w.id"), 0);
        assert_eq!(env.eval_int("w.width"), 1280);
        assert_eq!(env.eval_int("w.height"), 720);
    }

    #[test]
    fn window_default_inner_size() {
        let env = TestEnv::new("window-default-size");
        env.run("var w = new Window();").unwrap();
        let scene = env.scene();
        // the ctor defaults to a sensible client size so width/height are
        // non-zero before the game calls setInnerSize
        assert_eq!(scene.windows[0].inner_size, (1280, 720));
        assert_eq!(env.eval_int("w.width"), 1280);
        assert_eq!(env.eval_int("w.height"), 720);
    }

    #[test]
    fn window_caption_and_visible() {
        let env = TestEnv::new("window-props");
        env.run("var w = new Window(); w.caption = 'hello'; w.visible = false;")
            .unwrap();
        let scene = env.scene();
        assert_eq!(scene.windows[0].title, "hello");
        assert!(!scene.windows[0].visible);
        // reads round-trip through the script
        assert_eq!(env.eval_string("w.caption"), "hello");
        assert_eq!(env.eval_int("w.visible"), 0);
    }

    #[test]
    fn window_primary_layer_property() {
        let env = TestEnv::new("window-primary");
        env.run("var w = new Window(); var l = new Layer(w, null); var p = w.primaryLayer;")
            .unwrap();
        // primaryLayer now returns the primary Layer's TJS **object**
        // (retained), so `p` evaluates as an object (a valid `isvalid`
        // target) and `l.id` still works for scene addressing.
        assert!(matches!(
            env.eval("p", "test"),
            Ok(tjs2_sys::TjsValue::Object)
        ));
        let scene = env.scene();
        assert_eq!(scene.windows[0].primary_layer, Some(scene.layers[0].id));
        assert!(scene.layers[0].is_primary);
    }

    #[test]
    fn window_close_removes_window_and_layers() {
        let env = TestEnv::new("window-close");
        env.run("var w = new Window(); var l = new Layer(w, null); w.close();")
            .unwrap();
        let scene = env.scene();
        assert_eq!(scene.windows.len(), 0);
        assert_eq!(scene.layers.len(), 0);
    }

    #[test]
    fn window_destroy_removes_layers() {
        let env = TestEnv::new("window-destroy");
        // One-shot create+null: TJS2 releases objects synchronously *within*
        // a script, but a VM register/ref lingers across exec boundaries, so
        // nulling in a *later* script does NOT destroy (the window would stay
        // pinned by the engine's last-object slot). Creating and nulling in a
        // single exec lets the native destroys run; the trailing object result
        // (`%[]`) replaces that slot so the window's final pin is released
        // before `env.run` returns.
        env.run("var w = new Window(); var l = new Layer(w, null); w = null; l = null; %[];")
            .unwrap();
        let scene = env.scene();
        assert_eq!(scene.windows.len(), 0);
        assert_eq!(scene.layers.len(), 0);
    }
}
