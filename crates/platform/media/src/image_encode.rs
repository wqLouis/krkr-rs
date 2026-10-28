//! Image encoding for the `media` backend.
//!
//! This is the encode counterpart of [`crate::decode_image`]: it turns a
//! tightly packed RGBA8 buffer into PNG/BMP/JPEG bytes. Like the rest of the
//! crate it is a pure function with no globals or caches — it depends only on
//! its arguments — and a request that cannot be honoured is a typed
//! [`MediaError`], never a fabricated result.
//!
//! PNG and JPEG delegate to the `image` crate. BMP is written by hand with
//! the layout games expect: a 14-byte file header, a 40-byte info header
//! (`biSize = 40`), an optional 1024-byte
//! palette for 8 bpp, then the bottom-up (positive `biHeight`) scanlines with
//! their 4-byte pitch padding. The `bmp`/`bmp32` mode is 32 bpp (BGRA),
//! `bmp24` is 24 bpp (BGR) and `bmp8` is 8 bpp with the 252-colour ordered
//! dither.

use api::error::MediaError;
use image::{ExtendedColorType, ImageEncoder};

use crate::Media;
use crate::hint::extension_of;

/// BMP byte depth.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BmpMode {
    /// 32 bpp BGRA (the `bmp`/`bmp32` default).
    Bpp32,
    /// 24 bpp BGR.
    Bpp24,
    /// 8 bpp palettized with the 252-colour ordered dither.
    Bpp8,
}

impl BmpMode {
    /// Bytes stored per pixel.
    fn pixel_bytes(self) -> usize {
        match self {
            BmpMode::Bpp32 => 4,
            BmpMode::Bpp24 => 3,
            BmpMode::Bpp8 => 1,
        }
    }

    /// Stable lowercase name for error values.
    fn name(self) -> &'static str {
        match self {
            BmpMode::Bpp32 => "bmp",
            BmpMode::Bpp24 => "bmp24",
            BmpMode::Bpp8 => "bmp8",
        }
    }
}

/// Encode destination resolved from a save hint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EncodeTarget {
    Png,
    Jpeg,
    Bmp(BmpMode),
}

impl EncodeTarget {
    /// Stable lowercase name for error values.
    fn name(self) -> &'static str {
        match self {
            EncodeTarget::Png => "png",
            EncodeTarget::Jpeg => "jpeg",
            EncodeTarget::Bmp(mode) => mode.name(),
        }
    }
}

/// Resolves an encode hint to a destination.
///
/// Accepts a bare format name (`"png"`), a leading-dot spelling (`".bmp"`), a
/// full storage name (`"out/shot.jpg"`) or a BMP mode
/// (`"bmp32"`/`"bmp24"`/`"bmp8"`), matching the decode router's extension
/// handling. An unknown hint returns `None` so the caller can raise
/// [`MediaError::Unsupported`].
fn target_for_encode_hint(hint: &str) -> Option<EncodeTarget> {
    let ext = extension_of(hint).unwrap_or(hint).trim_start_matches('.');
    if ext.eq_ignore_ascii_case("png") {
        Some(EncodeTarget::Png)
    } else if ext.eq_ignore_ascii_case("jpg")
        || ext.eq_ignore_ascii_case("jpeg")
        || ext.eq_ignore_ascii_case("jif")
    {
        Some(EncodeTarget::Jpeg)
    } else if ext.eq_ignore_ascii_case("bmp")
        || ext.eq_ignore_ascii_case("bmp32")
        || ext.eq_ignore_ascii_case("dib")
    {
        Some(EncodeTarget::Bmp(BmpMode::Bpp32))
    } else if ext.eq_ignore_ascii_case("bmp24") {
        Some(EncodeTarget::Bmp(BmpMode::Bpp24))
    } else if ext.eq_ignore_ascii_case("bmp8") {
        Some(EncodeTarget::Bmp(BmpMode::Bpp8))
    } else {
        None
    }
}

/// Builds the typed encode failure for `target`.
///
/// The underlying PNG/JPEG codecs only fail on malformed input or allocation
/// failure, both of which are genuine encoder faults, so they surface as
/// [`MediaError::Encode`] with the target format attached.
fn encode_error(target: EncodeTarget, message: impl Into<String>) -> MediaError {
    MediaError::Encode {
        format: target.name().to_string(),
        message: message.into(),
    }
}

