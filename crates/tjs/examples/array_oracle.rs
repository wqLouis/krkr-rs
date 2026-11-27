//! Records what the engine answers for an array's members.
//!
//! A script reaches an array through its index space and through the members
//! its class publishes. This recorder builds one array with a fixed set of
//! writes, then evaluates a probe table against it in order; a probe may read,
//! write, delete, grow or sort, so the state each probe sees is the state the
//! probes before it left. Every probe goes through a function call, so a
//! constant folder cannot answer it, and one answer is recorded per probe: the
//! kind the language's `typeof` names and the value as text.
//!
//! A probe the language raises records the raise as `!raise`: the raise is the
//! rule's answer, not a gap in the record.
//!
//! The record is written to `tests/fixtures/array_members.txt` atomically: the
//! whole table is written beside the fixture and renamed over it, so a failed
//! run leaves the old record in place.
//!
//! ```sh
//! cargo run -p tjs --example array_oracle
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
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/array_oracle.rs`; regenerate with the command in the example's docs";

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The script that builds the array the probes read and the function every
/// probe is evaluated through.
///
/// The builder is a named function because a script cannot declare one inside
/// an anonymous body, and it is deleted after the call so the global object is
/// left as it was. The array starts with a hole at each end of a value: index
/// zero and index four hold values, and index one and index three are holes.
const SETUP: &str = r#"function arrayMembers() {
    var a = new Array();
    a[2] = "c";
    a[0] = 1;
    a[4] = 5;
    return a;
}
var a = arrayMembers();
delete this.arrayMembers;
var invalidated = new Array();
invalidate invalidated;
function arrayProbe(value) {
    return (typeof (value)) + ":" + ("" + value);
}
function splitCount(array, pattern, text, reserved, purge) {
    array.split(pattern, text, reserved, purge);
    return array.count;
}
function splitElement(array, pattern, text, reserved, purge, index) {
    array.split(pattern, text, reserved, purge);
    return array[index];
}
function splitJoin(array, pattern, text, reserved, purge, delimiter) {
    array.split(pattern, text, reserved, purge);
    return array.join(delimiter);
}
"#;

/// The probes: the id the check names the rule by, and the expression.
///
/// The probes run in this order and share the one array, so an expression that
/// writes is a change the later probes see.
const PROBES: &[(&str, &str)] = &[
    // the count and the two ways to read it
    ("length_initial", "a.length"),
    ("count_initial", "a.count"),
    // a hole, an element, and a position past the end
    ("hole", "a[1]"),
    ("hole_typeof", "typeof (a[1])"),
    ("element", "a[2]"),
    ("past_end", "a[9]"),
    ("past_end_typeof", "typeof (a[9])"),
    ("length_after_read", "a.length"),
    // a negative index counts from the end
    ("negative_read", "a[-1]"),
    ("negative_beyond", "a[-9]"),
    ("negative_beyond_typeof", "typeof (a[-9])"),
    // a write past the end grows the array with holes
    ("write_past", "(a[7] = 9)"),
    ("write_gap", "a[5]"),
    ("length_after_write", "a.length"),
    // the count's setter shrinks and grows
    ("shrink", "(a.length = 3)"),
    ("length_after_shrink", "a.length"),
    ("shrink_typeof", "typeof (a[3])"),
    ("grow", "(a.length = 5)"),
    ("grown_hole", "a[4]"),
    // a negative write and a negative write below the start
    ("write_negative", "(a[-1] = \"z\")"),
    ("write_negative_read", "a[4]"),
    ("write_negative_beyond", "(a[-9] = 1)"),
    // the operations the class publishes
    ("add", "a.add(\"a\")"),
    ("add_read", "a[5]"),
    ("insert_head", "a.insert(0, \"i\")"),
    ("insert_head_read", "a[0]"),
    ("insert_middle", "a.insert(2, \"m\")"),
    ("insert_middle_read", "a[2]"),
    ("insert_middle_shift", "a[3]"),
    ("insert_negative", "a.insert(-1, \"e\")"),
    ("insert_negative_read", "a[7]"),
    ("insert_too_negative", "a.insert(-99, 1)"),
    ("insert_past", "a.insert(99, 1)"),
    ("erase", "a.erase(0)"),
    ("erase_read", "a[0]"),
    ("erase_negative", "a.erase(-1)"),
    ("erase_past", "a.erase(99)"),
    ("erase_too_negative", "a.erase(-99)"),
    ("push", "a.push(7, 7, 8)"),
    ("find_first", "a.find(7)"),
    ("find_start", "a.find(7, 8)"),
    ("find_miss", "a.find(99)"),
    ("find_kind", "a.find(1.0)"),
    ("find_same", "a.find(1)"),
    ("find_negative_start", "a.find(8, -1)"),
    ("find_before", "a.find(7, -99)"),
    ("remove_first", "a.remove(7, false)"),
    ("remove_rest", "a.remove(7)"),
    ("remove_miss", "a.remove(99)"),
    ("join", "a.join(\"-\")"),
    ("join_purge", "a.join(\"-\", void, true)"),
    ("reverse", "a.reverse()"),
    ("reverse_read", "a[0]"),
    ("clear", "a.clear()"),
    ("clear_length", "a.length"),
    ("clear_read", "a[0]"),
    // the sort's six orders and the stable flag
    ("sort_push", "a.push(3, 1, 2)"),
    ("sort_default", "a.sort()"),
    ("sort_default_join", "a.join(\",\")"),
    ("sort_desc", "a.sort(\"-\")"),
    ("sort_desc_join", "a.join(\",\")"),
    ("sort_text_clear", "a.clear()"),
    ("sort_text_push", "a.push(\"10\", \"9\", \"1\")"),
    ("sort_numeric", "a.sort(\"0\")"),
    ("sort_numeric_join", "a.join(\",\")"),
    ("sort_numeric_desc", "a.sort(\"9\")"),
    ("sort_numeric_desc_join", "a.join(\",\")"),
    ("sort_string", "a.sort(\"a\")"),
    ("sort_string_join", "a.join(\",\")"),
    ("sort_string_desc", "a.sort(\"z\")"),
    ("sort_string_desc_join", "a.join(\",\")"),
    ("sort_unknown_letter", "a.sort(\"?\")"),
    ("sort_unknown_join", "a.join(\",\")"),
    ("sort_stable", "a.sort(\"+\", true)"),
    ("sort_stable_join", "a.join(\",\")"),
    // the two ends of the sequence
    ("queue_clear", "a.clear()"),
    ("queue_push", "a.push(1, 2, 3)"),
    ("pop", "a.pop()"),
    ("shift", "a.shift()"),
    ("unshift", "a.unshift(0)"),
    ("queue_join", "a.join(\",\")"),
    ("queue_clear2", "a.clear()"),
    ("pop_empty", "a.pop()"),
    ("shift_empty", "a.shift()"),
    // delete answers a boolean, and an index delete shifts
    ("delete_push", "a.push(1, 2, 3)"),
    ("delete_index", "(delete a[0])"),
    ("delete_read", "a[0]"),
    ("delete_past", "(delete a[9])"),
    ("delete_negative", "(delete a[-1])"),
    ("delete_negative_beyond", "(delete a[-9])"),
    ("delete_string_index", "(delete a[\"0\"])"),
    // named members are the object's, not the index space's
    ("named_write", "(a.note = \"n\")"),
    ("named_read", "a.note"),
    ("named_absent_typeof", "typeof (a.nope)"),
    ("named_absent_read", "a.nope"),
    ("named_delete", "(delete a.note)"),
    ("named_deleted_typeof", "typeof (a.note)"),
    ("named_delete_again", "(delete a.note)"),
    ("named_void_write", "(a.blank = void)"),
    ("named_void_read", "a.blank"),
    ("named_length", "a.length"),
    // a name names an index by the class's own rule
    ("padded_write", "(a[\" 2 \"] = \"p\")"),
    ("padded_read", "a[\" 2 \"]"),
    ("signed_read", "a[\"+2\"]"),
    ("dotted_read", "a[\"2.9\"]"),
    ("named_string_typeof", "typeof (a[\"2x\"])"),
    ("named_string_read", "a[\"2x\"]"),
    ("minus_zero", "a[\"-0\"]"),
    ("whitespace_index", "a[\"\\t1\\n\"]"),
    ("signed_space_read", "a[\"- 1\"]"),
    // assign copies an array's elements and leaves the named members
    ("assign", "a.assign([4, 5])"),
    ("assign_length", "a.length"),
    ("assign_first", "a[0]"),
    ("assign_second", "a[1]"),
    ("named_survives_assign", "typeof (a.blank)"),
    // the class a script sees
    ("instanceof_array", "a instanceof Array"),
    ("instanceof_object", "a instanceof Object"),
    ("instanceof_dictionary", "a instanceof Dictionary"),
    // an array answers the class names it carries, and no other name
    ("instanceof_array_name", "a instanceof \"Array\""),
    ("instanceof_object_name", "a instanceof \"Object\""),
    ("instanceof_dictionary_name", "a instanceof \"Dictionary\""),
    ("instanceof_string_name", "a instanceof \"String\""),
    ("instanceof_number_name", "a instanceof \"Number\""),
    ("instanceof_octet_name", "a instanceof \"Octet\""),
    // split, a literal and a construction all carry the class name
    (
        "instanceof_split_name",
        "\"a,b\".split(\",\") instanceof \"Array\"",
    ),
    ("instanceof_literal_name", "[] instanceof \"Array\""),
    ("instanceof_new_name", "(new Array()) instanceof \"Array\""),
    // an invalidated array refuses the class question
    ("instanceof_invalidated", "invalidated instanceof \"Array\""),
    // split uses every character of the pattern as a boundary, and the purge
    // flag drops the empty fields a boundary leaves
    (
        "split_text_count",
        "splitCount(a, \",\", \"a,b,c\", void, void)",
    ),
    (
        "split_text_first",
        "splitElement(a, \",\", \"a,b,c\", void, void, 0)",
    ),
    (
        "split_text_second",
        "splitElement(a, \",\", \"a,b,c\", void, void, 1)",
    ),
    (
        "split_text_third",
        "splitElement(a, \",\", \"a,b,c\", void, void, 2)",
    ),
    (
        "split_text_join",
        "splitJoin(a, \",\", \"a,b,c\", void, void, \"|\")",
    ),
    (
        "split_multi_count",
        "splitCount(a, \",;\", \"a,b;c\", void, void)",
    ),
    (
        "split_multi_join",
        "splitJoin(a, \",;\", \"a,b;c\", void, void, \"|\")",
    ),
    (
        "split_empty_pattern_count",
        "splitCount(a, \"\", \"abc\", void, void)",
    ),
    (
        "split_empty_pattern_first",
        "splitElement(a, \"\", \"abc\", void, void, 0)",
    ),
    (
        "split_missing_count",
        "splitCount(a, \",\", \"abc\", void, void)",
    ),
    (
        "split_missing_first",
        "splitElement(a, \",\", \"abc\", void, void, 0)",
    ),
    (
        "split_leading_count",
        "splitCount(a, \",\", \",a\", void, void)",
    ),
    (
        "split_leading_first",
        "splitElement(a, \",\", \",a\", void, void, 0)",
    ),
    (
        "split_leading_purge_count",
        "splitCount(a, \",\", \",a\", void, true)",
    ),
    (
        "split_trailing_count",
        "splitCount(a, \",\", \"a,\", void, void)",
    ),
    (
        "split_trailing_last",
        "splitElement(a, \",\", \"a,\", void, void, 1)",
    ),
    (
        "split_trailing_purge_count",
        "splitCount(a, \",\", \"a,\", void, true)",
    ),
    (
        "split_consecutive_count",
        "splitCount(a, \",\", \"a,,b\", void, void)",
    ),
    (
        "split_consecutive_second",
        "splitElement(a, \",\", \"a,,b\", void, void, 1)",
    ),
    (
        "split_consecutive_purge_count",
        "splitCount(a, \",\", \"a,,b\", void, true)",
    ),
    (
        "split_consecutive_purge_join",
        "splitJoin(a, \",\", \"a,,b\", void, true, \"|\")",
    ),
    (
        "split_empty_text_count",
        "splitCount(a, \",\", \"\", void, void)",
    ),
    (
        "split_empty_text_first",
        "splitElement(a, \",\", \"\", void, void, 0)",
    ),
    (
        "split_empty_text_purge_count",
        "splitCount(a, \",\", \"\", void, true)",
    ),
    (
        "split_purge_zero_count",
        "splitCount(a, \",\", \"a,,b\", void, 0)",
    ),
    (
        "split_purge_one_count",
        "splitCount(a, \",\", \"a,,b\", void, 1)",
    ),
    (
        "split_purge_two_count",
        "splitCount(a, \",\", \"a,,b\", void, 2)",
    ),
    (
        "split_purge_text_count",
        "splitCount(a, \",\", \"a,,b\", void, \"abc\")",
    ),
    (
        "split_reserved_ignored_count",
        "splitCount(a, \",\", \"a,b\", true, false)",
    ),
    (
        "split_number_pattern_count",
        "splitCount(a, 1, \"a1b\", void, void)",
    ),
    (
        "split_void_pattern_count",
        "splitCount(a, void, \"abc\", void, void)",
    ),
    (
        "split_void_text_count",
        "splitCount(a, \",\", void, void, void)",
    ),
    (
        "split_void_text_first",
        "splitElement(a, \",\", void, void, void, 0)",
    ),
    ("split_one_param_refused", "a.split(\",\")"),
    (
        "split_octet_pattern_refused",
        "splitCount(a, [1].pack(\"C1\"), \"a,b\", void, void)",
    ),
    ("split_octet_pattern_cleared", "a.count"),
    (
        "split_octet_text_refused",
        "splitCount(a, \",\", [1].pack(\"C1\"), void, void)",
    ),
    ("split_octet_text_cleared", "a.count"),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "array_members") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, expression) in PROBES {
        let call = format!("arrayProbe({expression})");
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/array_members.txt")
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
    match engine.eval(call, "array_members") {
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
    eprintln!("array_oracle: {message}");
    std::process::exit(1)
}
