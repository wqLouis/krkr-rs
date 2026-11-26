//! The array model against the engine's recorded member answers.
//!
//! `tests/fixtures/array_members.txt` holds what the engine answered for one
//! array as a script read, wrote, grew, shrank, sorted and deleted it;
//! `crates/tjs/examples/array_oracle.rs` records it. The check below replays
//! the same operations through the array model (`tjs::vm::runtime::array`) in
//! the same order and compares the kind and the text of every answer, so the
//! model's index space and member operations are the engine's and not only
//! their own idea of them.
//!
//! A record that holds only the header is a failure, not a pass: the table
//! must be recorded before the check means anything.

use std::fs;
use std::path::PathBuf;

use tjs::vm::runtime::array::{ArrayError, ArrayObject, SortOrder};
use tjs::vm::runtime::object::{Dispatch, MemberFlags, ObjectError};
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::Variant;

/// The probes the record must hold, in the recorder's order.
const PROBES: &[&str] = &[
    "length_initial",
    "count_initial",
    "hole",
    "hole_typeof",
    "element",
    "past_end",
    "past_end_typeof",
    "length_after_read",
    "negative_read",
    "negative_beyond",
    "negative_beyond_typeof",
    "write_past",
    "write_gap",
    "length_after_write",
    "shrink",
    "length_after_shrink",
    "shrink_typeof",
    "grow",
    "grown_hole",
    "write_negative",
    "write_negative_read",
    "write_negative_beyond",
    "add",
    "add_read",
    "insert_head",
    "insert_head_read",
    "insert_middle",
    "insert_middle_read",
    "insert_middle_shift",
    "insert_negative",
    "insert_negative_read",
    "insert_too_negative",
    "insert_past",
    "erase",
    "erase_read",
    "erase_negative",
    "erase_past",
    "erase_too_negative",
    "push",
    "find_first",
    "find_start",
    "find_miss",
    "find_kind",
    "find_same",
    "find_negative_start",
    "find_before",
    "remove_first",
    "remove_rest",
    "remove_miss",
    "join",
    "join_purge",
    "reverse",
    "reverse_read",
    "clear",
    "clear_length",
    "clear_read",
    "sort_push",
    "sort_default",
    "sort_default_join",
    "sort_desc",
    "sort_desc_join",
    "sort_text_clear",
    "sort_text_push",
    "sort_numeric",
    "sort_numeric_join",
    "sort_numeric_desc",
    "sort_numeric_desc_join",
    "sort_string",
    "sort_string_join",
    "sort_string_desc",
    "sort_string_desc_join",
    "sort_unknown_letter",
    "sort_unknown_join",
    "sort_stable",
    "sort_stable_join",
    "queue_clear",
    "queue_push",
    "pop",
    "shift",
    "unshift",
    "queue_join",
    "queue_clear2",
    "pop_empty",
    "shift_empty",
    "delete_push",
    "delete_index",
    "delete_read",
    "delete_past",
    "delete_negative",
    "delete_negative_beyond",
    "delete_string_index",
    "named_write",
    "named_read",
    "named_absent_typeof",
    "named_absent_read",
    "named_delete",
    "named_deleted_typeof",
    "named_delete_again",
    "named_void_write",
    "named_void_read",
    "named_length",
    "padded_write",
    "padded_read",
    "signed_read",
    "dotted_read",
    "named_string_typeof",
    "named_string_read",
    "minus_zero",
    "whitespace_index",
    "signed_space_read",
    "assign",
    "assign_length",
    "assign_first",
    "assign_second",
    "named_survives_assign",
    "instanceof_array",
    "instanceof_object",
    "instanceof_dictionary",
];

/// One recorded answer.
struct Record {
    /// The probe's name.
    probe: String,
    /// The kind `typeof` reports for the answer.
    kind: String,
    /// The answer as text, escaped.
    text: String,
}

#[test]
fn the_array_model_reproduces_the_recorded_member_answers() {
    let records = recorded_answers();
    assert_eq!(
        records.len(),
        PROBES.len(),
        "the record holds {} probes and this check knows {}; record it again with \
         `cargo run -p tjs --example array_oracle`",
        records.len(),
        PROBES.len()
    );
    for (record, probe) in records.iter().zip(PROBES) {
        assert_eq!(
            record.probe.as_str(),
            *probe,
            "the record's probes are not the table this check knows; record it again with \
             `cargo run -p tjs --example array_oracle`"
        );
    }
    let array = replayed();
    for record in &records {
        if DEFERRED.contains(&record.probe.as_str()) {
            continue;
        }
        let (kind, text) = answer(run_probe(&array, &record.probe));
        assert_eq!(record.kind, kind, "probe {}: the kind", record.probe);
        assert_eq!(record.text, text, "probe {}: the text", record.probe);
    }
    deferred_are_what_the_engine_raised(&records);
}

