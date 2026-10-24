# `natives/visual/layer/` — the `Layer` class, the ordering and the modal stack

**Sources:**

* `crates/compat/src/natives/visual/layer/mod.rs` — the class
* `crates/compat/src/natives/visual/layer/order.rs` — the sibling order, the absolute order and the modal stack

The registered `Layer` surface is 199 members: the core class plus the
`layerExDraw.dll`, `layerExMovie.dll` and `perspective.dll` plugin members
attached to it. (The surface dump lists 201 entries for `Layer` — the
class-name constructor and the VM's `finalize` are two of them.)

---

## 1. Construction and teardown

| Phase | What happens |
|---|---|
| **native create** | allocates a **detached** L2 scene record with the default `32 × 32` rect and a default 32×32 main image. The record exists **before** `Construct`, so a script subclass may touch members in its class prelude. |
| **the `Layer(window, parent)` constructor** | `window` may be a `Window` object or an integer id; when absent or unresolvable the **scene's first window** is used; with no window at all it raises `"Layer: no window in the scene — create a Window first"`. `parent` may be a `Layer` object or a non-negative integer id; otherwise the layer attaches to the window root. The detached record is joined with `scene.attach_layer`. The constructor **returns the scene id as an integer**. A second call raises `"Layer: this layer is already constructed"`. |
| **`Invalidate`** | sets `layer.shutdown = true` **before** any detach or blur, drops the cached `Font` wrapper, blurs the tree, severs the direct children and detaches them from this layer so each keeps its own scene record, detaches the layer from the scene, and drops the per-layer plugin state. No child script object is invalidated: a child's record belongs to the child's script object and is released by that object's own finalizer. |
| **native destroy** | removes the registry entry, removes the scene record if it is still present, drops the plugin state and frees the payload. Idempotent with the invalidate hook. |

### The payload

```rust
struct LayerInst {
    id: Option<u32>,
    attached: bool,
    font_object: Option<DetachedValue>,   // the cached `layer.font` wrapper
    cursor_x_work: i32,                   // the pending X for the next `SetCursorY`
}
```

The payload id is read back through the VM's native-instance lookup. It
replaces the removed script-visible `id`/`nativeId` members — **`Layer.id`
does not exist**, by design.

---

## 2. The members

### Geometry

| Member | Behaviour |
|---|---|
| `left`, `top` | the layer rect origin. A **primary layer refuses a real move** to a non-zero value → `"Layer: Cannot move primary layer"`; writing the value it already holds is a silent no-op. |
| `width`, `height` | the rect extent, clamped at 0. The scene's size-changed follow-up runs only when the value actually changed. |
| `setPos(left, top[, w, h])` | needs 2 arguments. The 4-argument form is a bounds request; a negative extent raises `"Layer.setPos: Invalid argument"`. |
| `setSize(w, h)` | needs 2 arguments. |
| `setSizeToImageSize()` | sizes the layer to its main image; a layer with no main image raises `"Layer.setSizeToImageSize: Not drawable layer type"`. |

### Image placement

| Member | Behaviour |
|---|---|
| `imageLeft`, `imageTop` | the image offset. An offset is legal only when `offset <= 0` **and** `image_extent + offset >= rect_extent`; otherwise the write raises. A layer with no image raises `"Not drawable layer type"`. |
| `imageWidth`, `imageHeight` | the setters shrink the rect and clamp the offset, then resize the main image keeping the other dimension. A zero extent raises `"Cannot create empty layer image"`. |
| `setImagePos(left, top)` | needs 2 arguments. |
| `setImageSize(w, h)` | rejects 0. |
| `hasImage` | `true` allocates a main image the size of the rect and resets the clip; `false` releases the bitmap, deallocates the province image and sets `imageModified`. |

### Images and buffers

| Member | Behaviour |
|---|---|
| `assignImages(srcLayer)` | copies the source's main image and province image. An absent source image deallocates the destination's; the destination rect adjusts to the assigned image size, the clip resets and `imageModified` is set. An invalidated source Layer is still a Layer for the native-instance lookup (the reference keeps the instance registered until the object is destructed) and contributes no images, so it deallocates the destination's images the same way. A script-`Bitmap`-owned bitmap is never overwritten — a fresh copy is adopted. The source image is **resolved first** — a pending decode is waited for — and a source whose pixel buffer is not the bytes its size declares raises. |
| `copyFromBitmapToMainImage(bitmap)` | `null` is a successful no-op; a non-`Bitmap` argument raises. The bitmap is resolved first, and a bitmap whose pixel buffer is not the bytes its size declares raises. |
| `copyToBitmapFromMainImage(bitmap)` | replaces the bitmap's content with the layer's main image. The main image is resolved first, and an image whose pixel buffer is not the bytes its size declares raises. |
| `independMainImage()` | gives the layer a private main image. The image is resolved first, and an image whose pixel buffer is not the bytes its size declares raises. |
| `independProvinceImage([copy])` | allocates the 8-bit province plane; the default keeps the existing content, `copy = false` clears it. |
| `loadImages(name[, key])` | loads the image and installs it as the layer's main image. The **rect is not resized to the image**: the image-size adjustment only *shrinks* a rect the image is smaller than, so a rect smaller than the loaded image keeps its own extent and only `imageWidth`/`imageHeight` follow the image. The image offsets and the clip follow the loaded image, and the metainfo is `null` (the codec reports none). The load may be deferred to the decode pool — see below. |
| `mainImageBuffer`, `mainImageBufferForWrite` | read-only. The address is `0` when the main image is absent **and** when the main image declares more pixels than its pixel buffer holds. Both wait for a pending decode first; the `ForWrite` variant marks the image modified, and an image with no address is not marked. |
| `mainImageBufferPitch`, `provinceImageBuffer`, `provinceImageBufferForWrite`, `provinceImageBufferPitch` | read-only. The address is `0` when the buffer is absent. The `ForWrite` variant allocates the province plane. |
| `imageModified` | set by the operations that write pixels. |

### The pending main image

`Layer.loadImages` may hand the decode to the L2 decode pool. The load then
returns with the layer's **image size known** — the header probe ran on the
calling thread — and the pixels still decoding:

* the layer's main image is a **pending** bitmap: the probed size and no
  pixels;
* the image-size adjustment runs on the probed size: a rect the image is
  smaller than shrinks to the image, a rect the image is larger than keeps
  its own extent, and the image offsets and the clip follow the loaded
  image;
* every read of the main image's pixels waits for the decode, and the frame's
  image barrier waits for all of them;
* the `mainImageBuffer` and `mainImageBufferForWrite` addresses wait for it
  too, so a script never receives the address of an image whose pixels have
  not landed;
* a decode that **fails after** the probe installs a transparent blank of the
  probed size and logs the failure. It does **not** raise — the load already
  returned. A **probe** failure raises at the load, the way a load failure
  raises there.

A **province** load (`loadProvinceImage`) and every load on a host with **no
decode pool** decode synchronously, on the calling thread; a colour key is a
worker pass and does not prevent deferral. The member details are in
[`pixels.md`](pixels.md) §7.

### Visibility, opacity, blend and the draw face

| Member | Behaviour |
|---|---|
| `visible` | a "blur-off" property: setting it false blurs the focused subtree. |
| `opacity` | the reference `0..255` scale, stored as a clamped `0.0..=1.0`. |
| `type` | the native `tTVPLayerType` number — see §4. |
| `face` | the draw face: `dfBoth = 0`, `dfMain = 1`, `dfMask = 2`, `dfProvince = 3`, `dfAddAlpha = 4`. |
| `holdAlpha` | the reference hold-alpha flag; see [`pixels.md`](pixels.md). |
| `cached` | registers or unregisters a cache consumer; see [`transitions.md`](transitions.md). |
| `callOnPaint` | the reference flag. |
| `update()` | sets the layer's pending-paint flag; the frame pump dispatches `onPaint` before the draw. |

### Ordering

| Member | Behaviour |
|---|---|
| `absolute`, `order` | the sort key and the sibling position. See §3. |
| `absoluteOrderMode` | a property of the **parent** layer. |
| `bringToFront`, `bringToBack` | move the layer to the front or the back of its siblings. |
| `moveBefore(other)`, `moveBehind(other)` | move the layer directly in front of / behind a sibling. |
| `setMode()`, `removeMode()` | the modal stack. See §3. |

### Hit test, input and the cursor

| Member | Behaviour |
|---|---|
| `hitThreshold`, `hitType` | the mask hit-test parameters. |
| `cursor` | the cursor image. |
| `cursorX`, `cursorY` | the window pointer position expressed in the layer's local space: each layer's and every *parented* ancestor's rect offset is subtracted. The lowest active touch contact wins over the pointer. Writing `cursorX` stores a pending X; writing `cursorY` warps the pointer to (pending X, y) after adding the same ancestor offsets. |
| `getLayerAt(x, y[, excludeSelf[, getDisabled]])` | the point is in the receiver's local coordinates; the search walks front-to-back over the window's paint order, requires the point to be inside every ancestor's rect and the layer's clip, and applies the mask/province hit test. A disabled frontmost hit returns `null` unless `getDisabled` is true. |
| `attentionLeft`, `attentionTop`, `useAttention` | the attention anchor. |
| `hint`, `showParentHint`, `ignoreHintSensing` | the hint flags. |
| `imeMode`, `neutralColor` | the IME mode and the neutral colour. |

### Focus

`focus([direction])`, `focusNext()`, `focusPrev()`, `setAttentionPos(l, t)`,
`setCursorPos(x, y)`, `releaseCapture()`, `releaseTouchCapture([id])`,
`focusable`, `focused`, `joinFocusChain`, `nextFocusable`, `prevFocusable`,
`nodeVisible`, `nodeEnabled`, `nodeFocusable`. See [`focus.md`](focus.md).

### Structure

| Member | Behaviour |
|---|---|
| `name` | the layer name. |
| `parent` | read/write. Writing `null` moves the layer to the window root; a self-parent or a move into another focus domain raises. |
| `window` | the owning window object. |
| `children` | the child layer objects. |
| `isPrimary` | whether the layer is its window's primary layer. |
| `font` | the layer's lazily created, cached `Font` object. **Read-only**; reading it before the constructor has joined the layer raises `"Layer.font: this layer is not constructed"`. |

### Transitions

`beginTransition(name[, withchildren[, transwith[, options]]])` and
`stopTransition()` — see [`transitions.md`](transitions.md).

### The plugin members

| Plugin | Members |
|---|---|
| `layerExDraw` | `clear`, `drawPath`, `drawArc`, `drawPie`, `drawBezier`, `drawBeziers`, `drawClosedCurve`, `drawClosedCurve2`, `drawCurve`, `drawCurve2`, `drawCurve3`, `drawEllipse`, `drawLine`, `drawLines`, `drawPolygon`, `drawRectangle`, `drawRectangles`, `drawPathString`, `drawString`, `drawImage`, `drawImageRect`, `drawImageStretch`, `drawImageAffine`, `measureString`, `measureStringInternal`, `record`, `redrawRecord`, `saveRecord`, `loadRecord`, `getRecordImage`, `setTransform`, `resetTransform`, `rotateTransform`, `scaleTransform`, `translateTransform`, `setViewTransform`, `resetViewTransform`, `rotateViewTransform`, `scaleViewTransform`, `translateViewTransform`, `smoothingMode`, `textRenderingHint`, `updateWhenDraw` |
| `layerExMovie` | `openMovie`, `startMovie`, `stopMovie`, `isPlayingMovie` |
| `layerExPerspective` | `perspectiveCopy` |

The pixel members (`fillRect`, `colorRect`, `copyRect`, the stretches, the
affines, the filters, the pixel accessors, `loadImages`, `saveLayerImage`,
`drawText`, `drawGlyph`, …) are on [`pixels.md`](pixels.md).

The `layerExDraw` paths wait for a pending decode as well, so a plugin draw
never sees a bitmap whose pixels have not landed.

---

## 3. The ordering and the modal stack

### The sibling vector

The sibling order is the maintained `children` vector of the parent
`LayerState`, or the window's root `layers` vector for a parentless layer.
**Back is index 0, front is last.**

### The relative operations

| Operation | Behaviour |
|---|---|
| `bringToBack` | removes the layer from the sibling vector and inserts it at index 0. A layer with no parent raises `"Layer.bringToBack: this layer has no parent"`. |
| `moveBefore(other)` / `moveBehind(other)` | the target must exist and share the same **window** **and** the same **parent**. A self-move or a non-sibling target raises `"Layer.moveBefore: cannot move a layer next to itself or a non-sibling"`. The layer is removed and re-inserted directly in front of / behind the target. |
| `bringToFront` | leaves absolute mode on the parent, removes the layer from the sibling vector and pushes it to the end. |

Every relative operation **leaves** absolute mode, and every one of them
renumbers the siblings so the vector and the `z_order` keys stay consistent.

### The absolute mode

`LayerState.z_order` is the sort key: the sibling position in relative mode,
the stored absolute index while the parent is in absolute mode.

| Member | Behaviour |
|---|---|
| `absoluteOrderMode` | a property of the **parent**. Switching it on renumbers each child's `z_order` from its current sibling position; switching it off renumbers them back to positions. |
| `absolute` (setter) | requires a parent. It turns the **parent's** absolute mode on if it was off, computes the insertion point (the first sibling whose `z_order >= absolute`, compensated by −1 when the layer moves forward) and stores `layer.z_order = absolute`. |
| `absolute` (getter) | the stored absolute index while the parent is in absolute mode, otherwise the sibling position (`0` for a parentless layer). |
| `order` (setter) | requires a parent, **leaves** the parent's absolute mode, clamps `order` to `[0, sibling_count - 1]`, moves the layer and renumbers the siblings. |
| `order` (getter) | the sibling position (`0` for a parentless layer). |

`Scene::assign_front_order(id)` gives a layer the sort key of the front-most
slot among its current siblings — one past the largest sibling `z_order`, or
`0` when it is the first child. It is called **after** the parent is set but
**before** the layer is pushed into the sibling list, so the layer cannot
contribute a stale key.

The draw order is a **stable sibling sort** keyed by `(z_order, maintained
position, id)`, so absolute-mode keys and relative positions compose
deterministically.

### The modal stack

The modal stack lives in the L3 `LayerExtras.modes`, because L2 does not
model layer *modes*.

`setMode()` follows the reference's order:

1. The **top** of the modal stack blocks an ancestor-or-self of itself →
   `"Layer.setMode: cannot set mode to an ancestor of a modal layer"`.
2. An invisible layer is **forced visible** first.
3. A parent-invisible layer, or the layer's own disabled flag →
   `"Layer.setMode: cannot set mode to a disabled or parent-hidden layer"`.
4. Focus moves to the **first focusable node of the new modal's subtree**
   (the node itself when it is node-focusable and joins the focus chain,
   then its children in order). When the subtree has none, that domain's
   focus is cleared.
5. Only then is the layer pushed onto the modal stack.

`removeMode()` pops the layer from the stack; if it was modal, focus moves
to the next focusable layer of the domain, or the domain focus is cleared.
Removing a layer that is not modal is a **silent no-op**.

---

## 4. The layer type (the native blend numbering)

`Layer.type` carries the reference `tTVPLayerType` numbering:

| Value | Spelling | Value | Spelling |
|---|---|---|---|
| 0 | `ltBinder` | 15 | `ltPsSubtractive` |
| 1 | `ltOpaque` | 16 | `ltPsMultiplicative` |
| 2 | `ltAlpha` | 17 | `ltPsScreen` |
| 3 | `ltAdditive` | 18 | `ltPsOverlay` |
| 4 | `ltSubtractive` | 19 | `ltPsHardLight` |
| 5 | `ltMultiplicative` | 20 | `ltPsSoftLight` |
| 6 | `ltEffect` | 21 | `ltPsColorDodge` |
| 7 | `ltFilter` | 22 | `ltPsColorDodge5` |
| 8 | `ltDodge` | 23 | `ltPsColorBurn` |
| 9 | `ltDarken` | 24 | `ltPsLighten` |
| 10 | `ltLighten` | 25 | `ltPsDarken` |
| 11 | `ltScreen` | 26 | `ltPsDifference` |
| 12 | `ltAddAlpha` | 27 | `ltPsDifference5` |
| 13 | `ltPsNormal` | 28 | `ltPsExclusion` |
| 14 | `ltPsAdditive` | | |

L2 stores the **generic** blend vocabulary (`BlendMode`); the numbering is
TVP vocabulary and therefore lives in L3. `blend::from_native` maps a native
value to a `BlendMode` and `blend::to_native` is the exact inverse — every
distinct spelling round-trips, including the Photoshop spellings that share
a formula with a non-Photoshop type.

`ltBinder` (0), `ltEffect` (6) and `ltFilter` (7) have no generic
equivalent and **degrade to source-over** (a binder has no image in the
reference either). An **unknown or out-of-range value also degrades to
source-over** rather than raising.

The `tTVPBlendOperationMode` shares the `tTVPLayerType` numbering, so the
canonical native encoding of an operation mode *is* its layer type.

The `imageTagLayerType` global carries the Photoshop-ish spellings for the
image-tag API.

---

## 5. State ownership

| State | Owner |
|---|---|
| the whole layer tree, the rect, the visibility, the opacity, the blend, the z-order, the children, the parent, the domain, the image placement, the province plane, the clip, the hit settings, the face, the cache state, the font id, the pending-paint flag, the shutdown flag | L2 `Scene` / `LayerState` |
| the bitmaps | L2 `Scene` / `BitmapState` |
| the pending decode of a main image whose pixels have not landed yet | L2 `Scene` / `BitmapState` (`pending`) |
| the decode pool, its workers and its queue | L2 `engine::imagedecode::DecodePool`, attached to the host by the composition root |
| the focus | L2 `Scene` (per window and domain) |
| the projection and the draw order | L2 `Scene` |
| the input state | L2 `InputState` |
| the scene id, the attached flag, the cached `Font` wrapper, the pending cursor X | the L3 `LayerInst` payload |
| the active transitions, the cache revisions, the modal stack, the capture-release queues | L3 `LayerExtras` |
| the scene id → script-object mapping | L3 `VisualState` |
| the movie playback, the transform matrices, the record buffers | the `layerEx` plugin state |

---

## 6. Invariants

* The sibling vector and the `z_order` keys stay consistent: every relative
  reorder renumbers the siblings.
* Every relative-order operation leaves absolute mode; only the `absolute`
  setter enters it.
* `absoluteOrderMode` is a property of the parent, not of the child.
* A primary layer is forced visible when it is attached, and it refuses to
  move.
* A layer that is **not constructed** has no scene id; every member then
  raises the "not constructed" error.
* A layer whose scene record is gone raises the "no longer exists" error.
* Writing a stored value that the layer already holds is a **no-op**: it does
  not bump the cache revision.
* A layer that is **invalidated** has `shutdown = true` set *before* any
  detach or blur.
* **A pending main image carries the probed size.** The layer's image width,
  image height and clip are the loaded image's geometry before its pixels
  land; every read of the main image's pixels waits for the decode.
* **A failed pending decode installs a blank, not an exception.** The main
  image keeps the probed size and every pixel becomes transparent; the
  failure is logged. The load already returned, so there is no call to fail.
* **Every path that reads the main image's pixels waits for the decode** —
  the pixel accessors, the copy members, the text and filter operations and
  the `layerExDraw` plugin paths.
* **Every path that reads the main image's pixels refuses an image that
  declares more pixels than its buffer holds.** The pixel accessors and the
  pixel operations raise rather than read past the buffer.
* **A main image that declares more pixels than its buffer holds has no
  address.** `mainImageBuffer` and `mainImageBufferForWrite` answer `0`, and
  the copy seams — `assignImages`, `copyFromBitmapToMainImage`,
  `copyToBitmapFromMainImage`, `independMainImage` and `saveLayerImage` —
  raise instead of copying or encoding an empty buffer.

---

## 7. Errors

| Message | Raised when |
|---|---|
| `"Layer: compat context is not bound"` | the binding is not installed |
| `"Layer: no window in the scene — create a Window first"` | the constructor with no window in the scene |
| `"Layer: this layer is already constructed"` | the constructor ran twice |
| `"Layer: could not allocate the default image"` | the 32×32 default main image cannot be allocated |
| `"Layer: this layer is not constructed"` | the payload has no scene id |
| `"Layer: this layer no longer exists"` | the scene record is gone |
| `"Layer.setPos requires 2 arguments"`, `"Layer.setSize requires 2 arguments"`, `"Layer.setImagePos requires 2 arguments"`, `"Layer.setImageSize requires 2 arguments"`, `"Layer.setClip requires 4 arguments"`, `"Layer.fillRect requires 5 arguments"`, `"Layer.assignImages requires 1 argument"` | the arity checks |
| `"Layer.setPos: Invalid argument"` | a negative extent in the 4-argument form |
| `"Layer.setPos: Cannot move primary layer"`, `"Layer: Cannot move primary layer"` | a real move of the primary layer |
| `"Layer.setSizeToImageSize: Not drawable layer type"` | no main image |
| `"Layer.setImagePos: invalid image position"`, `"Layer.imageLeft: invalid image position"`, `"Layer.imageTop: invalid image position"` | the image-offset guard is violated |
| `"Layer.setImageSize: cannot create an empty layer image"`, `"Layer.imageWidth: Cannot create empty layer image"`, `"Layer.imageHeight: Cannot create empty layer image"` | a zero extent |
| `"Layer.assignImages: not a Layer object"` | the argument is not a `Layer` |
| `"Layer.assignImages: the bitmap {id} holds {bytes} pixel bytes instead of its {width}x{height} image"`, `"Layer.copyFromBitmapToMainImage: the bitmap {id} holds {bytes} pixel bytes instead of its {width}x{height} image"`, `"Layer.copyToBitmapFromMainImage: the bitmap {id} holds {bytes} pixel bytes instead of its {width}x{height} image"`, `"Layer.independMainImage: the bitmap {id} holds {bytes} pixel bytes instead of its {width}x{height} image"`, `"Layer.saveLayerImage: the bitmap {id} holds {bytes} pixel bytes instead of its {width}x{height} image"` | the image's pixel buffer is not the bytes its size declares |
| `"Layer.copyFromBitmapToMainImage: not a Bitmap"` | the argument is not a `Bitmap` |
| `"Layer.{member}: Layer: the main-image image {id} does not hold its pixels"` | a pixel member reads a main image that declares more pixels than its buffer holds |
| `"Layer: the destination image {id} does not hold its pixels"`, `"Layer: the source image {id} does not hold its pixels"` | a pixel operation copies between images that declare more pixels than their buffers hold |
| `"Layer: the source must be a Layer object"` | a `Bitmap` passed to a Layer-only member |
| `"Layer: the source is not a Layer or Bitmap object"` | an unresolvable source |
| `"Layer.moveBefore: cannot move a layer next to itself or a non-sibling"` | a self-move or a non-sibling target |
| `"Layer.moveBefore: this layer no longer exists"`, `"Layer.moveBefore: the sibling no longer exists"`, `"Layer.moveBefore: this layer has no parent"` | a missing record, sibling or parent |
| `"Layer.bringToBack: this layer has no parent"` | the layer has no sibling list |
| `"Layer.absolute: cannot move the primary layer or a siblingless layer"`, `"Layer.order: cannot move the primary layer or a siblingless layer"` | a parentless layer |
| `"Layer.setMode: cannot set mode to an ancestor of a modal layer"` | the modal stack's top is an ancestor-or-self |
| `"Layer.setMode: cannot set mode to a disabled or parent-hidden layer"` | the layer's own `enabled` is false, or an ancestor is hidden |
| `"Layer.parent: Cannot set parent self"` | `layer.parent = layer` |
| `"Layer.parent: Cannot move to under Other primary layer"` | the target parent is in another focus domain |
| `"Layer.<name>: requires N arguments"` | an event handler's arity check |
| `"visual: no input state attached"` | `setCursorPos` / `cursorY` with no attached input state |

---

## 8. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/layer.rs` | every member the parity report listed as missing is registered; the event handlers are assignable and default to a no-op; the pixel, geometry, focus, ordering, transition and mode behaviour; the **invalidation** cases — writing the stored value does not bump the cache revision, a changed write does; the lifecycle — a subclass member write before the base constructor lands, a repeated parent cycle releases the layers, bitmaps and fonts, a parent invalidation severs its children but leaves their script objects and records valid until their own finalizers run, and a collected child releases its font wrapper. |
| `crates/compat/tests/layer_behaviour.rs` | the reference behaviour suite: the blend numbering round-trips, the event arity counts, the focus search, the cursor and attention, the ordering, the modal stack, the pixel argument counts and semantics, the geometry and image rules; a hit test of a layer image that declares more pixels than its buffer holds is a miss. |
| `crates/compat/tests/layer_geometry.rs` | the geometry rules. |
| `crates/compat/tests/visual.rs` | the scene round-trips and the cross-object properties. |
| `crates/compat/tests/focus_behaviour.rs` | the focus model. |
| `crates/compat/tests/dispatch_visual.rs` | the receiver routing. |
| `crates/compat/tests/bitmap.rs` | the `Bitmap` class, and the main-image addresses of a layer whose image declares more pixels than its buffer holds. |
| `crates/compat/tests/pending_image.rs` | the deferred load of a layer image: the image size is known before the pixels land, the first pixel read waits for the decode, and a probe failure raises at the load; the hit test and the pointer hit test wait for the pending layer image. |

---

## See also

* [`focus.md`](focus.md) — the focus model
* [`pixels.md`](pixels.md) — the pixel operations
* [`transitions.md`](transitions.md) — `beginTransition` and the script cache
* [`common.md`](common.md) — the object registry and the re-parenting
* [`bitmap.md`](bitmap.md) — the `Bitmap` class and the shared load path
* [`../../../../arch/l2.md`](../../../arch/l2.md) §2 — the logical layer tree on the engine side
