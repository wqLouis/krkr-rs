//! `AsyncTrigger` native class — ported from
//! `reference/cpp/core/base/EventIntf.cpp`.
//!
//! Delivers an event to a script callback asynchronously: `trigger()`
//! schedules the callback; the app's event loop runs pending triggers at the
//! next flush ([`async_trigger_poll`]). `cached` defaults to **true** (the
//! reference's `tTJSNI_AsyncTrigger` constructor, EventIntf.cpp:1001): while
//! a trigger is pending, re-triggering coalesces it (one pending entry per
//! instance; the reference only coalesces when `cached` is set — with
//! `cached = false` the reference can queue several, a distinction this
//! single-entry queue does not model). Changing `cached` or `mode` cancels
//! pending events, like the reference `SetCached`/`SetMode`.
//!
//! The constructor is `AsyncTrigger(action [, actionName])`. `actionName`
//! selects a member to invoke on `action`; an **empty** name invokes the
//! object itself (the game's `new AsyncTrigger(onCleaning, "")`), and an
//! omitted/void name defaults to `"action"`, mirroring `TVPActionName`
//! (EventIntf.cpp:740).
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
    Tjs2Engine, Value,
};

/// Instance payload: the retained action callback + flags.
struct AsyncTriggerInst {
    /// Retained script callback (None after a non-cached fire).
    action: Option<DetachedValue>,
    /// Method to invoke on `action`; `None` means invoke `action` itself
    /// (the reference's empty action name). Defaults to `"action"` when the
    /// constructor's second argument is omitted/void.
    action_name: Option<String>,
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
        action_name: Some("action".to_string()),
        cached: true, // reference: Cached defaults to true (EventIntf.cpp:1001)
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
    // The ctor arg is a function object. Use the per-argument handle so the
    // retention targets `param[0]` even when the constructor call passes more
    // than one object (the engine's `last_object` slot only remembers one).
    match eng.retain_object_arg(&args[0]) {
        Ok(action) => inst.action = Some(action),
        Err(e) => {
            return crate::report_error(
                out_error,
                &format!("AsyncTrigger: cannot retain callback: {e}"),
            );
        }
    }
    // Second argument is the action method name; empty means "call the
    // object itself" (the game's `new AsyncTrigger(onCleaning, "")`).
    if args.len() >= 2 && args[1].ty != tjs2_sys::VAL_VOID {
        let name = crate::value_as_string(&args[1]);
        inst.action_name = if name.is_empty() { None } else { Some(name) };
    } else {
        inst.action_name = Some("action".to_string());
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

/// Invoke the retained action, honoring the action name (`None` calls the
/// object itself).
fn invoke_action(inst: &AsyncTriggerInst) {
    let Some(action) = &inst.action else {
        return;
    };
    let eng = crate::context_engine();
    let result = match &inst.action_name {
        Some(name) => eng.call_member(action.raw_id(), name, &[]),
        None => eng.call_detached(action, &[]),
    };
    if let Err(e) = result {
        // A failure here is why a deferred callback "never happened": KAG
        // dispatches transition-complete handlers (`loadStart`, `loadEnd`)
        // through `AsyncTrigger`, so a silent failure leaves the load screen
        // up with input still captured.
        log::warn!("AsyncTrigger: action failed: {e}");
    }
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
    inst.pending = false;
    PENDING.lock().unwrap().remove(&(instance as usize));
    invoke_action(inst);
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
    let new_value = value_as_bool(unsafe { &*value });
    if inst.cached != new_value {
        inst.cached = new_value;
        // reference SetCached: changing the flag cancels pending events
        inst.pending = false;
        PENDING.lock().unwrap().remove(&(instance as usize));
    }
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
    let new_mode = value_as_i64(unsafe { &*value });
    if inst.mode != new_mode {
        inst.mode = new_mode;
        // reference SetMode: changing the mode cancels pending events
        inst.pending = false;
        PENDING.lock().unwrap().remove(&(instance as usize));
    }
    0
}

/// Register the `AsyncTrigger` native class on the engine's global object.
pub fn register_async_trigger(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "AsyncTrigger",
        create: async_trigger_create,
        destroy: async_trigger_destroy,
        invalidate: None,
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
pub fn async_trigger_poll(_engine: &Tjs2Engine) {
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
        invoke_action(inst);
    }
}

#[cfg(test)]
mod tests {
    use crate::test_lock::vm_lock;

    use super::*;
    use tjs2_sys::{Tjs2Engine, TjsValue};

    fn engine() -> &'static Tjs2Engine {
        // Leak the engine so its address is stable: `register_all` stores the
        // `Tjs2Engine` wrapper address in a process global, and the native
        // callbacks (AsyncTrigger) resolve it later.
        let e = Box::leak(Box::new(Tjs2Engine::new().expect("create engine")));
        crate::register_all(e).expect("register all natives");
        e
    }

    #[test]
    fn cached_defaults_to_true() {
        let _vm_lock = vm_lock();
        let e = engine();
        e.exec_script("var t = new AsyncTrigger(function(){}, '');", "test")
            .unwrap();
        assert_eq!(
            e.eval("t.cached", "test").unwrap(),
            TjsValue::Integer(1),
            "reference tTJSNI_AsyncTrigger defaults Cached to true"
        );
    }

    #[test]
    fn trigger_fires_at_poll_and_coalesces() {
        let _vm_lock = vm_lock();
        let e = engine();
        e.exec_script(
            "var n = 0; var t = new AsyncTrigger(function(){ n += 1; }, ''); \
             t.trigger(); t.trigger(); t.trigger();",
            "test",
        )
        .unwrap();
        assert_eq!(e.eval("n", "test").unwrap(), TjsValue::Integer(0));
        async_trigger_poll(e);
        assert_eq!(e.eval("n", "test").unwrap(), TjsValue::Integer(1));
        // fire again later
        e.exec_script("t.trigger();", "test").unwrap();
        async_trigger_poll(e);
        assert_eq!(e.eval("n", "test").unwrap(), TjsValue::Integer(2));
    }

    #[test]
    fn cancel_drops_a_pending_trigger() {
        let _vm_lock = vm_lock();
        let e = engine();
        e.exec_script(
            "var n = 0; var t = new AsyncTrigger(function(){ n += 1; }, ''); \
             t.trigger(); t.cancel();",
            "test",
        )
        .unwrap();
        async_trigger_poll(e);
        assert_eq!(e.eval("n", "test").unwrap(), TjsValue::Integer(0));
    }

    #[test]
    fn changing_cached_or_mode_cancels_pending() {
        let _vm_lock = vm_lock();
        let e = engine();
        e.exec_script(
            "var n = 0; var t = new AsyncTrigger(function(){ n += 1; }, ''); \
             t.trigger(); t.cached = false;",
            "test",
        )
        .unwrap();
        async_trigger_poll(e);
        assert_eq!(e.eval("n", "test").unwrap(), TjsValue::Integer(0));

        e.exec_script("t.trigger(); t.mode = atmAtIdle;", "test")
            .unwrap();
        async_trigger_poll(e);
        assert_eq!(e.eval("n", "test").unwrap(), TjsValue::Integer(0));
        // setting the same value again must not cancel a fresh trigger
        e.exec_script("t.mode = atmAtIdle; t.trigger();", "test")
            .unwrap();
        async_trigger_poll(e);
        assert_eq!(e.eval("n", "test").unwrap(), TjsValue::Integer(1));
    }

    #[test]
    fn action_name_selects_a_member() {
        let _vm_lock = vm_lock();
        let e = engine();
        // The member is invoked with `this` = the retained object, so mutate
        // a property of that object (TJS member-call variable resolution).
        e.exec_script(
            "var obj = %[count: 0, action: function(){ this.count += 10; }]; \
             var t = new AsyncTrigger(obj); t.trigger();",
            "test",
        )
        .unwrap();
        async_trigger_poll(e);
        assert_eq!(
            e.eval("obj.count", "test").unwrap(),
            TjsValue::Integer(10),
            "omitted action name must invoke obj.action()"
        );
    }

    #[test]
    fn empty_action_name_invokes_the_object_itself() {
        let _vm_lock = vm_lock();
        let e = engine();
        e.exec_script(
            "var n = 0; var f = function(){ n += 5; }; \
             var t = new AsyncTrigger(f, ''); t.trigger();",
            "test",
        )
        .unwrap();
        async_trigger_poll(e);
        assert_eq!(e.eval("n", "test").unwrap(), TjsValue::Integer(5));
    }
}
