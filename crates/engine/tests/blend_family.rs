//! Integration tests for the operation-mode blend family.
//!
//! The reference has two blend shapes: the alpha blend that takes the
//! destination's alpha into the mix
//! (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3549-3556`) and
//! the channel mix that does not
//! (`:3402-3411`, `:3474-3482`). The Photoshop modes are the channel mix
//! around a per-channel function
//! (`reference/cpp/core/visual/gl/blend_functor_c.h:374-383`), and the
//! integer per-channel functions are the tree's CPU twins
//! (`reference/cpp/core/visual/gl/blend_functor_c.h:386-660`,
//! `reference/cpp/core/visual/gl/blend_function.cpp:526-547`).
//!
//! The modes below the Photoshop family are per-channel functions of their
//! own (`reference/cpp/core/visual/gl/blend_functor_c.h:182-349`); the
//! software renderer's method table names one per mode
//! (`reference/cpp/core/visual/RenderManager.cpp:2403-2415`), and at the full
//! opacity the method runs the plain functor
//! (`reference/cpp/core/visual/RenderManager.cpp:1802-1824`).

use api::blend::BlendMode;
use engine::layer_ops::{BlitOptions, BlitSource, operate_rect, render_layer_subtree};
use engine::scene::Rect;
use engine::{BitmapState, Scene};

/// A `width` x `height` bitmap filled with one straight-alpha colour.
fn solid_bitmap(scene: &mut Scene, width: u32, height: u32, color: [u8; 4]) -> u32 {
    let mut rgba = Vec::new();
    for _ in 0..width * height {
        rgba.extend_from_slice(&color);
    }
    scene.add_bitmap(width, height, rgba)
}

/// The colour the destination holds after `operate_rect` blended `source`
/// into it with `mode`.
fn blend(mode: i64, dest: [u8; 4], source: [u8; 4], opa: u8) -> [u8; 4] {
    blend_with(mode, dest, source, opa, false)
}

/// [`blend`] with the source read as an opaque image.
fn blend_with(
    mode: i64,
    dest: [u8; 4],
    source: [u8; 4],
    opa: u8,
    treat_as_opaque: bool,
) -> [u8; 4] {
    let mut dst = BitmapState::new(1, 1, dest.to_vec());
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
            opa,
            treat_as_opaque,
            hold_alpha: false,
        },
    );
    [dst.rgba[0], dst.rgba[1], dst.rgba[2], dst.rgba[3]]
}

/// Reference: the Photoshop normal blend is the tree's "PsAlphaBlend": the
/// source's alpha mixes the colours, the destination's alpha is held and the
/// mix carries no alpha correction
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3474-3482`; the CPU
/// twin is `d + ((s - d) * a >> 8)`,
/// `reference/cpp/core/visual/gl/blend_functor_c.h:374-383`).
#[test]
fn the_photoshop_normal_blend_mixes_without_an_alpha_correction() {
    // The destination's red and the source's green mix by the source's alpha
    // of 128: `255 + ((0 - 255) * 128 >> 8) = 127` and
    // `0 + ((255 - 0) * 128 >> 8) = 127`.
    assert_eq!(
        blend(13, [255, 0, 0, 128], [0, 255, 0, 128], 255),
        [127, 127, 0, 128]
    );
}

/// Reference: with an opaque destination the channel mix replaces the
/// destination's colours (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3474-3482`;
/// the CPU twin truncates the product by eight bits, so the green channel
/// lands on `255 * 255 >> 8 = 254`).
#[test]
fn the_channel_mix_replaces_the_colours_of_an_opaque_destination() {
    assert_eq!(
        blend(13, [255, 0, 0, 255], [0, 255, 0, 255], 255),
        [0, 254, 0, 255]
    );
}

/// Reference: the alpha-correct source-over takes the destination's alpha
/// into the mix, so the same operands give a different colour and a
/// recomputed alpha (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3549-3556`).
#[test]
fn the_source_over_computes_the_destination_alpha() {
    // `d.a = 128 + 128 - 128 * 128 / 255 = 191` and the green channel
    // becomes `255 * 128 / 191 = 170`.
    assert_eq!(
        blend(2, [255, 0, 0, 128], [0, 255, 0, 128], 255),
        [85, 170, 0, 191]
    );
}

