//! CPU shape rasterizer for the `GdiPlus` `Layer.draw*` methods.
//!
//! The L1 pipeline already uploads any dirty [`BitmapState`], so the vector
//! drawing operations rasterize shapes directly into a layer's attached
//! bitmap. This module keeps that pixel work separate from the layer-state
//! operations in [`crate::layer_ops`].
//!
//! Every operation that writes pixels marks its target bitmap dirty; a caller
//! never has to remember to do it.
//!
//! Coordinates are layer-local pixels with the scene's y-down convention.
//! Anti-aliasing is a 4×4 supersample of the even-odd fill; strokes use the
//! per-pixel distance to the segment for a round, anti-aliased band. All
//! colors are straight-alpha RGBA and are composited with [`blend_pixel`],
//! so the result matches the rest of the middle layer.

use crate::scene::BitmapState;

/// Supersample grid per axis for polygon fills (4×4 = 16 samples/pixel).
const AA: i32 = 4;

/// Alpha-composite one straight-alpha pixel and mark the bitmap dirty. Keeping
/// this in the middle layer makes the scene's RGBA convention explicit and
/// avoids renderer-only drawing paths.
pub fn blend_pixel(
    bitmap: &mut BitmapState,
    x: i32,
    y: i32,
    color: [u8; 4],
    coverage: u8,
    opa: u8,
) {
    if blend_pixel_raw(bitmap, x, y, color, coverage, opa) {
        bitmap.mark_dirty();
    }
}

/// [`blend_pixel`] without the dirty marking: a raster operation writes many
/// pixels and marks its target once. Returns whether the pixel exists.
fn blend_pixel_raw(
    bitmap: &mut BitmapState,
    x: i32,
    y: i32,
    color: [u8; 4],
    coverage: u8,
    opa: u8,
) -> bool {
    if x < 0 || y < 0 || x as u32 >= bitmap.width || y as u32 >= bitmap.height {
        return false;
    }
    let Some(i) = bitmap.pixel_offset(x as u32, y as u32) else {
        return false;
    };
    let Some(pixel) = bitmap.rgba.get(i..i + 4) else {
        return false;
    };
    let mut pixel = [pixel[0], pixel[1], pixel[2], pixel[3]];
    blend_pixel_at(&mut pixel, color, coverage, opa);
    if let Some(destination) = bitmap.rgba.get_mut(i..i + 4) {
        destination.copy_from_slice(&pixel);
    }
    true
}

/// Alpha-composite one straight-alpha pixel in place.
///
/// This is the per-pixel core of [`blend_pixel`], without the bitmap bounds
/// check: the caller owns the pixel, so the operation can run on a row slice
/// of a destination bitmap.
pub fn blend_pixel_at(pixel: &mut [u8; 4], color: [u8; 4], coverage: u8, opa: u8) {
    let alpha = (u32::from(color[3]) * u32::from(coverage) * u32::from(opa) / (255 * 255)) as u8;
    if alpha == 0 {
        return;
    }
    // An opaque source replaces the destination exactly; the general
    // straight-alpha source-over below evaluates to this same pixel.
    if alpha == 255 {
        *pixel = color;
        return;
    }
    let inv = 255u32 - u32::from(alpha);
    let old_a = u32::from(pixel[3]);
    let out_a = u32::from(alpha) + old_a * inv / 255;
    for (channel, &src_color) in color[..3].iter().enumerate() {
        let src = u32::from(src_color);
        let dst = u32::from(pixel[channel]);
        // Keep the scene buffer straight-alpha. This matters for antialiased
        // edges over transparent pixels: RGB must remain the requested color,
        // not color multiplied by coverage.
        pixel[channel] = ((src * u32::from(alpha) * 255 + dst * old_a * inv) / (out_a * 255)) as u8;
    }
    pixel[3] = out_a as u8;
}

