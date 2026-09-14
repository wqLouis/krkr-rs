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

    fn eval_string(&self, expr: &str) -> String {
        match self.engine.eval(expr, "layer_color_ops") {
            Ok(tjs2_sys::TjsValue::String(s)) => s,
            other => panic!("eval {expr:?} -> {other:?}"),
        }
    }

    fn eval_int(&self, expr: &str) -> i64 {
        match self.engine.eval(expr, "layer_color_ops") {
            Ok(tjs2_sys::TjsValue::Integer(i)) => i,
            other => panic!("eval {expr:?} -> {other:?}"),
        }
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

/// Read an RGBA pixel from a layer's attached bitmap (by layer index).
fn pixel_index(scene: &Scene, layer_index: usize, x: u32, y: u32) -> [u8; 4] {
    let bitmap_id = scene.layers[layer_index]
        .bitmap
        .expect("layer has a bitmap");
    let bitmap = scene.bitmap(bitmap_id).expect("bitmap exists");
    let i = ((y * bitmap.width + x) as usize) * 4;
    [
        bitmap.rgba[i],
        bitmap.rgba[i + 1],
        bitmap.rgba[i + 2],
        bitmap.rgba[i + 3],
    ]
}

/// `saveLayerImage` writes the image to storage and `loadImages` reads it
/// back, pixel-for-pixel (PNG round trip).
#[test]
fn save_layer_image_round_trips() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.setSize(4, 4); var b = new Bitmap(4, 4); l.setBitmap(b.id); \
         l.fillRect(0, 0, 4, 4, 0xff123456); \
         l.saveLayerImage('saved.png');",
    );
    assert!(
        env._dir.path().join("saved.png").exists(),
        "saveLayerImage must write the file"
    );
    env.run("var l2 = new Layer(w, null); l2.loadImages('saved.png');");
    let scene = env.scene();
    assert_eq!(
        pixel_index(&scene, 1, 1, 1),
        [0x12, 0x34, 0x56, 0xff],
        "PNG round trip preserves RGBA"
    );
}

/// `saveLayerImage` defaults to BMP and creates the parent directory.
#[test]
fn save_layer_image_bmp_creates_parent_dir() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.setSize(2, 2); var b = new Bitmap(2, 2); l.setBitmap(b.id); \
         l.fillRect(0, 0, 2, 2, 0xff00ff00); \
         l.saveLayerImage('thumb/a.bmp');",
    );
    let path = env._dir.path().join("thumb").join("a.bmp");
    assert!(path.exists(), "BMP saved under a created subdirectory");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..2], b"BM", "BMP magic");
}

/// `stretchCopy` with `stNearest` samples exact source pixels.
#[test]
fn stretch_copy_nearest_downscale() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); var sb = new Bitmap(4, 4); src.setBitmap(sb.id); \
         src.fillRect(0, 0, 2, 2, 0xffff0000); \
         src.fillRect(2, 0, 2, 2, 0xff00ff00); \
         src.fillRect(0, 2, 2, 2, 0xff0000ff); \
         src.fillRect(2, 2, 2, 2, 0xffffffff); \
         var dst = new Layer(w, null); dst.setSize(2, 2); var db = new Bitmap(2, 2); dst.setBitmap(db.id); \
         dst.stretchCopy(0, 0, 2, 2, src, 0, 0, 4, 4, 0);",
    );
    let scene = env.scene();
    // Nearest maps dest centers (0.5,0.5)/(1.5,1.5) back to source
    // (0.5,0.5)/(2.5,2.5) → the four quadrant representatives.
    assert_eq!(pixel_index(&scene, 1, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel_index(&scene, 1, 1, 1), [255, 255, 255, 255]);
}

/// `doGrayScale` applies the reference luma on each pixel, alpha held.
#[test]
fn do_gray_scale_matches_reference_luma() {
    let env = Env::new();
    setup(&env);
    env.run("l.fillRect(0, 0, 1, 1, 0xffff0000); l.doGrayScale();");
    let scene = env.scene();
    // (19*255) >> 8 == 18
    assert_eq!(pixel(&scene, 0, 0), [18, 18, 18, 255]);
}

