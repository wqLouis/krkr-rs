//! Records what the engine answers for a dictionary's members and its save.
//!
//! A script reaches a dictionary's members through one surface: a read, a
//! write, a delete. This recorder builds a dictionary with a fixed set of
//! writes, asks `typeof` and the text form for present, empty, null, created
//! and deleted names, asks the class questions, and writes two structured
//! saves to a temporary file and reads the text back. It writes one
//! `id<TAB>kind<TAB>text` line per probe; the check in
//! `tests/dictionary_members.rs` replays the same writes through the
//! dictionary object and compares.
//!
//! A save's text is carried on one line: a backslash is doubled, and a
//! carriage return, a line feed and a tab are written as `\r`, `\n` and `\t`.
//!
//! A dictionary's own save member belongs to its class, not to the dictionary
//! a script holds, so each save is written as the only item of an array; the
//! array's one indentation step is removed from the text, and a record holds
//! the dictionary's own structure.
//!
//! The record is written to `tests/fixtures/dictionary_members.txt`
//! atomically: the whole table is written to a temporary file beside the
//! fixture and renamed over it, so a failed run never leaves a half record
//! behind.
//!
//! ```sh
//! cargo run -p tjs --example dictionary_oracle
//! ```

use std::fs;
use std::path::{Path, PathBuf};

use tjs::{Tjs2Engine, TjsValue};

/// The fixture's first line, so that regenerating the record keeps the file
/// self-describing.
const HEADER: &str = "# the engine's answers, recorded by `crates/tjs/examples/dictionary_oracle.rs`; regenerate with the command in the example's docs";

/// The script that builds the two dictionaries the probes read.
///
/// The builders are named functions because a script cannot declare one
/// inside an anonymous body, and they are deleted after the call so the global
/// object is left as it was.
const SCRIPT: &str = r#"function dictionarySaved() {
    var d = new Dictionary();
    d["void"] = void;
    d.integer = -42;
    d.big = 1234567890123;
    d.real = 3.5;
    d.tiny = 0.0001;
    d.zero = -0.0;
    d.nan = 0.0 / 0.0;
    d.inf = 1e400;
    d.text = "quote \" slash \\ tab \t ctrl \x01" + "a 日本語";
    d.octet = <% 00 AB FF %>;
    d.nothing = null;
    var n = new Dictionary();
    n.inner = 1;
    n.deep = new Dictionary();
    n.deep.x = "y";
    d.nested = n;
    d.self = d;
    return d;
}
function dictionaryMembers() {
    var m = new Dictionary();
    m.v = void;
    m.nothing = null;
    m.created = 7;
    m.deleted = 1;
    delete m.deleted;
    return m;
}
var d = dictionarySaved();
var m = dictionaryMembers();
delete this.dictionarySaved;
delete this.dictionaryMembers;
var empty = new Dictionary();
"#;

/// The probes: the id the check names the rule by, and the expression.
const PROBES: &[(&str, &str)] = &[
    ("absent_typeof", "typeof (m.absent)"),
    ("absent_text", "\"\" + (m.absent)"),
    ("void_typeof", "typeof (m.v)"),
    ("void_text", "\"\" + (m.v)"),
    ("null_typeof", "typeof (m.nothing)"),
    ("created_text", "\"\" + (m.created)"),
    ("deleted_text", "\"\" + (m.deleted)"),
    ("own_class", "\"\" + (m instanceof \"Dictionary\")"),
    ("object_class", "\"\" + (m instanceof \"Object\")"),
    // the class names a dictionary answers, and the class object's text
    ("array_class", "\"\" + (m instanceof \"Array\")"),
    ("class_object", "\"\" + (m instanceof Dictionary)"),
    ("string_class", "\"\" + (m instanceof \"String\")"),
    ("number_class", "\"\" + (m instanceof \"Number\")"),
    ("octet_class", "\"\" + (m instanceof \"Octet\")"),
    // the class's static members, and the names that are not members at all
    ("instance_save_struct", "typeof (m.saveStruct)"),
    ("instance_load_struct", "typeof (m.loadStruct)"),
    ("instance_clear", "typeof (m.clear)"),
    ("instance_assign", "typeof (m.assign)"),
    ("instance_assign_struct", "typeof (m.assignStruct)"),
    ("instance_count", "typeof (m.count)"),
    ("instance_member", "typeof (m.member)"),
    ("instance_exists", "typeof (m.exists)"),
    ("class_save_struct", "typeof (Dictionary.saveStruct)"),
    ("class_load_struct", "typeof (Dictionary.loadStruct)"),
    ("class_clear", "typeof (Dictionary.clear)"),
    ("class_assign", "typeof (Dictionary.assign)"),
    ("class_assign_struct", "typeof (Dictionary.assignStruct)"),
    ("class_count", "typeof (Dictionary.count)"),
];

