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
L2  depends on  the middle-layer API (L1 traits), never on L1 impls
L1  depends on  OS/Vulkan/FFmpeg only; no TJS, no TVP, no layers
```

**Invariant A (the important one):** L1 must compile and be testable without
TJS, without `tvp-*`, and without knowing what a "layer" is. If it does not,
the boundary has leaked.

**Invariant B:** L2 must not reference Bevy, wgpu, `ash`, winit, FFmpeg,
MediaCodec, `std::fs` or any concrete backend. It talks only to the API
traits. If it does, the backend is not swappable.

**Invariant C (D9):** L1 is **stateless and functional**. It has no
process-global mutable state, no singletons, no hidden caches and no
implicit lifetimes. Every piece of persistent state — layer tree, caches,
indices, decoder progress, resource ownership — is a value owned by L2 and
either passed into L1 or exposed by L1 as an RAII handle that L2 stores.
Pure transforms are free functions; impure operations take an explicit
context/handle.

## 2. The middle-layer API

The API is a small set of **stateless operations** and plain data types. It
describes abstract actions, not TVP concepts, and it carries no engine
state:

| Surface | Abstract actions | Replaces today |
|---------|------------------|----------------|
| `fs` functions + `FileHandle` | open a path/URI, positional read/write, stat, list, create/delete/copy | `engine::Storage`, `xp3::Xp3Archive`, direct `std::fs` in natives |
| `Gpu` + `Texture`/`Target`/`Surface` | create/upload/drop textures, submit a flat draw list, present, resize, offscreen targets | `render/src/sync.rs`, `blend.rs`, wgpu |
| `rasterize_glyph` + `GlyphFace` | rasterize a glyph to coverage; query metrics | `tvp-text` raster/atlas + CPU `paint_layout` |
| media free functions + `AudioDecoder`/`VideoDecoder` | probe/decode image, probe/decode/stream audio, open/present video | `tvp-visual/bitmap.rs`, `tvp-sound/decode.rs`, `tvp-natives/video/*` |
| `AudioDevice` | start/stop the OS audio device; pull frames from a callback | `tvp-sound/player.rs` (rodio/cpal) |
| `InputSource` | deliver input events/state produced by the host | `render/src/input_bridge.rs` event capture |
| `HostWindow` | create/resize/title the OS window; logical↔physical mapping | Bevy `WindowPlugin`/`Window` |
| `Clock` / `LogSink` | monotonic time, log fan-out | Bevy `Time`, `log` setup |

### 2.1 The statelessness rule (D9)

Concretely, in L1:

* **No globals.** No `static`/`thread_local` mutable state, no `LazyLock`
  registries, no `OnceLock` caches. (The current Rust code is full of these;
  they are L3/L2 concerns and must not reappear in L1.)
* **Pure transforms are free functions.** `decode_image(bytes) -> Rgba`,
  `probe_audio(bytes) -> spec`, `yuv420_to_rgba(...)`,
  `rasterize_glyph(face, ...) -> coverage`, resampling, etc. Same inputs,
  same outputs, no remembered state.
* **Platform resources are RAII handles owned by L2.** `Texture`, `Target`,
  `Surface`, `FileHandle`, `GlyphFace`, `AudioDecoder`, `VideoDecoder`
  own their platform resource and are stored in L2. There is no
  renderer-side `HashMap<Id, Resource>` that L2 addresses by id; the handle
  *is* the resource value.
* **Caches live in L2.** The glyph atlas, decoded-image cache, storage
  index, pipeline cache, full-vs-stream decisions and loop metadata are L2
  values passed into L1 operations (e.g. `submit(gpu, &mut PipelineCache,
  &list)`), not L1 internals.
* **No policy in L1.** L1 does not choose to cache, reuse, defer or
  prefetch; it does exactly what the call says. Any decision is L2's.
* **No callbacks into engine state.** L1 may hand back plain owned buffers;
  it never calls into the layer tree or TJS.

This is what makes L1 mockable (`NullGpu`, an in-memory `FsRoot`, a stub media
backend) and what keeps the two backends (FFmpeg vs MediaCodec, Bevy adapter
vs Vulkan) from drifting into different semantics.

The API deliberately does **not** contain: layers, windows-as-TVP-objects,
bitmaps, blend types, transitions, `SoundBuffer`, `VideoOverlay`, storage
namespaces, `.xp3`, KAG, or TJS values.

DTOs are versionless plain structs/enums (`ImageDesc`, `AudioSpec`,
`GlyphRun`, `DrawQuad`, `BlendMode`, `StreamHandle`, …). They must not carry
`Arc<RwLock<…>>` or callbacks into TJS.

## 3. Where the layer system lives (L2)

The layer system is **entirely L2**:

* the logical tree: `Window`/`Layer`/`Bitmap`/`Font`/`Timer` state (today's
  `tvp-visual/src/scene.rs`/`bitmap.rs`/`tl*`), CPU pixel storage and every
  `layer_ops.rs` operation;
* composition: ancestor transform/opacity/visibility, z-order, screen-buffer
  primary layers, masks/province, clipping, transitions;
* the flattening step: `Scene` → `RenderList` of `DrawQuad`s (what
  `sync_scene` does today);
* CPU text composition: layout + glyph coverage blitted into a `BitmapState`
  (so `getPixel`/`piledCopy` still work).

L1 receives only the `RenderList` and a set of texture handles. It knows
"draw these textured quads with these blend states in this order", not
"window 1, primary layer, opacity 0.8".

### Why the reference validates this

The C++ engine already has this split:

* `reference/cpp/core/io/IFileBackend` — the file-system abstraction;
* `reference/cpp/core/visual/RenderManager.*` +
  `visual/impl/DrawDevice.*` — the renderer interface vs the layer managers;
* `reference/cpp/core/movie/ffmpeg` — the media engine;
* `reference/cpp/core/sound` — the sound engine;
* `reference/cpp/core/visual/LayerManager.*`/`LayerImpl.*` — the layer system.

The Rust port collapsed all of these into Bevy-driven `render`. The target
re-separates them, which is the stated design intent.

## 4. Proposed crate layout

Names are proposals; the important thing is the dependency direction. The
middle-layer API can be one crate so L1 implementations share the DTOs.

```
crates/
  # L1 — bottom core (stateless/functional, D9) ----------------------------
  krkr-api          operation signatures + DTOs only (no impls, no state)
  krkr-core         facade that assembles the platform core
  krkr-render       stateless Vulkan/ash ops + glyph rasterization; RAII
                    Texture/Target/Surface/GlyphFace handles
  krkr-media        stateless decode fns + RAII AudioDecoder/VideoDecoder
                    (ffmpeg | mediacodec)
  krkr-fs           stateless fs fns + RAII FileHandle

  # L2 — middle layer ------------------------------------------------------
  krkr-engine       layer system + TVP semantics + mixer + transitions;
                    depends on krkr-api only

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
| Glyph rasterization | L1 `krkr-render` | stateless: face + glyph + size → coverage |
| Glyph atlas (cache) | L2 `krkr-engine` | L2-owned value passed into composition |
| Sound object model, mixer, fades, `.sli` | L2 `krkr-engine` | today `tvp-sound` |
| Audio decode (symphonia/opus) | L1 `krkr-media` | stateless fns + `AudioDecoder` value owned by L2 |
| Audio device output (cpal/AAudio) | L1 `krkr-core` (`AudioDevice`) | device value owned by L2 |
| Image decode (TLG/image) | L1 `krkr-media` | TLG is format-specific but platform-free |
| Video decode (ffmpeg/mediacodec) | L1 `krkr-media` | backend by target; `VideoDecoder` owned by L2 |
| Storage namespace, `.xp3`, case rules | L2 `krkr-engine` | today `engine::Storage` + `xp3` |
| Raw path/URI byte IO | L1 `krkr-fs` | stateless fns over an explicit root |
| Archive container parsing | L2, over `FileHandle` | `xp3` reworked to positional reads |
| Vulkan device/queue | L1 `krkr-render` | `Gpu` value owned by L2, never global |
| Pipeline cache | L2 `krkr-engine` | L2-owned value passed to `submit` |
| Render list flattening | L2 `krkr-engine` | |
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
* **L2 worker pool**: owned by the middle layer; runs pure L1 decode/
  rasterize functions and delivers owned buffers into an L2 completion
  queue drained on the host thread. L1 provides no thread pool and no
  queue.
* **L2 GPU submission**: the host thread owns the `Gpu`/`Surface` values
  (simplest, matches today). A dedicated submission thread can later move
  the `Gpu` value; L1 code is unchanged either way.
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
   registries, and no `HashMap` keyed by an externally-supplied resource id.
   A lint/grep test in CI guards this; the only allowed state is a value
   returned by a constructor and stored by the caller (an RAII handle).
4. The desktop CLI and the Android cdylib assemble the same `krkr-engine`
   and the same core, differing only in host glue.
5. The game runs and the headless scene dump + native-parity report are
   unchanged or intentionally updated.
6. Bevy is absent from `Cargo.lock`.
