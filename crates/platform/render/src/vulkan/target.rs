//! Offscreen render targets for the Vulkan backend.
//!
//! An offscreen target is an RGBA8 device image usable as a colour attachment,
//! a sampled texture and a transfer source. Rendering into it uses a dedicated
//! per-format render pass that loads the attachment, so a render can follow
//! another one over the same target; the target is cleared explicitly instead.
//!
//! The live frame composes into a target of its own (the [`FrameTarget`]) and
//! presents it with a quad, because a destination-dependent blend needs a copy
//! of the destination and a colour attachment cannot be a copy's source: the
//! copy happens between two render passes over the same target.
//!
//! As with textures, the GPU resource is created lazily on first use because
//! [`crate::create_render_target`] has no [`crate::RenderState`]; the resource
//! is keyed by the handle identity and lives in the caller-owned
//! [`super::VulkanState`].
#![allow(unsafe_code)]

use ash::vk;

use api::error::RenderError;
use api::geometry::{Affine2, PixelRegion};

use crate::collections::Map;
use crate::render_target::RenderTargetDesc;
use crate::scene::RenderScene;
use crate::texture::TextureFormat;

use super::draw;
use super::pipelines::{self, PipelineResources, PipelineSet};
use super::shaders::{DESTINATION_DESCRIPTOR, SOURCE_DESCRIPTOR};
use super::texture::{StagingBuffer, VulkanTexture, find_memory_type, image_format};

/// Render pass plus pipelines compiled against it, one per target format.
pub(crate) struct OffscreenPass {
    /// Colour-only render pass for this format.
    pub(crate) render_pass: vk::RenderPass,
    /// Pipelines compiled for [`Self::render_pass`].
    pub(crate) pipelines: PipelineSet,
}

impl OffscreenPass {
    /// Builds the render pass and its pipelines for `format`.
    pub(crate) fn create(
        device: &ash::Device,
        format: TextureFormat,
        resources: &PipelineResources,
    ) -> Result<Self, RenderError> {
        let render_pass = create_render_pass(device, image_format(format))?;
        let pipelines = match pipelines::create(device, render_pass, resources) {
            Ok(pipelines) => pipelines,
            Err(error) => {
                // SAFETY: the render pass was created here and nothing uses it.
                unsafe { device.destroy_render_pass(render_pass, None) };
                return Err(error);
            }
        };
        Ok(Self {
            render_pass,
            pipelines,
        })
    }

    /// Destroys the pipelines and render pass.
    pub(crate) fn destroy(&self, device: &ash::Device) {
        self.pipelines.destroy(device);
        // SAFETY: the render pass was created by `device`; the caller
        // guarantees the GPU is idle and no framebuffer references it.
        unsafe { device.destroy_render_pass(self.render_pass, None) };
    }
}

/// One offscreen target's Vulkan resources.
pub(crate) struct VulkanRenderTarget {
    /// Descriptor the target was created with.
    pub(crate) desc: RenderTargetDesc,
    /// Colour image.
    image: vk::Image,
    /// Colour image view.
    view: vk::ImageView,
    /// Device memory backing [`Self::image`].
    memory: vk::DeviceMemory,
    /// Framebuffer over [`Self::view`].
    framebuffer: vk::Framebuffer,
    /// The layout the image is in.
    layout: vk::ImageLayout,
}

