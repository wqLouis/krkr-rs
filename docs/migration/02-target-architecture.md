# 02 — Target architecture

Status: design + implemented, **revised** for the final layering (see §1.1). This document
defines the layer boundaries, the crate layout, the dependency rules and
where each responsibility lands. The stateless-object contract between L1
and L2 is in [`03-middle-layer-api.md`](03-middle-layer-api.md); the TJS/VM
compatibility split is in
[`08-tjs-vm-and-native-plugins.md`](08-tjs-vm-and-native-plugins.md); game
loading is in [`09-game-loading.md`](09-game-loading.md).

---

## 1. Four layers

```
┌──────────────────────────────────────────────────────────────────────┐
│ L4  TJS VM engine                                                     │
│     tjs (the C++ VM) + VM lifecycle, GC, bytecode, script cache. │
│     The topmost layer. Drives the L3 native surface; uses the L1      │
│     platform abstraction directly.                                    │
├──────────────────────────────────────────────────────────────────────┤
│ L3  KiriKiri compatibility surface                                    │
│     native classes (System, Storages, Scripts, Window/Layer/Bitmap/   │
│     Font/Timer, SoundBuffer, VideoOverlay, Plugins, …) + KAG parser.  │
│     Implements the script API by *composing* L2 primitives; adapts    │
│     L1 events into TJS-visible events. Thin glue, not an engine.      │
├──────────────────────────────────────────────────────────────────────┤
│ L2  Middle layer — a small, generic game engine                       │
│     the most fundamental APIs for drawing and playing something:      │
│     windows/surfaces, bitmaps/sprites, **the layer system** (a scene  │
│     graph), audio/video playback, input stream, timers, a virtual     │
│     file system over parsed archives. Not TVP-specific; owns state.   │
├──────────────────────────────────────────────────────────────────────┤
│ L1  Platform core                                                     │
│     renderer (Vulkan) · media engine (FFmpeg|MediaCodec) · file       │
│     system **+ archive parsing** · **window/host, event loop, input,  │
│     clock, threads, process lifecycle** · **all platform-specific     │
│     code**, behind one platform abstraction. Stateless objects +      │
│     explicit state (D9). Selected at compile time (§4).               │
└──────────────────────────────────────────────────────────────────────┘
```

Dependency direction is strictly downward, with exactly one extra edge:

```
L4  →  L3, L1
L3  →  L2, L1
L2  →  L1
L1  →  OS / Vulkan / FFmpeg / MediaCodec only
```

* `L4 → L1` exists because the VM host uses the platform abstraction
  (clock, threads, file access for bytecode/state) without going through
  the compat surface.
* **L1 must not depend on L2/L3/L4.** It knows nothing of layers, TVP, TJS,
  storages or KAG.
* **L2 must not depend on L3/L4** and must not contain TJS or TVP vocabulary.
* **L3 may not contain platform code** (`#[cfg(target_os=…)]`, winit,
  `android-activity`, `std::fs`, JNI); it uses L1 abstractions.

### 1.1 What changed from the earlier draft

| Concern | Earlier draft | Now |
|---|---|---|
| Host / window / event loop | top layer (old L4 `app`) | **L1** platform core |
| Platform-specific code | spread across top and backends | **confined to L1**, one abstraction |
| TJS VM | old L3 (`krkr-tjs`) | **L4**, above the compat surface |
| Compat surface (native classes + KAG) | old L3 | **L3**, composing L2 |
| L2 role | TVP semantics + layer system + all state | **a generic game engine** with fundamental APIs **including the layer system (scene graph)**; owns engine state, but no TVP semantics |
| Archive parsing | L2 (`engine::archive`) | **L1** (stateless read+parse); L2 sees a parsed FS |
| Native plugins | static Rust families + `Plugins.link` no-op success | **statically built into L3**, with a null-plugin object for missing DLLs (D14) |
| Backend selection | runtime flag during migration | **compile-time features/cfg** (D13) |

> Decision **D2 is revised**: the **layer system (the generic scene graph)
> lives in L2** — game engines hold scene graphs — while the *TVP semantics*
> that scripts see (masks, province, exact property names, storage naming)
> are L3, layered on top. The render pipeline remains L1 and consumes the
> scene tree (D10 unchanged).

## 2. Responsibilities

### 2.1 L1 — platform core (mechanism + host)

