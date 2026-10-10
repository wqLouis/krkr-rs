//! Shared helpers for the `compat` integration tests.
//!
//! `vm_lock` serializes tests that touch the process-global TJS VM; [`Env`]
//! binds a VM + [`CompatHost`] + the L2 scene/timer/input (and optionally an
//! L2 storage) the same way the app does, so native dispatch can be exercised
//! through the real seams. The module is compiled into every `compat` test
//! binary, so items a particular binary does not use are allowed dead.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::Duration;

use compat::CompatHost;
use engine::Scene;
use engine::Storage;
use engine::input::InputState;
use engine::timer::TimerState;
use fs::FileSystem;
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
    _timer: Box<TimerState>,
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
        Env {
            _host: host,
            engine,
            scene,
            _timer: timer,
            _input: input,
            _storage: storage,
            _game_dir: game_dir,
        }
    }

    /// The temp game directory of [`Env::with_storage`], when one is mounted.
    pub fn game_dir(&self) -> Option<&PathBuf> {
        self._game_dir.as_ref()
    }

    pub fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "compat-test")
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

    /// One frame-body `timer_poll` step; returns whether any timer remains
    /// armed afterwards.
    pub fn poll_timer(&self) -> bool {
        let mut ctx = self._host.ctx().expect("bound compat context");
        compat::timer_poll(&mut ctx).expect("timer poll")
    }

    /// One frame-body `async_trigger_poll` step.
    pub fn poll_async(&self) {
        let mut ctx = self._host.ctx().expect("bound compat context");
        compat::async_trigger_poll(&mut ctx).expect("async trigger poll");
    }
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

/// Sleep helper for tests that poll a real-time L2 timer.
pub fn sleep_ms(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}
