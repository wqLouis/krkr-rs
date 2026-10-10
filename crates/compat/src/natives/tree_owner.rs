//! `BitmapLayerTreeOwner` — the layer-tree event owner.
//!
//! Behavioral reference: `reference/cpp/core/visual/BitmapLayerTreeOwner.cpp`
//! and its base `tTVPLayerTreeOwner` (`LayerTreeOwnerImpl.cpp`). The reference
//! class is the draw target for a layer tree: it owns an internal `Bitmap`,
//! keeps a focused/primary layer, and turns host/input events into a dispatch
//! on the layer tree (`Fire*`) while the tree notifies it back with `On*`
//! hooks.
//!
//! # The layered model
//!
//! There is no separate layer manager in the L2 scene: the state lives in
//! [`engine::Scene`] and the generic input lives in
//! [`engine::input::InputState`]. This adapter therefore:
//!
//! * reads the tree bounds from the LTO's primary layer (falling back to the
//!   scene window size) and the focused layer object from its own retained
//!   handle (the setter also pushes the id into the L2 scene, so the scene
//!   stays the single source of truth for `Scene::set_focus`);
//! * translates each `fire*` call into a **runtime** member lookup on the
//!   focused layer (`onClick`, `onKeyDown`, …) and invokes it. The handler is
//!   resolved by name at dispatch time, so it does not depend on `Layer`'s
//!   implementation existing at compile time; the event is reported as
//!   consumed only when the handler returns a truthy value (the port's no-op
//!   defaults return `void`, so an unimplemented event is not consumed);
//! * registers the `on*` hooks as assignable no-op methods whose default is
//!   callable, so a script subclass or host can override them and a direct
//!   call never raises. This is the same shape the port's `Layer` event
//!   handlers use; the reference declares them as overridable native methods.
//!
//! The `fire*` argument lists mirror the reference; a missing argument is
//! filled from the attached L2 input state (pointer, wheel, shift flags, last
//! key, text input), which is the port's version of the OS-supplied payload.
//!
//! # Deliberately deferred
//!
//! * `primaryLayer` is the reference-property but its setter is denied there.
//!   The port exposes an assignable setter because the L2 id→script-object
//!   registry lives in a private sibling module that this file cannot name;
//!   without it the getter could never return a scene layer. When it is unset
//!   the getter returns TJS `null`.
//! * The owner-notification `on*` hooks that the reference posts from layer
//!   resize/image/attention/IME/cursor/hint changes are assignable no-ops
//!   here: those layer-manager notifications are not part of the L2 service
//!   set yet. `fireReleaseCapture` does consult `onReleaseMouseCapture`,
//!   which *is* reachable through the reference's release path.
//! * `fireDisplayRotate` / `fireRecheckInputState` have no layer handler in
//!   the reference either; they resolve to a runtime lookup and report
//!   "not consumed".
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue, VAL_OBJECT, VAL_REAL, VAL_RETAINED, VAL_VOID,
    Value,
};

use super::input::{MB_LEFT, MOUSE_BUTTONS, mouse_button, shift_flags};
use crate::native_util::{args, report_error, set_bool, set_int, set_void};
use crate::natives::system::{resolve, value_as_i64};
use crate::{CompatCtx, CompatError};

/// The `on*` owner hooks, in reference order (test/documentation copy of the
/// explicit registrations in [`tree_owner_methods`]).
#[cfg(test)]
const HOOK_NAMES: [&str; 11] = [
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
];

// ---------------------------------------------------------------------------
// Per-object payload
// ---------------------------------------------------------------------------

/// Per-object `BitmapLayerTreeOwner` payload.
///
/// Every field is an explicit retained script object (or scalar) owned by the
/// payload; dropping the payload releases the retentions. The L2 scene owns
/// the actual layer/bitmap state — this struct only holds the handles the
/// owner needs to answer its properties and dispatch events.
#[derive(Default)]
struct TreeOwnerInst {
    /// The L2 window the owner was attached to at construction (the scene's
    /// first window), used for the tree bounds fallback.
    window: Option<u32>,
    /// The focused layer object (`null`/unset = none).
    focused: Option<DetachedValue>,
    /// The scene id of [`Self::focused`], used to clear the L2 focus.
    focused_id: Option<u32>,
    /// The primary layer object (`null`/unset = none).
    primary: Option<DetachedValue>,
    /// Lazily created internal render-target bitmap.
    bitmap: Option<DetachedValue>,
}

impl TreeOwnerInst {
    /// The event-dispatch target: the focused layer, falling back to the
    /// primary layer (the reference dispatches to the focused layer and, for
    /// some events, the primary layer when no focus exists).
    fn target(&self) -> Option<&DetachedValue> {
        self.focused.as_ref().or(self.primary.as_ref())
    }
}

