//! The dictionary object against the engine's recorded answers.
//!
//! `tests/fixtures/dictionary_members.txt` holds what the engine answered for a
//! dictionary's members and for two structured saves;
//! `crates/tjs/examples/dictionary_oracle.rs` records it. The check below
//! replays the same writes through `tjs::vm::runtime::dictionary` and compares
//! every answer, so the dictionary's member policy, its save text and the order
//! that text walks are the engine's and not only this module's idea of them.
//! The recorder reaches a dictionary's structure by saving it as the only item
//! of an array and removing the array's frame, so a recorded save is the
//! dictionary's own text.
//!
//! A record that holds only the header is a failure, not a pass: the table must
//! be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use tjs::vm::runtime::dictionary::{Dictionary, StructObjects};
use tjs::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use tjs::vm::runtime::octet::Octet;
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::{Closure, Variant};

/// The probes the record must hold, in the recorder's order.
const PROBES: &[&str] = &[
    "absent_typeof",
    "absent_text",
    "void_typeof",
    "void_text",
    "null_typeof",
    "created_text",
    "deleted_text",
    "own_class",
    "object_class",
    "instance_save_struct",
    "instance_load_struct",
    "instance_clear",
    "instance_assign",
    "instance_assign_struct",
    "instance_count",
    "instance_member",
    "instance_exists",
    "class_save_struct",
    "class_load_struct",
    "class_clear",
    "class_assign",
    "class_assign_struct",
    "class_count",
    "save_struct",
    "save_empty",
];

/// The probes whose rule this model does not answer.
///
/// A dictionary instance carries no class member (`saveStruct`, `clear`,
/// `assign` and their fellows are static members of the class object), and no
/// `count`, `member` or `exists` member exists at all. The class object and
/// its static table belong to the packet that owns classes, so these probes
/// are recorded and checked against what the engine answered rather than
/// replayed: when that packet lands, its record changes and this check turns
/// them over instead of skipping them.
const DEFERRED: &[(&str, &str)] = &[
    ("instance_save_struct", "undefined"),
    ("instance_load_struct", "undefined"),
    ("instance_clear", "undefined"),
    ("instance_assign", "undefined"),
    ("instance_assign_struct", "undefined"),
    ("instance_count", "undefined"),
    ("instance_member", "undefined"),
    ("instance_exists", "undefined"),
    ("class_save_struct", "Object"),
    ("class_load_struct", "Object"),
    ("class_clear", "Object"),
    ("class_assign", "Object"),
    ("class_assign_struct", "Object"),
    ("class_count", "undefined"),
];

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind `typeof` reports for the answer, or `save` for a save.
    kind: String,
    /// The answer as text.
    text: String,
}

/// The dictionaries the object values in a save name.
#[derive(Default)]
struct Dictionaries {
    /// The handle a value carries, and the dictionary it names.
    entries: Vec<(Rc<dyn Dispatch>, Rc<Dictionary>)>,
}

impl Dictionaries {
    /// Registers a dictionary and answers the value handle that names it.
    fn register(&mut self, dictionary: &Rc<Dictionary>) -> Rc<dyn Dispatch> {
        let handle: Rc<dyn Dispatch> = dictionary.clone();
        self.entries
            .push((Rc::clone(&handle), Rc::clone(dictionary)));
        handle
    }
}

impl StructObjects for Dictionaries {
    fn dictionary(&self, value: &Closure) -> Option<&Dictionary> {
        value.object.as_ref().and_then(|object| {
            self.entries
                .iter()
                .find(|(handle, _)| Rc::ptr_eq(handle, object))
                .map(|(_, dictionary)| dictionary.as_ref())
        })
    }
}

#[test]
fn the_dictionary_reproduces_the_recorded_answers() {
    let records = recorded_answers();
    let member = member_dictionary();
    let mut objects = Dictionaries::default();
    let saved = saved_dictionary(&mut objects);
    let empty = Dictionary::new();
    let mut seen = Vec::with_capacity(records.len());
    for record in &records {
        seen.push(record.probe.as_str());
        if DEFERRED
            .iter()
            .any(|(name, _)| record.probe.as_str() == *name)
        {
            continue;
        }
        let (kind, text) = answer(&record.probe, &member, &saved, &empty, &objects);
        assert_eq!(record.kind, kind, "probe {}: the kind", record.probe);
        assert_eq!(record.text, text, "probe {}: the text", record.probe);
    }
    assert_eq!(
        seen.as_slice(),
        PROBES,
        "the record's probes are not the table this check knows; record it again with \
         `cargo run -p tjs --example dictionary_oracle`"
    );
    deferred_are_what_the_engine_answered(&records);
}

