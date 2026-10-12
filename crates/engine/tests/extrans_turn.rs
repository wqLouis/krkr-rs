//! Integration tests for the `turn` transition (`engine::trans`).
//!
//! The warp table itself is private to `trans`, so these tests recover its
//! geometry from rendered frames: sources are either solid colours or a
//! gradient whose every pixel encodes its own `(x, y)`, which turns the
//! destination back into the table's `{ start, len }` spans and `{ sx, sy,
//! stepx, stepy }` source walk.
//!
//! The reference derives the frame's animation phase from elapsed time as
//! `Phase = floor(progress * span) - ycount * 2`, with
//! `span = 64 + (xcount + ycount) * 2` and block `(x, y)` evaluating at
//! `Phase - (x - y) * 2`; `progress_for_block_phase` inverts that so a test
//! can demand an exact block phase.

use engine::BitmapState;
use engine::trans::{Trans, TransKind};

/// `bgcolor` `0xFF070B0D` — opaque, and distinguishable from every pixel
/// the test sources can produce.
const BG: i64 = 0xFF07_0B0D;
/// [`BG`] as straight-alpha RGBA.
const BG_RGBA: [u8; 4] = [7, 11, 13, 255];

fn solid(width: u32, height: u32, pixel: [u8; 4]) -> BitmapState {
    BitmapState::new(
        width,
        height,
        pixel
            .iter()
            .copied()
            .cycle()
            .take((width * height * 4) as usize)
            .collect(),
    )
}

/// A source whose pixel `(x, y)` stores `[x, y, 200, 255]`, so a rendered
/// frame reveals which source pixel the warp sampled.
fn gradient(width: u32, height: u32) -> BitmapState {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[x as u8, y as u8, 200, 255]);
        }
    }
    BitmapState::new(width, height, rgba)
}

fn at(bitmap: &BitmapState, x: i32, y: i32) -> [u8; 4] {
    let i = bitmap.pixel_offset(x as u32, y as u32).expect("in bounds");
    [
        bitmap.rgba[i],
        bitmap.rgba[i + 1],
        bitmap.rgba[i + 2],
        bitmap.rgba[i + 3],
    ]
}

fn turn(duration_ms: f64) -> Trans {
    Trans::new(TransKind::Turn { background: BG }, duration_ms).unwrap()
}

/// `Trans::apply` over the whole destination (the region has to be read
/// before `dst` is borrowed mutably).
fn apply_full(trans: &Trans, dst: &mut BitmapState, src1: &BitmapState, src2: &BitmapState) {
    let region = Trans::full_region(dst);
    trans.apply(dst, src1, src2, region).unwrap();
}

/// Advance to an exact `progress` (< 1.0 keeps the endpoints with the
/// caller, as documented on `extrans_turn::apply`).
fn step_to(trans: &mut Trans, progress: f64) {
    trans.step(progress * trans.duration_ms());
}

/// Progress that puts block `block` of an `xcount * ycount`-tiled image at
/// reference block phase `n`. Landing on `n + 0.5` keeps `progress`'s `f32`
/// rounding far away from the bucket edges.
fn progress_for_block_phase(xcount: i32, ycount: i32, block: (i32, i32), n: i32) -> f64 {
    let span = 64 + (xcount + ycount) * 2;
    let phase = n + ycount * 2 + (block.0 - block.1) * 2;
    (f64::from(phase) + 0.5) / f64::from(span)
}

/// First warped column of a rendered row (`None` when the row is all
/// background).
fn span_start(dst: &BitmapState, y: i32) -> Option<i32> {
    (0..64).find(|&x| at(dst, x, y) != BG_RGBA)
}

