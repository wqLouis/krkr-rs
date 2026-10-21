//! `System` native class.
//!
//! `System` is the process and OS fact class. It composes the L2
//! `engine_api::Storage` and `engine_api::Scene` services, the `tjs` VM and the
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
//! `getKeyState` reads the attached L2 `engine_api::InputState`
//! (TVP virtual-key codes and pad/`VK_PADANY` semantics are the same as the
//! `Key` class); `touchDevice` derives its touch-capability bits from the
//! active contacts in the same L2 state; `clearGraphicCache` clears the
//! attached L2 `engine_api::Scene`'s decoded-image cache and releases every
//! bitmap it no longer references
//! (`engine_api::Scene::clear_decoded_image_cache`,
//! `engine_api::Scene::collect_unused_bitmaps`) — the L2 counterpart of the
//! reference's graphic cache.
//!
//! # Host-facts seam
//!
//! `desktopLeft`/`Top`/`Width`/`Height` are genuine host facts with no L2
//! counterpart. They compose the closure seam installed through
//! [`CompatHost`](crate::CompatHost)
//! ([`set_desktop_rect`](crate::CompatHost::set_desktop_rect)); when the fact
//! is unset the member raises a typed error and never returns a silent
//! default. `appDataPath` is not a host fact: the reference's
//! `TVPGetAppDataPath()` returns `TVPGetAppPath()`
//! (`reference/cpp/core/base/impl/SystemImpl.cpp:366-380`, `:819`), so it
//! reuses the application-path helper alongside `exePath`. `shellExecute` and
//! `system` match the reference's inert values instead of consulting a host
//! seam.
//!
//! # Command-line arguments
//!
//! `getArgument`/`setArgument` mirror the reference's `TVPProgramArguments`:
//! an ordered vector of `name=value` strings matched by prefix, where the
//! matched entry's next character must be `=` or the end of the string. A
//! value-less entry reads back as `"yes"`. The vector lives in
//! [`SystemState`], not a process global.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::path::Path;
use std::time::Instant;

use tjs::{
    DetachedValue, Engine, NativeClassBuilder, NativeMethodDef, NativePropertyDef, RetainedValue,
    TjsValue, VAL_ARRAY, VAL_INTEGER, VAL_NULL, VAL_OBJECT, VAL_OCTET, VAL_REAL, VAL_RETAINED,
    VAL_STRING, VAL_VOID, Value,
};

use crate::binding;
use crate::collections::Set;
use crate::host::host_desktop_rect;
use crate::native_util::{
    args, host_ref, report_error, services, set_bool, set_int, set_void, value_as_string,
};
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
    /// `System.exitOnWindowClose`.
    exit_on_window_close: bool,
    /// `System.exitOnNoWindowStartup`.
    exit_on_no_window_startup: bool,
    /// `System.drawThreadNum` (stored for compatibility).
    draw_thread_num: i64,
    /// Epoch of the first `getTickCount` call (the reference's
    /// `TVPStartTickCount`).
    tick_epoch: Option<Instant>,
    /// Rough tick of the last automatic compact, or `None` before the first
    /// check (the reference `LastCompactedTick`,
    /// `reference/cpp/core/environ/impl/SystemControl.cpp:52`).
    last_compacted_ms: Option<i64>,
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
    key_query_seen: Set<u32>,
}

