//! `System` native class.
//!
//! Behavioral reference: `main`'s `crates/tvp-natives/src/system.rs` (itself a
//! port of `reference/cpp/core/base/impl/SystemImpl.cpp`). This is a
//! reimplementation on the layered API: the class composes the L2
//! [`engine::Storage`] and [`engine::Scene`] services, the `tjs` VM and the
//! per-engine dispatch state kept in [`CompatServices::state`] — it owns no
//! process global.
//!
//! # Implemented members
//!
//! Methods: `inform`, `getTickCount`, `getArgument`, `setArgument`,
//! `createAppLock`, `exit`, `terminate`, `dumpHeap`, `nullpo`, `showVersion`,
//! `inputString`, `toActualColor`, `createUUID`, `assignMessage`,
//! `doCompact`, `readRegValue`, `touchImages`, `addContinuousHandler`,
//! `removeContinuousHandler`, `getKeyState`, `shellExecute`, `system`,
//! `clearGraphicCache`.
//!
//! Properties: `exePath`, `dataPath`, `personalPath`, `savedGamesPath`,
//! `exeName`, `title`, `eventDisabled`, `screenWidth`, `screenHeight`,
//! `versionString`, `versionInformation`, `platformName`, `osName`,
//! `processorNum`, `exeBits`, `osBits`, `graphicCacheLimit`,
//! `exitOnWindowClose`, `exitOnNoWindowStartup`, `drawThreadNum`,
//! `appDataPath`, `desktopLeft`, `desktopTop`, `desktopWidth`,
//! `desktopHeight`, `touchDevice`.
//!
//! # Composition
//!
//! `getKeyState` reads the attached L2 [`engine::input::InputState`]
//! (TVP virtual-key codes and pad/`VK_PADANY` semantics are the same as the
//! `Key` class); `touchDevice` derives its touch-capability bits from the
//! active contacts in the same L2 state; `clearGraphicCache` releases every
//! unreferenced bitmap in the attached L2 [`engine::Scene`]
//! ([`engine::Scene::collect_unused_bitmaps`]) — the L2 counterpart of the
//! reference's
//! graphic cache.
//!
//! # Host-facts seam (D11)
//!
//! `appDataPath`, `desktopLeft`/`Top`/`Width`/`Height`, `shellExecute` and
//! `system` are genuine host facts with no L2 counterpart. They compose the
//! closure seam installed through [`CompatHost`](crate::CompatHost)
//! ([`set_app_data_path`](crate::CompatHost::set_app_data_path),
//! [`set_desktop_rect`](crate::CompatHost::set_desktop_rect),
//! [`set_shell_execute`](crate::CompatHost::set_shell_execute) and
//! [`set_system_command`](crate::CompatHost::set_system_command)); when the
//! matching fact is unset the member raises a typed error and never returns a
//! silent default.
//!
//! # Command-line arguments
//!
//! `getArgument`/`setArgument` mirror the reference's `TVPProgramArguments`:
//! an ordered vector of `name=value` strings matched by prefix, where the
//! matched entry's next character must be `=` or the end of the string. A
//! value-less entry reads back as `"yes"`. The vector lives in
//! [`SystemState`], not a process global.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_void};
use std::path::Path;
use std::time::Instant;

use tjs::{
    DetachedValue, Engine, NativeClassBuilder, NativeMethodDef, NativePropertyDef, RetainedValue,
    TjsValue, VAL_INTEGER, VAL_REAL, VAL_RETAINED, VAL_STRING, VAL_VOID, Value,
};

use crate::binding;
use crate::host::{host_app_data_path, host_desktop_rect, host_shell_execute, host_system};
use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::input::vk_pressed;
use crate::{CompatCtx, CompatError, CompatServices};

// ---------------------------------------------------------------------------
// Per-engine dispatch state (never a process global)
// ---------------------------------------------------------------------------

/// One registered continuous handler: the retained closure plus its
/// **identity** — the `(Object, ObjThis)` pair the reference's
/// `TVPAddContinuousHandler`/`TVPRemoveContinuousHandler` compare with
/// `std::find`.
struct ContinuousHandler {
    value: DetachedValue,
    key: (usize, usize),
}

/// `System` dispatch state, keyed by type inside
/// [`CompatServices::state`](crate::CompatServices::state).
struct SystemState {
    /// Ordered `name=value` command-line arguments.
    arguments: Vec<String>,
    /// Pending `System.exit`/`System.terminate` code, consumed once.
    exit_request: Option<i32>,
    /// `System.title`.
    title: String,
    /// `System.eventDisabled`.
    event_disabled: bool,
    /// `System.graphicCacheLimit` (stored for compatibility).
    graphic_cache_limit: i64,
    /// `System.exitOnWindowClose`.
    exit_on_window_close: bool,
    /// `System.exitOnNoWindowStartup`.
    exit_on_no_window_startup: bool,
    /// `System.drawThreadNum` (stored for compatibility).
    draw_thread_num: i64,
    /// `System.assignMessage` overrides.
    messages: HashMap<String, String>,
    /// Epoch of the first `getTickCount` call (the reference's
    /// `TVPStartTickCount`).
    tick_epoch: Option<Instant>,
    /// Counter mixed into `createUUID` seeds (keeps the UUID generator
    /// stateless without a process global).
    uuid_counter: u64,
    /// Registered `addContinuousHandler` callbacks. A slot is `None` while
    /// its handler runs or after it was removed.
    continuous: Vec<Option<ContinuousHandler>>,
    /// Identity of the handler currently being delivered, if any.
    current_continuous: Option<(usize, usize)>,
    /// Set when `removeContinuousHandler` targets the running handler.
    remove_current_continuous: bool,
    /// Virtual-key codes already observed by a `getKeyState` query since they
    /// went down (the reference scancode `0x10` "pressed since last query"
    /// bit).
    key_query_seen: HashSet<u32>,
}

