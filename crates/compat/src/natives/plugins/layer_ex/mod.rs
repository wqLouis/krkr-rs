//! The `LayerEx*` plugin family: `LayerExBase`, `LayerExPerspective`,
//! `LayerExMovie` and the GDI+ `LayerExDraw` drawing extension.
//!
//! Behavioral references:
//!
//! * `reference/cpp/plugins/LayerExBase.{h,cpp}` / `layerExBase.hpp` — the
//!   shared layer-extension base class that caches a layer's geometry and
//!   main-image buffer and drives `Layer.update`.
//! * `reference/cpp/plugins/layerExPerspective.cpp` — `perspectiveCopy`,
//!   attached to `Layer`.
//! * `reference/cpp/plugins/layerExMovie.cpp` — the movie layer
//!   (`openMovie`/`startMovie`/`stopMovie`/`isPlayingMovie` plus the
//!   `onStartMovie`/`onStopMovie`/`onUpdateMovie` callbacks).
//! * `reference/cpp/plugins/layerex_draw/` — the GDI+ `LayerExDraw` extension
//!   (`Appearance`/`Path`/`PointF`/`RectF`/`FontInfo` value types and the
//!   `Layer.draw*` / transform / record members).
//!
//! The port composes the L2 [`engine::layer_ops`] / [`engine::raster`]
//! primitives for the pixel work and the L2 [`engine::Scene`] /
//! [`engine::LayerState`] for layer state; the classes own only the dispatch
//! state that has no home in L2 (the `LayerExMovie` playback slots and the
//! per-layer `LayerExDraw` transform/record state), kept in
//! [`CompatServices::state`](crate::CompatServices::state) rather than in a
//! process global.

mod appearance;
mod base;
mod draw;
mod font;
mod geom;
mod movie;
mod path;
mod perspective;
mod record;

use std::ffi::{c_char, c_void};

use engine::Scene;
use tjs::{
    DetachedValue, NativeInstanceMembers, RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue,
    VAL_INTEGER, VAL_OBJECT, VAL_REAL, VAL_STRING, Value,
};

use crate::natives::system::resolve;
use crate::{CompatCtx, CompatError};

pub use crate::natives::video::VideoSource;
pub use movie::{movie_poll, set_movie_source};

/// The per-class native payload shared by the three extension classes. The
/// reference `layerExBase` caches the owning layer's geometry; the port reads
/// it live from the L2 scene, so the payload only has to remember *which*
/// layer the extension instance is bound to.
#[derive(Default)]
pub(super) struct BaseInst {
    pub(super) layer: Option<u32>,
}

/// Resolve the installed engine → context binding for a native callback.
pub(super) fn bound(
    engine: *mut c_void,
) -> Result<(crate::natives::system::EngineBinding, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "LayerEx: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved, ctx))
}

/// The attached L2 scene, mutably.
pub(super) fn scene_mut<'a>(ctx: &CompatCtx) -> Result<&'a mut Scene, String> {
    // SAFETY: the host keeps the attached scene alive for the binding's
    // lifetime and the VM is single-threaded.
    let ptr = unsafe { ctx.services() }
        .scene()
        .ok_or_else(|| "LayerEx: no scene attached".to_string())?;
    // SAFETY: see above.
    Ok(unsafe { &mut *ptr })
}

/// The attached L2 scene, shared.
pub(super) fn scene_ref<'a>(ctx: &CompatCtx) -> Result<&'a Scene, String> {
    // SAFETY: see [`scene_mut`].
    let ptr = unsafe { ctx.services() }
        .scene()
        .ok_or_else(|| "LayerEx: no scene attached".to_string())?;
    // SAFETY: see above.
    Ok(unsafe { &*ptr })
}

/// ARGB (`0xAARRGGBB`) → straight-alpha RGBA, the scene's channel order.
pub(super) fn argb_to_rgba(color: i64) -> [u8; 4] {
    let value = color as u32;
    [
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    ]
}

/// Resolve a script object argument to a scene id. An object argument is
/// resolved through the VM's native-instance lookup for the `Layer` and
/// `Bitmap` classes (reference `TJS_GET_NATIVE_INSTANCE`); an object of
/// another class is an error.
pub(super) fn resolve_id(host: &Tjs2Engine, value: &Value) -> Result<i64, String> {
    crate::CompatCtx::resolve_object_id(host, value, &["Layer", "Bitmap"])
}

