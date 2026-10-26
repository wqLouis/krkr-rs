# `natives/visual/timer.rs` — the `Timer` class

**Source:** `crates/compat/src/natives/visual/timer.rs`

`Timer` is the script-visible timer. The L2 `TimerState` registry holds only
what "wait, then call" needs — the integer-millisecond interval, the armed
deadline and the dispatch callback — so **every script-visible readback is
served from the L3 binding, never from L2**.

The registered surface is 5 members — `onTimer` and the properties
`interval`, `enabled`, `capacity` and `mode` — plus the class-name
constructor the surface dump records separately.

---

## 1. The members

| Member | Behaviour |
|---|---|
| `Timer(actionOwner[, actionName])` | retains `param[0]` as the action owner, stores `param[1]` (or `"action"`) as the action name, registers the L2 timer and returns void. At least one argument is required. |
| `onTimer()` | the native member: invokes the constructor's action owner with the `{type: "onTimer", target: <timer>}` event. It never raises on a dispatch failure. |
| `interval` | always a **real** in milliseconds. The default is `1000.0`. |
| `enabled` | the script's own value, verbatim (`0`/`1`). The default is `0` (disabled). |
| `capacity` | the script's own value, verbatim (an integer, negatives included). The default is `6`. |
| `mode` | the event delivery priority (`0` normal, `1` exclusive, `2` idle), verbatim. The default is `0`. |

There is **no** `id`, `count`, `action` or `cancel` member — the reference
registers exactly these five.

---

## 2. How each property is stored and read back

### `interval` — the sub-millisecond fixed point

The reference stores the interval in sub-millisecond fixed point
(`TVP_SUBMILLI_FRAC_BITS = 16`).

| Step | Behaviour |
|---|---|
| storage | `TimerBinding.interval_units: u64` |
| setter | the script value is coerced with the real coercion (reals pass through, integers widen, numeric strings parse, anything else → `0.0`), then `units = ((requested × 65536) + 0.5) as i64 as u64`. This reproduces the reference's cast into the `tjs_uint64` parameter, so negatives **wrap** modulo 2^64. The value is stored **verbatim**: no clamping, no normalisation. |
| getter | `units as f64 / 65536.0` — the result is **always a TJS real**. The wrapped `-1` reads back `2^48 − 1 = 281474976710655.0`. |
| default | `1000 << 16` → `1000.0`. |
| L2 | the L2 registry only receives whole milliseconds, derived as `ceil(units / 65536)` when the value is strictly positive as an `i64`; `0` and the wrapped negatives are not representable in L2, so the timer is not armed. |

A timer with no id reports the default interval and the setter is a silent
no-op.

### `capacity` — a verbatim `i64`

Stored **verbatim**, with no clamp; the getter reports it back, including `0`
and the negatives. The default is `TVP_DEFAULT_TIMER_CAPACITY = 6`.

| Value | Meaning |
|---|---|
| `0` | the reference's "no limit" sentinel, read as `65535` at fire time. |
| negative | the reference computes `more = cap − queued ≤ 0` and posts **nothing**. The port expresses this by **not arming L2** while the binding still reports `enabled == true`. |
| positive | bounds the number of *queued* events only. Under the synchronous dispatch the queue is always empty, so it never stops delivery. |

A capacity write never cancels a pending fire, because the reference's
`SetCapacity` only stores.

### `mode` — a verbatim `i64`

Stored and read back verbatim. It is **not** a one-shot switch: it selects
the delivery priority at post time. `mode_priority` maps `1` → exclusive
(`0`), `2` → idle (`2`) and everything else → normal (`1`), and the drain
sorts the queue by that key — exclusive events are delivered before normal
before idle.

The mode setter performs **no** schedule sync: the mode never affects when
the timer fires.

### `enabled` — a verbatim `bool`

Stored and read back from the binding, **never** from L2. The default is
`false` — the reference constructs the timer disabled.

A timer reports `enabled == true` even while L2 is unarmed, because the
reference's two "armed and enabled yet never fires" states (`capacity < 0`
and `interval <= 0`) are expressed by *not arming* the L2 schedule.

`TimerBinding.armed` is L3's mirror of whether L2 currently has the timer
armed; only the schedule sync writes it.

---

## 3. The L2 schedule

All of the arming lives in `sync_schedule`:

```
want      = binding.enabled && binding.capacity >= 0 && interval_ms.is_some()
was_armed = binding.armed
```

**The cancellation comes first.** When the writer asks for it, the timer's
*delivery* generation is bumped, so any queued `onTimer` post from the old
generation is skipped by the drain. The interval setter and
`SetEnabled(false)` cancel; a capacity write does not.

Then the L2 transition:

| Transition | Action |
|---|---|
| unarmed → armed | inject the write's own timestamp, then `set_interval` and `set_enabled(true)` — L2 sets the deadline to `now + interval`. |
| armed → unarmed | `set_enabled(false)` — L2 drops the deadline outright. |
| armed → armed, re-arm the interval | inject the timestamp and `set_interval` — re-arms from the injected now and bumps L2's schedule generation, cancelling that round's pending fire. |
| armed → armed, no re-arm | nothing; a pending fire is untouched. |
| unarmed → unarmed | nothing; the interval is pushed at the next arming. |

| Writer | Re-arms the interval | Cancels the pending events |
|---|---|---|
| the `interval` setter | yes | yes |
| the `enabled` setter | no | yes, when it turns the timer off |
| the `capacity` setter | no | no |
| the `mode` setter | — | — (no schedule sync at all) |

The **constructor** calls `create_interval(1000, callback)` — which arms by
contract — and then immediately `set_enabled(id, false)` to disarm. The
binding starts `enabled: false`, `armed: false`, `interval_units = 1000 <<
16`, `capacity = 6`, `mode = 0` and `action_name = "action"`.

An L2 error on a schedule write is **logged**, not raised:
`"Timer {id:?}: set_interval({interval_ms}) failed ({error})"` and
`"Timer {id:?}: set_enabled({enabled}) failed ({error})"`. A missing
registry logs `"Timer {id:?}: no timer registry is attached; the schedule
write is dropped"`.

**Teardown:** the invalidate hook removes the binding (releasing the action
owner and the retained timer object), forgets the posts and removes the L2
entry. The destroy hook repeats the removal idempotently.

---

## 4. The two polls

### `timer_posts_poll(ctx)`

The **first** step of the frame body ([`../frame.md`](../frame.md)).

1. It takes the **whole** pending queue out of the dispatch state, so a
   handler that posts again lands in the *next* drain rather than re-entering
   the VM mid-drain.
2. It sorts a copy by the mode priority — exclusive, normal, idle.
3. For each post it checks that the binding still exists **and** that the
   timer's delivery generation still matches; a cancelled post is skipped.
4. It dispatches the live posts.
5. It returns whether the queue held any.

### `timer_poll(ctx)`

For a host or a test that drives the registry itself:

1. It requires an attached registry, else
   `CompatError::Engine("timer registry is not attached")`.
2. It drains with `timer_posts_poll`, which hands the queued `onTimer`
   events to the script.
3. It returns whether any binding is **armed** — the "any timer still
   enabled" signal the continuous-handler convention uses. A script-disabled
   timer, or one the reference would never post, does not keep the frame loop
   polling.

**The frame body does not call `timer_poll`.** The L2 engine advances the
registry before the hook runs.

**The poll does not advance the registry.** The registry's pump belongs to
the engine: `TimerState::advance` is crate-private and the compatibility
layer cannot reach it. A host or a test that owns a registry delivers the due
timers itself through `engine::timer::deliver_due`, and the callbacks enqueue
the posts this poll drains.

---

## 5. The `onTimer` dispatch

Two stages.

### Stage 1 — finding the member

1. Read the binding's `owner` — the raw `objthis` captured in the
   constructor, held with no reference.
2. Retain it for the duration of the dispatch so the object cannot
   finalise under the call. A `0` handle is rejected; the fire is **skipped**
   with a debug log.
3. `call_member(owner, "onTimer", &[])` — a **member** call with zero
   arguments, exactly the reference's `FuncCall(0, "onTimer", …)`. The call
   walks the class chain, so **a script subclass's `onTimer` override wins**
   and the native member is reached only when there is no override.
4. A failure is logged, never raised, and **never disables the timer**:
   `"Timer {id:?}: onTimer dispatch failed ({error}); the timer stays
   enabled"`.

### Stage 2 — the native member

