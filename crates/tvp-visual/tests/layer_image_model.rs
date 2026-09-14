//! Layer MainImage allocation / image-window model tests.
//!
//! These exercise the reference `tTJSNI_BaseLayer` image buffer management
//! (`reference/cpp/core/visual/LayerIntf.cpp`):
//!
//! * `hasImage = true` → `AllocateImage` (allocate the MainImage at the rect
//!   size filled with `NeutralColor`, reset the clip); `false` →
//!   `DeallocateImage`,
//! * `setImageSize` → `SetImageSize`/`ChangeImageSize` (require a MainImage,
//!   resize it exactly),
//! * `setSize` → `ImageLayerSizeChanged` (grow the MainImage to the rect),
//! * `imageLeft`/`imageTop` setter validation,
//! * `copyRect`/`stretchCopy`/`drawImage` allocating the destination.
//!
//! The real game symptoms these guard against: `Layer.stretchCopy: layer has
//! no image` (`ConfigWindow.tjs:1968`, the `ConfigVoiceSliderH` slider) and
//! "all cropped" artwork (the image window disagreeing with the MainImage
//! bitmap, which made the renderer sample a fraction of the sheet).

use std::sync::{Arc, Mutex, RwLock};

use engine::Storage;
use tempfile::TempDir;
use tjs2_sys::Tjs2Engine;
use tvp_visual::scene::Scene;

/// Fresh engine + scene + temp storage with the visual natives registered,
/// mirroring the other tvp-visual integration tests.
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
            .exec_script(script, "layer_image_model")
            .expect("script must run");
    }

    fn eval_string(&self, expr: &str) -> String {
        match self.engine.eval(expr, "layer_image_model") {
            Ok(tjs2_sys::TjsValue::String(s)) => s,
            other => panic!("eval {expr:?} -> {other:?}"),
        }
    }

    fn write_webp(&self, name: &str, rgba: &[u8], w: u32, h: u32) {
        let mut bytes = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
            .encode(rgba, w, h, image::ExtendedColorType::Rgba8)
            .expect("webp encode");
        std::fs::write(self._dir.path().join(name), bytes).expect("write fixture");
    }

    fn scene(&self) -> std::sync::RwLockReadGuard<'_, Scene> {
        self.scene.read().expect("scene lock poisoned")
    }
}

/// Read an RGBA pixel from a layer's attached bitmap.
fn pixel(scene: &Scene, layer_index: usize, x: u32, y: u32) -> [u8; 4] {
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

fn bitmap_dims(scene: &Scene, layer_index: usize) -> (u32, u32) {
    let bitmap_id = scene.layers[layer_index]
        .bitmap
        .expect("layer has a bitmap");
    let bitmap = scene.bitmap(bitmap_id).expect("bitmap exists");
    (bitmap.width, bitmap.height)
}

/// `hasImage = true` allocates the MainImage at the rect size, transparent,
/// resets the clip and reports `hasImage`; `false` drops it. This is the
/// setter that used to be a silent no-op and broke `ConfigVoiceSliderH`.
#[test]
fn has_image_allocates_and_deallocates_main_image() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         l.setSize(4, 3); \
         var before = l.hasImage; \
         l.hasImage = true;",
    );
    assert_eq!(env.eval_string("before == false ? 'no' : 'yes'"), "no");
    {
        let scene = env.scene();
        let layer = &scene.layers[0];
        assert!(layer.bitmap.is_some(), "hasImage=true allocates");
        assert_eq!((layer.rect.w, layer.rect.h), (4, 3));
        assert_eq!((layer.image_left, layer.image_top), (0, 0));
        assert_eq!((layer.image_width, layer.image_height), (4, 3));
        assert!(layer.clip.is_none(), "allocation resets the clip");
        assert_eq!(bitmap_dims(&scene, 0), (4, 3));
        // NeutralColor defaults to transparent.
        assert_eq!(pixel(&scene, 0, 1, 1), [0, 0, 0, 0]);
    }
    assert_eq!(env.eval_string("l.hasImage ? 'yes' : 'no'"), "yes");

    env.run("l.hasImage = false;");
    {
        let scene = env.scene();
        assert!(scene.layers[0].bitmap.is_none(), "hasImage=false drops it");
    }
    assert_eq!(env.eval_string("l.hasImage ? 'yes' : 'no'"), "no");
}

