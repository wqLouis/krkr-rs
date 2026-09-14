//! The `AffineLayer` paint idiom: a visible container layer keeps its image
//! on a hidden inner `_image` child (`system/AffineLayer.tjs`), and the
//! engine's `onPaint` dispatch copies that bitmap onto the visible outer
//! layer. These tests exercise both the native [`assignImages`] contract and
//! the `update()` → `timer_poll` → script `onPaint` composite that the
//! render loop drives.
//!
//! Headless: the natives only mutate the shared [`Scene`]; the test inspects
//! it under a read lock, the same contract the Bevy renderer uses.

use std::sync::{Arc, Mutex, RwLock};

use engine::Storage;
use tempfile::TempDir;
use tjs2_sys::Tjs2Engine;
use tvp_visual::scene::{LayerState, Scene};

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
            .exec_script(script, "affine_layer_paint")
            .expect("script must run");
    }

    fn scene(&self) -> std::sync::RwLockReadGuard<'_, Scene> {
        self.scene.read().expect("scene lock poisoned")
    }

    /// Write a webp into the mounted game dir so storage-backed loads
    /// resolve (the natives probe `.webp`).
    fn write_webp(&self, name: &str, rgba: &[u8], w: u32, h: u32) {
        let mut bytes = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
            .encode(rgba, w, h, image::ExtendedColorType::Rgba8)
            .expect("webp encode");
        std::fs::write(self._dir.path().join(name), bytes).expect("write fixture");
    }
}

/// A layer in the scene by id.
fn layer(scene: &Scene, id: u32) -> &LayerState {
    scene.layer(id).expect("layer exists")
}

/// `assignImages(layerObj)` shares the source's bitmap and copies its image
/// placement/size onto the target — the reference `AssignImages`
/// (`LayerIntf.cpp:2394`).
#[test]
fn assign_images_layer_copies_bitmap_and_image_rect() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var parent = new Layer(w, null); parent.visible = true; \
         var image = new Layer(w, parent); \
         var b = new Bitmap(20, 10); \
         image.setBitmap(b.id); \
         image.setSize(20, 10); \
         image.imageLeft = -4; image.imageTop = -6; \
         parent.assignImages(image);",
    );

    let scene = env.scene();
    let parent = layer(&scene, 0);
    let child = layer(&scene, 1);
    assert!(
        parent.bitmap.is_some() && parent.bitmap != child.bitmap,
        "parent gets its own copy of the child bitmap (not a shared id)"
    );
    let pb = parent.bitmap.and_then(|id| scene.bitmap(id)).unwrap();
    let cb = child.bitmap.and_then(|id| scene.bitmap(id)).unwrap();
    assert_eq!((pb.width, pb.height), (cb.width, cb.height));
    assert_eq!(pb.rgba, cb.rgba, "copied pixels match");
    assert_eq!(
        (parent.image_left, parent.image_top),
        (-4, -6),
        "image placement copied"
    );
    assert_eq!(
        (parent.image_width, parent.image_height),
        (20, 10),
        "image size copied (setSize grows imageWidth/Height)"
    );
    assert_eq!(
        (parent.rect.w, parent.rect.h),
        (20, 10),
        "layer size copied"
    );
    assert!(parent.visible, "outer layer stays visible");
    assert!(!child.visible, "inner _image child stays hidden");
}

/// `assignImages(bitmapObj)` attaches the bitmap and sizes the image to it,
/// even when the bitmap id collides with a live layer id (the two id spaces
/// are independent).
#[test]
fn assign_images_bitmap_attaches_bitmap() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var parent = new Layer(w, null); \
         var b = new Bitmap(32, 16); \
         parent.assignImages(b);",
    );
    let scene = env.scene();
    let parent = layer(&scene, 0);
    // Bitmap id 0 collides with the parent layer id 0; the `hasImage`
    // discriminator must still pick the bitmap.
    assert_eq!(parent.bitmap, Some(0), "bitmap attached by object");
    assert_eq!((parent.image_width, parent.image_height), (32, 16));
    assert_eq!((parent.rect.w, parent.rect.h), (32, 16));
}

