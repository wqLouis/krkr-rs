//! The value layer against the engine's recorded answers.
//!
//! `tests/fixtures/value_layer.txt` holds what the engine answered for a fixed
//! probe table; `crates/tjs/examples/value_oracle.rs` records it. The check
//! below reads the record and reproduces every answer with the Rust value layer
//! (`tjs::vm::runtime::{number, string}`), so the layer is proven against the
//! engine and not only against its own tests.
//!
//! A record that holds only the header is a failure, not a pass: the table must
//! be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;

use tjs::vm::runtime::number::{Number, NumberError};
use tjs::vm::runtime::string::TjsString;

/// The probes the record must hold, in the recorder's order.
const PROBES: &[&str] = &[
    "real_zero",
    "real_one_and_half",
    "real_third",
    "real_huge",
    "real_tiny",
    "real_negative_zero",
    "real_not_a_number",
    "real_infinite",
    "real_negative_infinite",
    "integer_zero",
    "integer_negative_one",
    "integer_int32_max",
    "integer_above_int32",
    "integer_above_uint32",
    "text_integer",
    "text_spaced",
    "text_hexadecimal",
    "text_binary",
    "text_exponent",
    "text_exponent_without_digits",
    "text_exponent_sign_without_digits",
    "text_leading_point",
    "text_trailing_point",
    "text_octal_prefix",
    "text_word",
    "text_empty",
    "text_above_integer_range",
    "text_lowest_integer",
    "text_infinite",
    "kind_of_integer_text",
    "kind_of_real_text",
    "kind_of_word",
    "escape",
    "length_ascii",
    "length_surrogate_pair",
    "length_top_unit",
    "compare_lower_against_upper",
    "compare_case",
    "compare_prefix",
];

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind `typeof` reports for the answer.
    kind: String,
    /// The answer as text, with the record's escaping decoded.
    text: String,
}