// ---------------------------------------------------------------------------
// Value marshalling helpers
// ---------------------------------------------------------------------------

/// A snapshot of the L2 input state used to fill missing `fire*` arguments.
///
/// The snapshot copies scalars out of the attached [`InputState`] so no
/// borrow is held across a VM call.
struct InputSnapshot {
    pointer: (f64, f64),
    flags: i64,
    wheel: f64,
    last_key: i64,
    text: String,
    button: i64,
}

/// Copy the attached L2 input state, if one is attached.
fn input_snapshot(ctx: &CompatCtx) -> Option<InputSnapshot> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine and the
    // host keeps the attached input state alive for the binding's lifetime.
    let ptr = unsafe { ctx.services() }.input()?;
    // SAFETY: see above.
    let state = unsafe { &*ptr };
    let (x, y) = state.pointer();
    let button = (0..MOUSE_BUTTONS)
        .find(|&index| mouse_button(index).is_some_and(|b| state.is_button_down(b)))
        .unwrap_or(MB_LEFT);
    Some(InputSnapshot {
        pointer: (f64::from(x), f64::from(y)),
        flags: i64::from(shift_flags(state)),
        wheel: f64::from(state.wheel()),
        last_key: i64::from(state.last_pressed().last().copied().unwrap_or(0)),
        text: state.text_input().to_string(),
        button: button as i64,
    })
}

/// The `i`-th argument as an integer, or `None` for a missing/void slot.
fn arg_i64(argv: &[Value], index: usize) -> Option<i64> {
    argv.get(index)
        .filter(|v| v.ty != VAL_VOID)
        .map(value_as_i64)
}

/// The `i`-th argument as a real, or `None` for a missing/void slot.
fn arg_f64(argv: &[Value], index: usize) -> Option<f64> {
    argv.get(index).filter(|v| v.ty != VAL_VOID).map(|v| {
        if v.ty == VAL_REAL {
            v.real
        } else {
            value_as_i64(v) as f64
        }
    })
}

/// TJS `operator bool` for a fired event's result: it reports whether the
/// handler consumed the event. The no-op defaults return `void` and therefore
/// do not consume; a handler opts in by returning a truthy value.
fn is_truthy(value: &TjsValue) -> bool {
    match value {
        TjsValue::Void => false,
        TjsValue::Integer(v) => *v != 0,
        TjsValue::Real(v) => *v != 0.0,
        TjsValue::String(v) => !v.is_empty(),
        TjsValue::Object | TjsValue::Retained(_) => true,
    }
}

/// Write a retained id into a native output slot (the C++ trampoline consumes
/// the retention).
fn write_retained(out: *mut Value, id: Tjs2ValueId) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = id as usize;
    }
}

/// Duplicate `id` and write it into `out`.
fn emit_retained(host: &Tjs2Engine, out: *mut Value, id: Tjs2ValueId) {
    if out.is_null() {
        return;
    }
    match host.duplicate_retained(id) {
        Ok(duplicate) => {
            write_retained(out, duplicate.raw_id());
            // The C++ conversion consumed the retention.
            std::mem::forget(duplicate);
        }
        Err(error) => {
            log::warn!("BitmapLayerTreeOwner: could not duplicate a retained object: {error}");
            set_void(out);
        }
    }
}

/// Write TJS `null` into `out`.
fn set_null(host: &Tjs2Engine, out: *mut Value) {
    if out.is_null() {
        return;
    }
    if let Ok(RetainedValue::Object(object)) =
        host.eval_retained("null", "BitmapLayerTreeOwner.null")
    {
        write_retained(out, object.raw_id());
        std::mem::forget(object);
        return;
    }
    set_void(out);
}

/// Evaluate an expression and retain an object result.
fn eval_object(host: &Tjs2Engine, expression: &str, what: &str) -> Option<DetachedValue> {
    match host.eval_retained(expression, what) {
        Ok(RetainedValue::Object(object)) => Some(object),
        Ok(_) => None,
        Err(error) => {
            log::warn!("{what}: evaluation failed: {error}");
            None
        }
    }
}

/// Read a numeric member of a retained object.
fn member_i64(host: &Tjs2Engine, id: Tjs2ValueId, member: &str) -> Option<i64> {
    match host.member(id, member) {
        Ok(TjsValue::Integer(v)) => Some(v),
        Ok(TjsValue::Real(v)) => Some(v as i64),
        _ => None,
    }
}

