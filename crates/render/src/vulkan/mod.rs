//! Vulkan (`ash`) render backend.
//!
//! This module implements the real Vulkan mechanism behind the stateless
//! [`crate::RenderPipeline`] facade. It follows the layer split in
//! `docs/migration/02-target-architecture.md` §2.1 (D9): the pipeline holds
//! only the instance and selected physical device; every mutable GPU resource
//! lives in the caller-owned [`VulkanState`]. No process globals, no
//! `&mut self`, no hidden caches.
//!
//! Implemented:
//!
//! * instance creation with `VK_KHR_surface` and the available platform
//!   surface extensions, plus optional `VK_LAYER_KHRONOS_validation`;
//! * surface creation through `ash-window`, physical-device selection and a
//!   combined graphics+present queue family;
//! * swapchain create/recreate with image views, a render pass and
//!   framebuffers (`OUT_OF_DATE`/`SUBOPTIMAL` handled);
//! * a staging-buffer upload path with layout transitions and sub-rectangle
//!   (`PixelRegion`) copies;
//! * textured and solid pipelines with per-blend fixed-function state, a
//!   host-visible unit-quad vertex buffer and per-texture descriptor sets;
//! * a per-frame command buffer and sync objects, and full scene traversal
//!   with transforms, opacity, scissor clipping and `z` ordering.
//!
//! Deferred (reported as [`RenderError::Unsupported`], never silently
//! degraded, per D11):
//!
//! * destination-dependent blend modes;
//! * mask clips (a rectangular clip uses the scissor);
//! * GPU glyph runs (per-glyph placement is not implemented);
//! * only mip level 0 is uploaded (no mip generation).
//!
//! # Safety
//!
//! `ash` is a raw FFI binding, so this module necessarily contains `unsafe`
//! blocks. The crate does not set `#![forbid(unsafe_code)]`. Every block is
//! annotated with the invariant that makes it sound. The instance is shared
//! through an [`Arc`] guard, so the pipeline and state may be dropped in any
//! order.

mod device;
mod draw;
mod pipelines;
mod shaders;
mod swapchain;
mod target;
mod texture;

use std::collections::HashMap;
use std::sync::Arc;

use ash::vk;
use raw_window_handle::{
    AndroidDisplayHandle, RawDisplayHandle, RawWindowHandle, WindowsDisplayHandle,
};

use api::error::RenderError;
use api::geometry::PixelRegion;
use api::host::{Presentation, SurfaceEvent};

use crate::render_target::{RenderTarget, RenderTargetDesc};
use crate::scene::RenderScene;
use crate::state::{RenderState, StateKind, Surface, SurfaceKind};
use crate::texture::{Texture, TextureDesc};
use crate::{BackendKind, GpuCapabilities, TextureFormat};

/// Number of frames recorded concurrently.
pub const MAX_FRAMES_IN_FLIGHT: usize = 2;

/// Size of the push-constant block shared with the shaders.
pub(crate) const PUSH_CONSTANT_SIZE: u32 = 96;

/// Immutable Vulkan mechanism held by [`crate::RenderPipeline`].
///
/// This value contains only handles and configuration selected at creation
/// time. The logical device, swapchain and every GPU resource that changes per
/// caller live in the caller-owned [`VulkanState`].
pub struct VulkanPipeline {
    /// The Vulkan loader and instance, destroyed when the last owner drops.
    instance_guard: Arc<InstanceGuard>,
    /// Selected physical device.
    physical_device: vk::PhysicalDevice,
    /// `maxImageDimension2D` of the selected device.
    max_texture_dimension: u32,
    /// Cached memory-type properties of the selected device.
    memory_properties: vk::PhysicalDeviceMemoryProperties,
    /// Preferred swapchain format requested by the caller.
    preferred_format: TextureFormat,
}

/// Owns the Vulkan loader and instance, destroying the instance and then
/// unloading the loader exactly once when the last [`VulkanPipeline`] /
/// [`VulkanState`] pair drops.
///
/// Owning both together is required for soundness: the instance's function
/// pointers point into the loader, so the loader must outlive the instance.
/// Sharing the guard through an [`Arc`] also makes the pipeline/state drop
/// order irrelevant.
pub(crate) struct InstanceGuard {
    /// The loader handle; kept alive for the instance's function pointers.
    entry: ash::Entry,
    /// The owned instance.
    instance: ash::Instance,
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        // SAFETY: no device or surface survives the guard, by construction, and
        // `entry` is still alive while the instance is destroyed here.
        unsafe {
            self.instance.destroy_instance(None);
        }
    }
}

impl VulkanPipeline {
    /// Builds the Vulkan mechanism for `config`.
    ///
    /// Creates the instance and selects a physical device that supports
    /// `VK_KHR_swapchain` and a graphics queue. The logical device is created
    /// in [`Self::begin_with_display`] because present-queue selection needs
    /// the window surface.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unsupported`] when no Vulkan loader or suitable
    /// device is available.
    pub(crate) fn new(config: &crate::PipelineConfig) -> Result<Self, RenderError> {
        let bundle = device::create_instance(config)?;
        if bundle.validation {
            log::info!("Vulkan validation layer VK_LAYER_KHRONOS_validation enabled");
        }
        Ok(Self {
            instance_guard: Arc::new(InstanceGuard {
                entry: bundle.entry,
                instance: bundle.instance,
            }),
            physical_device: bundle.physical_device,
            max_texture_dimension: bundle.max_texture_dimension,
            memory_properties: bundle.memory_properties,
            preferred_format: config.preferred_format,
        })
    }

    /// Returns the capabilities advertised by the selected device.
    pub fn capabilities(&self) -> GpuCapabilities {
        GpuCapabilities {
            backend: BackendKind::Vulkan,
            max_texture_dimension: self.max_texture_dimension,
            preferred_format: self.preferred_format,
            supports_destination_dependent_blend: false,
            // Offscreen targets are a real GPU path: colour-only render pass,
            // per-format pipelines and `vkCmdCopyImageToBuffer` readback.
            supports_offscreen_targets: true,
            // GPU glyph runs and mask clips are not implemented; a scene that
            // uses them fails with `RenderError::Unsupported` rather than
            // rendering incorrectly.
            supports_glyph_atlas: false,
        }
    }

