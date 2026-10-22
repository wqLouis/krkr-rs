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
//! a borrow of it is handed to L1 for every entry read. The image is read in
//! the background, so the mount does not wait for the archive's bytes.
//!
//! The bytes of an entry are read on a worker pool
//! ([`StorageReadPool`]): a lookup answers the entry's existence and its size
//! without reading the bytes, the raw read and the archive decompression run
//! on a worker, and the data access waits for that read. The pool is this
//! layer's own state — it is installed on the storage and drained by the
//! engine every frame — and there is no process-global registry.
//!
//! Resolution order mirrors the reference engine:
//!
//! 1. explicit `arc.xp3>path` addressing,
//! 2. the current project storage: the project archive for an archive
//!    project, the app directory for a folder project,
//! 3. the auto search-path list, later added first,
//! 4. the remaining mounted archives, later mounts first,
//! 5. the app directory as the loose-file fallback for an archive project —
//!    disk files below it are normalized before the script's spelling and the
//!    bounded component-wise case-insensitive walk.

use std::cell::RefCell;
use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use api::error::FsError;
use api::fs::{FsPath, OpenMode};
use fs::xp3::{self, Entry, Xp3Archive, Xp3ExtractionFilter, normalize_in_archive_name};
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
    archive: Arc<Xp3Archive>,
}

/// Where a mounted archive's bytes are read from.
///
/// The bytes are owned by the storage; the stateless L1 reader is handed the
/// backing on every entry read and never holds it. The backing is shared, so
/// a worker thread can read an entry of a mounted archive while the frame
/// thread reads another one.
#[derive(Clone)]
pub enum ArchiveBacking {
    /// The archive file stays open and each entry read is positional.
    Stream(Arc<FileState>),
    /// The archive file stays open and its whole image is read into memory in
    /// the background; an entry read copies from the image once it is loaded
    /// and reads the entry from the file until then.
    Image(Arc<ArchiveImage>),
}

/// The whole image of one preloaded archive.
///
/// The image is read on its own thread, so the mount returns as soon as the
/// index is parsed and the archive's bytes are read while the game runs. An
/// entry read copies from the image once it is loaded and reads the entry
/// from the archive file until then; the bytes are the same either way. A
/// load that fails is reported as a typed read error and is never ignored.
pub struct ArchiveImage {
    /// The bottom file system the archive is read through.
    fs: FileSystem,
    /// The open archive file; the entry reads use it while the image loads.
    file: Arc<FileState>,
    /// The load's state.
    state: Mutex<ImageState>,
}

/// The state of one archive image load.
enum ImageState {
    /// The image is being read.
    Loading,
    /// The image is in memory.
    Ready(Arc<[u8]>),
    /// The load failed.
    Failed(PreloadError),
}

impl ArchiveImage {
    /// Creates an image whose load has not started.
    fn pending(fs: &FileSystem, file: &Arc<FileState>) -> Arc<Self> {
        Arc::new(ArchiveImage {
            fs: fs.clone(),
            file: Arc::clone(file),
            state: Mutex::new(ImageState::Loading),
        })
    }

    /// Starts the background load of an archive file's whole image.
    ///
    /// The load runs on its own thread, so the mount does not wait for the
    /// archive's bytes: the index is parsed at the mount and the image is
    /// filled while the game runs. A thread that cannot be started loads the
    /// image on the calling thread instead, which keeps the image in memory
    /// before the first entry read.
    ///
    /// `len` is the archive's size in bytes, already checked to be
    /// addressable by the mount.
    fn start(fs: &FileSystem, file: &Arc<FileState>, path: &Path, len: usize) -> Arc<Self> {
        let image = Self::pending(fs, file);
        let worker_fs = fs.clone();
        let worker_file = Arc::clone(file);
        let worker_path = path.to_path_buf();
        let worker_image = Arc::clone(&image);
        let worker = std::thread::Builder::new()
            .name("krkr-archive-image".to_owned())
            .spawn(move || {
                let started = Instant::now();
                let result = load_archive_image(&worker_fs, &worker_file, &worker_path, len);
                worker_image.publish(&worker_path, result, started.elapsed());
            });
        if let Err(error) = worker {
            log::warn!(
                "the archive image of {} is loaded on the mounting thread: {error}",
                path.display()
            );
            let started = Instant::now();
            let result = load_archive_image(fs, file, path, len);
            image.publish(path, result, started.elapsed());
        }
        image
    }

    /// Publishes the load's result and reports how long the load took.
    fn publish(&self, path: &Path, result: Result<Arc<[u8]>, PreloadError>, elapsed: Duration) {
        let mut state = lock(&self.state);
        *state = match result {
            Ok(image) => {
                log::info!(
                    "the archive image of {} is loaded ({} bytes in {:.1} ms)",
                    path.display(),
                    image.len(),
                    elapsed.as_secs_f64() * 1000.0
                );
                ImageState::Ready(image)
            }
            Err(error) => {
                log::warn!(
                    "the archive image of {} could not be read: {error}",
                    path.display()
                );
                ImageState::Failed(error)
            }
        };
    }

    /// The image, once the load has finished; `None` while it is still
    /// running.
    fn loaded(&self) -> Result<Option<Arc<[u8]>>, PreloadError> {
        let state = lock(&self.state);
        match &*state {
            ImageState::Loading => Ok(None),
            ImageState::Ready(image) => Ok(Some(Arc::clone(image))),
            ImageState::Failed(error) => Err(error.clone()),
        }
    }

    /// Reads the entry `entry` of `archive`, at most `limit` bytes.
    ///
    /// The bytes come from the image once it is loaded and from the archive
    /// file until then.
    fn read_entry(
        &self,
        archive: &Xp3Archive,
        entry: &Entry,
        archive_name: &str,
        limit: Option<usize>,
        filter: Option<&Xp3ExtractionFilter<'_>>,
    ) -> Result<Vec<u8>, StorageReadError> {
        match self.loaded()? {
            Some(image) => match limit {
                Some(limit) => archive
                    .read_entry_prefix_from_bytes(&image, entry, archive_name, limit, filter)
                    .map_err(xp3_error),
                None => archive
                    .read_entry_from_bytes(&image, entry, archive_name, filter)
                    .map_err(xp3_error),
            },
            None => match limit {
                Some(limit) => archive
                    .read_prefix_with_filter(
                        &self.fs,
                        &self.file,
                        &entry.name,
                        archive_name,
                        limit,
                        filter,
                    )
                    .map_err(xp3_error),
                None => archive
                    .read_entry_with_filter(&self.fs, &self.file, entry, archive_name, filter)
                    .map_err(xp3_error),
            },
        }
    }
}

/// A mounted archive's index together with the bytes it reads from.
#[derive(Clone)]
pub struct ArchiveSource {
    /// The archive's parsed index.
    pub archive: Arc<Xp3Archive>,
    /// Where the archive's bytes come from.
    pub backing: ArchiveBacking,
}

/// Options controlling how [`Storage`] mounts archives.
///
/// The default keeps every archive on the file system and reads each entry
/// positionally. Enabling [`preload_archives`](Self::preload_archives) reads
/// each archive into memory in full while the game runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct MountOptions {
    /// Read each mounted archive into memory in full.
    ///
    /// The resident-memory cost is the archive's own size; there is no cap.
    /// Every entry then reads from that buffer instead of the file system,
    /// which removes the per-entry random reads a slow backing store makes
    /// visible. The image is read in the background from the mount, so the
    /// mount does not wait for the archive's bytes: an entry read copies
    /// from the image once it is loaded and reads the entry from the file
    /// until then. A load that fails is a typed [`PreloadError`], never a
    /// silent fall back to the streaming path.
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
    /// Whether an archive's whole image is read into memory in the background.
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
    media: RefCell<crate::hasher::Map<String, Vec<u8>>>,
    /// Lazily built resolver tables, invalidated when the auto-path list or
    /// the mounted archives change.
    resolution_cache: RefCell<ResolutionCache>,
    /// The worker pool the entry reads run on, or `None` when every read runs
    /// on the calling thread.
    ///
    /// The pool is this layer's own state: it holds the worker threads, the
    /// queue, the in-flight reads and the drained results. The engine drains
    /// it every frame, so a read the previous frame scheduled is published
    /// before the next frame's script runs.
    pub read_pool: Option<Arc<StorageReadPool>>,
    /// The resolution generation the scheduled reads were made against.
    ///
    /// The generation changes whenever the mounts or the auto paths change,
    /// so a read that was scheduled against an older resolution is never
    /// served to a newer one.
    generation: u64,
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
type ArchiveChildren = (PathBuf, crate::hasher::Map<String, String>);

