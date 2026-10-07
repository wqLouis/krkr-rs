//! Swapchain, render pass and framebuffer management for the Vulkan backend.
//!
//! The swapchain is recreated on resize and on `VK_ERROR_OUT_OF_DATE_KHR`
//! / `VK_SUBOPTIMAL_KHR`. The surface format, present mode and render pass are
//! pinned once (the first time the swapchain is built) and reused on every
//! recreation, so the compiled pipelines never have to be rebuilt.

use ash::vk;

use krkr_api::error::RenderError;

/// Immutable-per-format render pass plus its framebuffers.
pub(crate) struct SwapchainState {
    /// Swapchain handle.
    pub(crate) swapchain: vk::SwapchainKHR,
    /// Swapchain images.
    pub(crate) images: Vec<vk::Image>,
    /// Views created for [`Self::images`].
    pub(crate) image_views: Vec<vk::ImageView>,
    /// Framebuffers, one per image view.
    pub(crate) framebuffers: Vec<vk::Framebuffer>,
    /// Chosen surface format.
    pub(crate) format: vk::SurfaceFormatKHR,
    /// Current extent in physical pixels.
    pub(crate) extent: vk::Extent2D,
    /// Chosen present mode.
    pub(crate) present_mode: vk::PresentModeKHR,
    /// Single-attachment render pass used by every frame.
    pub(crate) render_pass: vk::RenderPass,
}

impl SwapchainState {
    /// Destroys the framebuffers, image views and swapchain.
    ///
    /// The render pass is intentionally left alive: it is owned by the pinned
    /// format and reused by the replacement swapchain.
    pub(crate) fn destroy(
        &self,
        device: &ash::Device,
        swapchain_loader: &ash::khr::swapchain::Device,
    ) {
        // SAFETY: all handles were created by `device`; the caller guarantees
        // the GPU is idle and that the swapchain is not in use. Null handles
        // are skipped so a failed build can be torn down safely.
        unsafe {
            for framebuffer in &self.framebuffers {
                device.destroy_framebuffer(*framebuffer, None);
            }
            for image_view in &self.image_views {
                device.destroy_image_view(*image_view, None);
            }
            if self.swapchain != vk::SwapchainKHR::null() {
                swapchain_loader.destroy_swapchain(self.swapchain, None);
            }
        }
    }

    /// Destroys the render pass (final teardown only).
    pub(crate) fn destroy_render_pass(&self, device: &ash::Device) {
        // SAFETY: the render pass was created by `device` and is no longer
        // referenced by any live framebuffer or pipeline.
        unsafe {
            if self.render_pass != vk::RenderPass::null() {
                device.destroy_render_pass(self.render_pass, None);
            }
        }
    }
}