/// The probes whose rule this model cannot answer: the engine raised before the
/// array was ever reached.
const DEFERRED: &[&str] = &["instanceof_object"];

/// A deferred record must say what the engine did, so that the packet which
/// owns the rule has something to turn green instead of a silent skip.
fn deferred_are_what_the_engine_raised(records: &[Record]) {
    for name in DEFERRED {
        let record = records
            .iter()
            .find(|record| record.probe == *name)
            .unwrap_or_else(|| panic!("the record does not hold the deferred probe {name}"));
        // "Object" is not a member of the global object, so the engine raised
        // while resolving the name, before the instance check
        // (`tjsInterCodeExec.cpp`, the member-read opcodes). Name resolution is
        // the context packet's rule, not the array's.
        assert_eq!(
            record.kind, "!raise",
            "the deferred probe {name} no longer raises: its rule has landed, so replay it \
             instead of deferring it"
        );
    }
}

/// The array the recorder's setup builds: a value, a hole, a value, a hole and
/// a value.
fn replayed() -> ArrayObject {
    let array = ArrayObject::new();
    write(&array, "2", &Variant::from("c"));
    write(&array, "0", &Variant::from(1i64));
    write(&array, "4", &Variant::from(5i64));
    array
}

/// The answer the array model gives for one probe.
///
/// The operations run in the record's order and share the one array, so a
/// probe that writes is a change the probes after it see.
fn run_probe(array: &ArrayObject, probe: &str) -> Result<Variant, ()> {
    match probe {
        "length_initial" | "count_initial" => Ok(count(array)),
        "hole" => read_at(array, 1),
        "hole_typeof" => typeof_at(array, 1),
        "element" => read_at(array, 2),
        "past_end" => read_at(array, 9),
        "past_end_typeof" => typeof_at(array, 9),
        "length_after_read" => Ok(count(array)),
        "negative_read" => read_at(array, -1),
        "negative_beyond" => read_at(array, -9),
        "negative_beyond_typeof" => typeof_at(array, -9),
        "write_past" => {
            array
                .prop_set_by_num(7, MemberFlags::ENSURE, &Variant::from(9i64))
                .expect("the write is served");
            Ok(Variant::from(9i64))
        }
        "write_gap" => read_at(array, 5),
        "length_after_write" => Ok(count(array)),
        "shrink" => {
            write(array, "length", &Variant::from(3i64));
            Ok(Variant::from(3i64))
        }
        "length_after_shrink" => Ok(count(array)),
        "shrink_typeof" => typeof_at(array, 3),
        "grow" => {
            write(array, "length", &Variant::from(5i64));
            Ok(Variant::from(5i64))
        }
        "grown_hole" => read_at(array, 4),
        "write_negative" => {
            array
                .prop_set_by_num(-1, MemberFlags::ENSURE, &Variant::from("z"))
                .expect("the write is served");
            Ok(Variant::from("z"))
        }
        "write_negative_read" => read_at(array, 4),
        "write_negative_beyond" => {
            let error = array
                .prop_set_by_num(-9, MemberFlags::ENSURE, &Variant::from(1i64))
                .expect_err("the write is refused");
            assert_eq!(error, ObjectError::MemberNotFound);
            Err(())
        }
        "add" => {
            let index = array
                .append(Variant::from("a"))
                .expect("the value is appended");
            Ok(Variant::from(
                i64::try_from(index).expect("the index fits a 64-bit integer"),
            ))
        }
        "add_read" => read_at(array, 5),
        "insert_head" => {
            array
                .insert(0, Variant::from("i"))
                .expect("the value is inserted");
            Ok(Variant::Void)
        }
        "insert_head_read" => read_at(array, 0),
        "insert_middle" => {
            array
                .insert(2, Variant::from("m"))
                .expect("the value is inserted");
            Ok(Variant::Void)
        }
        "insert_middle_read" => read_at(array, 2),
        "insert_middle_shift" => read_at(array, 3),
        "insert_negative" => {
            array
                .insert(-1, Variant::from("e"))
                .expect("the value is inserted");
            Ok(Variant::Void)
        }
        "insert_negative_read" => read_at(array, 7),
        "insert_too_negative" => {
            let error = array
                .insert(-99, Variant::from(1i64))
                .expect_err("the position is out of range");
            assert_eq!(error, ArrayError::IndexOutOfRange);
            Err(())
        }
        "insert_past" => {
            let error = array
                .insert(99, Variant::from(1i64))
                .expect_err("the position is out of range");
            assert_eq!(error, ArrayError::IndexOutOfRange);
            Err(())
        }
        "erase" => {
            array.erase(0).expect("the element is removed");
            Ok(Variant::Void)
        }
        "erase_read" => read_at(array, 0),
        "erase_negative" => {
            array.erase(-1).expect("the element is removed");
            Ok(Variant::Void)
        }
        "erase_past" => {
            let error = array.erase(99).expect_err("the position is out of range");
            assert_eq!(error, ArrayError::IndexOutOfRange);
            Err(())
        }
        "erase_too_negative" => {
            let error = array.erase(-99).expect_err("the position is out of range");
            assert_eq!(error, ArrayError::IndexOutOfRange);
            Err(())
        }
        "push" => {
            let length = array
                .push(&[
                    Variant::from(7i64),
                    Variant::from(7i64),
                    Variant::from(8i64),
                ])
                .expect("the values are appended");
            Ok(Variant::from(
                i64::try_from(length).expect("the length fits a 64-bit integer"),
            ))
        }
        "find_first" => Ok(find(array, &Variant::from(7i64), 0)),
        "find_start" => Ok(find(array, &Variant::from(7i64), 8)),
        "find_miss" => Ok(find(array, &Variant::from(99i64), 0)),
        "find_kind" => Ok(find(array, &Variant::from(1.0f64), 0)),
        "find_same" => Ok(find(array, &Variant::from(1i64), 0)),
        "find_negative_start" => Ok(find(array, &Variant::from(8i64), -1)),
        "find_before" => Ok(find(array, &Variant::from(7i64), -99)),
        "remove_first" => Ok(removed(
            array
                .remove(&Variant::from(7i64), false)
                .expect("the elements are removed"),
        )),
        "remove_rest" => Ok(removed(
            array
                .remove(&Variant::from(7i64), true)
                .expect("the elements are removed"),
        )),
        "remove_miss" => Ok(removed(
            array
                .remove(&Variant::from(99i64), true)
                .expect("the elements are removed"),
        )),
        "join" => Ok(Variant::from(
            array
                .join(&TjsString::from("-"), false)
                .expect("the elements join"),
        )),
        "join_purge" => Ok(Variant::from(
            array
                .join(&TjsString::from("-"), true)
                .expect("the elements join"),
        )),
        "reverse" => {
            array.reverse().expect("the elements reverse");
            Ok(Variant::Void)
        }
        "reverse_read" => read_at(array, 0),
        "clear" | "sort_text_clear" | "queue_clear" | "queue_clear2" => {
            array.clear().expect("the elements clear");
            Ok(Variant::Void)
        }
        "clear_length" | "named_length" => Ok(count(array)),
        "clear_read" => read_at(array, 0),
        "sort_push" => {
            let length = array
                .push(&[
                    Variant::from(3i64),
                    Variant::from(1i64),
                    Variant::from(2i64),
                ])
                .expect("the values are appended");
            Ok(Variant::from(
                i64::try_from(length).expect("the length fits a 64-bit integer"),
            ))
        }
        "sort_default" => {
            array
                .sort(SortOrder::Ascending, false)
                .expect("the values sort");
            Ok(Variant::Void)
        }
        "sort_default_join" => Ok(joined(array, ",")),
        "sort_desc" => {
            array
                .sort(SortOrder::Descending, false)
                .expect("the values sort");
            Ok(Variant::Void)
        }
        "sort_desc_join" => Ok(joined(array, ",")),
        "sort_text_push" => {
            let length = array
                .push(&[Variant::from("10"), Variant::from("9"), Variant::from("1")])
                .expect("the values are appended");
            Ok(Variant::from(
                i64::try_from(length).expect("the length fits a 64-bit integer"),
            ))
        }
        "sort_numeric" => {
            array
                .sort(SortOrder::NumericAscending, false)
                .expect("the values sort");
            Ok(Variant::Void)
        }
        "sort_numeric_join" => Ok(joined(array, ",")),
        "sort_numeric_desc" => {
            array
                .sort(SortOrder::NumericDescending, false)
                .expect("the values sort");
            Ok(Variant::Void)
        }
        "sort_numeric_desc_join" => Ok(joined(array, ",")),
        "sort_string" => {
            array
                .sort(SortOrder::TextAscending, false)
                .expect("the values sort");
            Ok(Variant::Void)
        }
        "sort_string_join" => Ok(joined(array, ",")),
        "sort_string_desc" => {
            array
                .sort(SortOrder::TextDescending, false)
                .expect("the values sort");
            Ok(Variant::Void)
        }
        "sort_string_desc_join" => Ok(joined(array, ",")),
        "sort_unknown_letter" => {
            array
                .sort(SortOrder::from_letter(Some('?')), false)
                .expect("the values sort");
            Ok(Variant::Void)
        }
        "sort_unknown_join" => Ok(joined(array, ",")),
        "sort_stable" => {
            array
                .sort(SortOrder::Ascending, true)
                .expect("the values sort");
            Ok(Variant::Void)
        }
        "sort_stable_join" => Ok(joined(array, ",")),
        "queue_push" | "delete_push" => {
            let length = array
                .push(&[
                    Variant::from(1i64),
                    Variant::from(2i64),
                    Variant::from(3i64),
                ])
                .expect("the values are appended");
            Ok(Variant::from(
                i64::try_from(length).expect("the length fits a 64-bit integer"),
            ))
        }
        "pop" => Ok(array.pop().expect("the element is removed")),
        "shift" => Ok(array.shift().expect("the element is removed")),
        "unshift" => {
            let length = array
                .unshift(&[Variant::from(0i64)])
                .expect("the value is inserted");
            Ok(Variant::from(
                i64::try_from(length).expect("the length fits a 64-bit integer"),
            ))
        }
        "queue_join" => Ok(joined(array, ",")),
        "pop_empty" => Ok(array.pop().expect("the empty answer is served")),
        "shift_empty" => Ok(array.shift().expect("the empty answer is served")),
        "delete_index" => delete_at(array, 0),
        "delete_read" => read_at(array, 0),
        "delete_past" => delete_at(array, 9),
        "delete_negative" => delete_at(array, -1),
        "delete_negative_beyond" => delete_at(array, -9),
        "delete_string_index" => delete_name(array, "0"),
        "named_write" => {
            write(array, "note", &Variant::from("n"));
            Ok(Variant::from("n"))
        }
        "named_read" => read_name(array, "note"),
        "named_absent_typeof" => typeof_name(array, "nope"),
        "named_absent_read" => {
            let error = array
                .prop_get(&text("nope"), MemberFlags::NONE, None)
                .expect_err("the member does not exist");
            assert_eq!(error, ObjectError::MemberNotFound);
            Err(())
        }
        "named_delete" => delete_name(array, "note"),
        "named_deleted_typeof" => typeof_name(array, "note"),
        "named_delete_again" => delete_name(array, "note"),
        "named_void_write" => {
            write(array, "blank", &Variant::Void);
            Ok(Variant::Void)
        }
        "named_void_read" => read_name(array, "blank"),
        "padded_write" => {
            write(array, " 2 ", &Variant::from("p"));
            Ok(Variant::from("p"))
        }
        "padded_read" => read_name(array, " 2 "),
        "signed_read" => read_name(array, "+2"),
        "dotted_read" => read_name(array, "2.9"),
        "named_string_typeof" => typeof_name(array, "2x"),
        "named_string_read" => {
            let error = array
                .prop_get(&text("2x"), MemberFlags::NONE, None)
                .expect_err("the member does not exist");
            assert_eq!(error, ObjectError::MemberNotFound);
            Err(())
        }
        "minus_zero" => read_name(array, "-0"),
        "whitespace_index" => read_name(array, "\t1\n"),
        "signed_space_read" => read_name(array, "- 1"),
        "assign" => {
            let source = ArrayObject::new();
            source
                .push(&[Variant::from(4i64), Variant::from(5i64)])
                .expect("the source values are appended");
            array.assign(&source).expect("the elements are assigned");
            Ok(Variant::Void)
        }
        "assign_length" => Ok(count(array)),
        "assign_first" => read_at(array, 0),
        "assign_second" => read_at(array, 1),
        "named_survives_assign" => typeof_name(array, "blank"),
        "instanceof_array" => Ok(instance_of(array, "Array")),
        "instanceof_object" => Ok(instance_of(array, "Object")),
        "instanceof_dictionary" => Ok(instance_of(array, "Dictionary")),
        other => panic!("the check has no replay for the recorded probe: {other}"),
    }
}

