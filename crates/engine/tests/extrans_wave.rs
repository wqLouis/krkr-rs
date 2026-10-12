//! Integration tests for the `wave` extrans provider (the L2 port of
//! `wave.cpp`'s `tTVPWaveTransHandler`).
//!
//! The tests drive `Trans` through `step` + `apply` and inspect the
//! destination pixels. The expected numbers follow the reference
//! arithmetic: the sine displacement of `StartProcess`, the `>> 8` blend
//! of `common.h`'s `Blend`/`TVPConstAlphaBlend_SD`, and the background
//! strips `Process` fills where a shifted row samples outside the source.

use engine::BitmapState;
use engine::layer_ops::RectI;
use engine::trans::{Trans, TransError, TransKind};

/// `bgcolor1`: fully transparent blue (`0xAARRGGBB` → `[0, 0, 255, 0]`).
const BG1: i64 = 0x0000_00FF;
/// `bgcolor2`: opaque green (`0xAARRGGBB` → `[0, 255, 0, 255]`).
const BG2: i64 = 0xFF00_FF00;

const BG1_RGBA: [u8; 4] = [0, 0, 255, 0];
const BG2_RGBA: [u8; 4] = [0, 255, 0, 255];

fn bitmap(width: u32, height: u32, pixel: [u8; 4]) -> BitmapState {
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

fn region(width: u32, height: u32) -> RectI {
    (0, 0, width as i32, height as i32)
}

/// Opaque red ramp, `R = x * 8` (valid for `width <= 32`).
fn gradient(width: u32, height: u32) -> BitmapState {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[(x * 8) as u8, 0, 0, 255]);
        }
    }
    BitmapState::new(width, height, rgba)
}

/// Opaque black/white vertical edge at `x = split`.
fn edge(width: u32, height: u32, split: u32) -> BitmapState {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..height {
        for x in 0..width {
            let red = if x < split { 0 } else { 255 };
            rgba.extend_from_slice(&[red, 0, 0, 255]);
        }
    }
    BitmapState::new(width, height, rgba)
}

fn pixel(bitmap: &BitmapState, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * bitmap.width + x) * 4) as usize;
    [
        bitmap.rgba[i],
        bitmap.rgba[i + 1],
        bitmap.rgba[i + 2],
        bitmap.rgba[i + 3],
    ]
}

/// First column of row `y` whose red channel clears the dark/bright
/// midpoint (dark side blends to 63, bright side to 191, background 0).
fn first_bright(bitmap: &BitmapState, y: u32) -> Option<u32> {
    (0..bitmap.width).find(|&x| pixel(bitmap, x, y)[0] > 127)
}

fn max_diff(a: [u8; 4], b: [u8; 4]) -> i32 {
    a.iter()
        .zip(b)
        .map(|(x, y)| (i32::from(*x) - i32::from(y)).abs())
        .max()
        .unwrap_or(0)
}

/// A `wave` kind with `maxomega = 0.2` and the two test backgrounds.
fn wave_kind(max_height: i32, wave_type: i32) -> TransKind {
    TransKind::Wave {
        max_height,
        max_omega: 0.2,
        background1: BG1,
        background2: BG2,
        wave_type,
    }
}

#[test]
fn mid_phase_output_differs_from_both_sources() {
    let (width, height) = (32, 16);
    let src1 = gradient(width, height);
    let src2 = bitmap(width, height, [0, 0, 255, 255]);
    let mut dst = bitmap(width, height, [0, 0, 0, 0]);

    let mut trans = Trans::new(wave_kind(10, 0), 100.0).unwrap();
    trans.step(50.0);
    trans
        .apply(&mut dst, &src1, &src2, region(width, height))
        .unwrap();

    assert_ne!(dst.rgba, src1.rgba, "mid-phase is not source 1");
    assert_ne!(dst.rgba, src2.rgba, "mid-phase is not source 2");

    // Row 0 at mid-phase: `tt = 1`, `CurOmega = 0.2`,
    // `d = trunc(sin(-1.6) * 10) = -9`. x = 31 therefore samples x = 40 —
    // outside the 32px source — so the strip shows
    // `Blend(bgcolor1, bgcolor2, 127)`.
    assert_eq!(pixel(&dst, 31, 0), [0, 126, 128, 126]);
    // In bounds the displaced row is the two sources cross-faded:
    // x = 0 samples x = 9 (`R = 72`) of source 1 over source 2 at 127.
    assert_eq!(pixel(&dst, 0, 0), [36, 0, 126, 255]);
}

#[test]
fn larger_max_height_displaces_further() {
    let (width, height) = (48, 16);
    let src1 = edge(width, height, 24);
    let src2 = bitmap(width, height, [128, 128, 128, 255]);

    let render = |max_height: i32| {
        let mut dst = bitmap(width, height, [0, 0, 0, 0]);
        let mut trans = Trans::new(wave_kind(max_height, 0), 100.0).unwrap();
        trans.step(50.0);
        trans
            .apply(&mut dst, &src1, &src2, region(width, height))
            .unwrap();
        dst
    };

    // Mid-phase: `tt = 1`, so `CurH = maxh` and row 15 sits at
    // `sin(0.2 * (15 - 8)) = sin(1.4) = 0.9854` — `d` is 3 px for
    // `max_height = 4` and 19 px for `max_height = 20`, pushing the edge
    // from its resting place at x = 24 to 27 and 43.
    let small = first_bright(&render(4), 15).expect("the edge stays visible");
    let large = first_bright(&render(20), 15).expect("the edge stays visible");

    assert!(small > 24, "edge moved right of 24: {small}");
    assert!(
        large > small,
        "expected further displacement, got {small} then {large}"
    );
}

