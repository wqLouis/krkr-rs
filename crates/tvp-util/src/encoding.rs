//! Character set conversions, ported from `base/CharacterSet.cpp` and the
//! encoding detection/naming in `base/TextStream.cpp`.
//!
//! # What is ported
//!
//! - `CharacterSet.cpp`'s UTF-8 ↔ UTF-16 conversions (as the low-level
//!   [`to_utf16`], [`from_utf16`] and friends). The C++ accepts loose 1–6
//!   byte UTF-8 sequences and truncates 4-byte code points to 16 bits; we
//!   use strict UTF-8 instead (surrogate validation, up to 4 bytes), like
//!   the reference's own `TextStream.cpp`, which decodes through
//!   `boost::locale::utf_to_utf`.
//! - `TextStream.cpp`'s `checkTextEncoding`: BOM detection order and the
//!   encoding names it produces (`"UTF-8"`, `"UTF-16LE"`, `"UTF-16BE"`,
//!   `"UTF-32LE"`, `"UTF-32BE"`, `"cp932"`, `"ASCII"`).
//! - The TJS `CharacterSet` native-class surface (`convert`/`encode`/
//!   `decode`) as free functions operating on byte slices; the script
//!   binding layer is responsible for TJS string marshaling.
//!
//! The C tables in `utils/encoding/` (GBK/JIS) and `utils/iconv/` are **not**
//! ported; CP932 and GBK go through the pure-Rust [`encoding_rs`] crate.
//!
//! # CP932 / Shift_JIS mapping caveat
//!
//! `encoding_rs::SHIFT_JIS` implements the **WHATWG** Shift_JIS encoding
//! (index-shift_jis). For three of the four JIS X 0208 "problem" code
//! points its **decoder** matches Windows-31J — the mapping KiriKiri
//! expects for CP932 — but for `81 7C` it picks a different code point
//! than the Unicode-consortium Windows-31J table:
//!
//! | bytes   | JIS X 0208    | Windows-31J (Unicode table) | WHATWG decode (`encoding_rs`) |
//! |---------|---------------|-----------------------------|-------------------------------|
//! | `81 60` | U+301C 〜      | U+FF5E ～                     | U+FF5E ～ ✓                    |
//! | `81 61` | U+2016 ‖      | U+2225 ∥                     | U+2225 ∥ ✓                    |
//! | `81 7C` | U+2014 —      | U+2015 ―                     | **U+FF0D －** (differs)        |
//! | `81 91` | U+00A2 ¢      | U+FFE0 ￠                     | U+FFE0 ￠ ✓                    |
//!
//! The **encoder** is stricter than Windows-31J: the WHATWG index replaced
//! the JIS code points above, so U+301C, U+2016 and U+2014 are **not
//! encodable** (`encoding_rs` reports them unmappable, and so does
//! [`Encoding::Cp932`]'s strict `encode`), while Windows-31J would emit
//! `81 60`/`81 61`/`81 7C`. What *is* encodable is the WHATWG side of each
//! pair (U+FF5E, U+2225, U+FF0D, U+FFE0); note that U+2015 (horizontal
//! bar) has its own JIS slot `81 5C` and is encodable in both mappings.
//!
//! Practical impact: CP932 game files decode mostly as KiriKiri expects;
//! only text containing byte `81 7C` (an em-dash-like glyph) differs —
//! U+FF0D here vs U+2015 on a Windows-31J system, both rendered as a
//! horizontal dash. The classic *wave dash* round-trip break only appears
//! when *writing* U+301C; test `wave_dash_round_trip_behavior` pins the
//! exact behavior. (For reference, Python's `cp932` codec is a third,
//! hybrid variant: it decodes `81 60`→U+301C and `81 61`→U+2016 like JIS,
//! `81 7C`→U+FF0D and `81 91`→U+FFE0 like WHATWG, and encodes both U+301C
//! and U+FF5E to `81 60`.)
//!
//! # GBK caveat
//!
//! `encoding_rs::GBK` is the WHATWG GBK, which extends GB2312 with the
//! CP936 areas and also accepts GB18030 4-byte sequences on decode. It maps
//! byte `80` to U+20AC (euro sign) as in CP936. Round-trips of ordinary
//! simplified-Chinese text match the reference `gbk2unicode.c` table.
//!
//! # Encoding names
//!
//! Names are case-insensitive and ignore `_`/`-`. Accepted: the
//! `TextStream.cpp` names above plus `"n"` (resolved to UTF-8, following
//! this reference's `G_DefaultReadEncoding = "UTF-8"`), `"SHIFT_JIS"`/
//! `"sjis"`/`"ms932"`/`"windows-31j"` (all CP932), `"gbk"`/`"cp936"`/
//! `"gb2312"` (GBK), `"us-ascii"`, and `"WINDOWS-1252"` (mapped to ASCII,
//! like `TextStream.cpp` does for uchardet results).

