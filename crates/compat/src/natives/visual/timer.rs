//! The visual `Timer` native class and its frame poll.
//!
//! Behavioral reference: `reference/cpp/core/utils/TimerIntf.cpp` and
//! `reference/cpp/core/utils/impl/TimerImpl.cpp`.
//!
//! The L3 glue owns exactly what `tTJSNI_BaseTimer` owns: the retained
//! action owner / action name, the script-visible `interval` (sub-ms fixed
//! point), `capacity`, `mode` and `enabled` — all stored verbatim in the
//! [`TimerBinding`] and never read back from L2 — plus the dispatch seam.
//! Scheduling is L2 (`engine_api::TimerState`): the constructor registers
//! a callback that performs the `onTimer` dispatch on every fire, and L3
//! decides whether the timer is armed *at all*. The reference's two "armed
//! and enabled yet never fires" cases — a negative `Capacity` posting
//! nothing (`TimerIntf.cpp:76-84`) and a zero interval being skipped by the
//! timer thread (`TimerImpl.cpp:125`) — are expressed by **not arming** the
//! L2 timer while the binding keeps reporting the script's `enabled`.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use crate::engine_api::{TimerCallback, TimerId, TimerState};
use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, TjsValue, VAL_INTEGER, VAL_REAL, VAL_STRING, VAL_VOID, Value,
};

use super::common::{DEFAULT_TIMER_CAPACITY, TimerBinding, VisualState, bound, set_real};
use crate::collections::Map;
use crate::native_util::{args, report_error, set_bool, set_int, set_void, value_as_string};
use crate::natives::system::{value_as_bool, value_as_i64};
use crate::{CompatCtx, CompatError, CompatServices};

/// The reference `TVPActionName` — the default action member
/// (`EventIntf.cpp:740`, stored at `TimerIntf.cpp:34`).
const DEFAULT_ACTION_NAME: &str = "action";

/// `TVP_SUBMILLI_FRAC_BITS` (`TimerIntf.h:18`): `interval` is stored as
/// integer sub-millisecond fixed point. The reference setter scales the
/// script value by `1 << 16` (`TimerIntf.cpp:196-197`) and the getter
/// converts back with `1.0 / (1 << 16)` (`TimerIntf.cpp:186-188`).
const SUBMILLI_FRAC_BITS: u32 = 16;

/// `1 << TVP_SUBMILLI_FRAC_BITS` as the setter/getter scale factor. A power
/// of two, so both directions are exact in `f64`.
const SUBMILLI_SCALE: f64 = 65536.0;

/// The script-visible default `interval`: the reference constructor default
/// of 1000 ms (`TimerImpl.cpp:363`) held in the same sub-ms fixed-point
/// storage the setter writes, so the getter's reference conversion
/// (`TimerIntf.cpp:186-188`) reports `1000.0` — matching
/// [`DEFAULT_INTERVAL_MS`].
const DEFAULT_INTERVAL_UNITS: u64 = 1000 << SUBMILLI_FRAC_BITS;

/// The L2 counterpart of [`DEFAULT_INTERVAL_UNITS`]: the reference default
/// interval of 1000 ms (`TimerImpl.cpp:363`) in whole milliseconds — the
/// interval the constructor registers its callback with.
const DEFAULT_INTERVAL_MS: u64 = 1000;

/// `Timer` payload: the L2 timer registry id.
#[derive(Default)]
struct TimerInst {
    id: Option<TimerId>,
}

/// Deferred `onTimer` deliveries for the current poll.
///
/// The reference `tTJSNI_BaseTimer::Fire` does not call the action: it
/// **posts** `onTimer` to the owner (`TimerIntf.cpp:100`) with the mode's
/// priority flag (`TimerIntf.cpp:86-98`) and the event loop delivers the
/// queued events later, exclusive first, then normal, then idle
/// (`EventIntf.cpp:426-519`). This is the L3 half of that model: the L2
/// callback enqueues one entry per fire, and [`timer_posts_poll`] drains the
/// queue after the L2 registry advanced, ordering by the mode captured at
/// fire time.
///
/// A schedule write while the queue is draining cancels the timer's pending
/// entries the way the reference's `CancelEvents` does, tracked by the
/// per-timer generation counter: [`post_on_timer`] stamps each entry with the
/// current generation and [`sync_schedule`] bumps it for the writes the
/// reference cancels on (`SetEnabled(false)`, `SetInterval`), so the drain
/// skips a cancelled entry.
#[derive(Default)]
struct TimerPosts {
    /// Posts in fire order; [`timer_posts_poll`] sorts a copy by priority
    /// before delivery.
    pending: Vec<TimerPost>,
    /// Cancellation generation per timer. An absent entry is generation 0.
    generation: Map<TimerId, u64>,
}

/// One queued `onTimer` posting: the timer, the mode captured at fire time,
/// and the schedule generation it was posted under.
struct TimerPost {
    id: TimerId,
    mode: i64,
    generation: u64,
}

/// The delivery priority of a `tTVPAsyncTriggerMode` (`EventIntf.h:312`):
/// exclusive events are delivered before normal events, which are delivered
/// before idle events (`EventIntf.cpp:426-519`).
fn mode_priority(mode: i64) -> u8 {
    match mode {
        // atmExclusive
        1 => 0,
        // atmAtIdle
        2 => 2,
        // atmNormal (0) and anything unrecognized
        _ => 1,
    }
}

