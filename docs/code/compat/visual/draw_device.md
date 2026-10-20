# `natives/visual/draw_device.rs` — the draw-device classes

**Source:** `crates/compat/src/natives/visual/draw_device.rs`

`BasicDrawDevice` and `PassThroughDrawDevice` are the two script-visible
draw devices. Both are registered as **top-level** classes and then
**attached to the `Window` class object**, so a script writes
`new Window.BasicDrawDevice()`.

There is no draw-device object on this host: the device classes are pure L3
state with no L2 counterpart.

---

## 1. The members

| Member | Kind | Behaviour |
|---|---|---|
| `interface` | read-only property | the native draw-device pointer as an integer. There is no draw-device object on this host, so the getter reports the **stable native payload address** — a non-zero synthetic pointer that does not change between reads. |
| `drawer` | read-only property | the drawer currently in use. |
| `preferredDrawer` | read/write property | the drawer the script asked for. The setter casts through `i32`, exactly like the reference's `(tjs_int)` truncation: `2.7` → `2`, `"4"` → `4`, `2^32` → `0`. |
| `dtNone` | static, `0` | |
| `dtDrawDib` | static, `1` | |
| `dtDBGDI` | static, `2` | |
| `dtDBDD` | static, `3` | |
| `dtDBD3D` | static, `4` | |
| `recreate()` | method | `BasicDrawDevice.recreate` is compiled out in the reference and is a **no-op** here — the drawer stays `dtDrawDib`. `PassThroughDrawDevice.recreate` models `SetToRecreateDrawer()` + `EnsureDrawer()`: the drawer becomes the software drawer `dtDrawDib` and the locked source size is read. |
| `lockTouchSize(w, h)` | method — **PassThrough only** | stores the forced source size (the reference's `SetLockedSize`). Requires 2 arguments. `BasicDrawDevice` has **no** `lockTouchSize` member at all, so the reference raises for it. |

Neither class registers a `name` member — reading it raises.

### The constructors

The reference constructor bodies are empty; both classes share a no-op
constructor.

### The payload defaults

```rust
struct DrawDeviceInst {
    drawer: i64,
    preferred_drawer: i64,
    locked: (i64, i64),
}
```

| Class | `drawer` | `preferredDrawer` | `locked` |
|---|---|---|---|
| `BasicDrawDevice` | `dtDrawDib` (1) | `dtDrawDib` (1) | `(0, 0)` |
| `PassThroughDrawDevice` | `dtNone` (0) | `dtNone` (0) | `(-1, -1)` |

`src_size()` reports the locked size only when **both** components are > 0;
otherwise `(0, 0)`.

---

## 2. The `dt*` constants

They are registered as **static** members on the class object, because the
port's instance-property dispatch requires a native payload that a
class-object access does not have, and `tTJSNativeClass::FuncCall` skips
static members when copying them onto instances. The documented use is
therefore class-qualified:

```tjs
Window.PassThroughDrawDevice.dtNone
```

---

## 3. State ownership

The **entire** state is in the L3 native payload. There is no L2 state and
`VisualState` is not involved.

---

## 4. Invariants

* The `dt*` constants are class-qualified statics.
* `drawer` is read-only — a write raises rather than mutating.
* `preferredDrawer` truncates to 32 bits like the reference's cast.
* `interface` is the payload address: stable for the object's life and
  non-zero.
* The two classes are registered top-level **and** attached to the `Window`
  class object.
* `Window`'s constructor creates a default `BasicDrawDevice` and raises
  `"Internal error"` when it cannot.

---

## 5. Errors

| Message | Raised when |
|---|---|
| `"PassThroughDrawDevice.lockTouchSize requires 2 arguments"` | fewer than 2 arguments |
| `"Internal error"` | the `Window` constructor could not create the default device |
| `CompatError::Engine("draw_device: {member} is not a class object")` | the attach target is not a class object |
| `CompatError::Engine("draw_device: cannot resolve {member}: {error}")` | the class object could not be resolved |
| `CompatError::Engine("draw_device: cannot attach {name} to {target}: {error}")` | the attach failed |

---

## 6. Host seams

None. The draw devices are pure L3 state; the only host interaction is
indirect — the `Window` constructor resolves `Window.BasicDrawDevice`
through the VM and creates an instance.

---

## 7. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/layer_behaviour.rs` | `basic_draw_device_defaults_and_recreate`, `passthrough_draw_device_defaults_and_recreate`, `preferred_drawer_round_trips_and_truncates`, `drawer_is_read_only`, `draw_device_constants_are_class_qualified`, `interface_getter_is_a_synthetic_pointer`, `invalidated_draw_device_refuses_member_access`, `lock_touch_size_requires_two_arguments` (and that `BasicDrawDevice` has no `lockTouchSize` at all). |
| `crates/compat/tests/visual.rs` | `draw_device_classes_are_attached_to_window` (including that `b.name` raises), `draw_device_state_round_trips_and_interface_is_a_synthetic_pointer`, `default_draw_device_creation_failure_raises_internal_error`. |
| `crates/compat/src/natives/visual/draw_device.rs` (unit test) | `reference_defaults_match_the_headers`. |

---

## See also

* [`window.md`](window.md) — the `Window.drawDevice` property and the default device
* [`../engine_api.md`](../engine_api.md) — the L2 items (there is no draw-device state on L2)
