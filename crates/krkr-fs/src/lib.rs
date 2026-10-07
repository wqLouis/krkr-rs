//! krkr-rs bottom file system.
//!
//! This crate implements the stateless [`FileSystem`] service and the
//! explicit [`FileState`] value described in
//! `docs/migration/03-middle-layer-api.md` §1 and
//! `docs/migration/06-file-system.md`.
//!
//! * [`FileSystem`] carries only immutable configuration (a backend kind and,
//!   for the host backend, a base directory). It has no mutable state, no
//!   caches and no open-file registry.
//! * [`FileState`] is the open handle plus the logical cursor. It is created
//!   by [`open`] and owned by the middle layer (L2), which stores every
//!   handle and performs all position decisions.
//! * All byte IO is positional ([`read_at`] / [`write_at`]); [`read_seq`] is a
//!   convenience that reads at the cursor and advances it.
//!
//! Two backends are provided:
//!
//! * [`FileSystem::host`] — `std::fs` rooted at a base directory.
//! * [`FileSystem::memory`] — an in-memory store, ideal for tests and the
//!   wasm-shaped future. Its backing store is a shared resource
//!   (`Arc<Mutex<MemoryStore>>`); no per-file state hides outside
//!   [`FileState`].
//!
//! The storage namespace, `.xp3` addressing and case-insensitive resolution
//! are deliberately absent: they belong to the middle layer.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use krkr_api::error::FsError;
use krkr_api::fs::{DirEntry, FileStat, FsPath, OpenMode, SeekFrom};

/// Backend kind and base configuration of a [`FileSystem`].
///
/// This is private state: callers select a backend through
/// [`FileSystem::host`] / [`FileSystem::memory`] / [`FsConfig`].
#[derive(Clone, Debug)]
enum Backend {
    /// A host directory used as the base for relative paths.
    Host {
        /// Directory joined with relative [`FsPath::Host`] paths.
        base: PathBuf,
    },
    /// An in-memory store shared by clones of the file system.
    Memory {
        /// Shared backing store (a resource, not per-file handle state).
        store: Arc<Mutex<MemoryStore>>,
    },
}

/// Configuration for constructing a [`FileSystem`].
///
/// Mirrors the two implemented backends. URI roots are intentionally absent
/// until an Android SAF / APK asset backend is added.
#[derive(Clone, Debug, Default)]
pub enum FsConfig {
    /// A `std::fs` root: relative host paths resolve against `base`.
    Host {
        /// Base directory for relative paths.
        base: PathBuf,
    },
    /// A fresh, private in-memory root.
    #[default]
    Memory,
}

/// A stateless file-system service.
///
/// The object stores only configuration — a backend kind and, for the host
/// backend, a base directory. It keeps no mutable fields, no caches and no
/// table of open files. All evolving state lives in the caller-owned
/// [`FileState`] values it returns from [`open`].
#[derive(Clone, Debug)]
pub struct FileSystem {
    /// Backend configuration selected at construction time.
    backend: Backend,
}

impl FileSystem {
    /// Creates a host-path root rooted at `base`.
    ///
    /// Relative [`FsPath::Host`] paths are joined to `base`; absolute paths
    /// are used as given. Namespace and sandbox policy are the middle
    /// layer's responsibility.
    pub fn host(base: impl Into<PathBuf>) -> Self {
        Self {
            backend: Backend::Host { base: base.into() },
        }
    }

    /// Creates an in-memory root backed by a fresh, private store.
    ///
    /// Two clones of the returned object share the same store; two separate
    /// calls to `memory` do not.
    pub fn memory() -> Self {
        Self {
            backend: Backend::Memory {
                store: Arc::new(Mutex::new(MemoryStore::default())),
            },
        }
    }

    /// Creates a file system from an [`FsConfig`].
    pub fn from_config(config: FsConfig) -> Self {
        match config {
            FsConfig::Host { base } => Self::host(base),
            FsConfig::Memory => Self::memory(),
        }
    }

    /// Returns the host base directory, or `None` for the in-memory backend.
    pub fn base(&self) -> Option<&Path> {
        match &self.backend {
            Backend::Host { base } => Some(base.as_path()),
            Backend::Memory { .. } => None,
        }
    }

