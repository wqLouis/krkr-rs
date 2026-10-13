//! Public file-system operations.
//!
//! Every function dispatches on the backend selected in the [`FileSystem`]
//! and is re-exported at the crate root by `lib.rs`, so the public paths
//! (`fs::open`, `fs::read_at`, …) are unchanged by the module
//! split.

use api::error::FsError;
use api::fs::{DirEntry, FileMetadata, FileStat, FsPath, OpenMode, SeekFrom};

use crate::host::{
    check_host_exists, check_host_file, host_permissions_mode, host_read_at, host_write_at,
    io_error, open_host, resolve_host,
};
use crate::memory::{
    ensure_memory_parent, file_name_string, join_suffix, memory_key, memory_range, open_memory,
    parent_is,
};
use crate::{Backend, FileHandle, FileState, FileSystem, lock};

/// Opens `path` under `fs` with `mode` and returns a caller-owned handle.
///
/// The returned [`FileState`] holds the open handle and the initial cursor
/// (0, or the file length for [`OpenMode::Append`]). No state is retained by
/// `fs`.
pub fn open(fs: &FileSystem, path: &FsPath, mode: OpenMode) -> Result<FileState, FsError> {
    match &fs.backend {
        Backend::Host { base } => open_host(base, path, mode),
        Backend::Memory { store } => open_memory(store, path, mode),
    }
}

/// Returns metadata for `path`.
pub fn stat(fs: &FileSystem, path: &FsPath) -> Result<FileStat, FsError> {
    match &fs.backend {
        Backend::Host { base } => {
            let full = resolve_host(base, path)?;
            let display = full.display().to_string();
            let meta = std::fs::metadata(&full).map_err(|e| io_error(&display, e))?;
            let modified_ms = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .and_then(|d| u64::try_from(d.as_millis()).ok());
            Ok(FileStat {
                size: if meta.is_dir() { 0 } else { meta.len() },
                is_dir: meta.is_dir(),
                modified_ms,
            })
        }
        Backend::Memory { store } => {
            let key = memory_key(path)?;
            let display = key.display().to_string();
            let guard = lock(store);
            if key.as_os_str().is_empty() || guard.is_dir(&key) {
                Ok(FileStat {
                    size: 0,
                    is_dir: true,
                    modified_ms: None,
                })
            } else if let Some(bytes) = guard.files.get(&key) {
                Ok(FileStat {
                    size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
                    is_dir: false,
                    modified_ms: None,
                })
            } else {
                Err(FsError::NotFound { path: display })
            }
        }
    }
}

/// Returns `true` if `path` exists and can be stat-ed.
pub fn exists(fs: &FileSystem, path: &FsPath) -> bool {
    stat(fs, path).is_ok()
}

/// Lists the immediate children of directory `path`.
///
/// Entries are sorted by name for deterministic output. The result contains
/// no `"."` or `".."` entries.
pub fn list(fs: &FileSystem, path: &FsPath) -> Result<Vec<DirEntry>, FsError> {
    match &fs.backend {
        Backend::Host { base } => {
            let full = resolve_host(base, path)?;
            let display = full.display().to_string();
            let meta = std::fs::metadata(&full).map_err(|e| io_error(&display, e))?;
            if !meta.is_dir() {
                return Err(FsError::NotADirectory { path: display });
            }
            let dir = std::fs::read_dir(&full).map_err(|e| io_error(&display, e))?;
            let mut entries = Vec::new();
            for entry in dir {
                let entry = entry.map_err(|e| io_error(&display, e))?;
                let file_type = entry.file_type().map_err(|e| io_error(&display, e))?;
                entries.push(DirEntry {
                    name: entry.file_name().to_string_lossy().into_owned(),
                    is_dir: file_type.is_dir(),
                });
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries)
        }
        Backend::Memory { store } => {
            let key = memory_key(path)?;
            let display = key.display().to_string();
            let guard = lock(store);
            if !key.as_os_str().is_empty() && !guard.is_dir(&key) {
                if guard.files.contains_key(&key) {
                    return Err(FsError::NotADirectory { path: display });
                }
                return Err(FsError::NotFound { path: display });
            }
            let mut entries = Vec::new();
            for dir_key in guard.dirs.iter() {
                if parent_is(dir_key, &key) {
                    entries.push(DirEntry {
                        name: file_name_string(dir_key),
                        is_dir: true,
                    });
                }
            }
            for file_key in guard.files.keys() {
                if parent_is(file_key, &key) {
                    entries.push(DirEntry {
                        name: file_name_string(file_key),
                        is_dir: false,
                    });
                }
            }
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries)
        }
    }
}