/// A cached auto-path candidate table, keyed by the auto-path entry.
enum AutoPathTable {
    /// A plain directory auto path.
    Folder {
        /// Absolute directory that was listed.
        root: PathBuf,
        /// Real child name -> `is_dir`, for the exact lower-cased probe.
        exact: crate::hasher::Map<String, bool>,
        /// Lower-cased child name -> the first **file** child with that name.
        first_file: crate::hasher::Map<String, String>,
    },
    /// An archive auto path with an in-archive prefix.
    Archive {
        /// Absolute path of the archive.
        archive: PathBuf,
        /// Lower-cased direct-child base name -> normalized in-archive name.
        entries: crate::hasher::Map<String, String>,
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
struct ResolutionCache {
    /// Candidate tables keyed by the auto-path entry string.
    auto_paths: crate::hasher::Map<String, Rc<AutoPathTable>>,
    /// Resolved hits keyed by the requested storage name (the reference's
    /// `TVPAutoPathCache`: a hit is cached, a miss is not).
    placed: crate::hasher::Map<String, Location>,
    /// Bottom file-system probes issued while resolving.
    probes: ProbeCounts,
    /// Entry bytes decoded and returned by `read`/`read_prefix`.
    read_bytes: u64,
}

impl Default for ResolutionCache {
    fn default() -> Self {
        Self {
            auto_paths: crate::hasher::map(),
            placed: crate::hasher::map(),
            probes: ProbeCounts::default(),
            read_bytes: 0,
        }
    }
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
    /// A preloaded archive is larger than this platform can address in a
    /// single buffer.
    #[error("archive {path} is too large to hold in memory: {size} bytes")]
    ArchiveTooLarge {
        /// Archive path as mounted.
        path: String,
        /// Reported size in bytes.
        size: u64,
    },
}

/// A preloaded archive's image could not be read into memory.
///
/// The image is read in the background, so the failure is reported to the
/// read that needs the image rather than to the mount. It is a typed error
/// in every case: the archive never falls back to serving the entry from
/// the file after a failed load.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum PreloadError {
    /// The archive was shorter than the size the mount read.
    #[error("archive {path} is truncated: read {read} of {expected} bytes")]
    Truncated {
        /// Archive path as mounted.
        path: String,
        /// Size the archive reported.
        expected: u64,
        /// Bytes actually read before the file ended.
        read: u64,
    },
    /// The archive's buffer could not be allocated.
    #[error("archive {path} could not be read into memory: {size} bytes")]
    Allocation {
        /// Archive path as mounted.
        path: String,
        /// Reported size in bytes.
        size: u64,
    },
    /// A bottom file-system read failed.
    #[error("archive {path} could not be read: {message}")]
    Io {
        /// Archive path as mounted.
        path: String,
        /// What the file system reported.
        message: String,
    },
}

/// Errors while reading or listing storage entries.
#[derive(Debug, Clone, thiserror::Error)]
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
    /// An archive read failed; the message is the archive reader's own.
    #[error("archive error: {0}")]
    Xp3(String),
    /// A preloaded archive's image could not be read into memory.
    #[error("archive image error: {0}")]
    Preload(#[from] PreloadError),
    /// The read produced no result: the worker stopped before it published
    /// one, or the read was cancelled.
    #[error("the storage read produced no result: {message}")]
    NoResult {
        /// What the pool reported.
        message: String,
    },
}

// ---------------------------------------------------------------------------
// The storage read pool
// ---------------------------------------------------------------------------

/// The identity of one scheduled read.
///
/// A read is identified by the storage name it was made for, the number of
/// bytes it wants and the resolution generation it was made against. The
/// generation changes whenever the mounts or the auto paths change, so a read
/// that was scheduled against an older resolution is never served to a newer
/// one.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StorageKey {
    /// The storage name the read was made for.
    name: String,
    /// The number of bytes the read wants, or `None` for the whole entry.
    limit: Option<usize>,
    /// The resolution generation the read was made against.
    generation: u64,
}

impl StorageKey {
    /// The identity of a read of `name` that wants `limit` bytes.
    ///
    /// `limit` is `None` for the whole entry.
    pub fn new(name: impl Into<String>, limit: Option<usize>, generation: u64) -> Self {
        Self {
            name: name.into(),
            limit,
            generation,
        }
    }

