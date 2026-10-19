//! `Layer` image / pixel operations (group 1).
//!
//! Every method composes the L2 [`engine::layer_ops`] / [`engine::raster`]
//! primitives directly (the same ones `natives/image_function.rs` uses) — it
//! never calls into `ImageFunction`. The destination is always the layer's
//! own `MainImage`; the source is resolved from a script `Layer`/`Bitmap`
//! argument.
//!
//! Reference: `tTJSNI_BaseLayer` in `reference/cpp/core/visual/LayerIntf.cpp`
//! (`ColorRect` 4338, `CopyRect` 4574, `OperateRect` 5224, `DoBoxBlur` 5691,
//! `AdjustGamma` 5715, `ApplyGaussianBlur` 5749, `DoGrayScale` 5898,
//! `LRFlip` 5910, `UDFlip` 5925, ...).
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use api::blend::{BlendMode, DstBlend};
use engine::layer_ops::{self, GammaAdjust, RectI};
use engine::{BitmapState, DecodedImageMode, Scene};
use tjs::{NativeInstanceMethodDef, Tjs2Engine, Value};

use super::super::bitmap::{COLOR_KEY_NONE, load_cached_storage_image, replace_bitmap_pixels};
use super::super::common::scene_mut;
use super::super::common::{
    DEFAULT_FONT_FACE, DEFAULT_FONT_HEIGHT, argb_to_rgba, rgba_to_argb, set_null_out,
};
use super::LayerInst;
use super::util::{
    self, clip_dest_src, layer_clip, main_bitmap, normalize_rect, operation_mode_from_layer_type,
    opt_bool, opt_f64, opt_i64, resolve_source_bitmap, resolve_source_layer, scale_rect,
    value_as_f64, with_two_bitmaps,
};
use crate::CompatCtx;
use crate::native_util::{args, report_error, set_int, set_void};
use crate::natives::system::value_as_i64;
use crate::resource::{TextDrawParams, draw_text_into_bitmap, glyph_text, resource_state};

/// Operation modes (`tTVPBlendOperationMode`).
const OM_OPAQUE: i64 = 1;
const OM_ALPHA: i64 = 2;
const OM_AUTO: i64 = 128;
/// Draw faces (`tTVPDrawFace`, `reference/cpp/core/visual/LayerIntf.h:59-68`).
pub(super) const DF_ALPHA: i32 = 0;
pub(super) const DF_OPAQUE: i32 = 1;
pub(super) const DF_MASK: i32 = 2;
pub(super) const DF_PROVINCE: i32 = 3;
pub(super) const DF_ADD_ALPHA: i32 = 4;
/// `dfAuto` — the face is chosen automatically from the layer type
/// (`LayerIntf.h:67`).
const DF_AUTO: i32 = 128;

// ---------------------------------------------------------------------------
// Small geometry helpers
// ---------------------------------------------------------------------------

/// Extract `[left, top, right, bottom)` of `src` into a new standalone
/// bitmap (the scratch the rect-limited box blur runs on).
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

/// Restore the RGB planes of `dst` (an extracted region) from `src`.
fn restore_region_rgb(src: &BitmapState, dst: &mut BitmapState, rect: RectI) {
    let src_width = src.width as usize;
    let dst_width = dst.width as usize;
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            let si = (y as usize * src_width + x as usize) * 4;
            let di = ((y - rect.1) as usize * dst_width + (x - rect.0) as usize) * 4;
            dst.rgba[di..di + 3].copy_from_slice(&src.rgba[si..si + 3]);
        }
    }
}

/// Restore every pixel outside `clip` from `before` (used to emulate the
/// reference `ClipRect` around the L2 affine primitive, which has no clip
/// argument).
fn restore_outside_clip(dst: &mut BitmapState, before: &BitmapState, clip: RectI) {
    if dst.width != before.width || dst.height != before.height {
        return;
    }
    let width = dst.width as usize;
    for y in 0..dst.height as i32 {
        for x in 0..dst.width as i32 {
            if x >= clip.0 && x < clip.2 && y >= clip.1 && y < clip.3 {
                continue;
            }
            let i = (y as usize * width + x as usize) * 4;
            dst.rgba[i..i + 4].copy_from_slice(&before.rgba[i..i + 4]);
        }
    }
    dst.mark_dirty();
}

/// Restore the alpha plane inside `clip` from `before` (the reference
/// `HoldAlpha` for the affine primitive).
fn restore_alpha_inside_clip(dst: &mut BitmapState, before: &BitmapState, clip: RectI) {
    if dst.width != before.width || dst.height != before.height {
        return;
    }
    let width = dst.width as usize;
    let x0 = clip.0.max(0) as usize;
    let y0 = clip.1.max(0) as usize;
    let x1 = clip.2.min(dst.width as i32).max(0) as usize;
    let y1 = clip.3.min(dst.height as i32).max(0) as usize;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = (y * width + x) * 4 + 3;
            dst.rgba[i] = before.rgba[i];
        }
    }
    dst.mark_dirty();
}

/// The destination bitmap + `HoldAlpha` flag of a layer, or `None` when its
/// MainImage has been deallocated (`Layer.hasImage = false`).
fn dest_info(scene: &Scene, id: u32, method: &str) -> Result<Option<(u32, bool)>, String> {
    let layer = scene
        .layer(id)
        .ok_or_else(|| format!("Layer.{method}: this layer no longer exists"))?;
    Ok(layer.bitmap.map(|bitmap| (bitmap, layer.hold_alpha)))
}

/// Mark a layer's image modified after a pixel write. The `imageModified`
/// flag is the signal scripts read to decide whether the layer must be
/// cleared before it is drawn again.
pub(super) fn mark_image_modified(scene: &mut Scene, id: u32) {
    if let Some(layer) = scene.layer_mut(id) {
        layer.image_modified = true;
    }
}

/// The layer's effective draw face — the port of the reference
/// `UpdateDrawFace` (`reference/cpp/core/visual/LayerIntf.cpp:1525-1620`):
/// when `face == dfAuto` the face is derived from the layer type, otherwise
/// the stored face is used verbatim.
pub(super) fn draw_face(scene: &Scene, id: u32) -> i32 {
    let Some(layer) = scene.layer(id) else {
        return DF_ALPHA;
    };
    if layer.face != DF_AUTO {
        return layer.face;
    }
    blend_face(layer.blend)
}

/// `UpdateDrawFace`'s type→face table for `Face == dfAuto`
/// (`LayerIntf.cpp:1529-1618`): every Photoshop `ltPs*` type resolves to
/// `dfAlpha`, `ltAddAlpha` resolves to `dfAddAlpha`, and the remaining types
/// (`ltOpaque` and the non-Photoshop additive/subtractive/multiplicative/
/// screen/dodge/darken/lighten blends) resolve to `dfOpaque`.
const fn blend_face(blend: BlendMode) -> i32 {
    match blend {
        BlendMode::Replace => DF_OPAQUE,
        BlendMode::SourceOver
        | BlendMode::PsNormal
        | BlendMode::PsAdditive
        | BlendMode::PsSubtractive
        | BlendMode::PsMultiplicative
        | BlendMode::PsScreen => DF_ALPHA,
        BlendMode::Additive => DF_OPAQUE,
        BlendMode::Subtractive => DF_OPAQUE,
        BlendMode::Multiply => DF_OPAQUE,
        BlendMode::Screen => DF_OPAQUE,
        BlendMode::AddAlpha => DF_ADD_ALPHA,
        BlendMode::DestinationDependent(
            DstBlend::ColorDodge | DstBlend::Darken | DstBlend::Lighten,
        ) => DF_OPAQUE,
        BlendMode::DestinationDependent(
            DstBlend::ColorDodge5
            | DstBlend::ColorBurn
            | DstBlend::Difference
            | DstBlend::Difference5
            | DstBlend::Exclusion
            | DstBlend::Overlay
            | DstBlend::HardLight
            | DstBlend::SoftLight
            | DstBlend::PsColorDodge
            | DstBlend::PsDarken
            | DstBlend::PsLighten,
        ) => DF_ALPHA,
    }
}

/// `GetBltMethodFromOperationModeAndDrawFace`
/// (`reference/cpp/core/visual/LayerIntf.cpp:4118-4271`): the face-sensitive
/// modes `omAlpha` / `omAddAlpha` / `omOpaque` only know the `dfAlpha`,
/// `dfAddAlpha` and `dfOpaque` faces — any other destination face makes the
/// reference raise `TVPNotDrawableFaceType`. The face-independent modes
/// (additive, subtractive, … , every `omPs*`) accept any face.
fn check_operate_face(mode: i64, face: i32) -> Result<(), String> {
    const FACE_SENSITIVE: [i64; 3] = [OM_ALPHA, OM_OPAQUE, super::blend::LT_ADD_ALPHA];
    if FACE_SENSITIVE.contains(&mode) && !matches!(face, DF_ALPHA | DF_OPAQUE | DF_ADD_ALPHA) {
        return Err(format!(
            "Layer: the operation mode {mode} cannot draw on draw face {face}"
        ));
    }
    Ok(())
}

