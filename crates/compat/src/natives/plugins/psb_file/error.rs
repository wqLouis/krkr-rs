//! Typed errors for the PSB container parser.

/// Convenience result alias for the PSB parser.
pub(crate) type PsbResult<T> = Result<T, PsbError>;

/// A typed PSB/MDF parse, decompression or image-bridge failure.
///
/// The parser never falls back to a partially parsed result: every failure is
/// surfaced here and turned into a TJS callback error by [`super`].
#[derive(Debug, thiserror::Error)]
pub(crate) enum PsbError {
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

impl From<PsbError> for String {
    fn from(error: PsbError) -> Self {
        error.to_string()
    }
}
