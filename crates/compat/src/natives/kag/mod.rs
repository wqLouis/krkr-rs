//! `KAGParser` native class and the `.ks` scenario parser.
//!
//! Behavioral reference: `main`'s `crates/tvp-kagparser/src/*` and the pure
//! `crates/kag` parser. The [`parser`] half is a dependency-free transform
//! (no VM, no engine); [`state`] is the pull state machine; [`kvp`] holds
//! the string codecs used by macro storage and `store`/`restore`; this
//! module is the thin native adapter that registers `KAGParser` on the VM
//! and marshals script values.
//!
//! # Object results (improvement over the old native)
//!
//! The reference `getNextTag` could only return a *string encoding* of the
//! tag dictionary because the old ABI had no object result. On the new
//! stack `tjs` can retain an object and the C++ trampoline consumes a
//! `VAL_RETAINED` result, so `getNextTag` now returns a **real TJS
//! Dictionary** (`tagname` plus every attribute plus `taglist`, an array of
//! member names in source order, matching the reference's `ArgValue`). The
//! dictionary is created with `%[]` and populated through
//! `Tjs2Engine::set_member`, so no scenario text is evaluated as TJS — only
//! the `taglist` array literal is built from escaped attribute names.
//! `macros`, `paramMacros` and `macroParams` likewise return real
//! Dictionaries.
//!
//! # No process globals
//!
//! The per-instance parser state lives in the native instance payload
//! (created/destroyed by [`kagparser_create`]/[`kagparser_destroy`]); the
//! only process-wide state touched is the documented engine binding
//! (`crate::binding`) used to resolve the VM and its attached
//! [`engine::Storage`]. The `out.string` scratch buffer is kept in
//! [`CompatServices`] state, not a thread-local.

mod kvp;
mod parser;
mod state;

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;

use engine::Storage;
use tjs::{
    DetachedValue, Engine, NativeInstanceBuilder, NativeInstanceMethodDef,
    NativeInstancePropertyDef, RetainedValue, Tjs2Engine, TjsValue, VAL_INTEGER, VAL_REAL,
    VAL_RETAINED, VAL_STRING, VAL_VOID, Value,
};

use crate::binding;
use crate::native_util::{args, report_error, set_int, set_void};
use crate::{CompatCtx, CompatError, CompatServices};
use state::{Environ, EvalResult, KagParserState, LabelCallback};

// ---------------------------------------------------------------------------
// per-VM scratch buffers
// ---------------------------------------------------------------------------

/// Reusable `out.string` scratch. The C++ trampoline copies the string
/// immediately after the callback returns, so the buffer only has to outlive
/// the callback — keeping it here (rather than leaking a `CString` or using
/// a thread-local) respects the "one process-wide binding" rule.
#[derive(Default)]
struct KagScratch {
    buf: Vec<u8>,
}

// ---------------------------------------------------------------------------
// context / environment
// ---------------------------------------------------------------------------

/// Resolve the bound VM for a callback's raw engine pointer.
fn resolve_host(engine: *mut c_void) -> Result<&'static Tjs2Engine, String> {
    binding::resolve(engine as *mut Engine)
        .map(|resolved| resolved.engine())
        .ok_or_else(|| "KAGParser: compat context is not bound".to_string())
}

/// The L2 storage attached to the bound services, if any.
fn resolve_storage(engine: *mut c_void) -> Result<Option<*mut Storage>, String> {
    let resolved = binding::resolve(engine as *mut Engine)
        .ok_or_else(|| "KAGParser: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    // SAFETY: `resolved` comes from the installed binding, so the services
    // pointer is live for the (single-threaded) callback.
    Ok(unsafe { ctx.services() }.storage())
}

/// Run `f` with the bound services.
fn with_services<R>(
    engine: *mut c_void,
    f: impl FnOnce(&CompatServices) -> R,
) -> Result<R, String> {
    let resolved = binding::resolve(engine as *mut Engine)
        .ok_or_else(|| "KAGParser: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    // SAFETY: `resolved` comes from the installed binding.
    Ok(f(unsafe { ctx.services() }))
}

/// A boxed expression evaluator (`Env.eval`).
type EvalFn = Box<dyn FnMut(&str) -> Result<EvalResult, String>>;
/// A boxed storage loader (`Env.load_storage`).
type LoadFn = Box<dyn FnMut(&str) -> Result<String, String>>;
/// A boxed owner `onLabel(label, page)` callback.
type LabelClosure = Box<dyn FnMut(&str, Option<&str>)>;

/// Reborrow a boxed label closure as a `dyn FnMut` with the borrow's own
/// object lifetime (`Environ<'a>` is invariant over `'a`).
fn shorten_label_closure<'a>(c: &'a mut LabelClosure) -> LabelCallback<'a> {
    &mut **c
}

