# krkr-rs — Real-game frame/loading performance hotspots

Audit only. **No source file was modified.** Deliverable: `docs/audits/perf-hotspots.md`.

> **Status, added after the fact.** Hotspot **#1 was fixed in `00a07d9`**, and its
> consequence **#5 with it**, on a tree that already contained this audit. The
> reference rule is `CacheRecalcRegion`: `Complete()` returns the cached image
> while that region is empty (`reference/cpp/core/visual/LayerIntf.cpp:7734-7737`)
> and a transition update never fills it, because a temporary update skips the
> region append (`:6305-6335`), so the subtree is composited once per content
> change and the cached images are blended per frame. The engine now gates on
> `LayerState::cache_recalc` plus a source-generation sum, and the compat
> transition writes through a path that does not bump `Scene::revision`, so the
> per-frame re-projection is gone too. `crates/engine/tests/layer_ops.rs` and
> `crates/compat/tests/transitions.rs` cover the reuse, the mid-transition source
> change and the no-reprojection property.
>
> The other findings were re-checked against the code afterwards, and two needed
> correcting. **#2** was partly stale: the character and shadow cache already
> existed (`00a07d9`) and the double rasterization was already gone (`b6188f7`);
> the real remaining defect was that both text caches evicted in insertion order
> while the reference is an LRU, which thrashes whenever the working set exceeds
> the limit — that is now an LRU. **#3** is fixed for the stall: a frame's dirty
> bitmaps are staged, recorded and submitted once with one wait, instead of one
> submit and one blocking wait per bitmap. Its second half, re-uploading only the
> changed rectangle, is **not** fixable at that seam as the code stands —
> `BitmapState` carries only a `dirty` flag and no region, so the caller has no
> rectangle to pass; adding one is an L2 change across every pixel-writing site.
> **#4** is the software-blit cost; **#6** is the environment. Read the impact
> column below as of the audit's date, not as the current state.

Game under test: `/mnt/DATA/Games/Others/test` (KAG title; `system/messagearea.tjs`,
`system/advscreen.tjs`). Branch `migration/middle-layer-ash`. The reference
`reference/cpp/**` is the only oracle.

Author: performance audit (read-only). Date: 2026-10-11.

---

## 0. TL;DR — ranked hotspots

| # | Hotspot | Where | Class | Impact |
|---|---|---|---|---|
| **1** | Whole-subtree transition re-renders **both** cached subtrees, clones both bitmaps and re-blends **every frame**; the reference reuses the cache until the source changes | `crates/engine/src/layer_ops.rs:2532` (`render_layer_subtree`), `crates/compat/.../transitions.rs:1092,1096,1100-1101`, `:1080` | **Fixed in `00a07d9`** — see the status note above | **Critical** — matches "clicking settings / transitions hangs" and "laggy gameplay" |
| **2** | Per-character text draw re-rasterizes glyph outlines and re-blurs the shadow on every `drawText`, with **no character/shadow cache**; the reference caches both | `crates/engine/src/text.rs:1254,1316,1507`; `crates/compat/src/resource.rs:400,408,437` | Cache and the double-raster fix already landed (`00a07d9`, `b6188f7`); the **eviction was FIFO where the reference is LRU** — **fixed** | **High** — matches "dialog pops up hangs" |
| **3** | Every dirty bitmap is re-uploaded **in full** to the GPU each frame, and each upload is a **synchronous** submit + `wait_for_fences(u64::MAX)` | `crates/app/src/lib.rs:441`; `crates/platform/render/src/vulkan/texture.rs:299,516` | Per-bitmap stall **fixed** (one batch submit per frame); the full re-upload needs a region the seam does not carry | High (Vulkan) — amplifies 1 and 2 |
| **4** | Subtree/transition per-pixel software blits and blends (`fill(0)` + `read_pixel`/`write_pixel` per pixel, no row specialization) | `crates/engine/src/layer_ops.rs:451,511,2165,2563`; `crates/engine/src/trans/mod.rs:721,748,783` | Pre-existing | High in debug, medium in release |
| **5** | Transition re-render forces a **full scene re-projection** every frame (`bitmap_mut` bumps revision; `project_scene` allocates a fresh tree) | `crates/engine/src/scene.rs:1700`; `crates/app/src/lib.rs:325`; `crates/engine/src/projection.rs:476` | Consequence of #1 — **fixed with it** | Medium |
| **6** | Startup/loading blocks in **FUSE (`fuseblk`/NTFS-3G) I/O for tens of seconds** | environment (`/mnt/DATA` mount) | Environmental, not code | High for *this* user, not a code defect |

