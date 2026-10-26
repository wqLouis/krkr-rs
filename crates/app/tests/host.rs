//! Integration tests for the `app` host assembly.
//!
//! These exercise only the public API of `app` and build their own
//! minimal scenes; the host itself ships no demo content.

mod common;

use api::config::CacheLimit;
use api::host::SurfaceEvent;
use app::InjectedFrameTime;
use common::{build_solid_and_bitmap_scene, headless_host};
use testkit::vm_lock;
use tjs::TjsValue;
use vm::VmHost;

/// The host applies the launcher's decoded-image cache policy to its scene.
#[test]
fn host_applies_the_configured_graphic_cache_limit() {
    let host = app::Host::new(
        app::HostConfig::headless().with_graphic_cache_limit(Some(CacheLimit::bytes(1_048_576))),
    )
    .expect("headless host");
    assert_eq!(
        host.scene().decoded_image_cache_limit(),
        CacheLimit::bytes(1_048_576)
    );
}

/// Evaluates `expression` and returns its integer result.
fn eval_int(vm: &VmHost, expression: &str) -> i64 {
    match vm.eval(expression, "t").expect("eval succeeds") {
        TjsValue::Integer(value) => value,
        other => panic!("expected Integer for {expression:?}, got {other:?}"),
    }
}

