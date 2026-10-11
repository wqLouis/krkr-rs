//! Integration tests for the `VideoOverlay` native class.
//!
//! L3 links no decoder, so the tests install a host [`VideoSource`] through
//! [`CompatHost::set_video_source`] — the same seam an L4 host uses — and then
//! drive the script surface against the L2 [`engine::video::VideoOverlay`]
//! state. The class is a standalone native (it does not derive from `Layer`);
//! a scene is attached only so the `Layer` fixtures exist for the layer-ref
//! arguments.

mod common;

use api::media::{RgbaFrame, VideoSpec};
use common::vm_lock;
use compat::CompatHost;
use engine::Scene;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host + attached scene. The host is declared first so it drops
/// before the engine (releasing every retained script object while the engine
/// is alive) and the scene.
struct Env {
    host: CompatHost,
    engine: Box<Tjs2Engine>,
    _scene: Box<Scene>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene outlives the host (declared after it).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            host,
            engine,
            _scene: scene,
        }
    }

    /// Install the host-decoded source: a 640x360 @ 30 fps stream with audio,
    /// 300 frames, and three decoded frames.
    fn set_default_source(&self) {
        self.host.set_video_source(default_spec(), default_frames());
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "video-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "video-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval_real(&self, expression: &str) -> f64 {
        match self.eval(expression) {
            TjsValue::Real(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a real"),
        }
    }

    /// The scene id of the layer named `name`.
    fn layer_id(&self, name: &str) -> u32 {
        self._scene
            .layers
            .iter()
            .find(|layer| layer.name == name)
            .unwrap_or_else(|| panic!("the layer {name:?}"))
            .id
    }

    /// The layer named `name`'s rectangle size in pixels.
    fn layer_rect(&self, name: &str) -> (u32, u32) {
        let id = self.layer_id(name);
        self._scene
            .layer(id)
            .map(|layer| (layer.rect.w, layer.rect.h))
            .unwrap_or((0, 0))
    }

    /// The RGBA pixel at `(x, y)` of the layer named `name`.
    fn bitmap_color(&self, name: &str, x: u32, y: u32) -> [u8; 4] {
        let id = self.layer_id(name);
        let bitmap_id = self
            ._scene
            .layer(id)
            .and_then(|layer| layer.bitmap)
            .expect("the layer bitmap");
        let bitmap = self._scene.bitmap(bitmap_id).expect("bitmap state");
        let offset = bitmap.pixel_offset(x, y).expect("pixel in range");
        [
            bitmap.rgba[offset],
            bitmap.rgba[offset + 1],
            bitmap.rgba[offset + 2],
            bitmap.rgba[offset + 3],
        ]
    }
}

/// A 4x4 frame filled with `color`, at timestamp 0.
fn solid_frame(color: [u8; 4]) -> RgbaFrame {
    let mut rgba = Vec::with_capacity(4 * 4 * 4);
    for _ in 0..(4 * 4) {
        rgba.extend_from_slice(&color);
    }
    RgbaFrame::new(4, 4, 0, rgba)
}

/// A 4x4 @ 10 fps, 10-frame stream.
fn movie_spec() -> VideoSpec {
    VideoSpec {
        width: 4,
        height: 4,
        fps: 10.0,
        total_frames: Some(10),
        sample_rate: 0,
        channels: 0,
    }
}

fn default_spec() -> VideoSpec {
    VideoSpec {
        width: 640,
        height: 360,
        fps: 30.0,
        total_frames: Some(300),
        sample_rate: 48_000,
        channels: 2,
    }
}

fn default_frames() -> Vec<RgbaFrame> {
    (0..3)
        .map(|i| RgbaFrame::new(1, 1, i * 100, vec![i as u8, 0, 0, 255]))
        .collect()
}

/// `open` reads its metadata from the host source, and the read-only media
/// properties report it.
#[test]
fn open_reads_host_metadata() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(null); \
         v.open('movie.mpg');",
    );
    assert_eq!(env.eval_int("v.originalWidth"), 640);
    assert_eq!(env.eval_int("v.originalHeight"), 360);
    assert_eq!(env.eval_int("v.numberOfFrame"), 300);
    assert_eq!(env.eval_int("v.totalTime"), 10_000);
    assert_eq!(env.eval_int("v.numberOfAudioStream"), 1);
    assert_eq!(env.eval_int("v.numberOfVideoStream"), 1);
    assert!((env.eval_real("v.fps") - 30.0).abs() < 1e-9);

    env.run("v.close();");
    assert_eq!(env.eval_int("v.originalWidth"), 0);
    assert_eq!(env.eval_int("v.numberOfFrame"), 0);
    assert_eq!(env.eval_int("v.numberOfVideoStream"), 0);
}

