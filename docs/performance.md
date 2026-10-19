# krkr-rs — performance: instruments and measured C++ VM flags

Status: **instruments + measured C++ VM build flags.** The per-frame line and
the start-up aggregates are the instruments that attribute a slow run;
[`optimization.md`](optimization.md) records the current hot paths and
[`startup-attribution.md`](startup-attribution.md) the start-up breakdown.

Machine for the measured numbers: 12 logical cores, 32 GB. The C++ numbers in
§5 are x86-64 host numbers from a standalone harness, not from cargo.

---

## 1. What is measured, and where

| Instrument | Output | Source |
|---|---|---|
| Start-up phases | `startup: archives mounted …`, `startup: images decoded …`, `startup: decoders opened audio=… video=…`, `startup: entry scripts count=…` and `startup: first frame N ms after open` | `crates/app/src/game.rs`, counters in `crates/app/src/startup.rs` |
| Per-script timing | `DEBUG startup: script <indent><name> in <ms> ms` | `crates/compat/src/natives/scripts.rs` |
| Per-native aggregate | `startup: native time (top 10): …` (`info` when a member reaches 50 ms) | `crates/compat/src/natives/mod.rs` |
| Slow frame | one `WARN frame … slow: …` line for a gap ≥ 250 ms | `crates/app/src/game.rs`; Vulkan timings from `crates/platform/render` |
| C++ VM build flags | one table row per configuration | `scripts/perf/bench-tjs-opt.sh`, `scripts/perf/tjs2_bench.cpp` |

The first four are described in detail in
[`startup-attribution.md`](startup-attribution.md); §4 below reads the slow
line. The C++ table and its A/B commands are §5.

---

## 2. Audio, mixer and decode

The current decode and mix path:

* L1 `media` decodes whole files and streams (`crates/platform/media/src/audio.rs`)
  with symphonia (wav, pcm, ogg, vorbis, flac, mp3) plus the Ogg Opus adapter.
  `probe_audio` reads only the container header. The decoder state is an
  explicit caller-owned `AudioDecoderState`; the pre-ops worker and streaming
  state share the compressed bytes through an `Arc<[u8]>` rather than copying
  the entry per open/seek.
* L2 `engine` owns the mixer (`crates/engine/src/sound.rs`). A streaming
  `SoundSource` owns a decode-ahead worker thread and a bounded window; a read
  outside the window reseeks the worker. `Mixer::mix` renders a caller-owned
  buffer, and the L1 `AudioDevice` pulls it into its own callback.
* L1 `platform` caps the queued audio at `MAX_BUFFER_MS = 2000` and pre-fills
  `PREFILL_MS = 150` (`crates/platform/platform/src/audio_queue.rs`), shared by
  the cpal and AAudio backends.

There is no retained mixer micro-benchmark in the tree; the slow-frame line's
`audio` phase and the start-up decoder count are the available evidence.

---

## 3. What is not measured here

* The per-glyph text raster and the per-shape GDI+ fill have no benchmark;
  [`optimization.md`](optimization.md) names the code paths to profile.
* Image decode is synchronous on the VM thread; whether it should move to a
  worker pool is decided from the start-up counters, not from a benchmark in
  the tree.
* The Rust release-build frame budget on a device is what the slow-frame line
  produces; no device number is claimed here.

## 4. Reading a slow run on a device

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
  gap - previous total - platform_wait`. A fast frame logs nothing. The two
  frames are reported separately so `gap` and `total` are never read as if
  they described the same frame.

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

### 4.1 Frame instrumentation

**Measurement only; no present mode, pacing or sleeping change.**

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
after a null frame. The real device gap is what the line measures; the host
gate only proves the instrument's shape.

**Gate.** The standard gate (AGENTS §0), in order:

```bash
uv run --project scripts krkr-gate gate       # static gate (main agent)
uv run --project scripts krkr-gate test       # both test suites
```

`gate` runs `cargo fmt --check`, the compiler-free checks in order,
`cargo clippy` for the workspace and for the `platform-android` host feature,
`android-target`, `surface-lock --update` + `surface-lock`, `native-parity`,
`plugin-parity --no-doc` and `host-facts`. `test` runs `cargo test
--workspace --no-fail-fast` and the two `platform` suites. One check runs on
its own with `krkr-gate <check>`.

The Android-only loop (`crates/android/src/host.rs`) is target-gated and is
**not** compiled by the host gate; its `FramePacing` plumbing is documented as
unverified until an Android build compiles it (AGENTS §0.3, `TODO.md` §14).

---

## 5. Build-level VM optimisation (vendored C++ `-O3` / LTO, Rust profile)

Status: **measured C++ configurations; A/B commands for the Rust and Android
builds.** The C++ numbers below are x86-64 host numbers produced by a
standalone harness, not by cargo; the Rust and Android commands are not run on
this host (see §5.5).

Machine: 12 logical cores, 32 GB, zig 0.16.0 (bundled clang), gcc 16.2.1,
bison. Sources: the exact set `crates/tjs/build.rs` compiles (35 vendored
tjs2 `*.cpp` + the three generated bison parsers + `tjs2_abi.cpp` +
`streams.cpp`) plus `scripts/perf/tjs2_bench.cpp`.

The 20.8 s entry-script figure is wall-clock around I/O stalls, not a single
VM call: in `krkr-rs-logs.txt` the timestamped lines around it show a ~14 s gap
(15:37:17 → 15:37:31) and then ~0.5 s per `load_layer_image` line while assets
are read, while the same run reports only 170 ms of actual image decode. The
VM-only cost of parsing and compiling all 48 real `.tjs` scripts is ~119 ms at
`-O2` (the `compile` column below). Build flags cannot move that wall-clock
number; the `startup:`/`slow:` lines distinguish a storage-bound entry script
from a VM-bound one.

### 5.1 Measured C++ configurations

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

### 5.2 Verdicts

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

### 5.3 Host A/B commands

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

### 5.4 Rust profile and panic strategy

Rust `lto = "thin"` → `"fat"` is not measured here. It is switchable without
editing the tree because Cargo exposes the profile through the environment:

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

### 5.5 Android A/B and what is unverified

The Android build links with the NDK clang driver (`build-android.sh` sets
`CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER`), so it is the one place the LTO
candidate can actually be tried. There is no NDK and no device on this host,
and the Android target is never built here (AGENTS §0.3), so none of this is
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

### 5.6 Allocator (mimalloc) — separate proposal, not compelling

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

### 5.7 PGO / BOLT — deferred

PGO and BOLT are both feasible in principle (clang is available, and the
workload here is a usable training input), but the only target that matters
for the Android startup number cannot be trained here: instrumented training
for `aarch64-linux-android` needs an NDK and a device (neither present), and a
Linux x86-64 profile has no evidence of transferring to arm64. BOLT needs
`llvm-bolt` and a Linux profile too. Deferred rather than proposed vaguely;
revisit only with a device in the loop.

### 5.8 Reproduce the C++ table by hand

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
