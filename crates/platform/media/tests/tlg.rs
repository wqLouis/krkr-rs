//! Integration tests for the native TLG5/TLG6 decoder.
//!
//! The `tlg` module is crate-private (implementation detail of the image
//! dispatcher), so these tests exercise it through the public
//! `decode_image`/`probe_image` router.

use media::{Media, decode_image, probe_image};
use testkit::{SDS_MAGIC, TLG6_MAGIC, encode_tlg5, encode_tlg6, test_image};

// ------------------------------------------------------------------
// Header probe + SDS wrapper. No on-disk fixtures are needed: the SDS
// container is assembled in memory around a synthetic TLG6 payload.
// ------------------------------------------------------------------

#[test]
fn probe_reports_tlg6_header_without_decoding() {
    let (w, h) = (13usize, 17usize);
    let img = test_image(w, h);
    let encoded = encode_tlg6(&img, w, h, 4);
    let media = Media::new();
    let header = probe_image(&media, &encoded, None).expect("synthetic TLG6 header");
    assert_eq!((header.width, header.height), (w as u32, h as u32));
    assert!(header.has_alpha);
    assert_eq!(header.format.as_deref(), Some("tlg6"));
}

#[test]
fn sds_wrapper_and_raw_payload_are_equivalent() {
    let (w, h) = (16usize, 16usize);
    let img = test_image(w, h);
    let raw = encode_tlg6(&img, w, h, 4);
    // Build the TLG0.0 sds wrapper: magic + raw length + raw payload.
    let mut wrapped = Vec::new();
    wrapped.extend_from_slice(&SDS_MAGIC);
    wrapped.extend_from_slice(&(raw.len() as i32).to_le_bytes());
    wrapped.extend_from_slice(&raw);
    let media = Media::new();
    assert!(
        probe_image(&media, &wrapped, None).is_ok(),
        "SDS wrapper must be detected as a TLG image"
    );
    let img_wrapped = decode_image(&media, &wrapped, None).unwrap();
    let img_raw = decode_image(&media, &raw, None).unwrap();
    assert_eq!(img_wrapped.rgba, img_raw.rgba);
}

// ------------------------------------------------------------------
// Synthetic round trips (encoder + decoder)
// ------------------------------------------------------------------

#[test]
fn tlg6_rgba_roundtrip_all_filters() {
    let (w, h) = (64usize, 32usize);
    let img = test_image(w, h);
    let encoded = encode_tlg6(&img, w, h, 4);
    let media = Media::new();
    let decoded = decode_image(&media, &encoded, None).expect("synthetic TLG6 should decode");
    assert_eq!((decoded.width as usize, decoded.height as usize), (w, h));
    assert_eq!(decoded.rgba, img, "TLG6 round trip must be lossless");
}

#[test]
fn tlg6_rgb_roundtrip_all_filters() {
    let (w, h) = (40usize, 24usize);
    let mut img = test_image(w, h);
    for px in img.as_chunks_mut::<4>().0 {
        px[3] = 0xff; // opaque 24-bit image
    }
    let encoded = encode_tlg6(&img, w, h, 3);
    let media = Media::new();
    let decoded =
        decode_image(&media, &encoded, None).expect("synthetic 24-bit TLG6 should decode");
    assert_eq!(decoded.rgba, img, "24-bit TLG6 round trip must be lossless");
}

#[test]
fn tlg6_narrow_width_roundtrip() {
    // width not a multiple of 8 (exercises the fractional block path)
    let (w, h) = (13usize, 17usize);
    let img = test_image(w, h);
    let encoded = encode_tlg6(&img, w, h, 4);
    let media = Media::new();
    let decoded = decode_image(&media, &encoded, None).expect("narrow TLG6 should decode");
    assert_eq!(decoded.rgba, img, "narrow TLG6 round trip must be lossless");
}

