# krkr-rs — Android frame-time and power audit

Audit only. **No rendering semantics or present timing were changed by this
task.** Deliverable: `docs/audits/perf.md`.

Scope: why the port is slower and draws more power than the reference C++
engine on a real Android device, ranked by expected impact on frame time and
power. Every behaviour the reference decides carries a
`reference/cpp/...:line` citation; every code location is `file:line`. Each
item is marked **safe-to-adopt** (no script-observable change) or
**needs-review** (present timing, cross-cutting L2 change, or a D34
script-observable value).

Evidence frame (Android, from the slow-frame line this task instruments):

```
frame 1719 slow: gap=389.0 ... timer=19.6 sound=0.0 video=0.0 async=0.0
continuous=0.0 project_and_render=0.0 body=19.6; its host: audio=33.6
host_sync=0.1 host_project=1.3 host_render=0.1 total=388.7; ... gpu:
acquire=0.0 present=0.0 recreate=0.0 fence_wait=0.0; this wait:
platform_wait=0.2 input=0.0 unaccounted=0.0
```

The Vulkan `render` call is 0.1 ms, yet the frame totals 388.7 ms: ~334 ms is
inside `vm.tick`/`host.step` and was not attributed by the six frame hooks.
Item **#9** closes that measurement gap; items **#1–#8** are the ranked
proposals.

---

## Measured baseline (desktop, windowed Vulkan, real title)

A windowed run of the release binary on `/mnt/DATA/Games/Others/test/`
(Yuzusoft, `data.xp3` + `patch.xp3`) with `--backend vulkan --scaling fit`, at
the 60 fps steady state, 347 clean frames:

| phase | p50 | p90 | max |
|---|---|---|---|
| total | 1.20 ms | 1.90 | 4.90 |
| **audio (pump)** | **1.10 ms** | 1.20 | 2.80 |
| host_total | 0.10 | 0.80 | 3.00 |
| host_render | 0.10 | 0.70 | 3.00 |
| present | 0.00 | 0.70 | 2.90 |
| submit | 0.00 | 0.00 | 0.10 |
| sync_upload / sync_prune / host_project | 0.00 | 0.00 | 0.00 |
| vm tick / input_* / polls / body | 0.00 | 0.00 | 0.10 |

CPU: 8.9 % of one core (main thread 6.5 %, `tvp-audio` 2.0 %). GPU: 4–5 %
util, 40–48 % memory controller, ~6.7 W, P8. gdb sampling found the main
thread blocked in the NVIDIA/Wayland present wait and the audio workers in
`futex`; no user-space hot loop outside the mixer.

**The port is not render-bound.** Render + present is ~0.1 ms of CPU; the frame
is ~90 % audio-mixer CPU. The measured ranking is:

1. **Audio mixer per-sample streaming reads** — the CPU/power root cause:
   `accumulate_source` takes the buffer lock and calls `notify_all` twice per
   output frame per playing streaming source (~88,200 lock+notify pairs/s at
   44.1 kHz stereo). Fix: bulk window read + notify only on lookahead advance.
2. **Unconditional draw + present every frame** — a real deviation from the
   reference `RequestUpdate` mechanism, but a **power/GPU duty-cycle** one, not
   a slow-frame one (render is 0.1 ms). This is a root-cause fix for the
   always-present deviation, **not** a mitigation.
