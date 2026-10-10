# 07 — Migration roadmap

Status: plan. Phases are ordered so the game stays runnable after each one.
No phase begins implementation until this plan is reviewed.

> **Revision note (final layering, 2026).** This roadmap was written for the
> original layering (host on top, Bevy strangler). Two things changed since:
> the Bevy frontends were removed and the new crates
> (`api/fs/media/render/engine/app`) were implemented (waves 1–3), and
> the layering was finalised in
> [`02-target-architecture.md`](02-target-architecture.md) — host/platform
> code and archive parsing in **L1**, **L2** a generic engine that holds the
> layer system, **L3** the KiriKiri compat surface, **L4** the TJS VM. The
> remaining work is:
>
> 1. Move the host/window/event loop out of `app` into a `platform`
>    crate (L1), and make platform/backends compile-time features (D13).
> 2. Move XP3 parsing from `engine` into L1; keep only the VFS
>    namespace in L2 (D6 rev., doc 02 §5).
> 3. Replace `Storage::mount`/`discover_archives` with explicit archive
>    loading + the startup-discovery order (D15, doc 09).
> 4. Introduce `compat` (L3) and `vm` (L4): port the `tvp-*`
>    natives into L3 plugins, with the null-plugin object (D14, doc 08).
> 5. Media video + the TJS execution path (boot `startup.tjs`).
> 6. Android host in L1.
> 7. Delete `tvp-*` once their replacements land.
>
> The phase list below is kept as the historical record of the
> Bevy-strangler plan.

---

## 1. Strategy: strangler, not big bang

The new architecture is introduced **alongside** the current one:

* the stateless middle-layer API is added first, with a **Bevy-backed
  adapter** implementing its operations, so `engine` can be written
  against the API while Bevy still renders;
* media and FS operations get adapters that wrap today's code
  (`decode.rs`, `tlg.rs`, `engine::Storage`, `xp3`) without moving it;
* the Vulkan core is built behind the same API and selected by a runtime
  flag (`--renderer vk` / `--renderer bevy`) during the overlap;
* Bevy is deleted only after the Vulkan path reaches parity on the real
  title and the Android build.

Every phase must leave:
* `cargo test` green,
* the native-parity report green (the script was removed with the legacy crates; restore from `main` when the natives are ported),
* `krkr-rs run <game> --headless` producing the same scene dump,
* the windowed game playable.

## 2. Phases

### Phase 0 — Plan and branch (this branch)

Deliverable: `docs/migration/*` (this directory).

Exit criteria: plan reviewed; open questions in §7 answered; no code change.

### Phase 1 — Define the stateless middle-layer API; extract pure helpers

Deliverables:
* `api`: object/action signatures + state/DTO types from
  `03-middle-layer-api.md` (stateless `RenderPipeline`/`Media`/`FileSystem`,
  explicit `RenderScene`/`RenderState`/`DecoderState`/`FileState`,
  `BlendMode`, `DecodedImage`, `AudioSpec`, `RgbaFrame`, …).
* Move the pure, already-tested helpers out of `render`:
  `compose_layer`, `visible_image_region`, `rect_center`,
  `window_to_game_transform`, `window_logical_size`, `host_window_resize`,
  `clamp_opacity`, `window_layer_order`, `sprite_z`, `bitmap_tint`,
  `fill_sprite_color`, and the TVP→`BlendMode` mapping.
* Add test doubles: an in-memory `FileSystem`, `NullRenderPipeline`, a stub media backend,
  `MockClock`, so L2 can be constructed with no window/GPU/codec/fs.
* Add the **D9 guard**: a CI grep/test that rejects mutable statics,
  `thread_local!`, `LazyLock`/`OnceLock`/`OnceCell`/`lazy_static!` and
  module-level `HashMap` caches in `krkr-core`/`api`.

Exit criteria:
* helpers live in a Bevy-free crate and their existing tests pass there;
* no behavior change in the renderer;
* the D9 guard is green and two `Engine`s with independent backends can be
  constructed in one test process.

### Phase 2 — Build the L2 layer system on the API

Deliverables:
* `engine`: the `Scene`/`LayerState`/`BitmapState` model moves here
  (out of `tvp-visual`), plus the **`Scene → RenderScene` projection** (a
  field-level sync, not a draw list).
* `Engine` struct with injected stateless objects and stored state values
  (`03-middle-layer-api.md §8`).
* A Bevy-backed **platform adapter** implementing the stateless operations
  by wrapping the current `sync_scene` on the L1 side, so the game still
  runs through Bevy while L2 is written against the new API.
* `tvp-visual` becomes the native adapter over `engine` (public
  methods preserved as shims).

