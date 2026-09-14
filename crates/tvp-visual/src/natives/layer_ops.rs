//! In-place pixel operations behind the `LayerExDraw` / `layerExImage`
//! surface of [`super::layer`].
//!
//! The scene stores straight-alpha RGBA8 (`r, g, b, a` per pixel), which is
//! the same pixel model [`super::raster::blend_pixel`] uses. The reference's
//! software bitmap is 32-bit ARGB in the opposite channel order; every
//! function here is the reference math with the channels mapped through
//! `argb_to_rgba` (see [`super::layer::argb_to_rgba`]).
//!
//! | function | reference |
//! |---|---|
//! | [`fill_color_on_alpha`] | `iTVPBaseBitmap::FillColorOnAlpha` → `BlendColor` → `TVPConstColorAlphaBlend_d` |
//! | [`fill_color_on_add_alpha`] | `iTVPBaseBitmap::FillColorOnAddAlpha` → `TVPConstColorAlphaBlend_a` |
//! | [`fill_color_hold_alpha`] | `iTVPBaseBitmap::FillColor` (destination alpha held) |
//! | [`fill_mask`] | `iTVPBaseBitmap::FillMask` |
//! | [`remove_const_opacity`] | `iTVPBaseBitmap::RemoveConstOpacity` → `TVPRemoveConstOpacity` |
//! | [`colorize`] | `layerExImage::colorize` (`reference/cpp/plugins/.../layerExImage`) |
//! | [`noise`] | `layerExImage::noise` |
//! | [`blit_copy`] | `iTVPBaseBitmap::CopyRect` |
//! | [`tile_rect`] | SDK `Layer.tileRect` (a `copyRect` loop) |
//! | [`do_drop_shadow`] / [`do_blur_light`] | SDK `Layer.doDropShadow` / `doBlurLight` |
//! | [`fill_operate_rect`] | SDK `Layer.fillOperateRect` (a tiled `operateRect`) |
//!
//! `colorize` / `noise` operate on the layer's current `ClipRect` (the
//! reference `layerExImage::reset` rebases its buffer on the clip), so the
//! callers pass the already-clipped region.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::scene::{BitmapState, Scene};

use super::raster::blend_pixel;

/// Half-open pixel rectangle `[x0, x1) x [y0, y1)`.
pub(crate) type RectI = (i32, i32, i32, i32);

/// `TVP_BB_COPY_MAIN` (`LayerBitmapIntf.h:127`): copy the RGB plane only
/// (the reference `CopyColor` render method; the destination alpha is held).
pub(crate) const COPY_MAIN: u8 = 1;
/// `TVP_BB_COPY_MASK` (`LayerBitmapIntf.h:128`): copy the alpha plane only
/// (the reference `CopyMask` render method; the destination RGB is held).
pub(crate) const COPY_MASK: u8 = 2;

/// Intersect `rect` with the bitmap's bounds, returning `None` when empty.
pub(crate) fn intersect(bitmap: &BitmapState, rect: RectI) -> Option<RectI> {
    let (x0, y0, x1, y1) = rect;
    let ix0 = x0.max(0);
    let iy0 = y0.max(0);
    let ix1 = x1.min(bitmap.width as i32);
    let iy1 = y1.min(bitmap.height as i32);
    (ix0 < ix1 && iy0 < iy1).then_some((ix0, iy0, ix1, iy1))
}

/// Intersect two half-open rectangles, returning `None` when empty.
pub(crate) fn intersect_rect(a: RectI, b: RectI) -> Option<RectI> {
    let r = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    (r.0 < r.2 && r.1 < r.3).then_some(r)
}

/// `TVPOpacityOnOpacityTable` value. `dopa` is the destination alpha, `opa`
/// the source opacity. Port of `TVPCreateTable` (`tvpgl.cpp`): the effective
/// color-blend factor for a source-over composite. A fully transparent
/// destination collapses to `255` (the source color is written unattenuated).
fn opacity_on_opacity(dopa: u8, opa: u8) -> u8 {
    if dopa == 0 {
        return 255;
    }
    // The reference computes this with `float` (see `TVPCreateTable`); the
    // operation order is reproduced so the truncation matches.
    let at = f32::from(dopa) / 255.0;
    let bt = f32::from(opa) / 255.0;
    let c = bt / at;
    let c = c / (1.0 - bt + c);
    let ci = (c * 255.0) as i32;
    ci.clamp(0, 255) as u8
}

/// The per-channel color blend shared by the `_d`/`_a` constant-color
/// routines: `d + ((s - d) * alpha >> 8)` with an arithmetic (flooring)
/// shift, matching the C unsigned-shift trick.
fn blend_channel(d: u8, s: u8, alpha: u8) -> u8 {
    let out = i32::from(d) + (((i32::from(s) - i32::from(d)) * i32::from(alpha)) >> 8);
    out.clamp(0, 255) as u8
}

fn write_pixel(bitmap: &mut BitmapState, x: i32, y: i32, rgba: [u8; 4]) {
    if let Some(i) = bitmap.pixel_offset(x as u32, y as u32) {
        bitmap.rgba[i..i + 4].copy_from_slice(&rgba);
    }
}

fn read_pixel(bitmap: &BitmapState, x: i32, y: i32) -> [u8; 4] {
    match bitmap.pixel_offset(x as u32, y as u32) {
        Some(i) => [
            bitmap.rgba[i],
            bitmap.rgba[i + 1],
            bitmap.rgba[i + 2],
            bitmap.rgba[i + 3],
        ],
        None => [0, 0, 0, 0],
    }
}

/// `FillColorOnAlpha`: source-over a constant color that **considers the
/// destination alpha**, exactly like `TVPConstColorAlphaBlend_d`.
///
/// `color` is straight RGBA. `opa` semantics follow `ColorRect`:
/// * `opa <= 0` does nothing,
/// * `opa >= 255` writes the RGB with a forced `255` alpha (`FillARGB`),
/// * otherwise RGB is blended with the destination's effective opacity while
///   the output alpha is `255 - ((255 - dst_a) * (255 - opa) >> 8)`.
pub(crate) fn fill_color_on_alpha(bitmap: &mut BitmapState, rect: RectI, color: [u8; 4], opa: i32) {
    if opa <= 0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    if opa >= 255 {
        let rgba = [color[0], color[1], color[2], 255];
        for y in y0..y1 {
            for x in x0..x1 {
                write_pixel(bitmap, x, y, rgba);
            }
        }
        bitmap.mark_dirty();
        return;
    }
    let opa = opa as u8;
    for y in y0..y1 {
        for x in x0..x1 {
            let d = read_pixel(bitmap, x, y);
            let alpha = opacity_on_opacity(d[3], opa);
            let out_a = 255 - (((255 - i32::from(d[3])) * (255 - i32::from(opa))) >> 8);
            let mut out = [0u8; 4];
            for c in 0..3 {
                out[c] = blend_channel(d[c], color[c], alpha);
            }
            out[3] = out_a.clamp(0, 255) as u8;
            write_pixel(bitmap, x, y, out);
        }
    }
    bitmap.mark_dirty();
}

/// Saturating per-channel addition of packed 8-bit values, a port of
/// `TVPSaturatedAdd` (`tvpgl.h`).
fn saturated_add(a: u32, b: u32) -> u32 {
    let tmp = ((a & b) + (((a ^ b) >> 1) & 0x7f7f_7f7f)) & 0x8080_8080;
    let tmp = (tmp << 1) - (tmp >> 7);
    (a + b - tmp) | tmp
}

/// `TVPConstColorAlphaBlend_a` (`FillColorOnAddAlpha`): additive-alpha
/// (premultiplied) fill.
pub(crate) fn fill_color_on_add_alpha(
    bitmap: &mut BitmapState,
    rect: RectI,
    color: [u8; 4],
    opa: i32,
) {
    if opa <= 0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    let opa = opa.min(255) as u32;
    // `TVPMulColor(color & 0xffffff, opa)`: scale each RGB channel by
    // `(channel * opa) >> 8`.
    let src = (((color[0] as u32 * opa) >> 8) << 16)
        | (((color[1] as u32 * opa) >> 8) << 8)
        | ((color[2] as u32 * opa) >> 8);
    let opa_inv = opa ^ 0xff;
    for y in y0..y1 {
        for x in x0..x1 {
            let d = read_pixel(bitmap, x, y);
            let mut dest = (u32::from(d[3]) << 24)
                | (u32::from(d[0]) << 16)
                | (u32::from(d[1]) << 8)
                | u32::from(d[2]);
            let mut dopa = dest >> 24;
            dopa = dopa + opa - ((dopa * opa) >> 8);
            dopa -= dopa >> 8;
            let scaled = ((((dest & 0x00ff_00ff) * opa_inv) >> 8) & 0x00ff_00ff)
                + ((((dest & 0x0000_ff00) * opa_inv) >> 8) & 0x0000_ff00);
            dest = (dopa << 24) + saturated_add(scaled, src);
            write_pixel(
                bitmap,
                x,
                y,
                [
                    ((dest >> 16) & 0xff) as u8,
                    ((dest >> 8) & 0xff) as u8,
                    (dest & 0xff) as u8,
                    ((dest >> 24) & 0xff) as u8,
                ],
            );
        }
    }
    bitmap.mark_dirty();
}

/// `iTVPBaseBitmap::FillColor`: set the RGB to `color` while **holding the
/// destination alpha**. `opa` 0 does nothing; `255` replaces RGB; otherwise
/// `TVPConstColorAlphaBlend_c` blends `(d * (255 - opa) + s * opa) >> 8`.
pub(crate) fn fill_color_hold_alpha(
    bitmap: &mut BitmapState,
    rect: RectI,
    color: [u8; 4],
    opa: i32,
) {
    if opa <= 0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    let opa = opa.min(255) as u32;
    for y in y0..y1 {
        for x in x0..x1 {
            let mut d = read_pixel(bitmap, x, y);
            if opa == 255 {
                d[0] = color[0];
                d[1] = color[1];
                d[2] = color[2];
            } else {
                let inv = 255 - opa;
                for c in 0..3 {
                    d[c] =
                        (((u32::from(d[c]) * inv + u32::from(color[c]) * opa) >> 8) & 0xff) as u8;
                }
            }
            write_pixel(bitmap, x, y, d);
        }
    }
    bitmap.mark_dirty();
}

