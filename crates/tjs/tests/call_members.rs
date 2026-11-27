//! The Rust executor against the engine's recorded member calls.
//!
//! `tests/fixtures/call_members.txt` holds what the engine answered when a
//! script called a member of an array, a function member of a plain object, a
//! non-function member and an absent one; `crates/tjs/examples/call_members_oracle.rs`
//! records it. The check below runs the same setup and probe expressions in the
//! Rust executor (`tjs::vm::runtime::engine_core`) and compares the kind and
//! text of every answer, raises included.
//!
//! A record that holds only the header is a failure, not a pass: the table must
//! be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;

use tjs::vm::runtime::engine_core::EngineCore;
use tjs::vm::runtime::variant::Variant;

/// The objects the probes call into, built the same way as the recorder's.
const SETUP: &str = r#"var a = (const) [1, 2, 3];
a.named = function () { return 7; };
var o = (const) %[];
o.f = function (x) { return x + 1; };
var n = (const) %[];
n.f = 1;
var p = (const) %[];
var d = (const) %[];
"#;

/// The probes the record must hold, in the recorder's order, with the
/// expression each id names.
const PROBES: &[(&str, &str)] = &[
    ("add", "a.add(1)"),
    ("add_read", "a[3]"),
    ("insert", "a.insert(1, 99)"),
    ("insert_read", "a[1]"),
    ("erase", "a.erase(1)"),
    ("erase_read", "a[1]"),
    ("find", "a.find(3)"),
    ("sort_push", "a.push(3, 1, 2)"),
    ("sort", "a.sort()"),
    ("sort_read", "a.join(\",\")"),
    ("count_call", "a.count()"),
    ("reverse", "a.reverse()"),
    ("reverse_read", "a.join(\",\")"),
    ("clear", "a.clear()"),
    ("clear_count", "a.count"),
    ("remove_push", "a.push(4, 5, 4)"),
    ("remove", "a.remove(4, false)"),
    ("remove_read", "a.join(\",\")"),
    ("add_wrong_count", "a.add()"),
    ("insert_wrong_count", "a.insert(0)"),
    ("array_named_call", "a.named()"),
    ("array_named_absent", "a.missing()"),
    ("object_call", "o.f(41)"),
    ("object_not_function", "n.f()"),
    ("object_absent", "p.f()"),
    ("dictionary_instance_member", "d.saveStruct"),
    ("dictionary_instance_call", "d.saveStruct()"),
];

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind `typeof` reports for the answer, or `!raise`.
    kind: String,
    /// The answer as text, escaped.
    text: String,
}

#[test]
fn the_rust_executor_reproduces_the_recorded_member_calls() {
    let records = recorded_answers();
    let core = EngineCore::new();
    if let Err(error) = core.run(SETUP, "call_members_setup") {
        panic!("the setup did not run: {error}");
    }
    let mut seen = Vec::with_capacity(records.len());
    for record in &records {
        let Some((_, expression)) = PROBES.iter().find(|(probe, _)| *probe == record.probe) else {
            panic!(
                "the check has no expression for the recorded probe {}; record it again with \
                 `cargo run -p tjs --example call_members_oracle`",
                record.probe
            );
        };
        let (kind, text) = answer(&core, expression, &record.probe);
        assert_eq!(record.kind, kind, "probe {}: the kind", record.probe);
        assert_eq!(record.text, text, "probe {}: the text", record.probe);
        seen.push(record.probe.as_str());
    }
    let expected: Vec<&str> = PROBES.iter().map(|(probe, _)| *probe).collect();
    assert_eq!(
        seen, expected,
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example call_members_oracle`"
    );
}

/// The kind and text one probe answers through the Rust executor.
fn answer(core: &EngineCore, expression: &str, probe: &str) -> (String, String) {
    let script = wrapped(expression);
    let value = match core.run(&script, probe) {
        Ok(value) => value,
        Err(error) => panic!("probe {probe}: the script failed: {error}"),
    };
    let answer = match value {
        Variant::String(answer) => answer.to_string(),
        other => panic!("probe {probe}: the answer is not a text: {other:?}"),
    };
    match answer.split_once('\t') {
        Some((kind, text)) => (kind.to_string(), text.to_string()),
        None => panic!("probe {probe}: the answer is not `kind<TAB>text`: {answer:?}"),
    }
}

/// The script that evaluates one expression and reports its kind and text.
fn wrapped(expression: &str) -> String {
    format!(
        "try {{ var value = ({expression}); return (typeof value) + \"\\t\" + (\"\" + value); }} \
         catch (e) {{ return \"!raise\\t\" + e.message; }}"
    )
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_answers() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/call_members.txt");
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "the call fixture {} is not readable: {error}; record it with \
             `cargo run -p tjs --example call_members_oracle`",
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
            text: unescaped(text),
        });
    }
    assert!(
        !records.is_empty(),
        "the call fixture records nothing; record it with \
         `cargo run -p tjs --example call_members_oracle`"
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