impl VulkanRenderTarget {
    /// Allocates the image, view and framebuffer for `desc`.
    pub(crate) fn create(
        device: &ash::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        pass: &OffscreenPass,
        desc: RenderTargetDesc,
    ) -> Result<Self, RenderError> {
        let format = image_format(desc.format);
        let (image, view, memory, framebuffer) = create_target_image(
            device,
            memory_properties,
            format,
            desc.width,
            desc.height,
            pass.render_pass,
            vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;

        Ok(Self {
            desc,
            image,
            view,
            memory,
            framebuffer,
            layout: vk::ImageLayout::UNDEFINED,
        })
    }

    /// Destroys the framebuffer, view, image and memory.
    pub(crate) fn destroy(&self, device: &ash::Device) {
        // SAFETY: all handles were created by `device`; the caller guarantees
        // the GPU is idle.
        unsafe {
            device.destroy_framebuffer(self.framebuffer, None);
            device.destroy_image_view(self.view, None);
            device.destroy_image(self.image, None);
            device.free_memory(self.memory, None);
        }
    }

    /// Moves the image into the layout the render pass expects.
    ///
    /// The render pass loads the attachment, so the image is transitioned to
    /// the colour-attachment layout before the pass begins.
    pub(crate) fn prepare(&mut self, device: &ash::Device, command: vk::CommandBuffer) {
        if self.layout == vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL {
            return;
        }
        barrier_color(
            device,
            command,
            self.image,
            &Barrier {
                old_layout: self.layout,
                new_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                src_access: vk::AccessFlags::empty(),
                dst_access: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                src_stage: vk::PipelineStageFlags::TOP_OF_PIPE,
                dst_stage: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            },
        );
        self.layout = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;
    }
}

/// A copy of the destination a destination-dependent blend reads.
///
/// The blend program samples the copy as its second texture. The copy is a
/// device image of the target's size; the region the blend reads is copied
/// into it at the region's own offsets, so the program reads the destination
/// at the fragment's own position.
pub(crate) struct DestinationSnapshot {
    /// The colour image the destination region is copied into.
    image: vk::Image,
    /// The sampled image view of the colour image.
    view: vk::ImageView,
    /// The device memory backing the colour image.
    memory: vk::DeviceMemory,
    /// The descriptor set the blend program binds the copy through.
    pub(crate) descriptor_set: vk::DescriptorSet,
    /// The layout the colour image is in.
    layout: vk::ImageLayout,
}

impl DestinationSnapshot {
    /// Allocates the image, view and descriptor set of one copy.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::OutOfMemory`] when a resource cannot be created.
    pub(crate) fn create(
        device: &ash::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        resources: &PipelineResources,
        format: vk::Format,
        width: u32,
        height: u32,
    ) -> Result<Self, RenderError> {
        let (image, view, memory, _) = create_target_image(
            device,
            memory_properties,
            format,
            width,
            height,
            vk::RenderPass::null(),
            vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED,
        )?;
        let descriptor_set = match allocate_sampled_set(
            device,
            resources.dst_descriptor_set_layout,
            resources.descriptor_pool,
            resources.sampler,
            view,
            DESTINATION_DESCRIPTOR.binding,
        ) {
            Ok(set) => set,
            Err(error) => {
                // SAFETY: the image, view and memory were created here and
                // nothing else uses them.
                unsafe {
                    device.destroy_image_view(view, None);
                    device.destroy_image(image, None);
                    device.free_memory(memory, None);
                }
                return Err(error);
            }
        };
        Ok(Self {
            image,
            view,
            memory,
            descriptor_set,
            layout: vk::ImageLayout::UNDEFINED,
        })
    }

    /// Destroys the descriptor set, view, image and memory.
    pub(crate) fn destroy(&self, device: &ash::Device, descriptor_pool: vk::DescriptorPool) {
        // SAFETY: all handles were created by `device`; the caller guarantees
        // the GPU is idle.
        unsafe {
            let _ = device.free_descriptor_sets(descriptor_pool, &[self.descriptor_set]);
            device.destroy_image_view(self.view, None);
            device.destroy_image(self.image, None);
            device.free_memory(self.memory, None);
        }
    }

