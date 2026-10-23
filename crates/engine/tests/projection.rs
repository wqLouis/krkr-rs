//! Integration tests for the L2 → L1 render projection.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/projection.rs`. The
//! private helpers used there are reimplemented here on top of the public
//! `render` API.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use api::blend::{BlendMode, DstBlend};
use api::geometry::{PixelRegion, RectF};
use engine::{
    BitmapState, Engine, EngineOverlay, EngineParts, GameFrame, Map, PixelTranslation, Rect, Scene,
    TransitionFrame, TransitionSource, compose_layer, project_scene, project_window,
    project_window_report, resolve_blend,
};
use render::{
    Clip, PipelineConfig, RenderContent, RenderNode, RenderNodeId, RenderPipeline, RenderScene,
    RenderState, Texture, TextureDesc, begin_render_state, create_render_pipeline, create_texture,
};
use testkit::headless_window;

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

    let projected = project_window(&scene, win, |_| Some(null_texture(1, 1)));
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

    let projected = project_window(&scene, win, |_| Some(null_texture(1, 1)));
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

    let projected = project_window(&scene, win, |_| Some(null_texture(1, 1)));
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

    let projected = project_window(&scene, win, |_| Some(null_texture(1, 1)));
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

    let projected = project_window(&scene, win, |_| Some(null_texture(1, 1)));
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

    let projected = project_scene(&scene, |_| Some(null_texture(1, 1)));
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

    let projected = project_window(&scene, win, |_| Some(null_texture(1, 1)));
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

    let projected = project_window(&scene, win, |_| Some(null_texture(1, 1)));
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

    let projected = project_window(&scene, win, |id| {
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

/// The draw report follows the emit path: a cached ancestor emits one node and
/// reports the visible descendants whose pixels that node's texture contains,
/// while a hidden descendant is not reported.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7156-7226
#[test]
fn drawn_report_matches_emitted_nodes_and_cached_subtree() {
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
    let hidden = scene.add_layer(win, Some(parent));
    {
        let l = scene.layer_mut(hidden).unwrap();
        l.visible = false;
        l.rect = Rect::new(8, 0, 8, 8);
    }
    attach_image(&mut scene, hidden, 8, 8);
    let sibling = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(sibling).unwrap();
        l.visible = true;
        l.rect = Rect::new(16, 0, 8, 8);
    }
    attach_image(&mut scene, sibling, 8, 8);

    let report = project_window_report(&scene, win, |id| {
        (id == cache || id == sibling).then(|| null_texture(8, 8))
    });
    assert_eq!(drawable_nodes(&report.scene).len(), 2);
    assert_eq!(report.drawn_layers, vec![parent, child, sibling]);
    assert!(
        !report.drawn_layers.contains(&hidden),
        "a hidden descendant is not drawn by the cached node"
    );
}

/// A transition without children blends the layer's own image and the
/// transition source's into a temporary that `DrawSelf` draws in place of the
/// `MainImage` (`reference/cpp/core/visual/LayerIntf.cpp:6745-6768`). Only the
/// layer's own image is replaced, so its children keep drawing live
/// (`:7278-7309`), and a layer in such a transition draws no cache of its own
/// (`:7156-7157`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:6745-6768
/// reference/cpp/core/visual/LayerIntf.cpp:6781-6783
/// reference/cpp/core/visual/LayerIntf.cpp:7156-7157
#[test]
fn transition_self_image_replaces_the_main_image_at_its_placement() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (64, 64));
    let layer = scene.add_layer(win, None);
    let main = attach_image(&mut scene, layer, 16, 16);
    {
        let l = scene.layer_mut(layer).unwrap();
        l.rect = Rect::new(4, 6, 32, 32);
        l.image_left = 2;
        l.image_top = 3;
        l.image_width = 16;
        l.image_height = 16;
        l.visible = true;
    }
    let child = scene.add_layer(win, Some(layer));
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(20, 20, 8, 8);
        l.visible = true;
    }
    let child_bitmap = attach_image(&mut scene, child, 8, 8);
    let self_image = engine::layer_ops::ensure_transition_self_image(&mut scene, layer, 16, 16)
        .expect("transition self image");

    // Resolving the main image alone leaves the layer without a texture: the
    // self image stands in for it, so only the live child is emitted.
    let only_main = project_window(&scene, win, |id| {
        (id == main || id == child_bitmap).then(|| null_texture(16, 16))
    });
    let nodes = drawable_nodes(&only_main);
    assert_eq!(
        nodes.len(),
        1,
        "the layer is not drawn from its main image while the self image stands in"
    );
    assert_eq!(nodes[0].dst, RectF::new(24.0, 26.0, 8.0, 8.0));
    assert_eq!(nodes[0].parent, Some(only_main.root));

    // The self image is drawn at the main image's placement — the layer rect
    // plus `image_left`/`image_top`, sized by `image_width`/`image_height` —
    // and the child stays live under it.
    let with_self = project_window(&scene, win, |id| {
        (id == self_image || id == child_bitmap).then(|| null_texture(16, 16))
    });
    let nodes = drawable_nodes(&with_self);
    assert_eq!(nodes.len(), 2, "the self image plus the live child");
    let self_node = nodes
        .iter()
        .find(|n| n.dst == RectF::new(6.0, 9.0, 16.0, 16.0))
        .expect("self image node at the main image placement");
    assert_eq!(self_node.src, PixelRegion::new(0, 0, 16, 16));
    let child_node = nodes
        .iter()
        .find(|n| n.dst == RectF::new(24.0, 26.0, 8.0, 8.0))
        .expect("live child node");
    let self_id = with_self
        .nodes
        .iter()
        .position(|n| n.dst == RectF::new(6.0, 9.0, 16.0, 16.0))
        .map(|index| RenderNodeId(u32::try_from(index).expect("node index")))
        .expect("self image node");
    assert_eq!(
        child_node.parent,
        Some(self_id),
        "the child hangs off the layer's self image node"
    );

    // A rendered cache does not stand in for the self image nor hide the
    // children: the layer draws its self image and the children stay live.
    let cache = scene.add_bitmap(32, 32, vec![0u8; 32 * 32 * 4]);
    {
        let l = scene.layer_mut(layer).unwrap();
        l.cache_enabled_count = 1;
        l.cache_bitmap = Some(cache);
    }
    let only_cache = project_window(&scene, win, |id| {
        (id == cache || id == child_bitmap).then(|| null_texture(32, 32))
    });
    let nodes = drawable_nodes(&only_cache);
    assert_eq!(
        nodes.len(),
        1,
        "the cache is not drawn instead of the self image"
    );
    assert_eq!(nodes[0].dst, RectF::new(24.0, 26.0, 8.0, 8.0));
    {
        let l = scene.layer_mut(layer).unwrap();
        l.cache_enabled_count = 0;
        l.cache_bitmap = None;
    }

    // Clearing the slot (the transition teardown) returns the layer to its own
    // image at the same placement.
    scene.set_transition_self_image(layer, None);
    let with_main = project_window(&scene, win, |id| {
        (id == main || id == child_bitmap).then(|| null_texture(16, 16))
    });
    let nodes = drawable_nodes(&with_main);
    assert_eq!(nodes.len(), 2);
    let main_node = nodes
        .iter()
        .find(|n| n.dst == RectF::new(6.0, 9.0, 16.0, 16.0))
        .expect("main image node at the same placement");
    assert_eq!(main_node.src, PixelRegion::new(0, 0, 16, 16));
}

