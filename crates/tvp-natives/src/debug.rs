//! `Debug` native class — ported from
//! `reference/cpp/core/utils/DebugIntf.cpp` (`tTJSNC_Debug`).
//!
//! Implemented methods: `message`, `notice`, `startLogToFile`, `logAsError`,
//! `getLastLog`. `addLoggingHandler` and `removeLoggingHandler` are void
//! stubs (the logging-handler list is not implemented). The properties
//! (`logLocation`, `logToFileOnError`, `clearLogFileOnError`) are pending on
//! the FFI property extension.
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

use tjs2_sys::{NativeClassBuilder, NativeMethodDef, Value};

use super::{
    args, lock_ok, report_error, set_string_out, set_void_out, value_as_bool, value_as_i64,
    value_as_string,
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
/// Reference: appends a callable to the logging-handler list (each log line
/// is dispatched to the handlers). Stub: validates the argument count and
/// does nothing — the handler list is not implemented.
extern "C" fn native_add_logging_handler(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.addLoggingHandler requires 1 argument");
    }
    set_void_out(out);
    0
}

/// `Debug.removeLoggingHandler(fn)` → void
///
/// Reference: removes a callable from the logging-handler list. Stub: same
/// as [`native_add_logging_handler`].
extern "C" fn native_remove_logging_handler(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.removeLoggingHandler requires 1 argument");
    }
    set_void_out(out);
    0
}

/// Register the `Debug` native class (methods only; properties are pending
/// on the FFI property extension — see the module docs).
pub fn register_debug(engine: &tjs2_sys::Tjs2Engine) -> Result<(), String> {
    engine.register_native_class(&NativeClassBuilder {
        name: "Debug",
        // property registration is pending on the FFI property extension
        // (tjs2_register_native_class_ex) landing in parallel
        properties: Vec::new(),
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
