//! `Debug` native class.
//!
//! Behavioral reference: `main`'s `crates/tvp-natives/src/debug.rs` (itself a
//! port of `reference/cpp/core/utils/DebugIntf.cpp`). This is a
//! reimplementation on the layered API: all logging state lives in
//! [`CompatServices::state`], never in a process global.
//!
//! # Implemented members
//!
//! Methods: `message`, `notice`, `startLogToFile`, `logAsError`,
//! `addLoggingHandler`, `removeLoggingHandler`, `getLastLog`.
//!
//! Properties: `logLocation`, `logToFileOnError`, `clearLogFileOnError`,
//! `controller`, `console`.
//!
//! # `controller` / `console`
//!
//! The reference returns the `Controller`/`Console` singleton class objects,
//! whose only member is the `visible` flag (a no-op in the reference UI too).
//! The port registers both as instance classes backed by the existing
//! [`DebugState`] visibility fields and returns a lazily created, cached
//! singleton from each property — no second state store, no process global.
//! A script that writes `Debug.console.visible` therefore sees the same value
//! through every reference to the object.
//!
//! # Log flow
//!
//! `message`/`notice` write to the `log` crate (`message` at `info!` — or
//! `error!` while [`logAsError`] is true — `notice` at `debug!`), append to the
//! file opened by `startLogToFile`, push to an in-memory ring buffer (cap 100)
//! read by `getLastLog`, and dispatch the string to every retained
//! `addLoggingHandler` callback. The handlers are matched by closure identity,
//! so `removeLoggingHandler` retires exactly the callback it names and a
//! duplicate registration is ignored.

use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_void};
use std::fs::{File, OpenOptions};
use std::io::Write;

use tjs::{
    DetachedValue, NativeClassBuilder, NativeInstanceBuilder, NativeInstanceMethodDef,
    NativeInstancePropertyDef, NativeMethodDef, NativePropertyDef, RetainedValue, Tjs2Engine,
    TjsValue, VAL_OBJECT, VAL_RETAINED, Value,
};

use super::system::{resolve, set_string_out, value_as_bool, value_as_i64};
use crate::native_util::{args, report_error, set_bool, set_void, value_as_string};
use crate::{CompatCtx, CompatError, CompatServices};

/// Maximum number of entries kept in the `getLastLog` ring buffer.
const LOG_BUFFER_CAP: usize = 100;

/// One retained `addLoggingHandler` callback: the closure plus its
/// `(Object, ObjThis)` identity, so removal matches exactly.
struct LoggingHandler {
    value: DetachedValue,
    key: (usize, usize),
}

/// `Debug` dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
#[derive(Default)]
struct DebugState {
    /// File opened by `startLogToFile`; `message`/`notice` append to it.
    log_file: Option<File>,
    /// Ring buffer of recent `message`/`notice` strings.
    log_buffer: VecDeque<String>,
    /// When true, `message` logs at `error!` instead of `info!`.
    log_as_error: bool,
    /// `Debug.logLocation`.
    log_location: String,
    /// `Debug.logToFileOnError`.
    log_to_file_on_error: bool,
    /// `Debug.clearLogFileOnError`.
    clear_log_file_on_error: bool,
    /// Retained `addLoggingHandler` callbacks.
    handlers: Vec<LoggingHandler>,
    /// Lazily created `Debug.console` singleton.
    console: Option<DetachedValue>,
    /// Lazily created `Debug.controller` singleton.
    controller: Option<DetachedValue>,
    /// `Console.visible` (a no-op in the reference UI).
    console_visible: bool,
    /// `Controller.visible` (a no-op in the reference UI).
    controller_visible: bool,
}

/// Join the callback arguments with `", "`, mirroring the reference's
/// multi-argument `message`/`notice` handling.
fn join_args(a: &[Value]) -> String {
    let parts: Vec<String> = a.iter().map(value_as_string).collect();
    parts.join(", ")
}

