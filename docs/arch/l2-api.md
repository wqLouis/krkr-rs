# L2 — the engine's public API

Status: **authoritative** for the `engine` crate's public surface. Describes
`crates/engine` as it is today.

This page is the reference for the surface of the `engine` crate: the types a
caller names, the operations it calls, the errors it handles, and the rules
that hold for each subsystem. It is written for a reader who has not seen
this repository before and who wants to know two things — what the crate
offers, and whether it could be replaced.

The crate is the middle layer of the engine: it owns every evolving value the
game has (the layer tree, the storage namespace, the mixer, the video
overlay, the timer registry, the input state, the text engine) and performs
actions on the stateless platform services underneath it. It speaks no
script vocabulary: every name a script sees is expressed by the compatibility
layer on top of the generic values documented here. The architecture behind
these pages is in [`l2.md`](l2.md); the layer rules are in
[`../rules/arch_rules.md`](../rules/arch_rules.md) and
[`../rules/api_rules.md`](../rules/api_rules.md).

---

## 1. How to read this page

### 1.1 The crate's shape

`crates/engine` is one crate with one module per subsystem. It depends on
the platform **type** crates — `api`, `fs` and `render` — and on no
render or media backend, no platform crate and no script VM. It is
`#![forbid(unsafe_code)]` and `#![warn(missing_docs)]`.

| module | source | what it owns |
|---|---|---|
| `engine` | `crates/engine/src/engine.rs` | the frame-owning object: the scene, the projection, the textures, the timers, the presentation |
| `clock` | `crates/engine/src/clock.rs` | the frame clock: the time base the frame advances and the script reads |
| `scene` | `crates/engine/src/scene.rs` | the logical layer tree: windows, layers, bitmaps, fonts, focus domains |
| `imagedecode` | `crates/engine/src/imagedecode.rs` | the deferred image decode pool and the pending-image future |
| `projection` | `crates/engine/src/projection.rs` | the pure field copy from the logical tree to the render tree |
| `translate` | `crates/engine/src/translate.rs` | the VM-pixel ↔ device-pixel mapping |
| `layer_ops` | `crates/engine/src/layer_ops.rs` | in-place CPU pixel operations on bitmaps: fills, blits, blur, subtree compositing |
| `raster` | `crates/engine/src/raster.rs` | the CPU shape rasterizer: polygons, strokes, hatches, arcs, béziers |
| `text` | `crates/engine/src/text.rs` | shaping, layout, coverage rasterization, the glyph atlas |
| `storage` | `crates/engine/src/storage.rs` | the storage namespace: mounts, resolution order, reads, listings |
| `startup` | `crates/engine/src/startup.rs` | the startup-script discovery |
| `sound` | `crates/engine/src/sound.rs` | the sound model and the software mixer |
| `video` | `crates/engine/src/video.rs` | the video-overlay playback machine |
| `trans` | `crates/engine/src/trans/` | the transition state machine and the `extrans` providers |
| `shader` | `crates/engine/src/shader/` | the shader programs, their pipelines and the effect draw they are drawn with |
| `timer` | `crates/engine/src/timer.rs` | the timer registry |
| `input` | `crates/engine/src/input.rs` | the generic input state |
| — | `crates/engine/src/hasher.rs` | the fixed-seed hash map the crate's lookup tables are keyed with |

### 1.2 The re-export policy

The crate root is a **curated surface**: it re-exports the names a consumer
reaches through `engine::`, and nothing else. Every other item stays
reachable through the module that owns it, so the flat root never becomes an
accidental copy of the crate's whole API. A flat glob re-export is rejected
by the `house-style` gate.

These are the crate root's re-exports, complete:

| group | items at the crate root |
|---|---|
| the clock | `FrameClock`, `FrameTimeSource`, `InjectedFrameTime` |
| the frame | `Engine`, `EngineError`, `EngineOverlay`, `EngineParts`, `FrameReport`, `GameFrame`, `ObjectCounts` |
| the decode pool | `DecodePool` |
| the mapping | `PixelTranslation`, `TranslateError` |
| the lookup map | `Map` |
| the archive naming rule | `normalize_in_archive_name` |
| the scene | `Scene`, `LayerState`, `BitmapState`, `FontState`, `Rect`, `TransitionFrame`, `TransitionSource`, `DecodedImageKey`, `DecodedImageMode` |
| the projection | `ComposedLayer`, `compose_layer`, `project_scene`, `project_window`, `project_window_report`, `resolve_blend` |
| the storage namespace | `Storage`, `Location`, `StorageReadError` |
| the startup discovery | `discover_startup`, `StartupError` |

Everything else lives under its module path:

| item | path |
|---|---|
| the window state, the hit candidate, the report of the projection | `engine::scene::WindowState`, `engine::scene::HitCandidate`, `engine::projection::ProjectionReport`, `engine::projection::project_scene_report` |
| the mounting and the read-pool types | `engine::storage::MountError`, `engine::storage::MountOptions`, `engine::storage::PreloadError`, `engine::storage::StorageMetadata`, `engine::storage::StartupPlan`, `engine::storage::StorageReadPool` |
| the startup result | `engine::startup::StartupScripts`, `engine::startup::StartupScript` |
| the shader surface | `engine::shader::ShaderLibrary`, `engine::shader::ProgramDesc`, `engine::shader::Program`, `engine::shader::ShaderId`, `engine::shader::PipelineId`, `engine::shader::EffectDraw`, `engine::shader::EffectPass`, `engine::shader::EffectTexture`, `engine::shader::EffectPipelines`, `engine::shader::LoadedProgram`, `engine::shader::ShaderError` |
| the pixel primitives, the blend numbering | `engine::layer_ops`, `engine::raster` |
| the text, the sound, the video, the transition, the input, the timer | `engine::text`, `engine::sound`, `engine::video`, `engine::trans`, `engine::input`, `engine::timer` |

### 1.3 What "public" means here

There are four surfaces in this tree, and this page describes one of them:

| surface | where it lives | how it is frozen |
|---|---|---|
| the script surface | the classes and members the compatibility host registers on the VM | the committed dump `docs/surface/runtime_surface.json`, checked by the `surface-lock` gate check |
| the L3 ↔ L2 protocol | `crates/compat/src/engine_api.rs` | the seam module plus the protocol document; the `layering` gate check enforces that it is the only place the compatibility layer names the engine |
| **the L2 public API** | **the `engine` crate** | **by review and audit against the layer rules — not by a snapshot** |
| the L1 public API | `api`, `fs`, `media`, `psd`, `render`, `platform` | by review; an L1 type may flow into an `engine` signature, which is accepted |

So the `engine` crate's public items are a **reviewed contract**: a new
function, method or variant is fine, and a removal, a changed signature or a
changed meaning needs a reviewed change. There is no public-API snapshot tool
in the gate, and none is wanted. §9 states the contract in full.

"Public" here also means *reachable*: an item is public when it is reachable
from outside the crate, whether through the crate root or through its
module. Items that only the compatibility layer needs are `pub(crate)` in
the engine and cross the layer boundary through the seam (§6).

### 1.4 This page and the code map

| | this page | [`../code/engine/`](../code/engine/README.md) |
|---|---|---|
| question | *what can I call, and what does it do when it fails?* | *what does this module own, and why is it shaped that way?* |
| unit | the subsystem, as a caller sees it | the module, as the tree sees it |
| content | the types, the operations, the error types, the invariants | the state, the data flow, the failure modes, the tests |
| upkeep | changes when the surface changes | changes when the implementation changes |

The code map is the raw material for this page and the place to look for the
data flow and the tests behind each subsystem. Each subsection in §5 names
its code map page.

---

## 2. The frame entry points

This is the page a composition root reads first. The `Engine` owns the
scene, the projected tree, the texture table, the timer registry, the input
state, the clock and the presentation; it advances them one frame at a time.

### 2.1 The types

