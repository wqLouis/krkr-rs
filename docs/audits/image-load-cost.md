# krkr-rs — real-game image-load cost (`Layer.loadImages` → decode)

Audit only. **No source file was modified.** Deliverable: this file.
The reference (`reference/cpp/**`) is the only oracle.
Author: middle-layer image-load audit (read-only). Branch `migration/middle-layer-ash`,
working tree at `649abc6` plus uncommitted edits under
`crates/compat/src/natives/visual/layer/`, `crates/engine/src/scene.rs` and
their tests (none in the image-decode path). Line numbers are from that tree.

Game under test: `/mnt/DATA/Games/Others/test` (`data.xp3` + `patch.xp3`).
The reported run has archive preload ON (`--preload-archives`), so **file I/O
is out**: reads come from `ArchiveBacking::Memory` (`crates/engine/src/storage.rs:72`,
`:783-787`), not the FUSE mount.

---

## 0. TL;DR — ranked findings

| # | Finding | Where | Class | Expected saving | Confidence |
|---|---|---|---|---|---|
| **1** | Every image is decoded **synchronously on the main thread, inside the frame**; a scene change that loads N images pays the full sum. There is no async worker and no decoded-image cache. | `crates/compat/src/natives/visual/layer/pixels.rs:2078` → `crates/compat/src/natives/visual/bitmap.rs:76,98`; `crates/platform/media/src/image_decode.rs:191`; `crates/platform/media/src/tlg.rs:292` | Algorithmic / placement | The bulk of the 6.77 s frame | **High** |
| **2** | **No decoded-image cache between the archive and the layer.** A repeated name is decoded again from scratch. `FRM_02C11` is loaded up to ~30× by the config window. The reference *has* such a cache (`TVPGraphicCache`) but it is **opt-in** and this game does not enable it. | port: no cache in `ResourceResolver` (`crates/compat/src/resource.rs:41-58`); reference: `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1433,1473,1524,1878,2423` | Missing cache | `(N−1) × decode` for repeats only — measured **1.77 s → 0.20 s** for 10× a 1280×403 TLG (debug) | **High** (no cache) / **Medium** (saving for *this* game) |
| **3** | `decoded_from_dynamic` uses `DynamicImage::to_rgba8(&self)`, which **clones** an already-RGBA buffer; one redundant full-image allocation + copy per PNG/WebP decode. | `crates/platform/media/src/image_decode.rs:102-103`; `image 0.25.10 src/images/dynimage.rs:304-307` (`ImageRgba8(x) => x.clone()`) | Redundant copy | ~1 image-sized alloc+copy per non-TLG decode (single-digit ms debug) | **High** (the clone) / **Medium** (share of the time) |
| **4** | TLG6 allocates per-`8`-row scratch (`pixelbuf`, `curline`, and a per-channel `out`) **inside the block-row loop** instead of once per image. | `crates/platform/media/src/tlg.rs:351,360,387,708` | Allocation churn | Small–moderate | **Medium** |
| **5** | The reference graphic cache **is not implemented**; `System.graphicCacheLimit` only stores the value and runs a purge. Script-observable getter is correct; the caching effect is absent. | `crates/compat/src/natives/system.rs:100,1729-1782`; reference `GraphicsLoaderIntf.cpp:2423-2444` | Missing feature | Only matters once the game sets the limit (it does not) | **High** |
| **6** | `released scene bitmap #N` is **not** the just-loaded image: `load_layer_image` never calls `release_bitmap`. It is a previous scene/window layer's bitmap released by layer teardown. | `crates/compat/src/natives/visual/layer/pixels.rs:2078-2114` (no release); `crates/engine/src/scene.rs:1784-1798`, `:911-1004` (`remove_layer`/`destroy_layer`) | Not a bug (churn) | none | **High** (code) / **Medium** (which id) |
| **7** | Game-side: `Button.create` is given the **string** `"FRM_02C11"`, so it calls `loadImages` once per button instead of reusing `_imageList`'s shared `Bitmap`. Same in the reference (cache off). | `data.xp3>system/configwindow.tjs:605-607,643-645,681-683`; `data.xp3>system/selectitem.tjs:310-318` | Script / repeated decode | ~30× decode of a 140×26 TLG (~0.1 s debug) | **High** |

Findings 1–2 are the frame stall; 3–7 are the honest secondary items.

---

