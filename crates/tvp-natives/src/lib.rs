//! tvp-natives — TVP native classes implemented in Rust.
//!
//! This crate ports the KiriKiri TVP native classes to Rust and registers
//! them on the TJS2 VM global object, so scripts can call
//! `System.method(...)` / `Debug.method(...)`. This wave implements the
//! **methods**; the property getters/setters need an FFI extension landing
//! in parallel and are documented as pending in the module docs.
//!
//! Call [`register_all`] (or [`register_system`] / [`register_debug`]) once
//! per engine, from the thread that owns the engine.
//!
//! # Global state
//!
//! The reference keeps process-wide state (command-line arguments, the log
//! ring buffer, the log file handle). This port mirrors that: the state
//! behind the natives is process-global (`static`), not per-engine, and
//! protected by mutexes so the natives also work when called directly from
//! Rust tests. The VM itself remains single-threaded.
//!
//! # Logging
//!
//! Natives emit through the `log` crate (`info!` / `debug!` / `warn!` /
//! `error!`). This crate does not install a logger; the host application is
//! expected to do so (without one the calls are no-ops).
//!
//! # Testing
//!
//! `cargo test -p tvp-natives -- --test-threads=1` (the process-global
//! state makes parallel tests racy).

mod async_trigger;
mod chain_item_base;
mod constants;
mod debug;
mod menu_item;
mod plugin_stubs;
mod plugins;
mod system;

pub use async_trigger::{async_trigger_poll, register_async_trigger};
pub use chain_item_base::register_chain_item_base;
pub use debug::register_debug;
pub use menu_item::register_menu_item;
pub use plugin_stubs::register_plugin_stubs;
pub use plugins::register_plugins;
pub use system::{
    SystemContext, continuous_handler_poll, register_system, set_key_state, set_system_context,
};

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_int};
use std::ptr;
use std::slice;
use std::sync::{LazyLock, Mutex, MutexGuard};

use tjs2_sys::{
    Tjs2Engine, TjsValue, VAL_INTEGER, VAL_REAL, VAL_RETAINED, VAL_STRING, VAL_VOID, Value,
    tjs2_malloc,
};

/// The engine the natives are registered on (needed by natives whose
/// callbacks receive only the raw `tjs2_engine*` ABI pointer, which is NOT a
/// `Tjs2Engine*` — see the visual Timer's comment). Set by [`register_all`];
/// the engine outlives every native call (process-lifetime app VM).
static ENGINE: LazyLock<Mutex<Option<usize>>> = LazyLock::new(|| Mutex::new(None));

/// The registered engine as a shared reference.
pub(crate) fn context_engine() -> &'static Tjs2Engine {
    // SAFETY: the address was stored by register_all and the engine is never
    // freed before the process ends.
    let ptr = *lock_ok(&ENGINE)
        .as_ref()
        .expect("tvp-natives: engine context not set");
    // SAFETY: the host keeps the registered engine alive while callbacks run.
    unsafe { &*(ptr as *const Tjs2Engine) }
}

/// Evaluate a TJS expression that returns an object and place that object in
/// a native callback result.  The C ABI cannot construct dictionaries
/// directly, but it can transfer a retained object; this helper centralizes
/// the same pattern used by the KAG parser and visual natives.
///
/// The C++ trampoline consumes the retained id while copying the result, so
/// the temporary Rust owner is intentionally forgotten after the id is put
/// in `out`.
pub(crate) fn set_object_result(
    out: *mut Value,
    expression: &str,
    name: &str,
) -> Result<(), String> {
    context_engine()
        .eval(expression, name)
        .map_err(|e| e.to_string())?;
    let value = context_engine().retain_value_detached(&TjsValue::Object)?;
    // SAFETY: `out` is the valid result slot supplied by the C++ trampoline.
    unsafe {
        (*out).ty = VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
        (*out).array = std::ptr::null();
        (*out).array_count = 0;
        (*out).retained = value.raw_id() as usize;
    }
    std::mem::forget(value);
    Ok(())
}

