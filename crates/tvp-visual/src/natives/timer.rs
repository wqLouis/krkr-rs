//! `Timer` native class — ported from
//! `reference/cpp/core/utils/TimerIntf.cpp` / `utils/impl/TimerImpl.cpp`.
//!
//! Script surface: `new Timer(callback[, actionName])`, properties
//! `interval` (ms, rw), `enabled` (rw). Firing happens through
//! [`timer_poll`], which the app's update loop calls each frame with a
//! monotonic millisecond clock; due timers dispatch the **`onTimer` member
//! on the timer object** — the reference posts an "onTimer" event to the
//! Timer object and the object's class chain resolves the handler, so a
//! script subclass that overrides `onTimer` (like the game's `OnceTimer`,
//! which cancels itself and calls the wrapped function once) runs its
//! override. A plain `Timer`'s `onTimer` is the native method below, which
//! invokes the constructor's callback argument.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tjs2_sys::{
    NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Tjs2Engine, TjsValue,
};

use super::ffi::{arg_bool, arg_i64, error_out, instance_ref};

/// One registered timer.
struct TimerState {
    /// Retained script callback (the value passed to the constructor); the
    /// native `onTimer` method invokes it. `None` while a callback is being
    /// invoked (taken out so the map guard can be released before calling
    /// back into the VM).
    callback: Option<tjs2_sys::DetachedValue>,
    /// Retained timer *object* (the `this` of the `new Timer(...)` /
    /// `super.Timer(...)` expression): `timer_poll` dispatches its
    /// `onTimer` member through the object's class chain, so script
    /// subclass overrides run.
    owner: Option<tjs2_sys::DetachedValue>,
    /// Interval in milliseconds.
    interval_ms: u64,
    enabled: bool,
    /// Monotonic-clock time of the next fire.
    next_fire_ms: u64,
    /// The most recent clock value seen by [`timer_poll`] (the reference
    /// reschedules from *now + interval* on enable/interval change, so we
    /// need the last polled clock; 0 before the first poll).
    last_now_ms: u64,
    /// How many times the timer has fired.
    count: u64,
    /// Event delivery mode (`tTVPAsyncTriggerMode`, `EventIntf.h:312`):
    /// `atmNormal` = 0, `atmExclusive` = 1, `atmAtIdle` = 2. The reference
    /// `tTJSNI_BaseTimer` stores it (`TimerIntf.h:39`) and uses it to tag the
    /// posted `onTimer` event; with no event queue the mode is retained but
    /// does not change the synchronous dispatch performed by [`timer_poll`].
    mode: i64,
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

/// `new Timer(callback[, actionName])` — retain the callback (arg0) and the
/// timer object (`objthis`). The action name is accepted and ignored (the
/// reference uses it to name the async trigger; we dispatch `onTimer`
/// directly).
extern "C" fn timer_ctor(
    _engine: *mut std::ffi::c_void,
    argc: std::ffi::c_int,
    argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    out_error: *mut *mut std::ffi::c_char,
    objthis: *mut std::ffi::c_void,
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
    let retained = match engine.retain_value_detached(&TjsValue::Object) {
        Ok(dv) => dv,
        Err(e) => return error_out(out_error, &format!("Timer: {e}")),
    };
    // Retain the timer object itself so `timer_poll` can dispatch its
    // `onTimer` member (which resolves script-subclass overrides).
    let owner = match engine.retain_object_detached(objthis) {
        Ok(dv) => Some(dv),
        Err(e) => {
            log::warn!("Timer: cannot retain timer object ({e}); falling back to direct callback");
            None
        }
    };
    let id = next_id();
    let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
    timers.insert(
        id,
        TimerState {
            callback: Some(retained),
            owner,
            interval_ms: 1000,
            enabled: false,
            next_fire_ms: 0,
            last_now_ms: 0,
            count: 0,
            mode: 0,
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
            drop(state); // drops the DetachedValues -> releases callback + object
        }
    }
    // SAFETY: instance came from Box::into_raw.
    unsafe { drop(Box::from_raw(instance as *mut TimerInst)) };
}

/// The native `onTimer` method: the reference's Timer class exposes
/// `onTimer` as the event handler that invokes the constructor's callback
/// (`ActionOwner`). A plain `new Timer(cb)` dispatches here; script
/// subclasses that override `onTimer` (OnceTimer) never reach this method.
extern "C" fn timer_on_timer(
    _engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    _argc: std::ffi::c_int,
    _argv: *const tjs2_sys::Value,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: instance is a valid TimerInst.
    let inst = unsafe { instance_ref::<TimerInst>(instance) };
    let engine = super::context_engine();
    // Take the callback out so the map guard is released before the VM
    // runs (the callback may register/disable timers).
    let cb = {
        let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
        timers.get_mut(&inst.id).and_then(|t| t.callback.take())
    };
    if let Some(cb) = cb {
        let result = engine.call_detached(&cb, &[]);
        // Put the callback back (unless the timer was destroyed during the
        // callback, in which case dropping it releases the retained value).
        // Scope the guard: the error branch below must re-lock TIMERS, and
        // `std::sync::Mutex` is not reentrant (a live guard here would
        // self-deadlock the VM thread whenever a callback errors).
        {
            let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
            match timers.get_mut(&inst.id) {
                Some(t) => t.callback = Some(cb),
                None => drop(cb),
            }
        }
        if let Err(e) = result {
            log::warn!("Timer {}: callback failed ({e}); disabling", inst.id);
            if let Some(t) = TIMERS
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get_mut(&inst.id)
            {
                t.enabled = false;
            }
        }
    }
    // SAFETY: out is a valid return slot.
    unsafe {
        (*out).ty = tjs2_sys::VAL_VOID;
        (*out).integer = 0;
        (*out).real = 0.0;
        (*out).string = std::ptr::null();
    }
    0
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
        // The reference reschedules an *enabled* timer from now + interval
        // when its interval changes (SetInterval: CancelEvents + SetNextTick
        // now+interval).
        if t.enabled {
            t.next_fire_ms = t.last_now_ms.saturating_add(t.interval_ms);
        }
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
            // The reference reschedules on enable: fire at now + interval
            // (SetEnabled: SetNextTick(now + interval)), not immediately.
            t.next_fire_ms = t.last_now_ms.saturating_add(t.interval_ms);
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

/// `timer.mode` — event delivery mode (`tTVPAsyncTriggerMode`, reference
/// `TimerIntf.cpp:240`). `tTJSNI_BaseTimer` stores the raw enum value; the
/// setter casts without validation, so any integer round-trips.
extern "C" fn timer_mode_get(
    _engine: *mut std::ffi::c_void,
    instance: *mut std::ffi::c_void,
    out: *mut tjs2_sys::Value,
    _out_error: *mut *mut std::ffi::c_char,
    _objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: instance is a valid TimerInst.
    let inst = unsafe { instance_ref::<TimerInst>(instance) };
    let timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
    let mode = timers.get(&inst.id).map(|t| t.mode).unwrap_or(0);
    drop(timers);
    // SAFETY: out is a valid return slot.
    unsafe {
        (*out).ty = tjs2_sys::VAL_INTEGER;
        (*out).integer = mode;
    }
    0
}

extern "C" fn timer_mode_set(
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
        // Reference `SetMode` casts the raw integer to the enum with no
        // validation (`TimerIntf.cpp:249`).
        t.mode = arg_i64(v);
    }
    0
}

/// `timer.count` — how many times the timer has fired (read-only).
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

/// Register the `Timer` native class (instance-based) plus the Kirikiroid2
/// compatibility globals `OnceCall` / `OnceCallCancel`.
///
/// The game this engine targets (`@set(kirikiriz=1)`, run on Kirikiroid2)
/// calls `OnceCall(fn, ms)` / `OnceCallCancel(fn)` as **bare global
/// functions** (e.g. `system/Title.tjs` Logo constructor:
/// `OnceCall(step01, 1000)`). They are one-shot timers: `OnceCall` runs the
/// callback once after `ms` milliseconds; `OnceCallCancel` cancels a
/// pending one by function identity. They are implemented as script
/// globals over the `Timer` native class (the same machinery the game's
/// own `OnceTimer` script class uses), with a function→timer registry so
/// `OnceCallCancel(fn)` can disable the pending timer.
pub(crate) fn register_timer(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "Timer",
        create: timer_create,
        destroy: timer_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Timer",
                f: timer_ctor_hook,
            },
            NativeInstanceMethodDef {
                name: "onTimer",
                f: timer_on_timer,
            },
        ],
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
            NativeInstancePropertyDef {
                name: "mode",
                get: Some(timer_mode_get),
                set: Some(timer_mode_set),
            },
        ],
    })?;

    // Kirikiroid2 compatibility: `OnceCall(fn, ms)` / `OnceCallCancel(fn)`.
    // One-shot timers over the `Timer` native class, with a function→timer
    // registry for cancellation by function identity. The game's scripts
    // (Title.tjs, EyeCatch.tjs, AttentionVoice, ...) call these as bare
    // globals; without them the logo keyframe chain never starts.
    let oncecall_script = r#"
        if(typeof global.OnceCall == "undefined"){
            global._onceCallReg = [];
            // Mirrors the game's own `OnceTimer` script class
            // (system/Utility.tjs): a one-shot timer that disables itself
            // after the first onTimer. The native Timer's onTimer invokes
            // the constructor callback; this subclass overrides it to
            // self-cancel first (so the wrapper's onTimer runs once).
            class _OnceCallTimer extends Timer{
                function _OnceCallTimer(func, time){
                    super.Timer(func, "");
                    interval = int(time);
                    capacity = 1;
                    enabled = true;
                }
                function onTimer(){
                    enabled = false;
                    super.onTimer();
                }
            }
            global.OnceCall = function(func, time){
                var t = new _OnceCallTimer(func, time);
                for(var i=0;i<global._onceCallReg.count;i++){
                    if(global._onceCallReg[i].func == func){
                        global._onceCallReg[i].timer.enabled = false;
                        global._onceCallReg[i].timer = t;
                        return t;
                    }
                }
                global._onceCallReg.add(%[func:func, timer:t]);
                return t;
            };
            global.OnceCallCancel = function(func){
                for(var i=0;i<global._onceCallReg.count;i++){
                    if(global._onceCallReg[i].func == func){
                        global._onceCallReg[i].timer.enabled = false;
                        return;
                    }
                }
            };
        }
    "#;
    // SAFETY-ish: exec_script runs on the VM thread at registration time.
    engine
        .exec_script(oncecall_script, "krkr_rs_oncecall")
        .map_err(|e| format!("failed to define OnceCall globals: {e}"))?;
    Ok(())
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
    objthis: *mut std::ffi::c_void,
) -> std::ffi::c_int {
    // SAFETY: instance is a valid TimerInst.
    let inst = unsafe { instance_ref::<TimerInst>(instance) };
    if inst.constructed {
        return error_out(out_error, "Timer: already constructed");
    }
    let rc = timer_ctor(engine, argc, argv, out, out_error, objthis);
    if rc == 0 {
        // SAFETY: timer_ctor wrote the new timer id into *out.
        inst.id = unsafe { (*out).integer } as u32;
        inst.constructed = true;
    }
    rc
}