/// Validates a scanline-perfect RGBA8 buffer and returns its expected length.
fn expected_rgba_len(width: u32, height: u32) -> Result<usize, MediaError> {
    if width == 0 || height == 0 {
        return Err(MediaError::InvalidData {
            message: format!("cannot encode a {width}x{height} image"),
        });
    }
    (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| MediaError::InvalidData {
            message: format!("image dimensions {width}x{height} overflow an address space"),
        })
}

/// Flattens straight-alpha RGBA8 to RGB8, dropping the alpha channel.
fn rgba_to_rgb(rgba: &[u8]) -> Vec<u8> {
    let mut rgb = Vec::with_capacity(rgba.len() / 4 * 3);
    for pixel in rgba.as_chunks::<4>().0 {
        rgb.extend_from_slice(&pixel[..3]);
    }
    rgb
}

/// The 14-byte file header and the 40-byte info header.
const BMP_FILE_HEADER_SIZE: usize = 14;
/// Size of the packed info header written as `biSize`.
const BMP_INFO_HEADER_SIZE: usize = 40;
/// Combined fixed header prefix.
const BMP_HEADERS_SIZE: usize = BMP_FILE_HEADER_SIZE + BMP_INFO_HEADER_SIZE;
/// The 8 bpp palette is 256 BGRA quads (`pixelbytes == 1 ? 1024 : 0`).
const BMP_PALETTE_SIZE: usize = 1024;

/// The 4x4 ordered dither matrix.
const DITHER_4X4: [[u8; 4]; 4] = [[0, 12, 2, 14], [8, 4, 10, 6], [3, 15, 1, 13], [11, 7, 9, 5]];

/// Appends a little-endian 16-bit value.
fn push_le16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// Appends a little-endian 32-bit value.
fn push_le32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

/// The ordered-dither tables.
///
/// Indexed `[channel][row][column][level]`: channel 0 maps the level to
/// the palette's low coordinate (no multiplier), channel 1 to the middle
/// coordinate (`* 6`) and channel 2 to the high coordinate (`* 42`).
fn dither_table() -> [[[[u8; 256]; 4]; 4]; 3] {
    let mut table = [[[[0u8; 256]; 4]; 4]; 3];
    let [low, mid, high] = &mut table;
    for j in 0..4usize {
        for i in 0..4usize {
            let v1 = f64::from(DITHER_4X4[j][i]) / 16.0;
            let v2 = f64::from(DITHER_4X4[(j + 1) % 2][(i + 1) % 2]) / 16.0;
            let v3 = f64::from(DITHER_4X4[j][(i + 1) % 2]) / 16.0;
            for (n, level) in high[i][j].iter_mut().enumerate() {
                let nt = n as f64 / 255.0;

                // 256 levels -> 6 levels (the red and blue coordinates).
                let main5 = (nt * 5.0) as i32;
                let frac5 = nt * 5.0 - f64::from(main5);
                *level = ((main5 + i32::from(v1 < frac5)) * (6 * 7)) as u8;
                low[i][j][n] = (main5 + i32::from(v2 < frac5)) as u8;

                // 256 levels -> 7 levels (the green coordinate).
                let main6 = (nt * 6.0) as i32;
                let frac6 = nt * 6.0 - f64::from(main6);
                mid[i][j][n] = ((main6 + i32::from(v3 < frac6)) * 6) as u8;
            }
        }
    }
    table
}

/// The 256-entry dither palette (`TVP252DitherPalette`).
///
/// Entry `c = r * 42 + g * 6 + b` holds the BMP byte order `[B, G, R, 0]`;
/// entries past 251 are left black.
fn dither_palette() -> [[u8; 256]; 3] {
    let mut palette = [[0u8; 256]; 3];
    let mut c = 0usize;
    for r in 0..6usize {
        for g in 0..7usize {
            for b in 0..6usize {
                palette[0][c] = (r * 255 / 5) as u8;
                palette[1][c] = (g * 255 / 6) as u8;
                palette[2][c] = (b * 255 / 5) as u8;
                c += 1;
            }
        }
    }
    palette
}

