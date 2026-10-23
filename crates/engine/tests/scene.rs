//! Integration tests for the logical scene graph (`Scene`).
//!
//! Relocated from the inline `#[cfg(test)]` module in `src/scene.rs`.

use api::geometry::PixelRegion;
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
    scene.layer_mut(c).unwrap().z_order = 0;
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

/// The two counters move independently: a structural change (window, layer,
/// bitmap create/remove/resize) bumps `structure_revision` and leaves
/// `content_revision` alone, a bitmap content write bumps `content_revision`
/// and leaves `structure_revision` alone, and clearing the renderer's dirty
/// flag bumps neither.
#[test]
fn structure_and_content_revisions_track_their_own_mutations() {
    let mut scene = Scene::default();
    let start = scene.structure_revision();
    let win = scene.add_window("t", (1, 1));
    assert!(
        scene.structure_revision() > start,
        "add_window bumps the structural revision"
    );
    let layer = scene.add_layer(win, None);
    let bitmap = scene.add_bitmap(1, 1, vec![0; 4]);

    let structure = scene.structure_revision();
    let content = scene.content_revision();
    scene.bitmap_mut(bitmap).unwrap().mark_dirty();
    assert_eq!(
        scene.structure_revision(),
        structure,
        "a bitmap content write is not structural"
    );
    assert!(
        scene.content_revision() > content,
        "a bitmap content write bumps the content revision"
    );

    let content = scene.content_revision();
    scene.layer_mut(layer).unwrap().opacity = 0.5;
    assert!(
        scene.structure_revision() > structure,
        "layer_mut bumps the structural revision"
    );
    assert_eq!(
        scene.content_revision(),
        content,
        "a layer write moves no pixels"
    );

    let second = scene.add_bitmap(1, 1, vec![0; 4]);
    let structure = scene.structure_revision();
    let content = scene.content_revision();
    scene.set_bitmap_clean(second);
    assert_eq!(
        (scene.structure_revision(), scene.content_revision()),
        (structure, content),
        "clearing the renderer dirty flag is not a scene mutation"
    );

    scene.remove_bitmap(second);
    assert!(
        scene.structure_revision() > structure,
        "remove_bitmap bumps the structural revision"
    );
    scene.remove_layer(layer);
    assert!(
        scene.structure_revision() > structure,
        "remove_layer bumps the structural revision"
    );
}

/// A bitmap image replace that keeps the size moves only the content
/// revision; one that changes the size is also structural, because the
/// projection reads the dimensions through the layer's image size. The
/// replace never claims script ownership: a script `Bitmap` sets that itself
/// and a layer's own image stays layer-owned.
#[test]
fn replace_bitmap_image_splits_resize_from_content_and_keeps_ownership() {
    let mut scene = Scene::default();
    let id = scene.add_bitmap(1, 1, vec![0; 4]);

    let structure = scene.structure_revision();
    let content = scene.content_revision();
    assert!(scene.replace_bitmap_image(id, 1, 1, vec![1; 4]));
    assert_eq!(
        scene.structure_revision(),
        structure,
        "a same-size replace does not change the drawn tree"
    );
    assert!(
        scene.content_revision() > content,
        "a same-size replace changes the pixels"
    );
    let bitmap = scene.bitmap(id).expect("the bitmap survives the replace");
    assert!(bitmap.dirty, "a replaced image is dirty");
    assert!(!bitmap.script_owned, "a replaced image is not script-owned");

    let structure = scene.structure_revision();
    assert!(scene.replace_bitmap_image(id, 2, 2, vec![1; 16]));
    assert!(
        scene.structure_revision() > structure,
        "a resize is structural"
    );
    assert_eq!(scene.bitmap(id).expect("the bitmap").rgba.len(), 16);
    assert!(!scene.replace_bitmap_image(9999, 2, 2, vec![0; 16]));
}

