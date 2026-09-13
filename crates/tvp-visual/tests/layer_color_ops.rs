//! Pixel-level tests for the `Layer` methods ported from the reference
//! `tTJSNI_BaseLayer` (`reference/cpp/core/visual/LayerIntf.cpp`) and the
//! `layerExImage` / SDK `LayerEx` plugin surface:
//!
//! * `colorRect` (blend math, `opa` semantics, clipping),
//! * `colorize`, `noise`, `tileRect`, `fillOperateRect`,
//! * `doDropShadow`, `doBlurLight`,
//! * `setClip` (`ClipRect`) clipping.
//!
//! Headless: the natives only mutate the shared [`Scene`], which the test
//! reads under a read lock, the same contract the Bevy renderer uses.

use std::sync::{Arc, Mutex, RwLock};

use engine::Storage;
use tempfile::TempDir;
use tjs2_sys::Tjs2Engine;
use tvp_visual::scene::Scene;

/// Fresh engine + scene + temp storage with the visual natives registered.
/// The crate-global VM context is shared, so the process-wide test lock is
/// held for the environment's lifetime (mirrors `tests/visual_natives.rs`).
struct Env {
    engine: Arc<Tjs2Engine>,
    scene: Arc<RwLock<Scene>>,
    #[allow(dead_code)]
    storage: Arc<Mutex<Storage>>,
    _dir: TempDir,
    _vm_lock: std::sync::MutexGuard<'static, ()>,
}

impl Env {
    fn new() -> Self {
        let _vm_lock = tvp_visual::natives::vm_test_lock()
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let dir = tempfile::tempdir().expect("temp dir");
        let storage = Arc::new(Mutex::new(
            Storage::mount(dir.path()).expect("mount temp game dir"),
        ));
        let scene = Arc::new(RwLock::new(Scene::default()));
        let engine = Arc::new(Tjs2Engine::new().expect("engine"));
        tvp_visual::register_visual(&engine, scene.clone(), storage.clone())
            .expect("register visual natives");
        Env {
            engine,
            scene,
            storage,
            _dir: dir,
            _vm_lock,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "layer_color_ops")
            .expect("script must run");
    }

    fn scene(&self) -> std::sync::RwLockReadGuard<'_, Scene> {
        self.scene.read().expect("scene lock poisoned")
    }
}

/// Read an RGBA pixel from the first layer's attached bitmap.
fn pixel(scene: &Scene, x: u32, y: u32) -> [u8; 4] {
    let bitmap_id = scene.layers[0].bitmap.expect("layer has a bitmap");
    let bitmap = scene.bitmap(bitmap_id).expect("bitmap exists");
    let i = ((y * bitmap.width + x) as usize) * 4;
    [
        bitmap.rgba[i],
        bitmap.rgba[i + 1],
        bitmap.rgba[i + 2],
        bitmap.rgba[i + 3],
    ]
}

/// A fresh 8×8 layer bitmap for the tests.
fn setup(env: &Env) {
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         l.setSize(8, 8); var b = new Bitmap(8, 8); l.setBitmap(b.id);",
    );
}

/// `opa == 255` forces the RGB and a full `255` alpha (`FillARGB`), even when
/// the color's own high byte is not `0xFF`.
#[test]
fn color_rect_opaque_forces_alpha() {
    let env = Env::new();
    setup(&env);
    env.run("l.colorRect(0, 0, 2, 2, 0x80112233);");
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [0x11, 0x22, 0x33, 0xff]);
    assert_eq!(pixel(&scene, 1, 1), [0x11, 0x22, 0x33, 0xff]);
    assert_eq!(
        pixel(&scene, 2, 2),
        [0, 0, 0, 0],
        "outside the rect untouched"
    );
}

/// Partial `opa` over a transparent destination: the TVP
/// `TVPConstColorAlphaBlend_d` math (`TVPOpacityOnOpacityTable` with a fully
/// transparent destination collapses the color factor to `255`).
#[test]
fn color_rect_partial_opacity_on_transparent() {
    let env = Env::new();
    setup(&env);
    // RGB(0, 0, 64) with opa=64 — the exact `album.tjs:451` call shape.
    env.run("l.colorRect(0, 0, 1, 1, 0x00000040, 64);");
    let scene = env.scene();
    assert_eq!(
        pixel(&scene, 0, 0),
        [0, 0, 63, 65],
        "partial-opacity blend over transparent"
    );
}