/// Push `text` into the ring buffer, dropping the oldest entry past the cap.
fn push_log(services: &CompatServices, text: &str) {
    let mut state = services.state::<DebugState>();
    if state.log_buffer.len() >= LOG_BUFFER_CAP {
        state.log_buffer.pop_front();
    }
    state.log_buffer.push_back(text.to_string());
}

/// Append `text` to the file opened by `startLogToFile`, if any.
fn write_log_file(services: &CompatServices, text: &str) {
    let mut state = services.state::<DebugState>();
    if let Some(file) = state.log_file.as_mut() {
        let _ = writeln!(file, "{text}");
        let _ = file.flush();
    }
}

/// Dispatch `text` to every registered logging handler. The handler list is
/// moved out of the state while the callbacks run so a handler that itself
/// logs cannot borrow the state re-entrantly.
fn dispatch_logging_handlers(ctx: &CompatCtx, text: &str) {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let handlers = {
        let mut state = services.state::<DebugState>();
        if state.handlers.is_empty() {
            return;
        }
        std::mem::take(&mut state.handlers)
    };
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    for handler in &handlers {
        if let Err(e) = engine.call_detached(&handler.value, &[TjsValue::String(text.to_string())])
        {
            log::debug!("Debug logging handler failed: {e}");
        }
    }
    // Restore the handlers unless a callback replaced the list.
    let mut state = services.state::<DebugState>();
    if state.handlers.is_empty() {
        state.handlers = handlers;
    }
}

/// `Debug.message(arg, ...)` → void
extern "C" fn native_message(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.message requires 1 argument");
    }
    let text = join_args(unsafe { args(argv, argc) });
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Debug.message: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    push_log(services, &text);
    write_log_file(services, &text);
    let as_error = services.state::<DebugState>().log_as_error;
    dispatch_logging_handlers(&ctx, &text);
    if as_error {
        log::error!("{text}");
    } else {
        log::info!("{text}");
    }
    set_void(out);
    0
}

/// `Debug.notice(arg, ...)` → void
extern "C" fn native_notice(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.notice requires 1 argument");
    }
    let text = join_args(unsafe { args(argv, argc) });
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Debug.notice: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    push_log(services, &text);
    write_log_file(services, &text);
    dispatch_logging_handlers(&ctx, &text);
    log::debug!("{text}");
    set_void(out);
    0
}

/// `Debug.startLogToFile(filename)` → void
///
/// Opens `filename` in append mode; subsequent `message`/`notice` calls also
/// write there. A second call while already logging is ignored (reference
/// behavior); an open failure logs a warning and leaves logging off.
extern "C" fn native_start_log_to_file(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.startLogToFile requires 1 argument");
    }
    let filename = value_as_string(&unsafe { args(argv, argc) }[0]);
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Debug.startLogToFile: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let already_open = services.state::<DebugState>().log_file.is_some();
    if already_open {
        set_void(out);
        return 0;
    }
    match OpenOptions::new().create(true).append(true).open(&filename) {
        Ok(file) => services.state::<DebugState>().log_file = Some(file),
        Err(e) => log::warn!("Debug.startLogToFile: failed to open {filename:?}: {e}"),
    }
    set_void(out);
    0
}

/// `Debug.logAsError([flag])` → void (flag defaults to true).
extern "C" fn native_log_as_error(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let flag = if argc >= 1 {
        value_as_bool(&unsafe { args(argv, argc) }[0])
    } else {
        true
    };
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Debug.logAsError: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }.state::<DebugState>().log_as_error = flag;
    set_void(out);
    0
}

/// `Debug.getLastLog([lines])` → string
///
/// Without an argument returns the most recent entry (or `""`); with one,
/// joins up to `lines` most-recent entries with `\n`.
extern "C" fn native_get_last_log(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let lines = if argc >= 1 {
        value_as_i64(&unsafe { args(argv, argc) }[0]).max(0) as usize
    } else {
        1
    };
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Debug.getLastLog: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let text = {
        let state = services.state::<DebugState>();
        let lines = lines.min(state.log_buffer.len());
        if lines == 0 {
            String::new()
        } else {
            let entries: Vec<&str> = state
                .log_buffer
                .iter()
                .rev()
                .take(lines)
                .rev()
                .map(String::as_str)
                .collect();
            entries.join("\n")
        }
    };
    set_string_out(&ctx, out, &text);
    0
}