## 1. What `load_layer_image` actually does per call

`Layer.loadImages` (`extern "C" fn layer_load_images` →
`load_layer_image`) is `crates/compat/src/natives/visual/layer/pixels.rs:2078-2114`.
Per call it:

1. `read_image_from_storage(storage, name)` — probes the name and then the
   fixed `IMAGE_EXTENSION_PROBE` list (`.webp`, `.bmp`, …, `.tlg`, …)
   (`crates/compat/src/natives/visual/bitmap.rs:62-93`). The winning candidate's
   bytes come from `Storage::read`; with preload, `read_archive` →
   `read_entry_from_bytes` (`crates/engine/src/storage.rs:783-787`) still
   **zlib-decompresses** the entry (the TLG segments are `m == 1`, zlib) but
   touches no disk. This is one pass over the *compressed* entry.
2. `decode_image(services, &resolved, &bytes)` → the host codec seam
   `ResourceResolver::decode` (`crates/compat/src/resource.rs:85`), which `app`
   fills with `media::decode_image` (`crates/app/src/image_codec.rs:42-67`).
3. `apply_color_key(&mut rgba, width, colorkey)` (`bitmap.rs:141-152`). For the
   game's `loadImages(name)` calls (one argument) `colorkey == COLOR_KEY_NONE`
   and this returns immediately at `bitmap.rs:142-144`; it is **not** a pass.
