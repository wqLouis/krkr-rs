# 07 — Migration roadmap

Status: plan. Phases are ordered so the game stays runnable after each one.
No phase begins implementation until this plan is reviewed.

---

## 1. Strategy: strangler, not big bang

The new architecture is introduced **alongside** the current one:

* the middle-layer API traits are added first, with a **Bevy-backed
  adapter** implementing them, so `krkr-engine` can be written against the
  API while Bevy still renders;
* media and FS traits get adapters that wrap today's code
  (`decode.rs`, `tlg.rs`, `engine::Storage`, `xp3`) without moving it;
* the Vulkan core is built behind the same traits and selected by a runtime
  flag (`--renderer vk` / `--renderer bevy`) during the overlap;
* Bevy is deleted only after the Vulkan path reaches parity on the real
  title and the Android build.

Every phase must leave:
* `cargo test` green,
* `scripts/native_parity.py` green,
* `krkr-rs run <game> --headless` producing the same scene dump,
* the windowed game playable.

## 2. Phases

### Phase 0 — Plan and branch (this branch)

Deliverable: `docs/migration/*` (this directory).

Exit criteria: plan reviewed; open questions in §7 answered; no code change.

### Phase 1 — Define the middle-layer API; extract pure helpers

Deliverables:
* `krkr-api`: traits + DTOs from `03-middle-layer-api.md` (signatures,
  `BlendMode`, `RenderList`, `DecodedImage`, `AudioSpec`, `RgbaFrame`, …).
* Move the pure, already-tested helpers out of `render`:
  `compose_layer`, `visible_image_region`, `rect_center`,
  `window_to_game_transform`, `window_logical_size`, `host_window_resize`,
  `clamp_opacity`, `window_layer_order`, `sprite_z`, `bitmap_tint`,
  `fill_sprite_color`, and the TVP→`BlendMode` mapping.
* Add a `MemoryFileSystem`, `NullRenderer`, `MockMediaEngine`, `MockClock`
  so the engine can be constructed in tests without any backend.

Exit criteria:
* helpers live in a Bevy-free crate and their existing tests pass there;
* no behavior change in the renderer.

### Phase 2 — Build the L2 layer system on the API

Deliverables:
* `krkr-engine`: the `Scene`/`LayerState`/`BitmapState` model moves here
  (out of `tvp-visual`), plus the `Scene → RenderList` flattener.
* `Engine` struct with injected backends (`03-middle-layer-api.md §8`).
* A `BevyRenderer` adapter implementing `Renderer` by wrapping
  `sync_scene`, so the game still runs through Bevy.
* `tvp-visual` becomes the native adapter over `krkr-engine` (public
  methods preserved as shims).

Exit criteria: identical rendering through the adapter; no native behavior
change; `tvp-visual` no longer owns `Scene`.

### Phase 3 — File system trait

Deliverables:
* `krkr-fs` `StdFileSystem` + `MemoryFileSystem`.
* `xp3::Xp3Archive` reads from a `Stream`/`Vec<u8>` (positional reads).
* `engine::Storage` moves onto the trait (shimmed public API).
* logging/state/savedata/native IO routed through the trait.

Exit criteria: `Storage` behaves identically against `MemoryFileSystem`
fixtures and the real game; no `std::fs` left outside `krkr-fs` except
tests. Decide the `cpp/streams.cpp` hook (`06-file-system.md §6`).

### Phase 4 — Media engine

Deliverables:
* `krkr-media`: `ImageDecoder` (TLG + `image`), audio decode/stream,
  `FfmpegMedia`, `UnsupportedMedia`; `MediaCodecMedia` after Linux parity.
* `tvp-sound` decode calls the engine; mixer/model stay.
* video frame + PCM sinks in the video overlay.
* Async completion drain on the host thread.

Exit criteria: BGM/voice/image load through the engine; video is visible and
audible (Linux); Android compiles with `MediaCodecMedia`.

### Phase 5 — Vulkan renderer bring-up

Deliverables:
* `krkr-render`: `ash` device/swapchain/pipelines, texture upload
  (sub-rect), `RenderList` submission, `SourceOver`/fixed-function blends.
* Golden-image test harness (Bevy vs Vulkan framebuffer compare).
* `--renderer vk` runtime selection while Bevy remains the default.

Exit criteria: the demo scene and the real title render acceptably under
Vulkan; fixed-function blend matrix passes; no Bevy in the Vulkan path.

Phase 5b (can follow immediately): destination-dependent blends via the
offscreen ping-pong path; then GPU transitions if wanted.

### Phase 6 — Replace the Bevy host

Deliverables:
* `winit` event loop + `InputSource` + `HostWindow`; fixed-step VM drive.
* `input_bridge`/`touch_gesture` split: raw events from the host, logic in
  L2.
* CPU-drawn menu replacing `render/src/menu.rs` Bevy UI.
* Remove `VM_RUN_LOCK` (no parallel scheduler anymore).

Exit criteria: the windowed game runs with no Bevy crate on the desktop
path.

### Phase 7 — Android host

Deliverables:
* `android-activity` host replacing `#[bevy_main]`; Vulkan surface from the
  `ANativeWindow`; own swapchain lifecycle.
* Preserve the JNI contract (`pendingGameDir`/`pendingGameName`/
  `pendingStateDir`), state file, log rotation, touch/back handling.
* `MediaCodecMedia` video; `krkr-fs` Android paths.

Exit criteria: APK builds and runs the game on device with Vulkan.

### Phase 8 — Delete Bevy and rename crates

Deliverables:
* remove `bevy` from every manifest and from `Cargo.lock`;
* final crate renames (`tvp-*` → `krkr-*`) if desired, in reviewable
  commits;
