//! Game storage namespace (L2).
//!
//! A [`Storage`] value owns everything needed to resolve TVP storage names:
//!
//! * an immutable bottom [`FileSystem`] object (created by L1) and the base
//!   directory it is mounted at;
//! * the parsed [`Xp3Archive`] values explicitly mounted by the caller, each
//!   with its L2-owned [`FileState`];
//! * the auto search-path list.
//!
//! This is the middle layer: the bottom layer knows nothing
//! about `.xp3`, `arc.xp3>path` addressing, case-insensitive lookup or the
//! auto-path list. There are no globals or process-wide caches — a `Storage`
//! is a plain value the caller owns, and all `FileState`s live inside it.
//! The XP3 container read + parse itself is L1 (`fs::xp3`); this layer
//! only owns the namespace.
//!
//! Loading is explicit: the caller supplies the archive list and the loader
//! never enumerates the game directory, with one documented exception —
//! [`discover_archives`], a folder scan a caller opts into by passing only a
//! directory (the ergonomic path for the CLI and the Android launcher).
//! See [`Storage::mount_archives`] / [`Storage::mount_archive`].
//!
//! Resolution order mirrors the reference engine:
//!
//! 1. explicit `arc.xp3>path` addressing,
//! 2. disk files (normalized spelling, then the script's spelling, then a
//!    bounded component-wise case-insensitive walk),
//! 3. mounted archives, later mounts first (later-mounted wins),
//! 4. the auto search-path list.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use api::error::FsError;
use api::fs::{FsPath, OpenMode};
use fs::xp3::{self, Xp3Archive, Xp3ExtractionFilter, normalize_in_archive_name};
use fs::{FileState, FileSystem};

/// Windows `FILE_ATTRIBUTE_DIRECTORY` (0x10).
pub const ATTR_DIRECTORY: i64 = 0x10;
/// Windows `FILE_ATTRIBUTE_ARCHIVE` (0x20) — the "normal file" bit.
pub const ATTR_NORMAL: i64 = 0x20;

/// One mounted `.xp3` archive: its logical path plus the parsed index and the
/// open handle that owns its bytes.
struct MountedArchive {
    /// Logical path of the archive file (as mounted).
    path: PathBuf,
    /// Base file name (used for `arc.xp3>path` matching and listings).
    file_name: String,
    /// L2-owned open handle for the archive bytes.
    file: FileState,
    /// Parsed index.
    archive: Xp3Archive,
}

/// A mounted game storage: one base directory plus the explicitly mounted
/// `.xp3` archives.
#[non_exhaustive]
pub struct Storage {
    /// Immutable bottom file-system object.
    fs: FileSystem,
    /// Base (app) directory used for disk assets.
    game_dir: PathBuf,
    /// Mounted archives in mount order; later entries take precedence.
    archives: Vec<MountedArchive>,
    /// Auto search-path prefixes (plain dirs or `arc.xp3>` prefixes).
    auto_paths: Vec<String>,
    /// The per-segment XP3 extraction filter (the `xp3filter` seam), if any.
    ///
    /// L2 owns the state; the stateless L1 reader receives the closure as a
    /// borrow on every read, and it is never stored in L1. `None` means no
    /// filter. The closure is invoked once per decoded segment with
    /// `(file_hash, offset, buffer, file_name, archive_name, file_size)` and
    /// may rewrite `buffer` in place.
    pub xp3_extraction_filter: Option<Box<Xp3ExtractionFilter<'static>>>,
}

/// A resolved storage location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A file on the bottom file system.
    Disk(PathBuf),
    /// A file inside an archive: `(archive path, normalized in-archive name)`.
    Archive(PathBuf, String),
}

/// Metadata returned for a resolved storage entry.
///
/// XP3 indexes carry the uncompressed file size but no timestamps, so archive
/// entries have `None` for all three time fields. Disk entries use the host
/// file-system metadata exposed by [`fs::stat`] (only the modification
/// time is surfaced by the bottom API).
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct StorageMetadata {
    /// Uncompressed size in bytes.
    pub size: u64,
    /// Last modification time, when the backend reports it.
    pub modified: Option<SystemTime>,
    /// Last access time, when the backend reports it.
    pub accessed: Option<SystemTime>,
    /// Creation time, when the backend reports it.
    pub created: Option<SystemTime>,
}

