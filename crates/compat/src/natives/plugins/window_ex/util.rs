//! Small marshalling helpers shared by the `windowEx` members.
//!
//! The common value writers (`set_void`, `set_int`, `set_bool`,
//! `value_as_string`, `report_error`) live in [`crate::native_util`]; this
//! module adds the string/real/retained writers and the `objthis` property
//! readers the `Window` members need (an objthis-only member receives no
//! native payload, so it resolves a property through the retained object
//! handle the VM marshalled).
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use tjs::{
    DetachedValue, Engine, RetainedValue, Tjs2Engine, TjsValue, VAL_ARRAY, VAL_INTEGER, VAL_NULL,
    VAL_OBJECT, VAL_OCTET, VAL_REAL, VAL_RETAINED, VAL_STRING, VAL_VOID, Value,
};

use crate::binding;

/// Scratch buffer for a string result. The C++ trampoline copies `out.string`
/// before the next native call, so a per-binding buffer (never a global) is
/// enough.
#[derive(Default)]
pub(crate) struct TextScratch(pub Vec<u8>);

/// Resolve the process-wide binding for the engine handle a callback received.
pub(crate) fn resolve(engine: *mut c_void) -> Option<binding::Resolved> {
    binding::resolve(engine as *mut Engine)
}

/// Write a real result into `out`.
pub(crate) fn set_real(out: *mut Value, value: f64) {
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

/// Write a string result into `out` through the per-binding scratch buffer.
pub(crate) fn set_string_out(out: *mut Value, services: &crate::CompatServices, string: &str) {
    let mut scratch = services.state::<TextScratch>();
    scratch.0.clear();
    scratch.0.extend_from_slice(string.as_bytes());
    scratch.0.push(0);
    if out.is_null() {
        return;
    }
    // SAFETY: the trampoline passes a valid out slot; the scratch buffer
    // outlives the call (the C++ side copies the bytes before returning).
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

/// Write a retained id into `out` (`VAL_RETAINED`); the C++ side consumes the
/// retention while converting the native result.
pub(crate) fn set_retained_out(out: *mut Value, id: tjs::Tjs2ValueId) {
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
        (*out).retained = id as usize;
    }
}

/// Marshal a scalar [`TjsValue`] into the result slot (objects must use the
/// retained path).
pub(crate) fn set_scalar_out(out: *mut Value, services: &crate::CompatServices, value: TjsValue) {
    match value {
        TjsValue::Void => crate::native_util::set_void(out),
        TjsValue::Integer(integer) => crate::native_util::set_int(out, integer),
        TjsValue::Real(real) => set_real(out, real),
        TjsValue::String(string) => set_string_out(out, services, &string),
        TjsValue::Object | TjsValue::Retained(_) => crate::native_util::set_void(out),
    }
}

/// Marshal a [`RetainedValue`] (or an error) into the native result slots.
pub(crate) fn finish_retained(
    out: *mut Value,
    out_error: *mut *mut c_char,
    services: &crate::CompatServices,
    result: Result<RetainedValue, String>,
) -> c_int {
    match result {
        Ok(RetainedValue::Value(value)) => {
            set_scalar_out(out, services, value);
            0
        }
        Ok(RetainedValue::Object(object)) => {
            set_retained_out(out, object.raw_id());
            // The C++ trampoline consumes the retention when it converts the
            // native result, so the guard must not release it first.
            std::mem::forget(object);
            0
        }
        Err(error) => crate::native_util::report_error(out_error, &error),
    }
}

/// Evaluate `%[]` and retain the resulting dictionary.
pub(crate) fn make_dict(engine: &Tjs2Engine, what: &str) -> Result<DetachedValue, String> {
    match engine
        .eval_retained("%[]", what)
        .map_err(|e| e.to_string())?
    {
        RetainedValue::Object(object) => Ok(object),
        RetainedValue::Value(_) => Err(format!("{what}: could not build a dictionary")),
    }
}

/// Retain a raw `objthis` object pointer.
pub(crate) fn retain_objthis(
    engine: &Tjs2Engine,
    objthis: *mut c_void,
) -> Result<DetachedValue, String> {
    engine
        .retain_object_detached(objthis)
        .map_err(|error| format!("windowEx: {error}"))
}

/// Read an integer-ish member from an `objthis` object.
///
/// The reference reads these through `ncbPropAccessor::getIntValue(name)`
/// (`reference/cpp/core/plugin/ncbind.hpp:689-695`), which returns the
/// default `0` when the member does not exist — so a missing (or
/// non-convertible) member reads as 0, exactly like `windowEx.cpp:405-408`
/// reads a bare window.
pub(crate) fn objthis_int(
    engine: &Tjs2Engine,
    objthis: *mut c_void,
    name: &str,
) -> Result<i64, String> {
    let object = retain_objthis(engine, objthis)?;
    match engine.member(object.raw_id(), name) {
        Ok(TjsValue::Integer(value)) => Ok(value),
        Ok(TjsValue::Real(value)) => Ok(value as i64),
        Ok(_) => Ok(0),
        // `getIntValue` returns its default for a missing member.
        Err(_) => Ok(0),
    }
}

/// Coerce a value to an integer exactly like the reference's
/// `tTJSVariant::AsInteger()`.
///
/// `reference/cpp/core/tjs2/tjsVariant.h:862-878`: `void` reads as 0,
/// integers as themselves, reals truncated toward zero, strings through
/// `tTJSVariantString::ToInteger()` (`reference/cpp/core/tjs2/tjsVariantString.cpp:457-466`,
/// a leading-number parse via `TJSParseNumber`), and objects/octet values
/// raise the variant conversion error — surfaced here as a typed error.
pub(crate) fn as_integer(value: &Value) -> Result<i64, String> {
    match value.ty {
        VAL_VOID => Ok(0),
        VAL_INTEGER => Ok(value.integer),
        VAL_REAL => Ok(value.real as i64),
        VAL_STRING => Ok(parse_number_prefix(&crate::native_util::value_as_string(
            value,
        ))),
        // tvtObject (including `null`) and tvtOctet raise
        // `TJSThrowVariantConvertError` in `AsInteger`.
        VAL_NULL | VAL_OBJECT | VAL_RETAINED | VAL_ARRAY | VAL_OCTET => {
            Err("cannot convert this value to an integer (variant conversion error)".into())
        }
        _ => Ok(0),
    }
}

/// `!!value.AsInteger()` — the truthiness test every `windowEx` flag write
/// uses (`windowEx.cpp:507,525,581,601,1085`).
pub(crate) fn as_bool(value: &Value) -> Result<bool, String> {
    Ok(as_integer(value)? != 0)
}

/// The leading-number parse `tTJSVariantString::ToInteger()` performs:
/// an optional sign, then a decimal / hexadecimal (`0x`) / binary (`0b`) /
/// octal (`0…`) number prefix; reals truncate through `AsInteger`. Anything
/// that does not start with a number parses as 0
/// (`reference/cpp/core/tjs2/tjsVariantString.cpp:457-466`).
fn parse_number_prefix(text: &str) -> i64 {
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    let mut negative = false;
    if index < chars.len() && (chars[index] == '+' || chars[index] == '-') {
        negative = chars[index] == '-';
        index += 1;
    }
    let rest: String = chars[index..].iter().collect();
    let lower = rest.to_ascii_lowercase();
    if lower.starts_with("true") {
        return if negative { -1 } else { 1 };
    }
    if lower.starts_with("false") {
        return 0;
    }
    let value = if let Some(hex) = lower.strip_prefix("0x") {
        let digits: String = hex.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        if digits.is_empty() {
            return 0;
        }
        i64::from_str_radix(&digits, 16).unwrap_or(i64::MAX)
    } else if let Some(bin) = lower.strip_prefix("0b") {
        let digits: String = bin.chars().take_while(|c| matches!(c, '0' | '1')).collect();
        if digits.is_empty() {
            return 0;
        }
        i64::from_str_radix(&digits, 2).unwrap_or(i64::MAX)
    } else if lower.starts_with('0') && lower[1..].starts_with(|c: char| c.is_ascii_digit()) {
        // leading-zero octal (`TJSParseNonDecimalNumber` with `TJSOctNum`)
        let digits: String = lower
            .chars()
            .take_while(|c| matches!(c, '0'..='7'))
            .collect();
        i64::from_str_radix(&digits, 8).unwrap_or(0)
    } else if lower.starts_with(|c: char| c.is_ascii_digit() || c == '.') {
        let end = leading_number_len(&lower);
        let number = &lower[..end];
        if number.is_empty() || number == "." {
            return 0;
        }
        if number.contains('.') || number.contains('e') {
            match number.parse::<f64>() {
                Ok(real) => real as i64,
                Err(_) => return 0,
            }
        } else {
            number.parse::<i64>().unwrap_or(i64::MAX)
        }
    } else {
        return 0;
    };
    if negative { -value } else { value }
}

/// Length of the leading decimal number in `text` (digits, one fraction dot,
/// one exponent).
fn leading_number_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut index = 0usize;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index < bytes.len() && bytes[index] == b'.' {
        index += 1;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
    }
    if index < bytes.len() && matches!(bytes[index], b'e' | b'E') {
        let mut probe = index + 1;
        if probe < bytes.len() && matches!(bytes[probe], b'+' | b'-') {
            probe += 1;
        }
        let digits_start = probe;
        while probe < bytes.len() && bytes[probe].is_ascii_digit() {
            probe += 1;
        }
        if probe > digits_start {
            index = probe;
        }
    }
    index
}

