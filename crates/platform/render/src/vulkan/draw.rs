//! Scene traversal, composition and command recording.
//!
//! The pipeline walks the [`RenderScene`] tree itself: it accumulates
//! parent transforms and opacity, clips with a scissor rectangle, orders
//! siblings by `z` and selects a pipeline + blend state per node. It never
//! asks the middle layer for a draw list.
//!
//! There are no silent fallbacks: a node whose composition needs a
//! feature this backend does not implement (a mask clip, a GPU glyph run or a
//! destination-dependent blend mode) makes the frame fail with a typed
//! [`RenderError::Unsupported`](api::error::RenderError::Unsupported).
#![allow(unsafe_code)]

use ash::vk;

use api::error::RenderError;
use api::geometry::{Affine2, RectF};

use super::pipelines::{PipelineSet, Program};
use super::texture::VulkanTexture;
use crate::MAX_TRAVERSAL_DEPTH;
use crate::collections::Map;
use crate::scene::{
    Clip, RenderContent, RenderNode, RenderNodeId, RenderScene, child_sort_key,
    children_are_sorted, node_spaces,
};

/// Push-constant block shared with the shaders (96 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct PushConstants {
    /// Column-major matrix mapping the unit quad to NDC.
    mvp: [f32; 16],
    /// RGBA tint; alpha carries the composed opacity.
    tint: [f32; 4],
    /// `(u, v, width, height)` source rectangle inside the texture.
    uv_rect: [f32; 4],
}

/// The push-constant block shared with the shaders must match the size the
/// pipeline layout reserves.
const _: () = assert!(std::mem::size_of::<PushConstants>() as u32 == super::PUSH_CONSTANT_SIZE);

/// The frame-wide resources one scene traversal records with.
pub(crate) struct SceneDraw<'a> {
    /// Logical device the command buffer belongs to.
    pub(crate) device: &'a ash::Device,
    /// Command buffer the scene is recorded into.
    pub(crate) command: vk::CommandBuffer,
    /// Pipelines and the shared unit-quad vertex buffer.
    pub(crate) pipelines: &'a PipelineSet,
    /// Uploaded textures keyed by texture identity.
    pub(crate) textures: &'a Map<u64, VulkanTexture>,
    /// Custom effect pipelines and the frame's uniform ring, when the caller
    /// has one. A scene with an effect draw and no state here is an error, not
    /// a skipped draw.
    pub(crate) effects: Option<&'a mut super::effect::EffectState>,
    /// Render pass the command buffer is inside: an effect pipeline is built
    /// per pass.
    pub(crate) render_pass: vk::RenderPass,
    /// Target extent in device pixels.
    pub(crate) extent: vk::Extent2D,
    /// Whether the target format is an sRGB surface.
    pub(crate) srgb: bool,
}

/// Records all visible content of `scene` into the draw's command buffer.
///
/// The caller has already begun the render pass and set the viewport.
/// `root` is the transform applied around the scene root. The live frame path
/// passes the identity and offscreen targets pass the identity as well.
///
/// # Errors
///
/// Returns [`RenderError::Unsupported`] when the scene uses a feature the
/// Vulkan backend does not implement (mask clips, GPU glyph runs or
/// destination-dependent blend modes).
pub(crate) fn record_scene(
    draw: SceneDraw<'_>,
    scene: &RenderScene,
    root: &Affine2,
) -> Result<(), RenderError> {
    let SceneDraw {
        device,
        command,
        pipelines,
        textures,
        effects,
        render_pass,
        extent,
        srgb,
    } = draw;
    let mut recorder = Recorder {
        device,
        command,
        pipelines,
        textures,
        effects,
        render_pass,
        scene,
        extent,
        srgb,
        cache: BindCache::default(),
    };

    // SAFETY: the command buffer is recording; the vertex buffer is owned by
    // `pipelines` and stays alive for the traversal.
    unsafe {
        device.cmd_bind_vertex_buffers(command, 0, &[pipelines.vertex_buffer], &[0]);
    }

    let full_scissor = vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent,
    };
    recorder.visit(scene.root, root, 1.0, full_scissor, 0)
}