/// A zero-sized rect allocates a 1x1 image (the reference `tTVPBaseTexture`
/// constructor clamps `0` to `1`), and `setSize` then grows it.
#[test]
fn has_image_on_empty_rect_allocates_one_pixel_then_grows() {
    let env = Env::new();
    env.run("var w = new Window(); var l = new Layer(w, null); l.hasImage = true;");
    assert_eq!(bitmap_dims(&env.scene(), 0), (1, 1));
    env.run("l.setSize(5, 2);");
    {
        let scene = env.scene();
        assert_eq!(bitmap_dims(&scene, 0), (5, 2), "setSize grows the image");
        assert_eq!(
            (scene.layers[0].image_width, scene.layers[0].image_height),
            (5, 2)
        );
    }
}

/// `hasImage = true` uses the layer's `neutralColor` to fill the new image.
#[test]
fn has_image_fills_with_neutral_color() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         l.neutralColor = 0xff00ff00; l.setSize(2, 2); l.hasImage = true;",
    );
    let scene = env.scene();
    assert_eq!(pixel(&scene, 0, 0, 0), [0, 255, 0, 255]);
    assert_eq!(pixel(&scene, 0, 1, 1), [0, 255, 0, 255]);
}

/// `setImageSize` requires a MainImage (`TVPNotDrawableLayerType`) and
/// rejects an empty size (`TVPCannotCreateEmptyLayerImage`).
#[test]
fn set_image_size_requires_main_image() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); l.setSize(4, 4); \
         var threw = false; try { l.setImageSize(8, 8); } catch (e) { threw = true; }",
    );
    assert_eq!(env.eval_string("threw ? 'yes' : 'no'"), "yes");

    env.run(
        "l.hasImage = true; \
         var empty = false; try { l.setImageSize(0, 8); } catch (e) { empty = true; }",
    );
    assert_eq!(env.eval_string("empty ? 'yes' : 'no'"), "yes");
}

/// The core "cropped images" regression: `setImageSize` must resize the
/// MainImage itself, not merely record a larger image window. If it only set
/// `imageWidth`, the renderer would scale a small bitmap up into the big
/// window (sampling a fraction of the sheet).
#[test]
fn set_image_size_resizes_main_image() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         l.neutralColor = 0xff00ff00; \
         l.setSize(2, 2); l.hasImage = true; \
         l.colorRect(0, 0, 1, 1, 0xff0000ff); \
         l.setImageSize(4, 2);",
    );
    let scene = env.scene();
    assert_eq!(
        bitmap_dims(&scene, 0),
        (4, 2),
        "MainImage resized to the requested image window"
    );
    assert_eq!(
        (scene.layers[0].image_width, scene.layers[0].image_height),
        (4, 2),
        "image window mirrors the MainImage"
    );
    assert_eq!(pixel(&scene, 0, 0, 0), [0, 0, 255, 255], "old pixel kept");
    assert_eq!(
        pixel(&scene, 0, 3, 1),
        [0, 255, 0, 255],
        "expansion filled with neutralColor"
    );
}

/// `setSize` runs `ImageLayerSizeChanged`: it grows the MainImage to the
/// layer rect (never shrinks it) and keeps the offset covering the rect.
#[test]
fn set_size_grows_main_image() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         l.setSize(2, 2); l.hasImage = true; \
         l.setSize(8, 4);",
    );
    {
        let scene = env.scene();
        assert_eq!(
            bitmap_dims(&scene, 0),
            (8, 4),
            "setSize grows the MainImage"
        );
        assert_eq!(
            (scene.layers[0].image_width, scene.layers[0].image_height),
            (8, 4)
        );
    }

    // Growing again must not shrink the image, and the image window follows
    // the (larger) bitmap, not the smaller rect.
    env.run("l.setSize(3, 3);");
    let scene = env.scene();
    assert_eq!(
        bitmap_dims(&scene, 0),
        (8, 4),
        "ImageLayerSizeChanged never shrinks the MainImage"
    );
    assert_eq!(
        (scene.layers[0].image_width, scene.layers[0].image_height),
        (8, 4)
    );
}

