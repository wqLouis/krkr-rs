//! Integration tests for the operation-mode blend family.
//!
//! The live per-pixel path of the reference is the C++ functor set that
//! `TVPGL_C_Init` installs
//! (`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:590-684`): the
//! kernels live in `gl/blend_functor_c.h`, the alpha-variation wrappers in
//! `gl/blend_variation.h` and the premultiplied helpers in
//! `gl/blend_util_func.h`. The legacy `_c` kernels in `tvpgl.cpp` are dead
//! in that tree — their registration blocks are `#if 0`
//! (`reference/krkr2/cpp/core/visual/tvpgl.cpp:13847-13882`) — so the
//! functors are the oracle.
//!
//! The software renderer's method table picks the kernel of a blt method
//! (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2396-2459`) and the
//! table the operation mode and the draw face select the method from
//! (`reference/krkr2/cpp/core/visual/LayerIntf.cpp:4118-4252`). The tests
//! drive the engine's own blend entry points, so every expected value is a
//! pixel the reference's kernel produces.

use api::blend::BlendMode;
use engine::layer_ops::{
    BlitOptions, BlitSource, operate_rect, operate_rect_on, render_layer_subtree, stretch_blit_mode,
};
use engine::scene::Rect;
use engine::trans::DestAlpha;
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
            source_over: false,
        },
    );
    [dst.rgba[0], dst.rgba[1], dst.rgba[2], dst.rgba[3]]
}

/// The colour the destination holds after the pass blended `source` into it
/// on a destination whose stored alpha follows `dst_alpha`.
fn blend_on(
    mode: i64,
    dst_alpha: DestAlpha,
    dest: [u8; 4],
    source: [u8; 4],
    opa: u8,
    hold_alpha: bool,
) -> [u8; 4] {
    let mut dst = BitmapState::new(1, 1, dest.to_vec());
    let src = BitmapState::new(1, 1, source.to_vec());
    operate_rect_on(
        dst_alpha,
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
            treat_as_opaque: false,
            hold_alpha,
            source_over: false,
        },
    );
    [dst.rgba[0], dst.rgba[1], dst.rgba[2], dst.rgba[3]]
}

/// The bytes of a row of pixels, row-major.
fn flatten(row: &[[u8; 4]]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(row.len() * 4);
    for pixel in row {
        bytes.extend_from_slice(pixel);
    }
    bytes
}

/// The pixels of the first row of `bitmap`.
fn read_row(bitmap: &BitmapState) -> Vec<[u8; 4]> {
    bitmap
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .map(|chunk| [chunk[0], chunk[1], chunk[2], chunk[3]])
        .collect()
}

/// Runs one `operateRect` pass of a whole row and returns the destination row.
fn operate_row(mode: i64, opa: u8, dest: &[[u8; 4]], source: &[[u8; 4]]) -> Vec<[u8; 4]> {
    let width = u32::try_from(dest.len()).expect("a row that fits a bitmap");
    let right = i32::try_from(width).expect("a row that fits a bitmap");
    let rect = (0, 0, right, 1);
    let mut dst = BitmapState::new(width, 1, flatten(dest));
    let src = BitmapState::new(width, 1, flatten(source));
    operate_rect(
        &mut dst,
        0,
        0,
        BlitSource { bitmap: &src, rect },
        BlitOptions {
            mode,
            opa,
            treat_as_opaque: false,
            hold_alpha: false,
            source_over: false,
        },
    );
    read_row(&dst)
}

/// Runs one `operateStretch` pass at the source's own size — so the resampler
/// is exact — and returns the destination row.
fn stretch_row(mode: i64, opa: u8, dest: &[[u8; 4]], source: &[[u8; 4]]) -> Vec<[u8; 4]> {
    let width = u32::try_from(dest.len()).expect("a row that fits a bitmap");
    let right = i32::try_from(width).expect("a row that fits a bitmap");
    let rect = (0, 0, right, 1);
    let mut dst = BitmapState::new(width, 1, flatten(dest));
    let src = BitmapState::new(width, 1, flatten(source));
    stretch_blit_mode(
        &mut dst,
        rect,
        BlitSource { bitmap: &src, rect },
        1, // stFastLinear
        BlitOptions {
            mode,
            opa,
            treat_as_opaque: false,
            hold_alpha: false,
            source_over: false,
        },
    );
    read_row(&dst)
}

