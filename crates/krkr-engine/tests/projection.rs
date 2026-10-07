//! Integration tests for the L2 → L1 render projection.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/projection.rs`. The
//! private helpers used there are reimplemented here on top of the public
//! `krkr_render` API.

use krkr_api::blend::{BlendMode, DstBlend};
use krkr_api::geometry::{PixelRegion, RectF};
use krkr_engine::{
    LT_ADD_ALPHA, LT_ADDITIVE, LT_ALPHA, LT_BINDER, LT_DARKEN, LT_DODGE, LT_EFFECT, LT_FILTER,
    LT_LIGHTEN, LT_MULTIPLICATIVE, LT_OPAQUE, LT_PS_ADDITIVE, LT_PS_COLOR_BURN, LT_PS_COLOR_DODGE,
    LT_PS_COLOR_DODGE5, LT_PS_DARKEN, LT_PS_DIFFERENCE, LT_PS_DIFFERENCE5, LT_PS_EXCLUSION,
    LT_PS_HARD_LIGHT, LT_PS_LIGHTEN, LT_PS_MULTIPLICATIVE, LT_PS_NORMAL, LT_PS_OVERLAY,
    LT_PS_SCREEN, LT_PS_SOFT_LIGHT, LT_PS_SUBTRACTIVE, LT_SCREEN, LT_SUBTRACTIVE, Rect, Scene,
    blend_mode_for, clamp_opacity, compose_layer, project_scene, project_window, resolve_blend,
    visible_image_region,
};
use krkr_render::{
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
    Rect { x: 0, y: 0, w, h }
}

#[test]
fn clamp_opacity_bounds() {
    assert_eq!(clamp_opacity(-0.5), 0.0);
    assert_eq!(clamp_opacity(0.5), 0.5);
    assert_eq!(clamp_opacity(1.5), 1.0);
    assert_eq!(clamp_opacity(f32::NAN), 0.0, "NaN clamps to 0");
}

/// Every documented native type maps to its own mode — never silently to
/// source-over except for the truly unknown / out-of-range values.
#[test]
fn blend_mode_mapping_is_total() {
    let expected = [
        (LT_OPAQUE, BlendMode::Replace),
        (LT_ALPHA, BlendMode::SourceOver),
        (LT_ADDITIVE, BlendMode::Additive),
        (LT_SUBTRACTIVE, BlendMode::Subtractive),
        (LT_MULTIPLICATIVE, BlendMode::Multiply),
        (
            LT_DODGE,
            BlendMode::DestinationDependent(DstBlend::ColorDodge),
        ),
        (LT_DARKEN, BlendMode::DestinationDependent(DstBlend::Darken)),
        (
            LT_LIGHTEN,
            BlendMode::DestinationDependent(DstBlend::Lighten),
        ),
        (LT_SCREEN, BlendMode::Screen),
        (LT_ADD_ALPHA, BlendMode::AddAlpha),
        (LT_PS_NORMAL, BlendMode::SourceOver),
        (LT_PS_ADDITIVE, BlendMode::Additive),
        (LT_PS_SUBTRACTIVE, BlendMode::Subtractive),
        (LT_PS_MULTIPLICATIVE, BlendMode::Multiply),
        (LT_PS_SCREEN, BlendMode::Screen),
        (
            LT_PS_OVERLAY,
            BlendMode::DestinationDependent(DstBlend::Overlay),
        ),
        (
            LT_PS_HARD_LIGHT,
            BlendMode::DestinationDependent(DstBlend::HardLight),
        ),
        (
            LT_PS_SOFT_LIGHT,
            BlendMode::DestinationDependent(DstBlend::SoftLight),
        ),
        (
            LT_PS_COLOR_DODGE,
            BlendMode::DestinationDependent(DstBlend::ColorDodge),
        ),
        (
            LT_PS_COLOR_DODGE5,
            BlendMode::DestinationDependent(DstBlend::ColorDodge5),
        ),
        (
            LT_PS_COLOR_BURN,
            BlendMode::DestinationDependent(DstBlend::ColorBurn),
        ),
        (
            LT_PS_LIGHTEN,
            BlendMode::DestinationDependent(DstBlend::Lighten),
        ),
        (
            LT_PS_DARKEN,
            BlendMode::DestinationDependent(DstBlend::Darken),
        ),
        (
            LT_PS_DIFFERENCE,
            BlendMode::DestinationDependent(DstBlend::Difference),
        ),
        (
            LT_PS_DIFFERENCE5,
            BlendMode::DestinationDependent(DstBlend::Difference5),
        ),
        (
            LT_PS_EXCLUSION,
            BlendMode::DestinationDependent(DstBlend::Exclusion),
        ),
    ];
    for (native, mode) in expected {
        assert_eq!(blend_mode_for(native), Some(mode), "type {native}");
    }
    // Effect/filter are callbacks, not blends.
    assert_eq!(blend_mode_for(LT_EFFECT), Some(BlendMode::SourceOver));
    assert_eq!(blend_mode_for(LT_FILTER), Some(BlendMode::SourceOver));
    // A binder is skipped entirely.
    assert_eq!(blend_mode_for(LT_BINDER), None);
    // Garbage values degrade safely.
    assert_eq!(blend_mode_for(-1), Some(BlendMode::SourceOver));
    assert_eq!(blend_mode_for(9999), Some(BlendMode::SourceOver));
}

#[test]
fn opaque_degrades_below_full_opacity() {
    assert_eq!(resolve_blend(LT_OPAQUE, 1.0), Some(BlendMode::Replace));
    assert_eq!(resolve_blend(LT_OPAQUE, 0.5), Some(BlendMode::SourceOver));
    assert_eq!(resolve_blend(LT_ALPHA, 0.5), Some(BlendMode::SourceOver));
    assert_eq!(resolve_blend(LT_BINDER, 1.0), None);
}

