# Render layer code review — `crates/platform/render`

Scope: `crates/platform/render/src/{lib,scene,state,texture,null,glyph}.rs` and
`src/vulkan/{mod,device,swapchain,pipelines,texture,draw,shaders}.rs`.

Read against the intended design:
`docs/migration/04-renderer-vulkan.md` and `docs/migration/03-middle-layer-api.md`
(§0 model, §2 render pipeline, §3 glyph), with cross-checks against the L2
projection (`crates/engine/src/projection.rs`,
`crates/app/src/lib.rs`) and `crates/platform/api/src/{geometry,blend,error}.rs`.

Method: source read-only. No `cargo`/build/test was run. Line references are
`path:line` from the repository root.

Severity legend:

| Severity | Meaning |
|----------|---------|
| **Critical** | Panic/abort or incorrect memory access reachable from a normal public call. |
| **High** | Wrong rendered output, persistent GPU resource leak, or a lifecycle bug that makes the backend fail after a routine event. |
| **Medium** | Correctness edge cases, unnecessary hot-path cost, or API/consistency drift that will bite later. |
| **Low** | Local robustness/idiom issues, worth fixing opportunistically. |
| **Nit** | Style/consistency only. |

---

## 0. Overall assessment

This is a strong first Vulkan backend. The D9 split is respected: the
`RenderPipeline` is stateless and holds only immutable mechanism
(`VulkanPipeline`, `InstanceGuard`), while every mutable GPU object lives in the
caller-owned `VulkanState` (`vulkan/mod.rs:75-96`, `:641-690`). The
`Arc<InstanceGuard>` trick makes pipeline/state drop order irrelevant
(`vulkan/mod.rs:87-107`) and the reverse-order teardown in `VulkanState::drop`
is careful. The render pass/format are pinned across swapchain recreation so the
compiled pipelines are never rebuilt (`vulkan/swapchain.rs:1-9`). Sub-rect
(`PixelRegion`) staging uploads are implemented from day one with correct
layout transitions, and the fixed-function blend table is pre-compiled into
7+7 pipelines. Docs and `//!`/`# Safety` coverage are excellent
(`#![warn(missing_docs)]`, `lib.rs:31`).

The problems are concentrated in three areas: (1) a likely **vertical flip in
the projection matrix** that also disagrees with the scissor, (2) **error-path
resource leaks** during `begin_render_state` and texture creation, and (3)
**per-frame/per-upload allocations and synchronous waits** that contradict the
design's persistent-staging-ring intent. There is also a set of lifecycle
robustness gaps around the `image_available` semaphore and swapchain
synchronization.

---

## 1. Vulkan correctness and lifecycle

### 1.1 The projection matrix flips the scene vertically — **High**

`crates/platform/render/src/vulkan/draw.rs:398-414`:

```rust
let b = -2.0 / extent.height.max(1) as f32;
...
a * affine.tx - 1.0,
b * affine.ty + 1.0,
```

With the identity affine this produces `ndc_y = -2·y/h + 1`. In Vulkan, NDC
`y = -1` is the **top** of the framebuffer (the viewport has positive height,
`mod.rs:545-552`), whereas L2 explicitly uses a **y-down** convention
(`crates/engine/src/raster.rs:8`, and `dst` from
`visible_image_region`/`projection.rs:400-430` is top-left based). The mapping
therefore puts world `y = 0` at the framebuffer bottom. The correct y-down
mapping is `ndc_y = +2·y/h - 1`.

This is corroborated by the shader comment itself
(`shaders/quad.vert:6`: "origin at the top-left (y grows downward)") and, more
seriously, by the **scissor**: `rect_to_scissor` (`draw.rs:335-363`) uses the
same world `y` directly for the Vulkan scissor offset (top-left origin), so
content and clip are mirrored relative to each other. Any clipped node will clip
the wrong half of the screen.

Fix `b` to `2.0/h` and the translation to `-1.0` (or rename the convention and
flip `rect_to_scissor` to match, but the former matches L2). Recommend adding a
golden-image test (design §11.1) because the current unit tests cannot observe
this (there is no GPU test).

### 1.2 `begin_render_state` leaks the device/surface on late errors — **High**