    /// Creates the surface and render state for `window`.
    ///
    /// The platform display connection cannot be recovered from a bare
    /// [`RawWindowHandle`], so this entry point only works on platforms whose
    /// display handle carries no extra data (Android, Win32). On X11, XCB and
    /// Wayland use [`crate::begin_render_state_with_display`] instead.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unsupported`] when the display handle must be
    /// supplied explicitly.
    pub(crate) fn begin(
        &self,
        window: &RawWindowHandle,
    ) -> Result<(Surface, RenderState), RenderError> {
        let display = display_for_window(window).ok_or_else(|| RenderError::Unsupported {
            message: "this platform needs an explicit display handle; call \
                      begin_render_state_with_display"
                .to_owned(),
        })?;
        self.begin_with_display(&display, window)
    }

    /// Creates the surface and render state from an explicit display handle.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError`] when surface/device/swapchain creation fails.
    pub(crate) fn begin_with_display(
        &self,
        display: &RawDisplayHandle,
        window: &RawWindowHandle,
    ) -> Result<(Surface, RenderState), RenderError> {
        let surface_loader = ash::khr::surface::Instance::new(
            &self.instance_guard.entry,
            &self.instance_guard.instance,
        );

        // SAFETY: `display` and `window` must describe a live window/display
        // pair; that is the caller's contract. `ash-window` dispatches on the
        // platform variant and creates the matching `VkSurfaceKHR`.
        let surface = unsafe {
            ash_window::create_surface(
                &self.instance_guard.entry,
                &self.instance_guard.instance,
                *display,
                *window,
                None,
            )
        }
        .map_err(|error| RenderError::Unsupported {
            message: format!("failed to create the Vulkan surface: {error}"),
        })?;

        let device_bundle = device::create_device(
            &self.instance_guard.instance,
            self.physical_device,
            &surface_loader,
            surface,
        )
        .inspect_err(|_error| {
            // SAFETY: `surface` was created here and no swapchain uses it yet.
            unsafe { surface_loader.destroy_surface(surface, None) };
        })?;

        let swapchain_loader =
            ash::khr::swapchain::Device::new(&self.instance_guard.instance, &device_bundle.device);

        let swapchain = match swapchain::create(
            &device_bundle.device,
            self.physical_device,
            &surface_loader,
            &swapchain_loader,
            surface,
            (0, 0),
            self.preferred_format,
            None,
        ) {
            Ok(swapchain) => swapchain,
            Err(error) => {
                // SAFETY: partial resources are destroyed here; nothing else
                // references them.
                unsafe {
                    device_bundle.device.destroy_device(None);
                    surface_loader.destroy_surface(surface, None);
                }
                return Err(error);
            }
        };

        let pipelines = match pipelines::create(
            &device_bundle.device,
            &self.memory_properties,
            swapchain.render_pass,
        ) {
            Ok(pipelines) => pipelines,
            Err(error) => {
                // SAFETY: tear down everything created above.
                unsafe {
                    swapchain.destroy(&device_bundle.device, &swapchain_loader);
                    swapchain.destroy_render_pass(&device_bundle.device);
                    device_bundle.device.destroy_device(None);
                    surface_loader.destroy_surface(surface, None);
                }
                return Err(error);
            }
        };

        // From here on the guard owns teardown: any `?` below destroys the
        // device, surface, swapchain, pipelines and whatever else was built.
        let mut teardown = BeginTeardown {
            device: &device_bundle.device,
            surface_loader: &surface_loader,
            swapchain_loader: &swapchain_loader,
            surface,
            swapchain: &swapchain,
            pipelines: &pipelines,
            frames: Vec::new(),
            render_finished: Vec::new(),
            upload: None,
            armed: true,
        };

        teardown.frames = create_frames(&device_bundle.device, device_bundle.queue_family_index)?;
        let image_count = swapchain.images.len().max(1);
        for _ in 0..image_count {
            // SAFETY: `device_bundle.device` is live.
            let semaphore = unsafe {
                device_bundle
                    .device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            }
            .map_err(|error| RenderError::Unsupported {
                message: format!("failed to create presentation semaphores: {error}"),
            })?;
            teardown.render_finished.push(semaphore);
        }

        let (upload_pool, upload_command, upload_fence) =
            create_upload_resources(&device_bundle.device, device_bundle.queue_family_index)?;
        // Copy the handles into the guard so a late failure frees them; ash
        // handles are cheap `Copy` values.
        teardown.upload = Some((upload_pool, upload_fence));

        // Success: transfer ownership to the state and disarm the guard.
        teardown.armed = false;
        let frames = std::mem::take(&mut teardown.frames);
        let render_finished = std::mem::take(&mut teardown.render_finished);
        drop(teardown);

        let surface_size = (swapchain.extent.width, swapchain.extent.height);

        let state = VulkanState {
            _instance_guard: self.instance_guard.clone(),
            physical_device: self.physical_device,
            device: device_bundle.device,
            graphics_queue: device_bundle.graphics_queue,
            present_queue: device_bundle.present_queue,
            surface_loader,
            swapchain_loader,
            surface,
            preferred_format: self.preferred_format,
            swapchain,
            pipelines,
            frames,
            images_in_flight: vec![vk::Fence::null(); image_count],
            render_finished,
            frame_in_flight: 0,
            textures: HashMap::new(),
            offscreen: HashMap::new(),
            render_targets: HashMap::new(),
            staging: texture::StagingBuffer::default(),
            upload_pool,
            upload_command,
            upload_fence,
            surface_size,
            frame_index: 0,
            swapchain_ready: true,
        };

        Ok((
            Surface {
                kind: SurfaceKind::Vulkan(VulkanSurface { surface }),
            },
            RenderState {
                kind: StateKind::Vulkan(Box::new(state)),
            },
        ))
    }

