//! `Window` — the native implementation class behind the script `Window`
//! wrapper (see `natives/mod.rs` for the architecture).
//!
//! Every instance payload holds the scene window id plus a constructed flag.
//! The class-name method (`Window`) is the constructor hook: `new Window()`
//! runs the script wrapper's constructor, which calls `super.Window()`; the
//! native adds a window to the scene and returns its id. `destroy` removes the
//! window and its layers from the scene.
//!
//! Surface mirrors `reference/cpp/core/visual/WindowIntf.cpp`
//! (`tTJSNC_Window`) and `reference/cpp/core/visual/impl/WindowImpl.cpp`
//! (`TVPCreateNativeClass_Window`). Because the OS window chrome is not
//! modeled, geometry/state members are persisted per native instance and
//! wired to the parts of the host that do exist (the logical scene
//! `inner_size`, and the TJS `action` event dispatch the input bridge drives).
//!
//! | member | behavior |
//! |---|---|
//! | `Window()` | add a window to the scene; return its id |
//! | `close()` / `destroy` | remove the window (and its layers) from the scene |
//! | `setSize` / `setInnerSize` / `width` / `height` / `innerWidth` / `innerHeight` | logical client size (real; fires `onResize`) |
//! | `setPos` / `left` / `top` | logical position (persisted; no OS chrome) |
//! | `setMinSize` / `setMaxSize` / `min*` / `max*` | real min/max size, persisted |
//! | `setZoom` / `zoomNumer` / `zoomDenom` | zoom factor, persisted |
//! | `focusable` / `useMouseKey` / `trapKey` / `imeMode` / `waitVSync` / `enableTouch` / `hintDelay` / touch thresholds | persisted state |
//! | `mouseCursorState` (3-state) / `hideMouseCursor` | persisted cursor state |
//! | `onResize` … `onDisplayRotate` | dispatch `objthis.action(event)` (`TVP_ACTION_INVOKE`) |
//! | `postInputEvent(name, params)` | synthesize an `onKeyDown`/`Up`/`Press` action event |
//! | `showModal` / `setMaskRegion` / `removeMaskRegion` | persisted modal/mask state |
//! | `mainWindow` / `focusedLayer` / `primaryLayer` | retained TJS objects (or `null`) |
//! | `HWND` / `layerTreeOwnerInterface` | opaque native-instance pointer (inert token) |
//! | `drawDevice` | documented inert: reads `null`, writes ignored (no draw-device class) |
//! | `getTouchPoint` / `touchPointCount` | tracked from the native touch events |
//! | `getMouseVelocity` / `getTouchVelocity` | return `0`; the ABI cannot write back out-params |
//! | `findFullScreenCandidates` / `registerMessageReceiver` | argument-checked no-ops |

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, RetainedValue,
    Tjs2Engine, TjsValue, Value,
};

use crate::scene::Scene;

use super::ffi::{
    arg_bool, arg_f64, arg_i64, arg_string, error_out, instance_ref, set_int_out, set_null_out,
    set_real_out, set_string_out, set_void_out,
};
use super::{context_engine, context_scene_mut, context_scene_read};

/// Reference `tTVPMouseCursorState` (`WindowIntf.h:50`), all three states
/// script-visible through `Window.mouseCursorState`.
const MCS_VISIBLE: i32 = 0;
const MCS_TEMP_HIDDEN: i32 = 1;
#[allow(dead_code)]
const MCS_HIDDEN: i32 = 2;

/// Reference `tTVPDisplayOrientation` defaults (`TVPWindow.h:25`).
const ORIENT_UNKNOWN: i64 = 0;

/// One active touch point tracked by the native touch events.
#[derive(Clone, Copy)]
struct TouchPoint {
    id: u32,
    start_x: f64,
    start_y: f64,
    x: f64,
    y: f64,
}

/// Payload of one script-visible `Window` object.
pub(crate) struct WindowInst {
    #[cfg_attr(not(test), allow(dead_code))]
    style: WindowStyleState,
    /// Scene window id, assigned by the constructor.
    pub id: u32,
    /// Whether the native constructor has run.
    pub constructed: bool,
    /// `Window.menu` is backed by a script object stored in a uniquely named
    /// global.  Keeping the reference in the VM (rather than returning a
    /// newly-created object on each property read) gives the property stable
    /// identity without coupling this crate to the optional menu plugin.
    pub menu_ready: bool,
    /// Logical window position (no OS chrome is modeled).
    left: i32,
    top: i32,
    /// Minimum client size (`0` = unset, reference default).
    min_width: i32,
    min_height: i32,
    /// Maximum client size (`0` = unset, reference default).
    max_width: i32,
    max_height: i32,
    /// Reference `iWindowLayer::GetFocusable` default is `true`.
    focusable: bool,
    use_mouse_key: bool,
    trap_key: bool,
    ime_mode: i32,
    mouse_cursor_state: i32,
    wait_vsync: bool,
    enable_touch: bool,
    hint_delay: i32,
    touch_scale_threshold: f64,
    touch_rotate_threshold: f64,
    zoom_numer: i32,
    zoom_denom: i32,
    /// Result passed to the base `onCloseQuery` handler.
    close_query_result: Option<bool>,
    /// `showModal` state (reference `iWindowLayer::ShowWindowAsModal`).
    modal: bool,
    /// `setMaskRegion` threshold, or `None` after `removeMaskRegion`.
    mask_region: Option<i32>,
    full_screen: bool,
    /// Active touch points, updated by `onTouchDown`/`Move`/`Up`.
    touch_points: Vec<TouchPoint>,
}

impl Default for WindowInst {
    fn default() -> Self {
        Self {
            style: WindowStyleState::default(),
            id: 0,
            constructed: false,
            menu_ready: false,
            left: 0,
            top: 0,
            min_width: 0,
            min_height: 0,
            max_width: 0,
            max_height: 0,
            focusable: true,
            use_mouse_key: false,
            trap_key: false,
            ime_mode: 0,
            mouse_cursor_state: MCS_VISIBLE,
            wait_vsync: false,
            enable_touch: false,
            // Reference `iWindowLayer` defaults (`TVPWindow.h:37-41`).
            hint_delay: 500,
            touch_scale_threshold: 5.0,
            touch_rotate_threshold: 5.0,
            zoom_numer: 1,
            zoom_denom: 1,
            close_query_result: None,
            modal: false,
            mask_region: None,
            full_screen: false,
            touch_points: Vec::new(),
        }
    }
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

/// Dereference the `WindowInst` payload.
fn win(instance: *mut c_void) -> &'static mut WindowInst {
    // SAFETY: instance is the Box::into_raw pointer from window_create.
    unsafe { instance_ref::<WindowInst>(instance) }
}

//---------------------------------------------------------------------------
// Generic property helpers
//---------------------------------------------------------------------------

/// Persisted `i32` property backed by a `WindowInst` field.
macro_rules! window_i32_property {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _e: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _err: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_int_out(out, i64::from(win(instance).$field));
            0
        }
        extern "C" fn $set(
            _e: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _err: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: value is a valid value slot for the duration of the call.
            let v = unsafe { &*value };
            win(instance).$field = arg_i64(v) as i32;
            0
        }
    };
}

/// Persisted `bool` property backed by a `WindowInst` field.
macro_rules! window_bool_property {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _e: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _err: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_int_out(out, i64::from(win(instance).$field));
            0
        }
        extern "C" fn $set(
            _e: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _err: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: value is a valid value slot for the duration of the call.
            let v = unsafe { &*value };
            win(instance).$field = arg_bool(v);
            0
        }
    };
}

