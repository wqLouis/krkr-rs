//! Headless (`Null`) render backend.
//!
//! The null backend accepts the entire render API and tracks scene traversal
//! and texture uploads in [`RenderState`], but never touches a GPU or a
//! window. It exists so the middle layer and the test suite can run without
//! a device. It also ships a real CPU software rasterizer for offscreen
//! render targets, which is the correctness baseline for
//! destination-dependent blends and pixel readback.

use api::error::RenderError;
use api::geometry::PixelRegion;
use api::host::{Presentation, SurfaceEvent};
use raw_window_handle::RawWindowHandle;

use crate::collections::Map;
use crate::cpu::{CpuTexture, TextureProvider};
use crate::effect::EffectPipeline;
use crate::render_target::{RenderTarget, RenderTargetDesc};
use crate::scene::{
    RenderContent, RenderNode, RenderNodeId, RenderScene, child_sort_key, children_are_sorted,
};
use crate::state::{RenderState, StateKind, Surface, SurfaceKind};
use crate::texture::{Texture, TextureDesc};
use crate::{BackendKind, FrameTimings, GpuCapabilities, MAX_TRAVERSAL_DEPTH, TextureFormat};

/// Maximum width/height advertised by null backend.
///
/// The limit belongs to this backend: a real device reports its own limit
/// through
/// [`GpuCapabilities::max_texture_dimension`](crate::GpuCapabilities::max_texture_dimension).
pub const NULL_MAX_TEXTURE_DIMENSION: u32 = 16384;

/// Per-frame traversal counters produced by [`NullRenderPipeline::render`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameStats {
    /// Nodes encountered, including invisible ones.
    pub nodes_visited: u32,
    /// Visible nodes with a drawable payload.
    pub draws: u32,
    /// Draws of [`RenderContent::Solid`].
    pub solid_draws: u32,
    /// Draws of [`RenderContent::Texture`].
    pub texture_draws: u32,
    /// Draws of [`RenderContent::Glyphs`].
    pub glyph_draws: u32,
    /// Draws of [`RenderContent::Effect`].
    pub effect_draws: u32,
    /// Invisible nodes whose subtree was skipped.
    pub invisible: u32,
    /// Texture draws whose texture had not been uploaded.
    pub missing_textures: u32,
    /// Depth of the deepest visited node.
    pub max_depth: u32,
}

/// Stateless headless render pipeline.
///
/// It is the test double for the Vulkan pipeline: presentation frames are
/// accepted with CPU-side bookkeeping only, while offscreen render targets
/// are composited by a real CPU software rasterizer.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullRenderPipeline;

impl NullRenderPipeline {
    /// Creates a null render pipeline.
    pub const fn new() -> Self {
        Self
    }

    /// Returns the fixed capabilities advertised by the null backend.
    pub fn capabilities(&self) -> GpuCapabilities {
        GpuCapabilities {
            backend: BackendKind::Null,
            max_texture_dimension: NULL_MAX_TEXTURE_DIMENSION,
            preferred_format: TextureFormat::Rgba8Srgb,
            // The null backend ships a real CPU software rasterizer for
            // offscreen targets, so it can evaluate destination-dependent
            // blends (and read pixels back) without a GPU: the
            // CPU-correctness baseline, not a silently-degraded GPU path.
            supports_destination_dependent_blend: true,
            supports_offscreen_targets: true,
            supports_glyph_atlas: false,
            // The headless backend has no device, so it never executes a
            // custom-shader pipeline; the caller keeps its own CPU path.
            supports_effect_pipelines: false,
        }
    }

    /// Creates a headless surface and its state, ignoring `window`.
    ///
    /// The null backend has no window, so the handle is accepted and
    /// discarded; the returned pair is a surface and the state that owns it.
    ///
    /// # Errors
    ///
    /// Never: the null backend can always produce a surface.
    pub fn begin(&self, _window: &RawWindowHandle) -> Result<(Surface, RenderState), RenderError> {
        let surface = Surface {
            kind: SurfaceKind::Null(NullSurface),
        };
        let state = RenderState {
            kind: StateKind::Null(Box::default()),
        };
        Ok((surface, state))
    }

