//! `ImageFunction` native class.
//!
//! Behavioral reference: the reference `ImageFunction` native
//! (`reference/cpp/core/visual/ImageFunction.cpp`). The reference class is a
//! **stateless service object**: `new ImageFunction()` allocates an empty
//! instance and every member is a *static* method whose first argument is the
//! destination `Bitmap`. This module keeps that shape — an empty instance
//! class plus the twelve `TJS_END_NATIVE_STATIC_METHOD_DECL` members — so the
//! class surface matches the reference one-for-one.
//!
//! Each method is a thin adapter over the L2 image primitives in
//! [`engine::layer_ops`] and [`engine::raster`]. The two regions the L2
//! surface does not expose directly are composed here from existing
//! primitives:
//!
//! * an axis-aligned *region* flip, built on [`engine::layer_ops::flip_lr`] /
//!   [`engine::layer_ops::flip_ud`] for the full-bitmap case and a small
//!   in-place swap otherwise;
//! * a *region* box blur, built by extracting the region into a scratch
//!   [`engine::BitmapState`], running [`engine::layer_ops::blur_bitmap_in_place`]
//!   on it and blitting it back.
//!
//! The class owns no engine state and keeps nothing in globals: every
//! persistent byte lives in the L2 [`engine::Scene`] the host attached, and
//! the text faces and host image codec come from the shared
//! [`crate::resource::ResourceResolver`].
//!
//! # Rectangle arguments and results
//!
//! The reference reads/writes `tTJSNI_Rect` instances. This crate's `Rect`
//! native exposes the same script-visible `left`/`top`/`right`/`bottom`
//! members, so [`read_rect`] reads those properties (a plain dictionary with
//! the same keys also works) and the result marshaller builds a real `Rect`
//! object. The rectangle convention is the reference's half-open
//! `[left, right) x [top, bottom)`.
//!
//! # Text drawing
//!
//! `drawText` reads the face and style from the `Font` argument and
//! rasterizes into the destination bitmap; `drawGlyph` extracts the glyph's
//! text and draws it with the resolver's default face.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use engine::layer_ops::{self, GammaAdjust, RectI};
use engine::{BitmapState, Scene, raster};
use tjs::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeMethodDef, NativeStaticMembers,
    RetainedValue, Tjs2Engine, TjsValue, VAL_INTEGER, VAL_NULL, VAL_OBJECT, VAL_REAL, VAL_STRING,
    VAL_VOID, Value,
};

use crate::native_util::{args, report_error, set_void, value_as_string};
use crate::natives::system::{EngineBinding, resolve, try_value_as_i64, value_as_bool};
use crate::resource::{TextDrawParams, draw_text_into_bitmap, glyph_text, resource_state};
use crate::{CompatCtx, CompatError};

// ---------------------------------------------------------------------------
// Reference constants (the constants native already publishes these)
// ---------------------------------------------------------------------------

/// `omOpaque` (`tTVPBlendOperationMode`).
const OM_OPAQUE: i64 = 1;
/// `omAlpha`.
const OM_ALPHA: i64 = 2;
/// `omAuto` — normalised to [`OM_ALPHA`], matching the reference.
const OM_AUTO: i64 = 128;
/// `dfAlpha` / `dfBoth`.
const DF_ALPHA: i64 = 0;
/// `dfOpaque` / `dfMain`.
const DF_OPAQUE: i64 = 1;
/// `dfMask` (accepted by `colorRect` only).
const DF_MASK: i64 = 2;
/// `dfAddAlpha`.
const DF_ADD_ALPHA: i64 = 4;

// ---------------------------------------------------------------------------
// Dispatch / marshalling
// ---------------------------------------------------------------------------

/// Resolve the bound engine for a callback and return its dispatch context.
fn bound(engine: *mut c_void) -> Result<(EngineBinding, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "ImageFunction: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved, ctx))
}

/// The attached L2 scene, mutably.
fn scene_mut<'a>(ctx: &CompatCtx) -> Result<&'a mut Scene, String> {
    // SAFETY: the host keeps the attached scene alive for the binding's
    // lifetime and the VM is single-threaded (the documented `attach_scene`
    // contract), so forming this reference is sound within the callback.
    let ptr = unsafe { ctx.services() }
        .scene()
        .ok_or_else(|| "ImageFunction: no scene attached".to_string())?;
    // SAFETY: see above.
    Ok(unsafe { &mut *ptr })
}