The two "prime suspect" changes were checked against `git show` and the reference.
Only **#1 is a true new regression**. The shadow rewrite is *faster* than the code
it replaced (details in §2). **#2 is a real, severe pre-existing gap** that the
shadow rewrite made more visible (now the shadow path exists correctly but is
still uncached).

---

## 1. Method, environment and what could be measured

Constraints respected: no `cargo`/`rustc`/`clippy`/`test` run (hard repo rule),
no source edits. I ran the already-built `target/debug/krkr` and the helper
scripts only.

What I measured / observed:

* **Headless run is dominated by filesystem wait, not CPU.**
  `timeout 120 ./target/debug/krkr --headless --frames 30 --base-dir
  /mnt/DATA/Games/Others/test` → **`real 2m00s, user 5.3s, sys 0.7s`**, killed by
  the timeout still in `startup.tjs`/`Window.tjs` init.
* A live sample of the process showed `State: S (sleeping)`,
  `wchan = request_wait_answer` — a **FUSE request wait**. `/mnt/DATA` is
  `fuseblk` (NTFS-3G): `mount` → `/dev/sda1 on /mnt/DATA type fuseblk …`. The
  repo itself is on that mount, so absolute timings in this environment are
  polluted by NTFS-3G latency and by concurrent `cargo` builds that were running
  during the audit (`ps` showed `cargo`/`rustc` actively linking `tjs`).
* Because of the above I could **not** get clean end-to-end frame timings, and
  the headless harness does not reach a dialog/transition frame. The numbers in
  §2/§3 are order-of-magnitude estimates derived from the loop structure and the
  game's own parameters (canvas-sized ⇒ `W*H ≈ 0.9M` at 1280×720), clearly
  labelled as estimates.
* `perf` is not installed and `ptrace` attach was denied
  (`yama/ptrace_scope = 1`), so no sampled profile.

Game facts extracted from `data.xp3` (scripts are UTF-16):

* `system/messagearea.tjs` draws **one character per `drawText` call**
  (`output()`/`charOutput()`, line 356), with
  `shadowlevel:3024, shadowwidth:3, shadowx:0, shadowy:0` (lines 28–32).
  `flush()` (line 467) draws all pending characters in one tight loop, so a
  dialog that is shown instantly does `N` `drawText` calls in a single frame.
* `system/advscreen.tjs` starts transitions with
  `beginTransition("crossfade"|"scroll"|"universal", true, this, elm)`
  (lines 3353, 3718–3739) — the second argument `true` is `withchildren`, and
  `withchildren` **defaults to true** (`LayerIntf.cpp:9819-9821`). So the
  whole-subtree path is the normal path for this game.

---

## 2. Prime suspect #1 — text shadow is a per-character cone convolution

### What the code does now

`Layer.drawText` / `ImageFunction.drawText` → `draw_text_into_bitmap`
(`crates/compat/src/resource.rs:350`):

1. If no pre-rendered `.tft` mapping exists, `layout` (cached) is resolved.
2. If `shadow_level != 0`:
   * `rasterize_text_coverage(...)` → **one coverage mask per glyph**
     (`crates/engine/src/text.rs:1254`), each calling `engine.rasterize_glyph`
     (`text.rs:693`) → L1 `render::rasterize_glyph` (`crates/platform/render/src/glyph.rs:139`),
     which runs the full `ab_glyph` outline + `outline.draw` every call — **no
     cache anywhere in that chain**.
   * for **each** mask, `blur_coverage(mask, level, shadow_width)`
     (`text.rs:1316`) — the integer cone convolution.
   * `blit_coverage_into` per mask (`text.rs:1425`).
3. Main pass: `compose_text_into` (`text.rs:1492`) — which **again** calls
   `rasterize_text_coverage` internally (`text.rs:1507`).

So one `drawText` of a single character (which is exactly what KAG's
`charOutput` does) performs **two complete outline rasterizations and one cone
blur** of that glyph. Nothing is memoized across calls.

