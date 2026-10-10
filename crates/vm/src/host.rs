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
/// and the L3 [`CompatHost`] that holds the native surface state.
pub struct VmHost {
    engine: Box<Tjs2Engine>,
    input: Box<InputState>,
    timer: Box<TimerState>,
    compat: CompatHost,
    adapter: EventAdapter,
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
        // The L2 services the natives compose are boxed so their addresses stay
        // stable for the binding, and they are declared after the engine / before
        // the compat host so the L3 binding never outlives them.
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
            engine,
            input,
            timer,
            compat,
            adapter: EventAdapter::new(),
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
        // Remove the L3 binding while the engine is still alive: native
        // teardown during `Tjs2Engine::drop` must not resolve a stale
        // binding (and `CompatHost`'s own Drop then becomes a no-op).
        self.compat.uninstall();
    }
}
