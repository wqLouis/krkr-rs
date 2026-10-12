# krkr-rs — Performance Audit (hot paths, measured wins, further opportunities)

Status: **targeted audit + implemented wins.** This document complements
[`optimization.md`](optimization.md) (the broad read-only audit). It focuses on
the paths owned by this pass and records **measured** before/after numbers,
plus findings in files owned by other agents (reported, not edited).

Machine: 12 logical cores, 32 GB. Build: **dev/debug** (`cargo build` /
`cargo test`, unoptimized) — every number below is debug unless marked
otherwise, because that is what this pass was asked to optimize. Debug
absolute numbers are several× release; the *ratios* are the useful signal.

Runner: the real title `/mnt/DATA/Games/Others/test` headless dumps
**1 window, 23 layers, 44 bitmaps, 24 fonts**. Machine load from concurrent
builds made end-to-end startup timing too noisy to quote (5.5–16.8 s across
three runs); decode/VM phase costs are referenced from `optimization.md`.

---

## 1. Methodology

* **Code reading** of the owned paths plus the callers that drive them.
* **Reproducible debug micro-benchmarks** live next to the code as `#[ignore]`
  tests and are run explicitly:
  * `crates/tvp-sound/tests/perf_probe.rs`
  * `crates/tvp-text/tests/perf_probe.rs`

  ```text
  cargo test -p tvp-sound --test perf_probe -- --ignored --nocapture --test-threads=1
  cargo test -p tvp-text  --test perf_probe -- --ignored --nocapture --test-threads=1
  ```

  They generate an in-memory PCM16 WAV (short = whole-file decode, ~130 s =
  streaming) and rasterize/lay out real CJK text with the system Noto CJK face.
* **Real-game headless** (`krkr-rs run <game> --headless`) for scene shape.
* Before/after was taken by measuring both code shapes **in the same binary**
  where possible (the batched `AudioTrack::read_frames` vs the old per-sample
  `AudioTrack::frame`, which is still public), and by temporarily reverting a
  single owned file for the text hasher. No `git stash` was used (shared
  working tree).

---

## 2. Hot spots found (with numbers)

| Area | Hot path | Measured (debug) | Verdict |
|---|---|---|---|
| Audio mixer | `Mixer::render_mix` reads one source frame at a time: for a **streaming** source each `AudioTrack::frame` takes the `StreamRing` mutex, and the loop calls it **twice per output sample** | 512-frame chunk ×20 000: per-sample `frame()` **1.13–1.14 s**; batched `read_frames()` **0.28–0.34 s** → **3.3–4.0×** | **Fixed** |
| Audio mixer | Same loop for a **whole-file** source: each `frame()` does a `OnceLock::get` + bounds check | per-sample **1.89–1.93 s** vs batched **0.90–1.22 s** → **1.55–2.16×** | **Fixed** |
| Audio mixer | `render_mix_advancing` allocated a fresh `Vec<Option<f64>>` watermark per audio callback | 512 frames ×2 ch, 20 000 calls: **1.21 s** total (≈60 µs/call) | **Fixed** (scratch reused) |
| Audio decode | `decode::open_setup` copied the entire compressed entry (`Cursor::new(bytes.to_vec())`) on **every** probe/open/seek; the async path paid it in `probe_audio`, again in the worker decode, and again on each streaming re-open | one 12,187 KiB copy = **0.30 ms** (34.5 GiB/s) | **Fixed** (`Arc<[u8]>`) |
| Text layout | `GlyphAtlas::slots` is a `HashMap<char, GlyphSlot>` with SipHash; hit once per laid-out character | cached `rasterize_char` **207.0 ns**; 3-line paragraph `layout` **20.4–21.7 µs/iter** | **Fixed** (FNV-1a) |
| Text atlas | New-glyph `rasterize_new` scanned the whole `cell×cell` grid and allocated a fresh scratch `Vec` per glyph | ~**105–120 µs/glyph**, dominated by `ab_glyph` outline rasterization | Scratch reuse measured **neutral** (446–460 ms / 4000 glyphs) → reverted |
| Text measure | `measure_width` re-derives the scaled face and looks up `glyph_id`/`h_advance` per char; no cache | **49 µs** per 20-char string | Reported below |
| Scene sync (not owned) | `upload_bitmap` clones the whole RGBA buffer on every dirty upload; `compose_layer` allocates a `Vec`+`HashSet` per layer per sync; `window_layer_order` is O(n²) in `scene.rs` | see `optimization.md` | Reported below |
| Blitting (not owned) | `layer_ops::stretch_blit` / `blit_over` / `affine_blit` do per-pixel `read_pixel`/`write_pixel`/`stretch_sample` with bounds checks, no row specialization | — | Reported below |
| Shape raster (scope mismatch) | `raster::fill_polygon` runs a 16-sample even-odd test per pixel, recomputing edge intersections | — | Reported below |
| Blend (`blend.rs`) | Pure mapping functions; the only lock (`warn_dst_dependent_once`, `Mutex<HashSet>`) is reached **only** for destination-dependent blend modes, which the title never uses (`type=2` everywhere) | no per-frame cost on the title | No change |