/// The table's stated invariants, observed through a rendered 64×64 frame
/// at block phase 32: every scanline's warped pixels form one contiguous
/// span, both span edges advance monotonically down the tile (the
/// generator walks `(0,0)–α–(63,63)` and `(0,0)–β–(63,63)`), the span
/// starts at `x = 0` on the first row, and the source walk never moves
/// backwards (`stepx >= 0` by construction) nor leaves the 64×64 square.
#[test]
fn warp_spans_are_contiguous_and_monotonic() {
    let src1 = solid(64, 64, [10, 10, 10, 255]);
    let src2 = gradient(64, 64);
    let mut dst = solid(64, 64, [0, 0, 0, 0]);

    let mut trans = turn(1000.0);
    step_to(&mut trans, progress_for_block_phase(1, 1, (0, 0), 32));
    apply_full(&trans, &mut dst, &src1, &src2);

    let mut previous_left = 0i32;
    let mut previous_right = -1i32;
    for y in 0..64 {
        let span: Vec<i32> = (0..64).filter(|&x| at(&dst, x, y) != BG_RGBA).collect();
        assert!(!span.is_empty(), "row {y} must show a warped pixel");
        // One contiguous span per scanline — the table stores a single
        // `{ start, len }` per row.
        let (left, right) = (span[0], *span.last().unwrap());
        assert_eq!(
            span,
            (left..=right).collect::<Vec<i32>>(),
            "row {y} must be one contiguous warped span"
        );
        assert!(
            left >= previous_left && right >= previous_right,
            "row {y} edges must advance monotonically ({left}, {right}) after \
             ({previous_left}, {previous_right})"
        );
        previous_left = left;
        previous_right = right;

        let mut previous_sample_x = -1i32;
        for &x in &span {
            let pixel = at(&dst, x, y);
            assert_eq!(pixel[2], 200, "row {y} column {x} must be a source pixel");
            assert!(
                pixel[0] < 64 && pixel[1] < 64,
                "sample must stay in the square"
            );
            assert!(
                i32::from(pixel[0]) >= previous_sample_x,
                "row {y} sample x must not decrease (stepx >= 0)"
            );
            previous_sample_x = i32::from(pixel[0]);
        }
    }

    // `l(0) = 0`: the first row's span always starts at the tile origin.
    assert_eq!(span_start(&dst, 0), Some(0));
    assert_eq!(at(&dst, 0, 0), [0, 0, 200, 255]);
}

/// The generator's corner cases: at the extremes the diamond degenerates
/// back into the full square (`α = (0, 63)`, `β = (63, 0)` at both `n = 1`
/// and `n = 62`), so the tile is an identity sample of one source and the
/// background never shows. `n = 0`/`n = 63` are the generator's dummy rows
/// and are unreachable (the caller copies a whole source there).
#[test]
fn extreme_block_phases_reproduce_a_whole_source() {
    let src1 = solid(64, 64, [10, 20, 30, 255]);
    let src2 = solid(64, 64, [200, 150, 100, 255]);
    let mut dst = solid(64, 64, [0, 0, 0, 0]);

    let mut first = turn(1000.0);
    step_to(&mut first, progress_for_block_phase(1, 1, (0, 0), 1));
    apply_full(&first, &mut dst, &src1, &src2);
    assert_eq!(dst.rgba, src1.rgba, "n = 1 is an identity map of source 1");

    let mut last = turn(1000.0);
    step_to(&mut last, progress_for_block_phase(1, 1, (0, 0), 62));
    apply_full(&last, &mut dst, &src1, &src2);
    assert_eq!(dst.rgba, src2.rgba, "n = 62 is an identity map of source 2");
}

/// A mid-transition frame shows both halves of the effect: the warped
/// span (block phase 32 samples source 2) and `bgcolor` around it.
#[test]
fn mid_phase_shows_the_warp_and_the_background() {
    let src1 = solid(64, 64, [10, 20, 30, 255]);
    let src2 = solid(64, 64, [200, 150, 100, 255]);
    let mut dst = solid(64, 64, [0, 0, 0, 0]);

    let mut trans = turn(1000.0);
    step_to(&mut trans, progress_for_block_phase(1, 1, (0, 0), 32));
    apply_full(&trans, &mut dst, &src1, &src2);

    // Scanline 32 spans x 31..=32 at block phase 32; scanline 0 is a
    // single warped pixel at x = 0.
    assert_eq!(at(&dst, 0, 32), BG_RGBA, "left of the span is background");
    assert_eq!(at(&dst, 31, 32), [200, 150, 100, 255], "warped source 2");
    assert_eq!(at(&dst, 32, 32), [200, 150, 100, 255], "warped source 2");
    assert_eq!(at(&dst, 33, 32), BG_RGBA, "right of the span is background");
    assert_eq!(at(&dst, 0, 0), [200, 150, 100, 255], "row 0 starts warped");
    assert_eq!(at(&dst, 1, 0), BG_RGBA, "row 0 spans a single pixel");
}

