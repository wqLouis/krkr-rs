//! Built-in `Array/Dictionary.saveStruct` text-stream round-trip.
//!
//! The reference `tTVPTextWriteStream` lays a compressed save out as the
//! three signature bytes `FE FE <mode>`, the UTF-16LE BOM `FF FE`, two
//! little-endian 64-bit sizes (compressed then uncompressed) and the zlib
//! stream, and patches the sizes when the stream closes
//! (`reference/cpp/core/base/TextStream.cpp:279-291`, `:296-299`,
//! `:337-357`). The serializer that feeds it is
//! `reference/cpp/core/tjs2/tjsArray.cpp:1114-1198` and
//! `reference/cpp/core/tjs2/tjsDictionary.cpp:417-447`; the game's save is
//! `temp = [_param]` with `_param` a `Dictionary`
//! (`system_advscreen.tjs:7346-7440`).
//!
//! The test writes a realistic payload through the built-in
//! `Array.saveStruct`, then checks the container layout directly and decodes
//! the zlib payload through the same reader the VM uses on load.
//!
//! The write mode selects the output form. `reference/cpp/core/base/BinaryStream.h:22-39`
//! parses a mode number into an optional — a key that is absent yields the
//! caller's default, a key that is present without a digit yields an empty
//! optional, a key with a digit yields the number — and
//! `reference/cpp/core/base/TextStream.cpp:245-258` turns that into the
//! cipher mode: the `c` digit, 1 for a `c` without a number, -1 when `c` is
//! absent, and the compressed container for a `z` without a number. A mode
//! without `c` and without `z` therefore writes plain UTF-16LE text, which
//! is the form a game save takes.

use std::ffi::{CStr, CString, c_char, c_int};
use std::path::PathBuf;
use std::ptr;
use std::sync::{Mutex, MutexGuard};

use tjs::{Tjs2Engine, TjsValue};

/// The C++ TJS2 VM keeps process-global state; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

// The reader helper is implemented in cpp/streams.cpp — the same decoder the
// VM's `loadStruct` uses.
unsafe extern "C" {
    fn tjs2_read_text_stream_all(
        path_utf8: *const c_char,
        mode_utf8: *const c_char,
        out_utf8: *mut *mut c_char,
    ) -> c_int;
    fn tjs2_free_string(s: *mut c_char);
}

fn temp_path(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!(
        "tjs-save-struct-{}-{nanos}-{name}",
        std::process::id()
    ))
}

fn decode_stream(path: &std::path::Path, mode: &str) -> String {
    let path_c = CString::new(path.to_string_lossy().as_bytes()).unwrap();
    let mode_c = CString::new(mode).unwrap();
    let mut out: *mut c_char = ptr::null_mut();
    // SAFETY: both C strings live for the call; the helper writes a malloc'd
    // NUL-terminated UTF-8 string into `out`.
    let rc = unsafe { tjs2_read_text_stream_all(path_c.as_ptr(), mode_c.as_ptr(), &mut out) };
    assert_eq!(rc, 0, "tjs2_read_text_stream_all failed for mode {mode:?}");
    assert!(!out.is_null(), "the reader returned a null string");
    // SAFETY: out is a NUL-terminated malloc'd string owned by us.
    let text = unsafe { CStr::from_ptr(out) }
        .to_string_lossy()
        .into_owned();
    // SAFETY: the helper allocated it with malloc; free it exactly once.
    unsafe { tjs2_free_string(out) };
    text
}

fn read_u64le(bytes: &[u8], at: usize) -> u64 {
    let mut v = 0u64;
    for i in 0..8 {
        v |= (bytes[at + i] as u64) << (i * 8);
    }
    v
}

/// The realistic save payload: a `Dictionary` with a UTF-16 non-ASCII string,
/// an integer, a real, an octet, a void, a nested `Array` and a nested
/// `Dictionary`, wrapped exactly as the game wraps `_param`
/// (`system_advscreen.tjs:7359-7440`).
const PAYLOAD_SCRIPT: &str = "\
    var d = %[];\n\
    d[\"unicode\"] = \"日本語 αβγ 😀\";\n\
    d[\"integer\"] = 42;\n\
    d[\"real\"] = 3.5;\n\
    d[\"octet\"] = <% 01 02 fe ff %>;\n\
    d[\"nothing\"] = void;\n\
    d[\"arr\"] = [1, \"two\", 3.5, void];\n\
    d[\"nested\"] = %[ \"key\" => \"value\", \"num\" => 7 ];\n\
    var temp = [];\n\
    temp[0] = d;\n";

