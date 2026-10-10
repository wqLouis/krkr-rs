//! Graphics pipelines, descriptor set layout, sampler and the unit quad.
//!
//! Two programs are compiled: a textured quad and a solid quad. Each is
//! instantiated once per fixed-function blend mode, so a draw only selects a
//! pipeline and never rebuilds state. Destination-dependent blend modes need
//! a shader that samples the destination, which this backend does not
//! implement; [`PipelineSet::pipeline_for`] reports them as
//! [`RenderError::Unsupported`] instead of silently degrading to source-over.
#![allow(unsafe_code)]

use ash::vk;

use api::blend::BlendMode;
use api::error::RenderError;

use super::shaders;
use super::texture::find_memory_type;

/// Number of fixed-function blend variants compiled per program.
pub(crate) const BLEND_SLOT_COUNT: usize = 7;

/// Which quad program a draw uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Program {
    /// Samples a texture and multiplies it by the tint.
    Textured,
    /// Outputs the tint color directly.
    Solid,
}

/// Compiled pipelines, descriptor layout, sampler and shared unit quad.
pub(crate) struct PipelineSet {
    /// Descriptor set layout for the combined image sampler.
    pub(crate) descriptor_set_layout: vk::DescriptorSetLayout,
    /// Pipeline layout (one descriptor set + push constants).
    pub(crate) pipeline_layout: vk::PipelineLayout,
    /// Pool that backs one descriptor set per texture.
    pub(crate) descriptor_pool: vk::DescriptorPool,
    /// Sampler bound for every textured draw.
    pub(crate) sampler: vk::Sampler,
    /// Textured pipelines indexed by blend slot.
    pub(crate) textured: Vec<vk::Pipeline>,
    /// Solid pipelines indexed by blend slot.
    pub(crate) solid: Vec<vk::Pipeline>,
    /// Host-visible unit-quad vertex buffer.
    pub(crate) vertex_buffer: vk::Buffer,
    /// Backing memory of [`Self::vertex_buffer`].
    pub(crate) vertex_memory: vk::DeviceMemory,
}

impl PipelineSet {
    /// Returns the pipeline for `program` and `mode`.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unsupported`] for a destination-dependent blend
    /// mode, which requires a destination-sampling shader this backend does
    /// not implement.
    pub(crate) fn pipeline_for(
        &self,
        program: Program,
        mode: BlendMode,
    ) -> Result<vk::Pipeline, RenderError> {
        let slot = blend_slot(mode).ok_or_else(|| RenderError::Unsupported {
            message: format!(
                "blend mode {mode:?} requires destination sampling, which the Vulkan backend \
                 does not implement"
            ),
        })?;
        Ok(match program {
            Program::Textured => self.textured[slot],
            Program::Solid => self.solid[slot],
        })
    }

    /// Destroys every resource owned by the set.
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
            device.destroy_pipeline_layout(self.pipeline_layout, None);
            device.destroy_descriptor_pool(self.descriptor_pool, None);
            device.destroy_descriptor_set_layout(self.descriptor_set_layout, None);
            device.destroy_sampler(self.sampler, None);
            device.destroy_buffer(self.vertex_buffer, None);
            device.free_memory(self.vertex_memory, None);
        }
    }
}

