//! Integration tests for the L2 transition state machine (`engine::trans`).
//!
//! The machine composites caller-owned bitmaps and never touches a render
//! backend, so every test drives it with `step(dt)` + `apply` and inspects the
//! destination pixels. The reference behaviour is the transition handlers in
//! `reference/cpp/core/visual/TransIntf.cpp`.

use engine::BitmapState;
use engine::layer_ops::RectI;
use engine::trans::{PHASE_MAX, RuleMask, ScrollFrom, ScrollStay, Trans, TransError, TransKind};

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

fn black(width: u32, height: u32) -> BitmapState {
    bitmap(width, height, [0, 0, 0, 255])
}

fn white(width: u32, height: u32) -> BitmapState {
    bitmap(width, height, [255, 255, 255, 255])
}

#[test]
fn cross_fade_hits_both_endpoints_exactly() {
    let src1 = black(2, 1);
    let src2 = white(2, 1);
    let mut dst = bitmap(2, 1, [0, 0, 0, 0]);

    let start = Trans::cross_fade(100.0).unwrap();
    start.apply(&mut dst, &src1, &src2, region(2, 1)).unwrap();
    assert_eq!(dst.rgba, src1.rgba);

    let mut end = Trans::cross_fade(100.0).unwrap();
    end.step(100.0);
    assert!(end.is_complete());
    end.apply(&mut dst, &src1, &src2, region(2, 1)).unwrap();
    assert_eq!(dst.rgba, src2.rgba);
}

#[test]
fn cross_fade_midpoint_blends_all_channels() {
    let src1 = bitmap(1, 1, [0, 0, 0, 255]);
    let src2 = bitmap(1, 1, [200, 100, 50, 255]);
    let mut dst = bitmap(1, 1, [0, 0, 0, 0]);

    let mut trans = Trans::cross_fade(100.0).unwrap();
    // 50/100 truncates to phase 127 (reference/cpp/core/visual/TransIntf.cpp:536).
    trans.step(50.0);
    trans.apply(&mut dst, &src1, &src2, region(1, 1)).unwrap();
    assert_eq!(dst.rgba, [99, 49, 24, 255]);
}

#[test]
fn step_progress_is_monotonic_and_clamped() {
    let mut trans = Trans::cross_fade(100.0).unwrap();
    let first = trans.step(25.0);
    assert!((first.progress - 0.25).abs() < 1e-6);
    assert_eq!(first.phase, 63);
    assert!(!first.completed);

    assert!((trans.step(25.0).progress - 0.5).abs() < 1e-6);
    let end = trans.step(1000.0);
    assert!(end.completed);
    assert_eq!(end.progress, 1.0);
    assert_eq!(end.phase, PHASE_MAX);
    assert_eq!(trans.step(1000.0), end, "completion is sticky");
    assert_eq!(trans.elapsed_ms(), 100.0);
}

/// `StartProcess` computes `(tick - StartTick) * PhaseMax / Time` with
/// integer truncation, so 50 ms of a 100 ms transition is phase 127 and the
/// completed 100 ms is phase 255.
///
/// reference/cpp/core/visual/TransIntf.cpp:528-538
#[test]
fn phase_uses_reference_truncating_division() {
    let mut trans = Trans::cross_fade(100.0).unwrap();
    trans.step(50.0);
    assert_eq!(trans.phase(), 127, "50/100 truncates to 127");
    assert!(!trans.is_complete());

    trans.step(50.0);
    assert_eq!(trans.phase(), PHASE_MAX, "100/100 is the completed phase");
    assert!(trans.is_complete());
}

