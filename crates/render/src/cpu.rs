//! CPU software rasterizer used for offscreen render targets.
//!
//! This is the reference ("D8") correctness baseline: it composites a
//! [`RenderScene`] into a caller-owned RGBA8 buffer using the same traversal
//! rules as the Vulkan backend (parent transforms, opacity, sibling `z`
//! ordering, rectangle clips) and the Photoshop/KiriKiri blend formulas from
//! `reference/cpp/core/visual/gl/blend_functor_c.h`.
//!
//! Unlike the GPU path, a CPU compositor naturally *samples the destination*
//! for every pixel, so the destination-dependent [`BlendMode`]s are evaluated
//! exactly here. The Vulkan backend uses this as the behavioural baseline.
//!
//! The rasterizer is a pure function over explicit state: it reads the scene
//! and texture pixels it is handed and writes only into `pixels`. It keeps no
//! globals and no cache.

use api::blend::{BlendMode, DstBlend};
use api::error::RenderError;
use api::geometry::{Affine2, PixelRegion, RectF};

use crate::MAX_TRAVERSAL_DEPTH;
use crate::scene::{
    Clip, RenderContent, RenderNode, RenderNodeId, RenderScene, child_sort_key, children_are_sorted,
};
use crate::texture::TextureFormat;

/// CPU view of one texture's pixels.
pub(crate) struct CpuTexture<'a> {
    /// Tightly packed, row-major pixels in `format`.
    pub(crate) pixels: &'a [u8],
    /// Texture width in pixels.
    pub(crate) width: u32,
    /// Texture height in pixels.
    pub(crate) height: u32,
    /// Storage format of `pixels`.
    pub(crate) format: TextureFormat,
}

/// Read-only lookup from a [`crate::Texture`] key to its CPU pixels.
pub(crate) trait TextureProvider {
    /// Returns the pixels for `key`, when the texture has been uploaded.
    fn texture(&self, key: u64) -> Option<CpuTexture<'_>>;
}

/// One rasterization vertex in target pixels plus texture coordinates in
/// texel units.
#[derive(Clone, Copy)]
struct Vertex {
    x: f64,
    y: f64,
    u: f64,
    v: f64,
}

/// Composites `scene` into `pixels`.
///
/// `pixels` must be `width * height * 4` bytes of tightly packed RGBA8. The
/// target is cleared with `background` (defaulting to opaque black, matching
/// the Vulkan clear value) before traversal.
///
/// # Errors
///
/// Returns [`RenderError::Unsupported`] for a GPU-only feature the CPU path
/// does not rasterize (mask clips and GPU glyph runs), matching the Vulkan
/// backend's policy (D11).
pub(crate) fn render_scene<P: TextureProvider>(
    width: u32,
    height: u32,
    pixels: &mut [u8],
    background: Option<[f32; 4]>,
    scene: &RenderScene,
    textures: &P,
) -> Result<(), RenderError> {
    let expected = (width as usize) * (height as usize) * 4;
    if pixels.len() != expected {
        return Err(RenderError::Framebuffer {
            message: format!(
                "offscreen target buffer is {} bytes, expected {expected}",
                pixels.len()
            ),
        });
    }

    let clear = background.unwrap_or([0.0, 0.0, 0.0, 1.0]);
    let clear = [
        to_u8(clear[0]),
        to_u8(clear[1]),
        to_u8(clear[2]),
        to_u8(clear[3]),
    ];
    for texel in pixels.chunks_exact_mut(4) {
        texel.copy_from_slice(&clear);
    }

    if scene.is_empty() || scene.node(scene.root).is_none() {
        return Ok(());
    }

    let mut canvas = Canvas {
        width,
        height,
        pixels,
        textures,
    };
    let full = PixelRegion::new(0, 0, width, height);
    canvas.visit(scene, scene.root, &Affine2::IDENTITY, 1.0, full, 0)
}

/// Mutable rasterization state.
struct Canvas<'a, P: TextureProvider> {
    width: u32,
    height: u32,
    pixels: &'a mut [u8],
    textures: &'a P,
}

