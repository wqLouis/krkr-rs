//! Marshalling helpers shared by the `motion_player` native callbacks.
//!
//! Every callback funnels through one of the `get_*` / `set_*` / `method_*`
//! entry points below. They resolve the process-wide engine binding, recover
//! the Rust instance payload and translate the result back into the raw
//! `tjs2_value` ABI slot. Keeping the unsafe pointer code here lets the
//! per-member callbacks stay one-line closures.

use std::ffi::{c_char, c_int, c_void};
use std::ptr;

use tjs::{RetainedValue, TjsValue, VAL_INTEGER, VAL_REAL, VAL_RETAINED, VAL_STRING, Value};

use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{resolve, value_as_bool, value_as_i64};

/// Error message used whenever the compat binding has gone away.
pub(crate) const NO_BINDING: &str = "motionplayer: compat context is not bound";

/// The reference `ncbind` argument-count gate
/// (`reference/cpp/core/plugin/ncbind.hpp:1178`): a native method registered
/// with `NCB_METHOD` fails with `TJS_E_BADPARAMCOUNT` when the script passes
/// fewer arguments than the C++ arity. The compat callbacks surface the same
/// failure as a typed callback error instead of silently substituting a
/// default.
pub(crate) fn require_args(args: &[Value], min: usize, method: &str) -> Result<(), String> {
    if args.len() < min {
        Err(format!("{method}: bad parameter count"))
    } else {
        Ok(())
    }
}

/// Whether `objthis` is an `EmotePlayer` / `D3DEmotePlayer` shell instance.
///
/// The reference registers the shell-only members (`getRot`, `skip`,
/// `queing`, ...) exclusively on the `EmotePlayer` classes
/// (`reference/cpp/plugins/motionplayer/main.cpp:300-401`), while the plain
/// `Player` class never has them (`main.cpp:129-296`). Probing for `getRot`
/// therefore tells the shared callbacks which arity/behaviour set the
/// reference uses for the receiver — no registration-table changes needed.
pub(crate) fn is_emote_instance(ctx: &crate::CompatCtx, objthis: *mut c_void) -> bool {
    if objthis.is_null() {
        return false;
    }
    // SAFETY: the VM passed a live `objthis` for this callback; the binding
    // was resolved by the caller, so the engine outlives the retained id.
    let Ok(target) = (unsafe { ctx.engine() }).retain_object_detached(objthis) else {
        return false;
    };
    // SAFETY: `target` keeps the id valid for this call.
    matches!(
        unsafe { ctx.engine() }.get_member(target.raw_id(), "getRot"),
        Ok(value) if !matches!(value, TjsValue::Void)
    )
}

/// Recover the mutable instance payload.
///
/// # Safety
/// `instance` must be the live payload produced by the matching `create`
/// callback (the VM guarantees this for the duration of the call).
pub(crate) unsafe fn inst_mut<'a, T>(instance: *mut c_void) -> Option<&'a mut T> {
    if instance.is_null() {
        None
    } else {
        // SAFETY: the caller upholds the payload contract above.
        Some(unsafe { &mut *(instance as *mut T) })
    }
}

/// Recover the shared instance payload.
///
/// # Safety
/// See [`inst_mut`].
pub(crate) unsafe fn inst_ref<'a, T>(instance: *mut c_void) -> Option<&'a T> {
    if instance.is_null() {
        None
    } else {
        // SAFETY: the caller upholds the payload contract above.
        Some(unsafe { &*(instance as *const T) })
    }
}