/// The reference's `yy >= Height` escape: a bottom tile walks its source
/// rows past the image and paints `bgcolor` instead of sampling.
#[test]
fn background_replaces_samples_below_the_source() {
    // 64×100: two tile rows, so `span = 70`, `ycount = 2` and progress
    // 0.5 puts the top tile at phase 31 (source 1) and the bottom tile at
    // phase 33 (source 2).
    let src1 = solid(64, 100, [10, 20, 30, 255]);
    let src2 = solid(64, 100, [200, 150, 100, 255]);
    let mut dst = solid(64, 100, [0, 0, 0, 0]);

    let mut trans = turn(1000.0);
    step_to(&mut trans, 0.5);
    apply_full(&trans, &mut dst, &src1, &src2);

    // In-bounds samples: row 0 of the top tile (phase 31) and table row 10
    // of the bottom tile, whose sample row is 64 + 17 = 81 < 100.
    assert_eq!(at(&dst, 0, 0), [10, 20, 30, 255]);
    assert_eq!(at(&dst, 8, 74), [200, 150, 100, 255]);
    assert_eq!(at(&dst, 12, 74), BG_RGBA, "right of that span");

    // Table row 35 of the bottom tile spans x 30..=38. Its source walk
    // starts at row 64 + 59 = 123 — past the 100-pixel source, so the
    // reference writes BGColor — but `stepy` is negative here and the
    // walk climbs back into the image after five pixels: x 30..=34 keep
    // the background, x 35..=38 sample row 64 + 31 = 95 again.
    for x in 30..=34 {
        assert_eq!(
            at(&dst, x, 99),
            BG_RGBA,
            "column {x} samples below the source and must be background"
        );
    }
    for x in 35..=38 {
        assert_eq!(
            at(&dst, x, 99),
            [200, 150, 100, 255],
            "column {x} walks back into the source"
        );
    }
}

/// Tiles turn along the bottom-left → top-right diagonal: with two tile
/// columns the right tile trails the left one by `TURN_WIDTH_FACTOR = 2`
/// phases, so at one instant the left tile already samples source 2 while
/// the right one still samples source 1. Each 64×64 tile also carries its
/// own background fill.
#[test]
fn tiles_phase_offsets_follow_the_diagonal() {
    let src1 = solid(128, 64, [10, 20, 30, 255]);
    let src2 = solid(128, 64, [200, 150, 100, 255]);
    let mut dst = solid(128, 64, [0, 0, 0, 0]);

    let mut trans = turn(1000.0);
    step_to(&mut trans, 0.5);
    apply_full(&trans, &mut dst, &src1, &src2);

    // Left tile at phase 33 (source 2), right tile at phase 31 (source 1).
    assert_eq!(at(&dst, 0, 0), [200, 150, 100, 255]);
    assert_eq!(at(&dst, 64, 0), [10, 20, 30, 255]);
    // Row 0 of both tiles is a single warped pixel; the rest is the tile's
    // own background.
    assert_eq!(at(&dst, 1, 0), BG_RGBA);
    assert_eq!(at(&dst, 65, 0), BG_RGBA);
}

/// The `gloss` flash: at block phase 8 the reference blends every warped
/// pixel of the tile with `Blend(pixel, 0xffffff, 192)` (`common.h`), i.e.
/// RGB toward white and — byte-exactly, alpha byte `0x00` included — the
/// alpha toward zero.
#[test]
fn gloss_flashes_the_turning_tile() {
    let src1 = solid(64, 64, [10, 20, 30, 255]);
    let src2 = solid(64, 64, [200, 150, 100, 255]);
    let mut dst = solid(64, 64, [0, 0, 0, 0]);

    let mut trans = turn(1000.0);
    step_to(&mut trans, progress_for_block_phase(1, 1, (0, 0), 8));
    apply_full(&trans, &mut dst, &src1, &src2);

    // 10 + ((255 - 10) * 192) >> 8 = 193, … and
    // 255 + ((0 - 255) * 192) >> 8 = 63.
    assert_eq!(
        at(&dst, 0, 0),
        [193, 196, 198, 63],
        "gloss blends toward 0xffffff"
    );
    assert_eq!(at(&dst, 1, 0), BG_RGBA, "background is not highlighted");
}
