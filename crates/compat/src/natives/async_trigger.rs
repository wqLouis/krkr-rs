//! `AsyncTrigger` native class.
//!
//! Behavioral reference: `reference/cpp/core/base/EventIntf.cpp`
//! (`tTJSNI_AsyncTrigger` / `tTJSNC_AsyncTrigger`) and `main`'s
//! `crates/tvp-natives/src/async_trigger.rs`.
//!
//! `AsyncTrigger` is a **deferred-callback** class: `trigger()` only
//! *schedules* a handler, and the frame body fires it later at the
//! [`async_trigger_poll`] step. No VM call is made from inside
//! `trigger()`, so a handler can never re-enter the VM through the native
//! call that scheduled it.
//!
//! The registry of scheduled fires is explicit L3 dispatch state in
//! [`CompatServices::state`], modelled like the L2
//! [`engine::timer::TimerState`](engine::timer::TimerState) registry: one
//! entry per live object holds the retained action closure, the action name,
//! the `cached`/`mode` flags and the object itself (for `onFire` dispatch),
//! plus a FIFO of scheduled fires. No process global is used.
//!
//! # Ported semantics
//!
//! * The constructor is `AsyncTrigger(action [, actionName])`. An empty name
//!   invokes `action` itself (the game's `new AsyncTrigger(onCleaning, "")`);
//!   an omitted/void name defaults to `"action"`, mirroring `TVPActionName`
//!   (`EventIntf.cpp:740`).
//! * `cached` defaults to **true** (`tTJSNI_AsyncTrigger` ctor,
//!   `EventIntf.cpp:1003-1011`) and, like the reference's native-instance
//!   members, survives `invalidate` (only `Owner`/`ActionOwner` are released
//!   at `EventIntf.cpp:1032-1040`). While a fire is scheduled, another
//!   `trigger()` cancels it and re-posts it — the reference `Trigger()` does
//!   `TVPCancelSourceEvents(Owner)` before `TVPPostEvent` when `Cached` is
//!   set (`EventIntf.cpp:1043-1060`), so the re-triggered fire moves to the
//!   end of the queue. With `cached = false` each `trigger()` queues its own
//!   fire. Changing `cached` cancels the scheduled fires (`SetCached`,
//!   `EventIntf.cpp:1072-1079`).
//! * `mode` (`atmNormal` = 0, `atmExclusive` = 1, `atmAtIdle` = 2) is stored
//!   and delivered uniformly at the poll. Changing it cancels the scheduled
//!   fires (`SetMode`, `EventIntf.cpp:1082-1089`).
//! * `cancel()` drops every scheduled fire (`Cancel`).
//! * `onFire()` invokes the retained action with the reference
//!   `TVP_ACTION_INVOKE_*` event dictionary `{type: "onFire", target:
//!   <the trigger>}` (`EventIntf.h:208-237`, `EventIntf.cpp:709-739`);
//!   [`async_trigger_poll`] dispatches the object's `onFire` member with the
//!   posted-event convention (zero arguments, `EventIntf.cpp:96-110`), so a
//!   script subclass override wins.
//! * After `invalidate`, `trigger`/`cancel` are silent no-ops — the
//!   reference guards them with `if(Owner)` and `Owner` is cleared at
//!   invalidation (`EventIntf.cpp:1036-1037`, `1044`, `1065`).
#![allow(unsafe_code)]

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, TjsValue, VAL_VOID, Value,
};

use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{resolve, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError, CompatServices};

/// The action-event `type` the reference passes to the action
/// (`TVP_ACTION_INVOKE_BEGIN(0, "onFire", objthis)`, `EventIntf.cpp:1139`).
const EVENT_TYPE: &str = "onFire";

/// The reference `TVPActionName`, the default action member (`EventIntf.cpp:740`).
const DEFAULT_ACTION_NAME: &str = "action";

/// Handle to one `AsyncTrigger` instance's registry entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct AsyncTriggerId(u64);

