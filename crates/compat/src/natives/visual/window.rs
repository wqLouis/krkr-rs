//! The visual `Window` native class.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use engine::input::InputState;
use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, RetainedValue,
    Tjs2Engine, TjsValue, Value,
};

use super::common::{
    ObjectKind, VisualState, bound, duplicate_object, input_ref, reparent_layer, resolve_object_id,
    scene_mut, scene_ref, set_null_out, set_object_out, set_real, set_retained_out,
};
use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{set_string_out, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError};

/// `Window` payload: the scene id plus the window properties the L2
/// [`engine::WindowState`] does not model (there is no OS chrome).
struct WindowInst {
    id: Option<u32>,
    left: i32,
    top: i32,
    min_width: i32,
    min_height: i32,
    max_width: i32,
    max_height: i32,
    focusable: bool,
    zoom_numer: i32,
    zoom_denom: i32,
    full_screen: bool,
    wait_vsync: bool,
    enable_touch: bool,
    hint_delay: i32,
    mouse_cursor_state: i32,
    ime_mode: i32,
    use_mouse_key: bool,
    trap_key: bool,
    modal: bool,
    mask_region: Option<i32>,
    /// `tTVPBorderStyle` (0 = `bsNone`).
    border_style: i32,
    stay_on_top: bool,
    /// Reference `tTVPWindow` defaults (`TVPWindow.h`: `5`, `5`).
    touch_scale_threshold: f64,
    touch_rotate_threshold: f64,
}

impl Default for WindowInst {
    fn default() -> Self {
        Self {
            id: None,
            left: 0,
            top: 0,
            min_width: 0,
            min_height: 0,
            max_width: 0,
            max_height: 0,
            // Reference `iWindowLayer::GetFocusable` default is true.
            focusable: true,
            zoom_numer: 1,
            zoom_denom: 1,
            full_screen: false,
            wait_vsync: false,
            enable_touch: false,
            hint_delay: 500,
            mouse_cursor_state: 0,
            ime_mode: 0,
            use_mouse_key: false,
            trap_key: false,
            modal: false,
            mask_region: None,
            border_style: 0,
            stay_on_top: false,
            touch_scale_threshold: 5.0,
            touch_rotate_threshold: 5.0,
        }
    }
}

/// The `Window` mouse-velocity sampler.
///
/// L2 [`InputState`] tracks the live pointer but, unlike touch contacts, has
/// no mouse velocity of its own. This compat state samples the pointer at
/// `Window.getMouseVelocity` and reports the delta since the previous sample
/// per window; `Window.resetMouseVelocity` clears it. The state is keyed by
/// scene id and lives in [`CompatServices::state`](crate::CompatServices::state),
/// never a global.
#[derive(Default)]
struct MouseVelocityState {
    /// window scene id → pointer position at the previous sample.
    last: HashMap<u32, (f32, f32)>,
}

/// Resolve a `getTouchPoint`/`getTouchVelocity` argument that may be a
/// contact id or a 0-based index into the contacts sorted by id.
fn resolve_touch_id(input: &InputState, arg: i64) -> Option<u64> {
    if arg < 0 {
        return None;
    }
    let value = arg as u64;
    if input.touch_contact(value).is_some() {
        return Some(value);
    }
    let mut ids: Vec<u64> = input.touch_contacts().map(|c| c.point.id).collect();
    ids.sort_unstable();
    ids.get(value as usize).copied()
}

/// Retain `objthis` under `id` so cross-object properties can return it.
fn register_window(ctx: &CompatCtx, host: &Tjs2Engine, id: u32, objthis: *mut c_void) {
    if objthis.is_null() {
        return;
    }
    match host.retain_object_detached(objthis) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        Ok(object) => {
            unsafe { ctx.services() }
                .state::<VisualState>()
                .objects
                .windows
                .insert(id, object);
        }
        Err(error) => log::warn!("Window: could not retain the script object: {error}"),
    }
}