/// `adjustGamma` with gamma 2.0 brightens mid grays and holds alpha; the
/// identity parameters leave the pixels unchanged.
#[test]
fn adjust_gamma_brightens_and_identity_is_noop() {
    let env = Env::new();
    setup(&env);
    env.run("l.fillRect(0, 0, 1, 1, 0xff808080);");
    let before = pixel(&env.scene(), 0, 0);
    assert_eq!(before, [0x80, 0x80, 0x80, 0xff]);
    env.run("l.adjustGamma(2.0, 0, 255, 2.0, 0, 255, 2.0, 0, 255);");
    let after = pixel(&env.scene(), 0, 0);
    assert!(after[0] > 0x80, "gamma 2.0 brightens, got {after:?}");
    assert_eq!(after[3], 255, "alpha held");
    // Identity leaves it as the brightened value (the table is identity).
    env.run("l.adjustGamma(1.0, 0, 255, 1.0, 0, 255, 1.0, 0, 255);");
    assert_eq!(pixel(&env.scene(), 0, 0), after);
}

/// Native `Layer` event methods dispatch to the action owner (the first
/// constructor argument) via `actionOwner.action(event)` with the event
/// dictionary (`TVP_ACTION_INVOKE`). This is the `_trim.onMouseMove(...)`
/// path from `system/SelectItem.tjs:963`.
#[test]
fn layer_event_dispatches_to_action_owner() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var got = ''; \
         var target_ok = false; \
         w.action = function(ev) { \
             got = ev.type; \
             if (ev.x !== void) got += ':' + ev.x + ',' + ev.y + ',' + ev.shift; \
             if (ev.key !== void) got += ':' + ev.key + ',' + ev.shift + ',' + ev.process; \
             target_ok = (ev.target === l); \
         }; \
         var l = new Layer(w, null); \
         l.onMouseMove(3, 4, 5);",
    );
    assert_eq!(env.eval_string("got"), "onMouseMove:3,4,5");
    assert_eq!(env.eval_int("target_ok"), 1, "event target is the layer");

    // Zero-argument events still dispatch with type/target.
    env.run("l.onMouseEnter();");
    assert!(
        env.eval_string("got").starts_with("onMouseEnter"),
        "onMouseEnter dispatched"
    );
    // Key events carry key/shift/process.
    env.run("l.onKeyDown(65, 1, 1);");
    assert_eq!(env.eval_string("got"), "onKeyDown:65,1,1");
}

/// The action-owner dispatch is a no-op when no owner was captured (an
/// integer window id and no registered window object).
#[test]
fn layer_event_without_action_owner_is_noop() {
    let env = Env::new();
    env.run("var w = new Window(); var l = new Layer(w, null); l.onClick(1, 2);");
    let scene = env.scene();
    assert_eq!(scene.layers.len(), 1);
}

/// `flipLR`/`flipUD` mirror the whole image.
#[test]
fn flip_lr_and_ud_mirror() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); l.setSize(2, 1); \
         var b = new Bitmap(2, 1); l.setBitmap(b.id); \
         l.fillRect(0, 0, 1, 1, 0xffff0000); \
         l.fillRect(1, 0, 1, 1, 0xff00ff00);",
    );
    env.run("l.flipLR();");
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [0, 255, 0, 255]);
    assert_eq!(pixel(&scene, 1, 0), [255, 0, 0, 255]);
    drop(scene);
    env.run("l.flipUD();");
    // Only one row, so flipUD is a no-op here; assert it did not corrupt.
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [0, 255, 0, 255]);
}

/// `light(brightness, contrast)` brightens/darkens channels.
#[test]
fn light_brightens_pixels() {
    let env = Env::new();
    setup(&env);
    env.run("l.fillRect(0, 0, 1, 1, 0xff404040); l.light(40, 0);");
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [0x68, 0x68, 0x68, 255]);
}

