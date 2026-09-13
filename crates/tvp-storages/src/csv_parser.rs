//! `CSVParser` native class — the csvParser.dll plugin surface the game's
//! `CharDataInit()` (System.tjs) uses:
//!
//! ```tjs
//! var csv = new CSVParser();
//! csv.initStorage("charData.csv");
//! var head = csv.getNextLine();          // array of strings
//! while((elm = csv.getNextLine()) !== void){ ... elm[i] ... }
//! ```
//!
//! Lines starting with `#` after the header are comment rows (the game
//! skips them itself). CSV quoting is minimal: a field enclosed in `"`
//! may contain commas (doubled `""` escapes a quote), matching the
//! reference plugin's common behavior.

use std::ffi::{c_char, c_int, c_void};

use tjs2_sys::{NativeInstanceBuilder, NativeInstanceMethodDef, Tjs2Engine, VAL_ARRAY, Value};

use crate::storage_arc;

#[derive(Default)]
struct CsvParserInst {
    lines: Vec<Vec<String>>,
    pos: usize,
}

extern "C" fn csv_parser_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(CsvParserInst::default())) as *mut c_void
}

extern "C" fn csv_parser_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if !instance.is_null() {
        // SAFETY: instance came from csv_parser_create.
        drop(unsafe { Box::from_raw(instance as *mut CsvParserInst) });
    }
}

/// Split one CSV line into fields (quoted fields may contain commas;
/// `""` inside quotes is a literal quote).
fn parse_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                fields.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    fields.push(cur.trim().to_string());
    fields
}

/// `initStorage(name)` — load + parse the CSV from mounted storage.
extern "C" fn csv_parser_init_storage(
    _engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload; argv/argc follow the contract.
    let inst = unsafe { &mut *(instance as *mut CsvParserInst) };
    let args = unsafe { crate::args(argc, argv) };
    if args.is_empty() {
        crate::set_error(out_error, "CSVParser.initStorage: missing storage name");
        return 1;
    }
    let name = match crate::expect_string_arg(args, "initStorage") {
        Ok(n) => n,
        Err(e) => {
            crate::set_error(out_error, &e);
            return 1;
        }
    };
    let Some(storage) = storage_arc() else {
        crate::set_error(out_error, "CSVParser.initStorage: storage not mounted");
        return 1;
    };
    let bytes = match storage.lock().unwrap().read(&name) {
        Ok(b) => b,
        Err(e) => {
            crate::set_error(out_error, &format!("CSVParser.initStorage: {e}"));
            return 1;
        }
    };
    // CSV data is usually UTF-8, UTF-16 (with a BOM) or CP932. The
    // reference detects the encoding with uchardet (which maps Shift_JIS to
    // cp932); `charData.csv` in the reference game is Windows-31J.
    let text = decode_text_lossy(&bytes);
    inst.lines = text
        .split('\n')
        .map(|l| l.trim_end_matches('\r').to_string())
        .filter(|l| !l.trim().is_empty())
        .map(|l| parse_csv_line(&l))
        .collect();
    inst.pos = 0;
    crate::set_void_out(out);
    0
}

/// `getNextLine()` — the next row as an array of field strings, or void at
/// end.
extern "C" fn csv_parser_get_next_line(
    _engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: instance is a valid payload.
    let inst = unsafe { &mut *(instance as *mut CsvParserInst) };
    if inst.pos >= inst.lines.len() {
        crate::set_void_out(out);
        return 0;
    }
    let fields = inst.lines[inst.pos].clone();
    inst.pos += 1;
    set_array_strings_out(out, &fields);
    0
}

/// `new CSVParser()` — no-op constructor.
extern "C" fn csv_parser_ctor(
    _engine: *mut c_void,
    _instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    crate::set_void_out(out);
    0
}

/// Register the `CSVParser` native class on the engine's global object.
pub fn register_csv_parser(engine: &Tjs2Engine) -> Result<(), String> {
    engine.register_native_class_instance(&NativeInstanceBuilder {
        name: "CSVParser",
        create: csv_parser_create,
        destroy: csv_parser_destroy,
        methods: vec![
            NativeInstanceMethodDef {
                name: "CSVParser",
                f: csv_parser_ctor,
            },
            NativeInstanceMethodDef {
                name: "initStorage",
                f: csv_parser_init_storage,
            },
            NativeInstanceMethodDef {
                name: "getNextLine",
                f: csv_parser_get_next_line,
            },
        ],
        properties: vec![],
    })
}

