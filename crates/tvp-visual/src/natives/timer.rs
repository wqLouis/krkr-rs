//! `Timer` native class — ported from
//! `reference/cpp/core/utils/TimerIntf.cpp` / `utils/impl/TimerImpl.cpp`.
//!
//! Script surface: `new Timer(callback[, actionName])`, properties
//! `interval` (ms, rw), `enabled` (rw). Firing happens through
//! [`timer_poll`], which the app's update loop calls each frame with a
//! monotonic millisecond clock; due timers invoke their retained callback
//! via [`tjs2_sys::Tjs2Engine::call_detached`].

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine, TjsValue,
};

use super::ffi::{arg_bool, arg_i64, error_out, instance_ref};

/// One registered timer.
struct TimerState {
    /// Retained script callback (the value passed to the constructor).
    /// `None` while a callback is being invoked (taken out so the map guard
    /// can be released before calling back into the VM).
    callback: Option<tjs2_sys::DetachedValue>,
    /// Interval in milliseconds.
    interval_ms: u64,
    enabled: bool,
    /// Monotonic-clock time of the next fire.
    next_fire_ms: u64,
    /// How many times the timer has fired.
    count: u64,
}

/// Payload of one script-visible `Timer` object.
#[derive(Default)]
pub(crate) struct TimerInst {
    pub id: u32,
    pub constructed: bool,
}

static TIMERS: LazyLock<Mutex<HashMap<u32, TimerState>>> = LazyLock::new(Default::default);
static NEXT_ID: LazyLock<Mutex<u32>> = LazyLock::new(|| Mutex::new(0));

fn next_id() -> u32 {
    let mut n = NEXT_ID.lock().unwrap_or_else(|p| p.into_inner());
    let id = *n;
    *n += 1;
    id
}

/// `new Timer(callback[, actionName])` — retain the callback; the action
/// name is accepted and ignored (the reference uses it to name the async
/// trigger; we call the callback directly).
extern "C" fn timer_ctor(
    _engine: *mut std::ffi::c_void,
    argc: std::ffi::c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: the trampoline guarantees valid argv/out/out_error.
    let args = unsafe { super::ffi::args(argc, argv) };
    if args.is_empty() {
        return error_out(out_error, "Timer: constructor requires a callback");
    }
    // The callback ABI's raw `engine` pointer is the `tjs2_engine*` (the
    // engine's `inner` field), NOT a `Tjs2Engine*` — reinterpreting it as a
    // `Tjs2Engine` and reading `.inner` would re-read the C struct's first
    // field and yield a bogus engine. Use the engine registered by
    // [`super::register_visual`] instead (see `context_engine`).
    let engine = super::context_engine();
    // Retain the callback (arg0) as a detached value so it can live in the
    // global registry. Object values are resolved against the most recent
    // object-valued script result — the constructor argument.
    let cb = TjsValue::Object;
    let retained = match engine.retain_value_detached(&cb) {
        Ok(dv) => dv,
        Err(e) => return error_out(out_error, &format!("Timer: {e}")),
    };
    let id = next_id();
    let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
    timers.insert(
        id,
        TimerState {
            callback: Some(retained),
            interval_ms: 1000,
            enabled: false,
            next_fire_ms: 0,
            count: 0,
        },
    );
    drop(timers);
    // SAFETY: out is a valid return slot.
    unsafe {
        (*out).ty = tjs2_sys::VAL_INTEGER;
        (*out).integer = i64::from(id);
    }
    0
}

extern "C" fn timer_destroy(_engine: *mut std::ffi::c_void, instance: *mut std::ffi::c_void) {
    // SAFETY: instance came from Box::into_raw in the create callback.
    let inst = unsafe { instance_ref::<TimerInst>(instance) };
    if inst.constructed {
        let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(state) = timers.remove(&inst.id) {
            drop(state); // drops the DetachedValue -> releases the callback
        }
    }
    // SAFETY: instance came from Box::into_raw.
    unsafe { drop(Box::from_raw(instance as *mut TimerInst)) };
}

extern "C" fn timer_interval_get(
    _engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: instance is a valid TimerInst.
    let inst = unsafe { instance_ref::<TimerInst>(instance) };
    let timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
    let ms = timers.get(&inst.id).map(|t| t.interval_ms).unwrap_or(0);
    drop(timers);
    // SAFETY: out is a valid return slot.
    unsafe {
        (*out).ty = tjs2_sys::VAL_INTEGER;
        (*out).integer = ms as i64;
    }
    0
}

extern "C" fn timer_interval_set(
    _engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    value: *const tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: instance is a valid TimerInst; value is a valid value slot.
    let inst = unsafe { instance_ref::<TimerInst>(instance) };
    let v = unsafe { &*value };
    let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(t) = timers.get_mut(&inst.id) {
        t.interval_ms = arg_i64(v).max(1) as u64;
    }
    0
}

extern "C" fn timer_enabled_get(
    _engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: instance is a valid TimerInst.
    let inst = unsafe { instance_ref::<TimerInst>(instance) };
    let timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
    let enabled = timers.get(&inst.id).map(|t| t.enabled).unwrap_or(false);
    drop(timers);
    // SAFETY: out is a valid return slot.
    unsafe {
        (*out).ty = tjs2_sys::VAL_INTEGER;
        (*out).integer = enabled as i64;
    }
    0
}

