//! Integration tests for XP3 archive name normalization and index hardening.
//!
//! Relocated from the inline `#[cfg(test)]` module of the former
//! `krkr-engine/src/archive/xp3.rs`; the parser now lives in L1 `krkr-fs`.

use krkr_api::error::FsError;
use krkr_api::fs::{FsPath, OpenMode};
use krkr_fs::xp3::{XP3_MAGIC, Xp3Archive, Xp3Error, normalize_in_archive_name};
use krkr_fs::{FileState, FileSystem};

#[path = "common/xp3_builder.rs"]
mod xp3_builder;
use xp3_builder::build_xp3;

#[test]
fn normalize_names() {
    assert_eq!(normalize_in_archive_name("Startup.tjs"), "startup.tjs");
    assert_eq!(normalize_in_archive_name(r"data\bg\a.jpg"), "data/bg/a.jpg");
    assert_eq!(
        normalize_in_archive_name("data//bg///a.jpg"),
        "data/bg/a.jpg"
    );
    assert_eq!(
        normalize_in_archive_name("//leading/slash"),
        "leading/slash"
    );
    assert_eq!(normalize_in_archive_name("/leading"), "leading");
    assert_eq!(normalize_in_archive_name("/"), "");
    assert_eq!(normalize_in_archive_name("a//b"), "a/b");
}

/// Write `bytes` into a fresh in-memory root and hand back an open read
/// handle together with the root that owns it.
fn fs_with(bytes: &[u8]) -> (FileSystem, FileState) {
    let fs = FileSystem::memory();
    let path = FsPath::host("/bad.xp3");
    let file = krkr_fs::open(&fs, &path, OpenMode::Write).expect("create file");
    krkr_fs::write_at(&fs, &file, 0, bytes).expect("write file");
    krkr_fs::flush(&fs, &file).expect("flush file");
    drop(file);
    let file = krkr_fs::open(&fs, &path, OpenMode::Read).expect("open file");
    (fs, file)
}

/// Build a raw-index archive whose single `File` chunk declares `chunk_size`
/// bytes of payload (far more than the index holds).
fn archive_with_oversized_chunk(chunk_size: u64) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(&XP3_MAGIC);
    file.extend_from_slice(&0u64.to_le_bytes()); // index offset, patched below

    let mut index_data = Vec::new();
    index_data.extend_from_slice(b"File");
    index_data.extend_from_slice(&chunk_size.to_le_bytes());

    let index_ofs = file.len() as u64;
    file.push(0u8); // raw index flag
    file.extend_from_slice(&(index_data.len() as u64).to_le_bytes());
    file.extend_from_slice(&index_data);
    file[11..19].copy_from_slice(&index_ofs.to_le_bytes());
    file
}

/// Build a raw-index archive with one entry whose segment declares an absurd
/// stored size. Opening must succeed (the size is only used on read).
fn archive_with_oversized_segment() -> Vec<u8> {
    let units: Vec<u16> = "big.bin".encode_utf16().collect();
    let mut info = Vec::new();
    info.extend_from_slice(&0u32.to_le_bytes()); // flags
    info.extend_from_slice(&0u64.to_le_bytes()); // org_size
    info.extend_from_slice(&u64::MAX.to_le_bytes()); // arc_size (bogus)
    info.extend_from_slice(&(units.len() as i16).to_le_bytes());
    for u in &units {
        info.extend_from_slice(&u.to_le_bytes());
    }

    let mut segm = Vec::new();
    segm.extend_from_slice(&0u32.to_le_bytes()); // raw
    segm.extend_from_slice(&0u64.to_le_bytes()); // start
    segm.extend_from_slice(&0u64.to_le_bytes()); // org_size
    segm.extend_from_slice(&u64::MAX.to_le_bytes()); // arc_size (bogus)

    let mut body = Vec::new();
    body.extend_from_slice(b"info");
    body.extend_from_slice(&(info.len() as u64).to_le_bytes());
    body.extend_from_slice(&info);
    body.extend_from_slice(b"segm");
    body.extend_from_slice(&(segm.len() as u64).to_le_bytes());
    body.extend_from_slice(&segm);

    let mut file_chunk = Vec::new();
    file_chunk.extend_from_slice(b"File");
    file_chunk.extend_from_slice(&(body.len() as u64).to_le_bytes());
    file_chunk.extend_from_slice(&body);

    let mut file = Vec::new();
    file.extend_from_slice(&XP3_MAGIC);
    file.extend_from_slice(&0u64.to_le_bytes());
    let index_ofs = file.len() as u64;
    file.push(0u8);
    file.extend_from_slice(&(file_chunk.len() as u64).to_le_bytes());
    file.extend_from_slice(&file_chunk);
    file[11..19].copy_from_slice(&index_ofs.to_le_bytes());
    file
}

