//! Text streams: [`TextReadStream`] / [`TextWriteStream`].
//!
//! This module ports the semantics of `tTVPTextReadStream` /
//! `tTVPTextWriteStream` (`base/TextStream.cpp`) onto the UTF-8 world:
//! the C++ streams exchange UTF-16 text with the VM and persist it as
//! UTF-16LE with a BOM; the Rust port exchanges [`String`] (UTF-8) and
//! persists UTF-8 by default, since `G_DefaultReadEncoding` is `"UTF-8"`
//! in this codebase.
//!
//! ## Reading
//!
//! * BOM detection on open (order as in `checkTextEncoding`):
//!   `EF BB BF` → UTF-8, `FF FE` → UTF-16LE, `FE FF` → UTF-16BE;
//!   no BOM → UTF-8.
//! * The whole remaining content of the underlying stream is decoded up
//!   front (the C++ constructor slurps the entire storage too).
//! * UTF-8 input must be valid (a decode error is reported); UTF-16
//!   input never fails — unpaired surrogates become U+FFFD (the C++
//!   buffer keeps raw `char16_t`s, which a Rust `String` cannot hold).
//! * Line endings: `CRLF`, lone `CR` and lone `LF` all terminate lines
//!   and are normalized to `\n`. [`read_line`](TextReadStream::read_line)
//!   returns each line *without* its terminator.
//!
//! ## Writing
//!
//! * [`TextWriteStream`] writes UTF-8 with a configurable
//!   [`LineEnding`] — **CRLF by default**, matching the
//!   `TJS_TEXT_OUT_CRLF` define used by the C++ build.
//! * [`write_line`](TextWriteStream::write_line) appends the configured
//!   terminator; [`write_str`](TextWriteStream::write_str) writes
//!   verbatim (no normalization on output).
//! * A UTF-8 BOM can be emitted with
//!   [`with_bom`](TextWriteStream::with_bom) (off by default).

use std::io;

use crate::{BinaryStream, Error, Result};

/// The UTF-8 byte-order mark, `EF BB BF`.
pub const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// A character encoding detected from (or forced on) a text stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// UTF-8 (the default when no BOM is present).
    Utf8,
    /// UTF-16 little-endian.
    Utf16Le,
    /// UTF-16 big-endian.
    Utf16Be,
}

impl Encoding {
    /// The BOM bytes for this encoding, if it has one.
    pub fn bom(self) -> Option<&'static [u8]> {
        match self {
            Encoding::Utf8 => Some(&UTF8_BOM),
            Encoding::Utf16Le => Some(&[0xFF, 0xFE]),
            Encoding::Utf16Be => Some(&[0xFE, 0xFF]),
        }
    }
}

/// Detect the encoding from a leading byte-order mark.
///
/// Returns the encoding and the BOM length in bytes, or `None` if there
/// is no recognizable BOM (the caller then defaults to UTF-8, like the
/// C++ `G_DefaultReadEncoding`). The checks follow `checkTextEncoding`
/// in `TextStream.cpp` (UTF-16LE before UTF-16BE before UTF-8; the byte
/// patterns are disjoint so the order only matters for exactness).
pub fn detect_bom(bytes: &[u8]) -> Option<(Encoding, usize)> {
    if bytes.len() >= 2 && bytes[0] == 0xFF && bytes[1] == 0xFE {
        Some((Encoding::Utf16Le, 2))
    } else if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        Some((Encoding::Utf16Be, 2))
    } else if bytes.len() >= 3 && bytes[0] == 0xEF && bytes[1] == 0xBB && bytes[2] == 0xBF {
        Some((Encoding::Utf8, 3))
    } else {
        None
    }
}

/// Decode `bytes` (BOM already stripped) as `encoding`.
fn decode(bytes: &[u8], encoding: Encoding) -> Result<String> {
    match encoding {
        Encoding::Utf8 => String::from_utf8(bytes.to_vec()).map_err(Error::from),
        Encoding::Utf16Le => decode_utf16(bytes, false),
        Encoding::Utf16Be => decode_utf16(bytes, true),
    }
}