// ---------------------------------------------------------------------------
// The projection's skip key: the structural revision, not the pixel content
// ---------------------------------------------------------------------------

/// An overlay that counts the engine's projections. The engine attaches it
/// once per rebuild, so `node()` is the exact rebuild count; its content is a
/// container, so it is not a drawable node.
struct ProjectionSpy {
    projections: Rc<Cell<u32>>,
}

impl EngineOverlay for ProjectionSpy {
    fn refresh(
        &mut self,
        _pipeline: &RenderPipeline,
        _state: &mut RenderState,
        _now_ms: u64,
        _objects: engine::ObjectCounts,
    ) -> Result<(), String> {
        Ok(())
    }

    fn node(&self) -> RenderNode {
        self.projections
            .set(self.projections.get().saturating_add(1));
        RenderNode::default()
    }
}

/// A headless engine and the counter its overlay increments once per
/// projection.
fn headless_engine_with_projection_count(size: (u32, u32)) -> (Engine, Rc<Cell<u32>>) {
    let pipeline = create_render_pipeline(PipelineConfig::null()).expect("null pipeline");
    let handle = headless_window();
    let (surface, render_state) = begin_render_state(&pipeline, &handle).expect("surface");
    let projections = Rc::new(Cell::new(0));
    let parts = EngineParts {
        pipeline,
        surface,
        render_state,
        surface_size: size,
        headless: true,
        overlay: Some(Box::new(ProjectionSpy {
            projections: Rc::clone(&projections),
        })),
    };
    (Engine::new(parts).expect("engine"), projections)
}

/// A frame hook that records nothing.
struct NoHooks;

impl GameFrame for NoHooks {
    fn before_render(&mut self, _now_ms: u64, _dt_ms: f64) -> Result<(), String> {
        Ok(())
    }
}

/// Adds the window's primary layer: a `32x32` rect carrying the `w` x `h`
/// bitmap handed back. The image size stays unset, so the projection draws the
/// bitmap at its own size.
fn drawable_layer(engine: &mut Engine, window: u32, w: u32, h: u32) -> (u32, u32) {
    let layer = engine.scene_mut().add_layer(window, None);
    let bitmap = engine
        .scene_mut()
        .add_bitmap(w, h, vec![0u8; 4 * (w * h) as usize]);
    let state = engine.scene_mut().layer_mut(layer).expect("layer");
    state.rect = Rect::new(0, 0, 32, 32);
    state.visible = true;
    state.bitmap = Some(bitmap);
    (layer, bitmap)
}

/// The projected layer nodes, in arena order. The root and the overlay are
/// containers; only a layer node carries a texture.
fn projected_layer_nodes(engine: &Engine) -> Vec<&RenderNode> {
    engine
        .render_scene()
        .nodes
        .iter()
        .filter(|node| !matches!(node.content, RenderContent::None))
        .collect()
}

