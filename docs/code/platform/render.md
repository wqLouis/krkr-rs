# `render` — the stateless render pipeline

Source: `crates/platform/render/src/` · crate `render` · layer **L1**

`render` owns **all** render mechanism: scene-tree traversal, parent
transforms, z-order, opacity composition, clipping, blend-state selection,
batching, texture upload scheduling, offscreen compositing and presentation.
L2 hands it a tree; it draws it.

The renderer design and the blend table are in
[`04-renderer-vulkan.md`](../../../docs.old/migration/04-renderer-vulkan.md) (evidence
trail: [`docs.old/migration/04-renderer-vulkan.md`](../../../docs.old/migration/04-renderer-vulkan.md)).

---

## 1. Responsibility

| concern | file |
|---|---|
| the draw tree the pipeline consumes | `scene.rs` |
| the service object, the state value, the free-function façade | `lib.rs` |
| the headless backend and the CPU software rasterizer | `null.rs`, `cpu.rs` |
| the Vulkan backend | `vulkan/{device,draw,pipelines,shaders,swapchain,target,texture}.rs` |
| texture handles and descriptors | `texture.rs` |
| the Photoshop family's per-channel functions and the program generated from them | `blend.rs`, `blend_shader.rs` |
| offscreen target handles and descriptors | `render_target.rs` |
| glyph rasterization | `glyph.rs` |

---

## 2. Layer and dependencies

| | |
|---|---|
| Layer | L1 (bottom core) |
| Depends on | `api` (L1 DTOs + error enums), `thiserror`, `log`, `ash` (Vulkan), `ash-window`, `raw-window-handle`, `ab_glyph`, `ttf-parser` |
| Must not depend on | `engine` (L2), `compat` (L3), `vm`/`tjs` (L4), a composition root |
| Forbidden | a `&mut self` method on the pipeline, a process-global registry, a draw list built outside the pipeline |
| Lint | `#![warn(missing_docs)]`, `#![deny(unsafe_code)]`, `#![deny(unsafe_op_in_unsafe_fn)]` |

The Vulkan module carries a **file-level** `#![allow(unsafe_code)]`: `ash` is
a raw FFI binding, so the module necessarily contains `unsafe` blocks, and
each one is annotated with the invariant that makes it sound. Every other
module in the crate is entirely safe.

---

## 3. The draw tree

`render::scene` defines the tree L1 consumes and L2 owns. It carries drawable
structure and blend semantics only: no engine state, no script vocabulary.

```rust
pub struct RenderScene {
    pub root: RenderNodeId,
    pub nodes: Vec<RenderNode>,
    pub background: Option<[f32; 4]>,
}

pub struct RenderNode {
    pub parent: Option<RenderNodeId>,
    pub children: Vec<RenderNodeId>,
    pub content: RenderContent,
    pub transform: Transform2D,
    pub dst: RectF,
    pub src: PixelRegion,
    pub opacity: f32,
    pub color: [f32; 4],
    pub visible: bool,
    pub z: i32,
    pub blend: BlendMode,
    pub clip: Option<Clip>,
    pub hold_alpha: bool,
}

pub enum RenderContent { None, Solid([f32; 4]), Texture(Texture), Glyphs(GlyphRun) }
pub enum Clip { Rect(RectF), Mask(Texture) }
pub struct RenderNodeId(pub u32);
```

Two free functions carry the space math, and **both backends call them**, so
the backends cannot drift:

* `node_spaces(node, parent)` returns `(node_space, content_space)`. Because
  `dst` is placed *before* the node transform, a device-space `dst` scales the
  rectangle's origin and its extent together — the property a letterbox
  needs.
* `child_sort_key` / `children_are_sorted` keep the sibling `z` ordering
  identical across backends; the common case (L2 appends children already in
  `z` order) iterates the borrowed slice without a clone and sort.

---

## 4. Public API

The service object is a thin façade over the two backends; the actions are
free functions that take the pipeline plus the caller-owned state.

