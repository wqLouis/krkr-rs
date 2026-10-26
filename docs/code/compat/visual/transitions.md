# `natives/visual/layer/transitions.rs` — `beginTransition`, the pumps and the script cache

**Source:** `crates/compat/src/natives/visual/layer/transitions.rs`

This module owns the transition state machine on the L3 side: the
`beginTransition` / `stopTransition` members, the transition providers, the
per-frame pumps and the script-cache refresh.

The L2 `engine_api::Trans` machine does the compositing; L3 owns the
script-side runtime (the options, the callback, the self-update mode and the
cache bookkeeping).

---

## 1. `beginTransition(name[, withchildren[, transwith[, options]]])`

Argument positions: `0` = the transition name, `1` = `withchildren`,
`2` = `transwith` (the source layer), `3` = the options dictionary.

The reference's **order of error precedence** is reproduced exactly:

1. **The name is required** — fewer than one argument raises
   `"Layer.beginTransition requires a name"`.
2. **`withchildren`** defaults to **`true`**; a void or omitted argument keeps
   the default. Any other value is its boolean coercion.
3. **`transwith` is required and must be a `Layer`** — a missing or
   non-object argument raises `"Layer.beginTransition: transwith is required"`;
   an object of another class raises
   `"Layer.beginTransition: transwith is not a Layer"`.
4. **A second transition on the same layer is rejected** —
   `"Layer.beginTransition: a transition is already running on this layer
   (reference: TVPCurrentTransitionMustBeStopping)"`.
5. **A mutual source is rejected** — the source is already transitioning from
   this layer.
6. **The size check.** With `withchildren` the **layer rects** are compared;
   without children the **main-image** sizes are. A mismatch raises
   `"Layer.beginTransition: transition source size mismatch: destination {w}x{h},
   source {w}x{h}"`.
7. **The provider is built** — the options are parsed and validated. This
   happens *before* the image requirement check, so a missing `time` wins
   over a missing image.
8. **Without children, both endpoints must have a main image** —
   `"Layer.beginTransition: Transition source and destination must have image"`.
9. **`selfupdate` and `callback`** are captured from the options object
   *before* the transition is registered.

On success, with `withchildren` the cache-consumer count of both layers is
incremented, the transition is stored in the L3 `LayerExtras::transitions`
map and the runtime state (`self_update`, the retained tick callback,
`pending_ms`) is stored separately.

---

## 2. `stopTransition()`

* No arguments.
* A layer with **no** active transition is a **silent no-op** (a debug log).
* With an active transition it performs the **full** reference teardown: the
  layer exchange happens **and** `onTransitionCompleted` fires synchronously.

The event is gated on the source still being alive: a transition whose source
was invalidated completes **silently**.

---

## 3. `onTransitionCompleted`

The lookup goes **through the object**, so a script subclass override wins
over the native no-op default.

* The arguments are `(dest, src)` — the destination layer object and the
  source layer object.
* A dead source suppresses the event entirely (not a null argument): the
  source object could not be duplicated, so a debug line is logged and no
  dispatch happens.
* A handler failure is logged and never aborts the frame.

---

## 4. The transition kinds

The lookup is an **exact, case-sensitive** name with **no silent fallback**.

| Script name | L2 kind | Options (reference defaults) |
|---|---|---|
| `"crossfade"`, `"fade"`, `""` | `CrossFade` | `time` (required) |
| `"scroll"`, `"slide"`, `"push"` | `Scroll { from, stay }` | `time`, `from` (0 = `sttLeft`), `stay` (0 = `ststNoStay`) |
| `"universal"` | `Universal { vague }` | `time`, `vague` (64), `rule` (required) |
| `"mask"` | `Mask` | `time`, `rule` (required) |
| `"wave"` | `Wave` | `time`, `maxh` (50), `maxomega` (0.2), `bgcolor1` (0), `bgcolor2` (0), `wavetype` (0) |
| `"mosaic"` | `Mosaic` | `time`, `maxsize` (30) |
| `"turn"` | `Turn` | `time`, `bgcolor` (0) |
| `"rotatezoom"` | `RotateZoom` | `time`, `factor` (1.0), `accel` (0.0), `twist` (2.0), `twistaccel` (-2.0), `centerx`/`centery` |
| `"rotatevanish"` | `RotateVanish` | `time`, `accel` (2.0), `twist` (2.0), `twistaccel` (2.0), `centerx`/`centery` |
| `"rotateswap"` | `RotateSwap` | `time`, `bgcolor` (0), `twist` (1.0) — **no centre option** |
| `"ripple"` | `Ripple` | `time`, `centerx`/`centery`, `rwidth` (128), `roundness` (1.0), `speed` (6.0), `maxdrift` (24) |

