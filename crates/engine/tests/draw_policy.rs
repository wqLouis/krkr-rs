//! Integration tests for the subtree draw policy: the GPU draws a subtree as
//! a whole when it can draw every node in it, and the CPU composites the
//! subtrees it cannot.
//!
//! The policy reads four things: the blend modes of the layers in the
//! subtree, the clip kinds the projection composes for them, the backend's
//! capabilities and the render layer's "this mode reads the destination"
//! predicate ([`render::blend::needs_destination`]).

use api::blend::{BlendMode, DstBlend};
use engine::Rect;
use engine::draw_policy::{composite_cpu_subtrees, cpu_composite_roots};
use engine::projection::project_scene_report;
use engine::scene::Scene;
use render::PipelineConfig;
use render::blend::PhotoshopBlend;
use render::{
    RenderContent, RenderNode, RenderScene, Texture, TextureDesc, create_render_pipeline,
    create_texture,
};

/// The destination colour of the composite: the parent layer's own image.
const DESTINATION: [u8; 4] = [200, 100, 50, 128];
/// The source colour of the composite: the child layer's own image.
const SOURCE: [u8; 4] = [64, 128, 192, 255];

/// A texture handle for the projection's bitmap lookup.
fn null_texture() -> Texture {
    let pipeline = create_render_pipeline(PipelineConfig::null()).expect("null pipeline");
    create_texture(&pipeline, TextureDesc::new(2, 2)).expect("null texture")
}

/// A `width` x `height` bitmap filled with one straight-alpha colour.
fn solid_bitmap(scene: &mut Scene, width: u32, height: u32, color: [u8; 4]) -> u32 {
    let mut rgba = Vec::new();
    for _ in 0..width * height {
        rgba.extend_from_slice(&color);
    }
    scene.add_bitmap(width, height, rgba)
}

/// A window with a parent layer and a child layer over it.
///
/// The child's blend reads the destination, so the child is the node the
/// backend cannot draw when it has no copy of the destination.
fn scene_with_a_destination_dependent_child() -> (Scene, u32, u32) {
    let mut scene = Scene::default();
    let window = scene.add_window("t", (2, 2));
    let parent = scene.add_layer(window, None);
    let parent_bitmap = solid_bitmap(&mut scene, 2, 2, DESTINATION);
    {
        let layer = scene.layer_mut(parent).expect("the parent layer");
        layer.rect = Rect::new(0, 0, 2, 2);
        layer.visible = true;
        layer.bitmap = Some(parent_bitmap);
    }
    let child = scene.add_layer(window, Some(parent));
    let child_bitmap = solid_bitmap(&mut scene, 2, 2, SOURCE);
    {
        let layer = scene.layer_mut(child).expect("the child layer");
        layer.rect = Rect::new(0, 0, 2, 2);
        layer.visible = true;
        layer.bitmap = Some(child_bitmap);
        layer.blend = BlendMode::DestinationDependent(DstBlend::Overlay);
    }
    (scene, parent, child)
}

/// The nodes of a projection that draw something.
fn drawable_nodes(scene: &RenderScene) -> Vec<&RenderNode> {
    scene
        .nodes
        .iter()
        .filter(|node| !matches!(node.content, RenderContent::None))
        .collect()
}

/// A subtree whose node reads the destination is composited on the CPU when
/// the backend cannot draw that mode, and the projection draws the composite
/// as one texture in place of the layer and its descendants.
///
/// The composited pixels are the shared table's overlay blend, which is what
/// the GPU's destination-dependent program computes: the program is generated
/// from that table
/// (`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:526-547`,
/// `reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-660`).
#[test]
fn a_subtree_that_needs_the_destination_is_composited_on_the_cpu() {
    let (mut scene, parent, _child) = scene_with_a_destination_dependent_child();
    // A backend that cannot draw a mode that reads the destination.
    scene.gpu_draws_destination_dependent_blends = false;

    // The subtree is rooted at the parent: the composite is drawn with the
    // parent's own blend, which the backend can draw.
    assert_eq!(cpu_composite_roots(&scene), vec![parent]);

    let mut registered = Vec::new();
    composite_cpu_subtrees(&mut scene, &mut registered);
    assert_eq!(registered, vec![parent]);

    // The parent draws its composited subtree.
    let content = scene
        .layer(parent)
        .expect("the parent layer")
        .cache_bitmap
        .expect("the composited subtree");
    let bitmap = scene.bitmap(content).expect("the composite bitmap");
    // The overlay of the source over the destination: the table's entry for
    // the overlay is the reference's overlay table
    // (`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:526-547`), and
    // the source's alpha mixes the result into the destination's, which
    // keeps its alpha
    // (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-383`).
    let expected = PhotoshopBlend::OVERLAY.blend_pixel(DESTINATION, SOURCE, 255);
    assert_eq!(&bitmap.rgba[..4], &expected[..]);
    assert_eq!(&bitmap.rgba[..4], &[173, 100, 74, 128][..]);

    // The projection draws the subtree as one node: the composite is drawn
    // with the parent's own blend, which the backend can draw.
    let texture = null_texture();
    let report = project_scene_report(&scene, |_| Some(texture.clone()));
    let nodes = drawable_nodes(&report.scene);
    assert_eq!(nodes.len(), 1, "the subtree is drawn as one node");
    assert_eq!(nodes[0].blend, BlendMode::SourceOver);
}

/// A backend that can draw a mode that reads the destination draws the
/// subtree node by node, and nothing is composited on the CPU.
#[test]
fn a_backend_that_can_draw_the_destination_draws_the_subtree_node_by_node() {
    let (mut scene, parent, child) = scene_with_a_destination_dependent_child();
    scene.gpu_draws_destination_dependent_blends = true;

    assert!(cpu_composite_roots(&scene).is_empty());

    let mut registered = Vec::new();
    composite_cpu_subtrees(&mut scene, &mut registered);
    assert!(registered.is_empty());
    assert!(
        scene
            .layer(parent)
            .expect("the parent layer")
            .cache_bitmap
            .is_none(),
        "the parent draws its own image"
    );

    // Both layers draw their own node.
    let texture = null_texture();
    let report = project_scene_report(&scene, |_| Some(texture.clone()));
    assert_eq!(drawable_nodes(&report.scene).len(), 2);
    assert!(report.drawn_layers.contains(&child));
}

/// A layer that is no longer a root gives its cache up, and the projection
/// draws its own image again.
#[test]
fn a_root_that_stops_needing_the_cpu_gives_its_cache_up() {
    let (mut scene, parent, child) = scene_with_a_destination_dependent_child();
    scene.gpu_draws_destination_dependent_blends = false;

    let mut registered = Vec::new();
    composite_cpu_subtrees(&mut scene, &mut registered);
    assert_eq!(registered, vec![parent]);

    // The child's blend becomes one the backend can draw.
    scene.layer_mut(child).expect("the child layer").blend = BlendMode::SourceOver;
    composite_cpu_subtrees(&mut scene, &mut registered);
    assert!(registered.is_empty());
    assert!(
        scene
            .layer(parent)
            .expect("the parent layer")
            .cache_bitmap
            .is_none(),
        "the parent draws its own image again"
    );

    let texture = null_texture();
    let report = project_scene_report(&scene, |_| Some(texture.clone()));
    assert_eq!(drawable_nodes(&report.scene).len(), 2);
}