`crates/platform/render/src/vulkan/mod.rs:257` (`create_frames(...)?`), `:259-271`
(the `render_finished` collect), and `:274` (`create_upload_resources(...)?`)
all early-return with `?` **after** the device, swapchain, pipelines and surface
were created. Unlike the two cleanup blocks at `:195-205` and `:240-250`, these
paths destroy nothing, so the logical device, its children and the surface leak
permanently. The `collect::<Result<_,_>>()` at `:259` additionally drops any
already-created semaphores without destroying them.

Wrap the post-swapchain setup in one fallible helper guarded by an RAII owner
(a small `VulkanStateBuilder` whose `Drop` tears down what exists), or extend
the explicit teardown. This is the kind of leak that only manifests when memory
is tight — exactly when the create calls fail.

### 1.3 No surface/state identity check — **High**

`render` (`vulkan/mod.rs:443-460`) and `resize` (`:319-343`) read
`surface_handle` from the `Surface` argument and then acquire/present/recreate
against it, while the framebuffers, render pass and swapchain belong to
`vstate`. The only validation is backend-kind (`SurfaceKind::Vulkan` vs
`StateKind::Vulkan`), so a `Surface` from a *different* pipeline/state pair is
accepted and used to create a swapchain on the state's device. The public
doc-comment promises `InvalidArgument` for a mismatched pair
(`lib.rs:306-311`).

Add `if surface_handle != vstate.surface { return Err(InvalidArgument) }` in
`render`/`resize`. Separately, `VulkanSurface` is a bare handle with **no
`Drop`** (`vulkan/mod.rs:629-632`) while the same `vk::SurfaceKHR` is owned and
destroyed by `VulkanState::drop` (`:729`): a `Surface` outliving its
`RenderState` is a use-after-free. Either give `Surface` the ownership and hand
the raw handle into the state, or have `render` take only `state` and derive the
surface from it.

### 1.4 Acquired-image semaphore desyncs on error returns — **Medium/High**

In `render`, once `acquire_next_image` succeeds (`vulkan/mod.rs:471`) the
`image_available` semaphore is signalled. Any `?` between acquire and
`queue_submit` — `reset_fences` (`:505`), `reset_command_pool` (`:508`),
`begin_command_buffer` (`:516`), `end_command_buffer` (`:600`) — returns an
error while leaving that semaphore signalled and never waited. The next frame
reuses the same frame-slot semaphore in `acquire_next_image`, which requires it
to be unsignalled. Result: validation error or a permanent stall. Either record
a minimal submit/present on the failure path to consume the semaphore, or
recreate the frame's `image_available` semaphore when an error unwinds.

### 1.5 `render_finished` semaphores reused after an out-of-date present — **Medium**

`rebuild_image_sync` (`vulkan/mod.rs:922-939`) only recreates the per-image
present semaphores when the image count changes and otherwise deliberately
reuses them "to avoid destroying a semaphore the presentation engine may still
be consuming". But `queue_present` can return `OUT_OF_DATE`/`SUBOPTIMAL` after
signalling/consuming a semaphore, and the code then recreates the swapchain and
reuses the old `render_finished[image_index]` on a later submit. Signalling an
already-signalled binary semaphore is invalid. The usual fixes are to recreate
the semaphores per swapchain generation (not per image count) under
`device_wait_idle`, or to gate present with a per-image fence. The comment at
`:920-921` should not be trusted as-is.

### 1.6 Partial swapchain-build failure retires the old swapchain — **Low/Medium**

`swapchain::create` passes `old_swapchain` (`swapchain.rs:130-135`). Per spec,
a **successful** `vkCreateSwapchainKHR` retires the old swapchain even if a
later step (image-view/framebuffer creation) fails. On such a failure,
`destroy_partial` (`swapchain.rs:238-260`) destroys the *new* swapchain and
`recreate_swapchain` keeps the *old* one as valid
(`vulkan/mod.rs:908-912`) — but it is retired, so a later retry passes a retired
handle. This window is narrow (only failures between create and framebuffer
success), but it should be handled explicitly: on any post-create failure,
retire and null the old swapchain too (or rebuild from a clean device state).

### 1.7 Minimize / zero-size surface handling — **Low**

`render` only guards `swapchain_ready`/`images.is_empty()`
(`vulkan/mod.rs:443`). `choose_extent` clamps to at least 1×1
(`swapchain.rs:280-297`), so a minimized (0-sized) surface becomes a 1×1 present
rather than a `SurfaceEvent::OutOfDate`. `resize` also stores the requested
`(0,0)` in `surface_size` (`:339`) but reports the clamped extent. The design
(§6) calls out minimize handling; make the intent explicit.

