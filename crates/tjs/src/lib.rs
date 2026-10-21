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
pub const VAL_ARRAY: c_int = 5;
pub const VAL_RETAINED: c_int = 6;
/// Raw binary data (C++ `tvtOctet`). For values produced by the engine the
/// bytes live in `string` and the length in `array_count`; native callbacks
/// read them with [`Value::octet_bytes`].
pub const VAL_OCTET: c_int = 7;
/// TJS `null`: an object value with no object pointer (`tvtObject` with
/// `Object == nullptr`), distinct from [`VAL_VOID`] and from [`VAL_OBJECT`]
/// with a zero handle.
pub const VAL_NULL: c_int = 8;

/// Storage read status: the entry was read and `out_data`/`out_len` are set.
pub const STORAGE_READ_OK: c_int = 0;
/// Storage read status: the storage has no such entry, so the stream factory
/// falls back to the local filesystem.
pub const STORAGE_READ_NOT_FOUND: c_int = 1;
/// Storage read status: the lookup failed; `out_error` names why.
pub const STORAGE_READ_ERROR: c_int = 2;

/// Reads a whole storage entry for the text/binary stream factories (the C
/// ABI type in `cpp/tjs2_abi.h`).
///
/// `name`/`name_len` are UTF-8 and may carry the `archive.xp3>member` form.
/// On [`STORAGE_READ_OK`] the callback stores a malloc'd buffer in `out_data`
/// (the stream layer frees it with `tjs2_free_storage_bytes`) and its byte
/// length in `out_len`. It returns [`STORAGE_READ_NOT_FOUND`] when the storage
/// has no such entry, or [`STORAGE_READ_ERROR`] with a malloc'd UTF-8 message
/// in `out_error` (freed with `tjs2_free_string`).
pub type StorageReadFn = extern "C" fn(
    user: *mut c_void,
    name: *const c_char,
    name_len: usize,
    out_data: *mut *mut u8,
    out_len: *mut usize,
    out_error: *mut *mut c_char,
) -> c_int;

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

/// Tear down a payload's native resources when the TJS object is invalidated
/// (reference `iTJSNativeInstance::Invalidate`). Optional: `None` means the
/// payload is only released by [`NativeDestroyInstanceFn`]. The payload must
/// stay valid after this call — the destroy callback still runs later.
pub type NativeInvalidateInstanceFn = extern "C" fn(engine: *mut c_void, instance: *mut c_void);

/// An instance method implemented in Rust: like [`NativeMethodFn`], plus
/// `instance` — the opaque payload of the object the method was called on.
pub type NativeInstanceMethodFn = extern "C" fn(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int;

/// C-side mirror of `tjs2_native_instance_method` (cpp/tjs2_abi.h).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NativeInstanceMethod {
    pub name: *const c_char,
    pub f: NativeInstanceMethodFn,
}

