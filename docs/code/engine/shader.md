# `engine::shader` — the shader programs, their pipelines and their draws

Source: `crates/engine/src/shader/` (`mod.rs`, `builtin.rs`, `draw.rs`,
`library.rs`, `pipelines.rs`, `program.rs`) ·
crate `engine` · layer **L2**

`engine::shader` is the **policy half** of the custom-shader contract: it
registers shader programs, allocates their ids, creates and caches the
backend pipelines they are drawn with, and carries the generic effect draw
the projection renders. The **data half** — the stage code, the binding and
sampler vocabulary, the pipeline descriptor — lives in the shared DTO crate
(`api::shader`), and this module re-exports the items a program descriptor
names, so a caller spells every type through `engine::shader` alone.

The split is the layer rule: `api` holds values and no policy, L2 holds the
policy, and the backend that *executes* a pipeline is `render` (L1).

---

## 1. Responsibility

* Validate a program descriptor and register the program.
* Allocate and issue the program's identity.
* Own an **independent copy** of the program's stage code, so a registered
  program outlives the buffer it was loaded from.
* Create the backend pipeline for a `(program, blend)` pair, cache it and
  release it with the program.
* Name the programs this crate ships and the descriptor each one is drawn
  with.
* Carry the **effect draw** — the passes of one layer's custom-shader work —
  from the code that composes it to the projection that renders it.

---

## 2. State it owns

```rust
pub struct ShaderLibrary {
    next_shader: u64,
    next_pipeline: u64,
    programs: Vec<LoadedProgram>,
    pipelines: Vec<LoadedPipeline>,
}
```

| field | meaning |
|---|---|
| `next_shader` | the id the next registration receives; monotonic, never reused |
| `next_pipeline` | the id the next created pipeline receives; monotonic, never reused |
| `programs` | the registered programs, in load order |
| `pipelines` | the created pipelines, in creation order |

The library holds **no backend object of its own**: a pipeline belongs to a
render backend and is created on demand from a registered program, and the
library keeps only the handle the backend returned. The library is therefore
plain middle-layer state and behaves identically on a headless host.

A second table, `Scene::effect_pipelines`, holds the **scene's** copy of the
handles: the projection binds a program's pipeline through it, and the
transition tick asks it whether the backend can draw the program at all. The
library owns the pipelines; the scene owns the mapping from a program to the
handle the frame draws with.

---

## 3. Public API

```rust
pub struct ShaderLibrary { … }

impl ShaderLibrary {
    pub fn new() -> Self;
    pub fn load(&mut self, desc: ProgramDesc<'_>) -> Result<ShaderId, ShaderError>;
    pub fn program(&self, id: ShaderId) -> Option<&LoadedProgram>;
    pub fn programs(&self) -> impl Iterator<Item = &LoadedProgram>;
    pub fn contains(&self, id: ShaderId) -> bool;
    pub fn pipeline(&mut self, render: &RenderPipeline, state: &mut RenderState,
                    shader: ShaderId, blend: BlendMode) -> Result<PipelineId, ShaderError>;
    pub fn pipelines(&self) -> impl Iterator<Item = (PipelineId, ShaderId, BlendMode)>;
    pub fn pipeline_handle(&self, pipeline: PipelineId) -> Option<&EffectPipeline>;
    pub fn pipeline_count(&self) -> usize;
    pub fn release_pipeline(&mut self, render: &RenderPipeline, state: &mut RenderState,
                            pipeline: PipelineId) -> Result<(), ShaderError>;
    pub fn unload(&mut self, render: &RenderPipeline, state: &mut RenderState,
                  id: ShaderId) -> Result<LoadedProgram, ShaderError>;
    pub fn clear(&mut self, render: &RenderPipeline, state: &mut RenderState);
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}

pub struct ProgramDesc<'a> { … }      // #[non_exhaustive]
pub struct LoadedProgram { … }
pub struct ShaderId(u64);
pub struct PipelineId(u64);
pub enum ShaderError { … }            // #[non_exhaustive]

pub struct EffectPipelines { … }
pub struct EffectDraw { … }           // #[non_exhaustive]
pub struct EffectPass { … }           // #[non_exhaustive]
pub struct EffectTexture { … }        // #[non_exhaustive]
pub enum Program { … }                // #[non_exhaustive]
```

