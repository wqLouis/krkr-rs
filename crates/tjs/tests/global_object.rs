//! The global scope as one object: its members, its class objects, and the
//! engine's global object behind it.
//!
//! Every case drives `tjs::vm::runtime::engine::run` — the Rust executor — and
//! asserts what a script sees. The object those calls run against is the scope's
//! `GlobalObject`: a `var` and a `function` a script declares are its members and
//! outlive the call, a class name it reads is published by the object's registry,
//! and a name neither holds is asked of the engine's global object, which is the
//! one source behind it.
//!
//! # Where the expectations come from
//!
//! The recorded answers were taken from the engine itself, one `exec_script` per
//! text, with the text this file runs. They are the oracle for the cases that
//! pin a language rule rather than a class's member table: what `typeof` makes
//! of an absent bare name, of an absent member and of a member that holds
//! nothing, and what `delete` answers for a name that is there and for one that
//! is not. The reference lines sit above the assertion they pin.

use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

use tjs::Tjs2Engine;
use tjs::vm::runtime::bridge::{SharedGlobals, global_members, scope_global};
use tjs::vm::runtime::engine::{ScriptFailure, run_with_members};
use tjs::vm::runtime::object::{Dispatch, MemberFlags};
use tjs::vm::runtime::string::TjsString;
use tjs::vm::runtime::variant::Variant;

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn engine() -> Rc<Tjs2Engine> {
    Rc::new(Tjs2Engine::new().expect("the engine starts"))
}

/// Runs one script on a scope of its own and answers the value it produced.
fn answer(script: &str) -> Variant {
    run_script(&engine(), script).unwrap_or_else(|failure| {
        panic!(
            "the script did not run: {} — {}\n{script}",
            failure.diagnostic(),
            failure.message()
        )
    })
}

/// Runs one script in a scope of its own on `engine`, so its failures carry the
/// frames and the line the script failed at.
fn run_script(engine: &Rc<Tjs2Engine>, script: &str) -> Result<Variant, ScriptFailure> {
    run_with_members(Rc::clone(engine), script, "global_object", global_members())
}

/// The text a script answered.
fn text(script: &str) -> String {
    match answer(script) {
        Variant::String(value) => value.to_string(),
        other => panic!("the answer is not a text: {other:?}\n{script}"),
    }
}

/// The integer a script answered.
fn integer(script: &str) -> i64 {
    match answer(script) {
        Variant::Integer(value) => value,
        other => panic!("the answer is not an integer: {other:?}\n{script}"),
    }
}

/// The message the engine gives for the script, which is the text a script can
/// catch.
fn failure(script: &str) -> String {
    match run_script(&engine(), script) {
        Ok(value) => panic!("the script answered {value:?} instead of failing\n{script}"),
        Err(failure) => failure.message(),
    }
}

/// An array a script constructs appends the value `add` gives it, and the count
/// is the number of elements.
///
/// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:846-862 — `add` appends its
/// argument and answers the position it stored it at;
/// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:988-1005 — `count` is the number
/// of elements the array holds;
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2117-2131 — `new C()` is
/// the class value's own construction, so the class object the global publishes
/// under `Array` is what makes the instance.
#[test]
fn a_constructed_array_appends_and_counts() {
    let _lock = vm_lock();
    assert_eq!(
        integer("var a = new Array(); return a.add(41);"),
        0,
        "the first element is stored at position zero"
    );
    assert_eq!(
        integer("var a = new Array(); a.add('x'); a.add('y'); return a.count;"),
        2,
        "count is the number of appended elements"
    );
    assert_eq!(
        text("var a = new Array(); a.add('x'); return a[0];"),
        "x",
        "the element reads back"
    );
}

/// A dictionary a script constructs answers the value written under a name.
///
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1352-1398 — a write with
/// `TJS_MEMBERENSURE` creates the member and stores the value;
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1276-1311 — a read answers the
/// value the member holds;
/// reference/krkr2/cpp/core/tjs2/tjsDictionary.cpp:735-743 — a name the
/// dictionary does not hold reads as the empty value unless the read requires
/// the member, and a read that requires it answers the absent name.
#[test]
fn a_constructed_dictionary_answers_a_written_key() {
    let _lock = vm_lock();
    assert_eq!(
        integer("var d = new Dictionary(); d['key'] = 5; return d['key'];"),
        5,
        "the written value reads back"
    );
    assert_eq!(
        text("var d = new Dictionary(); d.name = 'text'; return d.name;"),
        "text",
        "a dot write is the same member"
    );
    match answer("var d = new Dictionary(); return d['missing'];") {
        Variant::Void => {}
        other => panic!("a name the dictionary does not hold is void, not {other:?}"),
    }
    // the recorded answer: `typeof d['zz']` on a constructed dictionary is the
    // text `undefined`, because the read asks for the member it names
    assert_eq!(
        text("var d = new Dictionary(); return typeof d['zz'];"),
        "undefined"
    );
}