/// `independMainImage` detaches the layer's bitmap so later writes do not
/// affect the original.
#[test]
fn independ_main_image_clones() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); l.setSize(2, 2); \
         var b = new Bitmap(2, 2); l.setBitmap(b.id); \
         l.fillRect(0, 0, 2, 2, 0xffff0000); \
         l.independMainImage(); \
         l.fillRect(0, 0, 1, 1, 0xff0000ff);",
    );
    let scene = env.scene();
    // The layer's new bitmap has the clone with the overwritten pixel.
    assert_eq!(pixel(&scene, 0, 0), [0, 0, 255, 255]);
    assert_eq!(pixel(&scene, 1, 1), [255, 0, 0, 255]);
}

/// `operateRect` composites a source region with the requested mode.
#[test]
fn operate_rect_composites() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); var sb = new Bitmap(2, 2); src.setBitmap(sb.id); \
         src.fillRect(0, 0, 2, 2, 0xff0000ff); \
         var dst = new Layer(w, null); dst.setSize(4, 4); var db = new Bitmap(4, 4); dst.setBitmap(db.id); \
         dst.fillRect(0, 0, 4, 4, 0xffffffff); \
         dst.operateRect(0, 0, src, 0, 0, 2, 2, 2, 255);",
    );
    let scene = env.scene();
    // Source-over of opaque blue replaces the dest in the region.
    assert_eq!(pixel_index(&scene, 1, 0, 0), [0, 0, 255, 255]);
    assert_eq!(pixel_index(&scene, 1, 3, 3), [255, 255, 255, 255]);
}

/// `affineCopy` maps the source rect's corners to the given points.
#[test]
fn affine_copy_scales_source() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); var sb = new Bitmap(2, 2); src.setBitmap(sb.id); \
         src.fillRect(0, 0, 1, 1, 0xffff0000); \
         src.fillRect(1, 0, 1, 1, 0xff00ff00); \
         src.fillRect(0, 1, 1, 1, 0xff0000ff); \
         src.fillRect(1, 1, 1, 1, 0xffffffff); \
         var dst = new Layer(w, null); dst.setSize(4, 4); var db = new Bitmap(4, 4); dst.setBitmap(db.id); \
         dst.affineCopy(src, 0, 0, 2, 2, false, 0, 0, 4, 0, 0, 4, 0);",
    );
    let scene = env.scene();
    // 2x2 source scaled to 4x4, nearest: each source pixel is a 2x2 block.
    assert_eq!(pixel_index(&scene, 1, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel_index(&scene, 1, 2, 0), [0, 255, 0, 255]);
    assert_eq!(pixel_index(&scene, 1, 0, 2), [0, 0, 255, 255]);
    assert_eq!(pixel_index(&scene, 1, 2, 2), [255, 255, 255, 255]);
}

