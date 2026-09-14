//! `Debug` native class — ported from
//! `reference/cpp/core/utils/DebugIntf.cpp` (`tTJSNC_Debug`).
//!
//! Implemented methods: `message`, `notice`, `startLogToFile`, `logAsError`,
//! `getLastLog`, `addLoggingHandler`, `removeLoggingHandler` (the handler
//! list is real: each `message`/`notice` string is dispatched to the
//! retained callbacks). The properties `logLocation`, `logToFileOnError` and
//! `clearLogFileOnError` are stored (the reference's auto-log-to-file error
//! path is not wired to the host yet).
//!
//! Log flow: `message`/`notice` write to the `log` crate (`message` at
//! `info!` — or `error!` while [`logAsError`] is true — `notice` at
//! `debug!`), append to the file opened by [`startLogToFile`], and push to
//! an in-memory ring buffer (cap 100) read by [`getLastLog`]. The state is
//! process-global (the reference's log system is process-wide too).
//!
//! Deviations from the reference:
//!
//! * `startLogToFile(clear: bool)` in the reference starts logging to the
//!   engine's configured log file; this port takes a **filename** and
//!   appends to it in the current directory (task-specified API). A second
//!   call while already logging is ignored, like the reference's
//!   `TVPLoggingToFile` guard.
//! * `logAsError()` in the reference triggers error handling
//!   (`TVPOnError`); this port stores a flag that switches `message` to
//!   `error!`. The flag argument is optional and defaults to true.
//! * `getLastLog(lines)` in the reference returns up to `lines` recent
//!   entries, each prefixed with a timestamp. This port returns the single
//!   most recent entry by default (task-specified) and joins up to `lines`
//!   entries with `\n` when an argument is given, without timestamps.

use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_void};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use tjs2_sys::{DetachedValue, NativeClassBuilder, NativeMethodDef, NativePropertyDef, Value};

use super::{
    args, context_engine, lock_ok, report_error, set_int_out, set_object_result, set_string_out,
    set_void_out, value_as_bool, value_as_i64, value_as_string,
};

/// Handle of the file opened by `startLogToFile`; `message`/`notice` append
/// to it while it is set.
static LOG_FILE: Mutex<Option<File>> = Mutex::new(None);

/// Ring buffer of recent `message`/`notice` strings, read by `getLastLog`.
static LOG_BUFFER: Mutex<VecDeque<String>> = Mutex::new(VecDeque::new());

/// Maximum number of entries kept in [`LOG_BUFFER`].
const LOG_BUFFER_CAP: usize = 100;

/// When true, `message` logs at `error!` instead of `info!`.
static LOG_AS_ERROR: AtomicBool = AtomicBool::new(false);

/// `Debug.logLocation` (reference `TVPLogLocation`; a file/directory the
/// reference writes error logs to).
static LOG_LOCATION: Mutex<String> = Mutex::new(String::new());

/// `Debug.logToFileOnError` (reference `TVPAutoLogToFileOnError`).
static LOG_TO_FILE_ON_ERROR: AtomicBool = AtomicBool::new(false);

/// `Debug.clearLogFileOnError` (reference `TVPAutoClearLogOnError`).
static CLEAR_LOG_FILE_ON_ERROR: AtomicBool = AtomicBool::new(false);

/// Retained `addLoggingHandler` callbacks; each `message`/`notice` string is
/// dispatched to every handler (reference `TVPAddLoggingHandler`).
static LOGGING_HANDLERS: Mutex<Vec<DetachedValue>> = Mutex::new(Vec::new());

/// Dispatch `text` to every registered logging handler (errors are logged
/// and the handler is kept, like the reference). The handler list is moved
/// out of the mutex while callbacks run so a handler that itself logs cannot
/// deadlock on a re-entrant lock.
fn dispatch_logging_handlers(text: &str) {
    let handlers = {
        let mut guard = lock_ok(&LOGGING_HANDLERS);
        if guard.is_empty() {
            return;
        }
        std::mem::take(&mut *guard)
    };
    let engine = context_engine();
    for handler in &handlers {
        if let Err(e) = engine.call_detached(handler, &[tjs2_sys::TjsValue::String(text.into())]) {
            log::debug!("Debug logging handler failed: {e}");
        }
    }
    // Restore the handlers unless a callback replaced the list (re-entrant
    // add/remove while logging is not expected; new handlers win).
    let mut guard = lock_ok(&LOGGING_HANDLERS);
    if guard.is_empty() {
        *guard = handlers;
    }
}

