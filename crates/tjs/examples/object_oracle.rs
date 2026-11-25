//! Records what the engine answers for a dictionary's members.
//!
//! A script reaches an object's members through one surface: a read, a write,
//! a delete. This recorder builds a dictionary with a fixed set of writes in
//! both spellings, deletes one of its members, asks `typeof` for present,
//! void, absent and deleted names, and prints one `id<TAB>kind<TAB>text` line
//! per probe; the check in `tests/object_members.rs` replays the same writes
//! through the object model and compares.
//!
//! The record is written to `tests/fixtures/object_members.txt` atomically: the
//! whole table is written to a temporary file beside the fixture and renamed
//! over it, so a failed run never leaves a half record behind.
//!
//! ```sh
//! cargo run -p tjs --example object_oracle
//! ```

use std::fs;
use std::path::PathBuf;

use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/object_oracle.rs`; regenerate with the command in the example's docs";

/// The script that builds the dictionary the probes read.
///
/// The builder is a named function because a script cannot declare one inside
/// an anonymous body, and it is deleted after the call so the global object is
/// left as it was.
const SCRIPT: &str = r#"function objectMembers() {
    var d = new Dictionary();
    d.a = 1;
    d["b"] = 2;
    d.c = 3;
    d.v = void;
    d.n = 7;
    d["a"] = 4;
    d.b = d["a"] + 1;
    delete d.c;
    return d;
}
var d = objectMembers();
delete this.objectMembers;
"#;

/// The probes: the id the check names the rule by, and the expression.
const PROBES: &[(&str, &str)] = &[
    ("dot_read", "d.a"),
    ("bracket_read", "d[\"a\"]"),
    ("dot_read_from_bracket_write", "d.b"),
    ("bracket_read_from_dot_write", "d[\"b\"]"),
    ("created_read", "d.n"),
    ("present_typeof", "typeof (d.a)"),
    ("void_typeof", "typeof (d.v)"),
    ("absent_typeof", "typeof (d.zz)"),
    ("deleted_typeof", "typeof (d.c)"),
    ("created_typeof", "typeof (d.n)"),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SCRIPT, "object_members") {
        fail(format_args!("the engine rejected the script: {error}"));
    }
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, expression) in PROBES {
        let kind = answer(&engine, &format!("typeof ({expression})"), probe);
        let text = answer(&engine, &format!("\"\" + ({expression})"), probe);
        table.push_str(&format!("{probe}\t{kind}\t{text}\n"));
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/object_members.txt")
}

/// The text of one evaluation, which must be a text.
fn answer(engine: &Tjs2Engine, expression: &str, probe: &str) -> String {
    match engine.eval(expression, "object_members") {
        Ok(TjsValue::String(text)) => text,
        Ok(other) => fail(format_args!(
            "probe {probe}: the answer is not a text: {other:?}"
        )),
        Err(error) => fail(format_args!(
            "probe {probe}: the engine rejected the probe: {error}"
        )),
    }
}

/// Reports the failure and exits without writing a record.
fn fail(message: std::fmt::Arguments<'_>) -> ! {
    eprintln!("object_oracle: {message}");
    std::process::exit(1)
}
