//! The visual `Window` native class.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use engine::input::InputState;
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    NativePropertyDef, NativeStaticMembers, RetainedValue, Tjs2Engine, TjsValue, Value,
};

use super::common::{
    ObjectKind, VisualState, WindowObject, bound, duplicate_object, input_ref, resolve_object_id,
    scene_mut, scene_ref, set_null_out, set_object_out, set_real, set_retained_out,
};
use super::{clear_layer_focus, focus_layer_in_input_domain};
use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{set_string_out, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError};
use tjs::VAL_OBJECT;

/// The client size a window starts with.
const DEFAULT_WINDOW_SIZE: (u32, u32) = (1280, 720);

/// `Window` payload: the scene id plus the window properties the L2
/// [`engine::WindowState`] does not model (there is no OS chrome).
struct WindowInst {
    id: Option<u32>,
    /// Set by the native constructor (`super.Window(...)`). Before it runs the
    /// window exists but is not yet registered with the manager; `id` is
    /// already allocated so the reference's pre-`Construct` member writes
    /// land.
    attached: bool,
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
    /// The `Window.drawDevice` object (reference `DrawDeviceObject`,
    /// `WindowIntf.h:190-194`). The window installs a default
    /// `BasicDrawDevice` at construction.
    draw_device: Option<DetachedValue>,
    /// `Window.menu` (reference `WindowMenuProperty`,
    /// `impl/MenuItemImpl.cpp:65-91`): the per-window root `MenuItem`, built
    /// on first read and kept so repeated reads return the same object.
    menu_root: Option<DetachedValue>,
    /// `Window.registerMessageReceiver` entries as `(proc, userdata)`
    /// (reference `tTVPWMRRegMode` register/unregister).
    message_receivers: Vec<(i64, i64)>,
}

