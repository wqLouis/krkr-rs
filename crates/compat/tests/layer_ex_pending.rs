//! The `layer_ex` plugin family and the pending-image model: a member that
//! touches a bitmap's pixels waits for the decode that has not landed yet.
//!
//! The environment attaches an L2 [`Scene`], a [`Storage`] and an L2
//! [`DecodePool`], the same seams the app installs, so a load leaves the
//! image pending and the members that read or write its pixels run against
//! the unresolved state.
//!
//! The reference decodes every image before the load returns
//! (`reference/cpp/core/visual/LayerIntf.cpp:2850`,
//! `reference/cpp/core/visual/BitmapIntf.cpp:105`), so a member that touches
//! the pixels always sees the decoded image.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use api::media::{DecodedImage, ImageInfo};
use compat::CompatHost;
use engine::imagedecode::{DecodePool, DecodePoolConfig, ImageDecodeError, ImageDecoder};
use engine::input::InputState;
use engine::timer::TimerState;
use engine::{BitmapState, Scene, Storage};
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// The image the synthetic codec and the pool's decoder both produce.
fn image() -> DecodedImage {
    DecodedImage::new(
        2,
        2,
        vec![
            10, 20, 30, 255, 40, 50, 60, 255, 70, 80, 90, 255, 100, 110, 120, 255,
        ],
    )
}

/// The RGB value of the first pixel of [`image`], as `Bitmap.getPixel`
/// reports it: that member masks the alpha away.
const FIRST_PIXEL: i64 = 0x0A_141E;

/// The decoder the pool drives: every decode produces [`image`], unless the
/// decoder was built to fail.
struct TestDecoder {
    /// Whether every decode fails.
    fail: bool,
}

impl ImageDecoder for TestDecoder {
    fn probe(&self, _bytes: &[u8], _hint: Option<&str>) -> Result<ImageInfo, ImageDecodeError> {
        Ok(ImageInfo {
            width: image().width,
            height: image().height,
            has_alpha: true,
            format: Some("img".to_string()),
        })
    }

    fn decode(&self, _bytes: &[u8], _hint: Option<&str>) -> Result<DecodedImage, ImageDecodeError> {
        if self.fail {
            return Err(ImageDecodeError::Failed {
                message: "the picture is broken".to_string(),
            });
        }
        Ok(image())
    }
}

/// A bound VM + host + the attached scene, storage, timer, input and the
/// decode pool.
///
/// Fields drop in declaration order: the host first (releasing every retained
/// script object), then the engine, the scene, the storage, the input state,
/// the timer registry and the pool.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    scene: Box<Scene>,
    _storage: Box<Storage>,
    _input: Box<InputState>,
    _timer: Box<TimerState>,
    /// Kept alive for the host's raw pointer; never read directly.
    _pool: Box<DecodePool>,
    /// The game directory the storage is mounted from; removed on drop.
    _game_dir: PathBuf,
}

impl Env {
    /// An environment with a decode pool: every load leaves the image
    /// pending.
    fn with_pool() -> Env {
        Self::build(false)
    }

    /// An environment whose pool decodes fail. The header probe still
    /// succeeds, so the load returns and the failure surfaces on the worker.
    fn with_failing_pool() -> Env {
        Self::build(true)
    }

