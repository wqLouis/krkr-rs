//! `MenuItem` native class stub.
//!
//! k2compat's menu integration (`k2compat.tjs`) checks
//! `typeof global.MenuItem == "undefined"` and, when undefined, installs a
//! *delayed loader* whose getter runs `loadPlugin('menu.dll'); return
//! MenuItem;` — with native plugins unsupported, that getter **throws** when
//! the game later reads `global.MenuItem` (the `typeof` check itself fires
//! it, aborting startup). Registering a stub class makes the check see
//! `"Object"` and the whole menu-delay machinery (lines 233-241) is skipped,
//! exactly like the k2compat's own `%[]` fallbacks for Pad/console.
//!
//! The stub is a real instance class so `new MenuItem(win, sysarg)` parses
//! and no-ops; the debug-menu feature itself stays unimplemented.

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{NativeInstanceBuilder, NativeInstanceMethodDef, Tjs2Engine, Value};

use crate::set_void_out;

/// Instance payload (unused — the stub carries no state).
#[derive(Default)]
pub(crate) struct MenuItemInst;

extern "C" fn menu_item_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(MenuItemInst)) as *mut c_void
}

extern "C" fn menu_item_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: instance came from menu_item_create's Box::into_raw.
        drop(unsafe { Box::from_raw(instance as *mut MenuItemInst) });
    }
}

/// `new MenuItem(win, sysarg)` — accept the args, ignore them.
extern "C" fn menu_item_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    set_void_out(out);
    0
}

/// Register the stub `MenuItem` native class on the engine's global object.
pub fn register_menu_item(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "MenuItem",
        create: menu_item_create,
        destroy: menu_item_destroy,
        methods: vec![NativeInstanceMethodDef {
            name: "MenuItem", // the class-name ctor hook, like the visual natives
            f: menu_item_ctor,
        }],
        properties: vec![],
    })
}
