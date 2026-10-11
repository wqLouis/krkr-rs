//! Shared helpers, dispatch state and value utilities for the visual natives.
#![allow(unsafe_code)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{c_char, c_void};

use engine::Scene;
use engine::input::InputState;
use engine::timer::TimerId;
use platform::Clock;
use tjs::{
    DetachedValue, RetainedValue, Tjs2Engine, Tjs2ValueId, VAL_INTEGER, VAL_OBJECT, VAL_REAL,
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
    pub(crate) fonts: HashMap<u32, DetachedValue>,
}

/// One object registered with a window through `Window.add`, mirroring the
/// reference `tTJSNI_BaseWindow::ObjectVector`. The window holds the retained
/// closure and invalidates it when the window is destroyed (breaking
/// window↔object reference cycles).
pub(crate) struct WindowObject {
    /// Identity of the registered closure: the raw `iTJSDispatch2*` object
    /// pointer and the closure receiver (`0` for a plain object). Used to
    /// make a repeated `Window.add` of the same closure a no-op, like the
    /// reference `std::find` over `ObjectVector`.
    pub(crate) identity: (usize, usize),
    /// The retained value, released after invalidation.
    pub(crate) value: DetachedValue,
}

/// One registered `Timer`'s L3 payload: the retained action owner, the timer
/// object, the script-visible reference fields (`tTJSNI_BaseTimer`) and L3's
/// mirror of the L2 schedule. The L2 `TimerState` registry holds only what
/// "wait, then call" needs — the integer-millisecond interval, the armed
/// deadline and the dispatch callback — so every script-visible readback
/// (interval, capacity, mode, enabled) is served from here, never from L2.
pub(crate) struct TimerBinding {
    /// The constructor's `param[0]` retained as the reference's
    /// `ActionOwner` (`TimerIntf.cpp:51`, `param[0]->AsObjectClosure()`).
    /// `None` when the argument was not an object: the reference stores an
    /// empty closure then and its native `onTimer` skips the invocation
    /// (`if(obj.Object)`, `TimerIntf.cpp:161`), so such a timer is accepted
    /// silently and never called back. Taken out across the VM call so the
    /// state borrow is released while the action runs.
    pub(crate) action_owner: Option<DetachedValue>,
    /// The timer object itself (`Owner`, `TimerIntf.cpp:52`), for `onTimer`
    /// dispatch through its class chain (a script subclass override wins).
    pub(crate) owner: Option<DetachedValue>,
    /// The reference `ActionName` (`TimerIntf.cpp:48-49`), stored verbatim:
    /// it defaults to `TVPActionName == "action"` (`EventIntf.cpp:740`) and
    /// is overwritten only when the script passes a non-void second
    /// argument. An empty name dispatches the owner itself instead of a
    /// member (`TimerIntf.cpp:165`).
    pub(crate) action_name: String,
    /// Event delivery mode (`tTVPAsyncTriggerMode`, `EventIntf.h:312`),
    /// TVP-specific event priority — unrelated to when the timer fires.
    pub(crate) mode: i64,
    /// The script-visible `Capacity` (reference `tTJSNI_BaseTimer::Capacity`,
    /// `TimerIntf.h:63`), stored **verbatim** as the script wrote it —
    /// `0` and negatives survive, the getter reports them back
    /// (`TimerIntf.cpp:222-236`) — default [`DEFAULT_TIMER_CAPACITY`]
    /// (`TVP_DEFAULT_TIMER_CAPACITY`). Only the sign is re-read, to decide
    /// whether L2 is armed at all: a negative capacity makes the reference
    /// post no `onTimer` event at all (`TimerIntf.cpp:76-84`), while `0` is
    /// its "no limit" sentinel (`TimerIntf.cpp:77`) and any positive value
    /// only bounds the *queued* events — under synchronous dispatch the
    /// queue is always empty, so neither stops delivery.
    pub(crate) capacity: i64,
    /// The script-visible `enabled`, stored **verbatim** as the script wrote
    /// it (the reference getter reports what `SetEnabled` stored,
    /// `TimerIntf.cpp:204-216`): a timer the script enabled reports `true`
    /// even while L3 keeps it *unarmed* in L2, because the reference's
    /// "armed and enabled yet never posts" states — `capacity < 0`
    /// (`TimerIntf.cpp:76-84`) and `interval <= 0` (`TimerImpl.cpp:125`) —
    /// are expressed by not arming the L2 schedule at all. The getter reads
    /// this field, never L2.
    pub(crate) enabled: bool,
    /// Whether L2 currently has this timer armed — L3's mirror of the
    /// schedule it pushed (L2 has no introspection). Maintained
    /// exclusively by `timer.rs`'s schedule sync: armed iff `enabled &&
    /// capacity >= 0 && interval > 0`, and `timer_poll` reports "any timer
    /// still enabled" from this.
    pub(crate) armed: bool,
    /// The reference `Interval` in sub-millisecond fixed point
    /// (`TVP_SUBMILLI_FRAC_BITS = 16`, `TimerIntf.h:18`): the exact
    /// `tjs_uint64` the reference setter stores (`TimerIntf.cpp:196-197`),
    /// so the getter reproduces the reference readback
    /// (`TimerIntf.cpp:186-188`) *including* wrapped negatives (a script
    /// `interval = -1` reads back `2^48 - 1`). The L2 registry keeps integer
    /// milliseconds derived from this value instead (see `timer.rs`).
    ///
    /// Initial value: the reference constructor default interval of 1000 ms
    /// (`TimerImpl.cpp:363`) expressed in sub-ms units, so the reference
    /// getter conversion reports `1000.0` — matching the L2 default interval
    /// (1000 ms, pushed whenever L3 arms the timer) and the pinned
    /// `timer_behavior::interval_defaults_roundtrip_and_reports_a_real`.
    pub(crate) interval_units: u64,
}

