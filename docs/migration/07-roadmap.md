# 07 — Roadmap and phase contract

Status: the four-layer rework is implemented. The phases below are the ones
that still own work: the frame-ownership finish (Phase 9), the pixel-translation
readback (Phase 10), and the deferred design work that stays out until a real
title runs end-to-end. Open items are tracked in [`TODO.md`](../../TODO.md);
this document records the phase contract and the exit criteria, so a landed
slice can be checked rather than remembered.

The layers, crate layout and acceptance criteria are in
[`02-target-architecture.md`](02-target-architecture.md); the presentation and
translation contract is in
[`11-presentation-and-pixel-translation.md`](11-presentation-and-pixel-translation.md).

---

## 1. Phase discipline

Every phase leaves a runnable game. A phase moves one boundary, lands as a
replacement (the superseded code, tests, allowlists and docs are deleted in the
same change), and ends with the gate green and the headless run producing the
same scene dump. Nothing is kept behind a feature flag: a half-moved boundary
is a change that is not finished yet, not a configuration.

The old and new implementations never coexist in the tree; git is the history.

## 2. Phases

### Phase 9a — presentation scaling removed

**Landed.** Presentation is Fit with letterbox only (D39). The
`ScalingMode`/`PresentationScaling`/`--scaling`/`ViewTransform`/
`RenderScene.view`/`platform::native_size` line is gone; the contract and the
keep-list are [`11` §4](11-presentation-and-pixel-translation.md).

