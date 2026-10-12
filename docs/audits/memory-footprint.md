# Runtime memory footprint audit

Audit of the CPU memory footprint of the release/debug `krkr` host on the real
game `/mnt/DATA/Games/Others/test` (`data.xp3` 23 572 entries + `patch.xp3`
174 entries). This is an audit only: **no source was modified.**

The reference engine (`reference/cpp/**`) is the oracle. Decision numbers in
this document refer to `AGENTS.md`; the implementation comments do not.

---

## 0. Binaries, method and caveats (read this first)

| binary | built (local) | contains the sound-streaming change? | notes |
|---|---|---|---|
| `target/debug/krkr` | 2026-10-11 22:54:06 | **yes** | closest to the current tree; `strings` finds `streaming source` |
| `target/release/krkr` | 2026-10-11 21:03:38 | **no** | built before `crates/app/src/audio.rs` (21:55) and `crates/engine/src/sound.rs` (22:06) were changed; `strings` finds **0** occurrences of `streaming source` |

The repo HEAD moved while this audit ran (another agent committed
`983cac8 feat(sound): decode on demand and bound the visual buffer`,
`dcaa17d fix(vm): release the native state while the engine is alive`,
`bfa9c82 test(compat): …`). The debug binary predates the last two commits and
the `dcaa17d` drop-order change (`crates/vm/src/host.rs`, 22:57). That change
is a teardown fix, not a steady-state allocation, so it does not affect the
numbers below. **Every "current tree" number below is the 22:54 debug binary.**
No release build of the current tree exists, so a like-for-like
release-vs-release comparison of the audio fix is not possible here.

Method (no `cargo`/`rustc` was run, per the hard repo rule):

* `--headless --frames N --base-dir /mnt/DATA/Games/Others/test`, RSS sampled
  from `/proc/<pid>/status` (`VmRSS`/`VmHWM`) at ~20 Hz, plus
  `/proc/<pid>/smaps`;
* a small `LD_PRELOAD` malloc interposer (`gcc`, not cargo) that snapshots
  every live allocation ≥ 16 KB every 0.5 s, so a *steady-state* profile can
  be read while the game is running (ptrace is restricted, `yama=1`, so
  `/proc/pid/mem` scanning is unavailable);
* the game's XP3 index and the `.tft`/`.ogg` payloads were parsed offline with
  Python replicating the XP3 index format.

**Headless uses the `null` render backend.** No Vulkan device, swapchain or
GPU texture is created, so **none of the numbers below include GPU memory.**
The Vulkan path was audited by reading, not measured.

---

## 1. Measured RSS

| run | binary | frames | plateau RSS | peak RSS (VmHWM) |
|---|---|---|---|---|
| empty scene, no game | debug | 120 | ~10 MB | ~10 MB |
| game, title screen | debug | 5 | ~231 MB | 255 MB |
| game, title screen | debug | 600 | ~222–230 MB | 237 MB |
| game, title screen | debug | 3 000 | ~230 MB | 237 MB |
| game, title screen | release (old audio) | 5 | ~255 MB | **264 MB** |
| game, title screen | release (old audio) | 3 000 | ~255 MB | 263 MB |

Observations:

* The title screen is fully loaded within a few frames; memory is flat from
  ~frame 5 to frame 3 000. There is no per-frame growth at the title.