/// Persisted `f64` property backed by a `WindowInst` field.
macro_rules! window_real_property {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _e: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _err: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_real_out(out, win(instance).$field);
            0
        }
        extern "C" fn $set(
            _e: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _err: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: value is a valid value slot for the duration of the call.
            let v = unsafe { &*value };
            win(instance).$field = arg_f64(v);
            0
        }
    };
}

window_i32_property!(window_min_width_get, window_min_width_set, min_width);
window_i32_property!(window_min_height_get, window_min_height_set, min_height);
window_i32_property!(window_max_width_get, window_max_width_set, max_width);
window_i32_property!(window_max_height_get, window_max_height_set, max_height);
window_i32_property!(window_left_get, window_left_set, left);
window_i32_property!(window_top_get, window_top_set, top);
window_i32_property!(window_ime_mode_get, window_ime_mode_set, ime_mode);
window_i32_property!(
    window_mouse_cursor_state_get,
    window_mouse_cursor_state_set,
    mouse_cursor_state
);
window_i32_property!(window_hint_delay_get, window_hint_delay_set, hint_delay);

window_bool_property!(window_focusable_get, window_focusable_set, focusable);
window_bool_property!(
    window_use_mouse_key_get,
    window_use_mouse_key_set,
    use_mouse_key
);
window_bool_property!(window_trap_key_get, window_trap_key_set, trap_key);
window_bool_property!(window_wait_vsync_get, window_wait_vsync_set, wait_vsync);
window_bool_property!(
    window_enable_touch_get,
    window_enable_touch_set,
    enable_touch
);
window_bool_property!(window_full_screen_get, window_full_screen_set, full_screen);

window_real_property!(
    window_touch_scale_threshold_get,
    window_touch_scale_threshold_set,
    touch_scale_threshold
);
window_real_property!(
    window_touch_rotate_threshold_get,
    window_touch_rotate_threshold_set,
    touch_rotate_threshold
);

//---------------------------------------------------------------------------
// Event dispatch (TVP_ACTION_INVOKE)
//---------------------------------------------------------------------------

/// One member value copied into the event dictionary the action receives.
enum EventArg {
    Int(i64),
    Real(f64),
    /// A retained object id (already retained; consumed by the dispatch).
    Retained(u64),
    Void,
}

/// The TJS helper implementing `TVP_ACTION_INVOKE` (`EventIntf.h:208`): it
/// builds the event dictionary `%[type, target, ...members]` and calls
/// `owner.action(ev)`. Up to six member name/value pairs.
const WINDOW_EVENT_DISPATCH: &str = "(function(owner,t,n1,v1,n2,v2,n3,v3,n4,v4,n5,v5,n6,v6){\
    var ev=%[type:t,target:owner];\n    if(n1!==void)ev[n1]=v1;\n    if(n2!==void)ev[n2]=v2;\n    if(n3!==void)ev[n3]=v3;\n    if(n4!==void)ev[n4]=v4;\n    if(n5!==void)ev[n5]=v5;\n    if(n6!==void)ev[n6]=v6;\n    return owner.action(ev);})";

/// The maximum number of event members any `Window` event carries
/// (`onTouchRotate`).
const WINDOW_EVENT_MAX_MEMBERS: usize = 6;

/// Dispatch one window event to its `action` method: retain the window
/// (`objthis`, which is also the event target), evaluate the helper closure,
/// and invoke it with the event type plus alternating member name/value
/// pairs. This is the reference `TVP_ACTION_INVOKE_END(tTJSVariantClosure(
/// objthis, objthis))` path.
fn dispatch_window_event(
    engine: &Tjs2Engine,
    objthis: *mut c_void,
    event_type: &str,
    members: &[(&str, EventArg)],
) {
    if objthis.is_null() {
        return;
    }
    let Ok(owner) = engine.retain_object_detached(objthis) else {
        return;
    };
    let Ok(helper) = engine.eval_retained(WINDOW_EVENT_DISPATCH, "windowEvent") else {
        return;
    };
    let RetainedValue::Object(helper_dv) = helper else {
        return;
    };
    let mut args: Vec<TjsValue> = vec![
        TjsValue::Retained(owner.raw_id() as u64),
        TjsValue::String(event_type.to_string()),
    ];
    for i in 0..WINDOW_EVENT_MAX_MEMBERS {
        match members.get(i) {
            Some((name, value)) => {
                args.push(TjsValue::String((*name).to_string()));
                args.push(match value {
                    EventArg::Int(v) => TjsValue::Integer(*v),
                    EventArg::Real(v) => TjsValue::Real(*v),
                    EventArg::Retained(id) => TjsValue::Retained(*id),
                    EventArg::Void => TjsValue::Void,
                });
            }
            None => {
                args.push(TjsValue::Void);
                args.push(TjsValue::Void);
            }
        }
    }
    // `owner`/`helper_dv` retentions are consumed by the argument copy; their
    // drops are safe no-ops. Errors surface as a script `action` throw, which
    // the VM reports elsewhere; the native method itself stays void.
    if let Err(e) = engine.call_detached(&helper_dv, &args) {
        log::warn!("window event dispatch ({event_type}) failed: {e}");
    }
}

/// Invoke the window's own named event handler (`objthis.<name>()`). This
/// mirrors the reference posting a TJS event: a script override receives the
/// call, otherwise the native method runs (and dispatches to `action`).
fn fire_window_event(engine: &Tjs2Engine, objthis: *mut c_void, name: &str) {
    if objthis.is_null() {
        return;
    }
    let Ok(dv) = engine.retain_object_detached(objthis) else {
        return;
    };
    if let Err(e) = engine.call_member(dv.raw_id(), name, &[]) {
        log::warn!("window.{name} dispatch failed: {e}");
    }
}

/// Define a native `Window` event method whose integer arguments become the
/// named members of the event dictionary dispatched to `objthis.action`.
macro_rules! window_event_method {
    ($fn_name:ident, $event:literal, [$($member:literal),* $(,)?]) => {
        extern "C" fn $fn_name(
            _engine: *mut c_void,
            _instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: argv/out/out_error are valid for the call.
            let args = unsafe { super::ffi::args(argc, argv) };
            let names: &[&str] = &[$($member),*];
            if args.len() < names.len() {
                return error_out(
                    out_error,
                    concat!("Window.", $event, " requires more arguments"),
                );
            }
            let members: Vec<(&str, EventArg)> = names
                .iter()
                .enumerate()
                .map(|(i, name)| (*name, EventArg::Int(arg_i64(&args[i]))))
                .collect();
            let engine = crate::natives::context_engine();
            dispatch_window_event(engine, objthis, $event, &members);
            set_void_out(out);
            0
        }
    };
}

window_event_method!(window_on_resize, "onResize", []);
window_event_method!(window_on_mouse_enter, "onMouseEnter", []);
window_event_method!(window_on_mouse_leave, "onMouseLeave", []);
window_event_method!(window_on_click, "onClick", ["x", "y"]);
window_event_method!(window_on_double_click, "onDoubleClick", ["x", "y"]);
window_event_method!(
    window_on_mouse_down,
    "onMouseDown",
    ["x", "y", "button", "shift"]
);
window_event_method!(
    window_on_mouse_up,
    "onMouseUp",
    ["x", "y", "button", "shift"]
);
window_event_method!(window_on_mouse_move, "onMouseMove", ["x", "y", "shift"]);
window_event_method!(
    window_on_mouse_wheel,
    "onMouseWheel",
    ["shift", "delta", "x", "y"]
);
window_event_method!(window_on_multi_touch, "onMultiTouch", []);
window_event_method!(window_on_key_down, "onKeyDown", ["key", "shift"]);
window_event_method!(window_on_key_up, "onKeyUp", ["key", "shift"]);
window_event_method!(window_on_key_press, "onKeyPress", ["key"]);
window_event_method!(window_on_popup_hide, "onPopupHide", []);
window_event_method!(window_on_activate, "onActivate", []);
window_event_method!(window_on_deactivate, "onDeactivate", []);
window_event_method!(
    window_on_display_rotate,
    "onDisplayRotate",
    ["orientation", "angle", "bpp", "width", "height"]
);

