# 01 — Architecture investigation

Status: current-state inventory of the implemented tree. Every claim below is
from source inspection of this branch; file:line references point at the
present revision.

This document answers "what is the project arch today?" for the layers
described in `02-target-architecture.md`.

---

## 1. Workspace at a glance

12 workspace members (`Cargo.toml:3`); `default-members = ["crates/app"]`
(`Cargo.toml:4`). There is no `tvp-*` crate and no Bevy in the tree.

| crate | layer | role |
|-------|:-----:|------|
| `crates/tjs` | vendored | C++ TJS2 VM FFI (unchanged, all-FFI) |
| `crates/platform/api` | L1 | shared DTOs + typed `*Error` enums, no impls |
| `crates/platform/fs` | L1 | stateless `FileSystem`/`FileState` + XP3 read/parse |
| `crates/platform/media` | L1 | stateless `Media` + image/audio/video decode |
| `crates/platform/render` | L1 | stateless `RenderPipeline`/`FontSystem` + Vulkan + null |
| `crates/platform/platform` | L1 | window/surface, event loop, input, clock, audio, threads |
| `crates/engine` | L2 | scene graph, mixer, timers, VFS namespace, projection |
| `crates/compat` | L3 | native classes, KAG, event adapter, static plugins |
| `crates/vm` | L4 | VM host (`VmHost`, `FrameCtx`) |
| `crates/app` | root | host assembly + `krkr` binary (wiring only) |
| `crates/android` | root | Android composition root (`android_main`, GameActivity, JNI) |
| `crates/testkit` | dev | shared test helpers, dev-dependencies only |

Dependency edges (architectural; log/thiserror/env_logger omitted):

```
vm       → compat, engine, tjs, log, thiserror
compat   → engine, tjs, api, fs, log, thiserror, flate2, chardetng,
           encoding_rs, serde_json, md-5
engine   → api, fs, render, cosmic-text, log, thiserror
render   → api, ash, ash-window, raw-window-handle, ab_glyph, ttf-parser
fs       → api, flate2, thiserror
media    → api, symphonia(+opus), image, ffmpeg-next (feature), ndk (target)
platform → api, winit (feature), cpal (feature), ndk (target)
app      → api, engine, render, fs, compat, vm, raw-window-handle, platform
android  → app, platform, engine, api, android-activity, jni, log, env_logger
```

`engine` names L1 *type* crate types in its signatures (`RenderScene`,
`FileState`, `FontMetrics`, …); that edge is D33. `compat`
reaches L1 only through `engine`/`fs`/`api`; it never calls `render` or
`media` directly.

## 2. Runtime data flow

```
krkr (crates/app/src/main.rs)                 android cdylib (crates/android)
  runner::run(Launch)                           android_main
      │  resolves Launch/RuntimeConfig             │  reads JNI fields, config:
      │  build Host (L1 objects + L2 state)        │  game dir, state dir, settings
      ▼                                            ▼
  Host (crates/app/src/lib.rs)               host::run_loop (android-activity)
   ├─ RenderPipeline / FileSystem / Media      │  builds a PlatformWindow
   ├─ Scene · RenderScene · RenderState        │  from the ANativeWindow
   ├─ texture table, InputState                │
   └─ GameHost (crates/app/src/game.rs)        │
        ├─ Storage (mounted archives)          │
        ├─ VmHost (crates/vm)                  │
        │    ├─ Tjs2Engine + CompatHost        │
        │    └─ FrameCtx { now, dt, body, events }
        └─ per-frame step_with_pacing:
             VmHost::tick → compat::run_frame_body
               timer_poll → sound_poll → video_poll
               → async_trigger_poll → continuous_handler_poll
               → project_and_render
             Host::step: sync bitmaps → project scene → render
```

Key execution facts:

* **The VM is single-threaded.** `VmHost` owns the C++ engine and the L3
  dispatch state; all script execution happens on the loop thread.
* **One process-global binding.** The TJS2 ABI hands a native callback only
  the raw `tjs2_engine*` and exposes no user-data slot, so `compat::binding`
  keeps exactly one engine → context table. It is L3 and holds *dispatch*
  state (a pointer to the L2 scene/storage/services), never game state
  (`08-tjs-vm-and-native-plugins.md` §3.1). L1 has no globals
  (`scripts/check-stateless.sh`).
* **The frame is still orchestrated above L2.** `app::GameHost::step_with_pacing`
  owns `dt` and calls `compat::run_frame_body`; D26 (`Engine::frame`) is not
  built (`02-target-architecture.md` §3 "Implementation status").
* **Loading is explicit.** `GameInput { base_dir, archives }` is resolved by
  the launcher; a directory-only launch mounts the discovered `*.xp3` files
  (D35, `09-game-loading.md`).

## 3. Scene → GPU flow

1. `Host::step` (`crates/app/src/lib.rs`) syncs dirty bitmaps to textures,
   prunes stale textures, then projects.
2. `reproject_render_scene` has an **idle fast path** keyed on
   `Scene::revision()`; a rebuild reuses the persistent `RenderScene` arena.
3. `engine::projection::project_scene` walks the scene and produces one
   `RenderNode` per drawn layer, composing parent transforms, opacity,
   z-order, clip and resolved blend.
