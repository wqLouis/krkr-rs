//! Offscreen render-target and framebuffer-readback tests.
//!
//! The null backend's CPU path always runs here; the Vulkan target contract
//! test skips gracefully when no device is available (the CI environment is
//! headless). The target and its pixels are explicit state, so these
//! tests drive the public free functions only.

use api::blend::{BlendMode, DstBlend};
use api::error::RenderError;
use api::geometry::{PixelRegion, RectF};
use render::{
    PipelineConfig, RenderContent, RenderNode, RenderPipeline, RenderScene, RenderTarget,
    RenderTargetDesc, begin_offscreen_render_state, begin_render_state, capabilities,
    create_render_pipeline, create_render_target, create_texture, destroy_render_target,
    read_render_target, render_to_target, upload_texture,
};
use testkit::headless_window;

/// One blend case: the mode, the source, the destination and the expected
/// result.
type BlendCase = (BlendMode, [u8; 4], [u8; 4], [u8; 4]);

/// The scene that composites `src` over `background` into one pixel.
fn one_pixel_scene(mode: BlendMode, background: [f32; 4], src: [f32; 4]) -> RenderScene {
    let mut scene = RenderScene::with_root_node();
    let root = scene.root;
    let child = scene.push(RenderNode {
        parent: Some(root),
        content: RenderContent::Solid(src),
        dst: RectF::new(0.0, 0.0, 1.0, 1.0),
        blend: mode,
        ..RenderNode::default()
    });
    scene.node_mut(root).expect("root").children = vec![child];
    scene.background = Some(background);
    scene
}

/// Renders a 1×1 target with `background` filled, then a unit solid `src`
/// composited with `mode`, and returns the resulting RGBA8 pixel.
fn composite_one_pixel(mode: BlendMode, background: [f32; 4], src: [f32; 4]) -> [u8; 4] {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let target = create_render_target(&pipeline, RenderTargetDesc::new(1, 1)).expect("target");

    let scene = one_pixel_scene(mode, background, src);
    render_to_target(&pipeline, &mut state, &target, &scene).expect("render");
    let mut out = [0u8; 4];
    read_render_target(
        &pipeline,
        &state,
        &target,
        PixelRegion::new(0, 0, 1, 1),
        &mut out,
    )
    .expect("read");
    out
}

#[test]
fn null_offscreen_target_reads_back_the_clear_colour() {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let target = create_render_target(&pipeline, RenderTargetDesc::new(3, 2)).expect("target");

    let mut scene = RenderScene::with_root_node();
    scene.background = Some([1.0, 0.0, 0.0, 1.0]);
    render_to_target(&pipeline, &mut state, &target, &scene).expect("render");

    let mut out = vec![0u8; 3 * 2 * 4];
    read_render_target(
        &pipeline,
        &state,
        &target,
        PixelRegion::new(0, 0, 3, 2),
        &mut out,
    )
    .expect("read");
    for texel in out.as_chunks::<4>().0 {
        assert_eq!(texel, &[255, 0, 0, 255]);
    }
}

#[test]
fn null_offscreen_solid_fill_and_partial_readback() {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let target = create_render_target(&pipeline, RenderTargetDesc::new(2, 2)).expect("target");

    let mut scene = RenderScene::with_root_node();
    let root = scene.root;
    let child = scene.push(RenderNode {
        parent: Some(root),
        content: RenderContent::Solid([0.0, 0.0, 1.0, 1.0]),
        dst: RectF::new(0.0, 0.0, 2.0, 2.0),
        ..RenderNode::default()
    });
    scene.node_mut(root).expect("root").children = vec![child];
    scene.background = Some([1.0, 0.0, 0.0, 1.0]);

    render_to_target(&pipeline, &mut state, &target, &scene).expect("render");

    // A 1×1 readback of the bottom-right pixel touches only that pixel.
    let mut out = [0u8; 4];
    read_render_target(
        &pipeline,
        &state,
        &target,
        PixelRegion::new(1, 1, 1, 1),
        &mut out,
    )
    .expect("read");
    assert_eq!(out, [0, 0, 255, 255]);
}

