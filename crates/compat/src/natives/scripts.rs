//! `Scripts` native class (L3): storage-backed script execution helpers.
//!
//! Reimplemented over [`engine::Storage`] and the `tjs` engine handle exposed
//! by [`CompatCtx::engine`]. The reference is the `tvp-scripts` crate;
//! this port keeps only the parts the ABI supports and owns no state
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
//! `mode` honors the reference's `oN` byte offset. Script decoding goes
//! through [`crate::text::decode_text`], which handles the `FE FE` cipher /
//! zlib container, a UTF-8/UTF-16 BOM, an explicit `textEncoding` override,
//! then UTF-8 and CP932. An undecodable script raises a typed error instead
//! of producing mojibake. The `context` argument is the script's `this`, and
//! `lineofs` the reported line-number offset, both forwarded to the VM.
#![allow(unsafe_code)]

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::time::Instant;

use engine::Storage;
use tjs::{
    Engine, NativeClassBuilder, NativeMethodDef, NativePropertyDef, RetainedValue, Tjs2Engine,
    TjsValue, VAL_NULL, VAL_OBJECT, VAL_REAL, VAL_RETAINED, VAL_STRING, VAL_VOID, Value,
};

use crate::binding;
use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::natives::storages::extract_storage_name;
use crate::natives::system::try_value_as_i64;
use crate::{CompatCtx, CompatError, CompatServices};

/// Per-family dispatch state: the `Scripts.textEncoding` override and the
/// current script-execution nesting depth.
#[derive(Default)]
struct ScriptsState {
    /// The explicit read-encoding spelling, when a script set one. The
    /// *default* is the reference's `G_DefaultReadEncoding = "UTF-8"`
    /// (`base/TextStream.cpp:17`), but in the reference that value is only
    /// the last-resort fallback of `checkTextEncoding` — BOM and charset
    /// detection run first (`TextStream.cpp:21-66`, `141`) — so an unset
    /// property maps to `None` (the automatic decoder) rather than a forced
    /// UTF-8 override.
    text_encoding: Option<String>,
    /// Nesting depth of the script executions currently on the stack. The
    /// entry script runs at 0; a `Scripts.exec*`/`eval*` call it makes runs
    /// one deeper, which is what makes the emitted timing lines show nesting.
    depth: Cell<u32>,
}

/// Times one script execution and, at `debug`, logs its name and cost.
///
/// The guard restores the recorded depth on every exit path, including an
/// early storage/decode error. It exists only while `debug` logging is on, so
/// a release build pays nothing.
struct ScriptExecTimer<'a> {
    services: Option<&'a CompatServices>,
    label: &'a str,
    started: Instant,
    depth: u32,
}

impl<'a> ScriptExecTimer<'a> {
    /// Starts timing `label`, or returns `None` when `debug` logging is off.
    fn start(services: Option<&'a CompatServices>, label: &'a str) -> Option<Self> {
        if !log::log_enabled!(log::Level::Debug) {
            return None;
        }
        let depth = match services {
            Some(services) => {
                let state = services.state::<ScriptsState>();
                let depth = state.depth.get();
                state.depth.set(depth.saturating_add(1));
                depth
            }
            None => 0,
        };
        Some(Self {
            services,
            label,
            started: Instant::now(),
            depth,
        })
    }
}

impl Drop for ScriptExecTimer<'_> {
    fn drop(&mut self) {
        if let Some(services) = self.services {
            services.state::<ScriptsState>().depth.set(self.depth);
        }
        let ms = self.started.elapsed().as_secs_f64() * 1000.0;
        let indent = "  ".repeat(self.depth as usize);
        log::debug!("startup: script {indent}{} in {ms:.1} ms", self.label);
    }
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
/// does for boolean flags (`tjsVariant.h:862-877`): reals truncate, strings
/// parse, objects raise.
fn truthy(value: &Value) -> Result<bool, String> {
    try_value_as_i64(value).map(|value| value != 0)
}

/// An optional boolean flag argument: absent or `void` is false, present
/// values convert like the reference `(tjs_int)*param[i]`.
fn flag_arg(arguments: &[Value], index: usize) -> Result<bool, String> {
    match arguments.get(index) {
        Some(value) => truthy(value),
        None => Ok(false),
    }
}