/// The scene id bound to a native-instance payload.
pub(super) fn inst_layer(instance: *const c_void) -> Option<u32> {
    if instance.is_null() {
        return None;
    }
    // SAFETY: every `LayerEx*` class payload is a `BaseInst`.
    let inst = unsafe { &*(instance as *const BaseInst) };
    inst.layer
}

/// The scene id of an objthis-only `Layer` member, via the VM's
/// native-instance lookup (a `Layer` object's `id`/`nativeId` script members
/// no longer exist). `Ok(None)` means the object is not a `Layer`.
pub(super) fn layer_from_objthis(
    host: &Tjs2Engine,
    objthis: *mut c_void,
) -> Result<Option<u32>, String> {
    if objthis.is_null() {
        return Ok(None);
    }
    let object = host.retain_object_detached(objthis)?;
    crate::CompatCtx::native_instance_id(host, object.raw_id(), &["Layer"])
}

/// Read the bound layer (from a class payload) out of the attached scene.
pub(super) fn read_layer<'a>(
    engine: *mut c_void,
    instance: *const c_void,
    method: &str,
) -> Result<(u32, &'a Scene), String> {
    let id = inst_layer(instance).ok_or_else(|| format!("{method}: no layer bound"))?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_ref(&ctx)?;
    if scene.layer(id).is_none() {
        return Err(format!("{method}: the layer no longer exists"));
    }
    Ok((id, scene))
}

/// Mutably read the bound layer (from a class payload) out of the scene.
pub(super) fn read_layer_mut<'a>(
    engine: *mut c_void,
    instance: *const c_void,
    method: &str,
) -> Result<(u32, &'a mut Scene), String> {
    let id = inst_layer(instance).ok_or_else(|| format!("{method}: no layer bound"))?;
    let (_resolved, ctx) = bound(engine)?;
    let scene = scene_mut(&ctx)?;
    if scene.layer(id).is_none() {
        return Err(format!("{method}: the layer no longer exists"));
    }
    Ok((id, scene))
}

/// Mark a layer as needing a repaint (the reference `layerExBase::redraw`
/// calls `Layer.update` over the whole image).
pub(super) fn mark_layer_paint(scene: &mut Scene, id: u32) {
    if let Some(layer) = scene.layer_mut(id) {
        layer.pending_paint = true;
    }
}

/// A layer's clip rectangle in the half-open pixel convention the L2
/// operations use (`None` means the whole image).
pub(super) fn layer_clip(scene: &Scene, id: u32) -> engine::layer_ops::RectI {
    engine::layer_ops::clip_rect(scene.layer(id).and_then(|l| l.clip))
}

