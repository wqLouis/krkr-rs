//! The VM host: engine lifecycle, logging, script execution and the frame
//! body.
#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;

use compat::{CompatHost, EventAdapter, FrameBody, PluginRegistry, run_frame_body};
use engine::input::InputState;
use engine::timer::TimerState;
use platform::PlatformEvent;
use tjs::{LOG_DEBUG, LOG_ERROR, LOG_INFO, LOG_WARN, Tjs2Engine, TjsValue};

use crate::VmError;

/// The per-frame inputs handed to [`VmHost::tick`].
pub struct FrameCtx<'a> {
    /// Monotonic frame timestamp in milliseconds.
    pub now_ms: u64,
    /// Elapsed milliseconds since the previous frame.
    pub dt_ms: u32,
    /// The ordered frame-body polls (doc 08 §6).
    pub body: &'a mut dyn FrameBody,
    /// Platform events for L3 to adapt and deliver this frame.
    pub events: &'a [PlatformEvent],
}

/// The TJS VM host (L4).
///
/// Owns the C++ engine (boxed so its address is stable for the L3 binding)
/// and the L3 [`CompatHost`] that holds the native surface state. The fields
/// are ordered so the engine is dropped last: the dispatch state retains
/// values whose `Drop` releases an id on the engine.
pub struct VmHost {
    input: Box<InputState>,
    timer: Box<TimerState>,
    /// The L3 dispatch state. Declared before `engine` so its retained values
    /// are released first (fields drop in declaration order).
    compat: CompatHost,
    adapter: EventAdapter,
    /// The C++ engine. Declared last so it outlives every value the native
    /// state retains.
    engine: Box<Tjs2Engine>,
}

extern "C" fn vm_log(level: c_int, msg: *const c_char, _user: *mut c_void) {
    if msg.is_null() {
        return;
    }
    // SAFETY: the VM guarantees a NUL-terminated UTF-8 string valid for the
    // duration of the callback.
    let msg = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
    match level {
        LOG_ERROR => log::error!("tjs: {msg}"),
        LOG_WARN => log::warn!("tjs: {msg}"),
        LOG_INFO => log::info!("tjs: {msg}"),
        LOG_DEBUG => log::debug!("tjs: {msg}"),
        _ => log::trace!("tjs: {msg}"),
    }
}

impl VmHost {
    /// Create a host with the built-in plugin registry.
    pub fn with_default() -> Result<Self, VmError> {
        Self::new(PluginRegistry::builtin())
    }

    /// Create a host with the given plugin registry.
    ///
    /// The host boxes the engine and installs the L3 binding/classes before
    /// returning, so it is ready to [`exec`](Self::exec) and
    /// [`tick`](Self::tick).
    pub fn new(registry: PluginRegistry) -> Result<Self, VmError> {
        let engine = Box::new(Tjs2Engine::new().map_err(|err| VmError::Vm(err.to_string()))?);
        // SAFETY: the user pointer is null; the callback reads only `msg`.
        unsafe { engine.set_log_cb(Some(vm_log), ptr::null_mut()) };
        // The L2 services the natives compose are boxed so their addresses
        // stay stable for the binding; the engine is declared last so it
        // outlives every retained value.
        let mut input = Box::new(InputState::new());
        let mut timer = Box::new(TimerState::new());
        let mut compat = CompatHost::with_registry(registry);
        compat.install(&engine)?;
        // SAFETY: `input`/`timer` live for the whole `VmHost` and no reference to
        // them escapes past its drop; the binding is removed first.
        unsafe {
            compat
                .services_mut()
                .attach_input(input.as_mut() as *mut InputState);
            compat
                .services_mut()
                .attach_timer(timer.as_mut() as *mut TimerState);
        }
        Ok(Self {
            input,
            timer,
            compat,
            adapter: EventAdapter::new(),
            engine,
        })
    }

    /// The underlying TJS engine.
    pub fn engine(&self) -> &Tjs2Engine {
        &self.engine
    }

    /// The L3 compatibility host.
    pub fn compat(&self) -> &CompatHost {
        &self.compat
    }

    /// The L3 compatibility host, mutably.
    pub fn compat_mut(&mut self) -> &mut CompatHost {
        &mut self.compat
    }

    /// The L2 input state the input natives read.
    pub fn input(&self) -> &InputState {
        &self.input
    }

    /// The L2 input state, mutably (the host may drive it directly).
    pub fn input_mut(&mut self) -> &mut InputState {
        &mut self.input
    }

    /// The L2 timer registry the `Timer` native composes.
    pub fn timer(&self) -> &TimerState {
        &self.timer
    }

    /// The L2 timer registry, mutably.
    pub fn timer_mut(&mut self) -> &mut TimerState {
        &mut self.timer
    }

    /// Execute a TJS script.
    ///
    /// The `Storages` native and the boot sequence are a later wave; this is
    /// the raw execution seam they will call.
    pub fn exec(&self, source: &str, name: &str) -> Result<TjsValue, VmError> {
        self.engine
            .exec_script(source, name)
            .map_err(|err| VmError::Script(err.to_string()))
    }

    /// Evaluate a TJS expression.
    pub fn eval(&self, expression: &str, name: &str) -> Result<TjsValue, VmError> {
        self.engine
            .eval(expression, name)
            .map_err(|err| VmError::Script(err.to_string()))
    }

