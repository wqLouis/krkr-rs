//! Integration tests for the visual native classes (`Window`, `Layer`,
//! `Bitmap`, `Font`, `Timer`).
//!
//! The tests attach a caller-owned L2 [`Scene`] to a [`CompatHost`] and run
//! TJS against it. Because the host stores a raw pointer to the scene, the
//! scene is boxed and declared **after** the host so it outlives it (`Env`'s
//! fields drop in declaration order).

mod common;

use api::host::TouchPhase;
use common::vm_lock;
use compat::{CompatHost, EventAdapter};
use engine::Scene;
use engine::input::InputState;
use engine::timer::TimerState;
use platform::{InputEvent, PlatformEvent};
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host + attached scene, timer registry and input state.
struct Env {
    /// Dropped first: releases every retained script object before the scene,
    /// the timer registry, the input state and the engine go away.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
    /// Kept alive for the host's raw pointer; never read directly.
    _timer: Box<TimerState>,
    /// Kept alive for the host's raw pointer; never read directly.
    _input: Box<InputState>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut timer = Box::new(TimerState::new());
        let mut input = Box::new(InputState::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene, timer registry and input state outlive the
        // host (they are declared after `_host`, so they are dropped later),
        // matching the `attach_*` contracts.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
            host.services_mut()
                .attach_input(input.as_mut() as *mut InputState);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            scene,
            _timer: timer,
            _input: input,
        }
    }

    /// Feed one frame of platform events through the L3 adapter into the
    /// attached L2 input state (the same seam `VmHost` uses).
    fn feed_input(&self, events: &[PlatformEvent]) {
        let mut adapter = EventAdapter::new();
        let mut ctx = self._host.ctx().expect("bound host");
        adapter.begin_input_frame(&mut ctx);
        for event in events {
            adapter.apply_input_event(&mut ctx, event);
        }
        adapter.end_input_frame(&mut ctx);
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

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            TjsValue::Integer(value) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
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
         var base = new Layer(w, null); \
         var l = new Layer(w, base); \
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

    // The first layer on a window is its primary layer, which the reference
    // refuses to move (`reference/cpp/core/visual/LayerIntf.cpp:2206-2207`).
    let layer = env
        .scene
        .layers
        .iter()
        .find(|layer| layer.name == "sprite")
        .expect("the sprite layer");
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
fn layer_type_maps_every_native_blend_to_its_behaviour() {
    use api::blend::{BlendMode, DstBlend};
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window(); var l = new Layer(w, null);");
    // (native `lt*`, decoded behaviour, native it encodes back to)
    let cases: &[(i64, BlendMode, i64)] = &[
        (0, BlendMode::SourceOver, 2),  // ltBinder (container, no blend)
        (1, BlendMode::Replace, 1),     // ltOpaque
        (2, BlendMode::SourceOver, 2),  // ltAlpha
        (3, BlendMode::Additive, 3),    // ltAdditive
        (4, BlendMode::Subtractive, 4), // ltSubtractive
        (5, BlendMode::Multiply, 5),    // ltMultiplicative
        (6, BlendMode::SourceOver, 2),  // ltEffect (callback)
        (7, BlendMode::SourceOver, 2),  // ltFilter (callback)
        (8, BlendMode::DestinationDependent(DstBlend::ColorDodge), 8),
        (9, BlendMode::DestinationDependent(DstBlend::Darken), 9),
        (10, BlendMode::DestinationDependent(DstBlend::Lighten), 10),
        (11, BlendMode::Screen, 11),
        (12, BlendMode::AddAlpha, 12),
        (13, BlendMode::PsNormal, 13),
        (14, BlendMode::PsAdditive, 14),
        (15, BlendMode::PsSubtractive, 15),
        (16, BlendMode::PsMultiplicative, 16),
        (17, BlendMode::PsScreen, 17),
        (18, BlendMode::DestinationDependent(DstBlend::Overlay), 18),
        (19, BlendMode::DestinationDependent(DstBlend::HardLight), 19),
        (20, BlendMode::DestinationDependent(DstBlend::SoftLight), 20),
        (
            21,
            BlendMode::DestinationDependent(DstBlend::PsColorDodge),
            21,
        ),
        (
            22,
            BlendMode::DestinationDependent(DstBlend::ColorDodge5),
            22,
        ),
        (23, BlendMode::DestinationDependent(DstBlend::ColorBurn), 23),
        (24, BlendMode::DestinationDependent(DstBlend::PsLighten), 24),
        (25, BlendMode::DestinationDependent(DstBlend::PsDarken), 25),
        (
            26,
            BlendMode::DestinationDependent(DstBlend::Difference),
            26,
        ),
        (
            27,
            BlendMode::DestinationDependent(DstBlend::Difference5),
            27,
        ),
        (28, BlendMode::DestinationDependent(DstBlend::Exclusion), 28),
        (9999, BlendMode::SourceOver, 2), // unknown degrades safely
    ];
    for &(native, expected, canonical) in cases {
        env.run(&format!("l.type = {native};"));
        let stored = env.scene.layers.first().expect("layer").blend;
        assert_eq!(stored, expected, "native type {native}");
        assert_eq!(env.eval_int("l.type"), canonical, "native type {native}");
    }
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
    assert_eq!(env.eval_int("p === l"), 1);
    // And `l.window` points back at the window.
    assert_eq!(env.eval_int("l.window === w"), 1);
    assert_eq!(env.eval_int("p.isPrimary"), 1);
    // The reference `Layer`/`Window` member lists register no `id`
    // (`reference/cpp/core/visual/LayerIntf.cpp:8283`,
    // `reference/cpp/core/visual/WindowIntf.cpp:731`), so reading one raises.
    assert!(env.eval("l.id").is_err(), "Layer has no `id` member");
    assert!(env.eval("w.id").is_err(), "Window has no `id` member");

    let primary = env.scene.windows[0].primary_layer.expect("primary layer");
    assert_eq!(primary, env.scene.layers[0].id);
}

#[test]
fn primary_layer_without_a_layer_raises() {
    let _lock = vm_lock();
    let env = Env::new();

    // `Window.primaryLayer` raises `TVPWindowHasNoLayer` when the window has
    // no primary layer (`reference/cpp/core/visual/WindowIntf.cpp:1793-1794`).
    env.run("var w = new Window();");
    assert!(
        env.eval("w.primaryLayer").is_err(),
        "a fresh window has no primary layer"
    );

    // Once a layer is attached it becomes the primary layer and is returned.
    env.run("var l = new Layer(w, null);");
    assert_eq!(env.eval_int("w.primaryLayer === l"), 1);
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
         child.focusable = true; \
         child.visible = true; \
         w.focusedLayer = child; \
         var focused = w.focusedLayer;",
    );

    assert_eq!(env.eval_int("focused.name == 'child'"), 1);
    assert_eq!(env.eval_int("parent.children.count"), 1);
    assert_eq!(env.eval_int("parent.children[0].name == 'child'"), 1);
    assert_eq!(env.eval_int("child.parent.name == 'parent'"), 1);
    assert_eq!(env.eval_int("child.focused"), 1);

    // `Window.add` registers an object whose lifetime is tied to the window
    // (reference `tTJSNI_BaseWindow::ObjectVector`); it does **not** reparent
    // layers. `Window.remove` unregisters it. Neither touches the layer tree.
    env.run(
        "var extra = new Layer(w, null); extra.name = 'extra'; \
         w.add(extra); w.add(extra); w.remove(extra);",
    );
    assert_eq!(env.eval_int("extra.parent == null"), 1);
    assert_eq!(env.scene.layers.len(), 3);
}

