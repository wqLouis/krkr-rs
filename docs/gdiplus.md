# GdiPlus plugin — implementation spec for krkr-rs

Target: make the KiriKiri2 `GdiPlus` plugin (Windows GDI+ vector drawing,
exposed to scripts as `GdiPlus.Appearance` + `Layer.draw*` extensions) actually
rasterize in the Rust port. Read-only investigation; DEV builds only.

Repo: `/mnt/DATA/Document/code/krkr-rs`
Game scripts: `/tmp/gamescripts/system/*.tjs` (extracted from
`/mnt/DATA/Games/Others/test/data.xp3`).

Reference implementation found in-tree (gold source for semantics):
`reference/cpp/plugins/layerex_draw/windows/LayerExDraw.{hpp,cpp}` (GDI+
build) and `reference/cpp/plugins/layerex_draw/blend2d/LayerExDraw.cpp`
(cross-platform build with a CPU-ish hatch generator). `manual.tjs` documents
the script API.

---

## Executive summary

* The game only ever constructs `GdiPlus.Appearance` and calls
  `addBrush` / `addPen` / `clear`, then passes it to `Layer.drawPolygon` /
  `drawRectangle` / `drawLine` / `drawLines` / `drawArc` / `drawBeziers`.
* `Appearance` is **not** a brush by itself: it is an **ordered list of draw
  infos** (fills and strokes). The reference `Appearance::drawInfos` is a
  `vector<DrawInfo>`; `addBrush` appends a brush (fill), `addPen` appends a pen
  (stroke); `drawPath` iterates the list in order, filling with each brush and
  stroking with each pen. See
  `reference/.../windows/LayerExDraw.cpp:767` (`addBrush`), `:778`
  (`addPen`), `:422` (`clear`), `:1150` (`drawPath`).
* The layer draws are currently in the `noop_stubs` list in
  `crates/tvp-visual/src/natives/layer.rs:1767` and are silently ignored.
* **Critical implementation constraint** (empirically verified, see §0):
  the tjs2 ABI does not carry object handles for object arguments. The current
  `retain_value_detached(Object)` trick resolves only the *last* object
  argument. `drawPolygon(app, points)` has **two** objects and needs both
  (`app` for the brush/pen, `points` for geometry). This must be fixed before
  any real drawing is possible.
* Lowest-risk rasterization target: attach a `BitmapState` to
  `LayerState.bitmap` and rasterize on the CPU, exactly like the already
  working `Layer.drawText` path (`layer.rs:1358`). No new renderer path is
  needed — the Bevy sync already uploads dirty bitmaps.
* The ADV message frame itself is **image-based** (`loadImages("FRM_0101a")`
  … `FRM_0101j`); it does **not** need GdiPlus to look right. GdiPlus only
  affects: text emphasis marks (`drawMark`), the touch resize button, the
  system-menu voice progress bar (debug), the album selection highlights,
  the rain env-effect, and `DrawFrameCurve` used by select items.

---

## 0. Critical constraint: object arguments have no handle in the ABI

`crates/tjs2-sys/cpp/tjs2_abi.cpp:74-81` documents it: *"tjs2_value carries no
object handle, so tjs2_retain_value resolves an OBJECT-typed value against the
engine's most recent object-valued result."* `variant_to_value_one`
(`tjs2_abi.cpp:~230`) sets `e->last_object = var` for every `tvtObject`
argument, so the **last** object argument wins.

Verified with a throwaway probe (engine + two native instance classes, method
`draw(app, ...)` calling `retain_value_detached(Object)` + `get_member("id")`):

```
l.draw(app, 1, 2, 3, 4)          -> retain resolves app,  id = 42   (works)
l.draw(app, [[1,2],[3,4]])       -> retain resolves the points array, "id" missing
var p=[[1,2]]; l.draw(app, p)    -> retain resolves the points array
```

