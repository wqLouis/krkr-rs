//! Error types for the XP3 reader.

use std::path::PathBuf;

/// Errors produced while opening or reading an XP3 archive.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("not an XP3 archive: {0}")]
    NotAnXp3(PathBuf),

    #[error("archive index chain exceeded {0} blocks (corrupt archive?)")]
    IndexChainTooLong(usize),

    #[error("bad index flag 0x{0:02x}")]
    BadIndexFlag(u8),

    #[error("corrupt index data: {0}")]
    CorruptIndex(&'static str),

    #[error("entry is protected (DRM): {0}")]
    Protected(String),

    #[error("unknown segment encode method {0} (flags 0x{1:08x})")]
    UnknownSegmentMethod(u32, u32),

    #[error("zlib decompression failed for segment of `{0}`")]
    Inflate(String),

    #[error("entry not found in archive: {0}")]
    NotFound(String),

    #[error("archive has no index (empty?)")]
    NoIndex,
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;
