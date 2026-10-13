# Review — `engine` and `app`

Scope: `crates/engine/` (layer system state, `layer_ops`, `raster`,
`projection`, `storage`) and `crates/app/` (host assembly + CLI). Reviewed
against:

- `docs/migration/02-target-architecture.md` (D9/D10, Invariants A/B/C)
- `docs/migration/03-middle-layer-api.md` (the L2/L1 contract)
- the sibling new crates `api`, `fs`, `media`

Method: read-only. No build/test/cargo was run.

Severity legend: **Critical** (breaks an invariant / exploitable) · **High**
(likely bug or clear design break) · **Medium** (correctness or cost risk) ·
**Low** (quality) · **Nit** (style).

---

## 0. Verdict

The crates are in good shape: `#![forbid(unsafe_code)]` and
`#![warn(missing_docs)]` are set, there are **no globals, statics,
`LazyLock`/`OnceLock`, or `thread_local`** anywhere in either crate, the L2/L1
split (state in `Scene`/`Storage`, actions on `FileSystem`/`RenderPipeline`
values) is respected, and the code is unusually well
documented with reference citations.

> **Resolution status (current tree).**
> * §1.1 — D33 and `02-target-architecture.md` §12.1 now state that `engine`
>   depends on the L1 *type* crates (`api`, `fs`, `render`); the earlier `02`
>   §4 wording is fixed. `flate2` is used by the FS-side XP3 reader, not by
>   `engine`.
> * §1.2 — there is no `krkr-core`; `app` and `crates/android` are the two
>   composition roots.
> * §1.3/§1.4/§2.4/§4.2/§4.3/§4.7 — the XP3 parser now lives in
>   `crates/platform/fs/src/xp3.rs` with `Xp3Error`, checked arithmetic and
>   typed `CorruptIndex`; see `reviews/fs_engine.md` resolution status.
> * §3.1/§4.1/§4.4 — `blur_bitmap_in_place` is a separable sliding running-sum
>   box blur with `u64` accumulators, and the coordinate arithmetic uses
>   `saturating_*` throughout `layer_ops`/`projection`/`raster`.
> * §3.3 — `Host::step` keeps a persistent `RenderScene`, reuses its arena and
>   skips reprojection while `Scene::revision()` is unchanged
>   (`crates/app/src/lib.rs`).
>
> The remaining perf/consistency findings (§2.1, §2.3, §3.2, §3.4–3.9, §4.5,
> §4.6, §4.8, §4.9, §4.10) have not all been re-checked; read them as a
> snapshot, not as current defects.

---

## 1. Consistency

### 1.1 Dependency graph contradicts `02-target-architecture.md` §4 — High

`02` §4 states `engine` "depends on `api` only". The actual
`crates/engine/Cargo.toml` depends on `api`, **`fs`**,
**`render`** and **`flate2`**; `projection.rs:32` imports
`render::{RenderContent, RenderNode, RenderScene, Texture}` and
`storage.rs:31` imports `fs::{FileState, FileSystem}`.

There is a real tension in the design: `03-middle-layer-api.md` §2.2 defines
`RenderScene`/`RenderState`/`Surface` in L1 (`render`), which forces L2 to
depend on the L1 crate. The crate docs (`lib.rs:8-13`) call out only the
`fs` exception, not `render`/`flate2`.

Recommendation: pick one and make it consistent — either move the
render tree/state/DTO types into `api` so `engine` truly depends on
`api` only, or amend `02` §4 to state that L2 depends on the L1 *type*
crates (`fs`, `render`) but not on their backends. `flate2` in L2
(used by `archive/xp3.rs:33`) is also a concrete codec dependency that
Invariant B's spirit discourages; at minimum document it.

### 1.2 No `krkr-core`; `app` assembles L1 directly — Medium

Acceptance criterion 1 (`02` §9) and criterion 5 reference a `krkr-core`
facade, and the `app` doc (`lib.rs:1-22`) describes itself as L4. The
crate instead depends on `render`/`fs`/`media` directly
(`app/Cargo.toml`). This is workable for now but means the desktop and
future Android hosts each wire L1 themselves. Flagging so the roadmap's
"krkr-core" step is not silently dropped.

### 1.3 Error-type naming diverges from the sibling crates — Low

