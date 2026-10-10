//! `Pad` member added by `windowEx.dll` (`struct PadEx`).
//!
//! The reference attaches `registerExEvent` to `Pad`; its implementation is
//! empty (the native pad window does not exist on this host). We register the
//! member so the surface matches and the call is accepted.

use std::ffi::{c_char, c_int, c_void};

use tjs::{Engine, NativeInstanceMembers, NativeObjThisMethodDef, Value};

use crate::CompatCtx;
use crate::CompatError;
use crate::binding;
use crate::native_util::{report_error, set_void};

extern "C" fn pad_register_ex_event(
    engine: *mut c_void,
    _objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if binding::resolve(engine as *mut Engine).is_none() {
        return report_error(
            out_error,
            "Pad.registerExEvent: compat context is not bound",
        );
    }
    set_void(out);
    0
}

/// Register the `Pad` member added by `windowEx.dll`.
pub(super) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance_members(&NativeInstanceMembers {
        class_name: "Pad",
        methods: vec![NativeObjThisMethodDef {
            name: "registerExEvent",
            f: pad_register_ex_event,
        }],
        properties: vec![],
    })
}