/// A bitmap content write is not a change to the layer tree, so the frame
/// uploads the pixels to the existing texture and reuses the projected scene:
/// the projection still reads geometry from the scene graph.
#[test]
fn a_content_only_bitmap_write_does_not_reproject() {
    let (mut engine, projections) = headless_engine_with_projection_count((32, 32));
    let window = engine.scene_mut().add_window("game", (32, 32));
    let (layer, bitmap) = drawable_layer(&mut engine, window, 8, 8);
    let mut hooks = NoHooks;

    engine.step(1.0, &mut hooks).expect("first frame");
    assert_eq!(projections.get(), 1);
    assert_eq!(projected_layer_nodes(&engine).len(), 1);

    for value in [1u8, 2, 3] {
        {
            let state = engine.scene_mut().bitmap_mut(bitmap).expect("bitmap");
            state.rgba.fill(value);
            state.mark_dirty();
        }
        engine.step(1.0, &mut hooks).expect("content frame");
        assert_eq!(
            projections.get(),
            1,
            "a bitmap content write must not re-project"
        );
        assert!(
            !engine.scene().bitmap(bitmap).expect("bitmap").dirty,
            "the dirty bitmap is still uploaded"
        );
    }
    assert_eq!(
        projected_layer_nodes(&engine)[0].dst,
        RectF::new(0.0, 0.0, 8.0, 8.0),
        "the reused projection keeps the layer's placement"
    );
    assert_eq!(engine.scene().layer(layer).expect("layer").rect.w, 32);
}

/// A layer property write, a window write and a layer creation all change the
/// tree the projection reads, so each one rebuilds the projected scene.
#[test]
fn layer_window_and_order_changes_reproject() {
    let (mut engine, projections) = headless_engine_with_projection_count((32, 32));
    let window = engine.scene_mut().add_window("game", (32, 32));
    let (layer, _bitmap) = drawable_layer(&mut engine, window, 8, 8);
    let mut hooks = NoHooks;

    engine.step(1.0, &mut hooks).expect("first frame");
    assert_eq!(projections.get(), 1);

    engine.scene_mut().layer_mut(layer).expect("layer").rect = Rect::new(0, 0, 16, 16);
    engine.step(1.0, &mut hooks).expect("geometry frame");
    assert_eq!(projections.get(), 2, "a layer geometry change re-projects");

    engine
        .scene_mut()
        .window_mut(window)
        .expect("window")
        .opacity = 0.5;
    engine.step(1.0, &mut hooks).expect("window frame");
    assert_eq!(projections.get(), 3, "a window property change re-projects");

    engine.scene_mut().add_layer(window, None);
    engine.step(1.0, &mut hooks).expect("order frame");
    assert_eq!(projections.get(), 4, "a new layer re-projects");
}

/// A bitmap that is created, resized or removed changes the geometry the
/// projection reads through the layer's image, so the projected node follows
/// it even though the layer record itself is untouched in the resize case.
#[test]
fn a_new_resized_or_removed_bitmap_still_projects() {
    let (mut engine, projections) = headless_engine_with_projection_count((32, 32));
    let window = engine.scene_mut().add_window("game", (32, 32));
    let (layer, _bitmap) = drawable_layer(&mut engine, window, 8, 8);
    let mut hooks = NoHooks;

    engine.step(1.0, &mut hooks).expect("first frame");
    assert_eq!(projections.get(), 1);
    assert_eq!(
        projected_layer_nodes(&engine)[0].dst,
        RectF::new(0.0, 0.0, 8.0, 8.0)
    );

    // A new bitmap assigned as the layer image.
    let created = engine.scene_mut().add_bitmap(4, 4, vec![0u8; 4 * 4 * 4]);
    engine.scene_mut().layer_mut(layer).expect("layer").bitmap = Some(created);
    engine.step(1.0, &mut hooks).expect("new bitmap frame");
    assert_eq!(projections.get(), 2, "a new bitmap re-projects");
    assert_eq!(
        projected_layer_nodes(&engine)[0].dst,
        RectF::new(0.0, 0.0, 4.0, 4.0)
    );

    // A resize of the bitmap the layer draws.
    assert!(engine.scene_mut().resize_bitmap(created, 6, 6, [0; 4]));
    engine.step(1.0, &mut hooks).expect("resized bitmap frame");
    assert_eq!(projections.get(), 3, "a resized bitmap re-projects");
    assert_eq!(
        projected_layer_nodes(&engine)[0].dst,
        RectF::new(0.0, 0.0, 6.0, 6.0)
    );

    // Removing the layer's image and the bitmap drops the node.
    engine.scene_mut().layer_mut(layer).expect("layer").bitmap = None;
    engine.scene_mut().remove_bitmap(created);
    engine.step(1.0, &mut hooks).expect("removed bitmap frame");
    assert_eq!(projections.get(), 4, "a removed bitmap re-projects");
    assert!(
        projected_layer_nodes(&engine).is_empty(),
        "the layer has no image to draw"
    );
}

/// A texture-less container is still a clip: the reference intersects every
/// child's draw rect with the parent's `Rect`
/// (`reference/cpp/core/visual/LayerIntf.cpp:7138-7140`, `:7184-7185`), so a
/// child entirely outside the container emits nothing.
#[test]
fn textureless_parent_clips_a_child_outside_its_rect() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (320, 240));
    let parent = scene.add_layer(win, None);
    scene.layer_mut(parent).unwrap().rect = Rect::new(10, 10, 100, 100);
    scene.layer_mut(parent).unwrap().visible = true;
    let child = scene.add_layer(win, Some(parent));
    scene.layer_mut(child).unwrap().rect = Rect::new(0, 150, 20, 20);
    scene.layer_mut(child).unwrap().visible = true;
    attach_image(&mut scene, child, 20, 20);

    let textures: Rc<RefCell<Vec<(u32, Texture)>>> = Rc::new(RefCell::new(Vec::new()));
    let projected = project_window(&scene, win, |bitmap| {
        let texture = null_texture(20, 20);
        textures.borrow_mut().push((bitmap, texture.clone()));
        Some(texture)
    });
    assert!(
        drawable_nodes(&projected).is_empty(),
        "the child is outside the textureless parent's rect"
    );
}

