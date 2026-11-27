//! The message table against the engine's recorded answers.
//!
//! `tests/fixtures/message_table.txt` holds what the engine answered for a
//! fixed probe table; `crates/tjs/examples/message_oracle.rs` records it. The
//! check below reproduces every default through `message_of`, replays the
//! assignments through `tjs::vm::runtime::message`, and compares a later raise,
//! an exception that was already made, an unknown id and a text carrying
//! placeholders.
//!
//! A record that holds only the header is a failure, not a pass: the table must
//! be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;

use tjs::vm::runtime::exec::Raise;
use tjs::vm::runtime::exec::throw::{Unwind, message_of};
use tjs::vm::runtime::message::{
    self, TJS_BAD_PARAM_COUNT, TJS_INTEGER_OUT_OF_RANGE, TJS_NOT_IMPLEMENTED,
    TJS_VARIANT_CONVERT_ERROR,
};
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::{Closure, Target, Variant};

/// The probes the record must hold, in the recorder's order.
const PROBES: &[&str] = &[
    "default_variant_convert",
    "default_variant_convert_to_object",
    "default_divide_by_zero",
    "default_integer_out_of_range",
    "default_member_not_found",
    "default_access_denied",
    "default_range_error",
    "default_not_callable",
    "default_parameter_count",
    "default_invalid_parameter",
    "default_format_string",
    "default_not_an_array",
    "default_not_implemented",
    "assign_known",
    "later_raise",
    "saved_raise",
    "assign_replacement",
    "already_raised",
    "later_raise_after_replacement",
    "assign_unknown",
    "assign_internal_error",
    "assign_with_placeholder",
    "later_raise_with_placeholder",
    "assign_not_implemented",
    "later_raise_not_implemented",
    "assign_integer_out_of_range",
    "later_raise_integer_out_of_range",
];

// reference/krkr2/cpp/core/tjs2/tjsError.h:264, :266-273, :294-296, :302, :308 —
// the raise texts; reference/krkr2/cpp/core/tjs2/tjsMessage.cpp:36-48, :116-120 —
// the assignment and the unknown id.
#[test]
fn the_message_table_reproduces_the_recorded_answers() {
    let records = recorded_messages();
    let probes: Vec<&str> = records.iter().map(|record| record.probe.as_str()).collect();
    assert_eq!(
        probes.as_slice(),
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example message_oracle`"
    );
    message::reset();
    let mut saved = None;
    for record in &records {
        assert_eq!(
            record.message,
            answer(&record.probe, &mut saved),
            "probe {}: the message",
            record.probe
        );
    }
    message::reset();
}

/// A failure the front end made keeps the text it carries rather than reading a
/// registered id (`reference/krkr2/cpp/core/tjs2/tjsError.h:288`).
#[test]
fn a_raise_without_a_registered_id_keeps_its_text() {
    message::reset();
    assert!(message::assign(TJS_NOT_IMPLEMENTED, "changed"));
    let unwind = Unwind::raised(Raise::ScriptError)
        .with_text(TjsString::from("Syntax error (expected an expression)"));
    assert_eq!(message_of(&unwind), "Syntax error (expected an expression)");
    message::reset();
}

/// The message the Rust table gives a probe.
fn answer(probe: &str, saved: &mut Option<String>) -> String {
    match probe {
        "default_variant_convert" => message_of(&conversion(
            Variant::Object(Closure::null()),
            Target::Integer,
        )),
        "default_variant_convert_to_object" => {
            message_of(&conversion(Variant::Integer(1), Target::Object))
        }
        "default_divide_by_zero" => message_of(&Raise::DivideByZero.into()),
        "default_integer_out_of_range" => message_of(&Raise::IntegerOutOfRange.into()),
        "default_member_not_found" => {
            message_of(&Unwind::raised(Raise::MemberNotFound).with_text(TjsString::from("absent")))
        }
        "default_access_denied" => message_of(&Raise::AccessDenied.into()),
        "default_range_error" => message_of(&Raise::RangeError.into()),
        "default_not_callable" => message_of(&Raise::NotCallable.into()),
        "default_parameter_count" => message_of(&Raise::ParameterCount.into()),
        "default_invalid_parameter" => message_of(&Raise::InvalidParameter.into()),
        "default_format_string" => message_of(&Raise::FormatString.into()),
        "default_not_an_array" => message_of(&Raise::NotAnArray.into()),
        "default_not_implemented" => message_of(&not_implemented()),
        "assign_known" => bool_text(message::assign(TJS_BAD_PARAM_COUNT, "the assigned count")),
        "later_raise" => message_of(&Raise::ParameterCount.into()),
        "saved_raise" => {
            let text = message_of(&Raise::ParameterCount.into());
            *saved = Some(text.clone());
            text
        }
        "assign_replacement" => bool_text(message::assign(TJS_BAD_PARAM_COUNT, "the second count")),
        "already_raised" => saved
            .clone()
            .expect("the record saves an exception before it reassigns the message"),
        "later_raise_after_replacement" => message_of(&Raise::ParameterCount.into()),
        "assign_unknown" => bool_text(message::assign("not-a-real-message", "x")),
        "assign_internal_error" => bool_text(message::assign("TJSInternalError", "hello")),
        "assign_with_placeholder" => bool_text(message::assign(
            TJS_VARIANT_CONVERT_ERROR,
            "changed %1 to %2",
        )),
        "later_raise_with_placeholder" => message_of(&conversion(
            Variant::Object(Closure::null()),
            Target::Integer,
        )),
        "assign_not_implemented" => {
            bool_text(message::assign(TJS_NOT_IMPLEMENTED, "the assigned method"))
        }
        "later_raise_not_implemented" => message_of(&not_implemented()),
        "assign_integer_out_of_range" => bool_text(message::assign(
            TJS_INTEGER_OUT_OF_RANGE,
            "the assigned integer",
        )),
        "later_raise_integer_out_of_range" => message_of(&Raise::IntegerOutOfRange.into()),
        other => panic!("the check has no replay for the recorded probe: {other}"),
    }
}

/// A refused conversion's raise.
fn conversion(value: Variant, target: Target) -> Unwind {
    Unwind::raised(Raise::Convert { to: target.name() }).with_value(&value)
}

/// The declaration failure as the call path makes it, before the message table
/// is read.
fn not_implemented() -> Unwind {
    Unwind::raised(Raise::ScriptError)
        .with_text(TjsString::from("Called method is not implemented"))
}

/// The text a boolean answers in the record.
fn bool_text(value: bool) -> String {
    if value {
        String::from("1")
    } else {
        String::from("0")
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
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/message_table.txt");
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "the message fixture {} is not readable: {error}; record it with \
             `cargo run -p tjs --example message_oracle`",
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
         `cargo run -p tjs --example message_oracle`"
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
