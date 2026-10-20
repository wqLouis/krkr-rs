//! Generic shader-program and effect-pipeline data.
//!
//! This module is the data half of the custom-shader contract: the stage code,
//! the descriptor a pipeline is built from, the sampler and binding vocabulary,
//! and the uniform block a draw carries. The policy half — program ids, the
//! pipeline cache, the lifetime — is the middle layer's
//! (`engine::shader`), and the backend that executes a pipeline is
//! `render`. Nothing here names a backend, a device, a layer type or a
//! particular effect.
//!
//! Code is SPIR-V. [`ShaderCode`] has exactly one variant, so a caller cannot
//! believe a format exists that the backend cannot consume; adding WGSL later
//! is an additive variant and no signature in this module changes.
//!
//! Validation here is *data* validation — shape, budgets, index ranges — and
//! it returns [`ShaderDataError`]. A backend still validates what only it can
//! (the driver's SPIR-V parser, device limits) and reports that through
//! `RenderError`; the middle layer wraps both in its own error type.

use thiserror::Error;

/// Maximum number of sampled textures one effect draw binds.
///
/// The budget covers the widest built-in program (a source pair plus a rule
/// image) with one spare slot; a descriptor that binds more is refused with
/// [`ShaderDataError::TooManyBindings`], never truncated.
pub const MAX_EFFECT_TEXTURES: usize = 4;

/// Maximum uniform words one program declares, in `f32` words.
///
/// The words travel in a uniform buffer, so this is an API budget rather than
/// a driver limit; a program that declares more is refused with
/// [`ShaderDataError::UniformOverflow`].
pub const MAX_EFFECT_UNIFORMS: usize = 64;

/// Bytes of the effect push-constant block: the vertex transform only.
///
/// `mvp` is a 64-byte matrix and Vulkan guarantees at least 128 bytes of push
/// constants on every device, so a pipeline built for this block needs no
/// device-limit check.
pub const EFFECT_PUSH_CONSTANT_BYTES: u32 = 64;

/// The first word of every SPIR-V module, little-endian.
pub const SPIRV_MAGIC: u32 = 0x0723_0203;

/// Highest SPIR-V version word accepted, encoded `0x00MMmm00` (1.5).
///
/// The check is a single comparison because the encoding is monotonic in the
/// major then the minor version.
pub const SPIRV_VERSION_MAX: u32 = 0x0001_0500;

/// The smallest valid SPIR-V module: the five-word header.
const SPIRV_HEADER_BYTES: usize = 20;

/// Descriptor binding index reserved for the program's uniform block.
///
/// A pipeline's set 0 always binds the uniform block at index 0, so every
/// texture binding sits at 1 or above.
pub const RESERVED_BINDING: u32 = 0;

/// A shader stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ShaderStage {
    /// Vertex stage.
    Vertex,
    /// Fragment stage.
    Fragment,
}

impl std::fmt::Display for ShaderStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Vertex => "vertex",
            Self::Fragment => "fragment",
        })
    }
}

/// The code of one shader stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ShaderCode<'a> {
    /// SPIR-V module bytes, little-endian, a whole number of four-byte words.
    SpirV(&'a [u8]),
}

impl<'a> ShaderCode<'a> {
    /// The stage code as bytes.
    pub fn bytes(self) -> &'a [u8] {
        match self {
            Self::SpirV(bytes) => bytes,
        }
    }

    /// Validates the code's shape for `stage` without parsing the module.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderDataError::EmptyModule`],
    /// [`ShaderDataError::TruncatedModule`],
    /// [`ShaderDataError::MisalignedModule`],
    /// [`ShaderDataError::BadSpirVMagic`] or
    /// [`ShaderDataError::UnsupportedSpirVVersion`].
    pub fn validate(self, stage: ShaderStage) -> Result<(), ShaderDataError> {
        let bytes = self.bytes();
        if bytes.is_empty() {
            return Err(ShaderDataError::EmptyModule { stage });
        }
        if bytes.len() < SPIRV_HEADER_BYTES {
            return Err(ShaderDataError::TruncatedModule {
                stage,
                len: bytes.len(),
            });
        }
        if !bytes.len().is_multiple_of(4) {
            return Err(ShaderDataError::MisalignedModule {
                stage,
                len: bytes.len(),
            });
        }
        let magic = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        if magic != SPIRV_MAGIC {
            return Err(ShaderDataError::BadSpirVMagic { stage, magic });
        }
        // Word 1 is the version; the header is at least five words here.
        let version = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        if version > SPIRV_VERSION_MAX {
            return Err(ShaderDataError::UnsupportedSpirVVersion { stage, version });
        }
        Ok(())
    }
}

