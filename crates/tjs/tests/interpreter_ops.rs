//! The ops table against the Rust executor.
//!
//! `tests/fixtures/interpreter_ops.txt` holds 81 scripts and what the engine
//! answered for each; `crates/tjs/examples/interpreter_oracle.rs` records it.
//! Each row carries the script it recorded, so the check replays exactly the
//! script that was run rather than a copy that could drift from it. The setup
//! script runs first in the Rust executor, as the recorder ran it in the
//! engine, and every script runs in the same scope, in the record's order.
//!
//! The scripts cover the frame and constant instructions, the condition flag
//! and the jumps, the step instructions, the operators in their plain and
//! member forms, the unary instructions, calls, members, the protected region
//! and the context instructions. The answer is the kind and the text the
//! setup's `probeAnswer` joins.
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
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/interpreter_oracle.rs` (the ops table, each row carrying its script); regenerate with `cargo run -p tjs --example interpreter_oracle`";

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
    "constant_integer",
    "invalidate_and_isvalid",
    "ignore_property",
    "constant_copies",
    "constant_cleared_in_loop",
    "if_true",
    "if_false",
    "if_else",
    "if_not",
    "while_count",
    "do_while_count",
    "for_count",
    "break_in_loop",
    "continue_in_loop",
    "condition_inverted_or",
    "condition_equal",
    "condition_strict_equal",
    "comparison_value",
    "negated_comparison_value",
    "increment_local",
    "decrement_local",
    "increment_prefix",
    "decrement_prefix",
    "increment_direct_member",
    "decrement_direct_member",
    "increment_index_member",
    "decrement_index_member",
    "add",
    "subtract",
    "multiply",
    "divide",
    "integer_divide",
    "remainder",
    "bit_or",
    "bit_xor",
    "bit_and",
    "shift_right",
    "shift_left",
    "shift_right_unsigned",
    "logical_or",
    "logical_and",
    "add_direct_member",
    "add_index_member",
    "compound_add_local",
    "compound_add_direct_member",
    "compound_add_index_member",
    "compound_multiply_index_member",
    "text_concat",
    "typeof_value",
    "typeof_direct_member",
    "typeof_index_member",
    "number_of_text",
    "negate_text",
    "character_from",
    "convert_integer",
    "convert_real",
    "convert_string",
    "evaluate_expression",
    "evaluate_statement",
    "call_declared_function",
    "call_direct_member",
    "call_index_member",
    "construct_array",
    "this_member_write",
    "call_three_arguments",
    "member_read_direct",
    "member_write_direct",
    "member_read_index",
    "member_write_index",
    "member_write_new_direct",
    "member_write_new_index",
    "member_delete_direct",
    "member_delete_index",
    "member_text_length",
    "member_text_index",
    "try_catch_script_throw",
    "try_catch_engine_raise",
    "try_catch_continue",
    "in_context_of",
    "global_read",
    "debugger_statement",
];

/// One recorded script and its answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The script the recorder ran.
    script: String,
    /// The kind `typeof` reported.
    kind: String,
    /// The value's text.
    text: String,
}

/// The Rust executor answers what the engine recorded for every script.
#[test]
fn the_rust_executor_reproduces_the_recorded_ops() {
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

/// Runs one recorded script and compares the kind and the text.
fn replay(
    engine: &Rc<Tjs2Engine>,
    globals: &SharedGlobals,
    record: &Record,
    divergences: &mut Vec<String>,
) {
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:869-1300 — one
    // instruction dispatch, and `SRV` stores the answer the top-level `RET`
    // hands back.
    let answer = match run_with_members(
        engine.clone(),
        &record.script,
        "interpreter_ops",
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
            "the ops fixture {} is not readable: {error}; record it with \
             `cargo run -p tjs --example interpreter_oracle`",
            path.display()
        ),
    };
    let mut lines = table.lines();
    match lines.next() {
        Some(header) if header == HEADER => {}
        Some(header) => panic!(
            "the ops fixture {} does not carry the recorder's header: {header:?}; record it \
             again with `cargo run -p tjs --example interpreter_oracle`",
            path.display()
        ),
        None => panic!(
            "the ops fixture {} is empty; record it with \
             `cargo run -p tjs --example interpreter_oracle`",
            path.display()
        ),
    }
    let mut records = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.splitn(4, '\t').collect();
        let [probe, script, kind, text] = fields.as_slice() else {
            panic!("the fixture line is not `probe<TAB>script<TAB>kind<TAB>text`: {line:?}");
        };
        assert!(
            !text.contains('\t'),
            "the fixture text holds an unescaped tab: {line:?}"
        );
        records.push(Record {
            probe: (*probe).to_owned(),
            script: unescaped(script),
            kind: (*kind).to_owned(),
            text: unescaped(text),
        });
    }
    assert!(
        !records.is_empty(),
        "the ops fixture records nothing; record it with \
         `cargo run -p tjs --example interpreter_oracle`"
    );
    records
}

/// The fixture the record is written to and this check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/interpreter_ops.txt")
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
