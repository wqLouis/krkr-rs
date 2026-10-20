# `engine::input` — the generic input state

Source: `crates/engine/src/input.rs`

L1 (`platform`) emits input events; L2 folds them into an explicit state value
that the rest of the engine and L3 read. The state is **generic** — pointer
position, buttons, wheel, key edges and multi-touch contacts — with no
TVP/VK semantics: the TVP `Mouse`/`Key` surface (VK codes, click-sequence
constants, shift flags) belongs to L3.

---

## 1. Responsibility

* Hold the per-frame input state: pointer, buttons, wheel, keys, touches and
  the text-input buffer.
* Implement the frame protocol so the per-frame edges (release, wheel
  accumulation, last-pressed list) are cleared at the right moment.
* Implement the generic multi-click sequence.

---

## 2. State it owns

```rust
pub struct InputState {
    pointer: (f32, f32),
    pointer_inside: bool,
    pub pointer_visible: bool,
    warp_request: Option<(f32, f32)>,
    buttons: HashMap<MouseButton, ButtonState>,
    wheel: f32,
    keys: HashMap<u32, KeyState>,
    last_pressed: Vec<u32>,
    touches: HashMap<u64, TouchContact>,
    text_input: String,
    frame: u64,
    click_sequence_frames: u64,
    click_max_move: f32,
}
```

| value | state |
|---|---|
| `ButtonState` | held, released-in-this-frame, the held-frame counter, the click count, the last click's position and frame |
| `KeyState` | held, released-in-this-frame, the held-frame counter |
| `TouchContact` | the latest touch point, the start position and the previous position |

`InputState` owns no global and no clock: all state is explicit.

---

## 3. Public API

### The frame protocol

```rust
pub fn begin_frame(&mut self);
pub fn end_frame(&mut self);
pub fn frame(&self) -> u64;
```

### Feeding

```rust
pub fn apply(&mut self, event: &InputEvent);
pub fn set_pointer(&mut self, x: f32, y: f32);
pub fn set_button(&mut self, button: MouseButton, down: bool);
pub fn set_button_raw(&mut self, button: MouseButton, down: bool);
pub fn set_key(&mut self, code: u32, pressed: bool);
pub fn add_wheel(&mut self, delta: f32);
pub fn warp_pointer(&mut self, x: f32, y: f32);
pub fn take_warp_request(&mut self) -> Option<(f32, f32)>;
pub fn set_click_sequence(&mut self, frames: u64, max_move: f32);
```

### Reading

```rust
pub fn pointer(&self) -> (f32, f32);
pub fn pointer_inside(&self) -> bool;
pub fn button_state(&self, button: MouseButton) -> Option<&ButtonState>;
pub fn is_button_down(&self, button: MouseButton) -> bool;
pub fn is_button_released(&self, button: MouseButton) -> bool;
pub fn button_hold_frames(&self, button: MouseButton) -> u32;
pub fn button_click_count(&self, button: MouseButton) -> u32;
pub fn button_last_click_pos(&self, button: MouseButton) -> Option<(f32, f32)>;
pub fn clear_click_sequence(&mut self, button: MouseButton);
pub fn wheel(&self) -> f32;
pub fn is_key_down(&self, code: u32) -> bool;
pub fn is_key_released(&self, code: u32) -> bool;
pub fn key_hold_frames(&self, code: u32) -> u32;
pub fn last_pressed(&self) -> &[u32];
pub fn held_keys(&self) -> impl Iterator<Item = u32> + '_;
pub fn released_keys(&self) -> impl Iterator<Item = u32> + '_;
pub fn text_input(&self) -> &str;
pub fn touch_point_count(&self) -> usize;
pub fn touch_point(&self, id: u64) -> Option<TouchPoint>;
pub fn touch_contact(&self, id: u64) -> Option<&TouchContact>;
pub fn touch_contacts(&self) -> impl Iterator<Item = &TouchContact> + '_;
pub fn touch_velocity(&self, id: u64) -> Option<(f32, f32)>;
pub fn touch_speed(&self, id: u64) -> Option<f32>;
```

### Constants and constructors

```rust
pub const DEFAULT_CLICK_SEQUENCE_FRAMES: u64 = 30;
pub const DEFAULT_CLICK_MAX_MOVE: f32 = 4.0;

pub fn new() -> Self;
pub fn with_click_sequence(click_sequence_frames: u64, click_max_move: f32) -> Self;
```

---

## 4. The frame protocol

```text
begin_frame            // clear the release flags, the wheel, the released
                      // keys, the last-pressed list and the text input
  apply(event) ...     // fold in every raw L1 event
end_frame              // bump the held counters, frame += 1
```

`begin_frame` preserves the held buttons and keys and the active touch
contacts; `end_frame` advances the held-frame counters exactly once per
frame.

---

## 5. Invariants

* **The pointer position is in VM pixels.** The composition root has already
  mapped it from the device before L3 sees it.
* **`apply` matches every `InputEvent` variant explicitly**; there is no
  catch-all arm.
* **A click sequence continues** when the new press is within
  `click_sequence_frames` frames **and** within `click_max_move` pixels of
  the previous press; otherwise the count restarts at 1.
* **The click count reads 0** once the sequence has timed out.
* **`set_button_raw` does not register a click** — it exists for the gesture
  synthesis that drives a virtual button.
* **A released key is dropped from the key map** at `begin_frame`, so
  `released_keys` reports only the current frame's releases.
* **The text input is cleared at `begin_frame`**, so it holds the current
  frame's characters only.
* **The wheel delta accumulates within the frame** and is cleared at
  `begin_frame`.
* **A warp request is taken, not read**: `take_warp_request` clears it, so
  one request is not replayed.
* **A touch contact keeps the previous position**, so the velocity is the
  delta between two updates.

---

## 6. Data flow

```
L1 platform events (device → VM pixels)
   │
   ▼
InputState::apply(event)        ── per event
   ...
InputState::begin_frame()       ── at the start of the frame
InputState::end_frame()         ── at the end of the frame
   │
   ▼
L3's event adapter ──► the script's onMouseDown / onMouseUp / onKeyDown …
   │
   ▼
Scene::layer_at (the hit-test, in VM pixels)
```

---

## 7. Failure modes

* **None.** This is state: every accessor is total, every setter is
  infallible, and an unknown button, key or touch id simply has no entry.
* A malformed event is not possible from the L1 side: `apply` matches the
  `InputEvent` variants exhaustively.

---

## 8. Tests

* `crates/engine/tests/input.rs` — the frame protocol, the button and key
  edges, the click sequence, the wheel and the touch contacts.

---

## 9. See also

| topic | document |
|---|---|
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the input model in the architecture | [`arch/l2.md`](../../arch/l2.md) §8 |
| the three coordinate spaces and the device → VM input mapping | [`arch/overall.md`](../../arch/overall.md) §3 |
| the hit-test that consumes the pointer | [scene.md](scene.md) |
| the frame that owns this state | [engine.md](engine.md) |
| the archived protocol trail | [`docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) §6.3 |