/// The scene id of a retained object as a native `Layer`, or `Ok(None)` when
/// it is not a `Layer`.
fn scene_id_of(host: &Tjs2Engine, object: Tjs2ValueId) -> Result<Option<u32>, String> {
    CompatCtx::native_instance_id(host, object, &["Layer"])
}

/// The attached scene's window size, if the window exists.
fn window_size(ctx: &CompatCtx, window: u32) -> Option<(i64, i64)> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine and the
    // host keeps the attached scene alive for the binding's lifetime.
    let ptr = unsafe { ctx.services() }.scene()?;
    // SAFETY: see above.
    let scene = unsafe { &*ptr };
    let state = scene.window(window)?;
    Some((i64::from(state.inner_size.0), i64::from(state.inner_size.1)))
}

/// The owner's tree bounds: the primary layer's size when one is attached,
/// otherwise the attached window's inner size.
fn tree_size(host: &Tjs2Engine, instance: *mut c_void, ctx: &CompatCtx) -> (i64, i64) {
    // SAFETY: `instance` is the live payload the VM passed to the callback.
    let (primary, window) = unsafe {
        let inst = &*(instance as *const TreeOwnerInst);
        (
            inst.primary.as_ref().map(DetachedValue::raw_id),
            inst.window,
        )
    };
    if let Some(raw) = primary
        && let Ok(object) = host.duplicate_retained(raw)
    {
        let width = member_i64(host, object.raw_id(), "width");
        let height = member_i64(host, object.raw_id(), "height");
        if let (Some(width), Some(height)) = (width, height) {
            return (width, height);
        }
    }
    window
        .and_then(|window| window_size(ctx, window))
        .unwrap_or((0, 0))
}

// ---------------------------------------------------------------------------
// Event dispatch
// ---------------------------------------------------------------------------

/// Dispatch `handler` on the owner's focused (or primary) layer and return
/// whether it consumed the event.
///
/// The target is duplicated before the call so a handler that re-enters the
/// owner (e.g. clears `focusedLayer`) cannot invalidate the object mid-call.
fn dispatch_event(
    host: &Tjs2Engine,
    instance: *mut c_void,
    handler: &str,
    args: &[TjsValue],
) -> bool {
    // SAFETY: `instance` is the live payload the VM passed to the callback.
    let target = unsafe { &*(instance as *const TreeOwnerInst) }
        .target()
        .map(DetachedValue::raw_id);
    let Some(target) = target else {
        return false;
    };
    let Ok(object) = host.duplicate_retained(target) else {
        return false;
    };
    match host.call_member(object.raw_id(), handler, args) {
        Ok(value) => is_truthy(&value),
        Err(_) => false,
    }
}

/// Invoke an owner hook on the LTO script object (side-effect only).
fn notify_owner(host: &Tjs2Engine, objthis: *mut c_void, handler: &str) -> bool {
    if objthis.is_null() {
        return false;
    }
    let Ok(owner) = host.retain_object_detached(objthis) else {
        return false;
    };
    host.call_member(owner.raw_id(), handler, &[]).is_ok()
}

// ---------------------------------------------------------------------------
// Payload builders (missing arguments fall back to the L2 input state)
// ---------------------------------------------------------------------------

fn pointer_x(snapshot: Option<&InputSnapshot>) -> i64 {
    snapshot.map_or(0, |s| s.pointer.0.round() as i64)
}

fn pointer_y(snapshot: Option<&InputSnapshot>) -> i64 {
    snapshot.map_or(0, |s| s.pointer.1.round() as i64)
}

fn payload_xy(argv: &[Value], snapshot: Option<&InputSnapshot>) -> Vec<TjsValue> {
    vec![
        TjsValue::Integer(arg_i64(argv, 0).unwrap_or_else(|| pointer_x(snapshot))),
        TjsValue::Integer(arg_i64(argv, 1).unwrap_or_else(|| pointer_y(snapshot))),
    ]
}

fn payload_mouse(argv: &[Value], snapshot: Option<&InputSnapshot>) -> Vec<TjsValue> {
    let button = arg_i64(argv, 2)
        .or_else(|| snapshot.map(|s| s.button))
        .unwrap_or(MB_LEFT as i64);
    let flags = arg_i64(argv, 3)
        .or_else(|| snapshot.map(|s| s.flags))
        .unwrap_or(0);
    vec![
        TjsValue::Integer(arg_i64(argv, 0).unwrap_or_else(|| pointer_x(snapshot))),
        TjsValue::Integer(arg_i64(argv, 1).unwrap_or_else(|| pointer_y(snapshot))),
        TjsValue::Integer(button),
        TjsValue::Integer(flags),
    ]
}

