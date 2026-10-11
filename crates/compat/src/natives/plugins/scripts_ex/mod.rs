//! `scripts_ex` plugin — full port of `reference/cpp/plugins/scriptsEx.cpp`.
//!
//! The reference attaches a set of object/collection helpers to the
//! `Scripts` class (most of them `TJS_STATICMEMBER`) and binds `rehash` to
//! the VM's `TJSDoRehash`. This port composes the L2 [`Tjs2Engine`] member
//! API (`member` / `set_member` / `call_member` / `retain_*`) rather than
//! reimplementing a script interpreter, and exposes the collection-oriented
//! helpers as static members of `Scripts` only, matching the reference.
//!
//! Script-visible surface (`Scripts.<name>(obj, ...)` static members):
//!
//! | member | behavior |
//! |---|---|
//! | `getObjectKeys(obj)` | member names as a sorted TJS `Array` |
//! | `getObjectCount(obj)` | member count (`Dictionary` counted by enumeration) |
//! | `getObjectContext(obj)` | the object's context (`ObjThis`) |
//! | `isNullContext(obj)` | whether that context is null |
//! | `equalStruct(a, b)` | recursive structural equality (`===` leaves) |
//! | `equalStructNumericLoose(a, b)` | as above but `0 == 0.0` |
//! | `foreach(obj, func, args*)` | iterate; a non-`void` return stops and is returned |
//! | `getMD5HashString(octet)` | lowercase hex MD5 of the octet |
//! | `clone(obj)` | deep copy of arrays/dictionaries; other objects are returned after invoking `clone()` |
//! | `propSet(obj, key, value[, flag])` | `PropSet`/`PropSetByNum` with `flag` (default `TJS_MEMBERENSURE`) |
//! | `propGet(obj, key[, flag])` | `PropGet`/`PropGetByNum` with `flag` (default `TJS_MEMBERMUSTEXIST`) |
//! | `safeEvalStorage(name[, mode[, context]])` | evaluate a `(const)[...]` storage and return element 0 |
//! | `pf*` constants | the reference's `TJS_*` member flags |
//! | `rehash()` | the VM's `TJSDoRehash` |
//!
//! The recursive helpers operate on retained object ids and read/write
//! members through [`Tjs2Engine`], so script subclass overrides win and no
//! TJS source is generated from script data.
#![allow(unsafe_code)]

use super::md5::md5_hex;

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use tjs::{
    DetachedValue, Engine, NativeMethodDef, NativeStaticMembers, RetainedValue, Tjs2Engine,
    TjsValue, VAL_INTEGER, VAL_NULL, VAL_OBJECT, VAL_OCTET, VAL_REAL, VAL_RETAINED, VAL_STRING,
    VAL_VOID, Value,
};

use crate::binding;
use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::natives::system::value_as_i64;
use crate::{CompatCtx, CompatError, CompatServices};

// ---------------------------------------------------------------------------
// reference flag constants (tjsInterface.h)
// ---------------------------------------------------------------------------

const TJS_MEMBERENSURE: i64 = 0x0000_0200;
const TJS_MEMBERMUSTEXIST: i64 = 0x0000_0400;
const TJS_IGNOREPROP: i64 = 0x0000_0800;
const TJS_HIDDENMEMBER: i64 = 0x0000_1000;
const TJS_STATICMEMBER: i64 = 0x0001_0000;

// The vendored VM exports `TJS::TJSDoRehash()` with Itanium C++ linkage; the
// C ABI has no entry for it, but it is part of the statically linked VM and
// is exactly the function the reference binds `rehash` to.
unsafe extern "C" {
    #[link_name = "_ZN3TJS11TJSDoRehashEv"]
    fn tjs_do_rehash();
}

// ---------------------------------------------------------------------------
// per-VM dispatch state
// ---------------------------------------------------------------------------

/// Reusable result scratch and lazily-built helper closures.
///
/// The C++ trampoline copies string/octet results immediately after the
/// callback returns, so the scratch only has to outlive the call. Helper
/// closures are retained on the engine (never released here: the engine's
/// retained map dies with the VM) so a deep `foreach`/`equalStruct` walk does
/// not re-evaluate them.
#[derive(Default)]
struct ScriptsExState {
    scratch: Vec<u8>,
    helpers: std::collections::HashMap<&'static str, tjs::Tjs2ValueId>,
}

// ---------------------------------------------------------------------------
// result marshalling
// ---------------------------------------------------------------------------

/// A TJS value with object identity preserved.
///
/// The safe [`TjsValue`] surface cannot carry an object handle (an
/// `OBJECT`-typed value is opaque), so recursive operations keep objects as
/// retained [`DetachedValue`]s.
enum Owned {
    Void,
    Null,
    Integer(i64),
    Real(f64),
    String(String),
    Octet(Vec<u8>),
    Object(DetachedValue),
}

impl Owned {
    /// Render this value as an argument for a member/function call, handing
    /// ownership of an object retention to the C++ side.
    fn to_tjs_for_call(&self, engine: &Tjs2Engine) -> Result<TjsValue, String> {
        match self {
            Owned::Void => Ok(TjsValue::Void),
            Owned::Integer(value) => Ok(TjsValue::Integer(*value)),
            Owned::Real(value) => Ok(TjsValue::Real(*value)),
            Owned::String(value) => Ok(TjsValue::String(value.clone())),
            Owned::Object(object) => {
                let duplicate = engine.duplicate_retained(object.raw_id())?;
                let id = duplicate.raw_id() as u64;
                // The C++ `value_to_variant` consumes the retention.
                std::mem::forget(duplicate);
                Ok(TjsValue::Retained(id))
            }
            Owned::Null => Err("scripts_ex: a null value cannot be passed across the ABI".into()),
            Owned::Octet(_) => {
                Err("scripts_ex: an octet cannot be passed as a member argument".into())
            }
        }
    }
}

