//! Integration tests for the `VideoOverlay` native class.
//!
//! L3 links no decoder, so the tests install a host [`VideoSource`] through
//! [`CompatHost::set_video_source`] — the same seam an L4 host uses — and then
//! drive the script surface against the L2 [`engine::video::VideoOverlay`]
//! state. The class is a standalone native (it does not derive from `Layer`);
//! a scene is attached only so the `Layer` fixtures exist for the layer-ref
//! arguments.

mod common;

use std::sync::Arc;

use api::error::MediaError;
use api::media::{AudioSpec, DecodedAudio, RgbaFrame, VideoSpec};
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

    fn eval_string(&self, expression: &str) -> String {
        match self.eval(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
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

/// A 4x4 @ 10 fps, 10-frame video-only stream.
fn movie_spec() -> VideoSpec {
    VideoSpec {
        width: 4,
        height: 4,
        fps: 10.0,
        total_frames: Some(10),
        sample_rate: 0,
        channels: 0,
        audio_streams: 0,
        video_streams: 1,
        audio_stream: -1,
        video_stream: 0,
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
        audio_streams: 1,
        video_streams: 1,
        audio_stream: 0,
        video_stream: 0,
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
        "var v = new VideoOverlay(new Window()); \
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
        "var v = new VideoOverlay(new Window()); var caught = 0; \
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
        "var v = new VideoOverlay(new Window()); var caught = 0; \
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
        "var v = new VideoOverlay(new Window()); \
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
        "var v = new VideoOverlay(new Window()); \
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

/// Playback commands on a closed overlay are inert, not errors, and the
/// status stays `unload`.
///
/// Reference: `tTJSNI_VideoOverlay::Stop`/`Play`/`Pause`/`Prepare` guard the
/// backend call with `if(VideoOverlay)`, so a closed overlay fires no
/// `onStatusChanged` and leaves `Status` at `ssUnload`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:308-355`). This is the
/// `EnvEffect.tjs:804` call that must not fail.
#[test]
fn playback_on_a_closed_overlay_is_inert() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var v = new VideoOverlay(new Window()); var caught = 0; \
         try { v.play(); } catch (e) { caught++; } \
         try { v.pause(); } catch (e) { caught++; } \
         try { v.stop(); } catch (e) { caught++; } \
         try { v.prepare(); } catch (e) { caught++; } \
         var p = v.position;",
    );
    assert_eq!(env.eval_int("caught"), 0);
    assert_eq!(env.eval_int("p"), 0);
}

/// A fresh overlay has the reference's default 320x240 rectangle.
///
/// Reference: the constructor sets `Rect.right = 320` and
/// `Rect.bottom = 240` (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:88-91`).
#[test]
fn fresh_overlay_has_the_default_rectangle() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var v = new VideoOverlay(new Window());");
    assert_eq!(env.eval_int("v.left"), 0);
    assert_eq!(env.eval_int("v.top"), 0);
    assert_eq!(env.eval_int("v.width"), 320);
    assert_eq!(env.eval_int("v.height"), 240);
}

/// Loop, segment loop, period event and play rate.
#[test]
fn loop_and_period_members_round_trip() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(new Window()); \
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

/// Segment-loop bounds and the play rate are stored verbatim; the reference
/// validates neither.
///
/// Reference: `SetSegmentLoop` stores both frames and calls the backend with
/// no range check (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:357-361`);
/// `SetLoop(false)` itself uses `(-1, -1)` (`:787-789`); `SetPlayRate` is
/// unvalidated (`reference/cpp/core/movie/ffmpeg/KRMoviePlayer.cpp:75-79`).
#[test]
fn segment_loop_and_play_rate_accept_any_value() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(new Window()); \
         v.open('movie.mpg'); var caught = 0; \
         try { v.setSegmentLoop(-1, 5); } catch (e) { caught++; } \
         var s0 = v.segmentLoopStartFrame; var s1 = v.segmentLoopEndFrame; \
         try { v.setSegmentLoop(5, 2); } catch (e) { caught++; } \
         var s2 = v.segmentLoopStartFrame; var s3 = v.segmentLoopEndFrame; \
         try { v.playRate = 0; } catch (e) { caught++; } \
         var r0 = v.playRate; \
         try { v.playRate = -3; } catch (e) { caught++; } \
         var r1 = v.playRate;",
    );
    assert_eq!(env.eval_int("caught"), 0);
    assert_eq!(env.eval_int("s0"), -1);
    assert_eq!(env.eval_int("s1"), 5);
    assert_eq!(env.eval_int("s2"), 5);
    assert_eq!(env.eval_int("s3"), 2);
    assert!((env.eval_real("r0") - 0.0).abs() < 1e-9);
    assert!((env.eval_real("r1") + 3.0).abs() < 1e-9);
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
         var v = new VideoOverlay(new Window()); \
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
         var v = new VideoOverlay(new Window()); \
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