/// `bitmap_mut_with_source` borrows the write target and one read source at
/// once, in either pool order, and refuses the bitmap it writes. It is a
/// borrow accessor, not a content change: neither revision nor the target's
/// `content_generation` moves (the caller reports the write once).
#[test]
fn bitmap_mut_with_source_splits_without_touching_the_revision() {
    let mut scene = Scene::default();
    let first = scene.add_bitmap(1, 1, vec![1; 4]);
    let second = scene.add_bitmap(1, 1, vec![2; 4]);
    let structure = scene.structure_revision();
    let content = scene.content_revision();

    // Source after the target in the pool.
    {
        let (target, source) = scene
            .bitmap_mut_with_source(first, second)
            .expect("distinct bitmaps");
        target.rgba[0] = source.rgba[0];
    }
    assert_eq!(scene.bitmap(first).unwrap().rgba[0], 2);
    // Source before the target in the pool.
    {
        let (target, source) = scene
            .bitmap_mut_with_source(second, first)
            .expect("distinct bitmaps");
        target.rgba[1] = source.rgba[0];
    }
    assert_eq!(scene.bitmap(second).unwrap().rgba[1], 2);

    assert!(
        scene.bitmap_mut_with_source(first, first).is_none(),
        "a composite never samples the bitmap it writes"
    );
    assert!(scene.bitmap_mut_with_source(first, 9999).is_none());
    assert_eq!(scene.structure_revision(), structure);
    assert_eq!(scene.content_revision(), content);
    assert_eq!(scene.bitmap(first).unwrap().content_generation, 0);
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

/// A resize preserves only the rows the source buffer actually holds and
/// fills the rest (`LayerBitmapIntf.cpp:122-163`: `SetSize` keeps the
/// overlapping pixels, then the expanded region is filled). A bitmap that
/// declares a size without holding the pixels — a decode whose blank image
/// could not be allocated, or a truncated image buffer — must keep the fill
/// instead of reading outside its buffer.
#[test]
fn resize_bitmap_keeps_the_fill_for_rows_the_source_does_not_hold() {
    let mut scene = Scene::default();

    // A bitmap that declares 4x4 pixels but holds none.
    let empty = scene.add_bitmap(4, 4, Vec::new());
    assert!(scene.resize_bitmap(empty, 6, 2, [9, 8, 7, 6]));
    let bitmap = scene.bitmap(empty).expect("the bitmap survives the resize");
    assert_eq!((bitmap.width, bitmap.height), (6, 2));
    assert_eq!(bitmap.rgba.len(), 6 * 2 * 4);
    assert!(
        bitmap
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel == &[9, 8, 7, 6]),
        "a bitmap without pixels keeps the fill"
    );
    assert!(bitmap.dirty, "a resized bitmap is dirty");

    // A 3x3 bitmap whose buffer stops after two rows: those rows are
    // preserved, the third keeps the fill.
    let short = scene.add_bitmap(3, 3, vec![1; 3 * 2 * 4]);
    assert!(scene.resize_bitmap(short, 5, 3, [9, 8, 7, 6]));
    let bitmap = scene.bitmap(short).expect("the bitmap survives the resize");
    assert_eq!((bitmap.width, bitmap.height), (5, 3));
    assert_eq!(bitmap.rgba.len(), 5 * 3 * 4);
    // Each destination row is 20 bytes: two preserved rows (12 bytes of
    // source plus 8 bytes of fill) and one row of fill only.
    assert_eq!(&bitmap.rgba[..12], &[1u8; 12]);
    assert_eq!(&bitmap.rgba[12..20], &[9, 8, 7, 6, 9, 8, 7, 6]);
    assert_eq!(&bitmap.rgba[20..32], &[1u8; 12]);
    assert_eq!(&bitmap.rgba[32..40], &[9, 8, 7, 6, 9, 8, 7, 6]);
    assert!(
        bitmap.rgba[40..]
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel == &[9, 8, 7, 6]),
        "the row the source does not hold keeps the fill"
    );
}

