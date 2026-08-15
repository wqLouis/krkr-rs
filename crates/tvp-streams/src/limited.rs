//! [`LimitedStream`] — a read-only window over another stream.
//!
//! This is the Rust counterpart of `tTVPPartialStream`
//! (`base/UtilStreams.cpp`), which the reference engine uses to expose an
//! XP3 archive segment (or a range of a storage) as a standalone stream.
//!
//! The window is `[start, start + size)` of the underlying stream.
//! Seeks are relative to the window (`SeekFrom::Start(0)` is the first
//! window byte), reads never escape the window, and writes are rejected.
//!
//! Unlike the C++ class — which *owns* and deletes the wrapped stream —
//! this port borrows it, so no ownership/aliasing surprises. Wrap the
//! underlying stream in a `Box` if you need the C++ ownership model.

use std::io::{self, SeekFrom};

use crate::BinaryStream;

/// Read-only sub-range of another [`BinaryStream`].
///
/// `start` is the absolute offset of the window in the underlying stream
/// and `size` is the length of the window. Reading past the window end
/// returns `Ok(0)` (EOF) — bytes beyond the window are never touched.
#[derive(Debug)]
pub struct LimitedStream<'a, S: BinaryStream + ?Sized> {
    inner: &'a mut S,
    start: u64,
    size: u64,
    pos: u64,
}

impl<'a, S: BinaryStream + ?Sized> LimitedStream<'a, S> {
    /// Wrap `[start, start + size)` of `inner` as a read-only stream.
    ///
    /// No validation is performed up front (matching `tTVPPartialStream`,
    /// which only seeks the underlying stream to `start`); if the window
    /// reaches past the end of the underlying data, reads simply return
    /// EOF once the available bytes are exhausted.
    pub fn new(inner: &'a mut S, start: u64, size: u64) -> Self {
        Self {
            inner,
            start,
            size,
            pos: 0,
        }
    }