/// Recursively creates directory `path` and any missing parents.
pub fn create_dir_all(fs: &FileSystem, path: &FsPath) -> Result<(), FsError> {
    match &fs.backend {
        Backend::Host { base } => {
            let full = resolve_host(base, path)?;
            std::fs::create_dir_all(&full).map_err(|e| io_error(&full.display().to_string(), e))
        }
        Backend::Memory { store } => {
            let key = memory_key(path)?;
            let mut guard = lock(store);
            let mut cur = std::path::PathBuf::new();
            for comp in key.components() {
                cur.push(comp);
                guard.dirs.insert(cur.clone());
            }
            Ok(())
        }
    }
}

/// Removes the regular file at `path`.
///
/// The in-memory backend does not remove directories here; use the middle
/// layer's namespace logic if directory removal is required.
pub fn remove_file(fs: &FileSystem, path: &FsPath) -> Result<(), FsError> {
    match &fs.backend {
        Backend::Host { base } => {
            let full = resolve_host(base, path)?;
            let display = full.display().to_string();
            if let Ok(meta) = std::fs::metadata(&full)
                && meta.is_dir()
            {
                return Err(FsError::IsADirectory { path: display });
            }
            std::fs::remove_file(&full).map_err(|e| io_error(&display, e))
        }
        Backend::Memory { store } => {
            let key = memory_key(path)?;
            let display = key.display().to_string();
            let mut guard = lock(store);
            if key.as_os_str().is_empty() || guard.is_dir(&key) {
                return Err(FsError::IsADirectory { path: display });
            }
            match guard.files.remove(&key) {
                Some(_) => Ok(()),
                None => Err(FsError::NotFound { path: display }),
            }
        }
    }
}

/// Copies the file `from` to `to`.
///
/// The destination's parent directory must already exist, matching
/// `std::fs::copy`.
pub fn copy(fs: &FileSystem, from: &FsPath, to: &FsPath) -> Result<(), FsError> {
    match &fs.backend {
        Backend::Host { base } => {
            let src = resolve_host(base, from)?;
            let dst = resolve_host(base, to)?;
            check_host_file(&src)?;
            std::fs::copy(&src, &dst)
                .map(|_| ())
                .map_err(|e| io_error(&dst.display().to_string(), e))
        }
        Backend::Memory { store } => {
            let src = memory_key(from)?;
            let dst = memory_key(to)?;
            let mut guard = lock(store);
            if guard.is_dir(&src) {
                return Err(FsError::IsADirectory {
                    path: src.display().to_string(),
                });
            }
            if !guard.files.contains_key(&src) {
                return Err(FsError::NotFound {
                    path: src.display().to_string(),
                });
            }
            if guard.is_dir(&dst) {
                return Err(FsError::IsADirectory {
                    path: dst.display().to_string(),
                });
            }
            ensure_memory_parent(&guard, &dst)?;
            // `copy` preserves the source; copying a file onto itself is a
            // no-op.
            if src == dst {
                return Ok(());
            }
            let Some(bytes) = guard.files.get(&src).cloned() else {
                return Err(FsError::NotFound {
                    path: src.display().to_string(),
                });
            };
            guard.files.insert(dst, bytes);
            Ok(())
        }
    }
}

