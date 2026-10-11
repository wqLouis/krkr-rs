//! `System` static members added by `windowEx.dll`.
//!
//! Reference `WindowEx.cpp` (`struct System`): `getDisplayMonitors`,
//! `getMonitorInfo`, `getCursorPos`, `setCursorPos`, `getSystemMetrics`,
//! `readEnvValue`, `expandEnvString`, `setApplicationIcon`,
//! `setIconicPreview`, `getDoubleClickTime`, `breathe`, `isBreathing`,
//! `getAboutString`, `getCPUType`. (`clearGraphicCache` already exists on the
//! core `System` class and is intentionally not re-registered.)
//!
//! The display-monitor, cursor, system-metric, environment and icon facts are
//! host facts: they go through the [`crate::host`](super::host) closure seam
//! and raise a typed error until the host installs one.
//!
//! Each monitor returned by `getDisplayMonitors`/`getMonitorInfo` is a
//! dictionary carrying `primary`, `monitor` and `work`, where `monitor` and
//! `work` are `{x, y, w, h}` sub-dictionaries. The vendored
//! `reference/cpp/plugins/windowEx.cpp:1424-1435` stubs both bodies, so the
//! shipped `k2compat/k2compat_deskinfo.tjs` and
//! `k2compat/k2compat_scrinfo.tjs` are the shape's oracle.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{
    DetachedValue, NativeMethodDef, NativeStaticMembers, RetainedValue, Tjs2Engine, TjsValue,
    VAL_STRING, Value,
};

use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::{CompatCtx, CompatError};

use super::host;
use super::util::{as_bool, resolve, set_retained_out, set_string_out};

/// The monitor rectangle tuple shared with the host seam.
type MonitorRect = (i32, i32, i32, i32, i32, i32, i32, i32, bool);

/// Build a TJS array of monitor dictionaries from the host tuples.
fn monitors_to_array(
    engine: &Tjs2Engine,
    monitors: &[MonitorRect],
) -> Result<DetachedValue, String> {
    let array = match engine
        .eval_retained("[]", "System.getDisplayMonitors")
        .map_err(|e| e.to_string())?
    {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(_) => {
            return Err("System.getDisplayMonitors: cannot build the result array".into());
        }
    };
    for (index, monitor) in monitors.iter().enumerate() {
        let dict = super::util::make_dict(engine, "System.getDisplayMonitors")?;
        write_monitor(engine, dict.raw_id(), *monitor)?;
        let dict_id = dict.raw_id();
        // The retention is consumed by `set_member` (see `metrics_lookup`).
        std::mem::forget(dict);
        engine.set_member(
            array.raw_id(),
            &index.to_string(),
            &TjsValue::Retained(dict_id as u64),
        )?;
    }
    Ok(array)
}

/// The `System.metrics` lookup the reference performs inside
/// `getSystemMetrics`.
///
/// The reference *ensures* the `System.metrics` dictionary exists on every
/// call (`reference/cpp/plugins/windowEx.cpp:1466-1476` — a `TJS_MEMBERENSURE`
/// write that scripts can observe afterwards), then reads the **uppercased**
/// key with `getIntValue(key, -1)` and treats a negative reading as
/// `TJS_E_INVALIDPARAM` (`windowEx.cpp:1478-1481`). A missing key reads as
/// `Ok(None)` so the caller can fall through to the host seam.
fn metrics_lookup(engine: &Tjs2Engine, key: &str, what: &str) -> Result<Option<i64>, String> {
    // Distinguish "absent" (create it, like `PropGet(MEMBERMUSTEXIST)`
    // failing at `windowEx.cpp:1468-1476`) from "present but not a
    // dictionary" (the reference leaves it alone and the key misses).
    let (present, existing) = match engine.eval_retained("System.metrics", what) {
        Ok(RetainedValue::Object(object)) => (true, Some(object)),
        Ok(RetainedValue::Value(_)) => (true, None),
        Err(_) => (false, None),
    };
    if !present {
        let system = match engine
            .eval_retained("System", what)
            .map_err(|error| error.to_string())?
        {
            RetainedValue::Object(object) => object,
            RetainedValue::Value(_) => return Err(format!("{what}: System is not an object")),
        };
        let new_dict = super::util::make_dict(engine, what)?;
        let dict_id = new_dict.raw_id();
        // The retention is consumed by `set_member`; forget it *before* the
        // write so an error path cannot release it twice.
        std::mem::forget(new_dict);
        engine.set_member(
            system.raw_id(),
            "metrics",
            &TjsValue::Retained(dict_id as u64),
        )?;
        return Ok(None);
    }
    let Some(dict) = existing else {
        return Ok(None);
    };
    let found = match engine.member(dict.raw_id(), key) {
        Ok(TjsValue::Integer(value)) => Some(value),
        Ok(TjsValue::Real(value)) => Some(value as i64),
        _ => None,
    };
    Ok(found.filter(|value| *value >= 0))
}

