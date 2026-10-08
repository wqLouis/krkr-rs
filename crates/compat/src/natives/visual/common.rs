//! Shared helpers, dispatch state and value utilities for the visual natives.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void};

use engine::Scene;
use engine::input::InputState;
use engine::timer::TimerId;
use platform::Clock;
use tjs::{
    DetachedValue, RetainedValue, Tjs2Engine, TjsValue, VAL_INTEGER, VAL_OBJECT, VAL_REAL,
    VAL_RETAINED, Value,
};

use crate::CompatCtx;
use crate::native_util::set_void;
use crate::natives::system::resolve;

/// Default font face used when a `Font` is created without a face (the
/// reference `TVPDefaultFontFaceName`).
pub(crate) const DEFAULT_FONT_FACE: &str = "MS Gothic";
/// Default font height (`TVPDefaultFontHeight`).
pub(crate) const DEFAULT_FONT_HEIGHT: i32 = 16;
/// Default font color (opaque white, ARGB).
pub(crate) const DEFAULT_FONT_COLOR: i64 = 0xffff_ffff;

// ---------------------------------------------------------------------------
// Per-family dispatch state (never a process global)
// ---------------------------------------------------------------------------

/// Scene id → the retained script object (`objthis`) that wraps it. This is
/// the one cross-object mapping the visual natives need: it lets
/// `Window.primaryLayer`, `Layer.parent`, `Layer.window`, `Layer.children`
/// and `Layer.getLayerAt` hand back the *real* object a script created.
#[derive(Default)]
pub(crate) struct ObjectRegistry {
    pub(crate) windows: HashMap<u32, DetachedValue>,
    pub(crate) layers: HashMap<u32, DetachedValue>,
    pub(crate) bitmaps: HashMap<u32, DetachedValue>,
    pub(crate) fonts: HashMap<u32, DetachedValue>,
}

/// One registered `Timer`'s L3 payload: the retained callback/owner and the
/// TVP dispatch mode. The scheduling state (interval, enabled, fire count)
/// lives in the L2 [`TimerState`] registry.
pub(crate) struct TimerBinding {
    /// The constructor callback (a plain `Timer`'s `onTimer` invokes it).
    /// `None` while the callback is running (taken out so the state borrow is
    /// released before re-entering the VM).
    pub(crate) callback: Option<DetachedValue>,
    /// The timer object itself, for `onTimer` dispatch through its class
    /// chain (a script subclass override wins).
    pub(crate) owner: Option<DetachedValue>,
    /// Event delivery mode (`tTVPAsyncTriggerMode`), TVP-specific and
    /// distinct from the L2 one-shot/repeat firing mode.
    pub(crate) mode: i64,
}

/// `VisualState` — the visual family's dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
#[derive(Default)]
pub(crate) struct VisualState {
    pub(crate) objects: ObjectRegistry,
    /// L3 payload per L2 timer id.
    pub(crate) timers: HashMap<TimerId, TimerBinding>,
    /// The L1 monotonic clock that supplies `now_ms` to the L2 timer
    /// registry.
    pub(crate) clock: Clock,
    /// `Window.bringToFront` requests for the host to drain.
    pub(crate) bring_to_front: Vec<u32>,
    /// `Window.update` requests `(window_id, update_type)`.
    pub(crate) update_requests: Vec<(u32, i32)>,
    /// Reusable `out.string` scratch. The C++ trampoline copies the string
    /// before the callback returns, so the buffer only has to outlive the
    /// call. (System owns a shared `StringScratch`; the visual property
    /// setters need their own so a re-entrant visual callback cannot clobber
    /// a System result mid-marshal.)
    pub(crate) scratch: RefCell<Vec<u8>>,
}

/// Resolve the bound engine for a callback and return its dispatch context.
pub(crate) fn bound(
    engine: *mut c_void,
) -> Result<(crate::natives::system::EngineBinding, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "visual: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved, ctx))
}

