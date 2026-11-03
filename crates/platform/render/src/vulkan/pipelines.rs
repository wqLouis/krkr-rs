//! Graphics pipelines, descriptor set layouts, sampler and the unit quad.
//!
//! Two programs are compiled: a textured quad and a solid quad. Each is
//! instantiated once per fixed-function blend mode, so a draw only selects a
//! pipeline and never rebuilds state.
//!
//! A third program blends a source over a destination it samples: the
//! destination-dependent blend modes cannot be expressed with fixed-function
//! blend state, so the program reads a copy of the destination and computes
//! the blend itself. The copy is bound as the program's second descriptor
//! set, and the mode's index travels in the push constants, so one pipeline
//! serves every destination-dependent mode.
//!
//! The descriptor layouts, the descriptor pool, the sampler and the unit quad
//! do not depend on the render pass, so [`PipelineResources`] owns them and
//! every [`PipelineSet`] — one per render pass — shares them.
#![allow(unsafe_code)]

use ash::vk;

use api::blend::{BlendMode, DstBlend};
use api::error::RenderError;

use super::device;
use super::shaders;
use super::texture::find_memory_type;

/// Number of fixed-function blend variants compiled per program.
pub(crate) const BLEND_SLOT_COUNT: usize = 8;

/// Which quad program a draw uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Program {
    /// Samples a texture and multiplies it by the tint.
    Textured,
    /// Outputs the tint color directly.
    Solid,
    /// Samples the source texture and a copy of the destination and computes
    /// the destination-dependent blend.
    DstBlend,
}

/// The descriptor and layout resources every pipeline set shares.
///
/// The layouts, the pool, the sampler and the unit quad are independent of
/// the render pass the pipelines are built against, so one set of them serves
/// every pipeline set and every texture.
#[derive(Clone, Copy)]
pub(crate) struct PipelineResources {
    /// Descriptor set layout for the combined image sampler.
    pub(crate) descriptor_set_layout: vk::DescriptorSetLayout,
    /// Descriptor set layout for the destination copy's sampler.
    pub(crate) dst_descriptor_set_layout: vk::DescriptorSetLayout,
    /// Pipeline layout (one descriptor set + push constants).
    pub(crate) pipeline_layout: vk::PipelineLayout,
    /// Pipeline layout for the destination-dependent program: the source's
    /// descriptor set, the destination copy's descriptor set and the push
    /// constants.
    pub(crate) dst_pipeline_layout: vk::PipelineLayout,
    /// Pool that backs one descriptor set per texture.
    pub(crate) descriptor_pool: vk::DescriptorPool,
    /// Sampler bound for every textured draw.
    pub(crate) sampler: vk::Sampler,
    /// Host-visible unit-quad vertex buffer.
    pub(crate) vertex_buffer: vk::Buffer,
    /// Backing memory of [`Self::vertex_buffer`].
    pub(crate) vertex_memory: vk::DeviceMemory,
}

/// Compiled pipelines for one render pass.
pub(crate) struct PipelineSet {
    /// Textured pipelines indexed by blend slot.
    pub(crate) textured: Vec<vk::Pipeline>,
    /// Solid pipelines indexed by blend slot.
    pub(crate) solid: Vec<vk::Pipeline>,
    /// The destination-dependent program.
    pub(crate) dst_blend: vk::Pipeline,
}

impl PipelineSet {
    /// Returns the pipeline for `program` and `mode`.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unsupported`] for a destination-dependent mode
    /// drawn with a program that has no destination to sample.
    pub(crate) fn pipeline_for(
        &self,
        program: Program,
        mode: BlendMode,
    ) -> Result<vk::Pipeline, RenderError> {
        if matches!(program, Program::DstBlend) {
            return Ok(self.dst_blend);
        }
        let slot = blend_slot(mode).ok_or_else(|| RenderError::Unsupported {
            message: format!(
                "blend mode {mode:?} reads the destination, which the {program:?} program does \
                 not sample"
            ),
        })?;
        Ok(match program {
            Program::Textured => self.textured[slot],
            Program::Solid => self.solid[slot],
            Program::DstBlend => self.dst_blend,
        })
    }

