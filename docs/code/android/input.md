# `input` — the Android input events → the platform-neutral events

Source: `crates/android/src/input.rs`

The translation is deliberately thin: it reads the Android key code, the motion
action and the button off the `android-activity` event and hands them to the
L1 translation functions in `platform`'s Android backend. **No TVP concept is
named here**; the compatibility layer maps the physical key onto the
reference virtual-key table.

---

## 1. Responsibility

* drain the activity's queued input events;
* translate a motion event into a touch phase, a pointer move or a mouse
  button;
* translate a key event into an `InputEvent::Key`;
* forward the result through the normal host path.

---

## 2. Layer and dependencies

| | |
|---|---|
| Layer | root (composition root) |
| Depends on | `android-activity` (the input event types), `api` (`TouchPhase`), `platform` (`InputEvent`, `PlatformWindow`) |
| Must not depend on | `engine`, `compat`, `vm` |
| Forbidden | a reference virtual-key code, a game-specific special case |
| Gate | `cfg(target_os = "android")` — the module is Android-only |

---

## 3. Public API

The module is crate-private; it exposes one function to the loop:

```rust
pub(crate) fn translate_input(app: &AndroidApp, handler: &mut AndroidHost);
```

plus the private helpers `deliver`, `deliver_motion`, `deliver_touch` and
`deliver_key`.

---

## 4. The translation

### Motion events

The action is first read as a touch phase through
`PlatformWindow::touch_phase_from_android`:

| phase | what is delivered |
|---|---|
| `Moved`, `Cancelled` | **every** active pointer, because a move or a cancel names every contact |
| `Started`, `Ended` | only the pointer that changed, addressed by `motion.pointer_index()` |

A pointer index that is out of range for the pointer count is logged and
skipped.

A motion action that is *not* a contact phase is handled separately:

| action | what is delivered |
|---|---|
| `HoverMove` | `InputEvent::PointerMoved` for the first pointer |
| `ButtonPress` / `ButtonRelease` | `InputEvent::MouseButton` with the button from `PlatformWindow::mouse_button_from_android` |
| anything else (`OUTSIDE`, `SCROLL`, …) | dropped |

An unmapped mouse button is logged, not guessed at.

### Touch events are consumed, not forwarded

The raw touch is fed into the **touch → mouse synthesizer** ([gesture.md](gesture.md))
instead of being forwarded as `onTouchDown`/`onTouchMove`/`onTouchUp`. A KAG
menu resolves a click through `onMouseDown`/`onMouseUp` and hit-tests the
layer under the pointer, and such a menu never sees the raw-touch family —
forwarding both would fire the same interaction twice.

### Key events

`KeyAction::Down` and `KeyAction::Up` become
`InputEvent::Key { code, pressed }`; the code comes from
`PlatformWindow::physical_key_from_android`. The other key actions are
ignored.

`AKEYCODE_BACK` becomes `PhysicalKey::Escape`, so the Android back key
reaches the engine as Escape — the same key the two-finger back gesture
emits.

### Text and IME events

`TextEvent` and `TextAction` carry committed characters, not the raw key
state the engine's input model consumes, so they are **not translated**.

`android_activity::input::InputEvent` is non-exhaustive; an event the engine
does not consume is ignored rather than guessed at.

---

## 5. State ownership

The module owns no state. It reads the activity's queue and produces events;
the handler ([host.md](host.md)) owns the gesture machine and the pending
event queue, and the engine owns the input state.

The pointer id is carried into the synthesizer as a `u64`; a negative Android
pointer id is logged and ignored.

---

## 6. Platform gating

Android only. The module is `cfg(target_os = "android")`, because the
`android-activity` input types exist only there.

The **pure** part of the translation — `physical_key_from_android`,
`touch_phase_from_android`, `mouse_button_from_android` — lives in L1
(`platform::android`), names no Android type and is therefore host-tested.
See [`../platform/platform.md`](../platform/platform.md) §3.

---

## 7. Failure modes

| failure | behaviour |
|---|---|
| the input queue cannot be read | logged as an error and skipped — an unreadable queue must not stop the frame loop |
| the motion action does not fit in `i32` | logged and the event is dropped |
| the pointer index is out of range | logged and the event is dropped |
| the pointer id is negative | logged and the contact is ignored |
| the key code does not fit in `i32` | logged and the event is dropped |
| the mouse button is unmapped | logged and the event is dropped |
| the text/IME event | not translated |

Nothing here is fatal: a lost input event degrades the interaction, not the
run. Every dropped event is **logged**, so nothing is silent.

---

## 8. Tests

There is no dedicated test file for this module: it is Android-only, and the
`android-activity` input types are not available on the host. The parts that
*are* host-tested are the L1 translation functions it delegates to
(`crates/platform/platform/src/android.rs` `mod tests`): the Android back key
becoming Escape, the letter/digit/modifier mapping, an unmapped key keeping
its native code, the pointer actions masked to their phase and the
non-contact actions having no phase.

The gesture state machine this module feeds is host-tested in full — see
[gesture.md](gesture.md) §8.

**Not verified on the host:** that the Android motion actions and timestamps
actually arrive in the assumed order, and that the synthesised events hit a
real KAG menu.

---

## 9. Related documents

| topic | document |
|---|---|
| the Android composition root overview | [README.md](README.md) |
| the touch → mouse synthesizer this module feeds | [gesture.md](gesture.md) |
| the handler that receives the events | [host.md](host.md) |
| the L1 translation functions | [`../platform/platform.md`](../platform/platform.md) §3 |
| the reference Android input mapping | [`android.md`](../../../docs.old/android.md) §5 · snapshot [`docs.old/android.md`](../../../docs.old/android.md) |
| the L1↔L2 input protocol | [`../../rules/arch_rules.md`](../../rules/arch_rules.md) §5 |