use std::fmt;

use encoding_rs::{GBK, SHIFT_JIS};

/// UTF-8 BOM (`EF BB BF`).
pub const UTF8_BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];
/// UTF-16LE BOM (`FF FE`).
pub const UTF16LE_BOM: [u8; 2] = [0xFF, 0xFE];
/// UTF-16BE BOM (`FE FF`).
pub const UTF16BE_BOM: [u8; 2] = [0xFE, 0xFF];
/// UTF-32LE BOM (`FF FE 00 00`).
pub const UTF32LE_BOM: [u8; 4] = [0xFF, 0xFE, 0x00, 0x00];
/// UTF-32BE BOM (`00 00 FE FF`).
pub const UTF32BE_BOM: [u8; 4] = [0x00, 0x00, 0xFE, 0xFF];

/// An encoding understood by the engine, mirroring the names produced by
/// the reference's `TextStream.cpp` encoding detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Encoding {
    /// UTF-8.
    Utf8,
    /// UTF-16 little-endian (the native `tjs_char` order).
    Utf16Le,
    /// UTF-16 big-endian.
    Utf16Be,
    /// UTF-32 little-endian.
    Utf32Le,
    /// UTF-32 big-endian.
    Utf32Be,
    /// CP932, i.e. Windows-31J (Shift_JIS + NEC/IBM extensions) — what
    /// KiriKiri expects for Japanese text files.
    Cp932,
    /// WHATWG GBK (GB2312 superset, CP936 areas).
    Gbk,
    /// Pure ASCII (bytes < 0x80).
    Ascii,
}

impl Encoding {
    /// Resolves an encoding name to an [`Encoding`]. Case-insensitive, `_`
    /// and `-` ignored; see the [module docs](self) for the accepted names.
    pub fn from_name(name: &str) -> Option<Encoding> {
        let n = name.trim().to_ascii_lowercase().replace(['_', '-'], "");
        Some(match n.as_str() {
            "utf8" | "unicode(utf8)" | "n" => Encoding::Utf8,
            "utf16le" | "utf16" => Encoding::Utf16Le,
            "utf16be" => Encoding::Utf16Be,
            "utf32le" | "utf32" => Encoding::Utf32Le,
            "utf32be" => Encoding::Utf32Be,
            "cp932" | "shiftjis" | "sjis" | "ms932" | "windows31j" | "csshiftjis" | "xsjis" => {
                Encoding::Cp932
            }
            "gbk" | "cp936" | "gb2312" | "ms936" | "windows936" => Encoding::Gbk,
            "ascii" | "usascii" | "windows1252" | "iso88591" | "latin1" => Encoding::Ascii,
            _ => return None,
        })
    }