* **Stateless abstraction objects + explicit state (D9).** `RenderPipeline`,
  `Media`, `FileSystem`, `FontSystem`, `AudioDevice`, `Platform` are
  stateless services; every evolving value (`RenderState`, `FileState`,
  `DecoderState`, `SurfaceState`) is defined by L1 and stored by L2.
* **All platform code.** Windowing/surface (winit on Linux, the `ndk` window on
  Android; the `android-activity` GameActivity glue lives in the Android
  composition root, `crates/android`), the event loop, input, clock, threads,
  process lifecycle, JNI, SAF/AssetManager. Higher layers must not contain
  `#[cfg(target_os)]`.
* **The render pipeline.** Traversal, composition, batching, blend-state
  selection, upload and draw of the L2 `RenderScene` (D10).
* **The media engine.** Image/TLG decode, audio decode, video decode; the
  Linux backend is FFmpeg and the Android backend is MediaCodec.
* **File system + archive parsing.** Raw path/URI byte IO *and* the XP3
  container read+parse. Parsing is pure and stateless, so it belongs here
  (§5).
* **Compile-time platform selection.** Exactly one platform feature is
  active; the other platform's code is not compiled (§4).

### 2.2 L2 — the small game engine

L2 is deliberately "the most fundamental APIs for rendering something or
playing something", with **no TVP vocabulary**:

* **the layer system** — a generic scene graph (`Scene`/`Layer`/`Node`/
  `Bitmap`/`Sprite`): surfaces/windows, child layers, draw order, opacity,
  transforms, blend, bitmap pixel storage. Game engines hold scene graphs,
  so this is L2; it carries no TVP semantics (masks, province images,
  script callbacks and `arc.xp3>path` naming are L3);
* audio playback and mixing; video playback;
* input event stream and timers;
* a **virtual file system** over the parsed archives that L1 hands back
  (`mount`/`read`/`list`), with mount points and an index;
* caches and every other evolving value (glyph atlas, decoded assets,
  decoder progress, worker pool + completion queue).

L2 owns *engine* state. It projects its scene into the L1 `RenderScene` and
calls `render`; it never builds a draw list (D10).

### 2.3 L3 — KiriKiri compatibility surface

* The **native classes** games see (`System`, `Storages`, `Scripts`,
  `Window`/`Layer`/`Bitmap`/`Font`/`Timer`, `SoundBuffer`, `VideoOverlay`,
  `Plugins`, `Debug`, `AsyncTrigger`, transitions, menu, …).
* The **KAG parser** and the KAG layer semantics.
* The **TVP layer semantics on top of the L2 layer system**: the TJS
  `Window`/`Layer`/`Bitmap` classes, masks, province images, screen-buffer
  primary layers, focus/hit-test and script callbacks are implemented in
  L3 over the generic L2 scene graph.
* The **TVP storage semantics**: `arc.xp3>path` addressing, case rules,
  `addAutoPath`.
* **Event adaptation**: L1 pushes platform events; L3 turns them into the
  TJS-visible events the VM dispatches.
* **Native plugins** are compiled into L3 (static), with a null-plugin
  object for DLLs we cannot load (D14, §8 of doc 08).

L3 is **thin glue**: whenever a behaviour is a combination of primitive
operations L2 already exposes, L3 composes it rather than adding a new L2
feature.

### 2.4 L4 — TJS VM engine

* `tjs` (the C++ VM) plus the VM host: creation, the log callback,
  bytecode compilation, script cache, GC roots, `exec_script`.
* The topmost layer. It calls into the L3 native surface and uses the L1
  platform abstraction for its non-VM needs.
* The **frame body** (one VM tick) lives here; the **loop** is L1 (§3).

## 3. Who owns the loop, and the event flow

The event loop is **platform code, so it lives in L1**. L1 exposes an event
interface; higher layers subscribe. The direction is L1 → L3 → L4:

```
platform event   →  L1 (winit / android-activity backend)
                 →  L1 PlatformEvent (window, input, resize, lifecycle)
                 →  L3 adapter  (maps PlatformEvent to TJS-visible event)
                 →  L4 VM       (dispatches to script handlers)
```

Per frame (D26 — **L2 owns it**):

```
L2 Engine::step(dt_ms) / frame(now_ms)
  ├─ advance timers   → invoke their registered callbacks (L3 posts onTimer)
  ├─ GameFrame::before_render(now_ms, dt_ms)
  │      ← L4 vm.tick + L3 event dispatch + sound/video/async/continuous
  │        polls + the audio drain (the staged pumps)
  ├─ drain the posted onTimer events, then run the L3 polls
  ├─ refresh the render-only overlay, upload dirty bitmaps
  ├─ translate + project `scene` into L1 `RenderScene` (device space; D40)
  └─ call L1 RenderPipeline::render
```

