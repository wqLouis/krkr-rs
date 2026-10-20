# `compat::natives::tree_owner` — the `BitmapLayerTreeOwner` class

Source: `crates/compat/src/natives/tree_owner.rs` · layer **L3** · class
`BitmapLayerTreeOwner`

`BitmapLayerTreeOwner` is the **owner** of a layer tree: the object the layer
manager calls back into when the tree changes, and the object that forwards
input events to the focused layer.

The layer tree itself is L2's; this class is the L3 adapter. See
[`../../code/engine/scene.md`](../../code/engine/scene.md).

---

## 1. Registered surface

**Constructor:** `BitmapLayerTreeOwner()` binds the owner to the scene's
**first window**.

**Methods (31):**

* the constructor;
* nineteen `fire*` methods that deliver an input event to the focused layer;
* eleven `on*` owner hooks that the layer tree calls back into.

The `fire*` methods:

| method | payload | handler looked up |
|---|---|---|
| `fireClick` | `(x, y)` | `onClick` |
| `fireDoubleClick` | `(x, y)` | `onDoubleClick` |
| `fireMouseDown` | `(x, y, button, flags)` | `onMouseDown` |
| `fireMouseUp` | `(x, y, button, flags)` | `onMouseUp` |
| `fireMouseMove` | `(x, y, flags)` | `onMouseMove` |
| `fireMouseWheel` | `(shift, delta, x, y)` | `onMouseWheel` |
| `fireMouseOutOfWindow` | `()` | `onMouseLeave` |
| `fireTouchDown` | `(x, y, cx, cy, id)` | `onTouchDown` |
| `fireTouchUp` | `(x, y, cx, cy, id)` | `onTouchUp` |
| `fireTouchMove` | `(x, y, cx, cy, id)` | `onTouchMove` |
| `fireTouchScaling` | `(a, b, c, d, flag)` | `onTouchScaling` |
| `fireTouchRotate` | `(a, b, c, d, e, flag)` | `onTouchRotate` |
| `fireMultiTouch` | `()` | `onMultiTouch` |
| `fireKeyDown` | `(key, shift)` | `onKeyDown` |
| `fireKeyUp` | `(key, shift)` | `onKeyUp` |
| `fireKeyPress` | `(key)` | `onKeyPress` |
| `fireDisplayRotate` | `(orientation, rotate, bpp, hres, vres)` | `onDisplayRotate` |
| `fireRecheckInputState` | `()` | `onRecheckInputState` |
| `fireReleaseCapture` | `()` | `onReleaseCapture`, plus the owner hook `onReleaseMouseCapture` |

The eleven `on*` owner hooks are assignable no-op methods: `onChangeLayerImage`,
`onDisableAttentionPoint`, `onGetCursorPos`, `onReleaseMouseCapture`,
`onResetImeMode`, `onResizeLayer`, `onSetAttentionPoint`, `onSetCursorPos`,
`onSetHintText`, `onSetImeMode`, `onSetMouseCursor`.

**Properties (6):**

| member | access | value |
|---|---|---|
| `bitmap` | read-only | the lazily created internal render target (a 0×0 `Bitmap`) |
| `width` / `height` | read-only | the tree bounds: the primary layer's size if one is attached, else the owner window's inner size, else `0` |
| `focusedLayer` | read/write | the focused layer object, or `null` |
| `primaryLayer` | read-only | the primary layer object |
| `layerTreeOwnerInterface` | read-only | the owner script object itself |

---

## 2. Event dispatch

The dispatch target is the **focused layer**, falling back to the **primary
layer**. It is duplicated before the call, so a re-entrant handler (for
example one that clears `focusedLayer`) cannot invalidate the object
mid-call.

The handler is looked up **by name at dispatch time**, not through a
compile-time `Layer` dependency, so a script override wins.

The event is reported **consumed** only when the handler returns a truthy
value. TJS `operator bool` semantics apply: void is false, a number is
non-zero, a string is non-empty, an object is true. The port's no-op `Layer`
defaults return void, so an unimplemented event is *not* consumed.

### Payload fallbacks

A missing (or void) argument is filled from a snapshot of the L2 input state:
the pointer position, the `TVP_SS_*` shift mask, the wheel delta, the last
pressed key (converted to a virtual-key code through the L3 table in
[`input.md`](input.md)), the committed text input and the held mouse button.
Explicit arguments always win.