/// `Debug.addLoggingHandler(fn)` → void
extern "C" fn native_add_logging_handler(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.addLoggingHandler requires 1 argument");
    }
    let a = unsafe { args(argv, argc) };
    if a[0].ty != VAL_OBJECT {
        set_void(out);
        return 0;
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Debug.addLoggingHandler: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    let value = match resolved.engine().retain_object_arg(&a[0]) {
        Ok(value) => value,
        Err(e) => {
            return report_error(
                out_error,
                &format!("Debug.addLoggingHandler: cannot retain the handler: {e}"),
            );
        }
    };
    let key = (
        a[0].object_handle() as usize,
        a[0].object_objthis() as usize,
    );
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let mut state = unsafe { ctx.services() }.state::<DebugState>();
    if !state.handlers.iter().any(|h| h.key == key) {
        state.handlers.push(LoggingHandler { value, key });
    }
    drop(state);
    set_void(out);
    0
}

/// `Debug.removeLoggingHandler(fn)` → void
extern "C" fn native_remove_logging_handler(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.removeLoggingHandler requires 1 argument");
    }
    let a = unsafe { args(argv, argc) };
    if a[0].ty != VAL_OBJECT {
        set_void(out);
        return 0;
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Debug.removeLoggingHandler: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    let key = (
        a[0].object_handle() as usize,
        a[0].object_objthis() as usize,
    );
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<DebugState>()
        .handlers
        .retain(|h| h.key != key);
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Debug.controller / Debug.console
// ---------------------------------------------------------------------------

/// Which `Debug` singleton property is being read.
#[derive(Clone, Copy)]
enum DebugWindow {
    Console,
    Controller,
}

impl DebugWindow {
    /// The TJS property name (`console` / `controller`).
    fn member(self) -> &'static str {
        match self {
            DebugWindow::Console => "console",
            DebugWindow::Controller => "controller",
        }
    }

    /// The registered native class name.
    fn class(self) -> &'static str {
        match self {
            DebugWindow::Console => "Console",
            DebugWindow::Controller => "Controller",
        }
    }
}

/// Duplicate a cached object into `out`; the C++ trampoline consumes the
/// retention while converting the native result.
fn set_object_out(host: &Tjs2Engine, out: *mut Value, object: Option<&DetachedValue>) {
    let Some(object) = object else {
        set_void(out);
        return;
    };
    match host.duplicate_retained(object.raw_id()) {
        Ok(duplicate) => {
            if !out.is_null() {
                // SAFETY: the trampoline passes a valid out slot for the call.
                unsafe {
                    (*out).ty = VAL_RETAINED;
                    (*out).integer = 0;
                    (*out).real = 0.0;
                    (*out).string = std::ptr::null();
                    (*out).array = std::ptr::null();
                    (*out).array_count = 0;
                    (*out).retained = duplicate.raw_id() as usize;
                }
            }
            // The C++ conversion consumed the retention.
            std::mem::forget(duplicate);
        }
        Err(error) => {
            log::warn!("Debug: could not duplicate a singleton object: {error}");
            set_void(out);
        }
    }
}

