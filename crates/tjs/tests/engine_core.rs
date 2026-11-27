//! The engine as one object: the operations a host asks it, and their answers.
//!
//! The engine core is the owner of a global scope, of the values a caller keeps
//! across calls and of the blocks it has compiled. This check drives it the way
//! the host drives a script engine — read a member, write one, delete one,
//! enumerate, call, construct, ask a class question — and asserts the answers
//! the engine's own recordings and the language give.
//!
//! # Where the expectations come from
//!
//! The member rows are `tests/fixtures/object_members.txt`'s, recorded from the
//! engine by `crates/tjs/examples/object_oracle.rs`; the class rows are
//! `tests/fixtures/dictionary_members.txt`'s and `tests/fixtures/array_members.txt`'s.
//! One substitution is made and named where it is made: the recorder builds its
//! dictionary with `new Dictionary()`, which the engine core's global does not
//! carry yet, so the check builds the same object with the constant dictionary
//! literal — the same dictionary class, and the same member rules.
//!
//! Everything else is the language's: the reference lines sit above the
//! assertion they pin.

use tjs::vm::runtime::engine_core::{CoreError, EngineCore, Operation};
use tjs::vm::runtime::exec::Operator;
use tjs::vm::runtime::retained::{RetainError, RetainedId};
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::Variant;

/// The member name a text spells.
fn name(text: &str) -> TjsString {
    TjsString::from(text)
}

/// The integer an answer holds.
fn integer(value: &Variant) -> i64 {
    match value {
        Variant::Integer(value) => *value,
        other => panic!("the answer is not an integer: {other:?}"),
    }
}

/// The text an answer holds.
fn text(value: &Variant) -> String {
    match value {
        Variant::String(value) => value.to_string(),
        other => panic!("the answer is not a text: {other:?}"),
    }
}

/// The object an answer holds, kept for the engine's later calls.
fn retain_object(core: &EngineCore, value: &Variant) -> RetainedId {
    match value {
        Variant::Object(closure) if closure.object.is_some() => core.retain(value.clone()),
        other => panic!("the answer is not an object: {other:?}"),
    }
}

/// Runs one script, so a case checks one line.
fn run(core: &EngineCore, script: &str, block: &str) {
    match core.run(script, block) {
        Ok(_) => {}
        Err(error) => panic!("the script did not run: {error}"),
    }
}

/// The member `member` of the object `id`.
fn read(core: &EngineCore, id: RetainedId, member: &str) -> Variant {
    match core.member_read(id, &name(member)) {
        Ok(Some(value)) => value,
        Ok(None) => panic!("the member {member} is not there"),
        Err(error) => panic!("the member {member} was not readable: {error}"),
    }
}

/// Whether the object `id` holds the member `member`.
fn has(core: &EngineCore, id: RetainedId, member: &str) -> bool {
    match core.member_read(id, &name(member)) {
        Ok(value) => value.is_some(),
        Err(error) => panic!("the member {member} was not readable: {error}"),
    }
}

/// The text an expression answers, run against the engine's global.
fn evaluated(core: &EngineCore, expression: &str) -> String {
    match core.eval(expression, "engine_core") {
        Ok(value) => text(&value),
        Err(error) => panic!("the expression {expression} did not answer: {error}"),
    }
}

/// The dictionary the member fixture builds, and the array a script literal
/// builds.
///
/// The writes are the recorder's, in its order. `(const) %[]` stands in for
/// `new Dictionary()`: the engine core's global publishes the classes the
/// runtime's registry holds, and the container classes are not among them yet,
/// so the literal is how this check makes the same object.
fn members_script() -> &'static str {
    r#"var d = (const) %[];
var a = (const) [10, 20, 30];
var dict = (const) %[];
d.a = 1;
d["b"] = 2;
d.c = 3;
d.v = void;
d.n = 7;
d["a"] = 4;
d.b = d["a"] + 1;
delete d.c;
dict.n = 1;
dict.n += 2;"#
}

/// A script's members are the global object's members, and a declared function
/// is callable through the id the engine hands out for it.
#[test]
fn a_declared_var_and_a_declared_function_are_the_globals_members() {
    let core = EngineCore::new();
    run(
        &core,
        "var answer = 41;\nfunction next(value) { return value + 1; }",
        "declaration",
    );
    let global = core.global_object();

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1352-1398 — a write creates the
    // member, so the declaration leaves the value on the object.
    assert_eq!(integer(&read(&core, global, "answer")), 41);

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1276-1311 — a read answers the
    // member the object holds, here an object of its own.
    let function = read(&core, global, "next");
    assert!(
        matches!(function, Variant::Object(_)),
        "a function is an object"
    );

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1200-1225 — a call of a value
    // is the value's own call, and the function's code runs for it.
    let next = retain_object(&core, &function);
    let answer = core
        .call_value(next, &[Variant::from(41i64)])
        .expect("the function is callable");
    assert_eq!(integer(&answer), 42);
}