#[test]
fn tlg6_multi_band_partial_roundtrip() {
    // Taller than one 8-row block row with a remainder, and a width that is
    // not a multiple of 8, so the reused per-band scratch crosses the
    // block-row boundary.
    let (w, h) = (29usize, 37usize);
    let img = test_image(w, h);
    let encoded = encode_tlg6(&img, w, h, 4);
    let media = Media::new();
    let decoded = decode_image(&media, &encoded, None).expect("multi-band TLG6 should decode");
    assert_eq!((decoded.width as usize, decoded.height as usize), (w, h));
    assert_eq!(
        decoded.rgba, img,
        "multi-band TLG6 round trip must be lossless"
    );
}

#[test]
fn tlg6_multi_band_partial_rgb_roundtrip() {
    // Same shape as above with the 24-bit color count, so the reused scratch
    // is checked on a channel count whose unused alpha lane stays zero.
    let (w, h) = (45usize, 21usize);
    let mut img = test_image(w, h);
    for px in img.as_chunks_mut::<4>().0 {
        px[3] = 0xff;
    }
    let encoded = encode_tlg6(&img, w, h, 3);
    let media = Media::new();
    let decoded = decode_image(&media, &encoded, None).expect("24-bit multi-band TLG6");
    assert_eq!(
        decoded.rgba, img,
        "24-bit multi-band round trip must be lossless"
    );
}

#[test]
fn tlg5_rgba_roundtrip() {
    let (w, h) = (24usize, 16usize);
    let img = test_image(w, h);
    let encoded = encode_tlg5(&img, w, h, 4, 4);
    let media = Media::new();
    let decoded = decode_image(&media, &encoded, None).expect("synthetic TLG5 should decode");
    assert_eq!((decoded.width as usize, decoded.height as usize), (w, h));
    assert_eq!(decoded.rgba, img, "TLG5 round trip must be lossless");
}

#[test]
fn tlg5_block_height_larger_than_the_image_roundtrip() {
    // A small icon can be encoded with a block height above its own height.
    // The reference decodes it as one clipped block; rejecting it makes valid
    // game assets unloadable.
    let (w, h) = (9usize, 7usize);
    let img = test_image(w, h);
    let encoded = encode_tlg5(&img, w, h, 4, 12);
    let media = Media::new();
    let decoded = decode_image(&media, &encoded, None)
        .expect("a block height larger than the image is legal TLG5");
    assert_eq!((decoded.width as usize, decoded.height as usize), (w, h));
    assert_eq!(
        decoded.rgba, img,
        "clipped-block TLG5 round trip must be lossless"
    );
}

#[test]
fn tlg5_rgb_roundtrip() {
    let (w, h) = (21usize, 19usize);
    let mut img = test_image(w, h);
    for px in img.as_chunks_mut::<4>().0 {
        px[3] = 0xff;
    }
    let encoded = encode_tlg5(&img, w, h, 3, 4);
    let media = Media::new();
    let decoded =
        decode_image(&media, &encoded, None).expect("synthetic 24-bit TLG5 should decode");
    assert_eq!(decoded.rgba, img, "24-bit TLG5 round trip must be lossless");
}

// ------------------------------------------------------------------
// Channel order: the decoded scanline word is 0xAARRGGBB, so byte 0 is
// blue and byte 2 is red.
// ------------------------------------------------------------------

