//! Shared helpers for the `app` integration tests.

use api::blend::BlendMode;
use app::{Host, HostConfig};
use engine::scene::{Rect, Scene};

/// Creates a headless [`Host`] using the explicit headless configuration.
pub fn headless_host() -> Host {
    Host::new(HostConfig::headless()).expect("headless host")
}

/// Populates `scene` with a minimal two-layer scene: an opaque background
/// image covering the window and a small bitmap sprite drawn over it.
///
/// Returns the id of the window that owns both layers.
pub fn build_solid_and_bitmap_scene(scene: &mut Scene) -> u32 {
    let window = scene.add_window("test window", (320, 240));

    // Opaque background image. The first layer added becomes the window's
    // primary layer, which `Scene::add_layer` already forces visible.
    let background = scene.add_layer(window, None);
    let background_bitmap = scene.add_bitmap(320, 240, [24u8, 28, 40, 255].repeat(320 * 240));
    if let Some(layer) = scene.layer_mut(background) {
        layer.visible = true;
        layer.rect = Rect::new(0, 0, 320, 240);
        layer.bitmap = Some(background_bitmap);
        layer.image_width = 320;
        layer.image_height = 240;
        layer.blend = BlendMode::Replace;
    }

    // Small bitmap sprite blended over the background.
    let bitmap = scene.add_bitmap(2, 2, vec![200; 2 * 2 * 4]);
    let sprite = scene.add_layer(window, None);
    if let Some(layer) = scene.layer_mut(sprite) {
        layer.visible = true;
        layer.rect = Rect::new(8, 8, 2, 2);
        layer.bitmap = Some(bitmap);
        layer.blend = BlendMode::SourceOver;
    }

    window
}