#[test]
fn mask_uses_the_rule_image_as_a_hard_threshold() {
    // Left rule pixel switches early, right stays.
    let rule = RuleMask::new(2, 1, vec![0, 255]).unwrap();
    let src1 = black(2, 1);
    let src2 = white(2, 1);
    let mut dst = bitmap(2, 1, [0, 0, 0, 0]);

    let mut trans = Trans::mask(100.0, rule).unwrap();
    trans.step(50.0); // phase 127
    trans.apply(&mut dst, &src1, &src2, region(2, 1)).unwrap();
    // rule 0 < phase -> source 2 (the `>>8` blend gives 254, not 255).
    assert_eq!(&dst.rgba[0..4], &[254, 254, 254, 255]);
    assert_eq!(&dst.rgba[4..8], &[0, 0, 0, 255]);

    // At completion every pixel is source 2 verbatim.
    trans.complete();
    trans.apply(&mut dst, &src1, &src2, region(2, 1)).unwrap();
    assert_eq!(dst.rgba, src2.rgba);
}

#[test]
fn universal_soft_edge_blends_where_the_mask_snaps() {
    let rule = RuleMask::new(1, 1, vec![128]).unwrap();
    let src1 = black(1, 1);
    let src2 = white(1, 1);
    let mut dst = bitmap(1, 1, [0, 0, 0, 0]);

    let mut soft = Trans::universal(100.0, 64, rule.clone()).unwrap();
    soft.step(50.0);
    soft.apply(&mut dst, &src1, &src2, region(1, 1)).unwrap();
    let soft_value = dst.rgba[0];
    assert!(soft_value > 0 && soft_value < 255, "got {soft_value}");
    assert_eq!(soft.phase_max(), PHASE_MAX + 64);

    let mut hard = Trans::mask(100.0, rule).unwrap();
    hard.step(50.0);
    hard.apply(&mut dst, &src1, &src2, region(1, 1)).unwrap();
    assert!(dst.rgba[0] == 0 || dst.rgba[0] >= 254);
}

#[test]
fn scroll_pushes_source_two_in_from_the_left() {
    let src1 = black(2, 1);
    let src2 = white(2, 1);
    let mut dst = bitmap(2, 1, [0, 0, 0, 0]);

    let mut trans = Trans::scroll(100.0, ScrollFrom::Left, ScrollStay::NoStay).unwrap();
    trans.step(50.0); // half of the 2px travel
    trans.apply(&mut dst, &src1, &src2, region(2, 1)).unwrap();
    assert_eq!(&dst.rgba[0..4], &[255, 255, 255, 255]);
    assert_eq!(&dst.rgba[4..8], &[0, 0, 0, 255]);
}

#[test]
fn scroll_stay_src_keeps_source_one_fixed() {
    let src1 = black(2, 1);
    let src2 = white(2, 1);
    let mut dst = bitmap(2, 1, [0, 0, 0, 0]);
    let mut trans = Trans::scroll(100.0, ScrollFrom::Left, ScrollStay::StaySrc).unwrap();
    trans.step(50.0);
    trans.apply(&mut dst, &src1, &src2, region(2, 1)).unwrap();
    assert_eq!(&dst.rgba[0..4], &[255, 255, 255, 255]);
    assert_eq!(&dst.rgba[4..8], &[0, 0, 0, 255]);
}

#[test]
fn construction_errors_are_typed() {
    assert!(matches!(
        Trans::cross_fade(0.0),
        Err(TransError::InvalidDuration { .. })
    ));
    assert!(matches!(
        Trans::cross_fade(f64::NAN),
        Err(TransError::InvalidDuration { .. })
    ));
    assert!(matches!(
        Trans::new(TransKind::Mask, 10.0),
        Err(TransError::MissingRule)
    ));
    assert!(matches!(
        Trans::with_rule(
            TransKind::CrossFade,
            10.0,
            Some(RuleMask::new(1, 1, vec![0]).unwrap())
        ),
        Err(TransError::UnexpectedRule)
    ));
    assert!(matches!(
        Trans::universal(10.0, -1, RuleMask::new(1, 1, vec![0]).unwrap()),
        Err(TransError::InvalidVague { .. })
    ));
    assert!(matches!(
        RuleMask::new(2, 2, vec![0; 3]),
        Err(TransError::InconsistentRule { .. })
    ));
}

