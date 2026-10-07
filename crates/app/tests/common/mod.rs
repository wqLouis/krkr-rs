//! Shared helpers for the `app` integration tests.

use app::{Host, HostConfig};
use engine::scene::{LT_ALPHA, LT_OPAQUE, Rect, Scene};

/// Creates a headless [`Host`] using the explicit headless configuration.
pub fn headless_host() -> Host {
    Host::new(HostConfig::headless()).expect("headless host")
}

/// Populates `scene` with a minimal two-layer scene: an opaque solid fill
/// covering the window and a small bitmap sprite drawn over it.
///
/// Returns the id of the window that owns both layers.
pub fn build_solid_and_bitmap_scene(scene: &mut Scene) -> u32 {
    let window = scene.add_window("test window", (320, 240));

    // Opaque background fill. The first layer added becomes the window's
    // primary layer, which `Scene::add_layer` already forces visible.
    let background = scene.add_layer(window, None);
    if let Some(layer) = scene.layer_mut(background) {
        layer.visible = true;
        layer.rect = Rect {
            x: 0,
            y: 0,
            w: 320,
            h: 240,
        };
        layer.fill_color = Some([24, 28, 40, 255]);
        layer.blend_type = LT_OPAQUE;
    }

    // Small bitmap sprite blended over the fill.
    let bitmap = scene.add_bitmap(2, 2, vec![200; 2 * 2 * 4]);
    let sprite = scene.add_layer(window, None);
    if let Some(layer) = scene.layer_mut(sprite) {
        layer.visible = true;
        layer.rect = Rect {
            x: 8,
            y: 8,
            w: 2,
            h: 2,
        };
        layer.bitmap = Some(bitmap);
        layer.blend_type = LT_ALPHA;
    }

    window
}