/// Write a scalar into `out` using the shared scratch buffer.
fn set_string_out(services: &CompatServices, out: *mut Value, value: &str) {
    if out.is_null() {
        return;
    }
    let mut state = services.state::<ScriptsExState>();
    state.scratch.clear();
    state.scratch.extend_from_slice(value.as_bytes());
    state.scratch.push(0);
    let pointer = state.scratch.as_ptr() as *const c_char;
    drop(state);
    // SAFETY: `out` is a valid result slot and the scratch buffer outlives
    // the callback (the trampoline copies the string before it returns).
    unsafe {
        (*out).ty = VAL_STRING;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = pointer;
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Write raw bytes into `out` as a TJS octet.
fn set_octet_out(services: &CompatServices, out: *mut Value, bytes: &[u8]) {
    if out.is_null() {
        return;
    }
    let mut state = services.state::<ScriptsExState>();
    state.scratch.clear();
    state.scratch.extend_from_slice(bytes);
    let pointer = state.scratch.as_ptr() as *const c_char;
    let len = bytes.len() as c_int;
    drop(state);
    // SAFETY: `out` is a valid result slot and the scratch buffer outlives
    // the callback (the trampoline copies the octet before it returns).
    unsafe {
        (*out).ty = VAL_OCTET;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = pointer;
        (*out).array = ptr::null();
        (*out).array_count = len;
        (*out).retained = 0;
    }
}

/// Write a real into `out`.
fn set_real(out: *mut Value, value: f64) {
    if out.is_null() {
        return;
    }
    // SAFETY: `out` is a valid result slot.
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

/// Write TJS `null` into `out`.
fn set_null(out: *mut Value) {
    if out.is_null() {
        return;
    }
    // SAFETY: `out` is a valid result slot.
    unsafe {
        (*out).ty = VAL_NULL;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = ptr::null();
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Write a retained object into `out` in the consuming `VAL_RETAINED` form.
fn set_retained_out(out: *mut Value, id: usize) {
    if out.is_null() {
        return;
    }
    // SAFETY: `out` is a valid result slot.
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

/// Marshal an [`Owned`] into the native result slot.
fn write_owned(services: &CompatServices, out: *mut Value, value: Owned) -> c_int {
    match value {
        Owned::Void => set_void(out),
        Owned::Integer(value) => set_int(out, value),
        Owned::Real(value) => set_real(out, value),
        Owned::String(value) => set_string_out(services, out, &value),
        Owned::Null => set_null(out),
        Owned::Octet(bytes) => set_octet_out(services, out, &bytes),
        Owned::Object(object) => {
            set_retained_out(out, object.raw_id() as usize);
            // The C++ trampoline consumes the retention.
            std::mem::forget(object);
        }
    }
    0
}

/// Resolve the bound VM/services and run `f`, marshalling its result.
fn dispatch<F>(
    engine: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    method: &str,
    f: F,
) -> c_int
where
    F: for<'a> FnOnce(&'a Tjs2Engine, &'a CompatServices) -> Result<Owned, String>,
{
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        return report_error(
            out_error,
            &format!("scripts_ex: {method}: compat context is not bound"),
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolved` is only produced for the installed, live binding.
    let services = unsafe { ctx.services() };
    let engine = resolved.engine();
    match f(engine, services) {
        Ok(value) => write_owned(services, out, value),
        Err(error) => report_error(out_error, &error),
    }
}

// ---------------------------------------------------------------------------
// argument conversion
// ---------------------------------------------------------------------------

/// Convert a raw callback argument into an [`Owned`].
fn owned_from_arg(engine: &Tjs2Engine, value: &Value) -> Result<Owned, String> {
    match value.ty {
        VAL_VOID => Ok(Owned::Void),
        VAL_NULL => Ok(Owned::Null),
        VAL_INTEGER => Ok(Owned::Integer(value.integer)),
        VAL_REAL => Ok(Owned::Real(value.real)),
        VAL_STRING => Ok(Owned::String(value_as_string(value))),
        VAL_OCTET => {
            // SAFETY: octet bytes are valid for the callback.
            let bytes = unsafe { value.octet_bytes() }.unwrap_or(&[]);
            Ok(Owned::Octet(bytes.to_vec()))
        }
        VAL_OBJECT => Ok(Owned::Object(engine.retain_object_arg(value)?)),
        other => Err(format!("scripts_ex: unsupported argument type {other}")),
    }
}

/// Convert a safe engine result into an [`Owned`], retaining an object.
fn owned_from_tjs(engine: &Tjs2Engine, value: TjsValue) -> Result<Owned, String> {
    match value {
        TjsValue::Void => Ok(Owned::Void),
        TjsValue::Integer(value) => Ok(Owned::Integer(value)),
        TjsValue::Real(value) => Ok(Owned::Real(value)),
        TjsValue::String(value) => Ok(Owned::String(value)),
        // Resolves against the engine's most recent object result, so this
        // must be called immediately after the producing engine call.
        TjsValue::Object => Ok(Owned::Object(
            engine.retain_value_detached(&TjsValue::Object)?,
        )),
        TjsValue::Retained(_) => {
            Err("scripts_ex: unexpected retained value in an engine result".into())
        }
    }
}

/// The member name for a `propGet`/`propSet` key argument: integers become
/// their decimal form so `PropGet` routes to the array's numeric lookup.
fn member_key(value: &Value) -> String {
    if value.ty == VAL_INTEGER {
        value.integer.to_string()
    } else {
        value_as_string(value)
    }
}

// ---------------------------------------------------------------------------
// cached helper closures
// ---------------------------------------------------------------------------

const STRICT_EQ_SRC: &str = "(function(a,b){return a===b;})";

/// `typeof obj[key]`, the VM's member-existence probe.
const MEMBER_TYPE_SRC: &str = "(function(o,k){return typeof o[k];})";

/// Build (once) and return the retained id of a helper closure.
fn cached_helper(
    engine: &Tjs2Engine,
    services: &CompatServices,
    key: &'static str,
    source: &str,
) -> Result<tjs::Tjs2ValueId, String> {
    if let Some(id) = services.state::<ScriptsExState>().helpers.get(key).copied() {
        return Ok(id);
    }
    match engine.eval_retained(source, "scripts_ex") {
        Ok(RetainedValue::Object(object)) => {
            let id = object.raw_id();
            // The engine's retained map lives as long as the VM; dropping the
            // Rust handle would release it, so leak it into the cache.
            std::mem::forget(object);
            services.state::<ScriptsExState>().helpers.insert(key, id);
            Ok(id)
        }
        Ok(RetainedValue::Value(value)) => Err(format!(
            "scripts_ex: helper {key} did not evaluate to a function ({value:?})"
        )),
        Err(error) => Err(format!("scripts_ex: helper {key}: {error}")),
    }
}

/// Whether `target`'s class-name chain contains `class` (reference
/// `IsInstanceOf(<class>)`). TJS2's `instanceof` operator takes a class-name
/// *string* on the right, so the class chain is read directly instead.
#[track_caller]
fn is_instance_of(
    engine: &Tjs2Engine,
    target: &DetachedValue,
    class: &str,
) -> Result<bool, String> {
    let names = engine.class_names(target.raw_id())?;
    Ok(keys_of(engine, &names)?.iter().any(|name| name == class))
}

// ---------------------------------------------------------------------------
// collection helpers
// ---------------------------------------------------------------------------

/// The `count` property of a real TJS array (never falls back to
/// enumeration).
fn array_count(engine: &Tjs2Engine, array: &DetachedValue) -> Result<i64, String> {
    match engine.member(array.raw_id(), "count")? {
        TjsValue::Integer(value) => Ok(value),
        TjsValue::Real(value) => Ok(value as i64),
        other => Err(format!(
            "scripts_ex: array count is not numeric ({other:?})"
        )),
    }
}

/// Flatten `target` into a fresh TJS array through the builtin
/// `Array.assign`, which runs the object's `EnumMembers` and appends
/// `[key0, value0, key1, value1, ...]` (hidden members skipped). This is
/// the only script-reachable enumeration the vendored VM exposes.
fn enumerate_flat(engine: &Tjs2Engine, target: &DetachedValue) -> Result<DetachedValue, String> {
    let flat = new_object(engine, "[]")?;
    let argument = engine.duplicate_retained(target.raw_id())?;
    engine.call_member(
        flat.raw_id(),
        "assign",
        &[TjsValue::Retained(argument.raw_id() as u64)],
    )?;
    Ok(flat)
}

/// Read the key at even index `index` of a flattened `assign` array.
fn read_key(engine: &Tjs2Engine, flat: &DetachedValue, index: i64) -> Result<String, String> {
    match engine.member(flat.raw_id(), &index.to_string())? {
        TjsValue::String(value) => Ok(value),
        TjsValue::Integer(value) => Ok(value.to_string()),
        TjsValue::Real(value) => Ok(value.to_string()),
        other => Err(format!("scripts_ex: unexpected member key {other:?}")),
    }
}

/// Whether a member value is one of the plugin methods the ABI attaches to
/// every `Array`/`Dictionary` instance.
///
/// The attached callbacks are bare dispatch objects: unlike data objects
/// (arrays, dictionaries, script classes) they report no class-name chain.
/// The reference keeps methods on the class and never enumerates them, so
/// such opaque objects are skipped by the enumeration helpers.
fn is_attached_method(engine: &Tjs2Engine, value: &Owned) -> Result<bool, String> {
    match value {
        Owned::Object(object) => {
            let names = engine.class_names(object.raw_id())?;
            Ok(keys_of(engine, &names)?.is_empty())
        }
        _ => Ok(false),
    }
}

/// Enumerate `target`'s own **data** members (reference `EnumMembers`).
///
/// The ABI attaches plugin instance members to every `Array`/`Dictionary`
/// instance as own members, so a plain `assign` flattening would expose the
/// attached methods (`clone`, `foreach`, `saveStruct2`, ...) as dictionary
/// entries. The reference keeps methods on the class (never on the instance),
/// so function-valued members are filtered here to match the reference's
/// observable member set.
fn enumerate_entries(
    engine: &Tjs2Engine,
    target: &DetachedValue,
) -> Result<Vec<(String, Owned)>, String> {
    let flat = enumerate_flat(engine, target)?;
    let count = array_count(engine, &flat)?;
    let mut entries = Vec::new();
    let mut index = 0;
    while index + 1 < count {
        let key = read_key(engine, &flat, index)?;
        let value = engine.member(flat.raw_id(), &(index + 1).to_string())?;
        let owned = owned_from_tjs(engine, value)?;
        if !is_attached_method(engine, &owned)? {
            entries.push((key, owned));
        }
        index += 2;
    }
    Ok(entries)
}

/// The member names of `target` as a freshly-built, sorted TJS `Array`
/// (reference `ScriptsAdd::_getKeys`).
///
/// Only reachable for non-`Array` objects: the reference enumerates with
/// `EnumMembers`, which a TJS `Array` rejects with `TJS_E_NOTIMPL`
/// (`crates/tjs/cpp/tjs2/tjsArray.h:216-219`) — see [`get_object_keys`].
fn object_keys(engine: &Tjs2Engine, target: &DetachedValue) -> Result<DetachedValue, String> {
    let keys = new_object(engine, "[]")?;
    for (key, _) in enumerate_entries(engine, target)? {
        engine.call_member(keys.raw_id(), "add", &[TjsValue::String(key)])?;
    }
    // The reference sorts the returned keys with the TJS `sort`.
    engine.call_member(keys.raw_id(), "sort", &[])?;
    Ok(keys)
}

/// The member count of `target` (reference `GetCount`).
///
/// `Array` exposes a `count` property; `Dictionary` and other objects do not,
/// so fall back to the `EnumMembers`-backed enumeration.
fn object_count(engine: &Tjs2Engine, target: &DetachedValue) -> Result<i64, String> {
    if let Ok(value) = engine.member(target.raw_id(), "count") {
        match value {
            TjsValue::Integer(value) => return Ok(value),
            TjsValue::Real(value) => return Ok(value as i64),
            _ => {}
        }
    }
    Ok(enumerate_entries(engine, target)?.len() as i64)
}

/// Read a TJS array of strings into a Rust `Vec`.
fn keys_of(engine: &Tjs2Engine, array: &DetachedValue) -> Result<Vec<String>, String> {
    let count = array_count(engine, array)?;
    let mut keys = Vec::with_capacity(count.max(0) as usize);
    for index in 0..count {
        keys.push(read_key(engine, array, index)?);
    }
    Ok(keys)
}

// ---------------------------------------------------------------------------
// structural equality
// ---------------------------------------------------------------------------

/// Recursive structural equality (reference `equalStruct` /
/// `equalStructNumericLoose`).
fn equal_struct(
    engine: &Tjs2Engine,
    services: &CompatServices,
    a: Owned,
    b: Owned,
    loose: bool,
) -> Result<bool, String> {
    match (a, b) {
        (Owned::Object(x), Owned::Object(y)) => {
            let x_array = is_instance_of(engine, &x, "Array")?;
            let y_array = is_instance_of(engine, &y, "Array")?;
            if x_array && y_array {
                return equal_arrays(engine, services, &x, &y, loose);
            }
            let x_dict = is_instance_of(engine, &x, "Dictionary")?;
            let y_dict = is_instance_of(engine, &y, "Dictionary")?;
            if x_dict && y_dict {
                return equal_dicts(engine, services, &x, &y, loose);
            }
            objects_identical(engine, services, &x, &y)
        }
        (Owned::Integer(x), Owned::Integer(y)) => Ok(x == y),
        (Owned::Real(x), Owned::Real(y)) => Ok(x == y),
        (Owned::String(x), Owned::String(y)) => Ok(x == y),
        (Owned::Void, Owned::Void) => Ok(true),
        (Owned::Null, Owned::Null) => Ok(true),
        (Owned::Octet(x), Owned::Octet(y)) => Ok(x == y),
        (Owned::Integer(x), Owned::Real(y)) | (Owned::Real(y), Owned::Integer(x)) if loose => {
            Ok((x as f64) == y)
        }
        _ => Ok(false),
    }
}

/// `a === b` for two retained objects (function/object identity).
fn objects_identical(
    engine: &Tjs2Engine,
    services: &CompatServices,
    a: &DetachedValue,
    b: &DetachedValue,
) -> Result<bool, String> {
    let helper = cached_helper(engine, services, "strict_eq", STRICT_EQ_SRC)?;
    let function = engine.duplicate_retained(helper)?;
    let left = engine.duplicate_retained(a.raw_id())?;
    let right = engine.duplicate_retained(b.raw_id())?;
    let result = engine.call_detached(
        &function,
        &[
            TjsValue::Retained(left.raw_id() as u64),
            TjsValue::Retained(right.raw_id() as u64),
        ],
    )?;
    Ok(matches!(result, TjsValue::Integer(value) if value != 0))
}

/// Compare two arrays element by element.
fn equal_arrays(
    engine: &Tjs2Engine,
    services: &CompatServices,
    x: &DetachedValue,
    y: &DetachedValue,
    loose: bool,
) -> Result<bool, String> {
    let x_count = object_count(engine, x)?;
    let y_count = object_count(engine, y)?;
    if x_count != y_count {
        return Ok(false);
    }
    for index in 0..x_count {
        let x_value = engine.member(x.raw_id(), &index.to_string())?;
        let x_owned = owned_from_tjs(engine, x_value)?;
        let y_value = engine.member(y.raw_id(), &index.to_string())?;
        let y_owned = owned_from_tjs(engine, y_value)?;
        if !equal_struct(engine, services, x_owned, y_owned, loose)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Compare two dictionaries.
fn equal_dicts(
    engine: &Tjs2Engine,
    services: &CompatServices,
    x: &DetachedValue,
    y: &DetachedValue,
    loose: bool,
) -> Result<bool, String> {
    if loose {
        // Count must match; every key of `x` that exists in `y` must have an
        // equal value (the reference ignores keys missing from `y`).
        let x_count = object_count(engine, x)?;
        let y_count = object_count(engine, y)?;
        if x_count != y_count {
            return Ok(false);
        }
        let keys_array = object_keys(engine, x)?;
        for key in keys_of(engine, &keys_array)? {
            let Ok(y_value) = engine.member(y.raw_id(), &key) else {
                continue;
            };
            let y_owned = owned_from_tjs(engine, y_value)?;
            let x_value = engine.member(x.raw_id(), &key)?;
            let x_owned = owned_from_tjs(engine, x_value)?;
            if !equal_struct(engine, services, x_owned, y_owned, true)? {
                return Ok(false);
            }
        }
        Ok(true)
    } else {
        // Key lists must be identical, then every value must match.
        let x_keys = object_keys(engine, x)?;
        let y_keys = object_keys(engine, y)?;
        if !equal_arrays(engine, services, &x_keys, &y_keys, false)? {
            return Ok(false);
        }
        for key in keys_of(engine, &x_keys)? {
            let x_value = engine.member(x.raw_id(), &key)?;
            let x_owned = owned_from_tjs(engine, x_value)?;
            let y_value = engine.member(y.raw_id(), &key)?;
            let y_owned = owned_from_tjs(engine, y_value)?;
            if !equal_struct(engine, services, x_owned, y_owned, false)? {
                return Ok(false);
            }
        }
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// foreach
// ---------------------------------------------------------------------------

/// Call `func(key, value, extra...)` for every member (reference
/// `ScriptsAdd::foreach`). A non-`void` return stops the walk and is
/// returned; otherwise the result is `void`.
fn foreach(
    engine: &Tjs2Engine,
    _services: &CompatServices,
    target: &DetachedValue,
    function: DetachedValue,
    extra: Vec<Owned>,
) -> Result<Owned, String> {
    if is_instance_of(engine, target, "Array")? {
        let count = object_count(engine, target)?;
        for index in 0..count {
            let value = engine.member(target.raw_id(), &index.to_string())?;
            let owned = owned_from_tjs(engine, value)?;
            let result = call_callback(engine, &function, Owned::Integer(index), owned, &extra)?;
            if let Some(result) = result {
                return Ok(result);
            }
        }
    } else {
        for (key, value) in enumerate_entries(engine, target)? {
            let result = call_callback(engine, &function, Owned::String(key), value, &extra)?;
            if let Some(result) = result {
                return Ok(result);
            }
        }
    }
    Ok(Owned::Void)
}

/// Invoke the `foreach` callback, returning `Some(result)` when it returned
/// something other than `void`.
fn call_callback(
    engine: &Tjs2Engine,
    function: &DetachedValue,
    key: Owned,
    value: Owned,
    extra: &[Owned],
) -> Result<Option<Owned>, String> {
    let mut arguments = Vec::with_capacity(2 + extra.len());
    arguments.push(key.to_tjs_for_call(engine)?);
    arguments.push(value.to_tjs_for_call(engine)?);
    for argument in extra {
        arguments.push(argument.to_tjs_for_call(engine)?);
    }
    match engine.call_detached(function, &arguments)? {
        TjsValue::Void => Ok(None),
        other => Ok(Some(owned_from_tjs(engine, other)?)),
    }
}

// ---------------------------------------------------------------------------
// clone
// ---------------------------------------------------------------------------

/// Deep-copy `value` (reference `ScriptsAdd::clone`).
fn clone_owned(
    engine: &Tjs2Engine,
    services: &CompatServices,
    value: Owned,
) -> Result<Owned, String> {
    match value {
        Owned::Object(object) => {
            if is_instance_of(engine, &object, "Array")? {
                clone_array(engine, services, &object)
            } else if is_instance_of(engine, &object, "Dictionary")? {
                clone_dict(engine, services, &object)
            } else {
                // Reference `ScriptsAdd::clone` for every other object
                // (`scriptsEx.cpp:626-646`):
                //
                // ```
                // if(o1.FuncCall(0, "clone", ..., &result, ...) == TJS_S_TRUE)
                //     return result;
                // return obj;
                // ```
                //
                // `TJS_S_TRUE` is `1` while a successfully executed script
                // function returns `TJS_S_OK` = `0`
                // (`crates/tjs/cpp/tjs2/tjsErrorDefs.h:32-36`), so the
                // comparison never holds: the object's `clone` member **is
                // invoked** (its side effects run) but its result is
                // discarded and the *original* object comes back. A missing
                // `clone` (`TJS_E_MEMBERNOTFOUND`) and a non-callable member
                // are error codes the reference compares away; only a thrown
                // script exception propagates, as it does here.
                match engine.member(object.raw_id(), "clone") {
                    Err(_) => Ok(Owned::Object(object)),
                    Ok(TjsValue::Object) => {
                        engine.call_member(object.raw_id(), "clone", &[])?;
                        Ok(Owned::Object(object))
                    }
                    Ok(_) => Ok(Owned::Object(object)),
                }
            }
        }
        other => Ok(other),
    }
}

/// Deep-copy an array, recursively cloning each element.
fn clone_array(
    engine: &Tjs2Engine,
    services: &CompatServices,
    source: &DetachedValue,
) -> Result<Owned, String> {
    let copy = new_object(engine, "[]")?;
    let count = object_count(engine, source)?;
    for index in 0..count {
        let value = engine.member(source.raw_id(), &index.to_string())?;
        let owned = owned_from_tjs(engine, value)?;
        let cloned = clone_owned(engine, services, owned)?;
        let argument = cloned.to_tjs_for_call(engine)?;
        engine.call_member(copy.raw_id(), "add", &[argument])?;
    }
    Ok(Owned::Object(copy))
}

/// Deep-copy a dictionary, recursively cloning each value.
fn clone_dict(
    engine: &Tjs2Engine,
    services: &CompatServices,
    source: &DetachedValue,
) -> Result<Owned, String> {
    let copy = new_object(engine, "%[]")?;
    let keys_array = object_keys(engine, source)?;
    for key in keys_of(engine, &keys_array)? {
        let value = engine.member(source.raw_id(), &key)?;
        let owned = owned_from_tjs(engine, value)?;
        let cloned = clone_owned(engine, services, owned)?;
        let argument = cloned.to_tjs_for_call(engine)?;
        engine.set_member(copy.raw_id(), &key, &argument)?;
    }
    Ok(Owned::Object(copy))
}

/// Evaluate a TJS array/dictionary literal into a retained object.
fn new_object(engine: &Tjs2Engine, expression: &str) -> Result<DetachedValue, String> {
    match engine.eval_retained(expression, "scripts_ex") {
        Ok(RetainedValue::Object(object)) => Ok(object),
        Ok(RetainedValue::Value(value)) => Err(format!(
            "scripts_ex: {expression:?} did not evaluate to an object ({value:?})"
        )),
        Err(error) => Err(format!("scripts_ex: {expression:?}: {error}")),
    }
}

// ---------------------------------------------------------------------------
// MD5 / storage
// ---------------------------------------------------------------------------

/// Lowercase hex MD5 of the first octet argument.
fn md5_hash_string(arguments: &[Value]) -> Result<Owned, String> {
    let Some(octet) = arguments.first() else {
        return Err("scripts_ex: getMD5HashString requires 1 argument".into());
    };
    if octet.ty != VAL_OCTET {
        return Err("scripts_ex: getMD5HashString expects an octet".into());
    }
    // SAFETY: the octet bytes are valid for the callback.
    let bytes = unsafe { octet.octet_bytes() }.unwrap_or(&[]);
    Ok(Owned::String(md5_hex(bytes)))
}

/// Reference `safeEvalStorage`: read the storage, evaluate
/// `(const)[<content>]` and return element `0`.
fn safe_eval_storage(
    engine: &Tjs2Engine,
    services: &CompatServices,
    arguments: &[Value],
) -> Result<Owned, String> {
    let Some(name) = arguments.first() else {
        return Err("scripts_ex: safeEvalStorage requires 1 argument".into());
    };
    let name = value_as_string(name);
    let mode = arguments.get(1).map(value_as_string).unwrap_or_default();
    if arguments.get(2).is_some_and(|value| value.ty != VAL_VOID) {
        log::warn!("Scripts.safeEvalStorage: the execution-context argument is ignored");
    }
    let Some(storage) = services.storage() else {
        return Err("scripts_ex: safeEvalStorage: no storage attached".into());
    };
    // SAFETY: the host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { &*storage };
    let bytes = storage
        .read(&name)
        .map_err(|error| format!("scripts_ex: safeEvalStorage '{name}': {error}"))?;
    let bytes = match mode_offset(&mode) {
        Some(offset) => &bytes[offset.min(bytes.len())..],
        None => &bytes[..],
    };
    let text = crate::text::decode_text(bytes, None)
        .map_err(|error| format!("scripts_ex: safeEvalStorage '{name}': {error}"))?;
    let wrapped = format!("(const)[{text}]");
    let shortname = storage_name(&name);
    match engine.eval_retained(&wrapped, &shortname) {
        Ok(RetainedValue::Object(object)) => {
            let value = engine.member(object.raw_id(), "0")?;
            owned_from_tjs(engine, value)
        }
        Ok(RetainedValue::Value(_)) => Ok(Owned::Void),
        Err(error) => Err(format!("scripts_ex: safeEvalStorage: {error}")),
    }
}

/// Parse a text-stream mode string for the `oN` byte offset.
fn mode_offset(mode: &str) -> Option<usize> {
    let bytes = mode.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'o' {
            if !bytes.get(index + 1).is_some_and(u8::is_ascii_digit) {
                return None;
            }
            let mut value = 0usize;
            for &byte in &bytes[index + 1..] {
                if !byte.is_ascii_digit() {
                    break;
                }
                value = value * 10 + usize::from(byte - b'0');
            }
            return Some(value);
        }
        index += 1;
    }
    None
}

/// Base name of a storage path (reference `TVPExtractStorageName`).
fn storage_name(name: &str) -> String {
    name.rsplit(['/', '\\']).next().unwrap_or(name).to_string()
}

// ---------------------------------------------------------------------------
// native wrappers: `Scripts` static members
// ---------------------------------------------------------------------------

/// `Scripts.getObjectKeys(obj)` (and `obj.getObjectKeys()`).
///
/// Reference `ScriptsAdd::_getKeys` (`scriptsEx.cpp:320-345`) enumerates with
/// `EnumMembers(TJS_IGNOREPROP | TJS_ENUM_NO_VALUE, ...)`; a TJS `Array`
/// rejects that with `TJS_E_NOTIMPL` (`tjsArray.h:216-219`), so the callback
/// never runs and the reference returns an **empty** array for arrays (the
/// error is discarded). Dictionaries and script objects enumerate normally.
fn get_object_keys(
    engine: &Tjs2Engine,
    _services: &CompatServices,
    target: &DetachedValue,
) -> Result<Owned, String> {
    if is_instance_of(engine, target, "Array")? {
        return Ok(Owned::Object(new_object(engine, "[]")?));
    }
    object_keys(engine, target).map(Owned::Object)
}

/// `Scripts.getObjectCount(obj)` (and `obj.getObjectCount()`).
///
/// Reference `ScriptsAdd::getCount` (`scriptsEx.cpp:474-484`) reads the
/// dispatch `GetCount`, **not** the script-visible `count` property. For a
/// TJS `Array` that dispatch entry is not overridden (`tjsArray.h:213-214`,
/// commented out) and falls back to `tTJSCustomObject::GetCount`, which
/// reports the object's own-member count (`tjsObject.cpp:1431-1442`) — `0`
/// for an array whose elements live in the native `Items` vector. Every
/// other object counts its own data members (the ABI-attached plugin methods
/// are filtered out, exactly as the reference never sees class members).
fn get_object_count(
    engine: &Tjs2Engine,
    _services: &CompatServices,
    target: &DetachedValue,
) -> Result<Owned, String> {
    if is_instance_of(engine, target, "Array")? {
        return Ok(Owned::Integer(0));
    }
    let count = enumerate_entries(engine, target)?.len() as i64;
    Ok(Owned::Integer(count))
}

/// `Scripts.foreach(obj, func, args*)` / `obj.foreach(func, args*)`.
fn run_foreach(
    engine: &Tjs2Engine,
    services: &CompatServices,
    target: &DetachedValue,
    function: DetachedValue,
    extra: Vec<Owned>,
) -> Result<Owned, String> {
    foreach(engine, services, target, function, extra)
}

/// Whether `target` has the member `key`, via `typeof target[key]`.
///
/// `typeof` compiles to `TypeOfMemberIndirect` with `TJS_MEMBERMUSTEXIST`
/// (`tjsInterCodeExec.cpp:1130-1135`), so it reports `"undefined"` for a
/// missing member and the value's type name otherwise — including `"void"`
/// for a member that exists but holds void. The flag matters: a dictionary's
/// `PropGet` turns a missing member into void unless `TJS_MEMBERMUSTEXIST` is
/// set (`tjsDictionary.cpp:732-742`), so a flag-0 read cannot distinguish
/// absent from present-void.
fn member_exists(
    engine: &Tjs2Engine,
    services: &CompatServices,
    target: &DetachedValue,
    key: &str,
) -> Result<bool, String> {
    let helper = cached_helper(engine, services, "member_type", MEMBER_TYPE_SRC)?;
    let function = engine.duplicate_retained(helper)?;
    let object = engine.duplicate_retained(target.raw_id())?;
    let result = engine.call_detached(
        &function,
        &[
            TjsValue::Retained(object.raw_id() as u64),
            TjsValue::String(key.to_string()),
        ],
    )?;
    match result {
        TjsValue::String(kind) => Ok(kind != "undefined"),
        other => Err(format!(
            "scripts_ex: member existence probe returned {other:?}"
        )),
    }
}

/// `Scripts.propSet(obj, key, value[, flag])` / `obj.propSet(key, value[, flag])`.
///
/// `flag` defaults to `TJS_MEMBERENSURE`, as in the reference
/// (`scriptsEx.cpp:683-698`). The reference branches on the key's *type*:
/// integer keys use `PropSetByNum` — on an array that rejects an
/// out-of-range index only under `TJS_MEMBERMUSTEXIST` and extends it
/// otherwise (`tTJSArrayObject::PropSetByNum`, `tjsArray.cpp:1597-1620`);
/// every other key uses `PropSet`, where the *absence* of
/// `TJS_MEMBERENSURE` fails on a missing member (`tTJSCustomObject::PropSet`,
/// `tjsObject.cpp:1352-1383`). The `TJS_HIDDENMEMBER`/`TJS_STATICMEMBER`
/// bits only change member *flags*, which the port's `PropSet` entry point
/// does not expose.
fn run_prop_set(
    engine: &Tjs2Engine,
    services: &CompatServices,
    target: &DetachedValue,
    key: &str,
    value: Owned,
    flag: i64,
) -> Result<Owned, String> {
    let array_index = is_instance_of(engine, target, "Array")? && key.parse::<i64>().is_ok();
    if array_index {
        if flag & TJS_MEMBERMUSTEXIST != 0 {
            let count = array_count(engine, target)?;
            let mut index = key.parse::<i64>().unwrap_or(0);
            if index < 0 {
                index += count;
            }
            if index < 0 || index >= count {
                return Err(format!("Scripts.propSet: member {key} not found"));
            }
        }
    } else if flag & TJS_MEMBERENSURE == 0 && !member_exists(engine, services, target, key)? {
        // Without TJS_MEMBERENSURE the write fails on a missing member.
        return Err(format!("Scripts.propSet: member {key} not found"));
    }
    let argument = value.to_tjs_for_call(engine)?;
    engine.set_member(target.raw_id(), key, &argument)?;
    Ok(Owned::Void)
}

/// `Scripts.propGet(obj, key[, flag])` / `obj.propGet(key[, flag])`.
///
/// `flag` defaults to `TJS_MEMBERMUSTEXIST` (`scriptsEx.cpp:700-714`). On a
/// missing member the reference errors for the default flag
/// (`tTJSCustomObject::PropGet` returns `TJS_E_MEMBERNOTFOUND`,
/// `tjsObject.cpp:1276-1309`; a dictionary's override keeps that error only
/// when `TJS_MEMBERMUSTEXIST` is set, `tjsDictionary.cpp:732-742`), while
/// `TJS_MEMBERENSURE` *creates* the member as `void` before reading it; an
/// out-of-range array index errors under `TJS_MEMBERMUSTEXIST` and reads
/// `void` otherwise (`ARRAY_GET_VAL`, `tjsArray.cpp:1381-1390`).
fn run_prop_get(
    engine: &Tjs2Engine,
    services: &CompatServices,
    target: &DetachedValue,
    key: &str,
    flag: i64,
) -> Result<Owned, String> {
    let array_index = is_instance_of(engine, target, "Array")? && key.parse::<i64>().is_ok();
    if array_index {
        if flag & TJS_MEMBERMUSTEXIST != 0 {
            let count = array_count(engine, target)?;
            let mut index = key.parse::<i64>().unwrap_or(0);
            if index < 0 {
                index += count;
            }
            if index < 0 || index >= count {
                return Err(format!("Scripts.propGet: member {key} not found"));
            }
        }
    } else if !member_exists(engine, services, target, key)? {
        if flag & TJS_MEMBERENSURE != 0 {
            // The reference creates the member as void before reading it.
            engine.set_member(target.raw_id(), key, &TjsValue::Void)?;
        } else if flag & TJS_MEMBERMUSTEXIST != 0 {
            return Err(format!("Scripts.propGet: member {key} not found"));
        }
        // Flag 0 on a dictionary: the read below returns void.
    }
    let value = engine.member(target.raw_id(), key)?;
    owned_from_tjs(engine, value)
}

extern "C" fn scripts_get_object_keys(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.getObjectKeys",
        |engine, services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            let target = arguments
                .first()
                .ok_or("Scripts.getObjectKeys: requires 1 argument")?;
            let target = engine.retain_object_arg(target)?;
            get_object_keys(engine, services, &target)
        },
    )
}

extern "C" fn scripts_get_object_count(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.getObjectCount",
        |engine, services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            let target = arguments
                .first()
                .ok_or("Scripts.getObjectCount: requires 1 argument")?;
            let target = engine.retain_object_arg(target)?;
            get_object_count(engine, services, &target)
        },
    )
}

