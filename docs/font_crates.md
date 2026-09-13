# Replacing handcrafted font rasterization/layout with an external crate

Evaluation of `cosmic-text`, `swash`, `rustybuzz`, `ab_glyph` (current),
`fontdue`, `parley`, `glyphon`, `ttf-parser` and `tiny-skia` for
`crates/tvp-text` and the `Layer.drawText` path in
`crates/tvp-visual/src/natives/layer.rs`.

Benchmarks in this document were run on the dev machine (Rust 1.97.1,
release, `NotoSansCJK-Regular.ttc` face 0, 32 px, 29 distinct CJK chars) in a
throwaway crate under `/tmp`; the repository was **not** modified. Numbers are
indicative, not a contract.

---

## TL;DR / Recommendation

**Keep `ab_glyph` + the custom layout/atlas/`.tft` code. Do not adopt
`cosmic-text`, `parley` or `glyphon`.** They replace the wrong layer for this
engine: full shaping/line-breaking/system-fallback pipelines we do not need
(the game draws **one character per `drawText` call**, see
`layer.rs:1571`), a fallback model that conflicts with the explicit
`fonts.json` selection in `tvp_text::resolve_face` / `FaceRequest`
(`font.rs:235,332`), and a 40–50 crate dependency tree with duplicate
`fontdb`/`skrifa`/`read-fonts` versions, for a per-char pipeline measured at
~11.5 µs versus our warm Atlas hash lookup of ~3 ns.

**The one worthwhile external upgrade is a contained rasterizer-only swap to
`swash`** ("`swash` rasterizer + keep our layout"), because it is the only
candidate that improves the actual weakness — rasterization quality (TrueType
hinting, stem darkening, subpixel positioning, variable/color fonts) — while
leaving `layout.rs`, the explicit font resolution, the per-char semantics and
the `.tft` path untouched. Treat it as an **optional, S–M follow-up gated on
visible small-size CJK quality regressions**, not a required change. The
current output already matches the reference engine's *synthetic* bold/italic
(the reference calls FreeType's `FT_GlyphSlot_Embolden` / `FT_GlyphSlot_Oblique`,
`reference/cpp/core/visual/FreeType.cpp:677-679`), so replacing the rasterizer
is a quality choice, not a correctness fix.

---

## 1. What the current code actually does

### 1.1 Stack

| Piece | Where | Role |
|---|---|---|
| `ab_glyph` 0.2 (wraps `ttf-parser` 0.25) | `crates/tvp-text/Cargo.toml:9` | parse + scale + rasterize outlines |
| `fontdb` 0.24 | `crates/tvp-text/Cargo.toml:10` | system discovery + family-name parsing of TTC/OTC (not rasterization) |
| `FontFace` | `font.rs:70` | owned `FontVec`, collection index, `px_scale_for_height` (`font.rs:104`) |
| `GlyphAtlas` | `atlas.rs:53` | fixed-cell, append-only RGBA atlas, `HashMap<char, GlyphSlot>` |
| `layout` | `layout.rs:101` | line breaks, wrap-anywhere CJK, alignment, vertical centring |
| `measure_width` | `measure.rs:11` | advance sum for hit-testing |
| `.tft` parser | `prerendered.rs:112` | custom TVP pre-rendered bitmap fonts |
| compositing | `layer.rs:2254` `paint_layout` | blend atlas pixels into `BitmapState`, italic/angle/shadow/underline |

### 1.2 The per-character call path

`layer_draw_text` (`layer.rs:1488`) is called by the game once per character
(typewriter), per the code comment at `layer.rs:1571-1574`. Each call:

1. snapshots `DrawTextStyle` (`layer.rs:1467`) from the layer's `FontState`;
2. checks the `.tft` registry (`prerendered_font`, `prerendered.rs:287`) — if
   every char in the call is present it paints and returns (`layer.rs:1584`);
3. otherwise resolves the face via `resolve_face` (process-global `HashMap`,
   `font.rs:332`) and the per-`(face,height,bold)` atlas via
   `with_cached_atlas_styled` (`atlas.rs:350`);
4. runs `layout()` and `paint_layout`.

