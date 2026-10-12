//! Reference-layout BMP encoder tests.
//!
//! The BMP writer reproduces the byte layout of
//! `reference/cpp/core/visual/GraphicsLoaderIntf.cpp` `TVPSaveTextureAsBMP`:
//! a 14-byte `TVP_WIN_BITMAPFILEHEADER`, a 40-byte `TVP_WIN_BITMAPINFOHEADER`,
//! an optional 1024-byte palette, and bottom-up scanlines with 4-byte pitch
//! padding. The old `image`-crate path used a 108-byte `BITMAPV4HEADER`, which
//! shifted the appended save struct 68 bytes and corrupted the thumbnail.

use media::{Media, encode_image};
use testkit::pattern;

/// Reads a little-endian `u16` field (`TVPWriteLE16`).
fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().expect("u16 field"))
}

/// Reads a little-endian `u32` field (`TVPWriteLE32`).
fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("u32 field"))
}

/// Reads a little-endian `i32` header field.
fn i32_at(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("i32 field"))
}

#[test]
fn bmp32_header_and_total_length_match_the_reference() {
    let media = Media::new();
    let (width, height) = (3u32, 2u32);
    let rgba = pattern(width, height);
    let bytes = encode_image(&media, width, height, &rgba, "bmp").expect("encode bmp");

    // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:783-794
    // bfType/bfSize/bfReserved1/bfReserved2/bfOffBits.
    assert_eq!(&bytes[0..2], b"BM", "bfType");
    assert_eq!(u32_at(&bytes, 2), 78, "bfSize");
    assert_eq!(u16_at(&bytes, 6), 0, "bfReserved1");
    assert_eq!(u16_at(&bytes, 8), 0, "bfReserved2");
    assert_eq!(u32_at(&bytes, 10), 54, "bfOffBits");

    // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:796-810
    // biSize/biWidth/biHeight/biPlanes/biBitCount and the zero fields.
    assert_eq!(u32_at(&bytes, 14), 40, "biSize");
    assert_eq!(i32_at(&bytes, 18), 3, "biWidth");
    assert_eq!(i32_at(&bytes, 22), 2, "biHeight");
    assert_eq!(u16_at(&bytes, 26), 1, "biPlanes");
    assert_eq!(u16_at(&bytes, 28), 32, "biBitCount");
    assert_eq!(u32_at(&bytes, 30), 0, "biCompression");
    assert_eq!(u32_at(&bytes, 34), 0, "biSizeImage");
    assert_eq!(u32_at(&bytes, 38), 0, "biXPelsPerMeter");
    assert_eq!(u32_at(&bytes, 42), 0, "biYPelsPerMeter");
    assert_eq!(u32_at(&bytes, 46), 0, "biClrUsed");
    assert_eq!(u32_at(&bytes, 50), 0, "biClrImportant");

    // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:786-800
    // 32 bpp has no palette and no pitch padding: w*h*4 + 54 bytes.
    assert_eq!(bytes.len(), (width * height * 4 + 54) as usize);
}

#[test]
fn bmp_and_bmp32_are_the_same_four_byte_mode() {
    // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:737-744
    let media = Media::new();
    let rgba = pattern(3, 2);
    let bmp = encode_image(&media, 3, 2, &rgba, "bmp").expect("bmp");
    let bmp32 = encode_image(&media, 3, 2, &rgba, "bmp32").expect("bmp32");
    assert_eq!(bmp, bmp32);
}

#[test]
fn bmp32_body_is_bottom_up_bgra() {
    let rgba: [u8; 16] = [
        1, 2, 3, 4, 5, 6, 7, 8, // y = 0
        9, 10, 11, 12, 13, 14, 15, 16, // y = 1
    ];
    let bytes = encode_image(&Media::new(), 2, 2, &rgba, "bmp32").expect("encode");

    // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:841-848
    // Rows are written from GetHeight()-1 down to 0 and each 32-bit pixel is
    // reversed to BGRA.
    let body = &bytes[54..];
    assert_eq!(&body[0..4], &[11, 10, 9, 12], "y=1,x=0");
    assert_eq!(&body[4..8], &[15, 14, 13, 16], "y=1,x=1");
    assert_eq!(&body[8..12], &[3, 2, 1, 4], "y=0,x=0");
    assert_eq!(&body[12..16], &[7, 6, 5, 8], "y=0,x=1");
}