---

## 3. Implemented changes

### 3.1 tvp-sound — batch source-frame reads in the mixer

**`crates/tvp-sound/src/source.rs`, `crates/tvp-sound/src/mixer.rs`**

Added `AudioTrack::read_frames(start, &mut [Option<(f32,f32)>])` and
`StreamRing::read_frames`, which read a contiguous window under **one** ring
lock (or one `OnceLock` read for whole-file). `Mixer::render_mix` now computes
the source frame range the output chunk interpolates
(`start..=last+1`, min/max so a hand-set negative `rate` stays correct) and
reads it once per channel per callback, instead of two locked `frame()` calls
per output sample.

Also added a reused `Mixer.watermarks` scratch for
`render_mix_advancing`, removing the per-callback `Vec` allocation.

Before/after (512-frame chunk, per channel, debug):

| Source | Old (`frame` per sample) | New (`read_frames`) | Speed-up |
|---|---:|---:|---:|
| Streaming (ring mutex) | 1.133 s | 0.281 s | **4.03×** |
| Streaming (2nd run) | 1.139 s | 0.344 s | **3.31×** |
| Whole-file | 1.931 s | 0.896 s | **2.16×** |
| Whole-file (2nd run) | 1.893 s | 1.220 s | **1.55×** |

No semantic change: unavailable/underrun frames still render silence, the
last frame still clamps for `i0 + 1`, and interpolation is unchanged. All
existing sound tests (including the real-game BGM streaming/loop tests) pass.

### 3.2 tvp-sound — share compressed bytes with `Arc<[u8]>`

**`crates/tvp-sound/src/decode.rs`, `crates/tvp-sound/src/source.rs`**

`open_setup` now takes `&Arc<[u8]>` and hands `Cursor<Arc<[u8]>>` to
symphonia (`MediaSource` is implemented for `Cursor<T: AsRef<[u8]> + Send +
Sync>`), so the reader shares the caller's buffer **by refcount** instead of
`bytes.to_vec()`.

* `open_track_bytes_inner` converts the `Vec<u8>` to `Arc<[u8]>` **once**;
  the probe, the whole-file/streaming worker, and every streaming re-open
  (initial open plus each seek/loop wrap) all share it.
* Public signatures are unchanged: `probe_audio(&[u8])`,
  `decode_audio_bytes(&[u8])`, and `StreamDecoder::open(&[u8])` still exist
  and copy once for external callers; the async path uses the new
  `pub(crate)` `_arc` variants.

A 12,187 KiB compressed entry now costs **one** 0.30 ms copy instead of two
to three (measured `Vec::clone` = 0.30 ms/clone, 34.5 GiB/s). The saving is
proportional to entry size and repeats on every streaming seek/loop wrap.

### 3.3 tvp-text — FNV-1a hasher for the glyph atlas map

**`crates/tvp-text/src/atlas.rs`**

`GlyphAtlas::slots` now uses `HashMap<char, GlyphSlot,
BuildHasherDefault<FnvHasher>>`. `char` keys are tiny and the map is hit for
every laid-out character; the default SipHash lookup dominated layout.

Before/after (debug):

| Probe | Before (SipHash) | After (FNV-1a) | Speed-up |
|---|---:|---:|---:|
| Cached `rasterize_char`, 2 000 000 lookups | 207.0 ns/lookup | 117.4 ns/lookup | **1.76×** |
| `layout` 3-line paragraph, 20 000 iters | 20.4–21.7 µs/iter | 13.07 µs/iter | **1.6–1.66×** |