#[test]
fn null_offscreen_textured_quad_reads_back() {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let target = create_render_target(&pipeline, RenderTargetDesc::new(2, 2)).expect("target");

    let texture = create_texture(&pipeline, render::TextureDesc::new(2, 2)).expect("texture");
    upload_texture(
        &pipeline,
        &mut state,
        &texture,
        PixelRegion::new(0, 0, 2, 2),
        &[0u8, 255, 0, 255].repeat(4),
    )
    .expect("upload");

    let mut scene = RenderScene::with_root_node();
    let root = scene.root;
    let child = scene.push(RenderNode {
        parent: Some(root),
        content: RenderContent::Texture(texture),
        dst: RectF::new(0.0, 0.0, 2.0, 2.0),
        ..RenderNode::default()
    });
    scene.node_mut(root).expect("root").children = vec![child];
    scene.background = Some([1.0, 0.0, 0.0, 1.0]);

    render_to_target(&pipeline, &mut state, &target, &scene).expect("render");
    let mut out = vec![0u8; 16];
    read_render_target(
        &pipeline,
        &state,
        &target,
        PixelRegion::new(0, 0, 2, 2),
        &mut out,
    )
    .expect("read");
    for texel in out.as_chunks::<4>().0 {
        assert_eq!(texel, &[0, 255, 0, 255]);
    }
}

#[test]
fn null_offscreen_texture_honours_node_tint() {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let target = create_render_target(&pipeline, RenderTargetDesc::new(1, 1)).expect("target");

    let texture = create_texture(&pipeline, render::TextureDesc::new(1, 1)).expect("texture");
    upload_texture(
        &pipeline,
        &mut state,
        &texture,
        PixelRegion::new(0, 0, 1, 1),
        &[200, 100, 50, 255],
    )
    .expect("upload");

    let mut scene = RenderScene::with_root_node();
    let root = scene.root;
    let child = scene.push(RenderNode {
        parent: Some(root),
        content: RenderContent::Texture(texture),
        dst: RectF::new(0.0, 0.0, 1.0, 1.0),
        color: [0.5, 1.0, 0.0, 1.0],
        ..RenderNode::default()
    });
    scene.node_mut(root).expect("root").children = vec![child];
    scene.background = Some([0.0, 0.0, 0.0, 1.0]);

    render_to_target(&pipeline, &mut state, &target, &scene).expect("render");
    let mut out = [0u8; 4];
    read_render_target(
        &pipeline,
        &state,
        &target,
        PixelRegion::new(0, 0, 1, 1),
        &mut out,
    )
    .expect("read");
    assert_eq!(out, [100, 100, 0, 255]);
}

#[test]
fn source_over_honours_source_alpha() {
    let out = composite_one_pixel(
        BlendMode::SourceOver,
        [1.0, 1.0, 1.0, 1.0],
        [0.0, 0.0, 0.0, 0.5],
    );
    assert_eq!(out, [128, 128, 128, 255]);
}

