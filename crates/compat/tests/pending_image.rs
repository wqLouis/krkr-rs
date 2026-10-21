//! The pending-image wiring: a load the pool took, and the paths that wait
//! for it.
//!
//! The environment attaches an L2 [`Scene`], a [`Storage`] and — the point of
//! this file — an L2 [`DecodePool`], the same seam the app installs. The
//! tests then drive the real natives through the VM.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use api::media::{DecodedImage, ImageInfo};
use compat::CompatHost;
use engine::imagedecode::{DecodePool, DecodePoolConfig, ImageDecodeError, ImageDecoder};
use engine::input::InputState;
use engine::timer::TimerState;
use engine::{DecodedImageKey, DecodedImageMode, Scene, Storage};
use fs::FileSystem;
use platform::{InputEvent, PlatformEvent};
use tjs::{Tjs2Engine, TjsValue};

/// The image the synthetic codec and the pool's decoder both produce.
fn image() -> DecodedImage {
    DecodedImage::new(
        2,
        2,
        vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255],
    )
}

/// The ARGB value of the first pixel of [`image`], as `Bitmap.getPixel`
/// reports it.
const FIRST_PIXEL: i64 = 0xFF01_0203;

/// The RGB value of the first pixel of [`image`]: the colour key the keyed
/// loads use.
const COLOR_KEY: u32 = 0x01_0203;

/// The RGB value of the first pixel of [`image`], as `Layer.getMainPixel`
/// reports it: that member masks the alpha away.
const FIRST_LAYER_PIXEL: i64 = FIRST_PIXEL & 0x00ff_ffff;

/// The decoder the pool drives: every decode produces [`image`].
struct FixedDecoder;

impl ImageDecoder for FixedDecoder {
    fn probe(&self, _bytes: &[u8], _hint: Option<&str>) -> Result<ImageInfo, ImageDecodeError> {
        Ok(ImageInfo {
            width: image().width,
            height: image().height,
            has_alpha: true,
            format: Some("img".to_string()),
        })
    }

    fn decode(&self, _bytes: &[u8], _hint: Option<&str>) -> Result<DecodedImage, ImageDecodeError> {
        Ok(image())
    }
}

/// A bound VM + host + the attached scene, storage, timer, input and — when
/// the test asks for one — the decode pool.
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
    _pool: Option<Box<DecodePool>>,
    /// The game directory the storage is mounted from; removed on drop.
    _game_dir: PathBuf,
}

impl Env {
    /// An environment without a decode pool: every load decodes
    /// synchronously.
    fn new() -> Env {
        Self::build(None)
    }

    /// An environment with a decode pool of the given configuration.
    fn with_pool(config: DecodePoolConfig) -> Env {
        Self::build(Some(config))
    }

    /// An environment whose header probe rejects every image.
    ///
    /// The pool is installed: only the deferred path probes the header, so
    /// the synchronous path would never reach the failing probe.
    fn with_failing_probe() -> Env {
        Self::build_with_probe(Some(config()), true)
    }

    fn build(pool: Option<DecodePoolConfig>) -> Env {
        Self::build_with_probe(pool, false)
    }

