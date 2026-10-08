//! The visual `Timer` native class and its frame poll.

use std::ffi::{c_char, c_int, c_void};

use engine::timer::{TimerConfig, TimerId};
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef, Value,
};

use super::common::{TimerBinding, VisualState, bound};
use crate::native_util::{args, report_error, set_bool, set_int, set_void};
use crate::natives::system::{value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError};

/// `Timer` payload: the L2 timer registry id.
#[derive(Default)]
struct TimerInst {
    id: Option<TimerId>,
}

// ---------------------------------------------------------------------------
// Timer (L3 adapter over the L2 `engine::timer::TimerState` registry)
// ---------------------------------------------------------------------------

extern "C" fn timer_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(TimerInst::default())) as *mut c_void
}

extern "C" fn timer_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    let inst = unsafe { &*(instance as *const TimerInst) };
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        if let Some(timer) = services.timer() {
            // SAFETY: `attach_timer` guarantees the pointer stays valid.
            let _ = unsafe { &mut *timer }.remove(id);
        }
        // The dropped `TimerBinding` releases the callback and owner.
        services.state::<VisualState>().timers.remove(&id);
    }
    // SAFETY: instance came from `timer_create`'s `Box::into_raw` and is
    // freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut TimerInst) });
}

/// `Timer(callback[, actionName])` — retain the callback and register the L2
/// timer. The action name is accepted and ignored.
extern "C" fn timer_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(out_error, "Timer: constructor requires a callback");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer: compat context is not bound");
    };
    let host = resolved.engine();
    let callback = match host.retain_object_arg(&unsafe { args(argv, argc) }[0]) {
        Ok(callback) => callback,
        Err(error) => return report_error(out_error, &format!("Timer: {error}")),
    };
    let owner = host.retain_object_detached(objthis).ok();
    let inst = unsafe { &mut *(instance as *mut TimerInst) };
    if inst.id.is_some() {
        return report_error(out_error, "Timer: already constructed");
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let Some(timer_ptr) = services.timer() else {
        return report_error(out_error, "Timer: timer registry is not attached");
    };
    // SAFETY: `attach_timer` guarantees the pointer stays valid.
    let id = match unsafe { &mut *timer_ptr }.create(TimerConfig::default()) {
        Ok(id) => id,
        Err(error) => return report_error(out_error, &format!("Timer: {error}")),
    };
    services.state::<VisualState>().timers.insert(
        id,
        TimerBinding {
            callback: Some(callback),
            owner,
            mode: 0,
        },
    );
    inst.id = Some(id);
    set_int(out, id.value() as i64);
    0
}

/// The native `onTimer` method: invoke the constructor callback. A script
/// subclass that overrides `onTimer` never reaches here.
extern "C" fn timer_on_timer(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.onTimer: compat context is not bound");
    };
    let host = resolved.engine();
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return report_error(out_error, "Timer.onTimer: this timer is not constructed");
    };
    // Take the callback out so the state borrow is released across the call
    // (the callback may register/disable timers).
    let callback = {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let mut state = unsafe { ctx.services() }.state::<VisualState>();
        state
            .timers
            .get_mut(&id)
            .and_then(|timer| timer.callback.take())
    };
    let Some(callback) = callback else {
        set_void(out);
        return 0;
    };
    let result = host.call_detached(&callback, &[]);
    {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let mut state = unsafe { ctx.services() }.state::<VisualState>();
        match state.timers.get_mut(&id) {
            Some(timer) => timer.callback = Some(callback),
            None => drop(callback),
        }
    }
    if let Err(error) = result {
        log::warn!("Timer {id:?}: callback failed ({error}); disabling");
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        if let Some(timer) = services.timer() {
            let now = services.state::<VisualState>().clock.now_ms();
            // SAFETY: `attach_timer` guarantees the pointer stays valid.
            let _ = unsafe { &mut *timer }.set_enabled(id, false, now);
        }
    }
    set_void(out);
    0
}

/// `timer.interval` (ms).
extern "C" fn timer_interval_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.interval: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let interval = unsafe { ctx.services() }
        .timer()
        .and_then(|timer| unsafe { &*timer }.interval_ms(id))
        .unwrap_or(0);
    set_int(out, interval as i64);
    0
}

extern "C" fn timer_interval_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.interval: compat context is not bound");
    };
    let interval = value_as_i64(unsafe { &*value }).max(1) as u64;
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let now = services.state::<VisualState>().clock.now_ms();
    if let Some(timer) = services.timer() {
        // SAFETY: `attach_timer` guarantees the pointer stays valid.
        let _ = unsafe { &mut *timer }.set_interval(id, interval, now);
    }
    0
}

