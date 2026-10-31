# `event.rs` — `EventAdapter`, the L1 → TJS event path

**Source:** `crates/compat/src/event.rs`

L1 owns the event loop and pushes `platform::PlatformEvent`s. This module
maps them to the TJS-visible events the VM dispatches to script handlers.
`EventAdapter::map` is the mapping, `EventAdapter::dispatch` is the delivery
seam and `EventAdapter::dispatch_frame` is the batch the VM host's frame
body runs.

L3 does **not** redeclare the platform event type: the event type and loop
are L1, and only the TJS mapping lives here. The layer-level description is
in [`../../arch/l3.md`](../../arch/l3.md) §6.

---

## 1. The types

```rust
pub struct TjsEvent {
    pub name: &'static str,     // the handler name, e.g. "onKeyDown"
    pub args: Vec<EventValue>,   // the positional arguments
}

pub enum EventValue {
    Int(i64),
    Real(f64),
    Str(String),
    Bool(bool),
}
```

The argument list is the reference's `Fire*` positional list, **not** an event
dictionary.

---

## 2. The platform → TJS mapping

| Platform event | TJS event | Arguments |
|---|---|---|
| `Resized(_, _)` | `onResize` | none |
| `ScaleFactorChanged(_)` | — | ignored; there is no KAG handler |
| `CloseRequested` | `onCloseQuery` | `1` |
| `Focused(true)` | `onActivate` | none |
| `Focused(false)` | `onDeactivate` | none |
| `PointerMoved { x, y }` | `onMouseMove` | `x, y, shift` |
| `PointerLeft` | `onMouseLeave` | none (the **window-level** leave) |
| `MouseButton { button, pressed }` | `onMouseDown` / `onMouseUp` | `x, y, button, shift` |
| `Wheel { delta }` | `onMouseWheel` | `shift, delta, x, y` |
| `Key { code, pressed }` | `onKeyDown` / `onKeyUp` | `key, shift, process` |
| `Text(text)` (empty) | — | ignored; the reference platform posts nothing for an empty key |
| `Text(text)` | `onKeyPress` | `text, process` |
| `Touch { id, phase, x, y }` | `onTouchDown` / `onTouchMove` / `onTouchUp` | `x, y, cx, cy, id` |

Notes:

* The **mouse button code** is the TVP numbering (`mbLeft` = 0 … `mbX2` = 4);
  the platform's `MouseButton::Other(code)` passes its code through.
* The **wheel delta** is scaled to the reference's `±120` per notch, because
  the scripts read the delta in those units.
* The **shift mask** is the Windows `MK_*` set (`MK_LBUTTON`, `MK_RBUTTON`,
  `MK_SHIFT`, `MK_CONTROL`, `MK_MBUTTON`, `MK_ALT`) — the TVP `ssShift` /
  `ssCtrl` / `ssAlt` bits mapped onto them, OR-ed with the held mouse
  buttons.
* A **touch cancel** ends the contact like an end: the reference has no
  touch-cancel input event.
* A committed multi-character string is **one `onKeyPress` per character**,
  matching the reference platform's per-`tjs_char` post.

### The pointer position

`EventAdapter` caches the latest L2 pointer position and the shift mask
after an input event is applied (`cache_input_state`), because `map` is
state-free and still needs the position to build the positional arguments.

---

## 3. The batch path

`dispatch_frame(ctx, events)` is what the VM host's frame body runs
(`crates/vm/src/host.rs`). It applies every event to the L2 input state,
maps it and delivers it, in the order the platform produced the events. Two
deliveries are narrowed by the reference's own rules, and only those two:

| Rule | Effect |
|---|---|
| the window's move is **discardable** | a newer move replaces the pending one, so the window's `onMouseMove` runs once per frame, with the newest position (`reference/cpp/core/visual/WindowIntf.cpp:361-372`) |
| the layer's move needs a **changed position** | the layer's `onMouseMove` fires only when the position differs from the previous move's; the hit test still runs on every move (`reference/cpp/core/visual/LayerManager.cpp:426`, `:433`, `:507`) |