#[test]
fn bitmap_pixels_and_size() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var b = new Bitmap(2, 2); \
         b.setPixel(0, 0, 0x00ff00); \
         var p = b.getPixel(0, 0); \
         b.setMaskPixel(1, 1, 128); \
         var m = b.getMaskPixel(1, 1); \
         var w0 = b.width; \
         var h0 = b.height; \
         b.setSize(4, 3);",
    );

    // `setPixel` writes the colour planes and holds the mask plane
    // (`reference/cpp/core/visual/BitmapIntf.cpp:60-66`); `getPixel` reports
    // the colour without the mask (`:51-58`) and `getMaskPixel` reports the
    // mask alone (`:67-75`).
    assert_eq!(env.eval_int("p == 0x00ff00"), 1);
    assert_eq!(env.eval_int("m"), 128);
    assert_eq!(env.eval_int("w0"), 2);
    assert_eq!(env.eval_int("h0"), 2);
    assert_eq!(env.eval_int("b.width"), 4);
    assert_eq!(env.eval_int("b.height"), 3);
    assert_eq!(env.eval_int("b.bufferPitch"), 16);

    let bitmap = env.scene.bitmaps.first().expect("one bitmap");
    assert_eq!((bitmap.width, bitmap.height), (4, 3));
    assert!(bitmap.script_owned);
    // The green pixel survived the resize (top-left region preserved); its
    // mask plane is still the one the fresh bitmap was allocated with.
    assert_eq!(&bitmap.rgba[0..4], &[0x00, 0xff, 0x00, 0x00]);
}

