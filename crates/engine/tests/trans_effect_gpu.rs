//! The generic effect path against the CPU blend (needs a device).
//!
//! The draw the generic path makes is checked against the CPU blend the
//! reference's arithmetic produces (`reference/cpp/core/visual/TransIntf.cpp:592-600`):
//! the cross-fade at several phases, with an opaque and with a translucent
//! second source, and the scroll's placements. The cases that need a device
//! skip cleanly when the host has none.
//!
//! The comparison is between the *composited* results: the CPU blend writes
//! straight-alpha pixels, while the renderer draws the frame over the scene,
//! and its target is an sRGB surface, so the compositor blends in linear
//! light. The reference below is the oracle's pixel put through that same
//! composition.

use api::blend::BlendMode;
use api::geometry::PixelRegion;
use engine::shader::Program;
use engine::shader::builtin;
use engine::trans::{ScrollFrom, ScrollStay, Trans};
use engine::{BitmapState, Map, Rect, Scene, project_window_report};
use render::{
    EffectPipeline, PipelineConfig, RenderContent, RenderPipeline, RenderScene, RenderState,
    RenderTargetDesc, Surface, Texture, TextureDesc, begin_render_state, create_effect_pipeline,
    create_render_pipeline, create_render_target, create_texture, destroy_render_target,
    read_render_target, render_to_target, upload_texture,
};
use testkit::headless_window;

/// The Vulkan pipeline, its surface and its render state.
struct Device {
    pipeline: RenderPipeline,
    /// Kept alive for the state it belongs to.
    _surface: Surface,
    state: RenderState,
}

/// The device the effect draw is drawn with, or `None` when the host has
/// neither a device nor a surface.
fn vulkan_device() -> Option<Device> {
    let pipeline = create_render_pipeline(PipelineConfig::vulkan()).ok()?;
    let (surface, state) = begin_render_state(&pipeline, &headless_window()).ok()?;
    Some(Device {
        pipeline,
        _surface: surface,
        state,
    })
}

/// The pipeline of one program, created on the device.
fn program_pipeline(device: &mut Device, program: Program) -> Option<EffectPipeline> {
    let Device {
        pipeline, state, ..
    } = device;
    let desc = match program {
        Program::CrossFade => builtin::crossfade(),
        Program::Scroll => builtin::scroll(),
        _ => return None,
    };
    let desc = desc.pipeline_desc(BlendMode::SourceOver);
    create_effect_pipeline(pipeline, state, &desc).ok()
}

/// A one-layer window whose layer carries the transition's two sources.
///
/// The returned ids are the two source bitmaps.
fn transition_scene(
    trans: &Trans,
    src1: Vec<u8>,
    src2: Vec<u8>,
    width: u32,
    height: u32,
    device: &mut Device,
    program: Program,
) -> (Scene, u32, u32, u32) {
    let mut scene = Scene::default();
    let window = scene.add_window("t", (width, height));
    let layer = scene.add_layer(window, None);
    {
        let state = scene.layer_mut(layer).expect("layer");
        state.rect = Rect::new(0, 0, width, height);
        state.visible = true;
    }
    let first = scene.add_bitmap(width, height, src1);
    let second = scene.add_bitmap(width, height, src2);
    let draw = trans
        .effect_draw(first, second, (width, height), true)
        .expect("effect draw");
    scene.set_effect(layer, Some(draw));
    if let Some(pipeline) = program_pipeline(device, program) {
        scene.effect_pipelines.insert(program, pipeline);
    }
    (scene, window, first, second)
}

/// The projected scene, with every bitmap uploaded as a texture.
fn project(
    scene: &Scene,
    window: u32,
    pipeline: &RenderPipeline,
    state: &mut RenderState,
) -> RenderScene {
    let mut textures: Map<u32, Texture> = Map::default();
    for bitmap in &scene.bitmaps {
        let Ok(texture) = create_texture(pipeline, TextureDesc::new(bitmap.width, bitmap.height))
        else {
            continue;
        };
        if upload_texture(
            pipeline,
            state,
            &texture,
            PixelRegion::new(0, 0, bitmap.width, bitmap.height),
            &bitmap.rgba,
        )
        .is_err()
        {
            continue;
        }
        textures.insert(bitmap.id, texture);
    }
    project_window_report(scene, window, |id| textures.get(&id).cloned()).scene
}