/// The decisive experiment for the title screen's background layer: one
/// opaque layer over the cleared background.
///
/// The scene's background is the projection's opaque black
/// (`crates/engine/src/projection.rs`, `BACKGROUND_CLEAR = [0, 0, 0, 1]`), and
/// the layer's own bitmap is fully opaque: the first pixel of the title
/// screen's background layer is `[242, 243, 224, 255]`. The reference's plain
/// alpha blend mixes the colours by the source's alpha and leaves the
/// destination's alpha byte alone
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:67-75`: the masks
/// `0xff00ff` and `0xff00` cover the red/blue and green lanes, and the alpha
/// byte is in neither), so the composited frame carries the picture and an
/// opaque alpha byte.
///
/// The port composites in `f32`, so a channel that lands on an eight-bit
/// boundary can differ from the reference's integer path by one level (D34);
/// the values below stay off that boundary.
#[test]
fn an_opaque_layer_over_the_cleared_background_is_opaque() {
    let out = composite_one_pixel(
        BlendMode::SourceOver,
        [0.0, 0.0, 0.0, 1.0],
        [242.0 / 255.0, 243.0 / 255.0, 224.0 / 255.0, 1.0],
    );
    assert_eq!(out, [242, 243, 224, 255]);
}

/// A blend that holds the destination's alpha leaves the frame's alpha where
/// the destination had it, and the frame keeps the picture's colour.
///
/// This is the state the title screen's background layer leaves the composed
/// frame in when the frame's alpha is zero: the picture is in the frame, and
/// the frame's alpha is zero. The window shows the frame's colour — the
/// reference's window surface is presented as the window's image — so the
/// present must copy that colour rather than scale it by the frame's alpha
/// (the Vulkan present's blend state, `crates/platform/render/src/vulkan/
/// pipelines.rs`, `KEEP_ALPHA_COPY_SLOT`).
#[test]
fn a_blend_that_holds_the_destination_alpha_keeps_the_colour() {
    let out = composite_one_pixel(
        BlendMode::PsNormal,
        [0.0, 0.0, 0.0, 0.0],
        [242.0 / 255.0, 243.0 / 255.0, 224.0 / 255.0, 1.0],
    );
    // The Photoshop normal blend truncates its mix by eight bits
    // (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-383`), so
    // every channel lands one level below the source's.
    assert_eq!(out, [241, 242, 223, 0]);
}

/// Composites two solid layers over a 1×1 background, in order, and returns
/// the resulting RGBA8 pixel.
///
/// The layers are drawn in the order they are given, so the second one is
/// composited over the first one's result.
fn composite_two_layers(
    background: [f32; 4],
    first: (BlendMode, [f32; 4]),
    second: (BlendMode, [f32; 4]),
) -> [u8; 4] {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let target = create_render_target(&pipeline, RenderTargetDesc::new(1, 1)).expect("target");

    let mut scene = RenderScene::with_root_node();
    let root = scene.root;
    let mut children = Vec::new();
    for (mode, colour) in [first, second] {
        let child = scene.push(RenderNode {
            parent: Some(root),
            content: RenderContent::Solid(colour),
            dst: RectF::new(0.0, 0.0, 1.0, 1.0),
            blend: mode,
            ..RenderNode::default()
        });
        children.push(child);
    }
    scene.node_mut(root).expect("root").children = children;
    scene.background = Some(background);

    render_to_target(&pipeline, &mut state, &target, &scene).expect("render");
    let mut out = [0u8; 4];
    read_render_target(
        &pipeline,
        &state,
        &target,
        PixelRegion::new(0, 0, 1, 1),
        &mut out,
    )
    .expect("read");
    out
}

/// A full-screen transparent additive-alpha layer over an opaque layer must
/// not change the destination's alpha.
///
/// This is the title screen: the background layer is fully opaque, and a
/// transparent additive-alpha layer covers the whole screen. The
/// additive-alpha blend is colour-only — it scales the destination by the
/// source's remaining alpha and adds the source's colour, and it leaves the
/// destination's alpha alone
/// (`reference/krkr2/cpp/core/visual/gl/blend_util_func.h:35-43`: the masks
/// `0xff00ff` and `0xff00` cover the red/blue and green lanes, and the alpha
/// byte is in neither) — so the picture and the opaque alpha byte both
/// survive.
///
/// The port composites in `f32`, so it scales the destination exactly; the
/// reference's integer kernel truncates that scale (`tvpgl.h:91-92` divides
/// by 256) and would leave the picture a level darker in each lane.
#[test]
fn a_transparent_additive_alpha_layer_keeps_the_destination_alpha() {
    let out = composite_two_layers(
        [0.0, 0.0, 0.0, 1.0],
        (
            BlendMode::SourceOver,
            [242.0 / 255.0, 243.0 / 255.0, 224.0 / 255.0, 1.0],
        ),
        (BlendMode::AddAlpha, [0.0, 0.0, 0.0, 0.0]),
    );
    assert_eq!(out, [242, 243, 224, 255]);
}