#[test]
fn bitmap_copy_from_copies_pixels() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var a = new Bitmap(2, 2); \
         a.setPixel(1, 0, 0xff0000); \
         var b = new Bitmap(1, 1); \
         b.copyFrom(a); \
         var p = b.getPixel(1, 0);",
    );
    // `getPixel` reports the colour planes without the mask
    // (`reference/cpp/core/visual/BitmapIntf.cpp:51-58`).
    assert_eq!(env.eval_int("p == 0xff0000"), 1);
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
         var height = f.height;",
    );

    assert_eq!(env.eval_int("face == 'Arial'"), 1);
    assert_eq!(env.eval_int("height"), 12);
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

/// `Window.close()` drops the window record; the layers live with their own
/// script objects.
///
/// The reference's `tTJSNI_Window::Close`
/// (`reference/cpp/core/visual/impl/WindowImpl.cpp:1469-1472`) only
/// closes the form, and the window's `Invalidate`
/// (`reference/cpp/core/visual/WindowIntf.cpp:175-241`) invalidates
/// only the `Window.add` registry (`:208-222`) — the primary-layer detach is
/// commented out (`:186-187`). A layer's native instance belongs to its own
/// script object, so closing the window leaves the layers alive until the
/// script drops them.
#[test]
fn window_close_removes_the_window_and_keeps_the_layers() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); \
         var b = new Layer(w, a); \
         w.close();",
    );

    assert!(env.scene.windows.is_empty());
    assert_eq!(
        env.scene.layers.len(),
        2,
        "close must not take the window's layers with it"
    );

    // The layers go with their own script objects.
    env.run("w = null; a = null; b = null;");
    env.run("System.doCompact();");
    env.engine.do_gc().expect("idle compact");
    assert!(
        env.scene.layers.is_empty(),
        "the collected layers must release their records"
    );
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
    assert_eq!(env.eval_real("t.interval"), 250.0);
    assert_eq!(env.eval_int("t.mode"), 2);
    assert_eq!(env.eval_int("t.capacity"), 1);
}

