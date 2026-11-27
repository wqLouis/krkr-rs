//! The two values that answer a call without an object, against the engine's
//! recorded answers.
//!
//! `tests/fixtures/string_members.txt` holds what the engine answered for a
//! text's method surface and for a byte string's one member; the class a script
//! cannot see, the member reads that are not method values, and one call per
//! rule with the arguments the rule turns on. `crates/tjs/examples/string_oracle.rs`
//! records it. The check below replays every call through the tables in
//! `tjs::vm::runtime::natives::string` and compares the kind and the text of
//! the answer, so the tables are the engine's and not only their own idea of the
//! surface.
//!
//! The probes that reach a rule this check cannot drive are recorded and stated
//! rather than replayed: the member read diversion, the class the engine does not
//! register, the pattern a `replace` asks, and the array a `split` asks, whose
//! `split` member this tree has not built yet.
//!
//! A record that holds only the header is a failure, not a pass: the table must
//! be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use tjs::vm::runtime::natives::string::{OctetMember, StringMember};
use tjs::vm::runtime::object::{Dispatch, MemberFlags};
use tjs::vm::runtime::octet::Octet;
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::Variant;

/// The kind a raised probe records.
const RAISED: &str = "!raise";

/// The probes the record must hold, in the recorder's order.
const PROBES: &[&str] = &[
    "surface_class_typeof",
    "property_length",
    "property_index",
    "property_method_read",
    "property_method_typeof",
    "property_octet_unpack_read",
    "property_octet_unpack_typeof",
    "char_at",
    "char_at_end",
    "char_at_negative",
    "char_at_blank",
    "char_at_text_index",
    "char_at_missing",
    "char_at_extra",
    "index_of",
    "index_of_start",
    "index_of_miss",
    "index_of_empty_needle",
    "index_of_empty_target",
    "index_of_number_needle",
    "index_of_start_past",
    "index_of_missing",
    "index_of_three",
    "upper",
    "upper_count",
    "lower",
    "lower_count",
    "substring_start",
    "substring_count",
    "substring_past",
    "substring_negative_count",
    "substring_at_end",
    "substring_negative_start",
    "substr_count",
    "substring_missing",
    "sprintf_text",
    "sprintf_width",
    "sprintf_zero_width",
    "sprintf_left",
    "sprintf_plus",
    "sprintf_hex",
    "sprintf_alternate",
    "sprintf_unsigned",
    "sprintf_real",
    "sprintf_real_width",
    "sprintf_exponent",
    "sprintf_general",
    "sprintf_percent",
    "sprintf_character",
    "sprintf_text_precision",
    "sprintf_unknown_kind",
    "sprintf_truncated",
    "sprintf_missing",
    "sprintf_extra",
    "sprintf_real_as_integer",
    "replace_text_pattern",
    "replace_missing",
    "replace_regex",
    "escape",
    "split_count",
    "split_first",
    "split_purge_count",
    "split_missing",
    "trim",
    "trim_blank",
    "trim_count",
    "reverse",
    "reverse_count",
    "repeat",
    "repeat_zero",
    "repeat_negative",
    "repeat_missing",
    "unpack_count",
    "unpack_first",
    "unpack_third",
    "unpack_type",
    "unpack_ascii",
    "unpack_extra_argument",
    "unpack_missing",
    "unpack_number",
    "unpack_unknown_format",
];

/// The probes whose rule this check cannot drive: the recorded answer.
///
/// A member read of a method's name is the read diversion's rule, and the class
/// probe is a name resolution; neither is a member call. The two shims ask an
/// object: a pattern's `replace` needs the regular-expression class, and a fresh
/// array's `split` needs the member this tree has not built yet.
const RECORDED: &[(&str, &str, &str)] = &[
    ("surface_class_typeof", RAISED, ""),
    ("property_length", "Integer", "3"),
    ("property_index", "String", "b"),
    ("property_method_read", RAISED, ""),
    ("property_method_typeof", RAISED, ""),
    ("property_octet_unpack_read", RAISED, ""),
    ("property_octet_unpack_typeof", RAISED, ""),
    ("replace_regex", "String", "aXb"),
    ("split_count", "Integer", "3"),
    ("split_first", "String", "a"),
    ("split_purge_count", "Integer", "2"),
];