/// `setSegmentLoop` on a closed overlay stores its bounds verbatim and does
/// not raise.
///
/// Reference: the wrapper assigns `SegLoopStartFrame`/`SegLoopEndFrame`
/// before guarding the backend call with `if(VideoOverlay)`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:357-361`).
#[test]
fn segment_loop_on_a_closed_overlay_is_stored() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var v = new VideoOverlay(new Window()); \
         v.setSegmentLoop(-1, 5); \
         var s0 = v.segmentLoopStartFrame; var s1 = v.segmentLoopEndFrame; \
         v.cancelSegmentLoop(); \
         var s2 = v.segmentLoopStartFrame; var s3 = v.segmentLoopEndFrame;",
    );
    assert_eq!(env.eval_int("s0"), -1);
    assert_eq!(env.eval_int("s1"), 5);
    assert_eq!(env.eval_int("s2"), -1);
    assert_eq!(env.eval_int("s3"), -1);
}

/// Backend-passthrough members: with no stream open their getters report the
/// reference's "no backend" sentinels and their setters are inert; once a
/// stream is open the volume/balance round-trip and the stream selectors
/// follow the decoder's single stream.
///
/// Reference: `GetAudioBalance`/`GetAudioVolume` seed the local with `0`,
/// `GetEnableAudioStreamNum`/`GetEnableVideoStreamNum` seed it with `-1`, and
/// every setter is guarded by `if(VideoOverlay)`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:814-880`). A selection
/// index outside the stream count is ignored: the reference's stream lookup
/// returns the invalid stream and the caller skips the switch
/// (`reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:1346-1356`). The mixing
/// movie alpha/background are stubbed by the FFmpeg backend to `1.0` and
/// `0xFF000000` while open (`reference/cpp/core/movie/ffmpeg/KRMoviePlayer.h:112-118`).
#[test]
fn passthrough_members_follow_the_open_state() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(new Window()); \
         v.mode = 1; var mode = v.mode; \
         v.playRate = 2.0; var closed_rate = v.playRate; \
         v.audioBalance = 10; v.audioVolume = 20; \
         v.enabledAudioStream = 1; v.enabledVideoStream = 2; \
         v.mixingMovieAlpha = 0.5; v.mixingMovieBGColor = 0x00ff00; \
         v.selectAudioStream(3); \
         var closed_ab = v.audioBalance; var closed_av = v.audioVolume; \
         var closed_eas = v.enabledAudioStream; var closed_evs = v.enabledVideoStream; \
         var closed_mma = v.mixingMovieAlpha; var closed_mmb = v.mixingMovieBGColor; \
         v.open('movie.mpg'); \
         v.audioBalance = 10; v.audioVolume = 20; \
         v.enabledAudioStream = 1; v.enabledVideoStream = 0; \
         v.mixingMovieAlpha = 0.5; v.mixingMovieBGColor = 0x00ff00; \
         var ab = v.audioBalance; var av = v.audioVolume; \
         var eas = v.enabledAudioStream; var evs = v.enabledVideoStream; \
         v.selectAudioStream(2); var eas2 = v.enabledAudioStream; \
         var mma = v.mixingMovieAlpha; var mmb = v.mixingMovieBGColor;",
    );
    assert_eq!(env.eval_int("mode"), 1);
    assert!((env.eval_real("closed_rate") - 0.0).abs() < 1e-9);
    assert_eq!(env.eval_int("closed_ab"), 0);
    assert_eq!(env.eval_int("closed_av"), 0);
    assert_eq!(env.eval_int("closed_eas"), -1);
    assert_eq!(env.eval_int("closed_evs"), -1);
    assert!((env.eval_real("closed_mma") - 0.0).abs() < 1e-9);
    assert_eq!(env.eval_int("closed_mmb"), 0);
    assert_eq!(env.eval_int("ab"), 10);
    assert_eq!(env.eval_int("av"), 20);
    // The single-stream movie keeps its decoder selection (`0`); both an
    // out-of-range selector and the same-stream write are no-ops.
    assert_eq!(env.eval_int("eas"), 0);
    assert_eq!(env.eval_int("evs"), 0);
    assert_eq!(env.eval_int("eas2"), 0);
    assert!((env.eval_real("mma") - 1.0).abs() < 1e-9);
    assert_eq!(env.eval_int("mmb"), 0xFF00_0000);
}