/// Queue the `onTimer` postings for the fires of `id` a delivery replays.
///
/// The effective capacity is 65535 for the no-limit sentinel (`0`) and the
/// binding's capacity otherwise; a non-positive capacity posts nothing. The
/// engine hands this function the whole replay count in one call, so it clamps
/// once and queues at most the room the capacity leaves: a backlog that does
/// not fit is dropped. The clamp is O(1) in the replay length — a process
/// suspended for an hour queues at most `cap` postings, not one per interval.
/// A negative capacity is already expressed by not arming the engine timer
/// (`sync_schedule`).
fn post_on_timer(services: &CompatServices, id: TimerId, fires: u64) {
    let mut posts = services.state::<TimerPosts>();
    let (mode, capacity) = {
        let state = services.state::<VisualState>();
        let Some(binding) = state.timers.get(&id) else {
            return;
        };
        (binding.mode, binding.capacity)
    };
    // `TimerIntf.cpp:76-77`: `0` is the "no limit" sentinel.
    let cap = if capacity == 0 { 65535 } else { capacity };
    if cap <= 0 {
        return;
    }
    // `cap` is positive here, so its magnitude is its value and the
    // conversion is total for every capacity that reaches this point.
    let cap = cap.unsigned_abs();
    let queued = posts.pending.iter().filter(|post| post.id == id).count() as u64;
    let count = fires.min(cap.saturating_sub(queued));
    if count == 0 {
        return;
    }
    let generation = posts.generation.get(&id).copied().unwrap_or(0);
    for _ in 0..count {
        posts.pending.push(TimerPost {
            id,
            mode,
            generation,
        });
    }
}

/// Bump `id`'s cancellation generation, invalidating every entry already
/// queued for it (the reference `CancelEvents`, `TimerIntf.cpp:62-67`).
fn cancel_pending(services: &CompatServices, id: TimerId) {
    let mut posts = services.state::<TimerPosts>();
    let entry = posts.generation.entry(id).or_insert(0);
    *entry = entry.wrapping_add(1);
}

/// Drop all queue state for a torn-down timer, so a long session does not
/// accumulate generation entries for ids that can never be reused.
fn forget_posts(services: &CompatServices, id: TimerId) {
    let mut posts = services.state::<TimerPosts>();
    posts.pending.retain(|post| post.id != id);
    posts.generation.remove(&id);
}

/// Whether a queued post may still be delivered: its timer has not been
/// invalidated and no schedule write cancelled it since it was posted.
fn timer_post_is_live(services: &CompatServices, id: TimerId, generation: u64) -> bool {
    if !services.state::<VisualState>().timers.contains_key(&id) {
        return false;
    }
    services
        .state::<TimerPosts>()
        .generation
        .get(&id)
        .copied()
        .unwrap_or(0)
        == generation
}

// ---------------------------------------------------------------------------
// Timer (L3 adapter over the L2 `engine_api::TimerState` registry)
// ---------------------------------------------------------------------------

/// The number of live `Timer` bindings in the dispatch state.
///
/// A host diagnostic: `timer_destroy` removes the binding, so a count that
/// grows across create/drop cycles is a retention leak.
pub(crate) fn binding_count(services: &CompatServices) -> usize {
    services.state::<VisualState>().timers.len()
}

extern "C" fn timer_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(TimerInst::default())) as *mut c_void
}

extern "C" fn timer_destroy(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; no `&mut` to it is live in `timer_destroy`.
    let inst = unsafe { &*(instance as *const TimerInst) };
    log::trace!("Timer destroyed: {:?}", inst.id);
    if let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) {
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        if let Some(timer) = services.timer() {
            // SAFETY: `attach_timer` guarantees the pointer stays valid.
            let _ = unsafe { &mut *timer }.remove(id);
        }
        // The dropped `TimerBinding` releases the action owner and the
        // retained timer object.
        services.state::<VisualState>().timers.remove(&id);
        forget_posts(services, id);
    }
    // SAFETY: instance came from `timer_create`'s `Box::into_raw` and is
    // freed exactly once by the VM.
    drop(unsafe { Box::from_raw(instance as *mut TimerInst) });
}

