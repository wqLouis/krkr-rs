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

/// A declared uncompressed length that is not a whole number of UTF-16 code
/// units cannot be decoded. The reader must reject it instead of copying
/// `uncompressed` bytes into a `uncompressed / 2`-element buffer.
#[test]
fn builtin_reader_rejects_an_odd_utf16_payload_length() {
    let _lock = vm_lock();
    // `FE FE 02 FF FE`, compressed=11, uncompressed=3, then zlib("ABC").
    // The declared size is odd, so no whole number of UTF-16 units follows.
    let fixture: &[u8] = &[
        0xfe, 0xfe, 0x02, 0xff, 0xfe, 0x0b, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x78, 0x9c, 0x73, 0x74, 0x72, 0x06, 0x00, 0x01, 0x8d,
        0x00, 0xc7,
    ];
    let path = temp_path("odd");
    std::fs::write(&path, fixture).unwrap();
    let text = decode_stream(&path, "");
    let _ = std::fs::remove_file(&path);
    // reference/cpp/core/base/TextStream.cpp:313-334: the decompressed bytes
    // are read as `char16_t` units, so an odd byte count is not a decodable
    // UTF-16 stream.
    assert!(
        text.is_empty(),
        "an odd declared UTF-16 length must not yield text: {text:?}"
    );
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