// ---------------------------------------------------------------------------
// per-object state guard
// ---------------------------------------------------------------------------

/// The private identity token stored *on the object* to guard state that is
/// keyed by its raw address.
///
/// The port has no per-object native payload, so plugin state is a
/// `HashMap<usize, …>` keyed by the `objthis` pointer. The reference hands
/// every object its **own** native instance and destroys it with the object
/// (`reference/cpp/plugins/windowEx.cpp:920-931`), so when a released
/// object's address is reused by a new one, the new object must read the
/// constructor defaults — not the previous occupant's flags. The first
/// access stamps the object with a unique token; a missing or mismatched
/// token resets the entry before it is used.
const STATE_TOKEN: &str = "__windowExToken";

/// A keyed state entry plus the identity token of the object it belongs to.
#[derive(Default)]
pub(crate) struct Tokened<V> {
    /// The token of the object this entry belongs to; `0` until claimed.
    pub(crate) token: i64,
    /// The stored flags for that object.
    pub(crate) value: V,
}

/// Read the identity token `objthis` carries, if any.
fn state_token(engine: &Tjs2Engine, objthis: *mut c_void) -> Option<i64> {
    let object = retain_objthis(engine, objthis).ok()?;
    match engine.member(object.raw_id(), STATE_TOKEN) {
        Ok(TjsValue::Integer(token)) => Some(token),
        _ => None,
    }
}