| type | what it is |
|---|---|
| `Engine` | the frame-owning object. Every value the middle layer needs is a field of it, so two engines can run in one process. |
| `EngineParts` | the bottom objects `Engine::new` assembles: the render pipeline, the surface, the render state, the initial surface size, the headless flag and the optional overlay. The caller creates them. |
| `GameFrame` | the **single upward edge** of the frame. The layer above implements it: it runs the script tick and the event dispatch, and returns a failure as a message. |
| `EngineOverlay` | the render-only debug overlay: a node drawn above every layer, refreshed once per frame. It is not part of the scene and owns no bitmap a script could reach. |
| `FrameReport` | what one frame did: the frame index, the clock, whether the frame was presented, the backend counters and the per-phase millisecond costs. |
| `FrameTimeSource` | the trait the frame's elapsed time comes through. The engine owns the clock it advances; the caller owns where the elapsed time comes from. |
| `InjectedFrameTime` | the frame time the caller injects. The windowed loop writes the real time the platform measured into it; the headless runner writes the time it measured between its frames. |
| `EngineError` | the errors a frame or a surface action surfaces. |
| `ObjectCounts` | the live object counts (windows, layers, bitmaps, fonts, textures) the overlay is refreshed with. |

### 2.2 What the composition root calls, in order

| step | call | what it does |
|---|---|---|
| 1 | build the L1 objects, then `Engine::new(parts)` | the engine takes the pipeline, the surface and the render state; it clamps the surface size to at least 1×1, performs the one surface action it does before its first frame, adopts the extent the backend actually took, and fits the presentation. Returns `Result<Engine, EngineError>`. |
| 2 | `Engine::install_decoder(decoder, config)` | installs the image codec and starts the decode pool. Optional: without it the engine has no deferred decodes. |
| 3 | `Engine::install_storage_reader(reader, config)` | installs the storage reader and starts the storage read pool, and hands the pool back so the storage and the engine share it. |
| 4 | `Engine::load_shader(desc)` / `Engine::effect_pipeline(shader, blend)` | register a shader program and create the backend pipeline for a (program, blend) pair. The engine registers its own two programs at construction when the backend can run effect pipelines. |
| 5 | mount the storage, then `Storage::startup_plan()` | the namespace and the startup plan: the four startup script names, resolved. `engine::startup::discover_startup(&storage)` answers the same question with resolved locations. These live outside the `Engine`: the storage is a value the root owns. |
| 6 | attach the L3 handles | the compatibility layer attaches the scene, the storage, the input state, the timer registry and the clock. The attached values must outlive the binding. |
| 7 | **one frame**: `Engine::run(clock, hooks)` | the loop-level entry point. The caller chooses where the elapsed time comes from; every timeline the frame derives comes from that one value. |
| 8 | `Engine::resize(size)` | rebinds the surface to a new framebuffer size. The logical screen is untouched: the presentation refits the screen into the new framebuffer, so an OS resize moves no value a script observes. |
| 9 | `Engine::take_window_resize()` | hands the root the script-side window size it still has to apply to the OS window, and clears it. |

Two alternative frame entry points exist for callers that drive the frame
differently:

| call | when it is used |
|---|---|
| `Engine::step(dt_ms, hooks)` | the caller has a delta. The engine **adds** the full float delta to its own clock, keeping the sub-millisecond part: sixty `step(16.6)` calls advance the clock by 996 ms rather than 960. A negative delta counts as zero. |
| `Engine::frame(now_ms, hooks)` | the caller has an absolute time. The engine **sets** the clock to that value, discarding any fractional remainder, and the delta handed to the hook is the distance from the previous frame's clock (zero on the first frame). A test drives frames this way. |

All three return `Result<FrameReport, EngineError>`.

### 2.3 What one frame does, in order

The frame contract is fixed. `Engine::run`/`step`/`frame` all end in the
same sequence:

1. record the frame's clock;
2. fit the presentation — refit the VM↔device mapping, and observe the window
   client size;
3. fire the due timers (`timer::deliver_due`);
4. run the upward hook, `GameFrame::before_render(now_ms, dt_ms)`;
5. fit the presentation again — the hook may have resized a window's primary
   layer;
6. **if the sizes cannot be fitted**, count the frame as not presented, skip
   everything that draws, and return a report with no statistics;
7. refresh the render-only overlay;
8. upload the dirty bitmap regions, and prune the textures whose bitmap is
   gone;
9. project the scene when its structure revision changed (or the
   presentation or the texture table invalidated the projection);
10. run `GameFrame::after_project(&drawn_layers)`;
11. fit the presentation again, upload again and project again if the hook
    changed the scene's structure;
12. **one** render call;
13. recover a lost surface, record the frame in the presentation stall, bump
    the frame counter and return the report.

A hook that changes the scene in `after_project` is followed by a second
upload and projection, so its change is in the frame being drawn. The
projection is rebuilt **in place**: the node list is cleared and refilled.

### 2.4 The report and the errors

`FrameReport` carries:

| field | meaning |
|---|---|
| `frame_index` | the number of frames drawn so far, counting this one |
| `elapsed_ms` | the frame clock this frame ran at, with the fractional part for a clock advanced by `run`/`step` and without it for a clock set by `frame` |
| `presented` | whether the frame reached the display. A frame with `presented == false` updated the scene but the backend kept the previous surface contents; the engine reports it and counts it into the current presentation stall |
| `stats` | the null backend's per-frame counters, `None` on a GPU backend |
| `sync_ms` | the milliseconds spent syncing dirty bitmaps to textures |
| `upload_ms` | the part of `sync_ms` spent uploading |
| `prune_ms` | the part of `sync_ms` spent dropping textures whose bitmap is gone |
| `project_ms` | the milliseconds spent projecting the scene tree |
| `render_ms` | the milliseconds spent in the pipeline's render call |

`EngineError` has three variants, and the enum is `#[non_exhaustive]`:

| variant | meaning |
|---|---|
| `EngineError::Render(RenderError)` | a bottom-layer render action failed. The `RenderError` is the L1 error, carried unchanged. |
| `EngineError::Overlay(String)` | the overlay could not be refreshed. The frame is abandoned without projecting or drawing. |
| `EngineError::Frame(String)` | the frame hook reported a failure. The frame is abandoned without projecting or drawing. |

A failure that does not stop the frame — a bitmap with a zero dimension, a
texture the backend refuses to create, a surface that was lost, a display that
did not release a swapchain image — is reported, counted or warned about,
never turned into a substitute value.

The code map for this module is [`../code/engine/engine.md`](../code/engine/engine.md);
the clock is [`../code/engine/clock.md`](../code/engine/clock.md).

---

## 3. The state the engine owns

### 3.1 The scene

`engine::scene::Scene` is the whole logical scene: the windows, the layers,
the bitmaps and the fonts, with every property a script can set on them. It
is generic state — no blend numbering, no archive-path spelling, no script
tags. The compatibility layer writes the script-shaped properties onto these
values; the projection reads them back.

`Scene` is `#[non_exhaustive]`. Its public fields are:

| field | meaning |
|---|---|
| `windows: Vec<WindowState>` | one entry per window, in creation order |
| `layers: Vec<LayerState>` | one entry per layer, in creation order |
| `bitmaps: Vec<BitmapState>` | one entry per bitmap, in creation order |
| `fonts: Vec<FontState>` | one entry per font, in creation order |
| `composite_transition_frames: bool` | the one host-selected field: whether a running transition draws from the layer's transition frame (the projection composites it) or is blended into the layer's display bitmap on the CPU |
| `effect_pipelines: EffectPipelines` | the backend handles of the programs the layers' custom-shader draws name |

The four vectors may be edited directly by a caller; the id lookups then
fall back to a linear scan and still return the correct result.

### 3.2 The state types