/// The frame the renderer draws for the scene, read back from an offscreen
/// target.
fn composite(
    scene: &Scene,
    window: u32,
    pipeline: &RenderPipeline,
    state: &mut RenderState,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let report = project(scene, window, pipeline, state);
    let target =
        create_render_target(pipeline, RenderTargetDesc::new(width, height)).expect("target");
    render_to_target(pipeline, state, &target, &report).expect("render");
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    read_render_target(
        pipeline,
        state,
        &target,
        PixelRegion::new(0, 0, width, height),
        &mut pixels,
    )
    .expect("read");
    destroy_render_target(pipeline, state, target);
    pixels
}

/// The CPU blend's result for the same frame: the oracle the draw reproduces.
fn cpu_blend(
    trans: &Trans,
    scene: &Scene,
    first: u32,
    second: u32,
    width: u32,
    height: u32,
) -> BitmapState {
    let source = scene.bitmap(first).expect("first source").clone();
    let other = scene.bitmap(second).expect("second source").clone();
    let mut cpu = BitmapState::new(width, height, vec![0u8; (width * height * 4) as usize]);
    trans
        .apply(
            &mut cpu,
            &source,
            &other,
            (0, 0, width as i32, height as i32),
        )
        .expect("cpu blend");
    cpu
}

/// Decodes one encoded channel to linear light, the value an sRGB texture
/// hands a shader.
fn srgb_to_linear(channel: f32) -> f32 {
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

/// Encodes one linear-light channel the way an sRGB surface stores it.
fn linear_to_srgb(channel: f32) -> f32 {
    if channel <= 0.003_130_8 {
        channel * 12.92
    } else {
        1.055 * channel.powf(1.0 / 2.4) - 0.055
    }
}

/// The background the projection clears the frame with.
const BACKGROUND: [u8; 4] = [0, 0, 0, 255];

/// The pixel the renderer produces for the CPU blend's straight-alpha result.
///
/// The render target is an sRGB surface, so the compositor blends in linear
/// light: the oracle's byte values are decoded, premultiplied by the alpha
/// and encoded again. An opaque source replaces the background, so the result
/// is the oracle's own bytes.
fn composited(oracle: [u8; 4], background: [u8; 4]) -> [f32; 4] {
    let alpha = f32::from(oracle[3]) / 255.0;
    let mut out = [0.0f32; 4];
    for channel in 0..3 {
        let src = srgb_to_linear(f32::from(oracle[channel]) / 255.0);
        let dst = srgb_to_linear(f32::from(background[channel]) / 255.0);
        out[channel] = src * alpha + dst * (1.0 - alpha);
    }
    out[3] = alpha + f32::from(background[3]) / 255.0 * (1.0 - alpha);
    out
}

/// Asserts that the drawn frame matches the CPU blend's composited result.
///
/// The bound is the blend's own rounding: the CPU blend truncates the
/// per-channel difference, the program rounds it, and the two differ by at
/// most one level before the composition carries it.
fn assert_matches_cpu(drawn: &[u8], cpu: &BitmapState, bound: f32, what: &str) {
    for (index, (got, expected)) in drawn
        .as_chunks::<4>()
        .0
        .iter()
        .zip(cpu.rgba.as_chunks::<4>().0.iter())
        .enumerate()
    {
        let reference = composited(
            [expected[0], expected[1], expected[2], expected[3]],
            BACKGROUND,
        );
        for channel in 0..3 {
            let want = linear_to_srgb(reference[channel]) * 255.0;
            let difference = (f32::from(got[channel]) - want).abs();
            assert!(
                difference <= bound,
                "{what}: pixel {index}, channel {channel}: drawn {} vs cpu blend {want}",
                got[channel]
            );
        }
        let want_alpha = reference[3] * 255.0;
        assert!(
            (f32::from(got[3]) - want_alpha).abs() <= bound,
            "{what}: pixel {index}: alpha {} vs cpu blend {want_alpha}",
            got[3]
        );
    }
}

/// The cross-fade the generic path draws matches the CPU blend at every
/// phase: the two blend the same two sources at the same weight.
#[test]
fn the_cross_fade_matches_the_cpu_blend() {
    let Some(mut device) = vulkan_device() else {
        return;
    };
    let width = 4u32;
    let height = 4u32;
    let src1: Vec<u8> = [90u8, 120, 150, 255].repeat((width * height) as usize);
    let src2: Vec<u8> = [180u8, 60, 30, 255].repeat((width * height) as usize);
    for step_ms in [0.0, 20.0, 40.0, 60.0, 80.0, 100.0] {
        let mut trans = Trans::cross_fade(100.0).expect("crossfade");
        trans.step(step_ms);
        let (scene, window, first, second) = transition_scene(
            &trans,
            src1.clone(),
            src2.clone(),
            width,
            height,
            &mut device,
            Program::CrossFade,
        );
        let drawn = composite(
            &scene,
            window,
            &device.pipeline,
            &mut device.state,
            width,
            height,
        );
        let cpu = cpu_blend(&trans, &scene, first, second, width, height);
        assert_matches_cpu(&drawn, &cpu, 1.0, &format!("cross-fade at {step_ms} ms"));
    }
}

/// Where the second source carries alpha, the blend still matches: the
/// composition over the scene is the one the renderer performs, and the
/// blend's own rounding is the only difference.
#[test]
fn a_translucent_second_source_matches_the_cpu_blend() {
    let Some(mut device) = vulkan_device() else {
        return;
    };
    let width = 2u32;
    let height = 1u32;
    let src1: Vec<u8> = [90u8, 120, 150, 255].repeat((width * height) as usize);
    let src2: Vec<u8> = [180u8, 60, 30, 128].repeat((width * height) as usize);
    for step_ms in [20.0, 50.0, 80.0] {
        let mut trans = Trans::cross_fade(100.0).expect("crossfade");
        trans.step(step_ms);
        let (scene, window, first, second) = transition_scene(
            &trans,
            src1.clone(),
            src2.clone(),
            width,
            height,
            &mut device,
            Program::CrossFade,
        );
        let drawn = composite(
            &scene,
            window,
            &device.pipeline,
            &mut device.state,
            width,
            height,
        );
        let cpu = cpu_blend(&trans, &scene, first, second, width, height);
        assert_matches_cpu(&drawn, &cpu, 2.0, &format!("translucent at {step_ms} ms"));
    }
}

/// The scroll the generic path draws reproduces the CPU blit: the two
/// sources are copied at their placed rectangles with no blend.
#[test]
fn the_scroll_matches_the_cpu_blit() {
    let Some(mut device) = vulkan_device() else {
        return;
    };
    let width = 4u32;
    let height = 2u32;
    let src1: Vec<u8> = [90u8, 120, 150, 255].repeat((width * height) as usize);
    let src2: Vec<u8> = [180u8, 60, 30, 255].repeat((width * height) as usize);
    for step_ms in [0.0, 50.0, 100.0] {
        for from in [ScrollFrom::Left, ScrollFrom::Top] {
            let mut trans = Trans::scroll(100.0, from, ScrollStay::NoStay).expect("scroll");
            trans.step(step_ms);
            let (scene, window, first, second) = transition_scene(
                &trans,
                src1.clone(),
                src2.clone(),
                width,
                height,
                &mut device,
                Program::Scroll,
            );
            let drawn = composite(
                &scene,
                window,
                &device.pipeline,
                &mut device.state,
                width,
                height,
            );
            let cpu = cpu_blend(&trans, &scene, first, second, width, height);
            assert_matches_cpu(&drawn, &cpu, 1.0, &format!("scroll at {step_ms} ms"));
        }
    }
}

/// The projection turns the draw into one node per pass.
#[test]
fn the_projection_draws_one_node_per_pass() {
    let Some(mut device) = vulkan_device() else {
        return;
    };
    let width = 4u32;
    let height = 4u32;
    let src: Vec<u8> = vec![0u8; (width * height * 4) as usize];

    let mut trans = Trans::cross_fade(100.0).expect("crossfade");
    trans.step(50.0);
    let (scene, window, _first, _second) = transition_scene(
        &trans,
        src.clone(),
        src.clone(),
        width,
        height,
        &mut device,
        Program::CrossFade,
    );
    let report = project(&scene, window, &device.pipeline, &mut device.state);
    let effects = report
        .nodes
        .iter()
        .filter(|node| matches!(node.content, RenderContent::Effect(_)))
        .count();
    assert_eq!(effects, 1, "the cross-fade is one blend");

    let mut trans = Trans::scroll(100.0, ScrollFrom::Left, ScrollStay::NoStay).expect("scroll");
    trans.step(50.0);
    let (scene, window, _first, _second) = transition_scene(
        &trans,
        src.clone(),
        src.clone(),
        width,
        height,
        &mut device,
        Program::Scroll,
    );
    let report = project(&scene, window, &device.pipeline, &mut device.state);
    let effects = report
        .nodes
        .iter()
        .filter(|node| matches!(node.content, RenderContent::Effect(_)))
        .count();
    assert_eq!(effects, 2, "the scroll places two sources");
}
