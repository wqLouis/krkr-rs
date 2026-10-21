//! Tests for the VM-pixel → device-pixel translation and the presentation root
//! transform.
//!
//! The uniform Fit-with-letterbox presentation is the documented extension
//! (D34/D39): the reference presents the VM surface at its own size and has no
//! scaled path, so the scale, offset and readback assertions below describe
//! the extension rather than reference behaviour. The layer placements fed
//! through it stay reference-compatible.

use api::geometry::{PixelRegion, RectF, Transform2D};
use engine::{PixelTranslation, TranslateError};
use render::{
    Clip, PipelineConfig, RenderContent, RenderNode, RenderScene, Texture, TextureDesc,
    create_render_pipeline, create_texture,
};

#[test]
fn fit_scales_up_uniformly_without_a_letterbox() {
    // 1280x720 into 1920x1080 is exactly 1.5x on both axes.
    let translation = PixelTranslation::fit((1280, 720), (1920, 1080)).expect("fit");
    assert_eq!(translation.scale, 1.5);
    assert_eq!(translation.offset, (0.0, 0.0));
    assert_eq!(translation.vm_size, (1280, 720));
    assert_eq!(translation.device_size, (1920, 1080));
    assert_eq!(translation.raster_scale(), 1.5);
    assert!(!translation.is_identity());
}

#[test]
fn fit_letterboxes_a_mismatched_aspect_ratio() {
    // 1280x720 into 1600x1200 is limited by the width: 1.25x leaves 300 device
    // rows, centred as 150 above and 150 below.
    let translation = PixelTranslation::fit((1280, 720), (1600, 1200)).expect("fit");
    assert_eq!(translation.scale, 1.25);
    assert_eq!(translation.offset, (0.0, 150.0));
    assert_eq!(translation.to_device_point((0.0, 0.0)), (0.0, 150.0));
    assert_eq!(
        translation.to_device_point((1280.0, 720.0)),
        (1600.0, 1050.0)
    );
}

#[test]
fn fit_scales_down_when_the_framebuffer_is_smaller() {
    let half = PixelTranslation::fit((1280, 720), (640, 360)).expect("fit");
    assert_eq!(half.scale, 0.5);
    assert_eq!(half.offset, (0.0, 0.0));

    let quarter = PixelTranslation::fit((640, 360), (160, 90)).expect("fit");
    assert_eq!(quarter.scale, 0.25);
    assert_eq!(quarter.to_device_point((320.0, 180.0)), (80.0, 45.0));
}

#[test]
fn fit_rejects_an_empty_dimension() {
    for (vm, device) in [
        ((0, 720), (1920, 1080)),
        ((1280, 0), (1920, 1080)),
        ((1280, 720), (0, 1080)),
        ((1280, 720), (1920, 0)),
    ] {
        let error = PixelTranslation::fit(vm, device).expect_err("an empty size has no fit");
        assert_eq!(
            error,
            TranslateError::EmptyDimension { vm, device },
            "no scale is invented for {vm:?} in {device:?}"
        );
    }
}

#[test]
fn fit_of_equal_sizes_is_the_identity() {
    let identity = PixelTranslation::fit((640, 480), (640, 480)).expect("fit");
    assert!(identity.is_identity());
    assert_eq!(identity.scale, 1.0);
    assert_eq!(identity.offset, (0.0, 0.0));
    assert_eq!(identity.to_device_point((12.0, 34.0)), (12.0, 34.0));
    assert_eq!(identity.to_vm_point((12.0, 34.0)), (12.0, 34.0));
}

#[test]
fn device_and_vm_points_round_trip() {
    let translation = PixelTranslation::fit((1280, 720), (1600, 1200)).expect("fit");
    for point in [(0.0, 0.0), (17.5, 40.25), (640.0, 360.0), (1280.0, 720.0)] {
        let device = translation.to_device_point(point);
        let back = translation.to_vm_point(device);
        assert!(
            (back.0 - point.0).abs() < 1e-3 && (back.1 - point.1).abs() < 1e-3,
            "{point:?} -> {device:?} -> {back:?}"
        );
    }
}

#[test]
fn rects_scale_their_extent_and_offset_their_origin() {
    let translation = PixelTranslation::fit((1280, 720), (1600, 1200)).expect("fit");
    let rect = translation.to_device_rect(RectF::new(10.0, 20.0, 100.0, 50.0));
    assert_eq!(rect, RectF::new(12.5, 175.0, 125.0, 62.5));
}

#[test]
fn vm_pixel_region_is_the_single_pixel_at_identity() {
    let identity = PixelTranslation::fit((640, 480), (640, 480)).expect("fit");
    assert_eq!(identity.vm_pixel_region(0, 0), PixelRegion::new(0, 0, 1, 1));
    assert_eq!(identity.vm_pixel_region(7, 9), PixelRegion::new(7, 9, 1, 1));
}

#[test]
fn vm_pixel_region_covers_every_device_pixel_at_scale_two() {
    let translation = PixelTranslation::fit((100, 100), (200, 200)).expect("fit");
    assert_eq!(
        translation.vm_pixel_region(0, 0),
        PixelRegion::new(0, 0, 2, 2)
    );
    assert_eq!(
        translation.vm_pixel_region(3, 4),
        PixelRegion::new(6, 8, 2, 2)
    );
    // The last VM pixel still ends inside the framebuffer.
    assert_eq!(
        translation.vm_pixel_region(99, 99),
        PixelRegion::new(198, 198, 2, 2)
    );
}

