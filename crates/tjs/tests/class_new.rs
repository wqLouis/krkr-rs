//! `new C()` on a script class, in the Rust executor.
//!
//! A class constructs by making an object, running the class's own code in that
//! object and then calling the member the class is named after in it
//! (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2916-2931`). The code
//! gives the object the class names and the members the class carries
//! (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:1285-1293`,
//! `:2708-2745`), the constructor runs with the new object as its `this`, and
//! the answer is the object the class made
//! (`reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2117-2131`).
//!
//! Every case drives `tjs::vm::runtime::engine::run`, so the class, its code and
//! its instance are the Rust executor's own.

use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};

use tjs::Tjs2Engine;
use tjs::vm::runtime::engine::{EngineError, run};
use tjs::vm::runtime::exec::Fault;
use tjs::vm::runtime::exec::throw::Unwind;
use tjs::vm::runtime::exec::throw::message_of;
use tjs::vm::runtime::variant::Variant;

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Runs one script in the Rust executor.
fn run_script(script: &str) -> Result<Variant, EngineError> {
    let engine = Rc::new(Tjs2Engine::new().expect("the engine starts"));
    run(engine, script, "class_new")
}

/// The text a script answered.
fn answer(script: &str) -> String {
    match run_script(script) {
        Ok(value) => value
            .to_string()
            .expect("the answer has a text")
            .to_string(),
        Err(error) => panic!("the script failed with {error:?}\n{script}"),
    }
}

/// The raise a script made.
fn refusal(script: &str) -> Unwind {
    match run_script(script) {
        Ok(value) => panic!("the script answered {value:?} instead of raising\n{script}"),
        Err(EngineError::Run(unwind)) if matches!(unwind.fault(), Fault::Raised(_)) => unwind,
        Err(error) => panic!("the script failed with {error:?} instead of raising\n{script}"),
    }
}

/// A construction runs the class's code in the new object and then its
/// constructor there: the constructor's `this` is the instance, and the class's
/// members are the instance's.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2916-2931 (make the
/// object, run the class's code in it, call the class's own name in it);
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2708-2745 (each non-static
/// member is written onto the new object with its receiver rebound to it);
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2117-2131 (a construction
/// answers the object the class made).
#[test]
fn a_class_constructs_its_instance() {
    let _lock = vm_lock();
    assert_eq!(
        answer(
            r#"
class C {
    function C(x) { this.value = x; }
    function twice() { return this.value * 2; }
}
var o = new C(3);
return o.value + "|" + o.twice();
"#
        ),
        "3|6"
    );
}

/// Two constructions make two objects: a member one instance holds is not the
/// other's, including the members the class's body declares.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2920 (`new inherited()`:
/// every construction makes a new object);
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:2875-2881 (a class body's
/// `var` becomes a member of `this`, so it is written into the object being
/// made).
#[test]
fn two_instances_do_not_share_a_member() {
    let _lock = vm_lock();
    assert_eq!(
        answer(
            r#"
class Counter {
    var value;
    function Counter(x) { this.value = x; }
}
var a = new Counter(1);
var b = new Counter(2);
return a.value + "|" + b.value;
"#
        ),
        "1|2"
    );
}

/// A constructor that declares no argument is called with the empty list the
/// construction was given.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2924 (`FuncCall(0, Name,
/// …)` passes the construction's own argument list, which is empty here).
#[test]
fn a_constructor_takes_no_argument() {
    let _lock = vm_lock();
    assert_eq!(
        answer(
            r#"
class C {
    function C() { this.value = 7; }
}
var o = new C();
return o.value;
"#
        ),
        "7"
    );
}

/// A class with no constructor is constructible: the construction calls the
/// member the class is named after, and a class that declares no such member
/// still answers the object it made.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2924, :2930 (the call's
/// failure is not the construction's: the object is answered either way).
#[test]
fn a_class_without_a_constructor_constructs() {
    let _lock = vm_lock();
    assert_eq!(
        answer(
            r#"
class Empty {
    function ping() { return 5; }
}
var o = new Empty();
return o.ping();
"#
        ),
        "5"
    );
}

/// What a constructor returns is not the construction's answer: the object the
/// class made is.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2924-2931 (the
/// constructor's answer is passed no result slot, and the object is what the
/// entry answers);
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2122-2126 (the
/// construction's result register receives the object the class made).
#[test]
fn a_constructor_that_returns_answers_the_instance() {
    let _lock = vm_lock();
    assert_eq!(
        answer(
            r#"
class C {
    function C() {
        this.value = 1;
        return 42;
    }
    function plus() { return this.value + 100; }
}
var o = new C();
return typeof o + "|" + o.plus();
"#
        ),
        "Object|101"
    );
}

/// A function is not a class: constructing one is the invalid type, which the
/// engine reports as the language's own "not callable".
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2916-2918 (a context that
/// is not a class answers `TJS_E_INVALIDTYPE`);
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2139-2140 (a failed
/// construction raises the error it answered);
/// reference/krkr2/cpp/core/tjs2/tjsError.h:270 (its message).
#[test]
fn a_plain_function_is_not_a_constructor() {
    let _lock = vm_lock();
    let raise = refusal(
        r#"
function f() { }
var o = new f();
"#,
    );
    assert_eq!(
        raise.fault(),
        Fault::Raised(tjs::vm::runtime::exec::Raise::NotCallable),
        "the raise is the invalid type"
    );
    assert_eq!(
        message_of(&raise),
        "Not a function or invalid method/property type"
    );
}

/// A property declaration is not a class either: its call is the member it
/// stands for, and a construction is not that.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2916-2918 (only a class
/// constructs; a property answers `TJS_E_INVALIDTYPE`);
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2799-2800 (a property's
/// call is the invalid type too).
#[test]
fn a_property_is_not_a_constructor() {
    let _lock = vm_lock();
    let raise = refusal(
        r#"
property p {
    getter() { return 1; }
    setter(v) { }
}
var o = new p();
"#,
    );
    assert_eq!(
        raise.fault(),
        Fault::Raised(tjs::vm::runtime::exec::Raise::NotCallable),
        "the raise is the invalid type"
    );
}

/// A super class's members reach the instance: the class's own code calls the
/// super class with the new object as its `this`, so the super class's members
/// are published onto the instance, its body runs there, and the derived class's
/// constructor runs in that same object.
///
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeGen.cpp:3607-3617 (the class's code
/// rebinds the super class value's receiver to `this` and calls it);
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2795-2797 (a class's call
/// runs its code with the receiver it was handed);
/// reference/krkr2/cpp/core/tjs2/tjsInterCodeExec.cpp:2708-2745 (the members are
/// published onto that receiver, and the class body's `var` is written to it).
#[test]
fn an_instance_reaches_its_super_class_members() {
    let _lock = vm_lock();
    assert_eq!(
        answer(
            r#"
class Base {
    var kind = "base";
    function greet() { return this.kind; }
}
class Derived extends Base {
    function Derived(x) { this.extra = x; }
}
var o = new Derived(5);
return o.greet() + "|" + o.extra;
"#
        ),
        "base|5"
    );
}
