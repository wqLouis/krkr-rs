# krkr-rs — Rendering / Loading / Decoding Optimization Audit

Status: **read-only audit, no code changed.** Scope: DEV (`cargo build`, dev profile,
unoptimized) on this machine (12 logical cores, RTX 5070, 32 GB) against the in-house
test game `/mnt/DATA/Games/Others/test` (633 MB `data.xp3` with 23,572 entries +
`patch.xp3` with 174). All numbers below are **measured** unless explicitly marked
*est.*

## How the measurements were taken

* `cargo run -p render --bin krkr-rs -- run /mnt/DATA/Games/Others/test --headless`
  (the real pipeline: mount → register natives → `startup.tjs` → one Update).
* An **out-of-tree** benchmark crate `/tmp/syncbench` that path-depends on the
  workspace crates (`render`, `tvp-visual`, `engine`, `xp3`, `tvp-sound`, `tvp-text`)
  and reuses `CARGO_TARGET_DIR=<repo>/target`. It rebuilds a title-like `Scene`
  (1 window, 23 layers, 34 bitmaps — matching the real headless dump) and times
  `sync_scene` in an isolated Bevy `App`, times `Scene::window_layer_order`, and
  decodes the real startup bitmaps/audio read from the real `data.xp3`.
  No repository file was modified.
* Real title scene from the headless dump: **1 window, 23 layers, 34 bitmaps,
  2 archives (23,572 + 174 entries)**. Every layer is in `window_layer_order`;
  blend `type=2` (`ltAlpha`) for all of them in this snapshot.

> Dev-profile caveat: the dev build is unoptimized; heavy byte loops (TLG, zlib,
> symphonia) are several× slower than release. The **architecture/ordering** of the
> findings holds in release, but absolute decode latencies will shrink. Prioritize by
> the structural costs (entity churn, O(n²) traversal, synchronous decode), not by the
> exact debug microsecond counts.

---

## (a) Prioritized optimization table

Ordered by expected impact ÷ effort. Costs are per frame unless stated.