/// Builds the swapchain for `surface`.
///
/// `requested` is the desired pixel size; it is only used when the surface
/// reports a free extent (`current_extent == u32::MAX`). When `old` is
/// supplied its format, present mode and render pass are reused so the
/// swapchain can be recreated without touching the graphics pipelines.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create(
    device: &ash::Device,
    _instance: &ash::Instance,
    physical_device: vk::PhysicalDevice,
    surface_loader: &ash::khr::surface::Instance,
    swapchain_loader: &ash::khr::swapchain::Device,
    surface: vk::SurfaceKHR,
    requested: (u32, u32),
    old: Option<&SwapchainState>,
) -> Result<SwapchainState, RenderError> {
    // A partially built state has a null render pass; treat it as "no old
    // swapchain" so a retry rebuilds everything from scratch.
    let old = old.filter(|state| state.render_pass != vk::RenderPass::null());
    // SAFETY: `physical_device` and `surface` belong to `instance`; the
    // queries are read-only.
    let capabilities = unsafe {
        surface_loader.get_physical_device_surface_capabilities(physical_device, surface)
    }
    .map_err(|error| RenderError::Framebuffer {
        message: format!("failed to query surface capabilities: {error}"),
    })?;

    let format = match old {
        Some(old) => old.format,
        None => {
            // SAFETY: as above.
            let formats = unsafe {
                surface_loader.get_physical_device_surface_formats(physical_device, surface)
            }
            .map_err(|error| RenderError::Framebuffer {
                message: format!("failed to query surface formats: {error}"),
            })?;
            choose_surface_format(&formats).ok_or_else(|| RenderError::Unsupported {
                message: "the presentation surface exposes no color format".to_owned(),
            })?
        }
    };

    let present_mode = match old {
        Some(old) => old.present_mode,
        None => {
            // SAFETY: as above.
            let modes = unsafe {
                surface_loader.get_physical_device_surface_present_modes(physical_device, surface)
            }
            .map_err(|error| RenderError::Framebuffer {
                message: format!("failed to query surface present modes: {error}"),
            })?;
            choose_present_mode(&modes)
        }
    };

    let extent = choose_extent(&capabilities, requested);
    let mut image_count = capabilities.min_image_count.saturating_add(1);
    if capabilities.max_image_count > 0 {
        image_count = image_count.min(capabilities.max_image_count);
    }
    image_count = image_count.max(1);

    let composite_alpha = if capabilities
        .supported_composite_alpha
        .contains(vk::CompositeAlphaFlagsKHR::OPAQUE)
    {
        vk::CompositeAlphaFlagsKHR::OPAQUE
    } else {
        vk::CompositeAlphaFlagsKHR::INHERIT
    };

    let create_info = vk::SwapchainCreateInfoKHR::default()
        .surface(surface)
        .min_image_count(image_count)
        .image_format(format.format)
        .image_color_space(format.color_space)
        .image_extent(extent)
        .image_array_layers(1)
        .image_usage(vk::ImageUsageFlags::COLOR_ATTACHMENT)
        .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
        .pre_transform(capabilities.current_transform)
        .composite_alpha(composite_alpha)
        .present_mode(present_mode)
        .clipped(true)
        .old_swapchain(old.map_or(vk::SwapchainKHR::null(), |old| old.swapchain));

    // SAFETY: `create_info` borrows `surface`, which outlives the call; the
    // old swapchain is only used as a hint and is destroyed by the caller.
    let swapchain =
        unsafe { swapchain_loader.create_swapchain(&create_info, None) }.map_err(|error| {
            RenderError::Framebuffer {
                message: format!("failed to create the swapchain: {error}"),
            }
        })?;

    // SAFETY: `swapchain` was just created for this device.
    let images = unsafe { swapchain_loader.get_swapchain_images(swapchain) }.map_err(|error| {
        RenderError::Framebuffer {
            message: format!("failed to query swapchain images: {error}"),
        }
    })?;

    let render_pass = match old {
        Some(old) => old.render_pass,
        None => create_render_pass(device, format.format)?,
    };

    let mut image_views = Vec::with_capacity(images.len());
    for image in &images {
        match create_image_view(device, *image, format.format) {
            Ok(view) => image_views.push(view),
            Err(error) => {
                destroy_partial(
                    device,
                    swapchain_loader,
                    swapchain,
                    &image_views,
                    &[],
                    old,
                    render_pass,
                );
                return Err(error);
            }
        }
    }

    let mut framebuffers = Vec::with_capacity(image_views.len());
    for view in &image_views {
        let attachments = [*view];
        let framebuffer_info = vk::FramebufferCreateInfo::default()
            .render_pass(render_pass)
            .attachments(&attachments)
            .width(extent.width)
            .height(extent.height)
            .layers(1);
        // SAFETY: `render_pass` and `view` were created by `device`.
        match unsafe { device.create_framebuffer(&framebuffer_info, None) } {
            Ok(framebuffer) => framebuffers.push(framebuffer),
            Err(error) => {
                destroy_partial(
                    device,
                    swapchain_loader,
                    swapchain,
                    &image_views,
                    &framebuffers,
                    old,
                    render_pass,
                );
                return Err(RenderError::Framebuffer {
                    message: format!("failed to create a swapchain framebuffer: {error}"),
                });
            }
        }
    }

    Ok(SwapchainState {
        swapchain,
        images,
        image_views,
        framebuffers,
        format,
        extent,
        present_mode,
        render_pass,
    })
}

/// Destroys the partially built swapchain resources of a failed [`create`].
fn destroy_partial(
    device: &ash::Device,
    swapchain_loader: &ash::khr::swapchain::Device,
    swapchain: vk::SwapchainKHR,
    image_views: &[vk::ImageView],
    framebuffers: &[vk::Framebuffer],
    old: Option<&SwapchainState>,
    render_pass: vk::RenderPass,
) {
    // SAFETY: every handle was created by `device`; initialization failed so
    // nothing else references them.
    unsafe {
        for framebuffer in framebuffers {
            device.destroy_framebuffer(*framebuffer, None);
        }
        for view in image_views {
            device.destroy_image_view(*view, None);
        }
        swapchain_loader.destroy_swapchain(swapchain, None);
        if old.is_none() {
            device.destroy_render_pass(render_pass, None);
        }
    }
}

