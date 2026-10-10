# 01 — Current architecture investigation

Status: read-only investigation. Every claim below is from source inspection
of this branch's parent (`main` @ `7ba9435`); file:line references are to
that revision.

This document answers "what is the project arch today?" before proposing the
move in `02-target-architecture.md`.

---

## 1. Workspace at a glance

20 workspace members (`Cargo.toml:3`), `default-members = ["crates/platform/render",
"crates/app"]` (`Cargo.toml:4`). Approximate size (LOC in `src/`):

| crate | role | LOC | Bevy? |
|-------|------|----:|:-----:|
| `tvp-visual` | Window/Layer/Bitmap/Font/Timer natives + logical `Scene` + TLG | 27.0k | no |
| `tvp-natives` | System/Debug/VideoOverlay/MenuItem/Plugins/AsyncTrigger + Trans | 13.2k | no |
| `render` | Bevy app, scene→GPU sync, input bridge, menu, runner | 9.0k | **yes** |
| `tvp-sound` | decode + mixer + sound natives (symphonia/rodio/cpal) | 6.7k | no |
| `tvp-kagparser` | KAGParser native | 5.9k | no |
| `tjs` | C++ TJS2 VM FFI (build.rs compiles the C++ VM) | 4.7k (+C++) | no |
| `tvp-text` | font discovery, glyph atlas, layout, `.tft` | 2.7k | no |
| `tvp-storages` | Storages native over `engine::Storage` | 2.6k | no |
| `tvp-input` | input state + Mouse/Key natives | 2.3k | no |
| `tvp-streams` | `tTJSBinaryStream` port (`BinaryStream` trait exists) | 1.9k | no |
| `tvp-util` | charset/md5/random utils | 1.7k | no |
| `engine` | load module: mount storage, bootstrap VM, run startup.tjs | 1.7k | no |
| `tvp-scripts` | Scripts native | 1.6k | no |
| `kag` | `.ks` parser | 1.4k | no |
| `tvp-config` | config manager (JSON) | 1.3k | no |
| `tvp-archive` | zip/7z/tar readers (unused by the game path) | 0.7k | no |
| `xp3` | XP3 archive reader | 0.5k | no |
| `android` | `#[bevy_main]` cdylib entry | 0.2k | **yes** |
| `app` | `krkr-cli` binary (headless/CLI) | 0.2k | no |

**Bevy is confined to two crates**: `render` (`bevy = "0.19"` with default
features, `crates/platform/render/Cargo.toml`) and `android` (`bevy` with
`android-game-activity`, `crates/android/Cargo.toml`). This is the good news
for migration: the semantic crates are already engine-agnostic.

`ash` and `winit` are already in `Cargo.lock` (transitively via wgpu/Bevy),
so a raw-Vulkan renderer needs **no new heavyweight dependency download**,
only direct dependencies.

## 2. Runtime data flow today

```
krkr-rs run <dir>                     android cdylib
  render/src/main.rs                    crates/android/src/lib.rs
      │  runner::run_game               │  #[bevy_main] android_main
      ▼                                 ▼
  runner::game_app  ───────────────────────────────┐
      │  Bevy App, DefaultPlugins, WindowPlugin     │
      │  Startup: game_startup                       │
      │     engine::loader::prepare (mount + VM)     │
      │     register_natives(...)                    │
      │     run_startup (patch.tjs, startup.tjs)     │
      │  Update: (run_vm → consume_window_requests   │
      │           → sync_host_window_resolution      │
      │           → sync_scene).chain()              │
      │           input_bridge capture/dispatch      │
      ▼                                              │
  Arc<RwLock<Scene>>  ◄── natives write (VM thread)  │
      │                                              │
      ▼                                              │
  sync_scene: Scene → Bevy entities/materials/Assets │
      │                                              │
      ▼                                              ▼
              wgpu (Vulkan/GL/Metal/WebGPU)
```

Key execution facts:

* **The VM is single-threaded.** All native calls run under a process-wide
  `static VM_RUN_LOCK` (`render/src/runner.rs`) because Bevy's scheduler may
  run systems on worker threads. `tjs` makes the handle `Send`/`Sync`
  with `unsafe impl` but documents single-thread ownership.
* **The scene is the current de-facto middle layer.** `SharedScene =
  Arc<RwLock<Scene>>` (`render/src/sync.rs`), defined in
  `tvp-visual/src/scene.rs`. Natives mutate it under the write lock;
  `sync_scene` reads it under the read lock. It is **already Bevy-free**.