* update README, `docs/portability.md`, `docs/android.md`,
  `docs/optimization.md` to the new architecture.

Exit criteria: `02-target-architecture.md §9` acceptance criteria all hold.

## 3. Suggested commit/branch discipline

* One phase per feature branch off `migration/middle-layer-ash`, merged
  back only when the game is runnable.
* Keep the old and new implementations behind a feature flag until the
  phase's exit criteria pass, then delete the old one in a separate commit.
* Never move more than one boundary per commit (e.g. do not combine FS and
  media moves).

## 4. Testing strategy

| Level | What | Where |
|-------|------|-------|
| Unit | pure helpers, blend mapping, FS namespace, layout | L2/L1 crates |
| Golden image | Bevy vs Vulkan framebuffer compare per scripted scene | `krkr-render` |
| Blend matrix | one quad per `BlendMode` over a known background | `krkr-render` |
| Integration | `krkr-rs run <game> --headless` scene dump snapshot | `krkr-app` |
| Parity | `scripts/native_parity.py` (100% tracked coverage) | CI |
| Media | host-testable pure parts (YUV, TLG, decode wrappers) | `krkr-media` |
| FS | `MemoryFileSystem` fixtures, real-game mount | `krkr-fs`/`krkr-engine` |
| Android | compile + symbol checks in CI; device run manual | CI / manual |

Regression rule: the headless scene dump and the native-parity report are
the two coarse "nothing broke" signals and must be checked at every phase.

## 5. Metrics to watch

* startup decode time and BGM stall (baseline in `docs/optimization.md`:
  645 ms image decode, 8.5 s BGM);
* per-frame cost (Bevy sync baseline: 0.9 ms @23 layers, 4 ms @400);
* binary size and dependency count (`cargo tree`), especially after Bevy is
  removed;
* idle-frame work (should be ~zero via `Scene::revision`).

## 6. Risks and mitigations

| # | Risk | Likelihood | Impact | Mitigation |
|---|------|:---------:|:------:|------------|
| R1 | **Vulkan-only excludes devices/emulators** with no/old Vulkan, or flaky drivers. | Medium | High | Keep Bevy/GL as a fallback during migration; document min Vulkan; test on real devices; consider a GLES backend behind the same `Renderer` trait later (the trait makes it possible). |
| R2 | Reimplementing windowing/input/IME is more work than estimated. | High | Medium | Reuse the already-pure hit-test/gesture logic; winit is already a dependency; keep `VM_RUN_LOCK` until the host loop is proven. |
| R3 | Bevy UI menu loss causes visible regressions. | Medium | Medium | CPU-drawn menu early (Phase 6), golden-image it. |
| R4 | sRGB/color mismatch between Bevy and Vulkan paths. | High | Medium | Pin one convention (`Rgba8UnormSrgb` + linear shader math) and add a color test to the blend matrix. |
| R5 | CPU compositing/upload remains the bottleneck; Vulkan gains little. | Medium | Low | Sub-rect uploads, staging ring, revision fast path; GPU paths are an optimization behind the same API. |
| R6 | API churn: `RenderList`/`MediaEngine` need changes after L2 uses them. | Medium | Medium | Phase 1 mocks exercise the API before any backend; keep DTOs additive. |
| R7 | Android MediaCodec is still runtime-untested. | High | Medium | Treat as compile-verified until on-device; keep FFmpeg out only on Android; test on hardware in Phase 7. |
| R8 | `cpp/streams.cpp` `fopen` breaks Android savedata through the FS layer. | Medium | Medium | Decide the hook in Phase 3; option A (Rust callback) if savedata must route through FS. |
| R9 | Schedule/scope creep (this is a whole-engine rewrite). | High | High | Phased exit criteria; each phase independently shippable; default branch stays Bevy until parity. |
| R10 | TJS ABI limits (no object handles) surface as the layer system moves. | Medium | Medium | They already exist today; the move does not change the ABI. `docs/gdiplus.md §0` records the fix. |

## 7. Open questions for review

1. **Glyph engine placement**: `krkr-render` vs a standalone `krkr-glyph`
   crate vs `krkr-core`. It has no GPU dependency. (Plan: `krkr-render`
   initially.)
2. **GPU text ever?** If yes, does L2 keep CPU composition as the source of
   truth (recommended) or switch `MainImage` to a GPU texture with
   read-back? The latter breaks cheap pixel access.
3. **`cpp/streams.cpp`**: Rust IO hook vs Rust reimplementation vs leave.
4. **Crate renaming**: do the `tvp-*` crates become `krkr-*`, or do the
   `tvp-*` names stay (they mirror the reference and the TJS class names)?
5. **Vulkan fallback**: accept Vulkan-only (D4) or keep a GLES backend
   behind the `Renderer` trait for old devices?
6. **Destination-dependent blends & transitions**: implement via the GPU
   ping-pong compositor (plan) or keep them CPU-side and only blit results?
7. **`Engine` ownership**: one process-global engine (current) vs an
   injected instance (plan). Tests want injection; the TJS VM is one-per-
   process today.
8. **Android default**: keep SAF→path (option A) or invest in a SAF/asset
   FS backend (option B) now that the `FileSystem` trait exists?

## 8. Definition of done (architecture)

The migration is complete when, on both Linux and Android:

* the game runs on the Vulkan core with no Bevy dependency;
* the layer system, media, and file system sit in the layers defined in
  `02-target-architecture.md`, verified by the dependency acceptance tests
  in `02-target-architecture.md §9`;
* the old Bevy renderer, wgpu materials, and the `VM_RUN_LOCK` workaround
  are deleted;
* documentation (`README`, `docs/portability.md`, `docs/android.md`,
  `docs/optimization.md`) describes the new architecture, not the old one.
