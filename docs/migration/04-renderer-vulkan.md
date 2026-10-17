# 04 — Render pipeline: Vulkan (`ash`)

Status: implemented. The `render` crate (L1) ships the `ash` Vulkan backend
plus a fully functional headless `NullRenderPipeline`; the middle layer keeps
the layer-system state and passes a `RenderScene` tree plus a `RenderState`
value, and the pipeline consumes the tree and owns all traversal,
composition, batching and drawing. See
[`03-middle-layer-api.md`](03-middle-layer-api.md#2-render-pipeline).

---

## 1. Why `ash`, why Vulkan-only

The GPU workload is small and fixed:

* textured quads and solid fills;
* 7 fixed-function blend states (SourceOver, Replace, Additive, Subtractive,
  AddAlpha, Multiply, Screen);
* dirty-region texture uploads;
* no 3D, no compute, no skeletal animation, no MSAA requirement.

wgpu's value is backend portability, which we do not need (D4). `ash` gives
direct pipeline/blend/descriptor control and no extra abstraction. It is a
direct dependency (`ash` 0.38, `ash-window` 0.13) and pulls in no graphics
framework.

| Concern | Crate | Note |
|---------|-------|------|
| Vulkan bindings | `ash` | direct dependency |
| Surface creation | `ash-window` / `raw-window-handle` | winit provides the raw handles |
| Windowing | `winit` (direct, Linux) | no windowing framework |
| Android | `ndk` (window) + composition root | the `android-activity` GameActivity glue lives in `crates/android` |

## 2. Geometry of the renderer

```
L1 platform loop (winit / Android composition root)
   │  frame boundary
   ▼
render::RenderPipeline   (`&self`, stateless)
   ├─ Device       — instance, physical/logical device, queues
   ├─ Pipeline set — shader programs + blend/attachment state
   ├─ Traversal    — walks the L2-owned RenderScene tree
   ├─ Composition  — parent transforms, opacity, z, clip, blend selection
   ├─ Uploads      — dirty textures into RenderState
   └─ Frame        — acquire → record → submit → present

L2 stores the mutable side: RenderScene tree · RenderState (swapchain,
staging, compiled pipelines, texture table, atlas uploads, targets)
```

The render pipeline is single-threaded on the host thread.

**Object/state split (D9).** The only L1 object is the stateless
`RenderPipeline`; the device lives inside it as immutable mechanism. Every
mutable render thing is in the L2-owned `RenderState` (swapchain, staging,
compiled pipelines, texture table, offscreen targets, atlas uploads) and the
L2-owned `RenderScene` tree. `render(&self, surface, &mut state, &scene)`
mutates only the state the caller passed. L1 has no globals, no
`HashMap<Id, Resource>` and no lazy caches. The same tree + state produces
the same frame.

## 3. Pipeline set

Two base programs are required:

1. **Textured quad** — standard unit quad, `textureSample` × vertex color.
   This covers layers, video frames, and future GPU glyphs.
2. **Solid quad** — same vertex path, fragment outputs the vertex color.
   This covers fills, and the screen clear can use a scissor+clear or a
   fullscreen fill.

Blend state is not a shader concern; it is a per-pipeline
`VkPipelineColorBlendAttachmentState`. `blend_slot` maps a
[`BlendMode`](../../crates/platform/api/src/blend.rs) to one of 7 fixed slots
(`crates/platform/render/src/vulkan/pipelines.rs`):

| TVP / `LayerBlendMode` | Vulkan `srcColor` / `dstColor` / `op` |
|------------------------|----------------------------------------|
| SourceOver / Alpha | `SRC_ALPHA` / `ONE_MINUS_SRC_ALPHA` / `ADD` |
| Replace / Opaque | `ONE` / `ZERO` / `ADD` |
| Additive | `SRC_ALPHA` / `ONE` / `ADD` |
| Subtractive | `SRC_ALPHA` / `ONE` / `REVERSE_SUBTRACT` |
| AddAlpha | additive color, source alpha replaces destination alpha |
| Multiply | `DST_COLOR` / `ZERO` / `ADD` |
| Screen | `ONE_MINUS_DST_COLOR` / `ONE` / `ADD` |

`ltBinder` is not submitted at all (L2 drops it while flattening).

The pipeline set is built up front, so a frame never builds pipeline state.

## 4. Destination-dependent blends

The 11 destination-dependent modes (`DstBlend`: ColorDodge/5, ColorBurn,
Darken, Lighten, Difference/5, Exclusion, Overlay, HardLight, SoftLight) need
to read the destination color, which fixed-function blend state cannot
express. They are **CPU-only today**: the null backend's software rasterizer
implements the Photoshop/KiriKiri formulas
(`reference/cpp/core/visual/gl/blend_functor_c.h`), and the Vulkan backend
reports `RenderError::Unsupported` for them rather than substituting
source-over. A future Vulkan path renders to an offscreen target, copies the
target content for the quad's `dst` region into a sampled image, and draws
with a shader variant that samples both source and destination; the shader
variant selection is by `DstBlend`. That is also the mechanism for future GPU
transitions.

## 5. Texture upload and caching

* L2 owns `BitmapState` (CPU RGBA) and its dirty flags.
* `upload_texture` writes the dirty region into a
  `VK_IMAGE_USAGE_TRANSFER_DST` image via a staging buffer; `upload_textures`
  batches every upload's sub-rectangle in one staging pass.
* Sub-rect uploads (`PixelRegion`) are supported: `drawText` and raster ops
  only touch part of a bitmap.
* SRGB: images are `Rgba8Srgb` and colors are converted to linear in the
  shader. The convention is one format + linear shader math, applied
  consistently across the null and Vulkan backends; this is called out as a
  correctness risk in `07-roadmap.md §6`.
* Pruning: when L2 drops a bitmap it calls `drop_texture`; bitmap→texture
  lifetime is explicit L2 state.

## 6. Presentation, resize, DPI

* The frame acquires a swapchain image, records the scene, submits and
  presents.
* `winit` reports `Resized`/`ScaleFactorChanged`; the platform layer forwards
  the new surface size, and the render pipeline recreates the swapchain
  (including `OUT_OF_DATE`/`SUBOPTIMAL`/minimized handling).
* The letterboxed game projection is L2's job: the middle layer computes the
  device-space `dst` rects from the logical window size and hands the pipeline
  a device-space `RenderScene`. The pipeline has no presentation policy of its
  own.
* `Window.setSize`/`setZoom`/`changeScreenMode`: L2 writes the declared
  size; the host applies it through the platform window.

### 6.1 Presentation scaling

**Superseded and removed — see
[`11-presentation-and-pixel-translation.md`](11-presentation-and-pixel-translation.md).**

The `ScalingMode` enum (`Game`/`Fit`/`Fill`/`Expand`), its configuration key
and the whole-tree `ViewTransform` it drove have been deleted. Presentation is
now a single rule — Fit with letterbox (D39) — and the VM-pixel/device-pixel
split it needs lives in an L2 pixel-translation layer, not in the pipeline
(D40). The renderer receives device-space rectangles and has no notion of a
"presentation mode"; the cocos2d `SHOW_ALL`/`EXACT_FIT`/frame-aspect
comparison below is kept only as the historical reference for the one rule
that survives.

## 7. Window, input and UI

### 7.1 Windowing

* Desktop: `winit` `EventLoop` + `ApplicationHandler` in `platform::linux`.
* Android: the `crates/android` composition root owns the `android-activity`
  `GameActivity` loop and hands the `ANativeWindow` to `platform`; the
  renderer creates/recreates its Vulkan surface from the projected
  `RawWindowHandle::AndroidNdk`. The event loop in `platform`'s Android
  backend is still a typed stub (`PlatformError::NotImplemented`), so the
  desktop build is the runtime path.

### 7.2 Input

* The platform crate feeds a generic `InputState`; the pure parts
  (`window_to_game_transform`, `hit_test`/`layer_at`, touch gestures, the
  button-delay/transition state machine) live above it and are host-tested.
* Android touch/back-button handling is translated by the pure
  `*_from_android` functions and wired from the composition root's callbacks.

### 7.3 Menu UI

There is no native OS menu. `MenuItem` is a logical menu tree
(`MenuState`) built on L2 primitives; `MenuItem.popup` returns the reference
success value and `HMENU` is `0`. Drawing the reference's own engine-drawn
popup form is open work (`TODO.md` §5); a native popup is a deliberate
divergence in appearance, not a port.

## 8. What was removed

The Bevy renderer, its wgpu materials and WGSL shaders, `render/src/sync.rs`
(`sync_scene`, `BitmapAssets`, `GpuPrimitives`), `render/src/runner.rs`,
`render/src/input_bridge.rs`, `render/src/menu.rs`, and the `#[bevy_main]`
Android entry point are gone; their functionality is the L2 projection +
L1 `RenderPipeline` + the platform loop described above. Bevy is not in
`Cargo.lock`.

The pure helpers (`compose_layer`, `visible_image_region`,
`window_to_game_transform`, `window_layer_order`, `sprite_z`, `bitmap_tint`,
`fill_sprite_color`) live in `engine::projection`/`raster` and stay
unit-tested without a GPU.

## 9. Glyph rendering

L1 exposes a `FontSystem` object, immutable `Font` values and a stateless
`rasterize_glyph` that produces CPU coverage (`ab_glyph`/`ttf-parser`). L2
keeps the atlas, layout and CPU composition, so
`getPixel`/`piledCopy`/masks/province continue to work. A GPU glyph-atlas
path (`RenderContent::Glyphs`) is not implemented: both backends report
`RenderError::Unsupported`, `supports_glyph_atlas` is `false`, and text is
composited into the layer bitmap, which is what `getPixel`/`saveLayerImage`
read back. This is the compatibility baseline
(`docs/font_crates.md`).

## 10. Performance expectations and guards

* **Idle frames**: L2's `Scene::revision` fast path skips projection when the
  tree is unchanged, so a static frame records no new draw commands.
* **Graphical submission**: one command buffer per frame, one descriptor set
  per distinct texture, quads batched by pipeline.
* **Uploads**: sub-rect only; the staging ring is reused.
* **Per-frame attribution**: the renderer records `FrameTimings`
  (acquire/present/recreate/fence waits) in `RenderState`; the app's
  `FramePacing` (poll/input wait) closes the frame budget.
* **CPU compositing** remains the floor for destination-dependent blends and
  effects.

## 11. Validation

1. **Backend tests**: the null and Vulkan backends share the `RenderScene`
   contract; the null backend's software rasterizer is the correctness
   baseline.
2. **Blend matrix tests**: one quad per fixed-function `BlendMode` over a
   known background, checked against the reference formulas.
3. **Surface lifecycle tests**: resize, minimize, device-lost simulation.
4. **No-Bevy build**: `cargo tree -p render` contains neither `bevy` nor
   `wgpu`.
