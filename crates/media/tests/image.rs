//! Integration tests for the image format router (`probe_image`/`decode_image`).

mod common;

use api::error::MediaError;
use common::{encode_png, pattern, test_image};
use media::{Media, decode_image, probe_image};

/// Encodes an RGBA8 buffer as JPEG in memory.
fn encode_jpeg(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    use image::ImageEncoder;
    // The JPEG encoder accepts RGB (not RGBA); drop the alpha channel.
    let mut rgb = Vec::with_capacity((width as usize * height as usize) * 3);
    for px in rgba.chunks_exact(4) {
        rgb.extend_from_slice(&px[..3]);
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new(&mut out)
        .write_image(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .expect("jpeg encode");
    out
}

/// Encodes an RGBA8 buffer as BMP in memory.
fn encode_bmp(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    use image::ImageEncoder;
    let mut out = Vec::new();
    image::codecs::bmp::BmpEncoder::new(&mut out)
        .write_image(rgba, width, height, image::ExtendedColorType::Rgba8)
        .expect("bmp encode");
    out
}

#[test]
fn reference_extension_spellings_decode() {
    let media = Media::new();
    let (width, height) = (6u32, 4u32);
    let rgba = pattern(width, height);

    let png = encode_png(width, height, &rgba);
    for hint in [Some("a.png"), Some("png"), Some(".png")] {
        let decoded = decode_image(&media, &png, hint).expect("png hint");
        assert_eq!(
            (decoded.width, decoded.height),
            (width, height),
            "hint {hint:?}"
        );
    }

    // The reference registers extra JPEG/BMP spellings (`.jif`/`.dib`).
    let jpeg = encode_jpeg(width, height, &rgba);
    for hint in [Some("a.jpg"), Some("a.jif")] {
        let decoded = decode_image(&media, &jpeg, hint).expect("jpeg hint");
        assert_eq!(
            (decoded.width, decoded.height),
            (width, height),
            "hint {hint:?}"
        );
    }

    let bmp = encode_bmp(width, height, &rgba);
    for hint in [Some("a.bmp"), Some("a.dib")] {
        let decoded = decode_image(&media, &bmp, hint).expect("bmp hint");
        assert_eq!(
            (decoded.width, decoded.height),
            (width, height),
            "hint {hint:?}"
        );
    }
}

#[test]
fn tlg_hints_route_to_the_native_decoder() {
    let media = Media::new();
    let (width, height) = (13usize, 17usize);
    let img = test_image(width, height);
    let tlg = common::encode_tlg6(&img, width, height, 4);

    for hint in [Some("frm/bg01.tlg"), Some("bg.tlg5"), Some("tlg6")] {
        let info = probe_image(&media, &tlg, hint).expect("tlg hint");
        assert_eq!(
            (info.width, info.height),
            (width as u32, height as u32),
            "hint {hint:?}"
        );
        assert_eq!(info.format.as_deref(), Some("tlg6"), "hint {hint:?}");
    }

    // A `.png` hint on real PNG bytes does not route to the TLG decoder.
    let png = encode_png(2, 2, &pattern(2, 2));
    let info = probe_image(&media, &png, Some("bg.png")).expect("png hint");
    assert_eq!(info.format.as_deref(), Some("png"));
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
    // Synthetic (truncated) TLG6 header. The router must offer it to the
    // native decoder despite the `.png` hint.
    let (width, height) = (8usize, 8usize);
    let mut raw = Vec::new();
    raw.extend_from_slice(b"TLG6.0\x00raw\x1a");
    raw.push(4); // 4 channels (alpha)
    raw.extend_from_slice(&[0, 0, 0]);
    raw.extend_from_slice(&(width as i32).to_le_bytes());
    raw.extend_from_slice(&(height as i32).to_le_bytes());

    let media = Media::new();
    let result = decode_image(&media, &raw, Some("mislabeled.png"));
    assert!(matches!(result, Err(MediaError::Decode { .. })));

    // And a correct TLG hint on non-TLG bytes falls through to normal
    // detection instead of failing as a corrupt TLG.
    let png = encode_png(2, 2, &pattern(2, 2));
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

#[test]
fn large_images_decode_without_a_configured_size_cap() {
    // A 40000-pixel-wide PNG (well past any arbitrary safety cap) must decode:
    // the engine has no size policy and reports errors instead of faking data.
    let width = 40_000u32;
    let rgba = pattern(width, 1);
    let png = encode_png(width, 1, &rgba);
    let media = Media::new();
    let decoded = decode_image(&media, &png, Some("wide.png")).expect("large PNG must decode");
    assert_eq!((decoded.width, decoded.height), (width, 1));
}
