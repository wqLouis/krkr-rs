//! The answer table against the Rust executor.
//!
//! `tests/fixtures/interpreter_answers.txt` holds what the engine answered for
//! 130 expressions; `crates/tjs/examples/interpreter_oracle.rs` records it. The
//! check compiles the same expression list with the front end and runs it in
//! the Rust executor (`tjs::vm::runtime::engine`), with the recorder's setup
//! script run first, so every row is a differential between the two executors.
//!
//! An answer is the kind `typeof` names and the value's text, joined in one
//! call by the setup's `probeAnswer`, so a probe is evaluated once. Every probe
//! asks for a scalar, a `typeof` or a member that is one: an object's text is
//! the two addresses it carries and a byte string has no text form, so the
//! recorder refuses such an answer.
//!
//! The table's first line is the recorder's header; a table without it is
//! refused, because a stale or hand-written fixture is not a record.
//!
//! Every divergence is collected and reported together: one probe's failure
//! does not hide the next one's.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use tjs::Tjs2Engine;
use tjs::vm::runtime::bridge::{SharedGlobals, global_members};
use tjs::vm::runtime::engine::run_with_members;
use tjs::vm::runtime::variant::Variant;

/// The header line the recorder writes; a table without it is not a record.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/interpreter_oracle.rs` (the answer table); regenerate with `cargo run -p tjs --example interpreter_oracle`";

/// The setup script the recorder runs before its probes.
const SETUP: &str = r#"function probeValue(value) {
    return value;
}
function probeAnswer(value) {
    return (typeof value) + "|" + ("" + value);
}
function probeAddOne(value) {
    return value + 1;
}
function probeSumThree(a, b, c) {
    return a + b + c;
}
function probeThisMember() {
    return this.n;
}
var probeGlobalValue = 0;
function probeReadGlobal() {
    return probeGlobalValue;
}
function probeDictionary() {
    var d = new Dictionary();
    d.n = 11;
    d.s = "text";
    d.v = void;
    return d;
}
function probeArray() {
    var a = new Array();
    a[0] = 10;
    a[1] = 20;
    a.push(30);
    return a;
}
"#;