/// Builds all pipelines and the shared descriptor/sampler/quad resources.
pub(crate) fn create(
    device: &ash::Device,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    render_pass: vk::RenderPass,
) -> Result<PipelineSet, RenderError> {
    let descriptor_set_layout = create_descriptor_set_layout(device)?;

    let pipeline_layout = match create_pipeline_layout(device, descriptor_set_layout) {
        Ok(layout) => layout,
        Err(error) => {
            // SAFETY: the layout was created here and nothing else uses it.
            unsafe { device.destroy_descriptor_set_layout(descriptor_set_layout, None) };
            return Err(error);
        }
    };

    let descriptor_pool = match create_descriptor_pool(device) {
        Ok(pool) => pool,
        Err(error) => {
            // SAFETY: both handles were created here and nothing else uses them.
            unsafe {
                device.destroy_pipeline_layout(pipeline_layout, None);
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
                device.destroy_pipeline_layout(pipeline_layout, None);
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
                device.destroy_pipeline_layout(pipeline_layout, None);
                device.destroy_descriptor_set_layout(descriptor_set_layout, None);
            }
            return Err(error);
        }
    };

    let vertex_module = create_shader_module(device, shaders::QUAD_VERT)?;
    let textured_fragment = create_shader_module(device, shaders::TEXTURED_FRAG)?;
    let solid_fragment = create_shader_module(device, shaders::SOLID_FRAG)?;

    let mut textured = Vec::with_capacity(BLEND_SLOT_COUNT);
    let mut solid = Vec::with_capacity(BLEND_SLOT_COUNT);
    let mut failure = None;
    for slot in 0..BLEND_SLOT_COUNT {
        if failure.is_none() {
            match build_pipeline(
                device,
                pipeline_layout,
                render_pass,
                vertex_module,
                textured_fragment,
                slot,
            ) {
                Ok(pipeline) => textured.push(pipeline),
                Err(error) => failure = Some(error),
            }
        }
        if failure.is_none() {
            match build_pipeline(
                device,
                pipeline_layout,
                render_pass,
                vertex_module,
                solid_fragment,
                slot,
            ) {
                Ok(pipeline) => solid.push(pipeline),
                Err(error) => failure = Some(error),
            }
        }
    }

    // SAFETY: the shader modules are only referenced through the pipelines,
    // which have already captured the code.
    unsafe {
        device.destroy_shader_module(vertex_module, None);
        device.destroy_shader_module(textured_fragment, None);
        device.destroy_shader_module(solid_fragment, None);
    }

    if let Some(error) = failure {
        let set = PipelineSet {
            descriptor_set_layout,
            pipeline_layout,
            descriptor_pool,
            sampler,
            textured,
            solid,
            vertex_buffer,
            vertex_memory,
        };
        set.destroy(device);
        return Err(error);
    }

    Ok(PipelineSet {
        descriptor_set_layout,
        pipeline_layout,
        descriptor_pool,
        sampler,
        textured,
        solid,
        vertex_buffer,
        vertex_memory,
    })
}

/// Maps a [`BlendMode`] to its fixed-function pipeline slot.
///
/// Destination-dependent modes have no slot; `None` tells
/// [`PipelineSet::pipeline_for`] to report them as unsupported.
fn blend_slot(mode: BlendMode) -> Option<usize> {
    match mode {
        BlendMode::SourceOver => Some(0),
        BlendMode::Replace => Some(1),
        BlendMode::Additive => Some(2),
        BlendMode::Subtractive => Some(3),
        BlendMode::AddAlpha => Some(4),
        BlendMode::Multiply => Some(5),
        BlendMode::Screen => Some(6),
        // These modes must read the destination color, so the fixed-function
        // blend state cannot express them.
        BlendMode::DestinationDependent(_) => None,
    }
}

/// Returns the fixed-function blend state for a pipeline slot.
///
/// `AddAlpha` uses an additive color
/// path with the source alpha replacing the destination alpha, an
/// approximation of the legacy KiriKiri rule.
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

/// Creates the pipeline layout that binds the sampler and 96 bytes of push
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
    // SAFETY: `create_info` borrows a local valid for the call.
    unsafe { device.create_descriptor_pool(&create_info, None) }.map_err(|error| {
        RenderError::OutOfMemory {
            message: format!("failed to create the descriptor pool: {error}"),
        }
    })
}

/// Creates a clamp-to-edge linear sampler with no mip filtering.
fn create_sampler(device: &ash::Device) -> Result<vk::Sampler, RenderError> {
    let create_info = vk::SamplerCreateInfo::default()
        .mag_filter(vk::Filter::LINEAR)
        .min_filter(vk::Filter::LINEAR)
        .mipmap_mode(vk::SamplerMipmapMode::NEAREST)
        .address_mode_u(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_v(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .address_mode_w(vk::SamplerAddressMode::CLAMP_TO_EDGE)
        .anisotropy_enable(false)
        .unnormalized_coordinates(false)
        .min_lod(0.0)
        .max_lod(0.0);
    // SAFETY: `create_info` borrows a local valid for the call.
    unsafe { device.create_sampler(&create_info, None) }.map_err(|error| RenderError::Unsupported {
        message: format!("failed to create the texture sampler: {error}"),
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
    // `PipelineSet::destroy`.
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
fn create_shader_module(
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
fn build_pipeline(
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

    let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
    let dynamic_state =
        vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);

    let pipeline_info = vk::GraphicsPipelineCreateInfo::default()
        .stages(&stages)
        .vertex_input_state(&vertex_input)
        .input_assembly_state(&input_assembly)
        .viewport_state(&viewport_state)
        .rasterization_state(&rasterization)
        .multisample_state(&multisample)
        .color_blend_state(&color_blend)
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
