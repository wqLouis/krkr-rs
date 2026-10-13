//! `Debug` native class.
//!
//! Behavioral reference: `crates/tvp-natives/src/debug.rs` (itself a
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
//! `message`/`notice` write to the `log` crate (`message` at `info!`,
//! `notice` at `debug!`), push to an in-memory ring buffer (the reference's
//! `TVPLogMaxLines + 100` cap, `DebugIntf.cpp:43`, `376`), append to the file
//! opened by `startLogToFile` — when a `logLocation` is configured — and
//! dispatch the line to every retained `addLoggingHandler` callback. Like the
//! reference (`TVPAddLog`, `DebugIntf.cpp:331-393`), the file and the
//! handlers see `" " + line` while the ring buffer stores the raw line, and
//! `getLastLog` formats `Time + ' ' + Log + "\r\n"` per entry
//! (`DebugIntf.cpp:419-459`, `TJS_TEXT_OUT_CRLF` from
//! `core/CMakeLists.txt:39`). The handlers are matched by closure identity,
//! so `removeLoggingHandler` retires exactly the callback it names and a
//! duplicate registration is ignored.
#![allow(unsafe_code)]

use std::collections::VecDeque;
use std::ffi::{c_char, c_int, c_void};

use api::fs::{FsPath, OpenMode};

use tjs::{
    DetachedValue, NativeClassBuilder, NativeInstanceBuilder, NativeInstanceMethodDef,
    NativeInstancePropertyDef, NativeMethodDef, NativePropertyDef, RetainedValue, Tjs2Engine,
    TjsValue, VAL_NULL, VAL_OBJECT, VAL_RETAINED, Value,
};

use super::system::{get_command_line, resolve, set_string_out, try_value_as_i64, value_as_bool};
use crate::native_util::{args, report_error, set_bool, set_void, value_as_string};
use crate::natives::storages::normalize_storage_name;
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError, CompatServices};

/// The reference `TVPLogMaxLines` (`utils/DebugIntf.cpp:43`).
const TVP_LOG_MAX_LINES: usize = 2048;

/// Maximum number of entries kept in the `getLastLog` ring buffer — the
/// reference evicts once the deque reaches `TVPLogMaxLines + 100`
/// (`utils/DebugIntf.cpp:376`).
const LOG_BUFFER_CAP: usize = TVP_LOG_MAX_LINES + 100;

/// How many of the oldest entries are dropped at once once the buffer reaches
/// `LOG_BUFFER_CAP` (`DebugIntf.cpp:373-379`).
const LOG_EVICT_BATCH: usize = 100;

/// The lines `TVPStartLogToFile` rolls back into the file
/// (`TVPLogToFileRollBack`, `utils/DebugIntf.cpp:48`, `497`).
const LOG_TO_FILE_ROLLBACK: usize = 100;

/// The reference `TVPSeparatorCRLF` written when the log stream opens
/// (`base/MsgIntfInc.h:160-164`).
const LOG_OPEN_SEPARATOR: &str = "\r\n\r\n\r\n=========================================================\r\n=========================================================\r\n";

/// The reference dashed separator written by `TVPStartLogToFile`
/// (`utils/DebugIntf.cpp:489-496`).
const LOG_START_SEPARATOR: &str =
    "\r\n------------------------------------------------------------\r\n";

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
    log_file: Option<fs::FileState>,
    /// The reference `TVPLoggingToFile` flag — set even when no location is
    /// configured (the reference opens no stream then, `DebugIntf.cpp:241-244`).
    logging_to_file: bool,
    /// The reference `OpenFailed`: stop retrying an unopenable log file until
    /// the location changes (`DebugIntf.cpp:234-236`, `293-295`).
    log_open_failed: bool,
    /// Ring buffer of recent `(time, line)` entries as `TVPAddLog` stores them
    /// (`DebugIntf.cpp:356`).
    log_buffer: VecDeque<(String, String)>,
    /// The accumulated "important" log (`notice` → `TVPImportantLogs`,
    /// `DebugIntf.cpp:358-363`), flushed by `startLogToFile`.
    important_log: String,
    /// `Debug.logLocation` (reference `TVPLogLocation`, default empty).
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

