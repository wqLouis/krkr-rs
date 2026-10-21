//! Integration tests for `Tjs2Engine::set_member_octet` — the octet member
//! ABI.
//!
//! The reference builds an octet member straight from the raw bytes
//! (`reference/cpp/plugins/psbfile/PSBValue.cpp:201-204`,
//! `tTJSVariant(data, size)`), so a script sees an octet: `typeof` reports
//! `Octet`, `.length` is the byte count and an index yields the raw byte
//! (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:124-162`).
//!
//! The safe value surface carries no octet variant, so the bytes travel
//! through this dedicated entry point.

use std::sync::{Mutex, MutexGuard};

use tjs::{DetachedValue, RetainedValue, Tjs2Engine, TjsValue};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn retained_object(engine: &Tjs2Engine, expression: &str) -> DetachedValue {
    match engine
        .eval_retained(expression, "octet_member_test")
        .expect("eval expression")
    {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(other) => panic!("expected an object, got {other:?}"),
    }
}

fn eval(engine: &Tjs2Engine, expression: &str) -> TjsValue {
    engine
        .eval(expression, "octet_member_test")
        .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
}

/// The octet member a script reads: its type name, its length and its
/// bytes.
#[test]
fn octet_member_reads_back_as_an_octet() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");

    engine
        .set_member_octet(global.raw_id(), "AbiOctetProbe", &[0x00, 0x41, 0xFF])
        .expect("the octet member must be installed");

    assert_eq!(
        eval(&engine, "typeof AbiOctetProbe"),
        TjsValue::String("Octet".into()),
        "the member must be an octet variant"
    );
    assert_eq!(
        eval(&engine, "AbiOctetProbe.length"),
        TjsValue::Integer(3),
        "the length must be the byte count"
    );
    assert_eq!(eval(&engine, "AbiOctetProbe[0]"), TjsValue::Integer(0));
    assert_eq!(eval(&engine, "AbiOctetProbe[1]"), TjsValue::Integer(0x41));
    assert_eq!(eval(&engine, "AbiOctetProbe[2]"), TjsValue::Integer(0xFF));
}

/// The same member read through a script variable, the way a game reaches
/// a resource member (`reference/cpp/plugins/psbfile/PSBValue.cpp:201-204`).
#[test]
fn octet_member_reads_back_through_a_script_variable() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");

    engine
        .set_member_octet(global.raw_id(), "AbiOctetProbe", &[0x10, 0x20])
        .expect("the octet member must be installed");
    engine
        .exec_script("var probe = AbiOctetProbe;", "octet_member_test")
        .expect("the probe variable must be declared");

    assert_eq!(
        eval(&engine, "typeof probe"),
        TjsValue::String("Octet".into())
    );
    assert_eq!(eval(&engine, "probe.length"), TjsValue::Integer(2));
    assert_eq!(eval(&engine, "probe[1]"), TjsValue::Integer(0x20));
}

/// An empty payload is an octet of length zero — the reference's
/// `tTJSVariant(data, 0)` is the same empty octet
/// (`reference/cpp/plugins/psbfile/PSBValue.cpp:201-204`).
#[test]
fn an_empty_octet_member_has_no_bytes() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");

    engine
        .set_member_octet(global.raw_id(), "AbiEmptyOctet", &[])
        .expect("an empty octet member must be installed");

    assert_eq!(
        eval(&engine, "typeof AbiEmptyOctet"),
        TjsValue::String("Octet".into())
    );
    assert_eq!(eval(&engine, "AbiEmptyOctet.length"), TjsValue::Integer(0));
}

/// A payload longer than one byte keeps every byte, byte values included —
/// the octet member is not a string.
#[test]
fn a_large_octet_member_keeps_every_byte() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");

    let payload: Vec<u8> = (0..300u32).map(|index| (index % 256) as u8).collect();
    engine
        .set_member_octet(global.raw_id(), "AbiLargeOctet", &payload)
        .expect("the octet member must be installed");

    assert_eq!(
        eval(&engine, "typeof AbiLargeOctet"),
        TjsValue::String("Octet".into())
    );
    assert_eq!(
        eval(&engine, "AbiLargeOctet.length"),
        TjsValue::Integer(300)
    );
    assert_eq!(eval(&engine, "AbiLargeOctet[0]"), TjsValue::Integer(0));
    assert_eq!(eval(&engine, "AbiLargeOctet[255]"), TjsValue::Integer(255));
    assert_eq!(eval(&engine, "AbiLargeOctet[299]"), TjsValue::Integer(43));
}

/// The member lands on a dictionary, the shape the PSB resource tree uses.
#[test]
fn an_octet_member_can_be_installed_on_a_dictionary() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");
    let dictionary = retained_object(&engine, "%[]");

    // Publish a duplicate of the dictionary on the global object so a
    // script can read the member back; the original retention stays live.
    let published = engine
        .duplicate_retained(dictionary.raw_id())
        .expect("the dictionary must be duplicable");
    let published_id = published.raw_id().as_ptr() as usize as u64;
    engine
        .set_member(
            global.raw_id(),
            "AbiDict",
            &TjsValue::Retained(published_id),
        )
        .expect("the dictionary must be published");
    drop(published);

    engine
        .set_member_octet(dictionary.raw_id(), "chunk", &[0x01, 0x02, 0x03])
        .expect("the octet member must be installed");

    assert_eq!(
        eval(&engine, "typeof AbiDict.chunk"),
        TjsValue::String("Octet".into())
    );
    assert_eq!(eval(&engine, "AbiDict.chunk.length"), TjsValue::Integer(3));
    assert_eq!(eval(&engine, "AbiDict.chunk[2]"), TjsValue::Integer(3));
}

/// A target that is not a live retained object is a typed error, and a
/// member name with a NUL byte never names a member.
#[test]
fn an_invalid_target_or_name_is_a_typed_error() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();

    assert!(
        engine
            .set_member_octet(tjs::Tjs2ValueId::null(), "x", &[1])
            .is_err(),
        "a null retained id must be rejected"
    );

    let global = retained_object(&engine, "global");
    assert!(
        engine
            .set_member_octet(global.raw_id(), "x\0y", &[1])
            .is_err(),
        "a NUL byte in the member name must be rejected"
    );
}
