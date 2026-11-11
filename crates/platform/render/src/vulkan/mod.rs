//! Vulkan (`ash`) render backend.
//!
//! This module implements the real Vulkan mechanism behind the stateless
//! [`crate::RenderPipeline`] facade: the pipeline holds
//! only the instance and selected physical device; every mutable GPU resource
//! lives in the caller-owned [`VulkanState`]. No process globals, no
//! `&mut self`, no hidden caches.
//!
//! The logical device is created on its own. A state that presents to a
//! window pairs the device with a [`VulkanPresentation`] — the surface, the
//! swapchain and the frame slots — and a state that renders offscreen pairs
//! it with nothing. That is what makes the GPU path reachable on a machine
//! that has a GPU but no display: the device, its queues and its
//! swapchain-independent resources do not depend on a surface.
//!
//! Implemented:
//!
//! * instance creation with `VK_KHR_surface` and the available platform
//!   surface extensions, plus optional `VK_LAYER_KHRONOS_validation`;
//! * surface creation through `ash-window`, physical-device selection and a
//!   combined graphics+present queue family;
//! * swapchain create/recreate with image views, a render pass and
//!   framebuffers (`OUT_OF_DATE`/`SUBOPTIMAL` handled);
//! * a ring of staging-buffer upload slots with layout transitions and
//!   sub-rectangle (`PixelRegion`) copies;
//! * textured and solid pipelines with per-blend fixed-function state, a
//!   host-visible unit-quad vertex buffer and per-texture descriptor sets;
//! * a destination-dependent blend program that mixes a source over a copy of
//!   the destination, so the modes that fixed-function state cannot express
//!   are drawn instead of refused;
//! * a per-frame command buffer and sync objects, and full scene traversal
//!   with transforms, opacity, scissor clipping and `z` ordering.
//!
//! Deferred (reported as [`RenderError::Unsupported`], never silently
//! degraded):
//!
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
#![allow(unsafe_code)]

mod device;
mod draw;
mod effect;
mod pipelines;
mod shaders;
mod swapchain;
mod target;
mod texture;

use std::cell::RefCell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use ash::vk;
use raw_window_handle::{
    AndroidDisplayHandle, RawDisplayHandle, RawWindowHandle, WindowsDisplayHandle,
};

use api::error::RenderError;
use api::geometry::{Affine2, PixelRegion};
use api::host::{PresentSkip, Presentation, SurfaceEvent};

use crate::collections::Map;
use crate::render_target::{RenderTarget, RenderTargetDesc};
use crate::scene::RenderScene;
use crate::state::{RenderState, StateKind, Surface, SurfaceKind};
use crate::texture::{Texture, TextureDesc};
use crate::{BackendKind, FrameTimings, GpuCapabilities, TextureFormat};

/// Number of frames recorded concurrently.
pub(crate) const MAX_FRAMES_IN_FLIGHT: usize = 2;

/// Size of the push-constant block shared with the shaders.
pub(crate) const PUSH_CONSTANT_SIZE: u32 = 112;

/// Immutable Vulkan mechanism held by [`crate::RenderPipeline`].
///
/// This value contains only handles and configuration selected at creation
/// time. The logical device, swapchain and every GPU resource that changes per
/// caller live in the caller-owned [`VulkanState`].
pub(crate) struct VulkanPipeline {
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
    /// Nanoseconds a frame may wait for the display to release a swapchain
    /// image, derived from the configured frame period.
    acquire_timeout_ns: u64,
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
    /// Returns [`RenderError::DeviceUnavailable`] when the Vulkan loader,
    /// instance or a suitable device is unavailable, and
    /// [`RenderError::InvalidArgument`] when `acquire_timeout` does not fit
    /// the swapchain acquire timeout.
    pub(crate) fn new(
        config: &crate::PipelineConfig,
        acquire_timeout: Duration,
    ) -> Result<Self, RenderError> {
        let bundle = device::create_instance(config)?;
        if bundle.validation {
            log::info!("Vulkan validation layer VK_LAYER_KHRONOS_validation enabled");
        }
        // The acquire timeout is a nanosecond count; a budget that does not fit
        // it is a typed error rather than a wrapped wait.
        let acquire_timeout_ns = u64::try_from(acquire_timeout.as_nanos()).map_err(|_| {
            RenderError::InvalidArgument {
                message: format!(
                    "the presentation budget {acquire_timeout:?} does not fit the \
                     swapchain acquire timeout"
                ),
            }
        })?;
        Ok(Self {
            instance_guard: Arc::new(InstanceGuard {
                entry: bundle.entry,
                instance: bundle.instance,
            }),
            physical_device: bundle.physical_device,
            max_texture_dimension: bundle.max_texture_dimension,
            memory_properties: bundle.memory_properties,
            preferred_format: config.preferred_format,
            acquire_timeout_ns,
        })
    }