impl Default for SystemState {
    fn default() -> Self {
        Self {
            arguments: Vec::new(),
            exit_request: None,
            title: String::new(),
            event_disabled: false,
            // The reference's `EXIT_ON_WINDOW_CLOSE` defaults to true.
            exit_on_window_close: true,
            // The reference's `TVPTerminateOnNoWindowStartup` defaults to true
            // (`SysInitImpl.cpp:383`).
            exit_on_no_window_startup: true,
            draw_thread_num: 0,
            tick_epoch: None,
            last_compacted_ms: None,
            uuid_counter: 0,
            continuous: Vec::new(),
            current_continuous: None,
            remove_current_continuous: false,
            key_query_seen: Set::default(),
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
pub fn continuous_handler_poll(ctx: &mut CompatCtx) -> Result<bool, CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    let services = services(ctx);
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

/// The idle interval after which the frame body runs the TJS collector.
///
/// The reference `SystemWatchTimerTimer` delivers `TVP_COMPACT_LEVEL_IDLE`
/// once the rough tick is more than 4000 ms past the last compact
/// (`reference/cpp/core/environ/impl/SystemControl.cpp:174-179`), and that
/// callback runs `tTJS::DoGarbageCollection`
/// (`reference/cpp/core/base/ScriptMgnIntf.cpp:377-386`).
const COMPACT_INTERVAL_MS: i64 = 4000;

/// Run the TJS collector when the idle interval has elapsed.
///
/// This is the automatic half of `System.doCompact`: the reference's idle
/// check (`SystemControl.cpp:174-179`) is not driven from a script, and
/// without it a long run never compacts the VM at all. Returns whether the
/// collector ran this frame. On the first call the interval starts, matching
/// the reference's `LastCompactedTick = 0` initial state, so the first pass is
/// one interval after the rough clock's epoch.
///
/// The collector compacts the variant stack and the string heap. The
/// vendored VM leaves `TJSVariantArrayStackCompactNow` empty, so
/// `tTJS::DoGarbageCollection` is paired with an explicit stack compact on
/// the FFI side (see `tjs2_do_gc`).
pub fn compact_poll(ctx: &mut CompatCtx) -> bool {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    // SAFETY: see above.
    let services = unsafe { ctx.services() };
    let due = {
        let mut state = services.state::<SystemState>();
        let epoch = *state.tick_epoch.get_or_insert_with(Instant::now);
        let tick = epoch.elapsed().as_millis() as i64;
        match state.last_compacted_ms {
            Some(last) if tick - last <= COMPACT_INTERVAL_MS => false,
            Some(_) => {
                state.last_compacted_ms = Some(tick);
                true
            }
            None => {
                state.last_compacted_ms = Some(tick);
                false
            }
        }
    };
    if !due {
        return false;
    }
    if let Err(error) = engine.do_gc() {
        log::warn!("idle compact: TJS garbage collection failed: {error}");
    }
    true
}

// ---------------------------------------------------------------------------
// Marshalling helpers
// ---------------------------------------------------------------------------

/// Whether `System.eventDisabled` currently suppresses immediate events
/// (reference `TVPGetSystemEventDisabledState`, `EventImpl.cpp:100-102`).
/// `TVPPostEvent` drops immediate events while it is set
/// (`EventIntf.cpp:210-224`).
pub(crate) fn system_events_disabled(services: &CompatServices) -> bool {
    services.state::<SystemState>().event_disabled
}

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

/// TJS `operator bool` coercion (`tjs2/tjsVariant.h:913-935`): integers and
/// reals are non-zero, strings are `AsInteger() != 0` (so `"0"` and
/// non-numeric text are *false*, unlike a non-empty check), void is false,
/// `null` — a `tvtObject` with a null pointer (`tjs2_abi.cpp:525-527`) — is
/// false, and any other object is `Object != nullptr`, i.e. true.
pub(crate) fn value_as_bool(v: &Value) -> bool {
    match v.ty {
        VAL_INTEGER => v.integer != 0,
        VAL_REAL => v.real != 0.0,
        VAL_STRING => tjs_string_to_integer(&value_as_string(v)) != 0,
        VAL_VOID | VAL_NULL => false,
        _ => true,
    }
}

/// TJS `AsInteger` coercion (`tjs2/tjsVariant.h:862-877`).
///
/// Objects (including `null` and arrays) raise a conversion error in the
/// reference — use [`try_value_as_i64`] where that error must surface. The
/// loose form keeps returning `0` for them so call sites that only ever see
/// script scalars stay unchanged.
pub(crate) fn value_as_i64(v: &Value) -> i64 {
    match v.ty {
        VAL_INTEGER => v.integer,
        VAL_REAL => v.real as i64,
        VAL_STRING => tjs_string_to_integer(&value_as_string(v)),
        _ => 0,
    }
}

/// [`value_as_i64`] with the reference's error behaviour: `AsInteger` on an
/// object, `null` or an octet raises `TJSThrowVariantConvertError`
/// (`tjs2/tjsVariant.h:865-866`, `874`), so a strict call site turns those
/// into a typed callback error instead of a silent `0`.
pub(crate) fn try_value_as_i64(v: &Value) -> Result<i64, String> {
    match v.ty {
        VAL_INTEGER | VAL_REAL | VAL_STRING | VAL_VOID => Ok(value_as_i64(v)),
        VAL_NULL => Err("value is null, not an integer".into()),
        VAL_OBJECT | VAL_ARRAY => Err("value is an object, not an integer".into()),
        VAL_OCTET => Err("value is an octet, not an integer".into()),
        _ => Err("value cannot be converted to an integer".into()),
    }
}

// `tjs_string_to_integer` lives in the standalone `tjsnum` module so the
// KAG attribute walk can include it without the rest of `system`.
pub(crate) use crate::natives::tjsnum::tjs_string_to_integer;

/// The reference `TJSStringToReal` (`tjs2/tjsVariant.cpp:309-313`): the same
/// lexer number grammar as [`tjs_string_to_integer`], but a decimal token with
/// a point or exponent keeps its fractional value (`"12.5abc"` is `12.5`);
/// every other form converts through its integer value, exactly as the
/// reference's `AsReal()` does for an integer variant. Anything unparsable
/// yields `0.0`; trailing garbage is ignored.
pub(crate) fn tjs_string_to_real(s: &str) -> f64 {
    let bytes = s.as_bytes();
    let mut pos = 0usize;
    let mut negative = false;
    if pos < bytes.len() && (bytes[pos] == b'+' || bytes[pos] == b'-') {
        negative = bytes[pos] == b'-';
        pos += 1;
        while pos < bytes.len() && bytes[pos] == b' ' {
            pos += 1;
        }
    }
    let rest = &s[pos..];
    let apply_sign = |value: f64| if negative { -value } else { value };
    let word_follows = |after: &str| {
        after
            .chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
    };
    for (keyword, value) in [
        ("true", 1.0f64),
        ("false", 0.0f64),
        ("NaN", f64::NAN),
        ("Infinity", f64::INFINITY),
    ] {
        if let Some(after) = rest.strip_prefix(keyword)
            && !word_follows(after)
        {
            return apply_sign(value);
        }
    }
    let digits = rest.as_bytes();
    if digits.len() >= 2 && digits[0] == b'0' && (digits[1] == b'x' || digits[1] == b'X') {
        return apply_sign(crate::natives::tjsnum::parse_radix(&rest[2..], 16) as f64);
    }
    if digits.len() >= 2 && digits[0] == b'0' && (digits[1] == b'b' || digits[1] == b'B') {
        return apply_sign(crate::natives::tjsnum::parse_radix(&rest[2..], 2) as f64);
    }
    if digits.first() == Some(&b'0') && digits.len() > 1 {
        match digits[1] {
            b'.' | b'e' | b'E' => {}
            b'p' | b'P' => return 0.0,
            _ => return apply_sign(crate::natives::tjsnum::parse_radix(rest, 8) as f64),
        }
    }
    apply_sign(parse_decimal_real(rest))
}

/// Parse a leading decimal number token as a real (`TJSExtractNumber` +
/// `TJSParseDecimalReal`, `tjsLex.cpp:447-509`): digits with at most one `.`
/// and one `e`/`E` exponent. A token without a point or exponent keeps the
/// reference's wrapping integer value; an unparsable prefix is `0.0`.
fn parse_decimal_real(s: &str) -> f64 {
    let bytes = s.as_bytes();
    let mut i = 0usize;
    let mut seen_point = false;
    let mut seen_exp = false;
    let mut end = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            i += 1;
            end = i;
        } else if c == b'.' && !seen_point && !seen_exp {
            seen_point = true;
            i += 1;
        } else if (c == b'e' || c == b'E') && !seen_exp {
            seen_exp = true;
            i += 1;
            while i < bytes.len() && bytes[i] == b' ' {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                i += 1;
            }
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
                end = i;
            }
        } else {
            break;
        }
    }
    if end == 0 {
        return 0.0;
    }
    let token = &s[..end];
    if seen_point || seen_exp {
        token
            .parse::<f64>()
            .or_else(|_| match token.find(['e', 'E']) {
                Some(exp) if exp > 0 => token[..exp].parse::<f64>().map_err(|_| ()),
                _ => Err(()),
            })
            .unwrap_or(0.0)
    } else {
        token
            .bytes()
            .filter(u8::is_ascii_digit)
            .fold(0i64, |value, digit| {
                value.wrapping_mul(10).wrapping_add(i64::from(digit - b'0'))
            }) as f64
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
                    (*out).retained = dv.raw_id().as_ptr() as usize;
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
pub(crate) fn get_command_line(services: &CompatServices, name: &str) -> Option<String> {
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
    Some(host_ref(ptr).game_dir().to_path_buf())
}

/// The reference storage-name form of a native host path:
/// `TVPNormalizeStorageName` prepends the local media and domain `file://.` to
/// an absolute path (`TVPPreNormalizeStorageName`,
/// `reference/cpp/core/base/impl/StorageImpl.cpp:422-467`) and the file media
/// lowercases the domain and path and unifies `\` to `/`
/// (`tTVPFileMedia::NormalizeDomainName`/`NormalizePathName`,
/// `reference/cpp/core/base/impl/StorageImpl.cpp:95-117`, applied at
/// `reference/cpp/core/base/StorageIntf.cpp:362-369`). A non-absolute path is
/// returned as-is.
fn normalize_host_path(path: &Path) -> String {
    let native = path.to_string_lossy().replace('\\', "/");
    if native.starts_with('/') {
        format!("file://.{}", native.to_ascii_lowercase())
    } else {
        native
    }
}

/// The reference application-path form: normalized and carrying the trailing
/// `/` the reference's `TVPExtractStoragePath` keeps
/// (`reference/cpp/core/base/StorageIntf.cpp:837-859`).
fn normalized_app_path(path: &Path) -> String {
    let normalized = normalize_host_path(path);
    if normalized.ends_with('/') {
        normalized
    } else {
        format!("{normalized}/")
    }
}

/// The logical screen size: the first window's primary-layer rect, or its
/// client size while it has no primary layer, or `(0, 0)` when no window
/// exists yet (the reference's unset default).
fn screen_size(ctx: &CompatCtx) -> (u32, u32) {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine, and the
    // host keeps the attached scene alive for the binding's lifetime.
    let Some(ptr) = (unsafe { ctx.services() }).scene() else {
        return (0, 0);
    };
    let scene = host_ref(ptr);
    let Some(window) = scene.windows.first() else {
        return (0, 0);
    };
    window
        .primary_layer
        .and_then(|primary| scene.layer(primary))
        .map_or(window.inner_size, |layer| (layer.rect.w, layer.rect.h))
}

// ---------------------------------------------------------------------------
// System methods
// ---------------------------------------------------------------------------

/// `System.inform(text [, caption [, buttons]])`
///
/// The reference shows a modal message box and returns the clicked button's
/// index (or clears the result when no `buttons` argument is given,
/// `SystemImpl.cpp:570-620`). krkr-rs has no GUI, so the text is logged at
/// `info!` and the left/first button is emulated unless the button set is one
/// the reference dialog cannot render.
extern "C" fn native_inform(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.inform requires 1 argument");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let text = value_as_string(&a[0]);
    let caption = if argc >= 2 && a[1].ty != VAL_VOID {
        value_as_string(&a[1])
    } else {
        "Information".to_string()
    };
    log::info!("System.inform [{caption}]: {text}");
    if argc >= 3 && a[2].ty != VAL_VOID {
        let count = match inform_button_count(engine, &a[2]) {
            Ok(count) => count,
            Err(error) => return report_error(out_error, &format!("System.inform: {error}")),
        };
        // The reference's GTK dialog returns 0 for one or two buttons (the
        // first is "OK"/"Yes") and -1 when the button set has no dialog
        // (`environ/linux/Platform.cpp:140-183`).
        set_int(out, if (1..=2).contains(&count) { 0 } else { -1 });
    } else {
        set_void(out);
    }
    0
}

/// The button count of `System.inform`'s third argument: an array's `count`,
/// or the literal integer button count. A non-numeric object has no buttons.
fn inform_button_count(engine: *mut c_void, value: &Value) -> Result<i64, String> {
    if value.ty == VAL_OBJECT {
        let Some(resolved) = resolve(engine) else {
            return Err("compat context is not bound".to_string());
        };
        let object = resolved.engine().retain_object_arg(value)?;
        return match resolved.engine().member(object.raw_id(), "count") {
            Ok(TjsValue::Integer(count)) => Ok(count),
            Ok(TjsValue::Real(count)) => Ok(count as i64),
            Ok(_) => Ok(0),
            Err(error) => Err(error),
        };
    }
    try_value_as_i64(value)
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

/// `System.getArgument(name)`
///
/// Returns the stored value, or void when the name is missing. Extra
/// arguments are ignored; there is no default parameter.
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let name = value_as_string(&a[0]);
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.getArgument: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    // The reference `getArgument` takes **one** argument and clears the
    // result on a miss (`SystemImpl.cpp:709-726`) — there is no default
    // parameter; any second argument is ignored.
    match get_command_line(unsafe { ctx.services() }, &name) {
        Some(value) => {
            set_string_out(&ctx, out, &value);
            0
        }
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
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
        // `numparams > 0 ? param[0]->AsInteger() : 0` (`SystemIntf.cpp:111`,
        // `123`) — the strict conversion raises on objects.
        // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
        match try_value_as_i64(&unsafe { args(argv, argc) }[0]) {
            Ok(code) => code,
            Err(error) => return Some(error),
        }
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
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
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // The reference `createUUID` body has no `numparams` check
    // (`SystemIntf.cpp:259-286`) — extra arguments are ignored.
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.createUUID: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let uuid = {
        let mut state = services(&ctx).state::<SystemState>();
        make_uuid(&mut state)
    };
    set_string_out(&ctx, out, &uuid);
    0
}

/// `System.assignMessage(id, message)` → bool
///
/// Replaces the text of the registered TJS message `id` and returns whether a
/// message by that name exists (reference `SystemIntf.cpp:288-304`,
/// `TJS::TJSAssignMessage`). The VM's message registry is the override map.
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let id = value_as_string(&a[0]);
    let message = value_as_string(&a[1]);
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.assignMessage: compat context is not bound",
        );
    };
    match resolved.engine().assign_message(&id, &message) {
        Ok(assigned) => {
            set_bool(out, assigned);
            0
        }
        Err(error) => report_error(out_error, &format!("System.assignMessage: {error}")),
    }
}