#[test]
fn missing_members_raise_instead_of_silently_stubbing() {
    let _lock = vm_lock();
    let env = Env::new();
    // A member the VM does not know at all must raise, never silently return
    // void (D11: no silent fallbacks).
    for expression in [
        "(function(){ var w = new Window(); return w.thisMemberDoesNotExist; })()",
        "(function(){ var b = new Bitmap(1, 1); return b.thisMemberDoesNotExist(); })()",
        "(function(){ var f = new Font('A', 12, 0); return f.thisMemberDoesNotExist(); })()",
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

#[test]
fn window_touch_getters_read_the_l2_input_state() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");

    env.feed_input(&[
        PlatformEvent::Input(InputEvent::Touch {
            id: 5,
            phase: TouchPhase::Started,
            x: 10.0,
            y: 20.0,
        }),
        PlatformEvent::Input(InputEvent::Touch {
            id: 5,
            phase: TouchPhase::Moved,
            x: 14.0,
            y: 26.0,
        }),
        PlatformEvent::Input(InputEvent::Touch {
            id: 7,
            phase: TouchPhase::Started,
            x: 30.0,
            y: 40.0,
        }),
    ]);

    assert_eq!(env.eval_int("w.touchPointCount"), 2);

    // `getTouchPoint` returns the reference dictionary.
    assert_eq!(env.eval_int("w.getTouchPoint(5).ID"), 5);
    assert_eq!(env.eval_real("w.getTouchPoint(5).x"), 14.0);
    assert_eq!(env.eval_real("w.getTouchPoint(5).y"), 26.0);
    assert_eq!(env.eval_real("w.getTouchPoint(5).startX"), 10.0);
    assert_eq!(env.eval_real("w.getTouchPoint(5).startY"), 20.0);
    // The argument may also be a 0-based index into the contacts by id.
    assert_eq!(env.eval_int("w.getTouchPoint(1).ID"), 7);
    // An unknown contact is a typed TJS error, never a silent stub.
    assert!(env.eval("w.getTouchPoint(99)").is_err());

    // Velocity is the delta since the previous update.
    assert_eq!(env.eval_real("w.getTouchVelocity(5).x"), 4.0);
    assert_eq!(env.eval_real("w.getTouchVelocity(5).y"), 6.0);
    assert!((env.eval_real("w.getTouchVelocity(5).speed") - 52.0_f64.sqrt()).abs() < 1e-6);
    // A contact that never moved has zero velocity, not null.
    assert_eq!(env.eval_real("w.getTouchVelocity(7).x"), 0.0);
    // An unknown contact yields TJS null.
    assert_eq!(env.eval_int("w.getTouchVelocity(99) === null"), 1);

    // Ending a contact removes it from the count and lookups.
    env.feed_input(&[PlatformEvent::Input(InputEvent::Touch {
        id: 5,
        phase: TouchPhase::Ended,
        x: 14.0,
        y: 26.0,
    })]);
    assert_eq!(env.eval_int("w.touchPointCount"), 1);
    assert!(env.eval("w.getTouchPoint(5)").is_err());
}

#[test]
fn layer_cursor_reads_the_pointer_touch_and_layer_offset() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, null); \
         l.setPos(10, 20); \
         var c = new Layer(w, l); \
         c.setPos(5, 5);",
    );

    env.feed_input(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 100.0,
        y: 200.0,
    })]);

    // A root layer is window-relative.
    assert_eq!(env.eval_int("l.cursorX"), 100);
    assert_eq!(env.eval_int("l.cursorY"), 200);
    // A child subtracts the parent rect offset (reference GetCursorX/Y).
    assert_eq!(env.eval_int("c.cursorX"), 95);
    assert_eq!(env.eval_int("c.cursorY"), 195);

    // With an active touch, the cursor follows the touch (lowest id wins).
    env.feed_input(&[PlatformEvent::Input(InputEvent::Touch {
        id: 3,
        phase: TouchPhase::Started,
        x: 40.0,
        y: 60.0,
    })]);
    assert_eq!(env.eval_int("l.cursorX"), 40);
    assert_eq!(env.eval_int("l.cursorY"), 60);
    assert_eq!(env.eval_int("c.cursorX"), 35);
    assert_eq!(env.eval_int("c.cursorY"), 55);
}