```rust
pub enum BackendKind { Null, Vulkan }
pub struct PipelineConfig { pub backend: BackendKind, pub application_name: String,
                           pub preferred_format: TextureFormat, pub validation: bool,
                           pub frame_interval: Duration }
pub struct RenderPipeline { /* the backend mechanism */ }
pub struct RenderState { /* L2-owned */ }
pub struct Surface { /* L2-owned */ }

pub fn create_render_pipeline(config: PipelineConfig) -> Result<RenderPipeline, RenderError>;
pub fn presentation_budget(frame_interval: Duration) -> Result<Duration, RenderError>;
pub fn supports_effect_pipelines(p: &RenderPipeline) -> bool;
pub fn begin_render_state(p: &RenderPipeline, window: &RawWindowHandle)
    -> Result<(Surface, RenderState), RenderError>;
pub fn begin_render_state_with_display(p: &RenderPipeline, display: &RawDisplayHandle,
                                       window: &RawWindowHandle)
    -> Result<(Surface, RenderState), RenderError>;
pub fn begin_offscreen_render_state(p: &RenderPipeline)
    -> Result<RenderState, RenderError>;
pub fn resize(p: &RenderPipeline, surface: &Surface, state: &mut RenderState,
              size: (u32, u32)) -> Result<SurfaceEvent, RenderError>;
pub fn create_texture(p: &RenderPipeline, desc: TextureDesc) -> Result<Texture, RenderError>;
pub fn drop_texture(p: &RenderPipeline, state: &mut RenderState, tex: Texture);
pub fn upload_texture(p: &RenderPipeline, state: &mut RenderState, tex: &Texture,
                      region: PixelRegion, pixels: &[u8]) -> Result<(), RenderError>;
pub fn upload_textures(p: &RenderPipeline, state: &mut RenderState,
                        uploads: &[TextureUpload<'_>]) -> Result<(), RenderError>;
pub fn create_render_target(p: &RenderPipeline, desc: RenderTargetDesc)
    -> Result<RenderTarget, RenderError>;
pub fn destroy_render_target(p: &RenderPipeline, state: &mut RenderState, target: RenderTarget);
pub fn create_effect_pipeline(p: &RenderPipeline, state: &mut RenderState,
                              desc: &EffectPipelineDesc<'_>) -> Result<EffectPipeline, RenderError>;
pub fn destroy_effect_pipeline(p: &RenderPipeline, state: &mut RenderState,
                               pipeline: EffectPipeline);
pub fn render_to_target(p: &RenderPipeline, state: &mut RenderState, target: &RenderTarget,
                         scene: &RenderScene) -> Result<(), RenderError>;
pub fn read_render_target(p: &RenderPipeline, state: &RenderState, target: &RenderTarget,
                           region: PixelRegion, out: &mut [u8]) -> Result<(), RenderError>;
pub fn render(p: &RenderPipeline, surface: &Surface, state: &mut RenderState,
              scene: &RenderScene) -> Result<Presentation, RenderError>;
pub fn recover_surface(p: &RenderPipeline, surface: &mut Surface, state: &mut RenderState)
    -> Result<SurfaceEvent, RenderError>;
pub fn last_frame_timings(state: &RenderState) -> Option<FrameTimings>;
```

`render` is the **only** drawing entry point: the pipeline owns the
traversal, the composition, the batching and the presentation, and the caller
only keeps the tree current.

`create_render_pipeline` derives the frame's presentation budget from
`PipelineConfig::frame_interval` through `presentation_budget` (twice the frame
period, checked arithmetic) and bounds the swapchain acquire with it. A zero or
overflowing interval is `RenderError::InvalidArgument`, never a substituted
wait. A frame whose image the display does not release within that budget is
skipped: nothing is submitted, the previous image stays on screen, the
swapchain and its synchronisation objects are untouched, and the result is
`Presentation { presented: false, skip: Some(PresentSkip::AcquireTimeout) }`.
The frame and per-image fence waits stay unbounded — waiting there is GPU
progress, and a wedged device is reported by the driver.

`resize` adopts the new size and rebuilds the swapchain, with two rules that
keep a compositor's layout churn from costing frames:

* A resize that repeats the extent the swapchain already has rebuilds nothing.
  A compositor can report a size it has just applied (a scale or tiling
  rounding included); rebuilding for it cannot change the frame the display
  shows. An unusable swapchain is still rebuilt, because the retry is what
  makes it usable again.
* The swapchain a rebuild replaces is **retired**, not destroyed: the display
  may still be showing its last presented image. A retired swapchain is
  destroyed after a frame of its replacement has been presented (or at
  teardown). A swapchain that never presented has no image on screen and is
  destroyed immediately, and a lost surface destroys every retired swapchain
  before the surface it targets. The swapchain render pass is shared along the
  chain and is destroyed once, with the last swapchain that uses it.

