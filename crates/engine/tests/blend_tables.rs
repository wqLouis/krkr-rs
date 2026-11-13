//! The tables the engine's per-pixel blend kernels read.
//!
//! The colour dodge's reciprocal, the constant colour's weight and the
//! stretch's mapping are tables the per-pixel kernels read instead of
//! dividing per pixel. The tests compare what the kernels produce with the
//! formulas the reference's functors use, over every value a kernel can
//! meet.

use engine::BitmapState;
use engine::layer_ops::{
    BlitOptions, BlitSource, COPY_MAIN, COPY_MASK, fill_color_on_alpha, operate_rect,
    stretch_blit_plane,
};

/// The colour the destination holds after `operate_rect` blended `source`
/// into it with `mode`.
fn blend(mode: i64, destination: [u8; 4], source: [u8; 4], opacity: u8) -> [u8; 4] {
    let mut dst = BitmapState::new(1, 1, destination.to_vec());
    let src = BitmapState::new(1, 1, source.to_vec());
    operate_rect(
        &mut dst,
        0,
        0,
        BlitSource {
            bitmap: &src,
            rect: (0, 0, 1, 1),
        },
        BlitOptions {
            mode,
            opa: opacity,
            treat_as_opaque: false,
            hold_alpha: false,
            source_over: false,
        },
    );
    [dst.rgba[0], dst.rgba[1], dst.rgba[2], dst.rgba[3]]
}

/// The weight the constant colour's blend mixes with.
///
/// Reference: the constant colour blend on an alpha destination reads the
/// weight from the opacity-on-opacity table
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:711-724`), and the
/// table holds `(sa / da) / (1 - sa + sa / da)`, truncated to a byte, with
/// the second step in single precision
/// (`reference/krkr2/cpp/core/visual/tvpgl.cpp:224-251`).
fn reference_weight(destination_alpha: u8, opacity: u8) -> u8 {
    if destination_alpha == 0 {
        return 255;
    }
    let at = destination_alpha as f32 / 255.0;
    let bt = opacity as f32 / 255.0;
    let mut share = bt / at;
    share /= 1.0 - bt + share;
    let value = (share * 255.0) as i32;
    if value >= 256 { 255 } else { value as u8 }
}

/// Reference: the colour dodge scales the destination by the reciprocal of
/// the source's complement
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:243-255`), and the
/// reciprocal table holds `65536 / i` and 65536 at the zero index
/// (`reference/krkr2/cpp/core/visual/tvpgl.cpp:212-220`). The kernel reads
/// the table instead of dividing, and every source channel meets the table.
#[test]
fn the_dodge_reads_the_reciprocal_of_the_source_complement() {
    // The reciprocal table the kernel reads.
    let mut reciprocals = [0u32; 256];
    reciprocals[0] = 65536;
    for index in 1..256u16 {
        reciprocals[usize::from(index)] = 65536 / u32::from(index);
    }
    for source in 0..=u8::MAX {
        let complement = u16::from(255 - source);
        let reciprocal = reciprocals[usize::from(complement)];
        for destination in [0u8, 1, 63, 128, 200, 254, 255] {
            let expected = ((u32::from(destination) * reciprocal) >> 8).min(255);
            let blended = blend(8, [destination, 0, 0, 255], [source, 0, 0, 255], 255);
            assert_eq!(
                blended[0], expected as u8,
                "the dodge of the source {source} over the destination {destination}"
            );
        }
    }
}

/// The constant colour's fill mixes with the table's weight.
///
/// Reference: the constant colour blend on an alpha destination reads the
/// weight from the opacity-on-opacity table
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:711-724`), and the
/// table holds the weight the fill mixes with
/// (`reference/krkr2/cpp/core/visual/tvpgl.cpp:224-251`). The fill reads the
/// table, and the colour it leaves is the destination's colour moved towards
/// the fill's colour by that weight.
///
/// The alpha the fill leaves is the negative-multiplication table's entry for
/// the opacity and the destination's alpha
/// (`reference/krkr2/cpp/core/visual/tvpgl.cpp:248-249`); the test holds the
/// colour and leaves the alpha alone.
#[test]
fn the_constant_colour_fill_reads_the_opacity_weight_of_the_table() {
    let color = [32u8, 64, 96, 200];
    let destination = [10u8, 20, 30];
    for destination_alpha in [0u8, 1, 51, 128, 153, 170, 200, 254, 255] {
        for opacity in [1u8, 100, 128, 153, 165, 180, 204, 254] {
            let mut dst = BitmapState::new(
                1,
                1,
                vec![
                    destination[0],
                    destination[1],
                    destination[2],
                    destination_alpha,
                ],
            );
            fill_color_on_alpha(&mut dst, (0, 0, 1, 1), color, i32::from(opacity));
            let weight = reference_weight(destination_alpha, opacity);
            for channel in 0..3 {
                let from = i32::from(destination[channel]);
                let to = i32::from(color[channel]);
                let expected = (from + (((to - from) * i32::from(weight)) >> 8)).clamp(0, 255);
                assert_eq!(
                    i32::from(dst.rgba[channel]),
                    expected,
                    "the fill of the colour {color:?} over the alpha {destination_alpha} \
                     with the opacity {opacity} left the channel {channel} at {:?}",
                    dst.rgba[channel]
                );
            }
        }
    }
}

/// Reference: the stretch maps the destination's pixels onto the source's
/// with the destination's pixel centre
/// (`reference/krkr2/cpp/core/visual/gl/ResampleImage.cpp:313-317`), and the
/// nearest resampler takes the source pixel the position names
/// (`reference/krkr2/cpp/core/visual/tvpgl.cpp:8577-8599`). The mapping is a
/// table of the destination's columns and rows, and the pass reads it.
#[test]
fn the_stretch_reads_the_position_of_its_column_and_row() {
    // A source whose pixels carry their own position.
    let mut rgba = Vec::new();
    for y in 0..4u32 {
        for x in 0..4u32 {
            rgba.extend_from_slice(&[(x * 16) as u8, (y * 16) as u8, 0, 255]);
        }
    }
    let src = BitmapState::new(4, 4, rgba);
    let mut dst = BitmapState::new(4, 4, vec![0; 4 * 4 * 4]);
    // The destination covers the source's half, offset by one pixel.
    let dest = (1, 1, 3, 3);
    stretch_blit_plane(&mut dst, dest, &src, (0, 0, 4, 4), 0, COPY_MAIN | COPY_MASK);
    let dw = (dest.2 - dest.0) as f32;
    let dh = (dest.3 - dest.1) as f32;
    for y in dest.1..dest.3 {
        for x in dest.0..dest.2 {
            let fx = ((x - dest.0) as f32 + 0.5) / dw * 4.0 - 0.5;
            let fy = ((y - dest.1) as f32 + 0.5) / dh * 4.0 - 0.5;
            let source_x = (fx + 0.5).floor() as i32;
            let source_y = (fy + 0.5).floor() as i32;
            let offset = ((y * 4 + x) * 4) as usize;
            assert_eq!(
                &dst.rgba[offset..offset + 4],
                &[(source_x * 16) as u8, (source_y * 16) as u8, 0, 255],
                "the destination pixel ({x}, {y}) took the source pixel \
                 ({source_x}, {source_y})"
            );
        }
    }
}
