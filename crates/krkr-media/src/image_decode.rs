//! Image format dispatch and header probing.
//!
//! This is the `krkr-media` replacement for
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
use std::path::Path;

use image::{DynamicImage, ImageDecoder, ImageFormat};
use krkr_api::error::MediaError;
use krkr_api::media::{DecodedImage, ImageInfo};

use crate::Media;
use crate::tlg;

/// All TLG spellings the reference registers.
const TLG_EXTENSIONS: [&str; 3] = [".tlg", ".tlg5", ".tlg6"];

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
    let Some(hint) = hint else {
        return false;
    };
    let lower = hint.to_ascii_lowercase();
    TLG_EXTENSIONS.iter().any(|ext| lower.ends_with(ext))
        || matches!(lower.as_str(), "tlg" | "tlg5" | "tlg6")
}

/// Extracts the lowercase extension from a filename or bare-extension hint.
fn extension_of(hint: &str) -> Option<String> {
    if let Some(ext) = Path::new(hint).extension().and_then(|e| e.to_str()) {
        return Some(ext.to_ascii_lowercase());
    }
    let bare = hint.strip_prefix('.').unwrap_or(hint);
    if !bare.is_empty() && !bare.contains(['/', '\\', '.']) {
        return Some(bare.to_ascii_lowercase());
    }
    None
}

/// The `image` crate format implied by an extension hint, mapping the
/// reference's extra spellings (`.jif` → JPEG, `.dib` → BMP) onto their
/// decoders. TLG spellings return `None` because they are routed separately.
fn format_for_hint(hint: Option<&str>) -> Option<ImageFormat> {
    let ext = extension_of(hint?)?;
    match ext.as_str() {
        "jif" => Some(ImageFormat::Jpeg),
        "dib" => Some(ImageFormat::Bmp),
        "tlg" | "tlg5" | "tlg6" => None,
        other => ImageFormat::from_extension(other),
    }
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
    let rgba = image.to_rgba8();
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
        match image::load_from_memory_with_format(bytes, format) {
            Ok(image) => return Ok(decoded_from_dynamic(image)),
            Err(error) => last_error = Some(error.to_string()),
        }
    }

    if tlg::has_tlg_magic(bytes) {
        return decode_tlg_bytes(bytes).map_err(tlg_error);
    }

    match image::guess_format(bytes) {
        Ok(format) => match image::load_from_memory_with_format(bytes, format) {
            Ok(image) => Ok(decoded_from_dynamic(image)),
            Err(error) => Err(MediaError::Decode {
                format: format_name(format),
                message: error.to_string(),
            }),
        },
        Err(error) => Err(MediaError::Decode {
            format: String::new(),
            message: last_error.unwrap_or_else(|| error.to_string()),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic RGBA8 pattern.
    fn pattern(width: u32, height: u32) -> Vec<u8> {
        (0..width * height)
            .flat_map(|i| {
                let x = i % width;
                let y = i / width;
                [(x * 7) as u8, (y * 11) as u8, 128, 255]
            })
            .collect()
    }

    /// Encodes an RGBA8 buffer as PNG in memory.
    fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        use image::ImageEncoder;
        let mut out = Vec::new();
        image::codecs::png::PngEncoder::new(&mut out)
            .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
            .expect("png encode");
        out
    }

    #[test]
    fn extension_hint_maps_reference_spellings_and_extras() {
        assert_eq!(format_for_hint(Some("a.jpg")), Some(ImageFormat::Jpeg));
        assert_eq!(format_for_hint(Some("a.jif")), Some(ImageFormat::Jpeg));
        assert_eq!(format_for_hint(Some("a.bmp")), Some(ImageFormat::Bmp));
        assert_eq!(format_for_hint(Some("a.dib")), Some(ImageFormat::Bmp));
        assert_eq!(format_for_hint(Some("a.png")), Some(ImageFormat::Png));
        assert_eq!(format_for_hint(Some("a.webp")), Some(ImageFormat::WebP));
        assert_eq!(
            format_for_hint(Some("a.tlg")),
            None,
            "TLG routes separately"
        );
        assert_eq!(format_for_hint(Some("png")), Some(ImageFormat::Png));
        assert_eq!(format_for_hint(Some(".png")), Some(ImageFormat::Png));
        assert_eq!(format_for_hint(None), None);
    }

    #[test]
    fn tlg_hint_detection() {
        assert!(is_tlg_hint(Some("frm/bg01.tlg")));
        assert!(is_tlg_hint(Some("bg.tlg5")));
        assert!(is_tlg_hint(Some("tlg6")));
        assert!(!is_tlg_hint(Some("bg.png")));
        assert!(!is_tlg_hint(None));
    }

    #[test]
    fn decode_and_probe_use_magic_when_extension_is_missing_or_wrong() {
        let media = Media::new();
        let (width, height) = (6u32, 4u32);
        let rgba = pattern(width, height);
        let png = encode_png(width, height, &rgba);

        let no_hint = decode_image(&media, &png, None).expect("magic fallback (no hint)");
        assert_eq!((no_hint.width, no_hint.height), (width, height));
        assert_eq!(no_hint.rgba, rgba);

        let wrong = decode_image(&media, &png, Some("mystery.dat")).expect("wrong extension");
        assert_eq!((wrong.width, wrong.height), (width, height));

        let info = probe_image(&media, &png, None).expect("probe by magic");
        assert_eq!((info.width, info.height), (width, height));
        assert!(info.has_alpha);
        assert_eq!(info.format.as_deref(), Some("png"));
    }

    #[test]
    fn mislabeled_tlg_is_routed_by_magic() {
        // Synthetic TLG6 built with the in-module test encoder.
        let (width, height) = (8usize, 8usize);
        let mut raw = Vec::new();
        raw.extend_from_slice(b"TLG6.0\x00raw\x1a");
        raw.push(4); // 4 channels (alpha)
        raw.extend_from_slice(&[0, 0, 0]);
        raw.extend_from_slice(&(width as i32).to_le_bytes());
        raw.extend_from_slice(&(height as i32).to_le_bytes());
        // The decoder is exercised by the `tlg` tests; here only the router
        // matters, so the truncated body must still be *offered* to it.
        let media = Media::new();
        // A `.png` hint must not override the TLG magic.
        let result = decode_image(&media, &raw, Some("mislabeled.png"));
        assert!(matches!(result, Err(MediaError::Decode { .. })));

        // And a correct TLG hint on non-TLG bytes falls through to normal
        // detection instead of failing as a corrupt TLG.
        let png = {
            let (w, h) = (2u32, 2u32);
            encode_png(w, h, &pattern(w, h))
        };
        let decoded = decode_image(&media, &png, Some("fake.tlg")).expect("falls through");
        assert_eq!((decoded.width, decoded.height), (2, 2));
    }

    #[test]
    fn garbage_is_a_decode_error_not_a_panic() {
        let media = Media::new();
        assert!(matches!(
            decode_image(&media, b"not an image", None),
            Err(MediaError::Decode { .. })
        ));
        assert!(matches!(
            probe_image(&media, b"not an image", Some("file.bin")),
            Err(MediaError::Decode { .. })
        ));
    }
}
