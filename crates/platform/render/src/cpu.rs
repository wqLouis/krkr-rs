//! CPU software rasterizer used for offscreen render targets.
//!
//! This is the correctness baseline: it composites a
//! [`RenderScene`] into a caller-owned RGBA8 buffer using the same traversal
//! rules as the Vulkan backend (parent transforms, opacity, sibling `z`
//! ordering, rectangle clips) and the Photoshop blend formulas.
//!
//! Unlike the GPU path, a CPU compositor naturally *samples the destination*
//! for every pixel, so the destination-dependent [`BlendMode`]s are evaluated
//! exactly here. The Vulkan backend uses this as the behavioural baseline.
//!
//! The Photoshop blend family runs the shared table of per-channel functions
//! on the 32-bit ARGB word: the mix truncates by eight bits, so the result is
//! the byte-exact one, not a float approximation of it.
//!
//! The rasterizer is a pure function over explicit state: it reads the scene
//! and texture pixels it is handed and writes only into `pixels`. It keeps no
//! globals and no cache.

use api::blend::{BlendMode, DstBlend};
use api::error::RenderError;
use api::geometry::{Affine2, PixelRegion, RectF};

use crate::MAX_TRAVERSAL_DEPTH;
use crate::blend::PhotoshopBlend;
use crate::scene::{
    Clip, RenderContent, RenderNode, RenderNodeId, RenderScene, child_sort_key,
    children_are_sorted, node_spaces,
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
/// backend's policy.
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
    for texel in pixels.as_chunks_mut::<4>().0 {
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
                // byte-oriented pipeline.
                let src = [
                    to_u8(color[0]) as f32 / 255.0,
                    to_u8(color[1]) as f32 / 255.0,
                    to_u8(color[2]) as f32 / 255.0,
                    clamp_opacity(color[3]),
                ];
                let uv = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
                self.draw_quad(node, corners, uv, |_| src, opacity, scissor);
                Ok(())
            }
            RenderContent::Texture(texture) => {
                // Copy the provider handle out of `self` so the sampled view does
                // not borrow `self` while the canvas is mutated.
                let textures = self.textures;
                let Some(view) = textures.texture(texture.key()) else {
                    return Ok(());
                };
                if view.width == 0 || view.height == 0 {
                    return Ok(());
                }
                let corners = quad_corners(world);
                let uv = texture_uv(texture.width(), texture.height(), node.src);
                // The per-node tint modulates the sampled texture; the RGB
                // channels carry the text colour and the alpha is the composed
                // opacity. Default `[1, 1, 1, 1]` leaves ordinary textures
                // unchanged.
                let tint = [
                    clamp_opacity(node.color[0]),
                    clamp_opacity(node.color[1]),
                    clamp_opacity(node.color[2]),
                    clamp_opacity(node.color[3]),
                ];
                self.draw_quad(
                    node,
                    corners,
                    uv,
                    |uv| sample_texture(&view, uv.0, uv.1, tint),
                    opacity,
                    scissor,
                );
                Ok(())
            }
            RenderContent::Glyphs(_) => Err(RenderError::Unsupported {
                message: "GPU glyph runs are not implemented by the CPU renderer".to_owned(),
            }),
            RenderContent::Effect(_) => Err(RenderError::Unsupported {
                message: "custom effect draws are not rasterized by the CPU renderer".to_owned(),
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
        opacity: f32,
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
        self.raster_triangle(node, [v[0], v[1], v[2]], opacity, scissor, &sample);
        self.raster_triangle(node, [v[0], v[2], v[3]], opacity, scissor, &sample);
    }

    /// Rasterizes one triangle with barycentric interpolation.
    fn raster_triangle<F>(
        &mut self,
        node: &RenderNode,
        triangle: [Vertex; 3],
        opacity: f32,
        scissor: PixelRegion,
        sample: &F,
    ) where
        F: Fn((f64, f64)) -> [f32; 4],
    {
        let [a, mut b, mut c] = triangle;
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
                self.compose(x, y, node, src, opacity);
            }
        }
    }

    /// Composites one straight-alpha source pixel over the destination.
    fn compose(&mut self, x: u32, y: u32, node: &RenderNode, src: [f32; 4], opacity: f32) {
        let index = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        let dst = [
            self.pixels[index],
            self.pixels[index + 1],
            self.pixels[index + 2],
            self.pixels[index + 3],
        ];
        let out = composite(node.blend, dst, src, opacity);
        let alpha = if node.hold_alpha { dst[3] } else { out[3] };
        self.pixels[index] = out[0];
        self.pixels[index + 1] = out[1];
        self.pixels[index + 2] = out[2];
        self.pixels[index + 3] = alpha;
    }
}

