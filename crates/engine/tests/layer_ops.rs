//! Integration tests for the in-place CPU layer operations.
//!
//! These lock in the behaviour of the separable running-sum box blur
//! (`blur_bitmap_in_place`) against the original 2-D summation it replaced.

use api::blend::BlendMode;
use engine::BitmapState;
use engine::Scene;
use engine::layer_ops::{blur_bitmap_in_place, composite_primary_layer, render_layer_subtree};
use engine::raster::fill_rect_replace;
use engine::scene::Rect;

fn bitmap(width: u32, height: u32, rgba: Vec<u8>) -> BitmapState {
    BitmapState::new(width, height, rgba)
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

/// The primary composite resolves each layer's generic `BlendMode` to the
/// right pixel operation (the bridge L3 relies on for `Layer.type`).
#[test]
fn primary_composite_honours_the_layer_blend_mode() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let primary = scene.add_layer(win, None);
    scene.layer_mut(primary).unwrap().rect = Rect::new(0, 0, 2, 2);

    // Opaque grey background.
    let background = scene.add_layer(win, Some(primary));
    {
        let l = scene.layer_mut(background).unwrap();
        l.visible = true;
        l.rect = Rect::new(0, 0, 2, 2);
        l.fill_color = Some([100, 100, 100, 255]);
    }

    // Multiplicative grey foreground: 100 * 50 / 255 = 19, not the 50 a
    // source-over composite would produce.
    let foreground = scene.add_layer(win, Some(primary));
    {
        let l = scene.layer_mut(foreground).unwrap();
        l.visible = true;
        l.rect = Rect::new(0, 0, 2, 2);
        l.fill_color = Some([50, 50, 50, 255]);
        l.blend = BlendMode::Multiply;
    }

    let bitmap_id = composite_primary_layer(&mut scene, primary).expect("composite");
    let rgba = &scene.bitmap(bitmap_id).unwrap().rgba;
    assert_eq!(&rgba[..4], &[19, 19, 19, 255]);
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
    {
        let l = scene.layer_mut(child).unwrap();
        l.rect = Rect::new(0, 0, 2, 2);
        l.visible = true;
        l.fill_color = Some([10, 20, 30, 255]);
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