impl<P: TextureProvider> Canvas<'_, P> {
    /// Visits one node and its subtree in sibling `z` order.
    fn visit(
        &mut self,
        scene: &RenderScene,
        id: RenderNodeId,
        parent_transform: &Affine2,
        parent_opacity: f32,
        scissor: PixelRegion,
        depth: u32,
    ) -> Result<(), RenderError> {
        if depth >= MAX_TRAVERSAL_DEPTH {
            return Ok(());
        }
        let Some(node) = scene.node(id) else {
            return Ok(());
        };
        if !node.visible {
            return Ok(());
        }
        let opacity = parent_opacity * clamp_opacity(node.opacity);
        if opacity <= 0.0 {
            return Ok(());
        }

        let (node_space, content_space) = node_spaces(node, parent_transform);
        let scissor = self.scissor_for(node, &node_space, scissor)?;
        if scissor.is_empty() {
            return Ok(());
        }

        self.draw(node, &content_space, opacity, scissor)?;

        if children_are_sorted(scene, &node.children) {
            for &child in &node.children {
                self.visit(scene, child, &node_space, opacity, scissor, depth + 1)?;
            }
        } else {
            let mut children = node.children.clone();
            children.sort_by_key(|child| child_sort_key(scene, *child));
            for child in children {
                self.visit(scene, child, &node_space, opacity, scissor, depth + 1)?;
            }
        }
        Ok(())
    }

    /// Intersects the node's clip with the inherited scissor.
    fn scissor_for(
        &self,
        node: &RenderNode,
        world: &Affine2,
        current: PixelRegion,
    ) -> Result<PixelRegion, RenderError> {
        let bounds = match &node.clip {
            Some(Clip::Rect(rect)) => world_bounds(world, rect),
            Some(Clip::Mask(_)) => {
                return Err(RenderError::Unsupported {
                    message: "mask clips are not implemented by the CPU renderer".to_owned(),
                });
            }
            None => return Ok(current),
        };
        let clip = rect_to_region(bounds, self.width, self.height);
        Ok(intersect_region(current, clip))
    }

    /// Rasterizes the node's payload, if it has one.
    fn draw(
        &mut self,
        node: &RenderNode,
        world: &Affine2,
        opacity: f32,
        scissor: PixelRegion,
    ) -> Result<(), RenderError> {
        match &node.content {
            RenderContent::None => Ok(()),
            RenderContent::Solid(color) => {
                let corners = quad_corners(world);
                // Quantize to 8 bits to match the destination and the
                // reference's byte-oriented pipeline.
                let src = [
                    to_u8(color[0]) as f32 / 255.0,
                    to_u8(color[1]) as f32 / 255.0,
                    to_u8(color[2]) as f32 / 255.0,
                    clamp_opacity(color[3]) * opacity,
                ];
                let uv = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
                self.draw_quad(node, corners, uv, |_| src, scissor);
                Ok(())
            }
            RenderContent::Texture(texture) => {
                // Copy the provider reference out of `self` so the sampled
                // view does not borrow `self` while the canvas is mutated.
                let textures = self.textures;
                let Some(view) = textures.texture(texture.key()) else {
                    return Ok(());
                };
                if view.width == 0 || view.height == 0 {
                    return Ok(());
                }
                let corners = quad_corners(world);
                let uv = texture_uv(texture.width(), texture.height(), node.src);
                let tint = [1.0, 1.0, 1.0, opacity];
                self.draw_quad(
                    node,
                    corners,
                    uv,
                    |uv| sample_texture(&view, uv.0, uv.1, tint),
                    scissor,
                );
                Ok(())
            }
            RenderContent::Glyphs(_) => Err(RenderError::Unsupported {
                message: "GPU glyph runs are not implemented by the CPU renderer".to_owned(),
            }),
        }
    }

    /// Rasterizes one affine quad as two triangles.
    fn draw_quad<F>(
        &mut self,
        node: &RenderNode,
        corners: [(f64, f64); 4],
        uv: [(f64, f64); 4],
        sample: F,
        scissor: PixelRegion,
    ) where
        F: Fn((f64, f64)) -> [f32; 4],
    {
        let v = [
            Vertex {
                x: corners[0].0,
                y: corners[0].1,
                u: uv[0].0,
                v: uv[0].1,
            },
            Vertex {
                x: corners[1].0,
                y: corners[1].1,
                u: uv[1].0,
                v: uv[1].1,
            },
            Vertex {
                x: corners[2].0,
                y: corners[2].1,
                u: uv[2].0,
                v: uv[2].1,
            },
            Vertex {
                x: corners[3].0,
                y: corners[3].1,
                u: uv[3].0,
                v: uv[3].1,
            },
        ];
        self.raster_triangle(node, v[0], v[1], v[2], scissor, &sample);
        self.raster_triangle(node, v[0], v[2], v[3], scissor, &sample);
    }

    /// Rasterizes one triangle with barycentric interpolation.
    fn raster_triangle<F>(
        &mut self,
        node: &RenderNode,
        a: Vertex,
        mut b: Vertex,
        mut c: Vertex,
        scissor: PixelRegion,
        sample: &F,
    ) where
        F: Fn((f64, f64)) -> [f32; 4],
    {
        let area = edge(&a, &b, &c);
        if area.abs() < 1e-9 {
            return;
        }
        if area < 0.0 {
            std::mem::swap(&mut b, &mut c);
        }
        let area = edge(&a, &b, &c);

        let min_x = a.x.min(b.x).min(c.x);
        let max_x = a.x.max(b.x).max(c.x);
        let min_y = a.y.min(b.y).min(c.y);
        let max_y = a.y.max(b.y).max(c.y);

        let x0 = (min_x.floor().max(0.0) as i64).clamp(0, self.width as i64) as u32;
        let y0 = (min_y.floor().max(0.0) as i64).clamp(0, self.height as i64) as u32;
        let x1 = (max_x.ceil().max(0.0) as i64).clamp(0, self.width as i64) as u32;
        let y1 = (max_y.ceil().max(0.0) as i64).clamp(0, self.height as i64) as u32;

        let x0 = x0.max(scissor.x);
        let y0 = y0.max(scissor.y);
        let x1 = x1.min(scissor.right());
        let y1 = y1.min(scissor.bottom());
        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let inv_area = 1.0 / area;
        for y in y0..y1 {
            let py = y as f64 + 0.5;
            for x in x0..x1 {
                let px = x as f64 + 0.5;
                let p = Vertex {
                    x: px,
                    y: py,
                    u: 0.0,
                    v: 0.0,
                };
                let w0 = edge(&b, &c, &p);
                let w1 = edge(&c, &a, &p);
                let w2 = edge(&a, &b, &p);
                if !inside_edge(w0, &b, &c) || !inside_edge(w1, &c, &a) || !inside_edge(w2, &a, &b)
                {
                    continue;
                }
                let l0 = w0 * inv_area;
                let l1 = w1 * inv_area;
                let l2 = w2 * inv_area;
                let u = a.u * l0 + b.u * l1 + c.u * l2;
                let v = a.v * l0 + b.v * l1 + c.v * l2;
                let src = sample((u, v));
                self.compose(x, y, node, src);
            }
        }
    }

    /// Composites one straight-alpha source pixel over the destination.
    fn compose(&mut self, x: u32, y: u32, node: &RenderNode, src: [f32; 4]) {
        let index = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        let dst = [
            self.pixels[index] as f32 / 255.0,
            self.pixels[index + 1] as f32 / 255.0,
            self.pixels[index + 2] as f32 / 255.0,
            self.pixels[index + 3] as f32 / 255.0,
        ];
        let (rgb, alpha) = composite(node.blend, dst, src);
        let alpha = if node.hold_alpha { dst[3] } else { alpha };
        self.pixels[index] = to_u8(rgb[0]);
        self.pixels[index + 1] = to_u8(rgb[1]);
        self.pixels[index + 2] = to_u8(rgb[2]);
        self.pixels[index + 3] = to_u8(alpha);
    }
}

