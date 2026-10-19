//! Offscreen render targets for the Vulkan backend.
//!
//! An offscreen target is an RGBA8 device image usable as a colour attachment,
//! a sampled texture and a transfer source. Rendering into it uses a dedicated
//! per-format render pass whose final layout is `TRANSFER_SRC_OPTIMAL`, so
//! [`read`] can copy pixels straight back to the host.
//!
//! As with textures, the GPU resource is created lazily on first use because
//! [`crate::create_render_target`] has no [`crate::RenderState`]; the resource
//! is keyed by the handle identity and lives in the caller-owned
//! [`super::VulkanState`].
#![allow(unsafe_code)]

use std::collections::HashMap;

use ash::vk;

use api::error::RenderError;
use api::geometry::{Affine2, PixelRegion};

use crate::render_target::RenderTargetDesc;
use crate::scene::RenderScene;
use crate::texture::TextureFormat;

use super::draw;
use super::pipelines::{self, PipelineSet};
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
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        format: TextureFormat,
    ) -> Result<Self, RenderError> {
        let render_pass = create_render_pass(device, image_format(format))?;
        let pipelines = match pipelines::create(device, memory_properties, render_pass) {
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
        let image_info = vk::ImageCreateInfo::default()
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D {
                width: desc.width,
                height: desc.height,
                depth: 1,
            })
            .mip_levels(1)
            .array_layers(1)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(
                vk::ImageUsageFlags::COLOR_ATTACHMENT
                    | vk::ImageUsageFlags::SAMPLED
                    | vk::ImageUsageFlags::TRANSFER_SRC,
            )
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);

        // SAFETY: `image_info` borrows a local valid for the call.
        let image = unsafe { device.create_image(&image_info, None) }.map_err(|error| {
            RenderError::OutOfMemory {
                message: format!("failed to create the render-target image: {error}"),
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
                message: format!("failed to allocate render-target memory: {error}"),
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
                message: format!("failed to bind render-target memory: {error}"),
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
                    message: format!("failed to create the render-target view: {error}"),
                });
            }
        };

        let attachments = [view];
        let framebuffer_info = vk::FramebufferCreateInfo::default()
            .render_pass(pass.render_pass)
            .attachments(&attachments)
            .width(desc.width)
            .height(desc.height)
            .layers(1);
        // SAFETY: the render pass and view are compatible and live.
        let framebuffer = match unsafe { device.create_framebuffer(&framebuffer_info, None) } {
            Ok(framebuffer) => framebuffer,
            Err(error) => {
                // SAFETY: both handles are owned here.
                unsafe {
                    device.destroy_image_view(view, None);
                    device.destroy_image(image, None);
                    device.free_memory(memory, None);
                }
                return Err(RenderError::Framebuffer {
                    message: format!("failed to create the render-target framebuffer: {error}"),
                });
            }
        };

        Ok(Self {
            desc,
            image,
            view,
            memory,
            framebuffer,
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
}

/// The command resources one synchronous offscreen operation records into and
/// submits on.
#[derive(Clone, Copy)]
pub(crate) struct OffscreenSubmit<'a> {
    /// Logical device that owns every handle below.
    pub(crate) device: &'a ash::Device,
    /// Queue the command buffer is submitted to.
    pub(crate) queue: vk::Queue,
    /// Pool the command buffer is allocated from and reset with.
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
    target: &VulkanRenderTarget,
    pass: &OffscreenPass,
    textures: &HashMap<u64, VulkanTexture>,
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
    let clear_values = [vk::ClearValue {
        color: vk::ClearColorValue { float32: clear },
    }];
    let render_area = vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent,
    };
    let render_pass_begin = vk::RenderPassBeginInfo::default()
        .render_pass(pass.render_pass)
        .framebuffer(target.framebuffer)
        .render_area(render_area)
        .clear_values(&clear_values);

    record_and_submit(submit, |command_buffer| {
        // SAFETY: the command buffer is recording and every handle is live.
        unsafe {
            device.cmd_begin_render_pass(
                command_buffer,
                &render_pass_begin,
                vk::SubpassContents::INLINE,
            );
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
        draw::record_scene(
            &draw::SceneDraw {
                device,
                command: command_buffer,
                pipelines: &pass.pipelines,
                textures,
                extent,
                srgb,
            },
            scene,
            &Affine2::IDENTITY,
        )?;
        // SAFETY: the render pass and command buffer are recording.
        unsafe { device.cmd_end_render_pass(command_buffer) };
        Ok(())
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
/// The final layout is `TRANSFER_SRC_OPTIMAL` so the pixels can be copied out
/// without an extra layout transition.
fn create_render_pass(
    device: &ash::Device,
    format: vk::Format,
) -> Result<vk::RenderPass, RenderError> {
    let attachment = vk::AttachmentDescription::default()
        .format(format)
        .samples(vk::SampleCountFlags::TYPE_1)
        .load_op(vk::AttachmentLoadOp::CLEAR)
        .store_op(vk::AttachmentStoreOp::STORE)
        .stencil_load_op(vk::AttachmentLoadOp::DONT_CARE)
        .stencil_store_op(vk::AttachmentStoreOp::DONT_CARE)
        .initial_layout(vk::ImageLayout::UNDEFINED)
        .final_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL);
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
            .dst_stage_mask(vk::PipelineStageFlags::TRANSFER)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ),
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

/// Emits a barrier making the image readable as a transfer source.
fn barrier_to_transfer_src(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    target: &VulkanRenderTarget,
) {
    let subresource_range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1);
    let barrier = vk::ImageMemoryBarrier::default()
        .old_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(target.image)
        .subresource_range(subresource_range)
        .src_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE)
        .dst_access_mask(vk::AccessFlags::TRANSFER_READ);
    // SAFETY: `barrier` refers to a live image owned by `target`.
    unsafe {
        device.cmd_pipeline_barrier(
            command_buffer,
            vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );
    }
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
