//! The `Dictionary` class object against the engine's recorded answers.
//!
//! `tests/fixtures/dictionary_class.txt` holds what the engine answered for the
//! class object's member list and for each of its seven static members: called
//! through `incontextof` with a dictionary, and called directly on the class
//! object. The test below replays the same probes through the Rust executor and
//! compares every answer, so the class object, its statics and the rules behind
//! them are the engine's and not only this tree's idea of them.
//!
//! A record that holds only the header is a failure, not a pass: the table must
//! be recorded before the check means anything. The recorder is the ignored
//! test at the bottom of this file; it runs the same probes on the engine.
//!
//! # The rows this tree cannot replay yet
//!
//! A member that needs a receiver and is called on the class object raises the
//! engine's *Invalid object context*, and `assign(null)` raises *Accessing to
//! nullptr object*. The call surface has no raise that carries either text, so
//! those rows are compared on the record and asserted to raise, not compared on
//! the message. [`DEFERRED`] names them; when a raise that carries a text lands,
//! this check turns them over instead of skipping them.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use tjs::vm::runtime::engine_core::EngineCore;
use tjs::vm::runtime::stream::{self, MemoryStreams, SharedHost};
use tjs::vm::runtime::variant::Variant;
use tjs::{Tjs2Engine, TjsValue};

/// The functions every probe's script can call.
///
/// The builder is a named function because a script cannot declare one inside
/// an anonymous body.
const SETUP: &str = r#"function classMemberNames() {
    var names = (const) [];
    var candidates = (const) ["assign", "assignStruct", "clear", "count", "exists", "finalize", "getList", "load", "loadStruct", "member", "save", "saveStruct"];
    for (var i = 0; i < candidates.length; i = i + 1) {
        if (typeof Dictionary[candidates[i]] != "undefined") { names.add(candidates[i]); }
    }
    names.sort();
    return names.join(",");
}
function classMemberCount() {
    var count = 0;
    var candidates = (const) ["assign", "assignStruct", "clear", "count", "exists", "finalize", "getList", "load", "loadStruct", "member", "save", "saveStruct"];
    for (var i = 0; i < candidates.length; i = i + 1) {
        if (typeof Dictionary[candidates[i]] != "undefined") { count = count + 1; }
    }
    return count;
}
function dictionaryWith(key, value) {
    var d = new Dictionary();
    d[key] = value;
    return d;
}
"#;