    /// Returns `true` if this file system uses the host-path backend.
    pub fn is_host(&self) -> bool {
        matches!(self.backend, Backend::Host { .. })
    }

    /// Returns `true` if this file system uses the in-memory backend.
    pub fn is_memory(&self) -> bool {
        matches!(self.backend, Backend::Memory { .. })
    }
}

/// Creates a file system from an [`FsConfig`].
///
/// Convenience alias for [`FileSystem::from_config`].
pub fn create_file_system(config: FsConfig) -> FileSystem {
    FileSystem::from_config(config)
}

/// The open handle plus the logical cursor for one file.
///
/// Created by [`open`], owned and stored by the middle layer. The bottom
/// layer never keeps a registry of `FileState`s. Positional reads and writes
/// ([`read_at`] / [`write_at`]) do not move the cursor; [`read_seq`] does.
#[derive(Debug)]
pub struct FileState {
    /// Concrete handle: a host file or an in-memory key reference.
    handle: FileHandle,
    /// Access mode requested at open time.
    mode: OpenMode,
    /// Logical cursor used by [`read_seq`] and [`seek`].
    cursor: u64,
    /// Human-readable logical path, for error messages.
    path: String,
}

impl FileState {
    /// Returns the current logical cursor, in bytes from the start.
    pub fn cursor(&self) -> u64 {
        self.cursor
    }

    /// Returns the access mode this file was opened with.
    pub fn mode(&self) -> OpenMode {
        self.mode
    }
}

/// Concrete open handle held by a [`FileState`].
#[derive(Debug)]
enum FileHandle {
    /// A host file opened through `std::fs`.
    Host(File),
    /// An in-memory file referenced by key in a shared store.
    Memory {
        /// Shared backing store.
        store: Arc<Mutex<MemoryStore>>,
        /// Normalized key of the file within the store.
        key: PathBuf,
    },
}

// ---------------------------------------------------------------------------
// In-memory store
// ---------------------------------------------------------------------------

/// Mutable contents of an in-memory root.
///
/// This is a resource held behind an `Arc<Mutex<…>>`. It is not per-file
/// handle state: the logical cursor and mode live in [`FileState`].
#[derive(Debug, Default)]
struct MemoryStore {
    /// File contents keyed by normalized relative path.
    files: BTreeMap<PathBuf, Vec<u8>>,
    /// Explicitly created directories, including implicit parents.
    dirs: BTreeSet<PathBuf>,
}

impl MemoryStore {
    /// Returns `true` when `key` is the root or a known directory.
    fn is_dir(&self, key: &Path) -> bool {
        key.as_os_str().is_empty() || self.dirs.contains(key)
    }
}

// ---------------------------------------------------------------------------
// Public operations
// ---------------------------------------------------------------------------

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
                .map(|d| d.as_millis() as u64);
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
                    size: bytes.len() as u64,
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
            let mut cur = PathBuf::new();
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
            let Some(bytes) = guard.files.get(&src).cloned() else {
                return Err(FsError::NotFound {
                    path: src.display().to_string(),
                });
            };
            if guard.is_dir(&dst) {
                return Err(FsError::IsADirectory {
                    path: dst.display().to_string(),
                });
            }
            ensure_memory_parent(&guard, &dst)?;
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
                let bytes = guard.files.remove(&src).expect("checked file exists");
                guard.files.insert(dst, bytes);
                return Ok(());
            }

            // Directory move: relocate every descendant directory and file by
            // rewriting the `src` prefix to `dst`.
            let dir_keys: Vec<PathBuf> = guard
                .dirs
                .iter()
                .filter(|k| k.starts_with(&src))
                .cloned()
                .collect();
            let file_entries: Vec<(PathBuf, Vec<u8>)> = guard
                .files
                .iter()
                .filter(|(k, _)| k.starts_with(&src))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            for k in &dir_keys {
                guard.dirs.remove(k);
            }
            for (k, _) in &file_entries {
                guard.files.remove(k);
            }
            for k in dir_keys {
                let suffix = k.strip_prefix(&src).unwrap_or_else(|_| Path::new(""));
                guard.dirs.insert(join_suffix(&dst, suffix));
            }
            for (k, bytes) in file_entries {
                let suffix = k.strip_prefix(&src).unwrap_or_else(|_| Path::new(""));
                guard.files.insert(join_suffix(&dst, suffix), bytes);
            }
            let mut cur = PathBuf::new();
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
        return Err(FsError::Io {
            message: format!("{} was not opened for reading", file.path),
        });
    }
    match (&fs.backend, &file.handle) {
        (Backend::Host { .. }, FileHandle::Host(handle)) => {
            host_read_at(handle, offset, buf).map_err(|e| io_error(&file.path, e))
        }
        (Backend::Memory { .. }, FileHandle::Memory { store, key }) => {
            let guard = lock(store);
            match guard.files.get(key) {
                Some(bytes) => {
                    let start = offset as usize;
                    if start >= bytes.len() {
                        return Ok(0);
                    }
                    let n = buf.len().min(bytes.len() - start);
                    buf[..n].copy_from_slice(&bytes[start..start + n]);
                    Ok(n)
                }
                None => Err(FsError::NotFound {
                    path: file.path.clone(),
                }),
            }
        }
        _ => Err(backend_mismatch()),
    }
}

