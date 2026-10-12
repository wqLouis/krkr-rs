# 04 — Render pipeline: Bevy → `ash` (Vulkan)

Status: design sketch. Supersedes the Bevy projection in `render/src/sync.rs`
and the wgpu material system in `render/src/blend.rs`.

Scope: replace Bevy's renderer *and* its window/input/UI substrate with a
thin Vulkan core plus winit. The middle layer keeps the layer-system state
and passes a `RenderScene` tree plus a `RenderState` value; the bottom
`RenderPipeline` consumes the tree and owns all traversal, composition,
batching and drawing. See
[`03-middle-layer-api.md`](03-middle-layer-api.md#2-render-pipeline).

---

## 1. Why `ash`, why Vulkan-only

The GPU workload is small and fixed:

* textured quads and solid fills;
* ~7 fixed-function blend states plus 12 destination-dependent modes that
  currently degrade to source-over (`render/src/blend.rs:436`);
* dirty-region texture uploads;
* no 3D, no compute, no skeletal animation, no MSAA requirement.

wgpu's value is backend portability, which we do not need (D4). `ash` gives
direct pipeline/blend/descriptor control and no extra abstraction. `ash` is
already in `Cargo.lock` (transitive through wgpu), so direct use adds no new
download beyond helpers.

Likely helper crates (to be confirmed before implementation):

| Concern | Candidate | Note |
|---------|-----------|------|
| Vulkan bindings | `ash` | already in lock |
| Surface creation | `ash-window` / `raw-window-handle` | winit provides the raw handles |
| Memory allocation | `gpu-allocator` | already in lock via wgpu |
| Windowing | `winit` (direct) | already in lock |
| Android | `android-activity` | already in lock via winit |

## 2. Geometry of the new renderer

```
L4 host (winit / android-activity)
   │  frame boundary
   ▼
render::RenderPipeline   (`&self`, stateless)
   ├─ Device       — instance, physical/logical device, queues, allocator
   ├─ Pipeline set — shader programs + blend/attachment state
   ├─ Traversal    — walks the L2-owned RenderScene tree
   ├─ Composition  — parent transforms, opacity, z, clip, blend selection
   ├─ Uploads      — scheduling of dirty textures into RenderState
   └─ Frame        — acquire → record → submit → present

L2 stores the mutable side: RenderScene tree · RenderState (swapchain,
staging, compiled pipelines, texture table, atlas uploads, targets)
```

The render pipeline is single-threaded on the host thread initially. A
submission thread can be added later without changing the API.

**Object/state split (D9).** The only L1 object is the stateless
`RenderPipeline`; the device lives inside it as immutable mechanism. Every
mutable render thing is in the L2-owned `RenderState` (swapchain, staging,
compiled pipelines, texture table, offscreen targets, atlas uploads) and the
L2-owned `RenderScene` tree. `render(&self, surface, &mut state, &scene)`
mutates only the state the caller passed. L1 has no globals, no
`HashMap<Id, Resource>` and no lazy caches. The same tree + state produces
the same frame.

## 3. Pipeline set

Two base programs are required, plus one shader variant for the
destination-dependent blends (§4):

1. **Textured quad** — standard unit quad, `textureSample` × vertex color.
   This covers layers, video frames, and future GPU glyphs.
2. **Solid quad** — same vertex path, fragment outputs the vertex color.
   This covers fills, and the screen clear can use a scissor+clear or a
   fullscreen fill.

Blend state is not a shader concern; it is a per-pipeline
`VkPipelineColorBlendAttachmentState`. `render/src/blend.rs`
(`blend_state_for`, `blend.rs:352`) already encodes the intent as wgpu
`BlendState`; it is re-expressed as Vulkan blend factors/ops. The mapping:

| TVP / `LayerBlendMode` | Vulkan `srcColor` / `dstColor` / `op` | Notes |
|------------------------|----------------------------------------|-------|
| SourceOver / Alpha | `SRC_ALPHA` / `ONE_MINUS_SRC_ALPHA` / `ADD` | default |
| Replace / Opaque | `ONE` / `ZERO` / `ADD` (with pre-multiplied alpha or opaque) | |
| Additive | `SRC_ALPHA` / `ONE` / `ADD` | |
| Subtractive | `SRC_ALPHA` / `ONE` / `REVERSE_SUBTRACT` | |
| AddAlpha | `SRC_ALPHA` / `ONE` / `ADD` with alpha channel rules | see current `blend.rs` |
| Multiply | `DST_COLOR` / `ZERO` / `ADD` (pre-multiplied source) | matches current pre-multiply trick |
| Screen | `ONE_MINUS_DST_COLOR` / `ONE` / `ADD` (pre-multiplied) | |
| destination-dependent | any (ping-pong) | see §4 |

`ltBinder` is not submitted at all (L2 drops it while flattening).

Pipelines are keyed on `(program, blend, color-write-mask, sRGB)`. There are
few enough variants to build them lazily on first use and cache in a map.

## 4. Destination-dependent blends

These 12 modes (`render/src/blend.rs`: ColorDodge/5, ColorBurn, Darken, Lighten,
Difference/5, Exclusion, Overlay, HardLight, SoftLight) need to read the
destination. The current code punts (source-over + warning). The Vulkan core
implements them by rendering to an offscreen target:

1. L2 marks a node `RenderNode.blend = BlendMode::DestinationDependent(mode)`
   when it must composite over existing content (a layer with a
   destination-dependent `Layer.type`).
2. The renderer:
   * copies the current target content for the quad's `dst` region into a
     sampled image (or keeps the target already bound as a sampled texture),
   * draws the quad with a **shader variant** that samples both source and
     destination and evaluates the Porter-Duff/Photoshop formula,
   * blends the shader output with a plain replace/source-over pipeline.
3. The shader variant selection is by `DstBlend`; one WGSL/GLSL source with a
   specialization constant, or a small set of pipelines.

This is the only place that needs a more complex fragment shader. It is also
the mechanism for future GPU transitions (crossfade/scroll/wave/mosaic),
which today are a completion-callback stub.

## 5. Texture upload and caching

* L2 owns `BitmapState` (CPU RGBA) and its dirty flags.
* On `upload_texture`, the pipeline writes the dirty region into a
  `VK_IMAGE_USAGE_TRANSFER_DST` image via a staging buffer. A persistent
  staging ring (in `RenderState`) avoids per-upload allocation.
* Sub-rect uploads (`PixelRegion`) are supported from day one: `drawText`
  and raster ops only touch part of a bitmap (`render/src/sync.rs` currently
  always uploads the whole buffer).
* SRGB: today's images are `Rgba8UnormSrgb` and colors are converted
  through `to_linear()` (`sync.rs:955`). The Vulkan path must pick one color
  convention (sRGB image format + linear shader math) and apply it
  consistently; this is called out as a correctness risk in
  `07-roadmap.md §6`.
* Pruning: when L2 drops a bitmap, it calls `drop_texture`. The current
  renderer prunes automatically by diffing against the scene
  (`sync.rs:752`); in the new design L2 explicitly owns the bitmap→texture
  lifetime.

## 6. Presentation, resize, DPI

* `begin_frame` acquires a swapchain image; `end_frame` presents.
* `winit` reports `Resized`/`ScaleFactorChanged`; L4 forwards the new
  surface size to `RenderPipeline::resize`, which recreates the swapchain
  (including `OUT_OF_DATE`/`SUBOPTIMAL`/minimized handling).
* The letterboxed game projection is L2's job: the middle layer computes the
  `dst` rects in framebuffer pixels from the logical window size. The
  existing `window_logical_size`/`ScalingMode::AutoMin` logic is ported as a
  pure helper (it already is).
* `Window.setSize`/`setZoom`/`changeScreenMode`: L2 writes the declared
  size; L4 calls `HostWindow::set_logical_size` (today
  `sync_host_window_resolution`, `sync.rs:367`).

### 6.1 Presentation scaling

This is a deliberate departure from the Win32 reference, at the user's
request: a modern display should show the game scaled rather than pinned to
its declared size.

The behaviour to copy is not the Win32 environment's but the **cocos2d**
one's, which is what the reference's own non-Windows builds use. It sets a
design resolution and a `ResolutionPolicy`
(`reference/cpp/core/environ/cocos2d/AppDelegate.cpp:63-75` — `SHOW_ALL` on
desktop, `EXACT_FIT` on Android/iOS) and sizes its scene from the frame
aspect (`MainScene.cpp:436-441`).

`ScalingMode` selects how the engine presents the game:

| mode | logical size | transform | reference |
|---|---|---|---|
| `Game` (default) | the game's declared size | none | the Win32 environment |
| `Fit` | the game's declared size | uniform scale, letterboxed | cocos2d `SHOW_ALL` (`AppDelegate.cpp:73-75`) |
| `Fill` | the game's declared size | non-uniform scale to the window | cocos2d `EXACT_FIT` (`AppDelegate.cpp:70`) |
| `Expand` | the design height at the window's aspect | uniform scale, no letterbox | cocos2d `MainScene::initialize` (`MainScene.cpp:436-441`) |

`Game` is the default, so an unconfigured run matches the reference exactly
and every parity test keeps its meaning.

The scale and offset are L2 state: the middle layer owns the projection and
L1 applies it while compositing, per D10. Input is inverse-mapped from
framebuffer to logical coordinates before dispatch, so a script's own
hit-testing keeps working unchanged.

`System.screenWidth`/`screenHeight` and `Window.innerWidth`/`innerHeight`
report the **logical** size — the declared size for `Game`/`Fit`/`Fill`, the
grown size for `Expand` — and never the framebuffer size, preserving the
reference's design-versus-frame split (`MainScene.cpp:436-441`).

## 7. Window, input and UI (the Bevy substrate)

Bevy is not only a renderer here; it also provides windowing, input and UI.
Each must be replaced.

### 7.1 Windowing

* Desktop: `winit` `EventLoop` + `ApplicationHandler`. No Bevy plugins.
* Android: `android-activity` (`GameActivity`) owns the `ANativeWindow`;
  the `NativeWindow` is handed to `ash-window` to create/recreate the
  Vulkan surface. The current JNI contract in `crates/android/src/lib.rs`
  (three static fields `pendingGameDir`/`pendingGameName`/`pendingStateDir`
  read before `super.onCreate`) stays; only the app construction changes.

### 7.2 Input

* Replace `bevy::input` reads in `input_bridge.rs` with a `winit`-fed
  `InputState` (see API doc §6).
* The **pure** parts are reused verbatim: `window_to_game_transform`,
  `hit_test`/`layer_at`, `touch_gesture.rs`, the button-delay/transition
  state machine, and the static window/menu request queues
  (`natives/window.rs` → `consume_window_requests`).
* Android touch/back-button handling (`docs/android.md §5`) is wired from
  `android-activity` callbacks instead of winit's Android shim.

### 7.3 Menu UI

`render/src/menu.rs` is Bevy UI. Options, in order of plan preference:

1. **CPU-drawn menu over bitmaps** — the menu is simple (a vertical list of
   text + highlight). Rasterize it with the existing text/glyph and fill
   code into a `BitmapState`, then draw it as ordinary layer quads. Zero new
   renderer surface, matches the reference's own UI style.
2. **GPU text/rect draw list** — reuse glyph coverage + quads.
3. **A retained-mode UI crate** — rejected: it re-introduces a framework
   dependency exactly like Bevy.

The plan is option 1 for the first milestone; `MenuPlugin`/`Interaction`
handling becomes a small immediate-mode state machine fed by `InputState`.

## 8. What gets deleted or moved

| Today | Fate |
|-------|------|
| `render/src/sync.rs` (`sync_scene`, `BitmapAssets`, `GpuPrimitives`, `FrameBlendMaterials`) | replaced by the L2 `RenderScene` projection + L1 `RenderPipeline` |
| `render/src/blend.rs` (`LayerBlendMaterial`, wgpu `BlendState`, WGSL) | TVP→`BlendMode` mapping kept in L2; wgpu `BlendState` re-expressed as Vulkan state in L1 |
| `render/src/main.rs` demo harness | keep as a `render` example once the new API exists |
| `render/src/runner.rs` Bevy `App` wiring | replaced by `app` loop |
| `render/src/input_bridge.rs` Bevy event reads | split: event source → host; logic → L2 |
| `render/src/menu.rs` Bevy UI | reimplemented as CPU-drawn menu |
| `render/src/touch_gesture.rs` | moved to L2 (already pure) |
| `crates/android` `#[bevy_main]` | `android-activity` + `krkr-core` host |
| Bevy dependency | removed from the workspace at the end |

Pure helpers to lift out before the renderer swap (`sync.rs`/`scene.rs`):
`compose_layer`, `visible_image_region`, `rect_center`,
`window_to_game_transform`, `window_logical_size`, `host_window_resize`,
`clamp_opacity`, `window_layer_order`, `sprite_z`, `bitmap_tint`,
`fill_sprite_color`. They are already unit-tested without a GPU.

## 9. Glyph rendering engine

Decision (see `03-middle-layer-api.md §3`): the bottom renderer exposes a
`FontSystem` object, immutable `Font` values and a stateless `rasterize_glyph`
that produces CPU coverage. L2 keeps the atlas, layout and CPU composition,
so `getPixel`/`piledCopy`/masks/province continue to work.
An optional later phase uploads coverage to a glyph atlas and emits text as
quads; the API does not change.

Rationale: moving text fully to the GPU breaks pixel read-back and would
require a second copy of the layer contents back to the CPU. The current
CPU path (`Layer.drawText` → `paint_layout` → `BitmapState`,
`tvp-visual/src/natives/layer.rs`) is the compatibility baseline and is
already measured as fast on the warm path (`docs/font_crates.md`).

## 10. Performance expectations and guards

* **Idle frames**: L2's `Scene::revision` fast path already skips work; the
  new loop keeps it, so a static VN frame records zero draw commands.
* **Graphical submission**: one command buffer per frame, one descriptor set
  per distinct texture, quads batched by pipeline. Expect far fewer state
  changes than Bevy's ECS material pipeline.
* **Uploads**: sub-rect only; staging ring reused.
* **No per-frame entity churn**: the `sync_scene` spawn/despawn/material
  churn (`docs/optimization.md`) simply ceases to exist.
* **CPU compositing** remains the floor for transitions and effects; this is
  unchanged from today.

## 11. Validation

1. **Golden images**: render a scripted scene through the current Bevy path
   and the Vulkan path; compare framebuffer readbacks pixel-for-pixel
   (sRGB tolerance). Keep the headless scene dump as the scene generator.
2. **Blend matrix tests**: one quad per `BlendMode` over a known background,
   signed off against the reference formulas.
3. **Surface lifecycle tests**: resize, minimize, device-lost simulation.
4. **No-Bevy build**: `cargo tree -p render` contains neither `bevy`
   nor `wgpu`.
