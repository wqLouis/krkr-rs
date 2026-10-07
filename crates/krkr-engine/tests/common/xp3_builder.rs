//! Minimal in-memory XP3 archive builder shared by the integration tests.
//!
//! Builds the exact raw-index byte layout that [`krkr_fs::xp3::Xp3Archive`]
//! expects, so tests can mount synthetic archives without shipping binary
//! fixtures.

use std::io::Write;

use flate2::Compression;
use flate2::write::ZlibEncoder;

use krkr_fs::xp3::{SEGM_ENCODE_RAW, SEGM_ENCODE_ZLIB, XP3_MAGIC};

/// Build a minimal raw-index XP3 archive in memory.
///
/// Each entry is `(name, data, zlib)`: `zlib` selects a zlib-compressed
/// segment, otherwise the segment is stored raw. The index itself is always
/// stored raw. Layout matches what [`krkr_fs::xp3::Xp3Archive`] expects.
pub fn build_xp3(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
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

        let mut aldr = Vec::new();
        aldr.extend_from_slice(b"aldr");
        aldr.extend_from_slice(&4u64.to_le_bytes());
        aldr.extend_from_slice(&0u32.to_le_bytes());

        let mut fc = Vec::new();
        fc.extend_from_slice(b"File");
        fc.extend_from_slice(&((info.len() + segm.len() + aldr.len()) as u64).to_le_bytes());
        fc.extend_from_slice(&info);
        fc.extend_from_slice(&segm);
        fc.extend_from_slice(&aldr);
        file_chunks.push(fc);
    }

    let mut index_data = Vec::new();
    for fc in &file_chunks {
        index_data.extend_from_slice(fc);
    }
    let index_ofs = file.len() as u64;
    file.push(0u8); // raw index block
    file.extend_from_slice(&(index_data.len() as u64).to_le_bytes());
    file.extend_from_slice(&index_data);
    file[11..19].copy_from_slice(&index_ofs.to_le_bytes());
    file
}