/// Partial `opa` over an opaque destination.
#[test]
fn color_rect_partial_opacity_on_opaque() {
    let env = Env::new();
    setup(&env);
    env.run(
        "l.fillRect(0, 0, 1, 1, 0xffff0000); \
         l.colorRect(0, 0, 1, 1, 0x00000040, 64);",
    );
    let scene = env.scene();
    assert_eq!(
        pixel(&scene, 0, 0),
        [191, 0, 16, 255],
        "source-over keeps the destination opaque"
    );
}

/// `opa < 0` removes opacity (`RemoveConstOpacity`): the alpha scales by
/// `(alpha * (255 - level)) >> 8`, RGB is untouched.
#[test]
fn color_rect_negative_opacity_removes_alpha() {
    let env = Env::new();
    setup(&env);
    env.run(
        "l.fillRect(0, 0, 1, 1, 0xff0a141e); \
         l.colorRect(0, 0, 1, 1, 0x000000, -128);",
    );
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [10, 20, 30, 126], "(255 * 127) >> 8");
}

/// `setClip` restricts every pixel operation to the clip rectangle, exactly
/// like the reference `TVPIntersectRect(&destrect, rect, ClipRect)`.
#[test]
fn color_rect_respects_set_clip() {
    let env = Env::new();
    setup(&env);
    env.run(
        "l.setClip(2, 2, 2, 2); \
         l.colorRect(0, 0, 8, 8, 0xffff0000);",
    );
    let scene = env.scene();
    assert_eq!(pixel(&scene, 1, 1), [0, 0, 0, 0]);
    assert_eq!(pixel(&scene, 2, 2), [255, 0, 0, 255]);
    assert_eq!(pixel(&scene, 3, 3), [255, 0, 0, 255]);
    assert_eq!(pixel(&scene, 4, 4), [0, 0, 0, 0]);
    drop(scene);
    env.run("l.setClip(); l.colorRect(0, 0, 8, 8, 0xff00ff00);");
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [0, 255, 0, 255], "reset clip");
    assert_eq!(pixel(&scene, 7, 7), [0, 255, 0, 255]);
}

/// `colorize(hue, sat, blend)` — full blend of pure red to hue 170 (~240°,
/// blue) while preserving lightness.
#[test]
fn colorize_full_blend_replaces_hue() {
    let env = Env::new();
    setup(&env);
    env.run(
        "l.fillRect(0, 0, 1, 1, 0xffff0000); \
         l.colorize(170, 255, 1.0);",
    );
    let scene = env.scene();
    let p = pixel(&scene, 0, 0);
    assert_eq!(p[3], 255, "alpha preserved");
    assert!(p[2] >= 254, "hue moved to blue, got {p:?}");
    assert!(p[0] <= 2 && p[1] <= 2, "red/green nearly gone, got {p:?}");
}

/// `colorize` with `blend = 0` is a no-op; partial blend moves toward the
/// target.
#[test]
fn colorize_zero_blend_is_noop() {
    let env = Env::new();
    setup(&env);
    env.run(
        "l.fillRect(0, 0, 1, 1, 0xffff0000); \
         l.colorize(170, 255, 0.0);",
    );
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [255, 0, 0, 255]);
}

/// `noise(level)` perturbs RGB and holds alpha.
#[test]
fn noise_perturbs_rgb_and_holds_alpha() {
    let env = Env::new();
    setup(&env);
    env.run(
        "l.fillRect(0, 0, 8, 8, 0xff808080); \
         l.noise(100);",
    );
    let scene = env.scene();
    let changed = (0..8).any(|y| (0..8).any(|x| pixel(&scene, x, y) != [0x80, 0x80, 0x80, 255]));
    assert!(changed, "noise must change at least one RGB pixel");
    for y in 0..8 {
        for x in 0..8 {
            assert_eq!(pixel(&scene, x, y)[3], 255, "alpha held");
        }
    }
}

/// `tileRect` repeats the source tile over the destination rect
/// (`copyRect` in a loop).
#[test]
fn tile_rect_repeats_the_tile() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.setSize(4, 4); var b = new Bitmap(4, 4); l.setBitmap(b.id); \
         var tile = new Layer(w, null); var tb = new Bitmap(2, 2); tile.setBitmap(tb.id); \
         tile.fillRect(0, 0, 1, 1, 0xffff0000); \
         tile.fillRect(1, 0, 1, 1, 0xff00ff00); \
         tile.fillRect(0, 1, 1, 1, 0xff0000ff); \
         tile.fillRect(1, 1, 1, 1, 0xffffffff); \
         l.tileRect(0, 0, 4, 4, tile);",
    );
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&scene, 1, 0), [0, 255, 0, 255]);
    assert_eq!(pixel(&scene, 2, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&scene, 0, 1), [0, 0, 255, 255]);
    assert_eq!(pixel(&scene, 1, 1), [255, 255, 255, 255]);
    assert_eq!(pixel(&scene, 3, 3), [255, 255, 255, 255]);
}