/// Fire due timers. `now_ms` is a monotonic millisecond clock.
pub(crate) fn timer_poll(engine: &Tjs2Engine, now_ms: u64) {
    // Remember the latest clock so enable/interval changes reschedule from
    // the current time (the reference's SetEnabled/SetInterval use
    // TVPGetTickCount()).
    {
        let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
        for t in timers.values_mut() {
            t.last_now_ms = now_ms;
        }
    }
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
        // Dispatch the timer object's `onTimer` member (the reference posts
        // an "onTimer" event to the Timer object). The object's class chain
        // resolves the handler: a script subclass that overrides `onTimer`
        // (OnceTimer) runs its override; a plain Timer dispatches to the
        // native method below, which invokes the ctor callback.
        let owner = {
            let timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
            timers
                .get(&id)
                .and_then(|t| t.owner.as_ref())
                .map(|o| o.raw_id())
        };
        let trace = std::env::var("KRKR_TIMER_TRACE").is_ok();
        let fire_start = std::time::Instant::now();
        if trace {
            eprintln!("[timer-trace] firing timer {id} (interval {interval_ms}ms) at poll");
        }
        let result = match owner {
            Some(owner_id) => engine.call_member(owner_id, "onTimer", &[]),
            // No retained object (retain failed): fall back to the callback.
            None => {
                let cb = {
                    let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
                    timers.get_mut(&id).and_then(|t| t.callback.take())
                };
                let Some(cb) = cb else { continue };
                let result = engine.call_detached(&cb, &[]);
                let mut timers = TIMERS.lock().unwrap_or_else(|p| p.into_inner());
                match timers.get_mut(&id) {
                    Some(t) => t.callback = Some(cb),
                    None => drop(cb),
                }
                result
            }
        };
        if trace {
            let dt = fire_start.elapsed();
            if dt >= Duration::from_millis(500) {
                eprintln!("[timer-trace] timer {id} callback took {dt:?} (SLOW/HANGING?)");
            } else {
                eprintln!("[timer-trace] timer {id} callback done in {dt:?}");
            }
        }
        match result {
            Ok(_) => {}
            Err(e) => {
                log::warn!("Timer {id}: onTimer failed ({e}); disabling");
                if let Some(t) = TIMERS
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get_mut(&id)
                {
                    t.enabled = false;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::natives::tests::TestEnv;

    /// `Timer.mode` stores the raw `tTVPAsyncTriggerMode` value and round-trips
    /// it, like the reference `tTJSNI_BaseTimer` (`TimerIntf.cpp:240`); the
    /// setter casts without validation.
    #[test]
    fn timer_mode_property_round_trips() {
        let env = TestEnv::new("timer-mode");
        env.run("var t = new Timer(function() {}, '');").unwrap();
        assert_eq!(env.eval_int("t.mode"), 0, "atmNormal is the default");
        env.run("t.mode = 2;").unwrap();
        assert_eq!(env.eval_int("t.mode"), 2, "atmAtIdle");
        env.run("t.mode = 9;").unwrap();
        assert_eq!(env.eval_int("t.mode"), 9, "raw values round-trip");
    }
}