impl Default for SystemState {
    fn default() -> Self {
        Self {
            arguments: Vec::new(),
            exit_request: None,
            title: String::new(),
            event_disabled: false,
            graphic_cache_limit: 0,
            // The reference's `EXIT_ON_WINDOW_CLOSE` defaults to true.
            exit_on_window_close: true,
            exit_on_no_window_startup: false,
            draw_thread_num: 0,
            messages: HashMap::new(),
            tick_epoch: None,
            uuid_counter: 0,
            continuous: Vec::new(),
            current_continuous: None,
            remove_current_continuous: false,
            key_query_seen: HashSet::new(),
        }
    }
}

/// Scratch storage for native string results, kept in the per-engine dispatch
/// state so a returned `out.string` stays valid until the C++ trampoline
/// copies it after the callback returns. Owned here rather than in a
/// thread-local so the only process-wide state stays the documented binding.
#[derive(Default)]
struct StringScratch(Vec<u8>);

/// Atomically consume the pending `System.exit`/`System.terminate` request.
///
/// Returns the code recorded by the most recent call and clears it so a
/// request is reported **exactly once**; the host's frame loop polls this and
/// maps it to its own exit path.
// The host's frame loop wires this in when the migrated host lands; until
// then it is only a documented seam.
#[allow(dead_code)]
#[must_use]
pub fn take_exit_request(services: &CompatServices) -> Option<i32> {
    services.state::<SystemState>().exit_request.take()
}

/// Advance every `System.addContinuousHandler` callback once, passing the
/// current tick count (ms) — the reference's `TVPDeliverContinuousEvent`.
///
/// Returns whether any handler remains. A handler that raises a TJS error is
/// removed (as in the reference); a callback may register or remove handlers,
/// and a newly registered handler is delivered in the same pass. The
/// per-engine state borrow is released across each callback so a re-entrant
/// `System` call cannot panic on the `RefCell`.
#[allow(dead_code)]
pub fn continuous_handler_poll(ctx: &mut CompatCtx) -> Result<bool, CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    let services = unsafe { ctx.services() };
    let tick = tick_count_ms(services);
    let mut index = 0usize;
    loop {
        // Root the handler for the call, leaving a tombstone in its slot.
        let taken = {
            let mut state = services.state::<SystemState>();
            if index >= state.continuous.len() {
                break;
            }
            state.continuous[index].take()
        };
        if let Some(handler) = taken {
            {
                let mut state = services.state::<SystemState>();
                state.current_continuous = Some(handler.key);
                state.remove_current_continuous = false;
            }
            let callback = engine.call_detached(&handler.value, &[TjsValue::Integer(tick)]);
            if let Err(e) = &callback {
                log::warn!(
                    "System.addContinuousHandler: handler raised an error and was removed: {e}"
                );
            }
            let callback_ok = callback.is_ok();
            let removed = {
                let mut state = services.state::<SystemState>();
                state.current_continuous = None;
                std::mem::replace(&mut state.remove_current_continuous, false)
            };
            if callback_ok && !removed {
                let mut state = services.state::<SystemState>();
                if index < state.continuous.len() && state.continuous[index].is_none() {
                    state.continuous[index] = Some(handler);
                } else {
                    // The slot was consumed/moved: keep the handler alive at
                    // the end rather than losing it.
                    state.continuous.push(Some(handler));
                }
            }
        }
        index += 1;
    }
    let mut state = services.state::<SystemState>();
    state.continuous.retain(|slot| slot.is_some());
    Ok(!state.continuous.is_empty())
}

// ---------------------------------------------------------------------------
// Marshalling helpers
// ---------------------------------------------------------------------------

/// Resolve the installed engine → context binding for a native callback.
///
/// `None` means the binding is not installed; callers turn that into a typed
/// callback error.
pub(crate) fn resolve(engine: *mut c_void) -> Option<EngineBinding> {
    binding::resolve(engine as *mut Engine).map(EngineBinding)
}

/// Thin wrapper so the rest of the file can name the resolved binding without
/// depending on the (private) `binding` module path in every signature.
pub(crate) struct EngineBinding(crate::binding::Resolved);

impl EngineBinding {
    /// A registration/dispatch context for the bound VM.
    pub(crate) fn ctx(&self) -> CompatCtx {
        self.0.ctx()
    }

    /// The host engine.
    pub(crate) fn engine(&self) -> &'static tjs::Tjs2Engine {
        self.0.engine()
    }
}

/// TJS `operator bool` coercion: integers/reals are non-zero, strings are
/// non-empty, void is false, objects are true.
pub(crate) fn value_as_bool(v: &Value) -> bool {
    match v.ty {
        VAL_INTEGER => v.integer != 0,
        VAL_REAL => v.real != 0.0,
        VAL_STRING => !value_as_string(v).is_empty(),
        VAL_VOID => false,
        _ => true,
    }
}

/// TJS `AsInteger` coercion.
pub(crate) fn value_as_i64(v: &Value) -> i64 {
    match v.ty {
        VAL_INTEGER => v.integer,
        VAL_REAL => v.real as i64,
        VAL_STRING => value_as_string(v).parse().unwrap_or(0),
        _ => 0,
    }
}

/// Write a real result into a native callback's out slot.
fn set_real(out: *mut Value, value: f64) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_REAL;
        (*out).integer = 0;
        (*out).real = value;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Write a string result into a native callback's out slot.