### 1.8 Redundant idle before recreate — **Low**

`resize` calls `device_wait_idle` (`vulkan/mod.rs:340`) and then
`recreate_swapchain` calls it again (`:882`). One wait is enough.

### 1.9 Image layout transitions — mostly correct

The upload transitions `UNDEFINED`/`SHADER_READ_ONLY_OPTIMAL` →
`TRANSFER_DST_OPTIMAL` → `SHADER_READ_ONLY_OPTIMAL` with matching stage/access
masks (`vulkan/texture.rs:253-323`), and the render pass owns the
`UNDEFINED → COLOR_ATTACHMENT_OPTIMAL → PRESENT_SRC_KHR` transitions
(`swapchain.rs:263-311`). One gap: if `record_and_submit` fails **after** the
queue submit (hidden by the closure at `texture.rs:260-322`), the image may be
left in `TRANSFER_DST_OPTIMAL` while `uploaded` is still `true` from a previous
success; the next upload then assumes the wrong old layout. Rare, but the state
should be invalidated on any upload error.

### 1.10 Memory type selection — correct, but hot and slightly fragile

`find_memory_type` (`vulkan/texture.rs:503-524`) is correct, but:
- `1 << index` (`:512`) should be `1u32 << index`; it is safe only because
  `memory_type_count <= 32` (`index <= 31`). Express the intent.
- It calls `vkGetPhysicalDeviceMemoryProperties` on **every** texture creation
  and **every** upload (`:510`). Cache `PhysicalDeviceMemoryProperties` in
  `VulkanPipeline`/`VulkanState`.

### 1.11 `unsafe` blocks and SAFETY contracts — good, with nits

Nearly every block has a `SAFETY:` note, and `InstanceGuard` correctly keeps the
loader alive for the instance pointers (`mod.rs:83-107`). Nits:
- `shaders::to_words` (`vulkan/shaders.rs:22-28`) silently drops trailing bytes
  for a non-multiple-of-4 blob; add a `debug_assert_eq!(bytes.len() % 4, 0)`.
- The 96-byte push-constant layout is duplicated as a constant and a struct with
  no compile-time link (`mod.rs:67`, `draw.rs:23-33`). Add
  `const _: () = assert!(size_of::<PushConstants>() as u32 == PUSH_CONSTANT_SIZE);`.
- `draw.rs:247-253` builds a slice from a stack value; sound but template-y —
  prefer `bytemuck`/`as_bytes` helper or at least keep the SAFETY note (it is
  present).
- `device.rs:40-56` `extension_name`/`layer_name` have safety notes; fine.

---

## 2. Idiomatic Rust

### 2.1 Public API is broader than the contract — **Medium**

The contract has exactly two entry points for mechanism:
`create_render_pipeline(config)` and the free actions (`03-middle-layer-api.md
§2.1`). The crate also exposes:
- `RenderPipeline::null()` (`lib.rs:156`) and `impl From<NullRenderPipeline>`
  (`lib.rs:177`), which bypass `PipelineConfig` (validation, application name).
- `pub use crate::vulkan::VulkanPipeline` (`lib.rs:58`) together with
  `VulkanPipeline::new` (`vulkan/mod.rs:121`), a second way to build a backend.

Make the direct constructors `pub(crate)` (or feature-gate the test helpers) and
keep only `create_render_pipeline`. The null-only introspection
(`NullRenderPipeline::last_frame`/`texture_pixels`/`texture_uploaded_bytes`,
`null.rs`) is only reachable through the concrete type; consider a
`#[cfg(test)]`/`testing` feature so the released surface stays minimal.

### 2.2 `PipelineConfig.preferred_format` is dead — **Medium**

`lib.rs:89`/`:128` define it, neither backend reads it: the Vulkan backend picks
the surface format itself (`swapchain.rs:220-243`) and the null backend
hardcodes `Rgba8Srgb` (`null.rs:66`). Either honor the preference in
`choose_surface_format` or remove the field so callers are not misled. The
`supports_*` booleans in `GpuCapabilities` are likewise declared constants, not
queries (`lib.rs:126-135`, `null.rs:62-72`, `vulkan/mod.rs:137-146`).

### 2.3 Ownership / drop order — good

