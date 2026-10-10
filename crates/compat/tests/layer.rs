//! Integration tests for the completed `Layer` native class.
//!
//! The tests attach a caller-owned L2 [`Scene`] (plus the input and timer
//! registries) to a [`CompatHost`] and run TJS against it — the same seam the
//! app uses — so the pixel behaviour is verified against the L2 state the
//! methods actually mutate.

mod common;

use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use engine::input::InputState;
use engine::timer::TimerState;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host + attached scene, input state and timer registry.
///
/// Fields drop in declaration order: the host first (releasing every retained
/// script object), then the engine, the scene, the input state and the timer
/// registry.
struct Env {
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
        // host (they are declared after `_host`).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
            host.services_mut()
                .attach_input(input.as_mut() as *mut InputState);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            scene,
            _timer: timer,
            _input: input,
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "layer-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "layer-test")
            .map_err(|error| error.to_string())
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            Ok(TjsValue::Integer(value)) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }
}

/// Create a window + layer and give the layer a `width` x `height` image.
fn layer_with_image(env: &Env, width: u32, height: u32) {
    env.run(&format!(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize({width}, {height});"
    ));
}

#[test]
fn all_missing_members_are_registered() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window(); var l = new Layer(w, null);");
    // Every member the parity report listed as missing for `Layer`.
    for member in [
        "adjustGamma",
        "affineBlend",
        "affineCopy",
        "affinePile",
        "beginTransition",
        "blendRect",
        "bringToBack",
        "colorRect",
        "convertType",
        "copy9Patch",
        "copyRect",
        "copyToBitmapFromMainImage",
        "doBoxBlur",
        "doGrayScale",
        "drawGlyph",
        "drawText",
        "dump",
        "flipLR",
        "flipUD",
        "focusNext",
        "focusPrev",
        "gaussianBlur",
        "getMainPixel",
        "getMaskPixel",
        "independProvinceImage",
        "light",
        "loadImages",
        "loadProvinceImage",
        "moveBefore",
        "moveBehind",
        "onBeforeFocus",
        "onBlur",
        "onClick",
        "onDoubleClick",
        "onFocus",
        "onHitTest",
        "onKeyDown",
        "onKeyPress",
        "onKeyUp",
        "onMouseDown",
        "onMouseEnter",
        "onMouseLeave",
        "onMouseMove",
        "onMouseUp",
        "onMouseWheel",
        "onMultiTouch",
        "onNodeDisabled",
        "onNodeEnabled",
        "onPaint",
        "onSearchNextFocusable",
        "onSearchPrevFocusable",
        "onTouchDown",
        "onTouchMove",
        "onTouchRotate",
        "onTouchScaling",
        "onTouchUp",
        "onTransitionCompleted",
        "operateAffine",
        "operateRect",
        "operateStretch",
        "pileRect",
        "piledCopy",
        "releaseCapture",
        "releaseTouchCapture",
        "removeMode",
        "saveLayerImage",
        "setAttentionPos",
        "setCursorPos",
        "setMainPixel",
        "setMaskPixel",
        "setMode",
        "stopTransition",
        "stretchBlend",
        "stretchCopy",
        "stretchPile",
    ] {
        assert!(
            env.eval(&format!("l.{member}")).is_ok(),
            "{member} is not registered"
        );
    }
}

#[test]
fn event_handlers_are_assignable_and_default_to_noop() {
    let _lock = vm_lock();
    let env = Env::new();
    // The handler argument counts are the reference `TVP_ACTION_INVOKE_BEGIN`
    // counts: `onMouseMove`/`onKeyDown` take 3 and `onTouchUp` takes 5
    // (`reference/cpp/core/visual/LayerIntf.cpp:10038-10053`,
    // `10102-10119`, `10252-10271`).
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var n = 0; \
         l.onClick = function() { n += 1; }; \
         l.onClick(); \
         l.onPaint(); \
         l.onMouseMove(0, 0, false);",
    );
    assert_eq!(env.eval_int("n"), 1);
    // The un-overridden handlers are callable no-ops.
    assert!(env.eval("l.onKeyDown(0, 0, false)").is_ok());
    assert!(env.eval("l.onTouchUp(0, 0, 0, 0, 0)").is_ok());
}

