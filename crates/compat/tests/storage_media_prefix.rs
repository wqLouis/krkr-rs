//! The script-name → VFS-name boundary for storage-backed execution.
//!
//! A script-visible storage name may carry the local `file` media prefix
//! (`file://.`, `file://`, `file:`); the L2 namespace takes the bare name.
//! These tests drive a `file://.`-prefixed, fully lowercased absolute name
//! through `Scripts.execStorage`/`evalStorage` against a mixed-case on-disk
//! game directory, so resolution has to recover the real spelling through
//! L2's mount-prefix strip and case walk.
//!
//! Each test owns a unique temporary game directory (removed on drop) and
//! serializes on the shared VM lock, since the process-wide engine → context
//! binding is single.

mod common;

use std::path::{Path, PathBuf};

use common::vm_lock;
use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use tjs::{Tjs2Engine, TjsValue};

/// A unique temporary game directory whose final component is mixed case.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let unique = format!(
            "krkr-compat-media-prefix-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos()
        );
        let path = std::env::temp_dir().join(unique).join("MixedCaseGame");
        std::fs::create_dir_all(&path).expect("create temp game dir");
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// Write one fixture file (creating parent directories).
    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent dirs");
        }
        std::fs::write(path, bytes).expect("write fixture");
    }
}