/// Reference: the Photoshop normal blend is the "PsAlphaBlend" method
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2425-2426`), whose
/// kernel is `ps_alpha_blend_func`
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-383`): the
/// source's alpha mixes the colours, the destination's alpha is kept and the
/// mix carries no alpha correction.
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
/// destination's colours (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-383`);
/// the CPU kernel truncates the product by eight bits, so the green channel
/// lands on `255 * 255 >> 8 = 254`.
#[test]
fn the_channel_mix_replaces_the_colours_of_an_opaque_destination() {
    assert_eq!(
        blend(13, [255, 0, 0, 255], [0, 255, 0, 255], 255),
        [0, 254, 0, 255]
    );
}

/// Reference: the alpha blend on an alpha destination is the "AlphaBlend_d"
/// method (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2401-2402`),
/// whose kernel is `dest_alpha_op`
/// (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:109-149`): the
/// effective source weight comes from the opacity-on-opacity table
/// (`reference/krkr2/cpp/core/visual/tvpgl.cpp:224-301`) and the destination's
/// alpha is rewritten with the negative-multiply table.
///
/// The source's alpha of 128 and the destination's of 128 give the weight
/// `128 / 128` folded into `255 * 128 / (128 + 128) = 127`, and the
/// destination's alpha becomes `255 - (255 - 128) * (255 - 128) / 255 = 192`.
#[test]
fn the_alpha_blend_reads_the_destination_alpha() {
    // The green channel is `255 + ((0 - 255) * 127 >> 8) = 128` and the red
    // channel `0 + ((255 - 0) * 127 >> 8) = 126`; the mix truncates.
    assert_eq!(
        blend(2, [255, 0, 0, 128], [0, 255, 0, 128], 255),
        [85, 169, 0, 192]
    );
}

/// Reference: the alpha blend mixes with the *new* alpha, so a translucent
/// source over an identically translucent destination keeps the colour both
/// operands share (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:109-149`;
/// the destination's alpha becomes `255 - (255 - 26) * (255 - 26) / 255 = 50`).
#[test]
fn the_alpha_blend_keeps_the_colour_of_a_translucent_destination() {
    // The weight of the two alphas of 26 is `255 * 26 / 52 = 127`, and the
    // colour both operands share does not move.
    assert_eq!(
        blend(2, [255, 204, 204, 26], [255, 204, 204, 26], 255),
        [255, 204, 204, 50]
    );
}