/// Composites `src` over `dst` using `mode`, returning straight-alpha RGBA.
fn composite(mode: BlendMode, dst: [f32; 4], src: [f32; 4]) -> ([f32; 3], f32) {
    let a = src[3].clamp(0.0, 1.0);
    let d = [dst[0], dst[1], dst[2]];
    let s = [src[0], src[1], src[2]];
    match mode {
        BlendMode::Replace => (s, a),
        BlendMode::Additive | BlendMode::AddAlpha => {
            let rgb = [
                (d[0] + s[0] * a).clamp(0.0, 1.0),
                (d[1] + s[1] * a).clamp(0.0, 1.0),
                (d[2] + s[2] * a).clamp(0.0, 1.0),
            ];
            (rgb, (dst[3] + a).clamp(0.0, 1.0))
        }
        BlendMode::Subtractive => {
            let rgb = [
                (d[0] - s[0] * a).clamp(0.0, 1.0),
                (d[1] - s[1] * a).clamp(0.0, 1.0),
                (d[2] - s[2] * a).clamp(0.0, 1.0),
            ];
            (rgb, dst[3])
        }
        BlendMode::SourceOver => {
            let rgb = lerp3(d, s, a);
            (rgb, over_alpha(dst[3], a))
        }
        BlendMode::Multiply | BlendMode::Screen | BlendMode::DestinationDependent(_) => {
            let blended = [
                blend_channel(mode, d[0], s[0]),
                blend_channel(mode, d[1], s[1]),
                blend_channel(mode, d[2], s[2]),
            ];
            let rgb = lerp3(d, blended, a);
            (rgb, over_alpha(dst[3], a))
        }
    }
}

