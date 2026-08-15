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

/// A native method implemented in Rust, invoked by the VM when a script
/// calls `ClassName.method(...)`.
///
/// Returns 0 on success (and fills `out`); on error returns non-zero and
/// should set `*out_error` to a malloc'd NUL-terminated UTF-8 message
/// (allocate with the C-side `tjs2_malloc` helper exposed in the `unsafe
/// extern` block; the C++ side frees it with `tjs2_free_string`). Setting
/// `*out_error` is optional — a generic message is used when it is left
/// null.
///
/// `argv` points to `argc` [`Value`] entries and is only valid during the
/// call. `out` receives the return value; strings written there (as
/// `out.string`) must stay valid until the callback returns and the C++
/// side copies them — use a static/thread-local buffer, or storage that
/// outlives the call. `engine` is the opaque engine handle the method was
/// registered on (unused for static methods).
///
/// The callback runs on the VM thread and must not panic (a panic across
/// the `extern "C"` boundary aborts the process).
pub type NativeMethodFn = extern "C" fn(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int;

/// C-side mirror of `tjs2_native_method` (cpp/tjs2_abi.h).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NativeMethod {
    pub name: *const c_char,
    pub f: NativeMethodFn,
}

/// A native property accessor pair implemented in Rust (see
/// [`NativePropertyDef`]). Mirrors `tjs2_native_property` (cpp/tjs2_abi.h).
pub type NativePropertyGetFn =
    extern "C" fn(engine: *mut c_void, out: *mut Value, out_error: *mut *mut c_char) -> c_int;

/// Setter half of a native property. `value` is valid only during the call.
pub type NativePropertySetFn =
    extern "C" fn(engine: *mut c_void, value: *const Value, out_error: *mut *mut c_char) -> c_int;

/// C-side mirror of `tjs2_native_property` (cpp/tjs2_abi.h). `get`/`set` are
/// null when the property is write-only/read-only respectively.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NativeProperty {
    pub name: *const c_char,
    pub get: Option<NativePropertyGetFn>,
    pub set: Option<NativePropertySetFn>,
}

/// Allocate the Rust-owned payload of a new instance of a native class.
/// Called by the VM for every `new ClassName(...)`; must return non-null.
pub type NativeCreateInstanceFn = extern "C" fn(engine: *mut c_void) -> *mut c_void;

/// Free a payload created by [`NativeCreateInstanceFn`], called exactly once
/// when the TJS object is destroyed.
pub type NativeDestroyInstanceFn = extern "C" fn(engine: *mut c_void, instance: *mut c_void);

/// An instance method implemented in Rust: like [`NativeMethodFn`], plus
/// `instance` — the opaque payload of the object the method was called on.
pub type NativeInstanceMethodFn = extern "C" fn(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int;

/// C-side mirror of `tjs2_native_instance_method` (cpp/tjs2_abi.h).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NativeInstanceMethod {
    pub name: *const c_char,
    pub f: NativeInstanceMethodFn,
}

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
    fn tjs2_register_native_class_ex(
        e: *mut Engine,
        class_name: *const c_char,
        methods: *const NativeMethod,
        count: c_int,
        properties: *const NativeProperty,
        prop_count: c_int,
    ) -> c_int;
    fn tjs2_register_native_class_instance(
        e: *mut Engine,
        class_name: *const c_char,
        methods: *const NativeInstanceMethod,
        count: c_int,
        create_instance: NativeCreateInstanceFn,
        destroy_instance: NativeDestroyInstanceFn,
    ) -> c_int;
    /// malloc-compatible allocation (for building error strings on the Rust
    /// side; free with tjs2_free_string).
    pub fn tjs2_malloc(size: usize) -> *mut c_void;
}

// ---------------------------------------------------------------------------
// Safe wrapper
// ---------------------------------------------------------------------------