/// TJS `AsReal` coercion for integer/real/string values.
pub(crate) fn value_as_f64(v: &Value) -> f64 {
    match v.ty {
        VAL_INTEGER => v.integer as f64,
        VAL_REAL => v.real,
        VAL_STRING => value_as_string(v).parse().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// Write a real into an out slot.
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

/// Write a string into an out slot, keeping the bytes alive in the per-engine
/// scratch buffer.
pub(crate) fn set_string(ctx: &crate::CompatCtx, out: *mut Value, text: &str) {
    if out.is_null() {
        return;
    }
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let services = unsafe { ctx.services() };
    let mut scratch = services.state::<super::MotionState>();
    scratch.string.clear();
    scratch.string.extend_from_slice(text.as_bytes());
    scratch.string.push(0);
    // SAFETY: the trampoline passes a valid out slot for the call.
    unsafe {
        (*out).ty = VAL_STRING;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = scratch.string.as_ptr() as *const c_char;
        (*out).array = ptr::null();
        (*out).array_count = 0;
        (*out).retained = 0;
    }
}

/// Write a retained object result, transferring ownership of `value` to the
/// C++ side (which consumes the retention while copying the result).
pub(crate) fn set_retained(out: *mut Value, value: tjs::DetachedValue) {
    if !out.is_null() {
        // SAFETY: the trampoline passes a valid out slot for the call.
        unsafe {
            (*out).ty = VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = ptr::null();
            (*out).array = ptr::null();
            (*out).array_count = 0;
            (*out).retained = value.raw_id() as usize;
        }
    }
    std::mem::forget(value);
}

/// Write a scalar [`TjsValue`] into an out slot (object values are void).
pub(crate) fn set_scalar(ctx: &crate::CompatCtx, out: *mut Value, value: TjsValue) {
    match value {
        TjsValue::Void | TjsValue::Object | TjsValue::Retained(_) => set_void(out),
        TjsValue::Integer(i) => set_int(out, i),
        TjsValue::Real(r) => set_real(out, r),
        TjsValue::String(s) => set_string(ctx, out, &s),
    }
}

/// Getter helper for a `f64` member.
pub(crate) fn get_real<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&T) -> f64,
) -> c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_ref::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    set_real(out, f(inst));
    0
}

/// Setter helper for a `f64` member.
pub(crate) fn set_real_member<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&mut T, f64),
) -> c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    // SAFETY: the VM passes a valid value for the duration of the call.
    let v = if value.is_null() {
        0.0
    } else {
        match unsafe { &*value }.ty {
            VAL_REAL => unsafe { (*value).real },
            VAL_INTEGER => unsafe { (*value).integer as f64 },
            VAL_STRING => value_as_string(unsafe { &*value }).parse().unwrap_or(0.0),
            _ => 0.0,
        }
    };
    f(inst, v);
    0
}

/// Getter helper for an integer member.
pub(crate) fn get_int<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&T) -> i64,
) -> c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_ref::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    set_int(out, f(inst));
    0
}

/// Setter helper for an integer member.
pub(crate) fn set_int_member<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&mut T, i64),
) -> c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    // SAFETY: the VM passes a valid value for the duration of the call.
    let v = if value.is_null() {
        0
    } else {
        value_as_i64(unsafe { &*value })
    };
    f(inst, v);
    0
}

/// Getter helper for a boolean member.
pub(crate) fn get_bool<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&T) -> bool,
) -> c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_ref::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    set_bool(out, f(inst));
    0
}

/// Setter helper for a boolean member.
pub(crate) fn set_bool_member<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&mut T, bool),
) -> c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    // SAFETY: the VM passes a valid value for the duration of the call.
    let v = if value.is_null() {
        false
    } else {
        value_as_bool(unsafe { &*value })
    };
    f(inst, v);
    0
}

/// Getter helper for a string member.
pub(crate) fn get_string<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&T) -> String,
) -> c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_ref::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    set_string(&res.ctx(), out, &f(inst));
    0
}