    fn build_with_probe(pool: Option<DecodePoolConfig>, probe_fails: bool) -> Env {
        let game_dir = unique_dir("krkr-pending-test");
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
            move |_bytes, _hint| {
                if probe_fails {
                    return Err("the image is not a picture".to_string());
                }
                Ok(ImageInfo {
                    width: image().width,
                    height: image().height,
                    has_alpha: true,
                    format: Some("img".to_string()),
                })
            },
            |_name, _type, _width, _height, _rgba| Ok(()),
        );
        let mut pool = pool.map(|config| {
            Box::new(
                DecodePool::new(Arc::new(FixedDecoder), config).expect("start the decode pool"),
            )
        });
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
            if let Some(pool) = pool.as_mut() {
                host.services_mut()
                    .attach_decode_pool(pool.as_mut() as *mut DecodePool);
            }
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
        self.try_run(script)
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    /// Runs a script and reports the failure it raised.
    fn try_run(&self, script: &str) -> Result<(), String> {
        self.engine
            .exec_script(script, "pending-image-test")
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Evaluates an expression.
    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "pending-image-test")
            .map_err(|error| error.to_string())
    }

    /// Evaluates an expression that yields an integer.
    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval(expression) {
            Ok(TjsValue::Integer(value)) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    /// The layer whose image is still decoding.
    fn pending_layer(&self) -> &engine::LayerState {
        self.scene
            .layers
            .iter()
            .find(|layer| {
                layer
                    .bitmap
                    .and_then(|id| self.scene.bitmap(id))
                    .is_some_and(|bitmap| bitmap.pending.is_some())
            })
            .expect("a layer with a pending image")
    }

    /// Runs one input frame over the adapter, the sequence the frame body
    /// uses.
    fn dispatch_events(&self, events: &[PlatformEvent]) {
        let mut adapter = compat::EventAdapter::new();
        let mut ctx = self._host.ctx().expect("bound compat context");
        adapter.begin_input_frame(&mut ctx);
        for event in events {
            adapter.apply_input_event(&mut ctx, event);
            if let Some(mapped) = adapter.map(event) {
                adapter.dispatch(&mut ctx, &mapped).expect("dispatch event");
            }
        }
        adapter.end_input_frame(&mut ctx);
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

/// Without a pool the load decodes synchronously: the bitmap holds its
/// pixels the moment the constructor returns.
#[test]
fn a_load_without_a_pool_decodes_synchronously() {
    let _lock = common::vm_lock();
    let env = Env::new();
    env.run("var b = new Bitmap('picture.png');");
    assert_eq!(
        env.eval_int("b.getPixel(0, 0)"),
        FIRST_PIXEL,
        "the pixel is the decoded one"
    );
    let bitmap = env.scene.bitmaps.first().expect("the bitmap exists");
    assert_eq!(bitmap.rgba.len(), 16, "the bitmap holds its pixels");
    assert!(
        bitmap.pending.is_none(),
        "a load without a pool leaves no pending image"
    );
}

/// With a pool the load returns before the decode finishes, and the first
/// pixel read waits for it.
#[test]
fn a_pending_image_resolves_before_the_script_reads_a_pixel() {
    let _lock = common::vm_lock();
    let env = Env::with_pool(config());
    // The load returns with the image's size known and its pixels pending.
    env.run("var b = new Bitmap('picture.png');");
    let bitmap = env.scene.bitmaps.first().expect("the bitmap exists");
    assert!(bitmap.pending.is_some(), "the load left the image pending");
    assert_eq!(bitmap.width, image().width, "the probed width is known");
    assert!(bitmap.rgba.is_empty(), "the pixels have not landed yet");

    // The first pixel read waits for the decode.
    assert_eq!(
        env.eval_int("b.getPixel(0, 0)"),
        FIRST_PIXEL,
        "the pixel is the decoded one"
    );
    let bitmap = env.scene.bitmaps.first().expect("the bitmap exists");
    assert!(bitmap.pending.is_none(), "the pending image was resolved");
    assert_eq!(bitmap.rgba.len(), 16, "the bitmap holds its pixels");
}

/// A deferred decode enters the decoded-image cache, so a repeated load of
/// the same image is a cache hit instead of a second decode.
///
/// The reference consults the decoded-image cache before loading and stores
/// the decoded image under the same key
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1952-1960`).
#[test]
fn a_resolved_deferred_load_is_a_cache_hit_next_time() {
    let _lock = common::vm_lock();
    let mut env = Env::with_pool(config());
    env.scene.set_graphic_cache_limit(1024 * 1024);
    env.run("var b = new Bitmap('picture.png');");
    assert_eq!(
        env.eval_int("b.getPixel(0, 0)"),
        FIRST_PIXEL,
        "the first read resolves the decode"
    );
    let decodes = env
        ._pool
        .as_ref()
        .expect("the pool is attached")
        .stats()
        .decodes;
    assert_eq!(decodes, 1, "the first load decoded once");

    env.run("var b2 = new Bitmap('picture.png');");
    assert_eq!(
        env.eval_int("b2.getPixel(0, 0)"),
        FIRST_PIXEL,
        "the repeated load reads the cached image"
    );
    assert_eq!(
        env._pool
            .as_ref()
            .expect("the pool is attached")
            .stats()
            .decodes,
        1,
        "the cache hit did not decode the image again"
    );
    assert!(
        env.scene
            .bitmaps
            .iter()
            .filter(|bitmap| bitmap.id != 0)
            .all(|bitmap| bitmap.pending.is_none()),
        "the repeated load installs the image synchronously"
    );
}

/// The layer's image size is known before its pixels land: the size comes
/// from the header probe, not from the decode.
#[test]
fn the_image_size_is_known_before_the_pixels_land() {
    let _lock = common::vm_lock();
    let env = Env::with_pool(config());
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(10, 10); \
         l.loadImages('picture.png');",
    );
    assert_eq!(
        env.eval_int("l.imageWidth"),
        image().width as i64,
        "the image width comes from the probe"
    );
    assert_eq!(
        env.eval_int("l.imageHeight"),
        image().height as i64,
        "the image height comes from the probe"
    );
    let layer = env.pending_layer();
    assert!(
        layer.bitmap.is_some(),
        "the layer's image is still decoding"
    );
    assert_eq!(
        env.eval_int("l.getMainPixel(0, 0)"),
        FIRST_LAYER_PIXEL,
        "the pixel read waits for the decode"
    );
}

/// A header the probe rejects raises at the load, the way a load failure is
/// raised there.
#[test]
fn a_probe_failure_raises_at_the_load() {
    let _lock = common::vm_lock();
    let env = Env::with_failing_probe();
    let error = env
        .try_run(
            "var w = new Window(); \
             var l = new Layer(w, null); \
             l.setImageSize(10, 10); \
             l.loadImages('picture.png');",
        )
        .expect_err("the load raises");
    assert!(
        error.contains("not a picture"),
        "the probe's message reaches the script: {error}"
    );
}

/// A pool that refuses every job falls back to the synchronous decode: the
/// load still produces the image, and the refusal is reported.
#[test]
fn a_refused_submit_falls_back_to_the_synchronous_decode() {
    let _lock = common::vm_lock();
    let mut refusing = config();
    refusing.memory_limit = 0;
    let env = Env::with_pool(refusing);
    env.run("var b = new Bitmap('picture.png');");
    assert_eq!(
        env.eval_int("b.getPixel(0, 0)"),
        FIRST_PIXEL,
        "the pixel is the decoded one"
    );
    let bitmap = env.scene.bitmaps.first().expect("the bitmap exists");
    assert!(
        bitmap.pending.is_none(),
        "a refused job leaves no pending image"
    );
    assert_eq!(bitmap.rgba.len(), 16, "the bitmap holds its pixels");
}

/// A colour-keyed `Bitmap` load is deferred like an unkeyed one: the pool
/// runs the key on the worker, the load returns with the probe size, and the
/// first pixel read waits for the keyed pixels.
///
/// The reference passes the constructor's colour key to the load
/// (`reference/cpp/core/visual/BitmapIntf.cpp:19-28`,
/// `:98-105`) and applies it to the decoded scanlines, so the keyed pixel is
/// transparent and every other pixel is opaque
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1068-1074`).
#[test]
fn a_deferred_colour_keyed_bitmap_load_applies_the_key_on_the_worker() {
    let _lock = common::vm_lock();
    let mut env = Env::with_pool(config());
    env.scene.set_graphic_cache_limit(1024 * 1024);
    env.run(&format!("var b = new Bitmap('picture.png', {COLOR_KEY});"));
    let bitmap = env.scene.bitmaps.first().expect("the bitmap exists");
    assert!(
        bitmap.pending.is_some(),
        "the colour-keyed load was deferred"
    );
    assert!(bitmap.rgba.is_empty(), "the pixels have not landed yet");

    // The first pixel is the key and turns transparent; the second does not
    // and keeps its alpha.
    assert_eq!(
        env.eval_int("b.getPixel(0, 0)"),
        0x0001_0203,
        "the keyed pixel is transparent"
    );
    assert_eq!(
        env.eval_int("b.getPixel(1, 0)"),
        0xFF04_0506,
        "a different colour stays opaque"
    );
    let bitmap = env.scene.bitmaps.first().expect("the bitmap exists");
    assert!(bitmap.pending.is_none(), "the pending image was resolved");
    assert_eq!(bitmap.rgba[3], 0, "the keyed pixel's alpha is zero");
    assert_eq!(
        env._pool
            .as_ref()
            .expect("the pool is attached")
            .stats()
            .decodes,
        1,
        "the pool ran the keyed decode"
    );

    // The keyed image is the entry the load caches, under the keyed load key
    // (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1952-1960`).
    let key = DecodedImageKey::new("picture.png", COLOR_KEY, DecodedImageMode::Full, 0, 0);
    let cached = env
        .scene
        .lookup_decoded_image(&key)
        .expect("the keyed image is cached");
    assert_eq!(cached.rgba[3], 0, "the cache holds the keyed image");

    // A second keyed load is a cache hit: the same entry answers it, so the
    // key is not applied a second time and no new decode runs.
    env.run(&format!("var b2 = new Bitmap('picture.png', {COLOR_KEY});"));
    assert_eq!(
        env.eval_int("b2.getPixel(0, 0)"),
        0x0001_0203,
        "the cache hit installs the keyed image"
    );
    assert_eq!(
        env._pool
            .as_ref()
            .expect("the pool is attached")
            .stats()
            .decodes,
        1,
        "the repeated keyed load did not decode the image again"
    );
}

/// `Layer.loadImages` takes the same colour key and defers the same way: the
/// layer's image size comes from the probe and the pending image holds the
/// keyed decode until a read waits for it.
///
/// Reference `reference/cpp/core/visual/LayerIntf.cpp:2845-2855` (the key
/// reaches the load), `:8429-8440` (the `loadImages` member), and
/// `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1068-1074` (the keyed
/// scanline).
#[test]
fn a_deferred_colour_keyed_layer_load_applies_the_key_on_the_worker() {
    let _lock = common::vm_lock();
    let env = Env::with_pool(config());
    env.run(&format!(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(10, 10); \
         l.loadImages('picture.png', {COLOR_KEY});"
    ));
    assert_eq!(
        env.eval_int("l.imageWidth"),
        image().width as i64,
        "the image width comes from the probe"
    );
    assert_eq!(
        env.eval_int("l.imageHeight"),
        image().height as i64,
        "the image height comes from the probe"
    );
    assert!(
        env.pending_layer().bitmap.is_some(),
        "the layer's image is still decoding"
    );

    // The first pixel's alpha is the keyed zero; the second's is opaque.
    assert_eq!(
        env.eval_int("l.getMaskPixel(0, 0)"),
        0,
        "the keyed pixel is transparent"
    );
    assert_eq!(
        env.eval_int("l.getMaskPixel(1, 0)"),
        255,
        "a different colour stays opaque"
    );
    let bitmap = env
        .scene
        .layers
        .first()
        .and_then(|layer| layer.bitmap)
        .and_then(|id| env.scene.bitmap(id))
        .expect("the layer's bitmap exists");
    assert!(
        bitmap.pending.is_none(),
        "the pixel read resolved the keyed decode"
    );
    assert_eq!(bitmap.rgba[3], 0, "the keyed pixel's alpha is zero");
}

/// A province image smaller than the main image is tiled up to the main size,
/// not rejected; only an image larger than the main image is a size mismatch.
///
/// Reference `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:980-1031`
/// sizes the buffer to the requested destination and `:1092-1112` tiles the
/// smaller image into it; `LayerIntf.cpp:2893-2914` checks the resulting
/// province size against the main image.
#[test]
fn a_smaller_province_image_is_tiled_to_the_main_image() {
    let _lock = common::vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(5, 5); \
         l.loadProvinceImage('picture.png');",
    );
    // The plane is the main size, not the 2x2 image's.
    assert_eq!(env.eval_int("l.provinceImageBufferPitch"), 5);
    // The red channel of the 2x2 image (1 4 / 7 10) repeats across both axes;
    // the last column and row are the partial tile, which the reference fills
    // from the start of the source.
    assert_eq!(env.eval_int("l.getProvincePixel(0, 0)"), 1);
    assert_eq!(env.eval_int("l.getProvincePixel(1, 0)"), 4);
    assert_eq!(env.eval_int("l.getProvincePixel(2, 0)"), 1);
    assert_eq!(env.eval_int("l.getProvincePixel(3, 0)"), 4);
    assert_eq!(env.eval_int("l.getProvincePixel(4, 0)"), 1);
    assert_eq!(env.eval_int("l.getProvincePixel(0, 1)"), 7);
    assert_eq!(env.eval_int("l.getProvincePixel(1, 1)"), 10);
    assert_eq!(env.eval_int("l.getProvincePixel(0, 2)"), 1);
    assert_eq!(env.eval_int("l.getProvincePixel(3, 3)"), 10);
    assert_eq!(env.eval_int("l.getProvincePixel(4, 4)"), 1);
}