///
/// The bytes live in [`StringScratch`] inside the per-engine services, so the
/// pointer stays valid after this returns until the C++ trampoline copies it.
pub(crate) fn set_string_out(ctx: &CompatCtx, out: *mut Value, s: &str) {
    if out.is_null() {
        return;
    }
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let mut scratch = services.state::<StringScratch>();
    scratch.0.clear();
    scratch.0.extend_from_slice(s.as_bytes());
    scratch.0.push(0);
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_STRING;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = scratch.0.as_ptr() as *const c_char;
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Hand a retained evaluation result to the C++ side. Scalars are copied;
/// object results keep their retention (consumed by the return slot).
fn set_retained_value_out(ctx: &CompatCtx, out: *mut Value, value: RetainedValue) {
    match value {
        RetainedValue::Value(TjsValue::Void) => set_void(out),
        RetainedValue::Value(TjsValue::Integer(i)) => set_int(out, i),
        RetainedValue::Value(TjsValue::Real(r)) => set_real(out, r),
        RetainedValue::Value(TjsValue::String(s)) => set_string_out(ctx, out, &s),
        RetainedValue::Value(TjsValue::Object | TjsValue::Retained(_)) => set_void(out),
        RetainedValue::Object(dv) => {
            if !out.is_null() {
                // SAFETY: the trampoline passes a valid out slot for the call.
                unsafe {
                    (*out).ty = VAL_RETAINED;
                    (*out).integer = 0;
                    (*out).real = 0.0;
                    (*out).string = std::ptr::null();
                    (*out).array = std::ptr::null();
                    (*out).array_count = 0;
                    (*out).retained = dv.raw_id() as usize;
                }
            }
            // The C++ side consumes the retention while copying the result.
            std::mem::forget(dv);
        }
    }
}

/// Milliseconds since the first call (the reference's
/// `TVPStartTickCount`/`TVPGetTickCount`).
fn tick_count_ms(services: &CompatServices) -> i64 {
    let mut state = services.state::<SystemState>();
    let epoch = *state.tick_epoch.get_or_insert_with(Instant::now);
    epoch.elapsed().as_millis() as i64
}

// ---------------------------------------------------------------------------
// Command-line arguments
// ---------------------------------------------------------------------------

/// Reference-prefix lookup: `name` matches an entry whose next character after
/// the prefix is `=` (value) or end-of-string (`"yes"`).
fn get_command_line(services: &CompatServices, name: &str) -> Option<String> {
    let state = services.state::<SystemState>();
    for entry in state.arguments.iter() {
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
fn set_command_line(services: &CompatServices, name: &str, value: &str) {
    let mut state = services.state::<SystemState>();
    let new_entry = format!("{name}={value}");
    for entry in state.arguments.iter_mut() {
        if let Some(rest) = entry.strip_prefix(name)
            && (rest.is_empty() || rest.starts_with('='))
        {
            *entry = new_entry;
            return;
        }
    }
    state.arguments.insert(0, new_entry);
}

// ---------------------------------------------------------------------------
// L2 service helpers (Storage / Scene)
// ---------------------------------------------------------------------------

/// The game directory of the attached L2 storage, if any.
fn project_dir(ctx: &CompatCtx) -> Option<std::path::PathBuf> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine, and the
    // host keeps the attached storage alive for the binding's lifetime.
    let ptr = unsafe { ctx.services() }.storage()?;
    Some(unsafe { &*ptr }.game_dir().to_path_buf())
}

/// Append a trailing `/` the way the reference's `TVPNativeProjectDir` does.
fn dir_with_separator(path: &Path) -> String {
    let s = path.display().to_string();
    if s.ends_with('/') { s } else { format!("{s}/") }
}

/// The logical screen size: the first window of the attached L2 scene, or
/// `(0, 0)` when no window exists yet (the reference's unset default).
fn screen_size(ctx: &CompatCtx) -> (u32, u32) {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine, and the
    // host keeps the attached scene alive for the binding's lifetime.
    let Some(ptr) = (unsafe { ctx.services() }).scene() else {
        return (0, 0);
    };
    let scene = unsafe { &*ptr };
    scene
        .windows
        .first()
        .map(|window| window.inner_size)
        .unwrap_or((0, 0))
}

// ---------------------------------------------------------------------------
// System methods
// ---------------------------------------------------------------------------

/// `System.inform(text [, caption [, buttons]])`
///
/// The reference shows a modal message box; krkr-rs has no GUI, so the text is
/// logged at `info!`. With a `buttons` argument it returns `0` (the
/// left/first button, the "OK clicked" path); otherwise it returns void.
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
    let a = unsafe { args(argv, argc) };
    let text = value_as_string(&a[0]);
    let caption = if argc >= 2 && a[1].ty != VAL_VOID {
        value_as_string(&a[1])
    } else {
        "Information".to_string()
    };
    log::info!("System.inform [{caption}]: {text}");
    if argc >= 3 && a[2].ty != VAL_VOID {
        set_int(out, 0);
    } else {
        set_void(out);
    }
    0
}

/// `System.getTickCount()` → milliseconds since the first call.
extern "C" fn native_get_tick_count(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.getTickCount: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    set_int(out, tick_count_ms(unsafe { ctx.services() }));
    0
}

/// `System.getArgument(name [, default])`
///
/// Returns the stored value, the caller's default (type-preserving) when the
/// name is missing, or void when no default was given.
extern "C" fn native_get_argument(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.getArgument requires 1 argument");
    }
    let a = unsafe { args(argv, argc) };
    let name = value_as_string(&a[0]);
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.getArgument: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    match get_command_line(unsafe { ctx.services() }, &name) {
        Some(value) => {
            set_string_out(&ctx, out, &value);
            0
        }
        None if argc >= 2 => match a[1].ty {
            VAL_STRING => {
                set_string_out(&ctx, out, &value_as_string(&a[1]));
                0
            }
            VAL_INTEGER => {
                set_int(out, a[1].integer);
                0
            }
            VAL_REAL => {
                set_real(out, a[1].real);
                0
            }
            VAL_VOID => {
                set_void(out);
                0
            }
            _ => report_error(
                out_error,
                "System.getArgument: object default value is not supported",
            ),
        },
        None => {
            set_void(out);
            0
        }
    }
}