    /// Destroys every pipeline owned by the set.
    pub(crate) fn destroy(&self, device: &ash::Device) {
        // SAFETY: all handles were created by `device`; the caller guarantees
        // the GPU is idle.
        unsafe {
            for pipeline in &self.textured {
                device.destroy_pipeline(*pipeline, None);
            }
            for pipeline in &self.solid {
                device.destroy_pipeline(*pipeline, None);
            }
            device.destroy_pipeline(self.dst_blend, None);
        }
    }
}

impl PipelineResources {
    /// Builds the descriptor layouts, the pool, the sampler and the unit quad.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::OutOfMemory`] when a resource cannot be created.
    pub(crate) fn create(
        device: &ash::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
    ) -> Result<Self, RenderError> {
        let descriptor_set_layout = create_descriptor_set_layout(device)?;
        let dst_descriptor_set_layout = match create_dst_descriptor_set_layout(device) {
            Ok(layout) => layout,
            Err(error) => {
                // SAFETY: the layout was created here and nothing else uses it.
                unsafe { device.destroy_descriptor_set_layout(descriptor_set_layout, None) };
                return Err(error);
            }
        };
        let pipeline_layout = match create_pipeline_layout(device, descriptor_set_layout) {
            Ok(layout) => layout,
            Err(error) => {
                // SAFETY: the layouts were created here and nothing else uses them.
                unsafe {
                    device.destroy_descriptor_set_layout(dst_descriptor_set_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(error);
            }
        };
        let dst_pipeline_layout = match create_dst_pipeline_layout(
            device,
            descriptor_set_layout,
            dst_descriptor_set_layout,
        ) {
            Ok(layout) => layout,
            Err(error) => {
                // SAFETY: the layouts were created here and nothing else uses them.
                unsafe {
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(dst_descriptor_set_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(error);
            }
        };
        let descriptor_pool = match create_descriptor_pool(device) {
            Ok(pool) => pool,
            Err(error) => {
                // SAFETY: both handles were created here and are unused.
                unsafe {
                    device.destroy_pipeline_layout(dst_pipeline_layout, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(dst_descriptor_set_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(error);
            }
        };
        let sampler = match create_sampler(device) {
            Ok(sampler) => sampler,
            Err(error) => {
                // SAFETY: all three handles were created here and are unused.
                unsafe {
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline_layout(dst_pipeline_layout, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(dst_descriptor_set_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(error);
            }
        };
        let (vertex_buffer, vertex_memory) = match create_unit_quad(device, memory_properties) {
            Ok(quad) => quad,
            Err(error) => {
                // SAFETY: all four handles were created here and are unused.
                unsafe {
                    device.destroy_sampler(sampler, None);
                    device.destroy_descriptor_pool(descriptor_pool, None);
                    device.destroy_pipeline_layout(dst_pipeline_layout, None);
                    device.destroy_pipeline_layout(pipeline_layout, None);
                    device.destroy_descriptor_set_layout(dst_descriptor_set_layout, None);
                    device.destroy_descriptor_set_layout(descriptor_set_layout, None);
                }
                return Err(error);
            }
        };

        Ok(Self {
            descriptor_set_layout,
            dst_descriptor_set_layout,
            pipeline_layout,
            dst_pipeline_layout,
            descriptor_pool,
            sampler,
            vertex_buffer,
            vertex_memory,
        })
    }

    /// Destroys every resource owned by the resources.
    pub(crate) fn destroy(&self, device: &ash::Device) {
        // SAFETY: all handles were created by `device`; the caller guarantees
        // the GPU is idle.
        unsafe {
            device.destroy_pipeline_layout(self.dst_pipeline_layout, None);
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_descriptor_set_layout(self.dst_descriptor_set_layout, None);
            device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            device.destroy_descriptor_pool(self.descriptor_pool, None);
            device.destroy_sampler(self.sampler, None);
            device.destroy_buffer(self.vertex_buffer, None);
            device.free_memory(self.vertex_memory, None);
        }
    }
}

/// Builds the pipelines for one render pass against the shared resources.
///
/// # Errors
///
/// Returns [`RenderError::Shader`] when a shader module or a pipeline cannot
/// be created.
pub(crate) fn create(
    device: &ash::Device,
    render_pass: vk::RenderPass,
    resources: &PipelineResources,
) -> Result<PipelineSet, RenderError> {
    let vertex_module = create_shader_module(device, shaders::QUAD_VERT)?;
    let textured_fragment = create_shader_module(device, shaders::TEXTURED_FRAG)?;
    let solid_fragment = create_shader_module(device, shaders::SOLID_FRAG)?;
    let dst_fragment = create_shader_module(device, shaders::DST_BLEND_FRAG)?;

    let mut textured = Vec::with_capacity(BLEND_SLOT_COUNT);
    let mut solid = Vec::with_capacity(BLEND_SLOT_COUNT);
    let mut dst_blend = vk::Pipeline::null();

    let outcome = (|| -> Result<(), RenderError> {
        for slot in 0..BLEND_SLOT_COUNT {
            textured.push(build_pipeline(
                device,
                resources.pipeline_layout,
                render_pass,
                vertex_module,
                textured_fragment,
                slot,
            )?);
            solid.push(build_pipeline(
                device,
                resources.pipeline_layout,
                render_pass,
                vertex_module,
                solid_fragment,
                slot,
            )?);
        }
        // The destination-dependent program writes the blended colour itself,
        // so its fixed-function state disables blending.
        dst_blend = build_dst_pipeline(
            device,
            resources.dst_pipeline_layout,
            render_pass,
            vertex_module,
            dst_fragment,
        )?;
        Ok(())
    })();

    // SAFETY: the shader modules are only referenced through the pipelines,
    // which have already captured the code.
    unsafe {
        device.destroy_shader_module(vertex_module, None);
        device.destroy_shader_module(textured_fragment, None);
        device.destroy_shader_module(solid_fragment, None);
        device.destroy_shader_module(dst_fragment, None);
    }

    match outcome {
        Ok(()) => Ok(PipelineSet {
            textured,
            solid,
            dst_blend,
        }),
        Err(error) => {
            // SAFETY: the pipelines were created here and nothing else uses
            // them; the modules that built them are already destroyed.
            unsafe {
                for pipeline in &textured {
                    device.destroy_pipeline(*pipeline, None);
                }
                for pipeline in &solid {
                    device.destroy_pipeline(*pipeline, None);
                }
                if dst_blend != vk::Pipeline::null() {
                    device.destroy_pipeline(dst_blend, None);
                }
            }
            Err(error)
        }
    }
}

/// Maps a [`BlendMode`] to its fixed-function pipeline slot.
///
/// Destination-dependent modes have no slot; the destination-dependent
/// program serves them instead.
pub(crate) fn blend_slot(mode: BlendMode) -> Option<usize> {
    match mode {
        // The source-over is the alpha-correct blend; the Photoshop normal is
        // the channel mix with the destination's alpha held, so it needs its
        // own slot.
        BlendMode::SourceOver => Some(0),
        BlendMode::PsNormal => Some(7),
        BlendMode::Replace => Some(1),
        // Slot 2 is `src·a + dst` while the Photoshop additive blend is
        // `lerp(dst, min(1, dst+src), a)`; they differ only at the saturation
        // boundary. Reusing the slot is the same near-identical
        // approximation the multiplicative/screen arms make, and it keeps a
        // layer that rendered before from failing the frame.
        BlendMode::Additive | BlendMode::PsAdditive => Some(2),
        // The subtractive blend is the saturating difference `dst - src·a`.
        // The Photoshop subtractive blend is the linear burn, which no
        // fixed-function blend state expresses: the destination-dependent
        // program computes it from the destination it samples.
        BlendMode::Subtractive => Some(3),
        BlendMode::PsSubtractive => None,
        BlendMode::AddAlpha => Some(4),
        // The Photoshop multiplicative/screen fixed-function slots carry the
        // same per-channel blend function; the source-alpha weighting applied
        // around it is the approximation the non-Photoshop slots already make.
        BlendMode::Multiply | BlendMode::PsMultiplicative => Some(5),
        BlendMode::Screen | BlendMode::PsScreen => Some(6),
        // The destination-dependent modes must read the destination color and
        // have no fixed-function equivalent.
        BlendMode::DestinationDependent(_) => None,
    }
}

/// The mode index of a destination-dependent blend in the blend program.
///
/// The program's switch implements the same set; every mode has an index of
/// its own, and the index travels in the push constants. The order matches
/// the shader's `shaders/dst_blend.frag`.
///
/// `None` means the mode has a fixed-function slot and never reaches the
/// destination-dependent program.
pub(crate) const fn dst_blend_mode(mode: BlendMode) -> Option<u32> {
    match mode {
        BlendMode::DestinationDependent(kind) => Some(match kind {
            DstBlend::ColorDodge => 0,
            DstBlend::PsColorDodge => 1,
            DstBlend::ColorDodge5 => 2,
            DstBlend::ColorBurn => 3,
            DstBlend::Darken => 4,
            DstBlend::PsDarken => 5,
            DstBlend::Lighten => 6,
            DstBlend::PsLighten => 7,
            DstBlend::Difference => 8,
            DstBlend::Difference5 => 9,
            DstBlend::Exclusion => 10,
            DstBlend::Overlay => 11,
            DstBlend::HardLight => 12,
            DstBlend::SoftLight => 13,
        }),
        // The Photoshop subtractive blend is the linear burn, which no
        // fixed-function blend state expresses, so the program computes it
        // from the destination it samples.
        BlendMode::PsSubtractive => Some(14),
        BlendMode::SourceOver
        | BlendMode::Replace
        | BlendMode::Additive
        | BlendMode::Subtractive
        | BlendMode::AddAlpha
        | BlendMode::Multiply
        | BlendMode::Screen
        | BlendMode::PsNormal
        | BlendMode::PsAdditive
        | BlendMode::PsMultiplicative
        | BlendMode::PsScreen => None,
    }
}

/// Returns the fixed-function blend state for a pipeline slot.
///
/// `AddAlpha` uses an additive color
/// path with the source alpha replacing the destination alpha, an
/// approximation of the engine's legacy rule.
fn blend_state_for_slot(slot: usize) -> vk::PipelineColorBlendAttachmentState {
    let base = vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(true)
        .color_write_mask(vk::ColorComponentFlags::RGBA);

    match slot {
        1 => base
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ZERO)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
            .alpha_blend_op(vk::BlendOp::ADD),
        2 => base
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD),
        3 => base
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::REVERSE_SUBTRACT)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::REVERSE_SUBTRACT),
        4 => base
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
            .alpha_blend_op(vk::BlendOp::ADD),
        5 => base
            .src_color_blend_factor(vk::BlendFactor::DST_COLOR)
            .dst_color_blend_factor(vk::BlendFactor::ZERO)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::DST_ALPHA)
            .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
            .alpha_blend_op(vk::BlendOp::ADD),
        6 => base
            .src_color_blend_factor(vk::BlendFactor::ONE_MINUS_DST_COLOR)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_DST_ALPHA)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD),
        // Slot 7: the Photoshop normal - the per-channel mix `src*a +
        // dst*(1-a)` with the destination's alpha held: the colour factors
        // are `SRC_ALPHA`/`ONE_MINUS_SRC_ALPHA` and the alpha factors
        // `ZERO`/`ONE`.
        7 => base
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD),
        // Slot 0: source-over.
        _ => base
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD),
    }
}

/// Creates the combined-image-sampler descriptor set layout.
fn create_descriptor_set_layout(
    device: &ash::Device,
) -> Result<vk::DescriptorSetLayout, RenderError> {
    let binding = vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT);
    let bindings = [binding];
    let create_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: `create_info` borrows a local valid for the call.
    unsafe { device.create_descriptor_set_layout(&create_info, None) }.map_err(|error| {
        RenderError::Shader {
            message: format!("failed to create the descriptor set layout: {error}"),
        }
    })
}