    /// The storage name the read was made for.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// The work one scheduled read performs.
#[derive(Clone)]
pub enum StorageJob {
    /// A file on the bottom file system.
    Disk {
        /// The bottom file system the file is read through.
        fs: FileSystem,
        /// The path of the file.
        path: PathBuf,
        /// The number of bytes to read, or `None` for the whole file.
        limit: Option<usize>,
    },
    /// An entry of an archive.
    Archive {
        /// The bottom file system the archive is read through.
        fs: FileSystem,
        /// The mounted archive's index and bytes, or `None` when the archive
        /// is not mounted and the worker opens it.
        source: Option<ArchiveSource>,
        /// The archive's path on the bottom file system.
        path: PathBuf,
        /// The normalized in-archive name.
        in_arc: String,
        /// The archive's logical name.
        archive_name: String,
        /// The number of bytes to read, or `None` for the whole entry.
        limit: Option<usize>,
    },
}

/// A resolved read: the work to perform and the sizes the resolution named.
struct ReadPlan {
    /// The work the read performs.
    work: StorageJob,
    /// The size of the entry, when the resolution could name it.
    size: Option<u64>,
    /// The number of bytes the read is charged against the pool's memory
    /// limit.
    charge: u64,
}

/// The backend that performs a scheduled read.
///
/// The implementation reads through the bottom file system; the middle layer
/// owns the value and calls it. A reader is stateless and thread-safe: the
/// pool calls it from several worker threads at once, so the implementation
/// holds no per-call state.
pub trait StorageReader: Send + Sync {
    /// Reads the bytes the job names.
    ///
    /// A failure is the storage's own typed error; there is no substitute
    /// value and no empty result.
    fn read(&self, job: &StorageJob) -> Result<Vec<u8>, StorageReadError>;
}

/// The reader that reads through the bottom file system.
///
/// It is the default reader of the storage read pool: a disk file is read
/// positionally and an archive entry through the archive's own backing.
#[derive(Clone, Copy, Debug, Default)]
pub struct FileSystemReader;

impl StorageReader for FileSystemReader {
    fn read(&self, job: &StorageJob) -> Result<Vec<u8>, StorageReadError> {
        match job {
            StorageJob::Disk { fs, path, limit } => {
                let read = match limit {
                    Some(limit) => read_fs_file_prefix(fs, path, *limit),
                    None => read_fs_file(fs, path),
                };
                read.map_err(StorageReadError::Fs)
            }
            StorageJob::Archive {
                fs,
                source,
                path,
                in_arc,
                archive_name,
                limit,
            } => read_archive_entry(fs, source.as_ref(), path, in_arc, archive_name, *limit),
        }
    }
}

/// Reads an archive entry through the mounted archive's backing, or through a
/// transient open when the archive is not mounted.
fn read_archive_entry(
    fs: &FileSystem,
    source: Option<&ArchiveSource>,
    path: &Path,
    in_arc: &str,
    archive_name: &str,
    limit: Option<usize>,
) -> Result<Vec<u8>, StorageReadError> {
    if let Some(source) = source {
        return match &source.backing {
            ArchiveBacking::Stream(file) => match limit {
                Some(limit) => source
                    .archive
                    .read_prefix_with_filter(fs, file, in_arc, archive_name, limit, None)
                    .map_err(xp3_error),
                None => source
                    .archive
                    .read_with_filter(fs, file, in_arc, archive_name, None)
                    .map_err(xp3_error),
            },
            ArchiveBacking::Image(image) => {
                let entry = source
                    .archive
                    .entry(in_arc)
                    .ok_or_else(|| xp3_error(xp3::Xp3Error::NotFound(in_arc.to_string())))?;
                image.read_entry(&source.archive, entry, archive_name, limit, None)
            }
        };
    }
    let file = fs::open(fs, &FsPath::host(path), OpenMode::Read).map_err(StorageReadError::Fs)?;
    let archive = Xp3Archive::open(fs, &file).map_err(xp3_error)?;
    match limit {
        Some(limit) => archive
            .read_prefix_with_filter(fs, &file, in_arc, archive_name, limit, None)
            .map_err(xp3_error),
        None => archive
            .read_with_filter(fs, &file, in_arc, archive_name, None)
            .map_err(xp3_error),
    }
}

/// The bytes a read outcome holds.
///
/// A failed or cancelled read is the storage's own typed error; there is no
/// substitute value and no empty result. A file that vanished between the
/// resolution and the read is the same not-found the resolution reports.
fn outcome_to_bytes(outcome: StorageOutcome, name: &str) -> Result<Vec<u8>, StorageReadError> {
    match outcome {
        StorageOutcome::Ready(bytes) => Ok((*bytes).clone()),
        StorageOutcome::Failed(StorageReadError::Fs(FsError::NotFound { .. })) => {
            Err(StorageReadError::NotFound(name.to_string()))
        }
        StorageOutcome::Failed(error) => Err(error),
        StorageOutcome::Cancelled => Err(StorageReadError::NoResult {
            message: "the read was cancelled before it produced a result".to_string(),
        }),
    }
}

/// Wraps an archive error in the storage's own typed error.
///
/// The archive reader's own message is kept, so a caller sees the same words
/// a synchronous read would have produced.
fn xp3_error(error: xp3::Xp3Error) -> StorageReadError {
    StorageReadError::Xp3(error.to_string())
}

/// The result of a scheduled read.
#[derive(Clone, Debug)]
pub enum StorageOutcome {
    /// The read produced these bytes.
    Ready(Arc<Vec<u8>>),
    /// The read failed; the error is the storage's own typed error.
    Failed(StorageReadError),
    /// The read was cancelled before it produced a result.
    Cancelled,
}

impl StorageOutcome {
    /// The number of bytes the outcome holds.
    fn byte_len(&self) -> u64 {
        match self {
            StorageOutcome::Ready(bytes) => bytes.len() as u64,
            StorageOutcome::Failed(_) | StorageOutcome::Cancelled => 0,
        }
    }
}

/// The result slot and the wake-up a pending read shares with its waiters.
#[derive(Debug)]
struct PendingShared {
    /// The result slot; `None` while the read has not produced a result.
    state: Mutex<Option<StorageOutcome>>,
    /// Signalled when the slot leaves `None`.
    ready: Condvar,
}

/// A read that has not produced its bytes yet.
///
/// The handle is created by [`StorageReadPool::submit`] and handed to the
/// caller that scheduled the read. Every access to the bytes waits for it:
/// the wait is on this read alone, never on the pool.
#[derive(Clone, Debug)]
pub struct PendingStorage {
    /// The shared result slot and the wake-up.
    shared: Arc<PendingShared>,
    /// The size the lookup named for the entry.
    size: Option<u64>,
}

impl PendingStorage {
    /// Creates a pending read with no result yet.
    fn new(size: Option<u64>) -> Self {
        Self {
            shared: Arc::new(PendingShared {
                state: Mutex::new(None),
                ready: Condvar::new(),
            }),
            size,
        }
    }

    /// Creates a handle that already holds the bytes of the entry.
    fn ready(bytes: Vec<u8>, size: Option<u64>) -> Self {
        let pending = Self::new(size);
        {
            let mut state = lock(&pending.shared.state);
            *state = Some(StorageOutcome::Ready(Arc::new(bytes)));
        }
        pending
    }

    /// The size the lookup named for the entry.
    ///
    /// `None` when the resolution could not name it: an archive that is not
    /// mounted is opened by the worker, so its index is not known before the
    /// read.
    pub fn size(&self) -> Option<u64> {
        self.size
    }

    /// Whether the read has produced a result.
    pub fn is_ready(&self) -> bool {
        lock(&self.shared.state).is_some()
    }

    /// The current result, without waiting.
    pub fn outcome(&self) -> Option<StorageOutcome> {
        lock(&self.shared.state).clone()
    }

    /// Waits until the read produced a result and returns it.
    ///
    /// The wait releases the lock while it sleeps, and a result published
    /// between the check and the sleep is not missed.
    pub fn await_outcome(&self) -> StorageOutcome {
        let mut state = lock(&self.shared.state);
        loop {
            if let Some(outcome) = state.as_ref() {
                return outcome.clone();
            }
            state = match self.shared.ready.wait(state) {
                Ok(guard) => guard,
                Err(poisoned) => poisoned.into_inner(),
            };
        }
    }

    /// Cancels the read.
    ///
    /// A waiter sees [`StorageOutcome::Cancelled`] and a worker discards the
    /// result it would have published. Returns whether the read was still
    /// waiting.
    pub fn cancel(&self) -> bool {
        let mut state = lock(&self.shared.state);
        if state.is_some() {
            return false;
        }
        *state = Some(StorageOutcome::Cancelled);
        self.shared.ready.notify_all();
        true
    }

    /// Publishes the outcome when the read has not produced one yet.
    fn complete(&self, outcome: StorageOutcome) -> bool {
        let mut state = lock(&self.shared.state);
        if state.is_some() {
            return false;
        }
        *state = Some(outcome);
        self.shared.ready.notify_all();
        true
    }
}

/// The pool's configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoragePoolConfig {
    /// The number of worker threads.
    pub workers: usize,
    /// The maximum number of queued jobs.
    pub queue_limit: usize,
    /// The maximum number of bytes the pool may hold for the queued jobs and
    /// the drained results.
    pub memory_limit: u64,
}

impl Default for StoragePoolConfig {
    fn default() -> Self {
        Self {
            workers: default_read_worker_count(),
            queue_limit: 64,
            memory_limit: 64 * 1024 * 1024,
        }
    }
}

/// The worker count the pool uses when nothing else is configured.
///
/// Two cores are left to the frame thread and the host, so the pool never
/// competes with the renderer for them.
fn default_read_worker_count() -> usize {
    std::thread::available_parallelism()
        .map_or(1, |count| count.get().saturating_sub(2))
        .max(1)
}

/// The pool's failures.
#[derive(Debug, thiserror::Error)]
pub enum StoragePoolError {
    /// A worker thread could not be started.
    #[error("could not start a storage read worker: {message}")]
    Worker {
        /// The platform's message.
        message: String,
    },
    /// The configuration is not usable.
    #[error("the storage read pool configuration is invalid: {message}")]
    Config {
        /// What is wrong with the configuration.
        message: String,
    },
    /// The queue is full.
    #[error("the storage read pool queue holds its {limit} jobs")]
    QueueFull {
        /// The configured queue limit.
        limit: usize,
    },
    /// The memory limit would be exceeded.
    #[error("the storage read pool would exceed its memory limit of {limit} bytes")]
    MemoryLimit {
        /// The configured limit.
        limit: u64,
    },
}

