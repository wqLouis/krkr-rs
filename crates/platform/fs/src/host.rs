//! Host (`std::fs`) backend internals.

use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use api::error::FsError;
use api::fs::{FsPath, OpenMode};

use crate::{FileHandle, FileState};

/// Joins a relative host path against `base`, passing absolute paths through.
pub(crate) fn resolve_host(base: &Path, path: &FsPath) -> Result<PathBuf, FsError> {
    let raw = path.as_host().ok_or_else(|| FsError::InvalidPath {
        path: path_to_string(path),
        reason: "the host backend requires an FsPath::Host".to_owned(),
    })?;
    Ok(if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        base.join(raw)
    })
}

/// Opens a host file, rejecting directories before touching them.
pub(crate) fn open_host(base: &Path, path: &FsPath, mode: OpenMode) -> Result<FileState, FsError> {
    let full = resolve_host(base, path)?;
    let display = full.display().to_string();
    if let Ok(meta) = std::fs::metadata(&full)
        && meta.is_dir()
    {
        return Err(FsError::IsADirectory { path: display });
    }

    let mut opts = OpenOptions::new();
    match mode {
        OpenMode::Read => {
            opts.read(true);
        }
        OpenMode::Write => {
            opts.write(true).create(true).truncate(true);
        }
        OpenMode::Append => {
            // Opened without `O_APPEND`; the append position is handled by the
            // initial cursor and by the caller's explicit offsets.
            opts.write(true).create(true);
        }
        OpenMode::ReadWrite => {
            opts.read(true).write(true);
        }
    }
    let file = opts.open(&full).map_err(|e| io_error(&display, e))?;
    let cursor = if mode == OpenMode::Append {
        file.metadata().map_err(|e| io_error(&display, e))?.len()
    } else {
        0
    };
    Ok(FileState {
        handle: FileHandle::Host(file),
        mode,
        cursor,
        path: display,
    })
}

/// Ensures `path` exists and is not a directory.
pub(crate) fn check_host_file(path: &Path) -> Result<(), FsError> {
    let display = path.display().to_string();
    let meta = std::fs::metadata(path).map_err(|e| io_error(&display, e))?;
    if meta.is_dir() {
        Err(FsError::IsADirectory { path: display })
    } else {
        Ok(())
    }
}

/// Ensures `path` exists (file or directory).
pub(crate) fn check_host_exists(path: &Path) -> Result<(), FsError> {
    let display = path.display().to_string();
    std::fs::metadata(path)
        .map(|_| ())
        .map_err(|e| io_error(&display, e))
}

/// Positional read for the host backend (Unix).
#[cfg(unix)]
pub(crate) fn host_read_at(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buf, offset)
}

/// Positional read for the host backend (Windows).
#[cfg(windows)]
pub(crate) fn host_read_at(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buf, offset)
}

/// Positional read for the host backend (other platforms).
#[cfg(not(any(unix, windows)))]
pub(crate) fn host_read_at(_file: &File, _offset: u64, _buf: &mut [u8]) -> std::io::Result<usize> {
    Err(std::io::Error::new(
        ErrorKind::Unsupported,
        "positional reads are not supported on this platform",
    ))
}

/// Positional write for the host backend (Unix).
#[cfg(unix)]
pub(crate) fn host_write_at(file: &File, offset: u64, buf: &[u8]) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.write_at(buf, offset)
}

/// Positional write for the host backend (Windows).
#[cfg(windows)]
pub(crate) fn host_write_at(file: &File, offset: u64, buf: &[u8]) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(buf, offset)
}

/// Positional write for the host backend (other platforms).
#[cfg(not(any(unix, windows)))]
pub(crate) fn host_write_at(_file: &File, _offset: u64, _buf: &[u8]) -> std::io::Result<usize> {
    Err(std::io::Error::new(
        ErrorKind::Unsupported,
        "positional writes are not supported on this platform",
    ))
}

/// Renders an [`FsPath`] as a display string for error messages.
fn path_to_string(path: &FsPath) -> String {
    match path {
        FsPath::Host(p) => p.display().to_string(),
        FsPath::Uri(u) => u.clone(),
    }
}

/// Maps a `std::io::Error` into the typed [`FsError`].
///
/// Every `ErrorKind` that has a matching [`FsError`] variant is preserved so
/// the caller can distinguish "missing", "denied", "is/not a directory" and
/// the other concrete conditions; only an otherwise unclassified kind falls
/// back to [`FsError::Io`].
pub(crate) fn io_error(path: &str, err: std::io::Error) -> FsError {
    match err.kind() {
        ErrorKind::NotFound => FsError::NotFound {
            path: path.to_owned(),
        },
        ErrorKind::PermissionDenied => FsError::PermissionDenied {
            path: path.to_owned(),
        },
        ErrorKind::AlreadyExists => FsError::AlreadyExists {
            path: path.to_owned(),
        },
        ErrorKind::IsADirectory => FsError::IsADirectory {
            path: path.to_owned(),
        },
        ErrorKind::NotADirectory => FsError::NotADirectory {
            path: path.to_owned(),
        },
        ErrorKind::Unsupported => FsError::Unsupported {
            operation: format!("{path}: {err}"),
        },
        _ => FsError::Io {
            message: format!("{path}: {err}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind};

    use api::error::FsError;
    use api::fs::{FsPath, OpenMode};

    use super::io_error;
    use crate::{FileSystem, flush, open, write_at};

    /// Every `ErrorKind` covered by an `FsError` variant is preserved, and an
    /// unclassified kind falls back to `FsError::Io`.
    #[test]
    fn io_error_preserves_typed_kinds() {
        assert!(matches!(
            io_error("/p", Error::from(ErrorKind::NotFound)),
            FsError::NotFound { .. }
        ));
        assert!(matches!(
            io_error("/p", Error::from(ErrorKind::PermissionDenied)),
            FsError::PermissionDenied { .. }
        ));
        assert!(matches!(
            io_error("/p", Error::from(ErrorKind::AlreadyExists)),
            FsError::AlreadyExists { .. }
        ));
        assert!(matches!(
            io_error("/p", Error::from(ErrorKind::IsADirectory)),
            FsError::IsADirectory { .. }
        ));
        assert!(matches!(
            io_error("/p", Error::from(ErrorKind::NotADirectory)),
            FsError::NotADirectory { .. }
        ));
        assert!(matches!(
            io_error("/p", Error::from(ErrorKind::Unsupported)),
            FsError::Unsupported { .. }
        ));
        assert!(matches!(
            io_error("/p", Error::from(ErrorKind::Other)),
            FsError::Io { .. }
        ));
    }

    /// Reference: append mode opens the file with `O_APPEND`
    /// (`reference/cpp/core/base/impl/StorageImpl.cpp:651-652`), so a write
    /// lands at the current end and the initial cursor is the existing length.
    /// A metadata failure is a typed error, not an offset of `0`.
    #[test]
    fn append_cursor_starts_at_the_file_length() {
        let dir = std::env::temp_dir().join(format!("fs-append-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        let fs = FileSystem::host(dir.clone());

        let file = open(&fs, &FsPath::host("a.bin"), OpenMode::Write).expect("create file");
        write_at(&fs, &file, 0, b"hello").expect("write file");
        flush(&fs, &file).expect("flush file");
        drop(file);

        let append = open(&fs, &FsPath::host("a.bin"), OpenMode::Append).expect("append file");
        assert_eq!(append.cursor(), 5);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
