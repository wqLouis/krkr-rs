//! Integration tests for `tjs2_prop_set_default` — the `Tjs2Engine::set_default_member`
//! ABI a native `missing` handler uses to make an absent member callable.
//!
//! The exhaustive unit tests live in `src/lib.rs`; this covers the
//! default-member path and, importantly, the retention-consumption contract
//! (a `TjsValue::Retained` id is consumed by conversion even when `PropSet`
//! then fails).

use std::sync::{Mutex, MutexGuard};

use tjs::{DetachedValue, RetainedValue, Tjs2Engine, Tjs2ValueId, TjsValue};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

fn retained_object(engine: &Tjs2Engine, expression: &str) -> DetachedValue {
    match engine
        .eval_retained(expression, "prop_default_test")
        .expect("eval expression")
    {
        RetainedValue::Object(object) => object,
        RetainedValue::Value(other) => panic!("expected an object, got {other:?}"),
    }
}

fn retained_count(engine: &Tjs2Engine) -> usize {
    // SAFETY: `engine` is a live engine.
    unsafe { tjs::tjs2_retained_count(engine.raw()) }
}

#[test]
fn set_default_member_consumes_the_retained_value() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let target = retained_object(&engine, "%[]");
    let value = retained_object(&engine, "(function(){})");
    let before = retained_count(&engine);
    // The write may succeed or fail (a plain dictionary need not accept a
    // default member); either way the retained value is converted first and
    // therefore consumed.
    let result = engine.set_default_member(
        target.raw_id(),
        &TjsValue::Retained(value.raw_id().as_ptr() as u64),
    );
    let after = retained_count(&engine);
    assert_eq!(
        after,
        before - 1,
        "set_default_member must consume the retained value (result: {result:?})"
    );
}

#[test]
fn set_default_member_with_invalid_target_keeps_the_value_retained() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().unwrap();
    let value = retained_object(&engine, "(function(){})");
    let before = retained_count(&engine);
    let err = engine
        .set_default_member(
            Tjs2ValueId::null(),
            &TjsValue::Retained(value.raw_id().as_ptr() as u64),
        )
        .expect_err("a null target must fail");
    assert!(!err.is_empty(), "expected an error message: {err}");
    // The C++ side validates the target *before* converting the value, so a
    // bad target leaves the retention intact.
    assert_eq!(
        retained_count(&engine),
        before,
        "target validation precedes value conversion"
    );
}
