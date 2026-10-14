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
//! A mount can optionally read each archive into memory in full
//! ([`MountOptions::preload_archives`]); the image is owned by this layer and
//! a borrow of it is handed to L1 for every entry read.
//!
//! Resolution order mirrors the reference engine:
//!
//! 1. explicit `arc.xp3>path` addressing,
//! 2. disk files (normalized spelling, then the script's spelling, then a
//!    bounded component-wise case-insensitive walk),
//! 3. mounted archives, later mounts first (later-mounted wins),
//! 4. the auto search-path list.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use api::error::FsError;
use api::fs::{FsPath, OpenMode};
use fs::xp3::{self, Xp3Archive, Xp3ExtractionFilter, normalize_in_archive_name};
use fs::{FileState, FileSystem};

/// Windows `FILE_ATTRIBUTE_DIRECTORY` (0x10).
pub const ATTR_DIRECTORY: i64 = 0x10;
/// Windows `FILE_ATTRIBUTE_ARCHIVE` (0x20) — the "normal file" bit.
pub const ATTR_NORMAL: i64 = 0x20;

/// One mounted `.xp3` archive: its logical path, the backing that owns its
/// bytes and the parsed index.
struct MountedArchive {
    /// Logical path of the archive file (as mounted).
    path: PathBuf,
    /// Base file name (used for `arc.xp3>path` matching and listings).
    file_name: String,
    /// Where the archive's bytes are read from.
    backing: ArchiveBacking,
    /// Parsed index.
    archive: Xp3Archive,
}

/// Where a mounted archive's bytes are read from.
///
/// The bytes are owned by the storage; the stateless L1 reader is handed the
/// backing on every entry read and never holds it.
enum ArchiveBacking {
    /// The archive file stays open and each entry read is positional.
    Stream(FileState),
    /// The whole archive file was read into memory at mount; entry reads copy
    /// from this image.
    Memory(Vec<u8>),
}

/// Options controlling how [`Storage`] mounts archives.
///
/// The default keeps every archive on the file system and reads each entry
/// positionally. Enabling [`preload_archives`](Self::preload_archives) reads
/// each archive into memory once at mount.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MountOptions {
    /// Read each mounted archive into memory in full at mount.
    ///
    /// The resident-memory cost is the archive's own size; there is no cap.
    /// Every entry then reads from that buffer instead of the file system,
    /// which removes the per-entry random reads a slow backing store makes
    /// visible. A short read, an unrepresentable size or a failed allocation
    /// is a typed [`MountError`], never a silent fall back to the streaming
    /// path.
    pub preload_archives: bool,
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
    /// Whether an archive is read into memory in full when it is mounted.
    preload_archives: bool,
    /// The project storage path (the reference's current directory), supplied
    /// by the composition root via [`Self::set_project`]. `Some` when the
    /// project is an archive; a relative auto path is then normalized against
    /// it. `None` for a folder project (disk-relative auto paths).
    project: Option<PathBuf>,
    /// The per-segment XP3 extraction filter (the `xp3filter` seam), if any.
    ///
    /// L2 owns the state; the stateless L1 reader receives the closure as a
    /// borrow on every read, and it is never stored in L1. `None` means no
    /// filter. The closure is invoked once per decoded segment with
    /// `(file_hash, offset, buffer, file_name, archive_name, file_size)` and
    /// may rewrite `buffer` in place.
    pub xp3_extraction_filter: Option<Box<Xp3ExtractionFilter<'static>>>,
    /// Virtual media entries registered by L3 storage-media providers, keyed
    /// by the full storage name (e.g. `psb://quickmenu.pimg/11632.tlg`).
    /// [`Self::read`]/[`Self::read_prefix`]/[`Self::exists`] serve these
    /// before disk and archive resolution; L2 only stores the bytes.
    ///
    /// Each entry counts its live registrations so a name shared by several
    /// providers is freed only when the last one releases it, matching the
    /// reference `PSBMedia` shared-resource lifetime.
    media: RefCell<HashMap<String, MediaEntry>>,
    /// Lazily built resolver tables, invalidated when the auto-path list or
    /// the mounted archives change.
    resolution_cache: RefCell<ResolutionCache>,
}

/// One virtual media entry: the bytes plus the number of live registrations.
#[derive(Clone, Debug)]
struct MediaEntry {
    /// The bytes served for the name.
    bytes: Vec<u8>,
    /// How many providers currently hold the name registered.
    owners: usize,
}

/// A resolved storage location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A file on the bottom file system.
    Disk(PathBuf),
    /// A file inside an archive: `(archive path, normalized in-archive name)`.
    Archive(PathBuf, String),
}

/// Counts of the bottom file-system probes a [`Storage`] has issued.
///
/// A resolver table serves repeat lookups without a probe, so the counts show
/// whether a lookup came from the cache: a cached miss adds no `list` probe.
/// They are a diagnostic, not part of resolution.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProbeCounts {
    /// Directory-enumeration calls (`fs::list`).
    pub list: u64,
    /// Single-path stat calls (`fs::stat`).
    pub stat: u64,
    /// Lookups that resolved to nothing (the reference's empty placed path).
    pub misses: u64,
}

/// The direct children of an archive auto-path prefix: the archive path and a
/// map from lower-cased base name to normalized in-archive name.
type ArchiveChildren = (PathBuf, HashMap<String, String>);

/// A cached auto-path candidate table, keyed by the auto-path entry.
enum AutoPathTable {
    /// A plain directory auto path.
    Folder {
        /// Absolute directory that was listed.
        root: PathBuf,
        /// Real child name -> `is_dir`, for the exact lower-cased probe.
        exact: HashMap<String, bool>,
        /// Lower-cased child name -> the first **file** child with that name.
        first_file: HashMap<String, String>,
    },
    /// An archive auto path with an in-archive prefix.
    Archive {
        /// Absolute path of the archive.
        archive: PathBuf,
        /// Lower-cased direct-child base name -> normalized in-archive name.
        entries: HashMap<String, String>,
    },
}

/// One auto-path table hit.
enum AutoPathHit {
    /// The base resolved to a file location.
    File(Location),
    /// The base resolved to a directory, so a file lookup finds nothing here.
    Directory,
}

impl AutoPathTable {
    /// Looks up `base` (already lower-cased) in this table.
    fn lookup(&self, base: &str) -> Option<AutoPathHit> {
        match self {
            AutoPathTable::Folder {
                root,
                exact,
                first_file,
            } => {
                // The lower-cased spelling wins, matching the normalized
                // lookup; a directory at that spelling shadows a differently
                // cased file in the same auto-path directory.
                if let Some(is_dir) = exact.get(base) {
                    return Some(if *is_dir {
                        AutoPathHit::Directory
                    } else {
                        AutoPathHit::File(Location::Disk(root.join(base)))
                    });
                }
                first_file
                    .get(base)
                    .map(|real| AutoPathHit::File(Location::Disk(root.join(real.as_str()))))
            }
            AutoPathTable::Archive { archive, entries } => entries
                .get(base)
                .map(|full| AutoPathHit::File(Location::Archive(archive.clone(), full.clone()))),
        }
    }
}

/// The per-storage resolver cache.
#[derive(Default)]
struct ResolutionCache {
    /// Candidate tables keyed by the auto-path entry string.
    auto_paths: HashMap<String, Rc<AutoPathTable>>,
    /// Resolved hits keyed by the requested storage name (the reference's
    /// `TVPAutoPathCache`: a hit is cached, a miss is not).
    placed: HashMap<String, Location>,
    /// Bottom file-system probes issued while resolving.
    probes: ProbeCounts,
    /// Entry bytes decoded and returned by `read`/`read_prefix`.
    read_bytes: u64,
}

/// The number of leading characters kept in the bounded `debug` storage-miss
/// line; the rest is replaced by an elision marker naming the dropped count.
const MISS_LOG_HEAD_CHARS: usize = 200;