`blur_coverage` cost per glyph: the expanded mask has
`(W+2bw) × (H+2bw)` pixels and the kernel loops `(2bw+1)²` taps, i.e.
`(2bw+1)² · (W+2bw)(H+2bw)` inner operations. For the game's `shadowwidth=3`
and a ~24–32 px CJK glyph that is roughly `49 × (30×30) ≈ 44k` integer ops per
character, plus two `ab_glyph` outline rasterizations. `flush()` of a 200–400
character message therefore does 400–800 outline rasterizations + 200–400 cone
blurs in one frame.

### What the reference does

The reference caches the rasterized **and** blurred character in
`tTVPFontCache`, keyed by `tTVPFontAndCharacterData` (face, char, antialias,
`BlurWidth`, `BlurLevel`, `Blured`):

* `TVPGetCharacter` — `reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:244-320`:
  hash lookup; on miss, rasterize and, **if `font.Blured`, call `data->Blur()`
  before inserting** — so the blurred shadow is cached too.
* `DrawTextSingle` — `LayerBitmapImpl.cpp:1114-1200`: for a shadow it sets
  `font.Blured = true` and calls `TVPGetCharacter` again, receiving the
  **cached** blurred character.
* The cache object: `LayerBitmapImpl.cpp:224-232` (`tTVPFontCache`,
  `TVP_CH_MAX_CACHE_COUNT`); cleared only on rasterizer switch / low-memory
  (`:98,166,180,227-232`).
* The cone kernel itself is `TVPChBlurCopy65_c`,
  `reference/cpp/core/visual/tvpgl.cpp:10738-10813` — the port reproduces its
  arithmetic faithfully; the divergence is the missing cache, not the math.

### Is this a new regression?

**No — not relative to the code it replaced.** `git show 3ed61ca` shows the
previous shadow was:

```rust
for dy in -radius..=radius {
    for dx in -radius..=radius {          // disk of radius = shadow_width
        compose_text_into(..., origin + (dx,dy), ...)   // re-rasterizes EVERY glyph
    }
}
```

i.e. `π·r²` calls to `compose_text_into`, each of which re-rasterized the whole
run. For `r=3` that is **~29 rasterizations per glyph**; the new code is **one
rasterization + one 49-tap convolution**. The new cone path is *cheaper* per
`drawText` than what it replaced.

What `3ed61ca` **did** add as waste: it rasterizes the run for the shadow masks
and then calls `compose_text_into`, which rasterizes the same run **again**. The
masks are already in hand; a main-pass blit could consume them.

**Verdict:** the uncached character text path (`compose_text_into` calling
`rasterize_glyph` per draw) is **pre-existing**. The observed "dialog popup
hangs" is explained by that missing cache, now exercised by the correct shadow
path. `3ed61ca` added a redundant second rasterization. No commit has *cached*
the character/shadow coverage.

### Concrete fix (mirror the reference)

