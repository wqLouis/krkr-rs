# `natives/visual/window.rs` — the `Window` class

**Source:** `crates/compat/src/natives/visual/window.rs`

`Window` is the script-visible window: the client rectangle, the chrome
facts, the layer tree root, the focus owner and the event source.

The registered surface is 136 members: the core `Window` class plus the
`windowEx.dll` plugin's additions, which are attached to the same class
object. (The surface dump lists 138 entries for `Window` — the class-name
constructor and the VM's `finalize` are two of them.)

---

## 1. Construction and teardown

| Phase | What happens |
|---|---|
| **native create** | allocates the payload and **already creates the L2 scene window** at the default client size `1280 × 720`. The reference's `CreateNativeInstance` returns a window whose fields are usable, so a script subclass may touch a `Window` member **before** `super.Window(…)`. |
| **the `Window(…)` constructor** | raises `"Window: this window is already constructed"` on a second call; registers the script object in the object registry; installs the **default draw device** by evaluating `new Window.BasicDrawDevice()`; writes the scene window id into the out slot. A draw device that cannot be constructed raises `"Internal error"` (the reference's `TVPInternalError`). |
| **`Invalidate`** | invalidates every object registered through `Window.add`, drops the draw-device reference, then removes the window record from the scene. The window's layer script objects are **not** invalidated. |
| **native destroy** | releases the `Window.add` vector and removes the window record from the scene. The layer records stay: each one is released by its own script object's finalizer. |
| **`close()`** | drops the form first, then releases the registered objects and removes the window record. The layer records stay. A second `close()` raises `"Window.close: this window is not constructed"`. |

---

## 2. The members

### Geometry and the client size

| Member | Behaviour |
|---|---|
| `width`, `height`, `innerWidth`, `innerHeight` | the **window client size**. The setters write one axis each. |
| `setSize(w, h)`, `setInnerSize(w, h)` | write the client size. Negative values clamp to `0`. Both are **no-ops before the constructor has run**. |

The client size is **not** the logical screen: the screen is the primary
layer's rect, and `System.screenWidth`/`screenHeight` follow the primary
layer.

### Position, constraints and zoom

| Member | Behaviour |
|---|---|
| `left`, `top`, `setPos(left, top)` | the window origin. |
| `minWidth`, `minHeight`, `setMinSize(w, h)` | the minimum size constraint. |
| `maxWidth`, `maxHeight`, `setMaxSize(w, h)` | the maximum size constraint. |
| `zoomNumer`, `zoomDenom`, `setZoom(numer, denom)` | the zoom ratio. Both values are clamped to ≥ 1 and the ratio is stored verbatim — **no GCD reduction** — and it **resizes nothing**. |

The min and max sizes are stored but **never clamp** the client size.

### Visibility and the chrome facts

| Member | Behaviour |
|---|---|
| `visible` | the L2 window's visibility. |
| `caption` | the L2 window's title. |
| `focusable` | defaults to `true`. |
| `fullScreen`, `waitVSync`, `enableTouch`, `useMouseKey`, `trapKey`, `hintDelay` (default 500), `mouseCursorState` (default 0), `imeMode`, `borderStyle` (default 0 = `bsNone`), `stayOnTop`, `touchScaleThreshold` / `touchRotateThreshold` (default `5.0`) | pure round-trip facts in the L3 payload. |

### The fixed and Win32 facts

| Member | Value |
|---|---|
| `layerLeft`, `layerTop` | `0` — the setters are no-ops. |
| `innerSunken` | `false` — the setter is a no-op. |
| `showScrollBars` | `true` — the setter is a no-op. |
| `displayOrientation` | `2` (`orientLandscape`), read-only. |
| `displayRotate` | `0`, read-only. |
| `HWND` | the native instance pointer as an integer — a **stable synthetic handle**, not an OS handle. Read-only. |

These mirror the reference form's constant getters and empty setters.

### The layer tree and the focus

| Member | Behaviour |
|---|---|
| `primaryLayer` | the window's primary layer **object**. Read-only. It **raises** `"Window has no layer"` when the window has no primary layer — `null` only when the primary layer has no owner. |
| `focusedLayer` | the focused layer object or `null`. Assigning focuses the layer **in the window's input domain** regardless of the layer's own domain, or clears the focus on `null`/void. |
| `layerTreeOwnerInterface` | the window object itself — the L3 stand-in for the `iTVPLayerTreeOwner`. Read-only. |

### Object lifetime

| Member | Behaviour |
|---|---|
| `add(obj)` | retains the object and ties its lifetime to the window. Adding the same closure twice is a **no-op** (the identity is the object pointer plus the closure receiver). |
| `remove(obj)` | unregisters it. |

Neither touches the layer tree.

### The host requests

| Member | Behaviour |
|---|---|
| `bringToFront()` | pushes the window id onto the host request queue. |
| `update([type])` | marks **every layer of the window** as needing a repaint and pushes `(window_id, update_type)` onto the host request queue. |

### Input, touch and the pointer

| Member | Behaviour |
|---|---|
| `postInputEvent(name[, params])` | accepts only `onKeyDown`, `onKeyUp` and `onKeyPress`; it builds the same TJS event a real input event produces and dispatches it to the window and its focused layer. |
| `getTouchPoint(id)` | the reference dictionary `{startX, startY, x, y, ID}`. The argument may be a contact id **or** a 0-based index into the contacts sorted by id. An unknown contact **raises**. |
| `getTouchVelocity(id)` | `{x, y, speed}`, or **`null`** for an unknown contact. |
| `touchPointCount` | the L2 contact count. |
| `getMouseVelocity(x, y, speed)` | writes the out-parameters and returns whether the pointer moved. The sample is per window. |
| `resetMouseVelocity()` | clears the per-window sample. |

### The menu and the draw device

| Member | Behaviour |
|---|---|
| `menu` | the window's root `MenuItem`, built as `new MenuItem(this, this)` on the first read and **cached per window**. Read-only — writing raises the VM's read-only error. |
| `drawDevice` | the stored device object or `null`. Assigning a non-object is ignored; an object whose `interface` cannot be read, or is `0`, **raises**. The previous device is invalidated after the replacement is stored. |

### The inert Win32 actions

| Member | Behaviour |
|---|---|
| `showModal([modal])` | records a modal flag. |
| `setMaskRegion(threshold)`, `removeMaskRegion()` | record and clear a mask threshold. |
| `hideMouseCursor()` | sets `mouseCursorState = 2`. |
| `setLayerPos(l, t)` | requires 2 arguments and does nothing. |
| `beginMove()` | does nothing. |
| `findFullScreenCandidates(...)` | requires 5 arguments and returns void (the reference's body is compiled out). |
| `registerMessageReceiver(mode, proc, userdata)` | records the `(proc, userdata)` pair. Mode `0` appends, mode `1` removes the first matching pair, any other mode is a no-op. |

### The static member

`Window.mainWindow` is a **class-object** property — the **first** window of
the scene, or `null` when the scene has none. Reading it on an *instance*
raises.

### The event handlers

23 `on*` action handlers plus `onCloseQuery`:

`onActivate`, `onDeactivate`, `onResize`, `onMouseEnter`, `onMouseLeave`,
`onClick`, `onDoubleClick`, `onMouseDown`, `onMouseUp`, `onMouseMove`,
`onMouseWheel`, `onTouchDown`, `onTouchUp`, `onTouchMove`, `onTouchScaling`,
`onTouchRotate`, `onMultiTouch`, `onKeyDown`, `onKeyUp`, `onKeyPress`,
`onPopupHide`, `onFileDrop`, `onDisplayRotate`.

Each applies the reference arity check, then builds the reference **event
dictionary** (`type`, `target`, and one member per positional argument) and
calls `objthis.action(event)`. A missing `action` member is ignored; a script
assignment (`win.onClick = function…`) shadows the native default.

The event dictionary is the empty-array literal, evaluated through the
script cache, so the literal is compiled once instead of once per event
(`reference/cpp/core/tjs2/tjsScriptCache.cpp:135-190`). The reference builds
the object natively (`reference/cpp/core/base/EventIntf.cpp:709-736`).

`onCloseQuery` is registered as a **no-op** — it does not dispatch an
`action` event.

---

## 3. State ownership

| State | Owner |
|---|---|
| the window id, the title, the client size, the visibility, the opacity | L2 `Scene` / `WindowState` |
| the primary layer, the focused layer, the focus domains, the layer list | L2 `Scene` / `WindowState` |
| the layer tree, the bitmaps, the fonts | L2 `Scene` |
| the position, the min/max sizes, the zoom, the chrome flags, the modal flag, the mask region, the message receivers, the draw-device object, the cached menu root, the `attached` flag | the L3 `WindowInst` payload |
| the scene id → script-object mapping, the `Window.add` vector, the request queues, the per-window mouse-velocity sample | L3 `VisualState` |
| the pointer and the touch contacts | L2 `InputState` |

---

## 4. Invariants

1. The **first** layer attached to a window becomes its primary layer and is
   **forced visible**. A window with no primary layer makes `primaryLayer`
   raise. The primary layer refuses to be moved.
2. The **client size** and the **logical screen** are separate: the client
   size is `WindowState.inner_size`; the screen is the primary layer's rect.
3. A `setSize` before the constructor has run is a **no-op** — the reference
   form does not exist until `Construct`.
4. A second `Window(…)` on the same payload raises.
5. The min/max sizes are stored verbatim and never clamp the client size.
6. `setZoom` requires 2 arguments, clamps both to ≥ 1 and resizes nothing.
7. `mainWindow` is the first scene window; it becomes `null` after the window
   is invalidated or closed.
8. `focusedLayer` reads and writes the **input domain** only; a target that
   is not node-focusable is rejected before any state change, and no
   `onBlur`/`onFocus` fires.
9. `close()`, `Invalidate` and the native destroy all release the
   `Window.add` objects (running their `finalize`) and remove the window
   record. None of them touches the window's layers: a layer's native state
   belongs to the layer's script object, so its record — and the finalizer
   that still writes to it — outlive the window.
10. Adding the same closure to `Window.add` twice is a no-op.
11. `Window.drawDevice` must be an object with a non-zero `interface`; the
    window always has one after construction.

---

## 5. Errors

| Message | Raised when |
|---|---|
| `"Window: this window is already constructed"` | the constructor ran twice |
| `"Window.close: this window is not constructed"` | `close()` on an unconstructed window |
| `"Window.<member>: this window is not constructed"` | `update`, `add`, `remove`, `setSize`, `getMouseVelocity`, `resetMouseVelocity` before the constructor |
| `"Window has no layer"` | `primaryLayer` on a window with no primary layer |
| `"Window.<method> requires N argument(s)"` | the arity checks |
| `"Window.drawDevice: cannot retrieve the interface from the draw device"` | the assigned object's `interface` is unreadable or `0` |
| `"Window.menu: the root is not an object"` | the cached root could not be built |
| `"Window.layerTreeOwnerInterface: {error}"`, `"Window.focusedLayer: {error}"` | the object could not be produced |
| `"Specified event {name} is unknown"` | `postInputEvent` with a name other than the three key events |
| `"Specified event {name} needs parameter"` | a missing or non-object `params` |
| `"Specified event {event} needs parameter {param}"` | a missing `key`/`shift` member |
| `"value is an object, not an integer"` | `key`/`shift` holds an object |
| `"Window.getTouchPoint: no such touch contact"` | an unknown contact |
| `"Internal error"` | the default draw device could not be constructed |
| `"Window.<member>: compat context is not bound"` | the binding is not installed |
| `"visual: no scene attached"`, `"visual: no input state attached"` | the host attached no scene / input state |

---

## 6. Host seams

| Seam | Where | What the host does |
|---|---|---|
| `take_bring_to_front_requests(&services)` | re-exported from `compat` | drain the `bringToFront` requests |
| `take_update_requests(&services)` | re-exported from `compat` | drain the `update` requests |
| the client size | `WindowState.inner_size` | the L2 engine compares the client size with the last observed one and raises a pending resize, which the composition root turns into an OS resize. This is the **only** window fact with a working end-to-end host path. |
| `postInputEvent` | the event adapter | the host must have the adapter bound |
| the touch getters | the L2 `InputState` | the host feeds the platform touch events into it |
| `getMouseVelocity` | the L2 `InputState` | the host feeds the pointer moves into it |

`setMaskRegion`, `removeMaskRegion`, `hideMouseCursor`, `showModal`,
`setZoom`, `setPos`, `setMinSize`, `setMaxSize`, `fullScreen`, `visible` and
`caption` have **no host path**: they are recorded in L3 or L2 and nothing
reads them yet.

---

## 7. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/window.rs` | the event handlers exist and are callable at the reference arity; a script assignment shadows them; the mouse velocity samples the L2 pointer and resets; the host-fact properties round-trip; `mainWindow` is readable on the class object; the Win32 host facts return plausible values; the menu is a cached root and read-only; the draw device installs a default and round-trips; the message receivers register and unregister; the tree-owner interface is the window object; `postInputEvent` delivers to the window and the focused layer and rejects unknown names; the obsolete members are registered; a subclass may call a member before the base constructor; dropping the last reference, invalidating and closing all release the scene tree and the registered objects. |
| `crates/compat/tests/window_size.rs` | the client size and the logical screen are separate; the screen follows the primary layer at the real game sizes; `setZoom` stores the ratio only. |
| `crates/compat/tests/window_ex.rs` | the `windowEx` additions: the window rect members, the hit-test constants, the inert actions, the message hook, the notification table, the overlay bitmap validation, the ex events, and that `disableMove`/`resetExSystemMenu` are **not** registered. |
| `crates/compat/tests/visual.rs` | the scene round-trips; `primaryLayer` is the real object and raises without a layer; the focus and the children round-trip; `Window.add` does not reparent; `close()` empties the scene; the touch getters read the L2 input state; the draw-device classes are attached to `Window`. |
| `crates/compat/tests/dispatch_ui.rs` | a `with`-style write from a foreign receiver lands on the target window. |
| `crates/compat/tests/focus_behaviour.rs` | the focus rules, including that a mouse click never moves the layer focus. |

---

## See also

* [`draw_device.md`](draw_device.md) — the two draw-device classes
* [`layer.md`](layer.md) — the layers the window owns
* [`focus.md`](focus.md) — the focus model `focusedLayer` uses
* [`../event.md`](../event.md) — the event delivery to the window
