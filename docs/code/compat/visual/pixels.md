# `natives/visual/layer/pixels.rs` — the pixel operations, the image path and the text path

**Source:** `crates/compat/src/natives/visual/layer/pixels.rs`

This module registers 37 `Layer` members: the blits and the filters, the
pixel accessors, the image load/save path and the text drawing.

Every method composes the L2 `engine_api::layer_ops` and
`engine_api::raster` primitives directly — it never calls into
`ImageFunction`.

---

## 1. The members

### The blits

| Member | Behaviour |
|---|---|
| `operateRect(dx, dy, src, sx, sy, sw, sh[, mode[, opa]])` | blends the source rect into the destination with the requested operation mode. `omAuto` resolves from the source layer's type. |
| `operateStretch(dx, dy, dw, dh, src, sx, sy, sw, sh[, mode[, opa[, type]]])` | resamples the source rect into the destination rect and blends it. |
| `operateAffine(src, sx, sy, sw, sh, affine, x1..y3[, mode[, opa[, type]]])` | affine-maps the source rect onto the three destination corners and blends it. |
| `affineCopy(...)` | affine-maps the source onto the three corners, replacing the destination (optionally clearing it to `neutralColor` first). |
| `affinePile(...)` | affine-maps and source-over composites the source onto the destination. |
| `affineBlend(...)` | affine-maps and blends the source onto the destination (an opaque copy on `dfOpaque`). |
| `stretchCopy(dx, dy, dw, dh, src, sx, sy, sw, sh[, type])` | resamples the source rect into the destination rect, replacing the pixels (RGB, and the alpha unless `holdAlpha` on `dfOpaque`). |
| `stretchPile(...)` | resamples and source-over composites the source rect. |
| `stretchBlend(...)` | resamples and blends the source rect. |
| `copyRect(dx, dy, src, sx, sy, sw, sh[, opa])` | copies the source rect to `(dx, dy)`; the plane selection depends on the destination's draw face. |
| `pileRect(dx, dy, src, sx, sy, sw, sh[, opa])` | source-over composites the source rect at `(dx, dy)`. **Layer-only** source. |
| `blendRect(dx, dy, src, sx, sy, sw, sh[, opa])` | blends the source rect at `(dx, dy)`. **Layer-only** source. |
| `piledCopy(dx, dy, src, sx, sy, sw, sh[, opa])` | copies the source **layer's composited subtree** (its own image plus its visible children). **Layer-only** source. |
| `copy9Patch(src)` | copies the 9-patch source and returns its margin `Rect`, or `void`. |
| `copyToBitmapFromMainImage(bitmap)` | replaces the bitmap's content with the layer's main image (resizing the bitmap). |
| `fillRect(x, y, w, h, color[, opa])` | fills the rect with the colour. |
| `colorRect(x, y, w, h, color[, opa])` | fills the rect with `0xAARRGGBB`; a **negative** `opa` removes the const opacity instead of painting. |

### The filters

| Member | Behaviour |
|---|---|
| `flipLR()`, `flipUD()` | mirror the main image (and the province plane). |
| `doBoxBlur([xblur[, yblur]])` | box-blurs the clipped region; blurs the alpha on `dfAlpha` and the RGB on every other face. |
| `gaussianBlur(radius[, sigma])` | Gaussian-blurs the clipped region. `sigma` defaults to `max(1, radius / 2.5)` **only when the argument is missing**. |
| `adjustGamma(r, g, b, gamma, floor, ceil, ...)` | applies the 9-parameter gamma curve. A zero-argument call is a **valid no-op**. |
| `doGrayScale()` | converts the clipped region to grayscale. |
| `light(brightness, contrast)` | applies brightness and contrast to the clipped region. |
| `convertType(fromtype)` | converts the alpha plane between `dfAlpha` and `dfAddAlpha` (one direction only). |

### The pixel accessors

| Member | Behaviour |
|---|---|
| `getMainPixel(x, y)` | `0x00RRGGBB` — the RGB of the main image with the alpha masked out. |
| `setMainPixel(x, y, color)` | writes the RGB; the alpha (mask) plane survives. The write is **clipped to the layer's clip rect**. |
| `getMaskPixel(x, y)` | the alpha byte `0..255`. |
| `setMaskPixel(x, y, alpha)` | writes the alpha byte, **truncated to its low 8 bits, not clamped** — `256` → `0`, `-1` → `255`, `300` → `44`. |
| `getProvincePixel(x, y)`, `setProvincePixel(x, y, n)` | the 8-bit province plane (registered in `layer/mod.rs`). |