/// One unit-quad draw request: the pipeline it selects and the shader inputs
/// plus scissor it carries.
struct QuadDraw<'a> {
    /// Vertex/fragment program to draw with.
    program: Program,
    /// Blend mode selecting the pipeline variant.
    blend: api::blend::BlendMode,
    /// Transform from the node's local space to device pixels.
    world: &'a Affine2,
    /// RGBA tint; alpha carries the composed opacity.
    tint: [f32; 4],
    /// `(u, v, width, height)` source rectangle inside the texture.
    uv_rect: [f32; 4],
    /// Descriptor set of the sampled texture, for a textured draw.
    descriptor: Option<vk::DescriptorSet>,
    /// Scissor rectangle the draw is clipped to.
    scissor: vk::Rect2D,
}

/// What the recorder believes is bound in the command buffer.
///
/// The recorder skips a bind when the handle is already the bound one. A draw
/// that binds another pipeline or another set behind the recorder's back must
/// invalidate the cache, or the next draw runs with the wrong pipeline.
#[derive(Clone, Copy, Default)]
struct BindCache {
    /// Pipeline bound for graphics draws.
    pipeline: Option<vk::Pipeline>,
    /// Descriptor set bound for graphics draws.
    descriptor: Option<vk::DescriptorSet>,
}

impl BindCache {
    /// Whether the pipeline must be bound before the next draw.
    fn needs_pipeline(&self, pipeline: vk::Pipeline) -> bool {
        self.pipeline != Some(pipeline)
    }

    /// Whether the descriptor set must be bound before the next draw.
    fn needs_descriptor(&self, set: vk::DescriptorSet) -> bool {
        self.descriptor != Some(set)
    }

    /// Records the bound pipeline.
    fn bind_pipeline(&mut self, pipeline: vk::Pipeline) {
        self.pipeline = Some(pipeline);
    }

    /// Records the bound descriptor set.
    fn bind_descriptor(&mut self, set: vk::DescriptorSet) {
        self.descriptor = Some(set);
    }

    /// Forgets what is bound, because another pipeline or set was bound.
    fn invalidate(&mut self) {
        self.pipeline = None;
        self.descriptor = None;
    }
}

/// Command-recording traversal state.
struct Recorder<'a> {
    device: &'a ash::Device,
    command: vk::CommandBuffer,
    pipelines: &'a PipelineSet,
    textures: &'a Map<u64, VulkanTexture>,
    effects: Option<&'a mut super::effect::EffectState>,
    render_pass: vk::RenderPass,
    scene: &'a RenderScene,
    extent: vk::Extent2D,
    srgb: bool,
    /// The pipeline and descriptor set the command buffer has bound.
    cache: BindCache,
}

