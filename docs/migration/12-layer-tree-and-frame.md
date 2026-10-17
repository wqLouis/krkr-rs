# 12 — The layer tree, the projection and the frame

Status: **the render path and the L2 frame are implemented; the
pixel-translation layer (D40) is not.** This document is the end-to-end
contract for *how a layer tree becomes a frame*. It consolidates the ownership
rules that [02 §2.1/§3](02-target-architecture.md) and
[03 §2](03-middle-layer-api.md#2-render-pipeline) already state, adds the code
map that neither did, and fixes the seam the two coordinate spaces (D39/D40,
[11](11-presentation-and-pixel-translation.md)) must pass through. The Vulkan
internals it references are in [04](04-renderer-vulkan.md); the file system the
tree loads its bitmaps from is in [06](06-file-system.md).

---

## 1. Two trees and one seam

Rendering is the collapse of two trees that live in different layers:

```
 L2  logical tree          engine::scene::Scene          (owns every property)
        │
        │  projection — engine::projection::project_scene / project_window
        │  a pure field copy; NOT a draw list, no batching, no sorting
        ▼
 L1  draw tree             render::scene::RenderScene    (arena of RenderNode)
        │
        │  the single draw action — render::render(pipeline, surface, state, scene)
        ▼
 L1  backend               null: null.rs + cpu.rs   ·   vulkan: vulkan/mod.rs + draw.rs
```

The seam is deliberately one function call in each direction, owned by one
layer on each side (D10):

* **L2 owns the logical tree and the projection.** `engine::Scene` is the state
  store; `engine::projection` is the field copy that turns it into a
  `RenderScene`. Both are generic — no TVP/TJS vocabulary (D16).
* **L1 owns the draw tree's *shape*, the traversal, composition, batching and
  drawing.** `render::render` is the *only* drawing entry point; the pipeline
  never asks L2 for a draw list, and L2 never walks the tree to order or batch
  it.
* **L3 never appears here.** The TVP properties that make the tree meaningful to
  a script (masks, province images, `Layer.type`, focus, callbacks, the
  `arc.xp3>path` names its textures come from) are set on the L2 tree by L3 and
  are invisible to L1.

## 2. The L2 logical tree

`engine::scene::Scene` (`crates/engine/src/scene.rs`) is the whole logical
scene and all of its state:

| field / type | role |
|---|---|
| `windows: Vec<WindowState>` | one entry per `Window`; `inner_size`, `visible`, `opacity`, `primary_layer`, focused layer, focus domains, `layers` |
| `layers: Vec<LayerState>` | the layer nodes: `window`, `parent`, `rect` (x/y/w/h), `visible`, `opacity`, `blend`, `bitmap` (main image), `image_left/top/width/height`, `hold_alpha`, `cache_bitmap`/`cache_enabled_count`, and every TVP property L3 writes |
| `bitmaps: Vec<BitmapState>` | CPU RGBA pixels + dirty flags; the texture source |
| `fonts: Vec<FontState>` | parsed faces |
| `revision: u64` | bumped by `touch()` on every mutation; the projection's skip key (`revision()`) |

`Scene` keeps private id→index maps, a decoded-image cache and that revision
counter. It holds **no** globals, locks or hidden caches; the projection is a
pure function over it.

`Scene::window_layer_order(window)` returns the window's layers
**parent-first**, which is what lets the projection compose ancestors in a
single forward pass (§3).

## 3. The projection (L2 → L1)

`engine::projection::project_scene(scene, view, texture)` (or `project_window`
for one window) produces a fresh `RenderScene`:

```rust
pub fn project_scene<F>(scene: &Scene, view: ViewTransform, texture: F) -> RenderScene
where F: FnMut(u32) -> Option<Texture>;   // bitmap id → L1 texture handle
```

For every layer in `window_layer_order` it builds at most one `RenderNode`:

* **Composition** (`compose_layer` duplicates it for standalone callers): the
  absolute rect accumulates ancestors' `x`/`y` (saturating) while keeping the
  layer's own `w`/`h`; opacity is the product of the layer and every ancestor,
  each clamped to `[0,1]` (NaN → 0); visibility is the AND of the chain. A
  cross-window `parent` link, an invalid link or a cycle stops the chain.
* **Parent linking**: the node's `parent` is its nearest *drawn* ancestor's node,
  or the scene root. A skipped layer passes its own nearest-drawn ancestor down
  so its children still attach correctly.
* **Sprite placement** (`visible_image_region`): `Layer.rect` plus
  `ImageLeft/Top/Width/Height` give a destination `RectF` (window coordinates)
  and a source `PixelRegion` (bitmap pixels), clipped to the layer rect. A
  negative `ImageLeft` selects a sprite-sheet frame. A fully clipped image
  produces no node.
* **Blend** (`resolve_blend`): the stored generic `BlendMode` passes through,
  except an opaque `Replace` below full composed alpha degrades to
  `SourceOver` (it must still blend).
* **Content**: today only `RenderContent::Texture` is emitted — the layer's
  `bitmap` (or its rendered `cache_bitmap`). Solid fills and glyph runs are not
  produced by the projection.
* **Order**: `z` comes from a per-scene monotonic `next_z`, so siblings across
  all windows of a scene are globally ordered by insertion (layer order).
* **Root/background**: one default root node; `background = BACKGROUND_CLEAR`
  (`[0,0,0,1]`, opaque black, ported from the old Bevy camera clear);
  `view` is the framebuffer transform the caller passes.

A layer is skipped (contributes no node) when it is hidden anywhere in its
chain, has no texture, is fully clipped away, or sits under a layer whose cache
bitmap already contains its pixels (`InternalComplete2` renders the subtree into
`CacheBitmap`). The counts are trace-logged, so an almost-empty frame is
diagnosable.

**What the projection deliberately does not do.** It performs no batching, no
sorting, no transform nesting, and no mask/province/face work. Every node gets
`transform = IDENTITY` and `clip = None`; hierarchy exists for linking,
`z`-order and the inherited opacity/visibility only. The `dst` is already in
absolute window coordinates. Masks, province images, per-layer affine
transforms and the `hold_alpha` main-vs-province rule are open work (see §9).

## 4. The L1 draw tree

`render::scene` (`crates/platform/render/src/scene.rs`) defines the tree L1
consumes; L2 owns the value.

* `RenderScene { root, nodes: Vec<RenderNode>, background, view }` — an arena.
* `RenderNode { parent, children, content, transform, dst, src, opacity,
  color, visible, z, blend, clip, hold_alpha }`.
* `RenderContent` — `None` (container), `Solid([f32;4])`,
  `Texture(Texture)`, `Glyphs(GlyphRun)`.
* `Clip` — `Rect(RectF)` (axis-aligned, scissor) or `Mask(Texture)` (alpha).
* `Transform2D` — translate/scale/rotation/pivot; `ViewTransform` — the
  whole-scene framebuffer scale+offset.

Two free functions carry the space math, and both backends call them so they
cannot drift:

* `node_spaces(node, parent)` returns `(node_space, content_space)`:
  `node_space` applies the node transform and then the inherited parent
  transform; `content_space` additionally places the unit quad on the node's
  `dst` rectangle *before* `node_space` moves it to the target. Because `dst`
  is placed first, a presentation `ViewTransform` scales the rectangle's origin
  and its extent together — the property a letterbox needs.
* `child_sort_key` / `children_are_sorted` keep sibling `z` ordering identical
  across backends; the common case (L2 appends children already in `z` order)
  iterates the borrowed slice without a clone and sort.

`hold_alpha` is the one field whose name is a reference term. It travels
because the D14 main-vs-province selection is a documented hook; the pipeline
does not interpret a TVP concept from it.

## 5. The L1 traversal and draw

Both backends implement the same traversal rules — parent transforms, composed
opacity, sibling `z` order, rectangle clips, typed failures on what they do not
implement — and both are capped at `MAX_TRAVERSAL_DEPTH` (1024).

### 5.1 Vulkan (`vulkan/mod.rs` → `vulkan/draw.rs::record_scene`)

`Recorder::visit(id, parent_transform, parent_opacity, scissor, depth)`:

1. bounds-check the node, respect `visible`, multiply the opacity; skip when
   the composed opacity is ≤ 0;
2. `node_spaces` → `node_space`/`content_space`;
3. intersect the node's clip with the inherited scissor (`Clip::Rect` → a
   scissor; `Clip::Mask` → `RenderError::Unsupported`);