extern "C" fn scripts_clone(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.clone",
        |engine, services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            let value = arguments
                .first()
                .ok_or("Scripts.clone: requires 1 argument")?;
            // Scalars/strings/octets clone to themselves; objects go through the
            // deep-copy walk.
            clone_owned(engine, services, owned_from_arg(engine, value)?)
        },
    )
}

extern "C" fn scripts_get_object_context(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.getObjectContext",
        |engine, _services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            let value = arguments
                .first()
                .ok_or("Scripts.getObjectContext: requires 1 argument")?;
            let context = value.object_objthis();
            if context.is_null() {
                return Ok(Owned::Null);
            }
            Ok(Owned::Object(engine.retain_object_detached(context)?))
        },
    )
}

extern "C" fn scripts_is_null_context(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.isNullContext",
        |_engine, _services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            let value = arguments
                .first()
                .ok_or("Scripts.isNullContext: requires 1 argument")?;
            Ok(Owned::Integer(i64::from(value.object_objthis().is_null())))
        },
    )
}

extern "C" fn scripts_equal_struct(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.equalStruct",
        |engine, services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            if arguments.len() < 2 {
                return Err("Scripts.equalStruct requires 2 arguments".into());
            }
            let a = owned_from_arg(engine, &arguments[0])?;
            let b = owned_from_arg(engine, &arguments[1])?;
            equal_struct(engine, services, a, b, false)
                .map(|value| Owned::Integer(i64::from(value)))
        },
    )
}