`api` names domain errors `FsError`, `MediaError`, `RenderError`,
`GlyphError`, `AudioError`. In `engine` the archive parser exports a bare
`Error` with a module-local `Result<T>` alias (`archive/xp3.rs:64-74`), while
the same crate defines `MountError`/`ReadError` (`storage.rs:98-116`).
Recommend `Xp3Error` (or `ArchiveError`) to match, and `ReadError` →
`StorageReadError` for clarity at call sites.

### 1.4 Typed FS errors are erased through the `Read`/`Seek` adapter — Medium

`FileReader` converts every `FsError` to `io::Error::other`
(`archive/xp3.rs:444-464`), and `Error::Io(#[from] std::io::Error)` then
re-wraps it as an opaque string. This conflicts with `api/error.rs`'s
"owned, backend-agnostic, matchable" model and loses `FsError::NotFound` etc.
Add `Error::Fs(#[from] FsError)` and let `FileReader` carry the original
`FsError` (e.g. a `last_error` slot or a custom `Read` error enum).

### 1.5 `LT_*` constants split across two modules — Low

`LT_OPAQUE..LT_SUBTRACTIVE` live in `scene.rs:45-53`; `LT_BINDER..LT_PS_EXCLUSION`
live in `projection.rs:47-104`; `lib.rs:47-59` re-exports both. The split is
explained in a comment but is easy to miss. Move all `tTVPLayerType` values
into one module (e.g. `layer_type.rs`) and re-export.

### 1.6 `blend_pixel` location — Low

`raster::blend_pixel` (`raster.rs:24`) is the shared compositing primitive
used across `raster.rs` and `layer_ops.rs:30`. It is not a rasterizer concept;
a shared `pixel` module (or `layer_ops`) would better express ownership. Minor.

### 1.7 Doc-comment style is consistent and good, with two stale spots — Low

Module/function docs and reference citations are consistently excellent. Two
inaccuracies:

- `Scene::set_focus` doc says "A `None`/invalid `id` blurs the current
  holder" (`scene.rs:946`) but the signature takes `u32` and an invalid id
  returns `(None, None)` **without clearing focus** (`scene.rs:950-952`).
- `Scene::window_mut` says "bumping revision" (`scene.rs:309`) but `touch()`
  runs before the lookup, so a miss also bumps (`scene.rs:310-311`). Harmless
  but the docs imply otherwise.

### 1.8 Oversized modules — Low

`layer_ops.rs` is 2104 lines mixing constant-color fills, tone ops, blits,
and resampling; `scene.rs` is 1549 lines mixing
the model, tree traversal/sorting and the bitmap cache. Consider splitting
along those seams (`layer_ops/{blit,tone,resample}.rs`, `scene/{model,tree}.rs`).

### 1.9 `app` matches the sibling host style — OK

`HostConfig` builder methods, `StepReport`, typed `HostError`, and the
hand-rolled `Cli::parse` (`main.rs:64-95`) are consistent with the no-extra-deps
tone of the other crates. No change needed.

---

## 2. Idiomatic Rust (API Guidelines / standard practice)

### 2.1 `#[allow(clippy::too_many_arguments)]` on eight hot functions — Medium

`layer_ops.rs:502, 642, 763, 796, 824, 1693, 1729, 1782` take 8–10 positional
arguments (`blit_over`, `tile_rect`, `do_drop_shadow`, `do_blur_light`,
`fill_operate_rect`, `operate_rect`, `stretch_blit_mode`, `affine_blit`).
Bundling the geometry into a small `BlitParams`/`StretchParams` struct would
remove the lint suppressions and the easy argument-order mistakes.

### 2.2 Redundant `checked_div` after an explicit zero check — Nit

`convert_add_alpha_to_alpha` (`layer_ops.rs:1405-1415`) already returns early
for `a == 0`, then uses `checked_div(a).unwrap_or(0)`. Plain `/ a` is fine.

### 2.3 `BitmapState: Clone` invites full-buffer copies — Medium

`BitmapState` derives `Clone` (`scene.rs:180`), and `do_drop_shadow`/
`do_blur_light` rely on it to make 2–3 whole-image copies
(`layer_ops.rs:775, 777, 785, 807`). Deriving `Clone` on a multi-megabyte
pixel buffer makes accidental copies cheap to write and expensive to run.
Prefer removing the derive and exposing an explicit `copy_pixels()` / scratch
buffer API, so every full copy is deliberate.

### 2.4 `usize` from untrusted `u64` via `as` — Medium