/// Reads the little-endian `i32` at `at`.
fn read_i32(bytes: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

/// Builds a single-lane (grayscale) TLG6 file out of a four-lane one.
///
/// A file with fewer than three colour channels uses the identity colour
/// filter (`reference/cpp/core/visual/SaveTLG6.cpp:1185-1191`); the loader
/// decodes the single lane only and leaves the other three bytes of the
/// scanline word undefined (`reference/cpp/core/visual/LoadTLG.cpp:268-276`,
/// `:315-350`).
///
/// The four-lane file must hold a single block, so that its first block
/// uses the identity filter and its first lane is the whole image.
fn grayscale_tlg6(four_lane: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&four_lane[..11]); // the magic
    out.push(1); // one colour channel: the luminance
    out.extend_from_slice(&four_lane[12..15]); // data flag, colour type, golomb table
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(h as i32).to_le_bytes());
    out.extend_from_slice(&four_lane[23..27]); // the maximum bit length

    // The filter-type map: one identity filter per block. The LZSS stream
    // copies eight raw bytes per flag byte, so one flag byte covers the
    // single block of this image.
    let blocks = ((w - 1) / 8 + 1) * ((h - 1) / 8 + 1);
    assert!(
        blocks <= 8,
        "the raw LZSS copy writes eight bytes at a time"
    );
    let filter_stream = [0u8; 9];
    out.extend_from_slice(&(filter_stream.len() as i32).to_le_bytes());
    out.extend_from_slice(&filter_stream);

    // The residual lane: the first lane of the first block row.
    let mut at = 27;
    let filter_size = read_i32(four_lane, at);
    assert!(
        filter_size >= 0,
        "the filter-type size must not be negative"
    );
    at += 4 + filter_size as usize;
    let bit_length = read_i32(four_lane, at) as u32;
    let pool = (bit_length & 0x3fff_ffff).div_ceil(8) as usize;
    out.extend_from_slice(&four_lane[at..at + 4 + pool]);
    out
}

/// The colour channels of a decoded TLG6 image keep their places: the file's
/// first residual lane is the blue one and its third lane the red one
/// (`reference/cpp/core/visual/tvpgl.cpp:12431-12434` names the lanes of
/// the packed residual, `:12362-12368` places them in the scanline word, and
/// `reference/cpp/core/visual/LayerBitmapIntf.h:20-22` gives the scanline's
/// byte order).
#[test]
fn tlg6_channels_land_in_the_named_channels() {
    let (w, h) = (8usize, 8usize);
    let mut img = vec![0u8; w * h * 4];
    for (i, px) in img.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        px[0] = 10 + (i % 7) as u8; // red
        px[1] = 90 + (i % 5) as u8; // green
        px[2] = 170 + (i % 3) as u8; // blue
        px[3] = 200 + (i % 11) as u8; // alpha
    }
    let encoded = encode_tlg6(&img, w, h, 4);
    let media = Media::new();
    let decoded = decode_image(&media, &encoded, None).expect("a synthetic TLG6 should decode");
    for (x, (pixel, source)) in decoded
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(img.as_chunks::<4>().0.iter())
        .enumerate()
    {
        assert_eq!(
            pixel[0], source[0],
            "pixel {x}: the red stays in the red channel"
        );
        assert_eq!(
            pixel[1], source[1],
            "pixel {x}: the green stays in the green channel"
        );
        assert_eq!(
            pixel[2], source[2],
            "pixel {x}: the blue stays in the blue channel"
        );
        assert_eq!(
            pixel[3], source[3],
            "pixel {x}: the alpha stays in the alpha channel"
        );
    }
}

/// A one-channel TLG6 file stores the luminance in its single lane, which
/// the decoder reads as the image's blue byte
/// (`reference/cpp/core/visual/LoadTLG.cpp:315-350`: only the first lane is
/// decoded, and the zero line of a one-channel file is transparent black,
/// `:276`). The decoder presents it as a gray pixel with an opaque alpha.
#[test]
fn tlg6_grayscale_expands_the_single_lane() {
    let (w, h) = (8usize, 8usize);
    let img = test_image(w, h);
    let four_lane = encode_tlg6(&img, w, h, 4);
    let gray = grayscale_tlg6(&four_lane, w, h);
    let media = Media::new();
    let decoded = decode_image(&media, &gray, None).expect("a one-lane TLG6 should decode");
    assert_eq!((decoded.width as usize, decoded.height as usize), (w, h));
    for (x, (pixel, source)) in decoded
        .rgba
        .as_chunks::<4>()
        .0
        .iter()
        .zip(img.as_chunks::<4>().0.iter())
        .enumerate()
    {
        // The four-lane file keeps the blue channel in its first lane; that
        // lane is the luminance of the gray image.
        let luminance = source[2];
        assert_eq!(
            pixel,
            &[luminance, luminance, luminance, 0xff],
            "pixel {x} must be the gray expansion of the single lane"
        );
    }
}