Hashing is behavior-neutral; all text tests pass.

### Negative result (reverted)

Reusing a per-atlas `cell×cell` scratch and copying only the ink rectangle
instead of the whole cell measured **neutral** (446.4 ms HEAD vs 459.9 ms new
for 4000 glyphs — within run-to-run noise) because `ab_glyph` outline
rasterization dominates. Reverted to keep the diff minimal; not worth the risk.

### Could not apply (scope mismatch)

The brief lists `crates/tvp-visual/src/raster.rs`, but the code lives at
`crates/tvp-visual/src/natives/raster.rs`, which is outside this pass's edit
scope. Findings are reported in §4.1 instead. `crates/platform/render/src/blend.rs`
was inspected and needed no change (§2).

---

## 4. Prioritized further opportunities

### 4.1 Owned-area follow-ups

1. **`measure_width` memoization (`tvp-text`).** ~49 µs per 20-char string in
   debug because it re-derives the scaled face and does a fresh cmap/hmtx
   lookup per character. A process-global
   `(face_id, pixel_height) → HashMap<char, i32>` advance memo (the advance of
   a glyph in a face is immutable) would cut this to a map lookup per char,
   with the same S/M/L-effort profile as the atlas hasher win. Keep the
   missing-glyph rule (`id == 0 → pixel_height`) exact.
2. **`measure_width`/atlas metric reuse (`tvp-text`).** `layout` already has
   the cached `GlyphSlot.advance`; a `measure` fast path that reuses the atlas
   for present glyphs and only falls back for absent ones would avoid
   duplicate cmap walks (careful: the atlas uses U+FFFD fallback while
   `measure_width` counts missing glyphs as `pixel_height`).
3. **Batched/`SIMD` mixing (`tvp-sound`).** `render_mix` is still a scalar
   per-sample float loop; batching already removes the locking, and the
   interpolation arithmetic is now the remaining cost. Low priority in a
   debug VN build.
4. **Shape raster scanline fill (`tvp-visual/src/natives/raster.rs`, when in
   scope).** `fill_polygon`/`fill_polygon_hatch` call `point_in_polygon`
   16×/pixel and recompute `(xj-xi)/(yj-yi)` per edge per sample. A
   scanline/active-edge fill with per-edge precomputed deltas (keeping the
   4×4 coverage) would be a large win for GdiPlus fills. `fill_rect_replace`
   can also write whole row slices rather than per-pixel indexed copies.

### 4.2 Files owned by other agents (reported, not edited)

5. **`crates/platform/render/src/sync.rs` — RGBA clone on upload.** `upload_bitmap`
   still does `bitmap.rgba.clone()` into `Image::new` on every dirty upload.
   Moving the buffer (`std::mem::take` + refill, or uploading a sub-rect and
   only re-copying the changed region) removes a full-screen memcpy on every
   `drawText`/raster/transition repaint. High value for `@update`/`@blackout`
   and dialogue redraws.
6. **`crates/platform/render/src/sync.rs` — `compose_layer` per-layer allocations.**
   It allocates a `Vec` (ancestor chain) and a `HashSet` (cycle guard) for
   every layer on every mutating sync. Reuse scratch buffers held in
   `FrameBlendMaterials`, or bound ancestor depth with a fixed loop, to remove
   ~2 allocations × (layers) per frame while any layer animates.
