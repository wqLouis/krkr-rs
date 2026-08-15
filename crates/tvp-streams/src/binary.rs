//! The [`BinaryStream`] trait (a Rust `tTJSBinaryStream`) and its
//! in-memory implementation [`MemoryStream`] (a Rust `tTVPMemoryStream`),
//! plus a [`BinaryStream`] implementation for [`std::fs::File`].
//!
//! ## C++ semantics
//!
//! The trait mirrors `tTJSBinaryStream` from `tjs2/tjs.h`:
//!
//! * [`BinaryStream::read`] returns the number of bytes actually read and
//!   returns `Ok(0)` at end-of-stream (like the C++ `Read`).
//! * [`BinaryStream::write`] returns the number of bytes actually written.
//! * [`BinaryStream::seek`] never moves past the end of the stream and
//!   never goes negative — like the C++ implementations, where `Seek`
//!   refuses offsets outside `[0, GetSize()]` and leaves the position
//!   unchanged. As a Rust-idiomatic improvement the port reports such
//!   seeks as `io::ErrorKind::InvalidInput` instead of silently ignoring
//!   them.
//! * `tTJSBinaryStream::ReadBuffer(buffer, 0)` means "read everything
//!   remaining"; that is [`BinaryStream::read_to_end`] here. A zero-length
//!   [`read`](BinaryStream::read) slice always returns `Ok(0)`, per the
//!   usual Rust convention.
//! * `ReadBuffer`/`WriteBuffer` (loop until the whole buffer is
//!   transferred, raising on EOF / zero writes) are
//!   [`read_exact`](BinaryStream::read_exact) /
//!   [`write_all`](BinaryStream::write_all).

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

use crate::reader;

/// The C++ access flags (`TJS_BS_*` in `tjs2/tjs.h`), kept as
/// documentation of the mode letters used by `TVPCreateStream`.
#[allow(dead_code)]
mod access_flags {
    /// Open for reading only.
    pub const READ: u32 = 0;
    /// Open for writing only (truncates).
    pub const WRITE: u32 = 1;
    /// Open for appending.
    pub const APPEND: u32 = 2;
    /// Open for reading and writing.
    pub const UPDATE: u32 = 3;
    /// Delete the underlying storage on close.
    pub const DELETE_ON_CLOSE: u32 = 0x10;
    /// Mask of the low 4 access-mode bits.
    pub const ACCESS_MASK: u32 = 0x0f;
    /// Mask of the high option bits.
    pub const OPTION_MASK: u32 = 0xf0;
}

/// Byte-oriented read/write/seek stream, the Rust counterpart of the
/// C++ `tTJSBinaryStream`.
///
/// This trait is object-safe, so `Box<dyn BinaryStream>` can be used to
/// hold heterogeneous streams.
pub trait BinaryStream {
    /// Read up to `buf.len()` bytes into `buf`.
    ///
    /// Returns the number of bytes actually read; `Ok(0)` signals
    /// end-of-stream (or an empty buffer). Reading never moves the
    /// position past the end of the stream.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Write up to `buf.len()` bytes from `buf`.
    ///
    /// Returns the number of bytes actually written. The position
    /// advances by that amount and the stream size grows if needed.
    fn write(&mut self, buf: &[u8]) -> io::Result<usize>;

    /// Seek to a position relative to the start, the current position or
    /// the end of the stream.
    ///
    /// Returns the new absolute position. Out-of-range seeks (negative
    /// target, or past the end of the stream) return
    /// `io::ErrorKind::InvalidInput` and leave the position unchanged,
    /// matching the C++ behavior of never moving outside
    /// `[0, GetSize()]`.
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64>;

    /// Total size of the stream in bytes.
    fn get_size(&self) -> io::Result<u64>;

    /// Current position within the stream, in bytes from the start.
    ///
    /// Takes `&mut self` like `std::io::Seek::stream_position` and the
    /// C++ `tTJSBinaryStream::GetPosition` (both non-const, since a
    /// file-backed stream must query the OS).
    fn get_position(&mut self) -> io::Result<u64>;

