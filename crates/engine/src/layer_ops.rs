//! In-place CPU pixel operations on the layer system's [`BitmapState`]s.
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
//!
//! `colorize` / `noise` operate on the layer's current clip rectangle (the
//! effect rebases its buffer on the clip), so the callers pass the
//! already-clipped region.

use api::blend::{BlendMode, DstBlend};
use api::geometry::PixelRegion;

use crate::hasher::Map;
use crate::raster::blend_pixel_at;
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

/// Whether a bitmap's buffer holds every pixel its declared size names.
///
/// A bitmap can carry a size with no pixels behind it: a decode that has not
/// landed keeps the probed size and an empty buffer, and a blank image that
/// could not be allocated leaves the same state. The pixel primitives treat
/// such a bitmap as empty: they read the miss value and write nothing, so a
/// declared size can never index past the buffer.
fn pixels_present(bitmap: &BitmapState) -> bool {
    (bitmap.width as usize)
        .checked_mul(bitmap.height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .is_some_and(|bytes| bytes <= bitmap.rgba.len())
}

/// Whether `bitmap` may take part in `operation`, reporting a declared size
/// its buffer does not cover.
///
/// The report is issued once for the operation, not once for every pixel it
/// would have touched.
fn pixels_usable(bitmap: &BitmapState, operation: &str) -> bool {
    if pixels_present(bitmap) {
        return true;
    }
    log::error!(
        "engine: {operation} met a {}x{} bitmap holding {} pixel bytes; the operation is skipped",
        bitmap.width,
        bitmap.height,
        bitmap.rgba.len()
    );
    false
}

fn write_pixel(bitmap: &mut BitmapState, x: i32, y: i32, rgba: [u8; 4]) {
    if let Some(i) = bitmap.pixel_offset(x as u32, y as u32)
        && let Some(pixel) = bitmap.rgba.get_mut(i..i + 4)
    {
        pixel.copy_from_slice(&rgba);
    }
}

/// The pixel at `(x, y)`, or the transparent miss value when the bitmap holds
/// no pixel there.
fn read_pixel(bitmap: &BitmapState, x: i32, y: i32) -> [u8; 4] {
    let Some(i) = bitmap.pixel_offset(x as u32, y as u32) else {
        return [0, 0, 0, 0];
    };
    match bitmap.rgba.get(i..i + 4) {
        Some(pixel) => [pixel[0], pixel[1], pixel[2], pixel[3]],
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
    if !pixels_usable(bitmap, "fillColorOnAlpha") {
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
    if !pixels_usable(bitmap, "fillColorOnAddAlpha") {
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
    if !pixels_usable(bitmap, "fillColor") {
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
    if !pixels_usable(bitmap, "fillMask") {
        return;
    }
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
    if !pixels_usable(bitmap, "removeConstOpacity") {
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
    if !pixels_usable(bitmap, "colorize") {
        return;
    }
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
    if !pixels_usable(bitmap, "noise") {
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
    if !pixels_usable(dst, "copyRect") || !pixels_usable(src, "copyRect") {
        return;
    }
    // A rectangle whose source rows are all in bounds is a row copy, not a
    // per-pixel read/write walk. The bytes are identical; the row copy is the
    // path the full-screen UI copies take.
    if copy_rows_within(dst, dest_clip, src, src_rect, dx, dy) {
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
    // The pass wrote inside the clipped destination rectangle only, so the
    // upload needs that rectangle and not the whole bitmap.
    dst.mark_dirty_rect(clipped_region(dest_clip));
}

/// Copy the destination rectangle from the matching source rows when every
/// source row is inside the source bitmap. Returns whether the row copy ran; a
/// partial source row keeps the per-pixel path that skips out-of-bounds texels.
fn copy_rows_within(
    dst: &mut BitmapState,
    dest: RectI,
    src: &BitmapState,
    src_rect: RectI,
    dx: i32,
    dy: i32,
) -> bool {
    let width = i64::from(dest.2) - i64::from(dest.0);
    let rows = i64::from(dest.3) - i64::from(dest.1);
    let src_left = i64::from(src_rect.0) + i64::from(dest.0) - i64::from(dx);
    let src_top = i64::from(src_rect.1) + i64::from(dest.1) - i64::from(dy);
    if width <= 0
        || rows <= 0
        || src_left < 0
        || src_top < 0
        || src_left + width > i64::from(src.width)
        || src_top + rows > i64::from(src.height)
    {
        return false;
    }
    let (Ok(width), Ok(rows), Ok(src_left), Ok(src_top)) = (
        usize::try_from(width),
        usize::try_from(rows),
        usize::try_from(src_left),
        usize::try_from(src_top),
    ) else {
        return false;
    };
    let (Some(row_bytes), Some(src_pitch), Some(dst_pitch)) = (
        width.checked_mul(4),
        usize::try_from(src.width)
            .ok()
            .and_then(|w| w.checked_mul(4)),
        usize::try_from(dst.width)
            .ok()
            .and_then(|w| w.checked_mul(4)),
    ) else {
        return false;
    };
    let Some(src_x) = src_left.checked_mul(4) else {
        return false;
    };
    let Some(dst_x) = usize::try_from(dest.0).ok().and_then(|x| x.checked_mul(4)) else {
        return false;
    };
    let Ok(dst_y) = usize::try_from(dest.1) else {
        return false;
    };
    for row in 0..rows {
        let Some(source_start) = (src_top + row)
            .checked_mul(src_pitch)
            .and_then(|start| start.checked_add(src_x))
        else {
            return false;
        };
        let Some(source_end) = source_start.checked_add(row_bytes) else {
            return false;
        };
        let Some(dest_start) = (dst_y + row)
            .checked_mul(dst_pitch)
            .and_then(|start| start.checked_add(dst_x))
        else {
            return false;
        };
        let Some(dest_end) = dest_start.checked_add(row_bytes) else {
            return false;
        };
        let (Some(source), Some(target)) = (
            src.rgba.get(source_start..source_end),
            dst.rgba.get_mut(dest_start..dest_end),
        ) else {
            return false;
        };
        target.copy_from_slice(source);
    }
    dst.mark_dirty_rect(clipped_region(dest));
    true
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
    let BlitOptions { opa, .. } = options;
    let sw = src_rect.2.saturating_sub(src_rect.0);
    let sh = src_rect.3.saturating_sub(src_rect.1);
    if sw <= 0 || sh <= 0 || opa == 0 || src.width == 0 || src.height == 0 {
        return;
    }
    if !pixels_usable(dst, "blt") || !pixels_usable(src, "blt") {
        return;
    }
    let Some(in_bounds) = intersect(dst, (dx, dy, dx.saturating_add(sw), dy.saturating_add(sh)))
    else {
        return;
    };
    let Some(dc) = intersect_rect(in_bounds, clip) else {
        return;
    };
    let rows = (dc.3 - dc.1) as usize;
    let columns = (dc.2 - dc.0) as usize;
    let threads = affine_thread_count(rows, columns);
    let row_bytes = match usize::try_from(dst.width)
        .ok()
        .and_then(|w| w.checked_mul(4))
    {
        Some(width) => width,
        None => return,
    };
    let band_start = (dc.1 as usize).saturating_mul(row_bytes);
    let band_len = rows.saturating_mul(row_bytes);
    let Some(band) = dst
        .rgba
        .get_mut(band_start..band_start.saturating_add(band_len))
    else {
        return;
    };
    if threads <= 1 {
        blit_over_band(
            band,
            dc.1,
            dst.width as i32,
            BlitOverArea { clip: dc, dx, dy },
            BlitSource {
                bitmap: src,
                rect: src_rect,
            },
            options,
        );
    } else {
        let rows_per_band = rows.div_ceil(threads);
        let chunk_bytes = rows_per_band.saturating_mul(row_bytes);
        let chunks: Vec<&mut [u8]> = band.chunks_mut(chunk_bytes).collect();
        let dst_width = dst.width as i32;
        std::thread::scope(|scope| {
            for (index, chunk) in chunks.into_iter().enumerate() {
                let first_row = dc.1 + (index * rows_per_band) as i32;
                scope.spawn(move || {
                    blit_over_band(
                        chunk,
                        first_row,
                        dst_width,
                        BlitOverArea { clip: dc, dx, dy },
                        BlitSource {
                            bitmap: src,
                            rect: src_rect,
                        },
                        options,
                    );
                });
            }
        });
    }
    // The pass wrote inside the clipped destination rectangle only, so the
    // upload needs that rectangle and not the whole bitmap.
    dst.mark_dirty_rect(clipped_region(dc));
}

/// The destination window of one [`blit_over`] call: the clipped rectangle
/// plus the source placement.
#[derive(Clone, Copy)]
struct BlitOverArea {
    /// Clipped destination rectangle the pass writes inside.
    clip: RectI,
    /// Source origin on the destination.
    dx: i32,
    /// Source origin on the destination.
    dy: i32,
}

/// One contiguous band of destination rows for [`blit_over`].
///
/// A band owns whole rows, so the pixel loop can address its destination row
/// directly instead of resolving a scene offset per pixel. The parallel pass
/// splits the clipped rows into bands and runs this function per band.
fn blit_over_band(
    band: &mut [u8],
    first_row: i32,
    dst_width: i32,
    area: BlitOverArea,
    source: BlitSource<'_>,
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
    let (x0, x1) = (area.clip.0, area.clip.2);
    let (dx, dy) = (area.dx, area.dy);
    let (Ok(dst_width), Ok(src_width), Ok(src_height)) = (
        usize::try_from(dst_width),
        usize::try_from(src.width),
        usize::try_from(src.height),
    ) else {
        return;
    };
    let (Some(dst_pitch), Some(src_pitch)) = (dst_width.checked_mul(4), src_width.checked_mul(4))
    else {
        return;
    };
    for (row_index, row) in band.chunks_mut(dst_pitch).enumerate() {
        let Ok(row_index) = i32::try_from(row_index) else {
            break;
        };
        let y = first_row.saturating_add(row_index);
        let sy = src_rect.1.saturating_add(y.saturating_sub(dy));
        let Ok(sy) = usize::try_from(sy) else {
            continue;
        };
        if sy >= src_height {
            continue;
        }
        #[cfg(feature = "simd-experiment")]
        if opa == 255 && !treat_as_opaque && !hold_alpha {
            let columns = usize::try_from(x1.saturating_sub(x0)).ok();
            let source_x = usize::try_from(src_rect.0.saturating_add(x0.saturating_sub(dx))).ok();
            let dest_x = usize::try_from(x0).ok();
            if let (Some(columns), Some(source_x), Some(dest_x)) = (columns, source_x, dest_x)
                && source_x.saturating_add(columns) <= src_width
                && let (Some(source_start), Some(dest_start), Some(len)) = (
                    sy.checked_mul(src_pitch)
                        .and_then(|row| row.checked_add(source_x.checked_mul(4)?)),
                    dest_x.checked_mul(4),
                    columns.checked_mul(4),
                )
                && let (Some(source), Some(destination)) = (
                    src.rgba.get(source_start..source_start.saturating_add(len)),
                    row.get_mut(dest_start..dest_start.saturating_add(len)),
                )
                && crate::simd::copy_opaque_row(source, destination)
            {
                continue;
            }
        }
        for x in x0..x1 {
            let sx = src_rect.0.saturating_add(x.saturating_sub(dx));
            if sx < 0 || usize::try_from(sx).map_or(true, |sx| sx >= src_width) {
                continue;
            }
            let (Ok(sx), Ok(x)) = (usize::try_from(sx), usize::try_from(x)) else {
                continue;
            };
            let Some(source_start) = sy
                .checked_mul(src_pitch)
                .and_then(|row| row.checked_add(sx.checked_mul(4)?))
            else {
                continue;
            };
            let Some(source) = src.rgba.get(source_start..source_start.saturating_add(4)) else {
                continue;
            };
            let mut color = [source[0], source[1], source[2], source[3]];
            if treat_as_opaque {
                // `bmCopyOnAlpha`/`bmAlpha` treat the source as a fully
                // opaque image; the constant `opa` supplies the coverage.
                color[3] = 255;
            }
            let Some(dest_start) = x.checked_mul(4) else {
                continue;
            };
            let Some(destination) = row.get(dest_start..dest_start.saturating_add(4)) else {
                continue;
            };
            // `holdAlpha` suppresses the write to the destination alpha
            // channel entirely; capture it first so the composite math still
            // sees the real destination alpha.
            let old_alpha = destination[3];
            let mut pixel = [
                destination[0],
                destination[1],
                destination[2],
                destination[3],
            ];
            blend_pixel_at(&mut pixel, color, 255, opa);
            if hold_alpha {
                pixel[3] = old_alpha;
            }
            if let Some(destination) = row.get_mut(dest_start..dest_start.saturating_add(4)) {
                destination.copy_from_slice(&pixel);
            }
        }
    }
}

/// Copies the rectangle `dc` row by row when every destination row maps to
/// one contiguous source run. Returns false when the mapping needs the
/// per-pixel bounds checks, leaving the destination untouched for the caller's
/// scalar pass.
fn copy_plane_rows(
    dst: &mut BitmapState,
    dx: i32,
    dy: i32,
    src: &BitmapState,
    src_rect: RectI,
    dc: RectI,
) -> bool {
    let (Some(columns), Some(rows)) = (
        usize::try_from(dc.2 - dc.0).ok(),
        usize::try_from(dc.3 - dc.1).ok(),
    ) else {
        return false;
    };
    if columns == 0 || rows == 0 {
        return false;
    }
    let (Some(offset_x), Some(offset_y)) = (dc.0.checked_sub(dx), dc.1.checked_sub(dy)) else {
        return false;
    };
    let (Some(src_x), Some(src_y)) = (
        src_rect.0.checked_add(offset_x),
        src_rect.1.checked_add(offset_y),
    ) else {
        return false;
    };
    if src_x < 0 || src_y < 0 {
        return false;
    }
    let (Ok(src_x), Ok(src_y), Ok(src_w), Ok(src_h), Ok(dst_w)) = (
        usize::try_from(src_x),
        usize::try_from(src_y),
        usize::try_from(src.width),
        usize::try_from(src.height),
        usize::try_from(dst.width),
    ) else {
        return false;
    };
    let (Some(end_x), Some(end_y)) = (src_x.checked_add(columns), src_y.checked_add(rows)) else {
        return false;
    };
    if end_x > src_w || end_y > src_h {
        return false;
    }
    let (Some(src_pitch), Some(dst_pitch), Some(row_bytes)) = (
        src_w.checked_mul(4),
        dst_w.checked_mul(4),
        columns.checked_mul(4),
    ) else {
        return false;
    };
    let (Ok(dst_x), Ok(dst_y)) = (usize::try_from(dc.0), usize::try_from(dc.1)) else {
        return false;
    };
    let (Some(src_x_bytes), Some(dst_x_bytes)) = (src_x.checked_mul(4), dst_x.checked_mul(4))
    else {
        return false;
    };
    // Reject a short buffer before writing any row, so a failure leaves the
    // destination to the caller's scalar pass unchanged.
    let (Some(last_src_start), Some(last_dst_start)) = (
        src_y
            .checked_add(rows - 1)
            .and_then(|y| y.checked_mul(src_pitch))
            .and_then(|start| start.checked_add(src_x_bytes)),
        dst_y
            .checked_add(rows - 1)
            .and_then(|y| y.checked_mul(dst_pitch))
            .and_then(|start| start.checked_add(dst_x_bytes)),
    ) else {
        return false;
    };
    let (Some(last_src_end), Some(last_dst_end)) = (
        last_src_start.checked_add(row_bytes),
        last_dst_start.checked_add(row_bytes),
    ) else {
        return false;
    };
    if last_src_end > src.rgba.len() || last_dst_end > dst.rgba.len() {
        return false;
    }
    for row in 0..rows {
        let (Some(src_start), Some(dst_start)) = (
            src_y
                .checked_add(row)
                .and_then(|y| y.checked_mul(src_pitch))
                .and_then(|start| start.checked_add(src_x_bytes)),
            dst_y
                .checked_add(row)
                .and_then(|y| y.checked_mul(dst_pitch))
                .and_then(|start| start.checked_add(dst_x_bytes)),
        ) else {
            return false;
        };
        let (Some(src_end), Some(dst_end)) = (
            src_start.checked_add(row_bytes),
            dst_start.checked_add(row_bytes),
        ) else {
            return false;
        };
        let (Some(src_row), Some(dst_row)) = (
            src.rgba.get(src_start..src_end),
            dst.rgba.get_mut(dst_start..dst_end),
        ) else {
            return false;
        };
        dst_row.copy_from_slice(src_row);
    }
    true
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
    if !pixels_usable(dst, "copyRect") || !pixels_usable(src, "copyRect") {
        return;
    }
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
        // The common copy is a whole rectangle whose rows map one to one;
        // copy each row as a run instead of resolving every pixel.
        if !copy_plane_rows(dst, dx, dy, src, src_rect, dc) {
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
        }
        // The pass wrote inside the clipped destination rectangle only, so the
        // upload needs that rectangle and not the whole bitmap.
        dst.mark_dirty_rect(clipped_region(dc));
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
    // The pass wrote inside the clipped destination rectangle only, so the
    // upload needs that rectangle and not the whole bitmap.
    dst.mark_dirty_rect(clipped_region(dc));
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
    if !pixels_usable(dst, "tileRect") || !pixels_usable(tile, "tileRect") {
        return;
    }
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

/// The horizontal box sum for one row: `out[x]` receives the sum of
/// `rgba[y][max(0, x-rx) ..= min(w-1, x+rx)]` (four channels). Uses a sliding
/// running sum, so the row costs `O(w)`.
fn horizontal_box_sum(rgba: &[u8], w: usize, y: usize, rx: usize, out: &mut [[u64; 4]]) {
    let Some(row_bytes) = w.checked_mul(4) else {
        return;
    };
    let Some(base) = y.checked_mul(row_bytes) else {
        return;
    };
    let Some(src) = base
        .checked_add(row_bytes)
        .and_then(|end| rgba.get(base..end))
    else {
        return;
    };
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
    if !pixels_usable(bitmap, "blurBitmapInPlace") {
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
        (0, 0, dim_i32(bitmap.width), dim_i32(bitmap.height))
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
        (0, 0, dim_i32(bitmap.width), dim_i32(bitmap.height))
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
    if !pixels_usable(dst, "stretchBlt") || !pixels_usable(src, "stretchBlt") {
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
    // The pass wrote inside the clipped destination rectangle only, so the
    // upload needs that rectangle and not the whole bitmap.
    dst.mark_dirty_rect(clipped_region(dc));
}

/// Grey `(19*R + 183*G + 54*B) >> 8` written to all three colour channels,
/// the alpha held.
pub fn do_gray_scale(bitmap: &mut BitmapState, rect: RectI) {
    if !pixels_usable(bitmap, "doGrayScale") {
        return;
    }
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
    if !pixels_usable(bitmap, "adjustGamma") {
        return;
    }
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
    if !pixels_usable(bitmap, "lightContrast") {
        return;
    }
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
    if !pixels_usable(bitmap, "convertAlphaToAddAlpha") {
        return;
    }
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
    if !pixels_usable(bitmap, "convertAddAlphaToAlpha") {
        return;
    }
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
    if !pixels_usable(dst, "copy9Patch") || !pixels_usable(src, "copy9Patch") {
        return None;
    }
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
    if !pixels_usable(bitmap, "flipLR") {
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
    if !pixels_usable(bitmap, "flipUD") {
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
    if !pixels_usable(bitmap, "gaussianBlur") {
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

/// Blend one source pixel onto `dst` with an already resolved kernel.
///
/// A pass resolves its kernel once and hands it to every pixel, so the
/// per-pixel loop never dispatches on the operation mode.
pub(crate) fn blend_pixel_mode(
    dst: &mut BitmapState,
    x: i32,
    y: i32,
    src: [u8; 4],
    opa: u8,
    kernel: ModeKernel,
) {
    if x < 0 || y < 0 || x as u32 >= dst.width || y as u32 >= dst.height {
        return;
    }
    let mut pixel = read_pixel(dst, x, y);
    apply_mode_kernel(kernel, &mut pixel, src, opa);
    write_pixel(dst, x, y, pixel);
}

/// The per-pixel core of [`blend_pixel_mode`]: blend `src` into one
/// destination pixel, in place.
///
/// The kernel is factored out of the bitmap accessor so a pass that owns a
/// destination row slice (the affine nearest pass) blends with the same
/// arithmetic.
fn blend_pixel_mode_at(pixel: &mut [u8; 4], src: [u8; 4], opa: u8, kernel: ModeKernel) {
    apply_mode_kernel(kernel, pixel, src, opa);
}

/// The source pixel a pass blends, with the alpha the pass asks for.
///
/// A pass that treats the source as an opaque image reads the source's colour
/// with an opaque alpha.
fn opaque_source(color: [u8; 4], treat_as_opaque: bool) -> [u8; 4] {
    if treat_as_opaque {
        [color[0], color[1], color[2], 255]
    } else {
        color
    }
}

/// The per-channel operation a numeric blend mode applies.
///
/// The mode numbers are the operation-mode numbers the script reads and
/// writes. Every mode the engine knows has a kernel of its own.
#[derive(Clone, Copy)]
pub(crate) enum ModeKernel {
    /// The source replaces the destination, including its alpha.
    Copy,
    /// The constant-alpha blend: the opacity mixes the source's colour into
    /// the destination's and the destination's alpha is held.
    ConstAlpha,
    /// The straight-alpha source-over; the destination's alpha is
    /// recomputed from the source's alpha.
    SourceOver,
    /// The channel mix without an alpha correction: the source's alpha
    /// mixes the colours and the destination's alpha is held.
    ChannelMix,
    /// Saturating add of the source, scaled by the opacity.
    Additive,
    /// Saturating subtract of the source, scaled by the opacity.
    Subtractive,
    /// Multiply by the source, scaled by the opacity.
    Multiplicative,
    /// The darker of the two channels.
    Darken,
    /// The lighter of the two channels.
    Lighten,
    /// Screen.
    Screen,
    /// Colour dodge: the destination scaled by the source's inverse.
    Dodge,
    /// Additive with an explicit alpha channel.
    AddAlpha,
    /// Photoshop additive.
    PsAdditive,
    /// Photoshop subtractive.
    PsSubtractive,
    /// Photoshop multiplicative.
    PsMultiplicative,
    /// Photoshop screen.
    PsScreen,
    /// Photoshop overlay.
    PsOverlay,
    /// Photoshop hard light.
    HardLight,
    /// Photoshop soft light.
    PsSoftLight,
    /// Photoshop colour dodge.
    PsColorDodge,
    /// The legacy Photoshop 5.x colour dodge.
    PsColorDodge5,
    /// Photoshop colour burn.
    PsColorBurn,
    /// Photoshop lighten.
    PsLighten,
    /// Photoshop darken.
    PsDarken,
    /// Photoshop difference.
    PsDifference,
    /// The legacy Photoshop 5.x difference.
    PsDifference5,
    /// Photoshop exclusion.
    PsExclusion,
}

/// The kernel a numeric operation mode applies.
///
/// Every operation mode the engine knows has a kernel of its own; a mode
/// it does not know has none.
fn mode_kernel(mode: i64) -> Option<ModeKernel> {
    Some(match mode {
        1 => ModeKernel::Copy,
        2 => ModeKernel::SourceOver,
        3 => ModeKernel::Additive,
        4 => ModeKernel::Subtractive,
        5 => ModeKernel::Multiplicative,
        8 => ModeKernel::Dodge,
        9 => ModeKernel::Darken,
        10 => ModeKernel::Lighten,
        11 => ModeKernel::Screen,
        12 => ModeKernel::AddAlpha,
        13 => ModeKernel::ChannelMix,
        14 => ModeKernel::PsAdditive,
        15 => ModeKernel::PsSubtractive,
        16 => ModeKernel::PsMultiplicative,
        17 => ModeKernel::PsScreen,
        18 => ModeKernel::PsOverlay,
        19 => ModeKernel::HardLight,
        20 => ModeKernel::PsSoftLight,
        21 => ModeKernel::PsColorDodge,
        22 => ModeKernel::PsColorDodge5,
        23 => ModeKernel::PsColorBurn,
        24 => ModeKernel::PsLighten,
        25 => ModeKernel::PsDarken,
        26 => ModeKernel::PsDifference,
        27 => ModeKernel::PsDifference5,
        28 => ModeKernel::PsExclusion,
        _ => return None,
    })
}

/// The kernel a pass applies.
///
/// The copy family has a second spelling below the full opacity: the source's
/// colour is blended into the destination with the opacity as a constant
/// alpha. The copy of an opaque source takes the destination's alpha into
/// that mix; the plain copy holds it.
fn resolve_kernel(mode: i64, opa: u8, treat_as_opaque: bool) -> Option<ModeKernel> {
    let kernel = mode_kernel(mode)?;
    if opa != 255 && matches!(kernel, ModeKernel::Copy) {
        return Some(if treat_as_opaque {
            ModeKernel::SourceOver
        } else {
            ModeKernel::ConstAlpha
        });
    }
    Some(kernel)
}

/// The blend one destination pixel of a pass receives.
///
/// A pass resolves its operation mode once and blends every sampled texel
/// with the resolved kernel, so the per-pixel loop does not dispatch on the
/// mode.
trait BandBlend {
    /// Blends `src` into `pixel` with the operation's kernel.
    fn blend(kernel: ModeKernel, pixel: &mut [u8; 4], src: [u8; 4], opa: u8);
}

/// The source replaces the destination.
struct ReplaceBlend;

impl BandBlend for ReplaceBlend {
    fn blend(_kernel: ModeKernel, pixel: &mut [u8; 4], src: [u8; 4], _opa: u8) {
        *pixel = src;
    }
}

/// The source is composited over the destination.
struct SourceOverBlend;

impl BandBlend for SourceOverBlend {
    fn blend(_kernel: ModeKernel, pixel: &mut [u8; 4], src: [u8; 4], opa: u8) {
        // An opaque source at full opacity replaces the destination, which
        // is the pixel the general blend computes for it.
        if opa == 255 && src[3] == 255 {
            *pixel = src;
            return;
        }
        blend_pixel_at(pixel, src, 255, opa);
    }
}

/// A numeric operation mode with its own per-channel kernel.
struct ModeBlend;

impl BandBlend for ModeBlend {
    fn blend(kernel: ModeKernel, pixel: &mut [u8; 4], src: [u8; 4], opa: u8) {
        blend_pixel_mode_at(pixel, src, opa, kernel);
    }
}

/// The blend a pass's operation mode applies.
#[derive(Clone, Copy)]
enum AffineBlend {
    /// The source replaces the destination.
    Replace,
    /// The source is composited over the destination.
    SourceOver,
    /// A numeric operation mode with its own per-channel kernel.
    Mode,
}

/// The blend an operation mode applies, resolved once per pass.
fn affine_blend(kernel: ModeKernel) -> AffineBlend {
    match kernel {
        ModeKernel::Copy => AffineBlend::Replace,
        ModeKernel::SourceOver => AffineBlend::SourceOver,
        ModeKernel::Additive
        | ModeKernel::Subtractive
        | ModeKernel::Multiplicative
        | ModeKernel::Darken
        | ModeKernel::Lighten
        | ModeKernel::Screen
        | ModeKernel::Dodge
        | ModeKernel::AddAlpha
        | ModeKernel::ChannelMix
        | ModeKernel::ConstAlpha
        | ModeKernel::PsAdditive
        | ModeKernel::PsSubtractive
        | ModeKernel::PsMultiplicative
        | ModeKernel::PsScreen
        | ModeKernel::PsOverlay
        | ModeKernel::HardLight
        | ModeKernel::PsSoftLight
        | ModeKernel::PsColorDodge
        | ModeKernel::PsColorDodge5
        | ModeKernel::PsColorBurn
        | ModeKernel::PsLighten
        | ModeKernel::PsDarken
        | ModeKernel::PsDifference
        | ModeKernel::PsDifference5
        | ModeKernel::PsExclusion => AffineBlend::Mode,
    }
}

/// The factor the Photoshop blends mix their per-channel function with.
///
/// The blend functions take the source's alpha as the factor, and the opacity
/// scales it when the opacity is not the full one.
fn ps_mix_factor(src_alpha: u8, opa: u8) -> u8 {
    if opa == 255 {
        src_alpha
    } else {
        ((u32::from(src_alpha) * u32::from(opa)) >> 8) as u8
    }
}

/// Mixes a per-channel blend function into the destination's channels and
/// holds the destination's alpha.
///
/// The Photoshop blends write the destination back with its alpha untouched
/// and mix the per-channel function with `d + ((f - d) * a >> 8)`.
fn mix_channels(pixel: &mut [u8; 4], function: [u8; 3], factor: u8) {
    for (channel, value) in pixel[..3].iter_mut().zip(function) {
        *channel = blend_channel(*channel, value, factor);
    }
}

/// The Photoshop product of two channels: `d * s >> 8`.
fn ps_mul_channel(d: u8, s: u8) -> u8 {
    ((u32::from(d) * u32::from(s)) >> 8) as u8
}

/// The Photoshop screen of two channels: `d + s - (d * s >> 8)`.
fn ps_screen_channel(d: u8, s: u8) -> u8 {
    let value = i32::from(d) + i32::from(s) - i32::from(ps_mul_channel(d, s));
    value.clamp(0, 255) as u8
}

/// The Photoshop exclusion of two channels: `d + s - (d * s >> 7)`.
fn ps_exclusion_channel(d: u8, s: u8) -> u8 {
    let product = (i32::from(d) * i32::from(s)) >> 7;
    let value = i32::from(d) + i32::from(s) - product;
    value.clamp(0, 255) as u8
}

/// The Photoshop overlay of a source channel over a destination channel.
///
/// The overlay table is `(s * d * 2) / 255` below the midpoint and
/// `(s + d) * 2 - (s * d * 2) / 255 - 255` above it.
fn ps_overlay_channel(s: u8, d: u8) -> u8 {
    let product = i32::from(s) * i32::from(d) * 2;
    let value = if d < 128 {
        product / 255
    } else {
        (i32::from(s) + i32::from(d)) * 2 - product / 255 - 255
    };
    value.clamp(0, 255) as u8
}

/// The Photoshop hard light of a source channel over a destination channel.
///
/// The hard light is the overlay with the operands swapped, so the source
/// channel decides the branch.
fn ps_hard_light_channel(s: u8, d: u8) -> u8 {
    ps_overlay_channel(d, s)
}

/// The Photoshop soft light of a source channel over a destination channel.
///
/// The soft-light table is `pow(d / 255, 128 / s)` above the midpoint and
/// `pow(d / 255, (1 - s / 255) / 0.5)` below it.
fn ps_soft_light_channel(s: u8, d: u8) -> u8 {
    let base = f64::from(d) / 255.0;
    let value = if s >= 128 {
        base.powf(128.0 / f64::from(s))
    } else {
        base.powf((1.0 - f64::from(s) / 255.0) / 0.5)
    };
    (value * 255.0) as u8
}

/// The Photoshop colour dodge of a source channel over a destination channel.
///
/// The colour-dodge table is `0xff` when the source is light enough to blow
/// the channel out and `(d * 255) / (255 - s)` otherwise.
fn ps_color_dodge_channel(s: u8, d: u8) -> u8 {
    if 255 - i32::from(s) <= i32::from(d) {
        255
    } else {
        ((i32::from(d) * 255) / (255 - i32::from(s))).clamp(0, 255) as u8
    }
}

/// The Photoshop colour burn of a source channel over a destination channel.
///
/// The colour-burn table is `0x00` when the source is dark enough and
/// `255 - ((255 - d) * 255) / s` otherwise.
fn ps_color_burn_channel(s: u8, d: u8) -> u8 {
    if i32::from(s) <= 255 - i32::from(d) {
        0
    } else {
        (255 - ((255 - i32::from(d)) * 255) / i32::from(s)).clamp(0, 255) as u8
    }
}

/// The colour dodge of the non-Photoshop mode: the destination scaled by the
/// reciprocal of the source's complement.
///
/// The destination is multiplied by a 16.16 reciprocal of `255 - s` and the
/// result is clamped to a byte.
fn dodge_channel(d: u8, s: u8) -> u8 {
    let reciprocal = if s == 255 {
        65536
    } else {
        65536 / (255 - i32::from(s))
    };
    let value = (i32::from(d) * reciprocal) >> 8;
    value.clamp(0, 255) as u8
}

/// Blends `src` into one destination pixel with the kernel of `mode`.
fn apply_mode_kernel(kernel: ModeKernel, pixel: &mut [u8; 4], src: [u8; 4], opa: u8) {
    match kernel {
        // omOpaque / ltOpaque on an opaque face: the source replaces the
        // destination, alpha included.
        ModeKernel::Copy => *pixel = src,
        // The copy family below the full opacity: the opacity mixes the
        // source's colour into the destination's and the destination's alpha
        // is held.
        ModeKernel::ConstAlpha => {
            for c in 0..3 {
                pixel[c] = blend_channel(pixel[c], src[c], opa);
            }
        }
        // The alpha-correct source-over: the destination's alpha takes part
        // in the mix.
        ModeKernel::SourceOver => blend_pixel_at(pixel, src, 255, opa),
        // The channel mix: the source's alpha mixes the colours and the
        // destination's alpha is held. The mix is `d + ((f - d) * a >> 8)`.
        ModeKernel::ChannelMix => {
            let factor = ps_mix_factor(src[3], opa);
            mix_channels(pixel, [src[0], src[1], src[2]], factor);
        }
        // omAdditive / ltAdditive: saturating add, source scaled by `opa`.
        ModeKernel::Additive => {
            for c in 0..3 {
                pixel[c] =
                    pixel[c].saturating_add(((u32::from(src[c]) * u32::from(opa)) / 255) as u8);
            }
        }
        // omSubtractive / ltSubtractive.
        ModeKernel::Subtractive => {
            for c in 0..3 {
                pixel[c] =
                    pixel[c].saturating_sub(((u32::from(src[c]) * u32::from(opa)) / 255) as u8);
            }
        }
        // omMultiplicative / ltMultiplicative.
        ModeKernel::Multiplicative => {
            let opa = u32::from(opa);
            for c in 0..3 {
                let s = u32::from(src[c]) * opa / 255;
                pixel[c] = (u32::from(pixel[c]) * s / 255) as u8;
            }
        }
        // omDarken / ltDarken.
        ModeKernel::Darken => {
            for c in 0..3 {
                pixel[c] = pixel[c].min(src[c]);
            }
        }
        // omLighten / ltLighten.
        ModeKernel::Lighten => {
            for c in 0..3 {
                pixel[c] = pixel[c].max(src[c]);
            }
        }
        // omScreen / ltScreen.
        ModeKernel::Screen => {
            for c in 0..3 {
                let blended = 255 - (255 - u32::from(pixel[c])) * (255 - u32::from(src[c])) / 255;
                pixel[c] =
                    ((u32::from(pixel[c]) * (255 - u32::from(opa)) + blended * 255) / 255) as u8;
            }
        }
        // omDodge / ltDodge: the destination scaled by the source's inverse.
        ModeKernel::Dodge => {
            for c in 0..3 {
                let faded = ((u32::from(src[c]) * u32::from(opa)) >> 8) as u8;
                pixel[c] = dodge_channel(pixel[c], faded);
            }
        }
        // omAddAlpha / ltAddAlpha: the source fades by the opacity and the
        // destination by the source's remaining alpha.
        ModeKernel::AddAlpha => {
            let remaining = 255 - ((u32::from(src[3]) * u32::from(opa)) >> 8) as u8;
            for c in 0..3 {
                let base = ((u32::from(pixel[c]) * u32::from(remaining)) >> 8) as u8;
                let faded = ((u32::from(src[c]) * u32::from(opa)) >> 8) as u8;
                pixel[c] = base.saturating_add(faded);
            }
        }
        // The Photoshop modes apply their per-channel blend function and mix it
        // with the source's alpha; the destination's alpha is held.
        // omPsAdditive / ltPsAdditive: the saturating sum.
        ModeKernel::PsAdditive => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                pixel[0].saturating_add(src[0]),
                pixel[1].saturating_add(src[1]),
                pixel[2].saturating_add(src[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // omPsSubtractive / ltPsSubtractive: the saturating difference.
        ModeKernel::PsSubtractive => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                pixel[0].saturating_sub(src[0]),
                pixel[1].saturating_sub(src[1]),
                pixel[2].saturating_sub(src[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // omPsMultiplicative / ltPsMultiplicative: the product.
        ModeKernel::PsMultiplicative => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                ps_mul_channel(pixel[0], src[0]),
                ps_mul_channel(pixel[1], src[1]),
                ps_mul_channel(pixel[2], src[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // omPsScreen / ltPsScreen: the screen.
        ModeKernel::PsScreen => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                ps_screen_channel(pixel[0], src[0]),
                ps_screen_channel(pixel[1], src[1]),
                ps_screen_channel(pixel[2], src[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // omPsOverlay / ltPsOverlay: the overlay.
        ModeKernel::PsOverlay => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                ps_overlay_channel(src[0], pixel[0]),
                ps_overlay_channel(src[1], pixel[1]),
                ps_overlay_channel(src[2], pixel[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // ltPsHardLight (19): the hard light.
        ModeKernel::HardLight => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                ps_hard_light_channel(src[0], pixel[0]),
                ps_hard_light_channel(src[1], pixel[1]),
                ps_hard_light_channel(src[2], pixel[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // ltPsSoftLight (20): the soft light.
        ModeKernel::PsSoftLight => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                ps_soft_light_channel(src[0], pixel[0]),
                ps_soft_light_channel(src[1], pixel[1]),
                ps_soft_light_channel(src[2], pixel[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // ltPsColorDodge (21): the colour dodge.
        ModeKernel::PsColorDodge => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                ps_color_dodge_channel(src[0], pixel[0]),
                ps_color_dodge_channel(src[1], pixel[1]),
                ps_color_dodge_channel(src[2], pixel[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // ltPsColorDodge5 (22): the source fades first and the colour-dodge
        // function replaces the channel; the mix does not apply.
        ModeKernel::PsColorDodge5 => {
            let factor = ps_mix_factor(src[3], opa);
            for c in 0..3 {
                let faded = ((u32::from(src[c]) * u32::from(factor)) >> 8) as u8;
                pixel[c] = ps_color_dodge_channel(faded, pixel[c]);
            }
        }
        // ltPsColorBurn (23): the colour burn.
        ModeKernel::PsColorBurn => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                ps_color_burn_channel(src[0], pixel[0]),
                ps_color_burn_channel(src[1], pixel[1]),
                ps_color_burn_channel(src[2], pixel[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // ltPsLighten (24): the lighter channel.
        ModeKernel::PsLighten => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                pixel[0].max(src[0]),
                pixel[1].max(src[1]),
                pixel[2].max(src[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // ltPsDarken (25): the darker channel.
        ModeKernel::PsDarken => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                pixel[0].min(src[0]),
                pixel[1].min(src[1]),
                pixel[2].min(src[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // ltPsDifference (26): the absolute difference.
        ModeKernel::PsDifference => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                src[0].abs_diff(pixel[0]),
                src[1].abs_diff(pixel[1]),
                src[2].abs_diff(pixel[2]),
            ];
            mix_channels(pixel, function, factor);
        }
        // ltPsDifference5 (27): the source fades first and the difference
        // replaces the channel; the mix does not apply.
        ModeKernel::PsDifference5 => {
            let factor = ps_mix_factor(src[3], opa);
            for c in 0..3 {
                let faded = ((u32::from(src[c]) * u32::from(factor)) >> 8) as u8;
                pixel[c] = faded.abs_diff(pixel[c]);
            }
        }
        // ltPsExclusion (28): the exclusion.
        ModeKernel::PsExclusion => {
            let factor = ps_mix_factor(src[3], opa);
            let function = [
                ps_exclusion_channel(pixel[0], src[0]),
                ps_exclusion_channel(pixel[1], src[1]),
                ps_exclusion_channel(pixel[2], src[2]),
            ];
            mix_channels(pixel, function, factor);
        }
    }
}

#[cfg(feature = "simd-experiment")]
struct SourceOverRowSlices {
    height: usize,
    src_stride: usize,
    dst_stride: usize,
    src_base: usize,
    dst_base: usize,
    row_bytes: usize,
}

#[cfg(feature = "simd-experiment")]
fn source_over_row_slices(
    dst: &BitmapState,
    src: &BitmapState,
    dc: RectI,
    dx: i32,
    dy: i32,
    src_rect: RectI,
) -> Option<SourceOverRowSlices> {
    let width = i64::from(dc.2) - i64::from(dc.0);
    let height = i64::from(dc.3) - i64::from(dc.1);
    let src_x = i64::from(src_rect.0) + i64::from(dc.0) - i64::from(dx);
    let src_y = i64::from(src_rect.1) + i64::from(dc.1) - i64::from(dy);
    if src_x < 0
        || src_y < 0
        || src_x.checked_add(width)? > i64::from(src.width)
        || src_y.checked_add(height)? > i64::from(src.height)
    {
        return None;
    }
    let width = usize::try_from(width).ok()?;
    let height = usize::try_from(height).ok()?;
    let src_x = usize::try_from(src_x).ok()?;
    let src_y = usize::try_from(src_y).ok()?;
    let dc_x = usize::try_from(dc.0).ok()?;
    let dc_y = usize::try_from(dc.1).ok()?;
    let src_stride = usize::try_from(src.width).ok()?.checked_mul(4)?;
    let dst_stride = usize::try_from(dst.width).ok()?.checked_mul(4)?;
    let row_bytes = width.checked_mul(4)?;
    let src_base = src_y
        .checked_mul(src_stride)?
        .checked_add(src_x.checked_mul(4)?)?;
    let dst_base = dc_y
        .checked_mul(dst_stride)?
        .checked_add(dc_x.checked_mul(4)?)?;
    Some(SourceOverRowSlices {
        height,
        src_stride,
        dst_stride,
        src_base,
        dst_base,
        row_bytes,
    })
}

#[cfg(feature = "simd-experiment")]
fn source_over_runs(src: &[u8], dst: &mut [u8], opa: u8) {
    if src.len() != dst.len() || !src.len().is_multiple_of(4) {
        return;
    }
    let mut index = 0;
    while index + 4 <= src.len() {
        let alpha = src[index + 3];
        let start = index;
        if alpha == 0 {
            while index + 4 <= src.len() && src[index + 3] == 0 {
                index += 4;
            }
        } else if alpha == 255 && opa == 255 {
            while index + 4 <= src.len() && src[index + 3] == 255 {
                index += 4;
            }
            dst[start..index].copy_from_slice(&src[start..index]);
        } else {
            let mut pixel = [dst[index], dst[index + 1], dst[index + 2], dst[index + 3]];
            let color = [src[index], src[index + 1], src[index + 2], alpha];
            blend_pixel_at(&mut pixel, color, 255, opa);
            dst[index..index + 4].copy_from_slice(&pixel);
            index += 4;
        }
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
        treat_as_opaque,
        ..
    } = options;
    let sw = src_rect.2.saturating_sub(src_rect.0);
    let sh = src_rect.3.saturating_sub(src_rect.1);
    if sw <= 0 || sh <= 0 {
        return;
    }
    if !pixels_usable(dst, "operateRect") || !pixels_usable(src, "operateRect") {
        return;
    }
    // The kernel is resolved once for the whole pass, so the per-pixel loop
    // never dispatches on the operation mode.
    let Some(kernel) = resolve_kernel(mode, opa, treat_as_opaque) else {
        log::error!("engine: operateRect met the unknown operation mode {mode}");
        return;
    };
    let Some(dc) = intersect(dst, (dx, dy, dx.saturating_add(sw), dy.saturating_add(sh))) else {
        return;
    };
    // SIMD experiment: exact source-over spans. Opaque blocks copy, transparent
    // blocks leave the destination alone, and a row with partial alpha is
    // reduced to its exact runs (copy/leave) plus scalar blending only for the
    // partial pixels.
    #[cfg(feature = "simd-experiment")]
    if opa == 255
        && !hold_alpha
        && matches!(kernel, ModeKernel::SourceOver)
        && let Some(simd_rows) = source_over_row_slices(dst, src, dc, dx, dy, src_rect)
    {
        let mut handled = true;
        for row in 0..simd_rows.height {
            let Some(src_start) = row
                .checked_mul(simd_rows.src_stride)
                .and_then(|offset| simd_rows.src_base.checked_add(offset))
            else {
                handled = false;
                break;
            };
            let Some(dst_start) = row
                .checked_mul(simd_rows.dst_stride)
                .and_then(|offset| simd_rows.dst_base.checked_add(offset))
            else {
                handled = false;
                break;
            };
            let (Some(src_row), Some(dst_row)) = (
                src.rgba
                    .get(src_start..src_start.saturating_add(simd_rows.row_bytes)),
                dst.rgba
                    .get_mut(dst_start..dst_start.saturating_add(simd_rows.row_bytes)),
            ) else {
                handled = false;
                break;
            };
            if crate::simd::source_over_row(src_row, dst_row) {
                continue;
            }
            source_over_runs(src_row, dst_row, opa);
        }
        if handled {
            dst.mark_dirty_rect(clipped_region(dc));
            return;
        }
    }
    for y in dc.1..dc.3 {
        for x in dc.0..dc.2 {
            let color = read_pixel(src, src_rect.0 + (x - dx), src_rect.1 + (y - dy));
            let color = opaque_source(color, treat_as_opaque);
            let old_alpha = read_pixel(dst, x, y)[3];
            blend_pixel_mode(dst, x, y, color, opa, kernel);
            if hold_alpha {
                let mut d = read_pixel(dst, x, y);
                d[3] = old_alpha;
                write_pixel(dst, x, y, d);
            }
        }
    }
    // The pass wrote inside the clipped destination rectangle only, so the
    // upload needs that rectangle and not the whole bitmap.
    dst.mark_dirty_rect(clipped_region(dc));
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
        treat_as_opaque,
        ..
    } = options;
    let dw = dest.2 - dest.0;
    let dh = dest.3 - dest.1;
    if dw <= 0 || dh <= 0 || src_rect.2 <= src_rect.0 || src_rect.3 <= src_rect.1 {
        return;
    }
    if !pixels_usable(dst, "operateStretch") || !pixels_usable(src, "operateStretch") {
        return;
    }
    let Some(dc) = intersect(dst, dest) else {
        return;
    };
    // The kernel is resolved once for the whole pass, so the per-pixel loop
    // never dispatches on the operation mode.
    let Some(kernel) = resolve_kernel(mode, opa, treat_as_opaque) else {
        log::error!("engine: operateStretch met the unknown operation mode {mode}");
        return;
    };
    // The measured stage/UI stretches are one-to-one rectangles: the source
    // and destination have the same extent and only their origins differ.
    // At integer source centres every supported resampler returns the source
    // texel, so the pass is a copy (Copy mode) or a source-over blit. This
    // avoids the per-pixel float resampler without changing any pixel.
    if let Some((dx, dy)) = stretch_exact_translation(dest, src_rect, stretch_type) {
        match kernel {
            // The shortcut copies the source's pixels verbatim, so it cannot
            // serve a pass that reads the source as an opaque image: that
            // pass writes an opaque alpha where the copy would write the
            // source's own.
            ModeKernel::Copy if !hold_alpha && !treat_as_opaque => {
                blit_copy_clipped(
                    dst,
                    dx,
                    dy,
                    src,
                    src_rect,
                    (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
                );
                return;
            }
            ModeKernel::SourceOver => {
                blit_over(
                    dst,
                    dx,
                    dy,
                    BlitSource {
                        bitmap: src,
                        rect: src_rect,
                    },
                    (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
                    BlitOptions {
                        mode: 0,
                        opa,
                        treat_as_opaque,
                        hold_alpha,
                    },
                );
                return;
            }
            _ => {}
        }
    }
    let rows = (dc.3 - dc.1) as usize;
    let columns = (dc.2 - dc.0) as usize;
    let threads = affine_thread_count(rows, columns);
    let row_bytes = match usize::try_from(dst.width)
        .ok()
        .and_then(|w| w.checked_mul(4))
    {
        Some(width) => width,
        None => return,
    };
    let band_start = (dc.1 as usize).saturating_mul(row_bytes);
    let band_len = rows.saturating_mul(row_bytes);
    let Some(band) = dst
        .rgba
        .get_mut(band_start..band_start.saturating_add(band_len))
    else {
        return;
    };
    let area = StretchArea { dest, clip: dc };
    let blend = StretchBlend {
        opa,
        hold_alpha,
        treat_as_opaque,
        kernel,
    };
    if threads <= 1 {
        stretch_blit_mode_band(
            band,
            dc.1,
            dst.width as i32,
            area,
            BlitSource {
                bitmap: src,
                rect: src_rect,
            },
            stretch_type,
            blend,
        );
    } else {
        let rows_per_band = rows.div_ceil(threads);
        let chunk_bytes = rows_per_band.saturating_mul(row_bytes);
        let chunks: Vec<&mut [u8]> = band.chunks_mut(chunk_bytes).collect();
        let dst_width = dst.width as i32;
        std::thread::scope(|scope| {
            for (index, chunk) in chunks.into_iter().enumerate() {
                let first_row = dc.1 + (index * rows_per_band) as i32;
                scope.spawn(move || {
                    stretch_blit_mode_band(
                        chunk,
                        first_row,
                        dst_width,
                        area,
                        BlitSource {
                            bitmap: src,
                            rect: src_rect,
                        },
                        stretch_type,
                        blend,
                    );
                });
            }
        });
    }
    // The pass wrote inside the clipped destination rectangle only, so the
    // upload needs that rectangle and not the whole bitmap.
    dst.mark_dirty_rect(clipped_region(dc));
}

/// A one-to-one stretch with integer origins: the mapping is an integer
/// translation, so every supported resampler is exact.
fn stretch_exact_translation(
    dest: RectI,
    src_rect: RectI,
    stretch_type: i64,
) -> Option<(i32, i32)> {
    let kind = stretch_type & 0xffff;
    if !matches!(kind, 0..=13) {
        return None;
    }
    if dest.2.saturating_sub(dest.0) != src_rect.2.saturating_sub(src_rect.0)
        || dest.3.saturating_sub(dest.1) != src_rect.3.saturating_sub(src_rect.1)
    {
        return None;
    }
    Some((dest.0, dest.1))
}

/// The destination window of one stretch pass.
#[derive(Clone, Copy)]
struct StretchArea {
    /// Destination rectangle the source is mapped onto.
    dest: RectI,
    /// Clipped destination rectangle the pass writes inside.
    clip: RectI,
}

/// The blend a stretch band applies.
#[derive(Clone, Copy)]
struct StretchBlend {
    /// The source opacity.
    opa: u8,
    /// Whether the destination alpha is held.
    hold_alpha: bool,
    /// Whether the source is read as an opaque image.
    treat_as_opaque: bool,
    /// The resolved kernel.
    kernel: ModeKernel,
}

/// One contiguous band of destination rows for [`stretch_blit_mode`].
fn stretch_blit_mode_band(
    band: &mut [u8],
    first_row: i32,
    dst_width: i32,
    area: StretchArea,
    source: BlitSource<'_>,
    stretch_type: i64,
    blend: StretchBlend,
) {
    let BlitSource {
        bitmap: src,
        rect: src_rect,
    } = source;
    let StretchBlend {
        opa,
        hold_alpha,
        treat_as_opaque,
        kernel,
    } = blend;
    let Ok(dst_width) = usize::try_from(dst_width) else {
        return;
    };
    let Some(dst_pitch) = dst_width.checked_mul(4) else {
        return;
    };
    let (x0, x1) = (area.clip.0, area.clip.2);
    for (row_index, row) in band.chunks_mut(dst_pitch).enumerate() {
        let Ok(row_index) = i32::try_from(row_index) else {
            break;
        };
        let y = first_row.saturating_add(row_index);
        for x in x0..x1 {
            let color = stretch_sample(src, src_rect, area.dest, x, y, stretch_type);
            let color = opaque_source(color, treat_as_opaque);
            let Ok(x) = usize::try_from(x) else {
                continue;
            };
            let Some(dest_start) = x.checked_mul(4) else {
                continue;
            };
            let Some(destination) = row.get(dest_start..dest_start.saturating_add(4)) else {
                continue;
            };
            let old_alpha = destination[3];
            let mut pixel = [
                destination[0],
                destination[1],
                destination[2],
                destination[3],
            ];
            blend_pixel_mode_at(&mut pixel, color, opa, kernel);
            if hold_alpha {
                pixel[3] = old_alpha;
            }
            if let Some(destination) = row.get_mut(dest_start..dest_start.saturating_add(4)) {
                destination.copy_from_slice(&pixel);
            }
        }
    }
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
    affine_blit_clipped(dst, affine, source, options, clip_rect(None))
}

/// [`affine_blit`] with the clip rectangle the pass writes inside.
///
/// The pass touches the pixels of `clip` alone; the ones outside keep their
/// pixels. A caller that draws into a clipped region needs no copy of the
/// destination to restore afterwards.
pub fn affine_blit_clipped(
    dst: &mut BitmapState,
    affine: AffineBlit,
    source: BlitSource<'_>,
    options: BlitOptions,
    clip: RectI,
) {
    affine_blit_clipped_impl(dst, affine, source, options, clip, false);
}

/// [`affine_blit_clipped`] for corners expressed in the pixel-centre
/// convention: an origin at a half pixel denotes the pixel boundary, so a
/// one-to-one transfer still copies exact source texels.
pub fn affine_blit_clipped_pixel_center(
    dst: &mut BitmapState,
    affine: AffineBlit,
    source: BlitSource<'_>,
    options: BlitOptions,
    clip: RectI,
) {
    affine_blit_clipped_impl(dst, affine, source, options, clip, true);
}

fn affine_blit_clipped_impl(
    dst: &mut BitmapState,
    affine: AffineBlit,
    source: BlitSource<'_>,
    options: BlitOptions,
    clip: RectI,
    pixel_center: bool,
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
    let BlitOptions {
        mode,
        opa,
        treat_as_opaque,
        ..
    } = options;
    // The kernel is resolved once for the whole pass, so the per-pixel loop
    // never dispatches on the operation mode.
    let Some(kernel) = resolve_kernel(mode, opa, treat_as_opaque) else {
        log::error!("engine: the affine pass met the unknown operation mode {mode}");
        return;
    };
    let m = affine_forward(p0, p1, p2);
    let det = m[1] * m[5] - m[2] * m[4];
    if det.abs() < 1e-12 {
        return;
    }
    if !pixels_usable(dst, "affineCopy") || !pixels_usable(src, "affineCopy") {
        return;
    }
    // Inverse of the 2x2 [b c; e f].
    let inv = [m[5] / det, -m[2] / det, -m[4] / det, m[1] / det];
    // Bounding box of the mapped unit square.
    let bbox = affine_dest_bbox(m);
    let Some(dc) = intersect(dst, bbox) else {
        return;
    };
    // The pass writes inside the clip alone.
    let Some(dc) = intersect_rect(dc, clip) else {
        return;
    };
    if clear {
        for y in dc.1..dc.3 {
            for x in dc.0..dc.2 {
                write_pixel(dst, x, y, clear_color);
            }
        }
    }
    // A one-to-one integer transfer is a copy, whatever the affine resampler:
    // every supported kernel is exact at an integer source centre. This is
    // the full-screen UI/stage copy that otherwise pays the per-pixel inverse
    // map and float kernel. A pixel-centre origin at half a pixel denotes the
    // boundary, so the caller's convention decides whether the half offset is
    // applied; a fractional boundary still stays on the resampler.
    //
    // The shortcut copies the source's pixels verbatim, so it cannot serve a
    // pass that reads the source as an opaque image: that pass writes an
    // opaque alpha where the copy would write the source's own.
    if !treat_as_opaque
        && matches!(affine_blend(kernel), AffineBlend::Replace)
        && let Some((dx, dy)) = affine_unit_copy(m, src_rect, src, stretch_type, pixel_center)
    {
        blit_copy_clipped(dst, dx, dy, src, src_rect, clip);
        // The affine pass marks the mapped bounding box, even the one-pixel
        // margin the resampler skips; keep that upload region.
        dst.mark_dirty_rect(clipped_region(dc));
        return;
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
        kernel,
        opa,
        treat_as_opaque,
    };
    // A bilinear copy of an axis-aligned quad resolves its taps once per
    // column and row instead of once per pixel. The pass writes the generic
    // pass's pixels, so a blend other than copy keeps the generic path.
    let separable = if !treat_as_opaque && matches!(affine_blend(kernel), AffineBlend::Replace) {
        AffineAxisAlignedBilinear::new(&pass)
    } else {
        None
    };
    if let Some(separable) = separable {
        separable.run(dst, &pass);
    } else if pass.stretch_type == 0 && affine_nearest_axis_aligned(m) {
        affine_blit_nearest_rows(dst, pass);
    } else {
        affine_blit_generic(dst, pass);
    }
    // The pass wrote inside the clipped destination rectangle only, so the
    // upload needs that rectangle and not the whole bitmap.
    dst.mark_dirty_rect(clipped_region(dc));
}

/// The pixel region a clipped rectangle covers.
///
/// The rectangle comes from [`intersect`], so it lies inside the bitmap's
/// own bounds.
fn clipped_region(rect: RectI) -> PixelRegion {
    PixelRegion::from_min_max(
        rect.0.max(0) as u32,
        rect.1.max(0) as u32,
        rect.2.max(0) as u32,
        rect.3.max(0) as u32,
    )
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
    match affine_blend(pass.kernel) {
        AffineBlend::Replace => affine_blit_generic_with::<ReplaceBlend>(dst, pass),
        AffineBlend::SourceOver => affine_blit_generic_with::<SourceOverBlend>(dst, pass),
        AffineBlend::Mode => affine_blit_generic_with::<ModeBlend>(dst, pass),
    }
}

/// The generic affine pass with its blend resolved.
fn affine_blit_generic_with<B: BandBlend>(dst: &mut BitmapState, pass: AffinePass<'_>) {
    let AffinePass {
        map: m,
        inverse: inv,
        source: src,
        source_rect: src_rect,
        dest_rect: dc,
        stretch_type,
        kernel,
        opa,
        treat_as_opaque,
    } = pass;
    let sw = (src_rect.2 - src_rect.0) as f64;
    let sh = (src_rect.3 - src_rect.1) as f64;
    let row_bytes = dst.width as usize * 4;
    for y in dc.1..dc.3 {
        let row_start = y as usize * row_bytes;
        let Some(row) = dst.rgba.get_mut(row_start..row_start + row_bytes) else {
            continue;
        };
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
            let rect = (0, 0, dim_i32(src.width), dim_i32(src.height));
            let color = sample_kind(src, rect, fx, fy, stretch_type);
            let color = opaque_source(color, treat_as_opaque);
            let offset = x as usize * 4;
            let mut destination = [
                row[offset],
                row[offset + 1],
                row[offset + 2],
                row[offset + 3],
            ];
            B::blend(kernel, &mut destination, color, opa);
            row[offset..offset + 4].copy_from_slice(&destination);
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

/// The destination origin of a one-to-one, integer-aligned affine copy, or
/// None when the map resamples or the source rect leaves the image.
///
/// The generic pass samples at src_rect.0 + dest_x - m[0]; this is an integer
/// source centre exactly when the mapped origin (with the half-pixel offset of
/// the affine-matrix convention when present) is an integer. At an integer
/// centre the nearest, linear, cubic, spline and Lanczos kernels all return the
/// source texel unchanged, so the transfer is a row copy. The blur-style and
/// area-average kernels do not have that property.
fn affine_unit_copy(
    m: [f64; 6],
    src_rect: RectI,
    src: &BitmapState,
    stretch_type: i64,
    pixel_center: bool,
) -> Option<(i32, i32)> {
    let kind = stretch_type & 0xffff;
    if !matches!(kind, 0..=13) {
        return None;
    }
    let width = i64::from(src_rect.2) - i64::from(src_rect.0);
    let height = i64::from(src_rect.3) - i64::from(src_rect.1);
    if width <= 0 || height <= 0 {
        return None;
    }
    let width = width as f64;
    let height = height as f64;
    const EPSILON: f64 = 1e-6;
    if (m[1] - width).abs() > EPSILON
        || (m[5] - height).abs() > EPSILON
        || m[2].abs() > EPSILON
        || m[4].abs() > EPSILON
    {
        return None;
    }
    // The points convention maps the source corner itself; the affine matrix
    // convention maps the corner pixel centre at -0.5. Both describe a
    // pixel-edge origin exactly when adding the convention offset lands on an
    // integer.
    let near_integer = |value: f64| (value - value.round()).abs() <= EPSILON;
    let (origin_x, origin_y) = if near_integer(m[0]) && near_integer(m[3]) {
        (m[0].round(), m[3].round())
    } else if pixel_center && near_integer(m[0] + 0.5) && near_integer(m[3] + 0.5) {
        ((m[0] + 0.5).round(), (m[3] + 0.5).round())
    } else {
        return None;
    };
    if !origin_x.is_finite()
        || !origin_y.is_finite()
        || origin_x < f64::from(i32::MIN)
        || origin_x > f64::from(i32::MAX)
        || origin_y < f64::from(i32::MIN)
        || origin_y > f64::from(i32::MAX)
    {
        return None;
    }
    if src_rect.0 < 0
        || src_rect.1 < 0
        || i64::from(src_rect.2) > i64::from(src.width)
        || i64::from(src_rect.3) > i64::from(src.height)
    {
        return None;
    }
    Some((origin_x as i32, origin_y as i32))
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
    let columns = (pass.dest_rect.2 - pass.dest_rect.0) as usize;
    let band_start = pass.dest_rect.1 as usize * row_bytes;
    let Some(band) = dst.rgba.get_mut(band_start..band_start + rows * row_bytes) else {
        return;
    };
    let threads = affine_thread_count(rows, columns);
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
/// A pass that is short or narrow stays on the calling thread: the threads
/// it would spawn cost more to start than such a pass saves. A wide pass
/// splits into bands that each cover enough pixels to pay for their thread.
fn affine_thread_count(rows: usize, columns: usize) -> usize {
    const MIN_ROWS_PER_BAND: usize = 64;
    const MIN_PIXELS_PER_BAND: usize = 1 << 18;
    if rows < MIN_ROWS_PER_BAND * 2 {
        return 1;
    }
    let pixels = rows.saturating_mul(columns);
    if pixels < MIN_PIXELS_PER_BAND * 2 {
        return 1;
    }
    let available = std::thread::available_parallelism().map_or(1, |count| count.get());
    available
        .clamp(1, 8)
        .min(rows / MIN_ROWS_PER_BAND)
        .min(pixels / MIN_PIXELS_PER_BAND)
        .max(1)
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
    /// The resolved blend kernel.
    kernel: ModeKernel,
    /// The source opacity.
    opa: u8,
    /// Whether the source is read as an opaque image.
    treat_as_opaque: bool,
}

/// One destination column or row of a separable bilinear affine pass.
#[derive(Clone, Copy)]
struct AffineAxisTap {
    /// The source coordinate the generic pass computes for this column/row.
    coordinate: f32,
    /// The first source index the kernel reads; the second is `index + 1`.
    index: i32,
    /// The kernel weights of `index` and `index + 1`.
    first_weight: f32,
    second_weight: f32,
    /// Whether the generic pass's source-range check accepts this column/row.
    inside: bool,
}

/// A separable bilinear affine pass for an axis-aligned map.
///
/// The generic pass inverse-maps every destination pixel and evaluates the
/// kernel there. For an axis-aligned quad the source x depends on the
/// destination column alone and the source y on the destination row alone, so
/// the taps and weights repeat across the image. Building them once and
/// walking the destination with the same float arithmetic samples exactly the
/// pixels the generic pass samples.
struct AffineAxisAlignedBilinear {
    /// First destination column the column table covers.
    x0: i32,
    /// The tap of each destination column, in order.
    columns: Vec<AffineAxisTap>,
    /// First destination row the row table covers.
    y0: i32,
    /// The tap of each destination row, in order.
    rows: Vec<AffineAxisTap>,
    /// Source width in pixels (the kernel clamps to it).
    source_width: usize,
    /// Source height in pixels.
    source_height: usize,
}

/// The largest source coordinate the separable pass accepts.
///
/// `index as f32` and `(index + 1) as f32` must be exact for the generic
/// pass's tap arithmetic, which holds below 2^23. A larger coordinate takes
/// the generic pass, which has no such bound.
const MAX_AXIS_COORDINATE: f32 = 8_388_608.0;

impl AffineAxisAlignedBilinear {
    /// Builds the separable pass for `pass`, or `None` when the resampler is
    /// not bilinear, the map shears or rotates, or a source coordinate cannot
    /// use the generic pass's tap arithmetic.
    fn new(pass: &AffinePass<'_>) -> Option<Self> {
        if !matches!(pass.stretch_type, 1 | 2 | 4) {
            return None;
        }
        // The generic pass's source x is `inv[0]*px + inv[1]*py`; an
        // axis-aligned map makes `inv[1]` and `inv[2]` signed zeros, so each
        // axis depends on its own destination coordinate alone.
        if pass.map[1] == 0.0 || pass.map[5] == 0.0 || pass.map[2] != 0.0 || pass.map[4] != 0.0 {
            return None;
        }
        if !pass.map[0].is_finite()
            || !pass.map[3].is_finite()
            || !pass.inverse[0].is_finite()
            || !pass.inverse[3].is_finite()
        {
            return None;
        }
        if pass.source.width == 0
            || pass.source.height == 0
            || pass.source.width > i32::MAX as u32
            || pass.source.height > i32::MAX as u32
        {
            return None;
        }
        let source_width = pass.source.width as usize;
        let source_height = pass.source.height as usize;
        let source_extent_x = (pass.source_rect.2 - pass.source_rect.0) as f64;
        let source_extent_y = (pass.source_rect.3 - pass.source_rect.1) as f64;
        if source_extent_x <= 0.0 || source_extent_y <= 0.0 {
            return None;
        }
        let (x0, x1) = (pass.dest_rect.0, pass.dest_rect.2);
        let (y0, y1) = (pass.dest_rect.1, pass.dest_rect.3);
        if x0 < 0 || x1 <= x0 || y0 < 0 || y1 <= y0 {
            return None;
        }
        let column_count = usize::try_from(x1 - x0).ok()?;
        let row_count = usize::try_from(y1 - y0).ok()?;
        let mut columns = Vec::with_capacity(column_count);
        for x in x0..x1 {
            columns.push(axis_tap(
                x,
                pass.map[0],
                pass.inverse[0],
                pass.source_rect.0 as f32,
                source_extent_x,
            )?);
        }
        let mut rows = Vec::with_capacity(row_count);
        for y in y0..y1 {
            rows.push(axis_tap(
                y,
                pass.map[3],
                pass.inverse[3],
                pass.source_rect.1 as f32,
                source_extent_y,
            )?);
        }
        Some(Self {
            x0,
            columns,
            y0,
            rows,
            source_width,
            source_height,
        })
    }

    /// Runs the pass over the destination rectangle of `pass`.
    fn run(&self, dst: &mut BitmapState, pass: &AffinePass<'_>) {
        let dest = pass.dest_rect;
        let row_bytes = match usize::try_from(dst.width)
            .ok()
            .and_then(|w| w.checked_mul(4))
        {
            Some(row_bytes) if row_bytes > 0 => row_bytes,
            _ => return,
        };
        let Ok(rows) = usize::try_from(dest.3 - dest.1) else {
            return;
        };
        let Ok(columns) = usize::try_from(dest.2 - dest.0) else {
            return;
        };
        let Some(band_len) = rows.checked_mul(row_bytes) else {
            return;
        };
        let Some(band_start) = usize::try_from(dest.1)
            .ok()
            .and_then(|row| row.checked_mul(row_bytes))
        else {
            return;
        };
        let Some(band) = dst
            .rgba
            .get_mut(band_start..band_start.saturating_add(band_len))
        else {
            return;
        };
        let threads = affine_thread_count(rows, columns);
        if threads <= 1 {
            self.run_band(band, dest.1, row_bytes, pass.source);
            return;
        }
        let rows_per_band = rows.div_ceil(threads);
        let chunk_bytes = rows_per_band.saturating_mul(row_bytes);
        let chunks: Vec<&mut [u8]> = band.chunks_mut(chunk_bytes).collect();
        std::thread::scope(|scope| {
            for (index, chunk) in chunks.into_iter().enumerate() {
                let first_row = dest.1 + (index * rows_per_band) as i32;
                scope.spawn(move || self.run_band(chunk, first_row, row_bytes, pass.source));
            }
        });
    }

    /// Runs one band of destination rows.
    fn run_band(&self, band: &mut [u8], first_row: i32, row_bytes: usize, src: &BitmapState) {
        let Some(first) = usize::try_from(first_row - self.y0).ok() else {
            return;
        };
        let rows = band.len() / row_bytes;
        let Some(table) = self.rows.get(first..first.saturating_add(rows)) else {
            return;
        };
        let source_height = self.source_height as i32;
        let source_width = self.source_width as i32;
        let column_start = self.x0 as usize * 4;
        for (row, pixels) in table.iter().zip(band.chunks_exact_mut(row_bytes)) {
            if !row.inside {
                continue;
            }
            let Some(pixels) = pixels.get_mut(column_start..) else {
                continue;
            };
            let source_row0 =
                self.clamped_index(row.index, source_height) as usize * self.source_width;
            let source_row1 = self.clamped_index(row.index.saturating_add(1), source_height)
                as usize
                * self.source_width;
            for (column, target) in self
                .columns
                .iter()
                .zip(pixels.as_chunks_mut::<4>().0.iter_mut())
            {
                if !column.inside {
                    continue;
                }
                let source_col0 = self.clamped_index(column.index, source_width) as usize;
                let source_col1 =
                    self.clamped_index(column.index.saturating_add(1), source_width) as usize;
                let color = self.sample(
                    src,
                    row,
                    column,
                    (source_row0, source_row1),
                    (source_col0, source_col1),
                );
                target.copy_from_slice(&color);
            }
        }
    }

    /// The clamped source index the kernel reads for `index`.
    fn clamped_index(&self, index: i32, limit: i32) -> i32 {
        index.clamp(0, limit - 1)
    }

    /// Samples one destination pixel with the generic pass's tap order and
    /// float arithmetic.
    ///
    /// The generic kernel skips a zero weight; adding the same `pixel * 0.0`
    /// term instead leaves the accumulator and the weight sum unchanged, so
    /// the four taps run unconditionally.
    fn sample(
        &self,
        src: &BitmapState,
        row: &AffineAxisTap,
        column: &AffineAxisTap,
        source_row_offsets: (usize, usize),
        source_columns: (usize, usize),
    ) -> [u8; 4] {
        let weight00 = row.first_weight * column.first_weight;
        let weight01 = row.first_weight * column.second_weight;
        let weight10 = row.second_weight * column.first_weight;
        let weight11 = row.second_weight * column.second_weight;
        let offset00 = (source_row_offsets.0 + source_columns.0) * 4;
        let offset01 = (source_row_offsets.0 + source_columns.1) * 4;
        let offset10 = (source_row_offsets.1 + source_columns.0) * 4;
        let offset11 = (source_row_offsets.1 + source_columns.1) * 4;
        let p00 = &src.rgba[offset00..offset00 + 4];
        let p01 = &src.rgba[offset01..offset01 + 4];
        let p10 = &src.rgba[offset10..offset10 + 4];
        let p11 = &src.rgba[offset11..offset11 + 4];
        let mut acc = [0.0f32; 4];
        for (channel, value) in acc.iter_mut().enumerate() {
            *value += f32::from(p00[channel]) * weight00;
            *value += f32::from(p01[channel]) * weight01;
            *value += f32::from(p10[channel]) * weight10;
            *value += f32::from(p11[channel]) * weight11;
        }
        let wsum = weight00 + weight01 + weight10 + weight11;
        if wsum.abs() < 1e-6 {
            return read_pixel_rect(
                src,
                (0, 0, self.source_width as i32, self.source_height as i32),
                column.coordinate.round() as i32,
                row.coordinate.round() as i32,
            );
        }
        let mut out = [0u8; 4];
        if wsum == 1.0 {
            for (channel, value) in acc.iter().enumerate() {
                out[channel] = value.round().clamp(0.0, 255.0) as u8;
            }
        } else {
            for (channel, value) in acc.iter().enumerate() {
                out[channel] = (*value / wsum).round().clamp(0.0, 255.0) as u8;
            }
        }
        out
    }
}

/// The tap of one destination column or row.
fn axis_tap(
    destination: i32,
    origin: f64,
    inverse: f64,
    source_origin: f32,
    source_extent: f64,
) -> Option<AffineAxisTap> {
    let position = f64::from(destination) + 0.5 - origin;
    let unit = inverse * position;
    if !(0.0..=1.0).contains(&unit) {
        return Some(AffineAxisTap {
            coordinate: 0.0,
            index: 0,
            first_weight: 0.0,
            second_weight: 0.0,
            inside: false,
        });
    }
    let coordinate = source_origin + (unit * source_extent) as f32 - 0.5;
    if !coordinate.is_finite() || coordinate.abs() > MAX_AXIS_COORDINATE {
        return None;
    }
    let index = coordinate.floor() as i32;
    let first_weight = (1.0 - (coordinate - index as f32).abs()).max(0.0);
    let second_weight = (1.0 - (coordinate - (index + 1) as f32).abs()).max(0.0);
    Some(AffineAxisTap {
        coordinate,
        index,
        first_weight,
        second_weight,
        inside: true,
    })
}

/// The nearest pass over one band of destination rows.
///
/// `band` holds the destination rows from `first_row`, in order.
fn affine_nearest_band(band: &mut [u8], first_row: i32, dst_width: i32, pass: AffinePass<'_>) {
    match affine_blend(pass.kernel) {
        AffineBlend::Replace => {
            affine_nearest_band_with::<ReplaceBlend>(band, first_row, dst_width, pass);
        }
        AffineBlend::SourceOver => {
            affine_nearest_band_with::<SourceOverBlend>(band, first_row, dst_width, pass);
        }
        AffineBlend::Mode => {
            affine_nearest_band_with::<ModeBlend>(band, first_row, dst_width, pass);
        }
    }
}

/// The nearest pass over one band of destination rows, with its blend
/// resolved.
fn affine_nearest_band_with<B: BandBlend>(
    band: &mut [u8],
    first_row: i32,
    dst_width: i32,
    pass: AffinePass<'_>,
) {
    let AffinePass {
        map: m,
        inverse: inv,
        source: src,
        source_rect: src_rect,
        dest_rect: dc,
        kernel,
        opa,
        treat_as_opaque,
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
        // The source row is the floor of the sample position, clamped to the
        // source. A negative position clamps to the first row either way, so
        // the truncation is the floor.
        let sy = ((fy + 0.5) as i32).clamp(0, src_height - 1);
        let row_start = sy as usize * src_row_bytes;
        let Some(source_row) = src.rgba.get(row_start..row_start + src_row_bytes) else {
            continue;
        };
        // The `u` term of the inverse map that does not depend on the
        // destination column is the same for the whole row.
        let inv1_py = inv[1] * py;
        // The destination column's half-pixel position grows by exactly one
        // per column, so it is stepped instead of recomputed.
        let mut x_half = dc.0 as f64 + 0.5;
        for column in dc.0 as usize..dc.2 as usize {
            let px = x_half - m[0];
            x_half += 1.0;
            let u = inv[0] * px + inv1_py;
            if !(0.0..=1.0).contains(&u) {
                continue;
            }
            let fx = src_rect.0 as f32 + (u * sw) as f32 - 0.5;
            // The source column is the floor of the sample position, clamped
            // to the source; a negative position clamps to the first column
            // either way, so the truncation is the floor.
            let sx = ((fx + 0.5) as i32).clamp(0, src_width - 1);
            let source_offset = sx as usize * 4;
            let color = [
                source_row[source_offset],
                source_row[source_offset + 1],
                source_row[source_offset + 2],
                source_row[source_offset + 3],
            ];
            let color = opaque_source(color, treat_as_opaque);
            let offset = column * 4;
            let mut destination = [
                row[offset],
                row[offset + 1],
                row[offset + 2],
                row[offset + 3],
            ];
            B::blend(kernel, &mut destination, color, opa);
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

/// Drop `root`'s composited content, and the cache-slot bitmap with it when
/// the slot holds that content.
///
/// The cache slot holds either the composited content or the running
/// transition's frame, and the content slot holds the composited content. The
/// content goes with the refresh; the slot survives it, unless the slot *is*
/// the content — then it goes with the content and is released once.
fn release_subtree_caches(scene: &mut Scene, root: u32) {
    let (content, display) = match scene.layer_mut_untracked(root) {
        Some(layer) => (layer.cache_content.take(), layer.cache_bitmap.take()),
        None => return,
    };
    let slot_is_content = display.is_some() && display == content;
    if let Some(content) = content {
        scene.release_bitmap(content);
    }
    if !slot_is_content
        && let Some(display) = display
        && let Some(layer) = scene.layer_mut_untracked(root)
    {
        layer.cache_bitmap = Some(display);
    }
}

/// Allocate or reuse `root`'s transition frame at `w` x `h`: the bitmap a
/// whole-subtree transition blends into, which the projection draws in place
/// of the layer's subtree. Distinct from the composited content so the blend
/// never overwrites the content it reads.
pub fn ensure_transition_display(scene: &mut Scene, root: u32, w: u32, h: u32) -> Option<u32> {
    let layer = scene.layer(root)?;
    if let Some(id) = layer.cache_bitmap
        // A cache slot that holds the layer's own image or its composited
        // content is not a transition display: blending into it would
        // overwrite script-owned pixels and invalidate the content the frame
        // samples. Only a display left by this transition is reusable.
        && layer.cache_content != Some(id)
        && layer.bitmap != Some(id)
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

/// Sum of the drawn sources' [`BitmapState::content_generation`] for `root`'s
/// subtree. A descendant whose subtree cache is live contributes the cache's
/// generation and covers its own descendants, exactly like
/// [`collect_subtree_draws`], so a change to a hidden layer does not
/// invalidate the cache while an edit to any drawn image does — including the
/// self image a transition blends into, even when the edit did not go through
/// a layer accessor.
///
/// `depth` bounds the nested-cache walk: a parent chain cannot be deeper than
/// the layer pool, so a malformed cycle stops instead of recursing forever.
fn subtree_source_generation(scene: &Scene, root: u32, depth: usize) -> u64 {
    let Some(root_layer) = scene.layer(root) else {
        return 0;
    };
    let window = root_layer.window;
    let mut sum = 0u64;
    // Layer id -> the live cache bitmap whose pixels already cover it.
    let mut covered: Map<u32, u32> = crate::hasher::map();
    for id in scene.window_layer_order(window) {
        if id != root && !is_descendant(scene, id, root) {
            continue;
        }
        let Some(layer) = scene.layer(id) else {
            continue;
        };
        if let Some(owner) = layer
            .parent
            .and_then(|parent| covered.get(&parent).copied())
        {
            covered.insert(id, owner);
            continue;
        }
        if id != root && !(layer.visible && layer.opacity != 0.0) {
            continue;
        }
        if id != root && (subtree_opacity(scene, id, root) * 255.0).round() == 0.0 {
            continue;
        }
        if let Some(cache) = live_subtree_cache(scene, layer, root, depth) {
            covered.insert(id, cache);
            if let Some(bitmap) = scene.bitmap(cache) {
                sum = sum.wrapping_add(bitmap.content_generation);
            }
            continue;
        }
        if let Some(bitmap) = subtree_drawn_image(layer).and_then(|id| scene.bitmap(id)) {
            sum = sum.wrapping_add(bitmap.content_generation);
        }
    }
    sum
}

/// The image a subtree draw samples.
enum SubtreeSource {
    /// The layer's own live cache: the composited subtree at the layer rect,
    /// with no image placement of its own.
    Cache(u32),
    /// A bitmap placed through the layer's `imageLeft`/`imageTop`/
    /// `imageWidth`/`imageHeight` transform.
    Bitmap(u32),
    /// A plane derived from the layer's main image or province buffer, which
    /// has no bitmap in the scene to borrow.
    Derived(BitmapState),
}

impl SubtreeSource {
    /// The dimensions the source presents to the placement model.
    fn dims(&self, scene: &Scene) -> Option<(u32, u32)> {
        match self {
            Self::Cache(id) | Self::Bitmap(id) => scene
                .bitmap(*id)
                .map(|bitmap| (bitmap.width, bitmap.height)),
            Self::Derived(bitmap) => Some((bitmap.width, bitmap.height)),
        }
    }
}

/// The cache bitmap a descendant layer draws in place of its live subtree.
///
/// The cache is live while a consumer requires it
/// ([`Scene::inc_cache_enabled_count`]), no change is pending, its content is
/// up to date with the drawn sources, and it is the composited content rather
/// than a transition frame. The layer that owns the composite is never its
/// own source: it is writing that cache. `depth` is the nested-cache walk
/// depth; past the layer pool a malformed parent cycle is not trusted.
fn live_subtree_cache(
    scene: &Scene,
    layer: &crate::scene::LayerState,
    root: u32,
    depth: usize,
) -> Option<u32> {
    if layer.id == root
        || layer.cache_enabled_count == 0
        || layer.cache_recalc
        || layer.transition_self.is_some()
        || depth >= scene.layers.len()
    {
        return None;
    }
    let cache = layer.cache_bitmap?;
    if layer.cache_content != Some(cache) {
        return None;
    }
    let recorded = layer.cache_source_generation;
    (subtree_source_generation(scene, layer.id, depth + 1) == recorded).then_some(cache)
}

/// One resolved draw for the subtree cache: the source, its placement in the
/// cache, and the draw face.
struct SubtreeDraw {
    source: SubtreeSource,
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

/// The image a layer contributes to the composite: its live cache when one is
/// current, otherwise its `face` — the `dfMask`/`dfProvince` plane, or the
/// main image (or transition self image) it draws.
fn subtree_source(
    scene: &Scene,
    layer: &crate::scene::LayerState,
    root: u32,
    depth: usize,
) -> Option<SubtreeSource> {
    if let Some(cache) = live_subtree_cache(scene, layer, root, depth) {
        return Some(SubtreeSource::Cache(cache));
    }
    match layer.face {
        2 => {
            let main = layer.bitmap.and_then(|id| scene.bitmap(id))?;
            if !pixels_usable(main, "renderLayerSubtree") {
                return None;
            }
            grayscale_bitmap(main.width, main.height, |x, y| {
                main.pixel_offset(x, y)
                    .and_then(|i| main.rgba.get(i + 3).copied())
                    .unwrap_or(0)
            })
            .map(SubtreeSource::Derived)
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
            .map(SubtreeSource::Derived)
        }
        _ => subtree_drawn_image(layer).map(SubtreeSource::Bitmap),
    }
}

/// Where a subtree draw lands in the cache.
struct SubtreePlacement {
    /// Layer offset relative to the subtree root.
    offset: (i32, i32),
    /// Image rectangle in layer coordinates.
    image: (i32, i32, u32, u32),
    /// Source bitmap size.
    bmp: (u32, u32),
    /// Layer `ClipRect` to intersect, or `None` for a cache, which covers the
    /// layer rect and carries no `ClipRect` of its own.
    clip: Option<Rect>,
    /// Intersection of this layer's absolute rect and every ancestor rect, in
    /// cache-root-relative coordinates. The subtree cache applies the same
    /// ancestor clip as the live projection.
    ancestor: Option<RectI>,
}

/// Resolve a layer's drawn region in the cache: the source region in bitmap
/// pixels and the destination rectangle, clipped to the layer rect and to
/// `placement.clip`. `dw`/`dh` differ from the source extent only when the
/// image is scaled.
fn subtree_placement(
    layer: &crate::scene::LayerState,
    placement: SubtreePlacement,
    cache: (u32, u32),
) -> Option<(RectI, RectI)> {
    let SubtreePlacement {
        offset: (rel_x, rel_y),
        image: (image_left, image_top, img_w, img_h),
        bmp: (bmp_w, bmp_h),
        clip,
        ancestor,
    } = placement;
    let (w, h) = cache;
    if img_w == 0 || img_h == 0 {
        return None;
    }
    let img_x = rel_x.saturating_add(image_left);
    let img_y = rel_y.saturating_add(image_top);
    let img_x1 = img_x.saturating_add(img_w as i32);
    let img_y1 = img_y.saturating_add(img_h as i32);
    let rect_x1 = rel_x.saturating_add(layer.rect.w as i32);
    let rect_y1 = rel_y.saturating_add(layer.rect.h as i32);
    let (clx0, cly0, clx1, cly1) = clip_rect(clip);
    let (ax0, ay0, ax1, ay1) = ancestor.unwrap_or((i32::MIN, i32::MIN, i32::MAX, i32::MAX));
    let dx0 = img_x
        .saturating_add(clx0)
        .max(img_x)
        .max(rel_x)
        .max(0)
        .max(ax0);
    let dy0 = img_y
        .saturating_add(cly0)
        .max(img_y)
        .max(rel_y)
        .max(0)
        .max(ay0);
    let dx1 = img_x
        .saturating_add(clx1)
        .min(img_x1)
        .min(rect_x1)
        .min(w as i32)
        .min(ax1);
    let dy1 = img_y
        .saturating_add(cly1)
        .min(img_y1)
        .min(rect_y1)
        .min(h as i32)
        .min(ay1);
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

/// An `i32` bound for a pixel dimension.
fn rect_i32(value: u32) -> i32 {
    value.min(i32::MAX as u32) as i32
}

/// Intersect two absolute layer rectangles. A non-overlapping pair yields a
/// zero-sized rectangle at the shared origin.
fn intersect_layer_rects(a: Rect, b: Rect) -> Rect {
    let left = a.x.max(b.x);
    let top = a.y.max(b.y);
    let right =
        a.x.saturating_add(rect_i32(a.w))
            .min(b.x.saturating_add(rect_i32(b.w)));
    let bottom =
        a.y.saturating_add(rect_i32(a.h))
            .min(b.y.saturating_add(rect_i32(b.h)));
    Rect::new(
        left,
        top,
        (right.saturating_sub(left).max(0)) as u32,
        (bottom.saturating_sub(top).max(0)) as u32,
    )
}

/// Resolve the back-to-front draws for `root` and its subtree, relative to
/// `root`'s origin, using the same `imageLeft`/`imageTop`/`imageWidth`/
/// `imageHeight` placement the render projection applies. A layer whose own
/// cache is live draws that cache and covers its descendants.
fn collect_subtree_draws(scene: &Scene, root: u32, w: u32, h: u32) -> Vec<SubtreeDraw> {
    let Some(root_layer) = scene.layer(root) else {
        return Vec::new();
    };
    let window = root_layer.window;
    let Some((root_ax, root_ay)) = scene.absolute_offset(root) else {
        return Vec::new();
    };
    let mut draws = Vec::new();
    // Layer id -> the live cache bitmap whose pixels already cover it.
    let mut covered: Map<u32, u32> = crate::hasher::map();
    // Layer id -> the absolute intersection of its rect and every ancestor
    // rect. A texture-less container still has to clip its descendants.
    let mut subtree_clips: Map<u32, Option<Rect>> = crate::hasher::map();
    for id in scene.window_layer_order(window) {
        if id != root && !is_descendant(scene, id, root) {
            continue;
        }
        let Some(layer) = scene.layer(id) else {
            continue;
        };
        // A live cache in a cached ancestor already contains this layer's
        // pixels, so it contributes no draw of its own.
        if let Some(owner) = layer
            .parent
            .and_then(|parent| covered.get(&parent).copied())
        {
            covered.insert(id, owner);
            continue;
        }
        let Some((ax, ay)) = scene.absolute_offset(id) else {
            continue;
        };
        let clip = scene.clipping_rect(id).unwrap_or(layer.rect);
        let own_rect = Rect::new(ax, ay, clip.w, clip.h);
        let ancestor_clip = layer
            .parent
            .and_then(|parent| subtree_clips.get(&parent).copied().flatten());
        let subtree_clip = match ancestor_clip {
            Some(parent_clip) => intersect_layer_rects(parent_clip, own_rect),
            None => own_rect,
        };
        subtree_clips.insert(id, Some(subtree_clip));
        let ancestor_rel = Some((
            subtree_clip.x.saturating_sub(root_ax),
            subtree_clip.y.saturating_sub(root_ay),
            subtree_clip
                .x
                .saturating_sub(root_ax)
                .saturating_add(rect_i32(subtree_clip.w)),
            subtree_clip
                .y
                .saturating_sub(root_ay)
                .saturating_add(rect_i32(subtree_clip.h)),
        ));
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
        let Some(source) = subtree_source(scene, layer, root, 0) else {
            continue;
        };
        if let SubtreeSource::Cache(cache) = &source {
            covered.insert(id, *cache);
        }
        let Some((bmp_w, bmp_h)) = source.dims(scene) else {
            continue;
        };
        if bmp_w == 0 || bmp_h == 0 {
            continue;
        }
        let rel_x = ax.saturating_sub(root_ax);
        let rel_y = ay.saturating_sub(root_ay);
        // The cache covers the layer rect; a drawn image is placed by the
        // layer's image transform, defaulting to the bitmap's own size. The
        // cache is handed to the parent as the whole layer rect, so the layer
        // `ClipRect` does not clip it.
        let (image, clip) = match &source {
            SubtreeSource::Cache(_) => ((0, 0, bmp_w, bmp_h), None),
            SubtreeSource::Bitmap(_) | SubtreeSource::Derived(_) => (
                (
                    layer.image_left,
                    layer.image_top,
                    if layer.image_width > 0 {
                        layer.image_width
                    } else {
                        bmp_w
                    },
                    if layer.image_height > 0 {
                        layer.image_height
                    } else {
                        bmp_h
                    },
                ),
                layer.clip,
            ),
        };
        let Some((src, dest)) = subtree_placement(
            layer,
            SubtreePlacement {
                offset: (rel_x, rel_y),
                image,
                bmp: (bmp_w, bmp_h),
                clip,
                ancestor: ancestor_rel,
            },
            (w, h),
        ) else {
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

/// Composite one resolved draw into the cache. The source is borrowed for the
/// draw only; the cache stays zero wherever no draw reaches.
fn composite_subtree_draw(target: &mut BitmapState, draw: &SubtreeDraw, src: &BitmapState) {
    const FULL: RectI = (i32::MIN, i32::MIN, i32::MAX, i32::MAX);
    if draw.dw == src.width as i32 && draw.dh == src.height as i32 {
        match draw.mode {
            0 => {}
            1 if draw.opa == 255 => {
                blit_copy_clipped(target, draw.dx, draw.dy, src, draw.src, FULL);
            }
            1 | 2 => blit_over(
                target,
                draw.dx,
                draw.dy,
                BlitSource {
                    bitmap: src,
                    rect: draw.src,
                },
                FULL,
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
/// wherever no child drew, which the transparent clear reproduces. A
/// descendant whose own subtree cache is current is composited through that
/// cache bitmap, and its descendants are covered by it; otherwise the
/// descendant contributes its live image. Returns `None` for an unknown layer
/// or a content bitmap that cannot be allocated.
///
/// The content is a plain `BitmapState` in the scene: there is no hidden
/// registry, and the caller owns its lifetime through
/// [`Scene::inc_cache_enabled_count`] / [`Scene::dec_cache_enabled_count`].
pub fn render_layer_subtree(scene: &mut Scene, root: u32) -> Option<u32> {
    let (bitmap, w, h, trivial) = {
        let layer = scene.layer(root)?;
        let clip = scene.clipping_rect(root).unwrap_or(layer.rect);
        let w = clip.w.max(1);
        let h = clip.h.max(1);
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
    let content_bitmap = scene.bitmap(content)?;
    if !pixels_usable(content_bitmap, "renderLayerSubtree") {
        return None;
    }
    let current_generation = subtree_source_generation(scene, root, 0);
    let (recalc, recorded) = {
        let layer = scene.layer(root)?;
        (layer.cache_recalc, layer.cache_source_generation)
    };
    if recalc || current_generation != recorded {
        let draws = collect_subtree_draws(scene, root, w, h);
        scene.bitmap_mut(content)?.rgba.fill(0);
        for draw in &draws {
            match &draw.source {
                SubtreeSource::Cache(id) | SubtreeSource::Bitmap(id) => {
                    let (target, source) = scene.bitmap_mut_with_source(content, *id)?;
                    composite_subtree_draw(target, draw, source);
                }
                SubtreeSource::Derived(source) => {
                    let target = scene.bitmap_mut_untracked(content)?;
                    composite_subtree_draw(target, draw, source);
                }
            }
        }
        scene.bitmap_mut_untracked(content)?.mark_dirty();
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

    #[cfg(feature = "simd-experiment")]
    #[test]
    fn source_over_runs_copy_opaque_skip_transparent_and_blend_partial() {
        let src = [
            10, 20, 30, 255, // opaque
            40, 50, 60, 0, // transparent
            70, 80, 90, 128, // partial
        ];
        let original = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let mut actual = original;
        source_over_runs(&src, &mut actual, 255);
        assert_eq!(&actual[0..4], &[10, 20, 30, 255]);
        assert_eq!(&actual[4..8], &original[4..8]);
        let mut expected = original;
        blend_pixel_at(
            (&mut expected[8..12]).try_into().expect("one pixel"),
            [70, 80, 90, 128],
            255,
            255,
        );
        assert_eq!(&actual[8..12], &expected[8..12]);
    }

    /// A full-plane copy maps every destination pixel to one source pixel,
    /// including the clipped rectangle and the offset placement
    /// (`reference/cpp/core/visual/LayerIntf.cpp:8796-8831`).
    #[test]
    fn copy_rect_copies_the_clipped_rectangle_from_the_mapped_source() {
        let src = source_texture(40, 40);
        let mut dst = BitmapState::new(64, 64, vec![9u8; 64 * 64 * 4]);
        let clip = (5, 6, 43, 31);
        blit_plane(
            &mut dst,
            5,
            6,
            &src,
            (2, 3, 40, 33),
            clip,
            COPY_MAIN | COPY_MASK,
        );
        for y in clip.1..clip.3 {
            for x in clip.0..clip.2 {
                let sx = 2 + (x - 5);
                let sy = 3 + (y - 6);
                let got = read_pixel(&dst, x, y);
                let expected = read_pixel(&src, sx, sy);
                assert_eq!(got, expected, "pixel ({x}, {y})");
            }
        }
        // Outside the clip the destination keeps its own pixels.
        assert_eq!(read_pixel(&dst, 4, 6), [9, 9, 9, 9]);
        assert_eq!(read_pixel(&dst, 5, 5), [9, 9, 9, 9]);
        assert_eq!(read_pixel(&dst, 43, 6), [9, 9, 9, 9]);
    }

    /// A source rectangle whose left edge is outside the source falls back to
    /// the per-pixel pass, which skips the pixels the source does not hold.
    ///
    /// The reference's copy clips the source and leaves the destination
    /// outside the clipped region untouched
    /// (`reference/cpp/core/visual/LayerIntf.cpp:8796-8831`).
    #[test]
    fn copy_rect_leaves_pixels_the_source_does_not_cover() {
        let src = source_texture(10, 10);
        let mut dst = BitmapState::new(20, 20, vec![7u8; 20 * 20 * 4]);
        blit_plane(
            &mut dst,
            0,
            0,
            &src,
            (-2, -2, 8, 8),
            (0, 0, 20, 20),
            COPY_MAIN | COPY_MASK,
        );
        // The destination pixels the source rectangle does not cover keep
        // their own value; the first covered destination pixel takes the
        // source's first pixel.
        assert_eq!(read_pixel(&dst, 0, 0), [7, 7, 7, 7]);
        assert_eq!(read_pixel(&dst, 2, 2), read_pixel(&src, 0, 0));
        assert_eq!(read_pixel(&dst, 7, 7), read_pixel(&src, 5, 5));
        assert_eq!(read_pixel(&dst, 10, 10), [7, 7, 7, 7]);
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
        generic_pass_typed(dst, corners, src, mode, opa, 0);
    }

    /// [`generic_pass`] with an explicit resampler.
    fn generic_pass_typed(
        dst: &mut BitmapState,
        corners: [(f64, f64); 3],
        src: &BitmapState,
        mode: i64,
        opa: u8,
        stretch_type: i64,
    ) {
        let m = affine_forward(corners[0], corners[1], corners[2]);
        let det = m[1] * m[5] - m[2] * m[4];
        let inverse = [m[5] / det, -m[2] / det, -m[4] / det, m[1] / det];
        let src_rect = (0, 0, src.width as i32, src.height as i32);
        let Some(dest_rect) = intersect(dst, affine_dest_bbox(m)) else {
            return;
        };
        let Some(kernel) = resolve_kernel(mode, opa, false) else {
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
                stretch_type,
                kernel,
                opa,
                treat_as_opaque: false,
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
    ///
    /// The pass covers enough pixels to be split: a smaller one stays on the
    /// calling thread, because the threads it would spawn cost more than the
    /// pass saves.
    #[test]
    fn the_nearest_scanline_pass_splits_long_passes() {
        let src = source_texture(64, 64);
        let corners = [(0.0, 0.0), (1024.0, 0.0), (0.0, 1024.0)];
        let mut scanline = BitmapState::new(1024, 1024, vec![0u8; 1024 * 1024 * 4]);
        let mut generic = BitmapState::new(1024, 1024, vec![0u8; 1024 * 1024 * 4]);
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
        assert!(
            scanline
                .rgba
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[3] != 0)
        );
    }

    /// The separable bilinear pass and the generic per-pixel pass must sample
    /// the same texels for an axis-aligned quad.
    ///
    /// A character stand's per-frame redraw is an `affineCopy` of an
    /// axis-aligned quad with `stFastLinear | stRefNoClip`
    /// (`reference/cpp/core/visual/LayerIntf.cpp:9227-9251`,
    /// `reference/cpp/core/visual/LayerBitmapIntf.cpp:2773-2860`), so the
    /// separable pass must not change a single pixel that the generic pass
    /// would write.
    #[test]
    fn the_separable_bilinear_pass_matches_the_generic_pass() {
        let src = source_texture(9, 7);
        let placements: [[(f64, f64); 3]; 8] = [
            [(0.4, 0.0), (9.4, 0.0), (0.4, 7.0)],
            [(1.5, 2.25), (10.5, 2.25), (1.5, 9.25)],
            [(0.0, 0.0), (13.0, 0.0), (0.0, 10.0)],
            [(-2.0, -1.0), (7.0, -1.0), (-2.0, 6.0)],
            [(0.5, 0.5), (4.5, 0.5), (0.5, 4.5)],
            [(3.0, 3.0), (9.0, 3.0), (3.0, 9.0)],
            [(0.25, 0.75), (15.5, 0.75), (0.25, 12.25)],
            [(-1.75, 3.5), (2.25, 3.5), (-1.75, 7.5)],
        ];
        for corners in placements {
            let m = affine_forward(corners[0], corners[1], corners[2]);
            assert_eq!(m[2], 0.0, "the placement must be axis-aligned");
            assert_eq!(m[4], 0.0, "the placement must be axis-aligned");
            let mut fast = BitmapState::new(20, 20, vec![0u8; 20 * 20 * 4]);
            let mut generic = BitmapState::new(20, 20, vec![0u8; 20 * 20 * 4]);
            let mut public = BitmapState::new(20, 20, vec![0u8; 20 * 20 * 4]);
            let det = m[1] * m[5] - m[2] * m[4];
            let src_rect = (0, 0, src.width as i32, src.height as i32);
            let dest_rect = intersect(&fast, affine_dest_bbox(m)).expect("a destination");
            let pass = AffinePass {
                map: m,
                inverse: [m[5] / det, -m[2] / det, -m[4] / det, m[1] / det],
                source: &src,
                source_rect: src_rect,
                dest_rect,
                stretch_type: 1,
                kernel: ModeKernel::Copy,
                opa: 255,
                treat_as_opaque: false,
            };
            let separable = AffineAxisAlignedBilinear::new(&pass)
                .expect("an axis-aligned bilinear map takes the separable pass");
            separable.run(&mut fast, &pass);
            generic_pass_typed(&mut generic, corners, &src, 1, 255, 1);
            assert_eq!(
                fast.rgba, generic.rgba,
                "the separable pass must match the generic pass: {corners:?}"
            );
            affine_blit(
                &mut public,
                AffineBlit {
                    corners,
                    stretch_type: 0x1_0001,
                    clear: false,
                    clear_color: [0, 0, 0, 0],
                },
                BlitSource {
                    bitmap: &src,
                    rect: src_rect,
                },
                BlitOptions {
                    mode: 1,
                    opa: 255,
                    treat_as_opaque: false,
                    hold_alpha: false,
                },
            );
            assert_eq!(
                public.rgba, generic.rgba,
                "affineCopy must write the generic pixels: {corners:?}"
            );
        }
    }

    /// The separable pass writes the same pixels when its rows are handed to
    /// `run_band` in pieces.
    ///
    /// A large pass splits into row bands and runs them in parallel; the band
    /// bookkeeping must not shift a row or lose a column.  The split is driven
    /// directly because the thread threshold would make this test pay for a
    /// large generic pass in a debug build.
    #[test]
    fn the_separable_bilinear_pass_covers_every_row_band() {
        let src = source_texture(24, 18);
        let corners = [(1.25, 2.5), (33.25, 2.5), (1.25, 26.5)];
        let mut whole = BitmapState::new(40, 32, vec![0u8; 40 * 32 * 4]);
        let mut banded = BitmapState::new(40, 32, vec![0u8; 40 * 32 * 4]);
        let mut generic = BitmapState::new(40, 32, vec![0u8; 40 * 32 * 4]);
        let m = affine_forward(corners[0], corners[1], corners[2]);
        let det = m[1] * m[5] - m[2] * m[4];
        let src_rect = (0, 0, src.width as i32, src.height as i32);
        let dest_rect = intersect(&whole, affine_dest_bbox(m)).expect("a destination");
        let pass = AffinePass {
            map: m,
            inverse: [m[5] / det, -m[2] / det, -m[4] / det, m[1] / det],
            source: &src,
            source_rect: src_rect,
            dest_rect,
            stretch_type: 1,
            kernel: ModeKernel::Copy,
            opa: 255,
            treat_as_opaque: false,
        };
        let plan = AffineAxisAlignedBilinear::new(&pass).expect("a separable pass");
        plan.run(&mut whole, &pass);
        let row_bytes = 40 * 4;
        let top_start = dest_rect.1 as usize * row_bytes;
        let split = (dest_rect.1 + 12) as usize * row_bytes;
        let end = dest_rect.3 as usize * row_bytes;
        plan.run_band(
            &mut banded.rgba[top_start..split],
            dest_rect.1,
            row_bytes,
            &src,
        );
        plan.run_band(
            &mut banded.rgba[split..end],
            dest_rect.1 + 12,
            row_bytes,
            &src,
        );
        assert_eq!(
            whole.rgba, banded.rgba,
            "the rows must not depend on the band split"
        );
        generic_pass_typed(&mut generic, corners, &src, 1, 255, 1);
        assert_eq!(whole.rgba, generic.rgba);
    }

    /// A one-to-one stretch takes the exact source-over blit path.
    #[test]
    fn a_one_to_one_stretch_matches_the_source_over_blit() {
        let src = source_texture(8, 4);
        let full = (0, 0, 8, 4);
        let mut stretched = BitmapState::new(16, 16, vec![0u8; 16 * 16 * 4]);
        let mut blitted = BitmapState::new(16, 16, vec![0u8; 16 * 16 * 4]);
        let options = BlitOptions {
            mode: 2,
            opa: 255,
            treat_as_opaque: false,
            hold_alpha: false,
        };
        stretch_blit_mode(
            &mut stretched,
            (3, 4, 11, 8),
            BlitSource {
                bitmap: &src,
                rect: full,
            },
            1,
            options,
        );
        blit_over(
            &mut blitted,
            3,
            4,
            BlitSource {
                bitmap: &src,
                rect: full,
            },
            (i32::MIN, i32::MIN, i32::MAX, i32::MAX),
            options,
        );
        assert_eq!(stretched.rgba, blitted.rgba);
    }

    /// A one-to-one integer copy is recognised as a copy, including the
    /// pixel-centre half-pixel origin. A fractional boundary and the
    /// blur-style resamplers stay on the generic sampler: their output is not
    /// the source texel.
    #[test]
    fn a_one_to_one_integer_affine_copy_is_recognised() {
        let src = source_texture(4, 4);
        let corners = [(2.0, 3.0), (6.0, 3.0), (2.0, 7.0)];
        let map = affine_forward(corners[0], corners[1], corners[2]);
        for kind in [0, 1, 2, 3, 6, 8, 13] {
            assert_eq!(
                affine_unit_copy(map, (0, 0, 4, 4), &src, kind, false),
                Some((2, 3)),
                "stretch kind {kind} is exact at integer centres"
            );
        }
        assert!(
            affine_unit_copy(map, (0, 0, 4, 4), &src, 16, false).is_none(),
            "a Gaussian kernel does not copy at an integer centre"
        );
        let half = affine_forward((-0.5, -0.5), (3.5, -0.5), (-0.5, 3.5));
        assert_eq!(
            affine_unit_copy(half, (0, 0, 4, 4), &src, 1, true),
            Some((0, 0)),
            "the affine half-pixel convention copies at its integer origin"
        );
        let noisy = affine_forward(
            (-0.499999999999864, -0.499999999999727),
            (3.500000000000136, -0.499999999999727),
            (-0.499999999999864, 3.500000000000273),
        );
        assert_eq!(
            affine_unit_copy(noisy, (0, 0, 4, 4), &src, 1, true),
            Some((0, 0)),
            "a floating-point half origin is still the pixel boundary"
        );
        let fractional = affine_forward((-0.25, -0.5), (3.75, -0.5), (-0.25, 3.5));
        assert!(
            affine_unit_copy(fractional, (0, 0, 4, 4), &src, 1, true).is_none(),
            "a fractional origin still resamples"
        );
    }
}
