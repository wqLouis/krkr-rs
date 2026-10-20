//! Integration tests for the `ripple` transition (`engine::trans`).
//!
//! The provider is pure L2 state (`Trans` + two bitmaps), so every test
//! drives it with `step(500)` on a 1000 ms transition — the mid-phase frame
//! where `extrans_ripple::apply` actually runs — and inspects the pixels.
//! The reference behaviour is `tTVPRippleTransHandler` in `ripple.cpp`.

use engine::BitmapState;
use engine::layer_ops::RectI;
use engine::trans::{Trans, TransError, TransKind};

/// A full-image processing region.
fn region(width: u32, height: u32) -> RectI {
    (0, 0, width as i32, height as i32)
}

/// A flat bitmap of one colour.
fn solid(width: u32, height: u32, color: [u8; 4]) -> BitmapState {
    BitmapState::new(
        width,
        height,
        color
            .iter()
            .copied()
            .cycle()
            .take((width * height * 4) as usize)
            .collect(),
    )
}

/// A bitmap whose red/green channels encode the coordinate (`x * step`,
/// `y * step`), so a displaced sample is visible in the output.
fn gradient(width: u32, height: u32, step: u32) -> BitmapState {
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            rgba.extend_from_slice(&[
                ((x * step) & 0xff) as u8,
                ((y * step) & 0xff) as u8,
                128,
                255,
            ]);
        }
    }
    BitmapState::new(width, height, rgba)
}

/// A `ripple` kind with the given options (`speed` fixed at the default 6).
fn ripple(center: Option<(i32, i32)>, map_width: i32, roundness: f32, max_drift: i32) -> TransKind {
    TransKind::Ripple {
        center,
        map_width,
        roundness,
        speed: 6.0,
        max_drift,
    }
}

/// Render one frame into a destination pre-filled with source 1, so a test
/// can tell "the provider wrote pixels" (output ≠ source 1) from "nothing
/// happened".
///
/// The transition is built and stepped to exactly half of its 1000 ms
/// duration — `phase` 127 of 255, inside the window where `Trans::apply`
/// dispatches to the ripple provider — and returns the error, if any. A bad
/// option the reference rejects in `StartTransition` (i.e. while
/// `Layer.beginTransition` runs) fails at construction; a size-dependent one
/// fails when the frame is rendered.
fn render_in(
    kind: TransKind,
    src1: &BitmapState,
    src2: &BitmapState,
    rect: RectI,
) -> Result<BitmapState, TransError> {
    let mut dst = src1.clone();
    let mut trans = Trans::new(kind, 1000.0)?;
    trans.step(500.0);
    trans.apply(&mut dst, src1, src2, rect)?;
    Ok(dst)
}

#[test]
fn rwidth_outside_the_reference_set_is_rejected() {
    let src1 = gradient(32, 32, 8);
    let src2 = solid(32, 32, [255, 0, 0, 255]);
    let err = render_in(ripple(None, 17, 1.0, 24), &src1, &src2, region(32, 32)).unwrap_err();
    assert!(
        matches!(
            err,
            TransError::InvalidOption {
                option: "rwidth",
                ..
            }
        ),
        "expected an rwidth InvalidOption, got {err:?}"
    );

    // Every other reference width renders: 64 here.
    let dst = render_in(ripple(None, 64, 1.0, 24), &src1, &src2, region(32, 32)).unwrap();
    assert_ne!(dst.rgba, src1.rgba, "64 is a valid rwidth and renders");
}

#[test]
fn non_positive_roundness_is_rejected() {
    let src1 = gradient(32, 32, 8);
    let src2 = solid(32, 32, [255, 0, 0, 255]);
    for roundness in [0.0, -1.0] {
        let err = render_in(
            ripple(None, 128, roundness, 24),
            &src1,
            &src2,
            region(32, 32),
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                TransError::InvalidOption {
                    option: "roundness",
                    ..
                }
            ),
            "roundness {roundness} should be rejected, got {err:?}"
        );
    }
}

#[test]
fn max_drift_outside_the_reference_range_is_rejected() {
    let src1 = gradient(32, 32, 8);
    let src2 = solid(32, 32, [255, 0, 0, 255]);
    for max_drift in [-1, 128, 200] {
        let err = render_in(
            ripple(None, 128, 1.0, max_drift),
            &src1,
            &src2,
            region(32, 32),
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                TransError::InvalidOption {
                    option: "maxdrift",
                    ..
                }
            ),
            "maxdrift {max_drift} should be rejected, got {err:?}"
        );
    }
}

#[test]
fn max_drift_must_stay_below_both_dimensions() {
    // In range (32 < 128) but not smaller than the 32-pixel sources.
    let src1 = gradient(32, 32, 8);
    let src2 = solid(32, 32, [255, 0, 0, 255]);
    let err = render_in(ripple(None, 128, 1.0, 32), &src1, &src2, region(32, 32)).unwrap_err();
    assert!(
        matches!(
            err,
            TransError::InvalidOption {
                option: "maxdrift",
                ..
            }
        ),
        "expected a maxdrift InvalidOption, got {err:?}"
    );

    // 31 is smaller than both dimensions and renders.
    let dst = render_in(ripple(None, 128, 1.0, 31), &src1, &src2, region(32, 32)).unwrap();
    assert_ne!(dst.rgba, src1.rgba, "maxdrift 31 is valid and renders");
}

