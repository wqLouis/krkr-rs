//! Integration tests for the video path with really decoded pictures.
//!
//! The pictures come from the L1 `media` decoder: the test decodes a short
//! clip whose every picture differs and hands the decoder to the host's
//! `VideoSource` seam, the way the composition root installs it
//! (`crates/app/src/video.rs`). The clock is the engine's frame clock, the
//! one the picture reads, so the test drives time instead of waiting for it.
//!
//! The clip is a committed fixture (the media crate's `tests/fixtures`), so
//! the test reads it from the repository and never runs a command line tool.
//!
//! The reference presents a picture when the clock has reached the picture's
//! own timestamp (`reference/cpp/core/movie/ffmpeg/KRMovieLayer.cpp:45-56`),
//! and the script-visible position is the player's time
//! (`reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:1812-1837`).

mod common;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use api::error::MediaError;
use api::media::{DecodedAudio, RgbaFrame, VideoSpec};
use common::vm_lock;
use compat::{CompatHost, VideoSource};
use engine::FrameClock;
use engine::Scene;
use media::{Media, VideoDecoderState, open_video, present_video, video_audio};
use tjs::{Tjs2Engine, TjsValue};

/// The directory the committed fixtures live in.
const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../platform/media/tests/fixtures"
);

/// The picture width and height of the fixture.
const SIZE: u32 = 64;

/// The frames per second of the fixture.
const FPS: f32 = 10.0;

/// The number of pictures the fixture holds.
const PICTURES: u64 = 20;

/// The picture interval of the fixture, in milliseconds.
const INTERVAL_MS: u64 = 100;

/// The path of the fixture called `name`.
fn fixture(name: &str) -> PathBuf {
    Path::new(FIXTURES).join(name)
}

/// A bound VM + host + attached scene + the engine's frame clock.
///
/// The host is declared first so it drops before the engine (releasing every
/// retained script object while the engine is alive) and the scene.
struct Env {
    host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
    /// The engine's frame clock: the picture follows it.
    clock: RefCell<Box<FrameClock>>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut clock = Box::new(FrameClock::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene and clock outlive the host (declared after
        // it).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_clock(clock.as_mut() as *mut FrameClock);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            host,
            engine,
            scene,
            clock: RefCell::new(clock),
        }
    }

    /// Move the engine's clock forward by `dt_ms` and advance the overlay.
    fn advance_video(&self, dt_ms: f64) {
        self.clock.borrow_mut().advance(dt_ms);
        let ctx = self.host.ctx().expect("bound host");
        compat::video_advance(&ctx, f64::MAX).expect("advance");
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "videorepro")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "videorepro")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    /// A checksum of the picture the named layer shows.
    ///
    /// The clip's pictures differ from each other, so a changed checksum is a
    /// new picture on the layer.
    fn layer_picture(&self, name: &str) -> u64 {
        let id = self
            .scene
            .layers
            .iter()
            .find(|layer| layer.name == name)
            .unwrap_or_else(|| panic!("the layer {name:?}"))
            .id;
        let bitmap_id = self
            .scene
            .layer(id)
            .and_then(|layer| layer.bitmap)
            .expect("the layer bitmap");
        let bitmap = self.scene.bitmap(bitmap_id).expect("bitmap state");
        let mut hash = 0xcbf2_9ce4_8422_2325u64;
        for byte in &bitmap.rgba {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        hash
    }
}

/// The host's video source: the L1 decoder behind the seam the composition
/// root installs.
///
/// The source hands the decoder's picture for the requested time, exactly as
/// the composition root's source does
/// (`crates/app/src/video.rs`, `MediaVideoSource::frame_at`).
struct DecodedSource {
    /// The stream description the decoder reported.
    spec: VideoSpec,
    /// The decoder that produces the pictures.
    decoder: VideoDecoderState,
    /// The stateless media backend the decoder belongs to.
    media: Media,
    /// The decoded audio track, when the container has one.
    audio: Option<Arc<DecodedAudio>>,
}

impl VideoSource for DecodedSource {
    fn spec(&self, _path: &str) -> Option<VideoSpec> {
        Some(self.spec)
    }

