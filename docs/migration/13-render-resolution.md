# 13 — CPU raster at the render resolution, GPU for composition

Status: **design; the tree still rasterizes at VM resolution.** This document
supersedes the earlier “GPU-resident frame” proposal in this file. It keeps the
CPU rasterizer (the reference-compatible, plugin-compatible path) and gives it
a **render resolution** that is independent of both the VM size and the window
size. GPU work is limited to composition and presentation. The reference stays
the behavioural oracle; all script-visible values are VM units (D34).

---

## 1. Three spaces

| space | unit | who uses it | examples |
|---|---|---|---|
| **VM** | the game's declared pixel | scripts, L3, layer state | `System.screenWidth`, layer rects, `Bitmap.width/height`, `Font.size`, `getPixel` |
| **render** | the fixed internal raster resolution | the L2 CPU rasterizer and the layer surfaces | layer pixel storage, glyph raster size, readback region |
| **display** | the framebuffer/window | L1 presentation | swapchain extent, Fit letterbox |

`render_scale = render_size / vm_size`. The translation VM→render is embedded
in L2; the translation render→display is the presentation Fit. At
`render_size == vm_size` the first is the identity, which is the current tree
and the exact reference path.

The window/display can resize at any time; **render_size is fixed for the
session** by the launch argument, so resizing never re-rasterizes existing
content. Only the presentation scale changes.

## 2. Where the translation lives

`PixelTranslation` is an L2 implementation detail:

* every L2 render/pixel/readback method takes VM coordinates and applies the
  current VM→render mapping internally;
* L3 natives keep their VM-coordinate contract and never call `to_device_*`,
  `to_vm_point`, `to_render_*`, `raster_scale`, or name a render/device pixel
  (D41);
* the composition root passes platform input through an L2 mapping method and
  gives L3 only VM events;
* readback returns VM values; the render-region sampling happens inside L2.

## 3. Render resolution is an explicit argument

`render_size` (or an equivalent `render_scale`) is a **launch/engine argument**,
not derived from the window or display:

* `EngineParts.render_size` / `HostConfig.render_size` carry it;
* the desktop launcher exposes it (`--render-size WxH`, or a documented
  default), and the Android launcher can pass its own choice;
* default is the game's VM size (render_scale 1, the current reference path);
  a higher value (for example the display size or 2× VM) is the opt-in quality
  path;
* it is fixed for the session. A window resize changes only the presentation.

## 4. CPU rasterization at the render resolution

A layer's pixel storage is one of:

| layer kind | storage | display |
|---|---|---|
| **image-only** | the decoded native texture; no CPU layer copy is kept after upload | textured quad sampled at display resolution |
| **rasterized** (any `drawText`, fill, blit, copy, affine, mask, transition, cache, or raw-buffer access) | one CPU surface at **render resolution**, owned by the layer | uploaded as a texture and composited by the GPU |
| **readback of a direct/native layer** | the native texture or a transient render-res surface | readback only; no persistent second copy |

There is no persistent VM-resolution layer bitmap:

* a rasterized layer stores `vm_width · render_scale × vm_height · render_scale`
  pixels; VM dimensions remain the script-visible `width`/`height`;
* the first raster operation on an image-only layer materializes its
  render-res surface from the native image (scaled once), then subsequent CPU
  operations translate into it;
* every CPU operation translates its coordinates, sizes, font size and shadow
  radius by `render_scale`;
* `getPixel` maps the VM coordinate to the render region with
  `vm_pixel_region` and reduces it to one value.

The memory cost is the render-res surface for layers that actually receive
CPU raster operations. Image-only layers cost no extra copy; the `s²` term
applies only where a raster surface exists, and that surface is the layer's
only storage, not a duplicate.

### 4.1 Text

The font engine is L2 state (faces, coverage cache, layout). Text is laid out
**in VM units** (advances, extents, `Font.getTextWidth`/`getEscWidth`), and the
coverage is rasterized at `Font.size · render_scale` into the layer's render-res
surface. Shadow width/offset, angle and clip are scaled with the same factor.
Prerendered `.tft` faces are fixed bitmaps: they are sampled at the render
resolution (a documented D34 rasterization choice), because the source has no
higher-resolution outline.

No GPU text path is introduced in this design. Text is CPU-rasterized like the
rest of the layer content; the GPU only composites the resulting texture.

### 4.2 Images

Decoded images stay at their native resolution. An image-only layer is drawn
as a textured quad, so the GPU samples the source directly at display
resolution. When a raster operation needs a render-res layer surface, the image
is scaled into that surface once (CPU scale) and the surface becomes the
layer's content. The source texture is not copied into a VM-res layer.

### 4.3 GPU role

The GPU does **only**:

* compose layer textures (opacity, blend, transform, mask, clip) at display
  resolution;
* sample native image textures and the render-res layer textures;
* present the Fit-scaled frame to the window.

There is no GPU glyph pipeline, no GPU blit/copy/affine, and no GPU-side pixel
mutation. This keeps the reference formulas and the plugin/raw-buffer
compatibility in the CPU path, while the expensive scaling/composition stays on
the GPU.