// ---------------------------------------------------------------------------
// Window
// ---------------------------------------------------------------------------

extern "C" fn window_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(WindowInst::default())) as *mut c_void
}

extern "C" fn window_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = unsafe { &*(instance as *const WindowInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<VisualState>()
            .objects
            .windows
            .remove(&id);
    }
    // SAFETY: instance came from `window_create`'s `Box::into_raw` and is
    // freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut WindowInst) });
}

/// `Window()` — add a window to the scene and return its id.
extern "C" fn window_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window: compat context is not bound");
    };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    if inst.id.is_some() {
        return report_error(out_error, "Window: this window is already constructed");
    }
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The game immediately sizes the window; a non-zero default keeps
    // `win.width`/`win.height` sane before `setInnerSize` (the reference
    // opens with a default size).
    let id = scene.add_window(String::new(), (1280, 720));
    inst.id = Some(id);
    register_window(&ctx, resolved.engine(), id, objthis);
    set_int(out, i64::from(id));
    0
}

/// `Window.close()` — destroy the window and its layer tree.
extern "C" fn window_close(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.close: compat context is not bound");
    };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Window.close: this window is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    scene.remove_window(id);
    inst.id = None;
    // SAFETY: the caller resolved `ctx` from the installed binding.
    unsafe { ctx.services() }
        .state::<VisualState>()
        .objects
        .windows
        .remove(&id);
    set_void(out);
    0
}

/// `Window.bringToFront()` — record a host raise request.
extern "C" fn window_bring_to_front(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.bringToFront: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    if let Some(id) = inst.id {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<VisualState>()
            .bring_to_front
            .push(id);
    }
    set_void(out);
    0
}

/// `Window.update([type])` — mark every layer of the window for repaint and
/// record a host redraw request.
extern "C" fn window_update(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.update: compat context is not bound");
    };
    let update_type = if argc >= 1 {
        value_as_i64(&unsafe { args(argv, argc) }[0]) as i32
    } else {
        0
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Window.update: this window is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    for layer_id in scene.window_layer_order(id) {
        if let Some(layer) = scene.layer_mut(layer_id) {
            layer.pending_paint = true;
        }
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    unsafe { ctx.services() }
        .state::<VisualState>()
        .update_requests
        .push((id, update_type));
    set_void(out);
    0
}

/// `Window.add(layer)` — attach a layer under `primaryLayer`.
extern "C" fn window_add(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Window.add requires 1 argument");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.add: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let layer_id = match resolve_object_id(resolved.engine(), &a[0]) {
        Ok(id) if id >= 0 => id as u32,
        Ok(_) => return report_error(out_error, "Window.add: not a Layer object"),
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(window) = inst.id else {
        return report_error(out_error, "Window.add: this window is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let primary = scene
        .window(window)
        .and_then(|w| w.primary_layer)
        .filter(|&primary| primary != layer_id);
    reparent_layer(scene, layer_id, primary);
    set_void(out);
    0
}

/// `Window.remove(layer)` — part a layer from this window (`Part()`
/// detachment; the children stay alive).
extern "C" fn window_remove(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Window.remove requires 1 argument");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.remove: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let layer_id = match resolve_object_id(resolved.engine(), &a[0]) {
        Ok(id) if id >= 0 => id as u32,
        Ok(_) => return report_error(out_error, "Window.remove: not a Layer object"),
        Err(error) => return report_error(out_error, &error),
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(window) = inst.id else {
        return report_error(out_error, "Window.remove: this window is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if scene.layer(layer_id).is_some_and(|l| l.window == window) {
        scene.remove_layer(layer_id);
    }
    set_void(out);
    0
}

/// `Window.setSize(width, height)` and `setInnerSize(width, height)`.
extern "C" fn window_set_size(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Window.setSize requires 2 arguments");
    }
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.setSize: compat context is not bound");
    };
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Window.setSize: this window is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(window) = scene.window_mut(id) {
        window.inner_size = (width, height);
    }
    set_void(out);
    0
}

/// `Window.setInnerSize(width, height)`.
extern "C" fn window_set_inner_size_method(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    window_set_size(engine, instance, argc, argv, out, out_error, _objthis)
}

/// `Window.setPos(left, top)`.
extern "C" fn window_set_pos(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Window.setPos requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.left = value_as_i64(&a[0]) as i32;
    inst.top = value_as_i64(&a[1]) as i32;
    set_void(out);
    0
}

/// `Window.setMinSize(width, height)`.
extern "C" fn window_set_min_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Window.setMinSize requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.min_width = value_as_i64(&a[0]) as i32;
    inst.min_height = value_as_i64(&a[1]) as i32;
    set_void(out);
    0
}

/// `Window.setMaxSize(width, height)`.
extern "C" fn window_set_max_size(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Window.setMaxSize requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.max_width = value_as_i64(&a[0]) as i32;
    inst.max_height = value_as_i64(&a[1]) as i32;
    set_void(out);
    0
}

/// `Window.setZoom(numer, denom)`.
extern "C" fn window_set_zoom(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Window.setZoom requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    let numer = value_as_i64(&a[0]).max(1) as i32;
    let denom = value_as_i64(&a[1]).max(1) as i32;
    inst.zoom_numer = numer;
    inst.zoom_denom = denom;
    set_void(out);
    0
}

/// `Window.showModal([modal])` — record the modal state.
extern "C" fn window_show_modal(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.modal = if argc >= 1 {
        value_as_bool(&unsafe { args(argv, argc) }[0])
    } else {
        true
    };
    set_void(out);
    0
}

/// `Window.setMaskRegion(threshold)`.
extern "C" fn window_set_mask_region(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Window.setMaskRegion requires 1 argument");
    }
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.mask_region = Some(value_as_i64(&unsafe { args(argv, argc) }[0]) as i32);
    set_void(out);
    0
}

