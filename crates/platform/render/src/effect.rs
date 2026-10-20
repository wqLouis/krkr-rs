//! Effect (custom-shader) pipelines and their scene payload.
//!
//! An [`EffectPipeline`] is an immutable handle to one pipeline the backend
//! built from an `api::shader::EffectPipelineDesc`: two shader stages, its
//! descriptor bindings, its uniform block and the blend state baked into the
//! object. As with [`Texture`], cloning the handle clones nothing but the
//! handle: the resource lives in the caller-owned [`RenderState`] and is
//! released with [`crate::destroy_effect_pipeline`].
//!
//! One draw is [`RenderContent::Effect`](crate::scene::RenderContent::Effect).
//! The node's `dst` places the quad the program shades and
//! [`EffectUniforms`] carries the program's parameters; the traversal
//! overwrites the descriptor's declared opacity word with the composed draw
//! opacity, so a fading layer fades. The node's `src` and `blend` are not used:
//! the program's uniform rectangles select the texels and the pipeline carries
//! the blend, because a fixed-function backend bakes blend state into the
//! pipeline object.

use std::fmt;
use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use api::shader::{EffectUniforms, MAX_EFFECT_TEXTURES};

use crate::texture::Texture;

/// Monotonic source of [`EffectPipeline`] identities.
///
/// A counter, not the backing allocation address, so a dropped-and-rebuilt
/// handle can never alias a stale one (the same rule as [`crate::Texture`] and
/// [`crate::RenderTarget`]).
static NEXT_EFFECT_PIPELINE_ID: AtomicU64 = AtomicU64::new(1);

/// Immutable handle to one created effect pipeline.
///
/// Two handles compare equal only when they name the same creation. The
/// pipeline was built for one blend mode and one uniform block size; a draw
/// whose uniforms do not match the descriptor is rejected by the backend rather
/// than truncated.
#[derive(Clone)]
pub struct EffectPipeline {
    inner: Arc<EffectPipelineInner>,
}

/// Shared, immutable contents of an [`EffectPipeline`] handle.
struct EffectPipelineInner {
    /// Process-unique identity of this creation.
    id: u64,
    /// Diagnostic name from the descriptor.
    name: String,
    /// Uniform words the program's block declares.
    uniform_words: u32,
    /// Word the traversal overwrites with the composed draw opacity.
    opacity_uniform: Option<u32>,
}

impl EffectPipeline {
    /// Creates a handle from a validated descriptor.
    pub(crate) fn new(name: &str, uniform_words: u32, opacity_uniform: Option<u32>) -> Self {
        Self {
            inner: Arc::new(EffectPipelineInner {
                id: NEXT_EFFECT_PIPELINE_ID.fetch_add(1, Ordering::Relaxed),
                name: name.to_owned(),
                uniform_words,
                opacity_uniform,
            }),
        }
    }

    /// A process-unique key for the handle's backing creation.
    pub fn key(&self) -> u64 {
        self.inner.id
    }

    /// The descriptor's diagnostic name.
    pub fn name(&self) -> &str {
        &self.inner.name
    }

    /// The uniform words the program's block declares.
    pub fn uniform_words(&self) -> u32 {
        self.inner.uniform_words
    }

    /// The uniform word that receives a draw's composed opacity, if any.
    pub fn opacity_uniform(&self) -> Option<u32> {
        self.inner.opacity_uniform
    }
}

impl fmt::Debug for EffectPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EffectPipeline")
            .field("key", &self.key())
            .field("name", &self.name())
            .field("uniform_words", &self.uniform_words())
            .finish()
    }
}

impl PartialEq for EffectPipeline {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

impl Eq for EffectPipeline {}

impl Hash for EffectPipeline {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.key().hash(state);
    }
}

/// One custom-shader draw's payload.
///
/// `textures` are bound in the pipeline's binding order (the descriptor's
/// bindings, texture bindings only), and `uniforms` must declare exactly the
/// pipeline's `uniform_words`.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct RenderEffect {
    /// Pipeline to bind.
    pub pipeline: EffectPipeline,
    /// Sampled textures, in the pipeline's binding order.
    pub textures: Vec<Texture>,
    /// Uniform words for the program.
    pub uniforms: EffectUniforms,
}

impl RenderEffect {
    /// A draw of `pipeline` over `textures`, carrying `uniforms`.
    ///
    /// The descriptor is `#[non_exhaustive]`, so this is the way to build one
    /// outside this crate. Nothing here is validated: the backend checks the
    /// texture count against the pipeline's bindings and the uniform length
    /// against its declared block, and reports a mismatch with a typed error.
    pub fn new(pipeline: EffectPipeline, textures: Vec<Texture>, uniforms: EffectUniforms) -> Self {
        Self {
            pipeline,
            textures,
            uniforms,
        }
    }

    /// The sampled textures' identities, padded with zeros.
    ///
    /// Backends key their descriptor-set cache on this: two draws of one
    /// pipeline over the same textures reuse one set.
    pub(crate) fn texture_keys(&self) -> [u64; MAX_EFFECT_TEXTURES] {
        let mut keys = [0u64; MAX_EFFECT_TEXTURES];
        for (slot, texture) in keys.iter_mut().zip(&self.textures) {
            *slot = texture.key();
        }
        keys
    }
}