extern "C" fn scripts_equal_struct_numeric_loose(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.equalStructNumericLoose",
        |engine, services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            if arguments.len() < 2 {
                return Err("Scripts.equalStructNumericLoose requires 2 arguments".into());
            }
            let a = owned_from_arg(engine, &arguments[0])?;
            let b = owned_from_arg(engine, &arguments[1])?;
            equal_struct(engine, services, a, b, true).map(|value| Owned::Integer(i64::from(value)))
        },
    )
}

extern "C" fn scripts_foreach(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.foreach",
        |engine, services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            if arguments.len() < 2 {
                return Err("Scripts.foreach requires at least 2 arguments".into());
            }
            let target = engine.retain_object_arg(&arguments[0])?;
            let function = engine.retain_object_arg(&arguments[1])?;
            let extra = arguments[2..]
                .iter()
                .map(|value| owned_from_arg(engine, value))
                .collect::<Result<Vec<_>, _>>()?;
            run_foreach(engine, services, &target, function, extra)
        },
    )
}

extern "C" fn scripts_get_md5_hash_string(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.getMD5HashString",
        |_engine, _services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            md5_hash_string(arguments)
        },
    )
}

extern "C" fn scripts_prop_set(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.propSet",
        |engine, services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            if arguments.len() < 3 {
                return Err("Scripts.propSet requires at least 3 arguments".into());
            }
            let target = engine.retain_object_arg(&arguments[0])?;
            let key = member_key(&arguments[1]);
            let value = owned_from_arg(engine, &arguments[2])?;
            let flag = arguments
                .get(3)
                .map(value_as_i64)
                .unwrap_or(TJS_MEMBERENSURE);
            run_prop_set(engine, services, &target, &key, value, flag)
        },
    )
}