/// Without a host source `open` raises (D11) instead of fabricating metadata.
#[test]
fn open_without_a_source_raises() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var v = new VideoOverlay(null); var caught = 0; \
         try { v.open('movie.mpg'); } catch (e) { caught = 1; }",
    );
    assert_eq!(env.eval_int("caught"), 1);
    assert_eq!(env.eval_int("v.originalWidth"), 0);
}

/// An unusable spec (here: `fps == 0`) is a typed L2 error surfaced to the
/// script, not a silent open.
#[test]
fn open_with_an_invalid_spec_raises() {
    let _lock = vm_lock();
    let env = Env::new();
    env.host.set_video_source(
        VideoSpec {
            fps: 0.0,
            ..default_spec()
        },
        default_frames(),
    );
    env.run(
        "var v = new VideoOverlay(null); var caught = 0; \
         try { v.open('movie.mpg'); } catch (e) { caught = 1; }",
    );
    assert_eq!(env.eval_int("caught"), 1);
}

/// The rectangle/visibility surface maps to the L2 bounds.
#[test]
fn bounds_and_visibility_round_trip() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var v = new VideoOverlay(null); \
         v.left = 1; v.top = 2; v.width = 3; v.height = 4; \
         v.visible = true; \
         v.setPos(9, 8); v.setSize(7, 6); \
         v.setBounds(5, 6, 7, 8);",
    );
    assert_eq!(env.eval_int("v.left"), 5);
    assert_eq!(env.eval_int("v.top"), 6);
    assert_eq!(env.eval_int("v.width"), 7);
    assert_eq!(env.eval_int("v.height"), 8);
    assert_eq!(env.eval_int("v.visible"), 1);
    env.run("v.visible = false;");
    assert_eq!(env.eval_int("v.visible"), 0);
}

/// `play`/`pause`/`stop`/`rewind` and the position/frame clock map to L2.
#[test]
fn playback_commands_translate_to_l2() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(null); \
         v.open('movie.mpg'); \
         v.position = 100; var p1 = v.position; \
         v.frame = 15; var p2 = v.position; \
         v.play(); v.pause(); \
         v.stop(); var p3 = v.position; \
         v.position = 250; v.rewind(); var p4 = v.position; \
         v.prepare();",
    );
    assert_eq!(env.eval_int("p1"), 100);
    assert_eq!(env.eval_int("p2"), 500);
    assert_eq!(env.eval_int("p3"), 0);
    assert_eq!(env.eval_int("p4"), 0);
}

/// Playback commands on a closed overlay are typed errors.
#[test]
fn playback_on_a_closed_overlay_raises() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var v = new VideoOverlay(null); var caught = 0; \
         try { v.play(); } catch (e) { caught++; } \
         try { v.pause(); } catch (e) { caught++; } \
         try { v.stop(); } catch (e) { caught++; } \
         try { v.prepare(); } catch (e) { caught++; }",
    );
    assert_eq!(env.eval_int("caught"), 4);
}

/// Loop, segment loop, period event and play rate.
#[test]
fn loop_and_period_members_round_trip() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(null); \
         v.open('movie.mpg'); \
         v.loop = true; var loop = v.loop; \
         v.setSegmentLoop(10, 20); \
         var s0 = v.segmentLoopStartFrame; var s1 = v.segmentLoopEndFrame; \
         v.cancelSegmentLoop(); \
         var s2 = v.segmentLoopStartFrame; var s3 = v.segmentLoopEndFrame; \
         v.setPeriodEvent(5); var pe0 = v.periodEventFrame; \
         v.cancelPeriodEvent(); var pe1 = v.periodEventFrame; \
         v.periodEventFrame = 7; var pe2 = v.periodEventFrame; \
         v.playRate = 2.0; var rate = v.playRate;",
    );
    assert_eq!(env.eval_int("loop"), 1);
    assert_eq!(env.eval_int("s0"), 10);
    assert_eq!(env.eval_int("s1"), 20);
    assert_eq!(env.eval_int("s2"), -1);
    assert_eq!(env.eval_int("s3"), -1);
    assert_eq!(env.eval_int("pe0"), 5);
    assert_eq!(env.eval_int("pe1"), -1);
    assert_eq!(env.eval_int("pe2"), 7);
    assert!((env.eval_real("rate") - 2.0).abs() < 1e-9);
}