/// The injectable environment [`KagParserState`] walks in: TJS expression
/// evaluation through the bound VM, scenario loading through the attached
/// [`engine::Storage`], and the optional owner `onLabel` callback.
struct KagEnv {
    eval: EvalFn,
    load: LoadFn,
    label: Option<LabelClosure>,
}

impl KagEnv {
    fn new(engine: &'static Tjs2Engine, storage: Option<*mut Storage>, owner: *mut c_void) -> Self {
        let eval: EvalFn = Box::new(move |exp: &str| match engine.eval(exp, "KAGParser") {
            Ok(TjsValue::Void) => Ok(EvalResult::Void),
            Ok(TjsValue::Integer(i)) => Ok(EvalResult::Integer(i)),
            Ok(TjsValue::Real(r)) => Ok(EvalResult::Real(r)),
            Ok(TjsValue::String(s)) => Ok(EvalResult::Str(s)),
            Ok(TjsValue::Object) | Ok(TjsValue::Retained(_)) => {
                log::warn!("KAGParser: expression {exp:?} returned an object; treating as void");
                Ok(EvalResult::Void)
            }
            Err(e) => Err(e.to_string()),
        });
        let load: LoadFn = Box::new(move |name: &str| {
            let Some(ptr) = storage else {
                return Err("KAGParser: no storage is attached to the compat services".to_string());
            };
            // SAFETY: the host keeps an attached storage alive for the whole
            // VM lifetime (see `CompatServices::attach_storage`).
            let bytes = unsafe { &*ptr }.read(name).map_err(|e| e.to_string())?;
            decode_script(&bytes)
        });
        let label: Option<LabelClosure> = if owner.is_null() {
            None
        } else {
            Some(Box::new(move |label: &str, page: Option<&str>| {
                let Ok(id) = engine.retain_object_detached(owner) else {
                    return;
                };
                let arguments = [
                    TjsValue::String(label.to_string()),
                    page.map(|p| TjsValue::String(p.to_string()))
                        .unwrap_or(TjsValue::Void),
                ];
                // A missing `onLabel` (base KAGParser) or any error the
                // handler raises is ignored, matching the reference.
                let _ = engine.call_member(id.raw_id(), "onLabel", &arguments);
            }))
        };
        Self { eval, load, label }
    }

    fn environ(&mut self) -> Environ<'_> {
        let fire_label = self.label.as_mut().map(shorten_label_closure);
        Environ {
            eval: &mut *self.eval,
            load_storage: &mut *self.load,
            fire_label,
        }
    }
}

/// Build the walk environment for a callback, resolving the VM/storage.
fn make_env(engine: *mut c_void, owner: *mut c_void) -> Result<KagEnv, String> {
    let host = resolve_host(engine)?;
    let storage = resolve_storage(engine)?;
    Ok(KagEnv::new(host, storage, owner))
}

/// Decode scenario bytes the way the reference text stream does: honor a
/// UTF-8/UTF-16 BOM, else UTF-8. CP932 lives outside this layer on the new
/// stack, so a non-UTF-8 file is decoded lossily and logged (D11 keeps the
/// failure visible).
fn decode_script(bytes: &[u8]) -> Result<String, String> {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(rest.to_vec()).map_err(|e| e.to_string());
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16(rest, false);
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => Ok(text.to_string()),
        Err(_) => {
            log::warn!(
                "KAGParser: scenario is not valid UTF-8 (CP932 decoding is not available in \
                 this layer); decoding lossily"
            );
            Ok(String::from_utf8_lossy(bytes).into_owned())
        }
    }
}

fn decode_utf16(bytes: &[u8], little_endian: bool) -> Result<String, String> {
    if !bytes.len().is_multiple_of(2) {
        return Err("KAGParser: odd-length UTF-16 scenario".to_string());
    }
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| {
            if little_endian {
                u16::from_le_bytes([pair[0], pair[1]])
            } else {
                u16::from_be_bytes([pair[0], pair[1]])
            }
        })
        .collect();
    String::from_utf16(&units).map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// result marshalling
// ---------------------------------------------------------------------------

