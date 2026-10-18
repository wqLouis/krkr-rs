//! Integration tests for the logical scene graph (`Scene`).
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/scene.rs`.

use engine::Scene;

/// The real title scene's stack z-values (system_status.tjs:
/// LAYER_LOGO=110000, LAYER_COVER=150000, LAYER_HINT=210000; content
/// layers at 0). The game's own comment "数字が大きいほど手前" (larger =
/// closer/front) and the reference's AbsoluteOrderIndex semantics (higher
/// index draws later = in front) mean window_layer_order must return
/// ascending z, back→front, with insertion order only breaking ties.
#[test]
fn window_layer_order_real_title_z_values() {
    let mut scene = Scene::default();
    let win = scene.add_window("title", (1280, 720));
    // Insertion order deliberately disagrees with z-order (HINT created
    // first in MainWindow ctor, then LOGO, then z=0 content).
    let hint = scene.add_layer(win, None);
    let logo = scene.add_layer(win, None);
    let bg = scene.add_layer(win, None);
    let cover = scene.add_layer(win, None);
    let art = scene.add_layer(win, None);
    scene.layer_mut(hint).unwrap().z_order = 210000;
    scene.layer_mut(logo).unwrap().z_order = 110000;
    scene.layer_mut(cover).unwrap().z_order = 150000;
    // bg + art keep z = 0.

    let order = scene.window_layer_order(win);
    let pos = |id: u32| order.iter().position(|&x| x == id).unwrap();
    assert_eq!(order.len(), 5);
    assert!(pos(bg) < pos(art), "equal z keeps insertion order");
    assert!(
        pos(art) < pos(logo),
        "z=0 renders behind LAYER_LOGO (110000)"
    );
    assert!(pos(logo) < pos(cover), "110000 renders behind 150000");
    assert!(pos(cover) < pos(hint), "150000 renders behind 210000");
    assert_eq!(pos(hint), order.len() - 1, "LAYER_HINT is frontmost");
}

/// A parent is emitted before its children, and the complete child
/// subtree remains together before the next root sibling.
#[test]
fn window_layer_order_flattens_hierarchy() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1280, 720));
    let parent = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(parent));
    let sibling = scene.add_layer(win, None);
    let order = scene.window_layer_order(win);
    assert_eq!(order, vec![parent, child, sibling]);
}

#[test]
fn moving_layer_to_front_updates_equal_z_sibling_order() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let back = scene.add_layer(win, None);
    let front = scene.add_layer(win, None);
    scene.layer_move_to_front(back);
    assert_eq!(scene.window_layer_order(win), vec![front, back]);
}

/// Deterministic back→front order across z-order, insertion tie-breaks
/// and a nested subtree. This is the reference order the O(n log n)
/// rewrite must keep matching.
#[test]
fn window_layer_order_complex_hierarchy_matches_reference() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let a = scene.add_layer(win, None);
    let b = scene.add_layer(win, None);
    let c = scene.add_layer(win, None);
    scene.layer_mut(a).unwrap().z_order = 5;
    scene.layer_mut(b).unwrap().z_order = 1;
    // c keeps z = 0.
    let b1 = scene.add_layer(win, Some(b));
    let b2 = scene.add_layer(win, Some(b));
    scene.layer_mut(b2).unwrap().z_order = -1;
    let b1c = scene.add_layer(win, Some(b1));

    // Roots by (z, insertion, id): c(0), b(1), a(5).
    // b's children by (z, insertion, id): b2(-1), b1(0); b1c follows b1.
    assert_eq!(scene.window_layer_order(win), vec![c, b, b2, b1, b1c, a]);
}

/// A child that exists in the layer table but is missing from its
/// parent's `children` vector must still render (found by the parent
/// index) and stay in the same relative position.
#[test]
fn window_layer_order_finds_children_missing_from_parent_list() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let parent = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(parent));
    scene.layer_mut(parent).unwrap().children.clear();
    assert_eq!(scene.window_layer_order(win), vec![parent, child]);
}

/// A layer the window does not own is **not** drawn, whatever its parent
/// link looks like. The reference renders exactly the tree reachable from
/// the window's `Primary` layer (`LayerManager.cpp:208`), so a layer outside
/// that tree is invisible and not hit-tested.
///
/// This used to be a "defensive" pass that appended every unvisited layer of
/// the window, on the theory that a stale parent link should not hide a
/// layer. In practice it silently defeated `Part()` and `invalidate`: a
/// detached (invalidated) subtree was re-added to the draw order every
/// frame, which is what kept a settings page on screen after leaving it.
#[test]
fn window_layer_order_skips_layers_the_window_does_not_own() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let owned = scene.add_layer(win, None);
    let orphan = scene.add_layer(win, None);
    // The window no longer owns `orphan`, as after a `Part()`.
    scene
        .window_mut(win)
        .unwrap()
        .layers
        .retain(|&id| id != orphan);
    scene.layer_mut(orphan).unwrap().parent = Some(9999);
    assert_eq!(scene.window_layer_order(win), vec![owned]);
    // But a child of an owned layer is still reached.
    let child = scene.add_layer(win, Some(owned));
    assert!(scene.window_layer_order(win).contains(&child));
}

/// The `*_mut` lookups are index-accelerated but must remain correct once
/// a removal shifts the underlying vectors.
#[test]
fn id_lookups_stay_correct_after_layer_removal() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let a = scene.add_layer(win, None);
    let b = scene.add_layer(win, None);
    let c = scene.add_layer(win, None);
    scene.remove_layer(b);
    assert_eq!(scene.layer(a).unwrap().id, a);
    assert_eq!(scene.layer(c).unwrap().id, c);
    assert!(scene.layer(b).is_none());

    let bitmap = scene.add_bitmap(1, 1, vec![0; 4]);
    let font = scene.add_font("x".into(), 12, [0; 4]);
    assert_eq!(scene.bitmap(bitmap).unwrap().id, bitmap);
    assert_eq!(scene.font(font).unwrap().id, font);
    assert_eq!(scene.window(win).unwrap().id, win);
}

/// `remove_layer` is `Part()`: it severs the layer from its parent (or the
/// window roots) and severs its direct children from it, but destroys
/// nothing. A severed layer stops being **rendered and hit-tested**: the
/// reference draws exactly the tree reachable from the window's `Primary`
/// layer (`tTVPLayerManager::RecreateOverallOrderIndex`,
/// `LayerManager.cpp:208`) and `NotifyPart` invalidates that index. So a
/// `Part()`ed child is invisible until something re-parents it — it is not
/// promoted to a window-level layer.
#[test]
fn remove_layer_detaches_children_and_stops_rendering_them() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let parent = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(parent));
    let grandchild = scene.add_layer(win, Some(child));
    scene.remove_layer(parent);
    assert!(scene.layer(parent).is_none());
    assert!(scene.layer(child).is_some(), "child survives Part()");
    assert!(scene.layer(grandchild).is_some(), "grandchild survives");
    assert_eq!(scene.layer(child).unwrap().parent, None);
    assert_eq!(scene.layer(grandchild).unwrap().parent, Some(child));
    // Detached from the window's roots, so neither is drawn any more.
    let order = scene.window_layer_order(win);
    assert!(
        !order.contains(&child),
        "a Part()ed child is not rendered as a window root"
    );
    assert!(!order.contains(&grandchild));
    // Re-parenting it to a window-level layer restores it: the subtree was
    // detached, not destroyed.
    let other = scene.add_layer(win, None);
    scene.layer_mut(child).unwrap().parent = Some(other);
    scene.layer_mut(other).unwrap().children.push(child);
    let order = scene.window_layer_order(win);
    assert!(order.contains(&child), "re-parented layer renders again");
    assert!(order.contains(&grandchild), "with its subtree intact");
}

/// `destroy_layer` models the reference `Invalidate` + GC cascade: the
/// whole subtree is removed, so no descendant survives as a window root.
/// This is the fix for a torn-down scene's images staying on screen.
#[test]
fn destroy_layer_removes_whole_subtree() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let parent = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(parent));
    let grandchild = scene.add_layer(win, Some(child));
    let unrelated = scene.add_layer(win, None);
    scene.destroy_layer(parent);
    assert!(scene.layer(parent).is_none());
    assert!(scene.layer(child).is_none(), "child destroyed with parent");
    assert!(
        scene.layer(grandchild).is_none(),
        "grandchild destroyed with parent"
    );
    assert!(scene.layer(unrelated).is_some(), "unrelated layer survives");
    // None of the destroyed layers may reach the renderer.
    let order = scene.window_layer_order(win);
    assert_eq!(order, vec![unrelated]);
}

/// `Invalidate` severs the parent and each direct child, but it does not
/// destroy the child records: their script objects still have to finalize.
/// The detached children are no longer reachable from the window's draw order.
#[test]
fn invalidate_layer_severs_its_children() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let parent = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(parent));
    let grandchild = scene.add_layer(win, Some(child));
    let unrelated = scene.add_layer(win, None);

    scene.invalidate_layer(parent);

    assert!(scene.layer(parent).is_none());
    assert_eq!(scene.layer(child).and_then(|l| l.parent), None);
    assert_eq!(scene.layer(grandchild).and_then(|l| l.parent), Some(child));
    assert!(scene.layer(unrelated).is_some(), "unrelated layer survives");
    assert_eq!(scene.window_layer_order(win), vec![unrelated]);
}

/// Destroying a subtree that was itself `Part()`ed out to the window root
/// list removes it from that list and from `window_layer_order`, leaving
/// the rest of the window untouched.
#[test]
fn destroy_layer_removes_detached_root_from_layer_order() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let grandpa = scene.add_layer(win, None);
    let parent = scene.add_layer(win, Some(grandpa));
    let child = scene.add_layer(win, Some(parent));
    // Part `parent` out to the window root list; its child comes along.
    scene.layer_mut(grandpa).unwrap().children.clear();
    scene.layer_mut(parent).unwrap().parent = None;
    scene.window_mut(win).unwrap().layers.push(parent);
    assert!(scene.window_layer_order(win).contains(&parent));
    scene.destroy_layer(parent);
    let order = scene.window_layer_order(win);
    assert_eq!(order, vec![grandpa]);
    assert!(
        !order.contains(&child),
        "child destroyed with the detached parent"
    );
    assert_eq!(scene.window(win).unwrap().layers, vec![grandpa]);
}

/// `destroy_layer` drops the doomed layers' lazily-allocated fonts and
/// clears a primary/focus holder that pointed into the subtree.
#[test]
fn destroy_layer_cleans_fonts_primary_and_focus() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let primary = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(primary));
    let font = scene.add_font("f".into(), 12, [0; 4]);
    scene.layer_mut(child).unwrap().font_id = Some(font);
    scene.set_focus(child);
    assert_eq!(scene.window(win).unwrap().primary_layer, Some(primary));
    assert_eq!(scene.window(win).unwrap().focused_layer, Some(child));
    scene.destroy_layer(primary);
    assert_eq!(scene.window(win).unwrap().primary_layer, None);
    assert_eq!(scene.window(win).unwrap().focused_layer, None);
    assert!(scene.font(font).is_none(), "owned font destroyed");
}

/// `destroy_layer` releases the bitmaps its subtree alone held, while a
/// bitmap still attached to a surviving layer stays (reference
/// refcounted MainImage).
#[test]
fn destroy_layer_releases_layer_owned_bitmaps_but_keeps_shared() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let parent = scene.add_layer(win, None);
    let child_solo = scene.add_layer(win, Some(parent));
    let child_shared = scene.add_layer(win, Some(parent));
    let survivor = scene.add_layer(win, None);
    let solo = scene.add_bitmap(2, 2, vec![0; 16]);
    let shared = scene.add_bitmap(2, 2, vec![1; 16]);
    scene.layer_mut(child_solo).unwrap().bitmap = Some(solo);
    scene.layer_mut(child_shared).unwrap().bitmap = Some(shared);
    scene.layer_mut(survivor).unwrap().bitmap = Some(shared);

    scene.destroy_layer(parent);

    assert!(scene.bitmap(solo).is_none(), "solo bitmap freed");
    assert!(scene.bitmap(shared).is_some(), "shared bitmap survives");
    assert!(scene.bitmap_referenced_by_layer(shared));
}

/// A whole-scene teardown releases every layer-owned bitmap — the count
/// the renderer then prunes from `Assets<Image>`. Only a bitmap still
/// referenced by a surviving layer is kept.
#[test]
fn scene_teardown_releases_all_layer_owned_bitmaps() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1280, 720));
    let root = scene.add_layer(win, None);
    let mut owned = Vec::new();
    for _ in 0..100 {
        let layer = scene.add_layer(win, Some(root));
        let bmp = scene.add_bitmap(2, 2, vec![0; 16]);
        scene.layer_mut(layer).unwrap().bitmap = Some(bmp);
        owned.push(bmp);
    }
    let shared = scene.add_bitmap(2, 2, vec![0; 16]);
    scene.layer_mut(root).unwrap().bitmap = Some(shared);
    let survivor = scene.add_layer(win, None);
    scene.layer_mut(survivor).unwrap().bitmap = Some(shared);

    scene.destroy_layer(root);

    for bmp in &owned {
        assert!(scene.bitmap(*bmp).is_none(), "bitmap {bmp} released");
    }
    assert!(scene.bitmap(shared).is_some(), "shared bitmap survives");
    assert_eq!(
        scene.collect_unused_bitmaps(),
        0,
        "every layer-owned bitmap was already released"
    );
}

/// `remove_layer` (`Part()`) also releases the removed layer's own
/// bitmap; a child kept alive by the detach keeps its own image.
#[test]
fn remove_layer_releases_its_bitmap_and_keeps_children_images() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let parent = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(parent));
    let other = scene.add_layer(win, None);
    let parent_bmp = scene.add_bitmap(2, 2, vec![0; 16]);
    let child_bmp = scene.add_bitmap(2, 2, vec![1; 16]);
    let other_bmp = scene.add_bitmap(2, 2, vec![2; 16]);
    scene.layer_mut(parent).unwrap().bitmap = Some(parent_bmp);
    scene.layer_mut(child).unwrap().bitmap = Some(child_bmp);
    scene.layer_mut(other).unwrap().bitmap = Some(other_bmp);

    scene.remove_layer(parent);

    assert!(scene.bitmap(parent_bmp).is_none(), "parent bitmap freed");
    assert!(scene.bitmap(child_bmp).is_some(), "child survives Part");
    assert!(scene.bitmap(other_bmp).is_some(), "other layer untouched");
}

/// A script `Bitmap` object keeps its pixels alive even after every
/// layer sharing the id is destroyed (the reference refcounted
/// MainImage).
#[test]
fn script_owned_bitmap_survives_layer_destroy() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let layer = scene.add_layer(win, None);

    let script = scene.add_bitmap(2, 2, vec![0; 16]);
    scene.bitmap_mut(script).unwrap().script_owned = true;
    scene.layer_mut(layer).unwrap().bitmap = Some(script);
    scene.destroy_layer(layer);
    assert!(scene.bitmap(script).is_some(), "script Bitmap survives");
}

/// The window primary layer's bitmap survives while its window lives and is
/// freed when the primary layer is destroyed.
#[test]
fn primary_layer_bitmap_survives_until_the_layer_is_destroyed() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let primary = scene.add_layer(win, None);
    assert_eq!(scene.window(win).unwrap().primary_layer, Some(primary));
    let content = scene.add_layer(win, Some(primary));
    let primary_bmp = scene.add_bitmap(4, 4, vec![0; 64]);
    scene.layer_mut(primary).unwrap().bitmap = Some(primary_bmp);
    let content_bmp = scene.add_bitmap(2, 2, vec![1; 16]);
    scene.layer_mut(content).unwrap().bitmap = Some(content_bmp);

    // A sweep while the window lives must not free the primary bitmap.
    assert_eq!(scene.collect_unused_bitmaps(), 0);
    assert!(scene.bitmap(primary_bmp).is_some());

    scene.destroy_layer(primary);
    assert!(
        scene.bitmap(primary_bmp).is_none(),
        "primary bitmap freed with the primary layer"
    );
    assert!(scene.bitmap(content_bmp).is_none(), "content freed too");
}

/// `collect_unused_bitmaps` sweeps orphans but respects the script
/// protection.
#[test]
fn collect_unused_bitmaps_sweeps_orphans_only() {
    let mut scene = Scene::default();
    let orphan = scene.add_bitmap(1, 1, vec![0; 4]);
    let script = scene.add_bitmap(1, 1, vec![0; 4]);
    scene.bitmap_mut(script).unwrap().script_owned = true;

    assert_eq!(scene.collect_unused_bitmaps(), 1);
    assert!(scene.bitmap(orphan).is_none());
    assert!(scene.bitmap(script).is_some());
}

/// Rebuilding a scene after a teardown must not surface a stale layer:
/// the new generation renders exactly its own layers.
#[test]
fn scene_rebuild_after_teardown_leaves_no_stale_layers() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1280, 720));
    // First scene generation: a root with nested content.
    let old_root = scene.add_layer(win, None);
    let old_child = scene.add_layer(win, Some(old_root));
    let old_grandchild = scene.add_layer(win, Some(old_child));
    scene.destroy_layer(old_root);
    assert!(scene.layers.is_empty());
    // New generation.
    let new_root = scene.add_layer(win, None);
    let new_child = scene.add_layer(win, Some(new_root));
    let order = scene.window_layer_order(win);
    assert_eq!(order, vec![new_root, new_child]);
    for stale in [old_root, old_child, old_grandchild] {
        assert!(scene.layer(stale).is_none(), "no stale layer {stale}");
        assert!(!order.contains(&stale), "stale {stale} not rendered");
    }
}

/// `node_visible`/`node_enabled` walk the ancestor chain.
#[test]
fn node_state_walks_ancestors() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let parent = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(parent));
    scene.layer_mut(parent).unwrap().visible = true;
    scene.layer_mut(child).unwrap().visible = true;
    assert!(scene.node_visible(child));
    scene.layer_mut(parent).unwrap().visible = false;
    assert!(!scene.node_visible(child), "hidden ancestor hides child");
    assert!(scene.node_enabled(child));
    scene.layer_mut(parent).unwrap().enabled = false;
    assert!(!scene.node_enabled(child));
}

/// `set_focus` moves the window's focus and `remove_layer` blurs a
/// removed focus holder.
#[test]
fn focus_moves_and_clears_with_removal() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let a = scene.add_layer(win, None);
    let b = scene.add_layer(win, None);
    assert_eq!(scene.set_focus(a), (None, Some(a)));
    assert_eq!(scene.window(win).unwrap().focused_layer, Some(a));
    assert_eq!(scene.set_focus(b), (Some(a), Some(b)));
    scene.remove_layer(b);
    assert_eq!(scene.window(win).unwrap().focused_layer, None);
}

/// Natives can still mutate the public `fonts`/`windows` vectors with a
/// direct `retain`; the verified fallback must keep lookups correct even
/// though the index map then points at a shifted slot.
#[test]
fn lookups_fall_back_correctly_when_natives_retain_directly() {
    let mut scene = Scene::default();
    let win = scene.add_window("a", (1, 1));
    let _win_b = scene.add_window("b", (1, 1));
    let f1 = scene.add_font("one".into(), 10, [0; 4]);
    let f2 = scene.add_font("two".into(), 10, [0; 4]);
    scene.fonts.retain(|f| f.id != f1);
    assert!(scene.font(f1).is_none());
    assert_eq!(scene.font(f2).unwrap().id, f2);
    assert_eq!(scene.window(win).unwrap().title, "a");
}

#[test]
fn revision_tracks_mutations_but_not_dirty_clear() {
    let mut scene = Scene::default();
    let start = scene.revision();
    let win = scene.add_window("t", (1, 1));
    assert!(scene.revision() > start, "add_window bumps revision");
    let layer = scene.add_layer(win, None);
    let after_add = scene.revision();
    scene.layer_mut(layer).unwrap().opacity = 0.5;
    assert!(scene.revision() > after_add, "layer_mut bumps revision");
    let bitmap = scene.add_bitmap(1, 1, vec![0; 4]);
    let after_bitmap = scene.revision();
    scene.set_bitmap_clean(bitmap);
    assert_eq!(
        scene.revision(),
        after_bitmap,
        "clearing the renderer dirty flag is not a logical mutation"
    );
    let after_clean = scene.revision();
    scene.remove_layer(layer);
    assert!(
        scene.revision() > after_clean,
        "remove_layer bumps revision"
    );
}

/// `resize_province_image` uses checked arithmetic and a fallible allocation:
/// a size that cannot be represented/allocated leaves the existing plane
/// untouched, while a large-but-valid plane is allowed.
#[test]
fn resize_province_image_handles_large_sizes() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let layer = scene.add_layer(win, None);
    scene.resize_province_image(layer, 4, 3);
    assert_eq!(scene.province_image_pitch(layer), 4);

    // A large but valid plane (1024×1024 = 1 MiB) is accepted.
    scene.resize_province_image(layer, 1024, 1024);
    assert_eq!(scene.province_image_pitch(layer), 1024);
    assert!(
        scene
            .with_province_pixels(layer, |p| p.len() == 1024 * 1024)
            .unwrap()
    );

    // A structurally impossible size is a no-op; the existing plane stays.
    scene.resize_province_image(layer, u32::MAX, u32::MAX);
    assert_eq!(scene.province_image_pitch(layer), 1024);
    assert!(
        scene
            .with_province_pixels(layer, |p| p.len() == 1024 * 1024)
            .unwrap()
    );
}

/// An invalid `set_focus` id leaves the current holder untouched (the doc
/// previously claimed it blurred the holder).
#[test]
fn set_focus_invalid_id_leaves_holder() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let a = scene.add_layer(win, None);
    assert_eq!(scene.set_focus(a), (None, Some(a)));
    assert_eq!(scene.set_focus(9999), (None, None));
    assert_eq!(scene.window(win).unwrap().focused_layer, Some(a));
    assert_eq!(scene.clear_focus(win), Some(a));
    assert_eq!(scene.window(win).unwrap().focused_layer, None);
}