/// `Window.removeMaskRegion()`.
extern "C" fn window_remove_mask_region(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.mask_region = None;
    set_void(out);
    0
}

/// `Window.hideMouseCursor()` — record the cursor-hidden state.
extern "C" fn window_hide_mouse_cursor(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.mouse_cursor_state = 2;
    set_void(out);
    0
}

/// `Window.postInputEvent(name[, params])` — accepted and logged; the host
/// input bridge is not wired yet.
extern "C" fn window_post_input_event(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Window.postInputEvent requires an event name");
    }
    let name = value_as_string(&unsafe { args(argv, argc) }[0]);
    log::debug!("Window.postInputEvent({name:?}): no input bridge is wired; ignored");
    set_void(out);
    0
}

// --- Window properties -----------------------------------------------------

extern "C" fn window_id_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const WindowInst) };
    match inst.id {
        Some(id) => set_int(out, i64::from(id)),
        None => set_int(out, 0),
    }
    0
}

extern "C" fn window_visible_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.visible: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        set_bool(out, false);
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    set_bool(out, scene.window(id).is_some_and(|w| w.visible));
    0
}

extern "C" fn window_visible_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.visible: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let visible = value_as_bool(unsafe { &*value });
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(window) = scene.window_mut(id) {
        window.visible = visible;
    }
    0
}

extern "C" fn window_caption_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.caption: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let title = scene
        .window(id)
        .map(|w| w.title.clone())
        .unwrap_or_default();
    set_string_out(&ctx, out, &title);
    0
}

extern "C" fn window_caption_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.caption: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let title = value_as_string(unsafe { &*value });
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(window) = scene.window_mut(id) {
        window.title = title;
    }
    0
}

extern "C" fn window_width_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.width: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let width = scene.window(id).map_or(0, |w| w.inner_size.0);
    set_int(out, i64::from(width));
    0
}

