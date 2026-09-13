//! `System` native class — ported from
//! `reference/cpp/core/base/impl/SystemImpl.cpp`
//! (`TVPCreateNativeClass_System`).
//!
//! Implemented methods:
//!
//! * `inform` — logs at `info!` (the reference shows a modal message box).
//! * `getTickCount` — milliseconds since an arbitrary epoch (first call).
//! * `getKeyState` — stub, always false (input natives are a later wave).
//! * `shellExecute` — launches with the platform opener (`xdg-open` /
//!   `open` / `cmd start`).
//! * `system` — stub returning 0 (the reference's `_wsystem` is commented
//!   out).
//! * `readRegValue` — stub returning void (registry not implemented).
//! * `getArgument` / `setArgument` — process-global command-line arguments
//!   (see below).
//! * `createAppLock` — stub returning false (app lock not implemented).
//! * `dumpHeap` — void no-op, as in the reference (`TVPHeapDump` commented
//!   out).
//! * `nullpo` — the reference deliberately crashes (`*(int*)0 = 0` /
//!   `__builtin_trap`); this port logs an error and returns void instead of
//!   aborting.
//! * `showVersion` — logs the version at `info!` (the reference's version
//!   dialog is commented out).
//!
//! # `setArgument` / `getArgument` semantics
//!
//! Extracted from `TVPSetCommandLine` / `TVPGetCommandLine`
//! (`reference/cpp/core/base/impl/SysInitImpl.cpp`):
//!
//! * Names are **not normalized**: the leading dash is part of the name,
//!   passed verbatim by the caller. `System.setArgument("-debugwin", ...)`
//!   stores the key `"-debugwin"`; there is no implicit dash insertion or
//!   stripping (the reference only adds a dash when parsing *config-file*
//!   options at startup, not in `setArgument`/`getArgument`).
//! * Arguments are `name=value` pairs. `setArgument(name, value)` stores
//!   `name` → `value`, replacing any previous value (the reference replaces
//!   the matching entry) or inserting a new one.
//! * `getArgument(name [, default])` returns the stored value. When the
//!   name is missing it returns `default` if a second argument was given
//!   (preserving its type), otherwise void — the reference's
//!   `result->Clear()`. In the reference an argument stored without `=`
//!   (possible only via startup config parsing) reads back as the string
//!   `"yes"`; this port always stores explicit values, so that case never
//!   arises from `setArgument`.
//!
//! Deviation: the reference keeps an **ordered vector** of `name=value`
//! strings and matches names by **prefix** (`getArgument("-f")` matches an
//! entry `-foo=...`; `setArgument` rewrites the matched entry to the shorter
//! name). This port stores an exact-match `HashMap`, so names must match
//! exactly — which is how real scripts always call it.
//!
//! # Pending (FFI property extension, landing in parallel)
//!
//! The property getters/setters are not in this wave:
//! `exePath`, `platform`, `osName`, `personalPath`, `appDataPath`,
//! `dataPath`, `exeName`, `savedGamesPath`, `title`, `screenWidth`,
//! `screenHeight`, `desktopLeft`, `desktopTop`, `desktopWidth`,
//! `desktopHeight`, `touchDevice`.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_void};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Instant;

use tjs2_sys::{
    NativeClassBuilder, NativeMethodDef, VAL_INTEGER, VAL_REAL, VAL_STRING, VAL_VOID, Value,
};

use super::{
    args, lock_ok, report_error, set_int_out, set_object_result, set_real_out, set_string_out,
    set_void_out, value_as_bool, value_as_i64, value_as_string,
};

/// Process-global command-line arguments (`name` → `value`), mirroring the
/// reference's process-wide `TVPProgramArguments` stock. (`LazyLock` because
/// `HashMap::new` is not const.)
static ARGS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Keyboard state supplied by the host input bridge, keyed by Windows VK.
static KEY_STATES: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

/// Update one host key state before dispatching the corresponding script
/// input event. The render crate calls this from its Bevy input bridge.
pub fn set_key_state(key: u32, down: bool) {
    let mut keys = lock_ok(&KEY_STATES);
    if down {
        keys.insert(i64::from(key));
    } else {
        keys.remove(&i64::from(key));
    }
}

