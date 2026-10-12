//! Integration tests for the in-place CPU layer operations.
//!
//! These lock in the behaviour of the separable running-sum box blur
//! (`blur_bitmap_in_place`) against the original 2-D summation it replaced.

use engine::BitmapState;
use engine::Scene;
use engine::layer_ops::{blur_bitmap_in_place, render_layer_subtree};
use engine::raster::fill_rect_replace;
use engine::scene::Rect;

fn bitmap(width: u32, height: u32, rgba: Vec<u8>) -> BitmapState {
    BitmapState::new(width, height, rgba)
}

/// A `width` x `height` bitmap filled with one straight-alpha color.
fn solid_bitmap(scene: &mut Scene, width: u32, height: u32, color: [u8; 4]) -> u32 {
    let mut rgba = Vec::new();
    for _ in 0..width * height {
        rgba.extend_from_slice(&color);
    }
    scene.add_bitmap(width, height, rgba)
}

/// The original `blur_bitmap_in_place`: a 2-D clamped box sum with one final
/// integer division. The rewrite must reproduce it byte-for-byte.
fn naive_box_blur(bitmap: &BitmapState, xradius: u32, yradius: u32) -> Vec<u8> {
    let source = &bitmap.rgba;
    let w = bitmap.width;
    let h = bitmap.height;
    let mut out = vec![0u8; source.len()];
    for y in 0..h {
        for x in 0..w {
            let x0 = x.saturating_sub(xradius);
            let x1 = x.saturating_add(xradius).min(w - 1);
            let y0 = y.saturating_sub(yradius);
            let y1 = y.saturating_add(yradius).min(h - 1);
            let mut sums = [0u64; 4];
            let mut count = 0u64;
            for sy in y0..=y1 {
                for sx in x0..=x1 {
                    let i = ((sy * w + sx) * 4) as usize;
                    for (c, sum) in sums.iter_mut().enumerate() {
                        *sum += u64::from(source[i + c]);
                    }
                    count += 1;
                }
            }
            let i = ((y * w + x) * 4) as usize;
            for (c, &sum) in sums.iter().enumerate() {
                out[i + c] = (sum / count) as u8;
            }
        }
    }
    out
}

/// A deterministic pseudo-random RGBA image (xorshift; no dependencies).
fn sample(width: u32, height: u32, seed: u32) -> BitmapState {
    let mut state = seed | 1;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for _ in 0..width * height * 4 {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        rgba.push((state & 0xff) as u8);
    }
    bitmap(width, height, rgba)
}

#[test]
fn blur_matches_naive_reference() {
    for (w, h, rx, ry) in [
        (1u32, 1u32, 3u32, 2u32),
        (5, 7, 0, 0),
        (5, 7, 1, 1),
        (8, 4, 3, 2),
        (6, 6, 10, 10), // radius larger than the image
        (7, 3, 0, 4),
        (33, 17, 2, 5),
    ] {
        let mut fast = sample(w, h, w * 31 + h * 17 + rx * 7 + ry);
        let expected = naive_box_blur(&fast, rx, ry);
        blur_bitmap_in_place(&mut fast, rx, ry);
        assert_eq!(fast.rgba, expected, "size {w}x{h}, radius ({rx},{ry})");
    }
}

#[test]
fn blur_zero_radius_is_identity() {
    let mut bmp = sample(4, 4, 99);
    let original = bmp.rgba.clone();
    blur_bitmap_in_place(&mut bmp, 0, 0);
    assert_eq!(bmp.rgba, original);
}

#[test]
fn fill_rect_replace_still_clips_negative_origin() {
    let mut bmp = bitmap(2, 2, vec![0u8; 16]);
    fill_rect_replace(&mut bmp, -1, -1, 10, 10, [1, 2, 3, 4]);
    assert_eq!(bmp.rgba, [1, 2, 3, 4].repeat(4));
}