impl Recorder<'_> {
    /// Visits one node and its subtree.
    fn visit(
        &mut self,
        id: RenderNodeId,
        parent_transform: &Affine2,
        parent_opacity: f32,
        scissor: vk::Rect2D,
        depth: u32,
    ) -> Result<(), RenderError> {
        if depth > MAX_TRAVERSAL_DEPTH {
            return Ok(());
        }
        let Some(node) = self.scene.node(id) else {
            return Ok(());
        };
        if !node.visible {
            return Ok(());
        }
        let opacity = parent_opacity * clamp_opacity(node.opacity);
        if opacity <= 0.0 {
            return Ok(());
        }

        // `transform` positions the node's local space (and therefore its
        // children); `dst` places the node's own content in that space. Keeping
        // them separate means a zero-sized container (for example the scene
        // root) does not collapse its children.
        let (node_space, content_space) = node_spaces(node, parent_transform);
        let scissor = self.scissor_for(node, &node_space, scissor)?;
        if scissor.extent.width == 0 || scissor.extent.height == 0 {
            return Ok(());
        }

        self.draw(node, &content_space, opacity, scissor)?;

        // Draw in sibling `z` order. L2 appends children already in increasing
        // `z`, so the borrowed fast path is the common case; only clone and
        // sort when the caller actually left the list unordered.
        if children_are_sorted(self.scene, &node.children) {
            for &child in &node.children {
                self.visit(child, &node_space, opacity, scissor, depth + 1)?;
            }
        } else {
            let mut children = node.children.clone();
            children.sort_by_key(|child| child_sort_key(self.scene, *child));
            for child in children {
                self.visit(child, &node_space, opacity, scissor, depth + 1)?;
            }
        }
        Ok(())
    }

    /// Intersects the node's clip (if any) with the inherited scissor.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unsupported`] for a mask clip, which the Vulkan
    /// backend does not implement.
    fn scissor_for(
        &self,
        node: &RenderNode,
        world: &Affine2,
        current: vk::Rect2D,
    ) -> Result<vk::Rect2D, RenderError> {
        let bounds = match &node.clip {
            Some(Clip::Rect(rect)) => world_bounds(world, rect),
            Some(Clip::Mask(_)) => {
                return Err(RenderError::Unsupported {
                    message: "mask clips are not implemented by the Vulkan backend".to_owned(),
                });
            }
            None => return Ok(current),
        };
        let clip = rect_to_scissor(bounds, self.extent);
        Ok(intersect_scissor(current, clip))
    }

    /// Draws the node's payload, if it has one.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unsupported`] for a GPU glyph run or a
    /// destination-dependent blend mode.
    fn draw(
        &mut self,
        node: &RenderNode,
        world: &Affine2,
        opacity: f32,
        scissor: vk::Rect2D,
    ) -> Result<(), RenderError> {
        match &node.content {
            RenderContent::None => Ok(()),
            RenderContent::Solid(color) => {
                let tint = self.color_tint(*color, opacity);
                self.draw_quad(QuadDraw {
                    program: Program::Solid,
                    blend: node.blend,
                    world,
                    tint,
                    uv_rect: [0.0, 0.0, 1.0, 1.0],
                    descriptor: None,
                    scissor,
                })
            }
            RenderContent::Texture(texture) => {
                let Some(gpu) = self.textures.get(&texture.key()) else {
                    return Ok(());
                };
                if !gpu.uploaded {
                    return Ok(());
                }
                let uv = uv_rect(texture.width(), texture.height(), node.src);
                // The per-node tint carries the text colour (linearised for an
                // sRGB target); the alpha is the composed opacity. Ordinary
                // textures keep the default `[1, 1, 1, 1]`.
                let tint = self.color_tint(node.color, opacity);
                self.draw_quad(QuadDraw {
                    program: Program::Textured,
                    blend: node.blend,
                    world,
                    tint,
                    uv_rect: uv,
                    descriptor: Some(gpu.descriptor_set),
                    scissor,
                })
            }
            RenderContent::Glyphs(_) => Err(RenderError::Unsupported {
                message: "GPU glyph runs are not implemented by the Vulkan backend".to_owned(),
            }),
            RenderContent::Effect(effect) => {
                // Copy the borrowed fields out before the mutable borrow of the
                // effect state below.
                let device = self.device;
                let command = self.command;
                let render_pass = self.render_pass;
                let textures = self.textures;
                let mvp = to_ndc(world, self.extent);
                let Some(effects) = self.effects.as_deref_mut() else {
                    return Err(RenderError::Unsupported {
                        message: "a scene with an effect draw needs an effect pipeline state"
                            .to_owned(),
                    });
                };
                let recorded = effects.record(
                    &super::effect::EffectDrawContext {
                        device,
                        command,
                        render_pass,
                        textures,
                        mvp,
                        opacity,
                        scissor,
                    },
                    effect,
                );
                // The effect draw binds its own pipeline and its own
                // descriptor set, so the cache no longer says what is bound.
                self.cache.invalidate();
                recorded
            }
        }
    }

    /// Emits one unit-quad draw.
    fn draw_quad(&mut self, quad: QuadDraw<'_>) -> Result<(), RenderError> {
        let QuadDraw {
            program,
            blend,
            world,
            tint,
            uv_rect,
            descriptor,
            scissor,
        } = quad;
        let pipeline = self.pipelines.pipeline_for(program, blend)?;
        // SAFETY: the command buffer is recording and every handle is live.
        unsafe {
            if self.cache.needs_pipeline(pipeline) {
                self.device.cmd_bind_pipeline(
                    self.command,
                    vk::PipelineBindPoint::GRAPHICS,
                    pipeline,
                );
                self.cache.bind_pipeline(pipeline);
            }
            if let Some(set) = descriptor
                && self.cache.needs_descriptor(set)
            {
                self.device.cmd_bind_descriptor_sets(
                    self.command,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipelines.pipeline_layout,
                    0,
                    &[set],
                    &[],
                );
                self.cache.bind_descriptor(set);
            }

            let constants = PushConstants {
                mvp: to_ndc(world, self.extent),
                tint,
                uv_rect,
            };
            let bytes = std::slice::from_raw_parts(
                (&constants as *const PushConstants).cast::<u8>(),
                std::mem::size_of::<PushConstants>(),
            );
            self.device.cmd_push_constants(
                self.command,
                self.pipelines.pipeline_layout,
                vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
                0,
                bytes,
            );
            self.device.cmd_set_scissor(self.command, 0, &[scissor]);
            self.device.cmd_draw(self.command, 4, 1, 0, 0);
        }
        Ok(())
    }

    /// Converts a straight-alpha node colour into a shader tint.
    ///
    /// The RGB channels are linearised when the target is an sRGB surface;
    /// the alpha carries the composed opacity.
    fn color_tint(&self, color: [f32; 4], opacity: f32) -> [f32; 4] {
        [
            self.linearize(color[0]),
            self.linearize(color[1]),
            self.linearize(color[2]),
            color[3] * opacity,
        ]
    }

    /// Encodes an sRGB channel as linear when the target is an sRGB surface.
    fn linearize(&self, channel: f32) -> f32 {
        if self.srgb {
            srgb_to_linear(channel)
        } else {
            channel
        }
    }
}