    /// Updates the tracked surface size.
    ///
    /// The size is recorded in the state; the null backend never presents it.
    ///
    /// # Errors
    ///
    /// [`RenderError::InvalidArgument`] when the state does not belong to
    /// this backend.
    pub fn resize(
        &self,
        surface: &Surface,
        state: &mut RenderState,
        size: (u32, u32),
    ) -> Result<SurfaceEvent, RenderError> {
        check_surface(surface)?;
        let null = null_state_mut(state)?;
        null.surface_size = size;
        Ok(SurfaceEvent::Resized(size.0, size.1))
    }

    /// Recreates a lost surface; the null backend never loses one.
    ///
    /// # Errors
    ///
    /// [`RenderError::InvalidArgument`] when the state does not belong to
    /// this backend.
    pub fn recover_surface(
        &self,
        surface: &Surface,
        _state: &mut RenderState,
    ) -> Result<SurfaceEvent, RenderError> {
        check_surface(surface)?;
        Ok(SurfaceEvent::Restored)
    }

    /// Creates a texture handle after validating its descriptor.
    ///
    /// The handle owns no storage yet; the pixels arrive through
    /// [`upload_texture`](Self::upload_texture).
    ///
    /// # Errors
    ///
    /// [`RenderError::InvalidArgument`] when the descriptor is empty,
    /// oversized for the backend or otherwise malformed.
    pub fn create_texture(&self, desc: TextureDesc) -> Result<Texture, RenderError> {
        validate_desc(&desc)?;
        Ok(Texture::new(desc))
    }

    /// Refuses to create an effect pipeline: the headless backend does not run
    /// custom shaders, so an effect draw would have no meaning here.
    ///
    /// # Errors
    ///
    /// Always [`RenderError::Unsupported`].
    pub fn create_effect_pipeline(
        &self,
        _state: &mut RenderState,
        desc: &api::shader::EffectPipelineDesc<'_>,
    ) -> Result<EffectPipeline, RenderError> {
        Err(RenderError::Unsupported {
            message: format!(
                "the headless backend does not run effect pipelines (program {:?})",
                desc.name
            ),
        })
    }

    /// Accepts the release of an effect pipeline handle; none can exist on this
    /// backend.
    ///
    /// The call is a no-op because
    /// [`create_effect_pipeline`](Self::create_effect_pipeline) always
    /// refuses, so there is never a handle to release.
    pub fn destroy_effect_pipeline(&self, _state: &mut RenderState, _pipeline: EffectPipeline) {}

    /// Releases the state slot associated with `tex`, if any.
    pub fn drop_texture(&self, state: &mut RenderState, tex: Texture) {
        if let StateKind::Null(null) = &mut state.kind {
            null.textures.remove(&tex.key());
        }
    }

    /// Copies a sub-rectangle of pixels into the texture's CPU storage.
    ///
    /// The pixels are tightly packed and row-major for `region`.
    ///
    /// # Errors
    ///
    /// [`RenderError::InvalidArgument`] when the state does not belong to
    /// this backend, and [`RenderError::Upload`] when the region is out of
    /// bounds or the pixel slice does not match it.
    pub fn upload_texture(
        &self,
        state: &mut RenderState,
        tex: &Texture,
        region: PixelRegion,
        pixels: &[u8],
    ) -> Result<(), RenderError> {
        let null = null_state_mut(state)?;
        upload_into(null, tex, region, pixels)
    }

    /// Creates an offscreen render-target handle after validating its
    /// descriptor.
    ///
    /// The CPU storage is allocated when the target is first rendered into.
    ///
    /// # Errors
    ///
    /// [`RenderError::InvalidArgument`] when the descriptor is empty,
    /// oversized or not RGBA8.
    pub fn create_render_target(
        &self,
        desc: RenderTargetDesc,
    ) -> Result<RenderTarget, RenderError> {
        validate_target_desc(&desc)?;
        Ok(RenderTarget::new(desc))
    }

    /// Releases the CPU storage associated with `target`.
    pub fn destroy_render_target(&self, state: &mut RenderState, target: RenderTarget) {
        if let StateKind::Null(null) = &mut state.kind {
            null.render_targets.remove(&target.key());
        }
    }