Nested array access does work once you hold the array's id
(`get_member(id,"count")`, `get_member(id,"0")`, then
`retain_value_detached(Object)` again to descend), so the only missing piece is
the handle.

### Recommended minimal ABI change (does **not** change struct size)

Reuse the currently unused `retained` field for `VAL_OBJECT` arguments:

1. In `crates/tjs2-sys/cpp/tjs2_abi.cpp`, `variant_to_value_one`, `case
   tvtObject:` set the object handle in addition to `last_object`:
   ```cpp
   case tvtObject:
       e->last_object = var;
       out->retained = reinterpret_cast<tjs2_value_id>(var.AsObjectNoAddRef());
       out->type = TJS2_VAL_OBJECT;
       break;
   ```
   (`retained` is pointer-sized / `usize` in Rust, written as `nullptr` for
   non-object args, so this is unambiguous for `VAL_OBJECT`.)
2. In `crates/tjs2-sys/src/lib.rs`, add a convenience accessor next to the
   `Value` struct (around line 150):
   ```rust
   impl Value {
       /// Raw TJS object pointer for VAL_OBJECT args (0 for other types).
       pub fn object_handle(&self) -> *mut c_void {
           if self.ty == VAL_OBJECT { self.retained as *mut c_void } else { std::ptr::null_mut() }
       }
   }
   ```
3. Use `Tjs2Engine::retain_object_detached(ptr)` (already exists,
   `lib.rs:840`) when a retained id is needed (e.g. to walk the points array).

Alternative (more explicit, more churn): add a dedicated `void *object` field
to `tjs2_value` / `Value`. That touches ~37 `Value { … }` struct literals; the
`retained` reuse avoids all of them. Recommend the reuse.

No change is needed to `tjs2_dispatch_native_instance_method`: its `objthis`
argument is already the raw `iTJSDispatch2*`
(`tjs2_abi.cpp:~660`) and equals `Value::object_handle()` for the same object,
so a global registry keyed by `objthis` can be looked up directly from
`Layer.draw*` using `args[0].object_handle()`.

---

## 1. Inventory of every `GdiPlus.*` usage

Constants referenced by the game: **only**
`GdiPlus.BrushTypeHatchFill` (= 1) and `GdiPlus.HatchStyleDiagonalBrick`
(= 38), both in `system_advscreen.tjs:3033`. Registered today in
`crates/tvp-natives/src/gdiplus.rs`.

### 1.1 Construction sites (10)

| # | file:line | purpose |
|---|-----------|---------|
| 1 | `system_messageframe.tjs:1925` | `ResizeButton` polygon |
| 2 | `system_messagearea.tjs:857` | `drawMark` (圏点/emphasis marks) |
| 3 | `system_selectitem.tjs:1915` | `VoiceProgressBar` (debug) |
| 4 | `system_advscreen.tjs:3032` | debug "missing image" hatch |
| 5 | `system_album.tjs:125` | `_appG` (registered-cell highlight) |
| 6 | `system_album.tjs:128` | `_appK` (unregistered-cell highlight) |
| 7 | `system_album.tjs:266` | view-menu underline |
| 8 | `system_album.tjs:1690` | trim/zoom frame |
| 9 | `system_enveffect.tjs:456` | `EnvEffectRain` |
| 10 | `system_utility.tjs:1775` | `DrawFrameCurve` |

### 1.2 `addBrush` / `addPen` / `clear` argument forms

Integer color form (GDI+ `ARGB` = `0xAARRGGBB`, passed as a TJS integer;
`RGBA(r,g,b[,a])` produces the same encoding):