| # | Area | Evidence (file:line + measured/estimated cost) | Proposed optimization | Expected gain | Effort | Risk |
|---|------|-----------------------------------------------|-----------------------|---------------|--------|------|
| 1 | **Renderer full rebuild** | `sync.rs:241` despawns every `WindowRoot` (auto-despawns children) then `sync.rs:300-350` respawns one entity per layer every frame; `sync.rs:249` drains/removes every `LayerBlendMaterial` each frame. Measured isolated `sync_scene`: **309 µs @1 layer, 925 µs @23, 3.55 ms @120, 13.9 ms @400** (≈28–34 µs/layer marginal). Raw spawn+despawn micro-bench ≈**5.5–6.4 µs/entity/frame**. Per frame the title scene churns ~24 spawns + ~24 despawns + the material set. | Key entities by `layer_id` (`HashMap<u32, Entity>`); update `Transform`/`Sprite`/`Visibility` in place; spawn only new layers, despawn only removed ones. Cache one `LayerBlendMaterial` per layer and mutate its uniform/texture, never add/remove per frame. Re-use the camera (already done) and only re-insert its `Projection` when `inner_size` changes. Add a `Scene::revision` counter so an idle frame (no mutation) skips sync entirely. | Title ≈2× (0.9→~0.45 ms); 120-layer scenes ≈5×; 400-layer ≈10×. Idle VN frames near-free. | M | Low–Med (visual regression tests already exist) |
| 2 | **Scene traversal & lookups are O(n²)** | `Scene::window_layer_order` (`scene.rs:368`) calls `append_layer_subtree` (`scene.rs:430`) which scans **all** layers for "missing children" per layer, and `sort_siblings` (`scene.rs:409`) uses `Vec::position` inside its comparator. `layer()`/`layer_mut()`/`bitmap()`/`bitmap_mut()` are linear `iter().find()` (`scene.rs:254,258,330,334`). Measured `window_layer_order`: **34.6 µs @23, 465 µs @120, 4.10 ms @400, 24.5 ms @1000**. | Add id→index maps (`Vec<u32>`/`HashMap`) so lookups are O(1); rewrite `window_layer_order` to a single sorted traversal with a `parent→children` index (no per-layer full scan, no `position` in the comparator). Optionally memoize the flattened order + composed rects keyed by `Scene::revision`. | Removes ~4 ms/frame @400, ~24 ms @1000; makes #1 cheap for large scenes; also speeds input hit-testing (`input_bridge.rs:670`). | S–M | Low |
| 3 | **GPU texture uploads** | Upload is already dirty-gated: `BitmapAssets::handle_for` (`sync.rs:103-129`) returns the cached handle when `!bitmap.dirty`; `sync_scene` clears dirty after upload (`sync.rs:361`). But `upload_bitmap` (`sync.rs:132-141`) **clones the whole RGBA buffer** (`bitmap.rgba.clone()`) and allocates a fresh `Image`. Measured: 34 bitmaps dirty **every frame** costs **4.77 ms/frame** vs **0.90 ms** steady (≈3.9 ms for the re-upload). `drawText` and every raster op call `mark_dirty` (`layer.rs:456,1609,1674,1818,2390`). | Move the `Vec<u8>` into the `Image` (drain/replace the scene buffer, or `std::mem::take` + refill) instead of cloning; reuse the existing `Handle<Image>` via `Assets::insert` on re-upload. Track a dirty sub-rect for text/fill so only the changed region is re-uploaded (GPU `write_texture` of a sub-rect), or at least skip upload when pixels are unchanged. | Up to ~4 ms on all-dirty frames; large for dialogue text and `@update`/`@blackout` transitions that repaint full-screen bitmaps. | M | Med (needs dirty-rect plumbing) |
| 4 | **Image decode is synchronous on the VM thread** | `Layer.loadImages` (`layer.rs:494`) → `load_bitmap_from_storage` (`bitmap.rs:87`) → `storage.read` (`bitmap.rs:114`) → `decode_bytes` (`bitmap.rs:134`) all run inside the TJS native call on the VM thread. Measured **startup: 34 bitmaps read 35 ms + decode 645 ms**, single-threaded. Slowest: `frame/frm_0501b.tlg` (1280×403) **202.8 ms**, `frame/frm_0503.png` (1280×720) **83.1 ms**, `frm_0502a.tlg` 55.3 ms. A 64-image sample: read 806 ms (22.3 MB) + **decode 14.4 s**. TLG throughput ≈**3.5 Mpx/s** (fixture `frm_0303a` 280×200 = 21.5 ms). | Worker pool (Bevy `AsyncComputeTaskPool`, already a dependency, or `rayon`) + a decoded-asset cache keyed by resolved storage name. Decode images/PNG/TLG off-thread and hand results back to the VM thread to `scene.add_bitmap` + `mark_dirty`. To get real overlap at startup, add a **speculative prefetcher** that scans `startup.tjs`/`.ks`/`.tjs` source for image-name string literals and enqueues background decodes into a bounded LRU; synchronous `Bitmap`/`loadImages` first checks the cache and only blocks on a miss (see §b). Also consider intra-TLG parallelism where the algorithm allows (see §b). | Startup decode 645 ms → ~100–150 ms with 4–8 workers; large transitions similarly overlapped; hides the 83–203 ms single-file stalls. | L | Med–High (cache lifetime, prefetch waste) |
| 5 | **XP3 I/O + shared cursor** | `Xp3Archive::read_entry` (`archive.rs:131-149`) does `self.file.seek(...)` + `read_exact` on a single shared `File`, so reads cannot be concurrent. `Storage::mount` opens archives sequentially (`storage.rs:110-131`). Measured: `data.xp3` open+index **122.8–127.8 ms** (23,572 entries); `patch.xp3` 1.3 ms; 132 scripts / 8.9 MB read+zlib **125–142 ms** (63–71 MB/s). Note: this archive stores **all segments raw** (`raw=23572 zlib=0`), so zlib decompression is not a factor for this game (other games do use it). | Replace `seek`+`read` with positional reads (`std::os::unix::fs::FileExt::read_at`) on a shared `File`, making entry reads lock-free/parallel; open archives in parallel (`rayon`/`bevy_tasks`). Add an optional parallel prefetch of the next script/asset bytes. | Index parse ~128 ms can be overlapped/parallelized; reads become parallelizable (prerequisite for #4); script-read 125 ms overlap with VM execution. | S–M | Low |
| 6 | **Audio decode is synchronous and fully in memory** | `WaveSoundBuffer.open` (`wavesound.rs:283,305`) and `SoundBuffer` (`natives.rs:161,315`) call `decode_audio` (`decode.rs:110`) on the VM thread, which reads the file and decodes **all** PCM into `DecodedAudio.samples: Vec<f32>`. Measured: voice 17 KB → 14.6 ms; **BGM `bgm/bgm27.ogg` (5.37 MB) → 8.46–8.58 s** for 326.6 s stereo @48 kHz = **31.35 M samples ≈ 125 MB of f32**. | Move `open()` decode to the worker pool; keep the stream in a `"unload"`/loading state and install the `Arc<DecodedAudio>` + flip status on the VM thread when done (the existing `sound_poll`/`status` machinery already derives state). For long loops, prefer a **streaming/ring-buffer** source so the mixer decodes ahead instead of materializing 125 MB. | Eliminates a single **8.5 s** stall per long BGM and ~125 MB RAM; small voices still decode on-demand. | M–L | Med–High (status/handler semantics, streaming mixer rework) |
| 7 | **Font discovery blocks the VM** | `FontFace::discover_system_jp` (`font.rs:171`) rebuilds a `fontdb::Database` and calls `load_system_fonts()` on the VM thread. Measured first call **271.7 ms** (second call 196 ms — the rebuilt DB is not cached; `resolve_face`'s `FACE_CACHE` (`font.rs:307,332`) makes it once-per-process, but still a one-time stall on first text). | Warm `resolve_face(SystemJp)` (or the fontdb) on a worker at startup, before the first `drawText`; cache the `fontdb::Database` inside `FontFace`/a static. | Removes a one-time ~200–270 ms stall at the first text layer. | S | Low |
| 8 | **VM-thread blocking natives beyond decode** | `run_vm` (`main.rs:410`) is serialized by `VM_RUN_LOCK` (`main.rs:408`) and synchronously runs `async_trigger_poll` / `timer_poll` (→ `paint_poll`) / `continuous_handler_poll` / `sound_poll`. Blocking work reachable from scripts includes `Storages.getFileList` (disk walk + scan of all archive entries, measured ~3–14 ms over 23,572 entries), `Scripts.execStorage` reads (`tvp-scripts/src/lib.rs:266`), `Storages.stat`, and text/glyph rasterization in `Layer.drawText`. | Move pure, VM-independent work (storage enumeration, script-byte prefetch, font/atlas warm-up) to the worker pool; keep Scene mutation and all script execution on the VM thread. `getFileList` can build its result on a worker and return it via a completion to the VM. Raster into an off-thread bitmap where possible. | Removes multi-ms enumeration/read stalls; keeps the VM responsive. | M | Med |
| 9 | **Per-frame allocations in hot systems** | `input_bridge::collect_frame_events` builds a fresh `HashSet<u32>` every frame (`input_bridge.rs:782`); `hit_test_excluding` calls `window_layer_order` (a fresh `Vec`) per mouse event (`input_bridge.rs:670`); `plan_layer_calls` allocates `Vec`/arg `Vec`s (`input_bridge.rs:455,484,491`); `timer_poll` allocates a `due: Vec` (`timer.rs:503`) and locks `TIMERS` several times. `compose_layer` allocates a `Vec` + `HashSet` per layer per sync (`sync.rs:563,566`). | Reuse scratch buffers across frames (store in a `Resource`); iterate keys without collecting; cache the flattened order; replace the per-layer `HashSet`/`Vec` in `compose_layer` with a fixed-depth loop or reuse a scratch vec. | Tens of µs/frame + fewer allocator/GC-style pauses; compounds with #1/#2. | S | Low |
| 10 | **Frame pacing / Bevy** | No explicit `present_mode`/VSync config anywhere (`grep` finds none), so Bevy's default `PresentMode::AutoVsync` (Fifo) caps to the monitor refresh. No custom `ScheduleRunner`; `run_vm` → `sync_scene` are `.chain()`ed in `Update` (`main.rs:177,234`); `capture_input`/`dispatch_input` run after `run_vm` (`main.rs:186-190`). | No change required for normal play (vsync is appropriate for a VN). If high-refresh/uncapped is wanted later, expose `Window::present_mode`; the sync cost from #1/#2 becomes the frame limiter only when uncapped. | — | S | Low |

### Notes / non-issues found

* **Texture upload is correctly dirty-gated** (per the question in the brief): a bitmap
  is uploaded once on first sight and again only when `BitmapState::dirty`; the flag is
  cleared by `sync_scene` after upload (`sync.rs:361`, test
  `bitmap_upload_respects_dirty_flag`). The problem is not "every frame" but the
  **full-buffer clone** on each dirty upload.
* **No zlib decompression for the test game**: every XP3 segment is raw
  (`SEGM_ENCODE_RAW`), so the `ZlibDecoder` path (`archive.rs:144`) is cold here.
  Other games may use it; the parallel-read design covers both.
* **No rayon/tokio/futures in the workspace** (`Cargo.lock` has neither `rayon` nor
  `crossbeam`; only `bevy_tasks`). Prefer `bevy_tasks::AsyncComputeTaskPool` to avoid a
  new dependency, or add `rayon` explicitly for the decode pool.
* The renderer already keeps the **camera** across frames (`sync.rs:262-283`) and
  reuses the shared unit quad / white 1×1 texture (`GpuPrimitives`, `sync.rs:154`); the
  incremental design should extend the same idea to layer entities.
* The windowed app can run on this machine (NVIDIA + `DISPLAY=:0`/Wayland); no
  `WinitSettings`/`ScheduleRunner` tuning is present, so pacing is Bevy's default.

---

## (b) Parallel loading & decoding

### What is independent and safe to run concurrently

1. **Archive index parsing** — each `.xp3` archive and each index chunk/`File` chunk is
   independent; parsing `data.xp3` (23,572 entries) is ~128 ms of pure CPU/IO and can be
   sharded across workers. `Xp3Archive::entries` is a `BTreeMap` insert; insertion order
   across shards is not observable (lookups are by key), so parallel parse + merge is safe.
2. **Reading entry bytes** — each entry (and each segment) is an independent
   `(offset, length)`. This is currently *not* parallel-safe because `read_entry` uses a
   shared cursor (`archive.rs:137`). Making it safe is a small, well-contained change:
   use `FileExt::read_at(&self.file, buf, self.base + seg.start)` (Linux `pread`) so a
   `&File`/`Arc<File>` can be shared by any number of workers with no seek race. After
   that, entry reads parallelize trivially.
3. **Image decoding** — once bytes are in memory, `decode_tlg` / `image::load_from_memory`
   are pure CPU with no shared state and no VM access. Different bitmaps are fully
   independent. This is the highest-value parallel workstream.
4. **Audio decoding** — `decode_audio_bytes` is likewise pure CPU; different files are
   independent. The only serialization is installing the result into a mixer channel,
   which must happen on the VM thread.
5. **Font discovery/parsing** — `fontdb` loading and `FontFace::from_memory` are pure and
   VM-independent; can be warmed on a worker.
6. **Script source bytes** — reading/prefetching `.tjs`/`.ks` bytes is independent of the
   VM. **Executing** them is not.
7. **Storage enumeration** (`getFileList` disk walk + archive scan) — read-only; can run
   on a worker over a cloned/`Arc`ed entry snapshot.

### What is inherently serial

* **The TJS2 VM.** All script execution — `run_vm`'s timer/async/continuous/`paint_poll`
  callbacks, input dispatch handlers, and every native call — is serialized by
  `VM_RUN_LOCK` (`main.rs:408`). The VM is single-threaded by design; nothing should run
  scripts off-thread.
* **`Scene` mutation.** `Scene` is one `RwLock<Scene>` with `Vec` tables and no
  fine-grained locking. Natives mutate it under the write lock. Worker results must be
  applied on the VM/main thread; workers must never hold the scene lock.
* **Bevy ECS world + `Assets<Image>`.** Entity/asset creation and GPU upload happen on
  the main thread (Bevy owns the `World`/`RenderWorld` sync). Only *decoded CPU pixels*
  cross the boundary.

### Concrete integration design

**Worker pool.** Use `bevy_tasks::AsyncComputeTaskPool` (already in the dependency tree
via Bevy) or a dedicated `rayon::ThreadPool` sized `num_cpus - 1`. Jobs are pure CPU:
`Job { resolved_name, kind: Image | Audio | Font, bytes, request_hint }` → result
`Decoded { name, kind, width, height, rgba/pcm }`.

**Read layer.** Give each worker a `(archive_path, Vec<(offset, len, method)>)` and a
**fresh `File`** (or a shared `Arc<File>` with `read_at`). Never share the current
cursor. Raw segments are a copy; zlib segments use `ZlibDecoder` per segment. This
isolates the worker from `engine::Storage`'s `&mut self` read path.

**Request path (synchronous natives, semantics preserved).**
`load_bitmap_from_storage` keeps its current shape but consults, in order:
1. `BitmapCache.by_name` (already exists) → return id.
2. A new `DECODED_CACHE: HashMap<resolved_name, DecodedBitmap>` populated by workers →
   insert into `Scene` (`add_bitmap` + `dirty=true` + `name`), update `BitmapCache`,
   return id. This is the **fast, cache-hit path** and is what makes prefetch pay off.
3. On a miss: submit a job to the pool and **block the VM thread on a oneshot** until the
   result arrives (preserves `Bitmap(name).width` synchronous semantics). Submit-time
   dedup: if a job for the same name is already in flight, wait on the existing one.

**Prefetch (to actually overlap startup).** The synchronous path alone gains nothing
while the VM is blocked. Add a **speculative prefetcher**:
* When a script is read/executed (`startup.tjs`, `Scripts.execStorage`, KAGParser
  `.ks`), scan the source bytes for string literals that look like asset names
  (`.tlg/.png/.jpg/.webp/.bmp` for images; `.ogg/.opus/.wav/.mp3` for audio).
* Enqueue background decode jobs for those names into a **bounded LRU**
  (e.g. ≤64 MB / ≤N entries) using the same pool.
* A subsequent synchronous request for a prefetched name is a cache hit; a
  non-prefetched name still works via the blocking path.
* Cancel/drop prefetch jobs on scene change / `Storage` unmount to cap memory. Waste
  from wrong guesses is bounded by the LRU and is offline of the VM thread.
* Alternative if prefetch heuristics are undesirable: parse `.ks` `@bg`/`@image`/`@playse`
  tags (the KAG parser already tokenizes them) for a stronger hint set.

**Audio.** `ws_open` submits an audio job and returns immediately with the stream in a
new `Loading` state (script-visible `status` stays `"unload"`); `sound_poll` (or the
result drain at the top of `run_vm`) installs `Arc<DecodedAudio>` into the mixer channel
and lets the existing status-transition handler fire `onStatusChanged("stop"/"play")`.
`play()` before ready records the intent and starts on install. For long BGM prefer a
**streaming** `Source` that decodes chunks on the worker into a bounded ring buffer,
which also removes the 125 MB per-BGM allocation.

**Result drain / `mark_dirty`.** Drain the completion channel once per frame at a safe
point **before** `sync_scene` and **on the VM thread** (e.g. at the top of `run_vm`, or a
new `Update` system chained before `sync_scene`). Under the scene write lock:
`scene.add_bitmap(w, h, rgba)` (already sets `dirty=true`) and update `BitmapCache`; for
audio, store into a `READY` map and let `sound_poll` install it. `sync_scene`'s existing
dirty-gated upload (`sync.rs:103-129`) then picks the new texture up automatically — the
incremental upload path and the parallel decode path compose cleanly.

**Ordering / lifetime concerns.**
* Never apply worker results while `sync_scene` holds the read lock; the drain runs
  before it in the schedule chain.
* Decoded pixel buffers are **owned `Vec<u8>`s moved into `Scene`** (or into a cache that
  the caller removes from) — no borrows cross threads.
* `Storage`/`Tjs2Engine` are *not* `Send`-friendly in the current shape (`Xp3Archive`
  owns a `File` and needs `&mut self`); the read layer must be refactored to share an
  `Arc<File>` + `read_at`, which is the one prerequisite that touches `xp3`/`engine`.
* Prefetch cache entries must be evictable and must not keep an archive `File` alive
  after unmount.

---

## (c) Recommended first three changes

### 1. Make `sync_scene` incremental, and make `Scene` lookups O(1)

Do these together because #1's benefit depends on #2's cheap traversal and vice versa.

* Add `HashMap<u32 /*layer_id*/, Entity>` (and window roots) to a resource; update
  `Transform`/`Sprite`/`Visibility` in place; spawn/despawn only on appearance/removal.
* Replace per-frame `FrameBlendMaterials` add/remove with one cached material per layer
  whose uniform/texture is mutated in place.
* Only re-insert the camera `Projection` when `inner_size` changes.
* Add id→index maps to `Scene` (`scene.rs:254-334`) and rewrite `window_layer_order`
  (`scene.rs:368-460`) without the per-layer full scan / comparator `position`.
* Add a `Scene::revision` counter bumped by mutating natives; when unchanged, skip the
  sync entirely (idle VN frames become nearly free).

**Why first:** contained, well covered by existing tests (`sync.rs` test module,
`affine_layer_paint`, demo), low risk, and immediately removes 0.5 ms/frame at the title
scene and multi-ms for large ADV scenes. It also speeds the input bridge's hit-testing.

### 2. Add a parallel decode worker pool with a prefetch cache for images

* Introduce the read layer (`Arc<File>` + `read_at`, or per-job fresh `File`) so entry
  bytes can be read concurrently.
* Add `AsyncComputeTaskPool`/`rayon` jobs for `decode_tlg` / `image::load_from_memory`,
  with a bounded LRU `DECODED_CACHE`.
* Add the speculative literal prefetcher over script/`.ks` source.
* Drain completions on the VM thread before `sync_scene`; insert with `dirty=true` so the
  existing upload path handles the GPU side.

**Why second:** it is the largest *latency* win (measured 645 ms startup decode, up to
14.4 s for a 64-image batch) and it preserves synchronous script semantics because the
cache-hit path is synchronous. Effort is high, so ship the cache + pool before the
prefetcher (the pool alone already lets multiple concurrently-requested loads overlap).

### 3. Move audio `open()` decoding off the VM thread (and stream long BGM)

* `ws_open`/`SoundBuffer` submit the decode to the same pool; return with the stream in a
  loading state; install the `Arc<DecodedAudio>` and flip status on the VM thread in
  `sound_poll`.
* For long tracks, replace the fully-decoded `Vec<f32>` with a streaming/ring-buffer
  source so the 5.37 MB BGM no longer costs 8.5 s of VM thread and ~125 MB RAM.

**Why third:** the single worst stall measured (8.5 s for one BGM), but it is
semantics-sensitive (status/`onStatusChanged`/`onFadeCompleted` sequencing), so it
should follow the renderer and image work. If a smaller third step is preferred, do the
**XP3 `read_at` + parallel archive open** change first — it is low-risk, removes the
cursor serialization, and is a prerequisite for #2 anyway.

### Real-game headless startup budget (measured, dev, 2121 ms total)

| Phase | Cost | Notes |
|---|---:|---|
| Process start + mount (2 archives) | ~150 ms | `data.xp3` index parse ≈128 ms, `patch.xp3` ≈1.3 ms |
| `startup.tjs` script reads (132 files, 8.9 MB, raw) | ~125 ms | `Scripts.execStorage` / loader |
| Image decode (34 boot bitmaps) | **645 ms** | dominated by `frm_0501b.tlg` 203 ms + `frm_0503.png` 83 ms |
| Font discovery (`discover_system_jp`) | **272 ms** | once per process, first text layer |
| TJS VM parse/execute + native overhead | ~930 ms | inherently serial |

The three recommended changes target the 645 ms decode + the 8.5 s BGM stall + the
per-frame churn; the font 272 ms is a cheap S-effort follow-up, and the VM time is
inherent.