/// Write an `{x, y, w, h}` rectangle into a dictionary object.
fn write_rect(
    engine: &Tjs2Engine,
    dict: tjs::Tjs2ValueId,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) -> Result<(), String> {
    for (key, value) in [
        ("x", TjsValue::Integer(i64::from(x))),
        ("y", TjsValue::Integer(i64::from(y))),
        ("w", TjsValue::Integer(i64::from(w))),
        ("h", TjsValue::Integer(i64::from(h))),
    ] {
        engine.set_member(dict, key, &value)?;
    }
    Ok(())
}

/// Write one monitor tuple into a dictionary object as the nested shape the
/// shipped `k2compat/k2compat_deskinfo.tjs:22-25` and `:43-46` read: a
/// `primary` flag plus `monitor` and `work` sub-dictionaries, each
/// `{x, y, w, h}`. The work rectangle is derived from the L1 work bounds and
/// the monitor rectangle from the full bounds.
fn write_monitor(
    engine: &Tjs2Engine,
    dict: tjs::Tjs2ValueId,
    monitor: MonitorRect,
) -> Result<(), String> {
    let (left, top, right, bottom, wl, wt, wr, wb, primary) = monitor;
    let monitor_w = right
        .checked_sub(left)
        .ok_or("windowEx monitor rectangle: width overflows")?;
    let monitor_h = bottom
        .checked_sub(top)
        .ok_or("windowEx monitor rectangle: height overflows")?;
    let work_w = wr
        .checked_sub(wl)
        .ok_or("windowEx work rectangle: width overflows")?;
    let work_h = wb
        .checked_sub(wt)
        .ok_or("windowEx work rectangle: height overflows")?;
    for (key, x, y, w, h) in [
        ("monitor", left, top, monitor_w, monitor_h),
        ("work", wl, wt, work_w, work_h),
    ] {
        let sub = super::util::make_dict(engine, "windowEx monitor rectangle")?;
        write_rect(engine, sub.raw_id(), x, y, w, h)?;
        let id = sub.raw_id();
        // The write consumes the retention; forget the guard first so an
        // error path cannot release it twice.
        std::mem::forget(sub);
        engine.set_member(dict, key, &TjsValue::Retained(id as u64))?;
    }
    engine.set_member(dict, "primary", &TjsValue::Integer(i64::from(primary)))?;
    Ok(())
}

/// Build a dictionary holding one monitor, or return the typed error.
fn monitor_dict_out(
    resolved: &crate::binding::Resolved,
    out: *mut Value,
    out_error: *mut *mut c_char,
    monitor: Result<MonitorRect, String>,
    what: &str,
) -> c_int {
    let monitor = match monitor {
        Ok(monitor) => monitor,
        Err(error) => return report_error(out_error, &format!("{what}: {error}")),
    };
    let engine = resolved.engine();
    let dict = match super::util::make_dict(engine, what) {
        Ok(dict) => dict,
        Err(error) => return report_error(out_error, &error),
    };
    if let Err(error) = write_monitor(engine, dict.raw_id(), monitor) {
        return report_error(out_error, &format!("{what}: {error}"));
    }
    set_retained_out(out, dict.raw_id());
    std::mem::forget(dict);
    0
}