/// `imageLeft`/`imageTop` require a MainImage and reject positive offsets.
#[test]
fn image_left_top_validation() {
    let env = Env::new();
    env.run(
        "var w = new Window(); var l = new Layer(w, null); l.setSize(4, 4); \
         var noImage = false; try { l.imageLeft = -1; } catch (e) { noImage = true; } \
         l.hasImage = true; \
         var positive = false; try { l.imageLeft = 1; } catch (e) { positive = true; } \
         var positiveTop = false; try { l.imageTop = 1; } catch (e) { positiveTop = true; } \
         l.imageLeft = -1; l.imageTop = -2;",
    );
    assert_eq!(env.eval_string("noImage ? 'yes' : 'no'"), "yes");
    assert_eq!(env.eval_string("positive ? 'yes' : 'no'"), "yes");
    assert_eq!(env.eval_string("positiveTop ? 'yes' : 'no'"), "yes");
    {
        let scene = env.scene();
        assert_eq!(
            (scene.layers[0].image_left, scene.layers[0].image_top),
            (-1, -2)
        );
    }
    assert_eq!(env.eval_string("'' + l.imageLeft"), "-1");
    assert_eq!(env.eval_string("'' + l.imageTop"), "-2");
}

/// `system/SelectItem.tjs:310` `Button.create`: size the layer to one cell,
/// grow the image to the whole sheet, copy the sheet, then pan to the frame.
/// The MainImage must remain the full sheet so `setImagePos(-w*n, 0)` selects
/// a cell without the renderer scaling the sheet into one cell.
#[test]
fn button_create_sheet_pattern() {
    let env = Env::new();
    // A 6x2 sheet: three 2x2 cells, red / green / blue.
    let mut sheet = Vec::new();
    for y in 0..2u32 {
        for x in 0..6u32 {
            let color = if x < 2 {
                [255u8, 0, 0, 255]
            } else if x < 4 {
                [0, 255, 0, 255]
            } else {
                [0, 0, 255, 255]
            };
            let _ = y;
            sheet.extend_from_slice(&color);
        }
    }
    env.write_webp("button_sheet.webp", &sheet, 6, 2);

    env.run(
        "var w = new Window(); \
         var btn = new Layer(w, null); \
         btn.hasImage = true; \
         btn.setSize(2, 2); \
         btn.setImageSize(6, 2); \
         var sheet = new Bitmap('button_sheet'); \
         btn.copyRect(0, 0, sheet, 0, 0, 6, 2); \
         btn.setImagePos(-2, 0);",
    );

    let scene = env.scene();
    let btn = &scene.layers[0];
    assert_eq!((btn.rect.w, btn.rect.h), (2, 2), "layer rect is one cell");
    assert_eq!(
        (btn.image_width, btn.image_height),
        (6, 2),
        "the image window is the whole sheet, not the cell"
    );
    assert_eq!(bitmap_dims(&scene, 0), (6, 2), "the sheet is not cropped");
    assert_eq!(
        (btn.image_left, btn.image_top),
        (-2, 0),
        "setButton(1) pans to the second cell"
    );
    // All three cells are present in the MainImage.
    assert_eq!(pixel(&scene, 0, 0, 0), [255, 0, 0, 255]);
    assert_eq!(pixel(&scene, 0, 2, 0), [0, 255, 0, 255]);
    assert_eq!(pixel(&scene, 0, 4, 0), [0, 0, 255, 255]);
}

