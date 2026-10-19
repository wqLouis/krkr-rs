//! `LayerExDraw`: the `Layer.draw*` / transform / record extension.
//!
//! Reference: `reference/cpp/plugins/layerex_draw/windows/LayerExDraw.{cpp,hpp}`
//! and the cross-platform `blend2d` build. The reference attaches a native
//! `LayerExDraw` instance to every `Layer` with `NCB_ATTACH_CLASS_WITH_HOOK`;
//! the port attaches the same members to the existing Rust `Layer` class with
//! [`CompatCtx::register_instance_members`] and keeps the per-layer drawing
//! state in [`CompatServices::state`](crate::CompatServices::state) keyed by
//! the layer id (the reference keeps it in the attached instance).
//!
//! All pixel work goes through the L2 `engine_api::layer_ops` blits and the L1
//! `engine_api::raster` compositor; there is no second GPU path.
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use crate::engine_api::BitmapState;
use crate::engine_api::layer_ops::{self, RectI};
use crate::engine_api::raster::{self, blend_pixel};
use api::fs::FsPath;
use tjs::{NativeObjThisMethodDef, NativeObjThisPropertyDef, TjsValue, Value};

use super::appearance::{AppearanceData, AppearanceInst, Paint, paint_color_at};
use super::font::{TextBitmap, read_font, render_text, tint_coverage};
use super::path::{PathData, PathInst};
use super::{
    argb_to_rgba, bound, ensure_layer_image, layer_clip, layer_from_objthis, mark_layer_paint,
    new_rectf, read_instance, read_points, read_rects, scene_mut, set_real, set_retained_out,
    value_as_f64, value_as_usize,
};
use crate::CompatCtx;
use crate::native_util::{args, report_error, set_bool, set_void, value_as_string};
use crate::natives::system::{value_as_bool, value_as_i64};
use crate::storage::strip_file_media;

use super::record::{RecordOp, decode_record, encode_record, replay_record};

// ---------------------------------------------------------------------------
// Affine matrix
// ---------------------------------------------------------------------------

/// A GDI+ affine matrix in the `(m11, m12, m21, m22, dx, dy)` order, mapping
/// `(x, y)` to `(m11*x + m21*y + dx, m12*x + m22*y + dy)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Mat {
    pub(super) m11: f64,
    pub(super) m12: f64,
    pub(super) m21: f64,
    pub(super) m22: f64,
    pub(super) dx: f64,
    pub(super) dy: f64,
}

impl Default for Mat {
    fn default() -> Self {
        Self::IDENTITY
    }
}

impl Mat {
    pub(super) const IDENTITY: Self = Self {
        m11: 1.0,
        m12: 0.0,
        m21: 0.0,
        m22: 1.0,
        dx: 0.0,
        dy: 0.0,
    };

    pub(super) fn apply(self, p: (f64, f64)) -> (f64, f64) {
        (
            self.m11 * p.0 + self.m21 * p.1 + self.dx,
            self.m12 * p.0 + self.m22 * p.1 + self.dy,
        )
    }

    /// `self * other` — apply `self` first, then `other` (GDI+ `Append`).
    pub(super) fn mul(self, other: &Mat) -> Mat {
        Mat {
            m11: self.m11 * other.m11 + self.m21 * other.m12,
            m12: self.m12 * other.m11 + self.m22 * other.m12,
            m21: self.m11 * other.m21 + self.m21 * other.m22,
            m22: self.m12 * other.m21 + self.m22 * other.m22,
            dx: self.m11 * other.dx + self.m21 * other.dy + self.dx,
            dy: self.m12 * other.dx + self.m22 * other.dy + self.dy,
        }
    }

    pub(super) fn translate(dx: f64, dy: f64) -> Self {
        Mat {
            dx,
            dy,
            ..Self::IDENTITY
        }
    }

    pub(super) fn rotate(degrees: f64) -> Self {
        let r = degrees.to_radians();
        let (s, c) = r.sin_cos();
        Mat {
            m11: c,
            m12: s,
            m21: -s,
            m22: c,
            dx: 0.0,
            dy: 0.0,
        }
    }

    pub(super) fn scale(sx: f64, sy: f64) -> Self {
        Mat {
            m11: sx,
            m22: sy,
            ..Self::IDENTITY
        }
    }

    /// Average basis-vector length, used to scale pen widths.
    pub(super) fn scale_factor(self) -> f64 {
        let a = (self.m11 * self.m11 + self.m12 * self.m12).sqrt();
        let b = (self.m21 * self.m21 + self.m22 * self.m22).sqrt();
        ((a + b) / 2.0).max(0.0001)
    }

    /// Whether the transform is an (integer) translation with unit scale.
    fn is_integer_translation(self) -> Option<(i32, i32)> {
        let unit = (self.m11 - 1.0).abs() < 1e-9
            && self.m12.abs() < 1e-9
            && self.m21.abs() < 1e-9
            && (self.m22 - 1.0).abs() < 1e-9;
        if !unit {
            return None;
        }
        let dx = self.dx.round();
        let dy = self.dy.round();
        if (self.dx - dx).abs() < 1e-6 && (self.dy - dy).abs() < 1e-6 {
            Some((dx as i32, dy as i32))
        } else {
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Per-layer draw state
// ---------------------------------------------------------------------------

/// One layer's `LayerExDraw` state (the reference attached instance).
#[derive(Clone)]
pub(super) struct LayerDraw {
    pub(super) transform: Mat,
    pub(super) view_transform: Mat,
    pub(super) smoothing: i32,
    pub(super) text_hint: i32,
    pub(super) update_when_draw: bool,
    pub(super) recording: Option<Vec<RecordOp>>,
}

impl Default for LayerDraw {
    fn default() -> Self {
        Self {
            transform: Mat::IDENTITY,
            view_transform: Mat::IDENTITY,
            smoothing: 4,
            text_hint: 4,
            update_when_draw: true,
            recording: None,
        }
    }
}

impl LayerDraw {
    fn calc_transform(&self) -> Mat {
        self.transform.mul(&self.view_transform)
    }
}

/// Per-layer drawing state, keyed by layer id.
#[derive(Default)]
pub(super) struct DrawState {
    pub(super) layers: HashMap<u32, LayerDraw>,
}

fn draw_state(ctx: &CompatCtx) -> std::cell::RefMut<'_, DrawState> {
    // SAFETY: `ctx` was resolved from the installed binding.
    unsafe { ctx.services() }.state::<DrawState>()
}

/// Drop the per-layer drawing state of `id`.
///
/// The transform and the recorded operation list belong to the layer and must
/// not outlive it.
pub(crate) fn forget_layer(ctx: &CompatCtx, id: u32) {
    draw_state(ctx).layers.remove(&id);
}

// ---------------------------------------------------------------------------
// Geometry helpers
// ---------------------------------------------------------------------------

/// Axis-aligned rectangle in floating-point coordinates.
#[derive(Clone, Copy, Debug, Default)]
pub(super) struct RectF64 {
    pub(super) x: f64,
    pub(super) y: f64,
    pub(super) w: f64,
    pub(super) h: f64,
}

impl RectF64 {
    fn from_points(points: &[(f64, f64)]) -> Option<Self> {
        if points.is_empty() {
            return None;
        }
        let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
        let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        for &(x, y) in points {
            min_x = min_x.min(x);
            min_y = min_y.min(y);
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
        Some(Self {
            x: min_x,
            y: min_y,
            w: max_x - min_x,
            h: max_y - min_y,
        })
    }

    fn union(&mut self, other: &Self) {
        let min_x = self.x.min(other.x);
        let min_y = self.y.min(other.y);
        let max_x = (self.x + self.w).max(other.x + other.w);
        let max_y = (self.y + self.h).max(other.y + other.h);
        self.x = min_x;
        self.y = min_y;
        self.w = max_x - min_x;
        self.h = max_y - min_y;
    }

    fn transformed(&self, ctm: Mat) -> Self {
        let corners = [
            ctm.apply((self.x, self.y)),
            ctm.apply((self.x + self.w, self.y)),
            ctm.apply((self.x, self.y + self.h)),
            ctm.apply((self.x + self.w, self.y + self.h)),
        ];
        RectF64::from_points(&corners).unwrap_or_default()
    }
}

fn transform_points(points: &[(f64, f64)], ctm: Mat) -> Vec<(f64, f64)> {
    points.iter().map(|&p| ctm.apply(p)).collect()
}

// ---------------------------------------------------------------------------
// CPU paint rasterization (clipped)
// ---------------------------------------------------------------------------

/// 4×4 supersample grid per axis (matches `engine_api::raster`).
const AA: i32 = 4;

/// Dilate an 8-bit coverage mask by `radius` pixels (a cheap outline for
/// `drawPathString`'s pens).
fn dilate_coverage(coverage: &[u8], width: u32, height: u32, radius: i32) -> Vec<u8> {
    if radius <= 0 {
        return coverage.to_vec();
    }
    let mut out = vec![0u8; coverage.len()];
    let radius_sq = radius * radius;
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let mut hit = false;
            'search: for dy in -radius..=radius {
                for dx in -radius..=radius {
                    if dx * dx + dy * dy > radius_sq {
                        continue;
                    }
                    let nx = x + dx;
                    let ny = y + dy;
                    if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                        continue;
                    }
                    if coverage[ny as usize * width as usize + nx as usize] > 0 {
                        hit = true;
                        break 'search;
                    }
                }
            }
            if hit {
                out[y as usize * width as usize + x as usize] = 255;
            }
        }
    }
    out
}

