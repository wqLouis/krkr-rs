//! `Scripts` native class (L3): storage-backed script execution helpers.
//!
//! Reimplemented over [`engine::Storage`] and the `tjs` engine handle exposed
//! by [`CompatCtx::engine`]. The reference is the old `tvp-scripts` crate;
//! this port keeps only the parts the new ABI supports and owns no state
//! outside [`CompatServices::state`].
//!
//! Script-visible members:
//!
//! | member | behavior |
//! |---|---|
//! | `execStorage(name[, mode[, context]])` | read `name` through the L2 storage and execute it as a script; an explicit top-level `return` is the result |
//! | `evalStorage(name[, mode[, context]])` | read `name` and evaluate it as an expression |
//! | `exec(script[, name[, lineofs[, context]]])` | execute a raw script string |
//! | `eval(expression[, name[, lineofs[, context]]])` | evaluate a raw expression |
//! | `compileStorage(name, output[, isresult[, outputdebug[, isexpression]]])` | compile a storage script to a bytecode file |
//! | `setCallMissing(obj)` | enable `obj`'s `missing` member handler |
//! | `getClassNames(obj)` | the object's class-name chain as a TJS `Array` |
//! | `getTraceString(limit?)` | the current VM stack trace as a string |
//! | `dump()` | dump every live script block through the engine log |
//! | `textEncoding` | get/set the fallback text decoder (default `"UTF-8"`) |
//!
//! `mode` honors only the reference's `oN` byte offset. Script decoding
//! supports UTF-8 (with/without BOM) and UTF-16 (BOM or the BOM-less LE
//! heuristic); a configured `textEncoding` that is not a UTF variant is
//! rejected with a typed error. The reference's CP932 fallback and the
//! `FE FE` cipher / zlib container are **deferred**: `compat` does not depend
//! on the encoding or flate2 crates, so an undecodable script raises a typed
//! error instead of producing mojibake (D11). The `context` argument is
//! likewise ignored with a warning.

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;

use engine::Storage;
use tjs::{
    Engine, NativeClassBuilder, NativeMethodDef, NativePropertyDef, RetainedValue, Tjs2Engine,
    TjsValue, VAL_INTEGER, VAL_REAL, VAL_RETAINED, VAL_STRING, VAL_VOID, Value,
};

use crate::binding;
use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::{CompatCtx, CompatError, CompatServices};

/// Per-family dispatch state: the `Scripts.textEncoding` override.
#[derive(Default)]
struct ScriptsState {
    /// `None` keeps the automatic decoder and reports `"UTF-8"`.
    text_encoding: Option<String>,
}

/// Scratch buffer for a string result; the C++ trampoline copies it before the
/// next native call.
#[derive(Default)]
struct StringScratch(Vec<u8>);

/// Write a string result into `out` using the shared scratch buffer.
fn set_string_out(out: *mut Value, services: &CompatServices, s: &str) {
    let mut scratch = services.state::<StringScratch>();
    scratch.0.clear();
    scratch.0.extend_from_slice(s.as_bytes());
    scratch.0.push(0);
    if !out.is_null() {
        // SAFETY: the trampoline passes a valid out slot for the call.
        unsafe {
            (*out).ty = VAL_STRING;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = scratch.0.as_ptr() as *const c_char;
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = 0;
        }
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
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Write a retained id into `out` (`VAL_RETAINED`); the C++ side consumes the
/// retention while converting the native result.
fn set_retained_out(out: *mut Value, id: usize) {
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_RETAINED;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = id;
    }
}

/// Marshal a [`RetainedValue`] (or a decode/compile error) into the native
/// result slots.
fn finish_retained(
    out: *mut Value,
    out_error: *mut *mut c_char,
    services: &CompatServices,
    result: Result<RetainedValue, String>,
) -> c_int {
    match result {
        Ok(RetainedValue::Value(value)) => {
            set_scalar_out(out, services, value);
            0
        }
        Ok(RetainedValue::Object(object)) => {
            set_retained_out(out, object.raw_id() as usize);
            // The C++ trampoline consumes the retention when it converts the
            // native result, so the guard must not release it first.
            std::mem::forget(object);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Marshal a scalar [`TjsValue`] into the result slot.
fn set_scalar_out(out: *mut Value, services: &CompatServices, value: TjsValue) {
    match value {
        TjsValue::Void => set_void(out),
        TjsValue::Integer(integer) => set_int(out, integer),
        TjsValue::Real(real) => set_real(out, real),
        TjsValue::String(string) => set_string_out(out, services, &string),
        // Object results must go through the retained path; anything else
        // reaching here has no usable representation, so return void.
        TjsValue::Object | TjsValue::Retained(_) => set_void(out),
    }
}

/// The attached storage pointer, if the host attached one.
fn attached_storage(services: &CompatServices) -> Option<*mut Storage> {
    services.storage()
}

/// Interpret a value the way the reference's `(tjs_int)*param[i]` conversion
/// does for boolean flags.
fn truthy(value: &Value) -> bool {
    match value.ty {
        VAL_INTEGER => value.integer != 0,
        VAL_REAL => value.real != 0.0,
        _ => false,
    }
}

/// Parse a text-stream mode string for the `oN` byte offset (reference
/// `parseModeNumber(mode, 'o', 255, 0)`); other letters are ignored.
fn mode_offset(mode: &str) -> Option<usize> {
    let bytes = mode.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'o' {
            if !bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            let mut value = 0usize;
            for &b in &bytes[i + 1..] {
                if !b.is_ascii_digit() {
                    break;
                }
                value = value * 10 + usize::from(b - b'0');
            }
            return Some(value);
        }
        i += 1;
    }
    None
}

/// Heuristic: even-length payload with a high ratio of zero high bytes →
/// BOM-less UTF-16LE.
fn looks_like_utf16le(bytes: &[u8]) -> bool {
    if bytes.len() < 4 || !bytes.len().is_multiple_of(2) {
        return false;
    }
    let mut high_zeros = 0usize;
    for chunk in bytes.chunks_exact(2) {
        if chunk[1] == 0 {
            high_zeros += 1;
        }
    }
    high_zeros * 4 >= bytes.len() / 2
}

/// Decode a UTF-16LE byte payload.
fn decode_utf16le(bytes: &[u8]) -> Result<String, String> {
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]));
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map_err(|error| error.to_string())
}