4. Reuse the layer's bitmap via `replace_bitmap_pixels` (`bitmap.rs:117-128`) or
   allocate one with `Scene::add_bitmap` (`crates/engine/src/scene.rs:1472`).
   `replace_bitmap_pixels` calls `Scene::resize_bitmap`
   (`scene.rs:1496`), which **returns early when the dimensions are unchanged**
   (`scene.rs:1503-1505`); otherwise it allocates a `new_w × new_h × 4` buffer
   (the *image's* size, not the canvas), fills it with the neutral colour, and
   copies the overlapping top-left region. Then it moves the decoded `Vec`
   into `bitmap.rgba`.
5. Set `layer.image_width/height/image_modified`.

**How many full-image passes, and what are they proportional to?** For a TLG
load into a fresh layer with no colorkey: one pass over the compressed entry
(zlib), then the TLG decode itself (several passes over the image's own
pixels — see §5), then a *move* of the RGBA buffer. The layer install adds
**no** full-canvas pass: the buffer is image-sized and moved, not copied, and
`resize_bitmap` early-returns for a matching size. Nothing in this path is
proportional to the **canvas** or to the **layer count**; every pass is
proportional to the image's own dimensions.

The decode dispatch (`crates/platform/media/src/image_decode.rs:191-230`):
a TLG name hint routes to the native TLG5/6 decoder (`tlg.rs`); otherwise the
`image` crate decodes the hinted/guessed format and
`decoded_from_dynamic` converts it to RGBA8.

---

## 2. Is the same image decoded repeatedly? (yes — and the reference's cache)

**Port: no cache.** `ResourceResolver` holds only the five codec closures plus
font state (`crates/compat/src/resource.rs:41-58`); there is no name→pixels map
between `Storage` and the layer. Every `load_layer_image` /
`Bitmap.load` (they share `read_image_from_storage` + `decode_image`, see
`crates/compat/src/natives/visual/bitmap.rs:218-240`) decodes afresh.

**The game really does repeat names.** In `data.xp3>system/configwindow.tjs`:

* `IMAGELIST` (`:68-75`) declares `["PlayTest", "FRM_02C11"]`, and `setup()`
  decodes each once into a shared `Bitmap` (`_imageList`, `:322-326`).
* The 10 voice rows (`:285-294`) each carry `testButton:"FRM_02C11"`, and
  `createButton` passes the **string** to `Button.create`
  (`:605-607`, `:643-645`, `:681-683`).
* `Button.create` (`data.xp3>system/selectitem.tjs:310-318`) takes the
  `else` branch for a string and calls `loadImages(file)`. So `FRM_02C11`
  (140×26 TLG) is decoded up to ~30 times, once per generated test button,
  instead of reusing `_imageList.PlayTest`.

**Where the reference caches this.** `TVPLoadGraphic`
(`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1878`) first looks in
`TVPGraphicCache` keyed by
`tTVPGraphicsSearchData {Name, KeyIdx, Mode, DesW, DesH}`
(`GraphicsLoaderIntf.cpp:1257-1264`); a hit returns from the cache
(`:1886-1900`), a miss decodes and inserts (`:1937-1958`). The cache is:

* **disabled by default** — `TVPGraphicCacheEnabled = false` (`:1433`), and
  `TVPSetGraphicCacheLimit(0)` keeps it disabled (`:2426-2428`);
* enabled only when the script sets `System.graphicCacheLimit` to a non-zero
  value (or `-1` = the system limit) (`:2429-2436`, property setter
  `reference/cpp/core/base/SystemIntf.cpp:359-372`);
* bounded and evicted by total bytes (`TVPCheckGraphicCacheLimit`, `:1441-1454`)
  and cleared on minimize (`TVPClearGraphicCache`, `:1456-1470`);
* sharing the decoded bitmap object rather than copying
  (`TVPAllocGraphicCacheOnHeap = false`, `:1251`; `AssignToBitmap`,
  `:1367-1385`).

**Honest caveat.** I scanned all 220 `.tjs`/`.ks` entries of both archives
for `graphicCacheLimit`/`TouchImages`/`clearGraphicCache` (case-insensitive):
**zero hits**. This game never enables the reference cache. So for *this* game
the reference also re-decodes every repeat; its advantage is decoder speed,
not a warm cache. A cache in the port would therefore remove repeats that the
reference also pays for — a real optimisation, but not a reference-behaviour
fix.

**Quantified repeat cost** (release build, `--preload-archives`, null backend,
sum of 10 `new Bitmap` decodes of `frame/frm_0501b.tlg`, 1280×403):
**1966 ms**. A single decode is **197 ms** (debug). A cache removes the other
9, i.e. ~1.77 s. The memory trade is one RGBA buffer per cached image
(1280×403 ≈ 2.1 MB; 1280×720 ≈ 3.7 MB), which is exactly why the reference
bounds it by `graphicCacheLimit` and clears it on demand.

---

## 3. What is released, and why? (`released scene bitmap #N`)

`load_layer_image` (`pixels.rs:2078-2114`) contains **no** `release_bitmap`
call. It only ever reuses or allocates a bitmap. The `engine::scene] released
scene bitmap #N` line is `Scene::release_bitmap` (`crates/engine/src/scene.rs:1784-1798`),
whose callers are layer teardown and explicit "drop the image" writes:

* `Scene::remove_layer` / `invalidate_layer` (`scene.rs:911-1004`, `:1008-1017`) —
  releases the removed layer's `bitmap`, `cache_bitmap`, `cache_content`;
* `Scene::destroy_layer` (`scene.rs:1061+`) — the whole-subtree variant;
* `layer_has_image` with `wanted == false` (`crates/compat/src/natives/visual/layer/mod.rs:1842`);
* `Layer.copyRect`/`assignImages`-style adopt paths (`layer/mod.rs:628`);
* `Bitmap` invalidation (`crates/compat/src/natives/visual/bitmap.rs:289-297`);
* `Scene::collect_unused_bitmaps` (`scene.rs:1814`), reached from
  `System.clearGraphicCache` and the `System.graphicCacheLimit` setter
  (`crates/compat/src/natives/system.rs:1225-1234`, `:1349`, `:1779`).

So the released bitmap is a **previous** layer's/scene's image, not the one
just loaded. It is the old scene being invalidated (`data.xp3>system/gamescenemanager.tjs:158-162`
invalidates `_sceneObj[_scene]`; `data.xp3>system/configwindow.tjs:88-124`
`finalize()` invalidates `_base`/`_baseS`/`_btn`/`_imageList`), whose layers
drop their `MainImage` on the way out. In a clean headless startup run that
loads the title images I observed **8 `load_layer_image` and 0 releases** —
there was no old scene to tear down. The code cannot release the image it
just installed.

What I could not determine: which *specific* id each release refers to in the
provided log. What would settle it: log the layer id and the released bitmap id
together at the `remove_layer`/`release_bitmap` boundary and compare with the
`load_layer_image` line.

---

## 4. Is the 6.77 s frame explained by the above?

**Most likely yes — it is the sum of many synchronous decodes inside one
frame.** The audio pump measures the *whole* wall-clock frame
(`crates/app/src/audio.rs:308-327`), and the image loads in a scene change run
inside a timer callback inside one `GameHost::step`
(`crates/app/src/game.rs:275-293`); every decode therefore lands in that
frame's `dt`.

The arithmetic is consistent. Measured on the *same game, same archives,
preload ON*, on the null backend (no GPU), by looping `new Bitmap(name)`
(which shares `read_image_from_storage` + `decode_image` with `loadImages`):

| image | dimensions | px | debug ms/decode | release ms/decode |
|---|---|---|---|---|
| `frame/frm_02c11.tlg` | 140×26 | 3 640 | ~4 | ~0.15 |
| `frame/frm_0502e.tlg` | 181×226 | 40 906 | ~21 | – |
| `frame/frm_0507.tlg` | 539×149 | 80 311 | ~34 | – |
| `frame/frm_0502a.tlg` | 313×376 | 117 688 | ~57 | ~5.4 |
| `frame/frm_0501b.tlg` | 1280×403 | 515 840 | ~197 | ~15.3 |
| `frame/frm_0503.png` | 1280×720 | 921 600 | ~130 | ~15.2 |

The user's log is a **debug** binary (`target/debug/krkr`); debug is ~8–28×
slower than release here. A config-window/scene build that loads a few dozen
large images at ~150–200 ms each is 6–8 s; that is exactly the reported frame.
The 35–90 ms gap in the user's excerpt for the small `FRM_02C11` is larger than
the ~4 ms I measure for that file — the excerpt's image is likely a larger
variant or the gap contains more than the decode (see "Could not determine").

**Could not determine from the code/excerpt:** how many `load_layer_image`
calls fall inside the measured frame. What would settle it: emit a monotonic
frame index in `GameHost::step` and timestamp/count `load_layer_image` lines
between two frame boundaries, or put a `Instant` timer around
`decode_image` and log the per-call duration. A second, cheaper check: wrap
each `load_layer_image` with `System.getTickCount()` in a test script and sum
them against the frame `dt`.

---

## 5. Other per-image, avoidable work (ranked)

### 5.1 `to_rgba8` clones the buffer for the common RGBA case (Finding 3)

`decoded_from_dynamic` (`crates/platform/media/src/image_decode.rs:102-103`):

```rust
fn decoded_from_dynamic(image: DynamicImage) -> DecodedImage {
    let rgba = image.to_rgba8();
```

`DynamicImage::to_rgba8(&self)` is
`image-0.25.10/src/images/dynimage.rs:304-307`:

```rust
match self {
    DynamicImage::ImageRgba8(x) => x.clone(),
    x => dynamic_map!(x, ref p, p.cast_in_color_space()),
}
```

A PNG/WebP that decodes to RGBA8 (the game's PNGs) takes the `x.clone()`
arm: one extra `w*h*4` allocation + copy per decode, then the original is
dropped. `image.into_rgba8(self)` (`dynimage.rs:418-423`) moves the buffer for
the RGBA case and only converts otherwise.

Smallest honest fix: in `decoded_from_dynamic`, use `image.into_rgba8()`.
No script-observable value changes (same dimensions, same RGBA bytes).
Note this does not affect TLG decodes, which already move
(`decode_tlg_bytes` → `into_raw()`, `tlg.rs`, `image_decode.rs:94-100`).

### 5.2 TLG6 per-block-row scratch (Finding 4)

`decode_tlg6` allocates inside the `for y in (0..height).step_by(8)` loop:

* `let mut pixelbuf = vec![0u32; pixel_count];` (`tlg.rs:360`),
* `let mut curline = vec![0u32; width];` (`tlg.rs:387`),
* and `decode_golomb_lane` allocates `let mut out = vec![0u8; pixel_count*4];`
  per channel (`tlg.rs:708`).

For a 1280×720 image that is 90 block rows × up to 6 `Vec`s, each up to
~160 KB, zeroed every row. `prevline`/`img` are allocated once (`tlg.rs:351`),
so this is inconsistent. Hoisting `pixelbuf`/`curline`/`out` out of the loop
(reusing with `.fill(0)`/`.clear()`) removes the churn without changing
output. Keep the existing small `Vec` reuse discipline; do not introduce a
process-global pool (D9).

### 5.3 Game-side `copyRect` per button (Finding 7)

`Button.create` with a `Bitmap` object does
`copyRect(0,0,file,0,0,file.width,file.height)`
(`data.xp3>system/selectitem.tjs:316`), which reaches
`layer_ops::blit_copy` (`crates/engine/src/layer_ops.rs:451`). That is a
full-image copy per button, but a memcpy-class pass — far cheaper than a
decode, and identical to the reference script. No port change is warranted
here; it is listed so it is not mistaken for the stall.

### 5.4 Layer release is O(layers) per release (minor)

`Scene::release_bitmap` calls `bitmap_referenced_by_layer`, a linear scan of
`self.layers` (`scene.rs:1784-1798`, via `bitmap_referenced_by_layer`,
`scene.rs:1719`), and `remove_bitmap` rebuilds `bitmap_index`
(`scene.rs:1703-1717`). A teardown of L layers is therefore
O(L²). At the few-hundred-layer scale in this game that is ~10⁵ cheap
operations, not a multi-second cost; noted for completeness, not worth a
change unless profiling shows it.

### 5.5 What is **not** per-image

* No scaling step: `LoadImages` passes `desw = desh = 0` and the port never
  scales in this path (`pixels.rs:2078-2114`); the reference's `TVPLoadGraphic`
  likewise gets `0,0` from `tTJSNI_BaseLayer::LoadImages`
  (`reference/cpp/core/visual/LayerIntf.cpp:2823-2845`).
* No full-canvas clear and no canvas-sized scratch: `resize_bitmap` allocates
  the new image size and early-returns for a matching size (`scene.rs:1496-1513`).
* `apply_color_key` is O(1) when no key is given, which is the game's
  one-argument call shape.

---

## 6. Ruled out / confirmed cheap explanations

* **Filesystem I/O — ruled out.** The reported run is `--preload-archives`;
  reads use `ArchiveBacking::Memory`
  (`crates/engine/src/storage.rs:72,783-787`). My measurements also ran with
  preload ON. This says nothing about the *preload* cost itself (reading the
  633 MB archive at mount took ~5.4 s in my run, visible before `begin.tjs`),
  but that is startup, not the per-image load.
* **GPU upload — ruled out as the per-image cost.** The decode dominates in a
  `--headless --backend null` run with no GPU at all, so the stall is CPU-side.
  In the windowed run uploads are batched once per frame:
  `Host::sync_dirty_bitmaps` collects every dirty bitmap and issues a single
  `render::upload_textures` (`crates/app/src/lib.rs:570-644`,
  `crates/platform/render/src/lib.rs:360-373`), not one round-trip per image.
* **Transition cache — not involved.** The loads in `begin.tjs` /
  `game.changeScene` run while the scene object is constructed
  (`data.xp3>system/gamescenemanager.tjs:93-141`), before any transition; the
  transition-cache path is the separate topic of `docs/audits/perf-hotspots.md`.
* **Glyph rasterization — not involved.** `load_layer_image` never draws text;
  the text cache (`ResourceResolver::coverage_cache`, `resource.rs:42-45`) is
  not on this path.
* **No other name→bitmap cache exists.** `ResourceResolver`'s only caches are
  the text coverage cache and the pre-rendered-font map
  (`resource.rs:41-58`); `Scene` stores bitmaps by id
  (`scene.rs:1472-1494`), not by storage name.

---

## 7. Smallest honest fix per finding (no script-observable change)

1. **Repeated decode (Finding 2).** Implement the reference cache faithfully:
   a bounded `HashMap<(normalized_name, keyidx, mode, desw, desh), DecodedImage>`
   owned by L3, populated on a decode miss and consulted before decoding,
   enabled/cleared by `System.graphicCacheLimit`/`System.clearGraphicCache`
   (mirroring `GraphicsLoaderIntf.cpp:1878-1958,2423-2444`). This is the
   reference's exact policy and preserves the getter value. *Caveat:* because
   this game never sets the limit, this alone will not speed up this game; an
   always-on cache deviates from the reference default. If the goal is a warm
   cache by default, that is a deliberate policy change and should be recorded
   as such.
2. **Redundant RGBA clone (Finding 3).** One line: `image.into_rgba8()` in
   `decoded_from_dynamic` (`image_decode.rs:102-103`).
3. **TLG scratch churn (Finding 4).** Hoist the three per-block-row buffers out
   of `decode_tlg6`'s loop (`tlg.rs:360,387,708`) and reuse them; no output
   change.
4. **`graphicCacheLimit` no-op (Finding 5).** Fold into fix 1: the setter
   should establish/limit the cache, not just purge.
5. **Release churn (Finding 6).** No fix; the behaviour is correct. If the
   per-image release proves real after measurement, the cause is a script
   teardown pattern and belongs in the scene teardown path, not in the loader.

**Do not** push the cache into L2 or give L1 state (D9/D16); the decoded image
is L3's resource, and the reference keeps the cache at the graphics-loader
level, which maps to L3 here. **Do not** change any return value, error, or
`typeof`; the pixels and dimensions are already correct.

---

## 8. Method and environment

* No `cargo build/check/test/clippy/run`; subagent rule respected. I ran the
  already-built `target/debug/krkr` and `target/release/krkr`, and the
  repository's own `scripts/extract_xp3.py` (read-only) to inspect the game
  scripts and measure the images.
* Measurements used a temporary `startup.tjs` under `/tmp/auditgame` (never
  under the repo or the game directory) that loops `new Bitmap(name)` and
  reports `System.getTickCount()` deltas through `System.inform`. `new Bitmap`
  and `Layer.loadImages` share `read_image_from_storage` + `decode_image`
  (`crates/compat/src/natives/visual/bitmap.rs:218-240`), so the timing is the
  decode path, not a different codec.
* `target/release/krkr` is a pre-existing build (2026‑10‑12 12:13), not built
  for this audit. It is ~10–28× faster than debug and shows that even in
  release a few dozen 720p decodes are hundreds of ms.
* TLG sizes/dimensions were parsed from the extracted files; the archive
  entries are zlib-compressed (`segm` flag `m == 1`), so the "preloaded"
  reads still decompress per call (sub-millisecond for these sizes).

### Reproduce

```bash
cat > /tmp/auditgame/startup.tjs <<'EOF'
function bench(name, n){
  var t0 = System.getTickCount();
  for(var i=0;i<n;i++){ var b = new Bitmap(name); invalidate b; }
  System.inform("BENCH " + name + " n=" + n + " ms=" + (System.getTickCount()-t0));
}
bench("frame/frm_0501b.tlg", 10);
bench("frame/frm_0502a.tlg", 10);
bench("frame/frm_02c11.tlg", 30);
EOF
RUST_LOG=info target/debug/krkr --headless --frames 3 --preload-archives \
  --archive /mnt/DATA/Games/Others/test/data.xp3 --base-dir /tmp/auditgame
```

### Reference citations used

| Claim | Citation |
|---|---|
| `LoadImages` decodes into the layer's MainImage, no cache of its own | `reference/cpp/core/visual/LayerIntf.cpp:2823-2868` |
| Graphic cache key and default-off switch | `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1257-1264`, `:1433` |
| Cache hit on load | `GraphicsLoaderIntf.cpp:1878-1900` |
| Cache insert on decode | `GraphicsLoaderIntf.cpp:1937-1958` |
| Bound/evict and clear | `GraphicsLoaderIntf.cpp:1441-1470` |
| Enable via `System.graphicCacheLimit` | `GraphicsLoaderIntf.cpp:2423-2444`; `reference/cpp/core/base/SystemIntf.cpp:359-372` |
| Cache shares the decoded bitmap | `GraphicsLoaderIntf.cpp:1251`, `:1367-1385` |
| Async loader exists | `reference/cpp/core/visual/GraphicsLoadThread.cpp:89`; `reference/cpp/core/environ/Application.cpp:340` |
| Reference also decodes into a fresh bitmap then assigns | `GraphicsLoaderIntf.cpp:1689-1778`, `:980-1030` |

### Game-script citations used

| Claim | Citation |
|---|---|
| `IMAGELIST` shared Bitmaps | `data.xp3>system/configwindow.tjs:68-75,322-326` |
| `testButton:"FRM_02C11"` per voice row | `data.xp3>system/configwindow.tjs:285-294` |
| `create(item.testButton, 2)` builds a button per row | `data.xp3>system/configwindow.tjs:605-607,643-645,681-683` |
| `Button.create(string)` calls `loadImages` | `data.xp3>system/selectitem.tjs:310-318` |
| Scene change invalidates the old scene object | `data.xp3>system/gamescenemanager.tjs:78-100,158-162` |
| Config-window teardown invalidates its layers/bitmaps | `data.xp3>system/configwindow.tjs:88-130` |
