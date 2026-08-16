//! `Plugins` native class — ported from
//! `reference/cpp/core/plugin/PluginImpl.cpp`
//! (`TVPCreateNativeClass_Plugins`) + `PluginIntf.cpp`
//! (`tTJSNC_Plugins::tTJSNC_Plugins`).
//!
//! The reference loads native plugin DLLs (`.dll`/`.tpm`) into the engine
//! via `ncbAutoRegister::LoadModule`. krkr-rs does **not** load native
//! plugins — the games treat them as optional (a failed load only logs) —
//! so:
//!
//! * `link(name)` — accepts the plugin name, logs the load attempt at
//!   `info!`, and returns successfully (void). The reference
//!   (`TVPLoadPlugin`) logs `"Loading Plugin: ... Success"` at `debug!` /
//!   `"Failed"` at `error!` and always returns `TJS_S_OK`; our "success"
//!   mirrors the reference's tolerant behavior (it never throws for a
//!   missing plugin).
//! * `unlink(name)` — returns `true`, like the reference
//!   `TVPUnloadPlugin`, which always returns true.
//! * `getList()` — the reference returns an array of loaded plugin names
//!   (`TVPRegisteredPlugins`). Object results cannot cross the C ABI yet
//!   (see the crate docs), so this returns void with a warning and is
//!   documented as pending. No game startup script calls it.
//!
//! `Plugins` is a static class: the reference's `CreateNativeInstance`
//! throws `TVPCannotCreateInstance`, so there is no per-object state and no
//! instance natives.

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{NativeClassBuilder, NativeMethodDef, Value};

use super::{args, report_error, set_int_out, set_void_out, value_as_string};

/// `Plugins.link(name)` → void
///
/// Reference: `TVPLoadPlugin(name)` — loads a native plugin DLL.
/// krkr-rs does not load native plugins; logs the attempt at `info!` and
/// returns void. Requires exactly one argument (the reference returns
/// `TJS_E_BADPARAMCOUNT` on fewer).
extern "C" fn native_link(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Plugins.link requires 1 argument");
    }
    let name = value_as_string(&args(argv, argc)[0]);
    log::info!("Plugins.link({name:?}): native plugins are not supported by krkr-rs; ignored");
    set_void_out(out);
    0
}

/// `Plugins.unlink(name)` → bool
///
/// Reference: `TVPUnloadPlugin(name)` always returns `true`; the method
/// returns that boolean. krkr-rs never loads plugins, so unlink is a
/// no-op returning true.
extern "C" fn native_unlink(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Plugins.unlink requires 1 argument");
    }
    let name = value_as_string(&args(argv, argc)[0]);
    log::info!("Plugins.unlink({name:?}): no-op (native plugins are not loaded)");
    set_int_out(out, 1);
    0
}

/// `Plugins.getList()` → array (pending)
///
/// Reference: returns an array of the loaded plugin names
/// (`TVPRegisteredPlugins`). The C ABI carries no object handle, so this
/// port returns void with a warning; no game startup script uses it.
extern "C" fn native_get_list(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc != 0 {
        return report_error(out_error, "Plugins.getList takes no arguments");
    }
    log::warn!(
        "Plugins.getList(): the reference returns an array of loaded plugin names; \
         object results cannot cross the C ABI yet — returning void"
    );
    set_void_out(out);
    0
}

/// Register the static `Plugins` native class.
pub fn register_plugins(engine: &tjs2_sys::Tjs2Engine) -> Result<(), String> {
    engine.register_native_class(&NativeClassBuilder {
        name: "Plugins",
        // static class, no properties (the reference registers none)
        properties: Vec::new(),
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
    })
}

#[cfg(test)]
mod tests {
    use crate::test_lock::vm_lock;

    use super::*;
    use tjs2_sys::{Tjs2Engine, TjsValue};

    fn registered_engine() -> Tjs2Engine {
        let e = Tjs2Engine::new().expect("create engine");
        register_plugins(&e).expect("register Plugins");
        e
    }

    #[test]
    fn link_accepts_dll_names_and_returns_void() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        // the startup.tjs idiom: `Plugins.link("extrans.dll");` etc.
        e.exec_script(
            "Plugins.link('extrans.dll'); Plugins.link('csvParser.dll');",
            "test",
        )
        .unwrap();
        // the reference requires at least one argument
        assert!(e.eval("Plugins.link()", "test").is_err());
    }

    #[test]
    fn unlink_returns_true() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        assert_eq!(
            e.eval("Plugins.unlink('extrans.dll')", "test").unwrap(),
            TjsValue::Integer(1)
        );
        assert!(e.eval("Plugins.unlink()", "test").is_err());
    }

    #[test]
    fn get_list_is_void_pending() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        // pending FFI: returns void (not an array)
        assert_eq!(e.eval("Plugins.getList()", "test").unwrap(), TjsValue::Void);
    }
}
