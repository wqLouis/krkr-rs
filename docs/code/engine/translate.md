# `engine::translate` — the VM-pixel ↔ device-pixel mapping

Source: `crates/engine/src/translate.rs`

Scripts and the layer tree describe the scene in **VM pixels** — the size the
game declared for its window. The framebuffer is in **device pixels**, which
on most displays is a different size. `PixelTranslation` is the single
mapping between the two spaces: a uniform scale of `min(device / vm)` per
axis plus a centred offset, so the whole game surface is visible and the
leftover margin stays clear.

**Responsibility.** Hold the Fit-with-letterbox mapping for one pair of
surface sizes; provide the root transform that maps the projected VM tree into
device space and its inverse for the device-space overlay; map input **back**
into VM space for the composition root; describe the device region one VM
pixel covers, for the readback path.

It is the only layer that names both spaces. L3 never sees a device
coordinate; L1 receives the VM-space tree with an opaque root transform but
never interprets the VM semantics. The rule and the boundary are stated in
[`arch/l2.md`](../../arch/l2.md) §4 and
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
    pub fn root_transform(&self) -> Transform2D;
    pub fn inverse_root_transform(&self) -> Transform2D;
}

pub enum TranslateError {
    EmptyDimension { vm: (u32, u32), device: (u32, u32) },
}
```

`root_transform` is the Fit mapping written as a node-local `Transform2D`:
uniform scale, then the centred offset. The engine puts it on the render-scene
root. The projected layer nodes keep their VM `dst` and rectangle clips; L1
composes the root during traversal. `inverse_root_transform` cancels it for a
node authored in device pixels, such as the render-only overlay.

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
  `to_vm_point` or `raster_scale`, and must not name a device pixel; L1
  receives the mapping only as the root transform of the VM-space tree.
* **Input travels device → VM before the VM tick**, so L2's input state and
  L3's hit-test only ever see VM coordinates.
* **The root transform is the only render-space mapping.** No projected node
  is rewritten; the overlay cancels the root with its inverse.

---

## 5. Data flow

```
                ┌──────────────────────── L3 / scripts (VM pixels) ────────┐
                │  layer rects, bitmap sizes, Font.size, getPixel, hit-test │
                └───────────────────────────────▲──────────────────────────┘
                                                │
                                                │
   projected RenderScene ──► render::render ──► L1 framebuffer
   (VM tree, root.transform = Fit)
                                                ▲
                                                │
              platform input ──► to_vm_point ───┘
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
* **The root transform cannot fail.** It is a pure scale/offset over an
  already valid mapping; the sizes it maps between are the ones the
  translation was built from.

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
  point and rectangle mapping in both directions, the VM-pixel region, the
  root transform and its inverse, and the degenerate-size error.
* `crates/engine/tests/engine.rs` — the engine's refit behaviour, the root
  transform at scale/letterbox, the device-space overlay and the
  window-resize handoff.

---

## 9. See also

| topic | document |
|---|---|
| the three coordinate spaces | [`arch/overall.md`](../../arch/overall.md) §3 |
| the presentation rule and the boundary | [`arch/l2.md`](../../arch/l2.md) §4 |
| the projection that carries the root | [projection.md](projection.md) |
| the frame that refits it | [engine.md](engine.md) |
| the input state it feeds | [input.md](input.md) |
| the archived design trail | [`docs.old/migration/11-presentation-and-pixel-translation.md`](../../../docs.old/migration/11-presentation-and-pixel-translation.md) §6 |