fn payload_move(argv: &[Value], snapshot: Option<&InputSnapshot>) -> Vec<TjsValue> {
    let flags = arg_i64(argv, 2)
        .or_else(|| snapshot.map(|s| s.flags))
        .unwrap_or(0);
    vec![
        TjsValue::Integer(arg_i64(argv, 0).unwrap_or_else(|| pointer_x(snapshot))),
        TjsValue::Integer(arg_i64(argv, 1).unwrap_or_else(|| pointer_y(snapshot))),
        TjsValue::Integer(flags),
    ]
}

fn payload_wheel(argv: &[Value], snapshot: Option<&InputSnapshot>) -> Vec<TjsValue> {
    let shift = arg_i64(argv, 0)
        .or_else(|| snapshot.map(|s| s.flags))
        .unwrap_or(0);
    let delta = arg_i64(argv, 1)
        .or_else(|| snapshot.map(|s| s.wheel.round() as i64))
        .unwrap_or(0);
    vec![
        TjsValue::Integer(shift),
        TjsValue::Integer(delta),
        TjsValue::Integer(arg_i64(argv, 2).unwrap_or_else(|| pointer_x(snapshot))),
        TjsValue::Integer(arg_i64(argv, 3).unwrap_or_else(|| pointer_y(snapshot))),
    ]
}

fn payload_empty(_argv: &[Value], _snapshot: Option<&InputSnapshot>) -> Vec<TjsValue> {
    Vec::new()
}

fn payload_touch(argv: &[Value], snapshot: Option<&InputSnapshot>) -> Vec<TjsValue> {
    let x = arg_f64(argv, 0).unwrap_or_else(|| snapshot.map_or(0.0, |s| s.pointer.0));
    let y = arg_f64(argv, 1).unwrap_or_else(|| snapshot.map_or(0.0, |s| s.pointer.1));
    vec![
        TjsValue::Real(x),
        TjsValue::Real(y),
        TjsValue::Real(arg_f64(argv, 2).unwrap_or(0.0)),
        TjsValue::Real(arg_f64(argv, 3).unwrap_or(0.0)),
        TjsValue::Integer(arg_i64(argv, 4).unwrap_or(0)),
    ]
}

fn payload_touch_scaling(argv: &[Value], _snapshot: Option<&InputSnapshot>) -> Vec<TjsValue> {
    vec![
        TjsValue::Real(arg_f64(argv, 0).unwrap_or(0.0)),
        TjsValue::Real(arg_f64(argv, 1).unwrap_or(0.0)),
        TjsValue::Real(arg_f64(argv, 2).unwrap_or(0.0)),
        TjsValue::Real(arg_f64(argv, 3).unwrap_or(0.0)),
        TjsValue::Integer(arg_i64(argv, 4).unwrap_or(0)),
    ]
}

fn payload_touch_rotate(argv: &[Value], _snapshot: Option<&InputSnapshot>) -> Vec<TjsValue> {
    vec![
        TjsValue::Real(arg_f64(argv, 0).unwrap_or(0.0)),
        TjsValue::Real(arg_f64(argv, 1).unwrap_or(0.0)),
        TjsValue::Real(arg_f64(argv, 2).unwrap_or(0.0)),
        TjsValue::Real(arg_f64(argv, 3).unwrap_or(0.0)),
        TjsValue::Real(arg_f64(argv, 4).unwrap_or(0.0)),
        TjsValue::Integer(arg_i64(argv, 5).unwrap_or(0)),
    ]
}

fn payload_key2(argv: &[Value], snapshot: Option<&InputSnapshot>) -> Vec<TjsValue> {
    let key = arg_i64(argv, 0)
        .or_else(|| snapshot.map(|s| s.last_key))
        .unwrap_or(0);
    let shift = arg_i64(argv, 1)
        .or_else(|| snapshot.map(|s| s.flags))
        .unwrap_or(0);
    vec![TjsValue::Integer(key), TjsValue::Integer(shift)]
}

fn payload_key1(argv: &[Value], snapshot: Option<&InputSnapshot>) -> Vec<TjsValue> {
    let key = arg_i64(argv, 0)
        .or_else(|| snapshot.and_then(|s| s.text.chars().next().map(|c| i64::from(u32::from(c)))))
        .unwrap_or(0);
    vec![TjsValue::Integer(key)]
}

fn payload_display(argv: &[Value], _snapshot: Option<&InputSnapshot>) -> Vec<TjsValue> {
    (0..5)
        .map(|index| TjsValue::Integer(arg_i64(argv, index).unwrap_or(0)))
        .collect()
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

extern "C" fn tree_owner_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(TreeOwnerInst::default())) as *mut c_void
}

