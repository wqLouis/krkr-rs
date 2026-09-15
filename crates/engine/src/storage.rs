//! Game storage: mount a game directory and its `.xp3` archives, and resolve
//! storage names the way the reference engine does.

use std::cell::{Ref, RefCell};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use xp3::{Xp3Archive, normalize_in_archive_name};

/// Process-wide auto search paths (the reference's `TVPAutoPathTable`).
/// Entries are storage-name prefixes: plain dirs (`system/`) or archive
/// prefixes (`data.xp3>system/`). The game populates them at startup via
/// `Storages.addAutoPath`.
static AUTO_PATHS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Replace the auto-path list (used at startup).
pub fn set_auto_paths(paths: Vec<String>) {
    *AUTO_PATHS.lock().unwrap_or_else(|p| p.into_inner()) = paths;
}

/// Append one auto path (the `Storages.addAutoPath` native).
pub fn add_auto_path(path: String) {
    let mut list = AUTO_PATHS.lock().unwrap_or_else(|p| p.into_inner());
    if !list.contains(&path) {
        list.push(path);
    }
}

/// Remove one auto path (the `Storages.removeAutoPath` native).
pub fn remove_auto_path(path: &str) {
    let mut list = AUTO_PATHS.lock().unwrap_or_else(|p| p.into_inner());
    list.retain(|p| p != path);
}

/// Current auto-path list.
pub fn auto_paths() -> Vec<String> {
    AUTO_PATHS.lock().unwrap_or_else(|p| p.into_inner()).clone()
}

/// The base storage name (the part after the last `/`).
fn storage_base(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// Find a disk file by normalized relative name, comparing path components
/// case-insensitively (ASCII) and descending into a directory only when the
/// target's next component matches its name. Cost is therefore bounded by the
/// path depth rather than by the size of the tree, and no `str` is ever
/// sliced at a non-`char` boundary (components are split on `/`).
fn find_disk_case_insensitive(base: &Path, relative: &str) -> Option<PathBuf> {
    fn descend(dir: &Path, parts: &[&str]) -> Option<PathBuf> {
        let want = parts[0];
        // Directories whose name matches this component and that still have
        // remaining target components to match are searched after the direct
        // file hit, so a file directly at this level wins.
        let mut subdirs = Vec::new();
        for entry in fs::read_dir(dir).ok()?.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.eq_ignore_ascii_case(want) {
                continue;
            }
            let path = entry.path();
            if parts.len() == 1 {
                if path.is_file() {
                    return Some(path);
                }
            } else if path.is_dir() {
                subdirs.push(path);
            }
        }
        for subdir in subdirs {
            if let Some(found) = descend(&subdir, &parts[1..]) {
                return Some(found);
            }
        }
        None
    }

    let parts: Vec<&str> = relative.split('/').filter(|p| !p.is_empty()).collect();
    if parts.is_empty() {
        return None;
    }
    descend(base, &parts)
}

/// One mounted `.xp3` archive. The path is fixed at mount time; the parsed
/// [`Xp3Archive`] is opened on first use and dropped by
/// [`Storage::clear_archive_cache`], so a clear costs no archive I/O. The
/// next lookup re-opens only the archives it actually has to scan.
struct MountedArchive {
    path: PathBuf,
    /// Parsed archive, or `None` before the first use / after a cache clear.
    archive: RefCell<Option<Xp3Archive>>,
}

impl MountedArchive {
    fn new(path: PathBuf) -> Self {
        Self {
            path,
            archive: RefCell::new(None),
        }
    }

    /// Ensure the archive is parsed, opening it on demand. Kept separate from
    /// the borrow-returning accessors so no `Ref` is ever held across the
    /// mutation that populates the cell.
    fn ensure_open(&self) -> Result<(), xp3::Error> {
        if self.archive.borrow().is_some() {
            return Ok(());
        }
        match Xp3Archive::open(&self.path) {
            Ok(arc) => {
                log::info!("opened {} ({} entries)", self.path.display(), arc.len());
                *self.archive.borrow_mut() = Some(arc);
                Ok(())
            }
            Err(e) => {
                log::warn!("skipping {}: {e}", self.path.display());
                Err(e)
            }
        }
    }

