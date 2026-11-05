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
use super::shaders::{self, DESTINATION_DESCRIPTOR, SOURCE_DESCRIPTOR};
use super::texture::find_memory_type;

/// Number of fixed-function blend variants compiled per program.
pub(crate) const BLEND_SLOT_COUNT: usize = 9;

/// The blend slot that copies the source's colour and keeps the destination's
/// alpha.
///
/// The present draws the composed frame into the surface with it: the frame is
/// the window's image, so the window shows the frame's colour and the
/// surface's own alpha survives. A source-over present would multiply the
/// frame's colour by the frame's alpha and drop whatever the surface held, so
/// a frame that carries colour with a reduced alpha would lose that colour to
/// the surface's cleared background.
///
/// A copy that holds the destination's alpha uses the same state: the
/// source's colour replaces the destination's and the destination's alpha
/// stays.
pub(crate) const KEEP_ALPHA_COPY_SLOT: usize = 8;

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

impl Program {
    /// The program's name, as a failure message names it.
    fn name(&self) -> &'static str {
        match self {
            Program::Textured => "textured",
            Program::Solid => "solid",
            Program::DstBlend => "destination blend",
        }
    }

    /// The description a pipeline failure carries.
    ///
    /// A pipeline that serves a blend slot names the slot; the
    /// destination-dependent program serves no slot, so it names the program
    /// alone.
    fn pipeline_description(&self, slot: Option<usize>) -> String {
        match slot {
            Some(slot) => format!(
                "the {} program's pipeline for blend slot {slot}",
                self.name()
            ),
            None => format!("the {} program's pipeline", self.name()),
        }
    }
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
    /// `hold_alpha` is the node's hold-alpha flag: a blend that holds the
    /// destination's alpha runs its held face.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unsupported`] for a destination-dependent mode
    /// drawn with a program that has no destination to sample.
    pub(crate) fn pipeline_for(
        &self,
        program: Program,
        mode: BlendMode,
        hold_alpha: bool,
    ) -> Result<vk::Pipeline, RenderError> {
        if matches!(program, Program::DstBlend) {
            return Ok(self.dst_blend);
        }
        let slot = blend_slot_for(mode, hold_alpha).ok_or_else(|| RenderError::Unsupported {
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

    /// Returns the pipeline the present draw uses.
    ///
    /// The present has a blend state of its own: it copies the composed
    /// frame's colour into the surface and keeps the surface's alpha, so it
    /// is not one of the scene's blend slots.
    pub(crate) fn present_pipeline(&self, program: Program) -> vk::Pipeline {
        match program {
            Program::Textured => self.textured[KEEP_ALPHA_COPY_SLOT],
            Program::Solid => self.solid[KEEP_ALPHA_COPY_SLOT],
            Program::DstBlend => self.dst_blend,
        }
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
            textured.push(build_program_pipeline(
                device,
                resources.pipeline_layout,
                render_pass,
                vertex_module,
                textured_fragment,
                blend_state_for_slot(slot),
                Program::Textured.pipeline_description(Some(slot)),
            )?);
            solid.push(build_program_pipeline(
                device,
                resources.pipeline_layout,
                render_pass,
                vertex_module,
                solid_fragment,
                blend_state_for_slot(slot),
                Program::Solid.pipeline_description(Some(slot)),
            )?);
        }
        // The destination-dependent program writes the blended colour itself,
        // so its fixed-function state disables blending.
        dst_blend = build_program_pipeline(
            device,
            resources.dst_pipeline_layout,
            render_pass,
            vertex_module,
            dst_fragment,
            no_blending(),
            Program::DstBlend.pipeline_description(None),
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
/// The alpha factors carry the blend's alpha face. A colour-only blend keeps
/// the destination's alpha, which the fixed-function state spells `ZERO` for
/// the source and `ONE` for the destination. The two blends that write an
/// alpha name the source's: the source-over writes `src·a + dst·(1 - a)` and
/// the plain copy writes the source's alpha outright.
///
/// The keep-alpha copy's slot copies the composed frame's colour and keeps the
/// surface's alpha: the frame is the window's image, so its colour is what
/// the window shows, whatever alpha its pixels carry.
fn blend_state_for_slot(slot: usize) -> vk::PipelineColorBlendAttachmentState {
    let base = vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(true)
        .color_write_mask(vk::ColorComponentFlags::RGBA);

    match slot {
        // The plain copy: the source's colour and the source's alpha replace
        // the destination's.
        1 => base
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ZERO)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ZERO)
            .alpha_blend_op(vk::BlendOp::ADD),
        // The additive blend: the colour is `src·a + dst` and the blend is
        // colour-only, so the destination's alpha is kept.
        2 => base
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD),
        // The subtractive blend: the colour is the saturating difference
        // `dst - src·a` and the blend is colour-only, so the destination's
        // alpha is kept.
        3 => base
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::REVERSE_SUBTRACT)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD),
        // The additive-alpha blend: the colour is `src·a + dst` and the blend
        // is colour-only, so the destination's alpha is kept. The blend adds
        // the source's colour to the destination and never touches the
        // destination's alpha.
        4 => base
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD),
        // The multiplicative blend: the colour is the per-channel product
        // `dst·src` and the blend is colour-only, so the destination's alpha
        // is kept.
        5 => base
            .src_color_blend_factor(vk::BlendFactor::DST_COLOR)
            .dst_color_blend_factor(vk::BlendFactor::ZERO)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD),
        // The screen blend: the colour is `dst + src - dst·src` and the blend
        // is colour-only, so the destination's alpha is kept.
        6 => base
            .src_color_blend_factor(vk::BlendFactor::ONE_MINUS_DST_COLOR)
            .dst_color_blend_factor(vk::BlendFactor::ONE)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD),
        // Slot 7: the Photoshop normal - the per-channel mix `src*a +
        // dst*(1-a)` with the destination's alpha held: the colour factors
        // are `SRC_ALPHA`/`ONE_MINUS_SRC_ALPHA` and the alpha factors
        // `ZERO`/`ONE`.
        //
        // The slot is also the held face of the source-over: the same
        // colour mix with the destination's alpha kept.
        7 => base
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD),
        // The keep-alpha copy: the source's colour replaces the destination's
        // and the destination's alpha is kept. The present uses it to copy the
        // composed frame's colour into the surface, so the window shows the
        // frame's colour and stays opaque.
        KEEP_ALPHA_COPY_SLOT => base
            .src_color_blend_factor(vk::BlendFactor::ONE)
            .dst_color_blend_factor(vk::BlendFactor::ZERO)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ZERO)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE)
            .alpha_blend_op(vk::BlendOp::ADD),
        // Slot 0: the source-over. The only blend that writes the source's
        // alpha: the result's alpha is `src·a + dst·(1 - a)`.
        _ => base
            .src_color_blend_factor(vk::BlendFactor::SRC_ALPHA)
            .dst_color_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .color_blend_op(vk::BlendOp::ADD)
            .src_alpha_blend_factor(vk::BlendFactor::ONE)
            .dst_alpha_blend_factor(vk::BlendFactor::ONE_MINUS_SRC_ALPHA)
            .alpha_blend_op(vk::BlendOp::ADD),
    }
}