#[test]
fn draw_device_classes_are_attached_to_window() {
    let _lock = vm_lock();
    let env = Env::new();

    // The reference attaches both classes to the `Window` class object
    // (`ScriptMgnIntf.cpp`), so `new Window.X()` is the documented
    // construction.
    env.run(
        "var b = new Window.BasicDrawDevice(); \
         var p = new Window.PassThroughDrawDevice();",
    );

    // The reference has no `name` member (`BasicDrawDevice.cpp:898-913`
    // registers only `interface`, the `dt*` constants, `preferredDrawer` and
    // `drawer`), so a read raises.
    assert!(env.eval("b.name").is_err(), "BasicDrawDevice has no `name`");
    assert!(
        env.eval("p.name").is_err(),
        "PassThroughDrawDevice has no `name`"
    );
    // The drawer defaults match the reference constructors
    // (`BasicDrawDevice.h`: dtDrawDib; `PassThroughDrawDevice.cpp`: dtNone).
    assert_eq!(env.eval_int("b.drawer"), 1);
    assert_eq!(env.eval_int("b.preferredDrawer"), 1);
    assert_eq!(env.eval_int("p.drawer"), 0);
    assert_eq!(env.eval_int("p.preferredDrawer"), 0);

    // The `TVP_REGISTER_PTDD_ENUM` constants are class-qualified in the
    // reference docs (`Window.PassThroughDrawDevice.dtNone`).
    assert_eq!(env.eval_int("Window.BasicDrawDevice.dtNone"), 0);
    assert_eq!(env.eval_int("Window.BasicDrawDevice.dtDrawDib"), 1);
    assert_eq!(env.eval_int("Window.PassThroughDrawDevice.dtDBGDI"), 2);
    assert_eq!(env.eval_int("Window.PassThroughDrawDevice.dtDBDD"), 3);
    assert_eq!(env.eval_int("Window.PassThroughDrawDevice.dtDBD3D"), 4);
}

#[test]
fn draw_device_state_round_trips_and_interface_is_a_synthetic_pointer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var p = new Window.PassThroughDrawDevice(); \
         p.preferredDrawer = Window.PassThroughDrawDevice.dtDBD3D; \
         p.recreate(); \
         p.lockTouchSize(640, 480);",
    );

    // The preference is stored verbatim; `recreate` selects the software
    // drawer (`dtDrawDib`) exactly like the reference `EnsureDrawer`.
    assert_eq!(env.eval_int("p.preferredDrawer"), 4);
    assert_eq!(env.eval_int("p.drawer"), 1);

    // `lockTouchSize` requires both arguments.
    assert!(env.eval("p.lockTouchSize(640)").is_err());

    // Basic `recreate` is a compiled-out no-op in the reference.
    env.run("var b = new Window.BasicDrawDevice(); b.recreate();");
    assert_eq!(env.eval_int("b.drawer"), 1);

    // Reference (`PassThroughDrawDevice.cpp:532-544`): `interface` is
    // `reinterpret_cast<tjs_int64>(GetDevice())`, the native draw-device
    // pointer as an integer. This host has no draw-device object, so the
    // getter reports the stable native payload address instead.
    let interface = env.eval_int("p.interface");
    assert_ne!(
        interface, 0,
        "interface must be a non-zero synthetic pointer"
    );
    assert_eq!(env.eval_int("p.interface"), interface);
    // `drawer` is read-only.
    assert!(env.eval("b.drawer = 3").is_err());
}

#[test]
fn default_draw_device_creation_failure_raises_internal_error() {
    let _lock = vm_lock();
    let env = Env::new();

    // The window constructor builds a default `BasicDrawDevice` and throws
    // `TVPInternalError` (`Internal error`) when `CreateNew` fails
    // (`reference/cpp/core/visual/WindowIntf.cpp:150-156`). The port resolves
    // the registered `Window.BasicDrawDevice` class member, so replacing it
    // with a non-constructor forces the failure the reference reports.
    env.run("Window.BasicDrawDevice = 5;");
    let error = env
        .eval("new Window()")
        .expect_err("a Window whose default draw device cannot be created must raise");
    assert!(
        error.contains("Internal error"),
        "unexpected error: {error}"
    );
}