`InstanceGuard`'s `Drop` destroys the instance before unloading the loader
(`mod.rs:100-107`), and the `Arc` makes pipeline/state order irrelevant.
`VulkanState::drop` destroys textures → pipelines → swapchain → render pass →
frames → upload pool → surface → device in a correct order
(`mod.rs:694-731`). Keep this.

### 2.4 Error handling — mixed

`device::result_error` (`vulkan/device.rs:305-320`) is a good mapper, but many
sites bypass it: buffer/image/pool failures collapse to `Unsupported` or
`OutOfMemory` and lose the actual `vk::Result` (`texture.rs:73-88`,
`pipelines.rs:284-301`, `mod.rs:743-770`). Route every Vulkan call through
`result_error` so device-lost/out-of-memory are not mislabelled. `drop_texture`
correctly returns `()` and is silent (`mod.rs:357`, `null.rs`).

### 2.5 Unnecessary clones / allocations

- `vstate.device.clone()` and `vstate.swapchain_loader.clone()` per frame and
  per texture operation (`vulkan/mod.rs:449-450`, `texture.rs`, `:368`) are cheap
  ash handle clones but avoidable with a small local borrow scope.
- `RenderState` boxes `NullState` (`state.rs:19-23`) although it is tiny; keep
  it out of consistency, but it is an unnecessary allocation.
- `VulkanTexture.desc` is stored (`texture.rs:17`) but never consulted after
  creation — either validate re-uploads against it (the null backend does,
  `null.rs:262-266`) or drop the field.
- **ABA key hazard:** `Texture::key` is the `Arc` pointer address
  (`texture.rs:130-134`). If L2 drops a `Texture` handle without calling
  `drop_texture`, the GPU entry stays in `VulkanState::textures`; a later
  allocation can reuse the address and `upload_texture` will silently reuse the
  stale GPU image (`mod.rs:394-402`). A monotonic `u64` id in `TextureInner`
  removes the hazard. Low probability, but it is silent corruption.

### 2.6 Naming and docs — good

`BackendKind::name` (`lib.rs:66-74`), `FrameStats`, and the scene types are
consistently named and documented. `RenderState`/`Surface` are deliberately
opaque and lack `Debug`/`Clone` — fine. Add `#[non_exhaustive]` to the public
config/capability/error-adjacent structs before the API freezes (the core
review already raised this for `RenderError`), and consider
`#![deny(unsafe_op_in_unsafe_fn)]` plus
`#![warn(clippy::undocumented_unsafe_blocks)]` — `unsafe_code` cannot be
forbidden here, but these two catch most of the remaining risk.

---

## 3. Consistency with the other crates / backends

1. **Same free-function facade** — both backends are reached through the same
   `lib.rs` functions, and both traverse with sibling `z` sorting and a
   `MAX_TRAVERSAL_DEPTH` guard (`draw.rs:96`, `null.rs:319`). Good.
2. **Capability drift** — the null backend claims
   `supports_destination_dependent_blend: true` and `supports_offscreen_targets:
   true` (`null.rs:69-70`) while Vulkan reports `false` (`vulkan/mod.rs:142-143`);
   the null backend also does no rendering at all. And Vulkan reports
   `supports_glyph_atlas: true` (`:144`) although per-glyph placement is
   deferred (`draw.rs:181-193`). Make the flags truthful, or document null as
   "accepts every call" and reserve the booleans for real capability queries.
3. **Duplicated traversal/order logic** — `draw.rs:122` and `null.rs:334` each
   clone+sort children per node with the same `map_or(0, |n| n.z)` key, and the
   transform/scissor math lives only in `draw.rs`. Extract the sort key/order
   (and ideally the child iteration) into `scene.rs` so the two renderers cannot
   drift in ordering semantics.
4. **`mip_levels` accepted by both but effective in neither** — `TextureDesc`
   validates `>= 1` and allows more (`texture.rs:47-77`), Vulkan allocates the
   levels and transitions them but uploads only level 0 and caps
   `max_lod = 0.0` (`pipelines.rs:321`). Either reject `> 1` for now or
   implement generation.