/// One shader stage together with its code.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct ShaderModule<'a> {
    /// Stage the code implements.
    pub stage: ShaderStage,
    /// Stage code.
    pub code: ShaderCode<'a>,
}

impl<'a> ShaderModule<'a> {
    /// A SPIR-V vertex module.
    pub const fn vertex(code: &'a [u8]) -> Self {
        Self {
            stage: ShaderStage::Vertex,
            code: ShaderCode::SpirV(code),
        }
    }

    /// A SPIR-V fragment module.
    pub const fn fragment(code: &'a [u8]) -> Self {
        Self {
            stage: ShaderStage::Fragment,
            code: ShaderCode::SpirV(code),
        }
    }

    /// Validates the module's shape.
    ///
    /// # Errors
    ///
    /// As [`ShaderCode::validate`].
    pub fn validate(&self) -> Result<(), ShaderDataError> {
        self.code.validate(self.stage)
    }
}

/// The vertex input layout a pipeline is built for.
///
/// The backend has exactly one vertex buffer — the shared unit quad — so the
/// enum states that instead of pretending a general vertex format exists. A
/// pipeline that asks for a layout this build cannot build is a typed
/// `RenderError::Unsupported` at creation, never a silent substitution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum VertexLayout {
    /// Four unit-quad vertices `(x, y)` in `[0, 1]`, no per-vertex attributes.
    #[default]
    UnitQuad,
}

/// How a bound texture is sampled.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SamplerKind {
    /// Linear filtering, clamp to edge.
    #[default]
    LinearClamp,
    /// Linear filtering, repeat on both axes: a tiled rule image.
    LinearRepeat,
    /// Nearest filtering, clamp to edge: texel-exact sampling.
    NearestClamp,
}

/// What a descriptor binding holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum BindingKind {
    /// A combined image sampler: the draw supplies one texture for it.
    #[default]
    CombinedImageSampler,
}

/// One descriptor binding of an effect pipeline.
///
/// Binding index 0 is the program's uniform block, so texture bindings start at
/// 1 and index 0 is refused rather than silently shadowed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct BindingDesc {
    /// Binding index inside the pipeline's descriptor set (1 or higher).
    pub binding: u32,
    /// What the binding holds.
    pub kind: BindingKind,
    /// How the binding's texture is sampled.
    pub sampler: SamplerKind,
}

impl BindingDesc {
    /// A combined-image-sampler binding sampled with `sampler`.
    pub const fn texture(binding: u32, sampler: SamplerKind) -> Self {
        Self {
            binding,
            kind: BindingKind::CombinedImageSampler,
            sampler,
        }
    }
}

/// The uniform words of one effect draw, in the program's declared layout.
///
/// The block is a whole number of four-word (`vec4`) units so a program's
/// layout stays 16-byte aligned; zero words is a program that reads none. The
/// length is validated against [`MAX_EFFECT_UNIFORMS`] when the block is built,
/// so a draw can never carry more words than the API declared.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct EffectUniforms {
    words: [f32; MAX_EFFECT_UNIFORMS],
    len: u32,
}

impl EffectUniforms {
    /// An empty uniform block (a program that reads no uniforms).
    pub const fn new() -> Self {
        Self {
            words: [0.0; MAX_EFFECT_UNIFORMS],
            len: 0,
        }
    }

    /// Builds a block from `words`.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderDataError::UniformOverflow`] when `words` is longer than
    /// [`MAX_EFFECT_UNIFORMS`], and [`ShaderDataError::UniformLayout`] when the
    /// length is not a whole number of `vec4` units.
    pub fn from_words(words: &[f32]) -> Result<Self, ShaderDataError> {
        if words.len() > MAX_EFFECT_UNIFORMS {
            return Err(ShaderDataError::UniformOverflow {
                words: words.len(),
                max: MAX_EFFECT_UNIFORMS,
            });
        }
        if !words.len().is_multiple_of(UNIFORM_WORDS_PER_UNIT) {
            return Err(ShaderDataError::UniformLayout { words: words.len() });
        }
        let mut block = Self::new();
        block.words[..words.len()].copy_from_slice(words);
        block.len = words.len() as u32;
        Ok(block)
    }