* Empty-scene runs finish in 28 ms and allocate nothing of interest.
* `smaps` on the debug run (RSS 237 MB): **Anonymous 190 MB + file-backed
  ~47 MB** (the 296 MB *debug* binary and shared libraries; the release
  binary's file-backed share is much smaller). The single largest regions are
  a 126.5 MB `[heap]` and a 52.9 MB anonymous mmap; with
  `MALLOC_MMAP_THRESHOLD_=128K` the multi-MB buffers coalesce into a 129 MB
  region and the heap drops to 39 MB.
* The **old-audio release** and the **current debug** title profiles are
  essentially the same (both ≈255–264 MB peak). The headless title screen
  never decodes audio in either build — the malloc profile shows **no ~50 MB
  PCM allocation** in either. So the audio fix's win is invisible here and is
  a gameplay-time effect (see §3).

The reported "almost 700 MB" is therefore **not** a title-screen number. It is
a gameplay number that headless cannot reach (no input → the KAG script never
advances past the title).

---

## 2. Where the ~231 MB goes (allocation accounting)

The malloc interposer at the title plateau reports **142.3 MB live in 143
allocations ≥ 16 KB** (50 of them ≥ 256 KB). The top of that profile maps
one-to-one onto real assets:

| bytes | asset | note |
|---:|---|---|
| 21 394 008 | スーラ39.tft | 20.40 MB uncompressed |
| 20 873 888 | ハミング39.tft | 19.91 MB |
| 15 708 932 | スーラ30.tft | 14.98 MB |
| 14 924 620 | ハミング30.tft | 14.23 MB |
| 9 774 964 | スーラ21.tft | 9.32 MB |
| 9 241 744 | ハミング21.tft | 8.81 MB |
| 4 597 112 | スーラ12.tft | 4.38 MB |
| 4 540 876 | ハミング12.tft | 4.33 MB |
| 3 686 400 ×4 | 1280×720×4 RGBA bitmaps | **exactly** the game surface size |
| (16 more `.tft` 0.3–1.8 MB) | 筑紫明朝 / ニューシネマ / ロダン | |

Accounting for the 237 MB debug RSS:

| item | size | source |
|---|---:|---|
| 24 pre-rendered `.tft` fonts | **≈110.7 MB** | `PrerenderedFont.image: Vec<u8>` (whole file) held in `ResourceResolver.prerendered_storage` |
| scene bitmaps (51) | ≈30 MB | `BitmapState.rgba = width*height*4`; 4 full-screen 1280×720 (14.7 MB) + the rest |
| XP3 parsed index | ≈4.5 MB | `BTreeMap<String, Entry>`; `data.xp3` 23 572 entries, `patch.xp3` 174 |
| other allocations < 16 KB (TJS VM objects, object maps, strings, coverage cache) | ≈45 MB | residual anon − tracked |
| file-backed (debug binary + shared libs) | ≈47 MB | `smaps` |

### Why the `.tft` fonts dominate

`system/system.tjs` `PrerenderedFontInit()` iterates `PRFont.tjs` and calls
`mapPrerenderedFont(...)` for **all 24 entries** (6 faces × 4 heights). The
patch archive ships large bitmap fonts; measuring each `.tft`'s *uncompressed*
size (zlib segments use `org_size`) gives:

```
20.40 + 19.91 + 14.98 + 14.23 + 9.32 + 8.81 + 4.38 + 4.33   (patch, 8 fonts)  =  96.4 MB
+ 16 data fonts (0.31 … 1.71 MB)                                             =  14.3 MB
                                                                               = 110.7 MB total
```

`PrerenderedFont::parse` moved the whole file into `image` and kept the index
vectors on top, which was the single largest CPU allocation in the process.

**Startup change (this tree).** `mapPrerenderedFont` now resolves the storage,
validates only the fixed header and records the index offsets; the whole file
is read once on the first glyph request (`crates/compat/src/resource.rs`, via
`Storage::stat`/`Storage::read_prefix` and `Storage::read`). The storage map is
keyed by the resolved placed path, mirroring `TVPPrerenderedFonts`. The
reference reads and holds the whole file at map time
(`reference/cpp/core/visual/PrerenderedFont.cpp:9-76`) but validates only the
header there too, so deferring the body is not observable through the script
surface; the one divergence is that our index-bounds check (which the reference
performs as unchecked reads) moves with the body read. A mapping that is never
drawn reads one 36-byte header prefix and no glyph payload. Verified on the
host by the read-counter and reference-header-error tests in
`crates/compat/tests/font.rs`; the startup wall-clock A/B is still to be
measured.

### Formulas

* scene bitmap: `width * height * 4` bytes (`crates/engine/src/scene.rs:230`).
  Full screen at the game's resolution: `1280*720*4 = 3 686 400`.
* `.tft`: `file_len + 2*index_count + 16*index_count` bytes.
* old decoded audio: `total_frames * channels * 4` bytes.
* streaming audio window: `rate * channels * 4` for 1 s + `rate/4` decode
  scratch (`crates/engine/src/sound.rs:336-360`).
* XP3 index: `Σ (Entry + raw_name + name + segments)` ≈ 4.5 MB, plus the
  3.14 MB decompressed index block, which is freed after parsing.

---

## 3. The audio change: how much of the ~700 MB does it account for?

**Short answer: all of it is plausible, and it is entirely a gameplay-time
effect. On the headless title screen the change accounts for 0 MB, because no
audio is opened there.**

The numbers:

* The game has 30 BGM tracks. Decoding every one to interleaved `f32` stereo
  48 kHz totals **1 513 MB**. Individual tracks (Ogg granule / channel / rate
  parsed from the archive):

  | frames | channels | rate | old PCM | Ogg size | file |
  |---:|---:|---:|---:|---:|---|
  | 15 677 420 | 2 | 48000 | **119.6 MB** | 5.12 MB | bgm/bgm27.ogg |
  | 9 735 000 | 2 | 48000 | 74.3 MB | 3.10 MB | bgm/bgm26.ogg |
  | 8 894 592 | 2 | 48000 | 67.9 MB | 2.37 MB | bgm/bgm13.ogg |
  | 8 811 940 | 2 | 48000 | 67.2 MB | 2.42 MB | bgm/bgm10.ogg |
  | 7 031 808 | 2 | 48000 | 53.6 MB | 2.06 MB | bgm/bgm01.ogg (the task's example) |
  | … | | | median **52.0 MB** | | |

* **Old code** (the release binary and everything before commit `983cac8`):
  `add_audio_decoder`'s closure called `decode_audio(&media, bytes, hint)`,
  materialized the *entire* file to interleaved `f32`, and wrapped it in
  `SoundSource` as `Arc<DecodedAudio>`; the buffer lived for the sound's whole
  lifetime. This is exactly what the working tree replaced
  (`git diff crates/app/src/audio.rs`, `crates/engine/src/sound.rs`).
* **New code** opens the container without decoding (`open_audio`), and
  `SoundSource::streaming` keeps a `StreamWindow` of at most
  `rate` frames (~1 s → 384 KB at 48 kHz stereo) plus the `chunk_frames`
  decode scratch, and re-decodes on a backward seek
  (`crates/engine/src/sound.rs:326-360`).

Per simultaneously-live BGM the fix removes about **52 MB (median)** /
**119.6 MB (bgm27)** and replaces it with a window of ~0.4 MB plus the
retained compressed bytes.

Reconciling with the report: ~700 MB is consistent with the old code holding
roughly 6 median BGMs (≈312 MB) plus ~7 MB of decoded voice buffers, or
bgm27 + a handful of others + voices. The exact mix needs a gameplay run.
**This audit cannot reproduce gameplay headless, so the split between "the
audio fix" and "other leaks" in the 700 MB is an estimate, not a measurement.**
The estimate is that the audio fix removes the majority of it, because a
single BGM alone is 52–120 MB and the old code held every opened one.

Caveat / regression inside the fix: the new decoder retains the *compressed*
file for the sound's lifetime for reseeks —
`AudioDecoderState.bytes: Arc<[u8]>`
(`crates/platform/media/src/audio.rs:61-71`), populated from the
`&[u8]` the L3 decoder receives (`AudioDecoder` is
`Fn(&[u8], …)`, `crates/compat/src/natives/sound.rs:368`). The old code freed
those bytes after decoding. This adds up to the Ogg size per open sound
(≤ 5.1 MB for bgm27) — small next to the 52–120 MB it replaces, but it is a
new retention with no reference counterpart (the reference decodes from the
storage stream on demand).

---

## 4. Retention findings

Ranked by severity. "No reference counterpart" means the reference does not
keep this allocation.

### R1 — Script-owned bitmaps are never freed (unbounded; no reference counterpart)

* `crates/compat/src/natives/visual/bitmap.rs:295-317` `bitmap_destroy`
  only removes the id from `VisualState.objects.bitmaps`; it never calls
  `scene.release_bitmap`.
* `crates/engine/src/scene.rs:1769-1785` `release_bitmap` returns early when
  `bitmap.script_owned` is set, and `collect_unused_bitmaps`
  (`crates/engine/src/scene.rs:1790-1798`, the `System.clearGraphicCache`
  path at `crates/compat/src/natives/system.rs:1225-1232`) goes through
  `release_bitmap`, so it cannot free them either.
* `script_owned` is set to `true` in four places
  (`bitmap.rs:130,261,366,383`) and **never set back to `false`** anywhere in
  the tree.
* Result: every `new Bitmap(...)` (loaded image, sized bitmap, 32×32 default)
  leaves its `BitmapState.rgba` in `Scene.bitmaps` until the whole scene is
  dropped. In the Vulkan path the GPU texture also stays, because
  `prune_stale_textures` keys on the scene bitmap still existing
  (`crates/app/src/lib.rs:638-654`).
* Reference: `tTVPBaseBitmap` is refcounted; the TJS Bitmap's native
  destructor releases its reference and the bitmap is freed when the last
  owner goes (`reference/cpp/core/visual/impl/LayerIntf.cpp`, the
  `tTVPBaseBitmap` refcount used throughout).

The engine test `script_owned_bitmap_survives_layer_destroy`
(`crates/engine/tests/scene.rs:342`) pins the *while-alive* half of the
reference contract (a script bitmap must survive a layer destroy). The missing
half is the release when the TJS object dies.

**Impact for this game:** every KAG scene that creates `Bitmap`s (transitions,
effects, some sprites/map chips) leaks their pixels for the rest of the
process. The title screen holds 51 bitmaps and is stable, so the leak does not
show there; across 84 `.ks` scenarios it can accumulate to hundreds of MB.
This is the most likely *non-audio* contributor to the 700 MB and the only
unbounded leak found.

**Fix sketch:** in `bitmap_destroy`, before dropping the payload, clear
`script_owned` and call `scene.release_bitmap(id)` (which still refuses if a
layer references it). Keep the existing test.

### R2 — `prerendered_storage` is never evicted (110.7 MB here)

* `crates/compat/src/resource.rs:60` `prerendered_storage:
  HashMap<String, Rc<PrerenderedFont>>`; `load_prerendered` inserts and never
  removes (`resource.rs:158-205`).
* `unmap_prerendered` (`resource.rs:145-147`) removes only the *key→font*
  entry from `prerendered`; the `prerendered_storage` entry (and its whole
  `image` buffer) stays forever.
* Reference: `tTVPPrerenderedFont::Release` deletes `this` (and its `Image`)
  at refcount 1 and removes itself from the global `TVPPrerenderedFonts`
  table (`reference/cpp/core/visual/PrerenderedFont.cpp:83-90`);
  `TVPUnmapPrerenderedFont` releases the mapping
  (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:169-185`).

For *this* game `PrerenderedFontInit()` maps all 24 fonts and never unmaps, so
the reference also holds ~110.7 MB — the present number is reference-faithful.
The divergence is that a script that unmaps/remaps (font selection) grows the
port without bound while the reference frees.

**Fix sketch:** on unmap, drop the `prerendered_storage` entry when no
`prerendered` mapping still uses it (or implement a refcount).

### R3 — `SoundSource::materialized` `OnceLock` never cleared (latent, up to ~120 MB per source)

* `crates/engine/src/sound.rs:508` `materialized: OnceLock<DecodedAudio>`;
  `audio()` at `:594-607` does `get_or_init(|| self.decode_entire()…)` and is
  never reset.
* A streaming source whose `audio()` is ever called keeps the **entire**
  decoded PCM for the source's lifetime.
* Current callers: the phase-vocoder path materializes through
  `decode_entire` (`crates/compat/src/natives/sound.rs:711-714`), not
  `audio()`, so the `OnceLock` may be unreachable today
  (`crates/compat/src/natives/sound/phase_vocoder.rs:671` calls `audio()` on
  an already-materialized buffer). It is a latent full-buffer retention.
* Reference: the phase-vocoder filter owns whole `InputSegments`/
  `OutputSegments` only while the filter runs
  (`reference/cpp/core/sound/PhaseVocoderFilter.h:81-82`).

### R4 — Compressed audio bytes retained (≤ 5.1 MB per sound)

See §3. `AudioDecoderState.bytes: Arc<[u8]>` (`crates/platform/media/src/audio.rs:61-71`).
The reference reads from the storage stream, so it has no counterpart.

### R5 — Coverage cache evicts FIFO, not LRU (bounded)

* `crates/engine/src/text.rs:1358-1365`: `insert` pushes the key to `order`
  and, at `MAX_COVERAGE_CACHE_ENTRIES = 1300`, pops the front; a cache *hit*
  does **not** move the key to the back.
* Reference `TVPFontCache.FindAndTouchWithHash` is an LRU touch
  (`reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:257-263`), limit
  `TVP_CH_MAX_CACHE_COUNT = 1300` (`:108,222-225`).
* Memory is bounded to 1300 masks (~2–8 MB at the game's font sizes); the
  difference is behavioural (hot glyphs can be evicted under churn → more
  rasterization), not a leak.

### R6 — Layout cache: arbitrary cap + full clear (bounded, no reference counterpart)

* `crates/engine/src/text.rs:85` `MAX_LAYOUT_CACHE_ENTRIES = 512`; on overflow
  `:676-679` clears the whole map. There is no reference text-layout cache, so
  this is an L2-only cache. Bounded by 512 `TextLayout`s (each a
  `Vec<LaidOutGlyph>`), typically well under 1 MB.

### R7 — `set_cache_bitmap` does not release the previous bitmap (fragile, currently safe)

* `crates/engine/src/scene.rs:1861-1868` overwrites `layer.cache_bitmap`
  without releasing the old id. All three callers pre-release
  (`layer_ops.rs:2303-2305`, `ensure_transition_display`
  `layer_ops.rs:2340-2360`, `transitions.rs:1104`), so there is no leak today;
  a future caller could leak a whole-frame cache (3.7 MB).

### R8 — `Entry.raw_name` is redundant (≈0.5 MB)

* `crates/platform/fs/src/xp3.rs:459,568,575`: every `Entry` stores both
  `raw_name` and the normalized `name`, but `raw_name` is read nowhere outside
  `xp3.rs`. Dropping it saves one `String` per entry (23 572 × ~19 B ≈ 0.45 MB
  plus allocator overhead).

### Not issues (checked)

* **GPU textures**: `drop_texture` removes + destroys
  (`crates/platform/render/src/vulkan/mod.rs:399-413`), resize/drop recreates
  (`crates/app/src/lib.rs:598-608`), `prune_stale_textures`
  (`:638-654`) frees textures whose scene bitmap is gone, and `VulkanState`'s
  drop destroys the rest (`mod.rs:899-902`). Correct — but see R1: a leaked
  scene bitmap keeps its texture alive too.
* **Glyph atlas**: bounded by construction (`allocate` returns
  `TextError::AtlasFull`, `crates/engine/src/text.rs:1000-1010`); it is only
  instantiated in tests (`GlyphAtlas::new` has no non-test caller), so it is
  not a runtime contributor.
* **Layer subtree caches**: `dec_cache_enabled_count` releases both
  `cache_bitmap` and `cache_content` (`scene.rs:1732-1764`);
  `ensure_layer_content`/`ensure_transition_display` release stale caches
  (`layer_ops.rs:2296-2360`); `remove_layer`/`destroy_layer` release the layer's
  own MainImage and both caches (`scene.rs:965-990, 1080-1120`). Correct.
* **XP3 archive body**: never materialized; reads go through positional
  `read_at` (`crates/platform/fs/src/xp3.rs:600-620`). The index is ~4.5 MB.
* **`Vec::with_capacity` from script data**: no unbounded script-sized
  reservation found; bitmap sizes use `checked_mul` + `try_reserve_exact`
  (`scene.rs:1537-1547`, `1640-1650`), the XP3 index uses checked sizes
  (`xp3.rs:377-392`), and the font parser checks every offset
  (`resource.rs:744-770`).

---

## 5. Ranked optimization list

| rank | change | expected saving | risk |
|---:|---|---|---|
| 1 | **R1** release script-owned bitmaps when the TJS `Bitmap` dies (clear `script_owned`, `release_bitmap`) | unbounded; the main non-audio share of the 700 MB over a playthrough; a single 1280×720 bitmap is 3.7 MB, the title already holds 4 full-screen | medium — must preserve "survives layer destroy while the object lives" (test exists); touches L3 native + L2 |
| 2 | **Audio** (already landed): stream instead of materialize | **~46 MB per live BGM** (median 52.0 → ~5.4 MB), 114 MB for bgm27; ≈ the bulk of the reported 700 MB if several BGMs/voices were live | low (landed, tests) |
| 3 | **R2** evict `prerendered_storage` on unmap / refcount it | **110.7 MB max** (0 for this title, which never unmaps; matching the reference) | low–medium (behavioural: fonts must stay alive while mapped) |
| 4 | **R4** stream audio from storage instead of holding compressed bytes | ≤ 5.1 MB per live sound | medium (decoder plumbing) |
| 5 | **R3** clear/avoid `SoundSource::materialized` | up to ~120 MB per filtered sound (latent) | low |
| 6 | **R6/R5** bound the layout cache by bytes / make coverage LRU | < 1 MB, plus fewer re-rasterizations | low |
| 7 | **R8** drop `Entry.raw_name` | ~0.5 MB | low |

Because the reference also holds the 24 `.tft` fonts, R2's 110 MB is not a
port-vs-reference regression; it is the largest *present* allocation but only
becomes a divergence when a script unmaps. The largest genuine divergence from
the reference is **R1**.

---

## 6. What could not be measured

* **Gameplay**: headless has no input, so the KAG script stays at the title and
  never plays BGM/voice or churns scene bitmaps. The 700 MB gameplay figure and
  the audio/leak split are therefore estimated from asset sizes, not sampled.
  Reproducing it needs a windowed run or a scripted input path.
* **GPU**: the null backend creates no Vulkan objects; GPU texture memory is
  unmeasured (path audited only).
* **A release build of the current tree**: does not exist and cannot be built
  under the no-cargo rule. The debug binary is the current-tree proxy; the
  release binary is the pre-audio-fix proxy.
