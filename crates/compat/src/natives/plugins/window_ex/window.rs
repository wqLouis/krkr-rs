//! `Window` members added by `windowEx.dll` (`struct WindowEx`).
//!
//! The reference attaches `WindowEx` to the existing `Window` class
//! (`NCB_ATTACH_CLASS_WITH_HOOK(WindowEx, Window)`); there is no separate
//! `WindowEx` global class. Our TJS ABI has no subclassing or per-object
//! native payload for an existing class, so the members are attached to
//! `Window` with
//! [`register_instance_members`](crate::CompatCtx::register_instance_members)
//! and the per-window flags live in [`WindowEx`] keyed by the `objthis`
//! pointer. The class constants (`ncht*`) are `TJS_STATICMEMBER`
//! properties.
//!
//! Host actions the reference leaves inert on this host (`minimize`,
//! `maximize`, `showRestore`, the window icon, `focusMenuByKey`, `bringTo`,
//! `sendToBack`) are accepted; host facts (the cursor and system metrics)
//! live on `System` and raise a typed error until installed.
//!
//! The `maximized` and `minimized` properties answer the window's zoomed and
//! iconic state. The shipped `windowEx.dll` (sha256[:12] `124d2d426204`)
//! reads that state from the native window handle (`IsZoomed` and `IsIconic`
//! in `USER32.dll`); this host has no zoomed or iconic window, so both
//! properties answer `false` and their setters are inert.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{
    DetachedValue, NativeInstanceMembers, NativeObjThisMethodDef, NativeObjThisPropertyDef,
    NativePropertyDef, NativeStaticMembers, RetainedValue, Tjs2Engine, TjsValue, Value,
};

use crate::collections::Map;
use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::{CompatCtx, CompatError};

use super::notifications;
use super::util::{
    as_bool, as_integer, int_property_getter, is_object, objthis_int, resolve, set_retained_out,
};

/// The per-window flags `WindowEx` caches.
#[derive(Default)]
struct WindowFlags {
    disable_resize: bool,
    enable_nc_mouse_event: bool,
    message_hook_all: bool,
    sys_menu: Option<DetachedValue>,
    overlay: Option<DetachedValue>,
}

/// The shared `WindowEx` behaviour the `Window` members compose.
///
/// One flag set is held per script object, keyed by the raw receiver address
/// (see [`super::util::sync_object_state`]); the reference's per-object
/// operations are methods so the thin callbacks only marshal arguments and
/// results.
#[derive(Default)]
pub(crate) struct WindowEx {
    windows: Map<usize, super::util::Tokened<WindowFlags>>,
    /// Identity-token counter; see [`super::util::sync_object_state`].
    next_token: i64,
}

impl WindowEx {
    /// Borrow the flags that belong to `objthis`, syncing the per-object
    /// identity token first so a reused address starts from the constructor
    /// defaults (see [`super::util::sync_object_state`]).
    fn with_window<T>(
        &mut self,
        engine: &Tjs2Engine,
        objthis: *mut c_void,
        f: impl FnOnce(&mut WindowFlags) -> T,
    ) -> Result<T, String> {
        let key = objthis as usize;
        super::util::sync_object_state(&mut self.windows, &mut self.next_token, engine, objthis)?;
        let entry = self.windows.entry(key).or_default();
        Ok(f(&mut entry.value))
    }

    /// `setMessageHookAll`: assign the all-messages flag and return it.
    fn set_message_hook_all(
        &mut self,
        engine: &Tjs2Engine,
        objthis: *mut c_void,
        on: bool,
    ) -> Result<bool, String> {
        self.with_window(engine, objthis, |flags| {
            flags.message_hook_all = on;
            flags.message_hook_all
        })
    }

    /// `setMessageHookOnel`: return the all-messages flag unchanged.
    fn message_hook_all(
        &mut self,
        engine: &Tjs2Engine,
        objthis: *mut c_void,
    ) -> Result<bool, String> {
        self.with_window(engine, objthis, |flags| flags.message_hook_all)
    }

    /// The reference `TJS_S_OK` window actions (`minimize`, `maximize`,
    /// `showRestore`, `resetWindowIcon`, `setWindowIcon`, `bringTo`,
    /// `sendToBack`): accepted and inert.
    fn inert() {}