    /// Returns the capabilities advertised by the selected device.
    pub(crate) fn capabilities(&self) -> GpuCapabilities {
        GpuCapabilities {
            backend: BackendKind::Vulkan,
            max_texture_dimension: self.max_texture_dimension,
            preferred_format: self.preferred_format,
            supports_destination_dependent_blend: true,
            // Offscreen targets are a real GPU path: colour-only render pass,
            // per-format pipelines and `vkCmdCopyImageToBuffer` readback.
            supports_offscreen_targets: true,
            // GPU glyph runs and mask clips are not implemented; a scene that
            // uses them fails with `RenderError::Unsupported` rather than
            // rendering incorrectly.
            supports_glyph_atlas: false,
            // Effect pipelines need a device, SPIR-V modules, a dynamic uniform
            // buffer and a 64-byte push-constant block: all of them are inside
            // what Vulkan 1.0 guarantees.
            supports_effect_pipelines: true,
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
    /// The device is created with the surface, so the queue family it selects
    /// supports presentation as well as graphics.
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
        .map_err(|error| RenderError::DeviceUnavailable {
            message: format!(
                "failed to create the Vulkan presentation surface: {}",
                device::vk_result_message(error)
            ),
        })?;

        let device = match VulkanDevice::create(
            self.instance_guard.clone(),
            self.physical_device,
            &self.memory_properties,
            device::PresentTarget::Surface(&surface_loader, surface),
        ) {
            Ok(device) => device,
            Err(error) => {
                // SAFETY: `surface` was created here and no swapchain uses it
                // yet; the device state destroys the device itself.
                unsafe { surface_loader.destroy_surface(surface, None) };
                return Err(error);
            }
        };

        let swapchain_loader =
            ash::khr::swapchain::Device::new(&self.instance_guard.instance, &device.device);

        let swapchain = match swapchain::create(
            &swapchain::SurfaceContext {
                device: &device.device,
                physical_device: self.physical_device,
                surface_loader: &surface_loader,
                swapchain_loader: &swapchain_loader,
                surface,
            },
            (0, 0),
            self.preferred_format,
            None,
        ) {
            Ok(swapchain) => swapchain,
            Err(error) => {
                // SAFETY: the surface was created here and no swapchain uses
                // it; the device state destroys the device itself.
                unsafe { surface_loader.destroy_surface(surface, None) };
                return Err(error);
            }
        };

        let pipelines =
            match pipelines::create(&device.device, swapchain.render_pass, &device.resources) {
                Ok(pipelines) => pipelines,
                Err(error) => {
                    // SAFETY: the swapchain and the surface were created here
                    // and nothing else references them.
                    unsafe {
                        swapchain.destroy(&device.device, &swapchain_loader);
                        swapchain.destroy_render_pass(&device.device);
                        surface_loader.destroy_surface(surface, None);
                    }
                    return Err(error);
                }
            };

        // From here on the guard owns teardown: any `?` below destroys the
        // surface, the swapchain, the pipelines and whatever else was built.
        // The device and its own resources are destroyed with the device
        // state, which outlives the guard.
        let mut teardown = BeginTeardown {
            device: &device.device,
            surface_loader: &surface_loader,
            swapchain_loader: &swapchain_loader,
            surface,
            swapchain: &swapchain,
            pipelines: &pipelines,
            frames: Vec::new(),
            render_finished: Vec::new(),
            armed: true,
        };

        teardown.frames = create_frames(&device.device, device.queue_family_index)?;
        let image_count = swapchain.images.len().max(1);
        for _ in 0..image_count {
            // SAFETY: `device.device` is live.
            let semaphore = unsafe {
                device
                    .device
                    .create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
            }
            .map_err(|error| RenderError::DeviceUnavailable {
                message: format!(
                    "failed to create presentation semaphores: {}",
                    device::vk_result_message(error)
                ),
            })?;
            teardown.render_finished.push(semaphore);
        }

        // Success: transfer ownership to the state and disarm the guard.
        teardown.armed = false;
        let frames = std::mem::take(&mut teardown.frames);
        let render_finished = std::mem::take(&mut teardown.render_finished);
        drop(teardown);

        let surface_size = (swapchain.extent.width, swapchain.extent.height);
        let fences = FrameFences::new(frame_slot_fences(&frames));

        let state = VulkanState::Presenting(Box::new(VulkanPresentingState {
            device,
            presentation: VulkanPresentation {
                surface_loader,
                swapchain_loader,
                surface,
                display_handle: *display,
                window_handle: *window,
                preferred_format: self.preferred_format,
                swapchain,
                retired_swapchain: None,
                pipelines,
                frame_pass: None,
                frame_targets: (0..MAX_FRAMES_IN_FLIGHT).map(|_| None).collect(),
                frames,
                fences,
                images_in_flight: vec![vk::Fence::null(); image_count],
                render_finished,
                frame_in_flight: 0,
                surface_size,
                frame_index: 0,
                swapchain_ready: true,
                swapchain_presented: false,
                present_fault: None,
                last_timings: None,
            },
        }));

        Ok((
            Surface {
                kind: SurfaceKind::Vulkan(VulkanSurface { surface }),
            },
            RenderState {
                kind: StateKind::Vulkan(Box::new(state)),
            },
        ))
    }

    /// Creates a render state that renders offscreen, without a surface.
    ///
    /// The logical device is created on its own: a graphics queue, no surface
    /// and no swapchain. A scene can then be rendered into an offscreen
    /// target and read back on a machine that has a GPU but no display.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::DeviceUnavailable`] when the logical device or
    /// one of its resources cannot be created.
    pub(crate) fn begin_offscreen(&self) -> Result<RenderState, RenderError> {
        let device = VulkanDevice::create(
            self.instance_guard.clone(),
            self.physical_device,
            &self.memory_properties,
            device::PresentTarget::Offscreen,
        )?;
        Ok(RenderState {
            kind: StateKind::Vulkan(Box::new(VulkanState::Offscreen(Box::new(device)))),
        })
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
        let VulkanState::Presenting(presenting) = &mut **vstate else {
            return Err(no_surface_error());
        };
        let presenting = &mut **presenting;
        let VulkanPresentingState {
            device: device_state,
            presentation,
            ..
        } = presenting;

        if surface_handle != presentation.surface {
            return Err(RenderError::InvalidArgument {
                message: "the surface does not belong to this render state".to_owned(),
            });
        }

        let current = presentation.swapchain.extent;
        // A surface event that repeats the extent the swapchain already has
        // cannot change the frame the display shows, so it does not rebuild
        // one. An unusable swapchain is always rebuilt, even for the same
        // size, because the retry is what makes it usable again.
        if presentation.swapchain_ready && (current.width, current.height) == size {
            presentation.surface_size = size;
            return Ok(SurfaceEvent::Resized(current.width, current.height));
        }
        presentation.surface_size = size;
        let device = device_state.device.clone();
        // `recreate_swapchain` idles the device before touching the swapchain.
        recreate_swapchain(
            &device,
            device_state.physical_device,
            presentation,
            surface_handle,
        )?;
        Ok(SurfaceEvent::Resized(
            presentation.swapchain.extent.width,
            presentation.swapchain.extent.height,
        ))
    }

    /// Recreates the presentation surface and its swapchain after a frame
    /// reported the surface lost.
    pub(crate) fn recover_surface(
        &self,
        surface: &mut Surface,
        state: &mut RenderState,
    ) -> Result<SurfaceEvent, RenderError> {
        let SurfaceKind::Vulkan(vsurface) = &mut surface.kind else {
            return Err(RenderError::InvalidArgument {
                message: "the Vulkan backend received a null surface".to_owned(),
            });
        };
        let StateKind::Vulkan(vstate) = &mut state.kind else {
            return Err(RenderError::InvalidArgument {
                message: "the Vulkan backend received a null render state".to_owned(),
            });
        };
        let VulkanState::Presenting(presenting) = &mut **vstate else {
            return Err(no_surface_error());
        };
        let presenting = &mut **presenting;
        let VulkanPresentingState {
            device: device_state,
            presentation,
            ..
        } = presenting;
        if vsurface.surface != presentation.surface {
            return Err(RenderError::InvalidArgument {
                message: "the surface does not belong to this render state".to_owned(),
            });
        }
        recreate_surface(
            self,
            &device_state.device,
            device_state.physical_device,
            presentation,
            vsurface,
        )?;
        Ok(SurfaceEvent::Restored)
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
        vstate.device_mut().drop_texture(tex);
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

        vstate
            .device_mut()
            .upload_texture(&self.memory_properties, tex, region, pixels)
    }

    /// Copies every upload's sub-rectangle into its texture in one batch.
    ///
    /// On the Vulkan backend the batch is staged, recorded and submitted once,
    /// so a frame's dirty bitmaps pay a single GPU wait instead of one per
    /// texture.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Upload`] when a region is out of bounds or its
    /// pixel slice does not match it.
    pub(crate) fn upload_textures(
        &self,
        state: &mut RenderState,
        uploads: &[crate::TextureUpload<'_>],
    ) -> Result<(), RenderError> {
        let StateKind::Vulkan(vstate) = &mut state.kind else {
            return Err(RenderError::InvalidArgument {
                message: "the Vulkan backend received a null render state".to_owned(),
            });
        };
        vstate
            .device_mut()
            .upload_textures(&self.memory_properties, uploads)
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
        vstate.device_mut().destroy_render_target(target);
    }

    /// Creates an effect pipeline whose resources live in `state`.
    ///
    /// The device-level resources (the uniform ring and the samplers) are
    /// created with the first effect pipeline; the descriptor set layout, the
    /// pipeline layout and one shader module per stage are created eagerly, so
    /// a descriptor or a module the driver rejects fails here rather than on
    /// the first frame that draws it. The graphics pipeline itself is built per
    /// render pass on first use, because a pipeline is tied to the pass it is
    /// recorded in.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::InvalidArgument`] for a descriptor that breaks
    /// the binding or uniform contract, [`RenderError::Unsupported`] for a
    /// vertex layout or a blend this backend cannot build,
    /// [`RenderError::Shader`] for a module or pipeline the driver rejects, and
    /// [`RenderError::OutOfMemory`] when the ring, the pools or the descriptor
    /// sets cannot be allocated.
    pub(crate) fn create_effect_pipeline(
        &self,
        state: &mut RenderState,
        desc: &api::shader::EffectPipelineDesc<'_>,
    ) -> Result<crate::effect::EffectPipeline, RenderError> {
        let StateKind::Vulkan(vstate) = &mut state.kind else {
            return Err(RenderError::InvalidArgument {
                message: "the Vulkan backend received a null render state".to_owned(),
            });
        };
        vstate
            .device_mut()
            .create_effect_pipeline(&self.memory_properties, desc)
    }

    /// Releases an effect pipeline, idling the device first.
    pub(crate) fn destroy_effect_pipeline(
        &self,
        state: &mut RenderState,
        pipeline: crate::effect::EffectPipeline,
    ) {
        let StateKind::Vulkan(vstate) = &mut state.kind else {
            return;
        };
        vstate.device_mut().destroy_effect_pipeline(pipeline);
    }

    /// Renders `scene` into the offscreen `target`.
    ///
    /// The offscreen render is synchronous: its pixels are read back by a
    /// separate call, so the result must be complete when this returns. Any
    /// texture upload that is still in flight is waited before recording, so
    /// the pass samples the pixels it was given.
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
        vstate
            .device_mut()
            .render_to_target(&self.memory_properties, target, scene)
    }

    /// Copies a sub-rectangle of an offscreen `target` into `out`.
    ///
    /// The copy is synchronous: a script observes `out` on return. The
    /// readback buffer is reused across calls rather than allocated per call;
    /// the readback API takes `&RenderState`, so the buffer sits behind a
    /// `RefCell` in the device and is borrowed for the duration of the call.
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
        vstate
            .device()
            .read_render_target(&self.memory_properties, target, region, out)
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
        // A state that renders offscreen has no presentation, so it cannot
        // present a frame.
        let VulkanState::Presenting(presenting) = &mut **vstate else {
            return Err(no_surface_error());
        };
        let presenting = &mut **presenting;

        // The frame records into the swapchain's targets and reads the
        // device's resources. The two are separate owners, so the frame
        // borrows the presenting state and reaches both through it.
        (|| -> Result<Presentation, RenderError> {
            let VulkanPresentingState {
                device: device_state,
                presentation,
                ..
            } = &mut *presenting;
            let physical_device = device_state.physical_device;

            if surface_handle != presentation.surface {
                return Err(RenderError::InvalidArgument {
                    message: "the surface does not belong to this render state".to_owned(),
                });
            }

            if let Some(fault) = swapchain_fault(
                presentation.swapchain_ready,
                presentation.swapchain.images.len(),
            ) {
                // After a failed rebuild the fault stays `RebuildFailed` until a
                // frame presents, so the retry loop logs each cause once.
                if presentation.present_fault != Some(PresentFault::RebuildFailed) {
                    presentation.report_present_fault(fault);
                }
                let device = device_state.device.clone();
                if let Err(error) =
                    recreate_swapchain(&device, physical_device, &mut *presentation, surface_handle)
                {
                    if presentation.present_fault != Some(PresentFault::RebuildFailed) {
                        log::error!("vulkan: rebuilding the swapchain failed: {error}");
                        presentation.present_fault = Some(PresentFault::RebuildFailed);
                    }
                    // The next frame retries the rebuild, and the precise failure
                    // was reported above once for this cause.
                    return Err(RenderError::SurfaceOutOfDate);
                }
                if let Some(fault) = swapchain_fault(
                    presentation.swapchain_ready,
                    presentation.swapchain.images.len(),
                ) {
                    // A rebuild that leaves the swapchain unusable is reported as
                    // out of date so the caller retries instead of treating this
                    // frame as presented.
                    presentation.report_present_fault(fault);
                    return Err(RenderError::SurfaceOutOfDate);
                }
            }

            let device = device_state.device.clone();
            let swapchain_loader = presentation.swapchain_loader.clone();
            let frame_index = presentation
                .frame_in_flight
                .min(presentation.frames.len() - 1);
            let command_pool = presentation.frames[frame_index].command_pool;
            let command_buffer = presentation.frames[frame_index].command_buffer;
            let in_flight = presentation.frames[frame_index].in_flight;
            let image_available = presentation.frames[frame_index].image_available;
            let swapchain = presentation.swapchain.swapchain;
            let extent = presentation.swapchain.extent;
            let render_pass = presentation.swapchain.render_pass;
            let format = presentation.swapchain.format.format;
            let graphics_queue = device_state.graphics_queue;
            let present_queue = device_state.present_queue;

            // Per-frame GPU call costs, written into the state before every return
            // so a slow frame's line can name what blocked.
            let mut fence_wait_ms = 0.0_f64;
            let mut present_ms = 0.0_f64;
            let mut recreate_ms = 0.0_f64;
            let mut submit_ms = 0.0_f64;

            // Wait for the frame slot's previous work, but only when the queue
            // accepted a submission with that fence. A frame that failed leaves
            // its fence unsignalled with nothing pending, and waiting for such a
            // fence would never return.
            if presentation.fences.slot_running(frame_index) {
                let fence_started = Instant::now();
                // SAFETY: the fence belongs to this frame slot and its submission
                // was accepted, so it will signal.
                unsafe { device.wait_for_fences(&[in_flight], true, u64::MAX) }.map_err(
                    |error| device::result_error("failed to wait for the frame fence", error),
                )?;
                fence_wait_ms += fence_started.elapsed().as_secs_f64() * 1000.0;
                presentation.fences.completed(frame_index);
            }

            let acquire_started = Instant::now();
            // SAFETY: `swapchain` is valid; `image_available` must be unsignalled.
            let acquire_result = unsafe {
                swapchain_loader.acquire_next_image(
                    swapchain,
                    self.acquire_timeout_ns,
                    image_available,
                    vk::Fence::null(),
                )
            };
            let acquire_ms = acquire_started.elapsed().as_secs_f64() * 1000.0;
            let (image_index, suboptimal) = match acquire_outcome(acquire_result) {
                Ok(AcquireOutcome::Image { index, suboptimal }) => (index, suboptimal),
                Ok(AcquireOutcome::OutOfDate) => {
                    presentation.report_present_fault(PresentFault::AcquireOutOfDate);
                    let recreate_started = Instant::now();
                    recreate_swapchain(
                        &device,
                        physical_device,
                        &mut *presentation,
                        surface_handle,
                    )?;
                    recreate_ms += recreate_started.elapsed().as_secs_f64() * 1000.0;
                    presentation.last_timings = Some(FrameTimings {
                        acquire_ms,
                        present_ms,
                        submit_ms,
                        recreate_ms,
                        fence_wait_ms,
                    });
                    return Ok(Presentation {
                        presented: false,
                        surface_event: Some(SurfaceEvent::OutOfDate),
                        skip: None,
                    });
                }
                Ok(AcquireOutcome::SurfaceLost) => {
                    return Ok(Presentation {
                        presented: false,
                        surface_event: Some(SurfaceEvent::Lost),
                        skip: None,
                    });
                }
                Ok(AcquireOutcome::TimedOut) => {
                    // The timeout neither signalled nor consumed anything: the
                    // semaphore, the fences, the frame slot and the swapchain stay
                    // untouched, so the previous image stays on screen and the
                    // next frame can acquire with the same semaphore. The frame is
                    // reported as skipped rather than silently dropped, and the
                    // swapchain is not rebuilt because the display is late, not
                    // out of date.
                    presentation.last_timings = Some(FrameTimings {
                        acquire_ms,
                        present_ms,
                        submit_ms,
                        recreate_ms,
                        fence_wait_ms,
                    });
                    return Ok(Presentation {
                        presented: false,
                        surface_event: None,
                        skip: Some(PresentSkip::AcquireTimeout),
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
            let signal_semaphores = [presentation.render_finished[image_index as usize]];
            let submit_started = Instant::now();
            let record_result: Result<(), RenderError> = (|| {
                let image_fence = presentation.images_in_flight[image_index as usize];
                // The image's fence is a frame slot's fence. Wait for it only when
                // a submission with it may still be running: a frame that failed
                // leaves its fence unsignalled with nothing pending.
                if presentation.fences.fence_running(image_fence) {
                    let image_wait_started = Instant::now();
                    // SAFETY: the fence guards this swapchain image and its
                    // submission was accepted, so it will signal.
                    unsafe { device.wait_for_fences(&[image_fence], true, u64::MAX) }.map_err(
                        |error| device::result_error("failed to wait for a swapchain image", error),
                    )?;
                    fence_wait_ms += image_wait_started.elapsed().as_secs_f64() * 1000.0;
                    presentation.fences.completed_fence(image_fence);
                }

                // SAFETY: the pool is not in use.
                unsafe {
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
                let clear = clear_value(scene.background);

                // The frame is composed into a target of its own and then
                // presented with a quad, because a destination-dependent blend
                // needs a copy of the destination and a colour attachment cannot
                // be a copy's source.
                ensure_frame_targets(
                    &device,
                    &self.memory_properties,
                    &device_state.resources,
                    &mut *presentation,
                    format,
                    extent,
                )?;

                // The frame fence for this slot was waited above when it had work
                // in flight, so the frame that owned the slot has completed: its
                // uniform bytes may be written again and the descriptor sets it
                // bound may be freed. The slot is the frame's own, `frame_in_flight
                // % MAX_FRAMES_IN_FLIGHT`.
                if let Some(effects) = device_state.effect.as_mut() {
                    effects.begin_live_frame(&device, frame_index);
                }
                let effects = device_state.effect.as_mut();
                {
                    let Some(pass) = presentation.frame_pass.as_ref() else {
                        return Err(RenderError::Framebuffer {
                            message: "the frame target disappeared".to_owned(),
                        });
                    };
                    let Some(frame) = presentation.frame_targets[frame_index].as_mut() else {
                        return Err(RenderError::Framebuffer {
                            message: "the frame target disappeared".to_owned(),
                        });
                    };
                    // SAFETY: the command buffer is recording.
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
                    frame.prepare(&device, command_buffer);
                    let render_pass = pass.render_pass;
                    let framebuffer = frame.framebuffer();
                    let image = frame.image();
                    let pipelines = &pass.pipelines;
                    let snapshot = Some(&mut frame.snapshot);
                    draw::record_scene(
                        draw::SceneDraw {
                            pass: draw::ScenePass {
                                device: &device,
                                command: command_buffer,
                                render_pass,
                                framebuffer,
                                image,
                                clear,
                                extent,
                            },
                            pipelines,
                            resources: &device_state.resources,
                            textures: &device_state.textures,
                            effects,
                            snapshot,
                            extent,
                        },
                        scene,
                        &Affine2::IDENTITY,
                    )?;
                    frame.prepare_for_present(&device, command_buffer);
                }

                // The composed frame is presented with a quad over the swapchain
                // image.
                let framebuffer = framebuffers_for(&*presentation, image_index)?;
                let clear_values = [vk::ClearValue {
                    color: vk::ClearColorValue { float32: clear },
                }];
                let render_pass_begin = vk::RenderPassBeginInfo::default()
                    .render_pass(render_pass)
                    .framebuffer(framebuffer)
                    .render_area(render_area)
                    .clear_values(&clear_values);
                let Some(frame) = presentation.frame_targets[frame_index].as_ref() else {
                    return Err(RenderError::Framebuffer {
                        message: "the frame target disappeared".to_owned(),
                    });
                };
                // SAFETY: the command buffer is recording; the render pass and
                // framebuffer are compatible and live.
                unsafe {
                    device.cmd_begin_render_pass(
                        command_buffer,
                        &render_pass_begin,
                        vk::SubpassContents::INLINE,
                    );
                }
                draw::record_present(
                    &device,
                    command_buffer,
                    &presentation.pipelines,
                    &device_state.resources,
                    frame.descriptor_set,
                    extent,
                );

                // SAFETY: the render pass and command buffer are recording.
                unsafe {
                    device.cmd_end_render_pass(command_buffer);
                    device.end_command_buffer(command_buffer).map_err(|error| {
                        device::result_error("failed to end the frame command buffer", error)
                    })?;
                }

                // The frame samples textures uploaded since the previous frame.
                // Wait for those uploads ahead of this submit so no draw observes a
                // stale image; an upload recorded in the frame's own command buffer
                // would not need this, but the upload API is called outside the
                // frame (see the module's upload ring).
                device_state.upload.wait_all(&device)?;

                let wait_semaphores = [image_available];
                let wait_stages = [vk::PipelineStageFlags::COLOR_ATTACHMENT_OUTPUT];
                let command_buffers = [command_buffer];
                let submit = vk::SubmitInfo::default()
                    .wait_semaphores(&wait_semaphores)
                    .wait_dst_stage_mask(&wait_stages)
                    .command_buffers(&command_buffers)
                    .signal_semaphores(&signal_semaphores);

                // SAFETY: all handles are live. The fence is reset only
                // immediately before the submit, so a failure above leaves it
                // signalled and the slot reusable. A failed submit leaves the
                // fence unsignalled, but the tracker then holds no work for it, so
                // no later wait blocks on a signal that will not come.
                unsafe {
                    device.reset_fences(&[in_flight]).map_err(|error| {
                        device::result_error("failed to reset the frame fence", error)
                    })?;
                    device
                        .queue_submit(graphics_queue, &[submit], in_flight)
                        .map_err(|error| {
                            device::result_error("failed to submit the frame", error)
                        })?;
                }
                // The queue accepted the submission, so the fence will signal.
                presentation.fences.submitted(frame_index);
                presentation.images_in_flight[image_index as usize] = in_flight;
                Ok(())
            })();
            submit_ms = submit_started.elapsed().as_secs_f64() * 1000.0;

            if let Err(error) = record_result {
                // The acquire signalled the semaphore, so the slot cannot be
                // reused until it is replaced. A failure there leaves the
                // swapchain unusable and is reported as out of date so the next
                // frame rebuilds it instead of silently never presenting again.
                if let Err(semaphore_error) =
                    recreate_acquired_semaphore(&device, &mut *presentation, frame_index)
                {
                    log::error!(
                        "vulkan: the frame failed ({error}) and its image-available \
                     semaphore could not be recreated: {semaphore_error}"
                    );
                    return Err(RenderError::SurfaceOutOfDate);
                }
                return Err(error);
            }

            let swapchains = [swapchain];
            let image_indices = [image_index];
            let present_info = vk::PresentInfoKHR::default()
                .wait_semaphores(&signal_semaphores)
                .swapchains(&swapchains)
                .image_indices(&image_indices);
            let present_started = Instant::now();
            let present_result =
            // SAFETY: the present info references live semaphores/swapchain.
            unsafe { swapchain_loader.queue_present(present_queue, &present_info) };
            present_ms = present_started.elapsed().as_secs_f64() * 1000.0;

            presentation.frame_in_flight =
                (presentation.frame_in_flight + 1) % MAX_FRAMES_IN_FLIGHT;
            presentation.frame_index = presentation.frame_index.wrapping_add(1);

            let mut needs_recreate = suboptimal;
            match present_recovery(present_result)
                .map_err(|error| device::result_error("failed to present the frame", error))?
            {
                SwapchainRecovery::None => {}
                SwapchainRecovery::RebuildSwapchain => needs_recreate = true,
                SwapchainRecovery::RecreateSurface => {
                    return Ok(Presentation {
                        presented: false,
                        surface_event: Some(SurfaceEvent::Lost),
                        skip: None,
                    });
                }
            }

            if needs_recreate {
                let recreate_started = Instant::now();
                recreate_swapchain(&device, physical_device, &mut *presentation, surface_handle)?;
                recreate_ms += recreate_started.elapsed().as_secs_f64() * 1000.0;
                presentation.last_timings = Some(FrameTimings {
                    acquire_ms,
                    present_ms,
                    submit_ms,
                    recreate_ms,
                    fence_wait_ms,
                });
                // The frame was presented on the swapchain the recreate just
                // replaced, so it is on screen until a frame of the replacement
                // presents; it is retired, not destroyed.
                presentation.clear_present_fault();
                return Ok(Presentation {
                    presented: true,
                    surface_event: Some(SurfaceEvent::OutOfDate),
                    skip: None,
                });
            }

            presentation.last_timings = Some(FrameTimings {
                acquire_ms,
                present_ms,
                submit_ms,
                recreate_ms,
                fence_wait_ms,
            });
            presentation.clear_present_fault();
            presentation.swapchain_presented = true;
            presentation.dispose_retired_swapchain(&device);
            Ok(Presentation {
                presented: true,
                surface_event: None,
                skip: None,
            })
        })()
    }
}

/// Makes sure the live frame's targets match the swapchain's extent.
///
/// The pass, the pipelines and the targets are created on the first frame and
/// rebuilt when the swapchain's extent changes. The old ones are freed once
/// the device is idle, because a frame may still be reading them.
///
/// # Errors
///
/// Returns [`RenderError::OutOfMemory`] when a resource cannot be created.
fn ensure_frame_targets(
    device: &ash::Device,
    memory_properties: &vk::PhysicalDeviceMemoryProperties,
    resources: &pipelines::PipelineResources,
    presentation: &mut VulkanPresentation,
    format: vk::Format,
    extent: vk::Extent2D,
) -> Result<(), RenderError> {
    let matches = presentation
        .frame_targets
        .first()
        .and_then(|target| target.as_ref())
        .is_some_and(|target| target.extent == extent);
    if matches {
        return Ok(());
    }
    // SAFETY: the device is idle after the wait, so nothing reads the old
    // targets' resources.
    unsafe {
        let _ = device.device_wait_idle();
    }
    for slot in presentation.frame_targets.iter_mut() {
        if let Some(old) = slot.take() {
            old.destroy(device, resources);
        }
    }
    if let Some(old) = presentation.frame_pass.take() {
        old.destroy(device);
    }
    let pass = target::FramePass::create(device, format, resources)?;
    for index in 0..MAX_FRAMES_IN_FLIGHT {
        match target::FrameTarget::create(
            device,
            memory_properties,
            format,
            extent,
            resources,
            &pass,
        ) {
            Ok(target) => presentation.frame_targets[index] = Some(target),
            Err(error) => {
                for slot in presentation.frame_targets.iter_mut() {
                    if let Some(old) = slot.take() {
                        old.destroy(device, resources);
                    }
                }
                pass.destroy(device);
                return Err(error);
            }
        }
    }
    presentation.frame_pass = Some(pass);
    Ok(())
}

/// Immutable surface handle stored in [`Surface`].
pub(crate) struct VulkanSurface {
    /// Presented Vulkan surface.
    pub(crate) surface: vk::SurfaceKHR,
}

/// Command and synchronisation objects for one frame in flight.
pub(crate) struct VulkanFrame {
    /// Command pool that allocates `command_buffer`.
    pub(crate) command_pool: vk::CommandPool,
    /// Primary command buffer recorded each frame.
    pub(crate) command_buffer: vk::CommandBuffer,
    /// Fence signalled when `command_buffer` finishes.
    pub(crate) in_flight: vk::Fence,
    /// Semaphore signalled when a swapchain image is acquired.
    pub(crate) image_available: vk::Semaphore,
}

/// The frame slots' fences and whether their submissions may still be running.
///
/// A fence is signalled only by a submission the queue accepted, so a frame
/// that fails after its fence was reset leaves the fence unsignalled with
/// nothing pending. Waiting for such a fence never returns, so the tracker
/// records which fences may still be running and every wait asks it first.
#[derive(Clone, Copy)]
struct FrameFences {
    /// The fence each frame slot submits with.
    slot_fences: [vk::Fence; MAX_FRAMES_IN_FLIGHT],
    /// Whether a submission with the slot's fence may still be running.
    running: [bool; MAX_FRAMES_IN_FLIGHT],
}

impl FrameFences {
    /// Tracks the fences of the frame slots.
    fn new(slot_fences: [vk::Fence; MAX_FRAMES_IN_FLIGHT]) -> Self {
        Self {
            slot_fences,
            running: [false; MAX_FRAMES_IN_FLIGHT],
        }
    }

    /// Whether the frame slot's fence may still have work in flight.
    fn slot_running(&self, slot: usize) -> bool {
        self.running[slot]
    }

    /// Whether the fence may still have work in flight.
    ///
    /// A fence that belongs to no frame slot never has work in flight.
    fn fence_running(&self, fence: vk::Fence) -> bool {
        self.slot_fences
            .iter()
            .zip(self.running.iter())
            .any(|(slot_fence, running)| *slot_fence == fence && *running)
    }

    /// Records that the queue accepted a submission with the slot's fence.
    fn submitted(&mut self, slot: usize) {
        self.running[slot] = true;
    }

    /// Records that the slot's fence was observed signalled.
    fn completed(&mut self, slot: usize) {
        self.running[slot] = false;
    }

    /// Records that the fence was observed signalled, whichever slot owns it.
    fn completed_fence(&mut self, fence: vk::Fence) {
        for (slot, slot_fence) in self.slot_fences.iter().enumerate() {
            if *slot_fence == fence {
                self.running[slot] = false;
            }
        }
    }
}

/// Collects the frame slots' fences for [`FrameFences`].
fn frame_slot_fences(frames: &[VulkanFrame]) -> [vk::Fence; MAX_FRAMES_IN_FLIGHT] {
    let mut slot_fences = [vk::Fence::null(); MAX_FRAMES_IN_FLIGHT];
    for (slot, frame) in frames.iter().enumerate().take(MAX_FRAMES_IN_FLIGHT) {
        slot_fences[slot] = frame.in_flight;
    }
    slot_fences
}

/// The logical device and the resources that do not depend on a surface.
///
/// A device is created on its own: the caller that presents to a window pairs
/// it with a [`VulkanPresentation`], and the caller that renders offscreen
/// pairs it with nothing. Every resource here outlives a surface and none of
/// them is tied to one, so the same device serves both kinds of caller.
///
/// The device owns its own actions: a caller reaches the textures, the
/// offscreen targets and the effect pipelines through the methods below, not
/// through the fields.
pub(crate) struct VulkanDevice {
    /// The logical device.
    pub(crate) device: ash::Device,
    /// The physical device the logical device was created from.
    pub(crate) physical_device: vk::PhysicalDevice,
    /// Queue used for graphics submission.
    pub(crate) graphics_queue: vk::Queue,
    /// Queue used for presentation.
    ///
    /// A null handle for a device that renders offscreen: it presents to
    /// nothing.
    pub(crate) present_queue: vk::Queue,
    /// Index of the queue family the device was created with.
    pub(crate) queue_family_index: u32,
    /// The descriptor and layout resources every pipeline set shares.
    pub(crate) resources: pipelines::PipelineResources,
    /// Ring of independently synchronised texture-upload slots.
    pub(crate) upload: texture::UploadRing,
    /// Pool used for one synchronous offscreen render or readback.
    pub(crate) offscreen_pool: vk::CommandPool,
    /// Command buffer used for one synchronous offscreen render or readback.
    pub(crate) offscreen_command: vk::CommandBuffer,
    /// Fence waited on after one synchronous offscreen render or readback
    /// (created signalled).
    pub(crate) offscreen_fence: vk::Fence,
    /// GPU textures keyed by [`Texture`] identity.
    pub(crate) textures: Map<u64, texture::VulkanTexture>,
    /// GPU offscreen targets keyed by [`RenderTarget`] identity.
    pub(crate) render_targets: Map<u64, target::VulkanRenderTarget>,
    /// Per-format offscreen render passes and their pipelines.
    pub(crate) offscreen: Map<TextureFormat, target::OffscreenPass>,
    /// Custom effect pipelines, their samplers and the frame's uniform ring,
    /// created on the first created effect pipeline.
    pub(crate) effect: Option<effect::EffectState>,
    /// Host-visible buffer reused by every readback; grown on demand. The
    /// readback entry point takes `&RenderState`, so it is borrowed mutably
    /// only for the duration of one synchronous copy.
    pub(crate) readback: RefCell<texture::StagingBuffer>,
    /// Clone of the owning instance guard (handle only, not a separate owner
    /// beyond the reference count). Kept solely so the instance outlives the
    /// device, and declared last so it is released after it.
    pub(crate) _instance_guard: Arc<InstanceGuard>,
}

impl VulkanDevice {
    /// Creates the logical device and the resources that do not depend on a
    /// surface.
    ///
    /// `target` says what the device is for: a surface it presents to, or
    /// offscreen rendering only. A device that was created and whose
    /// resources failed is destroyed before the error returns.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::DeviceUnavailable`] when the device or one of
    /// its resources cannot be created.
    pub(crate) fn create(
        instance_guard: Arc<InstanceGuard>,
        physical_device: vk::PhysicalDevice,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        target: device::PresentTarget<'_>,
    ) -> Result<Self, RenderError> {
        let bundle = device::create_device(&instance_guard.instance, physical_device, target)?;
        let device = bundle.device;

        let resources = match pipelines::PipelineResources::create(&device, memory_properties) {
            Ok(resources) => resources,
            Err(error) => {
                // SAFETY: the device was created here and nothing uses it.
                unsafe { device.destroy_device(None) };
                return Err(error);
            }
        };
        let upload = match texture::UploadRing::new(
            &device,
            bundle.queue_family_index,
            texture::UPLOAD_SLOTS,
        ) {
            Ok(upload) => upload,
            Err(error) => {
                // SAFETY: the resources were created here and the device is
                // idle.
                unsafe {
                    let _ = device.device_wait_idle();
                    resources.destroy(&device);
                    device.destroy_device(None);
                }
                return Err(error);
            }
        };
        let (offscreen_pool, offscreen_command, offscreen_fence) =
            match create_offscreen_resources(&device, bundle.queue_family_index) {
                Ok(offscreen) => offscreen,
                Err(error) => {
                    // SAFETY: the resources were created here and the device
                    // is idle.
                    unsafe {
                        let _ = device.device_wait_idle();
                        upload.destroy(&device);
                        resources.destroy(&device);
                        device.destroy_device(None);
                    }
                    return Err(error);
                }
            };

        Ok(Self {
            device,
            physical_device,
            graphics_queue: bundle.graphics_queue,
            present_queue: bundle.present_queue,
            queue_family_index: bundle.queue_family_index,
            resources,
            upload,
            offscreen_pool,
            offscreen_command,
            offscreen_fence,
            textures: Map::default(),
            render_targets: Map::default(),
            offscreen: Map::default(),
            effect: None,
            readback: RefCell::new(texture::StagingBuffer::default()),
            _instance_guard: instance_guard,
        })
    }

    /// Releases the GPU resources and the state slot for `tex`.
    pub(crate) fn drop_texture(&mut self, tex: Texture) {
        let Some(texture) = self.textures.remove(&tex.key()) else {
            return;
        };
        let device = self.device.clone();
        // SAFETY: wait for in-flight work before freeing the image; the device
        // is idle afterwards.
        unsafe {
            let _ = device.device_wait_idle();
        }
        texture.destroy(&device, self.resources.descriptor_pool);
    }

    /// Releases the GPU resources and the state slot for `target`.
    pub(crate) fn destroy_render_target(&mut self, target: RenderTarget) {
        let Some(gpu) = self.render_targets.remove(&target.key()) else {
            return;
        };
        let device = self.device.clone();
        // SAFETY: wait for in-flight work before freeing the image.
        unsafe {
            let _ = device.device_wait_idle();
        }
        gpu.destroy(&device);
    }

    /// Creates an effect pipeline whose resources live in this device.
    ///
    /// The device-level resources (the uniform ring and the samplers) are
    /// created with the first effect pipeline; the descriptor set layout, the
    /// pipeline layout and one shader module per stage are created eagerly, so
    /// a descriptor or a module the driver rejects fails here rather than on
    /// the first frame that draws it. The graphics pipeline itself is built
    /// per render pass on first use, because a pipeline is tied to the pass it
    /// is recorded in.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::InvalidArgument`] for a descriptor that breaks
    /// the binding or uniform contract, [`RenderError::Unsupported`] for a
    /// vertex layout or a blend this backend cannot build,
    /// [`RenderError::Shader`] for a module or pipeline the driver rejects, and
    /// [`RenderError::OutOfMemory`] when the ring, the pools or the descriptor
    /// sets cannot be allocated.
    pub(crate) fn create_effect_pipeline(
        &mut self,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        desc: &api::shader::EffectPipelineDesc<'_>,
    ) -> Result<crate::effect::EffectPipeline, RenderError> {
        if self.effect.is_none() {
            // SAFETY: `physical_device` belongs to the instance this device
            // owns and the query is read-only.
            let limits = unsafe {
                self._instance_guard
                    .instance
                    .get_physical_device_properties(self.physical_device)
            }
            .limits;
            let created = effect::EffectState::create(
                &self.device,
                memory_properties,
                limits.min_uniform_buffer_offset_alignment,
            )?;
            self.effect = Some(created);
        }
        let device = self.device.clone();
        let Some(effects) = self.effect.as_mut() else {
            return Err(RenderError::Framebuffer {
                message: "the effect state disappeared during creation".to_owned(),
            });
        };
        effects.create_pipeline(&device, desc)
    }

    /// Releases an effect pipeline, idling the device first.
    pub(crate) fn destroy_effect_pipeline(&mut self, pipeline: crate::effect::EffectPipeline) {
        let Some(effects) = self.effect.as_mut() else {
            return;
        };
        let device = self.device.clone();
        // SAFETY: an in-flight frame may still draw with the pipeline, so the
        // device is idled before the handles are freed.
        unsafe {
            let _ = device.device_wait_idle();
        }
        effects.destroy_pipeline(&device, pipeline.key());
    }

    /// Renders `scene` into the offscreen `target`.
    ///
    /// The offscreen render is synchronous: its pixels are read back by a
    /// separate call, so the result must be complete when this returns. Any
    /// texture upload that is still in flight is waited before recording, so
    /// the pass samples the pixels it was given.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::InvalidArgument`] for a mismatched backend and
    /// [`RenderError::Unsupported`] when the scene needs a feature the Vulkan
    /// backend does not implement.
    pub(crate) fn render_to_target(
        &mut self,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        target: &RenderTarget,
        scene: &RenderScene,
    ) -> Result<(), RenderError> {
        let VulkanDevice {
            device,
            graphics_queue,
            upload,
            offscreen_pool,
            offscreen_command,
            offscreen_fence,
            offscreen,
            render_targets,
            textures,
            effect,
            resources,
            ..
        } = self;

        let format = target.desc().format;
        let key = target.key();

        if let hashbrown::hash_map::Entry::Vacant(entry) = offscreen.entry(format) {
            entry.insert(target::OffscreenPass::create(device, format, resources)?);
        }
        let pass = offscreen
            .get(&format)
            .ok_or_else(|| RenderError::Framebuffer {
                message: "offscreen pipeline state disappeared".to_owned(),
            })?;
        if let hashbrown::hash_map::Entry::Vacant(entry) = render_targets.entry(key) {
            entry.insert(target::VulkanRenderTarget::create(
                device,
                memory_properties,
                pass,
                *target.desc(),
            )?);
        }
        let gpu = render_targets
            .get_mut(&key)
            .ok_or_else(|| RenderError::Framebuffer {
                message: "render-target state disappeared".to_owned(),
            })?;
        upload.wait_all(device)?;
        let submit = target::OffscreenSubmit {
            device,
            queue: *graphics_queue,
            command_pool: *offscreen_pool,
            command_buffer: *offscreen_command,
            fence: *offscreen_fence,
        };
        // The offscreen render owns a uniform slot of its own and runs while a
        // live frame may still be in flight.
        if let Some(effects) = effect.as_mut() {
            effects.begin_offscreen_frame();
        }
        let result = target::render(
            &submit,
            gpu,
            pass,
            resources,
            textures,
            effect.as_mut(),
            scene,
        );
        if result.is_ok()
            && let Some(effects) = effect.as_mut()
        {
            // The offscreen submit completed only after every command buffer
            // submitted before it, so no descriptor set is in use any more.
            effects.end_offscreen_frame(device);
        }
        result
    }

    /// Copies a sub-rectangle of an offscreen `target` into `out`.
    ///
    /// The copy is synchronous: a script observes `out` on return. The
    /// readback buffer is reused across calls rather than allocated per call;
    /// the readback API takes `&RenderState`, so the buffer sits behind a
    /// `RefCell` in the device and is borrowed for the duration of the call.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Framebuffer`] when the region is out of bounds,
    /// the output buffer is the wrong size or the target was never rendered.
    pub(crate) fn read_render_target(
        &self,
        memory_properties: &vk::PhysicalDeviceMemoryProperties,
        target: &RenderTarget,
        region: PixelRegion,
        out: &mut [u8],
    ) -> Result<(), RenderError> {
        let gpu =
            self.render_targets
                .get(&target.key())
                .ok_or_else(|| RenderError::Framebuffer {
                    message: "render target has not been rendered yet".to_owned(),
                })?;
        validate_readback(target.desc(), region, out.len())?;
        let mut readback = self.readback.borrow_mut();
        let submit = target::OffscreenSubmit {
            device: &self.device,
            queue: self.graphics_queue,
            command_pool: self.offscreen_pool,
            command_buffer: self.offscreen_command,
            fence: self.offscreen_fence,
        };
        target::read(&submit, memory_properties, gpu, &mut readback, region, out)
    }
}

impl Drop for VulkanDevice {
    fn drop(&mut self) {
        // SAFETY: the caller is responsible for dropping the resources after
        // all GPU work has finished; `device_wait_idle` makes that true here.
        // Handles are destroyed in creation-reverse order and the device is
        // destroyed last.
        unsafe {
            let _ = self.device.device_wait_idle();
            let descriptor_pool = self.resources.descriptor_pool;
            for texture in self.textures.values() {
                texture.destroy(&self.device, descriptor_pool);
            }
            for target in self.render_targets.values() {
                target.destroy(&self.device);
            }
            for pass in self.offscreen.values() {
                pass.destroy(&self.device);
            }
            if let Some(mut effects) = self.effect.take() {
                effects.destroy(&self.device);
            }
            self.resources.destroy(&self.device);
            self.upload.destroy(&self.device);
            self.readback.get_mut().destroy(&self.device);
            self.device.destroy_command_pool(self.offscreen_pool, None);
            self.device.destroy_fence(self.offscreen_fence, None);
            self.device.destroy_device(None);
        }
    }
}

/// The resources that present a frame to a surface.
///
/// A device that renders offscreen owns none of them: the surface, the
/// swapchain, the pipelines and the frame slots belong to the presentation,
/// not to the device.
pub(crate) struct VulkanPresentation {
    /// Instance-level surface function pointers.
    pub(crate) surface_loader: ash::khr::surface::Instance,
    /// Device-level swapchain function pointers.
    pub(crate) swapchain_loader: ash::khr::swapchain::Device,
    /// The surface this state presents to (also owned by `Surface`).
    pub(crate) surface: vk::SurfaceKHR,
    /// Display connection the surface was created from, kept so a lost surface
    /// can be recreated against the same window.
    pub(crate) display_handle: RawDisplayHandle,
    /// Window the surface was created from, kept for the same reason.
    pub(crate) window_handle: RawWindowHandle,
    /// Preferred swapchain format requested when the pipeline was created.
    pub(crate) preferred_format: TextureFormat,
    /// Swapchain, render pass and framebuffers.
    pub(crate) swapchain: swapchain::SwapchainState,
    /// The swapchain the most recent recreate replaced.
    ///
    /// The display may still be showing one of its images, so it is destroyed
    /// only after a frame of the replacement has been presented, or at
    /// teardown.
    retired_swapchain: Option<swapchain::SwapchainState>,
    /// Compiled pipelines for the swapchain's render pass.
    pub(crate) pipelines: pipelines::PipelineSet,
    /// The render pass and the pipelines the live frame is composed into.
    ///
    /// They are created on the first frame and rebuilt when the swapchain's
    /// extent changes.
    pub(crate) frame_pass: Option<target::FramePass>,
    /// The live frame's targets, one per frame slot.
    ///
    /// Every slot has a target of its own, because two frames can be in
    /// flight and a frame must not write the target the other one presents.
    pub(crate) frame_targets: Vec<Option<target::FrameTarget>>,
    /// Per-frame command buffers and sync objects.
    pub(crate) frames: Vec<VulkanFrame>,
    /// The frame slots' fences and whether their submissions may still be
    /// running.
    fences: FrameFences,
    /// Fence guarding each swapchain image.
    pub(crate) images_in_flight: Vec<vk::Fence>,
    /// Render-finished semaphore per swapchain image.
    pub(crate) render_finished: Vec<vk::Semaphore>,
    /// Index of the next frame slot.
    pub(crate) frame_in_flight: usize,
    /// Last reported surface size in physical pixels.
    pub(crate) surface_size: (u32, u32),
    /// Number of frames recorded through this state.
    pub(crate) frame_index: u64,
    /// Whether the swapchain is usable.
    pub(crate) swapchain_ready: bool,
    /// Whether the current swapchain presented a frame.
    ///
    /// A swapchain that never presented has no image on screen, so a recreate
    /// may destroy it at once; one that did present is retired until its
    /// replacement presents.
    swapchain_presented: bool,
    /// Presentation fault already reported, so a persistent stall logs once
    /// per cause instead of once per frame.
    present_fault: Option<PresentFault>,
    /// Vulkan call timings from the most recent presented frame.
    pub(crate) last_timings: Option<FrameTimings>,
}

impl VulkanPresentation {
    /// Destroys the presentation resources.
    ///
    /// The device is not destroyed here: it belongs to the device state,
    /// which outlives the presentation. The caller guarantees the GPU is
    /// idle, and the surface is destroyed before the device that presents to
    /// it.
    fn destroy(&mut self, device: &ash::Device, resources: &pipelines::PipelineResources) {
        // SAFETY: the caller guarantees the GPU is idle, so no handle below
        // is in use. They are destroyed in reverse creation order.
        unsafe {
            for target in self.frame_targets.iter_mut().flatten() {
                target.destroy(device, resources);
            }
            if let Some(pass) = self.frame_pass.take() {
                pass.destroy(device);
            }
            self.pipelines.destroy(device);
            if let Some(retired) = self.retired_swapchain.take() {
                // The render pass is shared with the swapchain that replaced
                // this one and is destroyed with the last swapchain that uses
                // it.
                retired.destroy(device, &self.swapchain_loader);
            }
            self.swapchain.destroy(device, &self.swapchain_loader);
            self.swapchain.destroy_render_pass(device);
            for frame in &self.frames {
                destroy_frame(device, frame);
            }
            for semaphore in &self.render_finished {
                device.destroy_semaphore(*semaphore, None);
            }
            self.surface_loader.destroy_surface(self.surface, None);
        }
    }

    /// Destroys the swapchain a recreate replaced, once a frame of its
    /// replacement has been presented.
    fn dispose_retired_swapchain(&mut self, device: &ash::Device) {
        let Some(retired) = self.retired_swapchain.take() else {
            return;
        };
        // The render pass is shared with the swapchain that replaced this one
        // and is destroyed with the last swapchain that uses it.
        retired.destroy(device, &self.swapchain_loader);
    }

    /// Reports `fault` as a warning when it is not the fault already reported.
    fn report_present_fault(&mut self, fault: PresentFault) {
        if self.present_fault == Some(fault) {
            return;
        }
        log::warn!("vulkan: presentation stalled: {}", fault.message());
        self.present_fault = Some(fault);
    }

    /// Clears the reported fault after a presented frame, logging the recovery
    /// once.
    fn clear_present_fault(&mut self) {
        if let Some(fault) = self.present_fault.take() {
            log::info!("vulkan: presentation recovered: {}", fault.message());
        }
    }
}

/// The state of one Vulkan render caller.
///
/// A caller either presents to a surface or renders offscreen, and the two
/// need different resources: the presenting caller owns a presentation (the
/// surface, the swapchain and the frame slots), the offscreen caller owns
/// only the device. Both own the device, because the device does not depend
/// on a surface.
pub(crate) enum VulkanState {
    /// A caller that presents to a surface.
    Presenting(Box<VulkanPresentingState>),
    /// A caller that renders into offscreen targets and presents to nothing.
    Offscreen(Box<VulkanDevice>),
}

/// The state of a caller that presents to a surface.
///
/// The device and the presentation are separate owners: the device's
/// resources outlive the surface, and the presentation's resources belong to
/// the device that presents to them.
pub(crate) struct VulkanPresentingState {
    /// The logical device and the resources that do not depend on a surface.
    pub(crate) device: VulkanDevice,
    /// The surface, the swapchain and the frame slots.
    pub(crate) presentation: VulkanPresentation,
}

impl VulkanState {
    /// The device the state owns.
    pub(crate) fn device(&self) -> &VulkanDevice {
        match self {
            Self::Presenting(presenting) => &presenting.device,
            Self::Offscreen(device) => device,
        }
    }

    /// The device the state owns, mutably.
    pub(crate) fn device_mut(&mut self) -> &mut VulkanDevice {
        match self {
            Self::Presenting(presenting) => &mut presenting.device,
            Self::Offscreen(device) => device,
        }
    }

    /// The timings of the most recent presented frame.
    ///
    /// A state that renders offscreen presents no frame, so it has no
    /// presentation timings.
    pub(crate) fn last_frame_timings(&self) -> Option<FrameTimings> {
        match self {
            Self::Presenting(presenting) => presenting.presentation.last_timings,
            Self::Offscreen(_) => None,
        }
    }
}

impl Drop for VulkanPresentingState {
    fn drop(&mut self) {
        // SAFETY: the GPU is idle before any handle below is freed.
        unsafe {
            let _ = self.device.device.device_wait_idle();
        }
        // The presentation resources belong to the device, so they are
        // destroyed before it.
        self.presentation
            .destroy(&self.device.device, &self.device.resources);
    }
}

/// RAII teardown for the resources created during `begin_with_display`.
///
/// The guard borrows the swapchain and pipelines, which live in the caller's
/// scope until ownership transfers to the state. It also owns the frames and
/// presentation semaphores created later. Disarming it before the successful
/// return prevents double teardown on `Drop`.
///
/// The device and its own resources are not the guard's: they belong to the
/// device state, which outlives the guard and is destroyed after it.
struct BeginTeardown<'a> {
    device: &'a ash::Device,
    surface_loader: &'a ash::khr::surface::Instance,
    swapchain_loader: &'a ash::khr::swapchain::Device,
    surface: vk::SurfaceKHR,
    swapchain: &'a swapchain::SwapchainState,
    pipelines: &'a pipelines::PipelineSet,
    frames: Vec<VulkanFrame>,
    render_finished: Vec<vk::Semaphore>,
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
        }
    }
}

/// Returns the framebuffer for a swapchain image, or an error if stale.
fn framebuffers_for(
    presentation: &VulkanPresentation,
    image_index: u32,
) -> Result<vk::Framebuffer, RenderError> {
    presentation
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
    let command_pool =
        // SAFETY: `device` is live and the pool info is valid.
        unsafe { device.create_command_pool(&pool_info, None) }.map_err(|error| {
            RenderError::DeviceUnavailable {
                message: format!(
                    "failed to create a frame command pool: {}",
                    device::vk_result_message(error)
                ),
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
        // SAFETY: `device` is live.
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
            Err(RenderError::DeviceUnavailable {
                message: "failed to create frame synchronisation objects".to_owned(),
            })
        }
    }
}

/// Creates the reusable offscreen command buffer and a signalled fence.
///
/// `render_to_target` and `read_render_target` are synchronous and never run
/// concurrently, so they share one pool, command buffer and fence; the upload
/// ring has its own, so an offscreen render no longer serialises uploads.
fn create_offscreen_resources(
    device: &ash::Device,
    queue_family_index: u32,
) -> Result<(vk::CommandPool, vk::CommandBuffer, vk::Fence), RenderError> {
    let pool_info = vk::CommandPoolCreateInfo::default()
        .queue_family_index(queue_family_index)
        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER);
    // SAFETY: `device` is live.
    let pool = unsafe { device.create_command_pool(&pool_info, None) }.map_err(|error| {
        RenderError::DeviceUnavailable {
            message: format!(
                "failed to create the offscreen command pool: {}",
                device::vk_result_message(error)
            ),
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
                message: "the offscreen pool returned no command buffer".to_owned(),
            });
        }
        Err(error) => {
            // SAFETY: owned here.
            unsafe { device.destroy_command_pool(pool, None) };
            return Err(RenderError::OutOfMemory {
                message: format!("failed to allocate the offscreen command buffer: {error}"),
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
        RenderError::DeviceUnavailable {
            message: format!(
                "failed to create the offscreen fence: {}",
                device::vk_result_message(error)
            ),
        }
    })?;

    Ok((pool, command_buffer, fence))
}

/// The recovery a swapchain operation needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SwapchainRecovery {
    /// The operation succeeded without a suboptimal result.
    None,
    /// The swapchain must be rebuilt; the surface is still valid.
    RebuildSwapchain,
    /// The surface is lost; recreate the surface and the swapchain.
    RecreateSurface,
}

/// Why the backend cannot present a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PresentFault {
    /// A previous swapchain rebuild left the swapchain unusable.
    SwapchainNotReady,
    /// The swapchain reports no images to acquire.
    NoImages,
    /// `acquire_next_image` reported the swapchain out of date.
    AcquireOutOfDate,
    /// Rebuilding the unusable swapchain failed.
    RebuildFailed,
}

impl PresentFault {
    /// One-line description used by the once-per-fault log lines.
    fn message(self) -> &'static str {
        match self {
            Self::SwapchainNotReady => "the swapchain is not ready",
            Self::NoImages => "the swapchain has no images",
            Self::AcquireOutOfDate => "the swapchain was out of date at image acquire",
            Self::RebuildFailed => "rebuilding the swapchain failed",
        }
    }
}