* `""` is the KAG default and maps to `CrossFade`.
* `"fade"`, `"slide"` and `"push"` are **port extensions** (aliases), not
  reference provider names.

### The option rules

| Option | Rule |
|---|---|
| `time` | **required**. Read with the engine's integer coercion, so a numeric string is accepted. The value is truncated toward zero and clamped to a minimum of **2 ms**. A non-finite value reaches L2, which rejects it as an invalid duration rather than clamping. |
| `selfupdate` | false unless the member is present and non-void; read through the integer coercion. A read error is logged at debug and treated as false. |
| `callback` | retained only when it is an object or a function. A scalar raises `"Layer.beginTransition: transition option callback must be a function, got {other:?}"`. |
| `rule` | a required **string**. The image is read through the same storage and decoder pair `Layer.loadImages` uses and handed to L2 as a `RuleMask` (grayscale by the Rec. 601 luma `(77r + 150g + 29b) >> 8`, tiled when smaller than the source). A missing `rule` raises `"Layer.beginTransition: this transition requires a `rule` image (reference: TVPSpecifyOption)"`. |
| `from` / `stay` | numbers cast straight to the enum. An out-of-range number raises. The string aliases (`"top"`/`"up"`/`"right"`/`"bottom"`/`"down"`, `"dest"`/`"destination"`/`"src"`/`"source"`) are a **compat extension** and are case-insensitive. |
| `centerx` / `centery` | read independently, each defaulting to half the destination size. Supplying one without the other is legal; the missing side needs a destination size, otherwise the call raises. |

A non-numeric option value raises
`"Layer.beginTransition: invalid transition option {name}: value is not a number"`;
an unreadable member raises
`"Layer.beginTransition: cannot read option {name}: {error}"`.

---

## 5. The `withchildren` semantics

| `withchildren` | What is blended |
|---|---|
| `true` (the default) | the **whole layer tree**. Caching is enabled for both layers, and each frame blends the fully rendered **subtrees**: the source's content is published as the source's cache bitmap and a separate transition **frame** bitmap is the blend target. That frame is what the projection draws in place of the subtree while the transition runs. |
| `false` | the two **main images**, blended into the destination's **self image**. The source is the destination's own main image plus the source layer's main image. The self image is what the projection draws where the main image would be, while the destination's children keep drawing live. Neither main image is ever written. |

The teardown clears the self image and the recorded transition frame.

### The blend the destination's type selects

The blend a cross-fade runs is chosen by the destination layer's type, and
the type is read once, when `beginTransition` starts the transition:

| the destination's type | the blend |
|---|---|
| a type that uses its alpha channel (`ltAlpha` and the Photoshop spellings) | the alpha-vs-alpha blend: the source covers the destination with the share the two alphas give it, and the result's own alpha is the two alphas mixed by the phase |
| `ltAddAlpha` | the additive blend: all four channels are mixed with the phase as the ratio |
| every other type (`ltOpaque`, `ltAdditive`, the subtractive and the multiplicative ones) | the plain blend: the colour channels are mixed with the phase as the ratio |

L3 states the destination's handling as `engine_api::DestAlpha`, the generic
vocabulary the engine's transition machine understands; the engine never sees
the script's type numbering. The `universal` and `mask` kinds and the
`extrans` providers keep the four-channel mix.

### The composited form

`Scene::composite_transition_frames` is the one host-selected field: it says
whether the host draws the projected scene. When it is set, the two sources
are **not** blended into a bitmap. `Trans::transition_frame` returns the
frame for the two composition kinds (`crossfade` and `scroll`) — the two
sources placed inside one rectangle, with the fade source carrying the phase
weight — and the layer records it in `LayerState::transition_frame`. The
projection then draws the frame's two sources in place of the layer's image or
subtree, so a GPU host writes no pixels and uploads no bitmap per tick.

Every other kind needs a per-pixel rule or a coordinate warp, so
`transition_frame` returns nothing and the transition is blended with
`Trans::apply` on every host — GPU included.