#[test]
fn compose_layer_accumulates_position_opacity_visibility() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (640, 480));
    scene.window_mut(win).unwrap().opacity = 0.8;
    let parent = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(parent).unwrap();
        l.rect = Rect {
            x: 100,
            y: 40,
            w: 200,
            h: 100,
        };
        l.opacity = 0.5;
        l.visible = true;
    }
    let child = scene.add_layer(win, Some(parent));
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect {
            x: 12,
            y: 8,
            w: 20,
            h: 10,
        };
        l.opacity = 0.5;
        l.visible = true;
    }

    let composed = compose_layer(&scene, child).expect("composed");
    assert_eq!(
        composed.rect,
        Rect {
            x: 112,
            y: 48,
            w: 20,
            h: 10
        }
    );
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

#[test]
fn visible_image_region_selects_sprite_frame() {
    let layer = Rect {
        x: 100,
        y: 100,
        w: 32,
        h: 32,
    };
    let (src, dst) = visible_image_region(layer, -32, 0, 96, 32, 96, 32).unwrap();
    assert_eq!(src, PixelRegion::new(32, 0, 32, 32));
    assert_eq!(dst, RectF::new(100.0, 100.0, 32.0, 32.0));

    let (src, _) = visible_image_region(layer, 0, 0, 96, 32, 96, 32).unwrap();
    assert_eq!(src, PixelRegion::new(0, 0, 32, 32));

    let (src, _) = visible_image_region(layer, -64, 0, 96, 32, 96, 32).unwrap();
    assert_eq!(src, PixelRegion::new(64, 0, 32, 32));
}

#[test]
fn visible_image_region_selects_tbutton_sheet_cell() {
    let layer = Rect {
        x: 144,
        y: 244,
        w: 100,
        h: 40,
    };
    let (src, dst) = visible_image_region(layer, -100, 0, 400, 40, 400, 40).unwrap();
    assert_eq!(src, PixelRegion::new(100, 0, 100, 40));
    assert_eq!(dst, RectF::new(144.0, 244.0, 100.0, 40.0));

    for (pattern, expected_x) in [(0u32, 0u32), (1, 100), (2, 200), (3, 300)] {
        let (src, _) =
            visible_image_region(layer, -100 * pattern as i32, 0, 400, 40, 400, 40).unwrap();
        assert_eq!(
            src,
            PixelRegion::new(expected_x, 0, 100, 40),
            "pattern {pattern} must sample its own cell"
        );
    }
}

#[test]
fn visible_image_region_clips_large_image() {
    let layer = Rect {
        x: 50,
        y: 60,
        w: 100,
        h: 100,
    };
    let (src, dst) = visible_image_region(layer, -20, -30, 200, 200, 200, 200).unwrap();
    assert_eq!(dst, RectF::new(50.0, 60.0, 100.0, 100.0));
    assert_eq!(src, PixelRegion::new(20, 30, 100, 100));

    // An image placed entirely outside the layer yields nothing.
    assert!(visible_image_region(layer, -500, 0, 200, 200, 200, 200).is_none());
    assert!(visible_image_region(layer, 0, -500, 200, 200, 200, 200).is_none());
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
        l.rect = Rect {
            x: 100,
            y: 40,
            w: 200,
            h: 100,
        };
        l.opacity = 0.5;
        l.visible = true;
        l.fill_color = Some([255, 255, 255, 255]);
    }
    let child = scene.add_layer(win, Some(parent));
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect {
            x: 12,
            y: 8,
            w: 20,
            h: 10,
        };
        l.opacity = 0.5;
        l.visible = true;
        l.fill_color = Some([255, 255, 255, 255]);
    }
    let sibling = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(sibling).unwrap();
        l.rect = Rect {
            x: 1,
            y: 2,
            w: 3,
            h: 4,
        };
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
        l.rect = Rect {
            x: 297,
            y: 436,
            w: 251,
            h: 254,
        };
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
        l.rect = Rect {
            x: 10,
            y: 10,
            w: 1,
            h: 1,
        };
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
fn binder_is_skipped_but_children_render() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let binder = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(binder).unwrap();
        l.blend_type = LT_BINDER;
        l.fill_color = Some([9, 9, 9, 255]);
        l.rect = full_window_rect(64, 64);
    }
    let child = scene.add_layer(win, Some(binder));
    {
        let l = scene.layer_mut(child).unwrap();
        l.visible = true;
        l.fill_color = Some([1, 2, 3, 255]);
        l.rect = Rect {
            x: 0,
            y: 0,
            w: 8,
            h: 8,
        };
    }

    let projected = project_window(&scene, win, |_| None);
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1, "the binder is not a node");
    assert_eq!(nodes[0].dst, RectF::new(0.0, 0.0, 8.0, 8.0));
    // The binder has no node, so the child links to the root.
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
        l.rect = Rect {
            x,
            y: 0,
            w: 16,
            h: 16,
        };
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
        l.rect = Rect {
            x: 1,
            y: 1,
            w: 4,
            h: 4,
        };
    }
    let invisible = scene.add_layer(win, Some(grandparent));
    scene.layer_mut(invisible).unwrap().visible = false;
    let child = scene.add_layer(win, Some(invisible));
    {
        let l = scene.layer_mut(child).unwrap();
        l.visible = true;
        l.fill_color = Some([4, 5, 6, 255]);
        l.rect = Rect {
            x: 2,
            y: 2,
            w: 3,
            h: 3,
        };
    }

    let projected = project_window(&scene, win, |_| None);
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1, "an invisible parent hides its subtree");
    assert_eq!(nodes[0].dst, RectF::new(1.0, 1.0, 4.0, 4.0));
}