#[test]
fn wave_type_changes_the_pattern() {
    let (width, height) = (32, 16);
    let src1 = gradient(width, height);
    let src2 = bitmap(width, height, [0, 0, 255, 255]);

    let render = |wave_type: i32| {
        let mut dst = bitmap(width, height, [0, 0, 0, 0]);
        let mut trans = Trans::new(wave_kind(40, wave_type), 100.0).unwrap();
        trans.step(25.0);
        trans
            .apply(&mut dst, &src1, &src2, region(width, height))
            .unwrap();
        dst.rgba
    };

    // Quarter phase: `CurOmega` is `0.2 * tt = 0.1414` (type 0),
    // `0.15` (type 1, easing out) and `0.05` (type 2, easing in), so the
    // per-row shifts — and with them the background strips and the
    // displaced samples — differ between all three shapes.
    let (kind0, kind1, kind2) = (render(0), render(1), render(2));
    assert_ne!(kind0, kind1, "wavetype 1 differs from 0");
    assert_ne!(kind1, kind2, "wavetype 2 differs from 1");
    assert_ne!(kind0, kind2, "wavetype 2 differs from 0");
}

#[test]
fn background_colours_fill_where_a_sample_is_out_of_bounds() {
    let (width, height) = (32, 16);
    let src1 = bitmap(width, height, [255, 0, 0, 255]);
    let src2 = bitmap(width, height, [0, 0, 0, 255]);

    let render = |elapsed: f64| {
        let mut dst = bitmap(width, height, [0, 0, 0, 0]);
        let mut trans = Trans::new(wave_kind(64, 0), 100.0).unwrap();
        trans.step(elapsed);
        trans
            .apply(&mut dst, &src1, &src2, region(width, height))
            .unwrap();
        dst
    };

    // 10 ms in: `tt = sin(pi/10) = 0.3090`, `CurH = 19`, row 0 shifts by
    // `d = -9`, so x = 31 samples x = 40 (out of bounds) and shows
    // `Blend(bgcolor1, bgcolor2, BlendRatio = 25)` — nearly bgcolor1.
    let early = pixel(&render(10.0), 31, 0);
    assert_eq!(early, [0, 24, 230, 24]);
    assert!(
        max_diff(early, BG1_RGBA) <= 30,
        "{early:?} is not nearly bgcolor1"
    );
    assert!(
        max_diff(early, BG2_RGBA) > 200,
        "{early:?} must be unlike bgcolor2"
    );

    // 90 ms in: the wave is symmetric (`t = 100 - 90 = 10`, same `d`) but
    // `BlendRatio = 229`, so the same out-of-bounds strip shows nearly
    // bgcolor2.
    let late = pixel(&render(90.0), 31, 0);
    assert_eq!(late, [0, 228, 26, 228]);
    assert!(
        max_diff(late, BG2_RGBA) <= 30,
        "{late:?} is not nearly bgcolor2"
    );
    assert!(
        max_diff(late, BG1_RGBA) > 200,
        "{late:?} must be unlike bgcolor1"
    );

    // In bounds the sources are blended instead: x = 0 samples x = 9 of
    // the red source over black at `BlendRatio = 25`.
    assert_eq!(pixel(&render(10.0), 0, 0), [230, 0, 0, 255]);
}

#[test]
fn out_of_range_options_return_typed_errors() {
    let (width, height) = (4, 4);
    let src1 = bitmap(width, height, [255, 0, 0, 255]);
    let src2 = bitmap(width, height, [0, 0, 0, 255]);
    let mut dst = bitmap(width, height, [0, 0, 0, 0]);
    let region = region(width, height);

    // `wavetype` selects one of exactly three schedules; the reference's
    // `switch` has no `default` and would read a stale `CurOmega`. The
    // reference rejects it in `StartTransition` — while `beginTransition`
    // runs — so it surfaces when the transition is built.
    assert_eq!(
        Trans::new(wave_kind(50, 3), 100.0).unwrap_err(),
        TransError::InvalidOption {
            option: "wavetype",
            reason: "must be 0, 1 or 2",
        },
    );

    // `time < 2` would make `HalfTime = time / 2` zero; the reference
    // silently raises it to 2, L2 reports it instead (D11).
    let mut short_time = Trans::new(wave_kind(50, 0), 1.0).unwrap();
    short_time.step(0.5);
    assert_eq!(
        short_time.apply(&mut dst, &src1, &src2, region),
        Err(TransError::InvalidOption {
            option: "time",
            reason: "must be at least 2 ms",
        }),
    );
}