`archive/xp3.rs:227, 229, 238, 285` and `storage.rs:674` convert sizes with
`as usize` (truncating on 32-bit targets). Use `usize::try_from(...)` plus a
cap, matching `media/src/tlg.rs:184-190`.

### 2.5 `unwrap()` in the archive parser — Low

`archive/xp3.rs:309, 337, 352-355, 373-376, 382` use `try_into().unwrap()` on
fixed slices. The preceding length checks make them currently sound, but the
file is parsing untrusted bytes; returning `Error::CorruptIndex` on failure is
more robust and removes the last `unwrap`s from library paths.

### 2.6 Public API breadth via glob re-export — Low

`lib.rs:22-59` re-exports essentially every function, type and constant. This
is defensible for an L2 crate consumed by L3 natives, but it makes the public
surface effectively "everything" and weakens semver discipline. Consider
`pub mod` access (callers already use `engine::scene::...` in tests) and
re-export only the intended entry points.

### 2.7 Raw pointer addresses as safe `usize` returns — Medium

`Scene::main_image_pixel_buffer*` / `province_image_pixel_buffer*`
(`scene.rs:509-560`) hand out heap addresses as plain `usize`. They are
documented as an FFI boundary and the safe `with_*_pixels` closures exist, but
there is no compile-time marker. Consider a `#[doc = "... safety ..."]`
contract plus a dedicated `RawPixelBuffer(usize)` newtype so misuse is greppable,
and keep the closures as the only supported path outside `krkr-tjs`.

### 2.8 Index loops vs iterators — acceptable, with caveat

The pixel loops intentionally index (`read_pixel`/`write_pixel`,
`layer_ops.rs:100-122`) for bounds clipping. That is fine, but the per-pixel
helper re-derives `pixel_offset` and re-checks bounds on every sample, which is
measurable in the hot paths (see §3.5). Convert the innermost loops to slice
rows once the arithmetic is safe.

### 2.9 Derives — largely fine

`Rect` correctly derives `Copy, Eq` (`scene.rs:277`); `ComposedLayer` uses
`PartialEq` only (f32) (`projection.rs:168`); `BlendMode`/`DstBlend` in the API
crate derive `Eq, Hash`. No action, but note `Storage`, `MountedArchive`,
`Scene` index maps lack `Debug`, which makes failures harder to diagnose.

---

## 3. Elegance / optimization opportunities (ranked)

### 3.1 `blur_bitmap_in_place` is O(w·h·(2rx+1)·(2ry+1)) — High

`layer_ops.rs:942-972`. For every pixel it re-sums a full 2-D box and
recomputes `count`. This is the worst hot path; `do_drop_shadow`
(`layer_ops.rs:785`) and `do_blur_light` (`layer_ops.rs:807`) call it on
full-size buffers. Replace with a **separable running-sum box blur**
(two O(w·h) passes with a sliding column/row sum), which is both simpler and
asymptotically better. It also removes the `source = bitmap.rgba.clone()`
(`layer_ops.rs:946`) by writing into a single scratch buffer.

### 3.2 Projection allocates per layer and per frame — High

- `compose_layer` (`projection.rs:186-208`) allocates a `Vec` **and** a
  `HashSet` for every drawn layer to walk its ancestor chain.
- `nearest_drawn_parent` (`projection.rs:320-347`) allocates another `HashSet`
  per layer.
- `window_layer_order` (`scene.rs:1393-1444`) rebuilds a `HashMap<Option<u32>,
  Vec<u32>>`, a `Vec`, and a `HashSet` on every call, and is called by
  `layer_at`, `focusable_neighbor`, and the `compose`-adjacent paths.

For a scene of N layers this is O(N) heap allocations per frame. Since the
projection already walks parent-first, `compose_layer` and
`nearest_drawn_parent` can be folded into the same pass: compute
`absolute_offset`/composed opacity/visibility incrementally from the parent's
already-computed values, keyed by `layer.parent`, with no per-node
`HashSet`. Cache the flattened order per `Scene::revision`.

### 3.3 `Host::step` rebuilds the whole `RenderScene` every frame — High

`app/src/lib.rs:216-218`: `let projected = project_scene(...);
self.render_scene = projected;` drops and reallocates every `RenderNode`,
its `children: Vec`, and the arena each frame, even when nothing changed.
`RenderScene`/`RenderNode` derive `Clone` (`render/src/scene.rs:29,157`),
and `Scene::revision()` already exists for exactly this. Keep one persistent
`render_scene`, `clear()` the arena (retaining capacity) and rebuild only when
`scene.revision()` changed — or better, do a field-level sync (D10's stated
end state). This alone removes most per-frame allocator churn for a static
scene.