/// `Timer(actionOwner[, actionName])` — retain the action owner and register
/// the L2 timer.
///
/// Reference `tTJSNI_BaseTimer::Construct` (`TimerIntf.cpp:34-51`):
/// * `numparams < 1` raises `TJS_E_BADPARAMCOUNT` (`TimerIntf.cpp:41-42`) —
///   a missing argument is an error, our message text may differ;
/// * a second argument replaces the `TVPActionName == "action"` default only
///   when it is present and not void (`TimerIntf.cpp:48-49`); `""` selects
///   the "call the owner itself" dispatch;
/// * `param[0]` becomes the action owner *unconditionally*
///   (`param[0]->AsObjectClosure()`, `TimerIntf.cpp:51`) — a non-object
///   (e.g. `new Timer(42)`) yields an empty closure that is silently never
///   invoked, so it must not raise here.
///
/// The L2 timer starts at the reference defaults (`TimerImpl.cpp:361-366`):
/// interval 1000 ms, **disabled** — `create_interval` arms by contract, so
/// the constructor disarms it again and only a script `enabled = true`
/// arms it for real. `Capacity` is kept in the binding only: it bounds
/// *queued* `onTimer` events (`TimerIntf.cpp:69-92`), never fires, so it
/// never narrows the schedule.
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
        return report_error(out_error, "Timer: constructor requires an action owner");
    }
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer: compat context is not bound");
    };
    let host = resolved.engine();
    // SAFETY: the C++ trampoline passes `argc` valid `Value`s in `argv` for the duration of this callback.
    let argv = unsafe { args(argv, argc) };
    let Some(first) = argv.first() else {
        return report_error(out_error, "Timer: constructor requires an action owner");
    };
    // Reference `ActionOwner = param[0]->AsObjectClosure()`
    // (`TimerIntf.cpp:51`): any value is accepted; a value that is not an
    // object becomes an empty closure the native `onTimer` never invokes
    // (`if(obj.Object)`, `TimerIntf.cpp:161`), so a failed retain stores
    // `None` instead of raising `retain_object_arg: value is not an object`.
    let action_owner = host.retain_object_arg(first).ok();
    // Reference `if(numparams >= 2 && param[1]->Type() != tvtVoid)
    // ActionName = *param[1]` (`TimerIntf.cpp:48-49`).
    let action_name = match argv.get(1) {
        Some(value) if value.ty != VAL_VOID => value_as_string(value),
        _ => DEFAULT_ACTION_NAME.to_string(),
    };
    // `Owner = tjs_obj` (`TimerIntf.cpp:52`): the timer object itself, used
    // to dispatch `onTimer` through the class chain. The reference stores a
    // raw pointer with no reference (`tTJSNI_BaseTimer::Owner`), so the
    // handle is not retained here.
    let owner = objthis as usize;
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; `timer_ctor` is its only live accessor.
    let inst = unsafe { &mut *(instance as *mut TimerInst) };
    if inst.id.is_some() {
        return report_error(out_error, "Timer: already constructed");
    }
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let Some(timer_ptr) = services.timer() else {
        return report_error(out_error, "Timer: timer registry is not attached");
    };
    // The reference constructs the timer disabled with a 1000 ms interval
    // (`TimerImpl.cpp:361-366`). `create_interval` arms by contract, so the
    // fresh timer is disarmed immediately; the binding starts `enabled =
    // false`, and only a script `enabled = true` arms it (through the
    // schedule sync).
    let id = {
        // SAFETY: `attach_timer` guarantees the pointer stays valid.
        let timer = unsafe { &mut *timer_ptr };
        let id = match timer.create_interval(DEFAULT_INTERVAL_MS, timer_callback(engine)) {
            Ok(id) => id,
            Err(error) => return report_error(out_error, &format!("Timer: {error}")),
        };
        if let Err(error) = timer.set_enabled(id, false) {
            return report_error(out_error, &format!("Timer: {error}"));
        }
        id
    };
    services.state::<VisualState>().timers.insert(
        id,
        TimerBinding {
            action_owner,
            owner,
            action_name,
            mode: 0,
            capacity: DEFAULT_TIMER_CAPACITY,
            interval_units: DEFAULT_INTERVAL_UNITS,
            enabled: false,
            armed: false,
        },
    );
    inst.id = Some(id);
    set_void(out);
    0
}

