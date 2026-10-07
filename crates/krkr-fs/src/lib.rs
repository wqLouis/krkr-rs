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
//!
//! The public operations live in the private [`ops`] module and are
//! re-exported here, so their `krkr_fs::…` paths are stable while the crate
//! is split into host, memory and operation modules.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use krkr_api::fs::OpenMode;

mod host;
mod memory;
mod ops;

pub use ops::{
    copy, create_dir_all, exists, file_size, flush, list, open, read_at, read_seq, remove_file,
    rename, seek, stat, write_at,
};

use memory::MemoryStore;

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
///
/// Marked `#[non_exhaustive]`: new backends may be added without it being a
/// breaking change for callers that match on this enum.
#[derive(Clone, Debug, Default)]
#[non_exhaustive]
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
    pub(crate) handle: FileHandle,
    /// Access mode requested at open time.
    pub(crate) mode: OpenMode,
    /// Logical cursor used by [`read_seq`] and [`seek`].
    pub(crate) cursor: u64,
    /// Human-readable logical path, for error messages.
    pub(crate) path: String,
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
pub(crate) enum FileHandle {
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

/// Locks `mutex`, recovering from poisoning so one panic cannot brick IO.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
