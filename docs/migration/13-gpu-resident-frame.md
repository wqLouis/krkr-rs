# 13 — The GPU-resident frame

Status: **design; the tree is still on the transitional CPU-composite path.**
This document is the contract for the next architecture slice: the L2 frame
owns the layer state and translates every draw/readback operation internally,
while L1 executes the operations on the GPU. It refines
[`11`](11-presentation-and-pixel-translation.md) §6 and
[`12`](12-layer-tree-and-frame.md) §4–§7. The reference stays the behavioural
oracle (`reference/cpp/**`); where this document chooses a GPU path, the
choice is a rasterization/implementation choice under D34, not a change to a
script-observable value.

---

## 1. The rule

**L2 owns the frame, and the frame is not a CPU bitmap.** A layer is a set of
VM-space state plus an ordered set of draw operations. L2 applies the
VM↔device translation **inside its own draw and readback methods**, then asks
L1 to execute the operation at device resolution. L3 keeps calling L2 in VM
coordinates and never learns that a device space exists.

```
L3 native:  layer.drawText(x, y, text)        // VM coordinates only
                 │
L2 frame:   translate(x·s, y·s, size·s)       // PixelTranslation is private
                 │
L1 render:  draw glyph quads / textured quads / fills into the GPU frame
```

There is **no persistent VM-resolution copy of a layer** and no VM-res
intermediate texture that is later scaled. The GPU frame (or a GPU render
target for a layer that needs one) is the layer's storage; `getPixel` and the
other readback members sample that storage over
`PixelTranslation::vm_pixel_region`.

## 2. Where the translation lives

`PixelTranslation` is an **L2 implementation detail**, not an L2↔L3 value:

* every L2 render/pixel method takes its arguments in VM coordinates and
  applies the current translation internally;
* L3 native bodies keep their VM-coordinate contract — they do not call
  `to_device_*`, `to_vm_point`, `raster_scale`, or name `PixelTranslation`;
* the composition root passes L1 platform input to an L2 input-mapping method
  and gives L3 only VM events;
* readback members return VM values; the device-region sampling is inside L2.

This is the “embed the translation in L2” choice: L2 is small, L3 is the large
native surface, and the translation then has exactly one home. The layer rules
are unchanged (`L3 → L2 → L1`); this adds no edge.

## 3. Layer storage and the no-copy invariant

A layer is in one of three display modes:

| mode | when | storage |
|---|---|---|
| **direct** | one op, or ops whose composition is order-independent (opaque fills/images) | none; L2 translates and draws into the frame in call order |
| **GPU target** | the layer's semantics need an offscreen: opacity < 1, a layer transform, a mask/province, a cache bitmap, a transition, or a script readback | one device-resolution render target owned by the frame |
| **transient** | a script reads a direct layer, or a one-off composite is needed | a GPU target materialized for the read/operation and released afterwards |

A GPU target is the layer's **only** pixel storage. It is not a second copy of
a VM-resolution buffer: the VM dimensions remain the script-visible size, the
storage is `vm_size · s`, and the composition samples it 1:1. The `s²` memory
cost therefore applies only to layers that actually need an offscreen — a
directly drawn layer allocates nothing per layer, and a readback target is
transient.

No persistent CPU RGBA mirror exists for display. CPU bytes are transient at
the boundaries that require them:

* a decoded image arrives from the decoder as bytes and is uploaded once; the
  upload buffer is released unless a script operation still needs it;
* a readback copies only the requested device region to CPU (or maps it over
  `AHardwareBuffer` on Android), never the whole frame;
* the null/CPU render backend keeps its pixels in L1 render state as the test
  oracle, not as an L2 layer mirror.

## 4. Draw operations and their execution

The L3 native surface does not change. The existing calls (`Layer.drawText`,
`Layer.loadImages`, `Layer.fillRect`, `Layer.copyRect`, `Layer.blt`,
`Layer.affineCopy`, `Layer.setMaskBitmap`, transitions, `Bitmap.*` pixel
members, `getPixel`, `saveLayerImage`, …) keep taking VM coordinates, colours
and dimensions. L2 routes each one to the translation + L1 mechanism:

