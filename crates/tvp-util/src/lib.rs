//! tvp-util — pure-Rust ports of the KiriKiri **utils** module.
//!
//! This crate replaces the C++ utilities that the engine needs for text
//! handling, hashing and randomness, ported from `reference/cpp/core/`:
//!
//! - [`encoding`] — character set conversions (`base/CharacterSet.cpp` plus
//!   the encoding detection/naming from `base/TextStream.cpp`): UTF-8,
//!   UTF-16LE/BE, UTF-32LE/BE, CP932 (Windows-31J), GBK and ASCII, with
//!   BOM detection and the TJS `CharacterSet`-style `convert`/`encode`/
//!   `decode` API surface.
//! - [`random`] — the TVP environment-noise pseudo random generator
//!   (`utils/Random.cpp`): an MD5-hash-of-a-4KiB-seed-pool mixer.
//! - [`md5`] — MD5 message digest (RFC 1321), wrapping the `md-5` crate
//!   (`utils/md5.c` is a plain RFC 1321 implementation).
//! - [`misc`] — string/path helpers (`utils/StringUtil.h`,
//!   `utils/FilePathUtil.h`, `environ/Application.cpp` `ExtractFileDir`,
//!   TJS `ttstr::Replace` as `ReplaceStringAll`).

pub mod encoding;
pub mod md5;
pub mod misc;
pub mod random;

pub use encoding::{
    Encoding, EncodingError, convert, detect_bom, from_utf16, from_utf16le_bytes, is_valid_cp932,
    strip_bom, to_utf16, to_utf16le_bytes,
};
pub use md5::{Md5, digest, hex};
pub use misc::{
    change_file_ext, exclude_trailing_slash, extract_file_dir, extract_file_ext, extract_file_name,
    ieq, include_trailing_slash, replace_all, trim,
};
pub use random::Random;
