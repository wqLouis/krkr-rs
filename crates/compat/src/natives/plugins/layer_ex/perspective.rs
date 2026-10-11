//! `LayerExPerspective`: the `Layer.perspectiveCopy` extension.
//!
//! Reference: `reference/cpp/plugins/layerExPerspective.cpp`. The GL build
//! registers `perspectiveCopy` on the `Layer` class; the (compiled-out)
//! software build used an Anti-Grain projective span renderer. This port
//! implements the same operation with the L2 CPU raster primitives: an
//! inverse homography maps every destination pixel back into the source
//! rectangle, the source is bilinearly sampled, and the result is composited
//! with [`engine::raster::blend_pixel`] (the reference
//! `PerspectiveAlphaBlend_a`, opacity 255).
//!
//! Argument order (reference `PerspectiveCopy_GL`):
//!
//! ```text
//! perspectiveCopy(src, sleft, stop, swidth, sheight,
//!                 x1, y1, x2, y2, x3, y3, x4, y4)
//! ```
//!
//! where `(x1,y1)` is the destination top-left, `(x2,y2)` the top-right,
//! `(x3,y3)` the bottom-left and `(x4,y4)` the bottom-right corner, and the
//! source rectangle is `[sleft, sleft+swidth) x [stop, stop+sheight)`.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use engine::{BitmapState, layer_ops};
use tjs::{NativeObjThisMethodDef, Tjs2Engine, VAL_OBJECT, Value};

use super::{bound, layer_from_objthis, scene_mut};
use crate::CompatCtx;
use crate::native_util::{args, report_error, set_void};

// ---------------------------------------------------------------------------
// Projective sampling
// ---------------------------------------------------------------------------

/// Solve the 8 unknowns of the projective map `dst -> src` from four
/// correspondences. Returns `None` for a degenerate (collinear) quad.
#[allow(clippy::needless_range_loop)]
fn solve_homography(dst: [(f64, f64); 4], src: [(f64, f64); 4]) -> Option<[f64; 8]> {
    let mut m = [[0.0f64; 9]; 8];
    for i in 0..4 {
        let (x, y) = dst[i];
        let (u, v) = src[i];
        m[2 * i] = [x, y, 1.0, 0.0, 0.0, 0.0, -u * x, -u * y, u];
        m[2 * i + 1] = [0.0, 0.0, 0.0, x, y, 1.0, -v * x, -v * y, v];
    }
    for col in 0..8 {
        let mut pivot = col;
        for row in (col + 1)..8 {
            if m[row][col].abs() > m[pivot][col].abs() {
                pivot = row;
            }
        }
        if m[pivot][col].abs() < 1e-12 {
            return None;
        }
        m.swap(col, pivot);
        let d = m[col][col];
        for k in col..9 {
            m[col][k] /= d;
        }
        for row in 0..8 {
            if row == col {
                continue;
            }
            let factor = m[row][col];
            if factor == 0.0 {
                continue;
            }
            for k in col..9 {
                m[row][k] -= factor * m[col][k];
            }
        }
    }
    let mut h = [0.0f64; 8];
    h.copy_from_slice(&m.each_ref().map(|row| row[8]));
    Some(h)
}

/// Evaluate a projective map at `(x, y)`.
fn project(h: &[f64; 8], x: f64, y: f64) -> Option<(f64, f64)> {
    let w = h[6] * x + h[7] * y + 1.0;
    if w.abs() < 1e-12 {
        return None;
    }
    Some((
        (h[0] * x + h[1] * y + h[2]) / w,
        (h[3] * x + h[4] * y + h[5]) / w,
    ))
}

fn read_pixel(bitmap: &BitmapState, x: i32, y: i32) -> [u8; 4] {
    match bitmap.pixel_offset(x as u32, y as u32) {
        Some(i) => [
            bitmap.rgba[i],
            bitmap.rgba[i + 1],
            bitmap.rgba[i + 2],
            bitmap.rgba[i + 3],
        ],
        None => [0, 0, 0, 0],
    }
}