### 3.4 Full-buffer clones in drop-shadow/blur-light — Medium-High

`do_drop_shadow` (`layer_ops.rs:764-793`) makes three full clones
(`original`, `shadow`, `work`) and `do_blur_light` (`layer_ops.rs:797-813`)
makes one. Reuse one or two caller-supplied scratch `Vec`s, or process per row.
Combined with §3.1 this is the dominant cost of the layer-effect path.

### 3.5 Resampling recalculates kernel weights and clamps per sample — Medium

`sample_kernel` (`layer_ops.rs:1020-1060`) calls the closure `k(...)` for
every tap and `read_pixel_rect` (`layer_ops.rs:980-993`) re-clamps each
coordinate. Precompute the 2-D weight table for the common kinds
(`stLinear`, `stCubic`, `stLanczos*`) once per resample, and hoist row/column
clamping. `sample_area` (`layer_ops.rs:1062-1100`) is O(source footprint) per
destination pixel; for thumbnails (`saveLayerImage`) consider a summed-area
table or a two-pass box prefilter.

### 3.6 `gaussian_blur` clones the full bitmap for `tmp` — Medium

`layer_ops.rs:1566` (`let mut tmp = bitmap.rgba.clone()`). The separable
algorithm is correct; the allocation is avoidable with one scratch buffer
reused from `bitmap.rgba`'s capacity or a row-sized buffer.

### 3.7 `wildcard_match` allocates two `Vec<char>` per call — Medium

`storage.rs:690-692`. `get_file_list` calls it for every disk and archive
entry. Iterating `pattern.chars()`/`text.chars()` (or comparing with
`eq_ignore_ascii_case` on `u8` after ASCII folding) avoids the per-call
allocations.

### 3.8 `ordered_children` clones the child list twice — Low

`scene.rs:1027-1028`: `let mut ids = layer.children.clone(); let maintained =
ids.clone();`. `sort_siblings` already takes `&[u32]`; the "maintained" copy is
only needed because the slice is mutated. Compute the key vector first, or pass
an owned `maintained` only on the slow path.

### 3.9 `flip_lr` / `flip_ud` byte-by-byte swaps — Low

`layer_ops.rs:1352-1382` swap 4 bytes individually. Swapping the two `u32`/4-byte
words (or using `chunks_exact_mut(4)` with `swap`) reduces work ~4×.

### 3.10 `normalize_in_archive_name` `String::with_capacity(name.len())` — Nit

Correct and cheap; noted only because it is called on every lookup. Fine.

---

## 4. Correctness smells

### 4.1 Unchecked pixel-coordinate `i32` addition, inconsistently — High

`raster::fill_rect_replace` correctly saturates (`raster.rs:66-72`:
`x.saturating_add(w)`) — but the blit/resample/composite paths do not:

- `layer_ops.rs:468-469` `dx + (src_rect.2 - src_rect.0)` in `blit_copy_clipped`
- `layer_ops.rs:519, 584, 609` `dx + sw` in `blit_over`/`blit_plane`
- `layer_ops.rs:717, 735` `dx + src.width as i32` in `composite_normal`/`composite_hard_light`
- `layer_ops.rs:835` `left + width as i32` in `fill_operate_rect`
- `layer_ops.rs:1709` `dx + sw` in `operate_rect`
- `projection.rs:249-252` `img_x + image_width as i32` etc. in `visible_image_region`

Coordinates come from scripts, so these panic in debug and wrap in release
(producing wrong clipping). Use `saturating_add`/`saturating_sub` consistently,
or compute in `i64` and clamp once.

### 4.2 Untrusted allocation sizes in the XP3 parser — High

`archive/xp3.rs:227` `Vec::with_capacity(entry.org_size as usize)`,
`:229` `vec![0u8; seg.arc_size as usize]`, `:238` `vec![0u8; seg.org_size as
usize]`, `:285` `vec![0u8; real_size as usize]`, and `:525` `vec![0u8; len]`
allocate sizes taken directly from the archive index with **no cap and no
`checked_mul`**. `media/src/tlg.rs:51-52, 184-190` already caps
(`MAX_DIM`, `MAX_PIXEL_BYTES`, `checked_mul`); the engine should do the same
(and reject `org_size == 0`-with-segments / absurd totals). A malformed or
hostile `.xp3` can OOM the process.

