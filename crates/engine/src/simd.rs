//! Optional SIMD source-over row experiment.
//!
//! A source-over row is a byte copy where every source pixel is opaque, and a
//! no-op where every source pixel is transparent. This pass checks those
//! conditions in 16-pixel blocks with SIMD, copies or skips whole blocks, and
//! leaves a block with partial alpha to the caller's scalar run blend. It is
//! compiled only with the `simd-experiment` feature.

use fearless_simd::prelude::*;
use fearless_simd::{Level, u8x16, u32x4};

/// Copy `src` to `dst` when every source alpha is 255.
///
/// Returns false when the row is not fully opaque; the caller then runs the
/// scalar blend over the whole row. Blocks already copied are also opaque, so
/// re-blending them with the same opaque source leaves the same bytes.
pub(crate) fn copy_opaque_row(src: &[u8], dst: &mut [u8]) -> bool {
    if src.len() != dst.len() || !src.len().is_multiple_of(4) {
        return false;
    }
    let level = Level::new();
    fearless_simd::dispatch!(level, simd => copy_opaque_blocks(simd, src, dst))
}

#[inline(always)]
fn copy_opaque_blocks<S: Simd>(simd: S, src: &[u8], dst: &mut [u8]) -> bool {
    let mut index = 0;
    while index + 64 <= src.len() {
        let channels = u8x16::<S>::load_four_interleaved(simd, &src[index..index + 64]);
        if !channels[3].simd_eq(u8x16::<S>::splat(simd, 255)).all_true() {
            return false;
        }
        u8x16::<S>::store_four_interleaved(channels, &mut dst[index..index + 64]);
        index += 64;
    }
    while index + 4 <= src.len() {
        if src[index + 3] != 255 {
            return false;
        }
        dst[index..index + 4].copy_from_slice(&src[index..index + 4]);
        index += 4;
    }
    true
}

/// Apply one source-over row when every 16-pixel block is wholly opaque or
/// wholly transparent.
///
/// Opaque blocks copy the source bytes (source-over with `opa == 255`), and
/// transparent blocks leave the destination untouched. A block with any
/// partial alpha returns false; the caller processes the row's exact runs and
/// scalar-blends only the partial pixels, which is idempotent over the blocks
/// this pass already handled.
pub(crate) fn source_over_row(src: &[u8], dst: &mut [u8]) -> bool {
    if src.len() != dst.len() || !src.len().is_multiple_of(4) {
        return false;
    }
    let level = Level::new();
    fearless_simd::dispatch!(level, simd => source_over_blocks(simd, src, dst))
}

#[inline(always)]
fn source_over_blocks<S: Simd>(simd: S, src: &[u8], dst: &mut [u8]) -> bool {
    let opaque = u8x16::<S>::splat(simd, 255);
    let transparent = u8x16::<S>::splat(simd, 0);
    let mut index = 0;
    while index + 64 <= src.len() {
        let [_, _, _, alpha] = u8x16::<S>::load_four_interleaved(simd, &src[index..index + 64]);
        if alpha.simd_eq(opaque).all_true() {
            for offset in [0, 16, 32, 48] {
                let value = u8x16::<S>::from_slice(simd, &src[index + offset..index + offset + 16]);
                value.store_slice(&mut dst[index + offset..index + offset + 16]);
            }
        } else if !alpha.simd_eq(transparent).all_true() {
            return false;
        }
        index += 64;
    }
    while index + 4 <= src.len() {
        match src[index + 3] {
            255 => dst[index..index + 4].copy_from_slice(&src[index..index + 4]),
            0 => {}
            _ => return false,
        }
        index += 4;
    }
    true
}

/// The alpha blend over one row, block by block.
///
/// A group whose source is wholly opaque is blended with the constant
/// weight an opaque source gives: the alpha blend's weight table holds 255
/// for an opaque source at every destination alpha, so the mix is the same
/// for every pixel of the group. A group whose source is wholly transparent
/// leaves the destination alone where the destination's alpha is not zero:
/// there the weight is zero, so the mix keeps the destination's colour and
/// writes the destination's own alpha back.
///
/// A group that is neither is left to the caller's row loop. The pass applies
/// nothing unless every group of the row qualifies: the opaque group's mix
/// is not idempotent, so a row that was partly blended and then blended
/// again would not hold the bytes the blend gives.
///
/// `hold_alpha` says whether the destination's alpha is kept (the alpha
/// blend on an opaque destination) or written as opaque (the alpha blend on
/// an alpha destination, whose weight table leaves 255 behind an opaque
/// source).
///
/// The row's length must be a whole number of four-pixel groups.
pub(crate) fn alpha_blend_row(src: &[u8], dst: &mut [u8], hold_alpha: bool) -> bool {
    if src.len() != dst.len() || !src.len().is_multiple_of(16) {
        return false;
    }
    let level = Level::new();
    fearless_simd::dispatch!(level, simd => alpha_blend_groups(simd, src, dst, hold_alpha))
}

/// The number of pixels a group holds.
const GROUP_PIXELS: usize = 4;

