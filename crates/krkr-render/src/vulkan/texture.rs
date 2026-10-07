//! Texture image creation and sub-rectangle staging uploads.
//!
//! Because [`crate::create_texture`] has no access to a [`RenderState`], the
//! GPU image is allocated lazily on the first upload and cached in the state's
//! texture table. Every upload leaves the image in
//! `SHADER_READ_ONLY_OPTIMAL`, which is the layout the pipelines sample.

use ash::vk;

use krkr_api::error::RenderError;
use krkr_api::geometry::PixelRegion;

use crate::texture::{TextureDesc, TextureFormat};

/// GPU image, view and descriptor set for one texture handle.
pub(crate) struct VulkanTexture {
    /// Descriptor the texture was created with.
    pub(crate) desc: TextureDesc,
    /// Backing device image.
    pub(crate) image: vk::Image,
    /// Sampled image view.
    pub(crate) view: vk::ImageView,
    /// Device memory backing [`Self::image`].
    pub(crate) memory: vk::DeviceMemory,
    /// Descriptor set bound for textured draws.
    pub(crate) descriptor_set: vk::DescriptorSet,
    /// Whether the image has received data and is sampleable.
    pub(crate) uploaded: bool,
}

impl VulkanTexture {
    /// Destroys the view, image, memory and descriptor set.
    pub(crate) fn destroy(&self, device: &ash::Device, descriptor_pool: vk::DescriptorPool) {
        // SAFETY: all handles were created by `device`; the caller guarantees
        // the GPU is idle.
        unsafe {
            device.destroy_image_view(self.view, None);
            device.destroy_image(self.image, None);
            device.free_memory(self.memory, None);
            let _ = device.free_descriptor_sets(descriptor_pool, &[self.descriptor_set]);
        }
    }
}

/// Persistent host-visible staging buffer reused by every upload.
///
/// The buffer grows geometrically and is only recreated (after the GPU is
/// idle) when an upload needs more space, removing the per-upload allocation
/// and destruction of the original implementation.
pub(crate) struct StagingBuffer {
    buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    capacity: vk::DeviceSize,
}

impl Default for StagingBuffer {
    fn default() -> Self {
        Self {
            buffer: vk::Buffer::null(),
            memory: vk::DeviceMemory::null(),
            capacity: 0,
        }
    }
}

impl StagingBuffer {
    /// Destroys the staging buffer and its memory, if any.
    pub(crate) fn destroy(&self, device: &ash::Device) {
        if self.buffer == vk::Buffer::null() {
            return;
        }
        // SAFETY: both handles were created by `device` and the caller
        // guarantees no in-flight work still references them.
        unsafe {
            device.destroy_buffer(self.buffer, None);
            device.free_memory(self.memory, None);
        }
    }

    /// Grows the buffer to hold at least `size` bytes.
    ///
    /// `fence` must guard the previous upload that used this buffer; it is
    /// waited on before the old buffer is destroyed. The caller guarantees the
    /// fence is the upload fence and that a failure to wait is an error.
    fn reserve(
        &mut self,
        device: &ash::Device,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        fence: vk::Fence,
        size: vk::DeviceSize,
    ) -> Result<(), RenderError> {
        if self.capacity >= size {
            return Ok(());
        }
        // SAFETY: the fence guards the previous upload that used this buffer;
        // it is signalled once that upload completes.
        unsafe { device.wait_for_fences(&[fence], true, u64::MAX) }.map_err(|error| {
            RenderError::Upload {
                message: format!("failed to wait for the upload fence: {error}"),
            }
        })?;

        let capacity = size.next_power_of_two().max(64);
        let (buffer, memory) = create_staging_buffer(device, memory_properties, capacity)?;
        self.destroy(device);
        self.buffer = buffer;
        self.memory = memory;
        self.capacity = capacity;
        Ok(())
    }
}

/// Returns the Vulkan format used for a [`TextureFormat`].
fn image_format(format: TextureFormat) -> vk::Format {
    match format {
        TextureFormat::Rgba8Srgb => vk::Format::R8G8B8A8_SRGB,
        TextureFormat::Rgba8Unorm => vk::Format::R8G8B8A8_UNORM,
        TextureFormat::R8Unorm => vk::Format::R8_UNORM,
    }
}