7. **`crates/tvp-visual/src/scene.rs` — O(n²) traversal/lookups.**
   `window_layer_order` still scans all layers for missing children and uses
   `Vec::position` in a comparator; `layer()`/`bitmap()` are linear
   `iter().find()`. Add id→index maps and a parent→children index (as
   `optimization.md` item #2 recommends). This also speeds input hit-testing.
8. **`crates/tvp-visual/src/natives/layer_ops.rs` — per-pixel blits.**
   `stretch_blit`, `stretch_blit_mode`, `blit_over`, `blit_copy` and
   `affine_blit` do a `read_pixel`/`write_pixel`/`stretch_sample` call with a
   bounds check per pixel. Row-wise hoisting (clip once, walk `chunks_exact_mut(4)`,
   specialize nearest vs bilinear) would materially speed `copyRect`,
   `stretchCopy`, `drawImageStretch` and the `AffineLayer` composite.
9. **`crates/tvp-text/src/font.rs` — font discovery.** `discover_system_jp`
   rebuilds a `fontdb::Database` and calls `load_system_fonts()` on first use
   (~200–270 ms once per process). Warm it on a worker at startup and/or cache
   the `fontdb::Database`; low effort, removes a first-text stall.
10. **`crates/platform/render/src/blend.rs` — warning lock (low priority).**
    `warn_dst_dependent_once` uses a global `Mutex<HashSet>`. It is only
    reached for destination-dependent blend modes; if a game starts using
    them heavily, replace it with an `AtomicU32` bitmask keyed by mode so
    `render_path_for` never takes a lock.
11. **Image/audio decode still synchronous (per `optimization.md`).**
    Long-image TLG/PNG decode (645 ms startup) and long-BGM packet decode are
    already off-thread for audio; image decode workers + a prefetch cache
    remain the single largest startup latency opportunity and are outside this
    pass's ownership.

## 5. Files changed

| File | Change |
|---|---|
| `crates/tvp-sound/src/source.rs` | `Arc<[u8]>` compressed buffer; `AudioTrack::read_frames`; `StreamRing::read_frames` |
| `crates/tvp-sound/src/decode.rs` | `open_setup` takes `&Arc<[u8]>`; `probe_audio_arc`/`decode_audio_arc`/`StreamDecoder::open_arc`; public wrappers unchanged |
| `crates/tvp-sound/src/mixer.rs` | Batched frame window in `render_mix`; reused `watermarks` scratch in `render_mix_advancing` |
| `crates/tvp-text/src/atlas.rs` | FNV-1a `BuildHasherDefault` for the glyph-slot map |
| `crates/tvp-sound/tests/perf_probe.rs` | New ignored debug perf probes (mixer/decode/copy) |
| `crates/tvp-text/tests/perf_probe.rs` | New ignored debug perf probes (atlas/layout/measure) |
| `docs/performance.md` | This document |

Validation (debug): `cargo test -p tvp-sound -p render -p tvp-visual -p tvp-text`
green (all test binaries, 0 failures); `cargo clippy -p tvp-sound -p render
-p tvp-visual -p tvp-text --all-targets -- -D warnings` clean; `cargo fmt`
applied. `render`/`tvp-visual` were not modified by this pass — their sources
were being edited concurrently by other agents, so the full run was re-taken
after their in-progress edit settled.

---

## 6. Reading a slow run on a device

The Android build logs at `info` into `<filesDir>/krkr.log`, which the
launcher's Logs screen reads; `debug`/`trace` are selectable in the launcher
settings. Two families of lines carry the timing:

* **Per-frame attribution.** A frame whose wall-clock gap reaches 250 ms (the
  audio drain's playback cap, so it is the same frame that logs the existing
  `audio: ... exceeds the ... playback cap` warning) logs exactly one `warn`
  line, for example:

  ```text
  WARN frame 775 slow: gap=581.4 ms since the previous frame; the gap spans the previous frame, so these are its phases: timer=4.4 sound=0.0 video=0.0 async=0.0 continuous=0.0 project_and_render=0.0 body=4.4; its host: audio=17.6 host_sync=0.0 host_project=0.0 host_render=0.9 total=22.9; its gpu: acquire=0.0 present=0.0 recreate=0.0 fence_wait=0.0; this wait: platform_wait=15.6 input=0.2 unaccounted=542.9 ms
  ```

  `gap` is the platform's wall-clock interval since the previous frame
  started. The phases that explain that interval belong to the **previous**
  frame, so the line reports the previous frame's `timer`, `sound`, `video`,
  `async`, `continuous`, `project_and_render` and `body` (measured by
  `run_frame_body`), its `audio`, `host_sync`, `host_project`, `host_render`
  and `total` (the work outside the body), and its Vulkan call costs
  `acquire`, `present`, `recreate` and `fence_wait`. `platform_wait` and
  `input` are the current step's own wait (the event-loop poll, with input
  dispatch a subset of it), and `unaccounted =
  gap - previous total - platform_wait`. A fast frame logs nothing.

  This is the correction that makes the old line readable: previously the
  line printed the *current* frame's phases against the *previous* interval,
  so `gap=581.4` and `total=22.9` looked contradictory when they simply
  described different frames. The 581 ms in the real device log is the
  previous frame, and `unaccounted` now closes the budget instead of hiding
  the discrepancy.

