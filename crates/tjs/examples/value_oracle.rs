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
//! anything is printed, and a probe the engine rejects stops the recorder with
//! a message naming the probe.
//!
//! ```sh
//! cargo run -p tjs --example value_oracle > crates/tjs/tests/fixtures/value_layer.txt
//! ```

use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/value_oracle.rs`; regenerate with the command in the example's docs";

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
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, expression) in PROBES {
        let kind = answer(&engine, &format!("typeof ({expression})"), probe);
        let text = answer(&engine, &format!("\"\" + ({expression})"), probe);
        table.push_str(&format!("{probe}\t{kind}\t{}\n", escaped(&text)));
    }
    print!("{table}");
}

/// The text of one evaluation, which must be a text.
fn answer(engine: &Tjs2Engine, expression: &str, probe: &str) -> String {
    match engine.eval(expression, "value_oracle") {
        Ok(TjsValue::String(text)) => text,
        Ok(other) => fail(format_args!(
            "probe {probe}: the answer is not a text: {other:?}"
        )),
        Err(error) => fail(format_args!(
            "probe {probe}: the engine rejected it: {error}"
        )),
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