    /// The parsed archive, opening it on demand. `None` when the file is not
    /// a readable XP3 archive.
    fn get(&self) -> Option<Ref<'_, Xp3Archive>> {
        self.ensure_open().ok()?;
        Some(Ref::map(self.archive.borrow(), |a| {
            a.as_ref().expect("archive opened by ensure_open")
        }))
    }

    /// Mutable access for reading, opening on demand.
    fn get_mut(&mut self) -> Result<&mut Xp3Archive, xp3::Error> {
        self.ensure_open()?;
        Ok(self
            .archive
            .get_mut()
            .as_mut()
            .expect("archive opened by ensure_open"))
    }

    /// Drop the parsed archive, keeping the mount (and its path).
    fn drop_cache(&self) {
        *self.archive.borrow_mut() = None;
    }
}

/// A mounted game storage: one game directory plus all `.xp3` archives
/// found inside it.
pub struct Storage {
    /// Absolute path of the game directory.
    pub game_dir: PathBuf,
    /// Mounted archives in sorted-name order (deterministic). Each archive is
    /// parsed lazily on first use.
    archives: Vec<MountedArchive>,
}

/// A resolved storage location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// A file on disk (absolute path).
    Disk(PathBuf),
    /// A file inside an archive: (archive path, normalized in-archive name).
    Archive(PathBuf, String),
}

/// Metadata returned for a resolved storage entry.
///
/// XP3 indexes carry the uncompressed file size but do not carry filesystem
/// timestamps, so archive entries have `None` for all three time fields.
/// Disk entries use the host filesystem metadata and preserve the native
/// timestamp resolution exposed by [`std::fs::Metadata`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageMetadata {
    pub size: u64,
    pub modified: Option<SystemTime>,
    pub accessed: Option<SystemTime>,
    pub created: Option<SystemTime>,
}

impl Storage {
    /// Mount a game directory: opens every `*.xp3` in it (top level only,
    /// matching how the reference app scans the game folder).
    pub fn mount(game_dir: impl AsRef<Path>) -> Result<Self, MountError> {
        let game_dir = game_dir.as_ref().to_path_buf();
        if !game_dir.is_dir() {
            return Err(MountError::NotADirectory(game_dir));
        }
        let mut archives = Vec::new();
        for entry in fs::read_dir(&game_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path
                .extension()
                .map(|e| e.eq_ignore_ascii_case("xp3"))
                .unwrap_or(false)
            {
                // Record the mount only; parsing happens on first use.
                archives.push(MountedArchive::new(path));
            }
        }
        archives.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(Storage { game_dir, archives })
    }

    /// Game directory path.
    pub fn game_dir(&self) -> &Path {
        &self.game_dir
    }

    /// Number of mounted archives, whether or not they have been parsed yet.
    pub fn archive_count(&self) -> usize {
        self.archives.len()
    }

    /// Run `f` for every mounted archive, opening each on demand. Archives
    /// that cannot be opened are skipped (a warning is logged by the opener).
    ///
    /// This replaces the old `archives()` iterator: because a parsed archive
    /// lives behind a `RefCell`, no caller may hold a borrow across a
    /// (lazy-open) mutation, so the borrow is confined to the closure.
    pub fn for_each_archive(&self, mut f: impl FnMut(&Path, &Xp3Archive)) {
        for mounted in &self.archives {
            if let Some(archive) = mounted.get() {
                f(&mounted.path, &archive);
            }
        }
    }

    /// Drop every parsed archive, keeping the mount list and `game_dir`. This
    /// is the reference `TVPClearArchiveCache()`: the cache of *open archive
    /// objects* is cleared (`StorageIntf.cpp:728`), and the next access
    /// re-opens on demand through `TVPArchiveCache::Get` → `TVPOpenArchive`.
    /// The mount/auto-path table is not touched.
    pub fn clear_archive_cache(&self) {
        for mounted in &self.archives {
            mounted.drop_cache();
        }
    }

