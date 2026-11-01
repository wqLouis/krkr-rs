# `engine::text` — shaping, coverage, atlas and text projection

Source: `crates/engine/src/text.rs`, `crates/engine/src/font.rs`

The module delegates the hard parts:

* **Layout / shaping / bidi / line-breaking / font fallback** go to
  `cosmic-text` (harfbuzz shaping, `fontdb` discovery and fallback,
  `unicode-bidi`, `unicode-linebreak`).
* **Rasterization** is the L1 `render::FontSystem` contract — L1 owns the
  font and the coverage bitmap; **L2 owns the atlas** that packs those
  bitmaps.
* **GPU projection** is the L1 quad pipeline: `project_text` turns positioned
  glyphs into per-glyph textured quads against an L2-owned `GlyphAtlas`.

Script-visible text is drawn by compositing per-character coverage into the
layer's `BitmapState` (`rasterize_text_coverage` + `compose_text_into`), so
`getPixel` and every other readback observe the same pixels the reference
produces. The `GlyphAtlas` path rasterizes the same coverage once, packs it
into a texture, and `project_text` emits textured quads; it needs a live
`RenderPipeline`.

---

## 1. Responsibility

* Own the registered faces and the shaping system.
* Lay a string out and return positioned glyphs with their advances.
* Rasterize glyphs into reusable coverage masks, and blur them for the
  shadow pass.
* Pack glyphs into an atlas texture and project them into a `RenderScene`.
* Composite a text run into a bitmap.

---

## 2. State it owns

Everything is **explicit caller-owned state**; there are no globals, no
statics and no hidden caches.

| value | state |
|---|---|
| `TextEngine` | the L1 `FontSystem`, the `cosmic-text` shaping system, the font registry, the lazily built host font database, the memoized shaped layouts, the memoized per-character advances and the memoized missing system families |
| `FontRegistry` | the registered faces, the names they answer to, the registered-name order and the default face |
| `GlyphAtlas` | the atlas texture, its CPU mirror, the shelf packer, the glyph entry map and the pending GPU uploads |
| `TextCoverageCache` | the rasterized character masks and their blurred shadows |

The five L2 memos — the shaped layouts, the per-character advances, the
missing system families, the coverage masks and the atlas entries — share the
bound `MAX_TEXT_CACHE_ENTRIES` (1300) and evict least-recently-used.

---

## 3. Public API

### `TextEngine`

```rust
impl TextEngine {
    pub fn new() -> Self;
    pub fn set_system_font_dirs(&mut self, dirs: Vec<PathBuf>);
    pub fn add_font(&mut self, bytes: &[u8], index: u32) -> Result<FontId, TextError>;
    pub fn font_id(&self, family: &str) -> Option<FontId>;
    pub fn alias(&mut self, name: &str, id: FontId);
    pub fn registry(&self) -> &FontRegistry;
    pub fn default_face(&self) -> Option<FontId>;
    pub fn load_system_font(&mut self, family: &str) -> Result<FontId, TextError>;
    pub fn install_default_face(&mut self, codepoint: char) -> Result<FontId, TextError>;
    pub fn face(&self, id: FontId) -> Option<&Font>;
    pub fn family_name(&self, id: FontId) -> Option<&str>;
    pub fn has_glyph(&self, id: FontId, codepoint: char) -> bool;
    pub fn provider_for(&self, id: FontId, codepoint: char) -> Result<FontId, TextError>;
    pub fn measure_code_unit(&mut self, id: FontId, codepoint: char, style: &TextStyle)
        -> Result<f32, TextError>;
    pub fn font_metrics(&self, id: FontId, size_px: f32) -> Result<FontMetrics, TextError>;
    pub fn layout(&mut self, font: FontId, text: &str, style: &TextStyle,
                  wrap_width: Option<f32>) -> Result<TextLayout, TextError>;
    pub fn cached_layouts(&self) -> usize;
    pub fn rasterize_glyph(&self, font: FontId, codepoint: char,
                           style: &TextStyle) -> Result<GlyphBitmap, TextError>;
    pub fn glyph_advance(&mut self, font: FontId, codepoint: char, style: &TextStyle)
        -> Result<f32, TextError>;
    pub fn glyph_draw_rect(&self, font: FontId, codepoint: char, style: &TextStyle)
        -> Result<Option<(f32, f32, f32, f32)>, TextError>;
}
```

