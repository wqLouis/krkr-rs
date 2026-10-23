//! Optional SIMD source-over row experiment.
//!
//! A source-over row is a byte copy where every source pixel is opaque, and a
//! no-op where every source pixel is transparent. This pass checks those
//! conditions in 16-pixel blocks with SIMD, copies or skips whole blocks, and
//! leaves a block with partial alpha to the caller's scalar run blend. It is
//! compiled only with the `simd-experiment` feature.

use fearless_simd::prelude::*;
use fearless_simd::{Level, u8x16};

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
}