/// `System.setArgument(name, value)`
extern "C" fn native_set_argument(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "System.setArgument requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let name = value_as_string(&a[0]);
    let value = value_as_string(&a[1]);
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.setArgument: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    set_command_line(unsafe { ctx.services() }, &name, &value);
    set_void(out);
    0
}

/// `System.createAppLock(lockname)` → bool
///
/// A single-process emulator always wins the lock; returns true.
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
    set_bool(out, true);
    0
}

/// Record the exit request shared by `System.exit`/`System.terminate`.
fn record_exit(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
) -> Option<String> {
    let code = if argc >= 1 {
        value_as_i64(&unsafe { args(argv, argc) }[0])
    } else {
        0
    };
    let Some(resolved) = resolve(engine) else {
        return Some("compat context is not bound".to_string());
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<SystemState>()
        .exit_request = Some(code as i32);
    set_void(out);
    None
}

/// `System.exit(code)` — record the exit code for the host to consume.
extern "C" fn native_exit(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    match record_exit(engine, argc, argv, out) {
        Some(e) => report_error(out_error, &format!("System.exit: {e}")),
        None => {
            log::info!("System.exit: emulator termination requested");
            0
        }
    }
}

/// `System.terminate([code])` — record the exit code for the host to consume.
extern "C" fn native_terminate(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    match record_exit(engine, argc, argv, out) {
        Some(e) => report_error(out_error, &format!("System.terminate: {e}")),
        None => {
            log::info!("System.terminate: emulator termination requested");
            0
        }
    }
}

/// `System.dumpHeap()` → void (a no-op in the reference too).
extern "C" fn native_dump_heap(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_void(out);
    0
}

/// `System.nullpo()` → void
///
/// The reference deliberately crashes; krkr-rs logs an error and continues.
extern "C" fn native_nullpo(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    log::error!(
        "System.nullpo() called: the reference implementation crashes the process; \
         krkr-rs logs instead and continues"
    );
    set_void(out);
    0
}

/// `System.showVersion()` → void
extern "C" fn native_show_version(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    log::info!("KiriKiri (krkr-rs) {}", env!("CARGO_PKG_VERSION"));
    set_void(out);
    0
}

/// `System.inputString(caption, prompt, initial)` → string
///
/// The reference shows a modal input box; the headless path logs the prompt
/// and returns the initial value (the "OK" path). Requires three arguments.
extern "C" fn native_input_string(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 3 {
        return report_error(out_error, "System.inputString requires 3 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let caption = value_as_string(&a[0]);
    let prompt = value_as_string(&a[1]);
    let initial = value_as_string(&a[2]);
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.inputString: compat context is not bound");
    };
    let ctx = resolved.ctx();
    log::info!("System.inputString [{caption}]: {prompt} (returning initial value)");
    set_string_out(&ctx, out, &initial);
    0
}

/// `System.toActualColor(color)` → int
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
    let color = value_as_i64(&unsafe { args(argv, argc) }[0]) as u32;
    let actual = if color & 0xff00_0000 != 0 {
        color_to_actual(color)
    } else {
        color & 0x00ff_ffff
    };
    set_int(out, i64::from(actual));
    0
}

/// `System.createUUID()` → string (RFC-4122 v4)
extern "C" fn native_create_uuid(
    engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc != 0 {
        return report_error(out_error, "System.createUUID takes no arguments");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.createUUID: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let uuid = {
        let mut state = unsafe { ctx.services() }.state::<SystemState>();
        make_uuid(&mut state)
    };
    set_string_out(&ctx, out, &uuid);
    0
}

/// `System.assignMessage(id, message)` → bool
extern "C" fn native_assign_message(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "System.assignMessage requires 2 arguments");
    }
    let a = unsafe { args(argv, argc) };
    let id = value_as_string(&a[0]);
    let message = value_as_string(&a[1]);
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.assignMessage: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<SystemState>()
        .messages
        .insert(id, message);
    set_bool(out, true);
    0
}

/// `System.touchImages(storages[, limit[, timeout]])` → void
///
/// The reference pre-caches images; krkr-rs caches per layer on demand, so
/// this only validates the argument count and does nothing.
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
    set_void(out);
    0
}

/// `System.doCompact([level])` → void
///
/// The reference defaults `level` to `TVP_COMPACT_LEVEL_MAX` (100) and runs
/// the TJS garbage collector from `clIdle` (5) up. The archive/graphic cache
/// clearing at the higher thresholds needs services that have not landed; the
/// GC half is implemented here.
extern "C" fn native_do_compact(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let level = unsafe { args(argv, argc) }
        .first()
        .map(value_as_i64)
        .unwrap_or(100);
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.doCompact: compat context is not bound");
    };
    if level >= 5
        && let Err(e) = resolved.engine().do_gc()
    {
        log::warn!("System.doCompact: TJS garbage collection failed: {e}");
    }
    set_void(out);
    0
}