/// Epoch for [`native_get_tick_count`]: the first call (reference:
/// `TVPStartTickCount`).
static TICK_EPOCH: OnceLock<Instant> = OnceLock::new();

/// `System.inform(text [, caption [, buttons]])`
///
/// Reference: `TVPShowSimpleMessageBox` — a modal dialog; with a buttons
/// argument the pressed button index is returned. This port logs the text
/// (with the default caption `"Information"` when omitted) at `info!` and
/// returns void — there is no GUI yet. Button handling is not implemented.
extern "C" fn native_inform(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.inform requires 1 argument");
    }
    let a = args(argv, argc);
    let text = value_as_string(&a[0]);
    let caption = if argc >= 2 && a[1].ty != VAL_VOID {
        value_as_string(&a[1])
    } else {
        "Information".to_string()
    };
    log::info!("System.inform [{caption}]: {text}");
    set_void_out(out);
    0
}

/// Milliseconds since the first call (the reference's
/// `TVPGetTickCount`/`TVPStartTickCount`); shared by `System.getTickCount`
/// and the continuous-handler delivery so handlers see the same clock as
/// the scripts.
fn tick_count_ms() -> i64 {
    let epoch = TICK_EPOCH.get_or_init(Instant::now);
    epoch.elapsed().as_millis() as i64
}

/// `System.getTickCount()` → milliseconds since an arbitrary epoch (the
/// first call), like the reference's `TVPGetTickCount`/`TVPStartTickCount`.
extern "C" fn native_get_tick_count(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int_out(out, tick_count_ms());
    0
}

/// `System.getKeyState(key [, getcurrent])` → bool
///
/// Reference: `TVPGetAsyncKeyState` (real keyboard polling). The host input
/// bridge mirrors its current Windows-VK state into this native. The argument count check
/// matches the reference (`TJS_E_BADPARAMCOUNT` on zero arguments).
extern "C" fn native_get_key_state(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.getKeyState requires 1 argument");
    }
    let key = value_as_i64(&args(argv, argc)[0]);
    let down = lock_ok(&KEY_STATES).contains(&key);
    set_int_out(out, i64::from(down));
    0
}

/// `System.shellExecute(target [, execparam])` → bool
///
/// Launches `target` with the platform's default opener: `xdg-open` on
/// Linux, `open` on macOS, `cmd /C start` on Windows. Returns true when the
/// process was spawned (like the reference, which does not wait for the
/// launched application); false on spawn failure. `execparam` is accepted
/// but ignored for now.
extern "C" fn native_shell_execute(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.shellExecute requires 1 argument");
    }
    let a = args(argv, argc);
    let target = value_as_string(&a[0]);
    let spawned = if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", "start", "", target.as_str()])
            .spawn()
            .is_ok()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(target.as_str()).spawn().is_ok()
    } else {
        Command::new("xdg-open")
            .arg(target.as_str())
            .spawn()
            .is_ok()
    };
    if !spawned {
        log::warn!("System.shellExecute: failed to launch {target:?}");
    }
    set_int_out(out, i64::from(spawned));
    0
}

/// `System.system(command)` → int
///
/// Reference: the `_wsystem` call is commented out; the method returns 0
/// after delivering a compact event. Stub: returns 0 and executes nothing.
extern "C" fn native_system(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.system requires 1 argument");
    }
    set_int_out(out, 0);
    0
}

/// `System.readRegValue(key)` → void
///
/// Reference: `TVPReadRegValue` fills the result from the Windows registry.
/// Stub: returns void — registry access is not implemented. The argument
/// count check matches the reference.
extern "C" fn native_read_reg_value(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.readRegValue requires 1 argument");
    }
    set_void_out(out);
    0
}

