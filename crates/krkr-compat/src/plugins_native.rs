//! The `Plugins` native class: script-facing plugin linking.
//!
//! `link(name)` activates a built-in plugin emulation or installs the [null
//! plugin object](crate::null_plugin); `unlink(name)` forgets a linked name;
//! `getList()` returns the linked names. This mirrors the reference
//! `Plugins` class (static members only) with the null-plugin behavior from
//! doc 08 §4.2 wired in.

use std::cell::RefCell;
use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{Engine, NativeClassBuilder, NativeMethodDef, VAL_ARRAY, Value};

use crate::binding;
use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::{CompatCtx, CompatError, PluginOutcome};

thread_local! {
    /// Scratch storage for the `getList` string array, kept alive until the
    /// next native call on this thread (the C++ trampoline copies it
    /// immediately after the callback returns).
    static ARRAY_OUT: RefCell<(Vec<*const c_char>, Vec<Vec<u8>>)> =
        const { RefCell::new((Vec::new(), Vec::new())) };
}

fn set_array_out(out: *mut Value, values: &[String]) {
    ARRAY_OUT.with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.1.clear();
        slot.0.clear();
        for value in values {
            let mut bytes = value.as_bytes().to_vec();
            bytes.push(0);
            slot.1.push(bytes);
        }
        let pointers: Vec<*const c_char> = slot
            .1
            .iter()
            .map(|bytes| bytes.as_ptr() as *const c_char)
            .collect();
        slot.0.extend(pointers);
        if !out.is_null() {
            // SAFETY: the trampoline passes a valid out slot for the call.
            unsafe {
                (*out).ty = VAL_ARRAY;
                (*out).integer = 0;
                (*out).real = 0.0;
                (*out).string = std::ptr::null();
                (*out).array = slot.0.as_ptr();
                (*out).array_count = slot.0.len() as c_int;
                (*out).retained = 0;
            }
        }
    });
}

/// `Plugins.link(name)` → void.
extern "C" fn native_link(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Plugins.link requires 1 argument");
    }
    let name = value_as_string(&args(argv, argc)[0]);
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        return report_error(out_error, "Plugins.link: compat context is not bound");
    };
    let mut ctx = resolved.ctx();
    match resolved.registry().activate(&name, &mut ctx) {
        Ok(PluginOutcome::Builtin) => {
            log::info!("Plugins.link({name:?}): built-in emulation activated");
            set_void(out);
            0
        }
        Ok(PluginOutcome::Null) => {
            log::warn!(
                "Plugins.link({name:?}): no built-in emulation; installed a null plugin object"
            );
            set_void(out);
            0
        }
        Err(err) => report_error(out_error, &err.to_string()),
    }
}

/// `Plugins.unlink(name)` → bool (`true`, like the reference).
extern "C" fn native_unlink(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Plugins.unlink requires 1 argument");
    }
    let name = value_as_string(&args(argv, argc)[0]);
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        return report_error(out_error, "Plugins.unlink: compat context is not bound");
    };
    resolved.registry().unlink(&name);
    log::info!("Plugins.unlink({name:?})");
    set_int(out, 1);
    0
}

/// `Plugins.getList()` → array of linked plugin names.
extern "C" fn native_get_list(
    engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc != 0 {
        return report_error(out_error, "Plugins.getList takes no arguments");
    }
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        return report_error(out_error, "Plugins.getList: compat context is not bound");
    };
    set_array_out(out, &resolved.registry().linked());
    0
}

/// Register the `Plugins` native class.
pub(crate) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_class(&NativeClassBuilder {
        name: "Plugins",
        methods: vec![
            NativeMethodDef {
                name: "link",
                f: native_link,
            },
            NativeMethodDef {
                name: "unlink",
                f: native_unlink,
            },
            NativeMethodDef {
                name: "getList",
                f: native_get_list,
            },
        ],
        properties: vec![],
    })
}