/// Touch tracking shared by the three touch event methods.
fn track_touch_down(instance: *mut c_void, point: TouchPoint) {
    let inst = win(instance);
    inst.touch_points.retain(|p| p.id != point.id);
    inst.touch_points.push(point);
}

fn track_touch_move(instance: *mut c_void, id: u32, x: f64, y: f64) {
    if let Some(p) = win(instance).touch_points.iter_mut().find(|p| p.id == id) {
        p.x = x;
        p.y = y;
    }
}

fn track_touch_up(instance: *mut c_void, id: u32) {
    win(instance).touch_points.retain(|p| p.id != id);
}

/// `onTouchDown(x, y, cx, cy, id)` → track the new point and dispatch.
extern "C" fn window_on_touch_down(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Window.onTouchDown requires 5 arguments");
    }
    let (x, y) = (arg_f64(&args[0]), arg_f64(&args[1]));
    let (cx, cy) = (arg_f64(&args[2]), arg_f64(&args[3]));
    let id = arg_i64(&args[4]) as u32;
    track_touch_down(
        instance,
        TouchPoint {
            id,
            start_x: x,
            start_y: y,
            x: cx,
            y: cy,
        },
    );
    let members = [
        ("x", EventArg::Real(x)),
        ("y", EventArg::Real(y)),
        ("cx", EventArg::Real(cx)),
        ("cy", EventArg::Real(cy)),
        ("id", EventArg::Int(i64::from(id))),
    ];
    dispatch_window_event(context_engine(), objthis, "onTouchDown", &members);
    set_void_out(out);
    0
}

/// `onTouchUp(x, y, cx, cy, id)` → drop the point and dispatch.
extern "C" fn window_on_touch_up(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Window.onTouchUp requires 5 arguments");
    }
    let (x, y) = (arg_f64(&args[0]), arg_f64(&args[1]));
    let (cx, cy) = (arg_f64(&args[2]), arg_f64(&args[3]));
    let id = arg_i64(&args[4]) as u32;
    track_touch_up(instance, id);
    let members = [
        ("x", EventArg::Real(x)),
        ("y", EventArg::Real(y)),
        ("cx", EventArg::Real(cx)),
        ("cy", EventArg::Real(cy)),
        ("id", EventArg::Int(i64::from(id))),
    ];
    dispatch_window_event(context_engine(), objthis, "onTouchUp", &members);
    set_void_out(out);
    0
}

/// `onTouchMove(x, y, cx, cy, id)` → update the point and dispatch.
extern "C" fn window_on_touch_move(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Window.onTouchMove requires 5 arguments");
    }
    let (x, y) = (arg_f64(&args[0]), arg_f64(&args[1]));
    let (cx, cy) = (arg_f64(&args[2]), arg_f64(&args[3]));
    let id = arg_i64(&args[4]) as u32;
    track_touch_move(instance, id, cx, cy);
    let members = [
        ("x", EventArg::Real(x)),
        ("y", EventArg::Real(y)),
        ("cx", EventArg::Real(cx)),
        ("cy", EventArg::Real(cy)),
        ("id", EventArg::Int(i64::from(id))),
    ];
    dispatch_window_event(context_engine(), objthis, "onTouchMove", &members);
    set_void_out(out);
    0
}

/// `onTouchScaling(startdistance, currentdistance, cx, cy, flag)`.
extern "C" fn window_on_touch_scaling(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 5 {
        return error_out(out_error, "Window.onTouchScaling requires 5 arguments");
    }
    let members = [
        ("startdistance", EventArg::Real(arg_f64(&args[0]))),
        ("currentdistance", EventArg::Real(arg_f64(&args[1]))),
        ("cx", EventArg::Real(arg_f64(&args[2]))),
        ("cy", EventArg::Real(arg_f64(&args[3]))),
        ("flag", EventArg::Int(arg_i64(&args[4]))),
    ];
    dispatch_window_event(context_engine(), objthis, "onTouchScaling", &members);
    set_void_out(out);
    0
}

/// `onTouchRotate(startangle, currentangle, distance, cx, cy, flag)`.
extern "C" fn window_on_touch_rotate(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 6 {
        return error_out(out_error, "Window.onTouchRotate requires 6 arguments");
    }
    let members = [
        ("startangle", EventArg::Real(arg_f64(&args[0]))),
        ("currentangle", EventArg::Real(arg_f64(&args[1]))),
        ("distance", EventArg::Real(arg_f64(&args[2]))),
        ("cx", EventArg::Real(arg_f64(&args[3]))),
        ("cy", EventArg::Real(arg_f64(&args[4]))),
        ("flag", EventArg::Int(arg_i64(&args[5]))),
    ];
    dispatch_window_event(context_engine(), objthis, "onTouchRotate", &members);
    set_void_out(out);
    0
}

/// `onFileDrop(files)` — `files` is passed through as an object when the
/// caller supplied one.
extern "C" fn window_on_file_drop(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(first) = args.first() else {
        return error_out(out_error, "Window.onFileDrop requires an argument");
    };
    let engine = context_engine();
    let files = if first.ty == tjs2_sys::VAL_OBJECT {
        let handle = first.object_handle();
        if handle.is_null() {
            None
        } else {
            engine.retain_object_detached(handle).ok()
        }
    } else {
        None
    };
    let members = match &files {
        Some(dv) => vec![("files", EventArg::Retained(dv.raw_id() as u64))],
        None => vec![("files", EventArg::Void)],
    };
    dispatch_window_event(engine, objthis, "onFileDrop", &members);
    drop(files);
    set_void_out(out);
    0
}

/// `onCloseQuery(canClose)` — the reference base handler (`TVPWindow.cpp:
/// 852`): recording the answer, and hiding a non-modal window when closing is
/// allowed. It does **not** dispatch to `action`.
extern "C" fn window_on_close_query(
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
    let Some(first) = args.first() else {
        return error_out(out_error, "Window.onCloseQuery requires an argument");
    };
    let can_close = arg_bool(first);
    let (id, modal) = {
        let inst = win(instance);
        inst.close_query_result = Some(can_close);
        (inst.id, inst.modal)
    };
    if can_close
        && !modal
        && let Some(w) = context_scene_mut().window_mut(id)
    {
        w.visible = false;
    }
    set_void_out(out);
    0
}

//---------------------------------------------------------------------------
// Constructor / lifecycle
//---------------------------------------------------------------------------

/// `Window()` — the constructor hook. Returns the new window's scene id so
/// the script wrapper can record it as `this.__id`.
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

/// `close()` — remove the window from the scene (its layers go with it).
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

//---------------------------------------------------------------------------
// Geometry / state
//---------------------------------------------------------------------------

/// Apply a new logical client size and fire `onResize` when it changed.
fn set_window_inner_size(instance: *mut c_void, objthis: *mut c_void, w: i64, h: i64) {
    let id = win(instance).id;
    let changed = {
        let mut scene = context_scene_mut();
        match scene.window_mut(id) {
            Some(window) => {
                let new = (w.max(0) as u32, h.max(0) as u32);
                let changed = window.inner_size != new;
                window.inner_size = new;
                changed
            }
            None => false,
        }
    };
    if changed {
        fire_window_event(context_engine(), objthis, "onResize");
    }
}