### 4.3 Index-parser `pos + size` overflow — Medium-High

`archive/xp3.rs:310` and `:338` test `pos + size > data.len()` where `size` is
`usize::try_from(u64)` and can be near `usize::MAX`. `pos + size` overflows
(panic in debug, wrap in release → slice OOB panic). Use
`size > data.len() - pos` or `pos.checked_add(size)`. Same class at
`archive/xp3.rs:230, 257, 273` (`self.base + seg.start` / `index_ofs`) — use
`checked_add`.

### 4.4 `u32` accumulators can overflow in pixel sums — Medium

- `blur_bitmap_in_place` (`layer_ops.rs:957-966`) sums `[u32; 4]` over a box
  up to `(2rx+1)(2ry+1)`; large radii overflow. Also the index
  `(sy * bitmap.width + sx) * 4` is computed in `u32` (`layer_ops.rs:962`),
  which overflows for large bitmaps. Use `u64` accumulators and `usize` indices.
- `sample_area` (`layer_ops.rs:1083-1092`) accumulates `[u32; 4]` over the
  destination footprint; a large downscale (e.g. 65536² source) overflows.
  Use `u64`.

### 4.5 Unchecked allocations from layer/window sizes — Medium

- `Scene::resize_province_image` (`scene.rs:643`):
  `vec![0u8; w as usize * h as usize]` with no `checked_mul` (sizes come from
  the layer rect / MainImage).
Guard it with `checked_mul` and a sane cap, as `media` does.

### 4.6 Unbounded loop/count from script geometry — Medium

- `raster::flatten_arc` (`raster.rs:297`): `steps = (sweep_deg.abs()/4.0).ceil()
  as usize` is unbounded; a huge/`inf` sweep allocates an enormous `Vec`.
  Clamp `steps` (e.g. ≤ 4096).
- `raster::bounds` (`raster.rs:107-125`) can return values near `i32::MIN/MAX`;
  `fill_polygon` (`raster.rs:150-152`) then loops over that whole box. Clamp the
  bounds to the bitmap after intersect, and reject non-finite inputs.
  (`flatten_cubic` takes `steps` from the caller — cap it at the API edge.)

### 4.7 `file_size as usize` + unbounded read buffer — Medium

`storage::read_fs_file` (`storage.rs:673-683`): `let size =
fs::file_size(fs, &file)? as usize; let mut out = vec![0u8; size];`.
Truncates on 32-bit and trusts the host file size. Use `try_from` and a cap.

### 4.8 Unchecked `i32` accumulation — Low/Medium

- `Scene::absolute_offset` (`scene.rs:1137-1141`) does raw `x += l.rect.x` over
  the ancestor chain (can overflow).
- `Scene::set_province_pixel` clip test (`scene.rs:696`) uses `c.x + c.w as i32`
  and `c.y + c.h as i32`.
- `Scene::main_image_pitch` (`scene.rs:501`) `b.width * 4` can overflow `u32`.
Use `saturating_*`.

### 4.9 `add_bitmap` / `with_*_pixels` do not validate buffer length — Low

`Scene::add_bitmap` (`scene.rs:1219`) trusts `rgba.len() == width*height*4`,
and `layer_ops` indexes `bitmap.rgba` directly. Only `Host::sync_dirty_bitmaps`
(`app/src/lib.rs:326-352`) validates before upload. An early
`debug_assert_eq!`/`checked_mul` in `add_bitmap` would fail fast at the source.

### 4.10 D9/D10 stateless invariants — PASS, with two notes

- No `static`/`thread_local`/`LazyLock`/`OnceLock`/`OnceCell`, no
  `&mut self` service objects, no `unsafe`; `#![forbid(unsafe_code)]` in both
  crates (`engine/src/lib.rs:17`, `app/src/lib.rs:22`). ✔
- All evolving state lives in caller-owned values (`Scene`, `Storage`,
  `Xp3Archive`, `Host`) and is passed explicitly to `fs`/`render`
  actions. `archive/xp3.rs` correctly stores no file handle; its
  `FileReader.pos` cursor is a local adapter field. ✔
