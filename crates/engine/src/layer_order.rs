//! The paint-order index of the layer tree.
//!
//! The scene keeps one index of the order the layers of a window are drawn
//! in. The index is built when a window's order is asked for and reused
//! until the scene's structure moves, so a frame that walks the layer tree
//! many times — once per cached layer, once per hit test — pays for one
//! build instead of one per walk.
//!
//! The index holds the order alone. The geometry a walk needs — a layer's
//! absolute offset, its clip rectangle, the visibility of its chain — is
//! derived from the live scene while the walk runs, so a layer that moved
//! is picked up without rebuilding anything.

use std::cell::Cell;

/// The paint order of the layers of every window.
///
/// One entry per window: the layer ids in the order they are drawn, back to
/// front, together with the scene revision the order was built from. An
/// entry whose revision no longer matches is stale, and the next reader
/// rebuilds it.
#[derive(Debug, Default)]
pub(crate) struct LayerOrderIndex {
    /// The windows' orders: the window id, the revision the order was built
    /// from and the order itself.
    orders: Vec<(u32, u64, Vec<u32>)>,
    /// How many orders the index has built.
    ///
    /// A test reads the counter to see that a repeated walk reuses the index
    /// instead of rebuilding the order.
    builds: Cell<usize>,
}

impl LayerOrderIndex {
    /// The window's paint order, when it was built from `revision`.
    pub(crate) fn get(&self, window: u32, revision: u64) -> Option<&[u32]> {
        self.orders
            .iter()
            .find(|(id, built, _)| *id == window && *built == revision)
            .map(|(_, _, order)| order.as_slice())
    }

    /// Record the window's paint order, built from `revision`.
    pub(crate) fn set(&mut self, window: u32, revision: u64, order: Vec<u32>) {
        let position = self.orders.iter().position(|(id, _, _)| *id == window);
        match position {
            Some(index) => self.orders[index] = (window, revision, order),
            None => self.orders.push((window, revision, order)),
        }
        self.builds.set(self.builds.get() + 1);
    }

    /// How many orders the index has built.
    #[cfg(test)]
    pub(crate) fn builds(&self) -> usize {
        self.builds.get()
    }
}

#[cfg(test)]
mod tests {
    use crate::layer_ops::render_layer_subtree;
    use crate::scene::{Rect, Scene};

    /// A window with three hundred stacked layers under one root.
    ///
    /// The root is the window's primary layer; every child covers the same
    /// rectangle, is visible and accepts the point.
    fn stacked_scene() -> (Scene, u32, u32, Vec<u32>) {
        let mut scene = Scene::default();
        let win = scene.add_window("t", (640, 480));
        let root = scene.add_layer(win, None);
        let mut children = Vec::new();
        for _ in 0..300u32 {
            let child = scene.add_layer(win, Some(root));
            {
                let layer = scene.layer_mut(child).expect("the new layer");
                layer.rect = Rect::new(0, 0, 320, 240);
                layer.visible = true;
                layer.hit_threshold = 0;
            }
            children.push(child);
        }
        (scene, win, root, children)
    }

    /// The index is built once and reused: the composite and the hit test
    /// read the order the index already holds.
    ///
    /// The reference keeps the overall-order index on the layer manager and
    /// rebuilds it only when the tree changes
    /// (`reference/krkr2/cpp/core/visual/LayerManager.cpp:204-224`).
    #[test]
    fn the_index_serves_the_composite_and_the_hit_test_from_one_build() {
        let (mut scene, win, root, children) = stacked_scene();
        // The first read builds the order.
        let order = scene.window_layer_order(win);
        assert_eq!(order.len(), 301);
        assert_eq!(scene.layer_order_builds(), 1);
        // Every later read reuses it.
        for _ in 0..16 {
            assert_eq!(scene.window_layer_order(win).len(), 301);
        }
        assert_eq!(scene.layer_order_builds(), 1);
        // The hit test reads the same order: the frontmost layer that
        // accepts the point wins.
        let hit = scene
            .layer_hit_next(root, 1, 1, false, None)
            .expect("the point hits a layer");
        assert_eq!(hit.layer, children[299]);
        assert_eq!((hit.x, hit.y), (1, 1));
        assert_eq!(scene.layer_order_builds(), 1);
        // The composite reads the order once and hands it down: the nested
        // cache walk does not read it again for every layer.
        let content = render_layer_subtree(&mut scene, root).expect("the content");
        assert!(scene.bitmap(content).is_some());
        let builds = scene.layer_order_builds();
        // The content exists now, so the second composite reuses it and
        // reads the order the index already holds.
        let again = render_layer_subtree(&mut scene, root).expect("the content");
        assert_eq!(again, content);
        assert_eq!(scene.layer_order_builds(), builds);
    }

    /// A change to the tree rebuilds the order.
    #[test]
    fn a_change_to_the_tree_rebuilds_the_order() {
        let (mut scene, win, _root, children) = stacked_scene();
        assert_eq!(scene.window_layer_order(win).len(), 301);
        assert_eq!(scene.layer_order_builds(), 1);
        let extra = scene.add_layer(win, Some(children[0]));
        let order = scene.window_layer_order(win);
        assert_eq!(order.len(), 302);
        assert!(order.contains(&extra));
        assert_eq!(scene.layer_order_builds(), 2);
    }

    /// What the index saves: the walk that rebuilt the order for every
    /// layer it visited paid for one build per layer, the indexed walk pays
    /// for one build in total.
    ///
    /// The test is ignored by default: it measures the wall clock, and the
    /// gate runs it only when the measurement is wanted.
    #[test]
    #[ignore]
    fn the_index_saves_the_rebuild_of_every_layer() {
        let (mut scene, win, root, _children) = stacked_scene();
        // The composite warms the content, so the measurement covers the
        // order and not the first allocation.
        let _ = render_layer_subtree(&mut scene, root);
        let layers = scene.layers.len();
        let start = std::time::Instant::now();
        for _ in 0..layers {
            let _ = scene.build_window_layer_order(win);
        }
        let rebuild = start.elapsed();
        let start = std::time::Instant::now();
        for _ in 0..layers {
            let _ = scene.window_layer_order(win);
        }
        let cached = start.elapsed();
        let factor = rebuild.as_secs_f64() / cached.as_secs_f64().max(f64::EPSILON);
        println!(
            "the order of {layers} layers: rebuild {rebuild:?}, cached {cached:?}, factor {factor:.1}"
        );
    }
}
