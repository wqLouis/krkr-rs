# `compat::natives::image_function` — the `ImageFunction` class

Source: `crates/compat/src/natives/image_function.rs` · layer **L3** · class
`ImageFunction`

`ImageFunction` is the class of **static pixel operations**: blits, fills,
flips, blurs, gamma, grayscale and text drawing on a `Bitmap`.

The class is registered as an instance class with an empty payload; every
operation is a **static** member. The pixels themselves belong to the L2
scene's bitmaps; see [`../../code/engine/layer_ops.md`](../../code/engine/layer_ops.md)
for the primitives.

---

## 1. Registered surface

**Static methods (12):** `operateAffine`, `operateRect`, `operateStretch`,
`flipLR`, `flipUD`, `adjustGamma`, `doBoxBlur`, `doGrayScale`, `fillRect`,
`colorRect`, `drawText`, `drawGlyph`.

**Instance:** the constructor `ImageFunction` only. The instance carries no
state.

There are no properties.

---

## 2. The result

Every operation returns a **fresh `Rect`** describing the region it touched,
or **void** when the operation touched nothing. The rectangle is built by
evaluating `new Rect()` inside the callback, so it is a real script object.

If that evaluation fails, the member logs a warning and returns **void**
rather than raising.

---

## 3. The operations

| member | behaviour |
|---|---|
| `operateRect(dst, dleft, dtop, src, srect, cliprect, mode, face, opa, hda)` | blits `src[srect]` to `dst` at `(dleft, dtop)`, clipping the destination point and the source rect against `cliprect` |
| `operateStretch(dst, src, dstrect, srcrect, cliprect, mode, face, opa, type, hda, typeopt)` | scales `src[srcrect]` into `dst[dstrect]` |
| `operateAffine(dst, src, a, b, c, d, tx, ty, srcrect, cliprect, affine, mode, face, opa, type, hda)` | maps the source rect through an affine matrix (or three destination points) |
| `flipLR(dst, rect)` | mirrors a region horizontally, in place |
| `flipUD(dst, rect)` | mirrors a region vertically, in place |
| `adjustGamma(bmp, rgamma, rfloor, rceil, ggamma, gfloor, gceil, bgamma, bfloor, bceil, cliprect, isaddalpha)` | applies a per-channel gamma ramp |
| `doBoxBlur(bmp, xblur, yblur, clipRect, isalpha)` | a box blur |
| `doGrayScale(bmp, clipRect)` | a luma grayscale |
| `fillRect(bmp, value, rect, isalpha, cliprect)` | fills a rectangle |
| `colorRect(bmp, value, opa, rect, face, cliprect)` | fills a rectangle with a blend face |
| `drawText(bmp, font, x, y, text, color, opacity, antialias, …, cliprect)` | draws a string with a `Font` |
| `drawGlyph(bmp, x, y, glyph, color, opacity, antialias, …, cliprect)` | draws a glyph |

The defaults: `mode = omAlpha (2)`, `face = dfAlpha (0)`, `opa = 255`,
`hda = false`, `type = 0`, `xblur`/`yblur` = `1`, `isalpha` = true,
`antialias` = true, `opacity` = 255, `shadowlevel` = 0, `shadowcolor` = 0,
`shadowwidth` = 0, the shadow offsets = `(0, 0)`.

The source rectangle defaults to the whole source, the destination rectangle
to the whole destination and the clip rectangle to the whole destination.