/// Reference `tTJSNI_Timer::Invalidate` (`TimerImpl.cpp:378-382`) plus
/// `tTJSNI_BaseTimer::Invalidate` (`TimerIntf.cpp:53-60`): remove the timer
/// from the thread, cancel the queued events and drop `Owner` /
/// `ActionOwner`.
///
/// Our dispatch holds the subclass instance (`owner`) and the action owner
/// as strong retentions, so removing the registry entry does all three at
/// once: the entry's drop releases both retentions, the L2 timer is
/// unregistered so it never ticks again, and an action still running on the
/// stack finds no entry to be restored into after `invalidate` returns (no
/// resurrection). The native payload itself stays allocated and is freed
/// exactly once by `timer_destroy` when the object dies (the ABI runs
/// `Invalidate` before the destructor).
extern "C" fn timer_invalidate(engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; no `&mut` to it is live in `timer_invalidate`.
    let inst = unsafe { &*(instance as *const TimerInst) };
    let (Some(id), Ok((_resolved, ctx))) = (inst.id, bound(engine)) else {
        return;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    // Reference `CancelEvents()` + `Owner`/`ActionOwner` release: the entry's
    // drop releases both retentions, so `timer_poll` has nothing to dispatch.
    services.state::<VisualState>().timers.remove(&id);
    forget_posts(services, id);
    // Reference `tTVPTimerThread::Remove(this)`: the timer must not tick
    // again, and `timer_poll` must stop reporting an armed timer.
    if let Some(timer) = services.timer() {
        // SAFETY: `attach_timer` guarantees the pointer stays valid.
        let _ = unsafe { &mut *timer }.remove(id);
    }
}

/// Build the L2 callback that performs the reference-faithful `onTimer`
/// dispatch for a timer registered by [`timer_ctor`].
///
/// The closure captures only the engine pointer the ABI handed the
/// constructor: the binding key is the [`TimerId`] it receives on every
/// delivery, the replay count is how many `onTimer` postings that delivery
/// stands for, and the retained owner / action live in the [`TimerBinding`],
/// resolved through `bound(engine)` at fire time — so a timer invalidated
/// or destroyed after collection finds no binding and dispatches nothing.
fn timer_callback(engine: *mut c_void) -> TimerCallback {
    Box::new(move |id, fires| {
        // SAFETY: `engine` is the live pointer the native ABI passed to this
        // timer's constructor; L2 only invokes callbacks from the frame's
        // timer delivery, which requires the same bound context, so `bound`
        // resolves it the same way every other native does.
        let Ok((_resolved, ctx)) = bound(engine) else {
            log::warn!("Timer {id:?}: compat context is not bound; fire dropped");
            return;
        };
        // SAFETY: the caller resolved `ctx` from the installed binding.
        let services = unsafe { ctx.services() };
        post_on_timer(services, id, fires);
    })
}

/// Dispatch `id`'s `onTimer` — always the object's `onTimer` **member**
/// (`EventIntf.cpp:104`: `Target->FuncCall(0, "onTimer", ...)`), so a script
/// subclass override wins; the native member then invokes the constructor's
/// action owner (see [`invoke_action`]).
///
/// Delivery failures are logged and never disable the timer — the reference
/// has no disable-on-error path in `TimerIntf.cpp`/`TimerImpl.cpp`.
fn dispatch_on_timer(host: &Tjs2Engine, services: &CompatServices, id: TimerId) {
    let handle = {
        let state = services.state::<VisualState>();
        state.timers.get(&id).map(|timer| timer.owner).unwrap_or(0)
    };
    // The reference `Owner` is a raw pointer (`TimerIntf.cpp:52`); retain it
    // for the duration of the dispatch so the object cannot finalize under
    // the call, then release it. A zero handle is the null pointer, which
    // `retain_object_detached` rejects, yielding `None`.
    let owner = host.retain_object_detached(handle as *mut c_void).ok();
    log::trace!("timer {id:?}: dispatching (owner {})", owner.is_some());
    let result: Result<(), String> = match &owner {
        Some(owner) => host.call_member(owner.raw_id(), "onTimer", &[]).map(|_| ()),
        // The reference's `Owner` is always the timer object
        // (`TimerIntf.cpp:52`); a missing handle is a port-only failure,
        // and delivering without it would omit the event's `target`
        // (`TVP_ACTION_INVOKE_BEGIN(0, "onTimer", objthis)`,
        // `TimerIntf.cpp:164`). Skip rather than deliver a malformed event.
        None => {
            log::debug!("Timer {id:?}: no timer object handle; onTimer fire skipped");
            return;
        }
    };
    if let Err(error) = result {
        // Reference: `onTimer` returns `TJS_S_OK` whatever happened
        // (`TimerIntf.cpp:169`) and no reference code path disables a timer
        // because its callback failed — keep delivering.
        log::warn!("Timer {id:?}: onTimer dispatch failed ({error}); the timer stays enabled");
    }
}

/// Build the reference action event dictionary `{type: "onTimer", target:
/// <objthis>}` for a native `onTimer` invocation (`TVPCreateEventObject`,
/// `EventIntf.cpp:709-739`, built by `TVP_ACTION_INVOKE_BEGIN`,
/// `EventIntf.h:208-215`).
///
/// `objthis` may be null when the timer object itself could not be
/// retained (a port-only situation; the reference always has it), in which
/// case `target` is omitted.
fn action_event(host: &Tjs2Engine, objthis: *mut c_void) -> Result<DetachedValue, String> {
    let event = match host.eval_retained("%[]", "Timer.onTimer") {
        Ok(RetainedValue::Object(event)) => event,
        Ok(RetainedValue::Value(value)) => {
            return Err(format!("event dictionary came back as {value:?}"));
        }
        Err(error) => return Err(format!("event dictionary creation failed: {error}")),
    };
    host.set_member(event.raw_id(), "type", &TjsValue::String("onTimer".into()))?;
    if !objthis.is_null() {
        // The retention is consumed while the variant copy is built; on
        // failure it is left to `DetachedValue`'s idempotent release.
        if let Ok(target) = host.retain_object_detached(objthis) {
            let target_id = target.raw_id().as_ptr() as u64;
            let _ = host.set_member(event.raw_id(), "target", &TjsValue::Retained(target_id));
            drop(target);
        }
    }
    Ok(event)
}

/// Invoke the retained action for `id` exactly like the reference native
/// `Timer.onTimer` (`TimerIntf.cpp:156-171` with `TVP_ACTION_INVOKE_*`,
/// `EventIntf.h:208-237`):
///
/// * a non-empty action name is a **member call on the action owner** —
///   `owner.<name>(event)` — defaulting to `"action"`
///   (`TVPActionName`, `EventIntf.cpp:740`);
/// * the empty name calls the owner itself — `owner(event)`
///   (`TimerIntf.cpp:165` passes a null member name);
/// * both receive the `{type: "onTimer", target: <timer>}` event dictionary
///   as their single argument.
///
/// The action is taken out of the registry across the VM call (releasing
/// the state borrow so a re-entrant poll/construct sees consistent state)
/// and restored afterwards unless the object was invalidated or destroyed
/// meanwhile (the entry's drop then releases it). A missing entry and a
/// non-object constructor argument (`None` action owner — the reference's
/// `if(obj.Object)` guard at `TimerIntf.cpp:161`) are silent no-ops.
///
/// Dispatch failures are returned to the caller *for logging only*: the
/// reference never inspects the `FuncCall` result (`EventIntf.h:231-234`),
/// so a failed member lookup — a plain function has no `action` member, and
/// the VM's `MEMBERNOTFOUND` is discarded — or a throwing callback must
/// neither raise out of the delivery seam nor disable the timer. Nothing in
/// `TimerIntf.cpp`/`TimerImpl.cpp` links a delivery error to `SetEnabled`.
fn invoke_action(
    host: &Tjs2Engine,
    services: &CompatServices,
    id: TimerId,
    objthis: *mut c_void,
) -> Result<(), String> {
    let (action, action_name) = {
        let mut state = services.state::<VisualState>();
        match state.timers.get_mut(&id) {
            Some(binding) => (binding.action_owner.take(), binding.action_name.clone()),
            None => return Ok(()),
        }
    };
    let Some(action) = action else {
        // Reference `if(obj.Object)` (`TimerIntf.cpp:161`): an empty
        // `AsObjectClosure()` (a non-object ctor argument) never invokes.
        return Ok(());
    };
    let restore = |action: DetachedValue| {
        if let Some(binding) = services.state::<VisualState>().timers.get_mut(&id) {
            binding.action_owner = Some(action);
        } else {
            drop(action);
        }
    };
    let event = match action_event(host, objthis) {
        Ok(event) => event,
        Err(error) => {
            restore(action);
            return Err(error);
        }
    };
    // The event dictionary crosses the call as `TjsValue::Retained`; the
    // C++ side consumes that retention while copying the argument, so
    // dropping `event` afterwards is an idempotent no-op.
    let event_id = event.raw_id().as_ptr() as u64;
    let result = if action_name.is_empty() {
        // Empty action name: `FuncCall(0, nullptr, ...)` invokes the owner
        // itself — `owner(event)` (`TimerIntf.cpp:165`).
        host.call_detached(&action, &[TjsValue::Retained(event_id)])
    } else {
        // Non-empty name: a member call on the owner — `owner.<name>(event)`.
        host.call_member(
            action.raw_id(),
            &action_name,
            &[TjsValue::Retained(event_id)],
        )
    };
    drop(event);
    restore(action);
    result.map(|_| ())
}

/// The native `onTimer` method: dispatch the constructor's action owner the
/// way `tTJSNC_Timer::onTimer` does (`TimerIntf.cpp:156-171`). A script
/// subclass that overrides `onTimer` never reaches here (the override wins
/// the class-chain lookup in [`dispatch_on_timer`]).
///
/// The reference returns `TJS_S_OK` unconditionally (`TimerIntf.cpp:169`) —
/// the `FuncCall` result is discarded — so this never raises and never
/// disables the timer; dispatch failures are logged.
extern "C" fn timer_on_timer(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Ok((resolved, ctx)) = bound(engine) else {
        return report_error(out_error, "Timer.onTimer: compat context is not bound");
    };
    let host = resolved.engine();
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; no `&mut` to it is live in `timer_on_timer`.
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        // `Construct` never completed (`TimerIntf.cpp:34-51`), so the
        // reference's `ActionOwner` is null and `onTimer` is a no-op that
        // still returns `TJS_S_OK`.
        log::trace!("Timer.onTimer: the timer is not constructed; nothing to dispatch");
        set_void(out);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    if let Err(error) = invoke_action(host, services, id, objthis) {
        log::warn!("Timer {id:?}: action dispatch failed ({error}); the timer stays enabled");
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Schedule sync: the binding decides, L2 executes
// ---------------------------------------------------------------------------

/// The binding's interval in whole L2 milliseconds, rounded **up** so the
/// L2 deadline is never shorter than the reference's, or `None` when the
/// fixed-point value is not strictly positive as a `tjs_int64`.
///
/// `0` and wrapped negatives are not representable in L2 (the reference
/// thread skips a zero interval outright, `TimerImpl.cpp:125`, and a
/// wrapped value would be an ~8775-year interval): in both cases L3 keeps
/// the timer *unarmed* while the binding keeps reporting the script's
/// values verbatim.
fn armable_interval_ms(interval_units: u64) -> Option<u64> {
    let signed = interval_units as i64;
    (signed > 0).then(|| interval_ms_ceil(interval_units))
}

/// `ceil(units / 65536)` — the sub-ms fixed point rounded up to ms.
fn interval_ms_ceil(interval_units: u64) -> u64 {
    interval_units.div_ceil(1 << SUBMILLI_FRAC_BITS)
}

/// Push `interval_ms` into L2 (no-op when the binding's value is not
/// representable, i.e. `None`).
fn push_interval(timer: &mut TimerState, id: TimerId, interval_ms: Option<u64>) {
    let Some(interval_ms) = interval_ms else {
        return;
    };
    if let Err(error) = timer.set_interval(id, interval_ms) {
        log::warn!("Timer {id:?}: set_interval({interval_ms}) failed ({error})");
    }
}

/// Arm or disarm `id` in L2, logging a (cannot-happen) typed error instead
/// of unwinding out of a property setter.
fn push_enabled(timer: &mut TimerState, id: TimerId, enabled: bool) {
    if let Err(error) = timer.set_enabled(id, enabled) {
        log::warn!("Timer {id:?}: set_enabled({enabled}) failed ({error})");
    }
}

/// Reconcile the L2 schedule with `id`'s binding after a script write.
///
/// * `want` is whether the reference-equivalent state calls for an armed L2
///   timer: `enabled && capacity >= 0 && interval > 0`. The reference's two
///   never-post cases — a negative `Capacity` (`TimerIntf.cpp:76-84`) and a
///   non-positive interval (`TimerImpl.cpp:125`) — keep the timer *unarmed*
///   in L2 while the binding still reports `enabled = true`; writing a
///   valid value arms it for real.
/// * A transition into armed injects `now` and pushes the binding's current
///   interval first, so the first fire is one interval after the write —
///   the reference's `NextTick = GetTickCount() + Interval`
///   (`TimerImpl.cpp:274-276`).
/// * A transition out just drops the L2 deadline.
/// * `rearm_interval` (set by the `interval` setter) additionally re-arms an
///   already-armed timer from `now`, like `tTVPTimerThread::SetInterval`
///   (`TimerImpl.cpp:294-301`) — cancelling that round's pending fire. A
///   capacity or enabled write that changes nothing leaves a pending fire
///   untouched (`SetCapacity` only stores).
fn sync_schedule(
    services: &CompatServices,
    id: TimerId,
    now: u64,
    rearm_interval: bool,
    cancel_events: bool,
) {
    let (want, interval_ms, was_armed) = {
        let state = services.state::<VisualState>();
        let Some(binding) = state.timers.get(&id) else {
            // The binding vanished re-entrantly (teardown during the write):
            // there is nothing left to schedule.
            return;
        };
        let interval_ms = armable_interval_ms(binding.interval_units);
        let want = binding.enabled && binding.capacity >= 0 && interval_ms.is_some();
        (want, interval_ms, binding.armed)
    };
    // The reference cancels a timer's queued events on `SetEnabled(false)`
    // (`tTVPTimerThread::SetEnabled`, `TimerImpl.cpp:277-281`) and on
    // `SetInterval` while enabled (`TimerImpl.cpp:294-301`); `SetCapacity`
    // only stores (`TimerIntf.cpp:234-236`). `cancel_events` carries that
    // per-write distinction; bump the delivery generation so any queued post
    // is dropped.
    if cancel_events {
        cancel_pending(services, id);
    }
    let Some(timer_ptr) = services.timer() else {
        log::debug!("Timer {id:?}: no timer registry is attached; the schedule write is dropped");
        return;
    };
    // SAFETY: `attach_timer` guarantees the pointer stays valid. The block
    // below never invokes a callback (no script runs here), so no
    // `VisualState` borrow is held across it either.
    let timer = unsafe { &mut *timer_ptr };
    match (was_armed, want) {
        (false, true) => {
            // Arm for real: inject the write's timestamp, push the binding's
            // interval (L2 may never have seen the current value), then arm.
            timer.now_ms = now;
            push_interval(timer, id, interval_ms);
            push_enabled(timer, id, true);
        }
        (true, false) => push_enabled(timer, id, false),
        // Staying armed: only an `interval` write re-arms from `now`.
        (true, true) if rearm_interval => {
            timer.now_ms = now;
            push_interval(timer, id, interval_ms);
        }
        // Staying disarmed: the binding's interval is pushed at the next
        // arming, so L2 needs nothing now.
        _ => {}
    }
    if was_armed != want {
        let mut state = services.state::<VisualState>();
        if let Some(binding) = state.timers.get_mut(&id) {
            binding.armed = want;
        }
    }
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// `timer.interval` — the reference fixed-point readback converted to a
/// double (`TimerIntf.cpp:186-188`): always a TJS `Real`.
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
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; no `&mut` to it is live in `timer_interval_get`.
    let inst = unsafe { &*(instance as *const TimerInst) };
    let units = match inst.id {
        Some(id) => {
            // SAFETY: the caller resolved `ctx` from the installed binding.
            unsafe { ctx.services() }
                .state::<VisualState>()
                .timers
                .get(&id)
                .map_or(DEFAULT_INTERVAL_UNITS, |timer| timer.interval_units)
        }
        None => DEFAULT_INTERVAL_UNITS,
    };
    // Reference getter: `GetInterval() * (1.0 / (1 << 16))`
    // (`TimerIntf.cpp:186-188`). The u64 → f64 conversion rounds to the
    // nearest representable double exactly like the C++ one: for the wrapped
    // `interval = -1` storage `2^64 - 65535` it lands on `2^64 - 65536`, so
    // the readback is `(2^64 - 65536) / 65536 = 2^48 - 1 =
    // 281474976710655.0`, not a hardcoded magic number.
    set_real(out, units as f64 * (1.0 / SUBMILLI_SCALE));
    0
}

/// `timer.interval = v` — the reference fixed-point setter
/// (`TimerIntf.cpp:196-197`) plus the L2 schedule update.
///
/// The script value is stored **verbatim** in the binding (scaled by
/// `1 << 16` through `(tjs_int64)(v * 65536 + 0.5)` and reinterpreted as
/// the reference's `tjs_uint64`, so negatives wrap just like in C++) and
/// read back through the reference getter conversion. The schedule sync then
/// derives L2's state from it:
///
/// * `> 0` on an armed timer → `ceil(units / 65536)` ms applied through
///   `set_interval`, which re-arms from *this write's* timestamp like
///   `tTVPTimerThread::SetInterval` (`TimerImpl.cpp:294-301`);
/// * `<= 0` → the timer is **disarmed** instead: the reference thread skips
///   a zero interval entirely (`TimerImpl.cpp:125`) and a negative value
///   wraps to a ~8775-year interval, so in both cases the timer is "armed
///   and enabled yet never fires" — expressed here by not arming the engine while
///   the binding keeps reporting the script's `enabled`.
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
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let requested = value_as_f64(unsafe { &*value });
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; no `&mut` to it is live in `timer_interval_set`.
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    // Reference setter (`TimerIntf.cpp:196-197`):
    //   double interval = (double)*param * (1 << TVP_SUBMILLI_FRAC_BITS);
    //   SetInterval((tjs_int64)(interval + 0.5));
    // with `SetInterval` taking a `tjs_uint64` (`TimerImpl.h:30`), i.e. the
    // signed result is reinterpreted modulo 2^64. Rust's `f64 as i64` cast
    // saturates where the C++ cast is undefined for out-of-range operands
    // (x86 yields `i64::MIN`); every in-range value — including `-1`,
    // which stores `2^64 - 65535` — converts identically.
    let units = ((requested * SUBMILLI_SCALE) + 0.5) as i64 as u64;
    let now = {
        let Some(timer_ptr) = services.timer() else {
            return report_error(out_error, "Timer.interval: timer registry is not attached");
        };
        // SAFETY: `attach_timer` guarantees the pointer stays valid; the read
        // ends before the schedule sync borrows the registry mutably.
        unsafe { &*timer_ptr }.now_ms
    };
    {
        let mut state = services.state::<VisualState>();
        let Some(binding) = state.timers.get_mut(&id) else {
            return 0;
        };
        // Verbatim: no clamping, no normalisation — the getter's reference
        // conversion reproduces exactly this storage.
        binding.interval_units = units;
    }
    sync_schedule(services, id, now, true, true);
    0
}

/// TJS `(double)*param` coercion for the interval setter
/// (`TimerIntf.cpp:196`): reals pass through, integers widen, numeric
/// strings parse (`0.0` for anything else, like `AsReal` on a non-numeric
/// variant).
fn value_as_f64(v: &Value) -> f64 {
    match v.ty {
        VAL_REAL => v.real,
        VAL_INTEGER => v.integer as f64,
        VAL_STRING => crate::natives::system::tjs_string_to_real(&value_as_string(v)),
        _ => 0.0,
    }
}

/// `timer.enabled` — what the script set (`TimerIntf.cpp:204-208`), read
/// back **verbatim** from the binding: a timer the script enabled reports
/// `true` even while the never-post conditions keep L2 un-armed, because
/// the reference's `Enabled` is independent of what the timer thread does
/// with it.
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
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; no `&mut` to it is live in `timer_enabled_get`.
    let inst = unsafe { &*(instance as *const TimerInst) };
    let enabled = match inst.id {
        Some(id) => {
            // SAFETY: the caller resolved `ctx` from the installed binding.
            unsafe { ctx.services() }
                .state::<VisualState>()
                .timers
                .get(&id)
                .is_some_and(|binding| binding.enabled)
        }
        None => false,
    };
    set_bool(out, enabled);
    0
}

/// `timer.enabled = v` — store the script value verbatim and re-derive the
/// L2 arming (the schedule sync disarms unless `capacity >= 0` and the
/// interval is positive, so `enabled = true` on a never-post timer changes
/// the binding only).
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
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let enabled = value_as_bool(unsafe { &*value });
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; no `&mut` to it is live in `timer_enabled_set`.
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        log::trace!("Timer.enabled -> {enabled} but the instance has no id");
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let now = {
        let Some(timer_ptr) = services.timer() else {
            return report_error(out_error, "Timer.enabled: timer registry is not attached");
        };
        // SAFETY: `attach_timer` guarantees the pointer stays valid; the read
        // ends before the schedule sync borrows the registry mutably.
        unsafe { &*timer_ptr }.now_ms
    };
    {
        let mut state = services.state::<VisualState>();
        let Some(binding) = state.timers.get_mut(&id) else {
            return 0;
        };
        binding.enabled = enabled;
    }
    sync_schedule(services, id, now, false, !enabled);
    let armed = services
        .state::<VisualState>()
        .timers
        .get(&id)
        .map(|binding| binding.armed);
    log::trace!("Timer {id:?}: enabled -> {enabled} (engine armed: {armed:?})");
    0
}

