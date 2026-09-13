//! CPU shape rasterizer for the `GdiPlus` `Layer.draw*` methods.
//!
//! The renderer already uploads any dirty [`BitmapState`] (see
//! `render/src/sync.rs`), so the visual natives rasterize shapes directly
//! into a layer's attached bitmap exactly like `Layer.drawText` does. This
//! module keeps that pixel work separate from the native-callback plumbing in
//! [`super::layer`].
//!
//! Coordinates are layer-local pixels with the scene's y-down convention.
//! Anti-aliasing is a 4×4 supersample of the even-odd fill; strokes use the
//! per-pixel distance to the segment for a round, anti-aliased band. All
//! colors are straight-alpha RGBA and are composited with
//! [`blend_pixel`], so the result matches the rest of the visual crate.

use crate::scene::BitmapState;

/// Supersample grid per axis for polygon fills (4×4 = 16 samples/pixel).
const AA: i32 = 4;

/// Alpha-composite one straight-alpha pixel. Keeping this in the visual
/// crate makes the scene's RGBA convention explicit and avoids renderer-only
/// drawing paths.
pub(crate) fn blend_pixel(
    bitmap: &mut BitmapState,
    x: i32,
    y: i32,
    color: [u8; 4],
    coverage: u8,
    opa: u8,
) {
    if x < 0 || y < 0 || x as u32 >= bitmap.width || y as u32 >= bitmap.height {
        return;
    }
    let Some(i) = bitmap.pixel_offset(x as u32, y as u32) else {
        return;
    };
    let alpha = (u32::from(color[3]) * u32::from(coverage) * u32::from(opa) / (255 * 255)) as u8;
    if alpha == 0 {
        return;
    }
    let inv = 255u32 - u32::from(alpha);
    let old_a = u32::from(bitmap.rgba[i + 3]);
    let out_a = u32::from(alpha) + old_a * inv / 255;
    for (channel, &src_color) in color[..3].iter().enumerate() {
        let src = u32::from(src_color);
        let dst = u32::from(bitmap.rgba[i + channel]);
        // Keep the scene buffer straight-alpha. This matters for antialiased
        // edges over transparent pixels: RGB must remain the requested color,
        // not color multiplied by coverage.
        bitmap.rgba[i + channel] =
            ((src * u32::from(alpha) * 255 + dst * old_a * inv) / (out_a * 255)) as u8;
    }
    bitmap.rgba[i + 3] = out_a as u8;
}

/// Even-odd (alternate) point-in-polygon test at a sub-pixel sample.
fn point_in_polygon(x: f64, y: f64, pts: &[(f64, f64)]) -> bool {
    let mut inside = false;
    let n = pts.len();
    if n < 3 {
        return false;
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
    inside
}

/// Integer bounding box (`min_x, min_y, max_x, max_y`) of a point set, or
/// `None` when there is nothing to fill.
fn bounds(pts: &[(f64, f64)]) -> Option<(i32, i32, i32, i32)> {
    if pts.len() < 3 {
        return None;
    }
    let (mut min_x, mut min_y) = (f64::INFINITY, f64::INFINITY);
    let (mut max_x, mut max_y) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    for &(x, y) in pts {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    if !min_x.is_finite() || !max_x.is_finite() {
        return None;
    }
    Some((
        min_x.floor() as i32,
        min_y.floor() as i32,
        max_x.ceil() as i32,
        max_y.ceil() as i32,
    ))
}

/// The layer-wide allocation size needed to contain an open polyline.
pub(crate) fn bbox_size(pts: &[(f64, f64)]) -> (u32, u32) {
    let (mut max_x, mut max_y) = (0.0f64, 0.0f64);
    for &(x, y) in pts {
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    (max_x.ceil().max(1.0) as u32, max_y.ceil().max(1.0) as u32)
}

/// Fill a closed polygon with a flat color (even-odd rule), anti-aliased by
/// 4×4 supersampling. `pts` is treated as closed; open paths are closed
/// implicitly for filling, matching GDI+ `FillPath`.
pub(crate) fn fill_polygon(bitmap: &mut BitmapState, pts: &[(f64, f64)], color: [u8; 4]) {
    let Some((min_x, min_y, max_x, max_y)) = bounds(pts) else {
        return;
    };
    let samples = (AA * AA) as u32;
    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let mut hits = 0u32;
            for sy in 0..AA {
                for sx in 0..AA {
                    let x = px as f64 + (sx as f64 + 0.5) / AA as f64;
                    let y = py as f64 + (sy as f64 + 0.5) / AA as f64;
                    if point_in_polygon(x, y, pts) {
                        hits += 1;
                    }
                }
            }
            if hits == 0 {
                continue;
            }
            let coverage = ((hits * 255) / samples) as u8;
            if coverage > 0 {
                blend_pixel(bitmap, px, py, color, coverage, 255);
            }
        }
    }
}

/// Whether a hatch pixel is foreground for the given GDI+ hatch style.
///
/// Only the game's `HatchStyleDiagonalBrick` (= 38) matters; the reference's
/// cross-platform build falls through to a forward-diagonal pattern for it,
/// so this approximates every style with a diagonal stripe. Colors are the
/// only thing that has to look right for the debug "missing image" box.
fn hatch_on(x: i32, y: i32, _style: i32) -> bool {
    (x - y).rem_euclid(8) < 3
}

/// Fill a closed polygon with a two-color hatch approximation.
pub(crate) fn fill_polygon_hatch(
    bitmap: &mut BitmapState,
    pts: &[(f64, f64)],
    style: i32,
    fore: [u8; 4],
    back: [u8; 4],
) {
    let Some((min_x, min_y, max_x, max_y)) = bounds(pts) else {
        return;
    };
    let samples = (AA * AA) as u32;
    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let (mut fore_hits, mut back_hits) = (0u32, 0u32);
            for sy in 0..AA {
                for sx in 0..AA {
                    let x = px as f64 + (sx as f64 + 0.5) / AA as f64;
                    let y = py as f64 + (sy as f64 + 0.5) / AA as f64;
                    if point_in_polygon(x, y, pts) {
                        if hatch_on(px, py, style) {
                            fore_hits += 1;
                        } else {
                            back_hits += 1;
                        }
                    }
                }
            }
            if fore_hits > 0 {
                blend_pixel(
                    bitmap,
                    px,
                    py,
                    fore,
                    ((fore_hits * 255) / samples) as u8,
                    255,
                );
            }
            if back_hits > 0 {
                blend_pixel(
                    bitmap,
                    px,
                    py,
                    back,
                    ((back_hits * 255) / samples) as u8,
                    255,
                );
            }
        }
    }
}

