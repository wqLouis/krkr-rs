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

use api::media::{DecodedImage, ImageInfo};
use compat::CompatHost;
use engine::Scene;
use engine::Storage;
use engine::input::InputState;
use engine::timer::TimerState;
use fs::FileSystem;
use platform::PlatformEvent;
use tjs::{Tjs2Engine, TjsValue};

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
/// the boxed scene / timer / input / storage the host's raw pointers refer to.
pub struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    /// Read back by tests that assert on the L2 state a native write reached.
    pub scene: Box<Scene>,
    /// The attached timer registry; tests move its injected time directly so a
    /// poll never depends on the wall clock.
    timer: RefCell<Box<TimerState>>,
    _input: Box<InputState>,
    _storage: Option<Box<Storage>>,
    _game_dir: Option<PathBuf>,
}

impl Env {
    /// A bound Env with a scene, timer registry and input state attached.
    pub fn new() -> Env {
        Env::build(false)
    }

    /// A bound Env with a mounted L2 storage (the caller writes game files
    /// into its `game_dir`).
    pub fn with_storage() -> Env {
        Env::build(true)
    }

    fn build(with_storage: bool) -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut scene = Box::new(Scene::default());
        let mut timer = Box::new(TimerState::new());
        let mut input = Box::new(InputState::new());
        let mut host = CompatHost::new();
        // SAFETY: the boxed scene, timer registry and input state outlive the
        // host (they are declared after `_host`), matching `attach_*`.
        unsafe {
            host.services_mut()
                .attach_scene(scene.as_mut() as *mut Scene);
            host.services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
            host.services_mut()
                .attach_input(input.as_mut() as *mut InputState);
        }
        let (storage, game_dir) = if with_storage {
            let dir = unique_dir("krkr-compat-test");
            std::fs::create_dir_all(&dir).expect("create temp game dir");
            let mut storage = Box::new(
                Storage::mount_archives(
                    FileSystem::host(&dir),
                    dir.clone(),
                    std::iter::empty::<&str>(),
                )
                .expect("mount storage"),
            );
            // SAFETY: boxed and declared after `_host`, so it outlives the
            // binding (same contract as the other attachments).
            unsafe { host.services_mut().attach_storage(&mut *storage) };
            (Some(storage), Some(dir))
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
            _input: input,
            _storage: storage,
            _game_dir: game_dir,
        }
    }

    /// The temp game directory of [`Env::with_storage`], when one is mounted.
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
            .exec_script_discard(script, "compat-test")
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

    /// Move the attached timer registry's injected time forward by `ms`
    /// without polling.
    pub fn advance_time(&self, ms: u64) {
        let mut timer = self.timer.borrow_mut();
        timer.now_ms = timer.now_ms.saturating_add(ms);
    }

    /// One frame-body `timer_poll` step: advance the attached timer registry by
    /// a virtual frame step, then deliver the due timers. Returns whether any
    /// timer remains armed afterwards.
    pub fn poll_timer(&self) -> bool {
        // One virtual frame step at the reference's ~60 fps.
        const FRAME_STEP_MS: u64 = 16;
        self.advance_time(FRAME_STEP_MS);
        let mut ctx = self._host.ctx().expect("bound compat context");
        compat::timer_poll(&mut ctx).expect("timer poll")
    }

    /// One frame-body `async_trigger_poll` step.
    pub fn poll_async(&self) {
        let mut ctx = self._host.ctx().expect("bound compat context");
        compat::async_trigger_poll(&mut ctx).expect("async trigger poll");
    }

    /// Feed one frame's platform events through the L3 adapter and deliver
    /// each mapped event to the script handler the adapter resolves (the
    /// [`compat::EventAdapter::dispatch`] seam the VM host's `tick` uses).
    pub fn dispatch_events(&self, events: &[PlatformEvent]) {
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

    /// Link a built-in plugin the way a game script does: `Plugins.link`.
    ///
    /// A plugin's members exist only after its first link, so a test that
    /// exercises an attached member calls this before the script that uses
    /// it.
    pub fn link_plugin(&self, dll: &str) {
        link_plugin(&self.engine, dll);
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