/// Renames (moves) `from` to `to`.
pub fn rename(fs: &FileSystem, from: &FsPath, to: &FsPath) -> Result<(), FsError> {
    match &fs.backend {
        Backend::Host { base } => {
            let src = resolve_host(base, from)?;
            let dst = resolve_host(base, to)?;
            check_host_exists(&src)?;
            std::fs::rename(&src, &dst).map_err(|e| io_error(&dst.display().to_string(), e))
        }
        Backend::Memory { store } => {
            let src = memory_key(from)?;
            let dst = memory_key(to)?;
            let mut guard = lock(store);
            let src_is_dir = !src.as_os_str().is_empty() && guard.dirs.contains(&src);
            let src_is_file = guard.files.contains_key(&src);
            if !src_is_dir && !src_is_file {
                return Err(FsError::NotFound {
                    path: src.display().to_string(),
                });
            }

            if src_is_file {
                if guard.is_dir(&dst) {
                    return Err(FsError::IsADirectory {
                        path: dst.display().to_string(),
                    });
                }
                ensure_memory_parent(&guard, &dst)?;
                if let Some(bytes) = guard.files.remove(&src) {
                    guard.files.insert(dst, bytes);
                }
                return Ok(());
            }

            // Directory move: relocate every descendant directory and file by
            // rewriting the `src` prefix to `dst`. Keys that do not start with
            // `src` have no suffix to rewrite and are skipped.
            let dir_entries: Vec<(std::path::PathBuf, std::path::PathBuf)> = guard
                .dirs
                .iter()
                .filter_map(|k| {
                    k.strip_prefix(&src)
                        .ok()
                        .map(|suffix| (k.clone(), suffix.to_path_buf()))
                })
                .collect();
            let file_entries: Vec<(std::path::PathBuf, std::path::PathBuf, Vec<u8>)> = guard
                .files
                .iter()
                .filter_map(|(k, v)| {
                    k.strip_prefix(&src)
                        .ok()
                        .map(|suffix| (k.clone(), suffix.to_path_buf(), v.clone()))
                })
                .collect();
            for (k, _) in &dir_entries {
                guard.dirs.remove(k);
            }
            for (k, _, _) in &file_entries {
                guard.files.remove(k);
            }
            for (_, suffix) in dir_entries {
                guard.dirs.insert(join_suffix(&dst, &suffix));
            }
            for (_, suffix, bytes) in file_entries {
                guard.files.insert(join_suffix(&dst, &suffix), bytes);
            }
            let mut cur = std::path::PathBuf::new();
            for comp in dst.components() {
                cur.push(comp);
                guard.dirs.insert(cur.clone());
            }
            Ok(())
        }
    }
}

/// Reads up to `buf.len()` bytes at `offset` from `file`.
///
/// Positional: the logical cursor is not modified. Returns the number of
/// bytes read; `0` means end of file.
pub fn read_at(
    fs: &FileSystem,
    file: &FileState,
    offset: u64,
    buf: &mut [u8],
) -> Result<usize, FsError> {
    if !file.mode.is_readable() {
        return Err(FsError::InvalidState {
            message: format!("{} was not opened for reading", file.path),
        });
    }
    match (&fs.backend, &file.handle) {
        (Backend::Host { .. }, FileHandle::Host(handle)) => {
            host_read_at(handle, offset, buf).map_err(|e| io_error(&file.path, e))
        }
        (Backend::Memory { .. }, FileHandle::Memory { store, key }) => {
            let guard = lock(store);
            let Some(bytes) = guard.files.get(key) else {
                return Err(FsError::NotFound {
                    path: file.path.clone(),
                });
            };
            // An offset that does not fit `usize` cannot exist in a `Vec`, so
            // it is past EOF. Without this check a 32-bit target would
            // silently truncate the caller's `u64` offset to a low position.
            let Ok(start) = usize::try_from(offset) else {
                return Ok(0);
            };
            if start >= bytes.len() {
                return Ok(0);
            }
            let n = buf.len().min(bytes.len() - start);
            buf[..n].copy_from_slice(&bytes[start..start + n]);
            Ok(n)
        }
        _ => Err(backend_mismatch()),
    }
}

