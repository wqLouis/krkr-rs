# `engine::scene` — the logical layer tree

Source: `crates/engine/src/scene.rs`

`Scene` is the whole logical scene: the windows, the layers, the bitmaps and
the fonts, with every property a script can set on them. It is L2 state, and
it is **generic**: no `LT_*` blend numbering, no `arc.xp3>` naming, no KAG
tags. L3 writes the TVP-shaped properties onto these values; the projection
reads them back.

**Responsibility.** Hold the logical tree and the CPU pixels behind it;
maintain the id → index lookup tables, the sibling ordering, the focus domains
and the subtree-cache bookkeeping; keep two monotonic revisions so a consumer
can tell a structural change from a content change; own the decoded-image
cache (the reference's graphic cache) and the pending images — the bitmaps
whose pixels are still being decoded — and the pre-loads, the decodes that
will fill the cache when they land.

The module is backend-free: it knows nothing about rendering, script objects
or native registration. The field-level description of the tree and the draw
order are in [`arch/l2.md`](../../arch/l2.md) §2; this page is the code map.

---

## 1. State it owns

```rust
pub struct Scene {
    pub windows: Vec<WindowState>,
    pub layers: Vec<LayerState>,
    pub bitmaps: Vec<BitmapState>,
    pub fonts: Vec<FontState>,
    // private:
    next_window / next_layer / next_bitmap / next_font: u32,
    window_index / layer_index / bitmap_index / font_index: Map<u32, usize>,
    image_cache: DecodedImageCache,
    preloads: Vec<PreloadDecode>,
    structure_revision: u64,
    content_revision: u64,
}
```

The four public vectors may be edited directly by a caller; the id lookups
then fall back to a linear scan and still return the correct result.

`Scene::composite_transition_frames` is the one host-selected field: it says
whether a running transition draws from `LayerState::transition_frame` (the
projection composites it) or is blended into the layer's display bitmap on the
CPU. It defaults to the CPU path, which is what a bare `Scene` (a test, the
headless host) gets; `Engine::new` turns it on for a backend that draws the
projected scene.

| type | owns |
|---|---|
| `WindowState` | the client size, visibility, opacity, the primary layer, the focus domains and their focus, and the window's layer list |
| `LayerState` | the parent/child links, the focus domain, the rect, visibility, opacity, the generic `BlendMode`, the sibling sort key, the image placement, the province plane, the hit-test mode, the clip rect, the focus and hint properties, the cache slots (`cache_bitmap`, `cache_content`, `transition_self`, `transition_frame`, `cache_enabled_count`, `cache_recalc`), the content bookkeeping (`content_epoch`, `cache_content_signature`) and the `pending_paint` / `shutdown` flags |
| `TransitionFrame` / `TransitionSource` | the frame a composited transition draws on a layer: its pixel size, whether it covers the layer rect, and the two source draws (`bitmap`, `src`, `dst`, `alpha`) |
| `BitmapState` | the RGBA8 pixels, the dimensions, the dirty flag and dirty region, the script-ownership flag, the `content_generation` counter and the pending decode (`pending`) |
| `FontState` | the requested face, height, colour and the style flags |
| `Rect` | a window-coordinate rectangle (`x`, `y`, `w`, `h`) |
| `DecodedImageKey` / `DecodedImageMode` | the cache key of one decoded image and its load variant (`Full` / `Province`) |

---

## 2. Public API

### Revisions

```rust
pub fn structure_revision(&self) -> u64;
pub fn content_revision(&self) -> u64;
```

`structure_revision` moves on every change the projection reads: window/layer
creation and removal, sibling order, parenting, the geometry and draw
properties, and bitmap creation, removal and resize. `content_revision` moves
on every bitmap content change, including a resize. A bitmap content write
does **not** move the structure revision, so an animated frame paints without
re-projecting.

### Windows

`add_window`, `window`, `window_mut`, `remove_window`,
`bring_window_to_front`.

`remove_window` drops **only** the window record. Its layers keep their
records: a layer's native state belongs to the layer's **script object**, so
it is released by that object's own `invalidate_layer`, not by the window.
The surviving records are inert — a missing window has an empty
`window_layer_order`, so they are not drawn, hit-tested or focused — and their
bitmaps and fonts are released with them.

### Layers

| group | methods |
|---|---|
| creation | `add_layer`, `add_detached_layer`, `attach_layer` |
| lookup | `layer`, `layer_mut`, `layer_mut_untracked` (crate-private), `is_primary_layer` |
| geometry | `set_layer_image_size`, `internal_set_image_size`, `layer_size_changed`, `ensure_layer_image`, `clipping_rect`, `absolute_offset`, `main_image_pitch` |
| ordering | `assign_front_order`, `sibling_position`, `order_index`, `absolute_order_index`, `set_absolute_order_mode`, `set_absolute_order_index`, `set_order_index`, `renumber_siblings`, `ordered_children`, `window_layer_order`, `layer_move_to_front` |
| lifetime | `remove_layer` (detach), `destroy_layer` (destroy the subtree), `invalidate_layer` |
| focus | `set_focus`, `clear_focus`, `focused_in`, `set_domain_focus`, `window_input_root`, `next_focusable`, `prev_focusable`, `node_focusable` |
| visibility / hit-test | `node_visible`, `node_enabled`, `layer_hit_next`, `layer_hit_outcome` |
| caches | `inc_cache_enabled_count`, `dec_cache_enabled_count`, `set_cache_bitmap`, `set_transition_self_image`, `set_transition_frame`, `update_layer` |
| pixels | `main_image_pixel_buffer`, `main_image_pixel_buffer_for_write`, `province_image_pixel_buffer`, `province_image_pixel_buffer_for_write`, `with_main_image_pixels`, `with_main_image_pixels_mut`, `with_province_pixels`, `with_province_pixels_mut`, `allocate_province_image`, `resize_province_image`, `deallocate_province_image`, `province_pixel`, `set_province_pixel` |

### Bitmaps

`add_bitmap`, `bitmap`, `bitmap_mut`, `bitmap_mut_with_source`,
`bitmap_mut_with_refs`, `with_two_bitmaps`, `resize_bitmap`,
`replace_bitmap_image`, `remove_bitmap`, `release_bitmap`,
`release_script_bitmap`, `collect_unused_bitmaps`, `bitmap_referenced_by_layer`,
`set_bitmap_clean`, `mark_bitmap_whole`, `dirty_bitmaps_for_upload`,
`take_removed_bitmaps`.

### The dirty set

The scene keeps the bitmaps whose pixels may have changed, so the frame never
walks the bitmap table to find the work:

| method | what it does |
|---|---|
| `dirty_bitmaps_for_upload()` | the bitmaps whose pixels changed since the last upload, in id order. The set holds every bitmap the scene handed out for mutation — a mutable access is a possible content change — and the ones whose pixels did not change leave it here. |
| `take_removed_bitmaps()` | the bitmaps removed since the last prune, in id order; the set is empty afterwards. The engine drops the texture of each of them. |

A bitmap joins the set when the scene hands it out for mutation
(`bitmap_mut`, `bitmap_mut_untracked`, `bitmap_mut_with_source`,
`bitmap_mut_with_refs`, `with_two_bitmaps`), when it is created, and when its
decode is installed. It leaves the set when it is uploaded
(`set_bitmap_clean`) or removed.

### The pending image

A bitmap whose pixels are still being decoded carries the decode future in
`BitmapState::pending`; the scene keeps the set of the bitmaps that carry one,
so the frame waits for those and not for the whole table.

| method | what it does |
|---|---|
| `add_pending_bitmap(width, height, pending)` | adds a bitmap with the probed size and no pixels, and returns its id. `ImageSizeError::TooLarge` when the pixel count is not representable. |
| `set_pending_image(id, width, height, pending)` | puts a pending decode on an existing bitmap and cancels the one it carried. |
| `pending_image(id)` | the bitmap's pending future, or `None` when it holds its pixels. |
| `pending_decode_for(key)` | the decode already running for the key: the probed size and a clone of the shared future, or `None` when neither a bitmap nor a pre-load carries that decode. |
| `resolve_bitmap(id)` | waits for one bitmap's pending image and installs its pixels; a successful decode is also stored in the decoded-image cache under the key the load named. |
| `resolve_pending_images()` | waits for every pending image and installs the pixels, and takes the landed pre-loads into the decoded-image cache; returns how many bitmaps it resolved. |
| `has_pending_images()` | whether any bitmap waits for a decode. |

`resolve_bitmap` **waits on that image alone**, never on the pool. It marks
the image *demanded* first, so a worker takes it before the ordinary jobs in
the queue. The outcome decides what lands:

| outcome | what is installed |
|---|---|
| the decoded pixels | the pixels, when their length matches the probed size; otherwise a blank, and the mismatch is logged |
| a failure | a fully transparent blank of the probed size, and the failure is logged |
| a cancellation | a fully transparent blank of the probed size, and the cancellation is logged |

None of the three raises: the load that queued the decode already returned.

### The pre-load

A pre-load is a decode that was queued for the decoded-image cache without a
bitmap to hold it: the caller queues the decode and continues, and the pixels
reach the cache later, when they land.

| method | what it does |
|---|---|
| `add_preload_decode(key, width, height, pending)` | registers a decode that fills the cache entry `key`; a pre-load under a key the scene already holds is replaced. |
| `harvest_preload_decodes()` | takes the pixels of the pre-loads whose decode has landed into the decoded-image cache; returns how many it took. |
| `pending_decode_for(key)` | a pre-load holds a decode for its cache entry too, so a load of an image whose pre-load is still running shares that decode. |

The harvest **waits on no decode**: a pre-load that is still running stays
registered and a later frame takes its pixels. A decode that failed or was
cancelled is dropped and logged; a decode that produced an image that does
not match the probed size caches nothing.

### Fonts

`add_font`, `reset_font`, `font`.

### The decoded-image cache

`cached_decoded_image`, `lookup_decoded_image`, `touch_decoded_image`,
`graphic_cache_limit`, `decoded_image_cache_limit`, `set_graphic_cache_limit`,
`set_decoded_image_cache_limit`, `clear_decoded_image_cache`.

`lookup_decoded_image` is the cache lookup a caller that may **defer** the
decode uses: it looks the entry up, marks it most recently used and returns
an **owned copy** of the pixels, exactly as `cached_decoded_image` does on a
hit. The caller decodes only on a miss.

`touch_decoded_image` marks an entry most recently used and reports whether
the cache holds it, **without copying the pixels**: a caller that only wants
the entry's recency — a pre-load that touches the graphics it already holds
— pays nothing for the touch.

---

## 3. Invariants

* **Ids are stable** for the lifetime of the record; an operation on an
  unknown id is a no-op or a `None`, never a panic.
* **`window_layer_order(window)` is parent-first** — a parent's complete
  subtree is emitted before the next sibling — which is what lets the
  projection compose ancestors in a single forward pass. Shutdown layers are
  excluded from the walk.
* **A detached layer is not drawn.** `remove_layer` (the reference's `Part()`,
  `reference/cpp/core/visual/LayerIntf.cpp:624`) severs the layer from its
  parent and from the window's root list and drops its record, releasing its
  MainImage, its cache slots, its transition self image and its font — each
  bitmap only when no surviving layer and no script `Bitmap` object still
  holds it. The direct children are detached too (their parent becomes `None`)
  but **keep their records** until their own script objects are finalized
  (`reference/cpp/core/visual/LayerIntf.cpp:535-549`); they are not promoted
  to window-level layers, so a parted subtree stops being drawn and
  hit-tested until it is re-parented.
* **A destroyed layer takes its subtree with it.** `destroy_layer` collects
  the subtree through both the `parent` links and the maintained `children`
  vectors, so a stale link cannot leave an orphan.
* **A shut-down layer is inert.** `invalidate_layer` marks it before
  unregistering it (`reference/cpp/core/visual/LayerIntf.cpp:515`); every
  consumer (`node_visible`, `layer_hit_next`, `window_layer_order`) skips it.
* **The focus domain is fixed at creation.** A parentless layer founds its own
  domain (its own id); a child inherits its parent's, and the reference
  assigns the manager once and never reassigns it
  (`reference/cpp/core/visual/LayerIntf.cpp:449-473`). The window binds to
  the **first** registered root's domain — the reference's
  `PrimaryLayerManagerIndex` (`reference/cpp/core/visual/LayerTreeOwnerImpl.h:21`),
  pushed in creation order by `RegisterLayerManager`
  (`reference/cpp/core/visual/LayerTreeOwnerImpl.cpp:93-98`) — and
  `focused_layer` mirrors that domain's focus, so it cannot disagree with
  `domain_focus` (the reference's per-manager `FocusedLayer`,
  `reference/cpp/core/visual/LayerManager.h:483-490`).
* **The province plane is layer-owned**, not shared through a bitmap: it is
  an 8-bit plane allocated on demand
  (`reference/cpp/core/visual/LayerIntf.cpp:2359`, `AllocateProvinceImage`)
  and freed with the layer.
* **The hit test and the province reads are total.** A province plane shorter
  than the size it declares reads as `0`, a province write into it writes
  nothing, and a hit test over a short province plane — or over a main image
  that declares more pixels than its buffer holds — is a miss. No read runs
  past the buffer, and each inconsistency is reported in the log.
* **A script-owned bitmap outlives the layer that shows it.** `script_owned`
  marks a bitmap a script `Bitmap` object owns; destroying the layer must not
  free it.
* **A pending bitmap carries the probed size and no pixels.** The size comes
  from the header probe, so it is known before the decode finishes; the pixels
  arrive when the scene resolves the image, and every read of the pixels waits
  for that resolution.
* **A pending decode is cancelled when it is superseded.** Replacing the
  bitmap's image, giving it a new pending decode or removing it cancels the
  decode it carried; a size change resolves it first, so the decoded overlap
  is what the resize preserves and a waiter sees the cancellation instead of
  stale pixels. A decode another bitmap still waits for is left alone, and
  the same decode installed again is not a new one.
* **A failed decode installs a blank, not an exception.** The bitmap keeps the
  probed size and every pixel becomes transparent; the failure is logged. The
  load that queued the decode already returned, so there is no call to fail.
* **`set_bitmap_clean` does not move the structure revision.** Clearing a
  renderer-internal dirty flag is not a scene change.
* **The frame never walks the bitmap table.** The scene keeps the bitmaps
  whose pixels may have changed, the bitmaps that wait for a decode and the
  bitmaps that were removed; the upload pass, the image barrier and the
  texture prune read those sets, so their cost follows the work of the frame
  and not the size of the scene.
* **The decoded-image cache is disabled by default**, matching the reference's
  default (`reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1433-1434`). The
  limit counts decoded bytes (`Width × Height × PixelSize`,
  `reference/cpp/core/visual/GraphicsLoaderIntf.cpp:1305-1310`) and evicts
  least-recently-used entries, with the recency order the reference's
  additional order keeps (`reference/cpp/core/tjs2/tjsHashSearch.h:513-527`).
  A caller receives an **owned copy**, so mutating the returned pixels cannot
  corrupt a later hit.
* **Pixel-buffer addresses are opaque tokens.** `main_image_pixel_buffer`
  and its province counterpart report the address of the heap buffer the
  reference reports
  (`reference/cpp/core/visual/LayerIntf.cpp:3005-3044`); the safe
  `with_*_pixels` closures are the access path. The address is valid until the
  next resize of that image. The main-image address is `0` when the layer has
  no main image **and** when the image declares more pixels than its pixel
  buffer holds: an image with no pixels has no address to hand out, and the
  write variant does not mark it dirty.
* **The transition self image stands in for the main image** in a cached
  subtree, the reference's `CopySelfForRect`
  (`reference/cpp/core/visual/LayerIntf.cpp:6781-6783`).
* **A running transition frame or effect leaves the endpoint subtrees
  reusable.** A frame update marks only the layer's caching **ancestors**
  stale; the layer's own composited content is one of the frame's sources and
  does not change. Clearing the frame invalidates the layer itself, because
  it resumes drawing its own subtree.

---

## 4. Data flow

```
L3 native (script call)
   │  marshalled to an engine id
   ▼
Scene::add_layer / update_layer / layer_mut / bitmap_mut …
   │  bumps structure_revision or content_revision
   ▼
Scene  ──► projection::project_scene_report ──► RenderScene (VM pixels)
   │                    (reads rect, blend, image placement, cache slots)
   │
   ├──► engine::Engine::sync_dirty_bitmaps  (reads `dirty_region`, uploads the region)
   └──► layer_ops / raster (write pixels through BitmapState, then mark_dirty[_rect])
```

`BitmapState::mark_dirty` sets the whole bitmap dirty; `mark_dirty_rect`
widens a partial region. Both bump `content_generation`, which the
subtree-cache bookkeeping sums to detect an image edit that did not go through
a layer accessor. A full mark (`dirty_region == None`) overrides a partial one.

### The cache's validity

A layer's composited subtree content is valid while the **content signature**
of the subtree still matches the one the content was composited with. The
signature is the sum of the drawn sources' own contributions
(`LayerState::content_epoch`) and of their images' `content_generation`, and
it is recorded in `LayerState::cache_content_signature`. One comparison
therefore decides whether the content is still the content the tree
describes: a source that changed through the layer accessors advanced its
epoch, and a source whose pixels changed behind them advanced its image's
generation.

The counter is the engine's record of *that* the content changed; the
reference records *where*, in the layer's `CacheRecalcRegion` — a complex
rectangle of the part of the cache that must be reconstructed
(`reference/krkr2/cpp/core/visual/LayerIntf.h:963`). The reference fills it
with the rectangles that changed
(`reference/krkr2/cpp/core/visual/LayerIntf.cpp:6254`, `:6285`, `:6329`) and
`Complete` returns the cached bitmap untouched while it is empty
(`reference/krkr2/cpp/core/visual/LayerIntf.cpp:7734-7740`).

The transition's tick is the case the two shapes answer the same way: the
transition changes its own frame and no source's pixels, so it appends to no
region (`reference/krkr2/cpp/core/visual/LayerIntf.cpp:6322-6327`) and
advances no epoch, and the transition's endpoints keep their composites for
the whole run.

### The pending-image path

A load whose decode the L2 pool took installs a **pending** bitmap: the
probed size, no pixels, and the decode future in `BitmapState::pending`.

```
L3 native (Layer.loadImages / Bitmap.load)
   │  read the bytes, probe the header      ── on the calling thread
   ▼
DecodePool::submit ──► pending image (the future)
   │
   ▼
Scene::add_pending_bitmap / set_pending_image   (probed size, no pixels)
   │
   ├──► Scene::resolve_bitmap        ── a pixel read waits for the future
   └──► Scene::resolve_pending_images ── the frame barrier waits for them all
   │
   ▼
the decoded pixels land in the bitmap  (or a transparent blank, on a failure)
```

A load of an image whose decode is already running shares that decode: the
scene finds it with `pending_decode_for`, and every holder carries a clone of
the same future. The pixels land in all of them, and the decode stays alive
as long as one of them waits for it.

### The pool's byte limit

The pool bounds the bytes the queued and the in-flight decodes hold: a job is
charged the size of its bytes plus the size of the pixels it will produce, so
the limit bounds the pool's footprint. The default is **256 MiB**
(`engine::jobpool::DEFAULT_MEMORY_LIMIT`).

A full-resolution character part is a 1445×3304 WebP that charges the pool
about 19.6 MB — the compressed bytes plus the 19 112 720 bytes of RGBA pixels
it decodes into — so the default holds the four character parts of an avatar
switch at once. A job that does not fit is refused with
`JobPoolError::MemoryLimit`, and the load decodes synchronously on the
calling thread instead.

---

## 5. Failure modes

* An operation on an unknown id returns `None`/`false` or is a no-op; it never
  panics and never creates a record implicitly.
* `main_image_pixel_buffer` and `main_image_pixel_buffer_for_write` report
  `0` when the layer has no main image and when the image declares more pixels
  than its pixel buffer holds; the write variant marks no such image dirty.
* `province_pixel` reads `0` and `set_province_pixel` writes nothing when the
  province plane is shorter than the size it declares; the inconsistency is
  logged.
* `layer_hit_next` answers a miss when the province plane is short or when the main
  image declares more pixels than its buffer holds.
* `resize_bitmap` preserves only the whole rows the source buffer holds; the
  rest of the overlap keeps the fill, and the rows the source does not hold
  are reported in the log.
* `set_order_index` and `set_absolute_order_index` return `Result<(), String>`
  when the requested order cannot be represented.
* The image-size operations (`set_layer_image_size`,
  `internal_set_image_size`) return `false` when the requested size cannot be
  represented or allocated; L3 turns that into a script error.
* `cached_decoded_image` returns the decoder's own error unchanged.
* `add_pending_bitmap` returns `ImageSizeError::TooLarge` when the probed
  size's pixel count is not representable; the caller then decodes
  synchronously and uses `add_bitmap`.
* A pending decode that failed, was cancelled or produced a size that
  disagrees with the probe installs a transparent blank and logs the cause.
  It never raises and never leaves a bitmap without pixels.
* A `*_mut` accessor over-approximates: it bumps the revision even when the
  caller writes the same value. That only costs a resync.
* The hit-test and focus walks are bounded by the layer count, so a cyclic
  parent chain is a typed miss, not a hang.

---

## 6. Current limits

* **The dirty region is a conservative bounding rectangle.** A partial mark
  widens to the union of the rectangles, and only the affine,
  `operate_rect` and `stretch_blit_mode` writes use partial marks so far; the
  other primitive families still mark the whole bitmap. A new-texture upload
  is always whole.
* **The decoded-image cache is consulted on every load but is disabled by the
  reference default** (`System.graphicCacheLimit = 0`).
* **A deferred decode enters the decoded-image cache.** The synchronous
  path and the pending-image path store through the same insert, under the
  same key, with the same LRU and byte accounting. A failed, cancelled or
  malformed decode stores nothing.
* **The frame waits for the images it loaded.** The image barrier blocks the
  frame until every pending decode has landed, so the decodes overlap with
  each other but not with the frame that waits for them.
* **`Layer.face` is not read** by the steady-state projection: a
  `dfMask`/`dfProvince` layer is faithful only during a transition.
* **Masks and province images are not emitted** by the projection; `hold_alpha`
  travels but is not interpreted.
* **The hit-test is flattened across the focus domains.** `layer_hit_next` walks
  `window_layer_order` for the whole window, where the reference binds input
  to the window's input domain (the first registered root's);
  `Scene::window_input_root` names that root, and the dispatch does not use
  it yet.

