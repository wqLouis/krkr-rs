//! Integration tests for the `Scripts` native class.
//!
//! Raw `exec` / `eval`, `setCallMissing`, `getClassNames`, `getTraceString`,
//! `dump` and the `textEncoding` property are exercised against a bound VM.
//! The storage-backed members (`execStorage` / `evalStorage` /
//! `compileStorage`) are verified to require an attached L2 storage; their
//! end-to-end resolution is covered by the L2 `engine` tests, because the
//! `fs` crate needed to build a [`engine::Storage`] is not a dependency of
//! `compat`.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

/// A bound VM + host.
///
/// The engine is boxed and the host is declared **before** it so that (a) the
/// binding's stored engine address stays valid when `Env` is moved (the `Box`
/// keeps the `Tjs2Engine` on the heap) and (b) the host's `Drop` removes the
/// process-wide binding before the VM is destroyed.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
}

impl Env {
    fn new() -> Env {
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
        }
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "test")
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

#[test]
fn raw_exec_and_eval_run_in_the_vm() {
    let _lock = vm_lock();
    let env = Env::new();

    assert_eq!(env.eval_ok("Scripts.eval('6 * 7')"), TjsValue::Integer(42));
    assert_eq!(
        env.eval_ok("Scripts.eval(\"'a' + 'b'\")"),
        TjsValue::String("ab".into())
    );
    assert_eq!(
        env.eval_ok("Scripts.exec('var raw = 40; raw += 2;')"),
        TjsValue::Void
    );
    assert_eq!(env.eval_ok("raw"), TjsValue::Integer(42));
    // An explicit top-level `return` is the exec result.
    assert_eq!(
        env.eval_ok("Scripts.exec('return 6 * 7;')"),
        TjsValue::Integer(42)
    );
}

#[test]
fn eval_returns_a_callable_object() {
    let _lock = vm_lock();
    let env = Env::new();
    // The Action.tjs pattern: evaluate an action string and call the result.
    env.engine
        .exec_script("var action = 'function(a, b) { return a * b; }';", "test")
        .unwrap();
    env.eval_ok("Scripts.exec(\"var func = Scripts.eval(action); var result = func(6, 7);\")");
    assert_eq!(env.eval_ok("result"), TjsValue::Integer(42));
}

#[test]
fn set_call_missing_installs_a_handler() {
    let _lock = vm_lock();
    let env = Env::new();
    env.engine
        .exec_script(
            "var probe = '';\
             var o = %[missing: function(getorset, name, value) { probe = name; return true; }];",
            "test",
        )
        .unwrap();

    // Before enabling it, the handler does not run.
    let _ = env.eval("o.absentMember");
    assert_eq!(env.eval_ok("probe"), TjsValue::String(String::new()));

    assert_eq!(env.eval_ok("Scripts.setCallMissing(o)"), TjsValue::Void);

    // After enabling it, an absent member read routes through `missing`.
    assert_eq!(env.eval_ok("o.absentMember"), TjsValue::Void);
    assert_eq!(
        env.eval_ok("probe"),
        TjsValue::String("absentMember".into())
    );

    // A non-object argument is rejected rather than silently ignored.
    let message = env.error("Scripts.setCallMissing(42)");
    assert!(
        message.contains("not an object"),
        "unexpected message: {message}"
    );
}

#[test]
fn get_class_names_returns_the_object_class_chain() {
    let _lock = vm_lock();
    let env = Env::new();
    assert_eq!(
        env.eval_ok(
            "(function(){ var p = new Array(); var n = Scripts.getClassNames(p); return n[0]; })()"
        ),
        TjsValue::String("Array".into())
    );
    let message = env.error("Scripts.getClassNames(42)");
    assert!(
        message.contains("not an object"),
        "unexpected message: {message}"
    );
}

#[test]
fn get_trace_string_and_dump_run() {
    let _lock = vm_lock();
    let env = Env::new();
    assert!(matches!(
        env.eval_ok("Scripts.getTraceString()"),
        TjsValue::String(_)
    ));
    assert_eq!(env.eval_ok("Scripts.dump()"), TjsValue::Void);
}

#[test]
fn text_encoding_property_round_trips_through_dispatch_state() {
    let _lock = vm_lock();
    let env = Env::new();

    // Default reports UTF-8, like `TVPGetDefaultReadEncoding`.
    assert_eq!(
        env.eval_ok("Scripts.textEncoding"),
        TjsValue::String("UTF-8".into())
    );
    env.eval_ok("Scripts.textEncoding = 'no-such-encoding'");
    assert_eq!(
        env.eval_ok("Scripts.textEncoding"),
        TjsValue::String("no-such-encoding".into())
    );
    // The `sjis` alias maps to `cp932` (reference
    // `TVPSetDefaultReadEncoding`, `base/TextStream.cpp:418-430`).
    env.eval_ok("Scripts.textEncoding = 'SJIS'");
    assert_eq!(
        env.eval_ok("Scripts.textEncoding"),
        TjsValue::String("cp932".into())
    );
    // Any other spelling — including the empty string — is stored verbatim.
    env.eval_ok("Scripts.textEncoding = ''");
    assert_eq!(
        env.eval_ok("Scripts.textEncoding"),
        TjsValue::String("".into())
    );
}

#[test]
fn storage_backed_members_require_an_attached_storage() {
    let _lock = vm_lock();
    let env = Env::new();
    for expression in [
        "Scripts.execStorage('x.tjs')",
        "Scripts.evalStorage('x.tjs')",
        "Scripts.compileStorage('x.tjs', 'out.tjsb')",
    ] {
        let message = env.error(expression);
        assert!(
            message.contains("no storage"),
            "{expression:?} should report the missing storage, got: {message}"
        );
    }
}

#[test]
fn missing_arguments_are_reported() {
    let _lock = vm_lock();
    let env = Env::new();
    for expression in [
        "Scripts.exec()",
        "Scripts.eval()",
        "Scripts.execStorage()",
        "Scripts.compileStorage()",
        "Scripts.setCallMissing()",
    ] {
        let message = env.error(expression);
        assert!(
            message.contains("requires"),
            "{expression:?} should report a missing argument, got: {message}"
        );
    }
}
