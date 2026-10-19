//! L1-defined, L2-owned render state values.
//!
//! [`RenderState`] and [`Surface`] are opaque to the middle layer: it creates
//! them through [`crate::begin_render_state`], stores them next to the render
//! tree and passes them back to every render action. The render pipeline
//! itself keeps no mutable state.

use crate::null::{NullState, NullSurface};
use crate::vulkan::{VulkanState, VulkanSurface};

/// Opaque render state created by [`crate::begin_render_state`].
///
/// It holds the swapchain/staging/texture bookkeeping for one caller. The
/// middle layer stores the value and passes `&mut` to every render action.
pub struct RenderState {
    pub(crate) kind: StateKind,
}

/// Backend-specific payload of a [`RenderState`].
pub(crate) enum StateKind {
    /// Headless null backend.
    Null(Box<NullState>),
    /// Vulkan backend.
    Vulkan(Box<VulkanState>),
}

/// Presentation-surface handle.
///
/// The current extent lives in [`RenderState`] because [`crate::resize`]
/// only receives `&Surface`. The backend may replace the underlying
/// `VkSurfaceKHR` when a lost surface is recovered, which is why
/// [`crate::recover_surface`] takes it mutably.
pub struct Surface {
    pub(crate) kind: SurfaceKind,
}

/// Backend-specific payload of a [`Surface`].
pub(crate) enum SurfaceKind {
    /// Headless null backend.
    Null(NullSurface),
    /// Vulkan backend.
    Vulkan(VulkanSurface),
}
