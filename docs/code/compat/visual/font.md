# `natives/visual/font.rs` — the `Font` class

**Source:** `crates/compat/src/natives/visual/font.rs`

`Font` is the script-visible text style: a face, a height, the style flags
and an angle. The measurement goes through the L2 text engine and the
per-engine face resolver.

The registered surface is 21 members (the surface dump lists 23 entries —
the class-name constructor and the VM's `finalize` are two of them).

---

## 1. Construction and ownership

The font's own state lives in the L2 scene's font store (`FontState`). The
L3 payload holds three things:

| Field | Purpose |
|---|---|
| `id: Option<u32>` | the scene font id |
| `attached: bool` | whether the constructor has run |
| `owns_state: bool` | whether this object owns the scene font |

`owns_state` is `true` for `new Font(…)` and `false` for the `Layer.font`
wrapper: binding the wrapper to the layer-owned font id clears the flag and
drops the wrapper's previously-owned scene font. `font_destroy` therefore
removes the scene font only when the object owns it, while the
`faceIsFileName` entry is always removed.

The **native create** registers a default scene font eagerly (`MS Gothic`,
16, `0xffffffff`), so a subclass may touch a member before `super.Font(…)`.
The constructor then **resets** that font, so the constructor arguments win
over a pre-constructor write.

A second construction raises `"Font: this font is already constructed"`.

---

## 2. The members

| Member | Behaviour |
|---|---|
| `Font([face, height, color])` | creates or resets the font. The face defaults to `"MS Gothic"`, the height to `16` and the colour to `0xffffffff`. A negative height is normalised to positive. Returns the scene font id. |
| `getTextWidth(text)` | the sum of each character's rounded advance in pixels (an integer). For a **layer-bound** font with a mapped `.tft`, the sum of the `.tft` `Inc` values instead. |
| `getTextHeight(text)` | `abs(Font.Height)` in pixels (an integer). |
| `getEscWidthX(text)` | `cos(θ) · width`, with `θ = angle · π / 1800` (a real). |
| `getEscWidthY(text)` | `−sin(θ) · width` (a real). |
| `getEscHeightX(text)` | `sin(θ) · height` (a real). |
| `getEscHeightY(text)` | `cos(θ) · height` (a real). |
| `getGlyphDrawRect(text)` | a fresh `Rect` object with `left`/`top`/`right`/`bottom` = the rasterized glyph bounding box. |
| `doUserSelect(flags, caption, prompt, samplestring)` | requires 4 arguments; returns `0` and changes nothing (the reference's host-selection call is compiled out). |
| `getList(flags)` | a TJS array of the resolver's registered face family names. The `flags` argument is accepted and ignored. |
| `mapPrerenderedFont(storage)` | loads and validates the `.tft` header and maps this font value to it. A missing or malformed storage raises. |
| `unmapPrerenderedFont()` | drops the mapping; later measurement falls back to the rasterizer. |
| `face` | the requested face / family name. |
| `height` | the pixel height (always ≥ 0). |
| `bold`, `italic`, `strikeout`, `underline` | the style flags as `0`/`1`. |
| `angle` | the rotation in **tenths of a degree**, normalised to `0..3600`. The getter rounds the stored value. |
| `faceIsFileName` | the per-font flag (the reference's font-file flag). |
| `Font.rasterizer` (static) | the active rasterizer index. Only `0` is available; writing any other value raises. |
| `Font.defaultFaceName` (static) | the resolver's default face name (empty when none is registered). Writes are **ignored**. |

`Font.color` is **not** registered, although the L2 `FontState` carries a
colour (set only by the constructor).

---

## 3. The measurement rules

* A **mapped `.tft`** gives `width = Σ Inc` (falling back to the rasterizer
  advance for a unit the `.tft` lacks) and `height = abs(Font.Height)`.
* Otherwise the width is the sum of each character's rounded advance and the height is `abs(Font.Height)`.
* `getTextWidth` and `getTextHeight` are **unrotated**; only the `getEsc*`
  variants apply the angle.
* `getGlyphDrawRect` always returns a **fresh** `Rect` object; a missing
  rect becomes `(0, 0, 0, 0)`.
* A **pre-rendered** font is consulted only through a **layer-bound** font
  (`owns_state == false`); a standalone `new Font()` measures through the
  rasterizer.
* `Layer.getTextWidth` / `Layer.getTextHeight` measure the layer's current
  font with the same rules; the height is `abs(Font.Height)` and the angle
  does not rotate either result.

---

## 4. The face resolution

`Font.face` is resolved through the per-engine `ResourceResolver` — see
[`../resource.md`](../resource.md) §4 for the full order:

1. a cached resolution for the exact face string;
2. for each candidate of the comma-separated list (with an optional leading
   `@` for vertical writing): the engine's registered families, the
   `System.addFont` registry, the attached storage (only for a file-shaped
   name), the host's installed system fonts;
3. the host's default face, loaded under the fixed name
   `__krkr_default_face__`;
4. the engine's first registered face;
5. otherwise `"no font is available for face '{face}'"`.

The resolved family name is recorded in registration order, which is what
`Font.getList` reports and what `Font.defaultFaceName` returns.

---

## 5. State ownership

| State | Owner |
|---|---|
| the face, the height, the colour, the style flags, the angle | L2 `Scene` / `FontState` |
| the scene font id, `attached`, `owns_state` | the L3 `FontInst` payload |
| `faceIsFileName` (per font id), the active rasterizer index | the L3 `FontMetricsState` |
| the text engine, the face cache, the registered family names, the default face, the pre-rendered font maps | the L3 `ResourceResolver` |

---

## 6. Invariants

* **Usable from creation** — the native create registers a default scene
  font, so a subclass may touch a member before the constructor; the
  constructor's arguments then win.
* **The height** is clamped to the `i32` range and made positive.
* **The angle** is `rem_euclid(3600)`.
* **The ownership** rule: only an owner removes the scene font.
* **The rasterizer** accepts only index `0` — the single L1 render glyph
  rasterizer.
* **`Font.defaultFaceName`** writes are ignored: the reference's value comes
  from the preference / enumeration.

---

## 7. Errors

| Message | Raised when |
|---|---|
| `"Font: compat context is not bound"` | the binding is not installed |
| `"Font: this font is already constructed"` | the constructor ran twice |
| `"Font: this font is not constructed"` | the payload has no scene id |
| `"Font: font no longer exists"` | the scene font is gone |
| `"Font.face: compat context is not bound"` | the binding is not installed |
| `"Font.faceIsFileName: this font is not constructed"` | the payload has no scene id |
| `"Font.getTextWidth requires a string argument"`, `"Font.getTextHeight requires a string argument"`, `"Font.getEscWidthX requires a string argument"`, `"Font.getEscWidthY requires a string argument"`, `"Font.getEscHeightX requires a string argument"`, `"Font.getEscHeightY requires a string argument"`, `"Font.getGlyphDrawRect requires a string argument"` | the arity / type checks |
| `"Font.getGlyphDrawRect: {error}"` | the result could not be produced |
| `"Font.doUserSelect requires flags, caption, prompt and samplestring"` | fewer than 4 arguments |
| `"Font.getList requires a flags argument"` | no argument |
| `"Font.mapPrerenderedFont requires a storage argument"`, `"Font.mapPrerenderedFont: this font is not constructed"` | the argument or the state |
| `"Font.unmapPrerenderedFont: this font is not constructed"` | the payload has no scene id |
| `"Font.rasterizer: only rasterizer 0 (the platform render rasterizer) is available"` | a rasterizer index other than 0 |
| `"Font.defaultFaceName: compat context is not bound"` | the binding is not installed |
| `"Font: text layout failed: {error}"` | the L2 text engine rejected the layout |
| `"no font is available for face '{face}'"` | no candidate, no default face and no registered face at all |
| `"Cannot find storage {storage}"` | the pre-rendered font's storage does not resolve |
| `"Prerendered font mapping failed : {sub}"` | the `.tft` header is invalid |

`getGlyphDrawRect` logs `"Font: cannot create the result Rect: {error}"` and
yields void when the result object cannot be built.

---

## 8. Host seams

| Seam | Purpose |
|---|---|
| `attach_scene` | the `FontState` store |
| `attach_storage` | a face string is read as a storage name |
| `CompatHost::set_system_font_dirs(dirs)` | where the host's system fonts live — the composition root wires `platform::system_font_dirs()` into it |
| the `System.addFont` registry and `GdiPlus.addPrivateFont` | the other face sources |
| the L2 `engine::text::TextEngine` | shaping, layout, glyph rasterization, `glyph_advance`, `glyph_draw_rect` |

---

## 9. Tests

| Test | What it asserts |
|---|---|
| `crates/compat/tests/font.rs` | the metrics use the L2 text engine and the `getEsc*` projections at angle 0 and at `angle = 900`; a font with no source does not fabricate a metric; `getGlyphDrawRect` returns the rasterized bounds; `getList` and `defaultFaceName` report the registered face; `doUserSelect` returns zero and changes nothing; `rasterizer` is the single L1 rasterizer and another index raises; `mapPrerenderedFont` raises for a missing storage and for each of the four reference header errors; a mapped pre-rendered font serves each mapped font its own `Inc`; the remap serves both mappings; the pre-rendered pixels match the coverage; the metrics use the mapped `Inc` and height; `unmapPrerenderedFont` drops the mapping; the properties round-trip; the metrics fall back to a system face; a subclass member write before the base constructor does not raise. |
| `crates/compat/tests/visual.rs` | `font_properties_round_trip`, `layer_font_wrapper_is_cached_and_bound`. |
| `crates/compat/tests/dispatch_visual.rs` | the receiver routing. |
| `crates/compat/tests/layer_behaviour.rs` | the `Layer.font` wrapper is read-only. |

---

## See also

* [`../resource.md`](../resource.md) — the face resolution and the pre-rendered font map
* [`pixels.md`](pixels.md) — the text drawing path that uses the same resolver
* [`../engine_api.md`](../engine_api.md) — the L2 text items
