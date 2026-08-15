//! Integration tests for the XP3 reader, building synthetic archives in
//! memory with the exact byte layout the reader expects.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use flate2::Compression;
use flate2::write::ZlibEncoder;

use xp3::{
    Error, FILE_PROTECTED, INDEX_ENCODE_RAW, INDEX_ENCODE_ZLIB, SEGM_ENCODE_RAW, SEGM_ENCODE_ZLIB,
    XP3_MAGIC, Xp3Archive,
};

// ---------------------------------------------------------------------------
// Archive builder: writes the exact XP3 byte layout.
//
//   [0..11)   magic
//   [11..19)  u64 LE index offset (patched by `finish`)
//   ...       segment data
//   index block: 1 flag byte, then for raw:
//       u64 LE index_size + index bytes
//     for zlib:
//       u64 LE compressed_size + u64 LE real_size + zlib stream
//   index data = chunks [4-byte tag][u64 LE size][payload]
// ---------------------------------------------------------------------------

/// One 28-byte `segm` entry as stored in the index.
#[derive(Clone)]
struct Seg {
    flags: u32,
    start: u64,
    org_size: u64,
    arc_size: u64,
}

struct Builder {
    file: Vec<u8>,
    file_chunks: Vec<Vec<u8>>,
}

impl Builder {
    fn new() -> Self {
        let mut file = Vec::new();
        file.extend_from_slice(&XP3_MAGIC);
        file.extend_from_slice(&0u64.to_le_bytes()); // index-offset slot, patched in finish()
        Self {
            file,
            file_chunks: Vec::new(),
        }
    }

    /// Append a raw segment's bytes to the archive file.
    fn raw_segment(&mut self, bytes: &[u8]) -> Seg {
        let start = self.file.len() as u64;
        self.file.extend_from_slice(bytes);
        Seg {
            flags: SEGM_ENCODE_RAW,
            start,
            org_size: bytes.len() as u64,
            arc_size: bytes.len() as u64,
        }
    }

    /// Append a zlib-compressed segment to the archive file.
    fn zlib_segment(&mut self, bytes: &[u8]) -> Seg {
        let compressed = zlib(bytes);
        let start = self.file.len() as u64;
        self.file.extend_from_slice(&compressed);
        Seg {
            flags: SEGM_ENCODE_ZLIB,
            start,
            org_size: bytes.len() as u64,
            arc_size: compressed.len() as u64,
        }
    }

    /// Add a `File` chunk for one entry.
    fn add_file(&mut self, name: &str, flags: u32, hash: u32, segments: Vec<Seg>) {
        let org_size: u64 = segments.iter().map(|s| s.org_size).sum();
        let arc_size: u64 = segments.iter().map(|s| s.arc_size).sum();
        let parts = vec![
            info_chunk(flags, org_size, arc_size, name),
            segm_chunk(&segments),
            aldr_chunk(hash),
        ];
        self.file_chunks.push(file_chunk(&parts));
    }

    /// Finish the archive. `index_flags` selects raw (`INDEX_ENCODE_RAW`) or
    /// zlib (`INDEX_ENCODE_ZLIB`) storage of the index block.
    fn finish(self, index_flags: u8) -> Vec<u8> {
        let mut index_data = Vec::new();
        for fc in &self.file_chunks {
            index_data.extend_from_slice(fc);
        }

        let mut out = self.file;
        let index_ofs = out.len() as u64;

        out.push(index_flags);
        match index_flags & 0x07 {
            INDEX_ENCODE_RAW => {
                out.extend_from_slice(&(index_data.len() as u64).to_le_bytes());
                out.extend_from_slice(&index_data);
            }
            INDEX_ENCODE_ZLIB => {
                let compressed = zlib(&index_data);
                out.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
                out.extend_from_slice(&(index_data.len() as u64).to_le_bytes());
                out.extend_from_slice(&compressed);
            }
            other => panic!("unsupported index flag bits: 0x{other:02x}"),
        }

        out[11..19].copy_from_slice(&index_ofs.to_le_bytes());
        out
    }
}