/// Return the lazily created `Debug.console` / `Debug.controller` singleton.
fn debug_window_property(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    which: DebugWindow,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            &format!("Debug.{}: compat context is not bound", which.member()),
        );
    };
    let ctx = resolved.ctx();
    let host = resolved.engine();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let cached = {
        let state = services.state::<DebugState>();
        match which {
            DebugWindow::Console => state.console.is_some(),
            DebugWindow::Controller => state.controller.is_some(),
        }
    };
    if !cached {
        let class = which.class();
        let object = match host.eval_retained(&format!("new {class}()"), "Debug") {
            Ok(RetainedValue::Object(object)) => object,
            Ok(RetainedValue::Value(_)) => {
                return report_error(
                    out_error,
                    &format!("Debug.{}: unexpected scalar result", which.member()),
                );
            }
            Err(error) => {
                return report_error(out_error, &format!("Debug.{}: {error}", which.member()));
            }
        };
        let mut state = services.state::<DebugState>();
        match which {
            DebugWindow::Console => state.console = Some(object),
            DebugWindow::Controller => state.controller = Some(object),
        }
    }
    let state = services.state::<DebugState>();
    let object = match which {
        DebugWindow::Console => state.console.as_ref(),
        DebugWindow::Controller => state.controller.as_ref(),
    };
    set_object_out(host, out, object);
    0
}

/// The `Console`/`Controller` native-instance payload.
///
/// The reference classes carry no state beyond `visible`; the visibility
/// flags live in [`DebugState`] so the `Debug.*` singletons and any
/// script-created instance observe the same value.
#[derive(Default)]
struct DebugWindowState {
    /// Keeps the allocation non-zero-sized (all state is in [`DebugState`]).
    _private: u8,
}

extern "C" fn debug_window_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(DebugWindowState::default())) as *mut c_void
}

extern "C" fn debug_window_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: `instance` came from `debug_window_create` and is released once.
        drop(unsafe { Box::from_raw(instance as *mut DebugWindowState) });
    }
}

extern "C" fn debug_window_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

/// Shared body of the `Console.visible` / `Controller.visible` getters.
fn debug_visible_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    class: &str,
    pick: impl FnOnce(&DebugState) -> bool,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            &format!("{class}.visible: compat context is not bound"),
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let value = {
        let state = unsafe { ctx.services() }.state::<DebugState>();
        pick(&state)
    };
    set_bool(out, value);
    0
}

/// Shared body of the `Console.visible` / `Controller.visible` setters.
fn debug_visible_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    class: &str,
    set: impl FnOnce(&mut DebugState, bool),
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            &format!("{class}.visible: compat context is not bound"),
        );
    };
    if value.is_null() {
        return report_error(out_error, &format!("{class}.visible: missing value"));
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let flag = value_as_bool(unsafe { &*value });
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let mut state = unsafe { ctx.services() }.state::<DebugState>();
    set(&mut state, flag);
    0
}

extern "C" fn console_visible_get(
    engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    debug_visible_get(engine, out, out_error, "Console", |state| {
        state.console_visible
    })
}

extern "C" fn console_visible_set(
    engine: *mut c_void,
    _instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    debug_visible_set(engine, value, out_error, "Console", |state, flag| {
        state.console_visible = flag;
    })
}

extern "C" fn controller_visible_get(
    engine: *mut c_void,
    _instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    debug_visible_get(engine, out, out_error, "Controller", |state| {
        state.controller_visible
    })
}

extern "C" fn controller_visible_set(
    engine: *mut c_void,
    _instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    debug_visible_set(engine, value, out_error, "Controller", |state, flag| {
        state.controller_visible = flag;
    })
}

// ---------------------------------------------------------------------------
// Debug properties
// ---------------------------------------------------------------------------

extern "C" fn prop_log_location_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Debug.logLocation: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let location = unsafe { ctx.services() }
        .state::<DebugState>()
        .log_location
        .clone();
    set_string_out(&ctx, out, &location);
    0
}

extern "C" fn prop_log_location_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Debug.logLocation: compat context is not bound");
    };
    if value.is_null() {
        return report_error(out_error, "Debug.logLocation: missing value");
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let location = value_as_string(unsafe { &*value });
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }.state::<DebugState>().log_location = location;
    0
}

extern "C" fn prop_log_to_file_on_error_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Debug.logToFileOnError: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let flag = unsafe { ctx.services() }
        .state::<DebugState>()
        .log_to_file_on_error;
    set_bool(out, flag);
    0
}

