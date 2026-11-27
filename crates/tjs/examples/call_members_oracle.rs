//! Records what the engine answers when a member is called.
//!
//! An object or an array answers a named call through its members: the name is
//! looked up, and the value it reached is called with no name of its own. The
//! array also serves the names its class publishes, from its own table. This
//! recorder builds one array and three objects, then evaluates a probe table
//! against them in order; every probe is wrapped so a raise is recorded as its
//! own answer, with the message a script reads from `e.message`.
//!
//! The record is written to `tests/fixtures/call_members.txt` atomically: the
//! whole table is written beside the fixture and renamed over it, so a failed
//! run leaves the old record in place.
//!
//! ```sh
//! cargo run -p tjs --example call_members_oracle
//! ```

use std::fs;
use std::path::PathBuf;

use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/call_members_oracle.rs`; regenerate with the command in the example's docs";

/// The objects the probes call into.
///
/// The builder uses the constant forms so the same setup runs in the Rust
/// executor, which does not publish the container classes on its global. The
/// array carries a named function member as well, which is the base object's
/// fallback rather than a class member.
const SETUP: &str = r#"var a = (const) [1, 2, 3];
a.named = function () { return 7; };
var o = (const) %[];
o.f = function (x) { return x + 1; };
var n = (const) %[];
n.f = 1;
var p = (const) %[];
var d = (const) %[];
"#;

/// The probes: the id the check names the rule by, and the expression.
///
/// The probes run in this order and share the objects, so an expression that
/// writes is a change the probes after it see.
const PROBES: &[(&str, &str)] = &[
    // the class's own methods
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
    // a name the array's table does not serve is the base object's
    ("array_named_call", "a.named()"),
    ("array_named_absent", "a.missing()"),
    // a plain object's member: a function, a value of another kind, and none
    ("object_call", "o.f(41)"),
    ("object_not_function", "n.f()"),
    ("object_absent", "p.f()"),
    // a dictionary's class API is static, so an instance holds none of it
    ("dictionary_instance_member", "d.saveStruct"),
    ("dictionary_instance_call", "d.saveStruct()"),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SETUP, "call_members") {
        fail(format_args!("the engine rejected the setup: {error}"));
    }
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, expression) in PROBES {
        let (kind, text) = answer(&engine, expression, probe);
        table.push_str(&format!("{probe}\t{kind}\t{}\n", escaped(&text)));
    }
    let path = fixture_path();
    let temporary = path.with_extension("txt.tmp");
    if let Err(error) = fs::write(&temporary, table) {
        fail(format_args!(
            "the temporary record {} is not writable: {error}",
            temporary.display()
        ));
    }
    if let Err(error) = fs::rename(&temporary, &path) {
        fail(format_args!(
            "the record could not be renamed to {}: {error}",
            path.display()
        ));
    }
    println!("wrote {}", path.display());
}

/// The fixture the record is written to and the check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/call_members.txt")
}

/// The kind and text one probe answers.
fn answer(engine: &Tjs2Engine, expression: &str, probe: &str) -> (String, String) {
    let script = wrapped(expression);
    let answer = match engine.exec_script(&script, probe) {
        Ok(TjsValue::String(answer)) => answer,
        Ok(other) => fail(format_args!(
            "probe {probe}: the answer is not a text: {other:?}"
        )),
        Err(error) => fail(format_args!("probe {probe}: the script failed: {error}")),
    };
    match answer.split_once('\t') {
        Some((kind, text)) => (kind.to_string(), text.to_string()),
        None => fail(format_args!(
            "probe {probe}: the answer is not `kind<TAB>text`: {answer:?}"
        )),
    }
}

/// The script that evaluates one expression and reports its kind and text.
///
/// A raise is an answer too: the catch reports the message a script reads, so
/// the record pins what the engine says rather than only that it refused.
fn wrapped(expression: &str) -> String {
    format!(
        "try {{ var value = ({expression}); return (typeof value) + \"\\t\" + (\"\" + value); }} \
         catch (e) {{ return \"!raise\\t\" + e.message; }}"
    )
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

/// Reports the failure and exits without writing a record.
fn fail(message: std::fmt::Arguments<'_>) -> ! {
    eprintln!("call_members_oracle: {message}");
    std::process::exit(1)
}