| type | where it lives | what it holds |
|---|---|---|
| `Scene` | `engine::Scene` | the whole tree, plus the decoded-image cache, the pending images, the id→index tables and the two revisions |
| `WindowState` | `engine::scene::WindowState` | the window id and title, the client size, the visibility and the opacity, the primary layer, the focus domains and their focus, and the window's layer list |
| `LayerState` | `engine::LayerState` | the id, the owning window, the parent, the focus domain, the children, the main image, the rect, the visibility, the opacity, the blend mode, the sibling sort key, the image placement, the province plane, the hit-test properties, the clip rect, the focus properties, the cache slots (`cache_bitmap`, `cache_content`, `transition_self`, `transition_frame`, `effect`, the cache counters), the font, the hint properties and the lifetime flags |
| `BitmapState` | `engine::BitmapState` | the id, the dimensions, the straight-alpha RGBA8 pixels, the storage name it was loaded from, the dirty flag and the dirty region, the script-ownership flag, the content-generation counter and the pending decode |
| `FontState` | `engine::FontState` | the id, the requested face, the requested height, the colour and the style flags |
| `Rect` | `engine::Rect` | a window-coordinate rectangle: `x`, `y`, `w`, `h` |
| `TransitionFrame` | `engine::TransitionFrame` | the frame a composited transition draws on a layer: its pixel size, whether it covers the layer's whole rect, and its two sources |
| `TransitionSource` | `engine::TransitionSource` | one source of a transition frame: the bitmap it samples, the sub-rectangle it reads, the rectangle it occupies in the frame and its opacity |
| `DecodedImageKey` | `engine::DecodedImageKey` | the cache key of one decoded image: the normalized storage name, the colour-key index, the load variant and the desired size |
| `DecodedImageMode` | `engine::DecodedImageMode` | the load variant: `Full` (a full-colour RGBA load) or `Province` (a province plane read from the decoded image's red channel) |

### 3.3 The rules that hold for the state

* **Every value is a field of an object.** There are no globals, no statics,
  no `thread_local!` and no process-wide registries in the crate, so two
  engines can run in one process without interacting.
* **Ids are stable** for the lifetime of the record. An operation on an
  unknown id returns `None`, `false` or is a no-op; it never panics and
  never creates a record implicitly.
* **Two revisions** tell a structural change from a content change.
  `Scene::structure_revision()` moves on every change the projection reads:
  window and layer creation and removal, sibling order, parenting, the
  geometry and the draw properties, and bitmap creation, removal and resize.
  `Scene::content_revision()` moves on every bitmap content change. A
  bitmap content write does **not** move the structure revision, so an
  animated frame paints without re-projecting.
* **The scene owns the pixels.** A bitmap's `rgba` buffer is
  `width * height * 4` straight-alpha RGBA8 bytes. The pipeline uploads it;
  the pixel operations write it.
* **A pending bitmap carries the probed size and no pixels.** The size comes
  from the header probe, so it is known before the decode finishes. A read
  of the pixels waits for the resolution.
* **A failed decode installs a blank, not an exception.** The bitmap keeps
  the probed size and every pixel becomes transparent, and the cause is
  logged. The load that queued the decode already returned, so there is no
  call to fail.
* **A script-owned bitmap outlives the layer that shows it.** The
  `script_owned` flag marks a bitmap a script object owns.
* **The lookup tables are keyed with a fixed-seed hasher.**
  `crates/engine/src/hasher.rs` builds the crate's maps with fixed seeds,
  so the tables key deterministically across runs; `engine::Map` is the
  map alias those tables use.

The code map for this module is [`../code/engine/scene.md`](../code/engine/scene.md).

---

## 4. The projection seam

The middle layer owns the logical tree; the render pipeline underneath
consumes an abstract draw tree and owns traversal, batching, blending and
drawing. `engine::projection` is the single seam between the two: a pure
field copy that turns one window (or a whole scene) into a draw tree with
one node per **drawn** layer.

### 4.1 The operations

| operation | signature | what it returns |
|---|---|---|
| `project_scene` | `project_scene(scene: &Scene, texture: F) -> RenderScene` | the whole scene as a draw tree |
| `project_scene_report` | `project_scene_report(scene: &Scene, texture: F) -> ProjectionReport` (`engine::projection::project_scene_report`) | the draw tree plus the ids of the layers that contributed pixels |
| `project_window` | `project_window(scene: &Scene, window: u32, texture: F) -> RenderScene` | one window as a draw tree |
| `project_window_report` | `project_window_report(scene: &Scene, window: u32, texture: F) -> ProjectionReport` | one window's draw tree plus its drawn-layer ids |
| `compose_layer` | `compose_layer(scene: &Scene, layer_id: u32) -> Option<ComposedLayer>` | the standalone form of the composition the pass does inline: the layer's absolute rectangle, its composed opacity, its composed visibility and its clip |
| `resolve_blend` | `resolve_blend(blend: BlendMode, composed_alpha: f32) -> BlendMode` | the standalone form of the blend rule |

`F` is a texture lookup closure, `FnMut(u32) -> Option<Texture>`: the
projection asks for the texture of a bitmap id and gets `None` when the
bitmap has none. The same function therefore serves a live pipeline and a
test double.

`ProjectionReport` (`engine::projection::ProjectionReport`) holds the
`scene: RenderScene` and the `drawn_layers: Vec<u32>`.

`ComposedLayer` holds `rect: Rect`, `opacity: f32`, `visible: bool` and
`clip: Option<Rect>`.

### 4.2 What L2 decides and what L1 decides

| the projection does | the pipeline does |
|---|---|
| resolve each drawn layer's absolute rectangle through its ancestor chain | traverse the tree |
| compose the opacity (the product of the layer and every ancestor, each clamped to `[0, 1]`) and the visibility | batch the nodes |
| intersect the layer's rect with every ancestor's rect and its own clip rect | select the blend state |
| resolve the image placement into a destination rectangle and a source sub-rectangle | draw |
| pass the layer's generic `BlendMode` through, except that an opaque `Replace` below full composed opacity degrades to `SourceOver` | |
| skip a subtree that a cached ancestor already covers, and report the layers that were drawn | |
| emit one node per drawn layer, in parent-first order, and link it to its nearest drawn ancestor | |

The projection is a **field copy**: the output is in VM pixels, every node
carries an identity transform, and the tree carries drawable structure and
blend only.

Two rules govern what is drawn:

* **A layer is skipped** when it is hidden anywhere in its chain, has no
  texture, is fully clipped away, or sits under a layer whose cache bitmap
  already contains its pixels. A skipped layer is absent from
  `drawn_layers`, which makes the skip visible rather than silent.
* **A transition frame is two nodes of one layer.** The layer id is reported
  once; the second source's node is the layer's nearest node for its
  children.

### 4.3 What crosses into L1

| item | what it is |
|---|---|
| `RenderScene` | the draw tree: the root node, the node list and the background |
| `RenderNode` | one drawable: its content, its destination rectangle, its source rectangle, its transform, its blend mode and its z order |
| `RenderState` | the GPU and staging bookkeeping for the surface; it is an explicit `&mut` value the caller owns |
| `Surface` | the presented surface the pipeline renders into |
| `Texture` | an uploaded bitmap texture |

The projection's output is **not** on the compatibility layer's path: the
compatibility layer never sees the draw tree, the render state or the
surface. The engine puts the Fit presentation transform on the draw tree's
root and hands the tree to the pipeline in one call.

The code map for this module is
[`../code/engine/projection.md`](../code/engine/projection.md).

---

## 5. The subsystem surfaces

Each subsection names the types, the operations a host or the compatibility
layer performs, the error type, the invariants and the code map page.

### 5.1 clock — `engine::clock`

| item | what it is |
|---|---|
| `FrameClock` | the engine's time base: the elapsed milliseconds since the engine started, with the fractional part kept |
| `FrameTimeSource` | the trait the frame's elapsed time comes through: `elapsed_ms(&self) -> f64` |
| `InjectedFrameTime` | the value the caller injects: a public `elapsed_ms: f64` field and `InjectedFrameTime::new(elapsed_ms)` |

`FrameClock`'s operations: `new()`, `now_ms() -> u64` (the whole
milliseconds — the value the timer registry, the upward hook and the script
see), `now_f64() -> f64`, `advance(dt_ms: f64) -> u64` and
`set(now_ms: u64)`.