    /// Run one frame body: adapt/deliver platform events, then run the
    /// ordered polls (doc 08 §6).
    pub fn tick(&mut self, frame: FrameCtx<'_>) -> Result<(), VmError> {
        let mut ctx = self.compat.ctx()?;
        // The input state is per-frame: clear it, fold in every raw event,
        // then commit so held/repeat counters advance exactly once (doc 08 §6).
        self.adapter.begin_input_frame(&mut ctx);
        for event in frame.events {
            self.adapter.apply_input_event(&mut ctx, event);
            if let Some(mapped) = self.adapter.map(event) {
                self.adapter.dispatch(&mut ctx, &mapped)?;
            }
        }
        self.adapter.end_input_frame(&mut ctx);
        run_frame_body(frame.body, &mut ctx)?;
        Ok(())
    }
}

impl Drop for VmHost {
    fn drop(&mut self) {
        // Release the native state's retained values while the engine is
        // still alive, then remove the binding. Dropping the state after the
        // engine is destroyed would release ids against a freed engine.
        self.compat.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tjs::{DetachedValue, RetainedValue};

    use super::*;

    static STATE_DROPS: AtomicUsize = AtomicUsize::new(0);

    /// Native dispatch state that holds a value retained on the engine. Its
    /// `Drop` records that teardown reached the state; the retained value's
    /// own `Drop` then releases the id on the engine.
    #[derive(Default)]
    struct TeardownProbe {
        value: Option<DetachedValue>,
    }

    impl Drop for TeardownProbe {
        fn drop(&mut self) {
            STATE_DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }

    /// Dropping the host must release the native dispatch state while the
    /// engine is still alive. The dropped `DetachedValue` releases its id on
    /// the engine, so a teardown order that freed the engine first would use
    /// the freed pointer.
    #[test]
    fn dropping_the_host_releases_native_state_before_the_engine() {
        let _lock = testkit::vm_lock();
        let host = VmHost::with_default().expect("vm host");
        let value = host
            .engine()
            .eval_retained("[]", "teardown")
            .expect("retain an object");
        let RetainedValue::Object(object) = value else {
            panic!("eval_retained must return an object");
        };
        host.compat().services().state::<TeardownProbe>().value = Some(object);
        STATE_DROPS.store(0, Ordering::SeqCst);
        drop(host);
        assert_eq!(
            STATE_DROPS.load(Ordering::SeqCst),
            1,
            "VmHost::drop must drop the native dispatch state"
        );
    }

    /// A caught script exception travels the full path: the VM's spdlog
    /// record is bridged to the engine callback, which the host routes to
    /// the `log` crate. The record must be DEBUG and carry the exception
    /// message. Reference: `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1311`.
    #[test]
    fn caught_exception_reaches_the_engine_log() {
        use std::cell::RefCell;
        use std::ffi::{c_char, c_int, c_void};
        use tjs::{NativeClassBuilder, NativeMethodDef, Value};

        thread_local! {
            static LINES: RefCell<Vec<String>> =
                const { RefCell::new(Vec::new()) };
        }
        struct CaptureLogger;
        impl log::Log for CaptureLogger {
            fn enabled(&self, _metadata: &log::Metadata) -> bool {
                true
            }
            fn log(&self, record: &log::Record) {
                LINES.with(|lines| {
                    lines
                        .borrow_mut()
                        .push(format!("{} {}", record.level(), record.args()));
                });
            }
            fn flush(&self) {}
        }
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            let _ = log::set_boxed_logger(Box::new(CaptureLogger));
            log::set_max_level(log::LevelFilter::Debug);
        });

        // A native that always fails: the VM raises it as an `eTJS` error,
        // so a surrounding `try` reaches the logging catch arm.
        extern "C" fn boom(
            _engine: *mut c_void,
            _argc: c_int,
            _argv: *const Value,
            _out: *mut Value,
            out_error: *mut *mut c_char,
        ) -> c_int {
            const MSG: &[u8] = b"boom-message\0";
            // SAFETY: tjs2_malloc returns a writable buffer of the requested
            // size, and out_error is a valid slot for the call.
            unsafe {
                let p = tjs::tjs2_malloc(MSG.len()) as *mut u8;
                std::ptr::copy_nonoverlapping(MSG.as_ptr(), p, MSG.len());
                *out_error = p as *mut c_char;
            }
            1
        }

        let _lock = testkit::vm_lock();
        let host = VmHost::with_default().expect("vm host");
        host.engine()
            .register_native_class(&NativeClassBuilder {
                name: "Boom",
                methods: vec![NativeMethodDef {
                    name: "fire",
                    f: boom,
                }],
                properties: Vec::new(),
            })
            .expect("register Boom");

        LINES.with(|lines| lines.borrow_mut().clear());
        host.exec(
            "var r = 0; try { Boom.fire(); } catch(e) { r = 1; }",
            "test",
        )
        .expect("exec");
        assert_eq!(host.eval("r", "test").unwrap(), TjsValue::Integer(1));

        let lines = LINES.with(|lines| lines.borrow().clone());
        // The bare message is the record the spdlog bridge adds; a debug-mode
        // build also emits a `... at position` console record, which does not
        // match exactly.
        assert!(
            lines.iter().any(|line| line == "DEBUG tjs: boom-message"),
            "the caught exception must reach the engine log: {lines:?}"
        );
    }
}