/// Write a NUL-terminated copy of `s` into `out.string`. The scratch buffer
/// is owned by [`CompatServices`] state, so its pointer stays valid until the
/// next string result overwrites it (after the C++ side has copied).
fn set_string_result(services: &CompatServices, out: *mut Value, s: &str) {
    let mut scratch = services.state::<KagScratch>();
    scratch.buf.clear();
    scratch.buf.extend_from_slice(s.as_bytes());
    scratch.buf.push(0);
    if out.is_null() {
        return;
    }
    let ptr = scratch.buf.as_ptr() as *const c_char;
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_STRING;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr;
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Write a retained object into `out` as `VAL_RETAINED`; the C++ trampoline
/// consumes the retention when it copies the result.
fn set_retained_object(out: *mut Value, object: DetachedValue) {
    if !out.is_null() {
        // SAFETY: the trampoline passes a valid out slot for the call.
        unsafe {
            (*out).ty = VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = object.raw_id() as usize;
        }
    }
    // The C++ conversion consumes the id; forgetting the wrapper avoids a
    // redundant (but safe) release.
    std::mem::forget(object);
}

/// Evaluate a small object expression (`%[]`, an array literal) and retain
/// the resulting object.
fn retained_object(engine: &Tjs2Engine, expression: &str) -> Result<DetachedValue, String> {
    match engine
        .eval_retained(expression, "KAGParser")
        .map_err(|e| e.to_string())?
    {
        RetainedValue::Object(object) => Ok(object),
        RetainedValue::Value(value) => Err(format!(
            "KAGParser: expression {expression:?} did not yield an object ({value:?})"
        )),
    }
}

/// Build a TJS array literal from already-decoded strings (escaped).
fn array_literal(items: &[String]) -> String {
    let mut literal = String::from("[");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            literal.push(',');
        }
        literal.push_str(&tjs_escape(item));
    }
    literal.push(']');
    literal
}

/// Escape a string as a TJS double-quoted literal. Control characters use a
/// fixed-width four-digit `\xHHHH` escape so a following hex digit cannot be
/// absorbed.
fn tjs_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\x{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Build a real TJS Dictionary from ordered `(key, value)` entries.
fn retain_dict(engine: &Tjs2Engine, entries: &[(String, String)]) -> Result<DetachedValue, String> {
    let dict = retained_object(engine, "%[]")?;
    for (key, value) in entries {
        engine.set_member(dict.raw_id(), key, &TjsValue::String(value.clone()))?;
    }
    Ok(dict)
}

/// Build a tag dictionary: `tagname`, every attribute in source order, and
/// `taglist` (the member names in insertion order).
fn set_tag_result(
    engine: &Tjs2Engine,
    out: *mut Value,
    name: &str,
    params: &[(String, String)],
) -> Result<(), String> {
    let dict = retained_object(engine, "%[]")?;
    engine.set_member(
        dict.raw_id(),
        "tagname",
        &TjsValue::String(name.to_string()),
    )?;
    for (key, value) in params {
        engine.set_member(dict.raw_id(), key, &TjsValue::String(value.clone()))?;
    }
    let mut names: Vec<String> = Vec::with_capacity(params.len() + 1);
    names.push("tagname".to_string());
    names.extend(params.iter().map(|(key, _)| key.clone()));
    let taglist = retained_object(engine, &array_literal(&names))?;
    engine.set_member(
        dict.raw_id(),
        "taglist",
        &TjsValue::Retained(taglist.raw_id() as u64),
    )?;
    set_retained_object(out, dict);
    Ok(())
}

/// Build the `paramMacros` dictionary: macro name → array of alternating
/// parameter names and values.
fn set_param_macros_result(
    engine: &Tjs2Engine,
    out: *mut Value,
    entries: &[(String, Vec<(String, String)>)],
) -> Result<(), String> {
    let dict = retained_object(engine, "%[]")?;
    for (name, list) in entries {
        let mut items: Vec<String> = Vec::with_capacity(list.len() * 2);
        for (key, value) in list {
            items.push(key.clone());
            items.push(value.clone());
        }
        let array = retained_object(engine, &array_literal(&items))?;
        engine.set_member(
            dict.raw_id(),
            name,
            &TjsValue::Retained(array.raw_id() as u64),
        )?;
    }
    set_retained_object(out, dict);
    Ok(())
}

// ---------------------------------------------------------------------------
// argument conversion
// ---------------------------------------------------------------------------

/// Convert a native argument to a string the way `ttstr x = *param[i]` does.
fn param_to_string(v: &Value) -> Result<String, String> {
    match v.ty {
        VAL_STRING => {
            if v.string.is_null() {
                Ok(String::new())
            } else {
                // SAFETY: argv strings are NUL-terminated UTF-8 for the call.
                Ok(unsafe { CStr::from_ptr(v.string) }
                    .to_string_lossy()
                    .into_owned())
            }
        }
        VAL_INTEGER => Ok(v.integer.to_string()),
        VAL_REAL => Ok(v.real.to_string()),
        VAL_VOID => Ok(String::new()),
        _ => Err("KAGParser: argument cannot be converted to a string".to_string()),
    }
}

/// Convert a native argument to a boolean like `param->operator bool()`.
fn param_to_bool(v: &Value) -> Result<bool, String> {
    match v.ty {
        VAL_INTEGER => Ok(v.integer != 0),
        VAL_REAL => Ok(v.real != 0.0),
        VAL_STRING => {
            if v.string.is_null() {
                Ok(false)
            } else {
                // SAFETY: argv strings are NUL-terminated UTF-8 for the call.
                Ok(!unsafe { CStr::from_ptr(v.string) }.to_bytes().is_empty())
            }
        }
        VAL_VOID => Ok(false),
        _ => Err("KAGParser: argument cannot be converted to a boolean".to_string()),
    }
}

// ---------------------------------------------------------------------------
// instance payload
// ---------------------------------------------------------------------------

extern "C" fn kagparser_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(KagParserState::default())) as *mut c_void
}

