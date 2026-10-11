//! Integration tests for the L2 → L1 render projection.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/projection.rs`. The
//! private helpers used there are reimplemented here on top of the public
//! `render` API.

use api::blend::{BlendMode, DstBlend};
use api::geometry::{PixelRegion, RectF};
use engine::projection::{Presentation, ScalingMode};
use engine::{Rect, Scene, compose_layer, project_scene, project_window, resolve_blend};
use render::{
    PipelineConfig, RenderContent, RenderNode, RenderNodeId, RenderScene, Texture, TextureDesc,
    ViewTransform, create_render_pipeline, create_texture,
};

/// Build a real (headless) texture handle for tests, matching how the
/// caller-owned texture table is populated in production.
fn null_texture(width: u32, height: u32) -> Texture {
    let pipeline = create_render_pipeline(PipelineConfig::null()).expect("null pipeline");
    create_texture(&pipeline, TextureDesc::new(width, height)).expect("null texture")
}

/// All non-container nodes, in arena order.
fn drawable_nodes(scene: &RenderScene) -> Vec<&RenderNode> {
    scene
        .nodes
        .iter()
        .filter(|node| !matches!(node.content, RenderContent::None))
        .collect()
}

fn full_window_rect(w: u32, h: u32) -> Rect {
    Rect::new(0, 0, w, h)
}

/// Attach a `w` x `h` image to `layer`, positioned at the layer's origin and
/// drawn at its full size.
fn attach_image(scene: &mut Scene, layer: u32, w: u32, h: u32) -> u32 {
    let pixels = (w as usize)
        .checked_mul(h as usize)
        .and_then(|count| count.checked_mul(4))
        .expect("test image size");
    let bitmap = scene.add_bitmap(w, h, vec![0u8; pixels]);
    if let Some(state) = scene.layer_mut(layer) {
        state.bitmap = Some(bitmap);
        state.image_width = w;
        state.image_height = h;
    }
    bitmap
}

/// `resolve_blend` passes every generic mode through unchanged at full
/// opacity, so the projection keeps the blend L3 decoded from `Layer.type`.
#[test]
fn resolve_blend_passes_generic_modes_through() {
    let modes = [
        BlendMode::SourceOver,
        BlendMode::Additive,
        BlendMode::Subtractive,
        BlendMode::AddAlpha,
        BlendMode::Multiply,
        BlendMode::Screen,
        BlendMode::DestinationDependent(DstBlend::ColorDodge),
        BlendMode::DestinationDependent(DstBlend::ColorDodge5),
        BlendMode::DestinationDependent(DstBlend::ColorBurn),
        BlendMode::DestinationDependent(DstBlend::Darken),
        BlendMode::DestinationDependent(DstBlend::Lighten),
        BlendMode::DestinationDependent(DstBlend::Difference),
        BlendMode::DestinationDependent(DstBlend::Difference5),
        BlendMode::DestinationDependent(DstBlend::Exclusion),
        BlendMode::DestinationDependent(DstBlend::Overlay),
        BlendMode::DestinationDependent(DstBlend::HardLight),
        BlendMode::DestinationDependent(DstBlend::SoftLight),
    ];
    for mode in modes {
        assert_eq!(resolve_blend(mode, 1.0), mode, "mode {mode:?}");
    }
    assert_eq!(resolve_blend(BlendMode::Replace, 1.0), BlendMode::Replace);
}

#[test]
fn opaque_degrades_below_full_opacity() {
    assert_eq!(resolve_blend(BlendMode::Replace, 1.0), BlendMode::Replace);
    assert_eq!(
        resolve_blend(BlendMode::Replace, 0.5),
        BlendMode::SourceOver
    );
    assert_eq!(
        resolve_blend(BlendMode::SourceOver, 0.5),
        BlendMode::SourceOver
    );
    // Only `Replace` is opacity-sensitive; every other mode passes through.
    assert_eq!(resolve_blend(BlendMode::Multiply, 0.5), BlendMode::Multiply);
    assert_eq!(
        resolve_blend(BlendMode::DestinationDependent(DstBlend::Overlay), 0.5),
        BlendMode::DestinationDependent(DstBlend::Overlay)
    );
}