`begin_render_state_with_display` is the complete entry point for X11, XCB
and Wayland: `VK_KHR_surface` needs the platform display connection in
addition to the window handle, and `raw-window-handle` splits the two.

`begin_offscreen_render_state` brings up a **logical device without a
surface**: a graphics queue, no `VkSurfaceKHR` and no swapchain. The state it
returns renders into offscreen targets and reads them back, and it cannot
present a frame — `render`, `resize` and `recover_surface` report
`RenderError::InvalidArgument` for it. It is what makes the GPU path
reachable on a machine that has a GPU but no display, and what lets the GPU
path be compared with the CPU path on one scene.

The Vulkan backend creates the device on its own, with the surface only when
the caller presents to a window: the queue family is chosen for what the
device is for (graphics and presentation for a surface, graphics alone for an
offscreen device), and the swapchain extension is enabled only on the device
that creates a swapchain.

### Textures and targets

```rust
pub enum TextureFormat { Rgba8Srgb, Rgba8Unorm, R8Unorm }
pub struct TextureDesc { pub width: u32, pub height: u32, pub format: TextureFormat,
                         pub mip_levels: u32 }
pub struct Texture { /* an immutable handle */ }
pub struct RenderTargetDesc { pub width: u32, pub height: u32, pub format: TextureFormat }
pub struct RenderTarget { /* an immutable handle */ }
```

A `Texture` or a `RenderTarget` is an **immutable, backend-agnostic handle**:
the CPU/GPU resource it names lives in the L2-owned `RenderState`, keyed by
the handle's identity. Cloning a handle clones the handle, never the image.

The identity comes from a monotonic atomic counter rather than the backing
allocation address, which removes the ABA hazard where a dropped-and-freed
handle could be replaced by a new one at the same address and silently reuse a
stale resource.

### Glyphs

```rust
pub struct FontSystem;                       // the stateless service object
pub struct Font { /* an immutable parsed face */ }
pub fn create_font_system() -> FontSystem;
pub fn load_font(fs: &FontSystem, bytes: &[u8], index: u32) -> Result<Font, GlyphError>;
pub fn rasterize_glyph(fs: &FontSystem, font: &Font, glyph: char, size_px: f32,
                        style: GlyphStyle) -> Result<GlyphBitmap, GlyphError>;
pub fn glyph_advance(fs: &FontSystem, font: &Font, glyph: char, size_px: f32,
                      style: GlyphStyle) -> Result<f32, GlyphError>;
pub fn has_glyph(fs: &FontSystem, font: &Font, glyph: char) -> bool;
pub fn font_data_has_glyph(fs: &FontSystem, bytes: &[u8], index: u32, glyph: char) -> bool;
pub fn font_metrics(fs: &FontSystem, font: &Font, size_px: f32) -> FontMetrics;
```

The rasterization core reproduces the synthetic bold (1 px right dilation plus
the same pixel in the advance) and the synthetic oblique slant (0.25, the
conventional ~14°). Unlike the old atlas, the API returns **per-glyph
coverage positioned by its bearings** instead of packing pixels into a shared
RGBA cell.

A face that does not map a character has no glyph for it: `has_glyph` reports
that, and `rasterize_glyph` and `glyph_advance` raise
`GlyphError::MissingGlyph` instead of substituting another character. Choosing
the face that does map the character is the middle layer's job — the
per-character fallback — and not the rasterizer's.

The bold advance is the same for a measurement and a drawing: `glyph_advance`
and the `advance` field of `rasterize_glyph` both report the face's advance
plus one pixel, so a run that is measured and then drawn lays out the same
way. **Approximation:** the reference draws through FreeType's
`FT_GlyphSlot_Embolden` and measures the emboldened slot afterwards
(`reference/krkr2/cpp/core/visual/FreeType.cpp:675-677` emboldens inside
`LoadGlyphSlotFromCharcode`, and `:620-627` reads
`glyph->metrics.horiAdvance` from it, which is what
`reference/krkr2/cpp/core/visual/FreeTypeFontRasterizer.cpp:139-152` reports as
the width). That embolden grows the advance by `em / 24`, so the port's fixed
one pixel matches a 24 px em exactly and understates the advance for larger
sizes (two pixels at 48 px, three at 72 px).

A non-positive or non-finite `size_px` is `GlyphError::Rasterize` from
`rasterize_glyph` and `glyph_advance`, and the default metrics from
`font_metrics`; a control character advances by `0`; a character the face does
not map is `GlyphError::MissingGlyph`.