/// `assignImages(string)` keeps the storage-load path working.
#[test]
fn assign_images_string_loads_from_storage() {
    let env = Env::new();
    let (w, h) = (7u32, 5u32);
    let rgba: Vec<u8> = (0..w * h).flat_map(|_| [10u8, 200, 30, 255]).collect();
    env.write_webp("frm_assign.webp", &rgba, w, h);
    env.run(
        "var w = new Window(); \
         var parent = new Layer(w, null); \
         parent.assignImages('frm_assign');",
    );
    let scene = env.scene();
    let parent = layer(&scene, 0);
    let bmp_id = parent.bitmap.expect("assignImages(name) attaches a bitmap");
    let bmp = scene.bitmap(bmp_id).expect("bitmap registered");
    assert_eq!((bmp.width, bmp.height), (w, h));
    assert_eq!((parent.image_width, parent.image_height), (w, h));
}

/// A minimal `AffineLayer`: the constructor creates the hidden `_image`
/// child, and `onPaint` mirrors `system/AffineLayer.tjs:133-145`. `global.`
/// is required to name the built-in `Layer` class inside a subclass method
/// (plain `Layer` would parse as a super-constructor reference).
const AFFINE_CLASS: &str = "class TestAffine extends Layer { \
        var _image; \
        function TestAffine(win, par) { \
            super.Layer(win, par); \
            _image = new global.Layer(win, this); \
        } \
        function onPaint() { \
            super.onPaint(...); \
            super.assignImages(_image); \
            super.setSize(_image.width, _image.height); \
            super.setImagePos(_image.imageLeft, _image.imageTop); \
        } \
    }";

/// Calling the script `onPaint()` directly runs the composite (this is what
/// the game's `ADVScreen` does, and it must not throw on `super.onPaint`).
#[test]
fn on_paint_direct_call_composites_hidden_image() {
    let env = Env::new();
    env.run(&format!(
        "var w = new Window(); \
         {AFFINE_CLASS} \
         var p = new TestAffine(w, null); \
         p.visible = true; \
         var img = p._image; \
         var b = new Bitmap(24, 12); \
         img.setBitmap(b.id); \
         img.setSize(24, 12); \
         img.imageLeft = -2; img.imageTop = -3; \
         p.onPaint();",
    ));
    let scene = env.scene();
    let p = layer(&scene, 0);
    let img = layer(&scene, 1);
    assert!(
        p.bitmap.is_some() && p.bitmap != img.bitmap,
        "visible outer carries its own copy of the child bitmap"
    );
    assert_eq!((p.image_left, p.image_top), (-2, -3));
    assert_eq!((p.rect.w, p.rect.h), (24, 12));
    assert!(!img.visible, "_image child stays hidden (no double draw)");
}

/// The engine path: `update()` marks the layer, `timer_poll` dispatches the
/// script `onPaint`, and the visible parent ends up with the child bitmap.
#[test]
fn update_then_paint_poll_dispatches_on_paint() {
    let env = Env::new();
    env.run(&format!(
        "var w = new Window(); \
         {AFFINE_CLASS} \
         var p = new TestAffine(w, null); \
         p.visible = true; \
         var img = p._image; \
         var b = new Bitmap(40, 20); \
         img.setBitmap(b.id); \
         img.setSize(40, 20); \
         img.imageLeft = -5; img.imageTop = -7;",
    ));
    // Nothing composited yet: the inner child holds the bitmap and the outer
    // (visible) layer is still empty.
    {
        let scene = env.scene();
        let p = layer(&scene, 0);
        assert!(p.bitmap.is_none(), "composite has not run yet");
        assert!(p.visible);
    }

    // The script's `calcAffine` calls `update()`, which requests `onPaint`.
    env.run("p.update();");
    {
        let scene = env.scene();
        assert!(layer(&scene, 0).pending_paint, "update() requests a paint");
    }
    tvp_visual::timer_poll(&env.engine, 0);

    let scene = env.scene();
    let p = layer(&scene, 0);
    let img = layer(&scene, 1);
    assert!(
        p.bitmap.is_some() && p.bitmap != img.bitmap,
        "paint_poll gave the outer its own copy of the child bitmap"
    );
    assert_eq!((p.image_left, p.image_top), (-5, -7));
    assert_eq!((p.image_width, p.image_height), (40, 20));
    assert_eq!((p.rect.w, p.rect.h), (40, 20));
    assert!(
        !p.pending_paint,
        "the pending flag is consumed by the dispatch"
    );
    assert!(!img.visible, "inner child stays hidden");
}