/// One object's deferred-callback state.
struct AsyncTriggerBinding {
    /// The retained constructor action (`param[0]`). Taken out while it runs
    /// so the registry borrow is released across the VM call.
    action: Option<DetachedValue>,
    /// Member to invoke on `action`; `None` invokes `action` itself (the
    /// reference's empty action name). Defaults to `"action"`.
    action_name: Option<String>,
    /// The raw `objthis` handle of the object itself (reference
    /// `tTJSNI_AsyncTrigger::Owner`, `reference/cpp/core/base/EventIntf.h:322`,
    /// held with no reference), for `onFire` dispatch through its class chain
    /// so a script subclass override wins. `0` means no owner was recorded.
    owner: usize,
}

/// One scheduled fire: the object to dispatch and a strong reference that
/// keeps it alive until the poll delivers the fire.
struct PendingTrigger {
    /// The object whose `onFire` is dispatched.
    id: AsyncTriggerId,
    /// A strong reference to the source object while the fire is queued.
    owner: Option<DetachedValue>,
}

/// The `AsyncTrigger` registry: one entry per live object plus the FIFO of
/// scheduled fires. Lives in [`CompatServices::state`] keyed by type.
///
/// `Cached` and `Mode` are deliberately *not* kept here: the reference holds
/// them as native-instance members that outlive `Invalidate` (only `Owner` /
/// `ActionOwner` are released, `EventIntf.cpp:1032-1040`), so they live in
/// [`AsyncTriggerInst`].
#[derive(Default)]
struct AsyncTriggerState {
    /// Monotonic id source; never reused.
    next_id: u64,
    /// Per-object state keyed by [`AsyncTriggerId`].
    triggers: HashMap<AsyncTriggerId, AsyncTriggerBinding>,
    /// Scheduled fires in `trigger()` order. A `cached` object appears at
    /// most once. Each entry retains its source object until delivery, like
    /// the reference event queue.
    pending: Vec<PendingTrigger>,
}

/// Typed `AsyncTrigger` failures.
#[derive(Debug, thiserror::Error)]
enum AsyncTriggerError {
    /// No registry entry for the given handle.
    #[error("unknown AsyncTrigger {0:?}")]
    UnknownTrigger(AsyncTriggerId),
    /// The retained action raised while firing.
    #[error("AsyncTrigger action failed: {0}")]
    Action(String),
    /// The action event dictionary could not be built.
    #[error("AsyncTrigger action event: {0}")]
    Event(String),
}

impl AsyncTriggerState {
    /// Register a constructed object's retained action and return its handle.
    fn insert(
        &mut self,
        action: DetachedValue,
        action_name: Option<String>,
        owner: usize,
    ) -> AsyncTriggerId {
        let id = AsyncTriggerId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.triggers.insert(
            id,
            AsyncTriggerBinding {
                action: Some(action),
                action_name,
                owner,
            },
        );
        id
    }

    /// The entry for `id`, or [`AsyncTriggerError::UnknownTrigger`].
    fn binding(&self, id: AsyncTriggerId) -> Result<&AsyncTriggerBinding, AsyncTriggerError> {
        self.triggers
            .get(&id)
            .ok_or(AsyncTriggerError::UnknownTrigger(id))
    }

    /// Remove `id` and drop every scheduled fire for it.
    fn remove(&mut self, id: AsyncTriggerId) -> Result<(), AsyncTriggerError> {
        self.binding(id)?;
        self.triggers.remove(&id);
        self.pending.retain(|pending| pending.id != id);
        Ok(())
    }

    /// Schedule `id`'s `onFire`; returns `false` when the entry is gone (an
    /// invalidated object), which the reference treats as a silent no-op
    /// (`Trigger()`'s `if(Owner)` guard, `EventIntf.cpp:1044`).
    ///
    /// A `cached` object first drops its own scheduled fires and then
    /// re-queues, mirroring the reference `TVPCancelSourceEvents(Owner)` +
    /// `TVPPostEvent` pair (`EventIntf.cpp:1047-1058`): the re-triggered fire
    /// moves to the end of the queue. An uncached object queues each call.
    fn trigger(&mut self, id: AsyncTriggerId, cached: bool, owner: Option<DetachedValue>) -> bool {
        if !self.triggers.contains_key(&id) {
            return false;
        }
        if cached {
            self.pending.retain(|pending| pending.id != id);
        }
        self.pending.push(PendingTrigger { id, owner });
        true
    }