/// Creates the descriptor set layout the destination copy is bound through.
fn create_dst_descriptor_set_layout(
    device: &ash::Device,
) -> Result<vk::DescriptorSetLayout, RenderError> {
    let binding = vk::DescriptorSetLayoutBinding::default()
        .binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1)
        .stage_flags(vk::ShaderStageFlags::FRAGMENT);
    let bindings = [binding];
    let create_info = vk::DescriptorSetLayoutCreateInfo::default().bindings(&bindings);
    // SAFETY: `create_info` borrows a local valid for the call.
    unsafe { device.create_descriptor_set_layout(&create_info, None) }.map_err(|error| {
        RenderError::Shader {
            message: format!("failed to create the destination descriptor set layout: {error}"),
        }
    })
}

/// Creates the pipeline layout that binds the sampler and the push
/// constants shared by the vertex and fragment stages.
fn create_pipeline_layout(
    device: &ash::Device,
    descriptor_set_layout: vk::DescriptorSetLayout,
) -> Result<vk::PipelineLayout, RenderError> {
    let set_layouts = [descriptor_set_layout];
    let push_constant = vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
        .offset(0)
        .size(super::PUSH_CONSTANT_SIZE);
    let push_constants = [push_constant];
    let create_info = vk::PipelineLayoutCreateInfo::default()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_constants);
    // SAFETY: `create_info` borrows locals valid for the call.
    unsafe { device.create_pipeline_layout(&create_info, None) }.map_err(|error| {
        RenderError::Shader {
            message: format!("failed to create the pipeline layout: {error}"),
        }
    })
}