extern "C" fn kagparser_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: instance came from kagparser_create (Box::into_raw) and is
        // released exactly once by the VM.
        drop(unsafe { Box::from_raw(instance as *mut KagParserState) });
    }
}

/// The parser payload for the object a method was called on.
///
/// # Safety
/// `instance` must be the live payload the VM passed to the callback.
unsafe fn state_of(instance: *mut c_void) -> &'static mut KagParserState {
    // SAFETY: the dispatcher verified the object is a KAGParser instance.
    unsafe { &mut *(instance as *mut KagParserState) }
}

/// `KAGParser()` — constructor member (empty; the payload already exists for
/// `new KAGParser()` and is created for script subclasses).
extern "C" fn kagparser_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// scenario / walk
// ---------------------------------------------------------------------------

/// `loadScenario(storageName)`.
extern "C" fn native_load_scenario(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "KAGParser.loadScenario requires 1 argument");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let name = match param_to_string(&unsafe { args(argv, argc) }[0]) {
        Ok(name) => name,
        Err(e) => return report_error(out_error, &e),
    };
    let mut env = match make_env(engine, ptr::null_mut()) {
        Ok(env) => env,
        Err(e) => return report_error(out_error, &e),
    };
    let state = unsafe { state_of(instance) };
    log::info!("KAGParser: scenario loaded : {name}");
    match state.load_scenario(&name, &mut env.environ()) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
}

/// `getNextTag()` — the next tag as a real Dictionary, or void at end of
/// scenario.
extern "C" fn native_get_next_tag(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let host = match resolve_host(engine) {
        Ok(host) => host,
        Err(e) => return report_error(out_error, &e),
    };
    let mut env = match make_env(engine, objthis) {
        Ok(env) => env,
        Err(e) => return report_error(out_error, &e),
    };
    let state = unsafe { state_of(instance) };
    match state.next_tag(&mut env.environ()) {
        Ok(Some((name, params))) => match set_tag_result(host, out, &name, &params) {
            Ok(()) => 0,
            Err(e) => report_error(out_error, &e),
        },
        Ok(None) => {
            set_void(out);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
}

/// `goToLabel(label)`.
extern "C" fn native_go_to_label(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "KAGParser.goToLabel requires 1 argument");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let name = match param_to_string(&unsafe { args(argv, argc) }[0]) {
        Ok(name) => name,
        Err(e) => return report_error(out_error, &e),
    };
    match unsafe { state_of(instance) }.go_to_label(&name) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
}

/// `callLabel(label)`.
extern "C" fn native_call_label(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "KAGParser.callLabel requires 1 argument");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let name = match param_to_string(&unsafe { args(argv, argc) }[0]) {
        Ok(name) => name,
        Err(e) => return report_error(out_error, &e),
    };
    let mut env = match make_env(engine, ptr::null_mut()) {
        Ok(env) => env,
        Err(e) => return report_error(out_error, &e),
    };
    match unsafe { state_of(instance) }.call_label(&name, &mut env.environ()) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
}

/// `assign(storeString)` — copy state from a `store()` string.
extern "C" fn native_assign(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    native_restore(
        engine,
        instance,
        argc,
        argv,
        out,
        out_error,
        ptr::null_mut(),
    )
}