/// Drive one static `ImageFunction` method: resolve the binding and its
/// context, evaluate `f`, and marshal the updated region (if any) into a
/// fresh `Rect`.
fn run(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&EngineBinding, &CompatCtx, &[Value]) -> Result<Option<RectI>, String>,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "ImageFunction: compat context is not bound");
    };
    // SAFETY: the trampoline passes `argc` valid entries for this call.
    let a = unsafe { args(argv, argc) };
    match f(&resolved, &ctx, a) {
        Ok(region) => {
            set_region_out(resolved.engine(), out, region);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
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

/// Marshal an updated region into a fresh `Rect`, or TJS `void` when the
/// operation touched nothing (the reference `result->Clear()`).
fn set_region_out(host: &Tjs2Engine, out: *mut Value, region: Option<RectI>) {
    if out.is_null() {
        return;
    }
    let Some((left, top, right, bottom)) = region else {
        set_void(out);
        return;
    };
    let object = match host.eval_retained("new Rect()", "ImageFunction.rect") {
        Ok(RetainedValue::Object(object)) => object,
        Ok(RetainedValue::Value(_)) => {
            log::warn!("ImageFunction: `new Rect()` did not return an object");
            set_void(out);
            return;
        }
        Err(error) => {
            log::warn!("ImageFunction: cannot create the result Rect: {error}");
            set_void(out);
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
            log::warn!("ImageFunction: cannot set the result Rect.{name}: {error}");
            set_void(out);
            return;
        }
    }
    set_retained_out(out, object.raw_id());
    std::mem::forget(object);
}

/// `TVPToActualColor`/`argb_to_rgba`: script `0xAARRGGBB` → straight-alpha
/// `[r, g, b, a]`.
fn argb_to_rgba(color: i64) -> [u8; 4] {
    let value = color as u32;
    [
        (value >> 16) as u8,
        (value >> 8) as u8,
        value as u8,
        (value >> 24) as u8,
    ]
}

/// TJS `AsInteger` coercion with the reference's `tvtVoid` default — the
/// optional arguments convert with `(tjs_int)*param[i]`
/// (`ImageFunction.cpp:244-264` etc.), which raises on objects.
fn opt_i64(a: &[Value], index: usize, default: i64) -> Result<i64, String> {
    match a.get(index) {
        Some(v) if v.ty != VAL_VOID => try_value_as_i64(v),
        _ => Ok(default),
    }
}

/// TJS `operator bool` coercion with the reference's `tvtVoid` default.
fn opt_bool(a: &[Value], index: usize, default: bool) -> bool {
    match a.get(index) {
        Some(v) if v.ty != VAL_VOID => value_as_bool(v),
        _ => default,
    }
}

/// TJS real coercion with the reference's `tvtVoid` default — `AsReal`
/// raises on objects (`tjsVariant.h:891-901`).
fn opt_f64(a: &[Value], index: usize, default: f64) -> Result<f64, String> {
    match a.get(index) {
        Some(v) if v.ty != VAL_VOID => match v.ty {
            VAL_REAL => Ok(v.real),
            VAL_INTEGER => Ok(v.integer as f64),
            VAL_STRING => Ok(crate::natives::system::tjs_string_to_real(
                &value_as_string(v),
            )),
            _ => Err(format!("ImageFunction: argument {index} is not a number")),
        },
        _ => Ok(default),
    }
}

/// TJS real coercion for the required affine-matrix arguments — the reference
/// assigns the variants to the `tjs_real` matrix fields
/// (`mat.a = *param[2]`, `ImageFunction.cpp:273-278`), whose `AsReal`
/// raises on objects (`tjsVariant.h:891-901`).
fn try_value_as_f64(v: &Value) -> Result<f64, String> {
    match v.ty {
        VAL_INTEGER => Ok(v.integer as f64),
        VAL_REAL => Ok(v.real),
        VAL_STRING => Ok(crate::natives::system::tjs_string_to_real(
            &value_as_string(v),
        )),
        _ => Err("ImageFunction: argument is not a number".to_string()),
    }
}

/// Resolve a script value (a `Bitmap` object, or a raw id) to a scene bitmap
/// id. `Ok(None)` means the value is not a native `Bitmap`.
fn resolve_bitmap_id(host: &Tjs2Engine, value: &Value) -> Result<Option<u32>, String> {
    match value.ty {
        VAL_INTEGER if value.integer >= 0 => Ok(Some(value.integer as u32)),
        VAL_REAL if value.real >= 0.0 => Ok(Some(value.real as u32)),
        VAL_OBJECT if !value.object_handle().is_null() => {
            let object = host.retain_object_arg(value)?;
            CompatCtx::native_instance_id(host, object.raw_id(), &["Bitmap"])
        }
        _ => Ok(None),
    }
}

/// Resolve a required `Bitmap` argument.
fn bitmap_arg(host: &Tjs2Engine, value: &Value, method: &str) -> Result<u32, String> {
    // Reference: `param[0]->AsObjectClosureNoAddRef()` raises for a
    // non-object and `if(!dst) return TJS_E_INVALIDPARAM` rejects `null`
    // (`ImageFunction.cpp:533-548` and each method's identical preamble) —
    // raw integer ids are not accepted.
    if value.ty != VAL_OBJECT || value.object_handle().is_null() {
        return Err(format!(
            "ImageFunction.{method}: the argument is not a Bitmap object"
        ));
    }
    resolve_bitmap_id(host, value)?
        .ok_or_else(|| format!("ImageFunction.{method}: the argument is not a Bitmap object"))
}

/// Read an optional `Rect` argument as a half-open `RectI`. `None` is the
/// reference default: a non-object argument is skipped by the
/// `param[i]->Type() == tvtObject` guards and `null` by `if(clo.Object)`
/// (`ImageFunction.cpp:216-235`); an object that is not a native `Rect` is
/// the reference's `TJS_E_INVALIDPARAM` (`ImageFunction.cpp:220-224`).
fn read_rect(host: &Tjs2Engine, value: Option<&Value>) -> Result<Option<RectI>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.ty == VAL_NULL || value.ty != VAL_OBJECT || value.object_handle().is_null() {
        return Ok(None);
    }
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("ImageFunction: the Rect argument is invalid: {error}"))?;
    let Some(payload) = host
        .native_instance_payload(object.raw_id(), "Rect")
        .map_err(|error| format!("ImageFunction: the Rect argument is invalid: {error}"))?
    else {
        return Err("ImageFunction: the Rect argument is not a Rect object".to_string());
    };
    // SAFETY: the payload is the engine-owned `RectData` allocated by the
    // `Rect` class's create callback; it stays valid while `object` holds the
    // retained id and the single-threaded VM runs.
    let rect = unsafe { *(payload.as_ptr() as *const RectPayload) };
    Ok(Some((rect.left, rect.top, rect.right, rect.bottom)))
}