So the *warm* per-char cost is a handful of hash lookups + mutex locks +
per-glyph compositing. Rasterization happens **once per distinct glyph**, not
once per draw. Any external crate can only affect (a) the one-time raster of a
new glyph and (b) the quality of that raster — not the steady-state per-char
cost.

### 1.3 Synthetic styles (deliberate, reference-matching)

* **Bold**: `embolden_coverage` (`atlas.rs:305`) — 1 px rightward coverage
  dilation + `advance += 1.0`, selected by a separate `(face,height,bold)`
  atlas (`atlas.rs:329`).
* **Italic**: paint-time shear `ITALIC_SLANT = 0.25`, `layer.rs:2274-2294`.
* **Angle**: per-glyph rotation about the ink centre, `layer.rs:2280-2310`.
* **Underline/strikeout**: `paint_rule` (`layer.rs:2354`) for vector text and
  `paint_prerendered_rules` (`layer.rs:2214`) for `.tft`.
* **Shadow**: square dilation (`spread`) in both paint paths.

The reference engine does exactly the same two synthetic transforms via
FreeType. Real bold/italic font-file variants or variable-font axes are **not**
required for reference parity.

### 1.4 `.tft` pre-rendered fonts

`prerendered.rs` implements a format that exists **only in TVP/KiriKiri**
(magic `TVP pre-rendered font\x1a`, 20-byte char items, two RLE versions,
`TVP65_255` upscaling). No third-party crate parses it. It also dominates the
main dialogue path: the game ships ~24 `.tft` fonts used for main text, with
the vector path as per-glyph fallback. **This must remain custom regardless of
any decision below.**

---

## 2. Evaluation axes