4. `render::render` walks the `RenderScene` tree itself: it accumulates
   transforms, clips with a scissor, orders siblings by `z`, selects a
   pipeline + fixed-function blend state and draws.
5. Texture upload is dirty-gated per bitmap; sub-rect uploads go through
   `upload_textures`.
6. Blend mapping: `compat`'s `lt*` numbering maps to the generic
   `BlendMode`; the Vulkan backend has 7 fixed-function slots and rejects the
   destination-dependent modes with `RenderError::Unsupported` (the null
   backend composites them on the CPU).

## 4. Text / glyph path

Text is **CPU-rasterized into the layer bitmap**; it does not reach the GPU as
glyphs:

* face resolution: `compat::resource::resolve_face` via the `System.addFont`
  registry, the attached storage, then the host's installed system fonts
  (`TVPCreateFontStream` default), with a requested-face cache;
* `render::FontSystem`/`Font` load and rasterize coverage (`ab_glyph`);
* `engine::text` owns the `TextEngine`, glyph atlas, layout and CPU
  composition; pre-rendered `.tft` fonts are parsed and their body read
  lazily;
* a GPU glyph-atlas path (`RenderContent::Glyphs`) is not implemented; text
  composited into the bitmap is what `getPixel`/`saveLayerImage` read back.

## 5. CPU pixel surface

The TVP pixel semantics live in `engine::layer_ops` and `engine::raster` and
operate on `BitmapState`:

* fills/blends, blits, effects (drop shadow, blur, gaussian blur),
  resampling (`stretch_blit*`, `affine_blit`), tone, alpha conversion, flips,
  9-patch, vector raster for `GdiPlus.Appearance`;
* masks/faces (`dfMain`/`dfMask`/`dfProvince`/…, `holdAlpha`) and the province
  plane in `engine::scene`;
* transitions in `engine::trans` (`extrans_wave/ripple/mosaic/rotate/turn`
  plus the built-in set).

L3 composes these primitives; the GPU only ever sees flat quads.

## 6. Media (audio/image/video)

* **Image** — `media::decode_image`/`probe_image` (TLG5/TLG6 + `image`-crate
  formats) and `media::image_encode` (PNG/BMP/JPEG). Decoding is synchronous
  on the VM thread; async decode is a tracked optimisation (`TODO.md` §16).
* **Audio** — `media::audio` decodes with symphonia (wav/pcm, ogg/vorbis,
  flac, mp3) plus the Opus adapter; `engine::sound` owns the device-free
  mixer, `.sli` loop/label handling and the `SoundBuffer`/`SoundChannel`
  state machines; `platform::audio` is the L1 output device (cpal on Linux,
  AAudio on Android).
* **Video** — `media::video` has one interface with two backends: FFmpeg on
  Linux (in-memory IO, whole audio track decoded at open, back-seek
  contract) and NDK MediaCodec on Android (a `memfd` source, YUV420→RGBA in
  pure Rust, runtime-untested). `VideoOverlay` composites frames into
  `layer1`/`layer2` and the mixing layer.

## 7. File system / storage

* `fs::FileSystem` is a stateless root (host path or in-memory) with a
  positional `FileState`; `fs::xp3` reads and parses the XP3 container over
  positional reads and holds no cursor.
* `engine::Storage` owns the namespace: mount list, `arc.xp3>path`
  addressing, case-insensitive resolution, auto-path table, resolved-index
  cache, and the optional whole-archive preload (`preload_archives`).
* L3 file IO goes through the L1 `fs` abstraction (`crates/compat/Cargo.toml`
  depends on `fs`).
* One IO path stays outside the fs surface: `tjs/cpp/streams.cpp` uses
  `std::fopen` for TJS `Array.save/load/saveStruct`. Android uses a resolved
  external path plus `MANAGE_EXTERNAL_STORAGE` (`docs/android.md` §4); no
  SAF/`AssetManager` backend is wired.

## 8. State ownership

* L1 objects (`RenderPipeline`, `FileSystem`, `Media`, `FontSystem`,
  `AudioDevice`) are stateless services; every evolving value is an explicit
  `&mut` state passed in.
* L2 owns the engine state: the scene, the storage namespace, the mixer, the
  timer registry, the VFS, caches and the projected `RenderScene`.
* L3 keeps per-family dispatch state in `CompatServices::state` (typed,
  once-cell-free maps) plus the one ABI binding; it owns TVP semantics
  (masks, province, focus/hit-test, storage naming, `.sli`, VK codes).
* `Host` (app) owns the L1 service objects and the L2 state values for a run;
  two hosts can run in one process.

## 9. The remaining work

Open items live in `TODO.md`; the architecture-level ones are:

1. **`Engine::frame` (D26/D27)** — L2 does not yet own the frame; `app`
   owns `dt` and `compat::run_frame_body` orchestrates. Schedule:
   `07-roadmap.md` Phase 9.
2. **Native plugin coverage** — the unported plugin names take the null
   plugin path; coverage is the next major version (`TODO.md` §17, D38).
3. **Timer event-queue approximation** — the reference bounds queued events;
   the port enforces at most one fire per timer per `advance` (D36).
4. **Android on-device validation** — the window/audio/input path is
   compiler-checked but never run; the loop has no device run (D36).
5. **Deferred performance work** — GPU text, hardware video, GPU
   destination-dependent blends (`07-roadmap.md` §9, D22).