    /// The canonical name, in the same strings `TextStream.cpp` produces
    /// and compares against (`"UTF-8"`, `"UTF-16LE"`, `"UTF-16BE"`,
    /// `"UTF-32LE"`, `"UTF-32BE"`, `"cp932"`, `"GBK"`, `"ASCII"`).
    pub fn name(self) -> &'static str {
        match self {
            Encoding::Utf8 => "UTF-8",
            Encoding::Utf16Le => "UTF-16LE",
            Encoding::Utf16Be => "UTF-16BE",
            Encoding::Utf32Le => "UTF-32LE",
            Encoding::Utf32Be => "UTF-32BE",
            Encoding::Cp932 => "cp932",
            Encoding::Gbk => "GBK",
            Encoding::Ascii => "ASCII",
        }
    }

    /// Decodes `bytes` in this encoding to a UTF-8 `String`.
    ///
    /// A leading BOM is **not** consumed (use [`strip_bom`] first, exactly
    /// like the reference strips the BOM before converting). Invalid byte
    /// sequences are an error, reported with the byte offset.
    pub fn decode(self, bytes: &[u8]) -> Result<String, EncodingError> {
        match self {
            Encoding::Utf8 => std::str::from_utf8(bytes).map(str::to_owned).map_err(|e| {
                EncodingError::InvalidUtf8 {
                    offset: e.valid_up_to(),
                }
            }),
            Encoding::Utf16Le => from_utf16le_bytes(bytes),
            Encoding::Utf16Be => from_utf16be_bytes(bytes),
            Encoding::Utf32Le => decode_utf32le(bytes),
            Encoding::Utf32Be => decode_utf32be(bytes),
            Encoding::Cp932 => decode_legacy(SHIFT_JIS, bytes, "cp932"),
            Encoding::Gbk => decode_legacy(GBK, bytes, "GBK"),
            Encoding::Ascii => match bytes.iter().position(|b| *b >= 0x80) {
                None => Ok(bytes.iter().map(|&b| b as char).collect()),
                Some(offset) => Err(EncodingError::InvalidSequence {
                    encoding: "ASCII",
                    offset,
                }),
            },
        }
    }

    /// Encodes `text` (UTF-8) in this encoding, strictly: a character that
    /// cannot be represented is an error (no silent U+FFFD/`?` replacement),
    /// reported with the offending character.
    pub fn encode(self, text: &str) -> Result<Vec<u8>, EncodingError> {
        match self {
            Encoding::Utf8 => Ok(text.as_bytes().to_vec()),
            Encoding::Utf16Le => Ok(to_utf16le_bytes(text)),
            Encoding::Utf16Be => Ok(to_utf16be_bytes(text)),
            Encoding::Utf32Le => Ok(encode_utf32le(text)),
            Encoding::Utf32Be => Ok(encode_utf32be(text)),
            Encoding::Cp932 => encode_legacy(SHIFT_JIS, text, "cp932"),
            Encoding::Gbk => encode_legacy(GBK, text, "GBK"),
            Encoding::Ascii => match text.find(|c: char| c as u32 >= 0x80) {
                None => Ok(text.as_bytes().to_vec()),
                Some(_) => Err(EncodingError::Unmappable {
                    encoding: "ASCII",
                    ch: text
                        .chars()
                        .find(|&c| c as u32 >= 0x80)
                        .expect("found above"),
                }),
            },
        }
    }

    /// Encodes `text` and prepends the encoding's BOM when it has one
    /// (UTF-8/16/32 variants). Legacy encodings (CP932, GBK, ASCII) have no
    /// BOM and are encoded plainly.
    pub fn encode_with_bom(self, text: &str) -> Result<Vec<u8>, EncodingError> {
        let body = self.encode(text)?;
        let bom: &[u8] = match self {
            Encoding::Utf8 => &UTF8_BOM,
            Encoding::Utf16Le => &UTF16LE_BOM,
            Encoding::Utf16Be => &UTF16BE_BOM,
            Encoding::Utf32Le => &UTF32LE_BOM,
            Encoding::Utf32Be => &UTF32BE_BOM,
            // Legacy encodings have no BOM concept; encode plainly.
            Encoding::Cp932 | Encoding::Gbk | Encoding::Ascii => return Ok(body),
        };
        let mut out = Vec::with_capacity(bom.len() + body.len());
        out.extend_from_slice(bom);
        out.extend_from_slice(&body);
        Ok(out)
    }
}

/// Detects a byte-order mark at the start of `bytes`.
///
/// The check order mirrors `TextStream.cpp::checkTextEncoding` **exactly**,
/// including its quirk: the UTF-16LE check comes first, so a UTF-32LE BOM
/// (`FF FE 00 00`) is reported as UTF-16LE (the C++ UTF-32LE branch is
/// dead code for the same reason). Returns the encoding and the BOM length.
pub fn detect_bom(bytes: &[u8]) -> Option<(Encoding, usize)> {
    if bytes.starts_with(&UTF16LE_BOM) {
        Some((Encoding::Utf16Le, 2))
    } else if bytes.starts_with(&UTF16BE_BOM) {
        Some((Encoding::Utf16Be, 2))
    } else if bytes.starts_with(&UTF8_BOM) {
        Some((Encoding::Utf8, 3))
    } else if bytes.starts_with(&UTF32LE_BOM) {
        Some((Encoding::Utf32Le, 4))
    } else if bytes.starts_with(&UTF32BE_BOM) {
        Some((Encoding::Utf32Be, 4))
    } else {
        None
    }
}

/// Strips a leading BOM, returning the remaining bytes and the detected
/// encoding (if any).
pub fn strip_bom(bytes: &[u8]) -> (&[u8], Option<Encoding>) {
    match detect_bom(bytes) {
        Some((encoding, len)) => (&bytes[len..], Some(encoding)),
        None => (bytes, None),
    }
}