/// `clear()`.
extern "C" fn native_clear(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    unsafe { state_of(instance) }.clear();
    set_void(out);
    0
}

/// `store()` — serialize the parser state as a string.
extern "C" fn native_store(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let s = unsafe { state_of(instance) }.store();
    match with_services(engine, |services| set_string_result(services, out, &s)) {
        Ok(()) => 0,
        Err(e) => report_error(out_error, &e),
    }
}

/// `restore(storeString)`.
extern "C" fn native_restore(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "KAGParser.restore requires 1 argument");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let s = match param_to_string(&unsafe { args(argv, argc) }[0]) {
        Ok(s) => s,
        Err(e) => return report_error(out_error, &e),
    };
    let mut env = match make_env(engine, ptr::null_mut()) {
        Ok(env) => env,
        Err(e) => return report_error(out_error, &e),
    };
    match unsafe { state_of(instance) }.restore(&s, &mut env.environ()) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
}

/// `clearCallStack()`.
extern "C" fn native_clear_call_stack(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    unsafe { state_of(instance) }.clear_call_stack();
    set_void(out);
    0
}

/// `popMacroArgs()`.
extern "C" fn native_pop_macro_args(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match unsafe { state_of(instance) }.pop_macro_args_public() {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
}

/// `interrupt()`.
extern "C" fn native_interrupt(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    unsafe { state_of(instance) }.interrupt();
    set_void(out);
    0
}

/// `resetInterrupt()`.
extern "C" fn native_reset_interrupt(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    unsafe { state_of(instance) }.reset_interrupt();
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// property-style accessors (methods)
// ---------------------------------------------------------------------------

macro_rules! get_int_method {
    ($name:ident, $method:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            _argc: c_int,
            _argv: *const Value,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_int(out, unsafe { state_of(instance) }.$method() as i64);
            0
        }
    };
}

macro_rules! get_string_method {
    ($name:ident, $method:ident) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            _argc: c_int,
            _argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let s = unsafe { state_of(instance) }.$method().to_string();
            match with_services(engine, |services| set_string_result(services, out, &s)) {
                Ok(()) => 0,
                Err(e) => report_error(out_error, &e),
            }
        }
    };
}

macro_rules! get_bool_method {
    ($name:ident, $method:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            _argc: c_int,
            _argv: *const Value,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_int(out, i64::from(unsafe { state_of(instance) }.$method()));
            0
        }
    };
}

macro_rules! set_bool_method {
    ($name:ident, $method:ident, $msg:literal) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            argc: c_int,
            argv: *const Value,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            if argc < 1 {
                return report_error(out_error, $msg);
            }
            // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
            match param_to_bool(&unsafe { args(argv, argc) }[0]) {
                Ok(v) => {
                    unsafe { state_of(instance) }.$method(v);
                    set_void(out);
                    0
                }
                Err(e) => report_error(out_error, &e),
            }
        }
    };
}

macro_rules! prop_get_int {
    ($name:ident, $method:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_int(out, unsafe { state_of(instance) }.$method() as i64);
            0
        }
    };
}

macro_rules! prop_get_bool {
    ($name:ident, $method:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_int(out, i64::from(unsafe { state_of(instance) }.$method()));
            0
        }
    };
}

macro_rules! prop_set_bool {
    ($name:ident, $method:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: value follows the trampoline contract.
            match param_to_bool(unsafe { &*value }) {
                Ok(v) => {
                    unsafe { state_of(instance) }.$method(v);
                    0
                }
                Err(e) => report_error(out_error, &e),
            }
        }
    };
}

macro_rules! prop_get_string {
    ($name:ident, $method:ident) => {
        extern "C" fn $name(
            engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            let s = unsafe { state_of(instance) }.$method().to_string();
            match with_services(engine, |services| set_string_result(services, out, &s)) {
                Ok(()) => 0,
                Err(e) => report_error(out_error, &e),
            }
        }
    };
}

get_int_method!(native_get_cur_line, get_cur_line);
get_int_method!(native_get_cur_pos, get_cur_pos);
get_int_method!(native_get_call_stack_depth, get_call_stack_depth);
get_string_method!(native_get_cur_line_str, get_cur_line_str);
get_string_method!(native_get_cur_storage, get_storage_name);
get_string_method!(native_get_cur_label, get_cur_label);
get_bool_method!(native_get_ignore_cr, get_ignore_cr);
get_bool_method!(native_get_process_special_tags, get_process_special_tags);
get_bool_method!(
    native_get_multi_line_tag_enabled,
    get_multi_line_tag_enabled
);