```tjs
app.addBrush(0x7ff00000, 0, 0, 0);            // MessageFrame.tjs:1926
app.addBrush(0xff000000, 0, 0, 0);            // MessageArea.tjs:862,877,886
app.addBrush(RGBA(r, g, 64), 0, 0, 0);        // Album.tjs:460
app.addBrush(0x4000007f, 0, 0, 0);            // Album.tjs:1691
app.addBrush(0x40ff0000, 0, 0, 0);            // Album.tjs:1716
app.addBrush(0x7f00007f, 0, 0, 0);            // SelectItem.tjs:1959
app.addBrush(0xff0000ff, 0, 0, 0);            // SelectItem.tjs:1921
app.addBrush(0xff00ff00, 0, 0, 0);            // SelectItem.tjs:1932
app.addBrush(0xffff0000, 0, 0, 0);            // SelectItem.tjs:1944
app.addBrush(colBase, 0, 0);                  // Utility.tjs:1776

app.addPen(0xffffffff, 2, 0, 0);              // width int
app.addPen(0xff000000, 2, 0, 0, 0);           // extra 5th arg ignored
app.addPen(0x80ffffff, 0, 0, 0);              // width 0
app.addPen(RGBA(255,255,255,32+64*(_pos.z/100)), thickness, 0, 0); // EnvEffect.tjs:613
app.addPen(colFrame, %[width:thick, dummy:0], 0, 0);               // Utility.tjs:1777
```

Dictionary brush form (hatch), only in `system_advscreen.tjs:3033`:

```tjs
app.addBrush(%[
    type:        GdiPlus.BrushTypeHatchFill,        // 1
    hatchStyle:  GdiPlus.HatchStyleDiagonalBrick,   // 38
    foreColor:   RGBA(0,0,0),
    backColor:   RGBA(32+random(8),32+random(8),48+random(32))
], 0, 0);
```

Reference defaults for the dictionary form
(`windows/LayerExDraw.cpp:~630`):

* `type` default `BrushTypeSolidColor` (0)
* solid: `color` (default `0xffffffff`)
* hatch: `hatchStyle` (default `HatchStyleHorizontal`=0),
  `foreColor` (default `0xffffffff`), `backColor` (default `0xff000000`)
* (texture/path-gradient/linear-gradient exist but the game never uses them)

`addPen(colorOrBrush, widthOrOption, ox, oy)`:
* `colorOrBrush` integer → solid pen color; `widthOrOption` integer → width.
* `widthOrOption` dictionary → `width` key (game only uses `width`).
* The 4th/5th `ox`/`oy` args are always `0` in the game; index-drawing
  offsets can be ignored.

`clear()` empties the ordered draw-info list (`windows/LayerExDraw.cpp:422`).

### 1.3 Draw calls with an `Appearance` (all of them)

| file:line | call |
|-----------|------|
| `system_messageframe.tjs:1927` | `drawPolygon(app, [[w\2,0],[w,0],[w,h],[0,h],[0,h\2]])` |
| `system_messagearea.tjs:865,871` | `drawArc(app, x+w\2-s\2, y-s-4, s, s, 0, 360)` |
| `system_messagearea.tjs:882,891` | `drawPolygon(app, [[sx,sy],[sx-s,sy+s],[sx+s,sy+s]])` (and flipped) |
| `system_selectitem.tjs:1922` | `drawPolygon(app, [[7,0],[0,5],[14,5]])` |
| `system_selectitem.tjs:1933` | `drawArc(app, 2, 2, 5, 5, 0, 360)` |
| `system_selectitem.tjs:1946-1947` | `drawLine(app, 1,0,1,15)` + `drawPolygon(app, [[1,0],[7,3],[1,6]])` |
| `system_selectitem.tjs:1961` | `drawRectangle(app, x-1, y, w+1, h)` |
| `system_selectitem.tjs:1964,1967` | `drawLine(...)` ×2 |
| `system_selectitem.tjs:2000` | `drawLines(app, sample)` (polyline, ~100 points) |
| `system_album.tjs:268` | `drawLine(app, 2, h-4, w-4, h-4)` |
| `system_album.tjs:462,464` | `drawPolygon(_appG/_appK, quad)` |
| `system_album.tjs:1693,1697-1698` | `drawRectangle(app, 0,0,w-1,h-1)` + 2 × `drawLine` |
| `system_album.tjs:1718,1721-1722` | `drawRectangle(...)` + 2 × `drawLine` |
| `system_enveffect.tjs:618` | `drawLine(app, x-vx, y-vy, x+vx, y+vy)` |
| `system_utility.tjs:1790` | `drawBeziers(app, [16 points])` |
| `system_advscreen.tjs:3034` | `_spr._image.drawRectangle(app, 0, 0, w, h)` (debug) |