#[test]
fn bmp24_pads_each_row_to_a_four_byte_pitch() {
    let (width, height) = (3u32, 2u32);
    let rgba = pattern(width, height);
    let bytes = encode_image(&Media::new(), width, height, &rgba, "bmp24").expect("encode");

    // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:779-780,796-800
    // 3-byte rows of 9 bytes pad to a pitch of 12; no palette.
    let pitch = 12u32;
    assert_eq!(bytes.len(), (54 + pitch * height) as usize);
    assert_eq!(u32_at(&bytes, 2), 54 + pitch * height, "bfSize");
    assert_eq!(u32_at(&bytes, 10), 54, "bfOffBits");
    assert_eq!(u16_at(&bytes, 28), 24, "biBitCount");
    assert_eq!(&bytes[bytes.len() - 3..], &[0, 0, 0], "row padding");
}

#[test]
fn bmp24_body_is_bottom_up_bgr() {
    // A width of 4 makes the 3-byte pitch exact (no padding between rows).
    let rgba: [u8; 32] = [
        1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, // y = 0
        17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, // y = 1
    ];
    let bytes = encode_image(&Media::new(), 4, 2, &rgba, "bmp24").expect("encode");

    // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:855-864
    // Each pixel drops alpha and reverses to BGR; rows run bottom-up.
    let body = &bytes[54..];
    assert_eq!(&body[0..3], &[19, 18, 17], "y=1,x=0");
    assert_eq!(&body[3..6], &[23, 22, 21], "y=1,x=1");
    assert_eq!(&body[6..9], &[27, 26, 25], "y=1,x=2");
    assert_eq!(&body[9..12], &[31, 30, 29], "y=1,x=3");
    assert_eq!(&body[12..15], &[3, 2, 1], "y=0,x=0");
    assert_eq!(&body[21..24], &[15, 14, 13], "y=0,x=3");
}

#[test]
fn bmp8_writes_the_reference_palette_and_pitch() {
    let (width, height) = (3u32, 2u32);
    let rgba = pattern(width, height);
    let bytes = encode_image(&Media::new(), width, height, &rgba, "bmp8").expect("encode");

    // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:812-829
    // Palette first (1024 bytes), then pitch 4 * 2 dithered rows.
    assert_eq!(bytes.len(), 54 + 1024 + 4 * 2);
    assert_eq!(u32_at(&bytes, 2), (54 + 1024 + 8) as u32, "bfSize");
    assert_eq!(u32_at(&bytes, 10), 1078, "bfOffBits");
    assert_eq!(u16_at(&bytes, 28), 8, "biBitCount");

    // reference/cpp/core/visual/tvpgl.cpp:198-204 loops r*42+g*6+b, so entry 1
    // is (blue, green, red, 0) = (0, 0, 51, 0).
    assert_eq!(&bytes[54..58], &[0, 0, 0, 0], "palette entry 0");
    assert_eq!(&bytes[58..62], &[0, 0, 51, 0], "palette entry 1");
    // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:822-828 writes the
    // palette as BGRA quads.
    assert_eq!(&bytes[54 + 42 * 4..54 + 42 * 4 + 4], &[51, 0, 0, 0]);
}

#[test]
fn bmp8_rows_are_palette_indices_bottom_up() {
    // A single row keeps the dither phase fixed and half the pitch is padding.
    let rgba: [u8; 8] = [0, 0, 0, 255, 255, 255, 255, 255];
    let bytes = encode_image(&Media::new(), 2, 1, &rgba, "bmp8").expect("encode");

    // reference/cpp/core/visual/GraphicsLoaderIntf.cpp:849-853 and
    // reference/cpp/core/visual/tvpgl.cpp:11871-11910: black maps to palette
    // index 0 and white to index 251.
    let body = &bytes[54 + 1024..];
    assert_eq!(body.len(), 4);
    assert_eq!(body[0], 0, "black");
    assert_eq!(body[1], 251, "white");
    assert_eq!(&body[2..4], &[0, 0], "pitch padding");
}

#[test]
fn dib_is_the_default_bmp_mode() {
    let media = Media::new();
    let rgba = pattern(2, 2);
    let dib = encode_image(&media, 2, 2, &rgba, "dib").expect("dib");
    let bmp = encode_image(&media, 2, 2, &rgba, "bmp").expect("bmp");
    assert_eq!(dib, bmp);
}
