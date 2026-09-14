//! Game storage: mount a game directory and its `.xp3` archives, and resolve
//! storage names the way the reference engine does.

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

/// A mounted game storage: one game directory plus all `.xp3` archives
/// found inside it.
pub struct Storage {
    /// Absolute path of the game directory.
    pub game_dir: PathBuf,
    /// Opened archives in sorted-name order (deterministic).
    archives: Vec<(PathBuf, Xp3Archive)>,
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
                match Xp3Archive::open(&path) {
                    Ok(arc) => {
                        log::info!("mounted {} ({} entries)", path.display(), arc.len());
                        archives.push((path, arc));
                    }
                    Err(e) => log::warn!("skipping {}: {e}", path.display()),
                }
            }
        }
        archives.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(Storage { game_dir, archives })
    }

    /// Game directory path.
    pub fn game_dir(&self) -> &Path {
        &self.game_dir
    }

    /// Iterate over the mounted archives.
    pub fn archives(&self) -> impl Iterator<Item = (&Path, &Xp3Archive)> {
        self.archives.iter().map(|(p, a)| (p.as_path(), a))
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
            let arc = self.archives.iter().find(|(p, _)| {
                p.file_name()
                    .is_some_and(|f| f.eq_ignore_ascii_case(&*arc_name))
            })?;
            let rest = normalize_in_archive_name(rest);
            if arc.1.entry(&rest).is_some() {
                return Some(Location::Archive(arc.0.clone(), rest));
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
        for (path, arc) in &self.archives {
            if arc.entry(&normalized).is_some() {
                return Some(Location::Archive(path.clone(), normalized));
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
                let (_, arc) = self
                    .archives
                    .iter_mut()
                    .find(|(p, _)| *p == arc_path)
                    .expect("resolved archive must be mounted");
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
                let archive = self
                    .archives
                    .iter()
                    .find(|(path, _)| *path == archive_path)?;
                let entry = archive.1.entry(&in_archive)?;
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
