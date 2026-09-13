//! The `Scripts` native class: `execStorage` / `evalStorage` / `exec` /
//! `eval` over mounted game storage, ported from the reference
//! `tTJSNC_Scripts` (`core/base/ScriptMgnIntf.cpp`).
//!
//! `Scripts` is a static native class registered on the VM global object.
//! The engine hands over its context (the running VM + the mounted
//! storage) with [`set_context`] before executing `startup.tjs`;
//! `execStorage` then calls back into the *same* VM — this is how scenario
//! and template scripts load each other.
//!
//! # Method semantics (matching the reference)
//!
//! - `execStorage(name[, mode])` — resolve `name` through storage, decode
//!   the text (BOM → UTF-8 → CP932), and execute it as a script in the
//!   VM. The result is the value of the script's *top-level `return`*
//!   statement; a bare trailing expression statement is **not** returned
//!   (the TJS2 VM only captures a result register that `return` writes —
//!   see `tjsInterCodeGen.cpp` `ReturnFromFunc` vs. `CreateExprCode`).
//!   Use `evalStorage` for expression results.
//! - `evalStorage(name[, mode])` — same, but the text is evaluated as an
//!   expression and its value returned.
//! - `exec(script[, name[, lineofs]])` — execute a raw script string.
//! - `eval(expression[, name[, lineofs]])` — evaluate a raw expression.
//!
//! `mode` is the reference's text-stream mode string: only the `oN` offset
//! is honored (applied by slicing the read bytes, mirroring
//! `parseModeNumber(mode, 'o', 255, 0)` in `TextStream.cpp`); unknown
//! letters are ignored like the reference ignores them, and cipher /
//! compression script data (the `FE FE` magic the reference decrypts in
//! `tTVPTextReadStream`) is detected and reported as an error rather than
//! producing mojibake.
//!
//! `lineofs` (line-number offset for error reporting) cannot reach the C
//! ABI yet and is ignored with a warning. The reference's optional
//! `context` argument (execute inside another object's context) is not
//! supported either, and is ignored with a warning.
//!
//! `getTraceString(limit)` is real (over `tjs2_get_stack_trace_string`), and
//! `textEncoding` is a real get/set property: a non-empty value becomes the
//! fallback decoder for scripts with no BOM (reference
//! `G_DefaultReadEncoding`), while the default (`"UTF-8"`) keeps the port's
//! automatic BOM → UTF-16LE → UTF-8 → CP932 detection.
//!
//! # Pending (need VM internals; not implemented)
//!
//! - `compileStorage` — reference `TVPCompileStorage`: compiles a storage
//!   script to bytecode and writes it to an output stream. The C ABI exposes
//!   no bytecode writer.
//! - `dump` — reference `TVPDumpScriptEngine`: dumps the VM's compiled
//!   code. The C ABI exposes no dump entry point.
//! - `setCallMissing` / `getClassNames` — need `iTJSDispatch2::
//!   ClassInstanceInfo` (`TJS_CII_SET_MISSING` / `TJS_CII_GET`), which the C
//!   ABI does not expose.
//! - `dumpStringHeap` — reference `TJSDumpStringHeap()` (debug builds
//!   only).
//!
//! # ABI limits
//!
//! The C ABI (`tjs2_value` / `value_to_variant` in `tjs2_abi.cpp`)
//! marshals void / integer / real / string results directly. An object or
//! function result is retained on the engine and returned through the
//! `VAL_RETAINED` slot instead (see `Tjs2Engine::eval_retained` and
//! `RetainedValue`); the C++ trampoline consumes that retention when it
//! converts the native's return value.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::io::Read;
use std::ptr;
use std::sync::{Arc, Mutex};

use engine::Storage;
use tjs2_sys::{
    NativeClassBuilder, NativeMethodDef, NativePropertyDef, RetainedValue, Tjs2Engine, TjsValue,
    VAL_INTEGER, VAL_REAL, VAL_RETAINED, VAL_STRING, VAL_VOID, Value,
};
use tvp_util::encoding;

// ---------------------------------------------------------------------------
// context
// ---------------------------------------------------------------------------

/// Engine-side context: the running VM and the mounted game storage.
/// `execStorage`/`evalStorage` need both; `exec`/`eval` only the engine.
#[derive(Default)]
struct Context {
    engine: Option<Arc<Tjs2Engine>>,
    storage: Option<Arc<Mutex<Storage>>>,
}

/// The VM runs on a Bevy worker thread while `set_context` is called on the
/// main thread during startup, so the context must be process-global: a
/// thread-local would be empty on the worker. The TJS VM is single-threaded
/// and serialized by the render crate's `VM_RUN_LOCK`, so a `Mutex` is safe
/// (the getters clone the `Arc`s and release it before any VM call).
static CONTEXT: Mutex<Context> = Mutex::new(Context {
    engine: None,
    storage: None,
});

/// Process-global `Scripts.textEncoding` override. `None` keeps the port's
/// automatic detection (BOM → UTF-16LE heuristic → UTF-8 → CP932); a set
/// value is used verbatim as the fallback decoder, mirroring the reference
/// `G_DefaultReadEncoding` (`TextStream.cpp:141`). The getter reports
/// `"UTF-8"` when unset, matching `TVPGetDefaultReadEncoding`.
static TEXT_ENCODING: Mutex<Option<String>> = Mutex::new(None);