/// Decode a UTF-16BE byte payload.
fn decode_utf16be(bytes: &[u8]) -> Result<String, String> {
    let units = bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]));
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map_err(|error| error.to_string())
}

/// Decode script bytes the way the reference text stream does, minus the
/// unavailable CP932 and `FE FE` container support. A leading BOM (or the
/// BOM-less UTF-16LE heuristic) wins; otherwise a configured non-UTF
/// `textEncoding` is rejected rather than ignored.
fn decode_script(bytes: &[u8], encoding: Option<&str>) -> Result<String, String> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(rest.to_vec()).map_err(|error| error.to_string());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16le(rest);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16be(rest);
    }
    if looks_like_utf16le(bytes) {
        return decode_utf16le(bytes);
    }
    if let Some(encoding) = encoding
        && !encoding.is_empty()
        && !encoding.eq_ignore_ascii_case("utf-8")
        && !encoding.eq_ignore_ascii_case("utf8")
    {
        return Err(format!(
            "configured text encoding '{encoding}' is not supported by the compat surface"
        ));
    }
    String::from_utf8(bytes.to_vec()).map_err(|_| {
        "script is not valid UTF-8 (CP932 and the FE FE container are not yet \
         supported by the compat surface)"
            .to_string()
    })
}

/// The configured `Scripts.textEncoding`, if the host/script set one.
fn configured_encoding(services: &CompatServices) -> Option<String> {
    services.state::<ScriptsState>().text_encoding.clone()
}

/// Read storage `name`, apply the mode's `oN` offset and decode the text.
fn load_storage_text(
    storage: &Storage,
    name: &str,
    mode: &str,
    encoding: Option<&str>,
) -> Result<String, String> {
    let bytes = storage
        .read(name)
        .map_err(|error| format!("Scripts: '{name}': {error}"))?;
    let bytes = match mode_offset(mode) {
        Some(offset) => &bytes[offset.min(bytes.len())..],
        None => &bytes[..],
    };
    decode_script(bytes, encoding).map_err(|error| format!("Scripts: '{name}': {error}"))
}

/// Read `name` through storage and run it as a script (`expression == false`)
/// or an expression (`expression == true`), retaining an object result.
fn execute_storage(
    engine: &Tjs2Engine,
    storage: &Storage,
    name: &str,
    mode: &str,
    expression: bool,
    encoding: Option<&str>,
) -> Result<RetainedValue, String> {
    let text = load_storage_text(storage, name, mode, encoding)?;
    if expression {
        engine
            .eval_retained(&text, name)
            .map_err(|error| error.to_string())
    } else {
        engine
            .exec_script_retained(&text, name)
            .map_err(|error| error.to_string())
    }
}

/// Resolve the process-wide binding for a callback's raw engine pointer.
macro_rules! resolve_or_return {
    ($engine:expr, $out_error:expr, $method:literal) => {
        match binding::resolve($engine as *mut Engine) {
            Some(resolved) => resolved,
            None => {
                return report_error(
                    $out_error,
                    concat!("Scripts.", $method, ": compat context is not bound"),
                );
            }
        }
    };
}