`load_font` reads the face's name table as well: `Font::names` returns the
family names the face carries (the typographic family first, then the plain
family, in every language the face declares), decoded per name-table
platform. `font_data_has_glyph` answers the same question as `has_glyph`
straight from the bytes, without parsing the outlines, so a scan over the
host's fonts stays cheap.

The glyph atlas, the layout and every cache are middle-layer state and
deliberately absent here.

### Effect pipelines

```rust
pub struct EffectPipeline { /* an immutable handle */ }
pub struct RenderEffect { pub pipeline: EffectPipeline,
                          pub textures: Vec<Texture>,
                          pub uniforms: EffectUniforms }
```

An **effect pipeline** is a custom-shader program built for one fixed-function
blend. `create_effect_pipeline` validates the descriptor here and in the
backend: the stage code must be SPIR-V this backend accepts, the texture
bindings must fit the API budget, and the blend must be one the backend can
build — an effect program is drawn with fixed-function blending, so a
destination-dependent blend is reported as `RenderError::Unsupported` rather
than substituted.

The handle is immutable and backend-agnostic, like a texture handle: the GPU
resource lives in the L2-owned `RenderState`. The handle may still be
referenced by a `RenderScene` when it is destroyed; such a draw is skipped by
the backend with a warning, exactly like a draw of a released texture.

A draw carries the effect in `RenderContent::Effect(RenderEffect)`: the node's
`dst` places the quad, `opacity` becomes the program's declared opacity word,
and `src`/`blend` are unused — the program's own uniform rectangles select
the texels and the pipeline carries the blend.

The headless (null) backend reports `RenderError::Unsupported` for every effect
pipeline; the Vulkan backend builds them. `supports_effect_pipelines` says
which.

The descriptor and the data vocabulary are the shared DTO crate's
(`api::shader`); the **policy** half — which programs exist and how long they
live — is the middle layer's, and is documented in
[`../engine/shader.md`](../engine/shader.md).

### Backend behaviour and timings

```rust
pub struct FrameTimings { pub acquire_ms: f64, pub present_ms: f64, pub submit_ms: f64,
                          pub recreate_ms: f64, pub fence_wait_ms: f64 }
```

`FrameTimings` records the wall-clock costs of the presentation calls that
complete one frame — the acquire/present queue operations, the fence waits and
any swapchain recreation. The null backend records an all-zero value, so a
caller can rely on the shape without a GPU. `last_frame_timings` returns
`None` for a state that has not presented a frame, so "no measurement yet" is
distinguishable from a measured all-zero frame.

What a backend can do is not a table of flags: a caller learns it by making
the call and reading the typed error. The one question the crate answers
ahead of the call is `supports_effect_pipelines` — whether the backend can run
a custom-shader program at all.

| what | headless backend | Vulkan backend |
|---|---|---|
| an offscreen render target and its readback | the CPU rasterizer serves it | a device image, and `vkCmdCopyImageToBuffer` for the readback |
| a destination-dependent blend | the CPU rasterizer evaluates it | a presented frame draws it with the destination-blend program; an offscreen target has no copy of the destination and refuses it with `Unsupported` |
| an effect pipeline | `Unsupported` — there is no device | built from the SPIR-V modules |
| a GPU glyph run | `Unsupported` — the CPU renderer has no glyph path | `Unsupported` — the backend has no glyph path |

The texture and render-target dimensions have a limit per backend: the
headless backend accepts up to `NULL_MAX_TEXTURE_DIMENSION` pixels, and the
Vulkan backend applies the selected device's own `maxImageDimension2D`. A
descriptor over the limit is `RenderError::InvalidArgument`.

---

## 5. The two backends

| backend | file | role |
|---|---|---|
| **Vulkan** | `vulkan/` | the real renderer (`ash` 0.38, `ash-window`) |
| **Null / CPU** | `null.rs`, `cpu.rs` | the headless backend and the **correctness baseline** |

The traversal is identical in both. `Recorder::visit(id, parent_transform,
parent_opacity, scissor, depth)`:

1. bounds-check the node, respect `visible`, multiply the opacity, skip when
   the composed opacity is ≤ 0;
2. `node_spaces` → `node_space` / `content_space`;
3. intersect the node's clip with the inherited scissor (`Clip::Rect` becomes a
   scissor; `Clip::Mask` is `RenderError::Unsupported`);