/// The read accounting the pool keeps.
///
/// The counters are the number of reads the pool ran and the time they took.
/// A worker updates them under the pool's own lock, so a counter never
/// crosses a thread boundary: the thread that owns the pool reads the totals
/// whenever it wants them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StoragePoolStats {
    /// The number of reads the pool ran.
    pub reads: u64,
    /// The accumulated time the pool spent reading.
    pub elapsed: Duration,
}

/// One queued read.
struct Job {
    /// The handle the result is published into.
    pending: PendingStorage,
    /// The work the worker performs.
    work: StorageJob,
    /// The bytes this job holds against the pool's memory limit.
    charged: u64,
}

/// The queue, the tables and the accounting the workers and the frame thread
/// share.
struct PoolState {
    /// The jobs waiting for a worker.
    queue: VecDeque<Job>,
    /// The bytes the queued jobs hold.
    queued_bytes: u64,
    /// Whether the pool is shutting down.
    stopping: bool,
    /// The number of reads the pool ran.
    reads: u64,
    /// The accumulated time the workers spent reading.
    read_time: Duration,
    /// The reads that are in flight or finished but not yet drained.
    pending: crate::hasher::Map<StorageKey, PendingStorage>,
    /// The order the pending reads were scheduled in.
    pending_order: VecDeque<StorageKey>,
    /// The drained reads, ready to be taken.
    completed: crate::hasher::Map<StorageKey, StorageOutcome>,
    /// The order the drained reads were published in.
    completed_order: VecDeque<StorageKey>,
    /// The bytes the drained reads hold.
    completed_bytes: u64,
}

/// The queue and the wake-up the workers and the frame thread share.
struct PoolShared {
    /// The pool's only lock.
    state: Mutex<PoolState>,
    /// Signalled when a job is queued or the pool stops.
    wake: Condvar,
}

/// The worker pool that reads storage entries off the frame thread.
///
/// The pool owns its worker threads, its queue, its byte accounting and the
/// reads it has in flight. It is middle-layer state: the engine drains it
/// every frame and the compatibility layer schedules reads through the
/// storage it is installed on.
pub struct StorageReadPool {
    /// The queue and the tables the workers and the frame thread share.
    shared: Arc<PoolShared>,
    /// The worker threads, joined when the pool drops.
    workers: Vec<JoinHandle<()>>,
    /// The configuration the pool was started with.
    config: StoragePoolConfig,
}

impl StorageReadPool {
    /// Starts the pool.
    ///
    /// # Errors
    ///
    /// Returns [`StoragePoolError::Config`] when the configuration asks for no
    /// worker or no queue, and [`StoragePoolError::Worker`] when a thread
    /// cannot be started.
    pub fn new(
        reader: Arc<dyn StorageReader>,
        config: StoragePoolConfig,
    ) -> Result<Self, StoragePoolError> {
        if config.workers == 0 {
            return Err(StoragePoolError::Config {
                message: "the pool needs at least one worker".to_string(),
            });
        }
        if config.queue_limit == 0 {
            return Err(StoragePoolError::Config {
                message: "the pool needs a queue of at least one job".to_string(),
            });
        }
        let shared = Arc::new(PoolShared {
            state: Mutex::new(PoolState {
                queue: VecDeque::new(),
                queued_bytes: 0,
                stopping: false,
                reads: 0,
                read_time: Duration::ZERO,
                pending: crate::hasher::map(),
                pending_order: VecDeque::new(),
                completed: crate::hasher::map(),
                completed_order: VecDeque::new(),
                completed_bytes: 0,
            }),
            wake: Condvar::new(),
        });
        let mut workers = Vec::with_capacity(config.workers);
        for index in 0..config.workers {
            let worker_shared = Arc::clone(&shared);
            let worker_reader = Arc::clone(&reader);
            let handle = std::thread::Builder::new()
                .name(format!("krkr-storage-read-{index}"))
                .spawn(move || read_worker_loop(worker_reader, worker_shared));
            match handle {
                Ok(handle) => workers.push(handle),
                Err(error) => {
                    // The workers that already started must not outlive the
                    // pool: they are stopped and joined before the error
                    // goes out.
                    stop_read_workers(&shared, workers);
                    return Err(StoragePoolError::Worker {
                        message: error.to_string(),
                    });
                }
            }
        }
        Ok(Self {
            shared,
            workers,
            config,
        })
    }

    /// Schedules a read and returns the handle its result lands in.
    ///
    /// `size` is the size the lookup named for the entry, and `charge` is the
    /// number of bytes the read is expected to produce: it is what the job is
    /// charged against the memory limit.
    ///
    /// # Errors
    ///
    /// Returns [`StoragePoolError::QueueFull`] when the queue is full and
    /// [`StoragePoolError::MemoryLimit`] when the job would push the pool
    /// past its limit. The caller then reads on its own thread; the pool
    /// never drops a job silently.
    pub fn submit(
        &self,
        key: StorageKey,
        work: StorageJob,
        size: Option<u64>,
        charge: u64,
    ) -> Result<PendingStorage, StoragePoolError> {
        let pending = PendingStorage::new(size);
        {
            let mut state = lock(&self.shared.state);
            if state.stopping {
                return Err(StoragePoolError::Config {
                    message: "the storage read pool is stopping".to_string(),
                });
            }
            if state.queue.len() >= self.config.queue_limit {
                return Err(StoragePoolError::QueueFull {
                    limit: self.config.queue_limit,
                });
            }
            if state.queued_bytes.saturating_add(charge) > self.config.memory_limit {
                return Err(StoragePoolError::MemoryLimit {
                    limit: self.config.memory_limit,
                });
            }
            state.queued_bytes = state.queued_bytes.saturating_add(charge);
            state.pending.insert(key.clone(), pending.clone());
            state.pending_order.push_back(key);
            state.queue.push_back(Job {
                pending: pending.clone(),
                work,
                charged: charge,
            });
        }
        self.shared.wake.notify_one();
        Ok(pending)
    }

    /// The handle of the read `key` names, when one is in flight or finished.
    pub fn pending(&self, key: &StorageKey) -> Option<PendingStorage> {
        lock(&self.shared.state).pending.get(key).cloned()
    }

    /// Takes the handle of the read `key` names out of the pool's table.
    ///
    /// The read keeps running; the caller owns its result. A later read of
    /// the same name schedules a new one, so the storage never serves stale
    /// bytes.
    pub fn take_pending(&self, key: &StorageKey) -> Option<PendingStorage> {
        let mut state = lock(&self.shared.state);
        let pending = state.pending.remove(key)?;
        state.pending_order.retain(|queued| queued != key);
        Some(pending)
    }

    /// The drained result of the read `key` names, when the frame published
    /// it.
    pub fn take_completed(&self, key: &StorageKey) -> Option<StorageOutcome> {
        let mut state = lock(&self.shared.state);
        let outcome = state.completed.remove(key)?;
        state.completed_order.retain(|queued| queued != key);
        state.completed_bytes = state.completed_bytes.saturating_sub(outcome.byte_len());
        Some(outcome)
    }

    /// The drained result of the read `key` names, without taking it.
    pub fn peek_completed(&self, key: &StorageKey) -> Option<StorageOutcome> {
        lock(&self.shared.state).completed.get(key).cloned()
    }

