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
cache (the reference's graphic cache).

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
| `LayerState` | the parent/child links, the focus domain, the rect, visibility, opacity, the generic `BlendMode`, the sibling sort key, the image placement, the province plane, the hit-test mode, the clip rect, the focus and hint properties, the cache slots (`cache_bitmap`, `cache_content`, `transition_self`, `transition_frame`, `cache_enabled_count`, `cache_recalc`) and the `pending_paint` / `shutdown` flags |
| `TransitionFrame` / `TransitionSource` | the frame a composited transition draws on a layer: its pixel size, whether it covers the layer rect, and the two source draws (`bitmap`, `src`, `dst`, `alpha`) |
| `BitmapState` | the RGBA8 pixels, the dimensions, the dirty flag, the script-ownership flag and the `content_generation` counter |
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

`add_window`, `window`, `window_mut`, `remove_window`.

`remove_window` destroys **every** layer of the window (through
`destroy_layer`), so the layers' owned bitmaps and fonts are released and no
sibling reference is left dangling.

### Layers

| group | methods |
|---|---|
| creation | `add_layer`, `add_detached_layer`, `attach_layer` |
| lookup | `layer`, `layer_mut`, `layer_mut_untracked` (crate-private), `is_primary_layer` |
| geometry | `set_layer_image_size`, `internal_set_image_size`, `layer_size_changed`, `ensure_layer_image`, `clipping_rect`, `absolute_offset`, `main_image_pitch` |
| ordering | `assign_front_order`, `sibling_position`, `order_index`, `absolute_order_index`, `set_absolute_order_mode`, `set_absolute_order_index`, `set_order_index`, `renumber_siblings`, `ordered_children`, `window_layer_order`, `layer_move_to_front` |
| lifetime | `remove_layer` (detach), `destroy_layer` (destroy the subtree), `invalidate_layer`, `invalidate_window` |
| focus | `set_focus`, `clear_focus`, `focused_in`, `set_domain_focus`, `window_input_root`, `next_focusable`, `prev_focusable`, `node_focusable` |
| visibility / hit-test | `node_visible`, `node_enabled`, `layer_at` |
| caches | `inc_cache_enabled_count`, `dec_cache_enabled_count`, `set_cache_bitmap`, `set_transition_self_image`, `set_transition_frame`, `update_layer` |
| pixels | `main_image_pixel_buffer`, `main_image_pixel_buffer_for_write`, `province_image_pixel_buffer`, `province_image_pixel_buffer_for_write`, `with_main_image_pixels`, `with_main_image_pixels_mut`, `with_province_pixels`, `with_province_pixels_mut`, `allocate_province_image`, `resize_province_image`, `deallocate_province_image`, `province_pixel`, `set_province_pixel` |

### Bitmaps

`add_bitmap`, `bitmap`, `bitmap_mut`, `bitmap_mut_with_source`,
`bitmap_mut_with_refs`, `resize_bitmap`, `replace_bitmap_image`,
`remove_bitmap`, `release_bitmap`, `release_script_bitmap`,
`collect_unused_bitmaps`, `bitmap_referenced_by_layer`, `set_bitmap_clean`.

### Fonts

`add_font`, `reset_font`, `font`.

### The decoded-image cache

`cached_decoded_image`, `graphic_cache_limit`, `decoded_image_cache_limit`,
`set_graphic_cache_limit`, `set_decoded_image_cache_limit`,
`clear_decoded_image_cache`.

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
  consumer (`node_visible`, `layer_at`, `window_layer_order`) skips it.
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
* **A script-owned bitmap outlives the layer that shows it.** `script_owned`
  marks a bitmap a script `Bitmap` object owns; destroying the layer must not
  free it.
* **`set_bitmap_clean` does not move the structure revision.** Clearing a
  renderer-internal dirty flag is not a scene change.
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
  next resize of that image.
* **The transition self image stands in for the main image** in a cached
  subtree, the reference's `CopySelfForRect`
  (`reference/cpp/core/visual/LayerIntf.cpp:6781-6783`).

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
   ├──► engine::Engine::sync_dirty_bitmaps  (reads `dirty`, uploads `rgba`)
   └──► layer_ops / raster (write pixels through BitmapState, then mark_dirty)
```

`BitmapState::mark_dirty` sets the dirty flag and bumps
`content_generation`, which the subtree-cache bookkeeping sums to detect an
image edit that did not go through a layer accessor.

---

## 5. Failure modes

* An operation on an unknown id returns `None`/`false` or is a no-op; it never
  panics and never creates a record implicitly.
* `set_order_index` and `set_absolute_order_index` return `Result<(), String>`
  when the requested order cannot be represented.
* The image-size operations (`set_layer_image_size`,
  `internal_set_image_size`) return `false` when the requested size cannot be
  represented or allocated; L3 turns that into a script error.
* `cached_decoded_image` returns the decoder's own error unchanged.
* A `*_mut` accessor over-approximates: it bumps the revision even when the
  caller writes the same value. That only costs a resync.
* The hit-test and focus walks are bounded by the layer count, so a cyclic
  parent chain is a typed miss, not a hang.

---

## 6. Current limits

* **`BitmapState` carries no dirty region**, so a full-frame upload cannot be
  narrowed without a cross-cutting change.
* **The decoded-image cache is consulted on every load but is disabled by the
  reference default** (`System.graphicCacheLimit = 0`), and the decode itself
  is synchronous on the VM thread.
* **`Layer.face` is not read** by the steady-state projection: a
  `dfMask`/`dfProvince` layer is faithful only during a transition.
* **Masks and province images are not emitted** by the projection; `hold_alpha`
  travels but is not interpreted.
* **The hit-test is flattened across the focus domains.** `layer_at` walks
  `window_layer_order` for the whole window, where the reference binds input
  to the window's input domain (the first registered root's);
  `Scene::window_input_root` names that root, and the dispatch does not use
  it yet.

---

## 7. Tests

* `crates/engine/tests/scene.rs` — the tree: creation, ordering, focus
  domains, hit-testing, cache counts, province images.
* `crates/engine/tests/bitmap.rs` — the bitmap lifetime and the pixel
  buffers.
* `crates/engine/tests/oop_semantics.rs` — the value semantics of the state
  types.
* `crates/engine/src/scene.rs` `mod tests` — the unit-level invariants.

---

## 8. See also

| topic | document |
|---|---|
| the logical tree in the architecture | [`arch/l2.md`](../../arch/l2.md) §2 |
| the L3 ↔ L2 protocol for this module | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the projection that reads this tree | [projection.md](projection.md) |
| the pixel operations that write into it | [layer_ops.md](layer_ops.md) |
| the frame that owns it | [engine.md](engine.md) |
| the archived protocol trail | [`docs.old/migration/14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) §6.1 |