/// Invalid segment ranges and play rates are typed errors.
#[test]
fn invalid_loop_and_rate_raise() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(null); \
         v.open('movie.mpg'); var caught = 0; \
         try { v.setSegmentLoop(-1, 5); } catch (e) { caught++; } \
         try { v.setSegmentLoop(5, 2); } catch (e) { caught++; } \
         try { v.playRate = 0; } catch (e) { caught++; }",
    );
    assert_eq!(env.eval_int("caught"), 3);
}

/// `layer1`/`layer2` store a real Layer object and return it; a null clears
/// the slot and any non-Layer object raises (no member fallback).
///
/// Reference: the setters resolve the argument with
/// `NativeInstanceSupport(TJS_NIS_GETINSTANCE, tTJSNC_Layer::ClassID)` and
/// throw `TVPSpecifyLayer` otherwise
/// (`reference/cpp/core/visual/VideoOvlIntf.cpp:756-768`).
#[test]
fn layer_refs_round_trip_and_validate() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         var v = new VideoOverlay(null); \
         v.layer1 = l; var same1 = (v.layer1 === l); \
         v.layer2 = l; var same2 = (v.layer2 === l); \
         v.layer1 = null; var cleared = (v.layer1 === null); \
         var thrown = 0; \
         try { v.layer1 = %[nativeId: 7]; } catch (e) { thrown = 1; } \
         var thrown2 = 0; \
         try { v.layer1 = %[foo: 1]; } catch (e) { thrown2 = 1; }",
    );
    // Reference: the getter returns the assigned object, the setter's
    // `NativeInstanceSupport` lookup rejects the dictionary, and a null
    // argument clears the slot (`VideoOvlIntf.cpp:756-768`).
    assert_eq!(env.eval_int("same1"), 1);
    assert_eq!(env.eval_int("same2"), 1);
    assert_eq!(env.eval_int("cleared"), 1);
    assert_eq!(env.eval_int("thrown"), 1);
    assert_eq!(env.eval_int("thrown2"), 1);
}

/// `setMixingLayer`/`resetMixingLayer` accept a visible Layer, ignore a
/// non-visible one, and reject a non-Layer object.
///
/// Reference: `setMixingLayer` resolves the argument with
/// `NativeInstanceSupport(TJS_NIS_GETINSTANCE, tTJSNC_Layer::ClassID)` and
/// throws `TVPSpecifyLayer` otherwise
/// (`reference/cpp/core/visual/VideoOvlIntf.cpp:385-398`), then reads the
/// layer's `visible`/`opacity`.
#[test]
fn mixing_layer_selection_matches_the_reference() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var shown = new Layer(w, null); shown.visible = true; shown.opacity = 128; \
         var hidden = new Layer(w, null); hidden.visible = false; hidden.opacity = 255; \
         var v = new VideoOverlay(null); \
         v.setMixingLayer(shown); \
         v.setMixingLayer(hidden); \
         v.resetMixingLayer(); \
         v.setMixingLayer(null); \
         var thrown = 0; \
         try { v.setMixingLayer(%[foo: 1]); } catch (e) { thrown = 1; } \
         var thrown2 = 0; \
         try { v.setMixingLayer(%[nativeId: 3, visible: 1]); } catch (e) { thrown2 = 1; }",
    );
    // Reference: a non-Layer object raises `TVPSpecifyLayer`
    // (`VideoOvlIntf.cpp:385-398`), with no `nativeId` member fallback.
    assert_eq!(env.eval_int("thrown"), 1);
    assert_eq!(env.eval_int("thrown2"), 1);
}

/// The backend passthrough members (mode, streams, mixing colors) round-trip.
#[test]
fn passthrough_members_round_trip() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var v = new VideoOverlay(null); \
         v.mode = 1; var mode = v.mode; \
         v.audioBalance = 10; var ab = v.audioBalance; \
         v.audioVolume = 20; var av = v.audioVolume; \
         v.enabledAudioStream = 1; var eas = v.enabledAudioStream; \
         v.enabledVideoStream = 0; var evs = v.enabledVideoStream; \
         v.selectAudioStream(2); var eas2 = v.enabledAudioStream; \
         v.mixingMovieAlpha = 0.5; var mma = v.mixingMovieAlpha; \
         v.mixingMovieBGColor = 0x00ff00; var mmb = v.mixingMovieBGColor;",
    );
    assert_eq!(env.eval_int("mode"), 1);
    assert_eq!(env.eval_int("ab"), 10);
    assert_eq!(env.eval_int("av"), 20);
    assert_eq!(env.eval_int("eas"), 1);
    assert_eq!(env.eval_int("evs"), 0);
    assert_eq!(env.eval_int("eas2"), 2);
    assert!((env.eval_real("mma") - 0.5).abs() < 1e-9);
    assert_eq!(env.eval_int("mmb"), 0x00ff00);
}

