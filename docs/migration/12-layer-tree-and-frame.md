# 12 — The layer tree, the projection and the frame

Status: **the render path, the L2 frame and the VM↔device mapping are
implemented.** This document is the end-to-end
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
| `layers: Vec<LayerState>` | the layer nodes: `window`, `parent`, `rect` (x/y/w/h), `visible`, `opacity`, `blend`, `bitmap` (main image), `image_left/top/width/height`, `hold_alpha`, `cache_bitmap`/`cache_enabled_count`, `transition_self`, and every TVP property L3 writes |
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

`engine::projection::project_scene_report(scene, texture)` (or
`project_window_report` for one window) produces a fresh `ProjectionReport` in
VM pixels: the `RenderScene` plus `drawn_layers`, the layer ids whose pixels the
pass actually emitted. `project_scene`/`project_window` remain the scene-only
wrappers.

```rust
pub struct ProjectionReport {
    pub scene: RenderScene,
    pub drawn_layers: Vec<u32>,
}

pub fn project_scene_report<F>(scene: &Scene, texture: F) -> ProjectionReport
where F: FnMut(u32) -> Option<Texture>;   // bitmap id → L1 texture handle
```

`drawn_layers` comes from the emit path, never from scene order. A layer is
reported when it pushes a node; a cached ancestor also reports the visible
descendants whose pixels its emitted cache bitmap contains. A skipped, hidden,
fully clipped or texture-less layer is not reported.

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
  `bitmap` (or its rendered `cache_bitmap`, or its transition `transition_self`
  image, which stands in for the layer's own image while its children keep
  drawing live). Solid fills and glyph runs are not produced by the projection.
* **Order**: `z` comes from a per-scene monotonic `next_z`, so siblings across
  all windows of a scene are globally ordered by insertion (layer order).
* **Root/background**: one default root node; `background = BACKGROUND_CLEAR`
  (`[0,0,0,1]`, opaque black). There is no view transform in the scene value:
  the L2 translation rewrites the nodes into device space (§7).

A layer is skipped (contributes no node) when it is hidden anywhere in its
chain, has no texture, is fully clipped away, or sits under a layer whose cache
bitmap already contains its pixels (`InternalComplete2` renders the subtree into
`CacheBitmap`). A layer with a `transition_self` image draws that image at the
main image's placement and is not added to the cached subtree, so its children
keep drawing live. A cached ancestor's subtree composition takes the same self
image in place of the layer's main image (`CopySelfForRect`,
`reference/cpp/core/visual/LayerIntf.cpp:6781-6783`), and a change to it
invalidates that cache like any other drawn-image change. The counts are
trace-logged, so an almost-empty frame is diagnosable.

**What the projection deliberately does not do.** It performs no batching, no
sorting, no transform nesting, and no mask/province/face work. Every node gets
`transform = IDENTITY` and `clip = None`; hierarchy exists for linking,
`z`-order and the inherited opacity/visibility only. The `dst` is already in
absolute window coordinates. Masks, province images, per-layer affine
transforms and the `hold_alpha` main-vs-province rule are open work (see §9).

## 4. The L1 draw tree

`render::scene` (`crates/platform/render/src/scene.rs`) defines the tree L1
consumes; L2 owns the value.

* `RenderScene { root, nodes: Vec<RenderNode>, background }` — an arena.
* `RenderNode { parent, children, content, transform, dst, src, opacity,
  color, visible, z, blend, clip, hold_alpha }`.
* `RenderContent` — `None` (container), `Solid([f32;4])`,
  `Texture(Texture)`, `Glyphs(GlyphRun)`.
* `Clip` — `Rect(RectF)` (axis-aligned, scissor) or `Mask(Texture)` (alpha).
* `Transform2D` — translate/scale/rotation/pivot. There is no view transform:
  the L2 translation rewrites `dst` into device space (§7).

Two free functions carry the space math, and both backends call them so they
cannot drift:

* `node_spaces(node, parent)` returns `(node_space, content_space)`:
  `node_space` applies the node transform and then the inherited parent
  transform; `content_space` additionally places the unit quad on the node's
  `dst` rectangle *before* `node_space` moves it to the target. Because `dst`
  is placed first, a device-space `dst` scales the rectangle's origin and its
  extent together — the property a letterbox needs.
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
refilling the persistent arena, then re-attaching the overlay), and then run
`GameFrame::after_project(&drawn_layers)` with the projection's actual emit
report. If that hook changes the scene, the engine uploads and projects again
before the single `render` call, so the frame draws the changed state. The
report is `FrameReport` (`frame_index`, `elapsed_ms`, the null counters, the
upload/prune/project/render costs and the two projection passes when needed);
the latest drawn set is also available through `Engine::drawn_layers()`.

`app::Host` is a wrapper around the engine. `app::GameHost` implements the
hook: `vm.tick` (input begin → platform events → input end), then
`compat::run_frame_body` (drain the timer posts the engine produced, sound,
video, async triggers, continuous handlers, idle compaction, movie/transition
tail), then the audio drain. Its `after_project` hook advances the
self-updating transitions whose destination or source subtree the projection
reported as drawn, then recomposites script-cached layers before the engine's
second projection. The mixer, video and non-self-updating transition pumps are
the remaining staged work; see [07 Phase 9](07-roadmap.md).

The timer contract is D27's: L2 waits and calls; L3 decides whether to arm and
turns the fire into an `onTimer` post. `TimerState::advance` is called only by
`Engine::frame` (it stays public until the timer surface collapses).

## 7. Where the pixel translation fits (D39/D40)

The projection emits the layer tree in **VM pixels**; `engine::translate`
then maps every node's `dst` and every rectangle clip into **device pixels**
before the overlay is attached. The pipeline receives device-space rectangles
and has no presentation policy of its own, so presentation is one rule — Fit
with letterbox.

`PixelTranslation` is a pure value: a uniform `min(device/vm)` scale plus a
centred offset. The engine refits it whenever the VM window size or the
framebuffer size changes (twice per frame: before the hook so input uses the
current mapping, after it so a script size change is projected the same
frame). At `s == 1` the mapping is the identity and the reference is matched
exactly. The translation is **internal to L2** (D41): L3 keeps calling L2 in
VM coordinates.

Input travels the other way: the composition root maps pointer/touch
coordinates with `to_vm_point` before the VM tick, so L3 hit-tests in VM
pixels. Readback (`getPixel`/`piledCopy`/`saveLayerImage`) is still served by
the CPU bitmap the script can already read, which is VM-exact; the
device-pixel readback path for a GPU-composed backing uses
`vm_pixel_region` and is not wired yet ([11](11-presentation-and-pixel-translation.md)
§7, [10](10-android-backends.md) "Device-pixel readback").

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
  non-self-updating transition pumps still run inside the hook; self-updating
  transitions advance after the projection's draw report. `Engine::run` is not
  built.
* **Pixel translation (D40)** — the Fit mapping, device-space projection and
  input inverse-map have landed (§7). Device-pixel readback (the
  `vm_pixel_region` consumer) and device-resolution glyph rasterization are
  open; the CPU bitmap the script reads is already VM-exact.
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
