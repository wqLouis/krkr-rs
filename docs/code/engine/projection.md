# `engine::projection` — the L2 → L1 seam

Source: `crates/engine/src/projection.rs`

The middle layer owns the logical [`Scene`](scene.md) and every property on
it; the bottom `render::RenderPipeline` consumes an abstract `RenderScene`
tree and owns traversal, batching and drawing. This module is the **single
seam** between the two: a pure field copy that turns one window (or a whole
scene) into a `RenderScene` with one `RenderNode` per *drawn* layer.

It is **not** a draw list: no batching, no sorting, no transform nesting, no
mask or province work happens here. The composition rules are stated in
[`arch/l2.md`](../../arch/l2.md) §3; this page is the code map.

---

## 1. Responsibility

* Resolve each drawn layer's absolute rectangle, opacity, visibility and clip
  through its ancestor chain.
* Resolve the image placement into a destination rectangle and a source
  sub-rectangle.
* Pass the layer's generic `BlendMode` through to the render node.
* Report which layers actually contributed pixels.

---

## 2. State it owns

None. The projection is a **pure function** over caller-owned state: the
`Scene` goes in, a fresh `RenderScene` comes out. It holds no globals, no
caches and no locks.

The texture table is supplied by the caller through a lookup closure —
typically `|bitmap_id| textures.get(&bitmap_id).cloned()` — so the same
function serves a live pipeline and a test double.

---

## 3. Public API

```rust
pub fn project_scene<F>(scene: &Scene, texture: F) -> RenderScene
where F: FnMut(u32) -> Option<Texture>;

pub fn project_scene_report<F>(scene: &Scene, texture: F) -> ProjectionReport
where F: FnMut(u32) -> Option<Texture>;

pub fn project_window<F>(scene: &Scene, window: u32, texture: F) -> RenderScene
where F: FnMut(u32) -> Option<Texture>;

pub fn project_window_report<F>(scene: &Scene, window: u32, texture: F) -> ProjectionReport
where F: FnMut(u32) -> Option<Texture>;

pub fn compose_layer(scene: &Scene, layer_id: u32) -> Option<ComposedLayer>;
pub fn resolve_blend(blend: BlendMode, composed_alpha: f32) -> BlendMode;

pub struct ComposedLayer {
    pub rect: Rect,
    pub opacity: f32,
    pub visible: bool,
    pub clip: Option<Rect>,
}

pub struct ProjectionReport {
    pub scene: RenderScene,
    pub drawn_layers: Vec<u32>,
}
```

`compose_layer` is the standalone form of the composition the pass does
inline; `resolve_blend` is the standalone form of the blend rule.

---

## 4. What one pass does

For every window, in `Scene::windows` order, and for every layer in
`Scene::window_layer_order(window)` (parent-first):

1. **Skip a cached subtree.** A layer under a cached ancestor contributes no
   node: its pixels are already in the ancestor's rendered cache bitmap. The
   descendants that are visible under that ancestor are still reported as
   drawn, because their pixels are baked into the emitted cache.
2. **Pick the image to draw** — the layer's transition *frame* (the two
   sources a composited transition placed for this tick), its transition self
   image (which stands in for the main image while the children keep drawing
   live), its rendered cache bitmap, or its main image.
3. **Compose** the absolute rectangle (ancestor `x`/`y` accumulate, the layer
   keeps its own `w`/`h`), the opacity (the product of the layer and every
   ancestor, each clamped to `[0, 1]`, NaN → 0), the visibility (the AND of
   the chain) and the clip (the intersection of the layer's rect with every
   ancestor's rect and the layer's own clip rect).
4. **Link the node** to its nearest *drawn* ancestor's node, or to the scene
   root. A skipped layer passes its nearest-drawn ancestor down, so its
   children still attach correctly.
5. **Resolve the placement**: the destination is the layer rect plus the
   image placement (`image_left`, `image_top`, `image_width`,
   `image_height`), clipped to the layer rect; the source is the matching
   sub-rectangle of the bitmap. A negative `image_left` selects a
   sprite-sheet frame.
6. **Resolve the blend**: the stored generic `BlendMode` passes through,
   except that an opaque `Replace` below full composed opacity degrades to
   `SourceOver` — it must still blend.
7. **Emit** the node and record the layer id as drawn. A transition frame
   emits its two sources instead of one node: source 1 with the layer's blend
   and opacity, source 2 over it at the frame's alpha (source-over, because it
   fades), each with its own `dst`/`src` inside the frame (`Scroll`) or the
   whole frame (`CrossFade`).

