//! The `null` a native hands back to the script, and the price of producing
//! it.
//!
//! Reference: a native that has no object to return hands the script TJS
//! `null` — an object-typed variant with a null object pointer
//! (`reference/cpp/core/tjs2/tjsVariant.h:552-557`). `Layer.loadImages`
//! returns the graphic metainfo, which is that null when the format carries
//! none (`reference/cpp/core/visual/LayerIntf.cpp:8431-8438`); the `System`
//! class object's default data members are the same null
//! (`reference/cpp/core/base/SystemIntf.cpp:470-476`).
//!
//! The host writes that null through the ABI's own null value, so it never
//! compiles a script to produce one. The tests below count the scripts the
//! host pushed into the VM: a native that returns a null must not raise that
//! count.

mod common;

use std::sync::{Arc, Mutex};

use common::{Env, vm_lock};

/// The number of scripts the host pushed into the VM.
fn evaluations(env: &Env) -> u64 {
    env.engine().script_evaluation_count()
}

/// Whether an expression reads back as the null value.
///
/// TJS2's strict comparison of two object variants compares their object
/// pointers (`reference/cpp/core/tjs2/tjsVariant.cpp:806-811`), so the null
/// equals the null and differs from `void`.
fn is_null(env: &Env, expression: &str) -> bool {
    env.eval_int(&format!("({expression}) === null")) == 1
}

/// An environment with a storage that holds `picture.png` and a codec that
/// decodes it.
fn env_with_a_picture() -> Env {
    let env = Env::with_storage();
    env.record_decodes(Arc::new(Mutex::new(Vec::new())));
    let dir = env.game_dir().expect("the temp game directory").clone();
    std::fs::write(dir.join("picture.png"), b"not-a-real-image").expect("write the picture");
    env
}

/// The natives hand the script the null the reference hands it.
///
/// `Layer.loadImages` returns the graphic metainfo
/// (`reference/cpp/core/visual/LayerIntf.cpp:8431-8438`), which is a null
/// object pointer when the format carries none; `Window.focusedLayer` is null
/// while no layer holds the focus; the `System` class object's default data
/// members are null (`reference/cpp/core/base/SystemIntf.cpp:470-476`).
#[test]
fn the_natives_hand_the_script_the_null_the_reference_hands() {
    let _lock = vm_lock();
    let env = env_with_a_picture();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setSize(4, 4); \
         l.loadImages('picture.png');",
    );

    assert!(
        is_null(&env, "l.loadImages('picture.png')"),
        "the load returns the graphic metainfo, which is null"
    );
    assert!(is_null(&env, "w.focusedLayer"), "no layer holds the focus");
    assert!(
        is_null(&env, "System.onActivate"),
        "the default data member is null"
    );
    assert!(
        is_null(&env, "System.exceptionHandler"),
        "the default data member is null"
    );
}

/// The null costs no script: the host does not compile a script to produce
/// it.
///
/// The count of the scripts the host pushed into the VM moves by exactly the
/// test's own script — a native that returned its null through the script
/// compiler would add one more.
#[test]
fn the_null_costs_no_script_evaluation() {
    let _lock = vm_lock();
    let env = env_with_a_picture();
    env.run(
        "var w = new Window(); \
         var l = new Layer(w, null); \
         l.setSize(4, 4);",
    );

    let before = evaluations(&env);
    env.run("l.loadImages('picture.png');");
    let after = evaluations(&env);
    assert_eq!(
        after - before,
        1,
        "the load compiled a script to produce its null"
    );

    let before = evaluations(&env);
    env.run("w.focusedLayer;");
    let after = evaluations(&env);
    assert_eq!(
        after - before,
        1,
        "the focused layer compiled a script to produce its null"
    );
}
