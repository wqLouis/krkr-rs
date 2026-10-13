//! Integration tests for XP3 archive name normalization and index hardening.
//!
//! Relocated from the inline `#[cfg(test)]` module of the former
//! `engine/src/archive/xp3.rs`; the parser now lives in L1 `fs`.

use std::io::{Read, Seek, SeekFrom};

use api::error::FsError;
use api::fs::{FsPath, OpenMode};
use fs::xp3::{SEGM_ENCODE_RAW, XP3_MAGIC, Xp3Archive, Xp3Error, normalize_in_archive_name};
use fs::{FileState, FileSystem};
use testkit::{build_xp3, build_xp3_compressed_index};

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
    let file = fs::open(&fs, &path, OpenMode::Write).expect("create file");
    fs::write_at(&fs, &file, 0, bytes).expect("write file");
    fs::flush(&fs, &file).expect("flush file");
    drop(file);
    let file = fs::open(&fs, &path, OpenMode::Read).expect("open file");
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
    body.extend_from_slice(b"adlr");
    body.extend_from_slice(&4u64.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());

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

/// A real bottom-layer read failure while reading entry bytes reaches the
/// caller as the original typed `FsError`, not an opaque I/O error.
#[test]
fn adapter_read_failure_preserves_fs_error() {
    let xp3 = build_xp3(&[("gone.bin", b"payload", false)]);
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open archive");
    // The index is parsed; removing the backing storage makes the entry-data
    // read fail with `FsError::NotFound`.
    fs::remove_file(&fs, &FsPath::host("/bad.xp3")).expect("remove backing file");

    let err = archive.read(&fs, &file, "gone.bin").unwrap_err();
    assert!(
        matches!(err, Xp3Error::Fs(FsError::NotFound { .. })),
        "typed FsError was not preserved: {err:?}"
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

/// A zlib-compressed index block (`TVP_XP3_INDEX_ENCODE_ZLIB`) parses and
/// its entries read like a raw-index archive. The reference reads the method
/// from the low three bits of the flag byte, then an `i64` compressed size
/// and an `i64` uncompressed size before the zlib stream
/// (`reference/cpp/core/archive/xp3/XP3Archive.cpp:360-390`).
#[test]
fn parses_a_zlib_compressed_index() {
    let xp3 = build_xp3_compressed_index(&[
        ("plain.bin", b"raw bytes", false),
        ("packed.bin", b"zlib segment payload", true),
    ]);
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open compressed-index archive");
    assert_eq!(archive.len(), 2);
    assert_eq!(archive.read(&fs, &file, "plain.bin").unwrap(), b"raw bytes");
    assert_eq!(
        archive.read(&fs, &file, "packed.bin").unwrap(),
        b"zlib segment payload"
    );
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

// ---------------------------------------------------------------------------
// Structural-hardening tests: chained indexes, cycles, required sub-chunks and
// size agreement. These back the review fixes 1.1, 1.2 and 1.4.
// ---------------------------------------------------------------------------

/// Build a raw `info` sub-chunk payload.
fn info_payload(name: &str, org_size: u64, arc_size: u64) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut payload = Vec::new();
    payload.extend_from_slice(&0u32.to_le_bytes()); // flags
    payload.extend_from_slice(&org_size.to_le_bytes());
    payload.extend_from_slice(&arc_size.to_le_bytes());
    payload.extend_from_slice(&(units.len() as i16).to_le_bytes());
    for unit in &units {
        payload.extend_from_slice(&unit.to_le_bytes());
    }
    payload
}

/// Build a raw `segm` sub-chunk payload holding one 28-byte segment.
fn segm_payload(flags: u32, start: u64, org_size: u64, arc_size: u64) -> Vec<u8> {
    let mut payload = Vec::new();
    payload.extend_from_slice(&flags.to_le_bytes());
    payload.extend_from_slice(&start.to_le_bytes());
    payload.extend_from_slice(&org_size.to_le_bytes());
    payload.extend_from_slice(&arc_size.to_le_bytes());
    payload
}

/// Wrap `tag` + `payload` in the `[tag][u64 size][data]` sub-chunk framing.
fn sub_chunk(tag: &[u8; 4], payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(tag);
    out.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// Build a single-entry raw `File` chunk expecting `data` at `start`.
fn file_chunk_raw(name: &str, org_size: u64, arc_size: u64, start: u64) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&sub_chunk(b"info", &info_payload(name, org_size, arc_size)));
    body.extend_from_slice(&sub_chunk(
        b"segm",
        &segm_payload(0, start, org_size, arc_size),
    ));
    body.extend_from_slice(&sub_chunk(b"adlr", &0u32.to_le_bytes()));

    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"File");
    chunk.extend_from_slice(&(body.len() as u64).to_le_bytes());
    chunk.extend_from_slice(&body);
    chunk
}

