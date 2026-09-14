//! The `KAGParser` TJS native instance class for krkr-rs — the engine
//! behind KAG scenario (`.ks`) playback (milestone 3C: ADV text scenes).
//!
//! The native is a stateful wrapper around the workspace `kag` crate's
//! `.ks` parser: `loadScenario` reads a scenario file through
//! [`engine::Storage`] (decoding BOM/UTF-8/CP932/UTF-16 like the reference
//! text stream), and `getNextTag` walks the parsed events one tag at a
//! time with the reference `tTJSNI_KAGParser::_GetNextTag` semantics
//! (per-character `ch` tags, `r eol=true` line ends, `if`/`endif`
//! conditionals, `jump`/`call`/`return`, macro recording/expansion, the
//! interrupt flag, and store/restore).
//!
//! # How the game consumes the native
//!
//! The reference game (data.xp3) does **not** ship KAG3's
//! `system/kag/kag.tjs`/`kagParser.tjs`; it links the `KAGParserEx.dll`
//! plugin (`system/initialize.tjs`) and drives the native directly from
//! two script classes:
//!
//! * `system/sccontroller.tjs` — `class ScController extends KAGParser`
//!   with `super.KAGParser()`, `ignoreCR = true`,
//!   `processSpecialTags = true`, then `getNextTag()` in a callback loop;
//!   `tag === void` means end of scenario; `elm.tagname` selects the tag
//!   handler; `loadScenario`, `goToLabel("*"+label)`, `clear()` and the
//!   `macros` dictionary are used for macro files.
//! * `system/animationsequence.tjs` — `class AnimationSequenceController
//!   extends KAGParser`, same consumption (`getNextTag`, `loadScenario`,
//!   `goToLabel`, `clear`).
//!
//! The game's scenario files confirm the feature set: `scenario/01_01.ks`
//! uses `@if exp="ChkGlobalFlagOn(1)"`/`@endif` (8 pairs), text lines and
//! `@`-tags; `scenario/macro.ks` defines 165 macros with `%arg|default`
//! syntax; `scenario/movie.ks` uses `@jump target=*end`.
//!
//! # The getNextTag return-shape decision
//!
//! The reference `getNextTag` returns a **dictionary** (`tagname` plus the
//! tag's attributes), and the game reads `elm.tagname`/`elm.storage` from
//! it. The tjs2-sys C ABI, however, only marshals void/int/real/string
//! results — objects cannot cross the boundary (and a future ABI
//! extension is out of scope for this crate). **Decision: `getNextTag`
//! returns a STRING encoding of the tag dictionary**, documented here and
//! parsed by a script-side wrapper:
//!
//! ```text
//! <tagname>
//! <attr1>=<value1>
//! <attr2>=<value2>
//! ...
//! ```
//!
//! The first line is the tag name (the dictionary's `tagname` member);
//! each following line is one attribute in source order. Fields are
//! escaped with `` `\` `` → `` `\\` ``, newline → `` `\n` ``, CR →
//! `` `\r` `` (scenario tag content never contains raw newlines, but the
//! same codec is reused for multi-line macro bodies). End of scenario is
//! **void**, matching the game's `tag === void` check; an interrupt
//! returns the tag `interrupt`. [`shim::INSTALL_WRAPPER`] provides the
//! TJS decoder (`parseKAGTag`) and a `KAGParserCompat` class restoring the
//! dictionary-returning, property-style surface.
//!
//! The same string encoding is used for the other object-shaped results:
//! [`macros`](Self::getMacros) and [`macroParams`](Self::getMacroParams)
//! (dictionary → `key=value` lines) and [`store`](Self::store) (the full
//! parser state, see [`state::KagParserState::store`]).
//!
//! # ABI limitations (what is a method, not a property)
//!
//! `tjs2_register_native_class_instance` registers methods only — the C
//! ABI has no instance-property hook. The reference's properties
//! (`ignoreCR`, `processSpecialTags`, `curLine`, `curPos`, `curLineStr`,
//! `debugLevel`, `macros`, `macroParams`, `mp`, `callStackDepth`,
//! `curStorage`, `curLabel`) are therefore exposed as `getX`/`setX`
//! methods; the script-side wrapper translates them back into TJS
//! properties. Also, objects cannot be *passed in* either: `assign` (a
//! reference parameter) cannot receive another parser object and is
//! implemented as a `store`-string copy; `restore` takes the `store`
//! string.
//!
//! The reference fires owner-object events (`onScenarioLoad`,
//! `onScenarioLoaded`, `onLabel`, `onScript`, `onJump`, `onCall`,
//! `onReturn`, `onAfterReturn`) — the ABI carries no handle to the owning
//! TJS object, so these are not fired. Jumps/calls/returns always
//! process (no veto); `[iscript]` blocks are returned as ordinary tags.
//! `if`/`elsif`/`ignore` conditions, `emb` expressions, `&entity` values
//! and `cond` attributes are evaluated by calling back into the VM
//! through the context engine (`Scripts.exec`-style re-entrancy).
//! `class X extends KAGParser` + `super.KAGParser()` cannot construct the
//! native instance with the current ABI (no constructor member); use the
//! wrapper class instead.
//!
//! KAGParserEx (`KAGParserEx.dll`) is **merged into the core `KAGParser`** in
//! this reference: `reference/cpp/plugins/KAGParser/kagparserex.cpp` is a
//! 225-byte placeholder whose comment says it was built into core. Its
//! documented extensions (plugin `readme.txt`) are `multiLineTagEnabled`,
//! parameter-macro expansion (`macroParams`/`mp`, `@pmacro`/`@erasepmacro`)
//! and `emb`'s `escape` parameter — all implemented here. Both games set
//! `multiLineTagEnabled` (`system/animationsequence.tjs`) and
//! `processSpecialTags` (`system/sccontroller.tjs`).
//!
//! Names such as `getRawTag`/`getRawTagCount`/`getTag`/`isTagAvailable`/
//! `getParameter` belong to a *different* KAGParserEx variant: they are absent
//! from this reference (both core and plugin) and unused by the games, so they
//! are deliberately not registered (calling them correctly raises
//! "Member does not exist").

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::sync::{Arc, Mutex};

use engine::Storage;
use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine,
    TjsValue, VAL_INTEGER, VAL_REAL, VAL_STRING, VAL_VOID, Value,
};
use tvp_util::encoding;

pub mod kvp;
pub mod shim;
pub mod state;

use state::{Environ, EvalResult, KagParserState, LabelCallback};

// ---------------------------------------------------------------------------
// context
// ---------------------------------------------------------------------------

/// Engine-side context: the running VM (for expression evaluation inside
/// `getNextTag`) and the mounted game storage (for scenario loading).
#[derive(Default)]
struct Context {
    engine: Option<Arc<Tjs2Engine>>,
    storage: Option<Arc<Mutex<Storage>>>,
}

/// The VM runs on a Bevy worker thread while `set_context` is called on the
/// main thread during startup, so this must be process-global (a thread-local
/// would be empty on the worker). The TJS VM is single-threaded and
/// serialized by the render crate's `VM_RUN_LOCK`, so a `Mutex` is safe.
static CONTEXT: Mutex<Context> = Mutex::new(Context {
    engine: None,
    storage: None,
});

/// Point the `KAGParser` native at the running VM and storage (mirrors
/// `tvp_scripts::set_context`; call before executing `startup.tjs`).
pub fn set_context(engine: Option<Arc<Tjs2Engine>>, storage: Option<Arc<Mutex<Storage>>>) {
    *CONTEXT.lock().unwrap_or_else(|p| p.into_inner()) = Context { engine, storage };
}

fn context_engine() -> Result<Arc<Tjs2Engine>, String> {
    let ctx = CONTEXT.lock().unwrap_or_else(|p| p.into_inner());
    ctx.engine.clone().ok_or_else(|| {
        "KAGParser context is not set: set_context(engine, storage) must be called".into()
    })
}

fn context_storage() -> Result<Arc<Mutex<Storage>>, String> {
    let ctx = CONTEXT.lock().unwrap_or_else(|p| p.into_inner());
    ctx.storage.clone().ok_or_else(|| {
        "KAGParser context is not set: set_context(engine, storage) must be called".into()
    })
}

/// Decode scenario bytes the way the reference text stream does: honor a
/// leading BOM, else UTF-8, else CP932.
fn decode_script(bytes: &[u8]) -> Result<String, String> {
    let (body, enc) = encoding::strip_bom(bytes);
    match enc {
        Some(enc) => enc.decode(body).map_err(|e| e.to_string()),
        None => match String::from_utf8(body.to_vec()) {
            Ok(text) => Ok(text),
            Err(_) => encoding::decode(body, "cp932").map_err(|e| e.to_string()),
        },
    }
}

