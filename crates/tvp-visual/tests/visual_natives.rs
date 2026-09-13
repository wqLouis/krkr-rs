//! End-to-end integration test for the visual native classes, mirroring the
//! real game's init path (`system/window.tjs` + `begin.tjs` + the Logo
//! scene setup): a window, a primary layer, a fill, the common sprite
//! properties, a storage-backed `loadImages`, and a `Timer` driven by
//! [`tvp_visual::timer_poll`] exactly like the render crate's update loop
//! does (WAVE3.md, SA-5).
//!
//! Headless: no window/GPU — the natives only mutate the shared
//! [`tvp_visual::scene::Scene`], which the test inspects under a read lock,
//! the same contract the Bevy renderer uses.

use std::sync::{Arc, Mutex, RwLock};

use engine::Storage;
use tempfile::TempDir;
use tjs2_sys::Tjs2Engine;
use tvp_visual::scene::Scene;

/// Test harness: fresh engine + scene + a temp game dir mounted as storage,
/// with all visual natives registered (mirrors `render::game_startup`'s
/// `register_natives` for the visual subset).
struct Env {
    /// The script engine, in an `Arc` so its address is stable: the timer
    /// natives call back into the engine from native callbacks via the
    /// pointer `register_visual` records (see `natives/mod.rs`).
    engine: Arc<Tjs2Engine>,
    scene: Arc<RwLock<Scene>>,
    /// Keeps the Arc registered into the crate-global storage slot alive;
    /// reads go through the mounted dir on disk.
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
            .exec_script(script, "visual_natives")
            .expect("script must run");
    }

    fn eval_int(&self, expr: &str) -> i64 {
        match self.engine.eval(expr, "visual_natives") {
            Ok(tjs2_sys::TjsValue::Integer(i)) => i,
            other => panic!("eval {expr:?} -> {other:?}"),
        }
    }

    fn scene(&self) -> std::sync::RwLockReadGuard<'_, Scene> {
        self.scene.read().expect("scene lock poisoned")
    }

    /// Write a webp into the mounted game dir. The game ships `.webp`
    /// images; the natives probe extensions, so `loadImages("frm_test")`
    /// resolves to `frm_test.webp` exactly like `Bitmap("FRM_0501b")`
    /// finds `FRM_0501b.webp` in the reference.
    fn write_webp(&self, name: &str, rgba: &[u8], w: u32, h: u32) {
        let mut bytes = Vec::new();
        image::codecs::webp::WebPEncoder::new_lossless(&mut bytes)
            .encode(rgba, w, h, image::ExtendedColorType::Rgba8)
            .expect("webp encode");
        std::fs::write(self._dir.path().join(name), bytes).expect("write fixture");
    }
}