/// `CharacterSet.convert`: decodes `input` from encoding name `from` and
/// re-encodes it as `to`. Both names are resolved by
/// [`Encoding::from_name`].
pub fn convert(input: &[u8], from: &str, to: &str) -> Result<Vec<u8>, EncodingError> {
    let from_enc = Encoding::from_name(from)
        .ok_or_else(|| EncodingError::UnknownEncoding(from.to_string()))?;
    let to_enc =
        Encoding::from_name(to).ok_or_else(|| EncodingError::UnknownEncoding(to.to_string()))?;
    let text = from_enc.decode(input)?;
    to_enc.encode(&text)
}

/// Encodes `text` (UTF-8) to the encoding named `to` —
/// `CharacterSet.encode`-style.
pub fn encode(text: &str, to: &str) -> Result<Vec<u8>, EncodingError> {
    let enc = Encoding::from_name(to).ok_or_else(|| EncodingError::UnknownEncoding(to.into()))?;
    enc.encode(text)
}

/// Decodes `bytes` from the encoding named `from` to UTF-8 —
/// `CharacterSet.decode`-style.
pub fn decode(bytes: &[u8], from: &str) -> Result<String, EncodingError> {
    let enc =
        Encoding::from_name(from).ok_or_else(|| EncodingError::UnknownEncoding(from.into()))?;
    enc.decode(bytes)
}

/// Whether `bytes` is a valid CP932 (Windows-31J) byte sequence.
///
/// Uses `encoding_rs::SHIFT_JIS`'s error-isolating decoder, which fails on
/// undefined lead/trail bytes; ASCII, half-width katakana and all defined
/// double-byte pairs pass.
pub fn is_valid_cp932(bytes: &[u8]) -> bool {
    SHIFT_JIS
        .decode_without_bom_handling_and_without_replacement(bytes)
        .is_some()
}

/// Encodes `text` to UTF-16 code units (native order, i.e. LE in memory on
/// little-endian hosts). Mirrors the `tjs_char`/`char16_t` representation
/// the engine and the TJS VM use internally.
pub fn to_utf16(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

/// Decodes UTF-16 code units (native order) to UTF-8 text, rejecting
/// unpaired surrogates.
pub fn from_utf16(units: &[u16]) -> Result<String, EncodingError> {
    decode_utf16_units(units)
}

/// Encodes `text` as UTF-16LE bytes.
pub fn to_utf16le_bytes(text: &str) -> Vec<u8> {
    let units = to_utf16(text);
    let mut out = Vec::with_capacity(units.len() * 2);
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out
}

/// Encodes `text` as UTF-16BE bytes.
pub fn to_utf16be_bytes(text: &str) -> Vec<u8> {
    let units = to_utf16(text);
    let mut out = Vec::with_capacity(units.len() * 2);
    for u in units {
        out.extend_from_slice(&u.to_be_bytes());
    }
    out
}

/// Decodes UTF-16LE bytes (odd byte counts are an error).
pub fn from_utf16le_bytes(bytes: &[u8]) -> Result<String, EncodingError> {
    let units = bytes_to_utf16_units(bytes, true)?;
    decode_utf16_units(&units)
}

/// Decodes UTF-16BE bytes (odd byte counts are an error).
pub fn from_utf16be_bytes(bytes: &[u8]) -> Result<String, EncodingError> {
    let units = bytes_to_utf16_units(bytes, false)?;
    decode_utf16_units(&units)
}

/// Parses UTF-16 byte pairs into code units in native order.
fn bytes_to_utf16_units(bytes: &[u8], little_endian: bool) -> Result<Vec<u16>, EncodingError> {
    if !bytes.len().is_multiple_of(2) {
        return Err(EncodingError::InvalidUtf16 {
            offset: bytes.len() - 1,
        });
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|c| {
            let pair = [c[0], c[1]];
            if little_endian {
                u16::from_le_bytes(pair)
            } else {
                u16::from_be_bytes(pair)
            }
        })
        .collect())
}

/// Strict UTF-16 code-unit decoding (native order).
fn decode_utf16_units(units: &[u16]) -> Result<String, EncodingError> {
    let mut out = String::with_capacity(units.len());
    let mut iter = units.iter().copied().enumerate();
    while let Some((i, u)) = iter.next() {
        if (0xD800..=0xDBFF).contains(&u) {
            match iter.next() {
                Some((_, lo)) if (0xDC00..=0xDFFF).contains(&lo) => {
                    let cp = 0x1_0000 + (((u as u32 - 0xD800) << 10) | (lo as u32 - 0xDC00));
                    out.push(char::from_u32(cp).expect("valid surrogate pair"));
                }
                _ => return Err(EncodingError::InvalidUtf16 { offset: i }),
            }
        } else if (0xDC00..=0xDFFF).contains(&u) {
            return Err(EncodingError::InvalidUtf16 { offset: i });
        } else {
            out.push(char::from_u32(u as u32).expect("non-surrogate BMP char is valid"));
        }
    }
    Ok(out)
}

