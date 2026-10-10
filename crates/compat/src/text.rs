//! Script text decoding: BOM detection, UTF-8/UTF-16, CP932, and the
//! KiriKiri `FE FE` XOR/bit-swap container with an optional zlib payload.
//!
//! Behavioral references:
//!
//! * `crates/tvp-streams/src/text.rs` (`decode_bytes`): BOM detection and the
//!   UTF-8-then-CP932 content sniff.
//! * `crates/tvp-scripts/src/lib.rs` (`decompress_script`) and
//!   `reference/cpp/core/base/TextStream.cpp` /
//!   `crates/tjs2-sys/cpp/streams.cpp`: the `FE FE` container (modes 0/1 are
//!   UTF-16LE ciphers, mode 2 is zlib over UTF-16LE).
//!
//! The decoder is strict: an undecodable payload is a typed error, never
//! a lossy U+FFFD substitution.

use std::io::Read;

/// Bytes before the compressed payload of an `FE FE 02` container:
/// `FE FE 02` + the UTF-16LE BOM `FF FE` + the compressed and uncompressed
/// sizes (two little-endian `u64`s).
const CONTAINER_ZLIB_HEADER: usize = 21;

/// Decode scenario/script bytes to UTF-8 text.
///
/// Detection order:
///
/// 1. the `FE FE` container (mode 0/1 = UTF-16LE cipher, mode 2 = zlib);
/// 2. a UTF-8 / UTF-16LE / UTF-16BE byte-order mark;
/// 3. an explicit `encoding` override (`Scripts.textEncoding`);
/// 4. valid UTF-8;
/// 5. valid CP932 (Windows-31J).
///
/// Every failure is a typed error; the function never falls back to a lossy
/// substitution.
pub(crate) fn decode_text(bytes: &[u8], encoding: Option<&str>) -> Result<String, String> {
    // 1. `FE FE` container (never a valid UTF BOM, so it wins unambiguously).
    if bytes.len() >= 3 && bytes[0] == 0xFE && bytes[1] == 0xFE {
        return decode_container(bytes);
    }
    // 2. Byte-order marks.
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8(rest.to_vec()).map_err(|error| format!("UTF-8 BOM: {error}"));
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return decode_utf16le(rest, "UTF-16LE BOM");
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return decode_utf16be(rest, "UTF-16BE BOM");
    }
    // 3. An explicit `Scripts.textEncoding` forces the decoder.
    if let Some(name) = encoding.filter(|name| !name.is_empty()) {
        return decode_named(bytes, name);
    }
    // 4. Valid UTF-8 wins over CP932, which is a strict byte-language.
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Ok(text.to_string());
    }
    // 5. CP932 fallback.
    decode_cp932(bytes)
        .map_err(|_| "script is neither valid UTF-8 nor valid CP932 (Windows-31J)".to_string())
}

/// Decode a `FE FE` container, dispatching on its mode byte.
fn decode_container(bytes: &[u8]) -> Result<String, String> {
    let mode = bytes[2];
    match mode {
        0 | 1 => decode_container_cipher(bytes, mode),
        2 => decode_container_zlib(bytes),
        other => Err(format!("FE FE container mode {other} is not supported")),
    }
}

/// Decrypt the UTF-16LE payload of an `FE FE 00` / `FE FE 01` container.
///
/// The header is `FE FE <mode>` optionally followed by the UTF-16LE BOM
/// `FF FE`; the payload is little-endian code units. Mode 0 is the legacy
/// per-code-unit XOR (only applied to units `>= 0x20`); mode 1 is the
/// adjacent-bit swap.
fn decode_container_cipher(bytes: &[u8], mode: u8) -> Result<String, String> {
    let start = if bytes.len() >= 5 && bytes[3] == 0xFF && bytes[4] == 0xFE {
        5
    } else {
        3
    };
    let payload = &bytes[start..];
    if !payload.len().is_multiple_of(2) {
        return Err(format!(
            "FE FE mode {mode} cipher payload has an odd length"
        ));
    }
    let units = payload.chunks_exact(2).map(|chunk| {
        let ch = u16::from_le_bytes([chunk[0], chunk[1]]);
        if mode == 0 {
            if ch >= 0x20 {
                ch ^ (((ch & 0xfe) << 8) ^ 1)
            } else {
                ch
            }
        } else {
            ((ch & 0xaaaa) >> 1) | ((ch & 0x5555) << 1)
        }
    });
    from_utf16(units, "FE FE cipher")
}