/// Color adjustment is stubbed by the in-tree FFmpeg backend: every getter
/// reads `-1.0` whether or not a stream is open, and every setter is inert.
///
/// Reference: `KRMoviePlayer.h:130-168` leaves every `Get*` empty, so the
/// wrapper's `ret = -1.0f` seed survives even while a stream is open
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:979-1112`); every `Set*`
/// is equally a no-op.
#[test]
fn color_adjustment_is_stubbed_by_the_backend() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(new Window()); \
         v.contrast = 1.5; \
         var closed_c = v.contrast; var closed_cmin = v.contrastRangeMin; \
         var closed_cdef = v.contrastDefaultValue; \
         v.open('movie.mpg'); \
         v.contrast = 1.5; v.brightness = -0.5; v.hue = 90; v.saturation = 0.25; \
         var c = v.contrast; var b = v.brightness; var h = v.hue; var s = v.saturation; \
         var cmin = v.contrastRangeMin; var cmax = v.contrastRangeMax; \
         var cdef = v.contrastDefaultValue; var cstep = v.contrastStepSize; \
         var bmin = v.brightnessRangeMin; var bmax = v.brightnessRangeMax; \
         var bdef = v.brightnessDefaultValue; var bstep = v.brightnessStepSize; \
         var hmin = v.hueRangeMin; var hmax = v.hueRangeMax; \
         var hdef = v.hueDefaultValue; var hstep = v.hueStepSize; \
         var smin = v.saturationRangeMin; var smax = v.saturationRangeMax; \
         var sdef = v.saturationDefaultValue; var sstep = v.saturationStepSize;",
    );
    for value in [
        "closed_c",
        "closed_cmin",
        "closed_cdef",
        "c",
        "b",
        "h",
        "s",
        "cmin",
        "cmax",
        "cdef",
        "cstep",
        "bmin",
        "bmax",
        "bdef",
        "bstep",
        "hmin",
        "hmax",
        "hdef",
        "hstep",
        "smin",
        "smax",
        "sdef",
        "sstep",
    ] {
        assert!(
            (env.eval_real(value) + 1.0).abs() < 1e-9,
            "{value} must read the backend's -1.0 seed"
        );
    }
}

/// Every registered member is reachable from a script (the constructor plus
/// the overridable event hooks included).
#[test]
fn every_member_is_reachable() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(new Window()); \
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
        "var v = new VideoOverlay(new Window()); v.mode = 1; v.layer1 = l; \
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
        "var v = new VideoOverlay(new Window()); v.layer1 = l; \
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
        "var v = new VideoOverlay(new Window()); v.mode = 2; v.setMixingLayer(l); \
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
    env.run("var v = new VideoOverlay(new Window()); v.open('movie.mpg'); v.play();");
    let ctx = env.host.ctx().expect("bound context");
    compat::video_advance(&ctx, 50.0).expect("advance");
    assert_eq!(env.eval_int("v.position"), 50);
    compat::video_advance(&ctx, 150.0).expect("advance");
    assert_eq!(env.eval_int("v.position"), 200);
}

/// `onStatusChanged` fires synchronously from `open`/`close`/`play`/`pause`/
/// `stop`.
///
/// Reference: `SetStatus` posts the event with `TVP_EPT_IMMEDIATE`
/// (`reference/cpp/core/visual/VideoOvlIntf.cpp:91-110`), which is delivered
/// inside the posting call (`reference/cpp/core/visual/EventIntf.cpp:217-231`),
/// and `open`/`close` end in `SetStatus(ssStop)`/`SetStatus(ssUnload)`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:241,265`).
#[test]
fn status_events_fire_synchronously() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(new Window()); \
         var log = ''; \
         v.onStatusChanged = function(s) { log += s + ','; }; \
         v.open('movie.mpg'); var after_open = log; \
         v.play(); var after_play = log; \
         v.pause(); var after_pause = log; \
         v.stop(); var after_stop = log; \
         v.close(); var after_close = log;",
    );
    assert_eq!(env.eval_string("after_open"), "stop,");
    assert_eq!(env.eval_string("after_play"), "stop,play,");
    assert_eq!(env.eval_string("after_pause"), "stop,play,pause,");
    assert_eq!(env.eval_string("after_stop"), "stop,play,pause,stop,");
    assert_eq!(
        env.eval_string("after_close"),
        "stop,play,pause,stop,unload,"
    );
}