/// Reference: the alpha blend on an opaque destination is the "AlphaBlend"
/// method (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2400-2401`),
/// whose kernel is `alpha_blend_func`
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:67-76`): the
/// destination's alpha is not read and the destination's alpha is kept.
#[test]
fn the_alpha_blend_on_an_opaque_destination_keeps_its_alpha() {
    assert_eq!(
        blend_on(
            2,
            DestAlpha::Opaque,
            [255, 0, 0, 255],
            [0, 255, 0, 255],
            255,
            false
        ),
        [0, 254, 0, 255]
    );
}

/// Reference: every Photoshop mode has its own per-channel function, and the
/// tree's CPU kernels give the integer arithmetic
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:386-660`,
/// `reference/krkr2/cpp/core/visual/gl/blend_function.cpp:526-547`). The
/// destination is `(200, 100, 50, 128)` and the source `(60, 180, 90, 192)`,
/// so the mix factor is the source's alpha of 192.
#[test]
fn the_photoshop_modes_have_their_own_per_channel_functions() {
    let dest = [200, 100, 50, 128];
    let source = [60, 180, 90, 192];
    // The mode numbers are the operation modes
    // (`reference/krkr2/cpp/core/visual/LayerIntf.h:76-105`).
    let cases: [(i64, [u8; 3]); 14] = [
        (13, [95, 160, 80]),   // omPsNormal
        (14, [241, 216, 117]), // omPsAdditive: the saturating sum
        (15, [53, 43, 12]),    // omPsSubtractive: the linear burn
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

/// Reference: the Photoshop subtractive blend is the linear burn: the source
/// is inverted and the difference saturates, so the channel is
/// `max(0, d + s - 255)`
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:401-412`).
#[test]
fn the_photoshop_subtractive_blend_is_a_linear_burn() {
    // The red channel is `max(0, 200 + 60 - 255) = 5`, mixed by the
    // source's alpha of 192: `200 + ((5 - 200) * 192 >> 8) = 53`.
    assert_eq!(
        blend(15, [200, 100, 50, 128], [60, 180, 90, 192], 255),
        [53, 43, 12, 128]
    );
}

/// Reference: the legacy Photoshop 5.x modes fade the source first and
/// replace the channel; they do not mix
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:568-581`, `:630-642`).
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
/// (`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:539-542`): below the
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
/// (`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:539-542`).
#[test]
fn the_overlay_over_a_white_destination_keeps_the_destination() {
    assert_eq!(
        blend(18, [255, 255, 255, 255], [109, 167, 159, 255], 255),
        [255, 255, 255, 255]
    );
}

/// Reference: the exclusion blend is `src + dst - 2 * src * dst / 255`
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:646-659`), whose
/// product is taken with a seven-bit shift.
#[test]
fn the_exclusion_blend_follows_the_reference_product() {
    // The red channel's product is `200 * 60 >> 7 = 93`, so the blend is
    // `200 + 60 - 93 = 167`, mixed by the source's alpha of 192:
    // `200 + ((167 - 200) * 192 >> 8) = 175`.
    assert_eq!(
        blend(28, [200, 100, 50, 128], [60, 180, 90, 192], 255),
        [175, 130, 91, 128]
    );
}

/// Reference: the copy of an opaque source is the "CopyOpaqueImage" method
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2404-2405`), whose
/// kernel forces the source's alpha to opaque
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:174-178`).
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

/// Reference: the copy below the full opacity is the "ConstAlphaBlend"
/// method (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2397-2399`),
/// whose kernel returns a 24-bit word
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:662-671`): the
/// destination's alpha byte becomes zero.
#[test]
fn the_constant_alpha_blend_drops_the_destination_alpha() {
    // The colour is mixed by the opacity: `10 + ((60 - 10) * 128 >> 8) = 35`,
    // `20 + ((70 - 20) * 128 >> 8) = 45` and `30 + ((80 - 30) * 128 >> 8) = 55`.
    assert_eq!(
        blend_with(1, [10, 20, 30, 128], [60, 70, 80, 90], 128, false),
        [35, 45, 55, 0]
    );
}

/// Reference: the copy of an opaque source below the full opacity is the
/// "ConstAlphaBlend_d" method
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2400-2401`), whose
/// kernel reads the destination's alpha
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:708-720`): the
/// source is treated as an opaque image at the opacity, and the
/// destination's alpha is rewritten with the negative-multiply table.
#[test]
fn the_constant_alpha_blend_of_an_opaque_source_reads_the_destination_alpha() {
    // The destination's alpha of 128 and the opacity of 128 give the weight
    // `255 * 128 / 255 = 128`, so the colour is `10 + ((60 - 10) * 128 >> 8)
    // = 35` and the destination's alpha becomes `255 - (255 - 128) * (255 -
    // 128) / 255 = 192`.
    assert_eq!(
        blend_with(1, [10, 20, 30, 128], [60, 70, 80, 90], 128, true),
        [43, 53, 63, 192]
    );
}

/// Reference: the copy with the hold-alpha flag is the "CopyColor" method
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2397-2399`), whose
/// kernel copies the colour and keeps the destination's alpha
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:162-166`).
#[test]
fn the_copy_with_the_hold_flag_keeps_the_destination_alpha() {
    assert_eq!(
        blend_on(
            1,
            DestAlpha::Straight,
            [10, 20, 30, 128],
            [60, 70, 80, 90],
            255,
            true
        ),
        [60, 70, 80, 128]
    );
}

/// Reference: the tree composites a layer with the blend its type selects, and
/// the Photoshop normal type is the tree's "PsAlphaBlend"
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2425-2426`), whose kernel
/// mixes the colours without an alpha correction and holds the destination's
/// alpha (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-383`).
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
    // `0 + ((255 - 0) * 128 >> 8) = 127`.
    assert_eq!(&bitmap.rgba[..4], &[127, 127, 0, 128]);
}

// ---------------------------------------------------------------------------
// The operation modes below the Photoshop family
// ---------------------------------------------------------------------------

/// Reference: the additive blend is the saturating sum of the destination and
/// the source (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:182-188`),
/// the functor the software renderer's "AddBlend" method runs
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2867`). At the full
/// opacity the method runs the plain functor, which adds the source unscaled
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:1802-1810`), and the
/// destination's alpha is held
/// (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:74-84`).
#[test]
fn the_additive_mode_saturates_the_sum_of_the_channels() {
    // `200 + 60` and `100 + 180` both run past 255 and saturate, `50 + 90` is
    // 140, and the destination's alpha stays at 128.
    assert_eq!(
        blend(3, [200, 100, 50, 128], [60, 180, 90, 192], 255),
        [255, 255, 140, 128]
    );
}

