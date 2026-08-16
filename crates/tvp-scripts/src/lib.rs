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
//! # Pending (need VM internals; not implemented)
//!
//! - `dump` — reference `TVPDumpScriptEngine`: dumps the VM's compiled
//!   code. The C ABI exposes no dump entry point.
//! - `getTraceString` — reference `TJSGetStackTraceString(limit)`: reads
//!   the VM's current stack trace. Not exposed over the C ABI.
//! - `dumpStringHeap` — reference `TJSDumpStringHeap()` (debug builds
//!   only).
//!
//! # ABI limits
//!
//! The C ABI (`tjs2_value` / `value_to_variant` in `tjs2_abi.cpp`)
//! marshals void / integer / real / string results. Object results cannot
//! cross the boundary yet, so they are returned as `void` with a warning
//! (the reference returns the object itself).

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::io::Read;
use std::ptr;
use std::sync::{Arc, Mutex};

use engine::Storage;
use tjs2_sys::{
    NativeClassBuilder, NativeMethodDef, Tjs2Engine, TjsValue, VAL_INTEGER, VAL_REAL, VAL_STRING,
    VAL_VOID, Value,
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

thread_local! {
    static CONTEXT: RefCell<Context> = const {
        RefCell::new(Context {
            engine: None,
            storage: None,
        })
    };
}

/// Point the `Scripts` class at the running VM and storage.
///
/// The engine calls this before executing `startup.tjs` so that
/// `Scripts.execStorage` can call back into the same VM (this is how
/// k2compat-style scripts load each other). The TJS2 VM is single-threaded,
/// so the context is a thread-local: set it on the thread that owns the
/// engine (the crate tests use `--test-threads=1` for the same reason).
pub fn set_context(engine: Option<Arc<Tjs2Engine>>, storage: Option<Arc<Mutex<Storage>>>) {
    CONTEXT.with(|c| *c.borrow_mut() = Context { engine, storage });
}

/// Clone the engine + storage handles out of the context (the borrow ends
/// before any VM call, so nested `Scripts.*` calls can re-enter freely).
fn context_engine_and_storage() -> Result<(Arc<Tjs2Engine>, Arc<Mutex<Storage>>), String> {
    CONTEXT.with(|c| {
        let c = c.borrow();
        match (&c.engine, &c.storage) {
            (Some(engine), Some(storage)) => Ok((engine.clone(), storage.clone())),
            _ => Err(
                "Scripts context is not set: set_context(engine, storage) must be called \
                 before using Scripts.execStorage/evalStorage"
                    .into(),
            ),
        }
    })
}

/// Clone the engine handle out of the context.
fn context_engine() -> Result<Arc<Tjs2Engine>, String> {
    CONTEXT.with(|c| {
        c.borrow().engine.clone().ok_or_else(|| {
            "Scripts context is not set: set_context(engine, ...) must be called".into()
        })
    })
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
/// expression (`expression == true`).
fn execute_storage(name: &str, mode: &str, expression: bool) -> Result<TjsValue, String> {
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
        engine.eval(&text, name).map_err(|e| e.to_string())
    } else {
        engine.exec_script(&text, name).map_err(|e| e.to_string())
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
        TjsValue::Object => {
            // The C ABI carries no object handle; the reference returns the
            // object, we fall back to void.
            log::warn!("Scripts: object result cannot cross the C ABI yet; returning void");
            set_out_void(out);
        }
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

/// Convert a `TjsValue` produced by the context VM into the C result slot.
fn finish(out: *mut Value, out_error: *mut *mut c_char, result: Result<TjsValue, String>) -> c_int {
    match result {
        Ok(v) => {
            set_out_result(out, v);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// After an eval, if the script produced an OBJECT result (an array or dict
/// literal — `(const) [...]` save data), retain it and return it across the
/// ABI via the retained-value slot. Returns true when handled.
fn try_retain_object_result(out: *mut Value, engine: &tjs2_sys::Tjs2Engine) -> bool {
    match engine.retain_value_detached(&tjs2_sys::TjsValue::Object) {
        Ok(dv) => {
            // SAFETY: `out` is a valid result slot for this call; the C++
            // side copies the retained variant into the result before the
            // callback returns.
            unsafe {
                (*out).ty = tjs2_sys::VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = std::ptr::null();
                (*out).array = std::ptr::null();
                (*out).array_count = 0;
                (*out).retained = dv.raw_id() as usize;
            }
            // The C++ side CONSUMES the retention (copies + erases the map
            // entry) when it converts the result, so the DetachedValue must
            // outlive this callback — leak it (its Drop would release the
            // id before the conversion runs; forgetting skips the release,
            // which is then a safe no-op after the erase).
            std::mem::forget(dv);
            true
        }
        Err(_) => false, // no object result; keep the scalar void
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
    // Object results (e.g. `(const) [...]` save data) are retained and
    // returned across the ABI, exactly like evalStorage.
    if result.is_ok()
        && let Ok(engine) = context_engine()
        && try_retain_object_result(out, &engine)
    {
        return 0;
    }
    finish(out, out_error, result)
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
    if result.is_ok()
        && let Ok(engine) = context_engine()
        && try_retain_object_result(out, &engine)
    {
        return 0;
    }
    finish(out, out_error, result)
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
            .exec_script(&script, &name)
            .map_err(|e| e.to_string()),
        Err(e) => Err(e),
    };
    finish(out, out_error, result)
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
        Ok(engine) => engine.eval(&expression, &name).map_err(|e| e.to_string()),
        Err(e) => Err(e),
    };
    finish(out, out_error, result)
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

/// Register the `Scripts` native class on `engine`'s global object.
pub fn register_scripts(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class(&NativeClassBuilder {
        name: "Scripts",
        properties: vec![],
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
