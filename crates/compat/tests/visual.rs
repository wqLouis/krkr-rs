//! Integration tests for the visual native classes (`Window`, `Layer`,
//! `Bitmap`, `Font`, `Timer`).
//!
//! The tests attach a caller-owned L2 [`Scene`] to a [`CompatHost`] and run
//! TJS against it. Because the host stores a raw pointer to the scene, the
//! scene is boxed and declared **after** the host so it outlives it (`Env`'s
//! fields drop in declaration order).

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host + attached scene.
struct Env {
    /// Dropped first: releases every retained script object before the scene
    /// and the engine go away.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene outlives the host (it is declared after
        // `_host`, so it is dropped later), matching `attach_scene`'s
        // contract.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            scene,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "visual-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "visual-test")
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }
}

#[test]
fn window_and_layer_round_trip_through_the_scene() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         w.setPos(5, 6); \
         w.caption = 'hello'; \
         var l = new Layer(w, null); \
         l.setPos(10, 20); \
         l.setSize(100, 50); \
         l.visible = true; \
         l.opacity = 128; \
         l.name = 'sprite'; \
         l.absolute = 7;",
    );

    let window = env.scene.windows.first().expect("one window");
    assert_eq!(window.inner_size, (320, 240));
    assert_eq!(window.title, "hello");
    assert_eq!(env.eval_int("w.left"), 5);
    assert_eq!(env.eval_int("w.top"), 6);

    let layer = env.scene.layers.first().expect("one layer");
    assert_eq!((layer.rect.x, layer.rect.y), (10, 20));
    assert_eq!((layer.rect.w, layer.rect.h), (100, 50));
    assert!(layer.visible);
    assert_eq!(layer.z_order, 7);
    assert_eq!(layer.name, "sprite");
    assert!((layer.opacity - 128.0 / 255.0).abs() < 1e-6);
    assert_eq!(env.eval_int("l.opacity"), 128);
    assert_eq!(env.eval_int("l.width"), 100);
    assert_eq!(env.eval_int("l.height"), 50);
    assert_eq!(env.eval_int("l.absolute"), 7);
    assert_eq!(env.eval_int("l.name == 'sprite'"), 1);
}

#[test]
fn primary_layer_is_returned_as_the_real_object() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.name = 'primary'; \
         var p = w.primaryLayer;",
    );

    // `primaryLayer` is the *same* object as `l`, not a fresh wrapper.
    assert_eq!(env.eval_int("p.name == 'primary'"), 1);
    assert_eq!(env.eval_int("p.id == l.id"), 1);
    // And `l.window` points back at the window.
    assert_eq!(env.eval_int("l.window.id == w.id"), 1);
    assert_eq!(env.eval_int("p.isPrimary"), 1);

    let primary = env.scene.windows[0].primary_layer.expect("primary layer");
    assert_eq!(primary, env.scene.layers[0].id);
}

#[test]
fn focus_and_children_round_trip() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         var parent = new Layer(w, null); \
         parent.name = 'parent'; \
         var child = new Layer(w, parent); \
         child.name = 'child'; \
         w.focusedLayer = child; \
         var focused = w.focusedLayer;",
    );

    assert_eq!(env.eval_int("focused.name == 'child'"), 1);
    assert_eq!(env.eval_int("parent.children.count"), 1);
    assert_eq!(env.eval_int("parent.children[0].name == 'child'"), 1);
    assert_eq!(env.eval_int("child.parent.name == 'parent'"), 1);
    assert_eq!(env.eval_int("child.focused"), 1);

    // `Window.add` reparents a root layer under the primary layer.
    env.run("var extra = new Layer(w, null); extra.name = 'extra'; w.add(extra);");
    assert_eq!(env.eval_int("extra.parent.name == 'parent'"), 1);

    // `Window.remove` parts it: the L2 `Part()` detaches it and removes the
    // layer record (the script object remains but is inert).
    env.run("var extraId = extra.id; w.remove(extra);");
    let extra_id = env.eval_int("extraId") as u32;
    assert!(env.scene.layer(extra_id).is_none());
    assert_eq!(env.scene.layers.len(), 2);
}