`drawString(font, app, x, y, text)` and the other curve/ellipse/pie methods
exist in `manual.tjs` but are **never called by this game**; implement only if
cheap.

---

## 2. `Layer.draw*` semantics, signatures and brush/pen selection

All of these are **Layer instance methods** (the reference plugin extends
`Layer`; `manual.tjs` §"Layer"). Coordinates are **layer-local pixels**.
Signatures (`manual.tjs:686-826`, implementation
`windows/LayerExDraw.cpp:1204-1450`):

| method | signature | rasterization |
|--------|-----------|---------------|
| `drawRectangle` | `(app, x, y, w, h)` | closed rect path; fill with brushes, stroke with pens |
| `drawRectangles` | `(app, rects)` | each `[x,y,w,h]` (unused by game) |
| `drawLine` | `(app, x1,y1,x2,y2)` | open segment; **stroke only** (brushes ignored) |
| `drawLines` | `(app, points)` | open polyline `[[x,y],…]`; **stroke only** |
| `drawPolygon` | `(app, points)` | closed polygon `[[x,y],…]`; fill + stroke |
| `drawArc` | `(app, x,y,w,h,startAngle,sweepAngle)` | elliptical arc, degrees clockwise from +x; fill (implicit close) + stroke |
| `drawBezier` | `(app, x1..y4)` | single cubic; fill/stroke |
| `drawBeziers` | `(app, points)` | `points[0]` = start, then groups of 3 = (c1,c2,end); fill/stroke |
| `drawCurve*` | | cardinal splines (unused) |
| `drawPie`, `drawEllipse` | | (unused) |
| `drawString` | `(font, app, x, y, text)` | text as a path (unused; game uses `drawText`) |

**Brush/pen selection rule** (mirror `drawPath`,
`windows/LayerExDraw.cpp:1150-1200`):

```
for info in appearance.draw_infos (insertion order):
    if info is Brush:  fill the current path with it
    if info is Pen:    stroke the current path with it (width from pen)
```

Because the game always appends in that order, the visible result is
"fill first, then outline". Some appearances have only a pen (after `clear()`),
some have only a brush. `drawLine`/`drawLines` should ignore brushes entirely.

Path builders to mirror (`Add*` are GDI+ `GraphicsPath` operations):
`AddPolygon`, `AddRectangle(RectF(x,y,w,h))`, `AddLine`, `AddLines`,
`AddArc`, `AddBeziers`. Fill mode is the GDI+ default (alternate/even-odd);
game shapes are simple so even-odd is safe.

Value encoding: every color is TJS `ARGB` → convert with the existing
`argb_to_rgba` (`crates/tvp-visual/src/natives/layer.rs:109`). Opacity comes
from the alpha channel.

Suggested rasterizer primitives (new code, can reuse
`blend_pixel(&mut BitmapState, x, y, color, coverage, opa)` at
`layer.rs:1590`):

* `fill_polygon_even_odd(bitmap, &[(f64,f64)], color, aa)` — scanline.
* `stroke_polyline(bitmap, &[(f64,f64)], closed, color, width, aa)` —
  flatten each segment to a quad + round/square joins; width 1–3 in the game.
* `flatten_arc(cx,cy,rx,ry,start,sweep) -> Vec<(f64,f64)>` — sample every
  ~3–5°, close when `|sweep| >= 360`.
