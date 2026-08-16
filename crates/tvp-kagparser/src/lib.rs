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
//! The KAGParserEx additions (`getRawTag`, `getRawTagCount`, `getTag`,
//! `isTagAvailable`, `getParameter`) are **not** in the reference
//! KAGParser and the game does not use them; they are not implemented.

use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;
use std::sync::{Arc, Mutex};

use engine::Storage;
use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, Tjs2Engine, TjsValue, VAL_INTEGER, VAL_REAL,
    VAL_STRING, VAL_VOID, Value,
};
use tvp_util::encoding;

pub mod kvp;
pub mod shim;
pub mod state;

use state::{Environ, EvalResult, KagParserState};

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

thread_local! {
    static CONTEXT: RefCell<Context> = const {
        RefCell::new(Context { engine: None, storage: None })
    };
}

/// Point the `KAGParser` native at the running VM and storage (mirrors
/// `tvp_scripts::set_context`; call before executing `startup.tjs`).
pub fn set_context(engine: Option<Arc<Tjs2Engine>>, storage: Option<Arc<Mutex<Storage>>>) {
    CONTEXT.with(|c| *c.borrow_mut() = Context { engine, storage });
}

fn context_engine() -> Result<Arc<Tjs2Engine>, String> {
    CONTEXT.with(|c| {
        c.borrow().engine.clone().ok_or_else(|| {
            "KAGParser context is not set: set_context(engine, storage) must be called".into()
        })
    })
}