/// Returns the destination-over source alpha `a + ad·(1 − a)`.
fn over_alpha(dst_alpha: f32, a: f32) -> f32 {
    (a + dst_alpha * (1.0 - a)).clamp(0.0, 1.0)
}

/// Linear interpolation from `from` to `to` by `t`.
fn lerp3(from: [f32; 3], to: [f32; 3], t: f32) -> [f32; 3] {
    [
        from[0] + (to[0] - from[0]) * t,
        from[1] + (to[1] - from[1]) * t,
        from[2] + (to[2] - from[2]) * t,
    ]
}

/// Evaluates the per-channel blend function `f(dst, src)` in `[0, 1]`.
///
/// The formulas follow the Photoshop/KiriKiri definitions in
/// `reference/cpp/core/visual/gl/blend_functor_c.h` (including the
/// `ColorDodge5`/`Difference5` legacy variants, which share the modern path
/// here).
fn blend_channel(mode: BlendMode, d: f32, s: f32) -> f32 {
    let d = d.clamp(0.0, 1.0);
    let s = s.clamp(0.0, 1.0);
    let value = match mode {
        BlendMode::Multiply => d * s,
        BlendMode::Screen => d + s - d * s,
        BlendMode::DestinationDependent(kind) => match kind {
            DstBlend::Darken => d.min(s),
            DstBlend::Lighten => d.max(s),
            DstBlend::Difference | DstBlend::Difference5 => (d - s).abs(),
            DstBlend::Exclusion => d + s - 2.0 * d * s,
            DstBlend::Overlay => hard_light(s, d),
            DstBlend::HardLight => hard_light(d, s),
            DstBlend::SoftLight => soft_light(d, s),
            DstBlend::ColorDodge | DstBlend::ColorDodge5 => {
                if s >= 1.0 {
                    1.0
                } else {
                    (d / (1.0 - s)).min(1.0)
                }
            }
            DstBlend::ColorBurn => {
                if s <= 0.0 {
                    0.0
                } else {
                    1.0 - ((1.0 - d) / s).min(1.0)
                }
            }
        },
        // The remaining modes do not use the blend function.
        _ => s,
    };
    value.clamp(0.0, 1.0)
}

/// Hard-light blend, with `Overlay` expressed as swapped operands.
fn hard_light(d: f32, s: f32) -> f32 {
    if s < 0.5 {
        2.0 * d * s
    } else {
        1.0 - 2.0 * (1.0 - d) * (1.0 - s)
    }
}

/// Photoshop soft-light.
fn soft_light(d: f32, s: f32) -> f32 {
    if s <= 0.5 {
        d - (1.0 - 2.0 * s) * d * (1.0 - d)
    } else {
        let curve = if d <= 0.25 {
            ((16.0 * d - 12.0) * d + 4.0) * d
        } else {
            d.sqrt()
        };
        d + (2.0 * s - 1.0) * (curve - d)
    }
}

