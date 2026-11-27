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
use tjs::vm::runtime::octet::Octet;
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::Variant;

/// The text a class object has on the right of `instanceof`: its handle, never
/// its class name (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213`).
const CLASS_OBJECT_TEXT: &str = "(object 0:0)";

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
    "instanceof_array_name",
    "instanceof_object_name",
    "instanceof_dictionary_name",
    "instanceof_string_name",
    "instanceof_number_name",
    "instanceof_octet_name",
    "instanceof_split_name",
    "instanceof_literal_name",
    "instanceof_new_name",
    "instanceof_invalidated",
    "split_text_count",
    "split_text_first",
    "split_text_second",
    "split_text_third",
    "split_text_join",
    "split_multi_count",
    "split_multi_join",
    "split_empty_pattern_count",
    "split_empty_pattern_first",
    "split_missing_count",
    "split_missing_first",
    "split_leading_count",
    "split_leading_first",
    "split_leading_purge_count",
    "split_trailing_count",
    "split_trailing_last",
    "split_trailing_purge_count",
    "split_consecutive_count",
    "split_consecutive_second",
    "split_consecutive_purge_count",
    "split_consecutive_purge_join",
    "split_empty_text_count",
    "split_empty_text_first",
    "split_empty_text_purge_count",
    "split_purge_zero_count",
    "split_purge_one_count",
    "split_purge_two_count",
    "split_purge_text_count",
    "split_reserved_ignored_count",
    "split_number_pattern_count",
    "split_void_pattern_count",
    "split_void_text_count",
    "split_void_text_first",
    "split_one_param_refused",
    "split_octet_pattern_refused",
    "split_octet_pattern_cleared",
    "split_octet_text_refused",
    "split_octet_text_cleared",
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

