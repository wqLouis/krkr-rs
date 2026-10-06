# krkr-rs architecture migration — middle layer + Vulkan core

Status: **planning only. No production code is changed on this branch.**
Branch: `migration/middle-layer-ash`
Created from: `main` (`7ba9435 docs: removed TODO.md`)

This directory is the design + investigation record for the architecture
rework described below. Nothing in `crates/` is touched by this branch; the
only commits are documentation.

---

## 1. Why we are doing this

The current engine is built **inside Bevy**. Bevy's ECS, asset pipeline,
scheduling, windowing and input are used as the engine's substrate, not as a
replaceable backend:

* the game loop is a Bevy `Update` system chain
  (`render/src/runner.rs` `run_vm → … → sync_scene`);
* the logical layer tree is projected straight into Bevy entities, materials
  and sprites every mutating frame (`render/src/sync.rs`);
* the menu is Bevy UI (`render/src/menu.rs`);
* the Android entry point is `#[bevy_main]`
  (`crates/android/src/lib.rs`).

That is fine for a prototype, but it makes three things hard:

1. **Correctness against TVP.** KiriKiri's object model is an
   object-oriented layer/window/bitmap tree with CPU pixel access
   (`getPixel`, `piledCopy`, province images, masks, `MainImage` read-back).
   An ECS is a poor fit for owning that tree; we keep fighting the impedance
   mismatch and re-deriving the tree on every sync.
2. **Rendering control and cost.** Bevy + wgpu drags in OpenGL/Metal/WebGPU
   backends and an asset/material/entity pipeline we do not need. The actual
   GPU workload is a small set of textured quads with fixed blend states
   (`render/src/blend.rs`). We want a thin, explicit Vulkan pipeline.
3. **Platform surface.** Bevy's windowing/input/UI is the top of the
   dependency tree. Replacing any one piece (UI, input, surface) means
   replacing all of it, and Android is locked to Bevy's GameActivity glue.

The reference C++ engine already separates these concerns
(`reference/cpp/core/io/IFileBackend`, `core/visual/RenderManager`,
`core/movie/ffmpeg`, `core/sound`), and the user's target design restores
that separation in Rust.

## 2. The target in one picture

```
┌──────────────────────────────────────────────────────────────────────┐
│ L4  Host / frontend            CLI (krkr-cli) · Android cdylib        │
│     window, surface, event loop, platform lifecycle, JNI              │
├──────────────────────────────────────────────────────────────────────┤
│ L3  TJS compatibility surface  tjs2-sys + tvp-* native bindings + KAG │
│     maps KiriKiri script API onto the middle layer                    │
├──────────────────────────────────────────────────────────────────────┤
│ L2  Middle layer (krkr-engine)                                        │
│     • the layer/window/bitmap/font model  (the "layer system")        │
│     • CPU pixel operations, transitions, storage namespace, config    │
│     • sound object model + mixer, video-overlay state machine         │
│     • exposes a minimal, TVP-free service API to L3                   │
│     ── consumes the middle-layer API traits ──►                       │
├──────────────────────────────────────────────────────────────────────┤
│ L1  Bottom core (krkr-core)                                           │
│     renderer (Vulkan/ash + glyph engine) · media engine               │
│     (ffmpeg | mediacodec) · file system                               │
│     implements the middle-layer API; knows nothing about TVP/TJS      │
└──────────────────────────────────────────────────────────────────────┘
```

The **middle-layer API** is the contract between L2 and L1: a small set of
abstract actions ("decode this image", "upload these pixels", "draw this
list", "play this stream", "open this path"). L1 implements it on real
platforms. L2 builds everything KiriKiri-specific on top of it — including
the layer system. The bottom engine does **not** own layers; it only does
rendering, decoding and file I/O, and does them well.

## 3. Decisions recorded by this plan

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | Introduce an explicit middle-layer API (traits + plain DTOs) between the engine and the platform. | Decouple semantics from backends; make both mockable and testable; enable incremental migration. |
| D2 | The **layer system lives in the middle layer**, not in the renderer. | Layer composition, opacity, pixel read-back, masks and province images are TVP semantics. A renderer that owns them can never be swappable or Vulkan-thin. |
| D3 | Replace Bevy with **`ash` (raw Vulkan)** for rendering. | Smallest GPU workload is quads + fixed blends; we want explicit pipelines and no GL/Metal/WebGPU weight. |
| D4 | Vulkan only; no OpenGL/Metal/WebGPU/WebGL fallback. | User requirement. Documented risk in `07-roadmap.md §6`. |
| D5 | Media is a bottom-layer **media engine** with two backends: system **FFmpeg** on Linux, **MediaCodec** on Android. | Matches the reference (`core/movie/ffmpeg`) and current split (`tvp-natives/src/video/{ffmpeg,android}.rs`). |
| D6 | File system is a bottom-layer trait; archive/namespace logic stays in the middle layer. | Bottom = raw path/URI byte IO; TVP's `arc.xp3>path` addressing and case rules are engine semantics. |
| D7 | Strangler migration, not a big-bang rewrite. | Keep the game runnable at every step: first a Bevy-backed implementation of the new API, then swap in the Vulkan core. |
| D8 | Keep CPU rasterization/compositing as the correctness baseline; GPU acceleration is an optimization behind the same API. | Scripts read pixels back (`getPixel`, `piledCopy`, `saveLayerImage`); the pixel-exact path must survive. |

## 4. Document index

| Doc | Contents |
|-----|----------|
| [`01-investigation.md`](01-investigation.md) | What the project is today: crate graph, coupling map, data flow, measured facts, and the concrete seams that block the target. |
| [`02-target-architecture.md`](02-target-architecture.md) | The four-layer target, crate layout, dependency rules, ownership boundaries, and how the layer system fits. |
| [`03-middle-layer-api.md`](03-middle-layer-api.md) | The API surface: `FileSystem`, `Renderer`, `GlyphEngine`, `MediaEngine`, `AudioOutput`, `InputSource`, `HostWindow`, DTOs. |
| [`04-renderer-vulkan.md`](04-renderer-vulkan.md) | Bevy → `ash` design: render list, pipelines, blend mapping, offscreen compositing, glyph atlas, winit/GameActivity surface, what to delete. |
| [`05-media-engine.md`](05-media-engine.md) | Media engine: image/audio/video decode, FFmpeg vs MediaCodec backends, threading, TJS-poll semantics, migration of `tvp-sound`/`video`. |
| [`06-file-system.md`](06-file-system.md) | Bottom FS trait + `std::fs` backend; how `Storage`/`Xp3Archive`/natives move onto it; Android SAF/AssetManager; the C++ `fopen` bypass. |
| [`07-roadmap.md`](07-roadmap.md) | Phased plan, per-phase deliverables/exit criteria, risks, testing strategy, rollback, and open questions. |

## 5. Scope of this branch

In scope (docs only):

* the investigation above and the target design;
* the middle-layer API sketch and the migration sequencing.

Out of scope for this branch:

* any code, crate, dependency or manifest change;
* reversing the decisions above (they are recorded so they can be argued
  about before implementation starts).

## 6. How to read this as a reviewer

1. Read `01-investigation.md` to agree on the current-state facts.
2. Read `02-target-architecture.md` and push back on the layer boundaries.
3. Read `03-middle-layer-api.md`; the API is the expensive part to change
   later.
4. Read `07-roadmap.md`; every phase must leave a runnable game.