#[test]
fn size_mismatch_is_a_typed_error() {
    let src1 = black(2, 1);
    let src2 = black(3, 1);
    let mut dst = bitmap(3, 1, [0, 0, 0, 0]);
    let trans = Trans::cross_fade(10.0).unwrap();
    assert!(matches!(
        trans.apply(&mut dst, &src1, &src2, region(3, 1)),
        Err(TransError::SizeMismatch { .. })
    ));
}

#[test]
fn reset_and_complete_drive_the_lifecycle() {
    let mut trans = Trans::cross_fade(100.0).unwrap();
    trans.step(50.0);
    assert!(!trans.is_complete());
    trans.complete();
    assert!(trans.is_complete());
    assert_eq!(trans.phase(), PHASE_MAX);
    trans.reset();
    assert!(!trans.is_complete());
    assert_eq!(trans.phase(), 0);
    assert_eq!(trans.elapsed_ms(), 0.0);
}

#[test]
fn rule_mask_tiles_and_converts_rgba() {
    let rule = RuleMask::from_rgba(1, 1, &[255, 0, 0, 255]).unwrap();
    assert_eq!(rule.at(0, 0), 76);
    assert_eq!(rule.at(5, 9), 76, "tiles over the source");
    assert_eq!(rule.width(), 1);
    assert_eq!(rule.gray(), &[76]);
}

#[test]
fn two_transitions_are_independent() {
    let mut a = Trans::cross_fade(100.0).unwrap();
    let mut b = Trans::scroll(200.0, ScrollFrom::Top, ScrollStay::NoStay).unwrap();
    a.step(50.0);
    b.step(25.0);
    assert!((a.progress() - 0.5).abs() < 1e-6);
    assert!((b.progress() - 0.125).abs() < 1e-6);
    assert_ne!(a.kind(), b.kind());
}

/// The frame a composited `crossfade` draws: source 1 verbatim at the start,
/// the fade carrying `phase / 256`, and source 2 verbatim at the end — the
/// reference's two endpoint short-circuits
/// (`reference/cpp/core/visual/TransIntf.cpp:592-600`) and the `Phase`
/// weighting of `TVPConstAlphaBlend_SD` (`:660`).
#[test]
fn cross_fade_frame_is_two_full_source_draws() {
    let mut trans = Trans::cross_fade(100.0).unwrap();
    let frame = trans
        .transition_frame(7, 9, (4, 2), true)
        .expect("crossfade frame");
    assert_eq!(frame.size, (4, 2));
    assert!(frame.covers_layer);
    assert_eq!(frame.src1.bitmap, 7);
    assert_eq!(frame.src2.bitmap, 9);
    assert_eq!(frame.src1.src, engine::Rect::new(0, 0, 4, 2));
    assert_eq!(frame.src1.dst, engine::Rect::new(0, 0, 4, 2));
    assert_eq!(frame.src2.src, engine::Rect::new(0, 0, 4, 2));
    assert_eq!(frame.src2.dst, engine::Rect::new(0, 0, 4, 2));
    assert_eq!(frame.src1.alpha, 1.0);
    assert_eq!(frame.src2.alpha, 0.0, "phase 0 is source 1 verbatim");

    trans.step(50.0); // phase 127, reference/cpp/core/visual/TransIntf.cpp:536
    let frame = trans
        .transition_frame(7, 9, (4, 2), false)
        .expect("crossfade frame");
    assert!(!frame.covers_layer);
    assert_eq!(frame.src1.alpha, 1.0);
    assert!(
        (frame.src2.alpha - 127.0 / 256.0).abs() < 1e-6,
        "the fade source carries phase/256, got {}",
        frame.src2.alpha
    );

    trans.complete();
    let frame = trans
        .transition_frame(7, 9, (4, 2), true)
        .expect("crossfade frame");
    assert_eq!(frame.src1.alpha, 0.0, "phase max is source 2 verbatim");
    assert_eq!(frame.src2.alpha, 1.0);
}