* **Start-up aggregates.** After the startup scripts run, four `info` lines
  report archives mounted (count, index entries, bytes, milliseconds), images
  decoded, decoders opened, and entry scripts; the first frame then logs
  `startup: first frame N ms after open`.

Read the per-frame line first. A large previous `body`/one poll phase means
that poll; a large previous `host_render` means the render host; a large
`acquire`/`present`/`recreate`/`fence_wait` means the presentation call; a
large `platform_wait` means the event loop itself was blocked (a pause, a
suspend, or the platform starving the loop); a large `unaccounted` means loop
overhead or a stall outside both. The start-up lines separate mount/index
parse, decode, decoder open and script execution.

### 6.1 The 581 ms mobile stall — instrument and prediction

**The change (measurement only; no present mode, pacing or sleeping change).**

* `crates/platform/render/src/vulkan/mod.rs` times `vkAcquireNextImageKHR`,
  `vkQueuePresentKHR`, the frame and per-image `vkWaitForFences` calls, and
  every `recreate_swapchain` the frame triggers, exposing them as
  `FrameTimings { acquire_ms, present_ms, recreate_ms, fence_wait_ms }`.
* `crates/platform/render/src/lib.rs` defines `FrameTimings` and the free
  `pub fn last_frame_timings(state: &RenderState) -> Option<FrameTimings>`; the
  null backend records an all-zero value once a frame has rendered, so the
  shape is testable without a GPU.
* `crates/platform/platform/src` defines `FramePacing { poll_ms, input_ms }`,
  measured by the desktop loop (`linux.rs`) as the time between the end of the
  previous frame and the start of the current one, and by the Android loop
  (`crates/android/src/host.rs`) around each `poll_events` with input dispatch
  accumulated inside it. `EventHandler::frame` takes the `FramePacing`, so the
  data flow is one-way and per-frame instead of a mutable setter that a
  handler could read a frame late.
* `crates/app/src/game.rs` keeps one slow-frame line. `GameHost::step` stays
  as the no-measurement entry point (so out-of-tree callers and the headless
  loop are unchanged); `GameHost::step_with_pacing` carries the platform
  measurement and logs the previous frame's attribution. The stored
  `FrameAttribution` holds the phases, host report, GPU timings, audio cost
  and total of the last completed frame, and is overwritten only after the
  next frame runs.

**The expected device line.**

```text
WARN frame 774 slow: gap=581.4 ms since the previous frame; the gap spans the previous frame, so these are its phases: timer=<t> sound=<s> video=<v> async=<a> continuous=<c> project_and_render=<p> body=<b>; its host: audio=<au> host_sync=<hs> host_project=<hp> host_render=<hr> total=<tot>; its gpu: acquire=<ac> present=<pr> recreate=<rc> fence_wait=<fw>; this wait: platform_wait=<w> input=<i> unaccounted=<gap-tot-w> ms
```

**Falsifiable prediction.** From the real log (`krkr-rs-logs (2).txt`) the
frame that ends the 581.4 ms interval is preceded by a burst of
`DEBUG storage miss "FRM_02A43.<ext>"` lines — each one several KiB of
auto-path expansion (now compacted to ~0.5 KiB for the 41-path shape, grouped
by auto-path shape) — emitted on the image-load path inside the frame body.
The current line showed only the *next* frame's tiny `body=4.4`, which is
exactly why the 581 ms looked unaccounted. My prediction is therefore that the
new line will put the stall in the **previous frame's `body`** (most likely
`project_and_render` or `timer`, the poll that performs the load), with
`host_render`, `acquire`, `present`, `recreate` and `fence_wait` all small
(single-digit ms). If instead the new `acquire`/`present`/`fence_wait` fields
dominate, the cause is the Vulkan presentation and this prediction is wrong;
if `platform_wait` dominates, the app was paused or descheduled between
frames, not stalled in a frame. A device log settles it.