1. The action owner is **taken out** of the registry, so the state borrow is
   released across the VM call and a re-entrant poll or construct sees
   consistent state. A missing entry is a silent success.
2. An action owner of `None` — the constructor argument was not an object, so
   the reference's empty closure was stored — **skips the invocation
   silently**.
3. The event dictionary is built: `{type: "onTimer", target: <the timer
   object>}`. The `target` is omitted when the timer object could not be
   retained.
4. The dispatch is by action name: a non-empty name (the default is
   `"action"`) is a **member call** on the action owner; an empty name calls
   the owner itself.

Consequences:

* A subclass override **replaces** the constructor callback — the native
  member is never reached, so the constructor callback runs zero times.
* The reference's member-not-found result from a plain function with no
  `action` member is **discarded** — the reference never inspects the
  `FuncCall` result — so the script call still succeeds and the timer stays
  enabled.
* A throwing callback keeps the timer enabled and keeps its receiver on the
  next fire.

---

## 6. State ownership

| State | Owner |
|---|---|
| the action owner, the owner handle, the action name, the mode, the capacity, the enabled flag, the armed flag, the interval units | the L3 `TimerBinding` in `VisualState.timers` |
| the L2 timer id | the L3 `TimerInst` payload |
| the delivery queue and the per-timer cancellation generation | the L3 `TimerPosts` dispatch state |
| the interval in milliseconds, the deadline, the generation, the callback, the injected `now_ms` | the L2 `TimerState` |

---

## 7. Errors

| Message | Raised when |
|---|---|
| `"Timer: constructor requires an action owner"` | no argument |
| `"Timer: compat context is not bound"` | the binding is not installed |
| `"Timer: already constructed"` | the constructor ran twice |
| `"Timer: timer registry is not attached"` | no L2 registry |
| `"Timer: {error}"` | an L2 `create_interval` / `set_enabled` error |
| `"Timer.interval: compat context is not bound"`, `"Timer.interval: timer registry is not attached"` | the interval property |
| `"Timer.enabled: compat context is not bound"`, `"Timer.enabled: timer registry is not attached"` | the enabled property |
| `"Timer.capacity: compat context is not bound"`, `"Timer.capacity: timer registry is not attached"` | the capacity property |
| `"Timer.mode: compat context is not bound"` | the mode property |
| `"Timer.onTimer: compat context is not bound"` | the event member |

Logged, not raised: the dispatch failures, the schedule-write failures, the
missing registry and the missing owner handle.

---

## 8. Host seams

| Seam | Purpose |
|---|---|
| `attach_timer(*mut TimerState)` | the L2 registry |
| `timer_posts_poll(&mut ctx)` | the frame body's timer drain (crate-internal) |
| `compat::timer_poll(&mut ctx)` | the delivery form for a host or test that drives the registry itself |
| `CompatHost::timer_count()` | the retention-leak diagnostic |

The **host** (or the L2 engine) must advance the L2 registry; the L3 side
only delivers what the advance produced.

---

## 9. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/timer_behavior.rs` | the constructor arity error and the action-name shapes; the interval default, the real type, the zero interval, the wrapped negative and the re-arm from now; the enabled arming and the cancellation; the capacity default, the verbatim values and the "bounds queued events, not fires" rule; the mode as a priority and not a one-shot switch; the absence of the `count`/`action`/`id` members; the dispatch to the constructor callback, the subclass override winning, a self-disabling override, a re-entrant creation, a self-invalidating callback and a failing callback keeping the timer enabled; the teardown and the creation-order firing. |
| `crates/compat/tests/oop.rs` | `script_override_wins_over_native_on_timer`, `super_native_constructor_produces_a_fully_usable_instance`. |
| `crates/compat/tests/extend_dispatch.rs` | the subclass override and the capacity rules through the real dispatch path. |
| `crates/compat/tests/visual.rs` | `timer_properties_round_trip`. |
| `crates/compat/tests/dispatch_visual.rs` | the receiver routing. |
| `crates/compat/tests/timer_behavior.rs::the_registry_pump_delivers_the_timer_to_the_script` | the registry's pump delivers the timer to the script. |

---

## See also

* [`../frame.md`](../frame.md) — the frame body's first step
* [`../engine_api.md`](../engine_api.md) — the L2 timer registry
* [`common.md`](common.md) — the `TimerBinding` definition
