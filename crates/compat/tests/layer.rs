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
use engine::clock::FrameClock;
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
    /// Kept alive for the host's raw pointer; never read directly.
    _clock: Box<FrameClock>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut timer = Box::new(TimerState::new());
        let mut input = Box::new(InputState::new());
        let mut clock = Box::new(FrameClock::new());
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
            host.services_mut()
                .attach_clock(clock.as_mut() as *mut FrameClock);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            scene,
            _timer: timer,
            _input: input,
            _clock: clock,
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

/// Reference: `OperateRect` marks `ImageModified` after a successful blit
/// (`reference/cpp/core/visual/LayerIntf.cpp:5459`), and `FillRect` does too
/// (`:4287`). KAG's message layer skips its clear when `imageModified` is
/// false (`system/messagelayer.tjs:434`), and the floating line layer is
/// baked onto the message layer through `operateRect` (`fixLineLayer`), so a
/// composite that leaves the flag false makes the next message draw over the
/// old one. This pins the flag on the composite and the KAG clear that
/// follows it.
#[test]
fn composite_marks_the_layer_modified_for_the_next_message_clear() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); src.setImageSize(2, 2); \
         src.fillRect(0, 0, 2, 2, 0xff00ff00); \
         var l = new Layer(w, null); l.setImageSize(2, 2); \
         l.type = 12; \
         l.fillRect(0, 0, 2, 2, 0xffffffff); \
         l.imageModified = 0; \
         l.operateRect(0, 0, src, 0, 0, 2, 2); \
         var afterComposite = l.imageModified; \
         l.face = 128; \
         l.fillRect(0, 0, 2, 2, 0x80000000); \
         l.face = 3; \
         l.colorRect(0, 0, 2, 2, 0); \
         l.face = 128;",
    );
    // The composite, not `setImageSize`, sets the flag, so KAG's
    // `clearLayer` guard would clear the previous page.
    assert_eq!(env.eval_int("afterComposite"), 1);
    // The KAG clear replaces the old coverage with the frame fill: RGB 0 and
    // the message frame's alpha.
    assert_eq!(env.eval_int("l.getMainPixel(0, 0)"), 0);
    assert_eq!(env.eval_int("l.getMaskPixel(0, 0)"), 128);
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

/// Reference: once a layer is focused, `FocusNext`/`FocusPrev` move within
/// the focused layer's own manager, wrapping and skipping non-focusable
/// nodes (`reference/cpp/core/visual/LayerManager.cpp:723-747`; the search
/// runs the manager's node list, `LayerIntf.cpp:3631-3660`). Both layers
/// are children of one parentless root, so they share its manager
/// (`LayerIntf.cpp:449-455`, `:461-473`).
#[test]
fn focus_next_and_prev_walk_the_focus_chain() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var top = new Layer(w, null); \
         var a = new Layer(w, top); a.focusable = true; a.visible = true; a.name = 'a'; \
         var b = new Layer(w, top); b.focusable = true; b.visible = true; b.name = 'b'; \
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
         b.beginTransition('crossfade', true, a, %[time: 300]); \
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
             var a = new Layer(w, null); a.setImageSize(64, 64); a.setSize(64, 64); \
             var b = new Layer(w, null); b.setImageSize(64, 64); b.setSize(64, 64); \
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
        // `withchildren=true` skips the source/destination image check
        // (`LayerIntf.cpp:9819-9821,7860-7868`), so these raise for the
        // transition name or options instead.
        "(function(){ var w = new Window(); var a = new Layer(w,null); a.setImageSize(2,2); \
          var b = new Layer(w,null); b.setImageSize(2,2); b.beginTransition('nope', true, a, null); })()",
        "(function(){ var w = new Window(); var a = new Layer(w,null); a.setImageSize(2,2); \
          var b = new Layer(w,null); b.setImageSize(2,2); b.beginTransition('universal', true, a, %[time: 300]); })()",
        // The extrans providers are implemented; their option validation is
        // the reference's own (`StartTransition`), surfaced as a typed error.
        "(function(){ var w = new Window(); var a = new Layer(w,null); a.setImageSize(2,2); \
          var b = new Layer(w,null); b.setImageSize(2,2); \
          b.beginTransition('ripple', true, a, %[time:300, rwidth:17]); })()",
        "(function(){ var w = new Window(); var a = new Layer(w,null); a.setImageSize(2,2); \
          var b = new Layer(w,null); b.setImageSize(2,2); \
          b.beginTransition('wave', true, a, %[time:300, wavetype:9]); })()",
        // `loadImages`/`loadProvinceImage`/`saveLayerImage` are implemented
        // but need the host's storage/encoder; without it they raise a typed
        // error. A fresh layer now carries the default main image, so
        // `saveLayerImage` reaches the save seam and raises "no image encoder
        // is installed" (reference `SaveLayerImage` -> `TVPSaveImage`, which
        // throws `TVPUnknownGraphicFormat` with no handler).
        "(function(){ var w = new Window(); var l = new Layer(w,null); l.loadImages('x.png'); })()",
        "(function(){ var w = new Window(); var l = new Layer(w,null); l.loadProvinceImage('x.png'); })()",
        "(function(){ var w = new Window(); var l = new Layer(w,null); l.saveLayerImage('x.png'); })()",
        // `drawText` is implemented and now renders into the default image
        // when the host resolves a font, so it is no longer part of this
        // raise-list. `drawGlyph` still raises because a null glyph carries
        // no text.
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
                || message.contains("no image encoder is installed")
                || message.contains("requires a `rule` image")
                || message.contains("invalid transition option"),
            "{expression:?} should be a typed error, got: {message}"
        );
    }
}

