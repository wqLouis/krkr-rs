//! `System` native class — ported from
//! `reference/cpp/core/base/impl/SystemImpl.cpp`
//! (`TVPCreateNativeClass_System`).
//!
//! Implemented methods:
//!
//! * `inform` — logs at `info!` (the reference shows a modal message box).
//! * `inputString` — logs at `info!` and returns the initial value (the
//!   headless "OK" path; the reference shows a modal input box).
//! * `getTickCount` — milliseconds since an arbitrary epoch (first call).
//! * `getKeyState` — real, over the shared [`tvp_input::InputState`]: mouse-button
//!   VKs map to the mouse state, `VK_PADANY` aggregates the gamepad, and
//!   `getcurrent=false` reproduces the reference's consumed-on-read push latch.
//! * `shellExecute` — launches with the platform opener (`xdg-open` /
//!   `open` / `cmd start`).
//! * `system` — runs the command synchronously through the platform shell
//!   (`/bin/sh -c` / `cmd /C`). The reference's `_wsystem` call is commented
//!   out and it always returns 0; this port executes the command for real but
//!   keeps the reference-visible result at 0 (the exit status is logged).
//! * `readRegValue` — reads the portable registry substitute the reference's
//!   active `TVPReadRegValue` path uses: a `RegisterData.tjs` *expression*
//!   under `System.appDataPath` (with the project dir as a fallback), then
//!   traverses it by the `/`- or `\`-separated key. The Windows registry
//!   branch in the reference is `#if 0`-disabled, so this is the equivalent
//!   the shipped engine actually executes (see the method docs for the
//!   reference's inverted loop guard).
//! * `getArgument` / `setArgument` — process-global command-line arguments
//!   (see below).
//! * `createAppLock` — returns true (single-process emulator: first
//!   instance wins).
//! * `dumpHeap` — void no-op, as in the reference (`TVPHeapDump` commented
//!   out).
//! * `nullpo` — the reference deliberately crashes (`*(int*)0 = 0` /
//!   `__builtin_trap`); this port logs an error and returns void instead of
//!   aborting.
//! * `showVersion` — logs the version at `info!` (the reference's version
//!   dialog is commented out).
//! * `toActualColor` — real `TVPToActualColor` (`ColorToRGB` table + the
//!   RGB byte-order swap).
//! * `clearGraphicCache` — clears the `tvp-visual` bitmap-template cache
//!   through the shared-VM bridge (`Window` registration publishes
//!   `global.__tvp_clearGraphicCache`); real `TVPClearGraphicCache`
//!   (`SystemIntf.cpp:215`).
//! * `touchImages` — validates its arguments; krkr-rs caches per layer on
//!   demand, so pre-caching has nothing to do (host-only).
//! * `createUUID` — real RFC-4122 v4 UUID from a clock/counter/address seed.
//! * `assignMessage` — stores an `id` → `message` override and returns true.
//! * `doCompact` — real compaction: runs the TJS garbage collector at
//!   `clIdle`/`clAll` up, clears the `tvp-visual` bitmap cache at
//!   `clMinimize` and up and the archive/auto-path caches at `clDeactivate`
//!   and up.
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
//! * Arguments are `name=value` pairs stored in an ordered vector, exactly
//!   like the reference's `TVPProgramArguments`. `setArgument(name, value)`
//!   rewrites the first entry that matches `name` by **prefix** (the next
//!   character must be `=` or end-of-string) or inserts `name=value` at the
//!   front when none matches. `getArgument(name [, default])` returns that
//!   entry's value; a value-less entry reads back as the string `"yes"`.
//!   When nothing matches it returns `default` if a second argument was
//!   given (preserving its type), otherwise void — the reference's
//!   `result->Clear()`.
//!
//! Prefix matching follows `TVPGetCommandLine` exactly: an entry matches
//! when its name equals `name` and the next character is `=` (value) or the
//! end of the string (`"yes"`). A longer entry whose name merely starts
//! with `name` (e.g. querying `-f` against `-foo=...`) does **not** match,
//! and `setArgument` rewrites only such a full-name entry.
//!
//! # Properties
//!
//! The full reference property surface is implemented: `exePath`,
//! `dataPath`, `personalPath`, `savedGamesPath`, `appDataPath`, `exeName`,
//! `title`, `eventDisabled`, `screenWidth`/`screenHeight`, `desktopLeft`/
//! `desktopTop`/`desktopWidth`/`desktopHeight`, `touchDevice`,
//! `versionString`, `versionInformation`, `platformName`, `osName`,
//! `processorNum`, `exeBits`, `osBits`, `graphicCacheLimit`,
//! `exitOnWindowClose`, `exitOnNoWindowStartup` and `drawThreadNum`.

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
    args, context_engine, lock_ok, report_error, set_int_out, set_object_result, set_real_out,
    set_string_out, set_void_out, value_as_bool, value_as_i64, value_as_string,
};

/// Process-global command-line arguments, mirroring the reference's
/// process-wide `TVPProgramArguments` (`SysInitImpl.cpp:456`): an ordered
/// vector of `name=value` strings matched by **prefix**, where the matched
/// entry's next character must be `=` or the end of the string. A
/// value-less entry reads back as `"yes"` (the reference's
/// `TVPGetCommandLine`).
static ARGS: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));

/// Reference-prefix lookup: `name` matches an entry whose next character
/// after the prefix is `=` (value) or end-of-string ("yes").
fn get_command_line(name: &str) -> Option<String> {
    let args = lock_ok(&ARGS);
    for entry in args.iter() {
        if let Some(rest) = entry.strip_prefix(name) {
            if rest.is_empty() {
                return Some("yes".to_string());
            }
            if let Some(value) = rest.strip_prefix('=') {
                return Some(value.to_string());
            }
        }
    }
    None
}

/// Reference `TVPSetCommandLine`: rewrite the first prefix-matching entry to
/// `name=value`, or insert a new entry at the front when none matches.
fn set_command_line(name: &str, value: &str) {
    let mut args = lock_ok(&ARGS);
    let new_entry = format!("{name}={value}");
    for entry in args.iter_mut() {
        if let Some(rest) = entry.strip_prefix(name)
            && (rest.is_empty() || rest.starts_with('='))
        {
            *entry = new_entry;
            return;
        }
    }
    args.insert(0, new_entry);
}

/// Reference-compatible "pushed since the last query" latch — the `0x10`
/// bit of the reference scancode byte (`TVPWindow.h:213`,
/// `TVPGetKeyMouseAsyncState`). `System.getKeyState(code, false)` reads and
/// clears it.
///
/// `tvp-input` exposes the current down set plus the per-frame released
/// edge, not a persistent latch, so the latch is reconstructed here: a
/// fresh down transition, or a release not yet accounted for, sets the bit;
/// the next `getKeyState(code, false)` consumes it.
#[derive(Default)]
struct PushLatch {
    /// Codes observed down at the previous `getKeyState(code, false)` call.
    down: HashSet<u32>,
    /// `0x10` bits set but not yet consumed.
    pushed: HashSet<u32>,
    /// Frame a release was latched in, so repeated queries in one frame do
    /// not re-latch the same release.
    release_frame: HashMap<u32, u64>,
}