/// Bilinear sample with edge clamping (`agg::image_accessor_clip`).
fn sample_bilinear(src: &BitmapState, fx: f32, fy: f32) -> [u8; 4] {
    if src.width == 0 || src.height == 0 {
        return [0, 0, 0, 0];
    }
    let max_x = src.width as f32 - 1.0;
    let max_y = src.height as f32 - 1.0;
    let x = fx.clamp(0.0, max_x);
    let y = fy.clamp(0.0, max_y);
    let x0 = x.floor() as i32;
    let y0 = y.floor() as i32;
    let x1 = (x0 + 1).min(src.width as i32 - 1);
    let y1 = (y0 + 1).min(src.height as i32 - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let c00 = read_pixel(src, x0, y0);
    let c10 = read_pixel(src, x1, y0);
    let c01 = read_pixel(src, x0, y1);
    let c11 = read_pixel(src, x1, y1);
    let mut out = [0u8; 4];
    for c in 0..4 {
        let top = c00[c] as f32 * (1.0 - tx) + c10[c] as f32 * tx;
        let bottom = c01[c] as f32 * (1.0 - tx) + c11[c] as f32 * tx;
        out[c] = (top * (1.0 - ty) + bottom * ty).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Projective-copy `src` (the sub-rectangle `src_pts`) onto the destination
/// quad `dst_pts`.
fn perspective_blit(
    dst: &mut BitmapState,
    src: &BitmapState,
    src_pts: [(f64, f64); 4],
    dst_pts: [(f64, f64); 4],
) {
    let Some(h) = solve_homography(dst_pts, src_pts) else {
        return;
    };
    // The source rectangle bounds (the quad may be given with a negative
    // width/height, so normalize).
    let sx0 = src_pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min);
    let sx1 = src_pts
        .iter()
        .map(|p| p.0)
        .fold(f64::NEG_INFINITY, f64::max);
    let sy0 = src_pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min);
    let sy1 = src_pts
        .iter()
        .map(|p| p.1)
        .fold(f64::NEG_INFINITY, f64::max);

    // Destination bounding box, clipped to the destination image.
    let min_x = dst_pts
        .iter()
        .map(|p| p.0)
        .fold(f64::INFINITY, f64::min)
        .floor() as i32;
    let max_x = dst_pts
        .iter()
        .map(|p| p.0)
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil() as i32;
    let min_y = dst_pts
        .iter()
        .map(|p| p.1)
        .fold(f64::INFINITY, f64::min)
        .floor() as i32;
    let max_y = dst_pts
        .iter()
        .map(|p| p.1)
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil() as i32;
    let Some((x0, y0, x1, y1)) = layer_ops::intersect(
        dst,
        (
            min_x,
            min_y,
            max_x.saturating_add(1),
            max_y.saturating_add(1),
        ),
    ) else {
        return;
    };
    let eps = 1e-6;
    for y in y0..y1 {
        for x in x0..x1 {
            let Some((u, v)) = project(&h, x as f64 + 0.5, y as f64 + 0.5) else {
                continue;
            };
            if u < sx0 - eps || u > sx1 + eps || v < sy0 - eps || v > sy1 + eps {
                continue;
            }
            let color = sample_bilinear(src, u as f32 - 0.5, v as f32 - 0.5);
            engine::raster::blend_pixel(dst, x, y, color, 255, 255);
        }
    }
    dst.mark_dirty();
}

// ---------------------------------------------------------------------------
// Shared implementation
// ---------------------------------------------------------------------------

/// The reference `AsReal` conversion (`reference/cpp/core/tjs2/tjsVariant.h:
/// 947-966`) for the twelve scalar coordinates of `PerspectiveCopy_GL`
/// (`reference/cpp/plugins/layerExPerspective.cpp:181-199`): `void`/strings
/// coerce, objects and octets raise a conversion error instead of silently
/// coercing to 0.
fn strict_real(value: &Value) -> Result<f64, String> {
    match value.ty {
        tjs::VAL_INTEGER | tjs::VAL_REAL | tjs::VAL_STRING | tjs::VAL_VOID => {
            Ok(super::value_as_f64(value))
        }
        _ => Err("the argument is not a number".to_string()),
    }
}

/// Resolve the `src` argument: the reference accepts *only* a `Layer`
/// object (`objthis->NativeInstanceSupport(TJS_NIS_GETINSTANCE,
/// tTJSNC_Layer::ClassID, …)` on `param[0]` and
/// `TVPThrowExceptionMessage(TVPSpecifyLayer)` otherwise —
/// `reference/cpp/plugins/layerExPerspective.cpp:171-176`). A non-object
/// argument leaves the reference with a null `src` and crashes; the port
/// reports a typed error instead.
fn layer_source_id(host: &Tjs2Engine, value: &Value) -> Result<u32, String> {
    if value.ty != VAL_OBJECT {
        return Err("Layer.perspectiveCopy: the source is not a Layer".to_string());
    }
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("Layer.perspectiveCopy: {error}"))?;
    match crate::CompatCtx::native_instance_id(host, object.raw_id(), &["Layer"])? {
        Some(id) => Ok(id),
        None => Err("Layer.perspectiveCopy: the source is not a Layer".to_string()),
    }
}