#[test]
fn vm_pixel_region_follows_the_letterbox_offset() {
    // 100x100 into 200x300: scale 2, 50 device rows of margin above and below.
    let translation = PixelTranslation::fit((100, 100), (200, 300)).expect("fit");
    assert_eq!(translation.scale, 2.0);
    assert_eq!(translation.offset, (0.0, 50.0));
    assert_eq!(
        translation.vm_pixel_region(0, 0),
        PixelRegion::new(0, 50, 2, 2)
    );
    assert_eq!(
        translation.vm_pixel_region(10, 20),
        PixelRegion::new(20, 90, 2, 2)
    );
}

#[test]
fn vm_pixel_region_rounds_outwards_at_a_fractional_scale() {
    // One VM pixel covers half a device pixel, so the region is the covering
    // pixel and stays at least one pixel wide.
    let translation = PixelTranslation::fit((1280, 720), (640, 360)).expect("fit");
    assert_eq!(
        translation.vm_pixel_region(3, 5),
        PixelRegion::new(1, 2, 1, 1)
    );
}

/// A scene with one solid node, one clipped node and one masked node.
///
/// Node order is the root, the solid node, then the masked node.
fn scene_with_clips(texture: Texture) -> RenderScene {
    let mut scene = RenderScene::with_root_node();
    let root = scene.root;
    let solid = scene.push(RenderNode {
        parent: Some(root),
        content: RenderContent::Solid([1.0, 0.0, 0.0, 1.0]),
        dst: RectF::new(10.0, 20.0, 100.0, 50.0),
        clip: Some(Clip::Rect(RectF::new(10.0, 10.0, 20.0, 20.0))),
        ..RenderNode::default()
    });
    let masked = scene.push(RenderNode {
        parent: Some(root),
        content: RenderContent::Texture(texture.clone()),
        dst: RectF::new(0.0, 0.0, 8.0, 8.0),
        src: PixelRegion::new(1, 2, 4, 4),
        clip: Some(Clip::Mask(texture)),
        ..RenderNode::default()
    });
    if let Some(root_node) = scene.node_mut(root) {
        root_node.children.push(solid);
        root_node.children.push(masked);
    }
    scene
}

#[test]
fn a_root_transform_maps_the_vm_tree_into_device_pixels() {
    let pipeline = create_render_pipeline(PipelineConfig::null()).expect("null pipeline");
    let texture = create_texture(&pipeline, TextureDesc::new(8, 8)).expect("texture");
    let mut scene = scene_with_clips(texture.clone());
    let translation = PixelTranslation::fit((100, 100), (200, 300)).expect("fit");

    let root = scene.root;
    scene.node_mut(root).expect("root").transform = translation.root_transform();

    let solid = &scene.nodes[1];
    assert_eq!(
        solid.dst,
        RectF::new(10.0, 20.0, 100.0, 50.0),
        "a destination stays in VM pixels"
    );
    assert_eq!(
        solid.clip,
        Some(Clip::Rect(RectF::new(10.0, 10.0, 20.0, 20.0))),
        "a rectangle clip stays in VM pixels"
    );

    let masked = &scene.nodes[2];
    assert_eq!(masked.dst, RectF::new(0.0, 0.0, 8.0, 8.0));
    assert_eq!(
        masked.src,
        PixelRegion::new(1, 2, 4, 4),
        "a source rectangle is texture space"
    );
    assert_eq!(
        masked.clip,
        Some(Clip::Mask(texture)),
        "a mask clip is texture space"
    );

    let device = translation.to_device_rect(solid.dst);
    let origin = scene
        .node(root)
        .expect("root")
        .transform
        .to_affine()
        .transform_point((solid.dst.x, solid.dst.y));
    assert_eq!(
        (origin.0, origin.1),
        (device.x, device.y),
        "the root transform draws the VM rectangle at its device position"
    );
}

#[test]
fn an_identity_root_transform_changes_nothing() {
    let pipeline = create_render_pipeline(PipelineConfig::null()).expect("null pipeline");
    let texture = create_texture(&pipeline, TextureDesc::new(8, 8)).expect("texture");
    let mut scene = scene_with_clips(texture);
    let before = scene.clone();
    let identity = PixelTranslation::fit((100, 100), (100, 100)).expect("fit");

    let root = scene.root;
    scene.node_mut(root).expect("root").transform = identity.root_transform();

    assert_eq!(identity.root_transform(), Transform2D::IDENTITY);
    assert_eq!(identity.inverse_root_transform(), Transform2D::IDENTITY);
    assert_eq!(scene, before);
}

#[test]
fn the_overlay_inverse_cancels_the_root_transform() {
    let translation = PixelTranslation::fit((100, 100), (200, 300)).expect("fit");
    let composed = translation
        .inverse_root_transform()
        .to_affine()
        .then(&translation.root_transform().to_affine());
    for point in [(0.0, 0.0), (10.0, 20.0), (100.0, 100.0)] {
        let drawn = composed.transform_point(point);
        assert!(
            (drawn.0 - point.0).abs() < 1e-3 && (drawn.1 - point.1).abs() < 1e-3,
            "{point:?} authored in device pixels drew at {drawn:?}"
        );
    }
}