/// Reference: below the full opacity "AddBlend" runs `add_blend_func`, which
/// fades the source by the opacity with the eight-bit truncation
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:190-197`) before
/// `add_blend_functor`'s saturating sum (`:182-189`); the destination's alpha
/// is held by the HDA wrapper
/// (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:84-92`).
#[test]
fn the_additive_mode_fades_the_source_by_the_opacity() {
    // `faded = (s * opa) >> 8` then a saturating add, through
    // `operateStretch`:
    //   ( 60 * 128) >> 8 = 30  -> 200 +  30 = 230
    //   (180 * 128) >> 8 = 90  -> 100 +  90 = 190
    //   ( 90 * 128) >> 8 = 45  ->  50 +  45 =  95
    //   (250 * 128) >> 8 = 125 ->  10 + 125 = 135
    //   ( 20 * 128) >> 8 = 10  -> 240 +  10 = 250
    //   (200 * 128) >> 8 = 100 ->   5 + 100 = 105
    // and the destinations' alphas of 128 and 200 are held.
    assert_eq!(
        stretch_row(
            3,
            128,
            &[[200, 100, 50, 128], [10, 240, 5, 200]],
            &[[60, 180, 90, 192], [250, 20, 200, 64]],
        ),
        [[230, 190, 95, 128], [135, 250, 105, 200]]
    );
}

/// Reference: "SubBlend" is `tTVPRenderMethod_BltAndOpa<…, TVPSubBlend_HDA,
/// TVPSubBlend_HDA_o>` (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2831-2837`,
/// registered at `:2867`), so the full opacity runs `sub_blend_functor` and a
/// lower one `sub_blend_func`
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:1802-1819`). The faded
/// functor complements the source, fades it by `a >> 8` and complements back
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:209-217`), and the plain
/// one is the byte-wise `max(0, d + s - 255)` (`:202-208`). Both are therefore
/// `max(0, d - subtractend)`, with `subtractend` the whole complement
/// `255 - s` at the full opacity and its eight-bit truncation
/// `(255 - s) * opa >> 8` below it. The HDA wrapper holds the destination's
/// alpha (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:84-92`).
#[test]
fn the_subtractive_mode_subtracts_the_faded_complement() {
    // Full opacity: `max(0, d + s - 255)` is `max(0, d - (255 - s))`, so
    // `200 - 195` = 5, `100 - 75` = 25 and `50 - 165` saturates at 0.
    assert_eq!(
        blend(4, [200, 100, 50, 128], [60, 180, 90, 192], 255),
        [5, 25, 0, 128]
    );
    // Below it the subtractend is the truncated `(255 - s) * opa >> 8`:
    //   (255 - 60) * 128 >> 8  = 24960 >> 8 = 97   -> 200 - 97  = 103
    //   (255 - 180) * 128 >> 8 =  9600 >> 8 = 37   -> 100 - 37  =  63
    //   (255 - 90) * 128 >> 8  = 21120 >> 8 = 82   ->  50 - 82  ->   0
    //   (255 - 250) * 128 >> 8 =   640 >> 8 =  2   ->  10 - 2   =   8
    //   (255 - 20) * 128 >> 8  = 30080 >> 8 = 117  -> 240 - 117 = 123
    //   (255 - 200) * 128 >> 8 =  7040 >> 8 = 27   ->   5 - 27  ->   0
    // and the destinations' alphas of 128 and 200 are held.
    assert_eq!(
        operate_row(
            4,
            128,
            &[[200, 100, 50, 128], [10, 240, 5, 200]],
            &[[60, 180, 90, 192], [250, 20, 200, 64]],
        ),
        [[103, 63, 0, 128], [8, 123, 0, 200]]
    );
}