/// Creates the pipeline layout the destination-dependent program uses.
///
/// The first set binds the source texture, the second the copy of the
/// destination the blend reads.
fn create_dst_pipeline_layout(
    device: &ash::Device,
    descriptor_set_layout: vk::DescriptorSetLayout,
    dst_descriptor_set_layout: vk::DescriptorSetLayout,
) -> Result<vk::PipelineLayout, RenderError> {
    let set_layouts = [descriptor_set_layout, dst_descriptor_set_layout];
    let push_constant = vk::PushConstantRange::default()
        .stage_flags(vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT)
        .offset(0)
        .size(super::PUSH_CONSTANT_SIZE);
    let push_constants = [push_constant];
    let create_info = vk::PipelineLayoutCreateInfo::default()
        .set_layouts(&set_layouts)
        .push_constant_ranges(&push_constants);
    // SAFETY: `create_info` borrows locals valid for the call.
    unsafe { device.create_pipeline_layout(&create_info, None) }.map_err(|error| {
        RenderError::Shader {
            message: format!("failed to create the destination pipeline layout: {error}"),
        }
    })
}

/// Creates the descriptor pool used for per-texture descriptor sets.
fn create_descriptor_pool(device: &ash::Device) -> Result<vk::DescriptorPool, RenderError> {
    let pool_size = vk::DescriptorPoolSize::default()
        .ty(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .descriptor_count(1024);
    let pool_sizes = [pool_size];
    let create_info = vk::DescriptorPoolCreateInfo::default()
        .pool_sizes(&pool_sizes)
        .max_sets(1024)
        .flags(vk::DescriptorPoolCreateFlags::FREE_DESCRIPTOR_SET);
    // SAFETY: `create_info` borrows locals valid for the call.
    unsafe { device.create_descriptor_pool(&create_info, None) }.map_err(|error| {
        RenderError::OutOfMemory {
            message: format!("failed to create the descriptor pool: {error}"),
        }
    })
}

/// Creates a clamp-to-edge bilinear sampler with no mip filtering.
///
/// Magnification and minification interpolate between source texels; at a 1:1
/// mapping the half-texel sample points still select the exact source texel,
/// and scaled presentation is smooth rather than point-sampled.
fn create_sampler(device: &ash::Device) -> Result<vk::Sampler, RenderError> {
    let create_info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::LINEAR)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .anisotropy_enable(false)
        .unnormalized_coordinates(false)
        .min_lod(0.0)
        .max_lod(0.0);
    // SAFETY: `create_info` borrows locals valid for the call.
    unsafe { device.create_sampler(&create_info, None) }.map_err(|error| {
        RenderError::DeviceUnavailable {
            message: format!(
                "failed to create the texture sampler: {}",
                device::vk_result_message(error)
            ),
        }
    })
}