* **Registration is frontend-owned.** `runner::register_natives` wires
  `tvp_natives`, `tvp_input`, `tvp_kagparser`, `tvp_storages`,
  `tvp_scripts`, `tvp_visual`, `tvp_sound` (`render/src/runner.rs:440-461`);
  `crates/app/src/main.rs` duplicates this for the CLI. The crates
  communicate through **process-global statics + `set_*` context setters**,
  not through injected handles.

## 3. Where Bevy is actually used

By subsystem (all in `crates/platform/render` unless noted):

| Subsystem | Bevy surface | Representative files |
|-----------|--------------|----------------------|
| App/ECS/schedule | `App`, `Plugin`, `Resource`, `Component`, `Query`, `Res/ResMut`, `Commands`, `Startup/Update`, `.chain()` | `runner.rs`, `main.rs`, `sync.rs` |
| Textures/assets | `Assets<Image>`, `Handle<Image>`, `Image::new`, `Extent3d`, `TextureFormat::Rgba8UnormSrgb` | `sync.rs:99-151` |
| Materials/pipelines/shaders | `AsBindGroup`, `Material2d`, `Material2dPlugin`, `specialize`, `BlendState`, `BlendFactor`, `BlendOperation`, `ShaderRef`, `embedded_asset!`; WGSL imports Bevy's vertex output | `blend.rs`, `layer_blend_material.wgsl` |
| Sprites/mesh/camera | `Sprite`, `Mesh2d`, `MeshMaterial2d`, `Transform`, `Visibility`, `Camera2d`, `Projection`/`ScalingMode::AutoMin`, `ClearColorConfig` | `sync.rs` |
| Window/winit | `Window`, `WindowPlugin`, `Monitor`, `AppExit`, `CursorMoved` | `runner.rs`, `sync.rs:367-430` |
| Input | `ButtonInput<MouseButton/KeyCode>`, `Gamepad`, `TouchInput`, `MouseWheel` | `input_bridge.rs` |
| UI | Bevy UI nodes, `Interaction`, `BackgroundColor`, `Text` | `menu.rs` |
| Math/color | `Vec2/Vec3`, `Color`, `LinearRgba`, `bevy::math::Rect` | `sync.rs` |
| Android | `bevy::android::ANDROID_APP`, `#[bevy_main]` | `crates/android/src/lib.rs:46-57` |

## 4. Scene → GPU flow (the part a Vulkan renderer must replace)

1. `sync_scene` (`render/src/sync.rs:485`) has an **idle fast path** keyed on
   `Scene::revision()`, projection size and window count.
2. It spawns one `Camera2d` with an aspect-preserving projection
   (`sync.rs:531-563`).
3. For each window it walks `Scene::window_layer_order`, skips the
   **screen-buffer** primary layer (`sync.rs:442`), composes ancestors
   (`compose_layer`, `sync.rs:1029`), computes
   `alpha = window.opacity × composed.opacity`, and builds one visual per
   layer via `build_layer_visual` (`sync.rs:855`).
4. `build_layer_visual` returns one of:
   * `Sprite` — source-over / unknown, using Bevy's built-in sprite shader;
   * `Blended { mode, texture, color }` — a `LayerBlendMaterial` quad whose
     **pipeline variant** carries the fixed-function blend state;
   * `Skip` — `ltBinder`.
5. Texture upload is dirty-gated per bitmap (`BitmapAssets::handle_for`,
   `sync.rs:109`; `upload_bitmap`, `sync.rs:138`); the dirty flag is cleared
   under a short write lock after sync. Removed bitmaps prune their GPU
   texture (`sync.rs:752`).
6. Blend mapping: `render_path_for` (`blend.rs:490`) → `blend_state_for`
   (`blend.rs:352`) builds a **wgpu** `BlendState`. UI:
   * **fixed-function**: SourceOver, Replace, Additive, Subtractive,
     AddAlpha, Multiply, Screen;
   * **destination-dependent** (ColorDodge/Burn, Darken, Lighten,
     Difference, Exclusion, Overlay, HardLight, SoftLight): currently
     **degrade to source-over** with a one-shot warning (`blend.rs:436`).
7. The fragment shader is trivial (`color × textureSample`); all compositing
   is fixed-function.