/// The additive-alpha blend scales the destination by the source's
/// remaining alpha and adds the source's colour.
///
/// Reference: the additive-alpha blend is `TVPAddAlphaBlend_n_a`
/// (`reference/krkr2/cpp/core/visual/tvpgl.h:88-94`): the destination's
/// colour is scaled by the source's remaining alpha (`sopa = (~src) >> 24`)
/// and the source is added to it with a saturating per-byte sum
/// (`TVPSaturatedAdd`, `tvpgl.h:71-81`). The blend is colour-only — the
/// masks `0xff00ff` and `0xff00` cover the red/blue and the green lanes and
/// the alpha byte is in neither — so the held face
/// (`TVPAddAlphaBlend_HDA_n_a`, `tvpgl.h:96-100`) keeps the destination's
/// alpha.
///
/// The expected pixels are the reference kernel's own bytes. The reference
/// scales a lane with `(lane * sopa) >> 8`, which divides by 256, while the
/// fixed function and this path scale it exactly; the two agree on a lane
/// only where the product's quotient and remainder sum to at most 127. Every
/// destination lane below is even, which is such a lane for a source alpha
/// of 127, and the third vector's source alpha of 200 keeps its lanes on the
/// same side of the boundary.
#[test]
fn the_additive_alpha_blend_scales_the_destination_and_adds_the_source() {
    let cases: &[BlendCase] = &[
        // The destination is scaled by the source's remaining alpha and the
        // source's colour is added: 200·128/256 + 10 = 110, 100·128/256 + 20
        // = 70, 50·128/256 + 30 = 55.
        (
            BlendMode::AddAlpha,
            [200, 100, 50, 255],
            [10, 20, 30, 127],
            [110, 70, 55, 255],
        ),
        // The sum saturates per byte: 240·128/256 + 200 = 320 saturates at
        // 255, 240·128/256 + 100 = 220, 240·128/256 + 50 = 170.
        (
            BlendMode::AddAlpha,
            [240, 240, 240, 255],
            [200, 100, 50, 127],
            [255, 220, 170, 255],
        ),
        // A source alpha of 200 leaves the destination a fifth of its
        // brightness: 98·55/256 + 5 = 26, 56·55/256 + 10 = 22,
        // 20·55/256 + 15 = 19.
        (
            BlendMode::AddAlpha,
            [98, 56, 20, 255],
            [5, 10, 15, 200],
            [26, 22, 19, 255],
        ),
        // The blend is colour-only: the destination's alpha byte is kept.
        (
            BlendMode::AddAlpha,
            [100, 100, 100, 128],
            [10, 20, 30, 127],
            [60, 70, 80, 128],
        ),
    ];
    for (mode, destination, source, expected) in cases {
        let out = composite_one_pixel(*mode, normalised(*destination), normalised(*source));
        assert_eq!(out, *expected, "the blend {mode:?}");
    }
}

/// The normalised `[0, 1]` colour of a byte pixel.
fn normalised(pixel: [u8; 4]) -> [f32; 4] {
    [
        f32::from(pixel[0]) / 255.0,
        f32::from(pixel[1]) / 255.0,
        f32::from(pixel[2]) / 255.0,
        f32::from(pixel[3]) / 255.0,
    ]
}

/// The alpha each blend leaves on the destination.
///
/// The destination's alpha is 128 and the source's alpha is 128, so a blend
/// that keeps the destination's alpha leaves 128, the plain copy takes the
/// source's 128, and the source-over writes the mix of the two.
///
/// The colour-only blends are the additive, the subtractive, the
/// additive-alpha, the multiplicative, the screen and the Photoshop family
/// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:190-197`, `:209-217`,
/// `:222-229`, `:337-348`, `:374-381` and
/// `reference/krkr2/cpp/core/visual/gl/blend_util_func.h:35-43`): their masks
/// exclude the alpha byte, so they cannot change it.
#[test]
fn every_blend_leaves_the_alpha_its_face_names() {
    let dst = [0.25, 0.5, 0.75, 128.0 / 255.0];
    let src = [0.75, 0.5, 0.25, 128.0 / 255.0];
    let cases: &[(BlendMode, u8)] = &[
        // The source-over writes the source's alpha: the mix of the two.
        (BlendMode::SourceOver, 192),
        // The plain copy takes the source's alpha.
        (BlendMode::Replace, 128),
        // Every other blend keeps the destination's alpha.
        (BlendMode::Additive, 128),
        (BlendMode::Subtractive, 128),
        (BlendMode::AddAlpha, 128),
        (BlendMode::Multiply, 128),
        (BlendMode::Screen, 128),
        (BlendMode::PsNormal, 128),
        (BlendMode::PsAdditive, 128),
        (BlendMode::PsSubtractive, 128),
        (BlendMode::PsMultiplicative, 128),
        (BlendMode::PsScreen, 128),
        (BlendMode::DestinationDependent(DstBlend::ColorDodge), 128),
    ];
    for (mode, expected) in cases {
        let out = composite_one_pixel(*mode, dst, src);
        assert_eq!(out[3], *expected, "the mode {mode:?}");
    }
}