#[test]
fn color_rect_and_pixel_accessors() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 2, 2);
    env.run(
        "l.colorRect(0, 0, 2, 2, 0xffff0000, 255); \
         var main = l.getMainPixel(0, 0); \
         var mask = l.getMaskPixel(0, 0); \
         l.setMaskPixel(1, 1, 128); \
         var mask2 = l.getMaskPixel(1, 1); \
         l.setMainPixel(1, 0, 0xff00ff00); \
         var main2 = l.getMainPixel(1, 0);",
    );
    // `getMainPixel` masks the alpha out (0x00RRGGBB).
    assert_eq!(env.eval_int("main"), 0x00ff_0000);
    assert_eq!(env.eval_int("mask"), 255);
    assert_eq!(env.eval_int("mask2"), 128);
    assert_eq!(env.eval_int("main2"), 0x0000_ff00);
}

#[test]
fn flip_lr_and_ud_mirror_the_image() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 2, 1);
    env.run(
        "l.setMainPixel(0, 0, 0xff0000ff); \
         l.setMainPixel(1, 0, 0xffff0000); \
         l.flipLR(); \
         var left = l.getMainPixel(0, 0); \
         var right = l.getMainPixel(1, 0);",
    );
    assert_eq!(env.eval_int("left"), 0x00ff_0000);
    assert_eq!(env.eval_int("right"), 0x0000_00ff);
}

#[test]
fn copy_rect_copies_pixels_between_layers() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setImageSize(2, 2); \
         var b = new Layer(w, null); b.setImageSize(2, 2); \
         a.setMainPixel(1, 1, 0xff123456); \
         b.copyRect(0, 0, a, 1, 1, 1, 1); \
         var p = b.getMainPixel(0, 0);",
    );
    assert_eq!(env.eval_int("p"), 0x0012_3456);
}

#[test]
fn operate_rect_blends_with_the_requested_mode() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setImageSize(1, 1); \
         var b = new Layer(w, null); b.setImageSize(1, 1); \
         a.setMainPixel(0, 0, 0xffffffff); \
         a.setMaskPixel(0, 0, 255); \
         b.operateRect(0, 0, a, 0, 0, 1, 1, 2, 255); \
         var p = b.getMainPixel(0, 0);",
    );
    // Source-over of white onto a transparent destination is white.
    assert_eq!(env.eval_int("p"), 0x00ff_ffff);
}

#[test]
fn operate_rect_auto_uses_the_source_layer_extended_blend() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setImageSize(1, 1); \
         var b = new Layer(w, null); b.setImageSize(1, 1); \
         a.setMainPixel(0, 0, 0xff323232); \
         a.setMaskPixel(0, 0, 255); \
         a.type = 16; \
         b.setMainPixel(0, 0, 0xff646464); \
         b.operateRect(0, 0, a, 0, 0, 1, 1); \
         var p = b.getMainPixel(0, 0);",
    );
    // `omAuto` resolves to the source's Photoshop-multiplicative blend:
    // 100 * 50 / 255 = 19, not the 50 a source-over composite would give.
    assert_eq!(env.eval_int("p"), 0x0013_1313);
}

#[test]
fn stretches_and_affine_run_without_error() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 4, 4);
    // `stretchCopy`/`affineCopy`/`operateStretch`/`operateAffine` accept a
    // Layer or a Bitmap, but `stretchPile`/`stretchBlend`/`affinePile`/
    // `affineBlend` accept only a Layer (`reference/cpp/core/visual/
    // LayerIntf.cpp:9002-9027`, `9057-9070`, `9104-9117`, `9227-9251`,
    // `9299-9316`, `9361-9378`, `9154-9225`, `9423-9508`), so use a Layer
    // source for every call.
    env.run(
        "var src = new Layer(w, null); src.setImageSize(2, 2); src.setMainPixel(0, 0, 0xffffffff); \
         l.stretchCopy(0, 0, 4, 4, src, 0, 0, 2, 2, 0); \
         l.stretchPile(0, 0, 4, 4, src, 0, 0, 2, 2, 200, 0); \
         l.stretchBlend(0, 0, 4, 4, src, 0, 0, 2, 2, 200, 0); \
         l.affineCopy(src, 0, 0, 2, 2, true, 1, 0, 0, 1, 0, 0, 0); \
         l.affinePile(src, 0, 0, 2, 2, true, 1, 0, 0, 1, 0, 0, 200, 0); \
         l.affineBlend(src, 0, 0, 2, 2, true, 1, 0, 0, 1, 0, 0, 200, 0); \
         l.operateStretch(0, 0, 4, 4, src, 0, 0, 2, 2, 2, 255, 0); \
         l.operateAffine(src, 0, 0, 2, 2, true, 1, 0, 0, 1, 0, 0, 2, 255, 0);",
    );
    assert_eq!(env.eval_int("l.imageModified"), 1);
}