/// Maps the component swizzle used for R8 coverage textures.
///
/// An `R8_UNORM` image is broadcast to `(1, 1, 1, R)` so glyph coverage lands
/// in the alpha channel and the tint carries the text colour.
fn component_mapping(format: TextureFormat) -> vk::ComponentMapping {
    match format {
        TextureFormat::R8Unorm => vk::ComponentMapping {
            r: vk::ComponentSwizzle::ONE,
            g: vk::ComponentSwizzle::ONE,
            b: vk::ComponentSwizzle::ONE,
            a: vk::ComponentSwizzle::R,
        },
        _ => vk::ComponentMapping::default(),
    }
}

/// Creates the GPU image, view and descriptor set for `desc`.
pub(crate) fn create_texture(
    device: &ash::Device,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    descriptor_set_layout: vk::DescriptorSetLayout,
    descriptor_pool: vk::DescriptorPool,
    sampler: vk::Sampler,
    desc: TextureDesc,
) -> Result<VulkanTexture, RenderError> {
    let format = image_format(desc.format);
    let mip_levels = desc.mip_levels.max(1);
    let image_info = vk::ImageCreateInfo::default()
        .image_type(vk::ImageType::TYPE_2D)
        .format(format)
        .extent(vk::Extent3D {
            width: desc.width,
            height: desc.height,
            depth: 1,
        })
        .mip_levels(mip_levels)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);

    // SAFETY: `image_info` borrows a local valid for the call.
    let image = unsafe { device.create_image(&image_info, None) }.map_err(|error| {
        RenderError::OutOfMemory {
            message: format!("failed to create a texture image: {error}"),
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
            // SAFETY: `image` is owned here and not used elsewhere yet.
            unsafe { device.destroy_image(image, None) };
            return Err(error);
        }
    };
    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type);
    // SAFETY: `allocate_info` is valid; the memory is freed with the image.
    let memory = unsafe { device.allocate_memory(&allocate_info, None) }.map_err(|error| {
        // SAFETY: `image` is owned here and not used elsewhere yet.
        unsafe { device.destroy_image(image, None) };
        RenderError::OutOfMemory {
            message: format!("failed to allocate texture memory: {error}"),
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
            message: format!("failed to bind texture memory: {error}"),
        });
    }

    let subresource_range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(mip_levels)
        .base_array_layer(0)
        .layer_count(1);
    let view_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .components(component_mapping(desc.format))
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
                message: format!("failed to create a texture image view: {error}"),
            });
        }
    };

    let set_layouts = [descriptor_set_layout];
    let allocate_sets = vk::DescriptorSetAllocateInfo::default()
        .descriptor_pool(descriptor_pool)
        .set_layouts(&set_layouts);
    // SAFETY: `allocate_sets` borrows locals valid for the call.
    let descriptor_set = match unsafe { device.allocate_descriptor_sets(&allocate_sets) } {
        Ok(mut sets) => match sets.pop() {
            Some(set) => set,
            None => {
                // SAFETY: both handles are owned here.
                unsafe {
                    device.destroy_image_view(view, None);
                    device.destroy_image(image, None);
                    device.free_memory(memory, None);
                }
                return Err(RenderError::OutOfMemory {
                    message: "the descriptor pool returned no descriptor set".to_owned(),
                });
            }
        },
        Err(error) => {
            // SAFETY: both handles are owned here.
            unsafe {
                device.destroy_image_view(view, None);
                device.destroy_image(image, None);
                device.free_memory(memory, None);
            }
            return Err(RenderError::OutOfMemory {
                message: format!("failed to allocate a texture descriptor set: {error}"),
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
        .dst_binding(0)
        .descriptor_type(vk::DescriptorType::COMBINED_IMAGE_SAMPLER)
        .image_info(&image_infos);
    // SAFETY: the descriptor set is not in use and the image info is valid.
    unsafe { device.update_descriptor_sets(&[write], &[]) };

    Ok(VulkanTexture {
        desc,
        image,
        view,
        memory,
        descriptor_set,
        uploaded: false,
    })
}

/// Copies a tightly packed sub-rectangle of `pixels` into `texture`.
///
/// The command pool must not have any command buffers in flight; the function
/// resets it, records, submits to `queue` and waits for `fence`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn upload(
    device: &ash::Device,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    staging: &mut StagingBuffer,
    texture: &mut VulkanTexture,
    region: PixelRegion,
    pixels: &[u8],
) -> Result<(), RenderError> {
    let staging_size = pixels.len() as vk::DeviceSize;
    staging.reserve(device, memory_properties, fence, staging_size.max(1))?;

    // SAFETY: the staging memory is host-visible and not in use; `reserve`
    // guarantees the buffer holds at least `staging_size` bytes.
    let mapped =
        unsafe { device.map_memory(staging.memory, 0, staging_size, vk::MemoryMapFlags::empty()) }
            .map_err(|error| RenderError::Upload {
                message: format!("failed to map the staging buffer: {error}"),
            })?;
    // SAFETY: `mapped` aliases `staging_size` writable bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), mapped.cast::<u8>(), pixels.len());
        device.unmap_memory(staging.memory);
    }

    let staging_buffer = staging.buffer;
    let (old_layout, src_access, src_stage) = if texture.uploaded {
        (
            vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
            vk::AccessFlags::SHADER_READ,
            vk::PipelineStageFlags::FRAGMENT_SHADER,
        )
    } else {
        (
            vk::ImageLayout::UNDEFINED,
            vk::AccessFlags::empty(),
            vk::PipelineStageFlags::TOP_OF_PIPE,
        )
    };

    let result = record_and_submit(
        device,
        queue,
        command_pool,
        command_buffer,
        fence,
        |command_buffer| {
            // SAFETY: the command buffer is recording and `texture` is live.
            unsafe {
                transition_image(
                    device,
                    command_buffer,
                    texture,
                    old_layout,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    src_access,
                    vk::AccessFlags::TRANSFER_WRITE,
                    src_stage,
                    vk::PipelineStageFlags::TRANSFER,
                );

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
                device.cmd_copy_buffer_to_image(
                    command_buffer,
                    staging_buffer,
                    texture.image,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[copy],
                );
                transition_image(
                    device,
                    command_buffer,
                    texture,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    vk::ImageLayout::SHADER_READ_ONLY_OPTIMAL,
                    vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::SHADER_READ,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::FRAGMENT_SHADER,
                );
            }
        },
    );

    // SAFETY: the fence has been waited on, so the staging buffer is free and
    // persists for the next upload.
    // A failure that unwound a recorded (or partially recorded) upload may
    // have left the image mid-transition. Force the next upload to start from
    // `UNDEFINED` instead of assuming the sampled layout, and keep the
    // texture unrenderable until it is re-uploaded.
    texture.uploaded = result.is_ok();
    result
}

/// Creates a host-visible, host-coherent staging buffer.
fn create_staging_buffer(
    device: &ash::Device,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    size: vk::DeviceSize,
) -> Result<(vk::Buffer, vk::DeviceMemory), RenderError> {
    let buffer_info = vk::BufferCreateInfo::default()
        .size(size)
        .usage(vk::BufferUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE);
    // SAFETY: `buffer_info` borrows a local valid for the call.
    let buffer = unsafe { device.create_buffer(&buffer_info, None) }.map_err(|error| {
        RenderError::Upload {
            message: format!("failed to create the staging buffer: {error}"),
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
    // SAFETY: `allocate_info` is valid.
    let memory = unsafe { device.allocate_memory(&allocate_info, None) }.map_err(|error| {
        // SAFETY: `buffer` is owned here.
        unsafe { device.destroy_buffer(buffer, None) };
        RenderError::OutOfMemory {
            message: format!("failed to allocate staging memory: {error}"),
        }
    })?;

    // SAFETY: `memory` was allocated for `buffer`.
    if let Err(error) = unsafe { device.bind_buffer_memory(buffer, memory, 0) } {
        // SAFETY: both handles are owned here.
        unsafe {
            device.destroy_buffer(buffer, None);
            device.free_memory(memory, None);
        }
        return Err(RenderError::Upload {
            message: format!("failed to bind staging memory: {error}"),
        });
    }

    Ok((buffer, memory))
}

/// Emits an image-memory barrier that transitions `texture` between layouts.
#[allow(clippy::too_many_arguments)]
fn transition_image(
    device: &ash::Device,
    command_buffer: vk::CommandBuffer,
    texture: &VulkanTexture,
    old_layout: vk::ImageLayout,
    new_layout: vk::ImageLayout,
    src_access: vk::AccessFlags,
    dst_access: vk::AccessFlags,
    src_stage: vk::PipelineStageFlags,
    dst_stage: vk::PipelineStageFlags,
) {
    let subresource_range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(texture.desc.mip_levels.max(1))
        .base_array_layer(0)
        .layer_count(1);
    let barrier = vk::ImageMemoryBarrier::default()
        .old_layout(old_layout)
        .new_layout(new_layout)
        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
        .image(texture.image)
        .subresource_range(subresource_range)
        .src_access_mask(src_access)
        .dst_access_mask(dst_access);
    // SAFETY: `barrier` refers to a live image owned by `texture`.
    unsafe {
        device.cmd_pipeline_barrier(
            command_buffer,
            src_stage,
            dst_stage,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[barrier],
        );
    }
}

/// Records `record` into a one-time command buffer and submits it.
fn record_and_submit<F>(
    device: &ash::Device,
    queue: vk::Queue,
    command_pool: vk::CommandPool,
    command_buffer: vk::CommandBuffer,
    fence: vk::Fence,
    record: F,
) -> Result<(), RenderError>
where
    F: FnOnce(vk::CommandBuffer),
{
    // SAFETY: the caller guarantees no command buffers from this pool are in
    // flight; the fence was created signalled so the first wait returns.
    unsafe {
        device
            .wait_for_fences(&[fence], true, u64::MAX)
            .map_err(|error| RenderError::Upload {
                message: format!("failed to wait for the upload fence: {error}"),
            })?;
        device
            .reset_command_pool(command_pool, vk::CommandPoolResetFlags::empty())
            .map_err(|error| RenderError::Upload {
                message: format!("failed to reset the upload command pool: {error}"),
            })?;
        let begin_info = vk::CommandBufferBeginInfo::default()
            .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
        device
            .begin_command_buffer(command_buffer, &begin_info)
            .map_err(|error| RenderError::Upload {
                message: format!("failed to begin the upload command buffer: {error}"),
            })?;
    }

    record(command_buffer);

    // SAFETY: the command buffer is recording and was begun above. The fence
    // is reset only immediately before the submit so an earlier error leaves
    // it signalled and the frame slot reusable.
    unsafe {
        device
            .end_command_buffer(command_buffer)
            .map_err(|error| RenderError::Upload {
                message: format!("failed to end the upload command buffer: {error}"),
            })?;
        device
            .reset_fences(&[fence])
            .map_err(|error| RenderError::Upload {
                message: format!("failed to reset the upload fence: {error}"),
            })?;
        let command_buffers = [command_buffer];
        let submit = vk::SubmitInfo::default().command_buffers(&command_buffers);
        device
            .queue_submit(queue, &[submit], fence)
            .map_err(|error| RenderError::Upload {
                message: format!("failed to submit the upload: {error}"),
            })?;
        device
            .wait_for_fences(&[fence], true, u64::MAX)
            .map_err(|error| RenderError::Upload {
                message: format!("failed to wait for the upload: {error}"),
            })?;
    }

    Ok(())
}

/// Finds a memory type satisfying `properties` among `type_bits`.
pub(crate) fn find_memory_type(
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    type_bits: u32,
    properties: vk::MemoryPropertyFlags,
) -> Result<u32, RenderError> {
    for index in 0..memory_properties.memory_type_count {
        let supported = type_bits & (1u32 << index) != 0;
        if supported
            && memory_properties.memory_types[index as usize]
                .property_flags
                .contains(properties)
        {
            return Ok(index);
        }
    }
    Err(RenderError::OutOfMemory {
        message: format!("no memory type supports {properties:?}"),
    })
}