/// Reference: the alpha blend mixes with the *new* alpha, so a translucent
/// source over an identically translucent destination keeps the colour both
/// operands share (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3549-3556`).
#[test]
fn the_alpha_blend_keeps_the_colour_of_a_translucent_destination() {
    // `d.a = 26 + 26 - 26 * 26 / 255 = 49` and the mix factor is
    // `26 / 49.35`, so every channel keeps the value both operands share.
    assert_eq!(
        blend(2, [255, 204, 204, 26], [255, 204, 204, 26], 255),
        [255, 204, 204, 49]
    );
}

/// Reference: every Photoshop mode has its own per-channel function, and the
/// tree's CPU twins give the integer arithmetic
/// (`reference/cpp/core/visual/gl/blend_functor_c.h:386-660`,
/// `reference/cpp/core/visual/gl/blend_function.cpp:526-547`). The
/// destination is `(200, 100, 50, 128)` and the source `(60, 180, 90, 192)`,
/// so the mix factor is the source's alpha of 192.
#[test]
fn the_photoshop_modes_have_their_own_per_channel_functions() {
    let dest = [200, 100, 50, 128];
    let source = [60, 180, 90, 192];
    // The mode numbers are the operation modes
    // (`reference/cpp/core/visual/LayerIntf.h:76-105`).
    let cases: [(i64, [u8; 3]); 14] = [
        (13, [95, 160, 80]),   // omPsNormal
        (14, [241, 216, 117]), // omPsAdditive: the saturating sum
        (15, [155, 25, 12]),   // omPsSubtractive: the saturating difference
        (16, [84, 77, 25]),    // omPsMultiplicative: the product
        (17, [210, 182, 104]), // omPsScreen
        (18, [178, 130, 38]),  // omPsOverlay
        (19, [120, 148, 38]),  // omPsHardLight
        (20, [181, 123, 35]),  // omPsSoftLight
        (21, [241, 216, 70]),  // omPsColorDodge
        (23, [66, 52, 12]),    // omPsColorBurn
        (24, [200, 160, 80]),  // omPsLighten
        (25, [95, 100, 50]),   // omPsDarken
        (26, [155, 85, 42]),   // omPsDifference
        (28, [175, 130, 91]),  // omPsExclusion
    ];
    for (mode, expected) in cases {
        assert_eq!(
            blend(mode, dest, source, 255),
            [expected[0], expected[1], expected[2], 128],
            "mode {mode} must apply its own per-channel function"
        );
    }
}

/// Reference: the legacy Photoshop 5.x modes fade the source first and
/// replace the channel; they do not mix
/// (`reference/cpp/core/visual/gl/blend_functor_c.h:568-581`, `:630-642`).
#[test]
fn the_legacy_photoshop_modes_replace_the_channel() {
    let dest = [200, 100, 50, 128];
    let source = [60, 180, 90, 192];
    // The source fades by the mix factor of 192: `(60 * 192) >> 8 = 45`,
    // `(180 * 192) >> 8 = 135` and `(90 * 192) >> 8 = 67`.
    assert_eq!(
        blend(22, dest, source, 255),
        [242, 212, 67, 128],
        "the colour dodge of the legacy mode replaces the channel"
    );
    assert_eq!(
        blend(27, dest, source, 255),
        [155, 35, 17, 128],
        "the difference of the legacy mode replaces the channel"
    );
}

/// Reference: the overlay is the tree's overlay table
/// (`reference/cpp/core/visual/gl/blend_function.cpp:539-542`): below the
/// midpoint `(s * d * 2) / 255` and above it
/// `(s + d) * 2 - (s * d * 2) / 255 - 255`.
#[test]
fn the_overlay_follows_the_reference_table() {
    // Every destination channel of 100 is below the midpoint, so the overlay
    // is `(s * d * 2) / 255`: `(50 * 100 * 2) / 255 = 39`,
    // `(200 * 100 * 2) / 255 = 156` and `(150 * 100 * 2) / 255 = 117`.
    // The mix truncates by eight bits, so the channels land on 39, 155 and
    // 116.
    assert_eq!(
        blend(18, [100, 100, 100, 255], [50, 200, 150, 255], 255),
        [39, 155, 116, 255]
    );
    // A destination channel above the midpoint takes the other branch:
    // `(10 + 240) * 2 - (240 * 10 * 2) / 255 - 255 = 18`.
    assert_eq!(
        blend(18, [10, 240, 130, 255], [240, 10, 130, 255], 255),
        [17, 227, 132, 255]
    );
}