    /// Renders `scene` into the CPU storage of `target`.
    ///
    /// The target's pixels are composited by the CPU software rasterizer, so
    /// the result is the correctness baseline for the destination-dependent
    /// blends.
    ///
    /// # Errors
    ///
    /// [`RenderError::InvalidArgument`] when the state does not belong to
    /// this backend, [`RenderError::Framebuffer`] when the target is not
    /// owned by the state, and [`RenderError::OutOfMemory`] when the target is
    /// too large for the host address space.
    pub fn render_to_target(
        &self,
        state: &mut RenderState,
        target: &RenderTarget,
        scene: &RenderScene,
    ) -> Result<(), RenderError> {
        let null = null_state_mut(state)?;
        let NullState {
            render_targets,
            textures,
            ..
        } = null;
        let image = target_storage(render_targets, target)?;
        let full = PixelRegion::new(0, 0, image.desc.width, image.desc.height);
        crate::cpu::render_scene(
            image.desc.width,
            image.desc.height,
            &mut image.pixels,
            scene.background,
            scene,
            textures,
            full,
        )
    }

    /// Renders `scene` into the `region` of the CPU storage of `target`.
    ///
    /// The pixels outside `region` keep the values they had, and the pixels
    /// inside are cleared to the scene's background and then composited,
    /// exactly as a full render produces them.
    ///
    /// # Errors
    ///
    /// [`RenderError::InvalidArgument`] when the state does not belong to
    /// this backend, [`RenderError::Framebuffer`] when the target is not
    /// owned by the state, and [`RenderError::OutOfMemory`] when the target is
    /// too large for the host address space.
    pub fn render_to_target_region(
        &self,
        state: &mut RenderState,
        target: &RenderTarget,
        scene: &RenderScene,
        region: PixelRegion,
    ) -> Result<(), RenderError> {
        let null = null_state_mut(state)?;
        let NullState {
            render_targets,
            textures,
            ..
        } = null;
        let image = target_storage(render_targets, target)?;
        crate::cpu::render_scene(
            image.desc.width,
            image.desc.height,
            &mut image.pixels,
            scene.background,
            scene,
            textures,
            region,
        )
    }

    /// Copies a sub-rectangle of `target` into `out`.
    ///
    /// `out` receives tightly packed, row-major pixels and must be exactly
    /// `region.area() * 4` bytes long.
    ///
    /// # Errors
    ///
    /// [`RenderError::InvalidArgument`] when the state does not belong to
    /// this backend, and [`RenderError::Framebuffer`] when the target is not
    /// owned by the state or the region does not match `out`.
    pub fn read_render_target(
        &self,
        state: &RenderState,
        target: &RenderTarget,
        region: PixelRegion,
        out: &mut [u8],
    ) -> Result<(), RenderError> {
        let null = null_state(state)?;
        let image =
            null.render_targets
                .get(&target.key())
                .ok_or_else(|| RenderError::Framebuffer {
                    message: "render target is not owned by this render state".to_owned(),
                })?;
        read_region(&image.desc, &image.pixels, region, out)
    }

    /// Traverses `scene` and records per-frame counters.
    ///
    /// The frame is always reported as presented: the null backend has no
    /// display to present to.
    ///
    /// # Errors
    ///
    /// [`RenderError::InvalidArgument`] when the surface or the state does
    /// not belong to this backend.
    pub fn render(
        &self,
        surface: &Surface,
        state: &mut RenderState,
        scene: &RenderScene,
    ) -> Result<Presentation, RenderError> {
        check_surface(surface)?;
        let null = null_state_mut(state)?;
        let stats = traverse(null, scene);
        null.frame_index = null.frame_index.wrapping_add(1);
        null.last_frame = Some(stats);
        null.last_timings = Some(FrameTimings::default());
        Ok(Presentation {
            presented: true,
            surface_event: None,
            skip: None,
        })
    }

    /// Returns the counters from the most recent null-backend frame.
    pub fn last_frame(state: &RenderState) -> Option<FrameStats> {
        match &state.kind {
            StateKind::Null(null) => null.last_frame,
            StateKind::Vulkan(_) => None,
        }
    }

    /// Returns the CPU storage for `tex`, when it has been uploaded.
    pub fn texture_pixels<'a>(state: &'a RenderState, tex: &Texture) -> Option<&'a [u8]> {
        match &state.kind {
            StateKind::Null(null) => null
                .textures
                .get(&tex.key())
                .map(|image| image.pixels.as_slice()),
            StateKind::Vulkan(_) => None,
        }
    }

    /// Returns the number of bytes uploaded for `tex` over its lifetime.
    pub fn texture_uploaded_bytes(state: &RenderState, tex: &Texture) -> Option<u64> {
        match &state.kind {
            StateKind::Null(null) => null
                .textures
                .get(&tex.key())
                .map(|image| image.bytes_uploaded),
            StateKind::Vulkan(_) => None,
        }
    }
}

