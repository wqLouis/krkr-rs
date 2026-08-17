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
//! * `getList()` — returns an array of the emulated plugin names currently
//!   linked. The C ABI now supports string arrays, so this matches the
//!   reference shape for script callers.
//!
//! `Plugins` is a static class: the reference's `CreateNativeInstance`
//! throws `TVPCannotCreateInstance`, so there is no per-object state and no
//! instance natives.

use std::ffi::{c_char, c_int, c_void};
use std::sync::{LazyLock, Mutex};

use tjs2_sys::{NativeClassBuilder, NativeMethodDef, VAL_ARRAY, Value};

use super::{args, report_error, set_int_out, set_void_out, value_as_string};

static LOADED_PLUGINS: LazyLock<Mutex<Vec<String>>> = LazyLock::new(|| Mutex::new(Vec::new()));

thread_local! {
    static ARRAY_OUT: std::cell::RefCell<(Vec<*const c_char>, Vec<Vec<u8>>)> =
        const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
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
        unsafe {
            (*out).ty = VAL_ARRAY;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = std::ptr::null();
            (*out).array = slot.0.as_ptr();
            (*out).array_count = slot.0.len() as c_int;
        }
    });
}

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
    let normalized = name.to_ascii_lowercase();
    let emulated = matches!(
        normalized.as_str(),
        "csvparser.dll"
            | "fstat.dll"
            | "kagparser.dll"
            | "kagparserex.dll"
            | "menu.dll"
            | "wuvorbis.dll"
    );
    if emulated {
        log::info!("Plugins.link({name:?}): emulated by built-in Rust natives");
        let mut loaded = LOADED_PLUGINS.lock().unwrap_or_else(|p| p.into_inner());
        if !loaded.iter().any(|item| item.eq_ignore_ascii_case(&name)) {
            loaded.push(name);
        }
    } else {
        log::info!("Plugins.link({name:?}): optional native plugin ignored");
    }
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
    log::info!("Plugins.unlink({name:?})");
    LOADED_PLUGINS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retain(|item| !item.eq_ignore_ascii_case(&name));
    set_int_out(out, 1);
    0
}

/// `Plugins.getList()` → array of loaded plugin names.
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
    let loaded = LOADED_PLUGINS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clone();
    set_array_out(out, &loaded);
    0
}

/// Register the static `Plugins` native class.
pub fn register_plugins(engine: &tjs2_sys::Tjs2Engine) -> Result<(), String> {
    LOADED_PLUGINS
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .clear();
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
    fn get_list_returns_loaded_plugin_names() {
        let _vm_lock = vm_lock();
        let e = registered_engine();
        e.exec_script("Plugins.link('csvParser.dll');", "test")
            .unwrap();
        assert_eq!(
            e.eval("Plugins.getList().count", "test").unwrap(),
            TjsValue::Integer(1)
        );
        assert_eq!(
            e.eval("Plugins.getList()[0]", "test").unwrap(),
            TjsValue::String("csvParser.dll".into())
        );
    }
}