/// The current `Scripts.textEncoding` value for the getter.
fn text_encoding_value() -> String {
    TEXT_ENCODING
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
        .unwrap_or_else(|| "UTF-8".to_string())
}

/// The configured fallback encoding, if any.
fn configured_text_encoding() -> Option<String> {
    TEXT_ENCODING
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone()
}

/// Point the `Scripts` class at the running VM and storage.
///
/// The engine calls this before executing `startup.tjs` so that
/// `Scripts.execStorage` can call back into the same VM (this is how
/// k2compat-style scripts load each other).
pub fn set_context(engine: Option<Arc<Tjs2Engine>>, storage: Option<Arc<Mutex<Storage>>>) {
    *CONTEXT.lock().unwrap_or_else(|p| p.into_inner()) = Context { engine, storage };
}

/// Clone the engine + storage handles out of the context (the guard ends
/// before any VM call, so nested `Scripts.*` calls can re-enter freely).
fn context_engine_and_storage() -> Result<(Arc<Tjs2Engine>, Arc<Mutex<Storage>>), String> {
    let ctx = CONTEXT.lock().unwrap_or_else(|p| p.into_inner());
    match (&ctx.engine, &ctx.storage) {
        (Some(engine), Some(storage)) => Ok((engine.clone(), storage.clone())),
        _ => Err(
            "Scripts context is not set: set_context(engine, storage) must be called \
             before using Scripts.execStorage/evalStorage"
                .into(),
        ),
    }
}

/// Clone the engine handle out of the context.
fn context_engine() -> Result<Arc<Tjs2Engine>, String> {
    let ctx = CONTEXT.lock().unwrap_or_else(|p| p.into_inner());
    ctx.engine
        .clone()
        .ok_or_else(|| "Scripts context is not set: set_context(engine, ...) must be called".into())
}

// ---------------------------------------------------------------------------
// script loading
// ---------------------------------------------------------------------------

/// Decode script bytes the way the reference text stream does: honor a
/// leading BOM, else UTF-8, else CP932 (mirrors `ks_check.rs`).
/// Decompress/decrypt a script or save stream with the reference's `FE FE`
/// magic (tTVPTextReadStream, TextStream.cpp):
///
/// * mode 2 — zlib stream: `FE FE 02 FF FE <compressed:u64 LE>
///   <uncompressed:u64 LE> <zlib data>`; the payload is UTF-16LE text.
/// * mode 0/1 — XOR / bit-rotate ciphers over UTF-16 text (rare; the game's
///   system.dat uses mode 2).
///
/// Returns the decoded UTF-16LE bytes (no BOM) for non-magic data unchanged.
fn decompress_script(bytes: &[u8]) -> Result<Vec<u8>, String> {
    if bytes.len() < 3 || bytes[0] != 0xFE || bytes[1] != 0xFE {
        return Ok(bytes.to_vec());
    }
    let mode = bytes[2];
    if mode == 2 {
        // bytes[3..5] is the UTF-16LE BOM marker; sizes start at +5.
        if bytes.len() < 21 {
            return Err("FE FE data too short for compressed mode".into());
        }
        let compressed = u64::from_le_bytes(bytes[5..13].try_into().unwrap()) as usize;
        let uncompressed = u64::from_le_bytes(bytes[13..21].try_into().unwrap()) as usize;
        if 21 + compressed > bytes.len() {
            return Err("FE FE compressed size overruns the data".into());
        }
        let mut out = Vec::with_capacity(uncompressed);
        let mut dec = flate2::read::ZlibDecoder::new(&bytes[21..21 + compressed]);
        dec.read_to_end(&mut out)
            .map_err(|e| format!("FE FE zlib decode failed: {e}"))?;
        if out.len() != uncompressed {
            return Err(format!(
                "FE FE size mismatch: expected {uncompressed}, got {}",
                out.len()
            ));
        }
        Ok(out)
    } else if mode == 0 || mode == 1 {
        // UTF-16LE payload with a simple per-char cipher.
        if bytes.len() < 4 {
            return Err("FE FE data too short for cipher mode".into());
        }
        let src = &bytes[4..];
        if !src.len().is_multiple_of(2) {
            return Err("FE FE cipher payload must be even-length".into());
        }
        let mut out = Vec::with_capacity(src.len());
        for chunk in src.chunks_exact(2) {
            let mut ch = u16::from_le_bytes([chunk[0], chunk[1]]);
            if mode == 0 {
                if ch >= 0x20 {
                    ch ^= ((ch & 0xfe) << 8) ^ 1;
                }
            } else {
                ch = ((ch & 0xaaaa) >> 1) | ((ch & 0x5555) << 1);
            }
            out.extend_from_slice(&ch.to_le_bytes());
        }
        Ok(out)
    } else {
        Err(format!("FE FE unsupported mode {mode}"))
    }
}

fn decode_script(bytes: &[u8]) -> Result<String, String> {
    let (body, enc) = encoding::strip_bom(bytes);
    match enc {
        Some(enc) => enc.decode(body).map_err(|e| e.to_string()),
        None => {
            if looks_like_utf16le(body) {
                // BOM-less UTF-16LE (the FE FE decompressor yields this for
                // `(const) [...]` save data).
                let units: Vec<u16> = body
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                return String::from_utf16(&units).map_err(|e| e.to_string());
            }
            // An explicit `Scripts.textEncoding` wins over the heuristic
            // fallback, like the reference's `G_DefaultReadEncoding`.
            if let Some(name) = configured_text_encoding()
                && !name.is_empty()
            {
                return encoding::decode(body, &name).map_err(|e| e.to_string());
            }
            match String::from_utf8(body.to_vec()) {
                Ok(text) => Ok(text),
                Err(_) => encoding::decode(body, "cp932").map_err(|e| e.to_string()),
            }
        }
    }
}