* `flatten_cubic(p0,c1,c2,p3) -> Vec<(f64,f64)>`.
* hatch fill: generate an 8×8 (or 16×16) BGRA tile then use it as a
  per-pixel brush sample; for `HatchStyleDiagonalBrick` (38) the blend2d
  reference has no dedicated case and falls through to forward-diagonal
  (`blend2d/LayerExDraw.cpp:637-730`); a diagonal-brick approximation is
  acceptable (debug-only usage).

Anti-aliasing: GDI+ `SmoothingModeAntiAlias` is the reference default; compute
edge coverage (supersample 4× or signed-distance) and pass to `blend_pixel`.

---

## 3. Integration plan in krkr-rs: where the pixels live

### Recommendation: CPU-rasterize into an attached `BitmapState` (Option A)

Attach/allocate a bitmap exactly like `Layer.drawText` already does
(`crates/tvp-visual/src/natives/layer.rs:1358-1450`, allocation block at
`:1396-1409`):

1. Extract the allocation logic from `layer_draw_text` into a reusable
   `fn ensure_layer_bitmap(scene: &mut Scene, layer_id: u32, min_w: u32, min_h: u32) -> u32`
   that:
   * returns `layer.bitmap` if present;
   * otherwise allocates `add_bitmap(layer.rect.w.max(min_w).max(1),
     layer.rect.h.max(min_h).max(1), vec![0; w*h*4])`, sets
     `layer.bitmap = Some(id)` and grows `layer.rect` as `drawText` does.
2. Implement each `draw*` native in `layer.rs`:
   * parse `app` handle (`args[0].object_handle()`), look up the appearance
     registry (see §4);
   * parse geometry from `args`; for point-arrays use
     `retain_object_detached(args[1].object_handle())` then
     `get_member(id,"count")` / `get_member(id,"i")` (verified to work);
   * `ensure_layer_bitmap`, then rasterize into `scene.bitmap_mut(id)`;
   * `bitmap.mark_dirty()`.
3. Remove the implemented names from the `noop_stubs` list
   (`layer.rs:1752-1768`) and add real handlers to the `methods` vec next to
   `drawText` (`layer.rs:1907`).

Why not a vector-shape list rendered by `crates/render/src/sync.rs`:

* The renderer already uploads any dirty `BitmapState`
  (`render/src/sync.rs:98-140`, `:357-362`) and composites layers as sprites;
  a new mesh/tessellation path would duplicate blending/opacity handling.
* GDI+ features used here (hatch brushes, thick anti-aliased strokes, arcs)
  are far easier to match on the CPU with the existing `blend_pixel`
  straight-alpha convention.
* `drawText` established this exact pattern and is proven.
* The only cost is CPU rasterization per draw call, which for this game is
  small (a few shapes per UI element, no per-frame redraw in steady state).

Optional later optimization: keep a vector list on `LayerState` and only
tessellate in the renderer; not worth the risk now.

Files:
* `crates/tvp-visual/src/natives/layer.rs` — new handlers + extracted helper.
* `crates/tvp-visual/src/natives/raster.rs` (new) — shape rasterizer.
* `crates/tvp-visual/src/scene.rs` — **no structural change** (`BitmapState`
  already has `rgba` + `dirty`).

---

## 4. `Appearance` native shape and cross-crate access

### 4.1 State to store

Mirror the reference `Appearance::DrawInfo` ordered list
(`windows/LayerExDraw.hpp:100-160`):

```rust
#[derive(Clone)]
pub(crate) enum BrushKind {
    Solid([u8; 4]),                                          // straight-alpha RGBA
    Hatch { style: i32, fore: [u8; 4], back: [u8; 4] },
    // texture/path/linear gradients: not needed by the game
}
#[derive(Clone)]
pub(crate) enum DrawKind {
    Brush(BrushKind),                                        // fill
    Pen { brush: BrushKind, width: f64 },                    // stroke
}
#[derive(Default, Clone)]
pub(crate) struct AppearanceState {
    pub infos: Vec<DrawKind>,                                // insertion order
}
```

