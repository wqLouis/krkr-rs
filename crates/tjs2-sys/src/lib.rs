//! FFI bindings to the C++ TJS2 scripting VM.
//!
//! The C++ side is compiled by `build.rs` (bison + clang/gcc, no cmake) and
//! statically linked. This crate exposes a small, hand-written safe wrapper
//! over the C ABI defined in `cpp/tjs2_abi.h`.

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;

// ---------------------------------------------------------------------------
// C ABI mirror of cpp/tjs2_abi.h
// ---------------------------------------------------------------------------

pub const LOG_DEBUG: c_int = 0;
pub const LOG_INFO: c_int = 1;
pub const LOG_WARN: c_int = 2;
pub const LOG_ERROR: c_int = 3;

pub const VAL_VOID: c_int = 0;
pub const VAL_INTEGER: c_int = 1;
pub const VAL_REAL: c_int = 2;
pub const VAL_STRING: c_int = 3;
pub const VAL_OBJECT: c_int = 4;

/// Callback for console output / logs from the VM. `msg` is UTF-8 and only
/// valid for the duration of the call.
pub type LogCb = extern "C" fn(level: c_int, msg: *const c_char, user: *mut c_void);

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Value {
    pub ty: c_int,
    pub integer: i64,
    pub real: f64,
    pub string: *const c_char,
}

