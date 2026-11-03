//! The native classes' instance members reach their instances.
//!
//! A native class registers its members with the class object; the members
//! that are not static are the instance members, and every instance of the
//! class carries them
//! (`reference/cpp/core/tjs2/tjsNative.h:343` registers a member without the
//! static flag, `reference/cpp/core/tjs2/tjsNative.h:355` registers one with
//! it, and the class hands the non-static ones to the instance it creates at
//! `reference/cpp/core/tjs2/tjsNative.cpp:344-352`).
//!
//! The member belongs to the instance: the reference binds the instance as
//! the member's "objthis" when it copies the member into the instance
//! (`reference/cpp/core/tjs2/tjsNative.cpp:348-352`), so a native property
//! reads and writes the state of the instance it was read from.

use std::sync::{Mutex, MutexGuard};

use tjs::{Tjs2Engine, TjsValue};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Reads an integer member of the global object.
fn int_member(engine: &Tjs2Engine, expression: &str) -> i64 {
    match engine.eval(expression, "native_instance_members") {
        Ok(TjsValue::Integer(value)) => value,
        Ok(other) => panic!("expected an integer, got {other:?}"),
        Err(error) => panic!("`{expression}` must evaluate: {error}"),
    }
}

/// Reads a string member of the global object.
fn string_member(engine: &Tjs2Engine, expression: &str) -> String {
    match engine.eval(expression, "native_instance_members") {
        Ok(TjsValue::String(value)) => value,
        Ok(other) => panic!("expected a string, got {other:?}"),
        Err(error) => panic!("`{expression}` must evaluate: {error}"),
    }
}

/// Asserts that a member of the global object reads as void.
fn void_member(engine: &Tjs2Engine, expression: &str) {
    match engine.eval(expression, "native_instance_members") {
        Ok(TjsValue::Void) => {}
        Ok(other) => panic!("expected void, got {other:?}"),
        Err(error) => panic!("`{expression}` must evaluate: {error}"),
    }
}

/// The Array class declares its members as instance members
/// (`reference/cpp/core/tjs2/tjsArray.cpp:225-1033`): an array carries the
/// methods and the properties the class declares.
#[test]
fn an_array_carries_the_class_instance_members() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    engine
        .exec_script(
            "var a = [3, 1, 2]; \
             var countBefore = a.count; \
             var lengthBefore = a.length; \
             a.push(4); \
             var countAfter = a.count; \
             a.sort(); \
             var first = a[0];",
            "native_instance_members",
        )
        .expect("the script must run");

    assert_eq!(int_member(&engine, "countBefore"), 3);
    assert_eq!(int_member(&engine, "lengthBefore"), 3);
    assert_eq!(int_member(&engine, "countAfter"), 4);
    assert_eq!(int_member(&engine, "first"), 1);
}

/// The member belongs to the instance that holds it: two arrays of different
/// sizes read their own size. The property reads the state of the instance
/// through its "objthis" (`reference/cpp/core/tjs2/tjsNative.cpp:348-352`).
#[test]
fn an_instance_member_is_bound_to_its_own_instance() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    engine
        .exec_script(
            "var small = [1]; \
             var large = [1, 2, 3, 4]; \
             var smallCount = small.count; \
             var largeCount = large.count;",
            "native_instance_members",
        )
        .expect("the script must run");

    assert_eq!(int_member(&engine, "smallCount"), 1);
    assert_eq!(int_member(&engine, "largeCount"), 4);
}