    fn build(fail: bool) -> Env {
        let game_dir = unique_dir("krkr-layer-ex-pending-test");
        std::fs::create_dir_all(&game_dir).expect("create temp game dir");
        std::fs::write(game_dir.join("picture.png"), b"not-a-real-image")
            .expect("write the picture");
        let mut storage = Box::new(
            Storage::mount_archives(
                FileSystem::host(&game_dir),
                &game_dir,
                std::iter::empty::<&str>(),
            )
            .expect("mount storage"),
        );

        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut timer = Box::new(TimerState::new());
        let mut input = Box::new(InputState::new());
        let mut host = CompatHost::new();
        host.set_image_codec(
            |_bytes, _hint| Ok(image()),
            |_bytes, _hint| {
                Ok(ImageInfo {
                    width: image().width,
                    height: image().height,
                    has_alpha: true,
                    format: Some("img".to_string()),
                })
            },
            |_name, _type, _width, _height, _rgba| Ok(()),
        );
        let mut pool = Box::new(
            DecodePool::new(Arc::new(TestDecoder { fail }), config())
                .expect("start the decode pool"),
        );
        // SAFETY: the boxed scene, storage, timer registry, input state and
        // pool outlive the host (they are declared after `_host`).
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
            host.services_mut()
                .attach_input(input.as_mut() as *mut InputState);
            host.services_mut()
                .attach_decode_pool(pool.as_mut() as *mut DecodePool);
        }
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            scene,
            _storage: storage,
            _input: input,
            _timer: timer,
            _pool: pool,
            _game_dir: game_dir,
        }
    }

    /// Runs a script, panicking when it raises.
    fn run(&self, script: &str) {
        self.engine
            .exec_script_discard(script, "layer-ex-pending-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    /// Evaluates an expression that yields an integer.
    fn eval_int(&self, expression: &str) -> i64 {
        match self
            .engine
            .eval(expression, "layer-ex-pending-test")
            .map_err(|error| error.to_string())
        {
            Ok(TjsValue::Integer(value)) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    /// The scene id of the layer the script named `name`.
    ///
    /// The reference surface has no `Layer.id` member, so the tests name the
    /// layers they create and the id comes from the Rust-side scene.
    fn layer_id(&self, name: &str) -> u32 {
        self.scene
            .layers
            .iter()
            .find(|layer| layer.name == name)
            .unwrap_or_else(|| panic!("no scene layer named {name:?}"))
            .id
    }

    /// The straight-alpha RGBA pixel at `(x, y)` of a layer's MainImage.
    fn pixel(&self, name: &str, x: u32, y: u32) -> [u8; 4] {
        let bitmap_id = self
            .scene
            .layer(self.layer_id(name))
            .and_then(|layer| layer.bitmap)
            .expect("the layer has a MainImage");
        let bitmap = self.scene.bitmap(bitmap_id).expect("the bitmap exists");
        let offset = bitmap.pixel_offset(x, y).expect("the pixel is in range");
        [
            bitmap.rgba[offset],
            bitmap.rgba[offset + 1],
            bitmap.rgba[offset + 2],
            bitmap.rgba[offset + 3],
        ]
    }

    /// The number of scene bitmaps whose decode has not landed yet.
    fn pending_count(&self) -> usize {
        self.scene
            .bitmaps
            .iter()
            .filter(|bitmap| bitmap.pending.is_some())
            .count()
    }

    /// The scene bitmap of the script `Bitmap` the script created.
    ///
    /// The reference surface has no `Bitmap.id` member, so the test finds the
    /// bitmap by its size: the script bitmap is the one whose image is the
    /// decoded test image.
    fn script_bitmap(&self) -> &BitmapState {
        self.scene
            .bitmaps
            .iter()
            .find(|bitmap| bitmap.width == image().width && bitmap.height == image().height)
            .expect("the script bitmap exists")
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self._game_dir);
    }
}

/// A unique temporary directory for one test.
fn unique_dir(tag: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    dir
}

/// A pool configuration with one worker.
fn config() -> DecodePoolConfig {
    DecodePoolConfig {
        workers: 1,
        queue_limit: 8,
        memory_limit: 1024 * 1024,
    }
}

/// A pending source is resolved before `Layer.drawImage` copies it.
///
/// `Layer.drawImage` blits the source image into the layer's MainImage
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2319-2348`),
/// and the source is a finished image: the reference decodes it before the
/// load returns (`reference/cpp/core/visual/BitmapIntf.cpp:105`).
#[test]
fn a_pending_source_is_resolved_before_draw_image_copies_it() {
    let _lock = common::vm_lock();
    let env = Env::with_pool();
    env.run(
        "var w = new Window(); \
         var dst = new Layer(w, null); \
         dst.name = 'dst'; \
         dst.setImageSize(4, 4); \
         var b = new Bitmap('picture.png');",
    );
    // The load returned with the image's size known and its pixels still
    // decoding.
    assert_eq!(env.pending_count(), 1, "the load left the image pending");
    env.run("dst.drawImage(0, 0, b);");
    // The destination holds the decoded source pixel.
    assert_eq!(
        env.pixel("dst", 0, 0),
        [10, 20, 30, 255],
        "the copied pixel is the decoded one"
    );
    // The copy reaches the whole source rectangle.
    assert_eq!(
        env.pixel("dst", 1, 1),
        [100, 110, 120, 255],
        "the whole source rectangle is copied"
    );
}

/// A pending layer image is resolved before `Layer.clear` writes into it.
///
/// `LayerExDraw::clear` fills the whole image with the colour
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:1623-1633`),
/// and the image it fills is a finished one: the reference decodes it before
/// the load returns (`reference/cpp/core/visual/LayerIntf.cpp:2850`).
#[test]
fn a_pending_layer_image_is_resolved_before_clear_writes_it() {
    let _lock = common::vm_lock();
    let env = Env::with_pool();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.name = 'l'; \
         l.setImageSize(4, 4); \
         l.loadImages('picture.png');",
    );
    assert_eq!(env.pending_count(), 1, "the load left the image pending");
    env.run("l.clear(0xFF0000FF);");
    // The whole image carries the clear colour.
    assert_eq!(
        env.pixel("l", 0, 0),
        [0, 0, 255, 255],
        "the cleared pixel is the fill colour"
    );
    assert_eq!(
        env.pixel("l", 1, 1),
        [0, 0, 255, 255],
        "the whole image is cleared"
    );
}