/// A resize clears or fills only the part the old image does not cover and
/// keeps the overlapping pixels, whichever way the row pitch changes.
///
/// Reference `SetSizeWithFill` keeps the overlapping pixels and fills the
/// expanded region (`reference/cpp/core/visual/LayerBitmapIntf.cpp:122-163`).
#[test]
fn resize_bitmap_keeps_the_overlap_and_clears_only_the_new_region() {
    let mut scene = Scene::default();
    let mut rgba = Vec::new();
    for y in 0..3u8 {
        for x in 0..4u8 {
            rgba.extend_from_slice(&[x + 1, y + 1, 0x11, 0xff]);
        }
    }
    let id = scene.add_bitmap(4, 3, rgba);

    // Wider rows: the old pixels keep their values and the new right-hand
    // columns are zero, not the old buffer's trailing bytes.
    assert!(scene.resize_bitmap(id, 6, 3, [0, 0, 0, 0]));
    let bitmap = scene.bitmap(id).expect("the bitmap survives the resize");
    assert_eq!((bitmap.width, bitmap.height), (6, 3));
    for y in 0..3u8 {
        for x in 0..4u8 {
            let at = ((y as usize * 6) + x as usize) * 4;
            assert_eq!(
                &bitmap.rgba[at..at + 4],
                &[x + 1, y + 1, 0x11, 0xff],
                "old pixel ({x}, {y})"
            );
        }
        for x in 4..6u8 {
            let at = ((y as usize * 6) + x as usize) * 4;
            assert_eq!(&bitmap.rgba[at..at + 4], &[0, 0, 0, 0], "new column {x}");
        }
    }

    // Narrower and shorter: the top-left overlap survives.
    assert!(scene.resize_bitmap(id, 2, 2, [0, 0, 0, 0]));
    let bitmap = scene.bitmap(id).expect("the bitmap survives the resize");
    assert_eq!((bitmap.width, bitmap.height), (2, 2));
    for y in 0..2u8 {
        for x in 0..2u8 {
            let at = ((y as usize * 2) + x as usize) * 4;
            assert_eq!(&bitmap.rgba[at..at + 4], &[x + 1, y + 1, 0x11, 0xff]);
        }
    }

    // A non-transparent fill covers only the rows the old image does not
    // reach.
    assert!(scene.resize_bitmap(id, 2, 4, [5, 6, 7, 8]));
    let bitmap = scene.bitmap(id).expect("the bitmap survives the resize");
    for y in 0..2u8 {
        for x in 0..2u8 {
            let at = ((y as usize * 2) + x as usize) * 4;
            assert_eq!(
                &bitmap.rgba[at..at + 4],
                &[x + 1, y + 1, 0x11, 0xff],
                "the overlap keeps ({x}, {y})"
            );
        }
    }
    for pixel in bitmap.rgba[2 * 2 * 4..].as_chunks::<4>().0.iter() {
        assert_eq!(pixel, &[5, 6, 7, 8], "the new rows take the fill");
    }
}

/// A resize keeps the whole rows a partially held buffer contains and fills
/// the rest.
///
/// The reference's `SetSizeWithFill` keeps the overlapping pixels and fills
/// the expanded region (`reference/cpp/core/visual/LayerBitmapIntf.cpp:122-163`);
/// a buffer that stops inside a row holds no whole row to keep, so the copy
/// stops there and the remaining rows keep the fill.
#[test]
fn resize_bitmap_keeps_the_whole_rows_a_partial_buffer_holds() {
    let mut scene = Scene::default();

    // A 4x4 image whose buffer holds two whole rows and six bytes of a third.
    let mut rgba = Vec::new();
    rgba.extend_from_slice(&[1u8; 16]);
    rgba.extend_from_slice(&[2u8; 16]);
    rgba.extend_from_slice(&[3u8; 6]);
    let id = scene.add_bitmap(4, 4, rgba);
    assert!(scene.resize_bitmap(id, 6, 4, [9, 8, 7, 6]));

    let bitmap = scene.bitmap(id).expect("the bitmap survives the resize");
    assert_eq!((bitmap.width, bitmap.height), (6, 4));
    assert_eq!(bitmap.rgba.len(), 6 * 4 * 4);
    // Each destination row is 24 bytes: the two whole source rows keep their
    // 16 bytes and take 8 bytes of fill, the last two rows are fill only.
    assert_eq!(&bitmap.rgba[..16], &[1u8; 16], "the first row is preserved");
    assert_eq!(&bitmap.rgba[16..24], &[9, 8, 7, 6, 9, 8, 7, 6]);
    assert_eq!(
        &bitmap.rgba[24..40],
        &[2u8; 16],
        "the second row is preserved"
    );
    assert_eq!(&bitmap.rgba[40..48], &[9, 8, 7, 6, 9, 8, 7, 6]);
    assert!(
        bitmap.rgba[48..]
            .as_chunks::<4>()
            .0
            .iter()
            .all(|pixel| pixel == &[9, 8, 7, 6]),
        "the half-held row and the row after it keep the fill"
    );
}