#[test]
fn builtin_save_struct_writes_the_reference_crypt_container() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let path = temp_path("container");
    let path_lit = format!("{:?}", path.to_string_lossy());

    engine
        .exec_script(
            &format!("{PAYLOAD_SCRIPT}temp.saveStruct({path_lit}, \"z\");"),
            "save_struct",
        )
        .unwrap();

    let bytes = std::fs::read(&path).unwrap();

    // reference/cpp/core/base/TextStream.cpp:277-291: the crypt signature
    // (`FE FE <mode>`, mode 2 for zlib) then the UTF-16LE BOM.
    assert_eq!(
        &bytes[..5],
        &[0xFE, 0xFE, 0x02, 0xFF, 0xFE],
        "crypt signature + UTF-16LE BOM"
    );

    // reference/cpp/core/base/TextStream.cpp:296-299: two LE64 sizes follow
    // the BOM, compressed then uncompressed.
    let compressed = read_u64le(&bytes, 5);
    let uncompressed = read_u64le(&bytes, 13);

    // reference/cpp/core/base/TextStream.cpp:337-357: the close patches the
    // two placeholders with `total_out`/`total_in`, so the compressed field
    // is exactly the zlib payload length and nothing is left unwritten.
    assert_eq!(
        bytes.len() as u64,
        5 + 16 + compressed,
        "the file must be the header plus the measured zlib payload"
    );

    // The payload must decode back to the exact UTF-16 text through the same
    // reader the VM uses on load.
    let text = decode_stream(&path, "");
    let _ = std::fs::remove_file(&path);

    assert_eq!(
        text.encode_utf16().count() as u64 * 2,
        uncompressed,
        "the uncompressed size field must count the UTF-16 payload exactly"
    );

    // reference/cpp/core/tjs2/tjsArray.cpp:1114-1156: the array form.
    assert!(
        text.starts_with("(const) [\r\n"),
        "unexpected struct: {text:?}"
    );
    // reference/cpp/core/tjs2/tjsDictionary.cpp:417-447: the map form.
    assert!(
        text.contains("(const) %["),
        "the nested dictionary must serialize: {text:?}"
    );

    // reference/cpp/core/tjs2/tjsDictionary.cpp:465-490: each member is
    // `"<key>" => <expression>`.
    assert!(
        text.contains("\"unicode\" => \"日本語 αβγ 😀\""),
        "the UTF-16 string must round-trip verbatim: {text:?}"
    );
    assert!(
        text.contains("\"integer\" => 42"),
        "missing integer: {text:?}"
    );
    assert!(
        text.contains("\"real\" => 0x") && text.contains("/* 3.5 */"),
        "the real must serialize as hex with a decimal comment: {text:?}"
    );
    // reference/cpp/core/tjs2/tjsUtils.cpp:205-217: octets render as
    // `<% XX XX %>` (uppercase hex, space separated).
    assert!(
        text.contains("\"octet\" => <% 01 02 FE FF %>"),
        "missing octet: {text:?}"
    );
    assert!(text.contains("void"), "missing void: {text:?}");
    assert!(
        text.contains("\"nested\" => (const) %["),
        "missing nested dictionary: {text:?}"
    );

    // The decoded text must be re-interpretable as the expression the
    // reference's loader evaluates: read the values back and compare.
    engine
        .exec_script(&format!("var __restored = {text};"), "restore")
        .unwrap();
    assert_eq!(
        engine
            .eval("__restored[0][\"integer\"]", "restore")
            .unwrap(),
        TjsValue::Integer(42)
    );
    assert_eq!(
        engine.eval("__restored[0][\"real\"]", "restore").unwrap(),
        TjsValue::Real(3.5)
    );
    assert_eq!(
        engine
            .eval("__restored[0][\"unicode\"]", "restore")
            .unwrap(),
        TjsValue::String("日本語 αβγ 😀".into())
    );
    assert_eq!(
        engine
            .eval("typeof __restored[0][\"octet\"]", "restore")
            .unwrap(),
        TjsValue::String("Octet".into())
    );
    assert_eq!(
        engine
            .eval("__restored[0][\"nested\"][\"key\"]", "restore")
            .unwrap(),
        TjsValue::String("value".into())
    );
    assert_eq!(
        engine.eval("__restored[0][\"arr\"][1]", "restore").unwrap(),
        TjsValue::String("two".into())
    );
}