#[repr(C)]
pub struct Engine {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn tjs2_create() -> *mut Engine;
    fn tjs2_destroy(e: *mut Engine);
    fn tjs2_set_log_cb(e: *mut Engine, cb: LogCb, user: *mut c_void);
    fn tjs2_exec_script(
        e: *mut Engine,
        script: *const c_char,
        name: *const c_char,
        out_result: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    fn tjs2_eval(
        e: *mut Engine,
        expression: *const c_char,
        name: *const c_char,
        out_result: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    fn tjs2_free_string(s: *mut c_char);
}

// ---------------------------------------------------------------------------
// Safe wrapper
// ---------------------------------------------------------------------------

/// A handle to one TJS2 script engine instance.
///
/// NOT thread-safe: the tjs2 VM keeps global state (script cache, string
/// interning, ...), so engines must be confined to one thread and engines
/// in different threads must not exist concurrently. Do not implement
/// Send/Sync for this type.
pub struct Tjs2Engine {
    inner: *mut Engine,
}

/// Result of executing a script or evaluating an expression.
#[derive(Debug, Clone, PartialEq)]
pub enum TjsValue {
    Void,
    Integer(i64),
    Real(f64),
    String(String),
    Object,
}

/// Error raised by the VM during execution.
#[derive(Debug, thiserror::Error)]
#[error("TJS error: {0}")]
pub struct TjsError(String);

impl Tjs2Engine {
    /// Create a new script engine.
    pub fn new() -> Result<Self, &'static str> {
        // SAFETY: tjs2_create returns a heap-allocated engine or null.
        let inner = unsafe { tjs2_create() };
        if inner.is_null() {
            return Err("failed to create TJS2 engine");
        }
        Ok(Tjs2Engine { inner })
    }

    /// Install a console/log callback (replaces any previous one).
    ///
    /// # Safety
    /// `user` must stay valid (or be null) for as long as the callback can
    /// fire.
    pub unsafe fn set_log_cb(&self, cb: Option<LogCb>, user: *mut c_void) {
        // SAFETY: caller upholds the `user` validity contract.
        unsafe { tjs2_set_log_cb(self.inner, cb.unwrap_or(noop_log), user) }
    }

    /// Execute a TJS script given as UTF-8 text.
    pub fn exec_script(&self, script: &str, name: &str) -> Result<TjsValue, TjsError> {
        let script = CString::new(script).map_err(|_| TjsError("script contains NUL".into()))?;
        let name = CString::new(name).map_err(|_| TjsError("name contains NUL".into()))?;
        let mut result = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: all pointers point to valid, live data for the call.
        let rc = unsafe {
            tjs2_exec_script(
                self.inner,
                script.as_ptr(),
                name.as_ptr(),
                &mut result,
                &mut error,
            )
        };
        if rc != 0 {
            return Err(unsafe { take_error(error) });
        }
        Ok(unsafe { take_value(&result) })
    }

    /// Evaluate a TJS expression given as UTF-8 text.
    pub fn eval(&self, expression: &str, name: &str) -> Result<TjsValue, TjsError> {
        let expression =
            CString::new(expression).map_err(|_| TjsError("expression contains NUL".into()))?;
        let name = CString::new(name).map_err(|_| TjsError("name contains NUL".into()))?;
        let mut result = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: all pointers point to valid, live data for the call.
        let rc = unsafe {
            tjs2_eval(
                self.inner,
                expression.as_ptr(),
                name.as_ptr(),
                &mut result,
                &mut error,
            )
        };
        if rc != 0 {
            return Err(unsafe { take_error(error) });
        }
        Ok(unsafe { take_value(&result) })
    }
}

impl Drop for Tjs2Engine {
    fn drop(&mut self) {
        // SAFETY: self.inner is a valid engine created by tjs2_create.
        unsafe { tjs2_destroy(self.inner) };
    }
}

extern "C" fn noop_log(_level: c_int, _msg: *const c_char, _user: *mut c_void) {}

/// Convert a C value into an owned TjsValue. The string pointer is only valid
/// until the next engine call, so it is copied immediately.
///
/// # Safety
/// `v` must point to a valid tjs2_value filled by the C side.
unsafe fn take_value(v: *const Value) -> TjsValue {
    let v = unsafe { &*v };
    match v.ty {
        VAL_VOID => TjsValue::Void,
        VAL_INTEGER => TjsValue::Integer(v.integer),
        VAL_REAL => TjsValue::Real(v.real),
        VAL_STRING if !v.string.is_null() => {
            let s = unsafe { CStr::from_ptr(v.string) }
                .to_string_lossy()
                .into_owned();
            TjsValue::String(s)
        }
        VAL_STRING => TjsValue::String(String::new()),
        _ => TjsValue::Object,
    }
}

/// Take ownership of an error string returned by the C side.
///
/// # Safety
/// `e` must be either null or a malloc'd string owned by the C side.
unsafe fn take_error(e: *mut c_char) -> TjsError {
    if e.is_null() {
        return TjsError("unknown TJS error".into());
    }
    let msg = unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned();
    unsafe { tjs2_free_string(e) };
    TjsError(msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_creates_and_destroys() {
        let e = Tjs2Engine::new().expect("create engine");
        drop(e);
    }

    #[test]
    fn evaluates_integer_expression() {
        let e = Tjs2Engine::new().unwrap();
        let v = e.eval("1 + 2 * 3", "test").unwrap();
        assert_eq!(v, TjsValue::Integer(7));
    }

    #[test]
    fn evaluates_string_expression() {
        let e = Tjs2Engine::new().unwrap();
        let v = e.eval("'hello' + ' ' + 'world'", "test").unwrap();
        assert_eq!(v, TjsValue::String("hello world".into()));
    }

    #[test]
    fn executes_script_with_global_assignment() {
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var x = 40; x += 2;", "test").unwrap();
        let v = e.eval("x", "test").unwrap();
        assert_eq!(v, TjsValue::Integer(42));
    }

    #[test]
    fn script_error_is_reported() {
        let e = Tjs2Engine::new().unwrap();
        let err = e.exec_script("this is not valid tjs", "test").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn unicode_strings_roundtrip() {
        let e = Tjs2Engine::new().unwrap();
        let v = e.eval("'こんにちは' + '世界'", "test").unwrap();
        assert_eq!(v, TjsValue::String("こんにちは世界".into()));
    }
}