/// `System.getArgument(name [, default])`
///
/// See the module docs for the exact semantics. Requires at least one
/// argument; returns the stored value, the caller's default when the name is
/// missing, or void when no default was given.
extern "C" fn native_get_argument(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.getArgument requires 1 argument");
    }
    let a = args(argv, argc);
    let name = value_as_string(&a[0]);
    match lock_ok(&ARGS).get(&name).cloned() {
        Some(value) => {
            set_string_out(out, &value);
            0
        }
        None if argc >= 2 => {
            // default argument: preserve its type, like the reference's
            // `*result = *param[1]` (objects cannot cross the ABI).
            match a[1].ty {
                VAL_STRING => set_string_out(out, &value_as_string(&a[1])),
                VAL_INTEGER => set_int_out(out, a[1].integer),
                VAL_REAL => set_real_out(out, a[1].real),
                VAL_VOID => set_void_out(out),
                _ => {
                    return report_error(
                        out_error,
                        "System.getArgument: object default value is not supported",
                    );
                }
            }
            0
        }
        None => {
            // reference: `result->Clear()` when the argument is absent
            set_void_out(out);
            0
        }
    }
}

/// `System.setArgument(name, value)`
///
/// See the module docs for the exact semantics. Requires at least two
/// arguments; stores `name` → `value`, replacing any previous value.
extern "C" fn native_set_argument(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "System.setArgument requires 2 arguments");
    }
    let a = args(argv, argc);
    let name = value_as_string(&a[0]);
    let value = value_as_string(&a[1]);
    lock_ok(&ARGS).insert(name, value);
    set_void_out(out);
    0
}

/// `System.createAppLock(lockname)` → bool
///
/// Reference: `TVPCreateAppLock` creates a named lock to detect a second
/// instance. This emulator runs a single process per game, so the first
/// instance always "wins": return true. The argument count check matches
/// the reference.
extern "C" fn native_create_app_lock(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.createAppLock requires 1 argument");
    }
    set_int_out(out, 1);
    0
}

/// `System.terminate()` — request termination (used by exception handlers).
extern "C" fn native_terminate(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let code = if argc >= 1 {
        value_as_i64(&args(argv, argc)[0])
    } else {
        0
    };
    request_exit(code as i32);
    log::info!("System.terminate({code}) — emulator termination requested");
    set_void_out(out);
    0
}

/// `System.exit(code)` — terminate the emulator; does not return in the
/// reference (`TVPTerminateSync`). Records the exit code so the loader can
/// stop afterwards; scripts may observe a short continuation (the reference
/// unwinds via a thrown exception — approximated here).
extern "C" fn native_exit(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let code = if argc >= 1 {
        value_as_i64(&args(argv, argc)[0])
    } else {
        0
    };
    request_exit(code as i32);
    log::info!("System.exit({code}) — emulator termination requested");
    set_void_out(out);
    0
}

/// `System.dumpHeap()` → void
///
/// Reference: a no-op (`TVPHeapDump()` is commented out). Returns void.
extern "C" fn native_dump_heap(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_void_out(out);
    0
}

/// `System.nullpo()` → void
///
/// Reference: deliberately crashes the process (`*(int*)0 = 0` on MSVC,
/// `__builtin_trap` elsewhere). krkr-rs must not abort: logs an error and
/// returns void instead.
extern "C" fn native_nullpo(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    log::error!(
        "System.nullpo() called: the reference implementation intentionally crashes \
         the process; krkr-rs logs instead and continues"
    );
    set_void_out(out);
    0
}

/// `System.showVersion()` → void
///
/// Reference: `TVPShowVersionForm` (a version dialog) is commented out.
/// Logs the version at `info!`.
extern "C" fn native_show_version(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    log::info!("KiriKiri (krkr-rs) {}", env!("CARGO_PKG_VERSION"));
    set_void_out(out);
    0
}

/// `System.doCompact(...)` — stubbed no-op (compaction is a GC hint).
extern "C" fn system_do_compact(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    crate::set_void_out(out);
    0
}

/// Continuous-handler callbacks (`System.addContinuousHandler`), invoked
/// once per frame by [`continuous_handler_poll`]. A handler returning
/// `false`/0 removes itself (the reference `TVPDeliverContinuousEvents`
/// semantics).
static CONTINUOUS_HANDLERS: Mutex<Vec<tjs2_sys::DetachedValue>> = Mutex::new(Vec::new());
/// Set by `removeContinuousHandler` while a callback is executing. The poll
/// loop takes its handler list out of the mutex, so a direct vector removal
/// cannot see the currently-running entry; this flag closes that race.
static REMOVE_CURRENT_HANDLER: AtomicBool = AtomicBool::new(false);
static CONTINUOUS_IN_CALLBACK: AtomicBool = AtomicBool::new(false);
static READD_CURRENT_HANDLER: AtomicBool = AtomicBool::new(false);