    /// `checkExEvents`: probe the extended handler names on the script object.
    /// The results are not stored because the port has no native window
    /// message dispatcher.
    fn check_ex_events(engine: &Tjs2Engine, objthis: *mut c_void) {
        for name in ["onResizing", "onMoving", "onMove", "onNcMouseMove"] {
            Self::has_member(engine, objthis, name);
        }
    }

    /// Whether `objthis` defines (or inherits) `name` — the reference's
    /// `WindowEx::hasMember`.
    fn has_member(engine: &Tjs2Engine, objthis: *mut c_void, name: &str) -> bool {
        let Ok(object) = super::util::retain_objthis(engine, objthis) else {
            return false;
        };
        engine.member(object.raw_id(), name).is_ok()
    }
}

/// Emit a stored object (or TJS `null`) without holding the state borrow.
fn emit_stored(
    engine: &Tjs2Engine,
    out: *mut Value,
    stored: Option<tjs::Tjs2ValueId>,
    what: &str,
) -> c_int {
    let Some(raw) = stored else {
        return set_null(engine, out);
    };
    match engine.duplicate_retained(raw) {
        Ok(duplicate) => {
            set_retained_out(out, duplicate.raw_id());
            std::mem::forget(duplicate);
            0
        }
        Err(error) => {
            log::warn!("{what}: could not duplicate the stored object: {error}");
            set_null(engine, out)
        }
    }
}

/// Write TJS `null` into `out`.
fn set_null(engine: &Tjs2Engine, out: *mut Value) -> c_int {
    if out.is_null() {
        return 0;
    }
    if let Ok(RetainedValue::Object(object)) = engine.eval_retained("null", "windowEx.null") {
        set_retained_out(out, object.raw_id());
        std::mem::forget(object);
        return 0;
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// window rect helpers
// ---------------------------------------------------------------------------

fn window_rect(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    what: &str,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, &format!("{what}: compat context is not bound"));
    };
    let host = resolved.engine();
    let left = match objthis_int(host, objthis, "left") {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &format!("{what}: {error}")),
    };
    let top = match objthis_int(host, objthis, "top") {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &format!("{what}: {error}")),
    };
    let width = match objthis_int(host, objthis, "width") {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &format!("{what}: {error}")),
    };
    let height = match objthis_int(host, objthis, "height") {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &format!("{what}: {error}")),
    };
    let dict = match super::util::make_dict(host, what) {
        Ok(dict) => dict,
        Err(error) => return report_error(out_error, &error),
    };
    for (key, value) in [
        ("x", TjsValue::Integer(left)),
        ("y", TjsValue::Integer(top)),
        ("w", TjsValue::Integer(width)),
        ("h", TjsValue::Integer(height)),
    ] {
        if let Err(error) = host.set_member(dict.raw_id(), key, &value) {
            return report_error(out_error, &format!("{what}: {error}"));
        }
    }
    set_retained_out(out, dict.raw_id());
    std::mem::forget(dict);
    0
}

extern "C" fn window_get_window_rect(
    engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    window_rect(engine, objthis, out, out_error, "Window.getWindowRect")
}

extern "C" fn window_get_client_rect(
    engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    window_rect(engine, objthis, out, out_error, "Window.getClientRect")
}

extern "C" fn window_get_normal_rect(
    engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    window_rect(engine, objthis, out, out_error, "Window.getNormalRect")
}

extern "C" fn window_focus_menu_by_key(
    engine: *mut c_void,
    _objthis: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(
            out_error,
            "Window.focusMenuByKey: compat context is not bound",
        );
    }
    // The reference unconditionally reads its first parameter
    // (`reference/cpp/plugins/windowEx.cpp:379-383` does `(*p)->AsInteger()`),
    // so a missing argument is rejected instead of reading a void slot.
    if argc < 1 {
        return report_error(out_error, "Window.focusMenuByKey requires 1 argument");
    }
    set_void(out);
    0
}