/// Composites `src` over `dst` using `mode`, returning straight-alpha RGBA8.
///
/// `src[3]` is the source's own alpha and `opacity` is the node's composed
/// opacity; the source's effective alpha is their product. The alpha a blend
/// leaves is the blend's alpha face. The source-over writes the source's
/// alpha, the plain copy takes the source's alpha, and every other blend is
/// colour-only: it keeps the destination's alpha.
fn composite(mode: BlendMode, dst: [u8; 4], src: [f32; 4], opacity: f32) -> [u8; 4] {
    let a = (src[3] * opacity).clamp(0.0, 1.0);
    let d = [
        dst[0] as f32 / 255.0,
        dst[1] as f32 / 255.0,
        dst[2] as f32 / 255.0,
    ];
    let s = [src[0], src[1], src[2]];
    match mode {
        // The plain copy: the source's colour and the source's alpha replace
        // the destination's.
        BlendMode::Replace => [to_u8(s[0]), to_u8(s[1]), to_u8(s[2]), to_u8(a)],
        // The additive blend: the colour is `src·a + dst` and the blend
        // keeps the destination's alpha.
        BlendMode::Additive => {
            let rgb = [
                (d[0] + s[0] * a).clamp(0.0, 1.0),
                (d[1] + s[1] * a).clamp(0.0, 1.0),
                (d[2] + s[2] * a).clamp(0.0, 1.0),
            ];
            [to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2]), dst[3]]
        }
        // The additive-alpha blend: the destination is scaled by the
        // source's remaining alpha and the source's colour is added. The
        // blend is colour-only and keeps the destination's alpha.
        BlendMode::AddAlpha => {
            let rgb = [
                (d[0] * (1.0 - a) + s[0]).clamp(0.0, 1.0),
                (d[1] * (1.0 - a) + s[1]).clamp(0.0, 1.0),
                (d[2] * (1.0 - a) + s[2]).clamp(0.0, 1.0),
            ];
            [to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2]), dst[3]]
        }
        // The subtractive blend: the colour is the saturating difference
        // `dst - src·a` and the blend keeps the destination's alpha.
        BlendMode::Subtractive => {
            let rgb = [
                (d[0] - s[0] * a).clamp(0.0, 1.0),
                (d[1] - s[1] * a).clamp(0.0, 1.0),
                (d[2] - s[2] * a).clamp(0.0, 1.0),
            ];
            [to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2]), dst[3]]
        }
        // The source-over: the colour is `src·a + dst·(1 - a)` and the
        // result's alpha is the same mix.
        BlendMode::SourceOver => {
            let rgb = lerp3(d, s, a);
            let alpha = over_alpha(dst[3] as f32 / 255.0, a);
            [to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2]), to_u8(alpha)]
        }
        // The multiplicative and the screen blend: the colour is the
        // per-channel product or the screen blend, the source's alpha does
        // not weigh in, and the destination's alpha is kept.
        BlendMode::Multiply => {
            let rgb = [
                multiply_channel(d[0], s[0]),
                multiply_channel(d[1], s[1]),
                multiply_channel(d[2], s[2]),
            ];
            [to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2]), dst[3]]
        }
        BlendMode::Screen => {
            let rgb = [
                screen_channel(d[0], s[0]),
                screen_channel(d[1], s[1]),
                screen_channel(d[2], s[2]),
            ];
            [to_u8(rgb[0]), to_u8(rgb[1]), to_u8(rgb[2]), dst[3]]
        }
        // The Photoshop family: the shared table's modes.
        BlendMode::PsNormal => photoshop(PhotoshopBlend::NORMAL, dst, src, opacity),
        BlendMode::PsAdditive => photoshop(PhotoshopBlend::ADDITIVE, dst, src, opacity),
        BlendMode::PsSubtractive => photoshop(PhotoshopBlend::SUBTRACTIVE, dst, src, opacity),
        BlendMode::PsMultiplicative => photoshop(PhotoshopBlend::MULTIPLICATIVE, dst, src, opacity),
        BlendMode::PsScreen => photoshop(PhotoshopBlend::SCREEN, dst, src, opacity),
        // The destination-dependent blends are the rest of the Photoshop
        // family.
        BlendMode::DestinationDependent(kind) => {
            let mode = match kind {
                DstBlend::Overlay => PhotoshopBlend::OVERLAY,
                DstBlend::HardLight => PhotoshopBlend::HARD_LIGHT,
                DstBlend::SoftLight => PhotoshopBlend::SOFT_LIGHT,
                // The plain colour dodge, the plain darken and the plain
                // lighten read the family's table here; the engine's own path
                // runs the plain kernels for them.
                DstBlend::ColorDodge | DstBlend::PsColorDodge => PhotoshopBlend::COLOR_DODGE,
                DstBlend::ColorDodge5 => PhotoshopBlend::COLOR_DODGE5,
                DstBlend::ColorBurn => PhotoshopBlend::COLOR_BURN,
                DstBlend::Lighten | DstBlend::PsLighten => PhotoshopBlend::LIGHTEN,
                DstBlend::Darken | DstBlend::PsDarken => PhotoshopBlend::DARKEN,
                DstBlend::Difference => PhotoshopBlend::DIFFERENCE,
                DstBlend::Difference5 => PhotoshopBlend::DIFFERENCE5,
                DstBlend::Exclusion => PhotoshopBlend::EXCLUSION,
            };
            photoshop(mode, dst, src, opacity)
        }
    }
}