/// The slot a mode's draw uses.
///
/// A draw that holds the destination's alpha runs the blend's held face: the
/// copy keeps the destination's alpha and the source-over mixes the colours
/// without writing an alpha. Every other blend keeps the destination's alpha
/// on its own, so the flag changes nothing for them.
fn blend_slot_for(mode: BlendMode, hold_alpha: bool) -> Option<usize> {
    match (mode, hold_alpha) {
        // The held copy: the source's colour and the destination's alpha.
        (BlendMode::Replace, true) => Some(KEEP_ALPHA_COPY_SLOT),
        // The held source-over: the Photoshop normal's colour mix with the
        // destination's alpha kept.
        (BlendMode::SourceOver, true) => Some(7),
        _ => blend_slot(mode),
    }
}

/// Creates the combined-image-sampler descriptor set layout.
///
/// The layout binds the source texture's descriptor: the binding is the one
/// the shaders declare for it.
fn create_descriptor_set_layout(
    device: &ash::Device,
) -> Result<vk::DescriptorSetLayout, RenderError> {
    let binding = vk::DescriptorSetLayoutBinding::default()
        .binding(SOURCE_DESCRIPTOR.binding)
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
///
/// The layout binds the copy of the destination: the binding is the one the
/// destination-dependent shader declares for it.
fn create_dst_descriptor_set_layout(
    device: &ash::Device,
) -> Result<vk::DescriptorSetLayout, RenderError> {
    let binding = vk::DescriptorSetLayoutBinding::default()
        .binding(DESTINATION_DESCRIPTOR.binding)
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

// The destination-dependent program's pipeline layout binds the source's set
// first and the destination's second, so the two descriptors must name the
// first two sets.
const _: () = assert!(SOURCE_DESCRIPTOR.set == 0 && DESTINATION_DESCRIPTOR.set == 1);

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
///
/// The effect programs build their pipeline through this entry point; the
/// scene's programs name their program in the failure message.
pub(crate) fn build_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    vertex_module: vk::ShaderModule,
    fragment_module: vk::ShaderModule,
    slot: usize,
) -> Result<vk::Pipeline, RenderError> {
    build_program_pipeline(
        device,
        layout,
        render_pass,
        vertex_module,
        fragment_module,
        blend_state_for_slot(slot),
        format!("the effect program's pipeline for blend slot {slot}"),
    )
}

/// Builds one graphics pipeline for a program.
///
/// The fixed-function blend state is the program's own: a blend slot's state
/// for the textured and the solid program, and no blending for the
/// destination-dependent program, which writes the blended colour itself.
///
/// `description` names the program and the slot the failure message carries.
fn build_program_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    vertex_module: vk::ShaderModule,
    fragment_module: vk::ShaderModule,
    blend_attachment: vk::PipelineColorBlendAttachmentState,
    description: String,
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
        &description,
    )
}