### The font registry

```rust
impl FontRegistry {
    pub fn new() -> Self;
    pub fn register(&mut self, face: FontFace) -> FontId;
    pub fn alias(&mut self, name: &str, id: FontId);
    pub fn by_name(&self, name: &str) -> Option<FontId>;
    pub fn face(&self, id: FontId) -> Option<&FontFace>;
    pub fn names(&self) -> &[String];
    pub fn list_names(&self) -> Vec<String>;
    pub fn default_name(&self) -> Option<&str>;
    pub fn default_face(&self) -> Option<FontId>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}
```

The registry is the **one** place that knows which faces exist:

* A face is registered under **every** family name its name table carries
  (the typographic family first, then the plain family, in every language the
  face declares), so a lookup by any of them finds it. The names are matched
  case-insensitively.
* The **default face** is the face registered under the **oldest** name — the
  first entry the registry ever got. A later registration does not move it.
* The **registered-name list** is newest first and leaves the oldest name
  out, so a registry that holds a single name reports an empty list. The
  reference's font list stops at the map's oldest entry for the same reason
  (`reference/cpp/core/visual/FontImpl.cpp:242-245`).
* An **alias** is a name a caller asked for (a storage path, a
  comma-separated candidate list). It resolves the face but stays out of the
  registered-name list.

### The per-character fallback

`provider_for` answers the face that draws one character: the requested face
when it maps the character, otherwise the default face when it maps it, and
otherwise `TextError::MissingGlyph` — a character no face carries has no
glyph, and the rasterizer is told so instead of drawing another character's
shape. `layout` records that face in every `LaidOutGlyph`, so the drawing
path draws each character with the face that carries it.

The **measurement** and the **drawing** follow different rules on purpose:

* `measure_code_unit` measures the **requested** face: a character the face
  maps measures the rasterizer's advance for it, and a character the face
  does not map measures the face's em box (the requested size in pixels). The
  face that would draw the character never contributes to a measurement
  (`reference/cpp/core/visual/LayerIntf.cpp:11702-11713`).
