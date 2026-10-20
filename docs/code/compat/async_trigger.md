# `compat::natives::async_trigger` — the `AsyncTrigger` class

Source: `crates/compat/src/natives/async_trigger.rs` · layer **L3** · class
`AsyncTrigger`

`AsyncTrigger` is the script's **deferred callback**: `trigger()` schedules a
fire, and the fire happens at the next poll of the frame body — never inside
the call that scheduled it.

---

## 1. Registered surface

**Methods (3 plus the constructor):** `AsyncTrigger` (the constructor),
`trigger`, `cancel`, `onFire`.

**Properties (2), read/write:** `cached` and `mode`.

The mode constants are global, not class members: `atmNormal = 0`,
`atmExclusive = 1`, `atmAtIdle = 2`.

---

## 2. The constructor

`new AsyncTrigger(action[, actionName])`.

* The first argument is **mandatory** and is retained as the action.
* The second argument selects how the action is invoked:
  * omitted or `void` → the member `"action"` of the action object;
  * the empty string `""` → the action object **itself**;
  * a non-empty string → that member of the action object.

A second constructor call on the same instance raises
`AsyncTrigger: already constructed`.

---

## 3. `trigger`, `cancel`, `onFire`

`trigger()` **schedules** a fire. It makes no VM call, so the handler can
never re-enter the VM through the triggering call.

* With `cached = true` (the default) at most **one** fire is pending per
  object: a new `trigger()` cancels the pending one and re-posts it, so the
  re-triggered fire moves to the **end** of the queue.
* With `cached = false` every `trigger()` queues its own fire.

`cancel()` drops every scheduled fire for the object.

`onFire()` invokes the retained action **immediately** — it is not deferred.
The action receives the reference event dictionary
`{type: "onFire", target: <the trigger object>}`.

After invalidation, `trigger` and `cancel` are **silent no-ops** and `onFire`
does nothing.

---

## 4. `cached` and `mode`

`cached` defaults to **true**; `mode` defaults to `0` (`atmNormal`).

**Changing** either value cancels the object's pending fires; writing the
*same* value again does not.

Both survive `invalidate`, because the reference keeps them as native-instance
members that outlive the invalidation (only the owner and the action owner
are released).

`mode` is stored and delivered **uniformly** at the poll: `atmExclusive` and
`atmAtIdle` do not reorder or defer the delivery relative to other triggers.
See §8.

---

## 5. The poll

Everything runs on the **VM thread** inside the frame body. The poll order is
fixed:

```
drain the onTimer posts → sound_poll → video_poll → async_trigger_poll
  → continuous_handler_poll → idle compaction → frame_tail
```

`compat::async_trigger_poll(ctx)` is the `async_trigger_poll` step of
`FrameBody`.

The poll **takes the whole pending list first** and then dispatches each
entry. A `trigger()` issued from inside a handler lands in the emptied queue
and is therefore deferred to the next poll — there is no mid-dispatch
re-entry.

The dispatch calls the object's **`onFire` member**, so a script subclass
override wins. When the source object could not be retained, the native
action is invoked directly with an event dictionary that has **no `target`**.

---

## 6. State ownership

| state | owner |
|---|---|
| the id, the `cached` flag, the `mode` | the **per-object payload** (`AsyncTriggerInst`) |
| the monotonic id counter, the binding map (the retained action, the action name, the raw owner handle), the pending FIFO | the **per-engine dispatch state** (`CompatServices::state::<AsyncTriggerState>()`) |

There is **no L2 seam**: the class deliberately does not use the L2 timer
registry. The registry is modelled on it but stays L3-local.

A queued fire holds a **strong reference** to its source object, so a
trigger created in a local scope survives until the poll delivers the fire.
The retention is released on delivery, and the destroy callback removes the
binding so a dropped trigger leaks nothing.

---

## 7. Errors

| shape | example |
|---|---|
| `"AsyncTrigger: constructor requires an action callback"` | no action argument |
| `"AsyncTrigger: compat context is not bound"` | the engine binding is missing |
| `"AsyncTrigger: cannot retain action: {error}"` | the action could not be retained |
| `"AsyncTrigger: already constructed"` | a second constructor call |
| `"AsyncTrigger.trigger: compat context is not bound"` | the engine binding is missing |
| `"AsyncTrigger.cancel: compat context is not bound"` | the same |
| `"AsyncTrigger.onFire: compat context is not bound"` | the same |
| `"AsyncTrigger.cached: compat context is not bound"` | the same, for the getter and the setter |
| `"AsyncTrigger.mode: compat context is not bound"` | the same |

A handler that raises during a fire is **logged**, not propagated:
`AsyncTrigger.onFire: <error>` for a direct call and
`AsyncTrigger <id>: onFire failed (<error>)` in the poll.

---

## 8. Tests

`crates/compat/tests/async_trigger.rs` covers the constructor defaults and
the mode constants, the deferral to the poll, the coalescing with `cached`,
the per-call queueing without it, `cancel`, the cancellation on a `cached` or
`mode` change (and the fact that writing the same value does not cancel), the
three action-name forms, the immediate `onFire`, the deferral of a
re-trigger issued from inside a handler, the missing-action error, the
subclass behaviour before the base constructor, the source retention across
the queue and its release on delivery, and the binding leak check on dropped
triggers.

---

## 9. Known gaps

1. **`mode` is not honoured beyond storage.** `atmExclusive` and `atmAtIdle`
   do not change the delivery order and there is no idle-specific delivery
   point; every pending fire is delivered uniformly at the poll.
2. **`onFire` called from script invokes the action immediately**, whereas
   the reference's native `onFire` is an event body. The observable behaviour
   matches — the reference body always invokes the action — but the two entry
   points differ in timing.
3. **A handler that raises is logged, not propagated** to the trigger's
   caller.
4. **The pre-constructor `trigger()` in a script subclass is a no-op**
   (there is no owner yet), not an error.

---

## 10. Related documents

| topic | document |
|---|---|
| the frame body and its poll order | [`../../arch/l3.md`](../../arch/l3.md) §6 |
| the L2 timer registry (a different mechanism) | [`../../code/engine/timer.md`](../../code/engine/timer.md) |
| the L3 layer | [`../../arch/l3.md`](../../arch/l3.md) |
