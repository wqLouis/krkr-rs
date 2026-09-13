//! FFI marshaling helpers shared by the visual natives.
//!
//! Mirrors the helpers in `tvp-natives`/`tvp-storages` (the C++ trampoline
//! owns error strings allocated with `tjs2_malloc` and frees them with
//! `tjs2_free_string`; string return values live in a thread-local buffer
//! that stays valid until the next callback on the same thread).

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;

use tjs2_sys::{VAL_INTEGER, VAL_REAL, VAL_STRING, VAL_VOID, Value, tjs2_malloc};

thread_local! {
    /// Scratch buffer for string return values (`out.string`). Stays valid
    /// until the next native call on this thread — long enough, since the
    /// C++ side copies the string immediately after the callback returns.
    static STRING_OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// View the `argc` callback arguments as a slice (empty for null/zero).
///
/// # Safety
/// `argv` must point to `argc` valid `Value` entries for the duration of the
/// call (the C++ trampoline guarantees this).
pub(crate) unsafe fn args<'a>(argc: c_int, argv: *const Value) -> &'a [Value] {
    if argc <= 0 || argv.is_null() {
        return &[];
    }
    // SAFETY: the trampoline guarantees `argc` valid entries at `argv`.
    unsafe { std::slice::from_raw_parts(argv, argc as usize) }
}

/// Build a malloc'd NUL-terminated UTF-8 error message for `*out_error`
/// (the C++ trampoline frees it with `tjs2_free_string`).
pub(crate) fn alloc_error_string(msg: &str) -> *mut c_char {
    let bytes = msg.as_bytes();
    // SAFETY: tjs2_malloc is malloc-compatible; we write a NUL-terminated
    // copy the C++ side owns and frees with tjs2_free_string.
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

/// Set `*out_error` to a malloc'd copy of `msg` and return the non-zero
/// native status code the trampoline turns into a catchable TJS exception.
pub(crate) fn error_out(out_error: *mut *mut c_char, msg: &str) -> c_int {
    // SAFETY: out_error points at a `char*` slot owned by the trampoline
    // for the duration of the call.
    unsafe { *out_error = alloc_error_string(msg) };
    1
}

/// Write an integer return value into `*out`.
pub(crate) fn set_int_out(out: *mut Value, v: i64) {
    // SAFETY: `out` is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_INTEGER;
        (*out).integer = v;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

/// Write a real (floating-point) return value into `*out`.
pub(crate) fn set_real_out(out: *mut Value, v: f64) {
    // SAFETY: `out` is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = tjs2_sys::VAL_REAL;
        (*out).integer = 0;
        (*out).real = v;
        (*out).string = ptr::null();
    }
}

/// Write a void return value into `*out`.
pub(crate) fn set_void_out(out: *mut Value) {
    // SAFETY: `out` is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

/// Write a string return value into `*out` via the thread-local buffer.
pub(crate) fn set_string_out(out: *mut Value, s: &str) {
    STRING_OUT.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.clear();
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
        // SAFETY: `out` is a valid return slot for the duration of the call.
        unsafe {
            (*out).ty = VAL_STRING;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = buf.as_ptr() as *const c_char;
        }
    });
}

/// TJS `AsInteger` semantics for a callback argument.
pub(crate) fn arg_i64(v: &Value) -> i64 {
    match v.ty {
        VAL_INTEGER => v.integer,
        VAL_REAL => v.real as i64,
        VAL_STRING => arg_string(v).parse().unwrap_or(0),
        _ => 0,
    }
}

/// TJS `AsReal` semantics for a callback argument.
pub(crate) fn arg_f64(v: &Value) -> f64 {
    match v.ty {
        VAL_REAL => v.real,
        VAL_INTEGER => v.integer as f64,
        VAL_STRING => arg_string(v).parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// TJS `operator bool` semantics for a callback argument.
pub(crate) fn arg_bool(v: &Value) -> bool {
    match v.ty {
        VAL_INTEGER => v.integer != 0,
        VAL_REAL => v.real != 0.0,
        VAL_STRING => !arg_string(v).is_empty(),
        VAL_VOID => false,
        _ => true,
    }
}

/// A callback argument as a string (void/objects become `""`, matching the
/// reference's `ttstr(variant)` coercion for the common cases).
pub(crate) fn arg_string(v: &Value) -> String {
    match v.ty {
        VAL_STRING if v.string.is_null() => String::new(),
        VAL_STRING => {
            // SAFETY: the C++ side marshals strings as NUL-terminated UTF-8
            // valid for the duration of the call.
            let s = unsafe { CStr::from_ptr(v.string) };
            s.to_string_lossy().into_owned()
        }
        VAL_INTEGER => v.integer.to_string(),
        VAL_REAL => v.real.to_string(),
        _ => String::new(),
    }
}

/// Dereference a native instance payload pointer.
///
/// # Safety
/// `instance` must be the `Box::into_raw` pointer produced by the matching
/// create callback (the trampoline guarantees it is, for objects of this
/// native class).
pub(crate) unsafe fn instance_ref<'a, T>(instance: *mut c_void) -> &'a mut T {
    // SAFETY: caller upholds the payload contract.
    unsafe { &mut *(instance as *mut T) }
}