/// Setter helper for a string member.
pub(crate) fn set_string_member<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&mut T, String),
) -> c_int {
    let Some(_res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    // SAFETY: the VM passes a valid value for the duration of the call.
    let v = if value.is_null() {
        String::new()
    } else {
        value_as_string(unsafe { &*value })
    };
    f(inst, v);
    0
}

/// Getter helper for a member that yields a retained TJS value.
pub(crate) fn get_variant<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&T, &crate::CompatCtx) -> Result<RetainedValue, String>,
) -> c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_ref::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    match f(inst, &res.ctx()) {
        Ok(RetainedValue::Value(v)) => {
            set_scalar(&res.ctx(), out, v);
            0
        }
        Ok(RetainedValue::Object(dv)) => {
            set_retained(out, dv);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Setter helper for a member that consumes a raw TJS value.
pub(crate) fn set_variant_member<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&mut T, super::StoredValue),
) -> c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    // SAFETY: the VM passes a valid value for the duration of the call.
    let stored = if value.is_null() {
        super::StoredValue::Void
    } else {
        super::StoredValue::from_value(unsafe { &*value }, &res.ctx())
    };
    f(inst, stored);
    0
}

/// Instance method helper for a `void` return.
pub(crate) fn method_void<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&mut T, &[Value], &crate::CompatCtx) -> Result<(), String>,
) -> c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries for the call.
    let arguments = unsafe { args(argv, argc) };
    match f(inst, arguments, &res.ctx()) {
        Ok(()) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Instance method helper for an integer return.
pub(crate) fn method_int<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&mut T, &[Value], &crate::CompatCtx) -> Result<i64, String>,
) -> c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries for the call.
    let arguments = unsafe { args(argv, argc) };
    match f(inst, arguments, &res.ctx()) {
        Ok(v) => {
            set_int(out, v);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Instance method helper for a `f64` return.
pub(crate) fn method_real<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&mut T, &[Value], &crate::CompatCtx) -> Result<f64, String>,
) -> c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries for the call.
    let arguments = unsafe { args(argv, argc) };
    match f(inst, arguments, &res.ctx()) {
        Ok(v) => {
            set_real(out, v);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Instance method helper for a boolean return.
pub(crate) fn method_bool<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&mut T, &[Value], &crate::CompatCtx) -> Result<bool, String>,
) -> c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries for the call.
    let arguments = unsafe { args(argv, argc) };
    match f(inst, arguments, &res.ctx()) {
        Ok(v) => {
            set_bool(out, v);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Instance method helper for a string return.
pub(crate) fn method_string<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&mut T, &[Value], &crate::CompatCtx) -> Result<String, String>,
) -> c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries for the call.
    let arguments = unsafe { args(argv, argc) };
    match f(inst, arguments, &res.ctx()) {
        Ok(v) => {
            set_string(&res.ctx(), out, &v);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Instance method helper for a retained TJS value return.
pub(crate) fn method_variant<T>(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    f: impl FnOnce(&mut T, &[Value], &crate::CompatCtx) -> Result<RetainedValue, String>,
) -> c_int {
    let Some(res) = resolve(engine) else {
        return report_error(out_error, NO_BINDING);
    };
    // SAFETY: the VM guarantees the live payload.
    let Some(inst) = (unsafe { inst_mut::<T>(instance) }) else {
        return report_error(out_error, "motionplayer: null instance");
    };
    // SAFETY: the trampoline guarantees `argc` valid entries for the call.
    let arguments = unsafe { args(argv, argc) };
    match f(inst, arguments, &res.ctx()) {
        Ok(RetainedValue::Value(v)) => {
            set_scalar(&res.ctx(), out, v);
            0
        }
        Ok(RetainedValue::Object(dv)) => {
            set_retained(out, dv);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

/// Build a TJS object expression and retain it.
pub(crate) fn eval_object(
    ctx: &crate::CompatCtx,
    expression: &str,
) -> Result<tjs::DetachedValue, String> {
    match unsafe { ctx.engine() }.eval_retained(expression, "motion_player") {
        Ok(tjs::RetainedValue::Object(value)) => Ok(value),
        Ok(_) => Err("motionplayer: expression did not yield an object".into()),
        Err(error) => Err(error.to_string()),
    }
}

/// Escape a string as a TJS double-quoted literal.
pub(crate) fn tjs_string_literal(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}
