//! `Controller` / `Console` native class objects exposed through
//! `Debug.controller` / `Debug.console` (reference
//! `core/utils/impl/DebugImpl.cpp:36-180`).
//!
//! The reference's `Debug.controller` / `Debug.console` are read-only static
//! properties that return the singleton class dispatch object of the
//! `Controller` / `Console` native class (a `tTJSNativeClass`, not a fresh
//! instance). Each class carries a `visible` static property whose setter is
//! a no-op and whose getter reports `0`/`false` in this headless port.
//!
//! These builders live in their own file on purpose: the parity checker
//! unions every builder in `debug.rs` under the `Debug` class, so defining
//! them next to `Debug` would surface their members as `Debug` members.

use std::ffi::{c_char, c_int};

use tjs2_sys::{NativeClassBuilder, NativePropertyDef, Tjs2Engine, Value};

use crate::set_int_out;

/// `Controller.visible` / `Console.visible` getter. The reference returns the
/// host window/console visibility; a headless port has neither, so it reports
/// the reference's "not shown" value (`0` / `false`).
extern "C" fn visible_get(
    _engine: *mut std::ffi::c_void,
    out: *mut Value,
    _err: *mut *mut c_char,
) -> c_int {
    set_int_out(out, 0);
    0
}

/// `Controller.visible` / `Console.visible` setter: the reference setter is a
/// no-op in this build (`// TVPMainForm->setVisible(...)` is commented out).
extern "C" fn visible_set(
    _engine: *mut std::ffi::c_void,
    _value: *const Value,
    _err: *mut *mut c_char,
) -> c_int {
    0
}

/// Register `Controller` and `Console` as static native classes. Must run
/// before any script reads `Debug.controller` / `Debug.console`.
pub fn register_controller_console(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class(&NativeClassBuilder {
        name: "Controller",
        properties: vec![NativePropertyDef {
            name: "visible",
            get: Some(visible_get),
            set: Some(visible_set),
        }],
        methods: vec![],
    })?;
    engine.register_native_class(&NativeClassBuilder {
        name: "Console",
        properties: vec![NativePropertyDef {
            name: "visible",
            get: Some(visible_get),
            set: Some(visible_set),
        }],
        methods: vec![],
    })
}