/// Decode a text storage to UTF-8, honoring a leading BOM and falling back
/// to CP932 when the bytes are not valid UTF-8.
///
/// This mirrors the reference's `TextStream.cpp` behavior (BOM detection,
/// then `uchardet`, which reports Shift_JIS as `cp932`). It is
/// **infallible**: undecodable bytes are replaced with U+FFFD, so a corrupt
/// CSV never aborts `CharDataInit`.
pub(crate) fn decode_text_lossy(bytes: &[u8]) -> String {
    // A BOM pins the encoding exactly (UTF-8/16/32).
    let (body, bom) = tvp_util::encoding::strip_bom(bytes);
    if let Some(encoding) = bom
        && let Ok(text) = encoding.decode(body)
    {
        return text;
    }
    if let Ok(text) = std::str::from_utf8(body) {
        return text.to_owned();
    }
    // CP932 (Windows-31J) is the effective result for Japanese game data;
    // `encoding_rs` substitutes U+FFFD for invalid sequences.
    let (text, _, _) = encoding_rs::SHIFT_JIS.decode(body);
    text.into_owned()
}

thread_local! {
    /// Scratch state for array-string returns: the NUL-terminated string
    /// buffers plus the pointer array into them. Both live together until
    /// the next native call on this thread; the C++ side copies the
    /// elements into a TJS array before the callback returns.
    static ARRAY_OUT: std::cell::RefCell<(Vec<*const c_char>, Vec<Vec<u8>>)> =
        const { std::cell::RefCell::new((Vec::new(), Vec::new())) };
}

/// Write an array-of-strings return value into `*out`.
pub(crate) fn set_array_strings_out(out: *mut Value, items: &[String]) {
    ARRAY_OUT.with(|slot| {
        let mut slot = slot.borrow_mut();
        slot.1.clear();
        let mut ptrs: Vec<*const c_char> = Vec::with_capacity(items.len());
        for s in items {
            let mut b = s.as_bytes().to_vec();
            b.push(0);
            slot.1.push(b);
        }
        for b in &slot.1 {
            ptrs.push(b.as_ptr() as *const c_char);
        }
        slot.0 = ptrs;
        // SAFETY: out is a valid return slot for the call; slot.0/slot.1
        // stay alive until the next call on this thread (the C++ side
        // copies immediately).
        unsafe {
            (*out).ty = VAL_ARRAY;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = std::ptr::null();
            (*out).array = slot.0.as_ptr();
            (*out).array_count = slot.0.len() as c_int;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf8_with_and_without_bom() {
        assert_eq!(decode_text_lossy(b"a,b\n1,2"), "a,b\n1,2");
        let mut bom = tvp_util::encoding::UTF8_BOM.to_vec();
        bom.extend_from_slice("名前,値".as_bytes());
        assert_eq!(decode_text_lossy(&bom), "名前,値");
    }

    #[test]
    fn decodes_utf16le_with_bom() {
        let mut bytes = tvp_util::encoding::UTF16LE_BOM.to_vec();
        bytes.extend_from_slice(
            &"名前,値"
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        assert_eq!(decode_text_lossy(&bytes), "名前,値");
    }

    #[test]
    fn decodes_cp932_japanese() {
        // "キャラ名,テスト" in Windows-31J (cp932). This is the actual encoding
        // of the reference game's `system/charData.csv`.
        let cp932 = [
            0x83, 0x4c, 0x83, 0x83, 0x83, 0x89, 0x96, 0xbc, 0x2c, 0x83, 0x65, 0x83, 0x58, 0x83,
            0x67,
        ];
        assert_eq!(decode_text_lossy(&cp932), "キャラ名,テスト");
        // A CP932 string that also happens to contain an ASCII comma still
        // decodes (the comma is a normal byte in the legacy encoding).
        assert!(decode_text_lossy(&cp932).contains(','));
    }

    #[test]
    fn invalid_bytes_are_replaced_not_panicked() {
        // 0xFF is not a valid CP932 lead byte; the decoder must not panic and
        // must yield a replacement character.
        let text = decode_text_lossy(&[b'a', 0xFF, b'b']);
        assert!(text.starts_with('a') && text.ends_with('b'));
        assert!(text.contains('\u{FFFD}'), "got {text:?}");
    }

    #[test]
    fn csv_line_parsing_quotes_and_commas() {
        assert_eq!(parse_csv_line("a,b,c"), ["a", "b", "c"]);
        assert_eq!(parse_csv_line("\"a,b\",c"), ["a,b", "c"]);
        assert_eq!(parse_csv_line("\"a\"\"b\",c"), ["a\"b", "c"]);
    }
}
