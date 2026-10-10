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

#[test]
fn invalidate_retained_rejects_a_null_id() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    assert!(engine.invalidate_retained(ptr::null_mut()).is_err());
}