set_bool_method!(
    native_set_ignore_cr,
    set_ignore_cr,
    "KAGParser.setIgnoreCR requires 1 argument"
);
set_bool_method!(
    native_set_process_special_tags,
    set_process_special_tags,
    "KAGParser.setProcessSpecialTags requires 1 argument"
);
set_bool_method!(
    native_set_multi_line_tag_enabled,
    set_multi_line_tag_enabled,
    "KAGParser.setMultiLineTagEnabled requires 1 argument"
);

extern "C" fn native_get_debug_level(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_int(out, unsafe { state_of(instance) }.get_debug_level() as i64);
    0
}

extern "C" fn native_set_debug_level(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "KAGParser.setDebugLevel requires 1 argument");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let level = match param_to_string(&unsafe { args(argv, argc) }[0])
        .and_then(|s| s.parse::<i32>().map_err(|e| e.to_string()))
    {
        Ok(level) => level,
        Err(e) => return report_error(out_error, &e),
    };
    unsafe { state_of(instance) }.set_debug_level(level);
    set_void(out);
    0
}

/// `getMacros()` — a real Dictionary.
extern "C" fn native_get_macros(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let host = match resolve_host(engine) {
        Ok(host) => host,
        Err(e) => return report_error(out_error, &e),
    };
    let entries = unsafe { state_of(instance) }.get_macros_entries();
    match retain_dict(host, &entries) {
        Ok(dict) => {
            set_retained_object(out, dict);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
}

/// `setMacros(kvpString)`.
extern "C" fn native_set_macros(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "KAGParser.setMacros requires 1 argument");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let s = match param_to_string(&unsafe { args(argv, argc) }[0]) {
        Ok(s) => s,
        Err(e) => return report_error(out_error, &e),
    };
    match unsafe { state_of(instance) }.set_macros(&s) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
}

/// Decode the string form of a `paramMacros` dictionary: one
/// `macroName=name1\x1fvalue1...` line per macro.
fn decode_param_macros(s: &str) -> Vec<(String, Vec<(String, String)>)> {
    let mut out = Vec::new();
    for line in s.split('\n') {
        if line.is_empty() {
            continue;
        }
        let Some(eq) = line.find('=') else { continue };
        let name = kvp::unescape_field(&line[..eq]);
        let value = kvp::unescape_field(&line[eq + 1..]);
        let fields: Vec<&str> = if value.is_empty() {
            Vec::new()
        } else {
            value.split('\u{1f}').collect()
        };
        let list = fields
            .chunks(2)
            .map(|pair| {
                (
                    pair[0].to_string(),
                    pair.get(1).copied().unwrap_or("").to_string(),
                )
            })
            .collect();
        out.push((name, list));
    }
    out
}

/// `getParamMacros()` — a real Dictionary of Arrays.
extern "C" fn native_get_param_macros(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let host = match resolve_host(engine) {
        Ok(host) => host,
        Err(e) => return report_error(out_error, &e),
    };
    let entries = unsafe { state_of(instance) }.get_param_macros_entries();
    match set_param_macros_result(host, out, &entries) {
        Ok(()) => 0,
        Err(e) => report_error(out_error, &e),
    }
}

/// `setParamMacros(kvpString)`.
extern "C" fn native_set_param_macros(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "KAGParser.setParamMacros requires 1 argument");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let s = match param_to_string(&unsafe { args(argv, argc) }[0]) {
        Ok(s) => s,
        Err(e) => return report_error(out_error, &e),
    };
    unsafe { state_of(instance) }.set_param_macros(decode_param_macros(&s));
    set_void(out);
    0
}

/// `getMacroParams()` — the top macro-args dictionary, or void.
extern "C" fn native_get_macro_params(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let host = match resolve_host(engine) {
        Ok(host) => host,
        Err(e) => return report_error(out_error, &e),
    };
    match unsafe { state_of(instance) }.get_macro_params_entries() {
        Some(entries) => match retain_dict(host, &entries) {
            Ok(dict) => {
                set_retained_object(out, dict);
                0
            }
            Err(e) => report_error(out_error, &e),
        },
        None => {
            set_void(out);
            0
        }
    }
}

/// `getMP()` — alias of `getMacroParams`.
extern "C" fn native_get_mp(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    native_get_macro_params(
        engine,
        instance,
        0,
        ptr::null(),
        out,
        out_error,
        ptr::null_mut(),
    )
}