/// Register all native classes in this crate on `engine` (`System`, `Debug`,
/// `Plugins`, and the `MenuItem` stub that keeps k2compat's menu-delay
/// machinery from installing a throwing lazy loader).
pub fn register_all(engine: &Tjs2Engine) -> Result<(), String> {
    *lock_ok(&ENGINE) = Some(engine as *const Tjs2Engine as usize);
    // Global TVP constants (ltOpaque, ssShift, ...) the game scripts use as
    // bare globals.
    engine
        .exec_script(constants::CONSTANTS_SCRIPT, "TVP_global_constants")
        .map_err(|e| format!("failed to evaluate TVP global constants: {e}"))?;
    register_system(engine)?;
    register_debug(engine)?;
    register_plugins(engine)?;
    register_plugin_stubs(engine)?;
    register_menu_item(engine)?;
    register_chain_item_base(engine)?;
    register_async_trigger(engine)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared callback helpers
// ---------------------------------------------------------------------------

thread_local! {
    /// Scratch buffer for string return values (`out.string`). Stays valid
    /// until the next native call on this thread, which is long enough: the
    /// C++ trampoline copies the string immediately after the callback
    /// returns.
    static STRING_OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// View the `argc` callback arguments as a slice. Returns an empty slice for
/// a null pointer / zero count (the C++ side passes a null `argv` when a
/// method is called without arguments).
pub(crate) fn args<'a>(argv: *const Value, argc: c_int) -> &'a [Value] {
    if argc <= 0 || argv.is_null() {
        return &[];
    }
    // SAFETY: the C++ trampoline guarantees `argc` valid Value entries at
    // `argv` for the duration of the call.
    unsafe { slice::from_raw_parts(argv, argc as usize) }
}

/// Convert a callback argument to its string form, mirroring the reference's
/// `ttstr(variant)` coercion: strings pass through, integers/reals use their
/// decimal representation, void and objects become `""`.
pub(crate) fn value_as_string(v: &Value) -> String {
    match v.ty {
        VAL_STRING if v.string.is_null() => String::new(),
        VAL_STRING => {
            // SAFETY: the C++ side guarantees a NUL-terminated UTF-8 string
            // valid for the duration of the call.
            let s = unsafe { CStr::from_ptr(v.string) };
            s.to_string_lossy().into_owned()
        }
        VAL_INTEGER => v.integer.to_string(),
        VAL_REAL => v.real.to_string(),
        _ => String::new(),
    }
}

/// Convert a callback argument to a boolean (TJS `operator bool` semantics):
/// integers/reals are non-zero, strings are non-empty, void is false,
/// objects are true.
pub(crate) fn value_as_bool(v: &Value) -> bool {
    match v.ty {
        VAL_INTEGER => v.integer != 0,
        VAL_REAL => v.real != 0.0,
        VAL_STRING => !value_as_string(v).is_empty(),
        VAL_VOID => false,
        _ => true,
    }
}

/// Convert a callback argument to an integer (TJS `AsInteger` semantics).
pub(crate) fn value_as_i64(v: &Value) -> i64 {
    match v.ty {
        VAL_INTEGER => v.integer,
        VAL_REAL => v.real as i64,
        VAL_STRING => value_as_string(v).parse().unwrap_or(0),
        _ => 0,
    }
}

/// Report a native error: point `*out_error` at a malloc'd NUL-terminated
/// message (the C++ side frees it with `tjs2_free_string`) and return the
/// non-zero status code.
pub(crate) fn report_error(out_error: *mut *mut c_char, msg: &str) -> c_int {
    // SAFETY: out_error points at a valid char* slot for the duration of the
    // call.
    unsafe { *out_error = alloc_error_string(msg) };
    1
}

/// Build a malloc'd NUL-terminated UTF-8 error message (freed on the C++
/// side with `tjs2_free_string`).
pub(crate) fn alloc_error_string(msg: &str) -> *mut c_char {
    let bytes = msg.as_bytes();
    // SAFETY: tjs2_malloc is malloc-compatible; we write a NUL-terminated
    // copy and the C++ trampoline frees it with tjs2_free_string.
    unsafe {
        let buf = tjs2_malloc(bytes.len() + 1) as *mut u8;
        if buf.is_null() {
            return ptr::null_mut();
        }
        ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
        *buf.add(bytes.len()) = 0;
        buf as *mut c_char
    }
}

/// Lock a global mutex, recovering from poisoning (a previous panic while
/// holding it) instead of propagating the poison error across the FFI
/// boundary (a panic on the VM thread aborts the process).
pub(crate) fn lock_ok<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Write `s` into `*out` as a string return value.
pub(crate) fn set_string_out(out: *mut Value, s: &str) {
    STRING_OUT.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.clear();
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
        // SAFETY: out is a valid return slot for the duration of the call.
        unsafe {
            (*out).ty = VAL_STRING;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = buf.as_ptr() as *const c_char;
        }
    });
}