/// Wrap a `File` chunk body (the `info`/`segm`/`adlr` concatenation) in a
/// single-entry raw-index archive.
fn archive_with_file_body(body: &[u8]) -> Vec<u8> {
    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"File");
    chunk.extend_from_slice(&(body.len() as u64).to_le_bytes());
    chunk.extend_from_slice(body);
    archive_with_index_data(&chunk)
}

/// Wrap arbitrary raw index data in an archive whose first index block is raw.
fn archive_with_index_data(index_data: &[u8]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(&XP3_MAGIC);
    file.extend_from_slice(&0u64.to_le_bytes()); // index offset, patched below
    let index_ofs = file.len() as u64;
    file.push(0u8); // raw index flag
    file.extend_from_slice(&(index_data.len() as u64).to_le_bytes());
    file.extend_from_slice(index_data);
    file[11..19].copy_from_slice(&index_ofs.to_le_bytes());
    file
}

/// Build a two-block chained archive: the first raw index block carries
/// `INDEX_CONTINUE` and is followed by the next offset and a second block.
fn build_xp3_chained(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(&XP3_MAGIC);
    file.extend_from_slice(&0u64.to_le_bytes());

    let mut chunks = Vec::new();
    for (name, data) in entries {
        let start = file.len() as u64;
        file.extend_from_slice(data);
        chunks.push(file_chunk_raw(
            name,
            data.len() as u64,
            data.len() as u64,
            start,
        ));
    }

    let mid = chunks.len() / 2;
    let data1: Vec<u8> = chunks[..mid].iter().flatten().copied().collect();
    let data2: Vec<u8> = chunks[mid..].iter().flatten().copied().collect();

    let idx1 = file.len() as u64;
    let mut block1 = Vec::new();
    block1.push(0x80u8); // raw + INDEX_CONTINUE
    block1.extend_from_slice(&(data1.len() as u64).to_le_bytes());
    block1.extend_from_slice(&data1);

    let next_slot = idx1 + block1.len() as u64;
    let idx2 = next_slot + 8;

    file.extend_from_slice(&block1);
    file.extend_from_slice(&idx2.to_le_bytes());
    file.push(0x00u8); // raw, terminal
    file.extend_from_slice(&(data2.len() as u64).to_le_bytes());
    file.extend_from_slice(&data2);

    file[11..19].copy_from_slice(&idx1.to_le_bytes());
    file
}

/// Build an archive whose single `INDEX_CONTINUE` block points back at itself,
/// forming a cycle.
fn build_xp3_cycle(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(&XP3_MAGIC);
    file.extend_from_slice(&0u64.to_le_bytes());

    let mut data = Vec::new();
    for (name, bytes) in entries {
        let start = file.len() as u64;
        file.extend_from_slice(bytes);
        data.extend_from_slice(&file_chunk_raw(
            name,
            bytes.len() as u64,
            bytes.len() as u64,
            start,
        ));
    }

    let idx1 = file.len() as u64;
    file.push(0x80u8); // raw + INDEX_CONTINUE
    file.extend_from_slice(&(data.len() as u64).to_le_bytes());
    file.extend_from_slice(&data);
    // The "next" offset points back at this block: a cycle.
    file.extend_from_slice(&idx1.to_le_bytes());

    file[11..19].copy_from_slice(&idx1.to_le_bytes());
    file
}