/// The number of bytes a group holds.
const GROUP_BYTES: usize = GROUP_PIXELS * 4;

/// The alpha blend over one row, group by group.
#[inline(always)]
fn alpha_blend_groups<S: Simd>(simd: S, src: &[u8], dst: &mut [u8], hold_alpha: bool) -> bool {
    // The first pass: every group must be wholly opaque or wholly
    // transparent, and a transparent group's destination must not be fully
    // transparent, or the blend would reach the source's colour there.
    let mut index = 0;
    while index < src.len() {
        let Ok(source) = src[index..index + GROUP_BYTES].try_into() else {
            return false;
        };
        let alpha = alpha_of(simd, source);
        let opaque = alpha.simd_eq(u32x4::<S>::splat(simd, 255)).all_true();
        let transparent = alpha.simd_eq(u32x4::<S>::splat(simd, 0)).all_true();
        if !opaque && !transparent {
            return false;
        }
        if transparent {
            let Ok(destination) = dst[index..index + GROUP_BYTES].try_into() else {
                return false;
            };
            let alpha = alpha_of(simd, destination);
            if alpha.simd_eq(u32x4::<S>::splat(simd, 0)).any_true() {
                return false;
            }
        }
        index += GROUP_BYTES;
    }
    // The second pass: the groups.
    let mut index = 0;
    while index < src.len() {
        let mixed = {
            let Ok(source) = src[index..index + GROUP_BYTES].try_into() else {
                return false;
            };
            if alpha_of(simd, source)
                .simd_eq(u32x4::<S>::splat(simd, 0))
                .all_true()
            {
                // A wholly transparent source leaves the destination alone.
                None
            } else {
                let Ok(destination) = dst[index..index + GROUP_BYTES].try_into() else {
                    return false;
                };
                Some(alpha_blend_255(
                    simd,
                    argb_words(simd, load_words(simd, destination)),
                    argb_words(simd, load_words(simd, source)),
                    hold_alpha,
                ))
            }
        };
        if let Some(mixed) = mixed {
            let mut target = [0u8; GROUP_BYTES];
            store_words(simd, mixed, &mut target);
            dst[index..index + GROUP_BYTES].copy_from_slice(&target);
        }
        index += GROUP_BYTES;
    }
    true
}

/// The alpha bytes of a group of pixels.
///
/// The alpha byte is the high byte of the pixel's word in either byte
/// order, so the group is read in the machine's order.
#[inline(always)]
fn alpha_of<S: Simd>(simd: S, group: &[u8; GROUP_BYTES]) -> u32x4<S> {
    (load_words(simd, group) >> 24) & u32x4::<S>::splat(simd, 0xff)
}

/// The native words of a group of pixels.
#[inline(always)]
fn load_words<S: Simd>(simd: S, group: &[u8; GROUP_BYTES]) -> u32x4<S> {
    let words = [
        u32::from_ne_bytes([group[0], group[1], group[2], group[3]]),
        u32::from_ne_bytes([group[4], group[5], group[6], group[7]]),
        u32::from_ne_bytes([group[8], group[9], group[10], group[11]]),
        u32::from_ne_bytes([group[12], group[13], group[14], group[15]]),
    ];
    u32x4::<S>::load_array_ref(simd, &words)
}

/// The kernel's words of a native-order word vector.
///
/// The kernels work on the `0xAARRGGBB` word: the alpha byte is the high
/// byte and the red byte the next one. A native load gives the machine's
/// byte order, so the bytes are put in the kernels' order here. The change
/// is its own inverse, so the same function puts the words back.
#[inline(always)]
fn argb_words<S: Simd>(simd: S, words: u32x4<S>) -> u32x4<S> {
    #[cfg(target_endian = "little")]
    {
        // The native word is `0xAABBGGRR` and the kernel's word is
        // `0xAARRGGBB`: the red and the blue byte change places.
        let red = words & u32x4::<S>::splat(simd, 0x0000_00ff);
        let green = words & u32x4::<S>::splat(simd, 0x0000_ff00);
        let blue = words & u32x4::<S>::splat(simd, 0x00ff_0000);
        let alpha = words & u32x4::<S>::splat(simd, 0xff00_0000);
        (red << 16) | green | (blue >> 16) | alpha
    }
    #[cfg(target_endian = "big")]
    {
        // The native word is `0xRRGGBBAA` and the kernel's word is
        // `0xAARRGGBB`: the byte pairs change places.
        (words << 16) | (words >> 16)
    }
}