extern "C" fn scripts_prop_get(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.propGet",
        |engine, services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            if arguments.len() < 2 {
                return Err("Scripts.propGet requires at least 2 arguments".into());
            }
            let target = engine.retain_object_arg(&arguments[0])?;
            let key = member_key(&arguments[1]);
            let flag = arguments
                .get(2)
                .map(value_as_i64)
                .unwrap_or(TJS_MEMBERMUSTEXIST);
            run_prop_get(engine, services, &target, &key, flag)
        },
    )
}

extern "C" fn scripts_safe_eval_storage(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.safeEvalStorage",
        |engine, services| {
            // SAFETY: the trampoline guarantees `argc` valid entries.
            let arguments = unsafe { args(argv, argc) };
            safe_eval_storage(engine, services, arguments)
        },
    )
}

extern "C" fn scripts_rehash(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    dispatch(
        engine,
        out,
        out_error,
        "Scripts.rehash",
        |_engine, _services| {
            // SAFETY: the VM is linked into this binary; the function only bumps
            // the global rebuild-hash magic.
            unsafe { tjs_do_rehash() };
            Ok(Owned::Void)
        },
    )
}

// ---------------------------------------------------------------------------
// constants
// ---------------------------------------------------------------------------

/// The `pf*` flag constants and their values.
const PF_CONSTANTS: &[(&str, i64)] = &[
    ("pfMemberEnsure", TJS_MEMBERENSURE),
    ("pfMemberMustExist", TJS_MEMBERMUSTEXIST),
    ("pfIgnoreProp", TJS_IGNOREPROP),
    ("pfHiddenMember", TJS_HIDDENMEMBER),
    ("pfStaticMember", TJS_STATICMEMBER),
];