---

## 7. Tests

* `crates/engine/tests/scene.rs` — the tree: creation, ordering, focus
  domains, hit-testing, cache counts, province images; a resize keeps only
  the rows the source buffer holds, and a layer image that declares more
  pixels than its buffer holds has no main-image address.
* `crates/engine/tests/bitmap.rs` — the bitmap lifetime and the pixel
  buffers.
* `crates/engine/tests/imagedecode.rs` — the decode pool and the job pool
  under it: the workers, the queue, the byte accounting, the demanded-job
  order, the outcomes a waiter sees and the cast of a job's value.
* `crates/engine/tests/oop_semantics.rs` — the value semantics of the state
  types.
* `crates/engine/src/scene.rs` `mod tests` — the unit-level invariants.
* `crates/compat/tests/pending_image.rs` — the pending image through the
  real natives: a load without a pool decodes synchronously, a load with a
  pool leaves the bitmap pending, the first pixel read waits for the decode,
  the image size is known before the pixels land, a probe failure raises at
  the load and a refused submit falls back to the synchronous decode.

---

## 8. See also

| topic | document |
|---|---|
| the logical tree in the architecture | [`arch/l2.md`](../../arch/l2.md) §2 |
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the projection that reads this tree | [projection.md](projection.md) |
| the pixel operations that write into it | [layer_ops.md](layer_ops.md) |
| the frame that owns it | [engine.md](engine.md) |
| the frame that waits for the pending images | [`arch/l2.md`](../../arch/l2.md) §9 |
| the archived protocol trail | [`docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) §6.1 |
