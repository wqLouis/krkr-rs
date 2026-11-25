//! The object model against the engine's recorded member answers.
//!
//! `tests/fixtures/object_members.txt` holds what the engine answered for a
//! dictionary's members; `crates/tjs/examples/object_oracle.rs` records it.
//! The check below replays the same writes through the object model
//! (`tjs::vm::runtime::object`) and compares the kind and the text of every
//! answer, so the model's member rule is the engine's and not only its own
//! idea of one.
//!
//! A record that holds only the header is a failure, not a pass: the table
//! must be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;

use tjs::vm::runtime::object::{CustomObject, Dispatch, MemberFlags, ObjectError};
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::Variant;

/// The probes the record must hold, in the recorder's order.
const PROBES: &[&str] = &[
    "dot_read",
    "bracket_read",
    "dot_read_from_bracket_write",
    "bracket_read_from_dot_write",
    "created_read",
    "present_typeof",
    "void_typeof",
    "absent_typeof",
    "deleted_typeof",
    "created_typeof",
];

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind `typeof` reports for the answer.
    kind: String,
    /// The answer as text.
    text: String,
}

#[test]
fn the_object_model_reproduces_the_recorded_member_answers() {
    let records = recorded_answers();
    let object = replayed();
    let mut seen = Vec::with_capacity(records.len());
    for record in &records {
        let (kind, text) = answer(&object, &record.probe);
        assert_eq!(record.kind, kind, "probe {}: the kind", record.probe);
        assert_eq!(record.text, text, "probe {}: the text", record.probe);
        seen.push(record.probe.as_str());
    }
    assert_eq!(
        seen.as_slice(),
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example object_oracle`"
    );
}

/// The dictionary the recorder's script builds, replayed through the object
/// model.
///
/// The writes are both spellings of the same member surface; a dictionary
/// reaches the same members through `d.a` and `d["a"]`, so the replay uses the
/// one write call for both.
fn replayed() -> CustomObject {
    let object = CustomObject::new(Vec::new());
    let stored = [
        ("a", Variant::from(1i64)),
        ("b", Variant::from(2i64)),
        ("c", Variant::from(3i64)),
        ("v", Variant::Void),
        ("n", Variant::from(7i64)),
        ("a", Variant::from(4i64)),
        ("b", Variant::from(5i64)),
    ];
    for (name, value) in stored {
        object
            .prop_set(&text(name), MemberFlags::ENSURE, &value, None)
            .expect("the write is served");
    }
    object
        .delete_member(&text("c"), MemberFlags::NONE, None)
        .expect("the delete is served");
    object
}

/// The answer the object model gives for one probe.
fn answer(object: &CustomObject, probe: &str) -> (String, String) {
    match probe {
        // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1276-1311
        "dot_read" | "bracket_read" => read(object, "a"),
        "dot_read_from_bracket_write" => read(object, "b"),
        "bracket_read_from_dot_write" => read(object, "b"),
        // an assignment creates the member: :1352-1398
        "created_read" => read(object, "n"),
        // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2602-2638 — the
        // kind `typeof` names for a member the object holds
        "present_typeof" | "void_typeof" | "absent_typeof" | "deleted_typeof"
        | "created_typeof" => kind(object, &typeof_target(probe)),
        other => panic!("the check has no answer for the recorded probe: {other}"),
    }
}

/// The member a `typeof` probe reads.
fn typeof_target(probe: &str) -> String {
    match probe {
        "present_typeof" => String::from("a"),
        "void_typeof" => String::from("v"),
        "absent_typeof" => String::from("zz"),
        "deleted_typeof" => String::from("c"),
        "created_typeof" => String::from("n"),
        other => panic!("the check has no target for the recorded probe: {other}"),
    }
}

/// The kind and the text of a member read.
fn read(object: &CustomObject, name: &str) -> (String, String) {
    let value = object
        .prop_get(&text(name), MemberFlags::IGNORE_PROPERTY, None)
        .expect("the member is present");
    let text = value.to_string().expect("the value has a text").to_string();
    (value.type_name().to_owned(), text)
}

/// The kind and the text of a `typeof` answer.
///
/// `typeof` reports a text, so the kind of the answer is `String` and the text
/// is the kind the member read answers. The three states stay apart: a present
/// member holding the empty variant answers `void`, a present member holding a
/// value answers that value's kind, and a member the object does not hold is a
/// failure and answers `undefined` — never `void`.
fn kind(object: &CustomObject, name: &str) -> (String, String) {
    match object.prop_get(&text(name), MemberFlags::IGNORE_PROPERTY, None) {
        // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1065-1093
        Ok(Variant::Void) => (String::from("String"), String::from("void")),
        Ok(value) => (String::from("String"), value.type_name().to_owned()),
        // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1307-1308
        Err(ObjectError::MemberNotFound) => (String::from("String"), String::from("undefined")),
        Err(error) => panic!("probe {name}: the read failed: {error}"),
    }
}

/// A name.
fn text(name: &str) -> TjsString {
    TjsString::from(name)
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_answers() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/object_members.txt");
    let table = fs::read_to_string(&path).expect("read the object member fixture");
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
        "the object member fixture records nothing; record it with \
         `cargo run -p tjs --example object_oracle`"
    );
    records
}