/// An instance property implemented in Rust: like [`NativePropertyGetFn`]/
/// [`NativePropertySetFn`], plus `instance` — the payload of the object the
/// property was accessed on.
pub type NativeInstancePropertyGetFn = extern "C" fn(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int;

/// Setter half of an instance property. `value` is valid for the duration of
/// the call.
pub type NativeInstancePropertySetFn = extern "C" fn(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int;

/// C-side mirror of `tjs2_native_instance_property` (cpp/tjs2_abi.h).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NativeInstanceProperty {
    pub name: *const c_char,
    pub get: Option<NativeInstancePropertyGetFn>,
    pub set: Option<NativeInstancePropertySetFn>,
}

/// An instance member implemented in Rust that receives **no native payload**:
/// like [`NativeMethodFn`], plus `objthis` — the raw TJS object the member
/// was invoked on. Used by
/// [`Tjs2Engine::register_native_instance_members`] to attach members to an
/// already-existing class, including a TJS2 builtin such as `Array` /
/// `Dictionary` (reference `NCB_ATTACH_CLASS` / `NCB_ATTACH_FUNCTION`).
///
/// `objthis` is owned by the VM and only valid for the duration of the call;
/// unlike [`NativeInstanceMethodFn`] there is no per-class opaque payload.
pub type NativeObjThisMethodFn = extern "C" fn(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int;

/// C-side mirror of `tjs2_native_objthis_method` (cpp/tjs2_abi.h).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NativeObjThisMethod {
    pub name: *const c_char,
    pub f: NativeObjThisMethodFn,
}

/// Getter half of an objthis-only instance property (no native payload).
pub type NativeObjThisPropertyGetFn = extern "C" fn(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int;

/// Setter half of an objthis-only instance property (no native payload).
/// `value` is valid only during the call.
pub type NativeObjThisPropertySetFn = extern "C" fn(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int;

/// C-side mirror of `tjs2_native_objthis_property` (cpp/tjs2_abi.h).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NativeObjThisProperty {
    pub name: *const c_char,
    pub get: Option<NativeObjThisPropertyGetFn>,
    pub set: Option<NativeObjThisPropertySetFn>,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct Value {
    pub ty: c_int,
    pub integer: i64,
    pub real: f64,
    pub string: *const c_char,
    pub array: *const *const c_char,
    pub array_count: c_int,
    pub retained: usize,
}

impl Value {
    /// Raw TJS object pointer carried by a `VAL_OBJECT` argument, or null
    /// for any other value type.
    ///
    /// C++ `variant_to_value_one` records the raw `iTJSDispatch2*` of every
    /// object argument in the `retained` slot, so a native that receives
    /// several object arguments can address each one individually (e.g.
    /// `Layer.drawPolygon(app, points)`). The pointer is owned by the VM and
    /// is only guaranteed valid for the duration of the callback; use
    /// [`Tjs2Engine::retain_object_arg`] to keep the object past the call.
    pub fn object_handle(&self) -> *mut c_void {
        if self.ty == VAL_OBJECT {
            self.retained as *mut c_void
        } else {
            ptr::null_mut()
        }
    }

    /// The raw closure receiver (`objthis`) carried by a `VAL_OBJECT`, or
    /// null for a plain object.
    ///
    /// The C++ side stores a method/closure reference's receiver in the
    /// otherwise-unused `array` slot of the object value (see
    /// `object_handle`). Retaining the value through [`Tjs2Engine::retain_object_arg`]
    /// / [`retain_object_arg_raw`] then preserves the correct `this`, so a
    /// native that holds a method reference and calls it later (e.g. the
    /// game's `new AsyncTrigger(onCleaning, "")`) runs it on the right
    /// object. The pointer is owned by the VM and valid only for the call.
    pub fn object_objthis(&self) -> *mut c_void {
        if self.ty == VAL_OBJECT {
            self.array as *mut c_void
        } else {
            ptr::null_mut()
        }
    }

    /// Raw octet bytes of a `VAL_OCTET` value, or `None` for any other type.
    ///
    /// `array_count` is the byte length (the `VAL_OCTET` counterpart of the
    /// string length). An empty octet yields `None` because the engine cannot
    /// carry a zero-length payload pointer.
    ///
    /// # Safety
    /// The bytes are owned by the engine (or by the caller for the duration
    /// of the callback) and are only valid while the value is in scope — do
    /// not retain the returned slice past the callback that received it.
    pub unsafe fn octet_bytes(&self) -> Option<&[u8]> {
        if self.ty == VAL_OCTET && !self.string.is_null() && self.array_count > 0 {
            // SAFETY: the caller upholds the lifetime contract above; the
            // pointer is non-null and `array_count` bytes are readable.
            Some(unsafe {
                std::slice::from_raw_parts(self.string as *const u8, self.array_count as usize)
            })
        } else {
            None
        }
    }
}

#[repr(C)]
pub struct Engine {
    _private: [u8; 0],
}

unsafe extern "C" {
    fn tjs2_create() -> *mut Engine;
    fn tjs2_set_data_dir_checked(dir: *const c_char, out_error: *mut *mut c_char) -> c_int;
    fn tjs2_set_storage_read_callback(read: Option<StorageReadFn>, user: *mut c_void);
    fn tjs2_destroy(e: *mut Engine);
    fn tjs2_set_log_cb(e: *mut Engine, cb: LogCb, user: *mut c_void);
    fn tjs2_exec_script(
        e: *mut Engine,
        script: *const c_char,
        script_len: usize,
        name: *const c_char,
        context: *mut c_void,
        lineofs: c_int,
        out_result: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    fn tjs2_eval(
        e: *mut Engine,
        expression: *const c_char,
        expression_len: usize,
        name: *const c_char,
        context: *mut c_void,
        lineofs: c_int,
        out_result: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Execute a TJS script from UTF-16 code units: the lossless counterpart
    /// of `tjs2_exec_script`, so an unpaired surrogate survives the boundary.
    /// `script` points at `script_len` `u16` values.
    fn tjs2_exec_script_wide(
        e: *mut Engine,
        script: *const u16,
        script_len: usize,
        name: *const c_char,
        context: *mut c_void,
        lineofs: c_int,
        out_result: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Evaluate a TJS expression from UTF-16 code units; same conventions as
    /// `tjs2_exec_script_wide`.
    fn tjs2_eval_wide(
        e: *mut Engine,
        expression: *const u16,
        expression_len: usize,
        name: *const c_char,
        context: *mut c_void,
        lineofs: c_int,
        out_result: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Replace the text of a registered TJS message (reference
    /// `TJS::TJSAssignMessage`). Returns 1 when the message exists, 0 when it
    /// does not, and -1 on a C++ error (reported through `out_error`).
    fn tjs2_assign_message_checked(
        name: *const c_char,
        message: *const c_char,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// The vendored TJS2 engine version as a static `major.minor.release`
    /// string (reference `TJSVersionMajor`/`Minor`/`Release`).
    fn tjs2_tjs_version_string() -> *const c_char;
    /// The C++ compile date/time (`__DATE__`/`__TIME__`).
    fn tjs2_compile_date() -> *const c_char;
    fn tjs2_compile_time() -> *const c_char;
    /// Compile a UTF-8 script to a binary bytecode file (reference
    /// `tTJS::CompileScript` / `TVPCompileStorage`).
    pub fn tjs2_compile_script(
        e: *mut Engine,
        script: *const c_char,
        script_len: usize,
        output_path: *const c_char,
        isresult: c_int,
        outputdebug: c_int,
        isexpression: c_int,
        name: *const c_char,
        lineofs: c_int,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Load and execute TJS2 precompiled bytecode from an in-memory buffer
    /// (reference `tTJS::LoadByteCode(tTJSBinaryStream*, ...)`, the bytecode
    /// branch of `TVPExecuteStorage`). Writes the `out_is_bytecode` flag and a
    /// `TJS2_VAL_*` result; returns 0 on success and non-zero with a malloc'd
    /// message in `out_error` on a genuine loader failure.
    fn tjs2_load_bytecode(
        e: *mut Engine,
        buff: *const c_char,
        len: usize,
        name: *const c_char,
        context: *mut c_void,
        out_is_bytecode: *mut c_int,
        out_result: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Dump all live script blocks to the console/log output (`tTJS::Dump`).
    pub fn tjs2_dump(e: *mut Engine, out_error: *mut *mut c_char) -> c_int;
    /// Run the TJS2 garbage collector (`tTJS::DoGarbageCollection`); the
    /// `clIdle` half of `System.doCompact`.
    fn tjs2_do_gc(e: *mut Engine, out_error: *mut *mut c_char) -> c_int;
    /// Class names of a retained object, as a retained TJS Array (reference
    /// `Scripts.getClassNames`).
    pub fn tjs2_get_class_names(
        e: *mut Engine,
        obj: Tjs2ValueId,
        out: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Member names of a retained object, as a retained TJS Array of strings
    /// (reference `Scripts.getObjectKeys` / `ScriptsAdd::_getKeys`).
    pub fn tjs2_enum_members(
        e: *mut Engine,
        obj: Tjs2ValueId,
        out: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Enable the `missing` member handler on a retained object (reference
    /// `Scripts.setCallMissing`).
    pub fn tjs2_set_call_missing(
        e: *mut Engine,
        obj: Tjs2ValueId,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Resolve a retained object to the Rust payload of an instance-based
    /// native class (NativeInstanceSupport(TJS_NIS_GETINSTANCE) +
    /// GetNativePtr()). Returns null when there is none — see
    /// cpp/tjs2_abi.h for the full NULL/validity contract.
    fn tjs2_get_native_instance(
        e: *mut Engine,
        obj: Tjs2ValueId,
        class_name: *const c_char,
    ) -> *mut c_void;
    pub fn tjs2_free_string(s: *mut c_char);
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
        properties: *const NativeInstanceProperty,
        property_count: c_int,
        create_instance: NativeCreateInstanceFn,
        destroy_instance: NativeDestroyInstanceFn,
        invalidate_instance: Option<NativeInvalidateInstanceFn>,
    ) -> c_int;
    /// Attach static (class-level) members to an already-registered native
    /// class (reference `TJS_END_NATIVE_STATIC_METHOD_DECL` /
    /// `TJS_END_NATIVE_STATIC_PROP_DECL_OUTER`).
    fn tjs2_register_native_static_members(
        e: *mut Engine,
        class_name: *const c_char,
        methods: *const NativeMethod,
        count: c_int,
        properties: *const NativeProperty,
        prop_count: c_int,
    ) -> c_int;
    /// Attach instance members (no native payload) to an already-existing
    /// class, including a TJS2 builtin such as `Array` / `Dictionary`
    /// (reference `NCB_ATTACH_CLASS` / `NCB_ATTACH_FUNCTION`).
    fn tjs2_register_native_instance_members(
        e: *mut Engine,
        class_name: *const c_char,
        methods: *const NativeObjThisMethod,
        count: c_int,
        properties: *const NativeObjThisProperty,
        prop_count: c_int,
    ) -> c_int;
    /// Opaque, per-engine id of a retained script value (mirror of
    /// `tjs2_value_id` in cpp/tjs2_abi.h).
    fn tjs2_retain_value(engine: *mut Engine, v: *const Value) -> Tjs2ValueId;
    /// Retain a raw TJS object (see the C++ side). Returns a per-engine id
    /// or 0 on failure.
    pub fn tjs2_retain_object(engine: *mut Engine, obj: *mut c_void) -> Tjs2ValueId;
    /// Duplicate an existing retained id into a fresh id (the original stays
    /// live). Used to return a cached object without consuming the cache.
    pub fn tjs2_retain_retained_id(engine: *mut Engine, id: Tjs2ValueId) -> Tjs2ValueId;
    /// Find the retained id of a value already in the engine's retained
    /// map, without retaining anything new (identity match: the same
    /// function object + ObjThis). Returns the null id when not found.
    fn tjs2_find_retained_id(engine: *mut Engine, v: *const Value) -> Tjs2ValueId;
    /// Stack trace string (Scripts.getTraceString); malloc'd, free with
    /// `tjs2_free_string`.
    pub fn tjs2_get_stack_trace_string(engine: *mut Engine, limit: c_int) -> *mut c_char;
    /// Release a retained value. Idempotent on the C++ side.
    fn tjs2_release_value(engine: *mut Engine, id: Tjs2ValueId);
    /// Invalidate a retained object value (runs its script `finalize`).
    fn tjs2_invalidate_retained(engine: *mut Engine, id: Tjs2ValueId) -> c_int;
    /// Number of entries currently in the engine's retained-value map
    /// (diagnostic; the test suite uses it to prove retentions are consumed
    /// or released).
    pub fn tjs2_retained_count(engine: *mut Engine) -> usize;
    /// Invoke a retained value's default member with `argc` args.
    fn tjs2_call_value(
        engine: *mut Engine,
        id: Tjs2ValueId,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Like [`Self::tjs2_call_value`], but supplies a fallback receiver for
    /// the value: the value's own `ObjThis` wins, then `objthis`, then the
    /// function object itself.
    fn tjs2_call_value_with_objthis(
        engine: *mut Engine,
        id: Tjs2ValueId,
        argc: c_int,
        argv: *const Value,
        objthis: *mut c_void,
        out: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Read a named property from a retained object value through its class
    /// chain (`PropGet`).
    fn tjs2_prop_get(
        engine: *mut Engine,
        id: Tjs2ValueId,
        membername: *const c_char,
        out: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Write a named property on a retained object value through its class
    /// chain (`PropSet`, with `TJS_MEMBERENSURE` so a missing member is
    /// created and an existing property setter is invoked).
    fn tjs2_prop_set(
        engine: *mut Engine,
        id: Tjs2ValueId,
        membername: *const c_char,
        value: *const Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Delete a named property from a retained object value (`DeleteMember`,
    /// reference `iTJSDispatch2::DeleteMember`, `tjsObject.h:472`).
    fn tjs2_prop_delete(
        engine: *mut Engine,
        id: Tjs2ValueId,
        membername: *const c_char,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Write the default member of a retained object value (`PropSet` with a
    /// NULL member name). A native `missing` handler uses this to supply the
    /// value for an absent member.
    fn tjs2_prop_set_default(
        engine: *mut Engine,
        id: Tjs2ValueId,
        value: *const Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Write a value back into a by-reference argument of the native method
    /// call currently executing on this engine (reference native methods do
    /// `(*param[index]) = value`).
    fn tjs2_set_arg(
        engine: *mut Engine,
        index: c_int,
        value: *const Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// Whether the VM asked for the return value of the native method call
    /// currently executing on this engine. Reference native methods receive
    /// a null `result` pointer when the caller discards the value; see
    /// [`Tjs2Engine::result_needed`].
    fn tjs2_result_needed(engine: *mut Engine) -> c_int;
    /// Invoke a named member on a retained object value (member lookup
    /// goes through the object's own class chain, so script-subclass
    /// overrides win over native methods).
    fn tjs2_call_member(
        engine: *mut Engine,
        id: Tjs2ValueId,
        membername: *const c_char,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int;
    /// malloc-compatible allocation (for building error strings on the Rust
    /// side; free with tjs2_free_string).
    pub fn tjs2_malloc(size: usize) -> *mut c_void;
}

// The C++ stream layer (cpp/streams.cpp) inflates/deflates the `FE FE 02`
// save container with zlib. Linking it here keeps the build.rs C++ compile
// flags untouched.
#[link(name = "z")]
unsafe extern "C" {}

// Diagnostic helper implemented in cpp/streams.cpp: decode a whole text
// stream (honoring the mode's `oN` offset and the `FE FE` crypt container)
// into a malloc'd UTF-8 string (free with [`tjs2_free_string`]). Returns 0
// on success and non-zero when the file or container cannot be read. Used
// only by the stream tests below, so it is compiled for tests.
#[cfg(test)]
unsafe extern "C" {
    fn tjs2_read_text_stream_all(
        path_utf8: *const c_char,
        mode_utf8: *const c_char,
        out_utf8: *mut *mut c_char,
    ) -> c_int;
    /// The `TJS_BS_*` access the binary-write factory selects for a mode.
    fn tjs2_test_binary_write_access(mode_utf8: *const c_char) -> *const c_char;
    /// Open through the wired binary factory and close immediately.
    fn tjs2_test_open_binary_stream(
        name_utf8: *const c_char,
        mode_utf8: *const c_char,
        for_write: c_int,
        out_opened: *mut c_int,
        out_access: *mut *const c_char,
        out_error: *mut *mut c_char,
    ) -> c_int;
}

/// Opaque per-engine id of a retained script value (mirror of the C
/// `tjs2_value_id` typedef: an opaque pointer that is never null for a live
/// id).
///
/// The same type also carries a raw `iTJSDispatch2*` for the entry points that
/// take one (e.g. [`Tjs2Engine::retain_object_detached`]). The two are not
/// interchangeable: a retained id passed where a raw object pointer is
/// expected is dereferenced as one.
#[repr(transparent)]
#[derive(Copy, Clone, Default, PartialEq, Eq, Hash)]
pub struct Tjs2ValueId(*mut c_void);

impl From<*mut c_void> for Tjs2ValueId {
    fn from(raw: *mut c_void) -> Self {
        Self(raw)
    }
}

impl Tjs2ValueId {
    /// The null id, which no retained value or object uses.
    pub const fn null() -> Self {
        Self(ptr::null_mut())
    }

    /// Wraps a raw C `tjs2_value_id` (or an object/`objthis` pointer).
    pub const fn from_raw(raw: *mut c_void) -> Self {
        Self(raw)
    }

    /// Returns the raw C pointer for an FFI call.
    pub const fn as_ptr(self) -> *mut c_void {
        self.0
    }

    /// Whether the id is the null id.
    pub const fn is_null(self) -> bool {
        self.0.is_null()
    }
}

/// Non-script inputs to [`Tjs2Engine::compile_script`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompileOptions<'a> {
    /// Whether the compiled block keeps a result register.
    pub isresult: bool,
    /// Whether debug information is embedded in the output.
    pub outputdebug: bool,
    /// Whether the input is an expression rather than a statement script.
    pub isexpression: bool,
    /// Diagnostic label for the script block.
    pub name: &'a str,
    /// Reported line-number offset.
    pub lineofs: i32,
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
    /// Raw ids of all values retained on this engine (see
    /// [`Self::retain_value`]). The engine releases every id in this
    /// registry when it is dropped, so a forgotten [`ValueId`] (e.g. via
    /// `std::mem::forget`) cannot leak a retained value or crash the
    /// engine drop.
    retained: std::cell::RefCell<Vec<Tjs2ValueId>>,
}

/// An opaque id of a script value retained on a [`Tjs2Engine`] (see
/// [`Tjs2Engine::retain_value`]).
///
/// # Lifetimes
///
/// `ValueId<'a>` borrows the engine it was retained on, so a value id can
/// never outlive its engine: dropping the engine while ids are still alive
/// is rejected at compile time. (A `std::mem::forget`'d id is the one
/// exception — its Drop never runs — and the engine's registry still
/// releases it at engine drop.)
///
/// # Release
///
/// Dropping a `ValueId` releases the retained value on the C++ side. The
/// release is idempotent (releasing an already-released id is a safe
/// no-op), so dropping an id after an explicit
/// [`Tjs2Engine::release_value`] is harmless. Retained values hold their
/// own reference in the engine and stay callable across any number of
/// engine calls until released.
pub struct ValueId<'a> {
    engine: &'a Tjs2Engine,
    id: Tjs2ValueId,
}

/// A retained script value with a lifetime detached from the engine borrow
/// (see [`Tjs2Engine::retain_value_detached`]). Dropping it releases the
/// value. The engine MUST outlive every `DetachedValue` it created.
///
/// Releasing a value drops the engine's ownership of the TJS object, which
/// can run the object's destructor. A native instance's destructor drops the Rust
/// payload, and such a payload can own other `DetachedValue`s, so one
/// release can release further values: the engine's retained-value registry
/// is mutated while the release runs.
pub struct DetachedValue {
    engine: *mut Engine,
    id: Tjs2ValueId,
}

// SAFETY: the VM is single-threaded by construction (see Tjs2Engine); a
// DetachedValue is only ever used on that thread.
unsafe impl Send for DetachedValue {}
unsafe impl Sync for DetachedValue {}

impl DetachedValue {
    /// The raw retained id (for returning it across the ABI).
    pub fn raw_id(&self) -> Tjs2ValueId {
        self.id
    }
}

impl Drop for DetachedValue {
    fn drop(&mut self) {
        // SAFETY: the C++ side's release is idempotent. The release drops the
        // retained object's ownership, which can run the object's destructor
        // and therefore release further retained values.
        unsafe { tjs2_release_value(self.engine, self.id) }
    }
}

impl Drop for ValueId<'_> {
    fn drop(&mut self) {
        self.engine.release_retained(self.id);
    }
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
    /// A retained value id (from [`Tjs2Engine::retain_value_detached`] /
    /// [`Tjs2Engine::retain_object_detached`]) passed as an argument. The
    /// C++ side copies the retained variant into the argument slot, so
    /// object/dict values that cannot otherwise cross the ABI can be
    /// passed to a member call (e.g. a KiriKiri event dictionary to an
    /// owner's `action(ev)` method). The retention is consumed by the
    /// copy (the Rust side's `DetachedValue` drop is then a safe no-op).
    Retained(u64),
}

/// Result of [`Tjs2Engine::eval_retained`] / [`Tjs2Engine::exec_script_retained`].
///
/// Scalars cross the ABI directly, but an object/function result cannot be
/// represented by a [`TjsValue`] (no object handle crosses the boundary).
/// Those are retained on the engine and returned as an RAII
/// [`DetachedValue`]: pass its [`DetachedValue::raw_id`] to the C++ side as
/// `VAL_RETAINED` (which consumes the retention), or drop it to release the
/// reference.
pub enum RetainedValue {
    /// A void/integer/real/string result, usable directly.
    Value(TjsValue),
    /// An object/function result, retained on the engine.
    Object(DetachedValue),
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

/// Definition of one instance property to attach to a class.
pub struct NativeInstancePropertyDef {
    pub name: &'static str,
    pub get: Option<NativeInstancePropertyGetFn>,
    pub set: Option<NativeInstancePropertySetFn>,
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
    /// Optional native teardown hook (reference `Invalidate`). `None` keeps
    /// the previous behaviour: the payload is released only at destroy.
    pub invalidate: Option<NativeInvalidateInstanceFn>,
    pub methods: Vec<NativeInstanceMethodDef>,
    pub properties: Vec<NativeInstancePropertyDef>,
}

/// Static (class-level) members to attach to a native class already
/// registered through [`Tjs2Engine::register_native_class`] or
/// [`Tjs2Engine::register_native_class_instance`].
///
/// The reference declares these with `TJS_STATICMEMBER` (e.g.
/// `Bitmap.loadHeader` / `Bitmap.getSaveOption` and
/// `MenuItem.textToKeycode` / `MenuItem.keycodeToText`): they live on the
/// class object, are reachable without an instance, and are not copied onto
/// instances. Methods/properties use the static callback signatures (no
/// instance and no `objthis`).
///
/// The VM is single-threaded: register members only from the thread that
/// owns the engine, and after the class itself is registered.
pub struct NativeStaticMembers<'a> {
    pub class_name: &'a str,
    pub methods: Vec<NativeMethodDef>,
    pub properties: Vec<NativePropertyDef>,
}

/// Definition of one objthis-only instance method to attach to an existing
/// class (see [`Tjs2Engine::register_native_instance_members`]).
pub struct NativeObjThisMethodDef {
    pub name: &'static str,
    pub f: NativeObjThisMethodFn,
}

/// Definition of one objthis-only instance property to attach to an existing
/// class. Both `get` and `set` are optional, with the same semantics as
/// [`NativeInstancePropertyDef`].
pub struct NativeObjThisPropertyDef {
    pub name: &'static str,
    pub get: Option<NativeObjThisPropertyGetFn>,
    pub set: Option<NativeObjThisPropertySetFn>,
}

/// Instance members (no native payload) to attach to an already-existing
/// class, including a TJS2 builtin such as `Array` / `Dictionary` (reference
/// `NCB_ATTACH_CLASS` / `NCB_ATTACH_FUNCTION`). The members receive `objthis`
/// — the object they were invoked on — but no per-class opaque payload, so
/// they can be added to a builtin class object that the VM created itself.
///
/// The VM is single-threaded: register members only from the thread that
/// owns the engine, and after the class itself exists.
pub struct NativeInstanceMembers<'a> {
    pub class_name: &'a str,
    pub methods: Vec<NativeObjThisMethodDef>,
    pub properties: Vec<NativeObjThisPropertyDef>,
}

/// Install (or clear) the storage read callback the text/binary stream
/// factories use.
///
/// The read factories call it for every name before falling back to the local
/// filesystem, so `Array.load("archive.xp3>member")` reads an archive member.
/// The registration is process-wide, like the factories themselves: install it
/// before scripts run and clear it before `user` dies.
///
/// # Safety
/// `user` must stay valid for every VM call until the callback is cleared with
/// `None`, and `read` must follow the `cpp/tjs2_abi.h` contract.
pub unsafe fn set_storage_read_callback(read: Option<StorageReadFn>, user: *mut c_void) {
    // SAFETY: the caller upholds the user-pointer contract above.
    unsafe { tjs2_set_storage_read_callback(read, user) }
}

impl Tjs2Engine {
    /// The raw engine pointer (for FFI helpers that need it).
    pub fn raw(&self) -> *mut Engine {
        self.inner
    }

    /// Whether the VM asked for the return value of the native method call
    /// currently executing. A reference native method receives a null
    /// `result` pointer when the caller discards the value (a bare method
    /// call statement), and expression-mode evaluation must then compile
    /// without requiring a result. Reads as `true` outside a native callback.
    pub fn result_needed(&self) -> bool {
        // SAFETY: `self.inner` is a live engine; the ABI only reads a flag.
        unsafe { tjs2_result_needed(self.inner) != 0 }
    }

    /// Point the C++ stream factories at a directory for relative save/load
    /// paths (the game's DATA_PATH is absolute, but some code passes plain
    /// names).
    ///
    /// Returns [`TjsError`] when `dir` contains a NUL byte or the C++ side
    /// reports a failure.
    pub fn set_data_dir(&self, dir: &str) -> Result<(), TjsError> {
        let c =
            std::ffi::CString::new(dir).map_err(|_| TjsError("data dir contains NUL".into()))?;
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: `c` is a valid NUL-terminated string for the call; on
        // failure the C++ side stores an owned error string in `error`.
        let rc = unsafe { tjs2_set_data_dir_checked(c.as_ptr(), &mut error) };
        if rc != 0 {
            return Err(take_error(error));
        }
        Ok(())
    }

    /// Create a new script engine.
    pub fn new() -> Result<Self, &'static str> {
        // SAFETY: tjs2_create returns a heap-allocated engine or null.
        let inner = unsafe { tjs2_create() };
        if inner.is_null() {
            return Err("failed to create TJS2 engine");
        }
        Ok(Tjs2Engine {
            inner,
            retained: std::cell::RefCell::new(Vec::new()),
        })
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
        self.exec_script_with_context(script, name, Tjs2ValueId::null(), 0)
    }

    /// Returns the number of live retained-value entries.
    ///
    /// This is a diagnostic for native state that keeps script objects alive;
    /// the value is the engine's retained-id map size.
    pub fn retained_count(&self) -> usize {
        // SAFETY: self.inner is a live engine.
        unsafe { tjs2_retained_count(self.inner) }
    }

    /// Execute a TJS script and discard its result.
    ///
    /// The C ABI receives a null result slot, so the VM materialises no
    /// result value: an object produced by the script's last expression is
    /// neither left in the interpreter's result register nor recorded in the
    /// engine's last-object slot. Use this for scripts whose value is
    /// ignored.
    pub fn exec_script_discard(&self, script: &str, name: &str) -> Result<(), TjsError> {
        let name = CString::new(name).map_err(|_| TjsError("name contains NUL".into()))?;
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: the script pointer/length and the name are valid for the
        // call, `self.inner` is a live engine, and a null result slot is the
        // documented "no result" form.
        let rc = unsafe {
            tjs2_exec_script(
                self.inner,
                script.as_ptr().cast::<c_char>(),
                script.len(),
                name.as_ptr(),
                ptr::null_mut(),
                0,
                ptr::null_mut(),
                &mut error,
            )
        };
        if rc != 0 {
            return Err(take_error(error));
        }
        Ok(())
    }

    /// Execute a TJS script with an explicit `this` (`context`) and a reported
    /// line-number offset (`lineofs`), matching the reference
    /// `TJS::tTJS::ExecScript(content, result, context, name, lineofs)`.
    ///
    /// `context` is a raw `iTJSDispatch2*` obtained from an object argument's
    /// [`Value::object_handle`]; it is only dereferenced on the C++ side while
    /// the argument is alive.
    pub fn exec_script_with_context(
        &self,
        script: &str,
        name: &str,
        context: impl Into<Tjs2ValueId>,
        lineofs: i32,
    ) -> Result<TjsValue, TjsError> {
        let context = context.into();
        // The script is handed over as a byte pointer and a length, so the
        // wrapper does not cut it at a NUL. `name` is a diagnostic label and
        // stays a NUL-terminated C string.
        let name = CString::new(name).map_err(|_| TjsError("name contains NUL".into()))?;
        let mut result = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: all pointers point to valid, live data for the call; the
        // caller guarantees `context` is a live object handle or null.
        let rc = unsafe {
            tjs2_exec_script(
                self.inner,
                script.as_ptr().cast::<c_char>(),
                script.len(),
                name.as_ptr(),
                context.as_ptr(),
                lineofs,
                &mut result,
                &mut error,
            )
        };
        if rc != 0 {
            return Err(take_error(error));
        }
        Ok(take_value(&result))
    }

    /// Evaluate a TJS expression given as UTF-8 text.
    pub fn eval(&self, expression: &str, name: &str) -> Result<TjsValue, TjsError> {
        self.eval_with_context(expression, name, Tjs2ValueId::null(), 0)
    }

    /// Evaluate a TJS expression with an explicit `this` (`context`) and a
    /// reported line-number offset (`lineofs`), matching the reference
    /// `TJS::tTJS::EvalExpression(content, result, context, name, lineofs)`.
    pub fn eval_with_context(
        &self,
        expression: &str,
        name: &str,
        context: impl Into<Tjs2ValueId>,
        lineofs: i32,
    ) -> Result<TjsValue, TjsError> {
        let context = context.into();
        self.eval_with_context_result(expression, name, context, lineofs, true)
    }

    /// [`Self::eval_with_context`] with an explicit result-needed flag.
    ///
    /// When `need_result` is false the expression is compiled without a
    /// result slot, matching a reference native method that received a null
    /// `result` pointer: an expression that produces no value (a `swap`, for
    /// instance) then compiles, and the discarded value comes back as
    /// [`TjsValue::Void`].
    pub fn eval_with_context_result(
        &self,
        expression: &str,
        name: &str,
        context: impl Into<Tjs2ValueId>,
        lineofs: i32,
        need_result: bool,
    ) -> Result<TjsValue, TjsError> {
        let context = context.into();
        // Length-aware hand-off, as in `exec_script_with_context`.
        let name = CString::new(name).map_err(|_| TjsError("name contains NUL".into()))?;
        let mut result = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        let out_result = if need_result {
            &mut result as *mut Value
        } else {
            ptr::null_mut()
        };
        // SAFETY: all pointers point to valid, live data for the call; the
        // caller guarantees `context` is a live object handle or null.
        let rc = unsafe {
            tjs2_eval(
                self.inner,
                expression.as_ptr().cast::<c_char>(),
                expression.len(),
                name.as_ptr(),
                context.as_ptr(),
                lineofs,
                out_result,
                &mut error,
            )
        };
        if rc != 0 {
            return Err(take_error(error));
        }
        Ok(take_value(&result))
    }

    /// Replace the text of the registered TJS message `name` with `message`
    /// (reference `TJS::TJSAssignMessage`). Returns whether a message by that
    /// name exists; a C++ failure is a [`TjsError`].
    pub fn assign_message(&self, name: &str, message: &str) -> Result<bool, TjsError> {
        let name = CString::new(name).map_err(|_| TjsError("message name contains NUL".into()))?;
        let message =
            CString::new(message).map_err(|_| TjsError("message text contains NUL".into()))?;
        let mut error: *mut c_char = ptr::null_mut();
        let rc =
            // SAFETY: both pointers are valid, NUL-terminated UTF-8 for the call;
            // the function reads them only and stores an owned error string in
            // `error` on failure.
            unsafe { tjs2_assign_message_checked(name.as_ptr(), message.as_ptr(), &mut error) };
        match rc {
            1 => Ok(true),
            0 => Ok(false),
            _ => Err(take_error(error)),
        }
    }

    /// The vendored TJS2 engine version as `major.minor.release`.
    pub fn tjs_version_string(&self) -> String {
        // SAFETY: the ABI returns a static NUL-terminated string.
        let pointer = unsafe { tjs2_tjs_version_string() };
        if pointer.is_null() {
            return String::new();
        }
        // SAFETY: the pointer is a static NUL-terminated C string.
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    }

    /// The C++ translation unit's compile date and time (`__DATE__`/`__TIME__`),
    /// the counterpart of the reference `TVPCompileDate`/`TVPCompileTime`.
    pub fn compile_date_time(&self) -> (String, String) {
        // SAFETY: the ABI returns static NUL-terminated strings.
        let (date, time) = unsafe { (tjs2_compile_date(), tjs2_compile_time()) };
        let read = |pointer: *const c_char| {
            if pointer.is_null() {
                String::new()
            } else {
                // SAFETY: the pointer is a static NUL-terminated C string.
                unsafe { CStr::from_ptr(pointer) }
                    .to_string_lossy()
                    .into_owned()
            }
        };
        (read(date), read(time))
    }

    /// Compile a UTF-8 script to a binary bytecode file (reference
    /// `TVPCompileStorage` / `tTJS::CompileScript`).
    ///
    /// `output_path` is a plain filesystem path (the wired stream factory is
    /// file-backed) or a data-dir-relative name. `isresult` selects whether
    /// the compiled block keeps a result register, `outputdebug` embeds
    /// debug information, and `isexpression` compiles an expression rather
    /// than a statement script. `name` / `lineofs` label the script block
    /// for diagnostics.
    pub fn compile_script(
        &self,
        script: &str,
        output_path: &str,
        options: CompileOptions<'_>,
    ) -> Result<(), TjsError> {
        // The script bytes go over as a pointer and a length; the output
        // path and the diagnostic name stay NUL-terminated C strings (a path
        // is handed to the OS, and a name is a label).
        let output_path =
            CString::new(output_path).map_err(|_| TjsError("output path contains NUL".into()))?;
        let name = CString::new(options.name).map_err(|_| TjsError("name contains NUL".into()))?;
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: all pointers are valid for the call; the C++ side copies
        // the UTF-8 strings and writes the output file.
        let rc = unsafe {
            tjs2_compile_script(
                self.inner,
                script.as_ptr().cast::<c_char>(),
                script.len(),
                output_path.as_ptr(),
                options.isresult as c_int,
                options.outputdebug as c_int,
                options.isexpression as c_int,
                name.as_ptr(),
                options.lineofs,
                &mut error,
            )
        };
        if rc != 0 {
            return Err(take_error(error));
        }
        Ok(())
    }

    /// Load and execute TJS2 precompiled bytecode from an in-memory buffer
    /// (reference `TVPExecuteStorage`'s bytecode branch,
    /// `reference/cpp/core/base/ScriptMgnIntf.cpp:713-727`, which calls the
    /// VM's stream-based `tTJS::LoadByteCode`).
    ///
    /// Returns `Ok(Some(result))` when the loader recognized and consumed
    /// `bytes` (TJS2 bytecode, or the binary-serialized data the same loader
    /// entry also accepts), `Ok(None)` when it did not (the caller then decodes
    /// the same bytes as text), and `Err` when the loader recognized bytecode
    /// and rejected it. `context` is the script's `this` (a raw
    /// `iTJSDispatch2*`), or null for the global object.
    pub fn load_bytecode(
        &self,
        bytes: &[u8],
        name: &str,
        context: impl Into<Tjs2ValueId>,
    ) -> Result<Option<RetainedValue>, TjsError> {
        let context = context.into();
        // `bytes` is handed over as a pointer and a length; `name` is a
        // diagnostic label and stays a NUL-terminated C string.
        let name = CString::new(name).map_err(|_| TjsError("name contains NUL".into()))?;
        let mut is_bytecode: c_int = 0;
        let mut result = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: all pointers point to valid, live data for the call; the
        // caller guarantees `context` is a live object handle or null.
        let rc = unsafe {
            tjs2_load_bytecode(
                self.inner,
                bytes.as_ptr().cast::<c_char>(),
                bytes.len(),
                name.as_ptr(),
                context.as_ptr(),
                &mut is_bytecode,
                &mut result,
                &mut error,
            )
        };
        if rc != 0 {
            return Err(take_error(error));
        }
        if is_bytecode == 0 {
            return Ok(None);
        }
        self.retain_object_result(take_value(&result)).map(Some)
    }

    /// Dump all live script blocks through the log callback installed with
    /// [`Self::set_log_cb`] (reference `TVPDumpScriptEngine` / `tTJS::Dump`).
    pub fn dump(&self) -> Result<(), TjsError> {
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine.
        let rc = unsafe { tjs2_dump(self.inner, &mut error) };
        if rc != 0 {
            return Err(take_error(error));
        }
        Ok(())
    }

    /// Run the TJS2 garbage collector (reference `tTJS::DoGarbageCollection`,
    /// `tjs.cpp:495`). This is the `clIdle` half of `System.doCompact`: the
    /// reference's compact-event hook invokes it whenever the compact level
    /// reaches `TVP_COMPACT_LEVEL_IDLE`.
    pub fn do_gc(&self) -> Result<(), TjsError> {
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine.
        let rc = unsafe { tjs2_do_gc(self.inner, &mut error) };
        if rc != 0 {
            return Err(take_error(error));
        }
        Ok(())
    }

    /// Execute a script like [`Self::exec_script`], but retain an object
    /// result instead of losing it across the ABI.
    ///
    /// The C++ side records the most recent object-valued result when
    /// `tjs2_exec_script` returns, so [`Self::retain_value_detached`] can
    /// resolve an [`TjsValue::Object`] to the freshly produced object. Only
    /// the object arm is retained; void/integer/real/string results are
    /// returned as [`RetainedValue::Value`] and never allocate a retention.
    pub fn exec_script_retained(
        &self,
        script: &str,
        name: &str,
    ) -> Result<RetainedValue, TjsError> {
        self.exec_script_retained_with_context(script, name, Tjs2ValueId::null(), 0)
    }

    /// [`Self::exec_script_retained`] with an explicit `this` and line offset;
    /// see [`Self::exec_script_with_context`].
    pub fn exec_script_retained_with_context(
        &self,
        script: &str,
        name: &str,
        context: impl Into<Tjs2ValueId>,
        lineofs: i32,
    ) -> Result<RetainedValue, TjsError> {
        let context = context.into();
        let v = self.exec_script_with_context(script, name, context, lineofs)?;
        self.retain_object_result(v)
    }

    /// Evaluate an expression like [`Self::eval`], but retain an object
    /// result instead of losing it across the ABI. See
    /// [`Self::exec_script_retained`].
    pub fn eval_retained(&self, expression: &str, name: &str) -> Result<RetainedValue, TjsError> {
        self.eval_retained_with_context(expression, name, Tjs2ValueId::null(), 0)
    }

    /// [`Self::eval_retained`] with an explicit `this` and line offset; see
    /// [`Self::eval_with_context`].
    pub fn eval_retained_with_context(
        &self,
        expression: &str,
        name: &str,
        context: impl Into<Tjs2ValueId>,
        lineofs: i32,
    ) -> Result<RetainedValue, TjsError> {
        let context = context.into();
        self.eval_retained_with_context_result(expression, name, context, lineofs, true)
    }

    /// [`Self::eval_retained_with_context`] with an explicit result-needed
    /// flag. When `need_result` is false the expression compiles without a
    /// result slot (reference: a native method whose caller discarded the
    /// result) and the discarded value is returned as [`TjsValue::Void`];
    /// nothing is retained. See [`Self::eval_with_context_result`].
    pub fn eval_retained_with_context_result(
        &self,
        expression: &str,
        name: &str,
        context: impl Into<Tjs2ValueId>,
        lineofs: i32,
        need_result: bool,
    ) -> Result<RetainedValue, TjsError> {
        let context = context.into();
        if !need_result {
            self.eval_with_context_result(expression, name, context, lineofs, false)?;
            return Ok(RetainedValue::Value(TjsValue::Void));
        }
        let v = self.eval_with_context(expression, name, context, lineofs)?;
        self.retain_object_result(v)
    }

    /// Execute a TJS script given as UTF-16 code units, keeping every unit
    /// exactly as the reference text stream does.
    ///
    /// This is the lossless counterpart of [`Self::exec_script_with_context`]:
    /// `script` is a code-unit slice, so an unpaired surrogate survives the
    /// boundary instead of being dropped by a UTF-8 round trip.
    pub fn exec_script_wide_with_context(
        &self,
        script: &[u16],
        name: &str,
        context: impl Into<Tjs2ValueId>,
        lineofs: i32,
    ) -> Result<TjsValue, TjsError> {
        let context = context.into();
        let name = CString::new(name).map_err(|_| TjsError("name contains NUL".into()))?;
        let mut result = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: `script` is a live `&[u16]` and `name` a live C string; the
        // caller guarantees `context` is a live object handle or null.
        let rc = unsafe {
            tjs2_exec_script_wide(
                self.inner,
                script.as_ptr(),
                script.len(),
                name.as_ptr(),
                context.as_ptr(),
                lineofs,
                &mut result,
                &mut error,
            )
        };
        if rc != 0 {
            return Err(take_error(error));
        }
        Ok(take_value(&result))
    }

    /// [`Self::exec_script_wide_with_context`] retaining an object result; see
    /// [`Self::exec_script_retained`].
    pub fn exec_script_retained_wide_with_context(
        &self,
        script: &[u16],
        name: &str,
        context: impl Into<Tjs2ValueId>,
        lineofs: i32,
    ) -> Result<RetainedValue, TjsError> {
        let context = context.into();
        let v = self.exec_script_wide_with_context(script, name, context, lineofs)?;
        self.retain_object_result(v)
    }

    /// Evaluate a TJS expression given as UTF-16 code units.
    ///
    /// The lossless counterpart of [`Self::eval_with_context`].
    pub fn eval_wide_with_context(
        &self,
        expression: &[u16],
        name: &str,
        context: impl Into<Tjs2ValueId>,
        lineofs: i32,
    ) -> Result<TjsValue, TjsError> {
        let context = context.into();
        let name = CString::new(name).map_err(|_| TjsError("name contains NUL".into()))?;
        let mut result = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: `expression` is a live `&[u16]` and `name` a live C string;
        // the caller guarantees `context` is a live object handle or null.
        let rc = unsafe {
            tjs2_eval_wide(
                self.inner,
                expression.as_ptr(),
                expression.len(),
                name.as_ptr(),
                context.as_ptr(),
                lineofs,
                &mut result,
                &mut error,
            )
        };
        if rc != 0 {
            return Err(take_error(error));
        }
        Ok(take_value(&result))
    }

    /// [`Self::eval_wide_with_context`] retaining an object result; see
    /// [`Self::eval_retained`].
    pub fn eval_retained_wide_with_context(
        &self,
        expression: &[u16],
        name: &str,
        context: impl Into<Tjs2ValueId>,
        lineofs: i32,
    ) -> Result<RetainedValue, TjsError> {
        let context = context.into();
        let v = self.eval_wide_with_context(expression, name, context, lineofs)?;
        self.retain_object_result(v)
    }

    /// Wrap a successful eval/exec result: retain objects, pass scalars
    /// through. The C++ `variant_to_value` overwrites `last_object` with the
    /// final result when it is an object, so the retention always resolves
    /// to the value that was just produced (never a nested object).
    fn retain_object_result(&self, v: TjsValue) -> Result<RetainedValue, TjsError> {
        if matches!(v, TjsValue::Object) {
            self.retain_value_detached(&TjsValue::Object)
                .map(RetainedValue::Object)
                .map_err(TjsError)
        } else {
            Ok(RetainedValue::Value(v))
        }
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
        let property_names: Vec<CString> = builder
            .properties
            .iter()
            .map(|p| {
                CString::new(p.name)
                    .map_err(|_| format!("property name contains a NUL byte: {:?}", p.name))
            })
            .collect::<Result<_, _>>()?;
        let c_properties: Vec<NativeInstanceProperty> = builder
            .properties
            .iter()
            .zip(&property_names)
            .map(|(p, n)| NativeInstanceProperty {
                name: n.as_ptr(),
                get: p.get,
                set: p.set,
            })
            .collect();
        // SAFETY: self.inner is a valid engine; the names and arrays are
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
                if c_properties.is_empty() {
                    ptr::null()
                } else {
                    c_properties.as_ptr()
                },
                c_properties.len() as c_int,
                builder.create,
                builder.destroy,
                builder.invalidate,
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

    /// Attach static (class-level) members to an already-registered native
    /// class (reference `TJS_END_NATIVE_STATIC_METHOD_DECL` /
    /// `TJS_END_NATIVE_STATIC_PROP_DECL_OUTER`).
    ///
    /// Instance-capable classes put every member on their instances; the
    /// reference instead declares some members with `TJS_STATICMEMBER`
    /// (`Bitmap.loadHeader` / `Bitmap.getSaveOption`,
    /// `MenuItem.textToKeycode` / `MenuItem.keycodeToText`), which live on
    /// the class object, are reachable without an instance, and stay
    /// invisible to instances. Call this after
    /// [`Self::register_native_class_instance`] / [`Self::register_native_class`]
    /// with the class's name; methods and properties use the static callback
    /// signatures ([`NativeMethodDef`] / [`NativePropertyDef`]).
    pub fn register_native_static_members(
        &self,
        members: &NativeStaticMembers,
    ) -> Result<(), String> {
        let class_name = CString::new(members.class_name)
            .map_err(|_| format!("class name contains a NUL byte: {:?}", members.class_name))?;
        let method_names: Vec<CString> = members
            .methods
            .iter()
            .map(|m| {
                CString::new(m.name)
                    .map_err(|_| format!("method name contains a NUL byte: {:?}", m.name))
            })
            .collect::<Result<_, _>>()?;
        let c_methods: Vec<NativeMethod> = members
            .methods
            .iter()
            .zip(&method_names)
            .map(|(m, n)| NativeMethod {
                name: n.as_ptr(),
                f: m.f,
            })
            .collect();
        let property_names: Vec<CString> = members
            .properties
            .iter()
            .map(|p| {
                CString::new(p.name)
                    .map_err(|_| format!("property name contains a NUL byte: {:?}", p.name))
            })
            .collect::<Result<_, _>>()?;
        let c_properties: Vec<NativeProperty> = members
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
        // valid for the call. The C++ side copies what it needs during
        // registration.
        let rc = unsafe {
            tjs2_register_native_static_members(
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
                "failed to register static members on native class '{}' (error {rc})",
                members.class_name
            ));
        }
        Ok(())
    }

    /// Attach instance members (without a native payload) to an
    /// already-existing class, including a TJS2 builtin such as `Array` /
    /// `Dictionary` (reference `NCB_ATTACH_CLASS` / `NCB_ATTACH_FUNCTION`).
    ///
    /// A class registered through [`Self::register_native_class`] /
    /// [`Self::register_native_class_instance`] receives the members as
    /// ordinary (non-static) instance members. Any other name is looked up on
    /// the VM global object as a builtin class object and the members are
    /// attached to it, so they become callable on every instance of that
    /// class. Unlike [`NativeInstanceMethodDef`], the callbacks receive only
    /// `objthis` — the object the member was invoked on — and no per-class
    /// opaque payload.
    ///
    /// The VM is single-threaded: register members only from the thread that
    /// owns the engine, and after the class itself exists.
    pub fn register_native_instance_members(
        &self,
        members: &NativeInstanceMembers,
    ) -> Result<(), String> {
        let class_name = CString::new(members.class_name)
            .map_err(|_| format!("class name contains a NUL byte: {:?}", members.class_name))?;
        let method_names: Vec<CString> = members
            .methods
            .iter()
            .map(|m| {
                CString::new(m.name)
                    .map_err(|_| format!("method name contains a NUL byte: {:?}", m.name))
            })
            .collect::<Result<_, _>>()?;
        let c_methods: Vec<NativeObjThisMethod> = members
            .methods
            .iter()
            .zip(&method_names)
            .map(|(m, n)| NativeObjThisMethod {
                name: n.as_ptr(),
                f: m.f,
            })
            .collect();
        let property_names: Vec<CString> = members
            .properties
            .iter()
            .map(|p| {
                CString::new(p.name)
                    .map_err(|_| format!("property name contains a NUL byte: {:?}", p.name))
            })
            .collect::<Result<_, _>>()?;
        let c_properties: Vec<NativeObjThisProperty> = members
            .properties
            .iter()
            .zip(&property_names)
            .map(|(p, n)| NativeObjThisProperty {
                name: n.as_ptr(),
                get: p.get,
                set: p.set,
            })
            .collect();
        // SAFETY: self.inner is a valid engine; the names and arrays are
        // valid for the call. The C++ side copies what it needs during
        // registration.
        let rc = unsafe {
            tjs2_register_native_instance_members(
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
                "failed to register instance members on class '{}' (error {rc})",
                members.class_name
            ));
        }
        Ok(())
    }

    /// Retain a script value so it stays alive and callable across engine
    /// calls (see [`ValueId`] for the lifetime/release contract).
    ///
    /// A function object is obtained by evaluating its name:
    /// `let f = engine.eval("f", "test")?;` yields [`TjsValue::Object`],
    /// and the engine resolves it against the most recent object-valued
    /// script result. Scalars (void/integer/real/string) can also be
    /// retained, but only object values are callable.
    pub fn retain_value(&self, v: &TjsValue) -> Result<ValueId<'_>, String> {
        let mut strings = Vec::new();
        let ffi = match v {
            // The C++ side resolves OBJECT-typed values against the
            // engine's most recent object result (no handle crosses the
            // ABI).
            TjsValue::Object => Value {
                ty: VAL_OBJECT,
                integer: 0,
                real: 0.0,
                string: ptr::null(),
                array: ptr::null(),
                array_count: 0,
                retained: 0,
            },
            other => value_to_ffi(other, &mut strings)?,
        };
        // SAFETY: `ffi` mirrors `v` (strings in `strings` stay alive for
        // the call) and self.inner is a live engine.
        let id = unsafe { tjs2_retain_value(self.inner, &ffi) };
        if id.is_null() {
            return Err(
                "failed to retain value: object values are resolved against \
the most recent object-valued script result (e.g. eval of the function's \
name), and none was available"
                    .into(),
            );
        }
        self.retained.borrow_mut().push(id);
        Ok(ValueId { engine: self, id })
    }

    /// Invoke a retained value with the given arguments (the value's
    /// default member; no `this`). The value must be retained on this
    /// engine and not yet released.
    pub fn call_value(&self, id: &ValueId<'_>, args: &[TjsValue]) -> Result<TjsValue, String> {
        if !std::ptr::eq(id.engine, self) {
            return Err("value id belongs to a different engine".into());
        }
        if !self.retained.borrow().contains(&id.id) {
            return Err(
                "invalid retained value: not retained on this engine (already released?)".into(),
            );
        }
        let mut strings = Vec::new();
        let ffi_args: Vec<Value> = args
            .iter()
            .map(|a| value_to_ffi(a, &mut strings))
            .collect::<Result<_, _>>()?;
        let mut out = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine, id.id is a live retained id,
        // and ffi_args/strings stay alive for the call.
        let rc = unsafe {
            tjs2_call_value(
                self.inner,
                id.id,
                ffi_args.len() as c_int,
                if ffi_args.is_empty() {
                    ptr::null()
                } else {
                    ffi_args.as_ptr()
                },
                &mut out,
                &mut error,
            )
        };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        // SAFETY: `out` was filled by the C++ side on success.
        Ok(take_value(&out))
    }

    /// Release a retained value immediately (the id is also released when
    /// dropped; call this only to free it earlier). The second release of
    /// the same id is an error: the id is no longer registered on this
    /// engine. (The C++-level release itself is idempotent — a safe no-op
    /// for unknown ids — so dropping the id afterwards is harmless.)
    pub fn release_value(&self, id: &ValueId<'_>) -> Result<(), String> {
        if !std::ptr::eq(id.engine, self) {
            return Err("value id belongs to a different engine".into());
        }
        let mut retained = self.retained.borrow_mut();
        let Some(pos) = retained.iter().position(|i| *i == id.id) else {
            return Err("invalid retained value: already released".into());
        };
        retained.remove(pos);
        drop(retained);
        self.release_retained(id.id);
        Ok(())
    }

    /// FFI-release one raw retained id and drop it from the registry (used
    /// by both [`ValueId`]'s Drop and engine drop). Releasing an id that is
    /// not in the registry is still safe: the C++ side's erase of an
    /// unknown id is a no-op.
    fn release_retained(&self, id: Tjs2ValueId) {
        self.retained.borrow_mut().retain(|i| *i != id);
        // SAFETY: self.inner is a live engine while a ValueId (or the
        // engine itself) holds a reference; release is idempotent.
        unsafe { tjs2_release_value(self.inner, id) };
    }

    /// Retain a script value with a lifetime detached from the engine
    /// borrow — for storage in process-global registries (the timer
    /// natives).
    ///
    /// SAFETY: the engine MUST outlive every `DetachedValue` it created;
    /// krkr-rs keeps the VM alive for the whole app, so this holds.
    pub fn retain_value_detached(&self, v: &TjsValue) -> Result<DetachedValue, String> {
        let mut strings = Vec::new();
        let ffi = match v {
            TjsValue::Object => Value {
                ty: VAL_OBJECT,
                integer: 0,
                real: 0.0,
                string: ptr::null(),
                array: ptr::null(),
                array_count: 0,
                retained: 0,
            },
            other => value_to_ffi(other, &mut strings)?,
        };
        // SAFETY: `ffi` mirrors `v` and self.inner is a live engine.
        let id = unsafe { tjs2_retain_value(self.inner, &ffi) };
        if id.is_null() {
            return Err(
                "failed to retain value: object values are resolved against \
the most recent object-valued script result (e.g. eval of the function's \
name), and none was available"
                    .into(),
            );
        }
        Ok(DetachedValue {
            engine: self.inner,
            id,
        })
    }

    /// Retain the object carried by a native callback argument (`argv[i]`),
    /// using the per-argument handle recorded by the C++ side. Unlike
    /// [`Self::retain_value_detached`] with [`TjsValue::Object`], this does
    /// not depend on the engine's "most recent object" slot, so it is correct
    /// when the call passes more than one object (e.g.
    /// `Layer.drawPolygon(app, points)`).
    ///
    /// `v` must be a `VAL_OBJECT` argument; a value without a handle falls
    /// back to the last-object resolution of [`Self::retain_value_detached`].
    pub fn retain_object_arg(&self, v: &Value) -> Result<DetachedValue, String> {
        // SAFETY: self.inner is a live engine and `v` is a valid ABI value
        // for the duration of the call.
        unsafe { retain_object_arg_raw(self.inner, v) }
    }

    /// Retain a raw TJS object (e.g. a native instance's `objthis`) with a
    /// lifetime detached from the engine borrow, like
    /// [`Self::retain_value_detached`] but for a raw object pointer. The
    /// returned handle releases the retention when dropped.
    /// The raw `objthis` pointer comes from the C++ dispatch which holds the
    /// object alive for the duration of the call (the native-callback
    /// trampoline guarantees validity; the retained reference keeps it alive
    /// afterward). Dereferencing happens only on the C++ side.
    pub fn retain_object_detached(
        &self,
        obj: impl Into<Tjs2ValueId>,
    ) -> Result<DetachedValue, String> {
        let obj = obj.into();
        if obj.is_null() {
            return Err("cannot retain a null object".into());
        }
        // SAFETY: obj is a live TJS object for the duration of the call.
        let id = unsafe { tjs2_retain_object(self.inner, obj.as_ptr()) };
        if id.is_null() {
            return Err("failed to retain object".into());
        }
        Ok(DetachedValue {
            engine: self.inner,
            id,
        })
    }

    /// Duplicate an existing retained id into a fresh id, leaving the
    /// original live (C++ `tjs2_retain_retained_id`).
    ///
    /// Used to hand a cached object to a consuming call (e.g. the null
    /// plugin's default-member write) without invalidating the cache. The
    /// returned [`DetachedValue`] releases the duplicate when dropped; if the
    /// consuming call already consumed it, that release is a safe no-op.
    ///
    /// `id` must be a live retained id owned by a caller-held handle.
    pub fn duplicate_retained(&self, id: Tjs2ValueId) -> Result<DetachedValue, String> {
        if id.is_null() {
            return Err("cannot duplicate a null retained id".into());
        }
        // SAFETY: self.inner is a live engine and `id` is a live retained id
        // (the caller's handle keeps it alive for the call).
        let duplicate = unsafe { tjs2_retain_retained_id(self.inner, id) };
        if duplicate.is_null() {
            return Err("failed to duplicate retained id".into());
        }
        Ok(DetachedValue {
            engine: self.inner,
            id: duplicate,
        })
    }

    /// Invalidate a retained object value (the reference `invalidate obj;`
    /// statement): invokes the object's `iTJSDispatch2::Invalidate`, running
    /// its script `finalize`. Returns an error string on an unknown id or a
    /// script exception. The retention is **not** released; the owning
    /// [`DetachedValue`] still releases it when dropped.
    pub fn invalidate_retained(&self, id: Tjs2ValueId) -> Result<(), String> {
        if id.is_null() {
            return Err("cannot invalidate a null retained id".into());
        }
        // SAFETY: self.inner is a live engine and `id` is a live retained id
        // owned by the caller's handle.
        let rc = unsafe { tjs2_invalidate_retained(self.inner, id) };
        if rc == 0 {
            Ok(())
        } else {
            Err("tjs2_invalidate_retained failed".into())
        }
    }

    /// Find the retained id of a script value already retained on this
    /// engine, without creating a new entry (identity match: same closure
    /// object + ObjThis). Returns `None` when the value is not retained.
    /// Object values are resolved against the most recent object-valued
    /// script result, like [`Self::retain_value_detached`].
    pub fn find_retained_id(&self, v: &TjsValue) -> Option<Tjs2ValueId> {
        let mut strings = Vec::new();
        let ffi = match v {
            TjsValue::Object => Value {
                ty: VAL_OBJECT,
                integer: 0,
                real: 0.0,
                string: ptr::null(),
                array: ptr::null(),
                array_count: 0,
                retained: 0,
            },
            other => match value_to_ffi(other, &mut strings) {
                Ok(v) => v,
                Err(_) => return None,
            },
        };
        // SAFETY: `ffi` mirrors `v` and self.inner is a live engine.
        let id = unsafe { tjs2_find_retained_id(self.inner, &ffi) };
        if id.is_null() { None } else { Some(id) }
    }

    /// Invoke a named member on a retained object value (e.g. a Timer
    /// object's `onTimer`). Member lookup goes through the object's own
    /// class chain, so a script subclass overriding `onTimer` (the game's
    /// `OnceTimer`) runs its override.
    /// `id` is a retained value whose validity is owned by the
    /// `DetachedValue`/`ValueId` handles (which keep the engine alive via
    /// the borrow checker); the C++ side dereferences it within the call.
    pub fn call_member(
        &self,
        id: Tjs2ValueId,
        membername: &str,
        args: &[TjsValue],
    ) -> Result<TjsValue, String> {
        let mut strings = Vec::new();
        let ffi_args: Vec<Value> = args
            .iter()
            .map(|a| value_to_ffi(a, &mut strings))
            .collect::<Result<_, _>>()?;
        let mut out = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        let name = std::ffi::CString::new(membername)
            .map_err(|_| "member name contains a NUL byte".to_string())?;
        // SAFETY: self.inner is a live engine and id is a live retained id;
        // name/argv/out follow the ABI contract for the duration of the call.
        // (The retained id's validity is owned by DetachedValue/ValueId,
        // which keep the engine alive via the borrow checker; call_member
        // itself does not dereference the pointer — the C++ side does.)
        let rc = unsafe {
            tjs2_call_member(
                self.inner,
                id,
                name.as_ptr(),
                ffi_args.len() as c_int,
                if ffi_args.is_empty() {
                    ptr::null()
                } else {
                    ffi_args.as_ptr()
                },
                &mut out,
                &mut error,
            )
        };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        // SAFETY: `out` was filled by the C++ side on success.
        Ok(take_value(&out))
    }

    /// Read a named property from a retained object value through its class
    /// chain (`PropGet`). Natives use this to resolve an object argument to
    /// one of its script-visible properties — e.g. `Layer.parent = <Layer>`
    /// reads the right-hand object's `id`.
    pub fn member(&self, id: Tjs2ValueId, membername: &str) -> Result<TjsValue, String> {
        let name = std::ffi::CString::new(membername)
            .map_err(|_| "member name contains a NUL byte".to_string())?;
        let mut out = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine and id is a live retained id;
        // name/out follow the ABI contract for the duration of the call.
        let rc = unsafe { tjs2_prop_get(self.inner, id, name.as_ptr(), &mut out, &mut error) };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        // SAFETY: `out` was filled by the C++ side on success.
        Ok(take_value(&out))
    }

    /// Write a named property on a retained object value through its class
    /// chain (`PropSet`). `TJS_MEMBERENSURE` semantics match TJS plain
    /// assignment: a missing member is created, and an existing native or
    /// script property setter is invoked. `value` may be a scalar/string, an
    /// octet, null, or a [`TjsValue::Retained`] id (the retention is consumed
    /// by the write).
    ///
    /// **Consumption contract:** a [`TjsValue::Retained`] id is consumed by
    /// the conversion *before* `PropSet` runs, so it is gone even when this
    /// call returns `Err` (unless the target itself was invalid, in which
    /// case conversion never happens). Do not reuse the id after a call.
    pub fn set_member(
        &self,
        id: Tjs2ValueId,
        membername: &str,
        value: &TjsValue,
    ) -> Result<(), String> {
        let mut strings = Vec::new();
        let ffi = value_to_ffi(value, &mut strings)?;
        let name = std::ffi::CString::new(membername)
            .map_err(|_| "member name contains a NUL byte".to_string())?;
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine and id is a live retained id;
        // name/value (including any string storage in `strings`) follow the
        // ABI contract for the duration of the call.
        let rc = unsafe { tjs2_prop_set(self.inner, id, name.as_ptr(), &ffi, &mut error) };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        Ok(())
    }

    /// Write an octet member on a retained object value.
    ///
    /// An octet is the VM's byte-string variant: a script reads the member
    /// as `Octet` (`typeof`), with `.length` and index access. The safe
    /// [`TjsValue`] surface carries no octet variant, so the bytes travel
    /// through this dedicated entry point.
    ///
    /// The VM copies the bytes into the variant while the call runs, so
    /// `bytes` only has to outlive it. A payload longer than the variant's
    /// length field is a typed error.
    pub fn set_member_octet(
        &self,
        id: Tjs2ValueId,
        membername: &str,
        bytes: &[u8],
    ) -> Result<(), String> {
        let length = c_int::try_from(bytes.len())
            .map_err(|_| format!("octet member is too long: {} bytes", bytes.len()))?;
        let name = std::ffi::CString::new(membername)
            .map_err(|_| "member name contains a NUL byte".to_string())?;
        let ffi = Value {
            ty: VAL_OCTET,
            integer: 0,
            real: 0.0,
            string: bytes.as_ptr() as *const c_char,
            array: ptr::null(),
            array_count: length,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine and id is a live retained id;
        // name follows the ABI contract for the duration of the call, and
        // `bytes` outlives the call — the VM copies them into the variant
        // before it returns.
        let rc = unsafe { tjs2_prop_set(self.inner, id, name.as_ptr(), &ffi, &mut error) };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        Ok(())
    }

    /// Delete a named property from a retained object value
    /// (`iTJSDispatch2::DeleteMember`, reference `tjsObject.h:472`;
    /// `tTJSCustomObject::DeleteMember`, `tjsObject.cpp:1537-1552`, over
    /// `DeleteByName`, `tjsObject.cpp:759`).
    ///
    /// This is the removal path for members [`Self::set_member`] created and
    /// for the class-name globals the native registration entry points publish.
    /// The reference reaches the same VM operation through its `delete`
    /// operator, which passes flag 0 and the dispatch object itself
    /// (`tjsInterCodeExec.cpp:1826`); only the object's own member table is
    /// affected.
    ///
    /// An absent member is reported here as a typed error: the VM returns
    /// `TJS_E_MEMBERNOTFOUND` (`tjsObject.cpp:1546`), which the `delete`
    /// operator turns into the boolean `false` rather than an exception
    /// (`tjsInterCodeExec.cpp:1829-1833`). A [`Tjs2ValueId`] that is not a
    /// live object (null, released, or a non-object value such as TJS `null`)
    /// is likewise a typed error.
    pub fn delete_member(&self, id: Tjs2ValueId, membername: &str) -> Result<(), String> {
        let name = std::ffi::CString::new(membername)
            .map_err(|_| "member name contains a NUL byte".to_string())?;
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine and id is a live retained id;
        // name follows the ABI contract for the duration of the call.
        let rc = unsafe { tjs2_prop_delete(self.inner, id, name.as_ptr(), &mut error) };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        Ok(())
    }

    /// Write the **default member** of a retained object value (`PropSet`
    /// with a NULL member name).
    ///
    /// Used by the null-plugin `missing` handler: the VM passes a
    /// `tTJSSimpleGetSetProperty` as the handler's third argument and reads
    /// its default member back, so setting it is what lets an absent member
    /// be *called*. Same value/error conventions as [`Self::set_member`].
    ///
    /// **Consumption contract:** like [`Self::set_member`], a
    /// [`TjsValue::Retained`] id is consumed by the conversion before the
    /// `PropSet` runs, so it is gone even on error (unless the target was
    /// invalid and conversion never happened).
    pub fn set_default_member(&self, id: Tjs2ValueId, value: &TjsValue) -> Result<(), String> {
        let mut strings = Vec::new();
        let ffi = value_to_ffi(value, &mut strings)?;
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine and id is a live retained id;
        // value (including any string storage in `strings`) follows the ABI
        // contract for the duration of the call.
        let rc = unsafe { tjs2_prop_set_default(self.inner, id, &ffi, &mut error) };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        Ok(())
    }

    /// Write a value back into a by-reference argument of the native method
    /// call currently executing on this engine.
    ///
    /// The C ABI snapshots every native-method argument into a [`Value`]
    /// copy, so a Rust callback cannot modify the caller's variable by
    /// writing through `argv`. This entry point (backed by
    /// `tjs2_set_arg`) reaches the caller's original `tTJSVariant` slot and
    /// therefore implements the reference's `(*param[index]) = value`
    /// out-parameter pattern (e.g. `Window.getMouseVelocity`).
    ///
    /// Only valid while a native method callback is running on `self`; the
    /// call frame is pushed around the callback and popped afterwards. An
    /// `index` outside the current call's argument count is an error.
    pub fn set_arg(&self, index: usize, value: &TjsValue) -> Result<(), String> {
        let mut strings = Vec::new();
        let ffi = value_to_ffi(value, &mut strings)?;
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine; the call frame (if any) is
        // the one this callback was invoked for, and `ffi`/`strings` stay
        // alive for the call.
        let rc = unsafe { tjs2_set_arg(self.inner, index as c_int, &ffi, &mut error) };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        Ok(())
    }

    /// Class names of a retained object, most-derived first, as a new TJS
    /// Array (reference `Scripts.getClassNames`).
    ///
    /// The array is retained on the engine and returned as a
    /// [`DetachedValue`]; hand its [`DetachedValue::raw_id`] back as a
    /// `VAL_RETAINED` native result (the trampoline consumes it), or drop it
    /// to release the reference. `obj` must be a live retained object id.
    pub fn class_names(&self, obj: Tjs2ValueId) -> Result<DetachedValue, String> {
        let mut out = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine and obj is a retained id (its
        // validity is owned by the caller's ValueId/DetachedValue).
        let rc = unsafe { tjs2_get_class_names(self.inner, obj, &mut out, &mut error) };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        if out.ty != VAL_RETAINED || out.retained == 0 {
            return Err("tjs2_get_class_names returned no retained array".into());
        }
        Ok(DetachedValue {
            engine: self.inner,
            id: Tjs2ValueId::from_raw(out.retained as *mut c_void),
        })
    }

    /// Member names of a retained object, sorted and de-duplicated
    /// (reference `Scripts.getObjectKeys` / `ScriptsAdd::_getKeys`).
    ///
    /// Uses the VM's `EnumMembers` with `TJS_IGNOREPROP | TJS_ENUM_NO_VALUE`
    /// and skips hidden members, so the result is the object's observable
    /// member surface: for a class object this includes its registered (static
    /// and instance) members, and for an instance its instance members. `obj`
    /// must be a live retained object id.
    pub fn enum_members(&self, obj: Tjs2ValueId) -> Result<Vec<String>, String> {
        let mut out = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine and obj is a live retained id.
        let rc = unsafe { tjs2_enum_members(self.inner, obj, &mut out, &mut error) };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        if out.ty != VAL_RETAINED || out.retained == 0 {
            return Err("tjs2_enum_members returned no retained array".into());
        }
        // Hold the returned array in an RAII handle so it is released even if
        // reading an element fails; only the names escape.
        let array = DetachedValue {
            engine: self.inner,
            id: Tjs2ValueId::from_raw(out.retained as *mut c_void),
        };
        let count = match self.member(array.raw_id(), "count")? {
            TjsValue::Integer(value) => value,
            TjsValue::Real(value) => value as i64,
            other => {
                return Err(format!(
                    "enum_members: array count is not numeric ({other:?})"
                ));
            }
        };
        let mut members = Vec::with_capacity(count.max(0) as usize);
        for index in 0..count {
            match self.member(array.raw_id(), &index.to_string())? {
                TjsValue::String(name) => members.push(name),
                TjsValue::Integer(value) => members.push(value.to_string()),
                TjsValue::Real(value) => members.push(value.to_string()),
                other => {
                    return Err(format!(
                        "enum_members: member name is not a string ({other:?})"
                    ));
                }
            }
        }
        members.sort();
        members.dedup();
        Ok(members)
    }

    /// Enable the `missing` member handler on a retained object (reference
    /// `Scripts.setCallMissing`): after this call an access to an absent
    /// member invokes the object's `missing` method.
    pub fn set_call_missing(&self, obj: Tjs2ValueId) -> Result<(), String> {
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine and obj is a retained id; the
        // C++ side dereferences it through its retained-value map.
        let rc = unsafe { tjs2_set_call_missing(self.inner, obj, &mut error) };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        Ok(())
    }

    /// Resolve a retained script object to the payload pointer of an
    /// instance-based native class (reference
    /// `iTJSDispatch2::NativeInstanceSupport(TJS_NIS_GETINSTANCE, classid)`
    /// and `GetNativePtr()` — the lookup behind TJS_GET_NATIVE_INSTANCE and
    /// the instance method/property dispatchers).
    ///
    /// `class_name` must be the name the class was registered with via
    /// [`Self::register_native_class_instance`]; `obj` is a live retained
    /// object id (from [`Self::retain_value_detached`],
    /// [`Self::retain_object_arg`], [`Self::eval_retained`], …).
    ///
    /// Returns `Ok(Some(payload))` when `obj` is a live instance of that
    /// class. Returns `Ok(None)` — an ordinary miss, never an error — when
    /// the object is TJS `null`, a plain script object, the class object
    /// itself, an instance of a different native class, an invalidated
    /// instance, a retained id that is not an object, or when
    /// `class_name` is not an instance-capable class registered on this
    /// engine (static-only and builtin class names never carry a Rust
    /// payload). A NUL byte in `class_name` and a null `obj` id are
    /// caller mistakes and return `Err`.
    ///
    /// # Ownership and validity
    ///
    /// The payload stays owned by the engine (freed by the class's destroy
    /// callback when the object dies); this only borrows the pointer — do
    /// not free it. The pointer is valid only while the object is kept
    /// alive by its retained id **and** has not been invalidated (exactly
    /// the `GetNativePtr()` / `IsValid()` contract in `tjs2_abi.cpp`):
    /// discard it once the id is released or the object is invalidated,
    /// and never let it outlive the engine.
    pub fn native_instance_payload(
        &self,
        obj: Tjs2ValueId,
        class_name: &str,
    ) -> Result<Option<ptr::NonNull<c_void>>, String> {
        if obj.is_null() {
            return Err("native_instance_payload: null retained id".into());
        }
        let name = std::ffi::CString::new(class_name)
            .map_err(|_| "native_instance_payload: class name contains a NUL byte".to_string())?;
        // SAFETY: self.inner is a live engine, obj is a live retained id
        // (its validity is owned by the caller's ValueId/DetachedValue),
        // and name is a valid NUL-terminated string for the call. The C++
        // side only reads the payload pointer; it never hands out ownership.
        let payload = unsafe { tjs2_get_native_instance(self.inner, obj, name.as_ptr()) };
        Ok(ptr::NonNull::new(payload))
    }

    /// Invoke a retained detached value (see [`Self::retain_value_detached`]).
    pub fn call_detached(&self, dv: &DetachedValue, args: &[TjsValue]) -> Result<TjsValue, String> {
        // SAFETY: a null `objthis` is always valid; it means "no fallback
        // receiver", so the callable's own ObjThis (or the function object)
        // decides the call's `this`.
        unsafe { self.call_detached_with_objthis(dv, args, ptr::null_mut()) }
    }

    /// Like [`Self::call_detached`], but supplies a fallback receiver: the
    /// callable value's own `ObjThis` wins, then `objthis`, then the function
    /// object itself (`tTJSVariantClosure::FuncCall`). A helper that invokes
    /// an anonymous callback passes the helper's calling context as `objthis`
    /// (reference `ScriptsAdd::foreach`, `scriptsEx.cpp:690-693`).
    ///
    /// # Safety
    ///
    /// `objthis` must be a live TJS object pointer or null for the duration
    /// of the call.
    pub unsafe fn call_detached_with_objthis(
        &self,
        dv: &DetachedValue,
        args: &[TjsValue],
        objthis: *mut c_void,
    ) -> Result<TjsValue, String> {
        if !std::ptr::eq(dv.engine, self.inner) {
            return Err("detached value belongs to a different engine".into());
        }
        let mut strings = Vec::new();
        let ffi_args: Vec<Value> = args
            .iter()
            .map(|a| value_to_ffi(a, &mut strings))
            .collect::<Result<_, _>>()?;
        let mut out = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: self.inner is a live engine, dv.id is a live retained id,
        // and the caller guarantees `objthis` is a live object or null.
        let rc = unsafe {
            tjs2_call_value_with_objthis(
                self.inner,
                dv.id,
                ffi_args.len() as c_int,
                if ffi_args.is_empty() {
                    ptr::null()
                } else {
                    ffi_args.as_ptr()
                },
                objthis,
                &mut out,
                &mut error,
            )
        };
        if rc != 0 {
            return Err(take_error_string(error));
        }
        // SAFETY: `out` was filled by the C++ side on success.
        Ok(take_value(&out))
    }
}

/// Raw-engine form of [`Tjs2Engine::retain_object_arg`] for native method
/// callbacks, which receive the opaque `tjs2_engine*` instead of a
/// `&Tjs2Engine`.
///
/// # Safety
/// `engine` must be a live engine (the one the callback was registered on)
/// and `v` must point at a valid `tjs2_value` for the duration of the call.
pub unsafe fn retain_object_arg_raw(
    engine: *mut Engine,
    v: &Value,
) -> Result<DetachedValue, String> {
    if v.ty != VAL_OBJECT {
        return Err("retain_object_arg: value is not an object".into());
    }
    // SAFETY: caller guarantees `engine` is live and `v` is valid; the C++
    // side AddRefs the object into the engine's retained map.
    let id = unsafe { tjs2_retain_value(engine, v) };
    if id.is_null() {
        return Err("failed to retain object argument".into());
    }
    Ok(DetachedValue { engine, id })
}

impl Drop for Tjs2Engine {
    fn drop(&mut self) {
        // Release every still-retained value first (this covers ids whose
        // ValueId was forgotten via std::mem::forget). The C++ side's
        // release is idempotent, and its retained-value map also dies with
        // the engine struct below, so there is no double release.
        for &id in self.retained.get_mut().iter() {
            // SAFETY: self.inner is a live engine.
            unsafe { tjs2_release_value(self.inner, id) };
        }
        self.retained.get_mut().clear();
        // SAFETY: self.inner is a valid engine created by tjs2_create.
        unsafe { tjs2_destroy(self.inner) };
    }
}

extern "C" fn noop_log(_level: c_int, _msg: *const c_char, _user: *mut c_void) {}

/// Convert a C value into an owned TjsValue. The string pointer is only valid
/// until the next engine call, so it is copied immediately.
///
/// The caller reaches this only after a zero return from the C entry point, so
/// `v` holds the value that call produced.
fn take_value(v: &Value) -> TjsValue {
    match v.ty {
        VAL_VOID => TjsValue::Void,
        VAL_INTEGER => TjsValue::Integer(v.integer),
        VAL_REAL => TjsValue::Real(v.real),
        VAL_STRING if !v.string.is_null() => {
            // SAFETY: a `VAL_STRING` result carries a NUL-terminated UTF-8
            // string owned by the engine and valid until the next engine call;
            // it is copied before returning.
            let s = unsafe { CStr::from_ptr(v.string) }
                .to_string_lossy()
                .into_owned();
            TjsValue::String(s)
        }
        VAL_STRING => TjsValue::String(String::new()),
        // The safe `TjsValue` surface has no octet variant (adding one would
        // be a breaking API change for the whole workspace). Raw native
        // callbacks read octet arguments through `Value::octet_bytes`; an
        // octet returned by eval/exec is surfaced as an opaque Object, like
        // before this type existed.
        VAL_OCTET => TjsValue::Object,
        // TJS `null` is still an object to the safe wrapper (its `object_handle`
        // is null).
        VAL_NULL => TjsValue::Object,
        _ => TjsValue::Object,
    }
}

/// Take ownership of the error the C ABI stored in an `out_error` slot.
///
/// Every C entry point fills that slot with either null or a malloc'd
/// NUL-terminated UTF-8 string, so taking it is an invariant of the ABI rather
/// than a caller obligation; the one `unsafe` deref and free live here.
fn take_error(e: *mut c_char) -> TjsError {
    TjsError(take_error_string(e))
}

/// As [`take_error`], as a plain `String`.
fn take_error_string(e: *mut c_char) -> String {
    if e.is_null() {
        return "unknown TJS error".into();
    }
    // SAFETY: the C ABI stores a malloc'd NUL-terminated UTF-8 string in the
    // out-error slot on failure.
    let msg = unsafe { CStr::from_ptr(e) }.to_string_lossy().into_owned();
    // SAFETY: the string came from the C allocator and the ABI pairs it with
    // `tjs2_free_string`; this is its only owner.
    unsafe { tjs2_free_string(e) };
    msg
}

/// Convert a [`TjsValue`] into the C-side `tjs2_value` struct. Strings are
/// NUL-terminated copies kept alive in `strings` for the duration of the
/// call. Object values cannot be reconstructed on the Rust side (no object
/// handle crosses the ABI) and are rejected.
fn value_to_ffi(v: &TjsValue, strings: &mut Vec<CString>) -> Result<Value, String> {
    let mut out = Value {
        ty: VAL_VOID,
        integer: 0,
        real: 0.0,
        string: ptr::null(),
        array: ptr::null(),
        array_count: 0,
        retained: 0,
    };
    match v {
        TjsValue::Void => {}
        TjsValue::Integer(i) => {
            out.ty = VAL_INTEGER;
            out.integer = *i;
        }
        TjsValue::Real(r) => {
            out.ty = VAL_REAL;
            out.real = *r;
        }
        TjsValue::String(s) => {
            let c =
                CString::new(s.as_str()).map_err(|_| "string contains a NUL byte".to_string())?;
            out.ty = VAL_STRING;
            out.string = c.as_ptr();
            strings.push(c);
        }
        TjsValue::Object => {
            return Err(
                "object values cannot be passed as arguments (no object handle crosses the ABI)"
                    .into(),
            );
        }
        TjsValue::Retained(id) => {
            // Pass a retained value id; the C++ side copies the retained
            // variant into the argument slot (VAL_RETAINED path in
            // `value_to_variant`), consuming the retention. Checked so a
            // 64-bit id cannot silently truncate on a 32-bit target (Android
            // armeabi-v7a).
            let retained = usize::try_from(*id)
                .map_err(|_| "retained id does not fit in usize on this platform".to_string())?;
            out.ty = VAL_RETAINED;
            out.integer = 0;
            out.real = 0.0;
            out.string = ptr::null();
            out.array = ptr::null();
            out.array_count = 0;
            out.retained = retained;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C++ TJS2 VM (like the reference) is single-threaded by design:
    /// concurrent engines in one process corrupt the C++ heap. These tests
    /// are fast (31 tests ~0.1s), so serialize them with one process-wide
    /// mutex and let every other crate keep full parallel speed.
    static VM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn vm_lock() -> std::sync::MutexGuard<'static, ()> {
        VM_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn engine_creates_and_destroys() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().expect("create engine");
        drop(e);
    }

    #[test]
    fn evaluates_integer_expression() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        let v = e.eval("1 + 2 * 3", "test").unwrap();
        assert_eq!(v, TjsValue::Integer(7));
    }

    // The script/expression payload crosses the ABI as a byte range, so an
    // interior NUL is not rejected by the wrapper. The VM copies the text as
    // a NUL-terminated buffer (reference
    // `reference/cpp/core/tjs2/tjsScriptBlock.cpp:198-263`), so the truncated
    // prefix is what its lexer sees and the failure is a VM compile error, not
    // a host-side rejection.
    #[test]
    fn interior_nul_reaches_the_vm_and_fails_there() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        // The text before the NUL is an incomplete expression, so the VM
        // raises a compile error under either truncation or direct lexing.
        let err = e.eval("1 +\0 2", "nul-test").unwrap_err();
        let message = err.to_string();
        assert!(
            !message.contains("contains NUL"),
            "the ABI must not pre-reject the payload: {message}"
        );
        assert!(
            message.contains("nul-test"),
            "the block name must still label the VM error: {message}"
        );
    }

    // The VM stores the block text as a NUL-terminated buffer
    // (reference `reference/cpp/core/tjs2/tjsScriptBlock.cpp:198-263`), so an
    // interior NUL is cut by the VM itself, not by the wrapper: the bytes
    // after it are never lexed. This documents the representation limit the
    // reference shares.
    #[test]
    fn interior_nul_is_cut_by_the_vm_not_the_wrapper() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        assert_eq!(
            e.eval("1 + 2\0 + 100", "cut-test").unwrap(),
            TjsValue::Integer(3)
        );
    }

    // A trailing NUL is the ordinary terminator of the VM's text buffer, so
    // it does not change the executed script (reference
    // `reference/cpp/core/tjs2/tjsScriptBlock.cpp:198-263`).
    #[test]
    fn trailing_nul_does_not_change_the_result() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        assert_eq!(
            e.eval("1 + 2\0", "trail-test").unwrap(),
            TjsValue::Integer(3)
        );
    }

    // The length hand-off must leave an ordinary script untouched.
    #[test]
    fn plain_script_is_unaffected_by_the_length_hand_off() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        assert_eq!(
            e.eval("1 + 2 * 3", "plain-expr").unwrap(),
            TjsValue::Integer(7)
        );
        e.exec_script("var plainValue = 41; plainValue += 1;", "plain-script")
            .expect("a plain script must execute");
        assert_eq!(
            e.eval("plainValue", "plain-read").unwrap(),
            TjsValue::Integer(42)
        );
    }

    // `compile_script` also takes the payload as a byte range; an interior NUL
    // is left for the VM, not rejected here, and the normal compile is
    // unaffected (reference `reference/cpp/core/tjs2/tjs.h:193-197`).
    #[test]
    fn compile_script_hands_an_interior_nul_to_the_vm() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "krkr-rs-tjs2-compile-nul-{}-{}.tjsb",
            std::process::id(),
            nanos
        ));
        let path_str = path.to_string_lossy().into_owned();

        let err = e
            .compile_script(
                "var x = \0 1;",
                &path_str,
                CompileOptions {
                    isresult: false,
                    outputdebug: false,
                    isexpression: false,
                    name: "nul-compile",
                    lineofs: 0,
                },
            )
            .unwrap_err();
        assert!(
            !err.to_string().contains("contains NUL"),
            "compile must not pre-reject the payload: {err}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn evaluates_string_expression() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        let v = e.eval("'hello' + ' ' + 'world'", "test").unwrap();
        assert_eq!(v, TjsValue::String("hello world".into()));
    }

    #[test]
    fn executes_script_with_global_assignment() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var x = 40; x += 2;", "test").unwrap();
        let v = e.eval("x", "test").unwrap();
        assert_eq!(v, TjsValue::Integer(42));
    }

    #[test]
    fn script_error_is_reported() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        let err = e.exec_script("this is not valid tjs", "test").unwrap_err();
        assert!(!err.to_string().is_empty());
    }

    #[test]
    fn unicode_strings_roundtrip() {
        let _vm_lock = vm_lock();
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
        let _vm_lock = vm_lock();
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
        let _vm_lock = vm_lock();
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

    // Captured `(level, message)` records from the engine's log callback,
    // per thread (the tests run in parallel across threads).
    thread_local! {
        static VM_LOG_RECORDS: std::cell::RefCell<Vec<(c_int, String)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    extern "C" fn record_vm_log(level: c_int, msg: *const c_char, _user: *mut c_void) {
        if msg.is_null() {
            return;
        }
        // SAFETY: msg is a NUL-terminated UTF-8 string valid for the call.
        let s = unsafe { CStr::from_ptr(msg) }
            .to_string_lossy()
            .into_owned();
        VM_LOG_RECORDS.with(|logs| logs.borrow_mut().push((level, s)));
    }

    /// A `try`-caught VM exception is raised as an `eTJS` error and reaches
    /// the `catch(eTJS&)` arm, which logs the message with `spdlog::debug`.
    /// The ABI bridge must forward that record to the engine's log callback
    /// at DEBUG, with the message intact, so a device running at `debug`
    /// records it in the engine log. Reference:
    /// `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1311`.
    #[test]
    fn caught_exception_reaches_the_log_callback_as_debug() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&test_natives_builder()).unwrap();
        // SAFETY: the callback is a plain fn with no user pointer needs.
        unsafe { e.set_log_cb(Some(record_vm_log), ptr::null_mut()) };
        VM_LOG_RECORDS.with(|logs| logs.borrow_mut().clear());

        e.exec_script(
            "var r = ''; try { TestNatives.add(1); } catch(e) { r = 'caught'; }",
            "test",
        )
        .unwrap();
        assert_eq!(
            e.eval("r", "test").unwrap(),
            TjsValue::String("caught".into())
        );

        let records = VM_LOG_RECORDS.with(|logs| logs.borrow().clone());
        // The exact message (without the source position the console trace
        // path appends) is the record the spdlog bridge adds; debug-mode
        // builds also emit a `... at position` console record.
        assert!(
            records
                .iter()
                .any(|(level, msg)| *level == LOG_DEBUG && msg == "add requires 2 arguments"),
            "the caught exception must reach the log callback as debug: {records:?}"
        );
    }

    /// The uncaught path is unchanged: the VM reports the exception through
    /// the returned error and prints the diagnostic disassembly dump, not the
    /// caught-exception debug record. Reference:
    /// `reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1313`.
    #[test]
    fn uncaught_exception_still_surfaces_the_error_and_dump() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&test_natives_builder()).unwrap();
        // SAFETY: the callback is a plain fn with no user pointer needs.
        unsafe { e.set_log_cb(Some(record_vm_log), ptr::null_mut()) };
        VM_LOG_RECORDS.with(|logs| logs.borrow_mut().clear());

        let err = e.eval("TestNatives.add(1)", "test").unwrap_err();
        assert!(
            err.to_string().contains("add requires 2 arguments"),
            "the uncaught error must still surface: {err}"
        );

        let records = VM_LOG_RECORDS.with(|logs| logs.borrow().clone());
        assert!(
            records
                .iter()
                .any(|(level, msg)| *level == LOG_DEBUG && msg.contains("An exception occurred")),
            "the uncaught path still prints the disassembly dump: {records:?}"
        );
    }

    #[test]
    fn native_class_rejects_duplicate_registration() {
        let _vm_lock = vm_lock();
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
        let _vm_lock = vm_lock();
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
        let _vm_lock = vm_lock();
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
    static COUNTER_VALUE_SET: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    /// `Counter()` constructor member — lets a script subclass call
    /// `super.Counter()`, which creates + registers the native instance.
    extern "C" fn counter_ctor(
        _engine: *mut c_void,
        _instance: *mut c_void,
        _argc: c_int,
        _argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
        _objthis: *mut c_void,
    ) -> c_int {
        // SAFETY: out is a valid return slot.
        unsafe { (*out).ty = VAL_VOID };
        0
    }

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
        _objthis: *mut c_void,
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
        _objthis: *mut c_void,
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
        _objthis: *mut c_void,
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

    /// `Counter.out(v)`: writes `2 * counter` back into argument 0 through
    /// the `tjs2_set_arg` out-parameter ABI (the reference's
    /// `(*param[0]) = value` pattern).
    extern "C" fn counter_out(
        engine: *mut c_void,
        instance: *mut c_void,
        argc: c_int,
        _argv: *const Value,
        out: *mut Value,
        out_error: *mut *mut c_char,
        _objthis: *mut c_void,
    ) -> c_int {
        if argc < 1 {
            unsafe { *out_error = alloc_error_string("Counter.out requires 1 argument") };
            return 1;
        }
        let c = unsafe { *counter_ptr(instance) };
        let written = Value {
            ty: VAL_INTEGER,
            integer: i64::from(c) * 2,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut err: *mut c_char = ptr::null_mut();
        // SAFETY: `engine` is the live engine for this callback and a native
        // call frame is active (the trampoline pushes it around the fn call).
        let rc = unsafe { tjs2_set_arg(engine as *mut Engine, 0, &written, &mut err) };
        if rc != 0 {
            unsafe { *out_error = err };
            return 1;
        }
        unsafe {
            (*out).ty = VAL_INTEGER;
            (*out).integer = 1;
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
            invalidate: None,
            methods: vec![
                NativeInstanceMethodDef {
                    name: "Counter",
                    f: counter_ctor,
                },
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
                NativeInstanceMethodDef {
                    name: "out",
                    f: counter_out,
                },
                NativeInstanceMethodDef {
                    name: "objthis",
                    f: counter_objthis,
                },
            ],
            properties: vec![
                NativeInstancePropertyDef {
                    name: "value",
                    get: Some(counter_value_get),
                    set: Some(counter_value_set),
                },
                NativeInstancePropertyDef {
                    name: "readonly",
                    get: Some(counter_readonly_get),
                    set: None,
                },
            ],
        }
    }

    extern "C" fn counter_value_get(
        _engine: *mut c_void,
        instance: *mut c_void,
        out: *mut Value,
        _out_error: *mut *mut c_char,
        _objthis: *mut c_void,
    ) -> c_int {
        // SAFETY: instance is a valid Counter payload.
        let c = unsafe { &mut *counter_ptr(instance) };
        // SAFETY: out is a valid return slot.
        unsafe {
            (*out).ty = VAL_INTEGER;
            (*out).integer = i64::from(*c);
        }
        0
    }

    extern "C" fn counter_value_set(
        _engine: *mut c_void,
        instance: *mut c_void,
        value: *const Value,
        _out_error: *mut *mut c_char,
        _objthis: *mut c_void,
    ) -> c_int {
        // SAFETY: instance is a valid Counter payload; value is valid.
        let c = unsafe { &mut *counter_ptr(instance) };
        let v = unsafe { &*value };
        *c = v.integer as i32;
        COUNTER_VALUE_SET.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        0
    }

    extern "C" fn counter_readonly_get(
        _engine: *mut c_void,
        _instance: *mut c_void,
        out: *mut Value,
        _out_error: *mut *mut c_char,
        _objthis: *mut c_void,
    ) -> c_int {
        // SAFETY: out is a valid return slot.
        unsafe {
            (*out).ty = VAL_INTEGER;
            (*out).integer = 7;
        }
        0
    }

    #[test]
    fn native_instance_properties_work_from_scripts() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();

        e.exec_script(
            "var c = new Counter(); c.value = 42; var r = c.value;",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("r", "test").unwrap(), TjsValue::Integer(42));

        // read-only property: reads work, writes are denied.
        e.exec_script("var ro = c.readonly;", "test").unwrap();
        assert_eq!(e.eval("ro", "test").unwrap(), TjsValue::Integer(7));
        let write_err = e.exec_script("c.readonly = 1;", "test");
        assert!(write_err.is_err(), "write to read-only property must error");

        // per-instance state.
        e.exec_script(
            "var b = new Counter(); b.value = 5; var rb = b.value; var rc = c.value;",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("rb", "test").unwrap(), TjsValue::Integer(5));
        assert_eq!(e.eval("rc", "test").unwrap(), TjsValue::Integer(42));
    }

    #[test]
    fn native_instance_out_param_writes_back_to_the_caller() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        // `out(v)` must modify the caller's variable through the by-reference
        // argument slot, not the callback's copy. TJS2 only passes **local**
        // variables by reference; a global is a property read compiled into a
        // temporary, so the test uses a function-local variable and returns it.
        e.exec_script(
            "function probe() { var c = new Counter(); c.add(21); var v = 0; \
             var ok = c.out(v); return [v, ok]; } \
             var r = probe(); var probe_v = r[0]; var probe_ok = r[1];",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("probe_v", "test").unwrap(), TjsValue::Integer(42));
        assert_eq!(e.eval("probe_ok", "test").unwrap(), TjsValue::Integer(1));
    }

    #[test]
    fn native_instances_work_from_scripts() {
        let _vm_lock = vm_lock();
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
        let _vm_lock = vm_lock();
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
        let _vm_lock = vm_lock();
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
        let _vm_lock = vm_lock();
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

    /// Storing the result of a member call in a variable must not keep the
    /// receiver's native instance alive after the script drops its last
    /// reference.
    #[test]
    fn stored_member_result_does_not_pin_the_native_instance() {
        let _vm_lock = vm_lock();
        COUNTER_CREATED.store(0, std::sync::atomic::Ordering::SeqCst);
        COUNTER_DESTROYED.store(0, std::sync::atomic::Ordering::SeqCst);

        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        e.exec_script(
            "var c = new Counter(); c.inc(); var p = c.get(); c = null;",
            "test",
        )
        .unwrap();
        e.do_gc().unwrap();
        assert_eq!(
            COUNTER_DESTROYED.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the receiver must be destroyed after its last reference is dropped"
        );
    }

    /// `exec_script_discard` passes a null result slot, so the object produced
    /// by the script's last expression is not kept alive by the interpreter's
    /// result register. The native instance's `destroy` callback observes the
    /// release directly, without an engine call that would itself reset the
    /// result slot.
    #[test]
    fn exec_script_discard_does_not_pin_its_object_result() {
        let _vm_lock = vm_lock();
        COUNTER_CREATED.store(0, std::sync::atomic::Ordering::SeqCst);
        COUNTER_DESTROYED.store(0, std::sync::atomic::Ordering::SeqCst);

        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        e.exec_script_discard("global.discardedRan = 1; new Counter();", "discard")
            .unwrap();
        assert_eq!(
            COUNTER_CREATED.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the discarded script must still run"
        );
        e.do_gc().unwrap();
        assert_eq!(
            COUNTER_DESTROYED.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "a discarded object result must not stay rooted"
        );
        assert_eq!(
            e.eval("global.discardedRan", "test").unwrap(),
            TjsValue::Integer(1),
            "the discarded script's side effects must run"
        );
    }

    #[test]
    fn invalidated_native_instance_stops_dispatching_and_releases_on_destroy() {
        let _vm_lock = vm_lock();
        COUNTER_CREATED.store(0, std::sync::atomic::Ordering::SeqCst);
        COUNTER_DESTROYED.store(0, std::sync::atomic::Ordering::SeqCst);

        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        // `var m = c.inc` stores the method closure with ObjThis = c, so it
        // keeps `c` alive across `invalidate c`.
        e.exec_script(
            "var c = new Counter(); c.inc(); var m = c.inc; invalidate c;",
            "test",
        )
        .unwrap();
        // Reference lifecycle: `tTJSNativeInstance::Invalidate()` releases
        // resources but the native instance is destroyed by `Destruct()`
        // (tjsNative.h:35-49); the Rust payload is freed by the destructor,
        // NOT at invalidate. This keeps backing state alive for any
        // remaining finalizers.
        assert_eq!(
            COUNTER_DESTROYED.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "invalidate must not free the native payload"
        );
        // Direct dispatch on a finalized object is still refused (catchable),
        // rather than running on a torn-down instance.
        let err = e.exec_script("m();", "test").unwrap_err();
        assert!(
            err.to_string().contains("invalidated"),
            "expected an invalidated-instance error, got: {err}"
        );
        assert_eq!(
            COUNTER_DESTROYED.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "the failed dispatch must not have freed the payload"
        );
        // Releasing the last references must eventually run the destructor:
        // dropping the engine certainly does. Exactly one Rust destroy, no
        // double free.
        drop(e);
        assert_eq!(
            COUNTER_DESTROYED.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "payload must be destroyed exactly once"
        );
    }

    // -------------------------------------------------------------------
    // retained values (function objects)
    // -------------------------------------------------------------------

    #[test]
    fn retained_function_can_be_called() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var f = function(a, b) { return a + b; };", "test")
            .unwrap();
        // Evaluating the function's name yields an object value; the engine
        // resolves it against its most recent object result on retain.
        let f = e.eval("f", "test").unwrap();
        assert_eq!(f, TjsValue::Object);
        let id = e.retain_value(&f).unwrap();
        assert_eq!(
            e.call_value(&id, &[TjsValue::Integer(2), TjsValue::Integer(40)])
                .unwrap(),
            TjsValue::Integer(42)
        );
    }

    #[test]
    fn retained_function_mutates_globals_across_calls() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.exec_script(
            "var g = 0; var f = function() { g = g + 1; return g; };",
            "test",
        )
        .unwrap();
        let f = e.eval("f", "test").unwrap();
        let id = e.retain_value(&f).unwrap();
        assert_eq!(e.call_value(&id, &[]).unwrap(), TjsValue::Integer(1));
        assert_eq!(e.call_value(&id, &[]).unwrap(), TjsValue::Integer(2));
    }

    #[test]
    fn retained_noarg_function_returns_void() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var f = function() { };", "test").unwrap();
        let f = e.eval("f", "test").unwrap();
        let id = e.retain_value(&f).unwrap();
        assert_eq!(e.call_value(&id, &[]).unwrap(), TjsValue::Void);
    }

    #[test]
    fn retained_function_returns_string() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var f = function() { return 'hello from tjs'; };", "test")
            .unwrap();
        let f = e.eval("f", "test").unwrap();
        let id = e.retain_value(&f).unwrap();
        assert_eq!(
            e.call_value(&id, &[]).unwrap(),
            TjsValue::String("hello from tjs".into())
        );
    }

    #[test]
    fn eval_retained_returns_callable_function_and_scalars() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var f = function(a, b) { return a + b; };", "test")
            .unwrap();

        // Object/function result: retained (RAII) and callable.
        let f = e.eval_retained("f", "test").unwrap();
        let RetainedValue::Object(f) = f else {
            panic!("eval_retained('f') must be an object")
        };
        assert_eq!(unsafe { tjs2_retained_count(e.inner) }, 1);
        assert_eq!(
            e.call_detached(&f, &[TjsValue::Integer(2), TjsValue::Integer(40)])
                .unwrap(),
            TjsValue::Integer(42)
        );
        drop(f);
        assert_eq!(unsafe { tjs2_retained_count(e.inner) }, 0);

        // Scalar results pass through and never allocate a retention.
        assert!(matches!(
            e.eval_retained("6 * 7", "test").unwrap(),
            RetainedValue::Value(TjsValue::Integer(42))
        ));
        assert!(matches!(
            e.eval_retained("'a' + 'b'", "test").unwrap(),
            RetainedValue::Value(TjsValue::String(s)) if s == "ab"
        ));
        e.exec_script("function nothing() { }", "test").unwrap();
        assert!(matches!(
            e.eval_retained("nothing()", "test").unwrap(),
            RetainedValue::Value(TjsValue::Void)
        ));
        assert_eq!(unsafe { tjs2_retained_count(e.inner) }, 0);

        // A scalar eval after an object eval does not resurrect the object
        // (`tjs2_eval` clears `last_object` at entry).
        assert!(matches!(
            e.eval_retained("1 + 1", "test").unwrap(),
            RetainedValue::Value(TjsValue::Integer(2))
        ));
        assert_eq!(unsafe { tjs2_retained_count(e.inner) }, 0);
    }

    #[test]
    fn exec_script_retained_returns_object_member() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();

        let r = e
            .exec_script_retained("return %[a: 1, b: 'x'];", "test")
            .unwrap();
        let RetainedValue::Object(dv) = r else {
            panic!("exec_script_retained must return an object")
        };
        assert_eq!(e.member(dv.raw_id(), "a").unwrap(), TjsValue::Integer(1));
        assert_eq!(
            e.member(dv.raw_id(), "b").unwrap(),
            TjsValue::String("x".into())
        );
        drop(dv);
        assert_eq!(unsafe { tjs2_retained_count(e.inner) }, 0);

        // A void script returns a scalar value, not a retention.
        assert!(matches!(
            e.exec_script_retained("var x = 1;", "test").unwrap(),
            RetainedValue::Value(TjsValue::Void)
        ));
        assert_eq!(unsafe { tjs2_retained_count(e.inner) }, 0);
    }

    #[test]
    fn calling_a_released_value_errors() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var f = function() { return 1; };", "test")
            .unwrap();
        let f = e.eval("f", "test").unwrap();
        let id = e.retain_value(&f).unwrap();

        // Releasing twice is an error (the second id is no longer
        // registered on this engine).
        e.release_value(&id).unwrap();
        let err = e.release_value(&id).unwrap_err();
        assert!(err.contains("already released"), "unexpected: {err}");

        // Calling a released id is an error, not a crash.
        let err = e.call_value(&id, &[]).unwrap_err();
        assert!(err.contains("invalid retained value"), "unexpected: {err}");

        // Dropping the id afterwards is still safe: the C++-level release
        // is idempotent.
        drop(id);
    }

    #[test]
    fn ffi_call_value_with_unknown_id_errors() {
        let _vm_lock = vm_lock();
        // The C++ side itself must reject unknown ids gracefully (the safe
        // wrapper catches this before crossing the FFI, but the boundary
        // contract is what matters for the Timer use case).
        let e = Tjs2Engine::new().unwrap();
        let mut out = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: e.inner is a live engine; the id is deliberately bogus.
        let rc = unsafe {
            tjs2_call_value(
                e.inner,
                Tjs2ValueId::null(),
                0,
                ptr::null(),
                &mut out,
                &mut error,
            )
        };
        assert_ne!(rc, 0);
        // SAFETY: error was filled by the C++ side (or left null).
        let msg = take_error_string(error);
        assert!(msg.contains("invalid retained value"), "unexpected: {msg}");
    }

    #[test]
    fn engine_drop_releases_retained_values() {
        let _vm_lock = vm_lock();
        // A live ValueId borrows its engine, so "drop the engine while ids
        // are alive" cannot even be expressed. What can happen is a
        // forgotten id (std::mem::forget skips Drop); the engine's registry
        // still releases every retained id at engine drop — no leak, no
        // crash.
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var f = function() { return 42; };", "test")
            .unwrap();
        let f = e.eval("f", "test").unwrap();
        let id = e.retain_value(&f).unwrap();
        std::mem::forget(id);
        drop(e);

        // Normal path: ids dropped before the engine, then the engine
        // drops.
        let e2 = Tjs2Engine::new().unwrap();
        e2.exec_script("var f = function() { return 7; };", "test")
            .unwrap();
        let f2 = e2.eval("f", "test").unwrap();
        let id2 = e2.retain_value(&f2).unwrap();
        assert_eq!(e2.call_value(&id2, &[]).unwrap(), TjsValue::Integer(7));
        drop(id2);
        drop(e2);
    }

    // -------------------------------------------------------------------
    // FFI audit: retained-value consumption, scratch lifetimes, reentrancy
    // -------------------------------------------------------------------

    /// The objthis of the last instance method call (raw pointer as usize).
    static LAST_OBJTHIS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    /// `Counter.objthis()` — returns the raw objthis pointer value and
    /// records it, so tests can check the trailing C ABI arg is the real
    /// per-instance object (non-null, distinct per instance).
    extern "C" fn counter_objthis(
        _engine: *mut c_void,
        _instance: *mut c_void,
        _argc: c_int,
        _argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
        objthis: *mut c_void,
    ) -> c_int {
        LAST_OBJTHIS.store(objthis as usize, std::sync::atomic::Ordering::SeqCst);
        // SAFETY: out is a valid return slot.
        unsafe {
            (*out).ty = VAL_INTEGER;
            (*out).integer = objthis as usize as i64;
            (*out).real = 0.0;
            (*out).string = ptr::null();
        }
        0
    }

    thread_local! {
        /// Scratch state for array-string returns (mirrors the tvp-storages
        /// CSVParser helper): NUL-terminated buffers plus the pointer array
        /// into them. Valid until the next native call on this thread; the
        /// C++ side copies the elements into a TJS array before the callback
        /// returns.
        static ARRAY_OUT: std::cell::RefCell<(Vec<*const c_char>, Vec<Vec<u8>>)> =
            const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
    }

    /// Write an array-of-strings return value into `*out` (VAL_ARRAY).
    fn set_array_strings_out(out: *mut Value, items: &[String]) {
        ARRAY_OUT.with(|slot| {
            let mut slot = slot.borrow_mut();
            slot.1.clear();
            let mut ptrs: Vec<*const c_char> = Vec::with_capacity(items.len());
            for s in items {
                let mut b = s.as_bytes().to_vec();
                b.push(0);
                slot.1.push(b);
            }
            for b in &slot.1 {
                ptrs.push(b.as_ptr() as *const c_char);
            }
            slot.0 = ptrs;
            // SAFETY: out is a valid return slot; slot.0/slot.1 stay alive
            // until the next call on this thread (the C++ side copies
            // immediately).
            unsafe {
                (*out).ty = VAL_ARRAY;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = ptr::null();
                (*out).array = slot.0.as_ptr();
                (*out).array_count = slot.0.len() as c_int;
            }
        });
    }

    /// `RetainNatives.retainFirst(x)` — retains the (object) argument and
    /// returns it across the ABI as VAL_RETAINED. This is the exact pattern
    /// `Scripts.evalStorage` / `Layer.font` use: the C++ side consumes the
    /// map entry (copies + erases) when it converts the result.
    extern "C" fn native_retain_first_arg(
        engine: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        if argc < 1 {
            return 1;
        }
        // SAFETY: argv is valid for argc entries during the call.
        let first = unsafe { &*argv };
        if first.ty != VAL_OBJECT {
            return 1;
        }
        // A VAL_OBJECT tjs2_value carries no handle; the C++ side resolves
        // it against the engine's most recent object result — the argument
        // conversion just stored this object there.
        let ffi = Value {
            ty: VAL_OBJECT,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        // SAFETY: engine is a live engine; ffi mirrors the object arg.
        let id = unsafe { tjs2_retain_value(engine as *mut Engine, &ffi) };
        if id.is_null() {
            return 1;
        }
        // SAFETY: out is a valid return slot; the C++ side consumes the
        // retention before the callback returns.
        unsafe {
            (*out).ty = VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = id.as_ptr() as usize;
        }
        0
    }

    /// `RetainNatives.reentrant(x)` — retains its object argument, then
    /// re-enters the VM with an eval that itself runs another native retain
    /// (nested retain + consume), then returns the FIRST retention. If the
    /// nested eval corrupted the in-flight retention, the caller gets the
    /// wrong object.
    extern "C" fn native_reentrant(
        engine: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        if argc < 1 || unsafe { &*argv }.ty != VAL_OBJECT {
            return 1;
        }
        let ffi = Value {
            ty: VAL_OBJECT,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        // SAFETY: engine is a live engine.
        let id = unsafe { tjs2_retain_value(engine as *mut Engine, &ffi) };
        if id.is_null() {
            return 1;
        }
        // Re-enter the VM from inside the callback: the eval clobbers
        // last_object and runs a nested native that retains + consumes its
        // own object.
        let script = c"RetainNatives.retainFirst(%[nested: 1]); %[ev: 99]";
        let mut inner = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut err: *mut c_char = ptr::null_mut();
        // SAFETY: engine is a live engine; the strings are NUL-terminated
        // for the call.
        let rc = unsafe {
            tjs2_eval(
                engine as *mut Engine,
                script.as_ptr(),
                script.to_bytes().len(),
                c"reentrant".as_ptr(),
                ptr::null_mut(),
                0,
                &mut inner,
                &mut err,
            )
        };
        if rc != 0 {
            // SAFETY: err is malloc'd by the C++ side (or null).
            let msg = take_error_string(err);
            panic!("reentrant eval failed: {msg}");
        }
        // SAFETY: out is a valid return slot.
        unsafe {
            (*out).ty = VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = id.as_ptr() as usize;
        }
        0
    }

    /// `RetainNatives.retainProp` (static property getter): evaluates a
    /// fresh dict and returns it as VAL_RETAINED — the `Layer.font` pattern,
    /// reachable as a bare statement (discarded result) or an assigned read.
    extern "C" fn native_retain_prop_get(
        engine: *mut c_void,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        let mut inner = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut err: *mut c_char = ptr::null_mut();
        // SAFETY: engine is a live engine; the strings are NUL-terminated.
        let rc = unsafe {
            tjs2_eval(
                engine as *mut Engine,
                c"%[]".as_ptr(),
                c"%[]".to_bytes().len(),
                c"retainProp".as_ptr(),
                ptr::null_mut(),
                0,
                &mut inner,
                &mut err,
            )
        };
        if rc != 0 {
            return 1;
        }
        let ffi = Value {
            ty: VAL_OBJECT,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        // SAFETY: engine is a live engine; the eval just stored the dict in
        // last_object.
        let id = unsafe { tjs2_retain_value(engine as *mut Engine, &ffi) };
        if id.is_null() {
            return 1;
        }
        // SAFETY: out is a valid return slot; the C++ side consumes the
        // retention before the callback returns.
        unsafe {
            (*out).ty = VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = id.as_ptr() as usize;
        }
        0
    }

    fn retain_builder() -> NativeClassBuilder<'static> {
        NativeClassBuilder {
            name: "RetainNatives",
            methods: vec![
                NativeMethodDef {
                    name: "retainFirst",
                    f: native_retain_first_arg,
                },
                NativeMethodDef {
                    name: "reentrant",
                    f: native_reentrant,
                },
            ],
            properties: vec![NativePropertyDef {
                name: "retainProp",
                get: Some(native_retain_prop_get),
                set: None,
            }],
        }
    }

    /// Retain `args[index]` via its per-argument object handle and return it
    /// across the ABI as VAL_RETAINED (the C++ side consumes the retention
    /// when it converts the native result).
    fn retain_arg_and_return(
        engine: *mut c_void,
        args: &[Value],
        index: usize,
        out: *mut Value,
    ) -> c_int {
        let Some(arg) = args.get(index) else {
            return 1;
        };
        // Exercise the raw callback form of Tjs2Engine::retain_object_arg.
        let dv = match unsafe { retain_object_arg_raw(engine as *mut Engine, arg) } {
            Ok(dv) => dv,
            Err(_) => return 1,
        };
        // SAFETY: out is a valid return slot; the C++ side consumes the
        // retention after the callback returns.
        unsafe {
            (*out).ty = VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = dv.raw_id().as_ptr() as usize;
        }
        std::mem::forget(dv);
        0
    }

    /// `ArgNatives.pickFirst(a, b)` — retains the FIRST object argument.
    extern "C" fn native_pick_first(
        engine: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        if argc < 2 {
            return 1;
        }
        // SAFETY: argv is valid for argc entries during the call.
        let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
        retain_arg_and_return(engine, args, 0, out)
    }

    /// `ArgNatives.pickLast(a, b)` — retains the SECOND object argument.
    extern "C" fn native_pick_last(
        engine: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        if argc < 2 {
            return 1;
        }
        // SAFETY: argv is valid for argc entries during the call.
        let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
        retain_arg_and_return(engine, args, 1, out)
    }

    /// `ArgNatives.classify(x)` — encodes the argument kind and whether it
    /// carries a raw object handle (non-objects must carry none).
    extern "C" fn native_classify(
        _engine: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        let code: i64 = if argc < 1 {
            -1
        } else {
            // SAFETY: argv is valid for at least one entry during the call.
            let a = unsafe { &*argv };
            match a.ty {
                VAL_INTEGER => 100 + a.integer,
                VAL_STRING => 200 + a.object_handle().is_null() as i64,
                VAL_OBJECT => 300 + a.object_handle().is_null() as i64,
                VAL_NULL => 500,
                VAL_OCTET => 600,
                _ => 400,
            }
        };
        // SAFETY: out is a valid return slot.
        unsafe {
            (*out).ty = VAL_INTEGER;
            (*out).integer = code;
            (*out).real = 0.0;
            (*out).string = ptr::null();
        }
        0
    }

    fn arg_builder() -> NativeClassBuilder<'static> {
        NativeClassBuilder {
            name: "ArgNatives",
            methods: vec![
                NativeMethodDef {
                    name: "pickFirst",
                    f: native_pick_first,
                },
                NativeMethodDef {
                    name: "pickLast",
                    f: native_pick_last,
                },
                NativeMethodDef {
                    name: "classify",
                    f: native_classify,
                },
            ],
            properties: vec![],
        }
    }

    /// Instance class with a `list(n)` method returning an array of `n`
    /// strings (mirrors CSVParser.getNextLine).
    extern "C" fn array_create(_engine: *mut c_void) -> *mut c_void {
        Box::into_raw(Box::new(0i32)) as *mut c_void
    }

    extern "C" fn array_destroy(_engine: *mut c_void, instance: *mut c_void) {
        // SAFETY: instance came from array_create.
        unsafe { drop(Box::from_raw(instance as *mut i32)) };
    }

    /// `ArrayNatives.list(n)` — returns n distinct strings.
    extern "C" fn array_list(
        _engine: *mut c_void,
        _instance: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
        _objthis: *mut c_void,
    ) -> c_int {
        let n = if argc >= 1 {
            // SAFETY: argv is valid for argc entries during the call.
            unsafe { &*argv }.integer.max(0) as usize
        } else {
            0
        };
        let items: Vec<String> = (0..n)
            .map(|i| format!("item-{i}-{}", "x".repeat(i % 7)))
            .collect();
        set_array_strings_out(out, &items);
        0
    }

    /// `ArrayNatives.ret` (instance property getter): returns a fresh
    /// retained dict — the `Window.primaryLayer` pattern, reachable as a
    /// bare statement (discarded result).
    extern "C" fn array_ret_get(
        engine: *mut c_void,
        _instance: *mut c_void,
        out: *mut Value,
        _out_error: *mut *mut c_char,
        _objthis: *mut c_void,
    ) -> c_int {
        let mut inner = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut err: *mut c_char = ptr::null_mut();
        // SAFETY: engine is a live engine; the strings are NUL-terminated.
        let rc = unsafe {
            tjs2_eval(
                engine as *mut Engine,
                c"%[]".as_ptr(),
                c"%[]".to_bytes().len(),
                c"arrayRet".as_ptr(),
                ptr::null_mut(),
                0,
                &mut inner,
                &mut err,
            )
        };
        if rc != 0 {
            return 1;
        }
        let ffi = Value {
            ty: VAL_OBJECT,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        // SAFETY: engine is a live engine.
        let id = unsafe { tjs2_retain_value(engine as *mut Engine, &ffi) };
        if id.is_null() {
            return 1;
        }
        // SAFETY: out is a valid return slot.
        unsafe {
            (*out).ty = VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = id.as_ptr() as usize;
        }
        0
    }

    fn array_builder() -> NativeInstanceBuilder<'static> {
        NativeInstanceBuilder {
            name: "ArrayNatives",
            create: array_create,
            destroy: array_destroy,
            invalidate: None,
            methods: vec![NativeInstanceMethodDef {
                name: "list",
                f: array_list,
            }],
            properties: vec![NativeInstancePropertyDef {
                name: "ret",
                get: Some(array_ret_get),
                set: None,
            }],
        }
    }

    #[test]
    fn static_native_returns_retained_object_usable_from_script() {
        let _vm_lock = vm_lock();
        // The Scripts.evalStorage / Layer.font pattern: a static native
        // retains an object argument and returns it as VAL_RETAINED; the
        // C++ side consumes the retention. The returned object must be the
        // ORIGINAL (not a placeholder) and stay usable/mutable.
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&retain_builder()).unwrap();
        e.exec_script(
            "var r = RetainNatives.retainFirst(%[a: 1]); var a = r.a; r.b = 42; var b = r.b;",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("a", "test").unwrap(), TjsValue::Integer(1));
        assert_eq!(e.eval("b", "test").unwrap(), TjsValue::Integer(42));
        // still the same object across engine calls
        e.exec_script("r.c = 7;", "test").unwrap();
        assert_eq!(e.eval("r.c", "test").unwrap(), TjsValue::Integer(7));
    }

    #[test]
    fn two_object_args_retain_each_argument_individually() {
        let _vm_lock = vm_lock();
        // Regression: variant_to_value_one only remembered the LAST object
        // argument in last_object, so retaining the first of
        // `drawPolygon(app, points)` resolved to `points`. Each argument now
        // carries its own raw object handle.
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&arg_builder()).unwrap();
        e.exec_script(
            "var first = %[id: 1]; var second = %[id: 2]; \
             var fa = ArgNatives.pickFirst(first, second); \
             var la = ArgNatives.pickLast(first, second);",
            "test",
        )
        .unwrap();
        // pickFirst must resolve `first` (id 1), not the last argument.
        assert_eq!(e.eval("fa.id", "test").unwrap(), TjsValue::Integer(1));
        assert_eq!(e.eval("la.id", "test").unwrap(), TjsValue::Integer(2));
        // The returned handle is the ORIGINAL object, not a copy.
        e.exec_script("fa.id = 11;", "test").unwrap();
        assert_eq!(e.eval("first.id", "test").unwrap(), TjsValue::Integer(11));
        assert_eq!(e.eval("second.id", "test").unwrap(), TjsValue::Integer(2));
    }

    #[test]
    fn scalar_and_string_args_carry_no_object_handle() {
        let _vm_lock = vm_lock();
        // (c) scalar/string marshalling is unchanged and carries no handle;
        // object args do carry one.
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&arg_builder()).unwrap();
        e.exec_script(
            "var ci = ArgNatives.classify(5); \
             var cs = ArgNatives.classify('x'); \
             var co = ArgNatives.classify(%[k: 1]);",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("ci", "test").unwrap(), TjsValue::Integer(105));
        assert_eq!(e.eval("cs", "test").unwrap(), TjsValue::Integer(201));
        // object: 300 + (handle is null ? 1 : 0) => handle present => 300
        assert_eq!(e.eval("co", "test").unwrap(), TjsValue::Integer(300));
    }

    #[test]
    fn null_object_arg_is_distinct_from_void_and_object() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&arg_builder()).unwrap();
        // TJS `null` is `tvtObject` with Object == nullptr: it must not be
        // reported as VAL_OBJECT with a zero handle (which would make
        // retain_object_arg fall back to last_object and keep the wrong
        // object), nor as void.
        e.exec_script(
            "var cn = ArgNatives.classify(null); \
             var co = ArgNatives.classify(%[k: 1]);",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("cn", "test").unwrap(), TjsValue::Integer(500));
        assert_eq!(e.eval("co", "test").unwrap(), TjsValue::Integer(300));
    }

    #[test]
    fn discarded_retained_results_do_not_leak_in_the_map() {
        let _vm_lock = vm_lock();
        // The C++ retained-value map is the ground truth for retention
        // leaks: every retain must be consumed (assigned result) or
        // released (result discarded as a bare statement / bare property
        // read — the VM passes result==NULL there and the dispatch must
        // still release the entry). Repeated calls must not grow the map.
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&retain_builder()).unwrap();
        e.register_native_class_instance(&array_builder()).unwrap();
        // Assigned result: consumed by the conversion.
        e.exec_script(
            "var r = RetainNatives.retainFirst(%[a: 1]); r = null; %[];",
            "test",
        )
        .unwrap();
        assert_eq!(
            unsafe { tjs2_retained_count(e.inner) },
            0,
            "assigned retained result leaked"
        );
        // Bare statement (static method): the VM discards the result.
        for _ in 0..100 {
            e.exec_script("RetainNatives.retainFirst(%[b: 2]);", "test")
                .unwrap();
            assert_eq!(
                unsafe { tjs2_retained_count(e.inner) },
                0,
                "bare-statement retained result leaked"
            );
        }
        // Bare statement (static property getter; Layer.font pattern).
        for _ in 0..100 {
            e.exec_script("RetainNatives.retainProp;", "test").unwrap();
            assert_eq!(
                unsafe { tjs2_retained_count(e.inner) },
                0,
                "discarded static property result leaked"
            );
        }
        // Bare statement (instance property getter; primaryLayer pattern).
        e.exec_script("var a = new ArrayNatives();", "test")
            .unwrap();
        for _ in 0..100 {
            e.exec_script("a.ret;", "test").unwrap();
            assert_eq!(
                unsafe { tjs2_retained_count(e.inner) },
                0,
                "discarded instance property result leaked"
            );
        }
        // The map still works for live retentions afterwards.
        e.exec_script("var f = function() { return 3; };", "test")
            .unwrap();
        let f = e.eval("f", "test").unwrap();
        let id = e.retain_value(&f).unwrap();
        assert_eq!(e.call_value(&id, &[]).unwrap(), TjsValue::Integer(3));
        assert_eq!(unsafe { tjs2_retained_count(e.inner) }, 1);
        drop(id);
        assert_eq!(unsafe { tjs2_retained_count(e.inner) }, 0);
    }

    #[test]
    fn release_twice_and_unknown_id_are_noops() {
        let _vm_lock = vm_lock();
        // The C++ claim "release of an unknown id is a safe no-op" must
        // hold: double release of a live id, a raw id after the C++ side
        // consumed it, and completely bogus ids must not crash or corrupt
        // the engine.
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var f = function() { return 1; };", "test")
            .unwrap();
        let f = e.eval("f", "test").unwrap();
        let id = e.retain_value(&f).unwrap();
        let raw = id.id;
        drop(id); // first release
        // second release of the same raw id (no longer registered):
        unsafe { tjs2_release_value(e.inner, raw) };
        // never-allocated / null ids:
        unsafe {
            tjs2_release_value(
                e.inner,
                Tjs2ValueId::from_raw(std::ptr::dangling_mut::<c_void>()),
            )
        };
        unsafe { tjs2_release_value(e.inner, Tjs2ValueId::null()) };
        // engine still fully usable, and retains still work
        assert_eq!(e.eval("1 + 1", "test").unwrap(), TjsValue::Integer(2));
        e.exec_script("var g = function() { return 5; };", "test")
            .unwrap();
        let g = e.eval("g", "test").unwrap();
        let id2 = e.retain_value(&g).unwrap();
        assert_eq!(e.call_value(&id2, &[]).unwrap(), TjsValue::Integer(5));
    }

    #[test]
    fn ffi_call_value_with_retained_arg_errors_cleanly() {
        let _vm_lock = vm_lock();
        // The unsafe FFI entry point must tolerate a TJS2_VAL_RETAINED
        // argument: argument conversion consumes the map entry, so the
        // subsequent call-value lookup must fail cleanly instead of reading
        // through a dangling map iterator (the pre-fix UB).
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var f = function() { return 42; };", "test")
            .unwrap();
        let f = e.eval("f", "test").unwrap();
        let id = e.retain_value(&f).unwrap();
        // The argument is a RETAINED value for the SAME id being called.
        let retained_arg = Value {
            ty: VAL_RETAINED,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: id.id.as_ptr() as usize,
        };
        let mut out = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: e.inner is live; the id and arg are deliberately
        // self-referential.
        let rc = unsafe { tjs2_call_value(e.inner, id.id, 1, &retained_arg, &mut out, &mut error) };
        assert_ne!(rc, 0, "call with a consumed id must fail");
        // SAFETY: error was filled by the C++ side (or left null).
        let msg = take_error_string(error);
        assert!(msg.contains("invalid retained value"), "unexpected: {msg}");
        // The entry was consumed by the argument; the id's Drop release is
        // a no-op.
        drop(id);
        // The engine is still healthy and other retains still work.
        e.exec_script("var g = function() { return 9; };", "test")
            .unwrap();
        let g = e.eval("g", "test").unwrap();
        let id2 = e.retain_value(&g).unwrap();
        assert_eq!(e.call_value(&id2, &[]).unwrap(), TjsValue::Integer(9));
    }

    #[test]
    fn array_return_many_strings_repeated_calls() {
        let _vm_lock = vm_lock();
        // VAL_ARRAY results live in a thread-local scratch buffer that is
        // valid only until the next callback; repeated calls with different
        // sizes must not observe stale bytes from the previous call.
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&array_builder()).unwrap();
        e.exec_script("var a = new ArrayNatives();", "test")
            .unwrap();
        for n in [1usize, 50, 3, 1000, 7] {
            let script = format!("var arr = a.list({n}); var s = arr.join(',');");
            e.exec_script(&script, "test").unwrap();
            let got = match e.eval("s", "test").unwrap() {
                TjsValue::String(s) => s,
                other => panic!("list({n}) returned non-string: {other:?}"),
            };
            let expected: Vec<String> = (0..n)
                .map(|i| format!("item-{i}-{}", "x".repeat(i % 7)))
                .collect();
            assert_eq!(got, expected.join(","), "mismatch at n={n}");
            // element count round-trips
            let cnt = format!("var c = a.list({n}).count;");
            e.exec_script(&cnt, "test").unwrap();
            assert_eq!(e.eval("c", "test").unwrap(), TjsValue::Integer(n as i64));
        }
        // Interleave a string return between array returns (scratch reuse).
        e.exec_script("var s1 = a.list(2).join(',');", "test")
            .unwrap();
        assert_eq!(
            e.eval("s1", "test").unwrap(),
            TjsValue::String("item-0-,item-1-x".into())
        );
    }

    #[test]
    fn native_that_evals_during_callback_does_not_corrupt_retention() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&retain_builder()).unwrap();
        // Keep a function retained while a native re-enters the VM.
        e.exec_script("var f = function() { return 123; };", "test")
            .unwrap();
        let f = e.eval("f", "test").unwrap();
        let id = e.retain_value(&f).unwrap();
        // The native retains its argument, evals (nested native retain +
        // consume + last_object clobber), then returns the FIRST retention.
        e.exec_script("var r = RetainNatives.reentrant(%[a: 7]);", "test")
            .unwrap();
        // The returned object is the original dict, not the nested eval's.
        e.exec_script("var chk = r.a;", "test").unwrap();
        assert_eq!(e.eval("chk", "test").unwrap(), TjsValue::Integer(7));
        // The in-flight Rust-side retention survived the nested eval.
        assert_eq!(e.call_value(&id, &[]).unwrap(), TjsValue::Integer(123));
        // And the object returned by the reentrant native is still the
        // original: mutations on it are visible.
        e.exec_script("r.b = 99; var chk2 = r.b;", "test").unwrap();
        assert_eq!(e.eval("chk2", "test").unwrap(), TjsValue::Integer(99));
    }

    #[test]
    fn instance_property_on_class_object_errors_cleanly() {
        let _vm_lock = vm_lock();
        // An instance property dispatched with objthis = the class object
        // (no native instance behind it) must produce a clean TJS error,
        // not a crash, and the engine must stay usable.
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        let err = e.eval("Counter.value", "test").unwrap_err();
        assert!(!err.to_string().is_empty(), "expected an error: {err}");
        let err = e.eval("Counter.value = 5", "test").unwrap_err();
        assert!(!err.to_string().is_empty(), "expected an error: {err}");
        // engine still healthy afterwards
        e.exec_script("var c = new Counter(); c.value = 3;", "test")
            .unwrap();
        assert_eq!(e.eval("c.value", "test").unwrap(), TjsValue::Integer(3));
    }

    #[test]
    fn instance_callback_receives_valid_objthis() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        // Two distinct instances must produce distinct, non-null objthis
        // pointers on the trailing C ABI argument.
        e.exec_script(
            "var a = new Counter(); var b = new Counter(); var oa = a.objthis(); var ob = b.objthis();",
            "test",
        )
        .unwrap();
        let oa = match e.eval("oa", "test").unwrap() {
            TjsValue::Integer(v) => v,
            other => panic!("objthis returned {other:?}"),
        };
        let ob = match e.eval("ob", "test").unwrap() {
            TjsValue::Integer(v) => v,
            other => panic!("objthis returned {other:?}"),
        };
        assert_ne!(oa, 0, "objthis must be non-null");
        assert_ne!(ob, 0, "objthis must be non-null");
        assert_ne!(oa, ob, "different instances must have different objthis");
        // The recorded pointer matches the returned value.
        assert_eq!(
            LAST_OBJTHIS.load(std::sync::atomic::Ordering::SeqCst) as i64,
            ob
        );
    }

    // --- octets ---------------------------------------------------------

    /// `OctetNatives.make()` — builds a real TJS octet (4 bytes, including a
    /// NUL and a high byte) and returns it as a retained value; the C++ side
    /// converts the retention into a `tvtOctet`.
    extern "C" fn octet_make(
        engine: *mut c_void,
        _argc: c_int,
        _argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        const BYTES: [u8; 4] = [0x00, 0x01, 0x02, 0xFF];
        let ffi = Value {
            ty: VAL_OCTET,
            integer: 0,
            real: 0.0,
            string: BYTES.as_ptr() as *const c_char,
            array: ptr::null(),
            array_count: BYTES.len() as c_int,
            retained: 0,
        };
        // SAFETY: engine is a live engine; the C++ side copies the bytes
        // into a refcounted tTJSVariantOctet during the call.
        let id = unsafe { tjs2_retain_value(engine as *mut Engine, &ffi) };
        if id.is_null() {
            return 1;
        }
        // SAFETY: out is a valid return slot; the C++ side consumes the
        // retention when it converts the native result.
        unsafe {
            (*out).ty = VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = id.as_ptr() as usize;
        }
        0
    }

    /// `OctetNatives.sum(x)` — reads a `VAL_OCTET` argument through
    /// [`Value::octet_bytes`] and returns (sum * 1000 + length) so one
    /// integer checks both the bytes and the length.
    extern "C" fn octet_sum(
        _engine: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        if argc < 1 {
            return 1;
        }
        // SAFETY: argv is valid for argc entries during the call.
        let arg = unsafe { &*argv };
        if arg.ty != VAL_OCTET {
            return 1;
        }
        // SAFETY: the ABI guarantees the bytes live for the duration of the
        // callback.
        let bytes = unsafe { arg.octet_bytes() };
        let sum: i64 = bytes.map_or(0, |b| b.iter().map(|&x| i64::from(x)).sum());
        let len = bytes.map_or(0, |b| b.len() as i64);
        // SAFETY: out is a valid return slot.
        unsafe {
            (*out).ty = VAL_INTEGER;
            (*out).integer = sum * 1000 + len;
            (*out).real = 0.0;
            (*out).string = ptr::null();
        }
        0
    }

    fn octet_builder() -> NativeClassBuilder<'static> {
        NativeClassBuilder {
            name: "OctetNatives",
            methods: vec![
                NativeMethodDef {
                    name: "make",
                    f: octet_make,
                },
                NativeMethodDef {
                    name: "sum",
                    f: octet_sum,
                },
            ],
            properties: vec![],
        }
    }

    #[test]
    fn octet_arguments_and_results_round_trip() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&octet_builder()).unwrap();
        // The native-produced octet is a real TJS octet (`typeof` sees it),
        // not an opaque object or a string.
        assert_eq!(
            e.eval("typeof OctetNatives.make()", "test").unwrap(),
            TjsValue::String("Octet".into())
        );
        // Passing it back to a native exposes the exact bytes — the embedded
        // NUL and 0xFF included: sum(0,1,2,255)=258, length 4 => 258004.
        assert_eq!(
            e.eval("OctetNatives.sum(OctetNatives.make())", "test")
                .unwrap(),
            TjsValue::Integer(258_004)
        );
    }

    // --- property writes (tjs2_prop_set) -------------------------------

    #[test]
    fn prop_set_creates_and_reads_back_members_on_plain_object() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var o = %[];", "test").unwrap();
        let v = e.eval("o", "test").unwrap();
        let id = e.retain_value_detached(&v).unwrap();
        // New members are created (TJS_MEMBERENSURE); scalars and strings.
        e.set_member(id.raw_id(), "x", &TjsValue::Integer(42))
            .unwrap();
        e.set_member(id.raw_id(), "name", &TjsValue::String("hi".into()))
            .unwrap();
        e.set_member(id.raw_id(), "r", &TjsValue::Real(1.5))
            .unwrap();
        assert_eq!(e.member(id.raw_id(), "x").unwrap(), TjsValue::Integer(42));
        assert_eq!(
            e.member(id.raw_id(), "name").unwrap(),
            TjsValue::String("hi".into())
        );
        assert_eq!(e.member(id.raw_id(), "r").unwrap(), TjsValue::Real(1.5));
        // The writes are visible to the script's own reference to the object.
        e.exec_script("var got = o.x; var gotname = o.name;", "test")
            .unwrap();
        assert_eq!(e.eval("got", "test").unwrap(), TjsValue::Integer(42));
        assert_eq!(
            e.eval("gotname", "test").unwrap(),
            TjsValue::String("hi".into())
        );
    }

    #[test]
    fn prop_set_invokes_native_instance_property_setter() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        e.exec_script("var c = new Counter();", "test").unwrap();
        let v = e.eval("c", "test").unwrap();
        let id = e.retain_value_detached(&v).unwrap();
        // The write must run the native setter (Counter.value).
        e.set_member(id.raw_id(), "value", &TjsValue::Integer(7))
            .unwrap();
        assert_eq!(
            e.member(id.raw_id(), "value").unwrap(),
            TjsValue::Integer(7)
        );
        e.exec_script("var got = c.value;", "test").unwrap();
        assert_eq!(e.eval("got", "test").unwrap(), TjsValue::Integer(7));
        // Read-only native property: the write is denied as a clean error.
        let err = e
            .set_member(id.raw_id(), "readonly", &TjsValue::Integer(1))
            .unwrap_err();
        assert!(!err.is_empty(), "expected a denied-write error: {err}");
    }

    #[test]
    fn prop_set_accepts_retained_object_values() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var parent = %[]; var child = %[]; child.flag = 9;", "test")
            .unwrap();
        let p = e.eval("parent", "test").unwrap();
        let pid = e.retain_value_detached(&p).unwrap();
        let c = e.eval("child", "test").unwrap();
        let cid = e.retain_value_detached(&c).unwrap();
        // parent.child = <retained child>; the VAL_RETAINED conversion
        // consumes the child retention.
        e.set_member(
            pid.raw_id(),
            "child",
            &TjsValue::Retained(cid.raw_id().as_ptr() as u64),
        )
        .unwrap();
        e.exec_script("var f = parent.child.flag;", "test").unwrap();
        assert_eq!(e.eval("f", "test").unwrap(), TjsValue::Integer(9));
    }

    #[test]
    fn prop_set_errors_cleanly_for_null_and_invalidated_objects() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        // Unknown/null retained id.
        let err = e
            .set_member(Tjs2ValueId::null(), "x", &TjsValue::Integer(1))
            .unwrap_err();
        assert!(!err.is_empty(), "null id must error: {err}");
        // Invalidated object: finalize deletes its members, so PropSet returns
        // TJS_E_INVALIDOBJECT. The global `c` (and the retained id) keep it
        // alive, so this exercises the validity guard, not a dangling object.
        e.exec_script("var c = new Counter(); invalidate c;", "test")
            .unwrap();
        let v = e.eval("c", "test").unwrap();
        let id = e.retain_value_detached(&v).unwrap();
        let err = e
            .set_member(id.raw_id(), "value", &TjsValue::Integer(3))
            .unwrap_err();
        assert!(!err.is_empty(), "invalidated object must error: {err}");
    }

    // --- closure receiver (ObjThis) round-trip --------------------------

    /// `CallbackNatives.callWithSelf(fn)` — retains the function/closure
    /// argument (preserving its ObjThis) and invokes it immediately. If the
    /// receiver were lost, a method reference would run with the wrong `this`
    /// (the function object), so `this.n` would be missing.
    extern "C" fn native_call_with_self(
        engine: *mut c_void,
        argc: c_int,
        argv: *const Value,
        out: *mut Value,
        out_error: *mut *mut c_char,
    ) -> c_int {
        if argc < 1 {
            return 1;
        }
        // SAFETY: argv is valid for argc entries during the call.
        let arg = unsafe { &*argv };
        if arg.ty != VAL_OBJECT {
            return 1;
        }
        // Retain through the per-argument handle; the C++ side reconstructs
        // the closure with its ObjThis (see Value::object_objthis).
        let dv = match unsafe { retain_object_arg_raw(engine as *mut Engine, arg) } {
            Ok(dv) => dv,
            Err(_) => return 1,
        };
        let mut inner = Value {
            ty: VAL_VOID,
            integer: 0,
            real: 0.0,
            string: ptr::null(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        let mut err: *mut c_char = ptr::null_mut();
        // SAFETY: engine is live; dv holds the retention (tjs2_call_value
        // does not consume it; it is released when dv drops).
        let rc = unsafe {
            tjs2_call_value(
                engine as *mut Engine,
                dv.raw_id(),
                0,
                ptr::null(),
                &mut inner,
                &mut err,
            )
        };
        if rc != 0 {
            // SAFETY: err is malloc'd by the C++ side (or null).
            let msg = take_error_string(err);
            // SAFETY: out_error is a valid slot.
            unsafe { *out_error = alloc_error_string(&msg) };
            return 1;
        }
        // SAFETY: out is a valid return slot; inner holds a copied result.
        unsafe { *out = inner };
        0
    }

    fn callback_builder() -> NativeClassBuilder<'static> {
        NativeClassBuilder {
            name: "CallbackNatives",
            methods: vec![NativeMethodDef {
                name: "callWithSelf",
                f: native_call_with_self,
            }],
            properties: vec![],
        }
    }

    #[test]
    fn method_reference_argument_preserves_objthis() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class(&callback_builder()).unwrap();
        e.exec_script(
            "class Holder { \
                 var n; \
                 function Holder(v) { n = v; } \
                 function bump() { return n + 1; } \
             } \
             var h = new Holder(41);",
            "test",
        )
        .unwrap();
        // A method reference carries ObjThis = h. The native retains it and
        // calls it: `n` resolves on h, not on the function object.
        e.exec_script("var r = CallbackNatives.callWithSelf(h.bump);", "test")
            .unwrap();
        assert_eq!(e.eval("r", "test").unwrap(), TjsValue::Integer(42));
        // A plain global function (no receiver) still works.
        e.exec_script(
            "function plain() { return 7; } \
             var p = CallbackNatives.callWithSelf(plain);",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("p", "test").unwrap(), TjsValue::Integer(7));
        // The eval -> retain -> call path preserves the receiver too (via
        // the engine's last-object slot).
        let v = e.eval("h.bump", "test").unwrap();
        assert_eq!(v, TjsValue::Object);
        let id = e.retain_value_detached(&v).unwrap();
        assert_eq!(e.call_detached(&id, &[]).unwrap(), TjsValue::Integer(42));
    }

    // --- parent/child instance teardown --------------------------------

    static PARENT_TORN_DOWN: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    static CHILD_FINALIZE_SET: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    extern "C" fn parent_like_create(_engine: *mut c_void) -> *mut c_void {
        Box::into_raw(Box::new(0u8)) as *mut c_void
    }
    extern "C" fn parent_like_destroy(_engine: *mut c_void, instance: *mut c_void) {
        // Simulates a native destroy that tears down shared backing state
        // (the reference `tTJSNI_BaseWindow`/`BaseLayer` cleanup).
        PARENT_TORN_DOWN.store(true, std::sync::atomic::Ordering::SeqCst);
        // SAFETY: instance came from parent_like_create.
        unsafe { drop(Box::from_raw(instance as *mut u8)) };
    }
    extern "C" fn child_like_create(_engine: *mut c_void) -> *mut c_void {
        Box::into_raw(Box::new(0u8)) as *mut c_void
    }
    extern "C" fn child_like_destroy(_engine: *mut c_void, instance: *mut c_void) {
        // SAFETY: instance came from child_like_create.
        unsafe { drop(Box::from_raw(instance as *mut u8)) };
    }
    extern "C" fn child_like_ctor(
        _engine: *mut c_void,
        _instance: *mut c_void,
        _argc: c_int,
        _argv: *const Value,
        out: *mut Value,
        _out_error: *mut *mut c_char,
        _objthis: *mut c_void,
    ) -> c_int {
        // SAFETY: out is a valid return slot.
        unsafe { (*out).ty = VAL_VOID };
        0
    }
    extern "C" fn child_probe_set(
        _engine: *mut c_void,
        _instance: *mut c_void,
        _value: *const Value,
        _out_error: *mut *mut c_char,
        _objthis: *mut c_void,
    ) -> c_int {
        if PARENT_TORN_DOWN.load(std::sync::atomic::Ordering::SeqCst) {
            // Same shape as the Layer setter when its scene entry is gone.
            return 1;
        }
        CHILD_FINALIZE_SET.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        0
    }
    fn parent_like_builder() -> NativeInstanceBuilder<'static> {
        NativeInstanceBuilder {
            name: "ParentLike",
            create: parent_like_create,
            destroy: parent_like_destroy,
            invalidate: None,
            methods: vec![],
            properties: vec![],
        }
    }
    fn child_like_builder() -> NativeInstanceBuilder<'static> {
        NativeInstanceBuilder {
            name: "ChildLike",
            create: child_like_create,
            destroy: child_like_destroy,
            invalidate: None,
            methods: vec![NativeInstanceMethodDef {
                name: "ChildLike",
                f: child_like_ctor,
            }],
            properties: vec![NativeInstancePropertyDef {
                name: "childProbe",
                get: None,
                set: Some(child_probe_set),
            }],
        }
    }

    #[test]
    fn script_finalize_can_set_native_property_before_invalidate() {
        let _vm_lock = vm_lock();
        COUNTER_VALUE_SET.store(0, std::sync::atomic::Ordering::SeqCst);
        COUNTER_DESTROYED.store(0, std::sync::atomic::Ordering::SeqCst);
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        // Reference `tTJSCustomObject::Finalize()` (tjsObject.cpp:389-405)
        // runs the script `finalize` BEFORE `ClassInstances[i]->Invalidate()`,
        // and the reference `Invalidate` does not destroy the native instance
        // (tjsNative.h:35-49). A finalize-time native property write must
        // therefore reach a live payload.
        e.exec_script(
            "class FinalizeCounter extends Counter { \
                 function FinalizeCounter() { super.Counter(); } \
                 function finalize() { value = 123; super.finalize(); } \
             } \
             var fc = new FinalizeCounter(); \
             var probe = fc.value; \
             invalidate fc;",
            "test",
        )
        .unwrap();
        assert_eq!(
            e.eval("probe", "test").unwrap(),
            TjsValue::Integer(0),
            "the native instance exists before finalize"
        );
        assert_eq!(
            COUNTER_VALUE_SET.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the script finalize must set the native property"
        );
        assert_eq!(
            COUNTER_DESTROYED.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "Invalidate must not free the payload before other finalizers run"
        );
    }

    #[test]
    fn invalidating_a_parent_instance_does_not_tear_down_children_early() {
        // Mirrors the game's layer-tree teardown: a parent native instance's
        // destroy callback removes backing state, and a child's script
        // `finalize` sets a native property. With the reference lifecycle the
        // parent's payload is released at destruction, not at Invalidate, so
        // the child's finalize still sees live state.
        let _vm_lock = vm_lock();
        PARENT_TORN_DOWN.store(false, std::sync::atomic::Ordering::SeqCst);
        CHILD_FINALIZE_SET.store(0, std::sync::atomic::Ordering::SeqCst);
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&parent_like_builder())
            .unwrap();
        e.register_native_class_instance(&child_like_builder())
            .unwrap();
        e.exec_script(
            "class ChildSub extends ChildLike { \
                 function ChildSub() { super.ChildLike(); } \
                 function finalize() { childProbe = 1; super.finalize(); } \
             } \
             var p = new ParentLike(); \
             var c = new ChildSub(); \
             invalidate p; \
             invalidate c;",
            "test",
        )
        .unwrap();
        assert_eq!(
            CHILD_FINALIZE_SET.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the child finalize must set its native property"
        );
        assert!(
            !PARENT_TORN_DOWN.load(std::sync::atomic::Ordering::SeqCst),
            "the parent must not have torn down child state before the child finalize"
        );
    }

    // --- tjs2_compile_script / tjs2_dump --------------------------------

    #[test]
    fn compile_script_writes_binary_bytecode_file() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "krkr-rs-tjs2-compile-{}-{}.tjsb",
            std::process::id(),
            nanos
        ));
        let path_str = path.to_string_lossy().into_owned();

        // A statement script compiles and the binary output is written.
        e.compile_script(
            "var compiledValue = 42;",
            &path_str,
            CompileOptions {
                isresult: false,
                outputdebug: false,
                isexpression: false,
                name: "compiled.tjs",
                lineofs: 0,
            },
        )
        .expect("compile must succeed");
        let bytes = std::fs::read(&path).expect("compiled output file must exist");
        assert!(!bytes.is_empty(), "compiled bytecode must not be empty");

        // An expression compiles too (isexpression = true).
        let expr_path = std::env::temp_dir().join(format!(
            "krkr-rs-tjs2-compile-expr-{}-{}.tjsb",
            std::process::id(),
            nanos
        ));
        let expr_str = expr_path.to_string_lossy().into_owned();
        e.compile_script(
            "6 * 7",
            &expr_str,
            CompileOptions {
                isresult: true,
                outputdebug: false,
                isexpression: true,
                name: "expr.tjs",
                lineofs: 0,
            },
        )
        .expect("expression compile must succeed");
        assert!(!std::fs::read(&expr_path).unwrap().is_empty());

        // A syntactically invalid script is reported as an error.
        let err = e
            .compile_script(
                "this is not valid tjs",
                &path_str,
                CompileOptions {
                    isresult: false,
                    outputdebug: false,
                    isexpression: false,
                    name: "bad.tjs",
                    lineofs: 0,
                },
            )
            .unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "compile error must be reported"
        );

        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&expr_path);
    }

    #[test]
    fn dump_reports_the_context_through_the_log_callback() {
        static DUMP_SEEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        extern "C" fn capture(_level: c_int, msg: *const c_char, _user: *mut c_void) {
            if msg.is_null() {
                return;
            }
            // SAFETY: msg is a NUL-terminated UTF-8 string for the call.
            let s = unsafe { CStr::from_ptr(msg) }.to_string_lossy();
            if s.contains("TJS Context Dump") {
                DUMP_SEEN.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }

        let _vm_lock = vm_lock();
        DUMP_SEEN.store(false, std::sync::atomic::Ordering::SeqCst);
        let e = Tjs2Engine::new().unwrap();
        e.exec_script("var dumpedValue = 1;", "dump-test").unwrap();
        // SAFETY: the callback is a plain fn with no user pointer needs.
        unsafe { e.set_log_cb(Some(capture), ptr::null_mut()) };
        e.dump().expect("dump must succeed");
        assert!(
            DUMP_SEEN.load(std::sync::atomic::Ordering::SeqCst),
            "dump output must reach the log callback"
        );
    }

    // --- tjs2_get_class_names / tjs2_set_call_missing -------------------

    #[test]
    fn get_class_names_returns_the_object_class_chain() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        e.exec_script("var c = new Counter();", "test").unwrap();
        let v = e.eval("c", "test").unwrap();
        assert_eq!(v, TjsValue::Object);
        let obj = e.retain_value_detached(&v).unwrap();

        // A native instance registers its class name at construction
        // (tTJSNativeClass::FuncCall -> ClassInstanceInfo(TJS_CII_ADD)).
        let names = e
            .class_names(obj.raw_id())
            .expect("getClassNames must succeed");

        // Expose the returned array to the script for inspection: the
        // DetachedValue is consumed by the write (C++ erases the entry).
        e.exec_script("var holder = %[];", "test").unwrap();
        let hv = e.eval("holder", "test").unwrap();
        let holder = e.retain_value_detached(&hv).unwrap();
        e.set_member(
            holder.raw_id(),
            "names",
            &TjsValue::Retained(names.raw_id().as_ptr() as u64),
        )
        .unwrap();
        assert_eq!(
            e.eval("holder.names.length", "test").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval("holder.names[0]", "test").unwrap(),
            TjsValue::String("Counter".into())
        );
    }

    #[test]
    fn get_class_names_rejects_a_non_object_retained_value() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        let v = e.eval("41", "test").unwrap();
        let id = e.retain_value_detached(&v).unwrap();
        let err = match e.class_names(id.raw_id()) {
            Ok(_) => panic!("a scalar must not have class names"),
            Err(e) => e,
        };
        assert!(!err.is_empty(), "a scalar must not have class names: {err}");
    }

    #[test]
    fn set_call_missing_installs_a_missing_member_handler() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        e.exec_script(
            "var probe = '';\
             var o = %[missing: function(getorset, name, value) { probe = name; return true; }];\
             o.known = 5;",
            "test",
        )
        .unwrap();
        let v = e.eval("o", "test").unwrap();
        let id = e.retain_value_detached(&v).unwrap();

        // Before enabling it, the `missing` handler does not run (a missing
        // read yields void rather than a hard error in TJS).
        let _ = e.eval("o.absentMember", "test");
        assert_eq!(
            e.eval("probe", "test").unwrap(),
            TjsValue::String(String::new())
        );

        e.set_call_missing(id.raw_id())
            .expect("setCallMissing must succeed");

        // After enabling it, the `missing` method receives the member name;
        // it returns true (found) with no value, so the read is void.
        assert_eq!(e.eval("o.absentMember", "test").unwrap(), TjsValue::Void);
        assert_eq!(
            e.eval("probe", "test").unwrap(),
            TjsValue::String("absentMember".into())
        );
        // Known members still resolve normally (the handler is only a
        // fallback; it never runs because `known` exists).
        assert_eq!(e.eval("o.known", "test").unwrap(), TjsValue::Integer(5));
    }

    // --- static members on an instance class ----------------------------

    static STATIC_PROP_VALUE: std::sync::atomic::AtomicI64 = std::sync::atomic::AtomicI64::new(0);

    extern "C" fn static_prop_get(
        _engine: *mut c_void,
        out: *mut Value,
        _out_error: *mut *mut c_char,
    ) -> c_int {
        // SAFETY: out is a valid return slot.
        unsafe {
            (*out).ty = VAL_INTEGER;
            (*out).integer = STATIC_PROP_VALUE.load(std::sync::atomic::Ordering::SeqCst);
            (*out).real = 0.0;
            (*out).string = ptr::null();
        }
        0
    }

    extern "C" fn static_prop_set(
        _engine: *mut c_void,
        value: *const Value,
        out_error: *mut *mut c_char,
    ) -> c_int {
        // SAFETY: value points at a valid tjs2_value for the call.
        let v = unsafe { &*value };
        if v.ty != VAL_INTEGER {
            unsafe { *out_error = alloc_error_string("staticValue expects an integer") };
            return 1;
        }
        STATIC_PROP_VALUE.store(v.integer, std::sync::atomic::Ordering::SeqCst);
        0
    }

    #[test]
    fn static_members_are_visible_on_the_class_but_not_on_instances() {
        let _vm_lock = vm_lock();
        STATIC_PROP_VALUE.store(7, std::sync::atomic::Ordering::SeqCst);
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        // TJS_STATICMEMBER members: a method (reference
        // TJS_END_NATIVE_STATIC_METHOD_DECL) and a read-only property
        // (reference TJS_END_NATIVE_STATIC_PROP_DECL_OUTER). The property
        // is read-only like `MenuItem.textToKeycode`: a script *write* to a
        // class member lets TJS clear its static flag (PropSet without
        // TJS_STATICMEMBER clears TJS_SYMBOL_STATIC), so a writable static
        // property would then be copied to later instances — exactly the
        // reference's own behavior.
        e.register_native_static_members(&NativeStaticMembers {
            class_name: "Counter",
            methods: vec![NativeMethodDef {
                name: "staticAdd",
                f: native_add,
            }],
            properties: vec![NativePropertyDef {
                name: "staticValue",
                get: Some(static_prop_get),
                set: None,
            }],
        })
        .unwrap();

        // Reachable on the class object with no instance.
        assert_eq!(
            e.eval("Counter.staticAdd(2, 3)", "test").unwrap(),
            TjsValue::Integer(5)
        );
        assert_eq!(
            e.eval("Counter.staticValue", "test").unwrap(),
            TjsValue::Integer(7)
        );

        // Instance members are untouched by the static registration.
        e.exec_script("var c = new Counter(); c.inc(); c.add(41);", "test")
            .unwrap();
        assert_eq!(e.eval("c.get()", "test").unwrap(), TjsValue::Integer(42));
        assert_eq!(e.eval("c.value", "test").unwrap(), TjsValue::Integer(42));

        // ... and the static members are not copied onto instances.
        let err = e.eval("c.staticAdd(1, 2)", "test").unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "instance must not see staticAdd"
        );
        let err = e.eval("c.staticValue", "test").unwrap_err();
        assert!(
            !err.to_string().is_empty(),
            "instance must not see staticValue"
        );
        // The class-level property still reads normally afterwards.
        assert_eq!(
            e.eval("Counter.staticValue", "test").unwrap(),
            TjsValue::Integer(7)
        );
    }

    #[test]
    fn static_property_setter_runs_on_the_class_object() {
        let _vm_lock = vm_lock();
        STATIC_PROP_VALUE.store(0, std::sync::atomic::Ordering::SeqCst);
        let e = Tjs2Engine::new().unwrap();
        e.register_native_class_instance(&counter_builder())
            .unwrap();
        e.register_native_static_members(&NativeStaticMembers {
            class_name: "Counter",
            methods: vec![],
            properties: vec![NativePropertyDef {
                name: "writableStatic",
                get: Some(static_prop_get),
                set: Some(static_prop_set),
            }],
        })
        .unwrap();
        assert_eq!(
            e.eval("Counter.writableStatic", "test").unwrap(),
            TjsValue::Integer(0)
        );
        e.eval("Counter.writableStatic = 42", "test").unwrap();
        assert_eq!(
            e.eval("Counter.writableStatic", "test").unwrap(),
            TjsValue::Integer(42)
        );
        assert_eq!(
            STATIC_PROP_VALUE.load(std::sync::atomic::Ordering::SeqCst),
            42
        );
    }

    #[test]
    fn static_members_reject_an_unknown_class() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        let err = e
            .register_native_static_members(&NativeStaticMembers {
                class_name: "NoSuchClass",
                methods: vec![NativeMethodDef {
                    name: "m",
                    f: native_nop,
                }],
                properties: vec![],
            })
            .unwrap_err();
        assert!(err.contains("NoSuchClass"), "unexpected error: {err}");
    }

    // -------------------------------------------------------------------
    // save/load streams (cpp/streams.cpp)
    // -------------------------------------------------------------------

    /// Expected text of the embedded crypt-mode fixtures. Single line, so
    /// `Array.load` yields exactly one element.
    const FIXTURE_TEXT: &str =
        "(const) %[ \"title\" => \"フィクスチャ\", \"scenario\" => \"01_01\", \"value\" => 42 ]";

    const FIXTURE_MODE0: &[u8] = &[
        0xfe, 0xfe, 0x00, 0xff, 0xfe, 0x29, 0x28, 0x62, 0x62, 0x6e, 0x6e, 0x6f, 0x6e, 0x72, 0x72,
        0x75, 0x74, 0x28, 0x28, 0x21, 0x20, 0x24, 0x24, 0x5a, 0x5a, 0x21, 0x20, 0x23, 0x22, 0x75,
        0x74, 0x68, 0x68, 0x75, 0x74, 0x6d, 0x6c, 0x64, 0x64, 0x23, 0x22, 0x21, 0x20, 0x3c, 0x3c,
        0x3f, 0x3e, 0x21, 0x20, 0x23, 0x22, 0xd4, 0xe4, 0xa2, 0x92, 0xae, 0x9e, 0xb8, 0x88, 0xc0,
        0xf0, 0xe2, 0xd2, 0x23, 0x22, 0x2d, 0x2c, 0x21, 0x20, 0x23, 0x22, 0x72, 0x72, 0x62, 0x62,
        0x64, 0x64, 0x6f, 0x6e, 0x60, 0x60, 0x73, 0x72, 0x68, 0x68, 0x6e, 0x6e, 0x23, 0x22, 0x21,
        0x20, 0x3c, 0x3c, 0x3f, 0x3e, 0x21, 0x20, 0x23, 0x22, 0x31, 0x30, 0x30, 0x30, 0x5e, 0x5e,
        0x31, 0x30, 0x30, 0x30, 0x23, 0x22, 0x2d, 0x2c, 0x21, 0x20, 0x23, 0x22, 0x77, 0x76, 0x60,
        0x60, 0x6d, 0x6c, 0x74, 0x74, 0x64, 0x64, 0x23, 0x22, 0x21, 0x20, 0x3c, 0x3c, 0x3f, 0x3e,
        0x21, 0x20, 0x35, 0x34, 0x33, 0x32, 0x21, 0x20, 0x5c, 0x5c,
    ];

    const FIXTURE_MODE1: &[u8] = &[
        0xfe, 0xfe, 0x01, 0xff, 0xfe, 0x14, 0x00, 0x93, 0x00, 0x9f, 0x00, 0x9d, 0x00, 0xb3, 0x00,
        0xb8, 0x00, 0x16, 0x00, 0x10, 0x00, 0x1a, 0x00, 0xa7, 0x00, 0x10, 0x00, 0x11, 0x00, 0xb8,
        0x00, 0x96, 0x00, 0xb8, 0x00, 0x9c, 0x00, 0x9a, 0x00, 0x11, 0x00, 0x10, 0x00, 0x3e, 0x00,
        0x3d, 0x00, 0x10, 0x00, 0x11, 0x00, 0xea, 0x30, 0x53, 0x30, 0x5f, 0x30, 0x76, 0x30, 0xc2,
        0x30, 0xd3, 0x30, 0x11, 0x00, 0x1c, 0x00, 0x10, 0x00, 0x11, 0x00, 0xb3, 0x00, 0x93, 0x00,
        0x9a, 0x00, 0x9d, 0x00, 0x92, 0x00, 0xb1, 0x00, 0x96, 0x00, 0x9f, 0x00, 0x11, 0x00, 0x10,
        0x00, 0x3e, 0x00, 0x3d, 0x00, 0x10, 0x00, 0x11, 0x00, 0x30, 0x00, 0x32, 0x00, 0xaf, 0x00,
        0x30, 0x00, 0x32, 0x00, 0x11, 0x00, 0x1c, 0x00, 0x10, 0x00, 0x11, 0x00, 0xb9, 0x00, 0x92,
        0x00, 0x9c, 0x00, 0xba, 0x00, 0x9a, 0x00, 0x11, 0x00, 0x10, 0x00, 0x3e, 0x00, 0x3d, 0x00,
        0x10, 0x00, 0x38, 0x00, 0x31, 0x00, 0x10, 0x00, 0xae, 0x00,
    ];

    const FIXTURE_MODE2: &[u8] = &[
        0xfe, 0xfe, 0x02, 0xff, 0xfe, 0x65, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x8c, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x78, 0xda, 0xd3, 0x60, 0x48, 0x66, 0xc8, 0x67, 0xc8,
        0x63, 0x28, 0x66, 0x28, 0x61, 0xd0, 0x64, 0x50, 0x60, 0x50, 0x65, 0x88, 0x06, 0x92, 0x4a,
        0x40, 0x5e, 0x26, 0x10, 0xe7, 0x30, 0xa4, 0x02, 0xd9, 0x0a, 0x0c, 0xb6, 0x0c, 0x76, 0x60,
        0xd1, 0xab, 0x06, 0x8b, 0x0d, 0xd6, 0x1b, 0xec, 0x34, 0x38, 0x68, 0xf0, 0xd8, 0x40, 0x89,
        0x41, 0x07, 0x2c, 0x56, 0x0c, 0x34, 0x21, 0x15, 0x68, 0x42, 0x22, 0x43, 0x11, 0x50, 0x4f,
        0x3e, 0x8a, 0x7a, 0x03, 0x06, 0x43, 0x86, 0x78, 0x30, 0x09, 0x53, 0x5d, 0x06, 0x54, 0x97,
        0xc3, 0x50, 0x8a, 0x62, 0xae, 0x09, 0x83, 0x11, 0x90, 0x8c, 0x65, 0x00, 0x00, 0xc3, 0xea,
        0x16, 0x91,
    ];

    /// Decode a whole text stream through the C++ reader (honoring the
    /// mode's `oN` offset and the `FE FE` crypt container) and return the
    /// UTF-8 text.
    fn read_text_stream(path: &std::path::Path, mode: &str) -> String {
        let path = CString::new(path.to_string_lossy().as_bytes()).unwrap();
        let mode = CString::new(mode).unwrap();
        let mut out: *mut c_char = ptr::null_mut();
        // SAFETY: both C strings are valid for the call; the C++ helper
        // writes a malloc'd NUL-terminated UTF-8 string into `out`.
        let rc = unsafe { tjs2_read_text_stream_all(path.as_ptr(), mode.as_ptr(), &mut out) };
        assert_eq!(rc, 0, "tjs2_read_text_stream_all failed for mode {mode:?}");
        assert!(!out.is_null(), "helper returned a null string");
        // SAFETY: out is a NUL-terminated malloc'd string owned by us.
        let text = unsafe { CStr::from_ptr(out) }
            .to_string_lossy()
            .into_owned();
        // SAFETY: the helper allocated it with malloc; free it exactly once.
        unsafe { tjs2_free_string(out) };
        text
    }

    /// A unique temp path for a test file (no `tempfile` dependency).
    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("tjs-{}-{nanos}-{name}", std::process::id()))
    }

    #[test]
    fn reads_a_fixture_in_each_crypt_mode() {
        let _vm_lock = vm_lock();
        for (name, fixture) in [
            ("crypt-mode0", FIXTURE_MODE0),
            ("crypt-mode1", FIXTURE_MODE1),
            ("crypt-mode2", FIXTURE_MODE2),
        ] {
            let path = temp_path(name);
            std::fs::write(&path, fixture).unwrap();
            let text = read_text_stream(&path, "");
            assert_eq!(text, FIXTURE_TEXT, "{name} decoded incorrectly");
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn honors_the_offset_of_an_offset_prefixed_fixture() {
        let _vm_lock = vm_lock();
        let prefix = b"KRKR-SAVE-HEADER"; // 16 bytes
        let mut data = prefix.to_vec();
        data.extend_from_slice(FIXTURE_MODE2);
        let path = temp_path("offset");
        std::fs::write(&path, &data).unwrap();
        let text = read_text_stream(&path, &format!("o{}", prefix.len()));
        assert_eq!(text, FIXTURE_TEXT);
        let _ = std::fs::remove_file(&path);
    }

    // reference/cpp/core/base/TextStream.cpp:82-88 — a mode 0/1 container whose
    // bytes after the mode are not the mandatory UTF-16LE BOM `FF FE` is
    // rejected, not read as an offset-4 payload.
    #[test]
    fn rejects_a_cipher_container_without_the_bom() {
        let _vm_lock = vm_lock();
        let path = temp_path("no-bom");
        std::fs::write(&path, [0xFEu8, 0xFE, 0x00, 0x00, 0x40, 0x40]).unwrap();
        let path_c = CString::new(path.to_string_lossy().as_bytes()).unwrap();
        let mode = CString::new("").unwrap();
        let mut out: *mut c_char = ptr::null_mut();
        // SAFETY: both C strings are valid for the call.
        let rc = unsafe { tjs2_read_text_stream_all(path_c.as_ptr(), mode.as_ptr(), &mut out) };
        assert_ne!(rc, 0, "a missing BOM must fail the read");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn array_save_load_roundtrips_each_crypt_mode() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        for (name, mode) in [
            ("default-mode2", "o8"),
            ("mode0", "o8c0"),
            ("mode1", "o8c1"),
            ("mode2-level", "o8z9"),
        ] {
            let path = temp_path(name);
            let prefix = b"PREFIX!!";
            std::fs::write(&path, prefix).unwrap();
            let path_lit = format!("{:?}", path.to_string_lossy());
            let setup = format!(
                "var a = ['alpha', 'beta', 'gamma'];\n\
                 a.save({path_lit}, '{mode}');\n\
                 var b = [];\n\
                 b.load({path_lit}, '{mode}');"
            );
            e.exec_script(&setup, name).unwrap();
            let v = e.eval("b[0] + '|' + b[1] + '|' + b[2]", name).unwrap();
            assert_eq!(
                v,
                TjsValue::String("alpha|beta|gamma".into()),
                "mode {mode}"
            );
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(
                &bytes[..prefix.len()],
                prefix,
                "mode {mode} clobbered the prefix"
            );
            let _ = std::fs::remove_file(&path);
        }
    }

    // reference/cpp/core/base/TextStream.cpp:82-105 — the cipher modes write a
    // five-byte header (`FE FE <mode> FF FE`); mode 2 keeps the same
    // five-byte `FE FE 02 FF FE` header (`:104-116`).
    #[test]
    fn writer_emits_the_reference_crypt_header() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        for (name, mode, expected) in [
            (
                "header-mode0",
                "o8c0",
                &[0xFEu8, 0xFE, 0x00, 0xFF, 0xFE][..],
            ),
            (
                "header-mode1",
                "o8c1",
                &[0xFEu8, 0xFE, 0x01, 0xFF, 0xFE][..],
            ),
            (
                "header-mode2",
                "o8c2",
                &[0xFEu8, 0xFE, 0x02, 0xFF, 0xFE][..],
            ),
        ] {
            let path = temp_path(name);
            let prefix = b"PREFIX!!";
            std::fs::write(&path, prefix).unwrap();
            let path_lit = format!("{:?}", path.to_string_lossy());
            let setup = format!("var a = ['x']; a.save({path_lit}, '{mode}');");
            e.exec_script(&setup, name).unwrap();
            let bytes = std::fs::read(&path).unwrap();
            assert_eq!(
                &bytes[prefix.len()..prefix.len() + expected.len()],
                expected,
                "mode {mode}"
            );
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn struct_roundtrips_through_an_offset_prefixed_file() {
        let _vm_lock = vm_lock();
        let e = Tjs2Engine::new().unwrap();
        let path = temp_path("struct");
        let prefix = b"PREFIX!!";
        std::fs::write(&path, prefix).unwrap();
        let path_lit = format!("{:?}", path.to_string_lossy());
        // `Array.saveStruct` is an instance method (the Dictionary variant is
        // registered as a static member in the vendored core and is not
        // reachable from an instance); both write the same text struct
        // through cpp/streams.cpp.
        let script = format!(
            "var a = ['Round Trip', '05_12', 42];\n\
             a.saveStruct({path_lit}, 'o8');"
        );
        e.exec_script(&script, "test").unwrap();

        // The BMP-like prefix must survive the write.
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..prefix.len()], prefix, "the prefix was clobbered");

        // Read the struct back through the C++ reader and reconstruct it.
        let text = read_text_stream(&path, "o8");
        let retained = e.eval_retained(&text, "struct").unwrap();
        let retained = match retained {
            RetainedValue::Object(o) => o,
            RetainedValue::Value(v) => panic!("struct eval returned a scalar: {v:?}"),
        };
        assert_eq!(
            e.member(retained.raw_id(), "0").unwrap(),
            TjsValue::String("Round Trip".into())
        );
        assert_eq!(
            e.member(retained.raw_id(), "1").unwrap(),
            TjsValue::String("05_12".into())
        );
        assert_eq!(
            e.member(retained.raw_id(), "2").unwrap(),
            TjsValue::Integer(42)
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reads_the_real_kag_save_fixture() {
        let _vm_lock = vm_lock();
        // Set `KRKR_TEST_SAVE` to a real KAG save BMP (a thumbnail followed by
        // the struct) to run this against shipped data.
        let Ok(fixture) = std::env::var("KRKR_TEST_SAVE") else {
            eprintln!("skipping: set KRKR_TEST_SAVE to a real KAG save BMP");
            return;
        };
        let path = std::path::Path::new(&fixture);
        if !path.exists() {
            eprintln!("skipping: {fixture} does not exist");
            return;
        }
        // The BMP thumbnail is 83086 bytes; the save struct follows it.
        let text = read_text_stream(path, "o83086");
        assert!(text.contains("(const)"), "fixture is not a TJS struct");
        assert!(
            text.contains("\"title\""),
            "fixture is missing the title field"
        );
        assert!(
            text.contains("\"scenario\""),
            "fixture is missing the scenario field"
        );
    }

    // --- stream factory modes and dispositions ---------------------------

    /// A data directory for the wired tjs stream factories. `set_data_dir`
    /// points their relative-name resolution at it.
    fn stream_temp_dir(name: &str) -> std::path::PathBuf {
        let dir = temp_path(name);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn stream_test_access(mode: &str) -> String {
        let mode = CString::new(mode).unwrap();
        // SAFETY: the helper returns a static NUL-terminated string.
        let pointer = unsafe { tjs2_test_binary_write_access(mode.as_ptr()) };
        // SAFETY: the pointer is a static NUL-terminated string.
        unsafe { CStr::from_ptr(pointer) }
            .to_string_lossy()
            .into_owned()
    }

    fn open_test_binary(name: &str, mode: &str, for_write: bool) -> (bool, String) {
        let name = CString::new(name).unwrap();
        let mode = CString::new(mode).unwrap();
        let mut opened: c_int = 0;
        let mut access: *const c_char = ptr::null();
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: all pointers are valid for the call; the helper writes a
        // static access string and, on failure, a malloc'd message.
        let rc = unsafe {
            tjs2_test_open_binary_stream(
                name.as_ptr(),
                mode.as_ptr(),
                for_write as c_int,
                &mut opened,
                &mut access,
                &mut error,
            )
        };
        assert_eq!(rc, 0, "helper must not fail for {name:?}");
        if !error.is_null() {
            // SAFETY: the helper allocated it with malloc.
            unsafe { tjs2_free_string(error) };
        }
        let access = if access.is_null() {
            String::new()
        } else {
            // SAFETY: the helper returns a static NUL-terminated string.
            unsafe { CStr::from_ptr(access) }
                .to_string_lossy()
                .into_owned()
        };
        (opened != 0, access)
    }

    /// `BinaryStream.cpp:57-76`: `oN` (N > 0) is `TJS_BS_UPDATE`, everything
    /// else is `TJS_BS_WRITE`. The port additionally maps `a` to
    /// `TJS_BS_APPEND`; the reference binary factory has no `a` branch, so that
    /// is a port extension (reported, not silently fixed).
    #[test]
    fn binary_write_mode_selects_the_reference_access() {
        let _vm_lock = vm_lock();
        for (mode, expected) in [
            ("", "TJS_BS_WRITE"),
            ("wb", "TJS_BS_WRITE"),
            ("o0", "TJS_BS_WRITE"),
            ("o4", "TJS_BS_UPDATE"),
            ("o255", "TJS_BS_UPDATE"),
            ("a", "TJS_BS_APPEND"),
            ("ab", "TJS_BS_APPEND"),
        ] {
            assert_eq!(stream_test_access(mode), expected, "mode {mode:?}");
        }
    }

    /// `_TVPCreateStream` (`StorageIntf.cpp:1137-1141`) normalizes the name
    /// for `TJS_BS_WRITE` without requiring the file to exist, so a write to a
    /// new storage name creates it. This is the behaviour the `Config.tjs`
    /// hypothesis depends on.
    #[test]
    fn write_mode_creates_a_missing_storage_name() {
        let _vm_lock = vm_lock();
        let dir = stream_temp_dir("stream-write-create");
        let e = Tjs2Engine::new().unwrap();
        e.set_data_dir(dir.to_str().unwrap()).unwrap();
        assert!(!dir.join("created.tjs").exists());
        let (opened, access) = open_test_binary("created.tjs", "", true);
        assert!(opened, "a write must open a new file");
        assert_eq!(access, "TJS_BS_WRITE");
        assert!(
            dir.join("created.tjs").is_file(),
            "write mode must create the host file"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_mode_truncates_an_existing_file() {
        let _vm_lock = vm_lock();
        let dir = stream_temp_dir("stream-write-truncate");
        let e = Tjs2Engine::new().unwrap();
        e.set_data_dir(dir.to_str().unwrap()).unwrap();
        std::fs::write(dir.join("existing.tjs"), b"AAAA").unwrap();
        let (opened, _) = open_test_binary("existing.tjs", "", true);
        assert!(opened);
        assert_eq!(
            std::fs::read(dir.join("existing.tjs")).unwrap(),
            b"",
            "TJS_BS_WRITE must truncate"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `oN` opens at an offset and must not truncate (`BinaryStream.cpp:65-76`).
    /// The reference takes the placed path for UPDATE, so a missing file would
    /// throw `TVPCannotOpenStorage` (`StorageIntf.cpp:1137-1144`); the port's
    /// update fallback creates it instead -- a divergence recorded here.
    #[test]
    fn update_mode_preserves_the_file_and_creates_when_missing() {
        let _vm_lock = vm_lock();
        let dir = stream_temp_dir("stream-update");
        let e = Tjs2Engine::new().unwrap();
        e.set_data_dir(dir.to_str().unwrap()).unwrap();
        std::fs::write(dir.join("update.tjs"), b"AAAAAAAA").unwrap();
        let (opened, access) = open_test_binary("update.tjs", "o2", true);
        assert!(opened);
        assert_eq!(access, "TJS_BS_UPDATE");
        assert_eq!(
            std::fs::read(dir.join("update.tjs")).unwrap(),
            b"AAAAAAAA",
            "TJS_BS_UPDATE must not truncate"
        );

        let (opened, _) = open_test_binary("missing.tjs", "o2", true);
        assert!(opened, "the port's update fallback creates a missing file");
        assert!(dir.join("missing.tjs").is_file());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Read takes the placed path and requires it to exist
    /// (`StorageIntf.cpp:1136-1144`). A failed open raises, so the helper
    /// reports `opened=false`; the log line names the reason.
    #[test]
    fn read_mode_requires_an_existing_file() {
        let _vm_lock = vm_lock();
        let dir = stream_temp_dir("stream-read");
        let e = Tjs2Engine::new().unwrap();
        e.set_data_dir(dir.to_str().unwrap()).unwrap();
        let (opened, _access) = open_test_binary("nope.tjs", "", false);
        assert!(!opened, "a missing read must not open");

        std::fs::write(dir.join("there.tjs"), b"hi").unwrap();
        let (opened, _) = open_test_binary("there.tjs", "", false);
        assert!(opened, "an existing read must open");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The factory emits one line per stream: the requested name, the
    /// reference access, whether the host file existed, and the host path. A
    /// failed open is a `warn` naming the reason, so a silent write failure
    /// cannot hide.
    #[test]
    fn stream_creation_and_open_failure_are_logged() {
        let _vm_lock = vm_lock();
        let dir = stream_temp_dir("stream-log");
        let e = Tjs2Engine::new().unwrap();
        e.set_data_dir(dir.to_str().unwrap()).unwrap();
        // SAFETY: `record_vm_log` is a plain fn with no user pointer needs.
        unsafe { e.set_log_cb(Some(record_vm_log), ptr::null_mut()) };
        VM_LOG_RECORDS.with(|logs| logs.borrow_mut().clear());

        let (opened, _) = open_test_binary("logged.tjs", "", true);
        assert!(opened);
        let (opened, _) = open_test_binary("absent.tjs", "", false);
        assert!(!opened);

        let logs = VM_LOG_RECORDS.with(|logs| logs.borrow().clone());
        let created = logs
            .iter()
            .find(|(_, msg)| msg.contains("stream open binary-write 'logged.tjs'"))
            .unwrap_or_else(|| panic!("write creation must be logged: {logs:?}"));
        assert_eq!(created.0, LOG_DEBUG, "creation is a debug line");
        assert!(created.1.contains("access=TJS_BS_WRITE"), "{}", created.1);
        assert!(created.1.contains("resolved=true"), "{}", created.1);
        assert!(created.1.contains("existed=false"), "{}", created.1);
        assert!(
            created.1.contains(dir.join("logged.tjs").to_str().unwrap()),
            "the host path must be named: {}",
            created.1
        );

        let failed = logs
            .iter()
            .find(|(_, msg)| msg.contains("stream open binary-read 'absent.tjs'"))
            .unwrap_or_else(|| panic!("a failed open must be logged: {logs:?}"));
        assert_eq!(failed.0, LOG_WARN, "failure is a warn line");
        assert!(failed.1.contains("access=TJS_BS_READ"), "{}", failed.1);
        assert!(failed.1.contains("failed:"), "{}", failed.1);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
