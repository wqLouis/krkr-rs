//! Game storage: mount a game directory and its `.xp3` archives, and resolve
//! storage names the way the reference engine does.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use xp3::{Xp3Archive, normalize_in_archive_name};

/// Process-wide auto search paths (the reference's `TVPAutoPathTable`).
/// Entries are storage-name prefixes: plain dirs (`system/`) or archive
/// prefixes (`data.xp3>system/`). The game populates them at startup via
/// `Storages.addAutoPath`.
static AUTO_PATHS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Replace the auto-path list (used at startup).
pub fn set_auto_paths(paths: Vec<String>) {
    *AUTO_PATHS.lock().unwrap() = paths;
}

/// Append one auto path (the `Storages.addAutoPath` native).
pub fn add_auto_path(path: String) {
    let mut list = AUTO_PATHS.lock().unwrap();
    if !list.contains(&path) {
        list.push(path);
    }
}

/// Remove one auto path (the `Storages.removeAutoPath` native).
pub fn remove_auto_path(path: &str) {
    let mut list = AUTO_PATHS.lock().unwrap();
    list.retain(|p| p != path);
}

/// Current auto-path list.
pub fn auto_paths() -> Vec<String> {
    AUTO_PATHS.lock().unwrap().clone()
}

/// The base storage name (the part after the last `/`).
fn storage_base(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
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
    /// order: explicit `arc.xp3>path` → disk file (as-is, then normalized)
    /// → first archive containing the normalized name.
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

        // Disk file.
        for cand in [name, &normalize_in_archive_name(name)] {
            let path = self.game_dir.join(cand);
            if path.is_file() {
                return Some(Location::Disk(path));
            }
        }

        // Archives, in mount order.
        for (path, arc) in &self.archives {
            let normalized = normalize_in_archive_name(name);
            if arc.entry(&normalized).is_some() {
                return Some(Location::Archive(path.clone(), normalized));
            }
        }

        // Auto paths: the base name joined with each registered prefix
        // (last added wins, like the reference's hash-table Add). Archive
        // prefixes recurse into `find`; plain dirs hit the disk.
        let normalized_name = normalize_in_archive_name(name);
        let base = storage_base(&normalized_name);
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
}

#[cfg(test)]
mod probe_tests {
    use super::*;

    #[test]
    fn absolute_path_resolves() {
        let storage = Storage::mount("/mnt/DATA/Games/Others/test").unwrap();
        let loc = storage.find("/mnt/DATA/Games/Others/test/data.xp3");
        println!("absolute: {loc:?}");
        assert!(loc.is_some(), "absolute path must resolve");
        assert!(storage.find("data.xp3").is_some());
        assert!(storage.find("system/Utility.tjs").is_some());
    }
}