/// The engine-owned `Rect` payload layout (the `Rect` native instance from
/// `natives::rect.rs`, four `tjs_int` edges).
#[repr(C)]
#[derive(Clone, Copy)]
struct RectPayload {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

/// The whole bitmap as a half-open `RectI`.
fn full_rect(bitmap: &BitmapState) -> RectI {
    let w = bitmap.width.min(i32::MAX as u32) as i32;
    let h = bitmap.height.min(i32::MAX as u32) as i32;
    (0, 0, w, h)
}

/// Borrow the destination and source bitmaps of an operation simultaneously
/// (``dst`` and ``src`` may be the same bitmap, in which case the source is
/// snapshotted so the blend reads stable pixels).
fn with_two_bitmaps<R>(
    scene: &mut Scene,
    dst_id: u32,
    src_id: u32,
    f: impl FnOnce(&mut BitmapState, &BitmapState) -> R,
) -> Result<R, String> {
    let dst_index = scene
        .bitmaps
        .iter()
        .position(|b| b.id == dst_id)
        .ok_or_else(|| format!("ImageFunction: the destination bitmap {dst_id} does not exist"))?;
    let src_index = scene
        .bitmaps
        .iter()
        .position(|b| b.id == src_id)
        .ok_or_else(|| format!("ImageFunction: the source bitmap {src_id} does not exist"))?;
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

// ---------------------------------------------------------------------------
// Blend-mode helpers
// ---------------------------------------------------------------------------

/// Clamp `face` to the values `operateRect`/`operateStretch`/`operateAffine`
/// accept (the reference rejects the others back to `dfAlpha`).
fn normalize_draw_face(face: i64) -> i64 {
    match face {
        DF_ALPHA | DF_ADD_ALPHA | DF_OPAQUE => face,
        _ => DF_ALPHA,
    }
}

/// Clamp `face` to the values `colorRect` accepts (`dfMask` included).
fn normalize_color_face(face: i64) -> i64 {
    match face {
        DF_ALPHA | DF_ADD_ALPHA | DF_OPAQUE | DF_MASK => face,
        _ => DF_ALPHA,
    }
}

/// Map the reference `tTVPBlendOperationMode` + `tTVPDrawFace` to the
/// operation mode [`engine::layer_ops`] takes, plus whether the destination
/// alpha plane must be held.
///
/// The reference maps through `GetBltMethodFromOperationMode`; for `omOpaque`
/// the `OnAlpha`/`OnAddAlpha` methods hold the destination alpha, which the
/// engine's `omOpaque` copy does not do, so it becomes the `hold_alpha`
/// flag. The remaining modes pass through unchanged (the engine's blend table
/// is keyed by operation mode).
fn blit_method(mode: i64, face: i64) -> (i64, bool) {
    let mode = if mode == OM_AUTO { OM_ALPHA } else { mode };
    match mode {
        OM_OPAQUE => (OM_OPAQUE, normalize_draw_face(face) != DF_OPAQUE),
        _ => (mode, false),
    }
}

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

/// Normalise a rectangle so `right >= left` and `bottom >= top`.
fn normalize_rect(rect: RectI) -> RectI {
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
fn scale_rect(src: RectI, from: RectI, to: RectI) -> RectI {
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
fn clip_dest_src(dx: i32, dy: i32, src: RectI, clip: RectI) -> Option<(i32, i32, RectI)> {
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

/// In-place horizontal mirror of `rect`, using the whole-image primitive for
/// the common full-bitmap case.
fn flip_lr_region(bitmap: &mut BitmapState, rect: RectI) {
    let Some(r) = layer_ops::intersect(bitmap, rect) else {
        return;
    };
    if r == full_rect(bitmap) {
        layer_ops::flip_lr(bitmap);
        return;
    }
    let width = bitmap.width as usize;
    for y in r.1..r.3 {
        for i in 0..(r.2 - r.0) / 2 {
            let x0 = r.0 + i;
            let x1 = r.2 - 1 - i;
            let a = (y as usize * width + x0 as usize) * 4;
            let b = (y as usize * width + x1 as usize) * 4;
            for k in 0..4 {
                bitmap.rgba.swap(a + k, b + k);
            }
        }
    }
    bitmap.mark_dirty();
}

/// In-place vertical mirror of `rect`.
fn flip_ud_region(bitmap: &mut BitmapState, rect: RectI) {
    let Some(r) = layer_ops::intersect(bitmap, rect) else {
        return;
    };
    if r == full_rect(bitmap) {
        layer_ops::flip_ud(bitmap);
        return;
    }
    let width = bitmap.width as usize;
    for x in r.0..r.2 {
        for i in 0..(r.3 - r.1) / 2 {
            let y0 = r.1 + i;
            let y1 = r.3 - 1 - i;
            let a = (y0 as usize * width + x as usize) * 4;
            let b = (y1 as usize * width + x as usize) * 4;
            for k in 0..4 {
                bitmap.rgba.swap(a + k, b + k);
            }
        }
    }
    bitmap.mark_dirty();
}

/// Copy `rect` out of `src` into a new standalone bitmap (the scratch the
/// rect-limited box blur runs on).
fn extract_region(src: &BitmapState, rect: RectI) -> BitmapState {
    let width = (rect.2 - rect.0) as u32;
    let height = (rect.3 - rect.1) as u32;
    let mut out = BitmapState::new(
        width,
        height,
        vec![0u8; width as usize * height as usize * 4],
    );
    let src_width = src.width as usize;
    let dst_width = width as usize;
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            let si = (y as usize * src_width + x as usize) * 4;
            let di = ((y - rect.1) as usize * dst_width + (x - rect.0) as usize) * 4;
            out.rgba[di..di + 4].copy_from_slice(&src.rgba[si..si + 4]);
        }
    }
    out
}

/// Restore the alpha plane of `dst` (an extracted region) from `src`.
fn restore_region_alpha(src: &BitmapState, dst: &mut BitmapState, rect: RectI) {
    let src_width = src.width as usize;
    let dst_width = dst.width as usize;
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            let si = (y as usize * src_width + x as usize) * 4 + 3;
            let di = ((y - rect.1) as usize * dst_width + (x - rect.0) as usize) * 4 + 3;
            dst.rgba[di] = src.rgba[si];
        }
    }
}

/// Snapshot the alpha plane of `rect` (used to emulate the reference
/// `hold_alpha`/`bmCopyOnAlpha` for operations whose L2 primitive has no
/// `hold_alpha` argument).
fn snapshot_alpha(bitmap: &BitmapState, rect: RectI) -> Vec<u8> {
    let Some(r) = layer_ops::intersect(bitmap, rect) else {
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
fn restore_alpha(bitmap: &mut BitmapState, rect: RectI, saved: &[u8]) {
    let Some(r) = layer_ops::intersect(bitmap, rect) else {
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

// ---------------------------------------------------------------------------
// Static methods
// ---------------------------------------------------------------------------

/// Correct a clip rectangle to the destination bitmap's own dimensions (the
/// reference clamps `clipRect` to the bitmap inside `Blt`/`StretchBlt`).
fn clamp_to_bitmap(bitmap: &BitmapState, rect: RectI) -> Option<RectI> {
    layer_ops::intersect(bitmap, rect)
}

/// `ImageFunction.operateRect(dst, dleft, dtop, src, srect, cliprect, mode,
/// face, opa, hda)`.
extern "C" fn image_operate_rect(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.len() < 6 {
            return Err("ImageFunction.operateRect requires 6 arguments".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "operateRect")?;
        let dx = try_value_as_i64(&a[1])? as i32;
        let dy = try_value_as_i64(&a[2])? as i32;
        let src_id = bitmap_arg(host, &a[3], "operateRect")?;
        let src_rect = read_rect(host, a.get(4))?;
        let clip = read_rect(host, a.get(5))?;
        let mode = opt_i64(a, 6, OM_ALPHA)?;
        let face = opt_i64(a, 7, DF_ALPHA)?;
        let opa = opt_i64(a, 8, 255)?.clamp(0, 255) as u8;
        let hda = opt_bool(a, 9, false);

        let scene = scene_mut(ctx)?;
        let (dst_full, src_full) = {
            let dst = scene
                .bitmap(dst_id)
                .ok_or_else(|| format!("ImageFunction.operateRect: no bitmap {dst_id}"))?;
            let src = scene
                .bitmap(src_id)
                .ok_or_else(|| format!("ImageFunction.operateRect: no bitmap {src_id}"))?;
            (full_rect(dst), full_rect(src))
        };
        let src_rect = src_rect.unwrap_or(src_full);
        let clip = clip.unwrap_or(dst_full);
        let Some((dx, dy, src_rect)) = clip_dest_src(dx, dy, src_rect, clip) else {
            return Ok(None);
        };
        let (mode, face_hold) = blit_method(mode, face);
        with_two_bitmaps(scene, dst_id, src_id, |dst, src| {
            layer_ops::operate_rect(dst, dx, dy, src, src_rect, mode, opa, hda || face_hold);
        })?;
        let result = (
            dx,
            dy,
            dx.saturating_add(src_rect.2 - src_rect.0),
            dy.saturating_add(src_rect.3 - src_rect.1),
        );
        // `operate_rect` clips to the destination bitmap; the reported region
        // must too (the reference's `Blt` cannot update outside the image).
        let Some(result) = layer_ops::intersect_rect(dst_full, result) else {
            return Ok(None);
        };
        Ok(Some(result))
    })
}

/// `ImageFunction.operateStretch(dst, src, dstrect, srcrect, cliprect, mode,
/// face, opa, type, hda, typeopt)`.
extern "C" fn image_operate_stretch(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.len() < 2 {
            return Err("ImageFunction.operateStretch requires 2 arguments".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "operateStretch")?;
        let src_id = bitmap_arg(host, &a[1], "operateStretch")?;
        let dst_rect = read_rect(host, a.get(2))?;
        let src_rect = read_rect(host, a.get(3))?;
        let clip = read_rect(host, a.get(4))?;
        let mode = opt_i64(a, 5, OM_ALPHA)?;
        let face = opt_i64(a, 6, DF_ALPHA)?;
        let opa = opt_i64(a, 7, 255)?.clamp(0, 255) as u8;
        let stretch_type = opt_i64(a, 8, 0)?;
        let hda = opt_bool(a, 9, false);
        // `typeopt` (index 10) only selects the cubic sub-kernel in the
        // reference; the L2 resampler picks it from `stretch_type`.

        let scene = scene_mut(ctx)?;
        let (dst_full, src_full) = {
            let dst = scene
                .bitmap(dst_id)
                .ok_or_else(|| format!("ImageFunction.operateStretch: no bitmap {dst_id}"))?;
            let src = scene
                .bitmap(src_id)
                .ok_or_else(|| format!("ImageFunction.operateStretch: no bitmap {src_id}"))?;
            (full_rect(dst), full_rect(src))
        };
        let dst_rect = normalize_rect(dst_rect.unwrap_or(dst_full));
        let src_rect = src_rect.unwrap_or(src_full);
        let clip = clip.unwrap_or(dst_full);
        let Some(dest) = layer_ops::intersect_rect(dst_rect, clip) else {
            return Ok(None);
        };
        // The resampler clips to the bitmap; report the same region.
        let Some(dest) = layer_ops::intersect_rect(dst_full, dest) else {
            return Ok(None);
        };
        let src_trimmed = scale_rect(src_rect, dst_rect, dest);
        let (mode, face_hold) = blit_method(mode, face);
        with_two_bitmaps(scene, dst_id, src_id, |dst, src| {
            layer_ops::stretch_blit_mode(
                dst,
                dest,
                src,
                src_trimmed,
                stretch_type,
                mode,
                opa,
                hda || face_hold,
            );
        })?;
        Ok(Some(dest))
    })
}

/// `ImageFunction.operateAffine(dst, src, a, b, c, d, tx, ty, srcrect,
/// cliprect, affine, mode, face, opa, type, hda)`.
extern "C" fn image_operate_affine(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.len() < 11 {
            return Err("ImageFunction.operateAffine requires 11 arguments".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "operateAffine")?;
        let src_id = bitmap_arg(host, &a[1], "operateAffine")?;
        let coords = [
            try_value_as_f64(&a[2])?,
            try_value_as_f64(&a[3])?,
            try_value_as_f64(&a[4])?,
            try_value_as_f64(&a[5])?,
            try_value_as_f64(&a[6])?,
            try_value_as_f64(&a[7])?,
        ];
        let src_rect = read_rect(host, a.get(8))?;
        let clip = read_rect(host, a.get(9))?;
        let affine = opt_bool(a, 10, true);
        let mode = opt_i64(a, 11, OM_ALPHA)?;
        let face = opt_i64(a, 12, DF_ALPHA)?;
        let opa = opt_i64(a, 13, 255)?.clamp(0, 255) as u8;
        let stretch_type = opt_i64(a, 14, 0)?;
        let hda = opt_bool(a, 15, false);

        let scene = scene_mut(ctx)?;
        let (dst_full, src_full) = {
            let dst = scene
                .bitmap(dst_id)
                .ok_or_else(|| format!("ImageFunction.operateAffine: no bitmap {dst_id}"))?;
            let src = scene
                .bitmap(src_id)
                .ok_or_else(|| format!("ImageFunction.operateAffine: no bitmap {src_id}"))?;
            (full_rect(dst), full_rect(src))
        };
        let src_rect = src_rect.unwrap_or(src_full);
        let clip = clip.unwrap_or(dst_full);
        let Some(result) = layer_ops::intersect_rect(dst_full, clip) else {
            return Ok(None);
        };

        let (p0, p1, p2) = if affine {
            // Reference `AffineBlt(matrix)`: the matrix maps the source rect's
            // top-left pixel centre (-0.5, -0.5) and the top-right/bottom-left
            // centres; `t2DAffineMatrix` is `x' = a*x + c*y + tx`,
            // `y' = b*x + d*y + ty`.
            let [a_m, b_m, c_m, d_m, tx, ty] = coords;
            let width = f64::from(src_rect.2 - src_rect.0);
            let height = f64::from(src_rect.3 - src_rect.1);
            let map = |x: f64, y: f64| (a_m * x + c_m * y + tx, b_m * x + d_m * y + ty);
            (
                map(-0.5, -0.5),
                map(width - 0.5, -0.5),
                map(-0.5, height - 0.5),
            )
        } else {
            // Reference points mode: three destination points for the source
            // rect's LT/RT/LB corners.
            (
                (coords[0], coords[1]),
                (coords[2], coords[3]),
                (coords[4], coords[5]),
            )
        };
        let (mode, face_hold) = blit_method(mode, face);
        let hold_alpha = hda || face_hold;
        with_two_bitmaps(scene, dst_id, src_id, |dst, src| {
            // The L2 affine primitive has no `hold_alpha` argument; snapshot
            // and restore the result region's alpha to emulate the reference
            // `bmCopyOnAlpha`/`hda` handling.
            let saved = hold_alpha.then(|| snapshot_alpha(dst, result));
            layer_ops::affine_blit(
                dst,
                p0,
                p1,
                p2,
                src,
                src_rect,
                stretch_type,
                mode,
                opa,
                false,
                [0, 0, 0, 0],
            );
            if let Some(saved) = saved {
                restore_alpha(dst, result, &saved);
            }
        })?;
        // The reference `AffineBlt` reports the *clamped clip* as the update
        // rectangle whenever the operation is drawable.
        Ok(Some(result))
    })
}

/// `ImageFunction.flipLR(dst, rect)`.
extern "C" fn image_flip_lr(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.is_empty() {
            return Err("ImageFunction.flipLR requires 1 argument".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "flipLR")?;
        let rect = read_rect(host, a.get(1))?;
        let scene = scene_mut(ctx)?;
        let dst = scene
            .bitmap_mut(dst_id)
            .ok_or_else(|| format!("ImageFunction.flipLR: no bitmap {dst_id}"))?;
        let rect = rect.unwrap_or_else(|| full_rect(dst));
        if !rect_in_bounds(dst, rect) {
            return Err("ImageFunction.flipLR: the rectangle is outside the bitmap".into());
        }
        flip_lr_region(dst, rect);
        Ok(Some(rect))
    })
}

/// `ImageFunction.flipUD(dst, rect)`.
extern "C" fn image_flip_ud(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.is_empty() {
            return Err("ImageFunction.flipUD requires 1 argument".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "flipUD")?;
        let rect = read_rect(host, a.get(1))?;
        let scene = scene_mut(ctx)?;
        let dst = scene
            .bitmap_mut(dst_id)
            .ok_or_else(|| format!("ImageFunction.flipUD: no bitmap {dst_id}"))?;
        let rect = rect.unwrap_or_else(|| full_rect(dst));
        if !rect_in_bounds(dst, rect) {
            return Err("ImageFunction.flipUD: the rectangle is outside the bitmap".into());
        }
        flip_ud_region(dst, rect);
        Ok(Some(rect))
    })
}