    /// Drop every scheduled fire for `id` (reference `Cancel`,
    /// `EventIntf.cpp:1064-1069`); `false` when the entry is gone, which the
    /// reference also treats as a silent no-op (`if(Owner)` guard).
    fn cancel(&mut self, id: AsyncTriggerId) -> bool {
        let known = self.triggers.contains_key(&id);
        self.pending.retain(|pending| pending.id != id);
        known
    }

    /// Drop every scheduled fire for `id` — the reference `Cancel()` half of
    /// `SetCached`/`SetMode` (`EventIntf.cpp:1076`, `1086`), valid for any
    /// entry state (a gone entry has no pending fires anyway).
    fn cancel_pending(&mut self, id: AsyncTriggerId) {
        self.pending.retain(|pending| pending.id != id);
    }

    /// Take the fires scheduled for this poll, leaving the queue empty.
    ///
    /// `trigger()` calls made while a handler runs land in the emptied queue
    /// and are therefore deferred to the next poll instead of re-entering the
    /// VM mid-dispatch.
    fn take_pending(&mut self) -> Vec<PendingTrigger> {
        std::mem::take(&mut self.pending)
    }
}

/// The `AsyncTrigger` native instance payload: its registry handle plus the
/// reference native-instance members `Cached` / `Mode`.
///
/// Both live here rather than in the registry entry because the reference
/// keeps writing/reading them after `Invalidate` — `Invalidate` only clears
/// `Owner`/`ActionOwner` (`EventIntf.cpp:1032-1040`) — and the constructor
/// defaults (`Cached = true`, `Mode = atmNormal`, `EventIntf.cpp:1003-1011`)
/// apply to the instance from construction on.
struct AsyncTriggerInst {
    /// The registry handle, `None` until the constructor succeeded.
    id: Option<AsyncTriggerId>,
    /// Reference `Cached` — while set, one pending fire per object.
    cached: bool,
    /// Reference `tTVPAsyncTriggerMode` (`atmNormal`/`atmExclusive`/`atmAtIdle`).
    mode: i64,
}

impl Default for AsyncTriggerInst {
    fn default() -> Self {
        Self {
            id: None,
            // Reference ctor: `Cached = true`, `Mode = atmNormal`.
            cached: true,
            mode: 0,
        }
    }
}

/// Resolve the installed binding for a native callback.
fn bound(engine: *mut c_void) -> Result<(&'static Tjs2Engine, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "async_trigger: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved.engine(), ctx))
}

/// Invoke the action retained for `id` (the reference `onFire` body,
/// `EventIntf.cpp:1132-1147`).
///
/// The action is taken out of the registry across the VM call, releasing the
/// state borrow so a re-entrant `trigger`/`cancel` sees consistent state, then
/// restored. A missing entry or an action already firing is a no-op (the
/// reference checks `if(obj.Object)`). The action receives the reference
/// `TVP_ACTION_INVOKE_*` event dictionary `{type: "onFire", target:
/// <objthis>}` (`EventIntf.h:208-237`); `objthis` may be null for the
/// poll's owner-less fallback, in which case `target` is omitted.
fn invoke_action(
    host: &Tjs2Engine,
    services: &CompatServices,
    id: AsyncTriggerId,
    objthis: *mut c_void,
) -> Result<(), AsyncTriggerError> {
    let (action, action_name) = {
        let mut state = services.state::<AsyncTriggerState>();
        match state.triggers.get_mut(&id) {
            Some(binding) => (binding.action.take(), binding.action_name.clone()),
            None => return Ok(()),
        }
    };
    let Some(action) = action else {
        return Ok(());
    };
    let event = action_event(host, objthis)?;
    let event_id = event.raw_id() as u64;
    let result = match action_name {
        Some(name) => host.call_member(action.raw_id(), &name, &[TjsValue::Retained(event_id)]),
        None => host.call_detached(&action, &[TjsValue::Retained(event_id)]),
    };
    // The call consumed the event retention (`value_to_variant`'s
    // `TJS2_VAL_RETAINED` path erases the map entry); `DetachedValue`'s drop
    // is the idempotent release. Restore the action unless the object was
    // destroyed meanwhile (in which case dropping the handle releases the
    // retention).
    drop(event);
    if let Some(binding) = services.state::<AsyncTriggerState>().triggers.get_mut(&id) {
        binding.action = Some(action);
    }
    result.map(|_| ()).map_err(AsyncTriggerError::Action)
}