/// `System.readRegValue(key)` → value
///
/// The reference's active (non-Windows) path loads a `RegisterData.tjs`
/// expression and walks it by the `/`- or `\`-separated key. The port resolves
/// `RegisterData.tjs` through the attached L2 storage and evaluates the walk
/// in-script, so a miss yields void.
extern "C" fn native_read_reg_value(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.readRegValue requires 1 argument");
    }
    let key = value_as_string(&unsafe { args(argv, argc) }[0]);
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.readRegValue: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    match read_register_value(&ctx, &key) {
        Ok(Some(value)) => {
            set_retained_value_out(&ctx, out, value);
            0
        }
        Ok(None) => {
            set_void(out);
            0
        }
        Err(e) => report_error(out_error, &format!("System.readRegValue: {e}")),
    }
}

/// `System.addContinuousHandler(fn)` — register a per-frame callback.
extern "C" fn native_add_continuous_handler(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(out_error, "System.addContinuousHandler requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.addContinuousHandler: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    let value = match resolved.engine().retain_object_arg(&a[0]) {
        Ok(value) => value,
        Err(e) => {
            return report_error(
                out_error,
                &format!("System.addContinuousHandler: cannot retain the handler: {e}"),
            );
        }
    };
    let key = (
        a[0].object_handle() as usize,
        a[0].object_objthis() as usize,
    );
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<SystemState>();
    // The reference ignores a handler that is already registered (identity).
    if state.continuous.iter().flatten().any(|h| h.key == key) {
        set_void(out);
        return 0;
    }
    state
        .continuous
        .push(Some(ContinuousHandler { value, key }));
    drop(state);
    set_void(out);
    0
}

/// `System.removeContinuousHandler(fn)` — unregister a per-frame callback.
extern "C" fn native_remove_continuous_handler(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(
            out_error,
            "System.removeContinuousHandler requires 1 argument",
        );
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.removeContinuousHandler: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    let key = (
        a[0].object_handle() as usize,
        a[0].object_objthis() as usize,
    );
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<SystemState>();
    if state.current_continuous == Some(key) {
        // The running handler's slot is a tombstone; retire it after return.
        state.remove_current_continuous = true;
    } else {
        for slot in state.continuous.iter_mut() {
            if slot.as_ref().is_some_and(|h| h.key == key) {
                *slot = None;
            }
        }
    }
    drop(state);
    set_void(out);
    0
}

/// Reference `tTVPTouchDevice` capability bits (`ScriptMgnIntf.cpp`).
const TD_INTEGRATED_TOUCH: u32 = 0x0000_0001;
const TD_MULTI_INPUT: u32 = 0x0000_0040;
const TD_DIGITIZER_READY: u32 = 0x0000_0080;
const TD_MOUSE: u32 = 0x0000_0100;
const TD_MOUSE_WHEEL: u32 = 0x0000_0200;

/// Release every bitmap the attached L2 scene no longer references (the
/// reference `TVPClearGraphicCache`). Returns the number freed; a missing
/// scene has no cache and frees nothing.
fn clear_graphic_cache(services: &CompatServices) -> usize {
    let Some(ptr) = services.scene() else {
        return 0;
    };
    // SAFETY: the host keeps the attached scene alive for the binding's
    // lifetime, and the VM is single-threaded.
    unsafe { &mut *ptr }.collect_unused_bitmaps()
}

/// `System.getKeyState(code [, getcurrent])` → int
///
/// Reads the attached L2 [`engine::input::InputState`] through the same
/// virtual-key mapping the `Key` class uses (mouse-button VKs and the
/// `VK_PADANY` pseudo-code included). With `getcurrent` true (the default) it
/// reports whether the key is currently held; with false it reports whether
/// the key went down since the previous query for the same code. Both forms
/// consume the pending edge, mirroring the reference scancode bit handling.
extern "C" fn native_get_key_state(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.getKeyState requires 1 argument");
    }
    let a = unsafe { args(argv, argc) };
    let code = value_as_i64(&a[0]) as u32;
    let getcurrent = if argc >= 2 {
        value_as_bool(&a[1])
    } else {
        true
    };
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.getKeyState: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let Some(input_ptr) = services.input() else {
        return report_error(out_error, "System.getKeyState: input state is not attached");
    };
    // SAFETY: `attach_input` guarantees the pointer stays valid while the
    // binding is installed, and the VM is single-threaded.
    let input = unsafe { &*input_ptr };
    let held = vk_pressed(input, code);
    let newly = {
        let mut state = services.state::<SystemState>();
        if held {
            state.key_query_seen.insert(code)
        } else {
            state.key_query_seen.remove(&code);
            false
        }
    };
    set_bool(out, if getcurrent { held } else { newly });
    0
}

/// `System.shellExecute(target [, param])` → bool
///
/// Delegates to the host's platform shell. Without the installed seam the
/// member raises a typed error (D11).
extern "C" fn native_shell_execute(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.shellExecute requires 1 argument");
    }
    let a = unsafe { args(argv, argc) };
    let target = value_as_string(&a[0]);
    let param = if argc >= 2 {
        value_as_string(&a[1])
    } else {
        String::new()
    };
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.shellExecute: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    match host_shell_execute(services, &target, &param) {
        Some(Ok(ok)) => {
            set_bool(out, ok);
            0
        }
        Some(Err(error)) => report_error(out_error, &format!("System.shellExecute: {error}")),
        None => report_error(
            out_error,
            "System.shellExecute: the host shell seam is not installed",
        ),
    }
}