Invariants: the fractional part is kept, so a delta shorter than a
millisecond adds up over the frames; the clock never runs backwards (a
negative delta counts as zero); the clock never wraps (it saturates at the
integer ceiling); the script and the frame share one clock.

**Error type:** none. The clock cannot fail.

Code map: [`../code/engine/clock.md`](../code/engine/clock.md).

### 5.2 timer — `engine::timer`

| item | what it is |
|---|---|
| `TimerId` | the monotonic id of a registered timer; it equals the creation order |
| `TimerCallback` | the stored callback shape: `Box<dyn FnMut(TimerId, u64) + 'static>`, the second argument being the fires this delivery replays |
| `TimerState` | the registry: the timers, the next id and the injected `now_ms` |
| `TimerError` | the registry's failures |

Operations: `TimerState::new()`, `create_interval(interval_ms, callback) ->
Result<TimerId, TimerError>`, `remove(id)`, `set_interval(id, interval_ms)`,
`set_enabled(id, enabled)`, and `TimerState::advance(now_ms) -> u64`
(returns the number of fires handed to the callbacks; crate-private — the pump's public
face is the free function `timer::deliver_due(registry, now_ms) -> u64`).
Assigning the registry's public `now_ms` field injects the current time
**without** delivering.

Invariants: a timer fires once per elapsed interval — a timer whose
deadline passed by `n` intervals reports `n + 1` fires in that advance and is
re-armed from the deadline it missed, so the schedule does not drift
(`reference/cpp/core/utils/impl/TimerImpl.cpp:118-148`) — and the whole
replay is one callback invocation carrying the count, so the registry has no
backlog cap: the delivery above it decides how many postings the run makes; a
timer whose schedule was mutated between collection and delivery is skipped;
the interval is at least 1 ms; enabling arms at `now + interval_ms`; delivery
order is creation order; the delivery collects the due timers before it
invokes any of them, so a callback may re-enter the registry; a callback that
panics leaves the registry consistent and the panic propagates.

**Error type:** `TimerError` — `UnknownTimer(TimerId)` for an id that is
not registered, `InvalidInterval` for a zero interval.

Code map: [`../code/engine/timer.md`](../code/engine/timer.md).

### 5.3 trans — `engine::trans`

| item | what it is |
|---|---|
| `Trans` | one transition: the kind, the duration, the elapsed time, the phase, the rule and the completion flag |
| `TransKind` | the kinds: `CrossFade`, `Universal { vague }`, `Mask`, `Scroll { from, stay }`, `Wave`, `Mosaic`, `Turn`, `RotateZoom`, `RotateVanish`, `RotateSwap`, `Ripple` |
| `ScrollFrom` / `ScrollStay` | the scroll vocabulary: `Left`, `Top`, `Right`, `Bottom`; `NoStay`, `StayDest`, `StaySrc` |
| `RuleMask` | the decoded grayscale rule image, tiled over the source |
| `TransStep` | what one step produced: the normalized progress, the integer phase and the completed flag |
| `TransError` | the transition's failures |

Operations: `Trans::new(kind, duration_ms)`, `Trans::with_rule(kind,
duration_ms, rule)`, the named constructors `cross_fade`, `universal`,
`mask` and `scroll`; the state readers `kind`, `duration_ms`,
`elapsed_ms`, `progress`, `phase`, `phase_max`, `is_complete`,
`rule`; the drivers `step(dt_ms)`, `seek_ms(elapsed_ms)`,
`start_at(tick)`, `advance_to(tick)`, `complete()`, `reset()`; and the
three rendering forms `apply(dst, src1, src2, region)`,
`transition_frame(src1, src2, size, covers_layer)` and
`effect_draw(src1, src2, size, covers_layer)`.

A kind is composited in one of three ways, and the host picks the first one
it can serve: a **custom-shader draw** (`CrossFade` and `Scroll`), a
**composited frame** (the same two kinds), or a **CPU blend**
(`Universal`, `Mask` and the seven `extrans` kinds).

Invariants: the phase runs from 0 to `PHASE_MAX` (255) and is computed as
`elapsed × phase_max / duration`, truncated and clamped; a completed
transition stays complete; a non-finite or non-positive delta leaves the
state unchanged; the phase follows the clock, not the frames — `advance_to`
differences the tick against the transition's start tick; a rule is required
by `Universal` and `Mask` and rejected by the other kinds; the rule is
tiled; `phase_max` is `PHASE_MAX + vague` for `Universal`.

**Error type:** `TransError` — `InvalidDuration`, `InvalidVague`,
`MissingRule`, `UnexpectedRule`, `InconsistentRule`, `SizeMismatch`,
`InvalidOption`. Nothing is clamped to a fallback.

Code map: [`../code/engine/trans.md`](../code/engine/trans.md).

### 5.4 input — `engine::input`

| item | what it is |
|---|---|
| `InputState` | the per-frame input state: pointer, buttons, wheel, keys, touches and the text-input buffer |
| `TouchContact` | one touch: the latest touch point, the position the touch started at and the previous position. The per-button and per-key state is opaque: the readers below report it |

Operations: the frame protocol `begin_frame()`, `end_frame()`,
`frame()`; the feeding operations `apply(&InputEvent)`, `set_pointer`,
`set_button`, `set_button_raw`, `set_key`, `add_wheel`,
`warp_pointer`, `take_warp_request`, `set_click_sequence`; and the
readers — `pointer`, `pointer_inside`, `is_button_down`,
`is_button_released`, `button_hold_frames`, `button_click_count`,
`button_last_click_pos`, `clear_click_sequence`, `wheel`, `is_key_down`,
`is_key_released`, `key_hold_frames`, `last_pressed`, `held_keys`,
`released_keys`, `text_input`, `touch_point_count`, `touch_point`,
`touch_contact`, `touch_contacts`, `touch_velocity`, `touch_speed`.
The constructors are `new()` and `with_click_sequence(frames, max_move)`;
the constant `DEFAULT_CLICK_SEQUENCE_FRAMES` is the default click window, in
frames.

Invariants: the pointer position is in VM pixels — the composition root has
already mapped it; `apply` matches every event variant explicitly; a click
sequence continues within both the frame window and the movement window and
otherwise restarts at 1; a released key is dropped at `begin_frame`; the
text input is cleared at `begin_frame`; the wheel accumulates within the
frame; a warp request is taken, not read; a touch contact keeps the previous
position, so the velocity is the delta between two updates.

**Error type:** none. Every accessor is total and every setter is
infallible; an unknown button, key or touch id has no entry at all.

Code map: [`../code/engine/input.md`](../code/engine/input.md).

### 5.5 storage — `engine::storage`

| item | what it is |
|---|---|
| `Storage` | the namespace: the immutable file-system object, the base directory, the mounted archives, the auto search paths, the project path, the registered media table, the resolution cache and the read pool |
| `Location` | a resolved location: `Disk(PathBuf)` or `Archive(PathBuf, String)` |
| `StorageMetadata` | the size and the modified, accessed and created times |
| `MountOptions` / `MountError` / `PreloadError` | the mounting options and their failures |
| `StorageReadError` | the read and lookup failures |
| `StorageKey` / `StorageJob` / `StorageOutcome` / `PendingStorage` | the scheduled-read identity, the work, the result and the future |
| `StorageReadPool` / `StoragePoolConfig` / `StoragePoolStats` / `StoragePoolError` | the worker pool |
| `StartupPlan` | the four startup script names, resolved |
| `ProbeCounts` | a diagnostic: the number of listing and stat probes and the misses |

The mounting operations: `Storage::mount_archives(fs, base_dir, archives)`,
`mount_archives_with`, `mount_discovered_archives`,
`mount_discovered_archives_with`, `mount_archive(path)`,
`unmount_archive(path)`, `set_project(path)`.

The namespace operations: `add_auto_path`, `remove_auto_path`,
`find(name)`, `read(name)`, `read_prefix(name, max_len)`, `stat(name)`,
`exists(name)`, `list(dir)`, `file_list(mask, attr)`, `startup_plan()`,
`register_media(name, bytes)`, `relative_disk_name`, `find_disk`,
`local_name`, `clear_resolution_cache`, `probe_counts`, `read_bytes`,
`game_dir`, `file_system`, `archive_count`, `archive_paths`,
`for_each_archive`, `lookup(name)`.