/// Immutable null-backend surface handle.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NullSurface;

/// Mutable headless state owned by the middle layer.
#[derive(Debug, Default)]
pub(crate) struct NullState {
    /// Current surface size in pixels.
    pub(crate) surface_size: (u32, u32),
    /// Number of frames rendered through this state.
    pub(crate) frame_index: u64,
    /// CPU storage per texture identity.
    pub(crate) textures: Map<u64, NullImage>,
    /// CPU storage per offscreen render-target identity.
    pub(crate) render_targets: Map<u64, NullRenderTarget>,
    /// Counters from the most recent frame.
    pub(crate) last_frame: Option<FrameStats>,
    /// Vulkan call timings from the most recent frame; all zero because the
    /// null backend runs no GPU calls.
    pub(crate) last_timings: Option<FrameTimings>,
}

/// CPU-side image storage for one offscreen render target.
#[derive(Debug)]
pub(crate) struct NullRenderTarget {
    pub(crate) desc: RenderTargetDesc,
    pub(crate) pixels: Vec<u8>,
}

impl TextureProvider for Map<u64, NullImage> {
    fn texture(&self, key: u64) -> Option<CpuTexture<'_>> {
        self.get(&key).map(|image| CpuTexture {
            pixels: &image.pixels,
            width: image.desc.width,
            height: image.desc.height,
            format: image.desc.format,
        })
    }
}

/// CPU-side image storage for one texture.
#[derive(Debug)]
pub(crate) struct NullImage {
    pub(crate) desc: TextureDesc,
    pub(crate) pixels: Vec<u8>,
    pub(crate) uploads: u64,
    pub(crate) bytes_uploaded: u64,
}

/// Borrows the null payload, rejecting a mismatched backend.
fn null_state_mut(state: &mut RenderState) -> Result<&mut NullState, RenderError> {
    match &mut state.kind {
        StateKind::Null(null) => Ok(null),
        StateKind::Vulkan(_) => Err(RenderError::InvalidArgument {
            message: "the null backend received a Vulkan render state".to_owned(),
        }),
    }
}

/// Borrows the null payload immutably, rejecting a mismatched backend.
fn null_state(state: &RenderState) -> Result<&NullState, RenderError> {
    match &state.kind {
        StateKind::Null(null) => Ok(null),
        StateKind::Vulkan(_) => Err(RenderError::InvalidArgument {
            message: "the null backend received a Vulkan render state".to_owned(),
        }),
    }
}

/// Returns the CPU storage of `target`, allocating it on first use.
///
/// The storage is the target's pixels; the caller composites into them and
/// the content a later render leaves outside its region is kept.
fn target_storage<'a>(
    render_targets: &'a mut Map<u64, NullRenderTarget>,
    target: &RenderTarget,
) -> Result<&'a mut NullRenderTarget, RenderError> {
    if let hashbrown::hash_map::Entry::Vacant(entry) = render_targets.entry(target.key()) {
        let desc = *target.desc();
        let total = usize::try_from(desc.byte_len()).map_err(|_| RenderError::OutOfMemory {
            message: "render target is too large for the host address space".to_owned(),
        })?;
        entry.insert(NullRenderTarget {
            desc,
            pixels: vec![0; total],
        });
    }
    render_targets
        .get_mut(&target.key())
        .ok_or_else(|| RenderError::Framebuffer {
            message: "render target is not owned by this render state".to_owned(),
        })
}

/// Validates a render-target descriptor against the null backend's limits.
fn validate_target_desc(desc: &RenderTargetDesc) -> Result<(), RenderError> {
    if desc.is_empty() {
        return Err(RenderError::InvalidArgument {
            message: "render-target dimensions must be non-zero".to_owned(),
        });
    }
    if desc.width > NULL_MAX_TEXTURE_DIMENSION || desc.height > NULL_MAX_TEXTURE_DIMENSION {
        return Err(RenderError::InvalidArgument {
            message: format!(
                "render-target dimensions exceed the {NULL_MAX_TEXTURE_DIMENSION} pixel limit"
            ),
        });
    }
    if !desc.format.has_alpha() {
        return Err(RenderError::InvalidArgument {
            message: "an offscreen render target must use an alpha-capable RGBA8 format".to_owned(),
        });
    }
    Ok(())
}