/// `iTVPBaseBitmap::FillMask`: replace the alpha channel with `value`,
/// holding the RGB. In this engine the alpha channel *is* the mask plane.
pub(crate) fn fill_mask(bitmap: &mut BitmapState, rect: RectI, value: u8) {
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    for y in y0..y1 {
        for x in x0..x1 {
            let mut d = read_pixel(bitmap, x, y);
            d[3] = value;
            write_pixel(bitmap, x, y, d);
        }
    }
    bitmap.mark_dirty();
}

/// `RemoveConstOpacity`: multiply the alpha by `1 - level / 255`
/// (`TVPRemoveConstOpacity` uses `(alpha * (255 - level)) >> 8`). `level <= 0`
/// does nothing, `level >= 255` clears the alpha.
pub(crate) fn remove_const_opacity(bitmap: &mut BitmapState, rect: RectI, level: i32) {
    if level <= 0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    let strength = 255 - level.min(255) as u32;
    for y in y0..y1 {
        for x in x0..x1 {
            let mut d = read_pixel(bitmap, x, y);
            d[3] = ((u32::from(d[3]) * strength) >> 8) as u8;
            write_pixel(bitmap, x, y, d);
        }
    }
    bitmap.mark_dirty();
}

/// RGB → HSL with the byte ranges used by `layerExImage` (`H`, `S`, `L`
/// are all 0..=255, `H` is 0..=255 ≈ 0..=360°).
fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (u8, u8, u8) {
    const HSLMAX: u32 = 255;
    const RGBMAX: u32 = 255;
    let (r, g, b) = (u32::from(r), u32::from(g), u32::from(b));
    let c_max = r.max(g).max(b);
    let c_min = r.min(g).min(b);
    let l = ((c_max + c_min) * HSLMAX + RGBMAX) / (2 * RGBMAX);
    if c_max == c_min {
        return ((HSLMAX * 2 / 3) as u8, 0, l as u8);
    }
    let s = if l <= HSLMAX / 2 {
        ((c_max - c_min) * HSLMAX + (c_max + c_min) / 2) / (c_max + c_min)
    } else {
        ((c_max - c_min) * HSLMAX + (2 * RGBMAX - c_max - c_min) / 2) / (2 * RGBMAX - c_max - c_min)
    };
    let r_delta = ((c_max - r) * (HSLMAX / 6) + (c_max - c_min) / 2) / (c_max - c_min);
    let g_delta = ((c_max - g) * (HSLMAX / 6) + (c_max - c_min) / 2) / (c_max - c_min);
    let b_delta = ((c_max - b) * (HSLMAX / 6) + (c_max - c_min) / 2) / (c_max - c_min);
    let mut h = if r == c_max {
        b_delta as i32 - g_delta as i32
    } else if g == c_max {
        (HSLMAX / 3) as i32 + r_delta as i32 - b_delta as i32
    } else {
        (2 * HSLMAX / 3) as i32 + g_delta as i32 - r_delta as i32
    };
    if h > HSLMAX as i32 {
        h -= HSLMAX as i32;
    }
    (h as u8, s as u8, l as u8)
}

fn hue_to_rgb(n1: f32, n2: f32, mut hue: f32) -> f32 {
    if hue > 360.0 {
        hue -= 360.0;
    } else if hue < 0.0 {
        hue += 360.0;
    }
    if hue < 60.0 {
        n1 + (n2 - n1) * hue / 60.0
    } else if hue < 180.0 {
        n2
    } else if hue < 240.0 {
        n1 + (n2 - n1) * (240.0 - hue) / 60.0
    } else {
        n1
    }
}

/// HSL → RGB (the `layerExImage` byte-packed variant).
fn hsl_to_rgb(h: u8, s: u8, l: u8) -> (u8, u8, u8) {
    let h = f32::from(h) * 360.0 / 255.0;
    let s = f32::from(s) / 255.0;
    let l = f32::from(l) / 255.0;
    let m2 = if l <= 0.5 {
        l * (1.0 + s)
    } else {
        l + s - l * s
    };
    let m1 = 2.0 * l - m2;
    if s == 0.0 {
        let v = (l * 255.0) as u8;
        (v, v, v)
    } else {
        (
            (hue_to_rgb(m1, m2, h + 120.0) * 255.0) as u8,
            (hue_to_rgb(m1, m2, h) * 255.0) as u8,
            (hue_to_rgb(m1, m2, h - 120.0) * 255.0) as u8,
        )
    }
}

/// `layerExImage::colorize(hue, sat, blend)` — replace/reblend the hue and
/// saturation while preserving lightness. Operates in place on the
/// already-clipped region.
pub(crate) fn colorize(bitmap: &mut BitmapState, rect: RectI, hue: i32, sat: i32, blend: f64) {
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    let blend = blend.clamp(0.0, 1.0);
    let a0 = (256.0 * blend) as i32;
    let a1 = 256 - a0;
    let full = blend > 0.999;
    for y in y0..y1 {
        for x in x0..x1 {
            let mut p = read_pixel(bitmap, x, y);
            if full {
                let (_, _, l) = rgb_to_hsl(p[0], p[1], p[2]);
                let (r, g, b) = hsl_to_rgb(hue as u8, sat as u8, l);
                p[0] = r;
                p[1] = g;
                p[2] = b;
            } else {
                let (_, _, l) = rgb_to_hsl(p[0], p[1], p[2]);
                let (hr, hg, hb) = hsl_to_rgb(hue as u8, sat as u8, l);
                p[0] = ((i32::from(hr) * a0 + i32::from(p[0]) * a1) >> 8) as u8;
                p[1] = ((i32::from(hg) * a0 + i32::from(p[1]) * a1) >> 8) as u8;
                p[2] = ((i32::from(hb) * a0 + i32::from(p[2]) * a1) >> 8) as u8;
            }
            write_pixel(bitmap, x, y, p);
        }
    }
    bitmap.mark_dirty();
}

/// A tiny deterministic xorshift PRNG. The reference `noise` uses the C
/// library `rand()`; the seed is mixed from a process-wide counter so
/// repeated calls produce fresh noise (while a single call stays
/// reproducible for a given counter value).
struct NoiseRng(u32);
impl NoiseRng {
    fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x;
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}

/// Process-wide seed mixer so consecutive `noise()` calls differ, like the
/// reference's `rand()`.
static NOISE_SEED: AtomicU64 = AtomicU64::new(0x9e37_79b9_7f4a_7c15);

/// SplitMix64 finalizer: spreads the counter's bits into a 32-bit seed.
fn mix64(mut x: u64) -> u32 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^= x >> 33;
    x as u32
}

/// `layerExImage::noise(level)` — add `±level/2` uniform noise to each RGB
/// channel, holding alpha. Operates in place on the clipped region.
pub(crate) fn noise(bitmap: &mut BitmapState, rect: RectI, level: i32) {
    if level <= 0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    let mut rng = NoiseRng(mix64(NOISE_SEED.fetch_add(1, Ordering::Relaxed)));
    for y in y0..y1 {
        for x in x0..x1 {
            let mut p = read_pixel(bitmap, x, y);
            for channel in p.iter_mut().take(3) {
                let n = ((rng.next_f32() - 0.5) * level as f32) as i32;
                *channel = (i32::from(*channel) + n).clamp(0, 255) as u8;
            }
            write_pixel(bitmap, x, y, p);
        }
    }
    bitmap.mark_dirty();
}

/// `CopyRect(dx, dy, src, sx, sy, sw, sh)` — copy a source region
/// (`src_rect` = `[sx, sy, sx+sw, sy+sh)`) to `(dx, dy)` on `dst`, clipping
/// against both bitmaps.
pub(crate) fn blit_copy(
    dst: &mut BitmapState,
    dx: i32,
    dy: i32,
    src: &BitmapState,
    src_rect: RectI,
) {
    blit_copy_clipped(
        dst,
        dx,
        dy,
        src,
        src_rect,
        (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
    );
}

/// [`blit_copy`] additionally clipped against a layer `ClipRect`.
pub(crate) fn blit_copy_clipped(
    dst: &mut BitmapState,
    dx: i32,
    dy: i32,
    src: &BitmapState,
    src_rect: RectI,
    clip: RectI,
) {
    let Some(in_bounds) = intersect(
        dst,
        (
            dx,
            dy,
            dx + (src_rect.2 - src_rect.0),
            dy + (src_rect.3 - src_rect.1),
        ),
    ) else {
        return;
    };
    let Some(dest_clip) = intersect_rect(in_bounds, clip) else {
        return;
    };
    if src.width == 0 || src.height == 0 {
        return;
    }
    for y in dest_clip.1..dest_clip.3 {
        for x in dest_clip.0..dest_clip.2 {
            let sx = src_rect.0 + (x - dx);
            let sy = src_rect.1 + (y - dy);
            if sx < 0 || sy < 0 || sx as u32 >= src.width || sy as u32 >= src.height {
                continue;
            }
            let color = read_pixel(src, sx, sy);
            write_pixel(dst, x, y, color);
        }
    }
    dst.mark_dirty();
}

/// `tTVPBaseBitmap::Blt` source-over (`bmAlpha`/`bmAlphaOnAlpha`): composite
/// `src[src_rect]` onto `dst` at `(dx, dy)`, clipping the destination both to
/// the bitmap and to the layer's `clip` rect. Each pixel is blended with the
/// straight-alpha source-over math ([`blend_pixel`]); `opa` scales the source
/// alpha (the reference `Blt` opacity).
///
/// This is the pixel work behind `Layer.copyRect` and, with a different
/// `mode`, `operateRect`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn blit_over(
    dst: &mut BitmapState,
    dx: i32,
    dy: i32,
    src: &BitmapState,
    src_rect: RectI,
    clip: RectI,
    opa: u8,
    treat_as_opaque: bool,
    hold_alpha: bool,
) {
    let sw = src_rect.2 - src_rect.0;
    let sh = src_rect.3 - src_rect.1;
    if sw <= 0 || sh <= 0 || opa == 0 || src.width == 0 || src.height == 0 {
        return;
    }
    let Some(in_bounds) = intersect(dst, (dx, dy, dx + sw, dy + sh)) else {
        return;
    };
    let Some(dc) = intersect_rect(in_bounds, clip) else {
        return;
    };
    for y in dc.1..dc.3 {
        for x in dc.0..dc.2 {
            let sx = src_rect.0 + (x - dx);
            let sy = src_rect.1 + (y - dy);
            if sx < 0 || sy < 0 || sx as u32 >= src.width || sy as u32 >= src.height {
                continue;
            }
            let mut color = read_pixel(src, sx, sy);
            if treat_as_opaque {
                // `bmCopyOnAlpha`/`bmAlpha` treat the source as a fully
                // opaque image; the constant `opa` supplies the coverage.
                color[3] = 255;
            }
            // The reference `Blt(..., holdAlpha)` suppresses the write to the
            // destination alpha channel entirely; capture it first so the
            // composite math still sees the real destination alpha.
            let old_alpha = read_pixel(dst, x, y)[3];
            blend_pixel(dst, x, y, color, 255, opa);
            if hold_alpha {
                let mut d = read_pixel(dst, x, y);
                d[3] = old_alpha;
                write_pixel(dst, x, y, d);
            }
        }
    }
    dst.mark_dirty();
}

