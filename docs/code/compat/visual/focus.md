# `natives/visual/layer/focus.rs` — the focus domain model

**Source:** `crates/compat/src/natives/visual/layer/focus.rs`

Focus is **per domain**. The reference keeps one layer manager per
parentless-root tree, and every focus operation stays inside one manager. The
domain id is the founding parentless root's own layer id; a child inherits
its parent's domain, and the domain is fixed at creation.

---

## 1. The four entry points

| Function | Domain | Semantics |
|---|---|---|
| `focus_layer(ctx, id, direction)` | the **layer's own** domain | gives `id` the focus in its own domain and fires the focus events. Returns whether the focus moved. |
| `focus_layer_in_input_domain(ctx, window, layer, direction)` | the **window's input root** domain, **regardless of the layer's own domain** | the `Window.focusedLayer = layer` shape: the reference setter moves the first registered manager's `FocusedLayer` and never checks membership. A window with no input root is a no-op. |
| `clear_layer_focus(ctx, window, direction)` | the window's input root domain | `Window.focusedLayer = null`: clears the input-domain focus and fires `onBlur` on the outgoing layer. A window with no input root is a no-op. |
| `clear_domain_focus(ctx, window, domain, direction)` | the **given** domain | the `SetFocusTo(nullptr, direction)` tail for callers that operate on a layer's own domain (the modal-stack paths). |

---

## 2. `set_focus_to` — the `SetFocusTo` body

```rust
set_focus_to(ctx, window, domain, target: Option<u32>, direction: bool) -> bool
```

1. **Guard.** If the target is not node-focusable, return `false` with **no
   state change and no events**. `node_focusable` = `focusable && node_visible
   && node_enabled`, and the node predicates include every ancestor.
2. **No-op.** If the current focus of `(window, domain)` already equals the
   target, return `false` — no state change, no events.
3. **State move.** The focus state moves **before** the events, so an
   `onBlur` handler already reads the new focus.
4. **Events**, in this order:
   * `onBlur` on the **outgoing** layer — one argument: the newly focused
     layer or `null`.
   * `onFocus` on the **incoming** layer — two arguments: the previously
     focused layer or `null`, then the direction as `0`/`1`.
   * Each event is skipped while its layer is `shutdown`, so an invalidated
     layer that still holds the focus is cleared **silently**. A handler that
     invalidates the other end silences that end's event.
   * Both are delivered **synchronously** (the reference's immediate mode).
5. Returns `true` when the focus moved.

The event arguments are the layers' **registered script objects** (a real
`DetachedValue`), or TJS `null`. A missing script object logs a warning and
the event is skipped.

---

## 3. `blur_tree` — the "blur-off" properties

`blur_tree(ctx, root)` is the port of the reference's `BlurTree`:

* If the domain's focused layer is neither `root` nor a descendant of `root`,
  nothing happens.
* Otherwise the focus moves to the next focusable layer of the domain
  (forward), or is cleared when there is none. A `shutdown` root can only
  produce the clear — its manager is gone, so the neighbour search yields
  nothing.

### The blur-off properties

| Property | Effect |
|---|---|
| `visible = false` | `blur_tree` on the layer. |
| `enabled = false` | `blur_tree` on the layer. |
| `focusable = false` | `blur_tree` on the layer. |
| the `parent` setter | the blur happens **while the layer is still attached to its old parent**. |
| the invalidate / teardown path | the blur runs as part of the shutdown. |

The reference's `CheckTreeFocusableState` is compiled out, so **enabling or
showing a layer never auto-focuses it**.

### `enabled` and the node predicates

`Scene::node_enabled(id)` = the layer's own `enabled` **and** every
ancestor's `enabled` (a `shutdown` layer reports `false`).

`node_focusable` requires `node_enabled`, so disabling a layer also removes
it from the focus chain.

---

## 4. `focusNext` and `focusPrev`

`focus_neighbor(engine, instance, out, out_error, method, forward)`:

* The search starts from the **domain manager's focused layer**, never from
  the receiver.
* With nothing focused it starts from the **first focusable layer of the
  domain's primary chain** — the window's layer order filtered by domain,
  `join_focus_chain` and `node_focusable`.
* `focusNext` moves the focus with direction `true`; `focusPrev` with
  `false`.
* The method returns the **newly focused layer object**, or `null`.

`focus([direction])` on a layer reads the optional argument with the TJS
boolean coercion (default `true`) and calls `focus_layer`. It returns
**void**, and a non-focusable target is **not** an error — the focus simply
does not move.

---

## 5. The capture releases

| Member | Behaviour |
|---|---|
| `releaseCapture()` | the manager releases the mouse capture **globally**, ignoring which layer captured. The port pushes the layer id onto the L3 `LayerExtras::release_capture` queue and drops the adapter's mouse-capture state. |
| `releaseTouchCapture([id])` | with **no** argument it releases *every* touch capture. With one argument the id is the integer coercion of `param[0]` (a present-but-void argument therefore selects contact `0`) truncated to 32 bits. `None` in the queue models "all contacts". |

The frame body drains the queues with `take_capture_releases(ctx)`, which
returns `(Vec<u32>, Vec<(u32, Option<u64>)>)`.

---

## 6. The attention point and the cursor position

| Member | Behaviour |
|---|---|
| `setAttentionPos(left, top)` | stores the two attention anchor components. |
| `setCursorPos(x, y)` | the arguments are **layer-local**. The layer's and every *parented* ancestor's rect offset is accumulated up the parent chain (the root contributes nothing), and the resulting window coordinate is handed to the L2 `InputState::warp_pointer`. |

---

## 7. Errors

| Message | Raised when |
|---|---|
| `"Layer.focusNext: this layer is not constructed"` | the payload has no scene id |
| `"Layer.focusPrev: this layer is not constructed"` | the payload has no scene id |
| `"Layer.focusNext: this layer no longer exists"` | the scene record is gone |
| `"Layer.setAttentionPos requires 2 arguments"` | the arity check |
| `"Layer.setCursorPos requires 2 arguments"` | the arity check |
| `"Layer.releaseCapture: this layer is not constructed"` | the payload has no scene id |
| `"Layer.releaseTouchCapture: this layer is not constructed"` | the payload has no scene id |
| `"visual: no input state attached"` | `setCursorPos` with no attached L2 input state |

---

## 8. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/focus_behaviour.rs` | the focus rules end to end: the domain model, the blur-off properties, the focus chain walk, the capture releases, the attention point and the cursor position. |
| `crates/compat/tests/layer_behaviour.rs` | `focus_search_starts_from_the_focused_layer_not_the_receiver`, `focus_prev_without_focus_returns_the_first_focusable`, `focus_neighbours_walk_from_the_current_focus`, `focus_next_returns_null_when_nothing_is_focusable`, `set_attention_pos_stores_both_components`, `set_cursor_pos_accumulates_the_parent_chain_offsets`, `cursor_property_setters_store_then_warp_the_pointer`, `capture_release_members_queue_reference_shaped_requests`. |
| `crates/compat/tests/layer.rs` | `focus_next_and_prev_walk_the_focus_chain`, `attention_cursor_and_capture_members_are_effects`. |
| `crates/compat/tests/window.rs` | `focusedLayer` writes and reads the input domain. |

---

## See also

* [`layer.md`](layer.md) — the layer members that use the focus model
* [`../event.md`](../event.md) — the key and wheel events the focused layer receives
* [`../../../../arch/l2.md`](../../../arch/l2.md) §2 — the layer state on the engine side