    /// Copies `region` of `source` into the copy.
    ///
    /// The source is the pass's colour attachment, so it leaves the
    /// attachment layout for the copy and comes back afterwards.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Framebuffer`] when the copy cannot be recorded.
    pub(crate) fn copy_region(
        &mut self,
        device: &ash::Device,
        command: vk::CommandBuffer,
        source: vk::Image,
        region: vk::Rect2D,
    ) -> Result<(), RenderError> {
        barrier_color(
            device,
            command,
            source,
            &Barrier {
                old_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                new_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                src_access: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                dst_access: vk::AccessFlags::TRANSFER_READ,
                src_stage: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                dst_stage: vk::PipelineStageFlags::TRANSFER,
            },
        );
        barrier_color(
            device,
            command,
            self.image,
            &Barrier {
                old_layout: self.layout,
                new_layout: vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                src_access: match self.layout {
                    vk::ImageLayout::UNDEFINED => vk::AccessFlags::empty(),
                    _ => vk::AccessFlags::SHADER_READ,
                },
                dst_access: vk::AccessFlags::TRANSFER_WRITE,
                src_stage: match self.layout {
                    vk::ImageLayout::UNDEFINED => vk::PipelineStageFlags::TOP_OF_PIPE,
                    _ => vk::PipelineStageFlags::FRAGMENT_SHADER,
                },
                dst_stage: vk::PipelineStageFlags::TRANSFER,
            },
        );
        self.layout = vk::ImageLayout::TRANSFER_DST_OPTIMAL;

        let extent = vk::Extent3D {
            width: region.extent.width,
            height: region.extent.height,
            depth: 1,
        };
        let offset = vk::Offset3D {
            x: region.offset.x,
            y: region.offset.y,
            z: 0,
        };
        let subresource = vk::ImageSubresourceLayers::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .mip_level(0)
            .base_array_layer(0)
            .layer_count(1);
        let copy = vk::ImageCopy::default()
            .src_subresource(subresource)
            .src_offset(offset)
            .dst_subresource(subresource)
            .dst_offset(offset)
            .extent(extent);
        // SAFETY: the command buffer is recording and both images are live.
        unsafe {
            device.cmd_copy_image(
                command,
                source,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                self.image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[copy],
            );
        }

        barrier_color(
            device,
            command,
            self.image,
            &Barrier {
                old_layout: vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                new_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                src_access: vk::AccessFlags::TRANSFER_WRITE,
                dst_access: vk::AccessFlags::SHADER_READ,
                src_stage: vk::PipelineStageFlags::TRANSFER,
                dst_stage: vk::PipelineStageFlags::FRAGMENT_SHADER,
            },
        );
        self.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;

        barrier_color(
            device,
            command,
            source,
            &Barrier {
                old_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                new_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                src_access: vk::AccessFlags::TRANSFER_READ,
                dst_access: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                src_stage: vk::PipelineStageFlags::TRANSFER,
                dst_stage: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            },
        );
        Ok(())
    }
}

/// The render pass and the pipelines the live frame is composed into.
///
/// The pass loads the attachment, so a render can follow another one over the
/// same target; the target is cleared with a full-screen draw instead. Every
/// frame slot has a target of its own, and they share this pass and these
/// pipelines.
pub(crate) struct FramePass {
    /// The render pass the scene is composed into.
    pub(crate) render_pass: vk::RenderPass,
    /// The pipelines built against the render pass.
    pub(crate) pipelines: PipelineSet,
}

impl FramePass {
    /// Builds the render pass and the pipelines for `format`.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::OutOfMemory`] when a resource cannot be created.
    pub(crate) fn create(
        device: &ash::Device,
        format: vk::Format,
        resources: &PipelineResources,
    ) -> Result<Self, RenderError> {
        let render_pass = create_render_pass(device, format)?;
        let pipelines = match pipelines::create(device, render_pass, resources) {
            Ok(pipelines) => pipelines,
            Err(error) => {
                // SAFETY: the render pass was created here and nothing uses it.
                unsafe { device.destroy_render_pass(render_pass, None) };
                return Err(error);
            }
        };
        Ok(Self {
            render_pass,
            pipelines,
        })
    }

