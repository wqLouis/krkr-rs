//! Offscreen render-target and framebuffer-readback tests.
//!
//! The null backend's CPU path always runs here; the Vulkan target contract
//! test skips gracefully when no device is available (the CI environment is
//! headless). Per D9 the target and its pixels are explicit state, so these
//! tests drive the public free functions only.

mod common;

use api::blend::{BlendMode, DstBlend};
use api::error::RenderError;
use api::geometry::{PixelRegion, RectF};
use render::{
    PipelineConfig, RenderContent, RenderNode, RenderPipeline, RenderScene, RenderTarget,
    RenderTargetDesc, begin_render_state, capabilities, create_render_pipeline,
    create_render_target, create_texture, destroy_render_target, read_render_target,
    render_to_target, upload_texture,
};

/// Renders a 1×1 target with `background` filled, then a unit solid `src`
/// composited with `mode`, and returns the resulting RGBA8 pixel.
fn composite_one_pixel(mode: BlendMode, background: [f32; 4], src: [f32; 4]) -> [u8; 4] {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) =
        begin_render_state(&pipeline, &common::headless_window()).expect("begin");
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
    let (_surface, mut state) =
        begin_render_state(&pipeline, &common::headless_window()).expect("begin");
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
    for texel in out.chunks_exact(4) {
        assert_eq!(texel, [255, 0, 0, 255]);
    }
}

#[test]
fn null_offscreen_solid_fill_and_partial_readback() {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) =
        begin_render_state(&pipeline, &common::headless_window()).expect("begin");
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
    let (_surface, mut state) =
        begin_render_state(&pipeline, &common::headless_window()).expect("begin");
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
    for texel in out.chunks_exact(4) {
        assert_eq!(texel, [0, 255, 0, 255]);
    }
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
    let (_surface, mut state) =
        begin_render_state(&pipeline, &common::headless_window()).expect("begin");
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
    let (_surface, mut state) =
        begin_render_state(&pipeline, &common::headless_window()).expect("begin");
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
    // The GPU path still defers destination-dependent blends (D11).
    assert!(!caps.supports_destination_dependent_blend);
}