/// A pending source is resolved before `Layer.perspectiveCopy` reads it.
///
/// The reference copies the source layer's MainImage texture
/// (`reference/cpp/plugins/layerExPerspective.cpp:199-201`), and that image is
/// a finished one: the reference decodes it before the load returns
/// (`reference/cpp/core/visual/LayerIntf.cpp:2850`).
#[test]
fn a_pending_source_is_resolved_before_perspective_copy_reads_it() {
    let _lock = common::vm_lock();
    let env = Env::with_pool();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.name = 'src'; \
         var dst = new Layer(w, null); \
         dst.name = 'dst'; \
         src.setImageSize(2, 2); \
         dst.setImageSize(2, 2); \
         src.loadImages('picture.png');",
    );
    assert_eq!(env.pending_count(), 1, "the load left the image pending");
    env.run("dst.perspectiveCopy(src, 0, 0, 2, 2, 0, 0, 2, 0, 0, 2, 2, 2);");
    // The destination holds the decoded source pixel.
    assert_eq!(
        env.pixel("dst", 0, 0),
        [10, 20, 30, 255],
        "the copied pixel is the decoded one"
    );
}

/// A pending destination is resolved before `Layer.perspectiveCopy` writes
/// into it.
///
/// The reference renders into the destination layer's MainImage
/// (`reference/cpp/plugins/layerExPerspective.cpp:195-219`), and that image is
/// a finished one: the reference decodes it before the load returns
/// (`reference/cpp/core/visual/LayerIntf.cpp:2850`).
#[test]
fn a_pending_destination_is_resolved_before_perspective_copy_writes_it() {
    let _lock = common::vm_lock();
    let env = Env::with_pool();
    env.run(
        "var w = new Window(); \
         var src = new Layer(w, null); \
         src.name = 'src'; \
         var dst = new Layer(w, null); \
         dst.name = 'dst'; \
         src.setImageSize(2, 2); \
         dst.setImageSize(2, 2); \
         src.fillRect(0, 0, 2, 2, 0xFFFF0000); \
         dst.loadImages('picture.png');",
    );
    assert_eq!(env.pending_count(), 1, "the load left the image pending");
    env.run("dst.perspectiveCopy(src, 0, 0, 2, 2, 0, 0, 2, 0, 0, 2, 2, 2);");
    // The destination holds the copied source pixel.
    assert_eq!(
        env.pixel("dst", 0, 0),
        [255, 0, 0, 255],
        "the copied pixel is the source one"
    );
}

/// A pending layer image survives a size change: the decoded pixels are the
/// ones the overlapping region preserves.
///
/// The reference decodes the image before the load returns
/// (`reference/cpp/core/visual/LayerIntf.cpp:2850`) and its size change
/// preserves the finished image's overlap
/// (`reference/cpp/core/visual/LayerIntf.cpp:2307`).
#[test]
fn a_pending_layer_image_survives_a_size_change() {
    let _lock = common::vm_lock();
    let env = Env::with_pool();
    env.run(
        "var w = new Window(); \
         var screen = new Layer(w, null); \
         var dst = new Layer(w, screen); \
         dst.name = 'dst'; \
         dst.loadImages('picture.png');",
    );
    assert_eq!(env.pending_count(), 1, "the load left the image pending");
    env.run("dst.setImageSize(4, 4);");
    assert_eq!(
        env.pixel("dst", 0, 0),
        [10, 20, 30, 255],
        "the decoded overlap survives the resize"
    );
    // The layer has a parent, so it keeps the constructor's transparent
    // neutral colour and the expansion is transparent
    // (`reference/cpp/core/visual/LayerIntf.cpp:332-334`). The window's
    // primary layer carries the opaque one instead
    // (`reference/cpp/core/visual/LayerIntf.cpp:469-471`).
    assert_eq!(env.pixel("dst", 3, 3)[3], 0, "the expansion is transparent");
}