/// Build the reference action event dictionary `{type, target}` for a native
/// `onFire` invocation (`TVPCreateEventObject`, `EventIntf.cpp:709-739`).
fn action_event(
    host: &Tjs2Engine,
    objthis: *mut c_void,
) -> Result<DetachedValue, AsyncTriggerError> {
    let event = match host.eval_retained("%[]", "AsyncTrigger.onFire") {
        Ok(RetainedValue::Object(event)) => event,
        Ok(RetainedValue::Value(_)) | Err(_) => {
            return Err(AsyncTriggerError::Event(
                "failed to create the action event dictionary".into(),
            ));
        }
    };
    if let Err(error) =
        host.set_member(event.raw_id(), "type", &TjsValue::String(EVENT_TYPE.into()))
    {
        return Err(AsyncTriggerError::Event(error));
    }
    if !objthis.is_null()
        && let Ok(target) = host.retain_object_detached(objthis)
    {
        // The retention is consumed when the variant copy is built; a failure
        // leaves it to `DetachedValue`'s idempotent release.
        let target_id = target.raw_id() as u64;
        let _ = host.set_member(event.raw_id(), "target", &TjsValue::Retained(target_id));
        drop(target);
    }
    Ok(event)
}

extern "C" fn async_trigger_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(AsyncTriggerInst::default())) as *mut c_void
}

extern "C" fn async_trigger_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` is the live `AsyncTriggerInst` payload the VM allocated for this script object; no `&mut` to it is live in `async_trigger_destroy`.
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    if let (Some(id), Ok((_host, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        let _ = services.state::<AsyncTriggerState>().remove(id);
    }
    // SAFETY: instance came from `async_trigger_create`'s `Box::into_raw` and
    // is freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut AsyncTriggerInst) });
}

/// `AsyncTrigger(action [, actionName])` — retain the action and register it.
extern "C" fn async_trigger_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    if argc < 1 {
        return report_error(
            out_error,
            "AsyncTrigger: constructor requires an action callback",
        );
    }
    let Ok((host, ctx)) = bound(engine) else {
        return report_error(out_error, "AsyncTrigger: compat context is not bound");
    };
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let callback = match host.retain_object_arg(&unsafe { args(argv, argc) }[0]) {
        Ok(callback) => callback,
        Err(error) => {
            return report_error(
                out_error,
                &format!("AsyncTrigger: cannot retain action: {error}"),
            );
        }
    };
    // The optional second argument selects the member to invoke; empty means
    // "invoke the object itself", omitted/void keeps the `TVPActionName`
    // default.
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let action_name = match unsafe { args(argv, argc) }.get(1) {
        Some(value) if value.ty != VAL_VOID => {
            let name = value_as_string(value);
            if name.is_empty() { None } else { Some(name) }
        }
        _ => Some(DEFAULT_ACTION_NAME.to_string()),
    };
    // `Owner = tjs_obj` (`reference/cpp/core/base/EventIntf.h:322`): the
    // object itself, held as a raw pointer with no reference so it can reach
    // reference count zero. The poll retains it on demand.
    let owner = objthis as usize;
    // SAFETY: `instance` is the live `AsyncTriggerInst` payload the VM allocated for this script object; `async_trigger_ctor` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut AsyncTriggerInst) };
    if inst.id.is_some() {
        return report_error(out_error, "AsyncTrigger: already constructed");
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let id = services
        .state::<AsyncTriggerState>()
        .insert(callback, action_name, owner);
    inst.id = Some(id);
    set_void(out);
    0
}

/// Reference `tTJSNI_AsyncTrigger::Invalidate` (`EventIntf.cpp:1032-1039`):
/// cancel the source events and drop `Owner` / `ActionOwner`. Removing the
/// registry entry does all three at once — it releases the retained action
/// and owner and drops every scheduled fire — and the later `destroy` removal
/// of the (already gone) entry is a safe no-op.
extern "C" fn async_trigger_invalidate(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` is the live `AsyncTriggerInst` payload the VM allocated for this script object; no `&mut` to it is live in `async_trigger_invalidate`.
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    if let (Some(id), Ok((_host, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        let _ = services.state::<AsyncTriggerState>().remove(id);
    }
}

/// `trigger()` — schedule the action for the next [`async_trigger_poll`].
extern "C" fn async_trigger_trigger(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "AsyncTrigger.trigger: compat context is not bound",
        );
    };
    // SAFETY: `instance` is the live `AsyncTriggerInst` payload the VM allocated for this script object; no `&mut` to it is live in `async_trigger_trigger`.
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    // The registry entry is this port's owner: an unconstructed (or
    // invalidated) object is a silent no-op, not an error.
    let Some(id) = inst.id else {
        set_void(out);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    // Retain the source object so it survives until the poll delivers the
    // fire, even when the script held the trigger only in a local.
    let source = services
        .state::<AsyncTriggerState>()
        .triggers
        .get(&id)
        .and_then(|binding| std::num::NonZeroUsize::new(binding.owner));
    let owner =
        source.and_then(
            |source| match host.retain_object_detached(source.get() as *mut c_void) {
                Ok(owner) => Some(owner),
                Err(error) => {
                    log::debug!("AsyncTrigger.trigger: cannot retain the source object: {error}");
                    None
                }
            },
        );
    // An invalidated object has no registry entry: the reference's
    // `Trigger()` guards on `Owner`, which `Invalidate` clears
    // (`EventIntf.cpp:1036-1037`, `1044`) — a silent no-op, not an error.
    if !services
        .state::<AsyncTriggerState>()
        .trigger(id, inst.cached, owner)
    {
        log::debug!("AsyncTrigger.trigger: {id:?} was invalidated; no-op");
    }
    set_void(out);
    0
}

