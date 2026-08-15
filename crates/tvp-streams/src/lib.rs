//! Binary and text stream abstractions ported from KiriKiri (TVP).
//!
//! This crate is a pure-Rust port of the stream layer of the reference
//! engine (`reference/cpp/core/base/BinaryStream.*`, `UtilStreams.*`,
//! `TextStream.cpp` and the `tTJSBinaryStream` / `iTJSTextStream`
//! interfaces from `reference/cpp/core/tjs2/tjs.h`). It has no
//! dependency on the C++ codebase or on other workspace crates.
//!
//! ## Modules
//!
//! * [`binary`] — the [`BinaryStream`] trait (read/write/seek/size over a
//!   byte sink, matching `tTJSBinaryStream`) plus [`MemoryStream`], an
//!   in-memory implementation matching `tTVPMemoryStream`, and a
//!   [`BinaryStream`] implementation for [`std::fs::File`].
//! * [`limited`] — [`LimitedStream`], a read-only window over another
//!   stream (the Rust counterpart of `tTVPPartialStream`, used for XP3
//!   archive segments).
//! * [`reader`] — little-endian primitive codecs (`u16le`/`u32le`/`u64le`
//!   and signed counterparts) used by the [`BinaryStream`] convenience
//!   methods.
//! * [`text`] — [`TextReadStream`] / [`TextWriteStream`] with BOM
//!   detection and line-ending normalization, mirroring the semantics of
//!   `tTVPTextReadStream` / `tTVPTextWriteStream` in `TextStream.cpp`.
//!
//! ## Key C++ semantics honored
//!
//! * `Seek` never moves past the end of the stream and never goes
//!   negative; out-of-range seeks leave the position unchanged. The Rust
//!   port additionally reports them as `io::ErrorKind::InvalidInput`
//!   instead of silently ignoring them (see [`BinaryStream::seek`]).
//! * `Read` at end-of-stream returns `Ok(0)` (like the C++ `Read` which
//!   returns the actually-read size).
//! * `tTJSBinaryStream::ReadBuffer(_, 0)` reads everything remaining;
//!   here that is [`BinaryStream::read_to_end`].
//! * Text input detects the BOM (`EF BB BF` → UTF-8, `FF FE` →
//!   UTF-16LE, `FE FF` → UTF-16BE); a BOM-less stream is UTF-8, matching
//!   `G_DefaultReadEncoding = "UTF-8"` in this codebase.
//! * Text input treats `CRLF`, lone `CR` and lone `LF` all as line
//!   terminators and normalizes them to `\n`.
//! * Text output defaults to CRLF line endings, matching the
//!   `TJS_TEXT_OUT_CRLF` define used by the C++ build.

pub mod binary;
pub mod limited;
pub mod reader;
pub mod text;

pub use binary::{BinaryStream, MemoryStream};
pub use limited::LimitedStream;
pub use reader::{
    read_i8, read_i16le, read_i32le, read_i64le, read_u8, read_u16le, read_u32le, read_u64le,
    write_i8, write_i16le, write_i32le, write_i64le, write_u8, write_u16le, write_u32le,
    write_u64le,
};
pub use text::{Encoding, LineEnding, TextReadStream, TextWriteStream};

/// Errors produced by the text stream layer.
///
/// The binary stream layer itself reports [`std::io::Error`] directly;
/// this type adds text-specific failure modes on top.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying I/O failure.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Input could not be decoded as UTF-8 (only possible for BOM-less or
    /// UTF-8-BOM input; UTF-16 input never fails — unpaired surrogates
    /// are replaced with U+FFFD, see [`text`]).
    #[error("input is not valid UTF-8: {0}")]
    InvalidUtf8(#[from] std::string::FromUtf8Error),
}

/// Convenience alias for [`Error`].
pub type Result<T> = std::result::Result<T, Error>;