/// The real game's init pattern (window + primary layer + fill + sprite
/// properties + storage image), driven exactly as `startup.tjs` does it,
/// and the resulting [`Scene`] state.
#[test]
fn game_init_pattern_populates_scene() {
    let env = Env::new();
    let (img_w, img_h) = (64u32, 48u32);
    let rgba: Vec<u8> = (0..img_w * img_h)
        .flat_map(|_| [255u8, 0, 0, 255])
        .collect();
    env.write_webp("frm_test.webp", &rgba, img_w, img_h);

    env.run(
        "var w = new Window(); \
         w.setInnerSize(1280, 720); \
         w.bringToFront(); \
         var l = new Layer(w, null); \
         l.fillRect(0, 0, 1280, 720, 0xffffffff); \
         l.absolute = 1; \
         l.hitThreshold = 256; \
         l.visible = true; \
         l.opacity = 255; \
         l.loadImages('frm_test'); \
         var p = w.primaryLayer;",
    );

    {
        let scene = env.scene();
        assert_eq!(scene.windows.len(), 1);
        let win = &scene.windows[0];
        assert_eq!(
            win.inner_size,
            (1280, 720),
            "setInnerSize must size the window"
        );

        assert_eq!(scene.layers.len(), 1);
        let layer = &scene.layers[0];
        assert_eq!(layer.window, win.id);
        // `fillRect` set the rect to 1280×720, but the following `loadImages`
        // replaces the main image and resizes the layer to the 64×48 bitmap
        // (reference `InternalSetImageSize`), which is exactly what the game's
        // sprite layers rely on.
        assert_eq!(
            layer.rect,
            tvp_visual::scene::Rect {
                x: 0,
                y: 0,
                w: img_w,
                h: img_h
            },
            "loadImages resizes the layer to the loaded image"
        );
        assert_eq!(
            (layer.image_width, layer.image_height),
            (img_w, img_h),
            "image size tracks the loaded bitmap"
        );
        assert_eq!((layer.image_left, layer.image_top), (0, 0));
        // 0xffffffff -> straight-alpha RGBA white.
        assert_eq!(layer.fill_color, Some([255, 255, 255, 255]));
        assert_eq!(layer.z_order, 1, "absolute = 1");
        assert_eq!(layer.hit_threshold, 256);
        assert!(layer.visible, "visible = true");
        assert!(
            (layer.opacity - 1.0).abs() < 1e-6,
            "opacity = 255 -> 1.0 (scene stores 0..1)"
        );

        // loadImages attached the fixture bitmap (extension probing found
        // frm_test.webp) and registered it in the scene.
        let bmp_id = layer.bitmap.expect("loadImages must attach a bitmap");
        let bmp = scene.bitmap(bmp_id).expect("attached bitmap registered");
        assert_eq!((bmp.width, bmp.height), (img_w, img_h));
        assert_eq!(bmp.name.as_deref(), Some("frm_test.webp"));
        assert_eq!(bmp.rgba.len(), (img_w * img_h * 4) as usize);

        // The first layer of a window is its primary layer; `w.primaryLayer`
        // reads its scene id back (object returns are a later milestone).
        assert_eq!(win.primary_layer, Some(layer.id));
        assert!(layer.is_primary);
    };

    // Script reads after the scene guard is dropped (getters take the read
    // lock; keep the read/write pattern explicit). primaryLayer now returns
    // the primary Layer's TJS object (retained), so `p` is an object.
    assert!(matches!(
        env.engine.eval("p", "visual_natives"),
        Ok(tjs2_sys::TjsValue::Object)
    ));
    assert!(matches!(
        env.engine.eval("w.primaryLayer", "visual_natives"),
        Ok(tjs2_sys::TjsValue::Object)
    ));
    // imageWidth/imageHeight report the attached bitmap's size.
    assert_eq!(env.eval_int("l.imageWidth"), i64::from(img_w));
    assert_eq!(env.eval_int("l.imageHeight"), i64::from(img_h));
}

/// `Timer` fires its retained callback through `timer_poll` — the render
/// crate calls it every frame with a monotonic millisecond clock, and the
/// callback runs synchronously on the VM thread (WAVE3 SA-5).
#[test]
fn timer_fires_callback_via_poll() {
    let env = Env::new();
    env.run(
        "var fired = 0; \
         var t = new Timer(function() { fired++; }, 'onSceneChange'); \
         t.interval = 50; \
         t.enabled = true;",
    );
    assert_eq!(
        env.eval_int("fired"),
        0,
        "nothing fires before the first poll"
    );

    // The reference reschedules on enable: fire at *now + interval*, so
    // with the last polled clock 0 the first fire is at 50, not 0.
    tvp_visual::timer_poll(&env.engine, 0);
    assert_eq!(env.eval_int("fired"), 0, "not due until now+interval(50)");

    // 49 is not due; at 50 the timer fires (and reschedules to 100).
    tvp_visual::timer_poll(&env.engine, 49);
    assert_eq!(env.eval_int("fired"), 0);
    tvp_visual::timer_poll(&env.engine, 50);
    assert_eq!(env.eval_int("fired"), 1);
    assert_eq!(env.eval_int("t.count"), 1, "count tracks fires");

    // Next fire is 50 + interval(50) = 100; 99 is not due.
    tvp_visual::timer_poll(&env.engine, 99);
    assert_eq!(env.eval_int("fired"), 1);

    // At 100 the timer fires again and reschedules to 150.
    tvp_visual::timer_poll(&env.engine, 100);
    assert_eq!(env.eval_int("fired"), 2);
    assert_eq!(env.eval_int("t.count"), 2);

    // Disabled timers never fire, no matter how much time passes.
    env.run("t.enabled = false;");
    tvp_visual::timer_poll(&env.engine, 1_000_000);
    assert_eq!(env.eval_int("fired"), 2);
}