/// Inflate the payload of an `FE FE 02` container and decode it as UTF-16LE.
///
/// Layout: `FE FE 02 FF FE <compressed:u64 LE> <uncompressed:u64 LE> <zlib>`.
fn decode_container_zlib(bytes: &[u8]) -> Result<String, String> {
    if bytes.len() < CONTAINER_ZLIB_HEADER {
        return Err(format!(
            "FE FE zlib container needs {CONTAINER_ZLIB_HEADER} header bytes, got {}",
            bytes.len()
        ));
    }
    let compressed = u64::from_le_bytes(bytes[5..13].try_into().expect("8 header bytes"));
    let uncompressed = u64::from_le_bytes(bytes[13..21].try_into().expect("8 header bytes"));
    let compressed = usize::try_from(compressed)
        .map_err(|_| "FE FE compressed size does not fit the address space".to_string())?;
    let uncompressed = usize::try_from(uncompressed)
        .map_err(|_| "FE FE uncompressed size does not fit the address space".to_string())?;
    let payload = &bytes[CONTAINER_ZLIB_HEADER..];
    if compressed > payload.len() {
        return Err(format!(
            "FE FE compressed size {compressed} overruns the {} available payload bytes",
            payload.len()
        ));
    }
    let mut out = Vec::new();
    flate2::read::ZlibDecoder::new(&payload[..compressed])
        .read_to_end(&mut out)
        .map_err(|error| format!("FE FE zlib decode failed: {error}"))?;
    if out.len() != uncompressed {
        return Err(format!(
            "FE FE size mismatch: header says {uncompressed} bytes, decoded {}",
            out.len()
        ));
    }
    decode_utf16le(&out, "FE FE zlib payload")
}

/// Decode a UTF-16LE byte payload.
fn decode_utf16le(bytes: &[u8], label: &str) -> Result<String, String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(format!("{label} has an odd length"));
    }
    from_utf16(
        bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]])),
        label,
    )
}

/// Decode a UTF-16BE byte payload.
fn decode_utf16be(bytes: &[u8], label: &str) -> Result<String, String> {
    if !bytes.len().is_multiple_of(2) {
        return Err(format!("{label} has an odd length"));
    }
    from_utf16(
        bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]])),
        label,
    )
}

/// Strictly decode UTF-16 code units, rejecting unpaired surrogates.
fn from_utf16(units: impl Iterator<Item = u16>, label: &str) -> Result<String, String> {
    char::decode_utf16(units)
        .collect::<Result<String, _>>()
        .map_err(|error| format!("{label}: {error}"))
}

/// Decode `bytes` using an explicit `Scripts.textEncoding` name.
fn decode_named(bytes: &[u8], name: &str) -> Result<String, String> {
    let normalized = name.trim().to_ascii_lowercase().replace(['_', '-'], "");
    match normalized.as_str() {
        "utf8" | "unicode(utf8)" | "n" => {
            String::from_utf8(bytes.to_vec()).map_err(|error| format!("UTF-8: {error}"))
        }
        "utf16le" | "utf16" => decode_utf16le(bytes, "UTF-16LE"),
        "utf16be" => decode_utf16be(bytes, "UTF-16BE"),
        "cp932" | "shiftjis" | "sjis" | "ms932" | "windows31j" | "csshiftjis" | "xsjis" => {
            decode_cp932(bytes)
        }
        "ascii" | "usascii" | "windows1252" | "iso88591" | "latin1" => decode_ascii(bytes),
        _ => Err(format!(
            "configured text encoding '{name}' is not supported by the compat surface"
        )),
    }
}

/// Strictly decode CP932 (Windows-31J / WHATWG Shift_JIS).
fn decode_cp932(bytes: &[u8]) -> Result<String, String> {
    encoding_rs::SHIFT_JIS
        .decode_without_bom_handling_and_without_replacement(bytes)
        .map(|text| text.into_owned())
        .ok_or_else(|| "CP932: invalid byte sequence".to_string())
}

/// Strictly decode 7-bit ASCII.
fn decode_ascii(bytes: &[u8]) -> Result<String, String> {
    match bytes.iter().position(|byte| *byte >= 0x80) {
        Some(offset) => Err(format!(
            "ASCII: byte 0x{:02X} at offset {offset} is not ASCII",
            bytes[offset]
        )),
        None => Ok(bytes.iter().map(|&byte| byte as char).collect()),
    }
}