/// Every RegExp instance member reads back what that instance's own `exec`
/// produced.
///
/// The reference compiles the pattern with oniguruma in Perl syntax
/// (`reference/cpp/core/tjs2/tjsRegExp.cpp:647-651`), so a capture group
/// matches what it spells: `z(q)` matches "zq", not "zz". The search and the
/// result array follow the reference (`reference/cpp/core/tjs2/tjsRegExp.cpp:660-679`
/// searches, `reference/cpp/core/tjs2/tjsRegExp.cpp:681-687` stores the
/// result array on the instance, `reference/cpp/core/tjs2/tjsRegExp.cpp:727-754`
/// fills it with the matched groups), and a target that does not match leaves
/// the array empty — reading an index past the end of an array answers void
/// (`reference/cpp/core/tjs2/tjsArray.cpp:1389-1396`).
#[test]
fn every_regexp_instance_member_reads_back_its_own_exec() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    engine
        .exec_script(
            "var first = new RegExp('a(b)'); \
             var second = new RegExp('z(q)'); \
             first.exec('ab'); \
             second.exec('zq'); \
             var firstMatchCount = first.matches.count; \
             var secondMatchCount = second.matches.count; \
             var firstMatch = first.matches[0]; \
             var secondMatch = second.matches[0]; \
             var firstGroup = first.matches[1]; \
             var secondGroup = second.matches[1]; \
             var firstInput = first.input; \
             var secondInput = second.input; \
             var firstIndex = first.index; \
             var secondIndex = second.index; \
             var firstStart = first.start; \
             var secondStart = second.start; \
             var firstLastIndex = first.lastIndex; \
             var secondLastIndex = second.lastIndex; \
             var firstLastMatch = first.lastMatch; \
             var secondLastMatch = second.lastMatch; \
             var firstLastParen = first.lastParen; \
             var secondLastParen = second.lastParen; \
             var firstLeftContext = first.leftContext; \
             var secondLeftContext = second.leftContext; \
             var firstRightContext = first.rightContext; \
             var secondRightContext = second.rightContext; \
             var firstMatchMethod = first.match('ab')[0]; \
             var secondMatchMethod = second.match('zq')[0]; \
             var firstReplace = first.replace('ab', 'X'); \
             var secondReplace = second.replace('zq', 'Y');",
            "native_instance_members",
        )
        .expect("the script must run");

    // The match array: the whole match, then the groups.
    assert_eq!(int_member(&engine, "firstMatchCount"), 2);
    assert_eq!(int_member(&engine, "secondMatchCount"), 2);
    assert_eq!(string_member(&engine, "firstMatch"), "ab");
    assert_eq!(string_member(&engine, "secondMatch"), "zq");
    assert_eq!(string_member(&engine, "firstGroup"), "b");
    assert_eq!(string_member(&engine, "secondGroup"), "q");

    // The state the instance's own exec produced.
    assert_eq!(string_member(&engine, "firstInput"), "ab");
    assert_eq!(string_member(&engine, "secondInput"), "zq");
    assert_eq!(int_member(&engine, "firstIndex"), 0);
    assert_eq!(int_member(&engine, "secondIndex"), 0);
    assert_eq!(int_member(&engine, "firstStart"), 0);
    assert_eq!(int_member(&engine, "secondStart"), 0);
    assert_eq!(int_member(&engine, "firstLastIndex"), 2);
    assert_eq!(int_member(&engine, "secondLastIndex"), 2);
    assert_eq!(string_member(&engine, "firstLastMatch"), "ab");
    assert_eq!(string_member(&engine, "secondLastMatch"), "zq");
    assert_eq!(string_member(&engine, "firstLastParen"), "b");
    assert_eq!(string_member(&engine, "secondLastParen"), "q");
    assert_eq!(string_member(&engine, "firstLeftContext"), "");
    assert_eq!(string_member(&engine, "secondLeftContext"), "");
    assert_eq!(string_member(&engine, "firstRightContext"), "");
    assert_eq!(string_member(&engine, "secondRightContext"), "");

    // The methods read the same state of the same instance.
    assert_eq!(string_member(&engine, "firstMatchMethod"), "ab");
    assert_eq!(string_member(&engine, "secondMatchMethod"), "zq");
    assert_eq!(string_member(&engine, "firstReplace"), "X");
    assert_eq!(string_member(&engine, "secondReplace"), "Y");
}

/// A target that does not match leaves the match array empty, and the
/// instance says so: the count is zero and the elements are void
/// (`reference/cpp/core/tjs2/tjsRegExp.cpp:727-754` fills the array only for
/// a match, `reference/cpp/core/tjs2/tjsArray.cpp:1389-1396` answers void
/// for an index past the end).
#[test]
fn a_regexp_that_does_not_match_has_an_empty_match_array() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    engine
        .exec_script(
            "var re = new RegExp('z(q)'); \
             re.exec('zz'); \
             var matchCount = re.matches.count; \
             var matchElement = re.matches[0];",
            "native_instance_members",
        )
        .expect("the script must run");

    assert_eq!(int_member(&engine, "matchCount"), 0);
    void_member(&engine, "matchElement");
}