    /// Recreates the swapchain after a size change.
    pub(crate) fn resize(
        &self,
        surface: &Surface,
        state: &mut RenderState,
        size: (u32, u32),
    ) -> Result<SurfaceEvent, RenderError> {
        let surface_handle = match &surface.kind {
            SurfaceKind::Vulkan(surface) => surface.surface,
            SurfaceKind::Null(_) => {
                return Err(RenderError::InvalidArgument {
                    message: "the Vulkan backend received a null surface".to_owned(),
                });
            }
        };
        let vstate = match &mut state.kind {
            StateKind::Vulkan(state) => state,
            StateKind::Null(_) => {
                return Err(RenderError::InvalidArgument {
                    message: "the Vulkan backend received a null render state".to_owned(),
                });
            }
        };

        if surface_handle != vstate.surface {
            return Err(RenderError::InvalidArgument {
                message: "the surface does not belong to this render state".to_owned(),
            });
        }

        vstate.surface_size = size;
        let device = vstate.device.clone();
        // `recreate_swapchain` idles the device before touching the swapchain.
        recreate_swapchain(&device, vstate, surface_handle)?;
        Ok(SurfaceEvent::Resized(
            vstate.swapchain.extent.width,
            vstate.swapchain.extent.height,
        ))
    }

    /// Creates a texture handle after validating its descriptor.
    pub(crate) fn create_texture(&self, desc: TextureDesc) -> Result<Texture, RenderError> {
        validate_desc(&desc, self.max_texture_dimension)?;
        Ok(Texture::new(desc))
    }

    /// Releases the GPU resources and state slot for `tex`.
    pub(crate) fn drop_texture(&self, state: &mut RenderState, tex: Texture) {
        let StateKind::Vulkan(vstate) = &mut state.kind else {
            return;
        };
        let Some(texture) = vstate.textures.remove(&tex.key()) else {
            return;
        };
        let device = vstate.device.clone();
        // SAFETY: wait for in-flight work before freeing the image; the device
        // is idle afterwards.
        unsafe {
            let _ = device.device_wait_idle();
        }
        texture.destroy(&device, vstate.pipelines.descriptor_pool);
    }

    /// Copies a sub-rectangle of `pixels` into `tex`.
    pub(crate) fn upload_texture(
        &self,
        state: &mut RenderState,
        tex: &Texture,
        region: PixelRegion,
        pixels: &[u8],
    ) -> Result<(), RenderError> {
        validate_upload(tex.desc(), region, pixels)?;
        let StateKind::Vulkan(vstate) = &mut state.kind else {
            return Err(RenderError::InvalidArgument {
                message: "the Vulkan backend received a null render state".to_owned(),
            });
        };

        let key = tex.key();
        if !vstate.textures.contains_key(&key) {
            let gpu = texture::create_texture(
                &vstate.device,
                &self.memory_properties,
                vstate.pipelines.descriptor_set_layout,
                vstate.pipelines.descriptor_pool,
                vstate.pipelines.sampler,
                *tex.desc(),
            )?;
            vstate.textures.insert(key, gpu);
        }

        let gpu = vstate
            .textures
            .get_mut(&key)
            .ok_or_else(|| RenderError::Upload {
                message: "texture state disappeared during upload".to_owned(),
            })?;
        texture::upload(
            &vstate.device,
            &self.memory_properties,
            vstate.graphics_queue,
            vstate.upload_pool,
            vstate.upload_command,
            vstate.upload_fence,
            &mut vstate.staging,
            gpu,
            region,
            pixels,
        )
    }

    /// Creates an offscreen render-target handle after validating its
    /// descriptor.
    pub(crate) fn create_render_target(
        &self,
        desc: RenderTargetDesc,
    ) -> Result<RenderTarget, RenderError> {
        validate_target_desc(&desc, self.max_texture_dimension)?;
        Ok(RenderTarget::new(desc))
    }

    /// Releases the GPU resources and state slot for `target`.
    pub(crate) fn destroy_render_target(&self, state: &mut RenderState, target: RenderTarget) {
        let StateKind::Vulkan(vstate) = &mut state.kind else {
            return;
        };
        let Some(gpu) = vstate.render_targets.remove(&target.key()) else {
            return;
        };
        let device = vstate.device.clone();
        // SAFETY: wait for in-flight work before freeing the image.
        unsafe {
            let _ = device.device_wait_idle();
        }
        gpu.destroy(&device);
    }

    /// Renders `scene` into the offscreen `target`.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::InvalidArgument`] for a mismatched backend and
    /// [`RenderError::Unsupported`] when the scene needs a feature the Vulkan
    /// backend does not implement.
    pub(crate) fn render_to_target(
        &self,
        state: &mut RenderState,
        target: &RenderTarget,
        scene: &RenderScene,
    ) -> Result<(), RenderError> {
        let StateKind::Vulkan(vstate) = &mut state.kind else {
            return Err(RenderError::InvalidArgument {
                message: "the Vulkan backend received a null render state".to_owned(),
            });
        };
        let format = target.desc().format;
        let key = target.key();
        let vstate = &mut **vstate;
        let VulkanState {
            device,
            graphics_queue,
            upload_pool,
            upload_command,
            upload_fence,
            offscreen,
            render_targets,
            textures,
            ..
        } = vstate;

        if let std::collections::hash_map::Entry::Vacant(entry) = offscreen.entry(format) {
            entry.insert(target::OffscreenPass::create(
                device,
                &self.memory_properties,
                format,
            )?);
        }
        let pass = offscreen
            .get(&format)
            .ok_or_else(|| RenderError::Framebuffer {
                message: "offscreen pipeline state disappeared".to_owned(),
            })?;
        if let std::collections::hash_map::Entry::Vacant(entry) = render_targets.entry(key) {
            entry.insert(target::VulkanRenderTarget::create(
                device,
                &self.memory_properties,
                pass,
                *target.desc(),
            )?);
        }
        let gpu = render_targets
            .get(&key)
            .ok_or_else(|| RenderError::Framebuffer {
                message: "render-target state disappeared".to_owned(),
            })?;
        target::render(
            device,
            *graphics_queue,
            *upload_pool,
            *upload_command,
            *upload_fence,
            gpu,
            pass,
            textures,
            scene,
        )
    }

