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
scope. Findings are reported in §4.1 instead. `crates/render/src/blend.rs`
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

5. **`crates/render/src/sync.rs` — RGBA clone on upload.** `upload_bitmap`
   still does `bitmap.rgba.clone()` into `Image::new` on every dirty upload.
   Moving the buffer (`std::mem::take` + refill, or uploading a sub-rect and
   only re-copying the changed region) removes a full-screen memcpy on every
   `drawText`/raster/transition repaint. High value for `@update`/`@blackout`
   and dialogue redraws.
6. **`crates/render/src/sync.rs` — `compose_layer` per-layer allocations.**
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
10. **`crates/render/src/blend.rs` — warning lock (low priority).**
    `warn_dst_dependent_once` uses a global `Mutex<HashSet>`. It is only
    reached for destination-dependent blend modes; if a game starts using
    them heavily, replace it with an `AtomicU32` bitmask keyed by mode so
    `render_path_for` never takes a lock.
11. **Image/audio decode still synchronous (per `optimization.md`).**
    Long-image TLG/PNG decode (645 ms startup) and long-BGM packet decode are
    already off-thread for audio; image decode workers + a prefetch cache
    remain the single largest startup latency opportunity and are outside this
    pass's ownership.

---

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