/// Whether `rect` is fully inside `bitmap` (the reference flip methods reject
/// an out-of-bounds rectangle with `TVPSrcRectOutOfBitmap`).
fn rect_in_bounds(bitmap: &BitmapState, rect: RectI) -> bool {
    rect.0 >= 0
        && rect.1 >= 0
        && rect.2 <= bitmap.width as i32
        && rect.3 <= bitmap.height as i32
        && rect.2 >= rect.0
        && rect.3 >= rect.1
}

/// `ImageFunction.adjustGamma(bmp, rgamma, rfloor, rceil, ggamma, gfloor,
/// gceil, bgamma, bfloor, bceil, cliprect, isaddalpha)`.
extern "C" fn image_adjust_gamma(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.is_empty() {
            return Err("ImageFunction.adjustGamma requires 1 argument".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "adjustGamma")?;
        if a.len() == 1 {
            // Reference: a bare bitmap is accepted and does nothing.
            return Ok(None);
        }
        let data = GammaAdjust {
            r_gamma: opt_f64(a, 1, 1.0)?,
            r_floor: opt_i64(a, 2, 0)? as i32,
            r_ceil: opt_i64(a, 3, 255)? as i32,
            g_gamma: opt_f64(a, 4, 1.0)?,
            g_floor: opt_i64(a, 5, 0)? as i32,
            g_ceil: opt_i64(a, 6, 255)? as i32,
            b_gamma: opt_f64(a, 7, 1.0)?,
            b_floor: opt_i64(a, 8, 0)? as i32,
            b_ceil: opt_i64(a, 9, 255)? as i32,
        };
        let clip = read_rect(host, a.get(10))?;
        let isaddalpha = opt_bool(a, 11, false);

        let scene = scene_mut(ctx)?;
        let dst = scene
            .bitmap_mut(dst_id)
            .ok_or_else(|| format!("ImageFunction.adjustGamma: no bitmap {dst_id}"))?;
        let clip = clip.unwrap_or_else(|| full_rect(dst));
        if isaddalpha {
            // No dedicated additive-alpha gamma table is exposed through L2;
            // normalise to straight alpha, run the gamma ramp, then restore
            // the additive representation.
            layer_ops::convert_add_alpha_to_alpha(dst, clip);
            layer_ops::adjust_gamma(dst, clip, data);
            layer_ops::convert_alpha_to_add_alpha(dst, clip);
        } else {
            layer_ops::adjust_gamma(dst, clip, data);
        }
        Ok(Some(clip))
    })
}

