//! Integration tests for the L2 → L1 render projection.
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/projection.rs`. The
//! private helpers used there are reimplemented here on top of the public
//! `render` API.

use std::cell::Cell;
use std::rc::Rc;

use api::blend::{BlendMode, DstBlend};
use api::geometry::{PixelRegion, RectF};
use engine::{
    Engine, EngineOverlay, EngineParts, GameFrame, Rect, Scene, compose_layer, project_scene,
    project_window, project_window_report, resolve_blend,
};
use render::{
    PipelineConfig, RenderContent, RenderNode, RenderNodeId, RenderPipeline, RenderScene,
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