/// Resolve an `omAuto` operation mode against the source layer's blend.
fn resolve_mode(mode: i64, src_type: Option<BlendMode>, method: &str) -> Result<i64, String> {
    if mode != OM_AUTO {
        return Ok(mode);
    }
    match src_type {
        Some(blend) => Ok(operation_mode_from_layer_type(blend)),
        None => {
            // A Bitmap source has no layer blend; the reference keeps `omAlpha`.
            let _ = method;
            Ok(OM_ALPHA)
        }
    }
}

/// Build the three destination corners for the `affine`/points argument
/// convention shared by `affineCopy`/`affinePile`/`affineBlend`/
/// `operateAffine`.
fn affine_points(
    affine: bool,
    coords: &[Value],
    src_rect: RectI,
) -> ((f64, f64), (f64, f64), (f64, f64)) {
    let c = [
        value_as_f64(&coords[0]),
        value_as_f64(&coords[1]),
        value_as_f64(&coords[2]),
        value_as_f64(&coords[3]),
        value_as_f64(&coords[4]),
        value_as_f64(&coords[5]),
    ];
    if affine {
        let [a, b, cc, d, tx, ty] = c;
        let width = f64::from(src_rect.2 - src_rect.0);
        let height = f64::from(src_rect.3 - src_rect.1);
        let map = |x: f64, y: f64| (a * x + cc * y + tx, b * x + d * y + ty);
        (
            map(-0.5, -0.5),
            map(width - 0.5, -0.5),
            map(-0.5, height - 0.5),
        )
    } else {
        ((c[0], c[1]), (c[2], c[3]), (c[4], c[5]))
    }
}

struct AffineRun<'a> {
    scene: &'a mut Scene,
    dst_id: u32,
    src_id: u32,
    corners: [(f64, f64); 3],
    src_rect: RectI,
    stretch_type: i64,
    mode: i64,
    opa: u8,
    hold_alpha: bool,
    clear: bool,
    neutral: [u8; 4],
    clip: RectI,
}