/// Encodes an RGBA8 buffer as a BMP.
///
/// The layout is the one games expect: a packed file header + info header, an
/// optional palette, and bottom-up rows. `bmp`/`bmp32` store BGRA,
/// `bmp24` stores BGR and `bmp8` stores dithered palette indices. The 32 bpp
/// output is exactly `width * height * 4 + 54` bytes.
fn encode_bmp(mode: BmpMode, width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, MediaError> {
    let invalid = |message: String| MediaError::InvalidData { message };
    let width_i32 = i32::try_from(width).map_err(|_| {
        invalid(format!(
            "bmp width {width} does not fit the 32-bit header field"
        ))
    })?;
    let height_i32 = i32::try_from(height).map_err(|_| {
        invalid(format!(
            "bmp height {height} does not fit the 32-bit header field"
        ))
    })?;

    let pixel_bytes = mode.pixel_bytes();
    let row_bytes = (width as usize)
        .checked_mul(pixel_bytes)
        .ok_or_else(|| invalid(format!("bmp row for width {width} overflows")))?;
    // Rows pad up to the next 4-byte boundary; the value is
    // (((row_bytes - 1) >> 2) + 1) << 2.
    let pitch = row_bytes
        .checked_next_multiple_of(4)
        .ok_or_else(|| invalid(format!("bmp pitch for width {width} overflows")))?;

    let palette_size = if mode == BmpMode::Bpp8 {
        BMP_PALETTE_SIZE
    } else {
        0
    };
    let image_size = pitch
        .checked_mul(height as usize)
        .ok_or_else(|| invalid(format!("bmp image {width}x{height} overflows")))?;
    let offset = BMP_HEADERS_SIZE + palette_size;
    let file_size = offset
        .checked_add(image_size)
        .ok_or_else(|| invalid(format!("bmp file {width}x{height} overflows")))?;
    let file_size_u32 = u32::try_from(file_size)
        .map_err(|_| invalid(format!("bmp file size {file_size} exceeds a 32-bit field")))?;
    let offset_u32 = u32::try_from(offset)
        .map_err(|_| invalid(format!("bmp data offset {offset} exceeds a 32-bit field")))?;

    let mut out = Vec::with_capacity(file_size);

    // The BMP file header.
    push_le16(&mut out, 0x4d42); /* bfType */
    push_le32(&mut out, file_size_u32); /* bfSize */
    push_le16(&mut out, 0); /* bfReserved1 */
    push_le16(&mut out, 0); /* bfReserved2 */
    push_le32(&mut out, offset_u32); /* bfOffBits */

    // The BMP info header.
    push_le32(&mut out, BMP_INFO_HEADER_SIZE as u32); /* biSize */
    push_le32(&mut out, width_i32 as u32); /* biWidth */
    push_le32(&mut out, height_i32 as u32); /* biHeight (positive: bottom-up) */
    push_le16(&mut out, 1); /* biPlanes */
    push_le16(&mut out, (pixel_bytes as u16) * 8); /* biBitCount */
    push_le32(&mut out, 0); /* biCompression = BI_RGB */
    push_le32(&mut out, 0); /* biSizeImage */
    push_le32(&mut out, 0); /* biXPelsPerMeter */
    push_le32(&mut out, 0); /* biYPelsPerMeter */
    push_le32(&mut out, 0); /* biClrUsed */
    push_le32(&mut out, 0); /* biClrImportant */

    if mode == BmpMode::Bpp8 {
        let [blue, green, red] = dither_palette();
        for ((blue, green), red) in blue.iter().zip(green.iter()).zip(red.iter()) {
            out.push(*blue);
            out.push(*green);
            out.push(*red);
            out.push(0);
        }
    }

    let row_len = width as usize * 4;
    match mode {
        BmpMode::Bpp32 => {
            for y in (0..height as usize).rev() {
                let row = &rgba[y * row_len..y * row_len + row_len];
                for pixel in row.as_chunks::<4>().0 {
                    out.push(pixel[2]); // B
                    out.push(pixel[1]); // G
                    out.push(pixel[0]); // R
                    out.push(pixel[3]); // A
                }
            }
        }
        BmpMode::Bpp24 => {
            let pad = pitch - row_bytes;
            for y in (0..height as usize).rev() {
                let row = &rgba[y * row_len..y * row_len + row_len];
                for pixel in row.as_chunks::<4>().0 {
                    out.push(pixel[2]); // B
                    out.push(pixel[1]); // G
                    out.push(pixel[0]); // R
                }
                let padded = out.len() + pad;
                out.resize(padded, 0);
            }
        }
        BmpMode::Bpp8 => {
            let table = dither_table();
            let pad = pitch - row_bytes;
            for y in (0..height as usize).rev() {
                let row = &rgba[y * row_len..y * row_len + row_len];
                for (x, pixel) in row.as_chunks::<4>().0.iter().enumerate() {
                    let low = table[0][y & 3][x & 3][pixel[0] as usize] as usize;
                    let mid = table[1][y & 3][x & 3][pixel[1] as usize] as usize;
                    let high = table[2][y & 3][x & 3][pixel[2] as usize] as usize;
                    out.push((low + mid + high) as u8);
                }
                let padded = out.len() + pad;
                out.resize(padded, 0);
            }
        }
    }

    if out.len() != file_size {
        return Err(invalid(format!(
            "bmp writer produced {} bytes but the header declares {file_size}",
            out.len()
        )));
    }
    Ok(out)
}

/// Encodes a tightly packed, row-major RGBA8 buffer as `hint` (`png`, `bmp`,
/// `bmp32`, `bmp24`, `bmp8`, `jpg`/`jpeg`; a leading dot or a storage name is
/// accepted).
///
/// `width`/`height` are in pixels and `rgba` must hold exactly
/// `width * height * 4` bytes; anything else is [`MediaError::InvalidData`].
/// An unknown target is [`MediaError::Unsupported`], and a failure inside the
/// underlying codec is [`MediaError::Encode`]. JPEG drops the alpha channel;
/// BMP matches the expected layout byte for byte.
///
/// # Errors
///
/// [`MediaError::Unsupported`] when `hint` names no encoder this build has,
/// [`MediaError::InvalidData`] when `rgba` does not hold exactly
/// `width * height * 4` bytes or the size is not representable, and
/// [`MediaError::Encode`] when the codec itself fails.
pub fn encode_image(
    m: &Media,
    width: u32,
    height: u32,
    rgba: &[u8],
    hint: &str,
) -> Result<Vec<u8>, MediaError> {
    let _ = m;

    let Some(target) = target_for_encode_hint(hint) else {
        return Err(MediaError::Unsupported {
            message: format!("no image encoder for format {hint:?}"),
        });
    };
    let expected = expected_rgba_len(width, height)?;
    if rgba.len() != expected {
        return Err(MediaError::InvalidData {
            message: format!(
                "RGBA buffer is {} bytes but a {width}x{height} image needs {expected}",
                rgba.len()
            ),
        });
    }

    match target {
        EncodeTarget::Png => {
            let mut out = Vec::new();
            image::codecs::png::PngEncoder::new(&mut out)
                .write_image(rgba, width, height, ExtendedColorType::Rgba8)
                .map_err(|error| encode_error(target, error.to_string()))?;
            Ok(out)
        }
        EncodeTarget::Jpeg => {
            let mut out = Vec::new();
            let rgb = rgba_to_rgb(rgba);
            image::codecs::jpeg::JpegEncoder::new(&mut out)
                .write_image(&rgb, width, height, ExtendedColorType::Rgb8)
                .map_err(|error| encode_error(target, error.to_string()))?;
            Ok(out)
        }
        EncodeTarget::Bmp(mode) => encode_bmp(mode, width, height, rgba),
    }
}

#[cfg(test)]
mod tests {
    use super::{BmpMode, EncodeTarget, encode_image, target_for_encode_hint};
    use crate::Media;

    #[test]
    fn hints_resolve_to_the_supported_targets() {
        assert_eq!(target_for_encode_hint("png"), Some(EncodeTarget::Png));
        assert_eq!(target_for_encode_hint(".PNG"), Some(EncodeTarget::Png));
        assert_eq!(
            target_for_encode_hint("out/shot.bmp"),
            Some(EncodeTarget::Bmp(BmpMode::Bpp32))
        );
        assert_eq!(
            target_for_encode_hint("dib"),
            Some(EncodeTarget::Bmp(BmpMode::Bpp32))
        );
        assert_eq!(
            target_for_encode_hint("bmp24"),
            Some(EncodeTarget::Bmp(BmpMode::Bpp24))
        );
        assert_eq!(
            target_for_encode_hint("bmp8"),
            Some(EncodeTarget::Bmp(BmpMode::Bpp8))
        );
        assert_eq!(target_for_encode_hint("jpeg"), Some(EncodeTarget::Jpeg));
        assert_eq!(target_for_encode_hint("a.jif"), Some(EncodeTarget::Jpeg));
        assert_eq!(target_for_encode_hint("tlg"), None);
    }

    #[test]
    fn an_unknown_target_is_unsupported() {
        let media = Media::new();
        assert!(matches!(
            encode_image(&media, 1, 1, &[0, 0, 0, 255], "tlg"),
            Err(api::error::MediaError::Unsupported { .. })
        ));
    }

    #[test]
    fn a_mismatched_buffer_is_invalid_data() {
        let media = Media::new();
        assert!(matches!(
            encode_image(&media, 2, 2, &[0, 0, 0, 255], "png"),
            Err(api::error::MediaError::InvalidData { .. })
        ));
        assert!(matches!(
            encode_image(&media, 0, 4, &[], "png"),
            Err(api::error::MediaError::InvalidData { .. })
        ));
    }
}