    /// The declared words.
    pub fn words(&self) -> &[f32] {
        &self.words[..self.len as usize]
    }

    /// The number of declared words.
    pub fn len(&self) -> u32 {
        self.len
    }

    /// Whether the block declares no words.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The block's size in bytes.
    pub fn byte_len(&self) -> u32 {
        self.len * 4
    }
}

impl Default for EffectUniforms {
    fn default() -> Self {
        Self::new()
    }
}

/// Uniform words per `vec4` unit.
const UNIFORM_WORDS_PER_UNIT: usize = 4;

/// Everything a backend needs to build one effect pipeline.
///
/// The descriptor is plain data; [`Self::validate`] checks the budgets and the
/// index ranges, and the backend checks what only it can (the driver's SPIR-V
/// parser, the device's limits). `blend` is part of the descriptor because a
/// fixed-function backend bakes it into the pipeline object, so the middle
/// layer's pipeline identity is a `(program, blend)` pair.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct EffectPipelineDesc<'a> {
    /// Diagnostic name, carried into creation failures and logs.
    pub name: &'a str,
    /// Vertex stage.
    pub vertex: ShaderModule<'a>,
    /// Fragment stage.
    pub fragment: ShaderModule<'a>,
    /// Vertex input layout the stages read.
    pub vertex_layout: VertexLayout,
    /// Fixed-function blend state the pipeline is built with.
    pub blend: crate::blend::BlendMode,
    /// Descriptor bindings, in binding order.
    pub bindings: &'a [BindingDesc],
    /// Uniform words the fragment stage reads.
    pub uniform_words: u32,
    /// Uniform word that receives the draw's composed opacity, when the
    /// program reads one. The traversal overwrites it with the composed
    /// layer/window opacity, so the program applies it in its own arithmetic
    /// instead of the backend post-multiplying alpha.
    pub opacity_uniform: Option<u32>,
}

impl<'a> EffectPipelineDesc<'a> {
    /// Builds a descriptor for `name` with its two stages: the unit-quad
    /// layout, source-over blending, no bindings and no uniforms.
    ///
    /// The descriptor is `#[non_exhaustive]`, so this is the only way to build
    /// one outside this crate; the remaining fields are public and assigned by
    /// the caller. The middle layer fills them from a registered program
    /// (a program the shader library already validated), which is why nothing
    /// below the middle layer can fabricate a pipeline out of unrelated parts.
    pub const fn new(name: &'a str, vertex: ShaderModule<'a>, fragment: ShaderModule<'a>) -> Self {
        Self {
            name,
            vertex,
            fragment,
            vertex_layout: VertexLayout::UnitQuad,
            blend: crate::blend::BlendMode::SourceOver,
            bindings: &[],
            uniform_words: 0,
            opacity_uniform: None,
        }
    }