extern "C" fn window_nc_hit_test(
    engine: *mut c_void,
    _objthis: *mut c_void,
    argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, "Window.ncHitTest: compat context is not bound");
    }
    // `if(n < 2) return TJS_E_BADPARAMCOUNT;` — `windowEx.cpp:586-594`.
    if argc < 2 {
        return report_error(out_error, "Window.ncHitTest requires 2 arguments");
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// inert window actions
// ---------------------------------------------------------------------------

/// `minimize`/`maximize`/`showRestore`/`resetWindowIcon`/`setWindowIcon`/
/// `bringTo`/`sendToBack` — accepted and inert, matching the reference
/// (`reference/cpp/plugins/windowEx.cpp:364-397,623-650` never reaches the
/// Win32 window off Windows) and taking any number of arguments (those
/// bodies never read `n`/`p`).
extern "C" fn window_inert(
    engine: *mut c_void,
    _objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, "windowEx: compat context is not bound");
    }
    WindowEx::inert();
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// maximized / minimized
// ---------------------------------------------------------------------------

/// `Window.maximized` — whether the window is in the zoomed state.
///
/// The shipped `windowEx.dll` (sha256[:12] `124d2d426204`) answers the
/// getter with `IsZoomed` on the native window handle (the `USER32.dll`
/// import at `0x10015049`), so a window that is neither zoomed nor iconic
/// answers `false`. This host has no zoomed window state, so every window
/// answers `false`.
extern "C" fn window_maximized_get(
    engine: *mut c_void,
    _objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, "Window.maximized: compat context is not bound");
    }
    set_bool(out, false);
    0
}

/// `Window.minimized` — whether the window is in the iconic state.
///
/// The shipped `windowEx.dll` (sha256[:12] `124d2d426204`) answers the
/// getter with `IsIconic` on the native window handle (the `USER32.dll`
/// import at `0x100150f9`), so a window that is neither zoomed nor iconic
/// answers `false`. This host has no iconic window state, so every window
/// answers `false`.
extern "C" fn window_minimized_get(
    engine: *mut c_void,
    _objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, "Window.minimized: compat context is not bound");
    }
    set_bool(out, false);
    0
}

/// `Window.maximized = value` — accepted and inert.
///
/// The shipped `windowEx.dll` does not change the window state from the
/// property: its only `ShowWindow` calls pass `SW_HIDE` and
/// `SW_SHOWNORMAL`, never `SW_MAXIMIZE`.
extern "C" fn window_maximized_set(
    engine: *mut c_void,
    _objthis: *mut c_void,
    _value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, "Window.maximized: compat context is not bound");
    }
    0
}

/// `Window.minimized = value` — accepted and inert.
///
/// The shipped `windowEx.dll` does not change the window state from the
/// property: its only `ShowWindow` calls pass `SW_HIDE` and
/// `SW_SHOWNORMAL`, never `SW_MINIMIZE`.
extern "C" fn window_minimized_set(
    engine: *mut c_void,
    _objthis: *mut c_void,
    _value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, "Window.minimized: compat context is not bound");
    }
    0
}

// ---------------------------------------------------------------------------
// per-window flags
// ---------------------------------------------------------------------------

extern "C" fn window_disable_resize_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    window_bool_get(
        engine,
        objthis,
        out,
        out_error,
        "Window.disableResize",
        |flags| flags.disable_resize,
    )
}

extern "C" fn window_disable_resize_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    window_bool_set(
        engine,
        objthis,
        value,
        out_error,
        "Window.disableResize",
        |flags, enabled| flags.disable_resize = enabled,
    )
}

extern "C" fn window_enable_ncm_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    window_bool_get(
        engine,
        objthis,
        out,
        out_error,
        "Window.enableNCMouseEvent",
        |flags| flags.enable_nc_mouse_event,
    )
}

extern "C" fn window_enable_ncm_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    window_bool_set(
        engine,
        objthis,
        value,
        out_error,
        "Window.enableNCMouseEvent",
        |flags, enabled| flags.enable_nc_mouse_event = enabled,
    )
}

fn window_bool_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    what: &str,
    select: impl FnOnce(&WindowFlags) -> bool,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, &format!("{what}: compat context is not bound"));
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<WindowEx>();
    let value = match state.with_window(resolved.engine(), objthis, |flags| select(flags)) {
        Ok(value) => value,
        Err(error) => return report_error(out_error, &format!("{what}: {error}")),
    };
    set_bool(out, value);
    0
}