/// `gaussianBlur` spreads an opaque dot.
#[test]
fn gaussian_blur_spreads() {
    let env = Env::new();
    setup(&env);
    env.run("l.fillRect(4, 4, 1, 1, 0xffffffff); l.gaussianBlur(2, 1.0);");
    let scene = env.scene();
    assert!(pixel(&scene, 3, 4)[3] > 0, "blur spread left");
    assert!(pixel(&scene, 5, 4)[3] > 0, "blur spread right");
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

// ---------------------------------------------------------------------------
// `copyRect` blit (reference `tTJSNI_BaseLayer::CopyRect`,
// `LayerIntf.cpp:4574`) and the legacy/blit siblings.
// ---------------------------------------------------------------------------

/// `copyRect` honors the destination offset and the source sub-rect instead
/// of attaching the whole source bitmap (the old behavior).
#[test]
fn copy_rect_honors_dest_offset_and_source_rect() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); var sb = new Bitmap(4, 4); src.setBitmap(sb.id); \
         src.fillRect(0, 0, 2, 2, 0xffff0000); \
         src.fillRect(2, 0, 2, 2, 0xff00ff00); \
         src.fillRect(0, 2, 2, 2, 0xff0000ff); \
         src.fillRect(2, 2, 2, 2, 0xffffffff); \
         var dst = new Layer(w, null); dst.setSize(8, 8); var db = new Bitmap(8, 8); dst.setBitmap(db.id); \
         dst.copyRect(3, 1, src, 2, 0, 2, 2);",
    );
    let scene = env.scene();
    // Source (2,0)-(4,2) is the green quadrant, blitted at (3,1).
    assert_eq!(pixel_index(&scene, 1, 3, 1), [0, 255, 0, 255]);
    assert_eq!(pixel_index(&scene, 1, 4, 2), [0, 255, 0, 255]);
    // Outside the blit the destination stays transparent.
    assert_eq!(pixel_index(&scene, 1, 0, 0), [0, 0, 0, 0]);
    assert_eq!(pixel_index(&scene, 1, 2, 0), [0, 0, 0, 0]);
    assert_eq!(pixel_index(&scene, 1, 3, 0), [0, 0, 0, 0]);
    // The red source quadrant was not part of the source rect; one row
    // below the blit is still transparent.
    assert_eq!(pixel_index(&scene, 1, 3, 3), [0, 0, 0, 0]);
}

/// `copyRect` clips the destination to both the bitmap bounds and the layer
/// `ClipRect`.
#[test]
fn copy_rect_clips_to_dest_and_clip() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); var sb = new Bitmap(4, 4); src.setBitmap(sb.id); \
         src.fillRect(0, 0, 4, 4, 0xffff0000); \
         var dst = new Layer(w, null); dst.setSize(4, 4); var db = new Bitmap(4, 4); dst.setBitmap(db.id); \
         dst.setClip(1, 1, 2, 2); \
         dst.copyRect(-1, -1, src, 0, 0, 4, 4);",
    );
    let scene = env.scene();
    // The clip [1,3) x [1,3) keeps the (1,1) and (2,2) destination pixels.
    assert_eq!(pixel_index(&scene, 1, 0, 0), [0, 0, 0, 0]);
    assert_eq!(pixel_index(&scene, 1, 1, 1), [255, 0, 0, 255]);
    assert_eq!(pixel_index(&scene, 1, 2, 2), [255, 0, 0, 255]);
    assert_eq!(pixel_index(&scene, 1, 3, 3), [0, 0, 0, 0]);
}

/// `copyRect` accepts either a `Bitmap` or a `Layer` source.
#[test]
fn copy_rect_accepts_bitmap_and_layer_sources() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var b = new Bitmap(2, 2); \
         var bl = new Layer(w, null); bl.setBitmap(b.id); bl.fillRect(0, 0, 2, 2, 0xff00ff00); \
         var dst1 = new Layer(w, null); dst1.setSize(2, 2); var d1 = new Bitmap(2, 2); dst1.setBitmap(d1.id); \
         dst1.copyRect(0, 0, b, 0, 0, 2, 2); \
         var dst2 = new Layer(w, null); dst2.setSize(2, 2); var d2 = new Bitmap(2, 2); dst2.setBitmap(d2.id); \
         dst2.copyRect(0, 0, bl, 0, 0, 2, 2);",
    );
    let scene = env.scene();
    // Layers: bl=0, dst1=1, dst2=2.
    assert_eq!(
        pixel_index(&scene, 1, 0, 0),
        [0, 255, 0, 255],
        "Bitmap source"
    );
    assert_eq!(
        pixel_index(&scene, 2, 0, 0),
        [0, 255, 0, 255],
        "Layer source"
    );
}

