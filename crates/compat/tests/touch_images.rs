//! `System.touchImages`: the pre-load of the named graphics.
//!
//! The environment attaches an L2 [`Scene`] with its decoded-image cache
//! enabled, an L2 [`Storage`] and an L2 [`DecodePool`] — the same seams the
//! app installs. The decoder counts the decodes that run at the same time,
//! so a test can tell a burst from a sequence of single loads, and a test
//! can hold a decode at a gate, so it can tell a call that waits for its
//! decodes from a call that queues them.
//!
//! The reference pre-loads the named graphics into the graphic cache
//! (`reference/cpp/core/base/SystemIntf.cpp:224-256`,
//! `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2208-2258`): the list
//! is walked by index, every name the cache does not hold yet is queued for a
//! load, and the names the cache holds are touched so that the earlier ones
//! keep their priority in the cache order. The queued load runs on the
//! engine's image-loader thread and the pixels reach the cache later, on the
//! main thread, when the load completes
//! (`reference/cpp/core/visual/GraphicsLoadThread.cpp:167-176`), so the
//! call itself does not wait. The game's own tag handler collects the scene's
//! files into one list and calls it once
//! (`sysscn/override.tjs:1674-1682`).

mod common;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

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

/// How long a decode waits at the gate before it goes on.
///
/// The wait is bounded, so a call that blocks on its decodes ends instead of
/// hanging the test run.
const GATE_TIMEOUT: Duration = Duration::from_secs(5);

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

/// The gate a decode waits at.
///
/// The gate lets a test hold a decode at a known point: the decode
/// announces its arrival and waits until the test opens the gate. A decode
/// that is never released goes on after the gate's timeout, so a call that
/// waits for its decodes ends instead of hanging the test run.
struct Gate {
    /// Announces that a decode reached the gate.
    arrived: Mutex<mpsc::Sender<()>>,
    /// Whether the gate is open.
    open: Mutex<bool>,
    /// Signalled when the gate opens.
    opened: Condvar,
}

impl Gate {
    /// Creates a gate and the channel that reports the arrivals.
    fn new() -> (Arc<Gate>, mpsc::Receiver<()>) {
        let (arrived, arrivals) = mpsc::channel();
        let gate = Arc::new(Gate {
            arrived: Mutex::new(arrived),
            open: Mutex::new(false),
            opened: Condvar::new(),
        });
        (gate, arrivals)
    }

    /// Announces that a decode reached the gate and waits for it to open.
    fn wait(&self) {
        let _ = self.arrived.lock().expect("the gate's lock").send(());
        let mut open = self.open.lock().expect("the gate's lock");
        while !*open {
            let (guard, timeout) = self
                .opened
                .wait_timeout(open, GATE_TIMEOUT)
                .expect("the gate's lock");
            open = guard;
            if timeout.timed_out() {
                break;
            }
        }
    }

    /// Opens the gate: the waiting decodes go on.
    fn open(&self) {
        let mut open = self.open.lock().expect("the gate's lock");
        *open = true;
        self.opened.notify_all();
    }
}

/// The decoder the pool drives: every decode produces [`image`], and the
/// decoder counts the decodes that run at the same time.
struct CountingDecoder {
    /// The decodes that are running right now.
    running: Arc<AtomicUsize>,
    /// The highest number of decodes that ran at the same time.
    peak: Arc<AtomicUsize>,
    /// The decodes that ran in all.
    total: Arc<AtomicUsize>,
    /// The gate the decodes wait at, when the test holds them.
    gate: Option<Arc<Gate>>,
    /// How long a decode takes when no gate holds it.
    delay: Duration,
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
        match &self.gate {
            Some(gate) => gate.wait(),
            None => std::thread::sleep(self.delay),
        }
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
    /// The gate the decodes wait at, when the test holds them.
    gate: Option<Arc<Gate>>,
    /// The channel the gate reports the arrivals on.
    arrivals: Option<mpsc::Receiver<()>>,
    /// The game directory the storage is mounted from; removed on drop.
    _game_dir: PathBuf,
}

impl Env {
    /// An environment whose decoded-image cache is enabled.
    fn new() -> Env {
        Self::build(true, None)
    }

    /// An environment whose decoded-image cache is disabled.
    fn without_cache() -> Env {
        Self::build(false, None)
    }

