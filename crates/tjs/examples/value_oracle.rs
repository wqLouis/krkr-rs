//! Records the engine's answers for the value layer's probe table.
//!
//! Each probe is a script expression whose answer the value layer
//! (`tjs::vm::runtime::{number, string}`) must reproduce. The recorder prints
//! the fixture's header and then one line per probe: the probe's name, the kind
//! `typeof` reports for the answer, and the answer as text. The check in
//! `tests/value_layer.rs` compares the value layer with that record, without
//! the engine and without a game.
//!
//! The record is written whole or not at all: every probe is evaluated before
//! the file is touched, a probe the engine rejects stops the recorder with a
//! message naming the probe, and the record is written beside the fixture and
//! renamed over it, so a failed recording leaves the old record in place.
//!
//! A probe whose answer has no text records the raise the language makes
//! instead — see [`RAISED`]: a raise at run time is the rule's answer, not a
//! gap. A probe the front end refuses to *compile* is another thing
//! altogether (`!compile`): it never reaches a rule, so it is not evidence,
//! and the recorder stops and writes nothing rather than record it.
//!
//! ```sh
//! cargo run -p tjs --example value_oracle
//! ```
//!
//! The record goes to the committed fixture; an argument names another path.

use std::ffi::CStr;
use std::path::{Path, PathBuf};
use std::ptr;