/// `copyRect` copies directly (`Copy` = `tTVPRenderMethod_DirectCopy`,
/// `RenderManager.cpp:1359`): the source rect (RGB *and* alpha, including
/// transparent pixels) overwrites the destination — not a source-over blend.
#[test]
fn copy_rect_is_a_direct_copy() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); var sb = new Bitmap(1, 1); src.setBitmap(sb.id); \
         src.fillRect(0, 0, 1, 1, 0x800000ff); \
         var dst = new Layer(w, null); dst.setSize(1, 1); var db = new Bitmap(1, 1); dst.setBitmap(db.id); \
         dst.fillRect(0, 0, 1, 1, 0xffff0000); \
         dst.copyRect(0, 0, src, 0, 0, 1, 1);",
    );
    let scene = env.scene();
    // The destination takes the source verbatim (no blending).
    assert_eq!(pixel_index(&scene, 1, 0, 0), [0, 0, 255, 128]);
}

/// The `Button.create` sheet pattern: `copyRect` copies the whole sheet into
/// the layer, then `setSize` + `setImagePos` select one pattern without
/// cropping the sheet.
#[test]
fn copy_rect_button_sheet_pattern() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var sheet = new Bitmap(6, 2); \
         var sl = new Layer(w, null); sl.setBitmap(sheet.id); \
         sl.fillRect(0, 0, 2, 2, 0xffff0000); \
         sl.fillRect(2, 0, 2, 2, 0xff00ff00); \
         sl.fillRect(4, 0, 2, 2, 0xff0000ff); \
         var btn = new Layer(w, null); btn.setSize(6, 2); var bb = new Bitmap(6, 2); btn.setBitmap(bb.id); \
         btn.copyRect(0, 0, sheet, 0, 0, 6, 2); \
         btn.setSize(2, 2); btn.setImagePos(-2, 0);",
    );
    let scene = env.scene();
    let btn = &scene.layers[1];
    assert_eq!(btn.image_left, -2, "pattern 2 selected");
    let bmp = scene.bitmap(btn.bitmap.unwrap()).unwrap();
    assert_eq!((bmp.width, bmp.height), (6, 2), "sheet is not cropped");
    assert_eq!(pixel_index(&scene, 1, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel_index(&scene, 1, 4, 0), [0, 0, 255, 255]);
}

/// `copyToBitmapFromMainImage` copies the layer's main image into a `Bitmap`.
#[test]
fn copy_to_bitmap_from_main_image() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.setSize(2, 2); var b = new Bitmap(2, 2); l.setBitmap(b.id); \
         l.fillRect(0, 0, 2, 2, 0xff336699); \
         var out = new Bitmap(2, 2); \
         l.copyToBitmapFromMainImage(out);",
    );
    let scene = env.scene();
    // `out` is the second scene bitmap (the first is the layer's own).
    let out = scene
        .bitmaps
        .iter()
        .find(|b| b.rgba[..4] == [0x33, 0x66, 0x99, 0xff]);
    assert!(out.is_some(), "the destination bitmap received the pixels");
    let out = out.unwrap();
    assert_eq!((out.width, out.height), (2, 2));
}

/// `convertType` premultiplies (`dfAlpha` -> `dfAddAlpha`) and
/// unpremultiplies in the other direction.
#[test]
fn convert_type_round_trips_add_alpha() {
    let env = Env::new();
    setup(&env);
    env.run("l.fillRect(0, 0, 1, 1, 0x80804020); l.face = 4; l.convertType(0);");
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [64, 32, 16, 128], "premultiplied");
    drop(scene);
    env.run("l.face = 0; l.convertType(4);");
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0), [127, 63, 31, 128], "unpremultiplied");
}

