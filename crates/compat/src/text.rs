//! Script text decoding: BOM detection, charset detection for BOM-less
//! legacy-encoded scripts, and the KiriKiri `FE FE` XOR/bit-swap container
//! with an optional zlib payload.
//!
//! Behavioral references:
//!
//! * `reference/cpp/core/base/TextStream.cpp` (`checkTextEncoding` and
//!   `tTVPTextReadStream`): BOM recognition (`:24-45`), the uchardet
//!   detection branch for BOM-less text (`:47-57`), conversion from the
//!   reported charset (`:137-191`), and the `FE FE` crypt/compression
//!   container (`:82-135`). The cipher modes 0/1 have a four-byte header
//!   (`FE FE <mode>` plus one skipped byte) and the UTF-16 payload starts at
//!   offset 4 (`:87-88`); a `FF FE` after the mode byte is header data, not a
//!   byte-order mark. Mode 2's header (`FE FE 02 FF FE` + two little-endian
//!   `u64` sizes) matches the reference (`:104-116`).
//! * `crates/tjs/cpp/streams.cpp`: the same `FE FE` handling for the TJS
//!   native text streams.
//!
//! Every conversion path -- a byte-order mark, `Scripts.textEncoding`, or
//! charset detection -- drops byte sequences the charset cannot decode,
//! matching the reference's Boost.Locale `skip` policy, and logs the drop once
//! per decode. Only an unsupported `Scripts.textEncoding` name is a typed
//! error. BOM-less text with no override is decoded from the charset reported
//! by `chardetng`, the maintained successor of the reference's uchardet
//! detector.

use chardetng::{EncodingDetector, Iso2022JpDetection, Utf8Detection};
use encoding_rs::DecoderResult;
use std::io::Read;

/// Bytes before the compressed payload of an `FE FE 02` container:
/// `FE FE 02` + the UTF-16LE BOM `FF FE` + the compressed and uncompressed
/// sizes (two little-endian `u64`s).
const CONTAINER_ZLIB_HEADER: usize = 21;

/// Selects the decoder for [`decode_text`].
///
/// [`Detect`](Self::Detect) runs charset detection, the reference's
/// text-stream behaviour. A caller that means the current code page or any
/// other fixed encoding must say so with [`Named`](Self::Named): do not use a
/// bare "no encoding" to mean detection in one place and the code page in
/// another.
#[derive(Clone, Copy, Debug)]
pub(crate) enum TextEncoding<'a> {
    /// Detect the charset from the bytes.
    Detect,
    /// Decode with an explicit charset name.
    Named(&'a str),
}

/// Decode scenario/script bytes to UTF-8 text.
///
/// Detection order:
///
/// 1. the `FE FE` container (mode 0/1 = UTF-16LE cipher, mode 2 = zlib);
/// 2. a UTF-32 / UTF-8 / UTF-16LE / UTF-16BE byte-order mark;
/// 3. an explicit [`TextEncoding::Named`] decoder (`Scripts.textEncoding` or a
///    plugin that names a fixed encoding);
/// 4. charset detection.
///
/// A payload the charset cannot decode is dropped and logged, matching the
/// reference's Boost.Locale `skip` policy; an unsupported explicit `encoding`
/// is a typed error.
pub(crate) fn decode_text(bytes: &[u8], encoding: TextEncoding<'_>) -> Result<String, String> {
    decode_text_inner(bytes, encoding).0
}

/// The UTF-8 text produced by a decode plus the encoding decision that
/// produced it, for diagnostic logging.
pub(crate) struct DecodedText {
    /// The decoded UTF-8 text.
    pub text: String,
    /// A short label naming the path taken: a byte-order mark, the `FE FE`
    /// container mode, the explicit override, or the detected charset.
    pub encoding: String,
}

/// Decode like [`decode_text`], additionally returning the encoding decision.
pub(crate) fn decode_text_reported(
    bytes: &[u8],
    encoding: TextEncoding<'_>,
) -> Result<DecodedText, String> {
    let (text, encoding) = decode_text_inner(bytes, encoding);
    text.map(|text| DecodedText { text, encoding })
}

