//! Scene traversal, composition and command recording.
//!
//! The pipeline walks the [`RenderScene`] tree itself (D10): it accumulates
//! parent transforms and opacity, clips with a scissor rectangle, orders
//! siblings by `z` and selects a pipeline + blend state per node. It never
//! asks the middle layer for a draw list.
//!
//! Per D11 there are no silent fallbacks: a node whose composition needs a
//! feature this backend does not implement (a mask clip, a GPU glyph run or a
//! destination-dependent blend mode) makes the frame fail with a typed
//! [`RenderError::Unsupported`](krkr_api::error::RenderError::Unsupported).

use std::collections::HashMap;

use ash::vk;

use krkr_api::error::RenderError;
use krkr_api::geometry::{Affine2, RectF};

use super::pipelines::{PipelineSet, Program};
use super::texture::VulkanTexture;
use crate::MAX_TRAVERSAL_DEPTH;
use crate::scene::{
    Clip, RenderContent, RenderNode, RenderNodeId, RenderScene, child_sort_key, children_are_sorted,
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

/// Records all visible content of `scene` into `command`.
///
/// The caller has already begun the render pass and set the viewport.
///
/// # Errors
///
/// Returns [`RenderError::Unsupported`] when the scene uses a feature the
/// Vulkan backend does not implement (mask clips, GPU glyph runs or
/// destination-dependent blend modes).
#[allow(clippy::too_many_arguments)]
pub(crate) fn record_scene(
    device: &ash::Device,
    command: vk::CommandBuffer,
    pipelines: &PipelineSet,
    textures: &HashMap<u64, VulkanTexture>,
    scene: &RenderScene,
    extent: vk::Extent2D,
    srgb: bool,
) -> Result<(), RenderError> {
    let mut recorder = Recorder {
        device,
        command,
        pipelines,
        textures,
        scene,
        extent,
        srgb,
        bound_pipeline: None,
        bound_descriptor: None,
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
    recorder.visit(scene.root, &Affine2::IDENTITY, 1.0, full_scissor, 0)
}

/// Command-recording traversal state.
struct Recorder<'a> {
    device: &'a ash::Device,
    command: vk::CommandBuffer,
    pipelines: &'a PipelineSet,
    textures: &'a HashMap<u64, VulkanTexture>,
    scene: &'a RenderScene,
    extent: vk::Extent2D,
    srgb: bool,
    bound_pipeline: Option<vk::Pipeline>,
    bound_descriptor: Option<vk::DescriptorSet>,
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
                let tint = self.solid_tint(*color, opacity);
                self.draw_quad(
                    Program::Solid,
                    node.blend,
                    world,
                    tint,
                    [0.0, 0.0, 1.0, 1.0],
                    None,
                    scissor,
                )
            }
            RenderContent::Texture(texture) => {
                let Some(gpu) = self.textures.get(&texture.key()) else {
                    return Ok(());
                };
                if !gpu.uploaded {
                    return Ok(());
                }
                let uv = uv_rect(texture.width(), texture.height(), node.src);
                let tint = [1.0, 1.0, 1.0, opacity];
                self.draw_quad(
                    Program::Textured,
                    node.blend,
                    world,
                    tint,
                    uv,
                    Some(gpu.descriptor_set),
                    scissor,
                )
            }
            RenderContent::Glyphs(_) => Err(RenderError::Unsupported {
                message: "GPU glyph runs are not implemented by the Vulkan backend".to_owned(),
            }),
        }
    }

    /// Emits one unit-quad draw.
    #[allow(clippy::too_many_arguments)]
    fn draw_quad(
        &mut self,
        program: Program,
        blend: krkr_api::blend::BlendMode,
        world: &Affine2,
        tint: [f32; 4],
        uv_rect: [f32; 4],
        descriptor: Option<vk::DescriptorSet>,
        scissor: vk::Rect2D,
    ) -> Result<(), RenderError> {
        let pipeline = self.pipelines.pipeline_for(program, blend)?;
        // SAFETY: the command buffer is recording and every handle is live.
        unsafe {
            if self.bound_pipeline != Some(pipeline) {
                self.device.cmd_bind_pipeline(
                    self.command,
                    vk::PipelineBindPoint::GRAPHICS,
                    pipeline,
                );
                self.bound_pipeline = Some(pipeline);
            }
            if let Some(set) = descriptor
                && self.bound_descriptor != Some(set)
            {
                self.device.cmd_bind_descriptor_sets(
                    self.command,
                    vk::PipelineBindPoint::GRAPHICS,
                    self.pipelines.pipeline_layout,
                    0,
                    &[set],
                    &[],
                );
                self.bound_descriptor = Some(set);
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

    /// Converts a straight-alpha solid color into a shader tint.
    fn solid_tint(&self, color: [f32; 4], opacity: f32) -> [f32; 4] {
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

/// Returns `(node_space, content_space)` for `node` under `parent`.
///
/// `node_space` is where the node's children live; `content_space` additionally
/// maps the unit quad onto the node's `dst` rectangle.
fn node_spaces(node: &RenderNode, parent: &Affine2) -> (Affine2, Affine2) {
    let node_space = parent.then(&node.transform.to_affine());
    let content_space = node_space.then(&place_rect(&node.dst));
    (node_space, content_space)
}

/// Maps the unit quad onto `rect` in the parent's coordinate space.
fn place_rect(rect: &RectF) -> Affine2 {
    Affine2 {
        m11: rect.width,
        m12: 0.0,
        m21: 0.0,
        m22: rect.height,
        tx: rect.x,
        ty: rect.y,
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
fn uv_rect(width: u32, height: u32, src: krkr_api::geometry::PixelRegion) -> [f32; 4] {
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