/// Decodes UTF-32LE bytes to UTF-8 text.
fn decode_utf32le(bytes: &[u8]) -> Result<String, EncodingError> {
    decode_utf32(bytes, true)
}

/// Decodes UTF-32BE bytes to UTF-8 text.
fn decode_utf32be(bytes: &[u8]) -> Result<String, EncodingError> {
    decode_utf32(bytes, false)
}

/// Strict UTF-32 decoding; rejects surrogates, values > U+10FFFF and
/// trailing partial code units.
fn decode_utf32(bytes: &[u8], little_endian: bool) -> Result<String, EncodingError> {
    let chunks = bytes.chunks_exact(4);
    let remainder = chunks.remainder();
    if !remainder.is_empty() {
        return Err(EncodingError::InvalidUtf32 {
            offset: bytes.len() - remainder.len(),
        });
    }
    let mut out = String::with_capacity(bytes.len() / 4);
    for (i, chunk) in chunks.enumerate() {
        let pair = [chunk[0], chunk[1], chunk[2], chunk[3]];
        let cp = if little_endian {
            u32::from_le_bytes(pair)
        } else {
            u32::from_be_bytes(pair)
        };
        match char::from_u32(cp) {
            Some(c) => out.push(c),
            None => return Err(EncodingError::InvalidUtf32 { offset: i * 4 }),
        }
    }
    Ok(out)
}

/// Encodes `text` as UTF-32LE bytes.
fn encode_utf32le(text: &str) -> Vec<u8> {
    encode_utf32(text, true)
}

/// Encodes `text` as UTF-32BE bytes.
fn encode_utf32be(text: &str) -> Vec<u8> {
    encode_utf32(text, false)
}

fn encode_utf32(text: &str, little_endian: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 4);
    for c in text.chars() {
        let v = c as u32;
        let bytes = if little_endian {
            v.to_le_bytes()
        } else {
            v.to_be_bytes()
        };
        out.extend_from_slice(&bytes);
    }
    out
}

/// Decodes a legacy single/multi-byte encoding via `encoding_rs`, strictly.
///
/// The decoder is fed the whole input up front; if its pre-allocated output
/// space runs out (`OutputFull`) we grow the buffer and continue from where
/// decoding stopped — legacy encodings can expand (e.g. CP932 2 bytes → 3
/// UTF-8 bytes), so one pass is not always enough.
fn decode_legacy(
    enc: &'static encoding_rs::Encoding,
    bytes: &[u8],
    name: &'static str,
) -> Result<String, EncodingError> {
    let mut decoder = enc.new_decoder_without_bom_handling();
    let mut out = String::with_capacity(bytes.len());
    let mut rest = bytes;
    let mut consumed = 0;
    loop {
        let (result, read) = decoder.decode_to_string_without_replacement(rest, &mut out, true);
        consumed += read;
        rest = &rest[read..];
        match result {
            encoding_rs::DecoderResult::InputEmpty => return Ok(out),
            encoding_rs::DecoderResult::Malformed(..) => {
                return Err(EncodingError::InvalidSequence {
                    encoding: name,
                    offset: consumed,
                });
            }
            encoding_rs::DecoderResult::OutputFull => {
                // The output buffer ran out; give it at least as much room
                // as the remaining input needs (worst case 3x for legacy
                // single-byte encodings).
                out.reserve(rest.len() * 2 + 1);
            }
        }
    }
}

/// Encodes into a legacy single/multi-byte encoding via `encoding_rs`,
/// strictly (unmappable characters are an error, never replaced).
fn encode_legacy(
    enc: &'static encoding_rs::Encoding,
    text: &str,
    name: &'static str,
) -> Result<Vec<u8>, EncodingError> {
    let mut encoder = enc.new_encoder();
    // Pre-size the buffer with the encoder's worst-case guarantee so a
    // single pass cannot hit `OutputFull`.
    let capacity = encoder
        .max_buffer_length_from_utf8_without_replacement(text.len())
        .expect("usize overflow while sizing the encoding buffer");
    let mut out = Vec::with_capacity(capacity);
    match encoder.encode_from_utf8_to_vec_without_replacement(text, &mut out, true) {
        (encoding_rs::EncoderResult::InputEmpty, _) => Ok(out),
        (encoding_rs::EncoderResult::Unmappable(ch), _) => {
            Err(EncodingError::Unmappable { encoding: name, ch })
        }
        (encoding_rs::EncoderResult::OutputFull, _) => {
            unreachable!("buffer pre-sized with max_buffer_length cannot fill up")
        }
    }
}