#[test]
fn bitmap_pixels_and_size() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var b = new Bitmap(2, 2); \
         b.setPixel(0, 0, 0xff00ff00); \
         var p = b.getPixel(0, 0); \
         b.setMaskPixel(1, 1, 128); \
         var m = b.getMaskPixel(1, 1); \
         var w0 = b.width; \
         var h0 = b.height; \
         b.setSize(4, 3);",
    );

    // 0xff00ff00 = opaque green.
    assert_eq!(env.eval_int("p == 0xff00ff00"), 1);
    assert_eq!(env.eval_int("m"), 128);
    assert_eq!(env.eval_int("w0"), 2);
    assert_eq!(env.eval_int("h0"), 2);
    assert_eq!(env.eval_int("b.width"), 4);
    assert_eq!(env.eval_int("b.height"), 3);
    assert_eq!(env.eval_int("b.bufferPitch"), 16);

    let bitmap = env.scene.bitmaps.first().expect("one bitmap");
    assert_eq!((bitmap.width, bitmap.height), (4, 3));
    assert!(bitmap.script_owned);
    // The green pixel survived the resize (top-left region preserved).
    assert_eq!(&bitmap.rgba[0..4], &[0x00, 0xff, 0x00, 0xff]);
}

#[test]
fn bitmap_copy_from_copies_pixels() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var a = new Bitmap(2, 2); \
         a.setPixel(1, 0, 0xffff0000); \
         var b = new Bitmap(1, 1); \
         b.copyFrom(a); \
         var p = b.getPixel(1, 0);",
    );
    assert_eq!(env.eval_int("p == 0xffff0000"), 1);
    assert_eq!(env.eval_int("b.width"), 2);
    assert_eq!(env.eval_int("b.height"), 2);
}

#[test]
fn font_properties_round_trip() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var f = new Font('Arial', 12, 0xffff0000); \
         f.bold = true; \
         f.italic = 1; \
         var face = f.face; \
         var height = f.height; \
         var color = f.color;",
    );

    assert_eq!(env.eval_int("face == 'Arial'"), 1);
    assert_eq!(env.eval_int("height"), 12);
    assert_eq!(env.eval_int("color == 0xffff0000"), 1);
    assert_eq!(env.eval_int("f.bold"), 1);
    assert_eq!(env.eval_int("f.italic"), 1);

    let font = env.scene.fonts.first().expect("one font");
    assert_eq!(font.face, "Arial");
    assert_eq!(font.height, 12);
    assert!(font.bold && font.italic);
}

#[test]
fn layer_font_wrapper_is_cached_and_bound() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var f1 = l.font; \
         f1.face = 'BoundFace'; \
         f1.height = 20; \
         var f2 = l.font; \
         var bound = (f1 == f2); \
         var face = f2.face;",
    );

    assert_eq!(env.eval_int("bound"), 1);
    assert_eq!(env.eval_int("face == 'BoundFace'"), 1);
    let font = env.scene.fonts.first().expect("layer font state");
    assert_eq!(font.face, "BoundFace");
    assert_eq!(font.height, 20);
}

#[test]
fn layer_image_allocation_and_province_pixels() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(16, 8); \
         var has = l.hasImage; \
         var iw = l.imageWidth; \
         var ih = l.imageHeight; \
         l.setProvincePixel(3, 2, 42); \
         var province = l.getProvincePixel(3, 2);",
    );

    assert_eq!(env.eval_int("has"), 1);
    assert_eq!(env.eval_int("iw"), 16);
    assert_eq!(env.eval_int("ih"), 8);
    assert_eq!(env.eval_int("province"), 42);

    let layer = env.scene.layers.first().expect("one layer");
    assert_eq!(layer.province_width, 16);
    assert_eq!(layer.province_height, 8);
    assert_eq!(layer.province.as_deref().unwrap()[2 * 16 + 3], 42);
}

#[test]
fn window_close_removes_the_window_and_layers() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); \
         var b = new Layer(w, a); \
         w.close();",
    );

    assert!(env.scene.windows.is_empty());
    assert!(env.scene.layers.is_empty());
}

#[test]
fn timer_properties_round_trip() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var ticks = 0; \
         var t = new Timer(function() { ticks++; }, ''); \
         var enabled0 = t.enabled; \
         t.interval = 250; \
         t.mode = 2; \
         t.capacity = 1;",
    );

    assert_eq!(env.eval_int("enabled0"), 0);
    assert_eq!(env.eval_int("t.interval"), 250);
    assert_eq!(env.eval_int("t.mode"), 2);
    assert_eq!(env.eval_int("t.capacity"), 1);
    assert_eq!(env.eval_int("t.count"), 0);
}

#[test]
fn missing_members_raise_instead_of_silently_stubbing() {
    let _lock = vm_lock();
    let env = Env::new();
    // Deferred members (image decode/encode, text metrics) are not registered.
    for expression in [
        "(function(){ var w = new Window(); return w.drawDevice; })()",
        "(function(){ var b = new Bitmap(1, 1); return b.save('x.png'); })()",
        "(function(){ var f = new Font('A', 12, 0); return f.getTextWidth('x'); })()",
    ] {
        let message = env
            .eval(expression)
            .expect_err(&format!("{expression:?} should raise"));
        assert!(
            message.contains("does not exist"),
            "{expression:?} should report an absent member, got: {message}"
        );
    }
}