Exit criteria, met: gate green; a headless and a windowed run present
one-to-one at the game's declared size; `git grep -n
'ScalingMode\|PresentationScaling\|native_size' crates` is empty (the GDI+
`Layer.*ViewTransform` surface stays); surface lock and both parity reports
unchanged.

### Phase 9 — L2 owns the frame (D26 / D27)

**Status: the frame slice has landed; the remaining pumps are still staged.**

This is the prerequisite for Phase 10: the pixel translation is recomputed per
size change *inside the frame*, and the frame is where projection happens.

Landed:

* `Engine::frame(now_ms, hooks)` + `Engine::step(dt_ms, hooks)` + the
  `GameFrame::before_render(now_ms, dt_ms)` trait declared in L2 and
  implemented by the composition root (`VmHook` runs the VM tick and the audio
  drain);
* the frame clock lives in L2 — `GameHost` no longer owns `now_ms` or a `dt`;
* `compat::run_frame_body` no longer fires timers or projects/renders: it
  drains the posts the engine produced, then runs sound/video/async/continuous
  and the movie/transition tail;
* L3 timer schedule writes read the injected frame clock, and the timer tests
  drive virtual time instead of the wall clock.

Remaining:

* move the mixer (audio pump), video and transition pumps from the hook into
  L2, then retire `SoundState.clock` and pass an L2-owned delta to those
  pumps;
* `Engine::run(clock, hooks)` (platform event pump) — blocked on the loop
  slice;
* privatize `TimerState::advance` (the four D27 methods — `create_interval`,
  `set_interval`, `set_enabled`, `remove` — already exist with no config
  struct or mode enum; `advance` is the only extra public method and runs
  inside `Engine::frame`).

Exit criteria for the slice: no code above L2 accumulates a frame clock, and
`cargo test -p engine -p compat -p app` is green with tests driving
`frame(now)` virtually. Both hold. The full Phase 9 exit (all pumps in L2)
still tracks the remaining list.

### Phase 10 — Pixel-translation layer (device resolution + readback)

**Status: the mapping landed; readback and device-resolution glyph
rasterization are open.**

The device-resolution presentation and the VM↔device boundary. Contract:
[`11`](11-presentation-and-pixel-translation.md) §6–§7.

Landed:

* `engine::translate::PixelTranslation` — Fit (D39) mapping with
  `to_device_*`/`to_vm_point`/`raster_scale`/`vm_pixel_region`, owned and
  recomputed by `Engine` on a VM or framebuffer size change (D40);
* the projection applies the mapping so the pipeline receives device-space
  rectangles; the overlay stays in device coordinates;
* input mapped device→VM (in the composition root, using the L2 mapping)
  before the VM tick and hit-testing;
* the OS window no longer rewrites the script's VM window size; a script
  window-size change is surfaced once and applied to the desktop window, while
  Android keeps the display surface and refits the declared size into it.

Open:

* glyph rasterization at `size_px · raster_scale()`, with **layout metrics
  still VM** so every script-visible measurement is unchanged (today glyphs
  are rasterized into the VM-resolution layer bitmap and the GPU scales the
  result; that is a D34 rasterization approximation, not a script-observable
  difference);
* readback through `vm_pixel_region`: the desktop and fallback path is a
  host-visible staging copy; on Android the backing is allocated as an
  `AHardwareBuffer` and read with `AHardwareBuffer_lock`
  ([`10-android-backends.md`](10-android-backends.md) "Device-pixel readback").
  Today `getPixel`/`piledCopy`/`saveLayerImage` read the CPU layer bitmap,
  which is already VM-exact.

Exit criteria: with `vm_size == device_size` the output is identical to Phase
9a (the mapping is the identity), so surface lock, parity and golden tests are
unchanged; the game then renders at device resolution with `getPixel`,
`piledCopy` and `saveLayerImage` addressed through the translation and every
script-observable value still VM and reference-exact.

### Deferred idea — render-resolution decoupling

A future option is to keep the game's VM size and CPU-rasterize at a fixed
higher render resolution (text at `Font.size · render_scale`), sampling that
backing to the window. It is documented here as a possible direction only: the
raw-buffer/plugin compatibility surface and the CPU-native reference path make
it a larger project, and it is **not implemented**.

## 3. Testing strategy

| Level | What | Where |
|-------|------|-------|
| Unit | pure helpers, blend mapping, FS namespace, layout | L2/L1 crates |
| **Statelessness (D9)** | `uv run --project scripts krkr-gate stateless` + build two independent engines in one process | gate |
| Backend parity | the null and Vulkan backends consume the same `RenderScene` contract; the null software rasterizer is the baseline | `render` |
| Blend matrix | one quad per fixed-function `BlendMode` over a known background | `render` |
| Integration | `krkr --headless` scene/phase log over the real title | `app` |
| **Surface lock (D29)** | the live VM dispatch dump vs the committed baseline: `uv run --project scripts krkr-gate surface-lock` — fails on any **added or removed** member | **gate, every commit** |
| Parity | reference-vs-Rust *report* of what is missing (coverage tracking — a presence metric, not a correctness claim) | report |
| Media | host-testable pure parts (YUV, TLG, decode wrappers) | `media` |
| FS | in-memory `FileSystem` fixtures, real-game mount | `fs`/`engine` |
| Android | `uv run --project scripts krkr-gate android-target`; device run manual | gate / manual |

Regression rule: the headless run, the **surface lock** and the native-parity
report are the three coarse "nothing broke" signals and must be checked at
every phase and before every commit. The lock is the hard one: it is the only
signal that fails when someone *invents* surface or silently drops a member.
Behaviour is constrained separately — every semantic must be cited to
`reference/cpp` (D28).

## 4. Risks

| # | Risk | Likelihood | Impact | Mitigation |
|---|------|:---------:|:------:|------------|
| R1 | **Vulkan-only excludes devices/emulators** with no/old Vulkan, or flaky drivers (D4). | Medium | High | Document the minimum Vulkan; test on real devices; a GLES backend behind the same stateless ops stays possible without changing L2. |
| R2 | Windowing/input/IME is more work than estimated (TODO §7). | High | Medium | Keep the pure hit-test/gesture logic in L2; the platform loop stays in L1; the input imprecisions are listed in `TODO.md` §7. |
| R3 | sRGB/colour mismatch across the CPU and Vulkan paths. | Medium | Medium | Pin one convention (`Rgba8UnormSrgb` + linear shader math) and keep the colour case in the blend matrix. |
| R4 | CPU compositing/upload remains the bottleneck; Vulkan gains little. | Medium | Low | Sub-rect uploads, the staging ring, the revision fast path; GPU paths are an optimization behind the same API (D8). |
| R5 | `BitmapState` carries no dirty region, so a full-frame upload cannot be narrowed without a cross-cutting L2 change. | Medium | Medium | Treat as open design work; measure before changing every pixel-writing site. |
| R6 | Android MediaCodec is still runtime-untested. | High | Medium | Compile-verified until an on-device run; FFmpeg stays off only on Android. |
| R7 | `crates/tjs/cpp/streams.cpp` `fopen` and the serializer break savedata on Android, and every save-path memory-safety defect so far was there. | Medium | Medium | The C++ adapter over a Rust implementation is deferred (TODO §16); reproduce against the reference before changing it. |
| R8 | TJS ABI limits (no object handles) surface as the layer system moves. | Medium | Medium | They already exist today; the move does not change the ABI. `docs/gdiplus.md` §0 records the workaround. |

## 5. Definition of done (architecture)

The migration is complete when, on both Linux and Android:

* the game runs on the Vulkan core, and the tree carries no ECS or `tvp-*`
  dependency;
* the layer system state, media and file system sit in the layers defined in
  `02-target-architecture.md`, and the render pipeline consumes the
  `RenderScene` tree, verified by the dependency acceptance tests in
  `02-target-architecture.md` §9;
* `engine` contains no draw-list, batching or draw-call code (traversal and
  drawing are L1);
* `compat` reaches the engine only through `compat/src/engine_api.rs`
  ([`14-l3-l2-protocol.md`](14-l3-l2-protocol.md)), and the `layering` check
  is green without a new exception;
* documentation describes the current tree, not a superseded one.

## 6. Performance

Performance work is deferred by D22 until a real game runs end-to-end.
[`../optimization.md`](../optimization.md) records the current hot paths and
the in-place optimizations; [`../performance.md`](../performance.md) holds the
instruments and the measured C++ VM build flags. The deferred items (a GPU text
path, an L3 render-target surface, zero-copy presentation, C++ LTO) are listed
in `TODO.md` §16, not here.
