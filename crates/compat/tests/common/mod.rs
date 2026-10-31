//! Shared helpers for the `compat` integration tests.
//!
//! `vm_lock` serializes tests that touch the process-global TJS VM; [`Env`]
//! binds a VM + [`CompatHost`] + the L2 scene/timer/input (and optionally an
//! L2 storage) the same way the app does, so native dispatch can be exercised
//! through the real seams. The module is compiled into every `compat` test
//! binary, so items a particular binary does not use are allowed dead.

#![allow(dead_code)]

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use api::media::{DecodedImage, ImageInfo};
use compat::CompatHost;
use engine::Scene;
use engine::Storage;
use engine::clock::{FrameClock, FrameTimeSource};
use engine::input::InputState;
use engine::timer::{TimerState, deliver_due};
use fs::FileSystem;
use platform::PlatformEvent;
use tjs::{Tjs2Engine, TjsValue};

/// The frame-time source the harness attaches.
///
/// The reading is the harness's own, not the frame clock's, so a test can move
/// it on its own: that is the only way to tell a tick count sampled from the
/// source apart from one read off the frame boundary. The `System` native takes
/// the later of this reading and the frame clock, so a source left behind the
/// clock (the harness's default, at zero) never changes what a test reads.
struct HarnessFrameTime {
    /// The source's own reading, in milliseconds.
    now_ms: std::cell::Cell<f64>,
}

impl FrameTimeSource for HarnessFrameTime {
    /// Zero: the harness steps the frame clock directly and never paces a
    /// frame through this trait, so it has no frame delta to report.
    fn elapsed_ms(&self) -> f64 {
        0.0
    }

    fn now_ms(&self) -> f64 {
        self.now_ms.get()
    }
}

fn lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Serialize tests that touch the process-global TJS VM and the single
/// engine → context binding.
pub fn vm_lock() -> MutexGuard<'static, ()> {
    lock().lock().unwrap_or_else(|poison| poison.into_inner())
}

/// A bound VM + host + attached scene, timer registry and input state, with an
/// optional temp game directory mounted as L2 storage.
///
/// Fields drop in declaration order: the host first (releasing every retained
/// script object and removing the process-wide binding), then the engine, then
/// the boxed scene / timer / input / clock / storage the host's raw pointers
/// refer to.
pub struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// Read back by tests that assert on the L2 state a native write reached.
    pub scene: Box<Scene>,
    /// The attached timer registry; tests move its injected time directly so a
    /// poll never depends on the wall clock.
    timer: RefCell<Box<TimerState>>,
    /// The attached frame clock; tests advance it directly so a tick count
    /// never depends on the wall clock.
    clock: RefCell<Box<FrameClock>>,
    /// The attached clock source: an independent reading a test can move with
    /// `advance_tick_source`. The `System` native takes the later of this and
    /// the frame clock, so a source left at zero never changes what a test
    /// reads. Boxed so the address the host holds stays stable when `Env`
    /// moves.
    frame_time: Box<HarnessFrameTime>,
    _input: Box<InputState>,
    _storage: Option<Box<Storage>>,
    _game_dir: Option<PathBuf>,
    /// The adapter the events are delivered through. It outlives one call
    /// because it keeps the events a frame deferred, the way the VM host's
    /// adapter does.
    adapter: RefCell<compat::EventAdapter>,
}

/// The storage layout an `Env` is built with.
enum StorageDirs {
    /// No storage is attached.
    None,
    /// A temporary mount root that is also the project directory.
    Temp,
    /// An explicit mount root and project directory.
    Explicit {
        mount_root: PathBuf,
        app_dir: PathBuf,
    },
}

impl Env {
    /// A bound Env with a scene, timer registry and input state attached.
    pub fn new() -> Env {
        Env::build(StorageDirs::None)
    }

    /// A bound Env with a mounted L2 storage (the caller writes game files
    /// into its `game_dir`).
    pub fn with_storage() -> Env {
        Env::build(StorageDirs::Temp)
    }

    /// A bound Env with a mounted L2 storage whose mount root and project
    /// directory differ: the archives are mounted under `mount_root`, while
    /// the disk namespace and the script-visible system paths are rooted at
    /// `app_dir`.
    pub fn with_storage_dirs(mount_root: PathBuf, app_dir: PathBuf) -> Env {
        Env::build(StorageDirs::Explicit {
            mount_root,
            app_dir,
        })
    }