extern "C" fn prop_log_to_file_on_error_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Debug.logToFileOnError: compat context is not bound",
        );
    };
    if value.is_null() {
        return report_error(out_error, "Debug.logToFileOnError: missing value");
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let flag = value_as_bool(unsafe { &*value });
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<DebugState>()
        .log_to_file_on_error = flag;
    0
}

extern "C" fn prop_clear_log_file_on_error_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Debug.clearLogFileOnError: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let flag = unsafe { ctx.services() }
        .state::<DebugState>()
        .clear_log_file_on_error;
    set_bool(out, flag);
    0
}

extern "C" fn prop_clear_log_file_on_error_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Debug.clearLogFileOnError: compat context is not bound",
        );
    };
    if value.is_null() {
        return report_error(out_error, "Debug.clearLogFileOnError: missing value");
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let flag = value_as_bool(unsafe { &*value });
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<DebugState>()
        .clear_log_file_on_error = flag;
    0
}

/// `Debug.console` — the singleton `Console` object.
extern "C" fn prop_console(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    debug_window_property(engine, out, out_error, DebugWindow::Console)
}

/// `Debug.controller` — the singleton `Controller` object.
extern "C" fn prop_controller(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    debug_window_property(engine, out, out_error, DebugWindow::Controller)
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `Debug` native class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_class(&NativeClassBuilder {
        name: "Debug",
        methods: vec![
            NativeMethodDef {
                name: "message",
                f: native_message,
            },
            NativeMethodDef {
                name: "notice",
                f: native_notice,
            },
            NativeMethodDef {
                name: "startLogToFile",
                f: native_start_log_to_file,
            },
            NativeMethodDef {
                name: "logAsError",
                f: native_log_as_error,
            },
            NativeMethodDef {
                name: "addLoggingHandler",
                f: native_add_logging_handler,
            },
            NativeMethodDef {
                name: "removeLoggingHandler",
                f: native_remove_logging_handler,
            },
            NativeMethodDef {
                name: "getLastLog",
                f: native_get_last_log,
            },
        ],
        properties: vec![
            NativePropertyDef {
                name: "logLocation",
                get: Some(prop_log_location_get),
                set: Some(prop_log_location_set),
            },
            NativePropertyDef {
                name: "logToFileOnError",
                get: Some(prop_log_to_file_on_error_get),
                set: Some(prop_log_to_file_on_error_set),
            },
            NativePropertyDef {
                name: "clearLogFileOnError",
                get: Some(prop_clear_log_file_on_error_get),
                set: Some(prop_clear_log_file_on_error_set),
            },
            NativePropertyDef {
                name: "controller",
                get: Some(prop_controller),
                set: None,
            },
            NativePropertyDef {
                name: "console",
                get: Some(prop_console),
                set: None,
            },
        ],
    })?;
    register_debug_window_classes(ctx)
}

/// Register the nested `Controller` and `Console` classes.
///
/// The reference exposes them only through `Debug.controller`/`Debug.console`,
/// but registering them also lets a script write `new Console()`. Each owns no
/// state; the `visible` flag lives in [`DebugState`].
fn register_debug_window_classes(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Controller",
        create: debug_window_create,
        destroy: debug_window_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "Controller",
            f: debug_window_ctor,
        }],
        properties: vec![NativeInstancePropertyDef {
            name: "visible",
            get: Some(controller_visible_get),
            set: Some(controller_visible_set),
        }],
    })?;
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Console",
        create: debug_window_create,
        destroy: debug_window_destroy,
        invalidate: None,
        methods: vec![NativeInstanceMethodDef {
            name: "Console",
            f: debug_window_ctor,
        }],
        properties: vec![NativeInstancePropertyDef {
            name: "visible",
            get: Some(console_visible_get),
            set: Some(console_visible_set),
        }],
    })?;
    Ok(())
}