use tjs::vm::{
    TJS2_SCRIPT_OK, tjs2_script_compile, tjs2_script_error_message, tjs2_script_free,
    tjs2_script_status,
};
use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/value_oracle.rs`; regenerate with the command in the example's docs";

/// The text a probe records when the language raises instead of answering a
/// text.
///
/// The raise is the rule's answer — an octet has no text, and the coercion
/// says so by raising — so the record pins the raise itself. The message is
/// not recorded: the message table words it, not the value.
const RAISED: &str = "!raise";

/// The probe table: the name the check uses, and the script expression.
const PROBES: &[(&str, &str)] = &[
    // the real-to-text rule
    ("real_zero", "0.0"),
    ("real_one_and_half", "1.5"),
    ("real_third", "1/3"),
    ("real_huge", "1e20"),
    ("real_tiny", "1e-20"),
    ("real_negative_zero", "-0.0"),
    ("real_not_a_number", "0.0/0.0"),
    ("real_infinite", "1e400"),
    ("real_negative_infinite", "-1e400"),
    // the integer-to-text rule
    ("integer_zero", "0"),
    ("integer_negative_one", "-1"),
    ("integer_int32_max", "2147483647"),
    ("integer_above_int32", "2147483648"),
    ("integer_above_uint32", "4294967296"),
    // reading a number out of text, forced by `* 1`
    ("text_integer", r#""12" * 1"#),
    ("text_spaced", r#"" 12 " * 1"#),
    ("text_hexadecimal", r#""0x1f" * 1"#),
    ("text_binary", r#""0b101" * 1"#),
    ("text_exponent", r#""1e3" * 1"#),
    ("text_exponent_without_digits", r#""1.5e" * 1"#),
    ("text_exponent_sign_without_digits", r#""12e-" * 1"#),
    ("text_leading_point", r#"".5" * 1"#),
    ("text_trailing_point", r#""12." * 1"#),
    ("text_octal_prefix", r#""08" * 1"#),
    ("text_word", r#""abc" * 1"#),
    ("text_empty", r#""" * 1"#),
    ("text_above_integer_range", r#""9223372036854775808" * 1"#),
    ("text_lowest_integer", r#""-9223372036854775808" * 1"#),
    ("text_infinite", r#""1e400" * 1"#),
    // the kind the conversion produces
    ("kind_of_integer_text", r#"typeof ("12" * 1)"#),
    ("kind_of_real_text", r#"typeof ("12.5" * 1)"#),
    ("kind_of_word", r#"typeof ("abc" * 1)"#),
    // the escape rule
    ("escape", r#""\t\n\\\"\x01'".escape()"#),
    // the length rule
    ("length_ascii", r#""abc".length"#),
    ("length_surrogate_pair", r#""😀".length"#),
    ("length_top_unit", r#""\xffff".length"#),
    // the comparison rule
    ("compare_lower_against_upper", r#""a" < "B""#),
    ("compare_case", r#""a" == "A""#),
    ("compare_prefix", r#""abc" < "abd""#),
    // the variant's arithmetic
    //
    // One operand of each of these is read at run time, from a dictionary's
    // absent member: the generator folds an operation whose operands are both
    // constants, and a folded operation never reaches the rule the probe is
    // named for. The read answers the empty variant, so the operation it feeds
    // is the same one the probe names, with the same operands.
    ("sum_integers", r#"((new Dictionary())["x"] + 1) + 2"#),
    ("divide_integers", r#"((new Dictionary())["x"] + 7) / 2"#),
    ("integer_divide", r#"((new Dictionary())["x"] + 7) \ 2"#),
    ("remainder", r#"((new Dictionary())["x"] + 7) % 3"#),
    ("negative_remainder", r#"((new Dictionary())["x"] - 7) % 3"#),
    ("multiply_texts", r#"((new Dictionary())["x"] + "5") * "2""#),
    (
        "subtract_from_text",
        r#"((new Dictionary())["x"] + "5") - 2"#,
    ),
    (
        "add_text_and_number",
        r#"((new Dictionary())["x"] + "a") + 1"#,
    ),
    (
        "add_number_and_text",
        r#"((new Dictionary())["x"] + 1) + "a""#,
    ),
    ("add_void_and_number", r#"((new Dictionary())["x"] + 1)"#),
    ("add_number_and_void", r#"(1 + (new Dictionary())["x"])"#),
    (
        "add_two_voids",
        r#"((new Dictionary())["x"] + (new Dictionary())["y"])"#,
    ),
    // the variant's bitwise family
    ("bit_and", r#"((new Dictionary())["x"] + 6) & 3"#),
    ("bit_or", r#"((new Dictionary())["x"] + 6) | 3"#),
    ("bit_xor", r#"((new Dictionary())["x"] + 6) ^ 3"#),
    ("shift_left", r#"((new Dictionary())["x"] + 1) << 4"#),
    ("shift_right", r#"((new Dictionary())["x"] - 8) >> 1"#),
    ("bit_not", r#"~((new Dictionary())["x"] + 5)"#),
    ("negate_integer", r#"-((new Dictionary())["x"] + 5)"#),
    // the variant's comparisons
    (
        "order_text_and_number",
        r#"((new Dictionary())["x"] + "10") < 9"#,
    ),
    (
        "equal_integer_and_real",
        r#"((new Dictionary())["x"] + 1) == 1.0"#,
    ),
    (
        "equal_number_and_text",
        r#"((new Dictionary())["x"] + 1) == "1""#,
    ),
    (
        "unequal_number_and_text",
        r#"((new Dictionary())["x"] + 1) == "2""#,
    ),
    (
        "equal_void_and_empty_text",
        "((new Dictionary())[\"x\"]) == \"\"",
    ),
    (
        "equal_null_and_void",
        r#"null == ((new Dictionary())["x"])"#,
    ),
    // the kind the typeof operator reports
    ("kind_of_integer", "typeof (1)"),
    ("kind_of_real", "typeof (1.5)"),
    ("kind_of_text", r#"typeof ("a")"#),
    ("kind_of_octet", "typeof (<%01%>)"),
];

/// Probes whose answer's text is not the value's own coercion: the name, the
/// value's expression, and the expression the answer's text is read from.
///
/// `typeof` is asked of the value's expression here too, so a probe of this
/// table reads exactly like one of the table above; only the text differs.
/// Two values are here: an object's own text is the two addresses it carries,
/// which a record cannot pin, and an octet has no text at all — the language
/// raises where the coercion would be asked for one.
const TEXT_PROBES: &[(&str, &str, &str)] = &[("kind_of_null", "null", "typeof (null)")];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, expression) in PROBES {
        let kind_expression = format!("typeof ({expression})");
        let text_expression = format!("\"\" + ({expression})");
        let kind = column(&engine, &kind_expression, probe, "kind");
        let text = column(&engine, &text_expression, probe, "text");
        table.push_str(&format!("{probe}\t{kind}\t{text}\n"));
    }
    for &(probe, expression, text_expression) in TEXT_PROBES {
        let kind_expression = format!("typeof ({expression})");
        let kind = column(&engine, &kind_expression, probe, "kind");
        let text = column(&engine, text_expression, probe, "text");
        table.push_str(&format!("{probe}\t{kind}\t{text}\n"));
    }
    record(&fixture_path(), &table, PROBES.len() + TEXT_PROBES.len());
}

/// The fixture's path: the first argument, or the committed fixture.
fn fixture_path() -> PathBuf {
    match std::env::args_os().nth(1) {
        Some(path) => PathBuf::from(path),
        None => PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join("value_layer.txt"),
    }
}

/// Writes the record where the check reads it, all at once.
///
/// The record is written to a file beside the fixture and renamed over it, so
/// a recording that fails cannot leave an empty or a partial record where a
/// good one was.
fn record(path: &Path, table: &str, probes: usize) {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    if let Err(error) = std::fs::write(&temporary, table) {
        fail(format_args!(
            "the record could not be written to {}: {error}",
            temporary.display()
        ));
    }
    if let Err(error) = std::fs::rename(&temporary, path) {
        fail(format_args!(
            "the record could not be moved to {}: {error}",
            path.display()
        ));
    }
    eprintln!(
        "value_oracle: recorded {probes} probes in {}",
        path.display()
    );
}

/// One column of one probe: the answer's text, or the raise it makes.
///
/// The expression is compiled first, so a refused compile is told apart from a
/// raise: the engine reports both the same way, but they are not the same
/// thing. A refused compile stops the recorder — a probe that never reaches a
/// rule is not evidence — while a raise at run time is the rule's answer.
fn column(engine: &Tjs2Engine, expression: &str, probe: &str, name: &str) -> String {
    if let Err(message) = compiles(expression) {
        fail(format_args!(
            "probe {probe}: the {name} expression does not compile (!compile): {message}"
        ));
    }
    match engine.eval(expression, "value_oracle") {
        Ok(TjsValue::String(text)) => escaped(&text),
        Ok(other) => fail(format_args!(
            "probe {probe}: the answer is not a text: {other:?}"
        )),
        Err(_raised) => String::from(RAISED),
    }
}

/// Whether the front end compiles the expression, and why it does not.
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

/// Reports the failure and exits without printing the rest of the table.
fn fail(message: std::fmt::Arguments<'_>) -> ! {
    eprintln!("value_oracle: {message}");
    std::process::exit(1)
}
