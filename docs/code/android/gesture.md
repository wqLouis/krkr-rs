# `gesture` — the touch → mouse bridge

Source: `crates/android/src/gesture.rs`

KiriKiri games are mouse-and-keyboard driven: a KAG menu resolves a click
through `onMouseDown`/`onMouseUp` and hit-tests the layer under the pointer,
while a raw touch stream reaches the compatibility layer as the separate
`onTouchDown`/`onTouchMove`/`onTouchUp` family, which such a menu never
sees. The reference Android environment therefore turns a single finger into
a virtual mouse — a tap clicks, a held finger presses, a drag moves the
cursor with the button held — and uses two fingers for the secondary button.

This module is that translation as a **pure state machine over milliseconds**.
The Android wiring ([input.md](input.md)) supplies the clock and forwards the
produced `InputEvent`s through the same host path as a real mouse; it consumes
the raw touch instead of forwarding it, so no event fires twice.

---

## 1. Responsibility

* turn a stream of touch phases into mouse and Escape input;
* keep the contact's start position and its last position, and nothing else,
  because only the coordinates and the frame the events arrive in are
  observable.

The module is **pure**: it has no Android type, no I/O and no clock of its
own. The caller passes `now_ms` in.

---

## 2. Layer and dependencies

| | |
|---|---|
| Layer | root (composition root) |
| Depends on | `api` (`TouchPhase`, `InputEvent`, `MouseButton`, `PhysicalKey`) |
| Must not depend on | `engine`, `compat`, `vm`, `android-activity` |
| Forbidden | a display service, an Android type, a wall-clock read |
| Gate | `cfg(any(target_os = "android", test))` — it compiles and is **fully tested** on the host |

---

## 3. Public API

```rust
pub(crate) struct GestureSynthesizer { … }

impl GestureSynthesizer {
    pub(crate) fn phase(&mut self, id: u64, phase: TouchPhase, x: f32, y: f32,
                        now_ms: u64) -> Vec<InputEvent>;
    pub(crate) fn tick(&mut self, now_ms: u64) -> Vec<InputEvent>;
}
```

`phase` feeds one touch phase and returns the input events it produces.
`tick` advances the clock so a held, stationary contact becomes a long press;
the Android wiring calls it once per event-loop iteration.

The thresholds are public constants of the module:

| constant | value | meaning |
|---|---|---|
| `DRAG_SLOP_PX` | `16.0` | a contact becomes a drag once it has travelled this far from where it went down |
| `LONG_PRESS_MS` | `1000` | a stationary finger held this long is a long press |
| `MULTI_TAP_MS` | `300` | a two-finger gesture that ends within this is a tap, not a hold |
| `SWIPE_DOWN_PX` | `64.0` | a two-finger drag down this far is the back gesture |

---

## 4. The mapping

| gesture | the events it emits |
|---|---|
| **Tap** — a single finger that does not travel beyond the slop and is released before the long-press timeout | left button down, then up, at the touch position (the L2 input state turns that pair into `onClick`/`onDoubleClick`) |
| **Drag** — a finger that travels past the slop | left button down, then a pointer move per move, then left button up |
| **Long press** — a finger held still past the long-press timeout | right button down (KAG opens its menu on the right button; `System.getMouseButtonState(1)`), then right button up on release |
| **Two-finger tap** — two fingers that both lift within the multi-tap window | an Escape press/release pair |
| **Two-finger swipe down** — two fingers dragged down past the swipe distance | an Escape press/release pair, fired once |

The Escape pair is what the Android back button already maps to
(`AKEYCODE_BACK` → `PhysicalKey::Escape`).

---

## 5. The state machine

```rust
struct Contact {
    id: u64,             // the stable Android pointer id
    start_x: f32, start_y: f32,   // where the contact went down
    x: f32, y: f32,               // the most recent position
    down_ms: u64,                 // when it went down
    button: Option<MouseButton>,  // the button it holds, once resolved
    long_press: bool,
}

struct GestureSynthesizer {
    primary: Option<Contact>,
    others: usize,          // the further contacts down while the primary is
    multi: bool,            // a second finger arrived before the primary resolved
    multi_start_ms: u64,
    multi_fired: bool,      // the back gesture already fired for this gesture
}
```

The rules that keep the interaction predictable:

* **A multi-touch gesture never holds a mouse button.** The primary contact's
  button is ignored while `multi` is set.
* **A second finger that arrives after a drag or a long press has begun does
  not turn the gesture into a back gesture.** It keeps the drag's capture; the
  gesture simply continues.
* **The back gesture fires once per gesture.** `multi_fired` latches it, so
  dragging further does not emit another Escape.
* **A cancelled gesture is not a tap.** A cancel releases the button the
  contact held and drops the multi-touch state without firing the back
  gesture.
* **A sub-slop wobble is still a tap**, at the release position.

### The DPI divergence

The reference scales its drag threshold by the display DPI
(`Device::getDPI() / 10`, so 32 px at 320 dpi), while this module uses a
**fixed physical-pixel slop**. That is a small deliberate divergence: it
keeps the synthesizer free of a display service, and the fixed value is small
enough that a deliberate drag passes it within a frame. The long-press
timeout is the reference's own 1000 ms.

---

## 6. State ownership

The synthesizer is owned by the `AndroidHost` ([host.md](host.md) §4) and
survives across input batches, because a tap or a drag spans several Android
motion events. The gesture clock is a monotonic `Instant` owned by the
handler; only the differences are compared.

Nothing here is global, and no state is shared between two hosts.

---

## 7. Platform gating

The module compiles on the host (`cfg(any(target_os = "android", test))`) and
is **fully host-tested**: it names no Android type and reads no clock.

---

## 8. Failure modes

The module cannot fail — it has no error channel. The two things it can do
instead are worth stating:

* A gesture it does not recognise (a contact that is neither the primary nor
  one of the others) produces **no events**. It is not an error and not a
  guess.
* The Escape pair is emitted **once** per multi-touch gesture, never twice.

The state machine never panics and never allocates unboundedly: the event
vector per phase holds at most a handful of events.

---

## 9. Tests

`crates/android/src/gesture.rs` `mod tests` covers the whole mapping:

* a tap emitting a left click at the touch position;
* a sub-slop move staying a tap, at the release position;
* a drag holding the left button across the moves;
* a long press emitting a right-button click, held until the finger lifts;
* a drag never long-pressing;
* a two-finger tap emitting Escape, when the last finger lifts;
* a slow two-finger lift not being a tap;
* a two-finger swipe down emitting Escape exactly once;
* a cancel releasing a held button;
* a cancel not emitting a back gesture;
* a second finger not cancelling an active drag;
* a fresh synthesizer starting idle.

**Not verified on the host:** that the Android motion actions and timestamps
actually reach the state machine in this order, that the synthesised events
hit a real KAG menu, and the DPI-based slop.

---

## 10. Related documents

| topic | document |
|---|---|
| the Android composition root overview | [README.md](README.md) |
| the input wiring that feeds this machine | [input.md](input.md) |
| the handler that owns the synthesizer | [host.md](host.md) |
| the reference Android touch mapping | [`android.md`](../../../docs.old/android.md) §5 · snapshot [`docs.old/android.md`](../../../docs.old/android.md) |
| the L1 input model the events feed | [`../platform/platform.md`](../platform/platform.md) §3 |
| the L2 input state that turns the pair into a click | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §5 |