/// A deferred record must say what the engine said, so that the packet which
/// owns class objects has something to turn green instead of a silent skip.
fn deferred_are_what_the_engine_answered(records: &[Record]) {
    for (name, text) in DEFERRED {
        let record = records
            .iter()
            .find(|record| record.probe.as_str() == *name)
            .unwrap_or_else(|| panic!("the record does not hold the deferred probe {name}"));
        assert_eq!(record.kind, "String", "the deferred probe {name}: the kind");
        assert_eq!(
            record.text, *text,
            "the deferred probe {name} no longer answers {text}; its rule has landed, so \
             replay it instead of deferring it"
        );
    }
}

/// The answer the dictionary gives for one probe.
fn answer(
    probe: &str,
    member: &Dictionary,
    saved: &Dictionary,
    empty: &Dictionary,
    objects: &Dictionaries,
) -> (String, String) {
    match probe {
        "save_struct" => save_answer(saved, objects),
        "save_empty" => save_answer(empty, objects),
        "absent_typeof" => typeof_answer(member, "absent"),
        "void_typeof" => typeof_answer(member, "v"),
        "null_typeof" => typeof_answer(member, "nothing"),
        "absent_text" | "void_text" | "created_text" | "deleted_text" => {
            let key = read_target(probe);
            (String::from("String"), text_of(&plain(member, key)))
        }
        "own_class" => instance_answer(member, "Dictionary"),
        "object_class" => instance_answer(member, "Object"),
        other => panic!("the check has no answer for the recorded probe: {other}"),
    }
}

/// The member a text probe reads.
fn read_target(probe: &str) -> &'static str {
    match probe {
        "absent_text" => "absent",
        "void_text" => "v",
        "created_text" => "created",
        "deleted_text" => "deleted",
        other => panic!("the check has no read target for the recorded probe: {other}"),
    }
}

/// A plain member read.
///
/// A member the dictionary does not hold answers the empty value rather than a
/// failure, which is the one rule the dictionary changes in the read path
/// (`crates/tjs/cpp/tjs2/tjsDictionary.cpp:745-758`).
fn plain(dictionary: &Dictionary, key: &str) -> Variant {
    dictionary
        .prop_get(&name(key), MemberFlags::NONE, None)
        .expect("a plain read answers the member or the empty value")
}

/// The kind and the text of a `typeof` answer.
///
/// `typeof` reads with the must-exist rule, so a member the dictionary does
/// not hold is a failure there and answers `undefined`
/// (`crates/tjs/cpp/tjs2/tjsInterCodeExec.cpp:1916-1968`); the three states
/// stay apart and a present empty member answers `void`.
fn typeof_answer(dictionary: &Dictionary, key: &str) -> (String, String) {
    let text = match dictionary.prop_get(&name(key), MemberFlags::MUST_EXIST, None) {
        Ok(value) => value.type_name().to_owned(),
        Err(ObjectError::MemberMustExist) => String::from("undefined"),
        Err(error) => panic!("probe {key}: the read failed: {error}"),
    };
    (String::from("String"), text)
}

/// The kind and the text of a structured save.
fn save_answer(dictionary: &Dictionary, objects: &Dictionaries) -> (String, String) {
    let save = dictionary
        .save_struct_text(objects)
        .expect("the dictionary saves");
    (
        String::from("save"),
        String::from_utf16(save.as_utf16()).expect("the save text is valid UTF-16"),
    )
}

/// The kind and the text of an `instanceof` answer.
fn instance_answer(dictionary: &Dictionary, class: &str) -> (String, String) {
    let answer = dictionary
        .is_instance_of(&name(class), None)
        .expect("the object answers its class");
    let text = if answer { "1" } else { "0" };
    (String::from("String"), String::from(text))
}

/// The text of a value.
fn text_of(value: &Variant) -> String {
    value.to_string().expect("the value has a text").to_string()
}

// reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:745-758
// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1585-1616
#[test]
fn an_absent_member_is_the_empty_value_and_a_write_that_does_not_create_fails() {
    let dictionary = Dictionary::new();
    assert!(matches!(
        dictionary.prop_get(&name("absent"), MemberFlags::NONE, None),
        Ok(Variant::Void)
    ));
    assert!(matches!(
        dictionary.prop_get(&name("absent"), MemberFlags::MUST_EXIST, None),
        Err(ObjectError::MemberMustExist)
    ));
    assert_eq!(
        dictionary.prop_set(
            &name("absent"),
            MemberFlags::NONE,
            &Variant::from(1i64),
            None
        ),
        Err(ObjectError::MemberNotFound)
    );
    assert_eq!(
        dictionary.delete_member(&name("absent"), MemberFlags::NONE, None),
        Err(ObjectError::MemberNotFound)
    );
}

// reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:468-474
// reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:429-457
#[test]
fn a_hidden_member_is_not_saved() {
    let dictionary = Dictionary::new();
    write(&dictionary, "seen", Variant::from(1i64));
    dictionary
        .prop_set(
            &name("hidden"),
            MemberFlags::ENSURE | MemberFlags::HIDDEN,
            &Variant::from(2i64),
            None,
        )
        .expect("the hidden member is written");
    let save = dictionary
        .save_struct_text(&Dictionaries::default())
        .expect("the dictionary saves");
    let text = String::from_utf16(save.as_utf16()).expect("the save text is valid UTF-16");
    assert!(text.contains("\"seen\" => 1"), "{text:?}");
    assert!(!text.contains("hidden"), "{text:?}");
}

