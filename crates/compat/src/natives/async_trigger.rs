//! `AsyncTrigger` native class.
//!
//! Behavioral reference: `reference/cpp/core/base/EventIntf.cpp`
//! (`tTJSNI_AsyncTrigger` / `tTJSNC_AsyncTrigger`) and `main`'s
//! `crates/tvp-natives/src/async_trigger.rs`.
//!
//! `AsyncTrigger` is a **deferred-callback** class: `trigger()` only
//! *schedules* a handler, and the frame body fires it later at the
//! [`async_trigger_poll`] step (doc 08 §6). No VM call is made from inside
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
//!   `EventIntf.cpp:1001`). While a fire is scheduled, another `trigger()`
//!   coalesces it (one pending entry per object). With `cached = false` each
//!   `trigger()` schedules its own fire, like the reference event queue.
//!   Changing `cached` cancels the scheduled fires (`SetCached`).
//! * `mode` (`atmNormal` = 0, `atmExclusive` = 1, `atmAtIdle` = 2) is stored
//!   and delivered uniformly at the poll. Changing it cancels the scheduled
//!   fires (`SetMode`).
//! * `cancel()` drops every scheduled fire (`Cancel`).
//! * `onFire()` invokes the retained action; [`async_trigger_poll`] dispatches
//!   the object's `onFire` member, so a script subclass override wins.

use std::collections::HashMap;
use std::ffi::{c_char, c_int, c_void};

use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    Tjs2Engine, VAL_VOID, Value,
};

use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{resolve, value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError, CompatServices};

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
    /// The object itself, for `onFire` dispatch through its class chain so a
    /// script subclass override wins.
    owner: Option<DetachedValue>,
    /// Reference `Cached`: while set, one pending fire per object.
    cached: bool,
    /// Reference `tTVPAsyncTriggerMode` (`atmNormal`/`atmExclusive`/`atmAtIdle`).
    mode: i64,
}

/// The `AsyncTrigger` registry: one entry per live object plus the FIFO of
/// scheduled fires. Lives in [`CompatServices::state`] keyed by type.
#[derive(Default)]
struct AsyncTriggerState {
    /// Monotonic id source; never reused.
    next_id: u64,
    /// Per-object state keyed by [`AsyncTriggerId`].
    triggers: HashMap<AsyncTriggerId, AsyncTriggerBinding>,
    /// Scheduled fires in `trigger()` order. A `cached` object appears at
    /// most once.
    pending: Vec<AsyncTriggerId>,
}

/// Typed `AsyncTrigger` failures (D11: no silent fallbacks).
#[derive(Debug, thiserror::Error)]
enum AsyncTriggerError {
    /// The native instance was not constructed (no registry entry).
    #[error("AsyncTrigger is not constructed")]
    NotConstructed,
    /// No registry entry for the given handle.
    #[error("unknown AsyncTrigger {0:?}")]
    UnknownTrigger(AsyncTriggerId),
    /// The retained action raised while firing.
    #[error("AsyncTrigger action failed: {0}")]
    Action(String),
}