/// A province image larger than the main image raises the reference's size
/// mismatch, and the failed load drops the plane instead of leaving the
/// previous allocation behind.
///
/// Reference `reference/cpp/core/visual/LayerIntf.cpp:2893-2914`: allocation
/// happens before the load and the catch deallocates it on any failure.
/// `:2985-3002` writes the previous plane; `:2973-2982` and `:3042-3046`
/// read it, so 7 and a pitch of 1 before the load and 0 after prove the drop.
#[test]
fn a_larger_province_image_raises_and_deallocates_the_plane() {
    let _lock = common::vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(1, 1); \
         l.setProvincePixel(0, 0, 7);",
    );
    // The layer already has a plane the failed load must not leave behind.
    assert_eq!(env.eval_int("l.provinceImageBufferPitch"), 1);
    assert_eq!(env.eval_int("l.getProvincePixel(0, 0)"), 7);
    let error = env
        .try_run("l.loadProvinceImage('picture.png');")
        .expect_err("the larger province image is a size mismatch");
    assert!(
        error.contains("the province image is 2x2 but the main image is 1x1"),
        "the failure names the mismatch: {error}"
    );
    // The failed load dropped the plane, so it no longer answers.
    assert_eq!(env.eval_int("l.provinceImageBufferPitch"), 0);
    assert_eq!(env.eval_int("l.getProvincePixel(0, 0)"), 0);
    let layer = env.scene.layers.first().expect("the layer exists");
    assert!(
        layer.province.is_none(),
        "a failed load drops the previous plane"
    );
}