impl Drop for GameDir {
    fn drop(&mut self) {
        if let Some(root) = self.0.parent() {
            let _ = std::fs::remove_dir_all(root);
        }
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
    fn new(dir: GameDir) -> Env {
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

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "test")
            .map_err(|error| error.to_string())
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
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

/// The script-visible name for `rel` under `dir`: `file://.` plus the
/// **lowercased** absolute host path, the form the application builds from
/// `System.dataPath`.
fn file_media_name(dir: &GameDir, rel: &str) -> String {
    let absolute = dir.path().join(rel);
    format!(
        "file://.{}",
        absolute.to_string_lossy().to_ascii_lowercase()
    )
}

#[test]
fn exec_storage_resolves_file_prefixed_lowercased_name() {
    let _lock = vm_lock();
    let dir = GameDir::new();
    dir.write("SaveData/SaveMng.tjs", b"return \"saveMng loaded\";");
    let name = file_media_name(&dir, "SaveData/SaveMng.tjs");
    // The name is fully lowercased while the on-disk path is not.
    assert!(name.starts_with("file://."));
    let env = Env::new(dir);

    // `tTVPStorageMediaManager::NormalizeStorageName` parses the
    // `media://domain/path` form and `TVPGetPlacedPath` resolves the path
    // case-insensitively against the mounted media
    // (`reference/cpp/core/base/StorageIntf.cpp:240-430`), after which
    // `TVPExecuteStorage` opens the placed path and runs it
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-729`).
    assert_eq!(
        env.eval_ok(&format!("Scripts.execStorage({name:?})")),
        TjsValue::String("saveMng loaded".into())
    );
}

#[test]
fn eval_storage_resolves_file_prefixed_lowercased_name() {
    let _lock = vm_lock();
    let dir = GameDir::new();
    dir.write("SaveData/EvalMng.tjs", b"\"evalMng loaded\"");
    let name = file_media_name(&dir, "SaveData/EvalMng.tjs");
    let env = Env::new(dir);

    // The same placed-path lookup and media-prefix normalization as
    // `execStorage` (`reference/cpp/core/base/StorageIntf.cpp:240-430`);
    // `TVPExecuteStorage` evaluates the storage as an expression
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-729,1348-1350`).
    assert_eq!(
        env.eval_ok(&format!("Scripts.evalStorage({name:?})")),
        TjsValue::String("evalMng loaded".into())
    );
}

#[test]
fn missing_file_prefixed_name_reports_the_script_spelling() {
    let _lock = vm_lock();
    let dir = GameDir::new();
    let name = file_media_name(&dir, "SaveData/Missing.tjs");
    let env = Env::new(dir);

    // The media prefix is unwrapped only for the namespace lookup; the
    // reference normalizes the name first (`reference/cpp/core/base/
    // StorageIntf.cpp:240-430`) while the port's `Scripts` diagnostic keeps
    // the script-supplied spelling.
    let message = env.error(&format!("Scripts.execStorage({name:?})"));
    assert!(
        message.contains(&name),
        "diagnostic dropped the script spelling: {message}"
    );
}

/// A write whose name carries the `file:` media prefix lands under the game
/// directory, not under a literal `file:/…` path.
///
/// Reference: `ArrayAdd::save2` opens the target through
/// `TVPCreateStream(param[0]->AsString(), TJS_BS_WRITE)`
/// (`reference/cpp/plugins/saveStruct.cpp:228-233`), which normalizes the
/// `media://domain/path` form before the storage write
/// (`reference/cpp/core/base/StorageIntf.cpp:240-430`).
#[test]
fn save2_strips_the_file_media_prefix() {
    let _lock = vm_lock();
    let dir = GameDir::new();
    let env = Env::new(dir);
    env.run("var a = ['hello']; a.save2('file:SaveData/out.txt');");
    let out = env._dir.path().join("SaveData/out.txt");
    let written =
        std::fs::read(&out).unwrap_or_else(|error| panic!("save2 did not write {out:?}: {error}"));
    assert!(!written.is_empty(), "save2 wrote an empty file");
    assert!(
        !env._dir.path().join("file:").exists(),
        "the media prefix leaked into the written path"
    );
}

/// A fully lowercased absolute save name — the shape `System.dataPath`
/// produces — must resolve to the real mixed-case on-disk directory rather
/// than creating a lowercase tree or failing on a read-only parent.
///
/// Reference: the write target goes through `TVPCreateStream`, which resolves
/// the normalized name through `TVPGetLocallyAccessibleName`
/// (`reference/cpp/core/base/StorageIntf.cpp:240-430`).
#[test]
fn save_struct_resolves_a_lowercased_absolute_name_to_the_real_path() {
    let _lock = vm_lock();
    let dir = GameDir::new();
    let save_dir = dir.path().join("SaveData");
    std::fs::create_dir_all(&save_dir).expect("create savedata dir");
    let target = save_dir.join("savecheck");
    let leaked = dir.path().to_string_lossy().to_ascii_lowercase();
    let lower = format!("file://.{}", target.to_string_lossy().to_ascii_lowercase());
    let literal = format!("{lower:?}");
    // `Env::new` takes ownership of `dir`, so every path derived from it is
    // computed above.
    let env = Env::new(dir);
    env.run(&format!(
        "var a = ['hello']; a.saveStruct({literal}, 'o0');"
    ));
    let written = std::fs::read(&target)
        .unwrap_or_else(|error| panic!("saveStruct did not write {target:?}: {error}"));
    assert!(!written.is_empty(), "saveStruct wrote an empty file");
    assert!(
        !Path::new(&leaked).exists(),
        "saveStruct created a second lowercase tree"
    );
}

/// A `saveStruct` name that carries the `file:` media prefix and a doubled
/// slash lands at the bare host path, not under a literal `file:/…` path.
///
/// `Array.saveStruct` is vendored TJS2 and opens the target through the
/// `crates/tjs` stream factory, which resolves names itself; the reference
/// resolves `TVPCreateStream` through `TVPGetLocallyAccessibleName`, which
/// strips the local `file` media prefix
/// (`reference/cpp/core/base/StorageIntf.cpp:240-430`), and the text write
/// stream then opens the placed path
/// (`reference/cpp/core/base/TextStream.cpp`).
#[test]
fn save_struct_strips_the_file_media_prefix_and_doubled_slash() {
    let _lock = vm_lock();
    let dir = GameDir::new();
    let save_dir = dir.path().join("SaveData");
    std::fs::create_dir_all(&save_dir).expect("create savedata dir");
    let target = save_dir.join("savecheck");
    let leaked = dir.path().join("file:");
    let prefixed = format!("file://.{s}//savecheck", s = save_dir.to_string_lossy());
    let literal = format!("{prefixed:?}");
    // `Env::new` takes ownership of `dir`, so every path derived from it is
    // computed above.
    let env = Env::new(dir);
    env.run(&format!(
        "var a = ['hello']; a.saveStruct({literal}, 'o0');"
    ));
    let written = std::fs::read(&target)
        .unwrap_or_else(|error| panic!("saveStruct did not write {target:?}: {error}"));
    assert!(!written.is_empty(), "saveStruct wrote an empty file");
    assert!(
        !leaked.exists(),
        "the media prefix leaked into the written path"
    );
}
