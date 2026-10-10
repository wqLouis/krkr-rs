//! `Console` members added by `windowEx.dll` (`struct ConsoleEx`).
//!
//! The reference attaches these to the `Debug.console` singleton through
//! `NCB_ATTACH_FUNCTION_WITHTAG(..., Debug.console, ...)`
//! (`reference/cpp/plugins/windowEx.cpp:1333-1346`). Our ABI cannot attach to
//! a dotted object path, but the singleton is a `Console` instance, so
//! attaching the objthis-only members to the `Console` class gives the
//! singleton the same surface (and leaves `new Console()` working too).
//!
//! Every reference body is inert: `restoreMaximize`/`maximize`/`getRect`/
//! `bringAfter`/`getPlacement` return `TJS_S_OK` without writing the result
//! (so they read back as **void**), `setPos` only validates its argument
//! count, and `setPlacement` only validates its argument count and type
//! (`windowEx.cpp:1290-1331`). Nothing is stored, so this module keeps no
//! state.

use std::ffi::{c_char, c_int, c_void};

use tjs::{Engine, NativeInstanceMembers, NativeObjThisMethodDef, Value};

use crate::CompatCtx;
use crate::CompatError;
use crate::binding;
use crate::native_util::{args, report_error, set_void};

use super::util::is_object;

/// `restoreMaximize`/`maximize`/`getRect`/`bringAfter`/`getPlacement` —
/// accepted and inert. The reference bodies only `return TJS_S_OK`, which
/// leaves the result slot untouched, so these read back as void
/// (`reference/cpp/plugins/windowEx.cpp:1290-1321`).
extern "C" fn console_inert(
    engine: *mut c_void,
    _objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if binding::resolve(engine as *mut Engine).is_none() {
        return report_error(out_error, "Debug.console: compat context is not bound");
    }
    set_void(out);
    0
}

/// `setPos(x, y)` — `if(n < 2) return TJS_E_BADPARAMCOUNT;`
/// (`reference/cpp/plugins/windowEx.cpp:1306-1311`); the reference stores
/// nothing, so the call only validates its argument count.
extern "C" fn console_set_pos(
    engine: *mut c_void,
    _objthis: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Debug.console.setPos requires 2 arguments");
    }
    if binding::resolve(engine as *mut Engine).is_none() {
        return report_error(
            out_error,
            "Debug.console.setPos: compat context is not bound",
        );
    }
    set_void(out);
    0
}

/// `setPlacement(value)` — `if(n < 1) return TJS_E_BADPARAMCOUNT;` and
/// `if(p[0]->Type() != tvtObject) return TJS_E_INVALIDPARAM;`
/// (`reference/cpp/plugins/windowEx.cpp:1323-1331`); the placement is never
/// stored (`getPlacement` does not read it back, `windowEx.cpp:1318-1321`).
extern "C" fn console_set_placement(
    engine: *mut c_void,
    _objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Debug.console.setPlacement requires 1 argument");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    // `null` is `tvtObject` too (`reference/cpp/core/tjs2/tjsVariant.h:552-557`),
    // so only non-object scalars raise `TJS_E_INVALIDPARAM`.
    if !is_object(&arguments[0]) && arguments[0].ty != tjs::VAL_NULL {
        return report_error(
            out_error,
            "Debug.console.setPlacement: the argument must be an object",
        );
    }
    if binding::resolve(engine as *mut Engine).is_none() {
        return report_error(
            out_error,
            "Debug.console.setPlacement: compat context is not bound",
        );
    }
    set_void(out);
    0
}

/// Register the `Console` members added by `windowEx.dll`.
pub(super) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance_members(&NativeInstanceMembers {
        class_name: "Console",
        methods: vec![
            NativeObjThisMethodDef {
                name: "restoreMaximize",
                f: console_inert,
            },
            NativeObjThisMethodDef {
                name: "maximize",
                f: console_inert,
            },
            NativeObjThisMethodDef {
                name: "getRect",
                f: console_inert,
            },
            NativeObjThisMethodDef {
                name: "setPos",
                f: console_set_pos,
            },
            NativeObjThisMethodDef {
                name: "getPlacement",
                f: console_inert,
            },
            NativeObjThisMethodDef {
                name: "setPlacement",
                f: console_set_placement,
            },
            NativeObjThisMethodDef {
                name: "bringAfter",
                f: console_inert,
            },
        ],
        properties: vec![],
    })
}