/// The reference `TVP_DEFAULT_TIMER_CAPACITY = 6`
/// (`reference/cpp/core/utils/TimerIntf.cpp:17,32`), script-visible and
/// stored verbatim (negatives included) — hence `i64`, not `u32`.
pub(crate) const DEFAULT_TIMER_CAPACITY: i64 = 6;

/// `VisualState` — the visual family's dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
#[derive(Default)]
pub(crate) struct VisualState {
    pub(crate) objects: ObjectRegistry,
    /// Window scene id → objects registered through `Window.add`, in
    /// registration order (reference `ObjectVector`).
    pub(crate) window_objects: HashMap<u32, Vec<WindowObject>>,
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

/// The scene id reader for one of the classes whose native-instance payload
/// carries it, keyed by the name L3 registers the class with (`Window`,
/// `Layer`, `Bitmap`, `Font`).
fn payload_id_reader(class: &str) -> Option<fn(*mut c_void) -> Option<u32>> {
    match class {
        "Window" => Some(super::window::window_payload_id),
        "Layer" => Some(super::layer::layer_payload_id),
        "Bitmap" => Some(super::bitmap::bitmap_payload_id),
        "Font" => Some(super::font::font_payload_id),
        _ => None,
    }
}

/// The scene id of `object` as an instance of one of `classes` (the first
/// class that matches wins), resolved through the VM's native-instance
/// lookup — the reference `NativeInstanceSupport(TJS_NIS_GETINSTANCE,
/// classid)` behind `TJS_GET_NATIVE_INSTANCE`.
///
/// `Ok(None)` is an ordinary miss (the `tjs2_get_native_instance` NULL
/// contract): TJS `null`, a plain script object, an instance of another
/// native class, or an instance whose payload carries no scene id yet —
/// never an error. A `classes` entry that is not one of the
/// scene-id-bearing class names is a caller mistake and returns `Err`.
fn native_instance_id(
    host: &Tjs2Engine,
    object: Tjs2ValueId,
    classes: &[&str],
) -> Result<Option<u32>, String> {
    for class in classes {
        let reader = payload_id_reader(class)
            .ok_or_else(|| format!("native_instance_id: unknown class {class:?}"))?;
        if let Some(payload) = host.native_instance_payload(object, class)?
            && let Some(id) = reader(payload.as_ptr())
        {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// Resolve a script object argument to a scene id.
///
/// Integers and reals pass through unchanged; anything that is neither an
/// object nor a number keeps the `-1` fallback. A non-null object is resolved
/// through [`native_instance_id`] for each expected class in `classes` — the
/// reference object → native-instance mechanism. An object that matches none
/// of `classes` is a typed error, never a silent sentinel: the reference
/// `NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)` rejects it.
pub(crate) fn resolve_object_id(
    host: &Tjs2Engine,
    value: &Value,
    classes: &[&str],
) -> Result<i64, String> {
    match value.ty {
        VAL_INTEGER => Ok(value.integer),
        VAL_REAL => Ok(value.real as i64),
        VAL_OBJECT => {
            let object = host.retain_object_arg(value)?;
            match native_instance_id(host, object.raw_id(), classes)? {
                Some(id) => Ok(i64::from(id)),
                None => Err(format!("expected a native {} object", classes.join("/"))),
            }
        }
        _ => Ok(-1),
    }
}

impl CompatCtx {
    /// Cross-family entry point for [`resolve_object_id`]: the object →
    /// scene-id resolution through the VM native-instance lookup, used by
    /// the `video` and plugin natives, which cannot name this module directly.
    pub(crate) fn resolve_object_id(
        host: &Tjs2Engine,
        value: &Value,
        classes: &[&str],
    ) -> Result<i64, String> {
        super::common::resolve_object_id(host, value, classes)
    }

    /// Cross-family entry point for [`native_instance_id`]: the scene id of
    /// a retained object as an instance of one of `classes` via
    /// [`Tjs2Engine::native_instance_payload`], `Ok(None)` on an ordinary
    /// miss (the reference `TJS_GET_NATIVE_INSTANCE` lookup).
    pub(crate) fn native_instance_id(
        host: &Tjs2Engine,
        object: Tjs2ValueId,
        classes: &[&str],
    ) -> Result<Option<u32>, String> {
        super::common::native_instance_id(host, object, classes)
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

// `set_real` is consumed by the real-valued properties (`Timer.interval`,
// `Font.angle`, `Window.touchScaleThreshold`, ...); no keep-alive needed.