#[test]
fn compose_layer_accumulates_position_opacity_visibility() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (640, 480));
    scene.window_mut(win).unwrap().opacity = 0.8;
    let parent = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(parent).unwrap();
        l.rect = Rect::new(100, 40, 200, 100);
        l.opacity = 0.5;
        l.visible = true;
    }
    let child = scene.add_layer(win, Some(parent));
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(12, 8, 20, 10);
        l.opacity = 0.5;
        l.visible = true;
    }

    let composed = compose_layer(&scene, child).expect("composed");
    assert_eq!(composed.rect, Rect::new(112, 48, 20, 10));
    assert!((composed.opacity - 0.25).abs() < 1e-6);
    assert!(composed.visible);
}

#[test]
fn compose_layer_honours_ancestor_invisibility() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let parent = scene.add_layer(win, None);
    scene.layer_mut(parent).unwrap().visible = false;
    let child = scene.add_layer(win, Some(parent));
    scene.layer_mut(child).unwrap().visible = true;

    let composed = compose_layer(&scene, child).expect("composed");
    assert!(!composed.visible, "an invisible ancestor hides the child");
}

/// Parent is centered at TVP (200, 90); the child is relative to its
/// parent at origin (112, 48) with its own 20×10 rect. The parent, child
/// and sibling get z = 0/1/2.
#[test]
fn hierarchy_composes_position_opacity_visibility_and_order() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (640, 480));
    scene.window_mut(win).unwrap().opacity = 0.8;
    let parent = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(parent).unwrap();
        l.rect = Rect::new(100, 40, 200, 100);
        l.opacity = 0.5;
        l.visible = true;
    }
    attach_image(&mut scene, parent, 200, 100);
    let child = scene.add_layer(win, Some(parent));
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(12, 8, 20, 10);
        l.opacity = 0.5;
        l.visible = true;
    }
    attach_image(&mut scene, child, 20, 10);
    let sibling = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(sibling).unwrap();
        l.rect = Rect::new(1, 2, 3, 4);
        l.visible = true;
    }
    attach_image(&mut scene, sibling, 3, 4);

    let projected = project_window(&scene, win, ViewTransform::IDENTITY, |_| {
        Some(null_texture(1, 1))
    });
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 3);

    let parent_node = nodes
        .iter()
        .find(|n| n.dst == RectF::new(100.0, 40.0, 200.0, 100.0))
        .expect("parent node");
    let child_node = nodes
        .iter()
        .find(|n| n.dst == RectF::new(112.0, 48.0, 20.0, 10.0))
        .expect("child node");
    let sibling_node = nodes
        .iter()
        .find(|n| n.dst == RectF::new(1.0, 2.0, 3.0, 4.0))
        .expect("sibling node");

    assert_eq!(parent_node.z, 0);
    assert_eq!(child_node.z, 1);
    assert_eq!(sibling_node.z, 2);
    assert!(parent_node.z < child_node.z);
    assert!(child_node.z < sibling_node.z);

    // Child links to the drawn parent; parent/sibling link to the root.
    assert_eq!(parent_node.parent, Some(projected.root));
    assert_eq!(sibling_node.parent, Some(projected.root));
    assert_eq!(child_node.parent, Some(RenderNodeId(1)));
    assert!(projected.nodes[1].children.contains(&RenderNodeId(2)));

    // Composed opacity: 0.8 × 0.5 and 0.8 × 0.5 × 0.5.
    assert!((parent_node.opacity - 0.4).abs() < 1e-6);
    assert!((child_node.opacity - 0.2).abs() < 1e-6);
    assert!((sibling_node.opacity - 0.8).abs() < 1e-6);
}

#[test]
fn window_opacity_scales_layer_opacity() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    scene.window_mut(win).unwrap().opacity = 0.5;
    let fill = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(fill).unwrap();
        l.rect = full_window_rect(64, 64);
        l.opacity = 0.5;
    }
    attach_image(&mut scene, fill, 64, 64);

    let projected = project_window(&scene, win, ViewTransform::IDENTITY, |_| {
        Some(null_texture(1, 1))
    });
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1);
    assert!((nodes[0].opacity - 0.25).abs() < 1e-6);
}

#[test]
fn bitmap_layer_uses_texture_and_source_region() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1280, 720));
    let bmp_id = scene.add_bitmap(251, 254, vec![0u8; 251 * 254 * 4]);
    let layer = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(layer).unwrap();
        l.bitmap = Some(bmp_id);
        l.rect = Rect::new(297, 436, 251, 254);
        l.image_width = 251;
        l.image_height = 254;
    }

    let projected = project_window(&scene, win, ViewTransform::IDENTITY, |_| {
        Some(null_texture(251, 254))
    });
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1);
    assert!(matches!(nodes[0].content, RenderContent::Texture(_)));
    assert_eq!(nodes[0].dst, RectF::new(297.0, 436.0, 251.0, 254.0));
    assert_eq!(nodes[0].src, PixelRegion::new(0, 0, 251, 254));
}