/// The (kind, text) pair one answer contributes to the record.
fn answer(result: Result<Variant, ()>) -> (String, String) {
    match result {
        Ok(value) => {
            let kind = value.type_name().to_owned();
            let text = value.to_string().expect("the answer has a text");
            (kind, escaped(&text.to_string()))
        }
        Err(()) => (String::from("!raise"), String::new()),
    }
}

/// The number of elements, as the answer a property read gives.
fn count(array: &ArrayObject) -> Variant {
    Variant::from(i64::try_from(array.len()).expect("the count fits a 64-bit integer"))
}

/// The element at `index`, a probe that expects a read to be served.
fn read_at(array: &ArrayObject, index: i64) -> Result<Variant, ()> {
    match array.prop_get_by_num(index, MemberFlags::NONE) {
        Ok(value) => Ok(value),
        Err(error) => panic!("the read at {index} failed: {error}"),
    }
}

/// The element `name` reads, a probe that expects a read to be served.
fn read_name(array: &ArrayObject, name: &str) -> Result<Variant, ()> {
    match array.prop_get(&text(name), MemberFlags::NONE, None) {
        Ok(value) => Ok(value),
        Err(error) => panic!("the read of {name:?} failed: {error}"),
    }
}

/// The kind `typeof` reports for the element at `index`.
fn typeof_at(array: &ArrayObject, index: i64) -> Result<Variant, ()> {
    match array.prop_get_by_num(index, MemberFlags::MUST_EXIST) {
        Ok(Variant::Void) => Ok(Variant::from("void")),
        Ok(value) => Ok(Variant::from(value.type_name())),
        Err(ObjectError::MemberNotFound) => Ok(Variant::from("undefined")),
        Err(error) => panic!("the typeof read at {index} failed: {error}"),
    }
}