`getMainPixel` and `getMaskPixel` **never raise** on a missing image — they
return `0`. `setMainPixel` and `setMaskPixel` set `imageModified`.

### The image and text paths

| Member | Behaviour |
|---|---|
| `independProvinceImage([copy])` | allocates the layer's 8-bit province plane; the default keeps the existing content, `copy = false` clears it. |
| `loadImages(name[, key])` | reads and probes the storage image, queues the decode in the L2 pool when possible, and installs it as the layer's main image (a pending bitmap until the pixels land). Returns `null` — the reference's absent metainfo. |
| `loadProvinceImage(name)` | loads an 8-bit province plane whose size **must** equal the main image's size. The **red channel** of every pixel becomes the province value. |
| `saveLayerImage(name[, type])` | encodes the main image through the host encoder and writes it through the storage. `type` defaults to `"bmp"`. |
| `drawText(x, y, text, color[, opacity[, antialias[, shadowLevel[, shadowColor[, shadowWidth[, shadowOffsetX[, shadowOffsetY]]]]]]])` | rasterizes a text run into the layer's main image. |
| `drawGlyph(x, y, glyph, color[, …same optionals…])` | the same, but the text comes from the glyph object's `text`/`char`/`character` member. |
| `dump()` | logs the layer's structure at debug level. |

---

## 2. The coordinate and rounding conventions

* Every rectangle is **half-open**: `[left, top, right, bottom)`.
* The **destination** rect of the stretch/affine family is normalised so
  `right >= left` and `bottom >= top`.
* A **source** rect is always built with `saturating_add`.
* **Fill** (`colorRect`): the destination is the intersection of the
  requested rect with the clip; the clip is the layer's `ClipRect`, or the
  whole image when unset.
* **Copy / pile / blend**: the destination point is pushed up to the clip's
  left/top edge and the source rect is shrunk from the right/bottom by the
  same amount; the operation is skipped entirely when the source collapses to
  zero width or height.
* **Stretches**: the destination rect is normalised, then intersected with
  the clip to give the visible destination. The source rect is remapped into
  that clipped window with integer truncation.
* **Affines**: the three destination corners are the images of
  `(-0.5, -0.5)`, `(width - 0.5, -0.5)` and `(-0.5, height - 0.5)` when
  `affine` is `true` — the reference's **pixel-centre** convention. When it
  is `false`, the six numbers are the three corner points themselves, in the
  order LT, RT, LB.
* The 1:1 copies are exact integer pixel transfers. Resampling happens only
  in the stretch and affine paths, which use a half-pixel-centre mapping and
  a bilinear sample respectively.