/// A valid two-block `INDEX_CONTINUE` chain parses and reads (review 1.1).
#[test]
fn chained_index_continue_blocks_are_followed() {
    let xp3 = build_xp3_chained(&[("first.bin", b"first"), ("second.bin", b"second")]);
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open chained archive");
    assert_eq!(archive.len(), 2);
    assert_eq!(archive.read(&fs, &file, "first.bin").unwrap(), b"first");
    assert_eq!(archive.read(&fs, &file, "second.bin").unwrap(), b"second");
}

/// A cyclic index chain is a typed corruption error, not an infinite loop and
/// not an arbitrary "chain too long" cap (review 1.1).
#[test]
fn index_chain_cycle_is_rejected() {
    let xp3 = build_xp3_cycle(&[("loop.bin", b"loop")]);
    let (fs, file) = fs_with(&xp3);
    assert!(matches!(
        Xp3Archive::open(&fs, &file),
        Err(Xp3Error::CorruptIndex("index chain cycle"))
    ));
}

/// A `File` chunk missing its `segm` sub-chunk is rejected (review 1.2).
#[test]
fn file_chunk_missing_segm_is_rejected() {
    let mut body = Vec::new();
    body.extend_from_slice(&sub_chunk(b"info", &info_payload("a.bin", 3, 3)));
    body.extend_from_slice(&sub_chunk(b"adlr", &0u32.to_le_bytes()));
    let (fs, file) = fs_with(&archive_with_file_body(&body));
    assert!(matches!(
        Xp3Archive::open(&fs, &file),
        Err(Xp3Error::CorruptIndex(_))
    ));
}

/// A `File` chunk missing its `info` sub-chunk is rejected rather than
/// inserting an entry named `""` (review 1.2).
#[test]
fn file_chunk_missing_info_is_rejected() {
    let mut body = Vec::new();
    body.extend_from_slice(&sub_chunk(b"segm", &segm_payload(0, 0, 3, 3)));
    body.extend_from_slice(&sub_chunk(b"adlr", &0u32.to_le_bytes()));
    let (fs, file) = fs_with(&archive_with_file_body(&body));
    assert!(matches!(
        Xp3Archive::open(&fs, &file),
        Err(Xp3Error::CorruptIndex(_))
    ));
}

/// An empty `segm` payload is rejected instead of producing a zero-segment
/// entry whose `stat` size disagrees with an empty `read` (review 1.2).
#[test]
fn file_chunk_empty_segm_is_rejected() {
    let mut body = Vec::new();
    body.extend_from_slice(&sub_chunk(b"info", &info_payload("a.bin", 3, 3)));
    body.extend_from_slice(&sub_chunk(b"segm", &[]));
    body.extend_from_slice(&sub_chunk(b"adlr", &0u32.to_le_bytes()));
    let (fs, file) = fs_with(&archive_with_file_body(&body));
    assert!(matches!(
        Xp3Archive::open(&fs, &file),
        Err(Xp3Error::CorruptIndex(_))
    ));
}

/// `info.org_size` that disagrees with the segment sum is a real read error,
/// not a debug-only assertion (review 1.4).
#[test]
fn org_size_mismatch_is_rejected_on_read() {
    let mut file = Vec::new();
    file.extend_from_slice(&XP3_MAGIC);
    file.extend_from_slice(&0u64.to_le_bytes());
    let start = file.len() as u64;
    file.extend_from_slice(b"hello"); // five stored bytes
    let chunk = file_chunk_raw("a.bin", 99, 5, start);
    let index_ofs = file.len() as u64;
    file.push(0u8);
    file.extend_from_slice(&(chunk.len() as u64).to_le_bytes());
    file.extend_from_slice(&chunk);
    file[11..19].copy_from_slice(&index_ofs.to_le_bytes());

    let (fs, file) = fs_with(&file);
    let archive = Xp3Archive::open(&fs, &file).expect("index is structurally valid");
    assert!(matches!(
        archive.read(&fs, &file, "a.bin"),
        Err(Xp3Error::CorruptIndex(_))
    ));
}

/// Trailing bytes after the last complete index chunk are corruption, not
/// silently dropped.
#[test]
fn trailing_index_bytes_are_rejected() {
    let (fs, file) = fs_with(&archive_with_index_data(&[0u8; 4]));
    assert!(matches!(
        Xp3Archive::open(&fs, &file),
        Err(Xp3Error::CorruptIndex(_))
    ));
}

