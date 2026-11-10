//! Integration tests for the paint-order index: the order the layers are
//! drawn in, the pointer hit test and the cached composite.
//!
//! The reference keeps an overall-order index on the layer manager and
//! rebuilds it only when the tree changes
//! (`reference/krkr2/cpp/core/visual/LayerManager.cpp:204-224`); the order
//! itself is the depth-first pre-order over the maintained children lists
//! (`reference/krkr2/cpp/core/visual/LayerIntf.cpp:741-750`). The hit test
//! takes the frontmost layer that accepts the point
//! (`reference/krkr2/cpp/core/visual/LayerIntf.cpp:3312-3345`).

use api::blend::BlendMode;
use engine::layer_ops::render_layer_subtree;
use engine::scene::HitCandidate;
use engine::{Rect, Scene};

/// A window whose root holds three hundred children, in creation order.
#[test]
fn the_paint_order_of_a_hundred_layers_is_the_creation_order() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (640, 480));
    let root = scene.add_layer(win, None);
    let mut children = Vec::new();
    for index in 0..300u32 {
        let child = scene.add_layer(win, Some(root));
        {
            let layer = scene.layer_mut(child).expect("the new layer");
            layer.rect = Rect::new((index % 20) as i32 * 16, (index / 20) as i32 * 16, 16, 16);
            layer.visible = true;
        }
        children.push(child);
    }
    let mut expected = vec![root];
    expected.extend(children.iter().copied());
    assert_eq!(scene.window_layer_order(win), expected);
}

/// The hit test takes the frontmost layer that accepts the point, and the
/// search behind the layer the caller tested last continues below it.
///
/// The reference's `GetMostFrontChildAt` walks the children from the front
/// and stops at the first layer that accepts the point
/// (`reference/krkr2/cpp/core/visual/LayerIntf.cpp:3312-3345`); a layer that
/// is not visible cannot be hit (`LayerIntf.cpp:3319-3321`).
#[test]
fn the_hit_test_takes_the_frontmost_layer_that_accepts_the_point() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (640, 480));
    let root = scene.add_layer(win, None);
    let container = scene.add_layer(win, Some(root));
    let inner = scene.add_layer(win, Some(container));
    let front = scene.add_layer(win, Some(root));
    {
        let layer = scene.layer_mut(container).expect("the container");
        layer.rect = Rect::new(10, 10, 100, 100);
        layer.visible = true;
        layer.hit_threshold = 0;
    }
    {
        let layer = scene.layer_mut(inner).expect("the inner layer");
        layer.rect = Rect::new(5, 5, 50, 50);
        layer.visible = true;
        layer.hit_threshold = 0;
    }
    {
        let layer = scene.layer_mut(front).expect("the front layer");
        layer.rect = Rect::new(0, 0, 640, 480);
        layer.visible = true;
        layer.hit_threshold = 0;
    }
    // The point is given in the inner layer's own coordinates; it lands
    // inside every layer of the tree.
    let hit = scene
        .layer_hit_next(inner, 20, 20, false, None)
        .expect("the point hits a layer");
    assert_eq!(
        hit,
        HitCandidate {
            layer: front,
            x: 35,
            y: 35
        }
    );
    // The search continues behind the layer the caller tested last.
    let behind = scene
        .layer_hit_next(inner, 20, 20, false, Some(front))
        .expect("the point hits a layer");
    assert_eq!(
        behind,
        HitCandidate {
            layer: inner,
            x: 20,
            y: 20
        }
    );
    // The layer the search starts at is not a candidate of its own.
    let excluded = scene
        .layer_hit_next(inner, 20, 20, true, None)
        .expect("the point hits a layer");
    assert_eq!(excluded.layer, front);
    // A hidden layer cannot be hit, and neither can a layer below it.
    scene.layer_mut(front).expect("the front layer").visible = false;
    let hidden = scene
        .layer_hit_next(inner, 20, 20, false, None)
        .expect("the point hits a layer");
    assert_eq!(
        hidden,
        HitCandidate {
            layer: inner,
            x: 20,
            y: 20
        }
    );
    scene.layer_mut(container).expect("the container").visible = false;
    let pruned = scene
        .layer_hit_next(inner, 20, 20, false, None)
        .expect("the point hits a layer");
    assert_eq!(
        pruned,
        HitCandidate {
            layer: root,
            x: 35,
            y: 35
        }
    );
}

/// The composite of a cached layer draws its subtree into its own bitmap and
/// keeps it.
///
/// The reference's layer draws its children into the parent's buffer and
/// caches the result while the layer is cached
/// (`reference/krkr2/cpp/core/visual/LayerIntf.cpp:3975-3979`).
#[test]
fn the_cached_composite_draws_the_subtree_and_keeps_it() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let parent = scene.add_layer(win, None);
    let parent_bitmap = scene.add_bitmap(
        2,
        2,
        vec![
            255, 0, 0, 128, 255, 0, 0, 128, 255, 0, 0, 128, 255, 0, 0, 128,
        ],
    );
    {
        let layer = scene.layer_mut(parent).expect("the parent");
        layer.rect = Rect::new(0, 0, 2, 2);
        layer.visible = true;
        layer.bitmap = Some(parent_bitmap);
    }
    let child = scene.add_layer(win, Some(parent));
    let child_bitmap = scene.add_bitmap(
        2,
        2,
        vec![
            0, 255, 0, 128, 0, 255, 0, 128, 0, 255, 0, 128, 0, 255, 0, 128,
        ],
    );
    {
        let layer = scene.layer_mut(child).expect("the child");
        layer.rect = Rect::new(0, 0, 2, 2);
        layer.visible = true;
        layer.bitmap = Some(child_bitmap);
        layer.blend = BlendMode::PsNormal;
    }
    // The parent is a cached layer: its subtree is composited into its own
    // bitmap.
    scene.inc_cache_enabled_count(parent);
    let content = render_layer_subtree(&mut scene, parent).expect("the cache bitmap");
    let bitmap = scene.bitmap(content).expect("the cache bitmap");
    // The child's green over the parent's red, with the alpha the normal
    // mode holds: `255 + ((0 - 255) * 128 >> 8) = 127` and
    // `0 + ((255 - 0) * 128 >> 8) = 127`.
    assert_eq!(&bitmap.rgba[..4], &[127, 127, 0, 128]);
    // The second composite keeps the content it wrote.
    let again = render_layer_subtree(&mut scene, parent).expect("the cache bitmap");
    assert_eq!(again, content);
    assert_eq!(
        &scene.bitmap(again).expect("the cache bitmap").rgba[..4],
        &[127, 127, 0, 128]
    );
}
