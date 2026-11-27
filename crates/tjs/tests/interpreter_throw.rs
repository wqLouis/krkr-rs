//! The throw table against the Rust executor.
//!
//! `tests/fixtures/interpreter_throw.txt` holds 20 scripts whose body raises
//! at run time, and the message the engine's handler read for each;
//! `crates/tjs/examples/interpreter_oracle.rs` records it. Each row carries the
//! script it recorded, so the check replays exactly the script that was run.
//! A body's operands come through the setup's `probeValue` where the constant
//! folder could otherwise answer them, so a raise that must happen while an
//! instruction runs is not folded into a compile failure; `7 \ 0` is the case
//! that made this necessary.
//!
//! The table's first line is the recorder's header; a table without it is
//! refused, because a stale or hand-written fixture is not a record. Every
//! divergence is collected and reported together.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use tjs::Tjs2Engine;
use tjs::vm::runtime::bridge::{SharedGlobals, global_members};
use tjs::vm::runtime::engine::run_with_members;
use tjs::vm::runtime::variant::Variant;

/// The header line the recorder writes; a table without it is not a record.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/interpreter_oracle.rs` (the throw table, each row carrying its script); regenerate with `cargo run -p tjs --example interpreter_oracle`";

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
const PROBES: &[&str] = &[
    "divide_integer_zero",
    "remainder_zero",
    "text_index_past_end",
    "text_index_negative",
    "text_length_write",
    "octet_index_past_end",
    "octet_to_string",
    "octet_to_number",
    "null_member_read",
    "integer_call",
    "absent_global_read",
    "absent_global_typeof",
    "absent_global_call",
    "new_non_class",
    "sprintf_format",
    "sprintf_missing",
    "unpack_format",
    "convert_null_integer",
    "convert_null_real",
    "property_access_integer",
];

/// One recorded script and the message its handler read.
struct Record {
    /// The probe's name.
    probe: String,
    /// The script the recorder ran.
    script: String,
    /// The message `e.message` answered.
    message: String,
}

/// The Rust executor's handler reads what the engine's handler read.
#[test]
fn the_rust_executor_raises_what_the_engine_recorded() {
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
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example interpreter_oracle`"
    );
    let mut divergences = Vec::new();
    for record in &records {
        replay(&engine, &globals, record, &mut divergences);
    }
    assert!(
        divergences.is_empty(),
        "{} probes diverge from the record:\n{}",
        divergences.len(),
        divergences.join("\n")
    );
}

/// Runs one recorded script and compares the message its handler reads.
fn replay(
    engine: &Rc<Tjs2Engine>,
    globals: &SharedGlobals,
    record: &Record,
    divergences: &mut Vec<String>,
) {
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1272-1277 — a
    // script's throw hands the value to the catch unchanged.
    // reference/krkr2/cpp/core/tjs2/tjsError.cpp:26-55 — an engine failure
    // becomes an Exception whose `message` is the raise's text.
    let answer = match run_with_members(
        engine.clone(),
        &record.script,
        "interpreter_throw",
        globals.clone(),
    ) {
        Ok(answer) => answer,
        Err(failure) => {
            divergences.push(format!(
                "{}: recorded {:?}, the Rust executor raised {failure:?}",
                record.probe, record.message
            ));
            return;
        }
    };
    let Variant::String(text) = answer else {
        divergences.push(format!(
            "{}: recorded {:?}, the Rust executor answered {answer:?}",
            record.probe, record.message
        ));
        return;
    };
    let message = text.to_string();
    if message != record.message {
        divergences.push(format!(
            "{}: recorded {:?}, the Rust handler read {:?}",
            record.probe, record.message, message
        ));
    }
}

/// The records of the fixture; a table with no header or no row is refused.
fn records() -> Vec<Record> {
    let path = fixture_path();
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "the throw fixture {} is not readable: {error}; record it with \
             `cargo run -p tjs --example interpreter_oracle`",
            path.display()
        ),
    };
    let mut lines = table.lines();
    match lines.next() {
        Some(header) if header == HEADER => {}
        Some(header) => panic!(
            "the throw fixture {} does not carry the recorder's header: {header:?}; record it \
             again with `cargo run -p tjs --example interpreter_oracle`",
            path.display()
        ),
        None => panic!(
            "the throw fixture {} is empty; record it with \
             `cargo run -p tjs --example interpreter_oracle`",
            path.display()
        ),
    }
    let mut records = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.splitn(3, '\t').collect();
        let [probe, script, message] = fields.as_slice() else {
            panic!("the fixture line is not `probe<TAB>script<TAB>message`: {line:?}");
        };
        records.push(Record {
            probe: (*probe).to_owned(),
            script: unescaped(script),
            message: unescaped(message),
        });
    }
    assert!(
        !records.is_empty(),
        "the throw fixture records nothing; record it with \
         `cargo run -p tjs --example interpreter_oracle`"
    );
    records
}

/// The fixture the record is written to and this check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/interpreter_throw.txt")
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