/// `info` sub-chunk: u32 flags, i64 org_size, i64 arc_size, i16 name_len
/// (UTF-16 code units), name as UTF-16LE.
fn info_chunk(flags: u32, org_size: u64, arc_size: u64, name: &str) -> Vec<u8> {
    let units: Vec<u16> = name.encode_utf16().collect();
    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"info");
    chunk.extend_from_slice(&((22 + units.len() * 2) as u64).to_le_bytes());
    chunk.extend_from_slice(&flags.to_le_bytes());
    chunk.extend_from_slice(&(org_size as i64).to_le_bytes());
    chunk.extend_from_slice(&(arc_size as i64).to_le_bytes());
    chunk.extend_from_slice(&(units.len() as i16).to_le_bytes());
    for u in units {
        chunk.extend_from_slice(&u.to_le_bytes());
    }
    chunk
}

/// `segm` sub-chunk: 28-byte segments (u32 flags, i64 start, i64 org_size,
/// i64 arc_size).
fn segm_chunk(segments: &[Seg]) -> Vec<u8> {
    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"segm");
    chunk.extend_from_slice(&((segments.len() * 28) as u64).to_le_bytes());
    for s in segments {
        chunk.extend_from_slice(&s.flags.to_le_bytes());
        chunk.extend_from_slice(&(s.start as i64).to_le_bytes());
        chunk.extend_from_slice(&(s.org_size as i64).to_le_bytes());
        chunk.extend_from_slice(&(s.arc_size as i64).to_le_bytes());
    }
    chunk
}

/// `aldr` sub-chunk: u32 hash.
fn aldr_chunk(hash: u32) -> Vec<u8> {
    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"aldr");
    chunk.extend_from_slice(&4u64.to_le_bytes());
    chunk.extend_from_slice(&hash.to_le_bytes());
    chunk
}

/// A `File` chunk wrapping its sub-chunks.
fn file_chunk(sub_chunks: &[Vec<u8>]) -> Vec<u8> {
    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"File");
    let size: usize = sub_chunks.iter().map(|c| c.len()).sum();
    chunk.extend_from_slice(&(size as u64).to_le_bytes());
    for c in sub_chunks {
        chunk.extend_from_slice(c);
    }
    chunk
}

fn zlib(data: &[u8]) -> Vec<u8> {
    let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
    enc.write_all(data).unwrap();
    enc.finish().unwrap()
}

// ---------------------------------------------------------------------------
// Temp-file helpers (the reader opens archives from paths).
// ---------------------------------------------------------------------------

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn write_temp(bytes: &[u8]) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let thread = std::thread::current()
        .name()
        .unwrap_or("t")
        .replace([' ', ':'], "_");
    let path = std::env::temp_dir().join(format!(
        "xp3-rs-test-{}-{thread}-{n}.xp3",
        std::process::id()
    ));
    std::fs::write(&path, bytes).unwrap();
    path
}