/// `System.system(target)` → int
///
/// The reference delivers a maximum-level compact event (which clears the
/// graphic cache) and then returns the (commented-out) shell status. This port
/// releases the L2 scene's unreferenced bitmaps and delegates the command to
/// the host seam; without the seam it raises a typed error (D11).
extern "C" fn native_system(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.system requires 1 argument");
    }
    let target = value_as_string(&unsafe { args(argv, argc) }[0]);
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.system: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    match host_system(services, &target) {
        Some(Ok(code)) => {
            clear_graphic_cache(services);
            set_int(out, i64::from(code));
            0
        }
        Some(Err(error)) => report_error(out_error, &format!("System.system: {error}")),
        None => report_error(
            out_error,
            "System.system: the host command seam is not installed",
        ),
    }
}

/// `System.clearGraphicCache()` → void
///
/// Releases every bitmap the attached L2 [`engine::Scene`] no longer
/// references — the L2 counterpart of the reference's `TVPClearGraphicCache`.
/// With no scene attached there is no cache, so the call is a no-op.
extern "C" fn native_clear_graphic_cache(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.clearGraphicCache: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let freed = clear_graphic_cache(services);
    log::debug!("System.clearGraphicCache: released {freed} unused bitmap(s)");
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// System properties
// ---------------------------------------------------------------------------

extern "C" fn prop_exe_path(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.exePath: compat context is not bound");
    };
    let ctx = resolved.ctx();
    match project_dir(&ctx) {
        Some(dir) => {
            set_string_out(&ctx, out, &dir_with_separator(&dir));
            0
        }
        None => report_error(out_error, "System.exePath: storage is not attached"),
    }
}

extern "C" fn prop_data_path(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.dataPath: compat context is not bound");
    };
    let ctx = resolved.ctx();
    match project_dir(&ctx) {
        Some(dir) => {
            set_string_out(&ctx, out, &dir_with_separator(&dir.join("savedata")));
            0
        }
        None => report_error(out_error, "System.dataPath: storage is not attached"),
    }
}

/// `System.personalPath` / `System.savedGamesPath`: the `savedata/` folder
/// next to the game.
fn saved_games_path_prop(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    name: &str,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            &format!("System.{name}: compat context is not bound"),
        );
    };
    let ctx = resolved.ctx();
    match project_dir(&ctx) {
        Some(dir) => {
            set_string_out(&ctx, out, &dir_with_separator(&dir.join("savedata")));
            0
        }
        None => report_error(
            out_error,
            &format!("System.{name}: storage is not attached"),
        ),
    }
}

extern "C" fn prop_personal_path(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    saved_games_path_prop(engine, out, out_error, "personalPath")
}

extern "C" fn prop_saved_games_path(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    saved_games_path_prop(engine, out, out_error, "savedGamesPath")
}

extern "C" fn prop_exe_name(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.exeName: compat context is not bound");
    };
    let ctx = resolved.ctx();
    set_string_out(&ctx, out, "krkr-rs");
    0
}

extern "C" fn prop_title_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.title: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let title = unsafe { ctx.services() }
        .state::<SystemState>()
        .title
        .clone();
    let title = if title.is_empty() {
        "krkr-rs".to_string()
    } else {
        title
    };
    set_string_out(&ctx, out, &title);
    0
}

extern "C" fn prop_title_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.title: compat context is not bound");
    };
    if value.is_null() {
        return report_error(out_error, "System.title: missing value");
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let title = value_as_string(unsafe { &*value });
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }.state::<SystemState>().title = title;
    0
}

extern "C" fn prop_event_disabled_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.eventDisabled: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let flag = unsafe { ctx.services() }
        .state::<SystemState>()
        .event_disabled;
    set_bool(out, flag);
    0
}

extern "C" fn prop_event_disabled_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.eventDisabled: compat context is not bound",
        );
    };
    if value.is_null() {
        return report_error(out_error, "System.eventDisabled: missing value");
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let flag = value_as_bool(unsafe { &*value });
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<SystemState>()
        .event_disabled = flag;
    0
}

extern "C" fn prop_screen_width(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.screenWidth: compat context is not bound");
    };
    let ctx = resolved.ctx();
    let (width, _) = screen_size(&ctx);
    set_int(out, i64::from(width));
    0
}

extern "C" fn prop_screen_height(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.screenHeight: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    let (_, height) = screen_size(&ctx);
    set_int(out, i64::from(height));
    0
}

extern "C" fn prop_version_string(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.versionString: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    set_string_out(&ctx, out, &format!("{}.0", env!("CARGO_PKG_VERSION")));
    0
}

extern "C" fn prop_version_information(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.versionInformation: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    set_string_out(
        &ctx,
        out,
        &format!(
            "krkr-rs {} (KiriKiri2/TVP native port)",
            env!("CARGO_PKG_VERSION")
        ),
    );
    0
}

/// The reference `TVPGetPlatformName` / `TVPGetOSName` value for this host.
fn platform_name() -> &'static str {
    match std::env::consts::OS {
        "windows" => "Win32",
        "macos" => "MacOS",
        "android" => "Android",
        "ios" => "iPhone",
        _ => "Linux",
    }
}

extern "C" fn prop_platform_name(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.platformName: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    set_string_out(&ctx, out, platform_name());
    0
}

extern "C" fn prop_os_name(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // In this fork `TVPGetOSName` returns `TVPGetPlatformName`.
    prop_platform_name(engine, out, out_error)
}

extern "C" fn prop_processor_num(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.processorNum: compat context is not bound",
        );
    };
    let _ = resolved;
    let n = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1);
    set_int(out, n as i64);
    0
}

extern "C" fn prop_exe_bits(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(_resolved) = resolve(engine) else {
        return report_error(out_error, "System.exeBits: compat context is not bound");
    };
    set_int(out, (std::mem::size_of::<usize>() * 8) as i64);
    0
}

