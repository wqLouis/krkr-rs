//! Scene traversal, composition and command recording.
//!
//! The pipeline walks the [`RenderScene`] tree itself: it accumulates
//! parent transforms and opacity, clips with a scissor rectangle, orders
//! siblings by `z` and selects a pipeline + blend state per node. It never
//! asks the middle layer for a draw list.
//!
//! There are no silent fallbacks: a node whose composition needs a
//! feature this backend does not implement (a mask clip or a GPU glyph run)
//! makes the frame fail with a typed
//! [`RenderError::Unsupported`](api::error::RenderError::Unsupported).
//!
//! A destination-dependent blend reads the destination the node is composed
//! over. The recorder takes that destination the way the engine's blend
//! shaders take it: the target is copied into a separate image, and the
//! program mixes the source over that copy. The copy happens outside the
//! render pass — a colour attachment cannot be a copy's source — so the pass
//! ends, copies and begins again.
#![allow(unsafe_code)]

use ash::vk;

use api::error::RenderError;
use api::geometry::{Affine2, RectF};

use super::pipelines::{PipelineResources, PipelineSet, Program, dst_blend_mode};
use super::target::DestinationSnapshot;
use super::texture::VulkanTexture;
use crate::MAX_TRAVERSAL_DEPTH;
use crate::collections::Map;
use crate::scene::{
    Clip, RenderContent, RenderNode, RenderNodeId, RenderScene, child_sort_key,
    children_are_sorted, node_spaces,
};

/// Push-constant block shared with the shaders (112 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct PushConstants {
    /// Column-major matrix mapping the unit quad to NDC.
    mvp: [f32; 16],
    /// RGBA tint; alpha carries the composed opacity.
    tint: [f32; 4],
    /// `(u, v, width, height)` source rectangle inside the texture.
    uv_rect: [f32; 4],
    /// The destination-dependent blend's mode index.
    mode: u32,
    /// The composed opacity of the node being drawn.
    opacity: f32,
    /// The size of the target the scene is composed into, in device pixels.
    target_size: [f32; 2],
}

/// The push-constant block shared with the shaders must match the size the
/// pipeline layout reserves.
const _: () = assert!(std::mem::size_of::<PushConstants>() as u32 == super::PUSH_CONSTANT_SIZE);

/// The render pass a scene is composed into.
///
/// The recorder owns the pass's lifetime because the destination-dependent
/// route ends the pass around the copy of the destination and begins it
/// again.
pub(crate) struct ScenePass<'a> {
    /// Logical device the command buffer belongs to.
    pub(crate) device: &'a ash::Device,
    /// Command buffer the pass is recorded into.
    pub(crate) command: vk::CommandBuffer,
    /// The render pass.
    pub(crate) render_pass: vk::RenderPass,
    /// The framebuffer the pass renders into.
    pub(crate) framebuffer: vk::Framebuffer,
    /// The colour image the pass renders into.
    pub(crate) image: vk::Image,
    /// The clear colour the target starts from.
    pub(crate) clear: [f32; 4],
    /// Target extent in device pixels.
    pub(crate) extent: vk::Extent2D,
}

impl ScenePass<'_> {
    /// Begins the pass.
    fn begin(&self) {
        let render_area = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: self.extent,
        };
        let render_pass_begin = vk::RenderPassBeginInfo::default()
            .render_pass(self.render_pass)
            .framebuffer(self.framebuffer)
            .render_area(render_area);
        // SAFETY: the command buffer is recording; the render pass and
        // framebuffer are compatible and live.
        unsafe {
            self.device.cmd_begin_render_pass(
                self.command,
                &render_pass_begin,
                vk::SubpassContents::INLINE,
            );
        }
    }

    /// Ends the pass.
    fn end(&self) {
        // SAFETY: the render pass and command buffer are recording.
        unsafe {
            self.device.cmd_end_render_pass(self.command);
        }
    }
}