extern "C" fn tree_owner_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `tree_owner_create` and is released once.
    drop(unsafe { Box::from_raw(instance as *mut TreeOwnerInst) });
}

/// `new BitmapLayerTreeOwner()` — bind the owner to the scene's first window.
extern "C" fn tree_owner_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    let window = {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine and
        // the host keeps the attached scene alive for the binding's lifetime.
        let ptr = unsafe { ctx.services() }.scene();
        // SAFETY: see above.
        ptr.and_then(|ptr| unsafe { &*ptr }.windows.first().map(|w| w.id))
    };
    // SAFETY: `instance` is the live payload the VM passed to the constructor.
    unsafe { &mut *(instance as *mut TreeOwnerInst) }.window = window;
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Owner hooks (`on*` assignable no-op methods)
// ---------------------------------------------------------------------------

/// The default implementation of every owner hook: a callable no-op.
///
/// A script override (`owner.onSetMouseCursor = function …`) shadows the
/// method on the instance; a host dispatches it by name. This mirrors how the
/// reference declares the `On*` notifications and how the port's `Layer`
/// event handlers are registered.
extern "C" fn hook_noop(
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

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// `bitmap` — the lazily created internal render target (`new Bitmap(0, 0)`).
extern "C" fn bitmap_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner.bitmap: compat context is not bound",
        );
    };
    // SAFETY: `instance` is the live payload the VM passed to the callback.
    let stored = unsafe { &*(instance as *const TreeOwnerInst) }
        .bitmap
        .as_ref()
        .map(DetachedValue::raw_id);
    if let Some(id) = stored {
        emit_retained(resolved.engine(), out, id);
        return 0;
    }
    let Some(created) = eval_object(
        resolved.engine(),
        "new Bitmap(0, 0)",
        "BitmapLayerTreeOwner.bitmap",
    ) else {
        set_void(out);
        return 0;
    };
    // SAFETY: `instance` is the live payload the VM passed to the callback.
    {
        let inst = unsafe { &mut *(instance as *mut TreeOwnerInst) };
        if inst.bitmap.is_none() {
            inst.bitmap = Some(created);
        }
    }
    // SAFETY: see above.
    let stored = unsafe { &*(instance as *const TreeOwnerInst) }
        .bitmap
        .as_ref()
        .map(DetachedValue::raw_id);
    match stored {
        Some(id) => emit_retained(resolved.engine(), out, id),
        None => set_void(out),
    }
    0
}

/// `width` — the tree bounds' width.
extern "C" fn width_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner.width: compat context is not bound",
        );
    };
    let (width, _) = tree_size(resolved.engine(), instance, &resolved.ctx());
    set_int(out, width);
    0
}

/// `height` — the tree bounds' height.
extern "C" fn height_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner.height: compat context is not bound",
        );
    };
    let (_, height) = tree_size(resolved.engine(), instance, &resolved.ctx());
    set_int(out, height);
    0
}

/// `focusedLayer` getter — the focused layer object, or `null`.
extern "C" fn focused_layer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner.focusedLayer: compat context is not bound",
        );
    };
    // SAFETY: `instance` is the live payload the VM passed to the callback.
    let stored = unsafe { &*(instance as *const TreeOwnerInst) }
        .focused
        .as_ref()
        .map(DetachedValue::raw_id);
    match stored {
        Some(id) => emit_retained(resolved.engine(), out, id),
        None => set_null(resolved.engine(), out),
    }
    0
}