/// A unique temporary directory, removed by the caller.
fn unique_temp_dir(tag: &str) -> std::path::PathBuf {
    let mut base = std::env::temp_dir();
    base.push(format!(
        "krkr-app-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    base
}

/// Boots a headless [`app::GameHost`] over a temporary directory holding a
/// no-op `startup.tjs`, returning the host and the directory to remove.
fn open_minimal_game(tag: &str) -> (app::GameHost, std::path::PathBuf) {
    let base = unique_temp_dir(tag);
    std::fs::create_dir_all(&base).expect("game dir");
    std::fs::write(base.join("startup.tjs"), "// no-op startup\n").expect("startup script");
    let host = app::Host::new(app::HostConfig::headless()).expect("headless host");
    let game = app::GameHost::open(host, Vec::new(), base.clone()).expect("open game");
    (game, base)
}

#[test]
fn headless_frame_draws_texture_nodes() {
    let mut host = headless_host();
    build_solid_and_bitmap_scene(host.scene_mut());

    let report = host.step(&InjectedFrameTime::new(16.0)).expect("step");
    let stats = report.stats.expect("null frame stats");
    assert!(stats.texture_draws >= 2, "expected both layers as textures");
    assert_eq!(stats.missing_textures, 0, "dirty bitmaps must be uploaded");
    assert_eq!(report.frame_index, 1);
}

#[test]
fn dirty_bitmaps_create_and_upload_one_texture() {
    let mut host = headless_host();
    let bitmap = host.scene_mut().add_bitmap(2, 1, vec![9; 8]);
    assert!(host.textures().is_empty());

    host.step(&InjectedFrameTime::new(16.0)).expect("step");
    assert_eq!(host.textures().len(), 1);
    assert!(!host.scene().bitmap(bitmap).expect("bitmap").dirty);

    // A clean bitmap must not be re-uploaded or re-created.
    host.step(&InjectedFrameTime::new(16.0)).expect("step");
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
        host.step(&InjectedFrameTime::new(10.0)).expect("step");
    }
    assert_eq!(host.frame_index(), 3);
    assert!((host.elapsed_ms() - 30.0).abs() < f64::EPSILON);
}

#[test]
fn unchanged_revision_reuses_arena_and_mutation_reprojects() {
    let mut host = headless_host();
    build_solid_and_bitmap_scene(host.scene_mut());

    // The first frame always projects, whatever the revision.
    host.step(&InjectedFrameTime::new(16.0)).expect("step");
    let projected_len = host.render_scene().nodes.len();
    let projected_ptr = host.render_scene().nodes.as_ptr();
    let projected_capacity = host.render_scene().nodes.capacity();
    assert!(projected_len > 1, "expected projected nodes");
    let snapshot = host.render_scene().clone();

    // A frame with an unchanged structural revision reuses the persistent
    // arena: the node buffer keeps its identity/capacity and the projection is
    // untouched.
    host.step(&InjectedFrameTime::new(16.0)).expect("step");
    assert_eq!(host.render_scene().nodes.len(), projected_len);
    assert_eq!(host.render_scene().nodes.as_ptr(), projected_ptr);
    assert_eq!(host.render_scene().nodes.capacity(), projected_capacity);
    assert_eq!(host.render_scene(), &snapshot);

    // A direct field edit does not bump `Scene::structure_revision`, so the
    // projection is deliberately skipped (the documented over-approximation
    // contract): an unchanged structural revision must not rebuild the render
    // scene.
    host.scene_mut().layers[1].rect.w = 123;
    host.step(&InjectedFrameTime::new(16.0)).expect("step");
    assert_eq!(host.render_scene(), &snapshot);

    // Any `*_mut` accessor bumps the structural revision, so the next frame
    // reprojects and the new layer geometry reaches the render tree.
    let background = host.scene().layers[0].id;
    if let Some(layer) = host.scene_mut().layer_mut(background) {
        layer.rect.w = 160;
    }
    host.step(&InjectedFrameTime::new(16.0)).expect("step");
    assert_ne!(host.render_scene(), &snapshot);
}

#[test]
fn resize_changes_only_the_framebuffer() {
    let mut host = headless_host();
    let window = build_solid_and_bitmap_scene(host.scene_mut());
    let primary = host
        .scene()
        .window(window)
        .expect("window")
        .primary_layer
        .expect("primary layer");

    host.resize((800, 600)).expect("resize");

    // The framebuffer follows the OS resize and nothing a script observes
    // does: the only path from a window resize into the layer tree is the
    // manager's resize notification, which the reference leaves unused
    // (`reference/cpp/core/visual/LayerManager.cpp:334-343`).
    assert_eq!(host.surface_size(), (800, 600));
    assert_eq!(
        host.scene().window(window).expect("window").inner_size,
        (320, 240)
    );
    let layer = host.scene().layer(primary).expect("primary layer");
    assert_eq!(
        (layer.rect.x, layer.rect.y, layer.rect.w, layer.rect.h),
        (0, 0, 320, 240)
    );
    // The logical screen the presentation fits is the primary layer, so it is
    // unchanged as well.
    assert_eq!(host.vm_size(), (320, 240));

    // The D40 presentation extension fits the VM size into the new
    // framebuffer: 800x600 is exactly 2.5x of 320x240, so there is no margin.
    let translation = host.translation().expect("fittable sizes");
    assert_eq!(translation.vm_size, (320, 240));
    assert_eq!(translation.device_size, (800, 600));
    assert_eq!(translation.scale, 2.5);
    assert_eq!(translation.offset, (0.0, 0.0));
}

#[test]
fn a_script_window_resize_is_surfaced_once() {
    let mut host = headless_host();
    let window = build_solid_and_bitmap_scene(host.scene_mut());

    // The window the scene created is surfaced on the first frame, because
    // its client size differs from the framebuffer.
    host.step(&InjectedFrameTime::new(16.0)).expect("step");
    assert_eq!(host.take_window_resize(), Some((320, 240)));
    assert_eq!(host.take_window_resize(), None, "a taken request is gone");

    // A later script resize of the window's client size is a new request.
    // It moves the window client only: the logical screen stays the primary
    // layer's size, so the presentation does not change
    // (`reference/cpp/core/visual/LayerManager.cpp:341-342`).
    if let Some(window_state) = host.scene_mut().window_mut(window) {
        window_state.inner_size = (400, 300);
    }
    host.step(&InjectedFrameTime::new(16.0)).expect("step");
    assert_eq!(host.take_window_resize(), Some((400, 300)));
    assert_eq!(host.take_window_resize(), None, "a taken request is gone");

    // The window client moved but the logical screen did not, so the
    // presentation still fits the primary layer's size into the unchanged
    // framebuffer.
    assert_eq!(host.vm_size(), (320, 240));
    let translation = host.translation().expect("fittable sizes");
    assert_eq!(translation.vm_size, (320, 240));
    assert_eq!(translation.device_size, (1280, 720));
}

/// Boots the production composition root and confirms it installs the
/// emulated Windows desktop rectangle.
///
/// `desktopLeft`/`desktopTop` are `0` and `desktopWidth`/`desktopHeight` are
/// the render surface, so a startup script centring the window on the desktop
/// sees the whole surface
/// (`reference/cpp/core/base/impl/SystemImpl.cpp:897-935`,
/// `reference/cpp/core/visual/impl/TVPScreen.cpp:16-19`).
#[test]
fn game_host_open_installs_the_desktop_rectangle() {
    let _lock = vm_lock();
    let (game, base) = open_minimal_game("desktop-rect");
    let vm = game.vm();

    assert_eq!(eval_int(vm, "System.desktopLeft"), 0);
    assert_eq!(eval_int(vm, "System.desktopTop"), 0);
    assert_eq!(eval_int(vm, "System.desktopWidth"), 1280);
    assert_eq!(eval_int(vm, "System.desktopHeight"), 720);

    drop(game);
    let _ = std::fs::remove_dir_all(&base);
}

/// Boots the production composition root and confirms it installs the inert
/// clipboard backend.
///
/// The reference clipboard is inert
/// (`reference/cpp/core/utils/impl/ClipboardImpl.cpp:5-19`): `hasFormat` is
/// false for every format, `asText` reads void, and a write is a no-op.
#[test]
fn game_host_open_installs_the_inert_clipboard() {
    let _lock = vm_lock();
    let (game, base) = open_minimal_game("clipboard");
    let vm = game.vm();

    assert_eq!(eval_int(vm, "Clipboard.hasFormat(1)"), 0);
    assert_eq!(
        vm.eval("Clipboard.asText", "t").expect("eval asText"),
        TjsValue::Void
    );

    // A write is accepted and does not change the void read.
    vm.exec("Clipboard.asText = 'x';", "t")
        .expect("assign asText");
    assert_eq!(
        vm.eval("Clipboard.asText", "t").expect("eval asText"),
        TjsValue::Void
    );

    drop(game);
    let _ = std::fs::remove_dir_all(&base);
}

/// A self-updating transition advances on the frame the projection actually
/// draws its destination, even when no script calls `Layer.update()`.
///
/// The transition is started with `selfupdate: true`, so it is not on the
/// continuous update hook; the destination is visible and the projection emits
/// it every frame, which is the draw report that drives `BeforeCompletion`.
///
/// reference/cpp/core/visual/LayerIntf.cpp:6410-6422
/// reference/cpp/core/visual/LayerIntf.cpp:7906-7908
#[test]
fn selfupdate_transition_advances_when_projection_draws_it() {
    let _lock = vm_lock();
    let (mut game, base) = open_minimal_game("selfupdate-projection");
    game.vm()
        .exec(
            "var hits = 0; \
             var w = new Window(); \
             var a = new Layer(w, null); a.setSize(2, 2); a.setImageSize(2, 2); \
             a.fillRect(0, 0, 2, 2, 0xff0000ff); \
             var b = new Layer(w, null); b.setSize(2, 2); b.setImageSize(2, 2); \
             b.fillRect(0, 0, 2, 2, 0xffff0000); b.visible = true; \
             b.onTransitionCompleted = function(dest, src) { hits++; }; \
             b.beginTransition('crossfade', false, a, %[time: 100, selfupdate: true]);",
            "selfupdate-projection",
        )
        .expect("start self-update transition");

    for _ in 0..8 {
        game.step(&InjectedFrameTime::new(16.0), &[])
            .expect("frame");
    }
    assert_eq!(
        eval_int(game.vm(), "hits"),
        1,
        "the projected destination draws drove the self-update transition"
    );

    drop(game);
    let _ = std::fs::remove_dir_all(&base);
}