/// Writes `buf` at `offset` in `file`, extending the file with zero bytes if
/// `offset` is past the current end.
///
/// Positional: the logical cursor is not modified. Returns the number of
/// bytes written.
///
/// There is no artificial size cap. The in-memory backend grows the backing
/// buffer with `try_reserve`, so a write the platform cannot represent or
/// allocate returns a typed [`FsError`] rather than aborting the process.
pub fn write_at(
    fs: &FileSystem,
    file: &FileState,
    offset: u64,
    buf: &[u8],
) -> Result<usize, FsError> {
    if !file.mode.is_writable() {
        return Err(FsError::InvalidState {
            message: format!("{} was not opened for writing", file.path),
        });
    }
    match (&fs.backend, &file.handle) {
        (Backend::Host { .. }, FileHandle::Host(handle)) => {
            host_write_at(handle, offset, buf).map_err(|e| io_error(&file.path, e))
        }
        (Backend::Memory { .. }, FileHandle::Memory { store, key }) => {
            let mut guard = lock(store);
            let Some(bytes) = guard.files.get_mut(key) else {
                return Err(FsError::NotFound {
                    path: file.path.clone(),
                });
            };
            // Validate before resizing: `offset`/`offset + buf.len()` must be
            // representable, so `resize` can never be asked for an overflowing
            // or unrepresentable region. No maximum size is imposed here; a
            // legitimate large file is permitted.
            let (start, end) = memory_range(&file.path, offset, buf.len())?;
            if end > bytes.len() {
                // Reserve fallibly so an allocation the platform cannot serve
                // is a typed error instead of a panic/abort.
                let additional = end - bytes.len();
                bytes.try_reserve(additional).map_err(|_| FsError::Io {
                    message: format!(
                        "cannot grow {} to {end} bytes: allocation failed",
                        file.path
                    ),
                })?;
                bytes.resize(end, 0);
            }
            bytes[start..end].copy_from_slice(buf);
            Ok(buf.len())
        }
        _ => Err(backend_mismatch()),
    }
}

/// Returns the current size of `file` in bytes.
pub fn file_size(fs: &FileSystem, file: &FileState) -> Result<u64, FsError> {
    match (&fs.backend, &file.handle) {
        (Backend::Host { .. }, FileHandle::Host(handle)) => handle
            .metadata()
            .map(|m| m.len())
            .map_err(|e| io_error(&file.path, e)),
        (Backend::Memory { .. }, FileHandle::Memory { store, key }) => {
            let guard = lock(store);
            guard
                .files
                .get(key)
                .map(|v| u64::try_from(v.len()).unwrap_or(u64::MAX))
                .ok_or_else(|| FsError::NotFound {
                    path: file.path.clone(),
                })
        }
        _ => Err(backend_mismatch()),
    }
}

/// Flushes any buffered data for `file` to its backing store.
///
/// The in-memory backend has no buffering, so this is a no-op.
pub fn flush(fs: &FileSystem, file: &FileState) -> Result<(), FsError> {
    match (&fs.backend, &file.handle) {
        (Backend::Host { .. }, FileHandle::Host(handle)) => {
            handle.sync_all().map_err(|e| io_error(&file.path, e))
        }
        (Backend::Memory { .. }, FileHandle::Memory { .. }) => Ok(()),
        _ => Err(backend_mismatch()),
    }
}

/// Reads at `file`'s cursor and advances the cursor by the number of bytes
/// read.
///
/// This is the sequential convenience used by the middle layer's stream
/// wrapper. Returns `0` at end of file, leaving the cursor unchanged.
pub fn read_seq(fs: &FileSystem, file: &mut FileState, buf: &mut [u8]) -> Result<usize, FsError> {
    let n = read_at(fs, file, file.cursor, buf)?;
    file.cursor += u64::try_from(n).unwrap_or(u64::MAX);
    Ok(n)
}

/// Moves `file`'s logical cursor to `from` and returns the new position.
///
/// This only affects [`read_seq`] / [`seek`]; positional IO ignores the
/// cursor.
pub fn seek(fs: &FileSystem, file: &mut FileState, from: SeekFrom) -> Result<u64, FsError> {
    let target: i128 = match from {
        SeekFrom::Start(off) => off as i128,
        SeekFrom::End(off) => file_size(fs, file)? as i128 + off as i128,
        SeekFrom::Current(off) => file.cursor as i128 + off as i128,
    };
    if target < 0 {
        return Err(FsError::Io {
            message: format!("seek before the start of {}", file.path),
        });
    }
    // Checked instead of `target as u64`, which would silently wrap a
    // `SeekFrom::End`/`Current` target that exceeds `u64::MAX`.
    let target = u64::try_from(target).map_err(|_| FsError::Io {
        message: format!("seek target for {} is out of range", file.path),
    })?;
    file.cursor = target;
    Ok(file.cursor)
}