fn window_bool_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    what: &str,
    select: impl FnOnce(&mut WindowFlags, bool),
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, &format!("{what}: compat context is not bound"));
    };
    let enabled = if value.is_null() {
        false
    } else {
        // SAFETY: the trampoline passes a valid value slot for the call.
        match as_bool(unsafe { &*value }) {
            Ok(enabled) => enabled,
            Err(error) => return report_error(out_error, &format!("{what}: {error}")),
        }
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<WindowEx>();
    if let Err(error) =
        state.with_window(resolved.engine(), objthis, |flags| select(flags, enabled))
    {
        return report_error(out_error, &format!("{what}: {error}"));
    }
    0
}

// ---------------------------------------------------------------------------
// exSystemMenu / overlay bitmap
// ---------------------------------------------------------------------------

extern "C" fn window_ex_system_menu_get(
    engine: *mut c_void,
    objthis: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Window.exSystemMenu: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let stored = {
        let mut state = services.state::<WindowEx>();
        match state.with_window(resolved.engine(), objthis, |flags| {
            flags.sys_menu.as_ref().map(|value| value.raw_id())
        }) {
            Ok(stored) => stored,
            Err(error) => {
                return report_error(out_error, &format!("Window.exSystemMenu: {error}"));
            }
        }
    };
    emit_stored(resolved.engine(), out, stored, "Window.exSystemMenu")
}

extern "C" fn window_ex_system_menu_set(
    engine: *mut c_void,
    objthis: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Window.exSystemMenu: compat context is not bound",
        );
    };
    let stored = if value.is_null() {
        None
    } else {
        // SAFETY: the trampoline passes a valid value slot for the call.
        let value = unsafe { &*value };
        match value.ty {
            // `null` is an object-typed variant with no pointer
            // (`reference/cpp/core/tjs2/tjsVariant.h:552-557`), so the
            // reference's `p[0]->AsObject()` accepts it and stores null
            // (`windowEx.cpp:550-556`).
            tjs::VAL_NULL => None,
            tjs::VAL_OBJECT | tjs::VAL_RETAINED => {
                match resolved.engine().retain_object_arg(value) {
                    Ok(object) => Some(object),
                    Err(error) => {
                        return report_error(out_error, &format!("Window.exSystemMenu: {error}"));
                    }
                }
            }
            // Anything else raises the variant conversion error in
            // `tTJSVariant::AsObject()` (`tjsVariant.h:682-691`).
            _ => {
                return report_error(
                    out_error,
                    "Window.exSystemMenu: the value must be an object or null",
                );
            }
        }
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<WindowEx>();
    if let Err(error) =
        state.with_window(resolved.engine(), objthis, |flags| flags.sys_menu = stored)
    {
        return report_error(out_error, &format!("Window.exSystemMenu: {error}"));
    }
    0
}