The free functions: `storage::discover_archives(file_system, base_dir)` —
the opt-in recursive scan for `*.xp3` — and `storage::local_name_in(fs,
game_dir, name)`, which returns the real on-disk spelling of a storage name.

The resolution order `find` follows: the explicit archive addressing; the
current project storage; the auto paths, last added first; the remaining
mounted archives, later mounts first; and the app directory as a loose-file
fallback for an archive project.

Invariants: storage names are case-insensitive even when the host filesystem
is not; a `..` component that escapes the mount is an error, not a miss; a
directory listing is never cached; the project storage is checked first; later
mounts win among the remaining archives; the discovery order is
deterministic; symbolic-link directories are not followed; the media table is
served before the disk and the archives; a read is scheduled rather than
cached; a scheduled read belongs to one resolution, and the generation
changes when the mounts or the auto paths change; a failed read is the
storage's own typed error, never a substitute value.

**Error types:** `MountError` — `NotADirectory`, `Fs`, `Xp3`,
`ArchiveTooLarge`; `PreloadError` — `Truncated`, `Allocation`, `Io`;
`StorageReadError` — `NotFound`, `InvalidPathName`, `Fs`, `Xp3`,
`Preload`, `NoResult`; `StoragePoolError` — `Worker`, `Config`,
`QueueFull`, `MemoryLimit`.

Code map: [`../code/engine/storage.md`](../code/engine/storage.md).

### 5.6 startup — `engine::startup`

| item | what it is |
|---|---|
| `StartupScripts` | the resolved locations: `patch`, `startup` and `after_startup` |
| `StartupScript` | the script to run: `Startup(Location)` or `Initialize(Location)` — the latter is the compatibility fallback |
| `StartupError` | the discovery's failures |

The one operation is `engine::discover_startup(storage: &Storage) ->
Result<StartupScripts, StartupError>`, with
`StartupScript::location()` and
`StartupScript::is_initialize_fallback()` as its readers. The module
locates the startup scripts; it never executes them.

Invariants: `patch.tjs` and `AfterStartup.tjs` are resolved at the app
path only, with no search and no archive lookup, and a missing entry is
`Ok(None)`; `startup.tjs` is searched through the namespace; the
`system/Initialize.tjs` fallback is used when `startup.tjs` is absent;
discovering neither is a typed error.

**Error type:** `StartupError` — `NoStartupScript`, `Fs`,
`Storage`.

Code map: [`../code/engine/startup.md`](../code/engine/startup.md).

### 5.7 sound — `engine::sound`

| item | what it is |
|---|---|
| `SoundSource` | a playable PCM source: decoded samples, or an `AudioStream` a decode-ahead worker fills a bounded window from |
| `AudioStream` | the incremental decoder the pool drives: `spec()`, `read()`, `seek()` |
| `SoundChannel` | one playback voice: play, stop, pause, loop, pan, the volume pair, the rate, the 3D position and the fades |
| `SoundBuffer` | the script-object half: the loaded source, the loop flags and the label and status tracking |
| `Mixer` | the channels, the output clock, the output latency, the global volume and the focus mode |
| `LoopInfo` / `LoopLink` / `LoopCondition` / `LoopFlags` / `SoundLabel` | the generic loop metadata |
| `Fade` | a linear volume ramp with a delay |
| `Status` / `SoundEvent` / `MixSettings` | the status values, the observed events and the mixer's settings |

The constants are `LOOP_FLAG_COUNT` (16), `MAX_LOOP_FLAG_VALUE` (9999),
`MAX_VOLUME` (1.0) and `MAX_PAN` (1.0).

The mixer operations: `Mixer::new`, `channels`, `spawn_channel`,
`remove_channel`, `channel`, `channel_ref`, `advance(dt)`,
`advance_to(now)`, `clock`, `output_latency`, `set_output_latency`,
`global_volume`, `set_global_volume`, `global_focus_mode`,
`set_global_focus_mode`, `focus_gain`, `mix(out, out_spec)` and
`mix_advancing(out, out_spec)`. The free functions are
`sound::mix(channels, settings, out, out_spec)` and
`sound::mix_source(source, position_seconds, rate, gain, pan, out, out_spec)`.

Invariants: the mixer owns its state, with no process global; the channel
ids start at 1 and `0` is the "no channel" sentinel; the output clock is
monotonic; the output latency is the distance between the rendered position
and the audible one; a fade is a linear ramp clamped to `0..=1`; the status
is `Unload`, `Stop` or `Play`; the loop flags are 16 values in
`0..=9999`; a streaming source decodes ahead on its own thread; `mix`
writes into the caller's buffer with the caller's output spec and never
allocates an output device.

**Error type:** `SoundError` — `InvalidSpec`, `UnsupportedChannels`,
`InconsistentBuffer`, `NoSource`, `Decode`, `DecodeThread`. An
unusable specification is rejected when the source is **built**, not when it
is played.

Code map: [`../code/engine/sound.md`](../code/engine/sound.md).

### 5.8 video — `engine::video`

| item | what it is |
|---|---|
| `VideoOverlay` | the playback machine: the status, the clock, the position, the rate, the looping, the segment loop, the period event, the current frame, the binding and the pending events |
| `PlayStatus` | the status: `Unload`, `Stop`, `Play`, `Pause`. `PlayStatus::as_str()` returns the script-visible spelling |
| `PeriodReason` | the script-visible period reason: `code()` returns the integer |
| `VideoEvent` | an observed event |
| `VideoBounds` / `OverlayBinding` | the overlay's rectangle and its bound layer or window |
| `VideoError` | the machine's failures |

The operations: `new`, `open(spec)`, `close`, `is_open`, `spec`,
`status`, `is_playing`, `play`, `pause`, `stop`, `rewind`,
`prepare`, `position_ms`, `position_exact_ms`, `frame`,
`restarted_at_loop`, `duration_ms`, `total_frames`,
`fps`, `seek_ms`, `seek_frame`, `segment_loop`, `set_segment_loop`,
`cancel_segment_loop`, `period_event`, `set_period_event`,
`cancel_period_event`, `visible`, `set_visible`, `set_position`,
`set_size`, `binding`, `bind_layer`, `bind_window`, `unbind`,
`set_binding_alpha`, `present_frame`, `complete_prepare`,
`current_frame`, `clear_frame`, `current_frame_pts_ms`,
`pending_events`, `poll`, `take_status_events`, `advance(dt_ms)`.

The two frame-copy operations are the free functions
`video::blit_frame_to_layer(scene, layer_id, frame, source_over)` and
`video::blit_frame_to_layer_opacity(scene, layer_id, frame, source_over,
opacity)`.

Invariants: `Pause` is a distinct status here; the status event fires only
on a real change; the frame update belongs to a picture and fires once per
decoded picture; prepare mode freezes the clock until the next delivered
frame; a segment loop wraps before the whole-stream end; a non-looping
stream that crosses its end stops and rewinds to frame 0; the position is
kept with sub-millisecond resolution and never rounded per step; the bounds,
the frame size and the blit rectangles are VM pixels; the current frame is
shared, not copied; the overlay's clock advances by the injected delta, and
nothing moves it onto the sound's position.

**Error type:** `VideoError` — `InvalidSpec`, `NotOpen`,
`InconsistentFrame`. The two blit functions return `false` for a frame
with no pixels, a layer with no main image, or a destination image that
declares more pixels than its buffer holds.

Code map: [`../code/engine/video.md`](../code/engine/video.md).

### 5.9 text — `engine::text`