/// A child inside the container keeps the ancestor clip; the projection emits
/// the clip in VM pixels and the presentation root transform scales it.
///
/// Reference: `r ∩ Rect` (`reference/cpp/core/visual/LayerIntf.cpp:7138-7140`)
/// and the child intersection (`:7184-7185`); the clip is the parent's absolute
/// rect for a child that lies wholly inside it.
#[test]
fn textureless_parent_clip_is_emitted_in_vm_pixels() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (320, 240));
    let parent = scene.add_layer(win, None);
    scene.layer_mut(parent).unwrap().rect = Rect::new(10, 10, 100, 100);
    scene.layer_mut(parent).unwrap().visible = true;
    let child = scene.add_layer(win, Some(parent));
    scene.layer_mut(child).unwrap().rect = Rect::new(0, 0, 20, 20);
    scene.layer_mut(child).unwrap().visible = true;
    attach_image(&mut scene, child, 20, 20);

    let texture = null_texture(20, 20);
    let mut projected = project_window(&scene, win, |_| Some(texture.clone()));
    let node = drawable_nodes(&projected)
        .into_iter()
        .next()
        .expect("the child draws");
    assert_eq!(
        node.clip,
        Some(Clip::Rect(RectF::new(10.0, 10.0, 20.0, 20.0))),
        "the parent rect bounds the child"
    );

    let translation = PixelTranslation::fit((100, 100), (200, 200)).expect("fit");
    let root = projected.root;
    projected.node_mut(root).expect("root").transform = translation.root_transform();
    let node = drawable_nodes(&projected)
        .into_iter()
        .next()
        .expect("the child draws");
    assert_eq!(
        node.clip,
        Some(Clip::Rect(RectF::new(10.0, 10.0, 20.0, 20.0))),
        "a rectangle clip stays in VM pixels"
    );
    assert_eq!(
        projected.node(root).expect("root").transform,
        translation.root_transform(),
        "the root carries the presentation"
    );
}

/// Reference `GetMostFrontChildAt` rejects a point outside the current
/// layer's `Rect` before descending into its children
/// (`reference/cpp/core/visual/LayerIntf.cpp:3322-3329`), so a child clipped
/// by an ancestor is not hit-testable outside that ancestor.
#[test]
fn hit_test_rejects_a_point_outside_an_ancestor_rect() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (320, 240));
    let parent = scene.add_layer(win, None);
    scene.layer_mut(parent).unwrap().rect = Rect::new(0, 0, 50, 50);
    scene.layer_mut(parent).unwrap().visible = true;
    let child = scene.add_layer(win, Some(parent));
    scene.layer_mut(child).unwrap().rect = Rect::new(0, 40, 20, 20);
    scene.layer_mut(child).unwrap().visible = true;
    attach_image(&mut scene, child, 20, 20);
    scene.layer_mut(child).unwrap().hit_threshold = 0;

    assert_eq!(
        scene.layer_at(child, 5, 5, false, false),
        Some(child),
        "a point inside both rects hits"
    );
    assert_eq!(
        scene.layer_at(child, 5, 45, false, false),
        None,
        "a point inside the child but outside the parent rect misses"
    );
}

/// A running transition the host composites draws its two sources in place of
/// the layer's image: the first with the layer's placement, blend and opacity,
/// the second faded over it by its own alpha.
///
/// The reference `Process` blends the two sources into the layer's frame and
/// the frame is drawn with the layer's own type and opacity
/// (`reference/cpp/core/visual/TransIntf.cpp:592-600`), so a full-opacity
/// source keeps the layer's blend and a fading one composites source-over.
#[test]
fn a_transition_frame_draws_both_sources_at_the_layer_placement() {
    let mut scene = Scene::default();
    scene.composite_transition_frames = true;
    let win = scene.add_window("t", (64, 64));
    let layer = scene.add_layer(win, None);
    {
        let state = scene.layer_mut(layer).unwrap();
        state.rect = Rect::new(4, 6, 8, 4);
        state.visible = true;
        state.opacity = 0.5;
        state.blend = BlendMode::Additive;
    }
    let src1 = attach_image(&mut scene, layer, 8, 4);
    let src2 = scene.add_bitmap(8, 4, vec![0u8; 8 * 4 * 4]);
    scene.set_transition_frame(
        layer,
        Some(TransitionFrame::new(
            (8, 4),
            true,
            TransitionSource::new(src1, Rect::new(0, 0, 8, 4), Rect::new(0, 0, 8, 4), 1.0),
            TransitionSource::new(src2, Rect::new(0, 0, 8, 4), Rect::new(2, 0, 6, 4), 0.4),
        )),
    );

    let projected = project_window(&scene, win, |_| Some(null_texture(8, 4)));
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 2, "one node per transition source");
    let first = nodes[0];
    assert_eq!(first.dst, RectF::new(4.0, 6.0, 8.0, 4.0));
    assert_eq!(first.src, PixelRegion::new(0, 0, 8, 4));
    assert_eq!(first.opacity, 0.5);
    assert_eq!(
        first.blend,
        BlendMode::Additive,
        "a placement keeps the blend"
    );
    let second = nodes[1];
    assert_eq!(second.dst, RectF::new(6.0, 6.0, 6.0, 4.0));
    assert_eq!(second.src, PixelRegion::new(0, 0, 6, 4));
    assert!((second.opacity - 0.2).abs() < 1e-6);
    assert_eq!(
        second.blend,
        BlendMode::SourceOver,
        "a fade composites over"
    );
}