/// The probes the record must hold, in the recorder's order.
const PROBES: &[(&str, &str)] = &[
    // the literals and the kind each one carries
    ("literal_integer", "42"),
    ("literal_integer_negative", "-42"),
    ("literal_integer_hexadecimal", "0x2a"),
    ("literal_integer_binary", "0b1010"),
    ("literal_integer_largest", "9223372036854775807"),
    ("literal_real_decimal", "1.25"),
    ("literal_real_exponent", "1.5e3"),
    ("literal_real_negative_zero", "-0.0"),
    ("literal_real_infinity", "1e400"),
    ("literal_real_not_a_number", "0.0 / 0.0"),
    ("literal_text_plain", r#""abc""#),
    ("literal_text_empty", r#""""#),
    ("literal_text_escapes", r#""a\tb\n\\""#),
    ("literal_text_wide", r#""あいう""#),
    ("literal_octet_typeof", "typeof <%01 02 03%>"),
    ("literal_regexp_typeof", "typeof /a/"),
    // the unary operators
    ("unary_plus_text", r#"+probeValue("12")"#),
    ("unary_minus_real", "-probeValue(1.5)"),
    ("unary_minus_text", r#"-probeValue("3")"#),
    ("unary_not_zero", "!probeValue(0)"),
    ("unary_not_text_zero", r#"!probeValue("0")"#),
    ("unary_not_empty_text", r#"!probeValue("")"#),
    ("unary_complement", "~probeValue(5)"),
    ("unary_complement_negative", "~probeValue(-1)"),
    ("unary_character_code", r#"#probeValue("A")"#),
    ("unary_character_from", "$probeValue(65)"),
    // integer arithmetic
    ("add_integers", "probeValue(2) + probeValue(3)"),
    ("subtract_integers", "probeValue(2) - probeValue(3)"),
    ("multiply_integers", "probeValue(6) * probeValue(7)"),
    ("divide_integers", "probeValue(7) / probeValue(2)"),
    ("divide_exact", "probeValue(8) / probeValue(2)"),
    (r#"integer_divide"#, r#"probeValue(7) \ probeValue(2)"#),
    (
        "integer_divide_negative",
        r#"probeValue(-7) \ probeValue(2)"#,
    ),
    ("remainder", "probeValue(7) % probeValue(3)"),
    ("remainder_negative", "probeValue(-7) % probeValue(3)"),
    (
        "add_large_integers",
        "probeValue(4294967296) + probeValue(4294967296)",
    ),
    (
        "multiply_large_integers",
        "probeValue(1000000) * probeValue(1000000)",
    ),
    ("truncate_real", "(int)probeValue(3.9)"),
    // real and mixed arithmetic
    ("add_reals", "probeValue(1.5) + probeValue(2.25)"),
    ("subtract_reals", "probeValue(1.5) - probeValue(2.25)"),
    ("multiply_real_integer", "probeValue(1.5) * probeValue(2)"),
    ("divide_reals", "probeValue(1.0) / probeValue(4.0)"),
    ("add_integer_real", "probeValue(1) + probeValue(2.5)"),
    ("multiply_negative_zero", "probeValue(0.0) * probeValue(-1)"),
    ("add_number_text", r#"probeValue(1) + probeValue("2")"#),
    ("add_text_number", r#"probeValue("a") + probeValue(1)"#),
    // the comparisons
    ("equal_integers", "probeValue(1) == probeValue(1)"),
    ("equal_integer_real", "probeValue(1) == probeValue(1.0)"),
    ("equal_integer_text", r#"probeValue(1) == probeValue("1")"#),
    ("equal_texts", r#"probeValue("a") == probeValue("a")"#),
    (
        "equal_void_zero",
        r#"(new Dictionary())["x"] == probeValue(0)"#,
    ),
    ("not_equal", "probeValue(1) != probeValue(2)"),
    ("less", "probeValue(1) < probeValue(2)"),
    ("greater", "probeValue(2) > probeValue(1)"),
    ("less_or_equal", "probeValue(2) <= probeValue(2)"),
    ("greater_or_equal", "probeValue(2) >= probeValue(2)"),
    ("less_texts", r#"probeValue("a") < probeValue("b")"#),
    ("less_text_number", r#"probeValue("10") < probeValue(9)"#),
    ("strict_equal_integers", "probeValue(1) === probeValue(1)"),
    (
        "strict_equal_integer_real",
        "probeValue(1) === probeValue(1.0)",
    ),
    ("strict_not_equal", "probeValue(1) !== probeValue(1.0)"),
    (
        "equal_not_a_number",
        "probeValue(0.0 / 0.0) == probeValue(0.0 / 0.0)",
    ),
    // the logical rules
    ("logical_and_true", "probeValue(1) && probeValue(1)"),
    ("logical_and_false", "probeValue(1) && probeValue(0)"),
    ("logical_or_false", "probeValue(0) || probeValue(0)"),
    ("logical_or_true", "probeValue(0) || probeValue(1)"),
    (
        "logical_and_short_circuit",
        r#"probeValue(0) && (new Dictionary())["x"]"#,
    ),
    (
        "logical_or_short_circuit",
        r#"probeValue(1) || (new Dictionary())["x"]"#,
    ),
    (
        "ternary_true",
        r#"probeValue(1) ? probeValue("yes") : probeValue("no")"#,
    ),
    (
        "ternary_false",
        r#"probeValue(0) ? probeValue("yes") : probeValue("no")"#,
    ),
    // the bitwise rules and the shifts
    ("bit_and", "probeValue(6) & probeValue(3)"),
    ("bit_or", "probeValue(6) | probeValue(3)"),
    ("bit_xor", "probeValue(6) ^ probeValue(3)"),
    ("shift_left", "probeValue(1) << probeValue(4)"),
    ("shift_right", "probeValue(-8) >> probeValue(1)"),
    ("shift_right_unsigned", "probeValue(-8) >>> probeValue(1)"),
    ("shift_real", "probeValue(1.9) << probeValue(1)"),
    ("bit_not_zero", "~probeValue(0)"),
    ("bit_and_negative", "probeValue(-1) & probeValue(255)"),
    ("shift_count_large", "probeValue(1) << probeValue(64)"),
    // the text surface
    ("text_add", r#"probeValue("a") + probeValue("b")"#),
    ("text_add_number", r#"probeValue("a") + probeValue(1)"#),
    ("text_length", r#"probeValue("abc").length"#),
    ("text_length_wide", r#"probeValue("😀").length"#),
    ("text_index", r#"probeValue("abc")[1]"#),
    (
        "text_index_of",
        r#"probeValue("abcabc").indexOf(probeValue("b"), probeValue(2))"#,
    ),
    (
        "text_substring",
        r#"probeValue("abcdef").substring(probeValue(2), probeValue(3))"#,
    ),
    ("text_upper", r#"probeValue("aBc").toUpperCase()"#),
    ("text_char_at", r#"probeValue("abc").charAt(probeValue(1))"#),
    ("text_compare", r#"probeValue("abc") < probeValue("abd")"#),
    (
        "text_sprintf",
        r#"probeValue("%d-%s").sprintf(probeValue(5), probeValue("x"))"#,
    ),
    ("text_trim", r#"probeValue("  a  ").trim()"#),
    // the conversions
    ("convert_integer_text", r#"(int)probeValue("12")"#),
    ("convert_integer_text_spaced", r#"(int)probeValue(" 12 ")"#),
    ("convert_integer_real", "(int)probeValue(3.99)"),
    ("convert_integer_real_negative", "(int)probeValue(-3.99)"),
    ("convert_real_text", r#"(real)probeValue("1.5")"#),
    ("convert_string_integer", "(string)probeValue(42)"),
    ("convert_string_real", "(string)probeValue(1.5)"),
    ("convert_integer_word", r#"(int)probeValue("abc")"#),
    ("convert_real_word", r#"(real)probeValue("abc")"#),
    ("convert_string_boolean", "(string)probeValue(1 == 1)"),
    // typeof and the members it diversifies
    ("typeof_integer", "typeof probeValue(1)"),
    ("typeof_real", "typeof probeValue(1.5)"),
    ("typeof_string", r#"typeof probeValue("a")"#),
    ("typeof_void", r#"typeof (new Dictionary())["x"]"#),
    ("typeof_null", "typeof null"),
    ("typeof_absent_member", "typeof this.absent"),
    (
        "typeof_member_absent_dictionary",
        "typeof probeDictionary().zz",
    ),
    ("typeof_object", "typeof probeDictionary()"),
    ("typeof_class", "typeof Dictionary"),
    ("dictionary_member", "probeDictionary().n"),
    ("dictionary_index", r#"probeDictionary()["s"]"#),
    ("dictionary_member_void", "typeof probeDictionary().v"),
    ("array_element", "probeArray()[1]"),
    ("array_count", "probeArray().count"),
    // the membership questions
    (r#"in_present"#, r#"probeValue("n") in probeDictionary()"#),
    (r#"in_absent"#, r#"probeValue("z") in probeDictionary()"#),
    (
        "instanceof_dictionary",
        "probeDictionary() instanceof Dictionary",
    ),
    (
        "instanceof_array_false",
        "probeDictionary() instanceof Array",
    ),
    ("instanceof_array_true", "probeArray() instanceof Array"),
    (
        "instanceof_integer_false",
        "probeValue(1) instanceof Dictionary",
    ),
    // the arithmetic class
    ("math_abs", "Math.abs(probeValue(-3))"),
    ("math_floor", "Math.floor(probeValue(1.9))"),
    ("math_ceil", "Math.ceil(probeValue(1.1))"),
    ("math_round_half", "Math.round(probeValue(2.5))"),
    (
        "math_max",
        "Math.max(probeValue(1), probeValue(7), probeValue(3))",
    ),
    (
        "math_min",
        "Math.min(probeValue(1), probeValue(7), probeValue(3))",
    ),
    ("math_pow", "Math.pow(probeValue(2), probeValue(10))"),
    ("math_sqrt", "Math.sqrt(probeValue(16))"),
];

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind `typeof` reported.
    kind: String,
    /// The value's text.
    text: String,
}

/// The Rust executor answers what the engine recorded for every expression.
#[test]
fn the_rust_executor_reproduces_the_recorded_answers() {
    let engine = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    let globals = global_members();
    if let Err(failure) =
        run_with_members(engine.clone(), SETUP, "interpreter_setup", globals.clone())
    {
        panic!("the setup does not run in the Rust executor: {failure:?}");
    }
    let records = records();
    assert_eq!(
        records
            .iter()
            .map(|record| record.probe.as_str())
            .collect::<Vec<_>>(),
        PROBES.iter().map(|(probe, _)| *probe).collect::<Vec<_>>(),
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example interpreter_oracle`"
    );
    let mut divergences = Vec::new();
    for record in &records {
        let expression = PROBES
            .iter()
            .find(|(probe, _)| *probe == record.probe)
            .map(|(_, expression)| *expression)
            .unwrap_or_else(|| panic!("the check has no expression for {}", record.probe));
        replay(&engine, &globals, record, expression, &mut divergences);
    }
    assert!(
        divergences.is_empty(),
        "{} probes diverge from the record:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}

/// Runs one recorded probe and compares the kind and the text.
fn replay(
    engine: &Rc<Tjs2Engine>,
    globals: &SharedGlobals,
    record: &Record,
    expression: &str,
    divergences: &mut Vec<String>,
) {
    let script = format!("return probeAnswer({expression});");
    let answer = match run_with_members(
        engine.clone(),
        &script,
        "interpreter_answers",
        globals.clone(),
    ) {
        Ok(answer) => answer,
        Err(failure) => {
            divergences.push(format!(
                "{}: recorded {}|{}, the Rust executor raised {failure:?}",
                record.probe, record.kind, record.text
            ));
            return;
        }
    };
    let Variant::String(text) = answer else {
        divergences.push(format!(
            "{}: recorded {}|{}, the Rust executor answered {answer:?}",
            record.probe, record.kind, record.text
        ));
        return;
    };
    let answer = text.to_string();
    let Some((kind, value)) = answer.split_once('|') else {
        divergences.push(format!(
            "{}: the Rust answer has no kind and text: {text:?}",
            record.probe
        ));
        return;
    };
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2600-2635 — the kind
    // `typeof` reports is the answer's own kind spelling.
    if kind != record.kind {
        divergences.push(format!(
            "{}: recorded kind {}, the Rust kind is {kind}",
            record.probe, record.kind
        ));
    }
    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:405-435 — the value's own
    // text form, which the concatenation in `probeAnswer` asks for.
    if value != record.text {
        divergences.push(format!(
            "{}: recorded text {:?}, the Rust text is {:?}",
            record.probe, record.text, value
        ));
    }
}

/// The records of the fixture; a table with no header or no row is refused.
fn records() -> Vec<Record> {
    let path = fixture_path();
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "the answer fixture {} is not readable: {error}; record it with \
             `cargo run -p tjs --example interpreter_oracle`",
            path.display()
        ),
    };
    let mut lines = table.lines();
    match lines.next() {
        Some(header) if header == HEADER => {}
        Some(header) => panic!(
            "the answer fixture {} does not carry the recorder's header: {header:?}; record it \
             again with `cargo run -p tjs --example interpreter_oracle`",
            path.display()
        ),
        None => panic!(
            "the answer fixture {} is empty; record it with \
             `cargo run -p tjs --example interpreter_oracle`",
            path.display()
        ),
    }
    let mut records = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let Some((probe, rest)) = line.split_once('\t') else {
            panic!("the fixture line is not `probe<TAB>kind<TAB>text`: {line:?}");
        };
        let Some((kind, text)) = rest.split_once('\t') else {
            panic!("the fixture line is not `probe<TAB>kind<TAB>text`: {line:?}");
        };
        assert!(
            !text.contains('\t'),
            "the fixture text holds an unescaped tab: {line:?}"
        );
        records.push(Record {
            probe: probe.to_owned(),
            kind: kind.to_owned(),
            text: unescaped(text),
        });
    }
    assert!(
        !records.is_empty(),
        "the answer fixture records nothing; record it with \
         `cargo run -p tjs --example interpreter_oracle`"
    );
    records
}

/// The fixture the record is written to and this check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/interpreter_answers.txt")
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