/// `cancel()` — drop every scheduled fire for this object.
extern "C" fn async_trigger_cancel(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "AsyncTrigger.cancel: compat context is not bound",
        );
    };
    // SAFETY: `instance` is the live `AsyncTriggerInst` payload the VM allocated for this script object; no `&mut` to it is live in `async_trigger_cancel`.
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    // See `async_trigger_trigger`: no registry entry is a silent no-op.
    let Some(id) = inst.id else {
        set_void(out);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    // Reference `Cancel()` guards on `Owner` (`EventIntf.cpp:1064-1069`):
    // an invalidated object silently ignores `cancel()`.
    if !services.state::<AsyncTriggerState>().cancel(id) {
        log::debug!("AsyncTrigger.cancel: {id:?} was invalidated; no-op");
    }
    set_void(out);
    0
}

/// `onFire()` — invoke the retained action now (the reference event body).
extern "C" fn async_trigger_on_fire(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    // The reference event dictionary's `target` is `objthis`
    // (`TVP_ACTION_INVOKE_BEGIN(0, "onFire", objthis)`, `EventIntf.cpp:1141`).
    objthis: *mut c_void,
) -> c_int {
    let Ok((host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "AsyncTrigger.onFire: compat context is not bound",
        );
    };
    // SAFETY: `instance` is the live `AsyncTriggerInst` payload the VM allocated for this script object; no `&mut` to it is live in `async_trigger_on_fire`.
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    // See `async_trigger_trigger`: no registry entry (and therefore no
    // retained action) makes `onFire` a no-op.
    let Some(id) = inst.id else {
        set_void(out);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    // The reference native body always invokes the action (guarded only by
    // `if(obj.Object)`, `EventIntf.cpp:1134`); an invalidated object has no
    // retained action left, which `invoke_action` treats as the same no-op.
    if let Err(error) = invoke_action(host, services, id, objthis) {
        log::warn!("AsyncTrigger.onFire: {error}");
    }
    set_void(out);
    0
}

/// `cached` (reference `Cached`, defaults to true).
///
/// Read from the instance payload: the reference reads the native-instance
/// member even after `Invalidate` (`EventIntf.cpp:1032-1040` only clears
/// `Owner`/`ActionOwner`), so an invalidated-but-alive object still reports
/// what the script last wrote.
extern "C" fn async_trigger_cached_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if bound(engine).is_err() {
        return report_error(
            out_error,
            "AsyncTrigger.cached: compat context is not bound",
        );
    }
    // SAFETY: `instance` is the live `AsyncTriggerInst` payload the VM allocated for this script object; no `&mut` to it is live in `async_trigger_cached_get`.
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    set_bool(out, inst.cached);
    0
}