/// The address of a layer's main image is 0 when the image declares its size
/// and holds no pixels.
///
/// The reference hands out the MainImage's first scanline
/// (`reference/cpp/core/visual/LayerIntf.cpp:3005-3009`) and 0 when the layer
/// has no MainImage; the write variant marks the image modified
/// (`reference/cpp/core/visual/LayerIntf.cpp:3012-3017`). An image that
/// declares the probed size without a buffer has no address to hand out.
#[test]
fn a_layer_image_without_pixels_has_no_main_image_address() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let layer = scene.add_layer(win, None);
    let bitmap = scene.add_bitmap(2, 2, vec![0u8; 16]);
    scene.layer_mut(layer).unwrap().bitmap = Some(bitmap);
    // An image that holds its pixels has an address.
    assert_ne!(
        scene.main_image_pixel_buffer(layer),
        0,
        "an image with pixels has an address"
    );

    // The state a failed blank allocation leaves: the size is declared and
    // the pixel buffer is empty.
    let image = scene.bitmap_mut(bitmap).expect("the image is real");
    image.rgba.clear();
    image.dirty = false;

    assert_eq!(
        scene.main_image_pixel_buffer(layer),
        0,
        "an image without pixels has no address"
    );
    assert_eq!(
        scene.main_image_pixel_buffer_for_write(layer),
        0,
        "an image without pixels has no write address"
    );
    assert!(
        !scene.bitmap(bitmap).expect("the image is real").dirty,
        "no write address is handed out, so the image is not modified"
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

/// Reference `AddChild` (`reference/cpp/core/visual/LayerIntf.cpp:635-654`)
/// appends a new child at the front-most slot: the relative order index is the
/// new sibling position, and in absolute order mode the child receives
/// `last->GetAbsoluteOrderIndex() + 1`.
#[test]
fn attach_layer_appends_at_the_front_and_assigns_order() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (640, 480));
    let parent = scene.add_layer(win, None);
    let a = scene.add_layer(win, Some(parent));
    let b = scene.add_layer(win, Some(parent));

    assert_eq!(scene.layer(parent).unwrap().children, vec![a, b]);
    assert_eq!(scene.layer(a).unwrap().z_order, 0);
    assert_eq!(scene.layer(b).unwrap().z_order, 1);
    assert_eq!(scene.order_index(b), Some(1));

    assert!(scene.set_absolute_order_mode(parent, true));
    assert!(scene.layer(parent).unwrap().absolute_order_mode);
    let c = scene.add_layer(win, Some(parent));
    assert_eq!(scene.layer(parent).unwrap().children, vec![a, b, c]);
    assert_eq!(
        scene.layer(c).unwrap().z_order,
        2,
        "the reference assigns last absolute index + 1"
    );
    assert_eq!(scene.absolute_order_index(c), Some(2));

    // A parentless window root appends at the end of the root list.
    let root_a = scene.add_layer(win, None);
    let root_b = scene.add_layer(win, None);
    assert_eq!(
        scene.window(win).unwrap().layers,
        vec![parent, root_a, root_b]
    );
    assert_eq!(scene.layer(root_b).unwrap().z_order, 2);
}

