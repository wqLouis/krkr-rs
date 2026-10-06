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

## 2. The middle-layer API

The API is a small set of traits and plain data types. It describes
**abstract actions**, not TVP concepts:

| Trait | Abstract actions | Replaces today |
|-------|------------------|----------------|
| `FileSystem` | open path/URI as a byte stream, stat, list, create/write/delete/copy | `engine::Storage`, `xp3::Xp3Archive`, direct `std::fs` in natives |
| `Renderer` | create/upload/drop textures, submit a flat draw list, present, resize, offscreen targets | `render/src/sync.rs`, `blend.rs`, wgpu |
| `GlyphEngine` | shape/rasterize a glyph run to coverage, provide an atlas | `tvp-text` raster/atlas + CPU `paint_layout` |
| `MediaEngine` | probe/decode image, probe/decode/stream audio, open/present video | `tvp-visual/bitmap.rs`, `tvp-sound/decode.rs`, `tvp-natives/video/*` |
| `AudioOutput` | start/stop the OS audio device; pull frames from a callback | `tvp-sound/player.rs` (rodio/cpal) |
| `InputSource` | deliver input events/state produced by the host | `render/src/input_bridge.rs` event capture |
| `HostWindow` | create/resize/title the OS window; logical↔physical mapping | Bevy `WindowPlugin`/`Window` |
| `Clock` / `LogSink` | monotonic time, log fan-out | Bevy `Time`, `log` setup |

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
  # L1 — bottom core -------------------------------------------------------
  krkr-api          trait definitions + DTOs only (no impls, no I/O)
  krkr-core         facade that assembles the platform core
  krkr-render       ash/Vulkan renderer + glyph engine   (impls Renderer,
                                                           GlyphEngine)
  krkr-media        media engine (ffmpeg | mediacodec)   (impls MediaEngine)
  krkr-fs           file system backends                 (impls FileSystem,
                                                           AudioOutput? no —
                                                           see §5)

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
| Glyph rasterization + glyph atlas | L1 `krkr-render` (`GlyphEngine`) | may move wholly to L1 |
| Sound object model, mixer, fades, `.sli` | L2 `krkr-engine` | today `tvp-sound` |
| Audio decode (symphonia/opus) | L1 `krkr-media` | |
| Audio device output (cpal/AAudio) | L1 `krkr-core` (`AudioOutput`) | |
| Image decode (TLG/image) | L1 `krkr-media` | TLG is format-specific but platform-free |
| Video decode (ffmpeg/mediacodec) | L1 `krkr-media` | backend by target |
| Storage namespace, `.xp3`, case rules | L2 `krkr-engine` | today `engine::Storage` + `xp3` |
| Raw path/URI byte IO | L1 `krkr-fs` | |
| Archive container parsing | L2, using `FileSystem` streams | `xp3` reworked to `Read+Seek` |
| Vulkan device/swapchain/pipelines | L1 `krkr-render` | |
| Render list flattening | L2 `krkr-engine` | |
| Input state + TVP hit-test semantics | L2 `krkr-engine` | today `tvp-input` + hit_test |
| OS input events | L4 host → L1/L2 via `InputSource` | |
| TJS VM and native registration | L3 `krkr-tjs` | today `tvp-*` natives |
| KAG parsing | L3 `kag`/`krkr-tjs` | unchanged |

## 6. Threading model in the target

The VM stays single-threaded. The new constraint is that L1 may have its own
threads (media workers, GPU thread) and must never call into L3:

* **L4 host thread**: event loop + per-frame VM drive (`timer_poll`,
  `sound_poll`, `video_overlay_poll`, `async_trigger_poll`,
  `continuous_handler_poll`) + render submission.
* **L1 media workers**: decode-only; results are handed back through a
  completion queue drained on the host thread.
* **L1 GPU**: either the host thread owns the queue (simplest, matches
  today) or a dedicated submission thread; the middle layer only hands over
  a finished `RenderList` and texture uploads.
* **L2**: mutated only on the host thread, exactly like today. No scene lock
  is needed if the VM and render both run on the host thread, but the API
  may keep a scoped lock discipline for tests.

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
3. The desktop CLI and the Android cdylib assemble the same `krkr-engine`
   and the same core, differing only in host glue.
4. The game runs and the headless scene dump + native-parity report are
   unchanged or intentionally updated.
5. Bevy is absent from `Cargo.lock`.
