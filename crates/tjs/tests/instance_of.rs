//! `instanceof` against a class object, against a class name and against a
//! value of another kind.
//!
//! The entry converts its right operand to text and matches that text against
//! the class names the left operand carries
//! (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1147-1150`: `CHKINS`
//! hands the right operand to `InstanceOf`;
//! `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705`: `AsString`
//! on the right operand, `TJSDefaultIsInstanceOf` with that text, and the
//! answer is whether the comparison was true). An object's text form is its
//! handle form, `(object %p:%p)`, never a class's name
//! (`reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213`), so a class object
//! on the right answers 0 even when the instance carries that class's name; the
//! same name written as text answers what the instance's chain holds
//! (`reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1759-1781`: a custom object
//! matches "Object" or one of its class names). A value that is not an object
//! answers by its kind, and `void` answers 0 before any name is compared
//! (`reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1712-1756`).
//!
//! # The names a kind answers
//!
//! The kind switch answers `"Object"` for every kind but the empty value, then
//! exactly one name for the kind: an integer and a real are `"Number"`, a text
//! is `"String"`, an octet is `"Octet"`. It names no other class, so the
//! `typeof` spellings `"Integer"` and `"Real"` name no instance even though the
//! engine prints them (`reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1728-1740`;
//! `reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2601-2635` is the
//! `typeof` spelling, a different question).
//!
//! # A native instance carries its class name
//!
//! A native construction adds the class's own name to the object it makes
//! (`reference/krkr2/cpp/core/tjs2/tjsNative.cpp:323-325`), so
//! `(new Exception("m")) instanceof "Exception"` is 1 while the class object
//! written without quotes is 0: the 0 is the operand rule above, not a missing
//! name.
//!
//! Every case runs twice: the engine behind the FFI answers first, and the Rust
//! executor (`tjs::vm::runtime::engine::run`) must answer the same. The engine's
//! answers for the native classes are also what
//! `tests/fixtures/array_members.txt` and `tests/fixtures/exception_members.txt`
//! record.

use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

use tjs::Tjs2Engine;
use tjs::TjsValue;
use tjs::vm::runtime::engine::run;
use tjs::vm::runtime::variant::Variant;

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// The integer the engine answers for a script.
fn engine_answer(engine: &Tjs2Engine, script: &str, what: &str) -> i64 {
    match engine.exec_script(script, "instance_of") {
        Ok(TjsValue::Integer(answer)) => answer,
        Ok(other) => panic!("{what}: the engine answered {other:?}"),
        Err(error) => panic!("{what}: the engine raised {error}"),
    }
}

/// The integer the Rust executor answers for the same script.
fn rust_answer(engine: &Rc<Tjs2Engine>, script: &str, what: &str) -> i64 {
    match run(Rc::clone(engine), script, "instance_of") {
        Ok(Variant::Integer(answer)) => answer,
        Ok(other) => panic!("{what}: the Rust executor answered {other:?}"),
        Err(error) => panic!("{what}: the Rust executor failed {error:?}"),
    }
}

/// Checks one script against both executors, which must answer the record.
fn both(engine: &Tjs2Engine, rust: &Rc<Tjs2Engine>, script: &str, recorded: i64, what: &str) {
    assert_eq!(
        engine_answer(engine, script, what),
        recorded,
        "{what}: the engine's answer"
    );
    assert_eq!(
        rust_answer(rust, script, what),
        recorded,
        "{what}: the Rust executor's answer"
    );
}

/// A script class's instance answers 0 against the class object: the class
/// object's text is its handle form, and the class name the instance carries is
/// what that text is compared with.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1147-1150, `:2684-2705`
/// (`CHKINS` converts the right operand to text and compares it);
/// reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213 (an object's text form
/// is `(object %p:%p)`).
#[test]
fn a_script_class_object_is_not_its_class_name() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    both(
        &engine,
        &rust,
        "class ScriptClassObject { }\n\
         return (new ScriptClassObject()) instanceof ScriptClassObject;",
        0,
        "a script class object",
    );
}

/// The same instance answers 1 against its class's name written as text: the
/// class's code added that name to the object it made, and the text is what the
/// entry compares.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1285-1293, `:2245-2254`
/// (a class's body adds its own name to the instance);
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1759-1781 (a custom object
/// matches a class name it carries).
#[test]
fn a_script_class_name_as_text_names_its_instance() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    both(
        &engine,
        &rust,
        "class ScriptClassText { }\n\
         return (new ScriptClassText()) instanceof \"ScriptClassText\";",
        1,
        "a script class name as text",
    );
}