/// A pending destination is resolved before `Layer.redrawRecord` replays the
/// recording into it.
///
/// `LayerExDraw::redrawRecord` re-renders the recording into the layer
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2399-2407`),
/// and the layer it renders into holds a finished image: the reference decodes
/// it before the load returns (`reference/cpp/core/visual/LayerIntf.cpp:2850`).
#[test]
fn a_pending_destination_is_resolved_before_redraw_record_writes_it() {
    let _lock = common::vm_lock();
    let env = Env::with_pool();
    env.run(
        "var w = new Window(); \
         var dst = new Layer(w, null); \
         dst.name = 'dst'; \
         dst.setImageSize(4, 4); \
         dst.record = true; \
         dst.clear(0xFFFF0000); \
         dst.loadImages('picture.png');",
    );
    assert_eq!(env.pending_count(), 1, "the load left the image pending");
    env.run("dst.redrawRecord();");
    // The replayed recording covers the whole image.
    assert_eq!(
        env.pixel("dst", 0, 0),
        [255, 0, 0, 255],
        "the replayed pixel is the recorded one"
    );
    assert_eq!(
        env.pixel("dst", 1, 1),
        [255, 0, 0, 255],
        "the whole image is replayed"
    );
}

/// The recorded blit holds the decoded source pixels.
///
/// `LayerExDraw::getRecordImage` returns the recording as an image
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2458-2467`),
/// and the recording holds the source image the blit copied
/// (`reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp:2319-2348`).
#[test]
fn the_recorded_blit_holds_the_decoded_source_pixels() {
    let _lock = common::vm_lock();
    let env = Env::with_pool();
    env.run(
        "var w = new Window(); \
         var dst = new Layer(w, null); \
         dst.name = 'dst'; \
         dst.setImageSize(4, 4); \
         var b = new Bitmap('picture.png'); \
         dst.record = true;",
    );
    assert_eq!(env.pending_count(), 1, "the load left the image pending");
    env.run("dst.drawImage(0, 0, b);");
    env.run("var rec = dst.getRecordImage();");
    // The recorded image holds the decoded source pixel.
    assert_eq!(
        env.eval_int("rec.getPixel(0, 0)"),
        FIRST_PIXEL,
        "the recorded pixel is the decoded one"
    );
}

/// The image barrier still resolves a pending image.
///
/// The barrier waits for every pending image and installs its pixels before
/// the frame draws; the pixel the script reads is the decoded one, exactly as
/// the reference's synchronous load leaves it
/// (`reference/cpp/core/visual/BitmapIntf.cpp:105`).
#[test]
fn the_image_barrier_resolves_a_pending_image() {
    let _lock = common::vm_lock();
    let mut env = Env::with_pool();
    env.run("var b = new Bitmap('picture.png');");
    assert_eq!(env.pending_count(), 1, "the load left the image pending");
    // The barrier: the engine waits for every pending image and installs its
    // pixels before the frame draws.
    let resolved = env.scene.resolve_pending_images();
    assert_eq!(resolved, 1, "the barrier resolved the pending image");
    assert_eq!(env.pending_count(), 0, "no image is pending any more");
    // The pixel is the decoded one.
    assert_eq!(
        env.eval_int("b.getPixel(0, 0)"),
        FIRST_PIXEL,
        "the pixel is the decoded one"
    );
}

/// A failed decode leaves a blank and does not raise at the member.
///
/// The reference raises the load failure at the load
/// (`reference/cpp/core/visual/BitmapIntf.cpp:105`); the deferred load has
/// already returned, so the failure surfaces as the blank the resolution
/// installs, and the member that waits for it still runs.
#[test]
fn a_failed_decode_leaves_a_blank_and_the_member_still_runs() {
    let _lock = common::vm_lock();
    let env = Env::with_failing_pool();
    env.run(
        "var w = new Window(); \
         var dst = new Layer(w, null); \
         dst.name = 'dst'; \
         dst.setImageSize(4, 4); \
         var b = new Bitmap('picture.png');",
    );
    assert_eq!(env.pending_count(), 1, "the load left the image pending");
    env.run("dst.drawImage(0, 0, b);");
    // The failed decode left a transparent blank of the probed size, so the
    // copy contributed nothing and the destination keeps its own pixels: the
    // reference's default image, transparent white
    // (`reference/cpp/core/visual/LayerIntf.cpp:104-110,2313`).
    assert_eq!(
        env.pixel("dst", 0, 0),
        [255, 255, 255, 0],
        "the failed decode contributed no pixel"
    );
    // The bitmap holds the blank the resolution installed.
    let bitmap = env.script_bitmap();
    assert_eq!(bitmap.rgba.len(), 16, "the blank has the probed size");
    assert!(
        bitmap.rgba.iter().all(|byte| *byte == 0),
        "the blank is transparent"
    );
}