/// Decode UTF-16 code units. A trailing odd byte is ignored, matching
/// the C++ `_buffer.assign(reinterpret_cast<const char16_t*>(...))` which
/// reads `size / 2` units. Unpaired surrogates are replaced with U+FFFD.
fn decode_utf16(bytes: &[u8], big_endian: bool) -> Result<String> {
    let units = bytes
        .chunks_exact(2)
        .map(|c| {
            if big_endian {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect::<Vec<u16>>();
    Ok(String::from_utf16_lossy(&units))
}

/// Normalize `CRLF`, lone `CR` and `LF` to `\n` in a single pass.
/// `CRLF` counts as one terminator, not two.
fn normalize_line_endings(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

/// A decoded text stream read from an underlying [`BinaryStream`].
///
/// The whole remaining content of the underlying stream is read and
/// decoded when the stream is opened; the underlying stream is not
/// touched afterwards (it ends up at EOF, like the C++ constructor which
/// slurps the entire storage). Line terminators are normalized to `\n`,
/// so [`read_line`](TextReadStream::read_line) yields lines without their
/// terminators.
#[derive(Debug)]
pub struct TextReadStream<S: BinaryStream> {
    inner: S,
    encoding: Encoding,
    text: String,
    pos: usize,
}

impl<S: BinaryStream> TextReadStream<S> {
    /// Read all remaining bytes of `inner`, detect the encoding from the
    /// BOM (defaulting to UTF-8), decode and normalize line endings.
    pub fn open(mut inner: S) -> Result<Self> {
        let mut raw = Vec::new();
        inner.read_to_end(&mut raw)?;
        let (encoding, bom_size) = detect_bom(&raw).unwrap_or((Encoding::Utf8, 0));
        let text = normalize_line_endings(&decode(&raw[bom_size..], encoding)?);
        Ok(Self {
            inner,
            encoding,
            text,
            pos: 0,
        })
    }

    /// The encoding the stream was decoded as.
    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    /// Read the next line, without its terminator.
    ///
    /// Returns `None` at end-of-stream. An empty line yields `Some("")`;
    /// a final line without a trailing newline is returned normally.
    pub fn read_line(&mut self) -> Result<Option<String>> {
        if self.pos >= self.text.len() {
            return Ok(None);
        }
        let rest = &self.text[self.pos..];
        let line = match rest.find('\n') {
            Some(i) => {
                let line = &rest[..i];
                self.pos += i + 1;
                line
            }
            None => {
                self.pos = self.text.len();
                rest
            }
        };
        Ok(Some(line.to_string()))
    }

    /// Read all remaining text (already line-normalized).
    pub fn read_to_end(&mut self) -> Result<String> {
        let rest = self.text[self.pos..].to_string();
        self.pos = self.text.len();
        Ok(rest)
    }

    /// The underlying stream (positioned at its end).
    pub fn into_inner(self) -> S {
        self.inner
    }
}

/// Line-ending mode used by [`TextWriteStream`] when writing lines.
///
/// The default is [`Crlf`](LineEnding::Crlf), matching the
/// `TJS_TEXT_OUT_CRLF` define of the reference build
/// (`reference/cpp/core/CMakeLists.txt`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineEnding {
    /// `\r\n` (CRLF) — Windows, and the reference build's default.
    #[default]
    Crlf,
    /// `\n` (LF) — Unix.
    Lf,
    /// `\r` (CR) — classic Mac.
    Cr,
}

impl LineEnding {
    /// The literal terminator bytes.
    pub fn as_str(self) -> &'static str {
        match self {
            LineEnding::Crlf => "\r\n",
            LineEnding::Lf => "\n",
            LineEnding::Cr => "\r",
        }
    }
}

/// A UTF-8 text writer over an underlying [`BinaryStream`].
///
/// Writes UTF-8 bytes; by default no BOM is emitted and lines are
/// terminated with CRLF. The C++ `tTVPTextWriteStream` instead always
/// writes UTF-16LE with a `FF FE` BOM and performs no line-ending
/// transformation at the stream level (call sites decide via
/// `TJS_TEXT_OUT_CRLF`) — the Rust port folds that choice into
/// [`LineEnding`].
#[derive(Debug)]
pub struct TextWriteStream<S: BinaryStream> {
    inner: S,
    line_ending: LineEnding,
    bom_pending: bool,
}

impl<S: BinaryStream> TextWriteStream<S> {
    /// Create a text writer over `inner` with CRLF line endings and no
    /// BOM.
    pub fn new(inner: S) -> Self {
        Self {
            inner,
            line_ending: LineEnding::default(),
            bom_pending: false,
        }
    }

    /// Configure whether a UTF-8 BOM is written at the start of the
    /// output (default: off).
    pub fn with_bom(mut self, on: bool) -> Self {
        self.bom_pending = on;
        self
    }

    /// The configured line-ending mode.
    pub fn line_ending(&self) -> LineEnding {
        self.line_ending
    }

    /// Change the line-ending mode used by
    /// [`write_line`](TextWriteStream::write_line) /
    /// [`write_newline`](TextWriteStream::write_newline).
    pub fn set_line_ending(&mut self, ending: LineEnding) {
        self.line_ending = ending;
    }

    /// Write `s` verbatim (no normalization, no terminator appended).
    pub fn write_str(&mut self, s: &str) -> io::Result<()> {
        self.ensure_bom()?;
        self.inner.write_all(s.as_bytes())
    }

    /// Write the configured line terminator.
    pub fn write_newline(&mut self) -> io::Result<()> {
        self.ensure_bom()?;
        self.inner.write_all(self.line_ending.as_str().as_bytes())
    }

    /// Write `s` followed by the configured line terminator.
    pub fn write_line(&mut self, s: &str) -> io::Result<()> {
        self.write_str(s)?;
        self.write_newline()
    }

    /// Flush the underlying stream, if it buffers.
    ///
    /// [`BinaryStream`] has no flush method; this is a no-op kept for
    /// `std::io::Write`-style symmetry.
    pub fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }

    /// The underlying stream.
    pub fn into_inner(self) -> S {
        self.inner
    }

    fn ensure_bom(&mut self) -> io::Result<()> {
        if self.bom_pending {
            self.bom_pending = false;
            self.inner.write_all(&UTF8_BOM)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MemoryStream;

    fn read(bytes: &[u8]) -> TextReadStream<MemoryStream> {
        TextReadStream::open(MemoryStream::from_bytes(bytes)).unwrap()
    }

    fn collect_lines(mut r: TextReadStream<MemoryStream>) -> Vec<String> {
        let mut lines = Vec::new();
        while let Some(line) = r.read_line().unwrap() {
            lines.push(line);
        }
        lines
    }

    // --- BOM detection -------------------------------------------------

    #[test]
    fn bom_utf8() {
        let mut bytes = UTF8_BOM.to_vec();
        bytes.extend_from_slice("hello".as_bytes());
        let mut r = read(&bytes);
        assert_eq!(r.encoding(), Encoding::Utf8);
        assert_eq!(r.read_to_end().unwrap(), "hello");
    }

    #[test]
    fn bom_utf16le() {
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend_from_slice(
            &"hello"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let mut r = read(&bytes);
        assert_eq!(r.encoding(), Encoding::Utf16Le);
        assert_eq!(r.read_to_end().unwrap(), "hello");
    }

    #[test]
    fn bom_utf16be() {
        let mut bytes = vec![0xFE, 0xFF];
        bytes.extend_from_slice(
            &"hello"
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<_>>(),
        );
        let mut r = read(&bytes);
        assert_eq!(r.encoding(), Encoding::Utf16Be);
        assert_eq!(r.read_to_end().unwrap(), "hello");
    }

    #[test]
    fn no_bom_defaults_to_utf8() {
        let mut r = read("hello".as_bytes());
        assert_eq!(r.encoding(), Encoding::Utf8);
        assert_eq!(r.read_to_end().unwrap(), "hello");
    }

    #[test]
    fn utf16_handles_non_ascii_and_odd_trailing_byte() {
        let text = "日本語テキスト";
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend_from_slice(
            &text
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        // append an odd stray byte; it must be ignored (C++ reads size/2 units)
        bytes.push(0x42);
        let mut r = read(&bytes);
        assert_eq!(r.read_to_end().unwrap(), text);
    }

    #[test]
    fn utf16be_non_ascii() {
        let text = "日本語";
        let mut bytes = vec![0xFE, 0xFF];
        bytes.extend_from_slice(
            &text
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<_>>(),
        );
        let mut r = read(&bytes);
        assert_eq!(r.read_to_end().unwrap(), text);
    }

    #[test]
    fn invalid_utf8_is_an_error() {
        // 0xC3 followed by a non-continuation byte is invalid UTF-8, and
        // there is no BOM to deflect it to UTF-16
        let err = TextReadStream::open(MemoryStream::from_bytes(&[0xC3, 0x28])).unwrap_err();
        assert!(matches!(err, Error::InvalidUtf8(_)));
        // ...and a UTF-8 BOM does not excuse invalid continuation bytes
        let mut bytes = UTF8_BOM.to_vec();
        bytes.extend_from_slice(&[0xC3, 0x28]);
        let err = TextReadStream::open(MemoryStream::from_bytes(&bytes)).unwrap_err();
        assert!(matches!(err, Error::InvalidUtf8(_)));
    }

    // --- line endings --------------------------------------------------

    #[test]
    fn crlf_lf_cr_all_terminate() {
        // CRLF, lone CR and lone LF all terminate
        let r = read(b"a\r\nb\rc\nd");
        assert_eq!(collect_lines(r), ["a", "b", "c", "d"]);
    }

    #[test]
    fn crlf_is_one_terminator() {
        let r = read(b"a\r\nb");
        assert_eq!(collect_lines(r), ["a", "b"]);
    }

    #[test]
    fn read_to_end_normalizes() {
        let mut r = read(b"a\r\nb\rc\nd");
        assert_eq!(r.read_to_end().unwrap(), "a\nb\nc\nd");
    }

    #[test]
    fn empty_lines_are_preserved() {
        let r = read(b"a\n\nb\n");
        assert_eq!(collect_lines(r), ["a", "", "b"]);
    }

    #[test]
    fn final_line_without_newline() {
        let r = read(b"a\nb");
        assert_eq!(collect_lines(r), ["a", "b"]);
    }

    #[test]
    fn trailing_newline_yields_no_empty_last_line() {
        let r = read(b"a\n");
        assert_eq!(collect_lines(r), ["a"]);
    }

    #[test]
    fn empty_stream() {
        let mut r = read(b"");
        assert_eq!(r.read_line().unwrap(), None);
        assert_eq!(r.read_to_end().unwrap(), "");
    }

    #[test]
    fn utf16_with_line_endings() {
        let text = "one\r\ntwo\rthree\nfour";
        let mut bytes = vec![0xFF, 0xFE];
        bytes.extend_from_slice(
            &text
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let r = read(&bytes);
        assert_eq!(collect_lines(r), ["one", "two", "three", "four"]);
    }

    #[test]
    fn read_line_then_read_to_end() {
        let mut r = read(b"a\nb\nc");
        assert_eq!(r.read_line().unwrap().as_deref(), Some("a"));
        assert_eq!(r.read_to_end().unwrap(), "b\nc");
        assert_eq!(r.read_to_end().unwrap(), "");
    }

    #[test]
    fn non_ascii_lines() {
        let r = read("こんにちは\n世界".as_bytes());
        assert_eq!(collect_lines(r), ["こんにちは", "世界"]);
    }

    // --- writer --------------------------------------------------------

    #[test]
    fn line_ending_modes() {
        let cases: [(LineEnding, &[u8]); 3] = [
            (LineEnding::Crlf, b"a\r\n"),
            (LineEnding::Lf, b"a\n"),
            (LineEnding::Cr, b"a\r"),
        ];
        for (ending, expected) in cases {
            let mut w = TextWriteStream::new(MemoryStream::new());
            w.set_line_ending(ending);
            w.write_line("a").unwrap();
            assert_eq!(w.into_inner().into_inner(), expected, "mode {ending:?}");
        }
    }

    #[test]
    fn default_is_crlf() {
        let mut w = TextWriteStream::new(MemoryStream::new());
        assert_eq!(w.line_ending(), LineEnding::Crlf);
        w.write_line("a").unwrap();
        w.write_line("b").unwrap();
        assert_eq!(w.into_inner().into_inner(), b"a\r\nb\r\n");
    }

    #[test]
    fn write_str_is_verbatim() {
        let mut w = TextWriteStream::new(MemoryStream::new());
        w.write_str("a\r\nb\nc").unwrap();
        assert_eq!(w.into_inner().into_inner(), b"a\r\nb\nc");
    }

    #[test]
    fn roundtrip_bomless_utf8() {
        let mut w = TextWriteStream::new(MemoryStream::new());
        w.write_line("hello").unwrap();
        w.write_line("日本語").unwrap();
        let bytes = w.into_inner().into_inner();
        assert_eq!(bytes, "hello\r\n日本語\r\n".as_bytes());

        let r = read(&bytes);
        assert_eq!(r.encoding(), Encoding::Utf8);
        assert_eq!(collect_lines(r), ["hello", "日本語"]);
    }

    #[test]
    fn roundtrip_with_utf8_bom() {
        let mut w = TextWriteStream::new(MemoryStream::new()).with_bom(true);
        w.write_line("hello").unwrap();
        let bytes = w.into_inner().into_inner();
        assert!(bytes.starts_with(&UTF8_BOM));
        let r = read(&bytes);
        assert_eq!(r.encoding(), Encoding::Utf8);
        assert_eq!(collect_lines(r), ["hello"]);
    }

    #[test]
    fn roundtrip_reader_writer_line_modes() {
        // CRLF out, normalized \n in
        let mut w = TextWriteStream::new(MemoryStream::new());
        for line in ["x", "y", "z"] {
            w.write_line(line).unwrap();
        }
        let bytes = w.into_inner().into_inner();
        assert_eq!(bytes, b"x\r\ny\r\nz\r\n");
        let mut r = read(&bytes);
        assert_eq!(r.read_line().unwrap().as_deref(), Some("x"));
        assert_eq!(r.read_to_end().unwrap(), "y\nz\n");
    }

    #[test]
    fn empty_write_produces_empty_output() {
        let w = TextWriteStream::new(MemoryStream::new());
        assert!(w.into_inner().into_inner().is_empty());
    }
}
