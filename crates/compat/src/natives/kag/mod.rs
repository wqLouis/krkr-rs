//! `KAGParser` native class and the `.ks` scenario parser.
//!
//! Behavioral reference: `crates/tvp-kagparser/src/*` and the pure
//! `crates/kag` parser. The [`parser`] half is a dependency-free transform
//! (no VM, no engine); [`state`] is the pull state machine; [`kvp`] holds
//! the string codecs used by macro storage and `store`/`restore`; this
//! module is the thin native adapter that registers `KAGParser` on the VM
//! and marshals script values.
//!
//! # Object results
//!
//! The reference `getNextTag` returns a *string encoding* of the
//! tag dictionary. `tjs` can retain an object and the C++ trampoline consumes a
//! `VAL_RETAINED` result, so `getNextTag` returns a **real TJS
//! Dictionary** (`tagname` plus every attribute plus `taglist`, an array of
//! member names in source order, matching the reference's `ArgValue`). The
//! dictionary is created with `%[]` and populated through
//! `Tjs2Engine::set_member`, so no scenario text is evaluated as TJS — only
//! the `taglist` array literal is built from escaped attribute names.
//! `macros`, `macroParams` and `paramMacros` likewise return real
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
#![allow(unsafe_code)]

mod kvp;
mod parser;
mod state;

// `state` is compiled standalone in `tests/kag.rs` and uses only `super::`
// paths, so expose the shared number coercion under a `kag::` name.
pub(crate) use crate::natives::tjsnum;

use std::ffi::{CStr, c_char, c_int, c_void};
use std::ptr;

use engine::Storage;
use tjs::{
    DetachedValue, Engine, NativeInstanceBuilder, NativeInstanceMethodDef,
    NativeInstancePropertyDef, RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue, VAL_INTEGER,
    VAL_OBJECT, VAL_REAL, VAL_RETAINED, VAL_STRING, VAL_VOID, Value,
};

use crate::binding;
use crate::native_util::{args, report_error, set_int, set_void};
use crate::natives::system::{tjs_string_to_integer, try_value_as_i64, value_as_bool};
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError, CompatServices};
use state::{
    Environ, EvalResult, KagParserState, LabelCallback, ScenarioLoadCallback,
    ScenarioLoadedCallback, ScriptCallback, TagCallback,
};

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
/// A boxed owner `onScript(script, storageShortName, scriptStart)` callback.
type ScriptClosure = Box<dyn FnMut(&str, &str, usize)>;
/// A boxed owner `onJump`/`onCall`/`onReturn` veto callback.
type TagClosure = Box<dyn FnMut(&str, &[(String, String)]) -> bool>;
/// A boxed owner `onScenarioLoad(name)` callback.
type ScenarioLoadClosure = Box<dyn FnMut(&str) -> Option<String>>;
/// A boxed owner `onScenarioLoaded(name)` callback.
type ScenarioLoadedClosure = Box<dyn FnMut(&str)>;

/// Reborrow a boxed label closure as a `dyn FnMut` with the borrow's own
/// object lifetime (`Environ<'a>` is invariant over `'a`).
fn shorten_label_closure<'a>(c: &'a mut LabelClosure) -> LabelCallback<'a> {
    &mut **c
}

/// Reborrow a boxed `onScript` closure (see [`shorten_label_closure`]).
fn shorten_script_closure<'a>(c: &'a mut ScriptClosure) -> ScriptCallback<'a> {
    &mut **c
}

/// Reborrow a boxed tag veto closure (see [`shorten_label_closure`]).
fn shorten_tag_closure<'a>(c: &'a mut TagClosure) -> TagCallback<'a> {
    &mut **c
}

/// Reborrow a boxed `onScenarioLoad` closure (see [`shorten_label_closure`]).
fn shorten_scenario_load_closure<'a>(c: &'a mut ScenarioLoadClosure) -> ScenarioLoadCallback<'a> {
    &mut **c
}

/// Reborrow a boxed `onScenarioLoaded` closure (see [`shorten_label_closure`]).
fn shorten_scenario_loaded_closure<'a>(
    c: &'a mut ScenarioLoadedClosure,
) -> ScenarioLoadedCallback<'a> {
    &mut **c
}

/// The injectable environment [`KagParserState`] walks in: TJS expression
/// evaluation through the bound VM, scenario loading through the attached
/// [`engine::Storage`], and the optional owner event callbacks.
struct KagEnv {
    eval: EvalFn,
    load: LoadFn,
    label: Option<LabelClosure>,
    script: Option<ScriptClosure>,
    jump: Option<TagClosure>,
    call: Option<TagClosure>,
    return_: Option<TagClosure>,
    scenario_load: Option<ScenarioLoadClosure>,
    scenario_loaded: Option<ScenarioLoadedClosure>,
}

/// A TJS closure that yields an object's string form (`'' + v`, the VM's
/// `TJSObjectToString` path).
const OBJECT_TO_STRING_SRC: &str = "(function(v){ return '' + v; })";

/// A TJS closure returning the `count` of a dictionary member (a
/// `[name, value, ...]` array). The member is read and counted in TJS so the
/// array's own receiver is used; an array cannot cross the ABI as a Rust
/// value without its `count` lookup resolving against the wrong receiver.
const PARAM_MACRO_COUNT_SRC: &str = "(function(d,k){ var a=d[k]; return a.count; })";

/// A TJS closure returning one element of a dictionary member array. Like
/// [`PARAM_MACRO_COUNT_SRC`] the array is indexed in TJS, so only a scalar
/// crosses the ABI.
const PARAM_MACRO_ELEMENT_SRC: &str = "(function(d,k,i){ var a=d[k]; return a[i]; })";