The crate root re-exports `EffectDraw`, `EffectPass`, `EffectPipelines`,
`EffectTexture`, `LoadedProgram`, `PipelineId`, `Program`,
`ProgramDesc`, `ShaderError`, `ShaderId` and `ShaderLibrary`.

The engine owns one library and exposes it to a host through
`Engine::load_shader` (register a program) and `Engine::effect_pipeline`
(create the pipeline for a `(program, blend)` pair).

### `ProgramDesc` — the descriptor to register

```rust
pub struct ProgramDesc<'a> {
    pub name: &'a str,
    pub vertex: ShaderModule<'a>,
    pub fragment: ShaderModule<'a>,
    pub vertex_layout: VertexLayout,
    pub bindings: &'a [BindingDesc],
    pub uniform_words: u32,
    pub opacity_uniform: Option<u32>,
}
```

The descriptor is **blend-free**: a blend is baked into a backend pipeline, so
one program feeds several pipelines. `new` names the program and its two
SPIR-V stages; the remaining fields carry their neutral value and are written
directly.

`pipeline_desc` is the **only** mapping from a program descriptor to a
backend pipeline description, so the two can never disagree about the
program. `validate` runs the pipeline rules on a neutral blend, so the program
rules have exactly one implementation.

### `LoadedProgram` — the registered record

```rust
impl LoadedProgram {
    pub fn id(&self) -> ShaderId;
    pub fn name(&self) -> &str;
    pub fn vertex(&self) -> &[u8];
    pub fn fragment(&self) -> &[u8];
    pub fn vertex_layout(&self) -> VertexLayout;
    pub fn bindings(&self) -> &[BindingDesc];
    pub fn uniform_words(&self) -> u32;
    pub fn opacity_uniform(&self) -> Option<u32>;
    pub fn pipeline_desc(&self, blend: BlendMode) -> EffectPipelineDesc<'_>;
}
```

The record owns its stage code and its binding list, so it is independent of
the buffers the descriptor borrowed. It is **read-only**: the library replaces
a program by unloading and registering again, never by mutation.

The returned pipeline descriptor **borrows** the record, so it lives no longer
than the registration: an unloaded program's bytes are gone with it.

### The data half (`api::shader`)

```rust
pub const MAX_EFFECT_TEXTURES: usize = 4;
pub const MAX_EFFECT_UNIFORMS: usize = 64;
pub const EFFECT_PUSH_CONSTANT_BYTES: u32 = 64;

pub enum ShaderStage { Vertex, Fragment }
pub enum ShaderCode<'a> { SpirV(&'a [u8]) }
pub struct ShaderModule<'a> { pub stage: ShaderStage, pub code: ShaderCode<'a> }
pub enum VertexLayout { UnitQuad }
pub enum SamplerKind { LinearClamp, LinearRepeat, NearestClamp }
pub enum BindingKind { CombinedImageSampler }
pub struct BindingDesc { pub binding: u32, pub kind: BindingKind, pub sampler: SamplerKind }
pub struct EffectUniforms { … }
pub struct EffectPipelineDesc<'a> { … }
pub enum ShaderDataError { … }         // #[non_exhaustive]
```

The code is **SPIR-V**. `ShaderCode` has exactly one variant, so a caller
cannot believe a format exists that the backend cannot consume; adding WGSL
later is an additive variant and no signature changes.

`ShaderCode::validate` checks the module's **shape** without parsing it: the
code is not empty, it is at least a SPIR-V header long, its length is a whole
number of four-byte words, it starts with the SPIR-V magic number, and it
declares a version this build accepts. The driver's own parser and the
device's limits are the backend's check, reported as a `RenderError`.

`VertexLayout` has one variant because the backend has exactly one vertex
buffer — the shared unit quad. A pipeline that asks for a layout this build
cannot build is a typed `RenderError::Unsupported` at creation, never a
silent substitution.