/// Heuristic: even-length payload with a high ratio of zero bytes in the
/// high position → BOM-less UTF-16LE.
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
    high_zeros * 4 >= bytes.len() / 2 // >= 25% of units have a zero high byte
}

/// Parse a text-stream mode string for the `oN` byte offset, mirroring the
/// reference `parseModeNumber(mode, 'o', 255, 0)` (`BinaryStream.h`): the
/// offset applies only when an `o` is directly followed by digits; other
/// letters are ignored. `None` means "no offset".
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

/// Read storage `name` (honoring the mode's `oN` offset), decode it and
/// run it in the context VM as a script (`expression == false`) or an
/// expression (`expression == true`), retaining an object result so it can
/// cross the ABI.
fn execute_storage(name: &str, mode: &str, expression: bool) -> Result<RetainedValue, String> {
    let (engine, storage) = context_engine_and_storage()?;

    let bytes = {
        // The lock is released before the VM runs, so a script executed
        // here may itself call Scripts.execStorage without deadlocking.
        let mut storage = storage
            .lock()
            .map_err(|_| "Scripts: storage lock is poisoned".to_string())?;
        storage.read(name).map_err(|e| e.to_string())?
    };

    // Apply the reference's stream offset (SetPosition(ofs) before read);
    // an offset past the end yields an empty script.
    let bytes = match mode_offset(mode) {
        Some(ofs) => &bytes[ofs.min(bytes.len())..],
        None => &bytes[..],
    };

    // The reference's tTVPTextReadStream decrypts/decompresses data with
    // the FE FE magic (TextStream.cpp): `FE FE <mode> FF FE <compressed:u64>
    // <uncompressed:u64> <zlib stream>` for mode 2; mode 0/1 are the
    // simple XOR/rotate ciphers for UTF-16 text.
    let bytes = match decompress_script(bytes) {
        Ok(b) => b,
        Err(e) => return Err(format!("Scripts: '{name}': {e}")),
    };

    let text = decode_script(&bytes)?;
    log::debug!(
        "Scripts: {} '{name}' ({} bytes)",
        if expression { "eval" } else { "exec" },
        text.len()
    );
    if expression {
        engine.eval_retained(&text, name).map_err(|e| e.to_string())
    } else {
        engine
            .exec_script_retained(&text, name)
            .map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// result marshaling
// ---------------------------------------------------------------------------

thread_local! {
    /// Scratch buffer for string results: stays valid until the next native
    /// callback on this thread, which is long enough — the C++ trampoline
    /// (`value_to_variant`) copies the string immediately after the
    /// callback returns.
    static STRING_OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Build a malloc'd NUL-terminated UTF-8 error message for `*out_error`
/// (the C++ trampoline frees it with `tjs2_free_string`).
fn alloc_error_string(msg: &str) -> *mut c_char {
    let bytes = msg.as_bytes();
    // SAFETY: tjs2_malloc is malloc-compatible; we write a NUL-terminated
    // copy that the C++ side owns and frees with tjs2_free_string.
    unsafe {
        let buf = tjs2_sys::tjs2_malloc(bytes.len() + 1) as *mut u8;
        if buf.is_null() {
            return ptr::null_mut();
        }
        ptr::copy_nonoverlapping(bytes.as_ptr(), buf, bytes.len());
        *buf.add(bytes.len()) = 0;
        buf as *mut c_char
    }
}

/// Set `*out_error` to a malloc'd copy of `msg` and return the non-zero
/// native code that the C++ trampoline turns into a catchable TJS
/// exception.
fn error_out(out_error: *mut *mut c_char, msg: &str) -> c_int {
    if !out_error.is_null() {
        // SAFETY: out_error points at a char* slot owned by the C++
        // trampoline for the duration of the call.
        unsafe { *out_error = alloc_error_string(msg) };
    }
    1
}

/// Write `s` into `*out` as a string using the thread-local buffer.
fn set_string_result(out: *mut Value, s: &str) {
    STRING_OUT.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.clear();
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
        // SAFETY: `out` is a valid result slot provided by the C++
        // trampoline for the duration of the call.
        unsafe {
            (*out).ty = VAL_STRING;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = buf.as_ptr() as *const c_char;
        }
    });
}

/// Marshal an owned [`TjsValue`] into the C result slot.
fn set_out_result(out: *mut Value, v: TjsValue) {
    match v {
        TjsValue::Void => set_out_void(out),
        TjsValue::Integer(i) => {
            // SAFETY: `out` is a valid result slot for the duration of the call.
            unsafe {
                (*out).ty = VAL_INTEGER;
                (*out).integer = i;
                (*out).real = 0.0;
                (*out).string = ptr::null();
            }
        }
        TjsValue::Real(r) => {
            // SAFETY: `out` is a valid result slot for the duration of the call.
            unsafe {
                (*out).ty = VAL_REAL;
                (*out).integer = 0;
                (*out).real = r;
                (*out).string = ptr::null();
            }
        }
        TjsValue::String(s) => set_string_result(out, &s),
        TjsValue::Retained(id) => set_out_retained(out, id as usize),
        TjsValue::Object => {
            // Object results go through the retained path
            // (`eval_retained` / `exec_script_retained`); an object reaching
            // this scalar-only marshaller means it was not retained, so
            // fall back to void rather than lying about a handle.
            log::warn!("Scripts: unretained object result; returning void");
            set_out_void(out);
        }
    }
}

/// Write a retained id into `*out` as `VAL_RETAINED`. The C++ trampoline
/// copies the retained value (consuming the map entry) when the native
/// returns, so the caller must not release the id first.
fn set_out_retained(out: *mut Value, id: usize) {
    // SAFETY: `out` is a valid result slot for the duration of the call.
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

fn set_out_void(out: *mut Value) {
    // SAFETY: `out` is a valid result slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

/// Convert a native argument to a string the way `ttstr x = *param[i]`
/// does in the reference: strings as-is, integers/reals formatted, void as
/// the empty string; objects cannot be converted.
fn param_to_string(v: &Value) -> Result<String, String> {
    match v.ty {
        VAL_STRING => {
            if v.string.is_null() {
                Ok(String::new())
            } else {
                // SAFETY: argv strings are NUL-terminated UTF-8, valid for
                // the duration of the call.
                Ok(unsafe { CStr::from_ptr(v.string) }
                    .to_string_lossy()
                    .into_owned())
            }
        }
        VAL_INTEGER => Ok(v.integer.to_string()),
        VAL_REAL => Ok(v.real.to_string()),
        _ => Err("Scripts: argument cannot be converted to a string".into()),
    }
}

/// Marshal a `RetainedValue` produced by the context VM into the C result
/// slot. Object results are handed over as a `VAL_RETAINED` id; the C++
/// side consumes the retention when it converts the result after the
/// callback returns, so the guard is leaked here.
fn finish_retained(
    out: *mut Value,
    out_error: *mut *mut c_char,
    result: Result<RetainedValue, String>,
) -> c_int {
    match result {
        Ok(RetainedValue::Value(v)) => {
            set_out_result(out, v);
            0
        }
        Ok(RetainedValue::Object(dv)) => {
            set_out_retained(out, dv.raw_id() as usize);
            // The C++ conversion consumes the retention after this callback
            // returns; dropping the guard now would erase the map entry
            // first, so leak it. (The release is idempotent, so the leaked
            // guard would be a no-op even if it did run.)
            std::mem::forget(dv);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

// ---------------------------------------------------------------------------
// native methods
// ---------------------------------------------------------------------------

/// `Scripts.execStorage(name[, mode[, context]])`.
extern "C" fn native_exec_storage(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return error_out(
            out_error,
            "Scripts.execStorage requires at least 1 argument",
        );
    }
    // SAFETY: argv points to `argc` valid entries for the duration of the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let name = match param_to_string(&args[0]) {
        Ok(name) => name,
        Err(e) => return error_out(out_error, &e),
    };
    let mode = if argc >= 2 && args[1].ty != VAL_VOID {
        match param_to_string(&args[1]) {
            Ok(mode) => mode,
            Err(e) => return error_out(out_error, &e),
        }
    } else {
        String::new()
    };
    if argc >= 3 && args[2].ty != VAL_VOID {
        log::warn!(
            "Scripts.execStorage: the execution-context argument is not supported yet; ignoring"
        );
    }
    let result = execute_storage(&name, &mode, false);
    // Object results (e.g. `(const) [...]` save data, or a function
    // expression) are retained and returned across the ABI, exactly like
    // evalStorage.
    finish_retained(out, out_error, result)
}

/// `Scripts.evalStorage(name[, mode[, context]])`.
extern "C" fn native_eval_storage(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return error_out(
            out_error,
            "Scripts.evalStorage requires at least 1 argument",
        );
    }
    // SAFETY: argv points to `argc` valid entries for the duration of the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let name = match param_to_string(&args[0]) {
        Ok(name) => name,
        Err(e) => return error_out(out_error, &e),
    };
    let mode = if argc >= 2 && args[1].ty != VAL_VOID {
        match param_to_string(&args[1]) {
            Ok(mode) => mode,
            Err(e) => return error_out(out_error, &e),
        }
    } else {
        String::new()
    };
    if argc >= 3 && args[2].ty != VAL_VOID {
        log::warn!(
            "Scripts.evalStorage: the execution-context argument is not supported yet; ignoring"
        );
    }
    let result = execute_storage(&name, &mode, true);
    finish_retained(out, out_error, result)
}

/// `Scripts.exec(script[, name[, lineofs[, context]]])`.
extern "C" fn native_exec(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return error_out(out_error, "Scripts.exec requires at least 1 argument");
    }
    // SAFETY: argv points to `argc` valid entries for the duration of the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let script = match param_to_string(&args[0]) {
        Ok(script) => script,
        Err(e) => return error_out(out_error, &e),
    };
    let name = if argc >= 2 && args[1].ty != VAL_VOID {
        match param_to_string(&args[1]) {
            Ok(name) => name,
            Err(e) => return error_out(out_error, &e),
        }
    } else {
        String::new()
    };
    if argc >= 3 && args[2].ty != VAL_VOID {
        log::warn!("Scripts.exec: the lineofs argument cannot cross the C ABI yet; ignoring");
    }
    if argc >= 4 && args[3].ty != VAL_VOID {
        log::warn!("Scripts.exec: the execution-context argument is not supported yet; ignoring");
    }
    let result = match context_engine() {
        Ok(engine) => engine
            .exec_script_retained(&script, &name)
            .map_err(|e| e.to_string()),
        Err(e) => Err(e),
    };
    finish_retained(out, out_error, result)
}

