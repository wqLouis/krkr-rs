# `engine::raster` — the CPU shape rasterizer

Source: `crates/engine/src/raster.rs`

The L1 pipeline uploads any dirty `BitmapState`, so the vector drawing
operations rasterize shapes **directly into a layer's attached bitmap**. This
module keeps that pixel work separate from the layer-state operations in
[`layer_ops`](layer_ops.md).

Coordinates are layer-local pixels with the scene's **y-down** convention.
Anti-aliasing is a 4×4 supersample of the even-odd fill; strokes use the
per-pixel distance to the segment for a round, anti-aliased band. All colours
are straight-alpha RGBA and are composited with `blend_pixel`, so the result
matches the rest of the middle layer.

---

## 1. Responsibility

* Rasterize polygons, polylines, hatches, arcs and béziers into a bitmap.
* Provide the shared per-pixel compositing primitive.
* Provide the reference `fillRect` semantics (a colour **copy**, not a
  blend).

---

## 2. State it owns

None. Every function takes the target `&mut BitmapState` (or a pixel slice)
and works on it in place. There is no geometry cache, no scratch buffer that
outlives a call and no global state.

---

## 3. Public API

```rust
pub fn blend_pixel(bitmap: &mut BitmapState, x: i32, y: i32, color: [u8; 4], coverage: u8, opa: u8);
pub fn blend_pixel_at(pixel: &mut [u8; 4], color: [u8; 4], coverage: u8, opa: u8);
pub fn fill_rect_replace(bitmap: &mut BitmapState, x: i32, y: i32, w: u32, h: u32, color: [u8; 4]);
pub fn fill_polygon(bitmap: &mut BitmapState, pts: &[(f64, f64)], color: [u8; 4]);
pub fn fill_polygon_hatch(bitmap: &mut BitmapState, pts: &[(f64, f64)], style: i32,
                          fore: [u8; 4], back: [u8; 4]);
pub fn hatch_foreground(style: i32, x: i32, y: i32) -> bool;
pub fn stroke_polyline(bitmap: &mut BitmapState, pts: &[(f64, f64)], closed: bool,
                       color: [u8; 4], width: f64);
pub fn bbox_size(pts: &[(f64, f64)]) -> (u32, u32);
pub fn flatten_arc(x: f64, y: f64, w: f64, h: f64, start_deg: f64, sweep_deg: f64) -> Vec<(f64, f64)>;
pub fn flatten_cubic(p0: (f64, f64), c1: (f64, f64), c2: (f64, f64), p3: (f64, f64),
                     steps: usize) -> Vec<(f64, f64)>;
```

| function | operation |
|---|---|
| `blend_pixel` | alpha-composite one straight-alpha pixel into a bitmap, with a bounds check |
| `blend_pixel_at` | the same, on a pixel the caller owns (no bounds check) |
| `fill_rect_replace` | the reference `fillRect`: the ARGB value **becomes** the pixel, including its alpha |
| `fill_polygon` | fill a closed polygon with a flat colour (even-odd rule, 4×4 supersampled) |
| `fill_polygon_hatch` | fill a closed polygon with a two-colour hatch approximation |
| `hatch_foreground` | whether a pixel is foreground for a GDI+ hatch style |
| `stroke_polyline` | stroke an open or closed polyline with a round, anti-aliased band |
| `bbox_size` | the layer-wide allocation size needed to contain an open polyline |
| `flatten_arc` | flatten an elliptical arc into a polyline |
| `flatten_cubic` | flatten a cubic bézier into `steps + 1` points |

---

## 4. Invariants

* **Straight alpha.** `blend_pixel_at` keeps the scene buffer straight-alpha:
  over a transparent destination the RGB stays the requested colour rather
  than the colour multiplied by the coverage.
* **The alpha formula is `color.a × coverage × opa / (255 × 255)`**, so a
  fully transparent source or a zero coverage writes nothing at all.
* **Polygon fill uses the even-odd (alternate) rule** with 16 samples per
  pixel (a 4×4 grid); the coverage is `hits × 255 / 16`.
* **An open path is closed implicitly for filling**, matching the reference's
  `FillPath`.
* **The hatch tile is 8×8** and repeats; the diagonal cases are reduced into
  the cell so the sums cannot overflow on extreme coordinates.
* **The stroke band is round**, not flat: joins and caps are round. The
  widths the game uses are 1–3 pixels, where the difference is invisible.
* **`fill_rect_replace` copies the alpha too**, which is what lets a
  `fillRect(…, RGB(0,0,0,0))` clear an existing image. It never consults or
  changes the layer's position.
* **Arc angles are degrees**, `0` on the +x axis, increasing clockwise, in
  the y-down convention.

---

## 5. Data flow

```
L3 native (GdiPlus draw call)
   │  geometry in layer-local pixels
   ▼
raster::fill_polygon(&mut scene.bitmap_mut(id)?, pts, color)
   │
   ├─► bounds → clamp to the bitmap → per-pixel coverage
   ├─► blend_pixel → straight-alpha RGBA write
   └─► the caller marks the bitmap dirty (the rasterizer only writes pixels)
              │
              ▼
      engine::Engine::sync_dirty_bitmaps → upload_textures
```

---

## 6. Failure modes

* **The primitives are infallible.** A shape that lies outside the bitmap, a
  polygon with fewer than three points, a polyline with fewer than two
  points, or a fully transparent colour produce no write — they are
  no-ops, not errors.
* **Non-finite geometry is rejected.** `flatten_arc` returns an empty
  polyline for a non-finite box or angle instead of iterating an unbounded
  step count.
* **A degenerate fill region is skipped.** The bounds helper returns an empty
  box and the loop does not run.

---

## 7. Current limits

* **The hatch `Percent50` style paints 25%**, where the reference's
  checkerboard paints 50%.
* **The stroke joins and caps are round**, not the reference's flat GDI+
  joins; the widths the game uses are 1–3 pixels, where the difference is
  invisible.

---

## 8. Tests

* `crates/engine/tests/raster.rs` — the polygon fill, the hatch styles, the
  stroke band and the flat-fill semantics.
* `crates/engine/src/raster.rs` `mod tests` — the hatch-pattern unit tests.

---

## 9. See also

| topic | document |
|---|---|
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the pixel model shared with `layer_ops` | [layer_ops.md](layer_ops.md) |
| the bitmap state these write into | [scene.md](scene.md) |
| the archived protocol trail | [`docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) §6.9 |