/// Advance every continuous handler once, passing the current tick count
/// (ms) as the handler's single argument — the reference
/// (`TVPDeliverContinuousEvent` in EventIntf.cpp) calls each handler with
/// `tick = TVPGetTickCount()`. A handler that raises a TJS error is
/// removed (also matching the reference); the return value is ignored.
/// Returns whether any remain.
pub fn continuous_handler_poll(engine: &tjs2_sys::Tjs2Engine) -> bool {
    let tick = tick_count_ms();
    let mut handlers = CONTINUOUS_HANDLERS
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    let taken = std::mem::take(&mut *handlers);
    let mut rest = Vec::with_capacity(taken.len());
    for h in taken {
        REMOVE_CURRENT_HANDLER.store(false, Ordering::SeqCst);
        READD_CURRENT_HANDLER.store(false, Ordering::SeqCst);
        CONTINUOUS_IN_CALLBACK.store(true, Ordering::SeqCst);
        let callback_ok = engine
            .call_detached(&h, &[tjs2_sys::TjsValue::Integer(tick)])
            .is_ok();
        CONTINUOUS_IN_CALLBACK.store(false, Ordering::SeqCst);
        let readd = READD_CURRENT_HANDLER.swap(false, Ordering::SeqCst);
        let keep = callback_ok && (readd || !REMOVE_CURRENT_HANDLER.swap(false, Ordering::SeqCst));
        if keep {
            rest.push(h);
        }
    }
    *handlers = rest;
    !handlers.is_empty()
}

/// `System.addContinuousHandler(fn)` — register a per-frame callback.
extern "C" fn native_add_continuous_handler(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let args = crate::args(argv, argc);
    if args.is_empty() {
        return crate::report_error(out_error, "System.addContinuousHandler requires 1 argument");
    }
    if CONTINUOUS_IN_CALLBACK.load(Ordering::SeqCst) {
        READD_CURRENT_HANDLER.store(true, Ordering::SeqCst);
        crate::set_void_out(out);
        return 0;
    }
    let engine = crate::context_engine();
    match engine.retain_value_detached(&tjs2_sys::TjsValue::Object) {
        Ok(dv) => {
            CONTINUOUS_HANDLERS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .push(dv);
            crate::set_void_out(out);
            0
        }
        Err(_) => {
            crate::set_void_out(out);
            0
        }
    }
}

/// `System.removeContinuousHandler(fn)` — unregister a per-frame callback.
extern "C" fn native_remove_continuous_handler(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let args = crate::args(argv, argc);
    if args.is_empty() {
        return crate::report_error(
            out_error,
            "System.removeContinuousHandler requires 1 argument",
        );
    }
    // The callback may be the handler currently being delivered. Mark it
    // for removal without performing a retained-object lookup while the VM
    // is re-entrant; the poll loop consumes the flag after the callback
    // returns. If the target is not current, retaining it across frames is
    // still harmless and it can be removed by the host lifecycle.
    REMOVE_CURRENT_HANDLER.store(true, Ordering::SeqCst);
    crate::set_void_out(out);
    0
}

/// Build the monitor dictionary shape used by k2compat's deskinfo script.
/// The reference exposes `monitor` and `work` rectangles and a `primary`
/// flag; the emulator has one primary monitor, so both rectangles use the
/// configured desktop bounds.
fn primary_monitor_expression() -> String {
    let ctx = system_context();
    let (x, y) = ctx.desktop_origin;
    let (w, h) = effective_desktop_size(&ctx);
    format!(
        "(function(){{var m=%[]; m.primary=1; m.monitor=%[x:{x},y:{y},w:{w},h:{h}]; m.work=%[x:{x},y:{y},w:{w},h:{h}]; return m;}})()"
    )
}

