//! Integration tests for the CPU shape rasterizer bounds, hatch coverage and
//! dirty-marking contract.
//!
//! Script-derived geometry must not turn into unbounded work: arc/polygon
//! loop counts follow from the actual input and terminate, non-finite
//! geometry is rejected, and the scan box is clipped to the bitmap.

use std::f64::consts::TAU;

use engine::BitmapState;
use engine::raster::{
    blend_pixel, fill_polygon, fill_polygon_hatch, fill_rect_replace, flatten_arc, flatten_cubic,
    hatch_foreground, stroke_polyline,
};

fn bitmap(width: u32, height: u32) -> BitmapState {
    BitmapState::new(width, height, vec![0u8; (width * height * 4) as usize])
}

#[test]
fn flatten_arc_rejects_non_finite_sweep() {
    // A non-finite sweep has no finite polyline: reject rather than iterate a
    // saturated step count.
    assert!(flatten_arc(0.0, 0.0, 10.0, 10.0, 0.0, f64::INFINITY).is_empty());
    assert!(flatten_arc(0.0, 0.0, 10.0, 10.0, 0.0, f64::NAN).is_empty());
}

#[test]
fn flatten_arc_follows_a_large_valid_sweep() {
    // 3600° / 4° = 900 segments, i.e. 901 points; no cap is involved.
    let pts = flatten_arc(0.0, 0.0, 10.0, 10.0, 0.0, 3600.0);
    assert_eq!(pts.len(), 901);
    // A full turn still yields a finite polyline.
    let pts = flatten_arc(0.0, 0.0, 10.0, 10.0, 0.0, 360.0);
    assert!(pts.len() >= 3);
}

#[test]
fn flatten_cubic_respects_large_valid_step_count() {
    assert_eq!(
        flatten_cubic((0.0, 0.0), (1.0, 1.0), (2.0, 1.0), (3.0, 0.0), 10).len(),
        11
    );
    let pts = flatten_cubic((0.0, 0.0), (1.0, 1.0), (2.0, 1.0), (3.0, 0.0), 100_000);
    assert_eq!(pts.len(), 100_001);
}

#[test]
fn huge_polygon_bounds_are_clamped_to_the_bitmap() {
    let mut bmp = bitmap(4, 4);
    // Bounds far outside the bitmap must not loop over the whole i32 range;
    // the scan box is clipped to the actual bitmap.
    fill_polygon(
        &mut bmp,
        &[(-1.0e12, -1.0e12), (1.0e12, -1.0e12), (0.0, 1.0e12)],
        [255, 0, 0, 255],
    );
    assert_eq!(bmp.rgba.len(), 4 * 4 * 4);
}

#[test]
fn huge_stroke_bounds_are_clamped_to_the_bitmap() {
    let mut bmp = bitmap(4, 4);
    stroke_polyline(
        &mut bmp,
        &[(-1.0e12, 0.0), (1.0e12, 0.0)],
        false,
        [0, 255, 0, 255],
        1.0,
    );
    assert_eq!(bmp.rgba.len(), 4 * 4 * 4);
}

#[test]
fn fill_polygon_accepts_a_many_vertex_path() {
    // A large-but-valid path is allowed: the work follows from the vertex
    // count and the (small) bitmap area, so no point cap applies.
    let pts: Vec<(f64, f64)> = (0..10_000)
        .map(|i| {
            let a = (i as f64) / 10_000.0 * TAU;
            (2.0 * a.cos(), 2.0 * a.sin())
        })
        .collect();
    let mut bmp = bitmap(1, 1);
    fill_polygon(&mut bmp, &pts, [255, 0, 0, 255]);
    assert!(bmp.rgba[3] > 0, "the many-vertex circle fills the pixel");
}

/// `HatchStyle50Percent` is a one-pixel checkerboard: 32 of the 64 pixels in
/// the 8×8 tile are foreground (`createHatchPattern`,
/// `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:683-689`), so
/// the tile covers exactly half its area.
#[test]
fn hatch_percent50_is_a_half_cover_checkerboard() {
    let mut foreground = 0;
    for y in 0..8 {
        for x in 0..8 {
            let on = hatch_foreground(12, x, y);
            assert_ne!(
                on,
                hatch_foreground(12, x + 1, y),
                "adjacent pixels alternate horizontally at ({x}, {y})"
            );
            assert_ne!(
                on,
                hatch_foreground(12, x, y + 1),
                "adjacent pixels alternate vertically at ({x}, {y})"
            );
            if on {
                foreground += 1;
            }
        }
    }
    assert_eq!(foreground, 32, "the 50% tile covers half of its 64 pixels");
}

/// A `HatchStyle50Percent` fill paints exactly half foreground and half
/// background over the covered rectangle, not the 25% an
/// even-row/even-column grid would give.
#[test]
fn hatch_percent50_fill_covers_half_the_rectangle() {
    let mut bmp = bitmap(8, 8);
    fill_polygon_hatch(
        &mut bmp,
        &[(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)],
        12,
        [255, 0, 0, 255],
        [0, 0, 255, 255],
    );

    let mut foreground = 0;
    let mut background = 0;
    for y in 0..8 {
        for x in 0..8 {
            let i = bmp.pixel_offset(x, y).expect("pixel in range");
            let pixel = &bmp.rgba[i..i + 4];
            if pixel == [255, 0, 0, 255] {
                foreground += 1;
            } else if pixel == [0, 0, 255, 255] {
                background += 1;
            } else {
                panic!("pixel ({x}, {y}) is neither hatch color");
            }
        }
    }
    assert_eq!(foreground, 32, "half the rectangle is foreground");
    assert_eq!(background, 32, "half the rectangle is background");
}

/// Every raster operation marks its target bitmap dirty itself. The reference
/// drawing operations set `ImageModified` in the operation (e.g.
/// `ImageModified = MainImage->Fill(...) || ImageModified` at
/// `reference/cpp/core/visual/LayerIntf.cpp:4287`; the writable buffer sets it
/// at `:3012-3016`), so a caller never has to remember to do it.
#[test]
fn raster_operations_mark_the_bitmap_dirty() {
    fn assert_marks(operation: &str, run: impl FnOnce(&mut BitmapState)) {
        let mut bmp = bitmap(8, 8);
        assert!(!bmp.dirty, "{operation}: a fresh bitmap starts clean");
        run(&mut bmp);
        assert!(bmp.dirty, "{operation} must mark the bitmap dirty");
        assert!(
            bmp.content_generation > 0,
            "{operation} must bump the content generation"
        );
    }

    let square = [(0.0, 0.0), (8.0, 0.0), (8.0, 8.0), (0.0, 8.0)];
    assert_marks("blend_pixel", |bmp| {
        blend_pixel(bmp, 0, 0, [255, 0, 0, 255], 255, 255);
    });
    assert_marks("fill_rect_replace", |bmp| {
        fill_rect_replace(bmp, 0, 0, 4, 4, [255, 0, 0, 255]);
    });
    assert_marks("fill_polygon", |bmp| {
        fill_polygon(bmp, &square, [255, 0, 0, 255]);
    });
    assert_marks("fill_polygon_hatch", |bmp| {
        fill_polygon_hatch(bmp, &square, 12, [255, 0, 0, 255], [0, 0, 255, 255]);
    });
    assert_marks("stroke_polyline", |bmp| {
        stroke_polyline(bmp, &[(0.0, 0.0), (8.0, 8.0)], false, [255, 0, 0, 255], 1.0);
    });
}