    /// An environment whose decodes wait at a gate.
    ///
    /// The gate lets a test see whether a call waited for its decodes: a
    /// decode that is still at the gate when the call returned was not
    /// waited for.
    fn gated() -> Env {
        let (gate, arrivals) = Gate::new();
        let mut env = Self::build(true, Some(gate));
        env.arrivals = Some(arrivals);
        env
    }

    fn build(cache: bool, gate: Option<Arc<Gate>>) -> Env {
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
            gate: gate.clone(),
            delay: Duration::from_millis(50),
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
            gate,
            arrivals: None,
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

    /// The number of decodes that are running right now.
    fn running(&self) -> usize {
        self.running.load(Ordering::SeqCst)
    }

    /// The highest number of decodes that ran at the same time.
    fn peak(&self) -> usize {
        self.peak.load(Ordering::SeqCst)
    }

    /// Whether a bitmap of the scene still waits for its decode.
    fn pending_bitmaps(&self) -> bool {
        self.scene
            .bitmaps
            .iter()
            .any(|bitmap| bitmap.pending.is_some())
    }

    /// Waits until a decode reached the gate.
    fn wait_arrival(&self) {
        let arrivals = self
            .arrivals
            .as_ref()
            .expect("the environment holds the gate's arrivals");
        arrivals
            .recv_timeout(Duration::from_secs(10))
            .expect("a decode reached the gate");
    }

    /// Opens the gate: the decodes that wait at it go on.
    fn open_gate(&self) {
        let gate = self.gate.as_ref().expect("the environment holds a gate");
        gate.open();
    }

    /// Waits until the pool ran `count` decodes.
    ///
    /// The wait is on the pool's own accounting, so it ends when the
    /// decodes have finished; a decode that never finishes ends the wait
    /// with a failure.
    fn wait_decodes(&self, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.pool.stats().jobs < count as u64 {
            assert!(
                Instant::now() < deadline,
                "the pool did not run {count} decodes"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Runs the frame boundary: the preloads take their pixels into the
    /// decoded-image cache.
    fn frame_boundary(&mut self) {
        self.scene.resolve_pending_images();
    }

    /// Runs the frame boundary until the cache holds every picture.
    ///
    /// The pool counts a job just before it publishes its result, so a
    /// frame boundary can run before the pixels are there; the wait ends
    /// when a boundary has taken them into the cache.
    fn frame_boundary_until_cached(&mut self, names: &[&str]) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let missing: Vec<&str> = names
                .iter()
                .copied()
                .filter(|&name| !self.cached(name))
                .collect();
            if missing.is_empty() {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the decoded-image cache never held {missing:?}"
            );
            self.frame_boundary();
            std::thread::sleep(Duration::from_millis(1));
        }
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

/// The pre-load does not wait for the decodes it queues: the call returns
/// while the decode is still running, and the script goes on.
///
/// The reference pushes the names onto its image loader's queue and returns
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2233-2258`); the
/// loader thread runs the load while the main thread continues
/// (`reference/cpp/core/visual/GraphicsLoadThread.cpp:167-176`).
#[test]
fn touch_images_does_not_wait_for_the_decodes() {
    let _lock = common::vm_lock();
    let env = Env::gated();
    env.run("System.touchImages(['a.png']);");
    // The decode reached the gate: the call returned while the decode was
    // still running.
    env.wait_arrival();
    assert_eq!(env.decodes(), 1, "the decode started");
    assert_eq!(
        env.running(),
        1,
        "the decode is still running: the call did not wait for it"
    );
    env.open_gate();
    env.wait_decodes(1);
    assert_eq!(env.running(), 0, "the decode finished");
}

/// The pixels of a pre-loaded image reach the decoded-image cache at the
/// frame boundary, not while the call is running.
///
/// The reference's main thread pushes the loaded image into the graphic
/// cache when the load completes
/// (`reference/cpp/core/visual/GraphicsLoadThread.cpp:167-176`).
#[test]
fn touch_images_caches_the_images_at_the_frame_boundary() {
    let _lock = common::vm_lock();
    let mut env = Env::gated();
    env.run("System.touchImages(['a.png']);");
    env.wait_arrival();
    assert!(
        !env.cached("a.png"),
        "the pixels have not landed yet: the call did not wait for them"
    );
    env.open_gate();
    env.wait_decodes(1);
    assert!(
        !env.cached("a.png"),
        "the pixels are not in the cache before the frame boundary"
    );
    env.frame_boundary_until_cached(&["a.png"]);
    assert!(
        env.cached("a.png"),
        "the frame boundary put the pixels in the cache"
    );
}

/// A load that runs while the pre-load's decode is still decoding shares
/// that decode: the same bytes are not decoded a second time.
///
/// The reference consults the graphic cache before it loads and stores the
/// decoded image under the same key
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1952-1960`), so the
/// second load of one image is served from the first load's result.
#[test]
fn a_later_load_shares_the_running_preload() {
    let _lock = common::vm_lock();
    let env = Env::gated();
    env.run("System.touchImages(['a.png']);");
    env.wait_arrival();
    env.run("var b = new Bitmap('a.png');");
    assert_eq!(env.decodes(), 1, "the load did not decode the image again");
    assert!(
        env.pending_bitmaps(),
        "the load shares the pre-load's decode: the bitmap waits for it"
    );
    env.open_gate();
    env.wait_decodes(1);
    assert_eq!(
        env.pool.stats().jobs,
        1,
        "the pool ran one decode: the load shared the pre-load's decode"
    );
}

/// The timeout is converted but never consulted: the pre-load never waits
/// for the decodes it queues, so a timeout does not cut the pre-load short.
///
/// The reference's wait is behind a commented-out guard, so the timeout is
/// never read (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2233-2258`).
#[test]
fn touch_images_ignores_the_timeout() {
    let _lock = common::vm_lock();
    let mut env = Env::gated();
    env.run("System.touchImages(['a.png'], 0, 1);");
    env.wait_arrival();
    env.open_gate();
    env.wait_decodes(1);
    env.frame_boundary_until_cached(&["a.png"]);
    assert!(
        env.cached("a.png"),
        "the pixels land: the timeout is not consulted"
    );
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
    env.wait_decodes(PICTURES.len());
    env.frame_boundary_until_cached(&PICTURES);
    for picture in PICTURES {
        assert!(
            env.cached(picture),
            "{picture} is in the decoded-image cache"
        );
    }
    assert_eq!(env.decodes(), PICTURES.len(), "every image was decoded");
}

/// The pre-load runs the decodes as one burst: the pool holds several of them
/// at the same time.
///
/// The reference queues every name the cache does not hold before it waits
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2237-2256`).
#[test]
fn touch_images_decodes_the_list_as_one_burst() {
    let _lock = common::vm_lock();
    let env = Env::new();
    env.run("System.touchImages(['a.png', 'b.png', 'c.png', 'd.png']);");
    env.wait_decodes(PICTURES.len());
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
    env.wait_decodes(2);
    env.frame_boundary_until_cached(&["a.png", "b.png"]);
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
    env.wait_decodes(1);
    env.frame_boundary_until_cached(&["a.png"]);
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
    env.wait_decodes(2);
    assert_eq!(env.decodes(), 2, "the two names were decoded");
    env.run("System.touchImages(['c.png']);");
    env.wait_decodes(3);
    env.frame_boundary_until_cached(&["c.png"]);
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
    env.wait_decodes(2);
    env.frame_boundary_until_cached(&["a.png", "b.png"]);
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
    env.wait_decodes(PICTURES.len());
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

/// The resolution of a name costs no read.
///
/// The reference's graphic resolution asks whether the file exists
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:301-318`), so a name
/// that resolves to nothing is never read.
#[test]
fn a_name_that_resolves_to_nothing_costs_no_read() {
    let _lock = common::vm_lock();
    let env = Env::new();
    let before = env._storage.read_bytes();
    env.run("try { System.touchImages(['missing.png']); } catch (e) {}");
    assert_eq!(
        env._storage.read_bytes(),
        before,
        "a name that resolves to nothing must not be read"
    );
}

/// The pre-load reads the names it decodes.
///
/// The bytes come from the storage's read pool, so the reads of a burst
/// overlap; the storage counts the bytes it handed out. The reference queues
/// its pre-load the same way
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2233-2262`).
#[test]
fn the_preload_reads_every_name_it_decodes() {
    let _lock = common::vm_lock();
    let env = Env::new();
    let before = env._storage.read_bytes();
    env.run("System.touchImages(['a.png', 'b.png']);");
    assert_eq!(
        env._storage.read_bytes() - before,
        2 * PICTURE.len() as u64,
        "both names were read"
    );
}