**Steady state cost.** A fast frame does one `f64` comparison in
`slow_frame_line` and returns `None`; no `String` is allocated and nothing is
formatted. The platform loops add a handful of `Instant::now()` reads per
frame (`linux.rs` two, the Android loop two plus per input batch). Formatting
happens only on a gap at or above 250 ms.

**Tests.** `crates/app/src/game.rs` unit-tests `FrameAttribution`:
`fast_gap_emits_nothing` and `first_frame_emits_nothing` assert no line for a
fast gap or a missing predecessor; `slow_line_names_the_previous_frame_and_balances_the_gap`
drives a deliberately slow previous phase (480 ms) and asserts every field
populates and that `unaccounted = gap - previous total - platform_wait`;
`slow_line_follows_the_most_recent_frame` shows the attribution moves to the
newly recorded frame. `crates/platform/render/src/lib.rs` asserts
`last_frame_timings` is `None` before a render and `Some(FrameTimings::default())`
after a null frame. Only a device can show the real 581 ms; this task ships
the instrument, not the answer.

**Gate.** The standard gate (AGENTS §0), in order:

```bash
cargo fmt --all -- --check
bash scripts/check-stateless.sh
bash scripts/check-error-handling.sh
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-fail-fast
cargo test -p platform --features platform-linux
cargo clippy -p platform --features platform-android --all-targets -- -D warnings
cargo test -p platform --features platform-android
python3 scripts/check-android-target.py
python3 scripts/surface_lock.py --update
python3 scripts/surface_lock.py
python3 scripts/native_parity.py
python3 scripts/plugin_parity.py --no-doc
```

The Android-only loop (`crates/android/src/host.rs`) is target-gated and is
**not** compiled by the host gate; its `FramePacing` plumbing is documented as
unverified until an Android build compiles it (AGENTS §0/§9).

---

## 7. Build-level VM optimisation (vendored C++ `-O3` / LTO, Rust profile)

Status: **measured, A/B prepared.** The C++ numbers below are x86-64 host
numbers produced by a standalone harness, not by cargo; the Rust and Android
changes are prepared with commands but were not built here (see §7.5).

Machine: 12 logical cores, 32 GB, zig 0.16.0 (bundled clang), gcc 16.2.1,
bison. Sources: the exact set `crates/tjs/build.rs` compiles (35 vendored
tjs2 `*.cpp` + the three generated bison parsers + `tjs2_abi.cpp` +
`streams.cpp`) plus `scripts/perf/tjs2_bench.cpp`.

The starting premise was a device log showing one entry script taking
20.8 s. The log in this tree (`krkr-rs-logs.txt`) is a **host** run, and in it
that line is wall-clock around I/O stalls, not a single VM call: the
timestamped lines around it show a ~14 s gap (15:37:17 → 15:37:31) and then
~0.5 s per `load_layer_image` line while assets are read, while the same run
reports only 170 ms of actual image decode. The VM-only cost of parsing and
compiling all 48 real `.tjs` scripts is ~119 ms at `-O2` (the `compile`
column below). Build flags cannot move that 20.8 s number; a device run with
the `startup:`/`slow:` lines is what would tell whether a device's entry-script
time is storage-bound or VM-bound.

### 7.1 Measured C++ configurations

Two measurements per binary, both single-threaded:

* **compile** — the 48 real-game `.tjs` files extracted from
  `/mnt/DATA/Games/Others/test/data.xp3`, compiled 20 times in one process
  (average per pass). The `.ks` files are KAG, not TJS, and the TJS2 compiler
  rejects them, so they are excluded.
* **exec** — `scripts/perf/workload.tjs`, a class/object/array/dictionary/
  string/Math workload (one execution per process). `check total` reads the
  script's accumulator back through the engine; it must be identical for every
  configuration.

Each configuration ran 7 processes; the table gives the median and
`[min-max]` across those runs.