    /// Destroys the pipelines and the render pass.
    pub(crate) fn destroy(&self, device: &ash::Device) {
        self.pipelines.destroy(device);
        // SAFETY: the render pass was created by `device`; the caller
        // guarantees the GPU is idle and no framebuffer references it.
        unsafe {
            device.destroy_render_pass(self.render_pass, None);
        }
    }
}

/// One frame slot's target: the scene is composed into it and presented with a
/// quad.
///
/// Every slot has a target of its own, because two frames can be in flight
/// and a frame must not write the target the other one is presenting.
pub(crate) struct FrameTarget {
    /// The colour image the scene is composed into.
    image: vk::Image,
    /// The image view of the colour image.
    view: vk::ImageView,
    /// The device memory backing the colour image.
    memory: vk::DeviceMemory,
    /// The framebuffer over the image view.
    framebuffer: vk::Framebuffer,
    /// The descriptor set the present quad binds the target through.
    pub(crate) descriptor_set: vk::DescriptorSet,
    /// The extent of the target in device pixels.
    pub(crate) extent: vk::Extent2D,
    /// The layout the colour image is in.
    pub(crate) layout: vk::ImageLayout,
    /// The copy of the destination the destination-dependent blend reads.
    pub(crate) snapshot: DestinationSnapshot,
}

impl FrameTarget {
    /// Builds the image, the descriptor set and the copies of the destination.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::OutOfMemory`] when a resource cannot be created.
    pub(crate) fn create(
        device: &ash::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        format: vk::Format,
        extent: vk::Extent2D,
        resources: &PipelineResources,
        pass: &FramePass,
    ) -> Result<Self, RenderError> {
        let (image, view, memory, framebuffer) = create_target_image(
            device,
            memory_properties,
            format,
            extent.width,
            extent.height,
            pass.render_pass,
            vk::ImageUsageFlags::COLOR_ATTACHMENT
                | vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC,
        )?;
        let descriptor_set = match allocate_sampled_set(
            device,
            resources.descriptor_set_layout,
            resources.descriptor_pool,
            resources.sampler,
            view,
            SOURCE_DESCRIPTOR.binding,
        ) {
            Ok(set) => set,
            Err(error) => {
                // SAFETY: every handle was created here and nothing else uses
                // them.
                unsafe {
                    device.destroy_framebuffer(framebuffer, None);
                    device.destroy_image_view(view, None);
                    device.destroy_image(image, None);
                    device.free_memory(memory, None);
                }
                return Err(error);
            }
        };
        let snapshot = match DestinationSnapshot::create(
            device,
            memory_properties,
            resources,
            format,
            extent.width,
            extent.height,
        ) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                // SAFETY: every handle was created here and nothing else uses
                // them.
                unsafe {
                    let _ =
                        device.free_descriptor_sets(resources.descriptor_pool, &[descriptor_set]);
                    device.destroy_framebuffer(framebuffer, None);
                    device.destroy_image_view(view, None);
                    device.destroy_image(image, None);
                    device.free_memory(memory, None);
                }
                return Err(error);
            }
        };
        Ok(Self {
            image,
            view,
            memory,
            framebuffer,
            descriptor_set,
            extent,
            layout: vk::ImageLayout::UNDEFINED,
            snapshot,
        })
    }

    /// Destroys the copies of the destination and the target.
    pub(crate) fn destroy(&self, device: &ash::Device, resources: &PipelineResources) {
        // SAFETY: every handle was created by `device`; the caller guarantees
        // the GPU is idle.
        unsafe {
            self.snapshot.destroy(device, resources.descriptor_pool);
            let _ = device.free_descriptor_sets(resources.descriptor_pool, &[self.descriptor_set]);
            device.destroy_framebuffer(self.framebuffer, None);
            device.destroy_image_view(self.view, None);
            device.destroy_image(self.image, None);
            device.free_memory(self.memory, None);
        }
    }

    /// The colour image the scene is composed into.
    pub(crate) fn image(&self) -> vk::Image {
        self.image
    }

    /// The framebuffer the scene is composed into.
    pub(crate) fn framebuffer(&self) -> vk::Framebuffer {
        self.framebuffer
    }

    /// Moves the image into the layout the render pass expects.
    ///
    /// The render pass loads the attachment, so the image is transitioned to
    /// the colour-attachment layout before the pass begins.
    pub(crate) fn prepare(&mut self, device: &ash::Device, command: vk::CommandBuffer) {
        if self.layout == vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL {
            return;
        }
        let reading = self.layout == vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
        barrier_color(
            device,
            command,
            self.image,
            &Barrier {
                old_layout: self.layout,
                new_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                src_access: if reading {
                    vk::AccessFlags::SHADER_READ
                } else {
                    vk::AccessFlags::empty()
                },
                dst_access: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                src_stage: if reading {
                    vk::PipelineStageFlags::FRAGMENT_SHADER
                } else {
                    vk::PipelineStageFlags::TOP_OF_PIPE
                },
                dst_stage: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            },
        );
        self.layout = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;
    }

    /// Moves the image into the layout the present quad samples it in.
    pub(crate) fn prepare_for_present(&mut self, device: &ash::Device, command: vk::CommandBuffer) {
        if self.layout == vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL {
            return;
        }
        barrier_color(
            device,
            command,
            self.image,
            &Barrier {
                old_layout: self.layout,
                new_layout: vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                src_access: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                dst_access: vk::AccessFlags::SHADER_READ,
                src_stage: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
                dst_stage: vk::PipelineStageFlags::FRAGMENT_SHADER,
            },
        );
        self.layout = vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL;
    }
}