/// `System.touchImages(storages[, limit[, timeout]])` → void
///
/// `System.touchImages(storages[, limit[, timeout]])` → void
///
/// The reference requires an object first argument and converts the optional
/// `limit`/`timeout` (`SystemIntf.cpp:224-256`); the pre-caching itself has no
/// L2 counterpart (the port caches per layer on demand), so only the
/// reference's argument validation is reproduced.
extern "C" fn native_touch_images(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.touchImages requires 1 argument");
    }
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // The reference requires an array object (`AsObjectClosureNoAddRef` +
    // `PropGetByNum` walk, `SystemIntf.cpp:224-245` — a non-object raises the
    // conversion error) and converts the optional `limit`/`timeout` with
    // `(tjs_int64)*param[i]` when present and non-void, which raises on
    // objects. The image pre-caching itself has no L2 counterpart (the port
    // caches per layer on demand), so only the reference's argument
    // validation is reproduced.
    if a[0].ty == VAL_NULL || a[0].ty != VAL_OBJECT {
        return report_error(
            out_error,
            "System.touchImages: the first argument must be an array",
        );
    }
    for (index, name) in [(1, "limit"), (2, "timeout")] {
        if let Some(value) = a.get(index)
            && value.ty != VAL_VOID
            && let Err(error) = try_value_as_i64(value)
        {
            return report_error(out_error, &format!("System.touchImages {name}: {error}"));
        }
    }
    set_void(out);
    0
}

