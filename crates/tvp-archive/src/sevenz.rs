//! 7z archive reader (read-only) wrapping `sevenz-rust`.
//!
//! Mirrors the reference `TVPOpen7ZArchive` in
//! `reference/cpp/core/archive/7z/7zArchive.cpp`: the archive header is parsed
//! at open time (entry names + sizes), directory entries are skipped, and
//! stored names are normalized the same way the reference engine does.
//!
//! `sevenz-rust` has no "read one file by name" API; its `SevenZReader`
//! decodes folders via [`sevenz_rust::BlockDecoder`]. We keep the raw file
//! and the parsed [`sevenz_rust::Archive`], then decode only the folder
//! containing the requested entry, reading (and discarding) the entries that
//! precede it inside that folder (required for solid blocks).

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use sevenz_rust::{Archive as SevenZData, BlockDecoder, Password};

use crate::error::{Error, Result};
use crate::{Archive, Entry, normalize_in_archive_name};

/// An opened 7z archive.
pub struct SevenZipArchive {
    path: PathBuf,
    file: File,
    archive: SevenZData,
    /// normalized name -> (file index in `archive.files`, uncompressed size)
    files: BTreeMap<String, (usize, u64)>,
}

impl SevenZipArchive {
    /// Open a 7z archive from a file on disk.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let mut file = File::open(&path)?;
        let len = file.metadata().map(|m| m.len()).map_err(Error::Io)?;
        let password = Password::empty();
        let archive = SevenZData::read(&mut file, len, password.as_slice()).map_err(map_7z_err)?;

        let mut files = BTreeMap::new();
        for (i, f) in archive.files.iter().enumerate() {
            if f.is_directory {
                continue; // match the reference, which skips directory entries
            }
            let name = normalize_in_archive_name(f.name());
            files.insert(name, (i, f.size()));
        }
        Ok(Self {
            path,
            file,
            archive,
            files,
        })
    }
}

impl Archive for SevenZipArchive {
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
        let &(file_index, size) = self
            .files
            .get(&normalized)
            .ok_or_else(|| Error::NotFound(normalized.clone()))?;

        // Files without a stream (empty files, pure directories) carry no
        // compressed data.
        let Some(folder_index) = self.archive.stream_map.file_folder_index[file_index] else {
            return Ok(Vec::new());
        };

        // Decode only the folder holding the requested file. Entries inside a
        // folder share one (possibly solid) compressed stream, so everything
        // before the target must be read and discarded to reach it.
        let start = self.archive.stream_map.folder_first_file_index[folder_index];
        let mut position = start;
        let mut out: Option<Vec<u8>> = None;
        let decoder = BlockDecoder::new(folder_index, &self.archive, &[], &mut self.file);
        decoder
            .for_each_entries(&mut |_entry, reader| {
                if position == file_index {
                    let mut buf = Vec::with_capacity(size as usize);
                    match reader.read_to_end(&mut buf) {
                        Ok(_) => {
                            out = Some(buf);
                            Ok(false) // stop after the target
                        }
                        Err(e) => Err(e.into()),
                    }
                } else {
                    position += 1;
                    match std::io::copy(reader, &mut std::io::sink()) {
                        Ok(_) => Ok(true),
                        Err(e) => Err(e.into()),
                    }
                }
            })
            .map_err(map_7z_err)?;

        out.ok_or_else(|| Error::Corrupt(format!("7z: no data for `{normalized}`")))
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

fn map_7z_err(e: sevenz_rust::Error) -> Error {
    match e {
        sevenz_rust::Error::Io(e, _) | sevenz_rust::Error::FileOpen(e, _) => Error::Io(e),
        other => Error::Corrupt(format!("7z: {other}")),
    }
}