#[test]
fn the_value_layer_reproduces_the_recorded_answers() {
    let records = recorded_answers();
    let mut seen = Vec::with_capacity(records.len());
    for record in &records {
        let (kind, text) = answer(&record.probe);
        assert_eq!(record.kind, kind, "probe {}: the kind", record.probe);
        assert_eq!(record.text, text, "probe {}: the answer", record.probe);
        seen.push(record.probe.as_str());
    }
    assert_eq!(
        seen.as_slice(),
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example value_oracle > crates/tjs/tests/fixtures/value_layer.txt`"
    );
}

/// The answers of the value layer, probe by probe.
///
/// Each arm names the rule the probe pins, cited above it from the same place
/// the value layer's own tests cite.
fn answer(probe: &str) -> (&'static str, String) {
    match probe {
        // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:241-249
        "real_zero" => number(Number::Real(0.0)),
        "real_one_and_half" => number(Number::Real(1.5)),
        "real_third" => number(Number::Real(1.0 / 3.0)),
        "real_huge" => number(Number::Real(1e20)),
        "real_tiny" => number(Number::Real(1e-20)),
        // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:219-240
        "real_negative_zero" => number(Number::Real(-0.0)),
        "real_not_a_number" => number(Number::Real(f64::NAN)),
        "real_infinite" => number(Number::Real(f64::INFINITY)),
        "real_negative_infinite" => number(Number::Real(f64::NEG_INFINITY)),
        // reference/krkr2/cpp/core/tjs2/tjsConfig.cpp:177-201
        "integer_zero" => number(Number::Integer(0)),
        "integer_negative_one" => number(Number::Integer(-1)),
        "integer_int32_max" => number(Number::Integer(2_147_483_647)),
        "integer_above_int32" => number(Number::Integer(2_147_483_648)),
        "integer_above_uint32" => number(Number::Integer(4_294_967_296)),
        // reference/krkr2/cpp/core/tjs2/tjsLex.cpp:611-697
        // reference/krkr2/cpp/core/tjs2/tjsVariantString.cpp:481-493
        "text_integer" => multiplied("12"),
        // reference/krkr2/cpp/core/tjs2/tjsLex.cpp:700-727
        "text_spaced" => multiplied(" 12 "),
        "text_hexadecimal" => multiplied("0x1f"),
        "text_binary" => multiplied("0b101"),
        // reference/krkr2/cpp/core/tjs2/tjsLex.cpp:593-597
        // reference/krkr2/cpp/core/tjs2/tjsConfig.cpp:672
        "text_exponent" => multiplied("1e3"),
        "text_exponent_without_digits" => multiplied("1.5e"),
        "text_exponent_sign_without_digits" => multiplied("12e-"),
        "text_leading_point" => multiplied(".5"),
        "text_trailing_point" => multiplied("12."),
        // reference/krkr2/cpp/core/tjs2/tjsLex.cpp:611-697
        "text_octal_prefix" => multiplied("08"),
        "text_word" => multiplied("abc"),
        "text_empty" => multiplied(""),
        "text_above_integer_range" => multiplied("9223372036854775808"),
        "text_lowest_integer" => multiplied("-9223372036854775808"),
        // reference/krkr2/cpp/core/tjs2/tjsConfig.cpp:789-814
        "text_infinite" => multiplied("1e400"),
        // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:1040-1054
        "kind_of_integer_text" => text(TjsString::from(multiplied("12").0)),
        "kind_of_real_text" => text(TjsString::from(multiplied("12.5").0)),
        "kind_of_word" => text(TjsString::from(multiplied("abc").0)),
        // reference/krkr2/cpp/core/tjs2/tjsString.cpp:160-230
        // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2470-2474
        "escape" => text(TjsString::from("\t\n\\\"\x01'").escape_c()),
        // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:47-62
        "length_ascii" => length("abc"),
        "length_surrogate_pair" => length("😀"),
        "length_top_unit" => length("\u{ffff}"),
        // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:856-883
        "compare_lower_against_upper" => truth(TjsString::from("a") < TjsString::from("B")),
        // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:665-695
        "compare_case" => truth(TjsString::from("a") == TjsString::from("A")),
        // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:856-883
        "compare_prefix" => truth(TjsString::from("abc") < TjsString::from("abd")),
        other => panic!("the check has no answer for the recorded probe: {other}"),
    }
}

/// A number's answer: the kind `typeof` reports and the number as text.
fn number(value: Number) -> (&'static str, String) {
    match value {
        Number::Integer(_) => ("Integer", value.to_string()),
        Number::Real(_) => ("Real", value.to_string()),
    }
}

/// A text's answer: the kind `typeof` reports and the text itself.
fn text(value: TjsString) -> (&'static str, String) {
    ("String", value.to_string())
}

/// A truth value as a script's comparison answers it.
fn truth(value: bool) -> (&'static str, String) {
    number(Number::Integer(i64::from(value)))
}

/// The answer of `"text" * 1`.
fn multiplied(text: &str) -> (&'static str, String) {
    match TjsString::from(text).to_number() {
        Ok(value) => number(value),
        // an arithmetic operand reads a text that is not a number as the
        // integer zero
        Err(NumberError::NotANumber) => number(Number::Integer(0)),
        Err(other) => panic!("the value layer could not read {text:?}: {other}"),
    }
}

/// The answer of `"text".length`.
fn length(text: &str) -> (&'static str, String) {
    let units = TjsString::from(text).len();
    number(Number::Integer(
        i64::try_from(units).expect("the length fits a 64-bit integer"),
    ))
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_answers() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/value_layer.txt");
    let table = fs::read_to_string(&path).expect("read the value layer fixture");
    let mut records = Vec::new();
    for line in table.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(probe), Some(kind), Some(escaped)) =
            (fields.next(), fields.next(), fields.next())
        else {
            panic!("the fixture line is not `probe<TAB>kind<TAB>text`: {line:?}");
        };
        assert!(
            fields.next().is_none(),
            "the fixture line has more than three fields: {line:?}"
        );
        records.push(Record {
            probe: probe.to_owned(),
            kind: kind.to_owned(),
            text: unescaped(escaped),
        });
    }
    assert!(
        !records.is_empty(),
        "the value layer fixture records nothing; record it with \
         `cargo run -p tjs --example value_oracle > crates/tjs/tests/fixtures/value_layer.txt`"
    );
    records
}

/// The text the fixture's escaping writes.
fn unescaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        match characters.next() {
            Some('t') => out.push('\t'),
            Some('n') => out.push('\n'),
            Some('\\') => out.push('\\'),
            Some('x') => {
                let high = characters.next().and_then(|digit| digit.to_digit(16));
                let low = characters.next().and_then(|digit| digit.to_digit(16));
                let (Some(high), Some(low)) = (high, low) else {
                    panic!("the fixture holds a truncated hexadecimal escape: {text:?}");
                };
                let code = high * 16 + low;
                out.push(char::from_u32(code).expect("the fixture escapes a code point"));
            }
            other => panic!("the fixture holds an unknown escape: {other:?}"),
        }
    }
    out
}