extern "C" fn window_width_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.width: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let width = value_as_i64(unsafe { &*value }).max(0) as u32;
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(window) = scene.window_mut(id) {
        window.inner_size.0 = width;
    }
    0
}

extern "C" fn window_height_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.height: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let height = scene.window(id).map_or(0, |w| w.inner_size.1);
    set_int(out, i64::from(height));
    0
}

extern "C" fn window_height_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.height: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    let height = value_as_i64(unsafe { &*value }).max(0) as u32;
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(window) = scene.window_mut(id) {
        window.inner_size.1 = height;
    }
    0
}

/// Shared accessor for the `WindowInst`-held integer/bool properties.
macro_rules! window_i64_prop {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { &*(instance as *const WindowInst) };
            set_int(out, i64::from(inst.$field));
            0
        }
        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { &mut *(instance as *mut WindowInst) };
            inst.$field = value_as_i64(unsafe { &*value }) as i32;
            0
        }
    };
}

window_i64_prop!(window_left_get, window_left_set, left);
window_i64_prop!(window_top_get, window_top_set, top);
window_i64_prop!(window_min_width_get, window_min_width_set, min_width);
window_i64_prop!(window_min_height_get, window_min_height_set, min_height);
window_i64_prop!(window_max_width_get, window_max_width_set, max_width);
window_i64_prop!(window_max_height_get, window_max_height_set, max_height);
window_i64_prop!(window_zoom_numer_get, window_zoom_numer_set, zoom_numer);
window_i64_prop!(window_zoom_denom_get, window_zoom_denom_set, zoom_denom);
window_i64_prop!(window_hint_delay_get, window_hint_delay_set, hint_delay);
window_i64_prop!(
    window_mouse_cursor_state_get,
    window_mouse_cursor_state_set,
    mouse_cursor_state
);
window_i64_prop!(window_ime_mode_get, window_ime_mode_set, ime_mode);

macro_rules! window_bool_prop {
    ($get:ident, $set:ident, $field:ident) => {
        extern "C" fn $get(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { &*(instance as *const WindowInst) };
            set_bool(out, inst.$field);
            0
        }
        extern "C" fn $set(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let inst = unsafe { &mut *(instance as *mut WindowInst) };
            inst.$field = value_as_bool(unsafe { &*value });
            0
        }
    };
}

window_bool_prop!(window_focusable_get, window_focusable_set, focusable);
window_bool_prop!(window_full_screen_get, window_full_screen_set, full_screen);
window_bool_prop!(window_wait_vsync_get, window_wait_vsync_set, wait_vsync);
window_bool_prop!(
    window_enable_touch_get,
    window_enable_touch_set,
    enable_touch
);
window_bool_prop!(
    window_use_mouse_key_get,
    window_use_mouse_key_set,
    use_mouse_key
);
window_bool_prop!(window_trap_key_get, window_trap_key_set, trap_key);

/// `Window.mainWindow` — the scene's first window object (`TVPMainWindow`).
extern "C" fn window_main_window_get(
    engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.mainWindow: compat context is not bound");
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let id = scene.windows.first().map(|w| w.id);
    let object =
        id.and_then(|id| duplicate_object(&ctx, resolved.engine(), ObjectKind::Window, id));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `Window.focusedLayer` — the window's focused layer object, or `null`.
extern "C" fn window_focused_layer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.focusedLayer: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        set_null_out(resolved.engine(), out);
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let focused = scene.window(id).and_then(|w| w.focused_layer);
    let object = focused
        .and_then(|layer| duplicate_object(&ctx, resolved.engine(), ObjectKind::Layer, layer));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `Window.focusedLayer = layer` — focus the layer (null/void clears it).
extern "C" fn window_focused_layer_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.focusedLayer: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(window) = inst.id else {
        return 0;
    };
    let value = unsafe { &*value };
    let layer_id = match resolve_object_id(resolved.engine(), value) {
        Ok(id) if id >= 0 => Some(id as u32),
        _ => None,
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    match layer_id {
        Some(layer) => {
            scene.set_focus(layer);
        }
        None => {
            scene.clear_focus(window);
        }
    }
    0
}

/// `Window.primaryLayer` — the window's primary layer object, or `null`.
extern "C" fn window_primary_layer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.primaryLayer: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        set_null_out(resolved.engine(), out);
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let primary = scene.window(id).and_then(|w| w.primary_layer);
    let object = primary
        .and_then(|layer| duplicate_object(&ctx, resolved.engine(), ObjectKind::Layer, layer));
    set_object_out(resolved.engine(), out, object.as_ref());
    0
}

