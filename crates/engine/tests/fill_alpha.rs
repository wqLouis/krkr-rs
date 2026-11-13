//! Integration tests for the alpha the constant colour's fills leave.
//!
//! The reference's fills are the C++ functors the software renderer's method
//! table installs
//! (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2669-2735`,
//! `reference/krkr2/cpp/core/visual/gl/blend_function.cpp:636-641`): the
//! alpha face's fill is `const_alpha_fill_blend_d_functor`, the additive
//! face's is `const_alpha_fill_blend_a_functor` and the opaque face's is
//! `const_alpha_fill_blend_functor`
//! (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:737-802`).

use engine::BitmapState;
use engine::layer_ops::{
    fill_color_hold_alpha, fill_color_on_add_alpha, fill_color_on_alpha, remove_const_opacity,
};

/// The alpha the reference's fill leaves on an alpha destination.
///
/// Reference: the destination's alpha a blend on an alpha destination leaves
/// is the negative-multiplication table's entry for the source and the
/// destination alpha
/// (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:127-131`,
/// `reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:716`), and the
/// table holds `255 - (255 - dst) * (255 - src) / 255`, the division
/// truncating (`reference/krkr2/cpp/core/visual/tvpgl.cpp:248-249`). The
/// shipped renderer computes the same quantity in floating point for the
/// constant colour's fill, `d.a = opacity + d.a - opacity * d.a`
/// (`reference/krkr2/cpp/core/visual/ogl/RenderManager_ogl.cpp:3096-3098`).
fn reference_alpha(destination_alpha: u8, opacity: u8) -> u8 {
    let product = (255 - u32::from(destination_alpha)) * (255 - u32::from(opacity));
    (255 - product / 255) as u8
}

/// The alpha the reference's additive fill leaves.
///
/// Reference: the additive fill's kernel composes the destination's alpha
/// with the opacity, `Da = Sa + Da - SaDa`, and adjusts it afterwards
/// (`reference/krkr2/cpp/core/visual/gl/blend_util_func.h:125-137`).
fn reference_additive_alpha(destination_alpha: u8, opacity: u8) -> u8 {
    let mut alpha = u32::from(destination_alpha) + u32::from(opacity);
    alpha -= (u32::from(destination_alpha) * u32::from(opacity)) >> 8;
    alpha -= alpha >> 8;
    alpha as u8
}

/// The fill of the alpha face leaves the destination's alpha through the
/// negative-multiplication table, over the whole range of the destination's
/// alpha and the opacity.
#[test]
fn the_constant_colour_fill_leaves_the_negative_multiplication_alpha() {
    let color = [200u8, 100, 50, 255];
    for destination_alpha in 0..=u8::MAX {
        for opacity in 0..=u8::MAX {
            let mut dst = BitmapState::new(1, 1, vec![10, 20, 30, destination_alpha]);
            fill_color_on_alpha(&mut dst, (0, 0, 1, 1), color, i32::from(opacity));
            assert_eq!(
                dst.rgba[3],
                reference_alpha(destination_alpha, opacity),
                "the fill with the opacity {opacity} over the destination alpha \
                 {destination_alpha} left the alpha at {}",
                dst.rgba[3]
            );
        }
    }
}

/// The additive fill composes the destination's alpha with the opacity
/// instead of reading the negative-multiplication table.
///
/// The opacities below the full one reach the additive blend; the full
/// opacity takes the reference's plain ARGB path on every face
/// (`reference/krkr2/cpp/core/visual/LayerBitmapIntf.cpp:497-507`), where the
/// colour is written as it is and the alpha is 255.
#[test]
fn the_additive_colour_fill_composes_the_destination_alpha() {
    let color = [200u8, 100, 50, 255];
    for destination_alpha in 0..=u8::MAX {
        for opacity in 1..=u8::MAX - 1 {
            let mut dst = BitmapState::new(1, 1, vec![10, 20, 30, destination_alpha]);
            fill_color_on_add_alpha(&mut dst, (0, 0, 1, 1), color, i32::from(opacity));
            assert_eq!(
                dst.rgba[3],
                reference_additive_alpha(destination_alpha, opacity),
                "the additive fill with the opacity {opacity} over the destination \
                 alpha {destination_alpha} left the alpha at {}",
                dst.rgba[3]
            );
        }
    }
}

/// The full opacity is the plain ARGB fill on the additive face as well: the
/// colour is written as it is and the alpha is 255.
#[test]
fn the_additive_colour_fill_at_the_full_opacity_writes_the_colour() {
    let color = [200u8, 100, 50, 255];
    for destination_alpha in 0..=u8::MAX {
        let mut dst = BitmapState::new(1, 1, vec![10, 20, 30, destination_alpha]);
        fill_color_on_add_alpha(&mut dst, (0, 0, 1, 1), color, 255);
        assert_eq!(
            &dst.rgba[..4],
            &[color[0], color[1], color[2], 255],
            "the additive fill at the full opacity over the destination alpha \
             {destination_alpha} left the pixel at {:?}",
            &dst.rgba[..4]
        );
    }
}

/// The opacity removal multiplies the alpha by `255 - level` and keeps the
/// low byte of the product.
///
/// Reference: the kernel the removal runs multiplies the destination's alpha
/// by `255 - level` and keeps the low byte of the product
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:914-921`,
/// `reference/krkr2/cpp/core/visual/tvpgl.cpp:6401-6438`).
#[test]
fn the_opacity_removal_keeps_the_low_byte_of_the_product() {
    for destination_alpha in 0..=u8::MAX {
        for level in 1..=i32::from(u8::MAX) {
            let mut dst = BitmapState::new(1, 1, vec![10, 20, 30, destination_alpha]);
            remove_const_opacity(&mut dst, (0, 0, 1, 1), level);
            let expected = ((u32::from(destination_alpha) * (255 - level as u32)) & 0xff) as u8;
            assert_eq!(
                dst.rgba[3], expected,
                "the removal of the level {level} over the destination alpha \
                 {destination_alpha} left the alpha at {}",
                dst.rgba[3]
            );
        }
    }
}

/// The opaque face's fill holds the destination's alpha.
#[test]
fn the_opaque_colour_fill_holds_the_destination_alpha() {
    let color = [200u8, 100, 50, 255];
    for destination_alpha in 0..=u8::MAX {
        for opacity in 0..=u8::MAX {
            let mut dst = BitmapState::new(1, 1, vec![10, 20, 30, destination_alpha]);
            fill_color_hold_alpha(&mut dst, (0, 0, 1, 1), color, i32::from(opacity));
            assert_eq!(
                dst.rgba[3], destination_alpha,
                "the opaque fill with the opacity {opacity} over the destination \
                 alpha {destination_alpha} left the alpha at {}",
                dst.rgba[3]
            );
        }
    }
}