/// Reference: the multiplicative blend is the product of the two channels
/// truncated by eight bits (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:222-229`),
/// the functor the software renderer's "MulBlend" method runs
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2869-2870`). At the full
/// opacity the method runs the plain functor, which multiplies by the source
/// unscaled (`reference/krkr2/cpp/core/visual/RenderManager.cpp:1802-1810`). The
/// blend reads the colours only; the source's alpha does not take part in it.
#[test]
fn the_multiplicative_mode_multiplies_the_channels() {
    // `200 * 64 >> 8` is `12800 >> 8` = 50, `100 * 128 >> 8` is 50 and
    // `50 * 192 >> 8` is `9600 >> 8` = 37.
    assert_eq!(
        blend(5, [200, 100, 50, 128], [64, 128, 192, 255], 255),
        [50, 50, 37, 0]
    );
}

/// Reference: the multiplicative blend without the hold-alpha flag runs the
/// non-HDA "MulBlend" method
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2405`), whose kernel
/// returns a 24-bit word
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:222-229`): the
/// destination's alpha byte becomes zero.
#[test]
fn the_multiplicative_mode_drops_the_destination_alpha() {
    assert_eq!(
        blend(5, [200, 100, 50, 128], [64, 128, 192, 255], 255),
        [50, 50, 37, 0]
    );
    // The hold-alpha flag selects the "MulBlend_HDA" method
    // (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2405`), whose kernel
    // keeps the destination's alpha.
    assert_eq!(
        blend_on(
            5,
            DestAlpha::Straight,
            [200, 100, 50, 128],
            [64, 128, 192, 255],
            255,
            true
        ),
        [50, 50, 37, 128]
    );
}

/// Reference: below the full opacity `MulBlend` runs `mul_blend_func`,
/// which fades the source towards white — `s = ~s`, the eight-bit fade, `~`
/// again (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:230-240`) — and then
/// `mul_blend_functor`'s `d * s >> 8` (`:222-229`).
#[test]
fn the_multiplicative_mode_fades_the_source_towards_white() {
    // `faded = 255 - ((255 - s) * opa >> 8)`, then `d * faded >> 8`:
    //   faded(60)  = 255 - 97  = 158 -> 200 * 158 >> 8 = 31600 >> 8 = 123
    //   faded(180) = 255 - 37  = 218 -> 100 * 218 >> 8 = 21800 >> 8 =  85
    //   faded(90)  = 255 - 82  = 173 ->  50 * 173 >> 8 =  8650 >> 8 =  33
    //   faded(250) = 255 - 2   = 253 ->  10 * 253 >> 8 =  2530 >> 8 =   9
    //   faded(20)  = 255 - 117 = 138 -> 240 * 138 >> 8 = 33120 >> 8 = 129
    //   faded(200) = 255 - 27  = 228 ->   5 * 228 >> 8 =  1140 >> 8 =   4
    // and the destination's alpha is dropped, because the non-HDA method
    // returns a 24-bit word.
    assert_eq!(
        operate_row(
            5,
            128,
            &[[200, 100, 50, 128], [10, 240, 5, 200]],
            &[[60, 180, 90, 192], [250, 20, 200, 64]],
        ),
        [[123, 85, 33, 0], [9, 129, 4, 0]]
    );
}

/// Reference: the colour dodge scales the destination by the reciprocal of
/// the source's complement (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:243-255`),
/// the functor the software renderer's "ColorDodgeBlend" method runs
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2871`). The reciprocal table
/// holds `65536 / i` and `65536` for the zero index
/// (`reference/krkr2/cpp/core/visual/tvpgl.cpp:213-218`). At the full opacity the
/// method runs the plain functor, which dodges with the source unscaled
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:1802-1810`); below the full
/// opacity it runs the one that fades the source by the opacity first
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:256-263`).
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

/// Reference: at the full opacity "ColorDodgeBlend" runs
/// `color_dodge_blend_functor`, which dodges with the source itself — the
/// destination scaled by `TVPRecipTable256[255 - s]` and truncated by eight
/// bits, clamped to a byte (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:243-255`);
/// the table holds `65536 / i` and 65536 at index zero
/// (`reference/krkr2/cpp/core/visual/tvpgl.cpp:213-217`). The faded form is the
/// test above.
#[test]
fn the_dodge_mode_dodges_with_the_unfaded_source_at_full_opacity() {
    // `d * (65536 / (255 - s)) >> 8`, clamped to 255:
    //   65536 / (255 -  60) = 336 -> 100 * 336 >> 8 = 131
    //   65536 / (255 -  80) = 374 -> 100 * 374 >> 8 = 146
    //   65536 / (255 - 100) = 422 -> 100 * 422 >> 8 = 164
    // and the destination's alpha of 128 is held.
    assert_eq!(
        blend(8, [100, 100, 100, 128], [60, 80, 100, 192], 255),
        [131, 146, 164, 128]
    );
}

