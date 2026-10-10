//! Image encoding for the `media` backend.
//!
//! This is the encode counterpart of [`crate::decode_image`]: it turns a
//! tightly packed RGBA8 buffer into PNG/BMP/JPEG bytes. Like the rest of the
//! crate it is a pure function with no globals or caches — it depends only on
//! its arguments — and a request that cannot be honoured is a typed
//! [`MediaError`], never a fabricated result.
//!
//! The supported targets are the reference saver's formats: `png`, `bmp`
//! (also `dib`) and `jpg`/`jpeg` (also `jif`). JPEG has no alpha channel, so
//! the RGBA source is flattened to RGB before encoding; the other formats
//! keep the alpha channel.

use api::error::MediaError;
use image::{ExtendedColorType, ImageEncoder, ImageFormat};

use crate::Media;
use crate::hint::extension_of;

/// Resolves the `image`-crate format for an encode hint.
///
/// Accepts a bare format name (`"png"`), a leading-dot spelling (`".bmp"`)
/// or a full storage name (`"out/shot.jpg"`), matching the decode router's
/// extension handling. Only the saveable formats are recognized; anything
/// else returns `None` so the caller can raise
/// [`MediaError::Unsupported`].
fn format_for_encode_hint(hint: &str) -> Option<ImageFormat> {
    let ext = extension_of(hint).unwrap_or(hint).trim_start_matches('.');
    if ext.eq_ignore_ascii_case("png") {
        Some(ImageFormat::Png)
    } else if ext.eq_ignore_ascii_case("bmp") || ext.eq_ignore_ascii_case("dib") {
        Some(ImageFormat::Bmp)
    } else if ext.eq_ignore_ascii_case("jpg")
        || ext.eq_ignore_ascii_case("jpeg")
        || ext.eq_ignore_ascii_case("jif")
    {
        Some(ImageFormat::Jpeg)
    } else {
        None
    }
}

/// Stable, lowercase name for an `image` crate format (the inverse of
/// [`format_for_encode_hint`]).
fn format_name(format: ImageFormat) -> String {
    format
        .extensions_str()
        .first()
        .map(|name| (*name).to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Builds the typed encode failure for `format`.
///
/// The underlying codecs only fail on malformed input or allocation failure,
/// both of which are genuine encoder faults, so they surface as
/// [`MediaError::Encode`] with the target format attached.
fn encode_error(format: &str, message: impl Into<String>) -> MediaError {
    MediaError::Encode {
        format: format.to_string(),
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
    for pixel in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&pixel[..3]);
    }
    rgb
}

/// Encodes a tightly packed, row-major RGBA8 buffer as `hint` (`png`, `bmp`,
/// `jpg`/`jpeg`; a leading dot or a storage name is accepted).
///
/// `width`/`height` are in pixels and `rgba` must hold exactly
/// `width * height * 4` bytes; anything else is [`MediaError::InvalidData`].
/// An unknown target is [`MediaError::Unsupported`], and a failure inside the
/// underlying codec is [`MediaError::Encode`]. JPEG drops the alpha channel.
pub fn encode_image(
    m: &Media,
    width: u32,
    height: u32,
    rgba: &[u8],
    hint: &str,
) -> Result<Vec<u8>, MediaError> {
    let _ = m;

    let Some(format) = format_for_encode_hint(hint) else {
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

    let name = format_name(format);
    let mut out = Vec::new();
    match format {
        ImageFormat::Png => image::codecs::png::PngEncoder::new(&mut out)
            .write_image(rgba, width, height, ExtendedColorType::Rgba8)
            .map_err(|error| encode_error(&name, error.to_string()))?,
        ImageFormat::Bmp => image::codecs::bmp::BmpEncoder::new(&mut out)
            .write_image(rgba, width, height, ExtendedColorType::Rgba8)
            .map_err(|error| encode_error(&name, error.to_string()))?,
        ImageFormat::Jpeg => {
            let rgb = rgba_to_rgb(rgba);
            image::codecs::jpeg::JpegEncoder::new(&mut out)
                .write_image(&rgb, width, height, ExtendedColorType::Rgb8)
                .map_err(|error| encode_error(&name, error.to_string()))?
        }
        // `format_for_encode_hint` only ever returns the three variants above.
        _ => {
            return Err(MediaError::Unsupported {
                message: format!("no image encoder for format {hint:?}"),
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{encode_image, format_for_encode_hint};
    use crate::Media;
    use image::ImageFormat;

    #[test]
    fn hints_resolve_to_the_supported_formats() {
        assert_eq!(format_for_encode_hint("png"), Some(ImageFormat::Png));
        assert_eq!(format_for_encode_hint(".PNG"), Some(ImageFormat::Png));
        assert_eq!(
            format_for_encode_hint("out/shot.bmp"),
            Some(ImageFormat::Bmp)
        );
        assert_eq!(format_for_encode_hint("dib"), Some(ImageFormat::Bmp));
        assert_eq!(format_for_encode_hint("jpeg"), Some(ImageFormat::Jpeg));
        assert_eq!(format_for_encode_hint("a.jif"), Some(ImageFormat::Jpeg));
        assert_eq!(format_for_encode_hint("tlg"), None);
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