/// `prepare` acts only in `vomLayer` mode and fires `perPrepare` on the first
/// decoded frame.
///
/// Reference: `Prepare()` is guarded by `Mode == vomLayer`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:349-355`); `EC_UPDATE`
/// fires `perPrepare` then pauses and rewinds (`:635-640`).
#[test]
fn prepare_only_acts_in_layer_mode() {
    let _lock = vm_lock();
    let env = Env::new();
    env.host
        .set_video_source(movie_spec(), vec![solid_frame([1, 2, 3, 255])]);
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.name = 'prep'; l.visible = true; \
         var a = new VideoOverlay(w); \
         var ap = ''; a.onPeriod = function(r) { ap += r + ','; }; \
         a.open('movie.mpg'); a.prepare(); \
         var b = new VideoOverlay(w); b.mode = 1; b.layer1 = l; \
         var bp = ''; b.onPeriod = function(r) { bp += r + ','; }; \
         b.open('movie.mpg'); b.prepare();",
    );
    let ctx = env.host.ctx().expect("bound context");
    compat::video_advance(&ctx, 50.0).expect("advance");
    assert_eq!(
        env.eval_string("ap"),
        "",
        "vomOverlay prepare must be a silent no-op"
    );
    assert_eq!(
        env.eval_string("bp"),
        "2,",
        "vomLayer prepare fires perPrepare"
    );
    assert_eq!(
        env.eval_int("b.position"),
        0,
        "prepare ends paused at frame 0"
    );
}