/// An invalidated array refuses the class question with the engine's own
/// invalid-object error, never with `false`.
///
/// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1750-1758 (a named class question
/// delegates to the custom object);
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1764-1765 (the custom object
/// checks its validity before it compares a name).
#[test]
fn an_invalidated_array_refuses_the_class_question() {
    let array = ArrayObject::new();
    assert!(array.invalidate(), "the array invalidates");
    assert_eq!(
        array.is_instance_of(&text("Array"), None),
        Err(ObjectError::InvalidObject)
    );
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
        // `a instanceof Array` compares the class object's handle text, not
        // its class name (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213`).
        "instanceof_array" | "instanceof_dictionary" => Ok(instance_of(array, CLASS_OBJECT_TEXT)),
        "instanceof_object" => Ok(instance_of(array, "Object")),
        // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1750-1758 (a named class
        // question is the custom object's), and
        // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1767-1781 (the custom
        // object answers "Object" or a class name it carries).
        "instanceof_array_name" => Ok(instance_of(array, "Array")),
        "instanceof_object_name" => Ok(instance_of(array, "Object")),
        "instanceof_dictionary_name" => Ok(instance_of(array, "Dictionary")),
        "instanceof_string_name" => Ok(instance_of(array, "String")),
        "instanceof_number_name" => Ok(instance_of(array, "Number")),
        "instanceof_octet_name" => Ok(instance_of(array, "Octet")),
        // A split array, a literal and a construction all carry the class
        // name the native construction adds
        // (`reference/krkr2/cpp/core/tjs2/tjsNative.cpp:323-325`).
        "instanceof_split_name" => {
            let split = ArrayObject::new();
            call_split(
                &split,
                &Variant::from(","),
                &Variant::from("a,b"),
                &Variant::Void,
                &Variant::Void,
            )
            .expect("the split is served");
            Ok(instance_of(&split, "Array"))
        }
        "instanceof_literal_name" => Ok(instance_of(&ArrayObject::new(), "Array")),
        "instanceof_new_name" => Ok(instance_of(&ArrayObject::new(), "Array")),
        // An invalidated array refuses the class question instead of answering
        // false (`reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1750-1758`;
        // `reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1764-1765`).
        "instanceof_invalidated" => {
            let invalidated = ArrayObject::new();
            assert!(invalidated.invalidate(), "the array invalidates");
            let error = invalidated
                .is_instance_of(&text("Array"), None)
                .expect_err("the invalidated array refuses the class question");
            assert_eq!(error, ObjectError::InvalidObject);
            Err(())
        }
        // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:490-560
        "split_text_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from("a,b,c"),
            &Variant::Void,
            &Variant::Void,
        ),
        "split_text_first" => split_element(
            array,
            &Variant::from(","),
            &Variant::from("a,b,c"),
            &Variant::Void,
            &Variant::Void,
            0,
        ),
        "split_text_second" => split_element(
            array,
            &Variant::from(","),
            &Variant::from("a,b,c"),
            &Variant::Void,
            &Variant::Void,
            1,
        ),
        "split_text_third" => split_element(
            array,
            &Variant::from(","),
            &Variant::from("a,b,c"),
            &Variant::Void,
            &Variant::Void,
            2,
        ),
        "split_text_join" => split_join(
            array,
            &Variant::from(","),
            &Variant::from("a,b,c"),
            &Variant::Void,
            &Variant::Void,
            "|",
        ),
        "split_multi_count" => split_count(
            array,
            &Variant::from(",;"),
            &Variant::from("a,b;c"),
            &Variant::Void,
            &Variant::Void,
        ),
        "split_multi_join" => split_join(
            array,
            &Variant::from(",;"),
            &Variant::from("a,b;c"),
            &Variant::Void,
            &Variant::Void,
            "|",
        ),
        "split_empty_pattern_count" => split_count(
            array,
            &Variant::from(""),
            &Variant::from("abc"),
            &Variant::Void,
            &Variant::Void,
        ),
        "split_empty_pattern_first" => split_element(
            array,
            &Variant::from(""),
            &Variant::from("abc"),
            &Variant::Void,
            &Variant::Void,
            0,
        ),
        "split_missing_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from("abc"),
            &Variant::Void,
            &Variant::Void,
        ),
        "split_missing_first" => split_element(
            array,
            &Variant::from(","),
            &Variant::from("abc"),
            &Variant::Void,
            &Variant::Void,
            0,
        ),
        "split_leading_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from(",a"),
            &Variant::Void,
            &Variant::Void,
        ),
        "split_leading_first" => split_element(
            array,
            &Variant::from(","),
            &Variant::from(",a"),
            &Variant::Void,
            &Variant::Void,
            0,
        ),
        "split_leading_purge_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from(",a"),
            &Variant::Void,
            &Variant::from(1i64),
        ),
        "split_trailing_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from("a,"),
            &Variant::Void,
            &Variant::Void,
        ),
        "split_trailing_last" => split_element(
            array,
            &Variant::from(","),
            &Variant::from("a,"),
            &Variant::Void,
            &Variant::Void,
            1,
        ),
        "split_trailing_purge_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from("a,"),
            &Variant::Void,
            &Variant::from(1i64),
        ),
        "split_consecutive_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from("a,,b"),
            &Variant::Void,
            &Variant::Void,
        ),
        "split_consecutive_second" => split_element(
            array,
            &Variant::from(","),
            &Variant::from("a,,b"),
            &Variant::Void,
            &Variant::Void,
            1,
        ),
        "split_consecutive_purge_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from("a,,b"),
            &Variant::Void,
            &Variant::from(1i64),
        ),
        "split_consecutive_purge_join" => split_join(
            array,
            &Variant::from(","),
            &Variant::from("a,,b"),
            &Variant::Void,
            &Variant::from(1i64),
            "|",
        ),
        "split_empty_text_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from(""),
            &Variant::Void,
            &Variant::Void,
        ),
        "split_empty_text_first" => split_element(
            array,
            &Variant::from(","),
            &Variant::from(""),
            &Variant::Void,
            &Variant::Void,
            0,
        ),
        "split_empty_text_purge_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from(""),
            &Variant::Void,
            &Variant::from(1i64),
        ),
        "split_purge_zero_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from("a,,b"),
            &Variant::Void,
            &Variant::from(0i64),
        ),
        "split_purge_one_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from("a,,b"),
            &Variant::Void,
            &Variant::from(1i64),
        ),
        "split_purge_two_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from("a,,b"),
            &Variant::Void,
            &Variant::from(2i64),
        ),
        "split_purge_text_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from("a,,b"),
            &Variant::Void,
            &Variant::from("abc"),
        ),
        "split_reserved_ignored_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::from("a,b"),
            &Variant::from(1i64),
            &Variant::from(0i64),
        ),
        "split_number_pattern_count" => split_count(
            array,
            &Variant::from(1i64),
            &Variant::from("a1b"),
            &Variant::Void,
            &Variant::Void,
        ),
        "split_void_pattern_count" => split_count(
            array,
            &Variant::Void,
            &Variant::from("abc"),
            &Variant::Void,
            &Variant::Void,
        ),
        "split_void_text_count" => split_count(
            array,
            &Variant::from(","),
            &Variant::Void,
            &Variant::Void,
            &Variant::Void,
        ),
        "split_void_text_first" => split_element(
            array,
            &Variant::from(","),
            &Variant::Void,
            &Variant::Void,
            &Variant::Void,
            0,
        ),
        "split_one_param_refused" => {
            let answer = array.func_call(&text("split"), Some(&[Variant::from(",")]), None);
            assert!(
                matches!(answer, Err(ObjectError::ParameterCount)),
                "the one-parameter call was served"
            );
            Err(())
        }
        "split_octet_pattern_refused" => split_refused(
            array,
            &Variant::Octet(Octet::from_slice(&[1])),
            &Variant::from("a,b"),
        ),
        "split_octet_pattern_cleared" => Ok(count(array)),
        "split_octet_text_refused" => split_refused(
            array,
            &Variant::from(","),
            &Variant::Octet(Octet::from_slice(&[1])),
        ),
        "split_octet_text_cleared" => Ok(count(array)),
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