| config | flags | compile (ms) | exec (ms) | vs `-O2` |
|---|---|---:|---:|---|
| `O2` | `-O2` | 118.9 [117.7–120.8] | 319.3 [315.8–326.0] | baseline |
| `O3` | `-O3` | 118.4 [118.1–119.9] | 327.8 [318.7–331.7] | 99.5% / 102.6% |
| `O2thin` | `-O2 -flto=thin` | 111.7 [110.6–111.9] | 303.2 [299.8–310.5] | 93.9% / 94.9% |
| `O2full` | `-O2 -flto` | 106.5 [104.7–109.3] | 295.5 [292.9–298.6] | 89.5% / 92.5% |
| `O2vis` | `-O2 -fvisibility=hidden -fno-semantic-interposition` | 117.4 [115.2–119.2] | 321.6 [314.3–329.8] | 98.7% / 100.7% |

`check total = 12023191456` and `compile ok=48 fail=0` for every
configuration.

### 7.2 Verdicts

* **`-O3` — reject.** Flat: compile 99.5%, exec 102.6% of `-O2` (the exec
difference is within run-to-run spread and leans the wrong way). No reason to
change the shipped `-O2`.
* **`-O2 -flto` — real but deferred; needs an LLVM link + device evidence.**
  Full LTO is the only measured win (compile 89.5%, exec 92.5% of `-O2`; thin
  LTO 93.9% / 94.9%). It is not adoptable as-is on the host: the C++ objects
  become LLVM bitcode, and the host's default final linker is gcc, which
  cannot consume them (`cc: file not recognized: file format not
  recognized`). Making it work needs an LLVM-aware final link (an
  `ld.lld -r --lto-O2` partial link, or `clang -fuse-ld=lld -flto`). The
  Android build already links with the NDK clang driver, so it is a candidate
  there — with a device A/B, never assumed from these x86-64 numbers.
* **`-fvisibility=hidden -fno-semantic-interposition` — reject.** Flat
  (98.7% / 100.7%): the TJS2 code is linked as a static archive into an
  executable, so symbol interposition was never costing a call.
* **Semantics guard.** No configuration here uses `-ffast-math`,
  `-funsafe-math-optimizations` or `-ffp-contract=fast`; float results are
  unchanged (the `exec` accumulator is compared across configurations). The
  `-O3` run re-exposed no UB over the 48 real scripts or the workload, but that
  is a smoke test, not the full compatibility suite.

### 7.3 Host A/B commands

The C++ harness (builds with `zig c++`; runs no cargo):

```bash
ZIGOUT=deps/zig-out scripts/perf/bench-tjs-opt.sh \
  --configs 'O2|-O2;O3|-O3;O2thin|-O2 -flto=thin;O2full|-O2 -flto;O2vis|-O2 -fvisibility=hidden -fno-semantic-interposition' \
  --runs 7 --iters 20
```

It extracts the `.tjs` corpus from the game XP3 on first run, generates the
bison parsers into `target/perf/tjs-opt/gen`, and prints one table row per
configuration with the raw `zig c++` compile and link lines on stderr.

For the real build, `crates/tjs/build.rs` now also honours
`TJS2_EXTRA_CXXFLAGS` (whitespace-separated, appended after the optimisation
level; unset by default) alongside the existing `TJS2_OPT`:

```bash
TJS2_OPT=-O3 cargo build --release -p krkr                       # -O3 A/B
TJS2_EXTRA_CXXFLAGS='-fvisibility=hidden' cargo build --release -p krkr
```

`TJS2_EXTRA_CXXFLAGS='-flto'` compiles, but the host link then fails on gcc as
above; it is intended for a link that uses the NDK/clang driver.

### 7.4 Rust profile and panic strategy

Rust `lto = "thin"` → `"fat"` is prepared, not measured here (cargo is out of
scope for this pass). It is switchable without editing the tree because Cargo
exposes the profile through the environment:

```bash
cargo build --release -p krkr                                   # as shipped
CARGO_PROFILE_RELEASE_LTO=fat cargo build --release -p krkr     # A/B
```

Adopting it means changing one line in `Cargo.toml`
(`[profile.release] lto = "fat"`); it raises build time and peak memory, so the
A/B needs a wall-clock build measurement, not just a runtime one.

**`panic = "abort"` — do not use as-is.** `crates/engine/src/timer.rs` relies
on unwinding: `RestoreCallback::drop` restores a taken timer callback *also on
unwind*, and the module documents that a panicking callback propagates after
the registry is made consistent. `crates/engine/tests/timer.rs` asserts that
contract with `catch_unwind`. `panic = "abort"` converts a panicking timer
callback (and any other script-triggerable panic) into a process abort, so it
is an observable behaviour change, not just a codegen knob; it is only safe if
the timer is deliberately changed to abort-on-panic.