    fn build(layout: StorageDirs) -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut timer = Box::new(TimerState::new());
        let mut input = Box::new(InputState::new());
        let mut clock = Box::new(FrameClock::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene, timer registry, input state and frame clock
        // outlive the host (they are declared after `_host`), matching
        // `attach_*`.
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
        // The source's reading starts at zero (so the frame clock leads it and
        // the script's tick count is the frame clock, as it is for a host that
        // attached no source); a test moves it on its own with
        // `advance_tick_source`.
        let frame_time = Box::new(HarnessFrameTime {
            now_ms: std::cell::Cell::new(0.0),
        });
        let source: &(dyn FrameTimeSource + 'static) = frame_time.as_ref();
        // SAFETY: the source is boxed inside `Env`, which outlives the host
        // (its fields are declared after `_host`), so its pointer stays valid.
        unsafe {
            host.services_mut()
                .attach_frame_time(source as *const (dyn FrameTimeSource + 'static));
        }
        let dirs = match layout {
            StorageDirs::None => None,
            StorageDirs::Temp => {
                let dir = unique_dir("krkr-compat-test");
                std::fs::create_dir_all(&dir).expect("create temp game dir");
                Some((dir.clone(), dir))
            }
            StorageDirs::Explicit {
                mount_root,
                app_dir,
            } => Some((mount_root, app_dir)),
        };
        let (storage, game_dir) = if let Some((mount_root, app_dir)) = dirs {
            std::fs::create_dir_all(&mount_root).expect("create temp mount root");
            std::fs::create_dir_all(&app_dir).expect("create temp project dir");
            let mut storage = Box::new(
                Storage::mount_archives(
                    FileSystem::host(&mount_root),
                    mount_root.clone(),
                    std::iter::empty::<&str>(),
                )
                .expect("mount storage"),
            );
            storage.set_app_dir(&app_dir);
            // SAFETY: boxed and declared after `_host`, so it outlives the
            // binding (same contract as the other attachments).
            unsafe { host.services_mut().attach_storage(&mut *storage) };
            (Some(storage), Some(app_dir))
        } else {
            (None, None)
        };
        host.install(&engine).expect("install compat surface");
        // The composition root finishes the startup lifecycle here before any
        // startup script runs (`crates/app/src/game.rs`): attach the
        // scene/storage, install the surface, complete the deferred,
        // service-dependent half of plugin registration (the `xp3filter.tjs`
        // read), then open the timing window. The harness keeps the same order
        // as the app.
        host.activate_deferred_plugins()
            .expect("deferred plugin activation");
        host.begin_native_timing();
        Env {
            _host: host,
            engine,
            scene,
            timer: RefCell::new(timer),
            clock: RefCell::new(clock),
            frame_time,
            _input: input,
            _storage: storage,
            _game_dir: game_dir,
            // The adapter's default budget is a wall-clock slice, so on a
            // loaded machine it can cut a test's event sequence short and
            // defer the rest to a frame the test never drives. The tests that
            // exercise the budget build their own adapter
            // (`EventAdapter::with_dispatch_budget`); this one delivers every
            // event it is given.
            adapter: RefCell::new(compat::EventAdapter::with_dispatch_budget(Duration::MAX)),
        }
    }

    /// The project directory of the mounted storage, when one is mounted.
    ///
    /// For [`Env::with_storage`] it is the temp directory the caller writes
    /// game files into; for [`Env::with_storage_dirs`] it is the project
    /// directory the storage was given.
    pub fn game_dir(&self) -> Option<&PathBuf> {
        self._game_dir.as_ref()
    }

    /// The mounted L2 storage, for tests that inspect its read counter.
    pub fn storage(&self) -> Option<&Storage> {
        self._storage.as_deref()
    }

    /// The VM the harness is bound to (for the shared [`eval_then`]
    /// helper, which takes the engine rather than an [`Env`]).
    pub fn engine(&self) -> &Tjs2Engine {
        &self.engine
    }

    pub fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "compat-test")
            .map(|_| ())
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    pub fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "compat-test")
            .map_err(|error| error.to_string())
    }

