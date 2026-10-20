# `engine::translate` — the VM-pixel ↔ device-pixel mapping

Source: `crates/engine/src/translate.rs`

Scripts and the layer tree describe the scene in **VM pixels** — the size the
game declared for its window. The framebuffer is in **device pixels**, which
on most displays is a different size. `PixelTranslation` is the single
mapping between the two spaces: a uniform scale of `min(device / vm)` per
axis plus a centred offset, so the whole game surface is visible and the
leftover margin stays clear.

**Responsibility.** Hold the Fit-with-letterbox mapping for one pair of
surface sizes; map geometry **into** device space for the projection and the
overlay; map input **back** into VM space for the composition root; describe
the device region one VM pixel covers, for the readback path.

It is the only layer that names both spaces. L3 never sees a device
coordinate; L1 never sees a VM coordinate. The rule and the boundary are
stated in [`arch/l2.md`](../../arch/l2.md) §4 and
[`rules/arch_rules.md`](../../rules/arch_rules.md) §5; this page is the code
map.

---

## 1. State it owns

```rust
pub struct PixelTranslation {
    pub scale: f32,
    pub offset: (f32, f32),
    pub vm_size: (u32, u32),
    pub device_size: (u32, u32),
}
```

The value is derived from the two sizes alone. It has **no clock and no
cache**: the engine recomputes it whenever the VM size or the framebuffer
size changes.

---

## 2. Public API

```rust
impl PixelTranslation {
    pub fn fit(vm_size: (u32, u32), device_size: (u32, u32)) -> Result<Self, TranslateError>;
    pub fn is_identity(&self) -> bool;
    pub fn to_device_point(&self, p: (f32, f32)) -> (f32, f32);
    pub fn to_device_rect(&self, r: RectF) -> RectF;
    pub fn to_vm_point(&self, p: (f32, f32)) -> (f32, f32);
    pub fn raster_scale(&self) -> f32;
    pub fn vm_pixel_region(&self, x: u32, y: u32) -> PixelRegion;
}

pub fn translate_render_scene(scene: &mut RenderScene, translation: &PixelTranslation);

pub enum TranslateError {
    EmptyDimension { vm: (u32, u32), device: (u32, u32) },
}
```

`translate_render_scene` rewrites every node's `dst` and every rectangle clip
into device space. A mask clip and a node's `src` sub-rectangle address
texture pixels, which are already in their own space, so they are left alone.
At an identity mapping the tree is unchanged.

---

## 3. The mapping

* **Scale**: `min(device_w / vm_w, device_h / vm_h)`, computed in `f64` and
  narrowed once, so the whole VM surface fits on both axes.
* **Offset**: `(device - vm · scale) / 2` per axis, so the scaled surface is
  centred and the remaining margin is an even letterbox or pillarbox.
* **Identity** at `scale == 1` and `offset == (0, 0)` — that is, when the VM
  surface covers the framebuffer exactly. The mapping is behaviour-neutral
  there: the projection and the input path reduce to the unscaled path.

`to_vm_point` is the exact inverse of `to_device_point`.

`vm_pixel_region(x, y)` returns the device pixels one VM pixel covers: the
span `[x·s+ox, (x+1)·s+ox)` on each axis, with the start floored and the end
ceiled so the region covers every device pixel the VM pixel touches, at least
one pixel wide and clipped to the framebuffer.

---

## 4. Invariants

* **One scale for both axes.** The mapping is uniform; a non-uniform stretch
  does not exist.
* **The mapping is a pure function of the two sizes.** It cannot drift across
  frames, because it is recomputed from the sizes rather than accumulated.
* **A degenerate size pair has no mapping.** A zero VM dimension or a zero
  device dimension is an error, never a divide-by-zero and never a synthetic
  `1.0` scale.
* **The translation is internal to L2.** L3 must not call `to_device_*`,
  `to_vm_point` or `raster_scale`, and must not name a device pixel; L1 never
  sees the value.
* **Input travels device → VM before the VM tick**, so L2's input state and
  L3's hit-test only ever see VM coordinates.

---

## 5. Data flow

```
                ┌──────────────────────── L3 / scripts (VM pixels) ────────┐
                │  layer rects, bitmap sizes, Font.size, getPixel, hit-test │
                └───────────────────────────────▲──────────────────────────┘
                                                │
   device pixels                        ┌──────┴──────┐
        │                               │  L2 owns    │
        ▼                               │  the mapping │
   L1 framebuffer ─────────────────────►│              │
        ▲                               └──────▲──────┘
        │                                      │
        │                              ┌───────┴────────┐
        └── render::render ◄── translate_render_scene │
                                │                      │
              platform input ───┴──► to_vm_point ──────┘
```

In the engine, the mapping is refitted at three points in the frame: before
the hook (so the input uses the current mapping), after the hook (the hook may
have resized a window's primary layer) and after `after_project`.

---

## 6. Failure modes

* **A zero dimension** in either size is `TranslateError::EmptyDimension`. The
  engine turns it into "no translation": the frame draws nothing and the
  condition is warned about **once** until the sizes fit again. No substitute
  scale is invented.
* **`translate_render_scene` cannot fail.** It is a rewrite over an existing
  tree; the sizes it maps between are the ones the translation was built from.

---

## 7. Current limits

* **Device-pixel readback is not wired.** `getPixel`, `piledCopy` and
  `saveLayerImage` read the CPU layer bitmap, which is VM-exact;
  `vm_pixel_region` has no consumer yet.
* **Glyphs are rasterized at VM resolution**, not at `size · raster_scale()`.
  The visible difference at `s > 1` is softness — a rasterization
  approximation, not a script-observable difference, because every layout
  metric stays in VM units.

---

## 8. Tests

* `crates/engine/tests/translate.rs` — the fit rule, the identity case, the
  point and rectangle mapping in both directions, the VM-pixel region, and
  the degenerate-size error.
* `crates/engine/tests/engine.rs` — the engine's refit behaviour and the
  window-resize handoff.

---

## 9. See also

| topic | document |
|---|---|
| the three coordinate spaces | [`arch/overall.md`](../../arch/overall.md) §3 |
| the presentation rule and the boundary | [`arch/l2.md`](../../arch/l2.md) §4 |
| the projection that is rewritten | [projection.md](projection.md) |
| the frame that refits it | [engine.md](engine.md) |
| the input state it feeds | [input.md](input.md) |
| the archived design trail | [`docs.old/migration/11-presentation-and-pixel-translation.md`](../../../docs.old/migration/11-presentation-and-pixel-translation.md) §6 |