/// The probes: the id the check names the rule by, and the script body.
///
/// A body returns a text; the check compares it. The wrapper adds the raise
/// branch, so a body never catches for itself.
const PROBES: &[(&str, &str)] = &[
    (
        "class_members",
        "return \"String\\t\" + classMemberNames();",
    ),
    (
        "class_member_count",
        "return \"Integer\\t\" + classMemberCount();",
    ),
    (
        "class_member_types",
        "return typeof Dictionary.load + \"|\" + typeof Dictionary.loadStruct + \"|\" + \
         typeof Dictionary.save + \"|\" + typeof Dictionary.saveStruct + \"|\" + \
         typeof Dictionary.assign + \"|\" + typeof Dictionary.assignStruct + \"|\" + \
         typeof Dictionary.clear;",
    ),
    (
        "instance_member_types",
        "var d = new Dictionary(); return typeof d.load + \"|\" + typeof d.loadStruct + \"|\" + \
         typeof d.save + \"|\" + typeof d.saveStruct + \"|\" + typeof d.assign + \"|\" + \
         typeof d.assignStruct + \"|\" + typeof d.clear;",
    ),
    (
        "constructor_instance",
        "var d = new Dictionary(); d.key = \"value\"; return typeof d + \"|\" + d.key;",
    ),
    (
        "load_with",
        "var d = dictionaryWith(\"a\", 1); var v = (Dictionary.load incontextof d)(); \
         return typeof v + \"|\" + (\"\" + v) + \"|\" + (\"\" + d.a);",
    ),
    (
        "save_with",
        "var d = dictionaryWith(\"a\", 1); var v = (Dictionary.save incontextof d)(); \
         return typeof v + \"|\" + (\"\" + v) + \"|\" + (\"\" + d.a);",
    ),
    (
        "clear_with",
        "var d = dictionaryWith(\"a\", 1); var v = (Dictionary.clear incontextof d)(); \
         return typeof v + \"|\" + (\"\" + v) + \"|\" + typeof d.a;",
    ),
    (
        "assign_with",
        "var d = dictionaryWith(\"a\", 1); var s = dictionaryWith(\"b\", 2); \
         var v = (Dictionary.assign incontextof d)(s); \
         return typeof v + \"|\" + (\"\" + v) + \"|\" + (\"\" + d.a) + \"|\" + (\"\" + d.b);",
    ),
    (
        "assign_keep",
        "var d = dictionaryWith(\"a\", 1); var s = dictionaryWith(\"b\", 2); \
         var v = (Dictionary.assign incontextof d)(s, false); \
         return typeof v + \"|\" + (\"\" + d.a) + \"|\" + (\"\" + d.b);",
    ),
    (
        "assign_array",
        "var d = new Dictionary(); var v = (Dictionary.assign incontextof d)((const) [\"x\", 5, \"y\", 6]); \
         return (\"\" + d.x) + \"|\" + (\"\" + d.y);",
    ),
    (
        "assign_null",
        "var d = dictionaryWith(\"a\", 1); var v = (Dictionary.assign incontextof d)(null); \
         return \"\" + v;",
    ),
    (
        "assign_struct_with",
        "var d = dictionaryWith(\"a\", 1); var s = dictionaryWith(\"b\", 2); \
         var v = (Dictionary.assignStruct incontextof d)(s); \
         return typeof v + \"|\" + (\"\" + v) + \"|\" + (\"\" + d.a) + \"|\" + (\"\" + d.b);",
    ),
    (
        "save_struct_with",
        "var d = dictionaryWith(\"a\", 1); \
         var v = (Dictionary.saveStruct incontextof d)(path, \"b\"); \
         return typeof v + \"|\" + (v == d) + \"|\" + (\"\" + v.a);",
    ),
    (
        "load_struct_with",
        "var source = dictionaryWith(\"a\", 1); \
         (Dictionary.saveStruct incontextof source)(path, \"b\"); \
         var d = dictionaryWith(\"old\", 9); \
         var v = (Dictionary.loadStruct incontextof d)(path); \
         return typeof v + \"|\" + (v == d) + \"|\" + (\"\" + d.a) + \"|\" + typeof d.old;",
    ),
    (
        "load_struct_without",
        "var source = dictionaryWith(\"a\", 1); \
         (Dictionary.saveStruct incontextof source)(path, \"b\"); \
         var v = Dictionary.loadStruct(path); \
         return typeof v + \"|\" + (\"\" + v.a);",
    ),
    (
        "load_without",
        "var d = dictionaryWith(\"a\", 1); var v = Dictionary.load(); \
         return typeof v + \"|\" + (\"\" + d.a);",
    ),
    (
        "save_without",
        "var d = dictionaryWith(\"a\", 1); var v = Dictionary.save(); \
         return typeof v + \"|\" + (\"\" + d.a);",
    ),
    (
        "clear_without",
        "var d = dictionaryWith(\"a\", 1); var v = Dictionary.clear(); \
         return typeof v + \"|\" + (\"\" + d.a);",
    ),
    (
        "assign_without",
        "var d = dictionaryWith(\"a\", 1); var s = dictionaryWith(\"b\", 2); \
         var v = Dictionary.assign(s); \
         return typeof v + \"|\" + (\"\" + d.a);",
    ),
    (
        "assign_struct_without",
        "var d = dictionaryWith(\"a\", 1); var s = dictionaryWith(\"b\", 2); \
         var v = Dictionary.assignStruct(s); \
         return typeof v + \"|\" + (\"\" + d.a);",
    ),
    (
        "save_struct_without",
        "var d = dictionaryWith(\"a\", 1); var v = Dictionary.saveStruct(path, \"b\"); \
         return (\"\" + v) + \"|\" + (\"\" + d.a);",
    ),
];