Exit criteria: identical rendering through the adapter; no native behavior
change; `tvp-visual` no longer owns `Scene`; L2 contains no draw-list or
traversal code.

### Phase 3 — File system (stateless)

Deliverables:
* `fs`: stateless `FileSystem` object + in-memory root, positional
  `FileState`.
* `xp3::Xp3Archive` reads positionally (no live `std::fs::File`).
* `engine::Storage` moves onto the stateless fs functions (shimmed public
  API); **L2 gains the namespace index and any buffering**.
* logging/state/savedata/native IO routed through the fs functions.

Exit criteria: `Storage` behaves identically against the in-memory root and
the real game; no `std::fs` left outside `fs` except tests. Decide the
`cpp/streams.cpp` hook (`06-file-system.md §6`).

### Phase 4 — Media engine

Deliverables:
* `media`: `decode_image`/`probe_*` free functions (TLG + `image`),
  `AudioDecoderState`/`VideoDecoderState` values, FFmpeg + unsupported
  backends; MediaCodec after Linux parity.
* `tvp-sound` decode calls the stateless API; mixer/model stay in L2.
* **L2 gains the worker pool and completion table**; L1 stays stateless.
* video frame + PCM sinks in the video overlay.

Exit criteria: BGM/voice/image load through the stateless API; video is
visible and audible (Linux); Android compiles with the MediaCodec backend.

### Phase 5 — Vulkan render pipeline bring-up

Deliverables:
* `render` `RenderPipeline`: `ash` device/swapchain/pipelines, tree
  traversal/composition, texture upload (sub-rect), `RenderScene`
  submission, `SourceOver`/fixed-function blends.
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
* MediaCodec video; `fs` Android roots.

Exit criteria: APK builds and runs the game on device with Vulkan.

### Phase 8 — Delete Bevy and rename crates

Deliverables:
* remove `bevy` from every manifest and from `Cargo.lock`;
* final crate renames (`tvp-*` → `krkr-*`) if desired, in reviewable
  commits;
* update README, `docs/portability.md`, `docs/android.md`,
  `docs/optimization.md` to the new architecture.

Exit criteria: `02-target-architecture.md §9` acceptance criteria all hold.

### Phase 9 — L2 owns the frame (D26 / D27)

Deliverables:
* `Engine::frame(now_ms, hooks)` + `Engine::run(clock, hooks)` + the
  `GameFrame` trait declared in L2 and implemented in L3/L4 (contract:
  `02-target-architecture.md` §3 “Frame ownership”);
* the frame clock moves into L2 — `GameHost::step(dt_ms, …)` stops owning
  `dt` and shrinks to wiring + `StepReport` plumbing;
* `compat::run_frame_body` collapses into a `GameFrame` implementation
  (it stops orchestrating project/render);
* `engine::timer` collapses to `create_interval` / `set_interval` /
  `set_enabled` / `remove`, with `advance` private to `frame` —
  `TimerConfig`, `TimerMode`, `TimerFire` and the introspection-only
  accessors go (D27);
* L2 advances mixer/video from its own frame clock, per
  `03-middle-layer-api.md` §5.