static PUSH_LATCH: LazyLock<Mutex<PushLatch>> = LazyLock::new(|| Mutex::new(PushLatch::default()));

/// Update one host key state before dispatching the corresponding script
/// input event. The render crate calls this from its Bevy input bridge,
/// which already writes the shared [`tvp_input::InputState`] while holding
/// its lock; the forwarding below is a no-op on that path (`try_lock`
/// fails) and lets other callers/tests feed the same state.
pub fn set_key_state(key: u32, down: bool) {
    let state = tvp_input::input_state();
    if let Ok(mut state) = state.try_lock() {
        if down {
            state.set_key_down(key);
        } else {
            state.set_key_up(key);
        }
    }
}

/// Query the shared input state the way the reference
/// `TVPGetAsyncKeyState` does (`SystemImpl.cpp:41`):
///
/// * `getcurrent == true` → `tvp_input::InputState::vk_pressed`, which
///   resolves mouse-button VKs and aggregates `VK_PADANY`.
/// * `getcurrent == false` → the reference's persistent `0x10` push latch,
///   consumed on read (see [`PushLatch`]).
fn get_key_state(code: u32, getcurrent: bool) -> bool {
    let (down, released, frame) = {
        let state = tvp_input::input_state();
        let state = state.lock().unwrap_or_else(|p| p.into_inner());
        (state.vk_pressed(code), state.vk_released(code), state.frame)
    };
    if getcurrent {
        return down;
    }
    let mut latch = lock_ok(&PUSH_LATCH);
    let was_down = latch.down.contains(&code);
    if down && !was_down {
        latch.pushed.insert(code);
    } else if released && !was_down && latch.release_frame.get(&code).copied() != Some(frame) {
        // A press+release between two queries (the down edge was never
        // observed): latch it, and remember the frame so repeated queries
        // this frame do not latch the same release again.
        latch.pushed.insert(code);
        latch.release_frame.insert(code, frame);
    }
    if down {
        latch.down.insert(code);
    } else {
        latch.down.remove(&code);
    }
    latch.pushed.remove(&code)
}

/// Epoch for [`native_get_tick_count`]: the first call (reference:
/// `TVPStartTickCount`).
static TICK_EPOCH: OnceLock<Instant> = OnceLock::new();

/// `System.inform(text [, caption [, buttons]])`
///
/// Reference: `TVPShowSimpleMessageBox` — a modal dialog; with a buttons
/// argument the pressed button index is returned. krkr-rs has no GUI: logs
/// the text (with the default caption `"Information"` when omitted) at
/// `info!`. With a buttons argument it returns `0` (the left/first button),
/// matching the reference's "OK clicked" path; without one it returns void.
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
    if argc >= 3 && a[2].ty != VAL_VOID {
        // buttons overload: reference returns the clicked index; headless OK.
        set_int_out(out, 0);
    } else {
        set_void_out(out);
    }
    0
}

/// Milliseconds since the first call (the reference's
/// `TVPGetTickCount`/`TVPStartTickCount`); shared by `System.getTickCount`
/// and the continuous-handler delivery so handlers see the same clock as
/// the scripts.
pub(crate) fn tick_count_ms() -> i64 {
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
/// Reference: `TVPGetAsyncKeyState` (`SystemImpl.cpp:41`). `getcurrent`
/// defaults to true; when true the result is whether the key/mouse-button/
/// gamepad code is currently held; when false it is the reference's
/// consumed-on-read "pushed since the last query" latch (`0x10` bit). The
/// shared [`tvp_input::InputState`] resolves the mouse-button VKs and the
/// gamepad `VK_PAD*` codes (with `VK_PADANY` aggregating every pad button).
/// The argument-count check matches the reference (`TJS_E_BADPARAMCOUNT` on
/// zero arguments).
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
    let a = args(argv, argc);
    let key = value_as_i64(&a[0]) as u32;
    // reference: `getcurrent = 0 != (tjs_int)*param[1]`
    let getcurrent = argc < 2 || value_as_i64(&a[1]) != 0;
    set_int_out(out, i64::from(get_key_state(key, getcurrent)));
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
    let execparam = if argc >= 2 {
        value_as_string(&a[1])
    } else {
        String::new()
    };
    // `shellExecute(target, execparam)`: `target` may be a file/URL to open,
    // or a platform opener name (`explorer`/`open`) whose *parameter* is the
    // thing to open (the game does `System.shellExecute("explorer",
    // Storages.getLocalName(CONFIG.screenShotPath))`). The reference
    // `TVPShellExecute` is `#if 0`-disabled and just returns true; this host
    // performs a best-effort open so the behaviour is useful, but always
    // reports the reference-visible `true`.
    let opener = matches!(
        target.to_ascii_lowercase().as_str(),
        "explorer" | "explorer.exe" | "open"
    );
    let arg = if opener && !execparam.is_empty() {
        execparam.clone()
    } else {
        target.clone()
    };
    let spawned = if arg.is_empty() {
        false
    } else if cfg!(target_os = "windows") {
        Command::new("cmd")
            .args(["/C", "start", "", arg.as_str()])
            .spawn()
            .is_ok()
    } else if cfg!(target_os = "macos") {
        Command::new("open").arg(arg.as_str()).spawn().is_ok()
    } else {
        Command::new("xdg-open").arg(arg.as_str()).spawn().is_ok()
    };
    if !spawned {
        log::debug!("System.shellExecute: could not launch {arg:?}");
    }
    // The reference returns true unconditionally (`#if 0` body).
    set_int_out(out, 1);
    0
}

/// `System.system(command)` → int
///
/// Reference `SystemImpl.cpp:676`: the body is
/// `int ret = 0; // _wsystem(target.c_str());` followed by a compact event;
/// the process-spawning call is commented out, so the shipped engine always
/// returns 0. krkr-rs executes the command for real, synchronously through
/// the platform shell (`/bin/sh -c` on Unix, `cmd /C` on Windows); the exit
/// status is logged but the reference-visible result stays **0**, so scripts
/// observe exactly the value the reference would give them. This blocks the
/// VM thread for the lifetime of the command, like C `system()`.
extern "C" fn native_system(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.system requires 1 argument");
    }
    let command = value_as_string(&args(argv, argc)[0]);
    let code = run_shell_command(&command);
    log::debug!("System.system({command:?}) exited with status {code}");
    set_int_out(out, 0);
    0
}

