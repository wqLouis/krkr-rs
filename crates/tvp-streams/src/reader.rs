//! Little-endian primitive codecs.
//!
//! The reference engine stores integers little-endian on disk (see
//! `tTJSBinaryStream::ReadI16LE`/`ReadI32LE`/`ReadI64LE` in `tjs2/tjs.h`
//! and the XP3 archive format). These functions encode/decode the
//! fixed-width primitives used by the
//! [`BinaryStream`](crate::BinaryStream) convenience methods; they
//! operate on `&[u8]` so they can be tested in isolation.
//!
//! All `read_*` functions panic if the slice is shorter than the
//! primitive — they are only meant to be fed exact-length buffers (the
//! [`BinaryStream::read_exact`](crate::BinaryStream::read_exact) path
//! guarantees this). Use the
//! [`BinaryStream`](crate::BinaryStream) methods (`read_u16le`,
//! `write_u32le`, …) for error-reporting I/O.

use std::convert::TryInto;

/// Decode one byte.
#[inline]
pub fn read_u8(bytes: &[u8]) -> u8 {
    bytes[0]
}

/// Decode one little-endian `u16`.
#[inline]
pub fn read_u16le(bytes: &[u8]) -> u16 {
    u16::from_le_bytes(bytes.try_into().expect("read_u16le: need 2 bytes"))
}

/// Decode one little-endian `u32`.
#[inline]
pub fn read_u32le(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("read_u32le: need 4 bytes"))
}

/// Decode one little-endian `u64`.
#[inline]
pub fn read_u64le(bytes: &[u8]) -> u64 {
    u64::from_le_bytes(bytes.try_into().expect("read_u64le: need 8 bytes"))
}

/// Decode one signed byte.
#[inline]
pub fn read_i8(bytes: &[u8]) -> i8 {
    i8::from_le_bytes([bytes[0]])
}

/// Decode one little-endian `i16`.
#[inline]
pub fn read_i16le(bytes: &[u8]) -> i16 {
    i16::from_le_bytes(bytes.try_into().expect("read_i16le: need 2 bytes"))
}

/// Decode one little-endian `i32`.
#[inline]
pub fn read_i32le(bytes: &[u8]) -> i32 {
    i32::from_le_bytes(bytes.try_into().expect("read_i32le: need 4 bytes"))
}

/// Decode one little-endian `i64`.
#[inline]
pub fn read_i64le(bytes: &[u8]) -> i64 {
    i64::from_le_bytes(bytes.try_into().expect("read_i64le: need 8 bytes"))
}

/// Encode one byte.
#[inline]
pub fn write_u8(v: u8) -> [u8; 1] {
    v.to_le_bytes()
}

/// Encode one little-endian `u16`.
#[inline]
pub fn write_u16le(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

/// Encode one little-endian `u32`.
#[inline]
pub fn write_u32le(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// Encode one little-endian `u64`.
#[inline]
pub fn write_u64le(v: u64) -> [u8; 8] {
    v.to_le_bytes()
}

/// Encode one signed byte.
#[inline]
pub fn write_i8(v: i8) -> [u8; 1] {
    v.to_le_bytes()
}

/// Encode one little-endian `i16`.
#[inline]
pub fn write_i16le(v: i16) -> [u8; 2] {
    v.to_le_bytes()
}

/// Encode one little-endian `i32`.
#[inline]
pub fn write_i32le(v: i32) -> [u8; 4] {
    v.to_le_bytes()
}

/// Encode one little-endian `i64`.
#[inline]
pub fn write_i64le(v: i64) -> [u8; 8] {
    v.to_le_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_roundtrips() {
        for v in [0u16, 1, 0x1234, 0xFFFF] {
            let bytes = write_u16le(v);
            assert_eq!(read_u16le(&bytes), v);
        }
        assert_eq!(write_u16le(0x1234), [0x34, 0x12]);

        for v in [0u32, 1, 0xDEAD_BEEF, u32::MAX] {
            let bytes = write_u32le(v);
            assert_eq!(read_u32le(&bytes), v);
        }
        assert_eq!(write_u32le(0xDEAD_BEEF), [0xEF, 0xBE, 0xAD, 0xDE]);

        for v in [0u64, 1, 0x1122_3344_5566_7788, u64::MAX] {
            let bytes = write_u64le(v);
            assert_eq!(read_u64le(&bytes), v);
        }
        assert_eq!(
            write_u64le(0x1122_3344_5566_7788),
            [0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11]
        );

        assert_eq!(read_u8(&[0xAB]), 0xAB);
        assert_eq!(write_u8(0xAB), [0xAB]);
    }

    #[test]
    fn signed_roundtrips() {
        for v in [0i16, 1, -1, i16::MIN, i16::MAX] {
            let bytes = write_i16le(v);
            assert_eq!(read_i16le(&bytes), v);
        }
        assert_eq!(write_i16le(-2), [0xFE, 0xFF]);
        assert_eq!(read_i16le(&[0xFE, 0xFF]), -2);

        for v in [0i32, 1, -1, i32::MIN, i32::MAX] {
            let bytes = write_i32le(v);
            assert_eq!(read_i32le(&bytes), v);
        }
        assert_eq!(write_i32le(-2), [0xFE, 0xFF, 0xFF, 0xFF]);

        for v in [0i64, 1, -1, i64::MIN, i64::MAX] {
            let bytes = write_i64le(v);
            assert_eq!(read_i64le(&bytes), v);
        }
        assert_eq!(
            write_i64le(-2),
            [0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
        );

        assert_eq!(read_i8(&[0xFF]), -1);
        assert_eq!(write_i8(-1), [0xFF]);
    }

    #[test]
    #[should_panic]
    fn read_u16le_requires_2_bytes() {
        read_u16le(&[0x01]);
    }

    #[test]
    #[should_panic]
    fn read_u64le_requires_8_bytes() {
        read_u64le(&[0; 7]);
    }
}