/// Join the callback arguments into one string with `", "` separators,
/// mirroring the reference's `message`/`notice` multi-argument handling.
fn join_args(a: &[Value]) -> String {
    let parts: Vec<String> = a.iter().map(value_as_string).collect();
    parts.join(", ")
}

/// Push `s` into the ring buffer, dropping the oldest entry past the cap.
fn push_log(s: String) {
    let mut buf = lock_ok(&LOG_BUFFER);
    if buf.len() >= LOG_BUFFER_CAP {
        buf.pop_front();
    }
    buf.push_back(s);
}

/// Append `s` to the file opened by `startLogToFile`, if any.
fn write_log_file(s: &str) {
    let mut guard = lock_ok(&LOG_FILE);
    if let Some(file) = guard.as_mut() {
        let _ = writeln!(file, "{s}");
        let _ = file.flush();
    }
}

/// `Debug.message(arg, ...)` → void
///
/// Reference: `TVPAddLog` — one argument is logged as-is, several are joined
/// with `", "`. Logs at `info!` (or `error!` while `logAsError` is true),
/// appends to the `startLogToFile` file, and records the string in the
/// `getLastLog` ring buffer.
extern "C" fn native_message(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.message requires 1 argument");
    }
    let a = args(argv, argc);
    let text = join_args(a);
    push_log(text.clone());
    write_log_file(&text);
    dispatch_logging_handlers(&text);
    if LOG_AS_ERROR.load(Ordering::Relaxed) {
        log::error!("{text}");
    } else {
        log::info!("{text}");
    }
    set_void_out(out);
    0
}

/// `Debug.notice(arg, ...)` → void
///
/// Reference: `TVPAddImportantLog` (same argument joining as `message`).
/// Logs at `debug!`, appends to the `startLogToFile` file, and records the
/// string in the `getLastLog` ring buffer.
extern "C" fn native_notice(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.notice requires 1 argument");
    }
    let a = args(argv, argc);
    let text = join_args(a);
    push_log(text.clone());
    write_log_file(&text);
    dispatch_logging_handlers(&text);
    log::debug!("{text}");
    set_void_out(out);
    0
}

/// `Debug.startLogToFile(filename)` → void
///
/// Opens `filename` (in the current directory) in append mode and records
/// the handle; subsequent `message`/`notice` calls also write there. A
/// second call while already logging is ignored (reference behavior). On
/// open failure a warning is logged and logging stays off.
///
/// Note: the reference's `startLogToFile(clear: bool)` starts logging to the
/// engine's configured log file; this port uses the task-specified filename
/// API.
extern "C" fn native_start_log_to_file(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.startLogToFile requires 1 argument");
    }
    let a = args(argv, argc);
    let filename = value_as_string(&a[0]);
    let mut guard = lock_ok(&LOG_FILE);
    if guard.is_some() {
        // already logging — reference: `if(TVPLoggingToFile) return;`
        set_void_out(out);
        return 0;
    }
    match OpenOptions::new().create(true).append(true).open(&filename) {
        Ok(file) => *guard = Some(file),
        Err(e) => log::warn!("Debug.startLogToFile: failed to open {filename:?}: {e}"),
    }
    set_void_out(out);
    0
}

/// `Debug.logAsError([flag])` → void
///
/// Sets the flag that switches `message` to the `error!` level; the flag
/// defaults to true when the argument is omitted. Returns void.
///
/// Note: the reference's `logAsError()` triggers error handling
/// (`TVPOnError`); this port repurposes it as the flag setter specified by
/// the task.
extern "C" fn native_log_as_error(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let a = args(argv, argc);
    let flag = if argc >= 1 {
        value_as_bool(&a[0])
    } else {
        true
    };
    LOG_AS_ERROR.store(flag, Ordering::Relaxed);
    set_void_out(out);
    0
}

/// `Debug.getLastLog([lines])` → string
///
/// Returns the most recent `message`/`notice` string (or `""` if none);
/// with an argument, up to that many most-recent entries joined with `\n`.
/// The reference prefixes each entry with a timestamp and defaults to a
/// large line count; this port returns the plain strings per the
/// task-specified API.
extern "C" fn native_get_last_log(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let a = args(argv, argc);
    let lines = if argc >= 1 {
        value_as_i64(&a[0]).max(0) as usize
    } else {
        1
    };
    let buf = lock_ok(&LOG_BUFFER);
    let lines = lines.min(buf.len());
    if lines == 0 {
        set_string_out(out, "");
        return 0;
    }
    let entries: Vec<&str> = buf
        .iter()
        .rev()
        .take(lines)
        .rev()
        .map(String::as_str)
        .collect();
    set_string_out(out, &entries.join("\n"));
    0
}

