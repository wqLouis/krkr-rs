# krkr-rs — real-game image-load cost (`Layer.loadImages` → decode)

Audit only. **No source file was modified.** The reference (`reference/cpp/**`)
is the only oracle. Measured on the tree at `649abc6`; the numbers are debug and
release builds of that tree, not a benchmark in the repository.

Game under test: `$KRKR_TEST_GAME` (`data.xp3` + `patch.xp3`),
run with archive preload ON (`--preload-archives`), so **file I/O is out**:
reads come from the in-memory archive backing, not the FUSE mount.

---

## 0. Finding

Every image is decoded **synchronously on the main thread, inside the frame**.
`Layer.loadImages` → `load_layer_image`
(`crates/compat/src/natives/visual/layer/pixels.rs`) →
`load_cached_storage_image` → `read_image_from_storage`
(`crates/compat/src/natives/visual/bitmap.rs`) → the decode seam
(`crates/platform/media/src/{image_decode,tlg}.rs`). There is no async worker:
a scene change that loads N images pays the sum of N decodes, and the scene
cannot present until the last one returns.

Measured: a transition loads ~60 images serially (~32 ms apart, ≈1.9 s end to
end), and the worst observed frame was **6.77 s**, the same sum of synchronous
decodes.

The decoded-image cache does **not** remove this: the scene keeps one
(`Scene::{graphic_cache_limit, decoded_image_cache_limit}`), but it is disabled
by the reference default (`System.graphicCacheLimit = 0`) and this game never
sets it, so a warm cache is not the path this title takes. The lever is **scheduling**: overlap
independent decodes on a worker pool handing results back to the VM thread (the
sound decode-ahead pattern), plus a per-frame budget. That is `TODO.md` §1.2.

---

## 1. What a load does per call

1. `load_cached_storage_image` asks the scene cache for
   `(name, colorkey, mode, width, height)`; on a miss `read_image_from_storage`
   probes the name and a fixed extension list
   (`.webp`, `.bmp`, …, `.tlg`), then reads the winning candidate. With
   preload, the archive bytes are still **zlib-decompressed** per call; no disk
   is touched.
2. The decode seam: sniff/the `image` crate, or the native TLG5/TLG6 decoder.
   TLG is the common case for a real title.
3. Apply the colour key when the script passed one (`loadImages(name, key)`);
   with the one-argument form this is a no-op.
4. Reuse the layer's bitmap when the dimensions match, otherwise allocate a new
   RGBA buffer (`Scene::add_bitmap`/`resize_bitmap`) and move the decoded
   pixels in.

The decode step dominates; the read and the colour key are noise next to it.
A single 1280×403 TLG decoded in ~197 ms (debug) and a repeated 10× load took
**1.77 s → 0.20 s** with the cache enabled, which is the measured value of one
decode per repeat.

---

## 2. Where the reference caches it

`TVPLoadGraphic` consults `TVPGraphicCache` keyed by
`(storage name, key index, mode, width, height)`
(`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1257-1264`); a hit returns
from the cache (`:1886-1900`), a miss decodes and inserts (`:1937-1958`). The
cache is:

* **disabled by default** — `TVPGraphicCacheEnabled = false` (`:1433`), and
  `TVPSetGraphicCacheLimit(0)` keeps it disabled (`:2426-2428`);
* enabled only when a script sets `System.graphicCacheLimit` to a non-zero
  value;
* bounded and evicted by total bytes (`TVPCheckGraphicCacheLimit`, `:1441-1454`)
  and cleared on minimize (`TVPClearGraphicCache`, `:1456-1470`);
* populated only when `TVPAllocGraphicCacheOnHeap = false` (`:1251`), i.e. the
  cache and the layer's bitmap share one backing.

The game never calls `System.graphicCacheLimit`, `TouchImages` or
`clearGraphicCache`, so the reference would decode the same repeats too — the
cache is not the difference between the two engines on this title.

---

## 3. Game-side repeats

`Button.create` is given the **string** `"FRM_02C11"`, so it calls `loadImages`
once per button instead of reusing `_imageList`'s shared `Bitmap`
(`data.xp3>system/configwindow.tjs:605-607,643-645,681-683`;
`data.xp3>system/selectitem.tjs:310-318`). The same repeat happens in the
reference with the cache off; it is a script pattern, not a port defect.

---

## 4. What is not the cause

* **File I/O / preload.** With `--preload-archives` the reads come from memory;
  the stall is decode, not I/O. Without preload, FUSE/NTFS-3G latency adds on
  top (an environment effect, not the engine).
* **The transition cache.** The loads in `begin.tjs` / `game.changeScene` run
  while the scene object is constructed
  (`data.xp3>system/gamescenemanager.tjs:93-141`), before any transition.
* **Text rasterization.** `load_layer_image` never draws text; the text
  coverage cache is not on this path.
* **`released scene bitmap #N`.** That line is a previous scene/window layer's
  bitmap released by layer teardown (`Scene::remove_layer`/`destroy_layer`),
  not the just-loaded image; `load_layer_image` never releases a bitmap itself.

---

## 5. Method

* No `cargo build/check/test/clippy/run`; the already-built `target/debug/krkr`
  and `target/release/krkr` were run, and `krkr-gate extract-xp3` (read-only)
  inspected the game scripts and images.
* Measurements used a temporary `startup.tjs` under `/tmp/auditgame` (never
  under the repository or the game directory) that loops `new Bitmap(name)` and
  reports `System.getTickCount()` deltas through `System.inform`. `new Bitmap`
  and `Layer.loadImages` share `read_image_from_storage` + the decode seam, so
  the timing is the decode path.
* TLG sizes/dimensions were parsed from the extracted files; the archive entries
  are zlib-compressed, so a preloaded read still decompresses per call
  (sub-millisecond at these sizes).

Reference citations used: `GraphicsLoaderIntf.cpp:1251,1257-1264,1433,1441-1470,
1878-1958,2423-2444`; `reference/cpp/core/base/SystemIntf.cpp:359-372`;
`LayerIntf.cpp:2823-2868` (load into MainImage).