#[test]
fn bitmap_layer_1x1_rect_stays_1px() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1280, 720));
    let bmp_id = scene.add_bitmap(64, 64, vec![0u8; 64 * 64 * 4]);
    let layer = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(layer).unwrap();
        l.bitmap = Some(bmp_id);
        l.rect = Rect::new(10, 10, 1, 1);
    }

    let projected = project_window(&scene, win, ViewTransform::IDENTITY, |_| {
        Some(null_texture(64, 64))
    });
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1);
    assert_eq!(nodes[0].dst, RectF::new(10.0, 10.0, 1.0, 1.0));
    assert_eq!(nodes[0].src, PixelRegion::new(0, 0, 1, 1));
}

#[test]
fn missing_bitmap_without_fill_draws_nothing() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let layer = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(layer).unwrap();
        l.bitmap = Some(9999); // never added
        l.rect = full_window_rect(64, 64);
    }

    let projected = project_window(&scene, win, ViewTransform::IDENTITY, |_| None);
    assert!(drawable_nodes(&projected).is_empty());
}

#[test]
fn empty_layer_is_skipped_but_children_render() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let parent = scene.add_layer(win, None);
    {
        // No bitmap: the layer draws nothing itself.
        let l = scene.layer_mut(parent).unwrap();
        l.rect = full_window_rect(64, 64);
    }
    let child = scene.add_layer(win, Some(parent));
    {
        let l = scene.layer_mut(child).unwrap();
        l.visible = true;
        l.rect = Rect::new(0, 0, 8, 8);
    }
    attach_image(&mut scene, child, 8, 8);

    let projected = project_window(&scene, win, ViewTransform::IDENTITY, |_| {
        Some(null_texture(1, 1))
    });
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1, "the empty layer is not a node");
    assert_eq!(nodes[0].dst, RectF::new(0.0, 0.0, 8.0, 8.0));
    // The parent has no node, so the child links to the root.
    assert_eq!(nodes[0].parent, Some(projected.root));
}

#[test]
fn invisible_layer_is_not_drawn() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let layer = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(layer).unwrap();
        l.visible = false;
        l.rect = full_window_rect(64, 64);
    }
    attach_image(&mut scene, layer, 64, 64);

    let projected = project_window(&scene, win, ViewTransform::IDENTITY, |_| {
        Some(null_texture(1, 1))
    });
    assert!(drawable_nodes(&projected).is_empty());
}

#[test]
fn hidden_window_projects_nothing() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    scene.window_mut(win).unwrap().visible = false;
    let layer = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(layer).unwrap();
        l.rect = full_window_rect(64, 64);
    }
    attach_image(&mut scene, layer, 64, 64);

    let projected = project_window(&scene, win, ViewTransform::IDENTITY, |_| {
        Some(null_texture(1, 1))
    });
    assert!(projected.nodes.iter().all(|n| n.children.is_empty()));
}

#[test]
fn project_scene_shares_z_across_windows() {
    let mut scene = Scene::default();
    let first = scene.add_window("a", (64, 64));
    let second = scene.add_window("b", (64, 64));
    for (window, x) in [(first, 0), (second, 32)] {
        let layer = scene.add_layer(window, None);
        scene.layer_mut(layer).unwrap().rect = Rect::new(x, 0, 16, 16);
        attach_image(&mut scene, layer, 16, 16);
    }

    let projected = project_scene(&scene, ViewTransform::IDENTITY, |_| {
        Some(null_texture(1, 1))
    });
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0].z, 0);
    assert_eq!(nodes[1].z, 1);
}

#[test]
fn hold_alpha_is_carried_through() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let layer = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(layer).unwrap();
        l.hold_alpha = true;
        l.rect = full_window_rect(64, 64);
    }
    attach_image(&mut scene, layer, 64, 64);

    let projected = project_window(&scene, win, ViewTransform::IDENTITY, |_| {
        Some(null_texture(1, 1))
    });
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1);
    assert!(nodes[0].hold_alpha);
}