/// A `var` and a `function` a script declares at the top level are the global
/// object's members, and the next call in the same scope reads and calls both.
///
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1352-1398 — a write creates the
/// member, so the declaration leaves the value on the object;
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:886-949 — a declared
/// function is registered on the object that holds it, which at the top level
/// is the global object;
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2186-2192 — a call of the
/// function value runs its code with the receiver the value carries.
#[test]
fn a_declared_var_and_function_are_the_global_objects_members() {
    let _lock = vm_lock();
    let engine = engine();
    let globals = global_members();
    declare(&engine, &globals);
    let call = run_in_scope(&engine, &globals, "return next(value);");
    assert_eq!(call, 42, "both members are there, and the function runs");

    // the scope's global object is the object the calls ran against
    let global = scope_global(&globals).expect("the scope has a global object");
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1276-1311 — the object answers
    // the member the declaration wrote
    let declared = global
        .prop_get(&TjsString::from("value"), MemberFlags::NONE, None)
        .expect("the declared value is a member");
    assert!(
        matches!(declared, Variant::Integer(41)),
        "the global object answers the declared value, not {declared:?}"
    );
    let function = global
        .prop_get(&TjsString::from("next"), MemberFlags::NONE, None)
        .expect("the declared function is a member");
    assert!(
        matches!(function, Variant::Object(_)),
        "a declared function is an object, not {function:?}"
    );
    assert!(
        global.engine_id().is_some(),
        "the object stands for the engine's global object"
    );
}

/// A name only the engine's own code wrote is answered by the engine's global
/// object, which is the one source behind the scope's object.
///
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1276-1311 — a read answers the
/// member the object holds, and the engine's global object is where its own code
/// put the name;
/// reference/krkr2/cpp/core/tjs2/tjs.cpp:178-190 — the engine sets the names its
/// scripts read on the global object itself.
#[test]
fn a_name_only_the_engine_holds_is_read_from_the_engine() {
    let _lock = vm_lock();
    let engine = engine();
    engine
        .exec_script_discard("var engineOnly = 7;", "engine_only")
        .expect("the engine's own script runs");
    // the same engine, two scopes: the first reads the name with a member this
    // script declared, the second asks the engine for it on its own
    let globals = global_members();
    assert_eq!(
        run_in_scope(&engine, &globals, "var own = 1; return engineOnly + own;"),
        8,
        "the engine's value is read beside the scope's own"
    );
    assert_eq!(
        run_in_scope(&engine, &global_members(), "return engineOnly;"),
        7,
        "a scope of its own falls back to the same engine object"
    );
}

/// An absent name, a member that holds nothing and an absent member are three
/// answers, and the tests assert the recorded ones.
///
/// The recorded answers: `typeof absent` **raises** `Member "absent" does not
/// exist`, `typeof this.absent` is the text `undefined`, and a declared member
/// holding nothing is `void` in both forms. The two `typeof` forms differ
/// because a bare name is a plain member read — a read that raises when the name
/// is not there — while the member form has the absent answer of its own:
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1884-1906 answers the text
/// `undefined` for a member read that reports the member is not there, and
/// reference/krkr2/cpp/core/tjs2/tjsError.h:264 is the raise's text;
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2602-2638 — `typeof`
/// answers the kind's own spelling, and a member holding nothing is `void`.
#[test]
fn an_absent_name_and_a_void_member_are_three_answers() {
    let _lock = vm_lock();
    assert_eq!(
        text("var present = void; return typeof present;"),
        "void",
        "a declared member that holds nothing is void"
    );
    assert_eq!(
        text("var present = void; return typeof this.present;"),
        "void",
        "the same member through `this`"
    );
    assert_eq!(
        text("return typeof this.absent;"),
        "undefined",
        "the member form answers the absent name"
    );
    assert_eq!(
        failure("return typeof absent;"),
        "Member \"absent\" does not exist",
        "the bare form is a plain read, and a plain absent read raises"
    );
}

