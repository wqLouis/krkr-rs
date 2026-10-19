# `layerExDraw` plugin — hidden-handle, marshalling and lifetime audit

- Repo: `$KRKR_REPO`
- Branch: `main`
- Date: 2026-10-13
- Scope: `crates/compat/src/natives/plugins/layer_ex/**` (`mod`, `draw`,
  `appearance`, `font`, `gdiplus`, `geom`, `image`, `matrix`, `movie`, `path`,
  `perspective`, `record`) against
  `reference/cpp/plugins/layerex_draw/{windows,blend2d}/LayerExDraw.{cpp,hpp}`,
  `reference/cpp/plugins/layerex_draw/windows/main.cpp`,
  `reference/cpp/plugins/layerExMovie.cpp`,
  `reference/cpp/plugins/layerExPerspective.cpp`,
  `reference/cpp/plugins/layerExBase.hpp`, and
  `reference/cpp/core/tjs2/tjsVariant.{h,cpp}` /
  `reference/cpp/core/plugin/ncbind.hpp` for the conversion semantics.
- **This is an audit; the only file written is this one.** No `cargo`/`rustc`/
  `clippy` command was run (AGENTS §0). One already-built test binary was run
  for evidence (`target/debug/deps/layer_ex-9dd5492238b216ec`).

House rules referenced: AGENTS §4 (no silent fallback), §5 (house style), §7
(genericity), §9 (VM traps), D9, D11, D26, D29, D31, D32, D34.

---

## 1. Executive summary

One plugin defect has already escaped to a device (the exemplar). The audit
finds that it is **not isolated**: the same fragile handle mechanism is used by
`Appearance` **and** `Path`, it is inconsistent with the class-chain
native-instance lookup that `Matrix`/`Image`/`Layer`/`Bitmap` already use in the
same plugin, and its shared error string names the wrong type. Beyond the
handle class there are several real D34 divergences in argument marshalling, one
font-cache staleness bug that is script-observable, and two lifetime/leak
issues that are low-severity but structural.

Ranked by "would break a real game":

| rank | finding | pattern | script-observable | evidence |
|---|---|---|---|---|
| 2 | **F3** `GdiPlus.addPrivateFont` never invalidates the shared `faces` cache, and `resolve_face` caches fallbacks under the requested name, so a later-added private face is unreachable | 6 | yes (metrics/drawing) | `font.rs:600-635`, `resource.rs:250-289` |
| 3 | **F4** `read_font` reads public properties, so a plain dictionary is accepted as a `FontInfo` where the reference requires a native instance | 1/3 | yes (no throw) | `font.rs:80-104` |
| 3 | **F5** Scalar `Layer.draw*` coordinates use `value_as_f64`, which maps objects/`null` to `0.0` where `AsReal` raises | 3 | yes | `draw.rs:793-923`, `mod.rs:538-552` |
| 3 | **F6** `Layer.drawCurve*` and several point-array methods do not enforce the reference's full NCB arity; omitted scalars silently default | 3 | yes | `draw.rs:924-978`, `draw.rs:756-783` |
| 3 | **F7** `read_points`/`read_rects` reject non-object elements and non-array objects where the reference yields `PointF()/empty` | 3 | yes | `mod.rs:425-495`, `windows/main.cpp:123` |
| 3 | **F10** `Path.draw*` report a fixed `"invalid points"/"invalid rectangles"` and discard the underlying error | 2 | yes (message) | `path.rs:555,583,611,637,680-681` |
| 3 | **F13** `Layer.setTransform`/`setViewTransform` cannot read a native `GdiPlus.Matrix` (`read_matrix` has no native-payload branch; `Matrix` exposes no `m11`/`dx`) | 3 | yes (throws) | `draw.rs:708-750`, `matrix.rs:139-163` |
| 4 | **F8** Per-layer `DrawState` and `LayerExMovie` slots are never cleaned on layer destruction; movie slots retain the script `Layer` object | 4/6 | no (leak) | `draw.rs:179-183,596`, `movie.rs:97-110,261-279` |
| 4 | **F9** `RectF.Union` is registered as an instance method with different semantics from the reference's static | 5 | yes (member set) | `geom.rs:484-560,793` |
| 4 | **F11** `appearance_ref`/`path_ref` return `&'a` with an unbounded lifetime; currently safe only because every caller uses the borrow synchronously | 4/6 | no (latent) | `draw.rs:640-658` |
| 4 | **F12** `drawCurve3`'s `offset`/`numberOfSegments` use `value_as_usize`; a negative value becomes `0` instead of the reference's `AsInteger` + range guard | 3 | yes (edge) | `draw.rs:943-954`, `path.rs` `cardinal_spline` |