/// `focusedLayer = layer` — retain the layer and mirror the id into the L2
/// scene focus (`null`/void clears it).
extern "C" fn focused_layer_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner.focusedLayer: compat context is not bound",
        );
    };
    if value.is_null() {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner.focusedLayer: missing value",
        );
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let value = unsafe { &*value };
    let host = resolved.engine();
    let (retained, scene_id) = if value.ty == VAL_VOID {
        (None, None)
    } else if value.ty == VAL_OBJECT && !value.object_handle().is_null() {
        let object = match host.retain_object_arg(value) {
            Ok(object) => object,
            Err(error) => {
                return report_error(
                    out_error,
                    &format!("BitmapLayerTreeOwner.focusedLayer: {error}"),
                );
            }
        };
        let scene_id = match scene_id_of(host, object.raw_id()) {
            Ok(Some(id)) => Some(id),
            Ok(None) => {
                return report_error(
                    out_error,
                    "BitmapLayerTreeOwner.focusedLayer: the value is not a Layer",
                );
            }
            Err(error) => {
                return report_error(
                    out_error,
                    &format!("BitmapLayerTreeOwner.focusedLayer: {error}"),
                );
            }
        };
        (Some(object), scene_id)
    } else {
        (None, None)
    };
    if let Some(id) = scene_id {
        let ctx = resolved.ctx();
        // SAFETY: a `CompatCtx` is only created for a bound, live engine and
        // the host keeps the attached scene alive for the binding's lifetime.
        let scene = unsafe { ctx.services() }.scene();
        if let Some(ptr) = scene {
            // SAFETY: see above; no VM call is made while the borrow is held.
            unsafe { &mut *ptr }.set_focus(id);
        }
    } else {
        // Clearing: blur the layer that had focus in its own window.
        // SAFETY: `instance` is the live payload the VM passed to the callback.
        let previous = unsafe { &*(instance as *const TreeOwnerInst) }.focused_id;
        if let Some(previous) = previous {
            let ctx = resolved.ctx();
            // SAFETY: see above.
            let scene = unsafe { ctx.services() }.scene();
            if let Some(ptr) = scene {
                // SAFETY: see above.
                let scene = unsafe { &mut *ptr };
                if let Some(window) = scene.layer(previous).map(|layer| layer.window) {
                    scene.clear_focus(window);
                }
            }
        }
    }
    // SAFETY: `instance` is the live payload the VM passed to the callback.
    let inst = unsafe { &mut *(instance as *mut TreeOwnerInst) };
    inst.focused = retained;
    inst.focused_id = scene_id;
    0
}

/// `primaryLayer` getter — the primary layer object, or `null`.
extern "C" fn primary_layer_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner.primaryLayer: compat context is not bound",
        );
    };
    // SAFETY: `instance` is the live payload the VM passed to the callback.
    let stored = unsafe { &*(instance as *const TreeOwnerInst) }
        .primary
        .as_ref()
        .map(DetachedValue::raw_id);
    match stored {
        Some(id) => emit_retained(resolved.engine(), out, id),
        None => set_null(resolved.engine(), out),
    }
    0
}

/// `primaryLayer = layer` — retain the primary layer (`null`/void clears it).
///
/// See the module docs: the reference denies this setter; the port exposes it
/// because the L2 id→object registry is not reachable from this module.
extern "C" fn primary_layer_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner.primaryLayer: compat context is not bound",
        );
    };
    if value.is_null() {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner.primaryLayer: missing value",
        );
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let value = unsafe { &*value };
    let retained = if value.ty == VAL_OBJECT && !value.object_handle().is_null() {
        match resolved.engine().retain_object_arg(value) {
            Ok(object) => Some(object),
            Err(error) => {
                return report_error(
                    out_error,
                    &format!("BitmapLayerTreeOwner.primaryLayer: {error}"),
                );
            }
        }
    } else {
        None
    };
    // SAFETY: `instance` is the live payload the VM passed to the callback.
    unsafe { &mut *(instance as *mut TreeOwnerInst) }.primary = retained;
    0
}

/// `layerTreeOwnerInterface` — the owner script object itself.
extern "C" fn layer_tree_owner_interface_get(
    engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    if objthis.is_null() {
        set_void(out);
        return 0;
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner.layerTreeOwnerInterface: compat context is not bound",
        );
    };
    match resolved.engine().retain_object_detached(objthis) {
        Ok(owner) => {
            write_retained(out, owner.raw_id());
            // The C++ conversion consumed the retention.
            std::mem::forget(owner);
        }
        Err(error) => {
            return report_error(
                out_error,
                &format!("BitmapLayerTreeOwner.layerTreeOwnerInterface: {error}"),
            );
        }
    }
    0
}

// ---------------------------------------------------------------------------
// `fire*` events
// ---------------------------------------------------------------------------

/// Declare a `fire*` method that builds its payload and dispatches it.
macro_rules! fire_method {
    ($name:ident, $handler:literal, $payload:ident) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let Some(resolved) = resolve(engine) else {
                return report_error(
                    out_error,
                    concat!(
                        "BitmapLayerTreeOwner.",
                        $handler,
                        ": compat context is not bound"
                    ),
                );
            };
            // SAFETY: the trampoline guarantees `argc` valid values.
            let argv = unsafe { args(argv, argc) };
            let snapshot = input_snapshot(&resolved.ctx());
            let payload = $payload(argv, snapshot.as_ref());
            set_bool(
                out,
                dispatch_event(resolved.engine(), instance, $handler, &payload),
            );
            0
        }
    };
}