/// A `mode` write after `open` is ignored.
///
/// Reference: `SetMode` writes only when no backend exists
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:794-799`).
#[test]
fn mode_is_frozen_after_open() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var v = new VideoOverlay(new Window()); \
         v.mode = 2; var before = v.mode; \
         v.open('movie.mpg'); \
         v.mode = 1; var after = v.mode;",
    );
    assert_eq!(env.eval_int("before"), 2);
    assert_eq!(env.eval_int("after"), 2);
}

/// In `vomLayer` mode `setPos`/`left`/`top` move the bound layers; the size
/// setters are no-ops and the getters read the wrapper's own stale rectangle.
///
/// Reference: `SetPosition`/`SetLeft`/`SetTop` move `Layer1`/`Layer2`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:392-442`);
/// `SetSize`/`SetWidth`/`SetHeight`/`SetBounds` return immediately (`:404-458`);
/// the getters read `Rect` (`reference/cpp/core/visual/impl/VideoOvlImpl.h:118-133`).
#[test]
fn layer_mode_geometry_targets_the_bound_layers() {
    let _lock = vm_lock();
    let env = Env::new();
    env.set_default_source();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.name = 'vgeom'; l.visible = true; \
         var v = new VideoOverlay(w); v.mode = 1; v.layer1 = l; \
         v.open('movie.mpg');",
    );
    let id = env.layer_id("vgeom");
    let before = env._scene.layer(id).expect("layer").rect;
    env.run(
        "v.setPos(5, 6); v.left = 7; \
         var wrapper_left = v.left; var wrapper_top = v.top; \
         v.setSize(100, 100); v.width = 50; \
         var wrapper_width = v.width; var wrapper_height = v.height;",
    );
    let rect = env._scene.layer(id).expect("layer").rect;
    assert_eq!(rect.x, 7, "setPos/left move the bound layer");
    assert_eq!(rect.y, 6);
    assert_eq!(
        (rect.w, rect.h),
        (before.w, before.h),
        "size setters are no-ops"
    );
    assert_eq!(env.eval_int("wrapper_left"), 0);
    assert_eq!(env.eval_int("wrapper_top"), 0);
    assert_eq!(env.eval_int("wrapper_width"), 320);
    assert_eq!(env.eval_int("wrapper_height"), 240);
}

/// The constructor requires a live `Window` argument.
///
/// Reference: `Construct` returns `TJS_E_BADPARAMCOUNT` for a missing argument
/// and throws `TVPSpecifyWindow` for a null/non-`Window` object
/// (`reference/cpp/core/visual/VideoOvlIntf.cpp:38-55`).
#[test]
fn constructor_requires_a_window() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var thrown = 0; \
         try { new VideoOverlay(); } catch (e) { thrown++; } \
         try { new VideoOverlay(null); } catch (e) { thrown++; } \
         try { new VideoOverlay(%[foo: 1]); } catch (e) { thrown++; } \
         var w = new Window(); var ok = 0; \
         try { new VideoOverlay(w); ok = 1; } catch (e) {}",
    );
    assert_eq!(env.eval_int("thrown"), 3);
    assert_eq!(env.eval_int("ok"), 1);
}

/// The `vomMixer` composite uses the mixing layer's opacity.
///
/// Reference: `SetMixingLayer` passes `opacity/255` to `SetMixingBitmap`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:882-913`).
#[test]
fn mixer_mode_applies_the_layer_opacity() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.name = 'mixop'; l.visible = true; \
         l.opacity = 128; l.setImageSize(4, 4);",
    );
    env.host
        .set_video_source(movie_spec(), vec![solid_frame([0, 255, 0, 255])]);
    env.run(
        "var v = new VideoOverlay(new Window()); v.mode = 2; v.setMixingLayer(l); \
         v.open('movie.mpg'); v.play();",
    );
    let ctx = env.host.ctx().expect("bound context");
    compat::video_advance(&ctx, 50.0).expect("advance");
    assert_eq!(env.bitmap_color("mixop", 1, 1), [0, 255, 0, 128]);
}

/// A host-decoded source that also supplies an audio track, so the seam's
/// `audio` path can be driven without a real movie file.
struct AudioSource {
    spec: VideoSpec,
    frames: Vec<RgbaFrame>,
    audio: DecodedAudio,
}

impl compat::VideoSource for AudioSource {
    fn spec(&self, _path: &str) -> Option<VideoSpec> {
        Some(self.spec)
    }

    fn frame_at(&mut self, _path: &str, at_ms: i64) -> Option<RgbaFrame> {
        let mut chosen = self.frames.first()?.clone();
        for frame in &self.frames {
            if frame.pts_ms as i64 <= at_ms {
                chosen = frame.clone();
            } else {
                break;
            }
        }
        Some(chosen)
    }

