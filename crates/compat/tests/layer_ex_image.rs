//! `GdiPlus.Image` over the engine's decoded-image cache, and the
//! `Layer.drawImage*` family's image argument.
//!
//! Reference: `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp`
//! (`loadImage`, `GdipImage`, `LayerExDraw::drawImage*`) and
//! `reference/cpp/core/visual/GraphicsLoaderIntf.cpp` (the graphic cache every
//! load of a placed path goes through).
//!
//! The tests attach a caller-owned L2 [`Scene`] and storage to a
//! [`CompatHost`] and drive the TJS surface — the same seam the app uses — so
//! the pixel work is verified against the L2 state the members mutate.

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use api::fs::{FsPath, OpenMode};
use api::media::{DecodedImage, ImageInfo};
use common::vm_lock;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// The storage entry the tests load.
const IMAGE_NAME: &str = "images/test.img";

/// What the mounted entries contain: the synthetic codec reads the name, not
/// the bytes.
const IMAGE_BYTES: &[u8] = b"not-a-real-image";

/// A 3x2 image with a distinct colour per pixel, row-major: `(0,0)`, `(1,0)`,
/// `(2,0)`, then `(0,1)`, `(1,1)`, `(2,1)`.
const IMAGE_RGBA: [u8; 24] = [
    1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255, 13, 14, 15, 255, 16, 17, 18, 255,
];
const IMAGE_WIDTH: u32 = 3;
const IMAGE_HEIGHT: u32 = 2;

/// A bound VM + host + attached scene and storage whose image codec returns
/// [`IMAGE_RGBA`] and records the name of every decode.
///
/// Fields drop in declaration order: the host first (releasing every retained
/// script object), then the engine, the scene and the storage.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
    _storage: Box<Storage>,
    /// One entry per decoder run, in order.
    decodes: Arc<Mutex<Vec<String>>>,
}

impl Env {
    /// An env whose mounted storage holds `entries`, with the synthetic codec
    /// installed.
    fn with_files(entries: &[&str]) -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut host = CompatHost::new();
        let decodes = Arc::new(Mutex::new(Vec::new()));
        {
            let decodes = Arc::clone(&decodes);
            host.set_image_codec(
                move |_bytes, hint| {
                    decodes
                        .lock()
                        .expect("decode lock")
                        .push(hint.unwrap_or_default().to_string());
                    Ok(DecodedImage::new(
                        IMAGE_WIDTH,
                        IMAGE_HEIGHT,
                        IMAGE_RGBA.to_vec(),
                    ))
                },
                |_bytes, _hint| {
                    Ok(ImageInfo {
                        width: IMAGE_WIDTH,
                        height: IMAGE_HEIGHT,
                        has_alpha: true,
                        format: Some("img".to_string()),
                    })
                },
                |_name, _type, _width, _height, _rgba| Ok(()),
            );
        }
        let files: Vec<(String, &[u8])> = entries
            .iter()
            .map(|entry| (format!("/{entry}"), IMAGE_BYTES))
            .collect();
        let mut storage = Box::new(memory_storage(&files));
        // SAFETY: the boxed scene and storage outlive the host (they are
        // declared after `_host`, so they drop later).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            scene,
            _storage: storage,
            decodes,
        }
    }

    /// An env with one window holding a transparent 4x4 layer `dst`.
    fn with_layer() -> Env {
        let env = Env::with_files(&[IMAGE_NAME]);
        env.run(
            "var w = new Window(); \
             var dst = new Layer(w, null); \
             dst.name = 'dst'; \
             dst.setImageSize(4, 4);",
        );
        env
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script_discard(script, "layer-ex-image")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "layer-ex-image")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            TjsValue::Integer(value) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a number"),
        }
    }

    /// The number of decoder runs so far.
    fn decodes(&self) -> usize {
        self.decodes.lock().expect("decode lock").len()
    }

    /// The names the decoder was handed, in order.
    fn decoded_names(&self) -> Vec<String> {
        self.decodes.lock().expect("decode lock").clone()
    }

    /// The id of the scene layer whose `name` member is `name`.
    fn layer_id(&self, name: &str) -> u32 {
        self.scene
            .layers
            .iter()
            .find(|layer| layer.name == name)
            .unwrap_or_else(|| panic!("no scene layer named {name:?}"))
            .id
    }

    /// The straight-alpha RGBA pixel at `(x, y)` of a layer's MainImage.
    fn pixel(&self, layer_id: u32, x: u32, y: u32) -> [u8; 4] {
        let bitmap_id = self
            .scene
            .layer(layer_id)
            .and_then(|layer| layer.bitmap)
            .expect("layer bitmap");
        let bitmap = self.scene.bitmap(bitmap_id).expect("bitmap");
        let offset = bitmap.pixel_offset(x, y).expect("pixel in range");
        [
            bitmap.rgba[offset],
            bitmap.rgba[offset + 1],
            bitmap.rgba[offset + 2],
            bitmap.rgba[offset + 3],
        ]
    }
}

