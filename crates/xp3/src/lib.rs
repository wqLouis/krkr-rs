//! Pure-Rust reader for the KiriKiri **XP3** archive format.
//!
//! This crate implements the same format semantics as the reference
//! implementation in `cpp/core/archive/xp3/XP3Archive.cpp` of the
//! krkr2 emulator, with no dependency on the C++ codebase.
//!
//! ## Format summary (as read by the reference implementation)
//!
//! * 11-byte magic: `"XP3\r\n \n\x1a\x8b\x67\x01"` (bytes
//!   `58 50 33 0D 0A 20 0A 1A 8B 67 01`).
//! * At offset 11: 8-byte little-endian *index offset* (relative to the
//!   archive start; for EXE-embedded archives the magic is searched on
//!   16-byte alignment and the offset is relative to the found magic).
//! * The index block: 1 flag byte + (raw or zlib-compressed) index data.
//!   Flag bits: low 3 = encode method (`0` raw, `1` zlib), `0x80` =
//!   continue (chain to another index block).
//! * Index data is a sequence of chunks `[4-byte tag][u32 LE size][data]`:
//!   - `File` chunks hold per-entry `info` / `segm` / `aldr` sub-chunks
//!   - `info`: `u32 flags`, `i64 org_size`, `i64 arc_size`,
//!     `i16 name_len` (UTF-16 code units), name (UTF-16LE)
//!   - `segm`: 28-byte segments: `u32 flags`, `i64 start` (absolute in
//!     archive), `i64 org_size`, `i64 arc_size`
//!   - `aldr`: `u32` name hash (adler32 of the normalized name)
//! * In-archive names are normalized: lowercased, `\` → `/`, duplicate
//!   slashes collapsed. Entries are stably sorted by normalized name.
//! * Reading a file concatenates its segments; zlib-flagged segments are
//!   inflated to `org_size`.

pub mod archive;
pub mod error;

pub use archive::{Entry, Xp3Archive};
pub use error::{Error, Result};

/// 11-byte XP3 magic: `"XP3\r\n \n\x1a\x8b\x67\x01"`.
pub const XP3_MAGIC: [u8; 11] = [
    0x58, 0x50, 0x33, // 'X','P','3'
    0x0d, 0x0a, 0x20, 0x0a, 0x1a, // \r \n ' ' \n EOF
    0x8b, 0x67, 0x01, // KANJI-code + version/coding
];

/// Index encode method mask (low 3 bits of the index flag byte).
pub const INDEX_ENCODE_METHOD_MASK: u8 = 0x07;
/// Index stored raw.
pub const INDEX_ENCODE_RAW: u8 = 0;
/// Index stored zlib-compressed.
pub const INDEX_ENCODE_ZLIB: u8 = 1;
/// Index continue bit — another index block follows.
pub const INDEX_CONTINUE: u8 = 0x80;

/// Entry protected bit — reading such entries is refused (DRM).
pub const FILE_PROTECTED: u32 = 1 << 31;

/// Segment encode method mask.
pub const SEGM_ENCODE_METHOD_MASK: u32 = 0x07;
/// Segment stored raw.
pub const SEGM_ENCODE_RAW: u32 = 0;
/// Segment stored zlib-compressed.
pub const SEGM_ENCODE_ZLIB: u32 = 1;

/// Normalize an in-archive storage name the way the reference engine does:
/// lowercase, `\` → `/`, collapse runs of `/` (a single leading `/` is kept).
pub fn normalize_in_archive_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut chars = name.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            'A'..='Z' => out.push(c.to_ascii_lowercase()),
            '\\' => out.push('/'),
            '/' => {
                out.push('/');
                while chars.peek() == Some(&'/') {
                    chars.next();
                }
            }
            _ => out.push(c),
        }
    }
    out
}