    /// Seek to an absolute position (C++ `SetPosition`).
    fn set_position(&mut self, pos: u64) -> io::Result<u64> {
        self.seek(SeekFrom::Start(pos))
    }

    /// Read exactly `buf.len()` bytes (C++ `ReadBuffer`).
    ///
    /// Fails with `io::ErrorKind::UnexpectedEof` if the stream ends
    /// first.
    fn read_exact(&mut self, mut buf: &mut [u8]) -> io::Result<()> {
        while !buf.is_empty() {
            match self.read(buf)? {
                0 => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "unexpected end of stream",
                    ));
                }
                n => buf = &mut buf[n..],
            }
        }
        Ok(())
    }

    /// Read everything remaining from the current position to the end of
    /// the stream (C++ `ReadBuffer(buffer, 0)` = read all).
    fn read_to_end(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        let mut chunk = [0u8; 4096];
        let mut total = 0;
        loop {
            let n = self.read(&mut chunk)?;
            if n == 0 {
                return Ok(total);
            }
            buf.extend_from_slice(&chunk[..n]);
            total += n;
        }
    }

    /// Write exactly `buf.len()` bytes (C++ `WriteBuffer`).
    ///
    /// Fails with `io::ErrorKind::WriteZero` if a write transfers zero
    /// bytes before the buffer is exhausted.
    fn write_all(&mut self, mut buf: &[u8]) -> io::Result<()> {
        while !buf.is_empty() {
            match self.write(buf)? {
                0 => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "failed to write whole buffer",
                    ));
                }
                n => buf = &buf[n..],
            }
        }
        Ok(())
    }

    /// Read one byte (C++ `ReadI8LE`).
    fn read_u8(&mut self) -> io::Result<u8> {
        let mut b = [0u8; 1];
        self.read_exact(&mut b)?;
        Ok(reader::read_u8(&b))
    }

    /// Read one little-endian `u16` (C++ `ReadI16LE`).
    fn read_u16le(&mut self) -> io::Result<u16> {
        let mut b = [0u8; 2];
        self.read_exact(&mut b)?;
        Ok(reader::read_u16le(&b))
    }

    /// Read one little-endian `u32` (C++ `ReadI32LE`).
    fn read_u32le(&mut self) -> io::Result<u32> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b)?;
        Ok(reader::read_u32le(&b))
    }

    /// Read one little-endian `u64` (C++ `ReadI64LE`).
    fn read_u64le(&mut self) -> io::Result<u64> {
        let mut b = [0u8; 8];
        self.read_exact(&mut b)?;
        Ok(reader::read_u64le(&b))
    }

    /// Read one signed byte.
    fn read_i8(&mut self) -> io::Result<i8> {
        let mut b = [0u8; 1];
        self.read_exact(&mut b)?;
        Ok(reader::read_i8(&b))
    }

    /// Read one little-endian `i16`.
    fn read_i16le(&mut self) -> io::Result<i16> {
        let mut b = [0u8; 2];
        self.read_exact(&mut b)?;
        Ok(reader::read_i16le(&b))
    }

    /// Read one little-endian `i32`.
    fn read_i32le(&mut self) -> io::Result<i32> {
        let mut b = [0u8; 4];
        self.read_exact(&mut b)?;
        Ok(reader::read_i32le(&b))
    }

    /// Read one little-endian `i64`.
    fn read_i64le(&mut self) -> io::Result<i64> {
        let mut b = [0u8; 8];
        self.read_exact(&mut b)?;
        Ok(reader::read_i64le(&b))
    }

    /// Write one byte.
    fn write_u8(&mut self, v: u8) -> io::Result<()> {
        self.write_all(&reader::write_u8(v))
    }

    /// Write one little-endian `u16`.
    fn write_u16le(&mut self, v: u16) -> io::Result<()> {
        self.write_all(&reader::write_u16le(v))
    }

    /// Write one little-endian `u32`.
    fn write_u32le(&mut self, v: u32) -> io::Result<()> {
        self.write_all(&reader::write_u32le(v))
    }

    /// Write one little-endian `u64`.
    fn write_u64le(&mut self, v: u64) -> io::Result<()> {
        self.write_all(&reader::write_u64le(v))
    }

    /// Write one signed byte.
    fn write_i8(&mut self, v: i8) -> io::Result<()> {
        self.write_all(&reader::write_i8(v))
    }

    /// Write one little-endian `i16`.
    fn write_i16le(&mut self, v: i16) -> io::Result<()> {
        self.write_all(&reader::write_i16le(v))
    }

    /// Write one little-endian `i32`.
    fn write_i32le(&mut self, v: i32) -> io::Result<()> {
        self.write_all(&reader::write_i32le(v))
    }

    /// Write one little-endian `i64`.
    fn write_i64le(&mut self, v: i64) -> io::Result<()> {
        self.write_all(&reader::write_i64le(v))
    }
}