    /// Validates the descriptor's budgets, stages and index ranges.
    ///
    /// # Errors
    ///
    /// Returns [`ShaderDataError::EmptyName`] for a descriptor without a name,
    /// [`ShaderDataError::WrongStage`] for a module in the wrong field, the
    /// module errors of [`ShaderCode::validate`],
    /// [`ShaderDataError::TooManyBindings`],
    /// [`ShaderDataError::DuplicateBinding`] or
    /// [`ShaderDataError::ReservedBinding`] for the binding list,
    /// [`ShaderDataError::UniformOverflow`] or
    /// [`ShaderDataError::UniformLayout`] for `uniform_words`, and
    /// [`ShaderDataError::OpacityUniformOutOfRange`] for an opacity word
    /// outside the block.
    pub fn validate(&self) -> Result<(), ShaderDataError> {
        if self.name.is_empty() {
            return Err(ShaderDataError::EmptyName);
        }
        if self.vertex.stage != ShaderStage::Vertex {
            return Err(ShaderDataError::WrongStage {
                expected: ShaderStage::Vertex,
                got: self.vertex.stage,
            });
        }
        if self.fragment.stage != ShaderStage::Fragment {
            return Err(ShaderDataError::WrongStage {
                expected: ShaderStage::Fragment,
                got: self.fragment.stage,
            });
        }
        self.vertex.validate()?;
        self.fragment.validate()?;
        if self.bindings.len() > MAX_EFFECT_TEXTURES {
            return Err(ShaderDataError::TooManyBindings {
                count: self.bindings.len(),
                max: MAX_EFFECT_TEXTURES,
            });
        }
        for (index, binding) in self.bindings.iter().enumerate() {
            if binding.binding == RESERVED_BINDING {
                return Err(ShaderDataError::ReservedBinding {
                    binding: binding.binding,
                });
            }
            if self.bindings[..index]
                .iter()
                .any(|other| other.binding == binding.binding)
            {
                return Err(ShaderDataError::DuplicateBinding {
                    binding: binding.binding,
                });
            }
        }
        let words = self.uniform_words as usize;
        if words > MAX_EFFECT_UNIFORMS {
            return Err(ShaderDataError::UniformOverflow {
                words,
                max: MAX_EFFECT_UNIFORMS,
            });
        }
        if !words.is_multiple_of(UNIFORM_WORDS_PER_UNIT) {
            return Err(ShaderDataError::UniformLayout { words });
        }
        if let Some(index) = self.opacity_uniform
            && index >= self.uniform_words
        {
            return Err(ShaderDataError::OpacityUniformOutOfRange {
                index,
                words: self.uniform_words,
            });
        }
        Ok(())
    }
}