/// A frame that covers only the layer's own image leaves the children live,
/// and a layer in a transition draws no cache of its own even when it is
/// marked cached: the frame replaces the image the cache holds
/// (`reference/cpp/core/visual/LayerIntf.cpp:6745-6768`, `:7156-7157`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:6745-6768
/// reference/cpp/core/visual/LayerIntf.cpp:7156-7157
#[test]
fn a_self_frame_transition_draws_no_cache_and_keeps_its_children_live() {
    let mut scene = Scene::default();
    scene.composite_transition_frames = true;
    let win = scene.add_window("t", (64, 64));
    let layer = scene.add_layer(win, None);
    {
        let state = scene.layer_mut(layer).unwrap();
        state.rect = Rect::new(0, 0, 32, 32);
        state.visible = true;
    }
    let main = attach_image(&mut scene, layer, 16, 16);
    let child = scene.add_layer(win, Some(layer));
    scene.layer_mut(child).unwrap().rect = Rect::new(20, 20, 8, 8);
    scene.layer_mut(child).unwrap().visible = true;
    let child_bitmap = attach_image(&mut scene, child, 8, 8);
    let cache = scene.add_bitmap(32, 32, vec![0u8; 32 * 32 * 4]);
    {
        let state = scene.layer_mut(layer).unwrap();
        state.cache_enabled_count = 1;
        state.cache_bitmap = Some(cache);
    }
    let src2 = scene.add_bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    scene.set_transition_frame(
        layer,
        Some(TransitionFrame::new(
            (16, 16),
            false,
            TransitionSource::new(main, Rect::new(0, 0, 16, 16), Rect::new(0, 0, 16, 16), 1.0),
            TransitionSource::new(src2, Rect::new(0, 0, 16, 16), Rect::new(0, 0, 16, 16), 0.5),
        )),
    );

    let projected = project_window(&scene, win, |id| {
        (id == main || id == src2 || id == child_bitmap || id == cache).then(|| {
            null_texture(
                if id == child_bitmap { 8 } else { 16 },
                if id == child_bitmap { 8 } else { 16 },
            )
        })
    });
    let nodes = drawable_nodes(&projected);
    assert_eq!(
        nodes.len(),
        3,
        "the two frame sources plus the live child draw; the cache does not"
    );
    assert!(
        nodes
            .iter()
            .any(|node| node.dst == RectF::new(20.0, 20.0, 8.0, 8.0)),
        "the child keeps drawing live"
    );
    assert!(
        nodes
            .iter()
            .all(|node| node.dst != RectF::new(0.0, 0.0, 32.0, 32.0)),
        "the layer's cached subtree is not drawn"
    );
}

/// A frame update keeps a transition source that is also an ancestor of the
/// destination cache-current: that source subtree supplies the frame pixels,
/// so recompositing it would be pure per-tick work.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7734-7737
#[test]
fn a_frame_update_keeps_an_ancestor_source_cache_current() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (16, 16));
    let source = scene.add_layer(win, None);
    scene.layer_mut(source).unwrap().rect = Rect::new(0, 0, 8, 8);
    scene.layer_mut(source).unwrap().visible = true;
    let source_bitmap = attach_image(&mut scene, source, 8, 8);
    let dest = scene.add_layer(win, Some(source));
    scene.layer_mut(dest).unwrap().rect = Rect::new(4, 4, 8, 8);
    scene.layer_mut(dest).unwrap().visible = true;
    let dest_bitmap = attach_image(&mut scene, dest, 8, 8);
    // Clearing the destination's cache through `layer_mut` also invalidates
    // its ancestors, so clear the source last.
    scene.layer_mut(dest).unwrap().cache_recalc = false;
    scene.layer_mut(source).unwrap().cache_recalc = false;

    let frame = TransitionFrame::new(
        (8, 8),
        true,
        TransitionSource::new(
            dest_bitmap,
            Rect::new(0, 0, 8, 8),
            Rect::new(0, 0, 8, 8),
            1.0,
        ),
        TransitionSource::new(
            source_bitmap,
            Rect::new(0, 0, 8, 8),
            Rect::new(0, 0, 8, 8),
            0.5,
        ),
    );
    scene.set_transition_frame_for(dest, source, Some(frame));
    assert!(!scene.layer(dest).unwrap().cache_recalc);
    assert!(
        !scene.layer(source).unwrap().cache_recalc,
        "the ancestor source stays reusable as the frame source"
    );
}