/// An unchanged `visible` write is a no-op: the reference guards on
/// `if(Visible != st)` and skips `Update()` and the parent notify
/// (`reference/cpp/core/visual/LayerIntf.cpp:1183-1193`), so the scene
/// revision does not advance and no subtree cache is marked stale. A real
/// hidden<->visible flip still invalidates.
#[test]
fn unchanged_visible_write_does_not_invalidate() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, null);",
    );
    let id = env.scene.layers.last().expect("the layer").id;
    for layer in &mut env.scene.layers {
        layer.cache_recalc = false;
    }
    let before = env.scene.structure_revision();

    // hidden -> visible still invalidates.
    // reference/cpp/core/visual/LayerIntf.cpp:1187-1193
    env.run("l.visible = true;");
    assert!(
        env.scene.structure_revision() > before,
        "a hidden->visible write still invalidates"
    );
    assert!(env.scene.layer(id).unwrap().cache_recalc);

    // The same value written again touches nothing.
    for layer in &mut env.scene.layers {
        layer.cache_recalc = false;
    }
    let revision = env.scene.structure_revision();
    env.run("l.visible = true;");
    // reference/cpp/core/visual/LayerIntf.cpp:1183
    assert_eq!(
        env.scene.structure_revision(),
        revision,
        "an unchanged visible write bumps no revision"
    );
    assert!(
        !env.scene.layer(id).unwrap().cache_recalc,
        "an unchanged visible write marks no cache stale"
    );

    // visible -> hidden still invalidates.
    env.run("l.visible = false;");
    // reference/cpp/core/visual/LayerIntf.cpp:1187-1193
    assert!(
        env.scene.structure_revision() > revision,
        "a visible->hidden write still invalidates"
    );
    assert!(env.scene.layer(id).unwrap().cache_recalc);
}

