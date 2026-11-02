//! `System.touchImages`: the pre-load of the named graphics.
//!
//! The environment attaches an L2 [`Scene`] with its decoded-image cache
//! enabled, an L2 [`Storage`] and an L2 [`DecodePool`] — the same seams the
//! app installs. The decoder counts the decodes that run at the same time,
//! so a test can tell a burst from a sequence of single loads.
//!
//! The reference pre-loads the named graphics into the graphic cache
//! (`reference/cpp/core/base/SystemIntf.cpp:224-256`,
//! `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2208-2258`): the list
//! is walked by index, every name the cache does not hold yet is queued for a
//! load, and the names the cache holds are touched so that the earlier ones
//! keep their priority in the cache order. The game's own tag handler
//! collects the scene's files into one list and calls it once
//! (`sysscn/override.tjs:1674-1682`).

mod common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use api::media::{DecodedImage, ImageInfo};
use compat::CompatHost;
use engine::imagedecode::{DecodePool, ImageDecodeError, ImageDecoder};
use engine::input::InputState;
use engine::jobpool::JobPoolConfig;
use engine::timer::TimerState;
use engine::{DecodedImageKey, DecodedImageMode, Scene, Storage};
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// `TVP_clNone`: the colour key a load without one uses. The value is the
/// engine's own (`crates/compat/src/natives/visual/bitmap.rs`), and it is
/// part of the decoded-image cache key.
const COLOR_KEY_NONE: u32 = 0x1fff_ffff;

/// The image the synthetic codec and the pool's decoder both produce.
fn image() -> DecodedImage {
    DecodedImage::new(
        2,
        2,
        vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255],
    )
}

/// The names of the pictures the storage holds.
const PICTURES: [&str; 4] = ["a.png", "b.png", "c.png", "d.png"];

/// The bytes of the pictures the storage holds.
const PICTURE: &[u8] = b"not-a-real-image";

/// The decoder the pool drives: every decode produces [`image`], and the
/// decoder counts the decodes that run at the same time.
struct CountingDecoder {
    /// The decodes that are running right now.
    running: Arc<AtomicUsize>,
    /// The highest number of decodes that ran at the same time.
    peak: Arc<AtomicUsize>,
    /// The decodes that ran in all.
    total: Arc<AtomicUsize>,
}

impl ImageDecoder for CountingDecoder {
    fn probe(&self, _bytes: &[u8], _hint: Option<&str>) -> Result<ImageInfo, ImageDecodeError> {
        Ok(ImageInfo {
            width: image().width,
            height: image().height,
            has_alpha: true,
            format: Some("img".to_string()),
        })
    }

    fn decode(&self, _bytes: &[u8], _hint: Option<&str>) -> Result<DecodedImage, ImageDecodeError> {
        let running = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.total.fetch_add(1, Ordering::SeqCst);
        self.peak.fetch_max(running, Ordering::SeqCst);
        // The decode takes a moment, so the decodes of one burst overlap.
        std::thread::sleep(Duration::from_millis(50));
        self.running.fetch_sub(1, Ordering::SeqCst);
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
    /// Kept alive for the host's raw pointer; never read directly.
    _storage: Box<Storage>,
    _input: Box<InputState>,
    _timer: Box<TimerState>,
    /// Kept alive for the host's raw pointer; the test reads its counters.
    pool: Box<DecodePool>,
    /// The decodes that are running right now.
    running: Arc<AtomicUsize>,
    /// The highest number of decodes that ran at the same time.
    peak: Arc<AtomicUsize>,
    /// The decodes that ran in all.
    total: Arc<AtomicUsize>,
    /// The game directory the storage is mounted from; removed on drop.
    _game_dir: PathBuf,
}

impl Env {
    /// An environment whose decoded-image cache is enabled.
    fn new() -> Env {
        Self::build(true)
    }

    /// An environment whose decoded-image cache is disabled.
    fn without_cache() -> Env {
        Self::build(false)
    }