/// `iTVPBaseBitmap::CopyRect` with an explicit `plane` selection
/// (`TVP_BB_COPY_MAIN` / `TVP_BB_COPY_MASK`), the primitive behind
/// `tTJSNI_BaseLayer::CopyRect` (`LayerIntf.cpp:4574`).
///
/// * `COPY_MAIN` — `CopyColor`: destination RGB = source RGB, destination
///   alpha held.
/// * `COPY_MASK` — `CopyMask`: destination alpha = source alpha, destination
///   RGB held.
/// * `COPY_MAIN | COPY_MASK` — `Copy`: source-over alpha blend.
pub(crate) fn blit_plane(
    dst: &mut BitmapState,
    dx: i32,
    dy: i32,
    src: &BitmapState,
    src_rect: RectI,
    clip: RectI,
    plane: u8,
) {
    if plane & (COPY_MAIN | COPY_MASK) == (COPY_MAIN | COPY_MASK) {
        // Reference `Copy` is `tTVPRenderMethod_DirectCopy`
        // (`RenderManager.cpp:1359`): a straight copy of the source rect over
        // the dest (RGB *and* alpha), transparent source pixels included —
        // NOT a source-over blend. Games rely on this to make old pixels
        // vanish when they redraw over existing content (e.g.
        // `system/album.tjs::drawCompleteNumber` redraws the percentage
        // digits over the previous frame; a blend leaves the old strokes).
        let sw = src_rect.2 - src_rect.0;
        let sh = src_rect.3 - src_rect.1;
        if sw <= 0 || sh <= 0 || src.width == 0 || src.height == 0 {
            return;
        }
        let Some(in_bounds) = intersect(dst, (dx, dy, dx + sw, dy + sh)) else {
            return;
        };
        let Some(dc) = intersect_rect(in_bounds, clip) else {
            return;
        };
        for y in dc.1..dc.3 {
            for x in dc.0..dc.2 {
                let sx = src_rect.0 + (x - dx);
                let sy = src_rect.1 + (y - dy);
                if sx < 0 || sy < 0 || sx as u32 >= src.width || sy as u32 >= src.height {
                    continue;
                }
                let s = read_pixel(src, sx, sy);
                write_pixel(dst, x, y, s);
            }
        }
        dst.mark_dirty();
        return;
    }
    let sw = src_rect.2 - src_rect.0;
    let sh = src_rect.3 - src_rect.1;
    if sw <= 0 || sh <= 0 || src.width == 0 || src.height == 0 {
        return;
    }
    let Some(in_bounds) = intersect(dst, (dx, dy, dx + sw, dy + sh)) else {
        return;
    };
    let Some(dc) = intersect_rect(in_bounds, clip) else {
        return;
    };
    for y in dc.1..dc.3 {
        for x in dc.0..dc.2 {
            let sx = src_rect.0 + (x - dx);
            let sy = src_rect.1 + (y - dy);
            if sx < 0 || sy < 0 || sx as u32 >= src.width || sy as u32 >= src.height {
                continue;
            }
            let s = read_pixel(src, sx, sy);
            let mut d = read_pixel(dst, x, y);
            if plane & COPY_MAIN != 0 {
                d[0..3].copy_from_slice(&s[0..3]);
            }
            if plane & COPY_MASK != 0 {
                d[3] = s[3];
            }
            write_pixel(dst, x, y, d);
        }
    }
    dst.mark_dirty();
}

/// `layerExImage`/`AddAutoPath`-style tile: repeated `copyRect` of the
/// `tile` source over `[left, top, left+width, top+height)`, starting at the
/// phase `(x, y)` (both `<= 0` after the SDK's modulo normalization).
///
/// Returns the phase-normalized `(x, y)` so a caller that wants the
/// `fillOperateRect` composition can reuse it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn tile_rect(
    dst: &mut BitmapState,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    tile: &BitmapState,
    tile_rect: RectI,
    x: i32,
    y: i32,
) {
    let tw = (tile_rect.2 - tile_rect.0).max(1);
    let th = (tile_rect.3 - tile_rect.1).max(1);
    let mut x = x % tw;
    if x > 0 {
        x -= tw;
    }
    let mut y = y % th;
    if y > 0 {
        y -= th;
    }
    let mut ty = y;
    while ty < height as i32 {
        let mut tx = x;
        while tx < width as i32 {
            let (mut sx, mut sy) = (tile_rect.0, tile_rect.1);
            let (mut sw, mut sh) = (tw, th);
            let mut dx = tx;
            let mut dy = ty;
            if dx < 0 {
                sx -= dx;
                sw += dx;
                dx = 0;
            }
            if dy < 0 {
                sy -= dy;
                sh += dy;
                dy = 0;
            }
            if dx + sw > width as i32 {
                sw = width as i32 - dx;
            }
            if dy + sh > height as i32 {
                sh = height as i32 - dy;
            }
            if sw > 0 && sh > 0 {
                // Clip the tile against the destination bitmap too.
                let dest_x = left + dx;
                let dest_y = top + dy;
                let src_rect = (sx, sy, sx + sw, sy + sh);
                let Some(dc) = intersect(dst, (dest_x, dest_y, dest_x + sw, dest_y + sh)) else {
                    tx += tw;
                    continue;
                };
                let adjusted = (
                    src_rect.0 + (dc.0 - dest_x),
                    src_rect.1 + (dc.1 - dest_y),
                    src_rect.0 + (dc.2 - dest_x),
                    src_rect.1 + (dc.3 - dest_y),
                );
                blit_copy(dst, dc.0, dc.1, tile, adjusted);
            }
            tx += tw;
        }
        ty += th;
    }
}

/// Composite `src` onto `dst` at `(dx, dy)` with source-over using the
/// established [`blend_pixel`] model. `opa` scales the source alpha.
fn composite_normal(dst: &mut BitmapState, dx: i32, dy: i32, src: &BitmapState, opa: u8) {
    if src.width == 0 || src.height == 0 {
        return;
    }
    let Some(dc) = intersect(dst, (dx, dy, dx + src.width as i32, dy + src.height as i32)) else {
        return;
    };
    for y in dc.1..dc.3 {
        for x in dc.0..dc.2 {
            let color = read_pixel(src, x - dx, y - dy);
            blend_pixel(dst, x, y, color, 255, opa);
        }
    }
}

/// Composite `src` onto `dst` with a Photoshop hard-light blend, using
/// [`blend_pixel`] for the final alpha composite. This is the default
/// `ltPsHardLight` light type of the SDK's `doBlurLight`.
fn composite_hard_light(dst: &mut BitmapState, dx: i32, dy: i32, src: &BitmapState, opa: u8) {
    if src.width == 0 || src.height == 0 {
        return;
    }
    let Some(dc) = intersect(dst, (dx, dy, dx + src.width as i32, dy + src.height as i32)) else {
        return;
    };
    for y in dc.1..dc.3 {
        for x in dc.0..dc.2 {
            let s = read_pixel(src, x - dx, y - dy);
            let d = read_pixel(dst, x, y);
            let mut blended = [0u8; 4];
            for c in 0..3 {
                let sv = i32::from(s[c]);
                let dv = i32::from(d[c]);
                blended[c] = if sv < 128 {
                    (2 * sv * dv / 255).clamp(0, 255) as u8
                } else {
                    (255 - 2 * (255 - sv) * (255 - dv) / 255).clamp(0, 255) as u8
                };
            }
            blended[3] = s[3];
            blend_pixel(dst, x, y, blended, 255, opa);
        }
    }
}

/// SDK `Layer.doDropShadow`: fill the source alpha with `shadow_color`,
/// blur it by `blur`, draw it at `(dx, dy)` with `shadow_opacity`, then draw
/// the original image back on top (both source-over). Operates on the whole
/// bitmap (the SDK version works on the image rect; callers assume
/// `imageLeft`/`imageTop` offsets of 0, which is the engine's normal case).
#[allow(clippy::too_many_arguments)]
pub(crate) fn do_drop_shadow(
    bitmap: &mut BitmapState,
    dx: i32,
    dy: i32,
    blur: u32,
    shadow_color: [u8; 4],
    shadow_opacity: u8,
) {
    if bitmap.width == 0 || bitmap.height == 0 {
        return;
    }
    let original = bitmap.clone();
    // Shadow = original alpha, RGB replaced by the shadow color, then blurred.
    let mut shadow = bitmap.clone();
    for chunk in shadow.rgba.chunks_exact_mut(4) {
        chunk[0] = shadow_color[0];
        chunk[1] = shadow_color[1];
        chunk[2] = shadow_color[2];
    }
    blur_bitmap_in_place(&mut shadow, blur, blur);
    // work = original with the offset shadow, then the original on top.
    let mut work = original.clone();
    composite_normal(&mut work, dx, dy, &shadow, shadow_opacity);
    composite_normal(&mut work, 0, 0, &original, 255);
    bitmap.rgba = work.rgba;
    bitmap.mark_dirty();
}