/// The color-adjustment values and their backend-defined metadata.
#[test]
fn color_adjustment_round_trips() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var v = new VideoOverlay(null); \
         var before = v.contrast; \
         v.contrast = 1.5; v.brightness = -0.5; v.hue = 90; v.saturation = 0.25; \
         var c = v.contrast; var b = v.brightness; var h = v.hue; var s = v.saturation; \
         var cmin = v.contrastRangeMin; var cmax = v.contrastRangeMax; \
         var cdef = v.contrastDefaultValue; var cstep = v.contrastStepSize; \
         var bmin = v.brightnessRangeMin; var bmax = v.brightnessRangeMax; \
         var hmin = v.hueRangeMin; var hmax = v.hueRangeMax; var hstep = v.hueStepSize; \
         var smin = v.saturationRangeMin; var smax = v.saturationRangeMax; \
         var sdef = v.saturationDefaultValue; var sstep = v.saturationStepSize;",
    );
    assert!((env.eval_real("before") - 1.0).abs() < 1e-9);
    assert!((env.eval_real("c") - 1.5).abs() < 1e-9);
    assert!((env.eval_real("b") + 0.5).abs() < 1e-9);
    assert!((env.eval_real("h") - 90.0).abs() < 1e-9);
    assert!((env.eval_real("s") - 0.25).abs() < 1e-9);
    assert!((env.eval_real("cmin") - 0.0).abs() < 1e-9);
    assert!((env.eval_real("cmax") - 2.0).abs() < 1e-9);
    assert!((env.eval_real("cdef") - 1.0).abs() < 1e-9);
    assert!((env.eval_real("cstep") - 0.01).abs() < 1e-9);
    assert!((env.eval_real("bmin") + 1.0).abs() < 1e-9);
    assert!((env.eval_real("bmax") - 1.0).abs() < 1e-9);
    assert!((env.eval_real("hmin") + 180.0).abs() < 1e-9);
    assert!((env.eval_real("hmax") - 180.0).abs() < 1e-9);
    assert!((env.eval_real("hstep") - 1.0).abs() < 1e-9);
    assert!((env.eval_real("smin") - 0.0).abs() < 1e-9);
    assert!((env.eval_real("smax") - 2.0).abs() < 1e-9);
    assert!((env.eval_real("sdef") - 1.0).abs() < 1e-9);
    assert!((env.eval_real("sstep") - 0.01).abs() < 1e-9);
}

/// Every registered member is reachable from a script (the constructor plus
/// the overridable event hooks included).
#[test]
fn every_member_is_reachable() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(null); \
         v.open('movie.mpg'); \
         v.play(); v.pause(); v.stop(); v.rewind(); v.prepare(); \
         v.setPos(1, 2); v.setSize(3, 4); v.setBounds(5, 6, 7, 8); \
         v.setSegmentLoop(0, 10); v.cancelSegmentLoop(); \
         v.setPeriodEvent(3); v.cancelPeriodEvent(); \
         v.setMixingLayer(null); v.resetMixingLayer(); \
         v.selectAudioStream(0); \
         v.onStatusChanged('play'); v.onCallbackCommand('a', 'b'); \
         v.onPeriod(1); v.onFrameUpdate(2); \
         v.left = 0; v.top = 0; v.width = 1; v.height = 1; v.visible = true; \
         v.position = 0; v.frame = 0; v.loop = false; v.mode = 0; \
         v.playRate = 1.0; v.periodEventFrame = -1; \
         v.audioBalance = 0; v.audioVolume = 0; \
         v.enabledAudioStream = 0; v.enabledVideoStream = 0; \
         v.mixingMovieAlpha = 1.0; v.mixingMovieBGColor = 0; \
         v.contrast = 1.0; v.brightness = 0.0; v.hue = 0.0; v.saturation = 1.0; \
         v.layer1 = null; v.layer2 = null; v.close();",
    );
    // Reading every read-only member must not throw either.
    env.run(
        "var a = v.segmentLoopStartFrame; var b = v.segmentLoopEndFrame; \
         var c = v.contrastRangeMin; var d = v.contrastRangeMax; \
         var e = v.contrastDefaultValue; var f = v.contrastStepSize; \
         var g = v.brightnessRangeMin; var h = v.brightnessRangeMax; \
         var i = v.brightnessDefaultValue; var j = v.brightnessStepSize; \
         var k = v.hueRangeMin; var l = v.hueRangeMax; \
         var m = v.hueDefaultValue; var n = v.hueStepSize; \
         var o = v.saturationRangeMin; var p = v.saturationRangeMax; \
         var q = v.saturationDefaultValue; var r = v.saturationStepSize; \
         var s = v.originalWidth; var t = v.originalHeight; \
         var u = v.numberOfFrame; var w = v.fps; var x = v.totalTime; \
         var y = v.numberOfAudioStream; var z = v.numberOfVideoStream;",
    );
}