/// Run an affine operation clipped to `clip`, emulating `HoldAlpha`.
fn affine_run(run: AffineRun<'_>) -> Result<(), String> {
    let AffineRun {
        scene,
        dst_id,
        src_id,
        corners: [p0, p1, p2],
        src_rect,
        stretch_type,
        mode,
        opa,
        hold_alpha,
        clear,
        neutral,
        clip,
    } = run;
    let before = scene.bitmap(dst_id).cloned();
    with_two_bitmaps(scene, dst_id, src_id, |dst, src| {
        layer_ops::affine_blit(
            dst,
            layer_ops::AffineBlit {
                corners: [p0, p1, p2],
                stretch_type,
                clear,
                clear_color: neutral,
            },
            layer_ops::BlitSource {
                bitmap: src,
                rect: src_rect,
            },
            layer_ops::BlitOptions {
                mode,
                opa,
                ..Default::default()
            },
        );
    })?;
    if let Some(before) = before
        && let Some(dst) = scene.bitmap_mut(dst_id)
    {
        restore_outside_clip(dst, &before, clip);
        if hold_alpha {
            restore_alpha_inside_clip(dst, &before, clip);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `operateRect` / `operateStretch` / `operateAffine`
// ---------------------------------------------------------------------------

/// `operateRect(dx, dy, src, sx, sy, sw, sh [, mode [, opa]])`.
extern "C" fn layer_operate_rect(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 7 {
        return report_error(out_error, "Layer.operateRect requires 7 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_operate_rect`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.operateRect: this layer is not constructed",
        );
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.operateRect: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let dx = value_as_i64(&a[0]) as i32;
        let dy = value_as_i64(&a[1]) as i32;
        let (src_id, src_type) =
            resolve_source_bitmap(&ctx, host.engine(), &a[2], "operateRect", false)?;
        let sx = value_as_i64(&a[3]) as i32;
        let sy = value_as_i64(&a[4]) as i32;
        let sw = value_as_i64(&a[5]) as i32;
        let sh = value_as_i64(&a[6]) as i32;
        let mode = resolve_mode(opt_i64(a, 7, OM_AUTO), src_type, "operateRect")?;
        let opa = opt_i64(a, 8, 255).clamp(0, 255) as u8;
        let scene = scene_mut(&ctx)?;
        let Some((dst, hold_alpha)) = dest_info(scene, id, "operateRect")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let src_rect = (sx, sy, sx.saturating_add(sw), sy.saturating_add(sh));
        let Some((dx, dy, src_rect)) = clip_dest_src(dx, dy, src_rect, clip) else {
            return Ok(());
        };
        // Reference order: clip first (`LayerIntf.cpp:5232-5234`), then the
        // mode/face validation (`LayerIntf.cpp:5235-5242`).
        check_operate_face(mode, face)?;
        with_two_bitmaps(scene, dst, src_id, |d, s| {
            layer_ops::operate_rect(
                d,
                dx,
                dy,
                layer_ops::BlitSource {
                    bitmap: s,
                    rect: src_rect,
                },
                layer_ops::BlitOptions {
                    mode,
                    opa,
                    hold_alpha,
                    ..Default::default()
                },
            );
        })?;
        mark_image_modified(scene, id);
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `operateStretch(dx, dy, dw, dh, src, sx, sy, sw, sh [, mode [, opa
/// [, type]]])`.
extern "C" fn layer_operate_stretch(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 9 {
        return report_error(out_error, "Layer.operateStretch requires 9 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_operate_stretch`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.operateStretch: this layer is not constructed",
        );
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "Layer.operateStretch: compat context is not bound",
        );
    };
    let result: Result<(), String> = (|| {
        let dx = value_as_i64(&a[0]) as i32;
        let dy = value_as_i64(&a[1]) as i32;
        let dw = value_as_i64(&a[2]) as i32;
        let dh = value_as_i64(&a[3]) as i32;
        let (src_id, src_type) =
            resolve_source_bitmap(&ctx, host.engine(), &a[4], "operateStretch", false)?;
        let sx = value_as_i64(&a[5]) as i32;
        let sy = value_as_i64(&a[6]) as i32;
        let sw = value_as_i64(&a[7]) as i32;
        let sh = value_as_i64(&a[8]) as i32;
        let mode = resolve_mode(opt_i64(a, 9, OM_AUTO), src_type, "operateStretch")?;
        let opa = opt_i64(a, 10, 255).clamp(0, 255) as u8;
        let stretch_type = opt_i64(a, 11, 0);
        let scene = scene_mut(&ctx)?;
        let Some((dst, hold_alpha)) = dest_info(scene, id, "operateStretch")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let dst_rect = normalize_rect((dx, dy, dx.saturating_add(dw), dy.saturating_add(dh)));
        let Some(dest) = layer_ops::intersect_rect(dst_rect, clip) else {
            return Ok(());
        };
        // Reference order: clip first (`LayerIntf.cpp:5387-5390`), then the
        // mode/face validation (`LayerIntf.cpp:5392-5399`).
        check_operate_face(mode, face)?;
        let src_rect = (sx, sy, sx.saturating_add(sw), sy.saturating_add(sh));
        let src_trimmed = scale_rect(src_rect, dst_rect, dest);
        with_two_bitmaps(scene, dst, src_id, |d, s| {
            layer_ops::stretch_blit_mode(
                d,
                dest,
                layer_ops::BlitSource {
                    bitmap: s,
                    rect: src_trimmed,
                },
                stretch_type,
                layer_ops::BlitOptions {
                    mode,
                    opa,
                    hold_alpha,
                    ..Default::default()
                },
            );
        })?;
        mark_image_modified(scene, id);
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Shared body of the four affine methods.
fn affine_op(
    argc: c_int,
    a: &[Value],
    id: u32,
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    method: &str,
    kind: AffineKind,
) -> Result<(), String> {
    if argc < 12 {
        return Err(format!("Layer.{method} requires 12 arguments"));
    }
    // Reference source classes: `affineCopy`/`operateAffine` accept Layer or
    // Bitmap (`LayerIntf.cpp:9227-9251`, `9423-9447`), `affinePile` and
    // `affineBlend` only a Layer (`LayerIntf.cpp:9299-9316`, `9361-9378`).
    let (src_id, src_type) = resolve_source_bitmap(
        ctx,
        host,
        &a[0],
        method,
        matches!(kind, AffineKind::Pile | AffineKind::Blend),
    )?;
    let sx = value_as_i64(&a[1]) as i32;
    let sy = value_as_i64(&a[2]) as i32;
    let sw = value_as_i64(&a[3]) as i32;
    let sh = value_as_i64(&a[4]) as i32;
    let affine = crate::natives::system::value_as_bool(&a[5]);
    let src_rect = (sx, sy, sx.saturating_add(sw), sy.saturating_add(sh));
    let coords = &a[6..12];
    let (p0, p1, p2) = affine_points(affine, coords, src_rect);
    let (mode, opa, stretch_type, clear, hold_alpha) = match kind {
        AffineKind::Copy => {
            let stretch_type = opt_i64(a, 12, 0);
            let clear = opt_bool(a, 13, false);
            (OM_OPAQUE, 255, stretch_type, clear, true)
        }
        AffineKind::Pile => {
            let opa = opt_i64(a, 12, 255).clamp(0, 255) as u8;
            let stretch_type = opt_i64(a, 13, 0);
            (OM_ALPHA, opa, stretch_type, false, true)
        }
        AffineKind::Blend => {
            let opa = opt_i64(a, 12, 255).clamp(0, 255) as u8;
            let stretch_type = opt_i64(a, 13, 0);
            (OM_ALPHA, opa, stretch_type, false, true)
        }
        AffineKind::Operate => {
            let mode = resolve_mode(opt_i64(a, 12, OM_AUTO), src_type, method)?;
            let opa = opt_i64(a, 13, 255).clamp(0, 255) as u8;
            let stretch_type = opt_i64(a, 14, 0);
            (mode, opa, stretch_type, false, true)
        }
    };
    let scene = scene_mut(ctx)?;
    let Some((dst, layer_hold_alpha)) = dest_info(scene, id, method)? else {
        return Ok(());
    };
    // Reference face gates: `AffineCopy` requires `dfAlpha`/`dfAddAlpha`/
    // `dfOpaque` (`LayerIntf.cpp:4733-4786`), `AffinePile`/`AffineBlend`
    // require `dfAlpha`/`dfOpaque` (`LayerIntf.cpp:5427-5431`),
    // `OperateAffine` validates the mode/face pair
    // (`LayerIntf.cpp:5626-5638`).
    let face = draw_face(scene, id);
    match kind {
        AffineKind::Copy => {
            if !matches!(face, DF_ALPHA | DF_ADD_ALPHA | DF_OPAQUE) {
                return Err(format!("Layer.{method}: unsupported draw face {face}"));
            }
        }
        AffineKind::Pile | AffineKind::Blend => {
            if !matches!(face, DF_ALPHA | DF_OPAQUE) {
                return Err(format!("Layer.{method}: unsupported draw face {face}"));
            }
        }
        AffineKind::Operate => check_operate_face(mode, face)?,
    }
    let clip = layer_clip(scene, id);
    let neutral_color = {
        let color = scene.layer(id).map_or(0, |l| l.neutral_color);
        argb_to_rgba(color)
    };
    let hold = hold_alpha && layer_hold_alpha;
    affine_run(AffineRun {
        scene,
        dst_id: dst,
        src_id,
        corners: [p0, p1, p2],
        src_rect,
        stretch_type,
        mode,
        opa,
        hold_alpha: hold,
        clear,
        neutral: neutral_color,
        clip,
    })?;
    mark_image_modified(scene, id);
    Ok(())
}

#[derive(Clone, Copy)]
enum AffineKind {
    Copy,
    Pile,
    Blend,
    Operate,
}

macro_rules! affine_method {
    ($name:ident, $method:literal, $kind:expr) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
            let a = unsafe { args(argv, argc) };
            // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live during this callback.
            let inst = unsafe { &*(instance as *const LayerInst) };
            let Some(id) = inst.id else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": this layer is not constructed"),
                );
            };
            let Ok((host, ctx)) = super::bound(engine) else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": compat context is not bound"),
                );
            };
            match affine_op(argc, a, id, &ctx, host.engine(), $method, $kind) {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

affine_method!(layer_affine_copy, "affineCopy", AffineKind::Copy);
affine_method!(layer_affine_pile, "affinePile", AffineKind::Pile);
affine_method!(layer_affine_blend, "affineBlend", AffineKind::Blend);
affine_method!(layer_operate_affine, "operateAffine", AffineKind::Operate);

// ---------------------------------------------------------------------------
// `stretchCopy` / `stretchPile` / `stretchBlend`
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum StretchKind {
    Copy,
    Pile,
    Blend,
}

fn stretch_op(
    a: &[Value],
    id: u32,
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    method: &str,
    kind: StretchKind,
) -> Result<(), String> {
    // Reference source classes: `stretchCopy` accepts Layer or Bitmap
    // (`LayerIntf.cpp:9002-9027`), `stretchPile`/`stretchBlend` only a Layer
    // (`LayerIntf.cpp:9057-9070`, `9104-9117`).
    let (src_id, _src_type) = resolve_source_bitmap(
        ctx,
        host,
        &a[4],
        method,
        matches!(kind, StretchKind::Pile | StretchKind::Blend),
    )?;
    let dx = value_as_i64(&a[0]) as i32;
    let dy = value_as_i64(&a[1]) as i32;
    let dw = value_as_i64(&a[2]) as i32;
    let dh = value_as_i64(&a[3]) as i32;
    let sx = value_as_i64(&a[5]) as i32;
    let sy = value_as_i64(&a[6]) as i32;
    let sw = value_as_i64(&a[7]) as i32;
    let sh = value_as_i64(&a[8]) as i32;
    let scene = scene_mut(ctx)?;
    let Some((dst, layer_hold_alpha)) = dest_info(scene, id, method)? else {
        return Ok(());
    };
    let face = draw_face(scene, id);
    // Reference face gates: `StretchPile`/`StretchBlend` require
    // `dfAlpha`/`dfOpaque` *before* clipping (`LayerIntf.cpp:5264-5281`,
    // `5319-5336`); `StretchCopy` validates after the clip intersection
    // (`LayerIntf.cpp:4677-4710`) and lands in its own arm below.
    if matches!(kind, StretchKind::Pile | StretchKind::Blend)
        && !matches!(face, DF_ALPHA | DF_OPAQUE)
    {
        return Err(format!("Layer.{method}: unsupported draw face {face}"));
    }
    let clip = layer_clip(scene, id);
    let dst_rect = normalize_rect((dx, dy, dx.saturating_add(dw), dy.saturating_add(dh)));
    let Some(dest) = layer_ops::intersect_rect(dst_rect, clip) else {
        return Ok(());
    };
    let src_rect = (sx, sy, sx.saturating_add(sw), sy.saturating_add(sh));
    let src_trimmed = scale_rect(src_rect, dst_rect, dest);
    match kind {
        StretchKind::Copy => {
            // `StretchCopy` throws `TVPNotDrawableFaceType` for every face
            // outside `dfAlpha`/`dfAddAlpha`/`dfOpaque`
            // (`LayerIntf.cpp:4683-4710`).
            if !matches!(face, DF_ALPHA | DF_ADD_ALPHA | DF_OPAQUE) {
                return Err(format!("Layer.{method}: unsupported draw face {face}"));
            }
            let stretch_type = opt_i64(a, 9, 0);
            let plane = if face == DF_OPAQUE && layer_hold_alpha {
                layer_ops::COPY_MAIN
            } else {
                layer_ops::COPY_MAIN | layer_ops::COPY_MASK
            };
            with_two_bitmaps(scene, dst, src_id, |d, s| {
                layer_ops::stretch_blit_plane(d, dest, s, src_trimmed, stretch_type, plane);
            })?;
        }
        StretchKind::Pile | StretchKind::Blend => {
            let opa = opt_i64(a, 9, 255).clamp(0, 255) as u8;
            let stretch_type = opt_i64(a, 10, 0);
            // `StretchBlend` on `dfOpaque` treats the source as opaque
            // (`bmCopy`, `LayerIntf.cpp:5347-5353`); everything else piles
            // source-over style.
            let mode = if matches!(kind, StretchKind::Blend) && face == DF_OPAQUE {
                OM_OPAQUE
            } else {
                OM_ALPHA
            };
            with_two_bitmaps(scene, dst, src_id, |d, s| {
                layer_ops::stretch_blit_mode(
                    d,
                    dest,
                    layer_ops::BlitSource {
                        bitmap: s,
                        rect: src_trimmed,
                    },
                    stretch_type,
                    layer_ops::BlitOptions {
                        mode,
                        opa,
                        hold_alpha: layer_hold_alpha,
                        ..Default::default()
                    },
                );
            })?;
        }
    }
    mark_image_modified(scene, id);
    Ok(())
}

macro_rules! stretch_method {
    ($name:ident, $method:literal, $kind:expr) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            if argc < 9 {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, " requires 9 arguments"),
                );
            }
            // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
            let a = unsafe { args(argv, argc) };
            // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live during this callback.
            let inst = unsafe { &*(instance as *const LayerInst) };
            let Some(id) = inst.id else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": this layer is not constructed"),
                );
            };
            let Ok((host, ctx)) = super::bound(engine) else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": compat context is not bound"),
                );
            };
            match stretch_op(a, id, &ctx, host.engine(), $method, $kind) {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

stretch_method!(layer_stretch_copy, "stretchCopy", StretchKind::Copy);
stretch_method!(layer_stretch_pile, "stretchPile", StretchKind::Pile);
stretch_method!(layer_stretch_blend, "stretchBlend", StretchKind::Blend);

// ---------------------------------------------------------------------------
// `copyRect` / `pileRect` / `blendRect` / `piledCopy` / `copy9Patch`
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum RectKind {
    Copy,
    Pile,
    Blend,
    PiledCopy,
}

fn rect_op(
    a: &[Value],
    id: u32,
    ctx: &CompatCtx,
    host: &Tjs2Engine,
    method: &str,
    kind: RectKind,
) -> Result<(), String> {
    // Reference source classes: `copyRect` accepts Layer or Bitmap
    // (`LayerIntf.cpp:8779-8811`); `pileRect`, `blendRect` and `piledCopy`
    // only a Layer (`LayerIntf.cpp:8752-8767`, `8822-8840`, `8858-8876`).
    let (src_id, _src_type) = resolve_source_bitmap(
        ctx,
        host,
        &a[2],
        method,
        matches!(kind, RectKind::Pile | RectKind::Blend | RectKind::PiledCopy),
    )?;
    // `piledCopy` reads the source layer's composited subtree, so it also
    // needs the layer id. Resolve it before the mutable scene borrow.
    let piled_src_layer = if matches!(kind, RectKind::PiledCopy) {
        Some(resolve_source_layer(ctx, host, &a[2], method)?)
    } else {
        None
    };
    let dx = value_as_i64(&a[0]) as i32;
    let dy = value_as_i64(&a[1]) as i32;
    let sx = value_as_i64(&a[3]) as i32;
    let sy = value_as_i64(&a[4]) as i32;
    let sw = value_as_i64(&a[5]) as i32;
    let sh = value_as_i64(&a[6]) as i32;
    let scene = scene_mut(ctx)?;
    let Some((dst, hold_alpha)) = dest_info(scene, id, method)? else {
        return Ok(());
    };
    let face = draw_face(scene, id);
    let clip = layer_clip(scene, id);
    let src_rect = (sx, sy, sx.saturating_add(sw), sy.saturating_add(sh));
    let opa = opt_i64(a, 7, 255).clamp(0, 255) as u8;
    match kind {
        RectKind::PiledCopy => {
            // Copy the source layer's own MainImage composited with its
            // visible children, not its bare MainImage. A layer with no own
            // image is already rejected by the resolution above.
            let src_layer = piled_src_layer
                .ok_or_else(|| format!("Layer.{method}: the source must be a Layer object"))?;
            let content = layer_ops::render_layer_subtree(scene, src_layer)
                .ok_or_else(|| format!("Layer.{method}: cannot compose the source layer"))?;
            with_two_bitmaps(scene, dst, content, |d, s| {
                layer_ops::blit_plane(
                    d,
                    dx,
                    dy,
                    s,
                    src_rect,
                    clip,
                    layer_ops::COPY_MAIN | layer_ops::COPY_MASK,
                );
            })?;
        }
        RectKind::Copy => match face {
            DF_ALPHA | DF_ADD_ALPHA => {
                with_two_bitmaps(scene, dst, src_id, |d, s| {
                    layer_ops::blit_plane(
                        d,
                        dx,
                        dy,
                        s,
                        src_rect,
                        clip,
                        layer_ops::COPY_MAIN | layer_ops::COPY_MASK,
                    );
                })?;
            }
            DF_OPAQUE => {
                let plane = if hold_alpha {
                    layer_ops::COPY_MAIN
                } else {
                    layer_ops::COPY_MAIN | layer_ops::COPY_MASK
                };
                with_two_bitmaps(scene, dst, src_id, |d, s| {
                    layer_ops::blit_plane(d, dx, dy, s, src_rect, clip, plane);
                })?;
            }
            DF_MASK => {
                with_two_bitmaps(scene, dst, src_id, |d, s| {
                    layer_ops::blit_plane(d, dx, dy, s, src_rect, clip, layer_ops::COPY_MASK);
                })?;
            }
            DF_PROVINCE => {
                clear_province_region(scene, id, (dx, dy, sx, sy, sw, sh));
            }
            // Reference `CopyRect` has no `default:` case — any other face
            // (including a resolved-away `dfAuto`) falls through silently
            // (`reference/cpp/core/visual/LayerIntf.cpp:4574-4657`).
            _ => {}
        },
        RectKind::Pile => {
            if face != DF_ALPHA && face != DF_OPAQUE {
                return Err(format!("Layer.{method}: unsupported draw face {face}"));
            }
            let treat_as_opaque = face == DF_OPAQUE;
            with_two_bitmaps(scene, dst, src_id, |d, s| {
                layer_ops::blit_over(
                    d,
                    dx,
                    dy,
                    layer_ops::BlitSource {
                        bitmap: s,
                        rect: src_rect,
                    },
                    clip,
                    layer_ops::BlitOptions {
                        opa,
                        treat_as_opaque,
                        hold_alpha,
                        ..Default::default()
                    },
                );
            })?;
        }
        RectKind::Blend => {
            if face != DF_ALPHA && face != DF_OPAQUE {
                return Err(format!("Layer.{method}: unsupported draw face {face}"));
            }
            if face == DF_OPAQUE {
                with_two_bitmaps(scene, dst, src_id, |d, s| {
                    layer_ops::blit_plane(
                        d,
                        dx,
                        dy,
                        s,
                        src_rect,
                        clip,
                        layer_ops::COPY_MAIN | layer_ops::COPY_MASK,
                    );
                })?;
            } else {
                with_two_bitmaps(scene, dst, src_id, |d, s| {
                    layer_ops::blit_over(
                        d,
                        dx,
                        dy,
                        layer_ops::BlitSource {
                            bitmap: s,
                            rect: src_rect,
                        },
                        clip,
                        layer_ops::BlitOptions {
                            opa,
                            treat_as_opaque: true,
                            hold_alpha,
                            ..Default::default()
                        },
                    );
                })?;
            }
        }
    }
    mark_image_modified(scene, id);
    Ok(())
}

/// Zero the province plane over the destination region (`copyRect` on the
/// `dfProvince` face with no province source).
fn clear_province_region(scene: &mut Scene, id: u32, args: (i32, i32, i32, i32, i32, i32)) {
    let (dx, dy, _sx, _sy, sw, sh) = args;
    let clip = scene
        .layer(id)
        .map_or(engine::layer_ops::clip_rect(None), |l| {
            engine::layer_ops::clip_rect(l.clip)
        });
    let Some((dx, dy, src_rect)) = clip_dest_src(dx, dy, (0, 0, sw, sh), clip) else {
        return;
    };
    let Some(layer) = scene.layer_mut(id) else {
        return;
    };
    let (Some(province), pw) = (layer.province.as_deref_mut(), layer.province_width) else {
        return;
    };
    let ph = layer.province_height;
    for y in dy.max(0)..dy.saturating_add(src_rect.3 - src_rect.1).min(ph as i32) {
        for x in dx.max(0)..dx.saturating_add(src_rect.2 - src_rect.0).min(pw as i32) {
            province[y as usize * pw as usize + x as usize] = 0;
        }
    }
    layer.image_modified = true;
}

macro_rules! rect_method {
    ($name:ident, $method:literal, $kind:expr) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            if argc < 7 {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, " requires 7 arguments"),
                );
            }
            // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
            let a = unsafe { args(argv, argc) };
            // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live during this callback.
            let inst = unsafe { &*(instance as *const LayerInst) };
            let Some(id) = inst.id else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": this layer is not constructed"),
                );
            };
            let Ok((host, ctx)) = super::bound(engine) else {
                return report_error(
                    out_error,
                    concat!("Layer.", $method, ": compat context is not bound"),
                );
            };
            match rect_op(a, id, &ctx, host.engine(), $method, $kind) {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(error) => report_error(out_error, &error),
            }
        }
    };
}