/// A handle to one TJS2 script engine instance.
///
/// Thread-safety: the tjs2 VM keeps process-global state (script cache,
/// string interning, ...), so engines are NOT safe to use from multiple
/// threads concurrently. The `Send`/`Sync` impls below are sound only
/// because krkr-rs confines all VM use to a single thread (see AGENT.md
/// "The TJS2 VM is not thread-safe"). Do not create engines on multiple
/// threads at the same time.
pub struct Tjs2Engine {
    inner: *mut Engine,
}

// SAFETY: process-wide VM globals mean engines must never run concurrently;
// krkr-rs guarantees a single VM thread by construction (and tests run with
// --test-threads=1).
unsafe impl Send for Tjs2Engine {}
unsafe impl Sync for Tjs2Engine {}

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

/// Definition of one native method to attach to a class.
pub struct NativeMethodDef {
    pub name: &'static str,
    pub f: NativeMethodFn,
}

/// Definition of one native property to attach to a class. Both `get` and
/// `set` are optional: a property without a getter reads as Void, one
/// without a setter raises an access-denied error on writes.
pub struct NativePropertyDef {
    pub name: &'static str,
    pub get: Option<NativePropertyGetFn>,
    pub set: Option<NativePropertySetFn>,
}

/// Definition of one instance method to attach to a class.
pub struct NativeInstanceMethodDef {
    pub name: &'static str,
    pub f: NativeInstanceMethodFn,
}

/// Describes a native class to register on the VM global object (see
/// [`Tjs2Engine::register_native_class`]).
///
/// The VM is single-threaded: register classes only from the thread that
/// owns the engine.
pub struct NativeClassBuilder<'a> {
    pub name: &'a str,
    pub methods: Vec<NativeMethodDef>,
    pub properties: Vec<NativePropertyDef>,
}