Nothing in `record.rs`, `image.rs`, `perspective.rs`, `gdiplus.rs`, `geom.rs`
or `matrix.rs` rises to rank 1-2. `matrix.rs` is the **positive control**: it
already resolves a `Matrix` argument through
`native_instance_payload(..., "Matrix")` (`matrix.rs:154-163`), the exact
class-chain mechanism the reference uses.

---

## 2. Method and coverage

Every table below was checked by reading the Rust and the reference side by
side; the coverage ledger is §8. The behavioural assertion below is the
exemplar from the device log (`krkr-rs-logs (2).txt:690,769`):

```
MessageFrame.tjs(1927): Layer.drawPolygon: the argument is not a LayerExDraw value type
```

The game source (`/tmp/systjs/system_MessageFrame.tjs`, the extracted
`system_MessageFrame.tjs`) is:

```tjs
class ResizeButton extends Layer{
    function ResizeButton(win, par){
        Layer(win, par);
        setSize(16, 16);
        var app = new GdiPlus.Appearance();
        app.addBrush(0x7ff00000, 0, 0, 0);
        drawPolygon(app, [[width\2, 0], ... ]);
        invalidate app;
    }
}
```

The equivalent modern-style test
(`crates/compat/tests/layer_ex.rs:449-465`) runs
`dst.drawPolygon(app, [[0,0],[4,0],[2,4]])` on a `new Layer(w, null)` and
passes (re-run here against the built `layer_ex` binary: `layer_draws_other_shapes ... ok`).
The only structural differences in the game are (a) the receiver is a **script
subclass of `Layer` with a legacy-style constructor** and (b) the call is
unqualified inside that constructor. That is the coverage gap: **no test in
`crates/compat/tests/layer_ex.rs` or `layer_ex_behaviour.rs` creates a
`class X extends Layer { function X(win,par){ Layer(win,par); ... } }` and
draws on it**, so the legacy-constructor/subclass receiver path is untested.

---

## 3. Pattern 1 — class-chain lookups

### F4 (rank 3) — `read_font` bypasses the native instance entirely

`read_font` (`font.rs:80-104`) reads the public members `familyName`, `emSize`,
`style`, `forceSelfPathDraw` off the argument, with defaults. It never checks
that the argument is a `GdiPlus.Font` native instance. The reference
`drawString(const FontInfo *font, ...)` (`windows/LayerExDraw.hpp:545`) and
`measureString(const FontInfo *font, ...)` receive a native instance and throw
on a non-instance. Consequence: `dst.drawString(%[familyName: 'Arial',
emSize: 16], app, 0, 0, "x")` succeeds in the port and throws in the reference.
`GdiPlus.Font` subclasses and getters are fine (member reads work), so the only
divergence is the accepted dictionary. **Script-observable:** yes (D34).
**Fix direction:** require `native_instance_payload(..., "FontInfo")` (the
internal class name, `gdiplus.rs:325-326`) and read the payload fields.

**The rest of pattern 1 is already class-chain correct** and was checked:
`layer_from_objthis` (`mod.rs:102-108`), `image_snapshot`
(`draw.rs:1058-1078`) for `Layer`/`Bitmap`, `get_record_image`/`draw_image*`
(`draw.rs:1058-1078,2060-2090`), `perspective.rs:230-247`, `matrix.rs:139-163`,
`image.rs:150-164`.

---

## 4. Pattern 2 — misleading fixed error messages

### F10 (rank 3) — `Path.draw*` fixed `"invalid points"` / `"invalid rectangles"`

`path.rs:555,583,611,637,680-681` collapse every `read_points`/`read_rects`
error to `"Path.<method>: invalid points"` (and `"Path curve: invalid points"`),
losing which element was malformed. Script-observable (message only). **Fix
direction:** keep the method prefix but append the underlying error, as
`draw.rs`'s `path_draw_method!` closures already do.

`read_number_member`/`read_point_object`/`read_rect_object`
(`mod.rs:283-350`) do map the underlying member error to a contextual
`"{what}: the object is not a point/rectangle"`, which is acceptable; the
`Path.draw*` sites above are the outlier.

---

## 5. Pattern 3 — argument marshalling that rejects/accepts what the reference does not

### F5 (rank 3) — scalar coordinates coerce objects instead of raising