The first line and the projection/render pair are L2's. The mixer, video and
transition pumps are still inside the hook; moving them into L2 is the rest of
Phase 9 and does not change the order above.

The composition root (the `krkr` binary, the Android `cdylib`) is trivial:
it selects the L1 platform, constructs L2/L3/L4 and starts the loop. It
contains **no platform logic** — only wiring.

### Frame ownership (D26)

L2 **is** the runtime: it owns the frame clock and the frame entry point.

```rust
impl Engine {
    /// The whole frame. Public and *deterministic* — tests and headless runs
    /// drive it directly with a virtual clock (no sleeps, no real time).
    pub fn frame(&mut self, now_ms: u64, hooks: &mut dyn GameFrame);

    /// Sugar over `frame`: platform event pump + `SystemClock` + render.
    pub fn run(&mut self, clock: impl Clock, hooks: &mut dyn GameFrame);
}

/// Declared in L2, implemented by L3/L4, wired by the composition root.
/// This is the only upward edge; L2 never names L3 or L4.
pub trait GameFrame {
    /// L4 `vm.tick` plus L3's KiriKiri event dispatch (async triggers,
    /// continuous handlers).
    fn before_render(&mut self);
}
```

Consequences of owning it:

* `TimerState::advance(now)` becomes **private** — time enters in exactly one
  place, and the timer's public surface is `create_interval(interval_ms,
  callback)` · `set_interval` · `set_enabled` · `remove` (D27).
* L2 subsystems stop being *handed* time from above; nothing above L2 owns a
  `dt`.
* The mixer advances from the frame clock that `03-middle-layer-api.md` §5
  already assumes L2 has.
* **Re-entrancy:** callbacks mutate L2 state (they create layers, change
  intervals), so a frame **collects due work first, then invokes** — the
  advance is never re-entered mid-iteration.

**Why L2 and not elsewhere.** L1 is stateless and a loop *is* state (time,
frame count, ordering, pending work), so L1 is out. Putting the loop above L2
leaves nobody responsible for `advance → project → render` — it degrades into a
convention shared between two files, which is exactly how the drift below
happened — and it keeps a second clock above L2, competing with the frame clock
L2 already needs.

### Device pixels and the pixel-translation layer (D40)

L3 and scripts describe the scene in **VM pixels** — the size the game
declared; L1 draws in **device pixels** — the framebuffer. L2 owns the mapping
between them (Fit with letterbox, D39) and applies it when it projects and when
it reads back. It is the only layer that names both spaces: L3 never sees a
device coordinate and L1 never sees a VM coordinate. The full contract — the
`PixelTranslation` API, the font-rasterization split, input, and device-pixel
readback over dmabuf — is
[`11-presentation-and-pixel-translation.md`](11-presentation-and-pixel-translation.md)
§6–§7. The translation is recomputed per size change inside `frame`, which is
why it depends on D26 above. The layer tree → projection → draw path the
translation feeds is mapped end to end in
[`12-layer-tree-and-frame.md`](12-layer-tree-and-frame.md).

Implemented: `engine::translate::PixelTranslation` owns the Fit mapping,
`Engine` refits it on a VM/framebuffer size change, and the composition root
inverse-maps pointer/touch coordinates before the VM tick.

The translation is internal to L2 (D41): L3 keeps calling L2 in VM
coordinates. The launcher can set the game's VM size explicitly (for example
to the display size); the translation then maps that VM surface to the window.
Readback samples the corresponding region. The render-resolution/supersampling
idea is a deferred design note, not part of the current tree.

### Implementation status

`engine::Engine` now owns the frame. It holds the scene, the projected
`RenderScene`, the `Surface`/`RenderState` pair, the bitmap texture table, the
timer registry and the script-visible input state, and exposes
`step(dt_ms, hooks)` / `frame(now_ms, hooks)`. A frame advances the L2 timer
registry, runs the single `GameFrame::before_render(now_ms, dt_ms)` hook (the
L4 VM tick plus the L3 polls and audio drain), refreshes the render-only
overlay, uploads dirty bitmaps, projects when the scene revision changed, and
performs one L1 render call. `app::Host` is a wrapper around it; `GameHost`
implements the hook.

Landed with this: `TimerState::advance` is called only from the engine frame;
`compat::run_frame_body` no longer fires timers, it drains the posts the
engine produced; L3 timer schedule writes read the injected frame clock
instead of a wall clock; and the timer tests drive virtual time.

Remaining in Phase 9: the mixer, video and transition pumps still run inside
the L3 hook (the audio device drain is part of the hook); `Engine::run(clock,
hooks)` is not built yet because the platform loop still owns the event pump;
and `TimerState::advance` stays public until the timer surface collapses to
D27's four methods. Pixel translation (D39/D40) is Phase 10.

**Schedule:** `07-roadmap.md` Phase 9.

## 4. Compile-time platform and backend selection

We do **not** bundle every platform into one binary. Cargo features select
what is compiled:

* platform: `platform-linux` | `platform-android` (exactly one);
* render: `render-vulkan`;
* media: `video-ffmpeg` (Linux) | `video-mediacodec` (Android).

`#[cfg]` and the backend crates are confined to L1. L2/L3/L4 are
platform-agnostic and compile identically everywhere. This is what keeps
"so much cross-platform code" out of the high layers.