/// Errors produced by encoding conversions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodingError {
    /// The encoding name is not recognized by [`Encoding::from_name`].
    UnknownEncoding(String),
    /// Invalid UTF-8 at the given byte offset.
    InvalidUtf8 { offset: usize },
    /// Invalid UTF-16 (unpaired surrogate or odd byte count) at the given
    /// unit/byte offset.
    InvalidUtf16 { offset: usize },
    /// Invalid UTF-32 (surrogate, out of range, or partial code unit) at
    /// the given byte offset.
    InvalidUtf32 { offset: usize },
    /// A byte sequence that is not valid in a legacy encoding.
    InvalidSequence {
        encoding: &'static str,
        offset: usize,
    },
    /// A character that cannot be represented in a legacy encoding.
    Unmappable { encoding: &'static str, ch: char },
}

impl fmt::Display for EncodingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EncodingError::UnknownEncoding(name) => {
                write!(f, "unknown encoding name {name:?}")
            }
            EncodingError::InvalidUtf8 { offset } => {
                write!(f, "invalid UTF-8 sequence at byte offset {offset}")
            }
            EncodingError::InvalidUtf16 { offset } => {
                write!(f, "invalid UTF-16 sequence at offset {offset}")
            }
            EncodingError::InvalidUtf32 { offset } => {
                write!(f, "invalid UTF-32 code point at byte offset {offset}")
            }
            EncodingError::InvalidSequence { encoding, offset } => {
                write!(f, "invalid {encoding} byte sequence at offset {offset}")
            }
            EncodingError::Unmappable { encoding, ch } => {
                write!(f, "character {ch:?} cannot be represented in {encoding}")
            }
        }
    }
}