/// A whole-subtree frame stands in for the layer's subtree, so its children
/// draw through it and are reported like the children a cache covers
/// (`reference/cpp/core/visual/LayerIntf.cpp:7221-7226`).
#[test]
fn a_whole_subtree_transition_frame_holds_its_children() {
    let mut scene = Scene::default();
    scene.composite_transition_frames = true;
    let win = scene.add_window("t", (64, 64));
    let layer = scene.add_layer(win, None);
    scene.layer_mut(layer).unwrap().rect = Rect::new(0, 0, 16, 16);
    scene.layer_mut(layer).unwrap().visible = true;
    let child = scene.add_layer(win, Some(layer));
    scene.layer_mut(child).unwrap().rect = Rect::new(2, 2, 4, 4);
    scene.layer_mut(child).unwrap().visible = true;
    attach_image(&mut scene, child, 4, 4);
    let src1 = attach_image(&mut scene, layer, 16, 16);
    let src2 = scene.add_bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    scene.inc_cache_enabled_count(layer);
    scene.set_transition_frame(
        layer,
        Some(TransitionFrame::new(
            (16, 16),
            true,
            TransitionSource::new(src1, Rect::new(0, 0, 16, 16), Rect::new(0, 0, 16, 16), 1.0),
            TransitionSource::new(src2, Rect::new(0, 0, 16, 16), Rect::new(0, 0, 16, 16), 0.0),
        )),
    );

    let report = project_window_report(&scene, win, |_| Some(null_texture(16, 16)));
    let nodes = drawable_nodes(&report.scene);
    assert_eq!(nodes.len(), 1, "only the frame's visible source draws");
    assert!(
        report.drawn_layers.contains(&child),
        "the covered child is reported: {:?}",
        report.drawn_layers
    );
    assert_eq!(
        report
            .drawn_layers
            .iter()
            .filter(|&&id| id == child)
            .count(),
        1
    );
}

/// A frame placed like the layer's own image follows the image placement, so a
/// scaled image scales the frame's sources with it.
#[test]
fn a_self_image_transition_frame_follows_the_image_placement() {
    let mut scene = Scene::default();
    scene.composite_transition_frames = true;
    let win = scene.add_window("t", (64, 64));
    let layer = scene.add_layer(win, None);
    {
        let state = scene.layer_mut(layer).unwrap();
        state.rect = Rect::new(10, 10, 8, 8);
        state.visible = true;
    }
    let src1 = attach_image(&mut scene, layer, 4, 4);
    // The image is drawn at twice its bitmap size.
    scene.layer_mut(layer).unwrap().image_width = 8;
    scene.layer_mut(layer).unwrap().image_height = 8;
    let src2 = scene.add_bitmap(4, 4, vec![0u8; 4 * 4 * 4]);
    scene.set_transition_frame(
        layer,
        Some(TransitionFrame::new(
            (4, 4),
            false,
            TransitionSource::new(src1, Rect::new(0, 0, 4, 4), Rect::new(0, 0, 4, 4), 1.0),
            TransitionSource::new(src2, Rect::new(0, 0, 4, 4), Rect::new(0, 0, 4, 4), 0.5),
        )),
    );

    let projected = project_window(&scene, win, |_| Some(null_texture(4, 4)));
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 2);
    assert_eq!(nodes[0].dst, RectF::new(10.0, 10.0, 8.0, 8.0));
    assert_eq!(nodes[1].dst, RectF::new(10.0, 10.0, 8.0, 8.0));
}

/// Clearing the frame returns the layer to its own image, and a layer whose
/// frame has no visible source draws nothing.
#[test]
fn clearing_a_transition_frame_returns_the_main_image() {
    let mut scene = Scene::default();
    scene.composite_transition_frames = true;
    let win = scene.add_window("t", (64, 64));
    let layer = scene.add_layer(win, None);
    scene.layer_mut(layer).unwrap().rect = Rect::new(0, 0, 4, 4);
    scene.layer_mut(layer).unwrap().visible = true;
    let src1 = attach_image(&mut scene, layer, 4, 4);
    scene.set_transition_frame(
        layer,
        Some(TransitionFrame::new(
            (4, 4),
            true,
            TransitionSource::new(src1, Rect::new(0, 0, 4, 4), Rect::new(0, 0, 0, 0), 1.0),
            TransitionSource::new(src1, Rect::new(0, 0, 4, 4), Rect::new(0, 0, 0, 0), 0.0),
        )),
    );
    let projected = project_window(&scene, win, |_| Some(null_texture(4, 4)));
    assert!(
        drawable_nodes(&projected).is_empty(),
        "a frame with no visible source draws nothing"
    );

    scene.set_transition_frame(layer, None);
    let projected = project_window(&scene, win, |_| Some(null_texture(4, 4)));
    let nodes = drawable_nodes(&projected);
    assert_eq!(nodes.len(), 1, "the main image is back");
    assert_eq!(nodes[0].dst, RectF::new(0.0, 0.0, 4.0, 4.0));
}

