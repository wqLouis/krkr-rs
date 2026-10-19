# 14 — The L3 ↔ L2 protocol

Status: implemented. The seam module is `crates/compat/src/engine_api.rs`; the
`l3-engine-api` layering rule and its self-test fixture enforce it.

This document is the protocol between the compatibility layer (`crates/compat`,
L3) and the engine (`crates/engine`, L2): the exact engine subset that compat
codes against, the state each part owns, the coordinate space it speaks, the
errors it reports, and how long the attached handles live. Read it when an L3
call into the engine is being added, changed or debugged.

Design sources: [`02`](02-target-architecture.md) §1–§2 (layers, one dependency
direction), [`03`](03-middle-layer-api.md) §0 and §8 (stateless objects, L2 is
the state store), [`08`](08-tjs-vm-and-native-plugins.md) §3.1 (the one
process-wide binding the C ABI forces), [`11`](11-presentation-and-pixel-translation.md)
§1 and §3 (VM pixels vs device pixels, where the translation lives),
[`12`](12-layer-tree-and-frame.md) §1 and §8 (the logical tree and the
projection seam), and [`README`](README.md) §3 (D9, D10, D16, D26, D32, D34,
D39–D41).

---

## 1. Purpose

L3 implements the script API by *composing* L2 primitives; it owns no engine
state and does not grow a second engine (02 §1, README D16). The engine subset
that composition depends on is declared here and in the seam module:

* **L3 codes against this subset.** Every engine item compat touches is listed
  in §6 and re-exported by the seam module.
* **Swapping L2 means implementing this surface.** A replacement engine must
  provide these types and functions with the same ownership, coordinate and
  error contracts; nothing else in `engine` is part of the L3 contract.
* **There is one place to read** for ownership, coordinate and error questions.

The seam is a re-export, not a wrapper: a protocol type *is* the engine type, so
`CompatHost`/`CompatServices` public signatures, the script surface and the
surface lock are unchanged. Behavior changes belong in L2 or in L3's marshalling,
never in the seam file.

### 1.1 Enforcement

`crates/compat/src/engine_api.rs` is the only file under `crates/compat/src`
allowed to contain `engine::`. The `l3-engine-api` rule in
`scripts/krkr_gates/checks/layering.py` reports any other file, and its
self-test proves the rule fires on a bad fixture and leaves a near-miss clean.

Adding a protocol item:

1. add or reuse the item in `engine` with an L2-generic name and contract;
2. re-export it in the correct `engine_api` group;
3. document it in §6 (and its state/coordinate/error contract if non-obvious);
4. use it from L3 through `crate::engine_api::…`.

Removing one deletes it from the engine, the seam, this document and every
call site in the same change; no compatibility shim is kept.

---

## 2. Ownership: L2 owns the state, L1 is stateless

D9 and 03 §0 draw one line: an L1 object is a stateless facade whose methods
take evolving state as an explicit `&mut` argument, and L2 stores every state
value. The protocol respects that line in both directions:

* **No L1 service appears in the seam.** `FileSystem`, `RenderPipeline`,
  `Media`, `FontSystem` and the platform services are L1; L2 calls them
  internally. L3 never names one.
* **Every seam item is L2 state or an L2 action on that state.** The layer
  tree, storage namespace, input/timer registries, text engine, mixer, video
  overlays and transitions are all L2 values. L3 reads and mutates them through
  the listed operations and keeps only dispatch state of its own.

L3 may *construct* an L2 state value (a `TextEngine`, a `Mixer`, a `Trans`) as
part of a native payload, but the value remains L2-typed state and lives in the
one process-wide `CompatServices` binding, never in a second global (08 §3.1).

---

## 3. Coordinates: VM pixels only

There are two pixel spaces in the engine, and only one of them crosses this
seam:

| space | owner | crosses L3 ↔ L2? |
|---|---|---|
| **VM pixel** — one pixel of the logical screen (the primary layer) | L3 and scripts; L2 stores layer/bitmap/font values in it | **yes** |
| **device pixel** — one pixel of the framebuffer/swapchain | L1; L2 maps into it during projection and readback | no |