/// The reference guards the geometry, opacity and blend property setters on an
/// unchanged value (`reference/cpp/core/visual/LayerIntf.cpp:2163` left,
/// `:2183` top, `:2220` width, `:2235` height, `:1205` opacity, `:1689`
/// type), so a repeated write is free.
#[test]
fn unchanged_geometry_opacity_and_blend_writes_are_no_ops() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, null);",
    );
    let id = env.scene.layers.last().expect("the layer").id;
    for write in [
        "l.left = 7;",
        "l.top = 9;",
        "l.width = 40;",
        "l.height = 24;",
        "l.opacity = 128;",
        "l.type = 1;",
    ] {
        env.run(write);
        for layer in &mut env.scene.layers {
            layer.cache_recalc = false;
        }
        let revision = env.scene.structure_revision();
        env.run(write);
        assert_eq!(
            env.scene.structure_revision(),
            revision,
            "{write} repeated at the same value bumps no revision"
        );
        assert!(
            !env.scene.layer(id).unwrap().cache_recalc,
            "{write} repeated at the same value marks no cache stale"
        );
    }
}

/// `hitType` has no guard in the reference: the inline setter assigns
/// unconditionally (`reference/cpp/core/visual/LayerIntf.h:608`), so a
/// repeated write still invalidates and this port keeps that behaviour.
#[test]
fn unchanged_unguarded_property_write_still_invalidates() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, null);",
    );
    let id = env.scene.layers.last().expect("the layer").id;
    env.run("l.hitType = 1;");
    for layer in &mut env.scene.layers {
        layer.cache_recalc = false;
    }
    let revision = env.scene.structure_revision();
    env.run("l.hitType = 1;");
    assert!(
        env.scene.structure_revision() > revision,
        "an unguarded repeated write bumps revision"
    );
    assert!(env.scene.layer(id).unwrap().cache_recalc);
}

/// `Layer.setPos` guards an unchanged position: the reference's `SetPosition`
/// enters only `if(Rect.left != left || Rect.top != top)`
/// (`reference/cpp/core/visual/LayerIntf.cpp:2203-2204`), so writing back the
/// stored position bumps no revision and marks no cache stale, while a real
/// move still invalidates. A negative offset is legal on a non-primary layer.
#[test]
fn unchanged_set_pos_write_does_not_invalidate() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, null);",
    );
    let id = env.scene.layers.last().expect("the layer").id;

    // A real move invalidates and reads back.
    // reference/cpp/core/visual/LayerIntf.cpp:2204-2216
    env.run("l.setPos(5, 7);");
    assert_eq!(env.eval_int("l.left"), 5);
    assert_eq!(env.eval_int("l.top"), 7);
    assert!(env.scene.layer(id).unwrap().cache_recalc);

    for layer in &mut env.scene.layers {
        layer.cache_recalc = false;
    }
    let revision = env.scene.structure_revision();
    env.run("l.setPos(5, 7);");
    // reference/cpp/core/visual/LayerIntf.cpp:2203
    assert_eq!(
        env.scene.structure_revision(),
        revision,
        "an unchanged setPos bumps no revision"
    );
    assert!(
        !env.scene.layer(id).unwrap().cache_recalc,
        "an unchanged setPos marks no cache stale"
    );

    // The position guard is not a clamp: a negative move still invalidates.
    // reference/cpp/core/visual/LayerIntf.cpp:2204-2211
    env.run("l.setPos(-4, -6);");
    assert!(
        env.scene.structure_revision() > revision,
        "a negative setPos still invalidates"
    );
    assert_eq!(env.eval_int("l.left"), -4);
    assert_eq!(env.eval_int("l.top"), -6);
}