/// Writes `buf` at `offset` in `file`, extending the file with zero bytes if
/// `offset` is past the current end.
///
/// Positional: the logical cursor is not modified. Returns the number of
/// bytes written.
pub fn write_at(
    fs: &FileSystem,
    file: &FileState,
    offset: u64,
    buf: &[u8],
) -> Result<usize, FsError> {
    if !file.mode.is_writable() {
        return Err(FsError::Io {
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
            let start = offset as usize;
            if start > bytes.len() {
                bytes.resize(start, 0);
            }
            let end = start + buf.len();
            if end > bytes.len() {
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
                .map(|v| v.len() as u64)
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
    file.cursor += n as u64;
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
    file.cursor = target as u64;
    Ok(file.cursor)
}

// ---------------------------------------------------------------------------
// Host backend internals
// ---------------------------------------------------------------------------

/// Joins a relative host path against `base`, passing absolute paths through.
fn resolve_host(base: &Path, path: &FsPath) -> Result<PathBuf, FsError> {
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
fn open_host(base: &Path, path: &FsPath, mode: OpenMode) -> Result<FileState, FsError> {
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
fn check_host_file(path: &Path) -> Result<(), FsError> {
    let display = path.display().to_string();
    let meta = std::fs::metadata(path).map_err(|e| io_error(&display, e))?;
    if meta.is_dir() {
        Err(FsError::IsADirectory { path: display })
    } else {
        Ok(())
    }
}

/// Ensures `path` exists (file or directory).
fn check_host_exists(path: &Path) -> Result<(), FsError> {
    let display = path.display().to_string();
    std::fs::metadata(path)
        .map(|_| ())
        .map_err(|e| io_error(&display, e))
}

/// Positional read for the host backend (Unix).
#[cfg(unix)]
fn host_read_at(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buf, offset)
}

/// Positional read for the host backend (Windows).
#[cfg(windows)]
fn host_read_at(file: &File, offset: u64, buf: &mut [u8]) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buf, offset)
}

/// Positional read for the host backend (other platforms).
#[cfg(not(any(unix, windows)))]
fn host_read_at(_file: &File, _offset: u64, _buf: &mut [u8]) -> std::io::Result<usize> {
    Err(std::io::Error::new(
        ErrorKind::Unsupported,
        "positional reads are not supported on this platform",
    ))
}

/// Positional write for the host backend (Unix).
#[cfg(unix)]
fn host_write_at(file: &File, offset: u64, buf: &[u8]) -> std::io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.write_at(buf, offset)
}

/// Positional write for the host backend (Windows).
#[cfg(windows)]
fn host_write_at(file: &File, offset: u64, buf: &[u8]) -> std::io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_write(buf, offset)
}

/// Positional write for the host backend (other platforms).
#[cfg(not(any(unix, windows)))]
fn host_write_at(_file: &File, _offset: u64, _buf: &[u8]) -> std::io::Result<usize> {
    Err(std::io::Error::new(
        ErrorKind::Unsupported,
        "positional writes are not supported on this platform",
    ))
}

// ---------------------------------------------------------------------------
// In-memory backend internals
// ---------------------------------------------------------------------------

/// Opens an in-memory file, enforcing the same parent-exists rule as host IO.
fn open_memory(
    store: &Arc<Mutex<MemoryStore>>,
    path: &FsPath,
    mode: OpenMode,
) -> Result<FileState, FsError> {
    let key = memory_key(path)?;
    let display = key.display().to_string();
    let mut guard = lock(store);

    if matches!(mode, OpenMode::Write | OpenMode::Append) {
        ensure_memory_parent(&guard, &key)?;
    }

    match mode {
        OpenMode::Read => {
            if guard.is_dir(&key) {
                return Err(FsError::IsADirectory { path: display });
            }
            if !guard.files.contains_key(&key) {
                return Err(FsError::NotFound { path: display });
            }
        }
        OpenMode::Write => {
            guard.files.insert(key.clone(), Vec::new());
        }
        OpenMode::Append => {
            if guard.is_dir(&key) {
                return Err(FsError::IsADirectory { path: display });
            }
            guard.files.entry(key.clone()).or_default();
        }
        OpenMode::ReadWrite => {
            if guard.is_dir(&key) {
                return Err(FsError::IsADirectory { path: display });
            }
            if !guard.files.contains_key(&key) {
                return Err(FsError::NotFound { path: display });
            }
        }
    }

    let cursor = if mode == OpenMode::Append {
        guard.files.get(&key).map(|v| v.len() as u64).unwrap_or(0)
    } else {
        0
    };
    drop(guard);

    Ok(FileState {
        handle: FileHandle::Memory {
            store: Arc::clone(store),
            key,
        },
        mode,
        cursor,
        path: display,
    })
}

/// Normalizes an [`FsPath`] into an in-memory key.
///
/// `Host` paths are canonicalized (absolute prefixes, `.` and `..` removed);
/// `Uri` paths are unsupported by the in-memory backend.
fn memory_key(path: &FsPath) -> Result<PathBuf, FsError> {
    match path {
        FsPath::Host(p) => Ok(normalize_memory_path(p)),
        FsPath::Uri(uri) => Err(FsError::Unsupported {
            operation: format!("the in-memory backend cannot address URI {uri}"),
        }),
    }
}

/// Normalizes a path into a store key (no root prefix, `.`/`..` resolved).
fn normalize_memory_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

/// Returns `true` when `child`'s parent is exactly `dir`.
fn parent_is(child: &Path, dir: &Path) -> bool {
    child.parent().map(|p| p == dir).unwrap_or(false)
}

/// Returns the final component of `path` as an owned string.
fn file_name_string(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Joins `suffix` onto `base`, treating an empty suffix as `base` itself.
fn join_suffix(base: &Path, suffix: &Path) -> PathBuf {
    if suffix.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(suffix)
    }
}

/// Verifies that `key`'s parent exists as a directory in the store.
fn ensure_memory_parent(store: &MemoryStore, key: &Path) -> Result<(), FsError> {
    if let Some(parent) = key.parent()
        && !parent.as_os_str().is_empty()
        && !store.is_dir(parent)
    {
        return Err(FsError::NotFound {
            path: parent.display().to_string(),
        });
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Renders an [`FsPath`] as a display string for error messages.
fn path_to_string(path: &FsPath) -> String {
    match path {
        FsPath::Host(p) => p.display().to_string(),
        FsPath::Uri(u) => u.clone(),
    }
}

/// Maps a `std::io::Error` into the typed [`FsError`].
fn io_error(path: &str, err: std::io::Error) -> FsError {
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

/// Error returned when a [`FileState`] does not belong to the given
/// [`FileSystem`].
fn backend_mismatch() -> FsError {
    FsError::Unsupported {
        operation: "the file handle does not belong to this file system".to_owned(),
    }
}

/// Locks `mutex`, recovering from poisoning so one panic cannot brick IO.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