/// Reference: the darken blend is the smaller of the two channels
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:268-277`), the functor
/// the software renderer's "DarkenBlend" method runs
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2872`). At the full opacity the
/// method runs the plain functor, which takes the smaller channel
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:1802-1810`).
#[test]
fn the_darken_mode_takes_the_smaller_channel() {
    // The red channel is the source's 60, the green channel the
    // destination's 100 and the blue channel the destination's 50.
    assert_eq!(
        blend(9, [200, 100, 50, 128], [60, 180, 90, 192], 255),
        [60, 100, 50, 128]
    );
}

/// Reference: below the full opacity "DarkenBlend" runs `darken_blend_func`,
/// which mixes the darker channel into the destination by the opacity with
/// `d + ((f - d) * a >> 8)`
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:278-292`); the destination's
/// alpha is held by the HDA wrapper
/// (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:84-92`).
#[test]
fn the_darken_mode_mixes_the_darker_channel_by_the_opacity() {
    // `min` then `d + ((min - d) * 128 >> 8)`:
    //   min(200, 60)  =  60 -> 200 + (-140 * 128 >> 8) = 200 - 70  = 130
    //   min(100, 180) = 100 -> 100 + (   0 * 128 >> 8) = 100 +  0  = 100
    //   min(50, 90)   =  50 ->  50 + (   0 * 128 >> 8) =  50 +  0  =  50
    //   min(10, 250)  =  10 ->  10 + (   0 * 128 >> 8) =  10 +  0  =  10
    //   min(240, 20)  =  20 -> 240 + (-220 * 128 >> 8) = 240 - 110 = 130
    //   min(5, 200)   =   5 ->   5 + (   0 * 128 >> 8) =   5 +  0  =   5
    // and the destinations' alphas of 128 and 200 are held.
    assert_eq!(
        operate_row(
            9,
            128,
            &[[200, 100, 50, 128], [10, 240, 5, 200]],
            &[[60, 180, 90, 192], [250, 20, 200, 64]],
        ),
        [[130, 100, 50, 128], [10, 130, 5, 200]]
    );
}

/// Reference: the lighten blend is the larger of the two channels
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:297-306`), the functor
/// the software renderer's "LightenBlend" method runs
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2876`). At the full opacity the
/// method runs the plain functor, which takes the larger channel
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:1802-1810`).
#[test]
fn the_lighten_mode_takes_the_larger_channel() {
    // The red channel is the destination's 200, the green channel the
    // source's 180 and the blue channel the source's 90.
    assert_eq!(
        blend(10, [200, 100, 50, 128], [60, 180, 90, 192], 255),
        [200, 180, 90, 128]
    );
}

/// Reference: below the full opacity "LightenBlend" runs `lighten_blend_func`,
/// which mixes the lighter channel into the destination by the opacity with
/// `d + ((f - d) * a >> 8)`
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:307-322`); the destination's
/// alpha is held by the HDA wrapper
/// (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:84-92`).
#[test]
fn the_lighten_mode_mixes_the_lighter_channel_by_the_opacity() {
    // `max` then `d + ((max - d) * 128 >> 8)`:
    //   max(200, 60)  = 200 -> 200 + (  0 * 128 >> 8) = 200 +   0 = 200
    //   max(100, 180) = 180 -> 100 + ( 80 * 128 >> 8) = 100 +  40 = 140
    //   max(50, 90)   =  90 ->  50 + ( 40 * 128 >> 8) =  50 +  20 =  70
    //   max(10, 250)  = 250 ->  10 + (240 * 128 >> 8) =  10 + 120 = 130
    //   max(240, 20)  = 240 -> 240 + (  0 * 128 >> 8) = 240 +   0 = 240
    //   max(5, 200)   = 200 ->   5 + (195 * 128 >> 8) =   5 +  97 = 102
    // and the destinations' alphas of 128 and 200 are held.
    assert_eq!(
        stretch_row(
            10,
            128,
            &[[200, 100, 50, 128], [10, 240, 5, 200]],
            &[[60, 180, 90, 192], [250, 20, 200, 64]],
        ),
        [[200, 140, 70, 128], [130, 240, 102, 200]]
    );
}