/// Calls the array's `split` member through the dispatch surface.
fn call_split(
    array: &ArrayObject,
    pattern: &Variant,
    subject: &Variant,
    reserved: &Variant,
    purge: &Variant,
) -> Result<(), ObjectError> {
    array
        .func_call(
            &text("split"),
            Some(&[
                pattern.clone(),
                subject.clone(),
                reserved.clone(),
                purge.clone(),
            ]),
            None,
        )
        .map(|_answer| ())
}

/// Splits and answers the count.
fn split_count(
    array: &ArrayObject,
    pattern: &Variant,
    text: &Variant,
    reserved: &Variant,
    purge: &Variant,
) -> Result<Variant, ()> {
    match call_split(array, pattern, text, reserved, purge) {
        Ok(()) => Ok(count(array)),
        Err(error) => panic!("the split failed: {error}"),
    }
}

/// Splits and answers the element at `index`.
fn split_element(
    array: &ArrayObject,
    pattern: &Variant,
    text: &Variant,
    reserved: &Variant,
    purge: &Variant,
    index: i64,
) -> Result<Variant, ()> {
    match call_split(array, pattern, text, reserved, purge) {
        Ok(()) => read_at(array, index),
        Err(error) => panic!("the split failed: {error}"),
    }
}

/// Splits and answers the elements joined with `delimiter`.
fn split_join(
    array: &ArrayObject,
    pattern: &Variant,
    text: &Variant,
    reserved: &Variant,
    purge: &Variant,
    delimiter: &str,
) -> Result<Variant, ()> {
    match call_split(array, pattern, text, reserved, purge) {
        Ok(()) => Ok(joined(array, delimiter)),
        Err(error) => panic!("the split failed: {error}"),
    }
}

/// Splits and asserts the dispatch surface refused the call.
fn split_refused(array: &ArrayObject, pattern: &Variant, text: &Variant) -> Result<Variant, ()> {
    match call_split(array, pattern, text, &Variant::Void, &Variant::Void) {
        Err(ObjectError::CallNotServed) => Err(()),
        Ok(()) => panic!("the split answered where the engine raised"),
        Err(error) => panic!("the split failed with another error: {error}"),
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