/// `setSize(w, h)` — set the logical window size (no OS chrome).
extern "C" fn window_set_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Window.setSize requires 2 arguments");
    }
    set_window_inner_size(instance, objthis, arg_i64(&args[0]), arg_i64(&args[1]));
    set_void_out(out);
    0
}

/// `setMinSize(w, h)` — set the minimum client size.
extern "C" fn window_set_min_size(
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
        return error_out(out_error, "Window.setMinSize requires 2 arguments");
    }
    let inst = win(instance);
    inst.min_width = arg_i64(&args[0]) as i32;
    inst.min_height = arg_i64(&args[1]) as i32;
    set_void_out(out);
    0
}

/// `setMaxSize(w, h)` — set the maximum client size.
extern "C" fn window_set_max_size(
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
        return error_out(out_error, "Window.setMaxSize requires 2 arguments");
    }
    let inst = win(instance);
    inst.max_width = arg_i64(&args[0]) as i32;
    inst.max_height = arg_i64(&args[1]) as i32;
    set_void_out(out);
    0
}

/// `setPos(x, y)` — set the logical window position (not `setPosition`).
extern "C" fn window_set_pos(
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
        return error_out(out_error, "Window.setPos requires 2 arguments");
    }
    let inst = win(instance);
    inst.left = arg_i64(&args[0]) as i32;
    inst.top = arg_i64(&args[1]) as i32;
    set_void_out(out);
    0
}

/// `setInnerSize(w, h)` — set the game-logical client size.
extern "C" fn window_set_inner_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.len() < 2 {
        return error_out(out_error, "Window.setInnerSize requires 2 arguments");
    }
    set_window_inner_size(instance, objthis, arg_i64(&args[0]), arg_i64(&args[1]));
    set_void_out(out);
    0
}

/// `setZoom(numer, denom)` — set the zoom factor.
extern "C" fn window_set_zoom(
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
        return error_out(out_error, "Window.setZoom requires 2 arguments");
    }
    let inst = win(instance);
    inst.zoom_numer = arg_i64(&args[0]) as i32;
    inst.zoom_denom = arg_i64(&args[1]) as i32;
    if inst.zoom_denom != 0 {
        inst.style.zoom = f64::from(inst.zoom_numer) / f64::from(inst.zoom_denom);
    }
    set_void_out(out);
    0
}

/// `showModal()` — the reference shows the window as modal and clears pending
/// input; headless we record the modal state.
extern "C" fn window_show_modal(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    win(instance).modal = true;
    set_void_out(out);
    0
}

/// `setMaskRegion(threshold = 1)` — the reference builds a window-region mask
/// from the primary layer's alpha; headless we persist the threshold and
/// require a layer like the reference (`TVPWindowHasNoLayer`).
extern "C" fn window_set_mask_region(
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
    let threshold = match args.first() {
        Some(v) if v.ty != tjs2_sys::VAL_VOID => arg_i64(v) as i32,
        _ => 1,
    };
    let id = win(instance).id;
    if context_scene_read()
        .window(id)
        .and_then(|w| w.primary_layer)
        .is_none()
    {
        return error_out(out_error, "Window.setMaskRegion: window has no layer");
    }
    win(instance).mask_region = Some(threshold);
    set_void_out(out);
    0
}

/// `removeMaskRegion()` — clear the persisted mask region.
extern "C" fn window_remove_mask_region(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    win(instance).mask_region = None;
    set_void_out(out);
    0
}

/// `hideMouseCursor()` — set the cursor to the reference `mcsTempHidden`
/// state (the base platform was a no-op; the 3-state property is what the
/// game inspects).
extern "C" fn window_hide_mouse_cursor(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    win(instance).mouse_cursor_state = MCS_TEMP_HIDDEN;
    set_void_out(out);
    0
}

/// `bringToFront()` — no-op (single-window milestone).
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

/// `update()` — no-op (the render loop syncs every frame).
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

/// `postInputEvent(name, params)` — synthesize a keyboard-input event. Only
/// the reference keyboard names are accepted; `params` must carry `key` (and
/// `shift` for down/up).
extern "C" fn window_post_input_event(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/out/out_error are valid for the call.
    let args = unsafe { super::ffi::args(argc, argv) };
    let Some(name_arg) = args.first() else {
        return error_out(out_error, "Window.postInputEvent requires an event name");
    };
    let name = arg_string(name_arg);
    let engine = context_engine();
    let params = args.get(1).and_then(|v| {
        let handle = v.object_handle();
        if handle.is_null() {
            None
        } else {
            engine.retain_object_detached(handle).ok()
        }
    });
    let read = |member: &str| -> Option<i64> {
        params
            .as_ref()
            .and_then(|dv| match engine.get_member(dv.raw_id(), member) {
                Ok(TjsValue::Integer(v)) => Some(v),
                Ok(TjsValue::Real(v)) => Some(v as i64),
                Ok(TjsValue::Void) => Some(0),
                _ => None,
            })
    };
    match name.as_str() {
        "onKeyDown" | "onKeyUp" => {
            let Some(key) = read("key") else {
                return error_out(out_error, "Window.postInputEvent: key parameter required");
            };
            let Some(shift) = read("shift") else {
                return error_out(out_error, "Window.postInputEvent: shift parameter required");
            };
            let members = [("key", EventArg::Int(key)), ("shift", EventArg::Int(shift))];
            dispatch_window_event(engine, objthis, &name, &members);
        }
        "onKeyPress" => {
            let Some(key) = read("key") else {
                return error_out(out_error, "Window.postInputEvent: key parameter required");
            };
            let members = [("key", EventArg::Int(key))];
            dispatch_window_event(engine, objthis, "onKeyPress", &members);
        }
        _ => {
            return error_out(
                out_error,
                &format!("Window.postInputEvent: unknown event name {name}"),
            );
        }
    }
    set_void_out(out);
    0
}

/// `findFullScreenCandidates(w, h, bpp, mode, zoomMode)` — the reference body
/// is disabled (`#if 0`); argument-checked no-op.
extern "C" fn window_find_full_screen_candidates(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 5 {
        return error_out(
            out_error,
            "Window.findFullScreenCandidates requires 5 arguments",
        );
    }
    set_void_out(out);
    0
}

/// `registerMessageReceiver(mode, proc, userdata)` — native callbacks cannot
/// cross this ABI; argument-checked no-op.
extern "C" fn window_register_message_receiver(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return error_out(
            out_error,
            "Window.registerMessageReceiver requires 3 arguments",
        );
    }
    set_void_out(out);
    0
}

/// `getTouchPoint(index)` — a dictionary `%[startX, startY, x, y, ID]` for
/// the tracked touch point, or a TJS error when the index is out of range.
extern "C" fn window_get_touch_point(
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
    let Some(index_arg) = args.first() else {
        return error_out(out_error, "Window.getTouchPoint requires an index");
    };
    let index = arg_i64(index_arg);
    let Some(point) = ({
        let points = &win(instance).touch_points;
        if index < 0 {
            None
        } else {
            points.get(index as usize).copied()
        }
    }) else {
        return error_out(out_error, "Window.getTouchPoint: index out of range");
    };
    let engine = context_engine();
    let expr = format!(
        "(%[startX:{},startY:{},x:{},y:{},ID:{}])",
        point.start_x, point.start_y, point.x, point.y, point.id
    );
    match engine.eval_retained(&expr, "Window.getTouchPoint") {
        Ok(RetainedValue::Object(dv)) => {
            set_detached_out(dv, out);
            0
        }
        Ok(_) => error_out(out_error, "Window.getTouchPoint: not an object"),
        Err(e) => error_out(out_error, &format!("Window.getTouchPoint: {e}")),
    }
}