/// Creates the host-visible unit-quad vertex buffer.
///
/// The quad is `(0,0) (1,0) (0,1) (1,1)` drawn as a triangle strip; the vertex
/// shader maps it through the per-node push constant matrix.
fn create_unit_quad(
    device: &ash::Device,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
) -> Result<(vk::Buffer, vk::DeviceMemory), RenderError> {
    const VERTICES: [f32; 8] = [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    let size = std::mem::size_of_val(&VERTICES) as vk::DeviceSize;

    let buffer_info = vk::BufferCreateInfo::default()
        .size(size)
        .usage(vk::BufferUsageFlags::VERTEX_BUFFER)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    // SAFETY: `buffer_info` borrows a local valid for the call.
    let buffer = unsafe { device.create_buffer(&buffer_info, None) }.map_err(|error| {
        RenderError::OutOfMemory {
            message: format!("failed to create the unit-quad buffer: {error}"),
        }
    })?;

    // SAFETY: `buffer` was just created by `device`.
    let requirements = unsafe { device.get_buffer_memory_requirements(buffer) };
    let memory_type = match find_memory_type(
        memory_properties,
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
    ) {
        Ok(memory_type) => memory_type,
        Err(error) => {
            // SAFETY: `buffer` is owned here and not used elsewhere yet.
            unsafe { device.destroy_buffer(buffer, None) };
            return Err(error);
        }
    };

    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type);
    // SAFETY: `allocate_info` is valid; the memory is freed with the buffer in
    // `PipelineResources::destroy`.
    let memory = unsafe { device.allocate_memory(&allocate_info, None) }.map_err(|error| {
        // SAFETY: `buffer` is owned here.
        unsafe { device.destroy_buffer(buffer, None) };
        RenderError::OutOfMemory {
            message: format!("failed to allocate unit-quad memory: {error}"),
        }
    })?;

    // SAFETY: `memory` was allocated for `buffer`.
    if let Err(error) = unsafe { device.bind_buffer_memory(buffer, memory, 0) } {
        // SAFETY: both handles are owned here and unused elsewhere yet.
        unsafe {
            device.destroy_buffer(buffer, None);
            device.free_memory(memory, None);
        }
        return Err(RenderError::OutOfMemory {
            message: format!("failed to bind unit-quad memory: {error}"),
        });
    }

    // SAFETY: the memory is host-visible and not currently in use by the GPU.
    let mapped = match unsafe { device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty()) } {
        Ok(mapped) => mapped,
        Err(error) => {
            // SAFETY: both handles are owned here and unused elsewhere yet.
            unsafe {
                device.destroy_buffer(buffer, None);
                device.free_memory(memory, None);
            }
            return Err(RenderError::OutOfMemory {
                message: format!("failed to map unit-quad memory: {error}"),
            });
        }
    };
    // SAFETY: `mapped` points at `size` writable bytes owned by `memory`.
    unsafe {
        std::ptr::copy_nonoverlapping(VERTICES.as_ptr(), mapped.cast::<f32>(), VERTICES.len());
        device.unmap_memory(memory);
    }

    Ok((buffer, memory))
}

