//! Small marshalling helpers shared by the raw native callbacks.
//!
//! The C ABI hands native callbacks a `argc`/`argv` pair, an out slot and an
//! error slot; these helpers convert the common cases without repeating the
//! unsafe pointer code in every native.
#![allow(unsafe_code)]

use std::ffi::{CStr, c_char, c_int};
use std::ptr;
use std::slice;

use tjs::{VAL_INTEGER, VAL_VOID, Value, tjs2_malloc};

/// View a native callback's `argc`/`argv` as a slice.
///
/// Returns an empty slice for a null pointer or a non-positive count (the
/// C++ side passes a null `argv` when a method is called with no arguments).
///
/// # Safety
/// `argv` must point to `argc` valid [`Value`] entries that stay valid for
/// the returned slice's use (i.e. for the duration of the callback). The
/// returned lifetime is chosen by the caller, so a safe wrapper cannot
/// enforce it — hence this is `unsafe`.
pub(crate) unsafe fn args<'a>(argv: *const Value, argc: c_int) -> &'a [Value] {
    if argc <= 0 || argv.is_null() {
        return &[];
    }
    // SAFETY: the caller upholds the validity contract above.
    unsafe { slice::from_raw_parts(argv, argc as usize) }
}

/// Convert an argument to its string form (TJS `ttstr` coercion): strings
/// pass through, integers/reals use their decimal form, void and objects are
/// empty.
pub(crate) fn value_as_string(v: &Value) -> String {
    match v.ty {
        tjs::VAL_STRING if v.string.is_null() => String::new(),
        tjs::VAL_STRING => {
            // SAFETY: the C++ side guarantees a NUL-terminated UTF-8 string
            // valid for the duration of the callback.
            let s = unsafe { CStr::from_ptr(v.string) };
            s.to_string_lossy().into_owned()
        }
        VAL_INTEGER => v.integer.to_string(),
        tjs::VAL_REAL => v.real.to_string(),
        _ => String::new(),
    }
}

/// Write a void result into a native callback's out slot.
pub(crate) fn set_void(out: *mut Value) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Write an integer result into a native callback's out slot.
pub(crate) fn set_int(out: *mut Value, value: i64) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_INTEGER;
        (*out).integer = value;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Write a boolean result as a TJS integer (`true` = 1, `false` = 0).
pub(crate) fn set_bool(out: *mut Value, value: bool) {
    set_int(out, i64::from(value));
}

/// Set the native callback error slot and return the non-zero status code.
///
/// The C++ trampoline frees the message with `tjs2_free_string`.
pub(crate) fn report_error(out_error: *mut *mut c_char, msg: &str) -> c_int {
    if !out_error.is_null() {
        // SAFETY: out_error is a valid char* slot for the call.
        unsafe { *out_error = alloc_error_string(msg) };
    }
    1
}

/// Build a malloc-compatible NUL-terminated UTF-8 error message.
fn alloc_error_string(msg: &str) -> *mut c_char {
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

/// Serialize unit tests that create a TJS VM.
///
/// The `tjs` engine keeps process-wide C++ globals and `compat` installs a
/// single process-wide engine → context binding, so two `#[cfg(test)]`
/// modules that create an engine concurrently race. Integration tests use the
/// per-binary `common::vm_lock`; this is the in-crate counterpart for module
/// unit tests. It is a no-op outside `cfg(test)`.
#[cfg(test)]
pub(crate) fn test_vm_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}