#[test]
fn centre_outside_the_source_is_rejected() {
    let src1 = gradient(32, 32, 8);
    let src2 = solid(32, 32, [255, 0, 0, 255]);
    for (center, expected) in [
        (Some((32, 16)), "centerx"),
        (Some((-1, 16)), "centerx"),
        (Some((16, -1)), "centery"),
        (Some((16, 32)), "centery"),
    ] {
        let err =
            render_in(ripple(center, 128, 1.0, 24), &src1, &src2, region(32, 32)).unwrap_err();
        match err {
            TransError::InvalidOption { option, .. } => {
                assert_eq!(
                    option, expected,
                    "centre {center:?} should be rejected as {expected}"
                );
            }
            other => panic!("centre {center:?} should be an InvalidOption, got {other:?}"),
        }
    }
}

#[test]
fn default_centre_is_the_image_centre() {
    let src1 = gradient(32, 32, 8);
    let src2 = solid(32, 32, [255, 0, 0, 255]);
    // A region whose own centre would be (12, 12), so a default derived
    // from the region rather than the image would disagree with the
    // explicit (16, 16) below.
    let sub_region = (0, 0, 24, 24);

    let default = render_in(ripple(None, 128, 1.0, 24), &src1, &src2, sub_region).unwrap();
    assert_ne!(
        default.rgba, src1.rgba,
        "the frame must actually render inside the region"
    );
    let explicit = render_in(
        ripple(Some((16, 16)), 128, 1.0, 24),
        &src1,
        &src2,
        sub_region,
    )
    .unwrap();
    assert_eq!(
        default.rgba, explicit.rgba,
        "the default centre must equal (width >> 1, height >> 1)"
    );
}

#[test]
fn an_explicit_centre_moves_the_ripple() {
    let src1 = gradient(32, 32, 8);
    let src2 = solid(32, 32, [255, 0, 0, 255]);
    let default = render_in(ripple(None, 128, 1.0, 24), &src1, &src2, region(32, 32)).unwrap();
    let shifted = render_in(
        ripple(Some((8, 8)), 128, 1.0, 24),
        &src1,
        &src2,
        region(32, 32),
    )
    .unwrap();
    assert_ne!(
        default.rgba, shifted.rgba,
        "moving the ripple centre must move the displacement field"
    );
}

#[test]
fn a_mid_phase_frame_differs_from_both_sources() {
    let src1 = gradient(32, 32, 8);
    let src2 = gradient(32, 32, 4);
    let dst = render_in(ripple(None, 128, 1.0, 24), &src1, &src2, region(32, 32)).unwrap();
    assert_ne!(dst.rgba, src1.rgba, "the frame must leave source 1");
    assert_ne!(
        dst.rgba, src2.rgba,
        "the frame must not collapse to source 2"
    );
}

#[test]
fn ripple_samples_converge_on_the_requested_centre() {
    // Source 1 encodes its own coordinates; source 2 is black, so the
    // blended red/green channels reveal the sampled coordinate through the
    // fixed attenuation of the frame's 127/256 blend ratio (elapsed
    // 500/1000 ms). Because `dx`/`dy` are never negative, every sample is
    // pulled toward the requested centre: right/below it samples at
    // `x - dx`/`y - dy`, left/above at `x + dx`/`y + dy`.
    let (width, height) = (64u32, 64u32);
    let mut pixels = Vec::with_capacity((width * height * 4) as usize);
    for y in 0..height {
        for x in 0..width {
            pixels.extend_from_slice(&[x as u8, y as u8, 0, 255]);
        }
    }
    let src1 = BitmapState::new(width, height, pixels);
    let src2 = solid(width, height, [0, 0, 0, 255]);

    let dst = render_in(
        ripple(Some((40, 40)), 128, 1.0, 16),
        &src1,
        &src2,
        region(width, height),
    )
    .unwrap();

    // `blend_level([s, …], [0, …], 127)`; monotone over `s`, so the
    // inequalities below survive the quantisation.
    let attenuate = |s: i32| (s + (((0 - s) * 127) >> 8)).clamp(0, 255) as u8;

    let mut displaced = false;
    for y in 0..height as i32 {
        for x in 0..width as i32 {
            let i = ((y as u32 * width + x as u32) * 4) as usize;
            let red = dst.rgba[i];
            let green = dst.rgba[i + 1];
            let want_red = attenuate(x);
            let want_green = attenuate(y);
            if red != want_red || green != want_green {
                displaced = true;
            }
            if x >= 40 {
                assert!(
                    red <= want_red,
                    "right of centre samples at x-dx: ({x},{y}) red {red} > {want_red}"
                );
            } else {
                assert!(
                    red >= want_red,
                    "left of centre samples at x+dx: ({x},{y}) red {red} < {want_red}"
                );
            }
            if y >= 40 {
                assert!(
                    green <= want_green,
                    "below centre samples at y-dy: ({x},{y}) green {green} > {want_green}"
                );
            } else {
                assert!(
                    green >= want_green,
                    "above centre samples at y+dy: ({x},{y}) green {green} < {want_green}"
                );
            }
            assert_eq!(dst.rgba[i + 3], 255, "the frame stays opaque");
        }
    }
    assert!(
        displaced,
        "the requested centre must produce a non-trivial displacement"
    );
}
