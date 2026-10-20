//! Effect (custom-shader) pipeline tests.
//!
//! The headless backend always runs here: it reports no effect pipeline support
//! and refuses to create one. The Vulkan cases skip cleanly when the host has no
//! device or no surface (the CI environment has neither), and exercise the real
//! pipeline creation and an offscreen draw when it does.

use api::blend::{BlendMode, DstBlend};
use api::error::RenderError;
use api::geometry::{PixelRegion, RectF};
use api::shader::{BindingDesc, EffectUniforms, SamplerKind, ShaderModule};
use render::{
    EffectPipelineDesc, PipelineConfig, RenderContent, RenderNode, RenderPipeline, RenderScene,
    RenderState, Surface, Texture, TextureDesc, begin_render_state, capabilities,
    create_effect_pipeline, create_render_pipeline, create_render_target, create_texture,
    destroy_effect_pipeline, destroy_render_target, read_render_target, render_to_target,
    upload_texture,
};
use testkit::headless_window;

/// The shared vertex stage the engine's effect programs use.
const EFFECT_VERT: &[u8] = include_bytes!("../../../engine/shaders/effect.vert.spv");
/// The cross-fade fragment stage.
const CROSSFADE_FRAG: &[u8] = include_bytes!("../../../engine/shaders/crossfade.frag.spv");
/// The scroll fragment stage.
const SCROLL_FRAG: &[u8] = include_bytes!("../../../engine/shaders/scroll.frag.spv");

/// Uniform words every engine effect program declares.
const UNIFORM_WORDS: u32 = 16;
/// Word the traversal overwrites with the composed draw opacity.
const OPACITY_UNIFORM: u32 = 15;

/// Bindings of the cross-fade: two bilinearly sampled sources.
const CROSSFADE_BINDINGS: [BindingDesc; 2] = [
    BindingDesc::texture(1, SamplerKind::LinearClamp),
    BindingDesc::texture(2, SamplerKind::LinearClamp),
];
/// Bindings of a scroll pass: one source.
const SCROLL_BINDINGS: [BindingDesc; 1] = [BindingDesc::texture(1, SamplerKind::LinearClamp)];

/// A cross-fade pipeline descriptor over the committed program.
fn crossfade_desc() -> EffectPipelineDesc<'static> {
    let mut desc = EffectPipelineDesc::new(
        "crossfade",
        ShaderModule::vertex(EFFECT_VERT),
        ShaderModule::fragment(CROSSFADE_FRAG),
    );
    desc.bindings = &CROSSFADE_BINDINGS;
    desc.uniform_words = UNIFORM_WORDS;
    desc.opacity_uniform = Some(OPACITY_UNIFORM);
    desc
}

/// A scroll pipeline descriptor over the committed program.
fn scroll_desc() -> EffectPipelineDesc<'static> {
    let mut desc = EffectPipelineDesc::new(
        "scroll",
        ShaderModule::vertex(EFFECT_VERT),
        ShaderModule::fragment(SCROLL_FRAG),
    );
    desc.bindings = &SCROLL_BINDINGS;
    desc.uniform_words = UNIFORM_WORDS;
    desc.opacity_uniform = Some(OPACITY_UNIFORM);
    desc
}

/// A Vulkan pipeline with a render state, or `None` when the host cannot
/// provide a device and a surface. Keeping the surface alive is the caller's
/// job, so it is returned with the state.
fn vulkan_state() -> Option<(RenderPipeline, Surface, RenderState)> {
    let pipeline = create_render_pipeline(PipelineConfig::vulkan()).ok()?;
    let (surface, state) = begin_render_state(&pipeline, &headless_window()).ok()?;
    Some((pipeline, surface, state))
}

#[test]
fn the_null_backend_reports_no_effect_pipelines() {
    let pipeline = RenderPipeline::null();
    let caps = capabilities(&pipeline);
    assert!(!caps.supports_effect_pipelines);
    assert_eq!(caps.backend, render::BackendKind::Null);
}

#[test]
fn the_null_backend_refuses_an_effect_pipeline() {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let error = create_effect_pipeline(&pipeline, &mut state, &crossfade_desc())
        .expect_err("the headless backend has no effect pipelines");
    assert!(
        matches!(&error, RenderError::Unsupported { message } if message.contains("crossfade")),
        "unexpected error: {error}"
    );
}