| VM operation | current L2/L3 implementation | target |
|---|---|---|
| `drawText` / `drawGlyph` | glyphs rasterized at `Font.size` into a VM-res bitmap | L2 font engine rasterizes at `Font.size · s` into a device-size glyph atlas; glyph quads draw into the frame/target |
| `loadImages` / image draw | decoded CPU image → VM-res bitmap → upload | decoded image uploaded once as a native texture; textured quad drawn at device res |
| `fillRect` / solid brush | CPU fill into the VM bitmap | solid quad drawn at device res (or into the layer target) |
| `copyRect` / `blt` | CPU pixel copy | GPU copy/blit between targets |
| `affineCopy` / rotate/zoom | CPU affine blit | textured quad with the affine transform |
| `setMaskBitmap` / province / `holdAlpha` | CPU mask composite | mask texture/sampler in the GPU pass |
| transitions (`Trans`) | CPU composite into the layer | GPU target/pass with the rule mask |
| cache bitmap / `InternalComplete2` | CPU cache bitmap | GPU target containing the subtree |
| `getPixel` / `piledCopy` / `saveLayerImage` | CPU bitmap read | region read from the GPU storage, sampled over `vm_pixel_region` |

L2 keeps the semantic work: which operation, in which order, with which blend,
clip and state. L1 keeps the rendering mechanism: pipelines, targets, glyph
atlas, sampler, copy/blit passes and readback. This is D10 applied to pixel
operations, not only to the projected node tree.

### 4.1 Ordering

Operations execute in call order into the current destination. For a layer
that needs a target, its operations fill the target in call order and the
layer is then composited once (opacity, transform, mask). For a direct layer,
the operations draw into the frame in the same order. A destination-dependent
blend (`DstBlend` family) is evaluated by the GPU pass or by the CPU oracle;
it is never substituted with source-over (D11/D34 boundary).

### 4.2 Fonts

The font state moves to L2 (it is engine state; L3 only marshals the face,
size, style and text). Layout metrics (`Font.getTextWidth`, `getEscWidth`,
advances, extents) stay **VM** and are returned unscaled (D34). Only the
rasterized coverage and glyph placement are device-space. The glyph atlas is
keyed by the device raster size (`size · s`), so a scale change invalidates it;
a colour or opacity change does not rewrite coverage.

### 4.3 Textures and filtering

A source image is decoded once at its native resolution and sampled at device
resolution. At `s == 1` the reference’s nearest-texel sampling is matched
exactly (`reference/cpp/core/visual/RenderManager.cpp:2902`). At `s > 1` the
magnification filter is a rasterization choice: nearest keeps the reference’s
blocky look, linear/mipmap is smoother while script-observable values are
unchanged. The policy is recorded and tested as a D34 approximation; it is not
a silent fallback.

## 5. Readback

A script read is a **scaled access**, not a second buffer:

* `getPixel(x, y)` maps the VM pixel to `vm_pixel_region(x, y)` and reduces the
  device region to one value (point at the origin or average, decided and
  documented once);
* `piledCopy`, province reads and `saveLayerImage` read the corresponding
  device region of the layer storage;
* at `s == 1` the region is exactly the reference pixel;
* the transport is region-scoped and transient: a host-visible staging copy on
  desktop, `AHardwareBuffer_lock` on Android when the backing is an
  `AHardwareBuffer` ([`10`](10-android-backends.md)). A read may stall the GPU;
  that is the cost of the access, not a persistent CPU copy.

## 6. What L1 must provide

The render crate grows the mechanisms this contract needs; L2 keeps calling
them through their stateless action functions:

1. **A GPU-resident frame/target**: create/destroy a device-resolution target,
   render into it, and sample it as a texture when compositing the layer.