/// `Window.touchPointCount` — the number of active touch contacts.
extern "C" fn window_touch_point_count_get(
    engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.touchPointCount: compat context is not bound",
        );
    };
    let input = match input_ref(&ctx) {
        Ok(input) => input,
        Err(error) => return report_error(out_error, &error),
    };
    set_int(out, input.touch_point_count() as i64);
    0
}

/// `Window.getTouchPoint(id)` — a dictionary `{ startX, startY, x, y, ID }`
/// for the contact, matching the reference. The argument may be a contact id
/// or a 0-based index into the contacts sorted by id.
extern "C" fn window_get_touch_point(
    engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Window.getTouchPoint requires 1 argument");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.getTouchPoint: compat context is not bound",
        );
    };
    let input = match input_ref(&ctx) {
        Ok(input) => input,
        Err(error) => return report_error(out_error, &error),
    };
    let a = unsafe { args(argv, argc) };
    let Some(id) = resolve_touch_id(input, value_as_i64(&a[0])) else {
        return report_error(out_error, "Window.getTouchPoint: no such touch contact");
    };
    let Some(contact) = input.touch_contact(id) else {
        return report_error(out_error, "Window.getTouchPoint: no such touch contact");
    };
    let host = resolved.engine();
    let dict = match host.eval_retained("%[]", "Window.getTouchPoint") {
        Ok(RetainedValue::Object(object)) => object,
        Ok(RetainedValue::Value(_)) | Err(_) => {
            return report_error(
                out_error,
                "Window.getTouchPoint: cannot build the result object",
            );
        }
    };
    let entries: [(&str, TjsValue); 5] = [
        ("startX", TjsValue::Real(f64::from(contact.start_pos.0))),
        ("startY", TjsValue::Real(f64::from(contact.start_pos.1))),
        ("x", TjsValue::Real(f64::from(contact.point.pos.0))),
        ("y", TjsValue::Real(f64::from(contact.point.pos.1))),
        ("ID", TjsValue::Integer(contact.point.id as i64)),
    ];
    for (key, value) in entries {
        if let Err(error) = host.set_member(dict.raw_id(), key, &value) {
            return report_error(out_error, &format!("Window.getTouchPoint: {error}"));
        }
    }
    set_retained_out(host, out, dict.raw_id());
    std::mem::forget(dict);
    0
}

/// `Window.getTouchVelocity(id)` — a dictionary `{ x, y, speed }` for the
/// contact, or TJS `null` when it is not active. The argument may be a
/// contact id or a 0-based index into the contacts sorted by id.
extern "C" fn window_get_touch_velocity(
    engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Window.getTouchVelocity requires 1 argument");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.getTouchVelocity: compat context is not bound",
        );
    };
    let input = match input_ref(&ctx) {
        Ok(input) => input,
        Err(error) => return report_error(out_error, &error),
    };
    let a = unsafe { args(argv, argc) };
    let Some(id) = resolve_touch_id(input, value_as_i64(&a[0])) else {
        set_null_out(resolved.engine(), out);
        return 0;
    };
    let (vx, vy) = input.touch_velocity(id).unwrap_or((0.0, 0.0));
    let speed = input.touch_speed(id).unwrap_or(0.0);
    let host = resolved.engine();
    let dict = match host.eval_retained("%[]", "Window.getTouchVelocity") {
        Ok(RetainedValue::Object(object)) => object,
        Ok(RetainedValue::Value(_)) | Err(_) => {
            return report_error(
                out_error,
                "Window.getTouchVelocity: cannot build the result object",
            );
        }
    };
    let entries: [(&str, TjsValue); 3] = [
        ("x", TjsValue::Real(f64::from(vx))),
        ("y", TjsValue::Real(f64::from(vy))),
        ("speed", TjsValue::Real(f64::from(speed))),
    ];
    for (key, value) in entries {
        if let Err(error) = host.set_member(dict.raw_id(), key, &value) {
            return report_error(out_error, &format!("Window.getTouchVelocity: {error}"));
        }
    }
    set_retained_out(host, out, dict.raw_id());
    std::mem::forget(dict);
    0
}