4. `draw` the payload (`Solid` or `Texture`; a `Texture` that is not uploaded
   is skipped; `Glyphs` → `Unsupported`);
5. recurse into children in `z` order, passing `node_space` and the scissor.

`draw_quad` selects the pipeline for `(program, blend)` in the prebuilt
`PipelineSet`, binds it when it changes, and pushes one `PushConstants` block
(`mvp`, `tint`, `uv_rect`). The per-node `color` is the tint (used by the
future glyph path to recolour an atlas), the alpha carries the composed
opacity, and RGB is linearised for the sRGB target.

### 5.2 Null / CPU (`cpu.rs::render_scene`, `null.rs::traverse`)

The headless null backend has two jobs. For the **framebuffer** it only
*counts* the draws (`null.rs::traverse` → `FrameStats`) — there is no window to
show. For an **offscreen target** it runs the real software rasterizer
(`cpu.rs::render_scene`), which is the **correctness baseline**: it clears to
`background`, walks the tree with the same `visit` rules and samples each quad
(`sample_texture`, nearest texel) into an RGBA8 buffer. Because a CPU
compositor naturally reads the destination, it evaluates the
destination-dependent `BlendMode`s (the `DstBlend` family) exactly, using the
KiriKiri/Photoshop formulas from
`reference/cpp/core/visual/gl/blend_functor_c.h` — the Vulkan backend reports
them `Unsupported` rather than substituting source-over, and uses the CPU
result as its behavioural reference.

### 5.3 Textures and uploads

