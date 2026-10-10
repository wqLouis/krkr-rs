//! Integration tests for the `rotatezoom`, `rotatevanish` and `rotateswap`
//! transitions (`engine::trans`), the port of `extrans.dll`'s rotation
//! family (`sample/extrans/rotatetrans.cpp`).
//!
//! The handlers are private to `trans`, so the tests recover their geometry
//! from rendered frames: both sources encode their own `(x, y)` and a
//! per-source blue byte in every pixel, which turns the destination back
//! into "which source covers this pixel" and "which pixel of it was
//! sampled" — enough to watch a quadrilateral rotate, shrink or grow.
//!
//! Every transition is parked at `progress = 0.5`: the two endpoints belong
//! to the caller, so only a middle frame ever reaches the providers.

use engine::BitmapState;
use engine::trans::{Trans, TransKind};

/// Edge length of the test images.
const SIZE: u32 = 32;

/// `bgcolor` `0xFF000000` — opaque black, distinguishable from every pixel
/// the test sources produce (their blue byte is 40 or 200).
const BG: i64 = 0xFF00_0000;
/// [`BG`] as straight-alpha RGBA.
const BG_RGBA: [u8; 4] = [0, 0, 0, 255];
/// Blue byte carried by every pixel of source 1.
const BLUE1: u8 = 40;
/// Blue byte carried by every pixel of source 2.
const BLUE2: u8 = 200;

/// A source whose pixel `(x, y)` stores `[x, y, blue, 255]`, so a rendered
/// frame reveals both *which* source covers a pixel and *which* pixel of it
/// was sampled.
fn source(blue: u8) -> BitmapState {
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            rgba.extend_from_slice(&[x as u8, y as u8, blue, 255]);
        }
    }
    BitmapState::new(SIZE, SIZE, rgba)
}

