//! Small marshalling helpers shared by the raw native callbacks.
//!
//! The C ABI hands native callbacks a `argc`/`argv` pair, an out slot and an
//! error slot; these helpers convert the common cases without repeating the
//! unsafe pointer code in every native.
#![allow(unsafe_code)]

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::ptr;
use std::slice;

use tjs::{
    NativeObjThisMethod, NativeObjThisMethodDef, NativeObjThisProperty, NativeObjThisPropertyDef,
    Tjs2Engine, VAL_INTEGER, VAL_VOID, Value, tjs2_malloc,
};

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

/// Attach static (class-level) members to an existing TJS2 builtin class
/// object such as `Dictionary`.
///
/// The reference declares these with `TJS_STATICMEMBER`
/// (`reference/cpp/plugins/saveStruct.cpp:364-366` via the
/// `ncbAttachTJS2Class` registration path in
/// `reference/cpp/core/plugin/ncbind.hpp:2029-2033`): the members live on the
/// class object, are reachable through it, and are not copied onto instances,
/// so an instance's `EnumMembers` never reports them.
///
/// The C++ entry point is `tjs2_register_native_builtin_static_members` in
/// `cpp/tjs2_abi.cpp`; it reuses the objthis dispatch so the callbacks receive
/// the receiver the member was invoked on, but marks the symbol static.
pub(crate) fn register_builtin_static_members(
    engine: &Tjs2Engine,
    class_name: &str,
    methods: &[NativeObjThisMethodDef],
    properties: &[NativeObjThisPropertyDef],
) -> Result<(), String> {
    let class_name_c = CString::new(class_name)
        .map_err(|_| format!("class name contains a NUL byte: {class_name:?}"))?;
    let method_names: Vec<CString> = methods
        .iter()
        .map(|m| {
            CString::new(m.name)
                .map_err(|_| format!("method name contains a NUL byte: {:?}", m.name))
        })
        .collect::<Result<_, _>>()?;
    let c_methods: Vec<NativeObjThisMethod> = methods
        .iter()
        .zip(&method_names)
        .map(|(m, n)| NativeObjThisMethod {
            name: n.as_ptr(),
            f: m.f,
        })
        .collect();
    let property_names: Vec<CString> = properties
        .iter()
        .map(|p| {
            CString::new(p.name)
                .map_err(|_| format!("property name contains a NUL byte: {:?}", p.name))
        })
        .collect::<Result<_, _>>()?;
    let c_properties: Vec<NativeObjThisProperty> = properties
        .iter()
        .zip(&property_names)
        .map(|(p, n)| NativeObjThisProperty {
            name: n.as_ptr(),
            get: p.get,
            set: p.set,
        })
        .collect();
    // SAFETY: the engine is live, the names and arrays are valid for the call,
    // and the C++ side copies what it needs during registration.
    let rc = unsafe {
        tjs2_register_native_builtin_static_members(
            engine.raw() as *mut c_void,
            class_name_c.as_ptr(),
            if c_methods.is_empty() {
                ptr::null()
            } else {
                c_methods.as_ptr()
            },
            c_methods.len() as c_int,
            if c_properties.is_empty() {
                ptr::null()
            } else {
                c_properties.as_ptr()
            },
            c_properties.len() as c_int,
        )
    };
    if rc != 0 {
        return Err(format!(
            "failed to register static members on builtin class '{class_name}' (error {rc})"
        ));
    }
    Ok(())
}

unsafe extern "C" {
    /// `cpp/tjs2_abi.cpp`: attach `TJS_STATICMEMBER` objthis members to the
    /// builtin class object named by `class_name_utf8`. Returns 0 on success,
    /// non-zero on failure.
    fn tjs2_register_native_builtin_static_members(
        e: *mut c_void,
        class_name_utf8: *const c_char,
        methods: *const NativeObjThisMethod,
        count: c_int,
        properties: *const NativeObjThisProperty,
        prop_count: c_int,
    ) -> c_int;
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