/// `timer.capacity` — the script-visible reference `Capacity` (default 6),
/// read back **verbatim** including `0` and negatives
/// (`TimerIntf.cpp:222-236`, `TimerIntf.h:63`).
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
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; no `&mut` to it is live in `timer_capacity_get`.
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        set_int(out, DEFAULT_TIMER_CAPACITY);
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let capacity = unsafe { ctx.services() }
        .state::<VisualState>()
        .timers
        .get(&id)
        .map_or(DEFAULT_TIMER_CAPACITY, |timer| timer.capacity);
    set_int(out, capacity);
    0
}

/// `timer.capacity = n` — store the script value verbatim and re-derive the
/// L2 arming.
///
/// Reference `SetCapacity` only stores the value (`TimerIntf.cpp:234-236`);
/// it is interpreted at fire time (`TimerIntf.cpp:69-92`), where a
/// **negative** capacity makes `more = cap - queued <= 0`, so no `onTimer`
/// is ever posted while the timer itself keeps running — expressed here by
/// *disarming* L2 while the binding keeps reporting `enabled = true`. `0`
/// is the reference "no limit" sentinel (`TimerIntf.cpp:77`) and every
/// positive value merely bounds the *queued* events (empty under
/// synchronous dispatch), so neither stops delivery: writing one arms the
/// timer for real if the script enabled it.
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
    // Verbatim: no `clamp` — a negative script value must survive for the
    // getter (`TimerIntf.cpp:233` stores `*param` as written).
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let requested = value_as_i64(unsafe { &*value });
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; no `&mut` to it is live in `timer_capacity_set`.
    let inst = unsafe { &*(instance as *const TimerInst) };
    let Some(id) = inst.id else {
        return 0;
    };
    // SAFETY: the caller resolved `ctx` from the installed binding.
    let services = unsafe { ctx.services() };
    let now = {
        let Some(timer_ptr) = services.timer() else {
            return report_error(out_error, "Timer.capacity: timer registry is not attached");
        };
        // SAFETY: `attach_timer` guarantees the pointer stays valid; the read
        // ends before the schedule sync borrows the registry mutably.
        unsafe { &*timer_ptr }.now_ms
    };
    {
        let mut state = services.state::<VisualState>();
        let Some(binding) = state.timers.get_mut(&id) else {
            return 0;
        };
        binding.capacity = requested;
    }
    sync_schedule(services, id, now, false, false);
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
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; no `&mut` to it is live in `timer_mode_get`.
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
    // SAFETY: `value` is the valid `Value` the C++ trampoline passes to this callback.
    let mode = value_as_i64(unsafe { &*value });
    // SAFETY: `instance` is the live `TimerInst` payload the VM allocated for this script object; no `&mut` to it is live in `timer_mode_set`.
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