/// `setCurStorage(name)` — like `loadScenario`.
extern "C" fn native_set_cur_storage(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    native_load_scenario(
        engine,
        instance,
        argc,
        argv,
        out,
        out_error,
        ptr::null_mut(),
    )
}

// ---------------------------------------------------------------------------
// native properties
// ---------------------------------------------------------------------------

prop_get_bool!(prop_ignore_cr_get, get_ignore_cr);
prop_set_bool!(prop_ignore_cr_set, set_ignore_cr);
prop_get_bool!(prop_process_special_tags_get, get_process_special_tags);
prop_set_bool!(prop_process_special_tags_set, set_process_special_tags);
prop_get_bool!(prop_multi_line_tag_enabled_get, get_multi_line_tag_enabled);
prop_set_bool!(prop_multi_line_tag_enabled_set, set_multi_line_tag_enabled);
prop_get_int!(prop_debug_level_get, get_debug_level);
prop_get_int!(prop_cur_line_get, get_cur_line);
prop_get_int!(prop_cur_pos_get, get_cur_pos);
prop_get_int!(prop_call_stack_depth_get, get_call_stack_depth);
prop_get_string!(prop_cur_line_str_get, get_cur_line_str);
prop_get_string!(prop_cur_storage_get, get_storage_name);
prop_get_string!(prop_cur_label_get, get_cur_label);

extern "C" fn prop_debug_level_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract.
    let level = match param_to_string(unsafe { &*value })
        .and_then(|s| s.parse::<i32>().map_err(|e| e.to_string()))
    {
        Ok(level) => level,
        Err(e) => return report_error(out_error, &e),
    };
    unsafe { state_of(instance) }.set_debug_level(level);
    0
}

extern "C" fn prop_cur_storage_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract.
    let name = match param_to_string(unsafe { &*value }) {
        Ok(name) => name,
        Err(e) => return report_error(out_error, &e),
    };
    let mut env = match make_env(engine, ptr::null_mut()) {
        Ok(env) => env,
        Err(e) => return report_error(out_error, &e),
    };
    match unsafe { state_of(instance) }.load_scenario(&name, &mut env.environ()) {
        Ok(()) => 0,
        Err(e) => report_error(out_error, &e),
    }
}

extern "C" fn prop_macros_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    native_get_macros(
        engine,
        instance,
        0,
        ptr::null(),
        out,
        out_error,
        ptr::null_mut(),
    )
}

extern "C" fn prop_macros_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract.
    let s = match param_to_string(unsafe { &*value }) {
        Ok(s) => s,
        Err(e) => return report_error(out_error, &e),
    };
    log::warn!(
        "KAGParser: macros property is read-only in the reference \
         (assign incontextof macros); treating `p.macros = ...` as setMacros"
    );
    match unsafe { state_of(instance) }.set_macros(&s) {
        Ok(()) => 0,
        Err(e) => report_error(out_error, &e),
    }
}

extern "C" fn prop_param_macros_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    native_get_param_macros(
        engine,
        instance,
        0,
        ptr::null(),
        out,
        out_error,
        ptr::null_mut(),
    )
}

extern "C" fn prop_param_macros_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract.
    let s = match param_to_string(unsafe { &*value }) {
        Ok(s) => s,
        Err(e) => return report_error(out_error, &e),
    };
    unsafe { state_of(instance) }.set_param_macros(decode_param_macros(&s));
    0
}

extern "C" fn prop_macro_params_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    native_get_macro_params(
        engine,
        instance,
        0,
        ptr::null(),
        out,
        out_error,
        ptr::null_mut(),
    )
}

// ---------------------------------------------------------------------------
// registration
// ---------------------------------------------------------------------------