/// The command resources one synchronous offscreen operation records into and
/// submits on.
#[derive(Clone, Copy)]
pub(crate) struct OffscreenSubmit<'a> {
    /// Logical device that owns every handle below.
    pub(crate) device: &'a ash::Device,
    /// Queue the command buffer is submitted to.
    pub(crate) queue: vk::Queue,
    /// Command pool the command buffer is allocated from and reset with.
    pub(crate) command_pool: vk::CommandPool,
    /// Command buffer the operation is recorded into.
    pub(crate) command_buffer: vk::CommandBuffer,
    /// Fence waited on after the submit.
    pub(crate) fence: vk::Fence,
}

/// Records `scene` into `target` and submits it synchronously.
///
/// # Errors
///
/// Propagates [`RenderError::Unsupported`] from the scene (mask clips, GPU
/// glyph runs, destination-dependent blends) and any command/submit failure.
pub(crate) fn render(
    submit: &OffscreenSubmit<'_>,
    target: &mut VulkanRenderTarget,
    pass: &OffscreenPass,
    resources: &PipelineResources,
    textures: &Map<u64, VulkanTexture>,
    effects: Option<&mut super::effect::EffectState>,
    scene: &RenderScene,
) -> Result<(), RenderError> {
    let OffscreenSubmit { device, .. } = *submit;
    let extent = vk::Extent2D {
        width: target.desc.width,
        height: target.desc.height,
    };
    let format = image_format(target.desc.format);
    let srgb = super::format_is_srgb(format);
    let clear = super::clear_value(scene.background, srgb);

    record_and_submit(submit, |command_buffer| {
        // SAFETY: the command buffer is recording and every handle is live.
        unsafe {
            let viewport = vk::Viewport {
                x: 0.0,
                y: 0.0,
                width: extent.width as f32,
                height: extent.height as f32,
                min_depth: 0.0,
                max_depth: 1.0,
            };
            device.cmd_set_viewport(command_buffer, 0, &[viewport]);
        }
        target.prepare(device, command_buffer);
        draw::record_scene(
            draw::SceneDraw {
                pass: draw::ScenePass {
                    device,
                    command: command_buffer,
                    render_pass: pass.render_pass,
                    framebuffer: target.framebuffer,
                    image: target.image,
                    clear,
                    extent,
                },
                pipelines: &pass.pipelines,
                resources,
                textures,
                effects,
                snapshot: None,
                extent,
                srgb,
            },
            scene,
            &Affine2::IDENTITY,
        )
    })
}