// ---------------------------------------------------------------------------
// result marshaling (same helpers as tvp-scripts)
// ---------------------------------------------------------------------------

thread_local! {
    static STRING_OUT: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

/// Build a malloc'd NUL-terminated UTF-8 error message for `*out_error`.
fn alloc_error_string(msg: &str) -> *mut c_char {
    let bytes = msg.as_bytes();
    // SAFETY: tjs2_malloc is malloc-compatible; the C++ side owns the copy
    // and frees it with tjs2_free_string.
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

fn error_out(out_error: *mut *mut c_char, msg: &str) -> c_int {
    if !out_error.is_null() {
        // SAFETY: out_error points at a char* slot owned by the C++
        // trampoline for the duration of the call.
        unsafe { *out_error = alloc_error_string(msg) };
    }
    1
}

fn set_string_result(out: *mut Value, s: &str) {
    STRING_OUT.with(|buf| {
        let mut buf = buf.borrow_mut();
        buf.clear();
        buf.extend_from_slice(s.as_bytes());
        buf.push(0);
        // SAFETY: `out` is a valid result slot for the duration of the call.
        unsafe {
            (*out).ty = VAL_STRING;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = buf.as_ptr() as *const c_char;
        }
    });
}

/// Escape a string for inclusion in a TJS `"..."` string literal so it
/// can be embedded verbatim in a `%[...]` dictionary literal.
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
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Build a TJS expression that constructs a dictionary with the given
/// entries, in source order.
///
/// This TJS2 build rejects **quoted keys** in `%[...]` dictionary literals
/// (verified: `%["a": 1]` is a syntax error, `%[a: 1]` works), so the
/// expression assigns each entry imperatively instead — bracket assignment
/// accepts arbitrary string keys and preserves the intended values:
///
/// ```tjs
/// (function(){ var d = %[]; d["k"] = "v"; ...; return d; })()
/// ```
fn dict_literal(tagname: Option<&str>, params: &[(String, String)]) -> String {
    let mut lit = String::from("(function(){ var d = %[]; ");
    if let Some(name) = tagname {
        lit.push_str("d[");
        lit.push_str(&tjs_escape("tagname"));
        lit.push_str("] = ");
        lit.push_str(&tjs_escape(name));
        lit.push_str("; ");
    }
    for (k, v) in params {
        lit.push_str("d[");
        lit.push_str(&tjs_escape(k));
        lit.push_str("] = ");
        lit.push_str(&tjs_escape(v));
        lit.push_str("; ");
    }
    lit.push_str("return d; })()");
    lit
}

/// Build a real TJS Dictionary from `entries` and write it into `out` as a
/// retained object result (the `VAL_RETAINED` ABI slot the C++ side
/// consumes as a reference). Returns Ok(()) on success; on failure returns
/// the error string (the caller can fall back or propagate).
fn set_dict_result(out: *mut Value, entries: &[(String, String)]) -> Result<(), String> {
    let engine = context_engine()?;
    let literal = dict_literal(None, entries);
    let _ = engine.eval(&literal, "KAGParser");
    match engine.retain_value_detached(&TjsValue::Object) {
        Ok(dv) => {
            // SAFETY: out is a valid result slot; the C++ side consumes the
            // retention (copies + erases) before the callback returns.
            unsafe {
                (*out).ty = tjs2_sys::VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = ptr::null();
                (*out).array = ptr::null();
                (*out).array_count = 0;
                (*out).retained = dv.raw_id() as usize;
            }
            // The C++ conversion consumes the retention; forget the wrapper
            // so its Drop does not release the id first (safe no-op after).
            std::mem::forget(dv);
            Ok(())
        }
        Err(e) => Err(e),
    }
}

/// Build a real TJS Dictionary for a parsed tag (`tagname` key plus every
/// attribute, preserving source order) and write it into `out`.
fn set_tag_dict_result(
    out: *mut Value,
    name: &str,
    params: &[(String, String)],
) -> Result<(), String> {
    let engine = context_engine()?;
    let literal = dict_literal(Some(name), params);
    let _ = engine.eval(&literal, "KAGParser");
    match engine.retain_value_detached(&TjsValue::Object) {
        Ok(dv) => {
            // SAFETY: out is a valid result slot; the C++ side consumes the
            // retention (copies + erases) before the callback returns.
            unsafe {
                (*out).ty = tjs2_sys::VAL_RETAINED;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = ptr::null();
                (*out).array = ptr::null();
                (*out).array_count = 0;
                (*out).retained = dv.raw_id() as usize;
            }
            std::mem::forget(dv);
            Ok(())
        }
        Err(e) => Err(e),
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

fn set_out_integer(out: *mut Value, v: i64) {
    // SAFETY: `out` is a valid result slot for the duration of the call.
    unsafe {
        (*out).ty = VAL_INTEGER;
        (*out).integer = v;
        (*out).real = 0.0;
        (*out).string = ptr::null();
    }
}

/// Convert a native argument to a string the way `ttstr x = *param[i]`
/// does: strings as-is, integers/reals formatted, void as the empty
/// string. Objects cannot be converted.
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
        _ => Err("KAGParser: argument cannot be converted to a string".into()),
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
        _ => Err("KAGParser: argument cannot be converted to a boolean".into()),
    }
}

// ---------------------------------------------------------------------------
// instance payload
// ---------------------------------------------------------------------------

/// Create a fresh parser payload for `new KAGParser()`.
extern "C" fn kagparser_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(KagParserState::default())) as *mut c_void
}

/// Free a parser payload (called exactly once per create).
extern "C" fn kagparser_destroy(_engine: *mut c_void, instance: *mut c_void) {
    // SAFETY: instance came from kagparser_create (Box::into_raw).
    unsafe { drop(Box::from_raw(instance as *mut KagParserState)) };
}

/// The parser payload for the object a method was called on.
// SAFETY: the dispatcher verified the object is a KAGParser instance, so
// `instance` is a valid payload for the call.
unsafe fn state_of(instance: *mut c_void) -> &'static mut KagParserState {
    unsafe { &mut *(instance as *mut KagParserState) }
}

/// A callback that evaluates a TJS expression (eval) or loads a scenario
/// (storage) for the [`Environ`] the parser walks in.
type EvalClosure = Box<dyn FnMut(&str) -> Result<EvalResult, String>>;
type LoadClosure = Box<dyn FnMut(&str) -> Result<String, String>>;
/// A callback that fires the owner object's `onLabel(label, pageName)`.
type LabelClosure = Box<dyn FnMut(&str, Option<&str>)>;

/// Reborrow a boxed label closure as a `dyn FnMut` with the borrow's own
/// object lifetime. `Environ<'a>` is invariant over `'a`, so the `'static`
/// object of `LabelClosure` cannot be returned directly; this helper applies
/// the trait-object lifetime shortening explicitly.
fn shorten_label_closure<'a>(c: &'a mut LabelClosure) -> LabelCallback<'a> {
    &mut **c
}

/// Build the `Environ` (eval + storage callbacks) from the context. The
/// closures own their `Arc`s so the VM and storage stay alive across the
/// re-entrant calls they make.
struct ContextEnv {
    _engine: Arc<Tjs2Engine>,
    eval_closure: EvalClosure,
    load_closure: LoadClosure,
    label_closure: Option<LabelClosure>,
}

impl ContextEnv {
    fn new() -> Result<Self, String> {
        Self::new_with_owner(ptr::null_mut())
    }

