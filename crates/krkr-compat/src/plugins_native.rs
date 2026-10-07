//! The `Plugins` native class: script-facing plugin linking.
//!
//! `link(name)` activates a built-in plugin emulation or installs the [null
//! plugin object](crate::null_plugin); `unlink(name)` forgets a linked name;
//! `getList()` returns the linked names. This mirrors the reference
//! `Plugins` class (static members only) with the null-plugin behavior from
//! doc 08 §4.2 wired in.

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{Engine, NativeClassBuilder, NativeMethodDef, VAL_ARRAY, Value};

use crate::binding;
use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::{CompatCtx, CompatError, PluginOutcome};

/// Write the linked-name array into `out`.
///
/// The scratch buffers live in the L3 [`CompatServices`](crate::CompatServices)
/// dispatch state (not a thread-local), so they stay alive until the next
/// native call while the C++ trampoline copies the array.
fn set_array_out(out: *mut Value, ctx: &CompatCtx, values: &[String]) {
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let mut slot = services.linked_array_scratch();
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
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let name = value_as_string(&unsafe { args(argv, argc) }[0]);
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

/// `Plugins.unlink(name)` → bool (whether the name had been linked).
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
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let name = value_as_string(&unsafe { args(argv, argc) }[0]);
    let Some(resolved) = binding::resolve(engine as *mut Engine) else {
        return report_error(out_error, "Plugins.unlink: compat context is not bound");
    };
    let mut ctx = resolved.ctx();
    let removed = resolved.registry().unlink(&name, &mut ctx);
    log::info!("Plugins.unlink({name:?}) -> {removed}");
    set_int(out, i64::from(removed));
    0
}

/// `Plugins.getList()` → array of linked plugin names (insertion order, the
/// script's original casing).
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
    // Bind the context before borrowing the registry so the scratch buffers
    // are owned by the same services the callback was resolved against.
    let ctx = resolved.ctx();
    let names = resolved.registry().linked();
    set_array_out(out, &ctx, &names);
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