/// SDK `Layer.doBlurLight`: blur a copy, composite it source-over at
/// `blur_opacity`, then composite it again with `light_type` at
/// `light_opacity`. `ltPsHardLight` uses a hard-light blend (the SDK
/// default); every other `light_type` falls back to source-over.
#[allow(clippy::too_many_arguments)]
pub(crate) fn do_blur_light(
    bitmap: &mut BitmapState,
    blur: u32,
    blur_opacity: u8,
    light_opacity: u8,
    light_type: i64,
) {
    if bitmap.width == 0 || bitmap.height == 0 {
        return;
    }
    let mut light = bitmap.clone();
    blur_bitmap_in_place(&mut light, blur, blur);
    composite_normal(bitmap, 0, 0, &light, blur_opacity);
    // `ltPsHardLight = 19` (see `drawable.h` / tvp-natives constants).
    if light_type == 19 {
        composite_hard_light(bitmap, 0, 0, &light, light_opacity);
    } else {
        composite_normal(bitmap, 0, 0, &light, light_opacity);
    }
    bitmap.mark_dirty();
}

/// Fill `[left, top, left+width, top+height)` with `color` using the
/// blend/operation `mode` (a `tTVPBlendOperationMode` value). This is the
/// SDK's `Layer.fillOperateRect`, which tiles a solid-color layer through
/// `operateRect`. The common modes are real; the Photoshop-specific modes
/// (`ltPs*`) reduce to source-over until the PS blend pipeline lands.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fill_operate_rect(
    bitmap: &mut BitmapState,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    color: [u8; 4],
    mode: i64,
    hold_alpha: bool,
) {
    let rect = (left, top, left + width as i32, top + height as i32);
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    // `ltOpaque`/`omOpaque` copy the source (including alpha); `ltAdditive`
    // and `ltSubtractive` do not consider the destination alpha; the rest are
    // source-over in this engine.
    match mode {
        1 => {
            // ltOpaque / omOpaque
            for y in y0..y1 {
                for x in x0..x1 {
                    let mut c = color;
                    if hold_alpha {
                        c[3] = read_pixel(bitmap, x, y)[3];
                    }
                    write_pixel(bitmap, x, y, c);
                }
            }
        }
        3 => {
            // ltAdditive
            for y in y0..y1 {
                for x in x0..x1 {
                    let mut d = read_pixel(bitmap, x, y);
                    for c in 0..3 {
                        d[c] = d[c].saturating_add(color[c]);
                    }
                    write_pixel(bitmap, x, y, d);
                }
            }
        }
        4 => {
            // ltSubtractive
            for y in y0..y1 {
                for x in x0..x1 {
                    let mut d = read_pixel(bitmap, x, y);
                    for c in 0..3 {
                        d[c] = d[c].saturating_sub(color[c]);
                    }
                    write_pixel(bitmap, x, y, d);
                }
            }
        }
        5 => {
            // ltMultiplicative
            for y in y0..y1 {
                for x in x0..x1 {
                    let mut d = read_pixel(bitmap, x, y);
                    for c in 0..3 {
                        d[c] = ((u32::from(d[c]) * u32::from(color[c])) / 255) as u8;
                    }
                    write_pixel(bitmap, x, y, d);
                }
            }
        }
        11 => {
            // ltScreen
            for y in y0..y1 {
                for x in x0..x1 {
                    let mut d = read_pixel(bitmap, x, y);
                    for c in 0..3 {
                        let a = u32::from(d[c]);
                        let b = u32::from(color[c]);
                        d[c] = (255 - (255 - a) * (255 - b) / 255) as u8;
                    }
                    write_pixel(bitmap, x, y, d);
                }
            }
        }
        9 | 10 => {
            // ltDarken / ltLighten
            for y in y0..y1 {
                for x in x0..x1 {
                    let mut d = read_pixel(bitmap, x, y);
                    for c in 0..3 {
                        d[c] = if mode == 9 {
                            d[c].min(color[c])
                        } else {
                            d[c].max(color[c])
                        };
                    }
                    write_pixel(bitmap, x, y, d);
                }
            }
        }
        _ => {
            // ltAlpha / ltPsNormal / ltAddAlpha / any other: source-over.
            for y in y0..y1 {
                for x in x0..x1 {
                    let old_alpha = read_pixel(bitmap, x, y)[3];
                    blend_pixel(bitmap, x, y, color, 255, 255);
                    if hold_alpha {
                        let mut d = read_pixel(bitmap, x, y);
                        d[3] = old_alpha;
                        write_pixel(bitmap, x, y, d);
                    }
                }
            }
        }
    }
    bitmap.mark_dirty();
}

/// A local box blur (the same operation as [`super::layer::blur_bitmap`] but
/// callable from this module without a `Scene`). Keeping the reference
/// blur identical avoids a second, subtly different kernel.
pub(crate) fn blur_bitmap_in_place(bitmap: &mut BitmapState, xradius: u32, yradius: u32) {
    if bitmap.width == 0 || bitmap.height == 0 || (xradius == 0 && yradius == 0) {
        return;
    }
    let source = bitmap.rgba.clone();
    for y in 0..bitmap.height {
        for x in 0..bitmap.width {
            let x0 = x.saturating_sub(xradius);
            let x1 = x.saturating_add(xradius).min(bitmap.width - 1);
            let y0 = y.saturating_sub(yradius);
            let y1 = y.saturating_add(yradius).min(bitmap.height - 1);
            let mut sums = [0u32; 4];
            let mut count = 0u32;
            for sy in y0..=y1 {
                for sx in x0..=x1 {
                    let i = ((sy * bitmap.width + sx) * 4) as usize;
                    for (channel, sum) in sums.iter_mut().enumerate() {
                        *sum += u32::from(source[i + channel]);
                    }
                    count += 1;
                }
            }
            let i = ((y * bitmap.width + x) * 4) as usize;
            for (channel, &sum) in sums.iter().enumerate() {
                bitmap.rgba[i + channel] = (sum / count) as u8;
            }
        }
    }
    bitmap.mark_dirty();
}

// ---------------------------------------------------------------------------
// Stretch / tone operations (the thumbnail pipeline)
// ---------------------------------------------------------------------------

/// Clamp a sample coordinate to `rect` (a half-open source region) or, when
/// `rect` is empty, to the whole bitmap. The `stRefNoClip` stretch flag makes
/// the reference sample outside the given rectangle, so callers pass the
/// whole image as `rect` in that case.
fn read_pixel_rect(bitmap: &BitmapState, rect: RectI, x: i32, y: i32) -> [u8; 4] {
    if bitmap.width == 0 || bitmap.height == 0 {
        return [0, 0, 0, 0];
    }
    let (x0, y0, x1, y1) = if rect.2 > rect.0 && rect.3 > rect.1 {
        rect
    } else {
        (0, 0, bitmap.width as i32, bitmap.height as i32)
    };
    read_pixel(bitmap, x.clamp(x0, x1 - 1), y.clamp(y0, y1 - 1))
}

/// The normalized sinc-based Lanczos window of order `a`.
fn lanczos(t: f32, a: f32) -> f32 {
    if t.abs() < 1e-6 {
        1.0
    } else if t.abs() >= a {
        0.0
    } else {
        let pt = std::f32::consts::PI * t;
        a * pt.sin() * (pt / a).sin() / (pt * pt)
    }
}

/// Catmull-Rom cubic kernel (support radius 2).
fn cubic_kernel(t: f32) -> f32 {
    let x = t.abs();
    if x < 1.0 {
        1.5 * x * x * x - 2.5 * x * x + 1.0
    } else if x < 2.0 {
        -0.5 * x * x * x + 2.5 * x * x - 4.0 * x + 2.0
    } else {
        0.0
    }
}