/// `System.doCompact([level])` → void
///
/// The reference defaults `level` to `TVP_COMPACT_LEVEL_MAX` (100) and runs
/// the TJS garbage collector from `clIdle` (5) up; a level at or above
/// `TVP_COMPACT_LEVEL_MINIMIZE` (15) also clears the graphic cache
/// (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1463-1470`).
extern "C" fn native_do_compact(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let level = match unsafe { args(argv, argc) }.first() {
        // `if(numparams >= 1 && param[0]->Type() != tvtVoid) level =
        // (tjs_int)*param[0]` (`SystemIntf.cpp:306-311`): absent/void keeps
        // `TVP_COMPACT_LEVEL_MAX` (100), the strict conversion raises on
        // objects.
        Some(value) if value.ty != VAL_VOID => match try_value_as_i64(value) {
            Ok(level) => level,
            Err(error) => return report_error(out_error, &error),
        },
        _ => 100,
    };
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.doCompact: compat context is not bound");
    };
    if level >= 5
        && let Err(e) = resolved.engine().do_gc()
    {
        log::warn!("System.doCompact: TJS garbage collection failed: {e}");
    }
    if level >= 15 {
        // The reference's `tTVPClearGraphicCacheCallback::OnCompact` clears the
        // graphic cache at or above `TVP_COMPACT_LEVEL_MINIMIZE`
        // (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1463-1470`).
        let ctx = resolved.ctx();
        // SAFETY: `ctx` borrows the live engine state resolved above; the
        // services handle is valid for `ctx`'s lifetime.
        let services = unsafe { ctx.services() };
        clear_graphic_cache(services);
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(out_error, "System.addContinuousHandler requires 1 argument");
    }
    // Reference `param[0]->AsObjectClosureNoAddRef()`
    // (`SystemIntf.cpp:167-180`): TJS `null` stores an empty closure that the
    // delivery loop skips (`EventIntf.cpp:816`) — a silent success — while
    // any other non-object raises the conversion error.
    if a[0].ty == VAL_NULL {
        set_void(out);
        return 0;
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    if a.is_empty() {
        return report_error(
            out_error,
            "System.removeContinuousHandler requires 1 argument",
        );
    }
    // Same conversion contract as `addContinuousHandler`
    // (`SystemIntf.cpp:183-197`): `null` matches an empty slot and no-ops.
    if a[0].ty == VAL_NULL {
        set_void(out);
        return 0;
    }
    if a[0].ty != VAL_OBJECT {
        return report_error(
            out_error,
            "System.removeContinuousHandler: argument is not an object",
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

/// Reference `tTVPTouchDevice` capability bits (`SystemImpl.cpp:424-434`).
const TD_MOUSE: u32 = 0x0000_0100;
const TD_MOUSE_WHEEL: u32 = 0x0000_0200;

/// Clear the attached scene's decoded-image cache and release every bitmap
/// it no longer references (the reference `TVPClearGraphicCache`,
/// `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1456-1459`). Returns the
/// number of unreferenced bitmaps freed; a missing scene has no cache and
/// frees nothing.
fn clear_graphic_cache(services: &CompatServices) -> usize {
    let Some(ptr) = services.scene() else {
        return 0;
    };
    // SAFETY: the host keeps the attached scene alive for the binding's
    // lifetime, and the VM is single-threaded.
    let scene = unsafe { &mut *ptr };
    scene.clear_decoded_image_cache();
    scene.collect_unused_bitmaps()
}

/// `System.getKeyState(code [, getcurrent])` → int
///
/// Reads the attached L2 `engine_api::InputState` through the same
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    // `(tjs_int)*param[0]` raises on objects (`SystemImpl.cpp:639-642`).
    let code = match try_value_as_i64(&a[0]) {
        Ok(code) => code as u32,
        Err(error) => return report_error(out_error, &error),
    };
    let getcurrent = if argc >= 2 {
        // `0 != (tjs_int)*param[1]` (`SystemImpl.cpp:644-646`) — void is 0
        // (false), objects raise.
        match try_value_as_i64(&a[1]) {
            Ok(value) => value != 0,
            Err(error) => return report_error(out_error, &error),
        }
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
/// The reference `TVPShellExecute` returns `true` without launching anything
/// (the `ShellExecute` call is disabled, `SystemImpl.cpp:77-88`); the port
/// reproduces that inert value (the null-plugin precedent, doc 08 §4.2) rather
/// than raising.
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
    // The reference converts both arguments (`ttstr`), so a conversion error
    // still propagates, but then discards them.
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let a = unsafe { args(argv, argc) };
    let _target = value_as_string(&a[0]);
    if argc >= 2 {
        let _param = value_as_string(&a[1]);
    }
    log::warn!("System.shellExecute: no shell is available; returning true without launching");
    set_bool(out, true);
    0
}

/// `System.system(target)` → int
///
/// The reference delivers a maximum-level compact event (which clears the
/// graphic cache) and returns the (commented-out) shell status `0`
/// (`SystemImpl.cpp:674-691`). The port releases the L2 scene's unreferenced
/// bitmaps and returns `0`; it never runs a command, so it logs a warning
/// instead of raising.
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
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let _target = value_as_string(&unsafe { args(argv, argc) }[0]);
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.system: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    clear_graphic_cache(services);
    log::warn!("System.system: no command is run; returning 0");
    set_int(out, 0);
    0
}

/// `System.clearGraphicCache()` → void
///
/// Clears the attached L2 `engine_api::Scene`'s decoded-image cache and releases
/// every bitmap it no longer references — the L2 counterpart of the
/// reference's `TVPClearGraphicCache`. With no scene attached there is no
/// cache, so the call is a no-op.
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
    // `System.exePath` is `TVPGetAppPath()`
    // (`reference/cpp/core/base/impl/SystemImpl.cpp:798-807`), which is
    // `TVPExtractStoragePath(TVPProjectDir)`
    // (`reference/cpp/core/base/impl/StorageImpl.cpp:504-507`).
    match project_dir(&ctx) {
        Some(dir) => {
            set_string_out(&ctx, out, &normalized_app_path(&dir));
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
    // `System.dataPath` is `TVPDataPath`
    // (`reference/cpp/core/base/impl/SystemImpl.cpp:828-836`), the application
    // directory plus `savedata/`
    // (`reference/cpp/core/environ/impl/ApplicationSpecialPath.h:13-19`).
    match project_dir(&ctx) {
        Some(dir) => {
            set_string_out(
                &ctx,
                out,
                &format!("{}savedata/", normalized_app_path(&dir)),
            );
            0
        }
        None => report_error(out_error, "System.dataPath: storage is not attached"),
    }
}

/// `System.personalPath` / `System.savedGamesPath`: the application directory
/// (the game folder), not its `savedata/` subfolder. The reference's
/// `TVPGetPersonalPath()` and `TVPGetSavedGamesPath()` both return
/// `TVPGetAppPath()` (`reference/cpp/core/base/impl/SystemImpl.cpp:341-359`,
/// `:387-397`); only `TVPDataPath` names `savedata/`.
fn app_path_prop(
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
            set_string_out(&ctx, out, &normalized_app_path(&dir));
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
    app_path_prop(engine, out, out_error, "personalPath")
}

extern "C" fn prop_saved_games_path(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    app_path_prop(engine, out, out_error, "savedGamesPath")
}

/// The synthetic app directory reported when no L2 storage is attached.
const FALLBACK_EXE_NAME: &str = "krkr-rs/";

/// `System.exeName` — the emulated `ExePath()` in normalized storage form.
///
/// The reference returns `TVPNormalizeStorageName(ExePath())`
/// (`reference/cpp/core/base/impl/SystemImpl.cpp:838-847`), where `ExePath()`
/// is the application path (`reference/cpp/core/environ/Application.cpp:92`).
/// The application path here is the L2 storage root ([`project_dir`], the
/// launcher's `--base-dir` / first archive's parent / default working
/// directory). When no storage is attached the app path is unknown, so the
/// getter reports the normalized working directory instead of raising: a
/// Windows application always has an executable path.
extern "C" fn prop_exe_name(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.exeName: compat context is not bound");
    };
    let ctx = resolved.ctx();
    let name = match project_dir(&ctx) {
        Some(dir) => normalized_app_path(&dir),
        None => match std::env::current_dir() {
            Ok(dir) => normalized_app_path(&dir),
            Err(_) => FALLBACK_EXE_NAME.to_string(),
        },
    };
    set_string_out(&ctx, out, &name);
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

/// The four components `TVPGetVersion` sets statically: major, minor,
/// release and build (`reference/cpp/core/base/impl/MsgImpl.cpp:20-30`).
const KIRIKIRI_VERSION: (i64, i64, i64, i64) = (2, 32, 2, 426);

/// `TVPGetVersionString`: `"{major}.{minor}.{release}.{build}"`
/// (`reference/cpp/core/base/MsgIntf.cpp:173-177`).
fn kirikiri_version_string() -> String {
    let (major, minor, release, build) = KIRIKIRI_VERSION;
    format!("{major}.{minor}.{release}.{build}")
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
    set_string_out(&ctx, out, &kirikiri_version_string());
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
    // Reference `TVPGetVersionInformation` (`base/MsgIntf.cpp:158-171`) with
    // the default `TVPVersionInformation` template (`base/MsgIntfInc.h:5-9`):
    // `%1` is the KiriKiri version, `%2` the TJS2 version, and `%DATE%`/
    // `%TIME%` the build macros (`base/MsgIntf.cpp:136-137`, `166-168`).
    let (date, time) = resolved.engine().compile_date_time();
    let tjs_version = resolved.engine().tjs_version_string();
    let version = kirikiri_version_string();
    let information = format!(
        "Kirikiri 2 Executable core /{version} (Compiled on {date} {time}) \
         TJS2/{tjs_version} Copyright (C) 1997-2013 W.Dee and contributors All rights reserved."
    );
    set_string_out(&ctx, out, &information);
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
    let services = unsafe { ctx.services() };
    let limit = match services.scene() {
        // SAFETY: the host keeps the attached scene alive for the binding's
        // lifetime and the VM is single-threaded.
        // The getter returns `(tjs_int)TVPGetGraphicCacheLimit()`
        // (`reference/cpp/core/base/SystemIntf.cpp:358-361`), so the stored
        // `tjs_uint64` narrows to 32 bits on the way out.
        Some(ptr) => i64::from(unsafe { &*ptr }.graphic_cache_limit() as i32),
        None => 0,
    };
    set_int(out, limit);
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
    // The strict conversion raises on objects, and the write immediately
    // evicts the decoded-image cache down to the new limit
    // (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:2423-2441`).
    let limit = match try_value_as_i64(unsafe { &*value }) {
        Ok(limit) => limit,
        Err(error) => return report_error(out_error, &error),
    };
    // The reference passes `(tjs_int)*param` to `TVPSetGraphicCacheLimit`
    // (`reference/cpp/core/base/SystemIntf.cpp:365`), i.e. the variant cast
    // `(tjs_int)AsInteger()` (`reference/cpp/core/tjs2/tjsVariant.h:933-935`):
    // a value outside the 32-bit range truncates to its low 32 bits instead
    // of raising.
    let limit = i64::from(limit as i32);
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    if let Some(ptr) = services.scene() {
        // SAFETY: the host keeps the attached scene alive for the binding's
        // lifetime and the VM is single-threaded.
        unsafe { &mut *ptr }.set_graphic_cache_limit(limit);
    }
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
    // `0 != (tjs_int)*param` (`SystemIntf.cpp:399-402`) — the strict
    // conversion raises on objects.
    let flag = match try_value_as_i64(unsafe { &*value }) {
        Ok(value) => value != 0,
        Err(error) => return report_error(out_error, &error),
    };
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
    // `0 != (tjs_int)*param` (`SystemIntf.cpp:457-460`).
    let flag = match try_value_as_i64(unsafe { &*value }) {
        Ok(value) => value != 0,
        Err(error) => return report_error(out_error, &error),
    };
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
    // `TVPDrawThreadNum = (tjs_int)*param` (`SystemIntf.cpp:413-416`).
    let n = match try_value_as_i64(unsafe { &*value }) {
        Ok(n) => n,
        Err(error) => return report_error(out_error, &error),
    };
    let ctx = resolved.ctx();
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    unsafe { ctx.services() }
        .state::<SystemState>()
        .draw_thread_num = n;
    0
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
    // `System.appDataPath` is `TVPGetAppDataPath()`, which returns
    // `TVPGetAppPath()` unconditionally — the OS-specific branch is disabled
    // (`reference/cpp/core/base/impl/SystemImpl.cpp:366-380`, getter at
    // `:819`) — so it equals `System.exePath`.
    app_path_prop(engine, out, out_error, "appDataPath")
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
/// The reference `TVPGetSupportTouchDevice` returns `tdMouse | tdMouseWheel`
/// unconditionally: the `GetSystemMetrics` probes are disabled, so the field
/// never varies (`SystemImpl.cpp:439-456`, `938-945`).
extern "C" fn prop_touch_device(
    _engine: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_int(out, i64::from(TD_MOUSE | TD_MOUSE_WHEEL));
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
        match host_ref(ptr).read("RegisterData.tjs") {
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
    })?;
    // Reference: the `System` class object carries default *data* members
    // `exceptionHandler`, `onActivate` and `onDeactivate`, all `null`
    // (`PropSet` at the end of the class constructor,
    // `base/SystemIntf.cpp:477-484`), so `System.onActivate` reads as `null`
    // before a script assigns one and `MEMBERMUSTEXIST` lookups succeed
    // (`SystemIntf.cpp:58-77`).
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let host = unsafe { ctx.engine() };
    if let (Ok(RetainedValue::Object(class)), Ok(RetainedValue::Object(null))) = (
        host.eval_retained("System", "System.defaults"),
        host.eval_retained("null", "System.defaults"),
    ) {
        for name in ["exceptionHandler", "onActivate", "onDeactivate"] {
            match host.duplicate_retained(null.raw_id()) {
                Ok(value) => {
                    let id = value.raw_id().as_ptr() as u64;
                    if let Err(error) =
                        host.set_member(class.raw_id(), name, &TjsValue::Retained(id))
                    {
                        log::warn!("System: default member {name}: {error}");
                    }
                    drop(value);
                }
                Err(error) => {
                    log::warn!("System: default member {name}: {error}");
                    break;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{tjs_string_to_integer, tjs_string_to_real};

    /// `TJSStringToReal` parses the leading number token
    /// (`reference/cpp/core/tjs2/tjsVariant.cpp:309-313`,
    /// `reference/cpp/core/tjs2/tjsLex.cpp:611-743`), so `"12abc"` is 12,
    /// not the `0` a whole-string parse would produce.
    #[test]
    fn string_to_real_parses_the_leading_number() {
        assert_eq!(tjs_string_to_real("12abc"), 12.0);
        assert_eq!(tjs_string_to_real("12.5abc"), 12.5);
        // The sign is recognised only at the first character, and a space is
        // skipped only *after* a sign (`tjsLex.cpp:700-720`).
        assert_eq!(tjs_string_to_real("  -3.5e2xyz"), 0.0);
        assert_eq!(tjs_string_to_real("- 3.5e2xyz"), -350.0);
        assert_eq!(tjs_string_to_real("0x10"), 16.0);
        assert_eq!(tjs_string_to_real("true"), 1.0);
        assert_eq!(tjs_string_to_real("false"), 0.0);
        assert_eq!(tjs_string_to_real("abc"), 0.0);
    }

    /// `TJSStringToInteger` (`reference/cpp/core/tjs2/tjsVariant.cpp:301-307`)
    /// parses the same leading token and truncates a real towards zero.
    #[test]
    fn string_to_integer_parses_the_leading_number() {
        assert_eq!(tjs_string_to_integer("12abc"), 12);
        assert_eq!(tjs_string_to_integer("12.9abc"), 12);
        assert_eq!(tjs_string_to_integer("0x10"), 16);
        assert_eq!(tjs_string_to_integer("abc"), 0);
    }
}