The uniform block is a whole number of four-word (`vec4`) units, so a
program's layout stays 16-byte aligned; a length that is not a multiple of
four is `ShaderDataError::UniformLayout`.

The budgets are **API** budgets, not driver limits: a program that declares
more than `MAX_EFFECT_UNIFORMS` uniform words, or binds more than
`MAX_EFFECT_TEXTURES` textures, is **refused** with a
`ShaderDataError` — never truncated. `EFFECT_PUSH_CONSTANT_BYTES` is the size
of the push-constant block: the vertex transform only. A 64-byte matrix plus
the reserved space fits inside the 128 bytes Vulkan guarantees on every
device, so a pipeline built for this block needs no device-limit check.

---

## 4. The programs this crate ships

`Program` names the programs the engine draws with:

| program | source | bindings |
|---|---|---|
| `CrossFade` | `crates/engine/shaders/crossfade.frag` | the two sources |
| `Scroll` | `crates/engine/shaders/scroll.frag` | the one source it places |

Both share the vertex stage `crates/engine/shaders/effect.vert` and the
uniform block the shipped programs declare: sixteen words — two source
rectangles, a parameters `vec4` and a spare `vec4` whose `w` carries the
composed draw opacity. The constants are the layout's contract with every
program here (`builtin::RECT0_UNIFORM`, `RECT1_UNIFORM`, `PARAM_UNIFORM`,
`OPACITY_UNIFORM`, `UNIFORM_WORDS`).

`Engine::install_effect_pipelines` registers the programs and creates their
pipelines when the backend can run effect pipelines, and records the handles
in `Scene::effect_pipelines`. A backend that cannot run effect pipelines gets
none, and the transitions that need one blend their display bitmap on the CPU.

---

## 5. The effect draw

An `EffectDraw` is one layer's worth of custom-shader work:

```rust
pub struct EffectDraw {
    pub passes: Vec<EffectPass>,
    pub size: (u32, u32),
    pub covers_layer: bool,
}

pub struct EffectPass {
    pub program: Program,
    pub textures: Vec<EffectTexture>,
    pub uniforms: EffectUniforms,
    pub dst: Rect,
}

pub struct EffectTexture { pub bitmap: u32 }
```

The draw is a **value**: the layer tree holds it, the projection turns it
into render nodes, and the renderer executes it. A pass names the program it
runs — never a pipeline — and the scene's pipeline table resolves that to the
backend's pipeline. The textures are named by the bitmap ids the layer tree
already owns, so the projection resolves them through the same lookup it uses
for every other bitmap.

`covers_layer` records whether the draw stands in for the layer's whole
subtree (drawn over the layer's rectangle, holding its children) or for the
layer's own image (drawn at the image's placement, with the children live
beside it).

The projection emits one render node per pass, with the pass's rectangle
placed where the layer's image would have been. A pass that names a program
the scene has no pipeline for, or a bitmap that has no texture, is not drawn,
and the layer says so in the log.

---

## 6. Invariants

* **Registration is idempotent.** Loading a descriptor identical to a
  registered one returns the id already issued, so a startup path that runs
  twice does not grow the table. A sequence of *different* programs grows it
  in load order.
* **An id is never reused.** `unload` removes the record and the id stays
  unused; a later registration of the same descriptor receives a fresh id.
* **A registered program outlives its buffers.** The library keeps an owned
  copy of the stage code and of the binding list.
* **A program is read-only.** The library replaces a program by unloading and
  registering again, never by mutating a record in place.
* **A pipeline is one `(program, blend)` pair.** The blend is part of the
  backend's pipeline object, so the same program drawn with two blends needs
  two pipelines and receives two ids. Asking for the same pair twice returns
  the pipeline already created.
* **An id is only meaningful inside the library that issued it.** The ids are
  that library's own counters, so a host keeps one library and never compares
  ids across libraries.
* **The library holds no backend state of its own**, so it is identical on a
  headless host.
* **Unloading a program releases its pipelines first**, because the backend
  pipeline is built from the program's stage code.

---

## 7. Data flow

