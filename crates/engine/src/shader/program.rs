//! What a program is, and how a registered program is read back.

use api::blend::BlendMode;
use api::shader::{BindingDesc, EffectPipelineDesc, ShaderDataError, ShaderModule, VertexLayout};

use crate::shader::ShaderId;

/// A shader program to register: stage code plus the pipeline properties it
/// needs.
///
/// The descriptor is blend-free: a blend is baked into a backend pipeline, so
/// the same program feeds several pipelines ([`Self::pipeline_desc`]). The
/// stage code is borrowed for the call and copied into the library, so a
/// registered program outlives the buffer it was loaded from. [`Self::new`]
/// names the program and its two stages; the remaining fields carry their
/// neutral value and are written directly.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct ProgramDesc<'a> {
    /// Diagnostic name; carried into logs and creation failures.
    pub name: &'a str,
    /// Vertex stage code.
    pub vertex: ShaderModule<'a>,
    /// Fragment stage code.
    pub fragment: ShaderModule<'a>,
    /// Vertex input layout the stages read.
    pub vertex_layout: VertexLayout,
    /// Descriptor bindings, in binding order.
    pub bindings: &'a [BindingDesc],
    /// Uniform words the fragment stage reads.
    pub uniform_words: u32,
    /// Uniform word that receives a draw's composed opacity, when the program
    /// reads one.
    pub opacity_uniform: Option<u32>,
}

impl<'a> ProgramDesc<'a> {
    /// A program with `name` and its two SPIR-V stages, the unit-quad layout,
    /// no bindings and no uniforms.
    pub const fn new(name: &'a str, vertex: &'a [u8], fragment: &'a [u8]) -> Self {
        Self {
            name,
            vertex: ShaderModule::vertex(vertex),
            fragment: ShaderModule::fragment(fragment),
            vertex_layout: VertexLayout::UnitQuad,
            bindings: &[],
            uniform_words: 0,
            opacity_uniform: None,
        }
    }

    /// The pipeline descriptor for this program built with `blend`.
    ///
    /// This is the only mapping from a program descriptor to a backend
    /// pipeline description, so the two cannot disagree about the program.
    pub fn pipeline_desc(&self, blend: BlendMode) -> EffectPipelineDesc<'a> {
        let mut desc = EffectPipelineDesc::new(self.name, self.vertex, self.fragment);
        desc.vertex_layout = self.vertex_layout;
        desc.blend = blend;
        desc.bindings = self.bindings;
        desc.uniform_words = self.uniform_words;
        desc.opacity_uniform = self.opacity_uniform;
        desc
    }

    /// Validates the descriptor.
    ///
    /// The blend does not participate in validity, so validation runs through
    /// [`EffectPipelineDesc::validate`] on a neutral blend: the program rules
    /// have exactly one implementation.
    ///
    /// # Errors
    ///
    /// As [`EffectPipelineDesc::validate`].
    pub fn validate(&self) -> Result<(), ShaderDataError> {
        self.pipeline_desc(BlendMode::SourceOver).validate()
    }
}

/// A program the library has registered.
///
/// The record owns its stage code and binding list, so it is independent of the
/// buffers the descriptor borrowed. It is read-only: the library replaces a
/// program by unloading and registering again, never by mutation.
#[derive(Debug)]
pub struct LoadedProgram {
    id: ShaderId,
    name: String,
    vertex: Vec<u8>,
    fragment: Vec<u8>,
    vertex_layout: VertexLayout,
    bindings: Vec<BindingDesc>,
    uniform_words: u32,
    opacity_uniform: Option<u32>,
}

impl LoadedProgram {
    /// The library's identity for this program.
    pub fn id(&self) -> ShaderId {
        self.id
    }

    /// The diagnostic name the descriptor carried.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The vertex stage's SPIR-V bytes.
    pub fn vertex(&self) -> &[u8] {
        &self.vertex
    }

    /// The fragment stage's SPIR-V bytes.
    pub fn fragment(&self) -> &[u8] {
        &self.fragment
    }

    /// The vertex input layout the program was registered for.
    pub fn vertex_layout(&self) -> VertexLayout {
        self.vertex_layout
    }

    /// The descriptor's bindings, in binding order.
    pub fn bindings(&self) -> &[BindingDesc] {
        &self.bindings
    }

    /// The uniform words the program reads.
    pub fn uniform_words(&self) -> u32 {
        self.uniform_words
    }

    /// The uniform word that receives a draw's composed opacity, if any.
    pub fn opacity_uniform(&self) -> Option<u32> {
        self.opacity_uniform
    }

    /// The pipeline descriptor for this program built with `blend`.
    ///
    /// The returned modules and bindings borrow this record, so the descriptor
    /// lives no longer than the registration: an unloaded program's bytes are
    /// gone with it.
    pub fn pipeline_desc(&self, blend: BlendMode) -> EffectPipelineDesc<'_> {
        let mut desc = EffectPipelineDesc::new(
            &self.name,
            ShaderModule::vertex(&self.vertex),
            ShaderModule::fragment(&self.fragment),
        );
        desc.vertex_layout = self.vertex_layout;
        desc.blend = blend;
        desc.bindings = &self.bindings;
        desc.uniform_words = self.uniform_words;
        desc.opacity_uniform = self.opacity_uniform;
        desc
    }

    /// Registers `desc` under `id`.
    pub(crate) fn register(id: ShaderId, desc: &ProgramDesc<'_>) -> Self {
        Self {
            id,
            name: desc.name.to_owned(),
            vertex: desc.vertex.code.bytes().to_vec(),
            fragment: desc.fragment.code.bytes().to_vec(),
            vertex_layout: desc.vertex_layout,
            bindings: desc.bindings.to_vec(),
            uniform_words: desc.uniform_words,
            opacity_uniform: desc.opacity_uniform,
        }
    }

    /// Whether this record describes the same program as `desc`.
    ///
    /// Registration is idempotent on this: an identical descriptor returns the
    /// id already issued instead of growing the table.
    pub(crate) fn matches(&self, desc: &ProgramDesc<'_>) -> bool {
        self.name == desc.name
            && self.vertex == desc.vertex.code.bytes()
            && self.fragment == desc.fragment.code.bytes()
            && self.vertex_layout == desc.vertex_layout
            && self.bindings == desc.bindings
            && self.uniform_words == desc.uniform_words
            && self.opacity_uniform == desc.opacity_uniform
    }
}