// ---------------------------------------------------------------------------
// registration
// ---------------------------------------------------------------------------

/// Static members of `Scripts`.
fn static_methods() -> Vec<NativeMethodDef> {
    vec![
        NativeMethodDef {
            name: "getObjectKeys",
            f: scripts_get_object_keys,
        },
        NativeMethodDef {
            name: "getObjectCount",
            f: scripts_get_object_count,
        },
        NativeMethodDef {
            name: "getObjectContext",
            f: scripts_get_object_context,
        },
        NativeMethodDef {
            name: "isNullContext",
            f: scripts_is_null_context,
        },
        NativeMethodDef {
            name: "equalStruct",
            f: scripts_equal_struct,
        },
        NativeMethodDef {
            name: "equalStructNumericLoose",
            f: scripts_equal_struct_numeric_loose,
        },
        NativeMethodDef {
            name: "foreach",
            f: scripts_foreach,
        },
        NativeMethodDef {
            name: "getMD5HashString",
            f: scripts_get_md5_hash_string,
        },
        NativeMethodDef {
            name: "clone",
            f: scripts_clone,
        },
        NativeMethodDef {
            name: "propSet",
            f: scripts_prop_set,
        },
        NativeMethodDef {
            name: "propGet",
            f: scripts_prop_get,
        },
        NativeMethodDef {
            name: "safeEvalStorage",
            f: scripts_safe_eval_storage,
        },
        NativeMethodDef {
            name: "rehash",
            f: scripts_rehash,
        },
    ]
}

/// Register the plugin's native surface on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "Scripts",
        methods: static_methods(),
        properties: vec![],
    })?;

    // The `pf*` flags are plain class data members rather than accessor
    // properties, so a script assignment overwrites the value. Write them
    // onto the `Scripts` class object directly.
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let engine = unsafe { ctx.engine() };
    let class = match engine.eval_retained("Scripts", "scripts_ex") {
        Ok(RetainedValue::Object(class)) => class,
        Ok(_) => {
            return Err(CompatError::Register(
                "scripts_ex: Scripts is not a class object".into(),
            ));
        }
        Err(error) => {
            return Err(CompatError::Register(format!(
                "scripts_ex: Scripts lookup failed: {error}"
            )));
        }
    };
    for &(name, value) in PF_CONSTANTS {
        engine
            .set_member(class.raw_id(), name, &TjsValue::Integer(value))
            .map_err(|error| CompatError::Register(format!("scripts_ex: {name}: {error}")))?;
    }
    Ok(())
}