/// An untouched destination (fully transparent black, never a legal
/// rendered pixel of these sources).
fn blank() -> BitmapState {
    BitmapState::new(SIZE, SIZE, vec![0; (SIZE * SIZE * 4) as usize])
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

/// `rotatezoom` with explicit options.
fn zoom(
    factor: f64,
    accel: f64,
    twist: f64,
    twist_accel: f64,
    center: Option<(i32, i32)>,
) -> TransKind {
    TransKind::RotateZoom {
        factor,
        accel,
        twist,
        twist_accel,
        center,
    }
}

/// A transition parked in the middle of its run.
fn mid(kind: TransKind) -> Trans {
    let mut trans = Trans::new(kind, 1000.0).unwrap();
    let step = trans.step(500.0);
    assert!(
        step.phase > 0 && step.phase < trans.phase_max(),
        "mid-phase"
    );
    trans
}

/// Render one mid-phase frame over the full image.
fn render(kind: TransKind, src1: &BitmapState, src2: &BitmapState) -> BitmapState {
    let mut dst = blank();
    mid(kind)
        .apply(&mut dst, src1, src2, (0, 0, SIZE as i32, SIZE as i32))
        .unwrap();
    dst
}

/// Horizontal span (`max - min`) of the pixels sampled from source 2 — the
/// width of the quadrilateral currently showing it.
fn extent(bitmap: &BitmapState) -> i32 {
    let (mut lo, mut hi) = (i32::MAX, i32::MIN);
    for (i, px) in bitmap.rgba.chunks_exact(4).enumerate() {
        if px[2] == BLUE2 {
            let x = (i % SIZE as usize) as i32;
            lo = lo.min(x);
            hi = hi.max(x);
        }
    }
    hi - lo
}

/// Whether any pixel was sampled from the source marked `blue`.
fn uses(bitmap: &BitmapState, blue: u8) -> bool {
    bitmap.rgba.chunks_exact(4).any(|px| px[2] == blue)
}

#[test]
fn rotate_zoom_mid_phase_transforms_the_frame() {
    let src1 = source(BLUE1);
    let src2 = source(BLUE2);
    let dst = render(zoom(0.5, 0.0, 2.0, -2.0, None), &src1, &src2);

    // Neither endpoint: the frame is a composition, not a copy.
    assert_ne!(dst.rgba, src1.rgba);
    assert_ne!(dst.rgba, src2.rgba);

    // The moving quadrilateral has shrunk to 3/4 of the frame and twisted
    // half a turn, so the corners still show the stationary source 1 …
    assert_eq!(at(&dst, 1, 1), [1, 1, BLUE1, 255]);
    assert_eq!(at(&dst, 30, 30), [30, 30, BLUE1, 255]);
    // … while the middle shows source 2, rotated about the centre — its
    // own centre still lands on the image centre.
    assert_eq!(at(&dst, 16, 16)[2], BLUE2);
    assert!(uses(&dst, BLUE1) && uses(&dst, BLUE2));
}

#[test]
fn rotate_vanish_mid_phase_transforms_the_frame() {
    let src1 = source(BLUE1);
    let src2 = source(BLUE2);
    let kind = TransKind::RotateVanish {
        accel: 2.0,
        twist: 2.0,
        twist_accel: 2.0,
        center: None,
    };
    let dst = render(kind, &src1, &src2);

    assert_ne!(dst.rgba, src1.rgba);
    assert_ne!(dst.rgba, src2.rgba);

    // Source 2 fills the frame (it is the stationary quadrilateral here)
    // and the shrinking, twisting source 1 sits on top of it.
    assert_eq!(at(&dst, 1, 1), [1, 1, BLUE2, 255]);
    assert_eq!(at(&dst, 30, 30), [30, 30, BLUE2, 255]);
    assert_eq!(at(&dst, 16, 16)[2], BLUE1);
    assert!(uses(&dst, BLUE1) && uses(&dst, BLUE2));
    // Source 1 has shrunk away from the frame edges: neither of the two
    // outermost rows/columns still holds a source-1 pixel.
    for i in 0..SIZE as i32 {
        assert_ne!(at(&dst, i, 1)[2], BLUE1, "source 1 at ({i}, 1)");
        assert_ne!(at(&dst, 1, i)[2], BLUE1, "source 1 at (1, {i})");
    }
}

#[test]
fn rotate_swap_mid_phase_transforms_and_reveals_the_background() {
    let src1 = source(BLUE1);
    let src2 = source(BLUE2);
    let kind = TransKind::RotateSwap {
        background: BG,
        twist: 1.0,
        center: None,
    };
    let dst = render(kind, &src1, &src2);

    assert_ne!(dst.rgba, src1.rgba);
    assert_ne!(dst.rgba, src2.rgba);

    // At the midpoint source 1 has twisted into a thin bar, source 2 has
    // grown out of the opposite corner, and `bgcolor` shows everywhere
    // else.
    assert_eq!(at(&dst, 1, 1), BG_RGBA);
    assert_eq!(at(&dst, 30, 30), BG_RGBA);
    assert!(dst.rgba.chunks_exact(4).any(|px| *px == BG_RGBA));
    assert!(uses(&dst, BLUE1), "source 1 is still on screen");
    assert!(uses(&dst, BLUE2), "source 2 has grown into the frame");
}

#[test]
fn centre_option_moves_the_rotation_centre() {
    let src1 = source(BLUE1);
    let src2 = source(BLUE2);
    // A plain linear zoom (no twist) isolates the pivot.
    let default = render(zoom(0.5, 2.0, 0.0, 0.0, None), &src1, &src2);
    let corner = render(zoom(0.5, 2.0, 0.0, 0.0, Some((0, 0))), &src1, &src2);

    assert_ne!(default.rgba, corner.rgba);
    // Both frames sample source 2 under the image centre …
    assert_eq!(at(&default, 16, 16)[2], BLUE2);
    assert_eq!(at(&corner, 16, 16)[2], BLUE2);

    // … but the default pivot maps the image centre back to the *source*
    // centre (16,16) …
    let d = at(&default, 16, 16);
    assert!((14..=18).contains(&d[0]) && (14..=18).contains(&d[1]));
    // … while pivoting on (0,0) maps it back to the source corner (4,4).
    let c = at(&corner, 16, 16);
    assert!((2..=6).contains(&c[0]) && (2..=6).contains(&c[1]));
}

#[test]
fn rotate_swap_centre_option_changes_the_pivot() {
    let src1 = source(BLUE1);
    let src2 = source(BLUE2);
    let swap = |center| TransKind::RotateSwap {
        background: BG,
        twist: 1.0,
        center,
    };
    let default = render(swap(None), &src1, &src2);
    let corner = render(swap(Some((0, 0))), &src1, &src2);
    assert_ne!(default.rgba, corner.rgba);
    // The pivot decides where each bar starts from, but both sources and
    // the background are still on screen.
    assert!(uses(&corner, BLUE1) && uses(&corner, BLUE2));
}

#[test]
fn factor_and_accel_change_the_zoom() {
    let src1 = source(BLUE1);
    let src2 = source(BLUE2);

    // `factor` 1 with no twist places source 2 exactly: a plain copy.
    let full = render(zoom(1.0, 0.0, 0.0, -2.0, None), &src1, &src2);
    assert_eq!(full.rgba, src2.rgba);

    // `factor` 0.5 starts the frame zoomed out to 3/4 at the midpoint,
    // with source 1 showing around it.
    let shrunk = render(zoom(0.5, 0.0, 0.0, 0.0, None), &src1, &src2);
    assert_ne!(shrunk.rgba, full.rgba);
    assert_eq!(at(&shrunk, 1, 1), [1, 1, BLUE1, 255]);

    // `accel` eases the same zoom: at the midpoint it has covered only
    // (0.5)² of the way, so the window is smaller still.
    let eased = render(zoom(0.5, 2.0, 0.0, 0.0, None), &src1, &src2);
    assert_ne!(eased.rgba, shrunk.rgba);
    assert_ne!(eased.rgba, full.rgba);
    assert!(extent(&full) > extent(&shrunk));
    assert!(extent(&shrunk) > extent(&eased));
}

#[test]
fn twist_changes_the_shear() {
    let src1 = source(BLUE1);
    let src2 = source(BLUE2);

    // No twist at all: the moving quadrilateral is the identity and the
    // frame is source 2 verbatim.
    let untwisted = render(zoom(1.0, 0.0, 0.0, -2.0, None), &src1, &src2);
    assert_eq!(untwisted.rgba, src2.rgba);

    // One turn and two turns shear differently (the eased twist reaches
    // 0.75 of a turn at the midpoint).
    let quarter = render(zoom(1.0, 0.0, 1.0, -2.0, None), &src1, &src2);
    let half = render(zoom(1.0, 0.0, 2.0, -2.0, None), &src1, &src2);
    assert_ne!(quarter.rgba, untwisted.rgba);
    assert_ne!(half.rgba, untwisted.rgba);
    assert_ne!(half.rgba, quarter.rgba);

    // Half a twist turns the frame upside down: the top-left destination
    // pixel now samples the bottom-right of source 2 instead of itself.
    assert_eq!(at(&half, 1, 1)[2], BLUE2);
    assert_ne!(at(&half, 1, 1), [1, 1, BLUE2, 255]);
    assert!(at(&half, 1, 1)[0] >= 29);
}