    /// Build the environment and, when `owner` is non-null, a callback that
    /// invokes `owner.onLabel(label, pageName)` (reference
    /// `SkipCommentOrLabel`). Member lookup goes through the object's class
    /// chain so a script subclass override runs; a missing `onLabel` (the
    /// base class) is ignored.
    fn new_with_owner(owner: *mut c_void) -> Result<Self, String> {
        let engine = context_engine()?;
        let storage = context_storage()?;
        let eval_closure: EvalClosure = {
            let engine = engine.clone();
            Box::new(move |exp: &str| -> Result<EvalResult, String> {
                match engine.eval(exp, "KAGParser") {
                    Ok(TjsValue::Void) => Ok(EvalResult::Void),
                    Ok(TjsValue::Integer(i)) => Ok(EvalResult::Integer(i)),
                    Ok(TjsValue::Real(r)) => Ok(EvalResult::Real(r)),
                    Ok(TjsValue::String(s)) => Ok(EvalResult::Str(s)),
                    Ok(TjsValue::Object) | Ok(TjsValue::Retained(_)) => {
                        log::warn!(
                            "KAGParser: expression {exp:?} returned an object; treating as void"
                        );
                        Ok(EvalResult::Void)
                    }
                    Err(e) => Err(e.to_string()),
                }
            })
        };
        let load_closure: LoadClosure = Box::new(move |name: &str| -> Result<String, String> {
            let bytes = {
                // The lock is released before any VM call.
                let mut storage = storage
                    .lock()
                    .map_err(|_| "KAGParser: storage lock is poisoned".to_string())?;
                storage.read(name).map_err(|e| e.to_string())?
            };
            decode_script(&bytes)
        });
        let label_closure: Option<LabelClosure> = if owner.is_null() {
            None
        } else {
            // Retain lazily inside the callback: labels are rare compared
            // with `getNextTag` calls, and the owner pointer is alive for the
            // whole native call anyway.
            let engine = engine.clone();
            Some(Box::new(move |label: &str, page: Option<&str>| {
                let Ok(id) = engine.retain_object_detached(owner) else {
                    return;
                };
                let args = [
                    TjsValue::String(label.to_string()),
                    page.map(|p| TjsValue::String(p.to_string()))
                        .unwrap_or(TjsValue::Void),
                ];
                // Ignore a missing `onLabel` (base KAGParser) and any error a
                // handler raises; the reference FuncCall result is likewise
                // not acted on.
                let _ = engine.call_member(id.raw_id(), "onLabel", &args);
            }))
        };
        Ok(ContextEnv {
            _engine: engine,
            eval_closure,
            load_closure,
            label_closure,
        })
    }

    fn environ(&mut self) -> Environ<'_> {
        let fire_label = self.label_closure.as_mut().map(shorten_label_closure);
        Environ {
            eval: &mut *self.eval_closure,
            load_storage: &mut *self.load_closure,
            fire_label,
        }
    }
}

// ---------------------------------------------------------------------------
// native methods
// ---------------------------------------------------------------------------