/// `getTouchVelocity(id, x, y, speed)` — the reference writes the velocity
/// into its by-reference out parameters. This ABI hands the native a *copy*
/// of the arguments, so the out values cannot be propagated; there is no
/// velocity tracker in the headless port, so the truthful result is `0`.
extern "C" fn window_get_touch_velocity(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 4 {
        return error_out(out_error, "Window.getTouchVelocity requires 4 arguments");
    }
    set_int_out(out, 0);
    0
}

/// `getMouseVelocity(x, y, speed)` — see [`window_get_touch_velocity`].
extern "C" fn window_get_mouse_velocity(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return error_out(out_error, "Window.getMouseVelocity requires 3 arguments");
    }
    set_int_out(out, 0);
    0
}

/// `resetMouseVelocity()` — no-op (no tracker).
extern "C" fn window_reset_mouse_velocity(
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

//---------------------------------------------------------------------------
// Integer/scene-backed properties
//---------------------------------------------------------------------------

/// `width` / `innerWidth` getter: the logical client width. No OS chrome is
/// modeled, so both reference notions coincide.
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

/// `height` / `innerHeight` getter.
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

/// `width` / `innerWidth` setter.
extern "C" fn window_width_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is a valid value slot for the duration of the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    let height = context_scene_read()
        .window(inst.id)
        .map(|w| w.inner_size.1)
        .unwrap_or(0);
    set_window_inner_size(instance, objthis, arg_i64(v), i64::from(height));
    0
}

/// `height` / `innerHeight` setter.
extern "C" fn window_height_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is a valid value slot for the duration of the call.
    let v = unsafe { &*value };
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    let width = context_scene_read()
        .window(inst.id)
        .map(|w| w.inner_size.0)
        .unwrap_or(0);
    set_window_inner_size(instance, objthis, i64::from(width), arg_i64(v));
    0
}

/// `zoomNumer` getter.
extern "C" fn window_zoom_numer_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, i64::from(win(instance).zoom_numer));
    0
}

/// `zoomNumer` setter (keeps the extra `zoom` real in sync).
extern "C" fn window_zoom_numer_set(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is a valid value slot for the duration of the call.
    let v = unsafe { &*value };
    let inst = win(instance);
    inst.zoom_numer = arg_i64(v) as i32;
    if inst.zoom_denom != 0 {
        inst.style.zoom = f64::from(inst.zoom_numer) / f64::from(inst.zoom_denom);
    }
    0
}

/// `zoomDenom` getter.
extern "C" fn window_zoom_denom_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, i64::from(win(instance).zoom_denom));
    0
}

/// `zoomDenom` setter.
extern "C" fn window_zoom_denom_set(
    _e: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value is a valid value slot for the duration of the call.
    let v = unsafe { &*value };
    let inst = win(instance);
    inst.zoom_denom = arg_i64(v) as i32;
    if inst.zoom_denom != 0 {
        inst.style.zoom = f64::from(inst.zoom_numer) / f64::from(inst.zoom_denom);
    }
    0
}

//---------------------------------------------------------------------------
// Special properties
//---------------------------------------------------------------------------

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
    set_string_out(out, &win.title);
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
    win.title = arg_string(v);
    0
}

/// `touchPointCount` — number of tracked touch points.
extern "C" fn window_touch_point_count_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, win(instance).touch_points.len() as i64);
    0
}

/// `displayOrientation` — read-only; the headless host reports
/// `orientUnknown`.
extern "C" fn window_display_orientation_get(
    _e: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, ORIENT_UNKNOWN);
    0
}

/// `displayRotate` — read-only; the headless host reports `-1`.
extern "C" fn window_display_rotate_get(
    _e: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, -1);
    0
}

/// `HWND` — the reference returns the native-instance pointer as an integer
/// (`WindowImpl.cpp:2288`). The port has no OS handle; return the
/// `WindowInst` pointer as an opaque, non-zero token for plugins that only
/// round-trip it.
extern "C" fn window_hwnd_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, instance as usize as i64);
    0
}

/// `layerTreeOwnerInterface` — the reference returns `this` cast to an
/// integer (`WindowIntf.cpp:1826`); the `WindowInst` pointer is the analogue.
extern "C" fn window_layer_tree_owner_interface_get(
    _e: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int_out(out, instance as usize as i64);
    0
}

/// `drawDevice` getter — no draw-device class exists in the headless port, so
/// the reference's default `BasicDrawDevice` cannot be constructed; reads
/// return TJS `null` (documented inert).
extern "C" fn window_draw_device_get(
    _e: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_null_out(context_engine(), out);
    0
}

/// `drawDevice` setter — accepted and ignored (no draw-device class).
extern "C" fn window_draw_device_set(
    _e: *mut c_void,
    _instance: *mut c_void,
    _value: *const Value,
    _err: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    0
}

/// Retain a TJS object into a `VAL_RETAINED` result slot. Returns `false`
/// when there is no live object to return.
fn set_object_out(engine: &Tjs2Engine, obj: *mut c_void, out: *mut Value) -> bool {
    if obj.is_null() {
        return false;
    }
    // SAFETY: obj is a live TJS object and engine.raw() is the registered VM.
    let rid = unsafe { tjs2_sys::tjs2_retain_object(engine.raw(), obj) };
    if rid.is_null() {
        return false;
    }
    // SAFETY: out is a valid result slot for the duration of the call.
    unsafe {
        (*out).ty = tjs2_sys::VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = rid as usize;
    }
    true
}

/// Hand a retained value (e.g. a dictionary from `eval_retained`) to the C++
/// side as the call result. The retention is consumed there.
fn set_detached_out(dv: tjs2_sys::DetachedValue, out: *mut Value) {
    let id = dv.raw_id() as usize;
    // SAFETY: out is a valid result slot for the duration of the call.
    unsafe {
        (*out).ty = tjs2_sys::VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = id;
    }
    std::mem::forget(dv);
}

/// `primaryLayer` — the window's primary layer as a retained TJS object, or
/// `null`.
extern "C" fn window_primary_layer_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    let engine = context_engine();
    // Return the primary Layer's TJS object (retained), so scripts can do
    // `with(primaryLayer){ .setSize(...) ... }` like the real engine. When
    // there is no primary layer (or its object is gone) return `null`, never
    // an integer: scripts treat `primaryLayer` as an object.
    let obj = context_scene_read()
        .window(inst.id)
        .and_then(|w| w.primary_layer)
        .map(super::layer_tjs_object)
        .filter(|obj| !obj.is_null());
    if let Some(obj) = obj
        && set_object_out(engine, obj, out)
    {
        return 0;
    }
    set_null_out(engine, out);
    0
}

/// `focusedLayer` — the scene's focused layer for this window (reference
/// `DrawDevice->GetFocusedLayer`, shared with `Layer.focus()`), or `null`.
extern "C" fn window_focused_layer_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    let engine = context_engine();
    let focused = context_scene_read()
        .window(inst.id)
        .and_then(|w| w.focused_layer);
    let obj = focused
        .map(super::layer_tjs_object)
        .filter(|obj| !obj.is_null());
    if let Some(obj) = obj
        && set_object_out(engine, obj, out)
    {
        return 0;
    }
    set_null_out(engine, out);
    0
}