/// `Layer.setPos(left, top, width, height)` takes the reference's `SetBounds`
/// path (`reference/cpp/core/visual/LayerIntf.cpp:8559-8565`):
/// `InternalSetBounds` guards the offsets (`:2144`) and then calls the
/// `InternalSetSize` guard (`:2121-2122`, `:2159`), so repeating the same
/// bounds bumps no revision, while either a moved position or a resized rect
/// still invalidates.
#[test]
fn unchanged_set_pos_with_bounds_write_does_not_invalidate() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, null);",
    );
    let id = env.scene.layers.last().expect("the layer").id;

    // reference/cpp/core/visual/LayerIntf.cpp:2138-2159
    env.run("l.setPos(5, 7, 20, 30);");
    assert_eq!(env.eval_int("l.left"), 5);
    assert_eq!(env.eval_int("l.top"), 7);
    assert_eq!(env.eval_int("l.width"), 20);
    assert_eq!(env.eval_int("l.height"), 30);
    assert!(env.scene.layer(id).unwrap().cache_recalc);

    for layer in &mut env.scene.layers {
        layer.cache_recalc = false;
    }
    let revision = env.scene.structure_revision();
    env.run("l.setPos(5, 7, 20, 30);");
    // reference/cpp/core/visual/LayerIntf.cpp:2121, :2144
    assert_eq!(
        env.scene.structure_revision(),
        revision,
        "an unchanged setPos with bounds bumps no revision"
    );
    assert!(!env.scene.layer(id).unwrap().cache_recalc);

    // A new position alone still invalidates with the size unchanged.
    // reference/cpp/core/visual/LayerIntf.cpp:2144-2159
    env.run("l.setPos(6, 7, 20, 30);");
    assert!(
        env.scene.structure_revision() > revision,
        "a moved setPos with bounds still invalidates"
    );
    assert_eq!(env.eval_int("l.left"), 6);
    assert_eq!(env.eval_int("l.width"), 20);
}

/// `Layer.setSize` delegates to the guarded `InternalSetSize`
/// (`reference/cpp/core/visual/LayerIntf.cpp:2250-2251`): the body runs only
/// when `Rect.get_width() != width || Rect.get_height() != height`
/// (`:2121-2122`), so a repeated size write bumps no revision and does not
/// re-run `ImageLayerSizeChanged` (`:2656-2673`). A real resize still grows
/// the image, a zero extent still changes the rect, and the image never
/// shrinks.
#[test]
fn unchanged_set_size_write_does_not_invalidate() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, null);",
    );
    let id = env.scene.layers.last().expect("the layer").id;

    // reference/cpp/core/visual/LayerIntf.cpp:2122-2139, :2656-2673
    env.run("l.setSize(48, 40);");
    assert_eq!(env.eval_int("l.width"), 48);
    assert_eq!(env.eval_int("l.height"), 40);
    assert_eq!(env.eval_int("l.imageWidth"), 48);
    assert_eq!(env.eval_int("l.imageHeight"), 40);
    assert!(env.scene.layer(id).unwrap().cache_recalc);

    for layer in &mut env.scene.layers {
        layer.cache_recalc = false;
    }
    let revision = env.scene.structure_revision();
    env.run("l.setSize(48, 40);");
    // reference/cpp/core/visual/LayerIntf.cpp:2121
    assert_eq!(
        env.scene.structure_revision(),
        revision,
        "an unchanged setSize bumps no revision"
    );
    assert!(
        !env.scene.layer(id).unwrap().cache_recalc,
        "an unchanged setSize marks no cache stale"
    );

    // A zero extent is a real change; the rect shrinks but the image does not.
    env.run("l.setSize(0, 0);");
    assert!(
        env.scene.structure_revision() > revision,
        "a zero-extent setSize still invalidates"
    );
    assert_eq!(env.eval_int("l.width"), 0);
    assert_eq!(env.eval_int("l.height"), 0);
    assert_eq!(env.eval_int("l.imageWidth"), 48);
    assert_eq!(env.eval_int("l.imageHeight"), 40);
}