impl<S: BinaryStream + ?Sized> BinaryStream for &mut S {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (**self).read(buf)
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (**self).write(buf)
    }

    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        (**self).seek(pos)
    }

    fn get_size(&self) -> io::Result<u64> {
        (**self).get_size()
    }

    fn get_position(&mut self) -> io::Result<u64> {
        (**self).get_position()
    }
}

impl<S: BinaryStream + ?Sized> BinaryStream for Box<S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (**self).read(buf)
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (**self).write(buf)
    }

    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        (**self).seek(pos)
    }

    fn get_size(&self) -> io::Result<u64> {
        (**self).get_size()
    }

    fn get_position(&mut self) -> io::Result<u64> {
        (**self).get_position()
    }
}

/// [`BinaryStream`] over a [`File`].
impl BinaryStream for File {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        Read::read(self, buf)
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Write::write(self, buf)
    }

    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        Seek::seek(self, pos)
    }

    fn get_size(&self) -> io::Result<u64> {
        Ok(self.metadata()?.len())
    }

    fn get_position(&mut self) -> io::Result<u64> {
        Seek::stream_position(self)
    }
}

/// An in-memory read/write/seek stream, the Rust counterpart of
/// `tTVPMemoryStream` (`base/UtilStreams.cpp`).
///
/// Unlike the C++ version (which can wrap an external, read-only memory
/// block via `Reference`), this port always owns its buffer and is always
/// writable. Read-only *windows* over existing data are the job of
/// [`LimitedStream`](crate::limited::LimitedStream).
///
/// Like the C++ class, seeking is confined to `[0, size]`: a seek past
/// the end of the current buffer (or to a negative position) is an error
/// and leaves the position unchanged. There are therefore no "holes" in
/// the buffer — writes always start at or before the end. The buffer
/// grows automatically on writes past the end, and `size` tracks the
/// highest byte written (or [`set_size`](MemoryStream::set_size)).
#[derive(Debug, Default)]
pub struct MemoryStream {
    data: Vec<u8>,
    pos: u64,
}

impl MemoryStream {
    /// Create an empty in-memory stream.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an in-memory stream from a byte slice (copies the data).
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self {
            data: bytes.to_vec(),
            pos: 0,
        }
    }

    /// Create an in-memory stream from an owned buffer.
    pub fn from_vec(data: Vec<u8>) -> Self {
        Self { data, pos: 0 }
    }

    /// The internal buffer as a slice (C++ `GetInternalBuffer`).
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// The internal buffer as a mutable slice (C++ `GetInternalBuffer`).
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        &mut self.data
    }

    /// Take the internal buffer, discarding the stream.
    pub fn into_inner(self) -> Vec<u8> {
        self.data
    }

    /// Reset the stream: empty buffer, position 0 (C++ `Clear`).
    pub fn clear(&mut self) {
        self.data.clear();
        self.pos = 0;
    }

    /// Truncate (or zero-extend) the buffer to `size` bytes (C++
    /// `SetSize`). Growth zero-fills; the C++ version leaves new memory
    /// uninitialized, which we deliberately do not replicate.
    ///
    /// The position is clamped to the new size when shrinking.
    pub fn set_size(&mut self, size: usize) {
        if size < self.data.len() {
            self.data.truncate(size);
        } else {
            self.data.resize(size, 0);
        }
        if self.pos > size as u64 {
            self.pos = size as u64;
        }
    }

    /// Truncate the buffer at the current position, discarding everything
    /// after it (C++ `SetEndOfStorage`).
    pub fn set_end_of_storage(&mut self) {
        self.data.truncate(self.pos as usize);
    }
}