## 5. Archive parsing in L1 (revised D6)

XP3 parsing is a pure transform over bytes: it holds no state and needs no
engine context, so it lives in **L1** alongside the file system. The
runtime flow for a script-mounted archive (see
[`09-game-loading.md`](09-game-loading.md)):

```
TJS script (L4)
  → native (L3)              "mount foo.xp3"
  → L2 Storage::mount(path)  creates an archive object
  → L1 FileSystem open+read  reads bytes through FileState
  → L1 parse                 stateless XP3 index/entry parse
  → L1 returns parsed index to L2
  → L2 registers it in the VFS namespace (state) and resolves names
```

L1 owns read+parse; **L2 owns the namespace/state** and sees only a parsed
file system; L3 is only the script adapter. This supersedes the earlier D6
("archive/namespace logic stays in L2"): the *namespace* stays in L2, the
*container parse* moves to L1.

## 6. Crate layout

Names are provisional; the dependency direction is the contract.

```
crates/
  # L1 — platform core ------------------------------------------------------
  platform/api      shared DTOs + error types (no impls)
  platform/fs       stateless FileSystem + FileState + XP3 read/parse
  platform/render   stateless RenderPipeline/FontSystem + Vulkan + glyph
  platform/media    stateless Media + image/audio/video decode
                    (video-ffmpeg | video-mediacodec)
  platform/platform window/surface, event loop, input, clock, threads,
                    process lifecycle; winit | ndk; the platform
                    abstraction higher layers use

  # L2 — the small game engine ---------------------------------------------
  engine     Scene/Bitmap/Sprite, mixer, timers, the VFS namespace,
                    RenderScene projection; owns engine state

  # L3 — KiriKiri compatibility --------------------------------------------
  compat     native classes + KAG + event adapter + static plugins
                    (absorbed the former `tvp-natives`/`tvp-visual`/
                     `tvp-storages`/`tvp-scripts`/`tvp-input`/`tvp-sound`/
                     `tvp-text`/`tvp-kagparser` crates)

  # L4 — TJS VM -------------------------------------------------------------
  vm         TJS VM host over tjs
  tjs        the C++ VM FFI (unchanged)

  # root --------------------------------------------------------------------
  app        thin binary: select platform, wire L1..L4, start the loop
  android    the Android composition root: android_main, the GameActivity
                    loop and JNI wiring
  testkit    dev-only shared test helpers
```

The crate consolidation is complete. For reference, the machinery was:
* keep `tjs` where it is unless in the way;
* fold the `tvp-*` native families into `compat` as `plugins/*`;
* the old `engine` loader split into mount/discover (L2) and execute
  (L3/L4);
* keep the game runnable at every step (roadmap).

## 7. Component ownership

| Concern | Owner | Notes |
|---------|-------|-------|
| Window/surface/event loop/input/clock/threads | L1 `platform` | all platform code here |
| Vulkan device/pipeline/draw | L1 `render` | consumes `RenderScene` |
| Glyph rasterization | L1 `render` | pure transform |
| Image/audio/video decode | L1 `media` | backend by compile-time feature |
| Raw path IO + XP3 parse | L1 `fs` | stateless |
| **Layer system (generic scene graph)**, mixer, timers, VFS namespace, caches | L2 `engine` | engine state; no TVP semantics |
| Projection scene → `RenderScene` | L2 `engine` | field copy, not a draw list |
| Native classes, KAG, TVP semantics, event adaptation, plugins | L3 `compat` | composes L2 |
| VM lifecycle, bytecode, GC, frame body | L4 `vm` | top layer |
| Composition/wiring | `app` | no platform logic |