`operateAffine` with `affine = true` (the default) reads `a, b, c, d, tx,
ty` as the reference's affine matrix (`x' = a·x + c·y + tx`,
`y' = b·x + d·y + ty`) mapping the source rect's pixel centres; with
`affine = false` the six numbers are three destination points (the source
rect's left-top, right-top and left-bottom).

`doBoxBlur` with `xblur == 0 && yblur == 0` is a no-op.

`adjustGamma` called with only a bitmap does nothing and returns void.

---

## 4. The faces

`colorRect`'s `face` selects how the colour is combined:

| face | behaviour |
|---|---|
| `dfAlpha` | `fill_color_on_alpha`, or `remove_const_opacity` when `opa <= 0` |
| `dfAddAlpha` | `fill_color_on_add_alpha` when `opa >= 0`; otherwise **no update** |
| `dfOpaque` | `fill_color_hold_alpha` — the RGB is replaced, the alpha is kept |
| `dfMask` | `layer_ops::fill_mask` — the **blue** byte becomes the alpha plane |

Any other face is clamped back to `dfAlpha`. `omAuto` (128) is normalized to
`omAlpha` (2).

`fillRect`'s `isalpha` flag decides whether the fill copies the colour
**including** its alpha (`isalpha = true`, the default) or replaces the RGB
and **holds** the destination alpha (`isalpha = false`).

Script colours are `0xAARRGGBB`; they are converted to straight-alpha
`[r, g, b, a]` internally.

---

## 5. Text and glyphs

`drawText` reads the face, size, bold, italic and angle from the `Font`
argument and rasterizes through the shared text resolver. The colour's alpha
is forced to 255 and the separate `opacity` argument carries the alpha.

`drawGlyph` takes the glyph's text from a string or from an object's `text`,
`char` or `character` member. An empty text draws nothing and returns void.

---

## 6. State ownership

| state | owner |
|---|---|
| the pixels, the bitmap dimensions, the font state | the **L2 scene** |
| the text faces and the host image codec | the shared **per-engine resource resolver** (`crate::resource`) |
| the instance payload | a one-byte dummy — the instance is stateless |

There is no dedicated per-engine dispatch state for this class.

---

## 7. Errors

| shape | example |
|---|---|
| `"ImageFunction.<member> requires N arguments"` | `ImageFunction.operateRect requires 6 arguments` |
| `"ImageFunction: compat context is not bound"` | the engine binding is missing |
| `"ImageFunction: no scene attached"` | no L2 scene |
| `"ImageFunction.<member>: the argument is not a Bitmap object"` | a non-object bitmap argument |
| `"ImageFunction.<member>: the argument is not a Font object"` | a non-object font argument |
| `"ImageFunction.<member>: no bitmap <id>"` | the bitmap id does not exist |
| `"ImageFunction: the destination bitmap <id> does not exist"` / `"… the source bitmap <id> does not exist"` | the same, from the two-bitmap path |
| `"ImageFunction.<member>: the rectangle is outside the bitmap"` | an out-of-bounds region |
| `"ImageFunction: the Rect argument is not a Rect object"` | an object that is not a native `Rect` |
| `"ImageFunction: the Rect argument is invalid: {error}"` | the rect payload lookup failed |
| `"ImageFunction: argument is not a number"` | a non-numeric matrix element |
| `"ImageFunction: argument <i> is not a number"` | a non-numeric optional argument |
| `"ImageFunction.drawGlyph: unsupported glyph argument"` | the glyph argument is neither a string nor a usable object |
| `"ImageFunction.drawGlyph: no font is registered"` | no face could be resolved |

A **null** rectangle argument is skipped (treated as absent), which is what
the reference's `param[i]->Type() == tvtObject` guard does. A plain
dictionary is **not** accepted as a rectangle — it raises
`the Rect argument is not a Rect object`.

The shared integer conversion errors are reported unprefixed: `value is null,
not an integer`, `value is an object, not an integer`, `value is an octet, not
an integer`, `value cannot be converted to an integer`.

---

## 8. Tests

`crates/compat/tests/image_function.rs` covers the member registration, the
constructor, `fillRect` in both alpha modes, `colorRect` with `dfMask` and
`dfOpaque`, `flipLR` over a sub-rect, `flipUD` over the whole bitmap, the
out-of-bounds rejection, `doGrayScale` with the reference's exact luma,
`adjustGamma` as a no-op on identity data, `doBoxBlur` as an average over the
kernel, `operateRect`, `operateStretch`, `operateAffine` in points mode, the
`drawText` / `drawGlyph` errors, and the argument-count and unknown-bitmap
errors.

---

## 9. Known gaps

1. **`typeopt` (argument index 10 of `operateStretch`) is ignored** — the
   cubic sub-kernel is chosen by `stretch_type` instead.
2. **`hold_alpha` is emulated for `operateAffine`**: the alpha plane is
   snapshotted and restored around the blit, because the L2 affine primitive
   has no such argument. `operateRect` and `operateStretch` pass it natively.
3. **`adjustGamma`'s additive-alpha path is emulated**: convert to straight
   alpha, apply the gamma, convert back.
4. **`doBoxBlur` is composed in L3** — the region is extracted into a scratch
   bitmap, blurred by L2 and blitted back — rather than being a single L2
   primitive.
5. **`drawGlyph` draws text**, not a reference glyph: the face, the 16 px
   size and the style are fixed.
6. **`drawText` and `drawGlyph` force the colour alpha to 255**; the alpha
   comes from the separate `opacity` argument.
7. **Only `dfAlpha`, `dfAddAlpha`, `dfOpaque` (and `dfMask` for `colorRect`)
   are honoured**; other faces clamp back to `dfAlpha`.
8. **A failed result-`Rect` creation is soft** — a warning and void instead
   of a raise.

---

## 10. Related documents

| topic | document |
|---|---|
| the L2 pixel operations | [`../../code/engine/layer_ops.md`](../../code/engine/layer_ops.md), [`../../code/engine/raster.md`](../../code/engine/raster.md) |
| the L2 text engine | [`../../code/engine/text.md`](../../code/engine/text.md) |
| the L3 layer | [`../../arch/l3.md`](../../arch/l3.md) |