/// Copies `region` of `target` into `out` and waits for completion.
///
/// `readback` is reused across calls: it is grown on demand and only the
/// copy region is mapped, so a readback no longer allocates and destroys a
/// buffer. The copy is synchronous because the caller observes `out` on
/// return.
///
/// # Errors
///
/// Returns [`RenderError::Framebuffer`] on allocation/copy/map failure.
pub(crate) fn read(
    submit: &OffscreenSubmit<'_>,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    target: &VulkanRenderTarget,
    readback: &mut StagingBuffer,
    region: PixelRegion,
    out: &mut [u8],
) -> Result<(), RenderError> {
    let OffscreenSubmit { device, .. } = *submit;
    let bytes_per_pixel = target.desc.format.bytes_per_pixel() as vk::DeviceSize;
    let size = (region.area() * bytes_per_pixel as u64).max(1);

    readback.reserve(
        device,
        memory_properties,
        size,
        vk::BufferUsageFlags::TRANSFER_DST,
    )?;
    let buffer = readback.buffer;
    let memory = readback.memory;

    record_and_submit(submit, |command_buffer| {
        barrier_to_transfer_src(device, command_buffer, target);
        let subresource = vk::ImageSubresourceLayers::default()
            .aspect_mask(vk::ImageAspectFlags::COLOR)
            .mip_level(0)
            .base_array_layer(0)
            .layer_count(1);
        let copy = vk::BufferImageCopy::default()
            .buffer_offset(0)
            .buffer_row_length(0)
            .buffer_image_height(0)
            .image_subresource(subresource)
            .image_offset(vk::Offset3D {
                x: region.x as i32,
                y: region.y as i32,
                z: 0,
            })
            .image_extent(vk::Extent3D {
                width: region.width,
                height: region.height,
                depth: 1,
            });
        // SAFETY: the command buffer is recording and both handles are
        // live.
        unsafe {
            device.cmd_copy_image_to_buffer(
                command_buffer,
                target.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                buffer,
                &[copy],
            );
        }
        barrier_color(
            device,
            command_buffer,
            target.image,
            &Barrier {
                old_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                new_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                src_access: vk::AccessFlags::TRANSFER_READ,
                dst_access: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
                src_stage: vk::PipelineStageFlags::TRANSFER,
                dst_stage: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            },
        );
        Ok(())
    })?;

    // SAFETY: the fence has been waited on, so the mapped memory is no
    // longer written by the GPU and `size` bytes are valid.
    let mapped = unsafe { device.map_memory(memory, 0, size, vk::MemoryMapFlags::empty()) }
        .map_err(|error| RenderError::Framebuffer {
            message: format!("failed to map the readback buffer: {error}"),
        })?;
    let expected = region.area() as usize * target.desc.format.bytes_per_pixel() as usize;
    if out.len() != expected {
        // SAFETY: `mapped` was mapped above with `size`.
        unsafe { device.unmap_memory(memory) };
        return Err(RenderError::Framebuffer {
            message: format!(
                "readback buffer is {} bytes, expected {expected}",
                out.len()
            ),
        });
    }
    // SAFETY: `mapped` points at `size` readable bytes and `out` is
    // `expected <= size` bytes long.
    unsafe {
        std::ptr::copy_nonoverlapping(mapped.cast::<u8>(), out.as_mut_ptr(), out.len());
        device.unmap_memory(memory);
    }
    Ok(())
}

/// Creates the colour-only render pass used for offscreen targets.
///
/// The pass loads the attachment, so a render can follow another one over the
/// same target; the target is cleared explicitly instead.
fn create_render_pass(
    device: &ash::Device,
    format: vk::Format,
) -> Result<vk::RenderPass, RenderError> {
    let attachment = vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::LOAD)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
        .final_layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
    let attachments = [attachment];

    let color_reference = vk::AttachmentReference::default()
        .attachment(0)
        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
    let color_references = [color_reference];
    let subpass = vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&color_references);
    let subpasses = [subpass];

    let dependencies = [
        vk::SubpassDependency::default()
            .src_subpass(vk::SUBPASS_EXTERNAL)
            .dst_subpass(0)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags::empty())
            .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE),
        vk::SubpassDependency::default()
            .src_subpass(0)
            .dst_subpass(vk::SUBPASS_EXTERNAL)
            .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
            .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
            .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE),
    ];

    let create_info = vk::RenderPassCreateInfo::default()
        .attachments(&attachments)
        .subpasses(&subpasses)
        .dependencies(&dependencies);

    // SAFETY: `create_info` borrows locals valid for the call.
    unsafe { device.create_render_pass(&create_info, None) }.map_err(|error| {
        RenderError::Framebuffer {
            message: format!("failed to create the offscreen render pass: {error}"),
        }
    })
}