    pub fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    pub fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    /// Numeric read for properties whose reference getter writes a Real
    /// (e.g. `Timer.interval`).
    pub fn eval_real(&self, expression: &str) -> f64 {
        match self.eval_ok(expression) {
            TjsValue::Real(value) => value,
            TjsValue::Integer(value) => value as f64,
            other => panic!("{expression:?} returned {other:?}, expected a number"),
        }
    }

    pub fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }

    /// Install a synthetic image codec whose decode closure appends the
    /// resolved storage name to `hints` and returns a fixed 2x2 straight-
    /// alpha RGBA test image, replacing any codec installed earlier.
    ///
    /// Every decoder run grows `hints` by one, so its length counts the
    /// loads that were not served from the scene's decoded-image cache.
    pub fn record_decodes(&self, hints: Arc<Mutex<Vec<String>>>) {
        self._host.set_image_codec(
            move |_bytes, hint| {
                hints
                    .lock()
                    .expect("decode lock")
                    .push(hint.unwrap_or_default().to_string());
                Ok(DecodedImage::new(
                    2,
                    2,
                    vec![1, 2, 3, 255, 4, 5, 6, 255, 7, 8, 9, 255, 10, 11, 12, 255],
                ))
            },
            |_bytes, _hint| {
                Ok(ImageInfo {
                    width: 2,
                    height: 2,
                    has_alpha: true,
                    format: Some("img".to_string()),
                })
            },
            |_name, _type, _width, _height, _rgba| Ok(()),
        );
    }

    /// Advance the attached frame clock by `dt_ms` and return its new
    /// whole-millisecond value.
    ///
    /// The clock is the engine's frame clock, so a test drives time instead
    /// of waiting for it.
    pub fn advance_clock(&self, dt_ms: f64) -> u64 {
        let mut clock = self.clock.borrow_mut();
        clock.advance(dt_ms)
    }

    /// Move the attached clock source's own reading by `ms`, leaving the frame
    /// clock where it is.
    ///
    /// This is what a source that measures real time does between two frames: a
    /// script's tick count samples the source when it asks, so the reading moves
    /// while no frame runs and the frame clock does not move. A negative value
    /// moves the reading backwards, which the tick count must not follow.
    pub fn advance_tick_source(&self, ms: f64) {
        let source = &self.frame_time;
        source.now_ms.set(source.now_ms.get() + ms);
    }

    /// Move the attached timer registry's injected time forward by `ms`
    /// without polling.
    pub fn advance_time(&self, ms: u64) {
        let mut timer = self.timer.borrow_mut();
        timer.now_ms = timer.now_ms.saturating_add(ms);
    }

    /// One frame-body timer step: deliver the timers due at the registry's
    /// injected time, then hand the queued `onTimer` events to the script.
    /// Returns whether any timer remains armed afterwards.
    pub fn poll_timer(&self) -> bool {
        // One virtual frame step at the reference's ~60 fps.
        const FRAME_STEP_MS: u64 = 16;
        self.advance_time(FRAME_STEP_MS);
        self.deliver_due_timers();
        let mut ctx = self._host.ctx().expect("bound compat context");
        compat::timer_poll(&mut ctx).expect("timer poll")
    }

    /// Deliver the timers due at the registry's injected time.
    ///
    /// The registry's pump belongs to the engine; a test that owns the
    /// registry drives it directly.
    pub fn deliver_due_timers(&self) {
        let mut timer = self.timer.borrow_mut();
        let now = timer.now_ms;
        deliver_due(timer.as_mut(), now);
    }

    /// One frame-body `async_trigger_poll` step.
    pub fn poll_async(&self) {
        let mut ctx = self._host.ctx().expect("bound compat context");
        compat::async_trigger_poll(&mut ctx).expect("async trigger poll");
    }

    /// Feed one frame's platform events through the L3 adapter and deliver
    /// each mapped event to the script handler the adapter resolves (the
    /// [`compat::EventAdapter::dispatch_frame`] seam the VM host's `tick`
    /// uses).
    pub fn dispatch_events(&self, events: &[PlatformEvent]) {
        let mut adapter = self.adapter.borrow_mut();
        let mut ctx = self._host.ctx().expect("bound compat context");
        adapter.begin_input_frame(&mut ctx);
        adapter
            .dispatch_frame(&mut ctx, events)
            .expect("dispatch events");
        adapter.end_input_frame(&mut ctx);
    }

    /// Feed one frame's platform events through `adapter` and deliver each
    /// mapped event to the script handler the adapter resolves.
    ///
    /// The caller owns the adapter, so a test can give it a dispatch budget
    /// and drive several frames through it — the events a frame defers are
    /// then delivered by the next call.
    pub fn dispatch_with(&self, adapter: &mut compat::EventAdapter, events: &[PlatformEvent]) {
        let mut ctx = self._host.ctx().expect("bound compat context");
        adapter.begin_input_frame(&mut ctx);
        adapter
            .dispatch_frame(&mut ctx, events)
            .expect("dispatch events");
        adapter.end_input_frame(&mut ctx);
    }

    /// Link a built-in plugin the way a game script does: `Plugins.link`.
    ///
    /// A plugin's members exist only after its first link, so a test that
    /// exercises an attached member calls this before the script that uses
    /// it.
    pub fn link_plugin(&self, dll: &str) {
        link_plugin(&self.engine, dll);
    }

    /// One frame-body transition step: the `onPaint` dispatch for the layers
    /// whose `update()` armed it, and the transition advance.
    ///
    /// The frame body runs this step before the draw, so a layer that
    /// redraws itself in `onPaint` has its new pixels in the frame the engine
    /// draws.
    pub fn advance_transitions(&self) {
        let ctx = self._host.ctx().expect("bound compat context");
        compat::advance_transitions(&ctx).expect("transition step");
    }
}