/// Copies `region` from `pixels` into `out`.
fn read_region(
    desc: &RenderTargetDesc,
    pixels: &[u8],
    region: PixelRegion,
    out: &mut [u8],
) -> Result<(), RenderError> {
    let bytes_per_pixel = desc.format.bytes_per_pixel() as usize;
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
    let expected = (region.area() as usize)
        .checked_mul(bytes_per_pixel)
        .ok_or_else(|| RenderError::Framebuffer {
            message: "readback size overflows the host address space".to_owned(),
        })?;
    if out.len() != expected {
        return Err(RenderError::Framebuffer {
            message: format!(
                "readback buffer is {} bytes, expected {expected}",
                out.len()
            ),
        });
    }
    let row_bytes = region.width as usize * bytes_per_pixel;
    let stride = desc.width as usize * bytes_per_pixel;
    for row in 0..region.height as usize {
        let source = (region.y as usize + row) * stride + region.x as usize * bytes_per_pixel;
        let destination = row * row_bytes;
        out[destination..destination + row_bytes]
            .copy_from_slice(&pixels[source..source + row_bytes]);
    }
    Ok(())
}

/// Rejects a surface created by a different backend.
fn check_surface(surface: &Surface) -> Result<(), RenderError> {
    match &surface.kind {
        SurfaceKind::Null(_) => Ok(()),
        SurfaceKind::Vulkan(_) => Err(RenderError::InvalidArgument {
            message: "the null backend received a Vulkan surface".to_owned(),
        }),
    }
}

/// Validates a texture descriptor against the null backend's limits.
fn validate_desc(desc: &TextureDesc) -> Result<(), RenderError> {
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
    if desc.width > NULL_MAX_TEXTURE_DIMENSION || desc.height > NULL_MAX_TEXTURE_DIMENSION {
        return Err(RenderError::InvalidArgument {
            message: format!(
                "texture dimensions exceed the {NULL_MAX_TEXTURE_DIMENSION} pixel limit"
            ),
        });
    }
    Ok(())
}

/// Copies `pixels` into the stored image for `tex`.
fn upload_into(
    null: &mut NullState,
    tex: &Texture,
    region: PixelRegion,
    pixels: &[u8],
) -> Result<(), RenderError> {
    let desc = *tex.desc();
    let bytes_per_pixel = desc.format.bytes_per_pixel() as usize;
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
        .checked_mul(bytes_per_pixel)
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
    let total = usize::try_from(desc.base_byte_len()).map_err(|_| RenderError::OutOfMemory {
        message: "texture is too large for the host address space".to_owned(),
    })?;
    let image = null.textures.entry(tex.key()).or_insert_with(|| NullImage {
        desc,
        pixels: vec![0; total],
        uploads: 0,
        bytes_uploaded: 0,
    });
    if image.desc != desc {
        return Err(RenderError::Upload {
            message: "texture descriptor changed after creation".to_owned(),
        });
    }
    let row_bytes = region.width as usize * bytes_per_pixel;
    for row in 0..region.height as usize {
        let source = row * row_bytes;
        let destination =
            ((region.y as usize + row) * desc.width as usize + region.x as usize) * bytes_per_pixel;
        image.pixels[destination..destination + row_bytes]
            .copy_from_slice(&pixels[source..source + row_bytes]);
    }
    image.uploads = image.uploads.saturating_add(1);
    image.bytes_uploaded = image.bytes_uploaded.saturating_add(pixels.len() as u64);
    Ok(())
}

/// Visits the tree and returns traversal counters.
fn traverse(null: &mut NullState, scene: &RenderScene) -> FrameStats {
    let mut stats = FrameStats::default();
    if scene.is_empty() || scene.node(scene.root).is_none() {
        return stats;
    }
    visit(null, scene, scene.root, 0, &mut stats);
    stats
}

