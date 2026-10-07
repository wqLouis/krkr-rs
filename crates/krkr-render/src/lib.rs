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
//! [`NullRenderPipeline`]. The Vulkan (`ash`) backend lives in [`vulkan`] as
//! a typed skeleton for Phase 5. See `docs/migration/04-renderer-vulkan.md`.
//!
//! The pipeline keeps no mutable state: every call takes the evolving
//! [`RenderState`] as an explicit `&mut` parameter, and the scene is
//! consumed by reference. Two independent pipelines can therefore run in one
//! process.

#![warn(missing_docs)]

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
use raw_window_handle::RawWindowHandle;

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
pub use crate::vulkan::VulkanPipeline;
pub use krkr_api::glyph::{FontMetrics, GlyphBitmap, GlyphStyle};

/// Message returned by every operation of the unfinished Vulkan backend.
const VULKAN_UNIMPLEMENTED: &str = "the Vulkan backend is not implemented yet (Phase 5)";

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

impl From<NullRenderPipeline> for RenderPipeline {
    fn from(backend: NullRenderPipeline) -> Self {
        Self {
            kind: PipelineKind::Null(backend),
        }
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
        PipelineKind::Vulkan(_) => Err(unsupported()),
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
        PipelineKind::Vulkan(_) => Err(unsupported()),
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
        PipelineKind::Vulkan(_) => Err(unsupported()),
    }
}

/// Releases the render state slot associated with `tex`.
///
/// The handle may still be referenced by a [`RenderScene`]; such a draw is
/// simply skipped by the backend.
pub fn drop_texture(p: &RenderPipeline, state: &mut RenderState, tex: Texture) {
    match &p.kind {
        PipelineKind::Null(backend) => backend.drop_texture(state, tex),
        PipelineKind::Vulkan(_) => drop(tex),
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
        PipelineKind::Vulkan(_) => Err(unsupported()),
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
        PipelineKind::Vulkan(_) => Err(unsupported()),
    }
}

