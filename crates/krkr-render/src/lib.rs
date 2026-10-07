//! krkr-rs bottom render pipeline.
//!
//! This crate implements the L1 half of the render contract in
//! `docs/migration/03-middle-layer-api.md` §2:
//!
//! * the abstract [`RenderScene`] tree, which the middle layer owns and
//!   keeps current;
//! * the stateless [`RenderPipeline`] service, created by
//!   [`create_render_pipeline`];
//! * the explicit [`RenderState`] value and [`Surface`] handle, created by
//!   [`begin_render_state`] and stored by the middle layer;
//! * the [`Texture`] object and its upload/drop actions;
//! * the single [`render`] action, which performs all traversal,
//!   composition, batching and drawing;
//! * the stateless [`FontSystem`] and [`Font`] values with the pure
//!   [`rasterize_glyph`], [`glyph_advance`] and [`font_metrics`] functions
//!   (`03-middle-layer-api.md` §3).
//!
//! Round 1 ships the whole API plus a fully functional headless
//! [`NullRenderPipeline`]. The Vulkan (`ash`) backend in [`vulkan`] is a real
//! implementation: instance/device creation, `ash-window` surface creation,
//! swapchain management, staging uploads, fixed-function blend pipelines and
//! scene-tree traversal and presentation. See
//! `docs/migration/04-renderer-vulkan.md`.
//!
//! The pipeline keeps no mutable state: every call takes the evolving
//! [`RenderState`] as an explicit `&mut` parameter, and the scene is
//! consumed by reference. Two independent pipelines can therefore run in one
//! process.

#![warn(missing_docs)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod glyph;
mod null;
pub mod scene;
mod state;
pub mod texture;
pub mod vulkan;

use std::fmt;

use krkr_api::error::RenderError;
use krkr_api::geometry::PixelRegion;
use krkr_api::host::{Presentation, SurfaceEvent};
use raw_window_handle::{RawDisplayHandle, RawWindowHandle};

pub use crate::glyph::{
    Font, FontSystem, create_font_system, font_metrics, glyph_advance, load_font, rasterize_glyph,
};
pub use crate::null::{
    FrameStats, MAX_TRAVERSAL_DEPTH, NULL_MAX_TEXTURE_DIMENSION, NullRenderPipeline,
};
pub use crate::scene::{
    Clip, GlyphPlacement, GlyphRun, RenderContent, RenderNode, RenderNodeId, RenderScene,
};
pub use crate::state::{RenderState, Surface};
pub use crate::texture::{Texture, TextureDesc, TextureFormat};
pub use krkr_api::glyph::{FontMetrics, GlyphBitmap, GlyphStyle};

use crate::vulkan::VulkanPipeline;

/// Which render backend a [`RenderPipeline`] uses.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum BackendKind {
    /// Headless null backend; needs neither a GPU nor a window.
    #[default]
    Null,
    /// Vulkan (`ash`) backend.
    Vulkan,
}

impl BackendKind {
    /// Returns the backend name for logging and diagnostics.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Vulkan => "vulkan",
        }
    }
}

/// Immutable configuration for [`create_render_pipeline`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PipelineConfig {
    /// Backend to create.
    pub backend: BackendKind,
    /// Application name passed to the graphics API.
    pub application_name: String,
    /// Preferred swapchain/image format.
    pub preferred_format: TextureFormat,
    /// Whether backend validation should be enabled.
    pub validation: bool,
}

impl Default for PipelineConfig {
    fn default() -> Self {
        Self {
            backend: BackendKind::Null,
            application_name: "krkr-rs".to_owned(),
            preferred_format: TextureFormat::Rgba8Srgb,
            validation: false,
        }
    }
}

impl PipelineConfig {
    /// Returns the default headless configuration.
    pub fn null() -> Self {
        Self::default()
    }

    /// Returns a Vulkan configuration derived from the default.
    pub fn vulkan() -> Self {
        Self {
            backend: BackendKind::Vulkan,
            ..Self::default()
        }
    }
}

/// Static capabilities reported by a [`RenderPipeline`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuCapabilities {
    /// Backend these capabilities describe.
    pub backend: BackendKind,
    /// Maximum texture width/height in pixels.
    pub max_texture_dimension: u32,
    /// Preferred texture format.
    pub preferred_format: TextureFormat,
    /// Whether destination-dependent blend modes are supported.
    pub supports_destination_dependent_blend: bool,
    /// Whether offscreen compositing targets are supported.
    pub supports_offscreen_targets: bool,
    /// Whether GPU glyph-atlas uploads are supported.
    pub supports_glyph_atlas: bool,
}

/// Stateless render service.
///
/// Created once through [`create_render_pipeline`] and shared by reference.
/// Every render action takes the caller-owned [`RenderState`] as a `&mut`
/// parameter, so the pipeline itself never remembers anything between calls.
pub struct RenderPipeline {
    kind: PipelineKind,
}

/// Backend-specific mechanism of a [`RenderPipeline`].
enum PipelineKind {
    /// Headless null backend.
    Null(NullRenderPipeline),
    /// Vulkan backend.
    Vulkan(Box<VulkanPipeline>),
}

impl RenderPipeline {
    /// Creates a headless pipeline backed by [`NullRenderPipeline`].
    pub fn null() -> Self {
        Self {
            kind: PipelineKind::Null(NullRenderPipeline::new()),
        }
    }

    /// Returns the backend this pipeline uses.
    pub fn backend(&self) -> BackendKind {
        match self.kind {
            PipelineKind::Null(_) => BackendKind::Null,
            PipelineKind::Vulkan(_) => BackendKind::Vulkan,
        }
    }
}

impl Default for RenderPipeline {
    fn default() -> Self {
        Self::null()
    }
}

