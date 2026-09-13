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

use crate::scene::BitmapState;

use super::raster::blend_pixel;

/// Half-open pixel rectangle `[x0, x1) x [y0, y1)`.
pub(crate) type RectI = (i32, i32, i32, i32);

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
    let Some(dest_clip) = intersect(
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
pub(crate) fn fill_operate_rect(
    bitmap: &mut BitmapState,
    left: i32,
    top: i32,
    width: u32,
    height: u32,
    color: [u8; 4],
    mode: i64,
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
                    write_pixel(bitmap, x, y, color);
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
                    blend_pixel(bitmap, x, y, color, 255, 255);
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

/// Read a pixel clamped to the bitmap edges (out-of-range samples repeat the
/// border, the usual resampler edge policy).
fn read_pixel_clamped(bitmap: &BitmapState, x: i32, y: i32) -> [u8; 4] {
    if bitmap.width == 0 || bitmap.height == 0 {
        return [0, 0, 0, 0];
    }
    let x = x.clamp(0, bitmap.width as i32 - 1) as u32;
    let y = y.clamp(0, bitmap.height as i32 - 1) as u32;
    read_pixel(bitmap, x as i32, y as i32)
}

/// Catmull-Rom cubic interpolation of four samples at `t` in `0..=1`.
fn cubic(v0: f32, v1: f32, v2: f32, v3: f32, t: f32) -> f32 {
    let a = -0.5 * v0 + 1.5 * v1 - 1.5 * v2 + 0.5 * v3;
    let b = v0 - 2.5 * v1 + 2.0 * v2 - 0.5 * v3;
    let c = -0.5 * v0 + 0.5 * v2;
    a * t * t * t + b * t * t + c * t + v1
}

/// Bilinear sample at fractional `(fx, fy)`.
fn sample_bilinear(bitmap: &BitmapState, fx: f32, fy: f32) -> [u8; 4] {
    let x0 = fx.floor();
    let y0 = fy.floor();
    let tx = fx - x0;
    let ty = fy - y0;
    let (x0, y0) = (x0 as i32, y0 as i32);
    let p00 = read_pixel_clamped(bitmap, x0, y0);
    let p10 = read_pixel_clamped(bitmap, x0 + 1, y0);
    let p01 = read_pixel_clamped(bitmap, x0, y0 + 1);
    let p11 = read_pixel_clamped(bitmap, x0 + 1, y0 + 1);
    let mut out = [0u8; 4];
    for (c, o) in out.iter_mut().enumerate() {
        let top = f32::from(p00[c]) + (f32::from(p10[c]) - f32::from(p00[c])) * tx;
        let bot = f32::from(p01[c]) + (f32::from(p11[c]) - f32::from(p01[c])) * tx;
        *o = (top + (bot - top) * ty).round().clamp(0.0, 255.0) as u8;
    }
    out
}

/// Bicubic (Catmull-Rom) sample at fractional `(fx, fy)`.
fn sample_bicubic(bitmap: &BitmapState, fx: f32, fy: f32) -> [u8; 4] {
    let x0 = fx.floor() as i32;
    let y0 = fy.floor() as i32;
    let tx = fx - x0 as f32;
    let ty = fy - y0 as f32;
    let mut out = [0u8; 4];
    for (c, o) in out.iter_mut().enumerate() {
        let mut rows = [0.0f32; 4];
        for (j, row) in rows.iter_mut().enumerate() {
            let yy = y0 - 1 + j as i32;
            let p0 = read_pixel_clamped(bitmap, x0 - 1, yy)[c];
            let p1 = read_pixel_clamped(bitmap, x0, yy)[c];
            let p2 = read_pixel_clamped(bitmap, x0 + 1, yy)[c];
            let p3 = read_pixel_clamped(bitmap, x0 + 2, yy)[c];
            *row = cubic(
                f32::from(p0),
                f32::from(p1),
                f32::from(p2),
                f32::from(p3),
                tx,
            );
        }
        *o = cubic(rows[0], rows[1], rows[2], rows[3], ty)
            .round()
            .clamp(0.0, 255.0) as u8;
    }
    out
}

/// Sample the source at dest-pixel center `(x, y)` using the mapping from the
/// destination rect onto the source rect.
fn stretch_sample(
    bitmap: &BitmapState,
    src: RectI,
    dest: RectI,
    x: i32,
    y: i32,
    kind: i64,
) -> [u8; 4] {
    let dw = (dest.2 - dest.0) as f32;
    let dh = (dest.3 - dest.1) as f32;
    let sw = (src.2 - src.0) as f32;
    let sh = (src.3 - src.1) as f32;
    let u = (x as f32 - dest.0 as f32 + 0.5) / dw;
    let v = (y as f32 - dest.1 as f32 + 0.5) / dh;
    let fx = src.0 as f32 + u * sw - 0.5;
    let fy = src.1 as f32 + v * sh - 0.5;
    match kind {
        0 => {
            // stNearest
            let sx = (fx + 0.5).floor() as i32;
            let sy = (fy + 0.5).floor() as i32;
            read_pixel_clamped(bitmap, sx, sy)
        }
        // stCubic / stFastCubic → Catmull-Rom; the higher-quality spline
        // kernels reduce to bicubic (no assembly pipeline needed).
        3 | 5 => sample_bicubic(bitmap, fx, fy),
        // stLinear / stFastLinear / everything else.
        _ => sample_bilinear(bitmap, fx, fy),
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
            write_pixel(dst, x, y, color);
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
            blend_pixel_mode(dst, x, y, color, opa, mode);
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
            blend_pixel_mode(dst, x, y, color, opa, mode);
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
            let color = if stretch_type == 0 {
                read_pixel_clamped(src, (fx + 0.5).floor() as i32, (fy + 0.5).floor() as i32)
            } else if stretch_type == 3 || stretch_type == 5 {
                sample_bicubic(src, fx, fy)
            } else {
                sample_bilinear(src, fx, fy)
            };
            blend_pixel_mode(dst, x, y, color, opa, mode);
        }
    }
    dst.mark_dirty();
}