5. **Replace/Multiply + opacity semantics** — the L1 tint applies opacity to
   alpha only (`draw.rs:167`, `:198-203`), but the `Replace` pipeline ignores
   source alpha (`ONE`/`ZERO`, `pipelines.rs:194-203`) and `Multiply` multiplies
   color (`DST_COLOR`/`ZERO`, `:228-236`). L2 currently compensates by degrading
   `Replace` to `SourceOver` below full opacity
   (`crates/engine/src/projection.rs:190-193`), but L1 is supposed to own
   composition (§2.5). This needs the signed-off blend-matrix tests
   (`04-renderer-vulkan.md §11.2`).

---

## 4. Elegance / optimization (ranked)

1. **Persistent staging ring instead of per-upload alloc + synchronous wait** —
   **High.** `vulkan/texture.rs:225-340` creates, maps, submits, waits on a
   fence, and destroys a staging buffer for **every** upload
   (`:239` create, `:243` map, `:324` `wait_for_fences`, `:329-332` destroy).
   The design explicitly asks for a persistent ring/batched uploads
   (`04-renderer-vulkan.md §5`, §10). This serializes the CPU and reallocates on
   the hot `drawText`/raster path. Upload into a reused ring and record the
   copies into the frame command buffer (or one batched upload command buffer
   per frame).
2. **Avoid `children.clone()` + sort per node per frame** — **Medium.**
   `draw.rs:122` and `null.rs:334` allocate a `Vec` for every parent every
   frame, then sort it. Because L2 appends in `window_layer_order` / increasing
   `z` (`projection.rs:391-470`), the list is usually already sorted. Track
   whether each node's children are sorted (or sort indices only when needed)
   and iterate borrowed.
3. **Cache `PhysicalDeviceMemoryProperties`** — **Medium.** Remove the
   per-call query at `texture.rs:510` (see 1.10).
4. **Cache scissor state** — **Low.** `cmd_set_scissor` is issued for every quad
   (`draw.rs:262`) even when unchanged. Track the last scissor alongside
   `bound_pipeline`/`bound_descriptor`.
5. **Descriptor allocation strategy** — **Low/Medium.** One descriptor set is
   eagerly allocated per texture and the pool is capped at 1024 sets/descriptors
   (`pipelines.rs:292-302`). Consider a single updateable descriptor as a
   fallback, or at least map pool exhaustion to a typed, actionable error
   instead of `OutOfMemory`.
6. **Fold the affine into the matrix once** — **Low.** `to_ndc`
   (`draw.rs:398-414`), `place_rect`, and `node_spaces` (`:273-285`) build an
   `Affine2` chain per node and then expand it; a single `[f32;16]` multiply
   chain would avoid the intermediate copies (marginal, but it is the only
   per-quad math).
7. **`pipeline_for` direct indexing** — **Nit.** `blend_slot`/`pipeline_for`
   (`pipelines.rs:55-70`, `:169-184`) could index arrays by a `BlendMode`→slot
   table; the match is fine and clear.
8. **Frame-local handle clones** — **Nit.** `device.clone()` at
   `mod.rs:449` etc.; borrow with a scoped block instead.

---

## 5. Deferred / risky items to track

These are explicitly stubbed or unreachable; each should become a tracked work
item (with a test) rather than an implicit gap:

- **Destination-dependent blends** — all 12 modes collapse to `SourceOver`
  (`pipelines.rs:176-181`), contradicting `04-renderer-vulkan.md §4`. The
  `DstBlend` enum is fully populated and mapped by L2
  (`projection.rs:47-104`), so scripts silently lose the effect.
- **Mask clips** — `Clip::Mask(_)` is a no-op that silently keeps the inherited
  scissor (`draw.rs:136`). Documented, but it is a visible correctness gap for
  mask-based transitions.
- **GPU glyph runs** — a `GlyphRun` draws its atlas as a single quad and
  `GlyphPlacement` is never consumed (`draw.rs:181-193`, `scene.rs:63-100`).
  `supports_glyph_atlas` is nonetheless advertised `true`.
- **Mipmaps** — accepted but neither generated nor sampled (`max_lod = 0.0`,
  `pipelines.rs:321`); level memory is wasted.
- **Headless/offscreen targets** — `supports_offscreen_targets: false`
  (`vulkan/mod.rs:143`); textures are `TRANSFER_DST | SAMPLED` only
  (`texture.rs:88-89`), so there is no render-target/readback path yet.
- **GPU transitions** (crossfade/scroll/wave/mosaic) — still completion-callback
  stubs per `04-renderer-vulkan.md §4`.