/// The kind `typeof` reports for the member `name`.
fn typeof_name(array: &ArrayObject, name: &str) -> Result<Variant, ()> {
    match array.prop_get(&text(name), MemberFlags::MUST_EXIST, None) {
        Ok(Variant::Void) => Ok(Variant::from("void")),
        Ok(value) => Ok(Variant::from(value.type_name())),
        Err(ObjectError::MemberNotFound | ObjectError::MemberMustExist) => {
            Ok(Variant::from("undefined"))
        }
        Err(error) => panic!("the typeof read of {name:?} failed: {error}"),
    }
}

/// The position of the first element equal to `value`, or `-1`.
fn find(array: &ArrayObject, value: &Variant, start: i64) -> Variant {
    Variant::from(array.find(value, start).expect("the search is served"))
}

/// The number of removed elements as a script number.
fn removed(count: usize) -> Variant {
    Variant::from(i64::try_from(count).expect("the count fits a 64-bit integer"))
}

/// The elements joined with `delimiter`.
fn joined(array: &ArrayObject, delimiter: &str) -> Variant {
    Variant::from(
        array
            .join(&TjsString::from(delimiter), false)
            .expect("the elements join"),
    )
}

/// Deletes the element at `index` and answers the boolean the operator gives.
fn delete_at(array: &ArrayObject, index: i64) -> Result<Variant, ()> {
    match array.delete_member_by_num(index) {
        Ok(()) => Ok(Variant::from(1i64)),
        Err(ObjectError::MemberNotFound) => Ok(Variant::from(0i64)),
        Err(error) => panic!("the delete at {index} failed: {error}"),
    }
}