Every rect, bitmap dimension, font size, glyph metric, readback coordinate and
input position in the protocol is a VM pixel. L2 owns the Fit mapping
(`engine::translate::PixelTranslation`) and applies it inside projection and
readback; the composition root calls the same L2 mapping for platform input, so
L3 never calls `to_device_*`/`to_vm_point`/`raster_scale` and never names a
device pixel (11 §3, §6.4; README D39–D41). Input reaches L3 only in VM
coordinates, so even an L3 hit-test sees VM pixels (11 §6.3).

A script can observe only VM values (D34): a change that made a protocol item
return or accept device coordinates would be a compatibility bug, not an
internal refactor.

---

## 4. Errors: typed values, no silent fallbacks

L3 marshals a typed error into whatever the script-visible behavior requires;
it never invents a default value to keep going (D11, D32). The protocol's error
types are values owned by the module that declares them (03 §0.3):

| error | declared by | crossing rule |
|---|---|---|
| `StorageReadError` | `engine::storage` | returned by `Storage` reads; L3 maps it to the script error/`void` the reference produces |
| `TextError` | `engine::text` | returned by text shaping/measurement; L3 surfaces the script-visible failure |
| `SoundError` | `engine::sound` | returned by mixer/buffer/channel operations; L3 maps it to the `WaveSoundBuffer` error path |
| `TransError` | `engine::trans` | returned by `Trans::*`/`RuleMask` constructors; L3 converts its message to the script error text and does not name the type in the seam |
| `VideoError` | `engine::video` | returned by `VideoOverlay` playback operations; L3 logs/reports it and does not name the type in the seam |

Two conventions are deliberate and are not fallbacks:

* **`Option` for an absent object.** `render_layer_subtree` and
  `ensure_transition_display` return `None` when the requested layer/subtree
  does not exist; the caller turns that into the script-visible error. The
  absence is distinguishable from a real failure, which is a typed error.
* **`()` for infallible composition.** Most `layer_ops`/`raster` primitives
  operate on already-validated VM bounds and pixels; they have no error case.
  Validation that can fail (a bad clip, a missing layer) happens in the
  operation that receives script input, not in the pure primitive.

A new failure mode is a new variant in the owning module's error enum, mapped
at the L3 call site; it is never an `unwrap_or`, a wildcard arm that swallows a
variant, or an L3-side substitute.

---

## 5. Lifetime of the attached handles

The VM C ABI hands native callbacks only the raw `tjs2_engine*` and has no
user-data slot, so `compat` keeps exactly one process-wide engine → context
binding (08 §3.1). That binding is lifetime-free, which fixes the protocol's
lifetime rules:

* **`CompatServices` attaches raw pointers** to the shared L2 handles with
  `attach_scene`, `attach_storage`, `attach_input` and `attach_timer`. The
  attachment is `unsafe`: the pointed-to value must stay alive for as long as
  the binding is installed (the VM's lifetime). Detaching/teardown happens
  before the L2 owner is dropped.
* **The accessors return the raw pointer**, not a reference: `CompatServices`
  cannot prove the L2 owner is still alive, so forming a `&Scene` there would
  be an unsound shared reference. Natives dereference under their own `unsafe`
  contract.
* **`TimerCallback` is stored by L2.** L3 installs the callback when it creates
  a timer; L2's frame pump invokes it and L3 removes the timer before the
  binding is torn down. The callback may re-enter the VM, so it must not be
  called after teardown.
* **Other L2 values live in L3 dispatch state.** A `TextEngine` (inside the
  resource resolver), a `Mixer` (inside the sound state), a `VideoOverlay`
  (inside a `VideoOverlay` native payload) and a per-layer `Trans` are
  constructed by L3 but are L2 state values; they live in the one binding's
  dispatch state / native payloads, not in process globals, and are dropped
  with the object or host.

---

## 6. The surface, by engine module

The seam is grouped the same way. Short doc comments in `engine_api.rs` are the
authoritative in-tree summary; this section states the contract behind each
group.

### 6.1 `engine::scene` (re-exported flat)

**Items:** `Scene`, `LayerState`, `BitmapState`, `FontState`, `Rect`,
`DecodedImageKey`, `DecodedImageMode`.

**Owns:** the logical tree — layer ids, parent/child/domain links, z-order,
visibility, opacity, blend, image placement/size and rects; bitmap pixel
buffers and their decoded-image cache keys; font objects; window entries and
focus domains. `Scene` is the whole tree plus the decoded-image cache and a
mutation revision; the other types are its per-node/state values. Masks,
province images and other script concepts built on top of these values stay in
L3 state.

**Coordinates:** all rects and bitmap dimensions are VM pixels; bitmap buffers
are VM-sized because scripts read them back through L3. L2 projects them into
device space; L3 never sees that projection.

**Errors:** these are infallible state values. Mutations that can fail are
validated by the operation that receives script input (a typed error in
`storage`/`text`, an `Option` from `layer_ops`, or a script-level check in L3).

**Lifetime:** `Scene` is attached to `CompatServices` as `*mut Scene`; it must
outlive the binding.

### 6.2 `engine::storage`

**Items:** `Storage`, `Location`, `StorageReadError`,
`normalize_in_archive_name`.

**Owns:** the mounted archive namespace, normalized archive paths,
`Location` values, the registered media table and the installed XP3 extraction
filter. Raw IO is L1 (`fs`) and stateless; the namespace state is L2.

**Coordinates:** none — names and byte ranges.

**Errors:** `StorageReadError` (not found, archive/format, filtered read) is
returned by reads; L3 turns it into the script-visible result.

**Lifetime:** `Storage` is attached as `*mut Storage` and must outlive the
binding.

### 6.3 `engine::input`

**Items:** `InputState`.

**Owns:** per-frame key/button/touch state, pointer position and visibility,
the wheel delta, text input, the last-pressed list, the frame counter and the
click-sequence window. `InputState` is L2 state; L3 owns only the event→state
marshalling and the frame boundary calls around it.

**Coordinates:** pointer/touch positions are VM pixels; the composition root
has already mapped them from the device before L3 sees them (11 §6.3).

**Errors:** none; this is state.

**Lifetime:** attached as `*mut InputState`, valid for the binding. L3's
event adapter calls `begin_frame`/`end_frame` around the platform-event drain;
the bookkeeping itself stays in L2 state.

### 6.4 `engine::timer`

**Items:** `TimerState`, `TimerId`, `TimerCallback`.

**Owns:** the timer registry — interval, enabled flag, stored callback, live
count, the registry's `now_ms` and fire scheduling. L2's frame advances the
registry before the frame body runs (`Engine::frame`, 03 §8, README
D26/D27); the frame body only drains the posts L2 produced. A host or test
that drives the registry itself may call the poll entry point, which advances
at the registry's own timestamp and still takes no `dt`.

**Coordinates:** none (milliseconds).

**Errors:** none on the registry surface; a callback reports through the VM.

**Lifetime:** attached as `*mut TimerState`; callbacks are removed before the
binding is torn down.

### 6.5 `engine::text`

**Items:** `TextEngine`, `TextError`, `FontId`, `TextStyle`, `CoverageMask`,
`TextCoverageCache`, `rasterize_text_coverage`, `blurred_text_coverage`,
`blur_coverage`, `blit_coverage_into`, `compose_text_into`.

**Owns:** shaping/layout caches, the registered font faces, the glyph atlas and
the coverage/blur caches. A face is loaded from storage bytes through L2; L3
supplies the bytes and the face/style request.

**Coordinates:** font sizes, glyph advances and text rects are VM pixels.
A `CoverageMask` is a VM-sized alpha mask; L3 blits it into a VM bitmap.
Device-resolution glyph work stays inside L2's text path; 11 §7 records the
parts of it that are still open.

**Errors:** `TextError` (unknown/invalid face, shaping failure).

**Lifetime:** the `TextEngine` and `TextCoverageCache` are L2 values stored in
the L3 resource resolver (dispatch state), dropped with the host.

### 6.6 `engine::sound`

**Items:** `Mixer`, `AudioStream`, `SoundBuffer`, `SoundChannel`,
`SoundSource`, `SoundError`, `SoundEvent`, `SoundLabel`, `LoopCondition`,
`LoopInfo`, `LoopLink`, `Status`, `LOOP_FLAG_COUNT`.

**Owns:** the mixer and its output clock, per-buffer/channel playback state,
loop state and labels, and the decode-ahead stream. L2 mixes; the host installs
the L1 decoder through `AudioStream`/the audio-decoder seam. L3 maps the
script-visible 0..100000 volume scale to L2's normalized gain and never
re-implements mixing.

**Coordinates:** none — samples, frames, samples-per-second and seconds.

**Errors:** `SoundError` (decode, unsupported format, invalid state).

**Lifetime:** the `Mixer` lives in the sound dispatch state; buffers, channels
and streams live in the native payloads and are dropped with the host.

### 6.7 `engine::video`

**Items:** `VideoOverlay`, `VideoEvent`, `PlayStatus`, `PeriodReason`,
`VideoBounds`, `blit_frame_to_layer`, `blit_frame_to_layer_opacity`.

**Owns:** the overlay playback machine — status, clock, position, play rate,
whole-stream/segment looping, period-event state, the current decoded frame,
the overlay binding and the pending event stream. The host supplies decoded
frames; L2 does not call a codec. The `blit_*` functions compose the current
frame into a layer.

**Coordinates:** bounds, frame size and blit rects are VM pixels.

**Errors:** `VideoError` from playback operations; L3 reports/logs it and does
not name the type in the seam.

**Lifetime:** the `VideoOverlay` is a native-payload value; the composition
root installs the frame source through the host seam (`set_video_source`).

### 6.8 `engine::trans`

**Items:** `Trans`, `TransKind`, `RuleMask`, `ScrollFrom`, `ScrollStay`.

**Owns:** the transition state machine (kind, duration, elapsed/phase, rule
mask, completion) and its scroll vocabulary. L3 stores one machine per layer in
its dispatch state and advances it from the frame body; L2 owns the machine and
its stepping.

**Coordinates:** rule masks and the transition destination are VM pixels.

**Errors:** `TransError` is returned by the constructors and converted to the
script error text by L3.

**Lifetime:** per-layer value in L3 dispatch state; dropped with the layer.

### 6.9 `engine::raster` (submodule)

**Items:** `blend_pixel`, `fill_polygon`, `fill_rect_replace`, `flatten_arc`,
`flatten_cubic`, `hatch_foreground`.

**Owns:** nothing — pure functions over a `BitmapState`; the bitmap is the
state, passed in. This is the pixel primitive layer L3 uses for the few
plugin-specific gathers/scatters that have no higher-level operation.

**Coordinates:** bitmap-local VM pixels; the caller clamps to the bitmap.

**Errors:** none; bounds are the caller's validated VM bounds.

### 6.10 `engine::layer_ops` (submodule)

**Items:** `AffineBlit`, `BlitOptions`, `BlitSource`, `COPY_MAIN`, `COPY_MASK`,
`GammaAdjust`, `RectI`, `adjust_gamma`, `affine_blit`, `blit_copy`,
`blit_over`, `blit_plane`, `blur_bitmap_in_place`, `clip_rect`,
`convert_add_alpha_to_alpha`, `convert_alpha_to_add_alpha`, `copy_9patch`,
`do_gray_scale`, `ensure_transition_display`, `fill_color_hold_alpha`,
`fill_color_on_add_alpha`, `fill_color_on_alpha`, `fill_mask`, `flip_lr`,
`flip_ud`, `gaussian_blur`, `intersect`, `intersect_rect`, `light_contrast`,
`operate_rect`, `remove_const_opacity`, `render_layer_subtree`,
`stretch_blit_mode`, `stretch_blit_plane`.

**Owns:** nothing of its own — the layer/bitmap state lives in the `Scene` and
`BitmapState` arguments. These are the L2 composition and pixel-region
operations: clip intersection, blits, affine/stretch blits, fills, masks,
colour conversion, blur, flipping, gamma/light/contrast and subtree rendering.

**Coordinates:** VM pixels; `RectI`/clip rects are VM, and bitmap buffers are
VM-sized. The primitives do not apply the device translation — the projection
does.

**Errors:** `render_layer_subtree` and `ensure_transition_display` return
`Option` for an absent layer/subtree; the rest are infallible on validated VM
bounds.

---

## 7. What the protocol deliberately leaves out

These are L2↔L1 or L4↔L2 concerns and must not appear in the seam:

* **The renderer and Vulkan.** `RenderScene`, `RenderState`, `Surface`, the
  projection (`project_scene`/`project_window`), the draw tree and every
  backend detail. L2 projects the logical tree and L1 draws it; L3 is not on
  that path at all (12 §1–§5).
* **The pixel-translation value itself.** `PixelTranslation`,
  `to_device_*`/`to_vm_point`/`raster_scale` are L2-internal; exposing one to
  L3 would break D41 and spread the coordinate rule across natives (11 §3,
  §6.4).
* **The frame clock and `Engine`.** `Engine`, `EngineError`, `GameFrame`,
  `Engine::frame`/`step` belong to the composition root, which implements
  `GameFrame` and runs the L3 frame-body hook. L3 receives a `dt`; it does not
  own the clock (03 §8, README D26).
* **Platform backends.** Window/event loop/input devices, `fs`, `media`
  decoders (FFmpeg/MediaCodec), the clipboard backend and the platform clock.
  L3 reaches them only through L2 or through a host-installed seam (a decoder
  callback, a clipboard trait), never by naming an L1 crate type here.
* **Game loading and startup.** `discover_startup`, `StartupError` and archive
  discovery are composition-root concerns (09).
* **The VM.** `tjs::Tjs2Engine` is L4. L3 names it directly because it is the
  host of the natives; it is not part of the engine protocol.

---

## 8. Extending the protocol (and what not to do)

* **An L3 behavior that needs engine state** is a missing protocol item: add
  the generic operation to L2, re-export and document it here. Do not move
  TVP-specific logic into L2 to shrink L3, and do not reimplement the engine
  behavior in L3 (README D16).
* **A script-visible value** stays exactly reference behavior; the protocol
  document records the ownership/coordinate/error rule, not the script
  semantics, which live in the L3 native and its tests (README D32/D34).
* **An L1 mechanism** (a new decoder, a platform capability) never enters the
  seam directly; it enters through an L2 operation or a host-installed callback
  that L3 already has.

---

## 9. Debugging a protocol failure

When an L3 call fails, the failure names a protocol item (e.g.
`StorageReadError` from `Storage::read`, `TextError` from `TextEngine`,
`None` from `render_layer_subtree`). Use that name:

1. **Find the item in §6 (or in `engine_api.rs`).** The section states who owns
   the state and whether the result is VM or an error.
2. **Check the ownership layer.** Engine state belongs to L2. If the fix wants
   to cache, duplicate or reinterpret engine state inside L3, the bug is
   usually a missing L2 operation, not an L3 workaround (D16).
3. **Check the coordinate space.** If a value looks scaled, offset or clipped,
   confirm it is VM space; L3 must never scale for the device. The mapping is
   L2's and is applied inside the protocol operation (D40/D41).
4. **Check the lifetime.** A stale `*mut Scene`/`*mut Storage`/`*mut InputState`/
   `*mut TimerState` means the L2 owner was dropped or the binding outlived it;
   the attach contract in §5 is the invariant, not a retry point.
5. **Check the error path.** A typed error must be mapped to the script-visible
   failure; swallowing it (a default, a wildcard match, `ok()`) is a bug even
   if the scene continues to render (D11).
6. **If the rule fires,** the offending file names `engine::` outside the seam;
   move the use into `engine_api.rs` (and document a new item if needed)
   rather than allowlisting the site — `l3-engine-api` has no allowlist.

---

## 10. References

| topic | document |
|---|---|
| layers, dependency direction, crate roles | [`02`](02-target-architecture.md) §1–§2 |
| stateless L1 / L2 as state store, `Engine::frame` | [`03`](03-middle-layer-api.md) §0, §8 |
| the one process-wide binding | [`08`](08-tjs-vm-and-native-plugins.md) §3.1 |
| VM vs device pixels, where the translation lives | [`11`](11-presentation-and-pixel-translation.md) §1, §3, §6 |
| logical tree, projection, frame orchestration | [`12`](12-layer-tree-and-frame.md) §1, §6, §8 |
| decisions D9/D10/D16/D26/D32/D34/D39–D41 | [`README`](README.md) §3 |
| game loading, startup discovery | [`09`](09-game-loading.md) |