The scalar `path_draw_method!` bodies (`draw.rs:793-923`: `drawArc`, `drawPie`,
`drawBezier`, `drawEllipse`, `drawLine`, `drawRectangle`) call
`value_as_f64(&a[n])` directly. `value_as_f64` (`mod.rs:538-552`) returns `0.0`
for `VAL_OBJECT`/`VAL_NULL`/octet. The reference parameter is `REAL`, converted
with `AsReal`, which raises `TJSThrowVariantConvertError` on an object/octet
(`reference/cpp/core/tjs2/tjsVariant.h:947-966`, cited in the port's own
`strict_real` at `draw.rs`/`geom.rs`). So `dst.drawLine(app, {}, 0, 4, 0)`
draws a degenerate line in the port and throws in the reference. The same
pattern exists in the transform setters (`draw.rs:1771-1772,1886-1887`,
`rotateTransform`/`scaleTransform`/`translateTransform`/view variants) which use
`value_as_f64` where the reference uses `REAL`. **Script-observable:** yes.
**Fix direction:** route these through the existing `strict_real` (already
defined in `appearance.rs`, `font.rs`, `geom.rs`, `matrix.rs`, `path.rs`,
`perspective.rs`; `draw.rs` lacks a local copy).

Note the TJS "string → real" prefix semantics are already correct in
`value_as_f64` (it calls `tjs_string_to_real`, `mod.rs:544`), so the
error-handling audit's `str::parse` concern does not apply here.

### F6 (rank 3) — point-array methods omit the full NCB arity

`path_draw_method!` (`draw.rs:756-783`) only requires the Appearance. The
`drawCurve`/`drawCurve2`/`drawCurve3`/`drawClosedCurve2` bodies then default
the trailing scalars:
`draw.rs:939` (`tension` default `0.5`), `:951` (`tension` default `0.5`),
`:949-950` (`offset`/`segments` default `0`). The
reference declares `drawCurve2(app, points, REAL tension)` and
`drawCurve3(app, points, int offset, int numberOfSegments, REAL tension)`
(`windows/LayerExDraw.hpp:437,448`) and ncbind rejects a short call
(`reference/cpp/core/plugin/ncbind.hpp:1178`). So
`dst.drawCurve2(app, points)` / `dst.drawCurve3(app, points)` succeed in the
port and throw in the reference. The port's own `layer_ex_behaviour.rs`
`path_primitives_require_their_declared_arity` tests the **Path** class arities
but nothing tests the `Layer.drawCurve*` arities. **Script-observable:** yes.
**Fix direction:** enforce the reference parameter counts (`drawCurve` 2,
`drawCurve2` 3, `drawCurve3` 5 including the appearance) in each body.

### F12 (rank 4) — `drawCurve3` negative offset/segments

`draw.rs:949-950` uses `a.get(2).and_then(value_as_usize).unwrap_or(0)`, so a
negative `offset`/`segments` becomes `0`. The reference converts with
`AsInteger` and its range guard treats `offset < 0` as a no-op
(`blend2d/LayerExDraw.cpp:1277-1287`, and the port already reproduces that guard
in `path_curve_method!`). A game passing a negative offset would draw a curve
where the reference draws nothing. **Script-observable:** yes (edge).
**Fix direction:** read with `try_value_as_i64` and reproduce the guard, as
`path_curve_method!` does.

### F7 (rank 3) — point/rect element strictness

`read_points`/`read_rects` (`mod.rs:425-495`) require the argument to be a
`VAL_OBJECT`, require a numeric `count` member, and require every element to be
an object; otherwise they raise. The reference `getPoints`/`getRects`
(`windows/LayerExDraw.cpp:448-456`, `:484-490`; blend2d `:440-449`) iterate
`GetArrayCount()` and convert each element with `PointFConvertor`, which maps a
**non-object element to `PointF(0,0)`** (`windows/main.cpp:123`), and a
non-array object yields an empty list (no throw). So `dst.drawPolygon(app, [1,
2, 3])` (a flat array of integers) or `dst.drawPolygon(app, %[a:1])` draws
nothing in the reference and throws `"element 0 is not a point"` / `"the
argument is not an array"` in the port. This is the opposite of the
exemplar case (the port accepts nothing where the reference accepts something),
but it is the same marshalling surface. **Script-observable:** yes. **Fix direction:** match
`GetArrayCount` semantics (absent/non-array ⇒ empty) and default a non-object
element to `(0,0)` while still reading a native `PointF`/`RectF` via the
class-chain (which `read_point_object`/`read_rect_object` already do by member
reads).