Flat entity layout under one `WindowRoot` per window; global z =
`window_layer_order` index (`sprite_z`, `sync.rs:1162`). Position via
`rect_center` (y-flip, `sync.rs:1134`). Image placement/clipping via
`visible_image_region` (`sync.rs:1077`), which is how sprite-sheet buttons
select frames (`ImageLeft/ImageTop/ImageWidth/ImageHeight`).

## 5. Text / glyph path

Entirely **CPU**, and it does **not** reach the GPU as glyphs:

* discovery/lookup: `tvp_text::resolve_face` with `fonts.json`
  (`docs/fonts.md`); system discovery via `fontdb` only when permitted;
* atlas: `GlyphAtlas` fixed-cell RGBA grid, append-only rows, keyed per
  `(face, height, bold)` (`tvp-text/src/atlas.rs`);
* layout: custom y-down line layout, wrap-anywhere CJK, no kerning
  (`tvp-text/src/layout.rs`);
* `.tft` pre-rendered bitmap fonts (`tvp-text/src/prerendered.rs`);
* `Layer.drawText` (`tvp-visual/src/natives/layer.rs`) runs `layout` +
  `paint_layout` and **blits coverage into the layer's `BitmapState`**, then
  `mark_dirty()`. The atlas is only an intermediate CPU cache.

Consequence: a Vulkan renderer needs **no glyph pipeline** to reach parity.
"Glyph rendering engine" in the target design is therefore a decision point:
keep CPU rasterization (reference behaviour, pixel read-back preserved) or
add a GPU text path later behind the same middle-layer API.

## 6. CPU pixel surface that the renderer never sees

The hard TVP semantics live in `tvp-visual/src/natives/layer_ops.rs` and
operate directly on `BitmapState.rgba`:

* fills/blends (`fill_color_*`, `fill_mask`, `colorize`, `noise`),
* blits (`blit_copy/over/plane`, `tile_rect`),
* effects (`do_drop_shadow`, `do_blur_light`, `gaussian_blur`,
  `blur_bitmap_in_place`),
* resampling (`stretch_blit*`, `affine_blit`),
* tone (`do_gray_scale`, `adjust_gamma`, `light_contrast`),
* alpha/add-alpha conversion, flips, 9-patch,
* vector raster (`raster.rs`) for `GdiPlus.Appearance` draws,
* primary-layer screen-buffer composite (`composite_primary_layer`) used for
  script read-back (`piledCopy`, `saveLayerImage`).

Masks/faces: `face = dfMain/dfMask/dfProvince/dfOpaque/dfAddAlpha` and
`holdAlpha`; province plane in `scene.rs`. Transitions are currently a
**completion-callback stub** (`layer.rs` `beginTransition`), with provider
metadata in `tvp-natives/src/extrans.rs`; the interpolation itself is
unimplemented.

This is exactly why D2 (`README.md`) puts the layer system in the middle
layer: the GPU only ever sees flat quads; the semantics are CPU-side.

## 7. Media (audio/image/video) today

### 7.1 Audio (`tvp-sound`)

* Decode with **symphonia** directly (not rodio): whole-file
  `decode_audio` → interleaved `f32`, header-only `probe_audio`, and a
  `StreamDecoder::next_chunk` packet loop (`decode.rs`). Opus via
  `symphonia-adapter-libopus`; Opus failure falls back to ~60 ms silence.
* Asset policy in `source.rs`: read entry + `.sli`, probe, then choose
  whole-file vs streaming at an **8 MiB** PCM estimate; streaming uses a
  `StreamRing` with ~2 s decode-ahead on a named `tvp-audio-*` worker.
* Mixer (`mixer.rs`): device-free, clock-driven `Mixer`/`Channel` with
  volume/volume2/rate/pan/3D/fades/`.sli` loop flags; two clocks (headless
  `advance(dt)` vs device pull `render_mix_advancing`).
* Output (`player.rs`): rodio/cpal `DeviceSinkBuilder`, `!Send` guard
  created/dropped on the owning thread, held in a Bevy `Resource`;
  `sound_poll` fires `onStatusChanged`/`onFadeCompleted` on the VM thread.

### 7.2 Image (`tvp-visual`)

* `bitmap.rs::decode_image`: TLG5/TLG6 by extension/magic (`tlg.rs`), then
  `image::load_from_memory_with_format`, then `image::guess_format`
  (png/jpeg/webp/bmp/gif/tiff/tga/dds/pnm/ico/qoi/hdr/exr/ff).
