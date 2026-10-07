//! Integration tests for the CPU shape rasterizer bounds.
//!
//! Script-derived geometry must not turn into unbounded work: arc/polygon
//! loop counts follow from the actual input and terminate, non-finite
//! geometry is rejected, and the scan box is clipped to the bitmap.

use std::f64::consts::TAU;

use engine::{BitmapState, fill_polygon, flatten_arc, flatten_cubic, stroke_polyline};

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
