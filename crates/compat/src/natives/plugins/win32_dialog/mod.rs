//! `win32_dialog` plugin — the `win32dialog.dll` Win32-dialog emulation.
//!
//! Behavioral reference: `reference/cpp/plugins/win32dialog.cpp:6-10`, whose
//! whole registration is:
//!
//! ```cpp
//! static void InitPlugin_WIN32Dialog() {
//!     TVPExecuteScript(TJS_W("class WIN32Dialog {") TJS_W(
//!         "    function messageBox(message, caption, type) {return "
//!         "!System.inform(message, caption, 2);}") TJS_W("}"));
//! }
//! NCB_PRE_REGIST_CALLBACK(InitPlugin_WIN32Dialog);
//! ```
//!
//! It installs one class, `WIN32Dialog`, with one instance method,
//! `messageBox(message, caption, type)`, whose body negates
//! `System.inform(message, caption, 2)` — looked up **on the `System` object
//! at call time**, so a script override of `System.inform` wins, `message`
//! and `caption` pass through unchanged (`void` when the call omitted them,
//! exactly like the script class' parameters), and `type` is ignored in
//! favour of the literal `2`.
//!
//! The Win32 environment is emulated rather than refused: the
//! message box is shown through the already-emulated [`System.inform`]
//! (krkr-rs logs the text and reports the first button), so `messageBox`
//! returns the emulated button result instead of raising an "unsupported"
//! typed error. The `!` is applied with the reference `operator bool`
//! (`reference/cpp/core/tjs2/tjsVariant.h:911-932`, shared through
//! `natives::system::value_as_bool`), so a script override's return value is
//! negated exactly as KiriKiri would.
//!
//! The class is registered as a native instance class (rather than by
//! executing the reference script) so the behavior is owned by L3, while the
//! script-visible surface — `new WIN32Dialog().messageBox(...)` — is the
//! same. When `System.inform` cannot be reached — `System` is not an object
//! or the member (native or a script override) throws — the member reports a
//! typed TJS error rather than fabricating a result, so a broken host stays
//! visible; the reference propagates the same failure as a TJS
//! exception thrown from the script body.
//!
//! [`System.inform`]: crate::natives::system
#![allow(unsafe_code)]

use std::ffi::{CString, c_char, c_int, c_void};
use std::ptr;

use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, RetainedValue, Tjs2Engine,
    TjsValue, VAL_INTEGER, VAL_OBJECT, VAL_REAL, VAL_STRING, VAL_VOID, Value,
};

use crate::native_util::{args, report_error, set_bool, set_void, value_as_string};
use crate::natives::system::{resolve, value_as_bool};
use crate::{CompatCtx, CompatError};

/// Empty per-object payload: `WIN32Dialog` carries no state beyond the
/// synthetic dialog behavior.
struct DialogInstance;

/// Allocate the (empty) `WIN32Dialog` payload.
extern "C" fn dialog_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(DialogInstance)) as *mut c_void
}

/// Free a payload created by [`dialog_create`].
extern "C" fn dialog_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: `instance` came from `dialog_create`'s `Box::into_raw` and
        // is released exactly once.
        drop(unsafe { Box::from_raw(instance as *mut DialogInstance) });
    }
}