- **Device-lost recovery** — `RenderError::DeviceLost` is returned
  (`device.rs:308-310`) but no reinitialization path exists; only
  `OUT_OF_DATE`/`SUBOPTIMAL` recreate is wired (`mod.rs:604-620`).
- **sRGB consistency** — the renderer uses sRGB images + linear shader math and
  linearizes solid/clear colours (`draw.rs:376-382`, `mod.rs:974-987`); the
  textured tint is forced white with no per-node tint, and destination blends
  are absent. Validate against `07-roadmap.md §6`.
- **Platform surface entry point** — `begin_render_state` derives a display
  handle only for Android/Win32 (`mod.rs:946-953`), so the Linux/X11/Wayland
  path must use `begin_render_state_with_display`. `crates/app/src/lib.rs`
  calls the former, so the app cannot create a Vulkan surface on Linux as
  written. Consider a single `begin_render_state(display, window)` (or have the
  host always pass both).
- **`render_finished`/`image_available` semaphore lifecycle** — as in 1.4/1.5;
  add a resize/minimize/device-lost stress test (design §11.3).

---

## 6. Top 10 recommended changes

1. **Fix the vertical flip (and align the scissor).** `crates/platform/render/src/vulkan/draw.rs:400` —
   use `b = 2.0 / height` and `-b·ty - 1` so world y-down maps to Vulkan NDC
   (`y=0` → `-1`); re-check `rect_to_scissor` at `:335`. Add a golden-image test.
2. **Fix error-path leaks in `begin_with_display`.** `crates/platform/render/src/vulkan/mod.rs:257`,
   `:259`, `:274` — guard `create_frames`, the `render_finished` collect, and
   `create_upload_resources` with an RAII teardown.
3. **Reject mismatched `Surface`/`RenderState` and tie their lifetimes.**
   `crates/platform/render/src/vulkan/mod.rs:452` (render) and `:331` (resize) —
   compare against `vstate.surface`; give `Surface` ownership or a lifetime tie.
4. **Make the acquired-image semaphore error-safe.** `crates/platform/render/src/vulkan/mod.rs:494-516` —
   any failure after `acquire_next_image` must consume or recreate
   `image_available`.
5. **Replace per-upload staging alloc + fence wait with a persistent ring /
   batched uploads.** `crates/platform/render/src/vulkan/texture.rs:225-340`.
6. **Recreate `render_finished` semaphores per swapchain generation.**
   `crates/platform/render/src/vulkan/mod.rs:922-939` — do not reuse semaphores that
   a `present` may have signalled.
7. **Cache memory properties and use `1u32`.** `crates/platform/render/src/vulkan/texture.rs:503-512`.
8. **Stop cloning+sorting children every node every frame.**
   `crates/platform/render/src/vulkan/draw.rs:122` and `crates/platform/render/src/null.rs:334` —
   iterate borrowed and sort only when dirty/unsorted.
9. **Narrow the public construction surface and honor or remove
   `preferred_format`.** `crates/platform/render/src/lib.rs:58`, `:89`, `:156`,
   `:177`; `crates/platform/render/src/vulkan/mod.rs:121`.
10. **Fix the remaining partial-creation leaks.** `crates/platform/render/src/vulkan/texture.rs:107`
    (image after `find_memory_type`), `:243` (staging buffer after `map_memory`),
    `:362` (`create_staging_buffer`), and
    `crates/platform/render/src/vulkan/pipelines.rs:353` (`create_unit_quad`).

### Honourable mentions

- Make `GpuCapabilities` truthful across backends
  (`crates/platform/render/src/null.rs:69-70` vs
  `crates/platform/render/src/vulkan/mod.rs:142-144`).
- Add `#[non_exhaustive]` to `PipelineConfig`/`GpuCapabilities`/`BackendKind`
  (`crates/platform/render/src/lib.rs:66`, `:83`, `:119`) and
  `#![deny(unsafe_op_in_unsafe_fn)]` + `clippy::undocumented_unsafe_blocks`.
- Replace the pointer-identity `Texture` key with a monotonic id
  (`crates/platform/render/src/texture.rs:130-134`).
- Route all Vulkan failures through `device::result_error`
  (`crates/platform/render/src/vulkan/device.rs:305`).
- Add the compile-time push-constant size assertion
  (`crates/platform/render/src/vulkan/draw.rs:23`).