/// Builds the shared "Vulkan is not implemented" error.
fn unsupported() -> RenderError {
    RenderError::Unsupported {
        message: VULKAN_UNIMPLEMENTED.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use krkr_api::geometry::RectF;
    use raw_window_handle::XlibWindowHandle;

    fn headless_window() -> RawWindowHandle {
        RawWindowHandle::Xlib(XlibWindowHandle::new(0))
    }

    #[test]
    fn default_config_is_headless() {
        let config = PipelineConfig::default();
        assert_eq!(config.backend, BackendKind::Null);
        assert_eq!(config.preferred_format, TextureFormat::Rgba8Srgb);

        let pipeline = create_render_pipeline(config).expect("null pipeline");
        assert_eq!(pipeline.backend(), BackendKind::Null);
        assert_eq!(pipeline.backend().name(), "null");

        let caps = capabilities(&pipeline);
        assert_eq!(caps.backend, BackendKind::Null);
        assert_eq!(caps.max_texture_dimension, NULL_MAX_TEXTURE_DIMENSION);
    }

    #[test]
    fn vulkan_config_reports_unimplemented() {
        let error = create_render_pipeline(PipelineConfig::vulkan()).unwrap_err();
        assert!(matches!(error, RenderError::Unsupported { .. }));

        // A null pipeline still reports Vulkan capabilities for the vulkan arm.
        assert_eq!(BackendKind::Vulkan.name(), "vulkan");
    }

    #[test]
    fn scene_construction_links_parent_and_children() {
        let mut scene = RenderScene::new(RenderNodeId(0));
        assert!(scene.is_empty());

        let root = scene.push(RenderNode {
            content: RenderContent::Solid([1.0, 0.0, 0.0, 1.0]),
            ..RenderNode::default()
        });
        assert_eq!(root, RenderNodeId(0));
        assert_eq!(scene.len(), 1);

        let child = scene.push(RenderNode {
            parent: Some(root),
            content: RenderContent::None,
            dst: RectF::new(1.0, 2.0, 3.0, 4.0),
            z: 7,
            ..RenderNode::default()
        });
        scene.node_mut(root).expect("root").children.push(child);

        assert_eq!(scene.node(child).unwrap().parent, Some(root));
        assert_eq!(scene.node(root).unwrap().children, vec![child]);
        assert_eq!(
            scene.node(child).unwrap().dst,
            RectF::new(1.0, 2.0, 3.0, 4.0)
        );
        assert_eq!(scene.node(child).unwrap().z, 7);
        assert!(scene.node(RenderNodeId(99)).is_none());

        scene.background = Some([0.0, 0.0, 0.0, 1.0]);
        assert!(scene.background.is_some());
    }

    #[test]
    fn null_pipeline_round_trip_counts_draws() {
        let pipeline = RenderPipeline::null();
        let (surface, mut state) =
            begin_render_state(&pipeline, &headless_window()).expect("begin");

        let event = resize(&pipeline, &surface, &mut state, (1280, 720)).expect("resize");
        assert_eq!(event, SurfaceEvent::Resized(1280, 720));

        let texture = create_texture(&pipeline, TextureDesc::new(4, 2)).expect("texture");
        assert_eq!(texture.desc().base_byte_len(), 4 * 2 * 4);
        let pixels = [255u8; 4 * 2 * 4];
        upload_texture(
            &pipeline,
            &mut state,
            &texture,
            PixelRegion::new(0, 0, 4, 2),
            &pixels,
        )
        .expect("upload");
        assert_eq!(
            NullRenderPipeline::texture_pixels(&state, &texture).map(|p| p.len()),
            Some(32)
        );
        assert_eq!(
            NullRenderPipeline::texture_uploaded_bytes(&state, &texture),
            Some(32)
        );

        let mut scene = RenderScene::new(RenderNodeId(0));
        let root = scene.push(RenderNode {
            content: RenderContent::Texture(texture.clone()),
            ..RenderNode::default()
        });
        let solid = scene.push(RenderNode {
            parent: Some(root),
            content: RenderContent::Solid([0.0, 1.0, 0.0, 1.0]),
            z: 2,
            ..RenderNode::default()
        });
        let hidden = scene.push(RenderNode {
            parent: Some(root),
            content: RenderContent::None,
            visible: false,
            z: 1,
            ..RenderNode::default()
        });
        scene.node_mut(root).expect("root").children = vec![solid, hidden];
        scene.background = Some([0.0, 0.0, 0.0, 1.0]);

        let presentation = render(&pipeline, &surface, &mut state, &scene).expect("render");
        assert!(presentation.presented);
        assert_eq!(presentation.surface_event, None);

        let stats = NullRenderPipeline::last_frame(&state).expect("stats");
        assert_eq!(stats.nodes_visited, 3);
        assert_eq!(stats.draws, 2);
        assert_eq!(stats.texture_draws, 1);
        assert_eq!(stats.solid_draws, 1);
        assert_eq!(stats.invisible, 1);
        assert_eq!(stats.missing_textures, 0);
        assert_eq!(stats.max_depth, 1);

        // Dropping removes the state slot without invalidating clones.
        let handle = texture.clone();
        drop_texture(&pipeline, &mut state, texture);
        assert!(NullRenderPipeline::texture_pixels(&state, &handle).is_none());
    }

    #[test]
    fn partial_upload_touches_only_the_region() {
        let pipeline = RenderPipeline::null();
        let (_surface, mut state) =
            begin_render_state(&pipeline, &headless_window()).expect("begin");
        let texture = create_texture(&pipeline, TextureDesc::new(4, 4)).expect("texture");

        let pixels = [7u8; 8];
        upload_texture(
            &pipeline,
            &mut state,
            &texture,
            PixelRegion::new(1, 2, 2, 1),
            &pixels,
        )
        .expect("upload");

        let stored = NullRenderPipeline::texture_pixels(&state, &texture).expect("stored");
        let offset = (2 * 4 + 1) * 4;
        assert_eq!(&stored[offset..offset + 8], &[7u8; 8]);
        assert_eq!(&stored[0..4], &[0u8; 4]);
    }

    #[test]
    fn upload_validates_region_and_length() {
        let pipeline = RenderPipeline::null();
        let (_surface, mut state) =
            begin_render_state(&pipeline, &headless_window()).expect("begin");
        let texture = create_texture(&pipeline, TextureDesc::new(4, 4)).expect("texture");

        let out_of_bounds = upload_texture(
            &pipeline,
            &mut state,
            &texture,
            PixelRegion::new(3, 0, 2, 1),
            &[0u8; 8],
        );
        assert!(matches!(out_of_bounds, Err(RenderError::Upload { .. })));

        let wrong_len = upload_texture(
            &pipeline,
            &mut state,
            &texture,
            PixelRegion::new(0, 0, 2, 1),
            &[0u8; 4],
        );
        assert!(matches!(wrong_len, Err(RenderError::Upload { .. })));

        let empty = upload_texture(&pipeline, &mut state, &texture, PixelRegion::ZERO, &[]);
        assert!(matches!(empty, Err(RenderError::Upload { .. })));
    }

    #[test]
    fn create_texture_rejects_invalid_descriptors() {
        let pipeline = RenderPipeline::null();
        assert!(matches!(
            create_texture(&pipeline, TextureDesc::new(0, 4)),
            Err(RenderError::InvalidArgument { .. })
        ));
        assert!(matches!(
            create_texture(&pipeline, TextureDesc::new(4, 0)),
            Err(RenderError::InvalidArgument { .. })
        ));
        assert!(matches!(
            create_texture(&pipeline, TextureDesc::new(4, 4).with_mip_levels(0)),
            Err(RenderError::InvalidArgument { .. })
        ));
        assert!(matches!(
            create_texture(&pipeline, TextureDesc::new(u32::MAX, 1)),
            Err(RenderError::InvalidArgument { .. })
        ));
    }

    #[test]
    fn glyph_scene_construction() {
        let run = GlyphRun {
            atlas: None,
            size_px: 16.0,
            color: [1.0, 1.0, 1.0, 1.0],
            glyphs: vec![
                GlyphPlacement::new('A', 0.0, 0.0, 9.0, PixelRegion::new(0, 0, 8, 12)),
                GlyphPlacement::new('B', 9.0, 0.0, 9.0, PixelRegion::new(8, 0, 8, 12)),
            ],
        };
        assert_eq!(run.len(), 2);
        assert!(!run.is_empty());

        let node = RenderNode {
            content: RenderContent::Glyphs(run),
            ..RenderNode::default()
        };
        assert!(matches!(node.content, RenderContent::Glyphs(_)));
    }
}