/// Reference: the overlay of a colour over a white destination is the
/// destination: the table's upper branch is `(s + d) * 2 - (s * d * 2) / 255
/// - 255 = 255` for every channel
/// (`reference/cpp/core/visual/gl/blend_function.cpp:539-542`).
#[test]
fn the_overlay_over_a_white_destination_keeps_the_destination() {
    assert_eq!(
        blend(18, [255, 255, 255, 255], [109, 167, 159, 255], 255),
        [255, 255, 255, 255]
    );
}

/// Reference: the copy-on-alpha blend reads the source as an opaque image, so
/// the destination's alpha becomes opaque
/// (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3141-3147`; the CPU
/// twin writes `dest = src | 0xff000000`,
/// `reference/cpp/core/visual/tvpgl.cpp:3326-3370`).
#[test]
fn an_opaque_source_makes_the_destination_alpha_opaque() {
    assert_eq!(
        blend_with(1, [10, 20, 30, 128], [60, 70, 80, 90], 255, true),
        [60, 70, 80, 255]
    );
    // The plain copy keeps the source's alpha.
    assert_eq!(
        blend_with(1, [10, 20, 30, 128], [60, 70, 80, 90], 255, false),
        [60, 70, 80, 90]
    );
}

/// Reference: the tree composites a layer with the blend its type selects, and
/// the Photoshop normal type is the tree's "PsAlphaBlend"
/// (`reference/cpp/core/visual/RenderManager.cpp:2425-2426`), whose kernel
/// mixes the colours without an alpha correction and holds the destination's
/// alpha (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3474-3482`).
#[test]
fn a_photoshop_normal_layer_is_composited_without_an_alpha_correction() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let parent = scene.add_layer(win, None);
    let parent_bitmap = solid_bitmap(&mut scene, 2, 2, [255, 0, 0, 128]);
    {
        let layer = scene.layer_mut(parent).unwrap();
        layer.rect = Rect::new(0, 0, 2, 2);
        layer.visible = true;
        layer.bitmap = Some(parent_bitmap);
    }
    let child = scene.add_layer(win, Some(parent));
    let child_bitmap = solid_bitmap(&mut scene, 2, 2, [0, 255, 0, 128]);
    {
        let layer = scene.layer_mut(child).unwrap();
        layer.rect = Rect::new(0, 0, 2, 2);
        layer.visible = true;
        layer.bitmap = Some(child_bitmap);
        layer.blend = BlendMode::PsNormal;
    }
    let cache = render_layer_subtree(&mut scene, parent).expect("cache bitmap");
    let bitmap = scene.bitmap(cache).expect("the cache bitmap");
    // The source's alpha of 128 mixes the colours and the destination's alpha
    // is held: `255 + ((0 - 255) * 128 >> 8) = 127` and
    // `0 + ((255 - 0) * 128 >> 8) = 127`. The alpha-correct source-over
    // would have given `(85, 170, 0, 191)`.
    assert_eq!(&bitmap.rgba[..4], &[127, 127, 0, 128]);
}

// ---------------------------------------------------------------------------
// The operation modes below the Photoshop family
// ---------------------------------------------------------------------------

/// Reference: the additive blend is the saturating sum of the destination and
/// the source (`reference/cpp/core/visual/gl/blend_functor_c.h:182-188`), the
/// functor the software renderer's "AddBlend" method runs
/// (`reference/cpp/core/visual/RenderManager.cpp:2867`). At the full opacity
/// the method runs the plain functor, which adds the source unscaled
/// (`reference/cpp/core/visual/RenderManager.cpp:1802-1810`), and the
/// destination's alpha is held
/// (`reference/cpp/core/visual/gl/blend_variation.h:74-84`).
#[test]
fn the_additive_mode_saturates_the_sum_of_the_channels() {
    // `200 + 60` and `100 + 180` both run past 255 and saturate, `50 + 90` is
    // 140, and the destination's alpha stays at 128.
    assert_eq!(
        blend(3, [200, 100, 50, 128], [60, 180, 90, 192], 255),
        [255, 255, 140, 128]
    );
}