impl std::error::Error for EncodingError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_cp932_round_trip_japanese() {
        let samples = [
            "こんにちは",
            "日本語のテキスト",
            "テスト",
            "漢字ひらがなカタカナ",
            "これはテストです。",
        ];
        for s in samples {
            let encoded = Encoding::Cp932.encode(s).unwrap();
            assert!(
                is_valid_cp932(&encoded),
                "encoded {s:?} must be valid cp932"
            );
            assert_eq!(
                Encoding::Cp932.decode(&encoded).unwrap(),
                s,
                "round trip {s:?}"
            );
        }
    }

    #[test]
    fn cp932_known_bytes() {
        // "日本語" in CP932: 日=0x93FA, 本=0x967B, 語=0x8CEA.
        let encoded = Encoding::Cp932.encode("日本語").unwrap();
        assert_eq!(encoded, [0x93, 0xFA, 0x96, 0x7B, 0x8C, 0xEA]);
    }

    #[test]
    fn wave_dash_round_trip_behavior() {
        // The classic CP932 problem characters (see module docs). Decoding
        // follows WHATWG: three of the four match Windows-31J.
        assert_eq!(Encoding::Cp932.decode(&[0x81, 0x60]).unwrap(), "\u{FF5E}");
        assert_eq!(Encoding::Cp932.decode(&[0x81, 0x61]).unwrap(), "\u{2225}");
        assert_eq!(Encoding::Cp932.decode(&[0x81, 0x91]).unwrap(), "\u{FFE0}");
        // ...but 81 7C is U+FF0D here, where the Windows-31J Unicode table
        // has U+2015 (JIS X 0208 had U+2014).
        assert_eq!(Encoding::Cp932.decode(&[0x81, 0x7C]).unwrap(), "\u{FF0D}");

        // The WHATWG side of each pair encodes to those bytes and round-trips.
        assert_eq!(Encoding::Cp932.encode("\u{FF5E}").unwrap(), [0x81, 0x60]);
        assert_eq!(Encoding::Cp932.encode("\u{2225}").unwrap(), [0x81, 0x61]);
        assert_eq!(Encoding::Cp932.encode("\u{FF0D}").unwrap(), [0x81, 0x7C]);
        assert_eq!(Encoding::Cp932.encode("\u{FFE0}").unwrap(), [0x81, 0x91]);
        assert_eq!(
            Encoding::Cp932
                .decode(&Encoding::Cp932.encode("\u{FF5E}").unwrap())
                .unwrap(),
            "\u{FF5E}"
        );

        // The strict WHATWG encoder refuses the JIS X 0208 originals that
        // Windows-31J would silently emit 81 60/81 61/81 7C for. (U+2015 is
        // fine — it has its own slot, 81 5C.)
        for ch in ['\u{301C}', '\u{2016}', '\u{2014}'] {
            assert!(
                matches!(
                    Encoding::Cp932.encode(&ch.to_string()),
                    Err(EncodingError::Unmappable { .. })
                ),
                "U+{:04X} must be unmappable in strict WHATWG Shift_JIS",
                ch as u32
            );
        }
        assert_eq!(Encoding::Cp932.encode("\u{2015}").unwrap(), [0x81, 0x5C]);
    }

    #[test]
    fn utf16le_utf8_round_trip() {
        let text = "UTF-16でエンコードされたテキスト";
        let bytes = to_utf16le_bytes(text);
        assert_eq!(bytes.len(), text.encode_utf16().count() * 2);
        assert_eq!(from_utf16le_bytes(&bytes).unwrap(), text);
        assert_eq!(Encoding::Utf16Le.decode(&bytes).unwrap(), text);

        let be = to_utf16be_bytes(text);
        assert_eq!(from_utf16be_bytes(&be).unwrap(), text);
        assert_eq!(Encoding::Utf16Be.decode(&be).unwrap(), text);
    }

    #[test]
    fn utf16_units_native_order() {
        // to_utf16 yields code units, so astral characters need surrogates.
        let units = to_utf16("𠮷𠮷"); // U+20BB7 (surrogate pair)
        assert_eq!(units, [0xD842, 0xDFB7, 0xD842, 0xDFB7]);
        assert_eq!(from_utf16(&units).unwrap(), "𠮷𠮷");
    }

    #[test]
    fn utf16_invalid_sequences() {
        assert!(matches!(
            from_utf16(&[0xD842]),
            Err(EncodingError::InvalidUtf16 { offset: 0 })
        ));
        assert!(matches!(
            from_utf16(&[0xDFB7]),
            Err(EncodingError::InvalidUtf16 { offset: 0 })
        ));
        assert!(from_utf16le_bytes(&[0x00]).is_err(), "odd byte count");
    }

    #[test]
    fn utf32_round_trip() {
        let text = "𠮷テスト🙂"; // includes astral and emoji
        for enc in [Encoding::Utf32Le, Encoding::Utf32Be] {
            let bytes = enc.encode(text).unwrap();
            assert_eq!(enc.decode(&bytes).unwrap(), text);
        }
        assert!(
            matches!(
                Encoding::Utf32Le.decode(&[0x00, 0xD8, 0x00, 0x00]),
                Err(EncodingError::InvalidUtf32 { .. })
            ),
            "surrogates are not valid UTF-32"
        );
        assert!(
            Encoding::Utf32Le.decode(&[0x00, 0x00, 0x00]).is_err(),
            "partial code unit"
        );
    }

    #[test]
    fn bom_detection() {
        assert_eq!(detect_bom(&UTF8_BOM), Some((Encoding::Utf8, 3)));
        assert_eq!(detect_bom(&UTF16LE_BOM), Some((Encoding::Utf16Le, 2)));
        assert_eq!(detect_bom(&UTF16BE_BOM), Some((Encoding::Utf16Be, 2)));
        assert_eq!(detect_bom(&UTF32BE_BOM), Some((Encoding::Utf32Be, 4)));
        assert_eq!(detect_bom(b"no bom"), None);
        assert_eq!(detect_bom(&[]), None);
        assert_eq!(detect_bom(&[0xFF]), None, "partial BOM is not detected");
    }

    #[test]
    fn bom_utf32le_quirk_matches_reference() {
        // TextStream.cpp checks FF FE before the UTF-32LE pattern, so a
        // UTF-32LE BOM reports as UTF-16LE there; we mirror that.
        assert_eq!(detect_bom(&UTF32LE_BOM), Some((Encoding::Utf16Le, 2)));
    }

    #[test]
    fn strip_bom_round_trip() {
        let bytes = [&UTF8_BOM[..], "こんにちは".as_bytes()].concat();
        let (rest, enc) = strip_bom(&bytes);
        assert_eq!(enc, Some(Encoding::Utf8));
        assert_eq!(Encoding::Utf8.decode(rest).unwrap(), "こんにちは");
    }

    #[test]
    fn gbk_round_trip_chinese() {
        let samples = ["简体中文", "汉字测试", "你好，世界！"];
        for s in samples {
            let encoded = Encoding::Gbk.encode(s).unwrap();
            assert_eq!(Encoding::Gbk.decode(&encoded).unwrap(), s);
        }
        // GBK: 你=0xC4E3, 好=0xBAC3.
        assert_eq!(
            Encoding::Gbk.encode("你好").unwrap(),
            [0xC4, 0xE3, 0xBA, 0xC3]
        );
        // WHATWG GBK maps byte 0x80 to the euro sign (CP936 heritage).
        assert_eq!(Encoding::Gbk.decode(&[0x80]).unwrap(), "\u{20AC}");
    }

    #[test]
    fn ascii_strict() {
        assert_eq!(
            Encoding::Ascii.decode(b"plain ASCII").unwrap(),
            "plain ASCII"
        );
        assert!(matches!(
            Encoding::Ascii.decode(&[0x41, 0xE3]),
            Err(EncodingError::InvalidSequence { offset: 1, .. })
        ));
        assert!(matches!(
            Encoding::Ascii.encode("café"),
            Err(EncodingError::Unmappable { ch: 'é', .. })
        ));
    }

    #[test]
    fn convert_between_encodings() {
        // UTF-8 Japanese -> CP932 -> UTF-16LE.
        let cp932 = convert("こんにちは".as_bytes(), "utf-8", "cp932").unwrap();
        let utf16 = convert(&cp932, "cp932", "utf-16le").unwrap();
        assert_eq!(from_utf16le_bytes(&utf16).unwrap(), "こんにちは");

        // Shift_JIS name resolves to CP932.
        assert_eq!(Encoding::from_name("SHIFT_JIS"), Some(Encoding::Cp932));
        assert_eq!(Encoding::from_name("shift-jis"), Some(Encoding::Cp932));
        assert_eq!(Encoding::from_name("windows-31j"), Some(Encoding::Cp932));
        assert_eq!(Encoding::from_name("Windows-1252"), Some(Encoding::Ascii));
        assert_eq!(Encoding::from_name("UTF-16BE"), Some(Encoding::Utf16Be));
        assert_eq!(Encoding::from_name("n"), Some(Encoding::Utf8));
        assert_eq!(Encoding::from_name("klingon"), None);
    }

    #[test]
    fn convert_unknown_name_errors() {
        assert!(matches!(
            convert(b"x", "utf-8", "klingon"),
            Err(EncodingError::UnknownEncoding(ref n)) if n == "klingon"
        ));
    }

    #[test]
    fn invalid_utf8_reports_offset() {
        assert!(matches!(
            Encoding::Utf8.decode(b"ab\xffcd"),
            Err(EncodingError::InvalidUtf8 { offset: 2 })
        ));
    }

    #[test]
    fn invalid_cp932_byte_sequence() {
        assert!(!is_valid_cp932(&[0x81]));
        assert!(!is_valid_cp932(&[0x81, 0x20]), "invalid trail byte");
        assert!(!is_valid_cp932(&[0xFD]));
        assert!(is_valid_cp932(b"ASCII only"));
        assert!(is_valid_cp932(&[0x81, 0x60]));
        assert!(
            matches!(
                Encoding::Cp932.decode(&[0x81, 0x20]),
                Err(EncodingError::InvalidSequence { offset: 1, .. })
            ),
            "the ASCII 0x20 is consumed first, then 0x81 is malformed at offset 1"
        );
    }

    #[test]
    fn unmappable_char_is_an_error() {
        // U+20BB7 has no CP932 representation.
        assert!(matches!(
            Encoding::Cp932.encode("𠮷"),
            Err(EncodingError::Unmappable {
                encoding: "cp932",
                ch: '𠮷'
            })
        ));
    }

    #[test]
    fn encode_with_bom() {
        assert_eq!(
            Encoding::Utf8.encode_with_bom("a").unwrap(),
            [0xEF, 0xBB, 0xBF, b'a']
        );
        assert_eq!(
            Encoding::Utf16Le.encode_with_bom("a").unwrap(),
            [0xFF, 0xFE, b'a', 0x00]
        );
        assert_eq!(Encoding::Cp932.encode_with_bom("a").unwrap(), [b'a']);
    }

    #[test]
    fn names_and_canonical_names() {
        for enc in [
            Encoding::Utf8,
            Encoding::Utf16Le,
            Encoding::Utf16Be,
            Encoding::Utf32Le,
            Encoding::Utf32Be,
            Encoding::Cp932,
            Encoding::Gbk,
            Encoding::Ascii,
        ] {
            assert_eq!(Encoding::from_name(enc.name()), Some(enc));
        }
        assert_eq!(Encoding::Utf16Le.name(), "UTF-16LE");
        assert_eq!(Encoding::Cp932.name(), "cp932");
    }
}
