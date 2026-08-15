//! Uniform read-only archive readers for the archive formats KiriKiri can
//! open besides its native XP3: **zip**, **7z** and **tar**.
//!
//! This crate mirrors the surface of the `xp3` crate (and of the reference
//! `tTVPArchive` interface in `reference/cpp/core/base/StorageIntf.h`):
//! an archive is opened from a path, entries are looked up by *normalized*
//! name (`lowercase`, `\` → `/`, duplicate slashes collapsed — see
//! [`normalize_in_archive_name`], shared with the `xp3` crate), and the full
//! contents of an entry are decompressed into memory on demand.
//!
//! ```no_run
//! use tvp_archive::open;
//!
//! let mut arc = open("game/data.zip").expect("open");
//! assert!(arc.contains("script/startup.tjs"));
//! let data = arc.read("script/startup.tjs").unwrap();
//! ```
//!
//! The format is chosen from the file extension (`.zip`, `.7z`, `.tar`,
//! case-insensitive). The reference implementations this ports are
//! `reference/cpp/core/archive/{zip,7z,tar}/*.cpp`.

pub mod error;
#[path = "sevenz.rs"]
mod sevenz_archive;
#[path = "tar.rs"]
mod tar_archive;
#[path = "zip.rs"]
mod zip_archive;

pub use error::{Error, Result};
pub use xp3::normalize_in_archive_name;

use std::path::Path;

/// A single file entry inside an archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Normalized in-archive name (lowercase, `/` separators).
    pub name: String,
    /// Uncompressed size in bytes.
    pub org_size: u64,
}

/// Uniform interface over the archive readers.
///
/// Mirrors the `xp3` crate's `Xp3Archive` surface (and the reference
/// `tTVPArchive` interface): `contains` / `read` take normalized names, and
/// `entries` are returned in normalized-name order.
pub trait Archive: Send {
    /// Path of the archive file.
    fn path(&self) -> &Path;

    /// Number of entries.
    fn len(&self) -> usize;

    /// Whether the archive has no entries.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether an entry with the given name exists (normalized lookup).
    fn contains(&self, name: &str) -> bool;

    /// Read the full contents of an entry by normalized name.
    fn read(&mut self, name: &str) -> Result<Vec<u8>>;

    /// All entries as (normalized name, uncompressed size), sorted by name.
    fn entries(&self) -> Vec<Entry>;
}