/// A probe the check replays that the language raised.
struct Refused;

#[test]
fn the_value_member_tables_reproduce_the_recorded_answers() {
    let records = recorded_answers();
    let seen: Vec<&str> = records.iter().map(|record| record.probe.as_str()).collect();
    assert_eq!(
        seen.as_slice(),
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example string_oracle`"
    );
    for record in &records {
        if let Some((_, kind, text)) = RECORDED.iter().find(|(probe, _, _)| *probe == record.probe)
        {
            assert_eq!(record.kind, *kind, "probe {}: the kind", record.probe);
            assert_eq!(record.text, *text, "probe {}: the text", record.probe);
            continue;
        }
        match replay(&record.probe) {
            Ok(answer) => {
                assert_ne!(
                    record.kind, RAISED,
                    "probe {}: the call answers where the engine raised",
                    record.probe
                );
                assert_eq!(
                    record.kind,
                    answer.type_name(),
                    "probe {}: the kind",
                    record.probe
                );
                let text = answer.to_string().unwrap_or_else(|error| {
                    panic!("probe {}: the answer has no text: {error}", record.probe)
                });
                assert_eq!(
                    record.text,
                    escaped(&text),
                    "probe {}: the text",
                    record.probe
                );
            }
            Err(Refused) => assert_eq!(
                record.kind, RAISED,
                "probe {}: the call raised where the engine answered",
                record.probe
            ),
        }
    }
}

/// Calls a text member on a fixed target.
fn call(member: StringMember, target: &str, parameters: &[Variant]) -> Result<Variant, Refused> {
    member
        .call(&TjsString::from(target), parameters)
        .map_err(|_refused| Refused)
}

/// Calls the byte string's member on a fixed value.
fn unpack(bytes: &[u8], parameters: &[Variant]) -> Result<Variant, Refused> {
    OctetMember::Unpack
        .call(&Octet::from_slice(bytes), parameters)
        .map_err(|_refused| Refused)
}

/// The answer below an array the member answered.
fn array(answer: Result<Variant, Refused>) -> Result<Rc<dyn Dispatch>, Refused> {
    match answer? {
        Variant::Object(closure) => closure.object.ok_or(Refused),
        _ => Err(Refused),
    }
}

/// The number of elements the array the member answered holds.
fn count(answer: Result<Variant, Refused>) -> Result<Variant, Refused> {
    let object = array(answer)?;
    object
        .prop_get(&TjsString::from("count"), MemberFlags::NONE, None)
        .map_err(|_refused| Refused)
}

/// One element of the array the member answered holds.
fn element(answer: Result<Variant, Refused>, index: i64) -> Result<Variant, Refused> {
    let object = array(answer)?;
    object
        .prop_get_by_num(index, MemberFlags::NONE, None)
        .map_err(|_refused| Refused)
}

/// The answer the member table gives for one recorded call.
fn replay(probe: &str) -> Result<Variant, Refused> {
    match probe {
        "char_at" => call(StringMember::CharAt, "abc", &[Variant::from(1i64)]),
        "char_at_end" => call(StringMember::CharAt, "abc", &[Variant::from(3i64)]),
        "char_at_negative" => call(StringMember::CharAt, "abc", &[Variant::from(-1i64)]),
        "char_at_blank" => call(StringMember::CharAt, "", &[Variant::from(0i64)]),
        "char_at_text_index" => call(StringMember::CharAt, "abc", &[Variant::from("1")]),
        "char_at_missing" => call(StringMember::CharAt, "abc", &[]),
        "char_at_extra" => call(
            StringMember::CharAt,
            "abc",
            &[Variant::from(0i64), Variant::from(1i64)],
        ),
        "index_of" => call(StringMember::IndexOf, "abcabc", &[Variant::from("b")]),
        "index_of_start" => call(
            StringMember::IndexOf,
            "abcabc",
            &[Variant::from("b"), Variant::from(2i64)],
        ),
        "index_of_miss" => call(StringMember::IndexOf, "abc", &[Variant::from("z")]),
        "index_of_empty_needle" => call(StringMember::IndexOf, "abc", &[Variant::from("")]),
        "index_of_empty_target" => call(StringMember::IndexOf, "", &[Variant::from("a")]),
        "index_of_number_needle" => call(StringMember::IndexOf, "a1b", &[Variant::from(1i64)]),
        "index_of_start_past" => call(
            StringMember::IndexOf,
            "abc",
            &[Variant::from("a"), Variant::from(5i64)],
        ),
        "index_of_missing" => call(StringMember::IndexOf, "abc", &[]),
        "index_of_three" => call(
            StringMember::IndexOf,
            "abc",
            &[Variant::from("a"), Variant::from(0i64), Variant::from(0i64)],
        ),
        "upper" => call(StringMember::ToUpperCase, "aBc1", &[]),
        "upper_count" => call(StringMember::ToUpperCase, "a", &[Variant::from(1i64)]),
        "lower" => call(StringMember::ToLowerCase, "AbC1", &[]),
        "lower_count" => call(StringMember::ToLowerCase, "a", &[Variant::from(1i64)]),
        "substring_start" => call(StringMember::Substring, "abcdef", &[Variant::from(2i64)]),
        "substring_count" => call(
            StringMember::Substring,
            "abcdef",
            &[Variant::from(2i64), Variant::from(3i64)],
        ),
        "substring_past" => call(
            StringMember::Substring,
            "abcdef",
            &[Variant::from(2i64), Variant::from(99i64)],
        ),
        "substring_negative_count" => call(
            StringMember::Substring,
            "abcdef",
            &[Variant::from(2i64), Variant::from(-1i64)],
        ),
        "substring_at_end" => call(StringMember::Substring, "abcdef", &[Variant::from(6i64)]),
        "substring_negative_start" => {
            call(StringMember::Substring, "abcdef", &[Variant::from(-1i64)])
        }
        "substr_count" => call(
            StringMember::Substr,
            "abcdef",
            &[Variant::from(1i64), Variant::from(2i64)],
        ),
        "substring_missing" => call(StringMember::Substring, "abcdef", &[]),
        "sprintf_text" => call(
            StringMember::Sprintf,
            "%d and %s",
            &[Variant::from(5i64), Variant::from("x")],
        ),
        "sprintf_width" => call(StringMember::Sprintf, "[%5d]", &[Variant::from(42i64)]),
        "sprintf_zero_width" => call(StringMember::Sprintf, "[%05d]", &[Variant::from(-42i64)]),
        "sprintf_left" => call(StringMember::Sprintf, "[%-5d]", &[Variant::from(42i64)]),
        "sprintf_plus" => call(StringMember::Sprintf, "%+d", &[Variant::from(42i64)]),
        "sprintf_hex" => call(
            StringMember::Sprintf,
            "%x %X",
            &[Variant::from(255i64), Variant::from(255i64)],
        ),
        "sprintf_alternate" => call(
            StringMember::Sprintf,
            "%#x %#o",
            &[Variant::from(255i64), Variant::from(8i64)],
        ),
        "sprintf_unsigned" => call(StringMember::Sprintf, "%u", &[Variant::from(-1i64)]),
        "sprintf_real" => call(
            StringMember::Sprintf,
            "%.2f",
            &[Variant::from(std::f64::consts::PI)],
        ),
        "sprintf_real_width" => call(StringMember::Sprintf, "[%8.3f]", &[Variant::from(1.5f64)]),
        "sprintf_exponent" => call(
            StringMember::Sprintf,
            "%.3e",
            &[Variant::from(12345.678f64)],
        ),
        "sprintf_general" => call(StringMember::Sprintf, "%g", &[Variant::from(0.0001f64)]),
        "sprintf_percent" => call(StringMember::Sprintf, "%d%%", &[Variant::from(50i64)]),
        "sprintf_character" => call(StringMember::Sprintf, "%c", &[Variant::from(65i64)]),
        "sprintf_text_precision" => call(StringMember::Sprintf, "%.2s", &[Variant::from("abcdef")]),
        "sprintf_unknown_kind" => call(StringMember::Sprintf, "%q", &[]),
        "sprintf_truncated" => call(StringMember::Sprintf, "%", &[]),
        "sprintf_missing" => call(StringMember::Sprintf, "%d", &[]),
        "sprintf_extra" => call(
            StringMember::Sprintf,
            "%d",
            &[Variant::from(1i64), Variant::from(2i64)],
        ),
        "sprintf_real_as_integer" => call(StringMember::Sprintf, "%d", &[Variant::from(1.9f64)]),
        "replace_text_pattern" => call(
            StringMember::Replace,
            "abc",
            &[Variant::from("b"), Variant::from("x")],
        ),
        "replace_missing" => call(StringMember::Replace, "abc", &[Variant::from("b")]),
        "escape" => call(StringMember::Escape, "a\tb", &[]),
        "split_missing" => call(StringMember::Split, "a,b", &[]),
        "trim" => call(StringMember::Trim, " \t ab \n", &[]),
        "trim_blank" => call(StringMember::Trim, "   ", &[]),
        "trim_count" => call(StringMember::Trim, " a ", &[Variant::from(1i64)]),
        "reverse" => call(StringMember::Reverse, "abc", &[]),
        "reverse_count" => call(StringMember::Reverse, "a", &[Variant::from(1i64)]),
        "repeat" => call(StringMember::Repeat, "ab", &[Variant::from(3i64)]),
        "repeat_zero" => call(StringMember::Repeat, "ab", &[Variant::from(0i64)]),
        "repeat_negative" => call(StringMember::Repeat, "ab", &[Variant::from(-1i64)]),
        "repeat_missing" => call(StringMember::Repeat, "ab", &[]),
        "unpack_count" => count(unpack(&[1, 2, 3], &[Variant::from("c3")])),
        "unpack_first" => element(unpack(&[1, 2, 3], &[Variant::from("c3")]), 0),
        "unpack_third" => element(unpack(&[1, 2, 3], &[Variant::from("c3")]), 2),
        "unpack_type" => element(unpack(&[1, 2, 3], &[Variant::from("c3")]), 0)
            .map(|value| Variant::String(TjsString::from(value.type_name()))),
        "unpack_ascii" => element(unpack(&[1, 2, 3], &[Variant::from("a3")]), 0),
        "unpack_extra_argument" => count(unpack(
            &[1, 2, 3],
            &[Variant::from("c3"), Variant::from(9i64)],
        )),
        "unpack_missing" => unpack(&[1, 2, 3], &[]),
        "unpack_number" => unpack(&[1, 2, 3], &[Variant::from(5i64)]),
        "unpack_unknown_format" => unpack(&[1, 2, 3], &[Variant::from("q")]),
        other => panic!("the check has no replay for the recorded probe: {other}"),
    }
}

/// The text escaped so that one probe stays on one line, the same way the
/// recorder writes it.
fn escaped(text: &TjsString) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in String::from_utf16_lossy(text.as_utf16()).chars() {
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

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind `typeof` reports for the answer.
    kind: String,
    /// The answer as text, escaped.
    text: String,
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_answers() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/string_members.txt");
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "the member fixture {} is not readable: {error}; record it with \
             `cargo run -p tjs --example string_oracle`",
            path.display()
        ),
    };
    let mut records = Vec::new();
    for line in table.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut fields = line.split('\t');
        let (Some(probe), Some(kind), Some(text)) = (fields.next(), fields.next(), fields.next())
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
            text: text.to_owned(),
        });
    }
    assert!(
        !records.is_empty(),
        "the member fixture records nothing; record it with \
         `cargo run -p tjs --example string_oracle`"
    );
    records
}