    /// Absolute offset of the window in the underlying stream.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Length of the window in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

impl<S: BinaryStream + ?Sized> BinaryStream for LimitedStream<'_, S> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos >= self.size || buf.is_empty() {
            return Ok(0);
        }
        let n = buf.len().min((self.size - self.pos) as usize);
        // position the underlying stream at the window byte
        self.inner.seek(SeekFrom::Start(self.start + self.pos))?;
        let read = self.inner.read(&mut buf[..n])?;
        self.pos += read as u64;
        Ok(read)
    }

    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        // `tTVPPartialStream::Write` returns 0; we surface an explicit
        // error instead.
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "LimitedStream is read-only",
        ))
    }

    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        let size = self.size as i128;
        let target = match pos {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(o) => self.pos as i128 + o as i128,
            SeekFrom::End(o) => size + o as i128,
        };
        if target < 0 || target > size {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("seek out of window bounds: target {target} not in 0..={size}"),
            ));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }

    fn get_size(&self) -> io::Result<u64> {
        Ok(self.size)
    }

    fn get_position(&mut self) -> io::Result<u64> {
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryStream;

    #[test]
    fn reads_are_confined_to_the_window() {
        let mut ms = MemoryStream::from_bytes(b"hello world");
        let mut w = LimitedStream::new(&mut ms, 6, 5);
        // only "world" is reachable
        let mut buf = [0u8; 10];
        let n = w.read(&mut buf).unwrap();
        assert_eq!(n, 5);
        assert_eq!(&buf[..n], b"world");
        // the window is now exhausted; further reads are EOF and must not
        // leak the underlying bytes after the window
        assert_eq!(w.read(&mut buf).unwrap(), 0);
        assert_eq!(w.get_position().unwrap(), 5);
    }

    #[test]
    fn full_window_read() {
        let mut ms = MemoryStream::from_bytes(b"0123456789");
        let mut w = LimitedStream::new(&mut ms, 0, 10);
        let mut buf = Vec::new();
        w.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"0123456789");
    }

    #[test]
    fn read_returns_partial_at_window_end() {
        let mut ms = MemoryStream::from_bytes(b"0123456789");
        let mut w = LimitedStream::new(&mut ms, 2, 4);
        let mut buf = [0u8; 3];
        assert_eq!(w.read(&mut buf).unwrap(), 3);
        assert_eq!(&buf, b"234");
        assert_eq!(w.read(&mut buf).unwrap(), 1);
        assert_eq!(&buf[..1], b"5");
        assert_eq!(w.read(&mut buf).unwrap(), 0);
    }

    #[test]
    fn seek_within_window() {
        let mut ms = MemoryStream::from_bytes(b"0123456789");
        let mut w = LimitedStream::new(&mut ms, 2, 6);
        assert_eq!(w.get_size().unwrap(), 6);
        // window-relative seek: 0 is the first window byte ("2")
        w.seek(SeekFrom::Start(1)).unwrap();
        let mut buf = [0u8; 3];
        w.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"345");
        // relative to current position
        w.seek(SeekFrom::Current(1)).unwrap();
        assert_eq!(w.get_position().unwrap(), 5);
        // relative to window end: -1 is the last window byte ("7")
        w.seek(SeekFrom::End(-1)).unwrap();
        let mut one = [0u8; 1];
        w.read_exact(&mut one).unwrap();
        assert_eq!(&one, b"7");
        // at window end == EOF
        assert_eq!(w.read(&mut one).unwrap(), 0);
    }

    #[test]
    fn seek_outside_window_is_rejected() {
        let mut ms = MemoryStream::from_bytes(b"0123456789");
        let mut w = LimitedStream::new(&mut ms, 0, 5);
        w.seek(SeekFrom::Start(2)).unwrap();
        // past the window end
        let err = w.seek(SeekFrom::End(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(w.get_position().unwrap(), 2);
        // negative
        let err = w.seek(SeekFrom::Current(-3)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(w.get_position().unwrap(), 2);
        // exactly at the window end is allowed (EOF position)
        w.seek(SeekFrom::End(0)).unwrap();
        assert_eq!(w.get_position().unwrap(), 5);
    }

    #[test]
    fn write_is_rejected() {
        let mut inner = MemoryStream::from_bytes(b"data");
        {
            let mut w = LimitedStream::new(&mut inner, 0, 4);
            let err = w.write(b"x").unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::Unsupported);
        }
        // underlying data is untouched
        assert_eq!(inner.as_slice(), b"data");
    }

    #[test]
    fn window_covering_whole_stream_equals_stream() {
        let mut ms = MemoryStream::from_bytes(b"entire");
        let mut w = LimitedStream::new(&mut ms, 0, 6);
        let mut buf = Vec::new();
        w.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"entire");
    }

    #[test]
    fn empty_window() {
        let mut ms = MemoryStream::from_bytes(b"data");
        let mut w = LimitedStream::new(&mut ms, 0, 0);
        let mut buf = [0u8; 4];
        assert_eq!(w.read(&mut buf).unwrap(), 0);
        // position 0 is the only valid position (== window end)
        w.seek(SeekFrom::Start(0)).unwrap();
        assert_eq!(w.get_position().unwrap(), 0);
        // anything past the end is rejected
        let err = w.seek(SeekFrom::Start(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn window_over_boxed_dyn_stream() {
        // LimitedStream works over trait objects too, and the window
        // borrow does not prevent using the underlying stream again
        // once the window is dropped.
        let mut ms = MemoryStream::from_bytes(b"abcdef");
        {
            let mut boxed: Box<dyn BinaryStream> = Box::new(&mut ms);
            let mut w = LimitedStream::new(boxed.as_mut(), 1, 4);
            let mut buf = Vec::new();
            w.read_to_end(&mut buf).unwrap();
            assert_eq!(buf, b"bcde");
        }
        assert_eq!(ms.as_slice(), b"abcdef");
    }
}
