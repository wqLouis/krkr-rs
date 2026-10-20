# `compat::natives::input` — the `Mouse`, `Key` and `Pad` classes

Source: `crates/compat/src/natives/input.rs` · layer **L3** · classes `Mouse`,
`Key`, `Pad`

These are the script's static views of the input devices. All of them are
**static** classes: the members do not depend on a receiver, and a script can
call them from anywhere.

The `Controller` class is registered by the debug module, not here — see
[`debug.md`](debug.md).

The generic input state is L2's; see
[`../../code/engine/input.md`](../../code/engine/input.md).

---

## 1. Registered surface

### `Mouse` — 14 methods, no properties

| member | behaviour |
|---|---|
| `getCursorPos([obj])` | the pointer position as `"<x>,<y>"`; with an object argument it fills `obj.x` / `obj.y` and returns void |
| `getCursorX()` / `getCursorY()` | the pointer position, rounded |
| `setCursorPos(x, y)` | writes the pointer and queues a host cursor warp |
| `getWheelRot()` | the vertical wheel delta |
| `getWheelRotY()` | the same value |
| `getWheelRotX()` / `getWheelRotZ()` | always `0` |
| `isVisible()` / `setVisible(visible)` | the pointer-visibility flag |
| `getPressed(button)` | whether the button is held |
| `getReleased(button)` | whether the button was released during this frame |
| `getRepeat(button)` | how many consecutive frames the button has been held |
| `getClickCount(button)` | the press count of the current multi-click sequence |

The button indices are `mbLeft=0`, `mbRight=1`, `mbMiddle=2`, `mbX1=3`,
`mbX2=4`. An out-of-range index is **not** an error: `getPressed` returns
false, `getReleased` false, `getRepeat` and `getClickCount` `0`.

### `Key` — 3 methods and 185 read-only constants

| member | behaviour |
|---|---|
| `getPressed(code)` | whether the virtual key is held |
| `getReleased(code)` | whether it was released during this frame |
| `getRepeat(code)` | how many consecutive frames it has been held |

The 185 `kXXX` constants are read-only properties holding the Windows
virtual-key code: the letters and digits, the function keys, the editing and
navigation keys, the numpad, the media and browser keys, the OEM keys, the
**side-specific** modifiers (`kLShift=0xA0`, `kRShift=0xA1`, `kLControl`,
`kRControl`, `kLMenu`, `kRMenu`) and the gamepad codes (`kPadLeft=0x1B5` …
`kPad10=0x1C9`, `kPadAny=0x1DF`).

A constant read never fails.

### `Pad` — 1 method and 24 read/write properties

`Pad` is the reference's **text-editor placeholder**, not a gamepad class.
Every property has a getter and a setter, and **every setter is a no-op**:
the value is accepted and discarded, exactly as in the reference, whose
property bodies are all `return TJS_S_OK`.

The defaults are fixed: `text`/`fileName`/`title`/`fontFace`/`statusText` are
`""`, `color`/`fontColor`/`left`/`top`/`borderStyle` are `0`,
`fontHeight`/`fontSize` are `10`, `width`/`height` are `100`, `opacity` is
`255`, `readOnly` is true, and `visible`, `fontBold`, `fontItalic`,
`fontUnderline`, `fontStrikeOut`, `wordWrap`, `showStatusBar` and
`showScrollBars` are false.

Only the five string getters can fail; the integer and boolean getters have
no error path.

One more member, `registerExEvent`, is attached to the same class object by
the **windowEx plugin**.

---

## 2. The virtual-key mapping

**The whole mapping lives in L3**, in `input.rs`:

* `PHYSICAL_VK_TABLE` maps each L1 physical key to its Windows VK code. The
  left and right modifiers collapse onto the generic codes — `ShiftLeft` and
  `ShiftRight` both give `0x10`, `ControlLeft`/`ControlRight` both `0x11`,
  `AltLeft`/`AltRight` both `0x12`.
* `vk_from_physical` / `physical_from_vk` convert in both directions.
* `vk_from_index` turns a stored physical-key index back into a VK code (the
  tree owner needs this for its event payload).