4. draw the payload (`Solid`, `Texture` or `Effect`; an unuploaded texture is
   skipped; `Glyphs` is `Unsupported`, and the CPU renderer reports
   `Unsupported` for an effect draw);
5. recurse into the children in `z` order.

Both backends are capped at `MAX_TRAVERSAL_DEPTH` (1024), the crate-root
constant that names the limit for the pipeline as a whole.

The **Vulkan** backend is a private module: the public spelling of a backend
is [`BackendKind::Vulkan`] and the actions in §4, so no consumer can reach
the device handles. The **null** backend is the public `render::null` module,
because its introspection — `NullRenderPipeline::last_frame`,
`texture_pixels`, `texture_uploaded_bytes` — and its
`NULL_MAX_TEXTURE_DIMENSION` cap are part of the headless contract.

The **null** backend has two jobs: it *counts* the draws, and it composes the
scene with the real software rasterizer, which clears to `background`, walks
the tree with the same rules and samples each quad with nearest-texel
filtering. Because a CPU compositor naturally reads the destination, it
evaluates the destination-dependent `BlendMode`s exactly, using the
reference's Photoshop/KiriKiri formulas.

The composed frame is the **presented frame**: the null backend hands it to
the caller through the presented-frame slot, and a windowed run copies it into
the window's pixels with the platform's software presenter. That presenter
talks to the display server directly, so the null backend can show a game
without a GPU. A headless run can write the same frame to a PNG file
(`--dump-frame`), which is how a picture is inspected without a display.

The **Vulkan** backend creates the instance with `VK_KHR_surface` and the
available platform surface extensions (plus the optional validation layer),
creates the surface through `ash-window`, selects a physical device with a
combined graphics+present queue family, and manages the swapchain
(`OUT_OF_DATE`/`SUBOPTIMAL` handled), a ring of staging-buffer upload slots,
textured and solid pipelines with per-blend fixed-function state, and
per-frame command buffers and fences. `MAX_FRAMES_IN_FLIGHT` is 2.

The logical device and the presentation are separate owners. The device owns
the queues, the descriptor resources, the upload ring, the texture table, the
offscreen targets and the readback buffer — everything that does not depend on
a surface. The presentation owns the surface, the swapchain, the swapchain
pipelines and the frame slots, and a state that renders offscreen owns no
presentation at all.

### Blend states

Blend state is not a shader concern; it is a per-pipeline
`VkPipelineColorBlendAttachmentState`. The mapping is:

| `BlendMode` | Vulkan `srcColor` / `dstColor` / `op` |
|---|---|
| `SourceOver` | `SRC_ALPHA` / `ONE_MINUS_SRC_ALPHA` / `ADD` |
| `Replace` | `ONE` / `ZERO` / `ADD` |
| `Additive` | `SRC_ALPHA` / `ONE` / `ADD` |
| `Subtractive` | `SRC_ALPHA` / `ONE` / `REVERSE_SUBTRACT` |
| `AddAlpha` | `ONE` / `ONE_MINUS_SRC_ALPHA` / `ADD` |
| `Multiply` | `DST_COLOR` / `ZERO` / `ADD` |
| `Screen` | `ONE_MINUS_DST_COLOR` / `ONE` / `ADD` |

The destination-dependent modes need to read the destination colour, which
fixed-function blend state cannot express. The Vulkan backend draws them with
a third program, the **destination blend**: it samples the source and a copy of
the destination and mixes them itself, so its fixed-function blending is
disabled and the destination's alpha is held.

The sixteen modes of the **Photoshop family** — the normal, the additive, the
subtractive, the multiplicative, the screen, the overlay, the hard light, the
soft light, the colour dodge, the legacy colour dodge, the colour burn, the
lighten, the darken, the difference, the legacy difference and the exclusion —
run in that program too: their per-channel function reads the destination's
channel, and the family is registered as the HDA variant, so the destination's
alpha is held.

One table of per-channel functions owns the family: `render::blend` holds a
per-channel function and the way its result combines with the source's alpha
for every mode, and both the engine's bitmap path and the GPU's program come
from it. The program's GLSL is **generated** from that table
(`render::blend_shader::dst_blend_shader`, written into `shaders/dst_blend.frag`
by `cargo run -p render --bin write_dst_blend_shader`), so the shader and the
table cannot disagree; a test compares the committed shader with the generated
one.

The functions are whole-byte arithmetic, not floating-point math: the mix's
difference is signed and its shift rounds towards negative infinity, so a
channel below the destination's darkens it.

