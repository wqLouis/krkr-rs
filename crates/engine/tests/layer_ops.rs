//! Integration tests for the in-place CPU layer operations.
//!
//! These lock in the behaviour of the separable running-sum box blur
//! (`blur_bitmap_in_place`) against the original 2-D summation it replaced.

use api::geometry::PixelRegion;
use engine::BitmapState;
use engine::Scene;
use engine::layer_ops::{
    AffineBlit, BlitOptions, BlitSource, COPY_MAIN, COPY_MASK, affine_blit, affine_blit_clipped,
    affine_blit_clipped_pixel_center, blit_copy, blit_over, blit_plane, blur_bitmap_in_place,
    copy_9patch, do_gray_scale, ensure_transition_display, fill_color_on_alpha, fill_mask, flip_lr,
    gaussian_blur, operate_rect, render_layer_subtree, stretch_blit, tile_rect,
};
use engine::raster::fill_rect_replace;
use engine::scene::Rect;
use engine::trans::Trans;

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

/// A texture-less container clips its descendants inside a subtree cache as
/// well as in the live projection. The reference draws children only inside
/// the parent's `Rect` (`reference/cpp/core/visual/LayerIntf.cpp:7138-7140`,
/// `:7184-7185`), so a grandchild outside a container contributes no pixels
/// to the cache.
#[test]
fn cached_subtree_respects_a_textureless_container_clip() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (4, 4));
    let root = scene.add_layer(win, None);
    scene.layer_mut(root).unwrap().rect = Rect::new(0, 0, 4, 4);
    let container = scene.add_layer(win, Some(root));
    {
        let l = scene.layer_mut(container).unwrap();
        l.rect = Rect::new(0, 0, 2, 4);
        l.visible = true;
    }
    let child = scene.add_layer(win, Some(container));
    let child_bitmap = solid_bitmap(&mut scene, 2, 4, [255, 0, 0, 255]);
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(2, 0, 2, 4);
        l.visible = true;
        l.bitmap = Some(child_bitmap);
    }

    let cache = render_layer_subtree(&mut scene, root).expect("cache bitmap");
    let bitmap = scene.bitmap(cache).unwrap();
    assert_eq!((bitmap.width, bitmap.height), (4, 4));
    assert!(
        bitmap.rgba.iter().all(|&byte| byte == 0),
        "the grandchild is outside the texture-less container and must be clipped"
    );
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
/// either scene revision, so the renderer neither rebuilds the projected
/// scene nor reruns the cache pass for a frame that only reads the cache.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7734-7737
#[test]
fn render_layer_subtree_reuse_does_not_touch_the_scene_revisions() {
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
    let structure = scene.structure_revision();
    let texture = scene.content_revision();
    assert_eq!(render_layer_subtree(&mut scene, parent), Some(content));
    assert_eq!(
        scene.structure_revision(),
        structure,
        "a reused subtree cache does not mutate the tree's structure"
    );
    assert_eq!(
        scene.content_revision(),
        texture,
        "a reused subtree cache writes no pixels"
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

/// An outer layer over an inner layer that consumes a cache over a visible
/// grandchild. The inner layer composites its subtree and publishes the
/// content as its cache. Returns the scene, the outer/inner layer ids, the
/// inner cache content and the grandchild's main image.
fn nested_cache_scene() -> (Scene, u32, u32, u32, u32) {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let outer = scene.add_layer(win, None);
    {
        let l = scene.layer_mut(outer).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = true;
    }
    let inner = scene.add_layer(win, Some(outer));
    {
        let l = scene.layer_mut(inner).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = true;
    }
    let grandchild = scene.add_layer(win, Some(inner));
    let live = solid_bitmap(&mut scene, 2, 2, [10, 20, 30, 255]);
    {
        let l = scene.layer_mut(grandchild).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = true;
        l.bitmap = Some(live);
    }
    scene.inc_cache_enabled_count(inner);
    let content = render_layer_subtree(&mut scene, inner).expect("inner content");
    scene.set_cache_bitmap(inner, Some(content));
    (scene, outer, inner, content, live)
}

/// Fill a bitmap with one opaque colour.
fn paint(scene: &mut Scene, id: u32, color: [u8; 4]) {
    let bitmap = scene.bitmap_mut(id).expect("bitmap to paint");
    for pixel in bitmap.rgba.chunks_exact_mut(4) {
        pixel.copy_from_slice(&color);
    }
}

/// A descendant whose subtree cache is live draws that cache instead of its
/// live subtree: the parent's `Draw` calls the child's `Draw`
/// (`LayerIntf.cpp:7180-7198`), which hands its `CacheBitmap` to the parent
/// through `DrawCompleted` (`:7223-7226`) and covers the grandchildren.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7180-7198
/// reference/cpp/core/visual/LayerIntf.cpp:7223-7226
#[test]
fn render_layer_subtree_uses_a_live_nested_cache() {
    let (mut scene, outer, _inner, content, _live) = nested_cache_scene();
    // Paint the cached content a colour the live subtree never has, so the
    // outer composite reveals which source it sampled.
    paint(&mut scene, content, [200, 100, 50, 255]);

    let outer_content = render_layer_subtree(&mut scene, outer).expect("outer content");
    let bitmap = scene.bitmap(outer_content).unwrap();
    assert_eq!(
        &bitmap.rgba[..4],
        &[200, 100, 50, 255],
        "the outer cache holds the child's cached content"
    );
    assert_eq!(
        &bitmap.rgba[4..8],
        &[200, 100, 50, 255],
        "the grandchild covered by the cache is not drawn on top of it"
    );
}

/// A nested cache is sampled only while it is current. An edit behind the
/// layer accessors advances the drawn source's generation, so the outer
/// composite falls back to the child's live subtree; a layer change marks the
/// child's cache for recomposite the same way (`Update` appends to the
/// ancestor's `CacheRecalcRegion`). Once the child recomposites, the outer
/// samples the refreshed cache again.
///
/// reference/cpp/core/visual/LayerIntf.cpp:6305-6335
/// reference/cpp/core/visual/LayerIntf.cpp:7734-7737
#[test]
fn render_layer_subtree_falls_back_from_a_stale_nested_cache() {
    let (mut scene, outer, inner, content, live) = nested_cache_scene();
    paint(&mut scene, content, [200, 100, 50, 255]);
    let outer_content = render_layer_subtree(&mut scene, outer).expect("outer content");
    assert_eq!(
        &scene.bitmap(outer_content).unwrap().rgba[..4],
        &[200, 100, 50, 255]
    );

    // The grandchild's pixels change without a layer accessor: the inner
    // cache's recorded source generation no longer matches.
    scene.bitmap_mut(live).unwrap().rgba[..4].copy_from_slice(&[0, 255, 0, 255]);
    let outer_content = render_layer_subtree(&mut scene, outer).expect("outer content");
    assert_eq!(
        &scene.bitmap(outer_content).unwrap().rgba[..4],
        &[0, 255, 0, 255],
        "a stale cache is not sampled"
    );

    // Recompositing the inner cache makes it live again.
    let refreshed = render_layer_subtree(&mut scene, inner).expect("inner content");
    assert_eq!(refreshed, content, "the cache bitmap is reused");
    scene.set_cache_bitmap(inner, Some(refreshed));
    paint(&mut scene, refreshed, [50, 100, 200, 255]);
    scene.set_bitmap_clean(outer_content);
    let outer_content = render_layer_subtree(&mut scene, outer).expect("outer content");
    assert_eq!(
        &scene.bitmap(outer_content).unwrap().rgba[..4],
        &[50, 100, 200, 255],
        "the refreshed cache is sampled"
    );
    assert!(
        scene.bitmap(outer_content).unwrap().dirty,
        "the outer cache is recomposited"
    );

    // A layer write marks the child's cache for recomposite; the outer falls
    // back to the live subtree until the child rebuilds it.
    scene.layer_mut(inner).unwrap().name = "inner".to_string();
    let outer_content = render_layer_subtree(&mut scene, outer).expect("outer content");
    assert_eq!(
        &scene.bitmap(outer_content).unwrap().rgba[..4],
        &[0, 255, 0, 255],
        "a pending recomposite is not sampled"
    );
    let refreshed = render_layer_subtree(&mut scene, inner).expect("inner content");
    scene.set_cache_bitmap(inner, Some(refreshed));
    paint(&mut scene, refreshed, [5, 6, 7, 255]);
    let outer_content = render_layer_subtree(&mut scene, outer).expect("outer content");
    assert_eq!(
        &scene.bitmap(outer_content).unwrap().rgba[..4],
        &[5, 6, 7, 255],
        "the rebuilt cache is sampled"
    );
}

/// `Scene::update_layer` skips all invalidation when its closure reports no
/// change: the layer's subtree cache and every caching ancestor's stay clean
/// and `Scene::structure_revision` does not advance. The reference guards a
/// property write the same way, comparing the requested value with the stored
/// one before it invalidates (`tTJSNI_BaseLayer::SetVisible`,
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
    let revision = scene.structure_revision();
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
        scene.structure_revision(),
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
    let revision = scene.structure_revision();
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
        scene.structure_revision() > revision,
        "a changed write bumps the structural revision"
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
    let revision = scene.structure_revision();
    assert_eq!(scene.update_layer(9999, |_| true), None);
    assert_eq!(scene.structure_revision(), revision);
}

/// A bitmap may declare a size while holding no pixel bytes: a pending decode
/// keeps its probed size and an empty buffer. Every CPU pixel primitive clips
/// against the declared rectangle and writes nothing when nothing is
/// drawable, so such a bitmap must neither abort nor corrupt the bitmaps it
/// is composited with.
///
/// `iTVPBaseBitmap::CopyRect` clips the source rect against the source's
/// declared size and the destination rect against the destination's declared
/// size, and returns `false` before touching a pixel when either is empty
/// (`reference/cpp/core/visual/LayerBitmapIntf.cpp:969-1021`).
#[test]
fn a_bitmap_declaring_pixels_it_does_not_hold_is_left_alone() {
    const FULL: (i32, i32, i32, i32) = (0, 0, 4, 4);
    let source = sample(4, 4, 11);
    let mut destination = sample(4, 4, 12);
    let intact = destination.rgba.clone();
    let mut pixel_less = BitmapState::new(4, 4, Vec::new());
    let options = BlitOptions {
        mode: 0,
        opa: 255,
        treat_as_opaque: false,
        hold_alpha: false,
    };

    // A source with a declared size but no pixel bytes contributes nothing.
    blit_copy(&mut destination, 0, 0, &pixel_less, FULL);
    blit_plane(
        &mut destination,
        0,
        0,
        &pixel_less,
        FULL,
        FULL,
        COPY_MAIN | COPY_MASK,
    );
    blit_over(
        &mut destination,
        0,
        0,
        BlitSource {
            bitmap: &pixel_less,
            rect: FULL,
        },
        FULL,
        options,
    );
    tile_rect(
        &mut destination,
        Rect::new(0, 0, 4, 4),
        &pixel_less,
        FULL,
        (0, 0),
    );
    stretch_blit(&mut destination, FULL, &pixel_less, FULL, 0);
    // Axis-aligned `stNearest` takes the scanline pass.
    affine_blit(
        &mut destination,
        AffineBlit {
            corners: [(0.0, 0.0), (4.0, 0.0), (0.0, 4.0)],
            stretch_type: 0,
            clear: false,
            clear_color: [0, 0, 0, 0],
        },
        BlitSource {
            bitmap: &pixel_less,
            rect: FULL,
        },
        options,
    );
    assert_eq!(
        destination.rgba, intact,
        "a pixel-less source draws nothing"
    );

    // A destination with a declared size but no pixel bytes takes nothing and
    // keeps its empty buffer instead of aborting.
    fill_color_on_alpha(&mut pixel_less, FULL, [9, 8, 7, 255], 255);
    fill_mask(&mut pixel_less, FULL, 7);
    do_gray_scale(&mut pixel_less, FULL);
    flip_lr(&mut pixel_less);
    blur_bitmap_in_place(&mut pixel_less, 2, 2);
    gaussian_blur(&mut pixel_less, FULL, 2, 1.0);
    blit_copy(&mut pixel_less, 0, 0, &source, FULL);
    blit_plane(&mut pixel_less, 0, 0, &source, FULL, FULL, COPY_MASK);
    blit_over(
        &mut pixel_less,
        0,
        0,
        BlitSource {
            bitmap: &source,
            rect: FULL,
        },
        FULL,
        options,
    );
    tile_rect(
        &mut pixel_less,
        Rect::new(0, 0, 4, 4),
        &source,
        FULL,
        (0, 0),
    );
    stretch_blit(&mut pixel_less, FULL, &source, FULL, 0);
    // Fractional corners take the generic (per-pixel) affine pass.
    affine_blit(
        &mut pixel_less,
        AffineBlit {
            corners: [(0.0, 0.0), (3.5, 0.25), (0.25, 3.5)],
            stretch_type: 2,
            clear: true,
            clear_color: [1, 2, 3, 4],
        },
        BlitSource {
            bitmap: &source,
            rect: FULL,
        },
        BlitOptions {
            mode: 2,
            opa: 128,
            ..options
        },
    );
    assert!(
        pixel_less.rgba.is_empty(),
        "a pixel-less destination stays empty"
    );

    // `copy_9patch` already reports failure through its `Option`.
    let mut patch_target = BitmapState::new(16, 16, Vec::new());
    assert!(copy_9patch(&mut patch_target, &sample(16, 16, 13)).is_none());
    assert!(patch_target.rgba.is_empty());
}

/// A fill of an image that declares its size and holds no pixels writes
/// nothing and does not abort.
///
/// The reference fills the clipped rectangle of the layer's image
/// (`reference/cpp/core/visual/LayerIntf.cpp:4272-4286`,
/// `reference/cpp/core/visual/LayerBitmapIntf.cpp:286-303`); the image
/// always holds its pixels there, so an image that declares the probed size
/// without a buffer has nothing to fill.
#[test]
fn fill_rect_replace_skips_an_image_without_pixels() {
    let mut bmp = BitmapState::new(4, 4, Vec::new());
    fill_rect_replace(&mut bmp, 0, 0, 4, 4, [1, 2, 3, 4]);
    assert!(bmp.rgba.is_empty(), "an image without pixels is not filled");
    assert!(
        !bmp.dirty,
        "an image without pixels is not modified by the fill"
    );
}

/// A transition that blends into an image that declares its size and holds no
/// pixels writes nothing and does not abort.
///
/// The reference's cross-fade blends the two sources into the destination
/// (`reference/cpp/core/visual/TransIntf.cpp:584-612`,
/// `reference/cpp/core/visual/TransIntf.cpp:614-701`); the images always hold
/// their pixels there, so a destination that declares the probed size without
/// a buffer has nothing to write.
#[test]
fn a_transition_over_an_image_without_pixels_writes_nothing() {
    let src1 = sample(4, 4, 31);
    let src2 = sample(4, 4, 32);
    let mut dst = BitmapState::new(4, 4, Vec::new());
    let mut trans = Trans::cross_fade(100.0).expect("a cross-fade");
    // The midpoint phase, so the blend runs instead of an endpoint copy.
    trans.step(50.0);
    trans
        .apply(&mut dst, &src1, &src2, (0, 0, 4, 4))
        .expect("the transition applies");
    assert!(
        dst.rgba.is_empty(),
        "an image without pixels is not written by the transition"
    );
}

/// An affine blit marks the rectangle it wrote, not the whole bitmap.
///
/// The frame's upload sends only the marked region to the texture, so a
/// blit that covered one corner does not re-upload the whole image.
#[test]
fn an_affine_blit_marks_only_the_rectangle_it_wrote() {
    let source = bitmap(4, 4, vec![255u8; 4 * 4 * 4]);
    let mut destination = bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    affine_blit(
        &mut destination,
        AffineBlit {
            corners: [(2.0, 3.0), (6.0, 3.0), (2.0, 7.0)],
            stretch_type: 0,
            clear: false,
            clear_color: [0, 0, 0, 0],
        },
        BlitSource {
            bitmap: &source,
            rect: (0, 0, 4, 4),
        },
        BlitOptions {
            mode: 2,
            opa: 255,
            treat_as_opaque: false,
            hold_alpha: false,
        },
    );
    assert!(destination.dirty, "the blit marks the bitmap dirty");
    assert_eq!(
        destination.dirty_region,
        Some(PixelRegion::new(2, 3, 5, 5)),
        "only the mapped rectangle is marked"
    );
}

/// A clipped affine blit writes inside the clip alone, and the pixels it
/// writes there are the ones the unclipped blit writes.
///
/// The caller of a clipped blit needs no copy of the destination to restore
/// the pixels outside the clip, because the pass never touches them.
#[test]
fn a_clipped_affine_blit_writes_inside_the_clip_alone() {
    let source = bitmap(4, 4, vec![255u8; 4 * 4 * 4]);
    let affine = AffineBlit {
        corners: [(2.0, 2.0), (10.0, 2.0), (2.0, 10.0)],
        stretch_type: 0,
        clear: false,
        clear_color: [0, 0, 0, 0],
    };
    let options = BlitOptions {
        mode: 2,
        opa: 255,
        treat_as_opaque: false,
        hold_alpha: false,
    };
    let mut unclipped = bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    let mut clipped = bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    affine_blit(
        &mut unclipped,
        affine,
        BlitSource {
            bitmap: &source,
            rect: (0, 0, 4, 4),
        },
        options,
    );
    affine_blit_clipped(
        &mut clipped,
        affine,
        BlitSource {
            bitmap: &source,
            rect: (0, 0, 4, 4),
        },
        options,
        (4, 4, 8, 8),
    );

    for y in 0..16usize {
        for x in 0..16usize {
            let i = (y * 16 + x) * 4;
            let inside = (4..8).contains(&x) && (4..8).contains(&y);
            if inside {
                assert_eq!(
                    clipped.rgba[i..i + 4],
                    unclipped.rgba[i..i + 4],
                    "the clipped blit writes the pixels of the unclipped one"
                );
            } else {
                assert_eq!(
                    clipped.rgba[i..i + 4],
                    [0, 0, 0, 0],
                    "the pixels outside the clip are untouched"
                );
            }
        }
    }
    assert_eq!(
        clipped.dirty_region,
        Some(PixelRegion::new(4, 4, 4, 4)),
        "only the clipped rectangle is marked"
    );
}

/// A whole-subtree transition gets a display bitmap distinct from a script
/// cache. Reusing the cache as the blend target would overwrite pixels the
/// script owns and make the cache stale on every transition tick.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7961-7964
#[test]
fn a_transition_display_never_reuses_the_script_cache() {
    let mut scene = Scene::default();
    let window = scene.add_window("t", (8, 8));
    let layer = scene.add_layer(window, None);
    let cache = scene.add_bitmap(4, 4, vec![1u8; 4 * 4 * 4]);
    {
        let state = scene.layer_mut(layer).expect("layer");
        state.cache_enabled_count = 1;
        state.cache_bitmap = Some(cache);
        state.cache_content = Some(cache);
    }

    let display = ensure_transition_display(&mut scene, layer, 4, 4).expect("display");
    assert_ne!(display, cache, "the display is not the script cache");
    assert_eq!(
        scene.layer(layer).expect("layer").cache_content,
        Some(cache),
        "the composited content stays available as the frame source"
    );
    assert_eq!(
        scene.layer(layer).expect("layer").cache_bitmap,
        Some(display),
        "the display takes the transition slot"
    );

    let reused = ensure_transition_display(&mut scene, layer, 4, 4).expect("display");
    assert_eq!(
        reused, display,
        "a running transition reuses its own display"
    );
}

/// The pixel-centre convention maps the half-pixel corner back to the pixel
/// boundary: a 1:1 copy is the source exactly, including its alpha.
#[test]
fn a_pixel_center_one_to_one_affine_copy_is_exact() {
    let source = sample(4, 4, 31);
    let mut destination = bitmap(4, 4, vec![0u8; 4 * 4 * 4]);
    affine_blit_clipped_pixel_center(
        &mut destination,
        AffineBlit {
            corners: [(-0.5, -0.5), (3.5, -0.5), (-0.5, 3.5)],
            stretch_type: 1,
            clear: false,
            clear_color: [0, 0, 0, 0],
        },
        BlitSource {
            bitmap: &source,
            rect: (0, 0, 4, 4),
        },
        BlitOptions {
            mode: 1,
            opa: 255,
            treat_as_opaque: false,
            hold_alpha: false,
        },
        (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
    );
    assert_eq!(
        destination.rgba, source.rgba,
        "the half-origin fast path copies exact texels"
    );
}

/// A large stretch pass splits its rows across bands; a solid source scaled
/// into every destination pixel must produce that solid color everywhere.
#[test]
fn a_large_stretch_blit_matches_the_solid_source() {
    let source = bitmap(2, 2, vec![10, 20, 30, 255].repeat(4));
    let mut destination = bitmap(1024, 1024, vec![0u8; 1024 * 1024 * 4]);
    stretch_blit(
        &mut destination,
        (0, 0, 1024, 1024),
        &source,
        (0, 0, 2, 2),
        0,
    );
    assert!(
        destination
            .rgba
            .chunks_exact(4)
            .all(|pixel| pixel == [10, 20, 30, 255]),
        "the stretch row bands write the solid source"
    );
}

/// A large source-over pass splits its rows across bands; the bytes must be
/// identical to the single-threaded path.
#[test]
fn a_large_blit_over_matches_the_single_pixel_path() {
    let source = bitmap(1024, 1024, vec![128u8; 1024 * 1024 * 4]);
    let mut destination = bitmap(1024, 1024, vec![0u8; 1024 * 1024 * 4]);
    blit_over(
        &mut destination,
        0,
        0,
        BlitSource {
            bitmap: &source,
            rect: (0, 0, 1024, 1024),
        },
        (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
        BlitOptions {
            mode: 0,
            opa: 255,
            treat_as_opaque: false,
            hold_alpha: false,
        },
    );
    assert!(
        destination
            .rgba
            .chunks_exact(4)
            .all(|pixel| pixel == [128, 128, 128, 128]),
        "the row bands write the same straight-alpha result"
    );
}

/// An `operateRect` marks the rectangle it wrote, not the whole bitmap.
///
/// The frame's upload sends only the marked region to the texture, so the
/// icons the motion player draws do not re-upload the whole target image.
#[test]
fn an_operate_rect_marks_only_the_rectangle_it_wrote() {
    let source = bitmap(4, 4, vec![255u8; 4 * 4 * 4]);
    let options = BlitOptions {
        mode: 2,
        opa: 255,
        treat_as_opaque: false,
        hold_alpha: false,
    };

    let mut destination = bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    operate_rect(
        &mut destination,
        3,
        5,
        BlitSource {
            bitmap: &source,
            rect: (0, 0, 4, 4),
        },
        options,
    );
    assert!(destination.dirty, "the blit marks the bitmap dirty");
    assert_eq!(
        destination.dirty_region,
        Some(PixelRegion::new(3, 5, 4, 4)),
        "only the written rectangle is marked"
    );

    // A rectangle that runs past the bitmap is marked clipped.
    let mut destination = bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    operate_rect(
        &mut destination,
        14,
        14,
        BlitSource {
            bitmap: &source,
            rect: (0, 0, 4, 4),
        },
        options,
    );
    assert_eq!(
        destination.dirty_region,
        Some(PixelRegion::new(14, 14, 2, 2)),
        "the marked rectangle is the clipped one"
    );
}

/// A `copyRect` marks the rectangle it copied, not the whole bitmap.
///
/// The frame's upload sends only the marked region to the texture, so the
/// character sprite a game copies a face into is not re-uploaded whole: the
/// layer holds the full-resolution sprite and only the face rectangle
/// changes.
#[test]
fn a_copy_rect_marks_only_the_rectangle_it_copied() {
    let source = bitmap(4, 4, vec![255u8; 4 * 4 * 4]);
    let mut destination = bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    blit_plane(
        &mut destination,
        3,
        5,
        &source,
        (0, 0, 4, 4),
        (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
        COPY_MAIN | COPY_MASK,
    );
    assert!(destination.dirty, "the copy marks the bitmap dirty");
    assert_eq!(
        destination.dirty_region,
        Some(PixelRegion::new(3, 5, 4, 4)),
        "only the copied rectangle is marked"
    );

    // A copy that runs past the bitmap is marked clipped.
    let mut destination = bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    blit_plane(
        &mut destination,
        14,
        14,
        &source,
        (0, 0, 4, 4),
        (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
        COPY_MAIN | COPY_MASK,
    );
    assert_eq!(
        destination.dirty_region,
        Some(PixelRegion::new(14, 14, 2, 2)),
        "the marked rectangle is the clipped one"
    );
}

/// A copy clipped to a rectangle marks the clipped rectangle alone.
#[test]
fn a_clipped_copy_marks_only_the_clipped_rectangle() {
    let source = bitmap(4, 4, vec![255u8; 4 * 4 * 4]);
    let mut destination = bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    blit_copy(&mut destination, 2, 2, &source, (0, 0, 4, 4));
    assert_eq!(
        destination.dirty_region,
        Some(PixelRegion::new(2, 2, 4, 4)),
        "the copied rectangle is marked"
    );
}

/// A `Blt` source-over marks the rectangle it wrote, not the whole bitmap.
#[test]
fn a_blt_marks_only_the_rectangle_it_wrote() {
    let source = bitmap(4, 4, vec![255u8; 4 * 4 * 4]);
    let options = BlitOptions {
        mode: 2,
        opa: 255,
        treat_as_opaque: false,
        hold_alpha: false,
    };
    let mut destination = bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    blit_over(
        &mut destination,
        2,
        6,
        BlitSource {
            bitmap: &source,
            rect: (0, 0, 4, 4),
        },
        (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
        options,
    );
    assert!(destination.dirty, "the blit marks the bitmap dirty");
    assert_eq!(
        destination.dirty_region,
        Some(PixelRegion::new(2, 6, 4, 4)),
        "only the written rectangle is marked"
    );
}

/// A `stretchCopy` marks the destination rectangle it resampled into.
#[test]
fn a_stretch_copy_marks_only_the_destination_rectangle() {
    let source = bitmap(4, 4, vec![255u8; 4 * 4 * 4]);
    let mut destination = bitmap(16, 16, vec![0u8; 16 * 16 * 4]);
    stretch_blit(&mut destination, (2, 3, 10, 9), &source, (0, 0, 4, 4), 0);
    assert!(destination.dirty, "the copy marks the bitmap dirty");
    assert_eq!(
        destination.dirty_region,
        Some(PixelRegion::new(2, 3, 8, 6)),
        "only the destination rectangle is marked"
    );
}