/// Errors while mounting a game storage.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MountError {
    /// The mount path is not a directory.
    #[error("not a directory: {0}")]
    NotADirectory(String),
    /// A bottom file-system operation failed.
    #[error(transparent)]
    Fs(#[from] FsError),
    /// An archive failed to open or parse.
    #[error("archive error: {0}")]
    Xp3(#[from] xp3::Xp3Error),
}

/// Errors while reading or listing storage entries.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StorageReadError {
    /// The requested storage name did not resolve.
    #[error("storage not found: {0}")]
    NotFound(String),
    /// A bottom file-system operation failed.
    #[error(transparent)]
    Fs(#[from] FsError),
    /// An archive read failed.
    #[error("archive error: {0}")]
    Xp3(#[from] xp3::Xp3Error),
}

/// The searched startup script (reference `TVPStartupScriptName`).
pub const STARTUP_SCRIPT: &str = "startup.tjs";
/// The compatibility fallback run when `startup.tjs` is absent or fails
/// (reference `system/Initialize.tjs`).
pub const INITIALIZE_SCRIPT: &str = "system/Initialize.tjs";
/// `patch.tjs`, app-path only (reference `TVPExecuteStartupScript`).
pub const PATCH_SCRIPT: &str = "patch.tjs";
/// `AfterStartup.tjs`, app-path only (reference `TVPExecuteStartupScript`).
pub const AFTER_STARTUP_SCRIPT: &str = "AfterStartup.tjs";

/// The startup scripts resolved against a [`Storage`] (reference
/// `TVPExecuteStartupScript`).
///
/// `patch.tjs` and `AfterStartup.tjs` are **app-path only** (disk, no archive
/// search); `startup.tjs` and the `system/Initialize.tjs` fallback are
/// searched across disk and mounted archives. The caller decides whether to
/// run the fallback from whether `startup.tjs` succeeds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct StartupPlan {
    /// `patch.tjs`, executed first when present (app-path only).
    pub patch: Option<String>,
    /// `startup.tjs`, the main searched startup script.
    pub startup: Option<String>,
    /// `system/Initialize.tjs`, run when `startup.tjs` is absent or fails.
    pub fallback: Option<String>,
    /// `AfterStartup.tjs`, executed last when present (app-path only).
    pub after: Option<String>,
}

impl Storage {
    /// Creates a storage rooted at `base_dir` and mounts each archive in
    /// `archives`, in order.
    ///
    /// Loading is explicit: the caller supplies the archive list (from the
    /// command line or Android Intent extras), and the loader never scans the
    /// folder. Later archives take precedence over earlier ones, so
    /// `--archive data.xp3 --archive patch.xp3` lets `patch.xp3` override
    /// `data.xp3`.
    ///
    /// `fs` is the bottom file-system object; `base_dir` is the app path used
    /// for disk assets. An archive that cannot be opened or parsed is a typed
    /// error, because the caller asked for it.
    pub fn mount_archives(
        fs: FileSystem,
        base_dir: impl Into<PathBuf>,
        archives: impl IntoIterator<Item = impl AsRef<Path>>,
    ) -> Result<Self, MountError> {
        let mut storage = Self::with_base_dir(fs, base_dir.into())?;
        for path in archives {
            storage.mount_archive(path.as_ref())?;
        }
        Ok(storage)
    }

    /// Mounts a directory-scan archive list, skipping an archive that cannot
    /// be read or parsed and reporting it through `warn!`.
    ///
    /// A scan mounts whatever the folder holds, so one stray or corrupt
    /// archive must not abort an otherwise playable mount — the reference
    /// `TVPCheckArchive` catch-and-log
    /// (`reference/cpp/core/base/impl/StorageImpl.cpp:562-587`). Use
    /// [`Self::mount_archives`] for a caller-supplied list.
    pub fn mount_discovered_archives(
        fs: FileSystem,
        base_dir: impl Into<PathBuf>,
        archives: impl IntoIterator<Item = impl AsRef<Path>>,
    ) -> Result<Self, MountError> {
        let mut storage = Self::with_base_dir(fs, base_dir.into())?;
        for path in archives {
            if let Err(error) = storage.mount_archive(path.as_ref()) {
                log::warn!("skipping archive {}: {error}", path.as_ref().display());
            }
        }
        Ok(storage)
    }

    /// Creates an empty storage rooted at `base_dir`, validating that the
    /// base path exists and is a directory. No archives are mounted; use
    /// [`Self::mount_archive`] to add them explicitly.
    fn with_base_dir(fs: FileSystem, base_dir: PathBuf) -> Result<Self, MountError> {
        let meta = fs::stat(&fs, &FsPath::host(&base_dir))?;
        if !meta.is_dir {
            return Err(MountError::NotADirectory(base_dir.display().to_string()));
        }
        Ok(Storage {
            fs,
            game_dir: base_dir,
            archives: Vec::new(),
            auto_paths: Vec::new(),
            xp3_extraction_filter: None,
        })
    }

    /// The bottom file-system object this storage reads through.
    pub fn file_system(&self) -> &FileSystem {
        &self.fs
    }

    /// The game directory path.
    pub fn game_dir(&self) -> &Path {
        &self.game_dir
    }

    /// Number of mounted archives.
    pub fn archive_count(&self) -> usize {
        self.archives.len()
    }

    /// The logical paths of the mounted archives, in mount order.
    pub fn archive_paths(&self) -> impl Iterator<Item = &Path> {
        self.archives.iter().map(|m| m.path.as_path())
    }

    /// Runs `f` for every mounted archive and its parsed index.
    pub fn for_each_archive(&self, mut f: impl FnMut(&Path, &Xp3Archive)) {
        for mounted in &self.archives {
            f(&mounted.path, &mounted.archive);
        }
    }

    /// Mounts one archive by path, parsing its index eagerly.
    ///
    /// The path is resolved against the storage base directory (absolute
    /// paths are used as given) and opened through the bottom file system.
    /// Mounting the same archive twice is a no-op.
    pub fn mount_archive(&mut self, path: impl AsRef<Path>) -> Result<(), MountError> {
        let raw = path.as_ref();
        let path = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.game_dir.join(raw)
        };
        if self.archives.iter().any(|m| m.path == path) {
            return Ok(());
        }
        let file = fs::open(&self.fs, &FsPath::host(&path), OpenMode::Read)?;
        let archive = Xp3Archive::open(&self.fs, &file)?;
        let file_name = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        log::info!("opened {} ({} entries)", path.display(), archive.len());
        self.archives.push(MountedArchive {
            path,
            file_name,
            file,
            archive,
        });
        Ok(())
    }

    /// Unmounts an archive by logical path, resolving relative paths against
    /// the game directory exactly as [`Self::mount_archive`] does. A bare file
    /// name (no directory component) that does not match an exact path removes
    /// the single archive with that base name; if several mounted archives
    /// share the name the request is ambiguous and nothing is removed.
    ///
    /// Returns `true` if an archive was removed.
    pub fn unmount_archive(&mut self, path: impl AsRef<Path>) -> bool {
        let raw = path.as_ref();
        let resolved = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.game_dir.join(raw)
        };
        if let Some(index) = self.archives.iter().position(|m| m.path == resolved) {
            self.archives.remove(index);
            return true;
        }
        // Only a bare file name may fall back to base-name matching, and only
        // when it identifies exactly one archive.
        let bare = raw.parent().is_none_or(|p| p.as_os_str().is_empty());
        if !bare {
            return false;
        }
        let Some(target) = raw
            .file_name()
            .map(|f| f.to_string_lossy().to_ascii_lowercase())
        else {
            return false;
        };
        let mut matches = self
            .archives
            .iter()
            .enumerate()
            .filter(|(_, m)| m.file_name.to_ascii_lowercase() == target);
        let Some((index, _)) = matches.next() else {
            return false;
        };
        if matches.next().is_some() {
            return false;
        }
        self.archives.remove(index);
        true
    }

    /// Appends an auto search path (the `Storages.addAutoPath` semantics).
    /// Duplicates are ignored.
    pub fn add_auto_path(&mut self, path: impl Into<String>) {
        let path = path.into();
        if !self.auto_paths.contains(&path) {
            self.auto_paths.push(path);
        }
    }

    /// Removes an auto search path.
    pub fn remove_auto_path(&mut self, path: &str) {
        self.auto_paths.retain(|p| p != path);
    }

    /// Resolves a storage name to a location, mirroring the reference search
    /// order: explicit `arc.xp3>path` → the current path (the app directory)
    /// → the auto search paths in "later added wins" order → the mounted
    /// archives in later-mount order. Auto-path and explicit archives are
    /// opened on demand when they exist on disk but were not mounted.
    ///
    /// A name that simply does not resolve is `Ok(None)`. A genuine
    /// file-system failure while probing (permission denied, I/O, an
    /// unrepresentable path) is propagated as [`StorageReadError::Fs`]; it is
    /// never collapsed into "not found".
    ///
    /// Storage names are case-insensitive even when the host filesystem is
    /// not. Only the storage-relative part is normalized; the mount prefix
    /// keeps the real filesystem spelling, and a name that escapes the mount
    /// never resolves.
    pub fn find(&self, name: &str) -> Result<Option<Location>, StorageReadError> {
        // Explicit archive addressing: "foo.xp3>data/script.ks". The archive is
        // opened on demand when it exists but was not mounted.
        if let Some((arc, rest)) = name.split_once('>') {
            return self.find_in_archive(arc, rest);
        }

        // Resolve the storage-relative part. Absolute names must point inside
        // the mount, and traversal components are refused, so a name can never
        // resolve outside the game directory.
        let Some(relative) = self.relative_disk_name(name) else {
            return Ok(None);
        };

        // The current path (the app directory) is checked first.
        if let Some(location) = self.find_disk_relative(&relative)? {
            return Ok(Some(location));
        }

        let normalized = normalize_in_archive_name(&relative);
        let base = storage_base(&normalized);

        // Auto paths, last added first (the reference's "later added wins").
        // Archive prefixes are opened on demand; plain directory prefixes go
        // through the same disk resolution as every other disk hit, so
        // directories are rejected and case folding is uniform.
        for entry in self.auto_paths.iter().rev() {
            if let Some((arc, dir)) = entry.split_once('>') {
                let in_archive = format!("{dir}{base}");
                if let Some(loc) = self.find_in_archive(arc, &in_archive)? {
                    return Ok(Some(loc));
                }
            } else {
                let dir = entry.trim_end_matches('/');
                if !dir.is_empty()
                    && let Some(loc) = self.find_disk_relative(&format!("{dir}/{base}"))?
                {
                    return Ok(Some(loc));
                }
            }
        }

        // Mounted archives, later mounts first (later-mounted wins — see
        // `mount_archives`). Looked up by the normalized name.
        for mounted in self.archives.iter().rev() {
            if mounted.archive.entry(&normalized).is_some() {
                return Ok(Some(Location::Archive(mounted.path.clone(), normalized)));
            }
        }
        Ok(None)
    }

    /// Resolves `rest` inside the archive named by `arc`, opening the archive
    /// on demand when it exists on disk but is not in the mount list. A
    /// specific archive miss does not fall through to another archive.
    fn find_in_archive(&self, arc: &str, rest: &str) -> Result<Option<Location>, StorageReadError> {
        let rest = normalize_in_archive_name(rest);
        let arc_name = Path::new(arc)
            .file_name()
            .map(|f| f.to_string_lossy())
            .unwrap_or_default();
        for mounted in self.archives.iter().rev() {
            if mounted.file_name.eq_ignore_ascii_case(&arc_name) {
                if mounted.archive.entry(&rest).is_some() {
                    return Ok(Some(Location::Archive(mounted.path.clone(), rest)));
                }
                return Ok(None);
            }
        }
        // On demand: open and parse the archive when the file exists.
        let Some(path) = self.archive_disk_path(arc) else {
            return Ok(None);
        };
        let file = match fs::open(&self.fs, &FsPath::host(&path), OpenMode::Read) {
            Ok(file) => file,
            Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => {
                return Ok(None);
            }
            Err(error) => return Err(StorageReadError::Fs(error)),
        };
        let archive = Xp3Archive::open(&self.fs, &file)?;
        if archive.entry(&rest).is_some() {
            Ok(Some(Location::Archive(path, rest)))
        } else {
            Ok(None)
        }
    }

    /// The on-disk path of the archive named by `arc`: an absolute path is
    /// used as given, a relative one is resolved case-insensitively under the
    /// game directory.
    fn archive_disk_path(&self, arc: &str) -> Option<PathBuf> {
        if let Some(relative) = self.relative_disk_name(arc)
            && let Ok(Some(Location::Disk(path))) = self.find_disk_relative(&relative)
        {
            return Some(path);
        }
        let raw = Path::new(arc);
        if raw.is_absolute() {
            Some(raw.to_path_buf())
        } else {
            None
        }
    }

    /// The storage-relative part of `name` (using `/` separators), or `None`
    /// when the name is absolute and does not point inside the mount, or
    /// contains a traversal (`..`) component.
    pub fn relative_disk_name(&self, name: &str) -> Option<String> {
        // Fold Windows separators so a `\`-separated storage name behaves like
        // the reference on any host.
        let separators = name.replace('\\', "/");
        let path = Path::new(&separators);
        let relative = if path.is_absolute() {
            if self.game_dir.as_os_str().is_empty() {
                path.to_path_buf()
            } else {
                path.strip_prefix(&self.game_dir).ok()?.to_path_buf()
            }
        } else {
            path.to_path_buf()
        };
        if relative.components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }) {
            return None;
        }
        Some(relative.to_string_lossy().replace('\\', "/"))
    }

    /// Resolves an existing disk file for a storage name, case-insensitively.
    /// Returns the real on-disk path so writers can update the existing file
    /// instead of creating a case-variant duplicate.
    pub fn find_disk(&self, name: &str) -> Result<Option<PathBuf>, StorageReadError> {
        let Some(relative) = self.relative_disk_name(name) else {
            return Ok(None);
        };
        Ok(match self.find_disk_relative(&relative)? {
            Some(Location::Disk(path)) => Some(path),
            Some(Location::Archive(..)) => None,
            None => None,
        })
    }

    /// Disk branch of [`Self::find`], given an already-resolved relative name.
    fn find_disk_relative(&self, relative: &str) -> Result<Option<Location>, StorageReadError> {
        Ok(self.resolve_disk(relative)?.and_then(|(path, is_dir)| {
            if is_dir {
                None
            } else {
                Some(Location::Disk(path))
            }
        }))
    }

    /// Resolves a disk-relative name to an on-disk path plus whether it is a
    /// directory, applying the normalized spelling, then the script's exact
    /// spelling, then the bounded case-insensitive walk. A missing name is
    /// `Ok(None)`; other file-system failures propagate.
    fn resolve_disk(&self, relative: &str) -> Result<Option<(PathBuf, bool)>, StorageReadError> {
        if relative.is_empty() {
            return Ok(None);
        }
        // Reference semantics: storage names are normalized to lowercase
        // before lookup, so the lowercase spelling wins when both a lowercase
        // and a mixed-case file exist.
        let normalized = normalize_in_archive_name(relative);
        let candidate = self.game_dir.join(&normalized);
        match fs::stat(&self.fs, &FsPath::host(&candidate)) {
            Ok(meta) => return Ok(Some((candidate, meta.is_dir))),
            Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => {}
            Err(error) => return Err(StorageReadError::Fs(error)),
        }
        // Fast path: the exact spelling the script used.
        if normalized != relative {
            let exact = self.game_dir.join(relative);
            match fs::stat(&self.fs, &FsPath::host(&exact)) {
                Ok(meta) => return Ok(Some((exact, meta.is_dir))),
                Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => {}
                Err(error) => return Err(StorageReadError::Fs(error)),
            }
        }
        // Bounded, component-wise case-insensitive walk.
        self.find_disk_case_insensitive(&normalized)
    }

    /// Resolves a disk-relative name that must name a directory.
    fn find_disk_dir(&self, relative: &str) -> Result<Option<PathBuf>, StorageReadError> {
        Ok(self
            .resolve_disk(relative)?
            .and_then(|(path, is_dir)| is_dir.then_some(path)))
    }

    /// Bounded component-wise case-insensitive disk walk.
    ///
    /// Directories are entered only when their name matches the target's next
    /// component, so cost is bounded by the path depth rather than the size of
    /// the tree. A file directly at the current level wins over descending;
    /// a directory at the final level is returned as `is_dir` so callers can
    /// distinguish it from a file.
    fn find_disk_case_insensitive(
        &self,
        relative: &str,
    ) -> Result<Option<(PathBuf, bool)>, StorageReadError> {
        fn descend(
            fs: &FileSystem,
            dir: &Path,
            parts: &[&str],
        ) -> Result<Option<(PathBuf, bool)>, StorageReadError> {
            let want = parts[0];
            let entries = match fs::list(fs, &FsPath::host(dir)) {
                Ok(entries) => entries,
                Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => {
                    return Ok(None);
                }
                Err(error) => return Err(StorageReadError::Fs(error)),
            };
            // Directories matching this component are searched after the
            // direct file hit, so a file directly at this level wins.
            let mut subdirs = Vec::new();
            let mut dir_match = None;
            for entry in entries {
                if !entry.name.eq_ignore_ascii_case(want) {
                    continue;
                }
                let path = dir.join(&entry.name);
                if parts.len() == 1 {
                    if !entry.is_dir {
                        return Ok(Some((path, false)));
                    }
                    if dir_match.is_none() {
                        dir_match = Some(path);
                    }
                } else if entry.is_dir {
                    subdirs.push(path);
                }
            }
            if let Some(path) = dir_match {
                return Ok(Some((path, true)));
            }
            for subdir in subdirs {
                if let Some(found) = descend(fs, &subdir, &parts[1..])? {
                    return Ok(Some(found));
                }
            }
            Ok(None)
        }

        let parts: Vec<&str> = relative.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return Ok(None);
        }
        descend(&self.fs, &self.game_dir, &parts)
    }

    /// Runs `f` against the archive at `arc_path`: the mounted index when the
    /// archive is in the mount list, otherwise a transient open of the file on
    /// disk.
    fn with_archive<T>(
        &self,
        arc_path: &Path,
        f: impl FnOnce(&FileState, &Xp3Archive, &str) -> Result<T, StorageReadError>,
    ) -> Result<T, StorageReadError> {
        if let Some(mounted) = self.archives.iter().find(|m| m.path == arc_path) {
            return f(&mounted.file, &mounted.archive, &mounted.file_name);
        }
        let file = fs::open(&self.fs, &FsPath::host(arc_path), OpenMode::Read)?;
        let archive = Xp3Archive::open(&self.fs, &file)?;
        let file_name = arc_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        f(&file, &archive, &file_name)
    }

    /// Reads a storage entry to bytes (disk file or archive member).
    pub fn read(&self, name: &str) -> Result<Vec<u8>, StorageReadError> {
        match self.find(name)? {
            Some(Location::Disk(path)) => {
                read_fs_file(&self.fs, &path).map_err(StorageReadError::Fs)
            }
            Some(Location::Archive(arc_path, in_arc)) => {
                let filter = self.xp3_extraction_filter.as_deref();
                self.with_archive(&arc_path, |file, archive, file_name| {
                    archive
                        .read_with_filter(&self.fs, file, &in_arc, file_name, filter)
                        .map_err(StorageReadError::Xp3)
                })
            }
            None => Err(StorageReadError::NotFound(name.to_string())),
        }
    }

    /// Returns metadata for a resolved storage entry.
    ///
    /// Disk files are read through [`fs::stat`]. For an XP3 entry, `size`
    /// is the uncompressed size from the archive index; archive timestamps
    /// are absent because XP3 has no timestamp fields. Names follow the same
    /// resolution order as [`Self::find`].
    ///
    /// A name that does not resolve is `Ok(None)`; a file-system failure while
    /// probing propagates.
    pub fn stat(&self, name: &str) -> Result<Option<StorageMetadata>, StorageReadError> {
        match self.find(name)? {
            Some(Location::Disk(path)) => {
                let meta = fs::stat(&self.fs, &FsPath::host(&path))?;
                Ok(Some(StorageMetadata {
                    size: meta.size,
                    modified: meta.modified_ms.map(system_time_from_millis),
                    accessed: None,
                    created: None,
                }))
            }
            Some(Location::Archive(arc_path, in_arc)) => {
                self.with_archive(&arc_path, |_file, archive, _file_name| {
                    let entry = archive
                        .entry(&in_arc)
                        .ok_or_else(|| StorageReadError::NotFound(name.to_string()))?;
                    Ok(Some(StorageMetadata {
                        size: entry.org_size,
                        modified: None,
                        accessed: None,
                        created: None,
                    }))
                })
            }
            None => Ok(None),
        }
    }

    /// True if a storage name resolves to a file or a directory.
    ///
    /// This matches `TVPIsExistentStorage`, which reports directories as
    /// existing; [`Self::find`] itself is file-only. A file-system failure
    /// while probing propagates.
    pub fn exists(&self, name: &str) -> Result<bool, StorageReadError> {
        if self.find(name)?.is_some() {
            return Ok(true);
        }
        let Some(relative) = self.relative_disk_name(name) else {
            return Ok(false);
        };
        Ok(self.find_disk_dir(&relative)?.is_some())
    }

    /// Resolve the startup script sequence against this storage (reference
    /// `TVPExecuteStartupScript`).
    ///
    /// Pure storage lookups: `patch.tjs` and `AfterStartup.tjs` are app-path
    /// only (disk, no archive search, matching `TVPIsExistentStorageNoSearch`),
    /// while `startup.tjs` and the `system/Initialize.tjs` fallback are
    /// searched across disk and archives.
    ///
    /// # Errors
    ///
    /// A file-system failure while probing propagates.
    pub fn startup_plan(&self) -> Result<StartupPlan, StorageReadError> {
        Ok(StartupPlan {
            patch: self.app_path_file(PATCH_SCRIPT)?,
            startup: self
                .exists(STARTUP_SCRIPT)?
                .then(|| STARTUP_SCRIPT.to_owned()),
            fallback: self
                .exists(INITIALIZE_SCRIPT)?
                .then(|| INITIALIZE_SCRIPT.to_owned()),
            after: self.app_path_file(AFTER_STARTUP_SCRIPT)?,
        })
    }

    /// The app-path-only file `name` when it exists on disk (reference
    /// `TVPIsExistentStorageNoSearch`).
    fn app_path_file(&self, name: &str) -> Result<Option<String>, StorageReadError> {
        Ok(self.find_disk(name)?.map(|_| name.to_owned()))
    }

    /// Lists the immediate children of storage directory `dir`, merging disk
    /// entries with archive entries. Names are normalized (`/` separators,
    /// lowercase); directory children carry a trailing `/`. Results are sorted
    /// and deduplicated.
    ///
    /// A missing or non-directory `dir` lists nothing (it does not error);
    /// only a bottom file-system failure propagates.
    pub fn list(&self, dir: &str) -> Result<Vec<String>, StorageReadError> {
        let normalized = normalize_in_archive_name(dir);
        let normalized = normalized.trim_end_matches('/').to_string();
        let mut found = BTreeSet::new();

        let disk_dir = if normalized.is_empty() {
            self.game_dir.clone()
        } else {
            self.game_dir.join(&normalized)
        };
        match fs::list(&self.fs, &FsPath::host(&disk_dir)) {
            Ok(entries) => {
                for entry in entries {
                    found.insert(child_name(&normalized, &entry.name, entry.is_dir));
                }
            }
            Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => {}
            Err(e) => return Err(StorageReadError::Fs(e)),
        }

        let prefix = if normalized.is_empty() {
            String::new()
        } else {
            format!("{normalized}/")
        };
        for mounted in &self.archives {
            for entry in mounted.archive.entries() {
                let Some(rest) = entry.name.strip_prefix(&prefix) else {
                    continue;
                };
                if rest.is_empty() {
                    continue;
                }
                let (head, is_dir) = match rest.split_once('/') {
                    Some((head, _)) => (head, true),
                    None => (rest, false),
                };
                found.insert(if is_dir {
                    format!("{prefix}{head}/")
                } else {
                    format!("{prefix}{head}")
                });
            }
        }

        Ok(found.into_iter().collect())
    }

    /// `Storages.getFileList(mask, attr)`: storage names matching a wildcard
    /// mask, merged across the disk root and every mounted archive.
    ///
    /// * The mask is normalized (lowercased, `\` → `/`).
    /// * If it contains `>` it addresses archives: `arcmask>inarcmask`, where
    ///   `arcmask` wildcard-matches the archive file name.
    /// * The part before the last `/` is a required literal directory prefix;
    ///   the part after is a `*`/`?` wildcard matched case-insensitively
    ///   against the base name. Matching is **recursive under the prefix**
    ///   (matching the reference `Storages.getFileList`): a mask with no
    ///   `>` matches base names at any depth below the prefix, and a mask
    ///   with no `/` matches at any depth in the whole tree.
    /// * Disk entries are emitted as normalized relative names; archive
    ///   entries as `arc.xp3>path`.
    /// * `attr` filters results: `0` or [`ATTR_NORMAL`] → regular files,
    ///   [`ATTR_DIRECTORY`] → directories (with a trailing `/`); bits OR
    ///   together.
    ///
    /// A genuine file-system failure while walking the disk tree propagates;
    /// only a missing or non-directory subtree is treated as empty.
    pub fn file_list(&self, mask: &str, attr: i64) -> Result<Vec<String>, StorageReadError> {
        let mask = normalize_in_archive_name(mask);
        let (arc_pat, in_arc_mask) = match mask.split_once('>') {
            Some((arc, rest)) => (Some(arc.to_string()), rest.to_string()),
            None => (None, mask),
        };
        let (dir_prefix, base_pat) = match in_arc_mask.rsplit_once('/') {
            Some((dir, base)) => (
                format!("{dir}/"),
                if base.is_empty() {
                    "*".to_string()
                } else {
                    base.to_string()
                },
            ),
            None => (String::new(), in_arc_mask.clone()),
        };
        let include_files = attr == 0 || attr & ATTR_NORMAL != 0;
        let include_dirs = attr & ATTR_DIRECTORY != 0;

        let mut found = BTreeSet::new();

        // Disk entries under the game directory, recursively.
        let mut disk = Vec::new();
        self.collect_disk_entries(&self.game_dir, "", &mut disk)?;
        for (name, is_dir) in disk {
            let base = name.rsplit('/').next().unwrap_or(&name);
            let prefix_ok = dir_prefix.is_empty() || name.starts_with(&dir_prefix);
            let attr_ok = (is_dir && include_dirs) || (!is_dir && include_files);
            if prefix_ok && attr_ok && wildcard_match(&base_pat, base) {
                found.insert(if is_dir { format!("{name}/") } else { name });
            }
        }

        // Archive entries, addressed as "arc.xp3>path".
        for mounted in &self.archives {
            if let Some(pat) = &arc_pat
                && !wildcard_match(pat, &mounted.file_name)
            {
                continue;
            }
            for entry in mounted.archive.entries() {
                let base = entry.name.rsplit('/').next().unwrap_or(&entry.name);
                let prefix_ok = dir_prefix.is_empty() || entry.name.starts_with(&dir_prefix);
                if prefix_ok && wildcard_match(&base_pat, base) {
                    found.insert(format!("{}>{}", mounted.file_name, entry.name));
                }
            }
        }

        Ok(found.into_iter().collect())
    }

    /// Recursively collects disk entries as `(normalized relative name,
    /// is_dir)`, using the bottom file system for listing. A missing or
    /// non-directory subtree contributes nothing; any other listing failure
    /// propagates rather than silently truncating the result.
    fn collect_disk_entries(
        &self,
        dir: &Path,
        rel: &str,
        out: &mut Vec<(String, bool)>,
    ) -> Result<(), StorageReadError> {
        let entries = match fs::list(&self.fs, &FsPath::host(dir)) {
            Ok(entries) => entries,
            Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => {
                return Ok(());
            }
            Err(error) => return Err(StorageReadError::Fs(error)),
        };
        for entry in entries {
            let rel = if rel.is_empty() {
                entry.name.clone()
            } else {
                format!("{rel}/{}", entry.name)
            };
            let normalized = normalize_in_archive_name(&rel);
            let child = dir.join(&entry.name);
            if entry.is_dir {
                out.push((normalized, true));
                self.collect_disk_entries(&child, &rel, out)?;
            } else {
                out.push((normalized, false));
            }
        }
        Ok(())
    }
}