/// Write `data` to `path` in an in-memory file system, creating parents.
fn write_memory_file(fs: &FileSystem, path: &str, data: &[u8]) {
    if let Some(parent) = Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(fs, &FsPath::host(parent)).expect("create parent dirs");
    }
    let file = fs::open(fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
    fs::write_at(fs, &file, 0, data).expect("write memory file");
    fs::flush(fs, &file).expect("flush memory file");
}

/// An in-memory storage holding `files` (written before the mount).
fn memory_storage(files: &[(String, &[u8])]) -> Storage {
    let fs = FileSystem::memory();
    for (path, bytes) in files {
        write_memory_file(&fs, path, bytes);
    }
    Storage::mount_archives(fs, "/", std::iter::empty::<&str>()).expect("mount storage")
}

// ---------------------------------------------------------------------------
// The decoded-image cache
// ---------------------------------------------------------------------------

/// A second load of the same storage entry is served from the engine's
/// decoded-image cache instead of decoding the pixels again; disabling the
/// cache (the default) decodes every time.
///
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1951-1960: a graphic load
/// looks the name up in the graphic cache and stores the decoded pixels under
/// it; :2423-2441: `TVPSetGraphicCacheLimit(0)` disables the cache.
#[test]
fn gdiplus_image_load_reuses_the_decoded_image_cache() {
    let _lock = vm_lock();
    let env = Env::with_files(&[IMAGE_NAME]);

    // `System.graphicCacheLimit` is the script side of the reference's graphic
    // cache limit (`reference/cpp/core/base/impl/SystemImpl.cpp:358-361`).
    env.run("System.graphicCacheLimit = 1048576;");
    assert_eq!(env.scene.graphic_cache_limit(), 1_048_576);

    env.run(&format!("var first = new GdiPlus.Image('{IMAGE_NAME}');"));
    env.run(&format!("var second = new GdiPlus.Image('{IMAGE_NAME}');"));
    assert_eq!(
        env.decodes(),
        1,
        "the second load of the same entry is a cache hit"
    );
    assert_eq!(env.decoded_names(), vec![IMAGE_NAME.to_string()]);
    // The cached pixels and the probed alpha channel both reach the script.
    assert_eq!(env.eval_int("second.GetWidth()"), 3);
    assert_eq!(env.eval_int("second.GetHeight()"), 2);
    assert_eq!(env.eval_int("second.GetPixelFormat()"), 2_498_570);

    // `load` reads the same key, so it is a hit too.
    env.run(&format!("first.load('{IMAGE_NAME}');"));
    assert_eq!(env.decodes(), 1);
    assert_eq!(env.eval_int("first.GetWidth()"), 3);

    // With the cache disabled every load decodes again.
    env.run("System.graphicCacheLimit = 0;");
    assert_eq!(env.scene.graphic_cache_limit(), 0);
    env.run(&format!("var third = new GdiPlus.Image('{IMAGE_NAME}');"));
    assert_eq!(env.decodes(), 2);
}

/// The cache key is the storage name the candidate search resolved, not the
/// name the script asked for: a load that needed the extension probe shares
/// the entry of a load that named the file.
///
/// reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1591-1610 (the placed-path
/// search picks the existing candidate) and :1951-1960 (the cache is keyed by
/// the resolved name).
#[test]
fn gdiplus_image_cache_key_is_the_resolved_storage_name() {
    let _lock = vm_lock();
    let env = Env::with_files(&["images/test.png"]);
    env.run("System.graphicCacheLimit = 1048576;");

    env.run("var named = new GdiPlus.Image('images/test.png');");
    assert_eq!(env.decoded_names(), vec!["images/test.png".to_string()]);

    // `images/test` resolves through the extension probe to the same entry.
    env.run("var probed = new GdiPlus.Image('images/test');");
    assert_eq!(env.decodes(), 1, "the probed load shares the entry");
    assert_eq!(env.eval_int("probed.GetWidth()"), 3);
}

// ---------------------------------------------------------------------------
// The draw members' image argument
// ---------------------------------------------------------------------------