/// A native class's instance answers 0 against its class object, for the same
/// reason a script class's does, and the recorded answers of the array and the
/// exception are those 0s.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705;
/// reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213;
/// tests/fixtures/array_members.txt (`instanceof_array` is 0);
/// tests/fixtures/exception_members.txt (`value_instance_of` is 0).
#[test]
fn a_native_class_object_is_not_its_class_name() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    both(
        &engine,
        &rust,
        "return (new Array()) instanceof Array;",
        0,
        "the array's class object",
    );
    both(
        &engine,
        &rust,
        "return (new Exception(\"m\")) instanceof Exception;",
        0,
        "the exception's class object",
    );
    both(
        &engine,
        &rust,
        "return (new Dictionary()) instanceof Dictionary;",
        0,
        "the dictionary's class object",
    );
}

/// A native class's instance answers 1 against its class's name as text: the
/// construction added the name, and the text matches it.
///
/// reference/krkr2/cpp/core/tjs2/tjsNative.cpp:323-325 (a native class's
/// construction adds its class name to the object);
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1759-1781 (a custom object
/// matches a carried class name);
/// tests/fixtures/dictionary_members.txt (`own_class` is 1).
#[test]
fn a_native_class_name_as_text_names_its_instance() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    both(
        &engine,
        &rust,
        "return (new Array()) instanceof \"Array\";",
        1,
        "the array's class name as text",
    );
    both(
        &engine,
        &rust,
        "return (new Dictionary()) instanceof \"Dictionary\";",
        1,
        "the dictionary's class name as text",
    );
    both(
        &engine,
        &rust,
        "return (new Dictionary()) instanceof \"Object\";",
        1,
        "the dictionary's object name as text",
    );
}

/// A dictionary read that names no member is `void`, and `void` answers 0
/// against a class object and against "Object" alike: the kind is answered
/// before any name is compared.
///
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1712-1719 (a void value is false
/// before the name is read);
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705.
#[test]
fn a_void_value_is_no_instance() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    both(
        &engine,
        &rust,
        "var missing = (new Dictionary())[\"missing\"];\n\
         return missing instanceof Array;",
        0,
        "a void value against a class object",
    );
    both(
        &engine,
        &rust,
        "var missing = (new Dictionary())[\"missing\"];\n\
         return missing instanceof \"Object\";",
        0,
        "a void value against the object name",
    );
}

/// A value of another kind answers 0 against a name of a kind it is not: a text
/// is no `Number`, and a number is no `String`.
///
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1728-1736 (an integer matches
/// only "Number" and a text only "String").
#[test]
fn a_non_object_value_is_no_instance_of_another_kind() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    both(
        &engine,
        &rust,
        "return \"a\" instanceof \"Number\";",
        0,
        "a text against the number name",
    );
    both(
        &engine,
        &rust,
        "return 1 instanceof \"String\";",
        0,
        "a number against the text name",
    );
}