impl Default for WindowInst {
    fn default() -> Self {
        Self {
            id: None,
            attached: false,
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
            draw_device: None,
            menu_root: None,
            message_receivers: Vec::new(),
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
            // SAFETY: the callback resolved this context from the installed binding, whose service set is live for the whole VM lifetime.
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

extern "C" fn window_create(engine: *mut c_void) -> *mut c_void {
    // The reference's `CreateNativeInstance` returns a window whose fields are
    // already usable; only the manager registration and the default draw
    // device wait for `Construct`. A script subclass may touch a `Window`
    // member before it calls `super.Window(...)`, so the scene window exists
    // from creation, at the default client size.
    let mut inst = WindowInst::default();
    if let Ok((_resolved, ctx)) = bound(engine)
        && let Ok(scene) = scene_mut(&ctx)
    {
        inst.id = Some(scene.add_window(String::new(), DEFAULT_WINDOW_SIZE));
    }
    Box::into_raw(Box::new(inst)) as *mut c_void
}

extern "C" fn window_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    let inst = unsafe { &*(instance as *const WindowInst) };
    if let (Some(id), Ok((resolved, ctx))) = (inst.id, bound(engine)) {
        let registered = {
            // SAFETY: the caller resolved `ctx` from the installed binding.
            let mut state = unsafe { ctx.services() }.state::<VisualState>();
            state.objects.windows.remove(&id);
            state.window_objects.remove(&id)
        };
        // Invalidate every object registered through `Window.add` (reference
        // `ObjectVector`); the retained values are released when `registered`
        // drops, breaking the window→object references.
        if let Some(registered) = registered {
            for entry in &registered {
                if let Err(error) = resolved.engine().invalidate_retained(entry.value.raw_id()) {
                    log::warn!("Window: invalidate failed: {error}");
                }
            }
        }
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    if unsafe { &*(instance as *const WindowInst) }.attached {
        return report_error(out_error, "Window: this window is already constructed");
    }
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The game immediately sizes the window; a non-zero default keeps
    // `win.width`/`win.height` sane before `setInnerSize` (the reference
    // opens with a default size).
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM
    // owns it and keeps it alive and unaliased for the duration of this
    // callback.
    let id = match unsafe { &*(instance as *const WindowInst) }.id {
        // `window_create` already allocated the scene window; only the
        // manager join and the draw device wait for the constructor.
        Some(id) => id,
        // No scene was available at creation time; allocate it now.
        None => {
            let id = scene.add_window(String::new(), DEFAULT_WINDOW_SIZE);
            // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
            unsafe { &mut *(instance as *mut WindowInst) }.id = Some(id);
            id
        }
    };
    // The scene borrow ends here, before `eval_retained` below: that call runs
    // script, which could otherwise re-enter a `Window` member and mint a
    // second `&mut` to the same payload.
    register_window(&ctx, resolved.engine(), id, objthis);
    // Install the default draw device: the reference constructs a
    // `BasicDrawDevice` for every window and throws `TVPInternalError` when
    // `CreateNew` fails (`reference/cpp/core/visual/WindowIntf.cpp:150-156`).
    // `Window.BasicDrawDevice` is a registered class member, so this path is
    // reached when that member's `new` raises (e.g. a host removed the
    // member). A failed constructor destroys the half-built instance, so
    // `window_destroy` still removes the scene window and its registry entry.
    match resolved
        .engine()
        .eval_retained("new Window.BasicDrawDevice()", "Window")
    {
        Ok(RetainedValue::Object(object)) => {
            // SAFETY: `instance` is the `WindowInst` boxed by `window_create`;
            // this write runs after `eval_retained` returned, so the borrow
            // never spans a call that could re-enter the VM.
            unsafe { &mut *(instance as *mut WindowInst) }.draw_device = Some(object);
        }
        Ok(RetainedValue::Value(_)) | Err(_) => {
            return report_error(out_error, "Internal error");
        }
    }
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; this
    // write runs after `eval_retained` returned, so no borrow spans the call.
    unsafe { &mut *(instance as *mut WindowInst) }.attached = true;
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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
        // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
        value_as_i64(&unsafe { args(argv, argc) }[0]) as i32
    } else {
        0
    };
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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

/// `Window.add(obj)` — register an object whose lifetime is tied to the
/// window (reference `tTJSNI_BaseWindow::Add` / `ObjectVector`). The window
/// retains `obj` and invalidates it when the window is destroyed; adding the
/// same closure twice is a no-op.
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
    // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a[0].ty != VAL_OBJECT {
        return report_error(out_error, "Window.add requires an object argument");
    }
    // Identity is the raw object pointer plus the closure receiver (the
    // reference compares `tTJSVariantClosure`s).
    let identity = (a[0].retained, a[0].array as usize);
    let value = match resolved.engine().retain_object_arg(&a[0]) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(window) = inst.id else {
        return report_error(out_error, "Window.add: this window is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut registered = unsafe { ctx.services() }.state::<VisualState>();
    let entries = registered.window_objects.entry(window).or_default();
    if entries.iter().any(|entry| entry.identity == identity) {
        set_void(out);
        return 0;
    }
    entries.push(WindowObject { identity, value });
    set_void(out);
    0
}

/// `Window.remove(obj)` — unregister a `Window.add`ed object (reference
/// `tTJSNI_BaseWindow::Remove`). Dropping the retained value also breaks the
/// window→object reference.
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
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Window.remove: compat context is not bound");
    };
    // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a[0].ty != VAL_OBJECT {
        return report_error(out_error, "Window.remove requires an object argument");
    }
    let identity = (a[0].retained, a[0].array as usize);
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(window) = inst.id else {
        return report_error(out_error, "Window.remove: this window is not constructed");
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mut registered = unsafe { ctx.services() }.state::<VisualState>();
    if let Some(entries) = registered.window_objects.get_mut(&window) {
        entries.retain(|entry| entry.identity != identity);
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
    // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let width = value_as_i64(&a[0]).max(0) as u32;
    let height = value_as_i64(&a[1]).max(0) as u32;
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    let inst = unsafe { &*(instance as *const WindowInst) };
    // The reference stores the size on the form, and the form does not exist
    // until the native constructor runs, so a pre-`super.Window(...)` write is
    // a no-op.
    if !inst.attached {
        set_void(out);
        return 0;
    }
    let Some(id) = inst.id else {
        return report_error(out_error, "Window.setSize: this window is not constructed");
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // A script resize writes the window's client size; the logical screen is
    // the primary layer's rect, which only a primary-layer resize changes.
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
    // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.left = value_as_i64(&a[0]) as i32;
    inst.top = value_as_i64(&a[1]) as i32;
    set_void(out);
    0
}

/// `Window.setLayerPos(left, top)` — the reference form's base body does
/// nothing; two arguments are required (`reference/cpp/core/visual/WindowIntf.cpp:872-882`).
extern "C" fn window_set_layer_pos(
    _engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Window.setLayerPos requires 2 arguments");
    }
    set_void(out);
    0
}

/// `Window.beginMove()` — the reference form's base body does nothing.
extern "C" fn window_begin_move(
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
    // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
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
    // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.max_width = value_as_i64(&a[0]) as i32;
    inst.max_height = value_as_i64(&a[1]) as i32;
    set_void(out);
    0
}

/// `Window.setZoom(numer, denom)`.
///
/// The ratio is stored for the script to read back; the window client size and
/// the primary layer keep their sizes.
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
    // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.modal = if argc >= 1 {
        // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    inst.mouse_cursor_state = 2;
    set_void(out);
    0
}

/// Read `params.key` / `params.shift` and convert it the way the reference's
/// `(tjs_int)val` does (`reference/cpp/core/visual/impl/WindowImpl.cpp:1217-1235`).
/// A missing member is the reference `TVPSpecifiedEventNeedsParameter2`.
fn read_param_int(
    engine: &Tjs2Engine,
    object: tjs::Tjs2ValueId,
    event: &str,
    param: &str,
) -> Result<i64, String> {
    let value = engine
        .member(object, param)
        .map_err(|_| format!("Specified event {event} needs parameter {param}"))?;
    match value {
        TjsValue::Integer(value) => Ok(value),
        TjsValue::Real(value) => Ok(value as i64),
        TjsValue::String(text) => Ok(crate::natives::system::tjs_string_to_integer(&text)),
        TjsValue::Void => Ok(0),
        TjsValue::Object | TjsValue::Retained(_) => {
            Err("value is an object, not an integer".to_string())
        }
    }
}

/// `Window.postInputEvent(name[, params])` — post a synthetic input event to
/// the window and its focused layer.
///
/// The reference accepts only `onKeyDown`, `onKeyUp` and `onKeyPress`, reads
/// `params.key` (and `params.shift` for down/up), and hands the values to the
/// window form, which posts the same TJS event a real input event would
/// (`reference/cpp/core/visual/WindowIntf.cpp:913-930`,
/// `reference/cpp/core/visual/impl/WindowImpl.cpp:1190-1264`).
extern "C" fn window_post_input_event(
    engine: *mut c_void,
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
    let Ok((resolved, mut ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Window.postInputEvent: compat context is not bound",
        );
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let name = value_as_string(&a[0]);
    let event = match name.as_str() {
        "onKeyDown" | "onKeyUp" => {
            // `eventparams = param[1]->AsObjectNoAddRef()`; a non-object
            // leaves it null and the reference raises the needs-parameter
            // error (`reference/cpp/core/visual/impl/WindowImpl.cpp:1213-1216`).
            let Some(params) = (argc >= 2)
                .then(|| &a[1])
                .filter(|value| value.ty == VAL_OBJECT)
            else {
                return report_error(
                    out_error,
                    &format!("Specified event {name} needs parameter"),
                );
            };
            let object = match resolved.engine().retain_object_arg(params) {
                Ok(object) => object,
                Err(error) => {
                    return report_error(out_error, &format!("Window.postInputEvent: {error}"));
                }
            };
            let key = match read_param_int(resolved.engine(), object.raw_id(), &name, "key") {
                Ok(value) => value,
                Err(error) => return report_error(out_error, &error),
            };
            let shift = match read_param_int(resolved.engine(), object.raw_id(), &name, "shift") {
                Ok(value) => value,
                Err(error) => return report_error(out_error, &error),
            };
            // The window form takes the key as a `tjs_uint16`
            // (`reference/cpp/core/visual/impl/WindowImpl.cpp:1237-1242`).
            let key = i64::from(key as u16);
            let shift = if name == "onKeyUp" {
                crate::event::key_up_shift(&ctx, shift)
            } else {
                shift
            };
            crate::TjsEvent {
                name: if name == "onKeyDown" {
                    "onKeyDown"
                } else {
                    "onKeyUp"
                },
                args: vec![
                    crate::EventValue::Int(key),
                    crate::EventValue::Int(shift),
                    crate::EventValue::Int(1),
                ],
            }
        }
        "onKeyPress" => {
            let Some(params) = (argc >= 2)
                .then(|| &a[1])
                .filter(|value| value.ty == VAL_OBJECT)
            else {
                return report_error(
                    out_error,
                    &format!("Specified event {name} needs parameter"),
                );
            };
            let object = match resolved.engine().retain_object_arg(params) {
                Ok(object) => object,
                Err(error) => {
                    return report_error(out_error, &format!("Window.postInputEvent: {error}"));
                }
            };
            let key = match read_param_int(resolved.engine(), object.raw_id(), &name, "key") {
                Ok(value) => value,
                Err(error) => return report_error(out_error, &error),
            };
            // `char vcl_key = key` widens the low 8 bits to the 16-bit
            // `tjs_char` the window form carries
            // (`reference/cpp/core/visual/impl/WindowImpl.cpp:1258-1261`).
            let character = String::from_utf16_lossy(&[key as u8 as i8 as i16 as u16]);
            crate::TjsEvent {
                name: "onKeyPress",
                args: vec![crate::EventValue::Str(character), crate::EventValue::Int(1)],
            }
        }
        _ => {
            return report_error(
                out_error,
                &format!("Specified event name {name} is unknown"),
            );
        }
    };
    if let Err(error) = crate::EventAdapter::new().dispatch(&mut ctx, &event) {
        return report_error(out_error, &format!("Window.postInputEvent: {error}"));
    }
    set_void(out);
    0
}

// --- Window properties -----------------------------------------------------

/// The scene id a `Window` payload carries — the reader behind the VM's
/// native-instance lookup (`native_instance_payload("Window")`), replacing
/// the removed `id` script member for engine call sites.
pub(super) fn window_payload_id(payload: *mut c_void) -> Option<u32> {
    // SAFETY: `native_instance_payload("Window")` only hands out Window payloads.
    unsafe { &*(payload as *const WindowInst) }.id
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the C++ property dispatcher passes a live `Value` for the duration of this call.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the C++ property dispatcher passes a live `Value` for the duration of this call.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the C++ property dispatcher passes a live `Value` for the duration of this call.
    let width = value_as_i64(unsafe { &*value }).max(0) as u32;
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The width setters write the client size, which is not the logical
    // screen: the primary layer's rect is.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the C++ property dispatcher passes a live `Value` for the duration of this call.
    let height = value_as_i64(unsafe { &*value }).max(0) as u32;
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    // The height setters write the client size, which is not the logical
    // screen: the primary layer's rect is.
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
            // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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
            // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
            let inst = unsafe { &mut *(instance as *mut WindowInst) };
            // SAFETY: the C++ property dispatcher passes a live `Value` for the duration of this call.
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
            // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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
            // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
            let inst = unsafe { &mut *(instance as *mut WindowInst) };
            // SAFETY: the C++ property dispatcher passes a live `Value` for the duration of this call.
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

/// `Window.innerSunken` — the reference window forwards to its form
/// (`reference/cpp/core/visual/impl/WindowImpl.cpp:1687-1697`), whose getter
/// returns a fixed `false` and ignores writes
/// (`reference/cpp/core/environ/impl/TVPWindow.h:143-144`).
extern "C" fn window_inner_sunken_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_bool(out, false);
    0
}

/// `Window.layerLeft` — the reference form reports a fixed `0` and ignores
/// writes (`reference/cpp/core/environ/impl/TVPWindow.h:178-179`).
extern "C" fn window_layer_left_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, 0);
    0
}

