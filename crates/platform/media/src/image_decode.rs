//! Image format dispatch and header probing.
//!
//! This is the `media` replacement for
//! `tvp-visual/src/bitmap.rs::decode_image`. It keeps the reference router's
//! effective order:
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
//! Supported `image`-crate formats are the crate's enabled feature list
//! (png, jpeg, webp, bmp, gif, tiff, tga, dds, pnm, ico, qoi, hdr, exr,
//! ff). AVIF/pvr/jxr are deliberately absent, matching the reference
//! migration note.

use std::io::Cursor;

use api::error::MediaError;
use api::media::{DecodedImage, ImageInfo};
use image::{DynamicImage, ImageDecoder, ImageFormat};

use crate::Media;
use crate::hint::extension_of;
use crate::tlg;

/// TLG spellings the reference registers (extension without the dot).
const TLG_EXTENSIONS: [&str; 3] = ["tlg", "tlg5", "tlg6"];

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

/// The `image` crate format implied by an extension hint, mapping the
/// reference's extra spellings (`.jif` → JPEG, `.dib` → BMP) onto their
/// decoders. TLG spellings return `None` because they are routed separately.
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
/// `hint` is the storage name or extension the caller used to find the
/// bytes; it only steers format selection and is never trusted over the
/// content. Returns [`MediaError::Decode`] when the bytes are not a
/// recognizable image.
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

/// Decodes image bytes into tightly packed RGBA8.
///
/// Dispatch matches the reference [[`format_for_hint`]] router (see the
/// module docs). `hint` is the storage name or extension the caller used and
/// only steers format selection; content always wins when the hint is
/// wrong. A failed decode is a real [`MediaError::Decode`], never a
/// placeholder image.
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