/// Open an archive from a file on disk, dispatching on the file extension
/// (`.zip`, `.7z` or `.tar`, case-insensitive).
///
/// Any other extension (or no extension) yields
/// [`Error::UnsupportedFormat`]; a file that does not actually contain the
/// archive its extension promises yields [`Error::Corrupt`].
pub fn open(path: impl AsRef<Path>) -> Result<Box<dyn Archive>> {
    let path = path.as_ref();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    let archive: Box<dyn Archive> = match ext.as_str() {
        "zip" => Box::new(zip_archive::ZipArchive::open(path)?),
        "7z" => Box::new(sevenz_archive::SevenZipArchive::open(path)?),
        "tar" => Box::new(tar_archive::TarArchive::open(path)?),
        _ => return Err(Error::UnsupportedFormat(path.to_path_buf())),
    };
    Ok(archive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;
    use std::path::{Path, PathBuf};

    /// Shared fixture layout (built per format):
    /// * `Data/File.TXT` → `hello <fmt>` (9/8 bytes)
    /// * `Sub\empty.txt` → empty (stored with a backslash)
    /// * `deep/deeper/deepest/<120×x>/file.txt` → `long!` (forces GNU long
    ///   names in tar)
    ///
    /// plus a `Dir/` directory entry, which must be skipped by the readers.
    const LONG_DIR: &str = "deep/deeper/deepest";

    fn long_name() -> String {
        format!("{LONG_DIR}/{}/file.txt", "x".repeat(120))
    }

    /// Unique per-process temp path for a test artifact.
    fn tmp_path(kind: &str, test: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "tvp-archive-test-{}-{test}.{kind}",
            std::process::id()
        ))
    }

    fn make_zip(path: &Path) {
        let file = fs::File::create(path).unwrap();
        let mut w = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default();
        w.start_file("Data/File.TXT", opts).unwrap();
        w.write_all(b"hello zip").unwrap();
        w.start_file(r"Sub\empty.txt", opts).unwrap();
        w.add_directory("Dir", opts).unwrap();
        w.start_file(long_name(), opts).unwrap();
        w.write_all(b"long!").unwrap();
        w.finish().unwrap();
    }

    fn make_7z(path: &Path) {
        let mut w = sevenz_rust::SevenZWriter::create(path).unwrap();
        let mut e = sevenz_rust::SevenZArchiveEntry::new();
        e.name = "Data/File.TXT".into();
        w.push_archive_entry(e, Some(&b"hello 7z"[..])).unwrap();
        // Empty file: no stream (K_EMPTY_STREAM), name stored with a
        // backslash.
        let mut e = sevenz_rust::SevenZArchiveEntry::new();
        e.name = r"Sub\empty.txt".into();
        w.push_archive_entry(e, None::<&[u8]>).unwrap();
        let mut e = sevenz_rust::SevenZArchiveEntry::new();
        e.name = "Dir".into();
        e.is_directory = true;
        w.push_archive_entry(e, None::<&[u8]>).unwrap();
        let mut e = sevenz_rust::SevenZArchiveEntry::new();
        e.name = long_name();
        w.push_archive_entry(e, Some(&b"long!"[..])).unwrap();
        w.finish().unwrap();
    }

    fn make_tar(path: &Path) {
        let file = fs::File::create(path).unwrap();
        let mut b = tar::Builder::new(file);
        let mut h = tar::Header::new_gnu();
        h.set_size(9);
        h.set_cksum();
        b.append_data(&mut h, "Data/File.TXT", &b"hello tar"[..])
            .unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(0);
        h.set_cksum();
        b.append_data(&mut h, r"Sub\empty.txt", &b""[..]).unwrap();
        let mut h = tar::Header::new_gnu();
        h.set_size(5);
        h.set_cksum();
        b.append_data(&mut h, long_name(), &b"long!"[..]).unwrap();
        // Directory entry (typeflag '5') — must be skipped.
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Directory);
        h.set_size(0);
        h.set_cksum();
        b.append_data(&mut h, "Dir", &b""[..]).unwrap();
        b.finish().unwrap();
    }

    /// The per-format fixture writer.
    fn make_fixture(fmt: &str, path: &Path) {
        match fmt {
            "zip" => make_zip(path),
            "7z" => make_7z(path),
            "tar" => make_tar(path),
            other => panic!("unknown fixture format {other}"),
        }
    }

    /// Round-trip checks shared by all three formats.
    fn roundtrip(fmt: &str) {
        let expected = format!("hello {fmt}");
        let path = tmp_path(fmt, "roundtrip");
        make_fixture(fmt, &path);

        let mut arc = open(&path).expect("open archive");
        assert_eq!(arc.path(), path);
        assert_eq!(arc.len(), 3, "directory entries must be skipped");

        // Case-insensitive lookup of a mixed-case stored name.
        assert!(arc.contains("data/file.txt"));
        assert!(arc.contains(r"DATA\FILE.TXT"));
        assert!(!arc.contains("missing"));

        // Mixed-case query reads the stored `Data/File.TXT`.
        assert_eq!(arc.read("DATA/FILE.TXT").unwrap(), expected.as_bytes());
        assert_eq!(arc.read("data/file.txt").unwrap(), expected.as_bytes());
        // Stored `Sub\empty.txt` found via the normalized slash form.
        assert_eq!(arc.read("sub/empty.txt").unwrap(), b"");
        // GNU-long-name member (tar) / long name (zip, 7z).
        assert_eq!(arc.read(&long_name()).unwrap(), b"long!");

        // Missing entry.
        let err = arc.read("nope.txt").unwrap_err();
        assert!(matches!(err, Error::NotFound(_)), "got {err:?}");

        // Entries sorted by normalized name, sizes correct.
        let entries = arc.entries();
        assert_eq!(entries.len(), 3);
        assert!(entries.windows(2).all(|w| w[0].name <= w[1].name));
        assert_eq!(entries[0].name, "data/file.txt");
        assert_eq!(entries[0].org_size, expected.len() as u64);
        assert_eq!(entries[1].name, long_name());
        assert_eq!(entries[1].org_size, 5);
        assert_eq!(entries[2].name, "sub/empty.txt");
        assert_eq!(entries[2].org_size, 0);

        drop(arc);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn zip_roundtrip() {
        roundtrip("zip");
    }

    #[test]
    fn sevenz_roundtrip() {
        roundtrip("7z");
    }

    #[test]
    fn tar_roundtrip() {
        roundtrip("tar");
    }

    #[test]
    fn open_dispatch_and_errors() {
        // Unsupported extension.
        let p = tmp_path("bin", "unsupported");
        fs::write(&p, b"whatever").unwrap();
        assert!(matches!(open(&p), Err(Error::UnsupportedFormat(_))));
        fs::remove_file(&p).ok();

        // No extension at all.
        let p = tmp_path("noext", "noext");
        fs::write(&p, b"whatever").unwrap();
        assert!(matches!(open(&p), Err(Error::UnsupportedFormat(_))));
        fs::remove_file(&p).ok();

        // Missing file (but valid extension).
        let p = tmp_path("zip", "missing");
        assert!(matches!(open(&p), Err(Error::Io(_))));

        // Wrong contents for the extension.
        let p = tmp_path("zip", "garbage");
        fs::write(&p, b"this is not a zip").unwrap();
        assert!(matches!(open(&p), Err(Error::Corrupt(_))));
        fs::remove_file(&p).ok();
    }

    #[test]
    fn empty_archives() {
        // Empty zip (just the end-of-central-directory record).
        let p = tmp_path("zip", "empty");
        {
            let file = fs::File::create(&p).unwrap();
            zip::ZipWriter::new(file).finish().unwrap();
        }
        let mut arc = open(&p).unwrap();
        assert!(arc.is_empty());
        assert!(arc.entries().is_empty());
        assert!(matches!(arc.read("x").unwrap_err(), Error::NotFound(_)));
        drop(arc);
        fs::remove_file(&p).ok();

        // Empty tar (only the two end-of-archive zero blocks).
        let p = tmp_path("tar", "empty");
        {
            let file = fs::File::create(&p).unwrap();
            tar::Builder::new(file).finish().unwrap();
        }
        let mut arc = open(&p).unwrap();
        assert!(arc.is_empty());
        assert!(matches!(arc.read("x").unwrap_err(), Error::NotFound(_)));
        drop(arc);
        fs::remove_file(&p).ok();

        // Empty 7z (header with zero files).
        let p = tmp_path("7z", "empty");
        {
            sevenz_rust::SevenZWriter::create(&p)
                .unwrap()
                .finish()
                .unwrap();
        }
        let mut arc = open(&p).unwrap();
        assert!(arc.is_empty());
        assert!(matches!(arc.read("x").unwrap_err(), Error::NotFound(_)));
        drop(arc);
        fs::remove_file(&p).ok();
    }

    #[test]
    fn normalize_names() {
        // Same expectations as the xp3 crate's own test.
        assert_eq!(normalize_in_archive_name("Startup.tjs"), "startup.tjs");
        assert_eq!(normalize_in_archive_name(r"data\bg\a.jpg"), "data/bg/a.jpg");
        assert_eq!(
            normalize_in_archive_name("data//bg///a.jpg"),
            "data/bg/a.jpg"
        );
        assert_eq!(
            normalize_in_archive_name("//leading/slash"),
            "/leading/slash"
        );
    }
}