* `vk_pressed` / `vk_released` / `vk_repeat` are the query kernels. They
  handle the mouse-button VK codes (`0x01`, `0x02`, `0x04`, `0x05`, `0x06`),
  `VK_PADANY` ("any pad button held"), a concrete `VK_PAD*`, the generic
  modifier codes ("either side") and everything else through
  `physical_from_vk`.

L2's input state knows nothing about VK: it keys its map by an opaque code.

`System.getKeyState` uses the same mapping — see [`system.md`](system.md).

---

## 3. The click sequence

The multi-click policy is L3's, expressed through L2's generic mechanism:
`EventAdapter::begin_input_frame` pushes a click-sequence window of 30 frames
and a 4-pixel movement tolerance into the input state at the start of every
frame. `Mouse.getClickCount` reads the resulting count.

---

## 4. State ownership

| state | owner |
|---|---|
| the pointer, the wheel, the held/released/repeat buttons, the click sequence, the pointer visibility, the key states, the committed text input | the **L2 input state** |
| the VK table, the button-index table, the `TVP_SS_*` bits, the click-sequence policy | the **L3 module** (constants and pure functions) |
| the `Pad` property values | the **per-object payload** (`PadState`) |

The L2 input state is attached to the compat services by the host; the
natives read it through the attached pointer, never through a process
global.

---

## 5. Errors

| shape | example |
|---|---|
| `"Mouse.<member> requires N argument(s)"` | `Mouse.setCursorPos requires 2 arguments` |
| `"Mouse.<member>: compat context is not bound"` | the engine binding is missing |
| `"Mouse.<member>: input state is not attached"` | no L2 input state |
| `"Mouse.getCursorPos: cannot retain object"` | the argument object could not be retained |
| `"Key.<member>: compat context is not bound"` | the engine binding is missing |
| `"Key.<member>: input state is not attached"` | no L2 input state |
| `"Pad.<name>: compat context is not bound"` | the engine binding is missing (string getters only) |

The shared integer conversion errors are reported unprefixed: `value is null,
not an integer`, `value is an object, not an integer`, `value is an octet, not
an integer`, `value cannot be converted to an integer`.

---

## 6. Tests

`crates/compat/tests/input.rs` covers the mouse state fed from events (the
cursor, pressed/repeat, the wheel, the one-frame release visibility, the
click count), `setCursorPos` writing the state and queuing the warp, the
object-filling form of `getCursorPos` (including a missing member being
created and a non-object falling back to the string form), the pointer
visibility and its default, the key lifecycle through `Key.kReturn`, the
mouse-button and pad VKs, spot-checked key constants, and an unknown code
returning false rather than raising.

`crates/compat/tests/dispatch_ui.rs` pins that the mouse and key statics are
receiver-independent and that the pad properties are constants.

Unit tests inside the module cover the VK lookup for mouse and pad keys, the
button-index mapping, the click-sequence policy, and the unknown-code rule.

---

## 7. Known gaps

1. **`getWheelRotX` and `getWheelRotZ` are hard-coded `0`.** The generic
   wheel is vertical-only.
2. **`Mouse.getWheelRot` returns the raw wheel accumulation**, whereas the
   *event* path scales by the reference's 120-unit notch — two different
   units for the same concept.
3. **`Mouse.setVisible` is state-only**: there is no OS cursor show/hide.
4. **The reference's `Mouse`/`Key` source is not vendored**, so the wheel
   sign and axis, the click-count and the repeat timing are **not verified
   against the reference**. The test file says so in a banner.
5. **A negative key code becomes a large unsigned value** and simply never
   matches — false / `0`, not an error.
6. **`Pad` is inert**: no editor, no file I/O, no cursor, no selection.

---

## 8. Related documents

| topic | document |
|---|---|
| the L2 input state and the frame protocol | [`../../code/engine/input.md`](../../code/engine/input.md) |
| the event adapter and the VM-visible events | [`../../arch/l3.md`](../../arch/l3.md) §6 |
| the layer tree owner's event dispatch | [`tree_owner.md`](tree_owner.md) |
| `System.getKeyState` | [`system.md`](system.md) |