    /// Publishes the finished reads and returns how many were published.
    ///
    /// A finished read is moved from the in-flight table into the drained
    /// table, where the caller that waits for it takes it. The drained table
    /// is bounded by the pool's memory limit: the oldest entries are dropped
    /// when the limit is reached, and a dropped read is simply read again.
    pub fn drain(&self) -> usize {
        let mut state = lock(&self.shared.state);
        let finished: Vec<StorageKey> = state
            .pending_order
            .iter()
            .filter(|key| {
                state
                    .pending
                    .get(*key)
                    .is_some_and(|pending| pending.is_ready())
            })
            .cloned()
            .collect();
        let mut drained = 0;
        for key in finished {
            let Some(pending) = state.pending.remove(&key) else {
                continue;
            };
            state.pending_order.retain(|queued| queued != &key);
            let Some(outcome) = pending.outcome() else {
                continue;
            };
            let bytes = outcome.byte_len();
            state.completed_bytes = state.completed_bytes.saturating_add(bytes);
            state.completed.insert(key.clone(), outcome);
            state.completed_order.push_back(key);
            drained += 1;
        }
        while state.completed_bytes > self.config.memory_limit {
            let Some(key) = state.completed_order.pop_front() else {
                break;
            };
            if let Some(outcome) = state.completed.remove(&key) {
                state.completed_bytes = state.completed_bytes.saturating_sub(outcome.byte_len());
            }
        }
        drained
    }

    /// The number of jobs waiting for a worker.
    pub fn queued(&self) -> usize {
        lock(&self.shared.state).queue.len()
    }

    /// The number of reads that are in flight or finished but not drained.
    pub fn pending_count(&self) -> usize {
        lock(&self.shared.state).pending.len()
    }

    /// The read accounting: how many reads the pool ran and how long they
    /// took.
    ///
    /// The counters cover every read the pool performed, the successful ones
    /// and the failed ones alike.
    pub fn stats(&self) -> StoragePoolStats {
        let state = lock(&self.shared.state);
        StoragePoolStats {
            reads: state.reads,
            elapsed: state.read_time,
        }
    }
}

impl Drop for StorageReadPool {
    fn drop(&mut self) {
        stop_read_workers(&self.shared, std::mem::take(&mut self.workers));
    }
}

/// Stops the pool's workers and joins them.
///
/// The queued reads are cancelled, so a waiter on one of them wakes instead
/// of waiting for a read that will never run. A running read is not
/// preempted: the join waits for it, and it publishes its own result.
fn stop_read_workers(shared: &PoolShared, workers: Vec<JoinHandle<()>>) {
    {
        let mut state = lock(&shared.state);
        state.stopping = true;
        while let Some(job) = state.queue.pop_front() {
            state.queued_bytes = state.queued_bytes.saturating_sub(job.charged);
            job.pending.complete(StorageOutcome::Cancelled);
        }
    }
    shared.wake.notify_all();
    for worker in workers {
        if worker.join().is_err() {
            log::warn!("engine: a storage read worker stopped with a panic");
        }
    }
}

/// Locks a mutex, recovering from a poisoned lock.
///
/// No code path panics while the lock is held, so the recovery cannot expose
/// a half-updated state.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Takes the next job from the queue.
fn take_next_read(state: &mut PoolState) -> Option<Job> {
    state.queue.pop_front()
}

/// Returns a job's bytes to the pool's accounting.
fn release_read_bytes(shared: &PoolShared, charged: u64) {
    let mut state = lock(&shared.state);
    state.queued_bytes = state.queued_bytes.saturating_sub(charged);
}

/// Records the read a worker ran: the count and the time it took.
fn record_read(shared: &PoolShared, elapsed: Duration) {
    let mut state = lock(&shared.state);
    state.reads = state.reads.saturating_add(1);
    state.read_time = state.read_time.saturating_add(elapsed);
}

/// Publishes a failure for a read whose worker stops before it does.
///
/// The guard holds the handle of the read a worker is running. Its drop
/// publishes a failure when the read has not produced a result, so a waiter
/// wakes with a failure instead of waiting for a worker that is gone.
struct ReadGuard {
    /// The handle the result is published into.
    pending: PendingStorage,
}

impl Drop for ReadGuard {
    fn drop(&mut self) {
        self.pending
            .complete(StorageOutcome::Failed(StorageReadError::NoResult {
                message: "the storage read worker stopped before it published a result".to_string(),
            }));
    }
}

/// The message of a panic payload.
///
/// The payload is a `Box<dyn Any + Send>`: a string payload carries the
/// message the panicking code wrote, and anything else is reported as an
/// unnamed panic. The panic is never silenced: it becomes the read's failed
/// outcome, and the waiter sees it.
fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "the storage reader panicked".to_string()
}

