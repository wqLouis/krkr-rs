//! Pixel decoding for the PSB icon textures the player draws.
//!
//! A group's icons live either in the group's `texture` atlas, which each
//! icon crops with its `left`/`top`/`width`/`height`, or in a per-icon `pixel`
//! chunk. Either chunk is raw 32-bit RGBA, `RL` run-length compressed, or
//! 8-bit index data resolved through the sibling `pal` chunk. Decoding yields
//! straight-alpha RGBA8 in the scene's pixel layout.

/// Decoded straight-alpha RGBA8 pixels of one pixel chunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pixels {
    /// The image width in pixels.
    pub(crate) width: u32,
    /// The image height in pixels.
    pub(crate) height: u32,
    /// `width * height * 4` bytes.
    pub(crate) rgba: Vec<u8>,
}

/// Why a pixel chunk could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TextureError {
    /// The palette chunk holds no complete entry, so no index resolves.
    EmptyPalette,
    /// An 8-bit index addresses a palette entry that does not exist.
    PaletteIndex {
        /// The offending index.
        index: u8,
        /// The number of whole entries the palette holds.
        entries: usize,
    },
    /// A dimension or element count does not fit the address space.
    TooLarge,
}

impl std::fmt::Display for TextureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyPalette => write!(f, "the palette chunk holds no entries"),
            Self::PaletteIndex { index, entries } => {
                write!(f, "palette index {index} is outside {entries} entries")
            }
            Self::TooLarge => write!(f, "the image dimensions are too large"),
        }
    }
}

/// Decode one pixel chunk into RGBA8 pixels.
///
/// `palette` is the sibling `pal` chunk when the container carries one; it
/// takes precedence over `compressed`, whose 8-bit path is
/// run-length compressed too. A stream that ends early leaves the remaining
/// pixels transparent.
pub(crate) fn decode_pixels(
    bytes: &[u8],
    palette: Option<&[u8]>,
    width: u32,
    height: u32,
    compressed: bool,
) -> Result<Pixels, TextureError> {
    let count = (width as usize)
        .checked_mul(height as usize)
        .ok_or(TextureError::TooLarge)?;
    if count.checked_mul(4).is_none() {
        return Err(TextureError::TooLarge);
    }
    let rgba = match palette.filter(|palette| !palette.is_empty()) {
        Some(palette) => expand_palette(bytes, palette, count, compressed)?,
        None if compressed => swap_red_blue(rl_decode(bytes, count, 4)),
        None => swap_red_blue(raw(bytes, count)),
    };
    Ok(Pixels {
        width,
        height,
        rgba,
    })
}

/// Swap the red and blue channels of straight-alpha RGBA pixels.
///
/// The PSB raw/RL pixel chunks store their colour channels in B,G,R,A order;
/// the scene bitmap stores R,G,B,A, so the two channels are exchanged once
/// after decoding. The palette path already writes the scene order.
fn swap_red_blue(mut rgba: Vec<u8>) -> Vec<u8> {
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    rgba
}

/// Copy `count` RGBA pixels from a raw chunk, zero-filling what is short.
fn raw(bytes: &[u8], count: usize) -> Vec<u8> {
    let mut out = vec![0u8; count * 4];
    let copy = bytes.len().min(out.len());
    out[..copy].copy_from_slice(&bytes[..copy]);
    out
}

/// Resolve 8-bit indices through a `pal` chunk, whose entries are RGBA quads.
fn expand_palette(
    bytes: &[u8],
    palette: &[u8],
    count: usize,
    compressed: bool,
) -> Result<Vec<u8>, TextureError> {
    let indices = if compressed {
        rl_decode(bytes, count, 1)
    } else {
        let mut indices = vec![0u8; count];
        let copy = bytes.len().min(count);
        indices[..copy].copy_from_slice(&bytes[..copy]);
        indices
    };
    let entries = palette.len() / 4;
    if entries == 0 {
        return Err(TextureError::EmptyPalette);
    }
    let mut out = Vec::with_capacity(count * 4);
    for index in indices {
        let entry = usize::from(index);
        if entry >= entries {
            return Err(TextureError::PaletteIndex { index, entries });
        }
        out.extend_from_slice(&palette[entry * 4..entry * 4 + 4]);
    }
    Ok(out)
}