Add a character cache owned by L2 (the reference's `tTVPFontCache`), not L1
(D9 forbids L1 state). Two options, both matching the reference key:

1. Extend `TextEngine` with a `HashMap<CoverageKey, Arc<CoverageMask>>` (key =
   font, codepoint, `size_px.to_bits`, bold, italic, antialias) and a second
   cache keyed additionally by `(blur_level, blur_width)`. `rasterize_text_coverage`
   and `blur_coverage` consult it; `TextEngine::layout` already takes `&mut self`,
   so the facade can thread the mutable cache (or use `RefCell` as the atlas does
   via its own `&mut self`).
2. Or reuse the existing `GlyphAtlas` cache for sharp coverage and add a
   shadow-mask cache beside it.

Also **remove the double rasterization** in `draw_text_into_bitmap`: compute the
masks once, blur a clone for the shadow, and blit the sharp masks for the main
pass (`compose_text_into` is only needed for the non-shadow path). Expected
effect on a flushed 300-char dialog: ~600 outline rasterizations → ~0 (cache
hits after the first frame the glyph is seen) + 300 blurs amortized to zero for
repeat characters; the reference behaves this way.

Adjacent (non-perf) observation while here: the game sets `shadowlevel:3024`,
and `resource.rs:405` clamps it to `u8` (`clamp(0,255)`), whereas the reference
keeps `tjs_int` and lets `TVPChBlurCopy65` saturate the multiply
(`tvpgl.cpp:10780-10786`). Not a speed issue, but worth a correctness ticket.

---

## 3. Prime suspect #2 — transition subtree cache re-rendered every frame

### What the code does now

`Host::step` → `compat::advance_transitions`
(`crates/compat/src/natives/visual/layer/transitions.rs:681`) runs **once per
frame** and, for every active transition with `withchildren`, calls
`composite_transition` (`:1080`). Its `with_children` branch (`:1087-1113`):

```rust
let dst_render = render_layer_subtree(scene, id)?;       // re-render dest subtree
let src_render = render_layer_subtree(scene, source)?;   // re-render source subtree
let (src1, src2) = (scene.bitmap(dst_render).cloned(),   // full clone
                    scene.bitmap(src_render).cloned());  // full clone
let dst = scene.bitmap_mut(dst_render)?;                 // bumps revision
trans.apply(dst, &src1, &src2, region)                   // full-canvas blend
```

`render_layer_subtree` (`crates/engine/src/layer_ops.rs:2532`) does, **every
call, with no dirty check**:

* `ensure_layer_cache` (`:2297`) — reuses the bitmap only if the size matches
  (so no per-frame *realloc*, good), but …
* `collect_subtree_draws` (`:2455`) — for **every** visible layer in the
  subtree, `subtree_source_image` (`:2362`) does
  `layer.bitmap…and_then(|id| scene.bitmap(id)).cloned()` (`:2383`): a **full
  clone of each child bitmap, every frame**;
* `target.rgba.fill(0)` (`:2563`) — full-canvas clear;
* blit each clone into the cache with per-pixel `blit_copy` / `blit_over` /
  `stretch_blit_mode` (`:451`, `:511`, `:1851`, `:2165`).

Then `composite_transition` clones both cache bitmaps again and `trans.apply`
(`crates/engine/src/trans/mod.rs:567`) runs a full-region per-pixel blend
(`:721`/`:748`).

So per transition frame, over a canvas-sized subtree, the engine does roughly:
**2 subtree composites (clear + blit per layer) + 2 full bitmap clones + 1
full-canvas per-pixel blend**, and `bitmap_mut` bumps `Scene::revision`
(`scene.rs:349,1700`) forcing a full re-projection + render in `Host::step`
(`crates/app/src/lib.rs:294,325`). At 1280×720 (≈0.9 M px) that is on the order
of 8–12 full-canvas passes per frame; in a debug build a single per-pixel pass
over 0.9 M px is on the order of tens of milliseconds, so a transition frame can
take hundreds of milliseconds to seconds — a visible hang. In release it is
still tens of milliseconds per frame, i.e. sustained lag.

### What the reference does

`tTJSNI_BaseLayer::Complete` (`reference/cpp/core/visual/LayerIntf.cpp:7673`):

* trivial subtree → return `MainImage` (`:7718-7725`);
* **`if(CacheRecalcRegion.GetCount() == 0) return CacheBitmap;`**
  (`:7734-7737`) — **the completed cache is reused while nothing changed**;
* only otherwise does it call `InternalComplete` to re-composite.

`CacheRecalcRegion` is appended to **only** when `tempupdate == false`
(`LayerIntf.cpp:6305-6335`), and a transition update is `tempupdate == true`
(“the layer content is not changed, but the layer need to be updated … mainly
used by transition update”). Therefore during a whole-subtree transition the
reference composites each subtree **once** and then reuses `CacheBitmap`; it
does **not** re-clone the source bitmaps per frame. `AllocateCache`
(`:3954-3962`) ORs the full rect into `CacheRecalcRegion` so the cache is built
once at enable time, and `Src1Bmp`/`Src2Bmp` are (re)captured from the cheap
`Complete()` (`LayerIntf.cpp:6431-6435`), which is a pointer return, not a
re-render. Caching for both endpoints is enabled at start
(`LayerIntf.cpp:7869-7876`) and released on teardown (`:7961-7964`).

### Is this a new regression?

**Yes.** Before `a63b9fb`, `withchildren` was ignored and the port transitioned
main images only (`git show a63b9fb` diff of `transitions.rs`). Commit `a63b9fb`
introduced `render_layer_subtree`, the per-frame calls at
`transitions.rs:1092/1096`, the two `.cloned()`s, and the caches
(`inc/dec_cache_enabled_count`, `scene.rs:1624/1636`). It is the direct cause of
a per-frame full-canvas software recomposite during every transition — exactly
the "clicking settings/transitions hangs" symptom.

### Concrete fix (match `Complete`)

Add a per-cache **dirty generation** to `LayerState` (e.g.
`cache_recalc: bool` / `cache_generation: u64`), set when a layer property,
child set, bitmap pixels, or the endpoint content changes, and clear after the
cache is composited. `render_layer_subtree` should:

1. return the cached bitmap immediately when the subtree is clean (the
   `CacheRecalcRegion.GetCount()==0` early return), and
2. capture `Src1Bmp`/`Src2Bmp` **once** at `beginTransition` (or on first use),
   not per frame; `composite_transition` should blend into the destination
   without re-rendering and without cloning the sources (`trans.apply` can read
   `&Scene` bitmaps by reference).

If a dirty-tracking refactor is too large for one change, the minimal fix is to
render each subtree **once at transition start** and cache the two `BitmapState`s
in the `LayerTransition` entry (L3 state already exists,
`crates/compat/.../layer/mod.rs:53`), re-rendering only when the endpoint
reports a change. Either way, drop the unconditional per-frame
`render_layer_subtree` + `.cloned()` pair.

---

## 4. Rest of the frame — other algorithmic issues

### 4.1 Synchronous full-bitmap GPU upload per dirty bitmap — pre-existing, high

`Host::step` calls `sync_dirty_bitmaps` (`crates/app/src/lib.rs:441`) every
frame. It scans **all** bitmaps (`O(bitmaps)`), and for each dirty one:
`create_texture` only if the size changed (good), then `upload_texture` of the
**entire** bitmap (`region = (0,0,w,h)`, `:501-513`). On Vulkan,
`render::upload_texture` → `vulkan/texture.rs:299` →
`record_and_submit` (`:516`) which does `queue_submit` followed by
`wait_for_fences(&[fence], true, u64::MAX)` — i.e. **every upload blocks the CPU
until the GPU completes**, and resets/records a command buffer + layout
transitions per upload.

The reference updates only the changed rectangle (`LayerIntf::Update(rect)` and
`Manager->AddUpdateRegion`, `LayerIntf.cpp:6305-6360`) and, on GPU, composites
with `tCompleteDrawable_GPU`/`BltImage` in device memory. Effect: any per-frame
redraw of a canvas-sized layer (the message layer while text types, the
transition caches, `@blackout`) costs a full copy + a GPU round-trip per frame.
Fix: upload only the dirty sub-rectangle (the `PixelRegion` parameter already
exists), and batch all uploads of a frame into one command buffer / one fence
wait.

### 4.2 Per-pixel software blits/composites — pre-existing, high in debug

`blit_copy_clipped` (`layer_ops.rs:463`), `blit_over` (`:511`),
`blit_plane` (`:571`) and `stretch_blit_mode` (`:1851`) all call
`read_pixel`/`write_pixel` (`:102-118`) — each a bounds-checked
`pixel_offset` + 4-byte copy — inside the innermost loop, with no row
specialization, no `chunks_exact_mut(4)`, no bulk copy. `compose_text_into` and
`blit_coverage_into` (`text.rs:1425`) use a generic per-pixel
`blend_tinted_coverage` with `f32` math per channel. The projection reaches the
screen through these same helpers.

These are the building blocks of #1, #3 and the text path. A rowwise fast path
(`copy_from_slice` for `mode=0`/opaque and `32`-bit word operations for the
common `SourceOver`) would remove most of the debug-build cost.

### 4.3 Full re-projection forced every transition frame — consequence of #1

`scene.bitmap_mut` calls `touch()` (`scene.rs:1700`) which bumps
`Scene::revision`; `Host::reproject_render_scene`
(`crates/app/src/lib.rs:325`) rebuilds when the revision changes. Because the
transition writes the cache every frame, the projection rebuilds every frame
and `project_scene` (`projection.rs:476`) allocates a **fresh `RenderScene`** +
`ProjectionState` (two `HashMap`s + a `HashSet`) each time, then
`Host` clears+extends its arena. It is not O(n²) — `window_layer_order`
(`scene.rs:1785`) is a single pass plus sibling sorts — but it is per-frame
allocation churn proportional to layer count. Fixing #1 removes this.

### 4.4 Per-frame scans that are cheap but repeated

* `paint_poll` (`crates/compat/.../layer/mod.rs:950`) scans all layers for
  `pending_paint` each frame (usually empty).
* `sync_dirty_bitmaps` scans all bitmaps each frame.
* `prune_stale_textures` (`app/src/lib.rs:516`) scans all textures each frame.
  All three are `O(n)` with `n` small (23 layers / 44 bitmaps) and are not the
  problem; noted for completeness.

### 4.5 Checked and found **not** to be hotspots

* No per-frame image/audio decode: the transition `rule` mask is decoded once in
  `rule_mask` (`transitions.rs:440`); `decode_image`/`read_image_from_storage`
  are only on script calls. `movie_poll`/`video_advance`
  (`plugins/layer_ex/movie.rs:286`, `natives/video.rs:1695`) decode only while a
  video is playing.
* No per-frame texture creation: `create_texture` runs only when dimensions
  change or the texture is missing.
* No blocking `thread::sleep` outside tests (`compat/src/natives/visual/mod.rs:186`
  is test-only).
* `window_layer_order` is **not** O(n²) anymore (parent index + position map).
* The cone kernel and the transition blend math match the reference arithmetic
  (`tvpgl.cpp:10738-10813`; `trans/mod.rs:721/748`), so the per-pixel formulas
  themselves are not the divergence — the missing caching and the CPU/GPU
  placement are.

### 4.6 Environmental: the real-game "loading hang" is largely FUSE I/O

The blocking sample (`wchan = request_wait_answer`) and the
`real 2m00s / user 5.3s` profile show the process mostly waiting on the
`fuseblk` (NTFS-3G) mount at `/mnt/DATA`, not computing. The storage index parse
itself is one pass (`fs/xp3.rs` `read_index`, `read_entry_with_filter`), and
mounted archives keep an open handle (`Storage::with_archive`,
`crates/engine/src/storage.rs:628`), so this is host-filesystem latency rather than an
obvious read-pattern bug. It nonetheless makes "clicking settings" feel like a
hang on this machine: every `Storage::read` (image decode, rule image, font)
goes through NTFS-3G. Recommendation for benchmarking: copy the game to a local
ext4 path before attributing load times to the engine.

---

## 5. Suggested order of work

1. **Cache the subtree completion (#1).** Biggest win and the clearest reference
   divergence; removes the per-frame composites, the per-layer bitmap clones and
   the forced re-projection for the whole transition.
2. **Add the character/shadow cache (#2) and drop the double rasterization.**
   Makes dialog pop-ups cheap and matches `tTVPFontCache`. Do this in L2.
3. **Sub-rectangle + batched texture uploads (#3).** Removes the synchronous
   full-canvas GPU round-trips per frame.
4. **Rowwise blits / word-wise copy (#4).** Multiplies the win of 1–3,
   especially in debug builds.
5. Re-measure from a local ext4 copy of the game to separate FUSE latency from
   engine cost.

---

## 6. Reference citations used

| Claim | Citation |
|---|---|
| Character + blurred-shadow cache | `reference/cpp/core/visual/impl/LayerBitmapImpl.cpp:224-232`, `:244-320` |
| Per-char draw uses the cache; shadow is `font.Blured` | `LayerBitmapImpl.cpp:1114-1200` |
| Inline glyph shadow caches/blurs too | `LayerBitmapImpl.cpp:1029-1064` |
| Cone blur arithmetic | `reference/cpp/core/visual/tvpgl.cpp:10738-10813` |
| `Complete` early-returns the cache when clean | `reference/cpp/core/visual/LayerIntf.cpp:7673-7757` (esp. `:7734-7737`) |
| `CacheRecalcRegion` only on non-temp update | `LayerIntf.cpp:6305-6335` |
| `AllocateCache` rebuilds once | `LayerIntf.cpp:3954-3962` |
| `Src1Bmp`/`Src2Bmp` from cheap `Complete()` | `LayerIntf.cpp:6431-6435` |
| Caching enabled/released per transition | `LayerIntf.cpp:7869-7876`, `:7961-7964` |
| `withchildren` default true | `LayerIntf.cpp:9819-9821` |
| Partial-region updates | `LayerIntf.cpp:6305-6360` |

## 7. Files inspected (no edits)

`crates/engine/src/{text.rs,scene.rs,layer_ops.rs,projection.rs,trans/mod.rs}`,
`crates/compat/src/{resource.rs,frame.rs}`,
`crates/compat/src/natives/visual/layer/{transitions.rs,mod.rs}`,
`crates/platform/render/src/{glyph.rs,vulkan/texture.rs}`,
`crates/app/src/{game.rs,lib.rs}`,
plus `reference/cpp/core/visual/{LayerIntf.cpp,impl/LayerBitmapImpl.cpp,tvpgl.cpp}`
and the game scripts extracted from `data.xp3`.