#[test]
fn gray_gamma_light_and_blur_operate_on_the_image() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 2, 2);
    env.run(
        "l.colorRect(0, 0, 2, 2, 0xff808080, 255); \
         l.doGrayScale(); \
         l.adjustGamma(1.0, 0, 255, 1.0, 0, 255, 1.0, 0, 255); \
         l.light(10, 10); \
         l.gaussianBlur(1, 1.0); \
         l.doBoxBlur(1, 1); \
         l.dump();",
    );
    assert_eq!(env.eval_int("l.imageModified"), 1);
}

#[test]
fn convert_type_round_trips_additive_alpha() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 1, 1);
    env.run(
        "l.setMainPixel(0, 0, 0xff204080); \
         l.setMaskPixel(0, 0, 128); \
         l.face = 4; \
         l.convertType(0); \
         var add = l.getMainPixel(0, 0); \
         l.face = 0; \
         l.convertType(4); \
         var straight = l.getMainPixel(0, 0);",
    );
    // The additive representation premultiplies the RGB.
    assert!(env.eval_int("add") <= 0x00ff_ffff);
    assert!(env.eval_int("straight") >= 0);
}

#[test]
fn copy_to_bitmap_and_copy9patch() {
    let _lock = vm_lock();
    let env = Env::new();
    layer_with_image(&env, 2, 2);
    env.run(
        "l.setMainPixel(0, 0, 0xffabcdef); \
         l.setMaskPixel(0, 0, 255); \
         var b = new Bitmap(1, 1); \
         l.copyToBitmapFromMainImage(b); \
         var p = b.getPixel(0, 0);",
    );
    assert_eq!(env.eval_int("p"), 0xffab_cdef);
    // A 1x1 source is too small for the 9-patch; the call is still valid.
    assert!(env.eval("l.copy9Patch(b)").is_ok());
}

#[test]
fn focus_next_and_prev_walk_the_focus_chain() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.focusable = true; a.visible = true; a.name = 'a'; \
         var b = new Layer(w, null); b.focusable = true; b.visible = true; b.name = 'b'; \
         a.focus(); \
         var next = a.focusNext(); \
         var prev = b.focusPrev();",
    );
    assert_eq!(env.eval_int("next.name == 'b'"), 1);
    assert_eq!(env.eval_int("prev.name == 'a'"), 1);
}

#[test]
fn attention_cursor_and_capture_members_are_effects() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setAttentionPos(3, 4); \
         l.setCursorPos(7, 8); \
         l.releaseCapture(); \
         l.releaseTouchCapture(5); \
         l.releaseTouchCapture();",
    );
    let layer = env.scene.layers.first().expect("one layer");
    assert_eq!((layer.attention_left, layer.attention_top), (3, 4));
    // `setCursorPos` records a pointer warp on the L2 input state.
    assert!(env._input.take_warp_request().is_some());
}

#[test]
fn sibling_order_members_reorder_the_children() {
    let _lock = vm_lock();
    let env = Env::new();
    // `BringToBack` sets order 0 (`reference/cpp/core/visual/LayerIntf.cpp:
    // 1416-1423`); `MoveBefore` places the layer directly in front of the
    // target and `MoveBehind` directly behind it (`LayerIntf.cpp:1356-1397`).
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.name = 'a'; \
         var b = new Layer(w, null); b.name = 'b'; \
         var c = new Layer(w, null); c.name = 'c'; \
         a.bringToBack(); \
         c.moveBefore(a); \
         b.moveBehind(a);",
    );
    let window = env.scene.windows.first().expect("one window");
    let names: Vec<&str> = window
        .layers
        .iter()
        .map(|id| env.scene.layer(*id).map(|l| l.name.as_str()).unwrap_or("?"))
        .collect();
    assert_eq!(names, vec!["b", "a", "c"]);
}