/// Loads a precompiled SPIR-V blob into a shader module.
pub(crate) fn create_shader_module(
    device: &ash::Device,
    bytes: &[u8],
) -> Result<vk::ShaderModule, RenderError> {
    let words = super::shaders::to_words(bytes);
    let create_info = vk::ShaderModuleCreateInfo::default().code(&words);
    // SAFETY: `words` is a valid SPIR-V module produced by glslc.
    unsafe { device.create_shader_module(&create_info, None) }.map_err(|error| {
        RenderError::Shader {
            message: format!("failed to create a shader module: {error}"),
        }
    })
}

/// Builds one graphics pipeline for a blend slot.
pub(crate) fn build_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    vertex_module: vk::ShaderModule,
    fragment_module: vk::ShaderModule,
    slot: usize,
) -> Result<vk::Pipeline, RenderError> {
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vertex_module)
            .name(c"main"),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(fragment_module)
            .name(c"main"),
    ];

    let vertex_binding = vk::VertexInputBindingDescription::default()
        .binding(0)
        .stride(8)
        .input_rate(vk::VertexInputRate::VERTEX);
    let vertex_bindings = [vertex_binding];
    let vertex_attribute = vk::VertexInputAttributeDescription::default()
        .location(0)
        .binding(0)
        .format(vk::Format::R32G32_SFLOAT)
        .offset(0);
    let vertex_attributes = [vertex_attribute];
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&vertex_bindings)
        .vertex_attribute_descriptions(&vertex_attributes);

    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_STRIP);

    let viewport_state = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);

    let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);

    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);

    let blend_attachment = blend_state_for_slot(slot);
    let blend_attachments = [blend_attachment];
    let color_blend =
        vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);

    build_graphics_pipeline(
        device,
        layout,
        render_pass,
        &stages,
        &PipelineStates {
            vertex_input: &vertex_input,
            input_assembly: &input_assembly,
            viewport_state: &viewport_state,
            rasterization: &rasterization,
            multisample: &multisample,
            color_blend: &color_blend,
        },
    )
}

