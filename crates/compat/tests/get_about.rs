//! Integration tests for the `getabout.dll` plugin shim.
//!
//! `getabout.cpp` is a single `NCB_ATTACH_FUNCTION(getAboutString, System,
//! TVPGetAboutString)` (`reference/cpp/plugins/getabout.cpp:5`). The Rust
//! `windowEx` port owns the whole `System` extension surface and registers
//! that member (`reference/cpp/plugins/windowEx.cpp:1526`), so
//! `get_about::register` installs nothing; this test pins the observable
//! result — `System.getAboutString` exists and returns the
//! `TVPGetAboutString` string.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host.
///
/// The engine is boxed and the host is declared **before** it so the
/// process-wide binding outlives the VM in declaration order.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
        }
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "get-about-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }
}

/// `getabout.dll` attaches `TVPGetAboutString` to `System`
/// (`reference/cpp/plugins/getabout.cpp:5`), the same member the `windowEx`
/// port registers (`reference/cpp/plugins/windowEx.cpp:1526`); it returns a
/// non-empty string (`reference/cpp/core/base/MsgIntf.cpp:144`).
#[test]
fn get_about_string_is_attached_to_system() {
    let _lock = vm_lock();
    let env = Env::new();

    match env.eval_ok("System.getAboutString()") {
        TjsValue::String(text) => assert!(!text.is_empty()),
        other => panic!("System.getAboutString returned {other:?}, expected a string"),
    }
    // Reachable only through `System`, never as a free global.
    assert!(
        env.engine
            .eval("getAboutString()", "get-about-test")
            .is_err()
    );
}