/// `Window.layerTop` — the reference form reports a fixed `0` and ignores
/// writes (`reference/cpp/core/environ/impl/TVPWindow.h:180-181`).
extern "C" fn window_layer_top_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, 0);
    0
}

/// `Window.showScrollBars` — the reference form reports `true` and ignores
/// writes (`reference/cpp/core/environ/impl/TVPWindow.h:183-184`).
extern "C" fn window_show_scroll_bars_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_bool(out, true);
    0
}

/// The empty setter shared by the obsolete properties whose reference form
/// setter is a no-op (`layerLeft`, `layerTop`, `showScrollBars`,
/// `innerSunken`).
extern "C" fn window_ignored_prop_set(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    0
}

/// `Window.mainWindow` (static) — the scene's first window object
/// (`TVPMainWindow`). It is a static member of the reference `Window` class,
/// so it lives on the class object and is read as `global.Window.mainWindow`.
extern "C" fn window_main_window_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(window) = inst.id else {
        return 0;
    };
    // SAFETY: the C++ property dispatcher passes a live `Value` for the duration of this call.
    let value = unsafe { &*value };
    let layer_id = match resolve_object_id(resolved.engine(), value, &["Layer"]) {
        Ok(id) if id >= 0 => Some(id as u32),
        Ok(_) => None,
        Err(error) => return report_error(out_error, &format!("Window.focusedLayer: {error}")),
    };
    // `SetFocusedLayer` = `SetFocusTo(layer, false)` on the window's
    // **first registered manager**, regardless of the layer's own domain
    // (`reference/cpp/core/environ/WindowIntf.cpp:1781` goes through the
    // draw device, `reference/cpp/core/visual/impl/DrawDevice.cpp:555-559`,
    // to `LayerTreeOwnerImpl.cpp:385-389` and `LayerManager.h:488-490`):
    // the guard and the focus events apply exactly as for any focus
    // transition, inside that domain.
    match layer_id {
        Some(layer) => {
            focus_layer_in_input_domain(&ctx, window, layer, false);
        }
        None => {
            clear_layer_focus(&ctx, window, false);
        }
    }
    0
}

