//! In-memory XP3 archive fixtures.
//!
//! The builders produce the exact byte layout the archive reader expects, so
//! a test can mount an archive without a file on disk.

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use fs::xp3::{INDEX_ENCODE_RAW, INDEX_ENCODE_ZLIB, SEGM_ENCODE_RAW, SEGM_ENCODE_ZLIB, XP3_MAGIC};

/// Build a minimal raw-index XP3 archive in memory.
///
/// Each entry is `(name, data, zlib)`: `zlib` selects a zlib-compressed
/// segment, otherwise the segment is stored raw. The index itself is stored
/// raw; see [`build_xp3_compressed_index`] for the zlib-compressed index.
///
/// The bytes follow the reference reader `tTVPXP3Archive::Init`: the index
/// offset is an `i64` at byte 11
/// (`reference/cpp/core/archive/xp3/XP3Archive.cpp:340-343`); a raw index
/// block is a `TVP_XP3_INDEX_ENCODE_RAW` flag byte then an `i64` size
/// (`:392-399`); the index data is `File` chunks containing `info`
/// (`flags:u32 org:i64 arc:i64 namelen:i16 name:u16[]`, `:424-436`), `segm`
/// (a run of the 28-byte `flags:u32 start:i64 org:i64 arc:i64` record,
/// `:451-476`) and `adlr` (`:483-489`).
pub fn build_xp3(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
    build_xp3_with_index(entries, false)
}

/// Build a minimal XP3 archive in memory whose index block is
/// zlib-compressed (`TVP_XP3_INDEX_ENCODE_ZLIB`).
///
/// A compressed index block is a flag byte with `0x01` in the encode-method
/// bits, an `i64` compressed size, an `i64` uncompressed size, then the zlib
/// stream (`reference/cpp/core/archive/xp3/XP3Archive.cpp:360-390`). The
/// entries follow [`build_xp3`].
pub fn build_xp3_compressed_index(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
    build_xp3_with_index(entries, true)
}

fn build_xp3_with_index(entries: &[(&str, &[u8], bool)], compressed_index: bool) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(&XP3_MAGIC);
    file.extend_from_slice(&0u64.to_le_bytes()); // index-offset slot

    let mut file_chunks: Vec<Vec<u8>> = Vec::new();
    for (name, data, zlib) in entries {
        let (stored, flags): (Vec<u8>, u32) = if *zlib {
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
            enc.write_all(data).expect("zlib compress");
            (enc.finish().expect("zlib finish"), SEGM_ENCODE_ZLIB)
        } else {
            (data.to_vec(), SEGM_ENCODE_RAW)
        };
        let start = file.len() as u64;
        file.extend_from_slice(&stored);

        let units: Vec<u16> = name.encode_utf16().collect();
        let mut info = Vec::new();
        info.extend_from_slice(b"info");
        info.extend_from_slice(&((22 + units.len() * 2) as u64).to_le_bytes());
        info.extend_from_slice(&0u32.to_le_bytes()); // flags
        info.extend_from_slice(&(data.len() as i64).to_le_bytes());
        info.extend_from_slice(&(stored.len() as i64).to_le_bytes());
        info.extend_from_slice(&(units.len() as i16).to_le_bytes());
        for u in &units {
            info.extend_from_slice(&u.to_le_bytes());
        }

        let mut segm = Vec::new();
        segm.extend_from_slice(b"segm");
        segm.extend_from_slice(&28u64.to_le_bytes());
        segm.extend_from_slice(&flags.to_le_bytes());
        segm.extend_from_slice(&(start as i64).to_le_bytes());
        segm.extend_from_slice(&(data.len() as i64).to_le_bytes());
        segm.extend_from_slice(&(stored.len() as i64).to_le_bytes());

        let mut adlr = Vec::new();
        adlr.extend_from_slice(b"adlr");
        adlr.extend_from_slice(&4u64.to_le_bytes());
        adlr.extend_from_slice(&0u32.to_le_bytes());

        let mut fc = Vec::new();
        fc.extend_from_slice(b"File");
        fc.extend_from_slice(&((info.len() + segm.len() + adlr.len()) as u64).to_le_bytes());
        fc.extend_from_slice(&info);
        fc.extend_from_slice(&segm);
        fc.extend_from_slice(&adlr);
        file_chunks.push(fc);
    }

    let mut index_data = Vec::new();
    for fc in &file_chunks {
        index_data.extend_from_slice(fc);
    }
    let index_ofs = file.len() as u64;
    if compressed_index {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&index_data).expect("zlib compress index");
        let compressed = enc.finish().expect("zlib finish");
        file.push(INDEX_ENCODE_ZLIB);
        file.extend_from_slice(&(compressed.len() as u64).to_le_bytes());
        file.extend_from_slice(&(index_data.len() as u64).to_le_bytes());
        file.extend_from_slice(&compressed);
    } else {
        file.push(INDEX_ENCODE_RAW);
        file.extend_from_slice(&(index_data.len() as u64).to_le_bytes());
        file.extend_from_slice(&index_data);
    }
    file[11..19].copy_from_slice(&index_ofs.to_le_bytes());
    file
}