// reference/krkr2/cpp/core/tjs2/tjsNative.cpp:417-430
// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:2011-2035
#[test]
fn a_dictionary_answers_its_class_and_the_object_class() {
    let dictionary = Dictionary::new();
    assert_eq!(
        dictionary.is_instance_of(&name("Dictionary"), None),
        Ok(true)
    );
    assert_eq!(dictionary.is_instance_of(&name("Object"), None), Ok(true));
    assert_eq!(dictionary.is_instance_of(&name("Array"), None), Ok(false));
}

/// The dictionary the recorder's member script builds, replayed through the
/// dictionary object.
///
/// The writes are in the script's order: `m.v`, `m.nothing`, `m.created`,
/// `m.deleted`, then the delete of `m.deleted`.
fn member_dictionary() -> Dictionary {
    let dictionary = Dictionary::new();
    write(&dictionary, "v", Variant::Void);
    write(&dictionary, "nothing", Variant::Object(Closure::null()));
    write(&dictionary, "created", Variant::from(7i64));
    write(&dictionary, "deleted", Variant::from(1i64));
    dictionary
        .delete_member(&name("deleted"), MemberFlags::NONE, None)
        .expect("the member is deleted");
    dictionary
}

/// The dictionary the recorder's save script builds, replayed through the
/// dictionary object.
///
/// The writes are in the script's order. The nested dictionaries are built
/// between `d.nothing` and `d.nested`, exactly as the script builds them, so
/// the top dictionary's member order is the recorded one.
fn saved_dictionary(objects: &mut Dictionaries) -> Rc<Dictionary> {
    let dictionary = Rc::new(Dictionary::new());
    let handle = objects.register(&dictionary);
    write(&dictionary, "void", Variant::Void);
    write(&dictionary, "integer", Variant::from(-42i64));
    write(&dictionary, "big", Variant::from(1_234_567_890_123i64));
    write(&dictionary, "real", Variant::from(3.5f64));
    write(&dictionary, "tiny", Variant::from(0.0001f64));
    write(&dictionary, "zero", Variant::from(-0.0f64));
    write(&dictionary, "nan", Variant::from(f64::NAN));
    write(&dictionary, "inf", Variant::from(f64::INFINITY));
    write(
        &dictionary,
        "text",
        Variant::from("quote \" slash \\ tab \t ctrl \u{01}a 日本語"),
    );
    write(
        &dictionary,
        "octet",
        Variant::from(Octet::from_slice(&[0x00, 0xab, 0xff])),
    );
    write(&dictionary, "nothing", Variant::Object(Closure::null()));
    let nested = Rc::new(Dictionary::new());
    let nested_handle = objects.register(&nested);
    write(&nested, "inner", Variant::from(1i64));
    let deep = Rc::new(Dictionary::new());
    let deep_handle = objects.register(&deep);
    write(&deep, "x", Variant::from("y"));
    write(
        &nested,
        "deep",
        Variant::Object(Closure {
            object: Some(deep_handle),
            objthis: None,
        }),
    );
    write(
        &dictionary,
        "nested",
        Variant::Object(Closure {
            object: Some(nested_handle),
            objthis: None,
        }),
    );
    write(
        &dictionary,
        "self",
        Variant::Object(Closure {
            object: Some(handle),
            objthis: None,
        }),
    );
    dictionary
}

/// A name.
fn name(text: &str) -> TjsString {
    TjsString::from(text)
}

/// Writes a member the way an assignment does.
fn write(dictionary: &Dictionary, key: &str, value: Variant) {
    dictionary
        .prop_set(&name(key), MemberFlags::ENSURE, &value, None)
        .expect("the write is served");
}

/// The records of the fixture; a record with no data line is a failure.
fn recorded_answers() -> Vec<Record> {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/dictionary_members.txt");
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "read {}: {error}; record it with `cargo run -p tjs --example dictionary_oracle`",
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
            text: decode(text),
        });
    }
    assert!(
        !records.is_empty(),
        "the dictionary fixture records nothing; record it with \
         `cargo run -p tjs --example dictionary_oracle`"
    );
    records
}

/// The text a record's one-line form carries.
fn decode(encoded: &str) -> String {
    let mut decoded = String::with_capacity(encoded.len());
    let mut characters = encoded.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            decoded.push(character);
            continue;
        }
        match characters.next() {
            Some('\\') => decoded.push('\\'),
            Some('r') => decoded.push('\r'),
            Some('n') => decoded.push('\n'),
            Some('t') => decoded.push('\t'),
            Some(other) => panic!("the record holds an unknown escape: {other:?}"),
            None => panic!("the record ends with a backslash: {encoded:?}"),
        }
    }
    decoded
}