/// Recursively discovers every `*.xp3` under `base_dir`, in mount order.
///
/// This is a documented extension to the reference (D15 / doc 09): the
/// reference never scans a folder, but a game directory is the ergonomic
/// input for both the desktop CLI and the Android launcher. The order is
/// deterministic — file name in natural order (embedded decimal runs are
/// compared by value, so `patch@r1071` overrides `patch@r996` and
/// `patch_append10` overrides `patch_append5`), then a shallower directory
/// last, then full path — so `patch.xp3` overrides `data.xp3` and a root
/// archive overrides a nested one of the same name (later mounts win).
/// Symbolic-link directories are
/// not followed (the listing reports the link, not its target), so a link
/// cycle cannot recurse forever.
///
/// # Errors
///
/// A missing or non-directory subtree contributes nothing; any other listing
/// failure is returned rather than silently truncating the scan.
pub fn discover_archives(
    file_system: &FileSystem,
    base_dir: &Path,
) -> Result<Vec<PathBuf>, StorageReadError> {
    let mut found = Vec::new();
    discover_archives_inner(file_system, base_dir, &mut found)?;
    // Natural (numeric-aware) file-name order; among equal names a shallower
    // path sorts last (and therefore mounts last and wins); the full path is
    // the final tie-break.
    found.sort_by(|a, b| {
        natural_cmp(&archive_name(a), &archive_name(b))
            .then_with(|| b.components().count().cmp(&a.components().count()))
            .then_with(|| a.cmp(b))
    });
    Ok(found)
}