#[test]
fn destination_dependent_blends_sample_the_destination() {
    // Destination `[0.25, 0.5, 0.75]`, source `[0.75, 0.5, 0.25]`, both
    // opaque, so the result is the per-channel blend function, truncated by
    // eight bits (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:447-660`).
    let dst = [0.25, 0.5, 0.75, 1.0];
    let src = [0.75, 0.5, 0.25, 1.0];
    let cases: &[(DstBlend, [u8; 4])] = &[
        (DstBlend::Darken, [64, 128, 64, 255]),
        (DstBlend::Lighten, [190, 128, 191, 255]),
        (DstBlend::Difference, [126, 0, 127, 255]),
        (DstBlend::Exclusion, [159, 128, 160, 255]),
        (DstBlend::Overlay, [94, 128, 160, 255]),
        (DstBlend::HardLight, [159, 128, 95, 255]),
        (DstBlend::ColorDodge, [254, 254, 254, 255]),
        (DstBlend::ColorBurn, [0, 2, 0, 255]),
    ];
    for (mode, expected) in cases {
        let out = composite_one_pixel(BlendMode::DestinationDependent(*mode), dst, src);
        assert_eq!(out, *expected, "blend {mode:?}");
    }
}

#[test]
fn fixed_function_blends_match_the_baseline() {
    let dst = [0.25, 0.5, 0.75, 1.0];
    let src = [0.75, 0.5, 0.25, 1.0];
    let cases: &[(BlendMode, [u8; 4])] = &[
        (BlendMode::Multiply, [48, 64, 48, 255]),
        (BlendMode::Screen, [207, 192, 207, 255]),
    ];
    for (mode, expected) in cases {
        let out = composite_one_pixel(*mode, dst, src);
        assert_eq!(out, *expected, "blend {mode:?}");
    }
}

/// Reference: the Photoshop subtractive blend is the linear burn: the source
/// is inverted and the difference saturates, so the channel is
/// `max(0, d + s - 255)`
/// (`reference/krkr2/cpp/core/visual/tvpgl.cpp:12565-12572`,
/// `reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:401-412`). The
/// GPU's shader computes the same formula
/// (`reference/krkr2/cpp/core/visual/ogl/RenderManager_ogl.cpp:3723-3727`).
#[test]
fn the_photoshop_subtractive_blend_is_a_linear_burn() {
    // The destination is 204 and the source 102 with an alpha of 128: the
    // burn is `max(0, 204 + 102 - 255) = 51`, and the source's alpha mixes
    // it over the destination: `204 + ((51 - 204) * 128 >> 8) = 127`.
    let out = composite_one_pixel(
        BlendMode::PsSubtractive,
        [204.0 / 255.0, 204.0 / 255.0, 204.0 / 255.0, 1.0],
        [102.0 / 255.0, 102.0 / 255.0, 102.0 / 255.0, 128.0 / 255.0],
    );
    assert_eq!(out, [127, 127, 127, 255]);
}

#[test]
fn null_capabilities_advertise_the_cpu_target_path() {
    let pipeline = RenderPipeline::null();
    let caps = capabilities(&pipeline);
    assert!(caps.supports_offscreen_targets);
    assert!(caps.supports_destination_dependent_blend);
}

