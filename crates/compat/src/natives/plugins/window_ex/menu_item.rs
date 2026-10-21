//! `MenuItem` members added by `windowEx.dll` (`struct MenuItemEx`).
//!
//! The reference attaches the `bi*` class constants, the
//! `rightJustify`/`bmpItem`/`bmpChecked`/`bmpUnchecked` properties and the
//! `popupEx` function to `MenuItem`. Our ABI has no per-object native payload
//! for an existing class, so the small flag set is kept per-object in
//! [`MenuItemExState`] keyed by the `objthis` pointer.
//!
//! `popupEx` is the reference's empty `TJS_S_OK` stub
//! (`windowEx.cpp:1185-1188`), so it accepts its arguments and returns void;
//! the bitmap properties report the reference's `BMT_BMP`/`BMT_SYS` values
//! (`-1`/`0`).
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{
    NativeInstanceMembers, NativeObjThisMethodDef, NativeObjThisPropertyDef, NativePropertyDef,
    NativeStaticMembers, Tjs2Engine, Value,
};

use crate::CompatCtx;
use crate::CompatError;
use crate::collections::Map;
use crate::native_util::{report_error, set_int, set_void};

use super::util::{as_bool, int_property_getter, is_object, resolve};

/// Per-binding `MenuItem` plugin state.
#[derive(Default)]
pub(crate) struct MenuItemExState {
    flags: Map<usize, super::util::Tokened<ItemFlags>>,
    /// Identity-token counter; see [`super::util::sync_object_state`].
    next_token: i64,
}

/// The per-item flags `MenuItemEx` caches. `bmp_*` is `true` when the
/// reference stored a bitmap object (`BMT_BMP`, read back as `-1`).
#[derive(Default, Clone, Copy)]
struct ItemFlags {
    right_justify: bool,
    bmp_item: bool,
    bmp_checked: bool,
    bmp_unchecked: bool,
}

/// Borrow the flags that belong to `objthis`, syncing the per-object
/// identity token first so a reused address starts from the constructor
/// defaults (see [`super::util::sync_object_state`]).
fn with_flags<T>(
    services: &crate::CompatServices,
    engine: &Tjs2Engine,
    objthis: *mut c_void,
    f: impl FnOnce(&mut ItemFlags) -> T,
) -> Result<T, String> {
    let key = objthis as usize;
    let mut state = services.state::<MenuItemExState>();
    let state = &mut *state;
    super::util::sync_object_state(&mut state.flags, &mut state.next_token, engine, objthis)?;
    let entry = state.flags.entry(key).or_default();
    Ok(f(&mut entry.value))
}

// ---------------------------------------------------------------------------
// popupEx
// ---------------------------------------------------------------------------

/// `MenuItem.popupEx(flags, x?, y?, hwnd?, rect?, menulist?)` — the reference
/// body is an empty `TJS_S_OK` stub (`reference/cpp/plugins/windowEx.cpp:1185-1188`).
/// Accept any arguments and return void.
extern "C" fn menu_popup_ex(
    engine: *mut c_void,
    _objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, "MenuItem.popupEx: compat context is not bound");
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// properties
// ---------------------------------------------------------------------------

extern "C" fn menu_right_justify_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "MenuItem.rightJustify: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let value = match with_flags(services, resolved.engine(), objthis, |flags| {
        flags.right_justify
    }) {
        Ok(value) => value,
        Err(error) => {
            return report_error(out_error, &format!("MenuItem.rightJustify: {error}"));
        }
    };
    set_int(out, i64::from(value));
    0
}

extern "C" fn menu_right_justify_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "MenuItem.rightJustify: compat context is not bound",
        );
    };
    let truthy = if value.is_null() {
        false
    } else {
        // `rj = !!v.AsInteger();` (`reference/cpp/plugins/windowEx.cpp:1084-1088`)
        // with the `AsInteger` coercion rules of
        // `reference/cpp/core/tjs2/tjsVariant.h:862-878`.
        // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
        match as_bool(unsafe { &*value }) {
            Ok(truthy) => truthy,
            Err(error) => {
                return report_error(out_error, &format!("MenuItem.rightJustify: {error}"));
            }
        }
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    if let Err(error) = with_flags(services, resolved.engine(), objthis, |flags| {
        flags.right_justify = truthy
    }) {
        return report_error(out_error, &format!("MenuItem.rightJustify: {error}"));
    }
    0
}

extern "C" fn menu_bmp_item_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    bmp_get(engine, objthis, out, out_error, "bmpItem", |flags| {
        flags.bmp_item
    })
}

extern "C" fn menu_bmp_item_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    bmp_set(
        engine,
        objthis,
        value,
        out_error,
        "bmpItem",
        |flags, is_bitmap| flags.bmp_item = is_bitmap,
    )
}

extern "C" fn menu_bmp_checked_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    bmp_get(engine, objthis, out, out_error, "bmpChecked", |flags| {
        flags.bmp_checked
    })
}

extern "C" fn menu_bmp_checked_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    bmp_set(
        engine,
        objthis,
        value,
        out_error,
        "bmpChecked",
        |flags, is_bitmap| flags.bmp_checked = is_bitmap,
    )
}