Methods:
* `addBrush(colorOrBrush, ox, oy[, ...])` — if arg0 is an integer →
  `Solid(argb)`; if a dictionary → read `type` (default 0); `type==1` → read
  `hatchStyle`/`foreColor`/`backColor` (defaults as in §1.2); append
  `DrawKind::Brush`. Extra args tolerated.
* `addPen(colorOrBrush, widthOrOption, ox, oy[, ...])` — arg0 integer → solid
  color; arg1 integer → width, dictionary → `width` key (default 1.0); append
  `DrawKind::Pen`.
* `clear()` — `infos.clear()`.
* Constants `BrushTypeHatchFill=1`, `HatchStyleDiagonalBrick=38` (already
  correct in `crates/tvp-natives/src/gdiplus.rs`).

### 4.2 Registry and the crate-direction problem

`Appearance` is registered in `tvp-natives`, but `Layer.draw*` lives in
`tvp-visual`. Cargo graph (verified):
`tvp-natives → tjs2-sys` only; `tvp-visual → tjs2-sys, engine, tvp-text`;
`render → tvp-natives + tvp-visual`. Neither depends on the other, so no Rust
type can be shared today.

**Recommended: co-locate `GdiPlus` with the visual natives (move the plugin
into `tvp-visual`).** This mirrors the reference (the plugin extends `Layer`
and owns `Appearance`), needs no new crate and no inverted dependency, and
makes the appearance state a `pub(crate)` type in the same crate as the
rasterizer.

Concrete move:
1. Move `crates/tvp-natives/src/gdiplus.rs` →
   `crates/tvp-visual/src/natives/gdiplus.rs`, adding the payload fields above.
2. Register it from `register_visual` in
   `crates/tvp-visual/src/natives/mod.rs:243-247` (add
   `gdiplus::register_gdiplus(engine)?;`).
3. Remove `mod gdiplus;` / `pub use gdiplus::register_gdiplus;` /
   `register_gdiplus(engine)?` from `crates/tvp-natives/src/lib.rs`
   (`:36`, `:49`, `:133`). Move the `#[cfg(test)]` block too.
4. Registry (process-global, VM is single-threaded):
   ```rust
   static APPEARANCES: LazyLock<Mutex<HashMap<usize, AppearanceState>>> = ...;
   ```
   * Key = `objthis as usize` (the raw `iTJSDispatch2*`). The
     `addBrush`/`addPen`/`clear` callbacks receive `objthis` as their 7th
     parameter, so they can lazily insert/update `APPEARANCES[objthis]`.
   * `AppearanceInst { objthis: AtomicUsize, state: ... }`; store `objthis`
     on first method call and remove the registry entry in the destroy
     callback to avoid leaks. (Simplest acceptable variant: leave entries and
     rely on the bounded number of appearances.)
5. `Layer.draw*` reads the app with `args[0].object_handle() as usize` and
   looks up `APPEARANCES`. No VM round-trip, no `get_member` needed for the
   brush/pen state.

Alternative if moving crates is undesirable: create a tiny `tvp-gdiplus`
crate holding only `AppearanceState`/`BrushKind` + the registry, depended on
by both `tvp-natives` and `tvp-visual`. This adds a workspace member but keeps
`register_gdiplus` in `tvp-natives`. The co-location move is preferred
because it matches the reference architecture and avoids a new crate.

(An option that needs **no** shared Rust type — keep `Appearance` in
`tvp-natives`, expose `brushType`/`brushColor`/… as native instance properties,
and have `Layer.draw*` read them via retained-id + `get_member` — is possible
but awkward: it cannot represent multiple ordered draw infos cleanly, and it
still requires the §0 ABI fix. Not recommended.)

---

## 5. What is actually needed for the ADV message frame to LOOK right

Read `system_messageframe.tjs` around the Appearance usage and the frame
construction. Findings:

* **The frame is image-based, not GdiPlus.** `createMessage()` loads the frame
  bitmap with `loadImages(FRAMELIST[id].base)` where the bases are
  `FRM_0101a`…`FRM_0101j` (`system_messageframe.tjs:86-96`, `:342-362`). It is
  attached to `_msgBase[id].inner` (an `ActivateLayer`). The "nobel" mode is a
  plain `fillRect(0,0,w,h,0xcf000000)` (`system_messageframe.tjs:382-392`).
* The name/message text is drawn by `MessageArea` via the native `drawText`
  (already implemented) at `system_messagearea.tjs:356` and `:830`.
* The **only** `GdiPlus` usage in the message frame is the
  `ResizeButton` constructor (`system_messageframe.tjs:1916-1930`), which
  belongs to `TouchPanelControl`. The touch panel is constructed
  (`system_messageframe.tjs:176`) but its `visible = true` is **commented out**
  (`:1861`), so it does not affect normal play.
* The system menu frame is also image-based (`_frame.loadImages("FRM_0121")`,
  `system_messageframe.tjs:1120-1126`).
* There is **no `onPaint` / `drawMessageFrame` function** in
  `system_messageframe.tjs`; rendering is declarative.
* `drawMark` (`system_messagearea.tjs:856-896`, called at `:358` when
  `_markType && _fEnableMark`) is the GdiPlus feature that can affect normal
  ADV text: it draws 圏点 (emphasis dots/circles/triangles) over characters.
  It uses `drawArc` (fill+stroke or stroke only) and `drawPolygon`
  (fill+stroke). `_fEnableMark` defaults true; `system_systemwindow.tjs:2123`
  disables it in one place.

**Conclusion:** the frame, name and message text already look right from the
image/`drawText` paths. GdiPlus is only needed for emphasis marks, the
touch-only resize button, and the other UI (album/rain/select-item) — not for
the frame background.

### Resize button vs. frame

`ResizeButton` is a 16×16 `Layer` that calls
`drawPolygon(app, [[8,0],[16,0],[16,16],[0,16],[0,8]])` with a single
`addBrush(0x7ff00000)` (translucent red) → a filled polygon. Implementing
`drawPolygon` with a solid brush covers it. It is hidden unless a touch panel
enables it, so it is lower priority than `drawMark`.

---

## 6. Prioritized, minimal task list

Ordered by user-visible value per unit of risk.

0. **ABI: object handles for object args** (§0). Prerequisite for everything.
   * `crates/tjs2-sys/cpp/tjs2_abi.cpp` `variant_to_value_one` (~line 230):
     set `out->retained` to `AsObjectNoAddRef()` for `tvtObject`.
   * `crates/tjs2-sys/src/lib.rs`: `Value::object_handle()` helper.
   * Add a probe/unit test for `l.draw(app, [[...]])` resolving both handles.
   * Risk: low; no struct-layout change.

1. **Move `GdiPlus` into `tvp-visual` and give it real state** (§4).
   * Move `gdiplus.rs`, register from `register_visual`, remove the
     `tvp-natives` registration; add the `AppearanceState` registry keyed by
     `objthis`; implement `addBrush`/`addPen`/`clear`.
   * Highest value: unblocks all real drawing and fixes the album `_appG` /
     `_appK` distinction (per-object state, not a global "current" brush).
   * Risk: medium (crate move + test move), but no new crate.

2. **`Layer.drawPolygon` + `drawRectangle` + `drawLine` + `drawLines`**
   (§2, §3) — solid brush fill + solid pen stroke + polyline.
   * Covers: emphasis marks (`drawMark` triangles/circles), resize button,
     album highlights/trim frames, rain streaks, `DrawFrameCurve` outlines
     (once beziers land), voice-progress bar (debug).
   * Add `ensure_layer_bitmap` (extract from `layer_draw_text`), a new
     `raster.rs`, register handlers, drop them from `noop_stubs`.
   * Highest user-visible value for normal play is `drawArc`+`drawPolygon`
     for `drawMark`; `drawLine`/`drawLines` cover rain/select-item.