    fn build(cache: bool) -> Env {
        let game_dir = unique_dir("krkr-touch-test");
        std::fs::create_dir_all(&game_dir).expect("create temp game dir");
        for picture in PICTURES {
            std::fs::write(game_dir.join(picture), PICTURE).expect("write the picture");
        }
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
        if cache {
            scene.set_graphic_cache_limit(1024 * 1024);
        }
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
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let total = Arc::new(AtomicUsize::new(0));
        let decoder = CountingDecoder {
            running: Arc::clone(&running),
            peak: Arc::clone(&peak),
            total: Arc::clone(&total),
        };
        let mut pool =
            Box::new(DecodePool::new(Arc::new(decoder), config()).expect("start the decode pool"));
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
            pool,
            running,
            peak,
            total,
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
            .exec_script(script, "touch-images-test")
            .map(|_| ())
            .map_err(|error| error.to_string())
    }

    /// Evaluates an expression.
    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "touch-images-test")
            .map_err(|error| error.to_string())
    }

    /// The decoded-image cache key of a picture.
    fn key(name: &str) -> DecodedImageKey {
        DecodedImageKey::new(name, COLOR_KEY_NONE, DecodedImageMode::Full, 0, 0)
    }

    /// Whether the decoded-image cache holds the picture.
    fn cached(&mut self, name: &str) -> bool {
        self.scene.lookup_decoded_image(&Self::key(name)).is_some()
    }

    /// The number of decodes that ran.
    fn decodes(&self) -> usize {
        self.total.load(Ordering::SeqCst)
    }

    /// The highest number of decodes that ran at the same time.
    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
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

/// A pool configuration with one worker per picture.
fn config() -> JobPoolConfig {
    JobPoolConfig {
        workers: 4,
        queue_limit: 16,
        memory_limit: 1024 * 1024,
    }
}

/// The pre-load puts every named image into the decoded-image cache, so a
/// later load of the same image is a cache hit instead of a second decode.
///
/// The reference queues the names the graphic cache does not hold and stores
/// the loaded graphics under their name
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2237-2256`).
#[test]
fn touch_images_puts_the_named_images_into_the_cache() {
    let _lock = common::vm_lock();
    let mut env = Env::new();
    env.run("System.touchImages(['a.png', 'b.png', 'c.png', 'd.png']);");
    for picture in PICTURES {
        assert!(
            env.cached(picture),
            "{picture} is in the decoded-image cache"
        );
    }
    assert_eq!(env.decodes(), PICTURES.len(), "every image was decoded");
}

/// The pre-load runs the decodes as one burst: the pool holds several of them
/// at the same time, and the wait is the time the last decode needs rather
/// than the sum of them.
///
/// The reference queues every name the cache does not hold before it waits
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2237-2256`).
#[test]
fn touch_images_decodes_the_list_as_one_burst() {
    let _lock = common::vm_lock();
    let env = Env::new();
    env.run("System.touchImages(['a.png', 'b.png', 'c.png', 'd.png']);");
    assert!(
        env.peak() > 1,
        "the decodes overlapped, the peak was {}",
        env.peak()
    );
}

/// A name the cache already holds is touched, not decoded again.
#[test]
fn touch_images_touches_the_names_the_cache_holds() {
    let _lock = common::vm_lock();
    let mut env = Env::new();
    env.run("System.touchImages(['a.png', 'b.png']);");
    assert_eq!(
        env.decodes(),
        2,
        "the first pre-load decoded the two images"
    );
    env.run("System.touchImages(['a.png', 'b.png']);");
    assert_eq!(
        env.decodes(),
        2,
        "the second pre-load decoded nothing: the cache held the images"
    );
    for picture in ["a.png", "b.png"] {
        assert!(env.cached(picture), "{picture} is still cached");
    }
}