/// An EXE-embedded archive (MZ header + 16-byte-aligned magic) is located and
/// read correctly, exercising the rewritten `find_xp3_offset`.
#[test]
fn exe_embedded_archive_is_found() {
    let xp3 = build_xp3(&[("embedded.bin", b"payload", false)]);
    let mut exe = vec![0u8; 64];
    exe[0] = b'M';
    exe[1] = b'Z';
    exe.extend_from_slice(&xp3);

    let (fs, file) = fs_with(&exe);
    let archive = Xp3Archive::open(&fs, &file).expect("find embedded archive");
    assert_eq!(archive.base_offset(), 64);
    assert_eq!(
        archive.read(&fs, &file, "embedded.bin").unwrap(),
        b"payload"
    );
}

/// A `Read + Seek` wrapper that never returns more than `max` bytes from a
/// single `read`, forcing every stream boundary through the parser.
struct StingyReader<'a> {
    data: &'a [u8],
    pos: usize,
    max: usize,
}

impl Read for StingyReader<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let remaining = self.data.len().saturating_sub(self.pos);
        let n = buf.len().min(self.max).min(remaining);
        buf[..n].copy_from_slice(&self.data[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl Seek for StingyReader<'_> {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let target: i128 = match pos {
            SeekFrom::Start(off) => off as i128,
            SeekFrom::End(off) => self.data.len() as i128 + off as i128,
            SeekFrom::Current(off) => self.pos as i128 + off as i128,
        };
        self.pos = target.max(0) as usize;
        Ok(self.pos as u64)
    }
}

/// A short read must not hide an 11-byte magic that straddles the boundary
/// (review 1.7). `StingyReader` hands back at most three bytes at a time, so
/// the magic never arrives whole in one `read`.
#[test]
fn short_read_straddle_finds_embedded_archive() {
    let xp3 = build_xp3(&[("embedded.bin", b"payload", false)]);
    let mut exe = vec![0u8; 64];
    exe[0] = b'M';
    exe[1] = b'Z';
    exe.extend_from_slice(&xp3);

    let mut reader = StingyReader {
        data: &exe,
        pos: 0,
        max: 3,
    };
    let archive = Xp3Archive::open_reader(&mut reader).expect("short-read straddle");
    assert_eq!(archive.base_offset(), 64);
    assert_eq!(archive.len(), 1);
}

/// The stateless extraction-filter hook is invoked once per decoded segment
/// with the entry metadata and the running uncompressed offset, and its
/// in-place rewrite is what `read_with_filter` returns. A zlib segment is
/// filtered after inflation, matching `tTVPXP3ArchiveStream::Read`
/// (`XP3Archive.cpp:998`).
#[test]
fn extraction_filter_rewrites_decoded_segments_in_place() {
    use std::cell::RefCell;

    let key = 0x5Au8;
    let plain_raw = b"first raw segment".to_vec();
    let plain_zlib = b"a compressed second segment".to_vec();
    let cipher_raw: Vec<u8> = plain_raw.iter().map(|b| b ^ key).collect();
    let cipher_zlib: Vec<u8> = plain_zlib.iter().map(|b| b ^ key).collect();
    let xp3 = build_xp3(&[
        ("plain.bin", &cipher_raw, false),
        // `build_xp3` compresses whatever data it is given; for the reference
        // ordering the decompressed payload is the ciphertext the filter
        // decrypts.
        ("dir/packed.bin", &cipher_zlib, true),
    ]);
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open archive");

    // Without a filter the stored (encrypted) bytes come back unchanged.
    assert_eq!(archive.read(&fs, &file, "plain.bin").unwrap(), cipher_raw);

    let calls = RefCell::new(Vec::new());
    let filter = |file_hash: u32,
                  offset: u64,
                  buffer: &mut [u8],
                  file_name: &str,
                  archive_name: &str,
                  file_size: u64| {
        calls.borrow_mut().push((
            file_hash,
            offset,
            file_name.to_string(),
            archive_name.to_string(),
            file_size,
            buffer.to_vec(),
        ));
        for byte in buffer.iter_mut() {
            *byte ^= key;
        }
    };

    let plain = archive
        .read_with_filter(&fs, &file, "dir/packed.bin", "data.xp3", Some(&filter))
        .expect("filtered read");
    assert_eq!(plain, plain_zlib);

    let calls = calls.borrow();
    assert_eq!(calls.len(), 1);
    let (hash, offset, file_name, archive_name, file_size, before) = &calls[0];
    assert_eq!(*hash, 0, "build_xp3 writes a zero adlr hash");
    assert_eq!(*offset, 0);
    assert_eq!(file_name, "dir/packed.bin");
    assert_eq!(archive_name, "data.xp3");
    assert_eq!(*file_size, plain_zlib.len() as u64);
    assert_eq!(before, &cipher_zlib);
}