/// `ImageFunction.doBoxBlur(bmp, xblur, yblur, clipRect, isalpha)`.
extern "C" fn image_do_box_blur(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.is_empty() {
            return Err("ImageFunction.doBoxBlur requires 1 argument".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "doBoxBlur")?;
        let xblur = opt_i64(a, 1, 1)? as i32;
        let yblur = opt_i64(a, 2, 1)? as i32;
        let clip = read_rect(host, a.get(3))?;
        let isalpha = opt_bool(a, 4, true);

        let scene = scene_mut(ctx)?;
        let dst = scene
            .bitmap_mut(dst_id)
            .ok_or_else(|| format!("ImageFunction.doBoxBlur: no bitmap {dst_id}"))?;
        let clip = clip.unwrap_or_else(|| full_rect(dst));
        if xblur == 0 && yblur == 0 {
            return Ok(None);
        }
        let Some(region) = clamp_to_bitmap(dst, clip) else {
            return Ok(None);
        };
        let mut scratch = extract_region(dst, region);
        if !isalpha {
            // `DoBoxBlur` (RGB only) holds the alpha plane; the L2 blur runs
            // over all four channels, so restore the saved alpha.
            restore_region_alpha(dst, &mut scratch, region);
        }
        layer_ops::blur_bitmap_in_place(&mut scratch, xblur.unsigned_abs(), yblur.unsigned_abs());
        let width = (region.2 - region.0) as u32;
        let height = (region.3 - region.1) as u32;
        layer_ops::blit_copy(
            dst,
            region.0,
            region.1,
            &scratch,
            (0, 0, width as i32, height as i32),
        );
        dst.mark_dirty();
        Ok(Some(clip))
    })
}