/// Ensure `layer_id` owns a MainImage of at least `w` x `h`, preserving
/// content and filling the expansion with the layer's neutral color. Mirrors
/// the reference `AllocateImage` / the `Layer.setImageSize` path.
pub(super) fn ensure_layer_image(scene: &mut Scene, layer_id: u32, w: u32, h: u32) {
    let w = w.max(1);
    let h = h.max(1);
    let existing = scene.layer(layer_id).and_then(|l| l.bitmap);
    match existing {
        Some(bitmap_id) => {
            let fill = scene
                .layer(layer_id)
                .map(|l| argb_to_rgba(l.neutral_color))
                .unwrap_or([0, 0, 0, 0]);
            scene.resize_bitmap(bitmap_id, w, h, fill);
        }
        None => {
            let Some(len) = (w as usize)
                .checked_mul(h as usize)
                .and_then(|n| n.checked_mul(4))
            else {
                return;
            };
            let bitmap_id = scene.add_bitmap(w, h, vec![0u8; len]);
            if let Some(layer) = scene.layer_mut(layer_id) {
                layer.bitmap = Some(bitmap_id);
                layer.image_left = 0;
                layer.image_top = 0;
                layer.image_width = w;
                layer.image_height = h;
                layer.rect.w = w;
                layer.rect.h = h;
                layer.image_modified = true;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Value marshalling helpers shared by the `LayerExDraw` classes
// ---------------------------------------------------------------------------

/// Write a real result into a native callback's out slot.
pub(super) fn set_real(out: *mut Value, value: f64) {
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

/// Write a retained object id into `out` (the C++ conversion consumes the
/// retention).
pub(super) fn set_retained_out(out: *mut Value, id: Tjs2ValueId) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = tjs::VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = id as usize;
    }
}

/// Scratch buffer for string results whose pointer must outlive the callback.
#[derive(Default)]
pub(super) struct Scratch {
    pub(super) buf: Vec<u8>,
}

/// Write a string result using the family's persistent scratch buffer.
pub(super) fn set_string_out(
    engine: *mut c_void,
    out: *mut Value,
    text: &str,
) -> Result<(), String> {
    if out.is_null() {
        return Ok(());
    }
    let (_resolved, ctx) = bound(engine)?;
    let services = unsafe { ctx.services() };
    let mut state = services.state::<Scratch>();
    state.buf.clear();
    state.buf.extend_from_slice(text.as_bytes());
    state.buf.push(0);
    // SAFETY: the trampoline passes a valid out slot for the call; `state.buf`
    // stays allocated (the state outlives the callback) until the next string.
    unsafe {
        (*out).ty = VAL_STRING;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = state.buf.as_ptr() as *const c_char;
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
    Ok(())
}

fn finite(value: f64) -> f64 {
    if value.is_finite() { value } else { 0.0 }
}

fn eval_object(host: &Tjs2Engine, expression: &str, what: &str) -> Result<DetachedValue, String> {
    match host.eval_retained(expression, what) {
        Ok(RetainedValue::Object(object)) => Ok(object),
        Ok(RetainedValue::Value(value)) => Err(format!(
            "{what}: `{expression}` returned {value:?}, not an object"
        )),
        Err(error) => Err(error.to_string()),
    }
}

/// Build a fresh `PointF` object.
pub(super) fn new_pointf(host: &Tjs2Engine, x: f64, y: f64) -> Result<DetachedValue, String> {
    let expression = format!("new PointF({}, {})", finite(x), finite(y));
    eval_object(host, &expression, "LayerEx.new PointF")
}

/// Build a fresh `RectF` object.
pub(super) fn new_rectf(
    host: &Tjs2Engine,
    x: f64,
    y: f64,
    w: f64,
    h: f64,
) -> Result<DetachedValue, String> {
    let expression = format!(
        "new RectF({}, {}, {}, {})",
        finite(x),
        finite(y),
        finite(w),
        finite(h)
    );
    eval_object(host, &expression, "LayerEx.new RectF")
}

/// TJS numeric coercion to a non-negative `usize`.
pub(super) fn value_as_usize(value: &Value) -> Option<usize> {
    match value.ty {
        VAL_INTEGER if value.integer >= 0 => Some(value.integer as usize),
        VAL_REAL if value.real >= 0.0 => Some(value.real as usize),
        _ => None,
    }
}

fn read_number_member(host: &Tjs2Engine, id: Tjs2ValueId, name: &str) -> Result<f64, String> {
    match host.member(id, name) {
        Ok(TjsValue::Integer(value)) => Ok(value as f64),
        Ok(TjsValue::Real(value)) => Ok(value),
        Ok(TjsValue::String(value)) => value
            .parse()
            .map_err(|_| format!("LayerEx: member {name} is not numeric")),
        Ok(_) => Err(format!("LayerEx: member {name} is not numeric")),
        Err(error) => Err(format!("LayerEx: cannot read member {name}: {error}")),
    }
}

fn read_point_object(
    host: &Tjs2Engine,
    object: &DetachedValue,
    what: &str,
) -> Result<(f64, f64), String> {
    let is_array = matches!(
        host.member(object.raw_id(), "count"),
        Ok(TjsValue::Integer(count)) if count >= 2
    );
    if is_array {
        let x = read_number_member(host, object.raw_id(), "0")?;
        let y = read_number_member(host, object.raw_id(), "1")?;
        return Ok((x, y));
    }
    let x = read_number_member(host, object.raw_id(), "x")
        .map_err(|_| format!("{what}: the object is not a point"))?;
    let y = read_number_member(host, object.raw_id(), "y")
        .map_err(|_| format!("{what}: the object is not a point"))?;
    Ok((x, y))
}

fn read_rect_object(
    host: &Tjs2Engine,
    object: &DetachedValue,
    what: &str,
) -> Result<(f64, f64, f64, f64), String> {
    let is_array = matches!(
        host.member(object.raw_id(), "count"),
        Ok(TjsValue::Integer(count)) if count >= 4
    );
    if is_array {
        return Ok((
            read_number_member(host, object.raw_id(), "0")?,
            read_number_member(host, object.raw_id(), "1")?,
            read_number_member(host, object.raw_id(), "2")?,
            read_number_member(host, object.raw_id(), "3")?,
        ));
    }
    Ok((
        read_number_member(host, object.raw_id(), "x")
            .map_err(|_| format!("{what}: the object is not a rectangle"))?,
        read_number_member(host, object.raw_id(), "y")
            .map_err(|_| format!("{what}: the object is not a rectangle"))?,
        read_number_member(host, object.raw_id(), "width")
            .map_err(|_| format!("{what}: the object is not a rectangle"))?,
        read_number_member(host, object.raw_id(), "height")
            .map_err(|_| format!("{what}: the object is not a rectangle"))?,
    ))
}

/// Read a `[x, y]` / `%[x=>, y=>]` / `PointF` argument.
pub(super) fn read_point_arg(
    engine: *mut c_void,
    value: &Value,
    what: &str,
) -> Result<(f64, f64), String> {
    let (resolved, _ctx) = bound(engine)?;
    let host = resolved.engine();
    if value.ty != VAL_OBJECT {
        return Err(format!("{what}: expected a point"));
    }
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("{what}: {error}"))?;
    read_point_object(host, &object, what)
}

/// Read a `[x, y, w, h]` / `%[x=>, y=>, width=>, height=>]` / `RectF`
/// argument.
pub(super) fn read_rect_arg(
    engine: *mut c_void,
    value: &Value,
    what: &str,
) -> Result<(f64, f64, f64, f64), String> {
    let (resolved, _ctx) = bound(engine)?;
    let host = resolved.engine();
    if value.ty != VAL_OBJECT {
        return Err(format!("{what}: expected a rectangle"));
    }
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("{what}: {error}"))?;
    read_rect_object(host, &object, what)
}

/// Read an object member as a point, `None` when the member is absent.
pub(super) fn read_point_member(
    host: &Tjs2Engine,
    id: Tjs2ValueId,
    name: &str,
) -> Result<Option<(f64, f64)>, String> {
    match host.member(id, name) {
        Ok(TjsValue::Object) => {
            let object = host
                .retain_value_detached(&TjsValue::Object)
                .map_err(|error| format!("LayerEx: {name}: {error}"))?;
            Ok(Some(read_point_object(host, &object, name)?))
        }
        _ => Ok(None),
    }
}

/// Read an object member as a rectangle, `None` when the member is absent.
pub(super) fn read_rect_member(
    host: &Tjs2Engine,
    id: Tjs2ValueId,
    name: &str,
) -> Result<Option<(f64, f64, f64, f64)>, String> {
    match host.member(id, name) {
        Ok(TjsValue::Object) => {
            let object = host
                .retain_value_detached(&TjsValue::Object)
                .map_err(|error| format!("LayerEx: {name}: {error}"))?;
            Ok(Some(read_rect_object(host, &object, name)?))
        }
        _ => Ok(None),
    }
}

/// Read an array argument as a list of points.
pub(super) fn read_points(
    engine: *mut c_void,
    value: &Value,
    what: &str,
) -> Result<Vec<(f64, f64)>, String> {
    let (resolved, _ctx) = bound(engine)?;
    let host = resolved.engine();
    if value.ty != VAL_OBJECT {
        return Err(format!("{what}: expected a point array"));
    }
    let array = host
        .retain_object_arg(value)
        .map_err(|error| format!("{what}: {error}"))?;
    let count = match host.member(array.raw_id(), "count") {
        Ok(TjsValue::Integer(count)) if count >= 0 => count as usize,
        Ok(TjsValue::Real(count)) if count >= 0.0 => count as usize,
        _ => return Err(format!("{what}: the argument is not an array")),
    };
    let mut out = Vec::with_capacity(count.min(1 << 20));
    for index in 0..count {
        let element = host
            .member(array.raw_id(), &index.to_string())
            .map_err(|error| format!("{what}: element {index}: {error}"))?;
        match element {
            TjsValue::Object => {
                let object = host
                    .retain_value_detached(&TjsValue::Object)
                    .map_err(|error| format!("{what}: element {index}: {error}"))?;
                out.push(read_point_object(host, &object, what)?);
            }
            _ => return Err(format!("{what}: element {index} is not a point")),
        }
    }
    Ok(out)
}

/// Read an array argument as a list of rectangles.
pub(super) fn read_rects(
    engine: *mut c_void,
    value: &Value,
    what: &str,
) -> Result<Vec<(f64, f64, f64, f64)>, String> {
    let (resolved, _ctx) = bound(engine)?;
    let host = resolved.engine();
    if value.ty != VAL_OBJECT {
        return Err(format!("{what}: expected a rectangle array"));
    }
    let array = host
        .retain_object_arg(value)
        .map_err(|error| format!("{what}: {error}"))?;
    let count = match host.member(array.raw_id(), "count") {
        Ok(TjsValue::Integer(count)) if count >= 0 => count as usize,
        Ok(TjsValue::Real(count)) if count >= 0.0 => count as usize,
        _ => return Err(format!("{what}: the argument is not an array")),
    };
    let mut out = Vec::with_capacity(count.min(1 << 20));
    for index in 0..count {
        let element = host
            .member(array.raw_id(), &index.to_string())
            .map_err(|error| format!("{what}: element {index}: {error}"))?;
        match element {
            TjsValue::Object => {
                let object = host
                    .retain_value_detached(&TjsValue::Object)
                    .map_err(|error| format!("{what}: element {index}: {error}"))?;
                out.push(read_rect_object(host, &object, what)?);
            }
            _ => return Err(format!("{what}: element {index} is not a rectangle")),
        }
    }
    Ok(out)
}

/// Read a native value object's payload pointer from its hidden `__handle`
/// property (the ABI carries no cross-class native-instance handle).
pub(super) fn read_handle(engine: *mut c_void, value: &Value, what: &str) -> Result<usize, String> {
    let (resolved, _ctx) = bound(engine)?;
    let host = resolved.engine();
    if value.ty != VAL_OBJECT {
        return Err(format!("{what}: expected a native value object"));
    }
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("{what}: {error}"))?;
    match host.member(object.raw_id(), "__handle") {
        Ok(TjsValue::Integer(handle)) if handle > 0 => Ok(handle as usize),
        Ok(TjsValue::Real(handle)) if handle > 0.0 => Ok(handle as usize),
        _ => Err(format!(
            "{what}: the argument is not a LayerExDraw value type"
        )),
    }
}

/// Register the `LayerExBase`/`LayerExPerspective`/`LayerExMovie` classes and
/// the `Layer` extension members.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    base::register_class(ctx)?;
    perspective::register_class(ctx)?;
    movie::register_class(ctx)?;
    geom::register_classes(ctx)?;
    path::register_class(ctx)?;
    appearance::register_class(ctx)?;
    font::register_class(ctx)?;

    // `Layer`-instance members. The reference attaches them with
    // `NCB_ATTACH_CLASS_WITH_HOOK` (movie), `Layer.perspectiveCopy`
    // (perspective) and `LayerExDraw` (draw).
    let mut methods = perspective::layer_method_defs();
    methods.extend(movie::layer_method_defs());
    methods.extend(draw::layer_method_defs());
    let properties = draw::layer_property_defs();
    ctx.register_instance_members(&NativeInstanceMembers {
        class_name: "Layer",
        methods,
        properties,
    })?;
    Ok(())
}

/// TJS `AsReal` coercion.
pub(super) fn value_as_f64(v: &Value) -> f64 {
    match v.ty {
        VAL_INTEGER => v.integer as f64,
        VAL_REAL => v.real,
        VAL_STRING => crate::native_util::value_as_string(v)
            .parse()
            .unwrap_or(0.0),
        _ => 0.0,
    }
}