/// The string form of an already-evaluated TJS object. The object is passed
/// to a helper closure as a retained argument, so the expression that produced
/// it is never evaluated a second time.
fn object_to_string(engine: &Tjs2Engine, object: &DetachedValue) -> Result<String, String> {
    let helper = match engine.eval_retained(OBJECT_TO_STRING_SRC, "KAGParser.to_string") {
        Ok(RetainedValue::Object(helper)) => helper,
        Ok(RetainedValue::Value(value)) => {
            return Err(format!("KAGParser: object-to-string helper is {value:?}"));
        }
        Err(error) => return Err(error.to_string()),
    };
    let argument = engine.duplicate_retained(object.raw_id())?;
    match engine.call_detached(
        &helper,
        &[TjsValue::Retained(argument.raw_id().as_ptr() as u64)],
    ) {
        Ok(TjsValue::String(text)) => Ok(text),
        Ok(other) => Err(format!("KAGParser: object string form is {other:?}")),
        Err(error) => Err(error),
    }
}

impl KagEnv {
    fn new(engine: &'static Tjs2Engine, storage: Option<*mut Storage>, owner: *mut c_void) -> Self {
        let eval: EvalFn = Box::new(move |exp: &str| -> Result<EvalResult, String> {
            // Evaluate the expression exactly once; the object arm carries
            // everything later consumers need, so a side effect cannot run
            // twice.
            let value = engine
                .eval_retained(exp, "KAGParser")
                .map_err(|error| error.to_string())?;
            match value {
                RetainedValue::Value(TjsValue::Void) => Ok(EvalResult::Void),
                RetainedValue::Value(TjsValue::Integer(i)) => Ok(EvalResult::Integer(i)),
                RetainedValue::Value(TjsValue::Real(r)) => Ok(EvalResult::Real(r)),
                RetainedValue::Value(TjsValue::String(s)) => Ok(EvalResult::Str(s)),
                RetainedValue::Value(TjsValue::Object | TjsValue::Retained(_)) => {
                    Err("KAGParser: eval produced an unretained object".to_string())
                }
                RetainedValue::Object(object) => {
                    // An object is stringified on the string path and tested
                    // with `operator bool` (`Object != nullptr`) on the
                    // condition path. `class_names` fails for TJS `null` (an
                    // object with no dispatch target), so it tells null apart
                    // from a real object.
                    let truthy = engine.class_names(object.raw_id()).is_ok();
                    let text = object_to_string(engine, &object)?;
                    Ok(EvalResult::Object { text, truthy })
                }
            }
        });
        let load: LoadFn = Box::new(move |name: &str| {
            let Some(ptr) = storage else {
                return Err("KAGParser: no storage is attached to the compat services".to_string());
            };
            // SAFETY: the host keeps an attached storage alive for the whole
            // VM lifetime (see `CompatServices::attach_storage`).
            let bytes = unsafe { &*ptr }
                .read(strip_file_media(name))
                .map_err(|e| e.to_string())?;
            crate::text::decode_text(&bytes, crate::text::TextEncoding::Detect)
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
        let script: Option<ScriptClosure> = if owner.is_null() {
            None
        } else {
            Some(Box::new(
                move |script: &str, storage: &str, start: usize| {
                    let Ok(id) = engine.retain_object_detached(owner) else {
                        return;
                    };
                    let arguments = [
                        TjsValue::String(script.to_string()),
                        TjsValue::String(storage.to_string()),
                        TjsValue::Integer(start as i64),
                    ];
                    // A missing `onScript` or a handler error is ignored
                    // (`KAGParser.cpp:1163-1170`).
                    let _ = engine.call_member(id.raw_id(), "onScript", &arguments);
                },
            ))
        };
        let jump = owner_tag_closure(engine, owner, "onJump");
        let call = owner_tag_closure(engine, owner, "onCall");
        let return_ = owner_tag_closure(engine, owner, "onReturn");
        let scenario_load: Option<ScenarioLoadClosure> = if owner.is_null() {
            None
        } else {
            Some(Box::new(move |name: &str| -> Option<String> {
                let Ok(id) = engine.retain_object_detached(owner) else {
                    return None;
                };
                // `onScenarioLoad` overrides the scenario only when it
                // returns a string (`KAGParser.cpp:925-932`).
                match engine.call_member(
                    id.raw_id(),
                    "onScenarioLoad",
                    &[TjsValue::String(name.to_string())],
                ) {
                    Ok(TjsValue::String(text)) => Some(text),
                    _ => None,
                }
            }))
        };
        let scenario_loaded: Option<ScenarioLoadedClosure> = if owner.is_null() {
            None
        } else {
            Some(Box::new(move |name: &str| {
                let Ok(id) = engine.retain_object_detached(owner) else {
                    return;
                };
                let _ = engine.call_member(
                    id.raw_id(),
                    "onScenarioLoaded",
                    &[TjsValue::String(name.to_string())],
                );
            }))
        };
        Self {
            eval,
            load,
            label,
            script,
            jump,
            call,
            return_,
            scenario_load,
            scenario_loaded,
        }
    }

    fn environ(&mut self) -> Environ<'_> {
        let fire_label = self.label.as_mut().map(shorten_label_closure);
        let fire_script = self.script.as_mut().map(shorten_script_closure);
        let fire_jump = self.jump.as_mut().map(shorten_tag_closure);
        let fire_call = self.call.as_mut().map(shorten_tag_closure);
        let fire_return = self.return_.as_mut().map(shorten_tag_closure);
        let fire_scenario_load = self
            .scenario_load
            .as_mut()
            .map(shorten_scenario_load_closure);
        let fire_scenario_loaded = self
            .scenario_loaded
            .as_mut()
            .map(shorten_scenario_loaded_closure);
        Environ {
            eval: &mut *self.eval,
            load_storage: &mut *self.load,
            fire_label,
            fire_script,
            fire_jump,
            fire_call,
            fire_return,
            fire_scenario_load,
            fire_scenario_loaded,
        }
    }
}

/// Build an owner veto closure for `member` (`onJump`/`onCall`/`onReturn`).
/// A missing handler or any error leaves the tag processed, matching the
/// reference's `process = true` default.
fn owner_tag_closure(
    engine: &'static Tjs2Engine,
    owner: *mut c_void,
    member: &'static str,
) -> Option<TagClosure> {
    if owner.is_null() {
        return None;
    }
    Some(Box::new(
        move |name: &str, params: &[(String, String)]| -> bool {
            let Ok(id) = engine.retain_object_detached(owner) else {
                return true;
            };
            let Ok(dict) = build_tag_dict(engine, name, params) else {
                return true;
            };
            match engine.call_member(
                id.raw_id(),
                member,
                &[TjsValue::Retained(dict.raw_id().as_ptr() as u64)],
            ) {
                Ok(value) => tjs_value_truthy(&value),
                Err(_) => true,
            }
        },
    ))
}

/// TJS `operator bool` coercion for a callback result
/// (`tjs2/tjsVariant.h:913-935`): strings use `AsInteger() != 0`
/// (`tjs2/tjsVariant.cpp:301-307`), so `"0.5"` and a bare `"true"` flag
/// convert as their integer value.
fn tjs_value_truthy(value: &TjsValue) -> bool {
    match value {
        TjsValue::Void => false,
        TjsValue::Integer(i) => *i != 0,
        TjsValue::Real(r) => *r != 0.0,
        TjsValue::String(s) => tjs_string_to_integer(s) != 0,
        TjsValue::Object | TjsValue::Retained(_) => true,
    }
}

/// Build the walk environment for a callback, resolving the VM/storage.
fn make_env(engine: *mut c_void, owner: *mut c_void) -> Result<KagEnv, String> {
    let host = resolve_host(engine)?;
    let storage = resolve_storage(engine)?;
    Ok(KagEnv::new(host, storage, owner))
}

/// Synchronize the live `macros` dictionary with the parser's macro table.
///
/// The reference keeps one `Macros` dictionary that scripts and
/// `@macro`/`@erasemacro` both mutate and the walk reads
/// (`KAGParser.cpp:2471-2481`). Our walk state owns the table, so:
/// * when the state changed since the last publish, rebuild the cached
///   dictionary from it;
/// * otherwise read the cached dictionary back, picking up script
///   mutations.
fn sync_macros(host: &Tjs2Engine, instance: &mut KagParserInstance) -> Result<(), String> {
    let Some(dict_id) = instance.macros_dict.as_ref().map(DetachedValue::raw_id) else {
        return Ok(());
    };
    let state_entries = instance.state.macros_entries();
    if state_entries != instance.macros_snapshot {
        return publish_cached_dict(
            host,
            &mut instance.macros_dict,
            &mut instance.macros_snapshot,
            &state_entries,
        );
    }
    let names = host.enum_members(dict_id)?;
    let mut entries: Vec<(String, String)> = Vec::with_capacity(names.len());
    for name in names {
        let body = scalar_member(host, dict_id, &name).unwrap_or_default();
        entries.push((name, body));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    if entries != instance.macros_snapshot {
        instance.state.set_macros(entries.clone());
        instance.macros_snapshot = entries;
    }
    Ok(())
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
            (*out).retained = object.raw_id().as_ptr() as usize;
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
fn build_tag_dict(
    engine: &Tjs2Engine,
    name: &str,
    params: &[(String, String)],
) -> Result<DetachedValue, String> {
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
        &TjsValue::Retained(taglist.raw_id().as_ptr() as u64),
    )?;
    Ok(dict)
}

/// Write a tag dictionary to a native result slot.
fn set_tag_result(
    engine: &Tjs2Engine,
    out: *mut Value,
    name: &str,
    params: &[(String, String)],
) -> Result<(), String> {
    let dict = build_tag_dict(engine, name, params)?;
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

/// Convert a native argument to a boolean like the reference
/// `param->operator bool()` (`tjsVariant.h:913-935`): strings are
/// `AsInteger() != 0`, void and `null` are false, objects are `Object !=
/// nullptr`. The reference conversion never fails, so neither does this.
fn param_to_bool(v: &Value) -> bool {
    value_as_bool(v)
}

// ---------------------------------------------------------------------------
// instance payload
// ---------------------------------------------------------------------------

/// The native instance payload: the parser state plus the cached `macros` /
/// `macroParams` dictionary objects.
///
/// The reference hands the *same* dictionary object out on every read
/// (`GetMacrosNoAddRef` / `GetMacroTopNoAddRef`, `KAGParser.cpp:2471-2508`)
/// and releases those dictionaries at `Invalidate`
/// (`KAGParser.cpp:373-391`), so the cache lives with the payload and the
/// `invalidate` hook drops it.
struct KagParserInstance {
    state: KagParserState,
    /// The cached `macros` dictionary (identity-stable while unchanged).
    macros_dict: Option<DetachedValue>,
    /// The content last published through `macros_dict`.
    macros_snapshot: Vec<(String, String)>,
    /// The cached `macroParams`/`mp` dictionary (identity-stable while
    /// unchanged).
    macro_args_dict: Option<DetachedValue>,
    /// The content last published through `macro_args_dict`.
    macro_args_snapshot: Vec<(String, String)>,
}

impl KagParserInstance {
    fn new() -> Self {
        Self {
            state: KagParserState::default(),
            macros_dict: None,
            macros_snapshot: Vec::new(),
            macro_args_dict: None,
            macro_args_snapshot: Vec::new(),
        }
    }
}

extern "C" fn kagparser_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(KagParserInstance::new())) as *mut c_void
}

extern "C" fn kagparser_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: instance came from kagparser_create (Box::into_raw) and is
        // released exactly once by the VM.
        drop(unsafe { Box::from_raw(instance as *mut KagParserInstance) });
    }
}