/// `ImageFunction.doGrayScale(bmp, clipRect)`.
extern "C" fn image_do_gray_scale(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.is_empty() {
            return Err("ImageFunction.doGrayScale requires 1 argument".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "doGrayScale")?;
        let clip = read_rect(host, a.get(1))?;
        let scene = scene_mut(ctx)?;
        let dst = scene
            .bitmap_mut(dst_id)
            .ok_or_else(|| format!("ImageFunction.doGrayScale: no bitmap {dst_id}"))?;
        let clip = clip.unwrap_or_else(|| full_rect(dst));
        layer_ops::do_gray_scale(dst, clip);
        Ok(Some(clip))
    })
}

/// `ImageFunction.fillRect(bmp, value, rect, isalpha, cliprect)`.
extern "C" fn image_fill_rect(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.len() < 2 {
            return Err("ImageFunction.fillRect requires 2 arguments".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "fillRect")?;
        let color = argb_to_rgba(try_value_as_i64(&a[1])?);
        let rect = read_rect(host, a.get(2))?;
        let isalpha = opt_bool(a, 3, true);
        let clip = read_rect(host, a.get(4))?;

        let scene = scene_mut(ctx)?;
        let dst = scene
            .bitmap_mut(dst_id)
            .ok_or_else(|| format!("ImageFunction.fillRect: no bitmap {dst_id}"))?;
        let rect = rect.unwrap_or_else(|| full_rect(dst));
        let clip = clip.unwrap_or_else(|| full_rect(dst));
        let Some(dest) = layer_ops::intersect_rect(rect, clip) else {
            return Ok(None);
        };
        let Some(dest) = clamp_to_bitmap(dst, dest) else {
            return Ok(None);
        };
        if isalpha {
            // Reference `Fill` (`FillARGB`): copy the colour *including* its
            // alpha. `fill_rect_replace` is that exact primitive.
            raster::fill_rect_replace(
                dst,
                dest.0,
                dest.1,
                (dest.2 - dest.0) as u32,
                (dest.3 - dest.1) as u32,
                color,
            );
            dst.mark_dirty();
        } else {
            // Reference `FillColor`: replace the RGB, hold the destination
            // alpha.
            layer_ops::fill_color_hold_alpha(dst, dest, color, 255);
        }
        Ok(Some(dest))
    })
}

