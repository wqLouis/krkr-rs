//! Layer geometry and subtree-cache behaviour: the reference image-size
//! adjustment applied after a load or an assign, the `imageLeft`/`imageTop`
//! guards and `Layer.cached`.
//!
//! The tests attach a caller-owned L2 [`Scene`] plus a mounted storage and a
//! synthetic image codec to a [`CompatHost`] and run TJS against it — the same
//! seam the app uses — so the geometry they assert is the L2 state the natives
//! actually leave behind.

mod common;

use api::fs::{FsPath, OpenMode};
use api::media::{DecodedImage, ImageInfo};
use common::vm_lock;
use compat::CompatHost;
use engine::{Scene, Storage};
use fs::FileSystem;
use tjs::Tjs2Engine;

/// Writes `data` to `path` in an in-memory file system.
fn write_memory_file(fs: &FileSystem, path: &str, data: &[u8]) {
    let file = fs::open(fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
    fs::write_at(fs, &file, 0, data).expect("write memory file");
    fs::flush(fs, &file).expect("flush memory file");
}

/// A bound VM + host + scene whose storage holds `clickglyph.png`, decoded by a
/// synthetic codec to a `width` x `height` opaque image.
///
/// The boxed scene and storage are declared **after** `host` so the host
/// (which stores their raw addresses) is dropped first.
struct Env {
    host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
    /// Kept alive for the host's raw pointer; never read directly.
    _storage: Box<Storage>,
}

impl Env {
    fn with_image(width: u32, height: u32) -> Env {
        let fs = FileSystem::memory();
        write_memory_file(&fs, "clickglyph.png", b"clickglyph");
        let storage =
            Storage::mount_archives(fs, "/", std::iter::empty::<&str>()).expect("mount storage");
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut storage = Box::new(storage);
        let mut host = CompatHost::new();
        let pixels = usize::try_from(width).expect("image width")
            * usize::try_from(height).expect("image height")
            * 4;
        host.set_image_codec(
            move |_bytes, _hint| Ok(DecodedImage::new(width, height, vec![0xff; pixels])),
            move |_bytes, _hint| {
                Ok(ImageInfo {
                    width,
                    height,
                    has_alpha: true,
                    format: Some("png".to_string()),
                })
            },
            |_name, _type, _width, _height, _rgba| Ok(()),
        );
        // SAFETY: the boxed scene and storage outlive the host (they are
        // declared after `host`), matching `attach_scene`/`attach_storage`.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            host,
            engine,
            scene,
            _storage: storage,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script_discard(script, "layer-geometry-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self
            .engine
            .eval(expression, "layer-geometry-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
        {
            tjs::TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn error(&self, expression: &str) -> String {
        self.engine
            .eval(expression, "layer-geometry-test")
            .expect_err(&format!("{expression:?} should have raised"))
            .to_string()
    }

    /// One frame-body cache pass (the `frame_tail` step the app runs after the
    /// transition step).
    fn refresh_caches(&self) {
        let ctx = self.host.ctx().expect("bound compat context");
        compat::refresh_cached_layers(&ctx).expect("refresh cached layers");
    }
}

/// The most recently created scene layer carrying the script `name`.
fn named_layer<'a>(env: &'a Env, name: &str) -> &'a engine::LayerState {
    env.scene
        .layers
        .iter()
        .rev()
        .find(|layer| layer.name == name)
        .unwrap_or_else(|| panic!("layer {name:?} not found"))
}

/// The scene bitmaps one window projection asks its texture table for, in draw
/// order. An empty texture table reports the projection's choice without
/// needing a render pipeline.
fn requested_bitmaps(env: &Env, window: u32) -> Vec<u32> {
    let mut requested = Vec::new();
    engine::project_window(&env.scene, window, |id| {
        requested.push(id);
        None
    });
    requested
}

/// The `0xRRGGBB` colour of a scene bitmap at `(x, y)`.
fn scene_pixel(env: &Env, bitmap: u32, x: u32, y: u32) -> i64 {
    let bitmap = env.scene.bitmap(bitmap).expect("scene bitmap");
    let offset = bitmap.pixel_offset(x, y).expect("pixel offset");
    let px = &bitmap.rgba[offset..offset + 4];
    (i64::from(px[0]) << 16) | (i64::from(px[1]) << 8) | i64::from(px[2])
}

/// `LoadImages` installs the graphic and then calls `InternalSetImageSize` with
/// the image's dimensions (`reference/cpp/core/visual/LayerIntf.cpp:2854`): a
/// smaller image shrinks the layer rectangle and clears the image offset, a
/// larger one leaves the rectangle alone, and the offset clamp is measured
/// against the image that was just installed (`:2626-2645`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:2626-2645
/// reference/cpp/core/visual/LayerIntf.cpp:2854
#[test]
fn load_images_applies_the_image_size_adjustment() {
    let _lock = vm_lock();
    let env = Env::with_image(60, 1500);
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setSize(200, 100); \
         l.loadImages('clickglyph.png');",
    );
    // 60 < 200 shrinks the width to 60; 1500 does not shrink the height.
    assert_eq!(env.eval_int("l.width"), 60);
    assert_eq!(env.eval_int("l.height"), 100);
    assert_eq!(env.eval_int("l.imageWidth"), 60);
    assert_eq!(env.eval_int("l.imageHeight"), 1500);
    assert_eq!(env.eval_int("l.imageLeft"), 0);
    assert_eq!(env.eval_int("l.imageTop"), 0);

    // The 100-pixel rectangle is covered by the 1500-pixel image from offsets
    // down to -1400; -1401 leaves it 99 pixels short.
    env.run("l.imageTop = -1400;");
    assert_eq!(env.eval_int("l.imageTop"), -1400);
    let message = env.error("l.imageTop = -1401");
    assert!(
        message.contains("invalid image position"),
        "unexpected message: {message}"
    );
    assert_eq!(
        env.eval_int("l.imageTop"),
        -1400,
        "the rejected value is not stored"
    );
}

/// `AssignImages` copies only the images
/// (`reference/cpp/core/visual/LayerIntf.cpp:2399-2417`): the destination keeps
/// its own `imageLeft`/`imageTop`, and its rectangle is then adjusted to the
/// assigned image (`:2417-2420`, `:2623-2643`), so a destination wider than the
/// source shrinks to it and a destination the image covers keeps its rectangle
/// and accepts the last clip offset the animation uses.
///
/// reference/cpp/core/visual/LayerIntf.cpp:2399-2420
/// reference/cpp/core/visual/LayerIntf.cpp:2623-2643
#[test]
fn assign_images_applies_the_image_size_adjustment() {
    let _lock = vm_lock();
    let env = Env::with_image(60, 1500);
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.setSize(60, 1500); \
         src.loadImages('clickglyph.png'); \
         src.setSize(60, 60); \
         src.setImagePos(0, -1440); \
         var wide = new Layer(w, null); \
         wide.setSize(200, 100); \
         wide.setImageSize(260, 160); \
         wide.setImagePos(-40, -30); \
         wide.assignImages(src); \
         var glyph = new Layer(w, null); \
         glyph.setSize(60, 60); \
         glyph.assignImages(src); \
         glyph.setImagePos(0, -1440);",
    );
    // 60 < 200 shrinks the width, 1500 leaves the height at 100. The offset is
    // the destination's own -30, not the source's -1440: only images are
    // copied, and -30 still covers the 100-pixel rectangle.
    assert_eq!(env.eval_int("wide.width"), 60);
    assert_eq!(env.eval_int("wide.height"), 100);
    assert_eq!(env.eval_int("wide.imageWidth"), 60);
    assert_eq!(env.eval_int("wide.imageHeight"), 1500);
    assert_eq!(env.eval_int("wide.imageLeft"), 0);
    assert_eq!(env.eval_int("wide.imageTop"), -30);

    // The 60x60 destination keeps its rectangle and receives the taller image;
    // 1500 - 1440 is exactly the rectangle height, so the clip frame is valid.
    assert_eq!(env.eval_int("glyph.width"), 60);
    assert_eq!(env.eval_int("glyph.height"), 60);
    assert_eq!(env.eval_int("glyph.imageWidth"), 60);
    assert_eq!(env.eval_int("glyph.imageHeight"), 1500);
    assert_eq!(env.eval_int("glyph.imageTop"), -1440);
}

/// `AssignMainImageWithUpdate` installs the bitmap as the MainImage and then
/// applies `InternalSetImageSize`
/// (`reference/cpp/core/visual/LayerIntf.cpp:2446-2449`), so the layer
/// rectangle shrinks to a smaller bitmap and an offset that leaves the new
/// image short of the rectangle is clamped (`:2623-2643`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:2432-2460
/// reference/cpp/core/visual/LayerIntf.cpp:2623-2643
#[test]
fn copy_from_bitmap_applies_the_image_size_adjustment() {
    let _lock = vm_lock();
    let env = Env::with_image(2, 2);
    env.run(
        "var w = new Window(); \
         var small = new Layer(w, null); \
         small.setSize(200, 100); \
         small.copyFromBitmapToMainImage(new Bitmap(60, 20)); \
         var clamped = new Layer(w, null); \
         clamped.setSize(100, 100); \
         clamped.setImageSize(150, 100); \
         clamped.setImagePos(-20, 0); \
         clamped.copyFromBitmapToMainImage(new Bitmap(110, 100));",
    );
    // 60 < 200 and 20 < 100 shrink both extents to the bitmap and clear the
    // offsets.
    assert_eq!(env.eval_int("small.width"), 60);
    assert_eq!(env.eval_int("small.height"), 20);
    assert_eq!(env.eval_int("small.imageWidth"), 60);
    assert_eq!(env.eval_int("small.imageHeight"), 20);
    assert_eq!(env.eval_int("small.imageLeft"), 0);
    assert_eq!(env.eval_int("small.imageTop"), 0);

    // The 100x100 rectangle stays; 110 + (-20) is 90, so the offset shifts to
    // -10 to keep the image covering the rectangle.
    assert_eq!(env.eval_int("clamped.width"), 100);
    assert_eq!(env.eval_int("clamped.height"), 100);
    assert_eq!(env.eval_int("clamped.imageWidth"), 110);
    assert_eq!(env.eval_int("clamped.imageHeight"), 100);
    assert_eq!(env.eval_int("clamped.imageLeft"), -10);
    assert_eq!(env.eval_int("clamped.imageTop"), 0);
}

/// `copyFromBitmapToMainImage(null)` copies nothing and raises nothing: `null`
/// is an object value with no object, so the reference skips the assignment
/// and returns `TJS_S_OK`
/// (`reference/cpp/core/visual/LayerIntf.cpp:9924-9935`); a non-object argument
/// raises the variant-conversion error and a non-Bitmap object raises
/// `TJS_E_INVALIDPARAM` (`:9926-9930`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:9924-9935
#[test]
fn copy_from_bitmap_accepts_null_as_a_noop() {
    let _lock = vm_lock();
    let env = Env::with_image(2, 2);
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setSize(8, 8); \
         l.setImageSize(8, 8); \
         l.fillRect(0, 0, 8, 8, 0xffff0000); \
         l.copyFromBitmapToMainImage(null);",
    );
    // The null copy leaves the image, the rectangle and the pixels untouched.
    assert_eq!(env.eval_int("l.width"), 8);
    assert_eq!(env.eval_int("l.height"), 8);
    assert_eq!(env.eval_int("l.imageWidth"), 8);
    assert_eq!(env.eval_int("l.imageHeight"), 8);
    assert_eq!(env.eval_int("l.getMainPixel(0, 0)"), 0xff0000);

    // A non-object argument is a variant-conversion error, not a no-op, and a
    // non-Bitmap object is an invalid parameter.
    let message = env.error("l.copyFromBitmapToMainImage(1)");
    assert!(
        message.contains("not a Bitmap"),
        "unexpected message: {message}"
    );
    let message = env.error("l.copyFromBitmapToMainImage(%[])");
    assert!(
        message.contains("not a Bitmap"),
        "unexpected message: {message}"
    );
}

