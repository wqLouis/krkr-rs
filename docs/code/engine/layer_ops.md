# `engine::layer_ops` — in-place CPU pixel operations

Source: `crates/engine/src/layer_ops.rs`

The operations in this module rewrite the pixels of a
[`BitmapState`](scene.md) in place: fills, blits, colour conversions, blur,
gamma, 9-slice scaling and subtree compositing. They are the L2 half of the
`LayerExDraw` / `layerExImage` surface.

The scene stores straight-alpha RGBA8 (`r, g, b, a` per pixel), the same
pixel model `engine::raster::blend_pixel` uses; these operations run on those
straight-alpha channels directly.

---

## 1. Responsibility

* Apply the CPU-side image effects and copy operations the script surface
  exposes.
* Clip every operation against the destination bitmap **and** the layer's
  `ClipRect`.
* Mark the bitmap dirty after a write, so the engine re-uploads it. The
  writes that touch one rectangle mark that rectangle, so the upload sends
  the changed part and not the whole bitmap.
* Provide the two transition slots (the whole-subtree frame and the
  transition's self image) and the subtree compositing that fills them.

---

## 2. State it owns

None. Every function is a **free function** over the caller's
`&mut BitmapState` / `&Scene`; the module holds no globals, caches or locks.
The layer state lives in the [`Scene`](scene.md); the pixels live in the
`BitmapState`.

The two transition slots are **not** separate state: they are the layer's own
`cache_bitmap` and `transition_self` entries, so the projection and the cache
bookkeeping see them like any other layer bitmap.

---

## 3. Public API

### Types

| type | purpose |
|---|---|
| `RectI` | a half-open pixel rectangle `[x0, x1) × [y0, y1)` |
| `BlitSource<'a>` | a source bitmap plus the sub-rectangle a blit reads |
| `BlitOptions` | the blend state of a blit: `mode`, `opa`, `treat_as_opaque` (the source is read as an opaque image), `hold_alpha` (the destination's alpha is held) |
| `AffineBlit` | the corner map, resampler and background of `affine_blit` |
| `GammaAdjust` | the per-channel gamma exponents and floors/ceilings |
| `NoiseRng` | the deterministic noise generator behind `noise` |
| `COPY_MAIN` / `COPY_MASK` | the plane selection of `blit_plane` |

### Geometry helpers

`intersect`, `intersect_rect`, `clip_rect`.

`clip_rect(None)` yields the whole `i32` range, so an operation without a
`ClipRect` still clips to the bitmap's own bounds.

### Fills and colour operations

| function | operation |
|---|---|
| `fill_color_on_alpha` | source-over a constant colour, considering the destination alpha |
| `fill_color_on_add_alpha` | additive-alpha (premultiplied) fill |
| `fill_color_hold_alpha` | set RGB to a constant colour, holding the destination alpha |
| `fill_mask` | replace the alpha channel with a constant |
| `remove_const_opacity` | scale the alpha by `1 - level / 255` |
| `colorize` | reblend hue and saturation, preserving lightness |
| `noise` | add uniform per-channel noise, holding alpha |
| `do_gray_scale` | greyscale: a weighted sum of the three colour channels, written to all three |
| `adjust_gamma` | per-channel gamma with floors and ceilings |
| `light_contrast` | brightness/contrast adjustment |
| `convert_alpha_to_add_alpha` / `convert_add_alpha_to_alpha` | premultiplied ↔ straight alpha |

### Copies and blits

| function | operation |
|---|---|
| `blit_copy` | copy a source region over the destination |
| `blit_copy_clipped` | the same, with an explicit clip rectangle |
| `blit_over` | source-over composite with the destination alpha considered |
| `blit_plane` | copy the RGB plane, the alpha plane, or both, holding the other |
| `tile_rect` | tile a source bitmap over a rectangle, from a phase |
| `stretch_blit` / `stretch_blit_plane` | resample a source region into a destination rectangle |
| `stretch_blit_mode` | resample and blend with an operation mode |
| `operate_rect` | blend a source region onto the destination with a mode and opacity |
| `fill_operate_rect` | fill a rectangle with a solid colour through a blend mode |
| `affine_blit` | map the source rect's LT/RT/LB corners and resample |
| `affine_blit_clipped` | the same, with an explicit clip rectangle the pass writes inside |
| `copy_9patch` | scale a bitmap with a 9-slice margin |

### Effects

| function | operation |
|---|---|
| `blur_bitmap_in_place` | box blur over a region |
| `gaussian_blur` | separable Gaussian blur (edge-clamped) |
| `do_drop_shadow` | blur a copy of the alpha, composite it as a shadow, draw the original on top |
| `do_blur_light` | blur a copy, composite it, then composite it again with a light blend |
| `flip_lr` / `flip_ud` | mirror the bitmap |

### Subtree compositing and the transition slots

| function | operation |
|---|---|
| `render_layer_subtree(scene, root)` | composite the layer's subtree into one bitmap |
| `ensure_transition_display(scene, root, w, h)` | allocate or reuse the whole-subtree transition frame |
| `ensure_transition_self_image(scene, root, w, h)` | allocate or reuse the transition's self image |

Neither slot is allocated for a transition a GPU backend composites: that host
records the frame on the layer (`Scene::set_transition_frame`) and the
projection draws the two sources, so nothing is written to a bitmap per tick
(see [trans.md](trans.md) §6.1).

---

## 4. Invariants

* **Straight alpha everywhere.** The operations run on the straight-alpha
  channels; the additive-alpha variants convert explicitly.
* **The blend of a pass is resolved once.** The operation mode selects the
  kernel, the compatibility layer resolves the draw face's variant of the
  face-sensitive modes, and the per-pixel loop only applies the resolved
  kernel. The kernels follow the reference's blend methods
  (`reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3402-3411`,
  `:3474-3482`, `:3549-3556`): the alpha blend takes the destination's
  alpha into the mix, the channel mix (the Photoshop modes and the opaque
  face's alpha blend) does not and holds the destination's alpha, and the
  per-channel Photoshop functions are the tree's CPU twins
  (`reference/cpp/core/visual/gl/blend_functor_c.h:374-660`).

### The blend family and where each formula comes from

The reference has three blend shapes, and every operation mode names one of
them:

| shape | what it does | the reference |
|---|---|---|
| the alpha-correct source-over | the destination's alpha takes part in the mix and is recomputed from the source's alpha; a channel is `(s*sa*255 + d*da*(255-sa)) / (sa*255 + da*(255-sa))`, divided by that **exact** new alpha and rounded to the nearest 8-bit value | `reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3549-3556` (the CPU twin is `reference/cpp/core/visual/tvpgl.cpp:582-700`) |
| the channel mix | the source's alpha mixes the colours with `d + ((f - d) * a >> 8)` and the destination's alpha is held | `reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:3402-3411`, `:3474-3482` (the CPU twin is `reference/cpp/core/visual/gl/blend_functor_c.h:374-383`) |
| the copy family | the source replaces the destination, with the destination's alpha copied, held or made opaque | `reference/cpp/core/visual/ogl/RenderManager_ogl.cpp:2986`, `:3141-3147` (the CPU twin is `reference/cpp/core/visual/tvpgl.cpp:3326-3370`) |

Below the full opacity a copy pass is a blend of its own: the opacity mixes
the source's colour into the destination's and the destination's alpha is
held. A pass that reads the source as an opaque image takes the
alpha-correct source-over instead, so the destination's alpha takes part in
the mix.

The per-channel function of every Photoshop mode is the tree's CPU twin, and
the tables the tree builds at start-up
(`reference/cpp/core/visual/gl/blend_function.cpp:526-547`) are the exact
formulas:

| mode | per-channel function `f(s, d)` | the reference |
|---|---|---|
| `omPsNormal` | `s` | `reference/cpp/core/visual/gl/blend_functor_c.h:374-383` |
| `omPsAdditive` | the saturating sum | `reference/cpp/core/visual/gl/blend_functor_c.h:386-397` |
| `omPsSubtractive` | the saturating difference | `reference/cpp/core/visual/gl/blend_functor_c.h:401-412` |
| `omPsMultiplicative` | `d * s >> 8` | `reference/cpp/core/visual/gl/blend_functor_c.h:416-426` |
| `omPsScreen` | `d + s - (d * s >> 8)` | `reference/cpp/core/visual/gl/blend_functor_c.h:430-445` |
| `omPsOverlay` | `(s * d * 2) / 255` below the midpoint, `(s + d) * 2 - (s * d * 2) / 255 - 255` above it | `reference/cpp/core/visual/gl/blend_functor_c.h:512-533`, the table at `reference/cpp/core/visual/gl/blend_function.cpp:539-542` |
| `omPsHardLight` | the overlay with the operands swapped | `reference/cpp/core/visual/gl/blend_functor_c.h:537-564` |
| `omPsSoftLight` | `pow(d / 255, 128 / s)` above the midpoint, `pow(d / 255, (1 - s / 255) / 0.5)` below it | `reference/cpp/core/visual/gl/blend_functor_c.h:486-488`, the table at `reference/cpp/core/visual/gl/blend_function.cpp:532-534` |
| `omPsColorDodge` | `0xff` when the source is light enough, `(d * 255) / (255 - s)` otherwise | `reference/cpp/core/visual/gl/blend_functor_c.h:494-496`, the table at `reference/cpp/core/visual/gl/blend_function.cpp:535-536` |
| `omPsColorDodge5` | the colour dodge of the faded source; the channel is replaced, not mixed | `reference/cpp/core/visual/gl/blend_functor_c.h:568-581` |
| `omPsColorBurn` | `0x00` when the source is dark enough, `255 - ((255 - d) * 255) / s` otherwise | `reference/cpp/core/visual/gl/blend_functor_c.h:502-504`, the table at `reference/cpp/core/visual/gl/blend_function.cpp:537-538` |
| `omPsLighten` | the lighter channel | `reference/cpp/core/visual/gl/blend_functor_c.h:585-596` |
| `omPsDarken` | the darker channel | `reference/cpp/core/visual/gl/blend_functor_c.h:600-611` |
| `omPsDifference` | the absolute difference | `reference/cpp/core/visual/gl/blend_functor_c.h:615-626` |
| `omPsDifference5` | the absolute difference of the faded source; the channel is replaced, not mixed | `reference/cpp/core/visual/gl/blend_functor_c.h:630-642` |
| `omPsExclusion` | `d + s - (d * s >> 7)` | `reference/cpp/core/visual/gl/blend_functor_c.h:646-660` |

The mix factor is the source's alpha, scaled by the opacity when the opacity
is not the full one
(`reference/cpp/core/visual/tvpps.inc:16-24`). The integer twins truncate
the product by eight bits, so a channel can land one unit below the value
the floating-point shader computes.
* **Every write is clipped** to the destination bitmap's bounds and to the
  supplied clip rectangle. A write outside the clip is dropped, not clamped.
* **A write marks the bitmap dirty**, so the engine re-uploads it on the next
  frame. `mark_dirty_rect` narrows the upload to the written rectangle for the
  affine, `operate_rect`, `stretch_blit_mode` and the copy and blend passes
  (`copyRect`, `Blt` and `stretchCopy`); the families that rewrite the whole
  bitmap (the fills, the blurs, the colour operations) mark it wholly. Both
  marks bump the bitmap's `content_generation`, which the subtree-cache
  bookkeeping sums to detect an edit that did not go through a layer
  accessor.
* **An integer one-to-one affine replace is a copy.** When the corner map is
  axis-aligned at unit scale with an integer origin, the supported resamplers
  are exact at integer source centres, so the pass routes to the clipped copy
  instead of the per-pixel inverse map and float kernel. A caller using the
  pixel-centre convention marks the pass with
  `affine_blit_clipped_pixel_center`, which resolves a half-pixel origin to
  the same boundary; a floating-point corner within 1e-6 of that boundary is
  treated as exact. A fractional boundary and the blur/area kernels stay on
  the resampler. A clipped copy whose source rows are all in bounds copies
  those rows as rows; the per-pixel path remains for a partially out-of-bounds
  source.
* **Large copy/blend/stretch bands run in parallel.** A clipped copy,
  source-over or stretch pass splits its destination rows into bands on the
  same threshold the nearest affine pass uses. A band owns whole rows, so its
  pixel loop walks the destination row directly; the bytes are identical to
  the single-band path.
* **A one-to-one stretch is a blit.** When the source and destination rects
  have the same extent and the resampler is one of the exact-at-integer kinds,
  the stretch maps each destination pixel to the matching source texel. It
  routes to the clipped copy for Copy mode and to the source-over blit for
  SourceOver mode instead of the float resampler. The copy shortcut does not
  serve a pass that reads the source as an opaque image: it writes the
  source's own alpha where that pass writes an opaque one.
* The `simd-experiment` feature adds an exact source-over row fast path for
  `operateRect`: a 16-pixel block whose source alpha is all 255 copies, a
  block whose alpha is all 0 leaves the destination alone, and a row with
  partial alpha is split into exact runs and only its partial pixels take the
  scalar blend. The pass only applies at `opa == 255`, without `hold_alpha`,
  and when the source rectangle is fully in bounds. It is enabled by default
  in the app build; the scalar loop is the A/B baseline.
* **The source rectangle is half-open**, and a source region that leaves the
  source bitmap contributes nothing rather than a repeated edge pixel.
* **`blit_plane` with both plane bits is a straight copy**, not a blend: the
  source's transparent pixels overwrite the destination. Games rely on this
  to make old pixels vanish when they redraw over existing content.
* **`colorize` and `noise` operate on the layer's current `ClipRect`** — the
  effect rebases its buffer on the clip — so the callers pass the
  already-clipped region.
* **The transition slots are layer-owned and distinct**: the whole-subtree
  frame is drawn in place of the subtree, the self image in place of the
  layer's own image while the children keep drawing live. The self image is
  never part of the subtree cache, so releasing the cache leaves it alone.
  The display slot is not the script cache or the main image either: a slot
  that holds either is replaced by a fresh display bitmap, so a transition
  blend never overwrites pixels the script or the frame source owns.
* **The subtree root never sources its own cache**, and a descendant whose own
  cache is live is composited through that cache bitmap.

---

## 5. Data flow

```
L3 native (script call)
   │  marshalled to a bitmap id + a region
   ▼
layer_ops::fill_color_on_alpha(&mut scene.bitmap_mut(id)?, rect, color, opa)
   │
   ├─► writes straight-alpha RGBA bytes
   ├─► bitmap.mark_dirty()      → content_revision + content_generation
   └─► engine::Engine::sync_dirty_bitmaps → upload_textures → the GPU texture
```

The subtree path:

```
render_layer_subtree(scene, root)
   ├─► walks the subtree, reusing a live descendant cache where it can
   ├─► composites the drawn images into a new bitmap
   └─► returns the bitmap id (or the MainImage for a trivial subtree)
              │
              ▼
       scene.set_cache_bitmap(root, id) → the projection draws that bitmap
```

---

## 6. Failure modes

* **The primitives are infallible.** They operate on already-validated VM
  bounds; a region that is empty, out of bounds or fully clipped is a
  no-op, not an error. Validation that *can* fail happens in the operation
  that receives the script input.
* **The allocating operations return `Option`.** `render_layer_subtree`,
  `ensure_transition_display` and `ensure_transition_self_image` return `None`
  for an unknown layer or a bitmap that cannot be allocated; the caller turns
  that into the script-visible error. The absence is distinguishable from a
  real failure.
* **`copy_9patch` returns `None`** when the source is smaller than 11×11 or
  the destination is smaller than the source minus the margins.
* **Every operation mode has a kernel of its own.** The mode's kernel
  follows the reference's blend method
  (`reference/cpp/core/visual/LayerIntf.cpp:4118-4271`): the alpha blend
  that takes the destination's alpha into the mix, the channel mix that does
  not (the Photoshop modes and the opaque face's alpha blend), and the
  per-channel Photoshop functions. An operation mode the reference does not
  know is rejected by the compatibility layer before the pass runs.
* **`do_blur_light` uses the hard-light blend only for `ltPsHardLight`
  (= 19)**; every other light type composites source-over.

---

## 7. Tests

* `crates/engine/tests/layer_ops.rs` — the fills, the blits, the clip
  behaviour, the colour conversions, the blur and the transition slots.
* `crates/engine/src/layer_ops.rs` `mod tests` — the unit-level invariants.

---

## 8. See also

| topic | document |
|---|---|
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the layer state these write into | [scene.md](scene.md) |
| the shape rasterizer that shares the blend primitive | [raster.md](raster.md) |
| the transitions that consume the transition slots | [trans.md](trans.md) |
| the video overlay that blits a frame into a layer | [video.md](video.md) |
| the archived protocol trail | [`docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) §6.10 |