    /// Copies a sub-rectangle of an offscreen `target` into `out`.
    pub(crate) fn read_render_target(
        &self,
        state: &RenderState,
        target: &RenderTarget,
        region: PixelRegion,
        out: &mut [u8],
    ) -> Result<(), RenderError> {
        let StateKind::Vulkan(vstate) = &state.kind else {
            return Err(RenderError::InvalidArgument {
                message: "the Vulkan backend received a null render state".to_owned(),
            });
        };
        let gpu =
            vstate
                .render_targets
                .get(&target.key())
                .ok_or_else(|| RenderError::Framebuffer {
                    message: "render target has not been rendered yet".to_owned(),
                })?;
        validate_readback(target.desc(), region, out.len())?;
        target::read(
            &vstate.device,
            &self.memory_properties,
            vstate.graphics_queue,
            vstate.upload_pool,
            vstate.upload_command,
            vstate.upload_fence,
            gpu,
            region,
            out,
        )
    }

    /// Acquires, records, submits and presents one frame.
    pub(crate) fn render(
        &self,
        surface: &Surface,
        state: &mut RenderState,
        scene: &RenderScene,
    ) -> Result<Presentation, RenderError> {
        let surface_handle = match &surface.kind {
            SurfaceKind::Vulkan(surface) => surface.surface,
            SurfaceKind::Null(_) => {
                return Err(RenderError::InvalidArgument {
                    message: "the Vulkan backend received a null surface".to_owned(),
                });
            }
        };
        let StateKind::Vulkan(vstate) = &mut state.kind else {
            return Err(RenderError::InvalidArgument {
                message: "the Vulkan backend received a null render state".to_owned(),
            });
        };

        if surface_handle != vstate.surface {
            return Err(RenderError::InvalidArgument {
                message: "the surface does not belong to this render state".to_owned(),
            });
        }

        if !vstate.swapchain_ready || vstate.swapchain.images.is_empty() {
            return Ok(Presentation {
                presented: false,
                surface_event: Some(SurfaceEvent::OutOfDate),
            });
        }

        let device = vstate.device.clone();
        let swapchain_loader = vstate.swapchain_loader.clone();
        let frame_index = vstate.frame_in_flight.min(vstate.frames.len() - 1);
        let command_pool = vstate.frames[frame_index].command_pool;
        let command_buffer = vstate.frames[frame_index].command_buffer;
        let in_flight = vstate.frames[frame_index].in_flight;
        let image_available = vstate.frames[frame_index].image_available;
        let swapchain = vstate.swapchain.swapchain;
        let extent = vstate.swapchain.extent;
        let render_pass = vstate.swapchain.render_pass;
        let format = vstate.swapchain.format.format;
        let graphics_queue = vstate.graphics_queue;
        let present_queue = vstate.present_queue;

        // SAFETY: the fence belongs to this frame slot and was created
        // signalled, so the first wait returns immediately.
        unsafe { device.wait_for_fences(&[in_flight], true, u64::MAX) }
            .map_err(|error| device::result_error("failed to wait for the frame fence", error))?;

        // SAFETY: `swapchain` is valid; `image_available` must be unsignalled.
        let (image_index, suboptimal) = match unsafe {
            swapchain_loader.acquire_next_image(
                swapchain,
                u64::MAX,
                image_available,
                vk::Fence::null(),
            )
        } {
            Ok(value) => value,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => {
                recreate_swapchain(&device, vstate, surface_handle)?;
                return Ok(Presentation {
                    presented: false,
                    surface_event: Some(SurfaceEvent::OutOfDate),
                });
            }
            Err(error) => {
                return Err(device::result_error(
                    "failed to acquire a swapchain image",
                    error,
                ));
            }
        };

        // `image_available` is signalled by the acquire above. Until the submit
        // succeeds it is neither waited on nor safe to reuse, so any error in
        // this block must recreate it (see `recreate_acquired_semaphore`).
        let signal_semaphores = [vstate.render_finished[image_index as usize]];
        let record_result: Result<(), RenderError> = (|| {
            if vstate.images_in_flight[image_index as usize] != vk::Fence::null() {
                let image_fence = vstate.images_in_flight[image_index as usize];
                // SAFETY: the fence guards this swapchain image.
                unsafe { device.wait_for_fences(&[image_fence], true, u64::MAX) }.map_err(
                    |error| device::result_error("failed to wait for a swapchain image", error),
                )?;
            }

            // SAFETY: the fence/image are idle and the pool is not in use.
            unsafe {
                device.reset_fences(&[in_flight]).map_err(|error| {
                    device::result_error("failed to reset the frame fence", error)
                })?;
                device
                    .reset_command_pool(command_pool, vk::CommandPoolResetFlags::empty())
                    .map_err(|error| {
                        device::result_error("failed to reset the frame command pool", error)
                    })?;
            }

            let begin_info = vk::CommandBufferBeginInfo::default()
                .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT);
            // SAFETY: the command buffer is reset and idle.
            unsafe { device.begin_command_buffer(command_buffer, &begin_info) }.map_err(
                |error| device::result_error("failed to begin the frame command buffer", error),
            )?;

            let render_area = vk::Rect2D {
                offset: vk::Offset2D { x: 0, y: 0 },
                extent,
            };
            let srgb = format_is_srgb(format);
            let clear = clear_value(scene.background, srgb);
            let clear_values = [vk::ClearValue {
                color: vk::ClearColorValue { float32: clear },
            }];
            let framebuffer = framebuffers_for(vstate, image_index)?;
            let render_pass_begin = vk::RenderPassBeginInfo::default()
                .render_pass(render_pass)
                .framebuffer(framebuffer)
                .render_area(render_area)
                .clear_values(&clear_values);

            // SAFETY: the command buffer is recording; the render pass and
            // framebuffer are compatible and live.
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
                &device,
                command_buffer,
                &vstate.pipelines,
                &vstate.textures,
                scene,
                extent,
                srgb,
            )?;

            // SAFETY: the render pass and command buffer are recording.
            unsafe {
                device.cmd_end_render_pass(command_buffer);
                device.end_command_buffer(command_buffer).map_err(|error| {
                    device::result_error("failed to end the frame command buffer", error)
                })?;
            }

            let wait_semaphores = [image_available];
            let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
            let command_buffers = [command_buffer];
            let submit = vk::SubmitInfo::default()
                .wait_semaphores(&wait_semaphores)
                .wait_dst_stage_mask(&wait_stages)
                .command_buffers(&command_buffers)
                .signal_semaphores(&signal_semaphores);

            // SAFETY: all handles are live; the fence is reset and unsignalled.
            unsafe { device.queue_submit(graphics_queue, &[submit], in_flight) }
                .map_err(|error| device::result_error("failed to submit the frame", error))?;
            vstate.images_in_flight[image_index as usize] = in_flight;
            Ok(())
        })();

        if let Err(error) = record_result {
            recreate_acquired_semaphore(&device, vstate, frame_index);
            return Err(error);
        }

        let swapchains = [swapchain];
        let image_indices = [image_index];
        let present_info = vk::PresentInfoKHR::default()
            .wait_semaphores(&signal_semaphores)
            .swapchains(&swapchains)
            .image_indices(&image_indices);
        // SAFETY: the present info references live semaphores/swapchain.
        let present_result =
            unsafe { swapchain_loader.queue_present(present_queue, &present_info) };

        vstate.frame_in_flight = (vstate.frame_in_flight + 1) % MAX_FRAMES_IN_FLIGHT;
        vstate.frame_index = vstate.frame_index.wrapping_add(1);

        let mut needs_recreate = suboptimal;
        match present_result {
            Ok(false) => {}
            Ok(true) => needs_recreate = true,
            Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => needs_recreate = true,
            Err(vk::Result::ERROR_SURFACE_LOST_KHR) => return Err(RenderError::SurfaceLost),
            Err(error) => {
                return Err(device::result_error("failed to present the frame", error));
            }
        }

        if needs_recreate {
            recreate_swapchain(&device, vstate, surface_handle)?;
            return Ok(Presentation {
                presented: true,
                surface_event: Some(SurfaceEvent::OutOfDate),
            });
        }

        Ok(Presentation {
            presented: true,
            surface_event: None,
        })
    }
}