/// `render_layer_subtree` returns the layer's MainImage for a trivial subtree
/// and otherwise composites the layer's own image and its visible children
/// into a cache bitmap (the reference `Complete`, `LayerIntf.cpp:7673-7755`).
#[test]
fn render_layer_subtree_composites_children_into_a_cache() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let parent = scene.add_layer(win, None);
    scene.layer_mut(parent).unwrap().rect = Rect::new(0, 0, 2, 2);
    let child = scene.add_layer(win, Some(parent));
    let child_bitmap = solid_bitmap(&mut scene, 2, 2, [10, 20, 30, 255]);
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = true;
        l.bitmap = Some(child_bitmap);
    }

    let cache = render_layer_subtree(&mut scene, parent).expect("cache bitmap");
    {
        let bitmap = scene.bitmap(cache).unwrap();
        assert_eq!((bitmap.width, bitmap.height), (2, 2));
        assert_eq!(&bitmap.rgba[..4], &[10, 20, 30, 255]);
    }

    // A trivial subtree returns the layer's MainImage and allocates no cache.
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let layer = scene.add_layer(win, None);
    let main = scene.add_bitmap(2, 2, vec![7u8; 16]);
    {
        let l = scene.layer_mut(layer).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.bitmap = Some(main);
    }
    assert_eq!(render_layer_subtree(&mut scene, layer), Some(main));
    assert!(scene.layer(layer).unwrap().cache_bitmap.is_none());
}

/// The subtree render applies each child's `ImageLeft/Top/Width/Height`
/// placement — the reference `DrawSelf`/`CopySelf` transform
/// (`LayerIntf.cpp:6744-6750`) — so a sprite-sheet frame selected with a
/// negative `image_left` composites the right pixels.
#[test]
fn render_layer_subtree_applies_image_placement() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let parent = scene.add_layer(win, None);
    scene.layer_mut(parent).unwrap().rect = Rect::new(0, 0, 2, 2);
    let child = scene.add_layer(win, Some(parent));
    // A 4x2 sheet: the left half is red, the right half blue.
    let mut rgba = vec![0u8; 4 * 2 * 4];
    for y in 0..2 {
        for x in 0..4usize {
            let i = (y * 4 + x) * 4;
            let color = if x < 2 {
                [255, 0, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            rgba[i..i + 4].copy_from_slice(&color);
        }
    }
    let sheet = scene.add_bitmap(4, 2, rgba);
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = true;
        l.bitmap = Some(sheet);
        l.image_left = -2;
    }
    let cache = render_layer_subtree(&mut scene, parent).expect("cache bitmap");
    let bitmap = scene.bitmap(cache).unwrap();
    assert_eq!(
        &bitmap.rgba[..4],
        &[0, 0, 255, 255],
        "the right frame is drawn"
    );
}

/// `dfMask`/`dfProvince` layers draw their alpha/province plane rather than
/// the main image (`tTVPDrawFace`, `LayerIntf.h:59-67`; `DrawSelf`,
/// `LayerIntf.cpp:6721-6793`).
#[test]
fn render_layer_subtree_selects_the_layer_face() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let parent = scene.add_layer(win, None);
    scene.layer_mut(parent).unwrap().rect = Rect::new(0, 0, 2, 2);
    let child = scene.add_layer(win, Some(parent));
    let main = scene.add_bitmap(2, 2, vec![9u8; 16]);
    let province = vec![200u8; 4].into_boxed_slice();
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = true;
        l.bitmap = Some(main);
        l.province = Some(province);
        l.province_width = 2;
        l.province_height = 2;
        l.face = 3; // dfProvince
    }
    let cache = render_layer_subtree(&mut scene, parent).expect("cache bitmap");
    let bitmap = scene.bitmap(cache).unwrap();
    assert_eq!(
        &bitmap.rgba[..4],
        &[200, 200, 200, 255],
        "the province plane is drawn"
    );
}