    fn audio(&mut self, _path: &str) -> Result<Option<Arc<DecodedAudio>>, MediaError> {
        Ok(Some(Arc::new(self.audio.clone())))
    }
}

/// The closed overlay reports the reference's zeroed audio state.
///
/// Reference: `GetAudioVolume`/`GetAudioBalance` return `0` while no backend
/// exists and `GetNumberOfAudioStream` returns `0`
/// (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:814-841`).
#[test]
fn audio_members_are_zeroed_before_open() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var v = new VideoOverlay(new Window()); \
         var av = v.audioVolume; var ab = v.audioBalance; \
         var na = v.numberOfAudioStream; var nv = v.numberOfVideoStream; \
         var ea = v.enabledAudioStream; var ev = v.enabledVideoStream;",
    );
    assert_eq!(env.eval_int("av"), 0);
    assert_eq!(env.eval_int("ab"), 0);
    assert_eq!(env.eval_int("na"), 0);
    assert_eq!(env.eval_int("nv"), 0);
    assert_eq!(env.eval_int("ea"), -1);
    assert_eq!(env.eval_int("ev"), -1);
}

/// `open` reports the decoder's real stream counts and selection, and the
/// decoded audio starts at the device's unity volume and centre pan.
///
/// Reference: `GetNumberOfAudioStream`/`GetEnableAudioStreamNum` read
/// `m_pPlayer->GetAudioStreamCount()`/`GetAudioStream()`
/// (`reference/cpp/core/movie/ffmpeg/KRMoviePlayer.cpp:119-131`); the sound
/// buffer's defaults are `_volume = 1` and `_pan = 0`
/// (`reference/cpp/core/sound/win32/WaveMixer.cpp:124-126`), which the wrapper
/// scales by `100000` (`reference/cpp/core/visual/impl/VideoOvlImpl.cpp:826-836`).
#[test]
fn open_reports_audio_stream_counts_and_default_gain() {
    let _lock = vm_lock();
    let env = Env::new();
    let ctx = env.host.ctx().expect("bound context");
    compat::set_video_source(
        &ctx,
        Box::new(AudioSource {
            spec: VideoSpec {
                audio_streams: 2,
                ..default_spec()
            },
            frames: default_frames(),
            audio: DecodedAudio::new(
                AudioSpec {
                    sample_rate: 48_000,
                    channels: 2,
                    total_frames: Some(1000),
                },
                vec![0.5; 2000],
            ),
        }),
    );
    env.run("var v = new VideoOverlay(new Window()); v.open('movie.mpg');");
    assert_eq!(env.eval_int("v.numberOfAudioStream"), 2);
    assert_eq!(env.eval_int("v.numberOfVideoStream"), 1);
    assert_eq!(env.eval_int("v.enabledAudioStream"), 0);
    assert_eq!(env.eval_int("v.enabledVideoStream"), 0);
    assert_eq!(env.eval_int("v.audioVolume"), 100_000);
    assert_eq!(env.eval_int("v.audioBalance"), 0);
}

/// A selection index inside the container's stream count but different from
/// the decoder's single opened stream is a typed error, not a silent lie; an
/// out-of-range index is ignored exactly as the reference ignores it.
///
/// Reference: `SelectAudioStream` looks the index up with
/// `m_SelectionStreams.Get` and switches only when the result is a real
/// stream (`reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:1346-1356`); this
/// decoder opens one stream, so a different in-range index cannot be honoured.
#[test]
fn selecting_another_audio_stream_raises() {
    let _lock = vm_lock();
    let env = Env::new();
    let ctx = env.host.ctx().expect("bound context");
    compat::set_video_source(
        &ctx,
        Box::new(AudioSource {
            spec: VideoSpec {
                audio_streams: 2,
                ..default_spec()
            },
            frames: default_frames(),
            audio: DecodedAudio::new(
                AudioSpec {
                    sample_rate: 48_000,
                    channels: 2,
                    total_frames: Some(1000),
                },
                vec![0.5; 2000],
            ),
        }),
    );
    env.run(
        "var v = new VideoOverlay(new Window()); v.open('movie.mpg'); \
         v.selectAudioStream(5); var ignored = v.enabledAudioStream; \
         var thrown = 0; \
         try { v.selectAudioStream(1); } catch (e) { thrown = 1; } \
         var still = v.enabledAudioStream;",
    );
    assert_eq!(env.eval_int("ignored"), 0);
    assert_eq!(env.eval_int("thrown"), 1);
    assert_eq!(env.eval_int("still"), 0);
}