/// `SetImageLeft`/`SetImageTop` raise `TVPNotDrawableLayerType` without a main
/// image, `TVPInvalidImagePosition` for a positive offset or one that leaves
/// the image short of the layer rectangle, and otherwise store the offset —
/// including a re-write of the value the layer already holds
/// (`reference/cpp/core/visual/LayerIntf.cpp:2504-2544`).
///
/// reference/cpp/core/visual/LayerIntf.cpp:2504-2544
#[test]
fn image_offset_setters_apply_the_reference_guard() {
    let _lock = vm_lock();
    let env = Env::with_image(60, 1500);
    // A 10x20 rectangle over the 60x1500 image: offsets down to -50 / -1480
    // still cover it.
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setSize(10, 20); \
         l.loadImages('clickglyph.png');",
    );
    assert_eq!(env.eval_int("l.width"), 10);
    assert_eq!(env.eval_int("l.height"), 20);

    env.run("l.imageLeft = -50; l.imageTop = -1480;");
    assert_eq!(env.eval_int("l.imageLeft"), -50);
    assert_eq!(env.eval_int("l.imageTop"), -1480);

    for (expression, member) in [
        ("l.imageLeft = 1", "imageLeft"),
        ("l.imageLeft = -51", "imageLeft"),
        ("l.imageTop = 1", "imageTop"),
        ("l.imageTop = -1481", "imageTop"),
    ] {
        let message = env.error(expression);
        assert!(
            message.contains("invalid image position"),
            "{member}: unexpected message: {message}"
        );
    }
    assert_eq!(
        env.eval_int("l.imageLeft"),
        -50,
        "a rejected left is not stored"
    );
    assert_eq!(
        env.eval_int("l.imageTop"),
        -1480,
        "a rejected top is not stored"
    );

    // An unchanged value stores nothing and raises nothing.
    env.run("l.imageLeft = -50; l.imageTop = -1480;");
    assert_eq!(env.eval_int("l.imageLeft"), -50);
    assert_eq!(env.eval_int("l.imageTop"), -1480);

    // Without a main image the setters raise before any range check.
    env.run("l.hasImage = false;");
    for expression in ["l.imageLeft = 0", "l.imageTop = 0"] {
        let message = env.error(expression);
        assert!(
            message.contains("Not drawable layer type"),
            "unexpected message: {message}"
        );
    }
}