/// `ImageFunction.colorRect(bmp, value, opa, rect, face, cliprect)`.
extern "C" fn image_color_rect(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.len() < 2 {
            return Err("ImageFunction.colorRect requires 2 arguments".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "colorRect")?;
        let color = argb_to_rgba(try_value_as_i64(&a[1])?);
        let opa = opt_i64(a, 2, 255)? as i32;
        let rect = read_rect(host, a.get(3))?;
        let face = normalize_color_face(opt_i64(a, 4, DF_ALPHA)?);
        let clip = read_rect(host, a.get(5))?;

        let scene = scene_mut(ctx)?;
        let dst = scene
            .bitmap_mut(dst_id)
            .ok_or_else(|| format!("ImageFunction.colorRect: no bitmap {dst_id}"))?;
        let rect = rect.unwrap_or_else(|| full_rect(dst));
        let clip = clip.unwrap_or_else(|| full_rect(dst));
        let Some(dest) = layer_ops::intersect_rect(rect, clip) else {
            return Ok(None);
        };
        let Some(dest) = clamp_to_bitmap(dst, dest) else {
            return Ok(None);
        };
        let updated = match face {
            DF_ALPHA => {
                if opa > 0 {
                    layer_ops::fill_color_on_alpha(dst, dest, color, opa);
                    true
                } else {
                    layer_ops::remove_const_opacity(dst, dest, -opa);
                    true
                }
            }
            DF_ADD_ALPHA => {
                if opa >= 0 {
                    layer_ops::fill_color_on_add_alpha(dst, dest, color, opa);
                    true
                } else {
                    false
                }
            }
            DF_OPAQUE => {
                layer_ops::fill_color_hold_alpha(dst, dest, color, opa);
                true
            }
            DF_MASK => {
                layer_ops::fill_mask(dst, dest, color[2]);
                true
            }
            _ => false,
        };
        if updated { Ok(Some(dest)) } else { Ok(None) }
    })
}

/// Resolve a required `Font` argument to its scene font id.
fn font_arg(host: &Tjs2Engine, value: &Value, method: &str) -> Result<u32, String> {
    if value.ty != VAL_OBJECT || value.object_handle().is_null() {
        return Err(format!(
            "ImageFunction.{method}: the argument is not a Font object"
        ));
    }
    let object = host.retain_object_arg(value)?;
    CompatCtx::native_instance_id(host, object.raw_id(), &["Font"])?
        .ok_or_else(|| format!("ImageFunction.{method}: the argument is not a Font object"))
}

/// `ImageFunction.drawText(bmp, font, x, y, text, color, ...)`.
extern "C" fn image_draw_text(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.len() < 6 {
            return Err("ImageFunction.drawText requires 6 arguments".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "drawText")?;
        let font_id = font_arg(host, &a[1], "drawText")?;
        let x = try_value_as_i64(&a[2])? as i32;
        let y = try_value_as_i64(&a[3])? as i32;
        let text = value_as_string(&a[4]);
        let mut color = argb_to_rgba(try_value_as_i64(&a[5])?);
        color[3] = 255;
        let opacity = opt_i64(a, 6, 255)?.clamp(0, 255) as i32;
        let antialias = opt_bool(a, 7, true);
        let shadow_level = opt_i64(a, 9, 0)? as i32;
        let mut shadow_color = argb_to_rgba(opt_i64(a, 10, 0)?);
        shadow_color[3] = 255;
        let shadow_width = opt_i64(a, 11, 0)? as i32;
        let shadow_offset = (opt_i64(a, 12, 0)? as i32, opt_i64(a, 13, 0)? as i32);
        let clip = read_rect(host, a.get(15))?;

        let (face, size_px, bold, italic, angle) = {
            let scene = scene_mut(ctx)?;
            let font = scene
                .font(font_id)
                .ok_or_else(|| format!("ImageFunction.drawText: no font {font_id}"))?;
            (
                font.face.clone(),
                font.height.unsigned_abs() as f32,
                font.bold,
                font.italic,
                font.angle,
            )
        };
        let params = TextDrawParams {
            text: &text,
            x,
            y,
            color,
            opacity,
            antialias,
            shadow_level,
            shadow_color,
            shadow_width,
            shadow_offset,
        };
        let mut resolver = resource_state(ctx);
        let scene = scene_mut(ctx)?;
        let Some(bitmap) = scene.bitmap_mut(dst_id) else {
            return Err(format!("ImageFunction.drawText: no bitmap {dst_id}"));
        };
        let clip = clip.unwrap_or_else(|| {
            (
                0,
                0,
                bitmap.width.min(i32::MAX as u32) as i32,
                bitmap.height.min(i32::MAX as u32) as i32,
            )
        });
        draw_text_into_bitmap(
            &mut resolver,
            ctx,
            bitmap,
            &face,
            size_px,
            bold,
            italic,
            angle,
            clip,
            &params,
        )
    })
}