extern "C" fn prop_os_bits(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(_resolved) = resolve(engine) else {
        return report_error(out_error, "System.osBits: compat context is not bound");
    };
    set_int(out, (std::mem::size_of::<usize>() * 8) as i64);
    0
}

extern "C" fn prop_graphic_cache_limit_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.graphicCacheLimit: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let value = unsafe { ctx.services() }
        .state::<SystemState>()
        .graphic_cache_limit;
    set_int(out, value);
    0
}

extern "C" fn prop_graphic_cache_limit_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.graphicCacheLimit: compat context is not bound",
        );
    };
    if value.is_null() {
        return report_error(out_error, "System.graphicCacheLimit: missing value");
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let limit = value_as_i64(unsafe { &*value });
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<SystemState>()
        .graphic_cache_limit = limit;
    0
}

extern "C" fn prop_exit_on_window_close_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.exitOnWindowClose: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let flag = unsafe { ctx.services() }
        .state::<SystemState>()
        .exit_on_window_close;
    set_bool(out, flag);
    0
}

extern "C" fn prop_exit_on_window_close_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.exitOnWindowClose: compat context is not bound",
        );
    };
    if value.is_null() {
        return report_error(out_error, "System.exitOnWindowClose: missing value");
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let flag = value_as_bool(unsafe { &*value });
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<SystemState>()
        .exit_on_window_close = flag;
    0
}

extern "C" fn prop_exit_on_no_window_startup_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.exitOnNoWindowStartup: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let flag = unsafe { ctx.services() }
        .state::<SystemState>()
        .exit_on_no_window_startup;
    set_bool(out, flag);
    0
}

extern "C" fn prop_exit_on_no_window_startup_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.exitOnNoWindowStartup: compat context is not bound",
        );
    };
    if value.is_null() {
        return report_error(out_error, "System.exitOnNoWindowStartup: missing value");
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let flag = value_as_bool(unsafe { &*value });
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<SystemState>()
        .exit_on_no_window_startup = flag;
    0
}

extern "C" fn prop_draw_thread_num_get(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.drawThreadNum: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let value = unsafe { ctx.services() }
        .state::<SystemState>()
        .draw_thread_num;
    set_int(out, value);
    0
}

extern "C" fn prop_draw_thread_num_set(
    engine: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.drawThreadNum: compat context is not bound",
        );
    };
    if value.is_null() {
        return report_error(out_error, "System.drawThreadNum: missing value");
    }
    // SAFETY: the trampoline passes a valid value slot for the call.
    let n = value_as_i64(unsafe { &*value });
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<SystemState>()
        .draw_thread_num = n;
    0
}

/// Report a `System` string host fact, raising a typed error when the seam is
/// unset or the provider fails.
fn host_string_property(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    member: &str,
    read: impl FnOnce(&CompatServices) -> Option<Result<String, String>>,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            &format!("System.{member}: compat context is not bound"),
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    match read(services) {
        Some(Ok(value)) => {
            set_string_out(&ctx, out, &value);
            0
        }
        Some(Err(error)) => report_error(out_error, &format!("System.{member}: {error}")),
        None => report_error(
            out_error,
            &format!("System.{member}: the host fact is not installed"),
        ),
    }
}

/// Report one field of the `System.desktop*` host rectangle.
fn host_desktop_property(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    member: &str,
    pick: impl FnOnce((i32, i32, u32, u32)) -> i64,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            &format!("System.{member}: compat context is not bound"),
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    match host_desktop_rect(services) {
        Some(Ok(rect)) => {
            set_int(out, pick(rect));
            0
        }
        Some(Err(error)) => report_error(out_error, &format!("System.{member}: {error}")),
        None => report_error(
            out_error,
            &format!("System.{member}: the host fact is not installed"),
        ),
    }
}

extern "C" fn prop_app_data_path(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    host_string_property(engine, out, out_error, "appDataPath", host_app_data_path)
}

extern "C" fn prop_desktop_left(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    host_desktop_property(engine, out, out_error, "desktopLeft", |rect| {
        i64::from(rect.0)
    })
}

extern "C" fn prop_desktop_top(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    host_desktop_property(engine, out, out_error, "desktopTop", |rect| {
        i64::from(rect.1)
    })
}

extern "C" fn prop_desktop_width(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    host_desktop_property(engine, out, out_error, "desktopWidth", |rect| {
        i64::from(rect.2)
    })
}

extern "C" fn prop_desktop_height(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    host_desktop_property(engine, out, out_error, "desktopHeight", |rect| {
        i64::from(rect.3)
    })
}

/// `System.touchDevice` → the reference `tTVPTouchDevice` bitfield.
///
/// The base `tdMouse | tdMouseWheel` capability matches this fork's active
/// reference path; the touch/pen bits are added while the attached L2 input
/// state has an active touch contact (the only touch signal the generic input
/// state carries). Without the L2 input state the member raises a typed error.
extern "C" fn prop_touch_device(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.touchDevice: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let Some(input_ptr) = services.input() else {
        return report_error(out_error, "System.touchDevice: input state is not attached");
    };
    // SAFETY: `attach_input` guarantees the pointer stays valid while the
    // binding is installed, and the VM is single-threaded.
    let input = unsafe { &*input_ptr };
    let mut flags = TD_MOUSE | TD_MOUSE_WHEEL;
    if input.touch_point_count() > 0 {
        flags |= TD_INTEGRATED_TOUCH | TD_MULTI_INPUT | TD_DIGITIZER_READY;
    }
    set_int(out, i64::from(flags));
    0
}

// ---------------------------------------------------------------------------
// Reference algorithms (colors, UUID, RegisterData traversal)
// ---------------------------------------------------------------------------