/// The worker loop: take a job, read it, then publish the result.
fn read_worker_loop(reader: Arc<dyn StorageReader>, shared: Arc<PoolShared>) {
    loop {
        let job = {
            let mut state = lock(&shared.state);
            loop {
                if state.stopping {
                    return;
                }
                if let Some(job) = take_next_read(&mut state) {
                    break job;
                }
                state = match shared.wake.wait(state) {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
            }
        };
        // The guard covers every path out of this iteration: a worker that
        // stops for any reason leaves the waiter with a result.
        let _guard = ReadGuard {
            pending: job.pending.clone(),
        };
        // A read that was cancelled before it ran is dropped without reading.
        if job.pending.is_ready() {
            release_read_bytes(&shared, job.charged);
            continue;
        }
        let started = Instant::now();
        let outcome =
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| reader.read(&job.work)))
            {
                Ok(Ok(bytes)) => StorageOutcome::Ready(Arc::new(bytes)),
                Ok(Err(error)) => StorageOutcome::Failed(error),
                Err(payload) => StorageOutcome::Failed(StorageReadError::NoResult {
                    message: panic_message(payload.as_ref()),
                }),
            };
        record_read(&shared, started.elapsed());
        release_read_bytes(&shared, job.charged);
        job.pending.complete(outcome);
    }
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
    /// folder. Later archives take precedence over earlier ones, so a folder
    /// project with `--archive data.xp3 --archive patch.xp3` lets `patch.xp3`
    /// override `data.xp3`. An archive project checks its project archive
    /// before the other mounts.
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
    /// and before any startup script runs. A relative path is resolved against
    /// the app directory, the same way [`Self::mount_archive`] resolves it.
    pub fn set_project(&mut self, path: impl AsRef<Path>) {
        let raw = path.as_ref();
        let path = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.game_dir.join(raw)
        };
        let is_archive = self.archives.iter().any(|m| m.path == path)
            || matches!(
                fs::stat(&self.fs, &FsPath::host(&path)),
                Ok(meta) if !meta.is_dir
            );
        self.project = is_archive.then_some(path);
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
            media: RefCell::new(crate::hasher::map()),
            resolution_cache: RefCell::new(ResolutionCache::default()),
            read_pool: None,
            generation: 0,
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
            f(&mounted.path, mounted.archive.as_ref());
        }
    }

    /// Mounts one archive by path, parsing its index eagerly.
    ///
    /// The path is resolved against the storage base directory (absolute
    /// paths are used as given) and opened through the bottom file system.
    /// Mounting the same archive twice is a no-op.
    ///
    /// A preloaded archive's whole image is read in the background: the
    /// mount returns as soon as the index is parsed, and the image is filled
    /// while the game runs.
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
        let archive = Arc::new(Xp3Archive::open(&self.fs, &file)?);
        let backing = if self.preload_archives {
            let size = fs::file_size(&self.fs, &file)?;
            let len = usize::try_from(size).map_err(|_| MountError::ArchiveTooLarge {
                path: path.display().to_string(),
                size,
            })?;
            ArchiveBacking::Image(ArchiveImage::start(&self.fs, &Arc::new(file), &path, len))
        } else {
            ArchiveBacking::Stream(Arc::new(file))
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
        // The resolution changed, so a read that was scheduled against the
        // old one must not be served to a caller that resolves now.
        self.generation = self.generation.saturating_add(1);
    }

    /// Looks a storage name up optimistically.
    ///
    /// The lookup resolves the name through the namespace's own search order
    /// and answers with the entry's existence and its size, without reading
    /// the bytes: the size comes from the mounted archive's index or from the
    /// file's metadata. The read itself is scheduled on the storage's read
    /// pool, and the returned handle carries its result.
    ///
    /// A name that does not resolve is `Ok(None)`; a file-system failure
    /// while probing is a typed error. The bytes of the entry are read
    /// through [`Self::read`] and [`Self::read_prefix`], which wait for the
    /// read this lookup scheduled.
    ///
    /// A storage without a read pool has no worker to schedule on: the lookup
    /// then reads the entry on the calling thread and answers with a handle
    /// that already holds the bytes.
    pub fn lookup(&self, name: &str) -> Result<Option<PendingStorage>, StorageReadError> {
        if let Some(bytes) = self.media.borrow().get(name).cloned() {
            let size = bytes.len() as u64;
            return Ok(Some(PendingStorage::ready(bytes, Some(size))));
        }
        let Some(plan) = self.plan_read(name, None)? else {
            return Ok(None);
        };
        let Some(pool) = self.read_pool.as_ref() else {
            // Without a pool the entry is read on the calling thread.
            let bytes = FileSystemReader.read(&plan.work)?;
            return Ok(Some(PendingStorage::ready(bytes, plan.size)));
        };
        let key = StorageKey::new(name, None, self.generation);
        // A read the frame already published is answered with its result.
        if let Some(outcome) = pool.peek_completed(&key) {
            let bytes = outcome_to_bytes(outcome, name)?;
            return Ok(Some(PendingStorage::ready(bytes, plan.size)));
        }
        // A read that is already in flight is the read this lookup names.
        let pending = match pool.pending(&key) {
            Some(pending) => pending,
            None => {
                let work = plan.work.clone();
                match pool.submit(key, work, plan.size, plan.charge) {
                    Ok(pending) => pending,
                    Err(error) => {
                        // The pool refused the read: the entry is read on the
                        // calling thread, and the refusal is reported.
                        log::warn!(
                            "storage: the read pool refused {name:?} ({error}); the entry is read on the calling thread"
                        );
                        let bytes = FileSystemReader.read(&plan.work)?;
                        return Ok(Some(PendingStorage::ready(bytes, plan.size)));
                    }
                }
            }
        };
        Ok(Some(pending))
    }

    /// Reads a storage entry through the read pool.
    ///
    /// Returns `Ok(None)` when the pool does not serve this read: the
    /// storage has no pool, or the installed extraction filter has to run on
    /// the calling thread because it is script code.
    fn read_scheduled(
        &self,
        name: &str,
        limit: Option<usize>,
    ) -> Result<Option<Vec<u8>>, StorageReadError> {
        let Some(pool) = self.read_pool.as_ref() else {
            return Ok(None);
        };
        // The extraction filter is script code: it runs on the thread that
        // asked for the bytes, never on a worker.
        if self.xp3_extraction_filter.is_some() {
            return Ok(None);
        }
        let key = StorageKey::new(name, limit, self.generation);
        // A read the frame already published is served without another wait.
        if let Some(outcome) = pool.take_completed(&key) {
            return outcome_to_bytes(outcome, name).map(Some);
        }
        let pending = match pool.take_pending(&key) {
            Some(pending) => pending,
            None => {
                let Some(plan) = self.plan_read(name, limit)? else {
                    return Err(StorageReadError::NotFound(name.to_string()));
                };
                match pool.submit(key, plan.work, plan.size, plan.charge) {
                    Ok(pending) => pending,
                    Err(error) => {
                        // The pool refused the read: the entry is read on the
                        // calling thread, and the refusal is reported.
                        log::warn!(
                            "storage: the read pool refused {name:?} ({error}); the entry is read on the calling thread"
                        );
                        return Ok(None);
                    }
                }
            }
        };
        let outcome = pending.await_outcome();
        outcome_to_bytes(outcome, name).map(Some)
    }

    /// Resolves a storage name into the work a read performs.
    ///
    /// `limit` is the number of bytes the read wants, or `None` for the whole
    /// entry. A name that does not resolve is `Ok(None)`.
    fn plan_read(
        &self,
        name: &str,
        limit: Option<usize>,
    ) -> Result<Option<ReadPlan>, StorageReadError> {
        let Some(location) = self.find(name)? else {
            return Ok(None);
        };
        Ok(Some(self.build_work(&location, limit)?))
    }

    /// Builds the work a read performs and the sizes the resolution named.
    ///
    /// The size of the entry comes from the mounted archive's index or from
    /// the file's metadata. An archive that is not mounted is opened by the
    /// worker, so its entry size is unknown before the read; the archive's
    /// own size bounds the read for the pool's accounting.
    fn build_work(
        &self,
        location: &Location,
        limit: Option<usize>,
    ) -> Result<ReadPlan, StorageReadError> {
        match location {
            Location::Disk(path) => {
                let size = match fs::stat(&self.fs, &FsPath::host(path)) {
                    Ok(meta) => meta.size,
                    Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => 0,
                    Err(error) => return Err(StorageReadError::Fs(error)),
                };
                Ok(ReadPlan {
                    work: StorageJob::Disk {
                        fs: self.fs.clone(),
                        path: path.clone(),
                        limit,
                    },
                    size: Some(size),
                    charge: size,
                })
            }
            Location::Archive(arc_path, in_arc) => {
                let source = self
                    .archives
                    .iter()
                    .find(|mounted| mounted.path.as_path() == arc_path)
                    .map(|mounted| ArchiveSource {
                        archive: Arc::clone(&mounted.archive),
                        backing: mounted.backing.clone(),
                    });
                let size = match &source {
                    Some(source) => source.archive.entry(in_arc).map(|entry| entry.org_size),
                    None => None,
                };
                let charge = match &source {
                    Some(_) => size.unwrap_or(0),
                    None => match fs::stat(&self.fs, &FsPath::host(arc_path)) {
                        Ok(meta) => meta.size,
                        Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => 0,
                        Err(error) => return Err(StorageReadError::Fs(error)),
                    },
                };
                let archive_name = arc_path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                Ok(ReadPlan {
                    work: StorageJob::Archive {
                        fs: self.fs.clone(),
                        source,
                        path: arc_path.clone(),
                        in_arc: in_arc.clone(),
                        archive_name,
                        limit,
                    },
                    size,
                    charge,
                })
            }
        }
    }

    /// Resolves a storage name to a location, mirroring the reference search
    /// order: explicit `arc.xp3>path` → the current project storage → the
    /// auto search paths in "later added wins" order → the remaining mounted
    /// archives, later mounts first → the app directory as the loose-file
    /// fallback for an archive project. The project storage is the project
    /// archive for an archive project and the app directory for a folder
    /// project. Auto-path and explicit archives are opened on demand when they
    /// exist on disk but were not mounted.
    ///
    /// A name that simply does not resolve is `Ok(None)`; a `..` that escapes
    /// the mount is [`StorageReadError::InvalidPathName`]. A genuine
    /// file-system failure while probing (permission denied, I/O, an
    /// unrepresentable path) is propagated as [`StorageReadError::Fs`]; neither
    /// is collapsed into "not found".
    ///
    /// A miss logs one bounded `debug` line and, when trace logging is on, the
    /// full location list at `trace`. The `debug` line names the searched
    /// locations (the project archive, auto-path candidates grouped by
    /// resolution shape, mounted archives in later-first order, disk
    /// spellings, or the explicit `arc.xp3>path` interpretation) until a small
    /// character budget is reached, then appends an elision marker with the
    /// number of dropped characters; the `trace` line carries the complete
    /// list, including the archive near-miss names. A hit logs nothing, and
    /// the trace is only built when debug logging is enabled; the near-miss
    /// scan is added only at `trace`.
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

        let normalized = normalize_in_archive_name(&relative);
        let base = storage_base(&normalized);
        let project = self.project.as_deref();

        // The current project storage is checked first. An archive project's
        // current directory is its project archive, with the archive
        // delimiter appended; a folder project's current directory is the app
        // directory.
        match project {
            Some(project) => {
                if let Some(location) = self.find_in_project_archive(project, &normalized, trace)? {
                    return Ok(Some(location));
                }
            }
            None => {
                if let Some(location) = self.find_disk_relative(&relative, trace)? {
                    return Ok(Some(location));
                }
            }
        }

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

        // The remaining mounted archives, later mounts first (later-mounted
        // wins — see `mount_archives`). The project archive was already
        // checked as the current project storage.
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
            if project.is_some_and(|project| project == mounted.path.as_path()) {
                continue;
            }
            if mounted.archive.entry(&normalized).is_some() {
                return Ok(Some(Location::Archive(mounted.path.clone(), normalized)));
            }
            trace.attempt_detailed(|detailed| {
                archive_miss_line(&mounted.path, &mounted.archive, &normalized, detailed)
            });
        }

        // An archive project still falls back to a loose app-directory file;
        // a folder project already checked the app directory first.
        if project.is_some()
            && let Some(location) = self.find_disk_relative(&relative, trace)?
        {
            return Ok(Some(location));
        }
        Ok(None)
    }

    /// Resolves an in-archive name in the project archive itself, opening the
    /// archive on demand when the caller named a file that is not mounted.
    fn find_in_project_archive(
        &self,
        project: &Path,
        normalized: &str,
        trace: &mut SearchTrace,
    ) -> Result<Option<Location>, StorageReadError> {
        if let Some(mounted) = self.archives.iter().find(|m| m.path.as_path() == project) {
            trace.attempt(|| format!("project archive {}", mounted.path.display()));
            if mounted.archive.entry(normalized).is_some() {
                return Ok(Some(Location::Archive(
                    mounted.path.clone(),
                    normalized.to_string(),
                )));
            }
            trace.attempt_detailed(|detailed| {
                archive_miss_line(&mounted.path, &mounted.archive, normalized, detailed)
            });
            return Ok(None);
        }
        let file = match fs::open(&self.fs, &FsPath::host(project), OpenMode::Read) {
            Ok(file) => file,
            Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => {
                trace.attempt(|| format!("project archive {} is not on disk", project.display()));
                return Ok(None);
            }
            Err(error) => return Err(StorageReadError::Fs(error)),
        };
        let archive = Xp3Archive::open(&self.fs, &file).map_err(xp3_error)?;
        trace.attempt(|| format!("project archive {} (opened on demand)", project.display()));
        if archive.entry(normalized).is_some() {
            Ok(Some(Location::Archive(
                project.to_path_buf(),
                normalized.to_string(),
            )))
        } else {
            trace.attempt_detailed(|detailed| {
                archive_miss_line(project, &archive, normalized, detailed)
            });
            Ok(None)
        }
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
        let mut exact = crate::hasher::map();
        let mut first_file = crate::hasher::map();
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
                entries: crate::hasher::map(),
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
                let mut entries = crate::hasher::map();
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
        let archive = Xp3Archive::open(&self.fs, &file).map_err(xp3_error)?;
        let mut entries = crate::hasher::map();
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
        let archive = Xp3Archive::open(&self.fs, &file).map_err(xp3_error)?;
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
        raw_relative_disk_name(&self.game_dir, name)
    }

    /// [`Self::relative_disk_name`] with a `..` that escapes the mount
    /// reported as [`StorageReadError::InvalidPathName`] instead of `None`.
    fn relative_disk_name_checked(&self, name: &str) -> Result<Option<String>, StorageReadError> {
        relative_disk_name_checked(&self.game_dir, name)
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
        local_name_in(&self.fs, &self.game_dir, name)
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
        let archive = Xp3Archive::open(&self.fs, &file).map_err(xp3_error)?;
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
    /// later registration for the same name replaces the earlier bytes.
    pub fn register_media(&self, name: String, bytes: Vec<u8>) {
        self.media.borrow_mut().insert(name, bytes);
    }

    /// Reads a storage entry to bytes (disk file or archive member).
    pub fn read(&self, name: &str) -> Result<Vec<u8>, StorageReadError> {
        if let Some(bytes) = self.media.borrow().get(name).cloned() {
            self.count_read_bytes(bytes.len());
            return Ok(bytes);
        }
        if let Some(bytes) = self.read_scheduled(name, None)? {
            self.count_read_bytes(bytes.len());
            return Ok(bytes);
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
        if let Some(mut bytes) = self.media.borrow().get(name).cloned() {
            bytes.truncate(max_len);
            self.count_read_bytes(bytes.len());
            return Ok(bytes);
        }
        if let Some(bytes) = self.read_scheduled(name, Some(max_len))? {
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
                    .map_err(xp3_error),
                ArchiveBacking::Image(image) => {
                    image.read_entry(&mounted.archive, entry, &mounted.file_name, None, filter)
                }
            };
        }
        let file = fs::open(&self.fs, &FsPath::host(arc_path), OpenMode::Read)?;
        let archive = Xp3Archive::open(&self.fs, &file).map_err(xp3_error)?;
        let file_name = arc_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        archive
            .read_with_filter(&self.fs, &file, in_arc, &file_name, filter)
            .map_err(xp3_error)
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
                    .map_err(xp3_error),
                ArchiveBacking::Image(image) => {
                    let entry = mounted
                        .archive
                        .entry(in_arc)
                        .ok_or_else(|| StorageReadError::NotFound(in_arc.to_string()))?;
                    image.read_entry(
                        &mounted.archive,
                        entry,
                        &mounted.file_name,
                        Some(max_len),
                        filter,
                    )
                }
            };
        }
        let file = fs::open(&self.fs, &FsPath::host(arc_path), OpenMode::Read)?;
        let archive = Xp3Archive::open(&self.fs, &file).map_err(xp3_error)?;
        let file_name = arc_path
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default();
        archive
            .read_prefix_with_filter(&self.fs, &file, in_arc, &file_name, max_len, filter)
            .map_err(xp3_error)
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
    /// and deduplicated. This is an internal enumeration helper; no reference
    /// member is bound to it.
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

    /// Lists storage names matching a wildcard mask, merged across the disk
    /// root and every mounted archive. This is an internal enumeration helper;
    /// no reference member is bound to it.
    ///
    /// * The mask is normalized (lowercased, `\` → `/`).
    /// * If it contains `>` it addresses archives: `arcmask>inarcmask`, where
    ///   `arcmask` wildcard-matches the archive file name.
    /// * The part before the last `/` is a required literal directory prefix;
    ///   the part after is a `*`/`?` wildcard matched case-insensitively
    ///   against the base name. Matching is **recursive under the prefix**: a
    ///   mask with no `>` matches base names at any depth below the prefix, and
    ///   a mask with no `/` matches at any depth in the whole tree.
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