/// Composites one scene's transition frame through the L1 pipeline into an
/// offscreen target and returns its pixels — the same composition the Vulkan
/// backend performs when it draws the frame.
fn composite_frame_offscreen(scene: &Scene, window: u32, width: u32, height: u32) -> Vec<u8> {
    let pipeline = create_render_pipeline(PipelineConfig::null()).expect("null pipeline");
    let (_surface, mut state) = begin_render_state(&pipeline, &headless_window()).expect("begin");
    let mut textures: Map<u32, Texture> = Map::default();
    let report = project_window_report(scene, window, |id| {
        if let Some(texture) = textures.get(&id) {
            return Some(texture.clone());
        }
        let bitmap = scene.bitmap(id)?;
        let texture =
            create_texture(&pipeline, TextureDesc::new(bitmap.width, bitmap.height)).ok()?;
        render::upload_texture(
            &pipeline,
            &mut state,
            &texture,
            PixelRegion::new(0, 0, bitmap.width, bitmap.height),
            &bitmap.rgba,
        )
        .ok()?;
        textures.insert(id, texture.clone());
        Some(texture)
    });
    let target =
        render::create_render_target(&pipeline, render::RenderTargetDesc::new(width, height))
            .expect("target");
    render::render_to_target(&pipeline, &mut state, &target, &report.scene).expect("render");
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    render::read_render_target(
        &pipeline,
        &state,
        &target,
        PixelRegion::new(0, 0, width, height),
        &mut pixels,
    )
    .expect("read");
    pixels
}

/// Builds a one-layer window whose transition frame names two source bitmaps,
/// and returns the scene, the window and the two bitmaps.
fn transition_scene(
    trans: &engine::trans::Trans,
    src1: Vec<u8>,
    src2: Vec<u8>,
    width: u32,
    height: u32,
    covers_layer: bool,
) -> (Scene, u32, u32, u32) {
    let mut scene = Scene::default();
    scene.composite_transition_frames = true;
    let win = scene.add_window("t", (width, height));
    let layer = scene.add_layer(win, None);
    {
        let state = scene.layer_mut(layer).unwrap();
        state.rect = Rect::new(0, 0, width, height);
        state.visible = true;
    }
    let first = scene.add_bitmap(width, height, src1);
    let second = scene.add_bitmap(width, height, src2);
    scene.layer_mut(layer).unwrap().bitmap = Some(first);
    let frame = trans
        .transition_frame(first, second, (width, height), covers_layer)
        .expect("composited frame");
    scene.set_transition_frame(layer, Some(frame));
    (scene, win, first, second)
}

/// The composited `crossfade` frame and the CPU blend agree within one level
/// per channel wherever the second source is opaque. The reference blends the
/// two sources per channel at `Phase`
/// (`reference/cpp/core/visual/TransIntf.cpp:592-600,660`); the composited
/// frame draws the same two images with the phase as the second source's
/// alpha, so only the 8-bit rounding differs.
#[test]
fn a_composited_cross_fade_matches_the_cpu_blend() {
    let width = 4u32;
    let height = 4u32;
    let src1: Vec<u8> = [10u8, 40, 90, 255].repeat((width * height) as usize);
    let src2: Vec<u8> = [200u8, 120, 30, 255].repeat((width * height) as usize);
    for step_ms in [0.0, 20.0, 40.0, 60.0, 80.0, 100.0] {
        let mut trans = engine::trans::Trans::cross_fade(100.0).expect("crossfade");
        trans.step(step_ms);
        let (scene, window, first, second) =
            transition_scene(&trans, src1.clone(), src2.clone(), width, height, true);
        let composited = composite_frame_offscreen(&scene, window, width, height);

        let src1_bitmap = scene.bitmap(first).unwrap().clone();
        let src2_bitmap = scene.bitmap(second).unwrap().clone();
        let mut cpu = BitmapState::new(width, height, vec![0u8; (width * height * 4) as usize]);
        trans
            .apply(&mut cpu, &src1_bitmap, &src2_bitmap, (0, 0, 4, 4))
            .expect("cpu blend");

        for (index, (gpu, expected)) in composited
            .as_chunks::<4>()
            .0
            .iter()
            .zip(cpu.rgba.as_chunks::<4>().0.iter())
            .enumerate()
        {
            for (channel, (gpu, expected)) in gpu.iter().zip(expected).enumerate() {
                let difference = i32::from(*gpu) - i32::from(*expected);
                assert!(
                    difference.abs() <= 1,
                    "step {step_ms} ms, pixel {index}, channel {channel}: \
                     composited {gpu} vs cpu blend {expected}"
                );
            }
        }
    }
}

/// The composited `scroll` frame reproduces the CPU blit exactly: the two
/// sources are copied at their placed rectangles with no blend
/// (`reference/cpp/core/visual/TransIntf.cpp:1208-1217`).
#[test]
fn a_composited_scroll_matches_the_cpu_blit() {
    let width = 4u32;
    let height = 2u32;
    for step_ms in [0.0, 25.0, 50.0, 75.0, 100.0] {
        for from in [
            engine::trans::ScrollFrom::Left,
            engine::trans::ScrollFrom::Right,
            engine::trans::ScrollFrom::Top,
        ] {
            for stay in [
                engine::trans::ScrollStay::NoStay,
                engine::trans::ScrollStay::StayDest,
                engine::trans::ScrollStay::StaySrc,
            ] {
                let mut trans = engine::trans::Trans::scroll(100.0, from, stay).expect("scroll");
                trans.step(step_ms);
                let src1: Vec<u8> = [0u8, 0, 255, 255].repeat((width * height) as usize);
                let src2: Vec<u8> = [255u8, 255, 0, 255].repeat((width * height) as usize);
                let (scene, window, first, second) =
                    transition_scene(&trans, src1.clone(), src2.clone(), width, height, true);
                let composited = composite_frame_offscreen(&scene, window, width, height);
                let src1_bitmap = scene.bitmap(first).unwrap().clone();
                let src2_bitmap = scene.bitmap(second).unwrap().clone();
                let mut cpu =
                    BitmapState::new(width, height, vec![0u8; (width * height * 4) as usize]);
                trans
                    .apply(&mut cpu, &src1_bitmap, &src2_bitmap, (0, 0, 4, 2))
                    .expect("cpu blit");
                assert_eq!(
                    composited, cpu.rgba,
                    "step {step_ms} ms, from {from:?}, stay {stay:?}: the placements must match"
                );
            }
        }
    }
}