// ------------------------------------------------------------------
// Error handling: garbage must return Err, never panic
// ------------------------------------------------------------------

#[test]
fn garbage_inputs_return_err() {
    let media = Media::new();
    let cases: Vec<Vec<u8>> = vec![
        vec![],
        b"TLG6.0\x00raw\x1a".to_vec(),                 // header only
        b"TLG5.0\x00raw\x1a\x04\x00\x00\x00".to_vec(), // truncated TLG5
        b"TLG6.0\x00raw\x1a\x04\x00\x00\x00".to_vec(), // truncated TLG6
        b"TLG6.0\x00raw\x1a\x05\x00\x00\x00".to_vec(), // bad color count
        b"RIFF\x00\x00\x00\x00WEBPVP8 ".to_vec(),      // webp masquerading as tlg
        b"GIF89a".to_vec(),                            // wrong magic
        b"TLG0.0\x00sds\x1a\xff\xff\xff\x7f".to_vec(), // SDS with absurd raw length
        b"TLG0.0\x00sds\x1a\x00\x00\x00\x00".to_vec(), // SDS with no payload
        {
            // TLG6 with huge dimensions
            let mut v = TLG6_MAGIC.to_vec();
            v.extend_from_slice(&[4, 0, 0, 0]);
            v.extend_from_slice(&(1i32 << 30).to_le_bytes());
            v.extend_from_slice(&(1i32 << 30).to_le_bytes());
            v.extend_from_slice(&0i32.to_le_bytes());
            v
        },
        {
            // valid header but truncated filter-type section
            let mut v = TLG6_MAGIC.to_vec();
            v.extend_from_slice(&[4, 0, 0, 0]);
            v.extend_from_slice(&8i32.to_le_bytes());
            v.extend_from_slice(&8i32.to_le_bytes());
            v.extend_from_slice(&0i32.to_le_bytes());
            v.extend_from_slice(&100i32.to_le_bytes()); // filter size larger than file
            v
        },
        {
            // TLG5 with a zero block height
            let mut v = b"TLG5.0\x00raw\x1a".to_vec();
            v.push(4); // 4 channels
            v.extend_from_slice(&8i32.to_le_bytes()); // width
            v.extend_from_slice(&8i32.to_le_bytes()); // height
            v.extend_from_slice(&0i32.to_le_bytes()); // zero block height
            v.extend_from_slice(&0i32.to_le_bytes()); // empty block-size table
            v
        },
        {
            // TLG6 with a negative filter-type size
            let mut v = TLG6_MAGIC.to_vec();
            v.extend_from_slice(&[4, 0, 0, 0]);
            v.extend_from_slice(&8i32.to_le_bytes());
            v.extend_from_slice(&8i32.to_le_bytes());
            v.extend_from_slice(&0i32.to_le_bytes());
            v.extend_from_slice(&(-1i32).to_le_bytes());
            v
        },
    ];
    for (i, c) in cases.iter().enumerate() {
        assert!(
            decode_image(&media, c, None).is_err(),
            "case {i} should fail"
        );
    }
    // random bytes of various lengths must not panic
    let mut seed = 0x1234_5678u64;
    for len in [1usize, 5, 11, 64, 1024] {
        let mut v = Vec::with_capacity(len);
        for _ in 0..len {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            v.push((seed >> 33) as u8);
        }
        let _ = decode_image(&media, &v, None);
    }
}