impl AsyncTriggerState {
    /// Register a constructed object's retained action and return its handle.
    fn insert(
        &mut self,
        action: DetachedValue,
        action_name: Option<String>,
        owner: Option<DetachedValue>,
    ) -> AsyncTriggerId {
        let id = AsyncTriggerId(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        self.triggers.insert(
            id,
            AsyncTriggerBinding {
                action: Some(action),
                action_name,
                owner,
                // Reference ctor: `Cached = true`, `Mode = atmNormal`.
                cached: true,
                mode: 0,
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

    /// The entry for `id`, mutably, or [`AsyncTriggerError::UnknownTrigger`].
    fn binding_mut(
        &mut self,
        id: AsyncTriggerId,
    ) -> Result<&mut AsyncTriggerBinding, AsyncTriggerError> {
        self.triggers
            .get_mut(&id)
            .ok_or(AsyncTriggerError::UnknownTrigger(id))
    }

    /// Remove `id` and drop every scheduled fire for it.
    fn remove(&mut self, id: AsyncTriggerId) -> Result<(), AsyncTriggerError> {
        self.binding(id)?;
        self.triggers.remove(&id);
        self.pending.retain(|pending| *pending != id);
        Ok(())
    }

    /// Schedule `id`'s `onFire`. A `cached` object coalesces while a fire is
    /// already pending; an uncached one queues each call.
    fn trigger(&mut self, id: AsyncTriggerId) -> Result<(), AsyncTriggerError> {
        let cached = self.binding(id)?.cached;
        if !(cached && self.pending.contains(&id)) {
            self.pending.push(id);
        }
        Ok(())
    }

    /// Drop every scheduled fire for `id` (reference `Cancel`).
    fn cancel(&mut self, id: AsyncTriggerId) -> Result<(), AsyncTriggerError> {
        self.binding(id)?;
        self.pending.retain(|pending| *pending != id);
        Ok(())
    }

    /// Set `cached`, cancelling scheduled fires on a change (reference
    /// `SetCached`).
    fn set_cached(&mut self, id: AsyncTriggerId, cached: bool) -> Result<(), AsyncTriggerError> {
        let changed = {
            let binding = self.binding_mut(id)?;
            let changed = binding.cached != cached;
            binding.cached = cached;
            changed
        };
        if changed {
            self.pending.retain(|pending| *pending != id);
        }
        Ok(())
    }

    /// Set `mode`, cancelling scheduled fires on a change (reference
    /// `SetMode`).
    fn set_mode(&mut self, id: AsyncTriggerId, mode: i64) -> Result<(), AsyncTriggerError> {
        let changed = {
            let binding = self.binding_mut(id)?;
            let changed = binding.mode != mode;
            binding.mode = mode;
            changed
        };
        if changed {
            self.pending.retain(|pending| *pending != id);
        }
        Ok(())
    }

    /// Take the fires scheduled for this poll, leaving the queue empty.
    ///
    /// `trigger()` calls made while a handler runs land in the emptied queue
    /// and are therefore deferred to the next poll instead of re-entering the
    /// VM mid-dispatch.
    fn take_pending(&mut self) -> Vec<AsyncTriggerId> {
        std::mem::take(&mut self.pending)
    }
}

/// The `AsyncTrigger` native instance payload: its registry handle.
#[derive(Default)]
struct AsyncTriggerInst {
    id: Option<AsyncTriggerId>,
}

/// Resolve the installed binding for a native callback.
fn bound(engine: *mut c_void) -> Result<(&'static Tjs2Engine, CompatCtx), String> {
    let resolved =
        resolve(engine).ok_or_else(|| "async_trigger: compat context is not bound".to_string())?;
    let ctx = resolved.ctx();
    Ok((resolved.engine(), ctx))
}

/// The registry handle of a constructed instance (D11 instead of a silent
/// default).
fn instance_id(inst: &AsyncTriggerInst) -> Result<AsyncTriggerId, AsyncTriggerError> {
    inst.id.ok_or(AsyncTriggerError::NotConstructed)
}

/// Invoke the action retained for `id` (the reference `onFire` body).
///
/// The action is taken out of the registry across the VM call, releasing the
/// state borrow so a re-entrant `trigger`/`cancel` sees consistent state, then
/// restored. A missing entry or an action already firing is a no-op.
fn invoke_action(
    host: &Tjs2Engine,
    services: &CompatServices,
    id: AsyncTriggerId,
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
    let result = match action_name {
        Some(name) => host.call_member(action.raw_id(), &name, &[]),
        None => host.call_detached(&action, &[]),
    };
    // Restore the action unless the object was destroyed meanwhile (in which
    // case dropping the handle releases the retention).
    if let Some(binding) = services.state::<AsyncTriggerState>().triggers.get_mut(&id) {
        binding.action = Some(action);
    }
    result.map(|_| ()).map_err(AsyncTriggerError::Action)
}

extern "C" fn async_trigger_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(AsyncTriggerInst::default())) as *mut c_void
}

extern "C" fn async_trigger_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
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
    let action_name = match unsafe { args(argv, argc) }.get(1) {
        Some(value) if value.ty != VAL_VOID => {
            let name = value_as_string(value);
            if name.is_empty() { None } else { Some(name) }
        }
        _ => Some(DEFAULT_ACTION_NAME.to_string()),
    };
    // The object retains itself so the poll can dispatch its `onFire` member.
    let owner = host.retain_object_detached(objthis).ok();
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
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "AsyncTrigger.trigger: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    let id = match instance_id(inst) {
        Ok(id) => id,
        Err(error) => return report_error(out_error, &format!("AsyncTrigger.trigger: {error}")),
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    if let Err(error) = services.state::<AsyncTriggerState>().trigger(id) {
        return report_error(out_error, &format!("AsyncTrigger.trigger: {error}"));
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
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    let id = match instance_id(inst) {
        Ok(id) => id,
        Err(error) => return report_error(out_error, &format!("AsyncTrigger.cancel: {error}")),
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    if let Err(error) = services.state::<AsyncTriggerState>().cancel(id) {
        return report_error(out_error, &format!("AsyncTrigger.cancel: {error}"));
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
    _objthis: *mut c_void,
) -> c_int {
    let Ok((host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "AsyncTrigger.onFire: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    let id = match instance_id(inst) {
        Ok(id) => id,
        Err(error) => return report_error(out_error, &format!("AsyncTrigger.onFire: {error}")),
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    if let Err(error) = invoke_action(host, services, id) {
        log::warn!("AsyncTrigger.onFire: {error}");
    }
    set_void(out);
    0
}

/// `cached` (reference `Cached`, defaults to true).
extern "C" fn async_trigger_cached_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(
            out_error,
            "AsyncTrigger.cached: compat context is not bound",
        );
    };
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let cached = inst
        .id
        .and_then(|id| {
            services
                .state::<AsyncTriggerState>()
                .binding(id)
                .ok()
                .map(|b| b.cached)
        })
        .unwrap_or(true);
    set_bool(out, cached);
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
    let cached = value_as_bool(unsafe { &*value });
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    let id = match instance_id(inst) {
        Ok(id) => id,
        Err(error) => return report_error(out_error, &format!("AsyncTrigger.cached: {error}")),
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    if let Err(error) = services.state::<AsyncTriggerState>().set_cached(id, cached) {
        return report_error(out_error, &format!("AsyncTrigger.cached: {error}"));
    }
    0
}

/// `mode` (reference `tTVPAsyncTriggerMode`).
extern "C" fn async_trigger_mode_get(
    engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Ok((_host, ctx)) = bound(engine) else {
        return report_error(out_error, "AsyncTrigger.mode: compat context is not bound");
    };
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let mode = inst
        .id
        .and_then(|id| {
            services
                .state::<AsyncTriggerState>()
                .binding(id)
                .ok()
                .map(|b| b.mode)
        })
        .unwrap_or(0);
    set_int(out, mode);
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
    let mode = value_as_i64(unsafe { &*value });
    let inst = unsafe { &*(instance as *const AsyncTriggerInst) };
    let id = match instance_id(inst) {
        Ok(id) => id,
        Err(error) => return report_error(out_error, &format!("AsyncTrigger.mode: {error}")),
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    if let Err(error) = services.state::<AsyncTriggerState>().set_mode(id, mode) {
        return report_error(out_error, &format!("AsyncTrigger.mode: {error}"));
    }
    0
}

/// Fire every scheduled `AsyncTrigger` (`FrameBody::async_trigger_poll`).
///
/// The frame body calls this once per frame, after the media polls and before
/// the continuous handlers (doc 08 §6). Each scheduled object's `onFire`
/// member is dispatched, so a script subclass override runs; when the object
/// could not be retained the action is invoked directly. The pending list is
/// taken before any VM call, so a `trigger()` issued from inside a handler is
/// deferred to the next poll rather than re-entering the VM mid-dispatch.
pub fn async_trigger_poll(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let (host, services) = unsafe { (ctx.engine(), ctx.services()) };
    let ready = services.state::<AsyncTriggerState>().take_pending();
    for id in ready {
        // Read the owner without holding the state borrow across the call.
        let owner = services
            .state::<AsyncTriggerState>()
            .triggers
            .get(&id)
            .and_then(|binding| binding.owner.as_ref().map(DetachedValue::raw_id));
        let result: Result<(), String> = match owner {
            Some(owner) => host.call_member(owner, "onFire", &[]).map(|_| ()),
            None => invoke_action(host, services, id).map_err(|error| error.to_string()),
        };
        if let Err(error) = result {
            log::warn!("AsyncTrigger {id:?}: onFire failed ({error})");
        }
    }
    Ok(())
}

/// Register the `AsyncTrigger` native class.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
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