/// The shared decode dispatch; the label is the encoding decision, present
/// even when the decode fails so a caller can report what was attempted.
fn decode_text_inner(bytes: &[u8], encoding: TextEncoding<'_>) -> (Result<String, String>, String) {
    // 1. `FE FE` container (never a valid UTF BOM, so it wins unambiguously).
    if bytes.len() >= 3 && bytes[0] == 0xFE && bytes[1] == 0xFE {
        return (
            decode_container(bytes),
            format!("FE FE container mode {}", bytes[2]),
        );
    }
    // 2. Byte-order marks. UTF-32 is tested before UTF-16 because a UTF-32LE
    //    BOM begins with the UTF-16LE BOM `FF FE`, so the longer match must
    //    win for UTF-32LE to be reachable.
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE, 0x00, 0x00]) {
        return (
            Ok(decode_utf32(rest, "UTF-32LE", u32::from_le_bytes)),
            "UTF-32LE BOM".to_string(),
        );
    }
    if let Some(rest) = bytes.strip_prefix(&[0x00, 0x00, 0xFE, 0xFF]) {
        return (
            Ok(decode_utf32(rest, "UTF-32BE", u32::from_be_bytes)),
            "UTF-32BE BOM".to_string(),
        );
    }
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return (
            decode_from_encoding(rest, encoding_rs::UTF_8),
            "UTF-8 BOM".to_string(),
        );
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return (
            decode_from_encoding(rest, encoding_rs::UTF_16LE),
            "UTF-16LE BOM".to_string(),
        );
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return (
            decode_from_encoding(rest, encoding_rs::UTF_16BE),
            "UTF-16BE BOM".to_string(),
        );
    }
    // 3. An explicit named encoding forces the decoder.
    if let TextEncoding::Named(name) = encoding
        && !name.is_empty()
    {
        return (decode_named(bytes, name), format!("explicit '{name}'"));
    }
    // 4. Detect the charset and decode from it.
    detect_and_decode(bytes)
}

/// Detect the charset of BOM-less, override-less text and decode it.
///
/// The reference runs uchardet over the bytes and converts from the reported
/// charset, mapping `SHIFT_JIS` to `cp932` and `WINDOWS-1252` to `ASCII`
/// (`reference/cpp/core/base/TextStream.cpp:47-57`). Legacy East-Asian
/// charsets overlap byte-for-byte, so the decision needs the statistical model
/// `chardetng` provides; `encoding_rs` then does the bulk decode and owns the
/// per-charset tables. Neither crate allocates per input byte: `chardetng`
/// scores fixed-size buffers and `encoding_rs` decodes the whole slice in one
/// pass, allocating only the output string.
fn detect_and_decode(bytes: &[u8]) -> (Result<String, String>, String) {
    // `Allow` keeps ISO-2022-JP in the candidate set, matching the charsets the
    // reference's uchardet can report.
    let mut detector = EncodingDetector::new(Iso2022JpDetection::Allow);
    detector.feed(bytes, true);
    let detected = detector.guess(None, Utf8Detection::Allow);
    // The reference maps `WINDOWS-1252` to `ASCII`; its ASCII branch assigns
    // each byte to a UTF-16 code unit (Latin-1), not CP1252.
    if detected.name() == encoding_rs::WINDOWS_1252.name() {
        return (
            Ok(encoding_rs::mem::decode_latin1(bytes).into_owned()),
            "detected WINDOWS-1252->latin1".to_string(),
        );
    }
    // The reference maps `SHIFT_JIS` to `cp932`; `encoding_rs`'s WHATWG
    // Shift_JIS is already Windows-31J (cp932), so the reported encoding is
    // used as-is.
    (
        decode_from_encoding(bytes, detected),
        format!("detected {}", detected.name()),
    )
}

/// Decode `bytes` from `encoding`, dropping byte sequences the charset cannot
/// decode.
///
/// Every reference conversion entry point defaults to the Boost.Locale `skip`
/// policy -- `to_utf` for the detected and explicit charsets
/// (`reference/cpp/core/base/TextStream.cpp:182-191`) and `utf_to_utf` for the
/// BOM paths (`:148-180`) -- so illegal/unconvertible bytes are dropped and
/// the surrounding text is kept. Boost.Locale's `encoding_errors.hpp` defines
/// `default_method = skip`, and `encoding_utf.hpp` documents that invalid
/// characters are skipped. The `encoding_rs` incremental decoder exposes
/// malformed sequences the same way, so the drop stays a single pass over the
/// input and is reported once per decode rather than silently. A payload whose
/// worst-case UTF-8 length overflows `usize` is a typed error.
fn decode_from_encoding(
    bytes: &[u8],
    encoding: &'static encoding_rs::Encoding,
) -> Result<String, String> {
    let mut decoder = encoding.new_decoder_without_bom_handling();
    let capacity = decoder
        .max_utf8_buffer_length_without_replacement(bytes.len())
        .ok_or_else(|| format!("{}: input is too large to decode", encoding.name()))?;
    let mut text = String::with_capacity(capacity);
    let mut dropped = 0usize;
    let mut consumed = 0usize;
    loop {
        let (result, read) =
            decoder.decode_to_string_without_replacement(&bytes[consumed..], &mut text, true);
        consumed += read;
        match result {
            DecoderResult::InputEmpty => break,
            DecoderResult::OutputFull => text.reserve(bytes.len().max(64)),
            DecoderResult::Malformed(length, _) => dropped += usize::from(length),
        }
    }
    warn_dropped(encoding.name(), dropped);
    Ok(text)
}

