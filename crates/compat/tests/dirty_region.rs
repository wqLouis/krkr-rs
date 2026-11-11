//! The dirty region a script's pixel write leaves behind.
//!
//! The frame's upload sends the rectangle a write touched, not the whole
//! image: the reference marks the changed rectangle
//! (`reference/cpp/core/visual/LayerIntf.cpp:2925-2941`,
//! `reference/cpp/core/visual/LayerIntf.cpp:6305-6345`,
//! `reference/cpp/core/visual/LayerIntf.cpp:6347-6358`), and the engine's
//! upload sends that rectangle alone.
//!
//! What a script reads is unchanged: the pixels the write left are the
//! pixels the reference leaves. Only the upload differs.

mod common;

use api::geometry::PixelRegion;
use common::vm_lock;

/// The main image of the layer the script named `name`.
fn main_image(env: &common::Env, name: &str) -> u32 {
    env.scene
        .layers
        .iter()
        .rev()
        .find(|layer| layer.name == name)
        .and_then(|layer| layer.bitmap)
        .unwrap_or_else(|| panic!("the layer {name:?} has no main image"))
}

/// The dirty region the image of the layer named `name` carries.
fn dirty_region(env: &common::Env, name: &str) -> Option<PixelRegion> {
    let image = main_image(env, name);
    let bitmap = env.scene.bitmap(image).expect("the layer image");
    assert!(bitmap.dirty, "the write marks the image dirty");
    bitmap.dirty_region
}

/// A layer with a 4x4 image, named `l`, whose image starts clean.
fn layer_with_clean_image(env: &mut common::Env) {
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.name = 'l'; l.setImageSize(4, 4);",
    );
    // The image was just created, so it is dirty as a whole; the region under
    // test is the one the write leaves.
    let image = main_image(env, "l");
    assert!(env.scene.set_bitmap_clean(image), "the image is cleaned");
}

/// A colour write marks the one pixel it wrote.
#[test]
fn a_main_pixel_write_marks_one_pixel() {
    let _lock = vm_lock();
    let mut env = common::Env::new();
    layer_with_clean_image(&mut env);

    env.run("l.setMainPixel(2, 1, 0x00ff00);");
    assert_eq!(
        dirty_region(&env, "l"),
        Some(PixelRegion::new(2, 1, 1, 1)),
        "the write marks the one pixel it wrote"
    );
}

/// A mask write marks the one pixel it wrote.
#[test]
fn a_mask_pixel_write_marks_one_pixel() {
    let _lock = vm_lock();
    let mut env = common::Env::new();
    layer_with_clean_image(&mut env);

    env.run("l.setMaskPixel(3, 2, 128);");
    assert_eq!(
        dirty_region(&env, "l"),
        Some(PixelRegion::new(3, 2, 1, 1)),
        "the write marks the one pixel it wrote"
    );
}

/// A write outside the image marks nothing: the reference's clip-rect check
/// returns before the write.
#[test]
fn a_pixel_write_outside_the_image_marks_nothing() {
    let _lock = vm_lock();
    let mut env = common::Env::new();
    layer_with_clean_image(&mut env);

    env.run("l.setMainPixel(9, 9, 0x00ff00);");
    let image = main_image(&env, "l");
    let bitmap = env.scene.bitmap(image).expect("the layer image");
    assert!(!bitmap.dirty, "a write outside the image changes nothing");
    assert_eq!(bitmap.dirty_region, None, "a clean image has no region");
}

/// A rectangle fill marks the rectangle it filled.
#[test]
fn a_fill_marks_the_rectangle_it_filled() {
    let _lock = vm_lock();
    let mut env = common::Env::new();
    layer_with_clean_image(&mut env);

    env.run("l.fillRect(1, 1, 2, 2, 0xff0000);");
    assert_eq!(
        dirty_region(&env, "l"),
        Some(PixelRegion::new(1, 1, 2, 2)),
        "the fill marks the rectangle it filled"
    );
}

/// A `Bitmap`'s own pixel write marks the one pixel it wrote.
#[test]
fn a_bitmap_pixel_write_marks_one_pixel() {
    let _lock = vm_lock();
    let mut env = common::Env::new();
    env.run("var b = new Bitmap(4, 4);");
    // The script's own image is the one the scene marks as script-owned.
    let image = env
        .scene
        .bitmaps
        .iter()
        .rev()
        .find(|bitmap| bitmap.script_owned)
        .expect("the script image")
        .id;
    assert!(env.scene.set_bitmap_clean(image), "the image is cleaned");

    env.run("b.setPixel(1, 3, 0x0000ff);");
    let bitmap = env.scene.bitmap(image).expect("the script image");
    assert!(bitmap.dirty, "the write marks the image dirty");
    assert_eq!(
        bitmap.dirty_region,
        Some(PixelRegion::new(1, 3, 1, 1)),
        "the write marks the one pixel it wrote"
    );
}
