# `engine::shader` — the shader-program registry

Source: `crates/engine/src/shader/` (`mod.rs`, `library.rs`, `program.rs`) ·
crate `engine` · layer **L2**

`engine::shader` is the **policy half** of the custom-shader contract: it
registers shader programs, allocates their ids and answers lookups by id. The
**data half** — the stage code, the binding and sampler vocabulary, the
pipeline descriptor — lives in the shared DTO crate (`api::shader`), and this
module re-exports the items a program descriptor names, so a caller spells
every type through `engine::shader` alone.

The split is the layer rule: `api` holds values and no policy, L2 holds the
policy, and the backend that *executes* a pipeline is `render` (L1).

> **Scope.** This page documents what the code does today. The render
> backend already builds an effect pipeline from a descriptor
> ([`../platform/render.md`](../platform/render.md) §4), but the **integration**
> — L2 emitting effect draws, and using custom programs for the transitions
> that need a per-pixel rule or a coordinate warp — is deferred work and is
> recorded in `TODO.md` §16. Nothing in the frame path calls the library yet.

---

## 1. Responsibility

* Validate a program descriptor and register the program.
* Allocate and issue the program's identity.
* Own an **independent copy** of the program's stage code, so a registered
  program outlives the buffer it was loaded from.
* Hand the pipeline descriptor a backend needs, with the blend the caller
  chose.

---

## 2. State it owns

```rust
pub struct ShaderLibrary {
    next: u64,
    programs: Vec<LoadedProgram>,
}
```

| field | meaning |
|---|---|
| `next` | the id the next registration receives; monotonic, never reused |
| `programs` | the registered programs, in load order |

The library holds **no backend object and no pipeline**: a pipeline belongs to
a render backend and is created on demand from a registered program. The
library is therefore plain middle-layer state and behaves identically on a
headless host.

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
    pub fn unload(&mut self, id: ShaderId) -> Result<LoadedProgram, ShaderError>;
    pub fn clear(&mut self);
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
}

pub struct ProgramDesc<'a> { … }      // #[non_exhaustive]
pub struct LoadedProgram { … }
pub struct ShaderId(u64);
pub enum ShaderError { … }            // #[non_exhaustive]
```

The crate root re-exports `LoadedProgram`, `ProgramDesc`, `ShaderError`,
`ShaderId` and `ShaderLibrary`.

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

impl<'a> ProgramDesc<'a> {
    pub const fn new(name: &'a str, vertex: &'a [u8], fragment: &'a [u8]) -> Self;
    pub fn pipeline_desc(&self, blend: BlendMode) -> EffectPipelineDesc<'a>;
    pub fn validate(&self) -> Result<(), ShaderDataError>;
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

## 4. Invariants

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
* **An id is only meaningful inside the library that issued it.** The ids are
  that library's own counters, so a host keeps one library and never compares
  ids across libraries.
* **The library holds no backend state**, so it is identical on a headless
  host.

---

## 5. Data flow

```
a caller (today: a test, tomorrow: L3 through the seam)
   │  ProgramDesc { name, stages, layout, bindings, uniforms }
   ▼
ShaderLibrary::load(desc)
   │  desc.validate()  ──► ShaderDataError          (a refusal, not a clamp)
   │  an identical descriptor? → the id already issued
   ▼
LoadedProgram (an owned copy of the stage code)
   │
   ▼  program.pipeline_desc(blend)
EffectPipelineDesc  ──► the render backend, which builds the pipeline
```

---

## 6. Failure modes

| failure | behaviour |
|---|---|
| the descriptor's stages, bindings or uniform budgets are not usable | `ShaderError::Data`, carrying the data-level refusal (`ShaderDataError`) |
| the id names a program the library never issued, or one that was unloaded | `ShaderError::UnknownShader` |

The data-level refusals are `ShaderDataError::EmptyName`,
`EmptyModule`, `TruncatedModule`, `MisalignedModule`, `BadSpirVMagic`,
`UnsupportedSpirVVersion`, `WrongStage`, `TooManyBindings`,
`DuplicateBinding`, `UniformOverflow`, `UniformLayout` and
`OpacityUniformOutOfRange`.

Nothing is clamped to a fallback: an over-budget descriptor is refused, and
an unknown id is an error, never a substitute program.

---

## 7. Current limits

* **The library is not on the frame path.** Nothing in the engine's frame,
  in the projection or in the transition pump calls it; the transition
  composition that a custom program would replace is the two-source frame
  described in [trans.md](trans.md) §4.
* **The projection never emits an effect draw.** The render backend can build
  an effect pipeline and draw `RenderContent::Effect`, but L2's projection
  emits `RenderContent::Texture` only, so no custom program reaches the
  screen yet.
* **The L3 ↔ L2 seam does not carry the shader types yet.** The protocol
  inventory in [`rules/api_rules.md`](../../rules/api_rules.md) §4 lists what
  crosses; the shader vocabulary is not on it.

The deferred design work — the generic L2 shader loading for the
all-shader transitions, and the interim two-texture composition it replaces —
is recorded in `TODO.md` §16.

---

## 8. Tests

* `crates/engine/tests/shader_library.rs` — the idempotent registration, the
  load order, the id allocation and non-reuse, the `unload` and `clear`
  behaviour, the owned copy of the stage code, and the descriptor refusals.

---

## 9. See also

| topic | document |
|---|---|
| the crate at a glance | [README.md](README.md) |
| the transition composition this will eventually replace | [trans.md](trans.md) |
| the L1 data half | [`../platform/api.md`](../platform/api.md) |
| the L3 ↔ L2 protocol | [`rules/api_rules.md`](../../rules/api_rules.md) §4 |
| the deferred design work | `TODO.md` §16 |