/// The filter receives the **normalized** in-archive name, not the raw stored
/// spelling: the archive is opened with `normalizeName=true`, so `GetName`
/// returns the name `Init` normalized.
// Reference: `reference/cpp/core/archive/xp3/XP3Archive.cpp:435-437`
// (`NormalizeInArchiveStorageName(item.Name)`) and `:996-1002`
// (`Owner->GetName(StorageIndex)`).
#[test]
fn extraction_filter_receives_the_normalized_name() {
    use std::cell::RefCell;

    let xp3 = build_xp3(&[(r"Dir\Packed.BIN", b"payload", false)]);
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open archive");
    let seen = RefCell::new(String::new());
    let filter = |_hash: u32,
                  _offset: u64,
                  _buffer: &mut [u8],
                  file_name: &str,
                  _archive_name: &str,
                  _file_size: u64| {
        *seen.borrow_mut() = file_name.to_string();
    };

    let data = archive
        .read_with_filter(&fs, &file, r"Dir\Packed.BIN", "data.xp3", Some(&filter))
        .expect("filtered read");
    assert_eq!(data, b"payload");
    assert_eq!(&*seen.borrow(), "dir/packed.bin");
}

/// Multiple segments advance the filter's uncompressed offset, so a streaming
/// read and a whole-entry read see the same `(offset, buffer)` sequence.
#[test]
fn extraction_filter_offset_advances_across_segments() {
    use std::cell::RefCell;

    let first = b"AAAA";
    let second = b"BBBBBB";
    let mut file = Vec::new();
    file.extend_from_slice(&XP3_MAGIC);
    file.extend_from_slice(&0u64.to_le_bytes());
    let first_start = file.len() as u64;
    file.extend_from_slice(first);
    let second_start = file.len() as u64;
    file.extend_from_slice(second);

    let units: Vec<u16> = "two.bin".encode_utf16().collect();
    let mut info = Vec::new();
    info.extend_from_slice(&0u32.to_le_bytes());
    info.extend_from_slice(&((first.len() + second.len()) as u64).to_le_bytes());
    info.extend_from_slice(&((first.len() + second.len()) as u64).to_le_bytes());
    info.extend_from_slice(&(units.len() as i16).to_le_bytes());
    for unit in &units {
        info.extend_from_slice(&unit.to_le_bytes());
    }
    let mut segm = Vec::new();
    for (start, data) in [(first_start, first.as_slice()), (second_start, second)] {
        segm.extend_from_slice(&0u32.to_le_bytes());
        segm.extend_from_slice(&start.to_le_bytes());
        segm.extend_from_slice(&(data.len() as u64).to_le_bytes());
        segm.extend_from_slice(&(data.len() as u64).to_le_bytes());
    }

    let mut body = Vec::new();
    body.extend_from_slice(b"info");
    body.extend_from_slice(&(info.len() as u64).to_le_bytes());
    body.extend_from_slice(&info);
    body.extend_from_slice(b"segm");
    body.extend_from_slice(&(segm.len() as u64).to_le_bytes());
    body.extend_from_slice(&segm);
    body.extend_from_slice(b"adlr");
    body.extend_from_slice(&4u64.to_le_bytes());
    body.extend_from_slice(&7u32.to_le_bytes());

    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"File");
    chunk.extend_from_slice(&(body.len() as u64).to_le_bytes());
    chunk.extend_from_slice(&body);
    let index_ofs = file.len() as u64;
    file.push(0u8);
    file.extend_from_slice(&(chunk.len() as u64).to_le_bytes());
    file.extend_from_slice(&chunk);
    file[11..19].copy_from_slice(&index_ofs.to_le_bytes());

    let (fs, file) = fs_with(&file);
    let archive = Xp3Archive::open(&fs, &file).expect("open archive");
    let calls = RefCell::new(Vec::new());
    let filter = |_hash: u32,
                  offset: u64,
                  buffer: &mut [u8],
                  _file_name: &str,
                  _archive_name: &str,
                  _file_size: u64| {
        calls.borrow_mut().push((offset, buffer.len()));
    };

    assert_eq!(
        archive
            .read_with_filter(&fs, &file, "two.bin", "two.xp3", Some(&filter))
            .unwrap(),
        b"AAAABBBBBB"
    );
    assert_eq!(*calls.borrow(), vec![(0, 4), (4, 6)]);
}