/// Reference: the subtractive blend is the saturating difference of the
/// destination and the source
/// (`reference/cpp/core/visual/gl/blend_functor_c.h:202-208`), the functor
/// the software renderer's "SubBlend" method runs
/// (`reference/cpp/core/visual/RenderManager.cpp:2868`). At the full opacity
/// the method runs the plain functor, which subtracts the source unscaled
/// (`reference/cpp/core/visual/RenderManager.cpp:1802-1810`).
#[test]
fn the_subtractive_mode_saturates_the_difference_of_the_channels() {
    // `200 - 60` is 140; `100 - 180` and `50 - 90` run below zero and
    // saturate.
    assert_eq!(
        blend(4, [200, 100, 50, 128], [60, 180, 90, 192], 255),
        [140, 0, 0, 128]
    );
}

/// Reference: the multiplicative blend is the product of the two channels
/// truncated by eight bits (`reference/cpp/core/visual/gl/blend_functor_c.h:222-229`),
/// the functor the software renderer's "MulBlend" method runs
/// (`reference/cpp/core/visual/RenderManager.cpp:2869-2870`). At the full
/// opacity the method runs the plain functor, which multiplies by the source
/// unscaled (`reference/cpp/core/visual/RenderManager.cpp:1802-1810`). The
/// blend reads the colours only; the source's alpha does not take part in it.
#[test]
fn the_multiplicative_mode_multiplies_the_channels() {
    // `200 * 64 >> 8` is `12800 >> 8` = 50, `100 * 128 >> 8` is 50 and
    // `50 * 192 >> 8` is `9600 >> 8` = 37.
    assert_eq!(
        blend(5, [200, 100, 50, 128], [64, 128, 192, 255], 255),
        [50, 50, 37, 128]
    );
}

/// Reference: the colour dodge scales the destination by the reciprocal of
/// the source's complement (`reference/cpp/core/visual/gl/blend_functor_c.h:243-255`),
/// the functor the software renderer's "ColorDodgeBlend" method runs
/// (`reference/cpp/core/visual/RenderManager.cpp:2871`). The reciprocal table
/// holds `65536 / i` and `65536` for the zero index
/// (`reference/cpp/core/visual/tvpgl.cpp:213-218`). At the full opacity the
/// method runs the plain functor, which dodges with the source unscaled
/// (`reference/cpp/core/visual/RenderManager.cpp:1802-1810`); below the full
/// opacity it runs the one that fades the source by the opacity first
/// (`reference/cpp/core/visual/gl/blend_functor_c.h:256-263`).
#[test]
fn the_dodge_mode_scales_the_destination_by_the_source_inverse() {
    // At the full opacity the source is not faded: `65536 / (255 - 32)` is
    // 293, so the red channel is `200 * 293 >> 8` = 228; the green channel
    // is `100 * (65536 / 231) >> 8` = `100 * 283 >> 8` = 110 and the blue
    // channel `50 * (65536 / 239) >> 8` = `50 * 274 >> 8` = 53.
    assert_eq!(
        blend(8, [200, 100, 50, 128], [32, 24, 16, 192], 255),
        [228, 110, 53, 128]
    );
    // Below the full opacity the source fades first: `(60 * 128) >> 8` is 30,
    // so the red channel is `200 * (65536 / 225) >> 8` = `200 * 291 >> 8` =
    // 227; the green channel fades to 90 and the blue one to 45.
    assert_eq!(
        blend(8, [200, 100, 50, 128], [60, 180, 90, 192], 128),
        [227, 155, 60, 128]
    );
}