/// `Window.primaryLayer` — the window's primary layer object.
///
/// Raises when the window has no primary layer; `null` only when the primary
/// layer has no owner object.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    let inst = unsafe { &*(instance as *const WindowInst) };
    let Some(id) = inst.id else {
        set_null_out(resolved.engine(), out);
        return 0;
    };
    let scene = match scene_ref(&ctx) {
        Ok(scene) => scene,
        Err(error) => return report_error(out_error, &error),
    };
    let Some(primary) = scene.window(id).and_then(|w| w.primary_layer) else {
        return report_error(out_error, "Window has no layer");
    };
    let object = duplicate_object(&ctx, resolved.engine(), ObjectKind::Layer, primary);
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
    // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
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
    // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
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

/// The default body of a `Window.on*` handler that does not dispatch an
/// `action` event. A script assignment (`win.onCloseQuery = function (...) { ... }`)
/// shadows it through the instance's own member, exactly like
/// `MenuItem.onClick`.
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

/// One named member of the reference action event dictionary and the
/// positional argument it is copied from (`TVP_ACTION_INVOKE_MEMBER`).
type EventMember = (&'static str, usize);

/// Copy one raw native argument into an event-dictionary member.
///
/// The reference `TVP_ACTION_INVOKE_MEMBER` uses `PropSet` with the variant
/// untouched, so integers, reals and strings keep their type; an object is
/// retained so it can cross as [`TjsValue::Retained`] (the retention is
/// consumed while the variant copy is built).
fn set_event_member(
    host: &Tjs2Engine,
    event: tjs::Tjs2ValueId,
    name: &str,
    value: &Value,
) -> Result<(), String> {
    match value.ty {
        tjs::VAL_VOID => host.set_member(event, name, &TjsValue::Void),
        tjs::VAL_INTEGER => host.set_member(event, name, &TjsValue::Integer(value.integer)),
        tjs::VAL_REAL => host.set_member(event, name, &TjsValue::Real(value.real)),
        tjs::VAL_STRING => host.set_member(event, name, &TjsValue::String(value_as_string(value))),
        _ => {
            let retained = host.retain_object_arg(value)?;
            let id = retained.raw_id() as u64;
            let result = host.set_member(event, name, &TjsValue::Retained(id));
            drop(retained);
            result
        }
    }
}

/// Build the reference event dictionary and invoke `owner.action(event)`.
///
/// `TVPCreateEventObject` fills `type` and `target`
/// (`reference/cpp/core/base/EventIntf.cpp:709-736`); each
/// `TVP_ACTION_INVOKE_MEMBER` then copies a positional argument to its named
/// member, and `TVP_ACTION_INVOKE_END` calls `action` on the action owner
/// (`reference/cpp/core/base/EventIntf.h:208-238`). The `FuncCall` result is
/// never inspected, so a missing `action` member is ignored.
fn dispatch_action_event(
    host: &Tjs2Engine,
    owner: &DetachedValue,
    objthis: *mut c_void,
    event_type: &str,
    argc: c_int,
    argv: *const Value,
    members: &[EventMember],
) -> Result<(), String> {
    let event = match host.eval_retained("%[]", "Window action") {
        Ok(RetainedValue::Object(event)) => event,
        Ok(RetainedValue::Value(value)) => {
            return Err(format!("event dictionary came back as {value:?}"));
        }
        Err(error) => return Err(error.to_string()),
    };
    host.set_member(event.raw_id(), "type", &TjsValue::String(event_type.into()))?;
    if !objthis.is_null()
        && let Ok(target) = host.retain_object_detached(objthis)
    {
        let target_id = target.raw_id() as u64;
        let _ = host.set_member(event.raw_id(), "target", &TjsValue::Retained(target_id));
        drop(target);
    }
    // SAFETY: `argv` carries `argc` live entries for this callback.
    let raw = unsafe { args(argv, argc) };
    for &(name, index) in members {
        if let Some(value) = raw.get(index)
            && let Err(error) = set_event_member(host, event.raw_id(), name, value)
        {
            drop(event);
            return Err(error);
        }
    }
    let event_id = event.raw_id() as u64;
    if let Err(error) = host.call_member(owner.raw_id(), "action", &[TjsValue::Retained(event_id)])
    {
        log::warn!("{event_type}: action dispatch failed: {error}");
    }
    drop(event);
    Ok(())
}

/// Define one `Window` event handler that forwards to the object's `action`
/// member, mirroring the reference `TVP_ACTION_INVOKE_*` body
/// (`reference/cpp/core/base/EventIntf.h:208-238`).
///
/// The generated handler first applies the reference
/// `if(numparams < argnum) return TJS_E_BADPARAMCOUNT;` check, then builds the
/// event dictionary and calls `objthis.action(event)`.
macro_rules! window_action_event {
    ($name:ident, $event:literal, $argnum:expr, [$(($member:literal, $index:expr)),* $(,)?]) => {
        extern "C" fn $name(
            engine: *mut c_void,
            _instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            objthis: *mut c_void,
        ) -> c_int {
            if argc < $argnum {
                return report_error(
                    out_error,
                    concat!(
                        "Window.",
                        $event,
                        ": requires ",
                        stringify!($argnum),
                        " arguments"
                    ),
                );
            }
            let Ok((resolved, _ctx)) = bound(engine) else {
                return report_error(
                    out_error,
                    concat!("Window.", $event, ": compat context is not bound"),
                );
            };
            let host = resolved.engine();
            let owner = match host.retain_object_detached(objthis) {
                Ok(owner) => owner,
                Err(error) => {
                    return report_error(
                        out_error,
                        &format!("Window.{}: {error}", $event),
                    );
                }
            };
            let result = dispatch_action_event(
                host,
                &owner,
                objthis,
                $event,
                argc,
                argv,
                &[$(($member, $index)),*],
            );
            drop(owner);
            match result {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(error) => report_error(out_error, &format!("Window.{}: {error}", $event)),
            }
        }
    };
}

// The member names and argument counts are the reference
// `TVP_ACTION_INVOKE_BEGIN`/`TVP_ACTION_INVOKE_MEMBER` pairs
// (`reference/cpp/core/visual/WindowIntf.cpp:934-1250`).
window_action_event!(window_on_activate, "onActivate", 0, []);
window_action_event!(window_on_deactivate, "onDeactivate", 0, []);
window_action_event!(window_on_resize, "onResize", 0, []);
window_action_event!(window_on_mouse_enter, "onMouseEnter", 0, []);
window_action_event!(window_on_mouse_leave, "onMouseLeave", 0, []);
window_action_event!(window_on_click, "onClick", 2, [("x", 0), ("y", 1)]);
window_action_event!(
    window_on_double_click,
    "onDoubleClick",
    2,
    [("x", 0), ("y", 1)]
);
window_action_event!(
    window_on_mouse_down,
    "onMouseDown",
    4,
    [("x", 0), ("y", 1), ("button", 2), ("shift", 3)]
);
window_action_event!(
    window_on_mouse_up,
    "onMouseUp",
    4,
    [("x", 0), ("y", 1), ("button", 2), ("shift", 3)]
);
window_action_event!(
    window_on_mouse_move,
    "onMouseMove",
    3,
    [("x", 0), ("y", 1), ("shift", 2)]
);
window_action_event!(
    window_on_mouse_wheel,
    "onMouseWheel",
    4,
    [("shift", 0), ("delta", 1), ("x", 2), ("y", 3)]
);
window_action_event!(
    window_on_touch_down,
    "onTouchDown",
    5,
    [("x", 0), ("y", 1), ("cx", 2), ("cy", 3), ("id", 4)]
);
window_action_event!(
    window_on_touch_up,
    "onTouchUp",
    5,
    [("x", 0), ("y", 1), ("cx", 2), ("cy", 3), ("id", 4)]
);
window_action_event!(
    window_on_touch_move,
    "onTouchMove",
    5,
    [("x", 0), ("y", 1), ("cx", 2), ("cy", 3), ("id", 4)]
);
window_action_event!(
    window_on_touch_scaling,
    "onTouchScaling",
    5,
    [
        ("startdistance", 0),
        ("currentdistance", 1),
        ("cx", 2),
        ("cy", 3),
        ("flag", 4)
    ]
);
window_action_event!(
    window_on_touch_rotate,
    "onTouchRotate",
    6,
    [
        ("startangle", 0),
        ("currentangle", 1),
        ("distance", 2),
        ("cx", 3),
        ("cy", 4),
        ("flag", 5)
    ]
);
window_action_event!(window_on_multi_touch, "onMultiTouch", 0, []);
window_action_event!(
    window_on_key_down,
    "onKeyDown",
    2,
    [("key", 0), ("shift", 1)]
);
window_action_event!(window_on_key_up, "onKeyUp", 2, [("key", 0), ("shift", 1)]);
window_action_event!(window_on_key_press, "onKeyPress", 1, [("key", 0)]);
window_action_event!(window_on_popup_hide, "onPopupHide", 0, []);
window_action_event!(window_on_file_drop, "onFileDrop", 1, [("files", 0)]);
window_action_event!(
    window_on_display_rotate,
    "onDisplayRotate",
    5,
    [
        ("orientation", 0),
        ("angle", 1),
        ("bpp", 2),
        ("width", 3),
        ("height", 4)
    ]
);

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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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

/// `Window.HWND` — the native instance pointer itself, cast to an integer
/// (reference `WindowImpl.cpp:2288-2298`):
/// `*result = (tTVInteger)(tjs_intptr_t)_this;`. The
/// `GetWindowHandleForPlugin()` call is commented out in the reference, so
/// this is the instance pointer, not an OS handle.
extern "C" fn window_hwnd_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, instance as i64);
    0
}