/// A load failure also drops the plane the allocation created: the catch
/// covers every failure, not only the size mismatch.
///
/// Reference `reference/cpp/core/visual/LayerIntf.cpp:2893-2914`: the plane is
/// allocated before the load and the catch deallocates it on any failure.
/// `:2985-3002` writes the previous plane; `:2973-2982` and `:3042-3046`
/// read it, so 7 and a pitch of 2 before the load and 0 after prove the drop.
#[test]
fn a_failed_province_load_drops_the_previous_plane() {
    let _lock = common::vm_lock();
    let env = Env::new();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(2, 2); \
         l.setProvincePixel(0, 0, 7);",
    );
    assert_eq!(env.eval_int("l.provinceImageBufferPitch"), 2);
    assert_eq!(env.eval_int("l.getProvincePixel(0, 0)"), 7);
    let error = env
        .try_run("l.loadProvinceImage('missing.png');")
        .expect_err("a missing province image raises");
    assert!(
        error.contains("storage not found: missing.png"),
        "the storage failure reaches the script: {error}"
    );
    assert_eq!(env.eval_int("l.provinceImageBufferPitch"), 0);
    assert_eq!(env.eval_int("l.getProvincePixel(0, 0)"), 0);
    let layer = env.scene.layers.first().expect("the layer exists");
    assert!(layer.province.is_none(), "the failed load drops the plane");
}