/// `loadScenario(storageName)`: load + parse a `.ks` from storage; throws
/// on a missing/unreadable file, an empty scenario or a syntax error.
extern "C" fn native_load_scenario(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    _out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return error_out(out_error, "KAGParser.loadScenario requires 1 argument");
    }
    // SAFETY: argv points to `argc` valid entries for the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let name = match param_to_string(&args[0]) {
        Ok(name) => name,
        Err(e) => return error_out(out_error, &e),
    };
    let st = unsafe { state_of(instance) };
    let mut ctx = match ContextEnv::new() {
        Ok(v) => v,
        Err(e) => return error_out(out_error, &e),
    };
    log::info!("KAGParser: scenario loaded : {name}");
    let result = st.load_scenario(&name, &mut ctx.environ());
    match result {
        Ok(()) => {
            set_out_void(_out);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// `goToLabel(label)`: jump to a `*label`; throws when missing.
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
        return error_out(out_error, "KAGParser.goToLabel requires 1 argument");
    }
    // SAFETY: argv points to `argc` valid entries for the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let name = match param_to_string(&args[0]) {
        Ok(name) => name,
        Err(e) => return error_out(out_error, &e),
    };
    match unsafe { state_of(instance) }.go_to_label(&name) {
        Ok(()) => {
            set_out_void(out);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// `callLabel(label)`: push the return position and jump.
extern "C" fn native_call_label(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return error_out(out_error, "KAGParser.callLabel requires 1 argument");
    }
    // SAFETY: argv points to `argc` valid entries for the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let name = match param_to_string(&args[0]) {
        Ok(name) => name,
        Err(e) => return error_out(out_error, &e),
    };
    let st = unsafe { state_of(instance) };
    let mut ctx = match ContextEnv::new() {
        Ok(v) => v,
        Err(e) => return error_out(out_error, &e),
    };
    let result = st.call_label(&name, &mut ctx.environ());
    match result {
        Ok(()) => {
            set_out_void(out);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// `getNextTag()`: the next tag as a string (void at end of scenario).
extern "C" fn native_get_next_tag(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let st = unsafe { state_of(instance) };
    // The owner object is needed to fire `onLabel(label, pageName)` as the
    // walk passes labels (the reference `SkipCommentOrLabel`).
    let mut ctx = match ContextEnv::new_with_owner(objthis) {
        Ok(v) => v,
        Err(e) => return error_out(out_error, &e),
    };
    let result = st.next_tag(&mut ctx.environ());
    match result {
        Ok(Some((name, params))) => match set_tag_dict_result(out, &name, &params) {
            Ok(()) => 0,
            Err(e) => error_out(out_error, &e),
        },
        Ok(None) => {
            set_out_void(out);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// `assign(otherOrStoreString)`: copy state from another parser. Objects
/// cannot cross the C ABI, so a `store()` string is copied instead.
extern "C" fn native_assign(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return error_out(out_error, "KAGParser.assign requires 1 argument");
    }
    // SAFETY: argv points to `argc` valid entries for the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    if args[0].ty == 4 {
        // VAL_OBJECT == 4; the ABI never marshals objects, so this branch
        // is unreachable in practice — kept for clarity.
        return error_out(
            out_error,
            "KAGParser.assign: passing a KAGParser object across the C ABI is not \
             supported; pass a store() string instead",
        );
    }
    let s = match param_to_string(&args[0]) {
        Ok(s) => s,
        Err(e) => return error_out(out_error, &e),
    };
    let st = unsafe { state_of(instance) };
    let mut ctx = match ContextEnv::new() {
        Ok(v) => v,
        Err(e) => return error_out(out_error, &e),
    };
    let result = st.restore(&s, &mut ctx.environ());
    match result {
        Ok(()) => {
            set_out_void(out);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// `clear()`: clear the scenario and per-scenario state.
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
    set_out_void(out);
    0
}

/// `store()`: serialize the parser state as a string.
extern "C" fn native_store(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let s = unsafe { state_of(instance) }.store();
    set_string_result(out, &s);
    0
}

/// `restore(storeString)`: restore parser state from a `store()` string.
extern "C" fn native_restore(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return error_out(out_error, "KAGParser.restore requires 1 argument");
    }
    // SAFETY: argv points to `argc` valid entries for the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let s = match param_to_string(&args[0]) {
        Ok(s) => s,
        Err(e) => return error_out(out_error, &e),
    };
    let st = unsafe { state_of(instance) };
    let mut ctx = match ContextEnv::new() {
        Ok(v) => v,
        Err(e) => return error_out(out_error, &e),
    };
    let result = st.restore(&s, &mut ctx.environ());
    match result {
        Ok(()) => {
            set_out_void(out);
            0
        }
        Err(e) => error_out(out_error, &e),
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
    set_out_void(out);
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
            set_out_void(out);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// `interrupt()`: make the next `getNextTag` return an `interrupt` tag.
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
    set_out_void(out);
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
    set_out_void(out);
    0
}

// --- property-style accessors (the ABI has no instance properties) --------

/// `getCurLine()`.
extern "C" fn native_get_cur_line(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(out, unsafe { state_of(instance) }.get_cur_line() as i64);
    0
}

/// `getCurPos()`.
extern "C" fn native_get_cur_pos(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(out, unsafe { state_of(instance) }.get_cur_pos() as i64);
    0
}

/// `getCurLineStr()`.
extern "C" fn native_get_cur_line_str(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let s = unsafe { state_of(instance) }.get_cur_line_str();
    set_string_result(out, &s);
    0
}

/// `getIgnoreCR()`.
extern "C" fn native_get_ignore_cr(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(out, unsafe { state_of(instance) }.get_ignore_cr() as i64);
    0
}

/// `setIgnoreCR(v)`.
extern "C" fn native_set_ignore_cr(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return error_out(out_error, "KAGParser.setIgnoreCR requires 1 argument");
    }
    // SAFETY: argv points to `argc` valid entries for the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    match param_to_bool(&args[0]) {
        Ok(v) => {
            unsafe { state_of(instance) }.set_ignore_cr(v);
            set_out_void(out);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// `getProcessSpecialTags()`.
extern "C" fn native_get_process_special_tags(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(
        out,
        unsafe { state_of(instance) }.get_process_special_tags() as i64,
    );
    0
}

/// `setProcessSpecialTags(v)`.
extern "C" fn native_set_process_special_tags(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return error_out(
            out_error,
            "KAGParser.setProcessSpecialTags requires 1 argument",
        );
    }
    // SAFETY: argv points to `argc` valid entries for the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    match param_to_bool(&args[0]) {
        Ok(v) => {
            unsafe { state_of(instance) }.set_process_special_tags(v);
            set_out_void(out);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// `getMultiLineTagEnabled()` (KAGParserEx extension).
extern "C" fn native_get_multi_line_tag_enabled(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(
        out,
        unsafe { state_of(instance) }.get_multi_line_tag_enabled() as i64,
    );
    0
}

/// `setMultiLineTagEnabled(v)` (KAGParserEx extension).
extern "C" fn native_set_multi_line_tag_enabled(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return error_out(
            out_error,
            "KAGParser.setMultiLineTagEnabled requires 1 argument",
        );
    }
    // SAFETY: argv points to `argc` valid entries for the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    match param_to_bool(&args[0]) {
        Ok(v) => {
            unsafe { state_of(instance) }.set_multi_line_tag_enabled(v);
            set_out_void(out);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// `getDebugLevel()`.
extern "C" fn native_get_debug_level(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(out, unsafe { state_of(instance) }.get_debug_level() as i64);
    0
}

/// `setDebugLevel(v)`.
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
        return error_out(out_error, "KAGParser.setDebugLevel requires 1 argument");
    }
    // SAFETY: argv points to `argc` valid entries for the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let v =
        match param_to_string(&args[0]).and_then(|s| s.parse::<i32>().map_err(|e| e.to_string())) {
            Ok(v) => v,
            Err(e) => return error_out(out_error, &e),
        };
    unsafe { state_of(instance) }.set_debug_level(v);
    set_out_void(out);
    0
}

/// `getMacros()`: the macro dictionary as `name=body` lines.
/// Get the macros `HashMap` as an ordered `Vec<(name, body)>` for
/// constructing a real Dictionary result.
fn macros_entries(instance: *mut c_void) -> Vec<(String, String)> {
    unsafe { state_of(instance) }.get_macros_entries()
}

/// `getMacros()`: the macro dictionary as a real TJS Dictionary object.
extern "C" fn native_get_macros(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let entries = macros_entries(instance);
    match set_dict_result(out, &entries) {
        Ok(()) => 0,
        Err(e) => error_out(out_error, &e),
    }
}

/// `setMacros(dict)`: replace the macro dictionary with a real Dictionary.
/// The C ABI marshals only void/int/real/string arguments, so the argument
/// is passed as a `setMacros.kvp`-style string; the game always installs
/// macros via `(Dictionary.assign incontextof macros)(_macros)` on the
/// `macros` property, which arrives here as a string-encoded dict.
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
        return error_out(out_error, "KAGParser.setMacros requires 1 argument");
    }
    // SAFETY: argv points to `argc` valid entries for the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let s = match param_to_string(&args[0]) {
        Ok(s) => s,
        Err(e) => return error_out(out_error, &e),
    };
    match unsafe { state_of(instance) }.set_macros(&s) {
        Ok(()) => {
            set_out_void(out);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// `getMacroParams()`: the top macro-args dictionary, void when none.
extern "C" fn native_get_macro_params(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match unsafe { state_of(instance) }.get_macro_params_entries() {
        Some(entries) => match set_dict_result(out, &entries) {
            Ok(()) => 0,
            Err(e) => error_out(out_error, &e),
        },
        None => {
            set_out_void(out);
            0
        }
    }
}

/// `getMP()`: alias of `getMacroParams`.
extern "C" fn native_get_mp(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    native_get_macro_params(
        _engine,
        instance,
        0,
        ptr::null(),
        out,
        _out_error,
        std::ptr::null_mut(),
    )
}

/// `getCallStackDepth()`.
extern "C" fn native_get_call_stack_depth(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(
        out,
        unsafe { state_of(instance) }.get_call_stack_depth() as i64,
    );
    0
}

/// `getCurStorage()`.
extern "C" fn native_get_cur_storage(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let s = unsafe { state_of(instance) }.get_storage_name().to_string();
    set_string_result(out, &s);
    0
}

/// `setCurStorage(name)`: like `loadScenario`.
extern "C" fn native_set_cur_storage(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return error_out(out_error, "KAGParser.setCurStorage requires 1 argument");
    }
    // SAFETY: argv points to `argc` valid entries for the call.
    let args = unsafe { std::slice::from_raw_parts(argv, argc as usize) };
    let name = match param_to_string(&args[0]) {
        Ok(name) => name,
        Err(e) => return error_out(out_error, &e),
    };
    let st = unsafe { state_of(instance) };
    let mut ctx = match ContextEnv::new() {
        Ok(v) => v,
        Err(e) => return error_out(out_error, &e),
    };
    let result = st.load_scenario(&name, &mut ctx.environ());
    match result {
        Ok(()) => {
            set_out_void(out);
            0
        }
        Err(e) => error_out(out_error, &e),
    }
}

/// `getCurLabel()`.
extern "C" fn native_get_cur_label(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let s = unsafe { state_of(instance) }.get_cur_label().to_string();
    set_string_result(out, &s);
    0
}

// ---------------------------------------------------------------------------
// native properties
//
// The reference registers `ignoreCR`, `processSpecialTags`, `debugLevel`,
// `curLine`, `curPos`, `curLineStr`, `callStackDepth`, `curStorage`,
// `curLabel`, `macros`, `macroParams`, `mp` as *properties* (see
// `TJS_BEGIN_NATIVE_PROP_DECL` in reference/cpp/core/base/KAGParser.cpp),
// and games' ScController subclasses write them as plain properties:
//
//     system/sccontroller.tjs:  ignoreCR = true; processSpecialTags = true;
//     system/advscreen.tjs:     _scCtrl.ignoreCR = false;   (talk handler)
//     system/animationsequence.tjs: debugLevel = tkdlNone;
//
// Without real properties these assignments land in the object's dynamic
// storage and never reach the parser state. The bool/int/string ones are
// wired here; the dictionary one (`macros`) returns the kvp string encoding
// until the object-return work lands.
// ---------------------------------------------------------------------------

extern "C" fn prop_ignore_cr_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(out, unsafe { state_of(instance) }.get_ignore_cr() as i64);
    0
}

extern "C" fn prop_ignore_cr_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract (one valid argument).
    match param_to_bool(unsafe { &*value }) {
        Ok(v) => {
            unsafe { state_of(instance) }.set_ignore_cr(v);
            0
        }
        Err(e) => error_out(_out_error, &e),
    }
}

extern "C" fn prop_process_special_tags_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(
        out,
        unsafe { state_of(instance) }.get_process_special_tags() as i64,
    );
    0
}

extern "C" fn prop_process_special_tags_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract.
    match param_to_bool(unsafe { &*value }) {
        Ok(v) => {
            unsafe { state_of(instance) }.set_process_special_tags(v);
            0
        }
        Err(e) => error_out(_out_error, &e),
    }
}

extern "C" fn prop_debug_level_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(out, unsafe { state_of(instance) }.get_debug_level() as i64);
    0
}

extern "C" fn prop_multi_line_tag_enabled_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(
        out,
        unsafe { state_of(instance) }.get_multi_line_tag_enabled() as i64,
    );
    0
}

extern "C" fn prop_multi_line_tag_enabled_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract.
    match param_to_bool(unsafe { &*value }) {
        Ok(v) => {
            unsafe { state_of(instance) }.set_multi_line_tag_enabled(v);
            0
        }
        Err(e) => error_out(_out_error, &e),
    }
}

extern "C" fn prop_debug_level_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract.
    let v = match param_to_string(unsafe { &*value })
        .and_then(|s| s.parse::<i32>().map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => return error_out(_out_error, &e),
    };
    unsafe { state_of(instance) }.set_debug_level(v);
    0
}

extern "C" fn prop_cur_line_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(out, unsafe { state_of(instance) }.get_cur_line() as i64);
    0
}

extern "C" fn prop_cur_pos_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(out, unsafe { state_of(instance) }.get_cur_pos() as i64);
    0
}

extern "C" fn prop_cur_line_str_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let s = unsafe { state_of(instance) }.get_cur_line_str();
    set_string_result(out, &s);
    0
}

extern "C" fn prop_call_stack_depth_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_integer(
        out,
        unsafe { state_of(instance) }.get_call_stack_depth() as i64,
    );
    0
}

extern "C" fn prop_cur_storage_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let s = unsafe { state_of(instance) }.get_storage_name().to_string();
    set_string_result(out, &s);
    0
}

