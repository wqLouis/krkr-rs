# L2 (`crates/engine`) public-API stability audit

Audit of the `engine` (L2) public surface against
[`../13-l2-api-stability.md`](../13-l2-api-stability.md) (§1 dependency rule,
§2 stability policy, §3 stable surface, §5 debt).

* **Method**: read-only review of `crates/engine/src/*.rs`; `cargo check -p
  engine -p compat -p vm -p app` (passes; only pre-existing warnings). No
  long-running jobs. Line numbers are from the current working tree
  (uncommitted; `git log -1` = `e1642f4`, with 31 modified files) — re-check
  before applying.
* **Consumers verified**: L3 `crates/compat/src/**`, L4 `crates/vm/src/**`,
  and the composition root `crates/app/src/**` (L5-ish, included because it is
  a real caller of the frozen seam).

---

## 0. Executive summary

| Metric | Value |
|---|---|
| Public structs + enums in `engine` | **65** |
| With `#[non_exhaustive]` | **1** (`TextError` only) → **64 missing** |
| Public `usize` address APIs | 4 (`*_pixel_buffer*`) |
| Safe `with_*_pixels` closures | 4, **all unused by L3** |
| TVP-specific public constants | **29 `LT_*` + `TVP_*`/`ATTR_*`/`COPY_*`/`PHASE_MAX`** |
| Public error enums | 8 (7 missing `#[non_exhaustive]`) |
| `engine::text` items used by L3/L4/app | **0** |
| `engine::sound`/`video`/`trans`/`raster`/`layer_ops` items used by L3 | **0** (only doc links) |

L3's real use of L2 is narrow: `Scene` + `{Window,Layer,Bitmap,Font}State` +
`Rect`, `Storage`/`Location`/`StorageMetadata`, `input::InputState`,
`timer::{TimerState,TimerConfig,TimerId}`. L4 adds `input::InputState` and
`timer::TimerState`. App adds `project_scene`, `Scene`, `Storage`,
`LT_ALPHA/LT_OPAQUE`.

Three structural problems dominate:

1. **L1 types leak into L2 public signatures** (`text` embeds `render`, `storage`
   embeds `fs`) — directly violates §1 ("L1 changes must not change any `engine`
   type, signature") and §4. Both modules are unused by L3, so they can be
   redesigned **now with zero L3/L4 breakage**.
2. **`#[non_exhaustive]` is absent on 64 of 65 types** — the single cheapest,
   lowest-breakage fix, because for structs it still permits external field
   access (only literal construction/functional-update is blocked).
3. **The whole family surface is frozen flat** (`lib.rs` re-exports every
   `layer_ops`/`raster` op plus 29 `LT_*` constants), so the documented §3
   contract is far narrower than the actual public ABI.

### Priority order (severity × low breakage)

Apply top-to-bottom. Breakage is stated per finding with the exact L3/L4 site.

| # | Finding | Severity | L3/L4 breakage |
|---|---|---|---|
| F-01 | `engine::text` embeds L1 `render` types | Critical | none (0 refs) |
| F-02 | `engine::storage` embeds L1 `fs` types | High | none in L3 src; app/compat **tests** only |
| F-03 | `#[non_exhaustive]` missing on 19/20 enums | High | none except `Location` (see F-13) |
| F-04 | `#[non_exhaustive]` missing on state/option structs | High | none except `Rect`/`FontState` (F-12) |
| F-05 | Error enums inconsistent (only `TextError` non-exh.) | High | none |
| F-06 | Raw `usize` pixel-buffer handles; safe closures unused | High | 4 sites (type change) |
| F-07 | TVP `LT_*`/`TVP_*`/`ATTR_*`/`COPY_*` constants in L2 | Medium | none in L3 src; app **test** only |
| F-08 | Dead/extra public surface | Medium | none (engine tests only) |
| F-09 | Flat crate-root re-export freezes ops beyond §3 | Medium | none |
| F-10 | Physical vs logical px undocumented | Medium | docs-only fix: none; type fix: L3 |
| F-11 | `advance` units disagree (s vs ms vs abs-ms) | Medium | `TimerState::advance` rename → L3:4904 |
| F-15 | `WaveFlags::as_slice` type names `TVP_WL_MAX_FLAGS` | Low | none |
| F-16 | `TimerId(pub u64)` public tuple field | Low | none |
| F-12 | `Rect`/`FontState` cannot take `#[non_exhaustive]` as-is | Medium | L3:2435, L3:3661, app tests |
| F-13 | `Location` `#[non_exhaustive]` needs L3 wildcard | Medium | L3 storages.rs:210-221 |
| F-14 | `projection` returns L1 `RenderScene`/`Texture` | Medium | app only |
| F-17a | Privatize fields L3 never touches | Medium | none |
| F-17b | Privatize fields L3 reads/writes directly | High | ~40+ sites |
| F-18 | Naming (`Rect` vs `VideoBounds`, `SoundChannel`, two `InputState`) | Medium | renames break L3 |

---

## 1. API inventory

### 1.1 `#[non_exhaustive]` inventory (all 65 public types)

`#[non_exhaustive]` present on **exactly one** item. "literal?" = an external
crate constructs it with a struct literal / functional update today (so adding
the attribute would break).

**Enums (20)** — all `no` except `TextError`:

| Type | Location | `#[non_exhaustive]` |
|---|---|---|
| `Location` | storage.rs:73 | no |
| `MountError` | storage.rs:100 | no |
| `StorageReadError` | storage.rs:114 | no |
| `StartupScript` | startup.rs:39 | no |
| `StartupError` | startup.rs:62 | no |
| `TextError` | text.rs:85 | **YES** |
| `TimerMode` | timer.rs:33 | no |
| `TimerError` | timer.rs:75 | no |
| `ScrollFrom` | trans.rs:55 | no |
| `ScrollStay` | trans.rs:68 | no |
| `TransKind` | trans.rs:79 | no |
| `TransError` | trans.rs:194 | no |
| `SoundError` | sound.rs:55 | no |
| `LoopCondition` | sound.rs:95 | no |
| `Status` | sound.rs:645 | no |
| `SoundEvent` | sound.rs:668 | no |
| `PlayStatus` | video.rs:50 | no |
| `PeriodReason` | video.rs:77 | no |
| `VideoEvent` | video.rs:102 | no |
| `VideoError` | video.rs:170 | no |

**Structs (45)** — all `no`:

| Type | Location | literal by L3/L4? |
|---|---|---|
| `ButtonState` | input.rs:43 | no |
| `KeyState` | input.rs:73 | no |
| `TouchContact` | input.rs:87 | no |
| `InputState` | input.rs:102 | no |
| `GammaAdjust` | layer_ops.rs:1374 | no — **public-by-design parameter block, exempt** |
| `ComposedLayer` | projection.rs:167 | no |
| `WindowState` | scene.rs:19 | no |
| `LayerState` | scene.rs:56 | no |
| `BitmapState` | scene.rs:187 | no |
| `FontState` | scene.rs:232 | **YES** — `visual.rs:3661` functional update |
| `Rect` | scene.rs:256 | **YES** — `visual.rs:2435`, `app/tests/common/mod.rs:23,38` |
| `Scene` | scene.rs:269 | no |
| `BitmapCache` | scene.rs:1642 | no |
| `CachedImage` | scene.rs:1649 | no |
| `LoopLink` | sound.rs:131 | no |
| `WaveLabel` | sound.rs:171 | no |
| `SliInfo` | sound.rs:180 | no |
| `WaveFlags` | sound.rs:387 | no |
| `SoundSource` | sound.rs:445 | no |
| `Fade` | sound.rs:590 | no |
| `SoundChannel` | sound.rs:687 | no |
| `SoundBuffer` | sound.rs:1014 | no |
| `Mixer` | sound.rs:1156 | no |
| `MixSettings` | sound.rs:1327 | no |
| `StartupScripts` | startup.rs:27 | no |
| `Storage` | storage.rs:60 | no |
| `StorageMetadata` | storage.rs:87 | no |
| `FontId` | text.rs:119 | no |
| `TextStyle` | text.rs:139 | no |
| `LaidOutGlyph` | text.rs:192 | no |
| `TextLayout` | text.rs:209 | no |
| `TextEngine` | text.rs:258 | no |
| `AtlasGlyph` | text.rs:490 | no |
| `GlyphAtlas` | text.rs:516 | no |
| `PositionedGlyph` | text.rs:732 | no |
| `TimerId` | timer.rs:22 | no |
| `TimerConfig` | timer.rs:42 | no |
| `TimerFire` | timer.rs:66 | no |
| `TimerState` | timer.rs:102 | no |
| `RuleMask` | trans.rs:105 | no |
| `TransStep` | trans.rs:237 | no |
| `Trans` | trans.rs:250 | no |
| `VideoBounds` | video.rs:116 | no |
| `OverlayBinding` | video.rs:146 | no |
| `VideoOverlay` | video.rs:232 | no |

> Confirmed by experiment: on a `#[non_exhaustive]` struct with `pub` fields,
> an external crate can still **read and write** the fields; only
> `Type { .. }` construction, functional update, and exhaustive destructuring
> are rejected. So `WindowState`/`LayerState`/`BitmapState`/`Scene`/`Storage`
> get future-proofing for free.

### 1.2 Public fields on state structs

| Struct | Public fields | L3 direct access? |
|---|---|---|
| `WindowState` scene.rs:21-36 | `id,title,inner_size,visible,opacity,primary_layer,focused_layer,layers` | **yes**, all: `visual.rs:925,998,1271,1295,1320,1346,1369,1394,1417,1442,1548,1578,1646,1869,3307`, `system.rs:412` |
| `LayerState` scene.rs:58-182 | `id,window,parent,children,bitmap,rect,visible,opacity,blend_type,z_order,fill_color,image_left,image_top,image_width,image_height,province,province_width,province_height,hit_threshold,hit_type,cursor,face,clip,hold_alpha,focusable,join_focus_chain,focus_work,enabled,name,attention_left,attention_top,use_attention,cached,hint,show_parent_hint,ignore_hint_sensing,ime_mode,neutral_color,absolute_order_mode,call_on_paint,image_modified,font_id,is_primary,pending_paint,shutdown` | **yes**, ~35 of them via the property macro `visual.rs:2692-2906` + setters `visual.rs:675-684,701-703,881,2039-2040,2127-2131,2272-2274,2399,2435,2602,3060` |
| `BitmapState` scene.rs:189-213 | `id,width,height,rgba,name,dirty,screen_buffer,script_owned` | **yes**: `rgba` `visual.rs:2214,2225,2264,2593,3869,3886-3889,3912-3916,3985,3994,4157,4188`; `dirty` `:2226`; `script_owned` `:3727`; `width/height` `:2214`; app `lib.rs:415` |
| `FontState` scene.rs:234-251 | `id,face,height,color,bold,italic,strikeout,underline,angle` | **yes**: `visual.rs:3653,3661,4448-4490,4510-4526` |
| `Rect` scene.rs:258-264 | `x,y,w,h` | **yes** literal `visual.rs:2435`; app `common/mod.rs:23,38` |
| `Scene` scene.rs:272-279 | `windows,layers,bitmaps,fonts` (plus private `next_*, *_index, revision`) | **yes**: `scene.windows` `visual.rs:1548,1868`, `system.rs:410`; `scene.fonts` `visual.rs:3660,4274,4352`; app `scene.bitmaps` `lib.rs:415`, `scene.layers` `tests/host.rs:81,87` |
| `ComposedLayer` projection.rs:170-175 | `rect,opacity,visible` | no |
| `GammaAdjust` layer_ops.rs:1376-1392 | `r_gamma,r_floor,r_ceil,g_*,b_*` | no (public-by-design) |
| `LoopLink` sound.rs:133-143 | `from,to,smooth,condition,ref_value,cond_var` | no |
| `WaveLabel` sound.rs:173-175 | `position,name` | no |
| `SliInfo` sound.rs:182-184 | `links,labels` | no |
| `Fade` sound.rs:592-600 | `from,to,duration,delay,elapsed` | no |
| `MixSettings` sound.rs:1329-1331 | `global_volume,focus_gain` | no |
| `TimerConfig` timer.rs:44-50 | `interval_ms,enabled,mode,capacity` | read via `TimerConfig::default()` only |
| `TimerFire` timer.rs:68-70 | `id,count` | read (`visual.rs:4905`) |
| `StorageMetadata` storage.rs:89-95 | `size,modified,accessed,created` | read `storages.rs:608` |
| `StartupScripts` startup.rs:29-34 | `patch,startup,after_startup` | no |
| `VideoBounds` video.rs:118-124 | `left,top,width,height` | no |
| `OverlayBinding` video.rs:148-154 | `layer,window,alpha,visible` | no |
| `TextStyle` text.rs:141-151 | `size_px,bold,italic,color` | no |
| `LaidOutGlyph` text.rs:194-204 | `codepoint,font,line,x,baseline_y,advance` | no |
| `TextLayout` text.rs:211-217 | `glyphs,width,height,line_height` | no |
| `AtlasGlyph` text.rs:492-500 | `region,bearing_x,bearing_y,advance,size_px` | no |
| `PositionedGlyph` text.rs:734-742 | `glyph,x,baseline_y,opacity,color` | no |
| `ButtonState` input.rs:45-55, `KeyState` input.rs:75-79, `TouchContact` input.rs:89-93 | public | no |
| `CachedImage` scene.rs:1651-1655, `BitmapCache` scene.rs:1644 | `by_name`, `width,height,rgba` | no |

### 1.3 Raw `usize`/address surface

| API | Location | L3 use |
|---|---|---|
| `Scene::main_image_pixel_buffer(u32) -> usize` | scene.rs:559 | `visual.rs:3089` |
| `Scene::main_image_pixel_buffer_for_write(u32) -> usize` | scene.rs:570 | `visual.rs:3117` |
| `Scene::province_image_pixel_buffer(u32) -> usize` | scene.rs:583 | `visual.rs:3171` |
| `Scene::province_image_pixel_buffer_for_write(u32) -> usize` | scene.rs:593 | `visual.rs:3199` |
| `Scene::main_image_pitch` / `province_image_pitch` | scene.rs:536 / 547 | `visual.rs:3144` / `3226` |
| `Scene::with_main_image_pixels{,_mut}` | scene.rs:610 / 621 | **0** |
| `Scene::with_province_pixels{,_mut}` | scene.rs:634 / 642 | **0** |

Also `BitmapState::pixel_offset -> usize` (scene.rs:224, offset not address — OK)
and `text::FontId::index -> usize` (text.rs:123 — index, OK). `TimerId(pub u64)`
(timer.rs:22) is a public tuple field (see F-16).

### 1.4 TVP-specific constants/types in L2

| Symbol(s) | Location | Note |
|---|---|---|
| `LT_OPAQUE/ALPHA/ADDITIVE/SUBTRACTIVE` | scene.rs:40-46 | re-exported at `lib.rs:49-52` |
| `LT_BINDER…LT_PS_EXCLUSION` (24) | projection.rs:46-94 | re-exported at `lib.rs:59-66` |
| `TVP_VOLUME_SCALE`, `TVP_WL_MAX_FLAGS`, `TVP_WL_MAX_FLAG_VALUE` | sound.rs:38,42,45 | `TVP_WL_MAX_FLAGS` also appears in the public signature sound.rs:423 |
| `ATTR_DIRECTORY`, `ATTR_NORMAL` | storage.rs:41,43 | Windows/TVP file attributes |
| `COPY_MAIN`, `COPY_MASK` | layer_ops.rs:38,41 | reference `TVP_BB_COPY_*` |
| `DEFAULT_CLICK_SEQUENCE_FRAMES`, `DEFAULT_CLICK_MAX_MOVE` | input.rs:37,39 | TVP click policy defaults |
| `PHASE_MAX` | trans.rs:51 | `tTVPCrossFadeTransHandler::PhaseMax` |
| `ScrollFrom`, `ScrollStay` | trans.rs:55,68 | `tTVPScrollTransFrom`/`tTVPScrollTransStay` |
| `PlayStatus::as_str`, `PeriodReason::code`, `Status::as_str` | video.rs:64-74, 90-98; sound.rs:657-665 | TVP protocol strings/ints |
| `LayerState::hit_type/face/cursor/ime_mode/blend_type` | scene.rs:75,109,112,115,154 | raw TVP enum codes |

### 1.5 Error enums

| Error | Location | `#[non_exhaustive]` | Derives | Payload style |
|---|---|---|---|---|
| `TextError` | text.rs:83-117 | **yes** | `Debug, thiserror::Error` | mixed: `UnknownFont(usize)`, `InvalidSize(f32)`, `InvalidFont{message}`, `AtlasFull{..}`, `#[from]` L1 errors |
| `SoundError` | sound.rs:54-92 | no | `Debug, PartialEq, Eq, Error` | `Sli{message}`, `InvalidSpec{message}`, `UnsupportedChannels{..}`, `InconsistentBuffer{..}`, `NoSource` |
| `TimerError` | timer.rs:74-99 | no | `Clone, Copy, Debug, PartialEq, Eq, Error` | `UnknownTimer(TimerId)`, unit variants |
| `VideoError` | video.rs:169-227 | no | `Debug, PartialEq, Error` | `InvalidSpec{message}`, `NotOpen`, `InconsistentFrame{..}`, `InvalidSegment{..}`, `InvalidPlayRate{rate}` |
| `TransError` | trans.rs:193-233 | no | `Debug, PartialEq, Error` | `InvalidDuration{duration_ms}`, `InvalidVague`, `MissingRule`, `UnexpectedRule`, `InconsistentRule{..}`, `SizeMismatch{..}` |
| `StartupError` | startup.rs:61-84 | no | `Debug, Error` | `NoStartupScript`, `#[from] FsError`, `#[from] StorageReadError` |
| `MountError` | storage.rs:99-111 | no | `Debug, Error` | `NotADirectory(String)`, `#[from] FsError`, `#[from] Xp3Error` |
| `StorageReadError` | storage.rs:113-135 | no | `Debug, Error` | `NotFound(String)`, `#[from] FsError`, `#[from] Xp3Error` |

No L3/L4 source names an `engine` error type (`grep` over `crates/compat/src`,
`crates/vm/src` = 0); L3 forwards them via `Display`/`to_string()`.

### 1.6 Naming / unit inventory

| Concern | Evidence |
|---|---|
| Time base | `SoundChannel::advance(dt: f64)` seconds sound.rs:940; `Mixer::advance` sound.rs:1217; `Mixer::advance_to(now)` seconds sound.rs:1228; `VideoOverlay::advance(dt_ms)` video.rs:678; `TimerState::advance(now_ms: u64)` timer.rs:275 |
| Position unit | `SoundChannel::position_seconds` sound.rs:795; `VideoOverlay::position_ms` video.rs:433; `TimerState::next_fire_ms` timer.rs:200; `Trans::elapsed_ms` trans.rs:368 |
| Rectangle shape | `Rect{x,y,w,h}` scene.rs:256 vs `VideoBounds{left,top,width,height}` video.rs:116 |
| Status type | `sound::Status` (unload/stop/play) sound.rs:645 vs `video::PlayStatus` (unload/stop/play/pause) video.rs:50 |
| Channel type | `SoundChannel` sound.rs:687 (doc §5 debt: settle `SoundChannel` vs `Channel`) |
| Input type collision | `engine::input::InputState` (engine) vs `api::host::InputState` (`crates/api/src/host.rs:123`); app imports the `api` one (`app/src/lib.rs:29`) |
| Integer width for enum-coded fields | `LayerState::blend_type: i64` scene.rs:75 vs `hit_type/face/cursor/ime_mode: i32` scene.rs:109-154 |
| Physical vs logical px | `InputState::pointer` "physical window pixels" input.rs:223; `WindowState::inner_size` "Game logical size in pixels" scene.rs:25; `touch_velocity` "physical pixels" input.rs:504; `Rect` "window coordinates" scene.rs:256 |

### 1.7 Dead / extra public surface (L3/L4/app never reference)

| Item | Location | Status |
|---|---|---|
| `BitmapCache`, `CachedImage` | scene.rs:1642,1649 (re-exported lib.rs:50) | 0 external refs; not in §3 contract |
| `projection::BACKGROUND_CLEAR` | projection.rs:101 (lib.rs:64) | 0 external refs; internal only |
| `projection::clamp_opacity` | projection.rs:107 (lib.rs:64) | 0 external refs (engine tests only); `render` has its own private copy |
| `projection::visible_image_region` | projection.rs:239 (lib.rs:65) | 0 external refs (engine tests only) |
| `layer_ops::blend_pixel_mode` | layer_ops.rs:1725 | pub, **not** in the `lib.rs:36-44` re-export list, 0 external refs |
| `Storage::file_system`, `for_each_archive`, `archive_paths`, `unmount_archive`, `set_auto_paths`, `auto_paths`, `relative_disk_name` | storage.rs:168,188,183,232,285,290,379 | 0 direct external refs; `mount_archive` (199) only via `mount_archives` |
| `startup::{discover_startup,StartupScripts,StartupScript,StartupError}` | startup.rs:27-86 | 0 external refs, but **explicitly in §3** ("intended contract") — keep, just note unimplemented caller |
| `input::{DEFAULT_CLICK_SEQUENCE_FRAMES,DEFAULT_CLICK_MAX_MOVE}` | input.rs:37,39 | 0 external refs; L3 defines its own `MOUSE_CLICK_*` (`visual.rs:1388`) |
| `sound::MixSettings` | sound.rs:1327 | required parameter type of public `mix`, not dead |

---

## 2. Findings (ordered by severity × low breakage)

### F-01 — `engine::text` public API embeds L1 `render` types — **Critical**, zero L3/L4 breakage

**Where**: `crates/engine/src/text.rs:541,561,595-598,668-671,335,344,768-774`;
imports at `text.rs:68-70`.

**Problem**: The §4 promise is that a GPU glyph-run/atlas swap is invisible
above `engine`, but the public text API is built directly on the concrete L1
renderer:

* `GlyphAtlas::new(pipeline: &RenderPipeline, size: u32)` (text.rs:541)
* `GlyphAtlas::glyph(pipeline: &RenderPipeline, state: &mut RenderState, …)`
  (text.rs:595-598)
* `GlyphAtlas::rasterize_layout(pipeline: &RenderPipeline, state: &mut RenderState, …)`
  (text.rs:668-671)
* `GlyphAtlas::texture(&self) -> &Texture` (text.rs:561)
* `TextEngine::face(&self, …) -> Option<&render::Font>` (text.rs:335)
* `TextEngine::font_metrics(…) -> Result<render::FontMetrics, TextError>` (text.rs:344)
* `project_text(scene: &mut RenderScene, parent: RenderNodeId, …) -> Vec<RenderNodeId>`
  (text.rs:768-774)

Any change to `render::RenderPipeline`/`RenderState`/`Texture`/`Font`/
`FontMetrics`/`RenderScene`/`RenderNodeId` is therefore an L2 breaking change,
contradicting §1. The doc's claim that "the `text` API is unchanged" is
currently false.

**Fix**: Keep only pure, renderer-free values in L2 (`TextEngine` layout
results, `TextStyle`, `TextLayout`, `LaidOutGlyph`, `PositionedGlyph`), and move
`GlyphAtlas`/`project_text`/`compose_text_cpu` behind an L2-owned handle or down
into L1. Replace `&RenderPipeline`/`&mut RenderState` with an opaque L2
`TextRasterizer` state, and `&Texture` with an opaque `GlyphAtlasTexture`.

**Breakage**: **none** — L3, L4 and app reference **0** `engine::text` items
(only `crates/engine/tests/text.rs` uses them). This is the safest large fix.

---

### F-02 — `engine::storage` public API embeds L1 `fs` types — **High**, L3-src-safe

**Where**: `crates/engine/src/storage.rs:140` (`mount_archives(fs: FileSystem,…)`),
`:168` (`file_system() -> &FileSystem`), `:188`
(`for_each_archive(&Xp3Archive)`), error `#[from]` at `:109,:123`; re-export
`crates/engine/src/lib.rs:35` (`fs::xp3::normalize_in_archive_name`).

**Problem**: `Storage` is §3 stable surface, but its signature names L1
`fs::FileSystem`, `fs::xp3::Xp3Archive` and `fs::xp3::Xp3Error`. "The `Storage`
API is unchanged" (§4) cannot hold if `fs` evolves. It also lets an L3 caller
reach L1 by value (`.file_system()`), against §1 ("L3 must not call `fs`").

**Fix**: Introduce an opaque L2 `StorageBackend` (wrapping `FileSystem`) and an
L2 `ArchiveInfo`; replace `for_each_archive`'s `&Xp3Archive` with L2 metadata;
wrap `Xp3Error` in a non-`#[from]` L2 variant (`Archive { message }`) so the L1
error type is not in the public enum. Keep `normalize_in_archive_name`
re-exported, but from an L2 module.

**Breakage**: **none in L3 src.** `mount_archives` is called only in tests:
`crates/app/tests/real_game.rs:39`, `crates/compat/tests/text.rs:76`,
`crates/engine/tests/common/mod.rs:46`. `file_system()`/`for_each_archive` have
0 external callers. L4: none.

---

### F-03 — `#[non_exhaustive]` missing on 19 of 20 public enums — **High**, zero breakage except `Location`

**Where**: table §1.1.

**Problem**: §2.2 requires it so variants can be added additively. Only
`TextError` has it. Every consumer must currently match exhaustively.

**Fix**: add `#[non_exhaustive]` to all enums in §1.1 **except** `Location`
(see F-13) and to the 7 other error enums (F-05).

**Breakage**: L3 matches engine enums in exactly one place —
`crates/compat/src/natives/storages.rs:210-221` on `Location` (handled by
F-13). No other L3/L4 code matches an `engine` enum. Within `engine` itself the
attribute is inert. So for `SoundError`, `LoopCondition`, `Status`,
`SoundEvent`, `PlayStatus`, `PeriodReason`, `VideoEvent`, `VideoError`,
`ScrollFrom`, `ScrollStay`, `TransKind`, `TransError`, `TimerMode`,
`TimerError`, `StartupScript`, `StartupError`, `MountError`, `StorageReadError`
the change is **non-breaking**.

---

### F-04 — `#[non_exhaustive]` missing on state/option structs — **High**, zero breakage for the safe subset

**Where**: all 45 structs in §1.1.

**Problem**: §2.2 wants future fields to be additive. `WindowState`/
`LayerState`/`BitmapState`/`Scene`/`Storage`/`TimerConfig`/`TextStyle`/… all
lack it.

**Fix**: add it to every struct **except**:
* `Rect` and `FontState` — constructed literally by L3/app tests (F-12);
* `GammaAdjust` — §3 says this parameter block is public-by-design.

The rest are non-breaking even though L3 reads/writes their fields, because
`#[non_exhaustive]` still permits field access from other crates.
`TimerConfig::default()` (L3 `visual.rs:4593`) keeps working.

**Breakage**: none for L3/L4. (If added to `Rect`, `visual.rs:2435` and
`app/tests/common/mod.rs:23,38` break; if added to `FontState`,
`visual.rs:3661` breaks. Both are excluded.)

---

### F-05 — Error enums are inconsistent — **High**, zero breakage

**Where**: §1.5.

**Problem**:
* Only `TextError` is `#[non_exhaustive]`; the other 7 can grow only by
  breaking callers.
* Payload style is mixed: `TextError::UnknownFont(usize)` uses a bare integer
  while `TimerError::UnknownTimer(TimerId)` uses the typed id; several variants
  carry `message: String` while others are structured.
* `TextError` lacks `PartialEq` (it contains `f32` and `String`, so
  `PartialEq` is derivable), unlike `SoundError`/`TimerError`/`VideoError`/
  `TransError`.
* `SoundError::UnsupportedChannels` hardcodes the mono/stereo cap (sound.rs:67-74),
  which conflicts with §2.4 "no hardcoded caps".
* `StartupError`/`MountError`/`StorageReadError` have no `PartialEq` and
  forward L1 errors (`#[from]`), which F-02 also argues against.

**Fix**: apply one shape: `#[non_exhaustive]`, a typed-id variant wherever an
id exists, `PartialEq` where possible, and a single L2 `message`/structured
convention. Change `TextError::UnknownFont(usize)` to `UnknownFont(FontId)`.

**Breakage**: none. L3/L4 never name or match `engine` error types (only
`Display` via `to_string()`); `TextError` is used only by `engine` tests.

---

### F-06 — Raw `usize` pixel-buffer handles; safe closures are dead — **High**, L3 breaks at 4 sites if the type changes

**Where**: `scene.rs:559,570,583,593`; safe alternatives `scene.rs:610,621,634,642`.
L3 use sites: `crates/compat/src/natives/visual.rs:3089,3117,3171,3199` (+ pitch
`:3144,3226`).

**Problem**: §2.5 forbids building the stable surface on raw pointers/`usize`
addresses. These are safe Rust (no `unsafe`) and are documented as opaque
addresses, but:
* L3 uses **only** the address form; the safe `with_*_pixels` closures have 0
  callers, so the risk isn't actually contained.
* L3 additionally bypasses them and pokes `BitmapState::rgba` directly
  (`visual.rs:2408,3617-3664,3905,3936`), including `as_mut_ptr() as i64`.
* The address goes stale after `Scene::resize_bitmap` (scene.rs:1299) /
  `resize_province_image` (scene.rs:672), because the `Vec`/`Box<[u8]>` is
  reallocated; the script can retain it.

**Fix**: add an opaque `#[non_exhaustive] pub struct PixelBufferAddress(usize)`
with `address() -> usize`, return it from the four methods, and mark the raw
`usize` form deprecated. Migrate L3 pixel I/O to `with_*_pixels*` so only the
single TJS `buffer`/`bufferForWrite` property reads the address. (If a
zero-break step is required first: keep `usize` but add `#[doc(hidden)]`-free
documentation of the invalidation contract and route at least the writes through
the closures.)

**Breakage**: changing the return type breaks L3 at the 4 sites above
(mechanical). Keeping `usize` breaks nothing. L4: none.

---

### F-07 — TVP constants/types in L2 — **Medium**, L3-src-safe

**Where**: §1.4.

**Problem**: §2.6 says `LT_*`/`TVP_*`/VK codes/`tTVP*` belong to L3; L2 should
keep only generic `BlendMode`/flags. Today 29 `LT_*` are re-exported at the
crate root (`lib.rs:49-66`), plus `TVP_VOLUME_SCALE`, `TVP_WL_MAX_FLAGS`,
`TVP_WL_MAX_FLAG_VALUE`, `ATTR_DIRECTORY/NORMAL`, `COPY_MAIN/MASK`, the TVP
click defaults, `PHASE_MAX`, and the `ScrollFrom/ScrollStay` enums.

**Fix**: make the `LT_*`/`COPY_*`/`ATTR_*`/`TVP_*` constants `pub(crate)`
(engine internals and `blend_mode_for` still need the numbering); expose L3
constants only from `compat` (it already defines them in
`crates/compat/src/natives/constants.rs:20`). Replace `ScrollFrom`/`ScrollStay`
with a generic `Direction`/`Stay` if the numeric mapping is kept in L3. Keep
`LayerState::blend_type` for now (it is the script-visible `layer.type`), but
type the generic blend concept as `BlendMode`.

**Breakage**: **none in L3 src** (`grep` for `LT_*`/`TVP_*`/`ATTR_*`/`COPY_*`
in `crates/compat` shows only L3-local names and doc links). The app **test**
`crates/app/tests/common/mod.rs:4,30,45` imports `LT_ALPHA`/`LT_OPAQUE` and
will need its own constants. L4: none.

---

### F-08 — Dead/extra public surface — **Medium**, zero breakage

**Where**: §1.7.

**Problem**: `BitmapCache`/`CachedImage` and the projection helpers are frozen
public surface that no consumer uses and §3 does not list. `blend_pixel_mode`
is public but is not even re-exported, so it is an inconsistent half-contract.

**Fix**: make `BitmapCache`, `CachedImage`, `clamp_opacity`,
`visible_image_region`, `BACKGROUND_CLEAR` and `blend_pixel_mode` `pub(crate)`
(or move the helpers into `render`, which already has its own `clamp_opacity`).
`startup::*` is explicitly in §3, so keep it and add the caller when game
loading lands.

**Breakage**: none for L3/L4/app; only `crates/engine/tests/projection.rs:15-16,
44-48,201-256` references the projection helpers.

---

### F-09 — Flat crate-root re-export freezes ops beyond §3 — **Medium**, zero breakage

**Where**: `crates/engine/src/lib.rs:35-66`.

**Problem**: `pub use {layer_ops::{…40 names…}, raster::{…}}` commits every
pixel primitive as stable ABI, while §3 describes the module as "the pixel
operations (…)". Adding one internal op is harmless, but *changing* one is a
breaking change that §3's wording does not clearly authorize. The same applies
to re-exporting 29 `LT_*` constants. §6's proposed `cargo public-api` guard
would make this explicit.

**Fix**: narrow the `lib.rs` re-export to the §3 contract and let callers use
`engine::layer_ops::…` for the rest (or keep the flat list but record it as the
reviewed snapshot). Add the §6 public-API snapshot check.

**Breakage**: none for L3/L4 (they do not import these); engine tests use the
flat names.

---

### F-10 — Physical vs logical pixels undocumented — **Medium**

**Where**: `input.rs:223-224` ("physical window pixels"), `scene.rs:25`
("Game logical size in pixels"), `input.rs:504` ("physical pixels"), `Rect`
`scene.rs:256` ("window coordinates"), `VideoBounds` `video.rs:116`
("window coordinates").

**Problem**: §2.7 requires units to be documented. Two coordinate spaces are in
play (host physical pixels vs game logical window coordinates) but there is no
`Scale`/`Logical`/`Physical` type and no single statement of which space each
API uses. `project_scene` mixes layer coordinates with the surface size. This
is a correctness hazard on HiDPI/Android.

**Fix**: document the contract on every geometry item and add newtypes
(`PhysicalPx`, `LogicalPx`) to the mutating input/size APIs; settle the scale
at the L1→L3 event boundary.

**Breakage**: a documentation-only pass breaks nothing. Adding newtypes to
`InputState`/`WindowState::inner_size`/`Rect` breaks L3 (`visual.rs` setters,
`common/mod.rs` literals) and app.

---

### F-11 — `advance`/position units disagree across families — **Medium**, rename breaks L3 timer

**Where**: §1.6.

**Problem**: `SoundChannel::advance(dt)` and `Mixer::advance/advance_to` take
**seconds**; `VideoOverlay::advance(dt_ms)` takes **ms**; `TimerState::advance`
takes an **absolute `now_ms: u64`**. The same name means three different things
across one API, and positions use `_seconds`/`_ms` inconsistently.

**Fix**: suffix the unit in every signature (`advance_seconds`,
`advance_ms`, `advance_to_ms`) and on every position getter/setter; document
the timer's absolute-vs-relative clock difference explicitly.

**Breakage**: renaming `TimerState::advance` breaks L3 at
`crates/compat/src/natives/visual.rs:4904` (and `any_enabled` at `:4949` is
unaffected). Sound/video/trans renames break nothing (0 L3 refs). L4: only
`TimerState` is used, and only `InputState`/`TimerState` types are named in
`crates/vm/src/host.rs:8-9`.

---

### F-12 — `Rect` / `FontState` cannot take `#[non_exhaustive]` as-is — **Medium**

**Where**: `Rect` scene.rs:256; `FontState` scene.rs:232.

**Problem**: L3 constructs `Rect { x, y, w, h }` at
`crates/compat/src/natives/visual.rs:2435` and app tests construct it at
`crates/app/tests/common/mod.rs:23,38`; L3 uses functional update
`FontState { id: destination, ..source }` at `crates/compat/src/natives/visual.rs:3661`.
Adding `#[non_exhaustive]` to those two types would break those sites.

**Fix**: for `Rect`, add constructor/`Default` + accessors and migrate the 3
literals, or accept it as a permanently-constructible value type (it is a plain
geometry struct, arguably `GammaAdjust`-like). For `FontState`, replace the
functional update with explicit field writes (or a `Scene::copy_font` method)
and then mark it non-exhaustive.

**Breakage**: L3 `visual.rs:2435` and `:3661` (mechanical, 2 sites); app tests
`common/mod.rs:23,38`.

---

### F-13 — `Location` `#[non_exhaustive]` requires an L3 wildcard — **Medium**, low breakage

**Where**: `storage.rs:73`; L3 match at `crates/compat/src/natives/storages.rs:210-221`
(and `Location::Archive(..)` read at `:581`).

**Problem**: `Location` is the only engine enum L3 matches exhaustively. Adding
the attribute uniformly (F-03) requires the L3 match to grow a `_ =>` arm.

**Fix**: add `#[non_exhaustive]` and update
`storages.rs:210-221` to `Ok(Some(Location::Disk(path)))` /
`Ok(Some(Location::Archive(..)))` / `_ =>` in the same change.

**Breakage**: L3 `storages.rs:210-221` (one site). L4: none.

---

### F-14 — `projection` returns/holds L1 `RenderScene`/`Texture` — **Medium**, app-only breakage

**Where**: `projection.rs:502-527` (`project_window`/`project_scene` return
`render::RenderScene`, take `FnMut(u32) -> Option<render::Texture>`);
`projection.rs:167` `ComposedLayer` is fine.

**Problem**: the §3 stable "projection" surface is expressed in L1 render types.
A `render` refactor changes the L2 signature. §4 says the projection is the
seam, so this may be deliberate, but it is not documented as an exception.

**Fix**: wrap the result in an opaque `ProjectedScene` and a `TextureHandle`;
or explicitly document `projection` as the one L1-coupled module and freeze the
`render` types it names.

**Breakage**: app calls `project_scene` and manipulates `RenderScene`
(`crates/app/src/lib.rs:30,199-210`). L3/L4: none.

---

### F-15 — `WaveFlags::as_slice` embeds the TVP constant in its type — **Low**, zero breakage

**Where**: `sound.rs:423` (`pub fn as_slice(&self) -> &[i32; TVP_WL_MAX_FLAGS]`).

**Problem**: the public signature's array length depends on `TVP_WL_MAX_FLAGS`;
moving that constant to L3 (F-07) would change the type.

**Fix**: return `&[i32]` and expose `len()`; keep the constant `pub(crate)`.

**Breakage**: none (sound has 0 L3/L4 refs); `engine` tests only.

---

### F-16 — `TimerId(pub u64)` public tuple field — **Low**, zero breakage

**Where**: `timer.rs:22`.

**Problem**: exposes the internal counter and allows constructing arbitrary
ids that collide with future allocations. A `value()` accessor already exists
(timer.rs:26).

**Fix**: make the field private, keep `value()` (and `From<u64>` if needed).

**Breakage**: none — L3/L4 never construct `TimerId(...)` (only `HashMap<TimerId,…>`
and option storage at `visual.rs:162,263`).

---

### F-17a — Privatize fields L3 never touches — **Medium**, zero breakage

**Where**: `LayerState::{province_width (scene.rs:100), province_height (102),
focus_work (133), shutdown (182), is_primary (169)}`,
`BitmapState::screen_buffer (scene.rs:207)`.

**Problem**: §5.1 wants state fields accessor-backed. These six are only read by
`engine` internals; L3 checks `l.province.is_some()` (visual.rs:639) and calls
`Scene::is_primary_layer` (scene.rs:498) instead.

**Fix**: make them private and add read accessors; they then need `Scene`
methods if L3 ever needs them. This is a safe first slice of the field-privacy
work.

**Breakage**: none (verified: no external references). L4: none.

---

### F-17b — Privatize the remaining public state fields — **High**, large L3 refactor

**Where**: every other `WindowState`/`LayerState`/`BitmapState`/`FontState`/
`Scene` field (§1.2).

**Problem**: these are §5.1's main debt: they leak internals and make invariants
unenforceable. But L3 treats them as a property bag — the ~40-pair getter/setter
macro at `crates/compat/src/natives/visual.rs:2692-2906` reads/writes
`LayerState` fields directly, `WindowState` is edited at ~15 sites, and
`BitmapState::rgba`/`dirty`/`script_owned` are written directly
(`visual.rs:2225-2226,2593,3727,3869-3916,4157,4188`). L3 also reads the public
`Scene` vectors (`scene.windows` `visual.rs:1548,1868`, `system.rs:410`;
`scene.fonts` `visual.rs:3660,4274,4352`). App does `self.scene.bitmaps`
(`app/src/lib.rs:415`) and `host.scene_mut().layers[0]` (`app/tests/host.rs:81,87`).

**Fix (staged)**: introduce `Scene::{add, set_*}`, `Window/Layer/Bitmap/Font`
accessor pairs and a `Scene::fonts_mut`/`bitmaps_mut` iterator, migrate L3 and
app, then make fields private. Because `#[non_exhaustive]` (F-04) already gives
future-field safety without the churn, schedule F-17b after the API snapshot
guard (F-09) so the migration is reviewable.

**Breakage**: L3 ~40+ sites (`visual.rs:2692-2906` plus the exact-field sites
in §1.2); app `lib.rs:415`, `tests/host.rs:81,87`. L4: none.

---

### F-18 — Naming inconsistencies — **Medium**, renames break L3

**Where**: §1.6.

**Problem**: `Rect` and `VideoBounds` are two rectangles with different field
spellings; `sound::Status` vs `video::PlayStatus` are parallel unload/stop/play
enums with different `Pause` handling; the doc itself flags `SoundChannel` vs
`Channel`; and there are two unrelated `InputState` types
(`engine::input::InputState` vs `api::host::InputState`, the latter imported by
app at `app/src/lib.rs:29`). `LayerState` mixes `i32` and `i64` for
enum-coded fields.

**Fix**: pick one rectangle (`Rect` with `left/top/width/height`, or keep `Rect`
and make `VideoBounds` a type alias/`From`); rename `Status` → `SoundStatus`
next to `VideoStatus`; settle `Channel` naming; rename the API DTO to
`InputSnapshot` to disambiguate from L2 `InputState`. Normalise the scalar width
of enum-coded fields.

**Breakage**: `Rect` is constructed in L3 `visual.rs:2435` and read throughout
`visual.rs:2704-2723`; app tests. Renames of `Status`/`SoundChannel` break no
L3 code today (sound unused) but block the future L3 audio worker. The
`InputState` rename is a cross-crate change (`api`, `engine`, `app`,
`compat`, `vm`) and should be scheduled with the platform snapshot work.

---

## 3. Quick-win checklist (safe first)

1. `#[non_exhaustive]` on 19 enums + 7 error enums (F-03/F-05), one enum
   (`Location`) handled with the L3 wildcard (F-13).
2. `#[non_exhaustive]` on the 43 structs that no external crate literal-constructs
   (F-04); leave `Rect`, `FontState`, `GammaAdjust`.
3. Make `BitmapCache`, `CachedImage`, `clamp_opacity`, `visible_image_region`,
   `BACKGROUND_CLEAR`, `blend_pixel_mode` `pub(crate)` (F-08).
4. Make the TVP constants `pub(crate)`; keep L3's own constants script (F-07).
5. `WaveFlags::as_slice -> &[i32]` (F-15); `TimerId` field private (F-16);
   privatize the six L3-unused state fields (F-17a).
6. Redesign `engine::text` behind opaque handles (F-01) and wrap `Storage`'s
   `fs` types (F-02) — both with zero L3/L4 breakage.
7. Add the §6 public-API snapshot (`docs/migration/l2-api-snapshot.txt` +
   `cargo public-api -p engine` diff) before any rename.

## 4. Guard (recommendation)

Implement §6(a): a checked-in `cargo public-api -p engine` snapshot diffed in
CI. Given the 65-type/29-constant surface and the F-09 flat re-export, a
snapshot is the only way to make "unreviewed L2 change" impossible; the current
doc-only contract has already drifted (e.g. `BitmapCache` and
`visible_image_region` are public but absent from §3).