/// `focusedLayer` setter — accepts a Layer object or integer id; `null`/void
/// clears the focus. Delegates to [`Scene::set_focus`] / [`Scene::clear_focus`]
/// so the render host observes the same focus holder.
extern "C" fn window_focused_layer_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    // SAFETY: value is a valid value slot for the duration of the call.
    let v = unsafe { &*value };
    let engine = context_engine();
    match resolve_object_id_arg(engine, v) {
        Ok(id) if id >= 0 => {
            // Only honor a layer that belongs to this window (the reference
            // focused layer is per draw device/window).
            let owns = context_scene_read()
                .layer(id as u32)
                .is_some_and(|l| l.window == inst.id);
            if owns {
                context_scene_mut().set_focus(id as u32);
            }
        }
        Ok(_) => {
            context_scene_mut().clear_focus(inst.id);
        }
        Err(e) => {
            return error_out(out_error, &format!("Window.focusedLayer: {e}"));
        }
    }
    0
}

/// `mainWindow` — the first window registered in the scene (`TVPMainWindow`),
/// as a retained TJS object, or `null`.
extern "C" fn window_main_window_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let engine = context_engine();
    let first = context_scene_read().windows.first().map(|w| w.id);
    let obj = first
        .map(super::window_tjs_object)
        .filter(|obj| !obj.is_null());
    if let Some(obj) = obj
        && set_object_out(engine, obj, out)
    {
        return 0;
    }
    set_null_out(engine, out);
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

/// Read a `nativeId`/`id` member from a retained object.
fn read_object_id(engine: &Tjs2Engine, id: tjs2_sys::Tjs2ValueId) -> Result<i64, String> {
    let mut last_error = None;
    for member in ["nativeId", "id"] {
        match engine.get_member(id, member) {
            Ok(TjsValue::Integer(v)) => return Ok(v),
            Ok(TjsValue::Real(v)) => return Ok(v as i64),
            Ok(_) => {}
            Err(e) => last_error = Some(e),
        }
    }
    match last_error {
        Some(e) => Err(e),
        None => Ok(-1),
    }
}

/// Resolve an object argument (a `Layer`/`Window`) or an integer to its scene
/// id; non-object values read as `-1`.
fn resolve_object_id_arg(engine: &Tjs2Engine, v: &Value) -> Result<i64, String> {
    match v.ty {
        tjs2_sys::VAL_INTEGER | tjs2_sys::VAL_REAL => Ok(arg_i64(v)),
        tjs2_sys::VAL_OBJECT => {
            let handle = v.object_handle();
            if handle.is_null() {
                return Ok(-1);
            }
            let dv = engine.retain_object_detached(handle)?;
            read_object_id(engine, dv.raw_id())
        }
        _ => Ok(-1),
    }
}

//---------------------------------------------------------------------------
// `Window.menu`
//---------------------------------------------------------------------------

/// True when the game registered the native `MenuItem` class from
/// `tvp-natives` (the class installs `global.__krkr_native_MenuItem`).
fn native_menu_item_available(engine: &Tjs2Engine) -> bool {
    matches!(
        engine.eval("global.__krkr_native_MenuItem", "Window.menu"),
        Ok(TjsValue::Integer(1))
    )
}

/// True when the window's root-menu global already holds a value.
fn window_menu_global_exists(engine: &Tjs2Engine, name: &str) -> bool {
    matches!(
        engine.eval(
            &format!("typeof global.{name} != 'undefined'"),
            "Window.menu"
        ),
        Ok(TjsValue::Integer(1))
    )
}

/// Run the `tvp-natives` helper that constructs the window's root MenuItem as
/// `new MenuItem(window, window)` (reference `TVPCreateMenuItemObject`,
/// `MenuItemIntf.cpp:560`) and stores it under `name`.
fn create_native_window_menu(
    engine: &Tjs2Engine,
    window: *mut c_void,
    name: &str,
) -> Result<(), String> {
    let window = engine
        .retain_object_detached(window)
        .map_err(|e| format!("cannot retain the window: {e}"))?;
    let helper = match engine.eval_retained("global.__krkr_make_window_menu", "Window.menu") {
        Ok(RetainedValue::Object(value)) => value,
        Ok(_) => return Err("native MenuItem window helper is not a function".into()),
        Err(e) => return Err(format!("native MenuItem window helper unavailable: {e}")),
    };
    engine
        .call_detached(&helper, &[TjsValue::Retained(window.raw_id() as u64)])
        .map_err(|e| format!("cannot create the root MenuItem: {e}"))?;
    if !window_menu_global_exists(engine, name) {
        return Err("root MenuItem was not stored".into());
    }
    Ok(())
}

/// Place a retained object result in `out` from a named global.
fn return_global_menu(
    engine: &Tjs2Engine,
    out: *mut Value,
    out_error: *mut *mut c_char,
    name: &str,
) -> c_int {
    match engine.eval_retained(&format!("global.{name}"), "Window.menu") {
        Ok(RetainedValue::Object(value)) => {
            // C++ consumes a retained return value.
            let id = value.raw_id();
            unsafe {
                (*out).ty = tjs2_sys::VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = std::ptr::null();
                (*out).array = std::ptr::null();
                (*out).array_count = 0;
                (*out).retained = id as usize;
            }
            std::mem::forget(value);
            0
        }
        Ok(_) => error_out(out_error, "Window.menu: root is not an object"),
        Err(e) => error_out(out_error, &format!("Window.menu: {e}")),
    }
}

/// Return the stable root `MenuItem` for this window (reference
/// `WindowMenuProperty::PropGet`, `MenuItemImpl.cpp:65`).
///
/// The normal application registers the native `MenuItem` class from
/// `tvp-natives`; when present the root is a real native item (so the render
/// layer can draw and activate it). Small visual-only/headless users
/// (including this crate's tests) may not register that optional class, so a
/// deliberately small TJS fallback with the same stateful tree surface is
/// installed instead.
extern "C" fn window_menu_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    if !inst.constructed {
        return error_out(out_error, "Window.menu: window is not constructed");
    }
    let engine = context_engine();
    let name = format!("__krkr_window_menu_{}", inst.id);
    if !inst.menu_ready {
        if native_menu_item_available(engine) {
            if !window_menu_global_exists(engine, &name)
                && let Err(e) = create_native_window_menu(engine, objthis, &name)
            {
                return error_out(out_error, &format!("Window.menu: {e}"));
            }
        } else {
            let init = format!(
                "if (typeof global.MenuItem == 'undefined') {{ \
                     global.MenuItem = function(owner, caption) {{ \
                       this.caption = (caption === undefined ? '' : caption); \
                       this.checked = false; this.enabled = true; this.radio = false; \
                       this.group = 0; this.visible = true; this.shortcut = ''; \
                       this.children = []; this.parent = null; this.root = this; \
                       this.add = function(item) {{ this.children[this.children.count] = item; item.parent = this; item.root = this.root; }}; \
                       this.insert = function(item, index) {{ this.children.splice(index, 0, item); item.parent = this; item.root = this.root; }}; \
                       this.remove = function(item) {{ var i = this.children.indexOf(item); if (i >= 0) {{ this.children.splice(i, 1); item.parent = null; item.root = item; }} }}; \
                       this.fireClick = function() {{ if (this.enabled && this.onClick) this.onClick(); }}; \
                       this.popup = function() {{ return 1; }}; \
                     }}; \
                   }}; \
                   if (typeof global.{0} == 'undefined') global.{0} = %[caption:'', checked:0, enabled:1, radio:0, group:0, visible:1, shortcut:'', children:[], parent:void, root:void]; global.{0}.root = global.{0}; global.{0}.add = function(item) {{ this.children[this.children.count] = item; item.parent = this; item.root = this.root; }}; global.{0}.insert = function(item, index) {{ this.children[index] = item; item.parent = this; item.root = this.root; }}; global.{0}.remove = function(item) {{ var i = 0; while (i < this.children.count && this.children[i] !== item) i++; if (i < this.children.count) this.children[i] = void; item.parent = void; item.root = item; }}; global.{0}.fireClick = function() {{ if (this.enabled && this.onClick !== void) this.onClick(); }}; global.{0}.popup = function() {{ return 1; }};",
                name
            );
            if let Err(e) = engine.exec_script(&init, "Window.menu") {
                return error_out(out_error, &format!("Window.menu: {e}"));
            }
        }
        inst.menu_ready = true;
    }
    return_global_menu(engine, out, out_error, &name)
}