/// Recursive depth-first visit in sibling `z` order.
fn visit(
    null: &mut NullState,
    scene: &RenderScene,
    id: RenderNodeId,
    depth: u32,
    stats: &mut FrameStats,
) {
    if depth >= MAX_TRAVERSAL_DEPTH {
        return;
    }
    let Some(node) = scene.node(id) else {
        return;
    };
    stats.nodes_visited = stats.nodes_visited.saturating_add(1);
    stats.max_depth = stats.max_depth.max(depth);
    if !node.visible {
        stats.invisible = stats.invisible.saturating_add(1);
        return;
    }
    count_content(null, node, stats);
    // Draw in sibling `z` order, borrowing the usual already-sorted list.
    if children_are_sorted(scene, &node.children) {
        for &child in &node.children {
            visit(null, scene, child, depth + 1, stats);
        }
    } else {
        let mut children = node.children.clone();
        children.sort_by_key(|child| child_sort_key(scene, *child));
        for child in children {
            visit(null, scene, child, depth + 1, stats);
        }
    }
}

/// Records one node's drawable payload in `stats`.
fn count_content(null: &mut NullState, node: &RenderNode, stats: &mut FrameStats) {
    match &node.content {
        RenderContent::None => {}
        RenderContent::Solid(_) => {
            stats.draws = stats.draws.saturating_add(1);
            stats.solid_draws = stats.solid_draws.saturating_add(1);
        }
        RenderContent::Texture(tex) => {
            stats.draws = stats.draws.saturating_add(1);
            stats.texture_draws = stats.texture_draws.saturating_add(1);
            if !null.textures.contains_key(&tex.key()) {
                stats.missing_textures = stats.missing_textures.saturating_add(1);
            }
        }
        RenderContent::Glyphs(run) => {
            stats.draws = stats.draws.saturating_add(1);
            stats.glyph_draws = stats.glyph_draws.saturating_add(1);
            if let Some(atlas) = &run.atlas
                && !null.textures.contains_key(&atlas.key())
            {
                stats.missing_textures = stats.missing_textures.saturating_add(1);
            }
        }
        RenderContent::Effect(effect) => {
            // The headless backend never executes one, so the count is the
            // whole story: it proves a scene carried an effect draw.
            stats.draws = stats.draws.saturating_add(1);
            stats.effect_draws = stats.effect_draws.saturating_add(1);
            if effect
                .textures
                .iter()
                .any(|texture| !null.textures.contains_key(&texture.key()))
            {
                stats.missing_textures = stats.missing_textures.saturating_add(1);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effect::{EffectPipeline, RenderEffect};
    use api::geometry::RectF;
    use api::shader::EffectUniforms;

    /// A scene holding one effect draw at the origin.
    fn effect_scene() -> RenderScene {
        let pipeline = EffectPipeline::new("test", 16, Some(15));
        let mut scene = RenderScene::with_root_node();
        let root = scene.root;
        let child = scene.push(RenderNode {
            parent: Some(root),
            content: RenderContent::Effect(Box::new(RenderEffect::new(
                pipeline,
                Vec::new(),
                EffectUniforms::new(),
            ))),
            dst: RectF::new(0.0, 0.0, 4.0, 4.0),
            ..RenderNode::default()
        });
        if let Some(node) = scene.node_mut(root) {
            node.children = vec![child];
        }
        scene
    }

    #[test]
    fn the_headless_backend_counts_an_effect_draw() {
        let pipeline = NullRenderPipeline::new();
        let (surface, mut state) = pipeline.begin(&testkit::headless_window()).expect("begin");
        let stats = {
            pipeline
                .render(&surface, &mut state, &effect_scene())
                .expect("render");
            NullRenderPipeline::last_frame(&state).expect("frame stats")
        };
        assert_eq!(stats.effect_draws, 1);
        assert_eq!(stats.draws, 1);
        assert_eq!(stats.texture_draws, 0);
    }

    #[test]
    fn the_cpu_rasterizer_refuses_an_effect_scene() {
        let pipeline = NullRenderPipeline::new();
        let (_surface, mut state) = pipeline.begin(&testkit::headless_window()).expect("begin");
        let target = pipeline
            .create_render_target(RenderTargetDesc::new(4, 4))
            .expect("target");
        let error = pipeline
            .render_to_target(&mut state, &target, &effect_scene())
            .expect_err("the CPU rasterizer cannot evaluate a shader");
        assert!(
            matches!(error, RenderError::Unsupported { .. }),
            "unexpected error: {error}"
        );
    }
}