/// Deletes the member `name` and answers the boolean the operator gives.
fn delete_name(array: &ArrayObject, name: &str) -> Result<Variant, ()> {
    match array.delete_member(&text(name), MemberFlags::NONE, None) {
        Ok(()) => Ok(Variant::from(1i64)),
        Err(ObjectError::MemberNotFound) => Ok(Variant::from(0i64)),
        Err(error) => panic!("the delete of {name:?} failed: {error}"),
    }
}

/// Whether the array belongs to `class`, as the operator's answer.
fn instance_of(array: &ArrayObject, class: &str) -> Variant {
    match array.is_instance_of(&text(class), None) {
        Ok(true) => Variant::from(1i64),
        Ok(false) => Variant::from(0i64),
        Err(error) => panic!("the class test for {class:?} failed: {error}"),
    }
}

/// Writes `value` to the member `name` the way an assignment does.
fn write(array: &ArrayObject, name: &str, value: &Variant) {
    array
        .prop_set(&text(name), MemberFlags::ENSURE, value, None)
        .expect("the write is served");
}

/// A name.
fn text(name: &str) -> TjsString {
    TjsString::from(name)
}

/// The text escaped so that one probe stays on one line, the same way the
/// recorder writes it.
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

/// The records of the fixture; a record with no data line is a failure.
fn recorded_answers() -> Vec<Record> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/array_members.txt");
    let table = match fs::read_to_string(&path) {
        Ok(table) => table,
        Err(error) => panic!(
            "the array member fixture {} is not readable: {error}; record it with \
             `cargo run -p tjs --example array_oracle`",
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
    assert!(
        !records.is_empty(),
        "the array member fixture records nothing; record it with \
         `cargo run -p tjs --example array_oracle`"
    );
    records
}