extern "C" fn timer_enabled_set(
    _engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    value: *const tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: instance is a valid TimerInst; value is a valid value slot.
    let inst = unsafe { instance_ref::<TimerInst>(instance) };
    let v = unsafe { &*value };
    let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(t) = timers.get_mut(&inst.id) {
        t.enabled = arg_bool(v);
        if t.enabled {
            // Reschedule from now (the reference reschedules on enable).
            t.next_fire_ms = 0;
        }
    }
    0
}

/// `timer.capacity` — the number of pending callback slots.
extern "C" fn timer_capacity_get(
    _engine: *mut std::ffi::c_void,
    _instance: *mut std::ffi::c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: out is a valid result slot.
    unsafe {
        (*out).ty = tjs2_sys::VAL_INTEGER;
        (*out).integer = 1;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
    }
    0
}

/// `timer.capacity = n` — accepted (the VM fires at most one callback per
/// interval, matching capacity>=1).
extern "C" fn timer_capacity_set(
    _engine: *mut std::ffi::c_void,
    _instance: *mut std::ffi::c_void,
    _value: *const tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    0
}

extern "C" fn timer_count_get(
    _engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: instance is a valid TimerInst.
    let inst = unsafe { instance_ref::<TimerInst>(instance) };
    let timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
    let count = timers.get(&inst.id).map(|t| t.count).unwrap_or(0);
    drop(timers);
    // SAFETY: out is a valid return slot.
    unsafe {
        (*out).ty = tjs2_sys::VAL_INTEGER;
        (*out).integer = count as i64;
    }
    0
}

extern "C" fn timer_id_get(
    _engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: instance is a valid TimerInst.
    let inst = unsafe { instance_ref::<TimerInst>(instance) };
    // SAFETY: out is a valid return slot.
    unsafe {
        (*out).ty = tjs2_sys::VAL_INTEGER;
        (*out).integer = i64::from(inst.id);
    }
    0
}

/// Register the `Timer` native class (instance-based).
pub(crate) fn register_timer(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "Timer",
        create: timer_create,
        destroy: timer_destroy,
        methods: vec![NativeInstanceMethodDef {
            name: "Timer",
            f: timer_ctor_hook,
        }],
        properties: vec![
            NativeInstancePropertyDef {
                name: "id",
                get: Some(timer_id_get),
                set: None,
            },
            NativeInstancePropertyDef {
                name: "interval",
                get: Some(timer_interval_get),
                set: Some(timer_interval_set),
            },
            NativeInstancePropertyDef {
                name: "enabled",
                get: Some(timer_enabled_get),
                set: Some(timer_enabled_set),
            },
            NativeInstancePropertyDef {
                name: "capacity",
                get: Some(timer_capacity_get),
                set: Some(timer_capacity_set),
            },
            NativeInstancePropertyDef {
                name: "count",
                get: Some(timer_count_get),
                set: None,
            },
        ],
    })
}

extern "C" fn timer_create(engine: *mut std::ffi::c_void) -> *mut std::ffi::c_void {
    let _ = engine;
    Box::into_raw(Box::new(TimerInst::default())) as *mut std::ffi::c_void
}

/// The constructor hook the instance machinery calls for `new Timer(...)`.
extern "C" fn timer_ctor_hook(
    engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    argc: std::ffi::c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: instance is a valid TimerInst.
    let inst = unsafe { instance_ref::<TimerInst>(instance) };
    if inst.constructed {
        return error_out(out_error, "Timer: already constructed");
    }
    let rc = timer_ctor(engine, argc, argv, out, out_error, std::ptr::null_mut());
    if rc == 0 {
        // SAFETY: timer_ctor wrote the new timer id into *out.
        inst.id = unsafe { (*out).integer } as u32;
        inst.constructed = true;
    }
    rc
}

/// Fire due timers. `now_ms` is a monotonic millisecond clock.
pub(crate) fn timer_poll(engine: &Tjs2Engine, now_ms: u64) {
    // Snapshot the ids of due timers, then fire them one at a time so a
    // callback that registers/disables timers is safe.
    let due: Vec<(u32, u64)> = {
        let timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
        timers
            .iter()
            .filter(|(_, t)| t.enabled && t.next_fire_ms <= now_ms)
            .map(|(&id, t)| (id, t.interval_ms))
            .collect()
    };
    for (id, interval_ms) in due {
        let fire = {
            let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
            match timers.get_mut(&id) {
                Some(t) if t.enabled && t.next_fire_ms <= now_ms => {
                    t.next_fire_ms = now_ms + interval_ms;
                    t.count += 1;
                    true
                }
                _ => false,
            }
        };
        if !fire {
            continue;
        }
        // Take the callback out so the map guard is released before the VM
        // runs (the callback may register/disable timers).
        let cb = {
            let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
            timers.get_mut(&id).and_then(|t| t.callback.take())
        };
        let Some(cb) = cb else { continue };
        match engine.call_detached(&cb, &[]) {
            Ok(_) => {}
            Err(e) => {
                log::warn!("Timer {id}: callback failed ({e}); disabling");
                if let Some(t) = TIMERS
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get_mut(&id)
                {
                    t.enabled = false;
                }
            }
        }
        // Put the callback back (unless the timer was destroyed during the
        // callback, in which case dropping it releases the retained value).
        let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
        match timers.get_mut(&id) {
            Some(t) => t.callback = Some(cb),
            None => drop(cb),
        }
    }
}
