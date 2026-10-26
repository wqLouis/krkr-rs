# `engine::clock` — the frame clock

Source: `crates/engine/src/clock.rs`

The clock is **the engine's time base**: the value the frame advances and the
value the script reads through `System.getTickCount`. It is a plain object
the engine owns — there is no process-global clock, and nothing above L2 keeps
a second time base of its own.

The module also holds the **clock source** — the seam the caller injects the
frame's elapsed time through. The engine owns the clock it advances; the
caller owns where the elapsed time comes from.

The reference has the same shape: `System.getTickCount` returns the tick
counter its frame pacing follows
(`reference/cpp/core/base/impl/SystemImpl.cpp:622-629`), so the script and
the frame measure the same time there too.

---

## 1. Responsibility

* Hold the elapsed milliseconds since the engine started.
* Accumulate a frame's delta without losing its fractional millisecond.
* Land on an exact value when a frame carries its own time.
* Never run backwards and never wrap.

---

## 2. State it owns

```rust
pub struct FrameClock {
    ms: f64,
}
```

| field | meaning |
|---|---|
| `ms` | the elapsed milliseconds, including the fractional part |

The clock holds no wall-clock reading: the time enters through
`advance(dt_ms)` and `set(now_ms)`, and nowhere else.

---

## 3. Public API

```rust
impl FrameClock {
    pub fn new() -> Self;
    pub fn now_ms(&self) -> u64;
    pub fn now_f64(&self) -> f64;
    pub fn advance(&mut self, dt_ms: f64) -> u64;
    pub fn set(&mut self, now_ms: u64);
}

/// The source of a frame's elapsed time.
pub trait FrameTimeSource {
    fn elapsed_ms(&self) -> f64;
}

/// A frame time the caller injects before every frame.
pub struct InjectedFrameTime {
    pub elapsed_ms: f64,
}

impl InjectedFrameTime {
    pub fn new(elapsed_ms: f64) -> Self;
}

impl FrameTimeSource for InjectedFrameTime {
    fn elapsed_ms(&self) -> f64;
}
```

| item | behaviour |
|---|---|
| `FrameTimeSource` | the trait the frame's elapsed time comes through; `elapsed_ms()` is the milliseconds since the previous frame |
| `InjectedFrameTime` | the value the caller injects; the windowed loop writes the real time the platform measured into it, the headless runner writes the frame period it simulates |

| method | behaviour |
|---|---|
| `new()` | a clock at zero |
| `now_ms()` | the clock in whole milliseconds — the value the timer registry, the upward hook and the script see |
| `now_f64()` | the clock with its fractional part |
| `advance(dt_ms)` | adds the delta and returns the new whole-millisecond clock; a negative delta counts as zero, the accumulated value keeps its fractional part, and the clock saturates at the integer ceiling |
| `set(now_ms)` | puts the clock at an exact value; the fractional remainder an earlier delta left behind is gone |

---

## 4. Who moves it

| caller | effect |
|---|---|
| `Engine::run(clock, …)` | `advance(clock.elapsed_ms())` — the caller's elapsed time accumulates, and the frame's `now_ms` is the clock's whole-millisecond part |
| `Engine::step(dt_ms, …)` | `advance(dt_ms)` — the delta accumulates, and the frame's `now_ms` is the clock's whole-millisecond part |
| `Engine::frame(now_ms, …)` | `set(now_ms)` — the clock is exactly the injected value, so a test can jump between frames |
| the composition root | nothing; the root injects the elapsed time, it never writes the clock |

The engine exposes the clock through `Engine::clock()` and
`Engine::clock_mut()`; the composition root hands the clock's address to the
compatibility layer, where `System.getTickCount` reads it.

---

## 5. Invariants

* **The fractional part is kept.** A frame delta shorter than a millisecond
  must add up over the frames: sixty `advance(16.6)` calls reach 996 ms, not
  960 ms.
* **The clock never runs backwards.** A negative delta counts as zero.
* **The clock never wraps.** It saturates at the integer ceiling.
* **The script and the frame share one clock.** The tick count a script reads
  is the clock the frame advances, so a timer the script arms is fired
  against the same time the script measures.
* **Two engines in one process do not interact.** The clock is a field of the
  engine.

---

## 6. Data flow

```
the caller's clock source
   │
   ▼
Engine::run(clock) ──► FrameClock::advance(elapsed) ──► now_ms (whole ms)
   │                              │
   │                              ├──► timer::deliver_due(now_ms)
   │                              ├──► the upward hook's now_ms / dt_ms
   │                              └──► System.getTickCount() ──► the script
   ▼
the frame draws
```

---

## 7. Failure modes

The clock cannot fail: it has no fallible operation. A delta that would
overflow the integer clock saturates instead of wrapping, and a negative
delta counts as zero instead of rewinding.

---

## 8. Current limits

* **The clock is a plain accumulator.** It carries no wall-clock reading and
  no monotonic source, so a host that wants the real elapsed time injects it
  through the clock source.
* **The source is an elapsed time, not an absolute tick.** The reference has
  no frame delta at all — every consumer reads an absolute clock and takes
  the difference itself. Accumulating the caller's elapsed time is
  observationally the same as an absolute clock as long as the caller injects
  the real elapsed time, which the windowed loop does; the two differ only
  for a frame that is *skipped* rather than run late.

---

## 9. Tests

* `crates/engine/tests/clock.rs` — the fractional millisecond across the
  deltas, the negative delta, the exact set and the saturation.
* `crates/engine/tests/engine.rs` — the engine exposes the clock it
  advances, and the frame's own clock behaviour.

---

## 10. See also

| topic | document |
|---|---|
| the frame that advances the clock | [engine.md](engine.md) |
| the timer registry the clock fires | [timer.md](timer.md) |
| the script's tick count | [../compat/system.md](../compat/system.md) |
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the frame contract in the architecture | [`arch/l2.md`](../../arch/l2.md) §9 |