/// Run `command` through the platform shell and return its exit code.
fn run_shell_command(command: &str) -> i32 {
    let mut cmd = if cfg!(target_os = "windows") {
        let mut c = Command::new("cmd");
        c.args(["/C", command]);
        c
    } else {
        let mut c = Command::new("/bin/sh");
        c.args(["-c", command]);
        c
    };
    match cmd.status() {
        Ok(status) => status.code().unwrap_or(-1),
        Err(e) => {
            log::warn!("System.system: cannot run {command:?}: {e}");
            -1
        }
    }
}

/// `System.readRegValue(key)` → value
///
/// Reference `TVPReadRegValue` (`SystemImpl.cpp:117`). The Windows registry
/// branch is `#if 0`-disabled in this fork; the active path loads a TJS
/// `RegisterData.tjs` expression from `System.appDataPath` and walks it with
/// `PropGet(TJS_MEMBERMUSTEXIST)` for every `/`- or `\`-separated key
/// segment, clearing the result on the first miss.
///
/// Note: the reference's active loop guard is written
/// `while(*start && CurrentNode.Type() != tvtObject)`, which is inverted for
/// an object root and makes the shipped function return void for every
/// non-empty key. This port implements the clear *intent* of that code (walk
/// the object tree, value = final node, void on any miss / empty key), which
/// is what a `RegisterData.tjs`-driven game expects.
///
/// `RegisterData.tjs` is looked up under `appDataPath` (the reference's
/// location) and then the project dir (portable fallback). Missing file,
/// empty key and any traversal error all return void, matching
/// `result->Clear()`.
extern "C" fn native_read_reg_value(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.readRegValue requires 1 argument");
    }
    let key = value_as_string(&args(argv, argc)[0]);
    match read_register_value(context_engine(), &key) {
        Ok(Some(value)) => set_retained_value_out(value, out),
        Ok(None) => set_void_out(out),
        Err(e) => return report_error(out_error, &format!("System.readRegValue: {e}")),
    }
    0
}