/// `GdiPlus.Image` is the image argument of all four draw members: the source
/// rectangle selects the image's pixels and the destination rectangle places
/// them.
///
/// reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2264-2267
/// (`drawImage`), :2280-2284 (`drawImageRect`), :2300-2307
/// (`drawImageStretch`) and :2319-2348 (`drawImageAffine`) all take the
/// plugin's `GdipImage`.
#[test]
fn layer_draw_members_accept_a_gdiplus_image() {
    let _lock = vm_lock();
    let env = Env::with_layer();
    let dst = env.layer_id("dst");
    env.run(&format!(
        "var img = new GdiPlus.Image('{IMAGE_NAME}'); dst.clear(0);"
    ));

    env.run("dst.drawImage(0, 0, img);");
    assert_eq!(env.pixel(dst, 0, 0), [1, 2, 3, 255]);
    assert_eq!(env.pixel(dst, 2, 0), [7, 8, 9, 255]);
    assert_eq!(env.pixel(dst, 1, 1), [13, 14, 15, 255]);
    assert_eq!(env.pixel(dst, 2, 1), [16, 17, 18, 255]);

    env.run("dst.clear(0); dst.drawImageRect(1, 1, img, 1, 0, 2, 2);");
    assert_eq!(env.pixel(dst, 1, 1), [4, 5, 6, 255]);
    assert_eq!(env.pixel(dst, 2, 1), [7, 8, 9, 255]);
    assert_eq!(env.pixel(dst, 1, 2), [13, 14, 15, 255]);
    assert_eq!(env.pixel(dst, 2, 2), [16, 17, 18, 255]);

    env.run("dst.clear(0); dst.drawImageStretch(0, 0, 4, 4, img, 0, 0, 3, 2);");
    assert_eq!(env.pixel(dst, 0, 0), [1, 2, 3, 255]);
    assert_eq!(env.pixel(dst, 3, 3), [16, 17, 18, 255]);

    env.run("dst.clear(0); dst.drawImageAffine(img, 0, 0, 3, 2, true, 1, 0, 0, 1, 0, 0);");
    assert_eq!(env.pixel(dst, 0, 0), [1, 2, 3, 255]);
    assert_eq!(env.pixel(dst, 2, 1), [16, 17, 18, 255]);
}

/// The plugin draws inside the clip rectangle, so an affine pass writes the
/// pixels of the clip alone and leaves the ones outside it.
///
/// The reference sets the drawing surface's clip region from the plugin's
/// `clipLeft`/`clipTop`/`clipWidth`/`clipHeight`
/// (`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:964-976`);
/// the port applies the layer's own clip rectangle to every draw, so the
/// pass stays inside it.
#[test]
fn layer_draw_affine_writes_inside_the_clip_alone() {
    let _lock = vm_lock();
    let env = Env::with_layer();
    let dst = env.layer_id("dst");
    // A second layer without a clip receives the same pass, so the pixels
    // inside the clip can be compared with the ones an unclipped pass writes.
    env.run(
        "var whole = new Layer(w, null); \
         whole.name = 'whole'; \
         whole.setImageSize(4, 4);",
    );
    let whole = env.layer_id("whole");
    let mut before = Vec::new();
    for y in 0..4u32 {
        for x in 0..4u32 {
            before.push(env.pixel(dst, x, y));
        }
    }
    env.run("dst.setClip(1, 1, 2, 2);");
    env.run(&format!(
        "var img = new GdiPlus.Image('{IMAGE_NAME}'); \
         dst.drawImageAffine(img, 0, 0, 3, 2, true, 2, 0, 0, 2, 0, 0); \
         whole.drawImageAffine(img, 0, 0, 3, 2, true, 2, 0, 0, 2, 0, 0);"
    ));
    for y in 0..4u32 {
        for x in 0..4u32 {
            let index = (y * 4 + x) as usize;
            let inside = (1..3).contains(&x) && (1..3).contains(&y);
            let clipped = env.pixel(dst, x, y);
            let unclipped = env.pixel(whole, x, y);
            if inside {
                assert_eq!(
                    clipped, unclipped,
                    "the clipped pass writes the pixels of the unclipped one at ({x}, {y})"
                );
                assert_ne!(
                    clipped, before[index],
                    "the pass writes inside the clip at ({x}, {y})"
                );
            } else {
                assert_eq!(
                    clipped, before[index],
                    "the pixels outside the clip keep their value at ({x}, {y})"
                );
                assert_ne!(
                    unclipped, before[index],
                    "the unclipped pass writes outside the clip at ({x}, {y})"
                );
            }
        }
    }
}