/// The decoded movie audio is mixed into the shared mixer and honours the TVP
/// volume/pan scale.
///
/// Reference: the movie player passes the script value as
/// `volume / 100000`/`balance / 100000` to the sound buffer
/// (`reference/cpp/core/movie/ffmpeg/KRMoviePlayer.cpp:93-117`), whose mixer
/// gain law silences one side at full pan
/// (`reference/cpp/core/sound/win32/WaveMixer.cpp:127-137`).
#[test]
fn movie_audio_reaches_the_mixer_with_volume_and_pan() {
    let _lock = vm_lock();
    let env = Env::new();
    let ctx = env.host.ctx().expect("bound context");
    compat::set_video_source(
        &ctx,
        Box::new(AudioSource {
            spec: default_spec(),
            frames: default_frames(),
            audio: DecodedAudio::new(
                AudioSpec {
                    sample_rate: 48_000,
                    channels: 2,
                    total_frames: Some(1000),
                },
                vec![0.5; 2000],
            ),
        }),
    );
    env.run("var v = new VideoOverlay(new Window()); v.open('movie.mpg'); v.play();");
    compat::video_advance(&ctx, 10.0).expect("advance");

    let out_spec = AudioSpec {
        sample_rate: 48_000,
        channels: 2,
        total_frames: None,
    };
    let mut out = vec![0.0f32; 200];
    compat::sound_render(&ctx, &mut out, out_spec).expect("render");
    assert!(
        out.iter().any(|sample| sample.abs() > 0.0),
        "movie audio is silent"
    );

    env.run("v.audioBalance = 100000;");
    let mut panned = vec![0.0f32; 200];
    compat::sound_render(&ctx, &mut panned, out_spec).expect("render");
    assert!(
        panned[0].abs() < 1e-6,
        "full right pan must silence the left channel"
    );
    assert!(panned[1].abs() > 0.0);

    env.run("v.audioVolume = 0;");
    let mut muted = vec![0.0f32; 200];
    compat::sound_render(&ctx, &mut muted, out_spec).expect("render");
    assert!(muted.iter().all(|sample| sample.abs() < 1e-6));
}

/// A ramp audio track: every sample is one step above the one before it.
fn ramp_audio(rate: u32, seconds: f64, step: f32) -> DecodedAudio {
    let frames = (rate as f64 * seconds) as usize;
    let mut interleaved = Vec::with_capacity(frames);
    for frame in 0..frames {
        interleaved.push(frame as f32 * step);
    }
    DecodedAudio::new(
        AudioSpec {
            sample_rate: rate,
            channels: 1,
            total_frames: Some(frames as u64),
        },
        interleaved,
    )
}