extern "C" fn async_trigger_cached_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "AsyncTrigger.cached: compat context is not bound",
        );
    };
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let cached = value_as_bool(unsafe { &*value });
    // SAFETY: `instance` is the live `AsyncTriggerInst` payload the VM allocated for this script object; `async_trigger_cached_set` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut AsyncTriggerInst) };
    // Only a change cancels, and cancelling without a registered owner is a
    // no-op, so the flag is written even before the constructor runs.
    let changed = inst.cached != cached;
    inst.cached = cached;
    if changed && let Some(id) = inst.id {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<AsyncTriggerState>()
            .cancel_pending(id);
    }
    0
}

/// `mode` (reference `tTVPAsyncTriggerMode`) — stored on the instance so it
/// survives `Invalidate`, like the reference's native-instance member.
extern "C" fn async_trigger_mode_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if bound(engine).is_err() {
        return report_error(out_error, "AsyncTrigger.mode: compat context is not bound");
    }
    // SAFETY: `instance` is the live `AsyncTriggerInst` payload the VM allocated for this script object; no `&mut` to it is live in `async_trigger_mode_get`.
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    set_int(out, inst.mode);
    0
}

extern "C" fn async_trigger_mode_set(
    engine: *mut c_void,
    instance: *mut c_void,
    value: *const Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(out_error, "AsyncTrigger.mode: compat context is not bound");
    };
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let mode = value_as_i64(unsafe { &*value });
    // SAFETY: `instance` is the live `AsyncTriggerInst` payload the VM allocated for this script object; `async_trigger_mode_set` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut AsyncTriggerInst) };
    // See `async_trigger_cached_set`: the mode is stored unconditionally and
    // only a change cancels, which is a no-op before the join.
    let changed = inst.mode != mode;
    inst.mode = mode;
    if changed && let Some(id) = inst.id {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        unsafe { ctx.services() }
            .state::<AsyncTriggerState>()
            .cancel_pending(id);
    }
    0
}

/// Fire every scheduled `AsyncTrigger` (`FrameBody::async_trigger_poll`).
///
/// The frame body calls this once per frame, after the media polls and before
/// the continuous handlers. Each scheduled object's `onFire`
/// member is dispatched, so a script subclass override runs; when the object
/// could not be retained the action is invoked directly. The pending list is
/// taken before any VM call, so a `trigger()` issued from inside a handler is
/// deferred to the next poll rather than re-entering the VM mid-dispatch.
pub fn async_trigger_poll(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let (host, services) = unsafe { (ctx.engine(), ctx.services()) };
    let ready = services.state::<AsyncTriggerState>().take_pending();
    for event in ready {
        let id = event.id;
        // The queued event's source retention keeps the trigger alive across
        // the call; the event entry is dropped after dispatch, releasing it.
        let result: Result<(), String> = match &event.owner {
            Some(owner) => host.call_member(owner.raw_id(), "onFire", &[]).map(|_| ()),
            // Reference-equivalent fallback for an event whose source could
            // not be retained: run the native `onFire` body, i.e. the action
            // with its event dictionary (no `target`, the object is
            // unavailable).
            None => invoke_action(host, services, id, std::ptr::null_mut())
                .map_err(|error| error.to_string()),
        };
        if let Err(error) = result {
            log::warn!("AsyncTrigger {id:?}: onFire failed ({error})");
        }
    }
    Ok(())
}

/// The number of live `AsyncTrigger` bindings in the dispatch state.
///
/// A host diagnostic: the trigger's destroy callback removes the binding, so a
/// count that grows across create/drop cycles is a retention leak.
pub(crate) fn trigger_count(services: &CompatServices) -> usize {
    services.state::<AsyncTriggerState>().triggers.len()
}

/// Register the `AsyncTrigger` native class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "AsyncTrigger",
        create: async_trigger_create,
        destroy: async_trigger_destroy,
        invalidate: Some(async_trigger_invalidate),
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