3. The per-frame scans/allocations and full-bitmap re-upload (#4/#5) measured
   **0.00 ms** on this title; they scale with scene size and remain
   robustness fixes.

Caveats: the slow-line only fires on a gap ≥ 250 ms, so it was forced with
`SIGSTOP`/`SIGCONT` (the phase fields describe the previous real frame); the
release binary is stripped, so gdb sampling is library-symbol-only; the
mixer-vs-`device.write` split inside `audio = 1.10 ms` still needs a sub-timer
(the code shows the write is a mutex + `VecDeque` push, so the mixer dominates).
**The 388 ms Android stall does not reproduce on the desktop** (max clean frame
4.9 ms); it must be localized on-device with the `host_total`/`tick`/`input_*`/
`submit` fields before committing to a fix — lazy rendering will not explain it
if the time is inside `vm.tick`/`host.step`.

---

## 0. Ranked findings

| # | Finding | Where | Class | Expected impact |
|---|---|---|---|---|
| **1** | The host renders **and presents every frame** regardless of scene revision or input; the reference presents only when an update event is posted | `crates/app/src/lib.rs:387,404`; `crates/platform/platform/src/linux.rs:254-261` | **needs-review** (present timing) | **Critical** power: a static VN screen keeps the GPU/display pipeline at the frame-rate cap forever |
| **2** | Streaming audio read the decode window through the `tvp-audio` mutex **twice per output sample** and woke the worker on every read | `crates/engine/src/sound.rs` (`accumulate_source`, `StreamBuffer::notify_worker`) | **fixed** | **Measured #1 root cause**; now a bulk window read + throttled notify: ~60 locks/s and ~4 notifies/s per source (~1,470× / ~22,000× fewer) |
| **3** | Swapchain is pinned to **`FIFO`** (vsync), so `queue_present` blocks until the display refresh and throttles the loop; the reference uses `D3DPRESENT_INTERVAL_IMMEDIATE` but compensates with update-on-demand | `crates/platform/render/src/vulkan/swapchain.rs:376-384` | **safe-to-adopt** (not script-observable, D34) | High power in combination with #1 |
| **4** | Every dirty bitmap is re-uploaded **in full**; `BitmapState` carries only a `dirty` flag, no region | `crates/app/src/lib.rs:673` | **needs-review** (cross-cutting L2) | Measured 0 uploads in the static steady state; matters only while a bitmap changes |
| **5** | `sync_dirty_bitmaps` and `prune_stale_textures` allocate a `Vec` and scan the whole bitmap/texture set **every frame**, even when nothing is dirty | `crates/app/src/lib.rs:612-690` | **safe-to-adopt** | Measured 0.00 ms on the test title; scales with scene size |
| **6** | Reference **update-region plumbing** (`tTVPComplexRect` → `BeginUpdate` → partial present) is absent | `reference/cpp/core/visual/LayerManager.cpp:1046-1065`; `WindowIntf.cpp:610-624` | **needs-review** (cross-cutting) | Medium–high; the real fix for #1/#4 |
| **7** | `Window.waitVSync` round-trips as a property but does not defer `Show()`; the reference does | `crates/compat/src/natives/visual/window.rs:1125,2520`; `reference/cpp/core/visual/WindowIntf.cpp:626-637,1808-1824` | **needs-review** (D34 semantics) | Medium: script-observable property with no effect |
| **8** | Per-frame GPU **upload wait** and staging-ring bookkeeping run even on frames with no upload; the decode worker allocates a chunk buffer per pull | `crates/platform/render/src/vulkan/mod.rs:823`; `crates/platform/render/src/vulkan/texture.rs:235`; `crates/engine/src/sound.rs:519` | **safe-to-adopt** | Low–medium |
| **9** | `vm.tick` (input dispatch) and the whole `host.step` were unmeasured, hiding the 334 ms | `crates/vm/src/host.rs:452-472`; `crates/app/src/game.rs:277,574` | **fixed by this task** | Enables the next device run to localize the stall |

Reference mechanisms that are **already present** and need no work:
`cache_enabled_count` / `cache_recalc` / `cache_source_generation`
(`crates/engine/src/scene.rs:192-217`, `layer_ops.rs:2401-2485`,
`transitions.rs:183-184,763-769`) and the script-observable
`System.graphicCacheLimit` property (`crates/compat/src/natives/system.rs`,
`reference/cpp/core/base/SystemIntf.cpp:358-370`). See §4.

---

## 1. (a) Presenting every frame — the mobile power sink

**What we do.** `Host::step` unconditionally calls `render()` at the end of
every step (`crates/app/src/lib.rs:387-407`). The only thing the scene
revision gates is *projection* (`crates/app/src/lib.rs:434-441`): a static
scene skips rebuilding `RenderScene`, but it still traverses it, records a
command buffer, submits it and presents it. The platform loop requests a
redraw on every ticker tick regardless of any update flag
(`crates/platform/platform/src/linux.rs:254-261`). On Android this holds the
display/GPU pipeline at the frame-rate cap for a screen that has not changed,
which is exactly the "slower and more power than the reference" symptom.

**What the reference does.** A layer image change calls
`Window->RequestUpdate()` (`reference/cpp/core/visual/impl/DrawDevice.cpp:185-189`),
which posts a window-update event (`reference/cpp/core/visual/WindowIntf.cpp:667-673`).
The dispatcher delivers it to `UpdateContent()`
(`reference/cpp/core/base/EventIntf.cpp:141-148`), which calls
`DrawDevice->Update()`, then `DrawDevice->Show()` only when `!WaitVSync`, then
`EndUpdate()` (`reference/cpp/core/visual/WindowIntf.cpp:626-637`). A static
screen posts no update event, so it neither recomposites nor presents. The
reference's own present call uses `D3DPRESENT_INTERVAL_IMMEDIATE`
(`reference/cpp/core/visual/impl/BasicDrawDevice.cpp:231-245`), i.e. it does
not vsync-block at `Present`; it avoids redundant work by *not presenting*.

**Recommendation.** Adopt the update-event gate: `Host::step` should present
only when the scene revision changed, the input/focus changed, a video/audio
overlay advanced, or a resize happened — the port already computes the scene
revision and has an input snapshot. This is the single largest power win and
it is reference-faithful. It is **needs-review** only because it changes
present timing (out of scope for this task) and must be paired with a
"force redraw" path (first frame, resize, resume, `Invalidate`).

**Lazy rendering — skip the whole frame, not just the present.** The
`render()` call is unconditional (`crates/app/src/lib.rs:404`); the scene
revision only gates the projection (`:434-441`). The Android loop already
carries a `frame_requested` flag, but `ticker.poll(now)` sets it on **every**
tick (`crates/android/src/host.rs:381`), so the step runs at `frame_period`
whether or not anything changed. Gating only the present still leaves the
scene traversal, the command-buffer recording and the submit running for a
screen that has not changed, so the CPU and GPU cost — and the power — is paid
even when the image is identical.

The reference separates the two beats: the VM/timer beat runs continuously
(`tTVPApplication`'s ~16 ms tick), while the *draw* is posted only by a layer
change through `RequestUpdate`
(`reference/cpp/core/visual/impl/DrawDevice.cpp:185-189`) →
`UpdateContent` → `DrawDevice->Update()` / `Show()`
(`reference/cpp/core/visual/WindowIntf.cpp:626-637`). A static screen runs no
draw at all. "Lazy rendering" is that split, and it is the mechanism the
reference uses to stay cool on a phone.

Design: the loop always advances the VM, timers, mixer and video from its
ticker, but skips `sync_dirty_bitmaps` + `reproject_render_scene` + `render()`
when nothing asks for a redraw. A redraw is requested by a `Scene::revision`
change (already computed, `crates/app/src/lib.rs:440`), an input or focus
event, a video/overlay advance, a resize/scale/inset change, resume, the first
frame, and the script-visible `Invalidate`. Because the platform holds the
last presented image, a skipped frame is the previous image, not a blank one —
exactly what the reference shows. The port already has both halves of the
signal (`frame_requested`, `Scene::revision`, and the `dirty` flags on
`BitmapState`); the missing piece is a single host-level `needs_render` value
that those set and `step` clears.

**How to check the efficiency.** Add a rendered/skipped counter to the slow
line (or a one-line summary every N frames) and compare the port's
`submit`/`present` totals for a static KAG screen against the reference. A
title/advance-prompt screen is the canonical case: the reference presents on
the order of once per scripted change, the port presents once per tick. The
same counter is the acceptance test for the change — a static screen should
show skipped frames climbing while rendered frames stay flat.

**Swapchain present mode / vsync / image count.**

* Present mode is pinned to `FIFO` (vsync)
  (`crates/platform/render/src/vulkan/swapchain.rs:376-384`). `FIFO` blocks in
  `queue_present` until the next display refresh, throttling the loop to the
  refresh rate instead of spinning at the frame-rate cap. The reference's
  `IMMEDIATE` is not vsync-blocked, but it compensates with update-on-demand
  (#1); combined with #1 the port's per-frame present would otherwise run
  unthrottled. Present mode is not script-observable, so this is
  **safe-to-adopt** on its own; #1 remains the root-cause fix.
* There is no `waitVSync`-driven behaviour; see §4 and item #7.
* Image count is `min_image_count + 1` capped by `max_image_count`
  (`crates/platform/render/src/vulkan/swapchain.rs:136-140`). One extra image
  is a small memory/latency cost, not a power sink on its own. **safe-to-adopt**
  to drop to `min_image_count` if latency matters, but low priority.

---

## 2. (b) Per-frame allocations and GPU uploads

**`sync_dirty_bitmaps`** (`crates/app/src/lib.rs:612-687`):
* allocates `dirty: Vec<u32>` every frame (`:613`) by scanning **all**
  `scene.bitmaps`;
* allocates `ready: Vec<u32>` (`:621`);
* allocates `uploads: Vec<TextureUpload<'_>>` (`:666`);
* always uploads the whole bitmap: `region: PixelRegion::new(0, 0, width, height)`
  (`:673`) even when only a sub-rectangle changed, because `BitmapState` has a
  `dirty` flag but no region.

**`prune_stale_textures`** (`crates/app/src/lib.rs:689-700`): allocates
`stale: Vec<u32>` and scans every texture key every frame.

**Recommendation (safe-to-adopt).** Track a dirty-id list on the host (the
scene already knows which bitmap was written; the host can maintain the
`dirty` set when it marks a bitmap dirty) and a texture-presence generation, so
a static frame does zero allocations and zero full scans. The full-region
re-upload needs a region on `BitmapState`, which is a cross-cutting L2 change
(every pixel-writing site) and is **needs-review**; the previous perf audit
(`docs/audits/perf-hotspots.md` §3) reached the same conclusion.

**Upload ring.** `UPLOAD_SLOTS = 3`
(`crates/platform/render/src/vulkan/texture.rs:164`); the render path calls
`upload.wait_all(&device)` every frame (`crates/platform/render/src/vulkan/mod.rs:823`)
before submitting, which waits any in-flight upload slot. On a static frame
no slot is in flight, so `wait_all` is three boolean checks
(`texture.rs:235-252`) — negligible. When uploads *are* in flight this is a
synchronous GPU wait inside the frame; the ring exists to absorb exactly one
frame of upload, so this is expected. **safe-to-adopt** to skip the loop when
the frame touched no textures.

**Decode worker allocation.** `decode_worker` allocates
`vec![0.0f32; chunk_frames * channels]` per pull
(`crates/engine/src/sound.rs:519`), i.e. one allocation per ~0.25 s per
streaming source. **safe-to-adopt** to keep a reusable scratch buffer on the
worker, but low impact.

**Audio render buffer.** `AudioOutput::pump` calls
`self.buffer.resize(frames * channels, 0.0)` (`crates/app/src/audio.rs:359`).
For a stable `dt` this is a no-op (same length); it only reallocates when the
chunk size changes. Low impact.

---

## 3. (c) Thread wakeups and locks

There is **no compositor thread**: `Host::step` is single-threaded, and the
Vulkan backend runs on the loop thread. The only worker is the `tvp-audio`
decode-ahead thread (`crates/engine/src/sound.rs:469-538`, spawned at `:565`).

**Hot path — fixed.** `accumulate_source` used to resample one output frame at
a time, calling `source.frame(i0u)` and `source.frame(i0u + 1)` **per output
sample** (`crates/engine/src/sound.rs:1884,1887`), each taking the shared
`Mutex`, calling `work_ready.notify_all()`, and `trim()`ing a `pop_front` per
channel — ~88,200 lock + notify pairs per second per playing streaming source
at 44.1 kHz stereo.

It now loads a contiguous interpolation window with one bulk
`SoundSource::read_into(&mut [f32], start_frame, frames)` under a single lock
and interpolates from local scratch, and `StreamBuffer::notify_worker` wakes
the worker only once the cursor advances by `retain_frames`. Measured effect:
**~60 locks/s and ~4 notifies/s** per source (~1,470× and ~22,000× fewer),
`trim()` once per window; `bulk_window_matches_the_per_frame_path` pins the
mixed samples byte-identical to the per-frame path.

**Worker wakeups.** The worker still waits on `work_ready` while the lookahead
is full and is woken on leading-edge starvation and reseek, so the lookahead is
maintained; it simply sleeps instead of being woken ~88,200×/s.

---

## 4. (d) Missing reference mechanisms, and D34 status

* **Update-on-demand present** (item #1): reference
  `WindowIntf.cpp:626-637,667-673`, `EventIntf.cpp:141-148`,
  `DrawDevice.cpp:185-189`. The `Window.waitVSync` property is
  **script-observable** (`reference/cpp/core/visual/WindowIntf.cpp:1808-1824`,
  `:716-718`) and is already registered in compat
  (`crates/compat/src/natives/visual/window.rs:1125,2520-2522`), but its
  present-timing effect is not implemented. **needs-review.**

* **Dirty-region update** (`tTVPComplexRect`): reference
  `LayerManager::AddUpdateRegion`/`RequestInvalidation`
  (`reference/cpp/core/visual/LayerManager.cpp:1046-1065`),
  `DrawDevice::RequestInvalidation`
  (`reference/cpp/core/visual/impl/DrawDevice.cpp:564-577`),
  `Window::BeginUpdate`/`NotifyUpdateRegionFixed`
  (`reference/cpp/core/visual/WindowIntf.cpp:610-624`). Not script-observable;
  **safe-to-adopt as an optimization**, but cross-cutting (**needs-review** for
  the implementation plan).

* **`Cached` / `cache_enabled_count`**: already implemented in L2. The field
  and its lifecycle are `LayerState::cache_enabled_count` / `cache_recalc` /
  `cache_source_generation` (`crates/engine/src/scene.rs:192-217`), the
  reference counts are `IncCacheEnabledCount`/`DecCacheEnabledCount`
  (`reference/cpp/core/visual/LayerIntf.cpp:3991-4014`, `:366`,
  `:4546-4561`), and transitions drive them
  (`crates/compat/src/natives/visual/layer/transitions.rs:183-184,763-769`).
  The remaining gap is not the cache itself but the per-frame
  present/re-projection gate (#1) and the per-frame full re-upload (#4).

* **`tTVPScreen` invalid-rect plumbing — correction.** `tTVPScreen` exposes
  only screen/desktop metrics (`reference/cpp/core/visual/impl/TVPScreen.h:4-15`,
  `TVPScreen.cpp:7-19`); it has no invalid-rect plumbing. The invalid rects
  live in `tTVPComplexRect`, carried by `LayerManager` and `Window` (above).
  "Adopt `tTVPScreen` invalid-rect plumbing" is therefore not a real
  mechanism; the correct path is the `tTVPComplexRect` one.

* **Graphic (decoded-image) cache** (`TVPGraphicCache`): reference
  `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1433,1441-1455,2217`. Its
  limit is **script-observable** through `System.graphicCacheLimit`
  (`reference/cpp/core/base/SystemIntf.cpp:358-370`), which compat already
  registers (`crates/compat/src/natives/system.rs:22`). Whether the decoded
  cache itself exists and honours the limit should be verified separately;
  **needs-review**, not a present-time item.

---

## 5. (e) The `vm.tick` / `host.step` instrumentation gap

**Before.** The slow line reported the six ordered polls
(`timer/sound/video/async/continuous/project_and_render`) and the host's
`sync/project/render` (`crates/app/src/game.rs`), but:
* `VmHost::tick` (`crates/vm/src/host.rs:452-472`) also runs the event
  adapter's `begin_input_frame`, the per-event `apply_input_event`/`dispatch`
  loop, and `end_input_frame`; none were measured;
* the **whole** `Host::step` was not measured, only its three inner sections,
  so time outside them was invisible;
* `render()`'s submit path (command recording + `queue_submit`) was inside the
  coarse `render_ms`, with only acquire/present/fence/recreate split out.

That is why 334 ms of the 388.7 ms frame was unattributed.

**This task's change (additive only).**
* `crates/compat/src/frame.rs:83-97` — three `FrameBody` hooks
  (`input_begin_completed`, `input_events_completed`, `input_end_completed`),
  defaulted to no-ops.
* `crates/vm/src/host.rs:452-472` — `VmHost::tick` reports each input segment.
* `crates/platform/render/src/lib.rs` — `FrameTimings::submit_ms`.
* `crates/platform/render/src/vulkan/mod.rs:840,857` — measures the
  record+submit path and `present`; all `FrameTimings` literals carry the new
  field.
* `crates/app/src/lib.rs:85-97,389-397` — `StepReport::upload_ms`/`prune_ms`
  split out of `sync_ms`.
* `crates/app/src/game.rs:100,277,574` — `TickPhases`, the whole
  `vm.tick` and `host.step` measurements, and the new slow-line fields
  (`its vm: tick=… input_begin=… input_events=… input_end=… tick_residual=…`;
  `its host: host_total=… … sync_upload=… sync_prune=… submit=…
  host_residual=…`; the whole-step `total=…` is retained next to `host_total=…`,
  so `unaccounted = gap - total - platform_wait` stays explainable). Existing
  fields are unchanged.
* `crates/app/tests/frame_timing.rs` — the tick test now proves the input
  segments report before the ordered polls; `crates/app/src/game.rs` unit
  tests cover the new slow-line fields.

**The VM exposes no GC hook**, so GC time lands in whichever poll triggered
it (already inside that poll's hook) or in `tick_residual`. `tick_residual`
being large on the next device run means the cost is in the compat context
lookup or VM code between the polls, not in a poll; a large `input_events`
means event dispatch to script is the cost. The `host_residual` splits the
host step the same way.

---

## 6. Suggested order of work (proposals only)

1. Add the lazy-rendering / update-present gate (#1) — advance the VM every
   tick but render only on a redraw request, paired with a forced-redraw path.
   The reference mechanism, biggest power win. **needs-review.**
2. Batch the streaming-audio window read and cut the per-read notify (#2).
   **safe-to-adopt** with a PCM-equality test.
3. Carry a dirty region on `BitmapState` and upload only it (#4).
   **needs-review.**
4. Keep a dirty-id set and texture generation to remove the per-frame scans
   and allocations (#5). **safe-to-adopt.**
5. Implement the `tTVPComplexRect` update region and partial present (#6).
   **needs-review.**
6. Re-run the device with the new slow line (#9) to confirm where the 334 ms
   actually is before committing to the audio work.