/// Returns the axis-aligned world bounds of `rect` transformed by `affine`.
fn world_bounds(affine: &Affine2, rect: &RectF) -> RectF {
    let corners = [
        affine.transform_point((rect.x, rect.y)),
        affine.transform_point((rect.right(), rect.y)),
        affine.transform_point((rect.x, rect.bottom())),
        affine.transform_point((rect.right(), rect.bottom())),
    ];
    let min_x = corners.iter().map(|p| p.0).fold(f32::INFINITY, f32::min);
    let min_y = corners.iter().map(|p| p.1).fold(f32::INFINITY, f32::min);
    let max_x = corners
        .iter()
        .map(|p| p.0)
        .fold(f32::NEG_INFINITY, f32::max);
    let max_y = corners
        .iter()
        .map(|p| p.1)
        .fold(f32::NEG_INFINITY, f32::max);
    RectF::new(min_x, min_y, max_x - min_x, max_y - min_y)
}

/// Converts a world rect into a clamped integer scissor.
fn rect_to_scissor(rect: RectF, extent: vk::Extent2D) -> vk::Rect2D {
    if rect.is_empty() {
        return vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: vk::Extent2D {
                width: 0,
                height: 0,
            },
        };
    }
    let x0 = rect.x.floor().max(0.0) as u32;
    let y0 = rect.y.floor().max(0.0) as u32;
    let x1 = rect.right().ceil().max(0.0) as u32;
    let y1 = rect.bottom().ceil().max(0.0) as u32;
    let x1 = x1.min(extent.width);
    let y1 = y1.min(extent.height);
    let width = x1.saturating_sub(x0);
    let height = y1.saturating_sub(y0);
    vk::Rect2D {
        offset: vk::Offset2D {
            x: x0 as i32,
            y: y0 as i32,
        },
        extent: vk::Extent2D { width, height },
    }
}

/// Intersects two scissors.
fn intersect_scissor(a: vk::Rect2D, b: vk::Rect2D) -> vk::Rect2D {
    let ax0 = a.offset.x.max(0) as u32;
    let ay0 = a.offset.y.max(0) as u32;
    let ax1 = ax0.saturating_add(a.extent.width);
    let ay1 = ay0.saturating_add(a.extent.height);
    let bx0 = b.offset.x.max(0) as u32;
    let by0 = b.offset.y.max(0) as u32;
    let bx1 = bx0.saturating_add(b.extent.width);
    let by1 = by0.saturating_add(b.extent.height);
    let x0 = ax0.max(bx0);
    let y0 = ay0.max(by0);
    let x1 = ax1.min(bx1);
    let y1 = ay1.min(by1);
    vk::Rect2D {
        offset: vk::Offset2D {
            x: x0 as i32,
            y: y0 as i32,
        },
        extent: vk::Extent2D {
            width: x1.saturating_sub(x0),
            height: y1.saturating_sub(y0),
        },
    }
}