/// `System.getDisplayMonitors()` — return the single primary monitor as a
/// real TJS array. This is the minimal windowEx-compatible surface needed by
/// k2compat's desktop-info fallback.
extern "C" fn native_get_display_monitors(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc != 0 {
        return report_error(out_error, "System.getDisplayMonitors takes no arguments");
    }
    let monitor = primary_monitor_expression();
    let expression = format!("(function(){{var a=[]; a[0]={monitor}; return a;}})()");
    match set_object_result(out, &expression, "System.getDisplayMonitors") {
        Ok(()) => 0,
        Err(e) => report_error(out_error, &format!("System.getDisplayMonitors: {e}")),
    }
}

/// `System.getMonitorInfo(primary, window)` — return the primary monitor.
/// Window association is intentionally ignored because this host currently
/// has one logical game window.
extern "C" fn native_get_monitor_info(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(
            out_error,
            "System.getMonitorInfo requires at least 1 argument",
        );
    }
    match set_object_result(out, &primary_monitor_expression(), "System.getMonitorInfo") {
        Ok(()) => 0,
        Err(e) => report_error(out_error, &format!("System.getMonitorInfo: {e}")),
    }
}

/// Register the `System` native class (methods + the property getters games
/// rely on: `exePath`, `dataPath`, `personalPath`, `savedGamesPath`, ...).
pub fn register_system(engine: &tjs2_sys::Tjs2Engine) -> Result<(), String> {
    engine.register_native_class(&NativeClassBuilder {
        name: "System",
        properties: system_properties(),
        methods: vec![
            NativeMethodDef {
                name: "inform",
                f: native_inform,
            },
            NativeMethodDef {
                name: "getTickCount",
                f: native_get_tick_count,
            },
            NativeMethodDef {
                name: "getKeyState",
                f: native_get_key_state,
            },
            NativeMethodDef {
                name: "shellExecute",
                f: native_shell_execute,
            },
            NativeMethodDef {
                name: "system",
                f: native_system,
            },
            NativeMethodDef {
                name: "readRegValue",
                f: native_read_reg_value,
            },
            NativeMethodDef {
                name: "getArgument",
                f: native_get_argument,
            },
            NativeMethodDef {
                name: "setArgument",
                f: native_set_argument,
            },
            NativeMethodDef {
                name: "createAppLock",
                f: native_create_app_lock,
            },
            NativeMethodDef {
                name: "exit",
                f: native_exit,
            },
            NativeMethodDef {
                name: "terminate",
                f: native_terminate,
            },
            NativeMethodDef {
                name: "dumpHeap",
                f: native_dump_heap,
            },
            NativeMethodDef {
                name: "nullpo",
                f: native_nullpo,
            },
            NativeMethodDef {
                name: "showVersion",
                f: native_show_version,
            },
            NativeMethodDef {
                name: "doCompact",
                f: system_do_compact,
            },
            NativeMethodDef {
                name: "addContinuousHandler",
                f: native_add_continuous_handler,
            },
            NativeMethodDef {
                name: "removeContinuousHandler",
                f: native_remove_continuous_handler,
            },
            NativeMethodDef {
                name: "getDisplayMonitors",
                f: native_get_display_monitors,
            },
            NativeMethodDef {
                name: "getMonitorInfo",
                f: native_get_monitor_info,
            },
        ],
    })
}

// ---------------------------------------------------------------------------
// System properties
// ---------------------------------------------------------------------------

/// Environment the emulator provides to games via `System.*` properties.
#[derive(Clone, Debug)]
pub struct SystemContext {
    /// Game/project directory — mapped to `exePath`/`dataPath` (Kirikiroid2
    /// mounts the game folder as the project dir).
    pub project_dir: std::path::PathBuf,
    /// Platform data directory for app-wide files — `appDataPath`.
    pub app_data_dir: std::path::PathBuf,
    /// (width, height) of the virtual screen; (0, 0) = unknown/headless.
    pub screen_size: (u32, u32),
    /// Primary desktop origin in physical pixels.
    pub desktop_origin: (i32, i32),
    /// Primary desktop/work-area size in physical pixels.
    pub desktop_size: (u32, u32),
    /// Whether the platform is touch-capable (`touchDevice`).
    pub touch_device: bool,
}