/// Log one warning when a decode dropped `dropped` bytes of `label`.
fn warn_dropped(label: &str, dropped: usize) {
    if dropped > 0 {
        log::warn!("script decode ({label}): dropped {dropped} undecodable byte(s)");
    }
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
/// The header is four bytes -- `FE FE <mode>` plus one skipped byte -- and the
/// payload begins at offset 4 as little-endian code units (reference
/// `reference/cpp/core/base/TextStream.cpp:82-105`); a trailing odd byte is
/// dropped. Mode 0 is the legacy per-code-unit XOR (only applied to units
/// `>= 0x20`); mode 1 is the adjacent-bit swap.
fn decode_container_cipher(bytes: &[u8], mode: u8) -> Result<String, String> {
    // The reference reads `(size - 4) / 2` code units starting at
    // `raw.data() + 4` (`reference/cpp/core/base/TextStream.cpp:87-88`). There
    // is no `FF FE` sniff in the cipher modes: a `FF FE` after the mode byte
    // is payload-adjacent header data, not a byte-order mark.
    let payload: &[u8] = bytes.get(4..).unwrap_or_default();
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
    let (text, dropped_units) = from_utf16(units);
    warn_dropped("FE FE cipher", dropped_units * 2 + payload.len() % 2);
    Ok(text)
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
    Ok(decode_utf16le(&out, "FE FE zlib payload"))
}

/// Decode a UTF-16LE byte payload, dropping unpaired surrogates and a trailing
/// half unit.
fn decode_utf16le(bytes: &[u8], label: &str) -> String {
    let (text, dropped_units) = from_utf16(
        bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]])),
    );
    warn_dropped(label, dropped_units * 2 + bytes.len() % 2);
    text
}

/// Decode a UTF-32 byte payload, dropping values that are not Unicode scalar
/// values.
///
/// The reference has a UTF-32 branch, but it is unreachable (the UTF-16LE BOM
/// test shadows `FF FE 00 00`) and its end pointer runs out of bounds, so this
/// is our own behaviour rather than a copy of a reachable reference path. It
/// decodes both byte orders and drops invalid scalar values with the same
/// warning the charset decoder uses.
fn decode_utf32(bytes: &[u8], label: &str, word: impl Fn([u8; 4]) -> u32) -> String {
    let mut dropped = bytes.len() % 4;
    let mut text = String::with_capacity(bytes.len() / 4);
    for chunk in bytes.chunks_exact(4) {
        let code = word(chunk.try_into().expect("chunks_exact(4) yields 4 bytes"));
        match char::from_u32(code) {
            Some(ch) => text.push(ch),
            None => dropped += 4,
        }
    }
    warn_dropped(label, dropped);
    text
}

/// Decode UTF-16 code units, returning the text and the number of unpaired
/// surrogates dropped.
///
/// The reference assigns the raw units into a `std::u16string` and keeps going;
/// a Rust `String` is UTF-8 and cannot hold an unpaired surrogate, so the unit
/// is dropped. This is a representation limit, not a reference behaviour.
fn from_utf16(units: impl Iterator<Item = u16>) -> (String, usize) {
    let mut text = String::new();
    let mut dropped = 0usize;
    for decoded in char::decode_utf16(units) {
        match decoded {
            Ok(ch) => text.push(ch),
            Err(_) => dropped += 1,
        }
    }
    (text, dropped)
}

