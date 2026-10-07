# 02 — Target architecture

Status: design, **revised** for the final layering (see §1.1). This document
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
* **All platform code.** Windowing/surface (winit on Linux,
  `android-activity`/NDK on Android), the event loop, input, clock, threads,
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

Per frame:

```
L1 tick  →  L4 VM frame body (timer_poll, sound_poll, video poll,
            async_trigger_poll, continuous_handler_poll)
         →  L3 → L2 (mutate engine state)
         →  L2 projects the scene, calls L1 RenderPipeline::render
```

The composition root (the `krkr` binary, the Android `cdylib`) is trivial:
it selects the L1 platform, constructs L2/L3/L4 and starts the loop. It
contains **no platform logic** — only wiring.

## 4. Compile-time platform and backend selection

We do **not** bundle every platform into one binary. Cargo features select
what is compiled:

* platform: `platform-linux` | `platform-android` (exactly one);
* render: `render-vulkan`;
* media: `media-ffmpeg` (Linux) | `media-mediacodec` (Android).

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
  api        shared DTOs + error types (no impls)
  fs         stateless FileSystem + FileState + XP3 read/parse
  render     stateless RenderPipeline/FontSystem + Vulkan + glyph
  media      stateless Media + image/audio/video decode
                  (media-ffmpeg | media-mediacodec)
  platform   window/surface, event loop, input, clock, threads,
                  process lifecycle; winit | android-activity; the
                  platform abstraction higher layers use

  # L2 — the small game engine ---------------------------------------------
  engine     Scene/Bitmap/Sprite, mixer, timers, the VFS namespace,
                  RenderScene projection; owns engine state

  # L3 — KiriKiri compatibility --------------------------------------------
  compat     native classes + KAG + event adapter + static plugins
                  (absorbs today's tvp-natives/tvp-visual/tvp-storages/
                   tvp-scripts/tvp-input/tvp-sound/tvp-text/tvp-kagparser)

  # L4 — TJS VM -------------------------------------------------------------
  vm         TJS VM host over tjs + the composition root
  tjs        the C++ VM FFI (unchanged)

  # root --------------------------------------------------------------------
  app        thin binary: select platform, wire L1..L4, start the loop
```

Existing crates are not renamed in one go. The machinery:
* keep `tjs`, `tvp-util`, `tvp-streams` where they are unless in the way;
* fold the `tvp-*` native families into `compat` as `plugins/*`;
* the old `engine` loader splits: mount/discover → L2, execute → L3/L4;
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
   `&mut self` on its service objects. Guarded in CI.
4. **D10:** L2 contains no traversal, draw-list, batching or blend-state
   selection; its render path is "sync scene → call render".
5. **D11:** no silent fallback and no hardcoded limit in L1/L2; the one
   documented exception is `Plugins.link` (D14).
6. The game runs on Linux and Android with Vulkan and no Bevy in
   `Cargo.lock`.
7. Loading takes an explicit archive list; no directory scan (§5, doc 09).

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