/// Creates a colour image, its view, its memory and a framebuffer over it.
///
/// `render_pass` may be null when the image is not a colour attachment.
fn create_target_image(
    device: &ash::Device,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    format: vk::Format,
    width: u32,
    height: u32,
    render_pass: vk::RenderPass,
    usage: vk::ImageUsageFlags,
) -> Result<(vk::Image, vk::ImageView, vk::DeviceMemory, vk::Framebuffer), RenderError> {
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width,
            height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(usage)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);

    // SAFETY: `image_info` borrows a local valid for the call.
    let image = unsafe { device.create_image(&image_info, None) }.map_err(|error| {
        RenderError::OutOfMemory {
            message: format!("failed to create the target image: {error}"),
        }
    })?;

    // SAFETY: `image` was just created by `device`.
    let requirements = unsafe { device.get_image_memory_requirements(image) };
    let memory_type = match find_memory_type(
        memory_properties,
        requirements.memory_type_bits,
        vk::MemoryPropertyFlags::DEVICE_LOCAL,
    ) {
        Ok(memory_type) => memory_type,
        Err(error) => {
            // SAFETY: `image` is owned here.
            unsafe { device.destroy_image(image, None) };
            return Err(error);
        }
    };
    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type);
    // SAFETY: `allocate_info` is valid; the memory is freed with the image.
    let memory = unsafe { device.allocate_memory(&allocate_info, None) }.map_err(|error| {
        // SAFETY: `image` is owned here.
        unsafe { device.destroy_image(image, None) };
        RenderError::OutOfMemory {
            message: format!("failed to allocate target memory: {error}"),
        }
    })?;

    // SAFETY: `memory` was allocated for `image`.
    if let Err(error) = unsafe { device.bind_image_memory(image, memory, 0) } {
        // SAFETY: both handles are owned here.
        unsafe {
            device.destroy_image(image, None);
            device.free_memory(memory, None);
        }
        return Err(RenderError::OutOfMemory {
            message: format!("failed to bind target memory: {error}"),
        });
    }

    let subresource_range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1);
    let view_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .components(vk::ComponentMapping::default())
        .subresource_range(subresource_range);
    // SAFETY: `image` was created with a compatible format.
    let view = match unsafe { device.create_image_view(&view_info, None) } {
        Ok(view) => view,
        Err(error) => {
            // SAFETY: both handles are owned here.
            unsafe {
                device.destroy_image(image, None);
                device.free_memory(memory, None);
            }
            return Err(RenderError::OutOfMemory {
                message: format!("failed to create the target image view: {error}"),
            });
        }
    };

    let framebuffer = if render_pass == vk::RenderPass::null() {
        vk::Framebuffer::null()
    } else {
        let attachments = [view];
        let framebuffer_info = vk::FramebufferCreateInfo::default()
            .render_pass(render_pass)
            .attachments(&attachments)
            .width(width)
            .height(height)
            .layers(1);
        // SAFETY: the render pass and view are compatible and live.
        match unsafe { device.create_framebuffer(&framebuffer_info, None) } {
            Ok(framebuffer) => framebuffer,
            Err(error) => {
                // SAFETY: both handles are owned here.
                unsafe {
                    device.destroy_image_view(view, None);
                    device.destroy_image(image, None);
                    device.free_memory(memory, None);
                }
                return Err(RenderError::Framebuffer {
                    message: format!("failed to create the target framebuffer: {error}"),
                });
            }
        }
    };

    Ok((image, view, memory, framebuffer))
}

/// Allocates a descriptor set that samples `view`.
///
/// # Errors
///
/// Returns [`RenderError::OutOfMemory`] when the set cannot be allocated.
fn allocate_sampled_set(
    device: &ash::Device,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    sampler: vk::Sampler,
    view: vk::ImageView,
    binding: u32,
) -> Result<vk::DescriptorSet, RenderError> {
    let set_layouts = [descriptor_set_layout];
    let allocate_sets = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(descriptor_pool)
        .set_layouts(&set_layouts);
    // SAFETY: `allocate_sets` borrows locals valid for the call.
    let descriptor_set = match unsafe { device.allocate_descriptor_sets(&allocate_sets) } {
        Ok(mut sets) => match sets.pop() {
            Some(set) => set,
            None => {
                return Err(RenderError::OutOfMemory {
                    message: "the descriptor pool returned no descriptor set".to_owned(),
                });
            }
        },
        Err(error) => {
            return Err(RenderError::OutOfMemory {
                message: format!("failed to allocate a descriptor set: {error}"),
            });
        }
    };
    let image_info = vk::DescriptorImageInfo::default()
        .image_layout(vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL)
        .image_view(view)
        .sampler(sampler);
    let image_infos = [image_info];
    let write = vk::WriteDescriptorSet::default()
        .dst_set(descriptor_set)
        .dst_binding(binding)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .image_info(&image_infos);
    // SAFETY: the descriptor set is not in use and the image info is valid.
    unsafe { device.update_descriptor_sets(&[write], &[]) };
    Ok(descriptor_set)
}

