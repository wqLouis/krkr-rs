# 02 — Target architecture

Status: design proposal. Nothing here is implemented yet.

This document defines the layer boundaries, the crate layout, the dependency
rules, and where each existing responsibility lands. The API details are in
[`03-middle-layer-api.md`](03-middle-layer-api.md).

---

## 1. Three semantic layers, four physical tiers

The user's design names three semantic layers. Physically they map to four
tiers because the host/frontend is distinct from the TJS compatibility
surface.

```
L4 Host/frontend         windowing · surface · event loop · platform lifecycle
L3 TJS compatibility     TJS2 VM + native bindings + KAG parser
L2 Middle layer          TVP semantics: layer system, sound/video model, mixer,
                         storage namespace, transitions, config; exposes a
                         minimal service API to L3
L1 Bottom core           real implementations of the middle-layer API:
                         Vulkan renderer + glyph engine, media engine
                         (ffmpeg|mediacodec), file system
```

Direction of dependency is strictly downward:

```
L4  depends on  L3, L2, L1 (glue only)
L3  depends on  L2
L2  depends on  krkr-api + the L1 type crates (krkr-fs, krkr-render,
                krkr-media) for their stateless objects / explicit state
                values; never on a backend crate's guts directly
L1  depends on  OS/Vulkan/FFmpeg only; no TJS, no TVP, no layers
```