fn discover_archives_inner(
    file_system: &FileSystem,
    dir: &Path,
    out: &mut Vec<PathBuf>,
) -> Result<(), StorageReadError> {
    let entries = match fs::list(file_system, &FsPath::host(dir)) {
        Ok(entries) => entries,
        Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => return Ok(()),
        Err(error) => return Err(StorageReadError::Fs(error)),
    };
    for entry in entries {
        let child = dir.join(&entry.name);
        if entry.is_dir {
            discover_archives_inner(file_system, &child, out)?;
        } else if entry.name.to_ascii_lowercase().ends_with(".xp3") {
            out.push(child);
        }
    }
    Ok(())
}

/// The lowercased file name used as the primary archive-discovery sort key.
fn archive_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

/// Consume a leading run of ASCII digits, returning
/// `(value, digit_count, remainder)`.
fn consume_number(bytes: &[u8]) -> (u128, usize, &[u8]) {
    let mut value: u128 = 0;
    let mut digits = 0usize;
    let mut rest = bytes;
    while let Some((&c, tail)) = rest.split_first() {
        if c.is_ascii_digit() {
            value = value
                .saturating_mul(10)
                .saturating_add(u128::from(c - b'0'));
            digits += 1;
            rest = tail;
        } else {
            break;
        }
    }
    (value, digits, rest)
}

