//! `Mouse` native class — port of the reference `Mouse` class (in the full
//! krkrz tree: `core/base/MouseIntf.cpp`, `TVPCreateNativeClass_Mouse`;
//! not part of this repository's reference subset).
//!
//! A static class: scripts call `Mouse.method(...)`; the class carries no
//! instance state. Every method reads (or writes, for `setCursorPos` /
//! `setVisible`) the shared [`crate::InputState`] the app feeds every
//! frame — see the crate docs for the frame protocol and the documented
//! deviations (notably `getCursorPos` returning a `"x,y"` string and
//! out-of-range buttons returning false instead of raising).

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{NativeClassBuilder, NativeMethodDef, Tjs2Engine, Value};

use crate::{
    MOUSE_BUTTONS, args, report_error, set_int_out, set_string_out, set_void_out, value_as_bool,
    value_as_i64, with_state,
};

/// `Mouse.getCursorPos()` → `"x,y"` string.
///
/// The reference takes an object argument and fills its `x`/`y` properties;
/// the C ABI cannot marshal TJS objects, so this port returns the position
/// as a `"x,y"` string instead (documented deviation).
extern "C" fn native_get_cursor_pos(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let (x, y) = with_state(|s| (s.mouse.x, s.mouse.y));
    set_string_out(out, &format!("{x},{y}"));
    0
}

/// `Mouse.getCursorX()` → int.
extern "C" fn native_get_cursor_x(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let x = with_state(|s| s.mouse.x);
    set_int_out(out, i64::from(x));
    0
}

/// `Mouse.getCursorY()` → int.
extern "C" fn native_get_cursor_y(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let y = with_state(|s| s.mouse.y);
    set_int_out(out, i64::from(y));
    0
}

/// `Mouse.setCursorPos(x, y)` → void.
///
/// Writes the cursor position back into the shared state (the app can read
/// it back, e.g. to re-apply it to the OS cursor). The reference requires
/// two arguments (`TJS_E_BADPARAMCOUNT` otherwise).
extern "C" fn native_set_cursor_pos(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "Mouse.setCursorPos requires 2 arguments");
    }
    let a = args(argv, argc);
    let x = value_as_i64(&a[0]) as i32;
    let y = value_as_i64(&a[1]) as i32;
    with_state(|s| s.set_mouse_pos(x, y));
    set_void_out(out);
    0
}

/// `Mouse.getWheelRot()` → int — the accumulated vertical wheel delta since
/// the last `begin_frame` (alias of `getWheelRotY`, the classic wheel).
extern "C" fn native_get_wheel_rot(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let y = with_state(|s| s.mouse.wheel.1);
    set_int_out(out, i64::from(y));
    0
}

/// `Mouse.getWheelRotX()` → int — horizontal wheel delta this frame.
extern "C" fn native_get_wheel_rot_x(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let x = with_state(|s| s.mouse.wheel.0);
    set_int_out(out, i64::from(x));
    0
}

/// `Mouse.getWheelRotY()` → int — vertical wheel delta this frame.
extern "C" fn native_get_wheel_rot_y(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let y = with_state(|s| s.mouse.wheel.1);
    set_int_out(out, i64::from(y));
    0
}

/// `Mouse.getWheelRotZ()` → int — wheel tilt delta this frame.
extern "C" fn native_get_wheel_rot_z(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let z = with_state(|s| s.mouse.wheel.2);
    set_int_out(out, i64::from(z));
    0
}

/// `Mouse.isVisible()` → bool — the cursor visibility flag.
extern "C" fn native_is_visible(
    _engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
) -> c_int {
    let visible = with_state(|s| s.mouse.visible);
    set_int_out(out, i64::from(visible));
    0
}

/// `Mouse.setVisible(visible)` → void — toggles the cursor visibility flag
/// in the shared state (the app applies it to the OS cursor).
extern "C" fn native_set_visible(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Mouse.setVisible requires 1 argument");
    }
    let visible = value_as_bool(&args(argv, argc)[0]);
    with_state(|s| s.set_mouse_visible(visible));
    set_void_out(out);
    0
}