/// The locations a failed storage lookup searched.
///
/// A lookup only records when debug logging is enabled, so a hit on the normal
/// path builds no trace and allocates nothing; the closure passed to
/// [`Self::attempt`] runs only when recording.
///
/// The per-entry detail an archive miss can add (the near-miss name scan)
/// walks the whole index, so it is recorded only when the full `trace` line is
/// on. The bounded `debug` line omits that detail, so building it there would
/// be wasted work on every miss.
#[derive(Default)]
struct SearchTrace {
    /// Whether recording is on (debug logging enabled).
    enabled: bool,
    /// Whether the full `trace` line is on, so an expensive per-entry detail
    /// is worth building.
    detailed: bool,
    /// The searched locations and rules, in search order.
    attempts: Vec<String>,
}

impl SearchTrace {
    /// Creates a trace that records only when `enabled`; `detailed` selects
    /// whether an attempt may build the expensive per-entry text.
    fn new(enabled: bool, detailed: bool) -> Self {
        Self {
            enabled,
            detailed,
            attempts: Vec::new(),
        }
    }

    /// Records one searched location or rule, formatting it only when
    /// recording.
    fn attempt(&mut self, text: impl FnOnce() -> String) {
        if self.enabled {
            self.attempts.push(text());
        }
    }

    /// Records one searched location, telling the closure whether the full
    /// trace is on so it can skip an expensive detail the `debug` line drops.
    fn attempt_detailed(&mut self, text: impl FnOnce(bool) -> String) {
        if self.enabled {
            self.attempts.push(text(self.detailed));
        }
    }

    /// Renders the recorded attempts for one line.
    fn render(&self) -> String {
        if self.attempts.is_empty() {
            "(nothing searched)".to_string()
        } else {
            self.attempts.join("; ")
        }
    }

    /// Renders at most `head_chars` leading characters followed by an elision
    /// marker naming how many characters were dropped.
    ///
    /// The budget is applied per character, never per byte, so a path with
    /// non-ASCII characters is cut only at a `char` boundary. [`Self::render`]
    /// keeps returning the full text.
    fn render_short(&self, head_chars: usize) -> String {
        let full = self.render();
        let total = full.chars().count();
        if total <= head_chars {
            return full;
        }
        let head: String = full.chars().take(head_chars).collect();
        format!("{head}… (+{} chars)", total - head_chars)
    }
}

/// The auto-path candidates tried for one miss, grouped by resolution shape.
///
/// A folder entry's candidate is the entry (minus a trailing slash) plus the
/// base name; an archive entry (`arc.xp3>dir`) appends the base to the entry.
/// The first entry of each shape prints its full candidate; the rest print only
/// the entry, because the shape's rule is already shown once. That names every
/// entry and every candidate while keeping a 41-entry list from repeating the
/// same boilerplate on each line.
struct AutoPathTrace {
    /// The storage-relative base name every candidate appends.
    base: String,
    /// Folder candidates (a `/` is inserted before the base).
    folders: ShapeEntries,
    /// Archive candidates (`arc.xp3>dir` plus the base).
    archives: ShapeEntries,
}

impl AutoPathTrace {
    /// Creates a trace for the given base name.
    fn new(base: &str) -> Self {
        Self {
            base: base.to_string(),
            folders: ShapeEntries::default(),
            archives: ShapeEntries::default(),
        }
    }

    /// Records one folder auto-path entry and its candidate.
    fn record_folder(&mut self, entry: &str, candidate: String) {
        self.folders.record(entry, candidate);
    }

    /// Records one archive auto-path entry and its candidate.
    fn record_archive(&mut self, entry: &str, candidate: String) {
        self.archives.record(entry, candidate);
    }

    /// Whether any auto-path entry was recorded.
    fn is_empty(&self) -> bool {
        self.folders.is_empty() && self.archives.is_empty()
    }

    /// Renders the grouped list: the base rule once, then per shape the full
    /// first candidate and the remaining entries.
    fn render(&self) -> String {
        let mut out = String::from("auto paths (later added first; candidates append \"");
        out.push_str(&self.base);
        out.push_str("\"): ");
        let sections: Vec<String> = [&self.folders, &self.archives]
            .into_iter()
            .filter_map(ShapeEntries::render)
            .collect();
        out.push_str(&sections.join("; "));
        out
    }
}

/// The entries of one resolution shape and the first entry's full candidate.
#[derive(Default)]
struct ShapeEntries {
    /// The full candidate of the first entry of the shape.
    example: Option<String>,
    /// The entries after the first, in search order.
    rest: Vec<String>,
}

impl ShapeEntries {
    /// Records one entry of the shape; the first also keeps its candidate.
    fn record(&mut self, entry: &str, candidate: String) {
        if self.example.is_none() {
            self.example = Some(candidate);
        } else {
            self.rest.push(entry.to_string());
        }
    }

    /// Whether this shape recorded no entries.
    fn is_empty(&self) -> bool {
        self.example.is_none()
    }

    /// Renders the shape, or `None` when it has no entries.
    fn render(&self) -> Option<String> {
        let example = self.example.as_ref()?;
        if self.rest.is_empty() {
            Some(example.clone())
        } else {
            Some(format!("{example} (also {})", self.rest.join(", ")))
        }
    }
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
    /// A preloaded archive was shorter than its reported size.
    #[error("archive {path} is truncated: read {read} of {expected} bytes")]
    TruncatedArchive {
        /// Archive path as mounted.
        path: String,
        /// Size the archive reported.
        expected: u64,
        /// Bytes actually read before the file ended.
        read: u64,
    },
    /// A preloaded archive is larger than this platform can address in a
    /// single buffer.
    #[error("archive {path} is too large to hold in memory: {size} bytes")]
    ArchiveTooLarge {
        /// Archive path as mounted.
        path: String,
        /// Reported size in bytes.
        size: u64,
    },
    /// A preloaded archive's buffer could not be allocated.
    #[error("archive {path} could not be read into memory: {size} bytes")]
    ArchiveAllocation {
        /// Archive path as mounted.
        path: String,
        /// Reported size in bytes.
        size: u64,
    },
}