fn main() {
    let engine = match Tjs2Engine::new() {
        Ok(engine) => engine,
        Err(error) => fail(format_args!("the engine did not start: {error}")),
    };
    if let Err(error) = engine.exec_script_discard(SCRIPT, "dictionary_members") {
        fail(format_args!("the engine rejected the script: {error}"));
    }
    let mut table = String::new();
    table.push_str(HEADER);
    table.push('\n');
    for &(probe, expression) in PROBES {
        let kind = answer(&engine, &format!("typeof ({expression})"), probe);
        let text = answer(&engine, &format!("\"\" + ({expression})"), probe);
        table.push_str(&format!("{probe}\t{kind}\t{}\n", encode(&text)));
    }
    let saved = save(&engine, "d", "saved");
    let empty = save(&engine, "empty", "empty");
    table.push_str(&format!("save_struct\tsave\t{}\n", encode(&saved)));
    table.push_str(&format!("save_empty\tsave\t{}\n", encode(&empty)));
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

/// Writes one dictionary's structure to a temporary file and answers the text
/// the file holds.
///
/// A dictionary's save member belongs to its class, not to the dictionary a
/// script holds, so the dictionary is written as the only item of an array:
/// the array's save reaches it, and writes the item one indentation step
/// deeper than the array's own frame. That one step is removed here, so the
/// answer is the dictionary's own structure.
fn save(engine: &Tjs2Engine, object: &str, name: &str) -> String {
    let path = temporary_path(name);
    let script = format!(
        "var {name}_frame = [{object}]; {name}_frame.saveStruct({:?}, \"\");",
        path.to_string_lossy()
    );
    if let Err(error) = engine.exec_script_discard(&script, "dictionary_save") {
        fail(format_args!(
            "the engine rejected the save of {object}: {error}"
        ));
    }
    let framed = read_text(&path);
    if let Err(error) = fs::remove_file(&path) {
        fail(format_args!(
            "the save file {} is not removable: {error}",
            path.display()
        ));
    }
    unframe(&framed)
}

/// The dictionary text inside a one-item array's structure.
fn unframe(framed: &str) -> String {
    let body = match framed.strip_prefix("(const) [\r\n ") {
        Some(body) => body,
        None => fail(format_args!("the save is not a one-item array: {framed:?}")),
    };
    let body = match body.strip_suffix("\r\n]") {
        Some(body) => body,
        None => fail(format_args!("the save is not a one-item array: {framed:?}")),
    };
    let mut text = String::with_capacity(body.len());
    for (index, line) in body.split("\r\n").enumerate() {
        if index == 0 {
            text.push_str(line);
            continue;
        }
        let Some(rest) = line.strip_prefix(' ') else {
            fail(format_args!(
                "the save has no array indentation to remove: {line:?}"
            ));
        };
        text.push_str("\r\n");
        text.push_str(rest);
    }
    text
}

/// The text of one save file: a byte-order mark and UTF-16 code units.
fn read_text(path: &Path) -> String {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) => fail(format_args!("{} is not readable: {error}", path.display())),
    };
    if bytes.len() < 2 || bytes[0] != 0xff || bytes[1] != 0xfe {
        fail(format_args!(
            "{} is not the text stream a save writes",
            path.display()
        ));
    }
    let payload = &bytes[2..];
    if !payload.len().is_multiple_of(2) {
        fail(format_args!(
            "{} holds half a code unit at its end",
            path.display()
        ));
    }
    let mut units = Vec::with_capacity(payload.len() / 2);
    for pair in payload.as_chunks::<2>().0 {
        units.push(u16::from_le_bytes(*pair));
    }
    match String::from_utf16(&units) {
        Ok(text) => text,
        Err(error) => fail(format_args!("{} is not UTF-16: {error}", path.display())),
    }
}

/// A name no other run of the recorder is using.
fn temporary_path(name: &str) -> PathBuf {
    let nanos = match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(elapsed) => elapsed.as_nanos(),
        Err(error) => fail(format_args!("the clock is not readable: {error}")),
    };
    std::env::temp_dir().join(format!(
        "tjs-dictionary-save-{}-{nanos}-{name}.txt",
        std::process::id()
    ))
}

/// The fixture the record is written to and the check reads.
fn fixture_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dictionary_members.txt")
}

/// The text of one evaluation, which must be a text.
fn answer(engine: &Tjs2Engine, expression: &str, probe: &str) -> String {
    match engine.eval(expression, "dictionary_members") {
        Ok(TjsValue::String(text)) => text,
        Ok(other) => fail(format_args!(
            "probe {probe}: the answer is not a text: {other:?}"
        )),
        Err(error) => fail(format_args!(
            "probe {probe}: the engine rejected the probe: {error}"
        )),
    }
}

/// The record's one-line form of a text.
fn encode(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => encoded.push_str("\\\\"),
            '\r' => encoded.push_str("\\r"),
            '\n' => encoded.push_str("\\n"),
            '\t' => encoded.push_str("\\t"),
            other => encoded.push(other),
        }
    }
    encoded
}

/// Reports the failure and exits without writing a record.
fn fail(message: std::fmt::Arguments<'_>) -> ! {
    eprintln!("dictionary_oracle: {message}");
    std::process::exit(1)
}
