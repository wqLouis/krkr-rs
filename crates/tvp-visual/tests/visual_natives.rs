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
        assert_eq!(
            layer.rect,
            tvp_visual::scene::Rect {
                x: 0,
                y: 0,
                w: 1280,
                h: 720
            },
            "fillRect sets the layer rect"
        );
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

    // Enabling reschedules "now" (0); the first poll at now_ms = 0 is due.
    tvp_visual::timer_poll(&env.engine, 0);
    assert_eq!(env.eval_int("fired"), 1);
    assert_eq!(env.eval_int("t.count"), 1, "count tracks fires");

    // Next fire is 0 + interval(50) = 50; 49 is not due.
    tvp_visual::timer_poll(&env.engine, 49);
    assert_eq!(env.eval_int("fired"), 1);

    // At 50 the timer fires again and reschedules to 100.
    tvp_visual::timer_poll(&env.engine, 50);
    assert_eq!(env.eval_int("fired"), 2);
    assert_eq!(env.eval_int("t.count"), 2);

    // Disabled timers never fire, no matter how much time passes.
    env.run("t.enabled = false;");
    tvp_visual::timer_poll(&env.engine, 1_000_000);
    assert_eq!(env.eval_int("fired"), 2);
}