/// A bare assignment is a member write with **no** `TJS_MEMBERENSURE`, so the
/// engine refuses it when the name does not exist; a declared name and the
/// member form are written.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2049-2056 — a bare
/// identifier in an assignment position compiles to `SPD` (`SPDE` only for
/// `this.name = value`); reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1352-1398
/// — a write without the ensuring flag on a name the object does not hold is
/// `TJS_E_MEMBERNOTFOUND`, and `tjsError.cpp:259-266` renders it with the name.
///
/// The engine's own answers for the same texts are recorded in the assertion
/// that compares them: this is a differential, not a reading of the C++.
#[test]
fn a_bare_assignment_writes_a_name_that_exists_and_only_that() {
    let _lock = vm_lock();
    let engine = engine();
    let engine_answer = |script: &str| match engine.exec_script_discard(script, "global_object") {
        Ok(()) => String::from("written"),
        Err(error) => error.to_string(),
    };
    assert_eq!(
        engine_answer("x = 1;"),
        "TJS error: global_object(1): Member \"x\" does not exist",
        "the engine refuses a bare assignment to an absent name"
    );
    assert_eq!(
        failure("x = 1;"),
        "Member \"x\" does not exist",
        "the Rust executor answers the engine's text"
    );
    assert_eq!(
        engine_answer("var v = 0; v = 1;"),
        "written",
        "the engine writes a name that exists"
    );
    assert_eq!(
        integer("var v = 0; v = 1; return v;"),
        1,
        "the Rust executor writes the declared name"
    );
    assert_eq!(
        engine_answer("this.y = 1;"),
        "written",
        "the member form carries the ensuring flag"
    );
    assert_eq!(
        integer("this.y = 1; return y;"),
        1,
        "the member the member form created is the global's"
    );
    // the value's kind does not decide the rule: a bare assignment of an
    // object is the same non-ensuring write, and the engine refuses it too
    assert_eq!(
        engine_answer("o = %[a:1];"),
        "TJS error: global_object(1): Member \"o\" does not exist",
        "the engine refuses a bare assignment of a dictionary"
    );
    assert_eq!(
        failure("o = %[a:1];"),
        "Member \"o\" does not exist",
        "the Rust executor answers the same text"
    );
}

/// A class name the script writes is the object's own member from then on, so
/// the registry answers it only while the object does not.
///
/// reference/krkr2/cpp/core/tjs2/tjs.cpp:178-190 — a class object is a member of
/// the global object, set with the ensure flag;
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1352-1398 — a write replaces the
/// member it names.
#[test]
fn a_class_name_the_script_writes_shadows_the_class_object() {
    let _lock = vm_lock();
    assert_eq!(
        text("return typeof Math;"),
        "Object",
        "the registry publishes the class object"
    );
    assert_eq!(
        text("Math = 3; return typeof Math;"),
        "Integer",
        "the write is the object's member, in front of the class object"
    );
}

/// A delete answers whether it removed the member, and the name is absent
/// afterwards — through the member form, while the bare form raises.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1806-1826 — `delete
/// a.b` answers `true` when the member was removed, `false` when the delete
/// refused, and does not raise;
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1537-1571 — the delete removes
/// the member, and a read after it is the missing member.
#[test]
fn a_deleted_class_name_stays_deleted() {
    let _lock = vm_lock();
    // the recorded answers: the delete answers 1, the member form answers
    // `undefined`, and the bare form raises the missing-member text
    assert_eq!(
        integer("var removed = delete this.Math; return removed;"),
        1,
        "the delete answers that it removed the name"
    );
    assert_eq!(
        text("var removed = delete this.Math; return typeof this.Math;"),
        "undefined",
        "the class name is gone after the delete"
    );
    assert_eq!(
        failure("var removed = delete this.Math; return typeof Math;"),
        "Member \"Math\" does not exist",
        "the bare form raises after the name is gone"
    );
}

/// A named call on the global object reads the member and calls the value, which
/// is how a top-level function is callable as `this.f(...)` and as `f(...)`.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2117-2131 — a named call
/// looks the member up and calls the value it reached with no name of its own;
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2916-2931 — a function
/// whose call is code runs that code.
#[test]
fn a_named_call_on_the_global_object_calls_a_declared_function() {
    let _lock = vm_lock();
    assert_eq!(
        integer("function twice(x) { return x * 2; } return this.twice(21);"),
        42,
        "the named call through `this` runs the declared function"
    );
    assert_eq!(
        integer("function twice(x) { return x * 2; } return twice(21);"),
        42,
        "the bare form reaches the same object"
    );
}

/// Declares the members the two-call case reads, in the shared scope.
fn declare(engine: &Rc<Tjs2Engine>, globals: &SharedGlobals) {
    match run_with_members(
        Rc::clone(engine),
        "var value = 41; function next(x) { return x + 1; }",
        "declaration",
        Rc::clone(globals),
    ) {
        Ok(_) => {}
        Err(failure) => panic!("the declaration did not run: {failure:?}"),
    }
}

/// Runs one script in the scope `globals` and answers its integer result.
fn run_in_scope(engine: &Rc<Tjs2Engine>, globals: &SharedGlobals, script: &str) -> i64 {
    match run_with_members(Rc::clone(engine), script, "call", Rc::clone(globals)) {
        Ok(Variant::Integer(answer)) => answer,
        Ok(other) => panic!("the call answered {other:?}\n{script}"),
        Err(failure) => panic!(
            "the call did not run: {} — {}\n{script}",
            failure.diagnostic(),
            failure.message()
        ),
    }
}