/// A read tells a member that is not there from one that holds nothing from one
/// that holds a value, which is the distinction `typeof` is made of.
#[test]
fn an_absent_member_a_void_member_and_a_value_are_three_answers() {
    let core = EngineCore::new();
    run(&core, members_script(), "members");
    let global = core.global_object();
    let dictionary = retain_object(&core, &read(&core, global, "d"));

    // the recorded rows of `tests/fixtures/object_members.txt`: a member the
    // script wrote answers its value, `d.v` holds nothing, and `d.zz` is not a
    // member at all
    assert_eq!(integer(&read(&core, dictionary, "a")), 4);
    assert!(matches!(read(&core, dictionary, "v"), Variant::Void));
    assert!(!has(&core, dictionary, "zz"), "`d.zz` was never a member");

    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2602-2638 — the kind
    // `typeof` reports is the answer's own kind spelling.
    assert_eq!(evaluated(&core, "typeof (d.a)"), "Integer");
    assert_eq!(evaluated(&core, "typeof (d.v)"), "void");
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1898-1903 — a member
    // that is not there is the text `undefined`, and not the empty value.
    assert_eq!(evaluated(&core, "typeof (d.zz)"), "undefined");
    // the member the script deleted is not there either
    assert_eq!(evaluated(&core, "typeof (d.c)"), "undefined");
}

/// A write creates a member, and the read sees what the write stored.
#[test]
fn a_written_member_reads_back_and_a_deleted_one_does_not() {
    let core = EngineCore::new();
    run(&core, members_script(), "members");
    let global = core.global_object();
    let dictionary = retain_object(&core, &read(&core, global, "d"));

    // the recorded `created_read` row: an assignment creates the member
    assert_eq!(integer(&read(&core, dictionary, "n")), 7);

    core.member_write(dictionary, &name("fresh"), &Variant::from(12i64))
        .expect("the write is served");
    assert_eq!(integer(&read(&core, dictionary, "fresh")), 12);

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1537-1571 — a delete removes
    // the member, and the read after it is the missing member.
    core.delete_member(dictionary, &name("fresh"))
        .expect("the delete is served");
    assert!(
        !has(&core, dictionary, "fresh"),
        "the delete removed the member"
    );
}

/// An array's element and its length are members, and its class is the array
/// class.
#[test]
fn an_array_element_reads_through_its_index() {
    let core = EngineCore::new();
    run(&core, members_script(), "members");
    let global = core.global_object();
    let array = retain_object(&core, &read(&core, global, "a"));

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:144-153 — a numeric subscript
    // is the member its decimal text names; reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1580-1586
    // — the array answers that name from the element it holds.
    assert_eq!(integer(&read(&core, array, "1")), 20);
    assert_eq!(integer(&read(&core, array, "length")), 3);
    assert_eq!(integer(&read(&core, array, "count")), 3);

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1970-2010 — the names the
    // object carries are the answer to a class question, and an array carries
    // the array class.
    let classes = core.class_names(array).expect("the object has classes");
    assert_eq!(classes, vec![name("Array")]);
}

/// A dictionary answers the dictionary class and the object class, and its
/// class names are its own.
#[test]
fn a_dictionary_answers_its_class_and_the_object_class() {
    let core = EngineCore::new();
    run(&core, members_script(), "members");
    let global = core.global_object();
    let dictionary = retain_object(&core, &read(&core, global, "d"));

    // the recorded `own_class` and `object_class` rows
    assert!(
        core.is_instance_of(dictionary, &name("Dictionary"))
            .expect("the question is asked")
    );
    assert!(
        core.is_instance_of(dictionary, &name("Object"))
            .expect("the question is asked")
    );
    assert_eq!(
        core.class_names(dictionary)
            .expect("the object has classes"),
        vec![name("Dictionary")]
    );

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1621-1665 — a usable object
    // answers that it is valid, and a value that is not an object is refused
    // rather than reported as either answer.
    assert!(core.is_valid(dictionary).expect("the object is usable"));
    let number = core.retain(Variant::from(1i64));
    assert!(matches!(
        core.is_valid(number),
        Err(CoreError::Retained(RetainError::NotAnObject(id))) if id == number
    ));
}

/// A compound assignment applies to the member, and the member holds the
/// result.
#[test]
fn a_compound_assignment_reads_applies_and_writes_the_member() {
    let core = EngineCore::new();
    run(&core, members_script(), "members");
    let global = core.global_object();
    let dictionary = retain_object(&core, &read(&core, global, "dict"));

    // the recorded `compound_dictionary` row, run by the script: `dict.n` was 1
    // and `dict.n += 2` left 3.
    assert_eq!(integer(&read(&core, dictionary, "n")), 3);

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:224-244 — the base object does
    // not serve the operator, so the caller reads the member, applies the
    // operator and writes the value back. The engine's own operation entry
    // answers the same rule the script ran.
    let answer = core
        .operation(
            dictionary,
            Some(&name("n")),
            Operation::Compound(Operator::Add),
            &Variant::from(2i64),
        )
        .expect("the operation is served");
    assert_eq!(integer(&answer), 5);
    assert_eq!(integer(&read(&core, dictionary, "n")), 5);
}

