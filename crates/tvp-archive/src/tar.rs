//! TAR archive reader (read-only) wrapping the `tar` crate.
//!
//! Mirrors the reference `TVPOpenTARArchive` in
//! `reference/cpp/core/archive/tar/TARArchive.cpp`: only regular-file members
//! are listed, stored names are normalized the same way the reference engine
//! does, and each file is read back by seeking to its recorded offset.
//! GNU long-name and PAX extended-name members are resolved by the `tar`
//! crate itself (`Entry::path`), like the reference resolves `././@LongLink`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::{Archive, Entry, normalize_in_archive_name};

/// Location of one tar member inside the archive file.
#[derive(Debug, Clone, Copy)]
struct TarRecord {
    size: u64,
    offset: u64,
}

/// An opened TAR archive.
pub struct TarArchive {
    path: PathBuf,
    file: File,
    /// normalized name -> member location
    files: BTreeMap<String, TarRecord>,
}

impl TarArchive {
    /// Open a TAR archive from a file on disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let mut archive = tar::Archive::new(file);

        let mut files = BTreeMap::new();
        {
            let entries = archive.entries_with_seek()?;
            for entry in entries {
                let entry = entry?;
                if !entry.header().entry_type().is_file() {
                    continue; // skip dirs, links, long-name headers, ...
                }
                let name = entry
                    .path()
                    .map_err(Error::Io)?
                    .to_string_lossy()
                    .into_owned();
                let name = normalize_in_archive_name(&name);
                files.insert(
                    name,
                    TarRecord {
                        size: entry.size(),
                        offset: entry.raw_file_position(),
                    },
                );
            }
        }

        Ok(Self {
            path,
            file: archive.into_inner(),
            files,
        })
    }
}

impl Archive for TarArchive {
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
        let rec = self
            .files
            .get(&normalized)
            .ok_or_else(|| Error::NotFound(normalized.clone()))?;
        let mut buf = vec![0u8; rec.size as usize];
        if rec.size == 0 {
            return Ok(buf);
        }
        self.file.seek(SeekFrom::Start(rec.offset))?;
        self.file.read_exact(&mut buf)?;
        Ok(buf)
    }

    fn entries(&self) -> Vec<Entry> {
        self.files
            .iter()
            .map(|(name, rec)| Entry {
                name: name.clone(),
                org_size: rec.size,
            })
            .collect()
    }
}
