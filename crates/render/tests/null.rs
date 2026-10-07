//! Integration tests for the headless null backend.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/lib.rs`.

mod common;

use api::geometry::PixelRegion;
use api::host::SurfaceEvent;
use render::{
    NullRenderPipeline, RenderContent, RenderNode, RenderNodeId, RenderPipeline, RenderScene,
    TextureDesc, begin_render_state, create_texture, drop_texture, render, resize, upload_texture,
};

#[test]
fn null_pipeline_round_trip_counts_draws() {
    let pipeline = RenderPipeline::null();
    let (surface, mut state) =
        begin_render_state(&pipeline, &common::headless_window()).expect("begin");

    let event = resize(&pipeline, &surface, &mut state, (1280, 720)).expect("resize");
    assert_eq!(event, SurfaceEvent::Resized(1280, 720));

    let texture = create_texture(&pipeline, TextureDesc::new(4, 2)).expect("texture");
    assert_eq!(texture.desc().base_byte_len(), 4 * 2 * 4);
    let pixels = [255u8; 4 * 2 * 4];
    upload_texture(
        &pipeline,
        &mut state,
        &texture,
        PixelRegion::new(0, 0, 4, 2),
        &pixels,
    )
    .expect("upload");
    assert_eq!(
        NullRenderPipeline::texture_pixels(&state, &texture).map(|p| p.len()),
        Some(32)
    );
    assert_eq!(
        NullRenderPipeline::texture_uploaded_bytes(&state, &texture),
        Some(32)
    );

    let mut scene = RenderScene::new(RenderNodeId(0));
    let root = scene.push(RenderNode {
        content: RenderContent::Texture(texture.clone()),
        ..RenderNode::default()
    });
    let solid = scene.push(RenderNode {
        parent: Some(root),
        content: RenderContent::Solid([0.0, 1.0, 0.0, 1.0]),
        z: 2,
        ..RenderNode::default()
    });
    let hidden = scene.push(RenderNode {
        parent: Some(root),
        content: RenderContent::None,
        visible: false,
        z: 1,
        ..RenderNode::default()
    });
    scene.node_mut(root).expect("root").children = vec![solid, hidden];
    scene.background = Some([0.0, 0.0, 0.0, 1.0]);

    let presentation = render(&pipeline, &surface, &mut state, &scene).expect("render");
    assert!(presentation.presented);
    assert_eq!(presentation.surface_event, None);

    let stats = NullRenderPipeline::last_frame(&state).expect("stats");
    assert_eq!(stats.nodes_visited, 3);
    assert_eq!(stats.draws, 2);
    assert_eq!(stats.texture_draws, 1);
    assert_eq!(stats.solid_draws, 1);
    assert_eq!(stats.invisible, 1);
    assert_eq!(stats.missing_textures, 0);
    assert_eq!(stats.max_depth, 1);

    // Dropping removes the state slot without invalidating clones.
    let handle = texture.clone();
    drop_texture(&pipeline, &mut state, texture);
    assert!(NullRenderPipeline::texture_pixels(&state, &handle).is_none());
}

#[test]
fn partial_upload_touches_only_the_region() {
    let pipeline = RenderPipeline::null();
    let (_surface, mut state) =
        begin_render_state(&pipeline, &common::headless_window()).expect("begin");
    let texture = create_texture(&pipeline, TextureDesc::new(4, 4)).expect("texture");

    let pixels = [7u8; 8];
    upload_texture(
        &pipeline,
        &mut state,
        &texture,
        PixelRegion::new(1, 2, 2, 1),
        &pixels,
    )
    .expect("upload");

    let stored = NullRenderPipeline::texture_pixels(&state, &texture).expect("stored");
    let offset = (2 * 4 + 1) * 4;
    assert_eq!(&stored[offset..offset + 8], &[7u8; 8]);
    assert_eq!(&stored[0..4], &[0u8; 4]);
}