/// Gather samples around `(fx, fy)` with a separable kernel `k` of the given
/// `radius` (samples to each side). Out-of-range samples clamp to `rect`
/// (the reference edge policy).
fn sample_kernel(
    bitmap: &BitmapState,
    rect: RectI,
    fx: f32,
    fy: f32,
    radius: i32,
    k: impl Fn(f32) -> f32,
) -> [u8; 4] {
    let cx = fx.floor() as i32;
    let cy = fy.floor() as i32;
    let mut acc = [0.0f32; 4];
    let mut wsum = 0.0f32;
    for j in -radius..=radius {
        let wy = k(fy - (cy + j) as f32);
        if wy == 0.0 {
            continue;
        }
        for i in -radius..=radius {
            let w = wy * k(fx - (cx + i) as f32);
            if w == 0.0 {
                continue;
            }
            let p = read_pixel_rect(bitmap, rect, cx + i, cy + j);
            for (c, a) in acc.iter_mut().enumerate() {
                *a += f32::from(p[c]) * w;
            }
            wsum += w;
        }
    }
    if wsum.abs() < 1e-6 {
        return read_pixel_rect(bitmap, rect, fx.round() as i32, fy.round() as i32);
    }
    let mut out = [0u8; 4];
    for (c, &a) in acc.iter().enumerate() {
        out[c] = (a / wsum).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Area-average sample (`stAreaAvg`): the mean of every source pixel whose
/// center lies inside the destination pixel's footprint. `src` maps the
/// destination onto the source; `rect` clamps the samples.
fn sample_area(
    bitmap: &BitmapState,
    rect: RectI,
    src: RectI,
    dest: RectI,
    x: i32,
    y: i32,
) -> [u8; 4] {
    let dw = (dest.2 - dest.0) as f32;
    let dh = (dest.3 - dest.1) as f32;
    let sw = (src.2 - src.0) as f32;
    let sh = (src.3 - src.1) as f32;
    let sx0 = src.0 as f32 + (x as f32 - dest.0 as f32) / dw * sw;
    let sx1 = src.0 as f32 + (x as f32 + 1.0 - dest.0 as f32) / dw * sw;
    let sy0 = src.1 as f32 + (y as f32 - dest.1 as f32) / dh * sh;
    let sy1 = src.1 as f32 + (y as f32 + 1.0 - dest.1 as f32) / dh * sh;
    let ix0 = sx0.floor() as i32;
    let ix1 = (sx1.ceil() as i32).max(ix0 + 1);
    let iy0 = sy0.floor() as i32;
    let iy1 = (sy1.ceil() as i32).max(iy0 + 1);
    let mut acc = [0u32; 4];
    let mut count = 0u32;
    for yy in iy0..iy1 {
        for xx in ix0..ix1 {
            let p = read_pixel_rect(bitmap, rect, xx, yy);
            for (c, a) in acc.iter_mut().enumerate() {
                *a += u32::from(p[c]);
            }
            count += 1;
        }
    }
    let mut out = [0u8; 4];
    let n = count.max(1);
    for (c, &a) in acc.iter().enumerate() {
        out[c] = (a / n) as u8;
    }
    out
}

/// Sample the source at dest-pixel center `(x, y)` using the mapping from the
/// destination rect onto the source rect. `stretch_type` is a
/// `tTVPBBStretchType`: the low 16 bits select the resampler, the
/// `stRefNoClip` (0x10000) flag lets the kernel read outside `src`. Unknown
/// resamplers fall back to bilinear rather than throwing.
fn stretch_sample(
    bitmap: &BitmapState,
    src: RectI,
    dest: RectI,
    x: i32,
    y: i32,
    stretch_type: i64,
) -> [u8; 4] {
    let kind = stretch_type & 0xffff;
    let ref_no_clip = stretch_type & 0x1_0000 != 0;
    let rect = if ref_no_clip {
        (0, 0, bitmap.width as i32, bitmap.height as i32)
    } else {
        src
    };
    let dw = (dest.2 - dest.0) as f32;
    let dh = (dest.3 - dest.1) as f32;
    let sw = (src.2 - src.0) as f32;
    let sh = (src.3 - src.1) as f32;
    let u = (x as f32 - dest.0 as f32 + 0.5) / dw;
    let v = (y as f32 - dest.1 as f32 + 0.5) / dh;
    let fx = src.0 as f32 + u * sw - 0.5;
    let fy = src.1 as f32 + v * sh - 0.5;
    // Area average needs the destination footprint; every other resampler
    // is a pure kernel evaluation.
    if kind == 14 || kind == 15 {
        return sample_area(bitmap, rect, src, dest, x, y);
    }
    sample_kind(bitmap, rect, fx, fy, kind)
}

/// Apply a `tTVPBBStretchType` (low 16 bits) kernel at source position
/// `(fx, fy)`, clamping samples to `rect`. `stAreaAvg` is handled by the
/// caller ([`sample_area`]) because it needs the destination footprint.
fn sample_kind(bitmap: &BitmapState, rect: RectI, fx: f32, fy: f32, kind: i64) -> [u8; 4] {
    match kind {
        // stNearest
        0 => read_pixel_rect(
            bitmap,
            rect,
            (fx + 0.5).floor() as i32,
            (fy + 0.5).floor() as i32,
        ),
        // stFastLinear(1)/stLinear(2)/stSemiFastLinear(4).
        1 | 2 | 4 => sample_kernel(bitmap, rect, fx, fy, 1, |t| (1.0 - t.abs()).max(0.0)),
        // stCubic(3)/stFastCubic(5) and the spline kernels.
        3 | 5 | 10..=13 => sample_kernel(bitmap, rect, fx, fy, 2, cubic_kernel),
        // stLanczos2(6)/stFastLanczos2(7).
        6 | 7 => sample_kernel(bitmap, rect, fx, fy, 2, |t| lanczos(t, 2.0)),
        // stLanczos3(8)/stFastLanczos3(9).
        8 | 9 => sample_kernel(bitmap, rect, fx, fy, 3, |t| lanczos(t, 3.0)),
        // stAreaAvg(14)/stFastAreaAvg(15) fall back to bilinear when sampled
        // directly (affine has no destination footprint); `stretch_sample`
        // routes these to `sample_area`.
        14 | 15 => sample_kernel(bitmap, rect, fx, fy, 1, |t| (1.0 - t.abs()).max(0.0)),
        // stGaussian(16)/stFastGaussian(17).
        16 | 17 => sample_kernel(bitmap, rect, fx, fy, 3, |t| (-t * t / 2.0).exp()),
        // stBlackmanSinc(18)/stFastBlackmanSinc(19).
        18 | 19 => sample_kernel(bitmap, rect, fx, fy, 3, |t| {
            if t.abs() >= 3.0 {
                0.0
            } else {
                let w = 0.42
                    + 0.5 * (std::f32::consts::PI * t / 3.0).cos()
                    + 0.08 * (2.0 * std::f32::consts::PI * t / 3.0).cos();
                w * lanczos(t, 3.0)
            }
        }),
        // Unknown: bilinear.
        _ => sample_kernel(bitmap, rect, fx, fy, 1, |t| (1.0 - t.abs()).max(0.0)),
    }
}

/// `StretchBlt` copy: resample `src[src_rect]` into `dst[dest]`, replacing
/// pixels. `dest`/`src_rect` are half-open; `dest` is clipped to the bitmap.
/// This is the `bmCopy` path of `tTJSNI_BaseLayer::StretchCopy`
/// (`LayerIntf.cpp:4672`) — the operation behind `saveLayerImage` thumbnails.
pub(crate) fn stretch_blit(
    dst: &mut BitmapState,
    dest: RectI,
    src: &BitmapState,
    src_rect: RectI,
    stretch_type: i64,
) {
    stretch_blit_plane(
        dst,
        dest,
        src,
        src_rect,
        stretch_type,
        COPY_MAIN | COPY_MASK,
    );
}

/// [`stretch_blit`] with an explicit `plane` selection, mirroring
/// `tTVPBaseBitmap::StretchBlt`'s `holdAlpha` argument (which the reference
/// `StretchCopy` uses to pick `bmCopy` with or without the alpha plane).
pub(crate) fn stretch_blit_plane(
    dst: &mut BitmapState,
    dest: RectI,
    src: &BitmapState,
    src_rect: RectI,
    stretch_type: i64,
    plane: u8,
) {
    let dw = dest.2 - dest.0;
    let dh = dest.3 - dest.1;
    let sw = src_rect.2 - src_rect.0;
    let sh = src_rect.3 - src_rect.1;
    if dw <= 0 || dh <= 0 || sw <= 0 || sh <= 0 {
        return;
    }
    let Some(dc) = intersect(dst, dest) else {
        return;
    };
    for y in dc.1..dc.3 {
        for x in dc.0..dc.2 {
            let color = stretch_sample(src, src_rect, dest, x, y, stretch_type);
            if plane == (COPY_MAIN | COPY_MASK) {
                write_pixel(dst, x, y, color);
                continue;
            }
            let mut d = read_pixel(dst, x, y);
            if plane & COPY_MAIN != 0 {
                d[0..3].copy_from_slice(&color[0..3]);
            }
            if plane & COPY_MASK != 0 {
                d[3] = color[3];
            }
            write_pixel(dst, x, y, d);
        }
    }
    dst.mark_dirty();
}

/// `TVPDoGrayScale`: luma `(19*R + 183*G + 54*B) >> 8` in all three channels,
/// alpha held (`tvpgl.cpp:10350`).
pub(crate) fn do_gray_scale(bitmap: &mut BitmapState, rect: RectI) {
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    for y in y0..y1 {
        for x in x0..x1 {
            let mut d = read_pixel(bitmap, x, y);
            let gray =
                ((19 * u32::from(d[0]) + 183 * u32::from(d[1]) + 54 * u32::from(d[2])) >> 8) as u8;
            d[0] = gray;
            d[1] = gray;
            d[2] = gray;
            write_pixel(bitmap, x, y, d);
        }
    }
    bitmap.mark_dirty();
}

/// The reference `tTVPGLGammaAdjustData` (R/G/B gamma, floor, ceil).
#[derive(Clone, Copy, Debug)]
pub(crate) struct GammaAdjust {
    pub r_gamma: f64,
    pub r_floor: i32,
    pub r_ceil: i32,
    pub g_gamma: f64,
    pub g_floor: i32,
    pub g_ceil: i32,
    pub b_gamma: f64,
    pub b_floor: i32,
    pub b_ceil: i32,
}

impl GammaAdjust {
    /// `TVPIntactGammaAdjustData` (identity).
    pub(crate) fn intact() -> Self {
        Self {
            r_gamma: 1.0,
            r_floor: 0,
            r_ceil: 255,
            g_gamma: 1.0,
            g_floor: 0,
            g_ceil: 255,
            b_gamma: 1.0,
            b_floor: 0,
            b_ceil: 255,
        }
    }
}

fn gamma_table(gamma: f64, floor: i32, ceil: i32) -> [u8; 256] {
    // `TVPInitGammaAdjustTempData_c` (`tvpgl.cpp:10405`): the reference uses
    // `exp(log(i/255) * (1/gamma))`, i.e. `pow(i/255, 1/gamma)`.
    let ramp = (ceil - floor) as f64;
    let g = if gamma == 0.0 { f64::MAX } else { 1.0 / gamma };
    let mut table = [0u8; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let rate = (i as f64 / 255.0).ln();
        let n = (rate * g).exp() * ramp + 0.5 + floor as f64;
        *slot = n.clamp(0.0, 255.0) as u8;
    }
    table
}

/// `TVPAdjustGamma`: per-channel gamma/level remap, only on non-fully-
/// transparent pixels (`tvpgl.cpp:10462`).
pub(crate) fn adjust_gamma(bitmap: &mut BitmapState, rect: RectI, data: GammaAdjust) {
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    let r = gamma_table(data.r_gamma, data.r_floor, data.r_ceil);
    let g = gamma_table(data.g_gamma, data.g_floor, data.g_ceil);
    let b = gamma_table(data.b_gamma, data.b_floor, data.b_ceil);
    for y in y0..y1 {
        for x in x0..x1 {
            let mut d = read_pixel(bitmap, x, y);
            if d[3] == 0 {
                continue;
            }
            d[0] = r[d[0] as usize];
            d[1] = g[d[1] as usize];
            d[2] = b[d[2] as usize];
            write_pixel(bitmap, x, y, d);
        }
    }
    bitmap.mark_dirty();
}

// ---------------------------------------------------------------------------
// Tone / geometry / composite operations (the rest of the LayerExDraw surface)
// ---------------------------------------------------------------------------

/// `tTJSNI_BaseLayer::ApplyLightContrast` (`LayerIntf.cpp:1006`): brightness
/// offset followed by a `(259*(C+255))/(255*(259-C))` contrast factor, both
/// clamped per channel; alpha held.
pub(crate) fn light_contrast(
    bitmap: &mut BitmapState,
    rect: RectI,
    brightness: i32,
    contrast: i32,
) {
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    let c = contrast.clamp(-255, 255);
    let factor = if c == 0 {
        1.0
    } else if c == 255 {
        259.0 * (255.0 + 255.0) / (255.0 * (259.0 - 254.9))
    } else if c == -255 {
        0.0
    } else {
        (259.0 * (c as f64 + 255.0)) / (255.0 * (259.0 - c as f64))
    };
    for y in y0..y1 {
        for x in x0..x1 {
            let mut d = read_pixel(bitmap, x, y);
            for channel in d.iter_mut().take(3) {
                let bright = (i32::from(*channel) + brightness).clamp(0, 255);
                let out = factor * (bright as f64 - 128.0) + 128.0;
                *channel = out.clamp(0.0, 255.0) as u8;
            }
            write_pixel(bitmap, x, y, d);
        }
    }
    bitmap.mark_dirty();
}

/// `TVPConvertAlphaToAdditiveAlpha` (`tvpgl.cpp:1190`, via `TVPMulColor`):
/// premultiply RGB by alpha (`(c * a) >> 8`), holding alpha. Reference
/// `tTVPBaseBitmap::ConvertAlphaToAddAlpha` (`LayerBitmapIntf.cpp:4748`).
pub(crate) fn convert_alpha_to_add_alpha(bitmap: &mut BitmapState, rect: RectI) {
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    for y in y0..y1 {
        for x in x0..x1 {
            let mut d = read_pixel(bitmap, x, y);
            let a = u32::from(d[3]);
            for channel in d.iter_mut().take(3) {
                *channel = ((u32::from(*channel) * a) >> 8) as u8;
            }
            write_pixel(bitmap, x, y, d);
        }
    }
    bitmap.mark_dirty();
}

/// `TVPConvertAdditiveAlphaToAlpha` (`tvpgl.cpp:1143`, `TVPDivTable`):
/// unpremultiply RGB (`min(c * 255 / a, 255)`, alpha 0 → 0), holding alpha.
/// Reference `tTVPBaseBitmap::ConvertAddAlphaToAlpha`
/// (`LayerBitmapIntf.cpp:4721`).
pub(crate) fn convert_add_alpha_to_alpha(bitmap: &mut BitmapState, rect: RectI) {
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    for y in y0..y1 {
        for x in x0..x1 {
            let mut d = read_pixel(bitmap, x, y);
            let a = u32::from(d[3]);
            if a == 0 {
                d[0] = 0;
                d[1] = 0;
                d[2] = 0;
            } else {
                for channel in d.iter_mut().take(3) {
                    *channel = (u32::from(*channel) * 255)
                        .checked_div(a)
                        .unwrap_or(0)
                        .min(255) as u8;
                }
            }
            write_pixel(bitmap, x, y, d);
        }
    }
    bitmap.mark_dirty();
}

/// `Copy9Patch` (`LayerBitmapIntf.cpp:1150`): scale a source bitmap into the
/// destination using the 9-slice `margin` (left, top, right, bottom). The
/// corners are copied 1:1, the edges are stretched along one axis and the
/// center on both. The reference reads the margins from a `tTVPRect`, where
/// `right`/`bottom` are *offsets from the far edge*.
pub(crate) fn copy_9patch(dst: &mut BitmapState, src: &BitmapState) -> Option<RectI> {
    if dst.width == 0 || dst.height == 0 || src.width < 11 || src.height < 11 {
        return None;
    }
    if dst.width < src.width - 2 || dst.height < src.height - 2 {
        return None;
    }
    let w = src.width as i32;
    let h = src.height as i32;
    // The reference derives the margins from the alpha runs on the source's
    // bottom row (left/right margins) and right column (top/bottom margins)
    // (`LayerBitmapIntf.cpp:1150`).
    let mut ml = -1;
    let mut mr = -1;
    for x in 1..w - 1 {
        let a = read_pixel(src, x, h - 1)[3];
        if ml == -1 && a == 255 {
            ml = x - 1;
        } else if ml != -1 && mr == -1 && a == 0 {
            mr = w - x - 1;
            break;
        }
    }
    let mut mt = -1;
    let mut mb = -1;
    for y in 1..h - 1 {
        let a = read_pixel(src, w - 1, y)[3];
        if mt == -1 && a == 255 {
            mt = y - 1;
        } else if mt != -1 && mb == -1 && a == 0 {
            mb = h - y - 1;
            break;
        }
    }
    if ml < 0 || mr < 0 || mt < 0 || mb < 0 {
        return None;
    }
    let src_center = (ml, mt, w - mr, h - mb);
    let dest_center = (ml, mt, dst.width as i32 - mr, dst.height as i32 - mb);
    if src_center.2 < ml || src_center.3 < mt || dest_center.2 < ml || dest_center.3 < mt {
        return None;
    }
    // 3-slice axes: (dest_start, dest_end, src_start, src_end) for the
    // start margin, the stretched middle and the end margin.
    let cols = [
        (0, ml, 0, ml),
        (ml, dest_center.2, ml, src_center.2),
        (dest_center.2, dst.width as i32, src_center.2, w),
    ];
    let rows = [
        (0, mt, 0, mt),
        (mt, dest_center.3, mt, src_center.3),
        (dest_center.3, dst.height as i32, src_center.3, h),
    ];
    for &(dy0, dy1, sy0, sy1) in &rows {
        for &(dx0, dx1, sx0, sx1) in &cols {
            let dest = (dx0, dy0, dx1, dy1);
            let sr = (sx0, sy0, sx1, sy1);
            if dest.2 > dest.0 && dest.3 > dest.1 && sr.2 > sr.0 && sr.3 > sr.1 {
                stretch_blit(dst, dest, src, sr, 0);
            }
        }
    }
    dst.mark_dirty();
    Some((ml, mt, mr, mb))
}

/// `tTJSNI_BaseLayer::LRFlip` (`LayerIntf.cpp:5910`): mirror the whole main
/// image horizontally.
pub(crate) fn flip_lr(bitmap: &mut BitmapState) {
    if bitmap.width < 2 || bitmap.height == 0 {
        return;
    }
    let w = bitmap.width as usize;
    for y in 0..bitmap.height as usize {
        for x in 0..w / 2 {
            let a = (y * w + x) * 4;
            let b = (y * w + (w - 1 - x)) * 4;
            for k in 0..4 {
                bitmap.rgba.swap(a + k, b + k);
            }
        }
    }
    bitmap.mark_dirty();
}

/// `tTJSNI_BaseLayer::UDFlip` (`LayerIntf.cpp:5925`): mirror vertically.
pub(crate) fn flip_ud(bitmap: &mut BitmapState) {
    if bitmap.height < 2 || bitmap.width == 0 {
        return;
    }
    let w = bitmap.width as usize;
    let h = bitmap.height as usize;
    for y in 0..h / 2 {
        for x in 0..w {
            let a = (y * w + x) * 4;
            let b = ((h - 1 - y) * w + x) * 4;
            for k in 0..4 {
                bitmap.rgba.swap(a + k, b + k);
            }
        }
    }
    bitmap.mark_dirty();
}

/// The 1-D Gaussian kernel used by the reference `generate_1d_gaussian_kernel`
/// (`LayerIntf.cpp:5729`).
fn gaussian_kernel(radius: i32, sigma: f32) -> Vec<f32> {
    let size = (2 * radius + 1) as usize;
    let mut kernel = vec![0.0f32; size];
    let r2 = 2.0 * sigma * sigma;
    let mut sum = 0.0f32;
    for (i, k) in kernel.iter_mut().enumerate() {
        let x = i as i32 - radius;
        *k = (-(x * x) as f32 / r2).exp();
        sum += *k;
    }
    if sum != 0.0 {
        for k in &mut kernel {
            *k /= sum;
        }
    }
    kernel
}

/// Separable Gaussian blur over the clip region (`Layer.gaussianBlur`;
/// reference `ApplyGaussianBlur` `LayerIntf.cpp:5745`). Pixels outside the
/// region clamp to the edge.
pub(crate) fn gaussian_blur(bitmap: &mut BitmapState, rect: RectI, radius: i32, sigma: f32) {
    if radius <= 0 || sigma <= 0.0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    let kernel = gaussian_kernel(radius, sigma);
    let r = radius;
    let mut tmp = bitmap.rgba.clone();
    let w = bitmap.width as i32;
    let h = bitmap.height as i32;
    let at = |buf: &[u8], x: i32, y: i32| -> [u8; 4] {
        let cx = x.clamp(0, w - 1) as usize;
        let cy = y.clamp(0, h - 1) as usize;
        let i = (cy * w as usize + cx) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    };
    // Horizontal pass: src -> tmp.
    for y in y0..y1 {
        for x in x0..x1 {
            let mut acc = [0.0f32; 4];
            for (ki, &k) in kernel.iter().enumerate() {
                let p = at(&bitmap.rgba, x - r + ki as i32, y);
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += f32::from(p[c]) * k;
                }
            }
            let i = ((y as usize) * bitmap.width as usize + x as usize) * 4;
            for (c, &a) in acc.iter().enumerate() {
                tmp[i + c] = a.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    // Vertical pass: tmp -> bitmap.
    for y in y0..y1 {
        for x in x0..x1 {
            let mut acc = [0.0f32; 4];
            for (ki, &k) in kernel.iter().enumerate() {
                let p = at(&tmp, x, y - r + ki as i32);
                for (c, a) in acc.iter_mut().enumerate() {
                    *a += f32::from(p[c]) * k;
                }
            }
            let i = ((y as usize) * bitmap.width as usize + x as usize) * 4;
            for (c, &a) in acc.iter().enumerate() {
                bitmap.rgba[i + c] = a.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    bitmap.mark_dirty();
}

/// Blend one source pixel onto `dst` using a `tTVPBlendOperationMode`
/// (`LayerIntf.h:76`). The Photoshop-specific modes without a C fallback in
/// the reference reduce to source-over (the assembly pipeline is absent);
/// `omPsHardLight` is implemented because the SDK `doBlurLight` uses it.
pub(crate) fn blend_pixel_mode(
    dst: &mut BitmapState,
    x: i32,
    y: i32,
    src: [u8; 4],
    opa: u8,
    mode: i64,
) {
    match mode {
        // omOpaque / ltOpaque: copy the source (including alpha).
        1 => write_pixel(dst, x, y, src),
        // omAdditive / ltAdditive: saturating add, source scaled by `opa`.
        3 => {
            let mut d = read_pixel(dst, x, y);
            for c in 0..3 {
                d[c] = d[c].saturating_add(((u32::from(src[c]) * u32::from(opa)) / 255) as u8);
            }
            write_pixel(dst, x, y, d);
        }
        // omSubtractive / ltSubtractive.
        4 => {
            let mut d = read_pixel(dst, x, y);
            for c in 0..3 {
                d[c] = d[c].saturating_sub(((u32::from(src[c]) * u32::from(opa)) / 255) as u8);
            }
            write_pixel(dst, x, y, d);
        }
        // omMultiplicative / ltMultiplicative.
        5 => {
            let mut d = read_pixel(dst, x, y);
            let opa = u32::from(opa);
            for c in 0..3 {
                let s = u32::from(src[c]) * opa / 255;
                d[c] = (u32::from(d[c]) * s / 255) as u8;
            }
            write_pixel(dst, x, y, d);
        }
        // omDarken / ltDarken.
        9 => {
            let mut d = read_pixel(dst, x, y);
            for c in 0..3 {
                d[c] = d[c].min(src[c]);
            }
            write_pixel(dst, x, y, d);
        }
        // omLighten / ltLighten.
        10 => {
            let mut d = read_pixel(dst, x, y);
            for c in 0..3 {
                d[c] = d[c].max(src[c]);
            }
            write_pixel(dst, x, y, d);
        }
        // omScreen / ltScreen.
        11 => {
            let mut d = read_pixel(dst, x, y);
            for c in 0..3 {
                let blended = 255 - (255 - u32::from(d[c])) * (255 - u32::from(src[c])) / 255;
                d[c] = ((u32::from(d[c]) * (255 - u32::from(opa)) + blended * 255) / 255) as u8;
            }
            write_pixel(dst, x, y, d);
        }
        // ltPsHardLight (19): standard hard-light, alpha-composited.
        19 => {
            let d = read_pixel(dst, x, y);
            let mut blended = [0u8; 4];
            for c in 0..3 {
                let sv = i32::from(src[c]);
                let dv = i32::from(d[c]);
                blended[c] = if sv < 128 {
                    (2 * sv * dv / 255).clamp(0, 255) as u8
                } else {
                    (255 - 2 * (255 - sv) * (255 - dv) / 255).clamp(0, 255) as u8
                };
            }
            blended[3] = src[3];
            blend_pixel(dst, x, y, blended, 255, opa);
        }
        // Everything else (omAlpha=2, omPsNormal=13, omAddAlpha=12, and the
        // remaining PS modes): source-over.
        _ => blend_pixel(dst, x, y, src, 255, opa),
    }
}

/// `tTJSNI_BaseLayer::OperateRect` (`LayerIntf.cpp:5224`): blend a source
/// region onto `dst` at `(dx, dy)` with `mode` and `opa`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn operate_rect(
    dst: &mut BitmapState,
    dx: i32,
    dy: i32,
    src: &BitmapState,
    src_rect: RectI,
    mode: i64,
    opa: u8,
    hold_alpha: bool,
) {
    let sw = src_rect.2 - src_rect.0;
    let sh = src_rect.3 - src_rect.1;
    if sw <= 0 || sh <= 0 {
        return;
    }
    let Some(dc) = intersect(dst, (dx, dy, dx + sw, dy + sh)) else {
        return;
    };
    for y in dc.1..dc.3 {
        for x in dc.0..dc.2 {
            let color = read_pixel(src, src_rect.0 + (x - dx), src_rect.1 + (y - dy));
            let old_alpha = read_pixel(dst, x, y)[3];
            blend_pixel_mode(dst, x, y, color, opa, mode);
            if hold_alpha {
                let mut d = read_pixel(dst, x, y);
                d[3] = old_alpha;
                write_pixel(dst, x, y, d);
            }
        }
    }
    dst.mark_dirty();
}

/// `StretchPile`/`StretchBlend`/`OperateStretch`: resample `src[src_rect]`
/// into `dst[dest]` and blend with `mode` (or copy when `mode == 1`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn stretch_blit_mode(
    dst: &mut BitmapState,
    dest: RectI,
    src: &BitmapState,
    src_rect: RectI,
    stretch_type: i64,
    mode: i64,
    opa: u8,
    hold_alpha: bool,
) {
    let dw = dest.2 - dest.0;
    let dh = dest.3 - dest.1;
    if dw <= 0 || dh <= 0 || src_rect.2 <= src_rect.0 || src_rect.3 <= src_rect.1 {
        return;
    }
    let Some(dc) = intersect(dst, dest) else {
        return;
    };
    for y in dc.1..dc.3 {
        for x in dc.0..dc.2 {
            let color = stretch_sample(src, src_rect, dest, x, y, stretch_type);
            let old_alpha = read_pixel(dst, x, y)[3];
            blend_pixel_mode(dst, x, y, color, opa, mode);
            if hold_alpha {
                let mut d = read_pixel(dst, x, y);
                d[3] = old_alpha;
                write_pixel(dst, x, y, d);
            }
        }
    }
    dst.mark_dirty();
}

/// The forward affine map of the three destination corners `p0` (src LT),
/// `p1` (src RT), `p2` (src LB) — see `AffineBlt` (`LayerBitmapIntf.cpp:3461`).
/// Returns `(a, b, c, d, e, f)` such that `u,v ∈ [0,1]` maps to
/// `x = a + b*u + c*v`, `y = d + e*u + f*v`.
fn affine_forward(p0: (f64, f64), p1: (f64, f64), p2: (f64, f64)) -> [f64; 6] {
    [
        p0.0,
        p1.0 - p0.0,
        p2.0 - p0.0,
        p0.1,
        p1.1 - p0.1,
        p2.1 - p0.1,
    ]
}

/// `affineCopy`/`affinePile`/`affineBlend`/`operateAffine`: map the source
/// rect's LT/RT/LB corners to `p0/p1/p2` and resample into `dst` with `mode`
/// (`1` = copy) and `opa`. Inverse-maps each destination pixel and bilinearly
/// samples the source.
#[allow(clippy::too_many_arguments)]
pub(crate) fn affine_blit(
    dst: &mut BitmapState,
    p0: (f64, f64),
    p1: (f64, f64),
    p2: (f64, f64),
    src: &BitmapState,
    src_rect: RectI,
    stretch_type: i64,
    mode: i64,
    opa: u8,
    clear: bool,
    clear_color: [u8; 4],
) {
    let m = affine_forward(p0, p1, p2);
    let det = m[1] * m[5] - m[2] * m[4];
    if det.abs() < 1e-12 {
        return;
    }
    // Inverse of the 2x2 [b c; e f].
    let inv = [m[5] / det, -m[2] / det, -m[4] / det, m[1] / det];
    // Bounding box of the mapped unit square.
    let corners = [
        (m[0], m[3]),
        (m[0] + m[1], m[3] + m[4]),
        (m[0] + m[2], m[3] + m[5]),
        (m[0] + m[1] + m[2], m[3] + m[4] + m[5]),
    ];
    let min_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f64::INFINITY, f64::min)
        .floor() as i32;
    let max_x = corners
        .iter()
        .map(|c| c.0)
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil() as i32;
    let min_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f64::INFINITY, f64::min)
        .floor() as i32;
    let max_y = corners
        .iter()
        .map(|c| c.1)
        .fold(f64::NEG_INFINITY, f64::max)
        .ceil() as i32;
    let Some(dc) = intersect(dst, (min_x, min_y, max_x + 1, max_y + 1)) else {
        return;
    };
    if clear {
        let clip = (min_x, min_y, max_x + 1, max_y + 1);
        if let Some(cc) = intersect(dst, clip) {
            for y in cc.1..cc.3 {
                for x in cc.0..cc.2 {
                    write_pixel(dst, x, y, clear_color);
                }
            }
        }
    }
    let sw = (src_rect.2 - src_rect.0) as f64;
    let sh = (src_rect.3 - src_rect.1) as f64;
    for y in dc.1..dc.3 {
        for x in dc.0..dc.2 {
            let px = x as f64 + 0.5 - m[0];
            let py = y as f64 + 0.5 - m[3];
            let u = inv[0] * px + inv[1] * py;
            let v = inv[2] * px + inv[3] * py;
            if !(0.0..=1.0).contains(&u) || !(0.0..=1.0).contains(&v) {
                continue;
            }
            let fx = src_rect.0 as f32 + (u * sw) as f32 - 0.5;
            let fy = src_rect.1 as f32 + (v * sh) as f32 - 0.5;
            // Affine sampling clamps to the whole source image (the reference
            // may interpolate across the source-rect border), and supports the
            // same resampler set as `stretch*`.
            let rect = (0, 0, src.width as i32, src.height as i32);
            let color = sample_kind(src, rect, fx, fy, stretch_type & 0xffff);
            blend_pixel_mode(dst, x, y, color, opa, mode);
        }
    }
    dst.mark_dirty();
}

// ---------------------------------------------------------------------------
// Window primary-layer compositing (screenshots / save thumbnails)
// ---------------------------------------------------------------------------
//
// The reference window's primary layer is its screen buffer: the layer
// manager composites the window's layer tree into the primary's MainImage
// while drawing (`reference/cpp/core/visual/LayerManager.cpp`),
// and games read it back for screenshots (`ADVScreen.tjs`
// `spr.piledCopy(0, 0, window.primaryLayer, ...)`) and save thumbnails.
//
// This engine renders the flat scene in Bevy, so the primary layer normally
// has no MainImage. We allocate one on demand and composite the visible tree
// into it with the same CPU blit/blend primitives `copyRect`/`piledCopy`
// use, so any later read of the primary as an image source returns a real
// screen buffer.

/// One layer's contribution to the primary-layer composite.
enum PrimaryDrawSource {
    /// The layer's own MainImage (cloned so the primary can be borrowed
    /// mutably while blitting).
    Bitmap(BitmapState),
    /// A solid fill for a bitmapless fill layer.
    Solid([u8; 4]),
}

/// A resolved, back-to-front draw command for the primary composite. All
/// coordinates are absolute window coordinates; `src` is in source pixels.
struct PrimaryDraw {
    source: PrimaryDrawSource,
    dx: i32,
    dy: i32,
    src: RectI,
    mode: i64,
    opa: u8,
}

/// The primary MainImage size: the window's declared logical size, falling
/// back to the primary layer's rect (then 1x1) for a degenerate window.
fn primary_target_size(scene: &Scene, primary_id: u32, window: u32) -> Option<(u32, u32)> {
    let (w, h) = scene.window(window)?.inner_size;
    if w > 0 && h > 0 {
        return Some((w, h));
    }
    let layer = scene.layer(primary_id)?;
    Some((layer.rect.w.max(1), layer.rect.h.max(1)))
}

/// Make sure the primary layer owns a MainImage. Allocates a transparent
/// buffer at the window's logical size when absent; an image the script
/// already attached (or a previous composite) is left untouched.
fn ensure_primary_bitmap(scene: &mut Scene, primary_id: u32, w: u32, h: u32) -> Option<u32> {
    if let Some(bitmap_id) = scene.layer(primary_id)?.bitmap {
        return Some(bitmap_id);
    }
    let bitmap_id = scene.add_bitmap(w, h, vec![0; w as usize * h as usize * 4]);
    let layer = scene.layer_mut(primary_id)?;
    layer.bitmap = Some(bitmap_id);
    layer.image_left = 0;
    layer.image_top = 0;
    layer.image_width = w;
    layer.image_height = h;
    layer.clip = None;
    layer.image_modified = true;
    Some(bitmap_id)
}

/// Whether `id` is a (direct or indirect) child of `ancestor` in the layer
/// tree. The window's primary layer is the screen buffer for **its** subtree
/// (the game parents every content layer under `window.primaryLayer`); other
/// window roots are separate trees and must not be composited into it.
fn is_descendant(scene: &Scene, id: u32, ancestor: u32) -> bool {
    let mut current = id;
    for _ in 0..4096 {
        let Some(layer) = scene.layer(current) else {
            return false;
        };
        match layer.parent {
            Some(parent) if parent == ancestor => return true,
            Some(parent) => current = parent,
            None => return false,
        }
    }
    false
}

/// Product of the layer's and its ancestors' opacities (the reference
/// composes a parent's opacity over its whole subtree). Bounded against a
/// malformed parent cycle.
fn node_opacity(scene: &Scene, id: u32) -> f32 {
    let mut opacity = 1.0f32;
    let mut current = Some(id);
    for _ in 0..1024 {
        let Some(layer) = current.and_then(|c| scene.layer(c)) else {
            break;
        };
        opacity *= layer.opacity.clamp(0.0, 1.0);
        current = layer.parent;
    }
    opacity
}

/// Blit one resolved bitmap draw onto the primary buffer using the same
/// primitives `copyRect`/`piledCopy` use: an opaque full-alpha mode copies,
/// source-over uses [`blit_over`], and the remaining modes go through
/// [`operate_rect`].
fn draw_primary_bitmap(dst: &mut BitmapState, draw: &PrimaryDraw, src: &BitmapState) {
    let full = (i32::MIN, i32::MIN, i32::MAX, i32::MAX);
    match draw.mode {
        // ltBinder: a container layer draws nothing itself.
        0 => {}
        // ltOpaque at full coverage replaces pixels (including alpha).
        1 if draw.opa == 255 => blit_copy_clipped(dst, draw.dx, draw.dy, src, draw.src, full),
        // ltOpaque below full opacity, ltAlpha and ltPsNormal blend
        // source-over (the opaque layer must not punch through its alpha).
        1 | 2 | 13 => blit_over(
            dst, draw.dx, draw.dy, src, draw.src, full, draw.opa, false, false,
        ),
        mode => operate_rect(dst, draw.dx, draw.dy, src, draw.src, mode, draw.opa, false),
    }
}

/// Fill a bitmapless solid layer's rect on the primary buffer.
fn draw_primary_solid(dst: &mut BitmapState, draw: &PrimaryDraw, color: [u8; 4]) {
    let (sx, sy, sx1, sy1) = draw.src;
    let dest = (draw.dx, draw.dy, draw.dx + (sx1 - sx), draw.dy + (sy1 - sy));
    let Some((x0, y0, x1, y1)) = intersect(dst, dest) else {
        return;
    };
    for y in y0..y1 {
        for x in x0..x1 {
            if draw.mode != 0 {
                blend_pixel_mode(dst, x, y, color, draw.opa, draw.mode);
            }
        }
    }
    dst.mark_dirty();
}

/// Composite the window's currently-visible layer tree into its primary
/// layer's MainImage, back-to-front. Allocates the primary image on demand
/// at the window's logical size. Returns the primary bitmap id.
pub(crate) fn composite_primary_layer(scene: &mut Scene, primary_id: u32) -> Option<u32> {
    let window = scene.layer(primary_id)?.window;
    if !scene.is_primary_layer(primary_id) {
        return None;
    }
    let (target_w, target_h) = primary_target_size(scene, primary_id, window)?;
    let bitmap_id = ensure_primary_bitmap(scene, primary_id, target_w, target_h)?;

    // Resolve every visible layer to a draw command under immutable borrows,
    // cloning the small source buffers (screenshots are rare; this avoids
    // aliasing the primary while it is mutated).
    let order = scene.window_layer_order(window);
    let mut draws: Vec<PrimaryDraw> = Vec::new();
    for id in order {
        if id == primary_id || !is_descendant(scene, id, primary_id) || !scene.node_visible(id) {
            continue;
        }
        let Some(layer) = scene.layer(id) else {
            continue;
        };
        let opacity = node_opacity(scene, id);
        let opa = (opacity * 255.0).round().clamp(0.0, 255.0) as u8;
        if opa == 0 {
            continue;
        }
        let (ax, ay) = scene.absolute_offset(id).unwrap_or((0, 0));
        if let Some(src_id) = layer.bitmap {
            let Some(src) = scene.bitmap(src_id).cloned() else {
                continue;
            };
            if src.width == 0 || src.height == 0 {
                continue;
            }
            // The image is placed at (rect + imageLeft, rect + imageTop),
            // clipped to the layer rect and its own ClipRect. Saturating
            // arithmetic keeps pathological script values from overflowing.
            let img_x = ax.saturating_add(layer.image_left);
            let img_y = ay.saturating_add(layer.image_top);
            let img_x1 = img_x.saturating_add(src.width as i32);
            let img_y1 = img_y.saturating_add(src.height as i32);
            let rect_x1 = ax.saturating_add(layer.rect.w as i32);
            let rect_y1 = ay.saturating_add(layer.rect.h as i32);
            let (clx0, cly0, clx1, cly1) = match layer.clip {
                Some(c) => (
                    c.x,
                    c.y,
                    c.x.saturating_add(c.w as i32),
                    c.y.saturating_add(c.h as i32),
                ),
                None => (0, 0, src.width as i32, src.height as i32),
            };
            let dx0 = img_x.saturating_add(clx0).max(img_x).max(ax);
            let dy0 = img_y.saturating_add(cly0).max(img_y).max(ay);
            let dx1 = img_x.saturating_add(clx1).min(img_x1).min(rect_x1);
            let dy1 = img_y.saturating_add(cly1).min(img_y1).min(rect_y1);
            if dx1 <= dx0 || dy1 <= dy0 {
                continue;
            }
            let sx = dx0 - img_x;
            let sy = dy0 - img_y;
            draws.push(PrimaryDraw {
                source: PrimaryDrawSource::Bitmap(src),
                dx: dx0,
                dy: dy0,
                src: (sx, sy, sx + (dx1 - dx0), sy + (dy1 - dy0)),
                mode: layer.blend_type,
                opa,
            });
        } else if let Some(fill) = layer.fill_color {
            draws.push(PrimaryDraw {
                source: PrimaryDrawSource::Solid(fill),
                dx: ax,
                dy: ay,
                src: (0, 0, layer.rect.w as i32, layer.rect.h as i32),
                mode: layer.blend_type,
                opa,
            });
        }
    }

    // Nothing below the primary: leave any script-attached image alone.
    if draws.is_empty() {
        return Some(bitmap_id);
    }

    // Clear the screen buffer, then composite back-to-front.
    let primary = scene.bitmap_mut(bitmap_id)?;
    primary.rgba.fill(0);
    for draw in &draws {
        match &draw.source {
            PrimaryDrawSource::Bitmap(src) => draw_primary_bitmap(primary, draw, src),
            PrimaryDrawSource::Solid(color) => draw_primary_solid(primary, draw, *color),
        }
    }
    primary.mark_dirty();
    Some(bitmap_id)
}