### F13 (rank 3) — `setTransform` cannot read a native `GdiPlus.Matrix`

`read_matrix` (`draw.rs:708-750`) reads the argument as a `count >= 6` array or
an object with `m11`/`m12`/`m21`/`m22`/`dx`/`dy` members. `GdiPlus.Matrix`
registers no such members (`matrix.rs:733-741` exposes `OffsetX`/`OffsetY` as
methods, `matrix.rs:810` registers no properties), so
`dst.setTransform(new GdiPlus.Matrix(1,0,0,1,3,4))` reads missing members and
raises `"the matrix member m11: ..."`. The reference `setTransform(const
Matrix *transform)` / `setViewTransform(const Matrix *transform)`
(`windows/LayerExDraw.hpp:268,278`) take the native matrix directly, converted
by `MatrixConvertor` (`blend2d/LayerExDraw.cpp:3124-3157`). `read_matrix_arg`
(`matrix.rs:139-220`) already does the correct thing (native payload first,
then array/object); `read_matrix` is the one that lacks the branch. The tests
never call `setTransform` with a `GdiPlus.Matrix` (`layer_ex.rs:486` only uses
`resetTransform`/`translateViewTransform`). **Script-observable:** yes.
**Fix direction:** make `read_matrix` delegate to the `matrix.rs` conversion
(or lift `native_instance_payload(..., "Matrix")` into it).

---

## 6. Pattern 4 — objects used after `invalidate`

The game's idiom is `drawPolygon(app, ...); invalidate app;`
(`system_MessageFrame.tjs:1927-1928`). I checked exactly what the port keeps:

- `finish_path` (`draw.rs:673-700`) rasterizes from the borrowed
  `app: &AppearanceData` inside the synchronous `with_layer` closure and, when
  recording, pushes **deep clones** `RecordOp::Path { path: path.clone(), app:
  app.clone() }` (`draw.rs:686-691`). `render_path` (`draw.rs:437-490`) does not
  store the reference.
- `draw_path` (`draw.rs:978-1013`) clones the `PathData` before `finish_path`.
- `finish_image` (`draw.rs:1108-1160`) records `src: src.rgba.clone()` and the
  geometry, not the source object.
- `draw_string`/`draw_path_string` (`draw.rs:1402-1580`) hold `app` only for the
  synchronous `with_layer` closure and clone the produced `BitmapState` into the
  record.
- `appearance_ref`/`path_ref` return raw payload borrows for the duration of the
  call. **No pointer into the invalidated object survives the
  callback.** A script cannot invalidate mid-call (single-threaded VM, and the
  only re-entry points — `new_rectf`/`eval_object` in `set_rectf_result` — run
  after the borrow is no longer used).

**Verdict: no use-after-free found.** The residual is F11 below.

### F11 (rank 4) — unbounded `'a` on the payload borrows

```rust
fn appearance_ref<'a>(engine, value, what) -> Result<&'a AppearanceData, String>
```

`draw.rs:640,652` return a reference with a **caller-chosen** lifetime derived
from a raw pointer, with only a `// SAFETY:` comment stating the object is alive
for the callback. Today every caller drops it before the callback returns, so
the invariant holds; it would silently break if a future caller stored it (e.g.
a cached appearance, or a borrow held across a VM re-entry). **Not
script-observable today.** **Fix direction:** either return the owned pointer
and dereference at the use site, or return a `&AppearanceData` tied to a
lifetime parameter derived from `value`/`engine` so the borrow checker enforces
the callback scope.

---

## 7. Pattern 5 — member registration shape and parity visibility

**Class/member shape matches the reference.** Comparing the registrations:

| reference | port | verdict |
|---|---|---|
| `NCB_REGISTER_SUBCLASS_DELAY(PointF)` ctor + `x`,`y`,`Equals` (`windows/main.cpp:133-137`) | `geom.rs:730-760` | match |
| `RectF` ctor + `x/y/width/height` + `left/top/right/bottom/location/bounds` + `Clone/Equals/Inflate/InflatePoint/IntersectsWith/IsEmptyArea/Offset/Union` (`windows/main.cpp:181-210`) | `geom.rs:763-878` | match, plus one extra (below) |
| `Image` ctor + `load/Clone/GetBounds/GetFlags/GetHeight/GetWidth/Get*/GetLastStatus/GetPixelFormat/GetType/RotateFlip` (`blend2d/LayerExDraw.cpp:3397-3403`) | `image.rs:520-580` | match |
| `FontInfo` ctor + 4 read/write + 5 read-only (`blend2d:3418-3429`) | `font.rs:501-548` | match |
| `Appearance` ctor + `clear/addBrush/addPen` (`blend2d:3431-3436`) | `appearance.rs:490-506` | match |
| `Path` ctor + 17 methods (`blend2d:3438-3457`) | `path.rs:820-905` | match |
| `NCB_ATTACH_CLASS_WITH_HOOK(LayerExDraw, Layer)` property/method set (`blend2d:3707-3755`) | `draw.rs:2305-2495` + `perspective.rs:341-347` + `movie.rs:505-530` | exact name-for-name match |
| `GdiPlus` namespace + 7 nested classes + `addPrivateFont`/`getFontList` (`blend2d:3463-3651`) | `gdiplus.rs:257-330` | match (globals deleted) |