/// Epoch seconds now, floored at the epoch if the clock predates 1970.
fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// `HH:MM:SS` of the current time. The fields are UTC because `std` has no
/// local-time API; the tests cross-check them against `gmtime_r`.
fn log_timestamp() -> String {
    let secs = now_epoch_secs() % 86_400;
    format!(
        "{:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    )
}

/// Weekday and month names as `asctime` spells them.
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Epoch days to `(year, month, day, weekday)`, weekday 0 = Sunday.
fn civil_from_days(days: i64) -> (i64, u32, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let day = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let month = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if month <= 2 { y + 1 } else { y };
    let weekday = (days + 4).rem_euclid(7); // 1970-01-01 was a Thursday
    (year, month as u32, day as u32, weekday as u32)
}

/// `asctime`-shaped `Www Mmm dd hh:mm:ss yyyy` for the log-file opener.
fn log_date_time() -> String {
    let secs = now_epoch_secs();
    let (year, month, day, weekday) = civil_from_days((secs / 86_400) as i64);
    let tod = secs % 86_400;
    format!(
        "{} {} {:>2} {:02}:{:02}:{:02} {}",
        WEEKDAYS[weekday as usize],
        MONTHS[(month - 1) as usize],
        day,
        tod / 3600,
        (tod % 3600) / 60,
        tod % 60,
        year
    )
}

/// Push `text` into the ring buffer with its reference timestamp, then drop
/// the oldest entries a hundred at a time while the buffer is at or over its
/// cap, so it holds between `TVP_LOG_MAX_LINES` and `LOG_BUFFER_CAP - 1`
/// entries (`tTVPLogDeque`, `DebugIntf.cpp:373-379`).
fn push_log(services: &CompatServices, text: &str) {
    let timestamp = log_timestamp();
    let mut state = services.state::<DebugState>();
    state.log_buffer.push_back((timestamp, text.to_string()));
    while state.log_buffer.len() >= LOG_BUFFER_CAP {
        state.log_buffer.drain(..LOG_EVICT_BATCH);
    }
}

/// Open (or reopen) the reference log stream: `{location}/krkr.console.log`
/// (`tTVPLogStreamHolder::Open`, `DebugIntf.cpp:240-296`) — an empty
/// `logLocation` means no stream at all (`DebugIntf.cpp:241-244`), and a
/// failed open is remembered so later lines do not retry
/// (`DebugIntf.cpp:234-236`). On success the reference writes its separator
/// and a `Logging to ... started on ...` header (`DebugIntf.cpp:267-290`);
/// the UTF-16 `ff fe` BOM is omitted because this port's stream is UTF-8.
fn open_log_file(state: &mut DebugState, truncate: bool) {
    if state.log_open_failed {
        return;
    }
    if state.log_location.is_empty() {
        state.log_file = None;
        state.log_open_failed = true;
        return;
    }
    let mut native = strip_file_media(&state.log_location).to_string();
    if !native.ends_with('/') {
        native.push('/');
    }
    let filename = format!("{native}/krkr.console.log");
    let file_system = fs::FileSystem::host("");
    let path = FsPath::host(&filename);
    let mode = if truncate {
        OpenMode::Write
    } else {
        OpenMode::Append
    };
    match fs::open(&file_system, &path, mode) {
        Ok(file) => {
            let header = format!(
                "{LOG_OPEN_SEPARATOR}Logging to {filename} started on {}\r\n",
                log_date_time()
            );
            let _ = fs::write_at(&file_system, &file, file.cursor(), header.as_bytes());
            let _ = fs::flush(&file_system, &file);
            state.log_file = Some(file);
        }
        Err(e) => {
            log::warn!("Debug: failed to open {filename:?}: {e}");
            state.log_file = None;
            state.log_open_failed = true;
        }
    }
}

/// Append `text` to the log file the way `TVPLogStreamHolder.Log` does
/// (`DebugIntf.cpp:312-340`): the stored line is prefixed with a space
/// (`TVPAddLog`'s `buf`, `DebugIntf.cpp:377-391` — the timestamp copy there is
/// commented out) and the line ends with `\r\n`. The stream opens lazily on
/// the first line while logging is enabled.
fn write_log_file(services: &CompatServices, text: &str) {
    let mut state = services.state::<DebugState>();
    if !state.logging_to_file {
        return;
    }
    if state.log_file.is_none() && !state.log_open_failed {
        open_log_file(&mut state, false);
    }
    if let Some(file) = state.log_file.as_mut() {
        let file_system = fs::FileSystem::host("");
        let mut bytes = Vec::with_capacity(text.len() + 4);
        bytes.push(b' ');
        bytes.extend_from_slice(text.as_bytes());
        bytes.extend_from_slice(b"\r\n");
        let _ = fs::append(&file_system, file, &bytes);
        let _ = fs::flush(&file_system, file);
    }
}

/// Dispatch `text` to every registered logging handler. The handler list is
/// moved out of the state while the callbacks run so a handler that itself
/// logs cannot borrow the state re-entrantly. Like the reference
/// (`_TVPDeliverLoggingEvent` fed by `TVPOnLog(buf)`,
/// `DebugIntf.cpp:122-147`, `386`), the handler receives `" " + line` as its
/// single argument.
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
        if let Err(e) =
            engine.call_detached(&handler.value, &[TjsValue::String(format!(" {text}"))])
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let text = join_args(unsafe { args(argv, argc) });
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Debug.message: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    push_log(services, &text);
    write_log_file(services, &text);
    dispatch_logging_handlers(&ctx, &text);
    log::info!("{text}");
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let text = join_args(unsafe { args(argv, argc) });
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Debug.notice: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    // `TVPAddImportantLog` also accumulates `Time ! line` into
    // `TVPImportantLogs` (`DebugIntf.cpp:358-363`), which `startLogToFile`
    // flushes into the file.
    {
        let mut state = services.state::<DebugState>();
        let timestamp = log_timestamp();
        state
            .important_log
            .push_str(&format!("{timestamp} ! {text}\r\n"));
    }
    push_log(services, &text);
    write_log_file(services, &text);
    dispatch_logging_handlers(&ctx, &text);
    log::debug!("{text}");
    set_void(out);
    0
}

