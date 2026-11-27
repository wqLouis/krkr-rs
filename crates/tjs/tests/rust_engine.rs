//! The Rust executor against the engine's recorded answers.
//!
//! `tests/fixtures/value_layer.txt` and `tests/fixtures/object_members.txt`
//! hold what the engine answered for two probe tables whose expressions live
//! in `crates/tjs/examples/value_oracle.rs` and
//! `crates/tjs/examples/object_oracle.rs`. This check compiles the same
//! expressions with the front end and runs them in the Rust executor
//! (`tjs::vm::runtime::engine`), with the engine's global object behind the
//! code, so every probe is a differential between the two executors.
//!
//! The object table's setup script is run by the engine before the probes,
//! exactly as its recorder runs it: the probes then read the same objects in
//! both executors, which is what makes the bridge — an engine object reached
//! through the Rust dispatch surface — the thing under test.
//!
//! The other recorders keep their expressions in their own programs; see
//! [`DEFERRED`]. A record that holds only the header is a failure, not a
//! pass: the table must be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use tjs::Tjs2Engine;
use tjs::vm::runtime::engine::{EngineError, run};
use tjs::vm::runtime::exec::Fault;
use tjs::vm::runtime::variant::Variant;

/// The text a record holds when the language raises instead of answering one.
const RAISED: &str = "!raise";