## 5. Readback

A script read is a **scaled access**, not a second buffer:

* `getPixel(x, y)` maps to `vm_pixel_region(x, y)` in the render-res surface and
  reduces the region to one value (point-read at the origin by default;
  averaging is a documented alternative if a reference edge case demands it);
* `piledCopy`, province reads and `saveLayerImage` read the corresponding
  render region;
* at `render_scale == 1` this is exactly the reference pixel;
* the transport copies only the requested region to CPU (host-visible staging
  on desktop, `AHardwareBuffer_lock` on Android where applicable). The copy is
  transient; no persistent VM-res mirror is created.

## 6. Plugins and raw buffers

`Bitmap.buffer`/`bufferForWrite`, `Layer.mainImageBuffer*`,
`Layer.provinceImageBuffer*` and plugins such as `LayerExDraw`/`ImageFunction`
expect CPU-addressable pixels with `width`/`height`/pitch semantics. A
render-res surface breaks those assumptions unless we adapt the bridge.

The compatibility rule:

* a bitmap whose raw buffer is exposed (or whose plugin path writes raw pixels)
  keeps a **VM-resolution CPU staging view** for the duration of that access;
  reads copy the render region into the VM view, writes are applied back
  (nearest/inverse-mapped) or, if the plugin's result is the authoritative
  content, the layer falls back to VM-res storage for that bitmap;
* this is the single documented exception to “no second copy”, and it is
  transient/per-bitmap, not a per-frame mirror;
* the native plugin surface must remain byte-compatible; a plugin that writes
  through the raw pointer must never observe render-res dimensions/pitch.

The exact policy (per-bitmap fallback vs. transient staging) is the open
compatibility decision in §9.1.

## 7. What changes in the crates

* **engine (L2)**: `PixelTranslation` gains the VM→render and render→display
  legs; `BitmapState` stores render-res pixels plus VM dimensions and a
  `render_scale`; layer ops, raster and text translate internally; readback
  samples the region; the font engine/coverage cache moves from the L3 resource
  resolver to L2.
* **render (L1)**: no GPU pixel ops; the existing node renderer samples the
  uploaded render-res or native textures and composites/presents. A texture
  upload size check and the presentation scale already exist.
* **compat (L3)**: natives keep their VM-coordinate contract; the text/bitmap
  bodies call the new L2 methods instead of mutating a CPU bitmap directly.
  No L3 code names render/device pixels or `PixelTranslation` (D41).
* **app**: passes `render_size` from the launcher/config; maps platform input
  through L2; no per-frame render-res computation.

## 8. Migration

| stage | change | output |
|---|---|---|
| 0 | add `render_size`/`render_scale` plumbing and the VM→render translation; default = VM size; no behavior change | identical to today |
| 1 | scale-aware raster for **text and simple fills** on layer surfaces at render res; readback region; font engine in L2 | crisp text at `render_scale > 1` |
| 2 | move the remaining raster ops (blit/copy/affine/mask/transition) to the render-res surface; image-only layers stay native textures | native-quality layers |
| 3 | raw-buffer/plugin compatibility carve-out; remove the VM-res duplicate storage paths | plugins still exact |
| 4 | delete the VM-res raster path; the CPU oracle remains for `render_scale == 1` and tests | target |

Stage 0 is output-neutral. Stage 1 is the visible win and does not require the
whole raster migration.

## 9. Open decisions

1. **Raw-buffer policy.** Per-bitmap VM-res fallback (recommended: a bitmap
   exposed through a raw pointer keeps VM-res storage and is display-scaled)
   versus a transient VM-res staging view synced on access. The former has no
   copies but gives that bitmap the old blurry path; the latter keeps native
   render-res storage but copies on raw access.
2. **Default render resolution.** VM size (current behavior, opt-in quality via
   `--render-size`) versus display size (native by default, higher CPU cost) —
   the argument is fixed per session either way.
3. **Where the argument lives.** Desktop CLI + `RuntimeConfig` + `HostConfig`;
   Android launcher setting later. Confirm the naming (`--render-size WxH`).
4. **Readback reduction.** Point-read the region origin (recommended, exact at
   scale 1) or average the box.
5. **Image-only layer storage.** Confirm that native textures stay the only
   storage for image layers (no CPU mirror after upload); a raster op
   materializes the render-res surface once.
6. **Prerendered fonts.** Scaled bitmap sampling is a D34 rasterization choice;
   confirm no outline fallback is expected.

## 10. Invariants

1. L3 never names render/device pixels or the translation; the mapping is L2
   internal (D41).
2. No persistent VM-resolution layer bitmap: a rasterized layer has one
   render-res surface; an image-only layer has its native texture.
3. All script-visible values and layout metrics are VM and reference-exact
   (D34); scaling is invisible.
4. At `render_scale == 1` output and readback match the reference/CPU oracle.
5. The GPU does no pixel mutation and no text; it composites and presents.
6. Native plugins and raw buffer access remain compatible; the exception is
   documented and per-bitmap, never a silent dimension change.
