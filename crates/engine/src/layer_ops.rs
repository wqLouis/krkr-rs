//! In-place CPU pixel operations on the layer system's [`BitmapState`]s (the
//! `LayerExDraw` / `layerExImage` surface).
//!
//! The scene stores straight-alpha RGBA8 (`r, g, b, a` per pixel), the same
//! pixel model [`crate::raster::blend_pixel`] uses; these operations run on
//! those straight-alpha channels directly.
//!
//! This is L2 state: the caller owns the [`Scene`]/[`BitmapState`] and every
//! function is a free function with no hidden state.
//!
//! | function | operation |
//! |---|---|
//! | [`fill_color_on_alpha`] | source-over a constant color, considering the destination alpha |
//! | [`fill_color_on_add_alpha`] | additive-alpha (premultiplied) fill |
//! | [`fill_color_hold_alpha`] | set RGB to a constant color, holding the destination alpha |
//! | [`fill_mask`] | replace the alpha channel with a constant |
//! | [`remove_const_opacity`] | scale the alpha by `1 - level / 255` |
//! | [`colorize`] | reblend hue and saturation, preserving lightness |
//! | [`noise`] | add uniform per-channel noise, holding alpha |
//! | [`blit_copy`] | copy a source region over the destination |
//! | [`tile_rect`] | tile a source bitmap over a rectangle with a phase |
//! | [`do_drop_shadow`] / [`do_blur_light`] | blur a copy, then composite it as a shadow / light pass |
//! | [`fill_operate_rect`] | fill a rectangle with a solid color through a blend mode |
//!
//! `colorize` / `noise` operate on the layer's current `ClipRect` (the effect
//! rebases its buffer on the clip), so the callers pass the already-clipped
//! region.

use api::blend::{BlendMode, DstBlend};

use crate::raster::{blend_pixel, blend_pixel_at};
use crate::scene::{BitmapState, Rect, Scene};

/// Half-open pixel rectangle `[x0, x1) x [y0, y1)`.
pub type RectI = (i32, i32, i32, i32);

/// A source bitmap and the sub-rectangle a blit reads from it.
#[derive(Clone, Copy)]
pub struct BlitSource<'a> {
    /// Source bitmap.
    pub bitmap: &'a BitmapState,
    /// Sub-rectangle of the source to read.
    pub rect: RectI,
}

/// The blend state shared by the blit operations.
#[derive(Clone, Copy, Debug, Default)]
pub struct BlitOptions {
    /// Numeric blend-operation mode; ignored by [`blit_over`], which always
    /// composites source-over.
    pub mode: i64,
    /// Source opacity, `0..=255`.
    pub opa: u8,
    /// Treat the source as fully opaque and let `opa` supply the coverage;
    /// [`blit_over`] only.
    pub treat_as_opaque: bool,
    /// Keep the destination alpha channel; unused by [`affine_blit`].
    pub hold_alpha: bool,
}

/// The corner map, resampler and background of [`affine_blit`].
#[derive(Clone, Copy, Debug)]
pub struct AffineBlit {
    /// Destination coordinates the source rect's LT/RT/LB corners map to.
    pub corners: [(f64, f64); 3],
    /// Resampler selector.
    pub stretch_type: i64,
    /// Whether to fill the destination bounding box first.
    pub clear: bool,
    /// Colour written when `clear` is set.
    pub clear_color: [u8; 4],
}

/// `TVP_BB_COPY_MAIN`: copy the RGB plane only, holding the destination alpha
/// (the `CopyColor` operation).
pub const COPY_MAIN: u8 = 1;
/// `TVP_BB_COPY_MASK`: copy the alpha plane only, holding the destination RGB
/// (the `CopyMask` operation).
pub const COPY_MASK: u8 = 2;

/// Intersect `rect` with the bitmap's bounds, returning `None` when empty.
pub fn intersect(bitmap: &BitmapState, rect: RectI) -> Option<RectI> {
    let (x0, y0, x1, y1) = rect;
    let ix0 = x0.max(0);
    let iy0 = y0.max(0);
    let ix1 = x1.min(bitmap.width as i32);
    let iy1 = y1.min(bitmap.height as i32);
    (ix0 < ix1 && iy0 < iy1).then_some((ix0, iy0, ix1, iy1))
}

/// Intersect two half-open rectangles, returning `None` when empty.
pub fn intersect_rect(a: RectI, b: RectI) -> Option<RectI> {
    let r = (a.0.max(b.0), a.1.max(b.1), a.2.min(b.2), a.3.min(b.3));
    (r.0 < r.2 && r.1 < r.3).then_some(r)
}

