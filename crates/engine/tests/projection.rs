//! Integration tests for the L2 → L1 render projection.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/projection.rs`. The
//! private helpers used there are reimplemented here on top of the public
//! `render` API.

use api::blend::{BlendMode, DstBlend};
use api::geometry::{PixelRegion, RectF};
use engine::{Rect, Scene, compose_layer, project_scene, project_window, resolve_blend};
use render::{
    PipelineConfig, RenderContent, RenderNode, RenderNodeId, RenderScene, Texture, TextureDesc,
    create_render_pipeline, create_texture,
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
/// parent at origin (112, 48) with its own 20×10 rect. The full-window
/// primary screen layer is skipped, so the parent/child/sibling get
/// z = 0/1/2.
#[test]
fn hierarchy_composes_position_opacity_visibility_and_order() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (640, 480));
    scene.window_mut(win).unwrap().opacity = 0.8;
    // The window's screen surface: a primary layer with a screen buffer.
    let screen = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(screen).unwrap();
        l.rect = full_window_rect(640, 480);
        l.visible = true;
    }
    let screen_bmp = scene.add_bitmap(1, 1, vec![0u8; 4]);
    scene.layer_mut(screen).unwrap().bitmap = Some(screen_bmp);
    scene.bitmap_mut(screen_bmp).unwrap().screen_buffer = true;

    let parent = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(parent).unwrap();
        l.rect = Rect::new(100, 40, 200, 100);
        l.opacity = 0.5;
        l.visible = true;
        l.fill_color = Some([255, 255, 255, 255]);
    }
    let child = scene.add_layer(win, Some(parent));
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(12, 8, 20, 10);
        l.opacity = 0.5;
        l.visible = true;
        l.fill_color = Some([255, 255, 255, 255]);
    }
    let sibling = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(sibling).unwrap();
        l.rect = Rect::new(1, 2, 3, 4);
        l.visible = true;
        l.fill_color = Some([255, 255, 255, 255]);
    }

    let projected = project_window(&scene, win, |_| None);
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 3, "the screen buffer is not a node");

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
        l.fill_color = Some([0, 0, 0, 255]);
    }

    let projected = project_window(&scene, win, |_| None);
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1);
    assert!((nodes[0].opacity - 0.25).abs() < 1e-6);
}

#[test]
fn fill_color_is_straight_alpha() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let fill = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(fill).unwrap();
        l.rect = full_window_rect(64, 64);
        l.fill_color = Some([255, 0, 0, 128]);
    }

    let projected = project_window(&scene, win, |_| None);
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1);
    match &nodes[0].content {
        RenderContent::Solid(color) => {
            assert_eq!(color[0], 1.0, "RGB untouched by alpha");
            assert_eq!(color[1], 0.0);
            assert_eq!(color[2], 0.0);
            assert!((color[3] - 128.0 / 255.0).abs() < 1e-6);
        }
        other => panic!("expected a solid fill, got {other:?}"),
    }
    // A solid fill has no source sub-region.
    assert_eq!(nodes[0].src, PixelRegion::ZERO);
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

    let projected = project_window(&scene, win, |_| Some(null_texture(251, 254)));
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

    let projected = project_window(&scene, win, |_| Some(null_texture(64, 64)));
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

    let projected = project_window(&scene, win, |_| None);
    assert!(drawable_nodes(&projected).is_empty());
}

#[test]
fn missing_bitmap_falls_back_to_solid_fill() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let layer = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(layer).unwrap();
        l.bitmap = Some(9999); // never added
        l.fill_color = Some([1, 2, 3, 255]);
        l.rect = full_window_rect(64, 64);
    }

    let projected = project_window(&scene, win, |_| None);
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1);
    assert!(matches!(nodes[0].content, RenderContent::Solid(_)));
}

#[test]
fn screen_buffer_primary_layer_is_not_drawn() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let layer = scene.add_layer(win, None);
    let bmp = scene.add_bitmap(64, 64, vec![0u8; 64 * 64 * 4]);
    {
        let l = scene.layer_mut(layer).unwrap();
        l.bitmap = Some(bmp);
        l.rect = full_window_rect(64, 64);
    }
    scene.bitmap_mut(bmp).unwrap().screen_buffer = true;

    let projected = project_window(&scene, win, |_| Some(null_texture(64, 64)));
    assert!(
        drawable_nodes(&projected).is_empty(),
        "the primary layer's screen buffer must not be drawn"
    );
}

#[test]
fn empty_layer_is_skipped_but_children_render() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let parent = scene.add_layer(win, None);
    {
        // No bitmap and no fill colour: the layer draws nothing itself.
        let l = scene.layer_mut(parent).unwrap();
        l.rect = full_window_rect(64, 64);
    }
    let child = scene.add_layer(win, Some(parent));
    {
        let l = scene.layer_mut(child).unwrap();
        l.visible = true;
        l.fill_color = Some([1, 2, 3, 255]);
        l.rect = Rect::new(0, 0, 8, 8);
    }

    let projected = project_window(&scene, win, |_| None);
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
        l.fill_color = Some([1, 2, 3, 255]);
        l.rect = full_window_rect(64, 64);
    }

    let projected = project_window(&scene, win, |_| None);
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
        l.fill_color = Some([1, 2, 3, 255]);
        l.rect = full_window_rect(64, 64);
    }

    let projected = project_window(&scene, win, |_| None);
    assert!(projected.nodes.iter().all(|n| n.children.is_empty()));
}

#[test]
fn project_scene_shares_z_across_windows() {
    let mut scene = Scene::default();
    let first = scene.add_window("a", (64, 64));
    let second = scene.add_window("b", (64, 64));
    for (window, x) in [(first, 0), (second, 32)] {
        let layer = scene.add_layer(window, None);
        let l = scene.layer_mut(layer).unwrap();
        l.fill_color = Some([1, 2, 3, 255]);
        l.rect = Rect::new(x, 0, 16, 16);
    }

    let projected = project_scene(&scene, |_| None);
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
        l.fill_color = Some([1, 2, 3, 255]);
        l.rect = full_window_rect(64, 64);
    }

    let projected = project_window(&scene, win, |_| None);
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
        l.fill_color = Some([1, 2, 3, 255]);
        l.rect = Rect::new(1, 1, 4, 4);
    }
    let invisible = scene.add_layer(win, Some(grandparent));
    scene.layer_mut(invisible).unwrap().visible = false;
    let child = scene.add_layer(win, Some(invisible));
    {
        let l = scene.layer_mut(child).unwrap();
        l.visible = true;
        l.fill_color = Some([4, 5, 6, 255]);
        l.rect = Rect::new(2, 2, 3, 3);
    }

    let projected = project_window(&scene, win, |_| None);
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1, "an invisible parent hides its subtree");
    assert_eq!(nodes[0].dst, RectF::new(1.0, 1.0, 4.0, 4.0));
}