#[test]
fn vulkan_creates_and_destroys_effect_pipelines_or_skips() {
    let Some((pipeline, _surface, mut state)) = vulkan_state() else {
        return;
    };
    assert!(capabilities(&pipeline).supports_effect_pipelines);

    let crossfade = create_effect_pipeline(&pipeline, &mut state, &crossfade_desc())
        .expect("create the cross-fade pipeline");
    assert_eq!(crossfade.name(), "crossfade");
    assert_eq!(crossfade.uniform_words(), UNIFORM_WORDS);
    assert_eq!(crossfade.opacity_uniform(), Some(OPACITY_UNIFORM));

    // A second creation is an independent pipeline object.
    let second = create_effect_pipeline(&pipeline, &mut state, &crossfade_desc())
        .expect("create a second cross-fade pipeline");
    assert_ne!(crossfade.key(), second.key());

    let scroll = create_effect_pipeline(&pipeline, &mut state, &scroll_desc())
        .expect("create the scroll pipeline");
    assert_eq!(scroll.name(), "scroll");

    destroy_effect_pipeline(&pipeline, &mut state, second);
    destroy_effect_pipeline(&pipeline, &mut state, scroll);
    destroy_effect_pipeline(&pipeline, &mut state, crossfade);
}

#[test]
fn vulkan_refuses_a_bad_module_or_skips() {
    let Some((pipeline, _surface, mut state)) = vulkan_state() else {
        return;
    };
    // A module the driver cannot parse: the magic number is corrupted.
    let mut corrupt = CROSSFADE_FRAG.to_vec();
    corrupt[0] = 0xff;
    let mut desc = crossfade_desc();
    desc.fragment = ShaderModule::fragment(&corrupt);
    let error = create_effect_pipeline(&pipeline, &mut state, &desc)
        .expect_err("a corrupted fragment module is refused");
    assert!(
        matches!(&error, RenderError::Shader { .. }),
        "unexpected error: {error}"
    );

    let mut truncated = crossfade_desc();
    truncated.vertex = ShaderModule::vertex(&EFFECT_VERT[..8]);
    let error = create_effect_pipeline(&pipeline, &mut state, &truncated)
        .expect_err("a truncated module is refused");
    assert!(
        matches!(&error, RenderError::Shader { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn vulkan_refuses_a_destination_dependent_blend_or_skips() {
    let Some((pipeline, _surface, mut state)) = vulkan_state() else {
        return;
    };
    let mut desc = crossfade_desc();
    desc.blend = BlendMode::DestinationDependent(DstBlend::Darken);
    let error = create_effect_pipeline(&pipeline, &mut state, &desc)
        .expect_err("a destination-dependent blend needs a destination sampler");
    assert!(
        matches!(&error, RenderError::Unsupported { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn vulkan_refuses_a_reserved_or_oversized_binding_set_or_skips() {
    let Some((pipeline, _surface, mut state)) = vulkan_state() else {
        return;
    };

    // Binding 0 holds the uniform block, so a texture cannot claim it.
    let reserved = [BindingDesc::texture(0, SamplerKind::LinearClamp)];
    let mut desc = crossfade_desc();
    desc.bindings = &reserved;
    let error = create_effect_pipeline(&pipeline, &mut state, &desc)
        .expect_err("binding index 0 is reserved");
    assert!(
        matches!(&error, RenderError::InvalidArgument { .. }),
        "unexpected error: {error}"
    );

    // Five textures is more than the API budget.
    let five = [
        BindingDesc::texture(1, SamplerKind::LinearClamp),
        BindingDesc::texture(2, SamplerKind::LinearClamp),
        BindingDesc::texture(3, SamplerKind::LinearClamp),
        BindingDesc::texture(4, SamplerKind::LinearClamp),
        BindingDesc::texture(5, SamplerKind::LinearClamp),
    ];
    let mut desc = crossfade_desc();
    desc.bindings = &five;
    let error = create_effect_pipeline(&pipeline, &mut state, &desc)
        .expect_err("five bindings exceed the budget");
    assert!(
        matches!(&error, RenderError::InvalidArgument { .. }),
        "unexpected error: {error}"
    );
}

/// The byte the GPU produces for a mix of two encoded channels at `weight`.
///
/// The shader decodes each sRGB sample to its encoded value, mixes there and
/// re-encodes for the sRGB target, so the result is the CPU blend's byte-space
/// lerp rather than a linear-light mix.
fn expected_mix(a: u8, b: u8, weight: f32) -> f32 {
    f32::from(a) * (1.0 - weight) + f32::from(b) * weight
}

/// Builds a one-pixel effect scene drawing `pipeline` over the two textures at
/// `weight`.
fn crossfade_scene(
    pipeline: &render::EffectPipeline,
    first: &Texture,
    second: &Texture,
    weight: f32,
) -> RenderScene {
    let mut words = [0.0f32; UNIFORM_WORDS as usize];
    words[0..4].copy_from_slice(&[0.0, 0.0, 1.0, 1.0]);
    words[4..8].copy_from_slice(&[0.0, 0.0, 1.0, 1.0]);
    words[8] = weight;
    words[OPACITY_UNIFORM as usize] = 1.0;
    let uniforms = EffectUniforms::from_words(&words).expect("a 16-word block");

    let mut scene = RenderScene::with_root_node();
    let root = scene.root;
    let child = scene.push(RenderNode {
        parent: Some(root),
        content: RenderContent::Effect(Box::new(render::RenderEffect::new(
            pipeline.clone(),
            vec![first.clone(), second.clone()],
            uniforms,
        ))),
        dst: RectF::new(0.0, 0.0, 1.0, 1.0),
        ..RenderNode::default()
    });
    if let Some(node) = scene.node_mut(root) {
        node.children = vec![child];
    }
    scene
}

/// Uploads one 1x1 texture of `rgba`.
fn one_pixel(pipeline: &RenderPipeline, state: &mut RenderState, rgba: [u8; 4]) -> Texture {
    let texture = create_texture(pipeline, TextureDesc::new(1, 1)).expect("texture");
    upload_texture(
        pipeline,
        state,
        &texture,
        PixelRegion::new(0, 0, 1, 1),
        &rgba,
    )
    .expect("upload");
    texture
}

#[test]
fn the_effect_endpoints_are_the_sources_verbatim_or_skip() {
    let Some((pipeline, _surface, mut state)) = vulkan_state() else {
        return;
    };
    let red = one_pixel(&pipeline, &mut state, [255, 0, 0, 255]);
    let blue = one_pixel(&pipeline, &mut state, [0, 0, 255, 255]);

    for (weight, expected) in [(0.0f32, [255u8, 0, 0, 255]), (1.0, [0, 0, 255, 255])] {
        let effect = create_effect_pipeline(&pipeline, &mut state, &crossfade_desc())
            .expect("effect pipeline");
        let scene = crossfade_scene(&effect, &red, &blue, weight);
        let target =
            create_render_target(&pipeline, render::RenderTargetDesc::new(1, 1)).expect("target");
        render_to_target(&pipeline, &mut state, &target, &scene).expect("render");
        let mut out = [0u8; 4];
        read_render_target(
            &pipeline,
            &state,
            &target,
            PixelRegion::new(0, 0, 1, 1),
            &mut out,
        )
        .expect("readback");
        assert_eq!(
            out, expected,
            "weight {weight}: an endpoint is the source itself"
        );
        destroy_render_target(&pipeline, &mut state, target);
        destroy_effect_pipeline(&pipeline, &mut state, effect);
    }
}

#[test]
fn the_effect_midpoint_mixes_in_byte_space_or_skip() {
    let Some((pipeline, _surface, mut state)) = vulkan_state() else {
        return;
    };
    let red = one_pixel(&pipeline, &mut state, [255, 0, 0, 255]);
    let blue = one_pixel(&pipeline, &mut state, [0, 0, 255, 255]);
    let effect =
        create_effect_pipeline(&pipeline, &mut state, &crossfade_desc()).expect("effect pipeline");
    let scene = crossfade_scene(&effect, &red, &blue, 0.5);
    let target =
        create_render_target(&pipeline, render::RenderTargetDesc::new(1, 1)).expect("target");
    render_to_target(&pipeline, &mut state, &target, &scene).expect("render");
    let mut out = [0u8; 4];
    read_render_target(
        &pipeline,
        &state,
        &target,
        PixelRegion::new(0, 0, 1, 1),
        &mut out,
    )
    .expect("readback");

    let expected_red = expected_mix(255, 0, 0.5);
    let expected_blue = expected_mix(0, 255, 0.5);
    assert!(
        (f32::from(out[0]) - expected_red).abs() <= 1.0,
        "red channel {} should be about {expected_red}",
        out[0]
    );
    assert!(
        (f32::from(out[2]) - expected_blue).abs() <= 1.0,
        "blue channel {} should be about {expected_blue}",
        out[2]
    );
    assert_eq!(out[1], 0, "neither source carries green");
    assert_eq!(out[3], 255, "both sources are opaque");

    destroy_render_target(&pipeline, &mut state, target);
    destroy_effect_pipeline(&pipeline, &mut state, effect);
}