/// Builds the destination-dependent program's pipeline.
///
/// The program writes the blended colour itself, so the fixed-function blend
/// state is disabled: the destination copy is read and mixed in the shader.
fn build_dst_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    vertex_module: vk::ShaderModule,
    fragment_module: vk::ShaderModule,
) -> Result<vk::Pipeline, RenderError> {
    let stages = [
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::VERTEX)
            .module(vertex_module)
            .name(c"main"),
        vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::FRAGMENT)
            .module(fragment_module)
            .name(c"main"),
    ];

    let vertex_binding = vk::VertexInputBindingDescription::default()
        .binding(0)
        .stride(8)
        .input_rate(vk::VertexInputRate::VERTEX);
    let vertex_bindings = [vertex_binding];
    let vertex_attribute = vk::VertexInputAttributeDescription::default()
        .location(0)
        .binding(0)
        .format(vk::Format::R32G32_SFLOAT)
        .offset(0);
    let vertex_attributes = [vertex_attribute];
    let vertex_input = vk::PipelineVertexInputStateCreateInfo::default()
        .vertex_binding_descriptions(&vertex_bindings)
        .vertex_attribute_descriptions(&vertex_attributes);

    let input_assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
        .topology(vk::PrimitiveTopology::TRIANGLE_STRIP);

    let viewport_state = vk::PipelineViewportStateCreateInfo::default()
        .viewport_count(1)
        .scissor_count(1);

    let rasterization = vk::PipelineRasterizationStateCreateInfo::default()
        .polygon_mode(vk::PolygonMode::FILL)
        .cull_mode(vk::CullModeFlags::NONE)
        .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
        .line_width(1.0);

    let multisample = vk::PipelineMultisampleStateCreateInfo::default()
        .rasterization_samples(vk::SampleCountFlags::TYPE_1);

    let blend_attachment = vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(false)
        .color_write_mask(vk::ColorComponentFlags::RGBA);
    let blend_attachments = [blend_attachment];
    let color_blend =
        vk::PipelineColorBlendStateCreateInfo::default().attachments(&blend_attachments);

    build_graphics_pipeline(
        device,
        layout,
        render_pass,
        &stages,
        &PipelineStates {
            vertex_input: &vertex_input,
            input_assembly: &input_assembly,
            viewport_state: &viewport_state,
            rasterization: &rasterization,
            multisample: &multisample,
            color_blend: &color_blend,
        },
    )
}