/// The mount-relative part of a storage `name`, or `None` when it is an
/// absolute path outside `game_dir`.
fn raw_relative_disk_name(game_dir: &Path, name: &str) -> Option<String> {
    // Fold Windows separators so a `\`-separated storage name behaves like
    // the reference on any host.
    let separators = name.replace('\\', "/");
    let path = Path::new(&separators);
    let relative = if path.is_absolute() {
        if game_dir.as_os_str().is_empty() {
            path.to_path_buf()
        } else {
            strip_prefix_ignore_ascii_case(path, game_dir)?
        }
    } else {
        path.to_path_buf()
    };
    Some(relative.to_string_lossy().replace('\\', "/"))
}

/// [`raw_relative_disk_name`] with a `..` that escapes the mount reported as
/// [`StorageReadError::InvalidPathName`] instead of `None`.
fn relative_disk_name_checked(
    game_dir: &Path,
    name: &str,
) -> Result<Option<String>, StorageReadError> {
    let Some(relative) = raw_relative_disk_name(game_dir, name) else {
        return Ok(None);
    };
    match fold_dot_components(&relative) {
        Some(folded) => Ok(Some(folded)),
        None => Err(StorageReadError::InvalidPathName(name.to_string())),
    }
}

/// The case-corrected host path for a storage `name` below `game_dir` — the
/// same resolution as [`Storage::local_name`], usable where only the
/// stateless file-system handle and the base directory are available.
///
/// This is the writer-facing form: a `name` that is an absolute, fully
/// lowercased form of a mixed-case on-disk path (the shape
/// `System.dataPath` produces) resolves to the real spelling instead of
/// creating a second lowercase tree.
pub fn local_name_in(
    fs: &FileSystem,
    game_dir: &Path,
    name: &str,
) -> Result<Option<PathBuf>, StorageReadError> {
    if name.contains('>') {
        return Ok(None);
    }
    let Some(relative) = relative_disk_name_checked(game_dir, name)? else {
        return Ok(None);
    };
    Ok(Some(case_correct_path(fs, game_dir, &relative)?))
}

