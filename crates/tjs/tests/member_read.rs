//! Integration tests for `Tjs2Engine::member_read` — the member read that
//! keeps the shapes apart.
//!
//! The safe value surface has no octet variant, so a member that holds
//! binary data reads as an opaque object there
//! (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:1415-1418` builds the
//! octet variant). A storage-media provider has to tell a file (an octet)
//! from a directory (an object) and from an absent member
//! (`reference/cpp/plugins/varfile.cpp:10-15`), so this entry point keeps
//! the three apart.

use std::sync::{Mutex, MutexGuard};

use tjs::{MemberRead, RetainedValue, Tjs2Engine, TjsValue};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn retained_object(engine: &Tjs2Engine, expression: &str) -> tjs::DetachedValue {
    match engine
        .eval_retained(expression, "member_read_test")
        .expect("eval expression")
    {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(other) => panic!("expected an object, got {other:?}"),
    }
}

/// The octet member a script reads: its type name and its length.
#[test]
fn an_octet_member_reads_as_an_octet() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");

    engine
        .set_member_octet(global.raw_id(), "ReadProbe", &[0x00, 0x41, 0xFF])
        .expect("the octet member must be installed");

    match engine
        .member_read(global.raw_id(), "ReadProbe")
        .expect("the member must be readable")
    {
        MemberRead::Octet(bytes) => assert_eq!(bytes, vec![0x00, 0x41, 0xFF]),
        other => panic!("expected an octet, got {other:?}"),
    }
}

/// A zero-length octet is an empty file, not a failure: the read answers
/// with an empty octet.
///
/// The octet member is the shape a resource member has
/// (`reference/cpp/plugins/psbfile/PSBValue.cpp:201-204`); a payload of no
/// bytes is an empty one.
#[test]
fn an_empty_octet_member_reads_as_an_empty_octet() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");

    engine
        .set_member_octet(global.raw_id(), "EmptyOctet", &[])
        .expect("the empty octet member must be installed");

    match engine
        .member_read(global.raw_id(), "EmptyOctet")
        .expect("the empty octet member must be readable")
    {
        MemberRead::Octet(bytes) => assert!(
            bytes.is_empty(),
            "an empty octet carries no bytes: {bytes:?}"
        ),
        other => panic!("expected an octet, got {other:?}"),
    }
}

/// An object member reads as an object, and the handle reads the members
/// below it.
#[test]
fn an_object_member_reads_as_an_object() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");
    let dictionary = retained_object(&engine, "%[]");
    engine
        .set_member_octet(dictionary.raw_id(), "chunk", &[0x01, 0x02])
        .expect("the octet member must be installed");
    let published = engine
        .duplicate_retained(dictionary.raw_id())
        .expect("the dictionary must be duplicable");
    let published_id = published.raw_id().as_ptr() as usize as u64;
    engine
        .set_member(
            global.raw_id(),
            "ReadDict",
            &TjsValue::Retained(published_id),
        )
        .expect("the dictionary must be published");
    drop(published);

    let read = engine
        .member_read(global.raw_id(), "ReadDict")
        .expect("the member must be readable");
    let MemberRead::Object(object) = read else {
        panic!("expected an object, got {read:?}");
    };
    match engine
        .member_read(object.raw_id(), "chunk")
        .expect("the nested member must be readable")
    {
        MemberRead::Octet(bytes) => assert_eq!(bytes, vec![0x01, 0x02]),
        other => panic!("expected an octet, got {other:?}"),
    }
}

/// A member that does not exist reads as void, the way a script read of an
/// absent member does (`reference/cpp/core/tjs2/tjsError.h:264`).
#[test]
fn an_absent_member_reads_as_void() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");

    match engine
        .member_read(global.raw_id(), "NoSuchMember")
        .expect("an absent member is not an error")
    {
        MemberRead::Void => {}
        other => panic!("expected void, got {other:?}"),
    }
}

