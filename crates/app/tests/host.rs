//! Integration tests for the `app` host assembly.
//!
//! These exercise only the public API of `app` and build their own
//! minimal scenes; the host itself ships no demo content.

mod common;

use api::host::SurfaceEvent;
use common::{build_solid_and_bitmap_scene, headless_host};

#[test]
fn headless_frame_draws_solid_and_texture_nodes() {
    let mut host = headless_host();
    build_solid_and_bitmap_scene(host.scene_mut());

    let report = host.step(16.0).expect("step");
    let stats = report.stats.expect("null frame stats");
    assert!(stats.solid_draws >= 1, "expected a solid fill layer");
    assert!(stats.texture_draws >= 1, "expected a bitmap layer");
    assert_eq!(stats.missing_textures, 0, "dirty bitmaps must be uploaded");
    assert_eq!(report.frame_index, 1);
}

#[test]
fn dirty_bitmaps_create_and_upload_one_texture() {
    let mut host = headless_host();
    let bitmap = host.scene_mut().add_bitmap(2, 1, vec![9; 8]);
    assert!(host.textures().is_empty());

    host.step(16.0).expect("step");
    assert_eq!(host.textures().len(), 1);
    assert!(!host.scene().bitmap(bitmap).expect("bitmap").dirty);

    // A clean bitmap must not be re-uploaded or re-created.
    host.step(16.0).expect("step");
    assert_eq!(host.textures().len(), 1);
}

#[test]
fn resize_updates_surface_size() {
    let mut host = headless_host();
    let event = host.resize((640, 360)).expect("resize");
    assert_eq!(event, SurfaceEvent::Resized(640, 360));
    assert_eq!(host.surface_size(), (640, 360));
}

#[test]
fn frame_index_and_elapsed_advance() {
    let mut host = headless_host();
    for _ in 0..3 {
        host.step(10.0).expect("step");
    }
    assert_eq!(host.frame_index(), 3);
    assert!((host.elapsed_ms() - 30.0).abs() < f64::EPSILON);
}

#[test]
fn unchanged_revision_reuses_arena_and_mutation_reprojects() {
    let mut host = headless_host();
    build_solid_and_bitmap_scene(host.scene_mut());

    // The first frame always projects, whatever the revision.
    host.step(16.0).expect("step");
    let projected_len = host.render_scene().nodes.len();
    let projected_ptr = host.render_scene().nodes.as_ptr();
    let projected_capacity = host.render_scene().nodes.capacity();
    assert!(projected_len > 1, "expected projected nodes");
    let snapshot = host.render_scene().clone();

    // A frame with an unchanged revision reuses the persistent arena: the
    // node buffer keeps its identity/capacity and the projection is untouched.
    host.step(16.0).expect("step");
    assert_eq!(host.render_scene().nodes.len(), projected_len);
    assert_eq!(host.render_scene().nodes.as_ptr(), projected_ptr);
    assert_eq!(host.render_scene().nodes.capacity(), projected_capacity);
    assert_eq!(host.render_scene(), &snapshot);

    // A direct field edit does not bump `Scene::revision`, so the projection
    // is deliberately skipped (the documented over-approximation contract):
    // an unchanged revision must not rebuild the render scene.
    host.scene_mut().layers[0].rect.w = 123;
    host.step(16.0).expect("step");
    assert_eq!(host.render_scene(), &snapshot);

    // Any `*_mut` accessor bumps the revision, so the next frame reprojects
    // and the new layer geometry reaches the render tree.
    let background = host.scene().layers[0].id;
    if let Some(layer) = host.scene_mut().layer_mut(background) {
        layer.rect.w = 160;
    }
    host.step(16.0).expect("step");
    assert_ne!(host.render_scene(), &snapshot);
}