// ---------------------------------------------------------------------------
// The Photoshop blend family
// ---------------------------------------------------------------------------

/// Packs four RGBA8 bytes into the 32-bit ARGB word the blend table works
/// on.
fn argb_word(pixel: [u8; 4]) -> u32 {
    (u32::from(pixel[3]) << 24)
        | (u32::from(pixel[0]) << 16)
        | (u32::from(pixel[1]) << 8)
        | u32::from(pixel[2])
}

/// The Photoshop blend family: the shared table's modes, computed on the
/// 32-bit ARGB word.
///
/// The table owns the family's per-channel functions and the way the
/// source's alpha mixes them into the destination, so this renderer, the
/// engine's bitmap path and the destination-dependent program all run the
/// same rules.
///
/// The source's alpha, scaled by the opacity, mixes the per-channel result
/// into the destination, and the destination's alpha is kept.
fn photoshop(mode: PhotoshopBlend, dst: [u8; 4], src: [f32; 4], opacity: f32) -> [u8; 4] {
    let destination = argb_word(dst);
    let source = argb_word([to_u8(src[0]), to_u8(src[1]), to_u8(src[2]), to_u8(src[3])]);
    let word = mode.blend_word(destination, source, to_u8(opacity));
    [(word >> 16) as u8, (word >> 8) as u8, word as u8, dst[3]]
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

/// The per-channel product of the destination and the source.
fn multiply_channel(d: f32, s: f32) -> f32 {
    (d.clamp(0.0, 1.0) * s.clamp(0.0, 1.0)).clamp(0.0, 1.0)
}

/// The per-channel screen blend of the destination and the source.
fn screen_channel(d: f32, s: f32) -> f32 {
    let d = d.clamp(0.0, 1.0);
    let s = s.clamp(0.0, 1.0);
    (d + s - d * s).clamp(0.0, 1.0)
}

/// Samples a texture at texel coordinates `(u, v)` with point (nearest)
/// filtering.
///
/// This is the default layer blit sampling: the software renderer starts at
/// the nearest-neighbour stretch type and selects the point copy for anything
/// below the fast-linear threshold. The GPU backend uses the same rule, so a
/// magnified layer is point-scaled rather than interpolated.
fn sample_texture(view: &CpuTexture<'_>, u: f64, v: f64, tint: [f32; 4]) -> [f32; 4] {
    let format = view.format;
    let width = view.width as i64;
    let height = view.height as i64;
    let ix = (u.floor() as i64).clamp(0, width - 1) as usize;
    let iy = (v.floor() as i64).clamp(0, height - 1) as usize;
    let index = (iy * view.width as usize + ix) * format.bytes_per_pixel() as usize;
    match format {
        TextureFormat::Rgba8Srgb | TextureFormat::Rgba8Unorm => {
            let pixel = &view.pixels[index..index + 4];
            [
                pixel[0] as f32 / 255.0 * tint[0],
                pixel[1] as f32 / 255.0 * tint[1],
                pixel[2] as f32 / 255.0 * tint[2],
                pixel[3] as f32 / 255.0 * tint[3],
            ]
        }
        TextureFormat::R8Unorm => {
            let coverage = view.pixels[index] as f32 / 255.0;
            [tint[0], tint[1], tint[2], coverage * tint[3]]
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Composites one pixel and encodes it as RGBA8, matching
    /// [`Canvas::compose`].
    fn blend_u8(mode: BlendMode, dst: [u8; 4], src: [u8; 4], opacity: u8) -> [u8; 4] {
        let source = [
            src[0] as f32 / 255.0,
            src[1] as f32 / 255.0,
            src[2] as f32 / 255.0,
            src[3] as f32 / 255.0,
        ];
        composite(mode, dst, source, opacity as f32 / 255.0)
    }

    /// A texture provider without any texture.
    struct NoTextures;

    impl TextureProvider for NoTextures {
        fn texture(&self, _key: u64) -> Option<CpuTexture<'_>> {
            None
        }
    }

    /// Renders one solid source pixel over one destination pixel with `mode`
    /// and returns the resulting RGBA8 pixel.
    fn render_pixel(mode: BlendMode, dest: [u8; 4], source: [u8; 4], opacity: u8) -> [u8; 4] {
        let mut scene = RenderScene::with_root_node();
        let root = scene.root;
        let child = scene.push(RenderNode {
            parent: Some(root),
            content: RenderContent::Solid([
                source[0] as f32 / 255.0,
                source[1] as f32 / 255.0,
                source[2] as f32 / 255.0,
                source[3] as f32 / 255.0,
            ]),
            dst: RectF::new(0.0, 0.0, 1.0, 1.0),
            opacity: opacity as f32 / 255.0,
            blend: mode,
            ..RenderNode::default()
        });
        scene.node_mut(root).expect("the root node").children = vec![child];
        let background = [
            dest[0] as f32 / 255.0,
            dest[1] as f32 / 255.0,
            dest[2] as f32 / 255.0,
            dest[3] as f32 / 255.0,
        ];
        let mut pixels = [0u8; 4];
        render_scene(1, 1, &mut pixels, Some(background), &scene, &NoTextures).expect("the frame");
        pixels
    }

    /// The vectors the Photoshop blend family is checked against: a source
    /// pixel, a destination pixel and a pass opacity.
    const VECTORS: &[([u8; 4], [u8; 4], u8)] = &[
        ([64, 128, 192, 255], [200, 100, 50, 128], 255),
        ([60, 180, 90, 192], [200, 100, 50, 128], 255),
        ([10, 20, 30, 40], [200, 190, 180, 170], 255),
        ([10, 20, 30, 40], [200, 190, 180, 170], 128),
        ([60, 180, 90, 192], [200, 100, 50, 128], 128),
        ([255, 255, 255, 255], [0, 0, 0, 255], 255),
        ([0, 0, 0, 0], [200, 100, 50, 128], 255),
        ([128, 128, 128, 255], [128, 128, 128, 255], 255),
    ];

    /// The expected result of every mode for every vector.
    const EXPECTED: &[(BlendMode, [[u8; 4]; 8])] = &[
        (
            BlendMode::PsNormal,
            [
                [64, 127, 191, 128],
                [95, 160, 80, 128],
                [170, 163, 156, 170],
                [185, 176, 168, 170],
                [147, 130, 65, 128],
                [254, 254, 254, 255],
                [200, 100, 50, 128],
                [128, 128, 128, 255],
            ],
        ),
        (
            BlendMode::PsAdditive,
            [
                [254, 227, 241, 128],
                [241, 216, 117, 128],
                [201, 193, 184, 170],
                [200, 191, 182, 170],
                [220, 158, 83, 128],
                [254, 254, 254, 255],
                [200, 100, 50, 128],
                [254, 254, 254, 255],
            ],
        ),
        (
            BlendMode::PsSubtractive,
            [
                [9, 0, 0, 128],
                [53, 43, 12, 128],
                [168, 160, 151, 170],
                [184, 175, 165, 170],
                [126, 71, 31, 128],
                [0, 0, 0, 255],
                [200, 100, 50, 128],
                [1, 1, 1, 255],
            ],
        ),
        (
            BlendMode::PsMultiplicative,
            [
                [50, 50, 37, 128],
                [84, 77, 25, 128],
                [169, 162, 155, 170],
                [184, 176, 167, 170],
                [142, 88, 37, 128],
                [0, 0, 0, 255],
                [200, 100, 50, 128],
                [64, 64, 64, 255],
            ],
        ),
        (
            BlendMode::PsScreen,
            [
                [213, 177, 204, 128],
                [210, 182, 104, 128],
                [200, 190, 181, 170],
                [200, 190, 180, 170],
                [205, 141, 77, 128],
                [254, 254, 254, 255],
                [200, 100, 50, 128],
                [191, 191, 191, 255],
            ],
        ),
        (
            BlendMode::DestinationDependent(DstBlend::Overlay),
            [
                [173, 100, 74, 128],
                [178, 130, 38, 128],
                [192, 181, 171, 170],
                [196, 185, 175, 170],
                [189, 115, 44, 128],
                [0, 0, 0, 255],
                [200, 100, 50, 128],
                [128, 128, 128, 255],
            ],
        ),
        (
            BlendMode::DestinationDependent(DstBlend::HardLight),
            [
                [100, 100, 153, 128],
                [120, 148, 38, 128],
                [171, 164, 158, 170],
                [185, 177, 169, 170],
                [160, 124, 44, 128],
                [254, 254, 254, 255],
                [200, 100, 50, 128],
                [128, 128, 128, 255],
            ],
        ),
        (
            BlendMode::DestinationDependent(DstBlend::SoftLight),
            [
                [177, 100, 85, 128],
                [181, 123, 35, 128],
                [193, 183, 173, 170],
                [196, 186, 176, 170],
                [190, 111, 42, 128],
                [0, 0, 0, 255],
                [200, 100, 50, 128],
                [128, 128, 128, 255],
            ],
        ),
        (
            BlendMode::DestinationDependent(DstBlend::PsColorDodge),
            [
                [254, 199, 201, 128],
                [241, 216, 70, 128],
                [201, 192, 183, 170],
                [200, 191, 181, 170],
                [220, 158, 60, 128],
                [254, 254, 254, 255],
                [200, 100, 50, 128],
                [254, 254, 254, 255],
            ],
        ),
        (
            BlendMode::DestinationDependent(DstBlend::ColorDodge5),
            [
                [255, 199, 199, 128],
                [242, 212, 67, 128],
                [200, 192, 182, 170],
                [200, 190, 181, 170],
                [218, 135, 57, 128],
                [0, 0, 0, 255],
                [200, 100, 50, 128],
                [255, 255, 255, 255],
            ],
        ),
        (
            BlendMode::DestinationDependent(DstBlend::ColorBurn),
            [
                [36, 0, 0, 128],
                [66, 52, 12, 128],
                [168, 160, 151, 170],
                [184, 175, 165, 170],
                [133, 76, 31, 128],
                [0, 0, 0, 255],
                [200, 100, 50, 128],
                [2, 2, 2, 255],
            ],
        ),
        (
            BlendMode::DestinationDependent(DstBlend::PsLighten),
            [
                [200, 127, 191, 128],
                [200, 160, 80, 128],
                [200, 190, 180, 170],
                [200, 190, 180, 170],
                [200, 130, 65, 128],
                [254, 254, 254, 255],
                [200, 100, 50, 128],
                [128, 128, 128, 255],
            ],
        ),
        (
            BlendMode::DestinationDependent(DstBlend::PsDarken),
            [
                [64, 100, 50, 128],
                [95, 100, 50, 128],
                [170, 163, 156, 170],
                [185, 176, 168, 170],
                [147, 100, 50, 128],
                [0, 0, 0, 255],
                [200, 100, 50, 128],
                [128, 128, 128, 255],
            ],
        ),
        (
            BlendMode::DestinationDependent(DstBlend::Difference),
            [
                [136, 28, 141, 128],
                [155, 85, 42, 128],
                [198, 186, 175, 170],
                [199, 188, 177, 170],
                [177, 92, 46, 128],
                [254, 254, 254, 255],
                [200, 100, 50, 128],
                [0, 0, 0, 255],
            ],
        ),
        (
            BlendMode::DestinationDependent(DstBlend::Difference5),
            [
                [137, 27, 141, 128],
                [155, 35, 17, 128],
                [199, 187, 176, 170],
                [200, 189, 178, 170],
                [178, 33, 17, 128],
                [254, 254, 254, 255],
                [200, 100, 50, 128],
                [1, 1, 1, 255],
            ],
        ),
        (
            BlendMode::DestinationDependent(DstBlend::Exclusion),
            [
                [164, 127, 166, 128],
                [175, 130, 91, 128],
                [199, 188, 178, 170],
                [199, 189, 179, 170],
                [187, 115, 70, 128],
                [254, 254, 254, 255],
                [200, 100, 50, 128],
                [128, 128, 128, 255],
            ],
        ),
    ];

    /// Reference: the default stretch blit samples the nearest source texel
    /// (`reference/cpp/core/visual/RenderManager.cpp:2902` starts at
    /// `StretchType(stNearest)` and `:3191-3226` selects the point
    /// `TVPStretchCopy` for anything below `stFastLinear`). A bilinear sampler
    /// would blend the neighbouring texels instead.
    #[test]
    fn default_layer_sampling_picks_the_nearest_texel() {
        // Four distinct corners: black, red / green, blue.
        let pixels = [
            0u8, 0, 0, 255, 255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255,
        ];
        let texture = CpuTexture {
            pixels: &pixels,
            width: 2,
            height: 2,
            format: TextureFormat::Rgba8Unorm,
        };
        let opaque = [1.0f32; 4];
        let sample = |u: f64, v: f64| sample_texture(&texture, u, v, opaque).map(to_u8);
        // Anywhere inside a texel's cell returns that texel unchanged; a
        // bilinear read partway into a cell would return a blend of corners.
        assert_eq!(sample(0.25, 0.25), [0, 0, 0, 255]);
        assert_eq!(sample(1.75, 0.25), [255, 0, 0, 255]);
        assert_eq!(sample(0.75, 1.75), [0, 255, 0, 255]);
        assert_eq!(sample(1.25, 1.25), [0, 0, 255, 255]);
        // A coordinate on a texel boundary belongs to the higher texel.
        assert_eq!(sample(1.0, 0.0), [255, 0, 0, 255]);
    }

    /// Reference: every Photoshop mode has its own per-channel function, and
    /// the classic kernels compute them on the 32-bit ARGB word
    /// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-660`); the
    /// soft-light, colour-dodge, colour-burn and overlay tables are
    /// `reference/krkr2/cpp/core/visual/gl/blend_function.cpp:526-547`. The mix
    /// factor is the source's alpha, scaled by the opacity below the full one
    /// (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:95-107`), and the
    /// destination's alpha is held
    /// (`reference/krkr2/cpp/core/visual/gl/blend_variation.h:27-141`).
    ///
    /// The word's lanes share their carries, so the byte arithmetic is the
    /// classic one and not a per-lane approximation of it.
    #[test]
    fn the_photoshop_blend_family_matches_the_classic_kernels() {
        for (mode, expected) in EXPECTED {
            for (index, (source, dest, opacity)) in VECTORS.iter().enumerate() {
                assert_eq!(
                    render_pixel(*mode, *dest, *source, *opacity),
                    expected[index],
                    "the mode {mode:?} on vector {index}"
                );
            }
        }
    }

    /// Reference: the legacy Photoshop 5.x modes fade the source first and
    /// replace the channel; they do not mix it into the destination
    /// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:568-581`,
    /// `:630-642`).
    #[test]
    fn the_legacy_photoshop_modes_replace_the_channel() {
        // The destination is (200, 100, 50, 128) and the source
        // (60, 180, 90, 192): the source fades by the mix factor of 192 to
        // (45, 135, 67) and the colour dodge of the legacy mode replaces the
        // channel with (242, 212, 67).
        assert_eq!(
            render_pixel(
                BlendMode::DestinationDependent(DstBlend::ColorDodge5),
                [200, 100, 50, 128],
                [60, 180, 90, 192],
                255,
            ),
            [242, 212, 67, 128]
        );
        // The legacy difference replaces the channel with the absolute
        // difference of the faded source: (155, 35, 17).
        assert_eq!(
            render_pixel(
                BlendMode::DestinationDependent(DstBlend::Difference5),
                [200, 100, 50, 128],
                [60, 180, 90, 192],
                255,
            ),
            [155, 35, 17, 128]
        );
    }

    /// Reference: the Photoshop normal blend mixes the colours by the
    /// source's alpha and keeps the destination's alpha
    /// (`reference/krkr2/cpp/core/visual/gl/blend_functor_c.h:374-383`); the
    /// additive blend saturates the per-channel sum before that mix
    /// (`:386-397`) and the subtractive blend is the linear burn, the source
    /// inverted and the difference saturated (`:401-412`).
    #[test]
    fn the_photoshop_blends_mix_by_the_source_alpha() {
        let dst = [64, 128, 191, 255];
        let src = [191, 128, 77, 255];
        // The classic kernels truncate by eight bits, so the red channel of
        // the normal blend is `64 + ((191 - 64) * 255 >> 8) = 190`.
        let opaque: &[(BlendMode, [u8; 4])] = &[
            (BlendMode::PsNormal, [190, 128, 77, 255]),
            (BlendMode::PsAdditive, [254, 254, 254, 255]),
            (BlendMode::PsSubtractive, [0, 1, 13, 255]),
            (BlendMode::PsMultiplicative, [47, 64, 57, 255]),
            (BlendMode::PsScreen, [207, 191, 210, 255]),
            (
                BlendMode::DestinationDependent(DstBlend::PsColorDodge),
                [254, 254, 254, 255],
            ),
            (
                BlendMode::DestinationDependent(DstBlend::PsDarken),
                [64, 128, 77, 255],
            ),
            (
                BlendMode::DestinationDependent(DstBlend::PsLighten),
                [190, 128, 191, 255],
            ),
        ];
        for (mode, expected) in opaque {
            assert_eq!(
                blend_u8(*mode, dst, src, 255),
                *expected,
                "opaque blend {mode:?}"
            );
        }

        // A half-transparent source mixes the per-channel result into the
        // destination by its alpha of 128.
        let half_dst = [51, 51, 51, 255];
        let half_src = [230, 230, 230, 128];
        let half: &[(BlendMode, [u8; 4])] = &[
            (BlendMode::PsNormal, [140, 140, 140, 255]),
            (BlendMode::PsAdditive, [153, 153, 153, 255]),
            (BlendMode::PsSubtractive, [38, 38, 38, 255]),
            (BlendMode::PsMultiplicative, [48, 48, 48, 255]),
            (BlendMode::PsScreen, [143, 143, 143, 255]),
        ];
        for (mode, expected) in half {
            assert_eq!(
                blend_u8(*mode, half_dst, half_src, 255),
                *expected,
                "half-alpha blend {mode:?}"
            );
        }

        // The Photoshop additive saturates before the alpha blend, so it is
        // not the non-Photoshop additive.
        assert_ne!(
            blend_u8(BlendMode::Additive, half_dst, half_src, 255),
            [153, 153, 153, 255]
        );

        // The Photoshop subtractive blend is the linear burn, so it is not
        // the non-Photoshop subtractive blend either.
        assert_ne!(
            blend_u8(BlendMode::Subtractive, half_dst, half_src, 255),
            blend_u8(BlendMode::PsSubtractive, half_dst, half_src, 255)
        );
    }
}
