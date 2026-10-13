//! File-system surface types.
//!
//! These are the values the middle layer passes to (and receives from) the
//! bottom `FileSystem` object. The bottom layer owns raw path/URI byte I/O;
//! namespaces, `.xp3` addressing and case rules stay in the middle layer and
//! never appear here.

use std::path::{Path, PathBuf};

/// A location understood by the bottom file system.
///
/// `Host` is a native path (Linux paths and Android app-private/resolved
/// directories). `Uri` is an opaque identifier for platform providers such
/// as Android SAF or APK assets; the bottom backend decides how to resolve
/// it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum FsPath {
    /// A native host path.
    Host(PathBuf),
    /// An opaque URI or provider-specific identifier.
    Uri(String),
}

impl FsPath {
    /// Creates a host-path location.
    pub fn host(path: impl Into<PathBuf>) -> Self {
        Self::Host(path.into())
    }

    /// Creates a URI location.
    pub fn uri(uri: impl Into<String>) -> Self {
        Self::Uri(uri.into())
    }

    /// Returns the host path when this is [`FsPath::Host`].
    pub fn as_host(&self) -> Option<&Path> {
        match self {
            Self::Host(path) => Some(path.as_path()),
            Self::Uri(_) => None,
        }
    }

    /// Returns the URI string when this is [`FsPath::Uri`].
    pub fn as_uri(&self) -> Option<&str> {
        match self {
            Self::Host(_) => None,
            Self::Uri(uri) => Some(uri.as_str()),
        }
    }

    /// Returns `true` if this is a host path.
    pub const fn is_host(&self) -> bool {
        matches!(self, Self::Host(_))
    }

    /// Returns `true` if this is a URI.
    pub const fn is_uri(&self) -> bool {
        matches!(self, Self::Uri(_))
    }
}

impl From<PathBuf> for FsPath {
    fn from(path: PathBuf) -> Self {
        Self::Host(path)
    }
}

impl From<&Path> for FsPath {
    fn from(path: &Path) -> Self {
        Self::Host(path.to_path_buf())
    }
}

/// Access mode requested when opening a file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum OpenMode {
    /// Read-only access.
    #[default]
    Read,
    /// Write-only access, truncating any existing content.
    Write,
    /// Write-only access positioned at the end of the file.
    Append,
    /// Read and write access without truncation.
    ReadWrite,
}

impl OpenMode {
    /// Returns `true` if reads are permitted.
    pub const fn is_readable(&self) -> bool {
        matches!(self, Self::Read | Self::ReadWrite)
    }

    /// Returns `true` if writes are permitted.
    pub const fn is_writable(&self) -> bool {
        matches!(self, Self::Write | Self::Append | Self::ReadWrite)
    }
}

/// Offset used by seek-style operations over a file cursor.
///
/// This is intentionally backend-agnostic (it does not re-export
/// `std::io::SeekFrom`) so the API crate carries no platform IO type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SeekFrom {
    /// Absolute offset from the start of the file.
    Start(u64),
    /// Offset from the end of the file; may be negative.
    End(i64),
    /// Offset from the current cursor position; may be negative.
    Current(i64),
}

/// Metadata for a file or directory.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FileStat {
    /// Size in bytes (0 for directories).
    pub size: u64,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Last modification time as milliseconds since the Unix epoch, when the
    /// backend can report it.
    pub modified_ms: Option<u64>,
}

impl FileStat {
    /// Returns `true` if the entry is a regular file.
    pub const fn is_file(&self) -> bool {
        !self.is_dir
    }
}

/// Full metadata for a file or directory.
///
/// Where [`FileStat`] carries the fields the storage namespace needs for
/// resolution, this carries the full host metadata a compatibility layer
/// emulating a stat call can observe: the three timestamps and the permission
/// bits. Timestamps are milliseconds since the Unix epoch, negative before it;
/// a time the backend cannot report (a platform without a creation time) is
/// `None` rather than a fabricated value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct FileMetadata {
    /// Size in bytes.
    pub size: u64,
    /// Whether the entry is a directory.
    pub is_dir: bool,
    /// Whether the entry is a regular file (following symlinks).
    pub is_file: bool,
    /// Last modification time as milliseconds since the Unix epoch.
    pub modified_ms: Option<i64>,
    /// Creation (birth) time as milliseconds since the Unix epoch.
    pub created_ms: Option<i64>,
    /// Last access time as milliseconds since the Unix epoch.
    pub accessed_ms: Option<i64>,
    /// Permission bits (the low `0o7777` bits of `st_mode`), when the backend
    /// exposes them.
    pub mode: Option<u32>,
}

/// A single entry produced by a directory listing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct DirEntry {
    /// File name relative to the listed directory (no separators).
    pub name: String,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}