    /// Resolve a storage name to a location, mirroring the reference search
    /// order: explicit `arc.xp3>path` → disk file (case-insensitive) → first
    /// archive containing the normalized name → auto paths.
    ///
    /// Storage names are case-insensitive in the reference engine even when
    /// the host filesystem is not. Only the *storage-relative* part is
    /// normalized; the mount prefix keeps the real filesystem spelling (and a
    /// name that escapes the mount never resolves).
    pub fn find(&self, name: &str) -> Option<Location> {
        // Explicit archive addressing: "foo.xp3>data/script.ks"
        if let Some((arc, rest)) = name.split_once('>') {
            // The archive part may be a bare name ("data.xp3") or a full
            // path ("/game/dir/data.xp3"); match on its file name.
            let arc_name = Path::new(arc)
                .file_name()
                .map(|f| f.to_string_lossy())
                .unwrap_or_default();
            let mounted = self.archives.iter().find(|m| {
                m.path
                    .file_name()
                    .is_some_and(|f| f.eq_ignore_ascii_case(&*arc_name))
            })?;
            let rest = normalize_in_archive_name(rest);
            if mounted.get()?.entry(&rest).is_some() {
                return Some(Location::Archive(mounted.path.clone(), rest));
            }
            return None;
        }

        // Resolve the storage-relative part. Absolute names must point inside
        // the mount, and traversal components are refused, so a name can
        // never resolve outside the game directory.
        let relative = self.relative_disk_name(name)?;

        // Disk file (exact normalized spelling first, then the script's
        // spelling, then a bounded case-insensitive walk).
        if let Some(location) = self.find_disk_relative(&relative) {
            return Some(location);
        }

        // Archives, in mount order (looked up by the storage-relative name).
        let normalized = normalize_in_archive_name(&relative);
        for mounted in &self.archives {
            // Resolve per archive and stop at the first hit, so a lookup after
            // a cache clear parses at most the archives it must walk. An
            // archive that fails to open is skipped, not fatal.
            let Some(archive) = mounted.get() else {
                continue;
            };
            if archive.entry(&normalized).is_some() {
                return Some(Location::Archive(mounted.path.clone(), normalized));
            }
        }

        // Auto paths: the base name joined with each registered prefix
        // (last added wins, like the reference's hash-table Add). Archive
        // prefixes recurse into `find`; plain dirs hit the disk.
        let base = storage_base(&normalized);
        for entry in auto_paths().iter().rev() {
            let joined = format!("{entry}{base}");
            if entry.contains('>') {
                if let Some(loc) = self.find(&joined) {
                    return Some(loc);
                }
            } else {
                let dir = entry.trim_end_matches('/');
                if !dir.is_empty() && self.game_dir.join(dir).join(base).is_file() {
                    return Some(Location::Disk(self.game_dir.join(dir).join(base)));
                }
            }
        }
        None
    }