rect_method!(layer_copy_rect, "copyRect", RectKind::Copy);
rect_method!(layer_pile_rect, "pileRect", RectKind::Pile);
rect_method!(layer_blend_rect, "blendRect", RectKind::Blend);
rect_method!(layer_piled_copy, "piledCopy", RectKind::PiledCopy);

/// `copy9Patch(src)` → the margin `Rect` or `void`.
extern "C" fn layer_copy_9patch(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.copy9Patch requires 1 argument");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_copy_9patch`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.copy9Patch: this layer is not constructed");
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.copy9Patch: compat context is not bound");
    };
    let (src_id, _) = match resolve_source_bitmap(&ctx, host.engine(), &a[0], "copy9Patch", false) {
        Ok(v) => v,
        Err(error) => return report_error(out_error, &error),
    };
    let scene = match scene_mut(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    let dst = match main_bitmap(scene, id, "copy9Patch") {
        Ok(d) => d,
        Err(error) => return report_error(out_error, &error),
    };
    let result = with_two_bitmaps(scene, dst, src_id, layer_ops::copy_9patch);
    match result {
        Ok(margins) => {
            util::set_rect_ltrb_out(host.engine(), out, margins);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

// ---------------------------------------------------------------------------
// `colorRect`
// ---------------------------------------------------------------------------

/// `colorRect(x, y, w, h, color [, opa])`.
extern "C" fn layer_color_rect(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 5 {
        return report_error(out_error, "Layer.colorRect requires 5 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_color_rect`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.colorRect: this layer is not constructed");
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.colorRect: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let x = value_as_i64(&a[0]) as i32;
        let y = value_as_i64(&a[1]) as i32;
        let w = value_as_i64(&a[2]) as i32;
        let h = value_as_i64(&a[3]) as i32;
        let color = argb_to_rgba(value_as_i64(&a[4]));
        let opa = opt_i64(a, 5, 255).clamp(-255, 255) as i32;
        let scene = scene_mut(&ctx)?;
        let Some((dst, _hold_alpha)) = dest_info(scene, id, "colorRect")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let Some(dest) =
            layer_ops::intersect_rect((x, y, x.saturating_add(w), y.saturating_add(h)), clip)
        else {
            return Ok(());
        };
        if face == DF_PROVINCE {
            let dest = {
                let bitmap = scene
                    .bitmap(dst)
                    .ok_or_else(|| "Layer.colorRect: this layer has no main image".to_string())?;
                layer_ops::intersect(bitmap, dest)
            };
            if let Some(dest) = dest {
                fill_province_region(scene, id, dest, color[2]);
            }
            return Ok(());
        }
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.colorRect: this layer has no main image".to_string())?;
        let Some(dest) = layer_ops::intersect(bitmap, dest) else {
            return Ok(());
        };
        let modified = match face {
            DF_ALPHA => {
                if opa > 0 {
                    layer_ops::fill_color_on_alpha(bitmap, dest, color, opa);
                } else {
                    layer_ops::remove_const_opacity(bitmap, dest, -opa);
                }
                opa != 0
            }
            DF_ADD_ALPHA => {
                if opa < 0 {
                    return Err(
                        "Layer.colorRect: negative opacity is not supported on dfAddAlpha".into(),
                    );
                }
                layer_ops::fill_color_on_add_alpha(bitmap, dest, color, opa);
                opa > 0
            }
            DF_OPAQUE => {
                layer_ops::fill_color_hold_alpha(bitmap, dest, color, opa);
                opa > 0
            }
            DF_MASK => {
                layer_ops::fill_mask(bitmap, dest, color[2]);
                true
            }
            // Reference `ColorRect` switches on the resolved face with no
            // `default:` (`reference/cpp/core/visual/LayerIntf.cpp:4338-4411`):
            // an out-of-vocabulary face falls through silently.
            _ => false,
        };
        if modified {
            mark_image_modified(scene, id);
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

pub(super) fn fill_province_region(scene: &mut Scene, id: u32, dest: RectI, value: u8) {
    let Some(layer) = scene.layer_mut(id) else {
        return;
    };
    if layer.province.is_none() {
        scene.allocate_province_image(id);
    }
    let Some(layer) = scene.layer_mut(id) else {
        return;
    };
    let (Some(province), pw, ph) = (
        layer.province.as_deref_mut(),
        layer.province_width,
        layer.province_height,
    ) else {
        return;
    };
    for y in dest.1.max(0)..dest.3.min(ph as i32) {
        for x in dest.0.max(0)..dest.2.min(pw as i32) {
            province[y as usize * pw as usize + x as usize] = value;
        }
    }
    layer.image_modified = true;
}

// ---------------------------------------------------------------------------
// `convertType` / flips / blur / gamma / gray / light
// ---------------------------------------------------------------------------

/// `convertType(fromtype)`.
extern "C" fn layer_convert_type(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.convertType requires 1 argument");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_convert_type`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.convertType: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.convertType: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let fromtype = value_as_i64(&a[0]) as i32;
        let scene = scene_mut(&ctx)?;
        let Some((dst, _)) = dest_info(scene, id, "convertType")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.convertType: this layer has no main image".to_string())?;
        if face == DF_ADD_ALPHA && fromtype == DF_ALPHA {
            layer_ops::convert_alpha_to_add_alpha(bitmap, clip);
        } else if face == DF_ALPHA && fromtype == DF_ADD_ALPHA {
            layer_ops::convert_add_alpha_to_alpha(bitmap, clip);
        } else {
            return Err(
                "Layer.convertType: cannot convert the layer type in the given direction".into(),
            );
        }
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `flipLR()`.
extern "C" fn layer_flip_lr(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    flip_axis(engine, instance, out, out_error, true)
}

/// `flipUD()`.
extern "C" fn layer_flip_ud(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    flip_axis(engine, instance, out, out_error, false)
}

fn flip_axis(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    horizontal: bool,
) -> c_int {
    let method = if horizontal { "flipLR" } else { "flipUD" };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `flip_axis`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            &format!("Layer.{method}: this layer is not constructed"),
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            &format!("Layer.{method}: compat context is not bound"),
        );
    };
    let result: Result<(), String> = (|| {
        let scene = scene_mut(&ctx)?;
        let Some((dst, _)) = dest_info(scene, id, method)? else {
            return Ok(());
        };
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| format!("Layer.{method}: this layer has no main image"))?;
        if horizontal {
            layer_ops::flip_lr(bitmap);
        } else {
            layer_ops::flip_ud(bitmap);
        }
        flip_province(scene, id, horizontal);
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

fn flip_province(scene: &mut Scene, id: u32, horizontal: bool) {
    let Some(layer) = scene.layer_mut(id) else {
        return;
    };
    let (Some(province), pw, ph) = (
        layer.province.as_deref_mut(),
        layer.province_width as usize,
        layer.province_height as usize,
    ) else {
        return;
    };
    if horizontal {
        for y in 0..ph {
            for x in 0..pw / 2 {
                province.swap(y * pw + x, y * pw + (pw - 1 - x));
            }
        }
    } else {
        for y in 0..ph / 2 {
            for x in 0..pw {
                province.swap(y * pw + x, (ph - 1 - y) * pw + x);
            }
        }
    }
}

/// `doBoxBlur([xblur [, yblur]])`.
extern "C" fn layer_do_box_blur(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_do_box_blur`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.doBoxBlur: this layer is not constructed");
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.doBoxBlur: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let xblur = opt_i64(a, 0, 1) as i32;
        let yblur = opt_i64(a, 1, 1) as i32;
        if xblur == 0 && yblur == 0 {
            return Ok(());
        }
        let scene = scene_mut(&ctx)?;
        let Some((dst, _)) = dest_info(scene, id, "doBoxBlur")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.doBoxBlur: this layer has no main image".to_string())?;
        let Some(region) = layer_ops::intersect(bitmap, clip) else {
            return Ok(());
        };
        let mut scratch = extract_region(bitmap, region);
        if face != DF_ALPHA {
            // `DoBoxBlur` blurs RGB only; hold the alpha plane.
            restore_region_alpha(bitmap, &mut scratch, region);
        }
        layer_ops::blur_bitmap_in_place(&mut scratch, xblur.unsigned_abs(), yblur.unsigned_abs());
        if face == DF_ALPHA {
            // `DoBoxBlurForAlpha` blurs alpha only; hold the RGB planes.
            restore_region_rgb(bitmap, &mut scratch, region);
        }
        let width = (region.2 - region.0) as u32;
        let height = (region.3 - region.1) as u32;
        layer_ops::blit_copy(
            bitmap,
            region.0,
            region.1,
            &scratch,
            (0, 0, width as i32, height as i32),
        );
        bitmap.mark_dirty();
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `adjustGamma(rgamma, rfloor, rceil, ggamma, gfloor, gceil, bgamma,
/// bfloor, bceil)`.
extern "C" fn layer_adjust_gamma(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_adjust_gamma`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.adjustGamma: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.adjustGamma: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        if argc == 0 {
            return Ok(());
        }
        let data = GammaAdjust {
            r_gamma: opt_f64(a, 0, 1.0),
            r_floor: opt_i64(a, 1, 0) as i32,
            r_ceil: opt_i64(a, 2, 255) as i32,
            g_gamma: opt_f64(a, 3, 1.0),
            g_floor: opt_i64(a, 4, 0) as i32,
            g_ceil: opt_i64(a, 5, 255) as i32,
            b_gamma: opt_f64(a, 6, 1.0),
            b_floor: opt_i64(a, 7, 0) as i32,
            b_ceil: opt_i64(a, 8, 255) as i32,
        };
        let scene = scene_mut(&ctx)?;
        let Some((dst, _)) = dest_info(scene, id, "adjustGamma")? else {
            return Ok(());
        };
        let face = draw_face(scene, id);
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.adjustGamma: this layer has no main image".to_string())?;
        if face == DF_ADD_ALPHA {
            layer_ops::convert_add_alpha_to_alpha(bitmap, clip);
            layer_ops::adjust_gamma(bitmap, clip, data);
            layer_ops::convert_alpha_to_add_alpha(bitmap, clip);
        } else {
            layer_ops::adjust_gamma(bitmap, clip, data);
        }
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `gaussianBlur(radius [, sigma])`.
extern "C" fn layer_gaussian_blur(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.gaussianBlur requires a radius");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_gaussian_blur`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.gaussianBlur: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.gaussianBlur: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let radius = opt_i64(a, 0, 1) as i32;
        if radius <= 0 {
            return Err("Layer.gaussianBlur: radius must be positive".into());
        }
        let sigma = match a.get(1) {
            Some(v) if v.ty != tjs::VAL_VOID => value_as_f64(v) as f32,
            // Reference: a *present* but `void` second argument keeps the
            // `1.0f` default; only a *missing* argument derives
            // `max(1.0f, radius / 2.5f)`
            // (`reference/cpp/core/visual/LayerIntf.cpp:9584-9597`).
            Some(_) => 1.0,
            None => (radius as f32 / 2.5).max(1.0),
        };
        if sigma <= 0.0 {
            return Err("Layer.gaussianBlur: sigma must be positive".into());
        }
        let scene = scene_mut(&ctx)?;
        let has_image = scene
            .layer(id)
            .ok_or_else(|| "Layer.gaussianBlur: this layer no longer exists".to_string())?
            .bitmap
            .is_some();
        if !has_image {
            // Reference `ApplyGaussianBlur` logs and returns without an
            // error when the layer cannot have or does not have an image
            // (`reference/cpp/core/visual/LayerIntf.cpp:5749-5755`).
            return Ok(());
        }
        let Some((dst, _)) = dest_info(scene, id, "gaussianBlur")? else {
            return Ok(());
        };
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.gaussianBlur: this layer has no main image".to_string())?;
        layer_ops::gaussian_blur(bitmap, clip, radius, sigma);
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `doGrayScale()`.
extern "C" fn layer_do_gray_scale(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_do_gray_scale`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.doGrayScale: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.doGrayScale: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let scene = scene_mut(&ctx)?;
        let Some((dst, _)) = dest_info(scene, id, "doGrayScale")? else {
            return Ok(());
        };
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.doGrayScale: this layer has no main image".to_string())?;
        layer_ops::do_gray_scale(bitmap, clip);
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `light(brightness, contrast)`.
extern "C" fn layer_light(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.light requires 2 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_light`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.light: this layer is not constructed");
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.light: compat context is not bound");
    };
    let result: Result<(), String> = (|| {
        let brightness = opt_i64(a, 0, 0) as i32;
        let contrast = opt_i64(a, 1, 0) as i32;
        let scene = scene_mut(&ctx)?;
        let has_image = scene
            .layer(id)
            .ok_or_else(|| "Layer.light: this layer no longer exists".to_string())?
            .bitmap
            .is_some();
        if !has_image {
            // Reference `ApplyLightContrast` logs and returns without an
            // error when the layer cannot have or does not have an image
            // (`reference/cpp/core/visual/LayerIntf.cpp:1006-1025`).
            return Ok(());
        }
        let Some((dst, _)) = dest_info(scene, id, "light")? else {
            return Ok(());
        };
        let clip = layer_clip(scene, id);
        let bitmap = scene
            .bitmap_mut(dst)
            .ok_or_else(|| "Layer.light: this layer has no main image".to_string())?;
        layer_ops::light_contrast(bitmap, clip, brightness, contrast);
        if let Some(layer) = scene.layer_mut(id) {
            layer.image_modified = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

// ---------------------------------------------------------------------------
// Pixel accessors / province / bitmap copy / dump
// ---------------------------------------------------------------------------

/// `getMainPixel(x, y)` → ARGB with the alpha masked out.
extern "C" fn layer_get_main_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.getMainPixel requires 2 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_get_main_pixel`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.getMainPixel: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.getMainPixel: compat context is not bound");
    };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let scene = match super::super::common::scene_ref(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    let value = main_bitmap(scene, id, "getMainPixel")
        .ok()
        .and_then(|dst| scene.bitmap(dst))
        .and_then(|b| read_pixel(b, x, y))
        .map_or(0, |px| rgba_to_argb(px) & 0x00ff_ffff);
    set_int(out, value);
    0
}

/// `setMainPixel(x, y, color)` (RGB written, alpha held).
extern "C" fn layer_set_main_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "Layer.setMainPixel requires 3 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_main_pixel`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setMainPixel: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.setMainPixel: compat context is not bound");
    };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let color = argb_to_rgba(value_as_i64(&a[2]));
    let scene = match scene_mut(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    let dst = match main_bitmap(scene, id, "setMainPixel") {
        Ok(d) => d,
        Err(error) => return report_error(out_error, &error),
    };
    let clip = layer_clip(scene, id);
    if x < clip.0 || y < clip.1 || x >= clip.2 || y >= clip.3 {
        set_void(out);
        return 0;
    }
    if let Some(bitmap) = scene.bitmap_mut(dst)
        && let Some(offset) = bitmap.pixel_offset(x as u32, y as u32)
    {
        bitmap.rgba[offset] = color[0];
        bitmap.rgba[offset + 1] = color[1];
        bitmap.rgba[offset + 2] = color[2];
        bitmap.mark_dirty();
    }
    if let Some(layer) = scene.layer_mut(id) {
        layer.image_modified = true;
    }
    set_void(out);
    0
}

/// `getMaskPixel(x, y)` → alpha.
extern "C" fn layer_get_mask_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Layer.getMaskPixel requires 2 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_get_mask_pixel`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.getMaskPixel: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.getMaskPixel: compat context is not bound");
    };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    let scene = match super::super::common::scene_ref(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    let value = main_bitmap(scene, id, "getMaskPixel")
        .ok()
        .and_then(|dst| scene.bitmap(dst))
        .and_then(|b| read_pixel(b, x, y))
        .map_or(0, |px| i64::from(px[3]));
    set_int(out, value);
    0
}

/// `setMaskPixel(x, y, alpha)`.
extern "C" fn layer_set_mask_pixel(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "Layer.setMaskPixel requires 3 arguments");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_set_mask_pixel`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.setMaskPixel: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.setMaskPixel: compat context is not bound");
    };
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    // Reference `SetPointMask` stores `(mask & 0xff)`
    // (`reference/cpp/core/visual/LayerBitmapIntf.cpp:210-220`), so the
    // script value is truncated to its low eight bits, not clamped.
    let mask = value_as_i64(&a[2]) as u8;
    let scene = match scene_mut(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    let dst = match main_bitmap(scene, id, "setMaskPixel") {
        Ok(d) => d,
        Err(error) => return report_error(out_error, &error),
    };
    let clip = layer_clip(scene, id);
    if x < clip.0 || y < clip.1 || x >= clip.2 || y >= clip.3 {
        set_void(out);
        return 0;
    }
    if let Some(bitmap) = scene.bitmap_mut(dst)
        && let Some(offset) = bitmap.pixel_offset(x as u32, y as u32)
    {
        bitmap.rgba[offset + 3] = mask;
        bitmap.mark_dirty();
    }
    if let Some(layer) = scene.layer_mut(id) {
        layer.image_modified = true;
    }
    set_void(out);
    0
}

/// Read one bitmap pixel as straight-alpha RGBA, or `None` out of bounds.
fn read_pixel(bitmap: &BitmapState, x: i32, y: i32) -> Option<[u8; 4]> {
    let offset = bitmap.pixel_offset(u32::try_from(x).ok()?, u32::try_from(y).ok()?)?;
    Some([
        bitmap.rgba[offset],
        bitmap.rgba[offset + 1],
        bitmap.rgba[offset + 2],
        bitmap.rgba[offset + 3],
    ])
}

/// `independProvinceImage([copy])`.
extern "C" fn layer_independ_province_image(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let copy = if argc >= 1 && a[0].ty != tjs::VAL_VOID {
        crate::natives::system::value_as_bool(&a[0])
    } else {
        true
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_independ_province_image`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.independProvinceImage: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "Layer.independProvinceImage: compat context is not bound",
        );
    };
    let result: Result<(), String> = (|| {
        let scene = scene_mut(&ctx)?;
        if !copy {
            scene.deallocate_province_image(id);
        }
        scene.allocate_province_image(id);
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `copyToBitmapFromMainImage(bitmap)`.
extern "C" fn layer_copy_to_bitmap_from_main_image(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(
            out_error,
            "Layer.copyToBitmapFromMainImage requires a Bitmap",
        );
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_copy_to_bitmap_from_main_image`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.copyToBitmapFromMainImage: this layer is not constructed",
        );
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "Layer.copyToBitmapFromMainImage: compat context is not bound",
        );
    };
    let result: Result<(), String> = (|| {
        let dst_id =
            match super::super::common::resolve_object_id(host.engine(), &a[0], &["Bitmap"]) {
                Ok(id) if id >= 0 => id as u32,
                _ => {
                    return Err(
                        "Layer.copyToBitmapFromMainImage: the argument is not a Bitmap".into(),
                    );
                }
            };
        let scene = scene_mut(&ctx)?;
        let src_id = main_bitmap(scene, id, "copyToBitmapFromMainImage")?;
        let (width, height, rgba) = {
            let src = scene
                .bitmap(src_id)
                .ok_or_else(|| "Layer.copyToBitmapFromMainImage: no source image".to_string())?;
            (src.width, src.height, src.rgba.clone())
        };
        if scene.bitmap(dst_id).is_none() {
            return Err(format!(
                "Layer.copyToBitmapFromMainImage: the destination bitmap {dst_id} does not exist"
            ));
        }
        scene.resize_bitmap(dst_id, width, height, [0, 0, 0, 0]);
        if let Some(bitmap) = scene.bitmap_mut(dst_id) {
            bitmap.rgba = rgba;
            bitmap.dirty = true;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// `dump()` — log the layer structure (the reference `DumpStructure`).
extern "C" fn layer_dump(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_dump`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.dump: this layer is not constructed");
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.dump: compat context is not bound");
    };
    let scene = match super::super::common::scene_ref(&ctx) {
        Ok(s) => s,
        Err(error) => return report_error(out_error, &error),
    };
    if let Some(layer) = scene.layer(id) {
        log::debug!(
            "Layer.dump: id={} window={} parent={:?} rect=({},{},{}x{}) visible={} \
             enabled={} focusable={} name={:?} bitmap={:?} children={:?}",
            layer.id,
            layer.window,
            layer.parent,
            layer.rect.x,
            layer.rect.y,
            layer.rect.w,
            layer.rect.h,
            layer.visible,
            layer.enabled,
            layer.focusable,
            layer.name,
            layer.bitmap,
            layer.children,
        );
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Deferred members (typed errors, never silent stubs)
// ---------------------------------------------------------------------------

/// `loadImages(name [, key])` — load the layer's main image (reference
/// `tTJSNI_BaseLayer::LoadImages`). The bytes are read through the attached
/// storage (probing image extensions), decoded by the host image codec, and
/// installed as the layer's bitmap with the image size set. Returns void:
/// the codec reports no per-format metainfo, so the reference's optional
/// metainfo dictionary is absent.
extern "C" fn layer_load_images(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.loadImages requires 1 argument");
    }
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_load_images`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Layer.loadImages: this layer is not constructed");
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(out_error, "Layer.loadImages: compat context is not bound");
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let name = crate::native_util::value_as_string(&a[0]);
    let colorkey = a
        .get(1)
        .filter(|value| value.ty != tjs::VAL_VOID)
        .map_or(COLOR_KEY_NONE, |value| value_as_i64(value) as u32);
    match load_layer_image(&ctx, id, &name, colorkey) {
        Ok(()) => {
            // The reference returns the graphic metainfo, which is `null`
            // when the format carries none (the common case). Returning
            // `void` would make the game's `ret != null` guard pass and then
            // fault on `ret.mode`.
            set_null_out(host.engine(), out);
            0
        }
        Err(error) => report_error(out_error, &format!("Layer.loadImages: {error}")),
    }
}

/// Reads, decodes and installs `name` as the layer's main image, reusing the
/// layer's existing bitmap so repeated loads do not leak scene bitmaps.
fn load_layer_image(
    ctx: &CompatCtx,
    layer_id: u32,
    name: &str,
    colorkey: u32,
) -> Result<(), String> {
    log::debug!("load_layer_image: layer {layer_id} <- {name:?}");
    let image = load_cached_storage_image(ctx, name, colorkey, DecodedImageMode::Full)?;
    let (width, height) = (image.width, image.height);
    let rgba = image.rgba;
    let scene = scene_mut(ctx)?;
    if scene.layer(layer_id).is_none() {
        return Err("the layer no longer exists".to_string());
    }
    match scene.layer(layer_id).and_then(|layer| layer.bitmap) {
        Some(id) => replace_bitmap_pixels(scene, id, width, height, rgba),
        None => {
            let id = scene.add_bitmap(width, height, rgba);
            if let Some(layer) = scene.layer_mut(layer_id) {
                layer.bitmap = Some(id);
            }
        }
    }
    if let Some(layer) = scene.layer_mut(layer_id) {
        layer.image_width = width;
        layer.image_height = height;
        layer.image_modified = true;
    }
    Ok(())
}

/// `loadProvinceImage(name)` — load an 8-bit province (mask) plane matching
/// the main image's size (reference `tTJSNI_BaseLayer::LoadProvinceImage`).
extern "C" fn layer_load_province_image(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.loadProvinceImage requires 1 argument");
    }
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_load_province_image`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.loadProvinceImage: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "Layer.loadProvinceImage: compat context is not bound",
        );
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let name = crate::native_util::value_as_string(&a[0]);
    match load_layer_province(&ctx, id, &name) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &format!("Layer.loadProvinceImage: {error}")),
    }
}

/// Reads and installs `name` as the layer's province plane. The decoded image
/// must match the main image's size (reference `TVPProvinceSizeMismatch`);
/// its red channel becomes the 8-bit province value.
fn load_layer_province(ctx: &CompatCtx, layer_id: u32, name: &str) -> Result<(), String> {
    let image = load_cached_storage_image(ctx, name, COLOR_KEY_NONE, DecodedImageMode::Province)?;
    let scene = scene_mut(ctx)?;
    let (main_w, main_h) = scene
        .layer(layer_id)
        .and_then(|layer| layer.bitmap)
        .and_then(|id| scene.bitmap(id))
        .map(|bitmap| (bitmap.width, bitmap.height))
        .ok_or_else(|| "the layer has no main image".to_string())?;
    if image.width != main_w || image.height != main_h {
        return Err(format!(
            "the province image is {}x{} but the main image is {main_w}x{main_h}",
            image.width, image.height,
        ));
    }
    let plane: Vec<u8> = image.rgba.chunks_exact(4).map(|pixel| pixel[0]).collect();
    scene.resize_province_image(layer_id, main_w, main_h);
    if let Some(layer) = scene.layer_mut(layer_id)
        && let Some(province) = layer.province.as_mut()
    {
        province.copy_from_slice(&plane);
        layer.image_modified = true;
    }
    Ok(())
}

/// `saveLayerImage(name [, type])` — encode the layer's main image through
/// the host image codec and write it through the attached storage.
extern "C" fn layer_save_layer_image(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Layer.saveLayerImage requires a name");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let name = crate::native_util::value_as_string(&a[0]);
    let type_name = match a.get(1) {
        Some(value) if value.ty != tjs::VAL_VOID => {
            let text = crate::native_util::value_as_string(value);
            if text.is_empty() {
                "bmp".to_string()
            } else {
                text
            }
        }
        _ => "bmp".to_string(),
    };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_save_layer_image`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            "Layer.saveLayerImage: this layer is not constructed",
        );
    };
    let Ok((_host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            "Layer.saveLayerImage: compat context is not bound",
        );
    };
    let result: Result<(), String> = (|| {
        let scene = scene_mut(&ctx)?;
        let bitmap_id = scene
            .layer(id)
            .and_then(|layer| layer.bitmap)
            .ok_or_else(|| "unsupported layer type (no main image)".to_string())?;
        let bitmap = scene
            .bitmap(bitmap_id)
            .ok_or_else(|| "Layer.saveLayerImage: this layer has no main image".to_string())?;
        let (width, height, rgba) = (bitmap.width, bitmap.height, bitmap.rgba.clone());
        resource_state(&ctx).save_image(&name, &type_name, width, height, &rgba)
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &format!("Layer.saveLayerImage: {error}")),
    }
}

/// `drawText(...)` — draw a text run into the layer's main image.
extern "C" fn layer_draw_text(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_draw_text_common(
        engine,
        instance,
        argc,
        argv,
        out,
        out_error,
        TextKind {
            method: "drawText",
            glyph: false,
        },
    )
}

/// `drawGlyph(...)` — draw the glyph's text into the layer's main image.
extern "C" fn layer_draw_glyph(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    layer_draw_text_common(
        engine,
        instance,
        argc,
        argv,
        out,
        out_error,
        TextKind {
            method: "drawGlyph",
            glyph: true,
        },
    )
}

struct TextKind<'a> {
    method: &'a str,
    glyph: bool,
}

/// Shared body of `drawText`/`drawGlyph` (the glyph argument only differs in
/// how the text is extracted).
fn layer_draw_text_common(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    kind: TextKind<'_>,
) -> c_int {
    let TextKind { method, glyph } = kind;
    if argc < 4 {
        return report_error(out_error, &format!("Layer.{method} requires 4 arguments"));
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live `LayerInst` payload the VM allocated for this script object; no `&mut` to it is live in `layer_draw_text_common`.
    let inst = unsafe { &*(instance as *const LayerInst) };
    let Some(id) = inst.id else {
        return report_error(
            out_error,
            &format!("Layer.{method}: this layer is not constructed"),
        );
    };
    let Ok((host, ctx)) = super::bound(engine) else {
        return report_error(
            out_error,
            &format!("Layer.{method}: compat context is not bound"),
        );
    };
    let text = if glyph {
        match glyph_text(host.engine(), &a[2]) {
            Some(text) => text,
            None => {
                return report_error(
                    out_error,
                    &format!("Layer.{method}: unsupported glyph argument"),
                );
            }
        }
    } else {
        crate::native_util::value_as_string(&a[2])
    };
    let result: Result<(), String> = (|| {
        let x = value_as_i64(&a[0]) as i32;
        let y = value_as_i64(&a[1]) as i32;
        let mut color = argb_to_rgba(value_as_i64(&a[3]));
        color[3] = 255;
        let opacity = opt_i64(a, 4, 255).clamp(0, 255) as i32;
        let antialias = opt_bool(a, 5, true);
        let shadow_level = opt_i64(a, 6, 0) as i32;
        let mut shadow_color = argb_to_rgba(opt_i64(a, 7, 0));
        shadow_color[3] = 255;
        let shadow_width = opt_i64(a, 8, 0) as i32;
        let shadow_offset = (opt_i64(a, 9, 0) as i32, opt_i64(a, 10, 0) as i32);
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
        let (bitmap_id, face, size_px, bold, italic, angle, clip) = {
            let scene = scene_mut(&ctx)?;
            let bitmap_id = ensure_layer_bitmap(scene, id, method)?;
            let layer = scene
                .layer(id)
                .ok_or_else(|| format!("Layer.{method}: this layer no longer exists"))?;
            let font = layer.font_id.and_then(|font_id| scene.font(font_id));
            let face = font
                .map(|font| font.face.clone())
                .filter(|face| !face.is_empty())
                .unwrap_or_else(|| DEFAULT_FONT_FACE.to_string());
            let size_px = font
                .map(|font| font.height.unsigned_abs() as f32)
                .filter(|size| *size > 0.0)
                .unwrap_or(DEFAULT_FONT_HEIGHT as f32);
            let bold = font.is_some_and(|font| font.bold);
            let italic = font.is_some_and(|font| font.italic);
            let angle = font.map_or(0.0, |font| font.angle);
            let clip = layer_clip(scene, id);
            (bitmap_id, face, size_px, bold, italic, angle, clip)
        };
        let ink = {
            let mut resolver = resource_state(&ctx);
            let scene = scene_mut(&ctx)?;
            let bitmap = scene
                .bitmap_mut(bitmap_id)
                .ok_or_else(|| format!("Layer.{method}: this layer has no main image"))?;
            draw_text_into_bitmap(
                &mut resolver,
                &ctx,
                bitmap,
                crate::resource::FaceSpec {
                    face: &face,
                    size_px,
                    bold,
                    italic,
                    angle_tenths: angle,
                },
                clip,
                &params,
            )?
        };
        if ink.is_some() {
            let scene = scene_mut(&ctx)?;
            if let Some(layer) = scene.layer_mut(id) {
                layer.image_modified = true;
            }
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Ensure the layer has a main-image bitmap, allocating a transparent one
/// sized from the layer rectangle when it does not. Returns the bitmap id.
fn ensure_layer_bitmap(scene: &mut Scene, layer_id: u32, method: &str) -> Result<u32, String> {
    let (width, height) = {
        let layer = scene
            .layer(layer_id)
            .ok_or_else(|| format!("Layer.{method}: this layer no longer exists"))?;
        if let Some(id) = layer.bitmap {
            return Ok(id);
        }
        (layer.rect.w, layer.rect.h)
    };
    if width == 0 || height == 0 {
        return Err(format!(
            "Layer.{method}: unsupported layer type (no main image)"
        ));
    }
    if !scene.ensure_layer_image(layer_id, width, height) {
        return Err(format!("Layer.{method}: the layer rectangle is too large"));
    }
    scene
        .layer(layer_id)
        .and_then(|layer| layer.bitmap)
        .ok_or_else(|| format!("Layer.{method}: the layer has no main image"))
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Every group-1 member as a `NativeInstanceMethodDef`.
pub(super) fn method_defs() -> Vec<NativeInstanceMethodDef> {
    vec![
        d("operateAffine", layer_operate_affine),
        d("operateRect", layer_operate_rect),
        d("operateStretch", layer_operate_stretch),
        d("affineCopy", layer_affine_copy),
        d("affineBlend", layer_affine_blend),
        d("affinePile", layer_affine_pile),
        d("stretchCopy", layer_stretch_copy),
        d("stretchBlend", layer_stretch_blend),
        d("stretchPile", layer_stretch_pile),
        d("pileRect", layer_pile_rect),
        d("piledCopy", layer_piled_copy),
        d("blendRect", layer_blend_rect),
        d("colorRect", layer_color_rect),
        d("copyRect", layer_copy_rect),
        d("copy9Patch", layer_copy_9patch),
        d(
            "copyToBitmapFromMainImage",
            layer_copy_to_bitmap_from_main_image,
        ),
        d("convertType", layer_convert_type),
        d("flipLR", layer_flip_lr),
        d("flipUD", layer_flip_ud),
        d("doBoxBlur", layer_do_box_blur),
        d("gaussianBlur", layer_gaussian_blur),
        d("adjustGamma", layer_adjust_gamma),
        d("doGrayScale", layer_do_gray_scale),
        d("light", layer_light),
        d("independProvinceImage", layer_independ_province_image),
        d("loadImages", layer_load_images),
        d("loadProvinceImage", layer_load_province_image),
        d("saveLayerImage", layer_save_layer_image),
        d("getMainPixel", layer_get_main_pixel),
        d("setMainPixel", layer_set_main_pixel),
        d("getMaskPixel", layer_get_mask_pixel),
        d("setMaskPixel", layer_set_mask_pixel),
        d("dump", layer_dump),
        d("drawText", layer_draw_text),
        d("drawGlyph", layer_draw_glyph),
    ]
}

fn d(name: &'static str, f: tjs::NativeInstanceMethodFn) -> NativeInstanceMethodDef {
    NativeInstanceMethodDef { name, f }
}