// --- Window event handlers -------------------------------------------------

/// The default body shared by every `Window.on*` event handler.
///
/// The reference declares each handler as an empty native method
/// (`TJS_BEGIN_NATIVE_METHOD_DECL(onClick)` in `WindowImpl.cpp`), which a
/// script assignment (`win.onClick = function (...) { ... }`) shadows through
/// the instance's own member — exactly like `MenuItem.onClick`. The L3 event
/// adapter / `BitmapLayerTreeOwner` invokes the handler by name, so a script
/// override wins and this no-op otherwise.
extern "C" fn window_noop_handler(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

// --- Window input ----------------------------------------------------------

/// Coerce an integer/real TJS value to `f64` (`0` for anything else).
fn value_as_f64(value: &Value) -> f64 {
    match value.ty {
        tjs::VAL_REAL => value.real,
        tjs::VAL_INTEGER => value.integer as f64,
        _ => 0.0,
    }
}

/// `Window.getMouseVelocity(x, y, speed)` — the pointer delta since the
/// previous sample over the L2 [`InputState`] pointer, with the reference
/// out-parameter/boolean-result shape.
extern "C" fn window_get_mouse_velocity(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "Window.getMouseVelocity requires 3 arguments");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.getMouseVelocity: compat context is not bound",
        );
    };
    let input = match input_ref(&ctx) {
        Ok(input) => input,
        Err(error) => return report_error(out_error, &error),
    };
    let (x, y) = input.pointer();
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Window.getMouseVelocity: this window is not constructed",
        );
    };
    let (vx, vy) = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let mut state = unsafe { ctx.services() }.state::<MouseVelocityState>();
        match state.last.insert(id, (x, y)) {
            Some((px, py)) => (x - px, y - py),
            None => (0.0, 0.0),
        }
    };
    let speed = (vx * vx + vy * vy).sqrt();
    let host = resolved.engine();
    for (index, value) in [f64::from(vx), f64::from(vy), f64::from(speed)]
        .into_iter()
        .enumerate()
    {
        if let Err(error) = host.set_arg(index, &TjsValue::Real(value)) {
            return report_error(out_error, &format!("Window.getMouseVelocity: {error}"));
        }
    }
    set_bool(out, vx != 0.0 || vy != 0.0);
    0
}

/// `Window.resetMouseVelocity()` — drop the pointer sample for this window.
extern "C" fn window_reset_mouse_velocity(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.resetMouseVelocity: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Window.resetMouseVelocity: this window is not constructed",
        );
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    unsafe { ctx.services() }
        .state::<MouseVelocityState>()
        .last
        .remove(&id);
    set_void(out);
    0
}

// --- Window host facts -----------------------------------------------------

/// `Window.HWND` — Win32-only. D11: an absent host fact is a typed error, not
/// a silent `0`.
extern "C" fn window_hwnd_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    report_error(
        out_error,
        "Window.HWND: no Win32 window handle is available on this host",
    )
}