* **Does the per-char model need HarfBuzz-class shaping?** No. Each call is
  usually a single `char`; the reference also draws per char
  (`LayerIntf.cpp:4433`). CJK ideographs/kana are not contextual;
  `layout.rs` already breaks anywhere and the atlas notes CJK fonts define no
  kerning pairs. Shaping is only relevant if the game ever passes a multi-char
  string — and then Latin kerning (`ab_glyph`'s `kern_unscaled`) is enough.
* **Fallback.** `fonts.json` (`font_config.rs`, `docs/fonts.md`) deliberately
  makes krkr-rs do **no implicit** selection. `FaceRequest` (`font.rs:235`)
  resolves `Named`/`Path`/`SystemJp` through the explicit `faces` then
  `fallback` chain. A crate with its own system-fallback database
  (`cosmic-text`, `parley`) fights this model; `swash` has no fallback at all,
  so it slots in under the existing resolver.
* **Per-char overhead / caching.** We already cache the final coverage bitmap
  in the atlas; any external crate still needs our atlas (or an equivalent
  image cache) to avoid re-rasterizing. Warm lookup is ~3 ns.
* **Bitmap compositing, not Bevy UI.** Glyphs are blended CPU-side into
  `BitmapState` (`layer.rs`, `natives/bitmap.rs`). Anything GPU/pipeline-shaped
  (`glyphon`) or layout-buffer-shaped (`cosmic-text`) is a mismatch.
* **License / MSRV / portability / weight.** Workpace license is
  `MIT OR Apache-2.0`, edition 2024 (MSRV ≥ 1.85). Targets include Android and
  wasm32 eventually (`docs/portability.md`), so pure-Rust, no-C, no-`fontconfig`
  crates are required.

---

## 3. Crate-by-crate

### `ab_glyph` (current) — keep
Pure Rust, `Apache-2.0`, 6-package closure (`ttf-parser`, `owned_ttf_parser`,
`ab_glyph_rasterizer`, `core_maths`, `libm`). Unhinted AA rasterization, no
variable-font or color/bitmap-glyph support, no shaping (advances + `kern`).
`fontdb` handles discovery/names. Already the smallest, most portable option.

### `swash` 0.2.10 — best optional upgrade (rasterizer only)
`Apache-2.0 OR MIT`, 16-package closure. Provides:
* TrueType **hinting** (`ScalerBuilder::hint(true)`), stem darkening, subpixel
  positioning, variable-font axes, color outlines and embedded bitmap glyphs;
* `Render::embolden(strength)` and `Render::transform(...)` — higher-quality
  synthetic bold/oblique than our 1 px dilation/shear;
* metrics/charmap and an optional own `Shaper`.
**No font discovery/fallback** — exactly what we want under `resolve_face`.
`ScaleContext` and `FontRef` are `Send + Sync` (verified), so it fits the
global `Mutex<GlyphAtlas>` cache (`atlas.rs:329`). Note it now pulls the
Fontations stack (`skrifa`, `read-fonts`, `font-types`, `zeno`, `yazi`), so its
weight is closer to ~16 crates than the old ~5.

### `cosmic-text` 0.19 — do not adopt
`MIT OR Apache-2.0`, MSRV 1.89, 47-package closure. Uses `harfrust` (pure-Rust
HarfBuzz port) + `skrifa` + `swash` and bundles **its own `fontdb` 0.23**
(duplicating our 0.24) and two `skrifa`/`read-fonts`/`font-types` versions. It
is a complete text pipeline (discovery, shaping, bidi, wrapping, raster,
image cache). Great crate, wrong layer: we would either surrender the explicit
`fonts.json` fallback to its `FontSystem` database, or pre-register faces and
fight its model. Measured full per-char pipeline (set_text + shape + raster):
**11.5 µs**, vs our warm atlas hit ~3 ns and ~4.9 µs one-time raster.

### `parley` 0.11.1 — do not adopt
`Apache-2.0 OR MIT`, MSRV 1.88. Linebender's layout engine: line breaking,
bidi, shaping (HarfBuzz-class), `fontique` fallback, `swash` raster. Same
layer mismatch as `cosmic-text`, plus a UI-oriented layout object we would
discard; heavier dependency closure. Our `layout.rs` is 234 lines and already
does the CJK wrap/align/centring the KAG path needs.

### `glyphon` 0.12 — not applicable
`MIT OR Apache-2.0 OR Zlib`. A `wgpu` text renderer built on `cosmic-text`: it
rasterizes into a GPU atlas and draws quads inside a wgpu render pass. We
composite into CPU `BitmapState` and do not use Bevy UI/text; adopting it would
require a render-pass contract `tvp-text` does not have and would not produce
the CPU pixels the scene uploads. Hard no.

### `rustybuzz` 0.20.1 — not needed
`MIT`. Pure-Rust HarfBuzz port (shaping only). Per-char CJK needs no shaping;
`cosmic-text` 0.19 itself has moved past it to `harfrust`. Adding it would buy
nothing our per-char model can use.

### `fontdue` 0.9.4 — no
`MIT OR Apache-2.0 OR Zlib`. Fast (1.4 µs cold raster), pure Rust, but
**no hinting**, no multi-face TTC selection, no synthetic bold/italic, no
color/variable fonts. A feature downgrade from `ab_glyph`, not an upgrade.

### `ttf-parser` 0.25.1 — already present
`MIT OR Apache-2.0`, MSRV 1.63. Parser only; reached transitively via
`ab_glyph` and `fontdb`. If we ever need variation axes / color tables
directly it can be added as a direct dep, but it is not a rasterizer.

### `tiny-skia` 0.12 — orthogonal
`BSD-3-Clause`. High-quality 2D scanline/AA path filler. Could render glyph
outlines after `ttf-parser` extraction, but with no hinting it would not beat
`ab_glyph`; its real use is the GDI+ vector ops in
`tvp-visual/src/natives/raster.rs`, not text. Not a font solution.

---

## 4. Comparison table

| Crate | Role | Raster quality | Fallback | Shaping | Per-char cost (measured) | License | Portability | Fits? |
|---|---|---|---|---|---|---|---|---|
| **ab_glyph 0.2** (current) | parse+scale+raster | AA, **unhinted**, no variable/color | none (we do it) | none (advances/`kern`) | ~4.9 µs cold; **~3 ns warm atlas** | Apache-2.0 | pure Rust, no_std, wasm/Android | ✅ **keep** |
| fontdb 0.24 (current) | discovery + names | n/a | system families | n/a | n/a | MIT OR Apache-2.0 | fs feature (in-mem works) | ✅ keep |
| **swash 0.2** | raster (+opt. shape) | AA + **hinting**, stem darkening, subpixel, variable/color, real synthetic bold/oblique | none | own `Shaper` (optional) | 8.6 µs cold / 2.2 µs re-render; warm via our atlas ~3 ns | Apache-2.0 OR MIT | pure Rust, `libm` no_std, `Send+Sync` | ✅ **optional hybrid** |
| cosmic-text 0.19 | full pipeline | swash quality | own `fontdb` DB | `harfrust` (HarfBuzz-class) | 11.5 µs full; 0.024 µs image-cache hit | MIT OR Apache-2.0 | pure Rust, wasm-web/no_std; MSRV 1.89 | ❌ conflicts, heavy |
| parley 0.11 | full layout | swash quality | `fontique` | HarfBuzz-class | ~cosmic-text (not benchmarked) | Apache-2.0 OR MIT | pure Rust; MSRV 1.88 | ❌ wrong layer |
| glyphon 0.12 | wgpu text renderer | swash via cosmic | cosmic | cosmic | GPU pass (not CPU) | MIT OR Apache-2.0 OR Zlib | wgpu/wasm | ❌ not our model |
| rustybuzz 0.20 | shaping only | n/a | none | HarfBuzz-class | n/a (not needed) | MIT | pure Rust | ❌ unneeded |
| fontdue 0.9 | raster | AA, **no hinting**, no TTC/variable/color | none | none | 1.4 µs cold | MIT OR Apache-2.0 OR Zlib | pure Rust | ❌ downgrade |
| ttf-parser 0.25 | parse only | n/a | n/a | n/a | n/a | MIT OR Apache-2.0 | pure Rust | ➖ already transitive |
| tiny-skia 0.12 | 2D vector raster | path AA, no hinting | n/a | n/a | not text | BSD-3-Clause | pure Rust, SIMD | ➖ orthogonal |

Per-char cost is deliberately split: "cold" is the one-time raster of a new
glyph; "warm" is what a typewriter call actually pays. The compositing loop in
`paint_layout` (`layer.rs:2254`) is O(ink pixels) and is **crate-independent**,
so it dominates and no candidate changes the steady-state budget.

---

## 5. Benchmarks (throwaway crate, `/tmp/fontbench`)

Release, `NotoSansCJK-Regular.ttc` face 0, ppem 32, 29 distinct CJK chars
cycled. `ab_glyph` mirrors the repo (rescale via `px_scale_for_height`, 64×64
scratch coverage, `outline.draw`).

| Operation | µs/call | calls |
|---|---:|---:|
| `ab_glyph` rasterize a distinct glyph (cold, scratch alloc) | **4.908** | 2 000 |
| `ab_glyph` atlas `HashMap` lookup (warm per-char draw) | **0.003** | 5 000 000 |
| `swash` rasterize a distinct glyph (cold, render alloc) | **8.637** | 2 000 |
| `swash` same glyph re-render (no public image cache) | **2.219** | 2 000 |
| `fontdue` rasterize a distinct glyph | **1.398** | 2 000 |
| `cosmic-text` full pipeline (set_text + shape + raster) | **11.510** | 300 |
| `cosmic-text` cached `SwashCache::get_image` lookup | **0.024** | 2 000 000 |

Interpretation:

* Cold rasterization is **single-digit µs** for every crate — a one-time cost
  per distinct glyph, invisible next to the ~272 ms font-discovery stall already
  tracked in `docs/optimization.md:44,231`.
* The steady-state typewriter cost is the atlas hash hit (~3 ns), then the
  per-pixel blend. `cosmic-text`'s cached lookup (24 ns) is close but requires
  its `FontSystem`/`Buffer`/shaper machinery; its full pipeline (11.5 µs) is
  ~4 000× our warm path for zero shaping benefit on single CJK chars.
* `swash`'s 2.2 µs same-glyph re-render shows its internal outline cache, but
  with our atlas we would never re-render a cached glyph anyway.

Verdict: **per-char overhead is not a differentiator**; the decision rests on
quality, integration fit and dependency weight.

---

## 6. Rasterization quality (hinting) evidence

Same font, `ab_glyph` (unhinted) vs `swash` `hint(true)`; `nz` = non-zero
pixels, `sum` = coverage total. Hinting snaps stems: fewer but denser coverage
pixels at small sizes.

| px | char | ab_glyph nz / sum | swash(hint) nz / sum |
|---|---:|---:|---:|
| 12 | 國 | 129 / 13 969 | 101 / **14 368** |
| 12 | 龍 | 121 / 13 860 | 98 / **14 480** |
| 12 | A | 41 / 5 199 | 37 / 5 112 |
| 16 | 龍 | 203 / 24 685 | 178 / **25 749** |
| 32 | 國 | 589 / 99 653 | 565 / **102 221** |
| 32 | A | 204 / 36 958 | 199 / 37 032 |

The difference is modest but consistent and is the only measurable quality
argument for swapping the rasterizer. For a VN whose main text is bitmapped
`.tft`, the vector path is fallback-only, so the visual impact is limited.

---

## 7. License, MSRV, portability, weight

| Crate | License | MSRV | wasm/Android | Normal-dep closure |
|---|---|---|---|---|
| ab_glyph 0.2.32 | Apache-2.0 | none (ed. 2021) | ✅ pure Rust | **6** |
| fontdb 0.24 | MIT OR Apache-2.0 | — | ✅ (fs gated) | — |
| swash 0.2.10 | Apache-2.0 OR MIT | none (ed. 2021) | ✅ `libm` no_std | **16** |
| cosmic-text 0.19 | MIT OR Apache-2.0 | **1.89** | ✅ wasm-web/no_std | **47** |
| parley 0.11.1 | Apache-2.0 OR MIT | **1.88** | ✅ | heavy |
| glyphon 0.12 | MIT OR Apache-2.0 OR Zlib | — | wgpu | heavy |
| rustybuzz 0.20.1 | MIT | — | ✅ | ~medium |
| fontdue 0.9.4 | MIT OR Apache-2.0 OR Zlib | — | ✅ | 8 |
| tiny-skia 0.12 | BSD-3-Clause | — | ✅ | low |

All licenses are permissive and compatible with `MIT OR Apache-2.0` (note
`tiny-skia` is BSD-3-Clause, adding a third license text). MSRV: edition 2024
already requires ≥1.85, so `cosmic-text` 1.89 / `parley` 1.88 are not blockers
but do raise the floor. `ab_glyph`/`swash` keep maximal portability with no
system `fontconfig` requirement. `cosmic-text` would additionally duplicate
`fontdb` (0.23 vs our 0.24) and `skrifa`/`read-fonts`/`font-types` versions in
`Cargo.lock` — real binary/compile-time weight for a fallback-only path.

---

## 8. Decision

1. **Keep `ab_glyph` + custom layout/atlas/`.tft`.** It is the smallest,
   most portable, reference-faithful option and has the best warm per-char
   path. Nothing about the per-char KAG model needs shaping or a system
   fallback engine.
2. **Do not adopt `cosmic-text`/`parley`/`glyphon`.** Wrong layer, fights
   `fonts.json`, 40–50 deps, duplicate font stacks, no per-char benefit.
3. **Optional (`S–M`, gated): swap only the rasterizer to `swash`**, keeping
   `layout.rs`, `resolve_face`/`FaceRequest`, the atlas/bold caching model,
   per-char semantics, shadow/underline, and the entire `.tft` path. Do it only
   if small-size CJK fallback text is visibly weaker than the reference; add
   golden-image tests before/after.

---

## 9. Migration plan — `swash` rasterizer hybrid (optional)

**Scope**: `crates/tvp-text` only (`atlas.rs`, `font.rs`, `measure.rs`
optionally). No changes to `layout.rs`, `prerendered.rs`, `font_config.rs`, or
the `.tft` selection in `layer.rs`.

1. **Dependency**: add
   `swash = { version = "0.2", default-features = false, features = ["std", "scale", "render"] }`
   to `crates/tvp-text/Cargo.toml` (repo `Cargo.toml` untouched unless/until
   this is actually adopted).
2. **Font data**: `ab_glyph::FontVec` exposes no raw-byte accessor, so store
   `Arc<[u8]>`/`Vec<u8>` alongside the `FontVec` in `FontFace` (`font.rs:70`)
   and construct `swash::FontRef::from_index(&bytes, index)` on demand (cheap)
   or cache it in the atlas.
3. **Atlas raster**: replace the `outline_glyph` + `outline.draw` block in
   `GlyphAtlas::rasterize_new` (`atlas.rs:173`) with a `swash` `ScaleContext` +
   `Scaler` + `Render` producing an alpha `Image`; copy `image.data` into the
   packed cell. Keep the rest of the packing/`GlyphSlot` code unchanged.
4. **Synthetic bold**: delete `embolden_coverage` (`atlas.rs:305`) and the
   `bold_pad`/`advance += 1.0` compensation; use `Render::embolden(strength)`.
   Re-derive the advance delta from the rendered `placement` and re-tune `PAD`.
5. **Scale**: `swash`'s `size(ppem)` is already em-based, so
   `px_scale_for_height` (`font.rs:104`) can be deleted for the raster path.
   `measure.rs` can keep `ab_glyph` advances (or move to `swash` metrics).
6. **Threading**: `ScaleContext` is `Send + Sync`; a context per
   `GlyphAtlas` behind its existing `Mutex` (`atlas.rs:329`) works unchanged.
7. **Tests**: existing image-based tests in `layer.rs` (bold adds coverage,
   italic shears, underline/strikeout, shadow) stay as regression gates;
   expect small pixel deltas from hinting/embolden.

**Effort**: S–M (≈0.5–2 days including golden-image review). **No layout
migration**, because `layout.rs` is already correct for the KAG per-char model.

---

## 10. What must stay custom no matter what

* **`.tft` parser + registry** — `prerendered.rs:112,196,222,287` — a
  TVP/KiriKiri-only format and the main-text path (~24 fonts).
* **Per-char `drawText` semantics** and the `.tft`-vs-vector per-call
  all-or-nothing choice (`layer.rs:1571-1620`), including the game's
  per-character typewriter rhythm.
* **Explicit font resolution** — `font_config.rs`, `FaceRequest`
  (`font.rs:235`), `resolve_face` (`font.rs:332`), `fonts.json`
  (`docs/fonts.md`) — the "no implicit selection" contract.
* **Bitmap compositing** — `paint_layout` (`layer.rs:2254`), shadow `spread`,
  `paint_rule`, `paint_prerendered_text/rules`, and the `BitmapState` dirty
  upload contract (`docs/optimization.md`). None of the candidate crates own
  this.
* **Layout/wrap/alignment** (`layout.rs:101`) unless a future KAG requirement
  (bidi, complex shaping) appears — and even then `swash::shape::Shaper` or
  `harfrust` would be added, not a full `cosmic-text` pipeline.

---

## 11. Risks and open questions

* **`swash` hinting changes pixels.** Golden-image deltas and possible
  line-height/advance differences must be reconciled with `layout.rs`'s
  vertical-centring convention (swash returns top-origin placement vs
  ab_glyph's baseline-relative `px_bounds`).
* **Synthetic bold parity.** `swash`'s embolden grows both sides of the
  outline, unlike the current right-only dilation; advance and PAD tuning must
  be re-derived, and the "bold adds coverage" test may need a tolerance.
* **Dependency creep.** `swash` 0.2 already pulls the Fontations stack; if the
  goal is minimal binary size, `ab_glyph` wins. `cosmic-text` would add
  duplicate `fontdb`/`skrifa`/`read-fonts` versions.
* **MSRV.** Choosing `cosmic-text` (1.89) or `parley` (1.88) raises the floor
  above edition 2024's 1.85; `ab_glyph`/`swash` do not.
* **No shaping need demonstrated.** If a game ever draws multi-char strings
  with Latin kerning/ligatures, add `kern()` (already in `ab_glyph`) or
  `swash::shape::Shaper`; do **not** stand up a full pipeline for it.
* **Fallback stays ours.** If glyphs are missing from the resolved face, the
  current behavior (`glyph_id_with_fallback`, `font.rs:429`) is .notdef/U+FFFD;
  a richer per-run fallback would be a `resolve_face`/`layout` change, not a
  rasterizer swap.

---

## 12. Reproducing the numbers

```sh
cargo new /tmp/fontbench --bin          # throwaway, never the repo
cd /tmp/fontbench
cargo add ab_glyph swash cosmic-text fontdue
cargo run --release                     # raster/cache timings
# + src/bin/quality.rs and src/bin/sendtest.rs from this evaluation
```

Font used: `/usr/share/fonts/noto-cjk/NotoSansCJK-Regular.ttc`, face 0.
The repository's `Cargo.toml` and `Cargo.lock` were not modified.
