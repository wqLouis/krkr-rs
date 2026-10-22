//! Integration tests for the transition blend the CPU path writes.
//!
//! The blend runs a row at a time, so the tests check that the row pass
//! writes the same bytes the per-pixel arithmetic of the reference produces:
//!
//! * the cross-fade blends source 1 into source 2 with the phase as the
//!   weight, over the channel extremes, the alpha extremes and a mid-tone
//!   (`reference/cpp/core/visual/tvpgl.cpp:4387-4399`);
//! * the rule-based blend takes the weight of every pixel from the rule
//!   image, tiled over the source
//!   (`reference/cpp/core/visual/tvpgl.cpp:4624-4636`), and the table that
//!   maps the rule's grayscale value to that weight
//!   (`reference/cpp/core/visual/tvpgl.cpp:4576-4595`);
//! * the endpoints stay exact copies of one source
//!   (`reference/cpp/core/visual/TransIntf.cpp:592-600`).
//!
//! The last test is a timing measurement of the 1920x1080 blend; it is
//! `#[ignore]`d because a measurement is a note, not an assertion.

use std::time::Instant;

use engine::BitmapState;
use engine::layer_ops::RectI;
use engine::trans::{PHASE_MAX, RuleMask, Trans};

/// The pixels the blend tests read: the channel extremes, the alpha
/// extremes and a mid-tone.
const PIXELS: [[u8; 4]; 8] = [
    [0, 0, 0, 0],
    [255, 255, 255, 255],
    [0, 255, 0, 255],
    [255, 0, 255, 0],
    [1, 254, 128, 64],
    [200, 100, 50, 255],
    [0, 0, 255, 128],
    [255, 255, 0, 1],
];

/// The per-channel blend the reference applies to a row
/// (`reference/cpp/core/visual/tvpgl.cpp:4387-4399`).
fn blend(s1: [u8; 4], s2: [u8; 4], opa: i32) -> [u8; 4] {
    let mut out = [0u8; 4];
    for c in 0..4 {
        let value = i32::from(s1[c]) + (((i32::from(s2[c]) - i32::from(s1[c])) * opa) >> 8);
        out[c] = value.clamp(0, 255) as u8;
    }
    out
}

/// The table the rule blend reads its weight from
/// (`reference/cpp/core/visual/tvpgl.cpp:4576-4595`).
fn rule_table(phase: i32, vague: i32) -> [u8; 256] {
    let mut table = [0u8; 256];
    let phase0 = phase - vague;
    for (i, slot) in table.iter_mut().enumerate() {
        let i = i as i32;
        let value = if i < phase0 {
            255
        } else if i >= phase || vague <= 0 {
            0
        } else {
            255 - ((i - phase0) * 255 / vague)
        };
        *slot = value.clamp(0, 255) as u8;
    }
    table
}

/// A bitmap whose pixels are the test pixels, one row per two pixels.
fn sources() -> (BitmapState, BitmapState) {
    let mut first = Vec::new();
    let mut second = Vec::new();
    for pixel in PIXELS {
        first.extend_from_slice(&pixel);
        second.extend_from_slice(&[pixel[3], pixel[2], pixel[1], pixel[0]]);
    }
    let width = PIXELS.len() as u32;
    let height = 2;
    let mut first_image = Vec::new();
    let mut second_image = Vec::new();
    for _ in 0..height {
        first_image.extend_from_slice(&first);
        second_image.extend_from_slice(&second);
    }
    (
        BitmapState::new(width, height, first_image),
        BitmapState::new(width, height, second_image),
    )
}

/// A cross-fade that stands at `phase`.
fn cross_fade_at(phase: i32) -> Trans {
    let duration = 1000.0;
    let mut trans = Trans::cross_fade(duration).expect("crossfade");
    // Half a phase into the phase's own interval, so the truncation of the
    // phase cannot land on its neighbour.
    let elapsed = f64::from(phase) + 0.5;
    trans.seek_ms(elapsed * duration / f64::from(PHASE_MAX));
    assert_eq!(trans.phase(), phase, "the transition stands at the phase");
    trans
}

/// The whole source as the region of the blend.
fn region(bitmap: &BitmapState) -> RectI {
    (0, 0, bitmap.width as i32, bitmap.height as i32)
}

/// One pixel of a bitmap.
fn pixel(bitmap: &BitmapState, index: usize) -> [u8; 4] {
    let offset = index * 4;
    [
        bitmap.rgba[offset],
        bitmap.rgba[offset + 1],
        bitmap.rgba[offset + 2],
        bitmap.rgba[offset + 3],
    ]
}

