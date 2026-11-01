//! Image format dispatch and header probing.
//!
//! The dispatcher keeps this effective order:
//!
//! 1. an explicit TLG spelling (`.tlg`/`.tlg5`/`.tlg6`) → the native TLG
//!    decoder; a corrupt real TLG reports its decode error, a *mislabeled*
//!    non-TLG file falls through;
//! 2. the format implied by the extension hint (disambiguates weak-magic
//!    formats such as TGA);
//! 3. content detection: TLG magic first (the `image` crate cannot sniff
//!    it), then `image::guess_format`'s magic table — so a wrong or missing
//!    extension still decodes.
//!
//! Neither [`probe_image`] nor [`decode_image`] touches any global state;
//! they depend only on their arguments.
//!
//! [`decode_index_plane`] is the third entry: it reads the 8-bit index plane
//! of a palettized or grayscale PNG — the palette index, not the colour the
//! palette maps it to. The palette is never applied, so the value the plane
//! carries is the image's own sample.
//!
//! Supported `image`-crate formats are the crate's enabled feature list
//! (png, jpeg, webp, bmp, gif, tiff, tga, dds, pnm, ico, qoi, hdr, exr,
//! ff). AVIF/pvr/jxr are deliberately absent.

use std::io::Cursor;

use api::error::MediaError;
use api::media::{DecodedImage, ImageInfo};
use image::{DynamicImage, ImageDecoder, ImageFormat};
use png::{BitDepth, ColorType as PngColorType, OutputInfo};

use crate::Media;
use crate::hint::extension_of;
use crate::tlg;

/// TLG spellings the router recognizes (extension without the dot).
const TLG_EXTENSIONS: [&str; 3] = ["tlg", "tlg5", "tlg6"];

/// The PNG file signature.
const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

/// Image format names advertised by [`crate::capabilities`].
pub(crate) const IMAGE_FORMATS: [&str; 16] = [
    "tlg5", "tlg6", "png", "jpeg", "webp", "bmp", "gif", "tiff", "tga", "dds", "pnm", "ico", "qoi",
    "hdr", "exr", "ff",
];

/// Returns `true` when the extension hint routes to the native TLG decoder.
///
/// Accepts a full storage name (`"frm/bg01.tlg5"`) or a bare extension
/// (`"tlg6"` / `".tlg6"`), so callers may pass whichever they have.
fn is_tlg_hint(hint: Option<&str>) -> bool {
    hint.and_then(extension_of).is_some_and(|ext| {
        TLG_EXTENSIONS
            .iter()
            .any(|tlg| ext.eq_ignore_ascii_case(tlg))
    })
}

/// The `image` crate format implied by an extension hint, mapping the extra
/// spellings (`.jif` → JPEG, `.dib` → BMP) onto their decoders. TLG
/// spellings return `None` because they are routed separately.
fn format_for_hint(hint: Option<&str>) -> Option<ImageFormat> {
    let ext = extension_of(hint?)?;
    if ext.eq_ignore_ascii_case("jif") {
        return Some(ImageFormat::Jpeg);
    }
    if ext.eq_ignore_ascii_case("dib") {
        return Some(ImageFormat::Bmp);
    }
    if TLG_EXTENSIONS
        .iter()
        .any(|tlg| ext.eq_ignore_ascii_case(tlg))
    {
        return None;
    }
    ImageFormat::from_extension(ext)
}