/// `Complete` returns the existing `CacheBitmap` while `CacheRecalcRegion` is
/// empty (`CacheRecalcRegion.GetCount() == 0`), so an unchanged subtree is
/// not recomposited.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7734-7737
#[test]
fn render_layer_subtree_reuses_the_cache_until_the_subtree_changes() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let parent = scene.add_layer(win, None);
    scene.layer_mut(parent).unwrap().rect = Rect::new(0, 0, 2, 2);
    let child = scene.add_layer(win, Some(parent));
    let child_bitmap = solid_bitmap(&mut scene, 2, 2, [10, 20, 30, 255]);
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = true;
        l.bitmap = Some(child_bitmap);
    }

    let content = render_layer_subtree(&mut scene, parent).expect("content bitmap");
    scene.set_bitmap_clean(content);
    // No change: the same composited content is returned and not rebuilt.
    assert_eq!(render_layer_subtree(&mut scene, parent), Some(content));
    assert!(
        !scene.bitmap(content).unwrap().dirty,
        "an unchanged subtree is not recomposited"
    );

    // The child's placement changed: `Update` appends to the ancestor's
    // `CacheRecalcRegion` and the next `Complete` recomposites.
    //
    // reference/cpp/core/visual/LayerIntf.cpp:6305-6335
    // reference/cpp/core/visual/LayerIntf.cpp:6254
    scene.layer_mut(child).unwrap().rect = Rect::new(1, 0, 2, 2);
    scene.set_bitmap_clean(content);
    assert_eq!(render_layer_subtree(&mut scene, parent), Some(content));
    let bitmap = scene.bitmap(content).unwrap();
    assert!(bitmap.dirty, "a moved child recomposites the cache");
    assert_eq!(&bitmap.rgba[0..4], &[0, 0, 0, 0], "the vacated pixel");
    assert_eq!(&bitmap.rgba[4..8], &[10, 20, 30, 255], "the moved child");
}

/// Reusing the composited subtree content does not mutate the scene graph:
/// the second `Complete` returns the existing `CacheBitmap` without bumping
/// `Scene::revision`, so the renderer does not rebuild the projected scene
/// for a frame that only reads the cache.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7734-7737
#[test]
fn render_layer_subtree_reuse_does_not_touch_the_scene_revision() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let parent = scene.add_layer(win, None);
    scene.layer_mut(parent).unwrap().rect = Rect::new(0, 0, 2, 2);
    let child = scene.add_layer(win, Some(parent));
    let child_bitmap = solid_bitmap(&mut scene, 2, 2, [10, 20, 30, 255]);
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = true;
        l.bitmap = Some(child_bitmap);
    }

    let content = render_layer_subtree(&mut scene, parent).expect("content bitmap");
    let revision = scene.revision();
    assert_eq!(render_layer_subtree(&mut scene, parent), Some(content));
    assert_eq!(
        scene.revision(),
        revision,
        "a reused subtree cache does not mutate the scene graph"
    );
}

/// An image edit that does not go through a layer accessor (the `Layer` pixel
/// natives borrow the scene's bitmap vec directly) still invalidates the
/// cache: the composited content records the source images'
/// `content_generation` and recomposites when it changes.
///
/// reference/cpp/core/visual/LayerIntf.cpp:6305-6335
#[test]
fn render_layer_subtree_rebuilds_when_a_drawn_image_changes() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let parent = scene.add_layer(win, None);
    scene.layer_mut(parent).unwrap().rect = Rect::new(0, 0, 2, 2);
    let child = scene.add_layer(win, Some(parent));
    let child_bitmap = solid_bitmap(&mut scene, 2, 2, [10, 20, 30, 255]);
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = true;
        l.bitmap = Some(child_bitmap);
    }

    let content = render_layer_subtree(&mut scene, parent).expect("content bitmap");
    scene.set_bitmap_clean(content);
    {
        let bitmap = scene.bitmap_mut(child_bitmap).expect("child bitmap");
        bitmap.rgba[..4].copy_from_slice(&[200, 100, 50, 255]);
        bitmap.mark_dirty();
    }
    assert_eq!(render_layer_subtree(&mut scene, parent), Some(content));
    let bitmap = scene.bitmap(content).unwrap();
    assert!(
        bitmap.dirty,
        "a changed source image recomposites the cache"
    );
    assert_eq!(&bitmap.rgba[..4], &[200, 100, 50, 255]);
}