fn context_storage() -> Result<Arc<Mutex<Storage>>, String> {
    CONTEXT.with(|c| {
        c.borrow().storage.clone().ok_or_else(|| {
            "KAGParser context is not set: set_context(engine, storage) must be called".into()
        })
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

/// Build the `Environ` (eval + storage callbacks) from the context. The
/// closures own their `Arc`s so the VM and storage stay alive across the
/// re-entrant calls they make.
struct ContextEnv {
    _engine: Arc<Tjs2Engine>,
    eval_closure: EvalClosure,
    load_closure: LoadClosure,
}

impl ContextEnv {
    fn new() -> Result<Self, String> {
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
                    Ok(TjsValue::Object) => {
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
        Ok(ContextEnv {
            _engine: engine,
            eval_closure,
            load_closure,
        })
    }

    fn environ(&mut self) -> Environ<'_> {
        Environ {
            eval: &mut *self.eval_closure,
            load_storage: &mut *self.load_closure,
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
    _objthis: *mut c_void,
) -> c_int {
    let st = unsafe { state_of(instance) };
    let mut ctx = match ContextEnv::new() {
        Ok(v) => v,
        Err(e) => return error_out(out_error, &e),
    };
    let result = st.next_tag(&mut ctx.environ());
    match result {
        Ok(Some((name, params))) => {
            set_string_result(out, &kvp::encode_tag(&name, &params));
            0
        }
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
extern "C" fn native_get_macros(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let s = unsafe { state_of(instance) }.get_macros();
    set_string_result(out, &s);
    0
}

/// `setMacros(dictString)`: replace the macro dictionary.
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
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    match unsafe { state_of(instance) }.get_macro_params() {
        Some(s) => {
            set_string_result(out, &s);
            0
        }
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
        properties: vec![],
    })
}

// ---------------------------------------------------------------------------
// tests (headless: temp game dir + real TJS2 VM + real Storage)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
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
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("ch\ntext=H".into())
        );
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("ch\ntext=e".into())
        );
        // Text runs are emitted one `ch` tag per character (the reference
        // `_GetNextTag` normal-character branch), so the third and fourth
        // tags are the remaining 'l' characters of "Hello".
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("ch\ntext=l".into())
        );
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("ch\ntext=l".into())
        );
        // ... walk to the end and assert the full sequence. The first four
        // `ch` tags (H, e, l, l) were already consumed above, so the
        // walked remainder starts with the 'o' and the space of "Hello ".
        env.exec_ok("var w = []; var wt; while ((wt = p.getNextTag()) !== void) w.add(wt);");
        let joined = env.eval_ok("w.join(\"\\x1e\")");
        let TjsValue::String(joined) = joined else {
            panic!("expected string")
        };
        let tags: Vec<&str> = joined.split('\x1e').collect();
        let names: Vec<&str> = tags.iter().map(|t| t.lines().next().unwrap()).collect();
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
        // wait tag carries the bare-attr "1000"="true"
        assert!(tags.contains(&"wait\n1000=true"));
        // bg tag params in source order
        assert!(tags.contains(&"bg\nstorage=chapter1/f01.jpg\neffect=0"));
        // end of scenario → void
        assert_eq!(
            env.eval_ok("(new KAGParser()).getNextTag()"),
            TjsValue::Void
        );
    }

    #[test]
    fn go_to_label_and_jump_loop_from_script() {
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
                 if (t !== void && t.indexOf('wait') == 0) waits++; } \
             var depth = p.getCallStackDepth();",
        );
        assert_eq!(env.eval_ok("waits"), TjsValue::Integer(2));
        assert_eq!(env.eval_ok("depth"), TjsValue::Integer(0));
        assert_eq!(
            env.eval_ok("seen[0]"),
            TjsValue::String("ch\ntext=O".into())
        );
        // goToLabel to a missing label is a catchable TJS error
        env.exec_ok(
            "var caught = ''; try { p.goToLabel('*nope'); } catch(e) { caught = 'missing'; }",
        );
        assert_eq!(env.eval_ok("caught"), TjsValue::String("missing".into()));
    }

    #[test]
    fn call_label_and_return_tag_from_script() {
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
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("ch\ntext=S".into())
        );
        // @return pops back to the caller (line after the callLabel)
        env.exec_ok("while (p.getNextTag() !== void) {}");
        assert_eq!(env.eval_ok("p.getCallStackDepth()"), TjsValue::Integer(0));
        // a bare @return with no call is a catchable error
        env.exec_ok("var q = new KAGParser(); q.loadScenario('test.ks');");
        env.exec_ok(
            "var caught = ''; try { q.goToLabel('*sub'); while (q.getNextTag() !== void) {} } \
             catch(e) { caught = 'underflow'; }",
        );
        assert_eq!(env.eval_ok("caught"), TjsValue::String("underflow".into()));
    }

    #[test]
    fn load_failure_is_a_catchable_tjs_error() {
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
        let env = TestEnv::new(
            "macro",
            &[ks(
                "test.ks",
                "[macro name=bgm]\n[bgmplay storage=%storage loop=true]\n[endmacro]\n@bgm storage=\"01.mp3\"\n",
            )],
        );
        env.exec_ok(
            "var p = new KAGParser(); p.loadScenario('test.ks'); \
             var r = []; var t; \
             while ((t = p.getNextTag()) !== void) r.add(t); \
             var macrosStr = p.getMacros();",
        );
        // the macro body round-trips: expansion emitted bgmplay with the
        // resolved %storage and the flag
        let joined = env.eval_ok("r.join(\"\\x1e\")");
        let TjsValue::String(joined) = joined else {
            panic!()
        };
        assert!(
            joined.contains("bgmplay\nstorage=01.mp3\nloop=true"),
            "{joined}"
        );
        // the recorded body contains the %-arg and the auto [macropop]
        let TjsValue::String(macros) = env.eval_ok("macrosStr") else {
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
        assert_eq!(env.eval_ok("r[2]"), TjsValue::String("move\nmy=10".into()));
        assert_eq!(env.eval_ok("r[4]"), TjsValue::String("move\nmy=99".into()));
        // macroParams returns the top macro-args dict (empty here)
        assert_eq!(env.eval_ok("p.getMacroParams()"), TjsValue::Void);
    }

    #[test]
    fn if_endif_evaluates_expressions_in_the_vm() {
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
             var r = []; var t; \
             while ((t = p.getNextTag()) !== void) r.add(t);",
        );
        let joined = env.eval_ok("r.join(\"\\x1e\")");
        let TjsValue::String(joined) = joined else {
            panic!()
        };
        assert!(joined.contains("onflag\nid=201"), "{joined}");
        assert!(!joined.contains("id=202"), "{joined}");
        // if-true → x emitted, else branch skipped; y must not appear
        assert!(joined.contains("x\na=1"), "{joined}");
        assert!(!joined.contains("y\nb=2"), "{joined}");
    }

    #[test]
    fn store_restore_round_trip_from_script() {
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
        // finish the scenario
        env.exec_ok(
            "var rest1 = []; var t; \
             while ((t = p.getNextTag()) !== void) rest1.add(t);",
        );
        // restore and replay the remainder — identical sequence
        env.exec_ok("p.restore(saved);");
        env.exec_ok(
            "var rest2 = []; var t2; \
             while ((t2 = p.getNextTag()) !== void) rest2.add(t2);",
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
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("ch\ntext=こ".into())
        );

        // CP932 without BOM (legacy Japanese)
        let body = tvp_util::encoding::Encoding::Cp932
            .encode("*start\nこんにちは\n@wait\n")
            .expect("encode cp932");
        let env = TestEnv::new("cp932", &[("j.ks", body)]);
        env.exec_ok("var p = new KAGParser(); p.loadScenario('j.ks');");
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("ch\ntext=こ".into())
        );
    }

    #[test]
    fn interrupt_returns_an_interrupt_tag() {
        let env = TestEnv::new("interrupt", &[ks("test.ks", "*start\nHello\n")]);
        env.exec_ok("var p = new KAGParser(); p.loadScenario('test.ks');");
        env.exec_ok("p.interrupt();");
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("interrupt".into())
        );
        // the walk continues normally afterwards
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("ch\ntext=H".into())
        );
        env.exec_ok("p.resetInterrupt(); p.interrupt(); p.resetInterrupt();");
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("ch\ntext=e".into())
        );
    }

    #[test]
    fn shim_wrapper_restores_dictionary_and_property_surface() {
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
        // macros is a raw "k=v\n..." string (no for-in in this TJS2 fork,
        // so Dictionary round-trips are unsupported)
        env.exec_ok(
            "var p3 = new KAGParserCompat(); p3.loadScenario('test.ks'); \
             p3.macros = 'foo=bar'; var m2 = p3.macros;",
        );
        assert_eq!(env.eval_ok("m2"), TjsValue::String("foo=bar".into()));
    }

    #[test]
    fn extending_the_native_class_is_an_abi_limitation() {
        // The game's `class ScController extends KAGParser` +
        // `super.KAGParser()` needs a constructor member that creates the
        // native instance; the C ABI registers methods only, so the super
        // constructor call fails. Documented limitation (the wrapper class
        // from shim::INSTALL_WRAPPER is the supported pattern).
        let env = TestEnv::new("extends", &[]);
        env.exec_ok(
            "var ok = false; \
             try { \
                 class X extends KAGParser { function X() { super.KAGParser(); } } \
                 var x = new X(); \
                 ok = true; \
             } catch(e) { }",
        );
        assert_eq!(env.eval_ok("ok"), TjsValue::Integer(0));
    }

    #[test]
    fn load_scenario_rewinds_and_clear_resets() {
        let env = TestEnv::new("rewind", &[ks("test.ks", "*start\nOne\nTwo\n")]);
        env.exec_ok("var p = new KAGParser(); p.loadScenario('test.ks'); p.getNextTag();");
        // the second call returns 'n' (the second character of "One": the
        // reference emits one `ch` tag per character)
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("ch\ntext=n".into())
        );
        // re-loading the same storage rewinds to the start
        env.exec_ok("p.loadScenario('test.ks');");
        assert_eq!(
            env.eval_ok("p.getNextTag()"),
            TjsValue::String("ch\ntext=O".into())
        );
        // clear() unloads: getNextTag returns void immediately
        env.exec_ok("p.clear();");
        assert_eq!(env.eval_ok("p.getNextTag()"), TjsValue::Void);
        assert_eq!(
            env.eval_ok("p.getCurStorage()"),
            TjsValue::String("".into())
        );
    }
}