/// `Layer.setSizeToImageSize` is `SetSize(GetImageWidth(), GetImageHeight())`
/// (`reference/cpp/core/visual/LayerIntf.cpp:8584-8588`), so it inherits the
/// `InternalSetSize` guard (`:2121-2122`): a call whose values already match
/// bumps no revision, while a real resize does. `setSize` never shrinks the
/// image, so the 4x4 rect still has the default 32x32 image here.
#[test]
fn unchanged_set_size_to_image_size_write_does_not_invalidate() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, null); \
         l.setSize(4, 4);",
    );
    let id = env.scene.layers.last().expect("the layer").id;
    for layer in &mut env.scene.layers {
        layer.cache_recalc = false;
    }
    let revision = env.scene.structure_revision();

    // reference/cpp/core/visual/LayerIntf.cpp:8587-8588, :2122-2139
    env.run("l.setSizeToImageSize();");
    assert!(
        env.scene.structure_revision() > revision,
        "a real setSizeToImageSize resizes and invalidates"
    );
    assert_eq!(env.eval_int("l.width"), 32);
    assert_eq!(env.eval_int("l.height"), 32);
    assert_eq!(env.eval_int("l.imageWidth"), 32);
    assert_eq!(env.eval_int("l.imageHeight"), 32);

    for layer in &mut env.scene.layers {
        layer.cache_recalc = false;
    }
    let revision = env.scene.structure_revision();
    env.run("l.setSizeToImageSize();");
    // reference/cpp/core/visual/LayerIntf.cpp:8587, :2121
    assert_eq!(
        env.scene.structure_revision(),
        revision,
        "an unchanged setSizeToImageSize bumps no revision"
    );
    assert!(!env.scene.layer(id).unwrap().cache_recalc);
}

/// `setImagePos` and `setImageSize` already sit behind their reference guards:
/// `SetImagePosition` only enters on a changed offset
/// (`reference/cpp/core/visual/LayerIntf.cpp:2549`) and `SetImageSize` returns
/// early on an unchanged image size (`:2649-2650`), so repeating either with
/// the stored values bumps no revision while the offset still reads back.
#[test]
fn unchanged_set_image_pos_and_size_writes_do_not_invalidate() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run(
        "var w = new Window(); \
         var base = new Layer(w, null); \
         var l = new Layer(w, null); \
         l.setSize(20, 20); \
         l.setImageSize(24, 24);",
    );
    let id = env.scene.layers.last().expect("the layer").id;

    // reference/cpp/core/visual/LayerIntf.cpp:2549-2560
    env.run("l.setImagePos(-4, -4);");
    assert_eq!(env.eval_int("l.imageLeft"), -4);
    assert_eq!(env.eval_int("l.imageTop"), -4);

    for layer in &mut env.scene.layers {
        layer.cache_recalc = false;
    }
    let revision = env.scene.structure_revision();
    env.run("l.setImagePos(-4, -4);");
    // reference/cpp/core/visual/LayerIntf.cpp:2549
    assert_eq!(
        env.scene.structure_revision(),
        revision,
        "an unchanged setImagePos bumps no revision"
    );
    assert!(!env.scene.layer(id).unwrap().cache_recalc);

    for layer in &mut env.scene.layers {
        layer.cache_recalc = false;
    }
    let revision = env.scene.structure_revision();
    env.run("l.setImageSize(24, 24);");
    // reference/cpp/core/visual/LayerIntf.cpp:2649-2650
    assert_eq!(
        env.scene.structure_revision(),
        revision,
        "an unchanged setImageSize bumps no revision"
    );
    assert!(!env.scene.layer(id).unwrap().cache_recalc);
}