/// `ImageFunction.drawGlyph(bmp, x, y, glyph, color, ...)`.
extern "C" fn image_draw_glyph(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    run(engine, argc, argv, out, out_error, |resolved, ctx, a| {
        if a.len() < 5 {
            return Err("ImageFunction.drawGlyph requires 5 arguments".into());
        }
        let host = resolved.engine();
        let dst_id = bitmap_arg(host, &a[0], "drawGlyph")?;
        let Some(text) = glyph_text(host, &a[3]) else {
            return Err("ImageFunction.drawGlyph: unsupported glyph argument".into());
        };
        if text.is_empty() {
            return Ok(None);
        }
        let x = try_value_as_i64(&a[1])? as i32;
        let y = try_value_as_i64(&a[2])? as i32;
        let mut color = argb_to_rgba(try_value_as_i64(&a[4])?);
        color[3] = 255;
        let opacity = opt_i64(a, 5, 255)?.clamp(0, 255) as i32;
        let antialias = opt_bool(a, 6, true);
        let shadow_level = opt_i64(a, 8, 0)? as i32;
        let mut shadow_color = argb_to_rgba(opt_i64(a, 9, 0)?);
        shadow_color[3] = 255;
        let shadow_width = opt_i64(a, 10, 0)? as i32;
        let shadow_offset = (opt_i64(a, 11, 0)? as i32, opt_i64(a, 12, 0)? as i32);
        let clip = read_rect(host, a.get(14))?;

        let mut resolver = resource_state(ctx);
        let face = resolver.default_face_name(ctx);
        if face.is_empty() {
            return Err("ImageFunction.drawGlyph: no font is registered".into());
        }
        let params = TextDrawParams {
            text: &text,
            x,
            y,
            color,
            opacity,
            antialias,
            shadow_level,
            shadow_color,
            shadow_width,
            shadow_offset,
        };
        let scene = scene_mut(ctx)?;
        let Some(bitmap) = scene.bitmap_mut(dst_id) else {
            return Err(format!("ImageFunction.drawGlyph: no bitmap {dst_id}"));
        };
        let clip = clip.unwrap_or_else(|| {
            (
                0,
                0,
                bitmap.width.min(i32::MAX as u32) as i32,
                bitmap.height.min(i32::MAX as u32) as i32,
            )
        });
        draw_text_into_bitmap(
            &mut resolver,
            ctx,
            bitmap,
            &face,
            16.0,
            false,
            false,
            0.0,
            clip,
            &params,
        )
    })
}

// ---------------------------------------------------------------------------
// Instance class (the stateless `new ImageFunction()` constructor)
// ---------------------------------------------------------------------------

/// Allocate the empty `ImageFunction` payload. The VM always calls this for
/// `new ImageFunction()`; the instance carries no state.
extern "C" fn image_function_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(0u8)) as *mut c_void
}

/// Free a payload created by [`image_function_create`].
extern "C" fn image_function_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: `instance` came from `image_function_create` and is freed
        // exactly once by the VM.
        drop(unsafe { Box::from_raw(instance as *mut u8) });
    }
}

/// `new ImageFunction()` — the reference constructor is empty.
extern "C" fn image_function_ctor(
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

/// Register the `ImageFunction` class and its static members.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "ImageFunction",
        create: image_function_create,
        destroy: image_function_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "ImageFunction",
            f: image_function_ctor,
        }],
        properties: vec![],
    })?;
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "ImageFunction",
        methods: vec![
            NativeMethodDef {
                name: "operateAffine",
                f: image_operate_affine,
            },
            NativeMethodDef {
                name: "operateRect",
                f: image_operate_rect,
            },
            NativeMethodDef {
                name: "operateStretch",
                f: image_operate_stretch,
            },
            NativeMethodDef {
                name: "flipLR",
                f: image_flip_lr,
            },
            NativeMethodDef {
                name: "flipUD",
                f: image_flip_ud,
            },
            NativeMethodDef {
                name: "adjustGamma",
                f: image_adjust_gamma,
            },
            NativeMethodDef {
                name: "doBoxBlur",
                f: image_do_box_blur,
            },
            NativeMethodDef {
                name: "doGrayScale",
                f: image_do_gray_scale,
            },
            NativeMethodDef {
                name: "fillRect",
                f: image_fill_rect,
            },
            NativeMethodDef {
                name: "colorRect",
                f: image_color_rect,
            },
            NativeMethodDef {
                name: "drawText",
                f: image_draw_text,
            },
            NativeMethodDef {
                name: "drawGlyph",
                f: image_draw_glyph,
            },
        ],
        properties: vec![],
    })?;
    Ok(())
}