/// Reference `SetAbsoluteOrderIndex` (`reference/cpp/core/visual/LayerIntf.cpp:1445-1452`)
/// requires a parent, turns the parent's absolute order mode on, inserts the
/// layer at the index selected by `ChildChangeAbsoluteOrder`
/// (`:1332-1354`) and stores the requested absolute index. The getter
/// (`:1435-1443`) returns that index while the mode is on.
#[test]
fn absolute_order_reorders_siblings_and_keeps_the_index() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (640, 480));
    let parent = scene.add_layer(win, None);
    let a = scene.add_layer(win, Some(parent));
    let b = scene.add_layer(win, Some(parent));
    let c = scene.add_layer(win, Some(parent));

    scene.set_absolute_order_index(c, 0).unwrap();
    assert!(scene.layer(parent).unwrap().absolute_order_mode);
    assert_eq!(scene.layer(parent).unwrap().children, vec![c, a, b]);
    assert_eq!(scene.absolute_order_index(c), Some(0));
    assert_eq!(scene.absolute_order_index(a), Some(0));
    assert_eq!(scene.order_index(a), Some(1));

    scene.set_absolute_order_index(c, 5).unwrap();
    assert_eq!(scene.layer(parent).unwrap().children, vec![a, b, c]);
    assert_eq!(scene.absolute_order_index(c), Some(5));
    assert_eq!(scene.order_index(c), Some(2));

    let error = scene
        .set_absolute_order_index(parent, 1)
        .expect_err("a parentless layer has no absolute order");
    assert!(error.contains("siblingless"), "{error}");
}

/// Reference `SetOrderIndex` (`reference/cpp/core/visual/LayerIntf.cpp:1396-1410`)
/// leaves absolute order mode, clamps the requested index to the sibling
/// range, moves the layer and makes the relative order index its position.
#[test]
fn order_setter_clamps_and_leaves_absolute_mode() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (640, 480));
    let parent = scene.add_layer(win, None);
    let a = scene.add_layer(win, Some(parent));
    let b = scene.add_layer(win, Some(parent));
    let c = scene.add_layer(win, Some(parent));

    scene.set_absolute_order_index(c, 9).unwrap();
    assert!(scene.layer(parent).unwrap().absolute_order_mode);
    scene.set_order_index(c, 0).unwrap();
    assert!(!scene.layer(parent).unwrap().absolute_order_mode);
    assert_eq!(scene.layer(parent).unwrap().children, vec![c, a, b]);
    assert_eq!(scene.layer(c).unwrap().z_order, 0);
    assert_eq!(scene.layer(a).unwrap().z_order, 1);
    assert_eq!(scene.layer(b).unwrap().z_order, 2);
    assert_eq!(scene.absolute_order_index(c), Some(0));

    scene.set_order_index(c, 99).unwrap();
    assert_eq!(scene.layer(parent).unwrap().children, vec![a, b, c]);
    assert_eq!(scene.layer(c).unwrap().z_order, 2);
    assert_eq!(scene.order_index(c), Some(2));
}

/// Reference `SetAbsoluteOrderMode` (`reference/cpp/core/visual/LayerIntf.cpp:1458-1470`)
/// seeds each child's absolute index from its current sibling position when
/// the mode is enabled, and the relative list is renumbered when it leaves.
#[test]
fn absolute_order_mode_seeds_child_indices() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (640, 480));
    let parent = scene.add_layer(win, None);
    let a = scene.add_layer(win, Some(parent));
    let b = scene.add_layer(win, Some(parent));
    let c = scene.add_layer(win, Some(parent));
    scene.layer_mut(a).unwrap().z_order = 9;
    scene.layer_mut(b).unwrap().z_order = 4;
    scene.layer_mut(c).unwrap().z_order = 7;

    assert!(scene.set_absolute_order_mode(parent, true));
    assert_eq!(scene.layer(a).unwrap().z_order, 0);
    assert_eq!(scene.layer(b).unwrap().z_order, 1);
    assert_eq!(scene.layer(c).unwrap().z_order, 2);

    assert!(scene.set_absolute_order_mode(parent, false));
    assert_eq!(scene.layer(a).unwrap().z_order, 0);
    assert_eq!(scene.layer(b).unwrap().z_order, 1);
    assert_eq!(scene.layer(c).unwrap().z_order, 2);
}