## 8. Threading

* The VM is single-threaded (L4).
* The loop is single-threaded and owns the frame (L1 drives, L4 executes
  the body).
* L2 owns the worker pool; it schedules stateless L1 transforms and drains
  completions on the loop thread.
* No `VM_RUN_LOCK` workaround (it existed only for Bevy's parallel
  scheduler).

## 9. Acceptance criteria

1. L1 builds with no L2/L3/L4 dependency; L2 builds with no L3/L4
   dependency; L3 builds with no `#[cfg]`/backend/JNI symbols.
2. The desktop and Android builds differ only in the L1 platform/backend
   features and the trivial composition root.
3. **D9:** L1 has no mutable statics, no `thread_local!`, no
   `LazyLock`/`OnceLock`/`OnceCell` caches, no id-keyed registries, no
   `&mut self` on its service objects. Guarded by `scripts/check-stateless.sh`
   in the gate.
4. **D10:** L2 contains no traversal, draw-list, batching or blend-state
   selection; its render path is "sync scene → call render".
5. **D11:** no silent fallback and no hardcoded limit in L1/L2; the one
   documented exception is the null plugin object (D14).
6. The game runs on Linux and Android with Vulkan and no Bevy in
   `Cargo.lock`.
7. Loading takes an explicit archive list; a directory scan runs only when no
   explicit list is given (§5, doc 09, D35).

## 10. Why the reference validates this

The C++ engine already separates these concerns: `core/io/IFileBackend`
(fs), `core/visual/RenderManager` + `visual/impl/DrawDevice` (pipeline),
`core/visual/LayerManager`/`LayerImpl` (layer state), `core/movie/ffmpeg`
(media), `core/sound` (sound), and the platform layer under
`core/environ/*` (win32 / android / cocos2d). The Rust port collapsed the
platform and host into Bevy; the target restores the separation with the
host in L1 and the VM on top.

## 11. Non-goals

* Multi-backend bundles at runtime (platform is compile-time).
* macOS/iOS/WebGPU/WebGL backends.
* A WASM plugin host in the first milestone (see doc 08 §; static plugins
  first).
* Rewriting the C++ TJS2 VM.

## 12. L2 API stability (merged from `13-l2-api-stability.md`)

Goal: **freeze the `engine` (L2) public API** so we can optimise everything
below it (L1: Vulkan batching, GPU glyph runs, advanced blends, hw video decode,
XP3 caches, …) without ever touching L3 again. L1 changes must be invisible above
the `engine` boundary.

See [`02-target-architecture.md`](02-target-architecture.md) for the layer rules
and [`07-roadmap.md`](07-roadmap.md) §9 for the
optimisations this freeze is meant to protect.

### 12.1. Dependency rule (what L3 may use)

- **L3 (`compat`) may depend on `api`, `engine`, `fs` and `tjs`.** It must
  **not** call `render` or `media` directly — those are L1 and reach the game
  only *through* `engine`. (Verified today: `compat`'s non-dev deps are
  `api`, `engine`, `fs`, `tjs`, `thiserror`, `log`, `flate2`, `chardetng`,
  `encoding_rs`, `serde_json`, `md-5`.) L3 file IO goes through the L1 `fs`
  abstraction rather than `std::fs`, which is platform code.
- **`engine` may name L1 types.** `engine` depends on the L1 *type* crates
  (`api`, `fs`, `render`) and its public surface may use their types
  (`RenderScene`, `FileState`, `FontMetrics`, …). A change to an L1 type that
  flows into an `engine` signature is **accepted, not forbidden** (D33); L1
  changes that need a new *engine concept* are still an additive L2 change,
  reviewed as such.

### 12.2. Stability policy

1. **Additive-only.** New functions/methods/variants are fine; removing or
   changing an existing public item requires an explicit decision entry (a `Dxx`
   in [`README.md`](README.md)).
2. **`#[non_exhaustive]` on public enums and option/state structs**, so we can
   add variants/fields later without a breaking change. (Most `engine` types now
   carry it; the remainder are listed in §12.5.)
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
   generic flags). The old L2 `LT_*`/`TVP_*` constants are gone from the code;
   the names survive only in doc comments citing the reference.
