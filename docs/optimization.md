# Optimization notes: current hot paths

This document records where the current engine spends time and which
optimizations are in place. The measured C++ VM numbers and the frame
instrument live in [`performance.md`](performance.md); the start-up
instrument is in [`startup-attribution.md`](startup-attribution.md). Numbers
are only stated here where a repository script reproduces them.

---

## The frame path

`app::GameHost::step_with_pacing` runs the frame body (timers, sound, video,
async/continuous handlers, project-and-render) and logs one `warn` line when
the gap since the previous frame reaches 250 ms. The line attributes the gap
to the previous frame's phases and to the current wait (`platform_wait`,
`input`, `unaccounted`), so a slow frame names its own cost rather than hiding
it. A fast frame does no formatting. Details and the real device example are
in [`performance.md`](performance.md) §4.

The Vulkan backend times `vkAcquireNextImageKHR`, `vkQueuePresentKHR`, the
per-image fence waits and every swapchain recreation, exposing them as
`FrameTimings` that the slow-frame line reports.

**Measured:** the C++ VM configurations in `performance.md` §5.
**Unmeasured here:** the Rust-side frame budgets on a release build; the slow
line is the instrument that produces them on a device.

---

## Text

Script-visible text is drawn by compositing per-character coverage into the
layer's `BitmapState` (`engine::text::rasterize_text_coverage` /
`compose_text_into`), so `getPixel` and every other readback observe the same
pixels the reference produces. This CPU composite is the production path.

The `GlyphAtlas` + `project_text` GPU path exists in L2 and rasterizes the
same coverage once, but no backend is wired to it: `RenderContent::Glyphs` is
unanswered by the CPU and Vulkan renderers, and the atlas requires a live
`RenderPipeline` that L3 does not have. Text caches are bounded by
`MAX_TEXT_CACHE_ENTRIES` (1300), matching the reference
`TVP_CH_MAX_CACHE_COUNT`
(`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:108`).

**Unmeasured:** the per-glyph and per-paragraph cost on the current release
build. The atlas is the natural place to measure if text becomes a hotspot.

---

## Images and archives

Image decode is synchronous on the VM thread (`compat` native → L1 `media`
decoder), and the start-up sequence counts and times it in `StartupStats`
(`crates/app/src/startup.rs`): images decoded, total decode time, audio/video
decoders opened, and entry scripts run. The TLG decoder is a native Rust port
(`crates/platform/media/src/tlg.rs`); other formats go through the `image`
crate.

The archive index is parsed once when `fs` mounts an `.xp3`; the start-up line
reports the archive count, entry count, byte total and parse time
(`crates/app/src/game.rs`). A synchronous decode or a large archive index shows
up in those two start-up lines rather than in a frame.

**Unmeasured:** whether decode should move to a worker pool. The start-up
counters are the evidence to decide it; there is no worker-pool
implementation for images today.

---

## Audio

The L2 mixer (`crates/engine/src/sound.rs`) renders caller-owned output
buffers; the L1 `AudioDevice` (`platform`) pulls them into its callback. A
streaming source owns a decode-ahead worker thread and a bounded window as
explicit state, so long tracks are not materialized whole in memory. Whole-file
and streaming decode live in `crates/platform/media/src/audio.rs`.

The music level is bounded by the mixer's channel model and the streaming
window; there are no hardcoded chain or size caps.

**Measured:** none retained here. The mixer has no micro-benchmark in the tree;
`crates/engine/tests/sound.rs` asserts behaviour, not throughput.

---

## C++ VM build flags

`crates/tjs/build.rs` builds the vendored VM at `-O2` by default, overridable
with `TJS2_OPT`, and appends `TJS2_EXTRA_CXXFLAGS` after the optimisation
level for A/B tests. `scripts/perf/bench-tjs-opt.sh` reproduces the measured
configuration table (`performance.md` §5): `-O3` is flat, `-O2 -flto` is a real
but deferred win (it needs an LLVM-aware final link and device evidence), and
`-fvisibility=hidden -fno-semantic-interposition` is flat. The harness builds
with `zig c++` and runs no cargo.

---

## Rendering and shape rasterization

GDI+ shapes and text coverage composite through the L2 `engine::raster`
CPU primitives (`fill_polygon`, `fill_polygon_hatch`, `stroke_polyline`,
`hatch_foreground`). The Vulkan pipeline does the composition of the layer
tree; the CPU path is authoritative for script readbacks.

**Unmeasured:** the per-shape fill cost. If a title draws many large vector
shapes per frame, `fill_polygon`'s per-pixel edge test is the first thing to
profile.
