//! Integration test for `Tjs2Engine::invalidate_retained` — the ABI that runs
//! an object's script `finalize` (the reference `invalidate obj;` statement,
//! used by the `Window.add` object vector at window destruction).

use std::ptr;
use std::sync::{Mutex, MutexGuard};

use tjs::{RetainedValue, Tjs2Engine, TjsValue};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[test]
fn invalidate_retained_runs_finalize() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    engine
        .exec_script(
            "global.finalized = 0; \
             class Widget { function finalize() { global.finalized = 1; } }",
            "def",
        )
        .unwrap();
    let object = match engine.eval_retained("new Widget()", "obj").unwrap() {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(other) => panic!("new Widget() is not an object: {other:?}"),
    };
    assert_eq!(
        engine.eval("global.finalized", "q").unwrap(),
        TjsValue::Integer(0),
    );
    engine.invalidate_retained(object.raw_id()).unwrap();
    assert_eq!(
        engine.eval("global.finalized", "q").unwrap(),
        TjsValue::Integer(1),
    );
}

/// A `class` with a method is self-referential: the class's members are stored
/// on the instance as closures whose `ObjThis` is the instance itself
/// (`TJSInterCodeContext::RegisterObjectMember` →
/// `tTJSVariant::ChangeClosureObjThis`), so the instance's reference count can
/// never reach zero from an external drop. TJS2 has no cycle collector —
/// `tTJS::DoGarbageCollection` only compacts the variant-array stack and the
/// string heap (`reference/cpp/core/tjs2/tjs.cpp:495-500`) and
/// `TJSVariantArrayStackCompactNow` is an empty stub in both trees
/// (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:627`) — so the reference runs
/// `finalize` on the explicit `invalidate obj` statement, whose `Finalize` →
/// `DeleteAllMembers` releases the member closures and breaks the cycle
/// (`reference/cpp/core/tjs2/tjsObject.cpp:389-405`). Dropping the last
/// external handle alone does not finalize it. This pins the contract so a
/// port-side change cannot silently diverge.
#[test]
fn self_referential_class_instance_finalizes_on_invalidate_only() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    engine
        .exec_script(
            "global.finalized = 0; \
             class Widget { function finalize() { global.finalized = 1; } }",
            "def",
        )
        .unwrap();
    let object = match engine.eval_retained("new Widget()", "obj").unwrap() {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(other) => panic!("new Widget() is not an object: {other:?}"),
    };
    // Release the external retention: the instance's own method closures still
    // reference it, and TJS2 has no cycle collector, so `finalize` has not run.
    drop(object);
    assert_eq!(
        engine.eval("global.finalized", "q").unwrap(),
        TjsValue::Integer(0),
        "a self-referential class instance is not finalised by dropping an external handle"
    );
    // The explicit invalidate is the reference's teardown: it runs `finalize`
    // once and releases the member closures.
    let object = match engine.eval_retained("new Widget()", "obj").unwrap() {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(other) => panic!("new Widget() is not an object: {other:?}"),
    };
    engine.invalidate_retained(object.raw_id()).unwrap();
    assert_eq!(
        engine.eval("global.finalized", "q").unwrap(),
        TjsValue::Integer(1),
        "invalidating the instance must run finalize once"
    );
}

#[test]
fn invalidate_retained_rejects_a_null_id() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    assert!(engine.invalidate_retained(ptr::null_mut()).is_err());
}