/// `Window.menu` — the per-window root `MenuItem`, built on first read and
/// cached (reference `WindowMenuProperty::PropGet`,
/// `reference/cpp/core/visual/impl/MenuItemImpl.cpp:65-82`): it reads the
/// window's `HWND`, returns the cached root for that handle, and otherwise
/// builds one with `TVPCreateMenuItemObject(objthis)`, i.e.
/// `new MenuItem(window, window)` (`MenuItemIntf.cpp:559-573`), caching it
/// under the handle. The property is registered read-only; a write is
/// `TJS_E_ACCESSDENYED` (`WindowMenuProperty::PropSet`,
/// `impl/MenuItemImpl.cpp:84-91`), which the read-only property registration
/// produces. The reference attaches this in
/// `reference/cpp/core/base/ScriptMgnIntf.cpp:538-542`.
extern "C" fn window_menu_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, _ctx)) = bound(engine) else {
        return report_error(out_error, "Window.menu: compat context is not bound");
    };
    // The reference reads `objthis->HWND` and reports `TJS_E_INVALIDOBJECT`
    // when it cannot (`impl/MenuItemImpl.cpp:66-72`); `HWND` always resolves
    // here, so a cached root is built for the window object directly.
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it, keeps it alive and passes it unaliased for the duration of this callback.
    let has_root = unsafe { &*(instance as *const WindowInst) }
        .menu_root
        .is_some();
    if !has_root {
        let host = resolved.engine();
        // `new MenuItem(this, this)` with `this` bound to the window object is
        // `TVPCreateMenuItemObject(objthis)`: the first argument is the action
        // owner and the second is the window (reference
        // `reference/cpp/core/visual/MenuItemIntf.cpp:559-573`).
        let object = match host.eval_retained_with_context(
            "new MenuItem(this, this)",
            "Window.menu",
            objthis,
            0,
        ) {
            Ok(RetainedValue::Object(object)) => object,
            Ok(_) => {
                return report_error(out_error, "Window.menu: the root is not an object");
            }
            Err(error) => return report_error(out_error, &format!("Window.menu: {error}")),
        };
        // SAFETY: as above; the eval above runs no code that reads or writes
        // this `WindowInst`, and no borrow is held across it.
        let inst = unsafe { &mut *(instance as *mut WindowInst) };
        if inst.menu_root.is_none() {
            inst.menu_root = Some(object);
        }
    }
    // SAFETY: as above; the cached value is only read here.
    let inst = unsafe { &*(instance as *const WindowInst) };
    set_object_out(resolved.engine(), out, inst.menu_root.as_ref());
    0
}