/// A `(x, y)` dictionary.
fn point_dict_out(
    resolved: &crate::binding::Resolved,
    out: *mut Value,
    out_error: *mut *mut c_char,
    point: (i32, i32),
    what: &str,
) -> c_int {
    let engine = resolved.engine();
    let dict = match super::util::make_dict(engine, what) {
        Ok(dict) => dict,
        Err(error) => return report_error(out_error, &error),
    };
    for (key, value) in [
        ("x", TjsValue::Integer(i64::from(point.0))),
        ("y", TjsValue::Integer(i64::from(point.1))),
    ] {
        if let Err(error) = engine.set_member(dict.raw_id(), key, &value) {
            return report_error(out_error, &format!("{what}: {error}"));
        }
    }
    set_retained_out(out, dict.raw_id());
    std::mem::forget(dict);
    0
}

// ---------------------------------------------------------------------------
// Callbacks (static native signature)
// ---------------------------------------------------------------------------

extern "C" fn sys_get_display_monitors(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.getDisplayMonitors: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let Some(result) = host::display_monitors(services) else {
        return report_error(
            out_error,
            "System.getDisplayMonitors: no host display-monitor provider is installed",
        );
    };
    let monitors = match result {
        Ok(monitors) => monitors,
        Err(error) => {
            return report_error(out_error, &format!("System.getDisplayMonitors: {error}"));
        }
    };
    match monitors_to_array(resolved.engine(), &monitors) {
        Ok(array) => {
            set_retained_out(out, array.raw_id());
            std::mem::forget(array);
            0
        }
        Err(error) => report_error(out_error, &error),
    }
}

extern "C" fn sys_get_monitor_info(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.getMonitorInfo: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let index = arguments
        .first()
        .map(|value| {
            if value.ty == tjs::VAL_INTEGER {
                value.integer
            } else {
                0
            }
        })
        .unwrap_or(0);
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let Some(result) = host::monitor_info(services, index) else {
        return report_error(
            out_error,
            "System.getMonitorInfo: no host monitor-info provider is installed",
        );
    };
    monitor_dict_out(&resolved, out, out_error, result, "System.getMonitorInfo")
}

extern "C" fn sys_get_cursor_pos(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.getCursorPos: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let Some(result) = host::cursor_pos(services) else {
        return report_error(
            out_error,
            "System.getCursorPos: no host cursor provider is installed",
        );
    };
    match result {
        Ok(point) => point_dict_out(&resolved, out, out_error, point, "System.getCursorPos"),
        Err(error) => report_error(out_error, &format!("System.getCursorPos: {error}")),
    }
}