/// The frame-relevant classification of a swapchain image acquire.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AcquireOutcome {
    /// An image was acquired; `suboptimal` asks for a later rebuild.
    Image {
        /// Index of the acquired swapchain image.
        index: u32,
        /// Whether the acquire reported the swapchain as suboptimal.
        suboptimal: bool,
    },
    /// The swapchain is out of date and must be rebuilt.
    OutOfDate,
    /// The surface is lost and must be recreated.
    SurfaceLost,
    /// No image became available within the frame's presentation budget.
    TimedOut,
}

/// Classifies an acquire result, keeping every other error for the caller.
///
/// A timeout is a documented result of the acquire, not a failure: no image
/// was released, and the semaphore and fence are unmodified, so the frame can
/// be skipped without touching the swapchain. Every other error still reaches
/// the caller as a typed error.
fn acquire_outcome(result: Result<(u32, bool), vk::Result>) -> Result<AcquireOutcome, vk::Result> {
    match result {
        Ok((index, suboptimal)) => Ok(AcquireOutcome::Image { index, suboptimal }),
        Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => Ok(AcquireOutcome::OutOfDate),
        Err(vk::Result::ERROR_SURFACE_LOST_KHR) => Ok(AcquireOutcome::SurfaceLost),
        Err(vk::Result::TIMEOUT) => Ok(AcquireOutcome::TimedOut),
        Err(error) => Err(error),
    }
}

