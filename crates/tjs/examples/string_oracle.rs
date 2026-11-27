//! Records what the engine answers for a text's and a byte string's members.
//!
//! A text and a byte string answer a member call from their own surface, and the
//! engine registers no class object for either. This recorder probes that
//! surface: the class the script cannot see, the member reads that are not
//! method values, one call per rule with the arguments the rule turns on, and
//! the byte string's one member. Every call goes through a helper function, so a
//! constant folder cannot answer it, and one answer is recorded per probe: the
//! kind the language's `typeof` names and the value as text.
//!
//! A probe the language raises records the raise as `!raise`: the raise is the
//! rule's answer, not a gap in the record.
//!
//! The record is written to `tests/fixtures/string_members.txt` atomically: the
//! whole table is written beside the fixture and renamed over it, so a failed
//! run leaves the old record in place.
//!
//! ```sh
//! cargo run -p tjs --example string_oracle
//! ```

use std::ffi::CStr;
use std::fs;
use std::path::PathBuf;
use std::ptr;

use tjs::vm::{
    TJS2_SCRIPT_OK, tjs2_script_compile, tjs2_script_error_message, tjs2_script_free,
    tjs2_script_status,
};
use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/string_oracle.rs`; regenerate with the command in the example's docs";

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The script that gives the probes their helpers and their one byte string.
///
/// The helpers are named functions because a script cannot declare one inside an
/// anonymous body. `stringProbe` answers the kind and the text in one call, so a
/// probe is evaluated once; the byte-string helpers read the array an unpack
/// answers, because a byte string itself has no text form.
const SETUP: &str = r#"function stringProbe(value) {
    return (typeof (value)) + ":" + ("" + value);
}
function memberType(value, name) {
    return typeof (value[name]);
}
function octetUnpackCount(bytes, format) {
    return bytes.unpack(format).count;
}
function octetUnpackElement(bytes, format, index) {
    return bytes.unpack(format)[index];
}
function octetUnpackType(bytes, format, index) {
    return typeof (bytes.unpack(format)[index]);
}
var octet3 = [1, 2, 3].pack("c3");
"#;

/// The probes: the id the check names the rule by, and the expression.
const PROBES: &[(&str, &str)] = &[
    // the class a script cannot see
    ("surface_class_typeof", "typeof String"),
    // a member read is not a method value; the two properties are
    ("property_length", r#""abc".length"#),
    ("property_index", r#""abc"[1]"#),
    ("property_method_read", r#""abc".charAt"#),
    ("property_method_typeof", r#"memberType("abc", "charAt")"#),
    ("property_octet_unpack_read", "octet3.unpack"),
    (
        "property_octet_unpack_typeof",
        r#"memberType(octet3, "unpack")"#,
    ),
    // charAt
    ("char_at", r#""abc".charAt(1)"#),
    ("char_at_end", r#""abc".charAt(3)"#),
    ("char_at_negative", r#""abc".charAt(-1)"#),
    ("char_at_blank", r#""".charAt(0)"#),
    ("char_at_text_index", r#""abc".charAt("1")"#),
    ("char_at_missing", r#""abc".charAt()"#),
    ("char_at_extra", r#""abc".charAt(0, 1)"#),
    // indexOf
    ("index_of", r#""abcabc".indexOf("b")"#),
    ("index_of_start", r#""abcabc".indexOf("b", 2)"#),
    ("index_of_miss", r#""abc".indexOf("z")"#),
    ("index_of_empty_needle", r#""abc".indexOf("")"#),
    ("index_of_empty_target", r#""".indexOf("a")"#),
    ("index_of_number_needle", r#""a1b".indexOf(1)"#),
    ("index_of_start_past", r#""abc".indexOf("a", 5)"#),
    ("index_of_missing", r#""abc".indexOf()"#),
    ("index_of_three", r#""abc".indexOf("a", 0, 0)"#),
    // toUpperCase and toLowerCase
    ("upper", r#""aBc1".toUpperCase()"#),
    ("upper_count", r#""a".toUpperCase(1)"#),
    ("lower", r#""AbC1".toLowerCase()"#),
    ("lower_count", r#""a".toLowerCase(1)"#),
    // substring and substr
    ("substring_start", r#""abcdef".substring(2)"#),
    ("substring_count", r#""abcdef".substring(2, 3)"#),
    ("substring_past", r#""abcdef".substring(2, 99)"#),
    ("substring_negative_count", r#""abcdef".substring(2, -1)"#),
    ("substring_at_end", r#""abcdef".substring(6)"#),
    ("substring_negative_start", r#""abcdef".substring(-1)"#),
    ("substr_count", r#""abcdef".substr(1, 2)"#),
    ("substring_missing", r#""abcdef".substring()"#),
    // sprintf
    ("sprintf_text", r#""%d and %s".sprintf(5, "x")"#),
    ("sprintf_width", r#""[%5d]".sprintf(42)"#),
    ("sprintf_zero_width", r#""[%05d]".sprintf(-42)"#),
    ("sprintf_left", r#""[%-5d]".sprintf(42)"#),
    ("sprintf_plus", r#""%+d".sprintf(42)"#),
    ("sprintf_hex", r#""%x %X".sprintf(255, 255)"#),
    ("sprintf_alternate", r#""%#x %#o".sprintf(255, 8)"#),
    ("sprintf_unsigned", r#""%u".sprintf(-1)"#),
    ("sprintf_real", r#""%.2f".sprintf(3.14159)"#),
    ("sprintf_real_width", r#""[%8.3f]".sprintf(1.5)"#),
    ("sprintf_exponent", r#""%.3e".sprintf(12345.678)"#),
    ("sprintf_general", r#""%g".sprintf(0.0001)"#),
    ("sprintf_percent", r#""%d%%".sprintf(50)"#),
    ("sprintf_character", r#""%c".sprintf(65)"#),
    ("sprintf_text_precision", r#""%.2s".sprintf("abcdef")"#),
    ("sprintf_unknown_kind", r#""%q".sprintf()"#),
    ("sprintf_truncated", r#""%".sprintf()"#),
    ("sprintf_missing", r#""%d".sprintf()"#),
    ("sprintf_extra", r#""%d".sprintf(1, 2)"#),
    ("sprintf_real_as_integer", r#""%d".sprintf(1.9)"#),
    // replace
    ("replace_text_pattern", r#""abc".replace("b", "x")"#),
    ("replace_missing", r#""abc".replace("b")"#),
    ("replace_regex", r#""a1b".replace(/1/, "X")"#),
    // escape
    ("escape", r#""a\tb".escape()"#),
    // split
    ("split_count", r#""a,b,c".split(",").count"#),
    ("split_first", r#""a,b,c".split(",")[0]"#),
    (
        "split_purge_count",
        r#""a,,b".split(",", void, true).count"#,
    ),
    ("split_missing", r#""a,b".split()"#),
    // trim, reverse and repeat
    ("trim", r#"" \t ab \n".trim()"#),
    ("trim_blank", r#""   ".trim()"#),
    ("trim_count", r#"" a ".trim(1)"#),
    ("reverse", r#""abc".reverse()"#),
    ("reverse_count", r#""a".reverse(1)"#),
    ("repeat", r#""ab".repeat(3)"#),
    ("repeat_zero", r#""ab".repeat(0)"#),
    ("repeat_negative", r#""ab".repeat(-1)"#),
    ("repeat_missing", r#""ab".repeat()"#),
    // the byte string's one member
    ("unpack_count", r#"octetUnpackCount(octet3, "c3")"#),
    ("unpack_first", r#"octetUnpackElement(octet3, "c3", 0)"#),
    ("unpack_third", r#"octetUnpackElement(octet3, "c3", 2)"#),
    ("unpack_type", r#"octetUnpackType(octet3, "c3", 0)"#),
    ("unpack_ascii", r#"octetUnpackElement(octet3, "a3", 0)"#),
    ("unpack_extra_argument", r#"octet3.unpack("c3", 9).count"#),
    ("unpack_missing", "octet3.unpack()"),
    ("unpack_number", "octet3.unpack(5)"),
    ("unpack_unknown_format", r#"octet3.unpack("q")"#),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "string_members") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, expression) in PROBES {
        let call = format!("stringProbe({expression})");
        let answer = answer(&engine, &call, probe);
        let (kind, text) = match answer {
            Answer::Answered(kind, text) => (kind, escaped(&text)),
            Answer::Raised => (String::from(RAISED), String::new()),
        };
        table.push_str(&format!("{probe}\t{kind}\t{text}\n"));
    }
    let path = fixture_path();
    let temporary = path.with_extension("txt.tmp");
    if let Err(error) = fs::write(&temporary, table) {
        fail(format_args!(
            "the temporary record {} is not writable: {error}",
            temporary.display()
        ));
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        fail(format_args!(
            "the record could not be renamed to {}: {error}",
            path.display()
        ));
    }
    println!("wrote {}", path.display());
}

/// The fixture the record is written to and the check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/string_members.txt")
}

/// What one probe answered.
enum Answer {
    /// The kind the answer's `typeof` reports, and its text.
    Answered(String, String),
    /// The probe raised.
    Raised,
}

/// Evaluates one probe, which must compile.
///
/// The call is compiled first, so a probe the front end refuses is told apart
/// from a raise: it never reaches a rule, and the recorder stops rather than
/// record it as the rule's answer.
fn answer(engine: &Tjs2Engine, call: &str, probe: &str) -> Answer {
    if let Err(message) = compiles(call) {
        fail(format_args!(
            "probe {probe}: the expression does not compile (!compile): {message}"
        ));
    }
    match engine.eval(call, "string_members") {
        Ok(TjsValue::String(text)) => match text.split_once(':') {
            Some((kind, value)) => Answer::Answered(kind.to_owned(), value.to_owned()),
            None => fail(format_args!(
                "probe {probe}: the answer has no kind and text: {text:?}"
            )),
        },
        Ok(other) => fail(format_args!(
            "probe {probe}: the answer is not a text: {other:?}"
        )),
        Err(_raised) => Answer::Raised,
    }
}

/// Whether the front end compiles the call, and why it does not.
fn compiles(expression: &str) -> Result<(), String> {
    let units: Vec<u16> = expression.encode_utf16().collect();
    let length = u32::try_from(units.len()).expect("a probe expression fits a 32-bit length");
    // SAFETY: the units are readable for `length` code units, the null values
    // pointer means "no conditional-compilation values", and the handle is
    // freed before this returns.
    unsafe {
        let script = tjs2_script_compile(units.as_ptr(), length, 1, 1, ptr::null());
        if script.is_null() {
            return Err(String::from("the front end answered no handle"));
        }
        let status = tjs2_script_status(script);
        if status == TJS2_SCRIPT_OK {
            tjs2_script_free(script);
            return Ok(());
        }
        let message = tjs2_script_error_message(script);
        let message = if message.is_null() {
            String::from("the front end gave no message")
        } else {
            // SAFETY: the handle owns a NUL-terminated message, live until the
            // handle is freed below.
            CStr::from_ptr(message).to_string_lossy().into_owned()
        };
        tjs2_script_free(script);
        Err(message)
    }
}

/// The text escaped so that one probe stays on one line.
///
/// A tab, a newline and a backslash are written as their two-character
/// escapes, and a code unit below `0x20` is written as `\xNN`.
fn escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\t' => escaped.push_str("\\t"),
            '\n' => escaped.push_str("\\n"),
            '\\' => escaped.push_str("\\\\"),
            character if character < ' ' => {
                escaped.push_str(&format!("\\x{:02x}", u32::from(character)));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

/// Reports the failure and exits without writing a record.
fn fail(message: std::fmt::Arguments<'_>) -> ! {
    eprintln!("string_oracle: {message}");
    std::process::exit(1)
}