/// One image-memory barrier's layout, access and stage span.
struct Barrier {
    /// Layout the image is in before the barrier.
    old_layout: vk::ImageLayout,
    /// Layout the image is in after the barrier.
    new_layout: vk::ImageLayout,
    /// Accesses that must complete before the barrier.
    src_access: vk::AccessFlags,
    /// Accesses the barrier makes visible afterwards.
    dst_access: vk::AccessFlags,
    /// Pipeline stage the barrier waits on.
    src_stage: vk::PipelineStageFlags,
    /// Pipeline stage the barrier releases to.
    dst_stage: vk::PipelineStageFlags,
}

/// Emits an image-memory barrier for one colour subresource.
fn barrier_color(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    image: vk::Image,
    barrier: &Barrier,
) {
    let subresource_range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1);
    let memory_barrier = vk::ImageMemoryBarrier::default()
        .old_layout(barrier.old_layout)
        .new_layout(barrier.new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(image)
        .subresource_range(subresource_range)
        .src_access_mask(barrier.src_access)
        .dst_access_mask(barrier.dst_access);
    // SAFETY: `memory_barrier` refers to a live image.
    unsafe {
        device.cmd_pipeline_barrier(
            command_buffer,
            barrier.src_stage,
            barrier.dst_stage,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[memory_barrier],
        );
    }
}

/// Emits the barriers that make the image readable as a transfer source.
///
/// The image is a colour attachment the render pass wrote, so it leaves the
/// attachment layout for the copy and comes back afterwards.
fn barrier_to_transfer_src(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    target: &VulkanRenderTarget,
) {
    barrier_color(
        device,
        command_buffer,
        target.image,
        &Barrier {
            old_layout: vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
            new_layout: vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
            src_access: vk::AccessFlags::COLOR_ATTACHMENT_WRITE,
            dst_access: vk::AccessFlags::TRANSFER_READ,
            src_stage: vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            dst_stage: vk::PipelineStageFlags::TRANSFER,
        },
    );
}

/// Records a fallible `record` closure and submits it synchronously.
fn record_and_submit<F>(submit: &OffscreenSubmit<'_>, record: F) -> Result<(), RenderError>
where
    F: FnOnce(vk::CommandBuffer) -> Result<(), RenderError>,
{
    let OffscreenSubmit {
        device,
        queue,
        command_pool,
        command_buffer,
        fence,
    } = *submit;
    // SAFETY: the caller guarantees no command buffers from this pool are in
    // flight; the fence was created signalled so the first wait returns.
    unsafe {
        device
            .wait_for_fences(&[fence], true, u64::MAX)
            .map_err(|error| RenderError::Framebuffer {
                message: format!("failed to wait for the offscreen fence: {error}"),
            })?;
        device
            .reset_command_pool(command_pool, vk::CommandPoolResetFlags::empty())
            .map_err(|error| RenderError::Framebuffer {
                message: format!("failed to reset the offscreen command pool: {error}"),
            })?;
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        device
            .begin_command_buffer(command_buffer, &begin_info)
            .map_err(|error| RenderError::Framebuffer {
                message: format!("failed to begin the offscreen command buffer: {error}"),
            })?;
    }

    record(command_buffer)?;

    // SAFETY: the command buffer is recording and was begun above.
    unsafe {
        device
            .end_command_buffer(command_buffer)
            .map_err(|error| RenderError::Framebuffer {
                message: format!("failed to end the offscreen command buffer: {error}"),
            })?;
        device
            .reset_fences(&[fence])
            .map_err(|error| RenderError::Framebuffer {
                message: format!("failed to reset the offscreen fence: {error}"),
            })?;
        let command_buffers = [command_buffer];
        let submit = vk::SubmitInfo::default().command_buffers(&command_buffers);
        device
            .queue_submit(queue, &[submit], fence)
            .map_err(|error| RenderError::Framebuffer {
                message: format!("failed to submit the offscreen work: {error}"),
            })?;
        device
            .wait_for_fences(&[fence], true, u64::MAX)
            .map_err(|error| RenderError::Framebuffer {
                message: format!("failed to wait for the offscreen work: {error}"),
            })?;
    }
    Ok(())
}