* **The affine primitive takes no clip argument**, so the destination is
  snapshotted before the blit and afterwards every pixel **outside** the clip
  is restored, and the alpha **inside** the clip is restored to its
  pre-blit value (the reference's hold-alpha behaviour for the affine).

### The clip-before-validation order

The reference's order is preserved exactly:

| Method | Order |
|---|---|
| `operateRect` | the clip first, then the mode/face validation. |
| `operateStretch` | the clip first, then the validation. |
| `stretchPile` / `stretchBlend` | the face gate **before** the clipping. |
| `stretchCopy` | the face validated **after** the clip intersection. |

---

## 3. The draw face

| Constant | Value | Meaning |
|---|---|---|
| `dfAlpha` | 0 | |
| `dfOpaque` | 1 | |
| `dfMask` | 2 | |
| `dfProvince` | 3 | |
| `dfAddAlpha` | 4 | |
| `dfAuto` | 128 | the face is derived from the layer type |

`draw_face(scene, id)` is the port of the reference's `UpdateDrawFace`: when
`layer.face != dfAuto` the stored face is used verbatim; otherwise the
blend decides it.

| Layer type / blend | Face |
|---|---|
| `ltOpaque` | `dfOpaque` |
| `ltAlpha` | `dfAlpha` |
| `ltAdditive`, `ltSubtractive`, `ltMultiplicative`, `ltScreen`, `ltDodge`, `ltDarken`, `ltLighten` | `dfOpaque` |
| `ltAddAlpha` | `dfAddAlpha` |
| every Photoshop type | `dfAlpha` |

### The per-method face gates

| Method | Accepted faces |
|---|---|
| `operateRect`, `operateStretch`, `operateAffine` | the face-**sensitive** modes need `dfAlpha`/`dfOpaque`/`dfAddAlpha`; the face-independent modes accept any |
| `affineCopy` | `dfAlpha`/`dfAddAlpha`/`dfOpaque` |
| `affinePile`, `affineBlend` | `dfAlpha`/`dfOpaque` |
| `stretchCopy` | `dfAlpha`/`dfAddAlpha`/`dfOpaque` |
| `stretchPile`, `stretchBlend` | `dfAlpha`/`dfOpaque` |
| `pileRect`, `blendRect` | `dfAlpha`/`dfOpaque` |
| `copyRect` | dispatches per face — `dfAlpha`/`dfAddAlpha` a full copy; `dfOpaque` an RGB copy (the alpha held iff `holdAlpha`); `dfMask` the alpha only; `dfProvince` zeroes the province region; **any other face is a silent no-op**, matching the reference's missing `default:` case |
| `colorRect` | `dfAlpha`, `dfAddAlpha`, `dfOpaque`, `dfMask`, `dfProvince`; **any other face is a silent no-op** |

---

## 4. The operation modes

The `tTVPBlendOperationMode` shares the native `tTVPLayerType` numbering, so
the canonical native encoding of a mode *is* its operation mode. The full
vocabulary is the `lt*` table on [`layer.md`](layer.md) §4.

| Constant | Value |
|---|---|
| `omOpaque` | 1 |
| `omAlpha` | 2 |
| `omAuto` | 128 — "use the source layer's type" |

`omAuto` resolution: a `Layer` source contributes its `layer.blend`; a
`Bitmap` source has no layer type and the reference keeps `omAlpha`.

**Face-sensitive modes** are `omAlpha`, `omOpaque` and `omAddAlpha` (12);
they require `dfAlpha`/`dfOpaque`/`dfAddAlpha` and otherwise the reference
raises. Every other mode — additive, subtractive, multiplicative, darken,
lighten, screen and all the `omPs*` spellings — is face-independent.

The default mode is `omAuto` and the default opacity is 255, clamped to
`0..255`.

---

## 5. The argument counts

| Method | Required `argc` |
|---|---|
| `operateRect` | 7 |
| `operateStretch` | 9 |
| `affineCopy`, `affinePile`, `affineBlend`, `operateAffine` | 12 |
| `stretchCopy`, `stretchPile`, `stretchBlend` | 9 |
| `copyRect`, `pileRect`, `blendRect`, `piledCopy` | 7 |
| `copy9Patch` | 1 |
| `copyToBitmapFromMainImage` | 1 |
| `colorRect` | 5 |
| `convertType` | 1 |
| `gaussianBlur` | 1 |
| `light` | 2 |
| `getMainPixel`, `getMaskPixel` | 2 |
| `setMainPixel`, `setMaskPixel` | 3 |
| `loadImages` | 1 |
| `loadProvinceImage` | 1 |
| `saveLayerImage` | 1 |
| `drawText`, `drawGlyph` | 4 |
| `adjustGamma` | 0 — a zero-argument call is a **valid no-op** |
| `doBoxBlur` | 0 — `xblur`/`yblur` default to 1 |
| `flipLR`, `flipUD`, `doGrayScale`, `dump`, `independProvinceImage` | no check |

The counts mirror the reference's `TJS_BEGIN_NATIVE_METHOD_DECL` counts.

### The source-class restriction

The reference splits the source argument between `TVPSpecifyLayer` (Layer
only) and `TVPSpecifyLayerOrBitmap` (Layer or Bitmap):

| Layer-only | Layer or Bitmap |
|---|---|
| `pileRect`, `blendRect`, `piledCopy`, `stretchPile`, `stretchBlend`, `affinePile`, `affineBlend` | `copyRect`, `copy9Patch`, `stretchCopy`, `operateRect`, `operateStretch`, `affineCopy`, `operateAffine` |

A `Bitmap` passed to a Layer-only member raises
`"Layer.{method}: the source must be a Layer object"`.

---

## 6. `holdAlpha`

`Layer.holdAlpha` drives:

* `copyRect` on `dfOpaque`: the alpha plane is **held** (RGB only) when
  `holdAlpha` is true, otherwise RGB and the alpha are copied;
* `stretchCopy` on `dfOpaque`: the same rule;
* `pileRect`, `blendRect`, `operateRect`, `operateStretch`, `stretchPile`,
  `stretchBlend`: the flag is forwarded to the L2 blend, which composites
  with the real destination alpha and then restores the old alpha byte;
* the affine family: the method-level hold flag is ANDed with the layer's,
  and the alpha inside the clip is restored from the pre-blit snapshot;
* `colorRect` on `dfOpaque` and `fillRect`: the same.

---

## 7. The image load and save path

### `loadImages(name[, key])`

1. `key` defaults to `COLOR_KEY_NONE` (`0x1fff_ffff`); a void second
   argument keeps the default.
2. The load goes through the scene's **decoded-image cache**, keyed by
   (name, colour key, mode, 0, 0), so a repeated load of the same file
   decodes once. A cache miss reads the storage and probes the header on
   the calling thread, then queues the decode in the L2 pool and installs a
   pending bitmap with the probed size; the pixels land when the scene
   resolves it. A colour-keyed or province load, a load with no pool and a
   refused job decode synchronously.
3. The storage read tries the **plain name first**; only on a miss are the 21
   common image extensions probed in order (`.webp .bmp .dib .jpeg .jpg .jif
   .png .tlg .tlg5 .tlg6 .gif .tif .tiff .tga .dds .pnm .ico .qoi .hdr .exr
   .ff`). A miss raises `"storage not found: {name}"`.
4. The colour key is applied, and the decoded image is checked for
   consistency — `"the decoded image is malformed"`.
5. The **existing** bitmap is reused, so repeated loads do not leak scene
   bitmaps.
6. The reference image-size adjustment runs — the layer rect shrinks to a
   smaller image, the image offsets are clamped and the clip is reset. A
   failure raises `"the layer image could not be sized"`.
7. The return value is **`null`** — the reference returns the graphic
   metainfo, which is `null` when the format carries none. Returning `void`
   would make a game's `ret != null` guard pass and then fault on
   `ret.mode`.

### `loadProvinceImage(name)`

The decoded size **must** equal the main image's size, otherwise
`"the province image is {w}x{h} but the main image is {mw}x{mh}"`. The
**red channel** of every pixel becomes the province value. A layer with no
main image raises `"the layer has no main image"`.

### `saveLayerImage(name[, type])`

`type` defaults to `"bmp"`; an empty string also becomes `"bmp"`. The layer's
main image is read and handed to the host encoder, and the media prefix is
stripped first. With no encoder installed the error is
`"no image encoder is installed"`.

---

## 8. The text path

`drawText` and `drawGlyph` share `layer_draw_text_common`.

| Argument | Meaning |
|---|---|
| 0 | x |
| 1 | y |
| 2 | the text (or the glyph) |
| 3 | the colour |
| 4 | the opacity (default 255, clamped `0..255`) |
| 5 | antialias (default `true`) |
| 6 | the shadow level (default 0) |
| 7 | the shadow colour (default 0) |
| 8 | the shadow width (default 0) |
| 9, 10 | the shadow offset |

* The colour's **own alpha is replaced by 255**; the effective alpha is the
  `opacity` argument. The shadow colour is treated the same way.
* `drawGlyph` extracts the text through `resource::glyph_text` — a string, or
  an object's `text`, `char` or `character` member. Anything else raises
  `"Layer.drawGlyph: unsupported glyph argument"`.
* The layer's font is read from the L2 scene (`face`, `height`, `bold`,
  `italic`, `angle`); the face falls back to `MS Gothic` and the size to
  `16` when the font has none. The angle is in tenths of a degree.