/// A shader or pipeline descriptor that cannot be used as given.
///
/// Every variant is a refusal, never a clamp: the middle layer surfaces it as
/// its own `ShaderError`, which also carries backend creation failures.
#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[non_exhaustive]
pub enum ShaderDataError {
    /// A descriptor carries no diagnostic name.
    #[error("shader descriptor has no name")]
    EmptyName,
    /// A stage module is empty.
    #[error("{stage} shader module is empty")]
    EmptyModule {
        /// Stage of the empty module.
        stage: ShaderStage,
    },
    /// A stage module is shorter than a SPIR-V header.
    #[error("{stage} shader module is {len} bytes, shorter than the SPIR-V header")]
    TruncatedModule {
        /// Stage of the truncated module.
        stage: ShaderStage,
        /// Declared byte length.
        len: usize,
    },
    /// A stage module's length is not a whole number of SPIR-V words.
    #[error("{stage} shader module is {len} bytes, not a whole number of four-byte words")]
    MisalignedModule {
        /// Stage of the misaligned module.
        stage: ShaderStage,
        /// Declared byte length.
        len: usize,
    },
    /// A stage module does not start with the SPIR-V magic number.
    #[error("{stage} shader module starts with {magic:#010x}, not the SPIR-V magic number")]
    BadSpirVMagic {
        /// Stage of the module.
        stage: ShaderStage,
        /// The first word, read little-endian.
        magic: u32,
    },
    /// A stage module declares a SPIR-V version this build does not accept.
    #[error(
        "{stage} shader module declares SPIR-V version {version:#010x}, newer than this build accepts"
    )]
    UnsupportedSpirVVersion {
        /// Stage of the module.
        stage: ShaderStage,
        /// The version word, read little-endian.
        version: u32,
    },
    /// A module was supplied for the wrong stage.
    #[error("expected a {expected} module, got a {got} one")]
    WrongStage {
        /// Stage the descriptor field requires.
        expected: ShaderStage,
        /// Stage the module declares.
        got: ShaderStage,
    },
    /// A descriptor binds more textures than the API binds.
    #[error("a pipeline binds {count} textures, the limit is {max}")]
    TooManyBindings {
        /// Declared binding count.
        count: usize,
        /// The API's maximum.
        max: usize,
    },
    /// A texture binding uses the index reserved for the uniform block.
    #[error("binding index {binding} is reserved for the uniform block")]
    ReservedBinding {
        /// The reserved index.
        binding: u32,
    },
    /// Two bindings share one binding index.
    #[error("binding index {binding} is declared twice")]
    DuplicateBinding {
        /// The repeated index.
        binding: u32,
    },
    /// A uniform block is longer than the API's budget.
    #[error("a uniform block declares {words} words, the budget is {max}")]
    UniformOverflow {
        /// Declared word count.
        words: usize,
        /// The API's maximum.
        max: usize,
    },
    /// A uniform block is not a whole number of `vec4` units.
    #[error("a uniform block of {words} words is not a whole number of vec4 units")]
    UniformLayout {
        /// Declared word count.
        words: usize,
    },
    /// The opacity word lies outside the declared block.
    #[error("opacity uniform word {index} is outside the {words} declared words")]
    OpacityUniformOutOfRange {
        /// The declared opacity word.
        index: u32,
        /// Declared word count.
        words: u32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal valid SPIR-V module: the five-word header, version 1.0.
    const MODULE: [u8; 20] = [
        0x03, 0x02, 0x23, 0x07, // magic, little-endian
        0x00, 0x00, 0x01, 0x00, // version 1.0
        0x00, 0x00, 0x00, 0x00, // generator
        0x01, 0x00, 0x00, 0x00, // bound
        0x00, 0x00, 0x00, 0x00, // schema
    ];

    fn desc<'a>(bindings: &'a [BindingDesc]) -> EffectPipelineDesc<'a> {
        EffectPipelineDesc {
            name: "copy",
            vertex: ShaderModule::vertex(&MODULE),
            fragment: ShaderModule::fragment(&MODULE),
            vertex_layout: VertexLayout::UnitQuad,
            blend: crate::blend::BlendMode::SourceOver,
            bindings,
            uniform_words: 4,
            opacity_uniform: Some(0),
        }
    }

    #[test]
    fn a_well_formed_descriptor_validates() {
        let bindings = [
            BindingDesc::texture(1, SamplerKind::LinearClamp),
            BindingDesc::texture(2, SamplerKind::NearestClamp),
        ];
        desc(&bindings).validate().expect("valid descriptor");
    }

    #[test]
    fn an_empty_name_is_refused() {
        let bindings = [BindingDesc::texture(1, SamplerKind::LinearClamp)];
        let mut item = desc(&bindings);
        item.name = "";
        assert_eq!(item.validate(), Err(ShaderDataError::EmptyName));
    }

    #[test]
    fn module_shape_errors_name_the_stage() {
        assert_eq!(
            ShaderModule::vertex(&[]).validate(),
            Err(ShaderDataError::EmptyModule {
                stage: ShaderStage::Vertex
            })
        );
        assert_eq!(
            ShaderModule::fragment(&MODULE[..16]).validate(),
            Err(ShaderDataError::TruncatedModule {
                stage: ShaderStage::Fragment,
                len: 16
            })
        );
        let mut misaligned = [0u8; 21];
        misaligned[..20].copy_from_slice(&MODULE);
        assert_eq!(
            ShaderModule::vertex(&misaligned).validate(),
            Err(ShaderDataError::MisalignedModule {
                stage: ShaderStage::Vertex,
                len: 21
            })
        );

        let mut bad_magic = MODULE;
        bad_magic[0] = 0xff;
        assert!(matches!(
            ShaderModule::fragment(&bad_magic).validate(),
            Err(ShaderDataError::BadSpirVMagic { .. })
        ));

        let mut future = MODULE;
        future[4..8].copy_from_slice(&0x0001_0600u32.to_le_bytes());
        assert_eq!(
            ShaderModule::vertex(&future).validate(),
            Err(ShaderDataError::UnsupportedSpirVVersion {
                stage: ShaderStage::Vertex,
                version: 0x0001_0600
            })
        );
    }

    #[test]
    fn a_module_for_the_wrong_stage_is_refused() {
        let bindings = [BindingDesc::texture(1, SamplerKind::LinearClamp)];
        let mut item = desc(&bindings);
        item.vertex = ShaderModule::fragment(&MODULE);
        assert_eq!(
            item.validate(),
            Err(ShaderDataError::WrongStage {
                expected: ShaderStage::Vertex,
                got: ShaderStage::Fragment,
            })
        );
    }

    #[test]
    fn binding_budgets_and_indices_are_refused_not_clamped() {
        let five = [
            BindingDesc::texture(1, SamplerKind::LinearClamp),
            BindingDesc::texture(2, SamplerKind::LinearClamp),
            BindingDesc::texture(3, SamplerKind::LinearClamp),
            BindingDesc::texture(4, SamplerKind::LinearClamp),
            BindingDesc::texture(5, SamplerKind::LinearClamp),
        ];
        assert_eq!(
            desc(&five).validate(),
            Err(ShaderDataError::TooManyBindings {
                count: 5,
                max: MAX_EFFECT_TEXTURES
            })
        );

        let repeated = [
            BindingDesc::texture(1, SamplerKind::LinearClamp),
            BindingDesc::texture(1, SamplerKind::NearestClamp),
        ];
        assert_eq!(
            desc(&repeated).validate(),
            Err(ShaderDataError::DuplicateBinding { binding: 1 })
        );

        // Index 0 belongs to the uniform block, so a texture cannot shadow it.
        let shadowing = [BindingDesc::texture(
            RESERVED_BINDING,
            SamplerKind::LinearClamp,
        )];
        assert_eq!(
            desc(&shadowing).validate(),
            Err(ShaderDataError::ReservedBinding { binding: 0 })
        );
    }

    #[test]
    fn the_constructor_is_the_documented_neutral_descriptor() {
        let item = EffectPipelineDesc::new(
            "copy",
            ShaderModule::vertex(&MODULE),
            ShaderModule::fragment(&MODULE),
        );
        assert_eq!(item.vertex_layout, VertexLayout::UnitQuad);
        assert_eq!(item.blend, crate::blend::BlendMode::SourceOver);
        assert!(item.bindings.is_empty());
        assert_eq!(item.uniform_words, 0);
        assert_eq!(item.opacity_uniform, None);
        item.validate()
            .expect("the neutral descriptor is valid on its own");

        // The caller completes it by assignment, and each part is validated.
        let bindings = [BindingDesc::texture(1, SamplerKind::LinearClamp)];
        let mut completed = item;
        completed.bindings = &bindings;
        completed.uniform_words = 4;
        completed.opacity_uniform = Some(3);
        completed
            .validate()
            .expect("a completed descriptor is valid");
    }

    #[test]
    fn uniform_budgets_are_refused_not_clamped() {
        let bindings = [BindingDesc::texture(1, SamplerKind::LinearClamp)];
        let mut item = desc(&bindings);
        item.uniform_words = (MAX_EFFECT_UNIFORMS + 4) as u32;
        assert_eq!(
            item.validate(),
            Err(ShaderDataError::UniformOverflow {
                words: MAX_EFFECT_UNIFORMS + 4,
                max: MAX_EFFECT_UNIFORMS
            })
        );

        item.uniform_words = 6;
        assert_eq!(
            item.validate(),
            Err(ShaderDataError::UniformLayout { words: 6 })
        );
    }

    #[test]
    fn the_opacity_word_must_lie_inside_the_block() {
        let bindings = [BindingDesc::texture(1, SamplerKind::LinearClamp)];
        let mut item = desc(&bindings);
        item.uniform_words = 4;
        item.opacity_uniform = Some(4);
        assert_eq!(
            item.validate(),
            Err(ShaderDataError::OpacityUniformOutOfRange { index: 4, words: 4 })
        );
        item.opacity_uniform = None;
        item.validate().expect("a program may read no opacity");
    }

    #[test]
    fn the_uniform_block_checks_its_own_bounds_and_alignment() {
        let block = EffectUniforms::from_words(&[0.5, 1.0, 2.0, 3.0]).expect("aligned block");
        assert_eq!(block.len(), 4);
        assert_eq!(block.byte_len(), 16);
        assert_eq!(block.words(), &[0.5, 1.0, 2.0, 3.0]);

        assert_eq!(
            EffectUniforms::from_words(&[0.0, 1.0]),
            Err(ShaderDataError::UniformLayout { words: 2 })
        );
        let too_many = [0.0f32; MAX_EFFECT_UNIFORMS + 4];
        assert_eq!(
            EffectUniforms::from_words(&too_many),
            Err(ShaderDataError::UniformOverflow {
                words: MAX_EFFECT_UNIFORMS + 4,
                max: MAX_EFFECT_UNIFORMS
            })
        );
        assert!(EffectUniforms::new().is_empty());
        assert_eq!(EffectUniforms::from_words(&[]), Ok(EffectUniforms::new()));
    }

    #[test]
    fn the_declared_limits_are_the_contract() {
        assert_eq!(MAX_EFFECT_TEXTURES, 4);
        assert_eq!(MAX_EFFECT_UNIFORMS, 64);
        assert_eq!(EFFECT_PUSH_CONSTANT_BYTES, 64);
    }
}