- The only `HashMap` keyed by an externally supplied id is `Host.textures:
  HashMap<u32, Texture>` (`app/src/lib.rs:154`), which is L2/L4 state
  holding L1 handles — allowed (D9 forbids it on L1, not L2). It is mirrored by
  the explicit `Scene` bitmap table, so it is a derived index, not a hidden
  cache. Acceptable, but worth a one-line comment.
- `Scene`'s private `window_index`/`layer_index`/`bitmap_index`/`font_index`
  are L2-internal caches over public `Vec` fields that "may be edited
  directly" by callers (`scene.rs:293-311` etc.). They are repaired on
  removals and guarded by a linear-scan fallback, which works but adds
  invariant surface. Either make the `Vec`s private (forcing use of the
  accessors) or drop the maps and rely on the scan; the hybrid is the most
  fragile option. **Medium** as a design cleanliness issue, not a live bug.

### 4.11 `main.rs` host loop — OK

`WindowedApp` guards double `resumed`, computes `dt` with
`saturating_duration_since`, clamps window sizes, and clamps `dt_ms` at
`Host::step`. No panics found. `--frames` uses `u32` and `0..frames`. Good.

---

## 5. Cross-cutting observations

- **Defensive caps are inconsistent between L1 crates.** `media` caps
  image dimensions/bytes and uses `checked_mul`; `engine`'s XP3/province
  paths do not. A shared `checked_dims(w, h, bpp)` helper in
  `api` (or at least duplicated) would align them.
- **Saturating arithmetic is applied ad hoc.** `fill_rect_replace` is
  careful; the blit/transform paths are not. Pick one policy (saturating
  everywhere coordinates are derived from scripts) and apply it uniformly.
- **Tests exist for the happy paths** (`engine/tests/{scene,projection,storage,xp3}.rs`,
  `app/tests/host.rs`) but none exercise negative coordinates, huge extents,
  malformed XP3 sizes, or the overflow cases above. Adding those would lock in
  the fixes.

---

## Top 10 recommended changes

1. **Cap and `checked_mul`/`checked_add` every XP3 index size and read
   allocation** (`archive/xp3.rs:227,229,238,285,310,338,525`), mirroring
   `media/src/tlg.rs:51-52,184-190`. *(High, security/robustness)*
2. **Make all script-derived coordinate arithmetic saturating or `i64`** in
   `layer_ops.rs:468-469,519,584,609,717,735,835,1709,1999` and
   `projection.rs:249-252`, matching `raster.rs:66-72`. *(High, correctness)*
3. **Rewrite `blur_bitmap_in_place` as a separable running-sum box blur**
   (`layer_ops.rs:942`) and drop the full `rgba.clone()`. *(High, perf)*
4. **Eliminate per-layer `Vec`/`HashSet` allocations in `compose_layer` and
   `nearest_drawn_parent`** (`projection.rs:186,320`) by composing in the
   parent-first projection pass, and cache `window_layer_order`
   (`scene.rs:1393`) per `Scene::revision`. *(High, perf)*
5. **Reuse the `RenderScene` arena in `Host::step`** and skip reprojection
   when `Scene::revision()` is unchanged (`app/src/lib.rs:216-218`).
   *(High, perf)*
6. **Resolve the dependency/architecture conflict:** either move
   `RenderScene`/`RenderState`/`Surface`/DTOs into `api` so
   `engine` depends on `api` only, or amend
   `docs/migration/02-target-architecture.md` §4 to allow the L1 type crates
   (`fs`, `render`) and document `flate2`. *(High, design)*
7. **Use `u64` accumulators in `blur_bitmap_in_place` and `sample_area`**
   (`layer_ops.rs:957,962,1083`) and `checked_mul`+cap for
   `resize_province_image` (`scene.rs:643`). *(Medium, correctness)*
8. **Bound `flatten_arc` steps and polygon bounds** (`raster.rs:297,107-152`)
   to prevent huge allocations/loops from script input. *(Medium, DoS)*
9. **Restore typed FS errors through `FileReader`** (`archive/xp3.rs:444-464`)
   and rename `archive::xp3::Error`/`storage::ReadError` to match the
   `*Error` convention. *(Medium, consistency/API)*
10. **Reduce full-buffer clones and the `BitmapState: Clone` derive**
    (`scene.rs:180`; `layer_ops.rs:775,777,785,807`) via explicit scratch
    buffers, and fix the stale `set_focus` doc (`scene.rs:946`) and
    `usize as` conversions in `storage.rs:674`. *(Medium/Low)*
