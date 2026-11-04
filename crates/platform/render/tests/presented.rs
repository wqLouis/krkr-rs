//! The null backend's CPU-presented frame.
//!
//! The presented frame is the picture the window shows when the null backend
//! presents it: the software rasterizer composites the presented scene into
//! it. The tests drive the public free functions only.

use api::geometry::{PixelRegion, RectF};
use render::{
    PresentedFrameSlot, RenderContent, RenderNode, RenderPipeline, RenderScene, RenderTargetDesc,
    TextureDesc, begin_render_state, create_render_target, create_texture,
    install_presented_frame_slot, read_render_target, render, render_to_target, resize,
    upload_texture,
};
use testkit::headless_window;

/// A 4×4 scene with an opaque red solid over the top-left 2×2 corner.
fn scene_with_solid() -> RenderScene {
    let mut scene = RenderScene::with_root_node();
    let root = scene.root;
    let child = scene.push(RenderNode {
        parent: Some(root),
        content: RenderContent::Solid([1.0, 0.0, 0.0, 1.0]),
        dst: RectF::new(0.0, 0.0, 2.0, 2.0),
        ..RenderNode::default()
    });
    scene.node_mut(root).expect("root").children = vec![child];
    scene.background = Some([0.0, 0.0, 0.0, 1.0]);
    scene
}

/// The pixel at `(x, y)` of a `width`-wide RGBA8 frame.
fn pixel(frame: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let offset = ((y * width + x) * 4) as usize;
    frame[offset..offset + 4].try_into().expect("a pixel")
}

/// The presented frame holds the composited picture.
///
/// The frame is the size of the surface, so it holds the picture in device
/// pixels, and the solid covers the corner it was given.
#[test]
fn the_presented_frame_holds_the_composited_picture() {
    let pipeline = RenderPipeline::null();
    let (surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    resize(&pipeline, &surface, &mut state, (4, 4)).expect("resize");

    let slot = PresentedFrameSlot::new();
    install_presented_frame_slot(&pipeline, &mut state, slot.clone()).expect("install");

    render(&pipeline, &surface, &mut state, &scene_with_solid()).expect("render");

    let frame = slot.with_frame(|frame| frame.clone()).expect("a frame");
    assert_eq!((frame.width, frame.height), (4, 4));
    assert_eq!(pixel(&frame.pixels, 4, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&frame.pixels, 4, 1, 1), [255, 0, 0, 255]);
    // The rest of the frame is the cleared background.
    assert_eq!(pixel(&frame.pixels, 4, 2, 0), [0, 0, 0, 255]);
    assert_eq!(pixel(&frame.pixels, 4, 0, 2), [0, 0, 0, 255]);
}

/// The presented frame and the offscreen target hold the same picture.
///
/// Both are composited by the software rasterizer, so the frame the window
/// shows and the frame a readback returns are the same bytes.
#[test]
fn the_presented_frame_matches_the_offscreen_readback() {
    let pipeline = RenderPipeline::null();
    let (surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    resize(&pipeline, &surface, &mut state, (4, 4)).expect("resize");

    let slot = PresentedFrameSlot::new();
    install_presented_frame_slot(&pipeline, &mut state, slot.clone()).expect("install");

    let texture = create_texture(&pipeline, TextureDesc::new(2, 2)).expect("texture");
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
        dst: RectF::new(0.0, 0.0, 4.0, 4.0),
        ..RenderNode::default()
    });
    scene.node_mut(root).expect("root").children = vec![child];
    scene.background = Some([0.0, 0.0, 0.0, 1.0]);

    render(&pipeline, &surface, &mut state, &scene).expect("render");

    let target = create_render_target(&pipeline, RenderTargetDesc::new(4, 4)).expect("target");
    render_to_target(&pipeline, &mut state, &target, &scene).expect("render to target");
    let mut readback = vec![0u8; 4 * 4 * 4];
    read_render_target(
        &pipeline,
        &state,
        &target,
        PixelRegion::new(0, 0, 4, 4),
        &mut readback,
    )
    .expect("read");

    let frame = slot.with_frame(|frame| frame.clone()).expect("a frame");
    assert_eq!(frame.pixels, readback);
    // The textured quad covers the whole frame.
    assert_eq!(pixel(&frame.pixels, 4, 0, 0), [0, 255, 0, 255]);
    assert_eq!(pixel(&frame.pixels, 4, 3, 3), [0, 255, 0, 255]);
}

/// The presented frame follows the surface size.
///
/// A window can be resized, and the frame the window shows is the size of
/// the surface, so the frame follows the resize.
#[test]
fn the_presented_frame_follows_the_surface_size() {
    let pipeline = RenderPipeline::null();
    let (surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    resize(&pipeline, &surface, &mut state, (4, 4)).expect("resize");

    let slot = PresentedFrameSlot::new();
    install_presented_frame_slot(&pipeline, &mut state, slot.clone()).expect("install");

    render(&pipeline, &surface, &mut state, &scene_with_solid()).expect("render");
    let frame = slot.with_frame(|frame| frame.clone()).expect("a frame");
    assert_eq!((frame.width, frame.height), (4, 4));

    resize(&pipeline, &surface, &mut state, (8, 6)).expect("resize");
    render(&pipeline, &surface, &mut state, &scene_with_solid()).expect("render");
    let frame = slot.with_frame(|frame| frame.clone()).expect("a frame");
    assert_eq!((frame.width, frame.height), (8, 6));
    assert_eq!(frame.pixels.len(), 8 * 6 * 4);
    // The solid still covers the corner it was given.
    assert_eq!(pixel(&frame.pixels, 8, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&frame.pixels, 8, 2, 0), [0, 0, 0, 255]);
}