/// `fillOperateRect(..., ltPsNormal)` is a source-over blend fill.
#[test]
fn fill_operate_rect_normal_blend() {
    let env = Env::new();
    setup(&env);
    env.run(
        "l.fillRect(0, 0, 8, 8, 0xffffffff); \
         l.fillOperateRect(0, 0, 8, 8, 0x800000ff, 13);",
    );
    let scene = env.scene();
    // Source-over of 50% blue over white.
    assert_eq!(pixel(&scene, 0, 0), [127, 127, 255, 255]);
    assert_eq!(pixel(&scene, 7, 7), [127, 127, 255, 255]);
}

/// `fillOperateRect(..., ltOpaque)` copies the source (including alpha).
#[test]
fn fill_operate_rect_opaque_copy() {
    let env = Env::new();
    setup(&env);
    env.run("l.fillOperateRect(0, 0, 8, 8, 0x800000ff, 1);");
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [0, 0, 255, 128]);
}

/// `doDropShadow` offsets a blurred shadow behind the original image, so a
/// pixel outside the original opaque shape gains alpha.
#[test]
fn drop_shadow_adds_offset_shadow() {
    let env = Env::new();
    setup(&env);
    env.run(
        "l.fillRect(2, 2, 2, 2, 0xffffffff); \
         l.doDropShadow(2, 2, 1, 0x000000, 200);",
    );
    let scene = env.scene();
    // The original shape stays opaque white.
    assert_eq!(pixel(&scene, 2, 2), [255, 255, 255, 255]);
    // The shadow has been offset to (4, 4), which was transparent before.
    let shadow = pixel(&scene, 4, 4);
    assert!(
        shadow[3] > 0,
        "offset shadow must add alpha at (4, 4), got {shadow:?}"
    );
    // A far-away pixel is untouched.
    assert_eq!(pixel(&scene, 7, 7), [0, 0, 0, 0]);
}

/// `doBlurLight` blur-composites the image into itself; the result stays
/// non-empty and (for the default hard-light light type) changes pixels.
#[test]
fn blur_light_keeps_and_changes_pixels() {
    let env = Env::new();
    setup(&env);
    env.run(
        "l.fillRect(3, 3, 2, 2, 0xffffffff); \
         l.doBlurLight(1, 128, 200, 19);",
    );
    let scene = env.scene();
    assert_eq!(
        pixel(&scene, 3, 3)[3],
        255,
        "the opaque core survives the composite"
    );
    // The blur spreads alpha outward.
    let spread = (0..8)
        .flat_map(|y| (0..8).map(move |x| (x, y)))
        .filter(|&(x, y)| pixel(&scene, x, y)[3] > 0)
        .count();
    assert!(
        spread > 4,
        "blur should spread coverage, got {spread} pixels"
    );
}

/// Replacing the main image resets the clip, matching the reference
/// (`LoadImages`/`AssignMainImage`/`AllocateImage` all call `ResetClip`).
#[test]
fn replacing_the_image_resets_clip() {
    let env = Env::new();
    setup(&env);
    env.run(
        "l.setClip(2, 2, 2, 2); \
         var b2 = new Bitmap(8, 8); l.setBitmap(b2.id); \
         l.colorRect(0, 0, 8, 8, 0xffff0000);",
    );
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&scene, 7, 7), [255, 0, 0, 255]);
}

/// `fillRect` (the pre-existing method) still replaces pixels exactly and
/// leaves the layer position alone — a guard against regressions from the
/// shared pixel-op module.
#[test]
fn fill_rect_still_replaces_pixels() {
    let env = Env::new();
    setup(&env);
    env.run("l.setPos(50, 60); l.fillRect(1, 1, 2, 2, 0xff0000ff);");
    let scene = env.scene();
    assert_eq!((scene.layers[0].rect.x, scene.layers[0].rect.y), (50, 60));
    assert_eq!(pixel(&scene, 1, 1), [0, 0, 255, 255]);
    assert_eq!(pixel(&scene, 0, 0), [0, 0, 0, 0]);
}
