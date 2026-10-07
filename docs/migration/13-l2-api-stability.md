# L2 API stability (the middle-layer contract)

Goal: **freeze the `engine` (L2) public API** so we can optimise everything
below it (L1: Vulkan batching, GPU glyph runs, advanced blends, hw video decode,
XP3 caches, …) without ever touching L3 again. L1 changes must be invisible above
the `engine` boundary.

See [`02-target-architecture.md`](02-target-architecture.md) for the layer rules
and [`10-performance-opportunities.md`](10-performance-opportunities.md) for the
optimisations this freeze is meant to protect.

## 1. Dependency rule (what L3 may use)

- **L3 (`compat`) may depend on `api`, `engine`, `platform` (event/DTO types
  only) and `tjs`.** It must **not** call `render`, `media`, or `fs` directly —
  those are L1 and reach the game only *through* `engine`. (Verified today:
  `compat`'s non-dev deps are `api`, `engine`, `platform`, `tjs`, `thiserror`,
  `log`, `flate2`, `encoding_rs`.)
- **L1 changes must not change any `engine` type, signature, or documented
  behaviour.** If an optimisation needs a new L2 concept, that is an *additive*
  L2 change (§3), reviewed as such.

## 2. Stability policy

1. **Additive-only.** New functions/methods/variants are fine; removing or
   changing an existing public item requires an explicit decision entry (a `Dxx`
   in [`README.md`](README.md)).
2. **`#[non_exhaustive]` on public enums and option/state structs**, so we can
   add variants/fields later without a breaking change. (Most `engine` types
   currently lack it — see §5.)
3. **Explicit state, not methods on hidden globals** (D9). Public state values
   implement `Default`/`new`; mutators take `&mut self`.
4. **Typed errors** (D11). Each family owns one `#[non_exhaustive]` error enum
   (`TextError`, `SoundError`, `TimerError`, `VideoError`, `TransError`, …); no
   silent fallbacks, no hardcoded caps.
5. **No raw pointers or `usize` addresses in the stable surface.** Opaque handle
   newtypes, not `*mut`/`usize` (a TJS `buffer` property may *report* an address
   as a number, but the L2 API must not be built on raw pointers).
6. **No TVP-specific constants in L2.** `LT_*`, `TVP_*`, VK codes, `tTVP*` enums
   are KiriKiri semantics = **L3**. L2 keeps only generic concepts (`BlendMode`,
   generic flags). (Debt: `projection.rs` `LT_*`, `scene.rs` `LT_OPAQUE/ALPHA/…`,
   `sound.rs` `TVP_*`.)
7. **Docs on every public item** (`#![warn(missing_docs)]` is on); state
   invariants and units (physical px vs logical, ms vs frames) are documented.

## 3. The stable L2 surface (per module)

| Module | Stable public surface | Notes |
|---|---|---|
| `scene` | `Scene` + its methods; `WindowState`, `LayerState`, `BitmapState`, `FontState`, `Rect` | The layer system. Fields are currently `pub`; they should become accessor-backed (§5). |
| `projection` | `project_window`, `project_scene`, `ComposedLayer`, `compose_layer`, `resolve_blend`, `blend_mode_for` | `LT_*` constants belong in L3. |
| `layer_ops` / `raster` | the pixel operations (`blit_*`, `fill_*`, `blur_*`, `affine_blit`, `fill_polygon`, …) | Pure transforms over `BitmapState`. `GammaAdjust` fields are public-by-design (a plain parameter block). |
| `storage` | `Storage` + `mount_archives`/`mount_archive`/`find`/`read`/`stat`/`exists`/`get_file_list`/`add_auto_path`/`extract…`; `discover_startup` | The VFS namespace + TVP addressing. |
| `startup` | `discover_startup` | |
| `sound` | `Mixer`, `SoundSource`, `SoundChannel`, `SoundBuffer`, `SliInfo`, `WaveFlags`, `WaveLabel`, `mix`/`mix_source`, `parse_sli` | Pure PCM state; L1 device consumes. `TVP_*` constants move to L3. |
| `text` | `TextEngine`, `GlyphAtlas`, `TextLayout`, `PositionedGlyph`, `TextStyle`, `project_text`, `compose_text_cpu`, `TextError` | L1 raster/atlas swap must not change this. |
| `input` | `InputState`, `ButtonState`, `KeyState`, `TouchContact` | Generic; no VK codes. |
| `timer` | `TimerState`, `TimerId`, `TimerConfig`, `TimerMode`, `TimerFire`, `TimerError` | Generic firing; TVP dispatch mode stays L3. |
| `video` | `VideoOverlay` + enums (`PlayStatus`, `VideoEvent`, `OverlayBinding`, `VideoBounds`, `PeriodReason`, `VideoError`) | Consumes frames from the caller; no codec dep. |
| `trans` | `Trans`, `TransKind`, `TransStep`, `RuleMask`, `TransError` | Over `layer_ops`. |

## 4. What the freeze protects

The optimisations in `10-performance-opportunities.md` are all *below* this
surface:

- GPU glyph runs / MSDF → implemented in `render` + the L2 `text` internals; the
  `text` API is unchanged.
- `VK_EXT_blend_operation_advanced` → `render` blend selection; `BlendMode` and
  `resolve_blend` are unchanged.
- hw video decode / streaming `video_audio` → `media` + the L2 `video` internals;
  `VideoOverlay` is unchanged.
- coverage-cache/tint (done), layout cache (done) → `text` internals only.
- XP3 decompress cache / mmap → `fs` + `engine::storage` internals; the `Storage`
  API is unchanged.

## 5. Known debt to stabilise (tracked)

1. **Public state fields** on `WindowState`/`LayerState`/`BitmapState`
   (`scene.rs`) — make private + accessors, or mark `#[non_exhaustive]` and treat
   as read-mostly. L3 currently reads/writes some directly.
2. **Raw `usize` handles**: `main_image_pixel_buffer*` / `province_image_pixel_buffer*`
   return addresses for the TJS `buffer`/`bufferForWrite` properties. Either keep
   them documented as *opaque addresses* (never dereferenced in Rust) or expose a
   safe `with_*_pixels` only.
3. **TVP constants in L2** (`LT_*` in `projection.rs`/`scene.rs`, `TVP_*` in
   `sound.rs`) — move to L3 or wrap behind `compat` names.
4. **`#[non_exhaustive]`** missing on ~all public enums/structs — add it so future
   growth is additive.
5. **Naming**: settle `SoundChannel` vs `Channel`, `VideoOverlay` methods vs the
   reference `vo_*`, etc.

## 6. Guard (proposed)

Add a CI check that fails on an *unreviewed* L2 public-API change. Options, in
order of effort: (a) a checked-in `cargo public-api` snapshot of `-p engine`;
(b) a nightly `rustdoc --output-format json` snapshot diff; (c) a
`docs/migration/l2-api-snapshot.txt` regenerated by a script and diffed. Until
then, this document is the contract and §5 is the debt list.