/// Visibility is inherited (`GetNodeVisible = parent_visible && visible`), so
/// an invisible parent hides its whole subtree.
#[test]
fn invisible_parent_hides_its_subtree() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let grandparent = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(grandparent).unwrap();
        l.rect = Rect::new(1, 1, 4, 4);
    }
    attach_image(&mut scene, grandparent, 4, 4);
    let invisible = scene.add_layer(win, Some(grandparent));
    scene.layer_mut(invisible).unwrap().visible = false;
    let child = scene.add_layer(win, Some(invisible));
    {
        let l = scene.layer_mut(child).unwrap();
        l.visible = true;
        l.rect = Rect::new(2, 2, 3, 3);
    }
    attach_image(&mut scene, child, 3, 3);

    let projected = project_window(&scene, win, ViewTransform::IDENTITY, |_| {
        Some(null_texture(1, 1))
    });
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1, "an invisible parent hides its subtree");
    assert_eq!(nodes[0].dst, RectF::new(1.0, 1.0, 4.0, 4.0));
}

/// A layer with an enabled and rendered subtree cache draws that cache and
/// its descendants are not emitted separately: the cache already contains
/// them (`InternalComplete2` composites the subtree into `CacheBitmap`,
/// `LayerIntf.cpp:7156-7226`; `Complete` hands it to the parent,
/// `:7673-7755`).
#[test]
fn cached_layer_draws_its_bitmap_and_hides_its_subtree() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let parent = scene.add_layer(win, None);
    let cache = scene.add_bitmap(64, 64, vec![0u8; 64 * 64 * 4]);
    {
        let l = scene.layer_mut(parent).unwrap();
        l.rect = full_window_rect(64, 64);
        l.visible = true;
        l.cache_enabled_count = 1;
        l.cache_bitmap = Some(cache);
    }
    let child = scene.add_layer(win, Some(parent));
    {
        let l = scene.layer_mut(child).unwrap();
        l.visible = true;
        l.rect = Rect::new(0, 0, 8, 8);
    }
    attach_image(&mut scene, child, 8, 8);

    let projected = project_window(&scene, win, ViewTransform::IDENTITY, |id| {
        if id == cache {
            Some(null_texture(64, 64))
        } else {
            None
        }
    });
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1, "only the cached layer draws");
    assert!(matches!(nodes[0].content, RenderContent::Texture(_)));
    assert_eq!(nodes[0].dst, RectF::new(0.0, 0.0, 64.0, 64.0));
}

/// A 1280x720 game, the design size every presentation test uses.
const DESIGN: (u32, u32) = (1280, 720);

/// Compare a floating-point component within a small tolerance.
fn assert_close(actual: f32, expected: f32) {
    assert!(
        (actual - expected).abs() <= 1e-5,
        "expected {expected}, got {actual}"
    );
}

/// `Game` is the untransformed Win32 presentation: the window client area is
/// the logical screen and one logical pixel is one framebuffer pixel
/// (`reference/cpp/core/base/impl/SystemImpl.cpp:877-895`,
/// `reference/cpp/core/visual/WindowIntf.cpp:1521-1547`).
#[test]
fn game_mode_is_the_untransformed_presentation() {
    for framebuffer in [(2560, 1440), (1920, 1080), (1600, 1200), (1920, 800)] {
        let presentation = Presentation::compute(ScalingMode::Game, DESIGN, framebuffer);
        assert_eq!(
            presentation.view,
            ViewTransform::IDENTITY,
            "{framebuffer:?}"
        );
        assert_eq!(presentation.logical_size, framebuffer, "{framebuffer:?}");
    }
}

/// cocos2d `SHOW_ALL` scales by the smaller axis ratio and centres the
/// viewport, leaving a letterbox on the free axis
/// (`reference/cpp/core/environ/cocos2d/AppDelegate.cpp:73-75`).
#[test]
fn fit_mode_letterboxes_uniformly() {
    // Same aspect as the design: no letterbox.
    let presentation = Presentation::compute(ScalingMode::Fit, DESIGN, (2560, 1440));
    assert_close(presentation.view.scale.0, 2.0);
    assert_close(presentation.view.scale.1, 2.0);
    assert_close(presentation.view.offset.0, 0.0);
    assert_close(presentation.view.offset.1, 0.0);
    assert_eq!(presentation.logical_size, DESIGN);

    let presentation = Presentation::compute(ScalingMode::Fit, DESIGN, (1920, 1080));
    assert_close(presentation.view.scale.0, 1.5);
    assert_close(presentation.view.scale.1, 1.5);
    assert_close(presentation.view.offset.0, 0.0);
    assert_close(presentation.view.offset.1, 0.0);
    assert_eq!(presentation.logical_size, DESIGN);

    // Taller window: the width fits, so the letterbox is vertical.
    let presentation = Presentation::compute(ScalingMode::Fit, DESIGN, (1600, 1200));
    assert_close(presentation.view.scale.0, 1.25);
    assert_close(presentation.view.scale.1, 1.25);
    assert_close(presentation.view.offset.0, 0.0);
    assert_close(presentation.view.offset.1, 150.0);
    assert_eq!(presentation.logical_size, DESIGN);

    // Wider window: the height fits, so the letterbox is horizontal.
    let presentation = Presentation::compute(ScalingMode::Fit, DESIGN, (1920, 800));
    assert_close(presentation.view.scale.0, 800.0 / 720.0);
    assert_close(presentation.view.scale.1, 800.0 / 720.0);
    assert_close(presentation.view.offset.0, 2240.0 / 9.0);
    assert_close(presentation.view.offset.1, 0.0);
    assert_eq!(presentation.logical_size, DESIGN);
}