/// Replace a rectangle's pixels with a straight-alpha color.
///
/// This is the reference `Layer.fillRect` semantics: `tTVPBaseBitmap::Fill`
/// maps to the `FillARGB` render method, a color **copy** (not a source-over
/// blend), so the requested ARGB value — including its alpha — becomes the
/// pixel. That is what lets `fillRect(0, 0, w, h, RGB(0, 0, 0, 0))` clear an
/// existing message image (`MessageArea.clear()`). The rectangle is clipped
/// to the bitmap; the layer's position is never consulted or changed.
pub fn fill_rect_replace(bitmap: &mut BitmapState, x: i32, y: i32, w: u32, h: u32, color: [u8; 4]) {
    if bitmap.width == 0 || bitmap.height == 0 {
        return;
    }
    let Some(expected) = (bitmap.width as usize)
        .checked_mul(bitmap.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
    else {
        return;
    };
    if bitmap.rgba.len() < expected {
        log::error!(
            "engine: fillRect met a {}x{} bitmap holding {} pixel bytes",
            bitmap.width,
            bitmap.height,
            bitmap.rgba.len()
        );
        return;
    }
    let w = w.min(i32::MAX as u32) as i32;
    let h = h.min(i32::MAX as u32) as i32;
    let x0 = x.max(0);
    let y0 = y.max(0);
    let x1 = x.saturating_add(w).min(bitmap.width as i32);
    let y1 = y.saturating_add(h).min(bitmap.height as i32);
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let width = bitmap.width as usize;
    let row_bytes = (x1 - x0) as usize * 4;
    let buffer_len = bitmap.rgba.len();
    for py in y0..y1 {
        let Some(start) = (py as usize)
            .checked_mul(width)
            .and_then(|row| row.checked_add(x0 as usize))
            .and_then(|offset| offset.checked_mul(4))
        else {
            continue;
        };
        let Some(end) = start.checked_add(row_bytes) else {
            continue;
        };
        // A row is one contiguous run of pixels, so it is written as a whole
        // slice. A run that reaches past the buffer ends at the buffer's end:
        // the pixels that fit are written, the rest keep their own color.
        let Some(destination) = bitmap.rgba.get_mut(start..end.min(buffer_len)) else {
            continue;
        };
        for pixel in destination.chunks_exact_mut(4) {
            pixel.copy_from_slice(&color);
        }
    }
    bitmap.mark_dirty();
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

/// Intersect an inclusive pixel box with the bitmap's bounds. Returns an
/// empty box (`min > max`) when there is nothing to draw.
fn clamp_bounds(
    bitmap: &BitmapState,
    min_x: i32,
    min_y: i32,
    max_x: i32,
    max_y: i32,
) -> (i32, i32, i32, i32) {
    if bitmap.width == 0 || bitmap.height == 0 {
        return (0, 0, -1, -1);
    }
    let max_px = bitmap.width.saturating_sub(1).min(i32::MAX as u32) as i32;
    let max_py = bitmap.height.saturating_sub(1).min(i32::MAX as u32) as i32;
    (
        min_x.max(0),
        min_y.max(0),
        max_x.min(max_px),
        max_y.min(max_py),
    )
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
pub fn bbox_size(pts: &[(f64, f64)]) -> (u32, u32) {
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
pub fn fill_polygon(bitmap: &mut BitmapState, pts: &[(f64, f64)], color: [u8; 4]) {
    let Some((min_x, min_y, max_x, max_y)) = bounds(pts) else {
        return;
    };
    let (min_x, min_y, max_x, max_y) = clamp_bounds(bitmap, min_x, min_y, max_x, max_y);
    if min_x > max_x || min_y > max_y {
        return;
    }
    let samples = (AA * AA) as u32;
    let mut painted = false;
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
                painted = blend_pixel_raw(bitmap, px, py, color, coverage, 255) || painted;
            }
        }
    }
    if painted {
        bitmap.mark_dirty();
    }
}

/// The hatch pattern repeats over an 8×8 cell.
const HATCH_TILE: i32 = 8;

/// Whether pixel `(x, y)` is foreground for the given GDI+ hatch style.
///
/// The reference builds an 8×8 tile from integer rectangles and diagonal
/// bands; this reproduces the named cases with integer tests so the CPU
/// sampler stays allocation-free and a future GPU path can evaluate the same
/// cell. Styles the reference leaves to its forward-diagonal `default` arm
/// share that pattern; `HatchStyleDiagonalBrick` (38) gets a distinct brick
/// because the Windows engine gives it its own brush and the game uses it.
/// Coordinates are layer-local pixels and may be negative.
pub fn hatch_foreground(style: i32, x: i32, y: i32) -> bool {
    let px = x.rem_euclid(HATCH_TILE);
    let py = y.rem_euclid(HATCH_TILE);
    // Reduce the diagonals into the cell so the sums/differences cannot
    // overflow on extreme coordinates; all are taken modulo the 8×8 tile.
    let diag = (px - py).rem_euclid(HATCH_TILE);
    let anti = (px + py).rem_euclid(HATCH_TILE);
    match style {
        // Horizontal / vertical line through the middle of the cell.
        0 => py == 4,
        1 => px == 4,
        // Forward (`\`) / backward (`/`) diagonal.
        2 => diag == 0,
        3 => anti == 0,
        // Cross / diagonal cross.
        4 => px == 4 || py == 4,
        5 => diag == 0 || anti == 0,
        // 5 percent: a single dot per cell.
        6 => px == 0 && py == 0,
        // 50 percent: an every-other-pixel checkerboard (32 of 64 pixels).
        12 => (px + py) % 2 == 0,
        // Zig zag: a horizontal line on every other row.
        36 => py % 2 == 0,
        // Diagonal brick: courses run along `\` with staggered `/` head
        // joints, so the mortar reads as a brick wall on the diagonal.
        38 => {
            let course = diag / 4;
            diag % 4 == 0 || anti == course * 4
        }
        // Weave: two-wide bands every four pixels in both directions.
        40 => px % 4 < 2 || py % 4 < 2,
        // Dotted grid: a dot every four pixels.
        43 => px % 4 == 0 && py % 4 == 0,
        // Small grid: a line every two pixels in both directions.
        48 => px % 2 == 0 || py % 2 == 0,
        // The reference forward-diagonal default, and every other style.
        _ => diag == 0,
    }
}

/// Fill a closed polygon with a two-color hatch approximation.
pub fn fill_polygon_hatch(
    bitmap: &mut BitmapState,
    pts: &[(f64, f64)],
    style: i32,
    fore: [u8; 4],
    back: [u8; 4],
) {
    let Some((min_x, min_y, max_x, max_y)) = bounds(pts) else {
        return;
    };
    let (min_x, min_y, max_x, max_y) = clamp_bounds(bitmap, min_x, min_y, max_x, max_y);
    if min_x > max_x || min_y > max_y {
        return;
    }
    let samples = (AA * AA) as u32;
    let mut painted = false;
    for py in min_y..=max_y {
        for px in min_x..=max_x {
            let (mut fore_hits, mut back_hits) = (0u32, 0u32);
            for sy in 0..AA {
                for sx in 0..AA {
                    let x = px as f64 + (sx as f64 + 0.5) / AA as f64;
                    let y = py as f64 + (sy as f64 + 0.5) / AA as f64;
                    if point_in_polygon(x, y, pts) {
                        if hatch_foreground(style, px, py) {
                            fore_hits += 1;
                        } else {
                            back_hits += 1;
                        }
                    }
                }
            }
            if fore_hits > 0 {
                painted = blend_pixel_raw(
                    bitmap,
                    px,
                    py,
                    fore,
                    ((fore_hits * 255) / samples) as u8,
                    255,
                ) || painted;
            }
            if back_hits > 0 {
                painted = blend_pixel_raw(
                    bitmap,
                    px,
                    py,
                    back,
                    ((back_hits * 255) / samples) as u8,
                    255,
                ) || painted;
            }
        }
    }
    if painted {
        bitmap.mark_dirty();
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
pub fn stroke_polyline(
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
    let max_px = bitmap.width.saturating_sub(1).min(i32::MAX as u32) as i32;
    let max_py = bitmap.height.saturating_sub(1).min(i32::MAX as u32) as i32;
    let mut painted = false;
    let mut segment = |a: (f64, f64), b: (f64, f64)| {
        let min_x = (a.0.min(b.0) - half - 1.0).floor() as i32;
        let min_y = (a.1.min(b.1) - half - 1.0).floor() as i32;
        let max_x = (a.0.max(b.0) + half + 1.0).ceil() as i32;
        let max_y = (a.1.max(b.1) + half + 1.0).ceil() as i32;
        let min_x = min_x.max(0);
        let min_y = min_y.max(0);
        let max_x = max_x.min(max_px);
        let max_y = max_y.min(max_py);
        if min_x > max_x || min_y > max_y {
            return;
        }
        for py in min_y..=max_y {
            for px in min_x..=max_x {
                let d = dist_point_segment((px as f64 + 0.5, py as f64 + 0.5), a, b);
                let coverage = ((half + 0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
                if coverage > 0 {
                    painted = blend_pixel_raw(bitmap, px, py, color, coverage, 255) || painted;
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
    if painted {
        bitmap.mark_dirty();
    }
}

/// Flatten an elliptical arc into a polyline.
///
/// `(x, y, w, h)` is the ellipse's bounding box; angles are degrees, `0` on
/// the +x axis, increasing clockwise (the scene's y-down convention).
pub fn flatten_arc(
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    start_deg: f64,
    sweep_deg: f64,
) -> Vec<(f64, f64)> {
    // Non-finite geometry has no finite polyline; reject it rather than
    // iterating an unbounded/saturated step count. A large but finite sweep
    // is allowed and determines its own (finite) step count.
    if !x.is_finite()
        || !y.is_finite()
        || !w.is_finite()
        || !h.is_finite()
        || !start_deg.is_finite()
        || !sweep_deg.is_finite()
    {
        return Vec::new();
    }
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
pub fn flatten_cubic(
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

#[cfg(test)]
mod tests {
    use super::{fill_polygon_hatch, hatch_foreground};
    use crate::scene::BitmapState;

    /// `createHatchPattern` (`reference/cpp/plugins/layerex_draw/blend2d/
    /// LayerExDraw.cpp:637-730`) fills each 8×8 cell with a foreground
    /// pattern over a foreground/background pair; the Windows `HatchBrush`
    /// (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:640-647`)
    /// gives `HatchStyleDiagonalBrick` (38) its own pattern, distinct from
    /// the forward diagonal (2).
    #[test]
    fn hatch_cell_paints_fore_and_back_and_brick_differs() {
        let cell = |style: i32| -> Vec<bool> {
            let mut out = Vec::new();
            for y in 0..8 {
                for x in 0..8 {
                    out.push(hatch_foreground(style, x, y));
                }
            }
            out
        };

        let forward = cell(2);
        assert!(
            forward.iter().any(|on| *on),
            "the forward diagonal paints foreground"
        );
        assert!(
            forward.iter().any(|on| !*on),
            "the forward diagonal paints background"
        );
        assert_ne!(
            forward,
            cell(38),
            "the brick tile differs from the forward diagonal"
        );
    }

    /// A hatch fill blends both tile colors into the bitmap.
    #[test]
    fn fill_polygon_hatch_paints_two_colors() {
        let mut bitmap = BitmapState::new(8, 8, vec![0u8; 8 * 8 * 4]);
        fill_polygon_hatch(
            &mut bitmap,
            &[(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)],
            2,
            [255, 0, 0, 255],
            [0, 0, 255, 255],
        );
        let mut red = 0;
        let mut blue = 0;
        for y in 0..8 {
            for x in 0..8 {
                let i = bitmap.pixel_offset(x, y).expect("pixel in range");
                if bitmap.rgba[i..i + 4] == [255, 0, 0, 255] {
                    red += 1;
                }
                if bitmap.rgba[i..i + 4] == [0, 0, 255, 255] {
                    blue += 1;
                }
            }
        }
        assert!(red > 0 && blue > 0, "the hatch fill paints both colors");
    }
}