/// Open an archive built from `bytes`, run `f`, and clean up the temp file.
fn with_archive(bytes: &[u8], f: impl FnOnce(&mut Xp3Archive)) {
    let path = write_temp(bytes);
    let mut archive = Xp3Archive::open(&path).expect("archive should open");
    f(&mut archive);
    let _ = std::fs::remove_file(&path);
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn raw_index_single_raw_segment_round_trip() {
    let mut b = Builder::new();
    let seg = b.raw_segment(b"Hello, XP3 world!");
    b.add_file("test.txt", 0, 0, vec![seg]);
    let bytes = b.finish(INDEX_ENCODE_RAW);

    with_archive(&bytes, |arc| {
        assert_eq!(arc.len(), 1);
        let entry = arc.entry("test.txt").expect("entry present");
        assert_eq!(entry.name, "test.txt");
        assert_eq!(entry.raw_name, "test.txt");
        assert_eq!(entry.org_size, 17);
        assert_eq!(entry.arc_size, 17);
        assert_eq!(entry.segments.len(), 1);
        assert_eq!(entry.file_hash, 0);
        assert_eq!(arc.read("test.txt").unwrap(), b"Hello, XP3 world!");
    });
}

#[test]
fn zlib_compressed_index() {
    let mut b = Builder::new();
    for i in 0..100 {
        let seg = b.raw_segment(format!("content of file {i:03}").as_bytes());
        b.add_file(&format!("file_{i:03}.dat"), 0, 0, vec![seg]);
    }
    let bytes = b.finish(INDEX_ENCODE_ZLIB);

    // Sanity-check the stored layout: flag byte 1, compressed/real sizes,
    // then a zlib stream (0x78 header). The index must actually shrink.
    let index_ofs = u64::from_le_bytes(bytes[11..19].try_into().unwrap()) as usize;
    assert_eq!(bytes[index_ofs], INDEX_ENCODE_ZLIB);
    let compressed_size =
        u64::from_le_bytes(bytes[index_ofs + 1..index_ofs + 9].try_into().unwrap());
    let real_size = u64::from_le_bytes(bytes[index_ofs + 9..index_ofs + 17].try_into().unwrap());
    assert_eq!(bytes[index_ofs + 17], 0x78, "zlib header expected");
    assert!(compressed_size < real_size, "index should compress");

    with_archive(&bytes, |arc| {
        assert_eq!(arc.len(), 100);
        assert_eq!(arc.read("file_042.dat").unwrap(), b"content of file 042");
        assert_eq!(arc.read("FILE_000.DAT").unwrap(), b"content of file 000");
    });
}

#[test]
fn zlib_compressed_segment() {
    let mut b = Builder::new();
    let text = b"The quick brown fox jumps over the lazy dog. ".repeat(100);
    let seg = b.zlib_segment(&text);
    b.add_file("compressme.bin", 0, 0, vec![seg]);
    let bytes = b.finish(INDEX_ENCODE_RAW);

    with_archive(&bytes, |arc| {
        let entry = arc.entry("compressme.bin").unwrap();
        assert_eq!(entry.org_size, text.len() as u64);
        assert!(
            entry.arc_size < entry.org_size,
            "zlib segment should shrink (arc {} < org {})",
            entry.arc_size,
            entry.org_size
        );
        assert_eq!(arc.read("compressme.bin").unwrap(), text);
    });
}

#[test]
fn multi_segment_file() {
    let mut b = Builder::new();
    let s1 = b.raw_segment(b"first part|");
    let s2 = b.zlib_segment(b"second part (compressed)|");
    let s3 = b.raw_segment(b"third part");
    b.add_file("multi.bin", 0, 0, vec![s1, s2, s3]);
    let bytes = b.finish(INDEX_ENCODE_RAW);

    with_archive(&bytes, |arc| {
        let entry = arc.entry("multi.bin").unwrap();
        assert_eq!(entry.segments.len(), 3);
        assert_eq!(
            entry.arc_size,
            entry.segments.iter().map(|s| s.arc_size).sum::<u64>()
        );
        assert_eq!(
            entry.org_size,
            entry.segments.iter().map(|s| s.org_size).sum::<u64>()
        );
        let data = arc.read("multi.bin").unwrap();
        assert_eq!(data, b"first part|second part (compressed)|third part");
    });
}

#[test]
fn unicode_file_names() {
    let mut b = Builder::new();
    let seg1 = b.raw_segment(b"nihongo");
    b.add_file("日本語のファイル.txt", 0, 0, vec![seg1]);
    let seg2 = b.raw_segment(b"cyrillic");
    b.add_file("Данные.библ", 0, 0, vec![seg2]);
    let seg3 = b.raw_segment(b"mixed case");
    b.add_file("日本語File.TXT", 0, 0, vec![seg3]);
    let bytes = b.finish(INDEX_ENCODE_RAW);

    with_archive(&bytes, |arc| {
        assert_eq!(arc.len(), 3);
        assert_eq!(arc.read("日本語のファイル.txt").unwrap(), b"nihongo");
        assert_eq!(arc.read("Данные.библ").unwrap(), b"cyrillic");
        // ASCII letters in the name are normalized to lowercase.
        let entry = arc.entry("日本語FILE.TXT").unwrap();
        assert_eq!(entry.name, "日本語file.txt");
        assert_eq!(arc.read("日本語FILE.TXT").unwrap(), b"mixed case");
    });
}

#[test]
fn name_normalization_uppercase_and_backslashes() {
    let mut b = Builder::new();
    let seg1 = b.raw_segment(b"init");
    b.add_file("Startup.tjs", 0, 0, vec![seg1]);
    let seg2 = b.raw_segment(b"img");
    b.add_file(r"data\bg\a.jpg", 0, 0, vec![seg2]);
    let seg3 = b.raw_segment(b"ogg");
    b.add_file("Data//BGM//Track1.ogg", 0, 0, vec![seg3]);
    let bytes = b.finish(INDEX_ENCODE_RAW);

    with_archive(&bytes, |arc| {
        // Stored uppercase name is normalized; lookup by uppercase works.
        let entry = arc.entry("STARTUP.TJS").expect("case-insensitive lookup");
        assert_eq!(entry.name, "startup.tjs");
        assert_eq!(entry.raw_name, "Startup.tjs");
        assert_eq!(arc.read("STARTUP.TJS").unwrap(), b"init");

        // Backslashes stored in the name normalize to slashes.
        assert!(arc.entry("data/bg/a.jpg").is_some());
        assert_eq!(arc.read(r"DATA\BG\A.JPG").unwrap(), b"img");

        // Duplicate slashes are collapsed.
        let entry = arc.entry("Data/BGM//Track1.ogg").unwrap();
        assert_eq!(entry.name, "data/bgm/track1.ogg");
        assert_eq!(arc.read("data/bgm/track1.ogg").unwrap(), b"ogg");
    });
}

#[test]
fn entries_are_sorted_by_normalized_name() {
    let mut b = Builder::new();
    let seg1 = b.raw_segment(b"z");
    b.add_file("zebra.txt", 0, 0, vec![seg1]);
    let seg2 = b.raw_segment(b"a");
    b.add_file("Alpha.txt", 0, 0, vec![seg2]);
    let seg3 = b.raw_segment(b"b");
    b.add_file(r"beta\file.txt", 0, 0, vec![seg3]);
    let bytes = b.finish(INDEX_ENCODE_RAW);

    with_archive(&bytes, |arc| {
        let names: Vec<&str> = arc.entries().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["alpha.txt", "beta/file.txt", "zebra.txt"]);
    });
}

