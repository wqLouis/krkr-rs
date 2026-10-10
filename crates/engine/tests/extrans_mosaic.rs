//! Integration tests for the `mosaic` extrans transition
//! (`engine::trans`, port of `sample/extrans/mosaic.cpp`).
//!
//! Every test drives the public machine — `Trans::new` + `step` +
//! `apply` — and inspects the destination pixels. `mosaic` is `ttExchange`
//! with the endpoints short-circuited by `Trans::apply`, so the interesting
//! frames are the mid-phase ones: there the reference paints flat blocks
//! whose size is `maxsize` and whose colour is the phase's `BlendRatio`
//! blend of a single block-centre sample.

use engine::BitmapState;
use engine::layer_ops::RectI;
use engine::trans::{Trans, TransError, TransKind};

/// A `size x size` bitmap where every pixel equals `pixel`.
fn solid(size: u32, pixel: [u8; 4]) -> BitmapState {
    BitmapState::new(
        size,
        size,
        pixel
            .iter()
            .copied()
            .cycle()
            .take((size * size * 4) as usize)
            .collect(),
    )
}

/// A `size x size` bitmap with `r = x * 16` and `g = y * 16`, so both axes
/// vary and every pixel of a row differs from its neighbours.
fn gradient(size: u32) -> BitmapState {
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            rgba.extend_from_slice(&[(x * 16) as u8, (y * 16) as u8, 0, 255]);
        }
    }
    BitmapState::new(size, size, rgba)
}

/// The full-image processing rectangle.
fn region(size: u32) -> RectI {
    (0, 0, size as i32, size as i32)
}

/// One pixel of `bitmap`.
fn pixel(bitmap: &BitmapState, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * bitmap.width + x) * 4) as usize;
    [
        bitmap.rgba[i],
        bitmap.rgba[i + 1],
        bitmap.rgba[i + 2],
        bitmap.rgba[i + 3],
    ]
}

/// Run lengths of horizontally equal pixels along row `y`.
fn row_runs(bitmap: &BitmapState, y: u32) -> Vec<usize> {
    let mut runs = Vec::new();
    let mut len = 1usize;
    for x in 1..bitmap.width {
        if pixel(bitmap, x, y) == pixel(bitmap, x - 1, y) {
            len += 1;
        } else {
            runs.push(len);
            len = 1;
        }
    }
    runs.push(len);
    runs
}

/// Run lengths of vertically equal pixels along column `x`.
fn column_runs(bitmap: &BitmapState, x: u32) -> Vec<usize> {
    let mut runs = Vec::new();
    let mut len = 1usize;
    for y in 1..bitmap.height {
        if pixel(bitmap, x, y) == pixel(bitmap, x, y - 1) {
            len += 1;
        } else {
            runs.push(len);
            len = 1;
        }
    }
    runs.push(len);
    runs
}

/// A `mosaic` transition with `max_size`, stepped to half of its 100 ms
/// duration (phase 128 — the peak of the block-size triangle wave, where
/// the reference's block size equals `maxsize` for every `maxsize >= 1`).
fn mid_phase(max_size: i32) -> Trans {
    let mut trans = Trans::new(TransKind::Mosaic { max_size }, 100.0).unwrap();
    trans.step(50.0);
    assert_eq!(trans.phase(), 128, "phase 128 is the midpoint of 0..=255");
    trans
}

/// The reference blend both fills use: `a + ((b - a) * opa >> 8)`.
fn blended(s1: [u8; 4], s2: [u8; 4], opa: i32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for c in 0..4 {
        let v = i32::from(s1[c]) + (((i32::from(s2[c]) - i32::from(s1[c])) * opa) >> 8);
        out[c] = v.clamp(0, 255) as u8;
    }
    out
}

#[test]
fn mid_phase_paints_flat_blocks() {
    let size = 16;
    let src1 = gradient(size);
    let src2 = solid(size, [255, 255, 0, 255]);
    let mut dst = solid(size, [0, 0, 0, 0]);

    let trans = mid_phase(8);
    trans.apply(&mut dst, &src1, &src2, region(size)).unwrap();
    assert!(dst.dirty, "the transition must mark the destination dirty");

    // `t = 255 - 128 = 127` → `block = (8 - 2) * 127 / 127 + 2 = 8`, and the
    // centring offset is `(16 - 8) / 2 - (8 / 8 * 8) = -4`, i.e. blocks
    // `[-4, 4)`, `[4, 12)`, `[12, 20)`: a 16 px row runs 4, 8, 4.
    assert_eq!(row_runs(&dst, 8), vec![4, 8, 4]);
    assert_eq!(column_runs(&dst, 6), vec![4, 8, 4]);

    // Every block is one flat colour taken from its own centre sample …
    assert_eq!(pixel(&dst, 0, 8), pixel(&dst, 3, 8));
    assert_eq!(pixel(&dst, 4, 8), pixel(&dst, 11, 8));
    assert_eq!(pixel(&dst, 12, 8), pixel(&dst, 15, 8));
    assert_eq!(pixel(&dst, 6, 4), pixel(&dst, 6, 11));
    // … and neighbouring blocks differ (the gradient moves between them).
    assert_ne!(pixel(&dst, 3, 8), pixel(&dst, 4, 8));
    assert_ne!(pixel(&dst, 11, 8), pixel(&dst, 12, 8));

    // The colour is the phase blend of the block-centre sample, not the
    // raw source: block `[4, 12)` samples `x = 8` → `r1 = 128`,
    // `BlendRatio = 128` → `128 + ((255 - 128) * 128 >> 8) = 191`.
    assert_eq!(pixel(&dst, 4, 8), [191, 191, 0, 255]);
}