fn point_in_figures(x: f64, y: f64, figures: &[Vec<(f64, f64)>]) -> bool {
    let mut inside = false;
    for pts in figures {
        let n = pts.len();
        if n < 3 {
            continue;
        }
        let mut j = n - 1;
        for i in 0..n {
            let (xi, yi) = pts[i];
            let (xj, yj) = pts[j];
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                inside = !inside;
            }
            j = i;
        }
    }
    inside
}

/// Fill the transformed figures with `paint`, clipped to `clip`.
pub(super) fn fill_figures(
    bitmap: &mut BitmapState,
    figures: &[Vec<(f64, f64)>],
    paint: &Paint,
    clip: RectI,
) {
    let mut all = Vec::new();
    for figure in figures {
        all.extend_from_slice(figure);
    }
    let Some(bounds) = RectF64::from_points(&all) else {
        return;
    };
    let max_px = bitmap.width.saturating_sub(1).min(i32::MAX as u32) as i32;
    let max_py = bitmap.height.saturating_sub(1).min(i32::MAX as u32) as i32;
    let x0 = (bounds.x.floor() as i32).max(clip.0).max(0);
    let y0 = (bounds.y.floor() as i32).max(clip.1).max(0);
    let x1 = ((bounds.x + bounds.w).ceil() as i32)
        .min(clip.2)
        .min(max_px);
    let y1 = ((bounds.y + bounds.h).ceil() as i32)
        .min(clip.3)
        .min(max_py);
    if x0 > x1 || y0 > y1 {
        return;
    }
    let samples = (AA * AA) as u32;
    for py in y0..=y1 {
        for px in x0..=x1 {
            let mut hits = 0u32;
            for sy in 0..AA {
                for sx in 0..AA {
                    let x = px as f64 + (sx as f64 + 0.5) / AA as f64;
                    let y = py as f64 + (sy as f64 + 0.5) / AA as f64;
                    if point_in_figures(x, y, figures) {
                        hits += 1;
                    }
                }
            }
            if hits == 0 {
                continue;
            }
            let coverage = ((hits * 255) / samples) as u8;
            let color = paint_color_at(paint, px as f64 + 0.5, py as f64 + 0.5);
            blend_pixel(bitmap, px, py, color, coverage, 255);
        }
    }
}

fn dist_point_segment(p: (f64, f64), a: (f64, f64), b: (f64, f64)) -> f64 {
    let (vx, vy) = (b.0 - a.0, b.1 - a.1);
    let (wx, wy) = (p.0 - a.0, p.1 - a.1);
    let len2 = vx * vx + vy * vy;
    let t = if len2 <= f64::EPSILON {
        0.0
    } else {
        ((wx * vx + wy * vy) / len2).clamp(0.0, 1.0)
    };
    let dx = p.0 - (a.0 + t * vx);
    let dy = p.1 - (a.1 + t * vy);
    (dx * dx + dy * dy).sqrt()
}

