//! ZIP archive reader (read-only) wrapping the `zip` crate.
//!
//! Mirrors the reference `TVPOpenZIPArchive` in
//! `reference/cpp/core/archive/zip/ZIPArchive.cpp`: the central directory is
//! enumerated at open time, stored names are normalized the same way the
//! reference engine does (`normalize_in_archive_name`), and entries are
//! looked up by normalized name. Decompression happens on demand per file.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use zip::ZipArchive as ZipReader;
use zip::result::ZipError;

use crate::error::{Error, Result};
use crate::{Archive, Entry, normalize_in_archive_name};

/// An opened ZIP archive.
pub struct ZipArchive {
    path: PathBuf,
    inner: ZipReader<File>,
    /// normalized name -> (central-directory index, uncompressed size)
    files: BTreeMap<String, (usize, u64)>,
}

impl ZipArchive {
    /// Open a ZIP archive from a file on disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let mut inner = ZipReader::new(file).map_err(map_zip_err)?;

        let mut files = BTreeMap::new();
        for i in 0..inner.len() {
            let f = inner.by_index(i).map_err(map_zip_err)?;
            if f.is_dir() {
                continue; // skip directory entries, like the 7z/tar readers
            }
            let name = normalize_in_archive_name(f.name());
            files.insert(name, (i, f.size()));
        }
        Ok(Self { path, inner, files })
    }
}

impl Archive for ZipArchive {
    fn path(&self) -> &Path {
        &self.path
    }

    fn len(&self) -> usize {
        self.files.len()
    }

    fn contains(&self, name: &str) -> bool {
        self.files.contains_key(&normalize_in_archive_name(name))
    }

    fn read(&mut self, name: &str) -> Result<Vec<u8>> {
        let normalized = normalize_in_archive_name(name);
        let &(index, size) = self
            .files
            .get(&normalized)
            .ok_or_else(|| Error::NotFound(normalized.clone()))?;
        let mut f = self.inner.by_index(index).map_err(|e| match e {
            ZipError::FileNotFound => Error::NotFound(normalized),
            other => map_zip_err(other),
        })?;
        let mut buf = Vec::with_capacity(size as usize);
        f.read_to_end(&mut buf).map_err(Error::Io)?;
        Ok(buf)
    }

    fn entries(&self) -> Vec<Entry> {
        self.files
            .iter()
            .map(|(name, &(_, size))| Entry {
                name: name.clone(),
                org_size: size,
            })
            .collect()
    }
}

fn map_zip_err(e: ZipError) -> Error {
    match e {
        ZipError::Io(e) => Error::Io(e),
        ZipError::FileNotFound => Error::NotFound("(zip lookup)".into()),
        other => Error::Corrupt(format!("zip: {other}")),
    }
}