/// The output form a write mode selects.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WrittenForm {
    /// Plain UTF-16LE text with a byte-order mark and no container.
    Plain,
    /// The `FE FE 01 FF FE` cipher container.
    Cipher,
    /// The `FE FE 02 FF FE` compressed container.
    Compressed,
}

/// Classifies the bytes a write produced.
fn written_form(bytes: &[u8]) -> WrittenForm {
    match bytes {
        [0xFF, 0xFE, ..] => WrittenForm::Plain,
        [0xFE, 0xFE, 0x01, ..] => WrittenForm::Cipher,
        [0xFE, 0xFE, 0x02, ..] => WrittenForm::Compressed,
        other => panic!("unexpected file header: {other:?}"),
    }
}

/// The mode string selects the output form of a text write stream.
///
/// `reference/cpp/core/base/BinaryStream.h:22-39` — `parseModeNumber` returns
/// an optional: a key that is absent yields the caller's default, a key that
/// is present without a digit yields an empty optional, and a key with a digit
/// yields the number.
///
/// `reference/cpp/core/base/TextStream.cpp:245-258` — the writer reads the
/// cipher mode from the `c` key (its digit, 1 for a `c` without a number,
/// -1 when the key is absent) and selects the compressed container (cipher
/// mode 2) when a `z` key carries no number. The `z` number is the
/// compression level; the cipher mode stays what the `c` key decided, so
/// `zN` on its own leaves the text plain.
///
/// | mode   | cipher mode | output                      |
/// |--------|-------------|-----------------------------|
/// | `""`   | -1          | plain UTF-16LE text         |
/// | `"z5"` | -1          | plain UTF-16LE text         |
/// | `"c"`  | 1           | the simple cipher container |
/// | `"c1"` | 1           | the simple cipher container |
/// | `"c2"` | 2           | the compressed container    |
/// | `"z"`  | 2           | the compressed container    |
#[test]
fn the_write_mode_selects_the_output_form() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    for (mode, expected) in [
        ("", WrittenForm::Plain),
        ("w", WrittenForm::Plain),
        ("z5", WrittenForm::Plain),
        ("c", WrittenForm::Cipher),
        ("c1", WrittenForm::Cipher),
        ("c2", WrittenForm::Compressed),
        ("z", WrittenForm::Compressed),
    ] {
        let path = temp_path(&format!("mode-{mode}"));
        let path_lit = format!("{:?}", path.to_string_lossy());
        engine
            .exec_script(
                &format!("{PAYLOAD_SCRIPT}temp.saveStruct({path_lit}, {mode:?});"),
                "save_struct_mode",
            )
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(written_form(&bytes), expected, "mode {mode:?}");
    }
}

/// A cipher mode outside {-1, 1, 2} raises the mode-string error; the writer
/// does not pick a mode of its own.
///
/// `reference/cpp/core/base/TextStream.cpp:256-258` — the cipher mode must be
/// -1, 1 or 2, otherwise the writer raises the unsupported-mode-string error
/// with "unsupported cipher mode" as its argument. The message format is
/// "%1 is unsupported mode string" (`reference/cpp/core/base/MsgIntfInc.h:46`),
/// so the raised message reads "unsupported cipher mode is unsupported mode
/// string".
#[test]
fn an_unsupported_cipher_mode_raises() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    for mode in ["c0", "c3"] {
        let path = temp_path(&format!("raise-{mode}"));
        let path_lit = format!("{:?}", path.to_string_lossy());
        let error = engine
            .exec_script(
                &format!("{PAYLOAD_SCRIPT}temp.saveStruct({path_lit}, {mode:?});"),
                "save_struct_raise",
            )
            .expect_err("an unsupported cipher mode must raise");
        assert!(
            error
                .to_string()
                .contains("unsupported cipher mode is unsupported mode string"),
            "mode {mode:?}: {error}"
        );
        assert!(!path.exists(), "the file must not be created");
    }
}