/// Walks `relative` (already folded and `/`-separated) below `game_dir`,
/// replacing each existing component with its real on-disk spelling. From the
/// first component that does not exist, the requested spelling of every
/// remaining component is appended unchanged.
fn case_correct_path(
    fs: &FileSystem,
    game_dir: &Path,
    relative: &str,
) -> Result<PathBuf, StorageReadError> {
    let mut host = game_dir.to_path_buf();
    let mut parts = relative.split('/').filter(|part| !part.is_empty());
    while let Some(part) = parts.next() {
        match case_correct_component(fs, &host, part)? {
            Some(real) => host.push(real),
            None => {
                // The requested spelling of this component and every
                // remaining one is appended, then correcting stops
                // (`reference/cpp/core/base/impl/StorageImpl.cpp:373-377`).
                host.push(part);
                for rest in parts {
                    host.push(rest);
                }
                break;
            }
        }
    }
    Ok(host)
}

/// The real on-disk spelling of the child of `dir` named `want`, or `None`
/// when no child matches. A directory-list failure other than a missing
/// directory is reported, never treated as a miss.
fn case_correct_component(
    fs: &FileSystem,
    dir: &Path,
    want: &str,
) -> Result<Option<String>, StorageReadError> {
    let entries = match fs::list(fs, &FsPath::host(dir)) {
        Ok(entries) => entries,
        Err(FsError::NotFound { .. }) | Err(FsError::NotADirectory { .. }) => return Ok(None),
        Err(error) => return Err(StorageReadError::Fs(error)),
    };
    Ok(entries
        .into_iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(want))
        .map(|entry| entry.name))
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
    out: &mut crate::hasher::Map<String, String>,
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
/// The size the mount read is authoritative: a file that ends before it is a
/// typed error, never a shortened buffer. There is no size cap; a buffer the
/// platform cannot allocate is a typed error instead. The returned image is
/// owned by the archive's backing and handed to L1 for each entry read; L1
/// never keeps it.
fn load_archive_image(
    fs: &FileSystem,
    file: &FileState,
    path: &Path,
    len: usize,
) -> Result<Arc<[u8]>, PreloadError> {
    let display = path.display().to_string();
    let size = len as u64;
    let mut image = Vec::new();
    image
        .try_reserve_exact(len)
        .map_err(|_| PreloadError::Allocation {
            path: display.clone(),
            size,
        })?;
    image.resize(len, 0);
    let mut read = 0usize;
    while read < len {
        let n = fs::read_at(fs, file, read as u64, &mut image[read..]).map_err(|error| {
            PreloadError::Io {
                path: display.clone(),
                message: error.to_string(),
            }
        })?;
        if n == 0 {
            return Err(PreloadError::Truncated {
                path: display,
                expected: size,
                read: read as u64,
            });
        }
        read += n;
    }
    Ok(Arc::from(image))
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

    /// An entry read that arrives before the image is loaded reads the entry
    /// from the archive file, and the same read copies from the image once
    /// the load has published it. The bytes are the same either way.
    #[test]
    fn an_entry_read_uses_the_image_once_it_is_loaded() {
        let xp3 = testkit::build_xp3(&[("data/a.bin", b"payload", false)]);
        let fs = FileSystem::memory();
        write_mem(&fs, "/game/data.xp3", &xp3);
        let file = fs::open(&fs, &FsPath::host("/game/data.xp3"), OpenMode::Read).expect("open");
        let archive = Xp3Archive::open(&fs, &file).expect("parse archive");
        let entry = archive.entry("data/a.bin").expect("entry");
        let image = ArchiveImage::pending(&fs, &Arc::new(file));

        assert!(
            image.loaded().expect("the image state").is_none(),
            "the image is not loaded yet"
        );
        let from_file = image
            .read_entry(&archive, entry, "data.xp3", None, None)
            .expect("read from the file");
        assert_eq!(from_file, b"payload");

        image.publish(
            Path::new("/game/data.xp3"),
            Ok(Arc::from(xp3)),
            Duration::ZERO,
        );
        assert!(
            image.loaded().expect("the image state").is_some(),
            "the image is loaded"
        );
        let from_image = image
            .read_entry(&archive, entry, "data.xp3", None, None)
            .expect("read from the image");
        assert_eq!(from_image, from_file);
    }

    /// A failed image load is a typed read error: the archive never falls
    /// back to serving the entry from the file after the load failed.
    #[test]
    fn a_failed_image_load_is_a_typed_read_error() {
        let xp3 = testkit::build_xp3(&[("data/a.bin", b"payload", false)]);
        let fs = FileSystem::memory();
        write_mem(&fs, "/game/data.xp3", &xp3);
        let file = fs::open(&fs, &FsPath::host("/game/data.xp3"), OpenMode::Read).expect("open");
        let archive = Xp3Archive::open(&fs, &file).expect("parse archive");
        let entry = archive.entry("data/a.bin").expect("entry");
        let image = ArchiveImage::pending(&fs, &Arc::new(file));

        image.publish(
            Path::new("/game/data.xp3"),
            Err(PreloadError::Allocation {
                path: "/game/data.xp3".to_owned(),
                size: 4096,
            }),
            Duration::ZERO,
        );
        let error = image
            .read_entry(&archive, entry, "data.xp3", None, None)
            .expect_err("the failed image is an error");
        assert!(
            matches!(error, StorageReadError::Preload(_)),
            "unexpected error: {error:?}"
        );
    }

    /// The mount does not wait for a preloaded archive's image: the mount
    /// returns while the image is still being read, and the image lands in
    /// the archive afterwards.
    ///
    /// The fixture carries megabytes of payload, so the image load takes far
    /// longer than the index parse the mount does.
    #[test]
    fn the_mount_does_not_wait_for_the_archive_image() {
        let payload = vec![b'x'; 16 << 20];
        let xp3 = testkit::build_xp3(&[("data/big.bin", &payload, false)]);
        let fs = FileSystem::memory();
        write_mem(&fs, "/game/data.xp3", &xp3);
        let storage = Storage::mount_archives_with(
            fs,
            "/game",
            ["data.xp3"],
            MountOptions {
                preload_archives: true,
            },
        )
        .expect("mount");

        let mounted = &storage.archives[0];
        let ArchiveBacking::Image(image) = &mounted.backing else {
            panic!("a preloaded archive is backed by its image");
        };
        assert!(
            image.loaded().expect("the image state").is_none(),
            "the mount waited for the archive's image"
        );

        let mut loaded = false;
        for _ in 0..10_000 {
            if image.loaded().expect("the image state").is_some() {
                loaded = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        assert!(loaded, "the archive image is read in the background");
    }
}