/// The attached L2 scene, mutably.
pub(crate) fn scene_mut<'a>(ctx: &CompatCtx) -> Result<&'a mut Scene, String> {
    // SAFETY: the host keeps the attached scene alive for the binding's
    // lifetime and the VM is single-threaded (the documented `attach_scene`
    // contract), so forming this reference is sound within the callback.
    let ptr = unsafe { ctx.services() }
        .scene()
        .ok_or_else(|| "visual: no scene attached".to_string())?;
    // SAFETY: see above.
    Ok(unsafe { &mut *ptr })
}

/// The attached L2 scene, shared.
pub(crate) fn scene_ref<'a>(ctx: &CompatCtx) -> Result<&'a Scene, String> {
    // SAFETY: see [`scene_mut`].
    let ptr = unsafe { ctx.services() }
        .scene()
        .ok_or_else(|| "visual: no scene attached".to_string())?;
    // SAFETY: see [`scene_mut`].
    Ok(unsafe { &*ptr })
}

/// The attached L2 generic input state, shared.
pub(crate) fn input_ref<'a>(ctx: &CompatCtx) -> Result<&'a InputState, String> {
    // SAFETY: the host keeps the attached input state alive for the binding's
    // lifetime and the VM is single-threaded (the documented `attach_input`
    // contract), so forming this reference is sound within the callback.
    let ptr = unsafe { ctx.services() }
        .input()
        .ok_or_else(|| "visual: no input state attached".to_string())?;
    // SAFETY: see above.
    Ok(unsafe { &*ptr })
}