/// Run the reference `perspectiveCopy` with the destination layer already
/// resolved.
pub(super) fn perspective_copy(
    ctx: &CompatCtx,
    host: &tjs::Tjs2Engine,
    dst_id: u32,
    a: &[Value],
) -> Result<(), String> {
    if a.len() < 13 {
        return Err("Layer.perspectiveCopy requires 13 arguments".to_string());
    }
    let src_id = layer_source_id(host, &a[0])?;

    let mut coords = [0.0f64; 12];
    for (slot, value) in coords.iter_mut().zip(a[1..].iter()) {
        *slot = strict_real(value).map_err(|error| format!("Layer.perspectiveCopy: {error}"))?;
    }
    let g_x1 = coords[0];
    let g_y1 = coords[1];
    let g_x2 = g_x1 + coords[2];
    let g_y2 = g_y1 + coords[3];
    let src_pts = [(g_x1, g_y1), (g_x2, g_y1), (g_x1, g_y2), (g_x2, g_y2)];
    let dst_pts = [
        (coords[4], coords[5]),
        (coords[6], coords[7]),
        (coords[8], coords[9]),
        (coords[10], coords[11]),
    ];

    let scene = scene_mut(ctx)?;
    let src_bitmap_id = match scene.layer(src_id) {
        Some(layer) => layer.bitmap.ok_or_else(|| {
            "Layer.perspectiveCopy: the source layer has no main image".to_string()
        })?,
        None => {
            // The resolved id must name a live scene layer: the reference
            // `NativeInstanceSupport` lookup only ever yields one
            // (`reference/cpp/plugins/layerExPerspective.cpp:171-175`).
            return Err("Layer.perspectiveCopy: the source is not a Layer".to_string());
        }
    };
    let dst_bitmap_id = scene
        .layer(dst_id)
        .and_then(|layer| layer.bitmap)
        .ok_or_else(|| "Layer.perspectiveCopy: this layer has no main image".to_string())?;
    let source = scene
        .bitmap(src_bitmap_id)
        .cloned()
        .ok_or_else(|| "Layer.perspectiveCopy: the source bitmap does not exist".to_string())?;
    {
        let dest = scene.bitmap_mut(dst_bitmap_id).ok_or_else(|| {
            "Layer.perspectiveCopy: the destination bitmap does not exist".to_string()
        })?;
        perspective_blit(dest, &source, src_pts, dst_pts);
    }
    if let Some(layer) = scene.layer_mut(dst_id) {
        layer.pending_paint = true;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// `Layer` member
// ---------------------------------------------------------------------------

extern "C" fn layer_perspective_copy(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "Layer.perspectiveCopy: compat context is not bound",
        );
    };
    let id = match layer_from_objthis(resolved.engine(), objthis) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return report_error(
                out_error,
                "Layer.perspectiveCopy: the receiver is not a Layer",
            );
        }
        Err(error) => return report_error(out_error, &format!("Layer.perspectiveCopy: {error}")),
    };
    let a = unsafe { args(argv, argc) };
    if let Err(error) = perspective_copy(&ctx, resolved.engine(), id, a) {
        return report_error(out_error, &error);
    }
    set_void(out);
    0
}

/// The `Layer`-instance method definitions.
pub(super) fn layer_method_defs() -> Vec<NativeObjThisMethodDef> {
    vec![NativeObjThisMethodDef {
        name: "perspectiveCopy",
        f: layer_perspective_copy,
    }]
}