    /// The storage-relative part of `name` (using `/` separators), or `None`
    /// when the name is absolute and does not point inside the mount, or
    /// contains a traversal (`..`) component.
    pub fn relative_disk_name(&self, name: &str) -> Option<String> {
        // Fold Windows separators so a `\`-separated storage name behaves
        // like the reference on any host. The mount prefix itself contains no
        // backslashes, so this cannot corrupt it.
        let separators = name.replace('\\', "/");
        let path = Path::new(&separators);
        let relative = if path.is_absolute() {
            path.strip_prefix(&self.game_dir).ok()?.to_path_buf()
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

    /// Resolve an existing disk file for a storage name, case-insensitively.
    /// Returns the real on-disk path so writers can update the existing file
    /// instead of creating a case-variant duplicate.
    pub fn find_disk(&self, name: &str) -> Option<PathBuf> {
        let relative = self.relative_disk_name(name)?;
        match self.find_disk_relative(&relative)? {
            Location::Disk(path) => Some(path),
            Location::Archive(..) => None,
        }
    }

    /// Disk branch of [`Self::find`], given an already-resolved relative name.
    fn find_disk_relative(&self, relative: &str) -> Option<Location> {
        if relative.is_empty() {
            return None;
        }
        // Reference semantics: storage names are normalized to lowercase
        // before lookup, so the lowercase spelling wins when both a lowercase
        // and a mixed-case file exist.
        let normalized = normalize_in_archive_name(relative);
        let candidate = self.game_dir.join(&normalized);
        if candidate.is_file() {
            return Some(Location::Disk(candidate));
        }
        // Fast path: the exact spelling the script used.
        if normalized != relative {
            let exact = self.game_dir.join(relative);
            if exact.is_file() {
                return Some(Location::Disk(exact));
            }
        }
        // Bounded, component-wise case-insensitive walk.
        find_disk_case_insensitive(&self.game_dir, &normalized).map(Location::Disk)
    }

    /// Read a storage entry to bytes (disk file or archive member).
    pub fn read(&mut self, name: &str) -> Result<Vec<u8>, ReadError> {
        match self.find(name) {
            Some(Location::Disk(path)) => Ok(fs::read(path)?),
            Some(Location::Archive(arc_path, in_arc)) => {
                let mounted = self
                    .archives
                    .iter_mut()
                    .find(|m| m.path == arc_path)
                    .expect("resolved archive must be mounted");
                let arc = mounted.get_mut()?;
                arc.read(&in_arc).map_err(ReadError::Xp3)
            }
            None => Err(ReadError::NotFound(name.to_string())),
        }
    }

    /// Return metadata for a resolved storage entry.
    ///
    /// Disk files are read through [`std::fs::metadata`]. For an XP3 entry,
    /// `size` is the uncompressed size from the archive index; archive
    /// timestamps are intentionally absent because XP3 has no timestamp
    /// fields. Names follow the same resolution order as [`Self::find`].
    pub fn stat(&self, name: &str) -> Option<StorageMetadata> {
        match self.find(name) {
            Some(Location::Disk(path)) => {
                let metadata = fs::metadata(path).ok()?;
                Some(StorageMetadata {
                    size: metadata.len(),
                    modified: metadata.modified().ok(),
                    accessed: metadata.accessed().ok(),
                    created: metadata.created().ok(),
                })
            }
            Some(Location::Archive(archive_path, in_archive)) => {
                let mounted = self.archives.iter().find(|m| m.path == archive_path)?;
                let archive = mounted.get()?;
                let entry = archive.entry(&in_archive)?;
                Some(StorageMetadata {
                    size: entry.org_size,
                    modified: None,
                    accessed: None,
                    created: None,
                })
            }
            None => None,
        }
    }

    /// True if a storage name resolves.
    pub fn exists(&self, name: &str) -> bool {
        self.find(name).is_some()
    }

    /// Number of archives whose parsed form is currently cached. Test-only:
    /// lets the storage tests observe the lazy open/clear behavior directly.
    #[cfg(test)]
    fn parsed_archive_count(&self) -> usize {
        self.archives
            .iter()
            .filter(|m| m.archive.borrow().is_some())
            .count()
    }
}

/// Errors while mounting a game storage.
#[derive(Debug, thiserror::Error)]
pub enum MountError {
    #[error("not a directory: {0}")]
    NotADirectory(PathBuf),
    #[error("cannot read game directory: {0}")]
    Io(#[from] std::io::Error),
}

/// Errors while reading a storage entry.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("storage not found: {0}")]
    NotFound(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("archive error: {0}")]
    Xp3(#[from] xp3::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal raw-index XP3 archive (no compression) for the lazy
    /// cache tests. Layout matches what [`Xp3Archive::open`] expects.
    fn build_xp3(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut file = Vec::new();
        file.extend_from_slice(&xp3::XP3_MAGIC);
        file.extend_from_slice(&0u64.to_le_bytes()); // index-offset slot

        let mut file_chunks: Vec<Vec<u8>> = Vec::new();
        for (name, data) in entries {
            let start = file.len() as u64;
            file.extend_from_slice(data);

            let units: Vec<u16> = name.encode_utf16().collect();
            let mut info = Vec::new();
            info.extend_from_slice(b"info");
            info.extend_from_slice(&((22 + units.len() * 2) as u64).to_le_bytes());
            info.extend_from_slice(&0u32.to_le_bytes()); // flags
            info.extend_from_slice(&(data.len() as i64).to_le_bytes());
            info.extend_from_slice(&(data.len() as i64).to_le_bytes());
            info.extend_from_slice(&(units.len() as i16).to_le_bytes());
            for u in &units {
                info.extend_from_slice(&u.to_le_bytes());
            }

            let mut segm = Vec::new();
            segm.extend_from_slice(b"segm");
            segm.extend_from_slice(&28u64.to_le_bytes());
            segm.extend_from_slice(&0u32.to_le_bytes()); // raw segment
            segm.extend_from_slice(&(start as i64).to_le_bytes());
            segm.extend_from_slice(&(data.len() as i64).to_le_bytes());
            segm.extend_from_slice(&(data.len() as i64).to_le_bytes());

            let mut aldr = Vec::new();
            aldr.extend_from_slice(b"aldr");
            aldr.extend_from_slice(&4u64.to_le_bytes());
            aldr.extend_from_slice(&0u32.to_le_bytes());

            let mut fc = Vec::new();
            fc.extend_from_slice(b"File");
            fc.extend_from_slice(&((info.len() + segm.len() + aldr.len()) as u64).to_le_bytes());
            fc.extend_from_slice(&info);
            fc.extend_from_slice(&segm);
            fc.extend_from_slice(&aldr);
            file_chunks.push(fc);
        }

        let mut index_data = Vec::new();
        for fc in &file_chunks {
            index_data.extend_from_slice(fc);
        }
        let index_ofs = file.len() as u64;
        file.push(0u8); // raw index block
        file.extend_from_slice(&(index_data.len() as u64).to_le_bytes());
        file.extend_from_slice(&index_data);
        file[11..19].copy_from_slice(&index_ofs.to_le_bytes());
        file
    }

    #[test]
    fn normalize_matches_reference() {
        assert_eq!(
            normalize_in_archive_name(r"Data\BG\Title.jpg"),
            "data/bg/title.jpg"
        );
        assert_eq!(
            normalize_in_archive_name("Data//BG//title.jpg"),
            "data/bg/title.jpg"
        );
    }

    /// A unique scratch directory with an **uppercase** component, so the
    /// tests prove the mount prefix is not lowercased.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("TvpStorage-{tag}-{}-{n}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn absolute_and_relative_mixed_case_resolve_to_real_file() {
        let dir = TempDir::new("mixedcase");
        fs::create_dir_all(dir.path().join("savedata")).unwrap();
        let real = dir.path().join("savedata/savemng.dat");
        fs::write(&real, b"save data").unwrap();
        let storage = Storage::mount(dir.path()).unwrap();

        let game = dir.path().to_string_lossy().into_owned();
        for name in [
            format!("{game}/savedata/saveMng.dat"),
            format!("{game}/SAVEDATA/SAVEMNG.DAT"),
            "savedata/saveMng.dat".to_string(),
            "SAVEDATA/SAVEMNG.DAT".to_string(),
        ] {
            let location = storage
                .find(&name)
                .unwrap_or_else(|| panic!("{name} must resolve"));
            assert_eq!(location, Location::Disk(real.clone()), "{name}");
            assert!(storage.exists(&name), "{name}");
            assert_eq!(storage.stat(&name).unwrap().size, 9, "{name}");
            assert_eq!(storage.find_disk(&name).unwrap(), real, "{name}");
        }

        // A path outside the mount never resolves, even when it exists.
        let outside =
            std::env::temp_dir().join(format!("TvpStorage-outside-{}.txt", std::process::id()));
        fs::write(&outside, b"x").unwrap();
        assert!(storage.find(&outside.to_string_lossy()).is_none());
        assert!(!storage.exists(&outside.to_string_lossy()));
        assert!(
            storage
                .relative_disk_name(&outside.to_string_lossy())
                .is_none()
        );
        let _ = fs::remove_file(&outside);
    }

    #[test]
    fn lowercase_canonical_wins_over_exact_case_variant() {
        // The reference lowercases storage names before lookup, so when two
        // files differ only by case the canonical (lowercase) one wins. This
        // is what makes the game's `saveMng.dat` read the real `savemng.dat`
        // even though a stray exact-case `saveMng.dat` exists next to it.
        let dir = TempDir::new("shadow");
        fs::create_dir_all(dir.path().join("savedata")).unwrap();
        fs::write(dir.path().join("savedata/savemng.dat"), b"canonical").unwrap();
        fs::write(dir.path().join("savedata/saveMng.dat"), b"stray").unwrap();
        let storage = Storage::mount(dir.path()).unwrap();
        let location = storage
            .find(&format!("{}/savedata/saveMng.dat", dir.path().display()))
            .unwrap();
        assert_eq!(
            location,
            Location::Disk(dir.path().join("savedata/savemng.dat"))
        );
    }

    #[test]
    fn traversal_and_outer_names_do_not_resolve() {
        let dir = TempDir::new("traversal");
        fs::create_dir_all(dir.path().join("savedata")).unwrap();
        fs::write(dir.path().join("escape.dat"), b"in").unwrap();
        fs::write(
            dir.path()
                .parent()
                .unwrap()
                .join(format!("TvpStorage-neighbour-{}.dat", std::process::id())),
            b"out",
        )
        .unwrap();
        let storage = Storage::mount(dir.path()).unwrap();
        assert!(storage.find("../escape.dat").is_none());
        assert!(storage.find("savedata/../../escape.dat").is_none());
        let _ = fs::remove_file(
            dir.path()
                .parent()
                .unwrap()
                .join(format!("TvpStorage-neighbour-{}.dat", std::process::id())),
        );
    }

    #[test]
    fn clear_archive_cache_drops_parsed_archives_without_opening() {
        let dir = TempDir::new("clear-cache");
        fs::write(
            dir.path().join("data.xp3"),
            build_xp3(&[("data/inside.txt", b"hello from archive")]),
        )
        .unwrap();
        let mut storage = Storage::mount(dir.path()).unwrap();

        // Mounting records the path but parses nothing.
        assert_eq!(storage.archive_count(), 1);
        assert_eq!(storage.parsed_archive_count(), 0);

        // The first lookup parses exactly the one archive it scans.
        assert!(storage.find("data/inside.txt").is_some());
        assert_eq!(storage.parsed_archive_count(), 1);
        assert_eq!(
            storage.read("data/inside.txt").unwrap(),
            b"hello from archive"
        );

        // The clear drops the parsed archive and opens nothing itself.
        storage.clear_archive_cache();
        assert_eq!(storage.parsed_archive_count(), 0);
        assert_eq!(storage.archive_count(), 1);

        // ...and the name still resolves and reads after the clear.
        assert!(storage.find("data/inside.txt").is_some());
        assert_eq!(storage.parsed_archive_count(), 1);
        assert_eq!(
            storage.read("data/inside.txt").unwrap(),
            b"hello from archive"
        );
    }

    #[test]
    fn clear_archive_cache_reopens_only_as_far_as_needed() {
        // A name in the first archive must resolve after one parse; a name in
        // the second must resolve after walking (and parsing) both.
        let dir = TempDir::new("clear-cache-two");
        fs::write(dir.path().join("a.xp3"), build_xp3(&[("only/a.txt", b"a")])).unwrap();
        fs::write(dir.path().join("b.xp3"), build_xp3(&[("only/b.txt", b"b")])).unwrap();
        let storage = Storage::mount(dir.path()).unwrap();
        assert_eq!(storage.archive_count(), 2);
        assert_eq!(storage.parsed_archive_count(), 0);

        assert_eq!(
            storage.find("only/a.txt"),
            Some(Location::Archive(
                dir.path().join("a.xp3"),
                "only/a.txt".into()
            ))
        );
        assert_eq!(storage.parsed_archive_count(), 1);

        storage.clear_archive_cache();
        assert_eq!(storage.parsed_archive_count(), 0);

        assert_eq!(
            storage.find("only/b.txt"),
            Some(Location::Archive(
                dir.path().join("b.xp3"),
                "only/b.txt".into()
            ))
        );
        assert_eq!(storage.parsed_archive_count(), 2);
    }

    #[test]
    fn repeated_clear_and_lookup_cycles_stay_correct() {
        let dir = TempDir::new("clear-cache-cycles");
        fs::write(
            dir.path().join("x.xp3"),
            build_xp3(&[("n/value.txt", b"v")]),
        )
        .unwrap();
        let mut storage = Storage::mount(dir.path()).unwrap();
        for _ in 0..3 {
            assert!(storage.exists("n/value.txt"));
            assert_eq!(storage.read("n/value.txt").unwrap(), b"v");
            storage.clear_archive_cache();
            assert_eq!(storage.parsed_archive_count(), 0);
            assert!(storage.exists("n/value.txt"));
        }
    }

    #[test]
    fn unreadable_archive_is_skipped_but_not_cached() {
        // A `.xp3` that is not an XP3 must not make lookups fail; it is
        // logged and skipped, and its failure is not cached as an open
        // archive. Disk files and later archives still resolve.
        let dir = TempDir::new("bad-archive");
        fs::write(dir.path().join("a.xp3"), b"not an xp3").unwrap();
        fs::write(
            dir.path().join("b.xp3"),
            build_xp3(&[("real.txt", b"real")]),
        )
        .unwrap();
        fs::write(dir.path().join("disk.txt"), b"disk").unwrap();
        let mut storage = Storage::mount(dir.path()).unwrap();
        assert!(storage.exists("disk.txt"));
        assert_eq!(
            storage.find("real.txt"),
            Some(Location::Archive(
                dir.path().join("b.xp3"),
                "real.txt".into()
            ))
        );
        assert_eq!(storage.read("real.txt").unwrap(), b"real");
        // Only the valid archive became cached.
        assert_eq!(storage.parsed_archive_count(), 1);
    }
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn absolute_path_resolves() {
        let game = "/mnt/DATA/Games/Others/test";
        if !Path::new(game).is_dir() {
            eprintln!("skipping: {game} is not present on this host");
            return;
        }
        let storage = Storage::mount(game).unwrap();
        let loc = storage.find(&format!("{game}/data.xp3"));
        println!("absolute: {loc:?}");
        assert!(loc.is_some(), "absolute path must resolve");
        assert!(storage.find("data.xp3").is_some());
        assert!(storage.find("system/Utility.tjs").is_some());
    }
}