/// The alpha blend's mix with the constant weight 255.
///
/// The kernels' mix runs on the `0xAARRGGBB` word: the red and the blue
/// lanes borrow against each other inside the word, so the mix runs on
/// 32-bit lanes, which keep the borrow.
#[inline(always)]
fn alpha_blend_255<S: Simd>(
    simd: S,
    destination: u32x4<S>,
    source: u32x4<S>,
    hold_alpha: bool,
) -> u32x4<S> {
    let red_blue_mask = u32x4::<S>::splat(simd, 0x00ff_00ff);
    let green_mask = u32x4::<S>::splat(simd, 0x0000_ff00);
    let weight = u32x4::<S>::splat(simd, 255);
    let mut red_blue = destination & red_blue_mask;
    let difference = (source & red_blue_mask) - red_blue;
    red_blue = (red_blue + ((difference * weight) >> 8)) & red_blue_mask;
    let mut green = destination & green_mask;
    let difference = (source & green_mask) - green;
    green = (green + ((difference * weight) >> 8)) & green_mask;
    let mut result = red_blue + green;
    if hold_alpha {
        // The destination's alpha is kept.
        result |= destination & u32x4::<S>::splat(simd, 0xff00_0000);
    } else {
        // The weight table leaves an opaque alpha behind an opaque source.
        result |= u32x4::<S>::splat(simd, 0xff00_0000);
    }
    result
}

/// The native bytes of a group of kernel words.
#[inline(always)]
fn store_words<S: Simd>(simd: S, words: u32x4<S>, group: &mut [u8; GROUP_BYTES]) {
    let native = argb_words(simd, words);
    let mut lanes = [0u32; GROUP_PIXELS];
    native.store_array(&mut lanes);
    for (target, lane) in group.as_chunks_mut::<4>().0.iter_mut().zip(lanes) {
        target.copy_from_slice(&lane.to_ne_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_opaque_row_copies_exactly() {
        let mut src = vec![17u8; 4 * 40];
        for pixel in src.chunks_mut(4) {
            pixel[3] = 255;
        }
        let mut dst = vec![0u8; 4 * 40];
        assert!(copy_opaque_row(&src, &mut dst));
        assert_eq!(dst, src);
    }

    #[test]
    fn a_translucent_row_is_left_to_the_scalar_path() {
        let mut src = vec![17u8; 4 * 40];
        src[31] = 128;
        let original = vec![9u8; 4 * 40];
        let mut dst = original.clone();
        assert!(!copy_opaque_row(&src, &mut dst));
        assert_eq!(dst, original);
    }

    #[test]
    fn a_transparent_source_over_row_leaves_the_destination_alone() {
        let src = vec![0u8; 4 * 40];
        let original = vec![9u8; 4 * 40];
        let mut dst = original.clone();
        assert!(source_over_row(&src, &mut dst));
        assert_eq!(dst, original);
    }

    #[test]
    fn an_opaque_source_over_row_copies_exactly() {
        let mut src = vec![17u8; 4 * 40];
        for pixel in src.chunks_mut(4) {
            pixel[3] = 255;
        }
        let mut dst = vec![0u8; 4 * 40];
        assert!(source_over_row(&src, &mut dst));
        assert_eq!(dst, src);
    }

    #[test]
    fn a_partial_source_over_row_falls_back_before_writing_the_block() {
        let mut src = vec![0u8; 4 * 40];
        src[31] = 128;
        let original = vec![9u8; 4 * 40];
        let mut dst = original.clone();
        assert!(!source_over_row(&src, &mut dst));
        assert_eq!(dst, original);
    }

    /// A row of four pixels, the size the alpha blend's groups hold.
    fn group() -> Vec<u8> {
        (0..16u8).collect()
    }

    #[test]
    fn an_opaque_alpha_blend_row_is_applied() {
        let mut src = group();
        for pixel in src.chunks_mut(4) {
            pixel[3] = 255;
        }
        let original = group();
        let mut dst = original.clone();
        assert!(alpha_blend_row(&src, &mut dst, false));
        // The opaque source leaves an opaque alpha behind.
        assert_eq!(dst[3], 255);
    }

    #[test]
    fn a_transparent_alpha_blend_row_leaves_the_destination_alone() {
        let mut src = group();
        for pixel in src.chunks_mut(4) {
            pixel[3] = 0;
        }
        let mut destination = group();
        for pixel in destination.chunks_mut(4) {
            pixel[3] = 255;
        }
        let original = destination.clone();
        let mut dst = original.clone();
        assert!(alpha_blend_row(&src, &mut dst, false));
        assert_eq!(dst, original);
    }

    #[test]
    fn a_transparent_alpha_blend_row_over_a_transparent_destination_falls_back() {
        let mut src = group();
        for pixel in src.chunks_mut(4) {
            pixel[3] = 0;
        }
        let mut original = group();
        for pixel in original.chunks_mut(4) {
            pixel[3] = 0;
        }
        let mut dst = original.clone();
        // The destination's alpha is zero, so the blend would reach the
        // source's colour; the block pass leaves the row to the row loop.
        assert!(!alpha_blend_row(&src, &mut dst, false));
        assert_eq!(dst, original);
    }

    #[test]
    fn a_partial_alpha_blend_row_falls_back_before_writing_the_group() {
        let mut src = group();
        for pixel in src.chunks_mut(4) {
            pixel[3] = 255;
        }
        src[7] = 128;
        let original = group();
        let mut dst = original.clone();
        assert!(!alpha_blend_row(&src, &mut dst, false));
        assert_eq!(dst, original);
    }
}