extern "C" fn sys_set_cursor_pos(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 2 {
        return report_error(out_error, "System.setCursorPos requires 2 arguments");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.setCursorPos: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let x = arguments[0].integer as i32;
    let y = arguments[1].integer as i32;
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    if let Some(Err(error)) = host::set_cursor_pos(services, x, y) {
        return report_error(out_error, &format!("System.setCursorPos: {error}"));
    }
    set_void(out);
    0
}

extern "C" fn sys_get_system_metrics(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        // `if(n < 1) return TJS_E_BADPARAMCOUNT;`
        // (`reference/cpp/plugins/windowEx.cpp:1450-1451`).
        return report_error(out_error, "System.getSystemMetrics requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.getSystemMetrics: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments[0].ty != VAL_STRING {
        // `if(p[0]->Type() != tvtString) return TJS_E_INVALIDPARAM;`
        // (`windowEx.cpp:1454-1455`).
        return report_error(
            out_error,
            "System.getSystemMetrics: the metric name must be a string",
        );
    }
    let name = value_as_string(&arguments[0]);
    if name.is_empty() {
        // (`windowEx.cpp:1456-1457`).
        return report_error(out_error, "System.getSystemMetrics: empty metric name");
    }
    // `key.ToUpperCase();` (`windowEx.cpp:1458`).
    let name = name.to_ascii_uppercase();
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    // The reference path: ensure `System.metrics` and read the uppercased
    // key (`windowEx.cpp:1466-1481`).
    match metrics_lookup(resolved.engine(), &name, "System.getSystemMetrics") {
        Ok(Some(value)) => {
            set_int(out, value);
            return 0;
        }
        Ok(None) => {}
        Err(error) => return report_error(out_error, &format!("System.getSystemMetrics: {error}")),
    }
    // Port extension: the reference would raise `TJS_E_INVALIDPARAM` for a
    // missing key; ask the host seam first (errors when nothing is installed).
    let Some(result) = host::system_metrics(services, &name) else {
        return report_error(
            out_error,
            &format!(
                "System.getSystemMetrics: {name} is not in System.metrics \
                 and no host system-metrics provider is installed"
            ),
        );
    };
    match result {
        Ok(value) => {
            set_int(out, value);
            0
        }
        Err(error) => report_error(out_error, &format!("System.getSystemMetrics: {error}")),
    }
}

extern "C" fn sys_read_env_value(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        // `if(n < 1) return TJS_E_BADPARAMCOUNT;`
        // (`reference/cpp/plugins/windowEx.cpp:1487-1488`).
        return report_error(out_error, "System.readEnvValue requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.readEnvValue: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    if arguments[0].ty != VAL_STRING {
        // `if(p[0]->Type() != tvtString) return TJS_E_INVALIDPARAM;`
        // (`windowEx.cpp:1490-1491`).
        return report_error(
            out_error,
            "System.readEnvValue: the variable name must be a string",
        );
    }
    let name = value_as_string(&arguments[0]);
    if name.is_empty() {
        // (`windowEx.cpp:1492-1493`).
        return report_error(out_error, "System.readEnvValue: empty variable name");
    }
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let Some(result) = host::read_env(services, &name) else {
        return report_error(
            out_error,
            "System.readEnvValue: no host environment provider is installed",
        );
    };
    match result {
        Ok(Some(value)) => {
            set_string_out(out, services, &value);
            0
        }
        Ok(None) => {
            set_void(out);
            0
        }
        Err(error) => report_error(out_error, &format!("System.readEnvValue: {error}")),
    }
}

extern "C" fn sys_expand_env_string(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "System.expandEnvString requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.expandEnvString: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let value = value_as_string(&arguments[0]);
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let Some(result) = host::expand_env(services, &value) else {
        return report_error(
            out_error,
            "System.expandEnvString: no host environment provider is installed",
        );
    };
    match result {
        Ok(expanded) => {
            set_string_out(out, services, &expanded);
            0
        }
        Err(error) => report_error(out_error, &format!("System.expandEnvString: {error}")),
    }
}

extern "C" fn sys_set_application_icon(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    // The reference body performs no argument validation at all
    // (`reference/cpp/plugins/windowEx.cpp:1504-1507`), so any argument
    // count is accepted; without a path there is nothing to hand the host.
    if argc < 1 {
        set_void(out);
        return 0;
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.setApplicationIcon: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let path = value_as_string(&arguments[0]);
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    if let Some(Err(error)) = host::set_application_icon(services, &path) {
        return report_error(out_error, &format!("System.setApplicationIcon: {error}"));
    }
    set_void(out);
    0
}

extern "C" fn sys_set_iconic_preview(
    engine: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    if argc < 1 {
        // The reference signature is `setIconicPreview(bool en)`
        // (`windowEx.cpp:1509`), which ncbind enforces as a one-parameter
        // call (`reference/cpp/core/plugin/ncbind.hpp:1178`).
        return report_error(out_error, "System.setIconicPreview requires 1 argument");
    }
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.setIconicPreview: compat context is not bound",
        );
    };
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let arguments = unsafe { args(argv, argc) };
    let enabled = match as_bool(&arguments[0]) {
        Ok(enabled) => enabled,
        Err(error) => {
            return report_error(out_error, &format!("System.setIconicPreview: {error}"));
        }
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let enabled = match host::set_iconic_preview(services, enabled) {
        Some(Ok(enabled)) => enabled,
        Some(Err(error)) => {
            return report_error(out_error, &format!("System.setIconicPreview: {error}"));
        }
        // `static bool setIconicPreview(bool en) { return true; }`
        // (`windowEx.cpp:1509`) — true regardless of the flag.
        None => true,
    };
    set_bool(out, enabled);
    0
}

extern "C" fn sys_get_double_click_time(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.getDoubleClickTime: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    match host::double_click_time(services) {
        Some(Ok(value)) => {
            set_int(out, value);
            0
        }
        Some(Err(error)) => report_error(out_error, &format!("System.getDoubleClickTime: {error}")),
        // `static tjs_int getDoubleClickTime() { return -1; }`
        // (`reference/cpp/plugins/windowEx.cpp:1422`) — the reference never
        // consults a host, so -1 is the default reading.
        None => {
            set_int(out, -1);
            0
        }
    }
}

extern "C" fn sys_breathe(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.breathe: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    if let Some(Err(error)) = host::breathe(services) {
        return report_error(out_error, &format!("System.breathe: {error}"));
    }
    set_void(out);
    0
}

extern "C" fn sys_is_breathing(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.isBreathing: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    let breathing = match host::is_breathing(services) {
        Some(Ok(breathing)) => breathing,
        Some(Err(error)) => {
            return report_error(out_error, &format!("System.isBreathing: {error}"));
        }
        // No host breathing loop: the process is never "breathing".
        None => false,
    };
    set_bool(out, breathing);
    0
}

extern "C" fn sys_get_about_string(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "System.getAboutString: compat context is not bound",
        );
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    set_string_out(
        out,
        services,
        concat!("krkr-rs windowEx compat ", env!("CARGO_PKG_VERSION")),
    );
    0
}