/// A `scroll` frame places its two sources exactly where the reference's
/// `Blend` copies them (`reference/cpp/core/visual/TransIntf.cpp:1167-1172`)
/// and clips both to the frame (`:1208-1217`).
#[test]
fn scroll_frame_tiles_the_two_sources_inside_the_frame() {
    let src1 = black(4, 2);
    let src2 = white(4, 2);
    let mut dst = bitmap(4, 2, [0, 0, 0, 0]);
    let mut trans = Trans::scroll(100.0, ScrollFrom::Left, ScrollStay::NoStay).unwrap();
    trans.step(50.0); // phase 2: half of the 4px travel

    let frame = trans
        .transition_frame(1, 2, (4, 2), true)
        .expect("scroll frame");
    // Source 1 is pushed right by the phase and clipped to the frame; source 2
    // enters from the left edge.
    assert_eq!(frame.src1.dst, engine::Rect::new(2, 0, 2, 2));
    assert_eq!(frame.src1.src, engine::Rect::new(0, 0, 2, 2));
    assert_eq!(frame.src2.dst, engine::Rect::new(0, 0, 2, 2));
    assert_eq!(frame.src2.src, engine::Rect::new(2, 0, 2, 2));
    assert_eq!(frame.src1.alpha, 1.0);
    assert_eq!(frame.src2.alpha, 1.0);

    // The two placements reproduce the CPU blit exactly.
    trans.apply(&mut dst, &src1, &src2, region(4, 2)).unwrap();
    assert_eq!(&dst.rgba[0..4], &[255, 255, 255, 255]);
    assert_eq!(&dst.rgba[4..8], &[255, 255, 255, 255]);
    assert_eq!(&dst.rgba[8..12], &[0, 0, 0, 255]);
    assert_eq!(&dst.rgba[12..16], &[0, 0, 0, 255]);
}

/// The completed `scroll` frame is the second source covering the whole frame,
/// so the first source has no visible rectangle.
#[test]
fn completed_scroll_frame_is_the_second_source_alone() {
    let mut trans = Trans::scroll(100.0, ScrollFrom::Left, ScrollStay::StaySrc).unwrap();
    trans.step(100.0);
    let frame = trans
        .transition_frame(1, 2, (4, 2), true)
        .expect("scroll frame");
    assert_eq!(frame.src1.dst, engine::Rect::default());
    assert_eq!(frame.src2.dst, engine::Rect::new(0, 0, 4, 2));
    assert_eq!(frame.src2.src, engine::Rect::new(0, 0, 4, 2));
}

/// Only the composition kinds produce a frame; a rule or warp kind needs the
/// per-pixel blend and returns none.
#[test]
fn per_pixel_kinds_have_no_composited_frame() {
    let rule = RuleMask::new(1, 1, vec![0]).unwrap();
    let universal = Trans::universal(10.0, 64, rule.clone()).unwrap();
    assert!(universal.transition_frame(1, 2, (2, 2), true).is_none());
    let mask = Trans::mask(10.0, rule).unwrap();
    assert!(mask.transition_frame(1, 2, (2, 2), true).is_none());
    let ripple = Trans::new(
        TransKind::Ripple {
            center: None,
            map_width: 16,
            roundness: 1.0,
            speed: 6.0,
            max_drift: 1,
        },
        10.0,
    )
    .unwrap();
    assert!(ripple.transition_frame(1, 2, (2, 2), true).is_none());
    // A zero-sized frame has nothing to place.
    let cross_fade = Trans::cross_fade(10.0).unwrap();
    assert!(cross_fade.transition_frame(1, 2, (0, 2), true).is_none());
}