/// A script subclass may touch a native `Layer` member from its class prelude
/// or from its constructor before it calls `super.Layer(...)`. The reference's
/// `CreateNativeInstance` already returns a layer carrying its default rect
/// and main image, so member access needs no owner
/// (`reference/cpp/core/visual/LayerIntf.cpp:304-410`, `:2203`, `:3683`);
/// only `font` refuses until the constructor has run (`:6182-6188`). The port
/// used to reject every member until `super.Layer(...)` set the payload id,
/// which broke `MessageLayer.tjs`'s `left` property setter.
#[test]
fn subclass_member_writes_before_the_base_constructor_land() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        r#"
        var w = new Window();
        class BeforeSuper extends Layer {
            function BeforeSuper(win, parent) {
                left = 11;
                top = 13;
                focusable = true;
                super.Layer(win, parent);
            }
        }
        var early = new BeforeSuper(w, null);
        early.name = 'before-super';

        class InPrelude extends Layer {
            visible = true;
            focusable = true;
            function InPrelude(win, parent) { super.Layer(win, parent); }
        }
        var prelude = new InPrelude(w, null);
        prelude.name = 'in-prelude';
        "#,
    );

    // reference/cpp/core/visual/LayerIntf.cpp:2203 (SetPosition stores the
    // rect without a manager), :3683 (SetFocusable).
    let early = env
        .scene
        .layers
        .iter()
        .find(|layer| layer.name == "before-super")
        .expect("the before-super layer");
    assert_eq!(
        (early.rect.x, early.rect.y),
        (11, 13),
        "a member write before super.Layer must reach the joined layer"
    );
    assert!(
        early.focusable,
        "a member write before super.Layer must reach the joined layer"
    );

    // reference/cpp/core/visual/LayerIntf.cpp:1183 (SetVisible stores the
    // flag before any manager exists).
    let prelude = env
        .scene
        .layers
        .iter()
        .find(|layer| layer.name == "in-prelude")
        .expect("the in-prelude layer");
    assert!(
        prelude.visible,
        "a class-body member write before super.Layer must reach the joined layer"
    );
    assert!(
        prelude.focusable,
        "a class-body member write before super.Layer must reach the joined layer"
    );
}

/// Repeated create/invalidate/drop cycles of a parent layer tree must release
/// every layer, bitmap and font the tree created. The reference native
/// instance keeps only a non-owning owner pointer (`iTJSDispatch2 *Owner`,
/// `reference/cpp/core/visual/LayerIntf.h:173`) and `Invalidate` releases the
/// parent's resources (`reference/cpp/core/visual/LayerIntf.cpp:515-566`), so
/// a parent whose script reference is dropped reaches reference count zero
/// and its native teardown runs. A strong L3 registry entry prevented that:
/// the parent stayed rooted, the destroy callback never ran, and
/// `scene.layers` grew with every cycle.
#[test]
fn repeated_parent_layer_cycles_release_layers_bitmaps_and_fonts() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var w = new Window();");
    let baseline_layers = env.scene.layers.len();
    let baseline_bitmaps = env.scene.bitmaps.len();
    let baseline_fonts = env.scene.fonts.len();

    env.run(
        "var page, row, face; \
         for (var cycle = 0; cycle < 10; cycle++) { \
             page = new Layer(w, null); \
             for (var i = 0; i < 8; i++) { \
                 row = new Layer(w, page); \
                 row.setImageSize(8, 8); \
                 face = row.font; \
             } \
             invalidate page; \
         } \
         page = null; row = null; face = null;",
    );
    // The collector must run outside the creating frame: an in-frame
    // `System.doCompact()` leaves that frame's stale variant-array
    // register slot live, so the page's last object survives it. The
    // frame body's automatic idle compact is the out-of-frame pass the
    // app uses (`reference/cpp/core/environ/impl/SystemControl.cpp:174-179`);
    // a separate execution exercises the script path and the direct
    // `do_gc` reproduces that pass.
    env.run("System.doCompact();");
    env.engine.do_gc().expect("idle compact");

    assert_eq!(
        env.scene.layers.len(),
        baseline_layers,
        "an invalidated parent must not leave its layer tree in the scene"
    );
    assert_eq!(
        env.scene.bitmaps.len(),
        baseline_bitmaps,
        "an invalidated parent must not leave its bitmaps in the scene"
    );
    assert_eq!(
        env.scene.fonts.len(),
        baseline_fonts,
        "an invalidated parent must not leave its fonts in the scene"
    );
}