The two **rendered** sources must have the same size in both forms; a mismatch
is `TransError::SizeMismatch` — `"transition source size mismatch: {src1:?} vs
{src2:?}"` — raised as a script error. This is a later check than the
provider's own up-front size check, which compares the endpoints' declared
sizes.

A composited frame changes every tick, but only because its blend weights (or
placements) change: the endpoint **subtrees** it samples do not. The frame
update therefore marks only the transitioning layer's caching **ancestors**
stale, which must repaint the frame, and leaves the layer's own composited
content reusable as the frame's source. A source layer that is also one of
the destination's ancestors is excluded from that invalidation: its content is
the frame source, not part of the changed output. Marking the layer itself
stale would recomposite a full-screen endpoint subtree on every transition
tick for no visible change. Clearing the frame does invalidate the layer
itself, because it then returns to drawing its own subtree and any cache kept
from before or during the transition is stale. The same rule applies to the
custom-shader effect draw.

While a layer owns a frame or effect draw, the projection never draws that
layer's own `cache_bitmap`: the draw replaces the image the cache holds.
A frame that covers only the layer's own image does not hold its children, so
they keep drawing live underneath it.

The reference's layer exchange uses the swap form for a transition that ran
*without* children, and the plain form otherwise.

---

## 6. The frame pumps

### `advance_transitions(ctx)`

The frame-body step:

1. `paint_poll(ctx)` — fires `onPaint` for every layer whose pending-paint
   flag is set, mirroring the reference's pre-draw step, which fires the
   `onPaint` event and then processes the transition in the same step.
2. `advance(ctx, None)` — the continuous pump.

### `advance_self_updating_transitions(ctx, drawn_layers)`

The post-projection step: `advance(ctx, Some(drawn_layers))`. The
`drawn_layers` argument is the projection's actual emit report, including the
descendants covered by a drawn cached ancestor.

### `advance(ctx, drawn_layers)`

For each active transition:

1. **Dead endpoints.** If the destination or the source script object has
   died, the transition is torn down **without** the completion event.
2. **Self-update routing.**
   * the continuous pump with `selfupdate` → the transition is **not** stepped;
     the draw that reports its layer steps it;
   * the continuous pump without `selfupdate` → the transition steps
     normally;
   * the self-update pump without `selfupdate` → the transition is skipped;
   * the self-update pump with `selfupdate` → the transition proceeds **only
     if the projection drew it**. A drawn descendant counts, and a drawn
     cached ancestor reports the descendants it covered. If it was not drawn,
     it keeps its place until a frame draws it.
3. **The callback tick** is fetched *before* the visibility check. The
   callback's return value is the transition's elapsed time, converted with
   the integer cast. A non-numeric return raises
   `"Layer.beginTransition: transition callback must return a numeric tick"`;
   a callback failure raises
   `"Layer.beginTransition: transition callback failed: {error}"`.
4. **An invisible destination stops at that frame**, before rendering — but
   the completion event still fires.
5. **Stepping**: with a callback the transition seeks to the tick; otherwise
   it advances to the **engine clock's** tick, and the machine's elapsed time
   is the difference against the tick the transition started at — the form the
   reference computes its phase in, a tick difference rather than an
   accumulated frame delta.
6. **Rendering**: the transition frame is written through the borrowed
   machine.
7. **Completion**: the transition is torn down.

The teardown **removes the entry before the dispatch**, so a handler that
starts a new transition for the layer registers cleanly.

---

## 7. The script cache

### `Layer.cached`

| Operation | Behaviour |
|---|---|
| get | the flag as an integer (`0`/`1`). |
| set | the boolean coercion. An **unchanged** value is a **no-op**. Otherwise the flag is stored and the **cache consumer count** is adjusted: incremented when it is turned on, decremented when it is turned off. |

The count is a counter, not a boolean, because a layer can be a cache
consumer for more than one reason — its own `cached` flag **and** a
whole-subtree transition's increment. The last release frees the rendered
cache.

### `refresh_cached_layers(ctx)`

The per-frame step of `Layer.cached`:

* It reads the scene's `(structure_revision, content_revision)` and returns
  immediately when the pair equals the last recorded one — no layer or
  bitmap edit, no timer-driven composite and no revision bump means no cache
  can be stale.
* The pass runs **after** the transition step (so a cache that contains a
  transition sees the frame just blended) and **before** the engine uploads
  and projects.
* A layer that owns an active transition is skipped — it keeps its cache
  slots for the transition step.