2. **Glyph atlas**: upload coverage at a device raster size and draw glyph
   quads with a tint/opacity; atlas eviction and re-upload.
3. **Target-to-target copy/blit and an affine textured quad** for the copy and
   transform operations.
4. **Mask sampling** (alpha texture) and the destination-dependent blends the
   reference defines.
5. **Region readback** from a target/frame (`read_render_target`-shaped), plus
   the Android `AHardwareBuffer` transport.
6. **A CPU oracle** for the null backend and tests: the same op stream
   rasterized in software at VM resolution for reference comparisons.

## 7. Migration

Each stage leaves the game runnable and the gate green. The CPU path remains
as the oracle while the GPU path is introduced.

| stage | change | visible result |
|---|---|---|
| 0 | document the contract; make `PixelTranslation` private to L2; add the op-list/state scaffolding without changing output | none |
| 1 | **text**: L2 font engine owns the atlas and rasterizes at `size·s`; text draws as glyph quads; the VM-res text bake is no longer the display source | crisp text at `s > 1` |
| 2 | **images and fills**: native texture upload + textured/solid quads via L2; no VM-res copy for those ops | native texture sampling, no per-image CPU mirror |
| 3 | **layer targets**: opacity/transform/mask/cache/transitions use one GPU target per layer; direct layers stay direct | correct layer composition at device res |
| 4 | **pixel-copy ops**: blit/copyRect/affineCopy/province/masks on the GPU; readback region path | no CPU compositing for display |
| 5 | delete the CPU display path (keep the null oracle); guard no persistent VM-res layer mirror | the target architecture |

Stage 0 is deliberately output-neutral so the surface lock, parity reports and
golden tests stay unchanged. Stage 1 is the text-only win and does not require
the full target/layer migration.

## 8. Invariants and tests

1. **L3 never names device pixels or the translation.** `PixelTranslation`,
   `to_device_*`, `to_vm_point`, `raster_scale` and `vm_pixel_region` are L2
   internals (a guard should fail on an L3 use).
2. **No persistent VM-res layer mirror.** A layer's display storage is the
   frame or a device-res target; CPU bytes are transient decode/readback.
3. **VM values stay reference-exact.** Script getters, return values, errors
   and metrics are computed in VM units; device scaling is invisible (D34).
4. **At `s == 1` the output and readback match the reference.** The CPU oracle
   is the evidence; golden tests compare against it.
5. **A GPU target is chosen only for a semantic need.** Direct layers allocate
   no per-layer target; a read may materialize a transient one.
6. **Blend fallbacks are typed errors, not substitutions.** A backend that
   cannot perform a destination-dependent blend reports `Unsupported`; it does
   not silently draw source-over (D11).

## 9. Open questions

1. **Text layout ownership.** The `TextEngine` instance currently lives in the
   L3 resource resolver even though the type is L2. Moving font/atlas state to
   L2 is part of stage 1; confirm the handle L3 passes (face name + style +
   size) and where the atlas lives.
2. **Readback reduction.** Point-read the region origin or average the box?
   The origin is exact at `s == 1` and cheapest; averaging needs a defined
   rounding. Decide against the reference `getPixel` edge case.
3. **Direct-vs-target policy.** Which layer properties force a target
   (`opacity < 1`, transform, mask, cache, readback)? The conservative default
   is a target for any layer with more than one op or a non-identity
   compositing state; measure before optimizing.
4. **Magnification filter** at `s > 1`: nearest (reference look) or linear
   (smooth) — D34 permits either, but it must be one documented policy.
5. **Android readback transport**: `CPU_READ_OFTEN`, tiling, and the `lock`
   stall (see [`10`](10-android-backends.md)).
6. **Memory bound for GPU targets.** With one target per offscreen layer, a
   screen-sized target is `vm_w·vm_h·s²·4` bytes. The policy is “no target
   unless needed”; a budget needs measurement, not an arbitrary cap (D11).