/// The shared body of `TVPStartLogToFile(clear)` (`DebugIntf.cpp:471-506`):
/// ignored while already logging, optionally truncates the stream, then
/// flushes the accumulated important log, the dashed separator and the last
/// `TVPLogToFileRollBack` lines.
fn start_log_file(services: &CompatServices, clear: bool) {
    let mut state = services.state::<DebugState>();
    // `if(TVPLoggingToFile) return;` runs *before* the clear
    // (`DebugIntf.cpp:478-481`).
    if state.logging_to_file {
        return;
    }
    if clear {
        // `TVPLogStreamHolder.Clear()`: close and reopen truncated
        // (`DebugIntf.cpp:303-310`).
        state.log_file = None;
        state.log_open_failed = false;
        open_log_file(&mut state, true);
    }
    state.logging_to_file = true;
    if state.log_file.is_none() {
        open_log_file(&mut state, false);
    }
    let important = std::mem::take(&mut state.important_log);
    let rollback: Vec<(String, String)> = state
        .log_buffer
        .iter()
        .rev()
        .take(LOG_TO_FILE_ROLLBACK)
        .rev()
        .cloned()
        .collect();
    if let Some(file) = state.log_file.as_mut() {
        let mut block = String::new();
        if !important.is_empty() {
            block.push_str(&important);
            block.push_str("\r\n");
        }
        block.push_str(LOG_START_SEPARATOR);
        for (time, line) in &rollback {
            block.push_str(time);
            block.push(' ');
            block.push_str(line);
            block.push_str("\r\n");
        }
        block.push_str("\r\n");
        let file_system = fs::FileSystem::host("");
        let _ = fs::append(&file_system, file, block.as_bytes());
        let _ = fs::flush(&file_system, file);
    }
}

/// `Debug.startLogToFile([clear])` → void — reference `TVPStartLogToFile`
/// (`DebugIntf.cpp:471-506`): an **optional boolean**, no filename — the
/// file is `{logLocation}/krkr.console.log`.
extern "C" fn native_start_log_to_file(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // `if(numparams >= 1) clear = param[0]->operator bool();`
    // (`DebugIntf.cpp:625-628`).
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let clear = argc >= 1 && value_as_bool(&unsafe { args(argv, argc) }[0]);
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Debug.startLogToFile: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    start_log_file(unsafe { ctx.services() }, clear);
    set_void(out);
    0
}

