//! Generic shader programs: registration, identity and lookup.
//!
//! This is the middle layer's half of the custom-shader contract. A *program*
//! is stage code plus the pipeline properties it needs
//! ([`ProgramDesc`]); the library registers it under a [`ShaderId`] and answers
//! lookups by that id. The policy that matters — which programs exist, how long
//! they live, and (from the pipeline layer) which blend a draw uses — lives
//! here, so the bottom renderer only ever executes a pipeline it was handed.
//!
//! The data half of the contract (stage code, bindings, samplers, uniform
//! blocks, the pipeline descriptor) is the shared DTO crate's
//! [`api::shader`]; this module re-exports the items a program descriptor names,
//! so a caller registers programs through `engine::shader` alone and never has
//! to know which crate a type came from.
//!
//! Registration validates the descriptor ([`ProgramDesc::validate`] →
//! [`EffectPipelineDesc::validate`]) and keeps an owned copy of the stage code,
//! so a registered program outlives the buffers it was loaded from.
//! Loading the same descriptor twice returns the id already issued: registration
//! is idempotent, which lets a startup path run twice without growing the table.
//!
//! A *pipeline* is one program built for one blend mode
//! ([`ShaderLibrary::pipeline`]). The blend is baked into the backend's
//! pipeline object, so the library caches one pipeline per `(program, blend)`
//! pair and hands out a [`PipelineId`]; a draw names only that id, never a
//! descriptor.
//!
//! [`builtin`] holds the programs this crate ships (their SPIR-V and the
//! descriptor each one must be drawn with); nothing else about them is
//! special-cased here.

pub mod builtin;
mod draw;
mod library;
mod pipelines;
mod program;

pub use draw::{EffectDraw, EffectPass, EffectTexture};
pub use library::ShaderLibrary;
pub use pipelines::EffectPipelines;
pub use program::{LoadedProgram, ProgramDesc};

pub use builtin::Program;

// The data half of the contract: re-exported so a program descriptor is spelled
// through this module. The items stay owned by `api`, which is the crate both
// the middle layer and the render backend name.
pub use api::shader::{
    BindingDesc, BindingKind, EFFECT_PUSH_CONSTANT_BYTES, EffectPipelineDesc, EffectUniforms,
    MAX_EFFECT_TEXTURES, MAX_EFFECT_UNIFORMS, SamplerKind, ShaderCode, ShaderDataError,
    ShaderModule, ShaderStage, VertexLayout,
};

/// Identity of a cached pipeline inside the [`ShaderLibrary`] that issued it.
///
/// Like [`ShaderId`], the value is opaque, stable for the library's lifetime,
/// never reused, and only meaningful inside the library that issued it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PipelineId(u64);

impl PipelineId {
    /// An id from a raw value; only the library allocates ids.
    pub(crate) const fn from_raw(value: u64) -> Self {
        Self(value)
    }
}

/// Identity of a registered program inside the [`ShaderLibrary`] that issued it.
///
/// The value is opaque: it is stable for the lifetime of the library and never
/// reused after [`ShaderLibrary::unload`]. Ids are that library's own counters,
/// so they are only meaningful inside it; a host keeps one library (the engine
/// owns it) and never compares ids across libraries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ShaderId(u64);

impl ShaderId {
    /// An id from a raw value; only the library allocates ids.
    pub(crate) const fn from_raw(value: u64) -> Self {
        Self(value)
    }
}

/// Errors of the shader library.
///
/// A descriptor that cannot be used is a [`ShaderError::Data`] carrying the
/// data-level refusal; an id the library never issued is
/// [`ShaderError::UnknownShader`] or [`ShaderError::UnknownPipeline`]; a
/// backend that cannot build effect pipelines reports
/// [`ShaderError::Unsupported`]; anything the driver rejects is
/// [`ShaderError::Render`]. Nothing is clamped or defaults.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ShaderError {
    /// The descriptor's data is not usable as given.
    #[error(transparent)]
    Data(#[from] ShaderDataError),
    /// The id was never issued by this library, or has been unloaded.
    #[error("unknown shader {0:?}")]
    UnknownShader(ShaderId),
    /// The pipeline id was never issued by this library, or has been released.
    #[error("unknown effect pipeline {0:?}")]
    UnknownPipeline(PipelineId),
    /// This backend does not build effect pipelines.
    #[error("this backend cannot create effect pipelines")]
    Unsupported,
    /// A bottom-layer render action failed.
    #[error(transparent)]
    Render(#[from] api::error::RenderError),
}

impl ShaderError {
    /// Maps a backend failure, so a backend that cannot run effect pipelines at
    /// all reports [`ShaderError::Unsupported`] rather than a wrapped generic
    /// error.
    fn from_render(error: api::error::RenderError) -> Self {
        match error {
            api::error::RenderError::Unsupported { .. } => Self::Unsupported,
            other => Self::Render(other),
        }
    }
}
