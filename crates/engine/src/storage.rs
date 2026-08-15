//! Game storage: mount a game directory and its `.xp3` archives, and resolve
//! storage names the way the reference engine does.

use std::fs;
use std::path::{Path, PathBuf};

use xp3::{Xp3Archive, normalize_in_archive_name};

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
            let arc = self.archives.iter().find(|(p, _)| {
                p.file_name()
                    .map(|f| f.eq_ignore_ascii_case(arc))
                    .unwrap_or(false)
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
