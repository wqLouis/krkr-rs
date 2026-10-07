//! In-memory backend internals.
//!
//! The backing store is a shared resource (`Arc<Mutex<MemoryStore>>`); the
//! per-file cursor and mode live in [`crate::FileState`], so the service
//! object itself stays stateless.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use krkr_api::error::FsError;
use krkr_api::fs::{FsPath, OpenMode};

use crate::{FileHandle, FileState, lock};

/// Mutable contents of an in-memory root.
///
/// This is a resource held behind an `Arc<Mutex<…>>`. It is not per-file
/// handle state: the logical cursor and mode live in [`FileState`].
#[derive(Debug, Default)]
pub(crate) struct MemoryStore {
    /// File contents keyed by normalized relative path.
    pub(crate) files: BTreeMap<PathBuf, Vec<u8>>,
    /// Explicitly created directories, including implicit parents.
    pub(crate) dirs: BTreeSet<PathBuf>,
}

impl MemoryStore {
    /// Returns `true` when `key` is the root or a known directory.
    pub(crate) fn is_dir(&self, key: &Path) -> bool {
        key.as_os_str().is_empty() || self.dirs.contains(key)
    }
}

/// Validates and converts a positional `[offset, offset + len)` region.
///
/// Returns the half-open region as `usize` indices, or a typed error when the
/// offset does not fit `usize` or `offset + len` overflows. There is no
/// artificial size cap: a legitimately large (but representable) region is
/// accepted, and the caller decides how to handle allocation failure. This is
/// the single place the caller-controlled `u64` offset is turned into an
/// index, so an overflowing or unrepresentable offset can never drive a
/// panic-shaped `Vec::resize`.
pub(crate) fn memory_range(path: &str, offset: u64, len: usize) -> Result<(usize, usize), FsError> {
    let len_u64 = u64::try_from(len).map_err(|_| region_error(path, offset))?;
    let end = offset
        .checked_add(len_u64)
        .ok_or_else(|| region_error(path, offset))?;
    let start = usize::try_from(offset).map_err(|_| region_error(path, offset))?;
    let end = usize::try_from(end).map_err(|_| region_error(path, offset))?;
    Ok((start, end))
}

/// Builds the typed error for an offset the current platform cannot address.
fn region_error(path: &str, offset: u64) -> FsError {
    FsError::Unsupported {
        operation: format!(
            "the in-memory backend cannot address offset {offset} in {path}: \
             the offset or resulting size is not representable on this platform"
        ),
    }
}

/// Opens an in-memory file, enforcing the same parent-exists rule as host IO.
pub(crate) fn open_memory(
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
pub(crate) fn memory_key(path: &FsPath) -> Result<PathBuf, FsError> {
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
pub(crate) fn parent_is(child: &Path, dir: &Path) -> bool {
    child.parent().map(|p| p == dir).unwrap_or(false)
}

/// Returns the final component of `path` as an owned string.
pub(crate) fn file_name_string(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Joins `suffix` onto `base`, treating an empty suffix as `base` itself.
pub(crate) fn join_suffix(base: &Path, suffix: &Path) -> PathBuf {
    if suffix.as_os_str().is_empty() {
        base.to_path_buf()
    } else {
        base.join(suffix)
    }
}

/// Verifies that `key`'s parent exists as a directory in the store.
pub(crate) fn ensure_memory_parent(store: &MemoryStore, key: &Path) -> Result<(), FsError> {
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