/// `Window.drawDevice` — the stored draw-device object, or `null` when none
/// was installed (reference `WindowImpl.cpp:2300-2316` returns
/// `GetDrawDeviceObject()`).
extern "C" fn window_draw_device_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, _ctx)) = bound(engine) else {
        return report_error(out_error, "Window.drawDevice: compat context is not bound");
    };
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
    let inst = unsafe { &*(instance as *const WindowInst) };
    let host = resolved.engine();
    match inst.draw_device.as_ref() {
        Some(object) => set_object_out(host, out, Some(object)),
        None => set_null_out(host, out),
    }
    0
}

/// `Window.drawDevice = value` — store a draw-device object (reference
/// `WindowImpl.cpp:2300-2316` calls `SetDrawDeviceObject`, which reads the
/// object's `interface`, `WindowIntf.cpp:255-292`). A non-object value is
/// ignored; an object whose `interface` cannot be read or is zero raises.
extern "C" fn window_draw_device_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ property dispatcher passes a live `Value` for the duration of this call.
    let value = unsafe { &*value };
    if value.ty != VAL_OBJECT {
        return 0;
    }
    let Ok((resolved, _ctx)) = bound(engine) else {
        return report_error(out_error, "Window.drawDevice: compat context is not bound");
    };
    let host = resolved.engine();
    let object = match host.retain_object_arg(value) {
        Ok(object) => object,
        Err(_) => {
            return report_error(
                out_error,
                "Window.drawDevice: cannot retrieve the interface from the draw device",
            );
        }
    };
    match host.member(object.raw_id(), "interface") {
        Ok(TjsValue::Integer(interface)) if interface != 0 => {
            // `invalidate_retained` runs the previous object's script
            // `finalize`, which may read or write `window.drawDevice` and
            // re-enter this callback, so the `WindowInst` borrow is released
            // before dispatching. The stored device object is replaced
            // (`WindowIntf.cpp:255-292`).
            let previous = {
                // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
                let inst = unsafe { &mut *(instance as *mut WindowInst) };
                inst.draw_device.replace(object)
            };
            if let Some(previous) = previous
                && let Err(error) = host.invalidate_retained(previous.raw_id())
            {
                log::warn!("Window.drawDevice: invalidate failed: {error}");
            }
            0
        }
        Ok(_) | Err(_) => report_error(
            out_error,
            "Window.drawDevice: cannot retrieve the interface from the draw device",
        ),
    }
}