/// The fixed-function state of a program that writes its colour itself.
///
/// The destination-dependent program mixes the colours in the shader, so the
/// fixed-function blending is off and every colour channel is written.
fn no_blending() -> vk::PipelineColorBlendAttachmentState {
    vk::PipelineColorBlendAttachmentState::default()
        .blend_enable(false)
        .color_write_mask(vk::ColorComponentFlags::RGBA)
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
///
/// `description` names the program and the blend slot the pipeline was for,
/// so a failure says which pipeline could not be built.
fn build_graphics_pipeline(
    device: &ash::Device,
    layout: vk::PipelineLayout,
    render_pass: vk::RenderPass,
    stages: &[vk::PipelineShaderStageCreateInfo],
    states: &PipelineStates<'_>,
    description: &str,
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
        message: format!("failed to create {description}: {error}"),
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

    /// The fixed-function composite of a copy that keeps the destination's
    /// alpha: the source's colour and the destination's alpha.
    fn copy_colour_keep_alpha(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
        [src[0], src[1], src[2], dst[3]]
    }

    /// The fixed-function composite of a source-over: the colour and the
    /// alpha are the source scaled by the source's alpha plus the
    /// destination scaled by its complement.
    fn source_over(src: [u8; 4], dst: [u8; 4]) -> [u8; 4] {
        let alpha = u32::from(src[3]);
        let inverse = 255 - alpha;
        let mut out = [0u8; 4];
        for (channel, value) in out.iter_mut().enumerate() {
            *value =
                ((u32::from(src[channel]) * alpha + u32::from(dst[channel]) * inverse) / 255) as u8;
        }
        out
    }

    /// The present copies the composed frame's colour and keeps the surface's
    /// alpha.
    ///
    /// The composed frame is the window's image: the reference's window
    /// surface is presented as the window's image and the window's own face
    /// is the opaque one, so the window shows the surface's colour whatever
    /// alpha the surface's pixels carry
    /// (`reference/krkr2/cpp/core/visual/BitmapLayerTreeOwner.cpp:73-107`:
    /// the primary layer's pixels are copied into the window's bitmap with a
    /// raw `memcpy`, with no blending).
    ///
    /// A source-over present multiplies the frame's colour by the frame's
    /// alpha. A layer whose blend holds the destination's alpha leaves the
    /// frame's alpha at zero, so the whole picture would be presented as the
    /// surface's cleared black while the frame still held the picture's
    /// colour. The vectors are that frame: the title screen's background
    /// layer's pixel `[242, 243, 224, 255]` in a frame whose alpha the blend
    /// held at zero, over the surface cleared to opaque black.
    #[test]
    fn the_present_copies_the_frame_colour_and_keeps_the_surface_alpha() {
        let state = blend_state_for_slot(KEEP_ALPHA_COPY_SLOT);
        assert_eq!(state.src_color_blend_factor, vk::BlendFactor::ONE);
        assert_eq!(state.dst_color_blend_factor, vk::BlendFactor::ZERO);
        assert_eq!(state.src_alpha_blend_factor, vk::BlendFactor::ZERO);
        assert_eq!(state.dst_alpha_blend_factor, vk::BlendFactor::ONE);

        // The frame holds the picture; its alpha is zero where the layer's
        // blend held the destination's alpha.
        let frame = [242u8, 243, 224, 0];
        // The surface was cleared to the scene's background: opaque black.
        let surface = [0u8, 0, 0, 255];
        // The present shows the picture and leaves the window opaque.
        assert_eq!(copy_colour_keep_alpha(frame, surface), [242, 243, 224, 255]);
        // The source-over present the slot replaced loses the picture: the
        // frame's colour is scaled by the frame's zero alpha and the
        // surface's black shows.
        assert_eq!(source_over(frame, surface), [0, 0, 0, 255]);
    }

    /// Every colour-only blend keeps the destination's alpha.
    ///
    /// A kernel whose masks exclude the alpha byte cannot change it, and the
    /// held face of such a kernel restores the destination's alpha outright:
    ///
    /// * the additive blend masks the source with `0x00ff00` and `0xff00ff`
    ///   (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:190-197`), so
    ///   the alpha byte is in neither mask;
    /// * the subtractive blend masks the same two lanes
    ///   (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:209-217`);
    /// * the multiplicative blend multiplies the blue, green and red lanes
    ///   only (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:222-228`),
    ///   and its held face adds the destination's alpha back
    ///   (`reference/krkr2/cpp/core/visual/tvpgl.cpp:7181-7196`);
    /// * the screen blend masks the same two lanes
    ///   (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:337-347`);
    /// * the additive-alpha blend masks the same two lanes
    ///   (`reference/krkr2/cpp/core/visual/gl/blend_util_func.h:35-43`), and
    ///   its held face restores the destination's alpha
    ///   (`reference/krkr2/cpp/core/visual/tvpgl.cpp:944-971`);
    /// * the Photoshop normal mixes the red/blue and the green lanes
    ///   (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-384`), so
    ///   the alpha byte is in neither mask.
    ///
    /// The fixed-function spelling of "keep the destination's alpha" is the
    /// source factor `ZERO` and the destination factor `ONE`.
    #[test]
    fn every_colour_only_blend_keeps_the_destination_alpha() {
        for (mode, slot) in [
            (BlendMode::Additive, 2usize),
            (BlendMode::Subtractive, 3),
            (BlendMode::AddAlpha, 4),
            (BlendMode::Multiply, 5),
            (BlendMode::Screen, 6),
            (BlendMode::PsNormal, 7),
        ] {
            assert_eq!(blend_slot(mode), Some(slot), "the mode {mode:?}");
            let state = blend_state_for_slot(slot);
            assert_eq!(
                state.src_alpha_blend_factor,
                vk::BlendFactor::ZERO,
                "the mode {mode:?} writes the source's alpha"
            );
            assert_eq!(
                state.dst_alpha_blend_factor,
                vk::BlendFactor::ONE,
                "the mode {mode:?} does not keep the destination's alpha"
            );
        }
    }

    /// The additive-alpha blend leaves an opaque destination alone.
    ///
    /// The blend adds the source's colour to the destination and never
    /// touches the destination's alpha
    /// (`reference/krkr2/cpp/core/visual/gl/blend_util_func.h:35-43`), so a
    /// fully transparent source leaves the destination's pixel as it was. The
    /// vector is the title screen's background layer's pixel
    /// `[242, 243, 224, 255]` with the transparent layer that covers it.
    #[test]
    fn the_additive_alpha_blend_leaves_the_destination_alpha_alone() {
        let state = blend_state_for_slot(4);
        assert_eq!(state.src_alpha_blend_factor, vk::BlendFactor::ZERO);
        assert_eq!(state.dst_alpha_blend_factor, vk::BlendFactor::ONE);

        // The source is fully transparent: the colour adds nothing and the
        // destination's alpha is kept.
        let source = [0u8, 0, 0, 0];
        let destination = [242u8, 243, 224, 255];
        // The colour factors are `SRC_ALPHA`/`ONE`: the source's colour is
        // scaled by the source's alpha and added to the destination's.
        let alpha = u32::from(source[3]);
        let mut colour = [0u8; 3];
        for (channel, value) in colour.iter_mut().enumerate() {
            *value =
                (u32::from(destination[channel]) + u32::from(source[channel]) * alpha / 255) as u8;
        }
        assert_eq!(
            [colour[0], colour[1], colour[2], destination[3]],
            destination
        );
    }

    /// The source-over is the one blend that writes the source's alpha.
    ///
    /// The engine's plain source-over writes the source's alpha
    /// (`crates/engine/src/layer_ops.rs:2969-2975`), and the reference's
    /// destination-alpha face writes the same value: `255 - (255-da)(255-sa)/255`
    /// (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:109-141` with
    /// `reference/krkr2/cpp/core/visual/tvpgl.cpp:582-620`), which is the
    /// source-over's `src·a + dst·(1 - a)`.
    #[test]
    fn the_source_over_writes_the_source_alpha() {
        let state = blend_state_for_slot(0);
        assert_eq!(state.src_alpha_blend_factor, vk::BlendFactor::ONE);
        assert_eq!(
            state.dst_alpha_blend_factor,
            vk::BlendFactor::ONE_MINUS_SRC_ALPHA
        );

        // The source-over's alpha: the source's alpha plus the destination's
        // alpha scaled by the source's complement.
        let source = [10u8, 20, 30, 128];
        let destination = [200u8, 100, 50, 255];
        let alpha = u32::from(source[3]);
        let over = alpha + u32::from(destination[3]) * (255 - alpha) / 255;
        assert_eq!(over, 255);
    }

    /// The plain copy takes the source's alpha.
    ///
    /// The copy replaces the destination's pixel with the source's
    /// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:158-160`), so
    /// the source's alpha is the result's alpha.
    #[test]
    fn the_plain_copy_takes_the_source_alpha() {
        let state = blend_state_for_slot(1);
        assert_eq!(state.src_alpha_blend_factor, vk::BlendFactor::ONE);
        assert_eq!(state.dst_alpha_blend_factor, vk::BlendFactor::ZERO);
    }

    /// A draw that holds the destination's alpha runs the blend's held face.
    ///
    /// The copy's held face keeps the destination's alpha
    /// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:162-167`), and
    /// the source-over's held face mixes the colours without writing an alpha
    /// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:67-75` with
    /// `reference/krkr2/cpp/core/visual/gl/blend_variation.h:63-73`).
    #[test]
    fn a_held_draw_runs_the_blends_held_face() {
        // The held copy: the source's colour and the destination's alpha.
        assert_eq!(
            blend_slot_for(BlendMode::Replace, true),
            Some(KEEP_ALPHA_COPY_SLOT)
        );
        // The held source-over: the Photoshop normal's colour mix with the
        // destination's alpha kept.
        assert_eq!(blend_slot_for(BlendMode::SourceOver, true), Some(7));
        // Every other blend keeps the destination's alpha on its own, so the
        // flag selects the same slot.
        for mode in [
            BlendMode::Additive,
            BlendMode::Subtractive,
            BlendMode::AddAlpha,
            BlendMode::Multiply,
            BlendMode::Screen,
            BlendMode::PsNormal,
        ] {
            assert_eq!(
                blend_slot_for(mode, true),
                blend_slot(mode),
                "the mode {mode:?}"
            );
        }
    }
}