    fn frame_at(&mut self, _path: &str, at_ms: i64) -> Option<Arc<RgbaFrame>> {
        let at = at_ms.max(0) as u64;
        present_video(&self.media, &mut self.decoder, at).ok()
    }

    fn audio(&mut self, _path: &str) -> Result<Option<Arc<DecodedAudio>>, MediaError> {
        Ok(self.audio.clone())
    }
}

/// Opens the clip called `name` and returns the host's source for it.
fn decoded_source(name: &str) -> DecodedSource {
    let path = fixture(name);
    let bytes =
        std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    let media = Media::new();
    let (spec, decoder) = open_video(&media, Arc::from(bytes.into_boxed_slice()), Some(name))
        .unwrap_or_else(|error| panic!("open {}: {error}", path.display()));
    assert_eq!(spec.width, SIZE);
    assert_eq!(spec.height, SIZE);
    assert_eq!(spec.fps, FPS);
    assert_eq!(spec.total_frames, Some(PICTURES));
    let audio = video_audio(&media, &decoder).ok();
    DecodedSource {
        spec,
        decoder,
        media,
        audio,
    }
}
/// The picture position advances with the clock, and a second picture reaches
/// the layer.
///
/// The reference's position is the player's time
/// (`reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:1812-1837`), and the
/// picture is presented once the clock has reached its timestamp
/// (`reference/cpp/core/movie/ffmpeg/KRMovieLayer.cpp:45-56`). A position
/// that does not move is a frozen clip.
#[test]
fn the_picture_position_advances_with_the_clock() {
    let _lock = vm_lock();
    let env = Env::new();
    let source = decoded_source("clip.mp4");
    let ctx = env.host.ctx().expect("bound context");
    compat::set_video_source(&ctx, Box::new(source));
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.name = 'video'; l.visible = true; \
         var v = new VideoOverlay(new Window()); v.mode = 1; v.layer1 = l; \
         v.open('clip.mp4'); v.play();",
    );

    env.advance_video(INTERVAL_MS as f64);
    assert_eq!(env.eval_int("v.position"), INTERVAL_MS as i64);

    env.advance_video(INTERVAL_MS as f64);
    assert_eq!(env.eval_int("v.position"), 2 * INTERVAL_MS as i64);
}

/// The picture on the layer advances to the next picture of the clip.
///
/// The reference presents the picture whose timestamp the clock has reached
/// (`reference/cpp/core/movie/ffmpeg/KRMovieLayer.cpp:45-56`), so a layer
/// that keeps the same picture is a frozen clip.
#[test]
fn the_picture_on_the_layer_advances_to_the_next_picture() {
    let _lock = vm_lock();
    let env = Env::new();
    let source = decoded_source("clip.mp4");
    let ctx = env.host.ctx().expect("bound context");
    compat::set_video_source(&ctx, Box::new(source));
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); l.name = 'video'; l.visible = true; \
         var v = new VideoOverlay(new Window()); v.mode = 1; v.layer1 = l; \
         v.open('clip.mp4'); v.play();",
    );

    env.advance_video(INTERVAL_MS as f64);
    let first = env.layer_picture("video");

    env.advance_video(INTERVAL_MS as f64);
    let second = env.layer_picture("video");
    assert_ne!(
        first, second,
        "the layer still shows the first picture: the clip is frozen"
    );
}

/// The frame index the script reads moves with the clock.
///
/// The reference reports the current frame as the player's time times the
/// frame rate (`reference/cpp/core/movie/ffmpeg/VideoPlayer.cpp:1839-1842`).
#[test]
fn the_frame_index_advances_with_the_clock() {
    let _lock = vm_lock();
    let env = Env::new();
    let source = decoded_source("clip.mp4");
    let ctx = env.host.ctx().expect("bound context");
    compat::set_video_source(&ctx, Box::new(source));
    env.run("var v = new VideoOverlay(new Window()); v.open('clip.mp4'); v.play();");

    env.advance_video(INTERVAL_MS as f64);
    assert_eq!(env.eval_int("v.frame"), 1);
    env.advance_video(INTERVAL_MS as f64);
    assert_eq!(env.eval_int("v.frame"), 2);
}