/// A hit test waits for the layer's pending image before it reads the alpha.
///
/// `_HitTestNoVisibleCheck` compares the main image's alpha against
/// `hitThreshold` (`reference/cpp/core/visual/LayerIntf.cpp:3233-3246`), and
/// `GetMostFrontChildAt` returns the layer that hit
/// (`reference/cpp/core/visual/LayerIntf.cpp:3363-3385`). The reference image
/// is complete when the hit test runs because the load decodes it before it
/// returns (`reference/cpp/core/visual/LayerIntf.cpp:2850`).
#[test]
fn a_hit_test_waits_for_the_pending_layer_image() {
    let _lock = common::vm_lock();
    let env = Env::with_pool(config());
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.loadImages('picture.png');",
    );
    // The load returned with the probed size and no pixels, and the default
    // `hitThreshold` is 16, so the hit test has to read the image.
    let bitmap_id = env
        .pending_layer()
        .bitmap
        .expect("the layer has a main image");
    assert!(
        env.scene
            .bitmap(bitmap_id)
            .expect("the image exists")
            .rgba
            .is_empty(),
        "the pixels have not landed yet"
    );

    // reference/cpp/core/visual/LayerIntf.cpp:3233-3246, :3363-3385
    assert_eq!(
        env.eval_int("l.getLayerAt(0, 0) === l"),
        1,
        "the layer under the point is itself"
    );
    let bitmap = env.scene.bitmap(bitmap_id).expect("the image exists");
    assert!(
        bitmap.pending.is_none(),
        "the hit test waited for the decode"
    );
    assert_eq!(bitmap.rgba.len(), 16, "the image is decoded");
}