impl Default for SystemContext {
    fn default() -> Self {
        let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        SystemContext {
            project_dir: cwd.clone(),
            app_data_dir: cwd,
            screen_size: (0, 0),
            desktop_origin: (0, 0),
            desktop_size: (0, 0),
            touch_device: false,
        }
    }
}

static SYSTEM_CONTEXT: LazyLock<Mutex<SystemContext>> =
    LazyLock::new(|| Mutex::new(SystemContext::default()));

/// Window title (writable via `System.title`).
static TITLE: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));

/// `System.eventDisabled` flag (writable; event processing not implemented).
static EVENT_DISABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Exit code recorded by `System.exit` (0 = not requested). Public mirror of
/// the most recent request; the *pending* request is `EXIT_REQUEST`.
pub static TERMINATE_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Pending exit request (`System.exit` / `System.terminate`), consumed
/// exactly once by the host. `None` until `request_exit` records a code;
/// `take_exit_request` clears it so a request is reported once.
static EXIT_REQUEST: Mutex<Option<i32>> = Mutex::new(None);

/// Record an exit request: mirror the code in the public `TERMINATE_CODE` and
/// latch it for `take_exit_request`. Called by both `System.exit` and
/// `System.terminate`.
fn request_exit(code: i32) {
    TERMINATE_CODE.store(code, Ordering::SeqCst);
    *lock_ok(&EXIT_REQUEST) = Some(code);
}

/// Atomically consume the pending exit request.
///
/// Returns the code recorded by the most recent `System.exit` /
/// `System.terminate` call and clears it, so a request is reported **exactly
/// once**: the first call after a request yields `Some(code)`, subsequent
/// calls yield `None` until a new request arrives. The game runner
/// (`render::game_app`) polls this each frame and maps it to Bevy's
/// `AppExit` (`Success` for code 0, `Error` otherwise).
#[must_use]
pub fn take_exit_request() -> Option<i32> {
    lock_ok(&EXIT_REQUEST).take()
}

/// Set the context the `System` property getters read. The app calls this
/// after mounting the game and before running `startup.tjs`.
pub fn set_system_context(ctx: SystemContext) {
    *lock_ok(&SYSTEM_CONTEXT) = ctx;
}

/// Current context (defaults when never set).
fn system_context() -> SystemContext {
    lock_ok(&SYSTEM_CONTEXT).clone()
}

fn effective_desktop_size(ctx: &SystemContext) -> (u32, u32) {
    if ctx.desktop_size != (0, 0) {
        ctx.desktop_size
    } else {
        ctx.screen_size
    }
}

/// The `savedata` folder inside the project dir (created lazily by the
/// reference on first use; we just compute the path).
fn saved_games_path() -> std::path::PathBuf {
    system_context().project_dir.join("savedata")
}

fn system_properties() -> Vec<tjs2_sys::NativePropertyDef> {
    use tjs2_sys::NativePropertyDef;
    // All System properties are get-only in the reference (setter denied).
    let getter =
        |name: &'static str,
         f: extern "C" fn(*mut c_void, *mut Value, *mut *mut c_char) -> c_int| {
            NativePropertyDef {
                name,
                get: Some(f),
                set: None,
            }
        };
    vec![
        getter("exePath", prop_exe_path),
        getter("dataPath", prop_data_path),
        getter("personalPath", prop_personal_path),
        getter("savedGamesPath", prop_saved_games_path),
        getter("appDataPath", prop_app_data_path),
        getter("exeName", prop_exe_name),
        NativePropertyDef {
            name: "title",
            get: Some(prop_title_get),
            set: Some(prop_title_set),
        },
        NativePropertyDef {
            name: "eventDisabled",
            get: Some(prop_event_disabled_get),
            set: Some(prop_event_disabled_set),
        },
        getter("screenWidth", prop_screen_width),
        getter("screenHeight", prop_screen_height),
        getter("desktopLeft", prop_desktop_left),
        getter("desktopTop", prop_desktop_top),
        getter("desktopWidth", prop_desktop_width),
        getter("desktopHeight", prop_desktop_height),
        getter("touchDevice", prop_touch_device),
    ]
}

