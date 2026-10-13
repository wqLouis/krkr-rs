//! Integration tests for the `Storages` native class.
//!
//! The L3 tests exercise the members that don't need a mounted L2 storage
//! (the pure string helpers, the placed-path cache clear and the argument /
//! "no storage attached" error paths). Storage-backed resolution itself is
//! covered by the L2 `engine` tests.

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

    // Reference: the pure storage-string helpers in
    // `reference/cpp/core/base/StorageIntf.cpp`: `TVPExtractStorageExt`
    // `:775-805`, `TVPExtractStorageName` `:809-833` (the `>` archive
    // delimiter breaks the name), `TVPExtractStoragePath` `:837-859`,
    // `TVPChopStorageExt` `:863-887`; `getFullPath` is the
    // `TVPNormalizeStorageName` native (`:1281-1292`, normalizer `:527`).
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
    // `getFullPath` lowercases the disk/domain path and folds backslashes; the
    // in-archive part is normalized too (no storage needed).
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

/// `Storages.getFullPath` is `TVPNormalizeStorageName`
/// (`reference/cpp/core/base/StorageIntf.cpp:1281-1292`): the disk/domain path
/// is lowercased and `\` folds to `/` (`tTVPFileMedia::NormalizeDomainName` /
/// `NormalizePathName`, `reference/cpp/core/base/impl/StorageImpl.cpp:81-103`,
/// applied at `reference/cpp/core/base/StorageIntf.cpp:362-372`), and the
/// in-archive part after `>` is normalized the way
/// `NormalizeInArchiveStorageName` does — lowercase, `\` → `/`, collapse runs
/// of `/`, drop leading slashes
/// (`reference/cpp/core/base/StorageIntf.cpp:566-606`).
#[test]
fn get_full_path_normalizes_the_in_archive_part() {
    let _lock = vm_lock();
    let env = Env::new();
    assert_eq!(
        env.eval_ok("Storages.getFullPath('Arc.XP3>Data//X.TJS')"),
        TjsValue::String("arc.xp3>data/x.tjs".into())
    );
    assert_eq!(
        env.eval_ok("Storages.getFullPath('Arc.XP3>/Leading/X.TJS')"),
        TjsValue::String("arc.xp3>leading/x.tjs".into())
    );
    assert_eq!(
        env.eval_ok("Storages.getFullPath('Arc.XP3>Inner\\\\Nested.Dat')"),
        TjsValue::String("arc.xp3>inner/nested.dat".into())
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
        "Storages.getLocalName('x.tjs')",
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
    ] {
        let message = env.error(expression);
        assert!(
            message.contains("requires"),
            "{expression:?} should report a missing argument, got: {message}"
        );
    }
}

#[test]
fn search_cd_and_select_file_return_the_reference_inert_values() {
    let _lock = vm_lock();
    let env = Env::new();
    // `TVPSearchCD` and `TVPSelectFile` return their inert values without a
    // host seam (`StorageImpl.cpp:1232-1246`, `FileSelector.cpp:100-420`).
    assert_eq!(
        env.eval_ok("Storages.searchCD('GAME')"),
        TjsValue::String(String::new())
    );
    assert_eq!(
        env.eval_ok("Storages.selectFile(%[name: 'x.ks'])"),
        TjsValue::Integer(0)
    );
    // Arity and parameter shape are still validated.
    assert!(env.error("Storages.searchCD()").contains("requires"));
    assert!(env.error("Storages.selectFile()").contains("requires"));
    assert!(
        env.error("Storages.selectFile(42)")
            .contains("parameter object")
    );
}