3. **`Layer.drawArc` and `Layer.drawBeziers`** (§2).
   * `drawArc` is required by `drawMark` (dots/circles) and the voice bar.
   * `drawBeziers` is required by `DrawFrameCurve`
     (`system_utility.tjs:1790`) used by select-item sample buttons
     (`system_selectitem.tjs:784-789`).

4. **Anti-aliasing + stroke joins/caps** to match GDI+ `SmoothingModeAntiAlias`
   and make small marks look clean. Can start with 1-bit fills and refine.

5. **Hatch brush (`BrushTypeHatchFill` / `HatchStyleDiagonalBrick`)** (§1.2).
   * Debug-only "missing image" placeholder (`system_advscreen.tjs:3032`).
   * Lowest value; a diagonal approximation is fine. Implement a tile
     generator + sampling last, or stub it as a solid `backColor` fill plus
     `foreColor` diagonals.

6. **`drawString`, `drawCurve*`, `drawPie`, `drawEllipse`, texture/gradient
   brushes** — not used by this game; skip unless a later title needs them.

### Suggested verification

* Unit test in `tvp-visual` mirroring the existing
  `layer_draw_text_rasterizes_into_scene_bitmap`
  (`layer.rs:2331`): construct a layer, an appearance, call
  `drawPolygon`/`drawArc`, assert non-transparent pixels and the layer has a
  bitmap.
* Integration: run the game DEV build, open the ADV scene, enable emphasis
  marks (or trigger rain / open the album) and confirm the shapes appear.
* Regression: the `tvp-natives` GdiPlus tests must move with the module; keep
  a test that `new GdiPlus.Appearance()` + the two constants still resolve
  after the registration move.

---

## Appendix A — files to touch

| file | change |
|------|--------|
| `crates/tjs2-sys/cpp/tjs2_abi.cpp` | set `out->retained` object handle for `tvtObject` args |
| `crates/tjs2-sys/src/lib.rs` | `Value::object_handle()` helper |
| `crates/tvp-natives/src/lib.rs` | remove gdiplus module/registration (`:36`,`:49`,`:133`) |
| `crates/tvp-natives/src/gdiplus.rs` | **move** to tvp-visual |
| `crates/tvp-visual/src/natives/gdiplus.rs` | new real Appearance + registry |
| `crates/tvp-visual/src/natives/mod.rs` | register gdiplus (`:243-247`) |
| `crates/tvp-visual/src/natives/layer.rs` | real draw handlers, `ensure_layer_bitmap`, drop stubs (`:1752-1768`, `:1907`) |
| `crates/tvp-visual/src/natives/raster.rs` | new CPU shape rasterizer |
| `crates/tvp-visual/src/scene.rs` | none (BitmapState sufficient) |
| `crates/render/src/sync.rs` | none (dirty-bitmap upload already works) |

## Appendix B — existing code anchors

* `crates/tvp-natives/src/gdiplus.rs` — current no-op stub (constants already
  correct).
* `crates/tvp-visual/src/natives/layer.rs:109` `argb_to_rgba`.
* `crates/tvp-visual/src/natives/layer.rs:1358` `layer_draw_text` (bitmap
  allocation pattern to extract).
* `crates/tvp-visual/src/natives/layer.rs:1590` `blend_pixel`.
* `crates/tvp-visual/src/natives/layer.rs:1767` `noop_stubs`.
* `crates/tvp-visual/src/natives/layer.rs:1950` stub registration loop.
* `crates/tvp-visual/src/scene.rs:90` `BitmapState` (`rgba`, `dirty`).
* `crates/render/src/sync.rs:98` per-bitmap texture upload.
* `reference/cpp/plugins/layerex_draw/windows/LayerExDraw.cpp:422/767/778/1150/1204-1450`
  — Appearance + draw semantics.
* `reference/cpp/plugins/layerex_draw/manual.tjs:686-826` — API signatures.
```