/// `RotateFlip` turns the image's pixels in place: a quarter turn swaps the
/// reported width and height, and `GetBounds` and `drawImage` agree with the
/// rotated raster.
///
/// reference/cpp/plugins/layerex_draw/windows/main.cpp:574 binds `RotateFlip`
/// to GDI+ `Image::RotateFlip`, which rotates the image itself; :555 and :572
/// report that image's height and width, and :546 `GetBounds` returns its own
/// bounds. reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:100-103
/// takes the bounds from `Image::GetBounds`, :1608-1613 draws the image at
/// that bounds origin and size, and :1695-1696 hands the image itself to the
/// drawing surface, so the pixels are turned once, inside `RotateFlip`. GDI+
/// accepts every `RotateFlipType` value 0..7 and answers a value outside that
/// range with `InvalidParameter` (2); the `Status` values are `Ok` = 0,
/// `InvalidParameter` = 2 and `NotImplemented` = 6
/// (reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:65-88), and the
/// shipped `layerExDraw.dll` binds the member straight to
/// `Gdiplus::Image::RotateFlip`, so the range check is GDI+'s (decompiled:
/// `plugins/analysis/_ghidra/layerExDraw.c:6020`; its status registration at
/// :922 fixes `InvalidParameter` = 2).
///
/// The blend2d build disagrees: `RotateFlip` records the transform in
/// `transMtx` without touching the raster or the reported size
/// (reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:736-773), its
/// switch reports `Rotate270FlipX` (7) as `NotImplemented`
/// (reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:736-773), and
/// its bitmap blit draws `_core` with only the caller matrix
/// (reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2339-2348), so
/// the transform never reaches a bitmap there. The Windows GDI+ build is the
/// compatibility oracle, so the port turns the pixels.
#[test]
fn gdiplus_image_rotate_flip_turns_the_pixels_in_place() {
    let _lock = vm_lock();
    let env = Env::with_layer();
    let dst = env.layer_id("dst");

    // `Rotate90FlipNone`: the 3x2 image is now a 2x3 one, so
    // `dst(1 - sy, sx)` holds the source pixel `(sx, sy)`.
    env.run(&format!(
        "var turned = new GdiPlus.Image('{IMAGE_NAME}'); \
         turned.RotateFlip(1); \
         dst.clear(0);"
    ));
    assert_eq!(env.eval_int("turned.GetWidth()"), 2);
    assert_eq!(env.eval_int("turned.GetHeight()"), 3);
    env.run("var turned_bounds = turned.GetBounds();");
    assert_eq!(env.eval_real("turned_bounds.x"), 0.0);
    assert_eq!(env.eval_real("turned_bounds.y"), 0.0);
    assert_eq!(env.eval_real("turned_bounds.width"), 2.0);
    assert_eq!(env.eval_real("turned_bounds.height"), 3.0);
    let untouched = env.pixel(dst, 2, 0);
    env.run("dst.drawImage(0, 0, turned);");
    assert_eq!(env.pixel(dst, 1, 0), [1, 2, 3, 255]);
    assert_eq!(env.pixel(dst, 0, 0), [10, 11, 12, 255]);
    assert_eq!(env.pixel(dst, 1, 2), [7, 8, 9, 255]);
    assert_eq!(env.pixel(dst, 0, 2), [16, 17, 18, 255]);
    assert_eq!(
        env.pixel(dst, 2, 0),
        untouched,
        "the rotated image covers only its 2x3 extent"
    );

    // `Rotate270FlipNone` is the same quarter turn the other way:
    // `dst(x, y)` holds the source pixel `(source width - 1 - y, x)`.
    env.run(&format!(
        "var reverse = new GdiPlus.Image('{IMAGE_NAME}'); \
         reverse.RotateFlip(3); \
         dst.clear(0); \
         dst.drawImage(0, 0, reverse);"
    ));
    assert_eq!(env.eval_int("reverse.GetWidth()"), 2);
    assert_eq!(env.eval_int("reverse.GetHeight()"), 3);
    assert_eq!(env.pixel(dst, 0, 0), [7, 8, 9, 255]);
    assert_eq!(env.pixel(dst, 1, 0), [16, 17, 18, 255]);
    assert_eq!(env.pixel(dst, 0, 2), [1, 2, 3, 255]);
    assert_eq!(env.pixel(dst, 1, 2), [10, 11, 12, 255]);

    // `Rotate270FlipX` (7) turns a quarter and then mirrors the columns:
    // `dst(x, y)` holds the source pixel `(width - 1 - y, height - 1 - x)`.
    env.run(&format!(
        "var turned_mirrored = new GdiPlus.Image('{IMAGE_NAME}'); \
         turned_mirrored.RotateFlip(7); \
         dst.clear(0); \
         dst.drawImage(0, 0, turned_mirrored);"
    ));
    assert_eq!(env.eval_int("turned_mirrored.GetWidth()"), 2);
    assert_eq!(env.eval_int("turned_mirrored.GetHeight()"), 3);
    assert_eq!(env.pixel(dst, 0, 0), [16, 17, 18, 255]);
    assert_eq!(env.pixel(dst, 1, 0), [7, 8, 9, 255]);
    assert_eq!(env.pixel(dst, 0, 2), [10, 11, 12, 255]);
    assert_eq!(env.pixel(dst, 1, 2), [1, 2, 3, 255]);

    // A value outside the `RotateFlipType` range is `InvalidParameter` (2)
    // and leaves the image as it was.
    env.run("var outside = turned_mirrored.RotateFlip(8);");
    assert_eq!(env.eval_int("outside"), 2);
    assert_eq!(env.eval_int("turned_mirrored.GetWidth()"), 2);
    assert_eq!(env.eval_int("turned_mirrored.GetHeight()"), 3);
}