/// Stroke a transformed polyline with `paint`, clipped to `clip`.
pub(super) fn stroke_polyline_paint(
    bitmap: &mut BitmapState,
    points: &[(f64, f64)],
    closed: bool,
    paint: &Paint,
    width: f64,
    clip: RectI,
) {
    if points.len() < 2 {
        return;
    }
    let half = width.max(1.0) / 2.0;
    let max_px = bitmap.width.saturating_sub(1).min(i32::MAX as u32) as i32;
    let max_py = bitmap.height.saturating_sub(1).min(i32::MAX as u32) as i32;
    let mut segment = |a: (f64, f64), b: (f64, f64)| {
        let x0 = ((a.0.min(b.0) - half - 1.0).floor() as i32)
            .max(clip.0)
            .max(0);
        let y0 = ((a.1.min(b.1) - half - 1.0).floor() as i32)
            .max(clip.1)
            .max(0);
        let x1 = ((a.0.max(b.0) + half + 1.0).ceil() as i32)
            .min(clip.2)
            .min(max_px);
        let y1 = ((a.1.max(b.1) + half + 1.0).ceil() as i32)
            .min(clip.3)
            .min(max_py);
        if x0 > x1 || y0 > y1 {
            return;
        }
        for py in y0..=y1 {
            for px in x0..=x1 {
                let d = dist_point_segment((px as f64 + 0.5, py as f64 + 0.5), a, b);
                let coverage = ((half + 0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
                if coverage > 0 {
                    let color = paint_color_at(paint, px as f64 + 0.5, py as f64 + 0.5);
                    blend_pixel(bitmap, px, py, color, coverage, 255);
                }
            }
        }
    };
    for i in 0..points.len() - 1 {
        segment(points[i], points[i + 1]);
    }
    if closed && points.len() >= 3 {
        segment(points[points.len() - 1], points[0]);
    }
}

// ---------------------------------------------------------------------------
// Path rendering
// ---------------------------------------------------------------------------

/// Render `path` with `app`'s ordered draw list under `ctm`, returning the
/// transformed update rectangle.
pub(super) fn render_path(
    bitmap: &mut BitmapState,
    path: &PathData,
    app: &AppearanceData,
    ctm: Mat,
    clip: RectI,
) -> Option<RectF64> {
    if path.figures.is_empty() {
        return None;
    }
    let mut update: Option<RectF64> = None;
    for info in &app.infos {
        let local = Mat::translate(info.ox, info.oy);
        let m = local.mul(&ctm);
        if info.is_pen {
            for figure in &path.figures {
                let points = transform_points(&figure.points, m);
                stroke_polyline_paint(
                    bitmap,
                    &points,
                    figure.closed,
                    &info.paint,
                    info.width * m.scale_factor(),
                    clip,
                );
                if let Some(bounds) = RectF64::from_points(&points) {
                    match &mut update {
                        Some(acc) => acc.union(&bounds),
                        None => update = Some(bounds),
                    }
                }
            }
        } else {
            let figures: Vec<Vec<(f64, f64)>> = path
                .figures
                .iter()
                .map(|figure| transform_points(&figure.points, m))
                .collect();
            fill_figures(bitmap, &figures, &info.paint, clip);
            let all: Vec<(f64, f64)> = figures.into_iter().flatten().collect();
            if let Some(bounds) = RectF64::from_points(&all) {
                match &mut update {
                    Some(acc) => acc.union(&bounds),
                    None => update = Some(bounds),
                }
            }
        }
    }
    update
}

/// Blit `src` so that its top-left lands at `origin`, transformed by `ctm`.
pub(super) fn blit_with_transform(
    bitmap: &mut BitmapState,
    src: &BitmapState,
    src_rect: RectI,
    origin: (f64, f64),
    ctm: Mat,
    clip: RectI,
) {
    let sw = src_rect.2 - src_rect.0;
    let sh = src_rect.3 - src_rect.1;
    if sw <= 0 || sh <= 0 {
        return;
    }
    if let Some((dx, dy)) = ctm.is_integer_translation() {
        layer_ops::blit_over(
            bitmap,
            dx + origin.0.round() as i32,
            dy + origin.1.round() as i32,
            layer_ops::BlitSource {
                bitmap: src,
                rect: src_rect,
            },
            clip,
            layer_ops::BlitOptions {
                opa: 255,
                ..Default::default()
            },
        );
        return;
    }
    let p0 = ctm.apply(origin);
    let p1 = ctm.apply((origin.0 + sw as f64, origin.1));
    let p2 = ctm.apply((origin.0, origin.1 + sh as f64));
    layer_ops::affine_blit(
        bitmap,
        layer_ops::AffineBlit {
            corners: [p0, p1, p2],
            stretch_type: 0,
            clear: false,
            clear_color: [0, 0, 0, 0],
        },
        layer_ops::BlitSource {
            bitmap: src,
            rect: src_rect,
        },
        layer_ops::BlitOptions {
            mode: 2,
            opa: 255,
            ..Default::default()
        },
    );
}

/// A source bitmap and the three destination corners it maps to.
#[derive(Clone, Copy)]
pub(super) struct ImagePlacement<'a> {
    pub(super) source: layer_ops::BlitSource<'a>,
    pub(super) corners: [(f64, f64); 3],
}

/// Blit `source` so its rectangle maps to `corners` (top-left, top-right,
/// bottom-left), transformed by `ctm`.
pub(super) fn blit_parallelogram(
    bitmap: &mut BitmapState,
    placement: ImagePlacement<'_>,
    ctm: Mat,
    clip: RectI,
) {
    let layer_ops::BlitSource {
        bitmap: src,
        rect: src_rect,
    } = placement.source;
    let [p0, p1, p2] = placement.corners;
    let p0 = ctm.apply(p0);
    let p1 = ctm.apply(p1);
    let p2 = ctm.apply(p2);
    let sw = (src_rect.2 - src_rect.0) as f64;
    let sh = (src_rect.3 - src_rect.1) as f64;
    let axis_aligned =
        (p1.1 - p0.1).abs() < 1e-6 && (p2.0 - p0.0).abs() < 1e-6 && p1.0 >= p0.0 && p2.1 >= p0.1;
    if axis_aligned {
        let dw = p1.0 - p0.0;
        let dh = p2.1 - p0.1;
        let dx = p0.0.round() as i32;
        let dy = p0.1.round() as i32;
        if (dw - sw).abs() < 1e-6
            && (dh - sh).abs() < 1e-6
            && (p0.0 - dx as f64).abs() < 1e-6
            && (p0.1 - dy as f64).abs() < 1e-6
        {
            layer_ops::blit_over(
                bitmap,
                dx,
                dy,
                layer_ops::BlitSource {
                    bitmap: src,
                    rect: src_rect,
                },
                clip,
                layer_ops::BlitOptions {
                    opa: 255,
                    ..Default::default()
                },
            );
            return;
        }
    }
    layer_ops::affine_blit(
        bitmap,
        layer_ops::AffineBlit {
            corners: [p0, p1, p2],
            stretch_type: 0,
            clear: false,
            clear_color: [0, 0, 0, 0],
        },
        layer_ops::BlitSource {
            bitmap: src,
            rect: src_rect,
        },
        layer_ops::BlitOptions {
            mode: 2,
            opa: 255,
            ..Default::default()
        },
    );
}

// ---------------------------------------------------------------------------
// Layer dispatch helpers
// ---------------------------------------------------------------------------

/// Resolve the receiving `Layer` id and its current draw state, then run `f`
/// with the layer bitmap, the state, the clip rectangle and the combined
/// transform.
fn with_layer<R>(
    engine: *mut c_void,
    objthis: *mut c_void,
    method: &str,
    min_size: (u32, u32),
    f: impl FnOnce(&mut BitmapState, &mut LayerDraw, RectI, Mat) -> Result<R, String>,
) -> Result<R, String> {
    let (resolved, ctx) = bound(engine)?;
    let layer_id = layer_from_objthis(resolved.engine(), objthis)?
        .ok_or_else(|| format!("{method}: the receiver is not a Layer"))?;
    // SAFETY: `ctx` is only created for a bound, live engine, so its service pointer is valid for `with_layer`.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<DrawState>();
    let layer_draw = state.layers.entry(layer_id).or_default();
    let scene = scene_mut(&ctx)?;
    if scene.layer(layer_id).is_none() {
        return Err(format!("{method}: the layer no longer exists"));
    }
    if scene.layer(layer_id).and_then(|l| l.bitmap).is_none() {
        ensure_layer_image(scene, layer_id, min_size.0.max(1), min_size.1.max(1));
    }
    let bitmap_id = scene
        .layer(layer_id)
        .and_then(|l| l.bitmap)
        .ok_or_else(|| format!("{method}: the layer has no main image"))?;
    let clip = layer_clip(scene, layer_id);
    let ctm = layer_draw.calc_transform();
    let update = layer_draw.update_when_draw;
    let result = {
        let bitmap = scene
            .bitmap_mut(bitmap_id)
            .ok_or_else(|| format!("{method}: the bitmap no longer exists"))?;
        f(bitmap, layer_draw, clip, ctm)
    };
    if update {
        mark_layer_paint(scene, layer_id);
    }
    result
}

fn set_rectf_result(engine: *mut c_void, out: *mut Value, rect: RectF64) {
    if out.is_null() {
        return;
    }
    let Ok((resolved, _ctx)) = bound(engine) else {
        set_void(out);
        return;
    };
    match new_rectf(resolved.engine(), rect.x, rect.y, rect.w, rect.h) {
        Ok(object) => {
            set_retained_out(out, object.raw_id());
            std::mem::forget(object);
        }
        Err(_) => set_void(out),
    }
}

fn appearance_ref<'a>(
    engine: *mut c_void,
    value: &Value,
    what: &str,
) -> Result<&'a AppearanceData, String> {
    let ptr = read_instance(engine, value, "Appearance", what)?;
    // SAFETY: the VM's native-instance lookup returned the payload of the
    // live `Appearance` argument, which is alive for the duration of the
    // callback.
    let inst = unsafe { &*(ptr as *const AppearanceInst) };
    Ok(&inst.data)
}

fn path_ref<'a>(engine: *mut c_void, value: &Value, what: &str) -> Result<&'a PathData, String> {
    let ptr = read_instance(engine, value, "Path", what)?;
    // SAFETY: see `appearance_ref`.
    let inst = unsafe { &*(ptr as *const PathInst) };
    Ok(&inst.data)
}

fn path_size(path: &PathData) -> (u32, u32) {
    let mut max_x: f64 = 1.0;
    let mut max_y: f64 = 1.0;
    for figure in &path.figures {
        for &(x, y) in &figure.points {
            max_x = max_x.max(x);
            max_y = max_y.max(y);
        }
    }
    (max_x.ceil() as u32, max_y.ceil() as u32)
}