// ---------------------------------------------------------------------------
// storage-backed execution
// ---------------------------------------------------------------------------

extern "C" fn native_exec_storage(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "execStorage");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(name) = arguments.first() else {
        return report_error(
            out_error,
            "Scripts.execStorage requires at least 1 argument",
        );
    };
    let name = value_as_string(name);
    let mode = arguments.get(1).map(value_as_string).unwrap_or_default();
    if arguments.get(2).is_some_and(|value| value.ty != VAL_VOID) {
        log::warn!("Scripts.execStorage: the execution-context argument is ignored");
    }
    let encoding = configured_encoding(services);
    let Some(storage) = attached_storage(services) else {
        return report_error(out_error, "Scripts.execStorage: no storage attached");
    };
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let result = execute_storage(
        resolved.engine(),
        unsafe { &*storage },
        &name,
        &mode,
        false,
        encoding.as_deref(),
    );
    finish_retained(out, out_error, services, result)
}

extern "C" fn native_eval_storage(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "evalStorage");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(name) = arguments.first() else {
        return report_error(
            out_error,
            "Scripts.evalStorage requires at least 1 argument",
        );
    };
    let name = value_as_string(name);
    let mode = arguments.get(1).map(value_as_string).unwrap_or_default();
    if arguments.get(2).is_some_and(|value| value.ty != VAL_VOID) {
        log::warn!("Scripts.evalStorage: the execution-context argument is ignored");
    }
    let encoding = configured_encoding(services);
    let Some(storage) = attached_storage(services) else {
        return report_error(out_error, "Scripts.evalStorage: no storage attached");
    };
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let result = execute_storage(
        resolved.engine(),
        unsafe { &*storage },
        &name,
        &mode,
        true,
        encoding.as_deref(),
    );
    finish_retained(out, out_error, services, result)
}

// ---------------------------------------------------------------------------
// raw execution
// ---------------------------------------------------------------------------

extern "C" fn native_exec(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "exec");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(script) = arguments.first() else {
        return report_error(out_error, "Scripts.exec requires at least 1 argument");
    };
    let script = value_as_string(script);
    let name = arguments.get(1).map(value_as_string).unwrap_or_default();
    if arguments.get(2).is_some_and(|value| value.ty != VAL_VOID) {
        log::warn!("Scripts.exec: the lineofs argument cannot cross the ABI; ignoring");
    }
    if arguments.get(3).is_some_and(|value| value.ty != VAL_VOID) {
        log::warn!("Scripts.exec: the execution-context argument is ignored");
    }
    let result = resolved
        .engine()
        .exec_script_retained(&script, &name)
        .map_err(|error| error.to_string());
    finish_retained(out, out_error, services, result)
}

extern "C" fn native_eval(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "eval");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(expression) = arguments.first() else {
        return report_error(out_error, "Scripts.eval requires at least 1 argument");
    };
    let expression = value_as_string(expression);
    let name = arguments.get(1).map(value_as_string).unwrap_or_default();
    if arguments.get(2).is_some_and(|value| value.ty != VAL_VOID) {
        log::warn!("Scripts.eval: the lineofs argument cannot cross the ABI; ignoring");
    }
    if arguments.get(3).is_some_and(|value| value.ty != VAL_VOID) {
        log::warn!("Scripts.eval: the execution-context argument is ignored");
    }
    let result = resolved
        .engine()
        .eval_retained(&expression, &name)
        .map_err(|error| error.to_string());
    finish_retained(out, out_error, services, result)
}

extern "C" fn native_compile_storage(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "compileStorage");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments.len() < 2 {
        return report_error(
            out_error,
            "Scripts.compileStorage requires at least 2 arguments",
        );
    }
    let name = value_as_string(&arguments[0]);
    let output = value_as_string(&arguments[1]);
    let isresult = arguments.get(2).map(truthy).unwrap_or(false);
    let outputdebug = arguments.get(3).map(truthy).unwrap_or(false);
    let isexpression = arguments.get(4).map(truthy).unwrap_or(false);
    let encoding = configured_encoding(services);
    let Some(storage) = attached_storage(services) else {
        return report_error(out_error, "Scripts.compileStorage: no storage attached");
    };
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let text = match load_storage_text(unsafe { &*storage }, &name, "", encoding.as_deref()) {
        Ok(text) => text,
        Err(error) => return report_error(out_error, &error),
    };
    match resolved.engine().compile_script(
        &text,
        &output,
        isresult,
        outputdebug,
        isexpression,
        &name,
        0,
    ) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &format!("Scripts.compileStorage: {error}")),
    }
}

