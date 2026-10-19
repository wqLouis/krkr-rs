//! `CSVParser` native class — the `csvParser.dll` plugin surface.
//!
//! Behavioral reference: `reference/cpp/plugins/csvParser.cpp`. The plugin
//! parses CSV text (a string or a storage file) into rows of field strings and
//! either returns them one at a time (`getNextLine`) or drives a caller-supplied
//! `target.doLine(fields, lineNo)` callback (`parse` / `parseStorage`).
//!
//! This is a faithful port of the reference state machine, not a simplified
//! CSV splitter. The `csv` crate is deliberately *not* used: `split`
//! deviates from RFC 4180 in observable ways — after a closing quote it
//! copies the remainder up to the next separator into the field, an
//! unterminated quote continues on the next physical line joined with the
//! configured `newline` string, and the line buffer persists across calls —
//! none of which its RFC-conforming reader models:
//!
//! * [`IFileStr`] is the reference `IFileStr` character-source cursor
//!   (`getc`/`ungetc`/`eof`/`endOfLine`/`addNextLine`). `addNextLine`
//!   **appends** the next physical line to the caller's buffer, which is how
//!   a quoted field may span lines.
//! * [`CsvParserInst::split`] mirrors `NI_CSVParser::split` exactly, including
//!   the `""` escape, the closing-quote remainder copy and the multi-line
//!   continuation joined with the configured `newline`.
//!
//! Every piece of state (the parse cursor, separator, newline, line number and
//! the constructor `target`) lives in the per-instance payload; there are no
//! process globals. The class is registered on the VM global object, so a game
//! `startup.tjs` doing `new CSVParser()` works.
#![allow(unsafe_code)]

use std::ffi::{c_char, c_int, c_void};

use tjs::{
    DetachedValue, NativeInstanceBuilder, NativeInstanceMethodDef, NativeInstancePropertyDef,
    RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue, VAL_OBJECT, VAL_STRING, VAL_VOID, Value,
};

use super::super::system::{resolve, try_value_as_i64, value_as_i64};
use crate::native_util::{args, report_error, set_int, set_void, value_as_string};
use crate::storage::strip_file_media;
use crate::{CompatCtx, CompatError};

// ---------------------------------------------------------------------------
// Reference-strict variant conversions
// ---------------------------------------------------------------------------

/// Reference `tTJSVariant::AsStringNoAddRef` (`core/tjs2/tjsVariant.h:777-789`):
/// a string passes through, `void` converts to no string (`nullptr`, which
/// the reference callers treat as an empty source), and every other type
/// raises `TJSThrowVariantConvertError`.
fn strict_string(value: &Value) -> Result<String, String> {
    match value.ty {
        VAL_STRING | VAL_VOID => Ok(value_as_string(value)),
        _ => Err(format!(
            "cannot convert a type-{ty} value to a string",
            ty = value.ty
        )),
    }
}

// ---------------------------------------------------------------------------
// Character source (reference `IFileStr`)
// ---------------------------------------------------------------------------

/// The reference `IFileStr`: a character cursor over decoded text.
///
/// The reference operates on UTF-16 code units (`ttstr`); this port keeps
/// Unicode scalar values, which is equivalent for every BMP character and for
/// the separators/newlines CSV data uses.
struct IFileStr {
    chars: Vec<char>,
    pos: usize,
}

impl IFileStr {
    fn new(text: &str) -> Self {
        Self {
            chars: text.chars().collect(),
            pos: 0,
        }
    }

    /// Reference `getc`: the next character, or `None` at end of input.
    fn getc(&mut self) -> Option<char> {
        let c = self.chars.get(self.pos).copied();
        if c.is_some() {
            self.pos += 1;
        }
        c
    }

    /// Reference `ungetc`: step back one character (never before the start).
    fn ungetc(&mut self) {
        if self.pos > 0 {
            self.pos -= 1;
        }
    }

    /// Reference `eof`.
    fn eof(&self) -> bool {
        self.pos >= self.chars.len()
    }

    /// Reference `endOfLine`: consume one line terminator, treating `\r\n` as
    /// a unit. A lone `\r` that is not followed by `\n` is pushed back.
    fn end_of_line(&mut self, c: char) -> bool {
        let eol = c == '\r' || c == '\n';
        if c == '\r' {
            let next = self.getc();
            if !self.eof() && next != Some('\n') {
                self.ungetc();
            }
        }
        eol
    }