Every other event is delivered as it arrived.

### The dispatch budget

A frame that collects a flood of events (a pointer-move burst, a held key,
a scroll) dispatches as many of them as fit in the **dispatch budget** and
defers the rest to the next frame:

| Rule | Value |
|---|---|
| the budget | the wall-clock time one frame may spend dispatching events — 4 ms by default (`EventAdapter::DEFAULT_DISPATCH_BUDGET`) |
| the check | after an event was dispatched, so **every frame delivers at least one event** |
| the deferred events | stay queued in the adapter, in arrival order, and the next `dispatch_frame` delivers them **first** |
| the order | the delivery order is the arrival order; no event is reordered, and only the window's superseded move is dropped (the discardable rule above) |

The budget is a **presentation policy**, not a compatibility rule: it bounds
how long one frame may spend in the event loop, so a flood cannot spend
seconds in a single frame. 4 ms is a quarter of a 60 fps frame, which leaves
the rest of the frame for the presentation, and it is far above the cost of
ordinary input — a frame with a handful of events never defers anything.

`EventAdapter::with_dispatch_budget(budget)` builds an adapter with another
budget; a budget of zero dispatches exactly one event per frame, which is
what the tests use to pin the deferral.

The budget is a wall-clock slice, so it can also expire on a machine that is
busy: a frame that dispatches a handful of events then defers the rest of
them, and a caller that drives one frame only sees the first ones. The test
harness therefore builds its adapter with `Duration::MAX`, so a test that
drives an explicit event sequence gets all of it; the tests that exercise
the budget build their own adapter.

Because the events are delivered in the order they arrived, the L2 input
state follows the deliveries: an event that waits for the next frame is
applied to the state of that frame.

### The events are not merged

The wheel, the key, the text and the touch events are each delivered on their
own, in arrival order, and so is every pointer move to the **layer** that
changed position. The reference posts every one of them as an ordinary event
(`reference/cpp/core/visual/LayerIntf.cpp:3438-3447`, `:3506-3513`,
`:3882-3889`, `:3844-3877`), and the layer's move is gated by `poschanged`
there too (`reference/cpp/core/visual/LayerManager.cpp:426`, `:507`), so the
count and the order are script-observable — one layer delivery per changed
position.

Only the window's move is not: it is posted discardable
(`reference/cpp/core/visual/WindowIntf.cpp:361-372`), so one frame's burst
leaves one event and the handler runs once.

---

## 4. Delivery

`dispatch(ctx, event)` delivers a mapped event to the script handler that
owns it.

### The order

1. The **window** receives every mapped event.
2. The **layer** receives the events that hit it or have focus:

   | Event | Target layer |
   |---|---|
   | `onKeyDown`, `onKeyUp`, `onKeyPress`, `onMouseWheel` | the focused layer |
   | `onMouseMove`, `onMouseDown`, `onMouseUp` | the hit layer (or the mouse capture) |
   | `onTouchDown`, `onTouchMove`, `onTouchUp` | the touch-captured or hit layer |

3. The **enter/leave transition** is checked before the move/press delivery,
   exactly like the reference's `PrimaryMouseMove`.
4. The layer handler is delivered — for a move, only when the position
   changed since the previous move (`poschanged`,
   `reference/cpp/core/visual/LayerManager.cpp:426`, `:507`).
5. A pointer move that **re-enters** the window posts the window-level
   `onMouseEnter`.
6. A mouse **release** that completes a click posts `onClick` /
   `onDoubleClick` — to the layer first, then to the window.

### The window and the layer get different argument lists

The window and layer handlers differ only for the key events:

| Handler | Window | Layer |
|---|---|---|
| `onKeyDown`, `onKeyUp` | `(key, shift)` | `(key, shift, process)` |
| `onKeyPress` | `(key)` | `(key, process)` |

Every other input event carries the same arguments to both.

The **layer** argument list additionally converts the pointer position from
window coordinates into the layer's own coordinates, by subtracting the
offset to the primary layer (each layer's and every *parented* ancestor's
rect offset). The window handler keeps the window coordinates.

### The click sequence

