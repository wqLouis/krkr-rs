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
#[allow(dead_code)] // The Vulkan variant is staged for Phase 5.
pub(crate) enum StateKind {
    /// Headless null backend.
    Null(Box<NullState>),
    /// Vulkan backend.
    Vulkan(Box<VulkanState>),
}

/// Immutable presentation-surface handle.
///
/// The current extent lives in [`RenderState`] because [`crate::resize`]
/// only receives `&Surface`.
pub struct Surface {
    pub(crate) kind: SurfaceKind,
}

/// Backend-specific payload of a [`Surface`].
#[allow(dead_code)] // The Vulkan variant is staged for Phase 5.
pub(crate) enum SurfaceKind {
    /// Headless null backend.
    Null(NullSurface),
    /// Vulkan backend.
    Vulkan(VulkanSurface),
}