extern "C" fn menu_bmp_unchecked_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    bmp_get(engine, objthis, out, out_error, "bmpUnchecked", |flags| {
        flags.bmp_unchecked
    })
}

extern "C" fn menu_bmp_unchecked_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    bmp_set(
        engine,
        objthis,
        value,
        out_error,
        "bmpUnchecked",
        |flags, is_bitmap| flags.bmp_unchecked = is_bitmap,
    )
}

fn bmp_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    what: &str,
    select: impl FnOnce(&ItemFlags) -> bool,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            &format!("MenuItem.{what}: compat context is not bound"),
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let is_bitmap = match with_flags(services, resolved.engine(), objthis, |flags| select(flags)) {
        Ok(is_bitmap) => is_bitmap,
        Err(error) => {
            return report_error(out_error, &format!("MenuItem.{what}: {error}"));
        }
    };
    // `BMT_BMP` reads back as -1, every other state as 0.
    set_int(out, if is_bitmap { -1 } else { 0 });
    0
}

fn bmp_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    what: &str,
    select: impl FnOnce(&mut ItemFlags, bool),
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            &format!("MenuItem.{what}: compat context is not bound"),
        );
    };
    let is_bitmap = if value.is_null() {
        false
    } else {
        // SAFETY: the trampoline passes a valid value slot for the call.
        let value = unsafe { &*value };
        // `setBmpSelect` stores `BMT_BMP` for every object-typed value —
        // including `null` (`tvtObject` with a null pointer,
        // `reference/cpp/core/tjs2/tjsVariant.h:552-557`) — and `BMT_SYS`
        // for void/integer/string (which read back as 0); reals fall
        // through the switch and also read back as 0
        // (`reference/cpp/plugins/windowEx.cpp:1113-1127`).
        is_object(value) || value.ty == tjs::VAL_NULL
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    if let Err(error) = with_flags(services, resolved.engine(), objthis, |flags| {
        select(flags, is_bitmap)
    }) {
        return report_error(out_error, &format!("MenuItem.{what}: {error}"));
    }
    0
}

// ---------------------------------------------------------------------------
// static constants
// ---------------------------------------------------------------------------

int_property_getter!(bi_system_get, 1);
int_property_getter!(bi_restore_get, 2);
int_property_getter!(bi_minimize_get, 3);
int_property_getter!(bi_close_get, 5);
int_property_getter!(bi_close_disabled_get, 6);
int_property_getter!(bi_minimize_disabled_get, 7);
int_property_getter!(bi_popup_close_get, 8);
int_property_getter!(bi_popup_restore_get, 9);
int_property_getter!(bi_popup_maximize_get, 10);
int_property_getter!(bi_popup_minimize_get, 11);

/// Register the `MenuItem` members added by `windowEx.dll`.
pub(super) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance_members(&NativeInstanceMembers {
        class_name: "MenuItem",
        methods: vec![NativeObjThisMethodDef {
            name: "popupEx",
            f: menu_popup_ex,
        }],
        properties: vec![
            NativeObjThisPropertyDef {
                name: "rightJustify",
                get: Some(menu_right_justify_get),
                set: Some(menu_right_justify_set),
            },
            NativeObjThisPropertyDef {
                name: "bmpItem",
                get: Some(menu_bmp_item_get),
                set: Some(menu_bmp_item_set),
            },
            NativeObjThisPropertyDef {
                name: "bmpChecked",
                get: Some(menu_bmp_checked_get),
                set: Some(menu_bmp_checked_set),
            },
            NativeObjThisPropertyDef {
                name: "bmpUnchecked",
                get: Some(menu_bmp_unchecked_get),
                set: Some(menu_bmp_unchecked_set),
            },
        ],
    })?;

    ctx.register_static_members(&NativeStaticMembers {
        class_name: "MenuItem",
        methods: vec![],
        properties: vec![
            NativePropertyDef {
                name: "biSystem",
                get: Some(bi_system_get),
                set: None,
            },
            NativePropertyDef {
                name: "biRestore",
                get: Some(bi_restore_get),
                set: None,
            },
            NativePropertyDef {
                name: "biMinimize",
                get: Some(bi_minimize_get),
                set: None,
            },
            NativePropertyDef {
                name: "biClose",
                get: Some(bi_close_get),
                set: None,
            },
            NativePropertyDef {
                name: "biCloseDisabled",
                get: Some(bi_close_disabled_get),
                set: None,
            },
            NativePropertyDef {
                name: "biMinimizeDisabled",
                get: Some(bi_minimize_disabled_get),
                set: None,
            },
            NativePropertyDef {
                name: "biPopupClose",
                get: Some(bi_popup_close_get),
                set: None,
            },
            NativePropertyDef {
                name: "biPopupRestore",
                get: Some(bi_popup_restore_get),
                set: None,
            },
            NativePropertyDef {
                name: "biPopupMaximize",
                get: Some(bi_popup_maximize_get),
                set: None,
            },
            NativePropertyDef {
                name: "biPopupMinimize",
                get: Some(bi_popup_minimize_get),
                set: None,
            },
        ],
    })
}