fire_method!(fire_click, "onClick", payload_xy);
fire_method!(fire_double_click, "onDoubleClick", payload_xy);
fire_method!(fire_mouse_down, "onMouseDown", payload_mouse);
fire_method!(fire_mouse_up, "onMouseUp", payload_mouse);
fire_method!(fire_mouse_move, "onMouseMove", payload_move);
fire_method!(fire_mouse_wheel, "onMouseWheel", payload_wheel);
fire_method!(fire_touch_down, "onTouchDown", payload_touch);
fire_method!(fire_touch_up, "onTouchUp", payload_touch);
fire_method!(fire_touch_move, "onTouchMove", payload_touch);
fire_method!(fire_touch_scaling, "onTouchScaling", payload_touch_scaling);
fire_method!(fire_touch_rotate, "onTouchRotate", payload_touch_rotate);
fire_method!(fire_multi_touch, "onMultiTouch", payload_empty);
fire_method!(fire_key_down, "onKeyDown", payload_key2);
fire_method!(fire_key_up, "onKeyUp", payload_key2);
fire_method!(fire_key_press, "onKeyPress", payload_key1);
fire_method!(fire_display_rotate, "onDisplayRotate", payload_display);
fire_method!(
    fire_recheck_input_state,
    "onRecheckInputState",
    payload_empty
);
fire_method!(fire_mouse_out_of_window, "onMouseLeave", payload_empty);

/// `fireReleaseCapture()` — dispatch `onReleaseCapture` on the focused layer
/// and notify the owner's `onReleaseMouseCapture` hook (the reference release
/// path).
extern "C" fn fire_release_capture(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "BitmapLayerTreeOwner.fireReleaseCapture: compat context is not bound",
        );
    };
    let consumed = dispatch_event(resolved.engine(), instance, "onReleaseCapture", &[]);
    notify_owner(resolved.engine(), objthis, "onReleaseMouseCapture");
    set_bool(out, consumed);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

fn tree_owner_methods() -> Vec<NativeInstanceMethodDef> {
    vec![
        NativeInstanceMethodDef {
            name: "BitmapLayerTreeOwner",
            f: tree_owner_ctor,
        },
        NativeInstanceMethodDef {
            name: "fireClick",
            f: fire_click,
        },
        NativeInstanceMethodDef {
            name: "fireDoubleClick",
            f: fire_double_click,
        },
        NativeInstanceMethodDef {
            name: "fireMouseDown",
            f: fire_mouse_down,
        },
        NativeInstanceMethodDef {
            name: "fireMouseUp",
            f: fire_mouse_up,
        },
        NativeInstanceMethodDef {
            name: "fireMouseMove",
            f: fire_mouse_move,
        },
        NativeInstanceMethodDef {
            name: "fireMouseWheel",
            f: fire_mouse_wheel,
        },
        NativeInstanceMethodDef {
            name: "fireMouseOutOfWindow",
            f: fire_mouse_out_of_window,
        },
        NativeInstanceMethodDef {
            name: "fireReleaseCapture",
            f: fire_release_capture,
        },
        NativeInstanceMethodDef {
            name: "fireTouchDown",
            f: fire_touch_down,
        },
        NativeInstanceMethodDef {
            name: "fireTouchUp",
            f: fire_touch_up,
        },
        NativeInstanceMethodDef {
            name: "fireTouchMove",
            f: fire_touch_move,
        },
        NativeInstanceMethodDef {
            name: "fireTouchScaling",
            f: fire_touch_scaling,
        },
        NativeInstanceMethodDef {
            name: "fireTouchRotate",
            f: fire_touch_rotate,
        },
        NativeInstanceMethodDef {
            name: "fireMultiTouch",
            f: fire_multi_touch,
        },
        NativeInstanceMethodDef {
            name: "fireKeyDown",
            f: fire_key_down,
        },
        NativeInstanceMethodDef {
            name: "fireKeyUp",
            f: fire_key_up,
        },
        NativeInstanceMethodDef {
            name: "fireKeyPress",
            f: fire_key_press,
        },
        NativeInstanceMethodDef {
            name: "fireDisplayRotate",
            f: fire_display_rotate,
        },
        NativeInstanceMethodDef {
            name: "fireRecheckInputState",
            f: fire_recheck_input_state,
        },
        // The owner hooks are assignable no-op methods: a script override
        // shadows them, and `fire*`/`notify_owner` resolve them by name at
        // dispatch time. Listed explicitly so the native-parity checker sees
        // every registered member.
        NativeInstanceMethodDef {
            name: "onChangeLayerImage",
            f: hook_noop,
        },
        NativeInstanceMethodDef {
            name: "onDisableAttentionPoint",
            f: hook_noop,
        },
        NativeInstanceMethodDef {
            name: "onGetCursorPos",
            f: hook_noop,
        },
        NativeInstanceMethodDef {
            name: "onReleaseMouseCapture",
            f: hook_noop,
        },
        NativeInstanceMethodDef {
            name: "onResetImeMode",
            f: hook_noop,
        },
        NativeInstanceMethodDef {
            name: "onResizeLayer",
            f: hook_noop,
        },
        NativeInstanceMethodDef {
            name: "onSetAttentionPoint",
            f: hook_noop,
        },
        NativeInstanceMethodDef {
            name: "onSetCursorPos",
            f: hook_noop,
        },
        NativeInstanceMethodDef {
            name: "onSetHintText",
            f: hook_noop,
        },
        NativeInstanceMethodDef {
            name: "onSetImeMode",
            f: hook_noop,
        },
        NativeInstanceMethodDef {
            name: "onSetMouseCursor",
            f: hook_noop,
        },
    ]
}