/// The probes `examples/value_oracle.rs` records, in its order.
///
/// The recorder writes two columns per probe: the answer's kind spelling —
/// what `typeof (expression)` answers — and the answer's text, which is
/// `"" + (expression)` with the one override in [`TEXT_OVERRIDES`].
const VALUE_PROBES: &[(&str, &str)] = &[
    ("real_zero", r#"0.0"#),
    ("real_one_and_half", r#"1.5"#),
    ("real_third", r#"1/3"#),
    ("real_huge", r#"1e20"#),
    ("real_tiny", r#"1e-20"#),
    ("real_negative_zero", r#"-0.0"#),
    ("real_not_a_number", r#"0.0/0.0"#),
    ("real_infinite", r#"1e400"#),
    ("real_negative_infinite", r#"-1e400"#),
    ("integer_zero", r#"0"#),
    ("integer_negative_one", r#"-1"#),
    ("integer_int32_max", r#"2147483647"#),
    ("integer_above_int32", r#"2147483648"#),
    ("integer_above_uint32", r#"4294967296"#),
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
    ("kind_of_integer_text", r#"typeof ("12" * 1)"#),
    ("kind_of_real_text", r#"typeof ("12.5" * 1)"#),
    ("kind_of_word", r#"typeof ("abc" * 1)"#),
    ("escape", r#""\t\n\\\"\x01'".escape()"#),
    ("length_ascii", r#""abc".length"#),
    ("length_surrogate_pair", r#""😀".length"#),
    ("length_top_unit", r#""\xffff".length"#),
    ("compare_lower_against_upper", r#""a" < "B""#),
    ("compare_case", r#""a" == "A""#),
    ("compare_prefix", r#""abc" < "abd""#),
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
    ("bit_and", r#"((new Dictionary())["x"] + 6) & 3"#),
    ("bit_or", r#"((new Dictionary())["x"] + 6) | 3"#),
    ("bit_xor", r#"((new Dictionary())["x"] + 6) ^ 3"#),
    ("shift_left", r#"((new Dictionary())["x"] + 1) << 4"#),
    ("shift_right", r#"((new Dictionary())["x"] - 8) >> 1"#),
    ("bit_not", r#"~((new Dictionary())["x"] + 5)"#),
    ("negate_integer", r#"-((new Dictionary())["x"] + 5)"#),
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
    ("kind_of_integer", r#"typeof (1)"#),
    ("kind_of_real", r#"typeof (1.5)"#),
    ("kind_of_text", r#"typeof ("a")"#),
    ("kind_of_octet", r#"typeof (<%01%>)"#),
    ("kind_of_null", r#"null"#),
];

/// The probes whose text column is not `"" + expression`.
///
/// The value recorder uses a different text expression where the value's own
/// text is not pinnable: an object's text is the two addresses it carries, so
/// the object probe records its `typeof` instead.
const TEXT_OVERRIDES: &[(&str, &str)] = &[("kind_of_null", "typeof (null)")];

/// The probes `examples/object_oracle.rs` records, in its order.
///
/// The recorder evaluates each expression once and reads the kind of the
/// answer; the text is `"" + (expression)`.
const OBJECT_PROBES: &[(&str, &str)] = &[
    ("dot_read", r#"d.a"#),
    ("bracket_read", r#"d["a"]"#),
    ("dot_read_from_bracket_write", r#"d.b"#),
    ("bracket_read_from_dot_write", r#"d["b"]"#),
    ("created_read", r#"d.n"#),
    ("present_typeof", r#"typeof (d.a)"#),
    ("void_typeof", r#"typeof (d.v)"#),
    ("absent_typeof", r#"typeof (d.zz)"#),
    ("deleted_typeof", r#"typeof (d.c)"#),
    ("created_typeof", r#"typeof (d.n)"#),
    ("call_function", r#"f(41)"#),
    ("new_class", r#"made.v"#),
    ("compound_object", r#"plain.n"#),
    ("compound_dictionary", r#"dict.n"#),
    ("string_subscript", r#"s[1]"#),
    ("string_subscript_typeof", r#"typeof (s[1])"#),
    ("octet_subscript", r#"oct[1]"#),
    ("by_number_read", r#"bynum[3]"#),
    ("by_number_write", r#"bynum["4"]"#),
];

/// The script the object recorder runs before its probes.
///
/// The probes name the members it leaves on the global object, so the same
/// script runs before the replay.
const OBJECT_SETUP: &str = r#"function objectMembers() {
    var d = new Dictionary();
    d.a = 1;
    d["b"] = 2;
    d.c = 3;
    d.v = void;
    d.n = 7;
    d["a"] = 4;
    d.b = d["a"] + 1;
    delete d.c;
    var f = function (a) { return a + 1; };
    var made = new Array();
    made.v = 7;
    var plain = new Dictionary();
    plain.n = 1;
    plain.n += 2;
    var dict = new Dictionary();
    dict.n = 1;
    dict.n += 2;
    var s = "abc";
    var oct = <%41 42 43%>;
    var bynum = new Dictionary();
    bynum["3"] = 30;
    bynum[4] = 40;
    var bag = new Dictionary();
    bag.d = d;
    bag.f = f;
    bag.made = made;
    bag.plain = plain;
    bag.dict = dict;
    bag.s = s;
    bag.oct = oct;
    bag.bynum = bynum;
    return bag;
}
var bag = objectMembers();
var d = bag.d;
var f = bag.f;
var made = bag.made;
var plain = bag.plain;
var dict = bag.dict;
var s = bag.s;
var oct = bag.oct;
var bynum = bag.bynum;
delete this.objectMembers;
delete this.bag;"#;

/// The recorders whose expressions are not in the open, so no probe of theirs
/// can be replayed without moving its expression out of the recorder first.
///
/// The fixture is named with its record's size: the count keeps the deferral
/// visible when a recorder grows, and the reason names where the expression
/// lives.
const DEFERRED: &[(&str, usize, &str)] = &[
    (
        "number_parse",
        1121,
        "the rows are text inputs to the number rule, not TJS expressions; \
         `tests/number_parse.rs` already replays them against the rule, and \
         wrapping each text in a script would probe the parser again, not the \
         executor",
    ),
    (
        "array_members",
        156,
        "the expressions live in `examples/array_oracle.rs`; a replay would have \
         to guess them from the ids",
    ),
    (
        "dictionary_members",
        25,
        "the expressions live in `examples/dictionary_oracle.rs`; a replay would \
         have to guess them from the ids",
    ),
    (
        "math_members",
        140,
        "the expressions live in `examples/math_oracle.rs`; a replay would have to \
         guess them from the ids",
    ),
    (
        "date_members",
        157,
        "the expressions live in `examples/date_oracle.rs`; a replay would have to \
         guess them from the ids",
    ),
];

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind the recorder reported.
    kind: String,
    /// The answer's text, or the marker of the raise the recorder saw.
    text: String,
}

/// Runs the value and object probe tables through the Rust executor.
#[test]
fn the_rust_executor_reproduces_the_recorded_answers() {
    let engine = Rc::new(Tjs2Engine::new().expect("the engine starts"));

    let value = records("value_layer.txt", true);
    assert_eq!(
        ids(&value),
        VALUE_PROBES
            .iter()
            .map(|(probe, _)| *probe)
            .collect::<Vec<_>>(),
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example value_oracle`"
    );
    let mut covered = 0;
    for (record, (probe, expression)) in value.iter().zip(VALUE_PROBES) {
        covered += 1;
        // reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2602-2638 — the kind
        // `typeof` reports is the answer's own kind spelling.
        let answer = evaluate(&engine, &format!("return ({expression});"), probe);
        assert_eq!(record.kind, answer.type_name(), "probe {probe}: the kind");
        // reference/cpp/core/tjs2/tjsVariant.cpp:241-249 — the value's own
        // text form, which the concatenation asks for.
        let text = recorded_text(&engine, probe, expression);
        assert_eq!(record.text, text, "probe {probe}: the answer");
    }
    assert_eq!(covered, VALUE_PROBES.len(), "every value probe is covered");

    engine
        .exec_script_discard(OBJECT_SETUP, "object_members")
        .expect("the object recorder's setup script runs on the engine");
    let object = records("object_members.txt", false);
    assert_eq!(
        ids(&object),
        OBJECT_PROBES
            .iter()
            .map(|(probe, _)| *probe)
            .collect::<Vec<_>>(),
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example object_oracle`"
    );
    for (record, (probe, expression)) in object.iter().zip(OBJECT_PROBES) {
        // reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2602-2638 — the kind
        // the marshalled answer carries.
        let answer = evaluate(&engine, &format!("return ({expression});"), probe);
        assert_eq!(record.kind, answer.type_name(), "probe {probe}: the kind");
        // reference/cpp/core/tjs2/tjsVariant.cpp:241-249 — the value's own
        // text form, which the concatenation asks for.
        let text = recorded_text(&engine, probe, expression);
        assert_eq!(record.text, text, "probe {probe}: the answer");
    }

    // The deferred recorders are still there, at the size the list names.
    for (fixture, count, reason) in DEFERRED {
        let recorded = row_count(&format!("{fixture}.txt"));
        assert_eq!(recorded, *count, "fixture {fixture}: {reason}");
    }
    // A text override names a probe the table holds, so it cannot go stale.
    for (probe, _) in TEXT_OVERRIDES {
        assert!(
            VALUE_PROBES.iter().any(|(name, _)| name == probe),
            "the text override {probe} is not in the table"
        );
    }
}

/// Runs one script and answers what its top-level call returned.
fn evaluate(engine: &Rc<Tjs2Engine>, script: &str, probe: &str) -> Variant {
    match run(engine.clone(), script, probe) {
        Ok(answer) => answer,
        Err(error) => panic!("probe {probe}: {error:?}"),
    }
}

/// The text the recorder would have written for `expression`.
///
/// The recorder's text column is `"" + (expression)`, with the one override
/// where the value's own text is not pinnable. The answer's text form is what
/// the concatenation asks for, so the value's own conversion answers it; a
/// conversion the language raises is the record's raise marker.
fn recorded_text(engine: &Rc<Tjs2Engine>, probe: &str, expression: &str) -> String {
    let text_expression = match TEXT_OVERRIDES.iter().find(|(name, _)| *name == probe) {
        Some((_, text)) => (*text).to_string(),
        None => format!("\"\" + ({expression})"),
    };
    match run(engine.clone(), &format!("return {text_expression};"), probe) {
        Ok(answer) => match answer.to_string() {
            Ok(text) => text.to_string(),
            Err(_) => RAISED.to_string(),
        },
        Err(error) if matches!(&error, EngineError::Run(Fault::Raised(_))) => RAISED.to_string(),
        Err(error) => panic!("probe {probe}: {error:?}"),
    }
}

/// The probe names of a record, in its order.
fn ids(records: &[Record]) -> Vec<&str> {
    records.iter().map(|record| record.probe.as_str()).collect()
}

/// The text the value fixture's escaping writes.
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

/// The number of data rows a fixture holds.
///
/// A deferred fixture's rows are not all three-column records, so only their
/// count is read: it keeps the deferral visible without guessing a shape.
fn row_count(fixture: &str) -> usize {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let table = fs::read_to_string(&path).expect("read the fixture");
    table
        .lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .count()
}

/// The records of a fixture; a record with no data line is a failure.
///
/// `escaped` says whether the recorder wrote its text with the fixture's
/// escaping: the value recorder does, so one probe stays on one line, and the
/// object recorder does not.
fn records(fixture: &str, escaped: bool) -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(fixture);
    let table = fs::read_to_string(&path).expect("read the fixture");
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
            text: if escaped {
                unescaped(text)
            } else {
                text.to_owned()
            },
        });
    }
    assert!(
        !records.is_empty(),
        "the fixture {fixture} records nothing; record it with the example that writes it"
    );
    records
}