/// `GetImageLeft`/`GetImageTop` raise `TVPNotDrawableLayerType` when the layer
/// has no `MainImage` (`reference/cpp/core/visual/LayerIntf.cpp:2518-2522`,
/// `:2539-2543`); with an image they read the stored offsets back.
///
/// reference/cpp/core/visual/LayerIntf.cpp:2518-2522
/// reference/cpp/core/visual/LayerIntf.cpp:2539-2543
#[test]
fn image_offset_getters_require_an_image() {
    let _lock = vm_lock();
    let env = Env::with_image(60, 1500);
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setSize(10, 20); \
         l.loadImages('clickglyph.png'); \
         l.setImagePos(-50, -1480);",
    );
    assert_eq!(env.eval_int("l.imageLeft"), -50);
    assert_eq!(env.eval_int("l.imageTop"), -1480);

    env.run("l.hasImage = false;");
    for expression in ["l.imageLeft", "l.imageTop"] {
        let message = env.error(expression);
        assert!(
            message.contains("Not drawable layer type"),
            "{expression}: unexpected message: {message}"
        );
    }
}

/// `Layer.cached` composites the layer's subtree into its cache bitmap through
/// the frame's cache pass (`reference/cpp/core/visual/LayerIntf.cpp:4018-4026`,
/// `:7156-7226`): the first frame publishes the cache, an unchanged frame
/// reuses it, a changed child is recomposited into the same cache bitmap, and
/// turning the flag off releases the cache.
///
/// reference/cpp/core/visual/LayerIntf.cpp:4018-4026
/// reference/cpp/core/visual/LayerIntf.cpp:7156-7226
#[test]
fn cached_layers_composite_and_refresh_through_the_frame_pump() {
    let _lock = vm_lock();
    let env = Env::with_image(2, 2);
    env.run(
        "var w = new Window(); \
         var p = new Layer(w, null); p.setSize(4, 4); p.setImageSize(4, 4); p.name = 'p'; \
         p.fillRect(0, 0, 4, 4, 0xff112233); \
         var c = new Layer(w, p); c.setSize(2, 2); c.setImageSize(2, 2); c.name = 'c'; \
         c.visible = true; c.fillRect(0, 0, 2, 2, 0xff00ff00); \
         p.cached = true;",
    );
    let window = env.scene.windows.first().expect("window").id;
    let parent = named_layer(&env, "p");
    let main = parent.bitmap.expect("main image");
    let child_bitmap = named_layer(&env, "c").bitmap.expect("child image");
    assert!(parent.cached);
    assert_eq!(parent.cache_enabled_count, 1, "one cache consumer");

    // The frame's cache pass composites the layer's own image and its child.
    env.refresh_caches();
    let cache = named_layer(&env, "p")
        .cache_bitmap
        .expect("the cache bitmap");
    assert_ne!(cache, main, "a subtree with a visible child composes");
    assert_eq!(
        scene_pixel(&env, cache, 0, 0),
        0x00ff00,
        "the child is cached"
    );
    assert_eq!(
        scene_pixel(&env, cache, 3, 3),
        0x112233,
        "the layer image is cached"
    );
    assert_eq!(
        requested_bitmaps(&env, window),
        vec![cache],
        "the cache stands in for the layer and its descendants"
    );

    // An unchanged frame reuses the cache: the pass composites nothing.
    let revisions = (env.scene.structure_revision(), env.scene.content_revision());
    env.refresh_caches();
    assert_eq!(named_layer(&env, "p").cache_bitmap, Some(cache));
    assert_eq!(
        (env.scene.structure_revision(), env.scene.content_revision()),
        revisions,
        "an unchanged frame does not recomposite"
    );

    // A child pixel change is recomposited into the same cache bitmap.
    env.run("c.fillRect(0, 0, 2, 2, 0xff0000ff);");
    env.refresh_caches();
    assert_eq!(named_layer(&env, "p").cache_bitmap, Some(cache));
    assert_eq!(
        scene_pixel(&env, cache, 0, 0),
        0x0000ff,
        "the changed child reached the cache"
    );
    assert_eq!(scene_pixel(&env, cache, 3, 3), 0x112233);

    // Turning the flag off releases the cache and returns the layer to live
    // drawing with its child.
    env.run("p.cached = false;");
    let layer = named_layer(&env, "p");
    assert!(!layer.cached);
    assert_eq!(layer.cache_enabled_count, 0, "the consumer is released");
    assert!(
        layer.cache_bitmap.is_none(),
        "the last release frees the cache"
    );
    assert!(
        env.scene.bitmap(cache).is_none(),
        "the cache bitmap is gone from the scene"
    );
    let live = requested_bitmaps(&env, window);
    assert!(live.contains(&main), "the layer draws its own image again");
    assert!(live.contains(&child_bitmap), "and the child draws live");
    assert!(!live.contains(&cache), "the released cache draws nothing");
}