/// The pointer hit test waits for the pending image of the window it tests.
///
/// A pointer move hit-tests the layer tree
/// (`reference/cpp/core/visual/LayerManager.cpp:461`) and fires
/// `onMouseMove` on the layer it found
/// (`reference/cpp/core/visual/LayerManager.cpp:506-509`); the layer hit test
/// reads the main image's alpha
/// (`reference/cpp/core/visual/LayerIntf.cpp:3233-3246`), and the reference
/// image is complete because the load decodes it before it returns
/// (`reference/cpp/core/visual/LayerIntf.cpp:2850`).
#[test]
fn a_pointer_hit_test_waits_for_the_pending_layer_image() {
    let _lock = common::vm_lock();
    let env = Env::with_pool(config());
    env.run(
        "var w = new Window(); \
         w.setSize(320, 240); \
         var l = new Layer(w, null); \
         l.loadImages('picture.png'); \
         var moves = 0; \
         l.onMouseMove = function(x, y, shift) { moves = moves + 1; };",
    );
    let bitmap_id = env
        .pending_layer()
        .bitmap
        .expect("the layer has a main image");
    assert!(
        env.scene
            .bitmap(bitmap_id)
            .expect("the image exists")
            .rgba
            .is_empty(),
        "the pixels have not landed yet"
    );

    env.dispatch_events(&[PlatformEvent::Input(InputEvent::PointerMoved {
        x: 1.0,
        y: 1.0,
    })]);
    // reference/cpp/core/visual/LayerManager.cpp:461, :506-509
    assert_eq!(
        env.eval_int("moves"),
        1,
        "the pointer reached the layer whose image was decoding"
    );
    let bitmap = env.scene.bitmap(bitmap_id).expect("the image exists");
    assert!(
        bitmap.pending.is_none(),
        "the pointer hit test waited for the decode"
    );
    assert_eq!(bitmap.rgba.len(), 16, "the image is decoded");
}

/// A fill into a layer whose image is still decoding waits for the decode and
/// fills the decoded pixels.
///
/// `tTJSNI_BaseLayer::FillRect` intersects the rectangle with the clip and
/// fills the main image and its mask
/// (`reference/cpp/core/visual/LayerIntf.cpp:4272-4286`), and the fill
/// operates on the image's own texture
/// (`reference/cpp/core/visual/LayerBitmapIntf.cpp:286-303`). The reference
/// image is complete when the fill runs because the load decodes it before it
/// returns (`reference/cpp/core/visual/LayerIntf.cpp:2850`); the deferred
/// model waits for the decode at the fill, so the fill lands on the decoded
/// pixels instead of being dropped.
#[test]
fn a_fill_waits_for_the_pending_layer_image() {
    let _lock = common::vm_lock();
    let mut env = Env::with_pool(config());
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.loadImages('picture.png');",
    );
    let bitmap_id = env
        .pending_layer()
        .bitmap
        .expect("the layer has a main image");
    assert!(
        env.scene
            .bitmap(bitmap_id)
            .expect("the image exists")
            .rgba
            .is_empty(),
        "the pixels have not landed yet"
    );

    env.run("l.fillRect(0, 0, 2, 2, 0x00ff00);");
    let bitmap = env.scene.bitmap(bitmap_id).expect("the image exists");
    assert!(bitmap.pending.is_none(), "the fill waited for the decode");
    assert_eq!(bitmap.rgba.len(), 16, "the image holds its pixels");

    // The barrier resolves nothing: the fill is still the image's content,
    // not the decoded image's own pixels.
    env.scene.resolve_pending_images();
    assert_eq!(
        env.eval_int("l.getMainPixel(0, 0)"),
        0x00ff00,
        "the fill survives the image barrier"
    );
}