/// `pileRect` source-over blends the source alpha; `blendRect` forces the
/// source opaque and applies a constant opacity. A transparent source
/// separates the two.
#[test]
fn pile_rect_and_blend_rect() {
    let env = Env::new();
    // Source: blue with alpha 0 (invisible).
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); var sb = new Bitmap(2, 2); src.setBitmap(sb.id); \
         src.fillRect(0, 0, 2, 2, 0x000000ff); \
         var d1 = new Layer(w, null); d1.setSize(2, 2); var b1 = new Bitmap(2, 2); d1.setBitmap(b1.id); \
         d1.fillRect(0, 0, 2, 2, 0xffff0000); \
         d1.pileRect(0, 0, src, 0, 0, 2, 2, 255); \
         var d2 = new Layer(w, null); d2.setSize(2, 2); var b2 = new Bitmap(2, 2); d2.setBitmap(b2.id); \
         d2.fillRect(0, 0, 2, 2, 0xffff0000); \
         d2.blendRect(0, 0, src, 0, 0, 2, 2, 255);",
    );
    let scene = env.scene();
    // Layers: src=0, d1=1, d2=2.
    assert_eq!(
        pixel_index(&scene, 1, 0, 0),
        [255, 0, 0, 255],
        "pileRect keeps a transparent source invisible"
    );
    assert_eq!(
        pixel_index(&scene, 2, 0, 0),
        [0, 0, 255, 255],
        "blendRect treats the source as opaque"
    );
}

/// `clipLeft`/`clipTop`/`clipWidth`/`clipHeight` expose and update the
/// `ClipRect`.
#[test]
fn clip_properties_round_trip() {
    let env = Env::new();
    setup(&env);
    env.run("l.setClip(1, 2, 3, 4);");
    assert_eq!(env.eval_int("l.clipLeft"), 1);
    assert_eq!(env.eval_int("l.clipTop"), 2);
    assert_eq!(env.eval_int("l.clipWidth"), 3);
    assert_eq!(env.eval_int("l.clipHeight"), 4);
    env.run("l.clipLeft = 5; l.clipHeight = 6;");
    assert_eq!(env.eval_int("l.clipLeft"), 5);
    assert_eq!(env.eval_int("l.clipHeight"), 6);
    assert_eq!(env.eval_int("l.clipWidth"), 3, "other components kept");
}

/// Focus/node/name properties: `nodeVisible`/`nodeEnabled` walk the tree,
/// `focus()` sets `focused`, and `isPrimary` reflects the window's primary
/// layer.
#[test]
fn focus_and_node_properties() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var parent = new Layer(w, null); parent.visible = true; \
         var child = new Layer(w, parent); child.visible = true; child.focusable = true; \
         child.name = 'btn';",
    );
    assert_eq!(
        env.eval_int("parent.isPrimary"),
        1,
        "first layer is primary"
    );
    assert_eq!(env.eval_int("child.nodeVisible"), 1);
    assert_eq!(env.eval_int("child.nodeEnabled"), 1);
    assert_eq!(env.eval_int("child.nodeFocusable"), 1);
    assert_eq!(env.eval_string("child.name"), "btn");
    env.run("child.focus();");
    assert_eq!(env.eval_int("child.focused"), 1);
    env.run("parent.visible = false;");
    assert_eq!(
        env.eval_int("child.nodeVisible"),
        0,
        "hidden ancestor hides"
    );
}

/// `drawImage`/`drawImageRect`/`drawImageStretch` copy a source Layer or
/// Bitmap onto the layer's main image.
#[test]
fn draw_image_variants() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); var sb = new Bitmap(4, 4); src.setBitmap(sb.id); \
         src.fillRect(0, 0, 2, 2, 0xffff0000); \
         src.fillRect(2, 2, 2, 2, 0xff00ff00); \
         var dst = new Layer(w, null); dst.setSize(8, 8); var db = new Bitmap(8, 8); dst.setBitmap(db.id); \
         dst.drawImage(1, 1, src); \
         dst.drawImageRect(0, 6, src, 2, 2, 2, 2);",
    );
    let scene = env.scene();
    assert_eq!(pixel_index(&scene, 1, 1, 1), [255, 0, 0, 255]);
    // `drawImageRect(0, 6, src, 2, 2, 2, 2)` places the green source
    // quadrant at (0, 6).
    assert_eq!(pixel_index(&scene, 1, 0, 6), [0, 255, 0, 255]);
    assert_eq!(pixel_index(&scene, 1, 1, 7), [0, 255, 0, 255]);
}