/// Describes an instance-based native class (see
/// [`Tjs2Engine::register_native_class_instance`]): `new ClassName()` creates
/// an object backed by a Rust-owned payload produced by `create` and freed by
/// `destroy` exactly once when the object is destroyed.
///
/// The VM is single-threaded: register classes only from the thread that
/// owns the engine.
pub struct NativeInstanceBuilder<'a> {
    pub name: &'a str,
    pub create: NativeCreateInstanceFn,
    pub destroy: NativeDestroyInstanceFn,
    pub methods: Vec<NativeInstanceMethodDef>,
}

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

    /// Register a native class on the VM global object so scripts can call
    /// `ClassName.method(...)` and read/write `ClassName.prop`.
    ///
    /// Methods are registered as static members and properties as static,
    /// class-level members (instance semantics are handled by
    /// [`Self::register_native_class_instance`]). The class and its members
    /// are owned by the engine and released when it is dropped.
    pub fn register_native_class(&self, builder: &NativeClassBuilder) -> Result<(), String> {
        let class_name = CString::new(builder.name)
            .map_err(|_| format!("class name contains a NUL byte: {:?}", builder.name))?;
        // Method and property names must stay alive for the duration of the
        // C call.
        let method_names: Vec<CString> = builder
            .methods
            .iter()
            .map(|m| {
                CString::new(m.name)
                    .map_err(|_| format!("method name contains a NUL byte: {:?}", m.name))
            })
            .collect::<Result<_, _>>()?;
        let c_methods: Vec<NativeMethod> = builder
            .methods
            .iter()
            .zip(&method_names)
            .map(|(m, n)| NativeMethod {
                name: n.as_ptr(),
                f: m.f,
            })
            .collect();
        let property_names: Vec<CString> = builder
            .properties
            .iter()
            .map(|p| {
                CString::new(p.name)
                    .map_err(|_| format!("property name contains a NUL byte: {:?}", p.name))
            })
            .collect::<Result<_, _>>()?;
        let c_properties: Vec<NativeProperty> = builder
            .properties
            .iter()
            .zip(&property_names)
            .map(|(p, n)| NativeProperty {
                name: n.as_ptr(),
                get: p.get,
                set: p.set,
            })
            .collect();
        // SAFETY: self.inner is a valid engine; the names and arrays are
        // valid for the call. The C++ side copies everything it needs
        // (names and callbacks) during registration.
        let rc = unsafe {
            tjs2_register_native_class_ex(
                self.inner,
                class_name.as_ptr(),
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
                "failed to register native class '{}' (error {rc})",
                builder.name
            ));
        }
        Ok(())
    }

    /// Register an instance-based native class on the VM global object.
    /// Scripts create objects with `new ClassName()`; each object carries the
    /// Rust-owned payload returned by `builder.create`, which is passed to
    /// every method call and released with `builder.destroy` when the object
    /// is destroyed. Methods are instance members: calling one on an object
    /// of a different native class (or on the class itself) raises a TJS
    /// error.
    pub fn register_native_class_instance(
        &self,
        builder: &NativeInstanceBuilder,
    ) -> Result<(), String> {
        let class_name = CString::new(builder.name)
            .map_err(|_| format!("class name contains a NUL byte: {:?}", builder.name))?;
        let method_names: Vec<CString> = builder
            .methods
            .iter()
            .map(|m| {
                CString::new(m.name)
                    .map_err(|_| format!("method name contains a NUL byte: {:?}", m.name))
            })
            .collect::<Result<_, _>>()?;
        let c_methods: Vec<NativeInstanceMethod> = builder
            .methods
            .iter()
            .zip(&method_names)
            .map(|(m, n)| NativeInstanceMethod {
                name: n.as_ptr(),
                f: m.f,
            })
            .collect();
        // SAFETY: self.inner is a valid engine; the names and array are
        // valid for the call. The C++ side copies everything it needs
        // (names and callbacks) during registration.
        let rc = unsafe {
            tjs2_register_native_class_instance(
                self.inner,
                class_name.as_ptr(),
                if c_methods.is_empty() {
                    ptr::null()
                } else {
                    c_methods.as_ptr()
                },
                c_methods.len() as c_int,
                builder.create,
                builder.destroy,
            )
        };
        if rc != 0 {
            return Err(format!(
                "failed to register native instance class '{}' (error {rc})",
                builder.name
            ));
        }
        Ok(())
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

    // -------------------------------------------------------------------
    // native class support
    // -------------------------------------------------------------------

    /// Build a malloc'd NUL-terminated UTF-8 error message for `*out_error`
    /// (the C++ side frees it with tjs2_free_string).
    fn alloc_error_string(msg: &str) -> *mut c_char {
        let bytes = msg.as_bytes();
        // SAFETY: tjs2_malloc is malloc; we write a NUL-terminated copy and
        // the C++ trampoline frees it with tjs2_free_string.
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
        /// Scratch buffer for `out.string`: stays valid until the next
        /// callback on this thread, which is long enough — the C++ side
        /// copies the string immediately after the callback returns.
        static STRING_OUT: std::cell::RefCell<Vec<u8>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// Write `s` into `*out` as a string, using the thread-local buffer.
    fn set_string_out(out: *mut Value, s: &str) {
        STRING_OUT.with(|buf| {
            let mut buf = buf.borrow_mut();
            buf.clear();
            buf.extend_from_slice(s.as_bytes());
            buf.push(0);
            // SAFETY: out is a valid slot provided by the C++ trampoline.
            unsafe {
                (*out).ty = VAL_STRING;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = buf.as_ptr() as *const c_char;
            }
        });
    }

    /// `TestNatives.add(a, b)`: integer sum; errors on argc < 2 or non-
    /// integer arguments.
    extern "C" fn native_add(
        _engine: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int {
        if argc < 2 {
            // SAFETY: out_error points at a valid char* slot for the call.
            unsafe { *out_error = alloc_error_string("add requires 2 arguments") };
            return 1;
        }
        // SAFETY: argv is valid for argc entries during the call.
        let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
        if args[0].ty != VAL_INTEGER || args[1].ty != VAL_INTEGER {
            unsafe { *out_error = alloc_error_string("add expects integer arguments") };
            return 1;
        }
        let sum = args[0].integer + args[1].integer;
        // SAFETY: out is a valid return slot for the call.
        unsafe {
            (*out).ty = VAL_INTEGER;
            (*out).integer = sum;
            (*out).real = 0.0;
            (*out).string = ptr::null();
        }
        0
    }

    /// `TestNatives.greet(name)`: returns "hello " + name; errors on argc
    /// < 1 or a non-string argument.
    extern "C" fn native_greet(
        _engine: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int {
        if argc < 1 {
            unsafe { *out_error = alloc_error_string("greet requires 1 argument") };
            return 1;
        }
        let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
        if args[0].ty != VAL_STRING {
            unsafe { *out_error = alloc_error_string("greet expects a string argument") };
            return 1;
        }
        // SAFETY: args[0].string is NUL-terminated UTF-8 for the call.
        let name = unsafe { CStr::from_ptr(args[0].string) }.to_string_lossy();
        set_string_out(out, &format!("hello {name}"));
        0
    }

    /// `TestNatives.half(x)`: returns x / 2 as a real.
    extern "C" fn native_half(
        _engine: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        if argc < 1 {
            return 1;
        }
        let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
        let v = if args[0].ty == VAL_REAL {
            args[0].real / 2.0
        } else {
            args[0].integer as f64 / 2.0
        };
        unsafe {
            (*out).ty = VAL_REAL;
            (*out).integer = 0;
            (*out).real = v;
            (*out).string = ptr::null();
        }
        0
    }

    /// `TestNatives.nop()`: returns nothing (void).
    extern "C" fn native_nop(
        _engine: *mut c_void,
        _argc: c_int,
        _argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        unsafe {
            (*out).ty = VAL_VOID;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
        }
        0
    }

    fn test_natives_builder() -> NativeClassBuilder<'static> {
        NativeClassBuilder {
            name: "TestNatives",
            methods: vec![
                NativeMethodDef {
                    name: "add",
                    f: native_add,
                },
                NativeMethodDef {
                    name: "greet",
                    f: native_greet,
                },
                NativeMethodDef {
                    name: "half",
                    f: native_half,
                },
                NativeMethodDef {
                    name: "nop",
                    f: native_nop,
                },
            ],
            properties: Vec::new(),
        }
    }

    #[test]
    fn native_class_static_methods_work_from_scripts() {
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&test_natives_builder()).unwrap();

        assert_eq!(
            e.eval("TestNatives.add(2, 3)", "test").unwrap(),
            TjsValue::Integer(5)
        );
        assert_eq!(
            e.eval("TestNatives.greet('x')", "test").unwrap(),
            TjsValue::String("hello x".into())
        );
        // UTF-8 round-trips both ways.
        assert_eq!(
            e.eval("TestNatives.greet('世界')", "test").unwrap(),
            TjsValue::String("hello 世界".into())
        );
        // real and void return paths
        assert_eq!(
            e.eval("TestNatives.half(5.0)", "test").unwrap(),
            TjsValue::Real(2.5)
        );
        assert_eq!(e.eval("TestNatives.nop()", "test").unwrap(), TjsValue::Void);
        // usable from a longer script, not just a bare expression
        e.exec_script("var r = TestNatives.add(10, 32); r *= 2;", "test")
            .unwrap();
        assert_eq!(e.eval("r", "test").unwrap(), TjsValue::Integer(84));
    }

    #[test]
    fn native_method_error_surfaces_as_tjs_error() {
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&test_natives_builder()).unwrap();

        // Error returned from Rust → error from eval, with the Rust message.
        let err = e.eval("TestNatives.add(1)", "test").unwrap_err();
        assert!(
            err.to_string().contains("add requires 2 arguments"),
            "unexpected error: {err}"
        );

        // ... and catchable from a script via try/catch (TJS requires `;`
        // after expression statements inside blocks).
        e.exec_script(
            "var r = ''; try { TestNatives.add(1); } catch(e) { r = 'caught'; }",
            "test",
        )
        .unwrap();
        assert_eq!(
            e.eval("r", "test").unwrap(),
            TjsValue::String("caught".into())
        );
    }

    #[test]
    fn native_class_rejects_duplicate_registration() {
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&test_natives_builder()).unwrap();
        let err = e
            .register_native_class(&test_natives_builder())
            .unwrap_err();
        assert!(err.contains("TestNatives"), "unexpected error: {err}");
    }

    // -------------------------------------------------------------------
    // native properties
    // -------------------------------------------------------------------

    /// `Prop.value`: get+set, backed by process-wide statics (the callbacks
    /// carry no user-data slot, so state lives in statics; the VM is
    /// single-threaded).
    static PROP_VALUE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);
    static PROP_READONLY: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(7);
    static PROP_WRITEONLY: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    extern "C" fn prop_get_value(
        _engine: *mut c_void,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        unsafe {
            (*out).ty = VAL_INTEGER;
            (*out).integer = PROP_VALUE.load(std::sync::atomic::Ordering::SeqCst);
            (*out).real = 0.0;
            (*out).string = ptr::null();
        }
        0
    }

    extern "C" fn prop_set_value(
        _engine: *mut c_void,
        value: *const Value,
        out_error: *mut *mut c_char,
    ) -> c_int {
        // SAFETY: value points at a valid tjs2_value for the call.
        let v = unsafe { &*value };
        if v.ty != VAL_INTEGER {
            unsafe { *out_error = alloc_error_string("Prop.value expects an integer") };
            return 1;
        }
        PROP_VALUE.store(v.integer, std::sync::atomic::Ordering::SeqCst);
        0
    }

    extern "C" fn prop_get_readonly(
        _engine: *mut c_void,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        unsafe {
            (*out).ty = VAL_INTEGER;
            (*out).integer = PROP_READONLY.load(std::sync::atomic::Ordering::SeqCst);
            (*out).real = 0.0;
            (*out).string = ptr::null();
        }
        0
    }

    extern "C" fn prop_set_writeonly(
        _engine: *mut c_void,
        value: *const Value,
        out_error: *mut *mut c_char,
    ) -> c_int {
        let v = unsafe { &*value };
        if v.ty != VAL_INTEGER {
            unsafe { *out_error = alloc_error_string("Prop.writeonly expects an integer") };
            return 1;
        }
        PROP_WRITEONLY.store(v.integer, std::sync::atomic::Ordering::SeqCst);
        0
    }

    fn prop_builder() -> NativeClassBuilder<'static> {
        NativeClassBuilder {
            name: "Prop",
            methods: Vec::new(),
            properties: vec![
                NativePropertyDef {
                    name: "value",
                    get: Some(prop_get_value),
                    set: Some(prop_set_value),
                },
                NativePropertyDef {
                    name: "readonly",
                    get: Some(prop_get_readonly),
                    set: None,
                },
                NativePropertyDef {
                    name: "writeonly",
                    get: None,
                    set: Some(prop_set_writeonly),
                },
            ],
        }
    }

    #[test]
    fn native_properties_work_from_scripts() {
        PROP_VALUE.store(0, std::sync::atomic::Ordering::SeqCst);
        PROP_READONLY.store(7, std::sync::atomic::Ordering::SeqCst);
        PROP_WRITEONLY.store(0, std::sync::atomic::Ordering::SeqCst);

        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&prop_builder()).unwrap();

        // get roundtrip: initial value, then write, then read back.
        assert_eq!(e.eval("Prop.value", "test").unwrap(), TjsValue::Integer(0));
        e.eval("Prop.value = 42", "test").unwrap();
        assert_eq!(e.eval("Prop.value", "test").unwrap(), TjsValue::Integer(42));
        // setters return the assigned value, usable in longer expressions.
        e.exec_script("Prop.value = 10; var r = Prop.value * 2;", "test")
            .unwrap();
        assert_eq!(e.eval("r", "test").unwrap(), TjsValue::Integer(20));

        // read-only property: reads work, writes are a TJS error.
        assert_eq!(
            e.eval("Prop.readonly", "test").unwrap(),
            TjsValue::Integer(7)
        );
        let err = e.eval("Prop.readonly = 99", "test").unwrap_err();
        assert!(!err.to_string().is_empty(), "expected an error: {err}");

        // write-only property: reads yield Void, writes work.
        assert_eq!(e.eval("Prop.writeonly", "test").unwrap(), TjsValue::Void);
        e.eval("Prop.writeonly = 5", "test").unwrap();
        assert_eq!(PROP_WRITEONLY.load(std::sync::atomic::Ordering::SeqCst), 5);

        // a registered property deletes cleanly; accessing it afterwards is
        // a member-not-found error.
        e.exec_script("delete Prop.value;", "test").unwrap();
        let err = e.eval("Prop.value", "test").unwrap_err();
        assert!(!err.to_string().is_empty(), "expected an error: {err}");
    }

    #[test]
    fn native_property_set_error_surfaces_as_tjs_error() {
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&prop_builder()).unwrap();
        let err = e.eval("Prop.value = 'oops'", "test").unwrap_err();
        assert!(
            err.to_string().contains("expects an integer"),
            "unexpected error: {err}"
        );
    }

    // -------------------------------------------------------------------
    // native instances
    // -------------------------------------------------------------------

    static COUNTER_CREATED: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    static COUNTER_DESTROYED: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    /// Create a fresh heap i32 counter payload for `new Counter()`.
    extern "C" fn counter_create(_engine: *mut c_void) -> *mut c_void {
        COUNTER_CREATED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Box::into_raw(Box::new(0i32)) as *mut c_void
    }

    /// Free a counter payload (called exactly once per create).
    extern "C" fn counter_destroy(_engine: *mut c_void, instance: *mut c_void) {
        COUNTER_DESTROYED.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        // SAFETY: instance came from counter_create (Box::into_raw).
        unsafe { drop(Box::from_raw(instance as *mut i32)) };
    }

    /// Fetch the counter payload for the object a method was called on;
    /// never fails in these tests because the dispatcher already verified
    /// the object is a Counter instance.
    // SAFETY: instance is a valid Counter payload for the call.
    unsafe fn counter_ptr(instance: *mut c_void) -> *mut i32 {
        instance as *mut i32
    }

    /// `Counter.inc()`: no arguments, void return.
    extern "C" fn counter_inc(
        _engine: *mut c_void,
        instance: *mut c_void,
        _argc: c_int,
        _argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        // SAFETY: instance is a valid Counter payload for the call.
        unsafe { *counter_ptr(instance) += 1 };
        unsafe {
            (*out).ty = VAL_VOID;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
        }
        0
    }

    /// `Counter.get()`: returns the current value.
    extern "C" fn counter_get(
        _engine: *mut c_void,
        instance: *mut c_void,
        _argc: c_int,
        _argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        let v = unsafe { *counter_ptr(instance) };
        unsafe {
            (*out).ty = VAL_INTEGER;
            (*out).integer = v as i64;
            (*out).real = 0.0;
            (*out).string = ptr::null();
        }
        0
    }

    /// `Counter.add(n)`: adds an integer argument to the counter.
    extern "C" fn counter_add(
        _engine: *mut c_void,
        instance: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int {
        if argc < 1 {
            unsafe { *out_error = alloc_error_string("Counter.add requires 1 argument") };
            return 1;
        }
        let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
        if args[0].ty != VAL_INTEGER {
            unsafe { *out_error = alloc_error_string("Counter.add expects an integer argument") };
            return 1;
        }
        unsafe { *counter_ptr(instance) += args[0].integer as i32 };
        unsafe {
            (*out).ty = VAL_VOID;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
        }
        0
    }

    fn counter_builder() -> NativeInstanceBuilder<'static> {
        NativeInstanceBuilder {
            name: "Counter",
            create: counter_create,
            destroy: counter_destroy,
            methods: vec![
                NativeInstanceMethodDef {
                    name: "inc",
                    f: counter_inc,
                },
                NativeInstanceMethodDef {
                    name: "get",
                    f: counter_get,
                },
                NativeInstanceMethodDef {
                    name: "add",
                    f: counter_add,
                },
            ],
        }
    }

    #[test]
    fn native_instances_work_from_scripts() {
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();

        // inc() + add(41) + get() == 42, end to end from a script.
        e.exec_script(
            "var c = new Counter(); c.inc(); c.add(41); var r = c.get();",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("r", "test").unwrap(), TjsValue::Integer(42));

        // two instances are independent.
        e.exec_script(
            "var a = new Counter(); var b = new Counter(); a.add(10); b.add(20); \
var ra = a.get(); var rb = b.get();",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("ra", "test").unwrap(), TjsValue::Integer(10));
        assert_eq!(e.eval("rb", "test").unwrap(), TjsValue::Integer(20));
        // and instance state survives across eval/exec boundaries.
        e.exec_script("a.inc();", "test").unwrap();
        assert_eq!(e.eval("a.get()", "test").unwrap(), TjsValue::Integer(11));

        // methods are usable inside longer scripts (closures, arithmetic).
        e.exec_script(
            "var d = new Counter(); d.add(40); var rd = d.get() + 2;",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("rd", "test").unwrap(), TjsValue::Integer(42));
    }

    #[test]
    fn native_instance_method_on_wrong_object_errors() {
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();

        // An array has no `get` member at all -> member-not-found error.
        let err = e.eval("[1, 2].get()", "test").unwrap_err();
        assert!(!err.to_string().is_empty(), "expected an error: {err}");

        // Calling an instance method on the class object itself (no instance
        // is registered there) fails the native-instance lookup.
        let err = e.eval("Counter.get()", "test").unwrap_err();
        assert!(
            err.to_string().contains("not an instance"),
            "unexpected error: {err}"
        );

        // A missing method on a real instance is a member-not-found error.
        e.exec_script("var c = new Counter();", "test").unwrap();
        let err = e.eval("c.missing()", "test").unwrap_err();
        assert!(!err.to_string().is_empty(), "expected an error: {err}");
    }

    #[test]
    fn native_instance_method_error_surfaces_as_tjs_error() {
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();

        let err = e.eval("(new Counter()).add('x')", "test").unwrap_err();
        assert!(
            err.to_string().contains("expects an integer"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn native_instances_destroy_callback_runs() {
        COUNTER_CREATED.store(0, std::sync::atomic::Ordering::SeqCst);
        COUNTER_DESTROYED.store(0, std::sync::atomic::Ordering::SeqCst);

        for i in 0..200 {
            let e = Tjs2Engine::new().unwrap();
            e.register_native_class_instance(&counter_builder())
                .unwrap();
            // `c = null` drops the last script reference synchronously, so
            // the destroy callback must have run by the time the script
            // returns — no leaks, no double frees.
            e.exec_script("var c = new Counter(); c.inc(); c = null;", "test")
                .unwrap();
            assert_eq!(
                COUNTER_CREATED.load(std::sync::atomic::Ordering::SeqCst),
                COUNTER_DESTROYED.load(std::sync::atomic::Ordering::SeqCst),
                "create/destroy imbalance in iteration {i}"
            );
            // dropping the engine (class release, shutdown) must not crash.
        }
        assert_eq!(
            COUNTER_CREATED.load(std::sync::atomic::Ordering::SeqCst),
            200,
            "every engine should have created one counter"
        );
        assert_eq!(
            COUNTER_DESTROYED.load(std::sync::atomic::Ordering::SeqCst),
            200,
            "every counter payload should have been destroyed"
        );
    }
}
