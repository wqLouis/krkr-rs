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
        file.metadata().map(|m| m.len()).unwrap_or(0)
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
        _ => FsError::Io {
            message: format!("{path}: {err}"),
        },
    }
}