fn tree_owner_properties() -> Vec<NativeInstancePropertyDef> {
    vec![
        NativeInstancePropertyDef {
            name: "bitmap",
            get: Some(bitmap_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "width",
            get: Some(width_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "height",
            get: Some(height_get),
            set: None,
        },
        NativeInstancePropertyDef {
            name: "focusedLayer",
            get: Some(focused_layer_get),
            set: Some(focused_layer_set),
        },
        NativeInstancePropertyDef {
            name: "primaryLayer",
            get: Some(primary_layer_get),
            set: Some(primary_layer_set),
        },
        NativeInstancePropertyDef {
            name: "layerTreeOwnerInterface",
            get: Some(layer_tree_owner_interface_get),
            set: None,
        },
    ]
}

/// Register the `BitmapLayerTreeOwner` native class on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "BitmapLayerTreeOwner",
        create: tree_owner_create,
        destroy: tree_owner_destroy,
        invalidate: None,
        methods: tree_owner_methods(),
        properties: tree_owner_properties(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_default_to_the_input_snapshot() {
        let snapshot = InputSnapshot {
            pointer: (12.0, 34.0),
            flags: 0x09,
            wheel: -2.0,
            last_key: 0x41,
            text: "A".to_string(),
            button: 1,
        };
        assert_eq!(
            payload_xy(&[], Some(&snapshot)),
            vec![TjsValue::Integer(12), TjsValue::Integer(34)]
        );
        assert_eq!(
            payload_mouse(&[], Some(&snapshot)),
            vec![
                TjsValue::Integer(12),
                TjsValue::Integer(34),
                TjsValue::Integer(1),
                TjsValue::Integer(0x09)
            ]
        );
        assert_eq!(
            payload_wheel(&[], Some(&snapshot)),
            vec![
                TjsValue::Integer(0x09),
                TjsValue::Integer(-2),
                TjsValue::Integer(12),
                TjsValue::Integer(34)
            ]
        );
        assert_eq!(
            payload_key2(&[], Some(&snapshot)),
            vec![TjsValue::Integer(0x41), TjsValue::Integer(0x09)]
        );
        assert_eq!(
            payload_key1(&[], Some(&snapshot)),
            vec![TjsValue::Integer(i64::from('A' as u32))]
        );
    }

    #[test]
    fn explicit_arguments_win_over_the_snapshot() {
        let snapshot = InputSnapshot {
            pointer: (12.0, 34.0),
            flags: 0,
            wheel: 0.0,
            last_key: 0,
            text: String::new(),
            button: 0,
        };
        let argv = [ffi_value(7), ffi_value(9)];
        assert_eq!(
            payload_xy(&argv, Some(&snapshot)),
            vec![TjsValue::Integer(7), TjsValue::Integer(9)]
        );
    }

    #[test]
    fn truthiness_matches_tjs_operator_bool() {
        assert!(!is_truthy(&TjsValue::Void));
        assert!(!is_truthy(&TjsValue::Integer(0)));
        assert!(!is_truthy(&TjsValue::Real(0.0)));
        assert!(is_truthy(&TjsValue::Integer(1)));
        assert!(is_truthy(&TjsValue::Real(-0.5)));
        assert!(!is_truthy(&TjsValue::String(String::new())));
        assert!(is_truthy(&TjsValue::String("x".to_string())));
    }

    #[test]
    fn hook_names_match_the_reference_surface() {
        assert_eq!(HOOK_NAMES.len(), 11);
        assert_eq!(HOOK_NAMES[3], "onReleaseMouseCapture");
        assert_eq!(HOOK_NAMES[10], "onSetMouseCursor");
    }

    /// A minimal integer [`Value`] for the payload unit tests.
    fn ffi_value(integer: i64) -> Value {
        Value {
            ty: tjs::VAL_INTEGER,
            integer,
            real: 0.0,
            string: std::ptr::null(),
            array: std::ptr::null(),
            array_count: 0,
            retained: 0,
        }
    }
}