/// The first layer of a window is its primary layer and the screen-sized draw
/// target. A script-created first layer starts with the default empty rect,
/// so the attach step must give it the window client size; otherwise ancestor
/// hit-testing rejects every child of a normally-sized primary.
#[test]
fn first_layer_is_a_window_sized_primary() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (320, 240));
    let primary = scene.add_layer(win, None);
    assert_eq!(scene.window(win).unwrap().primary_layer, Some(primary));
    assert_eq!(
        scene.layer(primary).unwrap().rect,
        engine::Rect::new(0, 0, 0, 0)
    );
    assert_eq!(
        scene.clipping_rect(primary),
        Some(engine::Rect::new(0, 0, 320, 240)),
        "the primary layer clips like the window client"
    );

    let child = scene.add_layer(win, Some(primary));
    scene.layer_mut(child).unwrap().rect = engine::Rect::new(1, 1, 4, 4);
    scene.layer_mut(child).unwrap().visible = true;
    let bitmap = scene.add_bitmap(4, 4, vec![255; 64]);
    scene.layer_mut(child).unwrap().bitmap = Some(bitmap);
    scene.layer_mut(child).unwrap().hit_threshold = 0;
    assert_eq!(scene.layer_at(child, 1, 1, false, false), Some(child));
}

/// A rectangle mark accumulates into the bitmap's dirty region, and a whole
/// mark overrides it.
///
/// The dirty region is what the frame's upload sends to the texture: a
/// bitmap that changed in a rectangle uploads that rectangle alone, while a
/// bitmap that changed wholly uploads every pixel.
#[test]
fn a_dirty_region_accumulates_and_a_whole_mark_overrides_it() {
    let mut bitmap = engine::BitmapState::new(16, 16, vec![0u8; 16 * 16 * 4]);
    assert!(!bitmap.dirty, "a new bitmap is clean");

    bitmap.mark_dirty_rect(PixelRegion::new(1, 2, 3, 4));
    assert!(bitmap.dirty, "a rectangle marks the bitmap dirty");
    assert_eq!(
        bitmap.dirty_region,
        Some(PixelRegion::new(1, 2, 3, 4)),
        "the first rectangle is the whole region"
    );

    bitmap.mark_dirty_rect(PixelRegion::new(10, 10, 2, 2));
    assert_eq!(
        bitmap.dirty_region,
        Some(PixelRegion::new(1, 2, 11, 10)),
        "two rectangles mark the union of the two"
    );

    bitmap.mark_dirty();
    assert!(bitmap.dirty, "a whole mark marks the bitmap dirty");
    assert_eq!(bitmap.dirty_region, None, "a whole mark covers every pixel");
}

/// A rectangle that covers no pixel of the bitmap marks nothing, and a
/// cleaned bitmap starts its next region with the rectangle it is given.
#[test]
fn a_rectangle_outside_the_bitmap_marks_nothing() {
    let mut bitmap = engine::BitmapState::new(4, 4, vec![0u8; 4 * 4 * 4]);
    bitmap.mark_dirty_rect(PixelRegion::new(10, 10, 2, 2));
    assert!(
        !bitmap.dirty,
        "a rectangle outside the bitmap changes nothing"
    );
    assert_eq!(bitmap.dirty_region, None);

    // A cleaned bitmap has no region, so the next rectangle starts one.
    let mut scene = Scene::default();
    let id = scene.add_bitmap(4, 4, vec![0u8; 4 * 4 * 4]);
    scene
        .bitmap_mut(id)
        .expect("bitmap")
        .mark_dirty_rect(PixelRegion::new(1, 1, 2, 2));
    scene.set_bitmap_clean(id);
    let bitmap = scene.bitmap(id).expect("bitmap");
    assert!(!bitmap.dirty, "a cleaned bitmap is clean");
    assert_eq!(bitmap.dirty_region, None, "a clean bitmap has no region");

    scene
        .bitmap_mut(id)
        .expect("bitmap")
        .mark_dirty_rect(PixelRegion::new(2, 2, 1, 1));
    let bitmap = scene.bitmap(id).expect("bitmap");
    assert!(bitmap.dirty);
    assert_eq!(
        bitmap.dirty_region,
        Some(PixelRegion::new(2, 2, 1, 1)),
        "the rectangle after a clean starts the region"
    );
}
