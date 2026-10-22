//! Integration tests for the cached expression evaluation
//! (`Tjs2Engine::eval_retained_cached`).
//!
//! An expression evaluated without a name goes through the script cache: the
//! block is compiled once and reused, while a named evaluation compiles a
//! fresh block every time (`tjsScriptCache.cpp:75-190`). The cached block is
//! the compiled code, so every run still produces a fresh result.

use std::sync::{Mutex, MutexGuard};

use tjs::{RetainedValue, Tjs2Engine, TjsValue};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// A cached expression returns the same value on every run.
#[test]
fn a_cached_expression_returns_the_same_value() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("engine");
    for _ in 0..3 {
        match engine.eval_retained_cached("1 + 1").expect("eval") {
            RetainedValue::Value(TjsValue::Integer(value)) => assert_eq!(value, 2),
            RetainedValue::Value(other) => panic!("expected an integer, got {other:?}"),
            RetainedValue::Object(_) => panic!("expected a value, got an object"),
        }
    }
}

/// A cached expression that builds an object returns a **new** object on
/// every run: the cache holds the compiled block, not its result.
#[test]
fn a_cached_expression_returns_a_fresh_object() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("engine");
    let first = engine.eval_retained_cached("%[]").expect("first eval");
    let second = engine.eval_retained_cached("%[]").expect("second eval");
    let (RetainedValue::Object(first), RetainedValue::Object(second)) = (first, second) else {
        panic!("the expression must return an object");
    };
    assert!(
        first.raw_id() != second.raw_id(),
        "each run must build a new object"
    );
}