/// `Scripts.eval(expression[, name[, lineofs[, context]]])`.
extern "C" fn native_eval(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return error_out(out_error, "Scripts.eval requires at least 1 argument");
    }
    // SAFETY: argv points to `argc` valid entries for the duration of the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let expression = match param_to_string(&args[0]) {
        Ok(expression) => expression,
        Err(e) => return error_out(out_error, &e),
    };
    let name = if argc >= 2 && args[1].ty != VAL_VOID {
        match param_to_string(&args[1]) {
            Ok(name) => name,
            Err(e) => return error_out(out_error, &e),
        }
    } else {
        String::new()
    };
    if argc >= 3 && args[2].ty != VAL_VOID {
        log::warn!("Scripts.eval: the lineofs argument cannot cross the C ABI yet; ignoring");
    }
    if argc >= 4 && args[3].ty != VAL_VOID {
        log::warn!("Scripts.eval: the execution-context argument is not supported yet; ignoring");
    }
    let result = match context_engine() {
        Ok(engine) => engine
            .eval_retained(&expression, &name)
            .map_err(|e| e.to_string()),
        Err(e) => Err(e),
    };
    finish_retained(out, out_error, result)
}

// ---------------------------------------------------------------------------
// registration
// ---------------------------------------------------------------------------

/// `Scripts.getTraceString(limit?)` — the current VM stack trace as a string
/// (reference `TJSGetStackTraceString`).
extern "C" fn native_get_trace_string(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let limit = if argc >= 1 && !argv.is_null() {
        // SAFETY: argv/argc follow the contract.
        unsafe { &*argv }.integer as i32
    } else {
        0
    };
    // SAFETY: engine is a live engine; the string is malloc'd and freed
    // below (the C++ trampoline copies the result immediately).
    let ptr =
        unsafe { tjs2_sys::tjs2_get_stack_trace_string(engine as *mut tjs2_sys::Engine, limit) };
    if ptr.is_null() {
        set_out_void(out);
        return 0;
    }
    // SAFETY: ptr is NUL-terminated (allocated by the C++ side).
    let s = unsafe { std::ffi::CStr::from_ptr(ptr) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: ptr was malloc'd by tjs2_get_stack_trace_string.
    unsafe { tjs2_sys::tjs2_free_string(ptr) };
    set_string_result(out, &s);
    0
}

/// `Scripts.textEncoding` getter (reference `TVPGetDefaultReadEncoding`).
extern "C" fn native_text_encoding_get(
    _engine: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    set_string_result(out, &text_encoding_value());
    0
}

/// `Scripts.textEncoding` setter (reference `TVPSetDefaultReadEncoding`).
/// An empty string restores the port's automatic detection.
extern "C" fn native_text_encoding_set(
    _engine: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    // SAFETY: the C++ trampoline passes a valid value slot.
    let name = value_as_string_arg(unsafe { &*value });
    *TEXT_ENCODING.lock().unwrap_or_else(|p| p.into_inner()) =
        if name.is_empty() { None } else { Some(name) };
    0
}

/// Convert an ABI value to a string for the property setter (void/number →
/// empty/adecimal, objects → empty).
fn value_as_string_arg(v: &Value) -> String {
    match v.ty {
        VAL_STRING if v.string.is_null() => String::new(),
        VAL_STRING => {
            // SAFETY: argv strings are NUL-terminated UTF-8 for the call.
            unsafe { CStr::from_ptr(v.string) }
                .to_string_lossy()
                .into_owned()
        }
        VAL_INTEGER => v.integer.to_string(),
        VAL_REAL => v.real.to_string(),
        _ => String::new(),
    }
}

/// Register the `Scripts` native class on `engine`'s global object.
pub fn register_scripts(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class(&NativeClassBuilder {
        name: "Scripts",
        properties: vec![NativePropertyDef {
            name: "textEncoding",
            get: Some(native_text_encoding_get),
            set: Some(native_text_encoding_set),
        }],
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
                name: "getTraceString",
                f: native_get_trace_string,
            },
        ],
    })
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod test_lock {
    /// The TJS2 VM is single-threaded and the script natives keep a
    /// process-global engine context; parallel tests race on it
    /// (segfault). Serialize with one process-wide lock.
    static VM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn vm_lock() -> std::sync::MutexGuard<'static, ()> {
        VM_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use crate::test_lock::vm_lock;

    use super::*;
    use tjs2_sys::TjsValue;

    /// A scratch game directory under the system temp dir, removed on drop.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let base = std::env::temp_dir().join(format!(
                "krkr-rs-tvp-scripts-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .expect("clock before epoch")
                    .as_nanos(),
                tag
            ));
            std::fs::create_dir_all(&base).expect("create temp game dir");
            TempDir(base)
        }

        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// An engine + mounted storage with `Scripts` registered and the
    /// context set, plus the temp game dir holding `files`.
    struct TestEnv {
        _dir: TempDir,
        engine: Arc<Tjs2Engine>,
    }

    impl TestEnv {
        // `set_context`'s public API is Arc-based, so the tests build the
        // `Arc<Tjs2Engine>` they hand over even though the TJS2 VM is
        // single-threaded (one engine per thread; the tests run with
        // `--test-threads=1`) and the Arc is never shared across threads.
        #[allow(clippy::arc_with_non_send_sync)]
        fn new(tag: &str, files: &[(&str, &[u8])]) -> TestEnv {
            let dir = TempDir::new(tag);
            for (name, bytes) in files {
                let path = dir.path().join(name);
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent).expect("create parent dirs");
                }
                std::fs::write(path, bytes).expect("write fixture file");
            }
            let engine = Arc::new(Tjs2Engine::new().expect("create engine"));
            let storage = Arc::new(Mutex::new(
                Storage::mount(dir.path()).expect("mount storage"),
            ));
            set_context(Some(engine.clone()), Some(storage.clone()));
            register_scripts(&engine).expect("register Scripts");
            TestEnv { _dir: dir, engine }
        }

        fn eval(&self, expression: &str) -> Result<TjsValue, String> {
            self.engine
                .eval(expression, "test")
                .map_err(|e| e.to_string())
        }

        fn eval_ok(&self, expression: &str) -> TjsValue {
            self.eval(expression)
                .unwrap_or_else(|e| panic!("eval {expression:?} failed: {e}"))
        }
    }

    #[test]
    fn exec_storage_runs_script_and_returns_top_level_return() {
        let _vm_lock = vm_lock();
        // A trailing expression statement is NOT the script result (the
        // reference only returns the value of a top-level `return`).
        let env = TestEnv::new(
            "exec-result",
            &[
                (
                    "a.tjs",
                    b"var loadedFromScript = 41; loadedFromScript + 1;".as_slice(),
                ),
                (
                    "b.tjs",
                    b"var returnedFromScript = 41; return returnedFromScript + 1;".as_slice(),
                ),
            ],
        );

        assert_eq!(env.eval_ok("Scripts.execStorage('a.tjs')"), TjsValue::Void);
        // the script still ran and defined the global
        assert_eq!(env.eval_ok("loadedFromScript"), TjsValue::Integer(41));
        // an explicit top-level `return` is the execStorage result
        assert_eq!(
            env.eval_ok("Scripts.execStorage('b.tjs')"),
            TjsValue::Integer(42)
        );
    }

    #[test]
    fn exec_storage_defines_global_visible_to_outer_script() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new(
            "exec-global",
            &[(
                "defs.tjs",
                b"var definedByScript = 7; function addDefined(n) { return definedByScript + n; }"
                    .as_slice(),
            )],
        );

        assert_eq!(
            env.eval_ok("Scripts.execStorage('defs.tjs')"),
            TjsValue::Void
        );
        assert_eq!(env.eval_ok("definedByScript"), TjsValue::Integer(7));
        assert_eq!(env.eval_ok("addDefined(35)"), TjsValue::Integer(42));
    }

    #[test]
    fn eval_storage_returns_expression_value() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new("eval-storage", &[("expr.tjs", b"2 * 21".as_slice())]);

        assert_eq!(
            env.eval_ok("Scripts.evalStorage('expr.tjs')"),
            TjsValue::Integer(42)
        );
        // string results round-trip through the C ABI
        let env = TestEnv::new(
            "eval-storage-str",
            &[("greet.tjs", "'hello' + ' ' + 'storage'".as_bytes())],
        );
        assert_eq!(
            env.eval_ok("Scripts.evalStorage('greet.tjs')"),
            TjsValue::String("hello storage".into())
        );
    }

    #[test]
    fn exec_runs_raw_string() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new("exec-raw", &[]);

        assert_eq!(
            env.eval_ok("Scripts.exec('var rawExec = 40; rawExec += 2;')"),
            TjsValue::Void
        );
        assert_eq!(env.eval_ok("rawExec"), TjsValue::Integer(42));
        // exec with an explicit top-level return
        assert_eq!(
            env.eval_ok("Scripts.exec('return 6 * 7;')"),
            TjsValue::Integer(42)
        );
    }

    #[test]
    fn eval_returns_expression_value() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new("eval-raw", &[]);

        assert_eq!(env.eval_ok("Scripts.eval('6 * 7')"), TjsValue::Integer(42));
        assert_eq!(
            env.eval_ok("Scripts.eval(\"'a' + 'b'\")"),
            TjsValue::String("ab".into())
        );
    }

    #[test]
    fn eval_returns_function_object_usable_from_script() {
        let _vm_lock = vm_lock();
        // The Action.tjs pattern: `var func = Scripts.eval(elm.action);
        // func(this, elm);` — the function must cross the ABI as a usable
        // object, not void.
        let env = TestEnv::new("eval-object", &[]);
        env.engine
            .exec_script("var action = 'function(a, b) { return a * b; }';", "test")
            .unwrap();
        env.eval_ok("Scripts.exec(\"var func = Scripts.eval(action); var result = func(6, 7);\")");
        assert_eq!(env.eval_ok("result"), TjsValue::Integer(42));

        // A named function and a dictionary result are usable too.
        env.engine
            .exec_script("function globalFn(a) { return a + 1; }", "test")
            .unwrap();
        env.eval_ok("Scripts.exec(\"var f = Scripts.eval('globalFn'); var n = f(41);\")");
        assert_eq!(env.eval_ok("n"), TjsValue::Integer(42));

        env.engine
            .exec_script("var globalObj = %[a: 7];", "test")
            .unwrap();
        env.eval_ok("Scripts.exec(\"var o = Scripts.eval('globalObj'); var a = o.a;\")");
        assert_eq!(env.eval_ok("a"), TjsValue::Integer(7));
    }

    #[test]
    fn exec_returns_object_usable_from_script() {
        let _vm_lock = vm_lock();
        // Scripts.exec's top-level `return` can be an object; the caller can
        // read its members (both exec and eval use the retained path).
        let env = TestEnv::new("exec-object", &[]);
        env.engine
            .exec_script(
                "var globalObj = %[a: 1, b: 'x']; var objRef = globalObj;",
                "test",
            )
            .unwrap();
        env.eval_ok(
            "Scripts.exec(\"var r = Scripts.exec('return objRef;'); var a = r.a; var b = r.b;\")",
        );
        assert_eq!(env.eval_ok("a"), TjsValue::Integer(1));
        assert_eq!(env.eval_ok("b"), TjsValue::String("x".into()));
    }

    #[test]
    fn nested_object_eval_does_not_leak_into_scalar_result() {
        let _vm_lock = vm_lock();
        // The outer eval returns 42, but a nested Scripts.eval produced an
        // object on the way. The outer result must stay an integer (the old
        // "retain last_object" hack would have returned the nested object).
        let env = TestEnv::new("eval-stale", &[]);
        env.engine.exec_script("var o = %[a: 1];", "test").unwrap();
        assert_eq!(
            env.eval_ok("Scripts.eval(\"(Scripts.eval('o'), 42)\")"),
            TjsValue::Integer(42)
        );
    }

    #[test]
    fn action_sequence_eval_pattern_runs() {
        let _vm_lock = vm_lock();
        // Exactly Action.tjs:314-315, the failing call:
        //   var func = Scripts.eval(elm.action);
        //   func(this, elm);
        // Before the fix Scripts.eval returned void, so `func(this, elm)`
        // raised "Cannot convert the variable type (() to Object)" and the
        // owning Timer was disabled. The evaluated function must be a real
        // callable object and receive both arguments.
        let env = TestEnv::new("action-pattern", &[]);
        let script = r#"
            var holder = %[];
            var elm = %[action: "function(self, e) { self.count = e.step; return true; }", step: 7];
            var func = Scripts.eval(elm.action);
            func(holder, elm);
        "#;
        env.engine
            .exec_script(script, "Action.tjs")
            .expect("the ActionSequense pattern must run");
        assert_eq!(env.eval_ok("holder.count"), TjsValue::Integer(7));
    }

    #[test]
    fn eval_object_results_do_not_leak_retentions() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new("eval-leak", &[]);
        env.engine.exec_script("var o = %[a: 1];", "test").unwrap();
        for _ in 0..50 {
            env.eval_ok("Scripts.eval('o')");
        }
        // Every object result was consumed by the native return conversion
        // (the C++ trampoline erases the entry), so the map is empty.
        let count = unsafe { tjs2_sys::tjs2_retained_count(env.engine.raw()) };
        assert_eq!(count, 0, "Scripts.eval object results leaked");
        // The engine is still healthy afterwards.
        assert_eq!(env.eval_ok("Scripts.eval('6 * 7')"), TjsValue::Integer(42));
    }

    #[test]
    fn mode_string_is_accepted_and_offset_applied() {
        let _vm_lock = vm_lock();
        // mode "" and "exec" (no `oN` offset) behave like no mode
        let env = TestEnv::new(
            "mode",
            &[
                ("m.tjs", b"return 1;".as_slice()),
                ("off.tjs", b"return 100;".as_slice()),
                ("off10.tjs", b"1234567890return 100;".as_slice()),
            ],
        );
        assert_eq!(
            env.eval_ok("Scripts.execStorage('m.tjs', '')"),
            TjsValue::Integer(1)
        );
        assert_eq!(
            env.eval_ok("Scripts.execStorage('m.tjs', 'exec')"),
            TjsValue::Integer(1)
        );
        // oN: the stream starts at byte N, so `off.tjs` read at offset 0
        // and `off10.tjs` (10 junk bytes + same script) read at offset 10
        // must produce the same script.
        assert_eq!(
            env.eval_ok("Scripts.execStorage('off.tjs', 'o0')"),
            TjsValue::Integer(100)
        );
        assert_eq!(
            env.eval_ok("Scripts.execStorage('off10.tjs', 'o10')"),
            TjsValue::Integer(100)
        );
    }

    #[test]
    fn decode_utf8_bom_and_cp932() {
        let _vm_lock = vm_lock();
        // UTF-8 with BOM — the common scenario-file encoding
        let env = TestEnv::new(
            "utf8-bom",
            &[(
                "u.tjs",
                &[
                    &tvp_util::encoding::UTF8_BOM[..],
                    "var fromBom = '世界';".as_bytes(),
                ]
                .concat(),
            )],
        );
        assert_eq!(env.eval_ok("Scripts.execStorage('u.tjs')"), TjsValue::Void);
        assert_eq!(env.eval_ok("fromBom"), TjsValue::String("世界".into()));

        // CP932 without BOM — Japanese legacy encoding
        let body = tvp_util::encoding::Encoding::Cp932
            .encode("var fromCp932 = 'こんにちは';")
            .expect("encode cp932");
        let env = TestEnv::new("cp932", &[("j.tjs", body.as_slice())]);
        assert_eq!(env.eval_ok("Scripts.execStorage('j.tjs')"), TjsValue::Void);
        assert_eq!(
            env.eval_ok("fromCp932"),
            TjsValue::String("こんにちは".into())
        );
    }

    #[test]
    fn text_encoding_property_controls_the_fallback() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new(
            "text-encoding",
            &[("plain.tjs", b"var tePlain = 1;".as_slice())],
        );
        // default (auto) reports UTF-8 like TVPGetDefaultReadEncoding
        assert_eq!(
            env.eval_ok("Scripts.textEncoding"),
            TjsValue::String("UTF-8".into())
        );
        // an explicitly configured encoding is consulted for BOM-less data
        env.eval_ok("Scripts.textEncoding = 'no-such-encoding'");
        assert_eq!(
            env.eval_ok("Scripts.textEncoding"),
            TjsValue::String("no-such-encoding".into())
        );
        assert!(
            env.eval("Scripts.execStorage('plain.tjs')").is_err(),
            "an invalid configured encoding must surface as a script error"
        );
        // resetting to '' restores automatic detection
        env.eval_ok("Scripts.textEncoding = ''");
        assert_eq!(
            env.eval_ok("Scripts.textEncoding"),
            TjsValue::String("UTF-8".into())
        );
        assert_eq!(
            env.eval_ok("Scripts.execStorage('plain.tjs')"),
            TjsValue::Void
        );
        assert_eq!(env.eval_ok("tePlain"), TjsValue::Integer(1));
    }

    #[test]
    fn missing_storage_and_bad_script_are_catchable() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new(
            "errors",
            &[("broken.tjs", b"this is not valid tjs".as_slice())],
        );

        // missing storage → TJS error, catchable from script
        assert!(env.eval("Scripts.execStorage('nope.tjs')").is_err());
        env.engine
            .exec_script(
                "var caught1 = ''; try { Scripts.execStorage('nope.tjs'); } catch(e) { caught1 = 'missing'; }",
                "test",
            )
            .expect("try/catch around missing storage");
        assert_eq!(env.eval_ok("caught1"), TjsValue::String("missing".into()));

        // script compile error inside the storage → same
        assert!(env.eval("Scripts.execStorage('broken.tjs')").is_err());
        env.engine
            .exec_script(
                "var caught2 = ''; try { Scripts.execStorage('broken.tjs'); } catch(e) { caught2 = 'broken'; }",
                "test",
            )
            .expect("try/catch around broken script");
        assert_eq!(env.eval_ok("caught2"), TjsValue::String("broken".into()));
    }

    #[test]
    fn nested_exec_storage_reenters_vm() {
        let _vm_lock = vm_lock();
        // A script loaded by execStorage loads another one itself (this is
        // how scenario/template chains work).
        let env = TestEnv::new(
            "nested",
            &[
                ("inner.tjs", b"var innerValue = 40; return 2;".as_slice()),
                (
                    "outer.tjs",
                    b"var r = Scripts.execStorage('inner.tjs'); var outerValue = innerValue + r; return outerValue;"
                        .as_slice(),
                ),
            ],
        );

        assert_eq!(
            env.eval_ok("Scripts.execStorage('outer.tjs')"),
            TjsValue::Integer(42)
        );
        assert_eq!(env.eval_ok("innerValue"), TjsValue::Integer(40));
    }

    #[test]
    fn real_and_void_results() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new(
            "types",
            &[
                ("r.tjs", b"return 3.5;".as_slice()),
                ("v.tjs", b"".as_slice()),
            ],
        );
        assert_eq!(
            env.eval_ok("Scripts.execStorage('r.tjs')"),
            TjsValue::Real(3.5)
        );
        // empty script → void
        assert_eq!(env.eval_ok("Scripts.execStorage('v.tjs')"), TjsValue::Void);
    }

    #[test]
    fn exec_storage_object_result_is_retained_and_usable() {
        let _vm_lock = vm_lock();
        // Like evalStorage, an object-returning script (e.g. `(const) [...]`
        // save data) is retained and crosses the ABI as a usable object.
        let env = TestEnv::new(
            "exec-storage-obj",
            &[("save.tjs", b"return %[a: 1, b: 'x'];".as_slice())],
        );
        // The stored script's top-level dict becomes the execStorage result
        // (retained), readable from the caller.
        env.eval_ok(
            "Scripts.exec(\"var r = Scripts.execStorage('save.tjs'); var a = r.a; var b = r.b;\")",
        );
        assert_eq!(env.eval_ok("a"), TjsValue::Integer(1));
        assert_eq!(env.eval_ok("b"), TjsValue::String("x".into()));
        // repeated calls do not corrupt the results (retain reuse)
        for _ in 0..20 {
            env.eval_ok(
                "Scripts.exec(\"var r2 = Scripts.execStorage('save.tjs'); var a2 = r2.a;\")",
            );
            assert_eq!(env.eval_ok("a2"), TjsValue::Integer(1));
        }
    }
}
