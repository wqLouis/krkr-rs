//! Integration tests for the `Storages` native class.
//!
//! The L3 tests exercise the members that don't need a mounted L2 storage
//! (the pure string helpers, the placed-path cache clear and the argument /
//! "no storage attached" error paths). Storage-backed resolution itself is
//! covered by the L2 `engine` tests; attaching a real [`engine::Storage`]
//! here would require the `fs` crate, which is not a dependency of `compat`
//! (and editing `Cargo.toml` is out of scope).

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
fn string_helpers_match_reference() {
    let _lock = vm_lock();
    let env = Env::new();

    assert_eq!(
        env.eval_ok("Storages.extractStorageName('data/x.tjs')"),
        TjsValue::String("x.tjs".into())
    );
    // The `>` archive delimiter splits like `/` and `\`.
    assert_eq!(
        env.eval_ok("Storages.extractStorageName('arc.xp3>data/x.tjs')"),
        TjsValue::String("x.tjs".into())
    );
    assert_eq!(
        env.eval_ok("Storages.extractStorageExt('a/b.ks')"),
        TjsValue::String(".ks".into())
    );
    assert_eq!(
        env.eval_ok("Storages.extractStoragePath('data/x.tjs')"),
        TjsValue::String("data/".into())
    );
    assert_eq!(
        env.eval_ok("Storages.chopStorageExt('data/x.tjs')"),
        TjsValue::String("data/x".into())
    );
    // `getFullPath` lowercases and folds backslashes (no storage needed).
    assert_eq!(
        env.eval_ok("Storages.getFullPath('Data\\\\X.TJS')"),
        TjsValue::String("data/x.tjs".into())
    );
    // No extension before a delimiter: the name is returned unchanged.
    assert_eq!(
        env.eval_ok("Storages.chopStorageExt('data/noext')"),
        TjsValue::String("data/noext".into())
    );
    assert_eq!(
        env.eval_ok("Storages.extractStorageExt('data/noext')"),
        TjsValue::String(String::new())
    );
}

#[test]
fn clear_archive_cache_runs_without_a_storage() {
    let _lock = vm_lock();
    let env = Env::new();
    assert_eq!(env.eval_ok("Storages.clearArchiveCache()"), TjsValue::Void);
}

#[test]
fn get_placed_path_of_empty_name_is_empty() {
    let _lock = vm_lock();
    let env = Env::new();
    // The empty-name short circuit happens before the storage is consulted.
    assert_eq!(
        env.eval_ok("Storages.getPlacedPath('')"),
        TjsValue::String(String::new())
    );
}

#[test]
fn auto_path_requires_a_trailing_delimiter() {
    let _lock = vm_lock();
    let env = Env::new();
    // The delimiter check runs before the storage is consulted, so the error
    // is the reference's trailing-delimiter message, not "no storage".
    let message = env.error("Storages.addAutoPath('no/trailing/slash')");
    assert!(
        message.contains("must end with"),
        "unexpected message: {message}"
    );
    let message = env.error("Storages.removeAutoPath('no/trailing/slash')");
    assert!(
        message.contains("must end with"),
        "unexpected message: {message}"
    );
}

#[test]
fn storage_backed_members_require_an_attached_storage() {
    let _lock = vm_lock();
    let env = Env::new();

    for expression in [
        "Storages.addAutoPath('dir/')",
        "Storages.removeAutoPath('dir/')",
        "Storages.getPlacedPath('x.tjs')",
        "Storages.isExistentStorage('x.tjs')",
        "Storages.getFileList('*')",
        "Storages.getLocalName('x.tjs')",
        "Storages.stat('x.tjs')",
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
        "Storages.getFullPath()",
        "Storages.getPlacedPath()",
        "Storages.isExistentStorage()",
        "Storages.extractStorageName()",
        "Storages.getFileList()",
    ] {
        let message = env.error(expression);
        assert!(
            message.contains("requires"),
            "{expression:?} should report a missing argument, got: {message}"
        );
    }
}