/// Compare two lowercased names with embedded decimal runs compared by
/// numeric value, so `patch@r1071` sorts after `patch@r996` and
/// `patch_append10` sorts after `patch_append5`. Non-digit bytes compare
/// byte-wise; equal numeric values order by digit count (shorter first).
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ai, mut bi) = (a.as_bytes(), b.as_bytes());
    loop {
        match (ai.first(), bi.first()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(&ca), Some(&cb)) => {
                if ca.is_ascii_digit() && cb.is_ascii_digit() {
                    let (value_a, digits_a, rest_a) = consume_number(ai);
                    let (value_b, digits_b, rest_b) = consume_number(bi);
                    match value_a.cmp(&value_b) {
                        std::cmp::Ordering::Equal => {
                            if digits_a != digits_b {
                                return digits_a.cmp(&digits_b);
                            }
                        }
                        other => return other,
                    }
                    ai = rest_a;
                    bi = rest_b;
                } else {
                    match ca.cmp(&cb) {
                        std::cmp::Ordering::Equal => {
                            ai = &ai[1..];
                            bi = &bi[1..];
                        }
                        other => return other,
                    }
                }
            }
        }
    }
}

/// The base storage name (the part after the last `/`).
fn storage_base(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Builds a listed child name from its parent prefix.
fn child_name(parent: &str, name: &str, is_dir: bool) -> String {
    let normalized = normalize_in_archive_name(name);
    let full = if parent.is_empty() {
        normalized
    } else {
        format!("{parent}/{normalized}")
    };
    if is_dir { format!("{full}/") } else { full }
}

/// Converts epoch milliseconds to a [`SystemTime`].
fn system_time_from_millis(ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(ms)
}

/// Reads a whole file through the bottom positional API.
fn read_fs_file(fs: &FileSystem, path: &Path) -> Result<Vec<u8>, FsError> {
    let file = fs::open(fs, &FsPath::host(path), OpenMode::Read)?;
    let size = fs::file_size(fs, &file)?;
    let size = usize::try_from(size).map_err(|_| FsError::Io {
        message: "file size does not fit in usize".to_string(),
    })?;
    let mut out = Vec::new();
    out.try_reserve_exact(size).map_err(|_| FsError::Io {
        message: "file size cannot be allocated".to_string(),
    })?;
    out.resize(size, 0);
    let mut read = 0usize;
    while read < size {
        let n = fs::read_at(fs, &file, read as u64, &mut out[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    out.truncate(read);
    Ok(out)
}

/// Case-insensitive `*`/`?` wildcard match (ASCII case folding).
fn wildcard_match(pattern: &str, text: &str) -> bool {
    let pat: Vec<char> = pattern.chars().collect();
    let text: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut retry) = (usize::MAX, 0usize);
    while ti < text.len() {
        if pi < pat.len() && (pat[pi] == '?' || pat[pi].eq_ignore_ascii_case(&text[ti])) {
            pi += 1;
            ti += 1;
        } else if pi < pat.len() && pat[pi] == '*' {
            star = pi;
            retry = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            retry += 1;
            ti = retry;
        } else {
            return false;
        }
    }
    while pi < pat.len() && pat[pi] == '*' {
        pi += 1;
    }
    pi == pat.len()
}