| item | what it is |
|---|---|
| `TextEngine` | the registered faces, the shaping system, the layout memo, the per-character advance memo and the missing-family memo |
| `FontId` | the identity of a registered face |
| `TextStyle` / `TextLayout` / `LaidOutGlyph` | the style, the laid-out text and its positioned glyphs |
| `CoverageMask` | a rasterized character mask; its length always equals `width * height` |
| `TextCoverageCache` | the cached character masks and their blurred shadows |
| `GlyphAtlas` / `AtlasGlyph` / `PositionedGlyph` | the packed atlas, its entries and the positioned glyphs a run produces |
| `RasterizedRun` / `TextRasterServices` | the rasterization result and the services a run needs |
| `GlyphBackend` | the backend selector a run is rasterized for; it belongs to the shared DTO crate (`api::glyph`), and the text module takes it as it is |
| `TextError` | the text engine's failures |

The engine's operations: `new`, `set_system_font_dirs`, `add_font`,
`font_id`, `load_system_font`, `load_system_font_exact`, `face`,
`family_name`, `default_font_for`, `font_metrics`, `layout`,
`cached_layouts`, `rasterize_glyph`, `glyph_advance`,
`glyph_draw_rect`.

The atlas's operations: `GlyphAtlas::new(pipeline, size)`, `clear`,
`texture`, `pixels`, `len`, `is_empty`, `pending_len`, `glyph`,
`flush`, `rasterize_layout`.

The free functions: `text::rasterize_text_coverage`,
`text::blurred_text_coverage`, `text::blur_coverage`,
`text::blit_coverage_into`, `text::compose_text_into`,
`text::rasterize_run`, `text::project_text` and
`text::compose_text_cpu`.

Invariants: the layout is the single source of truth for the glyph
positions, so the CPU and the GPU backends agree on the advances and the
metrics; a line's baseline is the face's ascent truncated to whole pixels;
the colour is not part of the atlas cache key; a coverage mask is always
internally consistent; the cached coverage is pen-relative; each glyph keeps
its own mask; a blurred shadow is derived from the cached sharp coverage;
with `antialias == false` the coverage is thresholded; the atlas packs
glyphs left to right, top to bottom, one pixel apart.

**Error type:** `TextError` — `UnknownFont`, `InvalidSize`,
`InvalidFont`, `FontNotFound`, `AtlasFull`, `InvalidCoverage`,
`MaskTooLarge`, `Glyph`, `Render`.

Code map: [`../code/engine/text.md`](../code/engine/text.md).

### 5.10 raster — `engine::raster`

| operation | what it does |
|---|---|
| `raster::blend_pixel(bitmap, x, y, color, coverage, opa)` | alpha-composite one straight-alpha pixel into a bitmap, with a bounds check |
| `raster::fill_rect_replace(bitmap, x, y, w, h, color)` | the flat fill: the colour **becomes** the pixel, including its alpha |
| `raster::fill_polygon(bitmap, pts, color)` | fill a closed polygon (even-odd rule, 4×4 supersampled) |
| `raster::fill_polygon_hatch(bitmap, pts, style, fore, back)` | fill a closed polygon with a two-colour hatch approximation |
| `raster::hatch_foreground(style, x, y)` | whether a pixel is foreground for a hatch style |
| `raster::stroke_polyline(bitmap, pts, closed, color, width)` | stroke an open or closed polyline with a round, anti-aliased band |
| `raster::flatten_arc(x, y, w, h, start_deg, sweep_deg)` | flatten an elliptical arc into a polyline |
| `raster::flatten_cubic(p0, c1, c2, p3, steps)` | flatten a cubic bézier into `steps + 1` points |

The coordinates are layer-local pixels with the y-down convention; the
colours are straight-alpha RGBA.

Invariants: the scene buffer stays straight-alpha — over a transparent
destination the RGB stays the requested colour; the alpha formula is
`color.a × coverage × opa / (255 × 255)`; the polygon fill uses the
even-odd rule with 16 samples per pixel; an open path is closed implicitly
for filling; the hatch tile is 8×8 and repeats; the stroke band is round;
`fill_rect_replace` copies the alpha too; arc angles are degrees on the
+x axis, increasing clockwise.

**Error type:** none. The primitives are infallible: a shape outside the
bitmap, a polygon with fewer than three points or a fully transparent
colour produce no write. Non-finite geometry is rejected —
`flatten_arc` returns an empty polyline.

Code map: [`../code/engine/raster.md`](../code/engine/raster.md).

### 5.11 layer_ops — `engine::layer_ops`

These are the free functions that rewrite a bitmap's pixels in place: the
fills, the blits, the colour conversions, the blur, the 9-slice scaling and
the subtree compositing.

| group | operations |
|---|---|
| types | `RectI` (a half-open pixel rectangle), `BlitSource`, `BlitOptions`, `AffineBlit`, `GammaAdjust`, and the plane selectors `COPY_MAIN` and `COPY_MASK` |
| geometry | `intersect`, `intersect_rect`, `clip_rect` |
| fills and colours | `fill_color_on_alpha`, `fill_color_on_add_alpha`, `fill_color_hold_alpha`, `fill_mask`, `remove_const_opacity`, `colorize`, `noise`, `do_gray_scale`, `adjust_gamma`, `light_contrast`, `convert_alpha_to_add_alpha`, `convert_add_alpha_to_alpha` |
| copies and blits | `blit_copy`, `blit_copy_clipped`, `blit_over`, `blit_plane`, `tile_rect`, `stretch_blit`, `stretch_blit_plane`, `stretch_blit_mode`, `operate_rect`, `affine_blit`, `affine_blit_clipped`, `affine_blit_clipped_pixel_center`, `copy_9patch` |
| effects | `blur_bitmap_in_place`, `gaussian_blur`, `flip_lr`, `flip_ud` |
| subtree compositing | `render_layer_subtree(scene, root)`, `ensure_transition_display(scene, root, w, h)`, `ensure_transition_self_image(scene, root, w, h)` |

Invariants: the operations run on straight-alpha channels; every write is
clipped to the destination bitmap's bounds and to the supplied clip
rectangle, and a write outside the clip is dropped rather than clamped; a
write marks the bitmap dirty, and the writes that touch one rectangle mark
that rectangle; the source rectangle is half-open and a source region that
leaves the source bitmap contributes nothing; `blit_plane` with both plane
bits is a straight copy rather than a blend; the transition slots are
layer-owned and distinct.

**Error type:** none for the primitives. The allocating operations —
`render_layer_subtree`, `ensure_transition_display` and
`ensure_transition_self_image` — return `Option`, and `copy_9patch`
returns `None` when the source is smaller than 11×11 or the destination is
smaller than the source minus the margins. An absent result is
distinguishable from a failure.

Code map: [`../code/engine/layer_ops.md`](../code/engine/layer_ops.md).

### 5.12 shader — `engine::shader`

| item | what it is |
|---|---|
| `ShaderLibrary` | the registry: the programs, the pipelines, the id counters |
| `ProgramDesc` | the descriptor to register: the name, the two SPIR-V stages, the vertex layout, the bindings, the uniform words and the opacity uniform |
| `LoadedProgram` | the registered record: an owned copy of the stage code and of the binding list |
| `ShaderId` / `PipelineId` | the opaque identities, meaningful only inside the library that issued them |
| `Program` | the programs this crate ships: `CrossFade` and `Scroll` |
| `EffectDraw` / `EffectPass` / `EffectTexture` | the generic effect draw: the passes, the size, whether the draw covers the layer, and each pass's program, textures, uniforms and destination rectangle |
| `EffectPipelines` | the scene's table of backend pipeline handles, keyed by program |
| `ShaderError` | the library's failures |

The library's operations: `new`, `load(desc)`, `program(id)`,
`programs()`, `contains(id)`, `pipeline(render, state, shader, blend)`,
`pipelines()`, `pipeline_handle(pipeline)`, `pipeline_count()`,
`release_pipeline(render, state, pipeline)`, `unload(render, state, id)`,
`clear(render, state)`, `len`, `is_empty`. The engine exposes
`Engine::load_shader` and `Engine::effect_pipeline` for the two a host
needs.

