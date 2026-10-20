# `frame.rs` — the ordered per-frame poll body

**Source:** `crates/compat/src/frame.rs`

L2 owns the frame clock and the frame entry point; the **hook body** is
L3/L4's work. This module defines the L3-side seam (`FrameBody`) and the
single function that enforces the order (`run_frame_body`).

The frame contract as a whole is in
[`../../rules/arch_rules.md`](../../rules/arch_rules.md) §4 and
[`../../arch/overall.md`](../../arch/overall.md) §4; this page is the L3
half.

---

## 1. The order

```
drain the onTimer posts L2 already fired
        → sound_poll → video_poll → async_trigger_poll
        → continuous_handler_poll → idle compaction → frame_tail
        → L2 projects the scene → L1 RenderPipeline::render
```

`run_frame_body` enforces this order; an implementor never does.

**The timer posts come first** because L2 advanced the timer registry and
its frame clock *before* the hook ran, and the registry's callbacks only
*post*. Delivering them before any other poll means the frame's `onTimer`
events are complete, and no other poll can disable or reschedule a timer
before its event was delivered.

---

## 2. The `FrameBody` trait

Every method defaults to a no-op, so a host implements only the polls its
natives need.

| Method | Advances |
|---|---|
| `sound_poll` | audio playback progress |
| `video_poll` | video playback progress |
| `async_trigger_poll` | async triggers ready to fire |
| `continuous_handler_poll` | continuous/periodic script handlers |
| `frame_tail` | the tail of the body: movie playback and layer transitions |

The `frame_tail` step runs **before** the projection and the render, so it
only advances the per-frame playback the projection reads. L3 owns neither
the projection nor the render.

### The timing hooks

Each poll has a matching `*_completed` hook that `run_frame_body` calls with
the poll's wall-clock duration:

`sound_completed`, `video_completed`, `async_trigger_completed`,
`continuous_handler_completed`, `frame_tail_completed`.

A host that wants per-frame cost attribution overrides the hooks it cares
about; the default discards the measurement. **A poll that returns an error
does not report a duration** — the remaining polls do not run after an error.

There are three more hooks the VM host reports, outside `run_frame_body`:

| Hook | Reported by |
|---|---|
| `input_begin_completed` | the event adapter's per-frame setup, before the events are drained |
| `input_events_completed` | the VM host, after the event loop |
| `input_end_completed` | the event adapter's per-frame commit |

---

## 3. `run_frame_body`

```rust
pub fn run_frame_body(body: &mut dyn FrameBody, ctx: &mut CompatCtx) -> Result<(), CompatError>;
```

The steps, in order:

1. `timer_posts_poll(ctx)` — deliver the `onTimer` events L2's advance
   posted.
2. `sound_poll` → `sound_completed`.
3. `video_poll` → `video_completed`.
4. `async_trigger_poll` → `async_trigger_completed`.
5. `continuous_handler_poll` → `continuous_handler_completed`.
6. `compact_poll(ctx)` — the idle compaction. The reference delivers
   `TVP_COMPACT_LEVEL_IDLE` from its per-loop idle check; it runs after the
   events and before the frame's draw, so the collector never runs inside a
   native callback.
7. `frame_tail` → `frame_tail_completed`.

**What `run_frame_body` does *not* do:** it does not fire timers and does not
project or render. `TimerState::advance` is private to L2 and is called by
the engine frame; the body only delivers the posts that advance produced.

---

## 4. The individual polls

| Poll | Module | What it advances |
|---|---|---|
| `timer_posts_poll` | `natives/visual/timer.rs` | the queued `onTimer` events, sorted by the timer `mode` priority (exclusive → normal → idle) |
| `timer_poll` | `natives/visual/timer.rs` | the registry **and** the drain — for a host or test that drives the registry itself |
| `sound_poll` | `natives/sound.rs` | the mixer to the L1 clock, and the `onStatusChanged` / `onFadeCompleted` / `onLabel` events |
| `video_poll` / `video_advance` | `natives/video.rs` | the open overlays by the frame's own `dt`, presenting the host frame at the current position |
| `async_trigger_poll` | `natives/async_trigger.rs` | the deferred `onFire` callbacks |
| `continuous_handler_poll` | `natives/system.rs` | the `System.addContinuousHandler` callbacks |
| `compact_poll` | `natives/system.rs` | the idle GC and the graphic cache |
| `frame_tail` | the host | the movie poll and the layer transitions |

`advance_transitions`, `advance_self_updating_transitions`,
`refresh_cached_layers` and `paint_poll` are the L3 pieces the host's
`frame_tail` composes — see [`visual/transitions.md`](visual/transitions.md).

---

## 5. Errors

`run_frame_body` returns the **first** error a poll produced, and the
remaining polls do not run. The errors are `CompatError::Engine` values
raised by the individual polls:

| Error | From |
|---|---|
| `"timer registry is not attached"` | `timer_poll` / `timer_posts_poll` without an attached L2 registry |

A failure inside a native is not an error here: the natives raise a typed TJS
error in the script's own context and log what they cannot raise.

---

## 6. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/src/natives/visual/mod.rs::timer_poll_fires_the_callback` | the timer drain is the first step of the body |
| `crates/compat/tests/timer_behavior.rs` | the drain delivers the posts L2 produced, in the mode priority order, and a cancelled post is skipped |
| `crates/compat/tests/sound.rs` | the sound poll advances the mixer and forwards the events |
| `crates/compat/tests/video.rs` | `video_advance` uses the frame body's `dt` |
| `crates/compat/tests/async_trigger.rs` | the trigger poll delivers the deferred fires |
| `crates/compat/tests/system.rs` | the continuous handlers run and can remove themselves |
| `crates/compat/tests/layer.rs::transitions_begin_and_stop` | the transition pump runs in the frame tail |

---

## See also

* [`event.md`](event.md) — the input phase of the frame
* [`visual/timer.md`](visual/timer.md) — the timer drain
* [`../../arch/l2.md`](../../arch/l2.md) §9 — the frame on the engine side
* [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §4 — the frame contract