Three of the family's functions are tables of 256 by 256 entries rather than
arithmetic: the soft light, the colour dodge and the colour burn. The soft
light's entry is the truncated value of `255 · (d/255)^p`, with the exponent
the source's channel selects (`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:530-533`);
the colour dodge's entry is `255` when the source's complement is below the
destination's and the destination scaled by `255 / (255 - s)` otherwise
(`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:534-535`); the colour
burn's entry is zero when the source is below the destination's complement and
`255 - (255 - d) · 255 / s` otherwise
(`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:536-537`). The
overlay's entry is the source's and the destination's product below the
destination's half and `2 · (s + d) - 2 · s · d / 255 - 255` above it
(`reference/krkr2/cpp/core/visual/gl/blend_function.cpp:539-541`).

The tables live in `render::blend::tables` and are the crate's constants: they
are written once by `cargo run -p render --bin write_blend_tables`, committed,
and read by every path that blends — the engine's bitmap path, the software
rasterizer and the graphics program — so a blend means the same thing on the
CPU and on the GPU. A test compares every entry of every table with the
formula it stands for.

The soft light is the one mode whose entries cannot be computed in a shader —
they are the truncated values of a double-precision power — so the program
reads them from the table the pipeline uploads as a storage buffer. The
program's declaration and its reader ask for whole numbers
(`uint soft_light[65536]`), so the pipeline writes the entries as whole
numbers. The colour dodge and the colour burn are whole-byte arithmetic that
a shader can do, so the program still computes them.

The copy is the offscreen-compositing route — render to a target, copy the
target content for the quad's `dst` region into a sampled image, and draw with
a shader variant selected by the `DstBlend` — which is also the mechanism a
future GPU transition would use. The live frame therefore composes into a
target of its own and presents it with a quad: a colour attachment cannot be a
copy's source, so the copy happens between two render passes over the target,
and the pass loads the attachment instead of clearing it.

Every frame slot has a target of its own, because two frames can be in flight
and a frame must not write the target the other one is presenting.

Three base programs are prebuilt (a textured quad, a solid quad and the
destination blend), so a frame never builds pipeline state.

### Descriptor sets

The shaders read their samplers through descriptor sets, and the set and the
binding are named in three places that must agree: the GLSL source under
`shaders/`, the compiled SPIR-V module the crate embeds, and the Vulkan
descriptor set layouts the pipelines are built with. The numbers live in one
place — `shaders::SOURCE_DESCRIPTOR` and `shaders::DESTINATION_DESCRIPTOR` —
and the shader, the layouts and the draw all use them.

| descriptor | set | binding |
|---|---|---|
| the source texture (`u_texture` in the textured program, `u_source` in the destination blend) | 0 | 0 |
| the copy of the destination (`u_destination` in the destination blend) | 1 | 0 |
| the blend tables (`blend_tables` in the destination blend) | 2 | 0 |

The destination-dependent program binds three sets: the source's set, the
destination's set and the tables' set. The first two hold one combined image
sampler each and the third one storage buffer. A shader that named a set and
a binding no layout provides would read a descriptor that does not exist: the
pipeline creation fails on a strict driver, and the draw reads unbound data on
a lenient one.

The scene's sampler is a **point** sampler: it takes the nearest source
texel, the way the classic draw copies the source texel for texel and the
way the software rasterizer takes the nearest source texel, so a magnified
or a minified layer is point-scaled on both. The destination blend reads the
destination's copy through the same sampler, so its per-channel functions see
the destination's own texels and not an interpolation of them. The effect
pipelines choose their own sampler per binding (`SamplerKind`), because their
programs are the transition's and not the scene's.

### Textures and uploads

* L2 owns the CPU bitmap and its dirty flags.
* `upload_texture` writes the dirty region into a transfer-destination image
  through a staging buffer; `upload_textures` batches every upload's
  sub-rectangle into **one** staging pass and one GPU wait, so a frame's
  dirty bitmaps pay a single submit rather than one per texture.
* Sub-rectangle uploads are supported: only part of a bitmap is touched by a
  text draw or a raster operation.
* The colour space is the **encoded** one: the bytes the engine stores are
  the bytes the blend reads, and the result is written back as encoded
  bytes. The images are `Rgba8Srgb` and the backend stores and samples them
  without a transfer function, so the shader blends the encoded values. The
  node's colours, the scene's background and the swapchain take the same
  space, so nothing is linearised on the way in or on the way out. This is
  the space the engine's blends are defined in, so the CPU and the GPU blend
  the same values.
