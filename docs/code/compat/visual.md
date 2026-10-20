# `natives/visual/` — the visual families

**Source:** `crates/compat/src/natives/visual/`

The visual families are `Window`, `Layer`, `Bitmap`, `Font` and `Timer`,
plus the two draw-device classes. They compose the L2 `engine_api::Scene` and
keep no engine state of their own.

---

## 1. The architecture

The native class is constructed by `new Window()` / `new Layer(…)`; the
**class-name method** (`Window`, `Layer`, …) is the constructor hook and
returns the new scene id. The instance payload holds the scene id; all
persistent state is read and written through the L2 `Scene` API, so the model
is never duplicated.

Cross-object properties (`Window.primaryLayer`, `Layer.parent`,
`Layer.window`, `Layer.children`, …) return **real** TJS objects. The mapping
from a scene id to the retained script object is the `ObjectRegistry` in
`VisualState`; each class registers its `objthis` at construction and removes
it on destroy.

---

## 2. The pages

| Page | Module | Covers |
|---|---|---|
| [`common.md`](visual/common.md) | `common.rs` | the shared dispatch state, the value marshalling and the object ↔ scene-id resolution |
| [`window.md`](visual/window.md) | `window.rs` | the `Window` class |
| [`layer.md`](visual/layer.md) | `layer/mod.rs`, `layer/order.rs` | the `Layer` class, the ordering and the modal stack |
| [`focus.md`](visual/focus.md) | `layer/focus.rs` | the focus domain model |
| [`pixels.md`](visual/pixels.md) | `layer/pixels.rs` | the pixel operations, the image load/save path and the text path |
| [`transitions.md`](visual/transitions.md) | `layer/transitions.rs` | `beginTransition`, the transition pumps and the script cache |
| [`bitmap.md`](visual/bitmap.md) | `bitmap.rs` | the `Bitmap` class and the image-codec seam |
| [`font.md`](visual/font.md) | `font.rs` | the `Font` class and the face resolution |
| [`timer.md`](visual/timer.md) | `timer.rs` | the `Timer` class and the L2 timer schedule |
| [`draw_device.md`](visual/draw_device.md) | `draw_device.rs` | `BasicDrawDevice` / `PassThroughDrawDevice` |

---

## 3. The registration order

```rust
window::register_window_class(ctx)?;
draw_device::register_draw_devices(ctx)?;   // needs the Window class object
layer::register_layer_class(ctx)?;
bitmap::register_bitmap_class(ctx)?;
font::register_font_class(ctx)?;
timer::register_timer_class(ctx)?;
```

The draw devices are registered **after** `Window`, because both classes are
attached to the `Window` class object — that is what makes
`new Window.BasicDrawDevice()` work.

---

## 4. The shared conventions

| Concern | Rule |
|---|---|
| the payload | holds the scene id and a couple of small flags; nothing else |
| the "not constructed" error | `"<Class>: this <thing> is not constructed"` when the payload has no id — the native constructor has not run |
| the "gone" error | `"<Class>: this <thing> no longer exists"` when the scene record was removed |
| the unbound error | `"<Class>: compat context is not bound"` when the binding is not installed |
| argument arity | the message is `"<Class>.<method> requires N argument(s)"`; the counts mirror the reference's `TJS_BEGIN_NATIVE_METHOD_DECL` |
| the void/null distinction | a member with no value returns `null`, not `void` — a script guard like `if (ret != null) ret.mode` passes for `void` and then throws on the member access |
| the receiver | an instance member resolves its payload **only** from `objthis`; a `with`-style write from a foreign receiver lands on the member's object, not the receiver's |
| subclass overrides | every event forwarder resolves the member **on the object**, so a script override wins |

The `void-getter` gate check enforces the void/null rule: a stored value
whose constructor default is `void` must be mapped to the reference's
non-void value before it reaches the script. A reference property that
genuinely reads back void is listed in the gate's allowlist.

---

## 5. The deliberate deferrals

These members are **not registered** — calling them raises a TJS "member not
found", never a silent stub:

* `Layer.colorize` / `Layer.noise` and the remaining `operate*`/`blend*`
  pile variants. The L2 `layer_ops` already implements the math
  (`colorize`, `noise`, `operate_rect`, …); wiring them here is a mechanical
  follow-up rather than a missing primitive.

---

## 6. State ownership at a glance

| State | Owner |
|---|---|
| the layer tree, the bitmaps, the fonts, the focus, the projection order | L2 `Scene` |
| the generic input state | L2 `InputState` |
| the timer schedule | L2 `TimerState` |
| the scene id → script-object mapping, the `Window.add` vector, the `Timer` bindings, the host request queues | L3 `VisualState` |
| the per-object state (the scene id, the cached `Font` wrapper, the pending cursor X) | the L3 native payload |
| the transition runtime, the modal stack, the capture-release queues, the cache revisions | L3 `LayerExtras` |

---

## See also

* [`../natives.md`](natives.md) — the non-visual families
* [`../engine_api.md`](engine_api.md) — the L2 items the visual family composes
* [`../../arch/l3.md`](../../arch/l3.md) §3 — the native classes at the layer level
* [`../../arch/l2.md`](../../arch/l2.md) §2 — the logical layer tree