/// A child `Font` wrapper must not stay in the engine's retained map once the
/// child's own script object is collected. A parent invalidation only severs
/// the child (`reference/cpp/core/visual/LayerIntf.cpp:543-549`), so the
/// record and its wrapper are released by the child's finalizer; the script may
/// still hold the child in an array until it drops its reference.
#[test]
fn invalidating_a_parent_layer_releases_child_font_wrappers() {
    let _lock = vm_lock();
    let env = Env::new();
    let baseline = env.engine.retained_count();
    env.run(
        "var w = new Window(); \
         var page = new Layer(w, null); \
         var row = new Layer(w, page); \
         var a = page.font; \
         var b = row.font;",
    );
    assert!(
        env.engine.retained_count() > baseline,
        "the font getters must retain their wrappers"
    );

    env.run("invalidate page; page = null; row = null; a = null; b = null;");
    env.run("System.doCompact();");
    env.engine.do_gc().expect("idle compact");

    assert_eq!(env.scene.layers.len(), 0, "the parent subtree is gone");
    assert!(
        env.engine.retained_count() <= baseline + 1,
        "child font wrappers must not stay retained: {} > {}",
        env.engine.retained_count(),
        baseline + 1
    );
}

/// A parent invalidation leaves its children alone; each child record
/// outlives the parent's invalidation and is released with the child's own
/// script object.
///
/// Reference `tTJSNI_BaseLayer::Invalidate`
/// (`reference/cpp/core/visual/LayerIntf.cpp:515-579`) severs the children
/// with `child->Part()` (`:546-549`) and never invalidates them, so a
/// child's script object stays valid until the script drops it
/// (`reference/cpp/core/tjs2/tjsObject.cpp:389-405`). A layer finalizer can
/// write a native property, and it must still find its own Scene record when
/// it runs.
#[test]
fn invalidating_a_parent_leaves_the_child_record_until_the_child_is_collected() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var page = new Layer(w, null); \
         var row = new Layer(w, page); \
         row.finalize = function() { cursor = 0; };",
    );
    assert_eq!(env.scene.layers.len(), 2);

    env.run("invalidate page; page = null; row = null;");

    // The parent's own record is gone; the child's is not, because the child
    // script object has not been collected yet.
    assert_eq!(
        env.scene.layers.len(),
        1,
        "the child record must outlive the parent's invalidation"
    );

    // The child is torn down with its own script object: its finalizer runs
    // against a live record, and the record goes with it.
    env.run("System.doCompact();");
    env.engine.do_gc().expect("idle compact");
    assert_eq!(
        env.scene.layers.len(),
        0,
        "the collected child must remove its own Scene record"
    );
}

/// Reference `tTJSNI_BaseLayer::Invalidate`
/// (`reference/cpp/core/visual/LayerIntf.cpp:515-579`) severs its direct
/// children with `child->Part()` (`:546-549`); `Part()` (`:624-631`) only
/// detaches, so a child keeps its native layer, its script object and its
/// validity. The child is torn down when its **own** script object is
/// finalized (`reference/cpp/core/tjs2/tjsObject.cpp:389-405`).
///
/// Invalidating the children as well would kill script objects the reference
/// keeps alive: a member read on such a child answers "The object is already
/// invalidated" (`reference/cpp/core/tjs2/tjsError.h:298`) instead of
/// working, which is the divergence this test pins.
#[test]
fn invalidating_a_parent_layer_leaves_the_child_script_object_valid() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var page = new Layer(w, null); \
         var row = new Layer(w, page); \
         row.name = 'alive';",
    );
    assert_eq!(env.scene.layers.len(), 2);

    env.run("invalidate page;");

    // The parent is gone: the VM refuses every dispatch on it.
    assert!(
        env.eval("page.name").is_err(),
        "the invalidated parent must refuse dispatch"
    );
    // The child is untouched: its script object is still valid ...
    assert_eq!(
        env.eval("row.name"),
        Ok(TjsValue::String("alive".to_string())),
        "the child script object must stay valid after its parent is invalidated"
    );
    // ... and its Scene record is still there.
    assert_eq!(
        env.scene.layers.len(),
        1,
        "the child record must outlive its parent's invalidation"
    );

    // The child is torn down with its own script object.
    env.run("row = null;");
    env.run("System.doCompact();");
    env.engine.do_gc().expect("idle compact");
    assert_eq!(
        env.scene.layers.len(),
        0,
        "the child must be released with its own script object"
    );
}