/// `Window.displayOrientation` — `orientLandscape` (reference
/// `TVPWindow.h:138`, enum `TVPWindow.h:25-28`:
/// `orientUnknown = 0, orientPortrait = 1, orientLandscape = 2`).
extern "C" fn window_display_orientation_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, 2);
    0
}

/// `Window.displayRotate` — `0` (reference `TVPWindow.h:137`).
extern "C" fn window_display_rotate_get(
    _engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, 0);
    0
}

/// Apply one `registerMessageReceiver` call: mode `0` (`wrmRegister`) pushes
/// the `(proc, userdata)` pair, mode `1` (`wrmUnregister`) removes the first
/// matching pair, and any other mode is a no-op (reference `WindowImpl.h:20`,
/// `WindowImpl.cpp:2150-2163`).
fn apply_message_receiver_mode(
    receivers: &mut Vec<(i64, i64)>,
    mode: i64,
    proc: i64,
    userdata: i64,
) {
    match mode {
        0 => receivers.push((proc, userdata)),
        1 => {
            if let Some(index) = receivers
                .iter()
                .position(|entry| *entry == (proc, userdata))
            {
                receivers.remove(index);
            }
        }
        _ => {}
    }
}

/// `Window.registerMessageReceiver(mode, recv, handler)` — record the
/// registration (reference `WindowImpl.cpp:2150-2163`, which forwards to the
/// platform form, `WindowImpl.cpp:1461-1467`). Fewer than three arguments
/// raises; a bound context never raises.
extern "C" fn window_register_message_receiver(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(
            out_error,
            "Window.registerMessageReceiver requires 3 arguments",
        );
    }
    // SAFETY: the C++ trampoline passes `argc` live `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let mode = value_as_i64(&a[0]);
    let proc = value_as_i64(&a[1]);
    let userdata = value_as_i64(&a[2]);
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    apply_message_receiver_mode(&mut inst.message_receivers, mode, proc, userdata);
    set_void(out);
    0
}

/// `Window.findFullScreenCandidates(...)` — the reference validates five
/// arguments and otherwise returns `TJS_S_OK` with no result; its body is
/// disabled (`#if 0`) (reference `WindowImpl.cpp:2129-2148`).
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
        return report_error(
            out_error,
            "Window.findFullScreenCandidates requires 5 arguments",
        );
    }
    set_void(out);
    0
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    // SAFETY: the C++ property dispatcher passes a live `Value` for the duration of this call.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM owns it and keeps it alive and unaliased for the duration of this callback.
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
    // SAFETY: `instance` is the `WindowInst` boxed by `window_create`; the VM dispatches one callback at a time on it, and this borrow ends before any call that could re-enter the VM.
    let inst = unsafe { &mut *(instance as *mut WindowInst) };
    // SAFETY: the C++ property dispatcher passes a live `Value` for the duration of this call.
    inst.touch_rotate_threshold = value_as_f64(unsafe { &*value });
    0
}