L2 owns `BitmapState` and its dirty flags. `upload_textures` batches every dirty
sub-rectangle (`PixelRegion`) into one staging pass and one GPU wait; the
Vulkan backend records and submits the batch once. `drop_texture` is called when
L2 drops a bitmap, so bitmap→texture lifetime is explicit L2 state.

## 6. Per-frame orchestration

**The engine owns the frame.** `engine::Engine::step(dt_ms, hooks)` /
`frame(now_ms, hooks)` set the frame clock, advance the L2 timer registry, run
the single `GameFrame::before_render(now_ms, dt_ms)` hook, refresh the overlay,
upload dirty bitmaps, project when `Scene::revision` changed (clearing and
refilling the persistent arena, then re-attaching the overlay), and make one
`render` call. The report is `FrameReport` (`frame_index`, `elapsed_ms`, the
null counters and the upload/prune/project/render costs).

`app::Host` is a wrapper around the engine. `app::GameHost` implements the
hook: `vm.tick` (input begin → platform events → input end), then
`compat::run_frame_body` (drain the timer posts the engine produced, sound,
video, async triggers, continuous handlers, idle compaction, movie/transition
tail), then the audio drain. The mixer, video and transition pumps are the
remaining staged work; see [07 Phase 9](07-roadmap.md).

The timer contract is D27's: L2 waits and calls; L3 decides whether to arm and
turns the fire into an `onTimer` post. `TimerState::advance` is called only by
`Engine::frame` (it stays public until the timer surface collapses).

## 7. Where the pixel translation fits (D39/D40)
## 7. Where the pixel translation fits (D39/D40)

Today the projection takes a **whole-tree `ViewTransform`** and the pipeline
applies it around the scene root (`RenderScene.view`). D39/D40 replace that
with a device-space projection:

* presentation is one rule — **Fit with letterbox** — so there is no
  `ScalingMode` and no `--scaling`;
* the **L2 pixel-translation layer** (`PixelTranslation`, D40) converts a
  VM-space layer rect into a device-space `dst` *before* the node is built, and
  converts a device read region back into a VM value on readback;
* `RenderScene.view` and `ViewTransform` disappear, because the rectangles the
  pipeline receives are already in device space and the pipeline has no
  presentation policy of its own ([04 §6](04-renderer-vulkan.md)).

At scale `s == 1` this is the identity and the reference is matched exactly.
The translation is recomputed per size change **inside `frame`**, which is why
D40 depends on D26. Readback (`getPixel`/`piledCopy`/`saveLayerImage`) scales
the *access*, not the buffer: a VM read of `(x, y)` samples the device region
`[x·s,(x+1)·s)×[y·s,(y+1)·s)`.
[11](11-presentation-and-pixel-translation.md) §6–§7 is the API contract;
[10](10-android-backends.md) "Device-pixel readback" is the Android transport.

The offscreen targets that already exist (`render_to_target` /
`read_render_target`) are the primitive the readback path builds on, and the
CPU rasterizer is the baseline the translation's readback is checked against.

## 8. Boundaries and invariants

1. **L2 owns the logical tree; L1 owns traversal/draw.** L2 never builds a draw
   list, sorts, batches or selects blend state; L1 never names a TVP concept.
   (Acceptance criterion 02 §9.4.)
2. **The projection is a field copy.** One node per *drawn* layer, ancestor
   opacity/position/visibility composed, `dst`/`src` resolved, blend passed
   through. No work that belongs to the pipeline.
3. **The draw tree carries drawable structure and blend only.** `RenderContent`
   is content, not semantics; `hold_alpha` is the single reference-named hook.
4. **No silent fallbacks (D11).** A mask clip, a GPU glyph run or a
   destination-dependent blend the backend cannot do is a typed
   `RenderError::Unsupported`, never a substituted draw. The two backends fail
   on the same set.
5. **One ordering rule.** Sibling `z`, the traversal depth cap and the space
   math live in `render::scene` and are shared by every backend, so the null
   and Vulkan paths cannot drift in composition.
6. **The two trees are separate values.** `Scene` is generic engine state;
   `RenderScene` is L1-shaped. The projection is the only bridge, and it takes
   the texture table as a closure so it is testable without a GPU.

## 9. What is not built yet

* **L2 frame (D26)** — the frame entry point, clock, timer pump and
  projection/render ownership have landed (§6). The mixer, video and
  transition pumps still run inside the hook, and `Engine::run` is not built.
* **Pixel translation (D40)** — device-space projection and scaled readback;
  the Fit mapping, input inverse-map and readback are open (§7, `11`).
* **Masks and province images** — `Clip::Mask` and the main-vs-province
  composition are emitted by neither the projection nor the backends;
  `hold_alpha` travels but is not interpreted.
* **Per-layer transforms** — the projection emits absolute `dst` with an
  identity `transform`; affine layer transforms are not projected.
* **GPU glyph runs** — `RenderContent::Glyphs` is defined but both backends
  report it `Unsupported`; text is composited into the layer bitmap instead
  ([04 §9](04-renderer-vulkan.md)).
* **Destination-dependent blends on Vulkan** — CPU-only; the Vulkan path is the
  offscreen-compositing design in [04 §4](04-renderer-vulkan.md).