/// A name that resolves to nothing does not raise: the pre-load carries on
/// with the rest of the list.
///
/// The reference logs the failure of a graphic it cannot load and loads the
/// next one (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2374-2394`).
#[test]
fn touch_images_carries_on_past_a_name_that_does_not_resolve() {
    let _lock = common::vm_lock();
    let mut env = Env::new();
    env.run("System.touchImages(['missing.png', 'a.png']);");
    assert!(env.cached("a.png"), "the image after the miss is cached");
    assert_eq!(env.decodes(), 1, "only the image that exists was decoded");
}

/// A disabled graphic cache leaves the pre-load with nothing to do.
///
/// The reference returns before it queues anything when the graphic cache is
/// off (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2217-2218`).
#[test]
fn touch_images_does_nothing_without_a_cache() {
    let _lock = common::vm_lock();
    let mut env = Env::without_cache();
    env.run("System.touchImages(['a.png', 'b.png']);");
    assert_eq!(env.decodes(), 0, "no image was decoded");
    assert!(!env.cached("a.png"), "the cache holds nothing");
}

/// The arguments are validated the way the reference validates them: the
/// first argument must be an object, and the optional `limit`/`timeout`
/// convert with the strict integer conversion, which raises on objects.
///
/// The reference walks the list by index and converts the two optional
/// arguments (`reference/cpp/core/base/SystemIntf.cpp:224-256`).
#[test]
fn touch_images_validates_its_arguments() {
    let _lock = common::vm_lock();
    let env = Env::new();
    assert!(
        env.eval("System.touchImages()").is_err(),
        "the call needs an argument"
    );
    assert!(
        env.eval("System.touchImages(5)").is_err(),
        "a number is not a list"
    );
    assert!(
        env.eval("System.touchImages([], {})").is_err(),
        "an object is not a limit"
    );
    assert!(
        env.eval("System.touchImages([], 0, {})").is_err(),
        "an object is not a timeout"
    );
    assert_eq!(
        env.eval("System.touchImages([])")
            .expect("an empty list is fine"),
        TjsValue::Void
    );
}

/// A list that is not an array of names is walked the way the reference walks
/// it: the walk ends at the first member the list does not hold, so the names
/// before it are pre-loaded and the ones after it are not.
#[test]
fn touch_images_walks_the_list_by_index() {
    let _lock = common::vm_lock();
    let mut env = Env::new();
    env.run("System.touchImages(['a.png', 'b.png']);");
    assert_eq!(env.decodes(), 2, "the two names were decoded");
    env.run("System.touchImages(['c.png']);");
    assert_eq!(env.decodes(), 3, "the third name was decoded too");
    assert!(env.cached("c.png"), "the third image is cached");
}

/// The list may be a dictionary: the walk reads the members by index, so a
/// dictionary with the members "0" and "1" holds two names.
///
/// The walk is a numeric member read, and the script engine's numeric member
/// read is the named member whose name is the number in decimal, so the
/// dictionary is built with the numeric index.
#[test]
fn touch_images_reads_a_dictionary_by_index() {
    let _lock = common::vm_lock();
    let mut env = Env::new();
    env.run("var d = %[]; d[0] = 'a.png'; d[1] = 'b.png'; System.touchImages(d);");
    assert_eq!(env.decodes(), 2, "the two names were decoded");
    assert!(env.cached("a.png"), "the first image is cached");
}

/// The pool's own accounting agrees with the burst: the pool ran every decode
/// the pre-load asked for.
#[test]
fn touch_images_runs_its_decodes_on_the_pool() {
    let _lock = common::vm_lock();
    let env = Env::new();
    env.run("System.touchImages(['a.png', 'b.png', 'c.png', 'd.png']);");
    assert_eq!(
        env.pool.stats().jobs,
        PICTURES.len() as u64,
        "the pool ran every decode"
    );
    assert_eq!(
        env.running.load(Ordering::SeqCst),
        0,
        "no decode is running"
    );
}