/// The frame-wide resources one scene traversal records with.
pub(crate) struct SceneDraw<'a> {
    /// The pass the scene is composed into.
    pub(crate) pass: ScenePass<'a>,
    /// Pipelines and the shared descriptor resources.
    pub(crate) pipelines: &'a PipelineSet,
    /// The descriptor and layout resources every pipeline set shares.
    pub(crate) resources: &'a PipelineResources,
    /// Uploaded textures keyed by texture identity.
    pub(crate) textures: &'a Map<u64, VulkanTexture>,
    /// Custom effect pipelines and the frame's uniform ring, when the caller
    /// has one. A scene with an effect draw and no state here is an error, not
    /// a skipped draw.
    pub(crate) effects: Option<&'a mut super::effect::EffectState>,
    /// The copy of the destination the destination-dependent blend reads.
    pub(crate) snapshot: Option<&'a mut DestinationSnapshot>,
    /// Target extent in device pixels.
    pub(crate) extent: vk::Extent2D,
    /// Whether the target format is an sRGB surface.
    pub(crate) srgb: bool,
}

/// Records all visible content of `scene` into the draw's command buffer.
///
/// The pass is begun and ended here, because the destination-dependent route
/// restarts it around the copy of the destination. `root` is the transform
/// applied around the scene root. The live frame path passes the identity and
/// offscreen targets pass the identity as well.
///
/// # Errors
///
/// Returns [`RenderError::Unsupported`] when the scene uses a feature the
/// Vulkan backend does not implement (mask clips or GPU glyph runs).
pub(crate) fn record_scene(
    draw: SceneDraw<'_>,
    scene: &RenderScene,
    root: &Affine2,
) -> Result<(), RenderError> {
    let SceneDraw {
        pass,
        pipelines,
        resources,
        textures,
        effects,
        snapshot,
        extent,
        srgb,
    } = draw;
    let mut recorder = Recorder {
        pass,
        pipelines,
        resources,
        textures,
        effects,
        snapshot,
        extent,
        srgb,
        scene,
        cache: BindCache::default(),
    };

    // SAFETY: the command buffer is recording; the vertex buffer is owned by
    // `resources` and stays alive for the traversal.
    unsafe {
        recorder.pass.device.cmd_bind_vertex_buffers(
            recorder.pass.command,
            0,
            &[resources.vertex_buffer],
            &[0],
        );
    }

    let full_scissor = vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent,
    };
    recorder.pass.begin();
    let result = match recorder.clear_target() {
        Ok(()) => recorder.visit(scene.root, root, 1.0, full_scissor, 0),
        Err(error) => Err(error),
    };
    recorder.pass.end();
    result
}

/// Records the draw that presents a composed target over the surface.
///
/// The draw is a full-screen quad that samples the target the scene was
/// composed into, so the surface shows the composed frame.
///
/// # Errors
///
/// Returns [`RenderError::Unsupported`] when the pipeline for the draw cannot
/// be selected.
pub(crate) fn record_present(
    device: &ash::Device,
    command: vk::CommandBuffer,
    pipelines: &PipelineSet,
    resources: &PipelineResources,
    target: vk::DescriptorSet,
    extent: vk::Extent2D,
) -> Result<(), RenderError> {
    let pipeline = pipelines.pipeline_for(Program::Textured, api::blend::BlendMode::SourceOver)?;
    let scissor = vk::Rect2D {
        offset: vk::Offset2D { x: 0, y: 0 },
        extent,
    };
    // The unit quad spans 0..1, so the transform maps it onto the whole
    // target: the scene draws carry device-pixel transforms of the same kind.
    let constants = PushConstants {
        mvp: to_ndc(
            &Affine2::scale(extent.width as f32, extent.height as f32),
            extent,
        ),
        tint: [1.0, 1.0, 1.0, 1.0],
        uv_rect: [0.0, 0.0, 1.0, 1.0],
        mode: 0,
        opacity: 1.0,
        target_size: [extent.width as f32, extent.height as f32],
    };
    // SAFETY: `constants` is a live, fully initialized push-constant block.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (&constants as *const PushConstants).cast::<u8>(),
            std::mem::size_of::<PushConstants>(),
        )
    };
    // SAFETY: the command buffer is recording and every handle is live.
    unsafe {
        device.cmd_bind_pipeline(command, vk::PipelineBindPoint::GRAPHICS, pipeline);
        device.cmd_bind_descriptor_sets(
            command,
            vk::PipelineBindPoint::GRAPHICS,
            resources.pipeline_layout,
            0,
            &[target],
            &[],
        );
        device.cmd_push_constants(
            command,
            resources.pipeline_layout,
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            0,
            bytes,
        );
        device.cmd_set_scissor(command, 0, &[scissor]);
        device.cmd_draw(command, 4, 1, 0, 0);
    }
    Ok(())
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

    /// Records the pipeline that was bound.
    fn bind_pipeline(&mut self, pipeline: vk::Pipeline) {
        self.pipeline = Some(pipeline);
    }

    /// Records the descriptor set that was bound.
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
    /// The pass the scene is composed into.
    pass: ScenePass<'a>,
    /// Pipelines for the pass's render pass.
    pipelines: &'a PipelineSet,
    /// The descriptor and layout resources every pipeline set shares.
    resources: &'a PipelineResources,
    /// Uploaded textures keyed by texture identity.
    textures: &'a Map<u64, VulkanTexture>,
    /// Custom effect pipelines and the frame's uniform ring.
    effects: Option<&'a mut super::effect::EffectState>,
    /// The copy of the destination the destination-dependent blend reads.
    snapshot: Option<&'a mut DestinationSnapshot>,
    /// The scene being traversed.
    scene: &'a RenderScene,
    /// Target extent in device pixels.
    extent: vk::Extent2D,
    /// Whether the target format is an sRGB surface.
    srgb: bool,
    /// The pipeline and descriptor set the command buffer has bound.
    cache: BindCache,
}