* `drop_texture` is called when L2 drops a bitmap, so the bitmap→texture
  lifetime is explicit L2 state. A handle may still be referenced by a
  `RenderScene`; such a draw is simply skipped.

---

## 6. State ownership

| value | owner |
|---|---|
| the Vulkan instance, the selected physical device | L1 (`RenderPipeline`) |
| the Vulkan logical device, its queues, the descriptor resources, the staging ring, the texture table, the offscreen targets | L2 (`RenderState`) |
| the surface, the swapchain, the swapchain pipelines, the frame slots, the frame timings | L2 (`RenderState`), only for a state that presents |
| the presentation surface handle | L2 (`Surface`) |
| the draw tree | L2 (`RenderScene`) |
| the CPU bitmap contents and the dirty flags | L2 |
| the glyph atlas and the layout | L2 |

`RenderPipeline` has **no `&mut self` method** and remembers nothing between
calls: the same tree plus the same state produces the same frame. Two
independent pipelines can run in one process.

The pipeline has **no scaling policy**. It receives device-space rectangles
and draws them; Fit-with-letterbox is L2's rule, applied during the projection
— see [`../../arch/l2.md`](../../arch/l2.md) §4. Its one presentation rule is
the timeout on the swapchain acquire described in §4: the pipeline never waits
for the display longer than the frame's budget.

---

## 7. Platform gating

None. `render` compiles identically on every target: the Vulkan backend builds
everywhere (the device may be unavailable at runtime, which is a typed
`RenderError::DeviceUnavailable`), and the null backend needs neither a GPU
nor a window.

The Vulkan surface is created from the `RawWindowHandle` the composition root
passes in, so the same crate serves the winit desktop window and the Android
`ANativeWindow`.

---

## 8. Failure modes

Every failure is a typed `RenderError` (the variants are listed in
[api.md](api.md) §7). The cases that matter here:

| failure | behaviour |
|---|---|
| the requested backend is not implemented in this build | `RenderError::Unsupported` |
| the Vulkan loader, instance or device cannot be brought up | `RenderError::DeviceUnavailable`, with the Vulkan result name and description |
| the logical device was lost | `RenderError::DeviceLost` |
| the presentation surface was lost | reported as `SurfaceEvent::Lost` in the returned `Presentation` — **not** as an error; the caller recovers it through `recover_surface` |
| the surface is out of date | `SurfaceEvent::OutOfDate` |
| the display does not release a swapchain image within the presentation budget | `Presentation { presented: false, skip: Some(PresentSkip::AcquireTimeout) }` — **not** an error and no swapchain rebuild; the caller records the skipped frame in its stall and keeps ticking |
| `resize` repeats the current swapchain extent | no rebuild; `SurfaceEvent::Resized` with the extent the swapchain already has |
| a GPU allocation failed | `RenderError::OutOfMemory` |
| the descriptor is empty, oversized or non-RGBA8 | `RenderError::InvalidArgument` |
| the state/surface/target does not belong to this pipeline | `RenderError::InvalidArgument` |
| a presentation action is asked of a state that has no surface | `RenderError::InvalidArgument` — the state renders offscreen and presents nothing |
| the upload region is out of bounds or the pixel slice does not match it | `RenderError::Upload` |
| the readback region is out of bounds or the output buffer is the wrong size | `RenderError::Framebuffer` |
| the shader or the pipeline failed to build | `RenderError::Shader` — the message names the program and the blend slot the pipeline was for |
| the effect pipeline's descriptor breaks the binding or uniform contract | `RenderError::InvalidArgument` |
| the effect pipeline is requested on the headless backend | `RenderError::Unsupported` |
| the effect pipeline's blend is destination-dependent | `RenderError::Unsupported` — a destination-sampling shader is not substituted |

A **lost surface is not an error**: it is an event, and `recover_surface`
rebuilds the surface and the swapchain while everything else in the state is
untouched, so the caller keeps the game and the VM alive across the recovery.

Nothing degrades silently: a mask clip or a glyph run on the Vulkan backend is
`Unsupported`, not source-over. A destination-dependent blend is drawn by the
destination-blend program; the offscreen-target path, which has no copy of the
destination to read, refuses it with `Unsupported`. Every mode of the
Photoshop family is one of those modes, so the offscreen-target path refuses
them as well.