**F9 (rank 4) — port-only surface.** `RectF.Union` is registered as an
**instance** method (`geom.rs:484-560`) in addition to the class-level static
(`geom.rs:562-640`); the port's own comment calls the instance form a port
extension. `r.Union(a,b)` returns a new `RectF` in the port while the reference
exposes only the 3-argument `static Union(RectF&, const RectF&, const
RectF&)` (`blend2d/LayerExDraw.hpp:389-397`, `windows/main.cpp:210`). The
`plugin-parity` check classifies extras without failing on them, so the gate is
green; it remains a D34 surface divergence. **Fix direction:** drop or align
the instance `Union`.

**Parity visibility.** The "plugin member registration invisible to the parity
scripts" standing concern does **not** apply to `layer_ex`:
The `plugin-parity` check parses `crates/compat/src/natives/plugins/**`
(native_parity helpers) and recognises `register_instance(NativeInstanceBuilder)`,
`register_static_members(NativeStaticMembers)`, and
`register_instance_members(NativeInstanceMembers{class_name: "Layer"})`
(`plugin_parity.py:1-60`), which is exactly how this plugin registers. The
`GdiPlus.*` enum constants are recognised as `set_member` values. The only
residual visibility issue is that `plugin_parity.py` does not **fail** on
extras, which is why F9 shipped silently; that is a policy choice, not a
missing-scanner bug.

---

## 8. Pattern 6 — state that survives the wrong amount of time

### F3 (rank 2) — `addPrivateFont` does not invalidate the face cache

`GdiPlus.addPrivateFont` parses the file into `TextState.engine`
(`font.rs:600-635`) and appends family names to `TextState.private_fonts`. It
does **not** touch `TextState.faces`. `ensure_face` (`font.rs:68-77`) calls the
shared `resource::resolve_face`, which caches the `face -> FontId` mapping
including **fallbacks**:

```rust
if let Ok(id) = engine.load_system_font(face) { cache.insert(face.to_string(), id); return Ok(id); }
if let Some(id) = engine.default_font_for(face) { cache.insert(face.to_string(), id); return Ok(id); }
```

(`resource.rs:270-280`). So: `new GdiPlus.Font('Acme', 16, 0)` before the font
is registered resolves to a fallback and caches `"Acme" -> fallback`; a later
`GdiPlus.addPrivateFont('acme.ttf')` makes `engine.font_id("Acme")` succeed
inside `try_face`, but the cache short-circuits first
(`resource.rs:263-265`) and the private face is never used. The reference has no
such cache — `addPrivateFont` pushes into `bl_font_face_vec` and each
`FontInfo`/draw re-resolves (`blend2d/LayerExDraw.cpp:93-126`) — so the new face
is immediately visible. **Script-observable:** yes (`getFontList`, metrics,
glyph rasterization). **Minimal repro:** `new GdiPlus.Font('Missing',16,0);`
then `GdiPlus.addPrivateFont('real.ttf');` then draw with `'Missing'` (or the
real family if it was requested before registration); the port keeps the
fallback. **Fix direction:** clear `TextState.faces` (and the
`ResourceState.faces` cache) in `register_private_font`, or key the cache by a
registration generation. AGENTS §4/D11 also disallow the cache's silent
fallback semantics for a name that is now registered.

The **same cache alias** applies to `System.addFont` (`add_font` registry):
registering a face after a failed lookup does not take effect.

### F8 (rank 4) — per-layer draw/movie state is never cleaned

