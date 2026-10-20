//! Typed errors for the PSB and PSD readers.

/// Convenience result alias for the PSB reader.
pub type PsbResult<T> = Result<T, PsbError>;

/// A PSB/MDF container or decompression failure.
///
/// The reader never returns a partially parsed document: every failure is
/// reported here.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PsbError {
    /// The four-byte magic was not `PSB\0`.
    #[error("not a valid PSB file (signature {0:?})")]
    BadSignature([u8; 4]),
    /// The stream starts with the `MDF`/`MFL` archive magic.
    #[error("this looks like an MDF/MFL archive, not a PSB file")]
    ArchiveNotPsb,
    /// The format version is outside the supported 1..=3 range.
    #[error("unsupported PSB version {0}")]
    UnsupportedVersion(u16),
    /// The header advertises an encrypted file and no seed was supplied.
    #[error("encrypted PSB file")]
    Encrypted,
    /// The stream ended before the requested field could be read.
    #[error("truncated PSB data while reading {0}")]
    Truncated(&'static str),
    /// The stream is structurally invalid (bad type byte, bad index, ...).
    #[error("malformed PSB data: {0}")]
    Format(String),
    /// The embedded Emote LZ4 frame or block was invalid.
    #[error("LZ4 decode failed: {0}")]
    Lz4(String),
    /// The MDF zlib payload could not be inflated.
    #[error("stream decompression failed: {0}")]
    Decompress(String),
}

/// Convenience result alias for the PSD reader and decoder.
pub type PsdResult<T> = Result<T, PsdError>;

/// A PSD parse, decompression or decode failure.
///
/// A damaged channel or descriptor is reported here, never zero-filled.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PsdError {
    /// The input ended before a required field could be read.
    #[error("unexpected end of PSD data")]
    Eof,
    /// The file is structurally invalid.
    #[error("invalid PSD: {0}")]
    Invalid(String),
    /// A compressed channel could not be expanded.
    #[error("PSD decompression failed: {0}")]
    Decompress(String),
}