/// A cached layer composites through its cached children in the same pass:
/// the pass visits cached layers deepest first, so a parent's recomposite
/// already samples the `CacheBitmap` its child just completed instead of the
/// previous frame's state.
///
/// reference/cpp/core/visual/LayerIntf.cpp:7196 (the parent's cache loop
/// completes each child before it completes itself)
/// reference/cpp/core/visual/LayerIntf.cpp:7226
#[test]
fn nested_caches_composite_deepest_first() {
    let _lock = vm_lock();
    let mut env = Env::with_image(2, 2);
    env.run(
        "var w = new Window(); \
         var p = new Layer(w, null); p.setSize(4, 4); p.setImageSize(4, 4); p.name = 'p'; \
         p.fillRect(0, 0, 4, 4, 0xff112233); \
         var c = new Layer(w, p); c.setSize(4, 4); c.name = 'c'; c.visible = true; \
         var g = new Layer(w, c); g.setSize(2, 2); g.name = 'g'; g.visible = true; \
         g.fillRect(0, 0, 2, 2, 0xff00ff00); \
         p.cached = true; c.cached = true;",
    );
    env.refresh_caches();
    let parent_cache = named_layer(&env, "p")
        .cache_bitmap
        .expect("the parent cache bitmap");
    let child_cache = named_layer(&env, "c")
        .cache_bitmap
        .expect("the child cache bitmap");
    assert_ne!(parent_cache, child_cache);
    assert_eq!(
        scene_pixel(&env, parent_cache, 0, 0),
        0x00ff00,
        "the parent's cache shows the child"
    );
    assert_eq!(
        scene_pixel(&env, parent_cache, 3, 3),
        0x112233,
        "and the parent's own image"
    );

    // Force the pass to run again without invalidating either cache. A parent
    // that composited the child's live image would still record the live
    // source generation and rewrite its cache now that the child's cache is
    // current; a parent that already sampled the child cache recomposites
    // nothing.
    let generation = env
        .scene
        .bitmap(parent_cache)
        .expect("parent cache")
        .content_generation;
    env.scene.add_bitmap(1, 1, vec![0; 4]);
    env.refresh_caches();
    assert_eq!(named_layer(&env, "p").cache_bitmap, Some(parent_cache));
    assert_eq!(
        env.scene
            .bitmap(parent_cache)
            .expect("parent cache")
            .content_generation,
        generation,
        "the parent sampled the child cache in the first pass"
    );
}