/// Stable, lowercase name for an `image` crate format.
fn format_name(format: ImageFormat) -> String {
    format
        .extensions_str()
        .first()
        .map(|name| (*name).to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

/// The typed decode error used for every TLG failure.
fn tlg_error(message: String) -> MediaError {
    MediaError::Decode {
        format: "tlg".to_string(),
        message,
    }
}

/// Decodes a TLG payload into a `DecodedImage`, dropping the alpha
/// descriptor (the DTO does not carry it; use [`probe_image`] for that).
fn decode_tlg_bytes(bytes: &[u8]) -> Result<DecodedImage, String> {
    tlg::decode_tlg_with_info(bytes).map(|(image, _has_alpha)| {
        let (width, height) = (image.width(), image.height());
        DecodedImage::new(width, height, image.into_raw())
    })
}

/// Converts an `image`-crate dynamic image to the RGBA8 DTO.
fn decoded_from_dynamic(image: DynamicImage) -> DecodedImage {
    let rgba = image.into_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    DecodedImage::new(width, height, rgba.into_raw())
}

/// Probes an image header for its dimensions, alpha presence and format
/// without decoding the pixel data.
///
/// `hint` is the storage name or extension the caller matched the bytes by;
/// it only steers format selection and is never trusted over the content.
///
/// # Errors
///
/// [`MediaError::Decode`] when the bytes are not a recognizable image, and
/// [`MediaError::Unsupported`] when the detected format has no header probe in
/// this build.
pub fn probe_image(m: &Media, bytes: &[u8], hint: Option<&str>) -> Result<ImageInfo, MediaError> {
    let _ = m;

    if is_tlg_hint(hint) {
        match tlg::probe_tlg(bytes) {
            Ok(header) => return Ok(probe_info_from_tlg(header)),
            Err(message) if tlg::has_tlg_magic(bytes) => return Err(tlg_error(message)),
            // A mislabeled file (e.g. `x.tlg` holding a PNG): fall through.
            Err(_) => {}
        }
    }

    if tlg::has_tlg_magic(bytes) {
        let header = tlg::probe_tlg(bytes).map_err(tlg_error)?;
        return Ok(probe_info_from_tlg(header));
    }

    let mut last_error = None;
    if let Some(format) = format_for_hint(hint) {
        match probe_with_format(bytes, format) {
            Ok(info) => return Ok(info),
            Err(message) => last_error = Some(message),
        }
    }

    if let Ok(format) = image::guess_format(bytes) {
        match probe_with_format(bytes, format) {
            Ok(info) => return Ok(info),
            Err(message) => last_error = Some(message),
        }
    }

    Err(MediaError::Decode {
        format: String::new(),
        message: last_error.unwrap_or_else(|| {
            "not a recognized image (no extension hint and no known magic bytes)".to_string()
        }),
    })
}

/// Builds an [`ImageInfo`] from a parsed TLG header.
fn probe_info_from_tlg(header: tlg::TlgHeader) -> ImageInfo {
    ImageInfo {
        width: header.width,
        height: header.height,
        has_alpha: header.has_alpha,
        format: Some(header.format.to_string()),
    }
}

/// Probes a specific `image`-crate format without decoding pixel data.
fn probe_with_format(bytes: &[u8], format: ImageFormat) -> Result<ImageInfo, String> {
    let decoder = image::ImageReader::with_format(Cursor::new(bytes), format)
        .into_decoder()
        .map_err(|error| error.to_string())?;
    let (width, height) = decoder.dimensions();
    Ok(ImageInfo {
        width,
        height,
        has_alpha: decoder.color_type().has_alpha(),
        format: Some(format_name(format)),
    })
}

/// Decodes a specific `image`-crate format.
fn decode_with_format(bytes: &[u8], format: ImageFormat) -> Result<DynamicImage, String> {
    image::load_from_memory_with_format(bytes, format).map_err(|error| error.to_string())
}

/// Decodes the 8-bit index plane of a palettized or grayscale PNG.
///
/// The plane is the image's own 8-bit sample: the palette index of a
/// palettized image, the gray level of a grayscale image. A 1-, 2- or 4-bit
/// sample is expanded to the full 8-bit range, a 16-bit sample keeps its high
/// byte. The palette itself is never applied — the index is the value.
///
/// The plane travels as a gray image: every pixel's red, green and blue
/// channels carry the plane value and the alpha is opaque. A consumer that
/// reads the red channel therefore reads the plane.
///
/// # Errors
///
/// [`MediaError::Unsupported`] when the bytes are not a PNG, or when the PNG
/// carries colours that have no 8-bit plane (a truecolour image, an image
/// with an alpha channel), and [`MediaError::Decode`] when the PNG is
/// malformed.
pub fn decode_index_plane(
    m: &Media,
    bytes: &[u8],
    hint: Option<&str>,
) -> Result<DecodedImage, MediaError> {
    let _ = m;

    if !bytes.starts_with(&PNG_SIGNATURE) {
        return Err(MediaError::Unsupported {
            message: match hint {
                Some(name) => format!(
                    "{name} is not a PNG; only a palettized or a grayscale PNG carries an 8-bit index plane"
                ),
                None => {
                    "only a palettized or a grayscale PNG carries an 8-bit index plane".to_string()
                }
            },
        });
    }

    let mut decoder = png::Decoder::new(Cursor::new(bytes));
    // No transformation may run: expanding the palette to RGB would throw
    // the index away, and the index is the plane.
    decoder.set_transformations(png::Transformations::IDENTITY);
    let mut reader = decoder.read_info().map_err(png_decode_error)?;
    let size = reader
        .output_buffer_size()
        .ok_or_else(|| MediaError::Decode {
            format: "png".to_string(),
            message: "the image is too large to decode".to_string(),
        })?;
    let mut buffer = Vec::new();
    buffer
        .try_reserve_exact(size)
        .map_err(|_| MediaError::Decode {
            format: "png".to_string(),
            message: "the image is too large to decode".to_string(),
        })?;
    buffer.resize(size, 0);
    let output = reader.next_frame(&mut buffer).map_err(png_decode_error)?;
    let plane = expand_plane(&output, &buffer)?;

    let (width, height) = (output.width, output.height);
    let rgba_len = plane
        .len()
        .checked_mul(4)
        .ok_or_else(|| MediaError::Decode {
            format: "png".to_string(),
            message: "the image size is not representable".to_string(),
        })?;
    let mut rgba = Vec::new();
    rgba.try_reserve_exact(rgba_len)
        .map_err(|_| MediaError::Decode {
            format: "png".to_string(),
            message: "the image is too large to decode".to_string(),
        })?;
    for value in plane {
        rgba.extend_from_slice(&[value, value, value, 255]);
    }
    Ok(DecodedImage::new(width, height, rgba))
}

/// The typed decode error of a PNG failure.
fn png_decode_error(error: png::DecodingError) -> MediaError {
    MediaError::Decode {
        format: "png".to_string(),
        message: error.to_string(),
    }
}

/// Expands the decoded rows of a palettized or grayscale PNG into one 8-bit
/// plane value per pixel.
///
/// A sub-byte sample is packed most significant first: the first sample of a
/// byte sits in its high bits. A grayscale sample is scaled to the full
/// 8-bit range, a palette index is not.
fn expand_plane(output: &OutputInfo, buffer: &[u8]) -> Result<Vec<u8>, MediaError> {
    let width = usize::try_from(output.width).map_err(|_| MediaError::Decode {
        format: "png".to_string(),
        message: "the image width is not representable".to_string(),
    })?;
    let height = usize::try_from(output.height).map_err(|_| MediaError::Decode {
        format: "png".to_string(),
        message: "the image height is not representable".to_string(),
    })?;
    let count = width
        .checked_mul(height)
        .ok_or_else(|| MediaError::Decode {
            format: "png".to_string(),
            message: "the image size is not representable".to_string(),
        })?;
    let mut plane = Vec::new();
    plane
        .try_reserve_exact(count)
        .map_err(|_| MediaError::Decode {
            format: "png".to_string(),
            message: "the image is too large to decode".to_string(),
        })?;
    plane.resize(count, 0);

    let line_size = output.line_size;
    if !matches!(
        output.color_type,
        PngColorType::Indexed | PngColorType::Grayscale
    ) {
        return Err(no_index_plane());
    }
    for y in 0..height {
        let row = buffer
            .get(y * line_size..(y + 1) * line_size)
            .ok_or_else(|| MediaError::Decode {
                format: "png".to_string(),
                message: "the image rows are shorter than its size".to_string(),
            })?;
        let start = y * width;
        match output.color_type {
            PngColorType::Indexed => match output.bit_depth {
                BitDepth::Eight => {
                    for x in 0..width {
                        plane[start + x] = row_byte(row, x)?;
                    }
                }
                BitDepth::One | BitDepth::Two | BitDepth::Four => {
                    let bits = packed_bits(output.bit_depth);
                    let per_byte = 8 / bits;
                    let mask = ((1u32 << bits) - 1) as u8;
                    for x in 0..width {
                        let byte = row_byte(row, x / per_byte)?;
                        let shift = 8 - bits * (x % per_byte + 1);
                        plane[start + x] = (byte >> shift) & mask;
                    }
                }
                BitDepth::Sixteen => {
                    return Err(MediaError::Unsupported {
                        message: "a 16-bit palette index is not a PNG".to_string(),
                    });
                }
            },
            PngColorType::Grayscale => match output.bit_depth {
                BitDepth::Eight => {
                    for x in 0..width {
                        plane[start + x] = row_byte(row, x)?;
                    }
                }
                BitDepth::Sixteen => {
                    for x in 0..width {
                        let pair = row.get(x * 2..x * 2 + 2).ok_or_else(row_too_short)?;
                        plane[start + x] = pair[0];
                    }
                }
                BitDepth::One | BitDepth::Two | BitDepth::Four => {
                    let bits = packed_bits(output.bit_depth);
                    let per_byte = 8 / bits;
                    let max = (1u32 << bits) - 1;
                    for x in 0..width {
                        let byte = row_byte(row, x / per_byte)?;
                        let shift = 8 - bits * (x % per_byte + 1);
                        let value = u32::from(byte >> shift) & max;
                        plane[start + x] = ((value * 255) / max) as u8;
                    }
                }
            },
            _ => {
                return Err(no_index_plane());
            }
        }
    }
    Ok(plane)
}

/// The typed error of an image that has no 8-bit index plane.
fn no_index_plane() -> MediaError {
    MediaError::Unsupported {
        message:
            "the image has no 8-bit index plane: only a palettized or a grayscale image carries one"
                .to_string(),
    }
}

/// The number of bits one packed sample holds.
fn packed_bits(depth: BitDepth) -> usize {
    match depth {
        BitDepth::One => 1,
        BitDepth::Two => 2,
        BitDepth::Four => 4,
        BitDepth::Eight => 8,
        BitDepth::Sixteen => 16,
    }
}

/// Reads one byte of a scanline.
fn row_byte(row: &[u8], index: usize) -> Result<u8, MediaError> {
    row.get(index).copied().ok_or_else(row_too_short)
}

/// The typed error of a scanline that is shorter than the image's size.
fn row_too_short() -> MediaError {
    MediaError::Decode {
        format: "png".to_string(),
        message: "the image rows are shorter than its size".to_string(),
    }
}

/// Decodes image bytes into tightly packed RGBA8.
///
/// Dispatch follows the [[`format_for_hint`]] router (see the module docs).
/// `hint` is the storage name or extension the caller matched the bytes by
/// and only steers format selection; content always wins when the hint is
/// wrong.
///
/// # Errors
///
/// [`MediaError::Decode`] when the bytes are not a decodable image,
/// [`MediaError::InvalidData`] when the decoded size is not representable, and
/// [`MediaError::Unsupported`] when the detected format has no decoder in
/// this build. A failed decode is never a placeholder image.
pub fn decode_image(
    m: &Media,
    bytes: &[u8],
    hint: Option<&str>,
) -> Result<DecodedImage, MediaError> {
    let _ = m;

    if is_tlg_hint(hint) {
        match decode_tlg_bytes(bytes) {
            Ok(image) => return Ok(image),
            Err(message) if tlg::has_tlg_magic(bytes) => return Err(tlg_error(message)),
            // A mislabeled file: fall through to content detection.
            Err(_) => {}
        }
    }

    let mut last_error: Option<String> = None;
    if let Some(format) = format_for_hint(hint) {
        match decode_with_format(bytes, format) {
            Ok(image) => return Ok(decoded_from_dynamic(image)),
            Err(error) => last_error = Some(error),
        }
    }

    if tlg::has_tlg_magic(bytes) {
        return decode_tlg_bytes(bytes).map_err(tlg_error);
    }

    match image::guess_format(bytes) {
        Ok(format) => match decode_with_format(bytes, format) {
            Ok(image) => Ok(decoded_from_dynamic(image)),
            Err(error) => Err(MediaError::Decode {
                format: format_name(format),
                message: error,
            }),
        },
        Err(error) => Err(MediaError::Decode {
            format: String::new(),
            message: last_error.unwrap_or_else(|| error.to_string()),
        }),
    }
}