**Nothing in L2 emits an effect draw yet.** The backend can build an effect
pipeline and draw `RenderContent::Effect`, but the projection emits
`RenderContent::Texture` only, so no custom program reaches the screen; the
L2-side registry is documented in [`../engine/shader.md`](../engine/shader.md).

---

## 9. Tests

* `crates/platform/render/tests/glyph.rs` — the rejected garbage font, the
  consistent Latin rasterization, the synthetic bold widening and advance
  bump, the synthetic oblique not narrowing, the positive metrics, the
  control characters and the bad sizes, the mapped glyph without an outline
  keeping its advance, and the glyph-scene construction. A missing system
  font **skips** the test rather than failing.
* `crates/platform/render/tests/blend.rs` — the committed shader being the
  one the table generates, every mode of the Photoshop family blending the
  reference's vectors exactly, every mode having an index of its own, the
  two legacy modes replacing the channel, the hard light being the swapped
  overlay table, and the soft-light table holding every entry.
* `crates/platform/render/tests/effect.rs` — the headless backend reporting no
  effect pipeline and refusing one with `Unsupported`, and the Vulkan cases
  building and destroying one, refusing a bad module, a destination-dependent
  blend and a broken binding set, and compositing a cross-fade (each skipping
  on a machine with no device).
* `crates/platform/render/tests/null.rs` — the null pipeline round trip
  counting the draws, and the partial upload touching only its region.
* `crates/platform/render/tests/pipeline.rs` — the default headless
  configuration, the Vulkan backend being either available or reporting
  `Unsupported`, and the presentation budget: two frame periods, a zero frame
  interval and an overflowing one each rejected as `InvalidArgument` rather
  than substituted.
* `crates/platform/render/tests/scene.rs` — the scene construction linking the
  parent and the children.
* `crates/platform/render/tests/target.rs` — the null offscreen target
  reading back the clear colour, the solid fill and the partial readback, the
  textured quad, the node tint, source-over honouring the source alpha, the
  destination-dependent blends sampling the destination, the fixed-function
  blends matching the baseline, the descriptor validation, the rejected
  mismatched buffer and region, the dropped target releasing its pixels, the
  Vulkan offscreen target refusing a destination-dependent blend (or a skip),
  and the Vulkan offscreen state compositing the CPU path's scene without a
  window (or a skip).
* `crates/platform/render/tests/texture.rs` — the upload region and length
  validation, and the rejected invalid descriptors, among them a descriptor
  over the headless backend's dimension limit.
* `crates/platform/render/src/lib.rs` `mod tests` — the batch upload applying
  every region, and the null frame timings recorded as zero.
* `crates/platform/render/src/vulkan/pipelines.rs` `mod tests` — every
  destination-dependent blend having a mode index of its own, so the
  destination-blend program cannot draw one mode's function for another.
* `crates/platform/render/src/vulkan/shaders.rs` `mod tests` — every shader's
  GLSL source, its compiled SPIR-V module and the descriptor set layouts
  naming the same descriptor sets and bindings, and every shader in the
  directory being covered by that comparison.
* `crates/platform/render/src/vulkan/mod.rs` `mod tests` — the Vulkan upload
  batch being **one** submit (or a skip on a machine with no surface). The
  assertion lives inside the crate because the submit counter is a
  test-only probe, not a public counter.

---

## 10. Related documents

| topic | document |
|---|---|
| the L1 overview | [README.md](README.md) |
| L1 as an architecture | [`arch/l1.md`](../../arch/l1.md) §3 |
| the renderer design and the blend table | [`04-renderer-vulkan.md`](../../../docs.old/migration/04-renderer-vulkan.md) · snapshot [`docs.old/migration/04-renderer-vulkan.md`](../../../docs.old/migration/04-renderer-vulkan.md) |
| the projection that builds the tree | [`../engine/projection.md`](../engine/projection.md) |
| the VM ↔ device pixel mapping | [`11-presentation-and-pixel-translation.md`](../../../docs.old/migration/11-presentation-and-pixel-translation.md) §6 |
| the L3 ↔ L2 protocol for the render path | [`14-l3-l2-protocol.md`](../../../docs.old/migration/14-l3-l2-protocol.md) |
| the typed-error rule | [`arch_rules.md`](../../rules/arch_rules.md) §6 |
| the Rust-level checklist | [`code_styles.md`](../../rules/code_styles.md) |