extern "C" fn prop_cur_storage_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract.
    let name = match param_to_string(unsafe { &*value }) {
        Ok(name) => name,
        Err(e) => return error_out(_out_error, &e),
    };
    let st = unsafe { state_of(instance) };
    let mut ctx = match ContextEnv::new() {
        Ok(v) => v,
        Err(e) => return error_out(_out_error, &e),
    };
    match st.load_scenario(&name, &mut ctx.environ()) {
        Ok(()) => 0,
        Err(e) => error_out(_out_error, &e),
    }
}

extern "C" fn prop_cur_label_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let s = unsafe { state_of(instance) }.get_cur_label().to_string();
    set_string_result(out, &s);
    0
}

// --- dictionary-shaped property -------------------------------------------------
// `macros` is read/written by the game's `loadMacro`:
//     (Dictionary.assign incontextof _macros)(macros);      // read
//     (Dictionary.assign incontextof macros)(_macros);      // write
// The getter returns a real Dictionary object so `Dictionary.assign` can
// copy it; the setter receives the string-encoded dict built by the TJS
// side when assigning a Dictionary back.
extern "C" fn prop_macros_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let entries = macros_entries(instance);
    match set_dict_result(out, &entries) {
        Ok(()) => 0,
        Err(e) => error_out(out_error, &e),
    }
}

/// `macroParams` / `mp` property getter (reference KAGParser.cpp:2484,2497).
/// Both expose `GetMacroTopNoAddRef()` — the top macro-args dictionary —
/// exactly like the `getMacroParams()` / `getMP()` methods; a parser with no
/// active macro returns void (the reference returns a null object).
extern "C" fn prop_macro_params_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match unsafe { state_of(instance) }.get_macro_params_entries() {
        Some(entries) => match set_dict_result(out, &entries) {
            Ok(()) => 0,
            Err(e) => error_out(out_error, &e),
        },
        None => {
            set_out_void(out);
            0
        }
    }
}

extern "C" fn prop_macros_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract.
    let s = match param_to_string(unsafe { &*value }) {
        Ok(s) => s,
        Err(e) => return error_out(_out_error, &e),
    };
    // The reference `macros` property is read-only (`TJS_DENY_NATIVE_PROP_SETTER`):
    // the game mutates it in place via `(Dictionary.assign incontextof macros)(dict)`,
    // never via `p.macros = dict`. A direct `p.macros = "..."` must not be treated
    // as a full `restore` — that would be a false restore of the whole parser state
    // (storageName/curLabel/callStack/etc.) from a kvp string like `"foo=bar"`
    // (anti-60 pattern). The legacy string-encoded setter path is therefore
    // `setMacros` (kvp dict), matching `native_set_macros`, with a read-only warning.
    log::warn!(
        "KAGParser: macros property is read-only in the reference (assign incontextof macros); treating `p.macros = ...` as setMacros"
    );
    match unsafe { state_of(instance) }.set_macros(&s) {
        Ok(()) => 0,
        Err(e) => error_out(_out_error, &e),
    }
}

/// `KAGParser()` — the constructor member. The tjs2-sys constructor
/// dispatch creates+registers the native payload on script-subclass
/// objects (the game's `class ScController extends KAGParser` calls
/// `super.KAGParser()` in its constructor); for `new KAGParser()` the
/// payload already exists. Nothing to initialize — the reference's
/// constructor is empty (`return TJS_S_OK`).
extern "C" fn kagparser_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_out_void(out);
    0
}

// ---------------------------------------------------------------------------
// registration
// ---------------------------------------------------------------------------

