//! Shared helpers for the `Layer` method submodules.
//!
//! These are the small marshalling / geometry / borrow routines every method
//! group needs. They live here (rather than in a process global) so the
//! `Layer` code stays a thin adapter over the L2 [`engine::Scene`].

use api::blend::BlendMode;
use engine::input::InputState;
use engine::layer_ops::RectI;
use engine::{BitmapState, Scene};
use tjs::{RetainedValue, Tjs2Engine, TjsValue, VAL_OBJECT, VAL_REAL, VAL_STRING, VAL_VOID, Value};

use super::super::common::{VisualState, resolve_object_id, scene_ref};
use crate::CompatCtx;
use crate::natives::system::{value_as_bool, value_as_i64};

/// TJS `AsInteger` coercion with the reference's `tvtVoid` default.
pub(super) fn opt_i64(a: &[Value], index: usize, default: i64) -> i64 {
    match a.get(index) {
        Some(v) if v.ty != VAL_VOID => value_as_i64(v),
        _ => default,
    }
}

/// TJS numeric coercion.
///
/// Port of `tTJSVariant::AsReal`
/// (`reference/cpp/core/tjs2/tjsVariant.h:937-956`): integers and reals
/// convert directly, `void` is `0.0`, and a **string parses like a number**
/// (`String->ToReal()`, `tjsVariant.cpp:1006-1013` — the lexer's number
/// grammar followed by `TJSStringToReal`, so the leading numeric token wins
/// and trailing garbage is ignored). Objects would raise
/// `TJSThrowVariantConvertError` in the reference; this loose form keeps
/// returning `0.0`, mirroring [`crate::natives::system::value_as_i64`].
pub(super) fn value_as_f64(v: &Value) -> f64 {
    match v.ty {
        tjs::VAL_INTEGER => v.integer as f64,
        VAL_REAL => v.real,
        VAL_STRING => string_to_real(&crate::native_util::value_as_string(v)),
        _ => 0.0,
    }
}

/// The reference `TJSStringToReal` behaviour: parse the leading numeric
/// token of `s` (sign, digits, fraction, exponent) and ignore the rest;
/// anything unparsable yields `0.0`.
fn string_to_real(s: &str) -> f64 {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let mut seen_digit = false;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
        seen_digit = true;
    }
    if i < bytes.len() && bytes[i] == b'.' {
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            seen_digit = true;
        }
    }
    if seen_digit && i < bytes.len() && (bytes[i] == b'e' || bytes[i] == b'E') {
        let save = i;
        i += 1;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            i += 1;
        }
        let mut exp_digit = false;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            i += 1;
            exp_digit = true;
        }
        if !exp_digit {
            i = save;
        }
    }
    if !seen_digit {
        return 0.0;
    }
    // The slice is a well-formed float literal by construction; a failure
    // (e.g. exponent overflow) still parses as an infinite/`f64` value.
    s[..i].trim().parse::<f64>().unwrap_or(0.0)
}

/// TJS numeric coercion for a real-valued default.
pub(super) fn opt_f64(a: &[Value], index: usize, default: f64) -> f64 {
    match a.get(index) {
        Some(v) if v.ty != VAL_VOID => value_as_f64(v),
        _ => default,
    }
}

/// TJS `operator bool` coercion with the reference's `tvtVoid` default.
pub(super) fn opt_bool(a: &[Value], index: usize, default: bool) -> bool {
    match a.get(index) {
        Some(v) if v.ty != VAL_VOID => value_as_bool(v),
        _ => default,
    }
}

/// Normalise a rectangle so `right >= left` and `bottom >= top`.
pub(super) fn normalize_rect(rect: RectI) -> RectI {
    let (mut left, mut top, mut right, mut bottom) = rect;
    if right < left {
        std::mem::swap(&mut left, &mut right);
    }
    if bottom < top {
        std::mem::swap(&mut top, &mut bottom);
    }
    (left, top, right, bottom)
}

/// Remap the sub-rectangle `src` from the `from` box into the `to` box,
/// preserving relative coordinates (used to trim a stretched source when the
/// destination is clipped).
pub(super) fn scale_rect(src: RectI, from: RectI, to: RectI) -> RectI {
    let fw = from.2 - from.0;
    let fh = from.3 - from.1;
    let sw = src.2 - src.0;
    let sh = src.3 - src.1;
    if fw == 0 || fh == 0 {
        return src;
    }
    let map_x = |x: i32| -> i32 {
        src.0
            .saturating_add((((x - from.0) as f64 * sw as f64) / fw as f64) as i32)
    };
    let map_y = |y: i32| -> i32 {
        src.1
            .saturating_add((((y - from.1) as f64 * sh as f64) / fh as f64) as i32)
    };
    (map_x(to.0), map_y(to.1), map_x(to.2), map_y(to.3))
}