/// `Window.drawDevice` — no L2 draw-device object is exposed. D11: typed
/// error instead of a fabricated object. The wording also keeps the legacy
/// "absent member" guard truthful: the host object really does not exist.
extern "C" fn window_draw_device_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    report_error(
        out_error,
        "Window.drawDevice: the host draw-device object does not exist on this host",
    )
}

/// `Window.drawDevice = value` — see [`window_draw_device_get`].
extern "C" fn window_draw_device_set(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    report_error(
        out_error,
        "Window.drawDevice: the host draw-device object does not exist on this host",
    )
}

/// `Window.displayOrientation` — no display is attached to the L2 scene.
/// D11: typed error instead of a hardcoded SM_* value.
extern "C" fn window_display_orientation_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    report_error(
        out_error,
        "Window.displayOrientation: no display is attached to this host",
    )
}

/// `Window.displayRotate` — no display is attached to the L2 scene. D11:
/// typed error instead of a hardcoded `0`.
extern "C" fn window_display_rotate_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    report_error(
        out_error,
        "Window.displayRotate: no display is attached to this host",
    )
}

/// `Window.registerMessageReceiver(mode, recv, handler)` — Win32-only. D11:
/// typed error, never a silent no-op.
extern "C" fn window_register_message_receiver(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(
            out_error,
            "Window.registerMessageReceiver requires 3 arguments",
        );
    }
    report_error(
        out_error,
        "Window.registerMessageReceiver: Win32 message receivers are not available on this host",
    )
}

/// `Window.findFullScreenCandidates(...)` — the reference body is disabled
/// (`#if 0`); no L2 platform display list is exposed. D11: typed error rather
/// than returning fabricated candidates.
extern "C" fn window_find_full_screen_candidates(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 5 {
        return report_error(
            out_error,
            "Window.findFullScreenCandidates requires 5 arguments",
        );
    }
    report_error(
        out_error,
        "Window.findFullScreenCandidates: no platform display-list service is attached",
    )
}

/// `Window.layerTreeOwnerInterface` — the window object itself is the layer
/// tree owner (the reference returns the `iTVPLayerTreeOwner` implemented by
/// `tTJSNI_Window`). Returning the owner object keeps the L3 surface free of
/// raw interface addresses and does not depend on the `BitmapLayerTreeOwner`
/// class; the two coordinate by member name only.
extern "C" fn window_layer_tree_owner_interface_get(
    engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, _ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.layerTreeOwnerInterface: compat context is not bound",
        );
    };
    let host = resolved.engine();
    if objthis.is_null() {
        set_null_out(host, out);
        return 0;
    }
    match host.retain_object_detached(objthis) {
        Ok(object) => {
            set_object_out(host, out, Some(&object));
            0
        }
        Err(error) => report_error(
            out_error,
            &format!("Window.layerTreeOwnerInterface: {error}"),
        ),
    }
}

// `borderStyle`/`stayOnTop` are per-window facts with no host side effect, so
// they round-trip through the instance payload like `left`/`top`.
window_i64_prop!(
    window_border_style_get,
    window_border_style_set,
    border_style
);
window_bool_prop!(window_stay_on_top_get, window_stay_on_top_set, stay_on_top);

/// `Window.touchScaleThreshold` (reference default `5`).
extern "C" fn window_touch_scale_threshold_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const WindowInst) };
    set_real(out, inst.touch_scale_threshold);
    0
}

extern "C" fn window_touch_scale_threshold_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.touch_scale_threshold = value_as_f64(unsafe { &*value });
    0
}

/// `Window.touchRotateThreshold` (reference default `5`).
extern "C" fn window_touch_rotate_threshold_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const WindowInst) };
    set_real(out, inst.touch_rotate_threshold);
    0
}

extern "C" fn window_touch_rotate_threshold_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.touch_rotate_threshold = value_as_f64(unsafe { &*value });
    0
}