/// Link a built-in plugin on `engine` (`Plugins.link`).
///
/// The engine must already have a compat host installed. Panics with the VM's
/// message when the link raises, so a missing built-in is a test failure
/// rather than an absent member later.
pub fn link_plugin(engine: &tjs::Tjs2Engine, dll: &str) {
    engine
        .exec_script_discard(&format!("Plugins.link('{dll}');"), "compat-test-link")
        .unwrap_or_else(|error| panic!("Plugins.link({dll:?}) failed: {error}"));
}

/// Link every ported plugin on `engine`.
///
/// Test environments that exercise member dispatch need the plugin-attached
/// members present, and the reference publishes them on the first link, not
/// at engine startup. The regression that pins the pre-link absence builds
/// its own environment instead of calling this.
pub fn link_all_builtins(engine: &tjs::Tjs2Engine) {
    for name in compat::builtin_plugin_names() {
        link_plugin(engine, name);
    }
}

/// Execute `write` as a statement list, then evaluate `read` separately.
///
/// [`Tjs2Engine::eval`] compiles its input in **expression mode**: the lexer
/// appends a `;` and, when a result is needed, rewrites the first token as a
/// `return`, so a statement before the trailing expression never runs
/// (`reference/cpp/core/tjs2/tjsScriptBlock.cpp:198-243`,
/// `reference/cpp/core/tjs2/tjsLex.cpp:1006-1008,1676-1679`). A getter that
/// must observe a preceding assignment therefore needs the assignment run
/// through `exec_script` and the read evaluated afterwards. This is the
/// shared form of the local `assign_then` helper in `tests/window_ex.rs`.
pub fn eval_then(
    engine: &tjs::Tjs2Engine,
    write: &str,
    read: &str,
) -> Result<tjs::TjsValue, String> {
    engine
        .exec_script_discard(write, "compat-test")
        .map_err(|error| error.to_string())?;
    engine
        .eval(read, "compat-test")
        .map_err(|error| error.to_string())
}

/// Add a window plus one throwaway primary layer, then return a fresh
/// non-primary layer.
///
/// [`Scene::add_layer`] promotes a window's first layer to the primary layer
/// and forces it visible, mirroring the reference
/// `tTVPLayerManager::AttachPrimary`
/// (`reference/cpp/core/visual/LayerManager.cpp:161-172`). A test that
/// expects a newly created layer to read back hidden must add this throwaway
/// layer first, so the layer under test is not the force-visible primary.
pub fn non_primary_layer(scene: &mut Scene) -> u32 {
    let window = scene.add_window("compat-non-primary", (1, 1));
    scene.add_layer(window, None);
    scene.add_layer(window, None)
}

/// A unique temporary directory under the system temp dir.
pub fn unique_dir(prefix: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "{prefix}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    ));
    path
}