/// Drain the queued `Window.bringToFront` requests (`(window_id)`).
pub fn take_bring_to_front_requests(services: &crate::CompatServices) -> Vec<u32> {
    std::mem::take(&mut services.state::<VisualState>().bring_to_front)
}

/// Drain the queued `Window.update` requests (`(window_id, update_type)`).
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
                name: "beginMove",
                f: window_begin_move,
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
                name: "setLayerPos",
                f: window_set_layer_pos,
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
                f: window_on_activate,
            },
            NativeInstanceMethodDef {
                name: "onClick",
                f: window_on_click,
            },
            NativeInstanceMethodDef {
                name: "onCloseQuery",
                f: window_noop_handler,
            },
            NativeInstanceMethodDef {
                name: "onDeactivate",
                f: window_on_deactivate,
            },
            NativeInstanceMethodDef {
                name: "onDisplayRotate",
                f: window_on_display_rotate,
            },
            NativeInstanceMethodDef {
                name: "onDoubleClick",
                f: window_on_double_click,
            },
            NativeInstanceMethodDef {
                name: "onFileDrop",
                f: window_on_file_drop,
            },
            NativeInstanceMethodDef {
                name: "onKeyDown",
                f: window_on_key_down,
            },
            NativeInstanceMethodDef {
                name: "onKeyPress",
                f: window_on_key_press,
            },
            NativeInstanceMethodDef {
                name: "onKeyUp",
                f: window_on_key_up,
            },
            NativeInstanceMethodDef {
                name: "onMouseDown",
                f: window_on_mouse_down,
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
                name: "onMouseMove",
                f: window_on_mouse_move,
            },
            NativeInstanceMethodDef {
                name: "onMouseUp",
                f: window_on_mouse_up,
            },
            NativeInstanceMethodDef {
                name: "onMouseWheel",
                f: window_on_mouse_wheel,
            },
            NativeInstanceMethodDef {
                name: "onMultiTouch",
                f: window_on_multi_touch,
            },
            NativeInstanceMethodDef {
                name: "onPopupHide",
                f: window_on_popup_hide,
            },
            NativeInstanceMethodDef {
                name: "onResize",
                f: window_on_resize,
            },
            NativeInstanceMethodDef {
                name: "onTouchDown",
                f: window_on_touch_down,
            },
            NativeInstanceMethodDef {
                name: "onTouchMove",
                f: window_on_touch_move,
            },
            NativeInstanceMethodDef {
                name: "onTouchRotate",
                f: window_on_touch_rotate,
            },
            NativeInstanceMethodDef {
                name: "onTouchScaling",
                f: window_on_touch_scaling,
            },
            NativeInstanceMethodDef {
                name: "onTouchUp",
                f: window_on_touch_up,
            },
        ],
        properties: vec![
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
                name: "layerLeft",
                get: Some(window_layer_left_get),
                set: Some(window_ignored_prop_set),
            },
            NativeInstancePropertyDef {
                name: "layerTop",
                get: Some(window_layer_top_get),
                set: Some(window_ignored_prop_set),
            },
            NativeInstancePropertyDef {
                name: "innerSunken",
                get: Some(window_inner_sunken_get),
                set: Some(window_ignored_prop_set),
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
                name: "menu",
                get: Some(window_menu_get),
                // Reference `WindowMenuProperty::PropSet` returns
                // `TJS_E_ACCESSDENYED` (`impl/MenuItemImpl.cpp:84-91`); a
                // read-only property registration produces the same error.
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
                name: "showScrollBars",
                get: Some(window_show_scroll_bars_get),
                set: Some(window_ignored_prop_set),
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
    })?;
    // `mainWindow` is a static member of the reference `Window` class, so it
    // is attached to the class object rather than copied onto instances.
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "Window",
        methods: Vec::new(),
        properties: vec![NativePropertyDef {
            name: "mainWindow",
            get: Some(window_main_window_get),
            set: None,
        }],
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{WindowInst, apply_message_receiver_mode};

    #[test]
    fn message_receiver_modes_update_the_list() {
        let mut inst = WindowInst::default();
        // Mode 0 (`wrmRegister`) appends the `(proc, userdata)` pair.
        apply_message_receiver_mode(&mut inst.message_receivers, 0, 1, 2);
        apply_message_receiver_mode(&mut inst.message_receivers, 0, 3, 4);
        assert_eq!(inst.message_receivers, vec![(1, 2), (3, 4)]);

        // Mode 1 (`wrmUnregister`) removes the first matching pair.
        apply_message_receiver_mode(&mut inst.message_receivers, 1, 1, 2);
        assert_eq!(inst.message_receivers, vec![(3, 4)]);

        // Any other mode leaves the list unchanged.
        apply_message_receiver_mode(&mut inst.message_receivers, 9, 5, 6);
        assert_eq!(inst.message_receivers, vec![(3, 4)]);
    }
}