/// Immutable surface handle stored in [`Surface`].
pub(crate) struct VulkanSurface {
    /// Presented Vulkan surface.
    pub(crate) surface: vk::SurfaceKHR,
}

/// Command and synchronisation objects for one frame in flight.
pub(crate) struct VulkanFrame {
    /// Command pool used to allocate `command_buffer`.
    pub(crate) command_pool: vk::CommandPool,
    /// Primary command buffer recorded each frame.
    pub(crate) command_buffer: vk::CommandBuffer,
    /// Fence signalled when `command_buffer` finishes.
    pub(crate) in_flight: vk::Fence,
    /// Semaphore signalled when a swapchain image is acquired.
    pub(crate) image_available: vk::Semaphore,
}

/// Mutable Vulkan state owned by the middle layer.
pub(crate) struct VulkanState {
    /// Clone of the owning instance guard (handle only, not a separate owner
    /// beyond the reference count). Kept solely so the instance outlives every
    /// device/surface handle in this state.
    pub(crate) _instance_guard: Arc<InstanceGuard>,
    /// Selected physical device.
    pub(crate) physical_device: vk::PhysicalDevice,
    /// Logical device.
    pub(crate) device: ash::Device,
    /// Combined graphics+present queue.
    pub(crate) graphics_queue: vk::Queue,
    /// Queue used for presentation (same as graphics here).
    pub(crate) present_queue: vk::Queue,
    /// Instance-level surface function pointers.
    pub(crate) surface_loader: ash::khr::surface::Instance,
    /// Device-level swapchain function pointers.
    pub(crate) swapchain_loader: ash::khr::swapchain::Device,
    /// The surface this state presents to (also owned by `Surface`).
    pub(crate) surface: vk::SurfaceKHR,
    /// Preferred swapchain format requested when the pipeline was created.
    pub(crate) preferred_format: TextureFormat,
    /// Swapchain, render pass and framebuffers.
    pub(crate) swapchain: swapchain::SwapchainState,
    /// Compiled pipelines and descriptor resources.
    pub(crate) pipelines: pipelines::PipelineSet,
    /// Per-frame command buffers and sync objects.
    pub(crate) frames: Vec<VulkanFrame>,
    /// Fence guarding each swapchain image.
    pub(crate) images_in_flight: Vec<vk::Fence>,
    /// Render-finished semaphore per swapchain image.
    pub(crate) render_finished: Vec<vk::Semaphore>,
    /// Index of the next frame slot.
    pub(crate) frame_in_flight: usize,
    /// GPU textures keyed by [`Texture`] identity.
    pub(crate) textures: HashMap<u64, texture::VulkanTexture>,
    /// Per-format offscreen render passes and their pipelines.
    pub(crate) offscreen: HashMap<TextureFormat, target::OffscreenPass>,
    /// GPU offscreen targets keyed by [`RenderTarget`] identity.
    pub(crate) render_targets: HashMap<u64, target::VulkanRenderTarget>,
    /// Persistent staging buffer reused by every upload.
    pub(crate) staging: texture::StagingBuffer,
    /// Pool and command buffer used for one-time uploads.
    pub(crate) upload_pool: vk::CommandPool,
    /// Persistent upload command buffer.
    pub(crate) upload_command: vk::CommandBuffer,
    /// Fence waited on after each upload (created signalled).
    pub(crate) upload_fence: vk::Fence,
    /// Last reported surface size in physical pixels.
    pub(crate) surface_size: (u32, u32),
    /// Number of frames recorded through this state.
    pub(crate) frame_index: u64,
    /// Whether the swapchain is usable.
    pub(crate) swapchain_ready: bool,
}