/// `Mouse.getPressed(button)` → bool — whether the TVP mouse button
/// `button` ([`crate::MB_LEFT`]..[`crate::MB_X2`]) is currently held.
/// Out-of-range buttons return 0 (the reference raises
/// `TJS_E_INVALIDPARAM` — documented deviation).
extern "C" fn native_get_pressed(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Mouse.getPressed requires 1 argument");
    }
    let a = args(argv, argc);
    let pressed = with_state(|s| button_index(&a[0]).is_some_and(|b| s.mouse.buttons[b]));
    set_int_out(out, i64::from(pressed));
    0
}

/// `Mouse.getReleased(button)` → bool — whether the button was released
/// this frame.
extern "C" fn native_get_released(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Mouse.getReleased requires 1 argument");
    }
    let a = args(argv, argc);
    let released = with_state(|s| button_index(&a[0]).is_some_and(|b| s.mouse.released[b]));
    set_int_out(out, i64::from(released));
    0
}

/// `Mouse.getRepeat(button)` → int — consecutive frames the button has been
/// held (0 when not held).
extern "C" fn native_get_repeat(
    _engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Mouse.getRepeat requires 1 argument");
    }
    let a = args(argv, argc);
    let repeat = with_state(|s| button_index(&a[0]).map_or(0, |b| s.mouse.hold[b]));
    set_int_out(out, i64::from(repeat));
    0
}

/// `Mouse.getClickCount(button)` → int.
///
/// Stub returning 0 — click counting (double-click detection) is not
/// implemented yet; the arg-count check matches the reference.
extern "C" fn native_get_click_count(
    _engine: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Mouse.getClickCount requires 1 argument");
    }
    set_int_out(out, 0);
    0
}

/// Map a TJS argument to a TVP mouse-button index
/// ([`crate::MB_LEFT`]..[`crate::MB_X2`]); `None` for out-of-range values
/// (the reference raises `TJS_E_INVALIDPARAM` — this port returns
/// false/0 instead, see the crate docs).
fn button_index(v: &Value) -> Option<usize> {
    let b = value_as_i64(v);
    if (0..MOUSE_BUTTONS as i64).contains(&b) {
        Some(b as usize)
    } else {
        None
    }
}

/// Register the `Mouse` native class (static; no properties).
pub fn register_mouse(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class(&NativeClassBuilder {
        name: "Mouse",
        methods: vec![
            NativeMethodDef {
                name: "getCursorPos",
                f: native_get_cursor_pos,
            },
            NativeMethodDef {
                name: "getCursorX",
                f: native_get_cursor_x,
            },
            NativeMethodDef {
                name: "getCursorY",
                f: native_get_cursor_y,
            },
            NativeMethodDef {
                name: "setCursorPos",
                f: native_set_cursor_pos,
            },
            NativeMethodDef {
                name: "getWheelRot",
                f: native_get_wheel_rot,
            },
            NativeMethodDef {
                name: "getWheelRotX",
                f: native_get_wheel_rot_x,
            },
            NativeMethodDef {
                name: "getWheelRotY",
                f: native_get_wheel_rot_y,
            },
            NativeMethodDef {
                name: "getWheelRotZ",
                f: native_get_wheel_rot_z,
            },
            NativeMethodDef {
                name: "isVisible",
                f: native_is_visible,
            },
            NativeMethodDef {
                name: "setVisible",
                f: native_set_visible,
            },
            NativeMethodDef {
                name: "getPressed",
                f: native_get_pressed,
            },
            NativeMethodDef {
                name: "getReleased",
                f: native_get_released,
            },
            NativeMethodDef {
                name: "getRepeat",
                f: native_get_repeat,
            },
            NativeMethodDef {
                name: "getClickCount",
                f: native_get_click_count,
            },
        ],
        properties: Vec::new(),
    })
}