/// Kirikiroid2 compatibility: `OnceCall(fn, ms)` fires the callback exactly
/// once after `ms` milliseconds, and `OnceCallCancel(fn)` cancels it by
/// function identity. This is what the game's Logo constructor calls
/// (`system/Title.tjs`: `OnceCall(step01, 1000)`); without it the logo
/// keyframe chain never starts.
#[test]
fn oncecall_fires_once_and_cancels() {
    let env = Env::new();
    env.run(
        "var fired = 0; \
         function step() { fired++; } \
         var t = OnceCall(step, 100);",
    );
    assert_eq!(env.eval_int("fired"), 0, "nothing before the poll");

    // Before the deadline: nothing.
    tvp_visual::timer_poll(&env.engine, 50);
    assert_eq!(env.eval_int("fired"), 0);

    // At/after the deadline the callback fires exactly once, and the timer
    // disables itself (one-shot): further polls do not re-fire.
    tvp_visual::timer_poll(&env.engine, 100);
    assert_eq!(env.eval_int("fired"), 1);
    tvp_visual::timer_poll(&env.engine, 10_000);
    assert_eq!(env.eval_int("fired"), 1, "one-shot: never fires again");

    // OnceCallCancel stops a pending callback by function identity.
    env.run(
        "var fired2 = 0; \
         function step2() { fired2++; } \
         OnceCall(step2, 100); \
         OnceCallCancel(step2);",
    );
    tvp_visual::timer_poll(&env.engine, 200);
    assert_eq!(env.eval_int("fired2"), 0, "cancelled callback never fires");
}

/// The in-game Config screen's `TBUTTON` toggles (`ConfigWindow.tjs`
/// `createButton` → `ToggleOnBaseButton.create` in `SelectItem.tjs`) build a
/// child `_check` layer with `copyFromBitmapToMainImage(file)` followed by
/// `setSize(sheetW \ nPattern, sheetH)`. The reference sizes the main image
/// to the bitmap *inside* `copyFromBitmapToMainImage`
/// (`LayerIntf.cpp:2432` `AssignMainImageWithUpdate`), so the layer rect clips
/// one 100×40 cell and `setImagePos(-100, 0)` selects pattern 1.
///
/// Regression: without that image-size assignment `ImageWidth` stayed 0,
/// `setSize` grew it to the cell width, and the renderer scaled the whole
/// 400×40 sheet into a single cell.
#[test]
fn tbutton_sprite_sheet_cell_setup() {
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var parent = new Layer(w, null); parent.visible = true; \
         var check = new Layer(w, parent); \
         var sheet = new Bitmap(400, 40); \
         check.copyFromBitmapToMainImage(sheet); \
         check.setSize(check.imageWidth \\ 4, check.imageHeight); \
         check.setPos(0, 0); \
         check.setImagePos(-(check.width * 1), 0);",
    );

    let scene = env.scene();
    let check = &scene.layers[1];
    assert_eq!(
        (check.image_width, check.image_height),
        (400, 40),
        "copyFromBitmapToMainImage sizes the image to the full sheet"
    );
    assert_eq!(
        (check.rect.w, check.rect.h),
        (100, 40),
        "setSize clips the layer to one cell"
    );
    assert_eq!(
        (check.image_left, check.image_top),
        (-100, 0),
        "setButton(1) pans the sheet to the second cell"
    );
}