#[test]
fn render_target_descriptor_validation() {
    let pipeline = RenderPipeline::null();
    assert!(matches!(
        create_render_target(&pipeline, RenderTargetDesc::new(0, 4)),
        Err(RenderError::InvalidArgument { .. })
    ));
    let single_channel = RenderTargetDesc::new(4, 4).with_format(render::TextureFormat::R8Unorm);
    assert!(matches!(
        create_render_target(&pipeline, single_channel),
        Err(RenderError::InvalidArgument { .. })
    ));
}

#[test]
fn readback_rejects_a_mismatched_buffer_and_region() {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let target = create_render_target(&pipeline, RenderTargetDesc::new(2, 2)).expect("target");
    let mut scene = RenderScene::with_root_node();
    scene.background = Some([0.0, 0.0, 0.0, 1.0]);
    render_to_target(&pipeline, &mut state, &target, &scene).expect("render");

    let mut too_small = [0u8; 4];
    assert!(matches!(
        read_render_target(
            &pipeline,
            &state,
            &target,
            PixelRegion::new(0, 0, 2, 2),
            &mut too_small,
        ),
        Err(RenderError::Framebuffer { .. })
    ));
    let mut whole = [0u8; 16];
    assert!(matches!(
        read_render_target(
            &pipeline,
            &state,
            &target,
            PixelRegion::new(1, 1, 2, 2),
            &mut whole,
        ),
        Err(RenderError::Framebuffer { .. })
    ));
}

#[test]
fn dropping_a_render_target_releases_its_pixels() {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let target: RenderTarget =
        create_render_target(&pipeline, RenderTargetDesc::new(2, 2)).expect("target");
    let handle = target.clone();
    destroy_render_target(&pipeline, &mut state, target);

    // The stale handle can no longer be read.
    let mut out = [0u8; 16];
    assert!(matches!(
        read_render_target(
            &pipeline,
            &state,
            &handle,
            PixelRegion::new(0, 0, 2, 2),
            &mut out,
        ),
        Err(RenderError::Framebuffer { .. })
    ));
}

#[test]
fn vulkan_target_capabilities_or_skip() {
    // A headless device may not exist; skip gracefully when it does not.
    let Ok(pipeline) = create_render_pipeline(PipelineConfig::vulkan()) else {
        return;
    };
    let caps = capabilities(&pipeline);
    assert!(caps.supports_offscreen_targets);
    // The GPU path draws the destination-dependent blends with the
    // destination-blend program.
    assert!(caps.supports_destination_dependent_blend);
}

/// The GPU path is reachable without a window.
///
/// A Vulkan state created offscreen owns a logical device and no surface, so
/// a scene can be rendered into an offscreen target and read back on a
/// machine that has a GPU but no display. The pixels are the CPU path's
/// pixels: the two backends composite the same scene.
///
/// A machine without a Vulkan device skips the test, exactly like the other
/// Vulkan cases here.
#[test]
fn vulkan_offscreen_state_composites_the_cpu_paths_scene_or_skip() {
    let Ok(pipeline) = create_render_pipeline(PipelineConfig::vulkan()) else {
        return;
    };
    let Ok(mut state) = begin_offscreen_render_state(&pipeline) else {
        return;
    };
    let target = create_render_target(&pipeline, RenderTargetDesc::new(1, 1)).expect("target");
    let scene = one_pixel_scene(
        BlendMode::SourceOver,
        [0.0, 0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0, 1.0],
    );
    render_to_target(&pipeline, &mut state, &target, &scene).expect("render");
    let mut gpu = [0u8; 4];
    read_render_target(
        &pipeline,
        &state,
        &target,
        PixelRegion::new(0, 0, 1, 1),
        &mut gpu,
    )
    .expect("read");

    let cpu = composite_one_pixel(
        BlendMode::SourceOver,
        [0.0, 0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0, 1.0],
    );
    assert_eq!(
        gpu, cpu,
        "the GPU and the CPU must composite the same scene: {gpu:?} vs {cpu:?}"
    );
}
