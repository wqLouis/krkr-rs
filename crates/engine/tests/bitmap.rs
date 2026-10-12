//! Ownership tests for a scene bitmap held by a script object.
//!
//! The reference shares a bitmap through a reference count: a script
//! `Bitmap` and a layer's MainImage each hold one reference, and the pixel
//! buffer is freed with the last one
//! (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:495-503`).
//! [`Scene::release_script_bitmap`] drops the script object's reference.

use engine::Scene;

/// Releasing an unreferenced script bitmap frees its image.
#[test]
fn release_script_bitmap_frees_an_unreferenced_image() {
    let mut scene = Scene::default();
    let id = scene.add_bitmap(2, 2, vec![0; 16]);
    scene.bitmap_mut(id).unwrap().script_owned = true;

    // reference/cpp/core/visual/BitmapIntf.cpp:46-49: the object's destruction
    // drops its reference; nothing else holds the bitmap, so it is freed.
    assert!(scene.release_script_bitmap(id), "the last holder frees it");
    assert!(scene.bitmap(id).is_none());
}

/// A layer's MainImage keeps the shared bitmap alive after the script object
/// releases its claim; the image is freed when the layer dies.
#[test]
fn release_script_bitmap_keeps_a_layer_referenced_image() {
    let mut scene = Scene::default();
    let win = scene.add_window("t", (8, 8));
    let layer = scene.add_layer(win, None);
    let id = scene.add_bitmap(2, 2, [0, 1, 2, 3].repeat(4));
    scene.bitmap_mut(id).unwrap().script_owned = true;
    scene.layer_mut(layer).unwrap().bitmap = Some(id);

    // reference/cpp/core/visual/LayerIntf.cpp:2432-2455:
    // `AssignMainImageWithUpdate` shares the bitmap, so the layer holds a
    // reference after the script object releases its own.
    assert!(!scene.release_script_bitmap(id), "the layer still holds it");
    assert!(scene.bitmap(id).is_some());

    // reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:495-503: the layer's
    // destructor releases the last reference, freeing the pixels.
    scene.destroy_layer(layer);
    assert!(scene.bitmap(id).is_none(), "the last holder frees it");
}