impl Drop for VulkanState {
    fn drop(&mut self) {
        // SAFETY: the caller is responsible for dropping the command buffers
        // and state after all GPU work has finished; `device_wait_idle` makes
        // that true here. Handles are destroyed in creation-reverse order and
        // the surface before the device.
        unsafe {
            let _ = self.device.device_wait_idle();
            let descriptor_pool = self.pipelines.descriptor_pool;
            for texture in self.textures.values() {
                texture.destroy(&self.device, descriptor_pool);
            }
            for target in self.render_targets.values() {
                target.destroy(&self.device);
            }
            for pass in self.offscreen.values() {
                pass.destroy(&self.device);
            }
            self.pipelines.destroy(&self.device);
            self.swapchain.destroy(&self.device, &self.swapchain_loader);
            self.swapchain.destroy_render_pass(&self.device);
            for frame in &self.frames {
                destroy_frame(&self.device, frame);
            }
            for semaphore in &self.render_finished {
                self.device.destroy_semaphore(*semaphore, None);
            }
            self.staging.destroy(&self.device);
            self.device.destroy_command_pool(self.upload_pool, None);
            self.device.destroy_fence(self.upload_fence, None);
            self.surface_loader.destroy_surface(self.surface, None);
            self.device.destroy_device(None);
        }
    }
}

/// RAII teardown for the resources created during `begin_with_display`.
///
/// The guard borrows the swapchain and pipelines, which live in the caller's
/// scope until ownership transfers to [`VulkanState`]. It also owns the frames
/// and presentation semaphores created later. Disarming it before the
/// successful return prevents double teardown on `Drop`.
struct BeginTeardown<'a> {
    device: &'a ash::Device,
    surface_loader: &'a ash::khr::surface::Instance,
    swapchain_loader: &'a ash::khr::swapchain::Device,
    surface: vk::SurfaceKHR,
    swapchain: &'a swapchain::SwapchainState,
    pipelines: &'a pipelines::PipelineSet,
    frames: Vec<VulkanFrame>,
    render_finished: Vec<vk::Semaphore>,
    upload: Option<(vk::CommandPool, vk::Fence)>,
    armed: bool,
}

impl Drop for BeginTeardown<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // SAFETY: nothing has been submitted, so no GPU work references these
        // handles. They are destroyed in reverse creation order, and the
        // surface is destroyed before the device.
        unsafe {
            let _ = self.device.device_wait_idle();
            if let Some((pool, fence)) = self.upload.take() {
                self.device.destroy_command_pool(pool, None);
                self.device.destroy_fence(fence, None);
            }
            for frame in &self.frames {
                destroy_frame(self.device, frame);
            }
            for semaphore in &self.render_finished {
                self.device.destroy_semaphore(*semaphore, None);
            }
            self.pipelines.destroy(self.device);
            self.swapchain.destroy(self.device, self.swapchain_loader);
            self.swapchain.destroy_render_pass(self.device);
            self.surface_loader.destroy_surface(self.surface, None);
            self.device.destroy_device(None);
        }
    }
}

/// Returns the framebuffer for a swapchain image, or an error if stale.
fn framebuffers_for(state: &VulkanState, image_index: u32) -> Result<vk::Framebuffer, RenderError> {
    state
        .swapchain
        .framebuffers
        .get(image_index as usize)
        .copied()
        .ok_or_else(|| RenderError::Framebuffer {
            message: format!("no framebuffer for swapchain image {image_index}"),
        })
}

/// Destroys the fences, semaphore and command pool owned by one frame slot.
///
/// Null handles are skipped so a partially built frame can be torn down safely.
fn destroy_frame(device: &ash::Device, frame: &VulkanFrame) {
    // SAFETY: all handles were created by `device`; the caller guarantees the
    // GPU is idle. `destroy_*` accepts null handles as a no-op.
    unsafe {
        if frame.in_flight != vk::Fence::null() {
            device.destroy_fence(frame.in_flight, None);
        }
        if frame.image_available != vk::Semaphore::null() {
            device.destroy_semaphore(frame.image_available, None);
        }
        if frame.command_pool != vk::CommandPool::null() {
            device.destroy_command_pool(frame.command_pool, None);
        }
    }
}

/// Creates the per-frame command pools, buffers, fences and semaphores.
fn create_frames(
    device: &ash::Device,
    queue_family_index: u32,
) -> Result<Vec<VulkanFrame>, RenderError> {
    let mut frames = Vec::with_capacity(MAX_FRAMES_IN_FLIGHT);
    for _ in 0..MAX_FRAMES_IN_FLIGHT {
        match create_frame(device, queue_family_index) {
            Ok(frame) => frames.push(frame),
            Err(error) => {
                // The frames already created are owned here; free them before
                // returning so a late failure does not leak GPU objects.
                for frame in &frames {
                    destroy_frame(device, frame);
                }
                return Err(error);
            }
        }
    }
    Ok(frames)
}

