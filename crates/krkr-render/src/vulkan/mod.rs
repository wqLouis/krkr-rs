//! Vulkan (`ash`) render backend skeleton.
//!
//! Round 1 provides only the typed shape of the Vulkan mechanism: the
//! immutable device bundle, the surface/swapchain handle, the per-caller
//! state and the texture image. Instance/device creation, `ash-window`
//! surface creation, the two base shader programs, blend-state selection,
//! command recording and presentation land in Phase 5
//! (`docs/migration/04-renderer-vulkan.md`).
//!
//! [`VulkanPipeline::new`] and the free functions below are stubs that return
//! [`RenderError::Unsupported`]; they exist so the facade in
//! [`crate`] already has a concrete Vulkan arm to grow into.

use ash::vk;
use raw_window_handle::RawWindowHandle;

use krkr_api::error::RenderError;
use krkr_api::geometry::PixelRegion;
use krkr_api::host::{Presentation, SurfaceEvent};

use crate::scene::RenderScene;
use crate::texture::TextureDesc;

/// Number of swapchain images recorded concurrently.
pub const MAX_FRAMES_IN_FLIGHT: usize = 2;

/// Immutable Vulkan mechanism held by [`crate::RenderPipeline`].
///
/// The value contains only handles and configuration. Every resource that
/// changes per caller lives in [`VulkanState`].
pub struct VulkanPipeline {
    /// Loaded Vulkan entry points.
    pub entry: ash::Entry,
    /// Vulkan instance.
    pub instance: ash::Instance,
    /// Selected physical device.
    pub physical_device: vk::PhysicalDevice,
    /// Logical device.
    pub device: ash::Device,
    /// Queue used for graphics work.
    pub graphics_queue: vk::Queue,
    /// Queue used for presentation.
    pub present_queue: vk::Queue,
    /// Index of the queue family backing both queues.
    pub queue_family_index: u32,
    /// Whether the debug validation layer is enabled.
    pub validation: bool,
}

impl VulkanPipeline {
    /// Tries to build the Vulkan mechanism for `config`.
    ///
    /// The Round 1 skeleton returns [`RenderError::Unsupported`]; the full
    /// implementation is Phase 5.
    pub fn new(_config: &crate::PipelineConfig) -> Result<Self, RenderError> {
        Err(RenderError::Unsupported {
            message: "the Vulkan backend is not implemented yet (Phase 5)".to_owned(),
        })
    }

    /// Returns the fixed capabilities advertised by the Vulkan backend.
    pub fn capabilities(&self) -> crate::GpuCapabilities {
        crate::GpuCapabilities {
            backend: crate::BackendKind::Vulkan,
            max_texture_dimension: 16384,
            preferred_format: crate::TextureFormat::Rgba8Srgb,
            supports_destination_dependent_blend: true,
            supports_offscreen_targets: true,
            supports_glyph_atlas: true,
        }
    }
}

/// Immutable Vulkan surface plus its swapchain.
pub struct VulkanSurface {
    /// Presented Vulkan surface.
    pub surface: vk::SurfaceKHR,
    /// Swapchain loader for the owning device.
    pub swapchain_loader: ash::khr::swapchain::Device,
    /// Swapchain handle.
    pub swapchain: vk::SwapchainKHR,
    /// Swapchain images.
    pub images: Vec<vk::Image>,
    /// Chosen surface format.
    pub format: vk::SurfaceFormatKHR,
    /// Current swapchain extent.
    pub extent: vk::Extent2D,
}

/// Mutable Vulkan state owned by the middle layer.
pub struct VulkanState {
    /// Current surface size in physical pixels.
    pub surface_size: (u32, u32),
    /// Number of frames recorded since the state was created.
    pub frame_index: u64,
    /// Per-frame-in-flight command and synchronisation objects.
    pub frames: Vec<VulkanFrame>,
    /// Texture resources keyed by [`crate::Texture`] identity.
    pub textures: std::collections::HashMap<usize, VulkanTexture>,
}

/// Command and synchronisation objects for one frame in flight.
pub struct VulkanFrame {
    /// Command pool used to allocate `command_buffer`.
    pub command_pool: vk::CommandPool,
    /// Primary command buffer recorded each frame.
    pub command_buffer: vk::CommandBuffer,
    /// Fence signalled when `command_buffer` finishes.
    pub in_flight: vk::Fence,
    /// Semaphore signalled when a swapchain image is acquired.
    pub image_available: vk::Semaphore,
}

/// GPU image, view and memory for one texture.
pub struct VulkanTexture {
    /// Texture descriptor.
    pub desc: TextureDesc,
    /// Image handle.
    pub image: vk::Image,
    /// Image view.
    pub view: vk::ImageView,
    /// Backing device memory.
    pub memory: vk::DeviceMemory,
}

/// Creates a Vulkan surface for `window` (unimplemented skeleton).
///
/// # Errors
///
/// Always returns [`RenderError::Unsupported`] until Phase 5.
pub fn create_surface(
    _pipeline: &VulkanPipeline,
    _window: &RawWindowHandle,
) -> Result<VulkanSurface, RenderError> {
    Err(RenderError::Unsupported {
        message: "Vulkan surface creation is not implemented yet (Phase 5)".to_owned(),
    })
}

/// Recreates the swapchain for a new size (unimplemented skeleton).
///
/// # Errors
///
/// Always returns [`RenderError::Unsupported`] until Phase 5.
pub fn resize(
    _pipeline: &VulkanPipeline,
    _surface: &VulkanSurface,
    _state: &mut VulkanState,
    _size: (u32, u32),
) -> Result<SurfaceEvent, RenderError> {
    Err(RenderError::Unsupported {
        message: "Vulkan resize is not implemented yet (Phase 5)".to_owned(),
    })
}

/// Uploads a sub-rectangle of pixels into a texture (unimplemented skeleton).
///
/// # Errors
///
/// Always returns [`RenderError::Unsupported`] until Phase 5.
pub fn upload_texture(
    _pipeline: &VulkanPipeline,
    _state: &mut VulkanState,
    _tex: &crate::Texture,
    _region: PixelRegion,
    _pixels: &[u8],
) -> Result<(), RenderError> {
    Err(RenderError::Unsupported {
        message: "Vulkan texture upload is not implemented yet (Phase 5)".to_owned(),
    })
}

/// Records and presents one frame (unimplemented skeleton).
///
/// # Errors
///
/// Always returns [`RenderError::Unsupported`] until Phase 5.
pub fn render(
    _pipeline: &VulkanPipeline,
    _surface: &VulkanSurface,
    _state: &mut VulkanState,
    _scene: &RenderScene,
) -> Result<Presentation, RenderError> {
    Err(RenderError::Unsupported {
        message: "Vulkan rendering is not implemented yet (Phase 5)".to_owned(),
    })
}