/// Write a retained script object into `out` (the C++ trampoline consumes the
/// retention). Falls back to `void` on the (unexpected) duplication failure.
pub(crate) fn set_retained_out(_host: &Tjs2Engine, out: *mut Value, id: tjs::Tjs2ValueId) {
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

/// Duplicate a registry object into `out`; `None` writes TJS `null`.
pub(crate) fn set_object_out(host: &Tjs2Engine, out: *mut Value, object: Option<&DetachedValue>) {
    let Some(object) = object else {
        set_null_out(host, out);
        return;
    };
    match host.duplicate_retained(object.raw_id()) {
        Ok(duplicate) => {
            set_retained_out(host, out, duplicate.raw_id());
            // The C++ conversion consumed the retention; the `DetachedValue`
            // drop is a safe no-op.
            std::mem::forget(duplicate);
        }
        Err(error) => {
            log::warn!("visual: could not duplicate a retained object: {error}");
            set_void(out);
        }
    }
}

/// Write TJS `null` into `out`. The C ABI has no null literal, so the `null`
/// expression is evaluated and its object variant retained (as the reference
/// native adapter does). Falls back to `void`, which is still falsy.
pub(crate) fn set_null_out(host: &Tjs2Engine, out: *mut Value) {
    if out.is_null() {
        return;
    }
    if let Ok(RetainedValue::Object(object)) = host.eval_retained("null", "visual.null") {
        set_retained_out(host, out, object.raw_id());
        std::mem::forget(object);
        return;
    }
    set_void(out);
}

/// Write a real result into `out`.
pub(crate) fn set_real(out: *mut Value, value: f64) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_REAL;
        (*out).integer = 0;
        (*out).real = value;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Write a string result into `out` using the visual family's scratch buffer.
pub(crate) fn set_visual_string_out(ctx: &CompatCtx, out: *mut Value, text: &str) {
    if out.is_null() {
        return;
    }
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let state = services.state::<VisualState>();
    let mut scratch = state.scratch.borrow_mut();
    scratch.clear();
    scratch.extend_from_slice(text.as_bytes());
    scratch.push(0);
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = tjs::VAL_STRING;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = scratch.as_ptr() as *const c_char;
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// ARGB (`0xAARRGGBB`) → straight-alpha RGBA.
pub(crate) fn argb_to_rgba(color: i64) -> [u8; 4] {
    let value = color as u32;
    [
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    ]
}

/// Straight-alpha RGBA → ARGB (`0xAARRGGBB`).
pub(crate) fn rgba_to_argb(color: [u8; 4]) -> i64 {
    (i64::from(color[3]) << 24)
        | (i64::from(color[0]) << 16)
        | (i64::from(color[1]) << 8)
        | i64::from(color[2])
}

/// Which [`ObjectRegistry`] map an id belongs to.
#[derive(Clone, Copy)]
pub(crate) enum ObjectKind {
    Window,
    Layer,
}

/// Duplicate a registered object into an owned [`DetachedValue`].
pub(crate) fn duplicate_object(
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    kind: ObjectKind,
    id: u32,
) -> Option<DetachedValue> {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let state = services.state::<VisualState>();
    let source = match kind {
        ObjectKind::Window => state.objects.windows.get(&id),
        ObjectKind::Layer => state.objects.layers.get(&id),
    }?;
    let raw = source.raw_id();
    drop(state);
    host.duplicate_retained(raw).ok()
}

/// Resolve a script object argument to a scene id by reading its `nativeId`
/// then `id` member (the reference wrappers expose both).
pub(crate) fn resolve_object_id(host: &Tjs2Engine, value: &Value) -> Result<i64, String> {
    match value.ty {
        VAL_INTEGER => Ok(value.integer),
        VAL_REAL => Ok(value.real as i64),
        VAL_OBJECT => {
            let object = host.retain_object_arg(value)?;
            let mut last_error = None;
            for member in ["nativeId", "id"] {
                match host.get_member(object.raw_id(), member) {
                    Ok(TjsValue::Integer(v)) => return Ok(v),
                    Ok(TjsValue::Real(v)) => return Ok(v as i64),
                    Ok(_) => {}
                    Err(error) => last_error = Some(error),
                }
            }
            match last_error {
                Some(error) => Err(error),
                None => Ok(-1),
            }
        }
        _ => Ok(-1),
    }
}

/// Detach `layer_id` from its current parent and attach it to
/// `new_parent` (`None` = the window root). Rejects self/descendant cycles.
pub(crate) fn reparent_layer(scene: &mut Scene, layer_id: u32, new_parent: Option<u32>) {
    let Some(layer) = scene.layer(layer_id) else {
        return;
    };
    let window = layer.window;
    let old_parent = layer.parent;
    let new_parent = new_parent.filter(|&parent| {
        if parent == layer_id {
            return false;
        }
        if scene.layer(parent).is_none_or(|p| p.window != window) {
            return false;
        }
        // Reject a cycle: the candidate must not be a descendant of this
        // layer.
        let mut cursor = Some(parent);
        while let Some(id) = cursor {
            if id == layer_id {
                return false;
            }
            cursor = scene.layer(id).and_then(|l| l.parent);
        }
        true
    });
    if old_parent == new_parent {
        return;
    }
    match old_parent {
        Some(parent) => {
            if let Some(parent_layer) = scene.layer_mut(parent) {
                parent_layer.children.retain(|&child| child != layer_id);
            }
        }
        None => {
            if let Some(window_state) = scene.window_mut(window) {
                window_state.layers.retain(|&child| child != layer_id);
            }
        }
    }
    if let Some(layer) = scene.layer_mut(layer_id) {
        layer.parent = new_parent;
    }
    match new_parent {
        Some(parent) => {
            if let Some(parent_layer) = scene.layer_mut(parent) {
                parent_layer.children.retain(|&child| child != layer_id);
                parent_layer.children.push(layer_id);
            }
        }
        None => {
            if let Some(window_state) = scene.window_mut(window) {
                window_state.layers.retain(|&child| child != layer_id);
                window_state.layers.push(layer_id);
            }
        }
    }
}

// `set_real` is used by future real-valued properties; keep it available.
#[allow(dead_code)]
const _: fn(*mut Value, f64) = set_real;
