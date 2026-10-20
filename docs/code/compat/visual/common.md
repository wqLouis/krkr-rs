# `natives/visual/common.rs` — the shared visual helpers

**Source:** `crates/compat/src/natives/visual/common.rs`

Everything the visual families share: the per-family dispatch state, the
value marshalling, the object ↔ scene-id resolution and the layer
re-parenting.

---

## 1. The dispatch state

```rust
pub(crate) struct VisualState {
    pub(crate) objects: ObjectRegistry,
    pub(crate) window_objects: HashMap<u32, Vec<WindowObject>>,
    pub(crate) timers: HashMap<TimerId, TimerBinding>,
    pub(crate) bring_to_front: Vec<u32>,
    pub(crate) update_requests: Vec<(u32, i32)>,
    pub(crate) scratch: RefCell<Vec<u8>>,
}
```

It lives in `CompatServices::state::<VisualState>()` — dispatch state, never
a process global.

| Field | Purpose |
|---|---|
| `objects` | the scene id → script-object mapping for windows and layers |
| `window_objects` | the `Window.add` registrations, in registration order |
| `timers` | the L3 payload per L2 timer id |
| `bring_to_front` | the `Window.bringToFront` requests for the host to drain |
| `update_requests` | the `Window.update` requests `(window_id, update_type)` |
| `scratch` | the reusable `out.string` buffer |

### `ObjectRegistry` and `ObjectHandle`

```rust
pub(crate) struct ObjectRegistry {
    pub(crate) windows: HashMap<u32, ObjectHandle>,
    pub(crate) layers: HashMap<u32, ObjectHandle>,
}

pub(crate) struct ObjectHandle { raw: usize }
```

The registry holds **non-owning** handles: the script object must be able to
reach reference count zero and run its invalidate/destructor when its script
references drop. A lookup retains the object only for the handle it returns.

This is the one cross-object mapping the visual natives need: it lets
`Window.primaryLayer`, `Layer.parent`, `Layer.window`, `Layer.children` and
`Layer.getLayerAt` hand back the **real** object the script created.

---

## 2. The `WindowObject` vector

One object registered with a window through `Window.add`, mirroring the
reference's `ObjectVector`.

| Field | Purpose |
|---|---|
| `identity` | `(object pointer, closure receiver)` — the identity of the registered closure, with `0` for a plain object. It makes a repeated `Window.add` of the same closure a no-op, like the reference's `std::find`. |
| `value` | the retained value, released after invalidation. |

The window holds the retained closure and invalidates it when the window is
destroyed, which breaks the window ↔ object reference cycles.

---

## 3. The `TimerBinding`

The L3 payload of one registered `Timer`: the retained action owner, the
timer object, the script-visible reference fields and L3's mirror of the L2
schedule.

The L2 `TimerState` registry holds only what "wait, then call" needs — the
integer-millisecond interval, the armed deadline and the dispatch callback —
so **every script-visible readback is served from the L3 binding, never from
L2**. The details are on [`timer.md`](timer.md).

---

## 4. Resolving the bound engine

```rust
pub(crate) fn bound(engine: *mut c_void) -> Result<(EngineBinding, CompatCtx), String>;
```

Resolves the compat context for a callback. The error is
`"visual: compat context is not bound"`.

| Helper | Returns | Error |
|---|---|---|
| `scene_mut(ctx)` | `&mut Scene` | `"visual: no scene attached"` |
| `scene_ref(ctx)` | `&Scene` | `"visual: no scene attached"` |
| `input_ref(ctx)` | `&InputState` | `"visual: no input state attached"` |

Each dereferences the attached pointer under the `attach_scene` /
`attach_input` contract: the host keeps the attached value alive for the
binding's lifetime, and the single-threaded VM holds no `&mut` to it during
a callback.

---

## 5. The value marshalling

| Helper | Writes |
|---|---|
| `set_void(out)` | TJS `void` |
| `set_int(out, value)` | a TJS integer |
| `set_real(out, value)` | a TJS real |
| `set_visual_string_out(ctx, out, text)` | a string, through the visual family's scratch buffer |
| `set_retained_out(host, out, id)` | a retained script object |
| `set_object_out(host, out, object)` | a duplicated retained object, or TJS `null` |
| `set_null_out(host, out)` | TJS `null` |

`set_null_out` evaluates the `null` expression and retains its object
variant, because the C ABI has no null literal. It falls back to `void`,
which is still falsy.

`set_visual_string_out` uses the **visual family's own** scratch buffer, not
`System`'s: a re-entrant visual callback must not clobber a `System` result
mid-marshal.

### The colour conversions

| Helper | Converts |
|---|---|
| `argb_to_rgba(color: i64) -> [u8; 4]` | `0xAARRGGBB` → straight-alpha RGBA |
| `rgba_to_argb(color: [u8; 4]) -> i64` | straight-alpha RGBA → `0xAARRGGBB` |

Script colours are ARGB; L2 stores straight-alpha RGBA8.

---

## 6. Resolving an object argument to a scene id

```rust
pub(crate) fn resolve_object_id(host, value, classes) -> Result<i64, String>;
```

| Argument | Result |
|---|---|
| an integer | the integer, unchanged |
| a real | the real, truncated |
| an object | the scene id, resolved through the VM's native-instance lookup |
| anything else | the `-1` fallback |

A **non-null** object that matches none of `classes` is a **typed error**
(`"expected a native {classes} object"`), never a silent sentinel — the
reference's native-instance lookup rejects it.

`native_instance_id` is the underlying lookup. `Ok(None)` is an ordinary
miss: TJS `null`, a plain script object, an instance of another native
class, or an instance whose payload carries no scene id yet.

The scene id is read from the native payload of one of the class names
`Window`, `Layer`, `Bitmap` or `Font` — the reader for each is registered in
`payload_id_reader`. A class name that is not one of those is a caller
mistake and returns `Err`.

Both functions are also exposed as `CompatCtx::resolve_object_id` and
`CompatCtx::native_instance_id` so the `video` and plugin families can use
them without naming this module.

---

## 7. Re-parenting a layer

```rust
pub(crate) fn reparent_layer(scene: &mut Scene, layer_id: u32, new_parent: Option<u32>);
```

Detaches the layer from its current parent and attaches it to `new_parent`
(`None` = the window root).

The candidate parent is rejected when:

* it is the layer itself;
* it belongs to a **different window**;
* it is a **descendant** of the layer — that would create a cycle.

After the move, the sibling vectors are updated and `scene.assign_front_order`
gives the layer the sort key of the front-most slot among its new siblings.

---

## 8. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/visual.rs` | `primaryLayer` is the real object; `focusedLayer` round-trips; `Window.add` does not reparent a layer |
| `crates/compat/tests/dispatch_visual.rs` | a `with`-style write from a foreign receiver lands on the member's object |
| `crates/compat/tests/layer.rs::repeated_parent_layer_cycles_release_layers_bitmaps_and_fonts` | a parent cycle is rejected and releases cleanly |
| `crates/compat/tests/layer_behaviour.rs` | the layer-only vs layer-or-bitmap source split |
| `crates/compat/tests/tree_owner.rs` | the focused-layer setter mirrors into the L2 scene |

---

## See also

* [`layer.md`](layer.md) — the class that uses these helpers most
* [`timer.md`](timer.md) — the `TimerBinding` details
* [`../ctx.md`](../ctx.md) — the dispatch state map