/// `Debug.addLoggingHandler(fn)` → void
///
/// Reference: appends a callable to the logging-handler list; each
/// `message`/`notice` string is dispatched to every handler. The callback is
/// retained and called with the logged text.
extern "C" fn native_add_logging_handler(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.addLoggingHandler requires 1 argument");
    }
    let a = args(argv, argc);
    if a[0].ty != tjs2_sys::VAL_OBJECT {
        set_void_out(out);
        return 0;
    }
    let engine = context_engine();
    // `retain_value_detached(Object)` resolves the call's most recent object
    // argument while preserving its closure `ObjThis`, so removal by
    // `find_retained_id` (which compares Object+ObjThis) matches.
    match engine.retain_value_detached(&tjs2_sys::TjsValue::Object) {
        Ok(handler) => lock_ok(&LOGGING_HANDLERS).push(handler),
        Err(e) => log::debug!("Debug.addLoggingHandler: cannot retain handler: {e}"),
    }
    set_void_out(out);
    0
}

/// `Debug.removeLoggingHandler(fn)` → void
///
/// Reference: removes a callable from the logging-handler list. The argument
/// object is resolved against the current call's object slot and matched by
/// retained identity.
extern "C" fn native_remove_logging_handler(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.removeLoggingHandler requires 1 argument");
    }
    let engine = context_engine();
    let a = args(argv, argc);
    // `find_retained_id` resolves an object against the engine's most recent
    // object argument, which the trampoline set to this call's `fn` (only
    // objects are accepted, so a stale slot cannot match).
    if a[0].ty == tjs2_sys::VAL_OBJECT
        && let Some(id) = engine.find_retained_id(&tjs2_sys::TjsValue::Object)
    {
        lock_ok(&LOGGING_HANDLERS).retain(|handler| handler.raw_id() != id);
    }
    set_void_out(out);
    0
}

/// `Debug.logLocation` getter.
extern "C" fn prop_log_location_get(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_string_out(out, &lock_ok(&LOG_LOCATION));
    0
}

/// `Debug.logLocation` setter.
extern "C" fn prop_log_location_set(
    _e: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes a valid value slot.
    *lock_ok(&LOG_LOCATION) = value_as_string(unsafe { &*value });
    0
}

extern "C" fn prop_log_to_file_on_error_get(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(out, i64::from(LOG_TO_FILE_ON_ERROR.load(Ordering::SeqCst)));
    0
}

extern "C" fn prop_log_to_file_on_error_set(
    _e: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes a valid value slot.
    LOG_TO_FILE_ON_ERROR.store(value_as_bool(unsafe { &*value }), Ordering::SeqCst);
    0
}

extern "C" fn prop_clear_log_file_on_error_get(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(
        out,
        i64::from(CLEAR_LOG_FILE_ON_ERROR.load(Ordering::SeqCst)),
    );
    0
}

extern "C" fn prop_clear_log_file_on_error_set(
    _e: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes a valid value slot.
    CLEAR_LOG_FILE_ON_ERROR.store(value_as_bool(unsafe { &*value }), Ordering::SeqCst);
    0
}

/// `Debug.controller` getter (reference `DebugIntf.cpp:158`,
/// `TVPCreateNativeClass_Debug`): returns the singleton `Controller` class
/// object (the reference wraps `TVPGetControllerClass()` in a variant).
/// Evaluating the global class object preserves its identity across reads.
extern "C" fn prop_controller_get(
    _e: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    match set_object_result(out, "Controller", "Debug.controller") {
        Ok(()) => 0,
        Err(e) => report_error(out_error, &format!("Debug.controller: {e}")),
    }
}

/// `Debug.console` getter (reference `DebugIntf.cpp:170`): returns the
/// singleton `Console` class object.
extern "C" fn prop_console_get(
    _e: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    match set_object_result(out, "Console", "Debug.console") {
        Ok(()) => 0,
        Err(e) => report_error(out_error, &format!("Debug.console: {e}")),
    }
}

/// Register the `Debug` native class (methods + the logging properties).
pub fn register_debug(engine: &tjs2_sys::Tjs2Engine) -> Result<(), String> {
    lock_ok(&LOGGING_HANDLERS).clear();
    // `Debug.controller` / `Debug.console` return these class objects, so
    // they must exist first.
    super::controller_console::register_controller_console(engine)?;
    engine.register_native_class(&NativeClassBuilder {
        name: "Debug",
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
            // Reference DebugIntf.cpp:158,170: read-only class objects.
            NativePropertyDef {
                name: "controller",
                get: Some(prop_controller_get),
                set: None,
            },
            NativePropertyDef {
                name: "console",
                get: Some(prop_console_get),
                set: None,
            },
        ],
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
    })
}