/// The pipeline states every program shares.
struct PipelineStates<'a> {
    /// Vertex input state.
    vertex_input: &'a vk::PipelineVertexInputStateCreateInfo<'a>,
    /// Input assembly state.
    input_assembly: &'a vk::PipelineInputAssemblyStateCreateInfo<'a>,
    /// Viewport state.
    viewport_state: &'a vk::PipelineViewportStateCreateInfo<'a>,
    /// Rasterization state.
    rasterization: &'a vk::PipelineRasterizationStateCreateInfo<'a>,
    /// Multisample state.
    multisample: &'a vk::PipelineMultisampleStateCreateInfo<'a>,
    /// Colour blend state.
    color_blend: &'a vk::PipelineColorBlendStateCreateInfo<'a>,
}

/// Creates a graphics pipeline from the states every program shares.
fn build_graphics_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    stages: &[vk::PipelineShaderStageCreateInfo],
    states: &PipelineStates<'_>,
) -> Result<vk::Pipeline, RenderError> {
    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic_state =
        vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

    let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
        .stages(stages)
        .vertex_input_state(states.vertex_input)
        .input_assembly_state(states.input_assembly)
        .viewport_state(states.viewport_state)
        .rasterization_state(states.rasterization)
        .multisample_state(states.multisample)
        .color_blend_state(states.color_blend)
        .dynamic_state(&dynamic_state)
        .layout(layout)
        .render_pass(render_pass)
        .subpass(0);
    let pipeline_infos = [pipeline_info];

    // SAFETY: all borrowed create-info structures live for the call.
    let mut pipelines = unsafe {
        device.create_graphics_pipelines(vk::PipelineCache::null(), &pipeline_infos, None)
    }
    .map_err(|(_, error)| RenderError::Shader {
        message: format!("failed to create a graphics pipeline: {error}"),
    })?;

    pipelines.pop().ok_or_else(|| RenderError::Shader {
        message: "the graphics pipeline compiler returned no pipeline".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::collections::Set;

    /// Every destination-dependent blend has a mode index of its own.
    ///
    /// The blend program selects its per-channel function with the index, so
    /// two modes sharing an index would draw the same function.
    #[test]
    fn every_destination_dependent_blend_has_a_mode_of_its_own() {
        let blends = [
            BlendMode::DestinationDependent(DstBlend::ColorDodge),
            BlendMode::DestinationDependent(DstBlend::PsColorDodge),
            BlendMode::DestinationDependent(DstBlend::ColorDodge5),
            BlendMode::DestinationDependent(DstBlend::ColorBurn),
            BlendMode::DestinationDependent(DstBlend::Darken),
            BlendMode::DestinationDependent(DstBlend::PsDarken),
            BlendMode::DestinationDependent(DstBlend::Lighten),
            BlendMode::DestinationDependent(DstBlend::PsLighten),
            BlendMode::DestinationDependent(DstBlend::Difference),
            BlendMode::DestinationDependent(DstBlend::Difference5),
            BlendMode::DestinationDependent(DstBlend::Exclusion),
            BlendMode::DestinationDependent(DstBlend::Overlay),
            BlendMode::DestinationDependent(DstBlend::HardLight),
            BlendMode::DestinationDependent(DstBlend::SoftLight),
            // The Photoshop subtractive blend is the linear burn, so it reads
            // the destination like the destination-dependent modes do.
            BlendMode::PsSubtractive,
        ];
        let mut modes: Set<u32> = Set::default();
        for blend in blends {
            let mode = dst_blend_mode(blend).expect("a destination-dependent blend has a mode");
            assert!(
                modes.insert(mode),
                "the blend {blend:?} reuses the index {mode}"
            );
        }
        assert_eq!(modes.len(), blends.len());
    }

    /// The Photoshop subtractive blend has no fixed-function slot.
    ///
    /// The linear burn reads the destination, so the destination-dependent
    /// program draws it; the non-Photoshop subtractive blend keeps the
    /// fixed-function subtract slot.
    #[test]
    fn the_photoshop_subtractive_blend_has_no_fixed_function_slot() {
        assert_eq!(blend_slot(BlendMode::PsSubtractive), None);
        assert_eq!(blend_slot(BlendMode::Subtractive), Some(3));
    }
}