/// `Debug.logAsError()` → void — reference `TVPOnError`
/// (`DebugIntf.cpp:508-513`): start the error log file when
/// `logToFileOnError` is set, with `clearLogFileOnError` as its clear flag.
/// The reference method takes no arguments, so any passed arguments are
/// ignored.
extern "C" fn native_log_as_error(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Debug.logAsError: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let (auto, clear) = {
        let state = services.state::<DebugState>();
        (state.log_to_file_on_error, state.clear_log_file_on_error)
    };
    if auto {
        start_log_file(services, clear);
    }
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
    // Reference `lines = TVPLogMaxLines + 100` by default, `(tjs_uint)`
    // truncation of `param[0]->AsInteger()` otherwise (`DebugIntf.cpp:1454-1457`):
    // reals truncate, strings parse, objects raise, and a negative value wraps
    // to a huge unsigned count (clamped to the buffer below).
    let lines = if argc >= 1 {
        // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
        match try_value_as_i64(&unsafe { args(argv, argc) }[0]) {
            Ok(value) => value as u32 as usize,
            Err(error) => return report_error(out_error, &error),
        }
    } else {
        TVP_LOG_MAX_LINES + 100
    };
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "Debug.getLastLog: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let text = {
        let state = services.state::<DebugState>();
        // Reference `TVPGetLastLog` (`DebugIntf.cpp:419-459`): every entry is
        // `Time + ' ' + Log + "\r\n"` (`TJS_TEXT_OUT_CRLF`,
        // `core/CMakeLists.txt:39`) — including a trailing separator after the
        // last entry; `n == 0` yields the empty string.
        let mut text = String::new();
        for (time, line) in state
            .log_buffer
            .iter()
            .rev()
            .take(lines.min(state.log_buffer.len()))
            .rev()
        {
            text.push_str(time);
            text.push(' ');
            text.push_str(line);
            text.push_str("\r\n");
        }
        text
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // Reference `param[0]->AsObjectClosureNoAddRef()`
    // (`DebugIntf.cpp:645-648`): TJS `null` is an empty closure that is
    // stored but never invoked (a silent success); any other non-object
    // raises the conversion error.
    if a[0].ty == VAL_NULL {
        set_void(out);
        return 0;
    }
    if a[0].ty != VAL_OBJECT {
        return report_error(
            out_error,
            "Debug.addLoggingHandler: argument is not an object",
        );
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // Same conversion contract as `addLoggingHandler` (`DebugIntf.cpp:660-663`).
    if a[0].ty == VAL_NULL {
        set_void(out);
        return 0;
    }
    if a[0].ty != VAL_OBJECT {
        return report_error(
            out_error,
            "Debug.removeLoggingHandler: argument is not an object",
        );
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
    // Reference `TVPSetLogLocation` (`DebugIntf.cpp:518-543`): normalize the
    // name, then `TVPGetLocallyAccessibleName`; an in-archive name has no
    // local form and clears the location (`StorageIntf.cpp:554-560`), then
    // `TVPLogStreamHolder.Reopen()`, then the `-forcelog` option.
    let location = normalize_storage_name(&value_as_string(unsafe { &*value }));
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let location = if location.contains('>') {
        String::new()
    } else {
        location
    };
    {
        let mut state = services.state::<DebugState>();
        state.log_location = location;
        // `Reopen()` (`DebugIntf.cpp:230-232`): close the stream; a failed
        // open may be retried at the new location.
        state.log_file = None;
        state.log_open_failed = false;
    }
    // `-forcelog=yes` starts logging, `-forcelog=clear` truncates first
    // (`DebugIntf.cpp:534-543`).
    if let Some(value) = get_command_line(services, "-forcelog") {
        let clear = match value.as_str() {
            "yes" => Some(false),
            "clear" => Some(true),
            _ => None,
        };
        if let Some(clear) = clear {
            services.state::<DebugState>().logging_to_file = false;
            start_log_file(services, clear);
        }
    }
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