/// Samples a texture at texel coordinates `(u, v)` with bilinear filtering.
fn sample_texture(view: &CpuTexture<'_>, u: f64, v: f64, tint: [f32; 4]) -> [f32; 4] {
    let format = view.format;
    let width = view.width as i64;
    let height = view.height as i64;
    let x = u - 0.5;
    let y = v - 0.5;
    let x0 = x.floor() as i64;
    let y0 = y.floor() as i64;
    let fx = (x - x0 as f64) as f32;
    let fy = (y - y0 as f64) as f32;

    let fetch = |ix: i64, iy: i64| -> [f32; 4] {
        let ix = ix.clamp(0, width - 1) as usize;
        let iy = iy.clamp(0, height - 1) as usize;
        let index = (iy * view.width as usize + ix) * format.bytes_per_pixel() as usize;
        match format {
            TextureFormat::Rgba8Srgb | TextureFormat::Rgba8Unorm => {
                let pixel = &view.pixels[index..index + 4];
                [
                    pixel[0] as f32 / 255.0,
                    pixel[1] as f32 / 255.0,
                    pixel[2] as f32 / 255.0,
                    pixel[3] as f32 / 255.0,
                ]
            }
            TextureFormat::R8Unorm => {
                let coverage = view.pixels[index] as f32 / 255.0;
                [tint[0], tint[1], tint[2], coverage * tint[3]]
            }
        }
    };

    let p00 = fetch(x0, y0);
    let p10 = fetch(x0 + 1, y0);
    let p01 = fetch(x0, y0 + 1);
    let p11 = fetch(x0 + 1, y0 + 1);
    let mut out = [0.0f32; 4];
    for channel in 0..4 {
        let top = p00[channel] + (p10[channel] - p00[channel]) * fx;
        let bottom = p01[channel] + (p11[channel] - p01[channel]) * fx;
        out[channel] = top + (bottom - top) * fy;
    }
    // The tint only applies to formats that do not carry their own colour.
    match format {
        TextureFormat::Rgba8Srgb | TextureFormat::Rgba8Unorm => [
            out[0] * tint[0],
            out[1] * tint[1],
            out[2] * tint[2],
            out[3] * tint[3],
        ],
        TextureFormat::R8Unorm => out,
    }
}

/// Returns `(node_space, content_space)` for `node` under `parent`.
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

/// Returns the four unit-quad corners transformed into target pixels.
fn quad_corners(affine: &Affine2) -> [(f64, f64); 4] {
    let point = |x: f32, y: f32| {
        let (px, py) = affine.transform_point((x, y));
        (px as f64, py as f64)
    };
    [
        point(0.0, 0.0),
        point(1.0, 0.0),
        point(1.0, 1.0),
        point(0.0, 1.0),
    ]
}

/// Returns the four texture coordinates for a source sub-rectangle.
fn texture_uv(width: u32, height: u32, src: PixelRegion) -> [(f64, f64); 4] {
    let (u0, v0, u1, v1) = if src.is_empty() || width == 0 || height == 0 {
        (0.0, 0.0, width as f64, height as f64)
    } else {
        (
            src.x as f64,
            src.y as f64,
            src.right() as f64,
            src.bottom() as f64,
        )
    };
    [(u0, v0), (u1, v0), (u1, v1), (u0, v1)]
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

/// Converts a world rect into a clamped integer region.
fn rect_to_region(rect: RectF, width: u32, height: u32) -> PixelRegion {
    if rect.is_empty() {
        return PixelRegion::ZERO;
    }
    let x0 = rect.x.floor().max(0.0).min(width as f32) as u32;
    let y0 = rect.y.floor().max(0.0).min(height as f32) as u32;
    let x1 = rect.right().ceil().max(0.0).min(width as f32) as u32;
    let y1 = rect.bottom().ceil().max(0.0).min(height as f32) as u32;
    PixelRegion::from_min_max(x0, y0, x1, y1)
}

/// Intersects two pixel regions.
fn intersect_region(a: PixelRegion, b: PixelRegion) -> PixelRegion {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = a.right().min(b.right());
    let y1 = a.bottom().min(b.bottom());
    PixelRegion::from_min_max(x0, y0, x1, y1)
}

/// Twice the signed area of triangle `(a, b, c)`.
fn edge(a: &Vertex, b: &Vertex, c: &Vertex) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
}

/// Returns `true` when a boundary pixel belongs to the edge `a -> b`.
///
/// Implements the top-left fill rule so a shared triangle edge is rasterized
/// exactly once (otherwise a half-transparent quad would double-composite
/// along its diagonal). The triangle has positive signed area at this point.
fn inside_edge(w: f64, a: &Vertex, b: &Vertex) -> bool {
    if w > 0.0 {
        return true;
    }
    if w < 0.0 {
        return false;
    }
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    dy > 0.0 || (dy == 0.0 && dx < 0.0)
}

/// Clamps an opacity to `[0, 1]`, mapping NaN to `0`.
fn clamp_opacity(opacity: f32) -> f32 {
    if opacity.is_nan() {
        0.0
    } else {
        opacity.clamp(0.0, 1.0)
    }
}

/// Encodes a `[0, 1]` channel as an 8-bit value.
fn to_u8(channel: f32) -> u8 {
    (channel.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}