/// Ensure `entries[objthis]` describes the object that is *currently* at
/// that address; resets the entry (releasing whatever it retained) and
/// stamps the object with a fresh token when the address has been reused.
pub(crate) fn sync_object_state<V: Default>(
    entries: &mut std::collections::HashMap<usize, Tokened<V>>,
    next_token: &mut i64,
    engine: &Tjs2Engine,
    objthis: *mut c_void,
) -> Result<(), String> {
    let key = objthis as usize;
    if let Some(token) = state_token(engine, objthis)
        && entries.get(&key).is_some_and(|entry| entry.token == token)
    {
        // The same object as last time.
        return Ok(());
    }
    // A new object owns this address (or this is the first touch): claim a
    // fresh entry, then stamp the object so later calls recognise it.
    *next_token += 1;
    let token = *next_token;
    let entry = entries.entry(key).or_default();
    entry.token = token;
    entry.value = V::default();
    let object = retain_objthis(engine, objthis)?;
    engine.set_member(object.raw_id(), STATE_TOKEN, &TjsValue::Integer(token))
}

/// Whether a value carries an object handle (a native instance argument).
pub(crate) fn is_object(value: &Value) -> bool {
    value.ty == VAL_OBJECT || value.ty == VAL_RETAINED
}

/// Define a read-only static integer-property getter (the `Variant`
/// class constants the reference registers with `TJS_STATICMEMBER`).
macro_rules! int_property_getter {
    ($name:ident, $value:expr) => {
        extern "C" fn $name(
            _engine: *mut std::ffi::c_void,
            out: *mut tjs::Value,
            _out_error: *mut *mut std::ffi::c_char,
        ) -> std::ffi::c_int {
            crate::native_util::set_int(out, $value);
            0
        }
    };
}
pub(crate) use int_property_getter;

/// Whether the class-name chain of an object argument contains `name` (or its
/// native `nameInstance` form).
pub(crate) fn arg_has_class(engine: &Tjs2Engine, value: &Value, name: &str) -> bool {
    let object = match engine.retain_object_arg(value) {
        Ok(object) => object,
        Err(_) => return false,
    };
    let names = match engine.class_names(object.raw_id()) {
        Ok(names) => names,
        Err(_) => return false,
    };
    for index in 0..64 {
        match engine.member(names.raw_id(), &index.to_string()) {
            Ok(TjsValue::String(found)) => {
                if found == name || found == format!("{name}Instance") {
                    return true;
                }
            }
            _ => return false,
        }
    }
    false
}
