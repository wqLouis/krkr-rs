//! `AsyncTrigger` native class — ported from
//! `reference/cpp/core/base/EventIntf.cpp`.
//!
//! Delivers an event to a script callback asynchronously: `trigger()`
//! schedules the callback; the app's event loop runs pending triggers at the
//! next flush ([`async_trigger_poll`]). With `cached = true` a pending
//! trigger coalesces (re-triggering while pending is a no-op); with
//! `cached = false` the trigger fires once and the action is discarded.
//! `mode` (`atmNormal`=0, `atmExclusive`=1, `atmAtIdle`=2) is stored;
//! delivery is uniform at the poll (idle) phase — exclusive/at-idle
//! distinctions are not meaningful in the headless loop yet.
//!
//! The game's `Utility.tjs` uses it for deferred cleanup:
//! `_cleaning = new AsyncTrigger(onCleaning, ""); _cleaning.cached = true;
//! _cleaning.mode = atmAtIdle;`

use std::collections::HashSet;
use std::ffi::{c_char, c_int, c_void};
use std::sync::{LazyLock, Mutex};

use crate::{set_void_out, value_as_bool, value_as_i64};
use tjs2_sys::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    Tjs2Engine, TjsValue, Value,
};

/// Instance payload: the retained action callback + flags.
struct AsyncTriggerInst {
    /// Retained script callback (None after a non-cached fire).
    action: Option<DetachedValue>,
    cached: bool,
    mode: i64,
    pending: bool,
}

/// Process-wide pending triggers: raw instance payload addresses. The app
/// runs a single VM on one thread, so a plain set is safe.
static PENDING: LazyLock<Mutex<HashSet<usize>>> = LazyLock::new(|| Mutex::new(HashSet::new()));

extern "C" fn async_trigger_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(AsyncTriggerInst {
        action: None,
        cached: false,
        mode: 0,
        pending: false,
    })) as *mut c_void
}

extern "C" fn async_trigger_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    PENDING.lock().unwrap().remove(&(instance as usize));
    // SAFETY: instance came from async_trigger_create.
    drop(unsafe { Box::from_raw(instance as *mut AsyncTriggerInst) }); // releases the action
}

/// `new AsyncTrigger(callback, name)` — retain the callback.
extern "C" fn async_trigger_ctor(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload; the trampoline holds argv alive.
    let inst = unsafe { &mut *(instance as *mut AsyncTriggerInst) };
    // The raw `engine` ABI pointer is the tjs2_engine*, NOT a Tjs2Engine*
    // (see the visual Timer's comment); use the engine registered at setup.
    let eng = crate::context_engine();
    let args = crate::args(argv, argc);
    if args.is_empty() {
        return crate::report_error(out_error, "AsyncTrigger: missing callback argument");
    }
    // The ctor arg is a function object; the ABI retains objects against
    // the engine's most recent object result (same as the visual Timer).
    match eng.retain_value_detached(&TjsValue::Object) {
        Ok(action) => inst.action = Some(action),
        Err(e) => {
            return crate::report_error(
                out_error,
                &format!("AsyncTrigger: cannot retain callback: {e}"),
            );
        }
    }
    inst.pending = false;
    set_void_out(out);
    0
}

/// `trigger()` — schedule the callback for the next poll.
extern "C" fn async_trigger_trigger(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload.
    let inst = unsafe { &mut *(instance as *mut AsyncTriggerInst) };
    if inst.action.is_none() {
        return crate::report_error(
            out_error,
            "AsyncTrigger: no action (already fired without cached)",
        );
    }
    if !inst.pending {
        inst.pending = true;
        PENDING.lock().unwrap().insert(instance as usize);
    }
    set_void_out(out);
    0
}

/// `cancel()` — drop a pending trigger (the action stays retained).
extern "C" fn async_trigger_cancel(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload.
    let inst = unsafe { &mut *(instance as *mut AsyncTriggerInst) };
    inst.pending = false;
    PENDING.lock().unwrap().remove(&(instance as usize));
    set_void_out(out);
    0
}

/// `onFire()` — fire now (the reference invokes the action through this).
extern "C" fn async_trigger_on_fire(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload; engine is the live engine.
    let inst = unsafe { &mut *(instance as *mut AsyncTriggerInst) };
    let eng = crate::context_engine();
    inst.pending = false;
    PENDING.lock().unwrap().remove(&(instance as usize));
    if let Some(action) = &inst.action {
        let _ = eng.call_detached(action, &[]); // errors are logged, not fatal
    }
    set_void_out(out);
    0
}

extern "C" fn async_trigger_cached_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload.
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    crate::set_int_out(out, i64::from(inst.cached));
    0
}

extern "C" fn async_trigger_cached_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is valid; value follows the trampoline contract.
    let inst = unsafe { &mut *(instance as *mut AsyncTriggerInst) };
    inst.cached = value_as_bool(unsafe { &*value });
    0
}

extern "C" fn async_trigger_mode_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload.
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    crate::set_int_out(out, inst.mode);
    0
}

extern "C" fn async_trigger_mode_set(
    _engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is valid; value follows the trampoline contract.
    let inst = unsafe { &mut *(instance as *mut AsyncTriggerInst) };
    inst.mode = value_as_i64(unsafe { &*value });
    0
}

/// Register the `AsyncTrigger` native class on the engine's global object.
pub fn register_async_trigger(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "AsyncTrigger",
        create: async_trigger_create,
        destroy: async_trigger_destroy,
        methods: vec![
            NativeInstanceMethodDef {
                name: "AsyncTrigger",
                f: async_trigger_ctor,
            },
            NativeInstanceMethodDef {
                name: "trigger",
                f: async_trigger_trigger,
            },
            NativeInstanceMethodDef {
                name: "cancel",
                f: async_trigger_cancel,
            },
            NativeInstanceMethodDef {
                name: "onFire",
                f: async_trigger_on_fire,
            },
        ],
        properties: vec![
            NativeInstancePropertyDef {
                name: "cached",
                get: Some(async_trigger_cached_get),
                set: Some(async_trigger_cached_set),
            },
            NativeInstancePropertyDef {
                name: "mode",
                get: Some(async_trigger_mode_get),
                set: Some(async_trigger_mode_set),
            },
        ],
    })
}

/// Fire all pending triggers (the app calls this once per frame, before
/// timers). A non-cached trigger's action is discarded after firing; cached
/// triggers stay armed for future `trigger()` calls.
pub fn async_trigger_poll(engine: &Tjs2Engine) {
    let pending: Vec<usize> = {
        let mut list = PENDING.lock().unwrap();
        let v: Vec<usize> = list.drain().collect();
        v
    };
    for addr in pending {
        if addr == 0 {
            continue;
        }
        // SAFETY: addresses are live AsyncTriggerInst payloads (destroy
        // removes them from PENDING before freeing).
        let inst = unsafe { &mut *(addr as *mut AsyncTriggerInst) };
        inst.pending = false;
        if let Some(action) = &inst.action {
            let _ = engine.call_detached(action, &[]); // errors are logged, not fatal
        }
    }
}
