//! Minimal `Window` native class stub.
//!
//! Real games' `startup.tjs` begins with `delete Window.innerSunken;` and
//! `delete Window.showScrollBars;` — the real `Window` class lives in the
//! visual module (Bevy, a later wave). Until then, register the two
//! properties games delete, so startup scripts don't fail on them.
//!
//! `delete` on a registered class member removes the member (verified in
//! tjs2-sys tests), so the game-visible behavior matches: after delete, the
//! property no longer exists on the class.

use tjs2_sys::{NativeClassBuilder, NativePropertyDef, Tjs2Engine};

/// Register the stub `Window` class.
pub fn register_window(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class(&NativeClassBuilder {
        name: "Window",
        methods: vec![
            // Keep a method table placeholder; instance natives (window
            // creation etc.) land with the Bevy render module.
        ],
        properties: vec![
            NativePropertyDef {
                name: "innerSunken",
                get: Some(window_inner_sunken_get),
                set: Some(window_inner_sunken_set),
            },
            NativePropertyDef {
                name: "showScrollBars",
                get: Some(window_show_scroll_bars_get),
                set: Some(window_show_scroll_bars_set),
            },
        ],
    })
}

extern "C" fn window_inner_sunken_get(
    _engine: *mut std::ffi::c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
) -> std::ffi::c_int {
    // SAFETY: the C++ trampoline passes valid out slots.
    unsafe {
        (*out).ty = tjs2_sys::VAL_INTEGER;
        (*out).integer = 0;
    }
    0
}

extern "C" fn window_inner_sunken_set(
    _engine: *mut std::ffi::c_void,
    _value: *const tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
) -> std::ffi::c_int {
    0
}

extern "C" fn window_show_scroll_bars_get(
    _engine: *mut std::ffi::c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
) -> std::ffi::c_int {
    // SAFETY: the C++ trampoline passes valid out slots.
    unsafe {
        (*out).ty = tjs2_sys::VAL_INTEGER;
        (*out).integer = 0;
    }
    0
}

extern "C" fn window_show_scroll_bars_set(
    _engine: *mut std::ffi::c_void,
    _value: *const tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
) -> std::ffi::c_int {
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tjs2_sys::TjsValue;

    #[test]
    fn window_properties_exist_and_are_deletable() {
        let e = Tjs2Engine::new().unwrap();
        register_window(&e).unwrap();
        // Read both before delete.
        assert_eq!(
            e.eval("Window.innerSunken", "t").unwrap(),
            TjsValue::Integer(0)
        );
        assert_eq!(
            e.eval("Window.showScrollBars", "t").unwrap(),
            TjsValue::Integer(0)
        );
        // The startup.tjs idiom: delete must not error.
        e.exec_script(
            "delete Window.innerSunken; delete Window.showScrollBars;",
            "t",
        )
        .unwrap();
    }
}