/// A chunk size that overruns the index data is a structural error, not a
/// size cap: parsing fails with `CorruptIndex`.
#[test]
fn oversized_index_chunk_is_rejected() {
    let (fs, file) = fs_with(&archive_with_oversized_chunk(u64::MAX));
    let err = match Xp3Archive::open(&fs, &file) {
        Ok(_) => panic!("expected the oversized index chunk to be rejected"),
        Err(err) => err,
    };
    assert!(
        matches!(err, Xp3Error::CorruptIndex(_)),
        "unexpected error: {err:?}"
    );
}

/// A segment that declares an unallocatable stored size is a structural
/// error on read, not a size cap.
#[test]
fn oversized_segment_read_is_rejected() {
    let (fs, file) = fs_with(&archive_with_oversized_segment());
    let archive = Xp3Archive::open(&fs, &file).expect("index itself is well-formed");
    let err = archive.read(&fs, &file, "big.bin").unwrap_err();
    assert!(
        matches!(err, Xp3Error::CorruptIndex(_)),
        "unexpected error: {err:?}"
    );
}

/// An `FsError` carried by the `Read`/`Seek` adapter is recovered as
/// `Xp3Error::Fs` rather than collapsed into an opaque I/O error.
#[test]
fn fs_errors_are_recovered_through_the_io_adapter() {
    let fs_error = FsError::NotFound {
        path: "missing.bin".into(),
    };
    let io_error = std::io::Error::other(fs_error.clone());
    let xp3_error: Xp3Error = io_error.into();
    assert!(
        matches!(xp3_error, Xp3Error::Fs(ref e) if *e == fs_error),
        "typed FsError was not preserved: {xp3_error:?}"
    );
}

/// A valid archive parses and reads both raw and zlib segments, and lookup
/// normalizes the requested name.
#[test]
fn parses_and_reads_valid_archive() {
    let xp3 = build_xp3(&[
        ("plain.bin", b"raw bytes", false),
        ("dir/Packed.bin", b"zlib compressed payload", true),
    ]);
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open archive");

    assert_eq!(archive.len(), 2);
    assert!(!archive.is_empty());
    assert_eq!(archive.entry("plain.bin").unwrap().org_size, 9);
    assert_eq!(archive.read(&fs, &file, "plain.bin").unwrap(), b"raw bytes");
    assert_eq!(
        archive.read(&fs, &file, r"DIR\PACKED.BIN").unwrap(),
        b"zlib compressed payload"
    );
    assert!(matches!(
        archive.read(&fs, &file, "missing.bin"),
        Err(Xp3Error::NotFound(_))
    ));
}

/// A non-XP3 file is a typed `NotAnXp3` error, not an I/O surprise.
#[test]
fn rejects_non_xp3_bytes() {
    let (fs, file) = fs_with(b"not an archive at all");
    assert!(matches!(
        Xp3Archive::open(&fs, &file),
        Err(Xp3Error::NotAnXp3)
    ));
}