extern "C" fn window_set_overlay_bitmap(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Window.setOverlayBitmap: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(value) = arguments.first() else {
        // `if(n > 0 && p[0]->Type() == tvtObject)` — with no argument the
        // reference returns `TJS_S_OK` unchanged
        // (`reference/cpp/plugins/windowEx.cpp:531-541`).
        set_void(out);
        return 0;
    };
    if !is_object(value) {
        if value.ty == tjs::VAL_NULL {
            // `null` *is* `tvtObject` (`reference/cpp/core/tjs2/tjsVariant.h:552-557`),
            // so the reference passes it to `OverlayBitmap::setBitmap`,
            // which rejects the null layer and fails (`windowEx.cpp:531-540`).
            return report_error(
                out_error,
                "Window.setOverlayBitmap: the argument must be a Layer",
            );
        }
        // Non-object arguments (integer/real/string/void) never enter the
        // reference's `tvtObject` branch and return `TJS_S_OK` unchanged.
        set_void(out);
        return 0;
    }
    if !super::util::arg_has_class(resolved.engine(), value, "Layer") {
        return report_error(
            out_error,
            "Window.setOverlayBitmap: the argument must be a Layer",
        );
    }
    let overlay = match resolved.engine().retain_object_arg(value) {
        Ok(object) => object,
        Err(error) => return report_error(out_error, &format!("Window.setOverlayBitmap: {error}")),
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let mut state = services.state::<WindowEx>();
    if let Err(error) = state.with_window(resolved.engine(), objthis, |flags| {
        flags.overlay = Some(overlay)
    }) {
        return report_error(out_error, &format!("Window.setOverlayBitmap: {error}"));
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// message hooks / notifications
// ---------------------------------------------------------------------------

extern "C" fn window_set_message_hook(
    engine: *mut c_void,
    objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Window.setMessageHook: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    // `bool on = (n >= 1) && !!p[0]->AsInteger();`
    // (`reference/cpp/plugins/windowEx.cpp:601`).
    let on = match arguments.first() {
        None => false,
        Some(value) => match as_bool(value) {
            Ok(on) => on,
            Err(error) => {
                return report_error(out_error, &format!("Window.setMessageHook: {error}"));
            }
        },
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let enabled = if let Some(second) = arguments.get(1) {
        // A string names a notification; anything else converts like
        // `(tjs_int)*p[1]` (`windowEx.cpp:602-609`).
        let number = if second.ty == tjs::VAL_STRING {
            // `getIntValue(key, -1)` on `_Notifications`
            // (`reference/cpp/core/plugin/ncbind.hpp:689-695`, called at
            // `windowEx.cpp:607`): a name that is not in the table reads -1,
            // which the range check below rejects exactly like an
            // out-of-range number.
            notifications::number_for(&value_as_string(second)).unwrap_or(-1)
        } else {
            match as_integer(second) {
                Ok(number) => number,
                Err(error) => {
                    return report_error(out_error, &format!("Window.setMessageHook: {error}"));
                }
            }
        };
        // `if(num < 0 || num >= 0x400) return TJS_E_FAIL;`
        // (`windowEx.cpp:611-612`).
        if !(0..0x400).contains(&number) {
            return report_error(
                out_error,
                "Window.setMessageHook: message number out of range",
            );
        }
        // The one-message form neither stores the bit nor changes the hook
        // state: `setMessageHookOnel` only returns the all-hook flag
        // (`windowEx.cpp:747`, called at `windowEx.cpp:613`).
        let mut state = services.state::<WindowEx>();
        match state.message_hook_all(resolved.engine(), objthis) {
            Ok(flag) => flag,
            Err(error) => {
                return report_error(out_error, &format!("Window.setMessageHook: {error}"));
            }
        }
    } else {
        // `setMessageHookAll(on)` assigns and returns the flag
        // (`windowEx.cpp:749`, called at `windowEx.cpp:615`).
        let mut state = services.state::<WindowEx>();
        match state.set_message_hook_all(resolved.engine(), objthis, on) {
            Ok(flag) => flag,
            Err(error) => {
                return report_error(out_error, &format!("Window.setMessageHook: {error}"));
            }
        }
    };
    set_bool(out, enabled);
    0
}

extern "C" fn window_get_notification_num(
    engine: *mut c_void,
    _objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(
            out_error,
            "Window.getNotificationNum: compat context is not bound",
        );
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let Some(name) = arguments.first() else {
        return report_error(out_error, "Window.getNotificationNum requires 1 argument");
    };
    let number = notifications::number_for(&value_as_string(name)).unwrap_or(-1);
    set_int(out, number);
    0
}

extern "C" fn window_get_notification_name(
    engine: *mut c_void,
    _objthis: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Window.getNotificationName: compat context is not bound",
        );
    };
    let number = {
        // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
        let arguments = unsafe { args(argv, argc) };
        let Some(number) = arguments.first() else {
            return report_error(out_error, "Window.getNotificationName requires 1 argument");
        };
        // The reference converts through `tjs_int` (`tjsVariant.h:933-935` →
        // `AsInteger` at `tjsVariant.h:862-878`): strings parse a leading
        // number, `void` reads as 0, objects raise.
        match as_integer(number) {
            Ok(number) => number,
            Err(error) => {
                return report_error(out_error, &format!("Window.getNotificationName: {error}"));
            }
        }
    };
    let name = notifications::name_for(number).unwrap_or("");
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    super::util::set_string_out(out, services, name);
    0
}

// ---------------------------------------------------------------------------
// registerExEvent
// ---------------------------------------------------------------------------

extern "C" fn window_register_ex_event(
    engine: *mut c_void,
    objthis: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "Window.registerExEvent: compat context is not bound",
        );
    };
    // The reference's `checkExEvents` probes the script object for the four
    // extended handlers; the handler names are the `EXEV_*` strings and none
    // is registered as a class member. The port has no native window message
    // dispatcher, so the probe results are not retained.
    WindowEx::check_ex_events(resolved.engine(), objthis);
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// static class constants (ncht*)
// ---------------------------------------------------------------------------

int_property_getter!(ncht_error_get, 65534);
int_property_getter!(ncht_transparent_get, 65535);
int_property_getter!(ncht_nowhere_get, 0);
int_property_getter!(ncht_client_get, 1);
int_property_getter!(ncht_caption_get, 2);
int_property_getter!(ncht_sysmenu_get, 3);
int_property_getter!(ncht_size_get, 4);
int_property_getter!(ncht_growbox_get, 4);
int_property_getter!(ncht_menu_get, 5);
int_property_getter!(ncht_hscroll_get, 6);
int_property_getter!(ncht_vscroll_get, 7);
int_property_getter!(ncht_minbutton_get, 8);
int_property_getter!(ncht_reduce_get, 8);
int_property_getter!(ncht_maxbutton_get, 9);
int_property_getter!(ncht_zoom_get, 9);
int_property_getter!(ncht_left_get, 10);
int_property_getter!(ncht_right_get, 11);
int_property_getter!(ncht_top_get, 12);
int_property_getter!(ncht_topleft_get, 13);
int_property_getter!(ncht_topright_get, 14);
int_property_getter!(ncht_bottom_get, 15);
int_property_getter!(ncht_bottomleft_get, 16);
int_property_getter!(ncht_bottomright_get, 17);
int_property_getter!(ncht_border_get, 18);

// ---------------------------------------------------------------------------
// _Notifications
// ---------------------------------------------------------------------------

/// Install `Window._Notifications`, the two-way bare-name/value dictionary
/// (`#key` from `windowEx.cpp:801`, e.g. `"PAINT" => 15`).
pub(super) fn install_notifications(engine: &Tjs2Engine) -> Result<(), CompatError> {
    let literal = notifications::dictionary_literal();
    let dict = match engine
        .eval_retained(&literal, "windowEx._Notifications")
        .map_err(|error| CompatError::Register(error.to_string()))?
    {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(_) => {
            return Err(CompatError::Register(
                "windowEx._Notifications: dictionary literal returned a scalar".into(),
            ));
        }
    };
    let class = match engine
        .eval_retained("Window", "windowEx._Notifications")
        .map_err(|error| CompatError::Register(error.to_string()))?
    {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(_) => {
            return Err(CompatError::Register(
                "windowEx._Notifications: Window is not an object".into(),
            ));
        }
    };
    let dict_id = dict.raw_id();
    // The retention is consumed by `set_member`; forget it *before* the
    // write so an error path cannot release it twice.
    std::mem::forget(dict);
    engine
        .set_member(
            class.raw_id(),
            "_Notifications",
            &TjsValue::Retained(dict_id.as_ptr() as u64),
        )
        .map_err(CompatError::Register)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `Window` instance members and the `Window` `ncht*` class
/// constants added by `windowEx.dll`.
pub(super) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    let methods = vec![
        NativeObjThisMethodDef {
            name: "minimize",
            f: window_inert,
        },
        NativeObjThisMethodDef {
            name: "maximize",
            f: window_inert,
        },
        NativeObjThisMethodDef {
            name: "showRestore",
            f: window_inert,
        },
        NativeObjThisMethodDef {
            name: "focusMenuByKey",
            f: window_focus_menu_by_key,
        },
        NativeObjThisMethodDef {
            name: "resetWindowIcon",
            f: window_inert,
        },
        NativeObjThisMethodDef {
            name: "setWindowIcon",
            f: window_inert,
        },
        NativeObjThisMethodDef {
            name: "getWindowRect",
            f: window_get_window_rect,
        },
        NativeObjThisMethodDef {
            name: "getClientRect",
            f: window_get_client_rect,
        },
        NativeObjThisMethodDef {
            name: "getNormalRect",
            f: window_get_normal_rect,
        },
        NativeObjThisMethodDef {
            name: "setOverlayBitmap",
            f: window_set_overlay_bitmap,
        },
        NativeObjThisMethodDef {
            name: "ncHitTest",
            f: window_nc_hit_test,
        },
        NativeObjThisMethodDef {
            name: "setMessageHook",
            f: window_set_message_hook,
        },
        NativeObjThisMethodDef {
            name: "bringTo",
            f: window_inert,
        },
        NativeObjThisMethodDef {
            name: "sendToBack",
            f: window_inert,
        },
        NativeObjThisMethodDef {
            name: "registerExEvent",
            f: window_register_ex_event,
        },
        NativeObjThisMethodDef {
            name: "getNotificationNum",
            f: window_get_notification_num,
        },
        NativeObjThisMethodDef {
            name: "getNotificationName",
            f: window_get_notification_name,
        },
    ];

    ctx.register_instance_members(&NativeInstanceMembers {
        class_name: "Window",
        methods,
        properties: vec![
            NativeObjThisPropertyDef {
                name: "disableResize",
                get: Some(window_disable_resize_get),
                set: Some(window_disable_resize_set),
            },
            NativeObjThisPropertyDef {
                name: "enableNCMouseEvent",
                get: Some(window_enable_ncm_get),
                set: Some(window_enable_ncm_set),
            },
            NativeObjThisPropertyDef {
                name: "exSystemMenu",
                get: Some(window_ex_system_menu_get),
                set: Some(window_ex_system_menu_set),
            },
            NativeObjThisPropertyDef {
                name: "maximized",
                get: Some(window_maximized_get),
                set: Some(window_maximized_set),
            },
            NativeObjThisPropertyDef {
                name: "minimized",
                get: Some(window_minimized_get),
                set: Some(window_minimized_set),
            },
        ],
    })?;

    ctx.register_static_members(&NativeStaticMembers {
        class_name: "Window",
        methods: vec![],
        properties: vec![
            static_int("nchtError", ncht_error_get),
            static_int("nchtTransparent", ncht_transparent_get),
            static_int("nchtNoWhere", ncht_nowhere_get),
            static_int("nchtClient", ncht_client_get),
            static_int("nchtCaption", ncht_caption_get),
            static_int("nchtSysMenu", ncht_sysmenu_get),
            static_int("nchtSize", ncht_size_get),
            static_int("nchtGrowBox", ncht_growbox_get),
            static_int("nchtMenu", ncht_menu_get),
            static_int("nchtHScroll", ncht_hscroll_get),
            static_int("nchtVScroll", ncht_vscroll_get),
            static_int("nchtMinButton", ncht_minbutton_get),
            static_int("nchtReduce", ncht_reduce_get),
            static_int("nchtMaxButton", ncht_maxbutton_get),
            static_int("nchtZoom", ncht_zoom_get),
            static_int("nchtLeft", ncht_left_get),
            static_int("nchtRight", ncht_right_get),
            static_int("nchtTop", ncht_top_get),
            static_int("nchtTopLeft", ncht_topleft_get),
            static_int("nchtTopRight", ncht_topright_get),
            static_int("nchtBottom", ncht_bottom_get),
            static_int("nchtBottomLeft", ncht_bottomleft_get),
            static_int("nchtBottomRight", ncht_bottomright_get),
            static_int("nchtBorder", ncht_border_get),
        ],
    })?;

    // SAFETY: `ctx` is only created for a bound, live engine, so its VM pointer is valid for `register`.
    install_notifications(unsafe { ctx.engine() })?;
    Ok(())
}

/// Build one read-only static integer property from a getter.
fn static_int(name: &'static str, get: tjs::NativePropertyGetFn) -> NativePropertyDef {
    NativePropertyDef {
        name,
        get: Some(get),
        set: None,
    }
}