/// A copy into a destination whose image is still decoding replaces the
/// destination's pixels, and the decode it carried is superseded.
///
/// `Layer.copyToBitmapFromMainImage` assigns the layer's MainImage into the
/// bitmap (`reference/cpp/core/visual/LayerIntf.cpp:2427-2432`); the
/// assignment replaces the destination's image, so the pixels that land later
/// are the copied ones and not the stale decode.
#[test]
fn a_copied_image_replaces_a_pending_destination_bitmap() {
    let _lock = common::vm_lock();
    let mut env = Env::with_pool(config());
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setImageSize(2, 2); \
         l.fillRect(0, 0, 2, 2, 0x00ff00); \
         var b = new Bitmap('picture.png');",
    );
    let destination = env
        .scene
        .bitmaps
        .iter()
        .find(|bitmap| bitmap.pending.is_some())
        .expect("the destination image is still decoding")
        .id;
    assert!(
        env.scene
            .bitmap(destination)
            .expect("the destination exists")
            .rgba
            .is_empty(),
        "the destination holds no pixels yet"
    );

    env.run("l.copyToBitmapFromMainImage(b);");
    assert!(
        env.scene
            .bitmap(destination)
            .expect("the destination exists")
            .pending
            .is_none(),
        "the copy superseded the destination's decode"
    );
    env.scene.resolve_pending_images();
    assert_eq!(
        env.eval_int("b.getPixel(0, 0) & 0xffffff"),
        0x00ff00,
        "the copied pixels survive the image barrier"
    );
}

/// A copy into a layer whose image is still decoding replaces the layer's
/// pixels, and the decode it carried is superseded.
///
/// `Layer.copyFromBitmapToMainImage` assigns the bitmap into the layer's
/// MainImage (`reference/cpp/core/visual/LayerIntf.cpp:2404-2412`); the
/// assignment replaces the layer's image, so the pixels that land later are
/// the copied ones and not the stale decode.
#[test]
fn a_copied_image_replaces_a_pending_layer_image() {
    let _lock = common::vm_lock();
    let mut env = Env::with_pool(config());
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.loadImages('picture.png'); \
         var src = new Bitmap(2, 2); \
         src.setPixel(0, 0, 0x00ff00);",
    );
    let destination = env
        .pending_layer()
        .bitmap
        .expect("the layer has a main image");
    assert!(
        env.scene
            .bitmap(destination)
            .expect("the layer image exists")
            .rgba
            .is_empty(),
        "the layer holds no pixels yet"
    );

    env.run("l.copyFromBitmapToMainImage(src);");
    assert!(
        env.scene
            .bitmap(destination)
            .expect("the layer image exists")
            .pending
            .is_none(),
        "the copy superseded the layer's decode"
    );
    env.scene.resolve_pending_images();
    assert_eq!(
        env.eval_int("l.getMainPixel(0, 0) & 0xffffff"),
        0x00ff00,
        "the copied pixels survive the image barrier"
    );
}

/// An assignment into a layer whose image is still decoding replaces the
/// layer's pixels, and the decode it carried is superseded.
///
/// `tTJSNI_BaseLayer::AssignImages` assigns the source's MainImage into the
/// destination's (`reference/cpp/core/visual/LayerIntf.cpp:2399-2408`), so the
/// assigned pixels are the destination's image and the decode it still
/// carries is superseded by them.
#[test]
fn an_assigned_image_replaces_a_pending_layer_image() {
    let _lock = common::vm_lock();
    let mut env = Env::with_pool(config());
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.loadImages('picture.png'); \
         var src = new Layer(w, null); \
         src.setImageSize(2, 2); \
         src.fillRect(0, 0, 2, 2, 0x00ff00);",
    );
    let destination = env
        .pending_layer()
        .bitmap
        .expect("the layer has a main image");
    assert!(
        env.scene
            .bitmap(destination)
            .expect("the layer image exists")
            .rgba
            .is_empty(),
        "the layer holds no pixels yet"
    );

    env.run("l.assignImages(src);");
    assert!(
        env.scene
            .bitmap(destination)
            .expect("the layer image exists")
            .pending
            .is_none(),
        "the assignment superseded the layer's decode"
    );
    env.scene.resolve_pending_images();
    assert_eq!(
        env.eval_int("l.getMainPixel(0, 0) & 0xffffff"),
        0x00ff00,
        "the assigned pixels survive the image barrier"
    );
}