/// The plain form is the UTF-16LE text after a byte-order mark, and the
/// reader decodes it.
///
/// `reference/cpp/core/base/TextStream.cpp:136-190` — a payload that does not
/// start with the `FE FE` container signature is read as text, and the
/// UTF-16LE byte-order mark selects the UTF-16 decoder.
#[test]
fn the_plain_form_is_the_utf16_text_and_round_trips() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let path = temp_path("plain");
    let path_lit = format!("{:?}", path.to_string_lossy());

    engine
        .exec_script(
            &format!("{PAYLOAD_SCRIPT}temp.saveStruct({path_lit}, \"\");"),
            "save_struct_plain",
        )
        .unwrap();

    let bytes = std::fs::read(&path).unwrap();
    let text = decode_stream(&path, "");
    let _ = std::fs::remove_file(&path);

    // The byte-order mark and nothing else precedes the text.
    assert_eq!(
        &bytes[..2],
        &[0xFF, 0xFE],
        "the plain form starts with the UTF-16LE byte-order mark"
    );
    let expected: Vec<u8> = text
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    assert_eq!(
        &bytes[2..],
        expected.as_slice(),
        "the plain form is the UTF-16LE text after the byte-order mark"
    );

    // The reader hands the same struct back.
    assert!(
        text.starts_with("(const) [\r\n"),
        "unexpected struct: {text:?}"
    );
    assert!(
        text.contains("\"integer\" => 42"),
        "missing integer: {text:?}"
    );
}

/// A declared uncompressed length that is not a whole number of UTF-16 code
/// units is truncated to `uncompressed / 2` code units, as the reference's
/// `_buffer.assign` does; it is not a failure.
#[test]
fn builtin_reader_truncates_an_odd_utf16_payload_length() {
    let _lock = vm_lock();
    // `FE FE 02 FF FE`, compressed=11, uncompressed=3, then zlib("ABC").
    // The declared size is odd: the reader takes `3 / 2 = 1` whole
    // `char16_t` unit (`0x4241` from bytes `41 42`) and drops the trailing
    // `0x43`.
    let fixture: &[u8] = &[
        0xfe, 0xfe, 0x02, 0xff, 0xfe, 0x0b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x78, 0x9c, 0x73, 0x74, 0x72, 0x06, 0x00, 0x01, 0x8d,
        0x00, 0xc7,
    ];
    let path = temp_path("odd");
    std::fs::write(&path, fixture).unwrap();
    let text = decode_stream(&path, "");
    let _ = std::fs::remove_file(&path);
    // reference/cpp/core/base/TextStream.cpp:129-131: `_buffer.assign` reads
    // the decompressed bytes as `char16_t` and the end pointer advances by
    // `uncompressed` bytes, so the buffer holds `uncompressed / 2` code units
    // and the odd trailing byte is dropped rather than rejected.
    assert_eq!(text, "䉁", "the odd trailing byte must be truncated");
}

#[test]
fn builtin_save_struct_writes_at_the_offset_and_preserves_the_prefix() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let path = temp_path("offset");
    let path_lit = format!("{:?}", path.to_string_lossy());

    // The game writes the thumbnail first and appends the struct at
    // `GetImageFileSize(sprThumb)` (`system_advscreen.tjs:7427,7440`).
    let prefix = b"KRKRSAVE"; // 8 bytes
    std::fs::write(&path, prefix).unwrap();

    engine
        .exec_script(
            &format!("{PAYLOAD_SCRIPT}temp.saveStruct({path_lit}, \"zo8\");"),
            "save_struct_offset",
        )
        .unwrap();

    let bytes = std::fs::read(&path).unwrap();

    // reference/cpp/core/base/TextStream.cpp:308-317: `oN` opens the stream
    // for update and seeks to N, so the prefix must survive untouched.
    assert_eq!(&bytes[..prefix.len()], prefix, "the prefix was clobbered");

    assert_eq!(
        &bytes[prefix.len()..prefix.len() + 5],
        &[0xFE, 0xFE, 0x02, 0xFF, 0xFE],
        "crypt signature + UTF-16LE BOM at the offset"
    );
    let compressed = read_u64le(&bytes, prefix.len() + 5);
    assert_eq!(
        bytes.len() as u64,
        prefix.len() as u64 + 5 + 16 + compressed,
        "the appended payload must be measured from the offset"
    );

    // The reader honors the same `oN` offset, so the decoded text must match
    // an unprefixed save of the same payload.
    let text = decode_stream(&path, &format!("o{}", prefix.len()));
    let _ = std::fs::remove_file(&path);
    assert!(
        text.starts_with("(const) [\r\n"),
        "unexpected struct: {text:?}"
    );
    assert!(
        text.contains("\"integer\" => 42"),
        "missing integer after the offset: {text:?}"
    );
}