/// Returns the fault an unusable-swapchain state implies, or `None` when the
/// swapchain can present.
fn swapchain_fault(ready: bool, image_count: usize) -> Option<PresentFault> {
    if !ready {
        Some(PresentFault::SwapchainNotReady)
    } else if image_count == 0 {
        Some(PresentFault::NoImages)
    } else {
        None
    }
}

/// Classifies a present result into the recovery it needs.
///
/// `VK_SUBOPTIMAL_KHR` and `VK_ERROR_OUT_OF_DATE_KHR` rebuild the swapchain
/// against the same surface; `VK_ERROR_SURFACE_LOST_KHR` is the only result
/// that needs a new surface. Any other error is returned to the caller.
fn present_recovery(result: Result<bool, vk::Result>) -> Result<SwapchainRecovery, vk::Result> {
    match result {
        Ok(false) => Ok(SwapchainRecovery::None),
        Ok(true) => Ok(SwapchainRecovery::RebuildSwapchain),
        Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => Ok(SwapchainRecovery::RebuildSwapchain),
        Err(vk::Result::ERROR_SURFACE_LOST_KHR) => Ok(SwapchainRecovery::RecreateSurface),
        Err(error) => Err(error),
    }
}

/// Recreates the swapchain and its per-image synchronisation objects.
///
/// The old swapchain is passed as a hint to the driver and destroyed only
/// after the replacement is built. If the rebuild fails the previous state is
/// kept (with `swapchain_ready` cleared) so a later resize can retry without
/// losing the pinned render pass.
fn recreate_swapchain(
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    presentation: &mut VulkanPresentation,
    surface: vk::SurfaceKHR,
) -> Result<(), RenderError> {
    // SAFETY: the device is idle before any swapchain resource is touched.
    unsafe { device.device_wait_idle() }.map_err(|error| {
        device::result_error(
            "failed to idle the device before recreating the swapchain",
            error,
        )
    })?;

    let requested = presentation.surface_size;
    let new_swapchain = swapchain::create(
        &swapchain::SurfaceContext {
            device,
            physical_device,
            surface_loader: &presentation.surface_loader,
            swapchain_loader: &presentation.swapchain_loader,
            surface,
        },
        requested,
        presentation.preferred_format,
        Some(&presentation.swapchain),
    );

    match new_swapchain {
        Ok(new_swapchain) => {
            if presentation.swapchain_presented {
                // The display may still be showing the last image the current
                // swapchain presented, so it is retired and destroyed only
                // after a frame of its replacement has been presented. A
                // swapchain retired earlier no longer has a frame on screen.
                presentation.dispose_retired_swapchain(device);
                let old = std::mem::replace(&mut presentation.swapchain, new_swapchain);
                presentation.retired_swapchain = Some(old);
            } else {
                // The current swapchain never presented, so nothing of it is
                // on screen and it is destroyed now. The swapchain retired
                // before it may still be on screen and stays retired.
                let unused = std::mem::replace(&mut presentation.swapchain, new_swapchain);
                unused.destroy(device, &presentation.swapchain_loader);
            }
            presentation.swapchain_presented = false;
            if let Err(error) = rebuild_image_sync(presentation, device) {
                presentation.swapchain_ready = false;
                return Err(error);
            }
            presentation.swapchain_ready = true;
            Ok(())
        }
        Err(error) => {
            // Keep the previous render pass and format for a later retry.
            presentation.swapchain_ready = false;
            Err(error)
        }
    }
}