/// Replace this window's root `MenuItem` (reference `WindowMenuProperty`
/// denies the setter, `MenuItemImpl.cpp:87`; the in-engine host honors it so
/// scripts can swap the whole menu bar).
extern "C" fn window_menu_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { instance_ref::<WindowInst>(instance) };
    if !inst.constructed {
        return error_out(out_error, "Window.menu: window is not constructed");
    }
    let value = unsafe { &*value };
    if value.ty != tjs2_sys::VAL_OBJECT || value.object_handle().is_null() {
        return error_out(out_error, "Window.menu: a MenuItem object is required");
    }
    let engine = context_engine();
    if !native_menu_item_available(engine) {
        return error_out(
            out_error,
            "Window.menu: assigning a root requires the native MenuItem class",
        );
    }
    let menu = match engine.retain_object_detached(value.object_handle()) {
        Ok(m) => m,
        Err(e) => return error_out(out_error, &format!("Window.menu: {e}")),
    };
    let window = match engine.retain_object_detached(objthis) {
        Ok(w) => w,
        Err(e) => return error_out(out_error, &format!("Window.menu: {e}")),
    };
    let helper = match engine.eval_retained("global.__krkr_set_window_menu", "Window.menu") {
        Ok(RetainedValue::Object(value)) => value,
        Ok(_) => return error_out(out_error, "Window.menu: setter helper is not a function"),
        Err(e) => return error_out(out_error, &format!("Window.menu: {e}")),
    };
    match engine.call_detached(
        &helper,
        &[
            TjsValue::Retained(window.raw_id() as u64),
            TjsValue::Retained(menu.raw_id() as u64),
        ],
    ) {
        Ok(_) => {
            inst.menu_ready = true;
            0
        }
        Err(e) => error_out(out_error, &format!("Window.menu: {e}")),
    }
}

//---------------------------------------------------------------------------
// Legacy no-op helpers (kept for the extras the game probes)
//---------------------------------------------------------------------------

/// `add(layer)` / `remove(layer)` — no-ops: the real TVP methods take Layer
/// **objects**, which the FFI could not resolve in this milestone. Layers are
/// attached to their window at construction and removed with `close`, so the
/// scene stays consistent without them.
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

/// Argument-tolerant no-op for the engine-internal extras
/// (`addInputNotify`, `changeScreenMode`, `registerExEvent`).
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

//---------------------------------------------------------------------------
// Window style property no-ops (stored per-instance so reads return what
// was written, matching the reference's persistent members).
//---------------------------------------------------------------------------

#[derive(Default, Clone)]
struct WindowStyleState {
    border_style: i64,
    inner_sunken: bool,
    show_scroll_bars: bool,
    zoom: f64,
    stay_on_top: bool,
}

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
    set_real_out(out, style(instance).zoom);
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

//---------------------------------------------------------------------------
// Host hooks
//---------------------------------------------------------------------------

/// Fire the window's `onResize` handler. Host hook for a real OS/Bevy
/// `WindowResized` event: the render bridge can call this with the scene
/// window id (or simply `call_member(window_tjs_object(id), "onResize", &[])`).
pub fn notify_window_resize(engine: &Tjs2Engine, window_id: u32) {
    let obj = super::window_tjs_object(window_id);
    if obj.is_null() {
        return;
    }
    fire_window_event(engine, obj, "onResize");
}

/// Fire `onActivate`/`onDeactivate`. Host hook for a real OS/Bevy window
/// focus event.
pub fn notify_window_activate(engine: &Tjs2Engine, window_id: u32, active: bool) {
    let obj = super::window_tjs_object(window_id);
    if obj.is_null() {
        return;
    }
    fire_window_event(
        engine,
        obj,
        if active { "onActivate" } else { "onDeactivate" },
    );
}

//---------------------------------------------------------------------------
// Registration
//---------------------------------------------------------------------------

