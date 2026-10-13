# The text stack: which crates, and why

This document records the crates the text path uses today and the role of
each. It is a present-state note, not a migration plan.

---

## Current stack

| Layer | Crate | Role |
|---|---|---|
| L2 `engine` | `cosmic-text` 0.19 (`default-features = false`, `features = ["std"]`) | shaping (`harfrust`), bidi, line breaking, font discovery/fallback, line metrics |
| L1 `render` | `ab_glyph` 0.2 (wrapping `ttf-parser` 0.25) | parse, scale and rasterize glyph outlines; `font_metrics`, `glyph_advance` |
| L2 `engine` | custom `.tft` parser | TVP pre-rendered bitmap fonts (see [`fonts.md`](fonts.md)) |

`engine::text::TextEngine` owns the shaping system and the registered faces and
delegates layout to `cosmic-text`. `render` owns the face, the coverage bitmap
and the rasterizer; L2 owns the glyph atlas that packs those bitmaps.
`cosmic-text`'s `swash` and `fontconfig` default features are disabled, so L2
does not pull a second rasterizer or a system fontconfig and the L1 rasterizer
stays authoritative.

This is the layering the crate is chosen for: `cosmic-text` is a complete,
renderer-agnostic text front end, while rasterization stays with the existing
L1 `FontSystem`/`rasterize_glyph` contract. `rustybuzz`/`harfrust` alone is a
shaper with no bidi, line breaking or font discovery; `swash` bundles shaping
*and* rasterization and would duplicate the L1 rasterizer; `glyphon` is a
`wgpu` integration that would pull a render backend into L2; `vello` is a GPU
2D renderer, not a layout engine.

---

## What must stay custom

* **The `.tft` parser.** A TVP/KiriKiri-only format (`TVP pre-rendered
  font\x1a`, 20-byte char items, two RLE versions, `TVP65_255` upscaling). No
  third-party crate parses it, and it dominates the main-dialogue path.
* **The per-character draw semantics.** The game draws text through the
  `Layer.drawText`/`Font` natives, and the CPU coverage composite is the
  script-visible path (`getPixel` observes it).
* **Bitmap compositing.** Glyphs are blended CPU-side into the layer's
  `BitmapState`; no external crate owns that.

---

## The `swash` rasterizer swap (not adopted)

`swash` could replace only the L1 rasterizer while keeping the L2 layout and
the custom `.tft` path, buying TrueType hinting, stem darkening, subpixel
positioning and synthetic bold/oblique. It has no font discovery/fallback, so
it would slot in under the existing resolver. It is **not** adopted: the
current output already matches the reference engine's synthetic bold/italic,
so this is a quality choice, not a correctness fix, and it would pull the
Fontations stack into L1. It stays a contained, optional L1 change with
golden-image tests.

---

## Measurement

The crate comparison was made with a throwaway benchmark crate outside the
repository; those numbers described a pre-migration raster/layout shape and are
not retained here. There is no crate-comparison benchmark in the tree today:
the text cost that matters is the per-character CPU composite, and the
instruments for it are the start-up and slow-frame lines
([`performance.md`](performance.md)).