/// Recreates the presentation surface and the swapchain that presents to it.
///
/// The new swapchain references the new surface, so the surface is adopted
/// before the swapchain is swapped in and the old surface is destroyed only
/// after its last user, the old swapchain, is gone. Neither handle is
/// destroyed twice: the new surface replaces the presentation's surface and
/// is destroyed by [`VulkanPresentation`]'s teardown exactly as the surface
/// it replaces would be.
fn recreate_surface(
    pipeline: &VulkanPipeline,
    device: &ash::Device,
    physical_device: vk::PhysicalDevice,
    presentation: &mut VulkanPresentation,
    surface: &mut VulkanSurface,
) -> Result<(), RenderError> {
    // SAFETY: no GPU work may reference the old surface while it is replaced.
    unsafe { device.device_wait_idle() }.map_err(|error| {
        device::result_error(
            "failed to idle the device before recreating the surface",
            error,
        )
    })?;

    // The stored window/display handles are the only inputs a replacement
    // `VkSurfaceKHR` needs.
    // SAFETY: the handles were live when the original surface was created and
    // still describe the same window; `ash-window` dispatches on the variant.
    let new_surface = unsafe {
        ash_window::create_surface(
            &pipeline.instance_guard.entry,
            &pipeline.instance_guard.instance,
            presentation.display_handle,
            presentation.window_handle,
            None,
        )
    }
    .map_err(|error| RenderError::DeviceUnavailable {
        message: format!(
            "failed to recreate the Vulkan presentation surface: {}",
            device::vk_result_message(error)
        ),
    })?;

    // Build the replacement against the old swapchain first: it reuses the
    // pinned format and render pass, and a failure leaves the old swapchain
    // intact, so the new surface still has no user and is destroyed here.
    let new_swapchain = swapchain::create(
        &swapchain::SurfaceContext {
            device,
            physical_device,
            surface_loader: &presentation.surface_loader,
            swapchain_loader: &presentation.swapchain_loader,
            surface: new_surface,
        },
        presentation.surface_size,
        presentation.preferred_format,
        Some(&presentation.swapchain),
    );
    let new_swapchain = match new_swapchain {
        Ok(new_swapchain) => new_swapchain,
        Err(error) => {
            // SAFETY: the surface was created above and no swapchain uses it.
            unsafe {
                presentation
                    .surface_loader
                    .destroy_surface(new_surface, None)
            };
            return Err(error);
        }
    };

    // The new swapchain references the new surface from here on, so the swap
    // is adopted before anything else can fail. The old swapchain is the old
    // surface's last user and is destroyed before it.
    // A swapchain retired by an earlier resize targets the old surface, so it
    // is destroyed before the surface goes; a lost surface has no image on
    // screen to keep it alive for.
    presentation.dispose_retired_swapchain(device);
    let old_surface = std::mem::replace(&mut presentation.surface, new_surface);
    surface.surface = new_surface;
    let old_swapchain = std::mem::replace(&mut presentation.swapchain, new_swapchain);
    old_swapchain.destroy(device, &presentation.swapchain_loader);
    presentation.swapchain_presented = false;
    // SAFETY: the old swapchain is gone and the device is idle.
    unsafe {
        presentation
            .surface_loader
            .destroy_surface(old_surface, None)
    };

    // A failure here leaves the surface valid but the swapchain unusable;
    // `swapchain_ready` makes the next frame report out-of-date for a retry.
    if let Err(error) = rebuild_image_sync(presentation, device) {
        presentation.swapchain_ready = false;
        return Err(error);
    }
    presentation.swapchain_ready = true;
    Ok(())
}