/// Deliver the `onTimer` events the timer registry queued and return
/// whether any timer remains **armed** (the `system::continuous_handler_poll`
/// convention): a timer the script disabled
/// or that the reference would never post (negative capacity, non-positive
/// interval) does not keep the frame loop polling.
///
/// The registry's pump belongs to the engine: it advances the registry and
/// its callbacks post the `onTimer` events (see [`timer_callback`]). This
/// step delivers the posts the pump produced and reports the armed state.
///
/// The frame body does not call this: the engine advances the timer registry
/// before it runs, so [`timer_posts_poll`] already finds the posts here.
///
/// Delivery failures inside the callbacks are logged by
/// [`dispatch_on_timer`] and never disable the timer — the reference has no
/// disable-on-error path in `TimerIntf.cpp`/`TimerImpl.cpp`.
///
/// # Errors
///
/// Returns [`CompatError::Engine`] when the L2 timer registry is not
/// attached to the host.
pub fn timer_poll(ctx: &mut CompatCtx) -> Result<bool, CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let attached = unsafe { ctx.services() }.timer().is_some();
    if !attached {
        return Err(CompatError::Engine("timer registry is not attached".into()));
    }
    timer_posts_poll(ctx)?;
    // L3 mirrors what it pushed to L2 (`TimerBinding::armed`).
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let any_armed = unsafe { ctx.services() }
        .state::<VisualState>()
        .timers
        .values()
        .any(|binding| binding.armed);
    Ok(any_armed)
}