The scene root carries `background = [0, 0, 0, 1]` (opaque black) and a
single monotonic `z` sequence shared by all windows of the scene, so siblings
across windows are globally ordered by insertion.

---

## 5. Invariants

* **The projection is a field copy.** One node per *drawn* layer; ancestor
  opacity, position and visibility already composed; `dst`/`src` resolved;
  blend passed through. Nothing that belongs to the pipeline happens here.
* **`drawn_layers` follows the emit path**, not the scene order. A layer is
  reported when it pushes a node; a cached ancestor also reports the visible
  descendants whose pixels its cache bitmap contains. A hidden, empty, fully
  clipped or texture-less layer is not reported.
* **A layer is skipped** when it is hidden anywhere in its chain, has no
  texture, is fully clipped away, or sits under a layer whose cache bitmap
  already contains its pixels.
* **A transition frame is two nodes of one layer**: the layer id is reported
  once, the second source's node is the layer's `nearest` node for its
  children, and a whole-subtree frame covers the children exactly like a cache
  bitmap (they are reported, not emitted).
* **A transition self image takes precedence over the cached subtree**: it
  covers only the layer's own image, so the children keep drawing live and
  the layer never becomes a cached-subtree root.
* **A cached ancestor's subtree composition takes the same self image** in
  place of the layer's main image.
* **A cross-window `parent` link, an invalid link or a cycle stops the
  chain** — the composition simply ends there.
* **Every node gets an identity transform.** The tree carries drawable
  structure and blend only; the hierarchy exists for linking, `z` order, the
  inherited opacity/visibility and the ancestor clip.
* **The clip is the intersection** of the layer's own absolute rect and every
  ancestor rect, including texture-less container layers; a zero-sized
  intersection emits no node.
* **The output is in VM pixels.** The device-space rewrite is the
  [`translate`](translate.md) module's job.

---

## 6. Data flow

```
Scene (VM pixels)                     textures: Map<u32, Texture>
      │                                        │
      ▼                                        ▼
project_scene_report(scene, |id| textures.get(&id).cloned())
      │
      ├─► RenderScene { root, nodes, background }      (VM pixels)
      └─► drawn_layers: Vec<u32>
                │
                ▼
      root.transform = PixelTranslation::root_transform()
                │
                ▼
      RenderScene (VM pixels, Fit root) ──► render::render(...)
```

---

## 7. Failure modes

* **No error type.** The projection cannot fail: it is a field copy over
  validated state. A layer that cannot be drawn is *skipped*, which is a
  visible outcome (it is absent from `drawn_layers`), not a silent failure.
* **A missing texture skips the layer.** The lookup closure returns `None` and
  the layer contributes no node; the engine invalidates the projection when a
  texture appears for a bitmap that had none.
* **The skip reasons are counted** (hidden / empty / clipped) and reported in
  a trace log, so a scene that renders almost nothing is diagnosable from the
  outside.

---

## 8. Current limits

* **Masks and province images are not emitted.** `Clip::Mask` and the
  main-vs-province composition are produced neither here nor by the
  backends; `hold_alpha` travels but is not interpreted.
* **Per-layer affine transforms are not projected.** The projection emits an
  absolute `dst` with an identity `transform`.
* **`Layer.face` is not read** by the steady-state projection.
* **Only `RenderContent::Texture` is emitted** — solid fills and glyph runs
  are not produced by the projection.

---

## 9. Tests

* `crates/engine/tests/projection.rs` — the composition rules, the blend
  downgrade, the sprite placement, the cache behaviour, the emit report, the
  transition frame's two nodes (including the offscreen composition that must
  agree with the CPU blend within the documented tolerance, see
  [trans.md](trans.md) §6.1), and an engine frame that draws a composited
  transition without a display bitmap.

---

## 10. See also

| topic | document |
|---|---|
| the seam and the composition rules | [`arch/l2.md`](../../arch/l2.md) §3 |
| the L1 draw tree the output feeds | [`arch/l1.md`](../../arch/l1.md) §3 |
| the tree this projects | [scene.md](scene.md) |
| the mapping applied afterwards | [translate.md](translate.md) |
| the archived design trail | [`docs.old/migration/12-layer-tree-and-frame.md`](../../../docs.old/migration/12-layer-tree-and-frame.md) §3 |