#[test]
fn entry_not_found() {
    let mut b = Builder::new();
    let seg = b.raw_segment(b"x");
    b.add_file("present.txt", 0, 0, vec![seg]);
    let bytes = b.finish(INDEX_ENCODE_RAW);

    with_archive(&bytes, |arc| {
        assert!(arc.entry("absent.txt").is_none());
        match arc.read("absent.txt") {
            Err(Error::NotFound(name)) => assert_eq!(name, "absent.txt"),
            other => panic!("expected Error::NotFound, got {other:?}"),
        }
    });
}

#[test]
fn garbage_magic_is_not_an_xp3() {
    // First 11 bytes are not the XP3 magic.
    let path = write_temp(b"this is definitely not an xp3 archive at all!!!");
    let err = open_err(&path);
    let _ = std::fs::remove_file(&path);
    assert!(matches!(err, Error::NotAnXp3(_)));

    // A file too short to even hold the magic is also rejected.
    let path = write_temp(b"short");
    let err = open_err(&path);
    let _ = std::fs::remove_file(&path);
    assert!(matches!(err, Error::NotAnXp3(_)));
}

#[test]
fn protected_file_is_readable() {
    let mut b = Builder::new();
    let seg = b.raw_segment(b"top secret");
    b.add_file("secret.dat", FILE_PROTECTED, 0, vec![seg]);
    let bytes = b.finish(INDEX_ENCODE_RAW);

    with_archive(&bytes, |arc| {
        // The entry is listed and read normally: the reference emulator sets
        // TVPAllowExtractProtectedStorage=true, so DRM-flagged entries are
        // readable (the bit only blocks extraction tooling).
        assert!(arc.entry("secret.dat").is_some());
        let data = arc.read("secret.dat").expect("protected entry is readable");
        assert_eq!(data, b"top secret");
        let entry = arc.entry("secret.dat").unwrap();
        assert_ne!(entry.flags & FILE_PROTECTED, 0, "flag bit is still exposed");
    });
}

#[test]
fn archive_without_file_chunks_is_empty() {
    // An index block that contains no File chunks: opens fine, zero entries.
    let b = Builder::new();
    let bytes = b.finish(INDEX_ENCODE_RAW);
    with_archive(&bytes, |arc| {
        assert!(arc.is_empty());
        assert_eq!(arc.len(), 0);
        assert!(matches!(arc.read("anything"), Err(Error::NotFound(_))));
    });

    // An index offset of 0 with no entries read so far is Error::NoIndex.
    let mut no_index = Vec::new();
    no_index.extend_from_slice(&XP3_MAGIC);
    no_index.extend_from_slice(&0u64.to_le_bytes());
    let path = write_temp(&no_index);
    let err = open_err(&path);
    let _ = std::fs::remove_file(&path);
    assert!(matches!(err, Error::NoIndex));
}

/// Open `path` expecting failure, returning the error.
fn open_err(path: &Path) -> Error {
    match Xp3Archive::open(path) {
        Err(e) => e,
        Ok(_) => panic!("expected open to fail: {}", path.display()),
    }
}