/// Port of the reference `ClipDestPointAndSrcRect`: clip the destination
/// point `(dx, dy)` and the source rectangle against a destination clip
/// rectangle. Returns the adjusted `(dx, dy, src_rect)` or `None` when the
/// operation is fully clipped away.
pub(super) fn clip_dest_src(
    dx: i32,
    dy: i32,
    src: RectI,
    clip: RectI,
) -> Option<(i32, i32, RectI)> {
    let mut dx = dx;
    let mut dy = dy;
    let mut left = src.0;
    let mut top = src.1;
    let mut right = src.2;
    let mut bottom = src.3;

    let dest_right = dx.saturating_add(right - left);
    let dest_bottom = dy.saturating_add(bottom - top);

    if dx < clip.0 {
        left += clip.0 - dx;
        dx = clip.0;
    }
    if dest_right > clip.2 {
        right -= dest_right - clip.2;
    }
    if right <= left {
        return None;
    }
    if dy < clip.1 {
        top += clip.1 - dy;
        dy = clip.1;
    }
    if dest_bottom > clip.3 {
        bottom -= dest_bottom - clip.3;
    }
    if bottom <= top {
        return None;
    }
    Some((dx, dy, (left, top, right, bottom)))
}

/// The attached L2 input state, mutably.
pub(super) fn input_mut<'a>(ctx: &CompatCtx) -> Result<&'a mut InputState, String> {
    // SAFETY: the host keeps the attached input state alive for the binding's
    // lifetime and the VM is single-threaded (the documented `attach_input`
    // contract), so forming this reference is sound within the callback.
    let ptr = unsafe { ctx.services() }
        .input()
        .ok_or_else(|| "visual: no input state attached".to_string())?;
    // SAFETY: see above.
    Ok(unsafe { &mut *ptr })
}

/// The layer's `ClipRect` (or the whole image when unset) as a half-open
/// pixel rectangle the L2 operations take.
pub(super) fn layer_clip(scene: &Scene, layer_id: u32) -> RectI {
    scene
        .layer(layer_id)
        .map_or(engine::layer_ops::clip_rect(None), |l| {
            engine::layer_ops::clip_rect(l.clip)
        })
}

/// The layer's main image bitmap id, or a typed error when the layer has no
/// drawable image (the reference `TVPNotDrawableLayerType`).
pub(super) fn main_bitmap(scene: &Scene, layer_id: u32, method: &str) -> Result<u32, String> {
    scene
        .layer(layer_id)
        .and_then(|l| l.bitmap)
        .ok_or_else(|| format!("Layer.{method}: this layer has no main image"))
}

/// The scene id of `value` when it is a native instance of `class` — the
/// strict object→class check the reference performs with
/// `TJS_NIS_GETINSTANCE` before any legacy member fallback
/// (`reference/cpp/core/visual/LayerIntf.cpp:8745-8752`). `None` for a
/// non-object, a plain script object or an instance of another class.
fn native_class_id(host: &Tjs2Engine, value: &Value, class: &str) -> Result<Option<u32>, String> {
    if value.ty != VAL_OBJECT {
        return Ok(None);
    }
    let object = host.retain_object_arg(value)?;
    let Some(payload) = host.native_instance_payload(object.raw_id(), class)? else {
        return Ok(None);
    };
    let id = match class {
        "Layer" => super::layer_payload_id(payload.as_ptr()),
        "Bitmap" => super::super::bitmap::bitmap_payload_id(payload.as_ptr()),
        _ => None,
    };
    Ok(id)
}

/// Resolve a `Layer` or `Bitmap` script argument to a scene bitmap id, plus
/// the source layer's generic blend when the argument is a `Layer` (needed
/// for the `omAuto` operation-mode default).
///
/// `layer_only` reproduces the reference split between
/// `TVPSpecifyLayer` (`LayerIntf.cpp:8740-8756`: `pileRect`, `blendRect`,
/// `piledCopy`, `stretchPile`, `stretchBlend`, `affinePile`, `affineBlend`
/// accept **only** a Layer object) and `TVPSpecifyLayerOrBitmap`
/// (`LayerIntf.cpp:8796-8831`: `copyRect`, `operateRect`, … also accept a
/// `Bitmap`): a `Bitmap` argument for a Layer-only member is an error, not
/// a silent success.
pub(super) fn resolve_source_bitmap(
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    value: &Value,
    method: &str,
    layer_only: bool,
) -> Result<(u32, Option<BlendMode>), String> {
    // The reference resolves an object argument by native class, Layer first
    // and then Bitmap (`LayerIntf.cpp:8796-8831`). The native-instance check
    // runs before the legacy id path so a Bitmap whose scene id collides with
    // a Layer is never mistaken for one.
    if let Some(layer_id) = native_class_id(host, value, "Layer")? {
        let scene = scene_ref(ctx)?;
        let layer = scene
            .layer(layer_id)
            .ok_or_else(|| format!("Layer.{method}: the source layer no longer exists"))?;
        let bitmap = layer
            .bitmap
            .ok_or_else(|| format!("Layer.{method}: the source layer has no main image"))?;
        return Ok((bitmap, Some(layer.blend)));
    }
    if let Some(bitmap_id) = native_class_id(host, value, "Bitmap")? {
        if layer_only {
            return Err(format!("Layer.{method}: the source must be a Layer object"));
        }
        let scene = scene_ref(ctx)?;
        if scene.bitmap(bitmap_id).is_none() {
            return Err(format!(
                "Layer.{method}: the source bitmap {bitmap_id} does not exist"
            ));
        }
        return Ok((bitmap_id, None));
    }
    // Legacy form: a numeric scene id or a plain script object carrying its
    // own `nativeId`/`id` member.
    let id = resolve_object_id(host, value, &["Layer", "Bitmap"])?;
    if id < 0 {
        return Err(format!(
            "Layer.{method}: the source is not a Layer or Bitmap object"
        ));
    }
    let id = id as u32;
    let is_layer = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        let state = services.state::<VisualState>();
        state.objects.layers.contains_key(&id)
    };
    let scene = scene_ref(ctx)?;
    if is_layer || scene.layer(id).is_some() {
        let layer = scene
            .layer(id)
            .ok_or_else(|| format!("Layer.{method}: the source layer no longer exists"))?;
        let bitmap = layer
            .bitmap
            .ok_or_else(|| format!("Layer.{method}: the source layer has no main image"))?;
        return Ok((bitmap, Some(layer.blend)));
    }
    if layer_only {
        return Err(format!("Layer.{method}: the source must be a Layer object"));
    }
    if scene.bitmap(id).is_none() {
        return Err(format!(
            "Layer.{method}: the source bitmap {id} does not exist"
        ));
    }
    Ok((id, None))
}