/// Finish a `draw*` call: rasterize `path` with `app`, record it, and return
/// the update `RectF`.
fn finish_path(
    engine: *mut c_void,
    objthis: *mut c_void,
    method: &str,
    app: &AppearanceData,
    path: PathData,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let size = path_size(&path);
    let result = with_layer(
        engine,
        objthis,
        method,
        size,
        |bitmap, layer_draw, clip, ctm| {
            let update = render_path(bitmap, &path, app, ctm, clip).unwrap_or_default();
            if let Some(record) = layer_draw.recording.as_mut() {
                record.push(RecordOp::Path {
                    path: path.clone(),
                    app: app.clone(),
                });
            }
            Ok(update)
        },
    );
    match result {
        Ok(update) => {
            set_rectf_result(engine, out, update);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

fn read_matrix(engine: *mut c_void, value: &Value, what: &str) -> Result<Mat, String> {
    let Ok((resolved, _ctx)) = bound(engine) else {
        return Err(format!("{what}: compat context is not bound"));
    };
    let host = resolved.engine();
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("{what}: the matrix argument is not an object: {error}"))?;
    let number = |name: &str| -> Result<f64, String> {
        match host.member(object.raw_id(), name) {
            Ok(TjsValue::Integer(v)) => Ok(v as f64),
            Ok(TjsValue::Real(v)) => Ok(v),
            Ok(_) => Err(format!("{what}: the matrix member {name} is not numeric")),
            Err(error) => Err(format!("{what}: the matrix member {name}: {error}")),
        }
    };
    let is_array = matches!(
        host.member(object.raw_id(), "count"),
        Ok(TjsValue::Integer(count)) if count >= 6
    );
    if is_array {
        let mut values = [0.0f64; 6];
        for (i, slot) in values.iter_mut().enumerate() {
            *slot = number(&i.to_string())?;
        }
        return Ok(Mat {
            m11: values[0],
            m12: values[1],
            m21: values[2],
            m22: values[3],
            dx: values[4],
            dy: values[5],
        });
    }
    Ok(Mat {
        m11: number("m11")?,
        m12: number("m12")?,
        m21: number("m21")?,
        m22: number("m22")?,
        dx: number("dx")?,
        dy: number("dy")?,
    })
}

// ---------------------------------------------------------------------------
// Simple shape draw methods
// ---------------------------------------------------------------------------

macro_rules! path_draw_method {
    ($name:ident, $method:literal, $build:expr) => {
        extern "C" fn $name(
            engine: *mut c_void,
            objthis: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
        ) -> c_int {
            // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
            let a = unsafe { args(argv, argc) };
            let Some(app_value) = a.first() else {
                return report_error(out_error, concat!($method, ": an Appearance is required"));
            };
            let app = match appearance_ref(engine, app_value, $method) {
                Ok(app) => app,
                Err(error) => return report_error(out_error, &error),
            };
            let build: fn(*mut c_void, &[Value]) -> Result<PathData, String> = $build;
            let path = match build(engine, a) {
                Ok(path) => path,
                Err(error) => return report_error(out_error, &error),
            };
            finish_path(engine, objthis, $method, app, path, out, out_error)
        }
    };
}

fn figure(points: Vec<(f64, f64)>, closed: bool) -> PathData {
    let mut data = PathData::default();
    if points.len() >= 2 {
        data.figures.push(super::path::Figure { points, closed });
    }
    data
}

path_draw_method!(draw_arc, "Layer.drawArc", |_engine, a| {
    if a.len() < 7 {
        return Err("Layer.drawArc requires 7 arguments".to_string());
    }
    let points = raster::flatten_arc(
        value_as_f64(&a[1]),
        value_as_f64(&a[2]),
        value_as_f64(&a[3]),
        value_as_f64(&a[4]),
        value_as_f64(&a[5]),
        value_as_f64(&a[6]),
    );
    Ok(figure(points, false))
});

path_draw_method!(draw_pie, "Layer.drawPie", |_engine, a| {
    if a.len() < 7 {
        return Err("Layer.drawPie requires 7 arguments".to_string());
    }
    let (x, y, w, h) = (
        value_as_f64(&a[1]),
        value_as_f64(&a[2]),
        value_as_f64(&a[3]),
        value_as_f64(&a[4]),
    );
    let mut points = raster::flatten_arc(x, y, w, h, value_as_f64(&a[5]), value_as_f64(&a[6]));
    let center = (x + w / 2.0, y + h / 2.0);
    points.push(center);
    points.push(center);
    Ok(figure(points, true))
});

path_draw_method!(draw_bezier, "Layer.drawBezier", |_engine, a| {
    if a.len() < 9 {
        return Err("Layer.drawBezier requires 9 arguments".to_string());
    }
    let p0 = (value_as_f64(&a[1]), value_as_f64(&a[2]));
    let c1 = (value_as_f64(&a[3]), value_as_f64(&a[4]));
    let c2 = (value_as_f64(&a[5]), value_as_f64(&a[6]));
    let p3 = (value_as_f64(&a[7]), value_as_f64(&a[8]));
    Ok(figure(raster::flatten_cubic(p0, c1, c2, p3, 24), false))
});

path_draw_method!(draw_beziers, "Layer.drawBeziers", |engine, a| {
    let Some(value) = a.get(1) else {
        return Err("Layer.drawBeziers requires a point array".to_string());
    };
    let points = read_points(engine, value, "Layer.drawBeziers")?;
    Ok(figure(super::path::beziers(&points), false))
});

path_draw_method!(draw_lines, "Layer.drawLines", |engine, a| {
    let Some(value) = a.get(1) else {
        return Err("Layer.drawLines requires a point array".to_string());
    };
    let points = read_points(engine, value, "Layer.drawLines")?;
    Ok(figure(points, false))
});

path_draw_method!(draw_polygon, "Layer.drawPolygon", |engine, a| {
    let Some(value) = a.get(1) else {
        return Err("Layer.drawPolygon requires a point array".to_string());
    };
    let points = read_points(engine, value, "Layer.drawPolygon")?;
    Ok(figure(points, true))
});

path_draw_method!(draw_ellipse, "Layer.drawEllipse", |_engine, a| {
    if a.len() < 5 {
        return Err("Layer.drawEllipse requires 5 arguments".to_string());
    }
    let points = raster::flatten_arc(
        value_as_f64(&a[1]),
        value_as_f64(&a[2]),
        value_as_f64(&a[3]),
        value_as_f64(&a[4]),
        0.0,
        360.0,
    );
    Ok(figure(points, true))
});

path_draw_method!(draw_line, "Layer.drawLine", |_engine, a| {
    if a.len() < 5 {
        return Err("Layer.drawLine requires 5 arguments".to_string());
    }
    Ok(figure(
        vec![
            (value_as_f64(&a[1]), value_as_f64(&a[2])),
            (value_as_f64(&a[3]), value_as_f64(&a[4])),
        ],
        false,
    ))
});

path_draw_method!(draw_rectangle, "Layer.drawRectangle", |_engine, a| {
    if a.len() < 5 {
        return Err("Layer.drawRectangle requires 5 arguments".to_string());
    }
    let (x, y, w, h) = (
        value_as_f64(&a[1]),
        value_as_f64(&a[2]),
        value_as_f64(&a[3]),
        value_as_f64(&a[4]),
    );
    Ok(figure(
        vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
        true,
    ))
});

path_draw_method!(draw_rectangles, "Layer.drawRectangles", |engine, a| {
    let Some(value) = a.get(1) else {
        return Err("Layer.drawRectangles requires a rectangle array".to_string());
    };
    let rects = read_rects(engine, value, "Layer.drawRectangles")?;
    let mut data = PathData::default();
    for (x, y, w, h) in rects {
        data.figures.push(super::path::Figure {
            points: vec![(x, y), (x + w, y), (x + w, y + h), (x, y + h)],
            closed: true,
        });
    }
    Ok(data)
});

fn curve_data(points: &[(f64, f64)], closed: bool, tension: f64) -> PathData {
    let curve = super::path::cardinal_spline(points, closed, tension, 0, 0);
    figure(curve, closed)
}

path_draw_method!(draw_curve, "Layer.drawCurve", |engine, a| {
    let points = read_points(
        engine,
        a.get(1).ok_or("Layer.drawCurve requires points")?,
        "Layer.drawCurve",
    )?;
    Ok(curve_data(&points, false, 0.5))
});

path_draw_method!(draw_curve2, "Layer.drawCurve2", |engine, a| {
    let points = read_points(
        engine,
        a.get(1).ok_or("Layer.drawCurve2 requires points")?,
        "Layer.drawCurve2",
    )?;
    let tension = a.get(2).map(value_as_f64).unwrap_or(0.5);
    Ok(curve_data(&points, false, tension))
});

path_draw_method!(draw_curve3, "Layer.drawCurve3", |engine, a| {
    let points = read_points(
        engine,
        a.get(1).ok_or("Layer.drawCurve3 requires points")?,
        "Layer.drawCurve3",
    )?;
    let offset = a.get(2).and_then(value_as_usize).unwrap_or(0);
    let segments = a.get(3).and_then(value_as_usize).unwrap_or(0);
    let tension = a.get(4).map(value_as_f64).unwrap_or(0.5);
    let curve = super::path::cardinal_spline(&points, false, tension, offset, segments);
    Ok(figure(curve, false))
});

path_draw_method!(draw_closed_curve, "Layer.drawClosedCurve", |engine, a| {
    let points = read_points(
        engine,
        a.get(1).ok_or("Layer.drawClosedCurve requires points")?,
        "Layer.drawClosedCurve",
    )?;
    Ok(curve_data(&points, true, 0.5))
});

path_draw_method!(draw_closed_curve2, "Layer.drawClosedCurve2", |engine, a| {
    let points = read_points(
        engine,
        a.get(1).ok_or("Layer.drawClosedCurve2 requires points")?,
        "Layer.drawClosedCurve2",
    )?;
    let tension = a.get(2).map(value_as_f64).unwrap_or(0.5);
    Ok(curve_data(&points, true, tension))
});

/// `drawPath(app, path)`.
extern "C" fn draw_path(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 2 {
        return report_error(
            out_error,
            "Layer.drawPath requires an Appearance and a Path",
        );
    }
    let app = match appearance_ref(engine, &a[0], "Layer.drawPath") {
        Ok(app) => app,
        Err(error) => return report_error(out_error, &error),
    };
    let path = match path_ref(engine, &a[1], "Layer.drawPath") {
        Ok(path) => path,
        Err(error) => return report_error(out_error, &error),
    };
    finish_path(
        engine,
        objthis,
        "Layer.drawPath",
        app,
        path.clone(),
        out,
        out_error,
    )
}

// ---------------------------------------------------------------------------
// clear
// ---------------------------------------------------------------------------

extern "C" fn clear(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let color = argb_to_rgba(a.first().map(value_as_i64).unwrap_or(0));
    let result = with_layer(
        engine,
        objthis,
        "Layer.clear",
        (1, 1),
        |bitmap, layer_draw, _clip, _ctm| {
            raster::fill_rect_replace(bitmap, 0, 0, bitmap.width, bitmap.height, color);
            if let Some(record) = layer_draw.recording.as_mut() {
                record.push(RecordOp::Clear(color));
            }
            Ok(RectF64 {
                x: 0.0,
                y: 0.0,
                w: bitmap.width as f64,
                h: bitmap.height as f64,
            })
        },
    );
    match result {
        Ok(update) => {
            set_rectf_result(engine, out, update);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

// ---------------------------------------------------------------------------
// Image draw methods
// ---------------------------------------------------------------------------

/// Resolve a `Bitmap` or `Layer` argument to a scene bitmap snapshot.
fn image_snapshot(engine: *mut c_void, value: &Value, what: &str) -> Result<BitmapState, String> {
    let (resolved, ctx) = bound(engine)?;
    let host = resolved.engine();
    let object = host
        .retain_object_arg(value)
        .map_err(|error| format!("{what}: the image argument is not an object: {error}"))?;
    let scene = super::scene_ref(&ctx)?;
    // The reference resolves the image argument with
    // `NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)`: a `Layer`
    // argument contributes its main image, a `Bitmap` argument itself.
    let (id, from_layer) =
        match crate::CompatCtx::native_instance_id(host, object.raw_id(), &["Layer"]) {
            Ok(Some(id)) => (id, true),
            Ok(None) => {
                match crate::CompatCtx::native_instance_id(host, object.raw_id(), &["Bitmap"]) {
                    Ok(Some(id)) => (id, false),
                    Ok(None) => {
                        return Err(format!(
                            "{what}: the image argument is not a Layer or Bitmap"
                        ));
                    }
                    Err(error) => return Err(format!("{what}: {error}")),
                }
            }
            Err(error) => return Err(format!("{what}: {error}")),
        };
    let bitmap_id = if from_layer {
        scene
            .layer(id)
            .and_then(|layer| layer.bitmap)
            .ok_or_else(|| format!("{what}: the source layer has no image"))?
    } else {
        id
    };
    scene
        .bitmap(bitmap_id)
        .cloned()
        .ok_or_else(|| format!("{what}: the source image {bitmap_id} does not exist"))
}

fn finish_image(
    engine: *mut c_void,
    objthis: *mut c_void,
    method: &str,
    placement: ImagePlacement<'_>,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let (p0, p1, p2) = placement.corners.into();
    let src = placement.source.bitmap;
    let src_rect = placement.source.rect;
    let result = with_layer(
        engine,
        objthis,
        method,
        (src_rect.2.max(1) as u32, src_rect.3.max(1) as u32),
        |bitmap, layer_draw, clip, ctm| {
            blit_parallelogram(bitmap, placement, ctm, clip);
            let fourth = (p1.0 + p2.0 - p0.0, p1.1 + p2.1 - p0.1);
            let corners = [
                ctm.apply(p0),
                ctm.apply(p1),
                ctm.apply(p2),
                ctm.apply(fourth),
            ];
            let rect = RectF64::from_points(&corners).unwrap_or_default();
            if let Some(record) = layer_draw.recording.as_mut() {
                record.push(RecordOp::Blit {
                    src: src.rgba.clone(),
                    sw: src.width,
                    sh: src.height,
                    src_rect,
                    p0,
                    p1,
                    p2,
                });
            }
            Ok(rect)
        },
    );
    match result {
        Ok(update) => {
            set_rectf_result(engine, out, update);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn draw_image(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 3 {
        return report_error(out_error, "Layer.drawImage requires x, y and an image");
    }
    let src = match image_snapshot(engine, &a[2], "Layer.drawImage") {
        Ok(src) => src,
        Err(error) => return report_error(out_error, &error),
    };
    let (x, y) = (value_as_f64(&a[0]), value_as_f64(&a[1]));
    let w = src.width as f64;
    let h = src.height as f64;
    finish_image(
        engine,
        objthis,
        "Layer.drawImage",
        ImagePlacement {
            source: layer_ops::BlitSource {
                bitmap: &src,
                rect: (0, 0, src.width as i32, src.height as i32),
            },
            corners: [(x, y), (x + w, y), (x, y + h)],
        },
        out,
        out_error,
    )
}

extern "C" fn draw_image_rect(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 7 {
        return report_error(
            out_error,
            "Layer.drawImageRect requires destination, source and rectangle",
        );
    }
    let src = match image_snapshot(engine, &a[2], "Layer.drawImageRect") {
        Ok(src) => src,
        Err(error) => return report_error(out_error, &error),
    };
    let (dleft, dtop) = (value_as_f64(&a[0]), value_as_f64(&a[1]));
    let (sleft, stop, swidth, sheight) = (
        value_as_f64(&a[3]),
        value_as_f64(&a[4]),
        value_as_f64(&a[5]),
        value_as_f64(&a[6]),
    );
    let src_rect = (
        sleft.round() as i32,
        stop.round() as i32,
        (sleft + swidth).round() as i32,
        (stop + sheight).round() as i32,
    );
    finish_image(
        engine,
        objthis,
        "Layer.drawImageRect",
        ImagePlacement {
            source: layer_ops::BlitSource {
                bitmap: &src,
                rect: src_rect,
            },
            corners: [
                (dleft, dtop),
                (dleft + swidth, dtop),
                (dleft, dtop + sheight),
            ],
        },
        out,
        out_error,
    )
}

extern "C" fn draw_image_stretch(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 9 {
        return report_error(
            out_error,
            "Layer.drawImageStretch requires destination, size, image and source",
        );
    }
    let src = match image_snapshot(engine, &a[4], "Layer.drawImageStretch") {
        Ok(src) => src,
        Err(error) => return report_error(out_error, &error),
    };
    let (dleft, dtop, dwidth, dheight) = (
        value_as_f64(&a[0]),
        value_as_f64(&a[1]),
        value_as_f64(&a[2]),
        value_as_f64(&a[3]),
    );
    let (sleft, stop, swidth, sheight) = (
        value_as_f64(&a[5]),
        value_as_f64(&a[6]),
        value_as_f64(&a[7]),
        value_as_f64(&a[8]),
    );
    let src_rect = (
        sleft.round() as i32,
        stop.round() as i32,
        (sleft + swidth).round() as i32,
        (stop + sheight).round() as i32,
    );
    finish_image(
        engine,
        objthis,
        "Layer.drawImageStretch",
        ImagePlacement {
            source: layer_ops::BlitSource {
                bitmap: &src,
                rect: src_rect,
            },
            corners: [
                (dleft, dtop),
                (dleft + dwidth, dtop),
                (dleft, dtop + dheight),
            ],
        },
        out,
        out_error,
    )
}

extern "C" fn draw_image_affine(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 12 {
        return report_error(
            out_error,
            "Layer.drawImageAffine requires source, rectangle, affine flag and six parameters",
        );
    }
    let src = match image_snapshot(engine, &a[0], "Layer.drawImageAffine") {
        Ok(src) => src,
        Err(error) => return report_error(out_error, &error),
    };
    let (sleft, stop, swidth, sheight) = (
        value_as_f64(&a[1]),
        value_as_f64(&a[2]),
        value_as_f64(&a[3]),
        value_as_f64(&a[4]),
    );
    let affine = value_as_bool(&a[5]);
    let (a0, b0, c0, d0, e0, f0) = (
        value_as_f64(&a[6]),
        value_as_f64(&a[7]),
        value_as_f64(&a[8]),
        value_as_f64(&a[9]),
        value_as_f64(&a[10]),
        value_as_f64(&a[11]),
    );
    let (p0, p1, p2) = if affine {
        (
            (a0 * 0.0 + c0 * 0.0 + e0, b0 * 0.0 + d0 * 0.0 + f0),
            (a0 * swidth + e0, b0 * swidth + f0),
            (c0 * sheight + e0, d0 * sheight + f0),
        )
    } else {
        let p0 = (a0, b0);
        let p1 = (c0, d0);
        let p2 = (e0, f0);
        (p0, p1, p2)
    };
    let src_rect = (
        sleft.round() as i32,
        stop.round() as i32,
        (sleft + swidth).round() as i32,
        (stop + sheight).round() as i32,
    );
    finish_image(
        engine,
        objthis,
        "Layer.drawImageAffine",
        ImagePlacement {
            source: layer_ops::BlitSource {
                bitmap: &src,
                rect: src_rect,
            },
            corners: [p0, p1, p2],
        },
        out,
        out_error,
    )
}

// ---------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------

/// The paint used for the text coverage: the first brush, or white.
fn text_paint(app: &AppearanceData) -> Paint {
    app.infos
        .iter()
        .find(|info| !info.is_pen)
        .map(|info| info.paint.clone())
        .unwrap_or(Paint::Solid([255, 255, 255, 255]))
}

fn render_and_blit_text(
    bitmap: &mut BitmapState,
    text_bitmap: &TextBitmap,
    info: Option<&super::appearance::DrawInfo>,
    origin: (f64, f64),
    ctm: Mat,
    clip: RectI,
) {
    let local = Mat::translate(
        info.map(|i| i.ox).unwrap_or(0.0),
        info.map(|i| i.oy).unwrap_or(0.0),
    );
    let m = local.mul(&ctm);
    blit_with_transform(
        bitmap,
        &text_bitmap.bitmap,
        (
            0,
            0,
            text_bitmap.bitmap.width as i32,
            text_bitmap.bitmap.height as i32,
        ),
        origin,
        m,
        clip,
    );
}

extern "C" fn draw_string(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 5 {
        return report_error(
            out_error,
            "Layer.drawString requires a FontInfo, Appearance, x, y and text",
        );
    }
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let font = match read_font(resolved.engine(), &a[0], "Layer.drawString") {
        Ok(font) => font,
        Err(error) => return report_error(out_error, &error),
    };
    let app = match appearance_ref(engine, &a[1], "Layer.drawString") {
        Ok(app) => app,
        Err(error) => return report_error(out_error, &error),
    };
    let (x, y) = (value_as_f64(&a[2]), value_as_f64(&a[3]));
    let text = value_as_string(&a[4]);
    let paint = text_paint(app);
    let rendered = match render_text(&ctx, &font, &text, &paint) {
        Ok(rendered) => rendered,
        Err(error) => return report_error(out_error, &error),
    };
    let result = with_layer(
        engine,
        objthis,
        "Layer.drawString",
        (rendered.bitmap.width, rendered.bitmap.height),
        |bitmap, layer_draw, clip, ctm| {
            render_and_blit_text(bitmap, &rendered, None, (x, y), ctm, clip);
            let rect = RectF64 {
                x,
                y,
                w: rendered.width,
                h: rendered.height,
            }
            .transformed(ctm);
            if let Some(record) = layer_draw.recording.as_mut() {
                record.push(RecordOp::Blit {
                    src: rendered.bitmap.rgba.clone(),
                    sw: rendered.bitmap.width,
                    sh: rendered.bitmap.height,
                    src_rect: (
                        0,
                        0,
                        rendered.bitmap.width as i32,
                        rendered.bitmap.height as i32,
                    ),
                    p0: (x, y),
                    p1: (x + rendered.bitmap.width as f64, y),
                    p2: (x, y + rendered.bitmap.height as f64),
                });
            }
            Ok(rect)
        },
    );
    match result {
        Ok(update) => {
            set_rectf_result(engine, out, update);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn draw_path_string(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 5 {
        return report_error(
            out_error,
            "Layer.drawPathString requires a FontInfo, Appearance, x, y and text",
        );
    }
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let font = match read_font(resolved.engine(), &a[0], "Layer.drawPathString") {
        Ok(font) => font,
        Err(error) => return report_error(out_error, &error),
    };
    let app = match appearance_ref(engine, &a[1], "Layer.drawPathString") {
        Ok(app) => app,
        Err(error) => return report_error(out_error, &error),
    };
    let (x, y) = (value_as_f64(&a[2]), value_as_f64(&a[3]));
    let text = value_as_string(&a[4]);
    // Render the text once per draw info: brushes fill the glyph coverage,
    // pens stroke it with the coverage dilated by half the pen width.
    let mut sources: Vec<(super::appearance::DrawInfo, BitmapState)> = Vec::new();
    for info in &app.infos {
        let text_bitmap = match render_text(&ctx, &font, &text, &info.paint) {
            Ok(text_bitmap) => text_bitmap,
            Err(error) => return report_error(out_error, &error),
        };
        let source = if info.is_pen {
            let radius = (info.width * 0.5).round().max(0.0) as i32;
            let dilated = dilate_coverage(
                &text_bitmap.coverage,
                text_bitmap.bitmap.width,
                text_bitmap.bitmap.height,
                radius,
            );
            tint_coverage(
                &dilated,
                text_bitmap.bitmap.width,
                text_bitmap.bitmap.height,
                &info.paint,
            )
        } else {
            text_bitmap.bitmap
        };
        sources.push((info.clone(), source));
    }
    let size = sources
        .first()
        .map(|(_, bitmap)| (bitmap.width, bitmap.height))
        .unwrap_or((1, 1));
    let result = with_layer(
        engine,
        objthis,
        "Layer.drawPathString",
        size,
        |bitmap, layer_draw, clip, ctm| {
            for (info, source) in &sources {
                let local = Mat::translate(info.ox, info.oy);
                let m = local.mul(&ctm);
                blit_with_transform(
                    bitmap,
                    source,
                    (0, 0, source.width as i32, source.height as i32),
                    (x, y),
                    m,
                    clip,
                );
            }
            let update = RectF64 {
                x,
                y,
                w: size.0 as f64,
                h: size.1 as f64,
            }
            .transformed(ctm);
            if let Some(record) = layer_draw.recording.as_mut() {
                for (info, source) in &sources {
                    record.push(RecordOp::Blit {
                        src: source.rgba.clone(),
                        sw: source.width,
                        sh: source.height,
                        src_rect: (0, 0, source.width as i32, source.height as i32),
                        p0: (x + info.ox, y + info.oy),
                        p1: (x + info.ox + source.width as f64, y + info.oy),
                        p2: (x + info.ox, y + info.oy + source.height as f64),
                    });
                }
            }
            Ok(update)
        },
    );
    match result {
        Ok(update) => {
            set_rectf_result(engine, out, update);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn measure_string(
    engine: *mut c_void,
    _objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 2 {
        return report_error(
            out_error,
            "Layer.measureString requires a FontInfo and text",
        );
    }
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let font = match read_font(resolved.engine(), &a[0], "Layer.measureString") {
        Ok(font) => font,
        Err(error) => return report_error(out_error, &error),
    };
    let text = value_as_string(&a[1]);
    match render_text(&ctx, &font, &text, &Paint::Solid([0, 0, 0, 0])) {
        Ok(rendered) => {
            set_rectf_result(
                engine,
                out,
                RectF64 {
                    x: 0.0,
                    y: 0.0,
                    w: rendered.width,
                    h: rendered.height,
                },
            );
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn measure_string_internal(
    engine: *mut c_void,
    _objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.len() < 2 {
        return report_error(
            out_error,
            "Layer.measureStringInternal requires a FontInfo and text",
        );
    }
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let font = match read_font(resolved.engine(), &a[0], "Layer.measureStringInternal") {
        Ok(font) => font,
        Err(error) => return report_error(out_error, &error),
    };
    let text = value_as_string(&a[1]);
    match render_text(&ctx, &font, &text, &Paint::Solid([0, 0, 0, 0])) {
        Ok(rendered) => {
            let (x, y, w, h) = rendered.ink.unwrap_or((0.0, 0.0, 0.0, 0.0));
            set_rectf_result(engine, out, RectF64 { x, y, w, h });
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

// ---------------------------------------------------------------------------
// Transforms
// ---------------------------------------------------------------------------

fn update_transforms(
    engine: *mut c_void,
    objthis: *mut c_void,
    method: &str,
    update_view: bool,
    apply: impl FnOnce(&mut LayerDraw),
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let _ = update_view;
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let layer_id = match layer_from_objthis(resolved.engine(), objthis) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return report_error(out_error, &format!("{method}: the receiver is not a Layer"));
        }
        Err(error) => return report_error(out_error, &error),
    };
    {
        let mut state = draw_state(&ctx);
        let layer_draw = state.layers.entry(layer_id).or_default();
        apply(layer_draw);
    }
    // The reference re-renders the recording when the transform changes.
    redraw_recording(engine, objthis);
    set_void(out);
    0
}

extern "C" fn set_transform(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let Some(value) = a.first() else {
        return report_error(out_error, "Layer.setTransform requires a matrix");
    };
    let matrix = match read_matrix(engine, value, "Layer.setTransform") {
        Ok(matrix) => matrix,
        Err(error) => return report_error(out_error, &error),
    };
    update_transforms(
        engine,
        objthis,
        "Layer.setTransform",
        false,
        move |state| state.transform = matrix,
        out,
        out_error,
    )
}

extern "C" fn reset_transform(
    engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    update_transforms(
        engine,
        objthis,
        "Layer.resetTransform",
        false,
        |state| state.transform = Mat::IDENTITY,
        out,
        out_error,
    )
}

extern "C" fn rotate_transform(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let angle = a.first().map(value_as_f64).unwrap_or(0.0);
    update_transforms(
        engine,
        objthis,
        "Layer.rotateTransform",
        false,
        move |state| state.transform = state.transform.mul(&Mat::rotate(angle)),
        out,
        out_error,
    )
}

extern "C" fn scale_transform(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let sx = a.first().map(value_as_f64).unwrap_or(1.0);
    let sy = a.get(1).map(value_as_f64).unwrap_or(sx);
    update_transforms(
        engine,
        objthis,
        "Layer.scaleTransform",
        false,
        move |state| state.transform = state.transform.mul(&Mat::scale(sx, sy)),
        out,
        out_error,
    )
}

extern "C" fn translate_transform(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let dx = a.first().map(value_as_f64).unwrap_or(0.0);
    let dy = a.get(1).map(value_as_f64).unwrap_or(0.0);
    update_transforms(
        engine,
        objthis,
        "Layer.translateTransform",
        false,
        move |state| state.transform = state.transform.mul(&Mat::translate(dx, dy)),
        out,
        out_error,
    )
}

extern "C" fn set_view_transform(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let Some(value) = a.first() else {
        return report_error(out_error, "Layer.setViewTransform requires a matrix");
    };
    let matrix = match read_matrix(engine, value, "Layer.setViewTransform") {
        Ok(matrix) => matrix,
        Err(error) => return report_error(out_error, &error),
    };
    update_transforms(
        engine,
        objthis,
        "Layer.setViewTransform",
        true,
        move |state| state.view_transform = matrix,
        out,
        out_error,
    )
}

extern "C" fn reset_view_transform(
    engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    update_transforms(
        engine,
        objthis,
        "Layer.resetViewTransform",
        true,
        |state| state.view_transform = Mat::IDENTITY,
        out,
        out_error,
    )
}

extern "C" fn rotate_view_transform(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let angle = a.first().map(value_as_f64).unwrap_or(0.0);
    update_transforms(
        engine,
        objthis,
        "Layer.rotateViewTransform",
        true,
        move |state| state.view_transform = state.view_transform.mul(&Mat::rotate(angle)),
        out,
        out_error,
    )
}

extern "C" fn scale_view_transform(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let sx = a.first().map(value_as_f64).unwrap_or(1.0);
    let sy = a.get(1).map(value_as_f64).unwrap_or(sx);
    update_transforms(
        engine,
        objthis,
        "Layer.scaleViewTransform",
        true,
        move |state| state.view_transform = state.view_transform.mul(&Mat::scale(sx, sy)),
        out,
        out_error,
    )
}

extern "C" fn translate_view_transform(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let dx = a.first().map(value_as_f64).unwrap_or(0.0);
    let dy = a.get(1).map(value_as_f64).unwrap_or(0.0);
    update_transforms(
        engine,
        objthis,
        "Layer.translateViewTransform",
        true,
        move |state| state.view_transform = state.view_transform.mul(&Mat::translate(dx, dy)),
        out,
        out_error,
    )
}

// ---------------------------------------------------------------------------
// Record
// ---------------------------------------------------------------------------

fn redraw_recording(engine: *mut c_void, objthis: *mut c_void) {
    let Ok((resolved, ctx)) = bound(engine) else {
        return;
    };
    let Ok(Some(layer_id)) = layer_from_objthis(resolved.engine(), objthis) else {
        return;
    };
    let ops = {
        let state = draw_state(&ctx);
        match state
            .layers
            .get(&layer_id)
            .and_then(|layer| layer.recording.clone())
        {
            Some(ops) if !ops.is_empty() => ops,
            _ => return,
        }
    };
    let scene = match scene_mut(&ctx) {
        Ok(scene) => scene,
        Err(_) => return,
    };
    let Some(bitmap_id) = scene.layer(layer_id).and_then(|layer| layer.bitmap) else {
        return;
    };
    let clip = layer_clip(scene, layer_id);
    let ctm = {
        let state = draw_state(&ctx);
        state
            .layers
            .get(&layer_id)
            .map(LayerDraw::calc_transform)
            .unwrap_or_default()
    };
    if let Some(bitmap) = scene.bitmap_mut(bitmap_id) {
        raster::fill_rect_replace(bitmap, 0, 0, bitmap.width, bitmap.height, [0, 0, 0, 0]);
        replay_record(bitmap, &ops, ctm, clip);
    }
    mark_layer_paint(scene, layer_id);
}

extern "C" fn record_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let layer_id = match layer_from_objthis(resolved.engine(), objthis) {
        Ok(Some(id)) => id,
        Ok(None) => return report_error(out_error, "Layer.record: the receiver is not a Layer"),
        Err(error) => return report_error(out_error, &error),
    };
    let state = draw_state(&ctx);
    let recording = state
        .layers
        .get(&layer_id)
        .and_then(|layer| layer.recording.as_ref())
        .is_some();
    set_bool(out, recording);
    0
}

extern "C" fn record_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let layer_id = match layer_from_objthis(resolved.engine(), objthis) {
        Ok(Some(id)) => id,
        Ok(None) => return report_error(out_error, "Layer.record: the receiver is not a Layer"),
        Err(error) => return report_error(out_error, &error),
    };
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let enabled = value_as_bool(unsafe { &*value });
    let mut state = draw_state(&ctx);
    let layer_draw = state.layers.entry(layer_id).or_default();
    if enabled {
        if layer_draw.recording.is_none() {
            layer_draw.recording = Some(Vec::new());
        }
    } else {
        layer_draw.recording = None;
    }
    0
}

extern "C" fn get_record_image(
    engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let host = resolved.engine();
    let layer_id = match layer_from_objthis(host, objthis) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return report_error(
                out_error,
                "Layer.getRecordImage: the receiver is not a Layer",
            );
        }
        Err(error) => return report_error(out_error, &error),
    };
    let (ops, ctm, size) = {
        let state = draw_state(&ctx);
        let Some(layer_draw) = state.layers.get(&layer_id) else {
            set_void(out);
            return 0;
        };
        let Some(ops) = layer_draw.recording.clone() else {
            set_void(out);
            return 0;
        };
        (ops, layer_draw.calc_transform(), (1u32, 1u32))
    };
    let (width, height) = {
        let scene = super::scene_ref(&ctx).ok();
        scene
            .and_then(|scene| scene.layer(layer_id))
            .map(|layer| (layer.image_width.max(1), layer.image_height.max(1)))
            .unwrap_or(size)
    };
    let expression = format!("new Bitmap({width}, {height})");
    let object = match host.eval_retained(&expression, "Layer.getRecordImage") {
        Ok(tjs::RetainedValue::Object(object)) => object,
        Ok(tjs::RetainedValue::Value(_)) => {
            return report_error(
                out_error,
                "Layer.getRecordImage: `new Bitmap` returned a scalar",
            );
        }
        Err(error) => return report_error(out_error, &error.to_string()),
    };
    let bitmap_id = match crate::CompatCtx::native_instance_id(host, object.raw_id(), &["Bitmap"]) {
        Ok(Some(id)) => id,
        Ok(None) => {
            return report_error(
                out_error,
                "Layer.getRecordImage: the bitmap is not a Bitmap",
            );
        }
        Err(error) => return report_error(out_error, &error),
    };
    if let Ok(scene) = scene_mut(&ctx)
        && let Some(bitmap) = scene.bitmap_mut(bitmap_id)
    {
        let clip = (0, 0, width as i32, height as i32);
        replay_record(bitmap, &ops, ctm, clip);
    }
    set_retained_out(out, object.raw_id());
    std::mem::forget(object);
    0
}

extern "C" fn redraw_record(
    engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Ok((resolved, _ctx)) = bound(engine) else {
        return report_error(out_error, "Layer.redrawRecord: compat context is not bound");
    };
    let has_recording = layer_from_objthis(resolved.engine(), objthis)
        .ok()
        .flatten()
        .is_some();
    if !has_recording {
        return report_error(out_error, "Layer.redrawRecord: the receiver is not a Layer");
    }
    redraw_recording(engine, objthis);
    set_bool(out, true);
    0
}

extern "C" fn save_record(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let Some(value) = a.first() else {
        return report_error(out_error, "Layer.saveRecord requires a filename");
    };
    let filename = value_as_string(value);
    // The reference writes the metafile through `TVPCreateIStream(filename, ...)`
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1856-1875`),
    // so the media prefix is L3 syntax; the host filesystem takes the bare name.
    let lookup = strip_file_media(&filename);
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let Ok(Some(layer_id)) = layer_from_objthis(resolved.engine(), objthis) else {
        return report_error(out_error, "Layer.saveRecord: the receiver is not a Layer");
    };
    let ops = {
        let state = draw_state(&ctx);
        state
            .layers
            .get(&layer_id)
            .and_then(|layer| layer.recording.clone())
    };
    let Some(ops) = ops else {
        set_bool(out, false);
        return 0;
    };
    let file_system = fs::FileSystem::host("");
    match fs::write(&file_system, &FsPath::host(lookup), &encode_record(&ops)) {
        Ok(()) => set_bool(out, true),
        Err(error) => {
            return report_error(out_error, &format!("Layer.saveRecord: {error}"));
        }
    }
    0
}

extern "C" fn load_record(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let Some(value) = a.first() else {
        return report_error(out_error, "Layer.loadRecord requires a filename");
    };
    let filename = value_as_string(value);
    // The reference reads the metafile through `loadImage(filename)`
    // (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:1890-1897`),
    // so the media prefix is L3 syntax; the host filesystem takes the bare name.
    let lookup = strip_file_media(&filename);
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let Ok(Some(layer_id)) = layer_from_objthis(resolved.engine(), objthis) else {
        return report_error(out_error, "Layer.loadRecord: the receiver is not a Layer");
    };
    let file_system = fs::FileSystem::host("");
    let bytes = match fs::read(&file_system, &FsPath::host(lookup)) {
        Ok(bytes) => bytes,
        Err(error) => return report_error(out_error, &format!("Layer.loadRecord: {error}")),
    };
    let ops = match decode_record(&bytes) {
        Ok(ops) => ops,
        Err(error) => return report_error(out_error, &format!("Layer.loadRecord: {error}")),
    };
    {
        let mut state = draw_state(&ctx);
        state.layers.entry(layer_id).or_default().recording = Some(ops);
    }
    redraw_recording(engine, objthis);
    set_bool(out, true);
    0
}

// ---------------------------------------------------------------------------
// Transforms unchanged helper (view/update)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

macro_rules! draw_property {
    ($get:ident, $set:ident, $field:ident, $ty:ty, $default:expr) => {
        extern "C" fn $get(
            engine: *mut c_void,
            objthis: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
        ) -> c_int {
            let (resolved, ctx) = match bound(engine) {
                Ok(value) => value,
                Err(error) => return report_error(out_error, &error),
            };
            let Ok(Some(layer_id)) = layer_from_objthis(resolved.engine(), objthis) else {
                return report_error(out_error, "Layer: the receiver is not a Layer");
            };
            let state = draw_state(&ctx);
            let value = state
                .layers
                .get(&layer_id)
                .map(|layer| layer.$field)
                .unwrap_or($default);
            set_real(out, value as f64);
            0
        }

        extern "C" fn $set(
            engine: *mut c_void,
            objthis: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
        ) -> c_int {
            let (resolved, ctx) = match bound(engine) {
                Ok(value) => value,
                Err(error) => return report_error(out_error, &error),
            };
            let Ok(Some(layer_id)) = layer_from_objthis(resolved.engine(), objthis) else {
                return report_error(out_error, "Layer: the receiver is not a Layer");
            };
            // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
            let new_value = value_as_f64(unsafe { &*value });
            let mut state = draw_state(&ctx);
            let layer_draw = state.layers.entry(layer_id).or_default();
            layer_draw.$field = new_value as $ty;
            0
        }
    };
}

draw_property!(smoothing_get, smoothing_set, smoothing, i32, 4);
draw_property!(text_hint_get, text_hint_set, text_hint, i32, 4);

extern "C" fn update_when_draw_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let Ok(Some(layer_id)) = layer_from_objthis(resolved.engine(), objthis) else {
        return report_error(
            out_error,
            "Layer.updateWhenDraw: the receiver is not a Layer",
        );
    };
    let state = draw_state(&ctx);
    let value = state
        .layers
        .get(&layer_id)
        .map(|layer| layer.update_when_draw)
        .unwrap_or(true);
    set_bool(out, value);
    0
}

extern "C" fn update_when_draw_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let (resolved, ctx) = match bound(engine) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &error),
    };
    let Ok(Some(layer_id)) = layer_from_objthis(resolved.engine(), objthis) else {
        return report_error(
            out_error,
            "Layer.updateWhenDraw: the receiver is not a Layer",
        );
    };
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let enabled = value_as_bool(unsafe { &*value });
    let mut state = draw_state(&ctx);
    state.layers.entry(layer_id).or_default().update_when_draw = enabled;
    0
}

// ---------------------------------------------------------------------------
// Registration surface
// ---------------------------------------------------------------------------

/// The `Layer`-instance method definitions added by `LayerExDraw`.
pub(super) fn layer_method_defs() -> Vec<NativeObjThisMethodDef> {
    vec![
        NativeObjThisMethodDef {
            name: "setViewTransform",
            f: set_view_transform,
        },
        NativeObjThisMethodDef {
            name: "resetViewTransform",
            f: reset_view_transform,
        },
        NativeObjThisMethodDef {
            name: "rotateViewTransform",
            f: rotate_view_transform,
        },
        NativeObjThisMethodDef {
            name: "scaleViewTransform",
            f: scale_view_transform,
        },
        NativeObjThisMethodDef {
            name: "translateViewTransform",
            f: translate_view_transform,
        },
        NativeObjThisMethodDef {
            name: "setTransform",
            f: set_transform,
        },
        NativeObjThisMethodDef {
            name: "resetTransform",
            f: reset_transform,
        },
        NativeObjThisMethodDef {
            name: "rotateTransform",
            f: rotate_transform,
        },
        NativeObjThisMethodDef {
            name: "scaleTransform",
            f: scale_transform,
        },
        NativeObjThisMethodDef {
            name: "translateTransform",
            f: translate_transform,
        },
        NativeObjThisMethodDef {
            name: "clear",
            f: clear,
        },
        NativeObjThisMethodDef {
            name: "drawPath",
            f: draw_path,
        },
        NativeObjThisMethodDef {
            name: "drawArc",
            f: draw_arc,
        },
        NativeObjThisMethodDef {
            name: "drawPie",
            f: draw_pie,
        },
        NativeObjThisMethodDef {
            name: "drawBezier",
            f: draw_bezier,
        },
        NativeObjThisMethodDef {
            name: "drawBeziers",
            f: draw_beziers,
        },
        NativeObjThisMethodDef {
            name: "drawClosedCurve",
            f: draw_closed_curve,
        },
        NativeObjThisMethodDef {
            name: "drawClosedCurve2",
            f: draw_closed_curve2,
        },
        NativeObjThisMethodDef {
            name: "drawCurve",
            f: draw_curve,
        },
        NativeObjThisMethodDef {
            name: "drawCurve2",
            f: draw_curve2,
        },
        NativeObjThisMethodDef {
            name: "drawCurve3",
            f: draw_curve3,
        },
        NativeObjThisMethodDef {
            name: "drawEllipse",
            f: draw_ellipse,
        },
        NativeObjThisMethodDef {
            name: "drawLine",
            f: draw_line,
        },
        NativeObjThisMethodDef {
            name: "drawLines",
            f: draw_lines,
        },
        NativeObjThisMethodDef {
            name: "drawPolygon",
            f: draw_polygon,
        },
        NativeObjThisMethodDef {
            name: "drawRectangle",
            f: draw_rectangle,
        },
        NativeObjThisMethodDef {
            name: "drawRectangles",
            f: draw_rectangles,
        },
        NativeObjThisMethodDef {
            name: "drawPathString",
            f: draw_path_string,
        },
        NativeObjThisMethodDef {
            name: "drawString",
            f: draw_string,
        },
        NativeObjThisMethodDef {
            name: "measureString",
            f: measure_string,
        },
        NativeObjThisMethodDef {
            name: "measureStringInternal",
            f: measure_string_internal,
        },
        NativeObjThisMethodDef {
            name: "drawImage",
            f: draw_image,
        },
        NativeObjThisMethodDef {
            name: "drawImageRect",
            f: draw_image_rect,
        },
        NativeObjThisMethodDef {
            name: "drawImageStretch",
            f: draw_image_stretch,
        },
        NativeObjThisMethodDef {
            name: "drawImageAffine",
            f: draw_image_affine,
        },
        NativeObjThisMethodDef {
            name: "getRecordImage",
            f: get_record_image,
        },
        NativeObjThisMethodDef {
            name: "redrawRecord",
            f: redraw_record,
        },
        NativeObjThisMethodDef {
            name: "saveRecord",
            f: save_record,
        },
        NativeObjThisMethodDef {
            name: "loadRecord",
            f: load_record,
        },
    ]
}

/// The `Layer`-instance property definitions added by `LayerExDraw`.
pub(super) fn layer_property_defs() -> Vec<NativeObjThisPropertyDef> {
    vec![
        NativeObjThisPropertyDef {
            name: "updateWhenDraw",
            get: Some(update_when_draw_get),
            set: Some(update_when_draw_set),
        },
        NativeObjThisPropertyDef {
            name: "smoothingMode",
            get: Some(smoothing_get),
            set: Some(smoothing_set),
        },
        NativeObjThisPropertyDef {
            name: "textRenderingHint",
            get: Some(text_hint_get),
            set: Some(text_hint_set),
        },
        NativeObjThisPropertyDef {
            name: "record",
            get: Some(record_get),
            set: Some(record_set),
        },
    ]
}
