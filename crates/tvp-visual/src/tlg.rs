//! KiriKiri TLG5/TLG6 image decoder — placeholder.
//!
//! The full decoder is being ported from the reference
//! (`reference/cpp/core/base/decoder/tlg5.cpp`, `tlg6.cpp`). Until it lands,
//! [`decode_tlg`] returns an error and the bitmap loader falls back to a
//! blank image so the game load path can continue.

/// Decode TLG5/TLG6 bytes into RGBA8. Returns an error when the format is
/// not (yet) supported.
pub fn decode_tlg(_bytes: &[u8]) -> Result<image::RgbaImage, String> {
    Err("TLG decoder not yet implemented".to_string())
}