/// The probes whose failure text the call surface does not carry yet.
///
/// The recorded answer is the engine's; the replay asserts the raise and the
/// record, not the message.
const DEFERRED: &[&str] = &[
    "assign_null",
    "load_without",
    "save_without",
    "clear_without",
    "assign_without",
    "assign_struct_without",
    "save_struct_without",
];

/// The VM keeps the installed stream host and the engine is single-threaded;
/// one check at a time.
static VM_LOCK: Mutex<()> = Mutex::new(());

/// The lock that serializes this file's checks.
fn vm_lock() -> MutexGuard<'static, ()> {
    match VM_LOCK.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// The host the check installs, restored when the check ends.
struct Host {
    /// The stream host the installation replaced.
    previous: Option<SharedHost>,
}

impl Host {
    /// Installs a store the check's saves and loads use.
    fn install() -> Self {
        let streams: SharedHost = Arc::new(MemoryStreams::new());
        let previous = stream::install(streams);
        Self { previous }
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        stream::uninstall();
        if let Some(previous) = self.previous.take() {
            stream::install(previous);
        }
    }
}

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind the answer reported, or `!raise`.
    kind: String,
    /// The answer as text.
    text: String,
}

#[test]
fn the_dictionary_class_reproduces_the_recorded_surface() {
    let _lock = vm_lock();
    let _host = Host::install();
    let records = recorded_answers();
    let mut seen = Vec::with_capacity(records.len());
    for record in &records {
        seen.push(record.probe.as_str());
        let (kind, text) = rust_answer(&record.probe);
        assert_eq!(record.kind, kind, "probe {}: the kind", record.probe);
        if DEFERRED.contains(&record.probe.as_str()) {
            assert_eq!(kind, "!raise", "deferred probe {}: the kind", record.probe);
            continue;
        }
        assert_eq!(record.text, text, "probe {}: the text", record.probe);
    }
    let expected: Vec<&str> = PROBES.iter().map(|(probe, _)| *probe).collect();
    assert_eq!(
        seen, expected,
        "the record's probes are not the table this check knows; record it again with \
         `cargo test -p tjs --test dictionary_class -- --ignored`"
    );
}

/// A dictionary instance publishes no class member, and the class members are
/// static on the class object.
///
/// reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:30-255 — every member is
/// registered with `TJS_END_NATIVE_STATIC_METHOD_DECL`;
/// reference/krkr2/cpp/core/tjs2/tjsNative.cpp:323, :411 — an instance copies
/// only the members that are not static.
#[test]
fn a_dictionary_instance_publishes_no_class_member() {
    let core = EngineCore::new();
    let answer = match core.run(
        "var d = new Dictionary(); d.key = 1; return typeof d.saveStruct;",
        "dictionary_class",
    ) {
        Ok(Variant::String(text)) => text.to_string(),
        Ok(other) => panic!("the answer is not a text: {other:?}"),
        Err(error) => panic!("the script did not run: {error}"),
    };
    assert_eq!(answer, "undefined");

    let answer = match core.run(
        "try { var d = new Dictionary(); d.saveStruct(); } catch (e) { return e.message; }",
        "dictionary_class",
    ) {
        Ok(Variant::String(text)) => text.to_string(),
        Ok(other) => panic!("the answer is not a text: {other:?}"),
        Err(error) => panic!("the script did not run: {error}"),
    };
    assert_eq!(answer, "Member \"saveStruct\" does not exist");
}

