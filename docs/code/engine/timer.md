# `engine::timer` — the timer registry

Source: `crates/engine/src/timer.rs`

The registry **remembers when to wake up, then calls back**. Timers are
explicit L2 state driven by the frame clock: the engine passes the current
time each frame to the registry's pump, which invokes the due callbacks in
creation order.

The registry is **repeating by construction**: a timer created through
`create_interval` wakes its callback every `interval_ms`, first one interval
from the injected now, and keeps re-arming until it is removed or disabled.
There is no one-shot mode, no fire budget and no suppression flag — L2's only
job is "wait, then call".

Whether a timer is armed *at all* is L3's decision: the reference's "armed
and enabled yet never posts" shapes — a negative capacity, a non-positive
interval — are expressed by L3 simply not arming the timer.

---

## 1. Responsibility

* Register a repeating callback with an interval.
* Arm, disarm, reschedule and remove timers.
* Deliver the due callbacks in creation order, exactly once per advance.
* Stay consistent when a callback re-enters the registry or panics.

---

## 2. State it owns

```rust
pub struct TimerState {
    timers: BTreeMap<TimerId, TimerEntry>,
    next_id: u64,
    pub now_ms: u64,
}

struct TimerEntry {
    interval_ms: u64,
    deadline_ms: Option<u64>,
    generation: u64,
    callback: Option<TimerCallback>,
}
```

| field | meaning |
|---|---|
| `timers` | keyed by the monotonic id, which equals creation order — the map's ordered iteration is what makes the delivery deterministic |
| `now_ms` | the most recently injected time: the last `advance(now_ms)`, or the last write to this field |
| `interval_ms` | the interval between fires; always at least 1 ms |
| `deadline_ms` | `Some(deadline)` = armed and due at that time; `None` = disarmed |
| `generation` | bumped on every schedule mutation **and** when the timer is collected as due |
| `callback` | `None` only while the callback itself is running |

The registry owns no global and no clock: time enters only through `now_ms`
and `advance`.

---

## 3. Public API

```rust
pub struct TimerId(pub u64);
impl TimerId { pub fn value(self) -> u64; }

pub type TimerCallback = Box<dyn FnMut(TimerId) + 'static>;

pub enum TimerError {
    UnknownTimer(TimerId),
    InvalidInterval,
}

impl TimerState {
    pub fn new() -> Self;
    pub fn create_interval(&mut self, interval_ms: u64, callback: TimerCallback)
        -> Result<TimerId, TimerError>;
    pub fn remove(&mut self, id: TimerId) -> Result<(), TimerError>;
    pub fn set_interval(&mut self, id: TimerId, interval_ms: u64) -> Result<(), TimerError>;
    pub fn set_enabled(&mut self, id: TimerId, enabled: bool) -> Result<(), TimerError>;
    pub fn advance(&mut self, now_ms: u64) -> usize;
}
```

---

## 4. Time injection

Two entry points inject time, and both keep the registry deterministic for
tests:

* **`deliver_due(registry, now_ms)`** takes the time from the frame clock and
  *delivers* — the frame's event-pump step. It is a free function and not a
  method, because the compatibility layer names the registry to arm timers and
  a method would hand it the pump as well.
* **Assigning `now_ms`** injects the current time *without* delivering. L3's
  property setters write it before (re-)arming, so a schedule write between
  two polls lands on the write's own timestamp, exactly like the reference
  deriving its next tick from the tick count at the enable/interval time.

---

## 5. Re-entrancy

Callbacks run **inside** the delivery, yet they may re-enter the registry —
creating timers, changing intervals, removing themselves — through the same
handle the host attached. The delivery is therefore structured as three
phases:

1. **Collect first.** Every due timer is collected in creation order and
   re-armed from `now_ms` *before* any callback runs. No phase iterates a
   collection it is mutating, a late frame fires exactly once (no catch-up
   burst), and a nested `advance` can never deliver the same timer twice in
   one round.
2. **Then invoke.** The callback box is *taken out* of the entry before the
   call, so the entry is plain registry state while the callback runs. A
   collected timer fires only if its schedule is untouched since collection:
   any `set_interval` or `set_enabled` bumps the entry's schedule generation,
   and a removed timer simply has no entry left — both cancel the pending
   fire, like the reference cancelling an already-queued event.
3. **Restore afterwards.** The taken callback is put back once the call
   returns — **including when the callback panics**: unwinding runs the
   restore guard, so the registry is left consistent (the timer keeps firing
   on later advances) and only the panic propagates.

The registry is single-threaded: a callback's re-entrant borrow is created
and dropped entirely within one callback invocation, strictly sequential with
`advance`'s own accesses.

---

## 6. Invariants

* **A timer fires at most once per advance** and is re-armed from `now_ms`,
  so a late frame does not burst catch-up fires.
* **A timer whose schedule was mutated between collection and delivery is
  skipped** — the mutation cancelled its pending fire. A timer removed
  mid-round has no entry left to invoke.
* **The interval is at least 1 ms.** A zero interval would be due
  immediately on every advance, so it is rejected at creation and on every
  `set_interval`.
* **Enabling arms at `now + interval_ms`**, measured from the injected now;
  disabling drops the pending deadline, and the next enable re-arms from
  then.
* **Rescheduling an armed timer re-arms it from the injected now** and
  cancels the pending fire. A disarmed timer only stores the interval.
* **Delivery order is creation order**, because the id is monotonic and the
  registry is an ordered map.
* **`advance` returns the number of callbacks invoked.**

---

## 7. Data flow

```
Engine::frame(now_ms)
   │
   ▼
timer::deliver_due(&mut timer, now_ms)
   ├─ collect the due timers in creation order, re-armed from now_ms
   └─ invoke each callback with its TimerId
          │
          ▼
      L3's dispatch closure ──► the VM posts the script's onTimer event
```

L3 owns the script-facing `Timer` object and registers its dispatch closure
as the callback; the frame body only drains the posts the engine produced.

---

## 8. Failure modes

| failure | behaviour |
|---|---|
| an id that is not registered | `TimerError::UnknownTimer` |
| a zero interval | `TimerError::InvalidInterval` |

Both are typed errors; the registry never substitutes a default interval or
silently ignores an unknown id. A callback that panics propagates its panic
after the registry has been restored to a consistent state.

---

## 9. Current limits

* **`TimerState::advance` is crate-private.** The pump's public face is
  `timer::deliver_due`, which the compatibility layer does not call: the
  engine owns the registry's pump.

---

## 10. Tests

* `crates/engine/tests/timer.rs` — the arming, the re-arming, the
  rescheduling, the cancellation and the re-entrancy contract.

---

## 11. See also

| topic | document |
|---|---|
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the timer contract in the architecture | [`arch/l2.md`](../../arch/l2.md) §8 |
| the frame that advances the registry | [engine.md](engine.md) |
| the archived protocol trail | [`docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) §6.4 |