/// The parser state for the object a method was called on.
///
/// # Safety
/// `instance` must be the live payload the VM passed to the callback.
unsafe fn state_of(instance: *mut c_void) -> &'static mut KagParserState {
    // SAFETY: the dispatcher verified the object is a KAGParser instance.
    &mut unsafe { &mut *(instance as *mut KagParserInstance) }.state
}

/// The full payload for the object a method was called on.
///
/// # Safety
/// `instance` must be the live payload the VM passed to the callback.
unsafe fn instance_of(instance: *mut c_void) -> &'static mut KagParserInstance {
    // SAFETY: the dispatcher verified the object is a KAGParser instance.
    unsafe { &mut *(instance as *mut KagParserInstance) }
}

/// Reference `tTJSNI_KAGParser::Invalidate` (`KAGParser.cpp:373-391`):
/// release the cached `Macros`/argument dictionaries, clear the macro-argument
/// stack and the scenario buffer, and drop `Owner`. This payload retains no
/// objects across calls, so clearing the parser state and the two cached
/// dictionaries releases everything the reference releases and leaves no
/// walk state to fire callbacks from.
extern "C" fn kagparser_invalidate(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let instance = unsafe { instance_of(instance) };
    instance.macros_dict = None;
    instance.macros_snapshot.clear();
    instance.macro_args_dict = None;
    instance.macro_args_snapshot.clear();
    instance.state.clear();
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
    objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "KAGParser.loadScenario requires 1 argument");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let name = match param_to_string(&unsafe { args(argv, argc) }[0]) {
        Ok(name) => name,
        Err(e) => return report_error(out_error, &e),
    };
    let mut env = match make_env(engine, objthis) {
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
    let instance = unsafe { instance_of(instance) };
    // Pick up script mutations of the live `macros` table before the walk.
    if let Err(e) = sync_macros(host, instance) {
        return report_error(out_error, &e);
    }
    match instance.state.next_tag(&mut env.environ()) {
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
    objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "KAGParser.callLabel requires 1 argument");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let name = match param_to_string(&unsafe { args(argv, argc) }[0]) {
        Ok(name) => name,
        Err(e) => return report_error(out_error, &e),
    };
    let mut env = match make_env(engine, objthis) {
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

/// Retain an object-valued member of a retained object (the reference
/// `PropGet` + variant copy). `member` records the object as the most
/// recent object result, which `retain_value_detached` resolves against.
fn retain_object_member(
    host: &Tjs2Engine,
    object: Tjs2ValueId,
    key: &str,
) -> Option<DetachedValue> {
    if !matches!(host.member(object, key), Ok(TjsValue::Object)) {
        return None;
    }
    host.retain_value_detached(&TjsValue::Object).ok()
}

/// A scalar member in its string form (reference `ttstr str = val;`).
fn scalar_member(host: &Tjs2Engine, object: Tjs2ValueId, key: &str) -> Option<String> {
    match host.member(object, key) {
        Ok(TjsValue::String(value)) => Some(value),
        Ok(TjsValue::Integer(value)) => Some(value.to_string()),
        Ok(TjsValue::Real(value)) => Some(value.to_string()),
        _ => None,
    }
}

/// A dictionary/array element count — the reference reads the `count` member
/// of `macroArgs` / `callStack` (`KAGParser.cpp:752`, `776`).
fn object_count(host: &Tjs2Engine, object: Tjs2ValueId) -> Result<i64, String> {
    match host.member(object, "count")? {
        TjsValue::Integer(count) => Ok(count),
        TjsValue::Real(count) => Ok(count as i64),
        other => Err(format!("KAGParser: count is not numeric ({other:?})")),
    }
}

/// `assign(src)` — copy another parser's state (reference
/// `_this->Assign(*src)` after the `NativeInstanceSupport(ClassID_KAGParser)`
/// check, `KAGParser.cpp:2273-2295`): a non-object or a non-`KAGParser`
/// object raises the reference `TVPKAGSpecifyKAGParser`
/// (`KAGParser.cpp:59-60`), and a live parser is copied wholesale
/// (`operator=`, `KAGParser.cpp:395-466`).
extern "C" fn native_assign(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "KAGParser.assign requires 1 argument");
    }
    let host = match resolve_host(engine) {
        Ok(host) => host,
        Err(e) => return report_error(out_error, &e),
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let value = &unsafe { args(argv, argc) }[0];
    // The bare reference `TVPKAGSpecifyKAGParser` text
    // (`KAGParser.cpp:59-60`).
    const UNSPECIFIED: &str = "Please specify KAGParser object.";
    if value.ty != VAL_OBJECT || value.object_handle().is_null() {
        return report_error(out_error, UNSPECIFIED);
    }
    let object = match host.retain_object_arg(value) {
        Ok(object) => object,
        Err(e) => return report_error(out_error, &e),
    };
    let payload = match host.native_instance_payload(object.raw_id(), "KAGParser") {
        Ok(Some(payload)) => payload,
        Ok(None) => return report_error(out_error, UNSPECIFIED),
        Err(e) => return report_error(out_error, &e),
    };
    // SAFETY: the payload is the engine-owned `KagParserInstance`
    // allocated by `kagparser_create`; it stays valid while `object` holds
    // the retained id and the single-threaded VM runs. Self-assignment
    // reads the source state before overwriting the destination, so it is
    // safe too.
    let source = unsafe { &*(payload.as_ptr() as *const KagParserInstance) };
    unsafe { state_of(instance) }.assign_from(&source.state);
    set_void(out);
    0
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

/// `store()` — serialize the parser state into a **dictionary** (reference
/// `tTJSNI_KAGParser::Store`, `KAGParser.cpp:469-636`): the reference
/// top-level members (`storageName`, `curLine`, `ExcludeLevel`, ... plus the
/// nested `macros`, `macroArgs` and `callStack` containers) and the lossless
/// emulator snapshot under `data`, which [`native_restore`] prefers for an
/// exact round-trip.
extern "C" fn native_store(
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
    let state = unsafe { state_of(instance) };
    match build_store_dict(host, state) {
        Ok(dict) => {
            set_retained_object(out, dict);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
}

/// Build the reference `Store()` dictionary for the current state.
fn build_store_dict(host: &Tjs2Engine, state: &KagParserState) -> Result<DetachedValue, String> {
    let dict = retained_object(host, "%[]")?;
    let raw = dict.raw_id();
    // The lossless emulator snapshot (preferred by `restore`).
    host.set_member(raw, "data", &TjsValue::String(state.store()))?;
    // Reference top-level members (`KAGParser.cpp:616-634` and the cur*
    // block below it).
    for (key, value) in [
        (
            "storageName",
            TjsValue::String(state.storage_name().to_string()),
        ),
        (
            "storageShortName",
            TjsValue::String(state.storage_short_name().to_string()),
        ),
        ("curLabel", TjsValue::String(state.cur_label().to_string())),
        ("curLine", TjsValue::Integer(state.cur_line() as i64)),
        ("curPos", TjsValue::Integer(state.cur_pos() as i64)),
        (
            "lineBuffer",
            TjsValue::String(state.line_buffer().to_string()),
        ),
        (
            "lineBufferUsing",
            TjsValue::Integer(state.line_buffer_using() as i64),
        ),
        (
            "ExcludeLevel",
            TjsValue::Integer(i64::from(state.exclude_level())),
        ),
        ("IfLevel", TjsValue::Integer(i64::from(state.if_level()))),
        (
            "ExcludeLevelStack",
            TjsValue::String(state::encode_int_stack(state.exclude_level_stack())),
        ),
        (
            "IfLevelExecutedStack",
            TjsValue::String(state::encode_bool_stack(state.if_level_executed_stack())),
        ),
        (
            "macroArgStackBase",
            TjsValue::Integer(state.macro_args_base() as i64),
        ),
        (
            "macroArgStackDepth",
            TjsValue::Integer(state.macro_args_depth() as i64),
        ),
    ] {
        host.set_member(raw, key, &value)?;
    }
    // `macros` dictionary (`KAGParser.cpp:506-520`).
    let macros = retain_dict(host, &state.macros_entries())?;
    host.set_member(
        raw,
        "macros",
        &TjsValue::Retained(macros.raw_id().as_ptr() as u64),
    )?;
    drop(macros);
    // `macroArgs`: an array of dictionaries (`KAGParser.cpp:521-544`).
    let levels = retained_object(host, "[]")?;
    for (index, level) in state.macro_args_entries().iter().enumerate() {
        let level_dict = retain_dict(host, level)?;
        host.set_member(
            levels.raw_id(),
            &index.to_string(),
            &TjsValue::Retained(level_dict.raw_id().as_ptr() as u64),
        )?;
        drop(level_dict);
    }
    host.set_member(
        raw,
        "macroArgs",
        &TjsValue::Retained(levels.raw_id().as_ptr() as u64),
    )?;
    drop(levels);
    // `callStack`: an array of dictionaries (`KAGParser.cpp:546-634`).
    let stack = retained_object(host, "[]")?;
    for (index, entry) in state.call_stack_snapshots().iter().enumerate() {
        let entry_dict = retained_object(host, "%[]")?;
        let entry_raw = entry_dict.raw_id();
        for (key, value) in [
            ("storage", TjsValue::String(entry.storage.clone())),
            ("label", TjsValue::String(entry.label.clone())),
            ("offset", TjsValue::Integer(entry.offset as i64)),
            ("orgLineStr", TjsValue::String(entry.org_line_str.clone())),
            ("lineBuffer", TjsValue::String(entry.line_buffer.clone())),
            (
                "lineBufferUsing",
                TjsValue::Integer(entry.line_buffer_using as i64),
            ),
            ("pos", TjsValue::Integer(entry.pos as i64)),
            (
                "macroArgStackBase",
                TjsValue::Integer(entry.macro_arg_stack_base as i64),
            ),
            (
                "macroArgStackDepth",
                TjsValue::Integer(entry.macro_arg_stack_depth as i64),
            ),
            (
                "ExcludeLevel",
                TjsValue::Integer(i64::from(entry.exclude_level)),
            ),
            ("IfLevel", TjsValue::Integer(i64::from(entry.if_level))),
            (
                "ExcludeLevelStack",
                TjsValue::String(entry.exclude_level_stack.clone()),
            ),
            (
                "IfLevelExecutedStack",
                TjsValue::String(entry.if_level_executed_stack.clone()),
            ),
        ] {
            host.set_member(entry_raw, key, &value)?;
        }
        host.set_member(
            stack.raw_id(),
            &index.to_string(),
            &TjsValue::Retained(entry_raw.as_ptr() as u64),
        )?;
        drop(entry_dict);
    }
    host.set_member(
        raw,
        "callStack",
        &TjsValue::Retained(stack.raw_id().as_ptr() as u64),
    )?;
    drop(stack);
    Ok(dict)
}

/// `restore(dict)` — restore from a `store()` dictionary (reference
/// `tTJSNI_KAGParser::Restore(iTJSDispatch2*)`, `KAGParser.cpp:723-906`).
/// The reference takes an **object**, raising on a non-object argument
/// (`AsObjectNoAddRef`, `tjsVariant.h:686-694`); our own snapshots round-trip
/// losslessly through the `data` member, and reference-format dictionaries
/// are imported through [`dict_to_entries`].
extern "C" fn native_restore(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "KAGParser.restore requires 1 argument");
    }
    let host = match resolve_host(engine) {
        Ok(host) => host,
        Err(e) => return report_error(out_error, &e),
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let value = &unsafe { args(argv, argc) }[0];
    if value.ty != VAL_OBJECT || value.object_handle().is_null() {
        return report_error(
            out_error,
            "KAGParser.restore: please specify the dictionary returned by store()",
        );
    }
    let object = match host.retain_object_arg(value) {
        Ok(object) => object,
        Err(e) => return report_error(out_error, &e),
    };
    let mut env = match make_env(engine, objthis) {
        Ok(env) => env,
        Err(e) => return report_error(out_error, &e),
    };
    let state = unsafe { state_of(instance) };
    match host.member(object.raw_id(), "data") {
        // The emulator's own snapshot: exact round-trip.
        Ok(TjsValue::String(snapshot)) => match state.restore(&snapshot, &mut env.environ()) {
            Ok(()) => {
                set_void(out);
                0
            }
            Err(e) => report_error(out_error, &e),
        },
        // A reference-format dictionary (or anything else): import it.
        _ => match dict_to_entries(host, object.raw_id()) {
            Ok(entries) => match state.restore_decoded(entries, &mut env.environ()) {
                Ok(()) => {
                    set_void(out);
                    0
                }
                Err(e) => report_error(out_error, &e),
            },
            Err(e) => report_error(out_error, &e),
        },
    }
}

/// Convert a reference-format save dictionary into the emulator's ordered
/// entries for `KagParserState::restore_decoded`, mapping the member names
/// the reference reads (`KAGParser.cpp:723-906`) onto the state's keys.
fn dict_to_entries(host: &Tjs2Engine, dict: Tjs2ValueId) -> Result<Vec<(String, String)>, String> {
    let mut entries: Vec<(String, String)> = Vec::new();
    let scalar = |src: &str, dst: &str, entries: &mut Vec<(String, String)>| {
        if let Some(value) = scalar_member(host, dict, src) {
            entries.push((dst.to_string(), value));
        }
    };
    // Top-level scalars (`KAGParser.cpp:857-906`), mapped onto the
    // emulator's walk state.
    scalar("storageName", "storageName", &mut entries);
    scalar("storageShortName", "storageShortName", &mut entries);
    scalar("curLabel", "curLabel", &mut entries);
    scalar("curLine", "curLine", &mut entries);
    scalar("curPos", "curPos", &mut entries);
    scalar("lineBuffer", "lineBuffer", &mut entries);
    scalar("lineBufferUsing", "lineBufferUsing", &mut entries);
    scalar("ignoreCR", "ignoreCR", &mut entries);
    scalar("processSpecialTags", "processSpecialTags", &mut entries);
    scalar("multiLineTagEnabled", "multiLineTagEnabled", &mut entries);
    scalar("debugLevel", "debugLevel", &mut entries);
    scalar("ExcludeLevel", "excludeLevel", &mut entries);
    scalar("IfLevel", "ifLevel", &mut entries);
    scalar("ExcludeLevelStack", "excludeLevelStack", &mut entries);
    scalar("IfLevelExecutedStack", "ifLevelExecutedStack", &mut entries);
    scalar("macroArgStackBase", "macroArgStackBase", &mut entries);

    // `macroArgs` (`KAGParser.cpp:740-769`), including the reference's
    // `TVPKAGMalformedSaveData` depth check (`KAGParser.cpp:51`).
    let explicit_depth =
        host.member(dict, "macroArgStackDepth")
            .ok()
            .and_then(|value| match value {
                TjsValue::Integer(v) => Some(v),
                TjsValue::Real(v) => Some(v as i64),
                _ => None,
            });
    if let Some(levels) = retain_object_member(host, dict, "macroArgs") {
        let count = object_count(host, levels.raw_id())?;
        if explicit_depth.unwrap_or(0) != count {
            return Err("Malformed savedata, data may damaged.".to_string());
        }
        entries.push(("macroArgStackDepth".to_string(), count.to_string()));
        for index in 0..count {
            let Some(level) = retain_object_member(host, levels.raw_id(), &index.to_string())
            else {
                continue;
            };
            for (ordinal, key) in host.enum_members(level.raw_id())?.iter().enumerate() {
                let value = scalar_member(host, level.raw_id(), key).unwrap_or_default();
                entries.push((format!("macroArgs.{index}.{ordinal}.name"), key.clone()));
                entries.push((format!("macroArgs.{index}.{ordinal}.value"), value));
            }
            drop(level);
        }
        drop(levels);
    }

    // `macros` (`KAGParser.cpp:727-735`).
    if let Some(macros) = retain_object_member(host, dict, "macros") {
        let names = host.enum_members(macros.raw_id())?;
        entries.push(("macrosCount".to_string(), names.len().to_string()));
        for name in &names {
            let body = scalar_member(host, macros.raw_id(), name).unwrap_or_default();
            entries.push((format!("macros.{}", kvp::escape_field(name)), body));
        }
        drop(macros);
    }

    // `callStack` (`KAGParser.cpp:774-855`): map the reference entry keys
    // onto the emulator's (`orgLineStr` → `orgSig`, `pos` → `curPos`,
    // `lineBuffer`/`lineBufferUsing` → the entry's expansion buffer).
    if let Some(stack) = retain_object_member(host, dict, "callStack") {
        let count = object_count(host, stack.raw_id())?;
        entries.push(("callStackCount".to_string(), count.to_string()));
        for index in 0..count {
            let Some(entry) = retain_object_member(host, stack.raw_id(), &index.to_string()) else {
                continue;
            };
            let prefix = format!("callStack.{index}.");
            for (src, dst) in [
                ("storage", "storage"),
                ("label", "label"),
                ("offset", "offset"),
                ("orgLineStr", "orgSig"),
                ("lineBuffer", "lineBuffer"),
                ("lineBufferUsing", "lineBufferUsing"),
                ("pos", "curPos"),
                ("macroArgStackBase", "macroArgStackBase"),
                ("macroArgStackDepth", "macroArgStackDepth"),
                ("ExcludeLevel", "excludeLevel"),
                ("IfLevel", "ifLevel"),
                ("ExcludeLevelStack", "excludeLevelStack"),
                ("IfLevelExecutedStack", "ifLevelExecutedStack"),
            ] {
                if let Some(value) = scalar_member(host, entry.raw_id(), src) {
                    entries.push((format!("{prefix}{dst}"), value));
                }
            }
            drop(entry);
        }
        drop(stack);
    }
    Ok(entries)
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

macro_rules! prop_get_int_field {
    ($name:ident, $field:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_int(out, unsafe { state_of(instance) }.$field as i64);
            0
        }
    };
}

macro_rules! prop_get_bool {
    ($name:ident, $field:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            out: *mut Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            set_int(out, i64::from(unsafe { state_of(instance) }.$field));
            0
        }
    };
}

macro_rules! prop_set_bool {
    ($name:ident, $field:ident) => {
        extern "C" fn $name(
            _engine: *mut c_void,
            instance: *mut c_void,
            value: *const Value,
            _out_error: *mut *mut c_char,
            _objthis: *mut c_void,
        ) -> c_int {
            // SAFETY: value follows the trampoline contract.
            let v = param_to_bool(unsafe { &*value });
            unsafe { state_of(instance) }.$field = v;
            0
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

/// Publish `entries` through a cached dictionary: reuse the cached object
/// while the content is unchanged — the reference hands the same dictionary
/// out on every read (`GetMacrosNoAddRef` / `GetMacroTopNoAddRef`,
/// `KAGParser.cpp:2474`, `2487-2501`) — and rebuild it when the content
/// changed. The reference mutates its dictionary in place instead, but a TJS
/// `Dictionary` cannot be cleared through an instance member (`clear` and
/// `assign` are `TJS_STATICMEMBER`s, `tjsDictionary.cpp:235-245`, `186-211`,
/// so instance lookup misses them — which is why the reference itself fetches
/// `DicClear` from the *class object* at `KAGParser.cpp:336-344`) and the C
/// ABI has no explicit-`objthis` invocation for the class-fetched method.
fn publish_cached_dict(
    host: &Tjs2Engine,
    cache: &mut Option<DetachedValue>,
    snapshot: &mut Vec<(String, String)>,
    entries: &[(String, String)],
) -> Result<(), String> {
    if cache.is_some() && snapshot.as_slice() == entries {
        return Ok(());
    }
    *cache = Some(retain_dict(host, entries)?);
    *snapshot = entries.to_vec();
    Ok(())
}

/// Write TJS `null` into `out` (the reference hands a null pointer through
/// `tTJSVariant(params, params)`, `KAGParser.cpp:2499-2501`).
fn set_null_result(host: &Tjs2Engine, out: *mut Value) {
    if out.is_null() {
        return;
    }
    match host.eval_retained("null", "KAGParser.null") {
        Ok(RetainedValue::Object(null)) => {
            set_retained_object(out, null);
        }
        _ => set_void(out),
    }
}

/// `getMacroParams()` — the top macro-args dictionary, or `null` when the
/// stack is empty (reference `GetMacroTopNoAddRef`, `KAGParser.cpp:2191-2196`
/// + the null `tTJSVariant` at `2499-2501`).
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
    let instance = unsafe { instance_of(instance) };
    let Some(entries) = instance.state.macro_params_entries() else {
        set_null_result(host, out);
        return 0;
    };
    if let Err(e) = publish_cached_dict(
        host,
        &mut instance.macro_args_dict,
        &mut instance.macro_args_snapshot,
        entries,
    ) {
        return report_error(out_error, &e);
    }
    let Some(dict) = instance.macro_args_dict.as_ref() else {
        set_void(out);
        return 0;
    };
    match host.duplicate_retained(dict.raw_id()) {
        Ok(duplicate) => set_retained_object(out, duplicate),
        Err(e) => return report_error(out_error, &e),
    }
    0
}

// ---------------------------------------------------------------------------
// native properties
// ---------------------------------------------------------------------------

prop_get_bool!(prop_ignore_cr_get, ignore_cr);
prop_set_bool!(prop_ignore_cr_set, ignore_cr);
prop_get_bool!(prop_process_special_tags_get, process_special_tags);
prop_set_bool!(prop_process_special_tags_set, process_special_tags);
prop_get_bool!(prop_multi_line_tag_enabled_get, multi_line_tag_enabled);
prop_set_bool!(prop_multi_line_tag_enabled_set, multi_line_tag_enabled);
prop_get_int_field!(prop_debug_level_get, debug_level);
prop_get_int!(prop_cur_line_get, cur_line);
prop_get_int!(prop_cur_pos_get, cur_pos);
prop_get_int!(prop_call_stack_depth_get, call_stack_depth);
prop_get_string!(prop_cur_line_str_get, cur_line_str);
prop_get_string!(prop_cur_storage_get, storage_name);
prop_get_string!(prop_cur_label_get, cur_label);

extern "C" fn prop_debug_level_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract.
    // `SetDebugLevel((tTVPKAGDebugLevel)(tjs_int)(*param))` uses the
    // reference `operator tjs_int` → `AsInteger` conversion, which truncates
    // reals and raises on objects (`tjsVariant.h:933-935`).
    let level = match try_value_as_i64(unsafe { &*value }) {
        Ok(level) => level as i32,
        Err(e) => return report_error(out_error, &e),
    };
    unsafe { state_of(instance) }.debug_level = level;
    0
}

extern "C" fn prop_cur_storage_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    // SAFETY: value follows the trampoline contract.
    let name = match param_to_string(unsafe { &*value }) {
        Ok(name) => name,
        Err(e) => return report_error(out_error, &e),
    };
    let mut env = match make_env(engine, objthis) {
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
    let host = match resolve_host(engine) {
        Ok(host) => host,
        Err(e) => return report_error(out_error, &e),
    };
    let instance = unsafe { instance_of(instance) };
    let entries = instance.state.macros_entries();
    if let Err(e) = publish_cached_dict(
        host,
        &mut instance.macros_dict,
        &mut instance.macros_snapshot,
        &entries,
    ) {
        return report_error(out_error, &e);
    }
    let Some(dict) = instance.macros_dict.as_ref() else {
        set_void(out);
        return 0;
    };
    match host.duplicate_retained(dict.raw_id()) {
        Ok(duplicate) => set_retained_object(out, duplicate),
        Err(e) => return report_error(out_error, &e),
    }
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

/// Build a `paramMacros` dictionary: macro name → the flattened
/// `[name, value, name, value, ...]` array the KAGParserEx readme documents
/// (`reference/cpp/plugins/KAGParser/readme.txt:51-55`).
fn retain_param_macros_dict(
    host: &Tjs2Engine,
    entries: &[(String, Vec<(String, String)>)],
) -> Result<DetachedValue, String> {
    let dict = retained_object(host, "%[]")?;
    for (name, list) in entries {
        let mut flat: Vec<String> = Vec::with_capacity(list.len() * 2);
        for (key, value) in list {
            flat.push(key.clone());
            flat.push(value.clone());
        }
        let array = retained_object(host, &array_literal(&flat))?;
        host.set_member(
            dict.raw_id(),
            name,
            &TjsValue::Retained(array.raw_id().as_ptr() as u64),
        )?;
    }
    Ok(dict)
}

/// Read a `paramMacros` dictionary back into the state's ordered entries.
/// Each value is the alternating `[name, value, ...]` array; a missing or
/// non-array value is an error (no silent fallback). The count and elements
/// are read through TJS helpers (see [`PARAM_MACRO_COUNT_SRC`] and
/// [`PARAM_MACRO_ELEMENT_SRC`]) so no object crosses the ABI.
/// One `paramMacros` entry: the macro name and its `[name, value, ...]` pairs.
type ParamMacroEntry = (String, Vec<(String, String)>);

fn read_param_macros_dict(
    host: &Tjs2Engine,
    dict: Tjs2ValueId,
) -> Result<Vec<ParamMacroEntry>, String> {
    let view = host.duplicate_retained(dict)?;
    let names = crate::natives::plugins::scripts_ex::data_member_names(host, &view)?;
    let count_helper = retained_object(host, PARAM_MACRO_COUNT_SRC)?;
    let element_helper = retained_object(host, PARAM_MACRO_ELEMENT_SRC)?;
    let mut entries = Vec::with_capacity(names.len());
    for name in names {
        let count = match call_param_macro_helper(host, &count_helper, dict, &name, None)? {
            TjsValue::Integer(value) => value,
            TjsValue::Real(value) => value as i64,
            other => {
                return Err(format!(
                    "KAGParser.paramMacros: {name} is not an array ({other:?})"
                ));
            }
        };
        if count < 0 {
            return Err(format!(
                "KAGParser.paramMacros: {name} has a negative parameter count"
            ));
        }
        if count % 2 != 0 {
            return Err(format!(
                "KAGParser.paramMacros: {name} has an odd-length parameter list"
            ));
        }
        let half = usize::try_from(count / 2)
            .map_err(|_| format!("KAGParser.paramMacros: {name} has too many parameters"))?;
        let mut list = Vec::with_capacity(half);
        let mut index = 0;
        while index < count {
            let key = scalar_param_macro_element(host, &element_helper, dict, &name, index)?;
            let value = scalar_param_macro_element(host, &element_helper, dict, &name, index + 1)?;
            list.push((key, value));
            index += 2;
        }
        entries.push((name, list));
    }
    Ok(entries)
}

/// Invoke a `paramMacros` reader helper with the dictionary and macro name
/// (and, for an element read, the array index). The dictionary is duplicated
/// so the call's argument conversion cannot invalidate the caller's handle
/// for the next iteration.
fn call_param_macro_helper(
    host: &Tjs2Engine,
    helper: &DetachedValue,
    dict: Tjs2ValueId,
    name: &str,
    index: Option<i64>,
) -> Result<TjsValue, String> {
    let dict_arg = host.duplicate_retained(dict)?;
    let mut arguments = vec![
        TjsValue::Retained(dict_arg.raw_id().as_ptr() as u64),
        TjsValue::String(name.to_string()),
    ];
    if let Some(index) = index {
        arguments.push(TjsValue::Integer(index));
    }
    host.call_detached(helper, &arguments)
}

/// Read one element of a `paramMacros` array through the element helper and
/// render it as the reference's `ttstr` conversion of a scalar.
fn scalar_param_macro_element(
    host: &Tjs2Engine,
    helper: &DetachedValue,
    dict: Tjs2ValueId,
    name: &str,
    index: i64,
) -> Result<String, String> {
    match call_param_macro_helper(host, helper, dict, name, Some(index))? {
        TjsValue::String(value) => Ok(value),
        TjsValue::Integer(value) => Ok(value.to_string()),
        TjsValue::Real(value) => Ok(value.to_string()),
        TjsValue::Void => Ok(String::new()),
        other => Err(format!(
            "KAGParser.paramMacros: {name}[{index}] is not a scalar ({other:?})"
        )),
    }
}

/// `paramMacros` getter (KAGParserEx). Returns the dictionary of registered
/// parameter macros.
extern "C" fn prop_param_macros_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let host = match resolve_host(engine) {
        Ok(host) => host,
        Err(e) => return report_error(out_error, &e),
    };
    let entries = unsafe { state_of(instance) }.param_macros_entries();
    match retain_param_macros_dict(host, &entries) {
        Ok(dict) => {
            set_retained_object(out, dict);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
}

/// `paramMacros` setter (KAGParserEx): replace the registered parameter
/// macros from a dictionary of `[name, value, ...]` arrays.
extern "C" fn prop_param_macros_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let host = match resolve_host(engine) {
        Ok(host) => host,
        Err(e) => return report_error(out_error, &e),
    };
    // SAFETY: value follows the trampoline contract.
    let value = unsafe { &*value };
    if value.ty != VAL_OBJECT || value.object_handle().is_null() {
        return report_error(
            out_error,
            "KAGParser.paramMacros: please specify a dictionary",
        );
    }
    let object = match host.retain_object_arg(value) {
        Ok(object) => object,
        Err(e) => return report_error(out_error, &e),
    };
    match read_param_macros_dict(host, object.raw_id()) {
        Ok(entries) => {
            unsafe { state_of(instance) }.set_param_macros(entries);
            0
        }
        Err(e) => report_error(out_error, &e),
    }
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
        invalidate: Some(kagparser_invalidate),
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
                // `TJS_DENY_NATIVE_PROP_SETTER` (`KAGParser.cpp:2482`).
                set: None,
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

#[cfg(test)]
mod tests {
    use super::tjs_value_truthy;
    use tjs::TjsValue;

    /// `tTJSVariant::operator bool` for a string is `AsInteger() != 0`
    /// (`reference/cpp/core/tjs2/tjsVariant.h:913-925`,
    /// `reference/cpp/core/tjs2/tjsVariant.cpp:301-307`), so `"0.5"` is false
    /// while `"true"` and `"12abc"` are true.
    #[test]
    fn truthy_strings_use_as_integer() {
        assert!(!tjs_value_truthy(&TjsValue::String("0.5".into())));
        assert!(!tjs_value_truthy(&TjsValue::String("false".into())));
        assert!(!tjs_value_truthy(&TjsValue::String("abc".into())));
        assert!(tjs_value_truthy(&TjsValue::String("true".into())));
        assert!(tjs_value_truthy(&TjsValue::String("12abc".into())));
    }
}