impl Recorder<'_> {
    /// Clears the target the scene is composed into.
    ///
    /// The render pass loads the attachment, so a render that follows another
    /// one keeps the earlier content; the target is cleared with a full-screen
    /// draw that replaces the colour instead.
    fn clear_target(&mut self) -> Result<(), RenderError> {
        let scissor = vk::Rect2D {
            offset: vk::Offset2D { x: 0, y: 0 },
            extent: self.extent,
        };
        self.draw_quad(QuadDraw {
            program: Program::Solid,
            blend: api::blend::BlendMode::Replace,
            world: &Affine2::IDENTITY,
            tint: self.pass.clear,
            uv_rect: [0.0, 0.0, 1.0, 1.0],
            descriptor: None,
            scissor,
        })
    }

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
        let clip = rect_to_scissor(&bounds, self.extent);
        Ok(intersect_scissor(current, clip))
    }

    /// Draws the node's payload, if it has one.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unsupported`] for a GPU glyph run.
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
                if let Some(kind) = node.blend.dst_blend() {
                    // The destination-dependent route: the source is blended
                    // over a copy of the destination in the program's shader.
                    let tint = self.source_tint(node.color);
                    self.draw_dst_blend(DstBlendDraw {
                        node,
                        world,
                        source: gpu.descriptor_set,
                        uv,
                        tint,
                        opacity,
                        kind,
                        scissor,
                    })
                } else {
                    // The per-node tint carries the text colour (linearised
                    // for an sRGB target); the alpha is the composed opacity.
                    // Ordinary textures keep the default `[1, 1, 1, 1]`.
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
            }
            RenderContent::Glyphs(_) => Err(RenderError::Unsupported {
                message: "GPU glyph runs are not implemented by the Vulkan backend".to_owned(),
            }),
            RenderContent::Effect(effect) => {
                // Copy the borrowed fields out before the mutable borrow of the
                // effect state below.
                let device = self.pass.device;
                let command = self.pass.command;
                let render_pass = self.pass.render_pass;
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
                self.pass.device.cmd_bind_pipeline(
                    self.pass.command,
                    vk::PipelineBindPoint::GRAPHICS,
                    pipeline,
                );
                self.cache.bind_pipeline(pipeline);
            }
            if let Some(set) = descriptor
                && self.cache.needs_descriptor(set)
            {
                self.pass.device.cmd_bind_descriptor_sets(
                    self.pass.command,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.resources.pipeline_layout,
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
                mode: 0,
                opacity: 1.0,
                target_size: target_size(self.extent),
            };
            push_constants(
                self.pass.device,
                self.pass.command,
                self.resources.pipeline_layout,
                &constants,
            );
            self.pass
                .device
                .cmd_set_scissor(self.pass.command, 0, &[scissor]);
            self.pass.device.cmd_draw(self.pass.command, 4, 1, 0, 0);
        }
        Ok(())
    }

    /// Emits the draw that mixes a source over a copy of the destination.
    ///
    /// # Errors
    ///
    /// Returns [`RenderError::Unsupported`] when the target has no copy of the
    /// destination to read.
    fn draw_dst_blend(&mut self, draw: DstBlendDraw<'_>) -> Result<(), RenderError> {
        let DstBlendDraw {
            node,
            world,
            source,
            uv,
            tint,
            opacity,
            kind,
            scissor,
        } = draw;
        let Some(snapshot) = self.snapshot.as_deref_mut() else {
            return Err(RenderError::Unsupported {
                message: "a destination-dependent blend needs a copy of the destination, which \
                          this target does not provide"
                    .to_owned(),
            });
        };
        // The destination is the target's content under the node, so the copy
        // takes the node's scissor rectangle. The copy's source is the
        // target, which is the pass's colour attachment, so the pass ends
        // around it and begins again afterwards.
        self.pass.end();
        snapshot.copy_region(
            self.pass.device,
            self.pass.command,
            self.pass.image,
            scissor,
        )?;
        self.pass.begin();
        let pipeline = self.pipelines.pipeline_for(Program::DstBlend, node.blend)?;
        let constants = PushConstants {
            mvp: to_ndc(world, self.extent),
            tint,
            uv_rect: uv,
            mode: dst_blend_mode(kind),
            opacity,
            target_size: target_size(self.extent),
        };
        // SAFETY: the command buffer is recording and every handle is live.
        unsafe {
            self.pass.device.cmd_bind_pipeline(
                self.pass.command,
                vk::PipelineBindPoint::GRAPHICS,
                pipeline,
            );
            self.pass.device.cmd_bind_descriptor_sets(
                self.pass.command,
                vk::PipelineBindPoint::GRAPHICS,
                self.resources.dst_pipeline_layout,
                0,
                &[source, snapshot.descriptor_set],
                &[],
            );
            push_constants(
                self.pass.device,
                self.pass.command,
                self.resources.dst_pipeline_layout,
                &constants,
            );
            self.pass
                .device
                .cmd_set_scissor(self.pass.command, 0, &[scissor]);
            self.pass.device.cmd_draw(self.pass.command, 4, 1, 0, 0);
        }
        // The destination-dependent program binds its own sets, so the cache
        // no longer says what is bound.
        self.cache.invalidate();
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

    /// Converts a straight-alpha node colour into the source tint of a
    /// destination-dependent draw.
    ///
    /// The composed opacity travels in its own push constant, because the
    /// blend functions mix the source's alpha with the opacity separately.
    fn source_tint(&self, color: [f32; 4]) -> [f32; 4] {
        [
            self.linearize(color[0]),
            self.linearize(color[1]),
            self.linearize(color[2]),
            color[3],
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
fn rect_to_scissor(rect: &RectF, extent: vk::Extent2D) -> vk::Rect2D {
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

/// The inputs of a destination-dependent draw.
struct DstBlendDraw<'a> {
    /// The node being drawn.
    node: &'a RenderNode,
    /// The node's world transform.
    world: &'a Affine2,
    /// The descriptor set of the node's source texture.
    source: vk::DescriptorSet,
    /// The source's UV rectangle.
    uv: [f32; 4],
    /// The source tint, including the node's alpha.
    tint: [f32; 4],
    /// The composed opacity.
    opacity: f32,
    /// The destination-dependent blend function.
    kind: api::blend::DstBlend,
    /// The node's scissor rectangle.
    scissor: vk::Rect2D,
}

/// Records the push constants of one draw.
///
/// `layout` must be the layout of the pipeline that is bound, so the
/// destination-dependent program receives its own layout.
fn push_constants(
    device: &ash::Device,
    command: vk::CommandBuffer,
    layout: vk::PipelineLayout,
    constants: &PushConstants,
) {
    // SAFETY: `constants` is a live, fully initialized push-constant block.
    let bytes = unsafe {
        std::slice::from_raw_parts(
            (constants as *const PushConstants).cast::<u8>(),
            std::mem::size_of::<PushConstants>(),
        )
    };
    // SAFETY: the command buffer is recording and the bytes are the
    // push-constant block the pipeline layout reserves.
    unsafe {
        device.cmd_push_constants(
            command,
            layout,
            vk::ShaderStageFlags::VERTEX | vk::ShaderStageFlags::FRAGMENT,
            0,
            bytes,
        );
    }
}

/// The size of the target in device pixels.
fn target_size(extent: vk::Extent2D) -> [f32; 2] {
    [extent.width as f32, extent.height as f32]
}

/// Clamps an opacity to `[0, 1]`.
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

    /// Two textured draws over one set bind once.
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