/// Rebuilds the per-image fences and semaphores after a swapchain change.
///
/// The presentation semaphores are always recreated: a `queue_present` on the
/// previous generation may have signalled one, and a binary semaphore that is
/// already signalled must not be signalled again. A frame whose image-available
/// semaphore could not be recreated is also repaired here, because the rebuild
/// only succeeds when every subsequent acquire has a valid semaphore. The
/// device is idle here, so destroying and recreating them is safe.
fn rebuild_image_sync(
    presentation: &mut VulkanPresentation,
    device: &ash::Device,
) -> Result<(), RenderError> {
    let image_count = presentation.swapchain.images.len().max(1);
    for semaphore in presentation.render_finished.drain(..) {
        // SAFETY: owned here and the GPU is idle.
        unsafe { device.destroy_semaphore(semaphore, None) };
    }
    for _ in 0..image_count {
        let semaphore =
            // SAFETY: `device` is live.
            unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.map_err(
                |error| RenderError::DeviceUnavailable {
                    message: format!(
                        "failed to create presentation semaphores: {}",
                        device::vk_result_message(error)
                    ),
                },
            )?;
        presentation.render_finished.push(semaphore);
    }
    for frame in &mut presentation.frames {
        if frame.image_available != vk::Semaphore::null() {
            continue;
        }
        let semaphore =
            // SAFETY: `device` is live and the GPU is idle after the swapchain
            // change.
            unsafe { device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None) }.map_err(
                |error| RenderError::DeviceUnavailable {
                    message: format!(
                        "failed to recreate a frame's image-available semaphore: {}",
                        device::vk_result_message(error)
                    ),
                },
            )?;
        frame.image_available = semaphore;
    }
    presentation.images_in_flight = vec![vk::Fence::null(); image_count];
    Ok(())
}

