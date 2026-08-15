//! Error types for the archive readers.

use std::path::PathBuf;

/// Errors produced while opening or reading a zip / 7z / tar archive.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying I/O failure (file not found, read error, ...).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The file extension is not one of `.zip`, `.7z`, `.tar`.
    #[error("unsupported archive format (expected .zip/.7z/.tar): {0}")]
    UnsupportedFormat(PathBuf),

    /// No entry with the requested name exists in the archive.
    #[error("entry not found in archive: {0}")]
    NotFound(String),

    /// The file is not a valid archive of the expected format, or its
    /// contents could not be decompressed.
    #[error("corrupt or unsupported archive: {0}")]
    Corrupt(String),
}

/// Convenience alias.
pub type Result<T> = std::result::Result<T, Error>;