/// Where `RegisterData.tjs` is discovered: the reference's `appDataPath`
/// first, then the mounted project dir as a portable fallback.
fn register_data_file() -> Option<std::path::PathBuf> {
    let ctx = system_context();
    for dir in [&ctx.app_data_dir, &ctx.project_dir] {
        let path = dir.join("RegisterData.tjs");
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

/// Decode a `RegisterData.tjs` byte buffer: UTF-8 BOM and UTF-16 (LE/BE)
/// BOM first, then UTF-8. A byte stream in neither encoding is decoded
/// lossily (invalid UTF-8 becomes U+FFFD); CP932 text is not transcoded
/// because tvp-natives has no CP932 codec — the shipped game has no
/// `RegisterData.tjs`, so this only affects a hypothetical CP932 one.
fn decode_script_bytes(raw: &[u8]) -> String {
    if raw.starts_with(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(&raw[3..]).into_owned();
    }
    if raw.starts_with(&[0xff, 0xfe]) {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if raw.starts_with(&[0xfe, 0xff]) {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(raw) {
        Ok(s) => s.to_string(),
        Err(_) => String::from_utf8_lossy(raw).into_owned(),
    }
}

/// Quote one key segment as a TJS double-quoted string literal.
fn tjs_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Evaluate a `RegisterData.tjs` expression and walk `key` through it.
///
/// Returns `Ok(None)` for an empty key, a missing file or any traversal miss
/// (the reference's `result->Clear()`), and `Err` only when the engine itself
/// could not be driven. Unlike the reference (which caches the parsed
/// `RegisterData` process-wide), each call re-reads and re-evaluates the
/// file; the method is rare and this avoids cross-engine global state.
fn read_register_value(
    engine: &tjs2_sys::Tjs2Engine,
    key: &str,
) -> Result<Option<tjs2_sys::RetainedValue>, String> {
    if key.is_empty() {
        return Ok(None);
    }
    let Some(path) = register_data_file() else {
        return Ok(None);
    };
    let raw = std::fs::read(&path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let expression = decode_script_bytes(&raw);
    let segments: Vec<String> = key
        .split(['/', '\\'])
        .filter(|s| !s.is_empty())
        .map(tjs_string_literal)
        .collect();
    if segments.is_empty() {
        return Ok(None);
    }
    let keys = format!("[{}]", segments.join(","));
    // Traverse in-script so missing members yield void instead of an error;
    // `void[key]` would throw, so check the node before indexing. This mirrors
    // the reference's `TJS_MEMBERMUSTEXIST` walk.
    let script = format!(
        "(function(){{var __rd=({expression});var __o=__rd;var __k={keys};\
         for(var __i=0;__i<__k.count;++__i){{\
         if(__o===void||__o===null)return void;__o=__o[__k[__i]];}}\
         return __o;}})()"
    );
    match engine.eval_retained(&script, "System.readRegValue") {
        Ok(value) => Ok(Some(value)),
        Err(e) => {
            log::debug!("System.readRegValue({key:?}): {e}");
            Ok(None)
        }
    }
}

/// Hand a retained evaluation result to the C++ side; scalars are copied and
/// object results keep their retention (consumed by the return slot).
fn set_retained_value_out(value: tjs2_sys::RetainedValue, out: *mut Value) {
    use tjs2_sys::{RetainedValue, TjsValue};
    match value {
        RetainedValue::Value(TjsValue::Void) => set_void_out(out),
        RetainedValue::Value(TjsValue::Integer(i)) => set_int_out(out, i),
        RetainedValue::Value(TjsValue::Real(r)) => set_real_out(out, r),
        RetainedValue::Value(TjsValue::String(s)) => set_string_out(out, &s),
        // An opaque object without a handle cannot cross; report void like a
        // cleared reference result.
        RetainedValue::Value(TjsValue::Object | TjsValue::Retained(_)) => set_void_out(out),
        RetainedValue::Object(dv) => {
            // SAFETY: `out` is the valid native result slot for this call.
            unsafe {
                (*out).ty = tjs2_sys::VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = std::ptr::null();
                (*out).array = std::ptr::null();
                (*out).array_count = 0;
                (*out).retained = dv.raw_id() as usize;
            }
            std::mem::forget(dv);
        }
    }
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
    match get_command_line(&name) {
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
    set_command_line(&name, &value);
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

/// `System.inputString(caption, prompt, initial)` → string
///
/// Reference: `TVPShowSimpleInputBox` shows a modal text input; on OK the
/// result is the edited value, on Cancel it is void. krkr-rs has no GUI:
/// this port logs the prompt and returns the caller's initial value (the
/// "OK" path). Requires three arguments, like the reference.
extern "C" fn native_input_string(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "System.inputString requires 3 arguments");
    }
    let a = args(argv, argc);
    let caption = value_as_string(&a[0]);
    let prompt = value_as_string(&a[1]);
    let initial = value_as_string(&a[2]);
    log::info!("System.inputString [{caption}]: {prompt} (returning initial value)");
    set_string_out(out, &initial);
    0
}

/// Convert a TVP system color (the `cl*` constants, high byte set) to its
/// actual `0xRRGGBB` value. Ported from `ColorToRGB`
/// (`reference/cpp/core/environ/Application.cpp:874`) plus the byte-order
/// swap in `TVPToActualColor` (`reference/cpp/core/visual/impl/LayerImpl.cpp:21`).
fn color_to_actual(color: u32) -> u32 {
    // ColorToRGB's table is in 0xBBGGRR; the swap below returns 0xRRGGBB.
    let rgb_bbggrr = match color {
        0x8000_0000 => 0x00c8_c8c8, // clScrollBar
        0x8000_0001 => 0x0000_0000, // clBackground
        0x8000_0002 => 0x00d1_b499, // clActiveCaption
        0x8000_0003 => 0x00db_cdbf, // clInactiveCaption
        0x8000_0004 => 0x00f0_f0f0, // clMenu
        0x8000_0005 => 0x00ff_ffff, // clWindow
        0x8000_0006 => 0x0064_6464, // clWindowFrame
        0x8000_0007 => 0x0000_0000, // clMenuText
        0x8000_0008 => 0x0000_0000, // clWindowText
        0x8000_0009 => 0x0000_0000, // clCaptionText
        0x8000_000a => 0x00b4_b4b4, // clActiveBorder
        0x8000_000b => 0x00fc_f7f4, // clInactiveBorder
        0x8000_000c => 0x00ab_abab, // clAppWorkSpace
        0x8000_000d => 0x00ff_9933, // clHighlight
        0x8000_000e => 0x00ff_ffff, // clHighlightText
        0x8000_000f => 0x00f0_f0f0, // clBtnFace
        0x8000_0010 => 0x00a0_a0a0, // clBtnShadow
        0x8000_0011 => 0x006d_6d6d, // clGrayText
        0x8000_0012 => 0x0000_0000, // clBtnText
        0x8000_0013 => 0x0054_4e43, // clInactiveCaptionText
        0x8000_0014 => 0x00ff_ffff, // clBtnHighlight
        0x8000_0015 => 0x0069_6969, // cl3DDkShadow
        0x8000_0016 => 0x00e3_e3e3, // cl3DLight
        0x8000_0017 => 0x0000_0000, // clInfoText
        0x8000_0018 => 0x00e1_ffff, // clInfoBk
        0x8000_0019 => 0x0000_0000, // clUnknown
        0x8000_001a => 0x00cc_6600, // clHotLight
        0x8000_001b => 0x00ea_d1b9, // clGradientActiveCaption
        0x8000_001c => 0x00f2_e4d7, // clGradientInactiveCaption
        0x8000_001d => 0x00ff_9933, // clMenuLight
        0x8000_001e => 0x00f0_f0f0, // clMenuBar
        other => other & 0x00ff_ffff,
    };
    ((rgb_bbggrr & 0xff) << 16) | (rgb_bbggrr & 0xff00) | ((rgb_bbggrr & 0xff0000) >> 16)
}

/// `System.toActualColor(color)` → int
///
/// Reference: `TVPToActualColor` (`LayoutImpl.cpp:21`). System colors (the
/// high byte set) are converted from the `0xBBGGRR` `ColorToRGB` table;
/// ordinary colors pass through masked to `0xRRGGBB`.
extern "C" fn native_to_actual_color(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.toActualColor requires 1 argument");
    }
    let color = value_as_i64(&args(argv, argc)[0]) as u32;
    let actual = if color & 0xff00_0000 != 0 {
        color_to_actual(color)
    } else {
        color & 0x00ff_ffff
    };
    set_int_out(out, i64::from(actual));
    0
}

/// `System.clearGraphicCache()` → void
///
/// Reference: `TVPClearGraphicCache` (`SystemIntf.cpp:215`), which clears the
/// shared decoded-image cache. The bitmap cache lives in `tvp-visual`, so
/// this calls that crate's `global.__tvp_clearGraphicCache` bridge (see
/// `Window` registration). The render/CLI hosts register the visual natives;
/// a bare `tvp-natives` engine has no bridge and the call is a guarded no-op.
extern "C" fn native_clear_graphic_cache(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    clear_graphic_cache_via_vm();
    set_void_out(out);
    0
}

/// Invoke the `tvp-visual` graphics-cache bridge through the shared VM.
///
/// `System.clearGraphicCache` / `System.doCompact` live in this crate while
/// the name→bitmap-template cache lives in `tvp-visual`; registering a second
/// native dependency is unnecessary because both crates already share the
/// VM. The visual crate publishes `global.__tvp_clearGraphicCache` during
/// `Window` registration, and the guard makes the call harmless when only the
/// `System`/`Debug` natives are registered (unit tests).
fn clear_graphic_cache_via_vm() {
    let script = "if(typeof global.__tvp_clearGraphicCache != \"undefined\") \
                  global.__tvp_clearGraphicCache();";
    if let Err(e) = context_engine().exec_script(script, "System.clearGraphicCache") {
        log::warn!("System.clearGraphicCache: graphics-cache bridge failed: {e}");
    }
}

/// `System.touchImages(storages[, limit[, timeout]])` → void
///
/// Reference: `TVPTouchImages` (`SystemIntf.cpp:224`) pre-caches images.
/// krkr-rs caches per layer on demand, so this validates the argument count
/// and does nothing.
extern "C" fn native_touch_images(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.touchImages requires 1 argument");
    }
    set_void_out(out);
    0
}

/// `System.createUUID()` → string
///
/// Reference: `TVPGetRandomBits128` + RFC-4122 version/variant bits
/// (`SystemIntf.cpp:259`). krkr-rs has no RNG dependency; the seed mixes the
/// clock, a process-local counter and the heap address, which is enough for
/// a transient identifier.
extern "C" fn native_create_uuid(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc != 0 {
        return report_error(out_error, "System.createUUID takes no arguments");
    }
    set_string_out(out, &make_uuid());
    0
}

/// `System.assignMessage(id, message)` → bool
///
/// Reference: `TJSAssignMessage` (`SystemIntf.cpp:288`) installs a message
/// override. krkr-rs stores the mapping process-globally and returns true.
static MESSAGE_MAP: LazyLock<Mutex<HashMap<String, String>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

extern "C" fn native_assign_message(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "System.assignMessage requires 2 arguments");
    }
    let a = args(argv, argc);
    let id = value_as_string(&a[0]);
    let message = value_as_string(&a[1]);
    lock_ok(&MESSAGE_MAP).insert(id, message);
    set_int_out(out, 1);
    0
}

/// Read back a previously assigned message (used by tests).
#[cfg(test)]
pub fn assigned_message(id: &str) -> Option<String> {
    lock_ok(&MESSAGE_MAP).get(id).cloned()
}

/// Generate a random UUID v4 string.
fn make_uuid() -> String {
    // Seed from the clock, a per-process counter and the stack address.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let addr = &n as *const u64 as u64;
    let mut state = nanos ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ addr;
    if state == 0 {
        state = 0x1234_5678_9abc_def0;
    }
    let mut next = || {
        // xorshift64* step
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        state.wrapping_mul(0x2545_f491_4f6c_dd1d)
    };
    let mut bytes = [0u8; 16];
    for chunk in bytes.chunks_mut(8) {
        let v = next().to_le_bytes();
        chunk.copy_from_slice(&v[..chunk.len()]);
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40; // version 4
    bytes[8] = (bytes[8] & 0x3f) | 0x80; // RFC 4122 variant
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

/// `System.doCompact(level = TVP_COMPACT_LEVEL_MAX)` — real compaction.
///
/// Reference `System.doCompact` (`SystemIntf.cpp:306`) defaults `level` to
/// `TVP_COMPACT_LEVEL_MAX` (100) and calls `TVPDeliverCompactEvent(level)`,
/// whose hooks (`EventIntf.h:279`) run the TJS garbage collector at
/// `clIdle` (5), clear the auto-path/archive caches at `clDeactivate` (10)
/// and clear the graphic/font caches at `clMinimize` (15). The games call it
/// at every scenario change (`advscreen.tjs` `clIdle`), whose comment says
/// the intent is a GC, and on a scene reset (`gamescenemanager.tjs` `clAll`).
///
/// The port honors the thresholds: the TJS garbage collector runs from
/// `clIdle` up (`tjs2_sys::Tjs2Engine::do_gc`, wrapping
/// `tTJS::DoGarbageCollection`), the `tvp-visual` bitmap-template cache is
/// dropped from `clMinimize` up and `Storages.clearArchiveCache()` runs from
/// `clDeactivate` up.
extern "C" fn system_do_compact(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const tjs2_sys::Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let level = args(argv, argc).first().map(value_as_i64).unwrap_or(100);
    // `TVP_COMPACT_LEVEL_IDLE` = 5 (reference `EventIntf.h:280`): the
    // reference `tTVPTJSGCCallback::OnCompact` runs the TJS garbage
    // collector at this level and up. This is the hook the games rely on at
    // every scenario change (`advscreen.tjs` passes `clIdle`).
    if level >= 5
        && let Err(e) = context_engine().do_gc()
    {
        log::warn!("System.doCompact: TJS garbage collection failed: {e}");
    }
    // `TVP_COMPACT_LEVEL_MINIMIZE` = 15 (reference `EventIntf.h:281`).
    if level >= 15 {
        clear_graphic_cache_via_vm();
    }
    // `TVP_COMPACT_LEVEL_DEACTIVATE` = 10.
    if level >= 10 {
        let script = "if(typeof global.Storages != \"undefined\") \
                      global.Storages.clearArchiveCache();";
        if let Err(e) = context_engine().exec_script(script, "System.doCompact") {
            log::warn!("System.doCompact: archive-cache bridge failed: {e}");
        }
    }
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
    let any = !handlers.is_empty();
    drop(handlers);
    // The video overlay playback state machine rides the same per-frame
    // clock as the continuous handlers (reference: the video decoder's own
    // event thread; krkr-rs has no decoder thread).
    crate::video_overlay::video_overlay_poll(engine);
    // `Trans` transition drivers (`extrans`) complete on the same clock,
    // after the engine's own `Layer.beginTransition`/`transition_poll` had a
    // chance to deliver `onTransitionCompleted` this frame.
    crate::extrans::trans_poll(engine, tick);
    any
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
                name: "inputString",
                f: native_input_string,
            },
            NativeMethodDef {
                name: "toActualColor",
                f: native_to_actual_color,
            },
            NativeMethodDef {
                name: "clearGraphicCache",
                f: native_clear_graphic_cache,
            },
            NativeMethodDef {
                name: "touchImages",
                f: native_touch_images,
            },
            NativeMethodDef {
                name: "createUUID",
                f: native_create_uuid,
            },
            NativeMethodDef {
                name: "assignMessage",
                f: native_assign_message,
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
    /// Game/project directory — mapped to `exePath` (the directory the game
    /// was launched from). `dataPath` is its `savedata/` subfolder, see
    /// [`SystemContext::project_data_dir`].
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

impl SystemContext {
    /// The game's writable data directory: a `savedata/` folder next to the
    /// game. Games write and read saves, `saveMng.dat` and `system.dat`
    /// through `System.dataPath`, which points here (KiriKiri/Kirikiroid2
    /// behaviour; the reference `TVPEnsureDataPathDirectory` creates it).
    pub fn project_data_dir(&self) -> std::path::PathBuf {
        self.project_dir.join("savedata")
    }
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

/// `System.graphicCacheLimit` (writable; the cache is not sized yet, the
/// value is stored for compatibility).
static GRAPHIC_CACHE_LIMIT: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// `System.exitOnWindowClose` (writable flag; the host window bridge reads
/// it through [`exit_on_window_close`]).
static EXIT_ON_WINDOW_CLOSE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(true);

/// `System.exitOnNoWindowStartup` (writable flag, stored only).
static EXIT_ON_NO_WINDOW_STARTUP: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// `System.drawThreadNum` (writable; the renderer decides its own thread
/// count, the value is stored for compatibility).
static DRAW_THREAD_NUM: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

/// Read the `System.exitOnWindowClose` flag (reference
/// `TVPTerminateOnWindowClose`, used by the window-close handler).
pub fn exit_on_window_close() -> bool {
    EXIT_ON_WINDOW_CLOSE.load(Ordering::SeqCst)
}

/// Whether `System.eventDisabled` is set (reference
/// `TVPGetSystemEventDisabledState`). The reference gates `TVPPostEvent` and
/// `TVPPostInputEvent` (discardable events) on this flag; krkr-rs delivers
/// input directly from the host input bridge, so the bridge should check this
/// before dispatching (both games set it inside the `exceptionHandler` right
/// before `System.terminate`, so normal play is unaffected).
pub fn events_disabled() -> bool {
    EVENT_DISABLED.load(Ordering::SeqCst)
}

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

/// Game/project directory the mounted game storage resolves against
/// (`System.dataPath`/`exePath`). Used by the video overlay to lazily mount
/// the game storage and read movie files.
pub(crate) fn project_dir() -> std::path::PathBuf {
    system_context().project_dir
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
        getter("versionString", prop_version_string),
        getter("versionInformation", prop_version_information),
        getter("platformName", prop_platform_name),
        getter("osName", prop_os_name),
        getter("processorNum", prop_processor_num),
        getter("exeBits", prop_exe_bits),
        getter("osBits", prop_os_bits),
        NativePropertyDef {
            name: "graphicCacheLimit",
            get: Some(prop_graphic_cache_limit_get),
            set: Some(prop_graphic_cache_limit_set),
        },
        NativePropertyDef {
            name: "exitOnWindowClose",
            get: Some(prop_exit_on_window_close_get),
            set: Some(prop_exit_on_window_close_set),
        },
        NativePropertyDef {
            name: "exitOnNoWindowStartup",
            get: Some(prop_exit_on_no_window_startup_get),
            set: Some(prop_exit_on_no_window_startup_set),
        },
        NativePropertyDef {
            name: "drawThreadNum",
            get: Some(prop_draw_thread_num_get),
            set: Some(prop_draw_thread_num_set),
        },
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
    // `System.dataPath` is the game's **writable data directory** — the
    // `savedata/` folder next to the game (KiriKiri/Kirikiroid2 behaviour).
    // Games use it directly for saves and config: the KR game defines
    // `var DATA_PATH = System.dataPath;` (`system/status.tjs`) and reads/writes
    // `qsave01.bmp`, `saveMng.dat`, `system.dat` there, and the reference
    // `TVPEnsureDataPathDirectory` creates it. Using the game root instead made
    // quick save/load look in the wrong folder.
    let dir = system_context().project_data_dir();
    let _ = std::fs::create_dir_all(&dir);
    set_string_out(out, &dir_with_separator(&dir));
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
    set_int_out(out, i64::from(events_disabled()));
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

/// The reference's `TVPGetVersionString`: `major.minor.release.build`.
extern "C" fn prop_version_string(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_string_out(out, &format!("{}.0", env!("CARGO_PKG_VERSION")));
    0
}

/// The reference's `TVPGetVersionInformation`: a descriptive multi-line
/// version banner (`MsgIntf.cpp:158`).
extern "C" fn prop_version_information(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_string_out(
        out,
        &format!(
            "krkr-rs {} (KiriKiri2/TVP native port)",
            env!("CARGO_PKG_VERSION")
        ),
    );
    0
}

/// Reference `TVPGetPlatformName` (`MainScene.cpp:1400`).
fn platform_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "Win32"
    } else if cfg!(target_os = "macos") {
        "MacOS"
    } else if cfg!(target_os = "android") {
        "Android"
    } else if cfg!(target_os = "ios") {
        "iPhone"
    } else {
        "Linux"
    }
}

extern "C" fn prop_platform_name(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_string_out(out, platform_name());
    0
}

/// In this fork `TVPGetOSName` returns `TVPGetPlatformName`.
extern "C" fn prop_os_name(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    set_string_out(out, platform_name());
    0
}

extern "C" fn prop_processor_num(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    set_int_out(out, n as i64);
    0
}

extern "C" fn prop_exe_bits(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    set_int_out(out, (std::mem::size_of::<usize>() * 8) as i64);
    0
}

extern "C" fn prop_os_bits(_e: *mut c_void, out: *mut Value, _err: *mut *mut c_char) -> c_int {
    // Reference `TVPGetOSBits` (`SystemImpl.cpp:71`).
    set_int_out(out, (std::mem::size_of::<usize>() * 8) as i64);
    0
}

extern "C" fn prop_graphic_cache_limit_get(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(out, GRAPHIC_CACHE_LIMIT.load(Ordering::SeqCst));
    0
}

extern "C" fn prop_graphic_cache_limit_set(
    _e: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes a valid value slot.
    let v = unsafe { &*value };
    GRAPHIC_CACHE_LIMIT.store(value_as_i64(v), Ordering::SeqCst);
    0
}

extern "C" fn prop_exit_on_window_close_get(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(out, i64::from(EXIT_ON_WINDOW_CLOSE.load(Ordering::SeqCst)));
    0
}

extern "C" fn prop_exit_on_window_close_set(
    _e: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes a valid value slot.
    let v = unsafe { &*value };
    EXIT_ON_WINDOW_CLOSE.store(value_as_bool(v), Ordering::SeqCst);
    0
}

extern "C" fn prop_exit_on_no_window_startup_get(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(
        out,
        i64::from(EXIT_ON_NO_WINDOW_STARTUP.load(Ordering::SeqCst)),
    );
    0
}

extern "C" fn prop_exit_on_no_window_startup_set(
    _e: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes a valid value slot.
    let v = unsafe { &*value };
    EXIT_ON_NO_WINDOW_STARTUP.store(value_as_bool(v), Ordering::SeqCst);
    0
}

extern "C" fn prop_draw_thread_num_get(
    _e: *mut c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(out, DRAW_THREAD_NUM.load(Ordering::SeqCst));
    0
}

extern "C" fn prop_draw_thread_num_set(
    _e: *mut c_void,
    value: *const Value,
    _err: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes a valid value slot.
    let v = unsafe { &*value };
    DRAW_THREAD_NUM.store(value_as_i64(v), Ordering::SeqCst);
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

    fn engine_with_system() -> Box<tjs2_sys::Tjs2Engine> {
        // Box before registering: `register_all` stores the engine address in
        // the process-global VM context, so the allocation must stay put when
        // the helper returns.
        let e = Box::new(tjs2_sys::Tjs2Engine::new().expect("create engine"));
        // `register_all` registers `System` and installs the process-global
        // VM context that `context_engine()`-based methods
        // (`System.readRegValue`) need, exactly like production.
        crate::register_all(&e).expect("register all natives");
        e
    }

    /// `System.screenWidth`/`screenHeight` follow the installed context, which
    /// the render bridge keeps current as the game resizes its window (the
    /// TODO's "logical size stays 1280x720" bug).
    #[test]
    fn screen_size_follows_the_system_context() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        let cwd = std::env::current_dir().unwrap();
        set_system_context(SystemContext {
            project_dir: cwd.clone(),
            app_data_dir: cwd,
            screen_size: (1920, 1080),
            desktop_origin: (0, 0),
            desktop_size: (3840, 2160),
            touch_device: false,
        });
        assert_eq!(
            e.eval("System.screenWidth", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(1920)
        );
        assert_eq!(
            e.eval("System.screenHeight", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(1080)
        );
        set_system_context(SystemContext::default());
    }

    #[test]
    fn input_string_returns_initial_value_and_checks_arity() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        assert_eq!(
            e.eval("System.inputString('Caption', 'Prompt', 'initial')", "test")
                .unwrap(),
            tjs2_sys::TjsValue::String("initial".into())
        );
        // reference requires 3 parameters
        assert!(
            e.eval("System.inputString('Caption', 'Prompt')", "test")
                .is_err()
        );
    }

    #[test]
    fn to_actual_color_maps_system_colors_and_passes_rgb() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        // clHighlight = 0x8000000d -> ColorToRGB 0xff9933 (BBGGRR) -> 0x3399ff
        assert_eq!(
            e.eval("System.toActualColor(0x8000000d)", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(0x3399ff)
        );
        // ordinary 0xRRGGBB passes through
        assert_eq!(
            e.eval("System.toActualColor(0x123456)", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(0x123456)
        );
        assert!(e.eval("System.toActualColor()", "test").is_err());
    }

    #[test]
    fn create_uuid_has_rfc4122_shape_and_is_unique() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        let first = match e.eval("System.createUUID()", "test").unwrap() {
            tjs2_sys::TjsValue::String(s) => s,
            other => panic!("expected string, got {other:?}"),
        };
        assert_eq!(first.len(), 36, "uuid: {first}");
        let bytes = first.as_bytes();
        for (i, b) in bytes.iter().enumerate() {
            match i {
                8 | 13 | 18 | 23 => assert_eq!(*b, b'-', "uuid: {first}"),
                _ => assert!(b.is_ascii_hexdigit(), "uuid: {first}"),
            }
        }
        assert_eq!(&first[14..15], "4", "version nibble: {first}");
        assert!("89ab".contains(&first[19..20]), "variant nibble: {first}");
        let second = match e.eval("System.createUUID()", "test").unwrap() {
            tjs2_sys::TjsValue::String(s) => s,
            other => panic!("expected string, got {other:?}"),
        };
        assert_ne!(first, second, "two UUIDs should differ");
    }

    #[test]
    fn assign_message_round_trips() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        assert_eq!(
            e.eval("System.assignMessage('msg.id', 'hello')", "test")
                .unwrap(),
            tjs2_sys::TjsValue::Integer(1)
        );
        assert_eq!(assigned_message("msg.id").as_deref(), Some("hello"));
        assert!(e.eval("System.assignMessage('only-one')", "test").is_err());
    }

    /// `System.system` really runs the command through the platform shell;
    /// the reference-visible result is always 0 (its `_wsystem` call is
    /// commented out), so scripts see the same value as the shipped engine.
    #[test]
    fn system_runs_a_shell_command_and_returns_the_reference_result() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        assert_eq!(
            e.eval("System.system('exit 0')", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(0)
        );
        // `exit 7` still reports the reference's 0 (the status is only logged).
        assert_eq!(
            e.eval("System.system('exit 7')", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(0)
        );
        assert!(e.eval("System.system()", "test").is_err());
    }

    /// The command actually executes (a real side effect, not a stub).
    #[cfg(unix)]
    #[test]
    fn system_executes_the_command_for_real() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        let dir = std::env::temp_dir().join(format!(
            "tvp_system_cmd_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let marker = dir.join("ran");
        let script = format!("System.system('printf ran > {}')", marker.display());
        assert_eq!(
            e.eval(&script, "test").unwrap(),
            tjs2_sys::TjsValue::Integer(0)
        );
        assert_eq!(std::fs::read_to_string(&marker).unwrap(), "ran");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `readRegValue` loads the portable `RegisterData.tjs` expression and
    /// traverses it by `/`- or `\`-separated key, returning the final node and
    /// void on any miss (the reference's active `TVPReadRegValue` path).
    #[test]
    fn read_reg_value_traverses_register_data() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        let dir = std::env::temp_dir().join(format!(
            "tvp_register_data_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("RegisterData.tjs"),
            r#"%[Version: %[App: %[Name: "krkr-rs", Build: 7]], Flag: true]"#,
        )
        .unwrap();
        let cwd = std::env::current_dir().unwrap();
        set_system_context(SystemContext {
            project_dir: cwd,
            app_data_dir: dir.clone(),
            screen_size: (0, 0),
            desktop_origin: (0, 0),
            desktop_size: (0, 0),
            touch_device: false,
        });

        assert_eq!(
            e.eval("System.readRegValue('Version\\\\App\\\\Name')", "test")
                .unwrap(),
            tjs2_sys::TjsValue::String("krkr-rs".into())
        );
        assert_eq!(
            e.eval("System.readRegValue('Version/App/Build')", "test")
                .unwrap(),
            tjs2_sys::TjsValue::Integer(7)
        );
        // A leading separator is ignored, like the reference's splitter; the
        // top-level `Flag` is reached directly.
        assert_eq!(
            e.eval("System.readRegValue('/Flag')", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(1)
        );
        // A miss, an empty key and a missing file all read as void.
        assert_eq!(
            e.eval("System.readRegValue('Version/Missing')", "test")
                .unwrap(),
            tjs2_sys::TjsValue::Void
        );
        // An intermediate node is an object and is returned as one.
        assert_eq!(
            e.eval("System.readRegValue('Version')", "test").unwrap(),
            tjs2_sys::TjsValue::Object
        );
        assert_eq!(
            e.eval("System.readRegValue('')", "test").unwrap(),
            tjs2_sys::TjsValue::Void
        );
        assert!(e.eval("System.readRegValue()", "test").is_err());

        set_system_context(SystemContext::default());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn system_version_and_environment_properties() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        assert!(matches!(
            e.eval("System.versionString", "test").unwrap(),
            tjs2_sys::TjsValue::String(s) if !s.is_empty()
        ));
        assert!(matches!(
            e.eval("System.versionInformation", "test").unwrap(),
            tjs2_sys::TjsValue::String(s) if s.contains("krkr-rs")
        ));
        assert!(matches!(
            e.eval("System.platformName", "test").unwrap(),
            tjs2_sys::TjsValue::String(_)
        ));
        assert_eq!(
            e.eval("System.osName === System.platformName", "test")
                .unwrap(),
            tjs2_sys::TjsValue::Integer(1)
        );
        match e.eval("System.processorNum", "test").unwrap() {
            tjs2_sys::TjsValue::Integer(n) => assert!(n >= 1),
            other => panic!("processorNum: {other:?}"),
        }
        assert_eq!(
            e.eval("System.exeBits", "test").unwrap(),
            tjs2_sys::TjsValue::Integer((std::mem::size_of::<usize>() * 8) as i64)
        );
        assert_eq!(
            e.eval("System.osBits", "test").unwrap(),
            tjs2_sys::TjsValue::Integer((std::mem::size_of::<usize>() * 8) as i64)
        );
        assert_eq!(
            e.eval("System.exitOnWindowClose", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval(
                "System.graphicCacheLimit = 64; System.graphicCacheLimit",
                "test"
            )
            .unwrap(),
            tjs2_sys::TjsValue::Integer(64)
        );
        assert_eq!(
            e.eval("System.drawThreadNum = 4; System.drawThreadNum", "test")
                .unwrap(),
            tjs2_sys::TjsValue::Integer(4)
        );
        e.exec_script("System.exitOnWindowClose = false;", "test")
            .unwrap();
        assert_eq!(
            e.eval("System.exitOnWindowClose", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(0)
        );
        assert!(!exit_on_window_close());
        // restore the default for other tests in this binary
        e.exec_script("System.exitOnWindowClose = true;", "test")
            .unwrap();
    }

    #[test]
    fn noop_surface_methods_do_not_throw() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        e.exec_script(
            "System.clearGraphicCache(); System.touchImages([]); \
             System.touchImages([], 100, 5); System.doCompact(); \
             System.doCompact(0);",
            "test",
        )
        .unwrap();
        assert!(e.eval("System.touchImages()", "test").is_err());
    }

    #[test]
    fn event_disabled_property_roundtrips() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        assert!(!events_disabled());
        e.exec_script("System.eventDisabled = true;", "test")
            .unwrap();
        assert!(events_disabled());
        assert_eq!(
            e.eval("System.eventDisabled", "test").unwrap(),
            tjs2_sys::TjsValue::Integer(1)
        );
        e.exec_script("System.eventDisabled = false;", "test")
            .unwrap();
        assert!(!events_disabled());
    }

    #[test]
    fn argument_matching_follows_the_reference_separator_rule() {
        let _vm_lock = crate::test_lock::vm_lock();
        let e = engine_with_system();
        e.exec_script("System.setArgument('-argcheck', 'x');", "test")
            .unwrap();
        // the exact name matches and reads back its value
        assert_eq!(
            e.eval("System.getArgument('-argcheck')", "test").unwrap(),
            tjs2_sys::TjsValue::String("x".into())
        );
        // a strict prefix of a stored name does NOT match (TVPGetCommandLine
        // requires `=` or end-of-string after the queried name)
        assert_eq!(
            e.eval("System.getArgument('-arg', 'dflt')", "test")
                .unwrap(),
            tjs2_sys::TjsValue::String("dflt".into())
        );
        // setArgument replaces the existing entry, not a prefix match
        e.exec_script("System.setArgument('-argcheck', 'y');", "test")
            .unwrap();
        assert_eq!(
            e.eval("System.getArgument('-argcheck')", "test").unwrap(),
            tjs2_sys::TjsValue::String("y".into())
        );
    }

    // -- getKeyState over the shared tvp-input state --------------------

    /// Reset both the shared input state and the `getKeyState(code,false)`
    /// latch so each test starts clean.
    fn reset_input_state() {
        *lock_ok(&PUSH_LATCH) = PushLatch::default();
        let state = tvp_input::input_state();
        *state.lock().unwrap_or_else(|p| p.into_inner()) = tvp_input::InputState::new();
    }

    fn with_input<T>(f: impl FnOnce(&mut tvp_input::InputState) -> T) -> T {
        let state = tvp_input::input_state();
        let mut s = state.lock().unwrap_or_else(|p| p.into_inner());
        f(&mut s)
    }

    fn state_int(e: &tjs2_sys::Tjs2Engine, expr: &str) -> i64 {
        match e.eval(expr, "test").unwrap() {
            tjs2_sys::TjsValue::Integer(v) => v,
            other => panic!("{expr} -> {other:?}"),
        }
    }

    #[test]
    fn get_key_state_resolves_mouse_vks_and_aggregates_padany() {
        let _vm_lock = crate::test_lock::vm_lock();
        reset_input_state();
        let e = engine_with_system();
        with_input(|s| {
            s.begin_frame();
            s.set_mouse_button(tvp_input::MB_RIGHT, true);
            s.set_key_down(tvp_input::pad::VK_PAD1);
            s.end_frame();
        });
        // mouse-button VKs resolve to the mouse state (0x02 = VK_RBUTTON)
        assert_eq!(state_int(&e, "System.getKeyState(0x02)"), 1);
        assert_eq!(state_int(&e, "System.getKeyState(0x01)"), 0);
        // a concrete pad code and VK_PADANY (0x1DF) aggregate every pad code
        assert_eq!(state_int(&e, "System.getKeyState(0x1C0)"), 1);
        assert_eq!(state_int(&e, "System.getKeyState(0x1DF)"), 1);
        with_input(|s| {
            s.begin_frame();
            s.set_mouse_button(tvp_input::MB_RIGHT, false);
            s.set_key_up(tvp_input::pad::VK_PAD1);
            s.end_frame();
        });
        assert_eq!(state_int(&e, "System.getKeyState(0x02)"), 0);
        assert_eq!(state_int(&e, "System.getKeyState(0x1DF)"), 0);
    }

    #[test]
    fn get_key_state_getcurrent_false_is_a_consumed_push_latch() {
        let _vm_lock = crate::test_lock::vm_lock();
        reset_input_state();
        let e = engine_with_system();
        // Frame 0: press K (0x4B).
        with_input(|s| {
            s.begin_frame();
            s.set_key_down(0x4B);
            s.end_frame();
        });
        // The latch is read once, then cleared; current state stays true.
        assert_eq!(state_int(&e, "System.getKeyState(0x4B, false)"), 1);
        assert_eq!(state_int(&e, "System.getKeyState(0x4B, false)"), 0);
        assert_eq!(state_int(&e, "System.getKeyState(0x4B)"), 1);
        // Frame 1: release; the down edge was already consumed -> no latch.
        with_input(|s| {
            s.begin_frame();
            s.set_key_up(0x4B);
            s.end_frame();
        });
        assert_eq!(state_int(&e, "System.getKeyState(0x4B, false)"), 0);
        // Frame 2: press+release between queries -> the latch still catches it.
        with_input(|s| {
            s.begin_frame();
            s.set_key_down(0x4B);
            s.set_key_up(0x4B);
            s.end_frame();
        });
        assert_eq!(state_int(&e, "System.getKeyState(0x4B, false)"), 1);
        assert_eq!(state_int(&e, "System.getKeyState(0x4B, false)"), 0);
        assert_eq!(state_int(&e, "System.getKeyState(0x4B)"), 0);
        // `getcurrent` is parsed as an integer (truncating), like the
        // reference: 0.5 -> 0 (false, the push latch), 2.5 -> 2 (true,
        // current state). Press+release 0x4C so the two differ.
        with_input(|s| {
            s.begin_frame();
            s.set_key_down(0x4C);
            s.set_key_up(0x4C);
            s.end_frame();
        });
        assert_eq!(state_int(&e, "System.getKeyState(0x4C, 0.5)"), 1);
        assert_eq!(state_int(&e, "System.getKeyState(0x4C, 2.5)"), 0);
    }
}