/// Replays one probe through the Rust executor and answers its kind and text.
fn rust_answer(probe: &str) -> (String, String) {
    let Some((_, body)) = PROBES.iter().find(|(name, _)| *name == probe) else {
        panic!("the check has no script for the recorded probe {probe}");
    };
    let core = EngineCore::new();
    let path = format!("dictionary-class-{probe}.bin");
    let setup = format!("{SETUP}\nvar path = {path:?};\n");
    if let Err(error) = core.run(&setup, "dictionary_class_setup") {
        panic!("probe {probe}: the setup did not run: {error}");
    }
    let script = format!("try {{ {body} }} catch (e) {{ return \"!raise\\t\" + e.message; }}");
    let answer = match core.run(&script, probe) {
        Ok(Variant::String(answer)) => answer.to_string(),
        Ok(other) => panic!("probe {probe}: the answer is not a text: {other:?}"),
        Err(error) => panic!("probe {probe}: the script did not run: {error}"),
    };
    match answer.split_once('\t') {
        Some((kind, text)) => (kind.to_string(), text.to_string()),
        None => (String::from("String"), answer),
    }
}

/// The fixture the check reads; a record with only the header is a failure.
fn recorded_answers() -> Vec<Record> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dictionary_class.txt");
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "read {}: {error}; record it with \
             `cargo test -p tjs --test dictionary_class -- --ignored`",
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
    assert!(!records.is_empty(), "the record holds no answer");
    records
}

/// Records the engine's answers to the fixture.
///
/// The recorder runs on the engine, one fresh VM per probe so a member call
/// that aborts the VM cannot hide the probes after it. It is deliberately not
/// part of the ordinary check: the record changes only when the surface does.
#[test]
#[ignore = "records tests/fixtures/dictionary_class.txt on the engine"]
fn record_the_engine_answers() {
    let mut table = String::new();
    table.push_str(
        "# the engine's answers, recorded by the ignored `record_the_engine_answers` test in \
         `crates/tjs/tests/dictionary_class.rs`; regenerate with \
         `cargo test -p tjs --test dictionary_class -- --ignored`\n",
    );
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is readable")
        .as_nanos();
    for &(probe, body) in PROBES {
        let path = std::env::temp_dir().join(format!(
            "tjs-dictionary-class-{}-{nanos}-{probe}.bin",
            std::process::id()
        ));
        let setup = format!("{SETUP}\nvar path = {:?};\n", path.to_string_lossy());
        let engine = Tjs2Engine::new().expect("the engine starts");
        if let Err(error) = engine.exec_script_discard(&setup, "dictionary_class_setup") {
            panic!("probe {probe}: the setup was rejected: {error}");
        }
        let script = format!("try {{ {body} }} catch (e) {{ return \"!raise\\t\" + e.message; }}");
        let answer = match engine.exec_script(&script, probe) {
            Ok(TjsValue::String(answer)) => answer,
            Ok(other) => panic!("probe {probe}: the answer is not a text: {other:?}"),
            Err(error) => panic!("probe {probe}: the script failed: {error}"),
        };
        let (kind, text) = answer
            .split_once('\t')
            .unwrap_or(("String", answer.as_str()));
        table.push_str(&format!("{probe}\t{kind}\t{}\n", escaped(text)));
        let _ = fs::remove_file(&path);
    }
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dictionary_class.txt");
    let temporary = path.with_extension("txt.tmp");
    fs::write(&temporary, table).expect("the temporary record is writable");
    fs::rename(&temporary, &path).expect("the record can be renamed");
    println!("wrote {}", path.display());
}

/// The text escaped so that one probe stays on one line.
fn escaped(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        match character {
            '\\' => escaped.push_str("\\\\"),
            '\r' => escaped.push_str("\\r"),
            '\n' => escaped.push_str("\\n"),
            '\t' => escaped.push_str("\\t"),
            other => escaped.push(other),
        }
    }
    escaped
}