/// A bitmap content write that bypasses the layer accessors still reaches a
/// cached subtree: the pass also keys on the scene's content revision, and
/// the compositor recomposites when a drawn image's generation changed.
///
/// reference/cpp/core/visual/LayerIntf.cpp:6305-6335
#[test]
fn a_content_only_bitmap_write_reaches_the_subtree_cache() {
    let _lock = vm_lock();
    let mut env = Env::with_image(2, 2);
    env.run(
        "var w = new Window(); \
         var p = new Layer(w, null); p.setSize(4, 4); p.setImageSize(4, 4); p.name = 'p'; \
         p.fillRect(0, 0, 4, 4, 0xff112233); \
         var c = new Layer(w, p); c.setSize(2, 2); c.setImageSize(2, 2); c.name = 'c'; \
         c.visible = true; c.fillRect(0, 0, 2, 2, 0xff00ff00); \
         p.cached = true;",
    );
    env.refresh_caches();
    let cache = named_layer(&env, "p")
        .cache_bitmap
        .expect("the cache bitmap");
    assert_eq!(scene_pixel(&env, cache, 0, 0), 0x00ff00);

    let child_bitmap = named_layer(&env, "c").bitmap.expect("child image");
    let structure = env.scene.structure_revision();
    {
        let bitmap = env.scene.bitmap_mut(child_bitmap).expect("child image");
        for pixel in bitmap.rgba.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[0, 0, 255, 255]);
        }
        bitmap.mark_dirty();
    }
    assert_eq!(
        env.scene.structure_revision(),
        structure,
        "a pixel write is not structural"
    );
    env.refresh_caches();
    assert_eq!(
        scene_pixel(&env, cache, 0, 0),
        0x0000ff,
        "the content-only write reached the cache"
    );
}