/// Reference: the screen blend is the complement of the product of the two
/// complements, truncated by eight bits
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:326-336`), the functor
/// the software renderer's "ScreenBlend" method runs
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2877`). At the full opacity the
/// method runs the plain functor
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:1802-1810`). The blend reads
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

/// Reference: below the full opacity "ScreenBlend" runs
/// `screen_blend_HDA_o_func`, which fades the source with `s = ~((s * a) >> 8)`
/// and screens the complement of the destination with it, so a channel is
/// `255 - (((255 - d) * (255 - ((s * opa) >> 8))) >> 8)`
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:350-362`).
#[test]
fn the_screen_mode_fades_the_source_before_screening() {
    // `s2 = 255 - ((s * opa) >> 8)`, then `255 - (((255 - d) * s2) >> 8)`:
    //   s2(60)  = 255 - 30  = 225 -> 255 - ( 55 * 225 = 12375 >> 8) = 255 -  48 = 207
    //   s2(180) = 255 - 90  = 165 -> 255 - (155 * 165 = 25575 >> 8) = 255 -  99 = 156
    //   s2(90)  = 255 - 45  = 210 -> 255 - (205 * 210 = 43050 >> 8) = 255 - 168 =  87
    //   s2(250) = 255 - 125 = 130 -> 255 - (245 * 130 = 31850 >> 8) = 255 - 124 = 131
    //   s2(20)  = 255 - 10  = 245 -> 255 - ( 15 * 245 =  3675 >> 8) = 255 -  14 = 241
    //   s2(200) = 255 - 100 = 155 -> 255 - (250 * 155 = 38750 >> 8) = 255 - 151 = 104
    // and the destinations' alphas of 128 and 200 are held.
    assert_eq!(
        operate_row(
            11,
            128,
            &[[200, 100, 50, 128], [10, 240, 5, 200]],
            &[[60, 180, 90, 192], [250, 20, 200, 64]],
        ),
        [[207, 156, 87, 128], [131, 241, 104, 200]]
    );
}

/// Reference: the additive-alpha blend on an opaque destination is the
/// "AdditiveAlphaBlend" method
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2413-2414`), whose kernel
/// scales the destination by the source's remaining alpha and adds the source
/// (`reference/krkr2/cpp/core/visual/gl/blend_util_func.h:35-43`); the
/// destination's alpha is held
/// (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:74-84`).
#[test]
fn the_add_alpha_mode_scales_the_destination_by_the_remaining_alpha() {
    // At the full opacity the source is not faded and the destination scales
    // by the source's remaining alpha `255 - 192` = 63: the red channel is
    // `200 * 63 >> 8` = 49 and the source adds 60, which is 109; the green
    // channel is `100 * 63 >> 8` = 24 and the source adds 180, which is 204;
    // the blue channel is `50 * 63 >> 8` = 12 and the source adds 90, which
    // is 102.
    assert_eq!(
        blend_on(
            12,
            DestAlpha::Opaque,
            [200, 100, 50, 128],
            [60, 180, 90, 192],
            255,
            false
        ),
        [109, 204, 102, 128]
    );
}

/// Reference: below the full opacity "AdditiveAlphaBlend" runs
/// `premulalpha_blend_HDA_o_functor`
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:132-152`), which fades
/// the source and its alpha by the opacity and then scales the destination by
/// the source's remaining alpha.
#[test]
fn the_add_alpha_mode_fades_the_source_by_the_opacity() {
    // The source fades to `(60 * 128) >> 8` = 30 and its alpha to
    // `(192 * 128) >> 8` = 96, so the destination scales by `255 - 96` = 159:
    // the red channel is `200 * 159 >> 8` = 124 and the source adds 30, which
    // is 154; the green channel is `100 * 159 >> 8` = 62 and the source adds
    // 90, which is 152; the blue channel is `50 * 159 >> 8` = 31 and the
    // source adds 45, which is 76.
    assert_eq!(
        blend_on(
            12,
            DestAlpha::Opaque,
            [200, 100, 50, 128],
            [60, 180, 90, 192],
            128,
            false
        ),
        [154, 152, 76, 128]
    );
}

/// Reference: the additive-alpha blend onto an alpha destination has no
/// implementation (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2419-2420`),
/// so the pass leaves the destination untouched.
#[test]
fn the_add_alpha_mode_does_nothing_on_an_alpha_destination() {
    assert_eq!(
        blend(12, [200, 100, 50, 128], [60, 180, 90, 192], 255),
        [200, 100, 50, 128]
    );
}