// ---------------------------------------------------------------------------
// introspection / missing-handler
// ---------------------------------------------------------------------------

extern "C" fn native_set_call_missing(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "setCallMissing");
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(object) = arguments.first() else {
        return report_error(out_error, "Scripts.setCallMissing requires 1 argument");
    };
    let engine = resolved.engine();
    let object = match engine.retain_object_arg(object) {
        Ok(object) => object,
        Err(_) => {
            return report_error(
                out_error,
                "Scripts.setCallMissing: argument is not an object",
            );
        }
    };
    match engine.set_call_missing(object.raw_id()) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &format!("Scripts.setCallMissing: {error}")),
    }
}

extern "C" fn native_get_class_names(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "getClassNames");
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(object) = arguments.first() else {
        return report_error(out_error, "Scripts.getClassNames requires 1 argument");
    };
    let engine = resolved.engine();
    let object = match engine.retain_object_arg(object) {
        Ok(object) => object,
        Err(_) => {
            return report_error(
                out_error,
                "Scripts.getClassNames: argument is not an object",
            );
        }
    };
    match engine.get_class_names(object.raw_id()) {
        Ok(names) => {
            set_retained_out(out, names.raw_id() as usize);
            // The C++ trampoline consumes the retention.
            std::mem::forget(names);
            0
        }
        Err(error) => report_error(out_error, &format!("Scripts.getClassNames: {error}")),
    }
}

extern "C" fn native_get_trace_string(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "getTraceString");
    let ctx = resolved.ctx();
    // SAFETY: `resolve_or_return!` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let limit = arguments
        .first()
        .and_then(|value| (value.ty == VAL_INTEGER).then_some(value.integer as i32))
        .unwrap_or(0);
    // SAFETY: the engine is live and the returned string is malloc'd.
    let pointer = unsafe { tjs::tjs2_get_stack_trace_string(resolved.engine().raw(), limit) };
    if pointer.is_null() {
        set_void(out);
        return 0;
    }
    // SAFETY: the pointer is NUL-terminated and owned by this caller.
    let trace = unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: the pointer was allocated by `tjs2_get_stack_trace_string`.
    unsafe { tjs::tjs2_free_string(pointer) };
    set_string_out(out, services, &trace);
    0
}

extern "C" fn native_dump(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let resolved = resolve_or_return!(engine, out_error, "dump");
    match resolved.engine().dump() {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &format!("Scripts.dump: {error}")),
    }
}

// ---------------------------------------------------------------------------
// textEncoding property
// ---------------------------------------------------------------------------

extern "C" fn native_text_encoding_get(
    engine: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        set_void(out);
        return 0;
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` succeeded for the installed, live binding.
    let services = unsafe { ctx.services() };
    let value = services
        .state::<ScriptsState>()
        .text_encoding
        .clone()
        .unwrap_or_else(|| "UTF-8".to_string());
    set_string_out(out, services, &value);
    0
}

extern "C" fn native_text_encoding_set(
    engine: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        return 1;
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` succeeded for the installed, live binding.
    let services = unsafe { ctx.services() };
    let name = if value.is_null() {
        String::new()
    } else {
        // SAFETY: the trampoline passes a valid value slot.
        value_as_string(unsafe { &*value })
    };
    services.state::<ScriptsState>().text_encoding =
        if name.is_empty() { None } else { Some(name) };
    0
}

/// Register the `Scripts` native class (static members plus the
/// `textEncoding` property).
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_class(&NativeClassBuilder {
        name: "Scripts",
        methods: vec![
            NativeMethodDef {
                name: "execStorage",
                f: native_exec_storage,
            },
            NativeMethodDef {
                name: "evalStorage",
                f: native_eval_storage,
            },
            NativeMethodDef {
                name: "exec",
                f: native_exec,
            },
            NativeMethodDef {
                name: "eval",
                f: native_eval,
            },
            NativeMethodDef {
                name: "compileStorage",
                f: native_compile_storage,
            },
            NativeMethodDef {
                name: "setCallMissing",
                f: native_set_call_missing,
            },
            NativeMethodDef {
                name: "getClassNames",
                f: native_get_class_names,
            },
            NativeMethodDef {
                name: "getTraceString",
                f: native_get_trace_string,
            },
            NativeMethodDef {
                name: "dump",
                f: native_dump,
            },
        ],
        properties: vec![NativePropertyDef {
            name: "textEncoding",
            get: Some(native_text_encoding_get),
            set: Some(native_text_encoding_set),
        }],
    })
}