/// Register the `KAGParser` native instance class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "KAGParser",
        create: kagparser_create,
        destroy: kagparser_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "KAGParser",
                f: kagparser_ctor,
            },
            NativeInstanceMethodDef {
                name: "loadScenario",
                f: native_load_scenario,
            },
            NativeInstanceMethodDef {
                name: "goToLabel",
                f: native_go_to_label,
            },
            NativeInstanceMethodDef {
                name: "callLabel",
                f: native_call_label,
            },
            NativeInstanceMethodDef {
                name: "getNextTag",
                f: native_get_next_tag,
            },
            NativeInstanceMethodDef {
                name: "assign",
                f: native_assign,
            },
            NativeInstanceMethodDef {
                name: "clear",
                f: native_clear,
            },
            NativeInstanceMethodDef {
                name: "store",
                f: native_store,
            },
            NativeInstanceMethodDef {
                name: "restore",
                f: native_restore,
            },
            NativeInstanceMethodDef {
                name: "clearCallStack",
                f: native_clear_call_stack,
            },
            NativeInstanceMethodDef {
                name: "popMacroArgs",
                f: native_pop_macro_args,
            },
            NativeInstanceMethodDef {
                name: "interrupt",
                f: native_interrupt,
            },
            NativeInstanceMethodDef {
                name: "resetInterrupt",
                f: native_reset_interrupt,
            },
            NativeInstanceMethodDef {
                name: "getCurLine",
                f: native_get_cur_line,
            },
            NativeInstanceMethodDef {
                name: "getCurPos",
                f: native_get_cur_pos,
            },
            NativeInstanceMethodDef {
                name: "getCurLineStr",
                f: native_get_cur_line_str,
            },
            NativeInstanceMethodDef {
                name: "getIgnoreCR",
                f: native_get_ignore_cr,
            },
            NativeInstanceMethodDef {
                name: "setIgnoreCR",
                f: native_set_ignore_cr,
            },
            NativeInstanceMethodDef {
                name: "getProcessSpecialTags",
                f: native_get_process_special_tags,
            },
            NativeInstanceMethodDef {
                name: "setProcessSpecialTags",
                f: native_set_process_special_tags,
            },
            NativeInstanceMethodDef {
                name: "getMultiLineTagEnabled",
                f: native_get_multi_line_tag_enabled,
            },
            NativeInstanceMethodDef {
                name: "setMultiLineTagEnabled",
                f: native_set_multi_line_tag_enabled,
            },
            NativeInstanceMethodDef {
                name: "getDebugLevel",
                f: native_get_debug_level,
            },
            NativeInstanceMethodDef {
                name: "setDebugLevel",
                f: native_set_debug_level,
            },
            NativeInstanceMethodDef {
                name: "getMacros",
                f: native_get_macros,
            },
            NativeInstanceMethodDef {
                name: "setMacros",
                f: native_set_macros,
            },
            NativeInstanceMethodDef {
                name: "getParamMacros",
                f: native_get_param_macros,
            },
            NativeInstanceMethodDef {
                name: "setParamMacros",
                f: native_set_param_macros,
            },
            NativeInstanceMethodDef {
                name: "getMacroParams",
                f: native_get_macro_params,
            },
            NativeInstanceMethodDef {
                name: "getMP",
                f: native_get_mp,
            },
            NativeInstanceMethodDef {
                name: "getCallStackDepth",
                f: native_get_call_stack_depth,
            },
            NativeInstanceMethodDef {
                name: "getCurStorage",
                f: native_get_cur_storage,
            },
            NativeInstanceMethodDef {
                name: "setCurStorage",
                f: native_set_cur_storage,
            },
            NativeInstanceMethodDef {
                name: "getCurLabel",
                f: native_get_cur_label,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "ignoreCR",
                get: Some(prop_ignore_cr_get),
                set: Some(prop_ignore_cr_set),
            },
            NativeInstancePropertyDef {
                name: "processSpecialTags",
                get: Some(prop_process_special_tags_get),
                set: Some(prop_process_special_tags_set),
            },
            NativeInstancePropertyDef {
                name: "multiLineTagEnabled",
                get: Some(prop_multi_line_tag_enabled_get),
                set: Some(prop_multi_line_tag_enabled_set),
            },
            NativeInstancePropertyDef {
                name: "debugLevel",
                get: Some(prop_debug_level_get),
                set: Some(prop_debug_level_set),
            },
            NativeInstancePropertyDef {
                name: "curLine",
                get: Some(prop_cur_line_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "curPos",
                get: Some(prop_cur_pos_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "curLineStr",
                get: Some(prop_cur_line_str_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "callStackDepth",
                get: Some(prop_call_stack_depth_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "curStorage",
                get: Some(prop_cur_storage_get),
                set: Some(prop_cur_storage_set),
            },
            NativeInstancePropertyDef {
                name: "curLabel",
                get: Some(prop_cur_label_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "macros",
                get: Some(prop_macros_get),
                set: Some(prop_macros_set),
            },
            NativeInstancePropertyDef {
                name: "paramMacros",
                get: Some(prop_param_macros_get),
                set: Some(prop_param_macros_set),
            },
            NativeInstancePropertyDef {
                name: "macroParams",
                get: Some(prop_macro_params_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "mp",
                get: Some(prop_macro_params_get),
                set: None,
            },
        ],
    })
}
