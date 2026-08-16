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
    // CSV data is UTF-8 or CP932; decode tolerantly.
    let text = match String::from_utf8(bytes.clone()) {
        Ok(t) => t,
        Err(_) => match decode_cp932_lossy(&bytes) {
            Ok(t) => t,
            Err(_) => String::from_utf8_lossy(&bytes).into_owned(),
        },
    };
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

/// A UTF-8→CP932-lossy helper (used when the CSV is not UTF-8).
pub(crate) fn decode_cp932_lossy(bytes: &[u8]) -> Result<String, String> {
    // tvp-util's encoding module is the canonical decoder; this crate does
    // not depend on it, so implement a minimal pass-through here (the game's
    // charData.csv is UTF-8 in practice).
    let _ = bytes;
    Err("cp932 decode unavailable".into())
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