* The **ancestors** of a whole-subtree transition are *blocked*: their cache
  would hold the un-blended image the frame replaces, so their published
  cache is withdrawn and the layer draws live until the transition ends.
* A hidden layer, or one with neither its own image nor a visible,
  non-transparent direct child, is skipped; a cached layer that no longer has
  content has its cache withdrawn.
* Otherwise the subtree is composited and published as the layer's cache
  bitmap (unless it is already the layer's own bitmap).
* Cached layers are visited **deepest first**, so a parent composites
  through the caches its children publish in the same pass.
* The new revision pair is recorded in the L3 `LayerExtras`.

---

## 8. `paint_poll(ctx)`

Fires `onPaint` for every layer whose pending-paint flag is set — the
per-draw step.

The flag is cleared **before** the call, so a re-entrant `update()` from
inside `onPaint` persists to the next frame's dispatch. The event is posted
to the layer object, so a script override runs. A handler failure is logged
as a warning and never aborts the frame.

This is what makes an `AffineLayer` render: its `onPaint` copies the hidden
`_image` layer onto the visible outer layer.

---

## 9. State ownership

| State | Owner |
|---|---|
| the transition machine, the rule image, the blend target, the destination's alpha handling | L2 `engine_api::Trans` / `TransKind` / `RuleMask` / `DestAlpha` |
| the active transition per layer (`Trans`, the source id, `with_children`) | L3 `LayerExtras::transitions` |
| the runtime state (`self_update`, the retained tick callback, `pending_ms`) | L3 `TransitionRuntimeState` |
| the cache revisions | L3 `LayerExtras::cache_revisions` |
| the cache slots and the consumer count | L2 `Scene` |
| the recorded transition frame | L2 `Scene` (`LayerState::transition_frame`) |

---

## 10. Errors

| Message | Raised when |
|---|---|
| `"Layer.beginTransition requires a name"` | no transition name |
| `"Layer.beginTransition: transwith is required"` | the source argument is missing or not an object |
| `"Layer.beginTransition: transwith is not a Layer"` | the source argument is another class |
| `"Layer.beginTransition: a transition is already running on this layer (reference: TVPCurrentTransitionMustBeStopping)"` | the layer is already transitioning |
| `"Layer.beginTransition: the transition source is already transitioning from this layer (reference: TVPTransitionMutualSource)"` | the source is transitioning from this layer |
| `"Layer.beginTransition: transition source size mismatch: destination {w}x{h}, source {w}x{h}"` | the sizes differ |
| `"Layer.beginTransition: Transition source and destination must have image"` | no children and an endpoint has no main image |
| `"Layer.beginTransition: Specify option time"` | the `time` option is missing |
| `"Layer.beginTransition: this transition requires a `rule` image (reference: TVPSpecifyOption)"` | the `rule` option is missing |
| `"Layer.beginTransition: transition option callback must be a function, got {other:?}"` | the `callback` option is a scalar |
| `"Layer.beginTransition: invalid transition option from: {n} is not a tTVPScrollTransFrom value (0..3)"` | the `from` option is out of range |
| `"Layer.beginTransition: invalid transition option stay: {n} is not a tTVPScrollTransStay value (0..2)"` | the `stay` option is out of range |
| `"Layer.beginTransition: invalid transition option {name}: value is not a number"` | a non-numeric option |
| `"Layer.beginTransition: cannot read option {name}: {error}"` | an unreadable option |
| `"Layer.beginTransition: centerx/centery needs a destination image to default the other side"` | one centre option without a destination size |
| `"Layer.beginTransition: transition callback must return a numeric tick"` | the callback returned a non-number |
| `"Layer.beginTransition: transition callback failed: {error}"` | the callback raised |

---

## 11. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/layer.rs` | `transitions_begin_and_stop`, `extrans_transitions_begin_and_stop`, `unsupported_transitions_and_media_helpers_raise_typed_errors`. |
| `crates/compat/tests/transitions.rs` | the transition behaviours: the kinds, the options, the `withchildren` semantics, the completion event, the callback tick and the self-update mode. |
| `crates/compat/tests/layer_behaviour.rs` | the cache and repaint semantics. |
| `crates/compat/tests/visual.rs` | the scene-level transitions. |

---

## See also

* [`layer.md`](layer.md) — the class the transitions belong to
* [`../frame.md`](../frame.md) — where the pumps run in the frame
* [`../../../../arch/l2.md`](../../../arch/l2.md) §8 — the L2 transition machine