/// `RotateFlip` mirrors the image's pixels in place as well: an axis flip
/// keeps the reported width and height, so `GetBounds` and `drawImage` keep
/// the original extent with the mirrored raster.
///
/// reference/cpp/plugins/layerex_draw/windows/main.cpp:574 runs GDI+
/// `Image::RotateFlip`, which flips the image itself; the bounds (:546,
/// `Image::GetBounds`) and the size accessors (:555, :572) therefore describe
/// the flipped image, which :1608-1613 then draws at that size. The blend2d
/// build stores the flip without applying it to a bitmap
/// (reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.hpp:736-773,
/// :2339-2348); the Windows GDI+ build is the oracle.
#[test]
fn gdiplus_image_rotate_flip_mirrors_the_pixels_in_place() {
    let _lock = vm_lock();
    let env = Env::with_layer();
    let dst = env.layer_id("dst");

    // `RotateNoneFlipX`: the extent stays 3x2 and the columns mirror.
    env.run(&format!(
        "var flipped = new GdiPlus.Image('{IMAGE_NAME}'); \
         flipped.RotateFlip(4); \
         dst.clear(0);"
    ));
    assert_eq!(env.eval_int("flipped.GetWidth()"), 3);
    assert_eq!(env.eval_int("flipped.GetHeight()"), 2);
    env.run("var flipped_bounds = flipped.GetBounds();");
    assert_eq!(env.eval_real("flipped_bounds.width"), 3.0);
    assert_eq!(env.eval_real("flipped_bounds.height"), 2.0);
    env.run("dst.drawImage(0, 0, flipped);");
    assert_eq!(env.pixel(dst, 0, 0), [7, 8, 9, 255]);
    assert_eq!(env.pixel(dst, 2, 0), [1, 2, 3, 255]);
    assert_eq!(env.pixel(dst, 0, 1), [16, 17, 18, 255]);
    assert_eq!(env.pixel(dst, 2, 1), [10, 11, 12, 255]);
}

/// An image with no pixels draws nothing and returns the empty update
/// rectangle, like the reference's empty image.
///
/// reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2341-2342: an
/// image whose core bitmap is empty returns `RectF()` before anything is
/// blitted.
#[test]
fn draw_image_with_an_empty_image_draws_nothing() {
    let _lock = vm_lock();
    let env = Env::with_layer();
    let dst = env.layer_id("dst");
    env.run("var empty = new GdiPlus.Image(); dst.clear(0);");
    let untouched = env.pixel(dst, 1, 1);

    env.run("var r = dst.drawImage(1, 1, empty);");
    assert_eq!(env.eval_real("r.x"), 0.0);
    assert_eq!(env.eval_real("r.y"), 0.0);
    assert_eq!(env.eval_real("r.width"), 0.0);
    assert_eq!(env.eval_real("r.height"), 0.0);
    assert_eq!(env.pixel(dst, 1, 1), untouched);

    // The same for the source-rectangle members.
    env.run("var empty_rect = dst.drawImageRect(0, 0, empty, 0, 0, 2, 2);");
    assert_eq!(env.eval_real("empty_rect.width"), 0.0);
    assert_eq!(env.pixel(dst, 1, 1), untouched);
}