`DrawState.layers: HashMap<u32, LayerDraw>` (`draw.rs:179-183`) is keyed by the
monotonic `Scene::next_layer` id (`engine/src/scene.rs:622-631`), so ids are not
reused and there is no cross-layer aliasing; but entries are never removed when
a layer is destroyed, and the plugin is never notified. The same holds for
`LayerExMovieState.slots` (`movie.rs:97-110`): `stop_movie` closes the overlay
but keeps the slot (`movie.rs:230-254`), and `remember_layer_object`
(`movie.rs:261-279`) stores a `DetachedValue` retaining the **script Layer
object**, so a layer that ever opened a movie is kept alive for the binding's
lifetime. A destroyed layer whose movie is still `playing` continues to be
polled by `movie_poll` and logs `"layer #N has no image to copy into"`
(`movie.rs:346-352`). **Not a crash and not directly script-observable**, but a
real leak and noisy state. **Fix direction:** expose a layer-destroyed hook (the
L2 scene already knows) or key these maps in the scene; at minimum drop the slot
and its retained object on `stopMovie` and on layer destruction.

### Pre-rendered fonts (`.tft` / `mapPrerenderedFont`) — not a `layer_ex` concern

`mapPrerenderedFont`/`.tft` are a core `Layer.drawText`/`Font` feature
(`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:116-132`,
`crates/compat/src/natives/visual/font.rs:802-880`). `layerExDraw`'s
`drawString`/`drawPathString` use GDI+
`Graphics::DrawString`/`AddString` in the reference, i.e. the system rasterizer,
not the prerendered font. The port's `render_text` (`font.rs:106-190`) likewise
rasterizes directly through `TextEngine::rasterize_glyph` and never consults
`ResourceState.prerendered*`. This is reference-faithful. The only interaction
is the shared `resolve_face` cache above (F3). **No separate finding.**

### `set_string_out` scratch (checked, no finding)

`set_string_out` (`mod.rs:176-206`) and `set_string_array_out`
(`mod.rs:209-233`) point the out slot at a persistent scratch buffer. The C++
side converts the `tjs2_value` to a `tTJSVariant` before the callback returns
(`tjs2_abi.cpp:1245-1248`), so the pointer is consumed synchronously and the
"valid until the next string result" note is conservative, not a dangling-read
bug. No change requested.

---

## 9. What this audit cannot decide without a build or a device

- **Whether a specific game calls the F5/F6/F7 shapes.** These are D34
  divergences confirmed against the reference; whether any shipped title passes
  an object where a `REAL` is expected, a short `drawCurve2`, or a flat point
  array is not determinable from this repo.
- **`RectF.Union`'s true reference level.** The C++ function is `static` but is
  declared with `NCB_METHOD(Union)` inside the `RectF` class block
  (`windows/main.cpp:210`); without building the reference plugin I cannot
  confirm whether ncbind exposes it as an instance member, a static, or both.
  The port currently exposes both. The `windows/main.cpp` registration and the
  `ncbind.hpp` `NCB_METHOD` expansion are the tiebreaker.

---

## 10. Coverage ledger

| file | lines read | findings |
|---|---|---|
| `mod.rs` | all 547 | F4 context, F7 (`read_points/rects`), F11 |
| `draw.rs` | all 2495 | F5, F6, F12, F7 (`read_matrix`), F11, F8 (`DrawState`); pattern-4 clone check clean |
| `appearance.rs` | all 532 | none |
| `path.rs` | all 859 | F10 |
| `font.rs` | all 709 | F3, F4 |
| `geom.rs` | all 898 | F9 (`RectF.Union` instance); PointF/RectF marshalling matches `PointFConvertor`/`RectFConvertor` |
| `image.rs` | all 596 | none (correct `native_instance_payload` use) |
| `matrix.rs` | all 812 | positive control for the class-chain pattern; `read_matrix_arg` correct |
| `movie.rs` | all 743 | F8 (`slots` never cleaned, retained layer object) |
| `perspective.rs` | all 347 | none (`native_instance_id` correct) |
| `record.rs` | all 368 | none (deep copies; length math checked with `checked_mul`) |
| `gdiplus.rs` | all 328 | namespace/global aliasing matches; F9 surface |
| `tests/layer_ex.rs`, `tests/layer_ex_behaviour.rs` | read | coverage gap: no legacy-constructor/subclass receiver test |
| references | `windows/LayerExDraw.{cpp,hpp}`, `windows/main.cpp`, `blend2d/LayerExDraw.{cpp,hpp}`, `layerExMovie.cpp`, `layerExPerspective.cpp`, `layerExBase.hpp`, `tjsVariant.{h,cpp}`, `ncbind.hpp`, `tjs2_abi.cpp` | citations above |