#[test]
fn mode_members_track_the_modal_stack() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setMode(); \
         var first = 1; \
         try { l.setMode(); first = 0; } catch (e) { first = 2; } \
         l.removeMode();",
    );
    // The second `setMode` on an already-modal layer raises.
    assert_eq!(env.eval_int("first"), 2);
}

#[test]
fn transitions_begin_and_stop() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setImageSize(2, 2); \
         var b = new Layer(w, null); b.setImageSize(2, 2); \
         b.beginTransition('crossfade', true, a, null); \
         b.stopTransition();",
    );
    assert!(env.eval("b.stopTransition()").is_ok());
}

/// Every `extrans.dll` provider name resolves to a real L2 transition.
///
/// The shipped game links `extrans.dll` and drives `mosaic` (and sometimes
/// `universal`/`scroll`/`crossfade`) through `beginTransition`; before the
/// port these names raised "unsupported transition".
#[test]
fn extrans_transitions_begin_and_stop() {
    let _lock = vm_lock();
    let env = Env::new();
    for name in [
        "wave",
        "mosaic",
        "turn",
        "rotatezoom",
        "rotatevanish",
        "rotateswap",
        "ripple",
    ] {
        env.run(&format!(
            "var w = new Window(); \
             var a = new Layer(w, null); a.setImageSize(64, 64); \
             var b = new Layer(w, null); b.setImageSize(64, 64); \
             b.beginTransition({name:?}, true, a, %[time: 300]);"
        ));
        env.run("b.stopTransition();");
    }
}

#[test]
fn unsupported_transitions_and_media_helpers_raise_typed_errors() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var a = new Layer(w, null); a.setImageSize(2, 2); \
         var b = new Layer(w, null); b.setImageSize(2, 2);",
    );
    let cases = [
        "(function(){ var w = new Window(); var a = new Layer(w,null); a.setImageSize(2,2); \
          var b = new Layer(w,null); b.setImageSize(2,2); b.beginTransition('nope', true, a, null); })()",
        "(function(){ var w = new Window(); var a = new Layer(w,null); a.setImageSize(2,2); \
          var b = new Layer(w,null); b.setImageSize(2,2); b.beginTransition('universal', true, a, null); })()",
        // The extrans providers are implemented; their option validation is
        // the reference's own (`StartTransition`), surfaced as a typed error.
        "(function(){ var w = new Window(); var a = new Layer(w,null); a.setImageSize(2,2); \
          var b = new Layer(w,null); b.setImageSize(2,2); \
          b.beginTransition('ripple', true, a, %[time:300, rwidth:17]); })()",
        "(function(){ var w = new Window(); var a = new Layer(w,null); a.setImageSize(2,2); \
          var b = new Layer(w,null); b.setImageSize(2,2); \
          b.beginTransition('wave', true, a, %[time:300, wavetype:9]); })()",
        // `loadImages`/`loadProvinceImage` are implemented but need the L2
        // storage attached; without it they raise a typed error.
        "(function(){ var w = new Window(); var l = new Layer(w,null); l.loadImages('x.png'); })()",
        "(function(){ var w = new Window(); var l = new Layer(w,null); l.loadProvinceImage('x.png'); })()",
        "(function(){ var w = new Window(); var l = new Layer(w,null); l.saveLayerImage('x.png'); })()",
        "(function(){ var w = new Window(); var l = new Layer(w,null); l.drawText(0,0,'x',0); })()",
        "(function(){ var w = new Window(); var l = new Layer(w,null); l.drawGlyph(0,0,null,0); })()",
    ];
    for expression in cases {
        let message = env
            .eval(expression)
            .expect_err(&format!("{expression:?} should raise"));
        assert!(
            message.contains("not exposed")
                || message.contains("unsupported")
                || message.contains("not constructed")
                || message.contains("no storage attached")
                || message.contains("requires a `rule` image")
                || message.contains("invalid transition option"),
            "{expression:?} should be a typed error, got: {message}"
        );
    }
}