/// Register the `Window` native class.
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
                name: "close",
                f: window_close,
            },
            NativeInstanceMethodDef {
                name: "bringToFront",
                f: window_bring_to_front,
            },
            NativeInstanceMethodDef {
                name: "update",
                f: window_update,
            },
            NativeInstanceMethodDef {
                name: "showModal",
                f: window_show_modal,
            },
            NativeInstanceMethodDef {
                name: "setMaskRegion",
                f: window_set_mask_region,
            },
            NativeInstanceMethodDef {
                name: "removeMaskRegion",
                f: window_remove_mask_region,
            },
            NativeInstanceMethodDef {
                name: "add",
                f: window_add_remove_noop,
            },
            NativeInstanceMethodDef {
                name: "remove",
                f: window_add_remove_noop,
            },
            NativeInstanceMethodDef {
                name: "setSize",
                f: window_set_size,
            },
            NativeInstanceMethodDef {
                name: "setMinSize",
                f: window_set_min_size,
            },
            NativeInstanceMethodDef {
                name: "setMaxSize",
                f: window_set_max_size,
            },
            NativeInstanceMethodDef {
                name: "setPos",
                f: window_set_pos,
            },
            NativeInstanceMethodDef {
                name: "setInnerSize",
                f: window_set_inner_size,
            },
            NativeInstanceMethodDef {
                name: "setZoom",
                f: window_set_zoom,
            },
            NativeInstanceMethodDef {
                name: "hideMouseCursor",
                f: window_hide_mouse_cursor,
            },
            NativeInstanceMethodDef {
                name: "postInputEvent",
                f: window_post_input_event,
            },
            NativeInstanceMethodDef {
                name: "findFullScreenCandidates",
                f: window_find_full_screen_candidates,
            },
            NativeInstanceMethodDef {
                name: "registerMessageReceiver",
                f: window_register_message_receiver,
            },
            NativeInstanceMethodDef {
                name: "getTouchPoint",
                f: window_get_touch_point,
            },
            NativeInstanceMethodDef {
                name: "getTouchVelocity",
                f: window_get_touch_velocity,
            },
            NativeInstanceMethodDef {
                name: "getMouseVelocity",
                f: window_get_mouse_velocity,
            },
            NativeInstanceMethodDef {
                name: "resetMouseVelocity",
                f: window_reset_mouse_velocity,
            },
            // Event methods (TVP_ACTION_INVOKE).
            NativeInstanceMethodDef {
                name: "onResize",
                f: window_on_resize,
            },
            NativeInstanceMethodDef {
                name: "onMouseEnter",
                f: window_on_mouse_enter,
            },
            NativeInstanceMethodDef {
                name: "onMouseLeave",
                f: window_on_mouse_leave,
            },
            NativeInstanceMethodDef {
                name: "onClick",
                f: window_on_click,
            },
            NativeInstanceMethodDef {
                name: "onDoubleClick",
                f: window_on_double_click,
            },
            NativeInstanceMethodDef {
                name: "onMouseDown",
                f: window_on_mouse_down,
            },
            NativeInstanceMethodDef {
                name: "onMouseUp",
                f: window_on_mouse_up,
            },
            NativeInstanceMethodDef {
                name: "onMouseMove",
                f: window_on_mouse_move,
            },
            NativeInstanceMethodDef {
                name: "onMouseWheel",
                f: window_on_mouse_wheel,
            },
            NativeInstanceMethodDef {
                name: "onTouchDown",
                f: window_on_touch_down,
            },
            NativeInstanceMethodDef {
                name: "onTouchUp",
                f: window_on_touch_up,
            },
            NativeInstanceMethodDef {
                name: "onTouchMove",
                f: window_on_touch_move,
            },
            NativeInstanceMethodDef {
                name: "onTouchScaling",
                f: window_on_touch_scaling,
            },
            NativeInstanceMethodDef {
                name: "onTouchRotate",
                f: window_on_touch_rotate,
            },
            NativeInstanceMethodDef {
                name: "onMultiTouch",
                f: window_on_multi_touch,
            },
            NativeInstanceMethodDef {
                name: "onKeyDown",
                f: window_on_key_down,
            },
            NativeInstanceMethodDef {
                name: "onKeyUp",
                f: window_on_key_up,
            },
            NativeInstanceMethodDef {
                name: "onKeyPress",
                f: window_on_key_press,
            },
            NativeInstanceMethodDef {
                name: "onFileDrop",
                f: window_on_file_drop,
            },
            NativeInstanceMethodDef {
                name: "onCloseQuery",
                f: window_on_close_query,
            },
            NativeInstanceMethodDef {
                name: "onPopupHide",
                f: window_on_popup_hide,
            },
            NativeInstanceMethodDef {
                name: "onActivate",
                f: window_on_activate,
            },
            NativeInstanceMethodDef {
                name: "onDeactivate",
                f: window_on_deactivate,
            },
            NativeInstanceMethodDef {
                name: "onDisplayRotate",
                f: window_on_display_rotate,
            },
            // Engine-internal extras the game probes.
            NativeInstanceMethodDef {
                name: "addInputNotify",
                f: window_noop2,
            },
            NativeInstanceMethodDef {
                name: "changeScreenMode",
                f: window_noop2,
            },
            NativeInstanceMethodDef {
                name: "registerExEvent",
                f: window_noop2,
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
                set: Some(window_width_set),
            },
            NativeInstancePropertyDef {
                name: "height",
                get: Some(window_height_get),
                set: Some(window_height_set),
            },
            NativeInstancePropertyDef {
                name: "minWidth",
                get: Some(window_min_width_get),
                set: Some(window_min_width_set),
            },
            NativeInstancePropertyDef {
                name: "minHeight",
                get: Some(window_min_height_get),
                set: Some(window_min_height_set),
            },
            NativeInstancePropertyDef {
                name: "maxWidth",
                get: Some(window_max_width_get),
                set: Some(window_max_width_set),
            },
            NativeInstancePropertyDef {
                name: "maxHeight",
                get: Some(window_max_height_get),
                set: Some(window_max_height_set),
            },
            NativeInstancePropertyDef {
                name: "left",
                get: Some(window_left_get),
                set: Some(window_left_set),
            },
            NativeInstancePropertyDef {
                name: "top",
                get: Some(window_top_get),
                set: Some(window_top_set),
            },
            NativeInstancePropertyDef {
                name: "focusable",
                get: Some(window_focusable_get),
                set: Some(window_focusable_set),
            },
            NativeInstancePropertyDef {
                name: "innerWidth",
                get: Some(window_width_get),
                set: Some(window_width_set),
            },
            NativeInstancePropertyDef {
                name: "innerHeight",
                get: Some(window_height_get),
                set: Some(window_height_set),
            },
            NativeInstancePropertyDef {
                name: "zoomNumer",
                get: Some(window_zoom_numer_get),
                set: Some(window_zoom_numer_set),
            },
            NativeInstancePropertyDef {
                name: "zoomDenom",
                get: Some(window_zoom_denom_get),
                set: Some(window_zoom_denom_set),
            },
            NativeInstancePropertyDef {
                name: "borderStyle",
                get: Some(window_int_prop_get_bs),
                set: Some(window_int_prop_set_bs),
            },
            NativeInstancePropertyDef {
                name: "stayOnTop",
                get: Some(window_bool_prop_get_sot),
                set: Some(window_bool_prop_set_sot),
            },
            NativeInstancePropertyDef {
                name: "useMouseKey",
                get: Some(window_use_mouse_key_get),
                set: Some(window_use_mouse_key_set),
            },
            NativeInstancePropertyDef {
                name: "trapKey",
                get: Some(window_trap_key_get),
                set: Some(window_trap_key_set),
            },
            NativeInstancePropertyDef {
                name: "imeMode",
                get: Some(window_ime_mode_get),
                set: Some(window_ime_mode_set),
            },
            NativeInstancePropertyDef {
                name: "mouseCursorState",
                get: Some(window_mouse_cursor_state_get),
                set: Some(window_mouse_cursor_state_set),
            },
            NativeInstancePropertyDef {
                name: "fullScreen",
                get: Some(window_full_screen_get),
                set: Some(window_full_screen_set),
            },
            NativeInstancePropertyDef {
                name: "mainWindow",
                get: Some(window_main_window_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "focusedLayer",
                get: Some(window_focused_layer_get),
                set: Some(window_focused_layer_set),
            },
            // The primary Layer's TJS object (retained), or `null` when the
            // window has no primary layer.
            NativeInstancePropertyDef {
                name: "primaryLayer",
                get: Some(window_primary_layer_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "waitVSync",
                get: Some(window_wait_vsync_get),
                set: Some(window_wait_vsync_set),
            },
            NativeInstancePropertyDef {
                name: "layerTreeOwnerInterface",
                get: Some(window_layer_tree_owner_interface_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "HWND",
                get: Some(window_hwnd_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "drawDevice",
                get: Some(window_draw_device_get),
                set: Some(window_draw_device_set),
            },
            NativeInstancePropertyDef {
                name: "touchScaleThreshold",
                get: Some(window_touch_scale_threshold_get),
                set: Some(window_touch_scale_threshold_set),
            },
            NativeInstancePropertyDef {
                name: "touchRotateThreshold",
                get: Some(window_touch_rotate_threshold_get),
                set: Some(window_touch_rotate_threshold_set),
            },
            NativeInstancePropertyDef {
                name: "touchPointCount",
                get: Some(window_touch_point_count_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "hintDelay",
                get: Some(window_hint_delay_get),
                set: Some(window_hint_delay_set),
            },
            NativeInstancePropertyDef {
                name: "enableTouch",
                get: Some(window_enable_touch_get),
                set: Some(window_enable_touch_set),
            },
            NativeInstancePropertyDef {
                name: "displayOrientation",
                get: Some(window_display_orientation_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "displayRotate",
                get: Some(window_display_rotate_get),
                set: None,
            },
            // Kept extras (not reference members).
            NativeInstancePropertyDef {
                name: "menu",
                get: Some(window_menu_get),
                set: Some(window_menu_set),
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
        ],
    })
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
    fn window_menu_is_stable_and_stateful() {
        let env = TestEnv::new("window-menu");
        env.run("var w = new Window(); var a = w.menu; var b = w.menu; a.caption = 'Root'; var c = %[caption:'Child']; a.add(c);").unwrap();
        assert_eq!(
            env.eval("a === b", "menu").unwrap(),
            tjs2_sys::TjsValue::Integer(1)
        );
        assert_eq!(env.eval_string("w.menu.caption"), "Root");
        assert_eq!(env.eval_int("w.menu.children.length"), 1);
        assert_eq!(env.eval_string("w.menu.children[0].caption"), "Child");
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