### 7.5 Android A/B and what is unverified

The Android build links with the NDK clang driver (`build-android.sh` sets
`CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER`), so it is the one place the LTO
candidate can actually be tried. There is no NDK and no device on this host,
and the Android target is never built here (AGENTS §0/§11), so none of this is
verified:

```bash
# C++ full LTO (candidate)
TJS2_EXTRA_CXXFLAGS='-flto' \
CARGO_TARGET_AARCH64_LINUX_ANDROID_RUSTFLAGS='-C link-arg=-flto' \
  ./crates/android/build-android.sh --ndk "$ANDROID_NDK_HOME"

# Rust fat LTO
CARGO_PROFILE_RELEASE_LTO=fat ./crates/android/build-android.sh --ndk "$ANDROID_NDK_HOME"
```

The 16 KB page-alignment and arm64 staging checks in `build-android.sh` run
after the link, so they still gate an LTO build; they must stay green.
`TJS2_EXTRA_CXXFLAGS=-flto` also makes `check_object_arch` see bitcode (it
skips non-ELF input), so the final `.so` arch check is the guard that matters
there.

A device run confirms or rejects the candidate through the existing lines:
`startup: entry scripts count=N in M ms` for the parse+compile+execute cost,
and the `slow:`/`startup: first frame` lines for the frame path. The build
must show no new `TJS2`/VM errors and an identical script-observable result.

### 7.6 Allocator (mimalloc) — separate proposal, not compelling

An `LD_PRELOAD` counter over one `exec` workload run (20 000 objects, ~1M
interpreter operations) records `malloc=82501 calloc=104 realloc=0 free=2447
bytes=14547636` — about 0.08 allocations per operation, all on the VM thread:

```bash
gcc -O2 -shared -fPIC -o malloc_count.so scripts/perf/malloc_count.c -ldl
LD_PRELOAD=$PWD/malloc_count.so target/perf/tjs-opt/O2full/bench \
  exec scripts/perf/workload.tjs 1 total
```

The VM is single-threaded and the allocation rate is low, which is the case
where a replacement allocator helps least. It is kept as a separate proposal
with its own risk (global allocator swap, Android allocator interaction, RSS
behaviour) rather than part of the flag set, and it needs a before/after
number before adoption.

### 7.7 PGO / BOLT — deferred

PGO and BOLT are both feasible in principle (clang is available, and the
workload here is a usable training input), but the only target that matters
for the Android startup number cannot be trained here: instrumented training
for `aarch64-linux-android` needs an NDK and a device (neither present), and a
Linux x86-64 profile has no evidence of transferring to arm64. BOLT needs
`llvm-bolt` and a Linux profile too. Deferred rather than proposed vaguely;
revisit only with a device in the loop.

### 7.8 Reproduce the C++ table by hand

For one configuration (the exact command the harness runs, per source):

```bash
GEN=target/perf/tjs-opt/gen
zig c++ -std=c++17 -fPIC -pthread -fno-sanitize=undefined -DNDEBUG \
  -DTJS_TEXT_OUT_CRLF -D__STDC_CONSTANT_MACROS -DUSE_UNICODE_FSTRING \
  -DFMT_HEADER_ONLY -DSPDLOG_FMT_EXTERNAL -Wno-date-time \
  -Wno-tautological-constant-out-of-range-compare \
  -I crates/tjs/cpp/tjs2 -I "$GEN" -I crates/tjs/cpp -I deps/zig-out/include \
  -O2 -c crates/tjs/cpp/tjs2/<one>.cpp -o /tmp/obj/<one>.o
zig c++ -std=c++17 -O2 -I crates/tjs/cpp -I crates/tjs/cpp/tjs2 -I "$GEN" \
  -I deps/zig-out/include scripts/perf/tjs2_bench.cpp /tmp/obj/*.o \
  deps/zig-out/lib/libonig.a -lz -o /tmp/bench
/tmp/bench compile target/perf/tjs-opt/corpus 20 --tjs-only
/tmp/bench exec scripts/perf/workload.tjs 1 total
```