The module also re-exports the **data half** of the contract from the shared
DTO crate, so a program descriptor is spelled through `engine::shader`
alone: `BindingDesc`, `BindingKind`, `EffectPipelineDesc`,
`EffectUniforms`, `SamplerKind`, `ShaderCode`, `ShaderDataError`,
`ShaderModule`, `ShaderStage`, `VertexLayout`, `MAX_EFFECT_TEXTURES`,
`MAX_EFFECT_UNIFORMS` and `EFFECT_PUSH_CONSTANT_BYTES`.

Invariants: registration is idempotent — loading an identical descriptor
returns the id already issued; an id is never reused; a registered program
outlives the buffers it was loaded from; a program is read-only; a pipeline
is one (program, blend) pair; the library holds no backend state of its own;
unloading a program releases its pipelines first.

**Error type:** `ShaderError` — `Data` (carrying the data-level refusal),
`UnknownShader`, `UnknownPipeline`, `Unsupported`, `Render`.

Code map: [`../code/engine/shader.md`](../code/engine/shader.md).

### 5.13 translate — `engine::translate`

| item | what it is |
|---|---|
| `PixelTranslation` | the Fit-with-letterbox mapping for one pair of sizes: the scale, the offset, the VM size and the device size |
| `TranslateError` | the mapping's failures |

The operations: `PixelTranslation::fit(vm_size, device_size) ->
Result<PixelTranslation, TranslateError>`, `is_identity()`,
`to_device_point(p)`, `to_device_rect(r)`, `to_vm_point(p)`,
`raster_scale()`, `vm_pixel_region(x, y)`, `root_transform()` and
`inverse_root_transform()`.

The mapping is a uniform scale of `min(device / vm)` per axis plus a
centred offset, computed in `f64` and narrowed once. It is identity at
`scale == 1` and `offset == (0, 0)`. `to_vm_point` is the exact inverse
of `to_device_point`. `root_transform` is the mapping written as a node
transform; the engine puts it on the draw tree's root, and the render-only
overlay cancels it with the inverse.

Invariants: one scale for both axes; the mapping is a pure function of the
two sizes, so it cannot drift across frames; a degenerate size pair has no
mapping; the translation is internal to this layer — the compatibility layer
must not call `to_device_*`, `to_vm_point` or `raster_scale`, and must
not name a device pixel; input travels device → VM before the script tick.

**Error type:** `TranslateError` — `EmptyDimension { vm, device }` for a
zero dimension in either size. The engine turns it into "no translation":
the frame draws nothing and the condition is warned about once until the
sizes fit again. No substitute scale is invented.

Code map: [`../code/engine/translate.md`](../code/engine/translate.md).

### 5.14 imagedecode — `engine::imagedecode`

| item | what it is |
|---|---|
| `DecodePool` | the pool: the worker threads, the queue, the byte accounting and the reads in flight |
| `DecodePoolConfig` | the configuration: the worker count, the queue limit and the memory limit. `DEFAULT_MEMORY_LIMIT` is 256 MiB |
| `DecodeStats` | the counters: the number of decodes and the accumulated time |
| `ImageDecoder` | the codec the pool drives: `probe(bytes, hint)` and `decode(bytes, hint)` |
| `PendingImage` | the future a pending bitmap carries, with the cache key it fills |
| `ImageOutcome` / `ImageState` | what a pending image produced: `Ready`, `Failed` or `Cancelled`; and whether it is still pending |
| `DecodePass` | the pass a worker runs over the decoded pixels |
| `DecodePoolError` / `ImageDecodeError` / `ImageSizeError` | the failures |

The engine's operations: `Engine::install_decoder(decoder, config)`,
`Engine::decode_pool()`, `Engine::decode_pool_mut()`,
`Engine::decode_stats()`. The pool's own operations:
`DecodePool::new(decoder, config)`, `submit`, `submit_cached`,
`cancel`, `queued`, `held_bytes`, `stats`. The pending image's
operations: `cache_key`, `fills_cache_entry`, `same_decode`,
`demand`, `is_demanded`, `is_ready`, `state`, `await_ready`,
`cancel`.

The scene's side: `Scene::add_pending_bitmap`,
`Scene::set_pending_image`, `Scene::pending_image`,
`Scene::pending_decode_for`, `Scene::resolve_bitmap` and
`Scene::resolve_pending_images`.

Invariants: the pool is the engine's own state and two pools in one process
do not interact; a job is charged the size of its bytes plus the size of the
pixels it will produce, so the memory limit bounds the pool's footprint; a
job that does not fit is refused and the caller decodes it synchronously; a
pending bitmap carries the probed size and no pixels; a pending decode is
cancelled when it is superseded; a failed, cancelled or mismatched decode
installs a transparent blank and logs the cause — it never raises and never
leaves a bitmap without pixels.

**Error types:** `DecodePoolError` — `Worker`, `Config`,
`QueueFull`, `MemoryLimit`, `SizeOverflow`, `IdExhausted`;
`ImageDecodeError` — `Failed { message }`; `ImageSizeError` —
`TooLarge { width, height }`.