/// Returns the normalized source rectangle for a texture draw.
fn uv_rect(width: u32, height: u32, src: api::geometry::PixelRegion) -> [f32; 4] {
    if src.is_empty() || width == 0 || height == 0 {
        return [0.0, 0.0, 1.0, 1.0];
    }
    [
        src.x as f32 / width as f32,
        src.y as f32 / height as f32,
        src.width as f32 / width as f32,
        src.height as f32 / height as f32,
    ]
}

/// Builds the column-major matrix mapping the unit quad to NDC.
///
/// L2 uses a y-down world convention (y = 0 is the top edge). Vulkan NDC has
/// y = -1 at the top of the framebuffer, so world `y` maps to
/// `ndc_y = 2*y/height - 1`.
fn to_ndc(affine: &Affine2, extent: vk::Extent2D) -> [f32; 16] {
    let a = 2.0 / extent.width.max(1) as f32;
    let b = 2.0 / extent.height.max(1) as f32;
    [
        a * affine.m11,
        b * affine.m21,
        0.0,
        0.0,
        a * affine.m12,
        b * affine.m22,
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
        0.0,
        a * affine.tx - 1.0,
        b * affine.ty - 1.0,
        0.0,
        1.0,
    ]
}

/// Clamps an opacity to `[0, 1]`, mapping NaN to `0`.
fn clamp_opacity(opacity: f32) -> f32 {
    if opacity.is_nan() {
        0.0
    } else {
        opacity.clamp(0.0, 1.0)
    }
}

/// Converts one sRGB channel to linear.
pub(crate) fn srgb_to_linear(channel: f32) -> f32 {
    let channel = channel.clamp(0.0, 1.0);
    if channel <= 0.04045 {
        channel / 12.92
    } else {
        ((channel + 0.055) / 1.055).powf(2.4)
    }
}

#[cfg(test)]
mod tests {
    use ash::vk::Handle;

    use super::*;

    /// The bind decisions a sequence of draws makes.
    ///
    /// The sequence mirrors a frame that draws a textured node, an effect node
    /// and another textured node. The effect node binds its own pipeline and
    /// its own descriptor set, so the second textured node has to bind again.
    #[test]
    fn an_effect_draw_between_two_textured_draws_rebinds() {
        let textured = vk::Pipeline::from_raw(1);
        let textured_set = vk::DescriptorSet::from_raw(2);
        let mut cache = BindCache::default();

        // The first textured draw binds its pipeline and its set.
        if cache.needs_pipeline(textured) {
            cache.bind_pipeline(textured);
        }
        if cache.needs_descriptor(textured_set) {
            cache.bind_descriptor(textured_set);
        }
        assert!(!cache.needs_pipeline(textured));
        assert!(!cache.needs_descriptor(textured_set));

        // The effect draw binds another pipeline and another set.
        cache.invalidate();

        // The second textured draw must bind again, or it would draw with the
        // effect pipeline and the effect's descriptor set.
        assert!(cache.needs_pipeline(textured));
        assert!(cache.needs_descriptor(textured_set));
    }

    /// Two textured draws over the same set bind once.
    #[test]
    fn two_textured_draws_over_one_set_bind_once() {
        let textured = vk::Pipeline::from_raw(1);
        let textured_set = vk::DescriptorSet::from_raw(2);
        let mut cache = BindCache::default();

        if cache.needs_pipeline(textured) {
            cache.bind_pipeline(textured);
        }
        if cache.needs_descriptor(textured_set) {
            cache.bind_descriptor(textured_set);
        }
        assert!(!cache.needs_pipeline(textured));
        assert!(!cache.needs_descriptor(textured_set));
    }
}
