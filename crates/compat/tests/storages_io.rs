//! Integration tests for the plugin surfaces that are **not** `fstat`'s.
//!
//! `fstat` is the sole owner of the 30
//! `Storages` members (tested in `tests/fstat.rs`). The reference
//! `varfile.cpp` and `xp3filter.cpp` plugins only register an L2 storage media
//! and an XP3 archive filter respectively, neither of which has an L2 seam in
//! this port; `dirlist.cpp` registers the global `getDirList(dir)` function.
//! This file drives that global function and pins the absence of the
//! over-scoped `Storages` members.
//!
//! Every test mounts a real host-backed L2 [`engine::Storage`] (through the
//! `fs` L1 file system, which is a dev-dependency) on a fresh temporary game
//! directory and drives the members through TJS. The process-wide
//! engine → context binding is single, so each test holds the shared VM lock.

mod common;

use std::path::{Path, PathBuf};

use common::vm_lock;
use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-storages-io-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        );
        path.push(unique);
        std::fs::create_dir_all(&path).expect("create temp game dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bound VM with a mounted storage attached to the compat services.
struct Env {
    // Drop order matters: the host removes the process-wide binding before
    // the boxed engine/storage it points at are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _storage: Box<Storage>,
    _dir: GameDir,
}

impl Env {
    fn new() -> Env {
        let dir = GameDir::new();
        std::fs::create_dir_all(dir.path().join("data/sub")).expect("create data dir");
        std::fs::write(dir.path().join("data/a.txt"), b"alpha").expect("write a.txt");
        std::fs::write(dir.path().join("data/b.txt"), b"bravo").expect("write b.txt");
        std::fs::write(dir.path().join("data/sub/c.txt"), b"charlie").expect("write c.txt");

        let mut storage = Box::new(
            Storage::mount_archives(
                FileSystem::host(dir.path()),
                dir.path().to_path_buf(),
                std::iter::empty::<&str>(),
            )
            .expect("mount storage"),
        );
        // Boxed so the address stays stable when it is moved into `Env`.
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        // SAFETY: `storage` is boxed and outlives the binding, which `_host`
        // removes on drop.
        unsafe { host.services_mut().attach_storage(&mut *storage) };
        host.install(&engine).expect("install compat surface");
        common::link_all_builtins(&engine);
        Env {
            _host: host,
            engine,
            _storage: storage,
            _dir: dir,
        }
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "storages-io-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn error(&self, expression: &str) -> String {
        self.engine
            .eval(expression, "storages-io-test")
            .expect_err(&format!("{expression:?} should have raised"))
            .to_string()
    }

    fn integer(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn string(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    fn defined(&self, expression: &str) -> bool {
        !matches!(
            self.eval_ok(&format!("typeof {expression}")),
            TjsValue::String(kind) if kind == "undefined"
        )
    }
}

/// Join an array's elements with `|` so the order-independent membership
/// checks can assert on a substring.
fn joined(env: &Env, array: &str) -> String {
    let expression = format!(
        "(function(){{ var a = {array}; var s = ''; \
         for (var i = 0; i < a.length; i++) s += a[i] + '|'; \
         return s; }})()"
    );
    env.string(&expression)
}

#[test]
fn global_get_dir_list_lists_immediate_children() {
    let _lock = vm_lock();
    let env = Env::new();

    // `dirlist.cpp` exposes `getDirList` as a global function; it returns the
    // immediate child names (directories without a trailing slash)
    // (`reference/cpp/plugins/dirlist.cpp:13-60`, registered at `:82`).
    assert_eq!(env.integer("getDirList('data/').count"), 3);
    let names = joined(&env, "getDirList('data/')");
    assert!(
        names.contains("a.txt|"),
        "getDirList missing a.txt: {names}"
    );
    assert!(
        names.contains("b.txt|"),
        "getDirList missing b.txt: {names}"
    );
    assert!(names.contains("sub|"), "getDirList missing sub: {names}");
}

#[test]
fn global_get_dir_list_enforces_trailing_slash() {
    let _lock = vm_lock();
    let env = Env::new();

    let message = env.error("getDirList('data')");
    // Reference: the trailing-slash check throws before listing
    // (`reference/cpp/plugins/dirlist.cpp:25-27`).
    assert!(message.contains("'/' must be specified"), "{message}");
}

#[test]
fn over_scoped_storages_members_are_not_registered() {
    let _lock = vm_lock();
    let env = Env::new();

    // `dirlist` owns the global function only
    // (`reference/cpp/plugins/dirlist.cpp:82`); it must not shadow the fstat
    // surface with a `Storages.getDirList` extra.
    assert!(
        !env.defined("Storages.getDirList"),
        "dirlist must not register a Storages.getDirList member"
    );

    // `varfile.cpp` registers a `var://` storage media (no L2 seam yet), while
    // `xp3filter.cpp` installs managed XP3 filter callbacks — the setters are
    // now registered
    // (`reference/cpp/plugins/xp3filter.cpp:407-416`).
    assert!(
        env.defined("Storages.setXP3ArchiveExtractionFilter"),
        "xp3_filter must register the extraction-filter setter"
    );
    assert!(
        env.defined("Storages.setXP3ArchiveContentFilter"),
        "xp3_filter must register the content-filter setter"
    );
}