fn dir_with_separator(path: &std::path::Path) -> String {
    // The game concatenates e.g. `System.exePath + "data.xp3"` and expects
    // path properties to end with a separator (the reference's
    // TVPNativeProjectDir ends with '/'); without it the join silently
    // merges (".../test" + "data.xp3" → ".../testdata.xp3").
    let s = path.display().to_string();
    if s.ends_with('/') { s } else { format!("{s}/") }
}

extern "C" fn prop_exe_path(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    set_string_out(out, &dir_with_separator(&system_context().project_dir));
    0
}

extern "C" fn prop_data_path(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    // This emulator mounts the game folder as the data dir (Kirikiroid2
    // behavior): System.dataPath == the game directory (trailing separator:
    // the game concatenates `DATA_PATH + "system.dat"`).
    set_string_out(out, &dir_with_separator(&system_context().project_dir));
    0
}

extern "C" fn prop_personal_path(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_string_out(out, &saved_games_path().display().to_string());
    0
}

extern "C" fn prop_saved_games_path(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_string_out(out, &saved_games_path().display().to_string());
    0
}

extern "C" fn prop_app_data_path(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_string_out(out, &system_context().app_data_dir.display().to_string());
    0
}

extern "C" fn prop_exe_name(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    set_string_out(out, "krkr-rs");
    0
}

extern "C" fn prop_title_get(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    let title = lock_ok(&TITLE).clone();
    let title = if title.is_empty() {
        "krkr-rs".to_string()
    } else {
        title
    };
    set_string_out(out, &title);
    0
}

extern "C" fn prop_title_set(
    _e: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes a valid value slot.
    let v = unsafe { &*value };
    *lock_ok(&TITLE) = value_as_string(v);
    log::debug!("System.title = {}", lock_ok(&TITLE));
    0
}

extern "C" fn prop_event_disabled_get(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(
        out,
        EVENT_DISABLED.load(std::sync::atomic::Ordering::SeqCst) as i64,
    );
    0
}

extern "C" fn prop_event_disabled_set(
    _e: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes a valid value slot.
    let v = unsafe { &*value };
    EVENT_DISABLED.store(value_as_bool(v), std::sync::atomic::Ordering::SeqCst);
    0
}

extern "C" fn prop_screen_width(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    set_int_out(out, system_context().screen_size.0 as i64);
    0
}

extern "C" fn prop_screen_height(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(out, system_context().screen_size.1 as i64);
    0
}

extern "C" fn prop_desktop_left(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    set_int_out(out, i64::from(system_context().desktop_origin.0));
    0
}

extern "C" fn prop_desktop_top(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    set_int_out(out, i64::from(system_context().desktop_origin.1));
    0
}

extern "C" fn prop_desktop_width(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(out, effective_desktop_size(&system_context()).0 as i64);
    0
}

extern "C" fn prop_desktop_height(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(out, effective_desktop_size(&system_context()).1 as i64);
    0
}

extern "C" fn prop_touch_device(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    set_int_out(out, system_context().touch_device as i64);
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exit_request_is_reported_exactly_once() {
        // Drain any request left behind by another test in this binary so
        // the assertions below start from a clean slate.
        let _ = take_exit_request();
        assert_eq!(take_exit_request(), None, "nothing pending after drain");

        // A recorded request is returned once, then cleared.
        request_exit(7);
        assert_eq!(take_exit_request(), Some(7));
        assert_eq!(take_exit_request(), None, "request must be consumed once");

        // Zero is a valid code (success) and is still consumed.
        request_exit(0);
        assert_eq!(take_exit_request(), Some(0));
        assert_eq!(take_exit_request(), None);

        // A later request replaces the previous state and is reported once.
        request_exit(3);
        request_exit(9);
        assert_eq!(take_exit_request(), Some(9));
        assert_eq!(take_exit_request(), None);

        // The public mirror always reflects the most recent request code.
        assert_eq!(TERMINATE_CODE.load(Ordering::SeqCst), 9);
    }
}
