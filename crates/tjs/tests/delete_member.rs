//! Integration tests for `tjs2_prop_delete` — the `Tjs2Engine::delete_member`
//! ABI. The reference operation is `iTJSDispatch2::DeleteMember`
//! (`tjsObject.h:472`), whose `tTJSCustomObject` implementation
//! (`tjsObject.cpp:1537-1552`) delegates to `DeleteByName`
//! (`tjsObject.cpp:759`); the VM's own `delete` operator passes flag 0 and
//! the dispatch object itself (`tjsInterCodeExec.cpp:1826`).
//!
//! Absent-member semantics are pinned to the VM: `DeleteByName` returns
//! `false` and `tTJSCustomObject::DeleteMember` maps that to
//! `TJS_E_MEMBERNOTFOUND` (`tjsObject.cpp:1546`). The `delete` *operator*
//! turns the failed status into the boolean `false`
//! (`tjsInterCodeExec.cpp:1829-1833`), but the raw dispatch operation this
//! ABI mirrors reports it as a failure, so `delete_member` returns a typed
//! error for an absent member.

use std::ptr;
use std::sync::{Mutex, MutexGuard};

use tjs::{DetachedValue, RetainedValue, Tjs2Engine, TjsValue};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn retained_object(engine: &Tjs2Engine, expression: &str) -> DetachedValue {
    match engine
        .eval_retained(expression, "delete_member_test")
        .expect("eval expression")
    {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(other) => panic!("expected an object, got {other:?}"),
    }
}

#[test]
fn delete_member_removes_a_global_member() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    // `global` is the VM global object (the `T_GLOBAL` expression compiles to
    // `VM_GLOBAL`, tjsInterCodeExec.cpp:1289-1292) — the same object the
    // native registration entry points PropSet the class name onto.
    let global = retained_object(&engine, "global");

    engine
        .set_member(global.raw_id(), "AbiDeleteProbe", &TjsValue::Integer(41))
        .unwrap();
    assert_eq!(
        engine.member(global.raw_id(), "AbiDeleteProbe").unwrap(),
        TjsValue::Integer(41),
        "the member must be readable before the delete"
    );

    engine
        .delete_member(global.raw_id(), "AbiDeleteProbe")
        .expect("deleting an existing member must succeed");

    // Reading now hits the VM's missing-member path: `tTJSCustomObject::
    // PropGet` returns TJS_E_MEMBERNOTFOUND (tjsObject.cpp:1309) once
    // DeleteByName removed the slot (tjsObject.cpp:759). Our `member`
    // surfaces that as a typed error, so the deleted member is no longer
    // readable.
    let read_err = engine
        .member(global.raw_id(), "AbiDeleteProbe")
        .expect_err("the deleted member must no longer be readable");
    assert!(
        !read_err.is_empty(),
        "expected an error message: {read_err}"
    );

    // A second delete reports the member as absent too, proving the slot is
    // gone (not merely hidden behind the missing-member error).
    assert!(
        engine
            .delete_member(global.raw_id(), "AbiDeleteProbe")
            .is_err(),
        "deleting the just-deleted member must report it as absent"
    );
}

#[test]
fn delete_member_removes_a_member_from_a_dictionary() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let dict = retained_object(&engine, "%[]");
    engine
        .set_member(dict.raw_id(), "k", &TjsValue::String("v".into()))
        .unwrap();
    engine.delete_member(dict.raw_id(), "k").unwrap();
    // A dictionary swallows a missing read as `void` (unlike a custom object,
    // whose `PropGet` reports the absence): `tTJSDictionaryObject::PropGet`
    // clears the result and returns success
    // (`crates/tjs/cpp/tjs2/tjsDictionary.cpp:739-743`).
    assert_eq!(
        engine.member(dict.raw_id(), "k").unwrap(),
        TjsValue::Void,
        "the deleted dictionary member reads back as void"
    );
    // The slot is genuinely gone, so deleting it again is a typed error.
    assert!(
        engine.delete_member(dict.raw_id(), "k").is_err(),
        "a second delete must report the absent member"
    );
}

#[test]
fn delete_member_of_an_absent_member_is_a_typed_error() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let global = retained_object(&engine, "global");
    let err = engine
        .delete_member(global.raw_id(), "NeverDefinedMember")
        .expect_err("an absent member must be a typed error");
    assert!(!err.is_empty(), "expected an error message: {err}");
}

#[test]
fn delete_member_rejects_dead_and_non_object_ids() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();

    // A null id is a caller mistake, not a missing member.
    assert!(
        engine.delete_member(ptr::null_mut(), "x").is_err(),
        "a null retained id must be rejected"
    );

    // A released retained id is dead.
    let dict = retained_object(&engine, "%[]");
    let dead_id = dict.raw_id();
    drop(dict);
    assert!(
        engine.delete_member(dead_id, "x").is_err(),
        "a released retained id must be rejected"
    );

    // A retained non-object has no member table to delete from.
    let scalar = engine.retain_value_detached(&TjsValue::Integer(7)).unwrap();
    assert!(
        engine.delete_member(scalar.raw_id(), "x").is_err(),
        "a non-object retained value must be rejected"
    );

    // TJS `null` is an object value with no object pointer: no member table.
    let null = retained_object(&engine, "null");
    assert!(
        engine.delete_member(null.raw_id(), "x").is_err(),
        "TJS null must be rejected"
    );

    // A member name containing a NUL byte can never name a member.
    let global = retained_object(&engine, "global");
    assert!(
        engine.delete_member(global.raw_id(), "x\0y").is_err(),
        "a NUL byte in the member name must be rejected"
    );
}