/// `system/ConfigWindow.tjs:1941` `ConfigVoiceSliderH`: the ctor sets
/// `hasImage = true`, `create` sizes the slider, and `setTrimPos` runs two
/// `stretchCopy` calls with a `Bitmap` source and `stFastLanczos2` (the
/// constant is `7`; this test env does not load the game's constants). Before
/// the `hasImage` setter allocated, this threw
/// `Layer.stretchCopy: layer has no image`.
#[test]
fn config_voice_slider_stretch_copy_flow() {
    let env = Env::new();
    // A small opaque bitmap source (the slider's trim/knob graphic).
    let src: Vec<u8> = (0..16).flat_map(|_| [200u8, 40, 10, 255]).collect();
    env.write_webp("slider_trim.webp", &src, 4, 4);

    env.run(
        "var w = new Window(); \
         var slider = new Layer(w, null); \
         slider.hasImage = true; \
         slider.setSize(100, 20); \
         var trim = new Bitmap('slider_trim'); \
         slider.stretchCopy(0, 0, 100, 10, trim, 0, 0, 4, 4, 7); \
         slider.stretchCopy(0, 10, 100, 10, trim, 0, 0, 4, 4, 7);",
    );

    let scene = env.scene();
    let slider = &scene.layers[0];
    assert_eq!((slider.rect.w, slider.rect.h), (100, 20));
    assert_eq!(bitmap_dims(&scene, 0), (100, 20));
    assert_eq!(
        (slider.image_width, slider.image_height),
        (100, 20),
        "stretchCopy requires and uses the allocated image"
    );
    // Both blits painted: top half and bottom half each carry the source.
    assert_eq!(
        pixel(&scene, 0, 50, 5),
        [200, 40, 10, 255],
        "first stretchCopy painted the top half"
    );
    assert_eq!(
        pixel(&scene, 0, 50, 15),
        [200, 40, 10, 255],
        "second stretchCopy painted the bottom half"
    );
}

/// `copyRect` allocates the destination MainImage at the copy extent when the
/// layer has none (rather than silently no-op'ing).
#[test]
fn copy_rect_allocates_dest_image() {
    let env = Env::new();
    let src: Vec<u8> = (0..16).flat_map(|_| [1u8, 2, 3, 255]).collect();
    env.write_webp("cr_src.webp", &src, 4, 4);
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         var b = new Bitmap('cr_src'); \
         l.copyRect(0, 0, b, 0, 0, 4, 4);",
    );
    let scene = env.scene();
    assert_eq!(
        bitmap_dims(&scene, 0),
        (4, 4),
        "copyRect allocated the destination"
    );
    assert_eq!(pixel(&scene, 0, 2, 2), [1, 2, 3, 255]);
}

/// `stretchCopy` allocates the destination MainImage at the destination
/// extent when absent.
#[test]
fn stretch_copy_allocates_dest_image() {
    let env = Env::new();
    let src: Vec<u8> = (0..16).flat_map(|_| [9u8, 8, 7, 255]).collect();
    env.write_webp("sc_src.webp", &src, 4, 4);
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         var b = new Bitmap('sc_src'); \
         l.stretchCopy(0, 0, 8, 6, b, 0, 0, 4, 4);",
    );
    let scene = env.scene();
    assert_eq!(bitmap_dims(&scene, 0), (8, 6));
    assert_eq!(pixel(&scene, 0, 4, 3), [9, 8, 7, 255]);
}

/// `drawImage*` allocates the destination MainImage at the destination
/// extent when absent.
#[test]
fn draw_image_allocates_dest_image() {
    let env = Env::new();
    let src: Vec<u8> = (0..16).flat_map(|_| [4u8, 5, 6, 255]).collect();
    env.write_webp("di_src.webp", &src, 4, 4);
    env.run(
        "var w = new Window(); var l = new Layer(w, null); \
         var b = new Bitmap('di_src'); \
         l.drawImageRect(0, 0, b, 0, 0, 4, 4);",
    );
    let scene = env.scene();
    assert_eq!(bitmap_dims(&scene, 0), (4, 4));
    assert_eq!(pixel(&scene, 0, 1, 1), [4, 5, 6, 255]);
}