/// Convert a TVP system color (high byte set) to its actual `0xRRGGBB` value.
/// Ported from `ColorToRGB` + the byte-order swap in `TVPToActualColor`.
fn color_to_actual(color: u32) -> u32 {
    // `ColorToRGB`'s table is in `0xBBGGRR`; the swap below returns `0xRRGGBB`.
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

/// Generate a random RFC-4122 v4 UUID string. The seed mixes the clock, a
/// per-engine counter and the stack address; there is no process global.
fn make_uuid(state: &mut SystemState) -> String {
    let n = state.uuid_counter;
    state.uuid_counter = state.uuid_counter.wrapping_add(1);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let addr = &n as *const u64 as u64;
    let mut state_bits = nanos ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ addr;
    if state_bits == 0 {
        state_bits = 0x1234_5678_9abc_def0;
    }
    let mut next = || {
        state_bits ^= state_bits >> 12;
        state_bits ^= state_bits << 25;
        state_bits ^= state_bits >> 27;
        state_bits.wrapping_mul(0x2545_f491_4f6c_dd1d)
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

/// Decode a `RegisterData.tjs` byte buffer: UTF-8 BOM and UTF-16 (LE/BE) BOM
/// first, then UTF-8 (lossily).
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
    String::from_utf8_lossy(raw).into_owned()
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

/// Evaluate `RegisterData.tjs` through the attached L2 storage and walk `key`.
///
/// Returns `Ok(None)` for an empty key, a missing storage/file or any
/// traversal miss (the reference's `result->Clear()`), and `Err` only when
/// the engine itself could not be driven.
fn read_register_value(ctx: &CompatCtx, key: &str) -> Result<Option<RetainedValue>, String> {
    if key.is_empty() {
        return Ok(None);
    }
    let raw = {
        // SAFETY: a `CompatCtx` is only created for a bound, live engine, and
        // the host keeps the attached storage alive for the binding lifetime.
        let Some(ptr) = (unsafe { ctx.services() }).storage() else {
            return Ok(None);
        };
        match unsafe { &*ptr }.read("RegisterData.tjs") {
            Ok(bytes) => bytes,
            // Missing file / unreadable: the reference clears the result.
            Err(_) => return Ok(None),
        }
    };
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
    // Traverse in-script so missing members yield void instead of an error.
    let script = format!(
        "(function(){{var __rd=({expression});var __o=__rd;var __k={keys};\
         for(var __i=0;__i<__k.count;++__i){{\
         if(__o===void||__o===null)return void;__o=__o[__k[__i]];}}\
         return __o;}})()"
    );
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    match engine.eval_retained(&script, "System.readRegValue") {
        Ok(value) => Ok(Some(value)),
        Err(e) => {
            log::debug!("System.readRegValue({key:?}): {e}");
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `System` native class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_class(&NativeClassBuilder {
        name: "System",
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
                name: "createUUID",
                f: native_create_uuid,
            },
            NativeMethodDef {
                name: "assignMessage",
                f: native_assign_message,
            },
            NativeMethodDef {
                name: "doCompact",
                f: native_do_compact,
            },
            NativeMethodDef {
                name: "readRegValue",
                f: native_read_reg_value,
            },
            NativeMethodDef {
                name: "touchImages",
                f: native_touch_images,
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
                name: "clearGraphicCache",
                f: native_clear_graphic_cache,
            },
        ],
        properties: vec![
            NativePropertyDef {
                name: "exePath",
                get: Some(prop_exe_path),
                set: None,
            },
            NativePropertyDef {
                name: "dataPath",
                get: Some(prop_data_path),
                set: None,
            },
            NativePropertyDef {
                name: "personalPath",
                get: Some(prop_personal_path),
                set: None,
            },
            NativePropertyDef {
                name: "savedGamesPath",
                get: Some(prop_saved_games_path),
                set: None,
            },
            NativePropertyDef {
                name: "exeName",
                get: Some(prop_exe_name),
                set: None,
            },
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
            NativePropertyDef {
                name: "screenWidth",
                get: Some(prop_screen_width),
                set: None,
            },
            NativePropertyDef {
                name: "screenHeight",
                get: Some(prop_screen_height),
                set: None,
            },
            NativePropertyDef {
                name: "versionString",
                get: Some(prop_version_string),
                set: None,
            },
            NativePropertyDef {
                name: "versionInformation",
                get: Some(prop_version_information),
                set: None,
            },
            NativePropertyDef {
                name: "platformName",
                get: Some(prop_platform_name),
                set: None,
            },
            NativePropertyDef {
                name: "osName",
                get: Some(prop_os_name),
                set: None,
            },
            NativePropertyDef {
                name: "processorNum",
                get: Some(prop_processor_num),
                set: None,
            },
            NativePropertyDef {
                name: "exeBits",
                get: Some(prop_exe_bits),
                set: None,
            },
            NativePropertyDef {
                name: "osBits",
                get: Some(prop_os_bits),
                set: None,
            },
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
            NativePropertyDef {
                name: "appDataPath",
                get: Some(prop_app_data_path),
                set: None,
            },
            NativePropertyDef {
                name: "desktopLeft",
                get: Some(prop_desktop_left),
                set: None,
            },
            NativePropertyDef {
                name: "desktopTop",
                get: Some(prop_desktop_top),
                set: None,
            },
            NativePropertyDef {
                name: "desktopWidth",
                get: Some(prop_desktop_width),
                set: None,
            },
            NativePropertyDef {
                name: "desktopHeight",
                get: Some(prop_desktop_height),
                set: None,
            },
            NativePropertyDef {
                name: "touchDevice",
                get: Some(prop_touch_device),
                set: None,
            },
        ],
    })
}