/// Errors while reading or listing storage entries.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum StorageReadError {
    /// The requested storage name did not resolve.
    #[error("storage not found: {0}")]
    NotFound(String),
    /// The storage name is malformed: a `..` component escapes the mount (or
    /// crosses the archive delimiter) and the reference raises
    /// `TVPInvalidPathName` (`reference/cpp/core/base/StorageIntf.cpp:400`).
    #[error("Invalid path name {0}")]
    InvalidPathName(String),
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
        Self::mount_archives_with(fs, base_dir, archives, MountOptions::default())
    }

    /// Creates a storage rooted at `base_dir` and mounts each archive in
    /// `archives`, in order, with an explicit choice of how the archive bytes
    /// are held (see [`MountOptions`]).
    ///
    /// This is [`Self::mount_archives`] with the backing selected; the mount
    /// order and the later-wins precedence are unchanged.
    ///
    /// # Errors
    ///
    /// As [`Self::mount_archives`], plus a typed error when a preloaded
    /// archive cannot be read into memory in full.
    pub fn mount_archives_with(
        fs: FileSystem,
        base_dir: impl Into<PathBuf>,
        archives: impl IntoIterator<Item = impl AsRef<Path>>,
        options: MountOptions,
    ) -> Result<Self, MountError> {
        let mut storage = Self::with_base_dir(fs, base_dir.into(), options.preload_archives)?;
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
        Self::mount_discovered_archives_with(fs, base_dir, archives, MountOptions::default())
    }

    /// Mounts a directory-scan archive list with an explicit backing, skipping
    /// an archive that cannot be read or parsed and reporting it through
    /// `warn!`.
    ///
    /// This is [`Self::mount_discovered_archives`] with [`MountOptions`]; the
    /// skip-and-warn rule and the mount order are unchanged. A preload failure
    /// on a scanned archive is one more archive that is skipped and reported,
    /// exactly like a parse failure — preloading never changes which archive
    /// wins.
    pub fn mount_discovered_archives_with(
        fs: FileSystem,
        base_dir: impl Into<PathBuf>,
        archives: impl IntoIterator<Item = impl AsRef<Path>>,
        options: MountOptions,
    ) -> Result<Self, MountError> {
        let mut storage = Self::with_base_dir(fs, base_dir.into(), options.preload_archives)?;
        for path in archives {
            if let Err(error) = storage.mount_archive(path.as_ref()) {
                log::warn!("skipping archive {}: {error}", path.as_ref().display());
            }
        }
        Ok(storage)
    }

    /// Sets the project path a relative auto path is resolved against.
    ///
    /// The reference's launcher supplies `TVPProjectDir` and the engine only
    /// decides whether that path is a storage (an archive, so the current
    /// directory gets the archive delimiter) or a folder
    /// (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`). An archive
    /// project prefixes a relative auto path (`data.xp3>system/`); a folder
    /// project leaves it disk-relative.
    ///
    /// Call after the archives are mounted, so the project archive is known,
    /// and before any startup script runs.
    pub fn set_project(&mut self, path: impl AsRef<Path>) {
        let path = path.as_ref();
        let is_archive = self.archives.iter().any(|m| m.path.as_path() == path)
            || matches!(
                fs::stat(&self.fs, &FsPath::host(path)),
                Ok(meta) if !meta.is_dir
            );
        self.project = is_archive.then(|| path.to_path_buf());
        self.clear_resolution_cache();
    }

    /// Creates an empty storage rooted at `base_dir`, validating that the
    /// base path exists and is a directory. No archives are mounted; use
    /// [`Self::mount_archive`] to add them explicitly.
    fn with_base_dir(
        fs: FileSystem,
        base_dir: PathBuf,
        preload_archives: bool,
    ) -> Result<Self, MountError> {
        let meta = fs::stat(&fs, &FsPath::host(&base_dir))?;
        if !meta.is_dir {
            return Err(MountError::NotADirectory(base_dir.display().to_string()));
        }
        Ok(Storage {
            fs,
            game_dir: base_dir,
            archives: Vec::new(),
            auto_paths: Vec::new(),
            preload_archives,
            project: None,
            xp3_extraction_filter: None,
            media: RefCell::new(HashMap::new()),
            resolution_cache: RefCell::new(ResolutionCache::default()),
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
        let backing = if self.preload_archives {
            ArchiveBacking::Memory(read_archive_image(&self.fs, &file, &path)?)
        } else {
            ArchiveBacking::Stream(file)
        };
        let file_name = path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        log::info!("opened {} ({} entries)", path.display(), archive.len());
        self.archives.push(MountedArchive {
            path,
            file_name,
            backing,
            archive,
        });
        self.clear_resolution_cache();
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
            self.clear_resolution_cache();
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
        self.clear_resolution_cache();
        true
    }

    /// Appends an auto search path (the `Storages.addAutoPath` semantics).
    /// Duplicates are ignored. A relative path is resolved against the current
    /// storage path first (the project archive when the project is
    /// archive-backed), mirroring `TVPAddAutoPath`'s normalization against
    /// `CurrentPath` (`reference/cpp/core/base/StorageIntf.cpp:921-937`).
    pub fn add_auto_path(&mut self, path: impl Into<String>) {
        let path = self.current_storage_path(path.into());
        if !self.auto_paths.contains(&path) {
            self.auto_paths.push(path);
        }
        // The reference clears its auto-path cache whenever the list changes
        // (`TVPClearAutoPathCache`), which also covers the added path itself.
        self.clear_resolution_cache();
    }

    /// Removes an auto search path.
    pub fn remove_auto_path(&mut self, path: &str) {
        let path = self.current_storage_path(path.to_string());
        self.auto_paths.retain(|p| p != &path);
        self.clear_resolution_cache();
    }

    /// Resolves an auto path against the current storage path.
    ///
    /// `TVPAddAutoPath` normalizes a relative path against the current
    /// directory (`TVPNormalizeStorageName` prepends `CurrentPath`,
    /// `reference/cpp/core/base/StorageIntf.cpp:347-425`), and the current
    /// directory is the project — the archive the reference appends `>` to
    /// (`reference/cpp/core/base/impl/SysInitImpl.cpp:160-165`). An absolute
    /// path, or one that already names an archive, is used as given.
    fn current_storage_path(&self, path: String) -> String {
        if path.contains('>') || Path::new(&path).is_absolute() {
            return path;
        }
        match &self.project {
            Some(project) => {
                let project = project.to_string_lossy().replace('\\', "/");
                format!("{project}>{path}")
            }
            None => path,
        }
    }

    /// The bottom file-system probes issued so far and the miss count.
    pub fn probe_counts(&self) -> ProbeCounts {
        self.resolution_cache.borrow().probes
    }

    /// Entry bytes decoded and returned by [`Self::read`] and
    /// [`Self::read_prefix`] since construction.
    ///
    /// This is the evidence for a lazy read: a caller that only needs a header
    /// sees only the header's bytes here, independent of the resolver cache.
    pub fn read_bytes(&self) -> u64 {
        self.resolution_cache.borrow().read_bytes
    }

    /// Drops the cached auto-path tables.
    ///
    /// Call when the auto-path list or the mounted archives change; the probe
    /// counters are kept so the diagnostic survives an invalidation.
    pub fn clear_resolution_cache(&mut self) {
        let cache = self.resolution_cache.get_mut();
        cache.auto_paths.clear();
        cache.placed.clear();
    }

    /// Resolves a storage name to a location, mirroring the reference search
    /// order: explicit `arc.xp3>path` → the current path (the app directory)
    /// → the auto search paths in "later added wins" order → the mounted
    /// archives in later-mount order. Auto-path and explicit archives are
    /// opened on demand when they exist on disk but were not mounted.
    ///
    /// A name that simply does not resolve is `Ok(None)`; a `..` that escapes
    /// the mount is [`StorageReadError::InvalidPathName`]. A genuine
    /// file-system failure while probing (permission denied, I/O, an
    /// unrepresentable path) is propagated as [`StorageReadError::Fs`]; neither
    /// is collapsed into "not found".
    ///
    /// A miss logs one bounded `debug` line and, when trace logging is on, the
    /// full location list at `trace`. The `debug` line names the searched
    /// locations (disk spellings, auto-path candidates grouped by resolution
    /// shape, mounted archives in later-first order, or the explicit
    /// `arc.xp3>path` interpretation) until a small character budget is
    /// reached, then appends an elision marker with the number of dropped
    /// characters; the `trace` line carries the complete list, including the
    /// archive near-miss names. A hit logs nothing, and the trace is only built
    /// when debug logging is enabled; the near-miss scan is added only at
    /// `trace`.
    ///
    /// Storage names are case-insensitive even when the host filesystem is
    /// not. Only the storage-relative part is normalized; the mount prefix
    /// keeps the real filesystem spelling. A `..` component that escapes the
    /// mount is [`StorageReadError::InvalidPathName`] (`TVPInvalidPathName`),
    /// not a miss.
    pub fn find(&self, name: &str) -> Result<Option<Location>, StorageReadError> {
        // The reference caches a resolved hit and never a miss
        // (`TVPAutoPathCache`; a miss is not cached,
        // `reference/cpp/core/base/StorageIntf.cpp:1101`).
        if let Some(hit) = self.resolution_cache.borrow().placed.get(name).cloned() {
            return Ok(Some(hit));
        }
        let mut trace = SearchTrace::new(
            log::log_enabled!(log::Level::Debug),
            log::log_enabled!(log::Level::Trace),
        );
        let found = self.find_recorded(name, &mut trace)?;
        match &found {
            Some(location) => {
                self.resolution_cache
                    .borrow_mut()
                    .placed
                    .insert(name.to_string(), location.clone());
            }
            None => {
                self.resolution_cache.borrow_mut().probes.misses += 1;
                if trace.enabled {
                    log::debug!(
                        "storage miss {name:?}: {}",
                        trace.render_short(MISS_LOG_HEAD_CHARS)
                    );
                    if log::log_enabled!(log::Level::Trace) {
                        log::trace!("storage miss {name:?}: {}", trace.render());
                    }
                }
            }
        }
        Ok(found)
    }

    /// [`Self::find`] with every searched location recorded into `trace`.
    fn find_recorded(
        &self,
        name: &str,
        trace: &mut SearchTrace,
    ) -> Result<Option<Location>, StorageReadError> {
        // Explicit archive addressing: "foo.xp3>data/script.ks". The archive is
        // opened on demand when it exists but was not mounted.
        if let Some((arc, rest)) = name.split_once('>') {
            trace.attempt(|| format!("explicit archive {arc:?}>{rest:?}"));
            return self.find_in_archive(arc, rest, trace);
        }

        // Resolve the storage-relative part. Absolute names must point inside
        // the mount; a `..` that escapes it is the reference's invalid name.
        let Some(relative) = self.relative_disk_name_checked(name)? else {
            trace.attempt(|| format!("name {name:?} is outside {}", self.game_dir.display()));
            return Ok(None);
        };

        // The current path (the app directory) is checked first.
        if let Some(location) = self.find_disk_relative(&relative, trace)? {
            return Ok(Some(location));
        }

        let normalized = normalize_in_archive_name(&relative);
        let base = storage_base(&normalized);

        // Auto paths, last added first (the reference's "later added wins").
        // Each auto path's candidate table is built once and cached, so the
        // lookup is an in-memory hit rather than a directory probe. Archive
        // prefixes are opened on demand when the table is built. The trace
        // groups the entries by resolution shape, so a 41-entry list names each
        // entry once instead of repeating the candidate boilerplate per line.
        let mut auto_trace = if trace.enabled {
            Some(AutoPathTrace::new(base))
        } else {
            None
        };
        for entry in self.auto_paths.iter().rev() {
            if let Some((arc, dir)) = entry.split_once('>') {
                if let Some(auto_trace) = auto_trace.as_mut() {
                    auto_trace.record_archive(entry, format!("{arc}>{dir}{base}"));
                }
            } else {
                let dir = entry.trim_end_matches('/');
                if dir.is_empty() {
                    continue;
                }
                if let Some(auto_trace) = auto_trace.as_mut() {
                    auto_trace.record_folder(entry, format!("{dir}/{base}"));
                }
            }
            match self.auto_path_table(entry)?.lookup(base) {
                Some(AutoPathHit::File(location)) => return Ok(Some(location)),
                Some(AutoPathHit::Directory) => continue,
                None => {}
            }
        }
        if let Some(auto_trace) = auto_trace.filter(|trace| !trace.is_empty()) {
            trace.attempt(|| auto_trace.render());
        }

        // Mounted archives, later mounts first (later-mounted wins — see
        // `mount_archives`). Looked up by the normalized name.
        if !self.archives.is_empty() {
            trace.attempt(|| {
                let order: Vec<String> = self
                    .archives
                    .iter()
                    .rev()
                    .map(|m| m.path.display().to_string())
                    .collect();
                format!("mounted archives, later first: [{}]", order.join(", "))
            });
        }
        for mounted in self.archives.iter().rev() {
            if mounted.archive.entry(&normalized).is_some() {
                return Ok(Some(Location::Archive(mounted.path.clone(), normalized)));
            }
            trace.attempt_detailed(|detailed| {
                archive_miss_line(&mounted.path, &mounted.archive, &normalized, detailed)
            });
        }
        Ok(None)
    }

    /// The live listing of `dir`, enumerating it on every call.
    ///
    /// The reference's case-insensitive lookup and `GetLocallyAccessibleName`
    /// both walk the real directory with a live `opendir`/`readdir` on every
    /// call (`reference/cpp/core/base/impl/StorageImpl.cpp:339-407`), so the
    /// listing is never cached: a script that creates a directory and then
    /// resolves it case-insensitively must see it. The auto-path candidate
    /// table is cached separately, so a repeat auto-path lookup still lists
    /// nothing.
    fn directory_listing(&self, dir: &Path) -> Result<Vec<(String, bool)>, StorageReadError> {
        self.resolution_cache.borrow_mut().probes.list += 1;
        let entries = match fs::list(&self.fs, &FsPath::host(dir)) {
            Ok(entries) => entries,
            Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => Vec::new(),
            Err(error) => return Err(StorageReadError::Fs(error)),
        };
        Ok(entries
            .into_iter()
            .map(|entry| (entry.name, entry.is_dir))
            .collect())
    }

    /// Counts and runs one `stat` probe, returning whether the path is a
    /// directory (`None` when it does not exist).
    fn stat_probe(&self, path: &Path) -> Result<Option<bool>, StorageReadError> {
        self.resolution_cache.borrow_mut().probes.stat += 1;
        match fs::stat(&self.fs, &FsPath::host(path)) {
            Ok(meta) => Ok(Some(meta.is_dir)),
            Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => Ok(None),
            Err(error) => Err(StorageReadError::Fs(error)),
        }
    }

    /// The cached candidate table for one auto-path entry, built on first use.
    fn auto_path_table(&self, entry: &str) -> Result<Rc<AutoPathTable>, StorageReadError> {
        let cached = self
            .resolution_cache
            .borrow()
            .auto_paths
            .get(entry)
            .cloned();
        if let Some(table) = cached {
            return Ok(table);
        }
        let table = Rc::new(self.build_auto_path_table(entry)?);
        self.resolution_cache
            .borrow_mut()
            .auto_paths
            .insert(entry.to_string(), Rc::clone(&table));
        Ok(table)
    }

    /// Builds the candidate table for one auto-path entry.
    fn build_auto_path_table(&self, entry: &str) -> Result<AutoPathTable, StorageReadError> {
        if let Some((arc, dir)) = entry.split_once('>') {
            self.build_archive_auto_path(arc, dir)
        } else {
            self.build_folder_auto_path(entry.trim_end_matches('/'))
        }
    }

    /// Builds the plain-directory candidate table from one live listing.
    fn build_folder_auto_path(&self, dir: &str) -> Result<AutoPathTable, StorageReadError> {
        let root = if Path::new(dir).is_absolute() {
            PathBuf::from(dir)
        } else {
            self.game_dir.join(dir)
        };
        let listing = self.directory_listing(&root)?;
        let mut exact = HashMap::new();
        let mut first_file = HashMap::new();
        for (name, is_dir) in listing.iter() {
            let key = name.to_ascii_lowercase();
            // The lower-cased spelling wins when both a lower-cased and a
            // mixed-case entry exist.
            if name.as_str() == key.as_str() {
                exact.insert(key.clone(), *is_dir);
            }
            if !is_dir {
                first_file.entry(key).or_insert_with(|| name.clone());
            }
        }
        Ok(AutoPathTable::Folder {
            root,
            exact,
            first_file,
        })
    }

    /// Builds the archive candidate table: the direct children of the
    /// in-archive prefix.
    fn build_archive_auto_path(
        &self,
        arc: &str,
        dir: &str,
    ) -> Result<AutoPathTable, StorageReadError> {
        let prefix = normalize_in_archive_name(dir);
        let Some((archive, entries)) = self.archive_auto_path_entries(arc, &prefix)? else {
            // A missing archive contributes no candidates, so the lookup stays
            // a miss, exactly like the on-demand open returning `NotFound`.
            return Ok(AutoPathTable::Archive {
                archive: PathBuf::new(),
                entries: HashMap::new(),
            });
        };
        Ok(AutoPathTable::Archive { archive, entries })
    }

    /// Collects the direct children of `prefix` from the archive named `arc`,
    /// using the mounted index when present and opening the archive on demand
    /// otherwise. `None` means the archive does not exist.
    fn archive_auto_path_entries(
        &self,
        arc: &str,
        prefix: &str,
    ) -> Result<Option<ArchiveChildren>, StorageReadError> {
        let arc_name = Path::new(arc)
            .file_name()
            .map(|f| f.to_string_lossy())
            .unwrap_or_default();
        for mounted in self.archives.iter().rev() {
            if mounted.file_name.eq_ignore_ascii_case(&arc_name) {
                let mut entries = HashMap::new();
                collect_archive_children(mounted.archive.entries(), prefix, &mut entries);
                return Ok(Some((mounted.path.clone(), entries)));
            }
        }
        let Some(path) = self.archive_disk_path(arc)? else {
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
        let mut entries = HashMap::new();
        collect_archive_children(archive.entries(), prefix, &mut entries);
        Ok(Some((path, entries)))
    }

    /// Resolves `rest` inside the archive named by `arc`, opening the archive
    /// on demand when it exists on disk but is not in the mount list. A
    /// specific archive miss does not fall through to another archive.
    fn find_in_archive(
        &self,
        arc: &str,
        rest: &str,
        trace: &mut SearchTrace,
    ) -> Result<Option<Location>, StorageReadError> {
        let Some(rest) = normalize_in_archive_path(rest) else {
            return Err(StorageReadError::InvalidPathName(rest.to_string()));
        };
        let arc_name = Path::new(arc)
            .file_name()
            .map(|f| f.to_string_lossy())
            .unwrap_or_default();
        for mounted in self.archives.iter().rev() {
            if mounted.file_name.eq_ignore_ascii_case(&arc_name) {
                if mounted.archive.entry(&rest).is_some() {
                    return Ok(Some(Location::Archive(mounted.path.clone(), rest)));
                }
                trace.attempt_detailed(|detailed| {
                    archive_miss_line(&mounted.path, &mounted.archive, &rest, detailed)
                });
                return Ok(None);
            }
        }
        // On demand: open and parse the archive when the file exists.
        let Some(path) = self.archive_disk_path(arc)? else {
            trace.attempt(|| format!("archive {arc:?} is not mounted and not on disk"));
            return Ok(None);
        };
        trace.attempt(|| {
            format!(
                "archive {} (opened on demand) entry {rest:?}",
                path.display()
            )
        });
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
            trace.attempt_detailed(|detailed| archive_miss_line(&path, &archive, &rest, detailed));
            Ok(None)
        }
    }

    /// The on-disk path of the archive named by `arc`: an absolute path is
    /// used as given, a relative one is resolved case-insensitively under the
    /// game directory.
    fn archive_disk_path(&self, arc: &str) -> Result<Option<PathBuf>, StorageReadError> {
        let mut trace = SearchTrace::default();
        if let Some(relative) = self.relative_disk_name_checked(arc)?
            && let Ok(Some(Location::Disk(path))) = self.find_disk_relative(&relative, &mut trace)
        {
            return Ok(Some(path));
        }
        let raw = Path::new(arc);
        Ok(if raw.is_absolute() {
            Some(raw.to_path_buf())
        } else {
            None
        })
    }

    /// The storage-relative part of `name` (using `/` separators), or `None`
    /// when the name is absolute and does not point inside the mount. `.`
    /// components are dropped and `..` removes the preceding component,
    /// matching the reference storage name normalizer. A `..` that escapes
    /// the mount is mapped to `None` by this lenient accessor;
    /// [`Self::relative_disk_name_checked`] raises instead.
    pub fn relative_disk_name(&self, name: &str) -> Option<String> {
        fold_dot_components(&self.raw_relative_disk_name(name)?)
    }

    /// The mount-relative part of `name` with `.`/`..` still present: `None`
    /// when the name is absolute and outside the mount.
    fn raw_relative_disk_name(&self, name: &str) -> Option<String> {
        // Fold Windows separators so a `\`-separated storage name behaves like
        // the reference on any host.
        let separators = name.replace('\\', "/");
        let path = Path::new(&separators);
        let relative = if path.is_absolute() {
            if self.game_dir.as_os_str().is_empty() {
                path.to_path_buf()
            } else {
                strip_prefix_ignore_ascii_case(path, &self.game_dir)?
            }
        } else {
            path.to_path_buf()
        };
        Some(relative.to_string_lossy().replace('\\', "/"))
    }

    /// [`Self::relative_disk_name`] with a `..` that escapes the mount
    /// reported as [`StorageReadError::InvalidPathName`] instead of `None`.
    fn relative_disk_name_checked(&self, name: &str) -> Result<Option<String>, StorageReadError> {
        let Some(relative) = self.raw_relative_disk_name(name) else {
            return Ok(None);
        };
        match fold_dot_components(&relative) {
            Some(folded) => Ok(Some(folded)),
            None => Err(StorageReadError::InvalidPathName(name.to_string())),
        }
    }

    /// Resolves an existing disk file for a storage name, case-insensitively.
    /// Returns the real on-disk path so writers can update the existing file
    /// instead of creating a case-variant duplicate.
    pub fn find_disk(&self, name: &str) -> Result<Option<PathBuf>, StorageReadError> {
        let Some(relative) = self.relative_disk_name_checked(name)? else {
            return Ok(None);
        };
        let mut trace = SearchTrace::default();
        Ok(match self.find_disk_relative(&relative, &mut trace)? {
            Some(Location::Disk(path)) => Some(path),
            Some(Location::Archive(..)) | None => None,
        })
    }

    /// The case-corrected host path for a storage name below the mount — the
    /// reference `tTVPFileMedia::GetLocallyAccessibleName`
    /// (`reference/cpp/core/base/impl/StorageImpl.cpp:339-407`).
    ///
    /// Every path component is matched against the real directory entries
    /// ASCII-case-insensitively and the real on-disk spelling is returned; from
    /// the first component that does not exist, the requested spelling is kept
    /// for it and the remaining components, like the reference's
    /// `newname += ptr_cur; break;`. Unlike [`Self::find`] this matches
    /// directories too and does not require the final component to exist.
    ///
    /// `Ok(None)` means `name` is not below the mount (including an in-archive
    /// name); a `..` that escapes the mount is
    /// [`StorageReadError::InvalidPathName`].
    pub fn local_name(&self, name: &str) -> Result<Option<PathBuf>, StorageReadError> {
        if name.contains('>') {
            return Ok(None);
        }
        let Some(relative) = self.relative_disk_name_checked(name)? else {
            return Ok(None);
        };
        Ok(Some(self.case_correct_path(&relative)))
    }

    /// Walks `relative` (already folded and `/`-separated) below the game
    /// directory, replacing each existing component with its real on-disk
    /// spelling. From the first component that does not exist, the requested
    /// spelling of every remaining component is appended unchanged.
    fn case_correct_path(&self, relative: &str) -> PathBuf {
        let mut host = self.game_dir.clone();
        let parts: Vec<&str> = relative
            .split('/')
            .filter(|part| !part.is_empty())
            .collect();
        for (index, part) in parts.iter().enumerate() {
            if let Some(real) = self.case_correct_component(&host, part) {
                host.push(real);
            } else {
                // The reference appends the requested spelling of this
                // component and every remaining one, then stops correcting
                // (`newname += ptr_cur`,
                // `reference/cpp/core/base/impl/StorageImpl.cpp:373-377`).
                for rest in &parts[index..] {
                    host.push(*rest);
                }
                break;
            }
        }
        host
    }

    /// The real on-disk spelling of the child of `dir` named `want`, or `None`
    /// when no child matches or the directory cannot be listed.
    fn case_correct_component(&self, dir: &Path, want: &str) -> Option<String> {
        let listing = match self.directory_listing(dir) {
            Ok(listing) => listing,
            // The reference's `opendir` failure appends the requested spelling
            // and stops the walk
            // (`reference/cpp/core/base/impl/StorageImpl.cpp:373-377`).
            Err(_) => return None,
        };
        listing
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(want))
            .map(|(name, _)| name.clone())
    }

    /// Disk branch of [`Self::find`], given an already-resolved relative name
    /// below the game directory.
    fn find_disk_relative(
        &self,
        relative: &str,
        trace: &mut SearchTrace,
    ) -> Result<Option<Location>, StorageReadError> {
        self.find_disk_relative_at(&self.game_dir, relative, trace)
    }

    /// Disk branch of [`Self::find`] below an explicit root directory. An
    /// absolute plain-directory auto path resolves below that host directory;
    /// every other caller passes the game directory.
    fn find_disk_relative_at(
        &self,
        root: &Path,
        relative: &str,
        trace: &mut SearchTrace,
    ) -> Result<Option<Location>, StorageReadError> {
        Ok(self
            .resolve_disk(root, relative, trace)?
            .and_then(|(path, is_dir)| (!is_dir).then_some(Location::Disk(path))))
    }

    /// Resolves a disk-relative name below `root` to an on-disk path plus
    /// whether it is a directory, applying the normalized spelling, then the
    /// script's exact spelling, then the bounded case-insensitive walk. A
    /// missing name is `Ok(None)`; other file-system failures propagate.
    fn resolve_disk(
        &self,
        root: &Path,
        relative: &str,
        trace: &mut SearchTrace,
    ) -> Result<Option<(PathBuf, bool)>, StorageReadError> {
        if relative.is_empty() {
            return Ok(None);
        }
        // Reference semantics: storage names are normalized to lowercase
        // before lookup, so the lowercase spelling wins when both a lowercase
        // and a mixed-case file exist.
        let normalized = normalize_in_archive_name(relative);
        let candidate = root.join(&normalized);
        trace.attempt(|| format!("disk {}", candidate.display()));
        if let Some(is_dir) = self.stat_probe(&candidate)? {
            return Ok(Some((candidate, is_dir)));
        }
        // Fast path: the exact spelling the script used.
        if normalized != relative {
            let exact = root.join(relative);
            trace.attempt(|| format!("disk (exact spelling) {}", exact.display()));
            if let Some(is_dir) = self.stat_probe(&exact)? {
                return Ok(Some((exact, is_dir)));
            }
        }
        // Bounded, component-wise case-insensitive walk.
        trace.attempt(|| {
            format!(
                "disk (case-insensitive walk) {normalized} under {}",
                root.display()
            )
        });
        self.find_disk_case_insensitive(root, &normalized)
    }

    /// Resolves a disk-relative name that must name a directory.
    fn find_disk_dir(&self, relative: &str) -> Result<Option<PathBuf>, StorageReadError> {
        let mut trace = SearchTrace::default();
        Ok(self
            .resolve_disk(&self.game_dir, relative, &mut trace)?
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
        root: &Path,
        relative: &str,
    ) -> Result<Option<(PathBuf, bool)>, StorageReadError> {
        fn descend(
            storage: &Storage,
            dir: &Path,
            parts: &[&str],
        ) -> Result<Option<(PathBuf, bool)>, StorageReadError> {
            let want = parts[0];
            let entries = storage.directory_listing(dir)?;
            // Directories matching this component are collected and only
            // entered after every file at this level has been checked, so a
            // file directly at this level wins.
            let mut subdirs = Vec::new();
            let mut dir_match = None;
            for (name, is_dir) in entries.iter() {
                if !name.eq_ignore_ascii_case(want) {
                    continue;
                }
                let path = dir.join(name.as_str());
                if parts.len() == 1 {
                    if !is_dir {
                        // Validate against the live file system: a file removed
                        // between the listing and this probe must not resolve.
                        if storage.stat_probe(&path)?.is_some() {
                            return Ok(Some((path, false)));
                        }
                    } else if dir_match.is_none() {
                        dir_match = Some(path);
                    }
                } else if *is_dir {
                    subdirs.push(path);
                }
            }
            if let Some(path) = dir_match
                && storage.stat_probe(&path)?.is_some()
            {
                return Ok(Some((path, true)));
            }
            for subdir in subdirs {
                if let Some(found) = descend(storage, &subdir, &parts[1..])? {
                    return Ok(Some(found));
                }
            }
            Ok(None)
        }

        let parts: Vec<&str> = relative.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return Ok(None);
        }
        descend(self, root, &parts)
    }

    /// Runs `f` against the parsed index of the archive at `arc_path`: the
    /// mounted one when present, otherwise a transient open+parse of the file
    /// on disk. The backing is not exposed because the only remaining caller
    /// ([`Self::stat`]) reads index metadata, not entry bytes.
    fn with_archive<T>(
        &self,
        arc_path: &Path,
        f: impl FnOnce(&Xp3Archive, &str) -> Result<T, StorageReadError>,
    ) -> Result<T, StorageReadError> {
        if let Some(mounted) = self.archives.iter().find(|m| m.path == arc_path) {
            return f(&mounted.archive, &mounted.file_name);
        }
        let file = fs::open(&self.fs, &FsPath::host(arc_path), OpenMode::Read)?;
        let archive = Xp3Archive::open(&self.fs, &file)?;
        let file_name = arc_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        f(&archive, &file_name)
    }

    /// Registers a virtual media entry under its full storage name.
    ///
    /// L3 uses this for TVP storage-media providers (`psb://…`, `psd://…`):
    /// the bytes are served by [`Self::read`]/[`Self::read_prefix`] and
    /// reported by [`Self::exists`] before disk and archive resolution. A
    /// later registration for the same name replaces the earlier bytes and
    /// adds one owner; [`Self::unregister_media`] removes one owner and frees
    /// the bytes when the last owner is gone.
    pub fn register_media(&self, name: String, bytes: Vec<u8>) {
        let mut media = self.media.borrow_mut();
        match media.get_mut(&name) {
            Some(entry) => {
                entry.bytes = bytes;
                entry.owners = entry.owners.saturating_add(1);
            }
            None => {
                media.insert(name, MediaEntry { bytes, owners: 1 });
            }
        }
    }

    /// Releases one registration of a virtual media entry.
    ///
    /// Returns `true` when the entry was freed (this was its last owner) and
    /// `false` when the name was unknown or other owners remain. A provider
    /// calls this for every name it registered when its owning object goes,
    /// so the table does not grow with every load.
    pub fn unregister_media(&self, name: &str) -> bool {
        let mut media = self.media.borrow_mut();
        let Some(entry) = media.get_mut(name) else {
            return false;
        };
        if entry.owners > 1 {
            entry.owners -= 1;
            false
        } else {
            media.remove(name);
            true
        }
    }

    /// The number of virtual media names currently registered.
    ///
    /// A diagnostic for tests and host reporting; it is not part of
    /// resolution.
    pub fn media_count(&self) -> usize {
        self.media.borrow().len()
    }

    /// Reads a storage entry to bytes (disk file or archive member).
    pub fn read(&self, name: &str) -> Result<Vec<u8>, StorageReadError> {
        if let Some(entry) = self.media.borrow().get(name).cloned() {
            self.count_read_bytes(entry.bytes.len());
            return Ok(entry.bytes);
        }
        let bytes = match self.find(name)? {
            Some(Location::Disk(path)) => {
                read_fs_file(&self.fs, &path).map_err(StorageReadError::Fs)
            }
            Some(Location::Archive(arc_path, in_arc)) => self.read_archive(&arc_path, &in_arc),
            None => Err(StorageReadError::NotFound(name.to_string())),
        }?;
        self.count_read_bytes(bytes.len());
        Ok(bytes)
    }

    /// Reads at most `max_len` bytes from the start of `name`, through the same
    /// resolution, segment decoding and extraction filter as [`Self::read`]. A
    /// name that does not resolve is [`StorageReadError::NotFound`]. An entry
    /// shorter than `max_len` is returned short; the caller decides whether
    /// that is an error.
    ///
    /// This is our own API: the reference exposes a seekable per-member stream
    /// (`tTVPXP3ArchiveStream`) instead. Resolution and filter semantics are
    /// [`Self::read`]'s.
    pub fn read_prefix(&self, name: &str, max_len: usize) -> Result<Vec<u8>, StorageReadError> {
        if let Some(entry) = self.media.borrow().get(name).cloned() {
            let mut bytes = entry.bytes;
            bytes.truncate(max_len);
            self.count_read_bytes(bytes.len());
            return Ok(bytes);
        }
        let bytes = match self.find(name)? {
            Some(Location::Disk(path)) => {
                read_fs_file_prefix(&self.fs, &path, max_len).map_err(StorageReadError::Fs)
            }
            Some(Location::Archive(arc_path, in_arc)) => {
                self.read_archive_prefix(&arc_path, &in_arc, max_len)
            }
            None => Err(StorageReadError::NotFound(name.to_string())),
        }?;
        self.count_read_bytes(bytes.len());
        Ok(bytes)
    }

    /// Counts entry bytes decoded and returned by [`Self::read`] and
    /// [`Self::read_prefix`].
    fn count_read_bytes(&self, len: usize) {
        self.resolution_cache.borrow_mut().read_bytes += len as u64;
    }

    /// Reads `in_arc` from the mounted archive at `arc_path`, or from a
    /// transient open when the archive is not in the mount list. A mounted
    /// archive reads through its own backing, so a preloaded archive never
    /// touches the file system.
    fn read_archive(&self, arc_path: &Path, in_arc: &str) -> Result<Vec<u8>, StorageReadError> {
        let filter = self.xp3_extraction_filter.as_deref();
        if let Some(mounted) = self.archives.iter().find(|m| m.path == arc_path) {
            let entry = mounted
                .archive
                .entry(in_arc)
                .ok_or_else(|| StorageReadError::NotFound(in_arc.to_string()))?;
            return match &mounted.backing {
                ArchiveBacking::Stream(file) => mounted
                    .archive
                    .read_entry_with_filter(&self.fs, file, entry, &mounted.file_name, filter)
                    .map_err(StorageReadError::Xp3),
                ArchiveBacking::Memory(bytes) => mounted
                    .archive
                    .read_entry_from_bytes(bytes, entry, &mounted.file_name, filter)
                    .map_err(StorageReadError::Xp3),
            };
        }
        let file = fs::open(&self.fs, &FsPath::host(arc_path), OpenMode::Read)?;
        let archive = Xp3Archive::open(&self.fs, &file)?;
        let file_name = arc_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        archive
            .read_with_filter(&self.fs, &file, in_arc, &file_name, filter)
            .map_err(StorageReadError::Xp3)
    }

    /// Prefix counterpart of [`Self::read_archive`]: the same backing and filter
    /// selection, decoding only the requested bytes.
    fn read_archive_prefix(
        &self,
        arc_path: &Path,
        in_arc: &str,
        max_len: usize,
    ) -> Result<Vec<u8>, StorageReadError> {
        let filter = self.xp3_extraction_filter.as_deref();
        if let Some(mounted) = self.archives.iter().find(|m| m.path == arc_path) {
            return match &mounted.backing {
                ArchiveBacking::Stream(file) => mounted
                    .archive
                    .read_prefix_with_filter(
                        &self.fs,
                        file,
                        in_arc,
                        &mounted.file_name,
                        max_len,
                        filter,
                    )
                    .map_err(StorageReadError::Xp3),
                ArchiveBacking::Memory(bytes) => {
                    let entry = mounted
                        .archive
                        .entry(in_arc)
                        .ok_or_else(|| StorageReadError::NotFound(in_arc.to_string()))?;
                    mounted
                        .archive
                        .read_entry_prefix_from_bytes(
                            bytes,
                            entry,
                            &mounted.file_name,
                            max_len,
                            filter,
                        )
                        .map_err(StorageReadError::Xp3)
                }
            };
        }
        let file = fs::open(&self.fs, &FsPath::host(arc_path), OpenMode::Read)?;
        let archive = Xp3Archive::open(&self.fs, &file)?;
        let file_name = arc_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        archive
            .read_prefix_with_filter(&self.fs, &file, in_arc, &file_name, max_len, filter)
            .map_err(StorageReadError::Xp3)
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
                self.with_archive(&arc_path, |archive, _file_name| {
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
        if self.media.borrow().contains_key(name) {
            return Ok(true);
        }
        if self.find(name)?.is_some() {
            return Ok(true);
        }
        let Some(relative) = self.relative_disk_name_checked(name)? else {
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
/// The reference never scans a folder, but a game directory is the ergonomic
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

/// Strips `prefix` from the leading components of `path`, comparing each
/// component ASCII-case-insensitively. Returns the remaining path, or `None`
/// when `path` does not start with `prefix`. The reference resolves an
/// absolute name by walking its components against the real directory
/// entries, so an absolute path that differs from the mount point only in
/// case still points inside the mount
/// (`reference/cpp/core/base/impl/StorageImpl.cpp:301-407`).
fn strip_prefix_ignore_ascii_case(path: &Path, prefix: &Path) -> Option<PathBuf> {
    let mut remaining = path.components();
    for expected in prefix.components() {
        let actual = remaining.next()?;
        let actual = actual.as_os_str().to_string_lossy();
        let expected = expected.as_os_str().to_string_lossy();
        if !actual.eq_ignore_ascii_case(&expected) {
            return None;
        }
    }
    Some(remaining.as_path().to_path_buf())
}

/// Folds `.` and `..` components the way the reference storage-name normalizer
/// does: `.` is dropped, `..` removes the preceding component, and a `..`
/// that would escape the root is rejected with `None`. The input is already
/// `/`-separated.
fn fold_dot_components(relative: &str) -> Option<String> {
    let mut parts: Vec<&str> = Vec::new();
    for component in relative.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            _ => parts.push(component),
        }
    }
    Some(parts.join("/"))
}

/// Normalizes an in-archive name (lowercase, `\` to `/`, duplicate-slash
/// collapse) and then folds `.`/`..`. `None` means a `..` crossed the archive
/// boundary, which the reference raises as `TVPInvalidPathName`.
fn normalize_in_archive_path(name: &str) -> Option<String> {
    fold_dot_components(&normalize_in_archive_name(name))
}

/// Inserts the direct children of `prefix` from an archive's entries into
/// `out`, keyed by the lower-cased base name and valued by the normalized
/// in-archive name.
fn collect_archive_children<'a>(
    entries: impl Iterator<Item = &'a xp3::Entry>,
    prefix: &str,
    out: &mut HashMap<String, String>,
) {
    for entry in entries {
        let Some(rest) = entry.name.strip_prefix(prefix) else {
            continue;
        };
        if rest.is_empty() || rest.contains('/') {
            continue;
        }
        out.insert(rest.to_string(), entry.name.clone());
    }
}

/// The base storage name (the part after the last `/`).
fn storage_base(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// How many near-miss names one archive-miss line lists.
const ARCHIVE_MISS_NEAR_NAMES: usize = 8;

/// Renders one archive probe for a miss: the entry count, the index blocks
/// that were read (how many names each contributed), and — when `detailed` — a
/// bounded set of near-miss names.
///
/// The near-miss scan walks every entry in the index, so it is built only for
/// the full `trace` line; the bounded `debug` line keeps the cheap header and
/// omits it.
fn archive_miss_line(path: &Path, archive: &Xp3Archive, request: &str, detailed: bool) -> String {
    let sections: Vec<String> = archive
        .index_sections()
        .iter()
        .map(|section| {
            let encode = match section.encode {
                xp3::IndexEncode::Raw => "raw",
                xp3::IndexEncode::Zlib => "zlib",
            };
            format!(
                "{encode} {} names (+{})",
                section.file_chunks, section.new_names
            )
        })
        .collect();
    let mut line = format!(
        "archive {} entry {request:?}: {} entries, {} index section(s) [{}]",
        path.display(),
        archive.len(),
        sections.len(),
        sections.join(", "),
    );
    if !detailed {
        return line;
    }
    let near = archive.near_misses(request, ARCHIVE_MISS_NEAR_NAMES);
    if near.total == 0 {
        line.push_str("; no near misses");
    } else {
        let names = near
            .names
            .iter()
            .map(|name| format!("{name:?}"))
            .collect::<Vec<_>>()
            .join(", ");
        line.push_str(&format!(
            "; nearest {}/{}: {names}",
            near.names.len(),
            near.total,
        ));
    }
    line
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

/// Reads at most `max_len` bytes from the start of a whole file.
///
/// The prefix of a host file is not filtered: the extraction filter is an
/// archive-segment transform only.
fn read_fs_file_prefix(fs: &FileSystem, path: &Path, max_len: usize) -> Result<Vec<u8>, FsError> {
    let file = fs::open(fs, &FsPath::host(path), OpenMode::Read)?;
    let size = fs::file_size(fs, &file)?;
    let want = usize::try_from(size.min(max_len as u64)).map_err(|_| FsError::Io {
        message: "file size does not fit in usize".to_string(),
    })?;
    let mut out = Vec::new();
    out.try_reserve_exact(want).map_err(|_| FsError::Io {
        message: "file size cannot be allocated".to_string(),
    })?;
    out.resize(want, 0);
    let mut read = 0usize;
    while read < want {
        let n = fs::read_at(fs, &file, read as u64, &mut out[read..])?;
        if n == 0 {
            break;
        }
        read += n;
    }
    out.truncate(read);
    Ok(out)
}

/// Reads the whole of an open archive file into memory.
///
/// The size the file system reports is authoritative: a file that ends before
/// it is a typed error, never a shortened buffer. There is no size cap; a size
/// the platform cannot represent or allocate is a typed error instead. The
/// returned image is owned by the storage and handed to L1 for each entry
/// read; L1 never keeps it.
fn read_archive_image(
    fs: &FileSystem,
    file: &FileState,
    path: &Path,
) -> Result<Vec<u8>, MountError> {
    let size = fs::file_size(fs, file)?;
    let display = path.display().to_string();
    let len = usize::try_from(size).map_err(|_| MountError::ArchiveTooLarge {
        path: display.clone(),
        size,
    })?;
    let mut image = Vec::new();
    image
        .try_reserve_exact(len)
        .map_err(|_| MountError::ArchiveAllocation {
            path: display.clone(),
            size,
        })?;
    image.resize(len, 0);
    let mut read = 0usize;
    while read < len {
        let n = fs::read_at(fs, file, read as u64, &mut image[read..])?;
        if n == 0 {
            return Err(MountError::TruncatedArchive {
                path: display,
                expected: size,
                read: read as u64,
            });
        }
        read += n;
    }
    Ok(image)
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

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        /// Log lines written on this thread since the last clear. Tests run in
        /// parallel, so the capture is per-thread.
        static LINES: std::cell::RefCell<Vec<String>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// A `log` sink that records every record on the calling thread.
    struct CaptureLogger;

    impl log::Log for CaptureLogger {
        fn enabled(&self, _metadata: &log::Metadata) -> bool {
            true
        }

        fn log(&self, record: &log::Record) {
            LINES.with(|lines| lines.borrow_mut().push(record.args().to_string()));
        }

        fn flush(&self) {}
    }

    /// Installs the recording sink once per test binary, clears this thread's
    /// buffer, runs `f`, and returns its result plus the thread's log lines.
    fn capture<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            let _ = log::set_boxed_logger(Box::new(CaptureLogger));
            log::set_max_level(log::LevelFilter::Debug);
        });
        LINES.with(|lines| lines.borrow_mut().clear());
        let result = f();
        let lines = LINES.with(|lines| lines.borrow().clone());
        (result, lines)
    }

    /// Writes one file into an in-memory root, creating parent directories.
    fn write_mem(fs: &FileSystem, path: &str, data: &[u8]) {
        let path_obj = Path::new(path);
        if let Some(parent) = path_obj.parent()
            && !parent.as_os_str().is_empty()
        {
            fs::create_dir_all(fs, &FsPath::host(parent)).expect("create parent dirs");
        }
        let file = fs::open(fs, &FsPath::host(path), OpenMode::Write).expect("open memory file");
        fs::write_at(fs, &file, 0, data).expect("write memory file");
        fs::flush(fs, &file).expect("flush memory file");
    }

    /// The shape grouping keeps the first candidate of each shape literal and
    /// collapses the rest to the entry name, so the candidate rule is stated
    /// once without losing any searched location.
    #[test]
    fn auto_path_trace_groups_by_shape() {
        let mut trace = AutoPathTrace::new("config.tjs");
        trace.record_folder("parts/", "parts/config.tjs".to_string());
        trace.record_folder("other/", "other/config.tjs".to_string());
        trace.record_archive("data.xp3>system/", "data.xp3>system/config.tjs".to_string());

        let rendered = trace.render();
        assert!(rendered.contains("parts/config.tjs"), "{rendered}");
        assert!(rendered.contains("other/"), "{rendered}");
        assert!(
            rendered.contains("data.xp3>system/config.tjs"),
            "{rendered}"
        );
        assert!(
            !rendered.contains("other/config.tjs"),
            "the repeated candidate is collapsed: {rendered}"
        );

        // A folder entry without a trailing slash still shows the inserted
        // separator in its example candidate.
        let mut no_slash = AutoPathTrace::new("config.tjs");
        no_slash.record_folder("nested", "nested/config.tjs".to_string());
        assert_eq!(
            no_slash.render(),
            "auto paths (later added first; candidates append \"config.tjs\"): nested/config.tjs"
        );
    }

    /// `render_short` cuts on a character boundary, so a non-ASCII path never
    /// splits a UTF-8 code point; a trace within the budget is unchanged.
    #[test]
    fn render_short_never_splits_a_utf8_char() {
        let mut trace = SearchTrace::new(true, true);
        trace.attempt(|| "é".repeat(300));
        assert_eq!(
            trace.render_short(10),
            format!("{}… (+290 chars)", "é".repeat(10))
        );

        let mut short = SearchTrace::new(true, true);
        short.attempt(|| "ok".to_string());
        assert_eq!(short.render_short(10), "ok");
    }

    /// The 41-auto-path startup shape still names every entry in the full
    /// trace, while the bounded `debug` line keeps only the head and names how
    /// much it dropped.
    #[test]
    fn storage_miss_line_is_bounded_and_the_trace_names_every_auto_path() {
        const AUTO_PATHS: usize = 41;
        let fs = FileSystem::memory();
        for i in 0..AUTO_PATHS {
            write_mem(&fs, &format!("/game/ap{i:02}/present.dat"), b"x");
        }
        let mut storage =
            Storage::mount_archives(fs, "/game", std::iter::empty::<&str>()).expect("mount");
        for i in 0..AUTO_PATHS {
            storage.add_auto_path(format!("ap{i:02}/"));
        }

        // The full trace still names every entry after the compaction.
        let mut trace = SearchTrace::new(true, true);
        assert!(
            storage
                .find_recorded("missing000.txt", &mut trace)
                .unwrap()
                .is_none()
        );
        let full = trace.render();
        for i in 0..AUTO_PATHS {
            assert!(
                full.contains(&format!("ap{i:02}/")),
                "entry ap{i:02}/ is not named: {full}"
            );
        }
        assert!(full.contains("ap40/missing000.txt"), "{full}");
        assert!(full.contains("/game/missing000.txt"), "{full}");
        assert!(
            !full.contains("auto path \""),
            "the per-entry boilerplate is gone: {full}"
        );

        // The `debug` line keeps a bounded head and names the dropped count.
        let (found, logs) = capture(|| storage.find("missing000.txt"));
        assert!(found.unwrap().is_none());
        let miss = logs
            .iter()
            .find(|line| line.contains("storage miss"))
            .expect("one miss line");
        assert!(miss.contains('…'), "the debug line is bounded: {miss}");
        assert!(
            miss.contains("(+") && miss.contains(" chars)"),
            "the dropped count is named: {miss}"
        );
        assert!(
            miss.len() < 300,
            "the bounded miss line is still large: {miss}"
        );
    }

    /// The cheap `debug` trace skips the archive near-miss scan, which walks
    /// every entry in the index; the full `trace` still carries the near-miss
    /// names.
    #[test]
    fn debug_trace_skips_the_archive_near_miss_scan() {
        let xp3 = testkit::build_xp3(&[("system/Config.tjs", b"cfg", false)]);
        let fs = FileSystem::memory();
        write_mem(&fs, "/game/data.xp3", &xp3);
        let storage = Storage::mount_archives(fs, "/game", ["data.xp3"]).expect("mount");

        let mut debug_trace = SearchTrace::new(true, false);
        assert!(
            storage
                .find_recorded("config.tjs", &mut debug_trace)
                .unwrap()
                .is_none()
        );
        let debug_line = debug_trace.render();
        assert!(debug_line.contains("1 entries"), "{debug_line}");
        assert!(
            !debug_line.contains("nearest") && !debug_line.contains("system/config.tjs"),
            "the cheap debug trace omits the near-miss scan: {debug_line}"
        );

        let mut full_trace = SearchTrace::new(true, true);
        assert!(
            storage
                .find_recorded("config.tjs", &mut full_trace)
                .unwrap()
                .is_none()
        );
        let full_line = full_trace.render();
        assert!(full_line.contains("nearest 1/1"), "{full_line}");
        assert!(full_line.contains("system/config.tjs"), "{full_line}");
    }

    /// The trace level is not part of resolution: the cheap `debug` trace and
    /// the full `trace` record the same result for a hit and for a miss.
    #[test]
    fn trace_detail_does_not_change_resolution() {
        let xp3 = testkit::build_xp3(&[
            ("system/Config.tjs", b"cfg", false),
            ("data/bg/title.jpg", b"img", false),
        ]);
        let fs = FileSystem::memory();
        write_mem(&fs, "/game/data.xp3", &xp3);
        let storage = Storage::mount_archives(fs, "/game", ["data.xp3"]).expect("mount");

        for name in [
            "system/Config.tjs",
            "Config.tjs",
            "data/bg/title.jpg",
            "missing.txt",
        ] {
            let mut debug_trace = SearchTrace::new(true, false);
            let mut full_trace = SearchTrace::new(true, true);
            assert_eq!(
                storage.find_recorded(name, &mut debug_trace).unwrap(),
                storage.find_recorded(name, &mut full_trace).unwrap(),
                "resolution differs for {name:?} between trace levels"
            );
        }
    }
}