* The drawn advance is the drawn glyph's own advance, so it is the provider's
  (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:1367`).

`add_font` loads the same bytes into the L1 rasterizer **and** the shaping
database, so layout and rasterization agree on the face, and registers the
face under every family name its name table carries.

`load_system_font` resolves a family against the host's installed fonts
(case-insensitively, including the localized family names in the name table);
only the resolved face enters the shaping database. A family the host does
not provide is memoized, so a repeated query reports the same
`TextError::FontNotFound` without scanning the host font database again. The
memo holds misses only: a family the host does provide resolves on the first
query, a face registered afterwards is found by `font_id` before the memo is
consulted, and the memo is dropped when `set_system_font_dirs` rebuilds the
font database.

`install_default_face` registers the engine's default face: the first
installed face that maps `codepoint`. The composition root calls it before
any script runs, so the host's own face is the default and the per-character
fallback has a face to reach for. A registry that already holds a face keeps
it.

### The glyph atlas

```rust
impl GlyphAtlas {
    pub fn new(pipeline: &RenderPipeline, size: u32) -> Result<Self, TextError>;
    pub fn clear(&mut self);
    pub fn texture(&self) -> &Texture;
    pub fn pixels(&self) -> &[u8];
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn pending_len(&self) -> usize;
    pub fn glyph(&mut self, engine: &TextEngine, font: FontId, codepoint: char,
                 style: &TextStyle) -> Result<AtlasGlyph, TextError>;
    pub fn flush(&mut self, pipeline: &RenderPipeline, state: &mut RenderState)
        -> Result<(), TextError>;
    pub fn rasterize_layout(&mut self, pipeline: &RenderPipeline, state: &mut RenderState,
                            engine: &TextEngine, layout: &TextLayout, style: &TextStyle,
                            opacity: f32) -> Result<Vec<PositionedGlyph>, TextError>;
}
```

`rasterize_layout` uploads every new glyph of the layout in **one** batch,
so a line of N new glyphs costs one GPU upload rather than N.

### Coverage and compositing

```rust
pub fn rasterize_text_coverage(cache: &mut TextCoverageCache, engine: &TextEngine,
                               layout: &TextLayout, style: &TextStyle,
                               antialias: bool) -> Result<Vec<CoverageMask>, TextError>;
pub fn blurred_text_coverage(cache: &mut TextCoverageCache, engine: &TextEngine,
                              layout: &TextLayout, style: &TextStyle, antialias: bool,
                              blur_level: u8, blur_width: i32) -> Result<Vec<CoverageMask>, TextError>;
pub fn blur_coverage(mask: &CoverageMask, blur_level: u8, blur_width: i32)
    -> Result<CoverageMask, TextError>;
pub fn blit_coverage_into(bitmap: &mut BitmapState, clip: RectI, mask: &CoverageMask,
                          dest_x: i32, dest_y: i32, color: [u8; 4], opacity: u8) -> Option<RectI>;
pub fn compose_text_into(masks: &[CoverageMask], style: &TextStyle, origin: (f32, f32),
                          angle: f32, bitmap: &mut BitmapState, clip: RectI,
                          opacity: u8) -> Result<Option<RectI>, TextError>;
pub fn rasterize_run(backend: GlyphBackend, services: TextRasterServices<'_>,
                     layout: &TextLayout, style: &TextStyle, antialias: bool,
                     opacity: f32) -> Result<RasterizedRun, TextError>;
pub fn project_text(scene: &mut RenderScene, parent: RenderNodeId, atlas: &GlyphAtlas,
                    glyphs: &[PositionedGlyph], start_z: i32) -> Vec<RenderNodeId>;
pub fn compose_text_cpu(atlas: &GlyphAtlas, glyphs: &[PositionedGlyph],
                        width: u32, height: u32) -> Vec<u8>;
```

### Types

`FontId`, `TextStyle`, `TextLayout`, `LaidOutGlyph`, `CoverageMask`,
`PositionedGlyph`, `AtlasGlyph`, `TextRasterServices`, `RasterizedRun`,
`TextError`.

---

## 4. Invariants

* **The layout is the single source of truth for the glyph positions.** The
  `cpu` and `gpu` backends are built from the same `TextLayout`, so the
  advances and metrics agree; only the rasterization and the draw path
  differ. Every script-visible measurement stays in VM pixels.
* **A line's baseline is the face's ascent truncated to whole pixels.** The
  ink box top is `baseline - bitmap_top`; a fractional or vertically centred
  baseline moves every glyph off its row (`reference/cpp/core/visual/FreeType.cpp:494`,
  `reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:911-914`).
* **The colour is not part of the atlas cache key.** The atlas stores
  colour-independent coverage and the colour is applied as a per-node tint,
  so recolouring reuses the cached glyphs.
* **A memoized per-character advance is the rasterizer's own value.** The
  advance memo is keyed by face, character, size and synthetic style, so a
  repeated measurement returns exactly the value a fresh measurement
  returns.
* **A coverage mask is always internally consistent**: `coverage.len() ==
  width * height`, enforced by the constructor.
* **The cached coverage is pen-relative.** The same coverage is reused at any
  pen position; placing it adds the layout's `x`/`baseline_y` and shares the
  buffer instead of copying it.
* **Each glyph keeps its own mask**, so a caller can blur or transform one
  character without bleeding into its neighbours.
* **A blurred shadow is derived from the cached sharp coverage**, so a shadow
  never rasterizes a glyph a second time.
* **With `antialias == false` the coverage is thresholded** to fully
  transparent or fully opaque, matching the aliased coverage the CPU
  compositor blends.
* **The atlas packs glyphs left to right, top to bottom, one pixel apart**, to
  avoid bilinear bleeding; the CPU mirror stays coherent with the GPU so the
  same pixels can be composited for readback.
* **Eviction drops the map entry only.** The shelf packer never reclaims a
  cell, so `GlyphAtlas::clear` is the way to reset the pixel space.
* **A run whose non-whitespace characters all rasterize to no ink is an
  error**, not an empty composite: the resolved face cannot render them.

---

## 5. Data flow

```
L3 native (Layer.drawText / ImageFunction.drawText)
   │  a font face + a string + a style (all VM pixels)
   ▼
TextEngine::layout(font, text, style, wrap_width) ──► TextLayout
   │        (memoized by font + text + size + style + wrap width)
   ▼
rasterize_text_coverage(cache, engine, layout, style, antialias)
   │        → per-glyph 8-bit coverage masks (cached, pen-relative)
   ▼
compose_text_into(masks, style, origin, angle, bitmap, clip, opacity)
   │        → tinted source-over into the layer bitmap
   ▼
bitmap.mark_dirty() ──► engine::Engine::sync_dirty_bitmaps ──► upload_textures
```

The GPU path:

```
rasterize_run(GlyphBackend::Gpu, services, layout, style, antialias, opacity)
   │        → GlyphAtlas::rasterize_layout (fills the atlas, queues the uploads)
   ▼
GlyphAtlas::flush(pipeline, state)      → one upload batch
project_text(render_scene, parent, atlas, glyphs, start_z)
         → one RenderContent::Texture quad per glyph
```

---

## 6. Failure modes

| failure | behaviour |
|---|---|
| an unregistered `FontId` | `TextError::UnknownFont` |
| a zero, negative or non-finite size | `TextError::InvalidSize` |
| unusable font data or a missing collection index | `TextError::InvalidFont` (or `TextError::Glyph`) |
| no installed family and no CJK fallback | `TextError::FontNotFound` |
| the atlas has no free cell | `TextError::AtlasFull` — the caller must grow or recreate the atlas; it is never worked around |
| a coverage buffer that does not match its dimensions | `TextError::InvalidCoverage` |
| a blur that would produce an unallocatable mask | `TextError::MaskTooLarge` |
| the L1 rasterizer fails | `TextError::Glyph` |
| the backend rejects a texture | `TextError::Render` |
| a run with no ink for non-whitespace characters | `TextError::Glyph(MissingGlyph)` |

`compose_text_into` returns the union of the drawn glyph rectangles, or
`None` when nothing had ink; `blit_coverage_into` returns the clipped
destination rectangle that received ink, or `None`.

---

## 7. Current limits

* **Glyphs are rasterized at VM resolution**, not at `size · raster_scale()`.
  The visible difference at `s > 1` is softness — a rasterization
  approximation, not a script-observable difference, because every layout
  metric stays in VM units.

---

## 8. Tests

* `crates/engine/tests/text.rs` — the layout, the metrics, the coverage
  masks, the blur and the errors.
* `crates/engine/tests/text_render.rs` — the atlas, the projection and the
  CPU composite.
* `crates/engine/src/text.rs` `mod tests` — the unit-level invariants.

---

## 9. See also

| topic | document |
|---|---|
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the L1 font surface | [`arch/l1.md`](../../arch/l1.md) §3 |
| the bitmap the text is composited into | [scene.md](scene.md) |
| the archived protocol trail | [`docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) §6.5 |
