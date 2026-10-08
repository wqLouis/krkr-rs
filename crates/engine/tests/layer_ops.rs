//! Integration tests for the in-place CPU layer operations.
//!
//! These lock in the behaviour of the separable running-sum box blur
//! (`blur_bitmap_in_place`) against the original 2-D summation it replaced.

use api::blend::BlendMode;
use engine::BitmapState;
use engine::Scene;
use engine::layer_ops::{blur_bitmap_in_place, composite_primary_layer};
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