> Dependency note (review finding #6): the concrete-object model means L2
> names the L1 object/state types (`FileSystem`/`FileState`,
> `RenderPipeline`/`RenderScene`/`RenderState`, `Media`/decoder states), so
> `krkr-engine` depends on `krkr-fs` + `krkr-render` (+ `flate2` for the
> XP3 zlib segments). What L2 must **not** do is reach into the backend
> implementation: no `ash`, `winit`, `cpal`, `ffmpeg-next`, `std::fs`
> symbols in L2. Swapping a backend means swapping the L1 crate, not
> editing L2.

**Invariant A (the important one):** L1 must compile and be testable without
TJS, without `tvp-*`, and without knowing what a "layer" is. If it does not,
the boundary has leaked.

**Invariant B:** L2 must not reference Bevy, wgpu, `ash`, winit, FFmpeg,
MediaCodec, `cpal`, `std::fs` or any concrete backend symbol directly. It
may depend on the L1 **type** crates (`krkr-fs`, `krkr-render`, `krkr-media`)
for their stateless objects and explicit state values, but it never calls
backend internals. If it does, the backend is not swappable.

**Invariant C (D9):** L1 is **stateless and functional**, built as
**stateless abstraction objects + explicit state values**. L1 creates the
objects (`RenderPipeline`, `Media`, `FileSystem`, `FontSystem`,
`AudioDevice`) and provides the mechanism; L2 stores every state value
(`RenderScene`, `RenderState`, `SurfaceState`, `DecoderState`, `FileState`,
caches, indices, decoder progress) and performs actions on the objects. L1
has no process-global mutable state, no singletons, no hidden caches, no
mutable fields on its objects and no implicit lifetimes. Pure transforms are
free functions; actions take the evolving state as an explicit parameter.

## 2. The middle-layer API

The API is a set of **stateless abstraction objects**, **explicit state
values**, and plain DTOs. L1 creates the objects; L2 stores the states and
dispatches actions. The API describes platform mechanism, not TVP concepts:

| L1 object (created by L1) | L2 state (stored by L2) | Replaces today |
|---------------------------|-------------------------|----------------|
| `FileSystem` | `FileState` (handle + cursor), namespace/index map | `engine::Storage`, `xp3::Xp3Archive`, direct `std::fs` in natives |
| `RenderPipeline` | `RenderScene` tree, `RenderState`, `Surface`; glyph atlas, backend cache | `render/src/sync.rs`, `blend.rs`, wgpu |
| `FontSystem` | `Font` values, `GlyphAtlas`, layout | `tvp-text` raster/atlas + CPU `paint_layout` |
| `Media` | `AudioDecoderState`, `VideoDecoderState`, decoded-asset caches, worker pool + completions | `tvp-visual/bitmap.rs`, `tvp-sound/decode.rs`, `tvp-natives/video/*` |
| `AudioDevice` | `AudioOutputState`, `Mixer` | `tvp-sound/player.rs` (rodio/cpal) |
| — (host glue) | TVP input state, focus, gestures | `render/src/input_bridge.rs` event capture |
| — (host glue) | window/desktop requests | Bevy `WindowPlugin`/`Window` |
| `Clock` / `LogSink` | — | Bevy `Time`, `log` setup |

### 2.1 The object/state rule (D9)

Concretely, in L1:

* **Objects are stateless services.** `RenderPipeline`, `Media`,
  `FileSystem`, `FontSystem`, `AudioDevice` are created once, shared by
  reference, and have no mutable fields and no `&mut self` that remembers
  anything.
* **All evolving state is an explicit value.** `RenderScene`, `RenderState`,
  `SurfaceState`, `DecoderState`, `FileState` are defined by L1 but owned
  and stored by L2; actions take them as `&mut` parameters
  (`render(&self, surface, state: &mut RenderState, scene: &RenderScene)`).
* **The render pipeline consumes the tree.** L2 does not flatten, sort,
  batch or compute a draw list; it maintains the `RenderScene` and calls
  `render`. L1 owns traversal, composition and drawing.
* **No globals.** No `static`/`thread_local` mutable state, no `LazyLock`/
  `OnceLock`/`OnceCell` caches, no process-wide registries.
* **No `Id`-addressed tables.** L1 never resolves an integer through a
  private map; L2 holds the state value directly.
* **Caches live in L2.** Glyph atlas, decoded-image cache, storage index,
  compiled pipelines, full-vs-stream decisions, loop metadata and the
  worker pool are L2 values.
* **No policy in L1.** L1 does not choose to cache, reuse, prefetch, defer
  or throttle. Any decision is L2's, expressed in L2 state.
* **No callbacks into engine state.** L1 returns plain owned buffers; it
  never calls into the layer tree or TJS.

This is what makes L1 mockable (`NullRenderPipeline`, an in-memory
`FileSystem`, a stub `Media`) and what keeps the two backends (FFmpeg vs
MediaCodec, Bevy adapter vs Vulkan pipeline) from drifting into different
semantics.

The API deliberately does **not** contain: layers, windows-as-TVP-objects,
bitmaps, blend types, transitions, `SoundBuffer`, `VideoOverlay`, storage
namespaces, `.xp3`, KAG, or TJS values.

DTOs are plain structs/enums (`ImageDesc`, `AudioSpec`, `GlyphRun`,
`RenderNode`, `BlendMode`, `PixelRegion`, …). They must not carry
`Arc<RwLock<…>>` or callbacks into TJS.

> The distinction from the earlier draft: an RAII handle that *contains* the
> resource state is not enough. The object and the state are split, so L2
> can store/serialize/inspect the state and L1 remembers nothing.

## 3. Where the layer system and the render pipeline live

The **layer system (state and TVP semantics) is L2**; the **render pipeline
is L1**. They meet at the `RenderScene` tree.

L2 owns:

* the logical tree: `Window`/`Layer`/`Bitmap`/`Font`/`Timer` state (today's
  `tvp-visual/src/scene.rs`/`bitmap.rs`/`tl*`), CPU pixel storage and every
  `layer_ops.rs`/`raster.rs` operation;
* every TVP property: masks, province images, `face`, `holdAlpha`,
  `ImageLeft/Top/Width/Height`, focus/hit-test, script callbacks,
  screen-buffer primary layers;
* CPU text composition: layout + glyph coverage blitted into a `BitmapState`
  (so `getPixel`/`piledCopy` still work);
* the projection of its layer tree into an L1 `RenderScene` (one node per
  drawn layer, parent links, composed opacity/z/visible, resolved texture
  handles) and the `RenderState` (GPU/upload/cache bookkeeping).

L1 owns:

* traversal of the `RenderScene`, parent transforms, z-order, opacity
  composition, clipping, blend-state selection, batching;
* texture upload scheduling and offscreen compositing;
* the actual Vulkan submission and presentation.

L2's per-frame render responsibility is a field-level sync of the tree plus
one `render(pipeline, surface, state, scene)` call. It never computes a draw
list and never walks the tree for rendering.

### Why the reference validates this

The C++ engine already has this split:

* `reference/cpp/core/io/IFileBackend` — the file-system abstraction;
* `reference/cpp/core/visual/RenderManager.*` +
  `visual/impl/DrawDevice.*` — the render pipeline / draw device;
* `reference/cpp/core/visual/LayerManager.*`/`LayerImpl.*` — the layer
  system state;
* `reference/cpp/core/movie/ffmpeg` — the media engine;
* `reference/cpp/core/sound` — the sound engine.

The Rust port collapsed all of these into Bevy-driven `render`. The target
re-separates them, which is the stated design intent.

## 4. Proposed crate layout

Names are proposals; the important thing is the dependency direction. The
middle-layer API can be one crate so L1 implementations share the DTOs.

```
crates/
  # L1 — bottom core (stateless objects + explicit state, D9) -------------
  krkr-api          object/action signatures + state/DTO types (no impls)
  krkr-core         facade that assembles the platform core
  krkr-render       stateless RenderPipeline/FontSystem objects + Vulkan/
                    glyph mechanism; defines RenderScene/RenderState/…
  krkr-media        stateless Media object + decode actions; defines
                    AudioDecoderState/VideoDecoderState (ffmpeg | mediacodec)
  krkr-fs           stateless FileSystem object; defines FileState

  # L2 — middle layer ------------------------------------------------------
  krkr-engine       layer system + TVP semantics + mixer + transitions;
                    depends on krkr-api + the krkr-fs/krkr-render type
                    crates (+ flate2 for XP3); no backend guts

  # L3 — TJS compatibility -------------------------------------------------
  tjs2-sys          (unchanged) C++ VM FFI
  krkr-tjs          native bindings, split by class family; replaces the
                    current tvp-natives/tvp-visual natives/storages/scripts/
                    input/kagparser/sound bindings; depends on krkr-engine

  # L4 — host ---------------------------------------------------------------
  krkr-app          CLI + desktop host (winit + krkr-core)
  krkr-android      Android cdylib host (android-activity + krkr-core)
```

Existing crates are not all renamed at once. The machinery is:

* keep `tjs2-sys`, `kag`, `tvp-util`, `tvp-streams`, `xp3`, `tvp-config`
  where they are unless they are in the way;
* split `tvp-visual` into `krkr-engine` (scene/bitmap/layer_ops/text
  composition) and `krkr-tjs` (the native adapters);
* split `tvp-sound` into `krkr-engine` (mixer/model) and `krkr-media`
  (decode) + `krkr-core` (output);
* split `tvp-natives/video` into `krkr-media`;
* fold `engine` into `krkr-engine` (it is the load/bootstrap module; it can
  stay a submodule or a thin `krkr-loader` crate).

Keep the `tvp-*` names during the transition; the roadmap stages the rename
so the game is runnable at every step.

## 5. Component ownership matrix

| Concern | Owner | Notes |
|---------|-------|-------|
| Window/layer/bitmap/font/timer model | L2 `krkr-engine` | today `tvp-visual::scene` |
| CPU pixel ops, masks, province, transitions | L2 `krkr-engine` | today `tvp-visual::natives::*` |
| Text layout + CPU glyph composition | L2 `krkr-engine` | glyph *rasterization* in L1 |
| Glyph rasterization | L1 `krkr-render` (`FontSystem`) | pure: object + `Font` + glyph + size → coverage |
| Glyph atlas (cache) | L2 `krkr-engine` | L2-owned value passed into composition |
| Sound object model, mixer, fades, `.sli` | L2 `krkr-engine` | today `tvp-sound` |
| Audio decode (symphonia/opus) | L1 `krkr-media` (`Media`) | actions take L2-owned `AudioDecoderState` |
| Audio device output (cpal/AAudio) | L1 `krkr-core` (`AudioDevice`) | action takes L2-owned `AudioOutputState` |
| Image decode (TLG/image) | L1 `krkr-media` | pure free functions; TLG is platform-free |
| Video decode (ffmpeg/mediacodec) | L1 `krkr-media` (`Media`) | backend by target; `VideoDecoderState` owned by L2 |
| Storage namespace, `.xp3`, case rules | L2 `krkr-engine` | today `engine::Storage` + `xp3` |
| Raw path/URI byte IO | L1 `krkr-fs` (`FileSystem`) | actions take L2-owned `FileState` |
| Archive container parsing | L2, over `FileState` | `xp3` reworked to positional reads |
| Render traversal, composition, batching, draw | L1 `krkr-render` (`RenderPipeline`) | consumes the `RenderScene` tree |
| Projection of layer tree → `RenderScene` | L2 `krkr-engine` | field copy, not a draw list |
| Vulkan device/queue | L1 `krkr-render` (`RenderPipeline`) | stateless object, never global |
| Compiled pipelines / `RenderState` | L2 `krkr-engine` | L1-defined value owned by L2 |
| Input state + TVP hit-test semantics | L2 `krkr-engine` | today `tvp-input` + hit_test |
| OS input events | L4 host → L1/L2 via `InputSource` | |
| TJS VM and native registration | L3 `krkr-tjs` | today `tvp-*` natives |
| KAG parsing | L3 `kag`/`krkr-tjs` | unchanged |

## 6. Threading model in the target

The VM stays single-threaded. Because L1 is stateless (D9), it does not own
worker threads either; L2 owns the thread pool and schedules stateless L1
work onto it:

* **L4 host thread**: event loop + per-frame VM drive (`timer_poll`,
  `sound_poll`, `video_overlay_poll`, `async_trigger_poll`,
  `continuous_handler_poll`) + render submission.
* **L2 worker pool**: owned by the middle layer; runs pure L1 transforms
  and L1 actions over L2-owned state, and delivers owned buffers into an L2 completion
  queue drained on the host thread. L1 provides no thread pool and no
  queue.
* **L2 render submission**: the host thread owns the `RenderPipeline` /
  `RenderState` values (simplest, matches today). A dedicated submission
  thread can later move the `RenderState`; L1 code is unchanged either way.
* **L2 state**: mutated only on the host thread, exactly like today. No
  scene lock is needed if the VM and render both run on the host thread,
  but the API may keep a scoped lock discipline for tests.

`VM_RUN_LOCK` (today's workaround for Bevy's parallel scheduler) should
**disappear** once the app loop is ours.

## 7. What stays exactly as it is

* `tjs2-sys` + `crates/tjs2-sys/cpp` (the VM) — the compatibility anchor.
* `kag`, `tvp-util`, `tvp-streams`, `tvp-config`, `xp3` parsing logic
  (XP3 gets a stream-source refactor, not a rewrite).
* `tvp-text` layout/atlas algorithms (moved into L2), `tvp-visual` pixel
  ops, `tvp-sound` mixer math, `tvp-natives/video` decoder internals.
* `docs/fonts.md` resolution rules, `scripts/native_parity.py` and its
  allowlist (must keep passing through the migration).

## 8. Non-goals of the target

* Multi-window full support beyond what TVP needs (the primary-window
  milestone already exists).
* macOS/iOS/WebGPU/WebGL backends.
* GPU-side layer compositing or GPU text in the first milestone.
* Rewriting the C++ TJS2 VM.

## 9. Architectural acceptance criteria

The migration is "done" when all of these hold:

1. `krkr-core` (render/media/fs) builds with `--no-default-features` and no
   `tvp-*`/`tjs2-sys`/Bevy dependency.
2. `krkr-engine` has no dependency on `bevy`, `ash`, `winit`, `ffmpeg-next`,
   `cpal`, or `std::fs` outside an explicitly allowed low-level module.
3. **D9 is enforced:** `krkr-core` contains no mutable `static`/
   `thread_local`, no `LazyLock`/`OnceLock`/`OnceCell` caches, no global
   registries, no `HashMap` keyed by an externally-supplied resource id,
   and no `&mut self` methods on its service objects. A lint/grep test in
   CI guards this; the only allowed state is a value returned by a
   constructor and stored by the caller (an explicit state value).
4. **D10 is enforced:** `krkr-engine` contains no render traversal,
   draw-list construction, batching, blend-state selection or per-frame
   entity/material code. Its render path is "sync `RenderScene`, upload
   dirty textures, call `RenderPipeline::render`".
5. The desktop CLI and the Android cdylib assemble the same `krkr-engine`
   and the same core, differing only in host glue.
6. The game runs and the headless scene dump + native-parity report are
   unchanged or intentionally updated.
7. Bevy is absent from `Cargo.lock`.