* Synchronous on the VM thread (`Bitmap.load`), with an async variant that
  polls completions (`loadAsync`).

### 7.3 Video (`tvp-natives`)

* One `MovieDecoder` interface, backend chosen by `cfg`:
  `ffmpeg` (system FFmpeg via `ffmpeg-next 9`, custom in-memory IO,
  whole audio track decoded at open, `present_at` back-seek contract) or
  `android` (NDK `AMediaExtractor`/`AMediaCodec` via a `memfd`, YUV420→RGBA
  in pure Rust, runtime-untested).
* `VideoOverlay` currently **does not feed the mixer or the renderer**: it
  exposes `frameBytes`/`audioSample*` and composites over a mixing layer's
  `MainImage` via `compose_mixing`; `video_overlay_poll` is driven from
  `continuous_handler_poll`.

## 8. File system / storage today

* **No trait on the read path.** `engine::Storage` is concrete (`PathBuf` +
  `Vec<MountedArchive>`), using `fs::read_dir`/`fs::read`/`fs::metadata`;
  `xp3::Xp3Archive` owns a live `std::fs::File` and seeks/reads per segment.
* `tvp-streams` has a real `BinaryStream` trait (`binary.rs:57`) with
  `File`/`MemoryStream` impls, but `Storage`/`Xp3Archive` do not use it.
* Direct `std::fs` in natives: `Storages` list/delete/copy/flush,
  `Debug.startLogToFile`, `System.readRegValue`, bitmap/layer save, logging
  and state files.
* **Out-of-band C++ IO**: `tjs/cpp/streams.cpp` uses `std::fopen` for
  TJS `Array.save/load/saveStruct`, bypassing Rust storage entirely.
* Platform reality (from `docs/android.md`, `docs/portability.md`):
  Android uses SAF-to-path + `MANAGE_EXTERNAL_STORAGE` (option A); a real
  SAF/`AssetManager` backend (option B) was explicitly deferred.

## 9. The de-facto middle layer today

It is not an API; it is a convention:

* process-global statics (`tvp-natives::ENGINE`, `tvp-visual::SCENE`/
  `STORAGE`/`ENGINE`, `tvp-sound::GLOBAL_MIXER`, `tvp-input`, …),
* `register_*` functions that take the VM and hand it raw `extern "C"`
  callbacks,
* `set_*_context` setters that install `Storage`/`Scene`/`SystemContext`,
* one `Arc<RwLock<Scene>>` for visual state,
* static queues for window/menu requests (`natives/window.rs`) drained by
  `consume_window_requests`.

The tjs2 ABI cannot pass object handles for native instance property setters
or object arguments (documented at length in `docs/gdiplus.md §0`), so many
object semantics are implemented in a generated TJS wrapper layer.

## 10. The concrete seams the migration must cut

1. **Scene → GPU sync** (`render/src/sync.rs`): replace with a
   `RenderScene` tree maintained by the middle layer and consumed by the L1
   `RenderPipeline`.
2. **Bevy app/loop** (`render/src/runner.rs`, `main.rs`): replace with a
   winit event loop + fixed-step VM drive.
3. **Bevy UI menu** (`render/src/menu.rs`): needs an owned replacement
   (CPU/atlas-drawn UI over the same render list is the natural choice).
4. **Input** (`render/src/input_bridge.rs`, `touch_gesture.rs`): keep the
   pure hit-test logic; replace the Bevy event source with winit events.
5. **Media**: introduce the media engine API and move `decode_image`,
   `tvp-sound::decode`, and `video/*` behind it.
6. **Storage**: introduce the stateless FS surface; move `Storage`/`Xp3Archive`
   onto it;
   decide how to deal with `cpp/streams.cpp` `fopen`.
7. **Android**: replace `#[bevy_main]` with `android-activity` + owning the
   `ANativeWindow`/surface lifecycle for the Vulkan swapchain.

Good news for the plan: items 1-4 are the *only* Bevy-coupled pieces, and
the pure helpers inside them (`compose_layer`, `visible_image_region`,
`rect_center`, `window_to_game_transform`, `window_logical_size`,
`host_window_resize`, `window_layer_order`, `hit_test`, `touch_gesture`,
blend-mode mapping) are already unit-tested and can be moved out almost
verbatim.
