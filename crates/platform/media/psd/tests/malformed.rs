//! Malformed containers surface a typed error.
//!
//! A damaged stream must never produce a partial document, a zero-filled pixel
//! buffer or a panic: every failure is one of the crate's error enums.

use psd::container;
use psd::decode;
use psd::error::{PsbError, PsdError};
use psd::parse;

/// A 26-byte header whose signature is not `8BPS`
/// (`reference/cpp/plugins/psdfile/psdparse/psdparse.h:211-219`).
#[test]
fn wrong_psd_signature_is_invalid() {
    let mut data = Vec::from(*b"NOPE");
    data.extend_from_slice(&[0u8; 22]);
    assert!(matches!(
        parse::parse(&data),
        Err(PsdError::Invalid(message)) if message.contains("signature")
    ));
}

/// The header itself is fixed-size, so a shorter stream is a truncation
/// (`reference/cpp/plugins/psdfile/psdparse/psdparse.h:211-219`).
#[test]
fn short_psd_header_is_a_truncation() {
    assert!(matches!(parse::parse(b""), Err(PsdError::Eof)));
    assert!(matches!(parse::parse(b"8BPS\0\0"), Err(PsdError::Eof)));
}

/// A layer/mask section that claims more bytes than the file carries is a
/// truncation, not an empty layer list
/// (`reference/cpp/plugins/psdfile/psdparse/psdparse.h:704-711`).
#[test]
fn truncated_layer_section_is_a_truncation() {
    let data = minimal_psd(&[0, 0, 0, 100, 1, 2, 3, 4, 5, 6]);
    assert!(matches!(parse::parse(&data), Err(PsdError::Eof)));
}

/// An image resource block must start with `8BIM`
/// (`reference/cpp/plugins/psdfile/psdparse/psdparse.h:251-260`).
#[test]
fn bad_image_resource_signature_is_invalid() {
    let mut data = psd_header(1, 1, 3, 8, 3);
    data.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
    data.extend_from_slice(&4u32.to_be_bytes()); // image resources
    data.extend_from_slice(b"XXXX");
    assert!(matches!(
        parse::parse(&data),
        Err(PsdError::Invalid(message)) if message.contains("image resource")
    ));
}

/// A merged image whose channels are shorter than the declared size is a
/// decompression error rather than a zero-filled buffer
/// (`reference/cpp/plugins/psdfile/psdparse/psdimage.cpp:1017`,
/// `getMergedImage`).
#[test]
fn truncated_merged_image_is_a_decompression_error() {
    let mut data = psd_header(2, 1, 3, 8, 3);
    data.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
    data.extend_from_slice(&0u32.to_be_bytes()); // image resources
    data.extend_from_slice(&0u32.to_be_bytes()); // layer and mask section
    data.extend_from_slice(&0u16.to_be_bytes()); // compression: raw
    data.extend_from_slice(&[0u8; 3]); // three of the six required bytes
    let file = parse::parse(&data).expect("the header parses");
    assert!(matches!(
        decode::merged_image(&file),
        Err(PsdError::Decompress(_))
    ));
}

/// A PSB stream shorter than the nine-byte header check is a truncation
/// (`reference/cpp/plugins/psbfile/PSBStreamDecompress.cpp:38-46`).
#[test]
fn short_psb_stream_is_a_truncation() {
    assert!(matches!(
        container::parse_stream(b"PSB", 0),
        Err(PsbError::Truncated(_))
    ));
}

/// An unknown PSB magic is a signature error
/// (`reference/cpp/plugins/psbfile/PSBHeader.h:58-65`).
#[test]
fn unknown_psb_magic_is_a_signature_error() {
    assert!(matches!(
        container::parse_stream(b"not a psb container at all", 0),
        Err(PsbError::BadSignature(_))
    ));
}

/// An MDF archive is not a PSB document and says so
/// (`reference/cpp/plugins/psbfile/PSBHeader.h:58-61`).
#[test]
fn psb_archive_magic_is_rejected() {
    let mut data = Vec::from(*b"MDF\0");
    data.extend_from_slice(&[0u8; 20]);
    assert!(matches!(
        container::parse(data, 0),
        Err(PsbError::ArchiveNotPsb)
    ));
}

/// A PSB header that claims an unsupported version is rejected with the
/// version it carries (`reference/cpp/plugins/psbfile/PSBFile.cpp:440-441`).
#[test]
fn unsupported_psb_version_is_reported() {
    let mut data = Vec::new();
    data.extend_from_slice(b"PSB\0");
    data.extend_from_slice(&4u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&40u32.to_le_bytes());
    data.extend_from_slice(&40u32.to_le_bytes());
    data.extend_from_slice(&[0u8; 40]);
    assert!(matches!(
        container::parse(data, 0),
        Err(PsbError::UnsupportedVersion(4))
    ));
}

/// A PSD header with no colour mode, resource or layer content.
fn psd_header(channels: u16, height: i32, width: i32, depth: u16, mode: u16) -> Vec<u8> {
    let mut data = Vec::new();
    data.extend_from_slice(b"8BPS");
    data.extend_from_slice(&1u16.to_be_bytes());
    data.extend_from_slice(&[0u8; 6]);
    data.extend_from_slice(&channels.to_be_bytes());
    data.extend_from_slice(&height.to_be_bytes());
    data.extend_from_slice(&width.to_be_bytes());
    data.extend_from_slice(&depth.to_be_bytes());
    data.extend_from_slice(&mode.to_be_bytes());
    data
}

/// A PSD header followed by the given bytes in place of the layer section.
fn minimal_psd(layer_section: &[u8]) -> Vec<u8> {
    let mut data = psd_header(3, 1, 1, 8, 3);
    data.extend_from_slice(&0u32.to_be_bytes()); // colour mode data
    data.extend_from_slice(&0u32.to_be_bytes()); // image resources
    let declared: u32 = 100;
    data.extend_from_slice(&declared.to_be_bytes());
    data.extend_from_slice(layer_section);
    data
}