/// A change to a hidden layer does not invalidate the cache, matching
/// `UpdateChildRegion`'s visible-child guard.
///
/// reference/cpp/core/visual/LayerIntf.cpp:6249-6256
#[test]
fn render_layer_subtree_ignores_hidden_child_changes() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let parent = scene.add_layer(win, None);
    scene.layer_mut(parent).unwrap().rect = Rect::new(0, 0, 2, 2);
    let visible = scene.add_layer(win, Some(parent));
    let visible_bitmap = solid_bitmap(&mut scene, 2, 2, [10, 20, 30, 255]);
    {
        let l = scene.layer_mut(visible).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = true;
        l.bitmap = Some(visible_bitmap);
    }
    let hidden = scene.add_layer(win, Some(parent));
    let hidden_bitmap = solid_bitmap(&mut scene, 2, 2, [1, 2, 3, 255]);
    {
        let l = scene.layer_mut(hidden).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = false;
        l.bitmap = Some(hidden_bitmap);
    }

    let content = render_layer_subtree(&mut scene, parent).expect("content bitmap");
    scene.set_bitmap_clean(content);
    {
        let bitmap = scene.bitmap_mut(hidden_bitmap).expect("hidden bitmap");
        bitmap.rgba[..4].copy_from_slice(&[9, 9, 9, 255]);
        bitmap.mark_dirty();
    }
    assert_eq!(render_layer_subtree(&mut scene, parent), Some(content));
    assert!(
        !scene.bitmap(content).unwrap().dirty,
        "a hidden child's image change is not composited"
    );
}

/// `Scene::update_layer` skips all invalidation when its closure reports no
/// change: the layer's subtree cache and every caching ancestor's stay clean
/// and `Scene::revision` does not advance. The reference guards a property
/// write the same way, comparing the requested value with the stored one
/// before it invalidates (`tTJSNI_BaseLayer::SetVisible`,
/// `reference/cpp/core/visual/LayerIntf.cpp:1183`).
#[test]
fn update_layer_unchanged_write_does_not_invalidate() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let parent = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(parent));
    // The first write changes the value, so it invalidates like `layer_mut`.
    assert_eq!(
        scene.update_layer(child, |layer| {
            if layer.visible {
                return false;
            }
            layer.visible = true;
            true
        }),
        Some(true)
    );
    for layer in &mut scene.layers {
        layer.cache_recalc = false;
    }
    let revision = scene.revision();
    // The second write receives the stored value: the guard skips `Update()`
    // and `Parent->NotifyChildrenVisualStateChanged()`
    // (`reference/cpp/core/visual/LayerIntf.cpp:1183-1193`).
    assert_eq!(
        scene.update_layer(child, |layer| {
            if layer.visible {
                return false;
            }
            layer.visible = true;
            true
        }),
        Some(false)
    );
    assert_eq!(
        scene.revision(),
        revision,
        "a no-op write bumps no revision"
    );
    assert!(!scene.layer(child).unwrap().cache_recalc);
    assert!(!scene.layer(parent).unwrap().cache_recalc);
}

/// A write that changes the value still invalidates: the layer and every
/// caching ancestor are marked for recomposite and the revision advances, so
/// the host re-projects. `reference/cpp/core/visual/LayerIntf.cpp:1190-1193`
/// (`SetVisible` calls `Update()` then notifies the parent).
#[test]
fn update_layer_changed_write_marks_subtree_and_ancestors() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (1, 1));
    let parent = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(parent));
    for layer in &mut scene.layers {
        layer.cache_recalc = false;
    }
    let revision = scene.revision();
    assert_eq!(
        scene.update_layer(child, |layer| {
            if layer.visible {
                return false;
            }
            layer.visible = true;
            true
        }),
        Some(true)
    );
    assert!(
        scene.revision() > revision,
        "a changed write bumps revision"
    );
    assert!(scene.layer(child).unwrap().cache_recalc);
    assert!(scene.layer(parent).unwrap().cache_recalc);
}

/// A missing layer is `None`, distinct from `Some(false)` (an untouched
/// write): `with_layer_write` must keep raising for a stale id instead of
/// treating it as a no-op.
#[test]
fn update_layer_missing_layer_reports_absent() {
    let mut scene = Scene::default();
    let revision = scene.revision();
    assert_eq!(scene.update_layer(9999, |_| true), None);
    assert_eq!(scene.revision(), revision);
}
