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
(`reference/cpp/core/base/impl/SystemImpl.cpp:622-629`), and that counter is
read live on every call (`reference/cpp/core/utils/TickCount.cpp:112-118`);
so there the script and the frame measure the same time too. Here the *source*
is that counter and it is read at two moments — the frame boundary and the
script's own call — which is the next section's subject.

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
    fn now_ms(&self) -> f64;
}
```

| item | behaviour |
|---|---|
| `FrameTimeSource` | the trait the frame's time comes through: `elapsed_ms()` is the milliseconds since the previous frame (read at the boundary), `now_ms()` is the source's reading *now* (read by the script's tick count) |
| `InjectedFrameTime` | the value a test injects; `now_ms()` answers with the injected frame time, because a simulated frame has no clock behind it |

### The two samplings

One source, read at two moments:

| moment | who reads | what it reports |
|---|---|---|
| the frame boundary | `Engine::run` → `FrameClock::advance` | the elapsed value, accumulated into the frame clock; the timer, the hook and the shallower timelines read that clock |
| `System.getTickCount()` | the compatibility layer, on demand | `FrameTimeSource::now_ms()`, never behind the frame clock |

A host whose source measures real time (the windowed loop's
`PlatformFrameTime`, the headless runner's `HeadlessClock`) therefore hands the
script a clock that moves **inside** a frame: a script that waits on the tick
count makes progress while no frame runs, and a frame that takes a second does
not hide that second. A host that attached no source, or a test that injected a
fixed frame time, reports the frame clock — the only time such a host has.

The frame clock stays the frame's value: the timer registry, the sound poll,
the transitions and the video step all read the boundary, so their schedules do
not jump mid-frame.

| method | behaviour |
|---|---|
| `new()` | a clock at zero |
| `now_ms()` | the clock in whole milliseconds — the value the timer registry, the sound poll, the transitions and the upward hook see |
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
| the composition root | nothing; the root injects the elapsed time and its source, it never writes the clock |

The engine exposes the clock through `Engine::clock()` and
`Engine::clock_mut()`; the composition root hands the clock's address to the
compatibility layer, where the timer registry, the sound poll and the
transitions read it, and the clock *source's* address, where
`System.getTickCount` reads it live.

---

## 5. Invariants

* **The fractional part is kept.** A frame delta shorter than a millisecond
  must add up over the frames: sixty `advance(16.6)` calls reach 996 ms, not
  960 ms.
* **The clock never runs backwards.** A negative delta counts as zero, and
  [`FrameClock::advance`] is the only mover that clamps; `set` (reached through
  `Engine::frame`) puts the clock at the value it is given, so a test that jumps
  backwards moves the frame's clock backwards deliberately.
* **The clock never wraps.** It saturates at the integer ceiling.
* **The script and the frame share one clock.** The tick count a script reads
  and the frame clock the timers are fired against are one source sampled at
  two moments, so a timer the script arms is fired against the same time the
  script measures; the script's sample is never behind the frame's.
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
   │                              ├──► the sound, video and transition polls
   │                              └──► the upward hook's now_ms / dt_ms

the same source, read on demand ──► System.getTickCount() ──► the script
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
* **The source is read at two moments, not one.** The frame's reading is
  accumulated into the clock and the script's is a live sample of the same
  source. The one thing the source does not supply is an epoch: a source that
  measures real time counts from the run's start, not from the system's boot
  (`reference/cpp/core/utils/TickCount.cpp:112-118` counts from the platform's
  monotonic origin), so the script's tick count starts near zero where the
  reference's starts near the machine's uptime.
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
* `crates/compat/tests/system.rs` — the tick count reads the source on demand
  (two reads inside one frame differ when the source moves), the frame clock
  with no source, and the missing-clock error.
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