### The owner hooks

Only `onReleaseMouseCapture` is actually reached: `fireReleaseCapture`
consults it. The other ten correspond to layer-manager notifications that are
not part of the L2 service set yet, so they are assignable no-ops.

---

## 3. `focusedLayer`

The setter retains the layer and mirrors its id into the L2 scene focus, so
the scene stays the single source of truth. `null` or void clears the focus
and blurs the previously focused layer in its own window. A non-`Layer`
object is rejected.

The getter returns the focused layer object, or `null`.

---

## 4. State ownership

| state | owner |
|---|---|
| the owner window, the focused layer and its id, the primary layer, the internal bitmap | the **per-object payload** (`TreeOwnerInst`) |
| the layer tree, the focus state | the **L2 scene** |
| the pointer, the wheel, the shift mask, the last pressed key, the text input, the held buttons | the **L2 input state** |

There is no L1 seam: the platform event stream reaches the owner only
through the L2 input state.

---

## 5. Errors

| shape | example |
|---|---|
| `"BitmapLayerTreeOwner.<handler>: compat context is not bound"` | the engine binding is missing. The message names the **handler**, not the method: `BitmapLayerTreeOwner.onClick: compat context is not bound` |
| `"BitmapLayerTreeOwner.fireReleaseCapture: compat context is not bound"` | the same for the release-capture method, which has its own message |
| `"BitmapLayerTreeOwner.focusedLayer: missing value"` | a setter without a value slot |
| `"BitmapLayerTreeOwner.focusedLayer: the value is not a Layer"` | a non-`Layer` object |
| `"BitmapLayerTreeOwner.focusedLayer: {error}"` | the layer lookup failed |
| `"BitmapLayerTreeOwner.layerTreeOwnerInterface: {error}"` | the object lookup failed |
| `"Not have primary layer"` | `primaryLayer` when no primary layer is attached (the reference's exact text) |

A retained object that could not be duplicated is **logged**, not raised.

---

## 6. Tests

`crates/compat/tests/tree_owner.rs` covers the constructor exposing the full
surface and the owner's identity, the focused-layer setter mirroring into the
scene, the primary layer being read-only and the tree falling back to the
window, the dispatch reaching the focused layer and reporting consumption, the
arguments defaulting to the input state, the owner hooks being assignable
with a callable no-op default, the touch and wheel payloads reaching the
handlers, and the release-capture path consulting the owner hook.

Unit tests inside the module cover the payload defaults, the explicit
arguments winning over the snapshot, the truthiness rule and the hook names
matching the reference surface.

---

## 7. Known gaps

1. **`primaryLayer` raises in practice** (`Not have primary layer`): the
   port has no layer-manager wiring, so the primary layer is never
   established and the tree bounds always fall back to the window's inner
   size. The reference's read-with-`null` behaviour is unreachable.
2. **`layerTreeOwnerInterface` returns the object**, not the reference's
   integer pointer cast.
3. **There are no arity checks.** The reference raises a bad-parameter-count
   for each `fire*` method; the port accepts any arity and fills the gaps
   from the L2 input state.
4. **A reference bug is not reproduced**: the reference's `fireMouseMove`
   checks `numparams < 3` but reads the flags from the fourth parameter; the
   port reads the third.
5. **Ten of the eleven owner hooks are inert.**
6. **The `fire*` methods do not run the reference's coordinate transform**
   and do not go through the layer manager's hit test or mouse capture; they
   call the focused layer's handler directly.
7. **`fireKeyPress`'s fallback uses the first character of the committed text
   input** as a code point, where the reference takes a `tjs_char`.

---

## 8. Related documents

| topic | document |
|---|---|
| the L2 scene, the layer tree and the focus state | [`../../code/engine/scene.md`](../../code/engine/scene.md) |
| the L2 input state | [`../../code/engine/input.md`](../../code/engine/input.md) |
| the event adapter | [`../../arch/l3.md`](../../arch/l3.md) §6 |
| the keyboard and the VK mapping | [`input.md`](input.md) |
| the layer tree → frame contract | [`../../../docs.old/migration/12-layer-tree-and-frame.md`](../../../docs.old/migration/12-layer-tree-and-frame.md) |