/// Write an integer return value into `*out`.
pub(crate) fn set_int_out(out: *mut Value, v: i64) {
    // SAFETY: out is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_INTEGER;
        (*out).integer = v;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

/// Write a real return value into `*out`.
pub(crate) fn set_real_out(out: *mut Value, v: f64) {
    // SAFETY: out is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_REAL;
        (*out).integer = 0;
        (*out).real = v;
        (*out).string = ptr::null();
    }
}

/// Clear the return slot (void return, matching the reference's
/// `result->Clear()`).
pub(crate) fn set_void_out(out: *mut Value) {
    // SAFETY: out is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_lock {
    /// The natives register a process-global VM context; parallel tests
    /// race on it (segfault). Serialize with one process-wide lock; other
    /// crates stay fully parallel.
    static VM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn vm_lock() -> std::sync::MutexGuard<'static, ()> {
        VM_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_lock::vm_lock;
    use tjs2_sys::TjsValue;

    fn registered_engine() -> Tjs2Engine {
        let e = Tjs2Engine::new().expect("create engine");
        register_all(&e).expect("register System + Debug");
        e
    }

    // -- System.setArgument / System.getArgument --------------------------

    #[test]
    fn system_set_and_get_argument_roundtrip() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        e.exec_script("System.setArgument('-foo', 'bar');", "test")
            .unwrap();
        assert_eq!(
            e.eval("System.getArgument('-foo')", "test").unwrap(),
            TjsValue::String("bar".into())
        );
        // a repeated set replaces the value
        e.exec_script("System.setArgument('-foo', 'baz');", "test")
            .unwrap();
        assert_eq!(
            e.eval("System.getArgument('-foo')", "test").unwrap(),
            TjsValue::String("baz".into())
        );
    }

    #[test]
    fn system_get_argument_returns_default_when_missing() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        assert_eq!(
            e.eval("System.getArgument('-missing', 'dflt')", "test")
                .unwrap(),
            TjsValue::String("dflt".into())
        );
        // a non-string default keeps its own type
        assert_eq!(
            e.eval("System.getArgument('-missing2', 42)", "test")
                .unwrap(),
            TjsValue::Integer(42)
        );
    }

    #[test]
    fn system_get_argument_missing_without_default_is_void() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        // reference behavior: `result->Clear()` when the argument is absent
        assert_eq!(
            e.eval("System.getArgument('-nope')", "test").unwrap(),
            TjsValue::Void
        );
    }

    #[test]
    fn system_bad_param_count_is_catchable() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        e.exec_script(
            "var r = ''; try { System.getArgument(); } catch(e) { r = 'caught'; }",
            "test",
        )
        .unwrap();
        assert_eq!(
            e.eval("r", "test").unwrap(),
            TjsValue::String("caught".into())
        );
    }

    // -- System misc ------------------------------------------------------

    #[test]
    fn system_inform_does_not_crash() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        assert_eq!(
            e.eval("System.inform('x')", "test").unwrap(),
            TjsValue::Void
        );
        // caption overload accepted
        assert_eq!(
            e.eval("System.inform('x', 'Cap')", "test").unwrap(),
            TjsValue::Void
        );
    }

    #[test]
    fn system_get_tick_count_returns_non_negative_integer() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        match e.eval("System.getTickCount()", "test").unwrap() {
            TjsValue::Integer(v) => assert!(v >= 0, "tick count must be >= 0, got {v}"),
            other => panic!("expected Integer, got {other:?}"),
        }
    }

    #[test]
    fn system_stubs_return_their_reference_types() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        assert_eq!(
            e.eval("System.getKeyState(1)", "test").unwrap(),
            TjsValue::Integer(0)
        );
        assert_eq!(
            e.eval("System.system('cmd')", "test").unwrap(),
            TjsValue::Integer(0)
        );
        assert_eq!(
            e.eval("System.createAppLock('lock')", "test").unwrap(),
            TjsValue::Integer(1) // single-process emulator: first instance wins
        );
        assert_eq!(e.eval("System.dumpHeap()", "test").unwrap(), TjsValue::Void);
        // nullpo must NOT crash the process (the reference traps)
        assert_eq!(e.eval("System.nullpo()", "test").unwrap(), TjsValue::Void);
        assert_eq!(
            e.eval("System.showVersion()", "test").unwrap(),
            TjsValue::Void
        );
        assert_eq!(
            e.eval("System.readRegValue('key')", "test").unwrap(),
            TjsValue::Void
        );
    }

    #[test]
    fn system_key_state_tracks_host_input() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        set_key_state(0x41, true);
        assert_eq!(
            e.eval("System.getKeyState(0x41)", "test").unwrap(),
            TjsValue::Integer(1)
        );
        set_key_state(0x41, false);
        assert_eq!(
            e.eval("System.getKeyState(0x41)", "test").unwrap(),
            TjsValue::Integer(0)
        );
    }

    #[test]
    fn system_monitor_properties_follow_context() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        let cwd = std::env::current_dir().unwrap();
        set_system_context(SystemContext {
            project_dir: cwd.clone(),
            app_data_dir: cwd,
            screen_size: (800, 600),
            desktop_origin: (-1920, 0),
            desktop_size: (3840, 2160),
            touch_device: false,
        });
        assert_eq!(
            e.eval("System.screenWidth", "test").unwrap(),
            TjsValue::Integer(800)
        );
        assert_eq!(
            e.eval("System.desktopLeft", "test").unwrap(),
            TjsValue::Integer(-1920)
        );
        assert_eq!(
            e.eval("System.desktopWidth", "test").unwrap(),
            TjsValue::Integer(3840)
        );
        assert_eq!(
            e.eval("System.desktopHeight", "test").unwrap(),
            TjsValue::Integer(2160)
        );
        set_system_context(SystemContext::default());
    }

    // -- Debug ------------------------------------------------------------

    #[test]
    fn debug_message_then_get_last_log() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        assert_eq!(
            e.eval("Debug.message('hi')", "test").unwrap(),
            TjsValue::Void
        );
        assert_eq!(
            e.eval("Debug.getLastLog()", "test").unwrap(),
            TjsValue::String("hi".into())
        );
    }

    #[test]
    fn debug_message_joins_multiple_arguments() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        e.exec_script("Debug.message('a', 'b', 'c');", "test")
            .unwrap();
        assert_eq!(
            e.eval("Debug.getLastLog()", "test").unwrap(),
            TjsValue::String("a, b, c".into())
        );
    }

    #[test]
    fn debug_get_last_log_lines_parameter() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        e.exec_script("Debug.message('one'); Debug.message('two');", "test")
            .unwrap();
        assert_eq!(
            e.eval("Debug.getLastLog()", "test").unwrap(),
            TjsValue::String("two".into())
        );
        assert_eq!(
            e.eval("Debug.getLastLog(2)", "test").unwrap(),
            TjsValue::String("one\ntwo".into())
        );
    }

    #[test]
    fn debug_log_as_error_toggles_message_level() {
        let _vm_lock = vm_lock();
        install_capture_logger();
        CAPTURED.lock().unwrap().clear();
        let e = registered_engine();
        e.exec_script(
            "Debug.logAsError(false); Debug.message('normal'); \
             Debug.logAsError(true); Debug.message('errmsg'); \
             Debug.logAsError(false);",
            "test",
        )
        .unwrap();
        let captured = CAPTURED.lock().unwrap().clone();
        assert!(
            captured.contains(&(log::Level::Info, "normal".to_string())),
            "expected info 'normal', captured: {captured:?}"
        );
        assert!(
            captured.contains(&(log::Level::Error, "errmsg".to_string())),
            "expected error 'errmsg', captured: {captured:?}"
        );
    }

    #[test]
    fn debug_start_log_to_file_appends() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        let path = "tvp_natives_test_log.txt";
        let _ = std::fs::remove_file(path);
        e.exec_script(
            "Debug.startLogToFile('tvp_natives_test_log.txt'); Debug.message('file-line');",
            "test",
        )
        .unwrap();
        let content = std::fs::read_to_string(path).expect("log file written");
        assert!(content.contains("file-line"), "content: {content:?}");
        let _ = std::fs::remove_file(path);
    }

    // -- log capture helper ------------------------------------------------

    struct CaptureLogger;

    static CAPTURE_LOGGER: CaptureLogger = CaptureLogger;
    static CAPTURED: Mutex<Vec<(log::Level, String)>> = Mutex::new(Vec::new());
    static LOGGER_ONCE: std::sync::Once = std::sync::Once::new();

    impl log::Log for CaptureLogger {
        fn enabled(&self, _metadata: &log::Metadata) -> bool {
            true
        }
        fn log(&self, record: &log::Record) {
            CAPTURED
                .lock()
                .expect("capture mutex")
                .push((record.level(), record.args().to_string()));
        }
        fn flush(&self) {}
    }

    fn install_capture_logger() {
        LOGGER_ONCE.call_once(|| {
            log::set_logger(&CAPTURE_LOGGER).expect("install capture logger");
            log::set_max_level(log::LevelFilter::Trace);
        });
    }
}