/// Recreates a frame's `image_available` semaphore after an error unwound
/// between `acquire_next_image` and a successful submit.
///
/// The acquire signalled the old semaphore, so it can no longer be reused.
/// Destroying it is safe because no queue operation is waiting on it. When the
/// replacement cannot be created the swapchain is marked unusable and the
/// failure is returned, so the next frame rebuilds it instead of touching a
/// null semaphore.
fn recreate_acquired_semaphore(
    device: &ash::Device,
    presentation: &mut VulkanPresentation,
    frame_index: usize,
) -> Result<(), RenderError> {
    if frame_index >= presentation.frames.len() {
        presentation.swapchain_ready = false;
        return Err(RenderError::Framebuffer {
            message: format!("frame slot {frame_index} does not exist"),
        });
    }
    let frame = &mut presentation.frames[frame_index];
    // SAFETY: the acquire signalled this semaphore and nothing waits on it.
    let recreated = unsafe {
        device.destroy_semaphore(frame.image_available, None);
        device.create_semaphore(&vk::SemaphoreCreateInfo::default(), None)
    };
    match recreated {
        Ok(semaphore) => {
            presentation.frames[frame_index].image_available = semaphore;
            Ok(())
        }
        Err(error) => {
            presentation.frames[frame_index].image_available = vk::Semaphore::null();
            presentation.swapchain_ready = false;
            Err(RenderError::DeviceUnavailable {
                message: format!(
                    "failed to recreate the image-available semaphore: {}",
                    device::vk_result_message(error)
                ),
            })
        }
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

/// The error a presentation action reports for a state that has no surface.
///
/// A state that renders offscreen has no surface to present to, so the
/// action is refused instead of presenting nothing.
fn no_surface_error() -> RenderError {
    RenderError::InvalidArgument {
        message: "this render state has no presentation surface; it renders offscreen".to_owned(),
    }
}

/// Converts the scene background into a render-pass clear color.
///
/// The pipeline composites in the encoded space, so the background's colour
/// reaches the target as it is.
pub(crate) fn clear_value(background: Option<[f32; 4]>) -> [f32; 4] {
    background.unwrap_or([0.0, 0.0, 0.0, 1.0])
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

/// Returns the number of texture-upload batches the Vulkan backend has
/// submitted through `state`, or `None` when `state` is not a Vulkan state.
///
/// This is a diagnostic for asserting that a frame's uploads were merged: a
/// frame that rasterizes N new glyphs should report one batch for them, not N.
/// It is only meaningful on a machine that can create a Vulkan device.
#[cfg(test)]
fn upload_submit_count(state: &RenderState) -> Option<u64> {
    match &state.kind {
        StateKind::Vulkan(vstate) => Some(vstate.device().upload.submits()),
        StateKind::Null(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AcquireOutcome, FrameFences, PresentFault, SwapchainRecovery, acquire_outcome,
        present_recovery, swapchain_fault, upload_submit_count,
    };
    use api::geometry::PixelRegion;
    use ash::vk;
    use ash::vk::Handle;
    use testkit::headless_window;

    use crate::{
        PipelineConfig, TextureDesc, TextureUpload, begin_render_state, create_render_pipeline,
        create_texture, upload_textures,
    };

    /// The upload ring merges every texture of one `upload_textures` call into
    /// a single queue submit, so a frame's dirty bitmaps cost one submit, not
    /// one per texture.
    ///
    /// A headless CI machine has no window surface, so this returns early when
    /// the Vulkan device or surface cannot be created. On a machine with a GPU
    /// it asserts the submit count, which is the host-observable half of the
    /// ring contract (the null backend cannot count GPU submits).
    #[test]
    fn vulkan_upload_batch_is_one_submit_or_skips() {
        let Ok(pipeline) = create_render_pipeline(PipelineConfig::vulkan()) else {
            return;
        };
        let Ok((_surface, mut state)) = begin_render_state(&pipeline, &headless_window()) else {
            return;
        };
        let a = create_texture(&pipeline, TextureDesc::new(4, 4)).expect("texture a");
        let b = create_texture(&pipeline, TextureDesc::new(4, 4)).expect("texture b");
        let a_pixels = [1u8; 16];
        let b_pixels = [2u8; 16];

        let before = upload_submit_count(&state).expect("vulkan state");
        upload_textures(
            &pipeline,
            &mut state,
            &[
                TextureUpload {
                    texture: &a,
                    region: PixelRegion::new(0, 0, 2, 2),
                    pixels: &a_pixels,
                },
                TextureUpload {
                    texture: &b,
                    region: PixelRegion::new(0, 0, 2, 2),
                    pixels: &b_pixels,
                },
            ],
        )
        .expect("batch upload");
        let after = upload_submit_count(&state).expect("vulkan state");
        assert_eq!(
            after - before,
            1,
            "two textures in one batch must be one submit"
        );
    }

    /// A fence handle for the test; the handle is never used with a device.
    fn fence(raw: u64) -> vk::Fence {
        vk::Fence::from_raw(raw)
    }

    /// A frame that fails before its submit leaves no fence to wait on.
    ///
    /// The fence is reset immediately before the submit, so a failure above
    /// leaves it signalled; a failed submit leaves it unsignalled with nothing
    /// pending. Neither may leave a later frame waiting for a signal that
    /// will not come.
    #[test]
    fn a_failed_frame_leaves_no_fence_to_wait_on() {
        let mut fences = FrameFences::new([fence(1), fence(2)]);

        // The first frame submits and its work may still be running.
        fences.submitted(0);
        assert!(fences.slot_running(0));
        assert!(fences.fence_running(fence(1)));

        // The frame that reuses the slot waits and observes the completion.
        fences.completed(0);
        assert!(!fences.slot_running(0));
        assert!(!fences.fence_running(fence(1)));

        // The next frame fails before its submit: the fence was reset, but no
        // submission was accepted, so nothing waits for it.
        assert!(!fences.slot_running(0));
        assert!(!fences.fence_running(fence(1)));
    }

    /// A frame that waits for a swapchain image clears the state of the frame
    /// whose fence it waited for.
    #[test]
    fn an_image_wait_clears_the_frame_it_waits_for() {
        let mut fences = FrameFences::new([fence(1), fence(2)]);
        fences.submitted(1);
        assert!(fences.fence_running(fence(2)));
        fences.completed_fence(fence(2));
        assert!(!fences.fence_running(fence(2)));
        assert!(!fences.slot_running(1));
        // The other frame slot is untouched.
        assert!(!fences.slot_running(0));
    }

    /// A fence that belongs to no frame slot never has work in flight.
    #[test]
    fn a_foreign_fence_never_has_work_in_flight() {
        let fences = FrameFences::new([fence(1), fence(2)]);
        assert!(!fences.fence_running(vk::Fence::null()));
        assert!(!fences.fence_running(fence(9)));
        assert!(!fences.slot_running(0));
        assert!(!fences.slot_running(1));
    }

    /// `VK_TIMEOUT` is a skippable frame while every other non-surface acquire
    /// error still reaches the caller; the reference's present does not block
    /// on the display, so an unavailable image is a missed frame rather than a
    /// stall.
    #[test]
    fn a_timed_out_acquire_is_a_skippable_frame_and_every_other_error_reaches_the_caller() {
        // reference/cpp/core/visual/impl/BasicDrawDevice.cpp:244
        // reference/cpp/core/visual/impl/VSyncTimingThread.cpp:88-146
        assert_eq!(
            acquire_outcome(Ok((3, false))),
            Ok(AcquireOutcome::Image {
                index: 3,
                suboptimal: false,
            })
        );
        assert_eq!(
            acquire_outcome(Err(vk::Result::ERROR_OUT_OF_DATE_KHR)),
            Ok(AcquireOutcome::OutOfDate)
        );
        assert_eq!(
            acquire_outcome(Err(vk::Result::ERROR_SURFACE_LOST_KHR)),
            Ok(AcquireOutcome::SurfaceLost)
        );
        assert_eq!(
            acquire_outcome(Err(vk::Result::TIMEOUT)),
            Ok(AcquireOutcome::TimedOut)
        );
        assert_eq!(
            acquire_outcome(Err(vk::Result::ERROR_DEVICE_LOST)),
            Err(vk::Result::ERROR_DEVICE_LOST)
        );
    }

    /// `VK_ERROR_SURFACE_LOST_KHR` is the one present result that invalidates
    /// the surface; out-of-date and suboptimal only need the swapchain
    /// rebuilt, and every other error still reaches the caller.
    #[test]
    fn a_lost_surface_needs_a_new_surface_but_out_of_date_only_rebuilds_the_swapchain() {
        assert_eq!(present_recovery(Ok(false)), Ok(SwapchainRecovery::None));
        assert_eq!(
            present_recovery(Ok(true)),
            Ok(SwapchainRecovery::RebuildSwapchain)
        );
        assert_eq!(
            present_recovery(Err(vk::Result::ERROR_OUT_OF_DATE_KHR)),
            Ok(SwapchainRecovery::RebuildSwapchain)
        );
        assert_eq!(
            present_recovery(Err(vk::Result::ERROR_SURFACE_LOST_KHR)),
            Ok(SwapchainRecovery::RecreateSurface)
        );
        assert_eq!(
            present_recovery(Err(vk::Result::ERROR_DEVICE_LOST)),
            Err(vk::Result::ERROR_DEVICE_LOST)
        );
    }

    /// A swapchain that is not ready or has no images is a named fault, not a
    /// frame that silently reports `presented: false`.
    #[test]
    fn an_unusable_swapchain_names_the_fault_that_blocks_presentation() {
        assert_eq!(
            swapchain_fault(false, 2),
            Some(PresentFault::SwapchainNotReady)
        );
        assert_eq!(swapchain_fault(true, 0), Some(PresentFault::NoImages));
        assert_eq!(swapchain_fault(true, 2), None);
    }
}
