//! Integration test for the periodic TJS object rehash.
//!
//! The reference system control calls `TJSDoRehash()` every 1500 ms
//! (`reference/cpp/core/environ/impl/SystemControl.cpp:181-185`); the call only
//! bumps the rebuild magic, and each object rebuilds its member table on its
//! next property access
//! (`reference/cpp/core/tjs2/tjsObject.cpp:1280-1282`). `Tjs2Engine::do_rehash`
//! is the C ABI for that request, and `Scripts.rehash` binds the same call
//! (`reference/cpp/plugins/scriptsEx.cpp:993`).

use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tjs::{Tjs2Engine, TjsValue};

/// The C++ TJS2 VM is single-threaded; serialize the tests.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// A dictionary that grew past its initial bucket count keeps every member
/// and reads it back after the lazy rebuild. The insertion also stays linear:
/// the table grows when it fills (`reference/cpp/core/tjs2/tjsObject.cpp:493-560`),
/// so building 100k members does not walk one long chain.
#[test]
fn rehash_preserves_a_grown_dictionary() {
    let _lock = vm_lock();
    let engine = Tjs2Engine::new().expect("create engine");
    let started = Instant::now();
    engine
        .exec_script(
            "var grown = %[];
             for (var i = 0; i < 100000; i++) { grown[\"k\" + i] = i; }",
            "rehash_growth",
        )
        .expect("populate the dictionary");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "building a 100k-member dictionary must not be quadratic, took {:?}",
        started.elapsed()
    );

    engine.do_rehash();
    // The first read after the magic bump performs the rebuild. Walk every
    // member so a lost or changed key anywhere in the table fails the count,
    // rather than sampling two keys.
    engine
        .exec_script(
            "var perfCount = 0;
             for (var i = 0; i < 100000; i++) {
                 var v = grown[\"k\" + i];
                 if (typeof v != \"Integer\") { perfCount = -1; break; }
                 if (v != i) { perfCount = -1; break; }
                 perfCount = perfCount + 1;
             }",
            "rehash_growth",
        )
        .expect("walk the rebuilt dictionary");
    assert_eq!(
        engine.eval("perfCount", "rehash_growth").unwrap(),
        TjsValue::Integer(100000),
        "the dictionary still holds every member after the rebuild"
    );
}