/// `drawEllipse` and `drawPie` rasterize the plugin's GDI+ shapes with the
/// appearance brushes/pens.
#[test]
fn draw_ellipse_and_pie_paint() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); l.setSize(16, 16); \
         var b = new Bitmap(16, 16); l.setBitmap(b.id); \
         var app = new GdiPlus.Appearance(); app.addBrush(0xffff0000); \
         l.drawEllipse(app, 0, 0, 16, 16); \
         l.drawPie(app, 0, 0, 16, 16, 0, 90);",
    );
    let scene = env.scene();
    // The ellipse/pie cover the center and the top-left quadrant.
    assert!(
        pixel_index(&scene, 0, 8, 8)[3] > 0,
        "ellipse center painted"
    );
    assert!(pixel_index(&scene, 0, 4, 4)[3] > 0, "pie quadrant painted");
}

/// `stretchCopy` supports the full `tTVPBBStretchType` set: `stFastLanczos2`
/// (`7`) and an unknown/out-of-range type must not throw, and must paint.
#[test]
fn stretch_copy_supports_lanczos_and_unknown_types() {
    let env = Env::new();
    for (type_id, tag) in [(7, "lanczos2"), (99, "unknown")] {
        env.run(&format!(
            "var w = new Window(); \
             var src = new Layer(w, null); var sb = new Bitmap(4, 4); src.setBitmap(sb.id); \
             src.fillRect(0, 0, 2, 2, 0xffff0000); \
             src.fillRect(2, 0, 2, 2, 0xff00ff00); \
             src.fillRect(0, 2, 2, 2, 0xff0000ff); \
             src.fillRect(2, 2, 2, 2, 0xffffffff); \
             var dst{type_id} = new Layer(w, null); dst{type_id}.setSize(4, 4); \
             var db{type_id} = new Bitmap(4, 4); dst{type_id}.setBitmap(db{type_id}.id); \
             dst{type_id}.stretchCopy(0, 0, 4, 4, src, 0, 0, 4, 4, {type_id});"
        ));
        let scene = env.scene();
        let layer = scene.layers.iter().find(|l| l.id != 0).unwrap();
        let bmp = scene.bitmap(layer.bitmap.unwrap()).unwrap();
        assert!(
            bmp.rgba.chunks_exact(4).any(|p| p[3] > 0),
            "stretch type {type_id} ({tag}) must paint"
        );
        drop(scene);
    }
}

/// `copy9Patch` detects the source margins from the border alpha runs and
/// scales the nine regions to fill the destination.
#[test]
fn copy_9patch_scales_to_destination() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); var sb = new Bitmap(12, 12); src.setBitmap(sb.id); \
         src.fillRect(0, 0, 2, 2, 0xffffffff); \
         src.fillRect(10, 0, 2, 2, 0xffffffff); \
         src.fillRect(0, 10, 2, 2, 0xffffffff); \
         src.fillRect(10, 10, 2, 2, 0xffffffff); \
         var dst = new Layer(w, null); dst.setSize(20, 20); var db = new Bitmap(20, 20); dst.setBitmap(db.id); \
         dst.copy9Patch(src);",
    );
    let scene = env.scene();
    // The opaque frame survives; the center stays transparent.
    assert_eq!(pixel_index(&scene, 1, 0, 0), [255, 255, 255, 255]);
    assert_eq!(pixel_index(&scene, 1, 10, 10), [0, 0, 0, 0]);
}

/// `drawCurve` flattens a Catmull-Rom spline through the points and strokes
/// it with the appearance pen.
#[test]
fn draw_curve_strokes_points() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); l.setSize(24, 24); \
         var b = new Bitmap(24, 24); l.setBitmap(b.id); \
         var pen = new GdiPlus.Appearance(); pen.addPen(0xffff0000, 2); \
         l.drawCurve(pen, [[2, 20], [8, 4], [16, 20], [22, 4]]);",
    );
    let scene = env.scene();
    assert!(
        pixel_index(&scene, 0, 8, 4)[3] > 0,
        "curve passes near (8,4)"
    );
}