/// The cross-fade blends the two sources with the phase as the weight, over
/// every phase and every channel of the test pixels.
///
/// The blend is the per-channel `src1 + (src2 - src1) * opa / 256` the
/// reference applies to a whole row at a time
/// (`reference/cpp/core/visual/tvpgl.cpp:4387-4399`).
#[test]
fn the_cross_fade_blends_every_phase_like_the_reference() {
    let (src1, src2) = sources();
    for phase in 1..PHASE_MAX {
        let trans = cross_fade_at(phase);
        let mut dst = BitmapState::new(src1.width, src1.height, vec![0u8; src1.rgba.len()]);
        trans
            .apply(&mut dst, &src1, &src2, region(&src1))
            .expect("crossfade");
        for index in 0..(src1.width as usize * src1.height as usize) {
            let expected = blend(pixel(&src1, index), pixel(&src2, index), phase);
            assert_eq!(
                pixel(&dst, index),
                expected,
                "pixel {index} at phase {phase}"
            );
        }
    }
}

/// The endpoints stay exact copies of one source, and the blend between them
/// is the per-channel arithmetic above
/// (`reference/cpp/core/visual/TransIntf.cpp:592-600`).
#[test]
fn the_cross_fade_endpoints_are_copies_of_one_source() {
    let (src1, src2) = sources();
    let mut dst = BitmapState::new(src1.width, src1.height, vec![0u8; src1.rgba.len()]);

    let start = Trans::cross_fade(1000.0).expect("crossfade");
    start
        .apply(&mut dst, &src1, &src2, region(&src1))
        .expect("crossfade");
    assert_eq!(dst.rgba, src1.rgba, "the first phase is source 1");

    let mut end = Trans::cross_fade(1000.0).expect("crossfade");
    end.seek_ms(1000.0);
    end.apply(&mut dst, &src1, &src2, region(&src1))
        .expect("crossfade");
    assert_eq!(dst.rgba, src2.rgba, "the last phase is source 2");
}

/// The rule blend takes the weight of every pixel from the rule image and
/// leaves the pixels the rule has not reached at source 1.
///
/// The weight comes from the rule (`reference/cpp/core/visual/tvpgl.cpp:4624-4636`)
/// and the table that maps the rule's grayscale value to it
/// (`reference/cpp/core/visual/tvpgl.cpp:4576-4595`).
#[test]
fn the_rule_blend_takes_the_weight_of_every_pixel_from_the_rule() {
    let (src1, src2) = sources();
    // A rule smaller than the source, so the tiling shows.
    let rule = RuleMask::new(4, 1, vec![0, 57, 90, 119]).expect("rule");
    let vague = 64;
    let mut trans = Trans::universal(1000.0, vague, rule.clone()).expect("universal");
    let phase = 120;
    // A universal transition runs to `PHASE_MAX + vague`.
    trans.seek_ms((f64::from(phase) + 0.5) * 1000.0 / f64::from(PHASE_MAX + vague));
    assert_eq!(trans.phase(), phase, "the transition stands at the phase");
    let table = rule_table(phase, vague);

    let mut dst = BitmapState::new(src1.width, src1.height, vec![0u8; src1.rgba.len()]);
    trans
        .apply(&mut dst, &src1, &src2, region(&src1))
        .expect("universal");
    for y in 0..src1.height as i32 {
        for x in 0..src1.width as i32 {
            let opa = i32::from(table[usize::from(rule.at(x, y))]);
            let index = (y as usize) * src1.width as usize + x as usize;
            let expected = blend(pixel(&src1, index), pixel(&src2, index), opa);
            assert_eq!(
                pixel(&dst, index),
                expected,
                "pixel ({x},{y}) with weight {opa}"
            );
        }
    }
}

/// A timing measurement of the 1920x1080 cross-fade blend.
///
/// The measurement is a note, not an assertion: run it with
/// `cargo test -p engine --release --test trans_blend -- --ignored --nocapture`.
#[test]
#[ignore = "a timing measurement, not an assertion"]
fn the_cross_fade_blend_timing_note() {
    let width = 1920u32;
    let height = 1080u32;
    let bytes = (width * height * 4) as usize;
    let src1 = BitmapState::new(width, height, vec![7u8; bytes]);
    let src2 = BitmapState::new(width, height, vec![219u8; bytes]);
    let mut dst = BitmapState::new(width, height, vec![0u8; bytes]);
    let trans = cross_fade_at(127);
    let area = region(&src1);

    let frames = 10;
    let started = Instant::now();
    for _ in 0..frames {
        trans
            .apply(&mut dst, &src1, &src2, area)
            .expect("crossfade");
    }
    let elapsed = started.elapsed();
    println!(
        "crossfade {width}x{height}: {:.1} ms per frame",
        elapsed.as_secs_f64() * 1000.0 / frames as f64
    );
}