/// Reference: the additive-alpha blend onto an additive-alpha destination is
/// the "AdditiveAlphaBlend_a" method
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2416-2417`), whose
/// kernel is `TVPAddAlphaBlend_a_a`
/// (`reference/krkr2/cpp/core/visual/tvpgl.h:116-133`,
/// `reference/krkr2/cpp/core/visual/gl/blend_util_func.h:70-84`): the two
/// alphas compose as `Da = Sa + Da - SaDa`, the destination's colour is
/// scaled by the source's remaining alpha and the source's colour is added.
///
/// The destination's alpha of 128 and the source's of 192 compose to
/// `128 + 192 - (128 * 192 >> 8) = 224`, and the destination's colour is
/// scaled by `255 - 192 = 63`: the red channel is `200 * 63 >> 8 = 49` and
/// the source adds 60, which is 109; the green channel is `100 * 63 >> 8` =
/// 24 and the source adds 180, which is 204; the blue channel is
/// `50 * 63 >> 8 = 12` and the source adds 90, which is 102.
#[test]
fn the_add_alpha_mode_composes_the_alphas_on_an_additive_alpha_destination() {
    assert_eq!(
        blend_on(
            12,
            DestAlpha::Additive,
            [200, 100, 50, 128],
            [60, 180, 90, 192],
            255,
            false
        ),
        [109, 204, 102, 224]
    );
    // An opaque destination keeps its alpha: the composed alpha is
    // `255 + 64 - (255 * 64 >> 8) = 256`, and the adjustment of the alpha by
    // its own eighth brings it back to 255. The destination's colour is
    // scaled by `255 - 64 = 191`: the red channel is `10 * 191 >> 8 = 7`
    // and the source adds 200, which is 207; the green channel is
    // `20 * 191 >> 8 = 14` and the source adds 150, which is 164; the blue
    // channel is `30 * 191 >> 8 = 22` and the source adds 100, which is 122.
    assert_eq!(
        blend_on(
            12,
            DestAlpha::Additive,
            [10, 20, 30, 255],
            [200, 150, 100, 64],
            255,
            false
        ),
        [207, 164, 122, 255]
    );
}

/// Reference: below the full opacity the additive-alpha blend onto an
/// additive-alpha destination is the "AdditiveAlphaBlend_ao" method
/// (`reference/krkr2/cpp/core/visual/RenderManager.cpp:2416-2417`), whose
/// kernel is `TVPAddAlphaBlend_a_a_o`
/// (`reference/krkr2/cpp/core/visual/tvpgl.h:153-159`,
/// `reference/krkr2/cpp/core/visual/gl/blend_util_func.h:85-93`): the source
/// and its alpha are faded by the opacity before the blend runs.
#[test]
fn the_add_alpha_mode_fades_the_source_on_an_additive_alpha_destination() {
    // The source fades to `(60 * 128) >> 8` = 30, `(180 * 128) >> 8` = 90 and
    // `(90 * 128) >> 8` = 45, and its alpha to `(192 * 128) >> 8` = 96. The
    // alphas compose to `128 + 96 - (128 * 96 >> 8) = 176`, and the
    // destination's colour is scaled by `255 - 96 = 159`: the red channel is
    // `200 * 159 >> 8` = 124 and the source adds 30, which is 154; the green
    // channel is `100 * 159 >> 8` = 62 and the source adds 90, which is 152;
    // the blue channel is `50 * 159 >> 8` = 31 and the source adds 45, which
    // is 76.
    assert_eq!(
        blend_on(
            12,
            DestAlpha::Additive,
            [200, 100, 50, 128],
            [60, 180, 90, 192],
            128,
            false
        ),
        [154, 152, 76, 176]
    );
    // The source fades to `(200 * 64) >> 8` = 50, `(150 * 64) >> 8` = 37 and
    // `(100 * 64) >> 8` = 25, and its alpha to `(64 * 64) >> 8` = 16. The
    // destination's colour is scaled by `255 - 16 = 239`: the red channel is
    // `10 * 239 >> 8` = 9 and the source adds 50, which is 59; the green
    // channel is `20 * 239 >> 8` = 18 and the source adds 37, which is 55;
    // the blue channel is `30 * 239 >> 8` = 28 and the source adds 25, which
    // is 53.
    assert_eq!(
        blend_on(
            12,
            DestAlpha::Additive,
            [10, 20, 30, 255],
            [200, 150, 100, 64],
            64,
            false
        ),
        [59, 55, 53, 255]
    );
}