impl BinaryStream for MemoryStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.data.len() as u64 {
            return Ok(0);
        }
        let n = buf.len().min((self.data.len() as u64 - self.pos) as usize);
        let start = self.pos as usize;
        buf[..n].copy_from_slice(&self.data[start..start + n]);
        self.pos += n as u64;
        Ok(n)
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let start = self.pos as usize;
        let end = start + buf.len();
        if end > self.data.len() {
            // grow the buffer; the gap (if the position was not at the
            // end, which can only happen after set_size growth) is
            // zero-filled
            self.data.resize(end, 0);
        }
        self.data[start..end].copy_from_slice(buf);
        self.pos = end as u64;
        Ok(buf.len())
    }

    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let size = self.data.len() as i128;
        let target = match pos {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(o) => self.pos as i128 + o as i128,
            SeekFrom::End(o) => size + o as i128,
        };
        if target < 0 || target > size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("seek out of bounds: target {target} not in 0..={size}"),
            ));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }

    fn get_size(&self) -> io::Result<u64> {
        Ok(self.data.len() as u64)
    }

    fn get_position(&mut self) -> io::Result<u64> {
        Ok(self.pos)
    }
}

impl From<Vec<u8>> for MemoryStream {
    fn from(data: Vec<u8>) -> Self {
        Self::from_vec(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn byte_stream(bytes: &[u8]) -> MemoryStream {
        MemoryStream::from_bytes(bytes)
    }

    #[test]
    fn write_seek_read_roundtrip() {
        let mut s = MemoryStream::new();
        s.write_all(b"hello ").unwrap();
        s.write_all(b"world").unwrap();
        assert_eq!(s.get_size().unwrap(), 11);
        assert_eq!(s.get_position().unwrap(), 11);

        s.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(s.get_position().unwrap(), 0);
        let mut buf = [0u8; 5];
        s.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"hello");
        assert_eq!(s.get_position().unwrap(), 5);

        let mut rest = Vec::new();
        s.read_to_end(&mut rest).unwrap();
        assert_eq!(rest, b" world");
        assert_eq!(s.get_position().unwrap(), 11);
        // at EOF, reads return 0
        let mut one = [0u8; 1];
        assert_eq!(s.read(&mut one).unwrap(), 0);
    }

    #[test]
    fn write_from_middle_overwrites_and_grows() {
        let mut s = byte_stream(b"abcdef");
        s.seek(SeekFrom::Start(2)).unwrap();
        s.write_all(b"XY").unwrap();
        assert_eq!(s.as_slice(), b"abXYef");
        // writing at the end grows the stream
        s.seek(SeekFrom::End(0)).unwrap();
        s.write_all(b"GH").unwrap();
        assert_eq!(s.as_slice(), b"abXYefGH");
        assert_eq!(s.get_size().unwrap(), 8);
    }

    #[test]
    fn seek_past_end_is_rejected_position_unchanged() {
        let mut s = byte_stream(b"0123456789");
        s.seek(SeekFrom::Start(4)).unwrap();
        // seek past end: rejected, position stays at 4
        let err = s.seek(SeekFrom::End(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(s.get_position().unwrap(), 4);
        // negative seek: rejected
        let err = s.seek(SeekFrom::Current(-5)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(s.get_position().unwrap(), 4);
    }

    #[test]
    fn seek_to_exact_end_is_eof() {
        let mut s = byte_stream(b"abc");
        s.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(s.get_position().unwrap(), 3);
        let mut buf = [0u8; 1];
        assert_eq!(s.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn size_and_position_tracking() {
        let mut s = MemoryStream::new();
        assert_eq!(s.get_size().unwrap(), 0);
        s.write_all(b"12345").unwrap();
        assert_eq!(s.get_size().unwrap(), 5);
        s.seek(SeekFrom::Start(2)).unwrap();
        assert_eq!(s.get_position().unwrap(), 2);
        s.seek(SeekFrom::Current(10)).unwrap_err();
        assert_eq!(s.get_position().unwrap(), 2);
        s.seek(SeekFrom::End(-1)).unwrap();
        assert_eq!(s.get_position().unwrap(), 4);
    }

    #[test]
    fn set_end_of_storage_truncates() {
        let mut s = byte_stream(b"0123456789");
        s.seek(SeekFrom::Start(4)).unwrap();
        s.set_end_of_storage();
        assert_eq!(s.get_size().unwrap(), 4);
        assert_eq!(s.as_slice(), b"0123");
        // reading at the end gives EOF
        let mut buf = [0u8; 4];
        assert_eq!(s.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn set_size_grows_zero_filled_and_shrinks() {
        let mut s = MemoryStream::new();
        s.set_size(5);
        assert_eq!(s.get_size().unwrap(), 5);
        assert_eq!(s.as_slice(), &[0u8; 5]);
        s.seek(SeekFrom::Start(3)).unwrap();
        s.write_all(b"XY").unwrap();
        assert_eq!(s.as_slice(), &[0, 0, 0, b'X', b'Y']);
        // shrink below the position clamps the position
        s.set_size(2);
        assert_eq!(s.get_size().unwrap(), 2);
        assert_eq!(s.get_position().unwrap(), 2);
    }

    #[test]
    fn clear_resets_everything() {
        let mut s = byte_stream(b"hello");
        s.seek(SeekFrom::Start(2)).unwrap();
        s.clear();
        assert_eq!(s.get_size().unwrap(), 0);
        assert_eq!(s.get_position().unwrap(), 0);
        assert!(s.as_slice().is_empty());
    }

    #[test]
    fn read_exact_fails_at_eof() {
        let mut s = byte_stream(b"abc");
        let mut buf = [0u8; 5];
        let err = s.read_exact(&mut buf).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn read_u64le_roundtrip_via_stream() {
        let mut s = MemoryStream::new();
        s.write_u64le(0x1122_3344_5566_7788).unwrap();
        s.write_u32le(0xDEAD_BEEF).unwrap();
        s.write_u16le(0xBEEF).unwrap();
        s.write_u8(0xFF).unwrap();
        s.write_i64le(-42).unwrap();
        s.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(s.read_u64le().unwrap(), 0x1122_3344_5566_7788);
        assert_eq!(s.read_u32le().unwrap(), 0xDEAD_BEEF);
        assert_eq!(s.read_u16le().unwrap(), 0xBEEF);
        assert_eq!(s.read_u8().unwrap(), 0xFF);
        assert_eq!(s.read_i64le().unwrap(), -42);
        // read past the end fails
        let err = s.read_u16le().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn into_inner_and_from_vec() {
        let s = MemoryStream::from_vec(vec![1, 2, 3]);
        assert_eq!(s.into_inner(), vec![1, 2, 3]);
        let s: MemoryStream = vec![9u8; 4].into();
        assert_eq!(s.as_slice(), &[9u8; 4]);
    }

    #[test]
    fn file_stream_roundtrip() {
        let path =
            std::env::temp_dir().join(format!("tvp-streams-test-{}.bin", std::process::id()));
        // `File::create` opens write-only, so write through one handle...
        {
            let mut f = File::create(&path).unwrap();
            BinaryStream::write_all(&mut f, b"file data 123").unwrap();
            assert_eq!(f.get_size().unwrap(), 13);
        }
        // ...and read back through a fresh read-only handle
        let mut f = File::open(&path).unwrap();
        assert_eq!(f.get_size().unwrap(), 13);
        let mut buf = Vec::new();
        BinaryStream::read_to_end(&mut f, &mut buf).unwrap();
        assert_eq!(buf, b"file data 123");
        drop(f);
        std::fs::remove_file(&path).unwrap();
    }
}