/// An operation on an instance member acts on the instance's own member: the
/// native property is read and written through the instance, the way the
/// reference's copy of the member in the instance is
/// (`reference/cpp/core/tjs2/tjsNative.cpp:344-352`).
#[test]
fn an_operation_on_an_instance_member_acts_on_the_instance() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    engine
        .exec_script(
            "var a = [1, 2, 3]; \
             var countBefore = a.count; \
             a.count++; \
             var countAfter = a.count; \
             var re = new RegExp('a'); \
             var startBefore = re.start; \
             re.start++; \
             var startAfter = re.start;",
            "native_instance_members",
        )
        .expect("the script must run");

    assert_eq!(
        int_member(&engine, "countAfter"),
        int_member(&engine, "countBefore") + 1
    );
    assert_eq!(
        int_member(&engine, "startAfter"),
        int_member(&engine, "startBefore") + 1
    );
}

/// Every native class of the VM declares its instance members the same way,
/// and every instance of every class carries them.
#[test]
fn every_native_class_carries_its_instance_members() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    engine
        .exec_script(
            "var array = [1, 2]; \
             var date = new Date(2020, 0, 2); \
             var exception = new Exception('boom'); \
             var generator = new Math.RandomGenerator(); \
             var regexp = new RegExp('a'); \
             var arrayCount = array.count; \
             var arrayLength = array.length; \
             var arraySorted = 0; \
             array.sort(); \
             arraySorted = array[0]; \
             var dateYear = date.getYear(); \
             var dateMonth = date.getMonth(); \
             var dateDay = date.getDate(); \
             var exceptionMessage = exception.message; \
             var exceptionFinalize = typeof exception.finalize; \
             var generatorValue = generator.random(); \
             var regexpTest = regexp.test('a'); \
             var regexpMatches = typeof regexp.matches;",
            "native_instance_members",
        )
        .expect("the script must run");

    assert_eq!(int_member(&engine, "arrayCount"), 2);
    assert_eq!(int_member(&engine, "arrayLength"), 2);
    assert_eq!(int_member(&engine, "arraySorted"), 1);
    assert_eq!(int_member(&engine, "dateYear"), 2020);
    assert_eq!(int_member(&engine, "dateMonth"), 0);
    assert_eq!(int_member(&engine, "dateDay"), 2);
    assert_eq!(string_member(&engine, "exceptionMessage"), "boom");
    // The empty finalizer is an instance member of the class
    // (`reference/cpp/core/tjs2/tjsException.cpp:26`).
    assert_eq!(string_member(&engine, "exceptionFinalize"), "Object");
    // The match is a boolean
    // (`reference/cpp/core/tjs2/tjsRegExp.cpp:323-349`).
    assert_eq!(int_member(&engine, "regexpTest"), 1);
    assert_eq!(string_member(&engine, "regexpMatches"), "Object");

    // The generator's own state answers a number in [0, 1)
    // (`reference/cpp/core/tjs2/tjsRandomGenerator.cpp:285-297`).
    let value = match engine.eval("generatorValue", "native_instance_members") {
        Ok(TjsValue::Real(value)) => value,
        Ok(other) => panic!("expected a real, got {other:?}"),
        Err(error) => panic!("`generatorValue` must evaluate: {error}"),
    };
    assert!(
        (0.0..1.0).contains(&value),
        "the value must be in [0, 1): {value}"
    );
}

/// The Dictionary class declares its members as static ones
/// (`reference/cpp/core/tjs2/tjsDictionary.cpp:30-255`): the class object
/// carries them, and a dictionary instance carries none. The reference's
/// copy of the class's members skips the static ones
/// (`reference/cpp/core/tjs2/tjsNative.cpp:347`).
#[test]
fn a_dictionary_instance_carries_no_instance_member() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    engine
        .exec_script(
            "var d = new Dictionary(); \
             d.key = 'value'; \
             var assign = typeof d.assign; \
             var clear = typeof d.clear; \
             var load = typeof d.load; \
             var save = typeof d.save; \
             var classAssign = typeof Dictionary.assign; \
             var stored = d.key;",
            "native_instance_members",
        )
        .expect("the script must run");

    assert_eq!(string_member(&engine, "assign"), "undefined");
    assert_eq!(string_member(&engine, "clear"), "undefined");
    assert_eq!(string_member(&engine, "load"), "undefined");
    assert_eq!(string_member(&engine, "save"), "undefined");
    assert_eq!(string_member(&engine, "classAssign"), "Object");
    assert_eq!(string_member(&engine, "stored"), "value");
}