/// The movie's audio voice keeps the mixer's own clock, so the samples the
/// device renders follow each other without a step between them.
///
/// The voice used to be placed on the picture's position on every frame.
/// The picture's clock was a rounded millisecond accumulator, so it drifted
/// away from the audio the device had rendered and every frame skipped a
/// frame's worth of samples: a cut in the middle of the waveform, once per
/// frame. The reference never moves the movie audio's play head; the
/// decoded frames are appended to a sound buffer the device plays at its own
/// rate (`reference/cpp/core/movie/ffmpeg/AEFactory.cpp:210-219`).
#[test]
fn the_movie_audio_stays_continuous_across_polls() {
    /// The output rate, and the ramp's rate.
    const RATE: u32 = 48_000;
    /// The ramp's step per sample; the whole track spans one unit.
    const STEP: f32 = 1.0 / 576_000.0;

    let _lock = vm_lock();
    let env = Env::new();
    let ctx = env.host.ctx().expect("bound context");
    // The host mixes on the frame loop, so the mixer advances by the frames
    // it renders.
    compat::set_sound_render_driven(env.host.services(), true);
    compat::set_video_source(
        &ctx,
        Box::new(AudioSource {
            spec: VideoSpec {
                sample_rate: RATE,
                channels: 1,
                // Long enough that the movie does not end while the poll runs.
                total_frames: Some(3000),
                ..default_spec()
            },
            frames: default_frames(),
            audio: ramp_audio(RATE, 12.0, STEP),
        }),
    );
    env.run("var v = new VideoOverlay(new Window()); v.open('movie.mpg'); v.play();");

    let out_spec = AudioSpec {
        sample_rate: RATE,
        channels: 1,
        total_frames: None,
    };
    // One 60 Hz frame at 48 kHz.
    let mut out = vec![0.0f32; 800];
    // The resampler's kernel reaches a few frames back, so the first samples of
    // a track are interpolated with a truncated kernel. Render one chunk
    // before the measurements start.
    compat::sound_render(&ctx, &mut out, out_spec).expect("render");
    let mut previous: Option<f32> = None;
    for _ in 0..600 {
        compat::video_advance(&ctx, 1000.0 / 60.0).expect("advance");
        compat::sound_render(&ctx, &mut out, out_spec).expect("render");
        for sample in &out {
            if let Some(last) = previous {
                let delta = sample - last;
                assert!(delta > 0.0, "the ramp did not rise: {delta} after {last}");
                assert!(
                    delta < STEP * 2.0,
                    "the ramp jumped: {delta} instead of {STEP}"
                );
            }
            previous = Some(*sample);
        }
    }
}

/// The picture follows the sound: the overlay's position is the position the
/// voice reached at the listener, not the wall clock.
///
/// The reference schedules the picture against the audio device's clock
/// (`reference/cpp/core/movie/ffmpeg/VideoPlayerAudio.cpp:264`), so the
/// sound drives the picture and not the other way round.
#[test]
fn the_picture_follows_the_sound() {
    /// The output rate, and the sound's rate.
    const RATE: u32 = 48_000;
    /// One 60 Hz frame at 48 kHz.
    const FRAMES: usize = 800;

    let _lock = vm_lock();
    let env = Env::new();
    let ctx = env.host.ctx().expect("bound context");
    // The host mixes on the frame loop, so the mixer advances by the frames
    // it renders.
    compat::set_sound_render_driven(env.host.services(), true);
    compat::set_video_source(
        &ctx,
        Box::new(AudioSource {
            spec: VideoSpec {
                sample_rate: RATE,
                channels: 1,
                // Long enough that the movie does not end while the poll runs.
                total_frames: Some(3000),
                ..default_spec()
            },
            frames: default_frames(),
            audio: ramp_audio(RATE, 12.0, 1.0 / 576_000.0),
        }),
    );
    env.run("var v = new VideoOverlay(new Window()); v.open('movie.mpg'); v.play();");

    let out_spec = AudioSpec {
        sample_rate: RATE,
        channels: 1,
        total_frames: None,
    };
    let mut out = vec![0.0f32; FRAMES];
    for _ in 0..10 {
        compat::video_advance(&ctx, 1000.0 / 60.0).expect("advance");
        compat::sound_render(&ctx, &mut out, out_spec).expect("render");
    }

    // The sound has rendered ten chunks of 800 frames, so it has reached
    // 8000 frames (166.667 ms). The picture follows the sound one frame
    // behind it: the sound of the frame being rendered has not reached the
    // mixer yet.
    assert_eq!(env.eval_int("v.position"), 150);
}
