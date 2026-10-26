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
use engine::trans::{DestAlpha, PHASE_MAX, RuleMask, Trans};

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

/// A cross-fade that stands at `phase` and mixes the way the
/// destination's alpha handling selects.
fn cross_fade_at(phase: i32, dest_alpha: DestAlpha) -> Trans {
    let duration = 1000.0;
    let mut trans = Trans::cross_fade(duration).expect("crossfade");
    trans.dest_alpha = dest_alpha;
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
/// reference applies to a whole row at a time, over all four channels
/// (`reference/cpp/core/visual/tvpgl.cpp:4446-4473`,
/// `reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3265-3268`).
#[test]
fn the_cross_fade_blends_every_phase_like_the_reference() {
    let (src1, src2) = sources();
    for phase in 1..PHASE_MAX {
        let trans = cross_fade_at(phase, DestAlpha::Additive);
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

/// The mix follows the destination's alpha handling.
///
/// The reference picks the blend by the destination's type
/// (`reference/cpp/core/visual/TransIntf.cpp:664-686`): a destination that
/// ignores its alpha and one that stores an additive alpha both cover the four
/// channels alike (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3265-3268`,
/// the plain and the additive cross-fade blends share that script), and a
/// destination that stores a straight alpha per pixel gets the blend that
/// covers the source with the share of the destination its two alphas give it
/// (`RenderManager_ogl.cpp:3278-3291`).
///
/// The CPU path writes the alpha-vs-alpha blend with the byte-exact opacity
/// table the reference's software path reads
/// (`reference/cpp/core/visual/tvpgl.cpp:224-251`,
/// `reference/cpp/core/visual/gl/blend_functor_c.h:689-707`).
///
/// The values are the ones the reference writes for an opaque red source 1 and
/// a source 2 that carries half of its alpha, at phase 127.
#[test]
fn the_cross_fade_mixes_by_the_destination_alpha_handling() {
    let src1 = BitmapState::new(1, 1, vec![255, 0, 0, 255]);
    let src2 = BitmapState::new(1, 1, vec![0, 0, 255, 128]);
    let area = (0, 0, 1, 1);

    let mut dst = BitmapState::new(1, 1, vec![0u8; 4]);
    cross_fade_at(127, DestAlpha::Opaque)
        .apply(&mut dst, &src1, &src2, area)
        .expect("crossfade");
    assert_eq!(dst.rgba, [128, 0, 126, 191], "the four-channel mix");

    let mut dst = BitmapState::new(1, 1, vec![0u8; 4]);
    cross_fade_at(127, DestAlpha::Additive)
        .apply(&mut dst, &src1, &src2, area)
        .expect("crossfade");
    assert_eq!(dst.rgba, [128, 0, 126, 191], "the additive mix");

    let mut dst = BitmapState::new(1, 1, vec![0u8; 4]);
    cross_fade_at(127, DestAlpha::Straight)
        .apply(&mut dst, &src1, &src2, area)
        .expect("crossfade");
    assert_eq!(dst.rgba, [155, 0, 99, 191], "the alpha-vs-alpha mix");
}

/// The rule blend takes the weight of every pixel from the rule image and
/// leaves the pixels the rule has not reached at source 1.
///
/// The weight comes from the rule (`reference/cpp/core/visual/tvpgl.cpp:4624-4636`)
/// and the table that maps the rule's grayscale value to it
/// (`reference/cpp/core/visual/tvpgl.cpp:4576-4595`). The mix is the one the
/// destination's alpha handling selects
/// (`reference/cpp/core/visual/TransIntf.cpp:737-761`); the additive one
/// covers the four channels alike, which is the arithmetic this test states.
#[test]
fn the_rule_blend_takes_the_weight_of_every_pixel_from_the_rule() {
    let (src1, src2) = sources();
    // A rule smaller than the source, so the tiling shows.
    let rule = RuleMask::new(4, 1, vec![0, 57, 90, 119]).expect("rule");
    let vague = 64;
    let mut trans = Trans::universal(1000.0, vague, rule.clone()).expect("universal");
    // The additive mix covers the four channels alike, which is the arithmetic
    // this test states.
    trans.dest_alpha = DestAlpha::Additive;
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

/// The rule blend's mix follows the destination's alpha handling too.
///
/// The reference's universal handler picks its blend by the destination's
/// type with the same test the cross-fade uses
/// (`reference/cpp/core/visual/TransIntf.cpp:737-761`): a destination that
/// ignores its alpha mixes the colour channels alone and keeps its own
/// alpha, one that stores an additive alpha covers the four channels alike,
/// and one that stores a straight alpha per pixel gets the mix that covers
/// the source with the share of the destination its two alphas give it
/// (`reference/cpp/core/visual/tvpgl.cpp:4612-4686`, `:5106-5138`,
/// `:4831-4905`).
///
/// The values are the ones the reference writes for an opaque red source 1
/// and a source 2 that carries half of its alpha, at the rule's weight 128.
#[test]
fn the_rule_blend_mixes_by_the_destination_alpha_handling() {
    let src1 = BitmapState::new(1, 1, vec![255, 0, 0, 255]);
    let src2 = BitmapState::new(1, 1, vec![0, 0, 255, 128]);
    // A rule of one pixel: its gray 150 maps to the weight 128 at phase 200
    // with a vague of 100.
    let rule = RuleMask::new(1, 1, vec![150]).expect("rule");
    let area = (0, 0, 1, 1);

    for (dest_alpha, expected) in [
        (DestAlpha::Opaque, [0x7f, 0x00, 0x7f, 0xff]),
        (DestAlpha::Additive, [0x7f, 0x00, 0x7f, 0xbf]),
        (DestAlpha::Straight, [0x99, 0x00, 0x65, 0xbf]),
    ] {
        let mut trans = Trans::universal(1000.0, 100, rule.clone()).expect("universal");
        trans.dest_alpha = dest_alpha;
        // 564 ms into a 1000 ms universal transition with a vague of 100
        // truncates to phase 200, where the rule's gray 150 weighs 128.
        trans.seek_ms(564.0);
        assert_eq!(trans.phase(), 200, "the transition stands at the phase");
        let mut dst = BitmapState::new(1, 1, vec![0u8; 4]);
        trans
            .apply(&mut dst, &src1, &src2, area)
            .expect("universal");
        assert_eq!(dst.rgba, expected, "the mix of {dest_alpha:?}");
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
    let trans = cross_fade_at(127, DestAlpha::Additive);
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