/// A getter that raises an error of its own is not an absent member: the
/// error travels, it does not read as void.
///
/// The VM names an absent member `Member "%1" does not exist`
/// (`reference/cpp/core/tjs2/tjsError.h:264`); an error of the script's own
/// that carries the same words is a different error and must be reported.
#[test]
fn a_getter_that_raises_reports_the_error() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");
    engine
        .exec_script(
            r#"
class RaisingHolder {
    property value {
        getter() { throw new Exception('Class "X" does not exist'); }
    }
}
var raisingHolder = new RaisingHolder();
"#,
            "member_read_test",
        )
        .expect("the class must be declared");

    let read = engine
        .member_read(global.raw_id(), "raisingHolder")
        .expect("the member must be readable");
    let MemberRead::Object(object) = read else {
        panic!("expected an object, got {read:?}");
    };
    let error = engine
        .member_read(object.raw_id(), "value")
        .expect_err("the getter's error must travel");
    assert!(
        error.contains("does not exist"),
        "the error must be the getter's own: {error}"
    );
    assert!(
        !error.starts_with("Member \""),
        "the error must not read as an absent member: {error}"
    );
}

/// A scalar member reads as a scalar and keeps its type.
#[test]
fn a_scalar_member_reads_as_a_scalar() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");
    engine
        .set_member(global.raw_id(), "ReadNumber", &TjsValue::Integer(42))
        .expect("the member must be installed");
    engine
        .set_member(
            global.raw_id(),
            "ReadText",
            &TjsValue::String("hello".into()),
        )
        .expect("the member must be installed");

    match engine
        .member_read(global.raw_id(), "ReadNumber")
        .expect("the member must be readable")
    {
        MemberRead::Scalar(TjsValue::Integer(value)) => assert_eq!(value, 42),
        other => panic!("expected an integer, got {other:?}"),
    }
    match engine
        .member_read(global.raw_id(), "ReadText")
        .expect("the member must be readable")
    {
        MemberRead::Scalar(TjsValue::String(value)) => assert_eq!(value, "hello"),
        other => panic!("expected a string, got {other:?}"),
    }
}

/// A property's getter runs: the read answers with the getter's value, not
/// with the property object
/// (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2831-2848`).
#[test]
fn a_property_getter_runs() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");
    engine
        .exec_script(
            "class ReadHolder {\n  property value {\n    getter() { return 7; }\n  }\n}\nvar readHolder = new ReadHolder();\n",
            "member_read_test",
        )
        .expect("the class must be declared");

    match engine
        .member_read(global.raw_id(), "readHolder")
        .expect("the member must be readable")
    {
        MemberRead::Object(object) => match engine
            .member_read(object.raw_id(), "value")
            .expect("the property must be readable")
        {
            MemberRead::Scalar(TjsValue::Integer(value)) => assert_eq!(value, 7),
            other => panic!("expected the getter's value, got {other:?}"),
        },
        other => panic!("expected an object, got {other:?}"),
    }
}

/// An array's numeric member is the element it names
/// (`reference/cpp/core/tjs2/tjsArray.cpp:1586-1593`).
#[test]
fn an_array_member_reads_the_element() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");
    let array = retained_object(&engine, "[10, 20, 30]");

    match engine
        .member_read(array.raw_id(), "1")
        .expect("the element must be readable")
    {
        MemberRead::Scalar(TjsValue::Integer(value)) => assert_eq!(value, 20),
        other => panic!("expected an integer, got {other:?}"),
    }

    // The array is reachable from the global object, so the same read works
    // through the global.
    let published = engine
        .duplicate_retained(array.raw_id())
        .expect("the array must be duplicable");
    let published_id = published.raw_id().as_ptr() as usize as u64;
    engine
        .set_member(
            global.raw_id(),
            "ReadArray",
            &TjsValue::Retained(published_id),
        )
        .expect("the array must be published");
    drop(published);

    match engine
        .member_read(global.raw_id(), "ReadArray")
        .expect("the member must be readable")
    {
        MemberRead::Object(object) => match engine
            .member_read(object.raw_id(), "2")
            .expect("the element must be readable")
        {
            MemberRead::Scalar(TjsValue::Integer(value)) => assert_eq!(value, 30),
            other => panic!("expected an integer, got {other:?}"),
        },
        other => panic!("expected an object, got {other:?}"),
    }
}

/// A member read of a target that is not a live retained object is an error,
/// not a silent void.
#[test]
fn an_invalid_target_is_an_error() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();

    assert!(
        engine.member_read(tjs::Tjs2ValueId::null(), "x").is_err(),
        "a null retained id must be rejected"
    );
}
