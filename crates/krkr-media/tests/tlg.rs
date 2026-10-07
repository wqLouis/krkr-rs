//! Integration tests for the native TLG5/TLG6 decoder.
//!
//! The `tlg` module is crate-private (implementation detail of the image
//! dispatcher), so these tests exercise it through the public
//! `decode_image`/`probe_image` router.

mod common;

use common::{SDS_MAGIC, TLG6_MAGIC, encode_tlg5, encode_tlg6, test_image};
use krkr_media::{Media, decode_image, probe_image};

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
    for px in img.chunks_exact_mut(4) {
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
fn tlg5_rgb_roundtrip() {
    let (w, h) = (21usize, 19usize);
    let mut img = test_image(w, h);
    for px in img.chunks_exact_mut(4) {
        px[3] = 0xff;
    }
    let encoded = encode_tlg5(&img, w, h, 3, 4);
    let media = Media::new();
    let decoded =
        decode_image(&media, &encoded, None).expect("synthetic 24-bit TLG5 should decode");
    assert_eq!(decoded.rgba, img, "24-bit TLG5 round trip must be lossless");
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
            // TLG5 with an out-of-range block height (larger than the image)
            let mut v = b"TLG5.0\x00raw\x1a".to_vec();
            v.push(4); // 4 channels
            v.extend_from_slice(&8i32.to_le_bytes()); // width
            v.extend_from_slice(&8i32.to_le_bytes()); // height
            v.extend_from_slice(&12i32.to_le_bytes()); // block height > height
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