/// Error returned when a [`FileState`] does not belong to the given
/// [`FileSystem`].
fn backend_mismatch() -> FsError {
    FsError::InvalidState {
        message: "the file handle does not belong to this file system".to_owned(),
    }
}

/// Returns full metadata for `path`.
///
/// Unlike [`stat`], this reports the raw size of a directory and the three
/// host timestamps plus the permission bits the platform exposes. A time the
/// backend cannot represent is `None`.
pub fn metadata(fs: &FileSystem, path: &FsPath) -> Result<FileMetadata, FsError> {
    match &fs.backend {
        Backend::Host { base } => {
            let full = resolve_host(base, path)?;
            let display = full.display().to_string();
            let meta = std::fs::metadata(&full).map_err(|e| io_error(&display, e))?;
            Ok(FileMetadata {
                size: meta.len(),
                is_dir: meta.is_dir(),
                is_file: meta.is_file(),
                modified_ms: meta.modified().ok().and_then(system_time_millis),
                created_ms: meta.created().ok().and_then(system_time_millis),
                accessed_ms: meta.accessed().ok().and_then(system_time_millis),
                mode: host_permissions_mode(&meta),
            })
        }
        Backend::Memory { .. } => {
            let file_stat = stat(fs, path)?;
            Ok(FileMetadata {
                size: file_stat.size,
                is_dir: file_stat.is_dir,
                is_file: !file_stat.is_dir,
                modified_ms: None,
                created_ms: None,
                accessed_ms: None,
                mode: None,
            })
        }
    }
}

/// Converts a `SystemTime` to milliseconds since the Unix epoch, negative for
/// pre-epoch times.
///
/// A duration that does not fit `i64` is `None`; the bottom layer does not
/// fabricate a time it cannot represent.
fn system_time_millis(time: std::time::SystemTime) -> Option<i64> {
    match time.duration_since(std::time::UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).ok(),
        Err(error) => i64::try_from(error.duration().as_millis())
            .ok()
            .map(|millis| -millis),
    }
}

/// Returns `true` if `path` exists and is a directory.
///
/// A stat failure is a typed error; the bottom layer keeps a permission or I/O
/// failure distinguishable from “not a directory”. A caller that needs the
/// reference's “missing or unreadable is `false`” convenience decides so at
/// its own boundary.
pub fn is_dir(fs: &FileSystem, path: &FsPath) -> Result<bool, FsError> {
    metadata(fs, path).map(|meta| meta.is_dir)
}

/// Returns `true` if `path` exists and is a regular file (symlinks are
/// followed).
///
/// A stat failure is a typed error; the bottom layer keeps a permission or I/O
/// failure distinguishable from “not a regular file”. A caller that needs the
/// reference's “missing or unreadable is `false`” convenience decides so at
/// its own boundary.
pub fn is_file(fs: &FileSystem, path: &FsPath) -> Result<bool, FsError> {
    metadata(fs, path).map(|meta| meta.is_file)
}

/// Removes the empty directory at `path`.
///
/// A directory with children is a typed error on both backends; use the
/// namespace layer for a recursive delete. The in-memory backend removes only
/// directories it explicitly created (or an implicit parent), never the root.
pub fn remove_dir(fs: &FileSystem, path: &FsPath) -> Result<(), FsError> {
    match &fs.backend {
        Backend::Host { base } => {
            let full = resolve_host(base, path)?;
            let display = full.display().to_string();
            std::fs::remove_dir(&full).map_err(|e| io_error(&display, e))
        }
        Backend::Memory { store } => {
            let key = memory_key(path)?;
            let display = key.display().to_string();
            let mut guard = lock(store);
            if !key.as_os_str().is_empty() && !guard.is_dir(&key) {
                if guard.files.contains_key(&key) {
                    return Err(FsError::NotADirectory { path: display });
                }
                return Err(FsError::NotFound { path: display });
            }
            if key.as_os_str().is_empty() {
                return Err(FsError::Io {
                    message: format!("cannot remove the storage root: {display}"),
                });
            }
            let has_children = guard.dirs.iter().any(|dir| parent_is(dir, &key))
                || guard.files.keys().any(|file| parent_is(file, &key));
            if has_children {
                return Err(FsError::Io {
                    message: format!("directory not empty: {display}"),
                });
            }
            guard.dirs.remove(&key);
            Ok(())
        }
    }
}