/// Deliver the `onTimer` postings queued since the last drain and return
/// whether the queue held any.
///
/// The L2 registry's callbacks only enqueue; this is the L3 delivery step. The
/// frame body runs it first, after L2 advanced the registry, so the events the
/// advance produced are delivered in the frame that produced them. Ordering is
/// the reference event queue's: exclusive before normal before idle
/// (`EventIntf.cpp:426-519`). The queue is taken out whole, so a handler that
/// posts again lands in the next drain rather than re-entering the VM
/// mid-drain.
///
/// # Errors
///
/// Returns [`CompatError::Engine`] when the L2 timer registry is not
/// attached to the host, so the queue cannot be drained against a live
/// registry. A delivery failure inside a handler is logged and never
/// disables the timer.
pub(crate) fn timer_posts_poll(ctx: &mut CompatCtx) -> Result<bool, CompatError> {
    // SAFETY: a `CompatCtx` is only created for a bound, live engine.
    let host = unsafe { ctx.engine() };
    // SAFETY: see above.
    let services = unsafe { ctx.services() };
    let mut posts = {
        let mut queue = services.state::<TimerPosts>();
        std::mem::take(&mut queue.pending)
    };
    if posts.is_empty() {
        return Ok(false);
    }
    posts.sort_by_key(|post| mode_priority(post.mode));
    for post in posts {
        if !timer_post_is_live(services, post.id, post.generation) {
            continue;
        }
        dispatch_on_timer(host, services, post.id);
    }
    Ok(true)
}

pub(super) fn register_timer_class(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "Timer",
        create: timer_create,
        destroy: timer_destroy,
        invalidate: Some(timer_invalidate),
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
        // Exactly the reference member set: constructor, `onTimer` and the
        // four properties (`TimerIntf.cpp:137-259`). No `id`, no `count`,
        // no `action`, no `cancel` — a compatible emulator exposes nothing
        // beyond the reference surface.
        properties: vec![
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