#[cfg(test)]
mod tests {
    use crate::test_lock::vm_lock;

    use tjs2_sys::{Tjs2Engine, TjsValue};

    fn engine() -> &'static Tjs2Engine {
        // Leak the engine so its address is stable: `register_all` stores the
        // `Tjs2Engine` wrapper address in a process global used by the Debug
        // logging handlers.
        let e = Box::leak(Box::new(Tjs2Engine::new().expect("create engine")));
        crate::register_all(e).expect("register all natives");
        e
    }

    #[test]
    fn logging_handler_receives_messages_and_can_be_removed() {
        let _vm_lock = vm_lock();
        let e = engine();
        e.exec_script(
            "var got = []; function h(s) { got.push(s); } \
             Debug.addLoggingHandler(h); Debug.message('one'); Debug.notice('two');",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("got.count", "test").unwrap(), TjsValue::Integer(2));
        assert_eq!(
            e.eval("got[0]", "test").unwrap(),
            TjsValue::String("one".into())
        );
        assert_eq!(
            e.eval("got[1]", "test").unwrap(),
            TjsValue::String("two".into())
        );
        // removing by identity stops delivery
        e.exec_script(
            "Debug.removeLoggingHandler(h); Debug.message('three');",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("got.count", "test").unwrap(), TjsValue::Integer(2));
    }

    #[test]
    fn non_object_handler_argument_is_ignored() {
        let _vm_lock = vm_lock();
        let e = engine();
        e.exec_script(
            "Debug.addLoggingHandler(42); Debug.message('still works');",
            "test",
        )
        .unwrap();
        assert_eq!(
            e.eval("Debug.getLastLog()", "test").unwrap(),
            TjsValue::String("still works".into())
        );
    }

    #[test]
    fn debug_properties_round_trip() {
        let _vm_lock = vm_lock();
        let e = engine();
        assert_eq!(
            e.eval("Debug.logToFileOnError", "test").unwrap(),
            TjsValue::Integer(0)
        );
        assert_eq!(
            e.eval("Debug.clearLogFileOnError", "test").unwrap(),
            TjsValue::Integer(0)
        );
        e.exec_script(
            "Debug.logLocation = 'logs/'; Debug.logToFileOnError = true; \
             Debug.clearLogFileOnError = true;",
            "test",
        )
        .unwrap();
        assert_eq!(
            e.eval("Debug.logLocation", "test").unwrap(),
            TjsValue::String("logs/".into())
        );
        assert_eq!(
            e.eval("Debug.logToFileOnError", "test").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval("Debug.clearLogFileOnError", "test").unwrap(),
            TjsValue::Integer(1)
        );
        // restore defaults for other tests in this binary
        e.exec_script(
            "Debug.logToFileOnError = false; Debug.clearLogFileOnError = false;",
            "test",
        )
        .unwrap();
    }

    /// `Debug.controller` / `Debug.console` return the `Controller` /
    /// `Console` class objects (reference `DebugIntf.cpp:158,170`), whose
    /// `visible` property is a no-op getter/setter in a headless port.
    #[test]
    fn controller_and_console_class_objects() {
        let _vm_lock = vm_lock();
        let e = engine();
        assert_eq!(
            e.eval("Debug.controller.visible", "test").unwrap(),
            TjsValue::Integer(0)
        );
        assert_eq!(
            e.eval("Debug.console.visible", "test").unwrap(),
            TjsValue::Integer(0)
        );
        // The class object is stable across reads (the reference caches it
        // in a static holder).
        assert_eq!(
            e.eval("Debug.controller === Debug.controller", "test")
                .unwrap(),
            TjsValue::Integer(1)
        );
        // The `visible` setter is a no-op, not an error.
        e.exec_script(
            "Debug.controller.visible = true; Debug.console.visible = true;",
            "test",
        )
        .unwrap();
        assert_eq!(
            e.eval("Debug.controller.visible", "test").unwrap(),
            TjsValue::Integer(0)
        );
        // `controller` / `console` themselves are read-only.
        assert!(e.eval("Debug.controller = 1", "test").is_err());
        assert!(e.eval("Debug.console = 1", "test").is_err());
    }
}
