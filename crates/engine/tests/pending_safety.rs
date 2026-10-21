//! Safety of the pending and empty pixel paths.
//!
//! A bitmap may declare a size while its pixels have not landed, and a
//! province plane may be shorter than the size it declares. Every read and
//! write of those buffers is a typed miss or a reported no-op instead of an
//! out-of-bounds index.

use api::media::RgbaFrame;
use engine::video::blit_frame_to_layer;
use engine::{BitmapState, Rect, Scene};

/// A pixel's byte offset is its row's scan line plus the column: a scan line
/// is `row * pitch` from the first, and a 32-bit pixel is four bytes
/// (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:443-450`). A
/// coordinate outside the declared size has no pixel
/// (`reference/cpp/core/visual/LayerBitmapIntf.cpp:165-171`), and neither has
/// a size whose byte count is not representable.
#[test]
fn pixel_offset_is_total_for_absurd_declared_sizes() {
    let bitmap = BitmapState::new(4, 3, vec![0; 48]);
    // Row 2, column 3, four bytes per pixel.
    assert_eq!(bitmap.pixel_offset(3, 2), Some(44));
    assert_eq!(bitmap.pixel_offset(4, 0), None);
    assert_eq!(bitmap.pixel_offset(0, 3), None);

    // A declared size whose byte count exceeds `usize` has no address at all,
    // rather than a wrapped row offset.
    let impossible = BitmapState::new(u32::MAX, u32::MAX, Vec::new());
    assert_eq!(impossible.pixel_offset(u32::MAX - 1, u32::MAX - 1), None);
    assert_eq!(impossible.pixel_offset(0, u32::MAX - 1), None);
}

/// `GetProvincePixel` returns 0 outside the plane
/// (`reference/cpp/core/visual/LayerIntf.cpp:2973-2981`), `SetProvincePixel`
/// stores only inside it (`reference/cpp/core/visual/LayerIntf.cpp:2985-3002`),
/// and `htProvince` misses outside it
/// (`reference/cpp/core/visual/LayerIntf.cpp:3262-3278`).
///
/// A plane that holds fewer bytes than its declared size is outside itself
/// from its first missing byte, so those accesses are that miss instead of an
/// out-of-bounds index.
#[test]
fn a_short_province_plane_is_a_miss_and_a_no_op() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (4, 4));
    let primary = scene.add_layer(win, None);
    let child = scene.add_layer(win, Some(primary));
    let image = scene.add_bitmap(4, 4, vec![0; 64]);
    {
        let layer = scene.layer_mut(child).expect("child layer");
        layer.rect = Rect::new(0, 0, 4, 4);
        layer.visible = true;
        layer.bitmap = Some(image);
        layer.hit_type = 1; // htProvince
        // The plane declares 4x4 but carries only its first row.
        layer.province = Some(vec![7; 4].into_boxed_slice());
        layer.province_width = 4;
        layer.province_height = 4;
    }

    // reference/cpp/core/visual/LayerIntf.cpp:2973-2981
    assert_eq!(scene.province_pixel(child, 0, 0), 7);
    assert_eq!(scene.province_pixel(child, 0, 1), 0);
    assert_eq!(scene.province_pixel(child, -1, 0), 0);

    // reference/cpp/core/visual/LayerIntf.cpp:2985-3002
    scene.set_province_pixel(child, 0, 1, 9);
    assert_eq!(scene.province_pixel(child, 0, 1), 0);
    assert_eq!(
        scene.with_province_pixels(child, |plane| plane.to_vec()),
        Some(vec![7; 4])
    );

    // reference/cpp/core/visual/LayerIntf.cpp:3262-3278
    assert_eq!(scene.layer_at(child, 0, 0, false, false), Some(child));
    assert_eq!(scene.layer_at(child, 0, 1, false, false), None);
}

/// The overlay mixes the decoded picture into the bound layer's MainImage
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:882-914`:
/// `SetMixingBitmap(l->GetMainImage(), alpha)`).
///
/// A MainImage that declares a size but holds no pixel bytes cannot take the
/// copy: the frame copy reports the inconsistency and fails instead of
/// claiming a copy that wrote nothing.
#[test]
fn a_destination_without_pixels_refuses_the_frame() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let layer = scene.add_layer(win, None);
    let image = scene.add_bitmap(2, 2, Vec::new());
    {
        let state = scene.layer_mut(layer).expect("layer");
        state.rect = Rect::new(0, 0, 2, 2);
        state.bitmap = Some(image);
    }
    let frame = RgbaFrame::new(2, 2, 0, vec![255; 16]);
    assert!(!blit_frame_to_layer(&mut scene, layer, &frame, false));
    assert!(
        scene.bitmap(image).expect("image").rgba.is_empty(),
        "the refused copy writes nothing"
    );
}

/// A consistent frame is still copied into the layer's MainImage
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:912`).
#[test]
fn a_valid_frame_still_blits() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (2, 2));
    let layer = scene.add_layer(win, None);
    let image = scene.add_bitmap(2, 2, vec![0; 16]);
    {
        let state = scene.layer_mut(layer).expect("layer");
        state.rect = Rect::new(0, 0, 2, 2);
        state.bitmap = Some(image);
    }
    let pixels: Vec<u8> = (0u8..16).collect();
    let frame = RgbaFrame::new(2, 2, 0, pixels.clone());
    assert!(blit_frame_to_layer(&mut scene, layer, &frame, false));
    assert_eq!(scene.bitmap(image).expect("image").rgba, pixels);
}