/// Register the `KAGParser` native instance class on `engine`'s global
/// object.
pub fn register_kagparser(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "KAGParser",
        create: kagparser_create,
        destroy: kagparser_destroy,
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
            // Reference KAGParser.cpp:2484 / :2497: `macroParams` and its
            // short alias `mp` are read-only dictionary properties (the
            // top macro-args dict). `getMacroParams()` / `getMP()` remain
            // as the method forms, matching the reference which has both.
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

// ---------------------------------------------------------------------------
// tests (headless: temp game dir + real TJS2 VM + real Storage)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    /// The KAGParser native registers a process-global engine context;
    /// parallel tests race on it (segfault). Serialize this crate's tests
    /// with one lock; all other crates stay fully parallel.
    static VM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn vm_lock() -> std::sync::MutexGuard<'static, ()> {
        VM_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }

    use super::*;

    /// A scratch game directory under the system temp dir, removed on drop.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let base = std::env::temp_dir().join(format!(
                "krkr-rs-tvp-kagparser-{}-{}-{}",
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

    /// An engine + mounted storage with `KAGParser` registered and the
    /// context set. The TJS2 VM is single-threaded; run tests with
    /// `--test-threads=1`.
    struct TestEnv {
        _dir: TempDir,
        engine: Arc<Tjs2Engine>,
    }

    impl TestEnv {
        #[allow(clippy::arc_with_non_send_sync)]
        fn new(tag: &str, files: &[(&str, Vec<u8>)]) -> TestEnv {
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
            register_kagparser(&engine).expect("register KAGParser");
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

        fn exec_ok(&self, script: &str) -> TjsValue {
            self.engine
                .exec_script(script, "test")
                .unwrap_or_else(|e| panic!("exec failed: {e}"))
        }
    }

    fn ks<'a>(name: &'a str, text: &str) -> (&'a str, Vec<u8>) {
        (name, text.as_bytes().to_vec())
    }

    #[test]
    fn native_walk_matches_reference_tag_sequence() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new(
            "walk",
            &[ks(
                "test.ks",
                "; comment\n\
                 *start\n\
                 Hello [b]world[/b]!\n\
                 @wait 1000\n\
                 [bg storage=\"chapter1/f01.jpg\" effect=0]\n\
                 [wait]\n",
            )],
        );
        env.exec_ok("var p = new KAGParser(); p.loadScenario('test.ks');");
        // First tags: per-char ch tags, the [b] tag, text, [/b], '!'.
        env.exec_ok(
            "var a = p.getNextTag(); var b = p.getNextTag(); \
             var c = p.getNextTag(); var d = p.getNextTag();",
        );
        // getNextTag returns a real TJS Dictionary now: tagname + params.
        assert_eq!(env.eval_ok("a.tagname"), TjsValue::String("ch".into()));
        assert_eq!(env.eval_ok("a.text"), TjsValue::String("H".into()));
        assert_eq!(env.eval_ok("b.tagname"), TjsValue::String("ch".into()));
        assert_eq!(env.eval_ok("b.text"), TjsValue::String("e".into()));
        // Text runs are emitted one `ch` tag per character (the reference
        // `_GetNextTag` normal-character branch), so the third and fourth
        // tags are the remaining 'l' characters of "Hello".
        assert_eq!(env.eval_ok("c.tagname"), TjsValue::String("ch".into()));
        assert_eq!(env.eval_ok("c.text"), TjsValue::String("l".into()));
        assert_eq!(env.eval_ok("d.tagname"), TjsValue::String("ch".into()));
        assert_eq!(env.eval_ok("d.text"), TjsValue::String("l".into()));
        // ... walk to the end and assert the full sequence. The first four
        // `ch` tags (H, e, l, l) were already consumed above, so the
        // walked remainder starts with the 'o' and the space of "Hello ".
        env.exec_ok(
            "var w = []; var names = []; var wt; \
             var waitd = null; var bgd = null; \
             while ((wt = p.getNextTag()) !== void) { \
                 w.add(wt); names.add(wt.tagname); \
                 if (wt.tagname == 'wait' && waitd == null) waitd = wt; \
                 if (wt.tagname == 'bg') bgd = wt; \
             }",
        );
        let joined = env.eval_ok("names.join(\"\\x1e\")");
        let TjsValue::String(joined) = joined else {
            panic!("expected string")
        };
        let names: Vec<&str> = joined.split('\x1e').collect();
        assert_eq!(
            names,
            [
                "ch", "ch", // remaining "o" and " " of "Hello "
                "b", "ch", "ch", "ch", "ch", "ch", // world
                "/b", "ch", // !
                "r",  // line end of the text line
                "wait", "bg", "r", // [bg] line end
                "wait", "r",
            ]
        );
        // wait tag carries the bare-attr "1000"="true" (as a dict member)
        assert_eq!(
            env.eval_ok("waitd[\"1000\"]"),
            TjsValue::String("true".into())
        );
        // bg tag params in source order
        assert_eq!(
            env.eval_ok("bgd.storage"),
            TjsValue::String("chapter1/f01.jpg".into())
        );
        assert_eq!(env.eval_ok("bgd.effect"), TjsValue::String("0".into()));
        // end of scenario → void
        assert_eq!(
            env.eval_ok("(new KAGParser()).getNextTag()"),
            TjsValue::Void
        );
    }

    #[test]
    fn native_properties_write_parser_state() {
        let _vm_lock = vm_lock();
        // The game's ScController writes these as plain properties; they
        // must reach the parser state (not dynamic member storage).
        // Reference defaults: processSpecialTags=true, ignoreCR=false,
        // debugLevel=tkdlSimple(1) (KAGParser.cpp:316-322).
        let env = TestEnv::new("props", &[ks("test.ks", "*start\nHello\n")]);
        env.exec_ok(
            "var p = new KAGParser(); \
             p.ignoreCR = true; \
             p.processSpecialTags = false; \
             p.debugLevel = 2; \
             p.loadScenario('test.ks'); \
             var ig = p.ignoreCR; \
             var pst = p.processSpecialTags; \
             var dl = p.debugLevel; \
             var cs = p.curStorage;",
        );
        assert_eq!(env.eval_ok("ig"), TjsValue::Integer(1));
        assert_eq!(env.eval_ok("pst"), TjsValue::Integer(0));
        assert_eq!(env.eval_ok("dl"), TjsValue::Integer(2));
        assert_eq!(env.eval_ok("cs"), TjsValue::String("test.ks".into()));
        // method getters still agree with the property values
        assert_eq!(env.eval_ok("p.getIgnoreCR()"), TjsValue::Integer(1));
        assert_eq!(
            env.eval_ok("p.getProcessSpecialTags()"),
            TjsValue::Integer(0)
        );
        assert_eq!(env.eval_ok("p.getDebugLevel()"), TjsValue::Integer(2));
        // a fresh parser has the reference defaults
        env.exec_ok("var q = new KAGParser(); var qdl = q.debugLevel; var qpst = q.processSpecialTags; var qcr = q.ignoreCR;");
        assert_eq!(env.eval_ok("qdl"), TjsValue::Integer(1));
        assert_eq!(env.eval_ok("qpst"), TjsValue::Integer(1));
        assert_eq!(env.eval_ok("qcr"), TjsValue::Integer(0));
    }

    #[test]
    fn go_to_label_and_jump_loop_from_script() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new(
            "jumploop",
            &[ks(
                "test.ks",
                "*start\nOne\n*loop\n[wait]\n@jump target=*loop\n",
            )],
        );
        env.exec_ok(
            "var p = new KAGParser(); p.loadScenario('test.ks'); \
             var waits = 0; var seen = []; \
             while (waits < 2) { var t = p.getNextTag(); seen.add(t); \
                 if (t !== void && t.tagname == 'wait') waits++; } \
             var depth = p.getCallStackDepth();",
        );
        assert_eq!(env.eval_ok("waits"), TjsValue::Integer(2));
        assert_eq!(env.eval_ok("depth"), TjsValue::Integer(0));
        assert_eq!(
            env.eval_ok("seen[0].tagname"),
            TjsValue::String("ch".into())
        );
        assert_eq!(env.eval_ok("seen[0].text"), TjsValue::String("O".into()));
        // goToLabel to a missing label is a catchable TJS error
        env.exec_ok(
            "var caught = ''; try { p.goToLabel('*nope'); } catch(e) { caught = 'missing'; }",
        );
        assert_eq!(env.eval_ok("caught"), TjsValue::String("missing".into()));
    }

    #[test]
    fn call_label_and_return_tag_from_script() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new(
            "callreturn",
            &[ks(
                "test.ks",
                "*start\nFirst\n@call target=*sub\n@jump target=*end\n*sub\nSubText\n@return\n*end\nLast\n",
            )],
        );
        env.exec_ok("var p = new KAGParser(); p.loadScenario('test.ks');");
        assert_eq!(env.eval_ok("p.getCallStackDepth()"), TjsValue::Integer(0));
        // callLabel pushes a return position and jumps
        env.exec_ok("p.callLabel('*sub');");
        assert_eq!(env.eval_ok("p.getCallStackDepth()"), TjsValue::Integer(1));
        env.exec_ok("var t = p.getNextTag(); var tn = t.tagname; var tx = t.text;");
        assert_eq!(env.eval_ok("tn"), TjsValue::String("ch".into()));
        assert_eq!(env.eval_ok("tx"), TjsValue::String("S".into()));
        // @return pops back to the caller (line after the callLabel)
        env.exec_ok("while (p.getNextTag() !== void) {}");
        assert_eq!(env.eval_ok("p.getCallStackDepth()"), TjsValue::Integer(0));
        // a bare @return with no call is a catchable error
        env.exec_ok("var q = new KAGParser(); q.loadScenario('test.ks');");
        eprintln!("[t] q created");
        env.exec_ok(
            "var caught = ''; try { q.goToLabel('*sub'); while (q.getNextTag() !== void) {} } \
             catch(e) { caught = 'underflow'; }",
        );
        eprintln!("[t] try-loop done");
        assert_eq!(env.eval_ok("caught"), TjsValue::String("underflow".into()));
        eprintln!("[t] assert done");
    }

    #[test]
    fn load_failure_is_a_catchable_tjs_error() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new("loadfail", &[ks("ok.ks", "*start\nHello\n")]);
        // missing file
        assert!(
            env.eval("(new KAGParser()).loadScenario('nope.ks')")
                .is_err()
        );
        env.exec_ok(
            "var caught = ''; \
             try { (new KAGParser()).loadScenario('nope.ks'); } catch(e) { caught = 'missing'; }",
        );
        assert_eq!(env.eval_ok("caught"), TjsValue::String("missing".into()));
        // syntax error in the file
        let env = TestEnv::new("loadfail-syntax", &[ks("broken.ks", "*start\n[tag\n")]);
        env.exec_ok(
            "var caught = ''; \
             try { (new KAGParser()).loadScenario('broken.ks'); } catch(e) { caught = 'syntax'; }",
        );
        assert_eq!(env.eval_ok("caught"), TjsValue::String("syntax".into()));
    }

    #[test]
    fn macro_recording_expansion_and_macros_property() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new(
            "macro",
            &[ks(
                "test.ks",
                "[macro name=bgm]\n[bgmplay storage=%storage loop=true]\n[endmacro]\n@bgm storage=\"01.mp3\"\n",
            )],
        );
        env.exec_ok(
            "var p = new KAGParser(); p.loadScenario('test.ks'); \
             var r = []; var t; var bgmd = null; \
             while ((t = p.getNextTag()) !== void) { r.add(t); \
                 if (t.tagname == 'bgmplay') bgmd = t; } \
             var m = p.getMacros(); var mbody = m['bgm'];",
        );
        // expansion emitted a bgmplay tag with the resolved %storage and
        // the loop flag (real dict members)
        assert_eq!(
            env.eval_ok("bgmd.tagname"),
            TjsValue::String("bgmplay".into())
        );
        assert_eq!(
            env.eval_ok("bgmd.storage"),
            TjsValue::String("01.mp3".into())
        );
        assert_eq!(env.eval_ok("bgmd.loop"), TjsValue::String("true".into()));
        // the recorded body contains the %-arg and the auto [macropop]
        let TjsValue::String(macros) = env.eval_ok("mbody") else {
            panic!()
        };
        assert!(macros.contains("bgmplay"), "{macros}");
        assert!(macros.contains("storage=%storage"), "{macros}");
        assert!(macros.contains("macropop"), "{macros}");
        // a macro with %name|default syntax
        let env = TestEnv::new(
            "macro-default",
            &[ks(
                "test.ks",
                "[macro name=mv]\n@move my=%height|10\n[endmacro]\n@mv\n@mv height=99\n",
            )],
        );
        env.exec_ok(
            "var p = new KAGParser(); p.loadScenario('test.ks'); \
             var r = []; var t; \
             while ((t = p.getNextTag()) !== void) r.add(t);",
        );
        assert_eq!(env.eval_ok("r[2].tagname"), TjsValue::String("move".into()));
        assert_eq!(env.eval_ok("r[2].my"), TjsValue::String("10".into()));
        assert_eq!(env.eval_ok("r[4].tagname"), TjsValue::String("move".into()));
        assert_eq!(env.eval_ok("r[4].my"), TjsValue::String("99".into()));
        // macroParams returns the top macro-args dict (empty here), and the
        // reference's `macroParams` / `mp` properties (KAGParser.cpp:2484,
        // :2497) are aliases with the same value.
        assert_eq!(env.eval_ok("p.getMacroParams()"), TjsValue::Void);
        assert_eq!(env.eval_ok("p.macroParams"), TjsValue::Void);
        assert_eq!(env.eval_ok("p.mp"), TjsValue::Void);
        // The reference registers both as read-only properties.
        assert!(env.eval("p.macroParams = 1").is_err());
        assert!(env.eval("p.mp = 1").is_err());
    }

    #[test]
    fn game_load_macro_pattern_reads_real_dictionaries() {
        let _vm_lock = vm_lock();
        // The real game's `loadMacro` reads the macro table with
        // `(Dictionary.assign incontextof _macros)(macros)` — `getMacros()`
        // (and the `macros` property) must hand back a real Dictionary
        // whose members, including Japanese macro names like the 165 in
        // scenario/macro.ks, are readable and drive expansion.
        let env = TestEnv::new(
            "loadmacro",
            &[ks(
                "test.ks",
                "[macro name=ジャンプ]\n@jump target=%target\n[endmacro]\n\
                 [macro name=move]\n@move x=%x|0 y=%y|0\n[endmacro]\n\
                 *start\n@ジャンプ target=*end\n*end\n",
            )],
        );
        env.exec_ok("var p = new KAGParser(); p.loadScenario('test.ks');");
        // The game's loadMacro walks the scenario to the end first (the
        // reference records [macro] blocks as the walk encounters them):
        // `loadScenario(file); while(getNextTag() !== void){}` -- sccontroller.tjs(140).
        env.exec_ok("while (p.getNextTag() !== void) {}");
        // the macros Dictionary is real: Japanese + ASCII names resolve,
        // unknown members are void
        env.exec_ok(
            "var m = p.getMacros(); \
             var j = m['ジャンプ']; var mv = m['move']; var absent = m['nope'];",
        );
        let TjsValue::String(j) = env.eval_ok("j") else {
            panic!()
        };
        assert!(j.contains("jump"), "{j}");
        assert!(j.contains("target=%target"), "{j}");
        let TjsValue::String(mv) = env.eval_ok("mv") else {
            panic!()
        };
        assert!(mv.contains("x=%x|0"), "{mv}");
        assert_eq!(env.eval_ok("absent"), TjsValue::Void);
        // the same Dictionary drives expansion: @ジャンプ target=end jumps
        // to *end, so no tags are emitted after it
        env.exec_ok(
            "var r = []; var t; \
             while ((t = p.getNextTag()) !== void) r.add(t.tagname);",
        );
        let TjsValue::String(names) = env.eval_ok("r.join('\x1e')") else {
            panic!()
        };
        assert_eq!(names, "");
        // Dictionary.assign against the property works like the game's
        // loadMacro: copy the macros dict into a script Dictionary
        env.exec_ok(
            "var _macros = new Dictionary(); \
             (Dictionary.assign incontextof _macros)(p.macros); \
             var copied = _macros['move'];",
        );
        let TjsValue::String(copied) = env.eval_ok("copied") else {
            panic!()
        };
        assert!(copied.contains("move"), "{copied}");
    }

    #[test]
    fn if_endif_evaluates_expressions_in_the_vm() {
        let _vm_lock = vm_lock();
        // ChkGlobalFlagOn is a global function, exactly like the real
        // game's 01_01.ks uses it.
        let env = TestEnv::new(
            "cond",
            &[ks(
                "test.ks",
                "@if exp=\"ChkGlobalFlagOn(1)\"\n@onFlag id=201\n@endif\n\
                 @if exp=\"ChkGlobalFlagOn(2)\"\n@onFlag id=202\n@endif\n\
                 @if exp=\"ChkGlobalFlagOn(3)\"\n@x a=1\n@else\n@y b=2\n@endif\n",
            )],
        );
        env.exec_ok(
            "var flag = 0; \
             function ChkGlobalFlagOn(n) { return n == 1 || n == 3; } \
             var p = new KAGParser(); p.loadScenario('test.ks'); \
             var r = []; var names = []; var t; \
             var onflag = null; var xtag = null; var ytag = null; \
             while ((t = p.getNextTag()) !== void) { \
                 r.add(t); names.add(t.tagname); \
                 if (t.tagname == 'onflag') onflag = t; \
                 if (t.tagname == 'x') xtag = t; \
                 if (t.tagname == 'y') ytag = t; \
             }",
        );
        // first if (flag 1 on) emitted, second (flag 2) skipped
        assert_eq!(env.eval_ok("onflag.id"), TjsValue::String("201".into()));
        let TjsValue::String(names) = env.eval_ok("names.join('\x1e')") else {
            panic!()
        };
        assert!(!names.contains("id=202"), "{names}");
        // if-true → x emitted, else branch skipped; y must not appear
        assert_eq!(env.eval_ok("xtag.tagname"), TjsValue::String("x".into()));
        assert_eq!(env.eval_ok("xtag.a"), TjsValue::String("1".into()));
        // `eval("ytag")` on a void-valued local falls back to the engine's
        // last_object slot (documented FFI quirk), so compare with `=== null`.
        assert_eq!(env.eval_ok("ytag === null"), TjsValue::Integer(1));
        assert!(!names.contains("y"), "{names}");
    }

    #[test]
    fn store_restore_round_trip_from_script() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new(
            "store",
            &[ks(
                "test.ks",
                "*start\nFirst\n@bg file=\"x.jpg\"\nSecond\n@wait\n",
            )],
        );
        env.exec_ok(
            "var p = new KAGParser(); p.loadScenario('test.ks'); \
             for (var i = 0; i < 5; i++) p.getNextTag(); \
             var saved = p.store();",
        );
        // finish the scenario (collect tagnames — tags are dicts now)
        env.exec_ok(
            "var rest1 = []; var t; \
             while ((t = p.getNextTag()) !== void) rest1.add(t.tagname);",
        );
        // restore and replay the remainder — identical sequence
        env.exec_ok("p.restore(saved);");
        env.exec_ok(
            "var rest2 = []; var t2; \
             while ((t2 = p.getNextTag()) !== void) rest2.add(t2.tagname);",
        );
        assert_eq!(
            env.eval_ok("rest1.join(\"\\x1e\")"),
            env.eval_ok("rest2.join(\"\\x1e\")")
        );
        // remainder after the 5 pre-consumed `ch` tags (F..t): the line-end
        // r, the bg tag, the six chars of "Second" + its r, and the final
        // wait tag.
        assert!(env.eval_ok("rest1.count").eq(&TjsValue::Integer(10)));
    }

    #[test]
    fn utf16le_and_cp932_scenarios_load() {
        let _vm_lock = vm_lock();
        // UTF-16LE with BOM
        let utf16 = {
            let mut b = encoding::UTF16LE_BOM.to_vec();
            b.extend(tvp_util::encoding::to_utf16le_bytes(
                "*start\nこんにちは\n@wait\n",
            ));
            b
        };
        let env = TestEnv::new("utf16", &[("u.ks", utf16)]);
        env.exec_ok("var p = new KAGParser(); p.loadScenario('u.ks');");
        env.exec_ok("var t = p.getNextTag(); var tn = t.tagname; var tx = t.text;");
        assert_eq!(env.eval_ok("tn"), TjsValue::String("ch".into()));
        assert_eq!(env.eval_ok("tx"), TjsValue::String("こ".into()));

        // CP932 without BOM (legacy Japanese)
        let body = tvp_util::encoding::Encoding::Cp932
            .encode("*start\nこんにちは\n@wait\n")
            .expect("encode cp932");
        let env = TestEnv::new("cp932", &[("j.ks", body)]);
        env.exec_ok("var p = new KAGParser(); p.loadScenario('j.ks');");
        env.exec_ok("var t = p.getNextTag(); var tn = t.tagname; var tx = t.text;");
        assert_eq!(env.eval_ok("tn"), TjsValue::String("ch".into()));
        assert_eq!(env.eval_ok("tx"), TjsValue::String("こ".into()));
    }

    #[test]
    fn interrupt_returns_an_interrupt_tag() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new("interrupt", &[ks("test.ks", "*start\nHello\n")]);
        env.exec_ok("var p = new KAGParser(); p.loadScenario('test.ks');");
        env.exec_ok("p.interrupt();");
        env.exec_ok("var t = p.getNextTag(); var iname = t.tagname;");
        assert_eq!(env.eval_ok("iname"), TjsValue::String("interrupt".into()));
        // the walk continues normally afterwards
        env.exec_ok("var t2 = p.getNextTag(); var tn = t2.tagname; var tx = t2.text;");
        assert_eq!(env.eval_ok("tn"), TjsValue::String("ch".into()));
        assert_eq!(env.eval_ok("tx"), TjsValue::String("H".into()));
        env.exec_ok("p.resetInterrupt(); p.interrupt(); p.resetInterrupt();");
        env.exec_ok("var t3 = p.getNextTag(); var t3n = t3.tagname; var t3x = t3.text;");
        assert_eq!(env.eval_ok("t3n"), TjsValue::String("ch".into()));
        assert_eq!(env.eval_ok("t3x"), TjsValue::String("e".into()));
    }

    #[test]
    fn shim_wrapper_restores_dictionary_and_property_surface() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new("shim", &[ks("test.ks", "*start\nHello\n@wait\n")]);
        env.exec_ok(shim::INSTALL_WRAPPER);
        env.exec_ok(
            "var p = new KAGParserCompat(); \
             p.ignoreCR = true; \
             p.processSpecialTags = true; \
             p.loadScenario('test.ks');",
        );
        // getNextTag returns a real dictionary via the wrapper
        env.exec_ok("var tag = p.getNextTag();");
        assert_eq!(env.eval_ok("tag.tagname"), TjsValue::String("ch".into()));
        assert_eq!(env.eval_ok("tag.text"), TjsValue::String("H".into()));
        // ignoreCR property setter took effect: no r tags
        env.exec_ok(
            "var names = []; var t; while ((t = p.getNextTag()) !== void) names.add(t.tagname);",
        );
        let TjsValue::String(names) = env.eval_ok("names.join(',')") else {
            panic!()
        };
        assert!(!names.contains('r'), "{names}");
        // curLabel / curLineStr property getters
        env.exec_ok(
            "var p2 = new KAGParserCompat(); p2.loadScenario('test.ks'); p2.goToLabel('*start');",
        );
        assert_eq!(
            env.eval_ok("p2.curLabel"),
            TjsValue::String("*start".into())
        );
        // macros property: native returns a real Dictionary now; the
        // string-encoded setter round-trips into a dict on read-back
        env.exec_ok(
            "var p3 = new KAGParserCompat(); p3.loadScenario('test.ks'); \
             p3.macros = 'foo=bar'; var m2 = p3.macros; var m2foo = m2['foo'];",
        );
        assert_eq!(env.eval_ok("m2foo"), TjsValue::String("bar".into()));
    }

    #[test]
    fn script_subclass_constructor_creates_the_native_instance() {
        let _vm_lock = vm_lock();
        // The game's `class ScController extends KAGParser` +
        // `super.KAGParser()`: the constructor member must create+register
        // the native instance on the script-subclass object so subsequent
        // native methods resolve (this used to be an ABI limitation).
        let env = TestEnv::new("extends", &[ks("test.ks", "*start\nHello\n")]);
        env.exec_ok(
            "var ok = false; \
             class X extends KAGParser { \
                 function X() { super.KAGParser(); } \
                 function getNext() { return getNextTag(); } \
             } \
             var x = new X(); \
             x.loadScenario('test.ks'); \
             var t = x.getNextTag(); \
             x.ignoreCR = true; \
             var ig = x.ignoreCR; \
             ok = (t !== void);",
        );
        // super.KAGParser() succeeded and the payload is reachable; the
        // subclass reads the real dict's members like the game does
        assert_eq!(env.eval_ok("ok"), TjsValue::Integer(1));
        assert_eq!(env.eval_ok("t.tagname"), TjsValue::String("ch".into()));
        assert_eq!(env.eval_ok("t.text"), TjsValue::String("H".into()));
        assert_eq!(env.eval_ok("ig"), TjsValue::Integer(1));
    }

    #[test]
    fn load_scenario_rewinds_and_clear_resets() {
        let _vm_lock = vm_lock();
        let env = TestEnv::new("rewind", &[ks("test.ks", "*start\nOne\nTwo\n")]);
        env.exec_ok("var p = new KAGParser(); p.loadScenario('test.ks'); p.getNextTag();");
        // the second call returns 'n' (the second character of "One": the
        // reference emits one `ch` tag per character)
        env.exec_ok("var t1 = p.getNextTag(); var t1x = t1.text;");
        assert_eq!(env.eval_ok("t1x"), TjsValue::String("n".into()));
        // re-loading the same storage rewinds to the start
        env.exec_ok("p.loadScenario('test.ks'); var t2 = p.getNextTag(); var t2x = t2.text;");
        assert_eq!(env.eval_ok("t2x"), TjsValue::String("O".into()));
        // clear() unloads: getNextTag returns void immediately
        env.exec_ok("p.clear();");
        assert_eq!(env.eval_ok("p.getNextTag()"), TjsValue::Void);
        assert_eq!(
            env.eval_ok("p.getCurStorage()"),
            TjsValue::String("".into())
        );
    }

    #[test]
    fn multi_line_tag_property_parses_ams_continuations() {
        let _vm_lock = vm_lock();
        // The exact shape the game's AnimationSequenceController uses:
        // `multiLineTagEnabled = true` then a `.ams` `@motion` whose `path`
        // lives on a `;`-prefixed continuation line.
        let env = TestEnv::new(
            "multiline",
            &[ks(
                "test.ams",
                "@motion id=MARK accel=2 time=750 \\\n; path=\"0, 1, 2\"\n@wait time=750\n",
            )],
        );
        env.exec_ok(
            "var p = new KAGParser(); \
             p.multiLineTagEnabled = true; \
             p.ignoreCR = true; \
             p.loadScenario('test.ams'); \
             var m = p.getNextTag(); var w = p.getNextTag();",
        );
        assert_eq!(env.eval_ok("m.tagname"), TjsValue::String("motion".into()));
        assert_eq!(env.eval_ok("m.id"), TjsValue::String("MARK".into()));
        assert_eq!(env.eval_ok("m.time"), TjsValue::String("750".into()));
        assert_eq!(env.eval_ok("m.path"), TjsValue::String("0, 1, 2".into()));
        assert_eq!(env.eval_ok("w.tagname"), TjsValue::String("wait".into()));
        // the property round-trips and defaults to off
        assert_eq!(env.eval_ok("p.multiLineTagEnabled"), TjsValue::Integer(1));
        env.exec_ok("var q = new KAGParser(); var qd = q.multiLineTagEnabled;");
        assert_eq!(env.eval_ok("qd"), TjsValue::Integer(0));
        // without the property the continuation line stays a comment and
        // `path` is lost
        env.exec_ok(
            "var r = new KAGParser(); r.ignoreCR = true; r.loadScenario('test.ams'); \
             var rm = r.getNextTag(); var rpath = rm.path;",
        );
        assert_eq!(
            env.eval_ok("(rpath === null) || (rpath === void)"),
            TjsValue::Integer(1)
        );
    }

    #[test]
    fn on_label_fires_on_script_subclass() {
        let _vm_lock = vm_lock();
        // AnimationSequenceController.onLabel reads the page name to decide
        // fixed vs volatile caching for `*attribute|...` labels.
        let env = TestEnv::new(
            "onlabel",
            &[ks("test.ks", "*attribute|fixed\n@x a=1\n*loop\n@y b=2\n")],
        );
        env.exec_ok(
            "var labels = []; var pages = []; \
             class C extends KAGParser { \
                 function C() { super.KAGParser(); ignoreCR = true; } \
                 function onLabel(l, p) { labels.add(l); pages.add(p); } \
             } \
             var c = new C(); c.loadScenario('test.ks'); \
             while (c.getNextTag() !== void) {}",
        );
        assert_eq!(env.eval_ok("labels.count"), TjsValue::Integer(2));
        assert_eq!(
            env.eval_ok("labels[0]"),
            TjsValue::String("*attribute".into())
        );
        assert_eq!(env.eval_ok("pages[0]"), TjsValue::String("fixed".into()));
        assert_eq!(env.eval_ok("labels[1]"), TjsValue::String("*loop".into()));
        // a label without a page passes void as the second argument
        assert_eq!(env.eval_ok("pages[1] === void"), TjsValue::Integer(1));
        // a base KAGParser has no onLabel; the walk must still work
        env.exec_ok(
            "var p = new KAGParser(); p.loadScenario('test.ks'); \
             var n = 0; while (p.getNextTag() !== void) n++;",
        );
        assert_eq!(env.eval_ok("n > 0"), TjsValue::Integer(1));
    }
}