/// Where the second source carries alpha, the composited frame fades its
/// contribution by `phase * alpha`, so it may differ from the CPU blend by at
/// most `255 * t * (1 - alpha)` per channel — the documented tolerance. An
/// opaque first source is required: the frame composites over the scene, while
/// the CPU blend writes straight-alpha pixels.
#[test]
fn a_translucent_second_source_stays_within_the_documented_tolerance() {
    let width = 2u32;
    let height = 1u32;
    let src1: Vec<u8> = [10u8, 40, 90, 255].repeat((width * height) as usize);
    let src2: Vec<u8> = [200u8, 120, 30, 128].repeat((width * height) as usize);
    let mut trans = engine::trans::Trans::cross_fade(100.0).expect("crossfade");
    trans.step(50.0);
    let phase = trans.phase();
    assert!(phase > 0 && phase < engine::trans::PHASE_MAX);
    let t = f64::from(phase) / 256.0;
    let (scene, window, first, second) =
        transition_scene(&trans, src1.clone(), src2.clone(), width, height, true);
    let composited = composite_frame_offscreen(&scene, window, width, height);
    let src1_bitmap = scene.bitmap(first).unwrap().clone();
    let src2_bitmap = scene.bitmap(second).unwrap().clone();
    let mut cpu = BitmapState::new(width, height, vec![0u8; (width * height * 4) as usize]);
    trans
        .apply(&mut cpu, &src1_bitmap, &src2_bitmap, (0, 0, 2, 1))
        .expect("cpu blend");

    let bound = 255.0 * t * (1.0 - 128.0 / 255.0) + 1.0;
    for (index, (gpu, expected)) in composited
        .as_chunks::<4>()
        .0
        .iter()
        .zip(cpu.rgba.as_chunks::<4>().0.iter())
        .enumerate()
    {
        for (channel, (gpu, expected)) in gpu.iter().zip(expected).enumerate() {
            let difference = (i32::from(*gpu) - i32::from(*expected)).abs();
            assert!(
                f64::from(difference) <= bound,
                "pixel {index}, channel {channel}: {gpu} vs {expected} differs by \
                 {difference}, bound {bound}"
            );
        }
    }
}

/// A composited transition reaches the engine's frame as two source draws: the
/// projection emits them, the pipeline draws them, and no display bitmap is
/// created or uploaded while the fade advances.
///
/// The reference blends the two sources into the destination's frame every tick
/// (`reference/cpp/core/visual/TransIntf.cpp:592-600`); the composited form
/// carries the same two sources and the phase as the fade source's alpha.
#[test]
fn an_engine_draws_a_composited_transition_without_a_display_bitmap() {
    let (mut engine, _projections) = headless_engine_with_projection_count((32, 32));
    engine.scene_mut().composite_transition_frames = true;
    let window = engine.scene_mut().add_window("game", (32, 32));
    let (layer, src1) = drawable_layer(&mut engine, window, 16, 16);
    {
        let bitmap = engine.scene_mut().bitmap_mut(src1).expect("source 1");
        bitmap.rgba.fill(180);
        bitmap.mark_dirty();
    }
    let src2 = engine
        .scene_mut()
        .add_bitmap(16, 16, vec![255u8; 4 * 16 * 16]);
    {
        let bitmap = engine.scene_mut().bitmap_mut(src2).expect("source 2");
        bitmap.rgba.fill(255);
        bitmap.mark_dirty();
    }
    let mut hooks = NoHooks;

    let mut last_alpha = 0.0f32;
    for tick in 1..=4u32 {
        let alpha = tick as f32 / 8.0;
        engine.scene_mut().set_transition_frame(
            layer,
            Some(TransitionFrame::new(
                (16, 16),
                true,
                TransitionSource::new(src1, Rect::new(0, 0, 16, 16), Rect::new(0, 0, 16, 16), 1.0),
                TransitionSource::new(
                    src2,
                    Rect::new(0, 0, 16, 16),
                    Rect::new(0, 0, 16, 16),
                    alpha,
                ),
            )),
        );
        engine
            .frame(u64::from(tick) * 16, &mut hooks)
            .expect("frame");
        let nodes = projected_layer_nodes(&engine);
        assert_eq!(nodes.len(), 2, "tick {tick}: the frame's two sources draw");
        let fade = nodes[1].opacity;
        assert!(
            (fade - alpha).abs() < 1e-6,
            "tick {tick}: the fade source's alpha reached the node: {fade} vs {alpha}"
        );
        assert!(fade > last_alpha, "tick {tick}: the fade advances");
        last_alpha = fade;
        assert_eq!(
            engine.textures().len(),
            2,
            "tick {tick}: only the two source bitmaps carry a texture"
        );
    }
    let state = engine.scene().layer(layer).expect("layer");
    assert!(
        state.cache_bitmap.is_none() && state.transition_self.is_none(),
        "a composited frame allocates no display bitmap"
    );
}