#[test]
fn larger_max_size_makes_larger_blocks() {
    let size = 16;
    let src1 = gradient(size);
    let src2 = solid(size, [255, 255, 0, 255]);

    let mut small_dst = solid(size, [0, 0, 0, 0]);
    mid_phase(4)
        .apply(&mut small_dst, &src1, &src2, region(size))
        .unwrap();
    let mut large_dst = solid(size, [0, 0, 0, 0]);
    mid_phase(16)
        .apply(&mut large_dst, &src1, &src2, region(size))
        .unwrap();

    // `maxsize = 4` → block 4, offset `(16 - 4) / 2 - 8 = -2`: runs of
    // 2, 4, 4, 4, 2 pixels. `maxsize = 16` → block 16, offset 0: the whole
    // image is a single block.
    let small_runs = row_runs(&small_dst, 8);
    let large_runs = row_runs(&large_dst, 8);
    assert_eq!(small_runs, vec![2, 4, 4, 4, 2]);
    assert_eq!(large_runs, vec![16]);
    assert!(
        large_runs.iter().max() > small_runs.iter().max(),
        "a larger maxsize must produce larger blocks: {large_runs:?} vs {small_runs:?}"
    );
}

#[test]
fn blend_ratio_moves_with_the_phase() {
    let size = 8;
    let src1 = solid(size, [0, 0, 0, 255]);
    let src2 = solid(size, [255, 0, 0, 255]);

    let mut reds = Vec::new();
    for elapsed in [25.0, 50.0, 75.0] {
        let mut dst = solid(size, [0, 0, 0, 0]);
        let mut trans = Trans::new(TransKind::Mosaic { max_size: 30 }, 100.0).unwrap();
        trans.step(elapsed);
        assert!(trans.phase() > 0 && trans.phase() < trans.phase_max());
        trans.apply(&mut dst, &src1, &src2, region(size)).unwrap();

        // `BlendRatio = CurTime * 255 / Time`; on the `phase_max == 255`
        // clock that is the phase itself.
        let opa = trans.phase() * 255 / trans.phase_max();
        assert_eq!(
            pixel(&dst, 3, 3),
            blended([0, 0, 0, 255], [255, 0, 0, 255], opa)
        );
        reds.push(dst.rgba[0]);
    }
    assert!(
        reds.windows(2).all(|w| w[0] < w[1]),
        "the blend must advance towards source 2 with the phase: {reds:?}"
    );
}

#[test]
fn size_one_degenerates_to_a_per_pixel_dissolve() {
    let size = 16;
    let src1 = gradient(size);
    let src2 = solid(size, [255, 255, 0, 255]);
    let mut dst = solid(size, [0, 0, 0, 0]);

    let trans = mid_phase(1);
    trans.apply(&mut dst, &src1, &src2, region(size)).unwrap();

    // At the triangle's peak `block = (1 - 2) * 127 / 127 + 2 = 1`: every
    // pixel is its own block, its own sample point and its own colour —
    // a plain per-pixel dissolve. The expectation is computed per pixel
    // from that pixel's own source value, so any block of size >= 2 would
    // fail the check.
    let opa = trans.phase() * 255 / trans.phase_max();
    for y in 0..size {
        for x in 0..size {
            assert_eq!(
                pixel(&dst, x, y),
                blended(pixel(&src1, x, y), pixel(&src2, x, y), opa),
                "pixel ({x}, {y}) must blend its own sources"
            );
        }
    }
    // Neighbouring pixels keep the gradient's difference instead of being
    // merged into one block colour.
    assert_ne!(pixel(&dst, 0, 0), pixel(&dst, 1, 0));
    assert_ne!(pixel(&dst, 0, 0), pixel(&dst, 0, 1));
}

#[test]
fn rejects_a_block_size_below_one_pixel() {
    let size = 8;
    let src1 = solid(size, [0, 0, 0, 255]);
    let src2 = solid(size, [255, 255, 255, 255]);
    let mut dst = solid(size, [0, 0, 0, 0]);

    // The reference rejects a bad option in `StartTransition`, i.e. while
    // `Layer.beginTransition` is running, so the typed error surfaces when
    // the transition is built — before any frame is rendered.
    let err = Trans::new(TransKind::Mosaic { max_size: 0 }, 100.0).unwrap_err();
    assert!(
        matches!(
            err,
            TransError::InvalidOption {
                option: "maxsize",
                ..
            }
        ),
        "unexpected error: {err:?}"
    );

    let negative = Trans::new(TransKind::Mosaic { max_size: -4 }, 100.0).unwrap_err();
    assert!(matches!(
        negative,
        TransError::InvalidOption {
            option: "maxsize",
            ..
        }
    ));

    // A well-formed block size still renders through the same path.
    let trans = mid_phase(8);
    trans
        .apply(&mut dst, &src1, &src2, region(size))
        .expect("maxsize 8 renders");
    assert_ne!(
        dst.rgba, src2.rgba,
        "a mid-phase frame is a blend, not source 2"
    );
}