/// Creates the single-attachment color render pass used by all pipelines.
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
        .final_layout(vk::ImageLayout::PRESENT_SRC_KHR);
    let attachments = [attachment];

    let color_reference = vk::AttachmentReference::default()
        .attachment(0)
        .layout(vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL);
    let color_references = [color_reference];
    let subpass = vk::SubpassDescription::default()
        .pipeline_bind_point(vk::PipelineBindPoint::GRAPHICS)
        .color_attachments(&color_references);
    let subpasses = [subpass];

    let dependency = vk::SubpassDependency::default()
        .src_subpass(vk::SUBPASS_EXTERNAL)
        .dst_subpass(0)
        .src_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .src_access_mask(vk::AccessFlags::empty())
        .dst_stage_mask(vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT)
        .dst_access_mask(vk::AccessFlags::COLOR_ATTACHMENT_WRITE);
    let dependencies = [dependency];

    let create_info = vk::RenderPassCreateInfo::default()
        .attachments(&attachments)
        .subpasses(&subpasses)
        .dependencies(&dependencies);

    // SAFETY: `create_info` borrows locals valid for the call.
    unsafe { device.create_render_pass(&create_info, None) }.map_err(|error| {
        RenderError::Framebuffer {
            message: format!("failed to create the render pass: {error}"),
        }
    })
}

/// Creates a color image view over one swapchain image.
fn create_image_view(
    device: &ash::Device,
    image: vk::Image,
    format: vk::Format,
) -> Result<vk::ImageView, RenderError> {
    let subresource_range = vk::ImageSubresourceRange::default()
        .aspect_mask(vk::ImageAspectFlags::COLOR)
        .base_mip_level(0)
        .level_count(1)
        .base_array_layer(0)
        .layer_count(1);
    let create_info = vk::ImageViewCreateInfo::default()
        .image(image)
        .view_type(vk::ImageViewType::TYPE_2D)
        .format(format)
        .components(vk::ComponentMapping::default())
        .subresource_range(subresource_range);

    // SAFETY: `image` was created by `device` with a compatible format.
    unsafe { device.create_image_view(&create_info, None) }.map_err(|error| {
        RenderError::Framebuffer {
            message: format!("failed to create a swapchain image view: {error}"),
        }
    })
}

/// Picks an 8-bit sRGB surface format, preferring BGRA (the common desktop
/// layout). Falls back to the first advertised format.
fn choose_surface_format(formats: &[vk::SurfaceFormatKHR]) -> Option<vk::SurfaceFormatKHR> {
    let preferred = [
        vk::Format::B8G8R8A8_SRGB,
        vk::Format::R8G8B8A8_SRGB,
        vk::Format::B8G8R8A8_UNORM,
        vk::Format::R8G8B8A8_UNORM,
    ];
    for format in preferred {
        if let Some(found) = formats.iter().find(|candidate| candidate.format == format) {
            return Some(*found);
        }
    }
    formats.first().copied()
}

/// Picks the present mode: `MAILBOX` when available (low latency), else the
/// universally supported `FIFO`.
fn choose_present_mode(modes: &[vk::PresentModeKHR]) -> vk::PresentModeKHR {
    if modes.contains(&vk::PresentModeKHR::MAILBOX) {
        vk::PresentModeKHR::MAILBOX
    } else {
        vk::PresentModeKHR::FIFO
    }
}

/// Resolves the swapchain extent from the capabilities and the requested size.
fn choose_extent(capabilities: &vk::SurfaceCapabilitiesKHR, requested: (u32, u32)) -> vk::Extent2D {
    if capabilities.current_extent.width != u32::MAX {
        return vk::Extent2D {
            width: capabilities.current_extent.width.max(1),
            height: capabilities.current_extent.height.max(1),
        };
    }
    let fallback = (1280, 720);
    let (mut width, mut height) = if requested.0 == 0 || requested.1 == 0 {
        fallback
    } else {
        requested
    };
    width = width.clamp(
        capabilities.min_image_extent.width,
        capabilities.max_image_extent.width,
    );
    height = height.clamp(
        capabilities.min_image_extent.height,
        capabilities.max_image_extent.height,
    );
    vk::Extent2D {
        width: width.max(1),
        height: height.max(1),
    }
}
