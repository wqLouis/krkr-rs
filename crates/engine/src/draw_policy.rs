//! The subtree draw policy: the GPU draws a subtree as a whole when it can
//! draw every node in it, and the CPU composites the subtrees it cannot.
//!
//! The GPU is the primary path. A node whose blend mode reads the
//! destination needs a copy of the destination's pixels, and a backend that
//! cannot supply one cannot draw that node at all. The CPU compositor reads
//! the destination for every pixel, so it draws such a mode exactly, and the
//! composited subtree is handed to the GPU as one texture.
//!
//! The decision is made once per subtree, never per node. A subtree's nodes
//! blend against each other — the later node's mode is evaluated over the
//! pixels the earlier one wrote — so a subtree split between the two paths
//! would break that order, and the CPU half would need the copy of the
//! destination the GPU holds.
//!
//! A subtree is composited at the nearest ancestor whose own blend the
//! backend can draw: the composited subtree is drawn with that ancestor's
//! blend, so the ancestor's node is the one the GPU draws.

use api::blend::BlendMode;
use render::blend::needs_destination;

use crate::layer_ops::render_layer_subtree;
use crate::projection::{compose_layer, composed_clip};
use crate::scene::{LayerState, Scene};

/// Whether the backend can draw a node with this blend mode.
///
/// A mode that reads the destination needs a copy of the destination's
/// pixels; the backend keeps such a copy only where it can, and
/// [`Scene::gpu_draws_destination_dependent_blends`] says whether it can.
fn gpu_can_draw(scene: &Scene, blend: BlendMode) -> bool {
    !needs_destination(blend) || scene.gpu_draws_destination_dependent_blends
}

/// The subtrees the backend cannot draw, with the layer each one is rooted at.
///
/// A layer whose own blend the backend cannot draw is drawn by a node the
/// backend refuses; the subtree that holds the node is composited on the
/// CPU instead. The composited subtree is drawn with the blend of the layer
/// it is rooted at, so the root is the nearest ancestor whose own blend the
/// backend can draw.
pub fn cpu_composite_roots(scene: &Scene) -> Vec<u32> {
    let mut roots: Vec<u32> = Vec::new();
    for layer in &scene.layers {
        if gpu_can_draw(scene, layer.blend) {
            continue;
        }
        // A layer that is not drawn draws no subtree.
        if !layer.visible || layer.opacity == 0.0 {
            continue;
        }
        // A subtree that is already composited holds this layer's pixels.
        if inside_root(scene, layer, &roots) {
            continue;
        }
        let Some(root) = fallback_root(scene, layer) else {
            continue;
        };
        // The CPU takes the subtree when every layer in it has a clip it can
        // draw and no transition owns it.
        if !cpu_can_take_subtree(scene, root) {
            continue;
        }
        roots.push(root);
    }
    roots
}

/// The layer the subtree of `layer` is composited at: the nearest ancestor
/// whose own blend the backend can draw.
///
/// The composited subtree is drawn with the root's blend, so the root must
/// be a layer the backend can draw. A layer with no such ancestor has no
/// root: the backend draws its own node.
fn fallback_root(scene: &Scene, layer: &LayerState) -> Option<u32> {
    let mut current = layer.parent;
    let mut depth = 0usize;
    while let Some(id) = current {
        depth += 1;
        if depth > scene.layers.len() {
            break;
        }
        let Some(ancestor) = scene.layer(id) else {
            break;
        };
        if ancestor.visible && ancestor.opacity != 0.0 && gpu_can_draw(scene, ancestor.blend) {
            return Some(id);
        }
        current = ancestor.parent;
    }
    None
}

/// Whether the layer is inside one of the composited subtrees.
fn inside_root(scene: &Scene, layer: &LayerState, roots: &[u32]) -> bool {
    let mut current = layer.parent;
    let mut depth = 0usize;
    while let Some(id) = current {
        depth += 1;
        if depth > scene.layers.len() {
            break;
        }
        if roots.contains(&id) {
            return true;
        }
        let Some(ancestor) = scene.layer(id) else {
            break;
        };
        current = ancestor.parent;
    }
    false
}

/// Whether the CPU can take the subtree under `root`.
///
/// The CPU compositor clips every draw with the layer's own rectangle, so it
/// draws the subtree when every layer in it has a clip it can draw. A layer
/// that runs a transition owns its own subtree: the transition draws its
/// frame, its display bitmap or its custom shader in the layer's place, so
/// the subtree is not drawn node by node and the policy leaves it alone.
fn cpu_can_take_subtree(scene: &Scene, root: u32) -> bool {
    let mut stack = vec![root];
    let mut visited = 0usize;
    while let Some(id) = stack.pop() {
        visited += 1;
        if visited > scene.layers.len() {
            break;
        }
        let Some(layer) = scene.layer(id) else {
            continue;
        };
        if layer.transition_self.is_some()
            || layer.transition_frame.is_some()
            || layer.effect.is_some()
        {
            return false;
        }
        let Some(composed) = compose_layer(scene, id) else {
            continue;
        };
        if composed_clip(&composed).is_some_and(|clip| !clip.is_drawable()) {
            return false;
        }
        stack.extend(layer.children.iter().copied());
    }
    true
}

/// Composites on the CPU the subtrees the backend cannot draw.
///
/// Each subtree is composited into the root layer's cache bitmap, and the
/// layer becomes a consumer of that cache, so the projection draws the
/// composited subtree in place of the layer's own image and its descendants.
///
/// `registered` holds the layers this function has made cache consumers. A
/// layer that is no longer a root gives the cache up, and its composited
/// content is released with it.
pub fn composite_cpu_subtrees(scene: &mut Scene, registered: &mut Vec<u32>) {
    let roots = cpu_composite_roots(scene);
    // A layer that is no longer a root gives its cache up.
    let released: Vec<u32> = registered
        .iter()
        .copied()
        .filter(|root| !roots.contains(root))
        .collect();
    for root in released {
        scene.dec_cache_enabled_count(root);
        registered.retain(|id| *id != root);
    }
    for root in roots {
        if !registered.contains(&root) {
            scene.inc_cache_enabled_count(root);
            registered.push(root);
        }
        let Some(content) = render_layer_subtree(scene, root) else {
            continue;
        };
        // The composited content is the layer's cache content; a trivial
        // subtree's content is the layer's own image, which the projection
        // draws with the layer's own placement.
        if scene.layer(root).and_then(|layer| layer.cache_content) == Some(content) {
            scene.set_cache_bitmap(root, Some(content));
        }
    }
}
