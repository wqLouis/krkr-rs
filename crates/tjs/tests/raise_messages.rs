//! The failure messages against the engine's recorded answers.
//!
//! `tests/fixtures/raise_messages.txt` holds what the engine answered for a
//! fixed probe table; `crates/tjs/examples/raise_oracle.rs` records it. The
//! check below reproduces every message through the message table in
//! `tjs::vm::runtime::exec::throw` and the readable form in
//! `tjs::vm::runtime::variant`, so the table is the engine's and not only its
//! own idea of the text.
//!
//! A record that holds only the header is a failure, not a pass: the table must
//! be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;

use tjs::vm::runtime::exec::Raise;
use tjs::vm::runtime::exec::throw::message_of;
use tjs::vm::runtime::octet::Octet;
use tjs::vm::runtime::variant::{Closure, Target, Variant};

/// The probes the record must hold, in the recorder's order.
const PROBES: &[&str] = &[
    "convert_int_null",
    "convert_real_null",
    "convert_string_octet",
    "convert_int_octet",
    "convert_real_octet",
    "convert_number_null",
    "convert_object_integer",
    "convert_increment_octet",
    "convert_increment_octet_long",
    "convert_string_octet_long",
    "access_denied_text_length",
    "range_error_text_index",
    "range_error_octet_index",
    "format_string_sprintf",
    "parameter_count_sprintf",
    "parameter_count_char_at",
    "parameter_count_unpack",
    "invalid_parameter_unpack",
];

// reference/krkr2/cpp/core/tjs2/tjsError.h:250-253 — the conversion message
// reference/krkr2/cpp/core/tjs2/tjsError.h:294-296 — the range and the access
// reference/krkr2/cpp/core/tjs2/tjsError.h:267-268, :276, :302 — the member failures
// reference/krkr2/cpp/core/tjs2/tjsUtils.cpp:78-90, :111-173 — the readable form
#[test]
fn the_raise_messages_are_the_recorded_ones() {
    let records = recorded_messages();
    let probes: Vec<&str> = records.iter().map(|record| record.probe.as_str()).collect();
    assert_eq!(
        probes.as_slice(),
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example raise_oracle`"
    );
    for record in &records {
        assert_eq!(
            record.message,
            message(&record.probe),
            "probe {}: the message",
            record.probe
        );
    }
}

/// The message the Rust table gives a probe's refusal.
fn message(probe: &str) -> String {
    let raise = match probe {
        "convert_int_null" => conversion(Variant::Object(Closure::null()), Target::Integer),
        "convert_real_null" => conversion(Variant::Object(Closure::null()), Target::Real),
        "convert_string_octet" => conversion(Variant::from(Octet::from_slice(&[1])), Target::Text),
        "convert_int_octet" => conversion(Variant::from(Octet::from_slice(&[1])), Target::Integer),
        "convert_real_octet" => conversion(Variant::from(Octet::from_slice(&[1])), Target::Real),
        "convert_number_null" => conversion(Variant::Object(Closure::null()), Target::Number),
        "convert_object_integer" => conversion(Variant::Integer(1), Target::Object),
        "convert_increment_octet" => {
            conversion(Variant::from(Octet::from_slice(&[1])), Target::Number)
        }
        "convert_increment_octet_long" => {
            conversion(Variant::from(Octet::from_slice(&[1; 174])), Target::Number)
        }
        "convert_string_octet_long" => {
            conversion(Variant::from(Octet::from_slice(&[1; 174])), Target::Text)
        }
        "access_denied_text_length" => Raise::AccessDenied,
        "range_error_text_index" => Raise::RangeError,
        "range_error_octet_index" => Raise::RangeError,
        "format_string_sprintf" => Raise::FormatString,
        "parameter_count_sprintf" => Raise::ParameterCount,
        "parameter_count_char_at" => Raise::ParameterCount,
        "parameter_count_unpack" => Raise::ParameterCount,
        "invalid_parameter_unpack" => Raise::InvalidParameter,
        other => panic!("the check has no replay for the recorded probe: {other}"),
    };
    message_of(&raise)
}

/// A refused conversion's raise.
fn conversion(value: Variant, target: Target) -> Raise {
    Raise::Convert {
        value: Box::new(value),
        to: target.name(),
    }
}

/// One recorded message.
struct Record {
    /// The probe's name.
    probe: String,
    /// The message as text, with the record's escaping decoded.
    message: String,
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_messages() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/raise_messages.txt");
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "the message fixture {} is not readable: {error}; record it with \
             `cargo run -p tjs --example raise_oracle`",
            path.display()
        ),
    };
    let mut records = Vec::new();
    for line in table.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((probe, message)) = line.split_once('\t') else {
            panic!("the fixture line is not `probe<TAB>message`: {line:?}");
        };
        assert!(
            !message.contains('\t'),
            "the fixture message holds an unescaped tab: {line:?}"
        );
        records.push(Record {
            probe: probe.to_owned(),
            message: unescaped(message),
        });
    }
    assert!(
        !records.is_empty(),
        "the message fixture records nothing; record it with \
         `cargo run -p tjs --example raise_oracle`"
    );
    records
}

/// The text the recorder's escaping writes.
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
