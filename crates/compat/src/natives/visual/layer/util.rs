//! Shared helpers for the `Layer` method submodules.
//!
//! These are the small marshalling / geometry / borrow routines every method
//! group needs. They live here (rather than in a process global) so the
//! `Layer` code stays a thin adapter over the L2 [`engine::Scene`].

use api::blend::BlendMode;
use engine::input::InputState;
use engine::layer_ops::RectI;
use engine::{BitmapState, Scene};
use tjs::{RetainedValue, Tjs2Engine, TjsValue, VAL_OBJECT, VAL_REAL, VAL_VOID, Value};

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
pub(super) fn value_as_f64(v: &Value) -> f64 {
    match v.ty {
        tjs::VAL_INTEGER => v.integer as f64,
        VAL_REAL => v.real,
        _ => 0.0,
    }
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

/// The whole bitmap as a half-open `RectI`.
#[allow(dead_code)]
pub(super) fn full_rect(bitmap: &BitmapState) -> RectI {
    let w = bitmap.width.min(i32::MAX as u32) as i32;
    let h = bitmap.height.min(i32::MAX as u32) as i32;
    (0, 0, w, h)
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

/// Resolve a `Layer` or `Bitmap` script argument to a scene bitmap id, plus
/// the source layer's generic blend when the argument is a `Layer` (needed
/// for the `omAuto` operation-mode default).
pub(super) fn resolve_source_bitmap(
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    value: &Value,
    method: &str,
) -> Result<(u32, Option<BlendMode>), String> {
    let id = resolve_object_id(host, value)?;
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

/// Snapshot the alpha plane of `rect` (used to emulate the reference
/// `hold_alpha` for operations whose L2 primitive has no `hold_alpha`
/// argument).
#[allow(dead_code)]
pub(super) fn snapshot_alpha(bitmap: &BitmapState, rect: RectI) -> Vec<u8> {
    let Some(r) = engine::layer_ops::intersect(bitmap, rect) else {
        return Vec::new();
    };
    let width = bitmap.width as usize;
    let mut out = Vec::with_capacity(((r.2 - r.0) * (r.3 - r.1)) as usize);
    for y in r.1..r.3 {
        for x in r.0..r.2 {
            out.push(bitmap.rgba[(y as usize * width + x as usize) * 4 + 3]);
        }
    }
    out
}

/// Restore an alpha snapshot taken by [`snapshot_alpha`].
#[allow(dead_code)]
pub(super) fn restore_alpha(bitmap: &mut BitmapState, rect: RectI, saved: &[u8]) {
    let Some(r) = engine::layer_ops::intersect(bitmap, rect) else {
        return;
    };
    let width = bitmap.width as usize;
    let mut index = 0;
    for y in r.1..r.3 {
        for x in r.0..r.2 {
            if let Some(&alpha) = saved.get(index) {
                bitmap.rgba[(y as usize * width + x as usize) * 4 + 3] = alpha;
            }
            index += 1;
        }
    }
    bitmap.mark_dirty();
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

/// Marshal a half-open `[left, right) x [top, bottom)` rectangle into a fresh
/// `Rect` object.
#[allow(dead_code)]
pub(super) fn set_region_out(host: &Tjs2Engine, out: *mut Value, rect: Option<RectI>) {
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

/// Whether a script value is a non-null object (used for optional Rect args).
#[allow(dead_code)]
pub(super) fn is_object(value: &Value) -> bool {
    value.ty == VAL_OBJECT && !value.object_handle().is_null()
}