// ---------------------------------------------------------------------------
// Prefix reads (`Xp3Archive::read_prefix_with_filter`).
//
// The prefix API is our own capability, not a reference behaviour: on this
// path the reference reads whole files. These tests pin the properties the
// middle-layer wrapper relies on — correct leading bytes, in-order
// concatenation across segments, short results, and the same typed errors as
// the whole-entry read.
// ---------------------------------------------------------------------------

/// Build a raw-index XP3 archive holding one entry made of several raw
/// segments, so a read must concatenate them in order. Layout mirrors
/// `testkit::build_xp3`; the index is stored raw.
fn build_xp3_raw_segments(name: &str, segments: &[&[u8]]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(&XP3_MAGIC);
    file.extend_from_slice(&0u64.to_le_bytes());

    let mut segm = Vec::new();
    let mut org_size = 0u64;
    let mut arc_size = 0u64;
    for data in segments {
        let start = file.len() as u64;
        file.extend_from_slice(data);
        segm.extend_from_slice(&SEGM_ENCODE_RAW.to_le_bytes());
        segm.extend_from_slice(&start.to_le_bytes());
        segm.extend_from_slice(&(data.len() as u64).to_le_bytes());
        segm.extend_from_slice(&(data.len() as u64).to_le_bytes());
        org_size += data.len() as u64;
        arc_size += data.len() as u64;
    }

    let units: Vec<u16> = name.encode_utf16().collect();
    let mut info = Vec::new();
    info.extend_from_slice(&0u32.to_le_bytes());
    info.extend_from_slice(&org_size.to_le_bytes());
    info.extend_from_slice(&arc_size.to_le_bytes());
    info.extend_from_slice(&(units.len() as i16).to_le_bytes());
    for unit in &units {
        info.extend_from_slice(&unit.to_le_bytes());
    }

    let mut body = Vec::new();
    body.extend_from_slice(b"info");
    body.extend_from_slice(&(info.len() as u64).to_le_bytes());
    body.extend_from_slice(&info);
    body.extend_from_slice(b"segm");
    body.extend_from_slice(&(segm.len() as u64).to_le_bytes());
    body.extend_from_slice(&segm);
    body.extend_from_slice(b"adlr");
    body.extend_from_slice(&4u64.to_le_bytes());
    body.extend_from_slice(&0u32.to_le_bytes());

    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"File");
    chunk.extend_from_slice(&(body.len() as u64).to_le_bytes());
    chunk.extend_from_slice(&body);

    let index_ofs = file.len() as u64;
    file.push(0u8); // raw index block
    file.extend_from_slice(&(chunk.len() as u64).to_le_bytes());
    file.extend_from_slice(&chunk);
    file[11..19].copy_from_slice(&index_ofs.to_le_bytes());
    file
}

/// A raw entry split across segments: the prefix is the leading bytes and the
/// segments are concatenated in order.
#[test]
fn read_prefix_concatenates_raw_segments_in_order() {
    let xp3 = build_xp3_raw_segments("multi.bin", &[b"AAAA", b"BBBBBB", b"CC"]);
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open archive");

    assert_eq!(
        archive
            .read_prefix_with_filter(&fs, &file, "multi.bin", "", 4, None)
            .unwrap(),
        b"AAAA"
    );
    assert_eq!(
        archive
            .read_prefix_with_filter(&fs, &file, "multi.bin", "", 6, None)
            .unwrap(),
        b"AAAABB"
    );
    assert_eq!(
        archive
            .read_prefix_with_filter(&fs, &file, "multi.bin", "", 12, None)
            .unwrap(),
        b"AAAABBBBBBCC"
    );
    assert_eq!(
        archive
            .read_prefix_with_filter(&fs, &file, "multi.bin", "", 64, None)
            .unwrap(),
        b"AAAABBBBBBCC"
    );
}