impl fmt::Debug for RenderPipeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RenderPipeline")
            .field("backend", &self.backend())
            .finish()
    }
}

/// Creates a render pipeline for `config`.
///
/// # Errors
///
/// Returns [`RenderError::Unsupported`] when the requested backend is not
/// implemented in this build (currently the Vulkan backend).
pub fn create_render_pipeline(config: PipelineConfig) -> Result<RenderPipeline, RenderError> {
    match config.backend {
        BackendKind::Null => Ok(RenderPipeline::null()),
        BackendKind::Vulkan => {
            let backend = VulkanPipeline::new(&config)?;
            Ok(RenderPipeline {
                kind: PipelineKind::Vulkan(Box::new(backend)),
            })
        }
    }
}

/// Returns the static capabilities of `p`.
pub fn capabilities(p: &RenderPipeline) -> GpuCapabilities {
    match &p.kind {
        PipelineKind::Null(backend) => backend.capabilities(),
        PipelineKind::Vulkan(backend) => backend.capabilities(),
    }
}

/// Creates the surface and render state for `window`.
///
/// The middle layer stores both values and passes them to every subsequent
/// render action.
///
/// # Errors
///
/// Returns [`RenderError::Unsupported`] when the backend is not implemented
/// in this build.
pub fn begin_render_state(
    p: &RenderPipeline,
    window: &RawWindowHandle,
) -> Result<(Surface, RenderState), RenderError> {
    match &p.kind {
        PipelineKind::Null(backend) => backend.begin(window),
        PipelineKind::Vulkan(backend) => backend.begin(window),
    }
}

/// Creates the surface and render state from an explicit display handle.
///
/// `VK_KHR_surface` needs the platform display connection (the X11 `Display*`,
/// the XCB connection or the Wayland `wl_display`) in addition to the window
/// handle. `raw-window-handle` splits those into [`RawDisplayHandle`] and
/// [`RawWindowHandle`], but [`begin_render_state`] only receives the window
/// handle. This function is the complete entry point for X11, XCB and
/// Wayland; the null backend ignores `display`.
///
/// The middle layer stores both returned values and passes them to every
/// subsequent render action.
///
/// # Errors
///
/// Returns [`RenderError::Unsupported`] when the backend cannot create a
/// surface for the supplied handles.
pub fn begin_render_state_with_display(
    p: &RenderPipeline,
    display: &RawDisplayHandle,
    window: &RawWindowHandle,
) -> Result<(Surface, RenderState), RenderError> {
    match &p.kind {
        PipelineKind::Null(backend) => backend.begin(window),
        PipelineKind::Vulkan(backend) => backend.begin_with_display(display, window),
    }
}

/// Reports a new surface size and updates `state`.
///
/// # Errors
///
/// Returns [`RenderError::InvalidArgument`] when `state`/`surface` do not
/// belong to `p`, and [`RenderError::Unsupported`] for an unimplemented
/// backend.
pub fn resize(
    p: &RenderPipeline,
    surface: &Surface,
    state: &mut RenderState,
    size: (u32, u32),
) -> Result<SurfaceEvent, RenderError> {
    match &p.kind {
        PipelineKind::Null(backend) => backend.resize(surface, state, size),
        PipelineKind::Vulkan(backend) => backend.resize(surface, state, size),
    }
}

/// Creates a texture whose resource will live in `state`.
///
/// # Errors
///
/// Returns [`RenderError::InvalidArgument`] for an empty or oversized
/// descriptor, and [`RenderError::Unsupported`] for an unimplemented backend.
pub fn create_texture(p: &RenderPipeline, desc: TextureDesc) -> Result<Texture, RenderError> {
    match &p.kind {
        PipelineKind::Null(backend) => backend.create_texture(desc),
        PipelineKind::Vulkan(backend) => backend.create_texture(desc),
    }
}

/// Releases the render state slot associated with `tex`.
///
/// The handle may still be referenced by a [`RenderScene`]; such a draw is
/// simply skipped by the backend.
pub fn drop_texture(p: &RenderPipeline, state: &mut RenderState, tex: Texture) {
    match &p.kind {
        PipelineKind::Null(backend) => backend.drop_texture(state, tex),
        PipelineKind::Vulkan(backend) => backend.drop_texture(state, tex),
    }
}

/// Copies a sub-rectangle of pixels into `tex`.
///
/// `pixels` is tightly packed, row-major, in the texture's format.
///
/// # Errors
///
/// Returns [`RenderError::Upload`] when `region` is out of bounds or
/// `pixels` does not match the region size, and [`RenderError::Unsupported`]
/// for an unimplemented backend.
pub fn upload_texture(
    p: &RenderPipeline,
    state: &mut RenderState,
    tex: &Texture,
    region: PixelRegion,
    pixels: &[u8],
) -> Result<(), RenderError> {
    match &p.kind {
        PipelineKind::Null(backend) => backend.upload_texture(state, tex, region, pixels),
        PipelineKind::Vulkan(backend) => backend.upload_texture(state, tex, region, pixels),
    }
}

/// Renders `scene` into `surface`, mutating `state`.
///
/// This is the only drawing entry point. The pipeline owns traversal,
/// composition, batching and presentation; the caller only keeps the tree
/// current.
///
/// # Errors
///
/// Returns [`RenderError::InvalidArgument`] for a malformed scene or a
/// mismatched state/surface, and [`RenderError::Unsupported`] for an
/// unimplemented backend.
pub fn render(
    p: &RenderPipeline,
    surface: &Surface,
    state: &mut RenderState,
    scene: &RenderScene,
) -> Result<Presentation, RenderError> {
    match &p.kind {
        PipelineKind::Null(backend) => backend.render(surface, state, scene),
        PipelineKind::Vulkan(backend) => backend.render(surface, state, scene),
    }
}
