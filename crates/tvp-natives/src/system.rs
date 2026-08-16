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

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};
use std::process::Command;
use std::sync::{LazyLock, Mutex, OnceLock};
use std::time::Instant;

use tjs2_sys::{
    NativeClassBuilder, NativeMethodDef, VAL_INTEGER, VAL_REAL, VAL_STRING, VAL_VOID, Value,
};

use super::{
    args, lock_ok, report_error, set_int_out, set_real_out, set_string_out, set_void_out,
    value_as_bool, value_as_i64, value_as_string,
};

/// Process-global command-line arguments (`name` → `value`), mirroring the
/// reference's process-wide `TVPProgramArguments` stock. (`LazyLock` because
/// `HashMap::new` is not const.)
static ARGS: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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
/// Reference: `TVPGetAsyncKeyState` (real keyboard polling). Stub: always
/// returns false — input natives are a later wave. The argument count check
/// matches the reference (`TJS_E_BADPARAMCOUNT` on zero arguments).
extern "C" fn native_get_key_state(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.getKeyState requires 1 argument");
    }
    set_int_out(out, 0);
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
    TERMINATE_CODE.store(code as i32, std::sync::atomic::Ordering::SeqCst);
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
    TERMINATE_CODE.store(code as i32, std::sync::atomic::Ordering::SeqCst);
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
        let keep = engine
            .call_detached(&h, &[tjs2_sys::TjsValue::Integer(tick)])
            .is_ok();
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
    let engine = crate::context_engine();
    // Match by script-value identity, not raw id: every `retain_value`
    // call allocates a fresh id, so the same function registered by
    // addContinuousHandler and passed to removeContinuousHandler would
    // never compare equal by id. find_retained_id finds the existing map
    // entry for the same closure without retaining anything new.
    if let Some(id) = engine.find_retained_id(&tjs2_sys::TjsValue::Object) {
        let mut handlers = CONTINUOUS_HANDLERS
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        handlers.retain(|h| h.raw_id() != id);
    }
    crate::set_void_out(out);
    0
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

/// Exit code recorded by `System.exit` (0 = not requested).
pub static TERMINATE_CODE: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

/// Set the context the `System` property getters read. The app calls this
/// after mounting the game and before running `startup.tjs`.
pub fn set_system_context(ctx: SystemContext) {
    *lock_ok(&SYSTEM_CONTEXT) = ctx;
}

/// Current context (defaults when never set).
fn system_context() -> SystemContext {
    lock_ok(&SYSTEM_CONTEXT).clone()
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
    set_int_out(out, 0);
    0
}

extern "C" fn prop_desktop_top(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    set_int_out(out, 0);
    0
}

extern "C" fn prop_desktop_width(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(out, system_context().screen_size.0 as i64);
    0
}

extern "C" fn prop_desktop_height(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(out, system_context().screen_size.1 as i64);
    0
}

extern "C" fn prop_touch_device(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    set_int_out(out, system_context().touch_device as i64);
    0
}