/// `new WIN32Dialog()` — accepts any arguments (the reference TJS class has
/// no constructor body).
extern "C" fn dialog_ctor(
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

/// An empty [`Value`] slot carrying `value`.
fn scalar_slot(value: &TjsValue) -> Value {
    let (ty, integer, real) = match value {
        TjsValue::Integer(value) => (VAL_INTEGER, *value, 0.0),
        TjsValue::Real(value) => (VAL_REAL, 0, *value),
        // Objects (`Object`/`Retained`) are truthy for `operator bool`
        // (`tjsVariant.h:917-918`: `Object.Object != nullptr`); the exact
        // pointer is irrelevant to the coercion.
        TjsValue::Object | TjsValue::Retained(_) => (VAL_OBJECT, 0, 0.0),
        _ => (VAL_VOID, 0, 0.0),
    };
    Value {
        ty,
        integer,
        real,
        string: ptr::null(),
        array: ptr::null(),
        array_count: 0,
        retained: 0,
    }
}

/// Reference `!value` for a native result: the reference script body negates
/// `System.inform(...)` (`reference/cpp/plugins/win32dialog.cpp:9`) with the
/// TJS `!` operator, i.e. `operator bool` (`tjsVariant.h:911-932`) — integers
/// and reals are non-zero, **strings parse their leading number token**
/// (`tjsVariant.h:920-921`, `tjsVariant.cpp:301-307`), void/`null` are false
/// and every other object is true. `natives::system::value_as_bool` carries
/// that coercion, so an override returning `"text"`, `7` or `void` is
/// negated exactly as KiriKiri would.
fn tjs_not(value: &TjsValue) -> bool {
    if let TjsValue::String(text) = value {
        // A result string crossed the ABI as a C string, so it never holds an
        // interior NUL; the fallback only guards the pathological case.
        let buffer = CString::new(text.as_str()).unwrap_or_default();
        let raw = Value {
            ty: VAL_STRING,
            integer: 0,
            real: 0.0,
            string: buffer.as_ptr(),
            array: ptr::null(),
            array_count: 0,
            retained: 0,
        };
        return !value_as_bool(&raw);
    }
    !value_as_bool(&scalar_slot(value))
}

/// Forward one `messageBox` argument to `System.inform` unchanged, so the
/// reference body's parameters pass through verbatim: a call that omits an
/// argument forwards `void` (the script class' parameter is `void`), and an
/// object keeps its handle (and closure receiver) so the override sees the
/// very same value (`tjs2_abi.cpp:339-414` records the handle per argument).
///
/// The object's [`DetachedValue`] is returned alongside the argument: the C++
/// side consumes the retention only while it converts the argument for
/// `System.inform`, so the caller must keep it alive until that call returns
/// (`tjs2_abi.cpp:478-489` raises when the id is already gone).
fn forward_arg(
    host: &Tjs2Engine,
    argument: Option<&Value>,
) -> Result<(TjsValue, Option<DetachedValue>), String> {
    let Some(value) = argument else {
        return Ok((TjsValue::Void, None));
    };
    match value.ty {
        VAL_VOID => Ok((TjsValue::Void, None)),
        VAL_INTEGER => Ok((TjsValue::Integer(value.integer), None)),
        VAL_REAL => Ok((TjsValue::Real(value.real), None)),
        VAL_STRING => Ok((TjsValue::String(value_as_string(value)), None)),
        VAL_OBJECT => host
            .retain_object_arg(value)
            .map(|retained| (TjsValue::Retained(retained.raw_id() as u64), Some(retained))),
        // `null` and octets have no `TjsValue` form: `null` models as `void`
        // (both are false for `!`), and an octet is not valid dialog text.
        _ => Ok((TjsValue::Void, None)),
    }
}

/// `WIN32Dialog.messageBox(message, caption, type)` → boolean.
///
/// The reference body is `!System.inform(message, caption, 2)`
/// (`win32dialog.cpp:8-9`): `message`/`caption` pass through as given (void
/// when omitted), `type` is ignored in favour of the literal `2`, `System`
/// is resolved through the object on every call — so a script override wins
/// — and the result is negated with the reference `operator bool`. A failed
/// member call is a typed error; the reference propagates the script
/// exception the same way.
extern "C" fn message_box(
    engine: *mut c_void,
    _instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: argv/argc follow the native-callback ABI for the call.
    let arguments = unsafe { args(argv, argc) };
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "WIN32Dialog.messageBox: compat context is not bound",
        );
    };
    let host = resolved.engine();
    // Resolve the `System` class object and call its `inform` member, so the
    // Win32 dialog and `System.inform` cannot diverge.
    let system = match host.eval_retained("System", "WIN32Dialog.messageBox") {
        Ok(RetainedValue::Object(object)) => object,
        Ok(_) => {
            return report_error(out_error, "WIN32Dialog.messageBox: System is not an object");
        }
        Err(error) => {
            return report_error(
                out_error,
                &format!("WIN32Dialog.messageBox: cannot resolve System: {error}"),
            );
        }
    };
    let (message, message_retained) = match forward_arg(host, arguments.first()) {
        Ok((value, retained)) => (value, retained),
        Err(error) => {
            return report_error(
                out_error,
                &format!("WIN32Dialog.messageBox: cannot forward message: {error}"),
            );
        }
    };
    let (caption, caption_retained) = match forward_arg(host, arguments.get(1)) {
        Ok((value, retained)) => (value, retained),
        Err(error) => {
            return report_error(
                out_error,
                &format!("WIN32Dialog.messageBox: cannot forward caption: {error}"),
            );
        }
    };
    let result = host.call_member(
        system.raw_id(),
        "inform",
        &[message, caption, TjsValue::Integer(2)],
    );
    // The C++ side consumed any object retention while converting the
    // arguments; dropping the handles now is the idempotent release.
    drop(message_retained);
    drop(caption_retained);
    match result {
        Ok(value) => {
            // `return !System.inform(message, caption, 2);`
            set_bool(out, tjs_not(&value));
            0
        }
        Err(error) => report_error(
            out_error,
            &format!("WIN32Dialog.messageBox: System.inform failed: {error}"),
        ),
    }
}

/// Register the `WIN32Dialog` native class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "WIN32Dialog",
        create: dialog_create,
        destroy: dialog_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "WIN32Dialog",
                f: dialog_ctor,
            },
            NativeInstanceMethodDef {
                name: "messageBox",
                f: message_box,
            },
        ],
        properties: vec![],
    })
}