/// A layer's `ClipRect` ([`Rect`]) as the half-open pixel rectangle the
/// operations here take. `None` means the whole image, expressed as the full
/// `i32` range so the operation still clips to the bitmap's own bounds.
pub fn clip_rect(clip: Option<Rect>) -> RectI {
    match clip {
        Some(c) => (
            c.x,
            c.y,
            c.x.saturating_add(c.w as i32),
            c.y.saturating_add(c.h as i32),
        ),
        None => (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
    }
}

/// The effective color-blend factor for a source-over composite. `dopa` is the
/// destination alpha, `opa` the source opacity. A fully transparent
/// destination collapses to `255` (the source color is written unattenuated).
fn opacity_on_opacity(dopa: u8, opa: u8) -> u8 {
    if dopa == 0 {
        return 255;
    }
    // Computed with `f32` in this operation order so the final truncation is
    // reproducible.
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

/// Clamp a `u32` pixel dimension to a non-negative `i32` (a plain `as i32`
/// would wrap for sizes above `i32::MAX`).
fn dim_i32(v: u32) -> i32 {
    v.min(i32::MAX as u32) as i32
}

/// `FillColorOnAlpha`: source-over a constant color that **considers the
/// destination alpha**.
///
/// `color` is straight RGBA. `opa` semantics follow `ColorRect`:
/// * `opa <= 0` does nothing,
/// * `opa >= 255` writes the RGB with a forced `255` alpha (`FillARGB`),
/// * otherwise RGB is blended with the destination's effective opacity while
///   the output alpha is `255 - ((255 - dst_a) * (255 - opa) >> 8)`.
pub fn fill_color_on_alpha(bitmap: &mut BitmapState, rect: RectI, color: [u8; 4], opa: i32) {
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

/// Saturating per-channel addition of packed 8-bit values (four lanes packed
/// into `a`/`b`), without carry propagation between lanes.
fn saturated_add(a: u32, b: u32) -> u32 {
    let tmp = ((a & b) + (((a ^ b) >> 1) & 0x7f7f_7f7f)) & 0x8080_8080;
    let tmp = (tmp << 1) - (tmp >> 7);
    (a + b - tmp) | tmp
}

/// `FillColorOnAddAlpha`: additive-alpha (premultiplied) fill.
pub fn fill_color_on_add_alpha(bitmap: &mut BitmapState, rect: RectI, color: [u8; 4], opa: i32) {
    if opa <= 0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    let opa = opa.min(255) as u32;
    // Scale each RGB channel by `(channel * opa) >> 8`.
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

/// `FillColor`: set the RGB to `color` while **holding the destination
/// alpha**. `opa` 0 does nothing; `255` replaces RGB; otherwise the channels
/// blend as `(d * (255 - opa) + s * opa) >> 8`.
pub fn fill_color_hold_alpha(bitmap: &mut BitmapState, rect: RectI, color: [u8; 4], opa: i32) {
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

/// `FillMask`: replace the alpha channel with `value`, holding the RGB. In
/// this engine the alpha channel *is* the mask plane.
pub fn fill_mask(bitmap: &mut BitmapState, rect: RectI, value: u8) {
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
/// (`(alpha * (255 - level)) >> 8`). `level <= 0` does nothing, `level >= 255`
/// clears the alpha.
pub fn remove_const_opacity(bitmap: &mut BitmapState, rect: RectI, level: i32) {
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
pub fn colorize(bitmap: &mut BitmapState, rect: RectI, hue: i32, sat: i32, blend: f64) {
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

/// A tiny deterministic xorshift PRNG. The caller supplies the seed and owns
/// the state, so a single call stays reproducible for a given seed and the
/// function keeps no hidden state.
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

/// SplitMix64 finalizer: spreads the caller's seed bits into a 32-bit seed.
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
///
/// `seed` drives the generator instead of process-wide random state: the
/// caller, which owns the state, passes a fresh value per call. The same
/// `seed` reproduces the same noise.
pub fn noise(bitmap: &mut BitmapState, rect: RectI, level: i32, seed: u64) {
    if level <= 0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    let mut rng = NoiseRng(mix64(seed));
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
pub fn blit_copy(dst: &mut BitmapState, dx: i32, dy: i32, src: &BitmapState, src_rect: RectI) {
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
pub fn blit_copy_clipped(
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
            dx.saturating_add(src_rect.2.saturating_sub(src_rect.0)),
            dy.saturating_add(src_rect.3.saturating_sub(src_rect.1)),
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

/// `Blt` source-over: composite `src[src_rect]` onto `dst` at `(dx, dy)`,
/// clipping the destination both to the bitmap and to the layer's `clip`
/// rect. Each pixel is blended with the straight-alpha source-over math
/// ([`blend_pixel`]); `opa` scales the source alpha.
///
/// This is the pixel work behind `Layer.copyRect` and, with a different
/// `mode`, `operateRect`.
pub fn blit_over(
    dst: &mut BitmapState,
    dx: i32,
    dy: i32,
    source: BlitSource<'_>,
    clip: RectI,
    options: BlitOptions,
) {
    let BlitSource {
        bitmap: src,
        rect: src_rect,
    } = source;
    let BlitOptions {
        opa,
        treat_as_opaque,
        hold_alpha,
        ..
    } = options;
    let sw = src_rect.2.saturating_sub(src_rect.0);
    let sh = src_rect.3.saturating_sub(src_rect.1);
    if sw <= 0 || sh <= 0 || opa == 0 || src.width == 0 || src.height == 0 {
        return;
    }
    let Some(in_bounds) = intersect(dst, (dx, dy, dx.saturating_add(sw), dy.saturating_add(sh)))
    else {
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
            // `holdAlpha` suppresses the write to the destination alpha
            // channel entirely; capture it first so the composite math still
            // sees the real destination alpha.
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

/// `CopyRect` with an explicit `plane` selection (`TVP_BB_COPY_MAIN` /
/// `TVP_BB_COPY_MASK`), the primitive behind `Layer.copyRect`.
///
/// * `COPY_MAIN` — `CopyColor`: destination RGB = source RGB, destination
///   alpha held.
/// * `COPY_MASK` — `CopyMask`: destination alpha = source alpha, destination
///   RGB held.
/// * `COPY_MAIN | COPY_MASK` — `Copy`: straight copy of the source rect,
///   including its alpha.
pub fn blit_plane(
    dst: &mut BitmapState,
    dx: i32,
    dy: i32,
    src: &BitmapState,
    src_rect: RectI,
    clip: RectI,
    plane: u8,
) {
    if plane & (COPY_MAIN | COPY_MASK) == (COPY_MAIN | COPY_MASK) {
        // `Copy` writes the source rect straight over the destination (RGB
        // *and* alpha), transparent source pixels included — NOT a source-over
        // blend. Games rely on this to make old pixels vanish when they redraw
        // over existing content (e.g. `system/album.tjs::drawCompleteNumber`
        // redraws the percentage digits over the previous frame; a blend
        // leaves the old strokes).
        let sw = src_rect.2.saturating_sub(src_rect.0);
        let sh = src_rect.3.saturating_sub(src_rect.1);
        if sw <= 0 || sh <= 0 || src.width == 0 || src.height == 0 {
            return;
        }
        let Some(in_bounds) =
            intersect(dst, (dx, dy, dx.saturating_add(sw), dy.saturating_add(sh)))
        else {
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
    let sw = src_rect.2.saturating_sub(src_rect.0);
    let sh = src_rect.3.saturating_sub(src_rect.1);
    if sw <= 0 || sh <= 0 || src.width == 0 || src.height == 0 {
        return;
    }
    let Some(in_bounds) = intersect(dst, (dx, dy, dx.saturating_add(sw), dy.saturating_add(sh)))
    else {
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
pub fn tile_rect(
    dst: &mut BitmapState,
    dest: Rect,
    tile: &BitmapState,
    tile_rect: RectI,
    phase: (i32, i32),
) {
    let Rect {
        x: left,
        y: top,
        w: width,
        h: height,
    } = dest;
    let (x, y) = phase;
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
            if dx.saturating_add(sw) > dim_i32(width) {
                sw = dim_i32(width) - dx;
            }
            if dy.saturating_add(sh) > dim_i32(height) {
                sh = dim_i32(height) - dy;
            }
            if sw > 0 && sh > 0 {
                // Clip the tile against the destination bitmap too.
                let dest_x = left.saturating_add(dx);
                let dest_y = top.saturating_add(dy);
                let src_rect = (sx, sy, sx.saturating_add(sw), sy.saturating_add(sh));
                let Some(dc) = intersect(
                    dst,
                    (
                        dest_x,
                        dest_y,
                        dest_x.saturating_add(sw),
                        dest_y.saturating_add(sh),
                    ),
                ) else {
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
    let Some(dc) = intersect(
        dst,
        (
            dx,
            dy,
            dx.saturating_add(dim_i32(src.width)),
            dy.saturating_add(dim_i32(src.height)),
        ),
    ) else {
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
    let Some(dc) = intersect(
        dst,
        (
            dx,
            dy,
            dx.saturating_add(dim_i32(src.width)),
            dy.saturating_add(dim_i32(src.height)),
        ),
    ) else {
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
pub fn do_drop_shadow(
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
    // `original` supplies the top layer and the shadow's alpha source;
    // `shadow` is the recoloured, blurred buffer. `bitmap` itself is the work
    // buffer, so only two full-buffer copies are made.
    let original = bitmap.clone();
    // The shadow buffer starts from the alpha of `bitmap`, with RGB set to the
    // shadow color, and is then blurred.
    let mut shadow = bitmap.clone();
    for chunk in shadow.rgba.chunks_exact_mut(4) {
        chunk[0] = shadow_color[0];
        chunk[1] = shadow_color[1];
        chunk[2] = shadow_color[2];
    }
    blur_bitmap_in_place(&mut shadow, blur, blur);
    // Original with the offset shadow, then the original on top.
    composite_normal(bitmap, dx, dy, &shadow, shadow_opacity);
    composite_normal(bitmap, 0, 0, &original, 255);
    bitmap.mark_dirty();
}

/// SDK `Layer.doBlurLight`: blur a copy, composite it source-over at
/// `blur_opacity`, then composite it again with `light_type` at
/// `light_opacity`. `ltPsHardLight` uses a hard-light blend (the SDK
/// default); every other `light_type` falls back to source-over.
pub fn do_blur_light(
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
    // `ltPsHardLight = 19`.
    if light_type == 19 {
        composite_hard_light(bitmap, 0, 0, &light, light_opacity);
    } else {
        composite_normal(bitmap, 0, 0, &light, light_opacity);
    }
    bitmap.mark_dirty();
}

/// Fill `[left, top, left+width, top+height)` with `color` using the
/// blend/operation `mode`. This is the
/// SDK's `Layer.fillOperateRect`, which tiles a solid-color layer through
/// `operateRect`. The common modes are real; the Photoshop-specific modes
/// (`ltPs*`) reduce to source-over until the PS blend pipeline lands.
pub fn fill_operate_rect(
    bitmap: &mut BitmapState,
    dest: Rect,
    color: [u8; 4],
    mode: i64,
    hold_alpha: bool,
) {
    let Rect {
        x: left,
        y: top,
        w: width,
        h: height,
    } = dest;
    let rect = (
        left,
        top,
        left.saturating_add(dim_i32(width)),
        top.saturating_add(dim_i32(height)),
    );
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

/// The horizontal box sum for one row: `out[x]` receives the sum of
/// `rgba[y][max(0, x-rx) ..= min(w-1, x+rx)]` (four channels). Uses a sliding
/// running sum, so the row costs `O(w)`.
fn horizontal_box_sum(rgba: &[u8], w: usize, y: usize, rx: usize, out: &mut [[u64; 4]]) {
    let base = y * w * 4;
    let src = &rgba[base..base + w * 4];
    let mut running = [0u64; 4];
    for x in 0..=rx.min(w - 1) {
        let i = x * 4;
        for c in 0..4 {
            running[c] += u64::from(src[i + c]);
        }
    }
    for (x, slot) in out.iter_mut().enumerate() {
        *slot = running;
        // Slide to `x + 1`: the left edge moves off `x - rx` (when it has
        // entered) and the right edge moves onto `x + 1 + rx` (when it is
        // still inside the row).
        if x >= rx {
            let i = (x - rx) * 4;
            for c in 0..4 {
                running[c] -= u64::from(src[i + c]);
            }
        }
        let add = x.saturating_add(1).saturating_add(rx);
        if add < w {
            let i = add * 4;
            for c in 0..4 {
                running[c] += u64::from(src[i + c]);
            }
        }
    }
}

/// A local **separable running-sum box blur** over the whole bitmap with a
/// window clamped to the image edges.
///
/// This is `O(w · h)` and reads the source in place into one scratch output
/// buffer (no full `rgba.clone()`). The two passes keep *sums* rather than
/// averages, so the final single integer division yields the exact box mean,
/// including at the clamped edges where the window is smaller.
pub fn blur_bitmap_in_place(bitmap: &mut BitmapState, xradius: u32, yradius: u32) {
    if bitmap.width == 0 || bitmap.height == 0 || (xradius == 0 && yradius == 0) {
        return;
    }
    let w = bitmap.width as usize;
    let h = bitmap.height as usize;
    // A radius wider than the image already covers the whole axis; clamping
    // keeps the sums and indices small without changing the result.
    let rx = (xradius as usize).min(w - 1);
    let ry = (yradius as usize).min(h - 1);

    let mut out = vec![0u8; bitmap.rgba.len()];
    let mut row = vec![[0u64; 4]; w];
    let mut acc = vec![[0u64; 4]; w];
    let window = |x: usize, r: usize| -> u64 {
        x.saturating_add(r).min(w - 1) as u64 - x.saturating_sub(r) as u64 + 1
    };

    // Seed the sliding column accumulator with the first row window.
    for y in 0..=ry {
        horizontal_box_sum(&bitmap.rgba, w, y, rx, &mut row);
        for (a, r) in acc.iter_mut().zip(row.iter()) {
            for c in 0..4 {
                a[c] += r[c];
            }
        }
    }

    for y in 0..h {
        let y0 = y.saturating_sub(ry);
        let cy = y.saturating_add(ry).min(h - 1) as u64 - y0 as u64 + 1;
        let base = y * w * 4;
        for (x, acc_x) in acc.iter().enumerate() {
            let count = window(x, rx) * cy;
            let i = base + x * 4;
            for c in 0..4 {
                out[i + c] = (acc_x[c] / count).min(255) as u8;
            }
        }
        // Slide to `y + 1`.
        if y >= ry {
            horizontal_box_sum(&bitmap.rgba, w, y - ry, rx, &mut row);
            for (a, r) in acc.iter_mut().zip(row.iter()) {
                for c in 0..4 {
                    a[c] -= r[c];
                }
            }
        }
        let add = y.saturating_add(1).saturating_add(ry);
        if add < h {
            horizontal_box_sum(&bitmap.rgba, w, add, rx, &mut row);
            for (a, r) in acc.iter_mut().zip(row.iter()) {
                for c in 0..4 {
                    a[c] += r[c];
                }
            }
        }
    }
    std::mem::swap(&mut bitmap.rgba, &mut out);
    bitmap.mark_dirty();
}

// ---------------------------------------------------------------------------
// Stretch / tone operations (the thumbnail pipeline)
// ---------------------------------------------------------------------------

/// Clamp a sample coordinate to `rect` (a half-open source region) or, when
/// `rect` is empty, to the whole bitmap. The `stRefNoClip` stretch flag allows
/// sampling outside the given rectangle, so callers pass the whole image as
/// `rect` in that case.
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
/// `radius` (samples to each side). Out-of-range samples clamp to the edge of
/// `rect`.
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
    let mut acc = [0u64; 4];
    let mut count = 0u64;
    for yy in iy0..iy1 {
        for xx in ix0..ix1 {
            let p = read_pixel_rect(bitmap, rect, xx, yy);
            for (c, a) in acc.iter_mut().enumerate() {
                *a += u64::from(p[c]);
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
/// destination rect onto the source rect. `stretch_type` is a numeric stretch
/// type: the low 16 bits select the resampler, the `stRefNoClip` (0x10000)
/// flag lets the kernel read outside `src`. Unknown resamplers fall back to
/// bilinear rather than throwing.
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

/// Apply a numeric stretch type (low 16 bits) kernel at source position
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
/// The straight-copy path is the operation behind `saveLayerImage` thumbnails.
pub fn stretch_blit(
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

/// [`stretch_blit`] with an explicit `plane` selection, which picks whether
/// the copy includes the alpha plane.
pub fn stretch_blit_plane(
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

/// Luma `(54*R + 183*G + 19*B) >> 8` written to all three channels, alpha
/// held.
pub fn do_gray_scale(bitmap: &mut BitmapState, rect: RectI) {
    let Some((x0, y0, x1, y1)) = intersect(bitmap, rect) else {
        return;
    };
    for y in y0..y1 {
        for x in x0..x1 {
            let mut d = read_pixel(bitmap, x, y);
            let gray =
                ((54 * u32::from(d[0]) + 183 * u32::from(d[1]) + 19 * u32::from(d[2])) >> 8) as u8;
            d[0] = gray;
            d[1] = gray;
            d[2] = gray;
            write_pixel(bitmap, x, y, d);
        }
    }
    bitmap.mark_dirty();
}

/// Per-channel gamma adjustment (R/G/B gamma, floor, ceil).
#[derive(Clone, Copy, Debug)]
pub struct GammaAdjust {
    /// Red gamma exponent.
    pub r_gamma: f64,
    /// Red output floor (added after the gamma ramp).
    pub r_floor: i32,
    /// Red output ceiling (the top of the gamma ramp).
    pub r_ceil: i32,
    /// Green gamma exponent.
    pub g_gamma: f64,
    /// Green output floor.
    pub g_floor: i32,
    /// Green output ceiling.
    pub g_ceil: i32,
    /// Blue gamma exponent.
    pub b_gamma: f64,
    /// Blue output floor.
    pub b_floor: i32,
    /// Blue output ceiling.
    pub b_ceil: i32,
}

impl GammaAdjust {
    /// The identity adjustment.
    pub fn intact() -> Self {
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
    // The ramp is `exp(log(i/255) * (1/gamma))`, i.e. `pow(i/255, 1/gamma)`.
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

/// Per-channel gamma/level remap applied only to non-fully-transparent
/// pixels.
pub fn adjust_gamma(bitmap: &mut BitmapState, rect: RectI, data: GammaAdjust) {
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

/// `ApplyLightContrast`: brightness offset followed by a
/// `(259*(C+255))/(255*(259-C))` contrast factor, both clamped per channel;
/// alpha held.
pub fn light_contrast(bitmap: &mut BitmapState, rect: RectI, brightness: i32, contrast: i32) {
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

/// `ConvertAlphaToAddAlpha`: premultiply RGB by alpha (`(c * a) >> 8`),
/// holding alpha.
pub fn convert_alpha_to_add_alpha(bitmap: &mut BitmapState, rect: RectI) {
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

/// `ConvertAddAlphaToAlpha`: unpremultiply RGB (`c * 255 / a` clamped to
/// `255`; alpha 0 leaves RGB zero), holding alpha.
pub fn convert_add_alpha_to_alpha(bitmap: &mut BitmapState, rect: RectI) {
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

/// `Copy9Patch`: scale a source bitmap into the destination using a 9-slice
/// `margin` (left, top, right, bottom). The corners are copied 1:1, the edges
/// are stretched along one axis and the center on both. `right`/`bottom` are
/// *offsets from the far edge*.
pub fn copy_9patch(dst: &mut BitmapState, src: &BitmapState) -> Option<RectI> {
    if dst.width == 0 || dst.height == 0 || src.width < 11 || src.height < 11 {
        return None;
    }
    if dst.width < src.width - 2 || dst.height < src.height - 2 {
        return None;
    }
    let w = src.width as i32;
    let h = src.height as i32;
    // The margins come from the alpha runs on the source's bottom row
    // (left/right margins) and right column (top/bottom margins).
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

/// `LRFlip`: mirror the whole main image horizontally.
pub fn flip_lr(bitmap: &mut BitmapState) {
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

/// `UDFlip`: mirror the whole main image vertically.
pub fn flip_ud(bitmap: &mut BitmapState) {
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

/// The normalized 1-D Gaussian kernel of `radius` taps for the given `sigma`.
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

/// Separable Gaussian blur over the clip region (`Layer.gaussianBlur`).
/// Pixels outside the region clamp to the edge.
pub fn gaussian_blur(bitmap: &mut BitmapState, rect: RectI, radius: i32, sigma: f32) {
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

/// Blend one source pixel onto `dst` using a numeric operation mode. The
/// Photoshop modes (`omPs*`) apply their per-channel blend function and then
/// the source alpha; the modes with no dedicated kernel reduce to source-over.
pub(crate) fn blend_pixel_mode(
    dst: &mut BitmapState,
    x: i32,
    y: i32,
    src: [u8; 4],
    opa: u8,
    mode: i64,
) {
    if x < 0 || y < 0 || x as u32 >= dst.width || y as u32 >= dst.height {
        return;
    }
    let Some(i) = dst.pixel_offset(x as u32, y as u32) else {
        return;
    };
    let mut pixel = [
        dst.rgba[i],
        dst.rgba[i + 1],
        dst.rgba[i + 2],
        dst.rgba[i + 3],
    ];
    blend_pixel_mode_at(&mut pixel, src, opa, mode);
    dst.rgba[i..i + 4].copy_from_slice(&pixel);
}

/// The per-pixel core of [`blend_pixel_mode`]: blend `src` into one
/// destination pixel, in place.
///
/// The kernel is factored out of the bitmap accessor so a pass that owns a
/// destination row slice (the affine nearest pass) blends with the same
/// arithmetic.
fn blend_pixel_mode_at(pixel: &mut [u8; 4], src: [u8; 4], opa: u8, mode: i64) {
    match mode {
        // omOpaque / ltOpaque: copy the source (including alpha).
        1 => *pixel = src,
        // omAdditive / ltAdditive: saturating add, source scaled by `opa`.
        3 => {
            for c in 0..3 {
                pixel[c] =
                    pixel[c].saturating_add(((u32::from(src[c]) * u32::from(opa)) / 255) as u8);
            }
        }
        // omSubtractive / ltSubtractive.
        4 => {
            for c in 0..3 {
                pixel[c] =
                    pixel[c].saturating_sub(((u32::from(src[c]) * u32::from(opa)) / 255) as u8);
            }
        }
        // omMultiplicative / ltMultiplicative.
        5 => {
            let opa = u32::from(opa);
            for c in 0..3 {
                let s = u32::from(src[c]) * opa / 255;
                pixel[c] = (u32::from(pixel[c]) * s / 255) as u8;
            }
        }
        // omDarken / ltDarken.
        9 => {
            for c in 0..3 {
                pixel[c] = pixel[c].min(src[c]);
            }
        }
        // omLighten / ltLighten.
        10 => {
            for c in 0..3 {
                pixel[c] = pixel[c].max(src[c]);
            }
        }
        // omScreen / ltScreen.
        11 => {
            for c in 0..3 {
                let blended = 255 - (255 - u32::from(pixel[c])) * (255 - u32::from(src[c])) / 255;
                pixel[c] =
                    ((u32::from(pixel[c]) * (255 - u32::from(opa)) + blended * 255) / 255) as u8;
            }
        }
        // ltPsHardLight (19): standard hard-light, alpha-composited.
        19 => {
            let mut blended = [0u8; 4];
            for c in 0..3 {
                let sv = i32::from(src[c]);
                let dv = i32::from(pixel[c]);
                blended[c] = if sv < 128 {
                    (2 * sv * dv / 255).clamp(0, 255) as u8
                } else {
                    (255 - 2 * (255 - sv) * (255 - dv) / 255).clamp(0, 255) as u8
                };
            }
            blended[3] = src[3];
            blend_pixel_at(pixel, blended, 255, opa);
        }
        // The Photoshop modes apply their per-channel blend function and then
        // the source alpha.
        // omPsNormal / ltPsNormal: the same alpha blend as omAlpha.
        13 => blend_pixel_at(pixel, src, 255, opa),
        // omPsAdditive / ltPsAdditive.
        14 => {
            let mut blended = [0u8; 4];
            for c in 0..3 {
                blended[c] = pixel[c].saturating_add(src[c]);
            }
            blended[3] = src[3];
            blend_pixel_at(pixel, blended, 255, opa);
        }
        // omPsSubtractive / ltPsSubtractive.
        15 => {
            let mut blended = [0u8; 4];
            for c in 0..3 {
                blended[c] = pixel[c].saturating_sub(src[c]);
            }
            blended[3] = src[3];
            blend_pixel_at(pixel, blended, 255, opa);
        }
        // omPsMultiplicative / ltPsMultiplicative.
        16 => {
            let mut blended = [0u8; 4];
            for c in 0..3 {
                blended[c] = (u32::from(pixel[c]) * u32::from(src[c]) / 255) as u8;
            }
            blended[3] = src[3];
            blend_pixel_at(pixel, blended, 255, opa);
        }
        // omPsScreen / ltPsScreen.
        17 => {
            let mut blended = [0u8; 4];
            for c in 0..3 {
                blended[c] =
                    (255 - (255 - u32::from(pixel[c])) * (255 - u32::from(src[c])) / 255) as u8;
            }
            blended[3] = src[3];
            blend_pixel_at(pixel, blended, 255, opa);
        }
        // omPsColorDodge / ltPsColorDodge.
        21 => {
            let mut blended = [0u8; 4];
            for c in 0..3 {
                let dv = i32::from(pixel[c]);
                let sv = i32::from(src[c]);
                blended[c] = if sv >= 255 {
                    255
                } else {
                    ((dv * 255) / (255 - sv)).min(255) as u8
                };
            }
            blended[3] = src[3];
            blend_pixel_at(pixel, blended, 255, opa);
        }
        // omPsLighten / ltPsLighten.
        24 => {
            let mut blended = [0u8; 4];
            for c in 0..3 {
                blended[c] = pixel[c].max(src[c]);
            }
            blended[3] = src[3];
            blend_pixel_at(pixel, blended, 255, opa);
        }
        // omPsDarken / ltPsDarken.
        25 => {
            let mut blended = [0u8; 4];
            for c in 0..3 {
                blended[c] = pixel[c].min(src[c]);
            }
            blended[3] = src[3];
            blend_pixel_at(pixel, blended, 255, opa);
        }
        // Everything else (omAlpha=2, omAddAlpha=12 and the remaining modes):
        // source-over.
        _ => blend_pixel_at(pixel, src, 255, opa),
    }
}

/// `OperateRect`: blend a source region onto `dst` at `(dx, dy)` with `mode`
/// and `opa`.
pub fn operate_rect(
    dst: &mut BitmapState,
    dx: i32,
    dy: i32,
    source: BlitSource<'_>,
    options: BlitOptions,
) {
    let BlitSource {
        bitmap: src,
        rect: src_rect,
    } = source;
    let BlitOptions {
        mode,
        opa,
        hold_alpha,
        ..
    } = options;
    let sw = src_rect.2.saturating_sub(src_rect.0);
    let sh = src_rect.3.saturating_sub(src_rect.1);
    if sw <= 0 || sh <= 0 {
        return;
    }
    let Some(dc) = intersect(dst, (dx, dy, dx.saturating_add(sw), dy.saturating_add(sh))) else {
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
pub fn stretch_blit_mode(
    dst: &mut BitmapState,
    dest: RectI,
    source: BlitSource<'_>,
    stretch_type: i64,
    options: BlitOptions,
) {
    let BlitSource {
        bitmap: src,
        rect: src_rect,
    } = source;
    let BlitOptions {
        mode,
        opa,
        hold_alpha,
        ..
    } = options;
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
/// `p1` (src RT), `p2` (src LB). Returns `(a, b, c, d, e, f)` such that
/// `u,v ∈ [0,1]` maps to `x = a + b*u + c*v`, `y = d + e*u + f*v`.
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
pub fn affine_blit(
    dst: &mut BitmapState,
    affine: AffineBlit,
    source: BlitSource<'_>,
    options: BlitOptions,
) {
    let AffineBlit {
        corners,
        stretch_type,
        clear,
        clear_color,
    } = affine;
    let [p0, p1, p2] = corners;
    let BlitSource {
        bitmap: src,
        rect: src_rect,
    } = source;
    let BlitOptions { mode, opa, .. } = options;
    let m = affine_forward(p0, p1, p2);
    let det = m[1] * m[5] - m[2] * m[4];
    if det.abs() < 1e-12 {
        return;
    }
    // Inverse of the 2x2 [b c; e f].
    let inv = [m[5] / det, -m[2] / det, -m[4] / det, m[1] / det];
    // Bounding box of the mapped unit square.
    let bbox = affine_dest_bbox(m);
    let Some(dc) = intersect(dst, bbox) else {
        return;
    };
    if clear {
        for y in dc.1..dc.3 {
            for x in dc.0..dc.2 {
                write_pixel(dst, x, y, clear_color);
            }
        }
    }
    // The nearest resampler on an axis-aligned quad reads one source row per
    // destination row, which the scanline pass does without the per-pixel
    // inverse map.
    let pass = AffinePass {
        map: m,
        inverse: inv,
        source: src,
        source_rect: src_rect,
        dest_rect: dc,
        stretch_type: stretch_type & 0xffff,
        mode,
        opa,
    };
    if pass.stretch_type == 0 && affine_nearest_axis_aligned(m) {
        affine_blit_nearest_rows(dst, pass);
    } else {
        affine_blit_generic(dst, pass);
    }
    dst.mark_dirty();
}

/// The bounding box of the mapped unit square.
fn affine_dest_bbox(m: [f64; 6]) -> RectI {
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
    (
        min_x,
        min_y,
        max_x.saturating_add(1),
        max_y.saturating_add(1),
    )
}

/// The generic affine pass: every destination pixel is inverse-mapped through
/// the 2x2 matrix and the source is sampled with the requested resampler.
fn affine_blit_generic(dst: &mut BitmapState, pass: AffinePass<'_>) {
    let AffinePass {
        map: m,
        inverse: inv,
        source: src,
        source_rect: src_rect,
        dest_rect: dc,
        stretch_type,
        mode,
        opa,
    } = pass;
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
            // Affine sampling clamps to the whole source image (it may
            // interpolate across the source-rect border), and supports the
            // same resampler set as `stretch*`.
            let rect = (0, 0, src.width as i32, src.height as i32);
            let color = sample_kind(src, rect, fx, fy, stretch_type);
            blend_pixel_mode(dst, x, y, color, opa, mode);
        }
    }
}

/// Whether the affine map is axis-aligned enough for the nearest pass.
///
/// The source's `u` axis must run along the destination's `x` axis, so the
/// `y` extent along `u` (`m[4]`) vanishes: the source row is then the same
/// for every pixel of a destination row, which is what lets the scanline
/// pass resolve it once per row. `m[4]` must vanish *exactly* — a remainder
/// would let the source row change inside a destination row, which only the
/// per-pixel pass resolves.
///
/// `m[2]` (the `x` extent along `v`) is the map's shear. The scanline pass
/// computes the source `x` with the generic pass's arithmetic, so a shear
/// would stay correct, but a sheared quad is not the axis-aligned case.
fn affine_nearest_axis_aligned(m: [f64; 6]) -> bool {
    m[4] == 0.0 && m[2].abs() <= 1e-12
}

/// The nearest resampler's scanline pass over an axis-aligned quad.
///
/// The generic pass inverse-maps every destination pixel through the 2x2
/// matrix and clamps one sample per pixel. On an axis-aligned quad a
/// destination row reads a single source row, so the pass resolves that row
/// once and walks the destination row with the generic pass's arithmetic:
/// the sampled texel is the same, pixel for pixel.
///
/// The destination rows are independent, so the pass splits them into bands
/// and runs the bands in parallel.
fn affine_blit_nearest_rows(dst: &mut BitmapState, pass: AffinePass<'_>) {
    let dst_width = dst.width as i32;
    let src_width = pass.source.width as i32;
    let src_height = pass.source.height as i32;
    if dst_width <= 0 || src_width <= 0 || src_height <= 0 {
        return;
    }
    let row_bytes = dst_width as usize * 4;
    let rows = (pass.dest_rect.3 - pass.dest_rect.1) as usize;
    let band_start = pass.dest_rect.1 as usize * row_bytes;
    let Some(band) = dst.rgba.get_mut(band_start..band_start + rows * row_bytes) else {
        return;
    };
    let threads = affine_thread_count(rows);
    if threads <= 1 {
        affine_nearest_band(band, pass.dest_rect.1, dst_width, pass);
        return;
    }
    let rows_per_band = rows.div_ceil(threads);
    let chunk_bytes = rows_per_band * row_bytes;
    let chunks: Vec<&mut [u8]> = band.chunks_mut(chunk_bytes).collect();
    std::thread::scope(|scope| {
        for (index, chunk) in chunks.into_iter().enumerate() {
            let first_row = pass.dest_rect.1 + (index * rows_per_band) as i32;
            scope.spawn(move || {
                affine_nearest_band(chunk, first_row, dst_width, pass);
            });
        }
    });
}

/// The number of bands the nearest pass splits its destination rows across.
///
/// A short pass stays on the calling thread: splitting it would cost more
/// than it saves.
fn affine_thread_count(rows: usize) -> usize {
    const MIN_ROWS_PER_BAND: usize = 64;
    if rows < MIN_ROWS_PER_BAND * 2 {
        return 1;
    }
    let available = std::thread::available_parallelism().map_or(1, |count| count.get());
    available.clamp(1, 8).min(rows / MIN_ROWS_PER_BAND).max(1)
}

/// The map and the blend an affine pass walks with.
#[derive(Clone, Copy)]
struct AffinePass<'a> {
    /// The forward map of the mapped quad.
    map: [f64; 6],
    /// The inverse of the map's 2x2 part.
    inverse: [f64; 4],
    /// The source bitmap.
    source: &'a BitmapState,
    /// The source rectangle the pass reads.
    source_rect: RectI,
    /// The clipped destination rectangle of the pass.
    dest_rect: RectI,
    /// The resampler.
    stretch_type: i64,
    /// The blend mode.
    mode: i64,
    /// The source opacity.
    opa: u8,
}

/// The nearest pass over one band of destination rows.
///
/// `band` holds the destination rows from `first_row`, in order.
fn affine_nearest_band(band: &mut [u8], first_row: i32, dst_width: i32, pass: AffinePass<'_>) {
    let AffinePass {
        map: m,
        inverse: inv,
        source: src,
        source_rect: src_rect,
        dest_rect: dc,
        mode,
        opa,
        ..
    } = pass;
    let sw = (src_rect.2 - src_rect.0) as f64;
    let sh = (src_rect.3 - src_rect.1) as f64;
    let src_width = src.width as i32;
    let src_height = src.height as i32;
    let row_bytes = dst_width as usize * 4;
    let src_row_bytes = src_width as usize * 4;
    for (index, row) in band.chunks_exact_mut(row_bytes).enumerate() {
        let y = first_row + index as i32;
        let py = y as f64 + 0.5 - m[3];
        // The map is axis-aligned, so the source row is the same for every
        // pixel of this destination row.
        let v = inv[3] * py;
        if !(0.0..=1.0).contains(&v) {
            continue;
        }
        let fy = src_rect.1 as f32 + (v * sh) as f32 - 0.5;
        let sy = ((fy + 0.5).floor() as i32).clamp(0, src_height - 1);
        let source_row = &src.rgba[(sy as usize) * src_row_bytes..][..src_row_bytes];
        // The `u` term of the inverse map that does not depend on the
        // destination column is the same for the whole row.
        let inv1_py = inv[1] * py;
        for column in dc.0 as usize..dc.2 as usize {
            let x = column as i32;
            let px = x as f64 + 0.5 - m[0];
            let u = inv[0] * px + inv1_py;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let fx = src_rect.0 as f32 + (u * sw) as f32 - 0.5;
            let sx = ((fx + 0.5).floor() as i32).clamp(0, src_width - 1);
            let source_offset = sx as usize * 4;
            let color = [
                source_row[source_offset],
                source_row[source_offset + 1],
                source_row[source_offset + 2],
                source_row[source_offset + 3],
            ];
            let offset = column * 4;
            let mut destination = [
                row[offset],
                row[offset + 1],
                row[offset + 2],
                row[offset + 3],
            ];
            blend_pixel_mode_at(&mut destination, color, opa, mode);
            row[offset..offset + 4].copy_from_slice(&destination);
        }
    }
}

/// The operation-mode number [`blend_pixel_mode`] uses for the generic
/// [`BlendMode`] stored on a layer.
///
/// The operation modes share the native layer-type numbering
/// (`omOpaque == ltOpaque == 1`, …), so each generic mode maps to the type
/// whose behaviour it names. The subtree composite starts from the typed L2
/// blend; the `operate*` family receives an explicit native operation mode
/// from L3.
const fn blend_operation_mode(blend: BlendMode) -> i64 {
    match blend {
        BlendMode::Replace => 1,           // omOpaque / ltOpaque
        BlendMode::SourceOver => 2,        // omAlpha / ltAlpha
        BlendMode::Additive => 3,          // omAdditive / ltAdditive
        BlendMode::Subtractive => 4,       // omSubtractive / ltSubtractive
        BlendMode::Multiply => 5,          // omMultiplicative / ltMultiplicative
        BlendMode::AddAlpha => 12,         // omAddAlpha / ltAddAlpha
        BlendMode::Screen => 11,           // omScreen / ltScreen
        BlendMode::PsNormal => 13,         // omPsNormal / ltPsNormal
        BlendMode::PsAdditive => 14,       // omPsAdditive / ltPsAdditive
        BlendMode::PsSubtractive => 15,    // omPsSubtractive / ltPsSubtractive
        BlendMode::PsMultiplicative => 16, // omPsMultiplicative / ltPsMultiplicative
        BlendMode::PsScreen => 17,         // omPsScreen / ltPsScreen
        BlendMode::DestinationDependent(kind) => match kind {
            DstBlend::ColorDodge => 8,    // omDodge / ltDodge
            DstBlend::PsColorDodge => 21, // omPsColorDodge / ltPsColorDodge
            DstBlend::ColorDodge5 => 22,  // omPsColorDodge5 / ltPsColorDodge5
            DstBlend::ColorBurn => 23,    // omPsColorBurn / ltPsColorBurn
            DstBlend::Darken => 9,        // omDarken / ltDarken
            DstBlend::PsDarken => 25,     // omPsDarken / ltPsDarken
            DstBlend::Lighten => 10,      // omLighten / ltLighten
            DstBlend::PsLighten => 24,    // omPsLighten / ltPsLighten
            DstBlend::Difference => 26,   // omPsDifference / ltPsDifference
            DstBlend::Difference5 => 27,  // omPsDifference5 / ltPsDifference5
            DstBlend::Exclusion => 28,    // omPsExclusion / ltPsExclusion
            DstBlend::Overlay => 18,      // omPsOverlay / ltPsOverlay
            DstBlend::HardLight => 19,    // omPsHardLight / ltPsHardLight
            DstBlend::SoftLight => 20,    // omPsSoftLight / ltPsSoftLight
        },
    }
}

/// Whether `id` is a (direct or indirect) child of `ancestor` in the layer
/// tree, so a subtree walk can be restricted to the root's descendants; a
/// sibling window root is a separate tree.
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

/// Whether `layer` has at least one direct child that is itself visible with
/// nonzero opacity.
fn has_visible_child(scene: &Scene, layer: &crate::scene::LayerState) -> bool {
    layer.children.iter().any(|&child| {
        scene
            .layer(child)
            .is_some_and(|c| c.visible && c.opacity != 0.0)
    })
}

/// Product of the opacities from `id` up to (but excluding) `root`.
///
/// The subtree root's own opacity is applied by the projection when it draws
/// the rendered content, so it must not be baked into the cache; a deeper
/// layer's own opacity and every intermediate ancestor's do belong to the
/// composited pixels.
fn subtree_opacity(scene: &Scene, id: u32, root: u32) -> f32 {
    let mut opacity = 1.0f32;
    let mut current = Some(id);
    for _ in 0..1024 {
        let Some(c) = current else { break };
        if c == root {
            break;
        }
        let Some(layer) = scene.layer(c) else { break };
        opacity *= layer.opacity.clamp(0.0, 1.0);
        current = layer.parent;
    }
    opacity
}

/// Allocate a zeroed `w` x `h` RGBA bitmap in the scene. `None` when the
/// size overflows or the allocation fails.
fn alloc_bitmap(scene: &mut Scene, w: u32, h: u32) -> Option<u32> {
    let bytes = (w as usize).checked_mul(h as usize)?.checked_mul(4)?;
    let mut rgba = Vec::new();
    if rgba.try_reserve_exact(bytes).is_err() {
        return None;
    }
    rgba.resize(bytes, 0);
    Some(scene.add_bitmap(w, h, rgba))
}

/// Allocate `root`'s composited subtree content at `w` x `h`, reusing the
/// existing one when its size already matches. A newly allocated content is
/// marked for recomposition.
fn ensure_layer_content(scene: &mut Scene, root: u32, w: u32, h: u32) -> Option<u32> {
    if let Some(id) = scene.layer(root)?.cache_content {
        if scene.bitmap(id).map(|b| (b.width, b.height)) == Some((w, h)) {
            return Some(id);
        }
        scene.remove_bitmap(id);
        if scene.layer(root)?.cache_bitmap == Some(id) {
            scene.set_cache_bitmap(root, None);
        }
    }
    let id = alloc_bitmap(scene, w, h)?;
    let layer = scene.layer_mut_untracked(root)?;
    layer.cache_content = Some(id);
    layer.cache_recalc = true;
    Some(id)
}

/// Drop `root`'s composited content and its transition frame. The two can be
/// the same bitmap (the source endpoint draws its content); it is released
/// once.
fn release_subtree_caches(scene: &mut Scene, root: u32) {
    let (content, display) = match scene.layer_mut_untracked(root) {
        Some(layer) => (layer.cache_content.take(), layer.cache_bitmap.take()),
        None => return,
    };
    if display != content
        && let Some(display) = display
    {
        scene.release_bitmap(display);
    }
    if let Some(content) = content {
        scene.release_bitmap(content);
    }
}

/// Allocate or reuse `root`'s transition frame at `w` x `h`: the bitmap a
/// whole-subtree transition blends into, which the projection draws in place
/// of the layer's subtree. Distinct from the composited content so the blend
/// never overwrites the content it reads.
pub fn ensure_transition_display(scene: &mut Scene, root: u32, w: u32, h: u32) -> Option<u32> {
    if let Some(id) = scene.layer(root)?.cache_bitmap
        && scene.bitmap(id).map(|b| (b.width, b.height)) == Some((w, h))
    {
        return Some(id);
    }
    let stale = scene.layer_mut_untracked(root)?.cache_bitmap.take();
    if let Some(old) = stale {
        scene.release_bitmap(old);
    }
    let id = alloc_bitmap(scene, w, h)?;
    scene.set_cache_bitmap(root, Some(id));
    Some(id)
}

/// Allocate or reuse `root`'s self image at `w` x `h`: the image a transition
/// blends this layer's own image and another layer's into while the layer's
/// children keep drawing live. It is drawn in place of the layer's main image
/// and is distinct from it, so the blend never overwrites pixels a script can
/// read. `None` for an unknown layer or an image that cannot be allocated;
/// the slot is not part of the subtree cache and [`release_subtree_caches`]
/// leaves it alone.
pub fn ensure_transition_self_image(scene: &mut Scene, root: u32, w: u32, h: u32) -> Option<u32> {
    if let Some(id) = scene.layer(root)?.transition_self
        && scene.bitmap(id).map(|b| (b.width, b.height)) == Some((w, h))
    {
        return Some(id);
    }
    scene.set_transition_self_image(root, None);
    let id = alloc_bitmap(scene, w, h)?;
    scene.set_transition_self_image(root, Some(id));
    Some(id)
}

/// Sum of the drawn source images' [`BitmapState::content_generation`] for
/// `root`'s subtree. It mirrors the visibility filter and the drawn-image
/// selection of [`collect_subtree_draws`], so a change to a hidden layer does
/// not invalidate the cache while an edit to any drawn image does — including
/// the self image a transition blends into, even when the edit did not go
/// through a layer accessor.
fn subtree_source_generation(scene: &Scene, root: u32) -> u64 {
    let Some(root_layer) = scene.layer(root) else {
        return 0;
    };
    let window = root_layer.window;
    let mut sum = 0u64;
    for id in scene.window_layer_order(window) {
        if id != root && !is_descendant(scene, id, root) {
            continue;
        }
        let Some(layer) = scene.layer(id) else {
            continue;
        };
        if id != root && !(layer.visible && layer.opacity != 0.0) {
            continue;
        }
        if id != root && (subtree_opacity(scene, id, root) * 255.0).round() == 0.0 {
            continue;
        }
        if let Some(bitmap) = subtree_drawn_image(layer).and_then(|id| scene.bitmap(id)) {
            sum = sum.wrapping_add(bitmap.content_generation);
        }
    }
    sum
}

/// One resolved draw for the subtree cache, with the layer's own placement
/// (the `imageLeft`/`imageTop`/`imageWidth`/`imageHeight` transform) and draw
/// face.
struct SubtreeDraw {
    source: BitmapState,
    dx: i32,
    dy: i32,
    /// Destination size in cache pixels; differs from the source region only
    /// when the layer scales its image.
    dw: i32,
    dh: i32,
    /// Source region in bitmap pixels.
    src: RectI,
    mode: i64,
    opa: u8,
    hold_alpha: bool,
}

/// A `width` x `height` grayscale image (the `dfMask`/`dfProvince` face).
fn grayscale_bitmap(
    width: u32,
    height: u32,
    value: impl Fn(u32, u32) -> u8,
) -> Option<BitmapState> {
    let bytes = (width as usize)
        .checked_mul(height as usize)?
        .checked_mul(4)?;
    let mut rgba = Vec::new();
    if rgba.try_reserve_exact(bytes).is_err() {
        return None;
    }
    rgba.resize(bytes, 0);
    for y in 0..height {
        for x in 0..width {
            let gray = value(x, y);
            let i = (y as usize * width as usize + x as usize) * 4;
            rgba[i] = gray;
            rgba[i + 1] = gray;
            rgba[i + 2] = gray;
            rgba[i + 3] = 255;
        }
    }
    Some(BitmapState::new(width, height, rgba))
}

/// The bitmap id a layer contributes to its own composite: the main image,
/// or the self image a transition stands in for it while the subtree stays
/// live. The `dfMask`/`dfProvince` faces derive their plane from the main
/// image or the province buffer, so they keep the main image.
fn subtree_drawn_image(layer: &crate::scene::LayerState) -> Option<u32> {
    match layer.face {
        2 | 3 => layer.bitmap,
        _ => layer.drawn_image(),
    }
}

/// The image a layer draws for its `face`: the main image (or the self image
/// that replaces it) for the ordinary faces, the alpha plane for `dfMask`
/// (`2`) and the 8-bit province plane for `dfProvince` (`3`).
fn subtree_source_image(scene: &Scene, layer: &crate::scene::LayerState) -> Option<BitmapState> {
    match layer.face {
        2 => {
            let main = layer.bitmap.and_then(|id| scene.bitmap(id))?;
            grayscale_bitmap(main.width, main.height, |x, y| {
                main.pixel_offset(x, y)
                    .map(|i| main.rgba[i + 3])
                    .unwrap_or(0)
            })
        }
        3 => {
            let province = layer.province.as_deref()?;
            let width = layer.province_width;
            let height = layer.province_height;
            grayscale_bitmap(width, height, |x, y| {
                province
                    .get(y as usize * width as usize + x as usize)
                    .copied()
                    .unwrap_or(0)
            })
        }
        _ => subtree_drawn_image(layer)
            .and_then(|id| scene.bitmap(id))
            .cloned(),
    }
}

/// Resolve a layer's drawn region in the cache: the source region in bitmap
/// pixels and the destination rectangle. Honors the
/// `imageLeft`/`imageTop`/`imageWidth`/`imageHeight` layout transform, the
/// layer rect and `ClipRect`. `dw`/`dh` differ from the source extent only
/// when the layer scales its image.
fn subtree_placement(
    layer: &crate::scene::LayerState,
    rel_x: i32,
    rel_y: i32,
    bmp_w: u32,
    bmp_h: u32,
    w: u32,
    h: u32,
) -> Option<(RectI, RectI)> {
    let img_w = if layer.image_width > 0 {
        layer.image_width
    } else {
        bmp_w
    };
    let img_h = if layer.image_height > 0 {
        layer.image_height
    } else {
        bmp_h
    };
    if img_w == 0 || img_h == 0 {
        return None;
    }
    let img_x = rel_x.saturating_add(layer.image_left);
    let img_y = rel_y.saturating_add(layer.image_top);
    let img_x1 = img_x.saturating_add(img_w as i32);
    let img_y1 = img_y.saturating_add(img_h as i32);
    let rect_x1 = rel_x.saturating_add(layer.rect.w as i32);
    let rect_y1 = rel_y.saturating_add(layer.rect.h as i32);
    let (clx0, cly0, clx1, cly1) = clip_rect(layer.clip);
    let dx0 = img_x.saturating_add(clx0).max(img_x).max(rel_x).max(0);
    let dy0 = img_y.saturating_add(cly0).max(img_y).max(rel_y).max(0);
    let dx1 = img_x
        .saturating_add(clx1)
        .min(img_x1)
        .min(rect_x1)
        .min(w as i32);
    let dy1 = img_y
        .saturating_add(cly1)
        .min(img_y1)
        .min(rect_y1)
        .min(h as i32);
    if dx1 <= dx0 || dy1 <= dy0 {
        return None;
    }
    let scale_x = f64::from(bmp_w) / f64::from(img_w);
    let scale_y = f64::from(bmp_h) / f64::from(img_h);
    let sx0 = ((f64::from(dx0 - img_x) * scale_x).floor().max(0.0)) as i32;
    let sy0 = ((f64::from(dy0 - img_y) * scale_y).floor().max(0.0)) as i32;
    let sx1 = ((f64::from(dx1 - img_x) * scale_x)
        .ceil()
        .min(f64::from(bmp_w))) as i32;
    let sy1 = ((f64::from(dy1 - img_y) * scale_y)
        .ceil()
        .min(f64::from(bmp_h))) as i32;
    if sx1 <= sx0 || sy1 <= sy0 {
        return None;
    }
    Some(((sx0, sy0, sx1, sy1), (dx0, dy0, dx1, dy1)))
}

/// Resolve the back-to-front draws for `root` and its subtree, relative to
/// `root`'s origin, using the same `imageLeft`/`imageTop`/`imageWidth`/
/// `imageHeight` placement the render projection applies.
fn collect_subtree_draws(scene: &Scene, root: u32, w: u32, h: u32) -> Vec<SubtreeDraw> {
    let Some(root_layer) = scene.layer(root) else {
        return Vec::new();
    };
    let window = root_layer.window;
    let Some((root_ax, root_ay)) = scene.absolute_offset(root) else {
        return Vec::new();
    };
    let mut draws = Vec::new();
    for id in scene.window_layer_order(window) {
        if id != root && !is_descendant(scene, id, root) {
            continue;
        }
        let Some(layer) = scene.layer(id) else {
            continue;
        };
        // The subtree root is rendered even when it is itself invisible: it is
        // drawn without a visibility check. Every other layer is skipped when
        // it is invisible or fully transparent.
        if id != root && !(layer.visible && layer.opacity != 0.0) {
            continue;
        }
        let opacity = subtree_opacity(scene, id, root);
        let opa = (opacity * 255.0).round().clamp(0.0, 255.0) as u8;
        if opa == 0 && id != root {
            continue;
        }
        let (ax, ay) = scene.absolute_offset(id).unwrap_or((0, 0));
        let rel_x = ax.saturating_sub(root_ax);
        let rel_y = ay.saturating_sub(root_ay);
        let Some(source) = subtree_source_image(scene, layer) else {
            continue;
        };
        if source.width == 0 || source.height == 0 {
            continue;
        }
        let Some((src, dest)) =
            subtree_placement(layer, rel_x, rel_y, source.width, source.height, w, h)
        else {
            continue;
        };
        draws.push(SubtreeDraw {
            source,
            dx: dest.0,
            dy: dest.1,
            dw: dest.2 - dest.0,
            dh: dest.3 - dest.1,
            src,
            mode: blend_operation_mode(layer.blend),
            opa,
            hold_alpha: layer.hold_alpha,
        });
    }
    draws
}

/// Render `root`'s own image and the visible layers of its subtree into the
/// layer's composited content. Returns the bitmap to use as the layer's
/// rendered content, reusing it while the subtree has no pending recalc:
///
/// * the layer's main image when the subtree is trivial: no visible direct
///   child, the image at the origin, and the image size equal to the layer
///   rect;
/// * otherwise the composited content bitmap, rebuilt only when the subtree
///   changed.
///
/// A layer with no main image is not a special case: the fill is transparent
/// wherever no child drew, which the transparent clear reproduces. Returns
/// `None` for an unknown layer or a content bitmap that cannot be allocated.
///
/// The content is a plain `BitmapState` in the scene: there is no hidden
/// registry, and the caller owns its lifetime through
/// [`Scene::inc_cache_enabled_count`] / [`Scene::dec_cache_enabled_count`].
pub fn render_layer_subtree(scene: &mut Scene, root: u32) -> Option<u32> {
    let (bitmap, w, h, trivial) = {
        let layer = scene.layer(root)?;
        let w = layer.rect.w.max(1);
        let h = layer.rect.h.max(1);
        let main_image_matches_rect = layer
            .bitmap
            .and_then(|id| scene.bitmap(id))
            .is_some_and(|b| b.width == layer.rect.w && b.height == layer.rect.h);
        let trivial = !has_visible_child(scene, layer)
            && layer.image_left == 0
            && layer.image_top == 0
            && main_image_matches_rect;
        (layer.bitmap, w, h, trivial)
    };
    if trivial {
        // The trivial path returns the main image, so any content composited
        // for an earlier, non-trivial frame is stale.
        release_subtree_caches(scene, root);
        return bitmap;
    }
    let content = ensure_layer_content(scene, root, w, h)?;
    let current_generation = subtree_source_generation(scene, root);
    let (recalc, recorded) = {
        let layer = scene.layer(root)?;
        (layer.cache_recalc, layer.cache_source_generation)
    };
    if recalc || current_generation != recorded {
        let draws = collect_subtree_draws(scene, root, w, h);
        let target = scene.bitmap_mut(content)?;
        target.rgba.fill(0);
        let full = (i32::MIN, i32::MIN, i32::MAX, i32::MAX);
        for draw in &draws {
            let src = &draw.source;
            if draw.dw == src.width as i32 && draw.dh == src.height as i32 {
                match draw.mode {
                    0 => {}
                    1 if draw.opa == 255 => {
                        blit_copy_clipped(target, draw.dx, draw.dy, src, draw.src, full);
                    }
                    1 | 2 | 13 => blit_over(
                        target,
                        draw.dx,
                        draw.dy,
                        BlitSource {
                            bitmap: src,
                            rect: draw.src,
                        },
                        full,
                        BlitOptions {
                            mode: 0,
                            opa: draw.opa,
                            treat_as_opaque: false,
                            hold_alpha: draw.hold_alpha,
                        },
                    ),
                    mode => operate_rect(
                        target,
                        draw.dx,
                        draw.dy,
                        BlitSource {
                            bitmap: src,
                            rect: draw.src,
                        },
                        BlitOptions {
                            mode,
                            opa: draw.opa,
                            treat_as_opaque: false,
                            hold_alpha: draw.hold_alpha,
                        },
                    ),
                }
            } else {
                stretch_blit_mode(
                    target,
                    (
                        draw.dx,
                        draw.dy,
                        draw.dx.saturating_add(draw.dw),
                        draw.dy.saturating_add(draw.dh),
                    ),
                    BlitSource {
                        bitmap: src,
                        rect: draw.src,
                    },
                    1, // stFastLinear
                    BlitOptions {
                        mode: draw.mode,
                        opa: draw.opa,
                        treat_as_opaque: false,
                        hold_alpha: draw.hold_alpha,
                    },
                );
            }
        }
        target.mark_dirty();
        if let Some(layer) = scene.layer_mut_untracked(root) {
            layer.cache_recalc = false;
            layer.cache_source_generation = current_generation;
        }
    }
    Some(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A source bitmap whose texels carry their own coordinates, with one
    /// half-transparent texel.
    fn source_texture(width: u32, height: u32) -> BitmapState {
        let mut rgba = Vec::with_capacity((width * height * 4) as usize);
        for y in 0..height {
            for x in 0..width {
                let alpha = if x == 1 && y == 1 { 128 } else { 255 };
                rgba.extend_from_slice(&[
                    (x * 37 % 251) as u8,
                    (y * 53 % 241) as u8,
                    ((x + y) * 29 % 239) as u8,
                    alpha,
                ]);
            }
        }
        BitmapState::new(width, height, rgba)
    }

    /// The generic (per-pixel) pass over the same mapping the scanline pass
    /// walks, so the two can be compared pixel for pixel.
    fn generic_pass(
        dst: &mut BitmapState,
        corners: [(f64, f64); 3],
        src: &BitmapState,
        mode: i64,
        opa: u8,
    ) {
        let m = affine_forward(corners[0], corners[1], corners[2]);
        let det = m[1] * m[5] - m[2] * m[4];
        let inverse = [m[5] / det, -m[2] / det, -m[4] / det, m[1] / det];
        let src_rect = (0, 0, src.width as i32, src.height as i32);
        let Some(dest_rect) = intersect(dst, affine_dest_bbox(m)) else {
            return;
        };
        affine_blit_generic(
            dst,
            AffinePass {
                map: m,
                inverse,
                source: src,
                source_rect: src_rect,
                dest_rect,
                stretch_type: 0,
                mode,
                opa,
            },
        );
    }

    /// The nearest resampler's scanline pass and the generic per-pixel pass
    /// must sample the same texel, so the layer a script reads back is the
    /// same whichever pass ran.
    ///
    /// The motion player asks the reference resampler for `stNearest`
    /// (`reference/cpp/plugins/motionplayer/PlayerRenderExecute.cpp:455-456`),
    /// and the reference's affine blitter walks the quad with 16.16
    /// fixed-point scanline steps
    /// (`reference/cpp/core/visual/LayerBitmapIntf.cpp:2884-2930`).
    #[test]
    fn the_nearest_scanline_pass_matches_the_generic_pass() {
        let src = source_texture(4, 4);
        // Integer, fractional, scaled and clipped axis-aligned placements.
        let placements: [[(f64, f64); 3]; 6] = [
            [(0.0, 0.0), (4.0, 0.0), (0.0, 4.0)],
            [(1.5, 2.25), (5.5, 2.25), (1.5, 6.25)],
            [(2.0, 2.0), (6.0, 2.0), (2.0, 6.0)],
            [(-2.0, -1.0), (2.0, -1.0), (-2.0, 3.0)],
            [(0.5, 0.5), (4.5, 0.5), (0.5, 4.5)],
            [(3.0, 3.0), (9.0, 3.0), (3.0, 9.0)],
        ];
        for (mode, opa) in [(2, 255), (2, 128), (1, 255)] {
            for corners in placements {
                let map = affine_forward(corners[0], corners[1], corners[2]);
                assert!(
                    affine_nearest_axis_aligned(map),
                    "the placement must take the scanline pass: {corners:?}"
                );
                let mut scanline = BitmapState::new(16, 16, vec![0u8; 16 * 16 * 4]);
                let mut generic = BitmapState::new(16, 16, vec![0u8; 16 * 16 * 4]);
                affine_blit(
                    &mut scanline,
                    AffineBlit {
                        corners,
                        stretch_type: 0,
                        clear: false,
                        clear_color: [0, 0, 0, 0],
                    },
                    BlitSource {
                        bitmap: &src,
                        rect: (0, 0, src.width as i32, src.height as i32),
                    },
                    BlitOptions {
                        mode,
                        opa,
                        treat_as_opaque: false,
                        hold_alpha: false,
                    },
                );
                generic_pass(&mut generic, corners, &src, mode, opa);
                assert_eq!(
                    scanline.rgba, generic.rgba,
                    "the scanline pass must match the generic pass: {corners:?}"
                );
            }
        }
    }

    /// A pass over many rows is split into bands and run in parallel; the
    /// result must not depend on the split.
    #[test]
    fn the_nearest_scanline_pass_splits_long_passes() {
        let src = source_texture(64, 64);
        let corners = [(0.0, 0.0), (192.0, 0.0), (0.0, 192.0)];
        let mut scanline = BitmapState::new(192, 192, vec![0u8; 192 * 192 * 4]);
        let mut generic = BitmapState::new(192, 192, vec![0u8; 192 * 192 * 4]);
        affine_blit(
            &mut scanline,
            AffineBlit {
                corners,
                stretch_type: 0,
                clear: false,
                clear_color: [0, 0, 0, 0],
            },
            BlitSource {
                bitmap: &src,
                rect: (0, 0, src.width as i32, src.height as i32),
            },
            BlitOptions {
                mode: 2,
                opa: 255,
                treat_as_opaque: false,
                hold_alpha: false,
            },
        );
        generic_pass(&mut generic, corners, &src, 2, 255);
        assert_eq!(scanline.rgba, generic.rgba);
        assert!(scanline.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
    }
}