/// cocos2d `EXACT_FIT` scales the two axes independently and covers the whole
/// frame with no offset
/// (`reference/cpp/core/environ/cocos2d/AppDelegate.cpp:70`).
#[test]
fn fill_mode_scales_each_axis_to_the_window() {
    let cases = [
        ((2560, 1440), (2.0, 2.0)),
        ((1920, 1080), (1.5, 1.5)),
        ((1600, 1200), (1.25, 5.0 / 3.0)),
        ((1920, 800), (1.5, 10.0 / 9.0)),
    ];
    for (framebuffer, (scale_x, scale_y)) in cases {
        let presentation = Presentation::compute(ScalingMode::Fill, DESIGN, framebuffer);
        assert_close(presentation.view.scale.0, scale_x);
        assert_close(presentation.view.scale.1, scale_y);
        assert_close(presentation.view.offset.0, 0.0);
        assert_close(presentation.view.offset.1, 0.0);
        assert_eq!(presentation.logical_size, DESIGN);
    }
}

/// cocos2d `MainScene::initialize` keeps the design height and rewrites the
/// width to the frame's aspect, so the uniform height scale fills exactly
/// (`reference/cpp/core/environ/cocos2d/MainScene.cpp:436-441`).
#[test]
fn expand_mode_grows_the_logical_size_to_the_window_aspect() {
    let cases = [
        ((2560, 1440), (1280, 720), 2.0),
        ((1920, 1080), (1280, 720), 1.5),
        ((1600, 1200), (960, 720), 5.0 / 3.0),
        ((1920, 800), (1728, 720), 10.0 / 9.0),
    ];
    for (framebuffer, logical_size, scale) in cases {
        let presentation = Presentation::compute(ScalingMode::Expand, DESIGN, framebuffer);
        assert_eq!(presentation.logical_size, logical_size, "{framebuffer:?}");
        assert_close(presentation.view.scale.0, scale);
        assert_close(presentation.view.scale.1, scale);
        assert_close(presentation.view.offset.0, 0.0);
        assert_close(presentation.view.offset.1, 0.0);
    }
}

/// A zero window dimension is degenerate: the presentation stays the identity
/// design projection and never divides by zero.
#[test]
fn degenerate_windows_keep_the_identity() {
    let modes = [
        ScalingMode::Game,
        ScalingMode::Fit,
        ScalingMode::Fill,
        ScalingMode::Expand,
    ];
    for mode in modes {
        for framebuffer in [(0, 0), (0, 720), (1280, 0)] {
            let presentation = Presentation::compute(mode, DESIGN, framebuffer);
            assert_eq!(
                presentation.view,
                ViewTransform::IDENTITY,
                "{mode:?} {framebuffer:?}"
            );
            assert_eq!(
                presentation.logical_size, DESIGN,
                "{mode:?} {framebuffer:?}"
            );
        }
        let presentation = Presentation::compute(mode, (0, 0), (1920, 1080));
        assert_eq!(presentation.view, ViewTransform::IDENTITY, "{mode:?}");
        assert_eq!(presentation.logical_size, (0, 0), "{mode:?}");
    }
}

/// The projected tree carries the presentation transform to the pipeline.
#[test]
fn projection_carries_the_presentation_view() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let _ = scene.add_layer(win, None);
    let view = Presentation::compute(ScalingMode::Fit, DESIGN, (2560, 1440)).view;
    let projected = project_window(&scene, win, view, |_| None);
    assert_eq!(projected.view, view);
}