/// The reference `TVPSetDefaultReadEncoding` spelling rules
/// (`base/TextStream.cpp:418-430`): the `sjis` aliases map to `cp932`,
/// `utf8`/`utf-8` to `UTF-8`, and any other spelling — including the empty
/// string — is stored verbatim.
fn reference_read_encoding(encoding: &str) -> String {
    match encoding.to_ascii_lowercase().as_str() {
        "sjis" | "shiftjis" | "shift_jis" | "shift-jis" => "cp932".to_string(),
        "utf8" | "utf-8" => "UTF-8".to_string(),
        _ => encoding.to_string(),
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

/// The configured `Scripts.textEncoding`, if it selects a decoder. An unset
/// property keeps the automatic decoder — the reference's `G_DefaultReadEncoding`
/// is only the fallback after BOM/detection (`base/TextStream.cpp:141`) — and
/// the empty spelling means auto-detection as well.
fn configured_encoding(services: &CompatServices) -> Option<String> {
    services
        .state::<ScriptsState>()
        .text_encoding
        .clone()
        .filter(|encoding| !encoding.is_empty())
}

/// Log one line describing a script read so a device log can identify the
/// raw bytes without dumping the script: the storage name, the byte length,
/// the first 32 bytes in hex, the encoding decision, the number of NUL bytes
/// in the raw input, and whether the decoded text contains a NUL.
fn log_script_read(name: &str, bytes: &[u8], encoding: Option<&str>, text: Option<&str>) {
    let head = bytes
        .iter()
        .take(32)
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ");
    let nul_bytes = bytes.iter().filter(|&&byte| byte == 0).count();
    let decoded_has_nul = text.is_some_and(|text| text.contains('\0'));
    log::debug!(
        "script read '{name}' bytes={} head32=[{head}] encoding={} nul_bytes={nul_bytes} decoded_has_nul={decoded_has_nul}",
        bytes.len(),
        encoding.unwrap_or("decode-error"),
    );
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
    let encoding = encoding.map_or(crate::text::TextEncoding::Detect, |name| {
        crate::text::TextEncoding::Named(name)
    });
    // The byte signature is only materialised when debug logging is on; the
    // decode itself is the normal path.
    let decoded = if log::log_enabled!(log::Level::Debug) {
        let reported = crate::text::decode_text_reported(bytes, encoding);
        match &reported {
            Ok(decoded) => log_script_read(
                name,
                bytes,
                Some(decoded.encoding.as_str()),
                Some(decoded.text.as_str()),
            ),
            Err(_) => log_script_read(name, bytes, None, None),
        }
        reported.map(|decoded| decoded.text)
    } else {
        crate::text::decode_text(bytes, encoding)
    };
    decoded.map_err(|error| format!("Scripts: '{name}': {error}"))
}

/// Read `name` through storage and run it as a script (`expression == false`)
/// or an expression (`expression == true`), retaining an object result. The
/// script block is labelled with `block_name` (the reference
/// `TVPExtractStorageName` of the placed path) and runs with `context` as its
/// `this` (`nullptr` for the global object).
#[allow(clippy::too_many_arguments)]
fn execute_storage(
    engine: &Tjs2Engine,
    storage: &Storage,
    name: &str,
    mode: &str,
    block_name: &str,
    context: *mut c_void,
    expression: bool,
    encoding: Option<&str>,
    services: Option<&CompatServices>,
) -> Result<RetainedValue, String> {
    let _timer = ScriptExecTimer::start(services, block_name);
    let text = load_storage_text(storage, name, mode, encoding)?;
    if expression {
        engine
            .eval_retained_with_context(&text, block_name, context, 0)
            .map_err(|error| error.to_string())
    } else {
        engine
            .exec_script_retained_with_context(&text, block_name, context, 0)
            .map_err(|error| error.to_string())
    }
}

/// Execute storage `name` as a script through `engine`: the core of
/// `Scripts.execStorage` without the native wrapper. The composition root
/// uses this to run the startup sequence.
///
/// # Errors
///
/// Returns the storage read, text decode, or script error as a string.
pub fn exec_storage(engine: &Tjs2Engine, storage: &Storage, name: &str) -> Result<(), String> {
    // The composition root calls this directly (the entry script), outside a
    // native callback, so resolve the dispatch state here to keep the
    // per-script nesting depth continuous across the boundary.
    let resolved = binding::resolve(engine.raw());
    let ctx = resolved.as_ref().map(|resolved| resolved.ctx());
    let services = ctx.as_ref().map(|ctx| unsafe { ctx.services() });
    execute_storage(
        engine,
        storage,
        name,
        "",
        &extract_storage_name(name),
        std::ptr::null_mut(),
        false,
        None,
        services,
    )
    .map(|_| ())
}

/// The `this` a reference `Scripts` execution member runs with: an object
/// argument's handle, or null for an absent/void/null argument. A present
/// non-object value raises the reference's variant conversion error
/// (`tTJSVariant::AsObjectNoAddRef`, `tjsVariant.h:694-699`).
pub(crate) fn context_argument(arguments: &[Value], index: usize) -> Result<*mut c_void, String> {
    match arguments.get(index) {
        None => Ok(std::ptr::null_mut()),
        Some(value) if value.ty == VAL_VOID || value.ty == VAL_NULL => Ok(std::ptr::null_mut()),
        Some(value) if value.ty == VAL_OBJECT => Ok(value.object_handle()),
        Some(_) => Err("the execution context is not an object".to_string()),
    }
}

/// The optional `lineofs` argument of `Scripts.exec`/`Scripts.eval`, using the
/// reference `(tjs_int)*param[2]` conversion (reals truncate, strings parse,
/// objects raise).
pub(crate) fn lineofs_argument(arguments: &[Value], index: usize) -> Result<i32, String> {
    match arguments.get(index) {
        Some(value) if value.ty != VAL_VOID => try_value_as_i64(value).map(|value| value as i32),
        _ => Ok(0),
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
    let _native = crate::natives::NativeTimer::start(services, "Scripts.execStorage");
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
    let context = match context_argument(arguments, 2) {
        Ok(context) => context,
        Err(error) => return report_error(out_error, &format!("Scripts.execStorage: {error}")),
    };
    let block_name = extract_storage_name(&name);
    log::debug!("Scripts.execStorage: {name:?} mode={mode:?}");
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
        &block_name,
        context,
        false,
        encoding.as_deref(),
        Some(services),
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
    let _native = crate::natives::NativeTimer::start(services, "Scripts.evalStorage");
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
    let context = match context_argument(arguments, 2) {
        Ok(context) => context,
        Err(error) => return report_error(out_error, &format!("Scripts.evalStorage: {error}")),
    };
    let block_name = extract_storage_name(&name);
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
        &block_name,
        context,
        true,
        encoding.as_deref(),
        Some(services),
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
    let _native = crate::natives::NativeTimer::start(services, "Scripts.exec");
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(script) = arguments.first() else {
        return report_error(out_error, "Scripts.exec requires at least 1 argument");
    };
    let script = value_as_string(script);
    let name = arguments.get(1).map(value_as_string).unwrap_or_default();
    let label = if name.is_empty() {
        "Scripts.exec"
    } else {
        name.as_str()
    };
    let _script = ScriptExecTimer::start(Some(services), label);
    let lineofs = match lineofs_argument(arguments, 2) {
        Ok(lineofs) => lineofs,
        Err(error) => return report_error(out_error, &format!("Scripts.exec: {error}")),
    };
    let context = match context_argument(arguments, 3) {
        Ok(context) => context,
        Err(error) => return report_error(out_error, &format!("Scripts.exec: {error}")),
    };
    let result = resolved
        .engine()
        .exec_script_retained_with_context(&script, &name, context, lineofs)
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
    let _native = crate::natives::NativeTimer::start(services, "Scripts.eval");
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(expression) = arguments.first() else {
        return report_error(out_error, "Scripts.eval requires at least 1 argument");
    };
    let expression = value_as_string(expression);
    let name = arguments.get(1).map(value_as_string).unwrap_or_default();
    let label = if name.is_empty() {
        "Scripts.eval"
    } else {
        name.as_str()
    };
    let _script = ScriptExecTimer::start(Some(services), label);
    let lineofs = match lineofs_argument(arguments, 2) {
        Ok(lineofs) => lineofs,
        Err(error) => return report_error(out_error, &format!("Scripts.eval: {error}")),
    };
    let context = match context_argument(arguments, 3) {
        Ok(context) => context,
        Err(error) => return report_error(out_error, &format!("Scripts.eval: {error}")),
    };
    let result = resolved
        .engine()
        .eval_retained_with_context(&expression, &name, context, lineofs)
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
    let _native = crate::natives::NativeTimer::start(services, "Scripts.compileStorage");
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
    let isresult = match flag_arg(arguments, 2) {
        Ok(flag) => flag,
        Err(error) => return report_error(out_error, &format!("Scripts.compileStorage: {error}")),
    };
    let outputdebug = match flag_arg(arguments, 3) {
        Ok(flag) => flag,
        Err(error) => return report_error(out_error, &format!("Scripts.compileStorage: {error}")),
    };
    let isexpression = match flag_arg(arguments, 4) {
        Ok(flag) => flag,
        Err(error) => return report_error(out_error, &format!("Scripts.compileStorage: {error}")),
    };
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
    // Reference `setCallMissing`: `AsObjectNoAddRef` yields a null pointer for
    // TJS `null` and `if(dsp)` skips it silently (`ScriptMgnIntf.cpp:1478-1492`);
    // a non-object raises the conversion error.
    if object.ty == tjs::VAL_NULL {
        set_void(out);
        return 0;
    }
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
    match engine.class_names(object.raw_id()) {
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
    // Reference `limit = *param[0]` when present and non-void: the
    // `operator tjs_int` conversion truncates reals, parses strings and
    // raises on objects (`KAGParser`-style `AsInteger`,
    // `tjsVariant.h:933-935`; `ScriptMgnIntf.cpp:1453-1456`).
    let limit = match arguments.first() {
        Some(value) if value.ty != VAL_VOID => match try_value_as_i64(value) {
            Ok(limit) => limit as i32,
            Err(error) => {
                return report_error(out_error, &format!("Scripts.getTraceString: {error}"));
            }
        },
        _ => 0,
    };
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
    // The unset default reports the reference's `G_DefaultReadEncoding`
    // (`TextStream.cpp:17`); a script-set spelling is reported verbatim
    // (through the `sjis`/`utf8` alias normalization).
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
    services.state::<ScriptsState>().text_encoding = Some(reference_read_encoding(&name));
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tjs::Tjs2Engine;

    use crate::CompatHost;
    use crate::native_util::test_vm_lock;
    use crate::natives::NativeTiming;

    /// A bound VM with the compat surface installed, for the timing tests.
    struct Env {
        host: CompatHost,
        engine: Box<Tjs2Engine>,
    }

    impl Env {
        fn new() -> Env {
            let engine = Box::new(Tjs2Engine::new().expect("create engine"));
            let mut host = CompatHost::new();
            host.install(&engine).expect("install compat surface");
            Env { host, engine }
        }
    }

    /// A script dominated by one native is attributed to that native's name.
    #[test]
    fn native_timing_attributes_a_dominated_script() {
        let _lock = test_vm_lock();
        let env = Env::new();
        env.host.begin_native_timing();
        env.engine
            .exec_script(
                "Scripts.exec('var total = 0; for(var i = 0; i < 400000; i++) total += i;', 'slow');",
                "timing-attribution",
            )
            .expect("run the busy script");
        env.host.end_native_timing();

        let timing = env.host.services().state::<NativeTiming>();
        let recorded = timing.recorded();
        assert_eq!(recorded.len(), 1);
        let entry = recorded
            .iter()
            .find(|entry| entry.0 == "Scripts.exec")
            .expect("Scripts.exec is recorded");
        assert_eq!(entry.1, 1);
        assert!(entry.2 > 0);
    }

    /// A fast native reports below the threshold; one that reaches it reports
    /// above.
    #[test]
    fn native_timing_report_level_tracks_the_threshold() {
        let timing = NativeTiming::default();
        timing.begin();
        timing.record("Scripts.exec", Duration::from_micros(1));
        let (slow, line) = timing.format_report().expect("a report");
        assert!(!slow);
        assert!(line.contains("Scripts.exec"));

        timing.begin();
        timing.record("Scripts.exec", Duration::from_millis(120));
        let (slow, _) = timing.format_report().expect("a report");
        assert!(slow);
    }
}
