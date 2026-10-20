//! Vectorized 8-bit pixel conversion.
//!
//! Both passes here are *prefix* passes: they convert whole 16-pixel blocks
//! whose source bytes are all in range and return how many pixels they wrote.
//! The caller's scalar loop owns the tail, so the short-slice rules — a missing
//! colour plane is black, a missing alpha plane is opaque, an out-of-range
//! sample reads `0xff` — keep exactly one implementation.

use fearless_simd::prelude::*;
use fearless_simd::{Level, u8x16, u16x8};

/// Pixels covered by one vector block: 16 bytes per plane, four planes.
const LANES: usize = 16;

/// Writes the RGBA pixels that the four planes cover completely.
///
/// `planes` holds the red, green, blue and alpha channel planes. A missing
/// plane fills with black, or opaque for the alpha plane. Returns the number of
/// pixels written, always a multiple of the block size.
pub(crate) fn pack_rgba8(level: Level, out: &mut [u8], planes: [Option<&[u8]>; 4]) -> usize {
    fearless_simd::dispatch!(level, simd => pack_rgba8_blocks(simd, out, planes))
}

#[inline(always)]
fn pack_rgba8_blocks<S: Simd>(simd: S, out: &mut [u8], planes: [Option<&[u8]>; 4]) -> usize {
    let pixels = out.len() / 4;
    let mut index = 0;
    while index + LANES <= pixels && covered(&planes, index + LANES) {
        let red = load_plane(simd, planes[0], index, 0x00);
        let green = load_plane(simd, planes[1], index, 0x00);
        let blue = load_plane(simd, planes[2], index, 0x00);
        let alpha = load_plane(simd, planes[3], index, 0xff);
        let chunk = &mut out[index * 4..(index + LANES) * 4];
        u8x16::<S>::store_four_interleaved([red, green, blue, alpha], chunk);
        index += LANES;
    }
    index
}

#[inline(always)]
fn load_plane<S: Simd>(simd: S, plane: Option<&[u8]>, index: usize, fill: u8) -> u8x16<S> {
    match plane {
        Some(plane) => u8x16::<S>::from_slice(simd, &plane[index..index + LANES]),
        None => u8x16::<S>::splat(simd, fill),
    }
}

/// Whether every present plane has at least `end` bytes.
fn covered(planes: &[Option<&[u8]>; 4], end: usize) -> bool {
    planes.iter().all(|plane| match plane {
        Some(plane) => plane.len() >= end,
        None => true,
    })
}

/// Writes `alpha[i] = alpha[i] * mask[i] / 255` over the bytes both slices
/// cover, rounded down. Returns the number of bytes written, always a
/// multiple of the block size.
pub(crate) fn mul_alpha8(level: Level, alpha: &mut [u8], mask: &[u8]) -> usize {
    fearless_simd::dispatch!(level, simd => mul_alpha8_blocks(simd, alpha, mask))
}

#[inline(always)]
fn mul_alpha8_blocks<S: Simd>(simd: S, alpha: &mut [u8], mask: &[u8]) -> usize {
    let len = alpha.len().min(mask.len());
    let mut index = 0;
    while index + LANES <= len {
        let alpha_block = u8x16::<S>::from_slice(simd, &alpha[index..index + LANES]);
        let mask_block = u8x16::<S>::from_slice(simd, &mask[index..index + LANES]);
        let (alpha_low, alpha_high) = alpha_block.widen();
        let (mask_low, mask_high) = mask_block.widen();
        let low = divide_by_255(simd, alpha_low * mask_low);
        let high = divide_by_255(simd, alpha_high * mask_high);
        low.narrow(high)
            .store_slice(&mut alpha[index..index + LANES]);
        index += LANES;
    }
    index
}

/// `value / 255` truncated.
///
/// `(x + 1 + (x >> 8)) >> 8` is exactly `x / 255` for every `x` up to 65534, and
/// the largest product of two bytes is 255 * 255 = 65025, so the shift is the
/// division.
#[inline(always)]
fn divide_by_255<S: Simd>(simd: S, value: u16x8<S>) -> u16x8<S> {
    let one = u16x8::<S>::splat(simd, 1);
    (value + one + (value >> 8)) >> 8
}
