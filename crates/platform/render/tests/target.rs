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
    RenderTargetDesc, begin_render_state, capabilities, create_render_pipeline,
    create_render_target, create_texture, destroy_render_target, read_render_target,
    render_to_target, upload_texture,
};
use testkit::headless_window;

/// Renders a 1×1 target with `background` filled, then a unit solid `src`
/// composited with `mode`, and returns the resulting RGBA8 pixel.
fn composite_one_pixel(mode: BlendMode, background: [f32; 4], src: [f32; 4]) -> [u8; 4] {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let target = create_render_target(&pipeline, RenderTargetDesc::new(1, 1)).expect("target");

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
    assert_eq!(out, [242, 243, 224, 0]);
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
/// additive-alpha blend is colour-only — it adds the source's colour to the
/// destination and leaves the destination's alpha alone
/// (`reference/krkr2/cpp/core/visual/gl/blend_util_func.h:35-43`: the masks
/// `0xff00ff` and `0xff00` cover the red/blue and green lanes, and the alpha
/// byte is in neither) — so the picture and the opaque alpha byte both
/// survive.
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
    // opaque, so the result is exactly the per-channel blend function.
    let dst = [0.25, 0.5, 0.75, 1.0];
    let src = [0.75, 0.5, 0.25, 1.0];
    let cases: &[(DstBlend, [u8; 4])] = &[
        (DstBlend::Darken, [64, 128, 64, 255]),
        (DstBlend::Lighten, [191, 128, 191, 255]),
        (DstBlend::Difference, [127, 0, 127, 255]),
        (DstBlend::Exclusion, [159, 127, 159, 255]),
        (DstBlend::Overlay, [96, 128, 159, 255]),
        (DstBlend::HardLight, [159, 128, 96, 255]),
        (DstBlend::ColorDodge, [255, 255, 255, 255]),
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