/// Reference: the darken blend is the smaller of the two channels
/// (`reference/cpp/core/visual/gl/blend_functor_c.h:268-277`), the functor
/// the software renderer's "DarkenBlend" method runs
/// (`reference/cpp/core/visual/RenderManager.cpp:2872`). At the full opacity
/// the method runs the plain functor, which takes the smaller channel
/// (`reference/cpp/core/visual/RenderManager.cpp:1802-1810`).
#[test]
fn the_darken_mode_takes_the_smaller_channel() {
    // The red channel is the source's 60, the green channel the
    // destination's 100 and the blue channel the destination's 50.
    assert_eq!(
        blend(9, [200, 100, 50, 128], [60, 180, 90, 192], 255),
        [60, 100, 50, 128]
    );
}

/// Reference: the lighten blend is the larger of the two channels
/// (`reference/cpp/core/visual/gl/blend_functor_c.h:297-306`), the functor
/// the software renderer's "LightenBlend" method runs
/// (`reference/cpp/core/visual/RenderManager.cpp:2876`). At the full opacity
/// the method runs the plain functor, which takes the larger channel
/// (`reference/cpp/core/visual/RenderManager.cpp:1802-1810`).
#[test]
fn the_lighten_mode_takes_the_larger_channel() {
    // The red channel is the destination's 200, the green channel the
    // source's 180 and the blue channel the source's 90.
    assert_eq!(
        blend(10, [200, 100, 50, 128], [60, 180, 90, 192], 255),
        [200, 180, 90, 128]
    );
}

/// Reference: the screen blend is the complement of the product of the two
/// complements, truncated by eight bits
/// (`reference/cpp/core/visual/gl/blend_functor_c.h:326-336`), the functor the
/// software renderer's "ScreenBlend" method runs
/// (`reference/cpp/core/visual/RenderManager.cpp:2877`). At the full opacity
/// the method runs the plain functor
/// (`reference/cpp/core/visual/RenderManager.cpp:1802-1810`). The blend reads
/// the colours only; the source's alpha does not take part in it.
#[test]
fn the_screen_mode_is_the_complement_of_the_product() {
    // `255 - ((255 - 200) * (255 - 16) >> 8)` is `255 - (55 * 239 >> 8)` =
    // `255 - 51` = 204; the green channel is `255 - (155 * 223 >> 8)` = 120
    // and the blue one `255 - (205 * 215 >> 8)` = 83.
    assert_eq!(
        blend(11, [200, 100, 50, 128], [16, 32, 40, 192], 255),
        [204, 120, 83, 128]
    );
}

/// Reference: the additive-alpha blend scales the destination by the source's
/// remaining alpha and adds the source
/// (`reference/cpp/core/visual/gl/blend_util_func.h:35-43`), the functor the
/// software renderer's "AdditiveAlphaBlend" method runs
/// (`reference/cpp/core/visual/RenderManager.cpp:2878`). Below the full
/// opacity the method runs the one that fades the source by the opacity
/// first (`reference/cpp/core/visual/gl/blend_functor_c.h:132-146`), and the
/// destination's alpha is held in both
/// (`reference/cpp/core/visual/gl/blend_variation.h:74-84`).
#[test]
fn the_add_alpha_mode_scales_the_destination_by_the_remaining_alpha() {
    // Below the full opacity the source fades to `(60 * 128) >> 8` = 30 and
    // its alpha to `(192 * 128) >> 8` = 96, so the destination scales by
    // `255 - 96` = 159: the red channel is `200 * 159 >> 8` = 124 and the
    // source adds 30, which is 154; the green channel is `100 * 159 >> 8` =
    // 62 and the source adds 90, which is 152; the blue channel is
    // `50 * 159 >> 8` = 31 and the source adds 45, which is 76.
    assert_eq!(
        blend(12, [200, 100, 50, 128], [60, 180, 90, 192], 128),
        [154, 152, 76, 128]
    );
    // At the full opacity the source is not faded and the destination scales
    // by the source's remaining alpha `255 - 192` = 63: the red channel is
    // `128 * 63 >> 8` = 31 and the source adds 16, which is 47.
    assert_eq!(
        blend(12, [128, 64, 32, 128], [16, 24, 48, 192], 255),
        [47, 39, 55, 128]
    );
}