/// Decode `bytes` using an explicit `Scripts.textEncoding` name.
fn decode_named(bytes: &[u8], name: &str) -> Result<String, String> {
    let normalized = name.trim().to_ascii_lowercase().replace(['_', '-'], "");
    match normalized.as_str() {
        "utf8" | "unicode(utf8)" | "n" => decode_from_encoding(bytes, encoding_rs::UTF_8),
        "utf16le" | "utf16" => decode_from_encoding(bytes, encoding_rs::UTF_16LE),
        "utf16be" => decode_from_encoding(bytes, encoding_rs::UTF_16BE),
        "cp932" | "shiftjis" | "sjis" | "ms932" | "windows31j" | "csshiftjis" | "xsjis" => {
            decode_from_encoding(bytes, encoding_rs::SHIFT_JIS)
        }
        // The reference's `ASCII` branch assigns each byte to a UTF-16 code
        // unit (Latin-1), so every byte decodes and nothing is dropped.
        "ascii" | "usascii" | "windows1252" | "iso88591" | "latin1" => {
            Ok(encoding_rs::mem::decode_latin1(bytes).into_owned())
        }
        _ => Err(format!(
            "configured text encoding '{name}' is not supported by the compat surface"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{TextEncoding, decode_from_encoding, decode_text, decode_text_reported};
    use encoding_rs::{BIG5, EUC_KR, GBK, SHIFT_JIS};

    thread_local! {
        /// Log lines written on this thread since the last clear; the tests run
        /// in parallel, so the capture is per-thread.
        static CAPTURED_LOGS: std::cell::RefCell<Vec<String>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }

    /// A `log` sink that records every record on the calling thread.
    struct CaptureLogger;

    impl log::Log for CaptureLogger {
        fn enabled(&self, _metadata: &log::Metadata) -> bool {
            true
        }

        fn log(&self, record: &log::Record) {
            CAPTURED_LOGS.with(|logs| {
                logs.borrow_mut()
                    .push(format!("{} {}", record.level(), record.args()));
            });
        }

        fn flush(&self) {}
    }

    /// Install the capturing sink once per test binary, clear this thread's
    /// buffer, run `f`, and return its result plus the thread's log lines.
    fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
        static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        ONCE.get_or_init(|| {
            let _ = log::set_boxed_logger(Box::new(CaptureLogger));
            log::set_max_level(log::LevelFilter::Debug);
        });
        CAPTURED_LOGS.with(|logs| logs.borrow_mut().clear());
        let result = f();
        let logs = CAPTURED_LOGS.with(|logs| logs.borrow().clone());
        (result, logs)
    }

    /// Assert exactly one drop warning names `label` and `count` bytes.
    fn assert_dropped_once(logs: &[String], label: &str, count: usize) {
        let expected = format!("dropped {count} undecodable byte(s)");
        let warnings: Vec<&String> = logs
            .iter()
            .filter(|line| line.starts_with("WARN") && line.contains(expected.as_str()))
            .collect();
        assert_eq!(
            warnings.len(),
            1,
            "expected one drop warning, got: {logs:?}"
        );
        assert!(
            warnings[0].contains(label),
            "warning must name {label}: {warnings:?}"
        );
    }

    // Simplified Chinese: the detector reports GBK.
    const GBK_TEXT: &str = "这是一个用于测试文字编码检测的中文脚本。程序需要在没有字节顺序标记的时候，正确认出这是中文编码，并把它变成统一文字。";
    // Traditional Chinese: the detector reports Big5.
    const BIG5_TEXT: &str = "這是一個用於測試文字編碼檢測的中文腳本。程式需要在沒有位元組順序標記的時候，正確辨認這是中文編碼，並把它變成統一文字。";
    // Korean: the detector reports EUC-KR.
    const EUC_KR_TEXT: &str = "이것은 문자 인코딩 감지를 테스트하기 위한 한국어 스크립트입니다. 프로그램은 바이트 순서 표시가 없어도 올바른 한국어 인코딩을 인식하고 텍스트로 바꾸어야 합니다.";
    // Japanese: the detector reports Shift_JIS (cp932).
    const SHIFT_JIS_TEXT: &str = "// これは日本語のスクリプトです。文字コードの判定を確かめるためのテストです。\nreturn \"日本語のテストに成功しました\";";

    fn encoded(encoding: &'static encoding_rs::Encoding, text: &str) -> Vec<u8> {
        let (bytes, _, had_errors) = encoding.encode(text);
        assert!(
            !had_errors,
            "fixture is not representable in {}",
            encoding.name()
        );
        bytes.into_owned()
    }

    /// Encrypt code units as an `FE FE <mode>` container (writer side): the
    /// four-byte header `FE FE <mode> 00` then the little-endian code units
    /// (reference `reference/cpp/core/base/TextStream.cpp:82-105`).
    fn fefe_cipher(mode: u8, units: &[u16]) -> Vec<u8> {
        let mut out = vec![0xFE, 0xFE, mode, 0x00];
        for &unit in units {
            let encrypted = match mode {
                0 if unit >= 0x20 => unit ^ (((unit & 0xfe) << 8) ^ 1),
                0 => unit,
                1 => ((unit & 0xaaaa) >> 1) | ((unit & 0x5555) << 1),
                _ => unit,
            };
            out.extend_from_slice(&encrypted.to_le_bytes());
        }
        out
    }

    /// Encrypt code units as an `FE FE 00` legacy XOR container (writer side).
    fn fefe_mode0(units: &[u16]) -> Vec<u8> {
        fefe_cipher(0, units)
    }

    // reference/cpp/core/base/TextStream.cpp:47-57 — BOM-less text is decoded
    // from the charset reported by detection.
    #[test]
    fn gbk_script_is_detected_and_decoded() {
        let bytes = encoded(GBK, GBK_TEXT);
        assert_eq!(
            decode_text(&bytes, TextEncoding::Detect).expect("GBK decodes"),
            GBK_TEXT
        );
    }

    #[test]
    fn big5_script_is_detected_and_decoded() {
        let bytes = encoded(BIG5, BIG5_TEXT);
        assert_eq!(
            decode_text(&bytes, TextEncoding::Detect).expect("Big5 decodes"),
            BIG5_TEXT
        );
    }

    #[test]
    fn euc_kr_script_is_detected_and_decoded() {
        let bytes = encoded(EUC_KR, EUC_KR_TEXT);
        assert_eq!(
            decode_text(&bytes, TextEncoding::Detect).expect("EUC-KR decodes"),
            EUC_KR_TEXT
        );
    }

    #[test]
    fn shift_jis_script_is_detected_and_decoded() {
        let bytes = encoded(SHIFT_JIS, SHIFT_JIS_TEXT);
        assert_eq!(
            decode_text(&bytes, TextEncoding::Detect).expect("Shift_JIS decodes"),
            SHIFT_JIS_TEXT
        );
    }

    // reference/cpp/core/base/TextStream.cpp:53 — `SHIFT_JIS` is remapped to
    // `cp932`; :418-428 — the `sjis`/`shiftjis` aliases normalize to `cp932`.
    #[test]
    fn short_cp932_sample_decodes_through_the_override() {
        let text = "return \"日本語 OK\";";
        let bytes = encoded(SHIFT_JIS, text);
        assert_eq!(
            decode_text(&bytes, TextEncoding::Named("cp932")).expect("cp932 decodes"),
            text
        );
        assert_eq!(
            decode_text(&bytes, TextEncoding::Named("sjis")).expect("sjis decodes"),
            text
        );
    }

    // reference/cpp/core/base/TextStream.cpp:24-45 — a BOM selects the encoder
    // before detection runs.
    #[test]
    fn bom_takes_precedence_over_detection() {
        let text = "bom 世界";
        let mut utf8 = vec![0xEF, 0xBB, 0xBF];
        utf8.extend_from_slice(text.as_bytes());
        assert_eq!(
            decode_text(&utf8, TextEncoding::Detect).expect("UTF-8 BOM"),
            text
        );

        let mut utf16le = vec![0xFF, 0xFE];
        utf16le.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        assert_eq!(
            decode_text(&utf16le, TextEncoding::Detect).expect("UTF-16LE BOM"),
            text
        );
    }

    // reference/cpp/core/base/TextStream.cpp:36-45 — UTF-32LE/BE BOMs are
    // recognized; :174-180 converts UTF-32 text.
    #[test]
    fn utf32_bom_is_decoded() {
        let text = "wide 世界";
        let mut little_endian = vec![0xFF, 0xFE, 0x00, 0x00];
        little_endian.extend(text.chars().flat_map(|ch| (ch as u32).to_le_bytes()));
        assert_eq!(
            decode_text(&little_endian, TextEncoding::Detect).expect("UTF-32LE BOM"),
            text
        );

        let mut big_endian = vec![0x00, 0x00, 0xFE, 0xFF];
        big_endian.extend(text.chars().flat_map(|ch| (ch as u32).to_be_bytes()));
        assert_eq!(
            decode_text(&big_endian, TextEncoding::Detect).expect("UTF-32BE BOM"),
            text
        );
    }

    // reference/cpp/core/base/TextStream.cpp:140-143 — an explicit
    // `Scripts.textEncoding` overrides detection; :182-188 drops bytes the
    // charset cannot decode.
    #[test]
    fn override_takes_precedence_over_detection() {
        let text = "return \"日本語 OK\";";
        let bytes = encoded(SHIFT_JIS, text);
        let (decoded, logs) = capture_logs(|| decode_text(&bytes, TextEncoding::Named("utf-8")));
        let decoded = decoded.expect("the override drops malformed bytes");
        assert_ne!(decoded, text, "detection must not have run");
        assert!(
            logs.iter().any(|line| line.starts_with("WARN")),
            "the drop must be logged: {logs:?}"
        );
    }

    // reference/cpp/core/base/TextStream.cpp:32-36 — a UTF-8 BOM selects UTF-8;
    // `utf_to_utf` (skip default) drops the malformed bytes and keeps the rest.
    #[test]
    fn malformed_utf8_bom_payload_is_dropped_and_logged() {
        let (text, logs) = capture_logs(|| {
            decode_text(&[0xEF, 0xBB, 0xBF, 0x41, 0xFF, 0x42], TextEncoding::Detect)
        });
        assert_eq!(text.expect("UTF-8 BOM decodes"), "AB");
        assert_dropped_once(&logs, "UTF-8", 1);
    }

    // reference/cpp/core/base/TextStream.cpp:28-31 — a UTF-16 BOM selects
    // UTF-16; a trailing partial unit is dropped.
    #[test]
    fn malformed_utf16le_bom_payload_is_dropped_and_logged() {
        let (text, logs) =
            capture_logs(|| decode_text(&[0xFF, 0xFE, 0x41, 0x00, 0x42], TextEncoding::Detect));
        assert_eq!(text.expect("UTF-16LE BOM decodes"), "A");
        assert_dropped_once(&logs, "UTF-16LE", 1);
    }

    #[test]
    fn malformed_utf16be_bom_payload_is_dropped_and_logged() {
        let (text, logs) =
            capture_logs(|| decode_text(&[0xFE, 0xFF, 0x00, 0x41, 0x00], TextEncoding::Detect));
        assert_eq!(text.expect("UTF-16BE BOM decodes"), "A");
        assert_dropped_once(&logs, "UTF-16BE", 1);
    }

    // reference/cpp/core/base/TextStream.cpp:182-191 — the reference converts
    // through Boost.Locale, whose default `skip` policy drops undecodable
    // bytes; the drop is logged rather than silent.
    #[test]
    fn malformed_bytes_are_dropped_and_logged() {
        // A Shift_JIS payload with text on both sides of one byte the charset
        // cannot decode (0xA0).
        let bytes = [0x41, 0x82, 0xA0, 0xA0, 0x82, 0xB1, 0x42];
        let (text, logs) = capture_logs(|| decode_from_encoding(&bytes, SHIFT_JIS));
        assert_eq!(
            text.expect("Shift_JIS decodes with one dropped byte"),
            "AあこB"
        );
        assert_dropped_once(&logs, "Shift_JIS", 1);
    }

    // The reference assigns raw UTF-16 units without failing; a Rust `String`
    // cannot hold an unpaired surrogate, so the unit is dropped and recorded.
    #[test]
    fn fefe_payload_drops_unpaired_surrogates() {
        let bytes = fefe_mode0(&[0x0041, 0xD800, 0x0042]);
        let (text, logs) = capture_logs(|| decode_text(&bytes, TextEncoding::Detect));
        assert_eq!(text.expect("FE FE cipher decodes"), "AB");
        assert_dropped_once(&logs, "FE FE cipher", 2);
    }

    // reference/cpp/core/base/TextStream.cpp:82-105 — cipher modes 0/1 have a
    // four-byte header and the payload begins at offset 4. The ciphertext
    // below is built from the reference formula, not through a shared helper.
    #[test]
    fn fefe_cipher_payload_starts_at_offset_four() {
        // Mode 0: `(const) %` -> units 0x0028, 0x0063, 0x006f, 0x006e, 0x0073,
        // 0x0074, 0x0029, 0x0020, 0x0025. '(' is 0x0028, which is >= 0x20, so
        // it is XORed: `0x0028 ^ (((0x0028 & 0xfe) << 8) ^ 1) = 0x2829`, LE
        // `29 28`. The remaining units are XORed the same way.
        let mode0 = [
            0xFE, 0xFE, 0x00, 0x00, // `FE FE 00` + one skipped header byte
            0x29, 0x28, // '(' 0x0028 -> 0x2829
            0x62, 0x62, // 'c' 0x0063 -> 0x6262
            0x6e, 0x6e, // 'o' 0x006f -> 0x6e6e
            0x6f, 0x6e, // 'n' 0x006e -> 0x6e6f
            0x72, 0x72, // 's' 0x0073 -> 0x7272
            0x75, 0x74, // 't' 0x0074 -> 0x7475
            0x28, 0x28, // ')' 0x0029 -> 0x2828
            0x21, 0x20, // ' ' 0x0020 -> 0x2021
            0x24, 0x24, // '%' 0x0025 -> 0x2424
        ];
        assert_eq!(
            decode_text(&mode0, TextEncoding::Detect).expect("mode 0 decodes"),
            "(const) %"
        );

        // Mode 1: the same plaintext through the adjacent-bit swap.
        // 0x0028 -> 0x0014, 0x0063 -> 0x0093, 0x006f -> 0x009f, 0x006e ->
        // 0x009d, 0x0073 -> 0x00b3, 0x0074 -> 0x00b8, 0x0029 -> 0x0016,
        // 0x0020 -> 0x0010, 0x0025 -> 0x001a.
        let mode1 = [
            0xFE, 0xFE, 0x01, 0x00, // `FE FE 01` + one skipped header byte
            0x14, 0x00, 0x93, 0x00, 0x9f, 0x00, 0x9d, 0x00, 0xb3, 0x00, 0xb8, 0x00, 0x16, 0x00,
            0x10, 0x00, 0x1a, 0x00,
        ];
        assert_eq!(
            decode_text(&mode1, TextEncoding::Detect).expect("mode 1 decodes"),
            "(const) %"
        );
    }

    // reference/cpp/core/base/TextStream.cpp:87-88 — the count is
    // `(size - 4) / 2`, so a trailing odd byte is dropped and logged.
    #[test]
    fn fefe_cipher_drops_an_odd_trailing_byte() {
        let mut bytes = fefe_mode0(&[0x0041]);
        bytes.push(0x20); // a stray half code unit after the last unit
        let (text, logs) = capture_logs(|| decode_text(&bytes, TextEncoding::Detect));
        assert_eq!(text.expect("FE FE cipher decodes"), "A");
        assert_dropped_once(&logs, "FE FE cipher", 1);
    }

    // reference/cpp/core/base/TextStream.cpp:87-88 — a `FF FE` after the mode
    // byte is not a BOM: the payload still starts at offset 4, so the `FE` at
    // byte 4 is the low byte of the first code unit and must not be eaten. A
    // plaintext unit ending in 0xff (0x00ff) encrypts to 0xfefe, so the
    // container begins `FE FE 00 FF FE FE`.
    #[test]
    fn fefe_cipher_does_not_treat_ff_fe_as_a_bom() {
        let bytes = [
            0xFE, 0xFE, 0x00, 0xFF, // header: the old code read `FF FE` as a BOM
            0xFE, 0xFE, // 0xfefe -> plaintext 0x00ff
            0x40, 0x40, // 0x4040 -> plaintext 0x0041 'A'
        ];
        // The old 5-byte path would have skipped the `FE` at index 4 and read
        // no complete unit (empty text); the reference path yields "\u{ff}A".
        let (text, logs) = capture_logs(|| decode_text(&bytes, TextEncoding::Detect));
        assert_eq!(text.expect("FE FE cipher decodes"), "\u{ff}A");
        assert!(
            !logs
                .iter()
                .any(|line| line.starts_with("WARN") && line.contains("FE FE cipher")),
            "no unit or byte must be dropped: {logs:?}"
        );
    }

    // reference/cpp/core/base/TextStream.cpp:47-57 — a detectable legacy
    // charset decodes with its malformed byte dropped, the text either side
    // intact, and one warning per decode.
    #[test]
    fn detected_script_with_a_malformed_byte_is_dropped() {
        // The detector reports Shift_JIS for this script; appending a byte the
        // charset cannot decode must not fail the load.
        let script = encoded(SHIFT_JIS, SHIFT_JIS_TEXT);
        let mut bytes = script.clone();
        bytes.extend_from_slice(&script);
        bytes.extend_from_slice(&script);
        bytes.push(0xA0);
        let expected = SHIFT_JIS_TEXT.repeat(3);
        let (text, logs) = capture_logs(|| decode_text(&bytes, TextEncoding::Detect));
        assert_eq!(text.expect("detected Shift_JIS decodes"), expected);
        assert_dropped_once(&logs, "Shift_JIS", 1);
    }

    // reference/cpp/core/base/TextStream.cpp:53 — `cp932` output; :182-188 —
    // the conversion drops malformed bytes.
    #[test]
    fn override_cp932_drops_malformed_bytes() {
        let bytes = [0x41, 0x82, 0xA0, 0xA0, 0x82, 0xB1, 0x42];
        let (text, logs) = capture_logs(|| decode_text(&bytes, TextEncoding::Named("cp932")));
        assert_eq!(text.expect("cp932 decodes"), "AあこB");
        assert_dropped_once(&logs, "Shift_JIS", 1);
    }

    // reference/cpp/core/base/TextStream.cpp:182-188 — forcing UTF-8 uses the
    // same skip conversion.
    #[test]
    fn override_utf8_drops_malformed_bytes() {
        let (text, logs) =
            capture_logs(|| decode_text(&[0x41, 0xFF, 0x42], TextEncoding::Named("utf-8")));
        assert_eq!(text.expect("UTF-8 decodes"), "AB");
        assert_dropped_once(&logs, "UTF-8", 1);
    }

    // The reference's UTF-32 branch is unreachable and unsafe; this is our
    // behaviour (see `decode_utf32`), applied with the same drop rule.
    #[test]
    fn utf32_bom_drops_invalid_scalars() {
        let little = [
            0xFF, 0xFE, 0x00, 0x00, // UTF-32LE BOM
            0x41, 0x00, 0x00, 0x00, // U+0041
            0x00, 0xD8, 0x00, 0x00, // U+D800, not a scalar value
        ];
        let (text, logs) = capture_logs(|| decode_text(&little, TextEncoding::Detect));
        assert_eq!(text.expect("UTF-32LE decodes"), "A");
        assert_dropped_once(&logs, "UTF-32LE", 4);

        let big = [
            0x00, 0x00, 0xFE, 0xFF, // UTF-32BE BOM
            0x00, 0x00, 0x00, 0x41, // U+0041
            0x00, 0x00, 0xD8, 0x00, // U+D800, not a scalar value
        ];
        let (text, logs) = capture_logs(|| decode_text(&big, TextEncoding::Detect));
        assert_eq!(text.expect("UTF-32BE decodes"), "A");
        assert_dropped_once(&logs, "UTF-32BE", 4);
    }

    // The diagnostic report names the path taken for each encoding family so
    // a device log can tell a BOM, the `FE FE` container, an explicit
    // override and detection apart.
    #[test]
    fn reported_decoding_names_the_encoding_path() {
        let mut utf8 = vec![0xEF, 0xBB, 0xBF];
        utf8.extend_from_slice(b"hi");
        let reported = decode_text_reported(&utf8, TextEncoding::Detect).expect("UTF-8 BOM");
        assert_eq!(reported.encoding, "UTF-8 BOM");
        assert_eq!(reported.text, "hi");

        let container = fefe_mode0(&[0x41, 0x42]);
        let reported =
            decode_text_reported(&container, TextEncoding::Detect).expect("FE FE container");
        assert_eq!(reported.encoding, "FE FE container mode 0");
        assert_eq!(reported.text, "AB");

        let reported = decode_text_reported(b"AB", TextEncoding::Named("cp932")).expect("explicit");
        assert_eq!(reported.encoding, "explicit 'cp932'");
        assert_eq!(reported.text, "AB");

        let bytes = encoded(SHIFT_JIS, SHIFT_JIS_TEXT);
        let reported = decode_text_reported(&bytes, TextEncoding::Detect).expect("detected");
        assert!(
            reported.encoding.starts_with("detected "),
            "got: {}",
            reported.encoding
        );
    }

    // reference/cpp/core/base/TextStream.cpp:189-191 — an unsupported charset
    // raises the reference's narrow-to-wide conversion error.
    #[test]
    fn unsupported_charset_name_raises_a_typed_error() {
        let bytes = encoded(SHIFT_JIS, SHIFT_JIS_TEXT);
        let message = decode_text(&bytes, TextEncoding::Named("no-such-encoding"))
            .expect_err("an unsupported charset name must fail");
        assert!(message.contains("not supported"), "got: {message}");
    }
}