/// The global object publishes the classes the runtime holds, and a script that
/// deletes one leaves it absent.
#[test]
fn the_global_publishes_the_class_names_and_a_delete_removes_one() {
    let core = EngineCore::new();
    let global = core.global_object();

    // reference/krkr2/cpp/core/tjs2/tjs.cpp:178-190 — the class objects are
    // members of the global object, written with the ensure flag.
    let math = read(&core, global, "Math");
    assert!(matches!(math, Variant::Object(_)));

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1525-1536 — a member walk
    // answers the names the object holds.
    let declared = core.enum_members(global).expect("the global is enumerable");
    assert!(declared.contains(&name("Math")));
    assert!(declared.contains(&name("Exception")));

    core.delete_member(global, &name("Math"))
        .expect("the delete is served");
    assert!(
        !has(&core, global, "Math"),
        "the delete removed the class name"
    );
    let after = core.enum_members(global).expect("the global is enumerable");
    assert!(!after.contains(&name("Math")));
}

/// A class object constructs an instance, and the instance's members are the
/// class's.
#[test]
fn a_class_value_constructs_an_instance() {
    let core = EngineCore::new();
    let global = core.global_object();
    let class = retain_object(&core, &read(&core, global, "Exception"));

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1667-1750 — a construction is
    // the class value's own, and the argument list is the class's.
    let instance = core
        .create_new(class, &[Variant::from("boom")])
        .expect("the class constructs");

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1670-1676 — an object that is
    // not a class cannot be constructed from, and it refuses the call rather
    // than answering an object.
    assert!(matches!(
        core.create_new(instance, &[]),
        Err(CoreError::Object(_))
    ));

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1276-1311 — the instance holds
    // the member the constructor set.
    assert_eq!(text(&read(&core, instance, "message")), "boom");
    assert!(core.is_valid(instance).expect("the instance is usable"));
}

/// A held value keeps its object through the calls that follow, and a released
/// id is a typed refusal rather than a read.
#[test]
fn a_released_id_is_a_typed_refusal() {
    let core = EngineCore::new();
    run(&core, members_script(), "members");
    let global = core.global_object();
    let dictionary = retain_object(&core, &read(&core, global, "d"));
    assert_eq!(core.retained_count(), 2);

    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:476-500 — a copy takes its
    // own reference to the object, so the duplicate outlives this release.
    let copy = core.duplicate(dictionary).expect("the value is held");
    core.release(dictionary).expect("the value is held");
    assert_eq!(core.retained_count(), 2);
    assert_eq!(integer(&read(&core, copy, "a")), 4);

    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:378-380 — a released value
    // has let its object go, and nothing may read it.
    core.release(copy).expect("the value is held");
    assert_eq!(core.retained_count(), 1);
    assert!(matches!(
        core.member_read(copy, &name("a")),
        Err(CoreError::Retained(RetainError::Stale(id))) if id == copy
    ));
    assert!(matches!(core.release(copy), Err(RetainError::Stale(id)) if id == copy));
}

/// The engine answers an expression, and the block it compiled is the same
/// block the next call with the same text runs.
#[test]
fn an_expression_answers_and_a_named_script_is_compiled_once() {
    let core = EngineCore::new();

    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2602-2638 — the answer
    // of an arithmetic expression is the value the operator made.
    match core.eval("1 + 2", "expression") {
        Ok(value) => assert_eq!(integer(&value), 3),
        Err(error) => panic!("the expression did not answer: {error}"),
    }

    // a script under a name compiles once: the run that follows reuses the block
    // and the members it published
    run(&core, "var cached = 1;", "cached");
    run(&core, "var cached = 1;", "cached");
    let global = core.global_object();
    assert_eq!(integer(&read(&core, global, "cached")), 1);

    // the same name with other text is other script, and it runs
    run(&core, "var cached = 2;", "cached");
    assert_eq!(integer(&read(&core, global, "cached")), 2);
}

/// A value that is not callable is refused by the language's own failure, and a
/// script that does not compile is the front end's.
#[test]
fn a_non_callable_value_and_a_broken_script_are_typed_failures() {
    let core = EngineCore::new();
    let number = core.retain(Variant::from(1i64));

    // reference/krkr2/cpp/core/tjs2/tjsVariant.h:219-231 — a value with no
    // object cannot be called, and a value that is not a function refuses the
    // call.
    assert!(matches!(
        core.call_value(number, &[]),
        Err(CoreError::Run(_))
    ));

    // reference/krkr2/cpp/core/tjs2/tjsScriptBlock.cpp:444-446 — a compile that
    // counted an error raises it, and nothing stands in for the script.
    assert!(matches!(
        core.run("var = 1;", "broken"),
        Err(CoreError::Compile(_))
    ));

    // a member operation on a released id is the table's refusal, not a panic
    core.release(number).expect("the value is held");
    assert!(matches!(
        core.member_read(number, &name("a")),
        Err(CoreError::Retained(RetainError::Stale(_)))
    ));
}