/// A prefix that spans a segment boundary is exactly the leading slice of the
/// whole-entry read, for every limit from zero to past the end.
#[test]
fn read_prefix_matches_a_truncated_whole_read() {
    let xp3 = build_xp3_raw_segments("span.bin", &[b"0123456789", b"abcdefghij", b"XYZ"]);
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open archive");
    let entry = archive.entry("span.bin").expect("entry");
    let whole = archive.read_entry(&fs, &file, entry).unwrap();
    assert_eq!(whole, b"0123456789abcdefghijXYZ");

    for max_len in [0usize, 1, 5, 9, 10, 11, 15, 20, 23, 24, 100] {
        let prefix = archive
            .read_prefix_with_filter(&fs, &file, "span.bin", "", max_len, None)
            .unwrap();
        assert_eq!(
            prefix.as_slice(),
            &whole[..max_len.min(whole.len())],
            "max_len {max_len}"
        );
    }
}

/// An entry shorter than `max_len` returns short, and a zero limit returns
/// empty. `read_prefix_with_filter` is our own API and the caller decides
/// whether a short result is an error.
#[test]
fn read_prefix_returns_short_entries_short() {
    let xp3 = build_xp3(&[("tiny.bin", b"hi", false)]);
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open archive");

    assert_eq!(
        archive
            .read_prefix_with_filter(&fs, &file, "tiny.bin", "", 64, None)
            .unwrap(),
        b"hi"
    );
    assert!(
        archive
            .read_prefix_with_filter(&fs, &file, "tiny.bin", "", 0, None)
            .unwrap()
            .is_empty()
    );
}

/// A zlib-backed entry's prefix is the leading bytes of its whole read.
#[test]
fn read_prefix_reads_a_zlib_entry() {
    let payload = b"zlib compressed leading bytes";
    let xp3 = build_xp3(&[("packed.bin", payload, true)]);
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open archive");
    let entry = archive.entry("packed.bin").expect("entry");

    assert_eq!(archive.read_entry(&fs, &file, entry).unwrap(), payload);
    assert_eq!(
        archive
            .read_prefix_with_filter(&fs, &file, "packed.bin", "", 5, None)
            .unwrap(),
        b"zlib "
    );
}

/// A genuine zlib decode failure is the same typed error the whole-entry read
/// produces. The single segment of `bad.bin` starts at offset 19 (11-byte
/// magic + 8-byte index-offset slot).
#[test]
fn read_prefix_reports_a_zlib_failure() {
    let payload = vec![0x5au8; 4096];
    let mut xp3 = build_xp3(&[("bad.bin", &payload, true)]);
    xp3[19] ^= 0xff; // corrupt the zlib header
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open archive");

    let err = archive
        .read_prefix_with_filter(&fs, &file, "bad.bin", "", 64, None)
        .unwrap_err();
    assert!(
        matches!(err, Xp3Error::Inflate(_)),
        "unexpected error: {err:?}"
    );
}

/// A prefix read preserves a bottom-layer read failure as the original typed
/// `FsError`, exactly like the whole-entry read.
#[test]
fn read_prefix_preserves_a_bottom_read_failure() {
    let xp3 = build_xp3(&[("gone.bin", b"payload", false)]);
    let (fs, file) = fs_with(&xp3);
    let archive = Xp3Archive::open(&fs, &file).expect("open archive");
    fs::remove_file(&fs, &FsPath::host("/bad.xp3")).expect("remove backing file");

    let err = archive
        .read_prefix_with_filter(&fs, &file, "gone.bin", "", 4, None)
        .unwrap_err();
    assert!(
        matches!(err, Xp3Error::Fs(FsError::NotFound { .. })),
        "typed FsError was not preserved: {err:?}"
    );
}