extern "C" fn sys_get_cpu_type(
    engine: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "System.getCPUType: compat context is not bound");
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding.
    let services = unsafe { ctx.services() };
    match host::cpu_type(services) {
        Some(Ok(flags)) => {
            set_int(out, flags);
            0
        }
        Some(Err(error)) => report_error(out_error, &format!("System.getCPUType: {error}")),
        // The reference attaches `TVPGetCPUType`, a `tjs_uint32`
        // CPU-feature bitmask (`reference/cpp/plugins/windowEx.cpp:1527`,
        // `reference/cpp/core/environ/DetectCPU.cpp:170-175`), so the result
        // is an *integer*; L3 owns no platform code, so the port reads 0
        // ("no features detected") until the host installs the real flags.
        None => {
            set_int(out, 0);
            0
        }
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `System` static members added by `windowEx.dll`.
pub(super) fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_static_members(&NativeStaticMembers {
        class_name: "System",
        methods: vec![
            NativeMethodDef {
                name: "getDisplayMonitors",
                f: sys_get_display_monitors,
            },
            NativeMethodDef {
                name: "getMonitorInfo",
                f: sys_get_monitor_info,
            },
            NativeMethodDef {
                name: "getCursorPos",
                f: sys_get_cursor_pos,
            },
            NativeMethodDef {
                name: "setCursorPos",
                f: sys_set_cursor_pos,
            },
            NativeMethodDef {
                name: "getSystemMetrics",
                f: sys_get_system_metrics,
            },
            NativeMethodDef {
                name: "readEnvValue",
                f: sys_read_env_value,
            },
            NativeMethodDef {
                name: "expandEnvString",
                f: sys_expand_env_string,
            },
            NativeMethodDef {
                name: "setApplicationIcon",
                f: sys_set_application_icon,
            },
            NativeMethodDef {
                name: "setIconicPreview",
                f: sys_set_iconic_preview,
            },
            NativeMethodDef {
                name: "getDoubleClickTime",
                f: sys_get_double_click_time,
            },
            NativeMethodDef {
                name: "breathe",
                f: sys_breathe,
            },
            NativeMethodDef {
                name: "isBreathing",
                f: sys_is_breathing,
            },
            NativeMethodDef {
                name: "getAboutString",
                f: sys_get_about_string,
            },
            NativeMethodDef {
                name: "getCPUType",
                f: sys_get_cpu_type,
            },
        ],
        properties: vec![],
    })
}