/// Borrow the destination and source bitmaps of an operation simultaneously
/// (`dst` and `src` may be the same bitmap, in which case the source is
/// snapshotted so the blend reads stable pixels).
pub(super) fn with_two_bitmaps<R>(
    scene: &mut Scene,
    dst_id: u32,
    src_id: u32,
    f: impl FnOnce(&mut BitmapState, &BitmapState) -> R,
) -> Result<R, String> {
    let dst_index = scene
        .bitmaps
        .iter()
        .position(|b| b.id == dst_id)
        .ok_or_else(|| format!("Layer: the destination bitmap {dst_id} does not exist"))?;
    let src_index = scene
        .bitmaps
        .iter()
        .position(|b| b.id == src_id)
        .ok_or_else(|| format!("Layer: the source bitmap {src_id} does not exist"))?;
    if dst_index == src_index {
        let source = scene.bitmaps[src_index].clone();
        return Ok(f(&mut scene.bitmaps[dst_index], &source));
    }
    if dst_index < src_index {
        let (left, right) = scene.bitmaps.split_at_mut(src_index);
        Ok(f(&mut left[dst_index], &right[0]))
    } else {
        let (left, right) = scene.bitmaps.split_at_mut(dst_index);
        Ok(f(&mut right[0], &left[src_index]))
    }
}

/// Map a source layer's generic [`BlendMode`] to the operation mode the
/// reference `GetOperationModeFromType` produces.
///
/// A `tTVPBlendOperationMode` shares the native `tTVPLayerType` numbering
/// (`omOpaque == ltOpaque == 1`, …), so the canonical native encoding of the
/// mode (see [`super::blend::to_native`]) *is* its operation mode.
pub(super) const fn operation_mode_from_layer_type(blend: BlendMode) -> i64 {
    super::blend::to_native(blend)
}

/// Write a retained object id into `out` (the C++ conversion consumes the
/// retention).
fn set_retained_out(out: *mut Value, id: tjs::Tjs2ValueId) {
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

/// Marshal a rectangle with explicit `left`/`top`/`right`/`bottom` edges into
/// a fresh `Rect` object (or TJS `void` when absent).
pub(super) fn set_rect_ltrb_out(
    host: &Tjs2Engine,
    out: *mut Value,
    rect: Option<(i32, i32, i32, i32)>,
) {
    let Some((left, top, right, bottom)) = rect else {
        crate::native_util::set_void(out);
        return;
    };
    set_rect_fields(host, out, left, top, right, bottom);
}

fn set_rect_fields(
    host: &Tjs2Engine,
    out: *mut Value,
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
) {
    if out.is_null() {
        return;
    }
    let object = match host.eval_retained("new Rect()", "Layer.rect") {
        Ok(RetainedValue::Object(object)) => object,
        Ok(RetainedValue::Value(_)) => {
            log::warn!("Layer: `new Rect()` did not return an object");
            crate::native_util::set_void(out);
            return;
        }
        Err(error) => {
            log::warn!("Layer: cannot create the result Rect: {error}");
            crate::native_util::set_void(out);
            return;
        }
    };
    for (name, value) in [
        ("left", left),
        ("top", top),
        ("right", right),
        ("bottom", bottom),
    ] {
        if let Err(error) =
            host.set_member(object.raw_id(), name, &TjsValue::Integer(i64::from(value)))
        {
            log::warn!("Layer: cannot set the result Rect.{name}: {error}");
            crate::native_util::set_void(out);
            return;
        }
    }
    set_retained_out(out, object.raw_id());
    std::mem::forget(object);
}