/// Reads the entire file at `path` into a new buffer.
///
/// A path that names a directory is [`FsError::IsADirectory`], and a missing
/// path is [`FsError::NotFound`]. The buffer grows fallibly, so a file the
/// platform cannot allocate is a typed error rather than a panic.
pub fn read(fs: &FileSystem, path: &FsPath) -> Result<Vec<u8>, FsError> {
    match &fs.backend {
        Backend::Host { base } => {
            let full = resolve_host(base, path)?;
            let display = full.display().to_string();
            std::fs::read(&full).map_err(|e| io_error(&display, e))
        }
        Backend::Memory { store } => {
            let key = memory_key(path)?;
            let display = key.display().to_string();
            let guard = lock(store);
            if key.as_os_str().is_empty() || guard.is_dir(&key) {
                return Err(FsError::IsADirectory { path: display });
            }
            match guard.files.get(&key) {
                Some(bytes) => Ok(bytes.clone()),
                None => Err(FsError::NotFound { path: display }),
            }
        }
    }
}

/// Writes `bytes` to `path`, creating it or truncating an existing file.
///
/// The destination's parent directory must already exist, matching
/// `std::fs::write`.
pub fn write(fs: &FileSystem, path: &FsPath, bytes: &[u8]) -> Result<(), FsError> {
    match &fs.backend {
        Backend::Host { base } => {
            let full = resolve_host(base, path)?;
            let display = full.display().to_string();
            std::fs::write(&full, bytes).map_err(|e| io_error(&display, e))
        }
        Backend::Memory { store } => {
            let key = memory_key(path)?;
            let display = key.display().to_string();
            let mut guard = lock(store);
            if guard.is_dir(&key) {
                return Err(FsError::IsADirectory { path: display });
            }
            ensure_memory_parent(&guard, &key)?;
            guard.files.insert(key, bytes.to_vec());
            Ok(())
        }
    }
}

/// Appends `buf` at the current end of `file`.
///
/// The position is read from the file's current length on every call, so the
/// logical cursor is not modified. The file must have been opened for
/// writing.
pub fn append(fs: &FileSystem, file: &FileState, buf: &[u8]) -> Result<usize, FsError> {
    let end = file_size(fs, file)?;
    write_at(fs, file, end, buf)
}

/// Sets both the access and the modification time of `path` to `seconds`
/// since the Unix epoch.
///
/// This is the host `utimes`/`SetFileTime` shape: the bottom layer opens the
/// file for writing (a read-only file is a typed error) and applies the same
/// instant to both timestamps. The in-memory backend does not track times and
/// reports [`FsError::Unsupported`].
pub fn set_times(fs: &FileSystem, path: &FsPath, seconds: i64) -> Result<(), FsError> {
    match &fs.backend {
        Backend::Host { base } => {
            let full = resolve_host(base, path)?;
            let display = full.display().to_string();
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&full)
                .map_err(|e| io_error(&display, e))?;
            let when = if seconds >= 0 {
                std::time::UNIX_EPOCH + std::time::Duration::from_secs(seconds as u64)
            } else {
                std::time::UNIX_EPOCH - std::time::Duration::from_secs(seconds.unsigned_abs())
            };
            file.set_times(
                std::fs::FileTimes::new()
                    .set_accessed(when)
                    .set_modified(when),
            )
            .map_err(|e| io_error(&display, e))
        }
        Backend::Memory { .. } => Err(FsError::Unsupported {
            operation: "the in-memory backend does not track file times".to_owned(),
        }),
    }
}