/// A value of a kind answers that kind's own name: an integer and a real are
/// both `"Number"`, a text is `"String"`, and every kind but the empty value is
/// `"Object"`.
///
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1717-1722 (the empty value is
/// false, every other kind is true for "Object");
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1728-1736 (an integer and a real
/// answer "Number", a text answers "String").
#[test]
fn a_kind_answers_the_name_of_its_kind() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    both(
        &engine,
        &rust,
        "return 1 instanceof \"Number\";",
        1,
        "an integer against the number name",
    );
    both(
        &engine,
        &rust,
        "return 1.5 instanceof \"Number\";",
        1,
        "a real against the number name",
    );
    both(
        &engine,
        &rust,
        "return \"a\" instanceof \"String\";",
        1,
        "a text against the text name",
    );
    both(
        &engine,
        &rust,
        "return 1 instanceof \"Object\";",
        1,
        "an integer against the object name",
    );
    both(
        &engine,
        &rust,
        "return 1.5 instanceof \"Object\";",
        1,
        "a real against the object name",
    );
    both(
        &engine,
        &rust,
        "return \"a\" instanceof \"Object\";",
        1,
        "a text against the object name",
    );
}

/// The `typeof` spellings name no instance: a number is no `"Integer"` and no
/// `"Real"`, and the names are compared exactly, so the lower-case spelling is
/// no name either. An octet is no `"Octet"`-less kind, and a number is no
/// `"Octet"`.
///
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1728-1740 (the switch names
/// "Number" for both numeric kinds and "Octet" for the octet, and nothing
/// else);
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2601-2635 (the `typeof`
/// spellings "Integer" and "Real" are that instruction's, not a class name).
#[test]
fn a_typeof_spelling_names_no_instance() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    both(
        &engine,
        &rust,
        "return 1 instanceof \"Integer\";",
        0,
        "an integer against its typeof spelling",
    );
    both(
        &engine,
        &rust,
        "return 1.5 instanceof \"Integer\";",
        0,
        "a real against the integer spelling",
    );
    both(
        &engine,
        &rust,
        "return 1 instanceof \"Real\";",
        0,
        "an integer against the real spelling",
    );
    both(
        &engine,
        &rust,
        "return 1 instanceof \"number\";",
        0,
        "a name is compared exactly",
    );
    both(
        &engine,
        &rust,
        "return \"a\" instanceof \"Octet\";",
        0,
        "a text against the octet name",
    );
    both(
        &engine,
        &rust,
        "return 1 instanceof \"Octet\";",
        0,
        "a number against the octet name",
    );
}

/// A native instance carries its class's name, which is what the name written
/// as text matches; the name is the class's own and nothing else, and a class
/// object written without quotes is still 0.
///
/// reference/krkr2/cpp/core/tjs2/tjsNative.cpp:323-325 (a native construction
/// adds the class name to the object it makes);
/// reference/krkr2/cpp/core/tjs2/tjsException.cpp:22-52 (the class's name and
/// its two-argument construction).
#[test]
fn a_native_instance_carries_its_class_name() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    both(
        &engine,
        &rust,
        "return (new Exception(\"m\")) instanceof \"Exception\";",
        1,
        "an exception against its class name",
    );
    both(
        &engine,
        &rust,
        "return (new Exception()) instanceof \"Exception\";",
        1,
        "an exception with no argument against its class name",
    );
    both(
        &engine,
        &rust,
        "return (new Exception(1)) instanceof \"Exception\";",
        1,
        "an integer message does not change the class name",
    );
    both(
        &engine,
        &rust,
        "return (new Exception(\"m\")) instanceof \"Object\";",
        1,
        "an exception against the object name",
    );
    both(
        &engine,
        &rust,
        "return (new Exception(\"m\")) instanceof \"Error\";",
        0,
        "an exception against a name its class does not carry",
    );
    both(
        &engine,
        &rust,
        "return (new Exception(\"m\")) instanceof \"exception\";",
        0,
        "a name is compared exactly",
    );
    both(
        &engine,
        &rust,
        "return (new Exception()) instanceof \"Function\";",
        0,
        "an exception is no function",
    );
}

/// The exception a handler receives carries the class name too, whether the
/// script threw it or the engine raised it, and the class object written
/// without quotes still answers 0.
///
/// reference/krkr2/cpp/core/tjs2/tjsNative.cpp:323-325;
/// reference/krkr2/cpp/core/tjs2/tjsError.cpp:26-54 (a failure becomes an
/// Exception object carrying the message and the trace);
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705 (the right
/// operand is the class's text form, not its name).
#[test]
fn a_thrown_exception_carries_its_class_name() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    both(
        &engine,
        &rust,
        "try { throw new Exception(\"m\"); } catch (e) { return e instanceof \"Exception\"; }",
        1,
        "a caught exception against its class name",
    );
    both(
        &engine,
        &rust,
        "try { throw new Exception(\"m\"); } catch (e) { return e instanceof \"Object\"; }",
        1,
        "a caught exception against the object name",
    );
    both(
        &engine,
        &rust,
        "try { throw new Exception(\"m\"); } catch (e) { return e instanceof Exception; }",
        0,
        "a caught exception against its class object",
    );
    both(
        &engine,
        &rust,
        "try { var zero = 0; return 7 \\ zero; }\n\
         catch (e) { return e instanceof \"Exception\"; }",
        1,
        "an engine failure's object against the class name",
    );
}

/// The identity matrix: every value kind and every container against the class
/// names it does and does not carry, and the class object on the right.
///
/// The default rule answers `"Object"` for every kind but the empty value, and
/// exactly one name for the kind (`"Number"`, `"String"`, `"Octet"`); an object
/// answers `"Object"` or one of the class names it carries. The right operand is
/// converted to text first, so a class object is its handle text and names no
/// class. The array forwards a named class question to its member map, which is
/// the same rule a dictionary already had.
#[test]
fn the_identity_matrix_is_the_engine_s() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));

    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1750-1758 (a named class
    // question is the custom object's); reference/krkr2/cpp/core/tjs2/tjsNative.cpp:323-325
    // (the construction adds the class name).
    both(
        &engine,
        &rust,
        "return (new Array()) instanceof \"Array\";",
        1,
        "an array against its class name",
    );
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1721-1722 (every value but
    // the empty one is an Object); reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1767-1781
    // (a custom object answers the object name before its class names).
    both(
        &engine,
        &rust,
        "return (new Array()) instanceof \"Object\";",
        1,
        "an array against the object name",
    );
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1728-1740 (a kind answers
    // only its own name); reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1767-1781
    // (a custom object answers no name it does not carry).
    both(
        &engine,
        &rust,
        "return (new Array()) instanceof \"Dictionary\";",
        0,
        "an array against the dictionary name",
    );
    both(
        &engine,
        &rust,
        "return (new Array()) instanceof \"String\";",
        0,
        "an array against the text name",
    );
    both(
        &engine,
        &rust,
        "return (new Array()) instanceof \"Number\";",
        0,
        "an array against the number name",
    );
    both(
        &engine,
        &rust,
        "return (new Array()) instanceof \"Octet\";",
        0,
        "an array against the octet name",
    );
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705 (the right
    // operand is converted to text); reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213
    // (a class object's text is its handle, not its class name).
    both(
        &engine,
        &rust,
        "return (new Array()) instanceof Array;",
        0,
        "an array against the array class object",
    );
    // reference/krkr2/cpp/core/tjs2/tjsNative.cpp:323-325 (a native
    // construction adds the class name, so split, a literal and a construction
    // all carry it).
    both(
        &engine,
        &rust,
        "return (\"a,b\".split(\",\")) instanceof \"Array\";",
        1,
        "a split array against its class name",
    );
    both(
        &engine,
        &rust,
        "return ([] instanceof \"Array\");",
        1,
        "an array literal against its class name",
    );
    both(
        &engine,
        &rust,
        "return (new Array()) instanceof \"Array\";",
        1,
        "a constructed array against its class name",
    );

    // reference/krkr2/cpp/core/tjs2/tjsNative.cpp:323-325 (a dictionary
    // construction adds the class name); reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1767-1781
    // (a custom object answers the names it carries).
    both(
        &engine,
        &rust,
        "return (new Dictionary()) instanceof \"Dictionary\";",
        1,
        "a dictionary against its class name",
    );
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1721-1722 (every value but
    // the empty one is an Object).
    both(
        &engine,
        &rust,
        "return (new Dictionary()) instanceof \"Object\";",
        1,
        "a dictionary against the object name",
    );
    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1767-1781 (a name the object
    // does not carry is false).
    both(
        &engine,
        &rust,
        "return (new Dictionary()) instanceof \"Array\";",
        0,
        "a dictionary against the array name",
    );
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705;
    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213 (the class object's
    // handle text names no class).
    both(
        &engine,
        &rust,
        "return (new Dictionary()) instanceof Dictionary;",
        0,
        "a dictionary against the dictionary class object",
    );
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705;
    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213 (the class object's
    // handle text names no class).
    both(
        &engine,
        &rust,
        "return (new Dictionary()) instanceof Array;",
        0,
        "a dictionary against the array class object",
    );

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1767-1781 (a plain object
    // carries the class its construction added and the object name).
    both(
        &engine,
        &rust,
        "class PlainArray { }\nreturn (new PlainArray()) instanceof \"Array\";",
        0,
        "a plain object against the array name",
    );
    both(
        &engine,
        &rust,
        "class PlainObject { }\nreturn (new PlainObject()) instanceof \"Object\";",
        1,
        "a plain object against the object name",
    );
    both(
        &engine,
        &rust,
        "class PlainNamed { }\nreturn (new PlainNamed()) instanceof \"PlainNamed\";",
        1,
        "a plain object against its class name",
    );
    both(
        &engine,
        &rust,
        "class PlainClass { }\nreturn (new PlainClass()) instanceof PlainClass;",
        0,
        "a plain object against its class object",
    );
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705;
    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213 (the class object's
    // handle text names no class).
    both(
        &engine,
        &rust,
        "class PlainArrayClass { }\nreturn (new PlainArrayClass()) instanceof Array;",
        0,
        "a plain object against the array class object",
    );

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1728-1740 (a text answers
    // "String" and no other kind's name); reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1721-1722
    // (a text is an Object).
    both(
        &engine,
        &rust,
        "return \"text\" instanceof \"String\";",
        1,
        "a text against its own name",
    );
    both(
        &engine,
        &rust,
        "return \"text\" instanceof \"Object\";",
        1,
        "a text against the object name",
    );
    both(
        &engine,
        &rust,
        "return \"text\" instanceof \"Array\";",
        0,
        "a text against the array name",
    );
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705;
    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213 (a class object's
    // handle text names no class).
    both(
        &engine,
        &rust,
        "return \"text\" instanceof Array;",
        0,
        "a text against the array class object",
    );

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1728-1740 (an octet answers
    // "Octet" and no other kind's name); reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1721-1722
    // (an octet is an Object).
    both(
        &engine,
        &rust,
        "var value = <% 00 AB %>;\nreturn value instanceof \"Octet\";",
        1,
        "an octet against its own name",
    );
    both(
        &engine,
        &rust,
        "var value = <% 00 AB %>;\nreturn value instanceof \"Object\";",
        1,
        "an octet against the object name",
    );
    both(
        &engine,
        &rust,
        "var value = <% 00 AB %>;\nreturn value instanceof \"Array\";",
        0,
        "an octet against the array name",
    );
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705;
    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213 (the class object's
    // handle text names no class).
    both(
        &engine,
        &rust,
        "var value = <% 00 AB %>;\nreturn value instanceof Array;",
        0,
        "an octet against the array class object",
    );

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1712-1719 (the empty value
    // is false before any name is compared).
    both(
        &engine,
        &rust,
        "var value;\nreturn value instanceof \"Array\";",
        0,
        "the empty value against the array name",
    );
    both(
        &engine,
        &rust,
        "var value;\nreturn value instanceof \"Object\";",
        0,
        "the empty value against the object name",
    );
    both(
        &engine,
        &rust,
        "var value;\nreturn value instanceof \"String\";",
        0,
        "the empty value against the text name",
    );
    both(
        &engine,
        &rust,
        "var value;\nreturn value instanceof \"Number\";",
        0,
        "the empty value against the number name",
    );
    both(
        &engine,
        &rust,
        "var value;\nreturn value instanceof \"Octet\";",
        0,
        "the empty value against the octet name",
    );
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705;
    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213 (the class object's
    // handle text names no class).
    both(
        &engine,
        &rust,
        "var value;\nreturn value instanceof Array;",
        0,
        "the empty value against the array class object",
    );

    // reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1728-1732 (an integer
    // answers "Number" and no other kind's name); reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1721-1722
    // (an integer is an Object).
    both(
        &engine,
        &rust,
        "return 1 instanceof \"Number\";",
        1,
        "an integer against its own name",
    );
    both(
        &engine,
        &rust,
        "return 1 instanceof \"Object\";",
        1,
        "an integer against the object name",
    );
    both(
        &engine,
        &rust,
        "return 1 instanceof \"Array\";",
        0,
        "an integer against the array name",
    );
    // reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2684-2705;
    // reference/krkr2/cpp/core/tjs2/tjsVariant.cpp:187-213 (the class object's
    // handle text names no class).
    both(
        &engine,
        &rust,
        "return 1 instanceof Array;",
        0,
        "an integer against the array class object",
    );
}

/// An invalidated array raises the engine's invalid-object error for a class
/// name, while `"Object"` still answers true before the object is asked.
///
/// reference/krkr2/cpp/core/tjs2/tjsArray.cpp:1750-1758 (the named question is
/// the custom object's); reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1764-1765
/// (the custom object checks its validity first);
/// reference/krkr2/cpp/core/tjs2/tjsObject.cpp:1721-1722 (the object name is
/// answered before the object is reached).
#[test]
fn an_invalidated_array_refuses_its_class_name() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("the engine starts");
    let rust = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    let script = "var value = new Array(); invalidate value; \
                  return value instanceof \"Array\";";
    assert!(
        engine.exec_script(script, "instance_of").is_err(),
        "the engine raises for an invalidated array's class name"
    );
    assert!(
        run(Rc::clone(&rust), script, "instance_of").is_err(),
        "the Rust executor raises for an invalidated array's class name"
    );
    both(
        &engine,
        &rust,
        "var value = new Array(); invalidate value; \
         return value instanceof \"Object\";",
        1,
        "an invalidated array against the object name",
    );
}
