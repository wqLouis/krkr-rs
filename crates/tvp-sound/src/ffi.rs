//! FFI plumbing for the TVP sound native classes: marshalling `tjs2_value`
//! arguments/results and error strings across the C ABI.
//!
//! The same conventions as `crates/tvp-natives`: errors are malloc'd
//! NUL-terminated UTF-8 strings freed by the C++ trampoline
//! (`tjs2_free_string`), string results are written into a thread-local
//! buffer that stays valid for the duration of the callback, and a panic is
//! never allowed to cross the `extern "C"` boundary.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::slice;

use tjs2_sys::{DetachedValue, VAL_INTEGER, VAL_REAL, VAL_STRING, VAL_VOID, Value, tjs2_malloc};

/// The argument slice of a native method call.
///
/// # Safety
/// The C++ trampoline guarantees `argc` valid `Value` entries at `argv` for
/// the duration of the call.
pub fn args<'a>(argv: *const Value, argc: c_int) -> &'a [Value] {
    if argc <= 0 || argv.is_null() {
        return &[];
    }
    // SAFETY: see doc comment.
    unsafe { slice::from_raw_parts(argv, argc as usize) }
}

/// Convert an argument to its string form (strings pass through, integers
/// and reals use their decimal representation, void/objects become `""`).
pub fn value_as_string(v: &Value) -> String {
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

/// Convert an argument to a real (TJS `AsReal` semantics: integers widen,
/// strings parse as best effort).
pub fn value_as_f64(v: &Value) -> f64 {
    match v.ty {
        VAL_INTEGER => v.integer as f64,
        VAL_REAL => v.real,
        VAL_STRING => value_as_string(v).parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Convert an argument to a boolean (TJS `operator bool` semantics):
/// integers/reals are non-zero, strings are non-empty, void is false,
/// objects are true.
pub fn value_as_bool(v: &Value) -> bool {
    match v.ty {
        VAL_INTEGER => v.integer != 0,
        VAL_REAL => v.real != 0.0,
        VAL_STRING => !value_as_string(v).is_empty(),
        VAL_VOID => false,
        _ => true,
    }
}

/// Report a native error: point `*out_error` at a malloc'd NUL-terminated
/// message (the C++ side frees it with `tjs2_free_string`) and return the
/// non-zero status code.
pub fn report_error(out_error: *mut *mut c_char, msg: &str) -> c_int {
    // SAFETY: out_error points at a valid char* slot for the duration of the
    // call.
    unsafe { *out_error = alloc_error_string(msg) };
    1
}

/// Build a malloc'd NUL-terminated UTF-8 error message (freed on the C++
/// side with `tjs2_free_string`).
pub fn alloc_error_string(msg: &str) -> *mut c_char {
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

thread_local! {
    /// Buffer holding the current string return value (valid until the
    /// next `set_string_out` call on this thread).
    static STRING_OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Write `s` into `*out` as a string return value.
pub fn set_string_out(out: *mut Value, s: &str) {
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
pub fn set_int_out(out: *mut Value, v: i64) {
    // SAFETY: out is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_INTEGER;
        (*out).integer = v;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

/// Write a real return value into `*out`.
pub fn set_real_out(out: *mut Value, v: f64) {
    // SAFETY: out is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_REAL;
        (*out).integer = 0;
        (*out).real = v;
        (*out).string = ptr::null();
    }
}

/// Hand a retained TJS object back as the callback result.
///
/// The C++ trampoline consumes the retained id while copying the result,
/// so the Rust owner is forgotten after the id is placed in `out` (same
/// pattern as `tvp-natives::set_object_result`).
pub fn set_retained_out(out: *mut Value, dv: DetachedValue) {
    let id = dv.raw_id();
    // SAFETY: out is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = tjs2_sys::VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = id as usize;
    }
    // The retained id now belongs to the result value.
    std::mem::forget(dv);
}

/// Write an empty TJS array return value into `*out` (the C++ side copies
/// the elements into a real TJS array before the callback returns; with
/// zero elements it produces `[]`).
pub fn set_empty_array_out(out: *mut Value) {
    // SAFETY: out is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = tjs2_sys::VAL_ARRAY;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
    }
}

/// Clear the return slot (void return).
pub fn set_void_out(out: *mut Value) {
    // SAFETY: out is a valid return slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

/// Cast a native instance payload pointer to the Rust payload type.
/// The C++ dispatcher guarantees `inst` is the payload produced by the
/// matching create callback before any method runs.
pub fn instance_ptr<T>(inst: *mut c_void) -> *mut T {
    inst as *mut T
}