7. **No cross-layer constants or vocabulary.** A layer owns its constants and
   types; an **upper layer must not import a lower layer's constant or type**.
   If an upper layer needs a value, it defines its own and the lower layer
   abstracts the concept behind a generic API. A constant consumed by more than
   one layer means the abstraction has leaked — fix the abstraction, not the
   import. (Corollary: L2's `LT_*`/`TVP_*` are `pub(crate)` at most; L3 owns the
   public TVP table. The one deliberate exception is the host-facing render
   seam — `RenderScene`/`Texture` — which the composition root, not L3, uses.)
7. **Docs on every public item** (`#![warn(missing_docs)]` is on); state
   invariants and units (physical px vs logical, ms vs frames) are documented.

### 12.3. The stable L2 surface (per module)

| Module | Stable public surface | Notes |
|---|---|---|
| `scene` | `Scene` + its methods; `WindowState`, `LayerState`, `BitmapState`, `FontState`, `Rect` | The layer system. State fields are `pub` by house style; the types carry `#[non_exhaustive]` so growth is additive. |
| `projection` | `project_window`, `project_scene`, `ComposedLayer`, `compose_layer`, `resolve_blend` | The native `lt*` blend numbering lives in L3 (`natives/visual/layer/blend.rs`). |
| `layer_ops` / `raster` | the pixel operations (`blit_*`, `fill_*`, `blur_*`, `affine_blit`, `fill_polygon`, …) | Pure transforms over `BitmapState`. `GammaAdjust` fields are public-by-design (a plain parameter block). |
| `storage` | `Storage` + `mount_archives`/`mount_archive`/`find`/`read`/`stat`/`exists`/`list`/`file_list`/`add_auto_path`/`extract…`; `discover_startup` | The VFS namespace + TVP addressing. |
| `startup` | `discover_startup` | |
| `sound` | `Mixer`, `SoundSource`, `SoundChannel`, `SoundBuffer`, `LoopInfo`, `LoopFlags`, `SoundLabel`, `LoopLink`, `mix`/`mix_source` | Pure PCM state; L1 device consumes. The `.sli` parser and the TVP `0..100000` scale live in L3. |
| `text` | `TextEngine`, `GlyphAtlas`, `TextLayout`, `PositionedGlyph`, `TextStyle`, `project_text`, `compose_text_cpu`, `TextError` | L1 raster/atlas swap must not change this. |
| `input` | `InputState`, `ButtonState`, `KeyState`, `TouchContact` | Generic; no VK codes. |
| `timer` | `TimerState`, `TimerId`, `TimerCallback`, `TimerError` | Generic firing; TVP dispatch mode stays L3. |
| `video` | `VideoOverlay` + enums (`PlayStatus`, `VideoEvent`, `OverlayBinding`, `VideoBounds`, `PeriodReason`, `VideoError`) | Consumes frames from the caller; no codec dep. |
| `trans` | `Trans`, `TransKind`, `TransStep`, `RuleMask`, `TransError` | Over `layer_ops`. |

### 12.4. What the freeze protects

The optimisations in `07-roadmap.md` §9 are all *below* this
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

### 12.5. Known debt to stabilise (tracked)

1. **Public state fields** on `WindowState`/`LayerState`/`BitmapState`
   (`scene.rs`) are still public; the types are `#[non_exhaustive]` so growth
   is additive, and L3 reads/writes some fields directly.
2. **Raw `usize` handles**: `main_image_pixel_buffer*` / `province_image_pixel_buffer*`
   return addresses for the TJS `buffer`/`bufferForWrite` properties. Either keep
   them documented as *opaque addresses* (never dereferenced in Rust) or expose a
   safe `with_*_pixels` only.
3. **Naming**: settle `SoundChannel` vs `Channel`, `VideoOverlay` methods vs the
   reference `vo_*`, etc. The TVP constants and most `#[non_exhaustive]` gaps
   have been closed: L2 enums/structs carry the attribute and the `LT_*`/`TVP_*`
   names remain only in doc comments.

### 12.6. Guard (D21, superseded by D33)

Decision (D33): the `cargo public-api` snapshot (option a) is **dropped**.
The L2 surface is frozen by **review and audit**: a reviewer checks every
change to `crates/engine`'s public items against this document (§12.3) and
the decision log, and the surface lock (D29) already pins the L3 dispatch
surface mechanically. Options considered: (a) a checked-in `cargo public-api`
snapshot; (b) a nightly `rustdoc --output-format json` snapshot diff; (c) a
`docs/migration/l2-api-snapshot.txt` regenerated by a script and diffed.
None is wired; §12.3 is the contract.