```
the caller (the engine, or a host that loads its own programs)
   │  ProgramDesc { name, stages, layout, bindings, uniforms }
   ▼
ShaderLibrary::load(desc)
   │  desc.validate()  ──► ShaderDataError          (a refusal, not a clamp)
   │  an identical descriptor? → the id already issued
   ▼
LoadedProgram (an owned copy of the stage code)
   │
   ▼  program.pipeline_desc(blend)
EffectPipelineDesc  ──► render::create_effect_pipeline  ──► EffectPipeline
   │                                                                │
   ▼  ShaderLibrary::pipeline(...) caches it                        │
PipelineId ──────────────────────────────────────────────────────────┘
   │
   ▼  the scene records the handle: Scene::effect_pipelines
   │
   ▼  the code that composes the work builds an EffectDraw
EffectDraw { passes, size, covers_layer }
   │
   ▼  the projection turns every pass into a render node
RenderContent::Effect ──► the backend draws the program
```

---

## 8. Failure modes

| failure | behaviour |
|---|---|
| the descriptor's stages, bindings or uniform budgets are not usable | `ShaderError::Data`, carrying the data-level refusal (`ShaderDataError`) |
| the id names a program the library never issued, or one that was unloaded | `ShaderError::UnknownShader` |
| the pipeline id was never issued, or its pipeline was released | `ShaderError::UnknownPipeline` |
| the backend cannot build effect pipelines at all | `ShaderError::Unsupported` |
| the driver refuses a module or a pipeline | `ShaderError::Render`, carrying the `RenderError` |

The data-level refusals are `ShaderDataError::EmptyName`,
`EmptyModule`, `TruncatedModule`, `MisalignedModule`, `BadSpirVMagic`,
`UnsupportedSpirVVersion`, `WrongStage`, `TooManyBindings`,
`DuplicateBinding`, `ReservedBinding`, `UniformOverflow`,
`UniformLayout` and `OpacityUniformOutOfRange`.

Nothing is clamped to a fallback: an over-budget descriptor is refused, and
an unknown id is an error, never a substitute program.

---

## 9. Current limits

* **Two programs are shipped.** `CrossFade` and `Scroll` are the kinds the
  transition machine can draw; every other kind blends on the CPU.
* **A draw is one layer's worth of work.** The projection emits the draw in
  place of the layer's own image (or its subtree), one node per pass. A pass
  covers an axis-aligned rectangle: the draw has no per-pass rotation, skew
  or mesh deformation, and the projection sets no per-pass transform.
* **A pass samples whole bitmaps.** The sub-rectangle a program reads is
  expressed in the program's own uniform rectangle, so the renderer never
  has to know how a program addresses its sources.
* **The programs are the engine's.** `Program` is a closed vocabulary and
  the L3 seam carries the `EffectDraw` value only: a compatibility-layer
  member never names a program, a pipeline or a shader.
* **The all-shader transitions are not built.** The kinds that need a
  per-pixel rule or a coordinate warp (`universal`, `mask` and the seven
  `extrans` kinds) have no program, so they blend on the CPU on every host.
  The deferred work is recorded in `TODO.md` §16.

---

## 10. Tests

* `crates/engine/tests/shader_library.rs` — the idempotent registration, the
  load order, the id allocation and non-reuse, the pipeline cache, the
  `unload` and `clear` behaviour, the owned copy of the stage code, and the
  descriptor refusals.
* `crates/engine/tests/shader_assets.rs` — the shipped programs: their
  stage code, their descriptors and their uniform layout.
* `crates/engine/tests/trans_effect.rs` — the effect draw a transition kind
  builds, and the projection's nodes.
* `crates/engine/tests/trans_effect_gpu.rs` — the draw against the CPU
  blend, on a device.

---

## 11. See also

| topic | document |
|---|---|
| the crate at a glance | [README.md](README.md) |
| the transition kinds the programs serve | [trans.md](trans.md) |
| the L1 data half | [`../platform/api.md`](../platform/api.md) |
| the L3 ↔ L2 protocol | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the deferred design work | `TODO.md` §16 |