/// Creates one frame slot's command pool, buffer, fence and semaphore.
fn create_frame(device: &ash::Device, queue_family_index: u32) -> Result<VulkanFrame, RenderError> {
    let pool_info = vk::CommandPoolCreateInfo::default()
        .queue_family_index(queue_family_index)
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
    // SAFETY: `device` is live and the pool info is valid.
    let command_pool =
        unsafe { device.create_command_pool(&pool_info, None) }.map_err(|error| {
            RenderError::Unsupported {
                message: format!("failed to create a frame command pool: {error}"),
            }
        })?;

    let allocate_info = vk::CommandBufferAllocateInfo::default()
        .command_pool(command_pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the pool was just created.
    let command_buffers = unsafe { device.allocate_command_buffers(&allocate_info) };
    let command_buffer = match command_buffers {
        Ok(mut buffers) if !buffers.is_empty() => buffers.remove(0),
        Ok(_) => {
            // SAFETY: the pool is owned here; nothing uses it.
            unsafe { device.destroy_command_pool(command_pool, None) };
            return Err(RenderError::OutOfMemory {
                message: "the command pool returned no command buffer".to_owned(),
            });
        }
        Err(error) => {
            // SAFETY: the pool is owned here.
            unsafe { device.destroy_command_pool(command_pool, None) };
            return Err(RenderError::OutOfMemory {
                message: format!("failed to allocate a frame command buffer: {error}"),
            });
        }
    };

    // SAFETY: `device` is live.
    let fence = unsafe {
        device.create_fence(
            &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
            None,
        )
    };
    let image_available =
        unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) };
    match (fence, image_available) {
        (Ok(in_flight), Ok(image_available)) => Ok(VulkanFrame {
            command_pool,
            command_buffer,
            in_flight,
            image_available,
        }),
        (fence, image_available) => {
            if let Ok(fence) = fence {
                // SAFETY: owned here.
                unsafe { device.destroy_fence(fence, None) };
            }
            if let Ok(semaphore) = image_available {
                // SAFETY: owned here.
                unsafe { device.destroy_semaphore(semaphore, None) };
            }
            // SAFETY: owned here.
            unsafe { device.destroy_command_pool(command_pool, None) };
            Err(RenderError::Unsupported {
                message: "failed to create frame synchronisation objects".to_owned(),
            })
        }
    }
}

/// Creates the reusable upload command buffer and a signalled fence.
fn create_upload_resources(
    device: &ash::Device,
    queue_family_index: u32,
) -> Result<(vk::CommandPool, vk::CommandBuffer, vk::Fence), RenderError> {
    let pool_info = vk::CommandPoolCreateInfo::default()
        .queue_family_index(queue_family_index)
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
    // SAFETY: `device` is live.
    let pool = unsafe { device.create_command_pool(&pool_info, None) }.map_err(|error| {
        RenderError::Unsupported {
            message: format!("failed to create the upload command pool: {error}"),
        }
    })?;

    let allocate_info = vk::CommandBufferAllocateInfo::default()
        .command_pool(pool)
        .level(vk::CommandBufferLevel::PRIMARY)
        .command_buffer_count(1);
    // SAFETY: the pool was just created.
    let command_buffer = match unsafe { device.allocate_command_buffers(&allocate_info) } {
        Ok(mut buffers) if !buffers.is_empty() => buffers.remove(0),
        Ok(_) => {
            // SAFETY: owned here.
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(RenderError::OutOfMemory {
                message: "the upload pool returned no command buffer".to_owned(),
            });
        }
        Err(error) => {
            // SAFETY: owned here.
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(RenderError::OutOfMemory {
                message: format!("failed to allocate the upload command buffer: {error}"),
            });
        }
    };

    // SAFETY: `device` is live.
    let fence = unsafe {
        device.create_fence(
            &vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED),
            None,
        )
    }
    .map_err(|error| {
        // SAFETY: owned here.
        unsafe { device.destroy_command_pool(pool, None) };
        RenderError::Unsupported {
            message: format!("failed to create the upload fence: {error}"),
        }
    })?;

    Ok((pool, command_buffer, fence))
}

/// Recreates the swapchain and its per-image synchronisation objects.
///
/// The old swapchain is passed as a hint to the driver and destroyed only
/// after the replacement is built. If the rebuild fails the previous state is
/// kept (with `swapchain_ready` cleared) so a later resize can retry without
/// losing the pinned render pass.
fn recreate_swapchain(
    device: &ash::Device,
    state: &mut VulkanState,
    surface: vk::SurfaceKHR,
) -> Result<(), RenderError> {
    // SAFETY: the device is idle before any swapchain resource is touched.
    unsafe { device.device_wait_idle() }.map_err(|error| {
        device::result_error(
            "failed to idle the device before recreating the swapchain",
            error,
        )
    })?;

    let requested = state.surface_size;
    let new_swapchain = swapchain::create(
        device,
        state.physical_device,
        &state.surface_loader,
        &state.swapchain_loader,
        surface,
        requested,
        state.preferred_format,
        Some(&state.swapchain),
    );

    match new_swapchain {
        Ok(new_swapchain) => {
            let old = std::mem::replace(&mut state.swapchain, new_swapchain);
            old.destroy(device, &state.swapchain_loader);
            if let Err(error) = rebuild_image_sync(state, device) {
                state.swapchain_ready = false;
                return Err(error);
            }
            state.swapchain_ready = true;
            Ok(())
        }
        Err(error) => {
            // Keep the previous render pass and format for a later retry.
            state.swapchain_ready = false;
            Err(error)
        }
    }
}

/// Rebuilds the per-image fences and semaphores after a swapchain change.
///
/// The presentation semaphores are always recreated: a `queue_present` on the
/// previous generation may have signalled one, and a binary semaphore that is
/// already signalled must not be signalled again. The device is idle here, so
/// destroying and recreating them is safe.
fn rebuild_image_sync(state: &mut VulkanState, device: &ash::Device) -> Result<(), RenderError> {
    let image_count = state.swapchain.images.len().max(1);
    for semaphore in state.render_finished.drain(..) {
        // SAFETY: owned here and the GPU is idle.
        unsafe { device.destroy_semaphore(semaphore, None) };
    }
    for _ in 0..image_count {
        // SAFETY: `device` is live.
        let semaphore =
            unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.map_err(
                |error| RenderError::Unsupported {
                    message: format!("failed to create presentation semaphores: {error}"),
                },
            )?;
        state.render_finished.push(semaphore);
    }
    state.images_in_flight = vec![vk::Fence::null(); image_count];
    Ok(())
}