/// Shortest distance from `p` to the segment `a`–`b`.
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

/// Stroke an open or closed polyline with a round, anti-aliased band of
/// `width` pixels (minimum 1). Joins/caps are round rather than GDI+ flat;
/// the game's widths are 1–3, where the difference is invisible.
pub(crate) fn stroke_polyline(
    bitmap: &mut BitmapState,
    pts: &[(f64, f64)],
    closed: bool,
    color: [u8; 4],
    width: f64,
) {
    if pts.len() < 2 || color[3] == 0 {
        return;
    }
    let half = width.max(1.0) / 2.0;
    let mut segment = |a: (f64, f64), b: (f64, f64)| {
        let min_x = (a.0.min(b.0) - half - 1.0).floor() as i32;
        let min_y = (a.1.min(b.1) - half - 1.0).floor() as i32;
        let max_x = (a.0.max(b.0) + half + 1.0).ceil() as i32;
        let max_y = (a.1.max(b.1) + half + 1.0).ceil() as i32;
        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let d = dist_point_segment((px as f64 + 0.5, py as f64 + 0.5), a, b);
                let coverage = ((half + 0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
                if coverage > 0 {
                    blend_pixel(bitmap, px, py, color, coverage, 255);
                }
            }
        }
    };
    for i in 0..pts.len() - 1 {
        segment(pts[i], pts[i + 1]);
    }
    if closed && pts.len() >= 3 {
        segment(pts[pts.len() - 1], pts[0]);
    }
}

/// Flatten an elliptical arc into a polyline.
///
/// `(x, y, w, h)` is the ellipse's bounding box; angles are degrees, `0` on
/// the +x axis, increasing clockwise (the scene's y-down convention).
pub(crate) fn flatten_arc(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    start_deg: f64,
    sweep_deg: f64,
) -> Vec<(f64, f64)> {
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    let (rx, ry) = (w / 2.0, h / 2.0);
    let steps = ((sweep_deg.abs() / 4.0).ceil() as usize).max(2);
    (0..=steps)
        .map(|i| {
            let a = (start_deg + sweep_deg * (i as f64) / (steps as f64)).to_radians();
            (cx + rx * a.cos(), cy + ry * a.sin())
        })
        .collect()
}

/// Flatten a cubic Bézier into `steps + 1` points.
pub(crate) fn flatten_cubic(
    p0: (f64, f64),
    c1: (f64, f64),
    c2: (f64, f64),
    p3: (f64, f64),
    steps: usize,
) -> Vec<(f64, f64)> {
    let steps = steps.max(2);
    (0..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            let mt = 1.0 - t;
            let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
            (
                a * p0.0 + b * c1.0 + c * c2.0 + d * p3.0,
                a * p0.1 + b * c1.1 + c * c2.1 + d * p3.1,
            )
        })
        .collect()
}
