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

/// A single entry produced by a directory listing.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct DirEntry {
    /// File name relative to the listed directory (no separators).
    pub name: String,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fs_path_host_and_uri_accessors() {
        let host = FsPath::host("/tmp/game");
        assert!(host.is_host());
        assert!(!host.is_uri());
        assert_eq!(host.as_host(), Some(Path::new("/tmp/game")));
        assert_eq!(host.as_uri(), None);

        let uri = FsPath::uri("content://game/data");
        assert!(uri.is_uri());
        assert_eq!(uri.as_uri(), Some("content://game/data"));
        assert_eq!(uri.as_host(), None);
    }

    #[test]
    fn fs_path_from_host_conversions() {
        // Only `PathBuf`/`&Path` convert implicitly (to `Host`); URI strings
        // must use the explicit `FsPath::uri` constructor so a plain `&str`
        // is never silently reinterpreted as a platform URI.
        assert_eq!(
            FsPath::from(PathBuf::from("/c")),
            FsPath::Host(PathBuf::from("/c"))
        );
        assert_eq!(
            FsPath::from(Path::new("/d")),
            FsPath::Host(PathBuf::from("/d"))
        );
        assert_eq!(
            FsPath::uri("content://x"),
            FsPath::Uri("content://x".to_owned())
        );
    }

    #[test]
    fn open_mode_capabilities() {
        assert!(OpenMode::Read.is_readable());
        assert!(!OpenMode::Read.is_writable());
        assert!(OpenMode::ReadWrite.is_readable());
        assert!(OpenMode::ReadWrite.is_writable());
        assert!(!OpenMode::Append.is_readable());
        assert!(OpenMode::Append.is_writable());
        assert_eq!(OpenMode::default(), OpenMode::Read);
    }

    #[test]
    fn file_stat_is_file() {
        assert!(FileStat::default().is_file());
        assert!(
            !FileStat {
                size: 0,
                is_dir: true,
                modified_ms: Some(1),
            }
            .is_file()
        );
    }
}