/// Drain the queued `Window.bringToFront` requests (`(window_id)`).
#[allow(dead_code)]
pub fn take_bring_to_front_requests(services: &crate::CompatServices) -> Vec<u32> {
    std::mem::take(&mut services.state::<VisualState>().bring_to_front)
}

/// Drain the queued `Window.update` requests (`(window_id, update_type)`).
#[allow(dead_code)]
pub fn take_update_requests(services: &crate::CompatServices) -> Vec<(u32, i32)> {
    std::mem::take(&mut services.state::<VisualState>().update_requests)
}

pub(super) fn register_window_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Window",
        create: window_create,
        destroy: window_destroy,
        invalidate: None,
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
                name: "add",
                f: window_add,
            },
            NativeInstanceMethodDef {
                name: "remove",
                f: window_remove,
            },
            NativeInstanceMethodDef {
                name: "setSize",
                f: window_set_size,
            },
            NativeInstanceMethodDef {
                name: "setInnerSize",
                f: window_set_inner_size_method,
            },
            NativeInstanceMethodDef {
                name: "setPos",
                f: window_set_pos,
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
                name: "setZoom",
                f: window_set_zoom,
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
                name: "hideMouseCursor",
                f: window_hide_mouse_cursor,
            },
            NativeInstanceMethodDef {
                name: "postInputEvent",
                f: window_post_input_event,
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
            NativeInstanceMethodDef {
                name: "registerMessageReceiver",
                f: window_register_message_receiver,
            },
            NativeInstanceMethodDef {
                name: "findFullScreenCandidates",
                f: window_find_full_screen_candidates,
            },
            NativeInstanceMethodDef {
                name: "onActivate",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onClick",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onCloseQuery",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onDeactivate",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onDisplayRotate",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onDoubleClick",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onFileDrop",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onKeyDown",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onKeyPress",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onKeyUp",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onMouseDown",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onMouseEnter",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onMouseLeave",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onMouseMove",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onMouseUp",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onMouseWheel",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onMultiTouch",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onPopupHide",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onResize",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onTouchDown",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onTouchMove",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onTouchRotate",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onTouchScaling",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onTouchUp",
                f: window_noop_handler,
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
                name: "focusable",
                get: Some(window_focusable_get),
                set: Some(window_focusable_set),
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
                name: "fullScreen",
                get: Some(window_full_screen_get),
                set: Some(window_full_screen_set),
            },
            NativeInstancePropertyDef {
                name: "waitVSync",
                get: Some(window_wait_vsync_get),
                set: Some(window_wait_vsync_set),
            },
            NativeInstancePropertyDef {
                name: "enableTouch",
                get: Some(window_enable_touch_get),
                set: Some(window_enable_touch_set),
            },
            NativeInstancePropertyDef {
                name: "hintDelay",
                get: Some(window_hint_delay_get),
                set: Some(window_hint_delay_set),
            },
            NativeInstancePropertyDef {
                name: "mouseCursorState",
                get: Some(window_mouse_cursor_state_get),
                set: Some(window_mouse_cursor_state_set),
            },
            NativeInstancePropertyDef {
                name: "imeMode",
                get: Some(window_ime_mode_get),
                set: Some(window_ime_mode_set),
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
                name: "mainWindow",
                get: Some(window_main_window_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "focusedLayer",
                get: Some(window_focused_layer_get),
                set: Some(window_focused_layer_set),
            },
            NativeInstancePropertyDef {
                name: "primaryLayer",
                get: Some(window_primary_layer_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "touchPointCount",
                get: Some(window_touch_point_count_get),
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
                name: "borderStyle",
                get: Some(window_border_style_get),
                set: Some(window_border_style_set),
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
            NativeInstancePropertyDef {
                name: "stayOnTop",
                get: Some(window_stay_on_top_get),
                set: Some(window_stay_on_top_set),
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
                name: "layerTreeOwnerInterface",
                get: Some(window_layer_tree_owner_interface_get),
                set: None,
            },
        ],
    })
}