/// `timer.enabled`.
extern "C" fn timer_enabled_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.enabled: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        set_bool(out, false);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let enabled = unsafe { ctx.services() }
        .timer()
        .and_then(|timer| unsafe { &*timer }.is_enabled(id))
        .unwrap_or(false);
    set_bool(out, enabled);
    0
}

extern "C" fn timer_enabled_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.enabled: compat context is not bound");
    };
    let enabled = value_as_bool(unsafe { &*value });
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let now = services.state::<VisualState>().clock.now_ms();
    if let Some(timer) = services.timer() {
        // SAFETY: `attach_timer` guarantees the pointer stays valid.
        let _ = unsafe { &mut *timer }.set_enabled(id, enabled, now);
    }
    0
}

/// `timer.capacity`.
extern "C" fn timer_capacity_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.capacity: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        set_int(out, 1);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let capacity = unsafe { ctx.services() }
        .timer()
        .and_then(|timer| unsafe { &*timer }.capacity(id))
        .unwrap_or(1);
    set_int(out, i64::from(capacity));
    0
}

extern "C" fn timer_capacity_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.capacity: compat context is not bound");
    };
    let capacity = value_as_i64(unsafe { &*value }).clamp(1, i64::from(u32::MAX)) as u32;
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    if let Some(timer) = unsafe { ctx.services() }.timer() {
        // SAFETY: `attach_timer` guarantees the pointer stays valid.
        let _ = unsafe { &mut *timer }.set_capacity(id, capacity);
    }
    0
}

/// `timer.mode` (the TVP `tTVPAsyncTriggerMode`, kept in L3).
extern "C" fn timer_mode_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.mode: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        set_int(out, 0);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let mode = unsafe { ctx.services() }
        .state::<VisualState>()
        .timers
        .get(&id)
        .map_or(0, |timer| timer.mode);
    set_int(out, mode);
    0
}

extern "C" fn timer_mode_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.mode: compat context is not bound");
    };
    let mode = value_as_i64(unsafe { &*value });
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    if let Some(timer) = unsafe { ctx.services() }
        .state::<VisualState>()
        .timers
        .get_mut(&id)
    {
        timer.mode = mode;
    }
    0
}

/// Fire every due timer (`FrameBody::timer_poll`). Returns whether any timer
/// remains enabled (the `system::continuous_handler_poll` convention).
#[allow(dead_code)]
pub fn timer_poll(ctx: &mut CompatCtx) -> Result<bool, CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let host = unsafe { ctx.engine() };
    let services = unsafe { ctx.services() };
    let now = services.state::<VisualState>().clock.now_ms();
    let Some(timer_ptr) = services.timer() else {
        return Err(CompatError::Engine("timer registry is not attached".into()));
    };
    // Advance the L2 registry; it reports the due timers and their counts.
    // SAFETY: `attach_timer` guarantees the pointer stays valid.
    let due = unsafe { &mut *timer_ptr }.advance(now);
    for fire in due {
        let id = fire.id;
        // Dispatch the object's `onTimer` member (a script subclass override
        // wins); fall back to the callback when the owner could not be
        // retained.
        let owner = {
            let state = services.state::<VisualState>();
            state
                .timers
                .get(&id)
                .and_then(|timer| timer.owner.as_ref().map(DetachedValue::raw_id))
        };
        let result = match owner {
            Some(owner) => host.call_member(owner, "onTimer", &[]),
            None => {
                let callback = {
                    let mut state = services.state::<VisualState>();
                    state
                        .timers
                        .get_mut(&id)
                        .and_then(|timer| timer.callback.take())
                };
                match callback {
                    Some(callback) => {
                        let result = host.call_detached(&callback, &[]);
                        let mut state = services.state::<VisualState>();
                        match state.timers.get_mut(&id) {
                            Some(timer) => timer.callback = Some(callback),
                            None => drop(callback),
                        }
                        result
                    }
                    None => continue,
                }
            }
        };
        if let Err(error) = result {
            let interval = unsafe { &*timer_ptr }.interval_ms(id).unwrap_or(0);
            log::warn!("Timer {id:?}: onTimer failed ({error}) after {interval} ms; disabling");
            // SAFETY: the pointer is still attached.
            let _ = unsafe { &mut *timer_ptr }.set_enabled(id, false, now);
        }
    }
    // SAFETY: the pointer is still attached.
    Ok(unsafe { &*timer_ptr }.any_enabled())
}

pub(super) fn register_timer_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Timer",
        create: timer_create,
        destroy: timer_destroy,
        invalidate: None,
        methods: vec![
            NativeInstanceMethodDef {
                name: "Timer",
                f: timer_ctor,
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
                name: "mode",
                get: Some(timer_mode_get),
                set: Some(timer_mode_set),
            },
        ],
    })
}

extern "C" fn timer_id_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let inst = unsafe { &*(instance as *const TimerInst) };
    set_int(out, inst.id.map_or(0, |id| id.value() as i64));
    0
}