/// `vomLayer` assigns the playing frame to `layer1` and sizes the layer to the
/// video.
///
/// Reference: the `EC_UPDATE` handler assigns the front buffer to
/// `layer1`/`layer2` and calls their `Update`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:583-621`), and only while
/// the status is `ssPlay` (`:554`); it also sizes the layer to the video
/// (`:585-604`).
#[test]
fn layer_mode_composites_the_frame_into_the_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.name = 'video'; l.visible = true;",
    );
    env.host
        .set_video_source(movie_spec(), vec![solid_frame([255, 0, 0, 255])]);
    env.run(
        "var v = new VideoOverlay(null); v.mode = 1; v.layer1 = l; \
         v.open('movie.mpg'); v.play();",
    );
    let ctx = env.host.ctx().expect("bound context");
    compat::video_advance(&ctx, 50.0).expect("advance");
    assert_eq!(env.bitmap_color("video", 1, 1), [255, 0, 0, 255]);
    assert_eq!(env.layer_rect("video"), (4, 4));
}

/// The default `vomOverlay` mode leaves the script layer untouched.
///
/// Reference: the `EC_UPDATE` composite is guarded by `Mode == vomLayer`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:554`); `vomOverlay` draws
/// through an OS overlay window instead.
#[test]
fn overlay_mode_leaves_the_layer_untouched() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.name = 'video'; l.visible = true; \
         l.setImageSize(4, 4);",
    );
    env.host
        .set_video_source(movie_spec(), vec![solid_frame([255, 0, 0, 255])]);
    env.run(
        "var v = new VideoOverlay(null); v.layer1 = l; \
         v.open('movie.mpg'); v.play();",
    );
    let ctx = env.host.ctx().expect("bound context");
    compat::video_advance(&ctx, 50.0).expect("advance");
    assert_ne!(env.bitmap_color("video", 1, 1), [255, 0, 0, 255]);
}

/// `vomMixer` presents the frame through the mixing layer.
///
/// Reference: the `EC_UPDATE` handler for `vomMixer` presents the video image
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:642-656`), with the mixing
/// bitmap the `setMixingLayer` setter installed (`:882-913`).
#[test]
fn mixer_mode_composites_through_the_mixing_layer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.name = 'mix'; l.visible = true; \
         l.setImageSize(4, 4);",
    );
    env.host
        .set_video_source(movie_spec(), vec![solid_frame([0, 255, 0, 255])]);
    env.run(
        "var v = new VideoOverlay(null); v.mode = 2; v.setMixingLayer(l); \
         v.open('movie.mpg'); v.play();",
    );
    let ctx = env.host.ctx().expect("bound context");
    compat::video_advance(&ctx, 50.0).expect("advance");
    assert_eq!(env.bitmap_color("mix", 1, 1), [0, 255, 0, 255]);
}

/// The advance uses the caller's per-frame `dt`, never wall time.
///
/// Reference: the frame body advances playback by the elapsed frame interval;
/// the port takes that interval from the frame body instead of the clock.
#[test]
fn advance_uses_the_frame_body_dt() {
    let _lock = vm_lock();
    let env = Env::new();
    env.host
        .set_video_source(movie_spec(), vec![solid_frame([255, 0, 0, 255])]);
    env.run("var v = new VideoOverlay(null); v.open('movie.mpg'); v.play();");
    let ctx = env.host.ctx().expect("bound context");
    compat::video_advance(&ctx, 50.0).expect("advance");
    assert_eq!(env.eval_int("v.position"), 50);
    compat::video_advance(&ctx, 150.0).expect("advance");
    assert_eq!(env.eval_int("v.position"), 200);
}