/// Recreates a frame's `image_available` semaphore after an error unwound
/// between `acquire_next_image` and a successful submit.
///
/// The acquire signalled the old semaphore, so it can no longer be reused.
/// Destroying it is safe because no queue operation is waiting on it. If the
/// replacement cannot be created the swapchain is marked unusable so the slot
/// is never touched again.
fn recreate_acquired_semaphore(device: &ash::Device, state: &mut VulkanState, frame_index: usize) {
    let recreated = {
        let Some(frame) = state.frames.get_mut(frame_index) else {
            return;
        };
        // SAFETY: the acquire signalled this semaphore and nothing waits on it.
        unsafe {
            device.destroy_semaphore(frame.image_available, None);
            match device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) {
                Ok(semaphore) => {
                    frame.image_available = semaphore;
                    true
                }
                Err(_) => {
                    frame.image_available = vk::Semaphore::null();
                    false
                }
            }
        }
    };
    if !recreated {
        state.swapchain_ready = false;
    }
}

/// Derives a platform display handle from a window handle when the platform
/// does not need connection data.
fn display_for_window(window: &RawWindowHandle) -> Option<RawDisplayHandle> {
    match window {
        RawWindowHandle::AndroidNdk(_) => {
            Some(RawDisplayHandle::Android(AndroidDisplayHandle::new()))
        }
        RawWindowHandle::Win32(_) => Some(RawDisplayHandle::Windows(WindowsDisplayHandle::new())),
        _ => None,
    }
}

/// Returns `true` when `format` applies the sRGB transfer function on write.
pub(crate) fn format_is_srgb(format: vk::Format) -> bool {
    matches!(
        format,
        vk::Format::B8G8R8A8_SRGB
            | vk::Format::R8G8B8A8_SRGB
            | vk::Format::A8B8G8R8_SRGB_PACK32
            | vk::Format::B8G8R8_SRGB
            | vk::Format::R8G8B8_SRGB
    )
}

/// Converts the scene background into a render-pass clear color.
pub(crate) fn clear_value(background: Option<[f32; 4]>, srgb: bool) -> [f32; 4] {
    let [r, g, b, a] = background.unwrap_or([0.0, 0.0, 0.0, 1.0]);
    if srgb {
        [
            draw::srgb_to_linear(r),
            draw::srgb_to_linear(g),
            draw::srgb_to_linear(b),
            a,
        ]
    } else {
        [r, g, b, a]
    }
}

/// Validates an offscreen render-target descriptor against the device limit.
fn validate_target_desc(desc: &RenderTargetDesc, max_dimension: u32) -> Result<(), RenderError> {
    if desc.is_empty() {
        return Err(RenderError::InvalidArgument {
            message: "render-target dimensions must be non-zero".to_owned(),
        });
    }
    if desc.width > max_dimension || desc.height > max_dimension {
        return Err(RenderError::InvalidArgument {
            message: format!("render-target dimensions exceed the {max_dimension} pixel limit"),
        });
    }
    if !desc.format.has_alpha() {
        return Err(RenderError::InvalidArgument {
            message: "an offscreen render target must use an alpha-capable RGBA8 format".to_owned(),
        });
    }
    Ok(())
}

/// Validates a readback region and output length against `desc`.
fn validate_readback(
    desc: &RenderTargetDesc,
    region: PixelRegion,
    len: usize,
) -> Result<(), RenderError> {
    if region.is_empty() {
        return Err(RenderError::Framebuffer {
            message: "readback region is empty".to_owned(),
        });
    }
    if region.right() > desc.width || region.bottom() > desc.height {
        return Err(RenderError::Framebuffer {
            message: format!(
                "readback region {}x{}+{}+{} exceeds the {}x{} target",
                region.width, region.height, region.x, region.y, desc.width, desc.height
            ),
        });
    }
    let expected = region.area() as usize * desc.format.bytes_per_pixel() as usize;
    if len != expected {
        return Err(RenderError::Framebuffer {
            message: format!("readback buffer is {len} bytes, expected {expected}"),
        });
    }
    Ok(())
}

/// Validates a texture descriptor against the device limit.
fn validate_desc(desc: &TextureDesc, max_dimension: u32) -> Result<(), RenderError> {
    if desc.is_empty() {
        return Err(RenderError::InvalidArgument {
            message: "texture dimensions must be non-zero".to_owned(),
        });
    }
    if desc.mip_levels == 0 {
        return Err(RenderError::InvalidArgument {
            message: "a texture must have at least one mip level".to_owned(),
        });
    }
    if desc.width > max_dimension || desc.height > max_dimension {
        return Err(RenderError::InvalidArgument {
            message: format!("texture dimensions exceed the {max_dimension} pixel limit"),
        });
    }
    Ok(())
}

/// Validates an upload region and pixel length.
fn validate_upload(
    desc: &TextureDesc,
    region: PixelRegion,
    pixels: &[u8],
) -> Result<(), RenderError> {
    if region.is_empty() {
        return Err(RenderError::Upload {
            message: "upload region is empty".to_owned(),
        });
    }
    if region.right() > desc.width || region.bottom() > desc.height {
        return Err(RenderError::Upload {
            message: format!(
                "upload region {}x{}+{}+{} exceeds the {}x{} texture",
                region.width, region.height, region.x, region.y, desc.width, desc.height
            ),
        });
    }
    let expected = (region.area() as usize)
        .checked_mul(desc.format.bytes_per_pixel() as usize)
        .ok_or_else(|| RenderError::Upload {
            message: "upload size overflows the host address space".to_owned(),
        })?;
    if pixels.len() != expected {
        return Err(RenderError::Upload {
            message: format!(
                "upload provided {} bytes, expected {expected}",
                pixels.len()
            ),
        });
    }
    Ok(())
}