Exit criteria: no code above L2 holds a clock or calls `advance`; the
timer's public surface is the four methods; `cargo test -p engine -p
compat -p app` green with tests driving `frame(now)` virtually; doc 02 §3
“Implementation status: not built yet” section removed.

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
| **Statelessness (D9)** | grep/lint for globals + build two independent engines in one process | CI / `krkr-core` |
| Golden image | Bevy vs Vulkan framebuffer compare per scripted scene | `render` |
| Blend matrix | one quad per `BlendMode` over a known background | `render` |
| Integration | `krkr-rs run <game> --headless` scene dump snapshot | `app` |
| **Surface lock (D29)** | the live VM dispatch dump vs the committed baseline: `python3 scripts/surface_lock.py` — fails on any **added or removed** member | **gate, every commit** |
| Parity | reference-vs-Rust *report* of what is missing (coverage tracking — a presence metric, not a correctness claim) | report |
| Media | host-testable pure parts (YUV, TLG, decode wrappers) | `media` |
| FS | in-memory `FileSystem` fixtures, real-game mount | `fs`/`engine` |
| Android | compile + symbol checks in CI; device run manual | CI / manual |

Regression rule: the headless scene dump, the **surface lock** and the
native-parity report are the three coarse "nothing broke" signals and must be
checked at every phase and before every commit. The lock is the hard one: it is
the only signal that fails when someone *invents* surface or silently drops a
member. Behaviour is constrained separately — every semantic must be
cited to `reference/cpp` (D28).

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
| R1 | **Vulkan-only excludes devices/emulators** with no/old Vulkan, or flaky drivers. | Medium | High | Keep Bevy/GL as a fallback during migration; document min Vulkan; test on real devices; consider a GLES backend behind the same stateless ops later (the boundary makes it possible). |
| R2 | Reimplementing windowing/input/IME is more work than estimated. | High | Medium | Reuse the already-pure hit-test/gesture logic; winit is already a dependency; keep `VM_RUN_LOCK` until the host loop is proven. |
| R3 | Bevy UI menu loss causes visible regressions. | Medium | Medium | CPU-drawn menu early (Phase 6), golden-image it. |
| R4 | sRGB/color mismatch between Bevy and Vulkan paths. | High | Medium | Pin one convention (`Rgba8UnormSrgb` + linear shader math) and add a color test to the blend matrix. |
| R5 | CPU compositing/upload remains the bottleneck; Vulkan gains little. | Medium | Low | Sub-rect uploads, staging ring, revision fast path; GPU paths are an optimization behind the same API. |
| R6 | API churn: `RenderScene`/media API need changes after L2 uses them. | Medium | Medium | Phase 1 mocks exercise the API before any backend; keep DTOs additive. |
| R7 | Android MediaCodec is still runtime-untested. | High | Medium | Treat as compile-verified until on-device; keep FFmpeg out only on Android; test on hardware in Phase 7. |
| R8 | `cpp/streams.cpp` `fopen` breaks Android savedata through the FS layer. | Medium | Medium | Decide the hook in Phase 3; option A (Rust callback) if savedata must route through FS. |
| R9 | Schedule/scope creep (this is a whole-engine rewrite). | High | High | Phased exit criteria; each phase independently shippable; default branch stays Bevy until parity. |
| R10 | TJS ABI limits (no object handles) surface as the layer system moves. | Medium | Medium | They already exist today; the move does not change the ABI. `docs/gdiplus.md §0` records the fix. |

## 7. Open questions for review

1. **Glyph engine placement**: `render` vs a standalone `krkr-glyph`
   crate vs `krkr-core`. The `FontSystem`/`Font` objects have no GPU
   dependency. (Plan: `render` initially.)
2. **`RenderScene` ownership**: does L2 keep a *parallel* `RenderScene`
   projected from the TVP layer tree each frame (plan: simple field copy),
   or do the L2 layer nodes embed a `RenderNode` so there is one tree and no
   copy? The latter couples TVP state to the L1 type but avoids a sync pass.
2. **GPU text ever?** If yes, does L2 keep CPU composition as the source of
   truth (recommended) or switch `MainImage` to a GPU texture with
   read-back? The latter breaks cheap pixel access.
3. **`cpp/streams.cpp`**: Rust IO hook vs Rust reimplementation vs leave.
4. **Crate renaming**: do the `tvp-*` crates become `krkr-*`, or do the
   `tvp-*` names stay (they mirror the reference and the TJS class names)?
5. **Vulkan fallback**: accept Vulkan-only (D4) or keep a GLES backend
   behind the same stateless ops for old devices?
6. **Destination-dependent blends & transitions**: implement via the GPU
   ping-pong compositor (plan) or keep them CPU-side and only blit results?
7. **`Engine` ownership**: one process-global engine (current) vs an
   injected instance (plan). Tests want injection; the TJS VM is one-per-
   process today.
8. **Android default**: keep SAF→path (option A) or invest in a SAF/asset
   FS root (option B) now that the stateless fs surface exists?

## 8. Definition of done (architecture)

The migration is complete when, on both Linux and Android:

* the game runs on the Vulkan core with no Bevy dependency;
* the layer system state, media, and file system sit in the layers defined
  in `02-target-architecture.md`, and the render pipeline consumes the
  `RenderScene` tree, verified by the dependency acceptance tests in
  `02-target-architecture.md §9`;
* `engine` contains no render traversal, draw-list or batching code;
* the old Bevy renderer, wgpu materials, and the `VM_RUN_LOCK` workaround
  are deleted;
* documentation (`README`, `docs/portability.md`, `docs/android.md`,
  `docs/optimization.md`) describes the new architecture, not the old one.

## 9. Performance opportunities (merged from `10-performance-opportunities.md`)

Status: **forward-looking backlog.** **Deferred by D22** until a real game runs
end-to-end — correctness first, then invest. This complements the historical audits
[`../optimization.md`](../optimization.md) and [`../performance.md`](../performance.md),
which measured the *old* Bevy/tvp-* tree and are kept only as a record. Numbers
here are not measured yet; they are the places we know we can win, ordered by
expected impact for real KiriKiri titles (text-heavy, many TLG layers, long BGM).

The rule for every item below: **the CPU path is the correctness reference**
(D8). An optimisation may change *how* pixels are produced, never *what* a
script can read back through `getPixel`/readback.

### L1 `render`

| Opportunity | Why it matters | Notes |
|---|---|---|
| **GPU glyph runs** (`RenderContent::Glyphs`) | Today L2 emits **one `RenderContent::Texture` node per glyph**; a KAG screen with 2 000 characters = 2 000 nodes, transforms and draw submissions. This is the single biggest text cost. | Expand a run into **one instanced draw** sampling the L2 atlas. Vulkan backend currently returns `Unsupported` for `Glyphs` (`vulkan/mod.rs:30`); null counts it (`null.rs:546`). |
| **Programmable/advanced blends** (`VK_EXT_blend_operation_advanced`) | Dest-dependent `DstBlend` modes (Multiply/Screen/Overlay/ColorDodge/…) are CPU-only today; GPU scenes fall back or return `Unsupported`. | Exposes most modes as hardware blend ops; keep the CPU path for `ColorDodge5`/`Difference5` and GPUs lacking the feature. |
| **MSDF atlas** | Crisp text at arbitrary zoom/rotation without re-rasterising per size (KAG effects scale layers). | Generate once (CPU `msdfgen`/Rust port or compute pass); shader derives coverage. |
| **Async readback** | `read_render_target` waits on a fence; a script that samples every frame stalls the CPU↔GPU sync. | Ring-buffer of readback targets + deferred map; return the previous frame's pixels. |
| **Batching / pipeline cache** | Fewer pipeline switches and vertex uploads per frame. | Batch by `(texture, blend, clip)`; reuse the staging buffer; cache `VkPipeline` by state. |

### L1 `media`

| Opportunity | Why it matters | Notes |
|---|---|---|
| **Hardware video decode** | Software FFmpeg decode of 1080p is the dominant startup/scrub cost. | VAAPI/NVDEC on Linux, MediaCodec on Android; zero-copy frames into the render path. |
| **Streaming `video_audio`** | The current `video_audio` decodes the **whole** track eagerly (the scaffold has no range parameter). | Add a range/streaming API and decode on demand. |
| **SIMD image/TLG decode** | TLG is the most common KiriKiri image; startup decodes dozens. | SIMD predictor + colour conversion; parallel decode with a bounded pool. |
| **Faster inflate** | XP3 entries and `FE FE` script containers use zlib. | `libdeflate`/`zlib-ng` instead of the default backend. |

### L1 `fs`

| Opportunity | Why it matters | Notes |
|---|---|---|
| **Index lookup** | `find`/`stat` run per storage open; a title can open thousands per boot. | Hash the XP3 index once; avoid repeated linear scans; cache decoded paths/offsets. |
| **Decompress cache** | Re-opening the same entry re-inflates it. | Small LRU of decoded entries keyed by `(archive, offset)`; never mutate the mounted set. |
| **Memory-map archives** | Large direct-from-disk reads. | mmap the container on desktop; `pread` on Android. |

### L2 `engine`

| Opportunity | Why it matters | Notes |
|---|---|---|
| **Glyph cache by coverage, not colour** | The atlas key includes the **colour** today, so every text-colour change re-rasterises and re-uploads. KAG text recolours constantly. | Cache straight-alpha coverage only and tint per node (the Vulkan shader already takes a push-constant tint — `vulkan/draw.rs:34`); the per-glyph tint becomes the text colour. **Highest text-cache win.** |
| **Layout cache** | `cosmic-text` shaping runs every frame for unchanged labels. | Key by `(face, text, size, style, wrap-width)`, invalidate on change. |
| **Multi-page atlas + eviction** | One fixed atlas hits `AtlasFull` and then fails. | Grow to several pages; LRU-evict cold glyphs. |
| **Run batching** | Feed `RenderContent::Glyphs` once the L1 path exists. | Removes the per-glyph node overhead. |
| **Scene projection dirty flags** | `project` rebuilds the whole `RenderScene` each frame. | Track per-layer dirty bits; reuse unchanged subtrees. |
| **Mixer SIMD + quality** | Software mixing of several BGM/voice channels. | SIMD the gain/pan/resample inner loops; make resampler quality configurable. |

### L3 `compat` / L4 `vm`

| Opportunity | Why it matters | Notes |
|---|---|---|
| **Native dispatch** | Every property read/write is a TJS→C thunk. | Cache resolved members; avoid allocating dictionaries per call. |
| **Script compile cache** | Re-parsing scripts on reload. | Cache compiled bytecode by storage path + mtime. |

### How to measure
Add `#[ignore]` perf probes next to the code (the old tree used this pattern) and
a scene benchmark that rebuilds a title-like `Scene` (1 window, ~23 layers,
~34 bitmaps) so the text/layer counts match the real test title.