/// `RL` run-length decode of `elements` values of `align` bytes each.
///
/// A marker with the high bit set repeats the next `align` bytes
/// `(marker & 0x7f) + 3` times; any other marker introduces `(marker + 1)`
/// literal values.
fn rl_decode(bytes: &[u8], elements: usize, align: usize) -> Vec<u8> {
    let mut out = vec![0u8; elements.saturating_mul(align)];
    let mut src = 0usize;
    let mut dst = 0usize;
    while src < bytes.len() && dst < out.len() {
        let marker = bytes[src];
        src += 1;
        if marker & 0x80 != 0 {
            let count = usize::from(marker & 0x7f) + 3;
            let Some(unit) = bytes.get(src..src + align) else {
                break;
            };
            for _ in 0..count {
                if dst + align > out.len() {
                    break;
                }
                out[dst..dst + align].copy_from_slice(unit);
                dst += align;
            }
            src += align;
        } else {
            let count = (usize::from(marker) + 1) * align;
            if src + count > bytes.len() {
                break;
            }
            let copy = count.min(out.len() - dst);
            out[dst..dst + copy].copy_from_slice(&bytes[src..src + copy]);
            src += count;
            dst += copy;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{TextureError, decode_pixels, rl_decode};

    #[test]
    fn an_rl_stream_mixes_literals_and_runs() {
        // A literal marker (0x00) carries one value, a run marker (0x81)
        // repeats the next value `(0x81 & 0x7f) + 3` times, then one more
        // literal value completes six pixels.
        // `reference/cpp/plugins/motionplayer/PlayerInternal.h:63-102`.
        let mut stream = vec![0x00, 1, 2, 3, 4];
        stream.push(0x81);
        stream.extend_from_slice(&[9, 8, 7, 6]);
        stream.push(0x00);
        stream.extend_from_slice(&[5, 5, 5, 5]);
        let out = rl_decode(&stream, 6, 4);
        assert_eq!(
            out,
            vec![
                1, 2, 3, 4, 9, 8, 7, 6, 9, 8, 7, 6, 9, 8, 7, 6, 9, 8, 7, 6, 5, 5, 5, 5
            ]
        );
    }

    #[test]
    fn an_rl_stream_that_ends_early_leaves_the_rest_transparent() {
        // `reference/cpp/plugins/motionplayer/PlayerInternal.h:63-102`: the
        // reference allocates the full buffer zeroed and stops at the end of
        // the stream.
        let out = rl_decode(&[0x00, 1, 2, 3, 4], 4, 4);
        assert_eq!(out, vec![1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn a_raw_chunk_decodes_as_rgba() {
        let out = decode_pixels(&[1, 2, 3, 4, 5, 6, 7, 8], None, 2, 1, false)
            .expect("a raw chunk decodes");
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(out.rgba, vec![3, 2, 1, 4, 7, 6, 5, 8]);
    }

    #[test]
    fn a_palette_chunk_resolves_each_index() {
        // Palette entries are RGBA quads; the indices are one byte per pixel
        // (`reference/cpp/plugins/motionplayer/PlayerInternal.h:132-165`).
        let palette = [10, 20, 30, 40, 50, 60, 70, 80];
        let out = decode_pixels(&[1, 0], Some(palette.as_slice()), 2, 1, false)
            .expect("a palettized chunk decodes");
        assert_eq!(out.rgba, vec![50, 60, 70, 80, 10, 20, 30, 40]);
    }

    #[test]
    fn a_palette_index_outside_the_palette_is_an_error() {
        let palette = [10, 20, 30, 40];
        assert_eq!(
            decode_pixels(&[0, 1], Some(palette.as_slice()), 2, 1, false),
            Err(TextureError::PaletteIndex {
                index: 1,
                entries: 1
            })
        );
    }

    #[test]
    fn a_palette_too_short_for_one_entry_is_an_error() {
        // An empty chunk counts as "no palette" (the raw path), but a chunk
        // that cannot hold one whole RGBA entry has nothing to index.
        let truncated = [10, 20, 30];
        assert_eq!(
            decode_pixels(&[0], Some(truncated.as_slice()), 1, 1, false),
            Err(TextureError::EmptyPalette)
        );
        let empty: &[u8] = &[];
        let raw = decode_pixels(&[1, 2, 3, 4], Some(empty), 1, 1, false)
            .expect("an empty palette falls back to the raw chunk");
        assert_eq!(raw.rgba, vec![3, 2, 1, 4]);
    }
}