* The destination is the layer's own main image; a layer without one gets a
  transparent one allocated from its rect. A zero-sized rect raises
  `"unsupported layer type (no main image)"`.
* The L3 `ResourceResolver` resolves the face, uses a **pre-rendered** font
  mapped under the same key when one exists, and otherwise lays the text out
  and rasterizes the coverage.
* The **shadow pass runs first** when the shadow level is not 0; a shadow
  level of 255 with a zero shadow width reuses the unblurred character data
  (the reference's "normal shadow"). Each character's coverage is rasterized
  **once** and reused for the shadow and the main pass.
* The clip rectangle is passed down, so the text cannot draw outside it.
* When there was ink, `imageModified` is set.

---

## 9. State ownership

| State | Owner |
|---|---|
| the pixel data (the main image) | L2 `Scene` → `BitmapState` (straight-alpha RGBA8) |
| the province plane | L2 `Scene`, per layer — an 8-bit plane separate from the RGBA bitmap |
| the geometry, the clip, the face, `holdAlpha`, the blend, `neutralColor`, `imageModified` | L2 `Scene` → `LayerState` |
| the decoded-image cache | L2 `Scene`, keyed by (name, colour key, mode, width, height) |
| the layer's `Font` object | the L3 `LayerInst` payload |
| the host image codec | the L3 `ResourceResolver` |

---

## 10. Errors

| Message | Raised when |
|---|---|
| `"Layer.<method>: this layer is not constructed"` | the payload has no scene id |
| `"Layer.<method>: compat context is not bound"` | the binding is not installed |
| `"Layer.<method>: this layer no longer exists"` | the scene record is gone |
| `"Layer.<method>: this layer has no main image"` | the layer has no drawable image |
| `"Layer.<method> requires N argument(s)"` | the arity checks |
| `"Layer.<method>: unsupported draw face {face}"` | a rejected face |
| `"Layer: the operation mode {mode} cannot draw on draw face {face}"` | a face-sensitive mode on an incompatible face |
| `"Layer: the source must be a Layer object"` | a `Bitmap` passed to a Layer-only member |
| `"Layer: the source is not a Layer or Bitmap object"` | an unresolvable source |
| `"Layer: the source layer has no main image"` | the source layer is imageless |
| `"Layer.colorRect: negative opacity is not supported on dfAddAlpha"` | a negative opacity on the add-alpha face |
| `"Layer.convertType: cannot convert the layer type in the given direction"` | an unsupported conversion direction |
| `"Layer.gaussianBlur: radius must be positive"`, `"Layer.gaussianBlur: sigma must be positive"` | a non-positive radius or sigma |
| `"Layer.loadImages: storage not found: {name}"` | the image does not resolve |
| `"Layer.loadImages: the decoded image is malformed"` | the decoded image is inconsistent |
| `"Layer.loadImages: the layer image could not be sized"` | the reference size adjustment failed |
| `"Layer.loadProvinceImage: the province image is {w}x{h} but the main image is {mw}x{mh}"` | the province size mismatch |
| `"Layer.loadProvinceImage: the layer has no main image"` | the layer is imageless |
| `"Layer.saveLayerImage: no image encoder is installed"` | the host encoder is missing |

### Deliberate no-ops

These are **not** errors — they match the reference:

* every method that needs a main image returns silently when the layer's
  main image has been deallocated;
* `gaussianBlur` and `light` are silent on an imageless layer (the reference
  logs and returns);
* `copyRect` and `colorRect` on an out-of-vocabulary face fall through
  silently;
* `adjustGamma` with zero arguments is a valid no-op;
* `doBoxBlur` with `xblur == 0 && yblur == 0` returns without touching the
  image.

---

## 11. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/layer.rs` | the pixel behaviour: `colorRect` and the pixel accessors, the flips, `copyRect` between layers, `operateRect` with the requested mode and with `omAuto`, the stretches and the affines, `composite` marking the layer modified, the filters, `convertType`, `copyToBitmap` and `copy9Patch`. |
| `crates/compat/tests/layer_behaviour.rs` | the reference behaviour: the pixel argument counts, the opacity defaults and the negative removal, the `gaussianBlur` sigma default distinguishing a void from a missing argument, `adjustGamma` parsing string reals and accepting no arguments, the mask alpha and the hold-mask plane, `setMaskPixel` truncating to eight bits, the Layer-only members rejecting a bitmap source, the face validations, the `dfAuto` face resolution, and the filters being silent on an imageless layer. |
| `crates/compat/tests/image_function.rs` | the same primitives through the `ImageFunction` surface. |
| `crates/compat/tests/bitmap.rs` | the codec seam the image path uses. |
| `crates/compat/tests/font.rs` | the pre-rendered font pixels matching the rasterizer's coverage. |

---

## See also

* [`layer.md`](layer.md) — the class these members belong to
* [`../resource.md`](../resource.md) — the resolver behind the text and image paths
* [`../bitmap.md`](bitmap.md) — the `Bitmap` class and the codec seam
* [`../../../../arch/l2.md`](../../../arch/l2.md) §8 — the L2 pixel operations