A release is a click when the current multi-click sequence has not timed out
and the pointer has not moved past the sequence distance. The sequence comes
from the L2 `InputState`; the count decides `onClick` (count 1) versus
`onDoubleClick` (count ≥ 2), and the click arguments are `(x, y)`.

### Missing and throwing handlers

A missing handler is a **no-op** (logged at debug). A throwing handler is
logged at warn and **never aborts the frame**.

---

## 5. The dispatch state

Some mouse state has no L2 home, so it lives in
`CompatServices::state::<InputDispatchState>()`:

| Field | Meaning |
|---|---|
| `mouse_capture` | the layer that received `onMouseDown` and owns the following `onMouseMove`/`onMouseUp` until every mouse button is released |
| `last_mouse_layer` | the layer that received the last `onMouseEnter`, for the enter/leave transition |
| `touch_capture` | the layer that captured each active touch contact, so a drag stays on the pressed layer |
| `pointer_inside` | whether the pointer is currently inside the window |

### The mouse capture

A press sets the capture; a release clears it **only when no standard mouse
button is held**. A captured layer that no longer exists is dropped and the
capture falls back to the hit test.

`release_mouse_capture(ctx)` is the `Layer.releaseCapture` path: the manager
releases the mouse capture globally, ignoring which layer captured.

### The touch capture

`onTouchDown` hit-tests and records the capture for the contact id;
`onTouchUp` uses the stored capture (falling back to a hit test) and removes
it; `onTouchMove` uses the stored capture (falling back to a hit test).

---

## 6. The hit test

`hit_layer(ctx, x, y)` walks the **visible** windows of the scene, and for
each takes its primary layer and asks the L2 scene for the layer at the
point. The L2 scene owns the hit test, the mask plane, the hit threshold and
the hit type.

The touch hit test rounds the (real) touch coordinates to integers before
the hit test.

---

## 7. Errors

The adapter does not raise. A missing handler is logged at debug; a throwing
handler is logged at warn. The only message it produces is:

```text
event adapter: {name} has no handler ({error})
event adapter: {name} dispatch failed ({error})
```

The error text of a missing member is recognised by the substring
`"does not exist"`.

---

## 8. Host seams

| Seam | Where |
|---|---|
| `EventAdapter::new()` | the composition root creates one and drives it |
| `dispatch_frame(ctx, events)` | the VM host's frame body delivers one frame's events |
| `cache_input_state(&InputState)` | called after each input event is applied, so the adapter can build the positional arguments |
| `attach_input` | the L2 `InputState` the adapter reads |

The composition root maps the platform pointer and touch coordinates from
device pixels into **VM pixels** before the adapter sees them; the
translation lives in L2 and is not repeated here.

---

## 9. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/event.rs` | the platform → TJS mapping table, the argument lists, the wheel scale, the shift mask, the touch family |
| `crates/compat/tests/event_dispatch.rs` | the delivery order: the window first, then the layer; the enter/leave transition; the click/double-click synthesis |
| `crates/compat/tests/input_move.rs` | the two move rules: one frame of moves runs the window's handler once at the newest position, and the layer's handler once per changed position |
| `crates/compat/tests/event_batch.rs` | the batch path: a burst of pointer moves is delivered across the frames in order, a deferred wheel reaches the focused layer in the next frame, the wheel/key/text events are all delivered, a target without a handler is a no-op and a throwing handler is reported |
| `crates/compat/tests/input.rs` | the input state the adapter feeds |
| `crates/compat/tests/focus_behaviour.rs` | the focused layer receives the key and wheel events |
| `crates/compat/tests/layer_behaviour.rs` | the layer-local coordinate conversion |
| `crates/compat/tests/window.rs::post_input_event_delivers_key_events_to_window_and_focused_layer` | `Window.postInputEvent` reaches the window `(key, shift)` and the focused layer `(key, shift, 1)` |

---

## See also

* [`frame.md`](frame.md) — where the adapter runs in the frame
* [`visual/focus.md`](visual/focus.md) — the focus state the adapter reads
* [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §5 — the coordinate spaces