Code map: [`../code/engine/scene.md`](../code/engine/scene.md#the-pending-image)
— the pending image, the pool's byte limit and the failures.

---

## 6. The L3 seam

`crates/compat/src/engine_api.rs` is the **only** file under
`crates/compat/src` allowed to name `engine::`. The `l3-engine-api`
rule of the `layering` gate check enforces it. The seam is a **re-export,
not a wrapper**: a protocol type *is* the engine type.

What crosses it:

| group | items |
|---|---|
| `engine::scene` | `Scene`, `LayerState`, `BitmapState`, `FontState`, `Rect`, `TransitionFrame`, `DecodedImageKey`, `DecodedImageMode`, and the hit candidate `engine::scene::HitCandidate` |
| `engine::imagedecode` | `DecodePool`, `PendingImage`, `DecodePass` |
| `engine::storage` | `Storage`, `Location`, `StorageReadError`, `normalize_in_archive_name` |
| `engine::input` | `InputState` |
| `engine::timer` | `TimerState`, `TimerId`, `TimerCallback` |
| `engine::clock` | `FrameClock` |
| `engine::text` | `TextEngine`, `TextError`, `FontId`, `TextStyle`, `CoverageMask`, `TextCoverageCache`, and the coverage and composition functions `rasterize_text_coverage`, `blurred_text_coverage`, `blur_coverage`, `blit_coverage_into`, `compose_text_into` |
| `engine::sound` | `Mixer`, `AudioStream`, `SoundBuffer`, `SoundChannel`, `SoundSource`, `SoundError`, `SoundEvent`, `SoundLabel`, `LoopCondition`, `LoopInfo`, `LoopLink`, `Status`, `LOOP_FLAG_COUNT` |
| `engine::video` | `VideoOverlay`, `VideoEvent`, `PlayStatus`, `VideoBounds`, `blit_frame_to_layer`, `blit_frame_to_layer_opacity` |
| `engine::trans` | `Trans`, `TransKind`, `RuleMask`, `ScrollFrom`, `ScrollStay`, `TransError` |
| `engine::shader` | `EffectDraw` |
| `engine::raster` | `blend_pixel`, `fill_polygon`, `fill_rect_replace`, `flatten_arc`, `flatten_cubic`, `hatch_foreground` |
| `engine::layer_ops` | the blit, fill, mask, blur, gamma and subtree operations, with their types `AffineBlit`, `BlitOptions`, `BlitSource`, `GammaAdjust`, `RectI` and the plane selectors `COPY_MAIN` and `COPY_MASK` |

Four things are deliberately **outside** the seam:

* **The projection and the draw tree** — the render scene, the render state
  and the surface. This layer projects, the layer below draws, and the
  compatibility layer is not on that path at all.
* **The pixel translation** — `PixelTranslation` and its device-mapping
  methods stay inside this layer. Exposing one would spread the coordinate
  rule across hundreds of call sites.
* **The frame owner** — `Engine` belongs to the composition root. The
  **frame clock** does cross the seam, because the script's tick count is
  the clock the frame advances.
* **The L1 services** — the file system, the render pipeline, the media
  services and the font system. This layer calls them internally; the
  compatibility layer never names one.

The rules the seam obeys: every item is this layer's state or an action on
that state; the coordinates are VM pixels only; the errors are typed values
owned by the module that declares them, with `Option` for an absent object
and `()` for an infallible primitive over already-validated bounds.

The protocol's own documentation is
[`../code/compat/engine_api.md`](../code/compat/engine_api.md); the rules
are in [`../rules/api_rules.md`](../rules/api_rules.md) §4.

---

## 7. The error types

One error enum per domain, owned by the crate that declares it. A new
failure mode is a new variant in the owning enum; it is never a default
value, a wildcard arm or a substitute on the calling side.

| enum | domain | what it means | `#[non_exhaustive]` |
|---|---|---|---|
| `EngineError` | the frame and the surface actions | a render action failed, the overlay could not be refreshed, or the frame hook reported a failure | yes |
| `TranslateError` | the VM ↔ device mapping | a size pair has a zero dimension, so it cannot be fitted | yes |
| `TimerError` | the timer registry | an id is not registered, or a zero interval was requested | yes |
| `TextError` | the text engine | an unknown face, an invalid size or font, a family that is not installed, a full atlas, an inconsistent coverage buffer, a mask that is too large, a glyph failure or a render failure | yes |
| `SoundError` | the sound model | an unusable specification, an unsupported channel count, a buffer that disagrees with its spec, no source, a decode failure or a worker that could not start | yes |
| `VideoError` | the video overlay | an unusable specification, a command on a closed overlay, or a frame that disagrees with its dimensions | yes |
| `TransError` | the transition machine | an invalid duration or vague, a missing or unexpected rule, an inconsistent rule buffer, a size mismatch or an option out of range | yes |
| `ShaderError` | the shader library | a descriptor that is not usable, an unknown shader or pipeline id, a backend that cannot build effect pipelines, or a driver refusal | yes |
| `StartupError` | the startup discovery | no startup script was found, a file-system operation failed, or the storage resolution failed | yes |
| `MountError` | the mounting | the mount path is not a directory, a file-system failure, an archive that cannot be opened or parsed, or an archive too large to address in one buffer | yes |
| `PreloadError` | a preloaded archive's image | the image is shorter than its reported size, the buffer cannot be allocated, or the read failed | yes |
| `StorageReadError` | the reads and the lookups | the name did not resolve, a path component escaped the mount, a file-system failure, an archive read failure, a preload failure, or no result | yes |
| `DecodePoolError` | the image decode pool | a worker could not start, the configuration is unusable, the queue is full, the memory limit would be exceeded, a size overflowed, or the ids are exhausted | no |
| `StoragePoolError` | the storage read pool | a worker could not start, the configuration is unusable, the queue is full, or the memory limit would be exceeded | no |
| `ImageDecodeError` | one image decode | the decode failed; the message is the codec's own | no |
| `ImageSizeError` | a probed image size | the pixel count does not fit in memory | no |

The enums marked `#[non_exhaustive]` may grow a variant without a breaking
change, so a caller that matches them needs a catch-all arm. The four that
are not marked are closed: they are the small, leaf-level errors whose
variants are stable.

---

## 8. The coordinate spaces

Three pixel spaces exist, and they are never the same:

| space | unit | who speaks it | examples |
|---|---|---|---|
| **VM pixel** | one pixel of the logical screen (the primary layer) | the compatibility layer and the script | the screen size, the layer rectangles, the bitmap dimensions, the font size, a readback coordinate |
| **window pixel** | one pixel of the OS window's client area | the compatibility layer and the script | the window's inner width and inner height |
| **device pixel** | one pixel of the framebuffer | the platform layer | the swapchain extent, the viewport |

**Only this layer names more than one of them.**
`engine::translate::PixelTranslation` owns the VM↔device mapping. The
compatibility layer never sees a device coordinate, and the platform layer
receives the VM-space draw tree with an opaque root transform without
interpreting the VM semantics. The presentation rule is one rule: **fit with
letterbox** — scale uniformly by `min(device / vm)`, centre, and clear the
margin.

Two consequences hold for a caller:

* every value the compatibility layer reads or writes is in VM pixels, and
  every value a script observes stays in VM pixels;
* an OS resize changes the framebuffer only. A script-side window-size
  change travels the other way, through `Engine::take_window_resize()`, and
  the translation fits the unchanged screen into the new client.

The rule is stated in [`overall.md`](overall.md) §3 and
[`../rules/arch_rules.md`](../rules/arch_rules.md) §5; the architecture is
in [`l2.md`](l2.md) §4; the code map is
[`../code/engine/translate.md`](../code/engine/translate.md).

---

## 9. The stability contract

### 9.1 What a caller may rely on

* The crate root's re-export list (§1.2) is the intended contract. An item
  that is not on it stays reachable through its module, and its module path
  is stable in the same way.
* An additive change — a new function, a new method, a new variant — is
  fine and needs no special process.
* An L1 type may flow into an `engine` signature. The engine depends on the
  platform type crates, so its public API may name `RenderScene`,
  `FileState`, `FontMetrics` and their neighbours. Such a change is
  accepted, not forbidden.
* The invariants stated per subsystem in §3 and §5 hold, and a change that
  breaks one is a change to the contract.

### 9.2 What may change, and how

| change | what it needs |
|---|---|
| a new function, method or variant | an additive change |
| a removal, a changed signature, a changed meaning | a decision entry and a reviewed change |
| an L1 type change that flows into an `engine` signature | accepted, and stated in the change description |
| a new engine item the compatibility layer needs | added in `engine` with a generic name, re-exported in the seam, documented in the protocol document, and used from the compatibility layer through the seam |
| a new item in the crate root's re-export list | a reviewed change to the curated surface |

Removing an item deletes it from the engine, from the seam, from the protocol
document and from every call site **in the same change**. No compatibility
shim, no stubbed old signature, no copy "kept for reference".

### 9.3 The rules an item must satisfy

* **No script vocabulary.** No blend numbering, no virtual-key codes, no
  archive-path spelling, no script tags, no volume scale. The generic concept
  is used instead: `BlendMode`, a normalized gain, an opaque key code.
* **No cross-layer import.** A layer owns its constants and types.
* **No raw pointers and no addresses.** A handle that crosses the stable
  surface is an opaque newtype or a stable id.
* **No process-global state.** This layer owns the state; it does not hide
  it in a static.
* **Documented.** Every public item carries a doc comment, and the crate
  root carries `#![warn(missing_docs)]`. The doc states the contract, the
  units and the failure mode.

### 9.4 Where the protocol is recorded

A new engine item that the compatibility layer needs is re-exported through
the seam and recorded in
[`../rules/api_rules.md`](../rules/api_rules.md) §4 — the L3 ↔ L2
protocol — with its ownership, its coordinate space and its error type where
those are not obvious. The itemised protocol per module is
[`../code/compat/engine_api.md`](../code/compat/engine_api.md).

---

## 10. Where to read next

| you want | read |
|---|---|
| the architecture behind these pages: the layer tree, the projection, the presentation, the storage namespace, the sound, the video, the text, the timers, the transitions, the input, the frame | [`l2.md`](l2.md) |
| the module map: what each module owns, its invariants, its data flow, its failure modes and its tests | [`../code/engine/`](../code/engine/README.md) |
| the layer rules, the state ownership, the frame contract, the coordinate spaces, the error contract | [`../rules/arch_rules.md`](../rules/arch_rules.md) |
| the public surfaces, the L3 ↔ L2 protocol, the ownership rules, the error enums | [`../rules/api_rules.md`](../rules/api_rules.md) |
| the Rust-level review checklist | [`../rules/code_styles.md`](../rules/code_styles.md) |
| the four layers, the crate map, the coordinate spaces, the frame in one page | [`overall.md`](overall.md) |
| the platform core this layer sits on | [`l1.md`](l1.md) |
| the compatibility surface that codes against this one | [`l3.md`](l3.md) |
| embedding the engine and the platform services in another application | [`embedding.md`](embedding.md) |
| the open work and the known gaps | [`../../TODO.md`](../../TODO.md) |