    /// Reference `addNextLine`: append the next physical line (without its
    /// terminator) to `out`; returns whether any input (even an empty line)
    /// was available.
    fn add_next_line(&mut self, out: &mut Vec<char>) -> bool {
        let mut length = 0usize;
        loop {
            match self.getc() {
                // EOF: true only when at least one character was appended.
                None => return length > 0,
                Some(c) => {
                    if self.end_of_line(c) {
                        return true;
                    }
                    out.push(c);
                    length += 1;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Per-instance payload
// ---------------------------------------------------------------------------

/// Per-object `CSVParser` payload.
///
/// `target` is the retained constructor argument (or `None`, in which case
/// `parse` falls back to the native object itself). `file` is the current
/// character source; `line` is the reference's scratch line buffer, which
/// `split` may extend across physical lines.
struct CsvParserInst {
    target: Option<DetachedValue>,
    file: Option<IFileStr>,
    line_no: i64,
    separator: char,
    newline: String,
    line: Vec<char>,
}

impl Default for CsvParserInst {
    fn default() -> Self {
        Self {
            target: None,
            file: None,
            line_no: 0,
            separator: ',',
            newline: "\r\n".to_string(),
            line: Vec::new(),
        }
    }
}

impl CsvParserInst {
    /// Reference `NI_CSVParser::clear`: drop the character source.
    fn clear(&mut self) {
        self.file = None;
    }

    /// Reference `NI_CSVParser::addline`.
    fn add_line(&mut self) -> bool {
        match self.file.as_mut() {
            Some(file) => file.add_next_line(&mut self.line),
            None => false,
        }
    }

    /// Reference `NI_CSVParser::find`: the first index `>= start` holding
    /// `ch`, or the line length when it is absent.
    fn find(line: &[char], ch: char, start: usize) -> usize {
        let start = start.min(line.len());
        match line[start..].iter().position(|&c| c == ch) {
            Some(offset) => start + offset,
            None => line.len(),
        }
    }

    /// Reference `NI_CSVParser::init`.
    fn init(&mut self, text: &str) {
        self.clear();
        self.file = Some(IFileStr::new(text));
        self.line_no = 0;
    }

    /// Reference `NI_CSVParser::getNextLine` (the returned fields).
    fn next_line(&mut self, host: &Tjs2Engine) -> Result<Option<DetachedValue>, String> {
        self.line.clear();
        if self.add_line() {
            self.line_no += 1;
            let fields = match host.eval_retained("[]", "CSVParser.getNextLine") {
                Ok(RetainedValue::Object(object)) => object,
                Ok(RetainedValue::Value(value)) => {
                    return Err(format!("expected an Array object, got {value:?}"));
                }
                Err(error) => return Err(error.to_string()),
            };
            self.split(host, fields.raw_id())?;
            Ok(Some(fields))
        } else {
            self.clear();
            Ok(None)
        }
    }

    /// Reference `NI_CSVParser::split`: split `self.line` into fields and
    /// append them to the TJS array `fields`.
    fn split(&mut self, host: &Tjs2Engine, fields: Tjs2ValueId) -> Result<(), String> {
        // An empty line yields an empty array (the reference returns before
        // registering anything).
        if self.line.is_empty() {
            return Ok(());
        }

        let mut count: i64 = 0;
        let mut i: usize = 0;
        loop {
            let (field, j): (String, usize) = if i < self.line.len() && self.line[i] == '"' {
                // Quoted field: scan to the closing quote, folding `""`
                // into a literal quote and continuation lines into
                // `newline`.
                i += 1;
                let mut value = String::new();
                let mut jj = i;
                'inner: loop {
                    while jj < self.line.len() {
                        if self.line[jj] == '"' {
                            jj += 1;
                            if self.line.get(jj).copied() != Some('"') {
                                // Closing quote: copy the remaining
                                // characters up to the next separator
                                // (reference behavior) and finish.
                                let mut k = Self::find(&self.line, self.separator, jj);
                                if k > self.line.len() {
                                    k = self.line.len();
                                }
                                while jj < k {
                                    value.push(self.line[jj]);
                                    jj += 1;
                                }
                                break 'inner (value, jj);
                            }
                            // `""` escape: fall through and append the
                            // second quote.
                        }
                        value.push(self.line[jj]);
                        jj += 1;
                    }
                    // The quoted field is unterminated: join the next
                    // physical line (appended to `self.line`) with
                    // `newline` and keep scanning from where we stopped.
                    value.push_str(&self.newline);
                    if !self.add_line() {
                        break 'inner (value, jj);
                    }
                }
            } else {
                // Unquoted field: up to the next separator (or the line
                // end).
                let mut jj = Self::find(&self.line, self.separator, i);
                if jj > self.line.len() {
                    jj = self.line.len();
                }
                (self.line[i..jj].iter().collect(), jj)
            };

            host.set_member(fields, &count.to_string(), &TjsValue::String(field))
                .map_err(|error| format!("could not set field {count}: {error}"))?;
            count += 1;

            i = j + 1;
            if j >= self.line.len() {
                break;
            }
        }
        Ok(())
    }

    /// Reference `NI_CSVParser::parse`: while lines remain, invoke
    /// `target.doLine(fields, lineNo)` on the constructor target (or the
    /// native object when no target was given).
    fn parse(&mut self, host: &Tjs2Engine, objthis: *mut c_void) -> Result<(), String> {
        if self.file.is_none() {
            return Ok(());
        }
        // Resolve the dispatch target. The `objthis` fallback is retained for
        // the duration of the loop so it stays valid across re-entrant calls.
        let fallback = if self.target.is_none() && !objthis.is_null() {
            host.retain_object_detached(objthis).ok()
        } else {
            None
        };
        let target = self
            .target
            .as_ref()
            .map(DetachedValue::raw_id)
            .or_else(|| fallback.as_ref().map(DetachedValue::raw_id));
        let Some(target) = target else {
            return Ok(());
        };
        // The reference only runs when the target actually has a valid
        // `doLine` member (`reference/cpp/plugins/csvParser.cpp:98-101`,
        // guard at `:323`). A Dictionary resolves an absent member to
        // `TJS_S_OK` plus a cleared (void) result
        // (`crates/tjs/cpp/tjs2/tjsDictionary.cpp:732-745`), so a void or a
        // failed lookup both mean there is no callback to drive.
        match host.member(target, "doLine") {
            Ok(TjsValue::Void) | Err(_) => return Ok(()),
            Ok(_) => {}
        }
        while let Some(fields) = self.next_line(host)? {
            let fields_id = fields.raw_id();
            std::mem::forget(fields);
            let arguments = [
                TjsValue::Retained(fields_id.as_ptr() as usize as u64),
                TjsValue::Integer(self.line_no),
            ];
            host.call_member(target, "doLine", &arguments)?;
        }
        self.clear();
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Storage access
// ---------------------------------------------------------------------------

/// Read `filename` through the attached L2 storage and decode it.
///
/// With `utf8` set the raw bytes are interpreted strictly as UTF-8 (the
/// reference's `char *buff` path); otherwise the normal text decoder
/// ([`crate::text::decode_text`]) detects the encoding.
fn read_storage_text(engine: *mut c_void, filename: &str, utf8: bool) -> Result<String, String> {
    let Some(resolved) = resolve(engine) else {
        return Err("compat context is not bound".to_string());
    };
    let ctx = resolved.ctx();
    // SAFETY: `resolve` only succeeds for the installed, live binding and the
    // host keeps the attached storage alive for the VM lifetime.
    let storage = unsafe { ctx.services() }.storage();
    let Some(storage) = storage else {
        return Err("no storage attached".to_string());
    };
    // SAFETY: see above.
    let bytes = unsafe { &*storage }
        .read(strip_file_media(filename))
        .map_err(|error| error.to_string())?;
    if utf8 {
        String::from_utf8(bytes).map_err(|error| format!("UTF-8: {error}"))
    } else {
        crate::text::decode_text(&bytes, crate::text::TextEncoding::Detect)
    }
}

// ---------------------------------------------------------------------------
// Result marshalling
// ---------------------------------------------------------------------------

/// Write a retained object into `out` as `VAL_RETAINED`; the C++ trampoline
/// consumes the retention when it copies the result.
fn set_retained_out(out: *mut Value, object: DetachedValue) {
    if !out.is_null() {
        // SAFETY: the trampoline passes a valid out slot for the call.
        unsafe {
            (*out).ty = tjs::VAL_RETAINED;
            (*out).integer = 0;
            (*out).real = 0.0;
            (*out).string = std::ptr::null();
            (*out).array = std::ptr::null();
            (*out).array_count = 0;
            (*out).retained = object.raw_id().as_ptr() as usize;
        }
    }
    // The C++ conversion consumes the id; forgetting the wrapper avoids a
    // redundant (but safe) release.
    std::mem::forget(object);
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

extern "C" fn csv_create(_engine: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(CsvParserInst::default())) as *mut c_void
}

extern "C" fn csv_destroy(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` came from `csv_create` and is released once.
    drop(unsafe { Box::from_raw(instance as *mut CsvParserInst) });
}

/// Reference `NI_CSVParser::Invalidate`: release the character source and the
/// constructor target. The payload itself is freed later by `csv_destroy`.
extern "C" fn csv_invalidate(_engine: *mut c_void, instance: *mut c_void) {
    if instance.is_null() {
        return;
    }
    // SAFETY: `instance` is the live payload the VM passed to the callback.
    let inst = unsafe { &mut *(instance as *mut CsvParserInst) };
    inst.clear();
    inst.target = None;
}

/// `new CSVParser(target?, separator?, newline?)` — reference `Construct`.
extern "C" fn csv_ctor(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "CSVParser: compat context is not bound");
    };
    let host = resolved.engine();
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries, and
    // `instance` is the live payload for the call.
    let argv = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &mut *(instance as *mut CsvParserInst) };

    if let Some(target) = argv.first() {
        // Reference `Construct` (`csvParser.cpp:224-231`) runs
        // `param[0]->AsObject()`: a non-object, non-null argument raises the
        // variant-convert error (`core/tjs2/tjsVariant.h:682-692`), `null`
        // yields a null dispatch (no target), and an object is retained.
        match target.ty {
            VAL_OBJECT if target.object_handle().is_null() => {}
            VAL_OBJECT => match host.retain_object_arg(target) {
                Ok(object) => inst.target = Some(object),
                Err(error) => {
                    return report_error(out_error, &format!("CSVParser: target: {error}"));
                }
            },
            tjs::VAL_NULL => {}
            _ => {
                return report_error(
                    out_error,
                    "CSVParser: the target argument cannot be converted to an object",
                );
            }
        }
    }
    if let Some(separator) = argv.get(1) {
        // Reference `separator = (tjs_int)*param[1]` (`csvParser.cpp:227`):
        // `AsInteger` parses strings, truncates reals and rejects
        // objects/octets (`core/tjs2/tjsVariant.h:862-879`).
        let code = match try_value_as_i64(separator) {
            Ok(value) => (value as u16) as u32,
            Err(error) => {
                return report_error(out_error, &format!("CSVParser: separator: {error}"));
            }
        };
        inst.separator = char::from_u32(code).unwrap_or(',');
    }
    if let Some(newline) = argv.get(2) {
        inst.newline = value_as_string(newline);
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Methods
// ---------------------------------------------------------------------------

/// `init(text)` — reset the parser over an in-memory string.
extern "C" fn csv_init(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(out_error, "CSVParser.init: compat context is not bound");
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(text) = argv.first() else {
        return report_error(out_error, "CSVParser.init requires 1 argument");
    };
    let text = match strict_string(text) {
        Ok(text) => text,
        Err(error) => return report_error(out_error, &format!("CSVParser.init: {error}")),
    };
    // SAFETY: `instance` is the live payload for the call.
    unsafe { &mut *(instance as *mut CsvParserInst) }.init(&text);
    set_void(out);
    0
}

/// `initStorage(filename, utf8?)` — reset the parser over a storage file.
extern "C" fn csv_init_storage(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    if resolve(engine).is_none() {
        return report_error(
            out_error,
            "CSVParser.initStorage: compat context is not bound",
        );
    }
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    let Some(filename) = argv.first() else {
        return report_error(out_error, "CSVParser.initStorage requires 1 argument");
    };
    let filename = match strict_string(filename) {
        Ok(filename) => filename,
        Err(error) => return report_error(out_error, &format!("CSVParser.initStorage: {error}")),
    };
    let utf8 = argv.get(1).is_some_and(|value| value_as_i64(value) != 0);
    let text = match read_storage_text(engine, &filename, utf8) {
        Ok(text) => text,
        Err(error) => {
            return report_error(out_error, &format!("CSVParser.initStorage: {error}"));
        }
    };
    // SAFETY: `instance` is the live payload for the call.
    unsafe { &mut *(instance as *mut CsvParserInst) }.init(&text);
    set_void(out);
    0
}

/// `getNextLine()` — the next row as a TJS Array, or `void` when exhausted.
extern "C" fn csv_get_next_line(
    engine: *mut c_void,
    instance: *mut c_void,
    _argc: c_int,
    _argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "CSVParser.getNextLine: compat context is not bound",
        );
    };
    let host = resolved.engine();
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &mut *(instance as *mut CsvParserInst) };
    match inst.next_line(host) {
        Ok(Some(fields)) => set_retained_out(out, fields),
        Ok(None) => set_void(out),
        Err(error) => {
            return report_error(out_error, &format!("CSVParser.getNextLine: {error}"));
        }
    }
    0
}

/// `parse([text])` — optionally re-init, then drive `target.doLine`.
extern "C" fn csv_parse(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(out_error, "CSVParser.parse: compat context is not bound");
    };
    let host = resolved.engine();
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &mut *(instance as *mut CsvParserInst) };
    if let Some(text) = argv.first() {
        let text = match strict_string(text) {
            Ok(text) => text,
            Err(error) => return report_error(out_error, &format!("CSVParser.parse: {error}")),
        };
        inst.init(&text);
    }
    if let Err(error) = inst.parse(host, objthis) {
        return report_error(out_error, &format!("CSVParser.parse: {error}"));
    }
    set_void(out);
    0
}

/// `parseStorage(filename, utf8?)` — optionally re-init from storage, then
/// drive `target.doLine`.
extern "C" fn csv_parse_storage(
    engine: *mut c_void,
    instance: *mut c_void,
    argc: c_int,
    argv: *const Value,
    out: *mut Value,
    out_error: *mut *mut c_char,
    objthis: *mut c_void,
) -> c_int {
    let Some(resolved) = resolve(engine) else {
        return report_error(
            out_error,
            "CSVParser.parseStorage: compat context is not bound",
        );
    };
    let host = resolved.engine();
    // SAFETY: the trampoline guarantees `argc` valid `Value` entries.
    let argv = unsafe { args(argv, argc) };
    // SAFETY: `instance` is the live payload for the call.
    let inst = unsafe { &mut *(instance as *mut CsvParserInst) };
    if let Some(filename) = argv.first() {
        let filename = match strict_string(filename) {
            Ok(filename) => filename,
            Err(error) => {
                return report_error(out_error, &format!("CSVParser.parseStorage: {error}"));
            }
        };
        let utf8 = argv.get(1).is_some_and(|value| value_as_i64(value) != 0);
        let text = match read_storage_text(engine, &filename, utf8) {
            Ok(text) => text,
            Err(error) => {
                return report_error(out_error, &format!("CSVParser.parseStorage: {error}"));
            }
        };
        inst.init(&text);
    }
    if let Err(error) = inst.parse(host, objthis) {
        return report_error(out_error, &format!("CSVParser.parseStorage: {error}"));
    }
    set_void(out);
    0
}

// ---------------------------------------------------------------------------
// Properties
// ---------------------------------------------------------------------------

/// `currentLineNumber` (getter only) — the number of the last line read.
extern "C" fn csv_current_line_number_get(
    _engine: *mut c_void,
    instance: *mut c_void,
    out: *mut Value,
    _out_error: *mut *mut c_char,
    _objthis: *mut c_void,
) -> c_int {
    // SAFETY: `instance` is the live payload the VM passed to the callback.
    let inst = unsafe { &*(instance as *const CsvParserInst) };
    set_int(out, inst.line_no);
    0
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Register the `CSVParser` native class on the VM.
pub fn register(ctx: &mut CompatCtx) -> Result<(), CompatError> {
    ctx.register_instance(&NativeInstanceBuilder {
        name: "CSVParser",
        create: csv_create,
        destroy: csv_destroy,
        invalidate: Some(csv_invalidate),
        methods: vec![
            NativeInstanceMethodDef {
                name: "CSVParser",
                f: csv_ctor,
            },
            NativeInstanceMethodDef {
                name: "init",
                f: csv_init,
            },
            NativeInstanceMethodDef {
                name: "initStorage",
                f: csv_init_storage,
            },
            NativeInstanceMethodDef {
                name: "getNextLine",
                f: csv_get_next_line,
            },
            NativeInstanceMethodDef {
                name: "parse",
                f: csv_parse,
            },
            NativeInstanceMethodDef {
                name: "parseStorage",
                f: csv_parse_storage,
            },
        ],
        properties: vec![NativeInstancePropertyDef {
            name: "currentLineNumber",
            get: Some(csv_current_line_number_get),
            set: None,
        }],
    })
}
