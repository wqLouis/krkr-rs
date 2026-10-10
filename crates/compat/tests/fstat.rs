//! Integration tests for the `fstat` plugin surface.
//!
//! The plugin attaches its `Storages` members and registers the
//! `TemporaryFiles` class during [`compat::CompatHost::install`]. `fstat` is
//! the sole owner of the 30 `Storages` members; the sibling plugin modules
//! (`dirlist`, `var_file`, `xp3_filter`) no longer attach to `Storages`, so
//! this test drives exactly the surface `fstat` registers.
//!
//! The `build_xp3` helper is a local copy of the shared `testkit` fixture
//! (compat does not depend on `testkit`), so the archive branch is exercised
//! too. Every test serializes on the shared VM lock, since the process-wide
//! engine → context binding is single.

mod common;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::vm_lock;
use compat::CompatHost;
use engine::Storage;
use flate2::Compression;
use flate2::write::ZlibEncoder;
use fs::FileSystem;
use fs::xp3::{SEGM_ENCODE_RAW, SEGM_ENCODE_ZLIB, XP3_MAGIC};
use tjs::{Tjs2Engine, TjsValue};

// ---------------------------------------------------------------------------
// A minimal raw-index XP3 builder (local copy of `testkit::build_xp3`).
// ---------------------------------------------------------------------------

/// Build a minimal raw-index XP3 archive in memory: `(name, data, zlib)`.
fn build_xp3(entries: &[(&str, &[u8], bool)]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(&XP3_MAGIC);
    file.extend_from_slice(&0u64.to_le_bytes());

    let mut file_chunks: Vec<Vec<u8>> = Vec::new();
    for (name, data, zlib) in entries {
        let (stored, flags): (Vec<u8>, u32) = if *zlib {
            let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
            enc.write_all(data).expect("zlib compress");
            (enc.finish().expect("zlib finish"), SEGM_ENCODE_ZLIB)
        } else {
            (data.to_vec(), SEGM_ENCODE_RAW)
        };
        let start = file.len() as u64;
        file.extend_from_slice(&stored);

        let units: Vec<u16> = name.encode_utf16().collect();
        let mut info = Vec::new();
        info.extend_from_slice(b"info");
        info.extend_from_slice(&((22 + units.len() * 2) as u64).to_le_bytes());
        info.extend_from_slice(&0u32.to_le_bytes());
        info.extend_from_slice(&(data.len() as i64).to_le_bytes());
        info.extend_from_slice(&(stored.len() as i64).to_le_bytes());
        info.extend_from_slice(&(units.len() as i16).to_le_bytes());
        for unit in &units {
            info.extend_from_slice(&unit.to_le_bytes());
        }

        let mut segm = Vec::new();
        segm.extend_from_slice(b"segm");
        segm.extend_from_slice(&28u64.to_le_bytes());
        segm.extend_from_slice(&flags.to_le_bytes());
        segm.extend_from_slice(&(start as i64).to_le_bytes());
        segm.extend_from_slice(&(data.len() as i64).to_le_bytes());
        segm.extend_from_slice(&(stored.len() as i64).to_le_bytes());

        let mut adlr = Vec::new();
        adlr.extend_from_slice(b"adlr");
        adlr.extend_from_slice(&4u64.to_le_bytes());
        adlr.extend_from_slice(&0u32.to_le_bytes());

        let mut chunk = Vec::new();
        chunk.extend_from_slice(b"File");
        chunk.extend_from_slice(&((info.len() + segm.len() + adlr.len()) as u64).to_le_bytes());
        chunk.extend_from_slice(&info);
        chunk.extend_from_slice(&segm);
        chunk.extend_from_slice(&adlr);
        file_chunks.push(chunk);
    }

    let mut index_data = Vec::new();
    for chunk in &file_chunks {
        index_data.extend_from_slice(chunk);
    }
    let index_ofs = file.len() as u64;
    file.push(0u8);
    file.extend_from_slice(&(index_data.len() as u64).to_le_bytes());
    file.extend_from_slice(&index_data);
    file[11..19].copy_from_slice(&index_ofs.to_le_bytes());
    file
}

// ---------------------------------------------------------------------------
// Test environment
// ---------------------------------------------------------------------------

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-fstat-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
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

    fn write(&self, name: &str, bytes: &[u8]) {
        let path = self.0.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture parent");
        }
        std::fs::write(path, bytes).expect("write fixture");
    }

    fn mkdir(&self, name: &str) {
        std::fs::create_dir_all(self.0.join(name)).expect("create fixture dir");
    }
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bound VM with (optionally) a mounted storage attached to the compat
/// services.
struct Env {
    // Drop order matters: the host removes the process-wide binding before the
    // boxed engine/storage it points at are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _storage: Option<Box<Storage>>,
    dir: Option<GameDir>,
}

impl Env {
    fn new() -> Env {
        Self::build(true)
    }

    fn without_storage() -> Env {
        Self::build(false)
    }

    fn build(with_storage: bool) -> Env {
        let (storage, dir) = if with_storage {
            let dir = GameDir::new();
            let storage = Box::new(
                Storage::mount_archives(
                    FileSystem::host(dir.path()),
                    dir.path().to_path_buf(),
                    std::iter::empty::<&str>(),
                )
                .expect("mount storage"),
            );
            (Some(storage), Some(dir))
        } else {
            (None, None)
        };
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        if let Some(storage) = storage.as_deref() {
            // SAFETY: `storage` is boxed and outlives the binding, which
            // `_host` removes on drop.
            unsafe {
                host.services_mut()
                    .attach_storage(storage as *const Storage as *mut Storage)
            };
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _storage: storage,
            dir,
        }
    }

    /// Mount an XP3 archive built from `entries` into the storage.
    fn mount_xp3(&mut self, entries: &[(&str, &[u8], bool)]) {
        let dir = self.dir.as_ref().expect("env has a game dir");
        let bytes = build_xp3(entries);
        dir.write("data.xp3", &bytes);
        let path = dir.path().join("data.xp3");
        if let Some(storage) = self._storage.as_deref_mut() {
            storage.mount_archive(path).expect("mount archive");
        }
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "fstat-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "fstat-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn error(&self, expression: &str) -> String {
        self.engine
            .eval(expression, "fstat-test")
            .expect_err(&format!("{expression:?} should have raised"))
            .to_string()
    }

    fn int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            TjsValue::Real(value) => value as i64,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn string(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    fn bool(&self, expression: &str) -> bool {
        self.int(expression) != 0
    }

    fn defined(&self, expression: &str) -> bool {
        !matches!(
            self.eval_ok(&format!("typeof {expression}")),
            TjsValue::String(kind) if kind == "undefined"
        )
    }

    /// Join an array's elements with `|` so the order-independent membership
    /// checks can assert on a substring.
    fn joined(&self, array: &str, member: &str) -> String {
        let expression = format!(
            "(function(){{ var a = {array}; var s = ''; \
             for (var i = 0; i < a.length; i++) s += a[i]{member} + '|'; \
             return s; }})()"
        );
        self.string(&expression)
    }
}

/// Set a file's modification time so the stat helpers have a known value.
fn set_mtime(path: &Path, seconds: i64) {
    let when = UNIX_EPOCH + Duration::from_secs(seconds as u64);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open fixture for times");
    file.set_times(std::fs::FileTimes::new().set_modified(when))
        .expect("set mtime");
}

fn read(dir: &GameDir, name: &str) -> Vec<u8> {
    std::fs::read(dir.path().join(name)).expect("read fixture")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The reference attaches exactly this member list to `Storages`
/// (`reference/cpp/plugins/fstat/main.cpp:918-949`), installs the
/// `FILE_ATTRIBUTE_*` globals from its post-registration script
/// (`main.cpp:43-51,1007`), registers `TemporaryFiles` with `entry` and
/// `entryFolder` (`main.cpp:987-991`) and exposes `currentPath` as a
/// read/write property (`main.cpp:909-915,947`).
#[test]
fn registration_surface_is_present() {
    let _lock = vm_lock();
    let env = Env::new();
    for member in [
        "clearStorageCaches",
        "fstat",
        "getTime",
        "setTime",
        "getLastModifiedFileTime",
        "setLastModifiedFileTime",
        "exportFile",
        "deleteFile",
        "truncateFile",
        "moveFile",
        "dirlist",
        "dirlistEx",
        "dirtree",
        "removeDirectory",
        "createDirectory",
        "createDirectoryNoNormalize",
        "changeDirectory",
        "setFileAttributes",
        "resetFileAttributes",
        "getFileAttributes",
        "selectDirectory",
        "isExistentDirectory",
        "copyFile",
        "copyFileNoNormalize",
        "isExistentStorageNoSearchNoNormalize",
        "getDisplayName",
        "getMD5HashString",
        "searchPath",
        "getTemporaryName",
    ] {
        assert!(
            env.defined(&format!("Storages.{member}")),
            "Storages.{member} is not registered"
        );
    }
    // The currentPath property getter resolves against a mounted storage.
    assert!(env.string("Storages.currentPath").ends_with('/'));

    // The reference's post-registration globals.
    assert_eq!(env.int("FILE_ATTRIBUTE_READONLY"), 0x1);
    assert_eq!(env.int("FILE_ATTRIBUTE_DIRECTORY"), 0x10);
    assert_eq!(env.int("FILE_ATTRIBUTE_TEMPORARY"), 0x100);

    // TemporaryFiles is a real instantiable class with both members.
    env.run("var tf = new TemporaryFiles();");
    assert!(env.defined("tf.entry"));
    assert!(env.defined("tf.entryFolder"));
}

/// `dirlist`/`dirlistEx` require the trailing `'/'` and a real directory
/// (`reference/cpp/plugins/fstat/main.cpp:506-525`), `dirlistEx` returns
/// `%[name, size, attrib, mtime, atime, ctime]` dictionaries
/// (`main.cpp:551-600`) and `dirtree` recurses with `/`-suffixed directory
/// names, honouring the `dironly` flag (`main.cpp:436-483`, default at
/// `:440`).
#[test]
fn directory_listing_helpers() {
    let _lock = vm_lock();
    let env = Env::new();
    let dir = env.dir.as_ref().expect("game dir");
    dir.write("data/a.txt", b"a");
    dir.write("data/b.txt", b"bb");
    dir.write("data/sub/c.txt", b"ccc");

    assert_eq!(env.int("Storages.dirlist('data/').length"), 3);
    let names = env.joined("Storages.dirlist('data/')", "");
    assert!(names.contains("a.txt|"), "dirlist missing a.txt: {names}");
    assert!(names.contains("b.txt|"), "dirlist missing b.txt: {names}");
    assert!(names.contains("sub|"), "dirlist missing sub: {names}");

    // dirlistEx carries name/size/attrib/times.
    assert_eq!(env.int("Storages.dirlistEx('data/').length"), 3);
    let detailed = env.joined("Storages.dirlistEx('data/')", ".name");
    assert!(
        detailed.contains("a.txt|"),
        "dirlistEx missing a.txt: {detailed}"
    );
    assert_eq!(
        env.int(
            "(function(){ var a = Storages.dirlistEx('data/'); \
             for (var i = 0; i < a.length; i++) \
               if (a[i].name == 'b.txt') return a[i].size; return -1; })()"
        ),
        2
    );
    assert!(env.defined("Storages.dirlistEx('data/')[0].mtime"));

    // dirtree is recursive and suffixes directories with '/'.
    assert_eq!(env.int("Storages.dirtree('data/').length"), 4);
    let tree = env.joined("Storages.dirtree('data/')", "");
    assert!(tree.contains("sub/|"), "dirtree missing sub/: {tree}");
    assert!(
        tree.contains("sub/c.txt|"),
        "dirtree missing sub/c.txt: {tree}"
    );

    // dironly keeps only directories.
    assert_eq!(env.int("Storages.dirtree('data/', true).length"), 1);
    assert_eq!(env.joined("Storages.dirtree('data/', true)", ""), "sub/|");
}

/// `createDirectory`/`removeDirectory` normalize the name, enforce the
/// trailing `'/'` (`reference/cpp/plugins/fstat/main.cpp:620-679`) and
/// report whether the operation changed anything; `isExistentDirectory`
/// is `exists && is_directory` (`main.cpp:757-765`), while `_dirlist`
/// raises for a name without the separator (`main.cpp:509-511`).
#[test]
fn directory_create_and_remove() {
    let _lock = vm_lock();
    let env = Env::new();

    assert!(env.bool("Storages.createDirectory('new/nested/')"));
    assert!(env.bool("Storages.isExistentDirectory('new/nested/')"));
    // An already-existing directory reports "not created".
    assert!(!env.bool("Storages.createDirectory('new/nested/')"));
    assert!(env.bool("Storages.createDirectoryNoNormalize('CaseDir/')"));

    assert!(env.bool("Storages.removeDirectory('new/nested/')"));
    assert!(!env.bool("Storages.isExistentDirectory('new/nested/')"));
    assert!(!env.bool("Storages.isExistentDirectory('absent/')"));
    assert!(!env.bool("Storages.removeDirectory('absent/')"));

    // The trailing-separator rule is enforced.
    assert!(
        env.error("Storages.dirlist('data')")
            .contains("must be specified")
    );
}

/// `fstat` returns `size` plus `Date` timestamps (`reference/cpp/plugins/fstat/main.cpp:260-291`,
/// dates stored by `Date.setTime` only when `filetime > 0`,
/// `main.cpp:112-116`), `getTime` the timestamps without `size`
/// (`main.cpp:222-248`) and `setTime` always reports success
/// (`main.cpp:305-329`), restoring the file time from the `mtime` `Date`
/// (`main.cpp:208-220`).
///
/// `getLastModifiedFileTime` is the reference's *inverted* stat check:
/// `TVP_stat` returns true on success
/// (`reference/cpp/core/environ/linux/Platform.cpp:252-264`), so
/// `if(TVP_stat(...)) return 0;` yields **0 for every file** while a failed
/// stat falls through to the zero-initialised struct (`main.cpp:331-338`).
/// The file times themselves are still settable — and visible through
/// `fstat`/`getTime`.
#[test]
fn file_statistics_and_times() {
    let _lock = vm_lock();
    let env = Env::new();
    let dir = env.dir.as_ref().expect("game dir");
    dir.write("a.txt", b"hello");
    set_mtime(&dir.path().join("a.txt"), 1_600_000_000);

    // fstat reports size plus the three timestamps. The reference stores
    // them verbatim via `Date.setTime`, so `Date.getTime()` *is* the file
    // time (truncated to a multiple of 1000 by the Date round-trip).
    assert_eq!(env.int("Storages.fstat('a.txt').size"), 5);
    assert_eq!(
        env.int("Storages.fstat('a.txt').mtime.getTime()"),
        1_600_000_000
    );
    // …but the fast reader is reference-broken and always reports 0.
    assert_eq!(env.int("Storages.getLastModifiedFileTime('a.txt')"), 0);

    // getTime reports the timestamps without size.
    assert_eq!(
        env.int("Storages.getTime('a.txt').mtime.getTime()"),
        1_600_000_000
    );
    assert_eq!(
        env.eval_ok("typeof Storages.getTime('a.txt').size"),
        TjsValue::String("undefined".into())
    );

    // setTime hands `Date.getTime()` straight to `TVP_utime` — no unit
    // conversion — so `Date.setTime(v)` puts `v` into the file time.
    env.run(
        "var times = %[]; \
         times.mtime = new Date(); times.mtime.setTime(1000000000);",
    );
    assert!(env.bool("Storages.setTime('a.txt', times)"));
    assert_eq!(
        env.int("Storages.fstat('a.txt').mtime.getTime()"),
        1_000_000_000
    );
    assert_eq!(env.int("Storages.getLastModifiedFileTime('a.txt')"), 0);
    // The reference restores the time from a `Date` object only.
    env.run(
        "var times2 = %[]; \
         times2.mtime = new Date(); times2.mtime.setTime(1400000000);",
    );
    assert!(env.bool("Storages.setTime('a.txt', times2)"));
    assert_eq!(
        env.int("Storages.getTime('a.txt').mtime.getTime()"),
        1_400_000_000
    );

    // setLastModifiedFileTime is the high-speed variant (raw seconds).
    assert!(env.bool("Storages.setLastModifiedFileTime('a.txt', 1500000000)"));
    assert_eq!(
        env.int("Storages.fstat('a.txt').mtime.getTime()"),
        1_500_000_000
    );
    assert_eq!(env.int("Storages.getLastModifiedFileTime('a.txt')"), 0);

    // A missing file has no timestamps.
    assert_eq!(
        env.eval_ok("typeof Storages.fstat('missing.txt').mtime"),
        TjsValue::String("undefined".into())
    );
    assert_eq!(
        env.int("Storages.getLastModifiedFileTime('missing.txt')"),
        0
    );
}

/// `copyFile`/`copyFileNoNormalize` resolve placed versus raw names
/// (`reference/cpp/plugins/fstat/main.cpp:774-810`), `moveFile` copies then
/// deletes (`main.cpp:417-434`), `deleteFile` only touches placed local
/// files (`main.cpp:382-402`), `truncateFile` warns and reports success
/// (`main.cpp:405-410`), `exportFile` streams a storage entry and throws
/// `cannot open readfile/storefile` (`main.cpp:351-379`),
/// `getDisplayName` returns `""` for a missing file (`main.cpp:826-833`)
/// and `isExistentStorageNoSearchNoNormalize` checks without normalizing
/// (`main.cpp:818-820`).
#[test]
fn file_copy_move_delete_export() {
    let _lock = vm_lock();
    let env = Env::new();
    let dir = env.dir.as_ref().expect("game dir");
    dir.write("src.txt", b"payload");

    assert!(env.bool("Storages.copyFile('src.txt', 'dst.txt')"));
    assert_eq!(read(dir, "dst.txt"), b"payload");

    assert!(env.bool("Storages.copyFileNoNormalize('src.txt', 'CaseDst.txt', true)"));
    assert_eq!(read(dir, "CaseDst.txt"), b"payload");

    assert!(env.bool("Storages.moveFile('dst.txt', 'moved.txt')"));
    assert!(!dir.path().join("dst.txt").exists());
    assert_eq!(read(dir, "moved.txt"), b"payload");

    assert!(env.bool("Storages.deleteFile('moved.txt')"));
    assert!(!dir.path().join("moved.txt").exists());
    assert!(!env.bool("Storages.deleteFile('moved.txt')"));

    // truncateFile is a reference no-op on every platform: it warns and
    // reports success without touching the file.
    dir.write("cut.txt", b"0123456789");
    assert!(env.bool("Storages.truncateFile('cut.txt', 4)"));
    assert_eq!(read(dir, "cut.txt"), b"0123456789");

    assert_eq!(env.string("Storages.getDisplayName('src.txt')"), "src.txt");
    assert_eq!(env.string("Storages.getDisplayName('missing.txt')"), "");
    assert!(env.bool("Storages.isExistentStorageNoSearchNoNormalize('src.txt')"));
    assert!(!env.bool("Storages.isExistentStorageNoSearchNoNormalize('missing.txt')"));

    // exportFile copies a storage entry to a local file.
    env.run("Storages.exportFile('src.txt', 'exported.bin');");
    assert_eq!(read(dir, "exported.bin"), b"payload");
    let message = env.error("Storages.exportFile('missing.txt', 'x.bin')");
    assert!(message.contains("cannot"), "unexpected error: {message}");
}

/// `getMD5HashString` requires one argument (`reference/cpp/plugins/fstat/main.cpp:842-844`),
/// raises `cannot open : …` for an unreadable entry (`main.cpp:845-851`)
/// and renders the digest with the lowercase hex alphabet
/// (`main.cpp:871-881`).
#[test]
fn md5_hash_string() {
    let _lock = vm_lock();
    let env = Env::new();
    let dir = env.dir.as_ref().expect("game dir");
    dir.write("abc.txt", b"abc");

    assert_eq!(
        env.string("Storages.getMD5HashString('abc.txt')"),
        "900150983cd24fb0d6963f7d28e17f72"
    );
    assert!(
        env.error("Storages.getMD5HashString('missing.txt')")
            .contains("cannot")
    );
}

/// The attribute members are reference no-ops: `setFileAttributes`/
/// `resetFileAttributes` warn and report success, `getFileAttributes`
/// returns `0` (`reference/cpp/plugins/fstat/main.cpp:700-738`);
/// `selectDirectory` validates one argument, warns and returns `void`
/// (`main.cpp:742-749`); `searchPath` returns the normalized absolute path
/// or `void` when nothing matched (`main.cpp:884-906`, cleared result at
/// `:901`); `clearStorageCaches` is attached as a plain method
/// (`main.cpp:919`).
#[test]
fn attributes_search_and_noops() {
    let _lock = vm_lock();
    let env = Env::new();
    let dir = env.dir.as_ref().expect("game dir");
    dir.write("a.txt", b"a");
    dir.mkdir("folder");

    // The attribute members are reference no-ops: the getter returns 0 and
    // the setters report success without changing anything.
    assert_eq!(env.int("Storages.getFileAttributes('a.txt')"), 0);
    assert_eq!(env.int("Storages.getFileAttributes('folder/')"), 0);
    assert_eq!(env.int("Storages.getFileAttributes('missing.txt')"), 0);
    assert!(env.bool("Storages.setFileAttributes('a.txt', 1)"));
    assert_eq!(env.int("Storages.getFileAttributes('a.txt')"), 0);
    assert!(env.bool("Storages.resetFileAttributes('a.txt', 1)"));
    assert_eq!(env.int("Storages.getFileAttributes('a.txt')"), 0);

    // searchPath finds a regular file on an explicit search path.
    let search_dir = dir.path().join("searchable");
    std::fs::create_dir_all(&search_dir).expect("create search dir");
    std::fs::write(search_dir.join("found.txt"), b"found").expect("write found");
    let query = format!(
        "Storages.searchPath('found.txt', '{}')",
        search_dir.display()
    );
    assert!(env.string(&query).ends_with("found.txt"));
    assert_eq!(
        env.eval_ok(&format!(
            "Storages.searchPath('missing.txt', '{}')",
            search_dir.display()
        )),
        TjsValue::Void
    );

    // selectDirectory is a reference no-op: it warns and returns void.
    assert_eq!(
        env.eval_ok("Storages.selectDirectory(%[name: ''])"),
        TjsValue::Void
    );
    assert_eq!(env.eval_ok("Storages.clearStorageCaches()"), TjsValue::Void);
}

/// `TemporaryFiles.entry`/`entryFolder` validate existence and kind — a
/// regular, openable file versus a directory — without recording anything
/// (`reference/cpp/plugins/fstat/main.cpp:952-984`), and the class is
/// registered with a constructor and both methods (`main.cpp:987-991`).
#[test]
fn temporary_files_class() {
    let _lock = vm_lock();
    let env = Env::new();
    let dir = env.dir.as_ref().expect("game dir");
    dir.write("a.txt", b"a");
    dir.mkdir("folder");

    env.run("var tf = new TemporaryFiles();");
    assert!(env.bool("tf.entry('a.txt')"));
    assert!(!env.bool("tf.entry('folder/')"));
    assert!(!env.bool("tf.entry('missing.txt')"));
    assert!(env.bool("tf.entryFolder('folder/')"));
    assert!(!env.bool("tf.entryFolder('a.txt')"));
    assert!(!env.bool("tf.entryFolder('missing/')"));
}

/// `currentPath` is a read/write property (`reference/cpp/plugins/fstat/main.cpp:947`)
/// whose getter normalizes the app path (`main.cpp:909-913`); the setter
/// forwards to `changeDirectory` (`main.cpp:915`), a warn-and-succeed no-op
/// (`main.cpp:683-691`). `getTemporaryName` binds the core
/// `TVPGetTemporaryName` (`main.cpp:948`), which builds
/// `…/krkr_<12 hex>_<counter>_<pid>` (`reference/cpp/core/base/impl/StorageImpl.cpp:468-481`).
#[test]
fn temporary_name_and_current_path() {
    let _lock = vm_lock();
    let env = Env::new();

    let path = env.string("Storages.currentPath");
    assert!(
        path.ends_with('/'),
        "currentPath should end with '/': {path}"
    );

    let name = env.string("Storages.getTemporaryName()");
    assert!(name.contains("krkr_"), "temporary name: {name}");
    let other = env.string("Storages.getTemporaryName()");
    assert_ne!(name, other, "temporary names must be unique");
}

/// The raw callbacks check their own arity (`reference/cpp/plugins/fstat/main.cpp:262-263,307-308,438-439,842-844,886-887`)
/// and the `NCB_METHOD` members go through ncbind's `doInvoke`, which
/// rejects shorter calls (`reference/cpp/core/plugin/ncbind.hpp:1178`);
/// the reference renders that as "Invalid argument count"
/// (`reference/cpp/core/tjs2/tjsError.h:268`), this port as a typed error
/// naming the member.
#[test]
fn missing_arguments_are_typed_errors() {
    let _lock = vm_lock();
    let env = Env::new();
    assert!(env.error("Storages.fstat()").contains("requires"));
    assert!(env.error("Storages.setTime('a.txt')").contains("requires"));
    assert!(
        env.error("Storages.exportFile('a.txt')")
            .contains("requires")
    );
    assert!(env.error("Storages.dirlist()").contains("requires"));
    assert!(env.error("Storages.searchPath()").contains("requires"));

    // `NCB_METHOD` signatures: `setFileAttributes(ttstr, DWORD)` and
    // `resetFileAttributes(ttstr, DWORD)` take two, `copyFileNoNormalize`
    // three (`reference/cpp/plugins/fstat/main.cpp:700,713,800`),
    // `truncateFile` two (`main.cpp:405`), `deleteFile` one (`main.cpp:382`).
    for (expression, expected) in [
        (
            "Storages.setFileAttributes('a.txt')",
            "requires 2 arguments",
        ),
        (
            "Storages.resetFileAttributes('a.txt')",
            "requires 2 arguments",
        ),
        (
            "Storages.copyFileNoNormalize('a.txt', 'b.txt')",
            "requires 3 arguments",
        ),
        ("Storages.truncateFile('a.txt')", "requires 2 arguments"),
        ("Storages.deleteFile()", "requires 1 argument"),
        ("Storages.getLastModifiedFileTime()", "requires 1 argument"),
    ] {
        let message = env.error(expression);
        assert!(
            message.contains(expected),
            "{expression:?}: unexpected message: {message}"
        );
    }
}

/// Every storage-backed member routes through the reference's storage
/// layer — `TVPGetPlacedPath`/`TVPGetLocalName` for `fstat`
/// (`reference/cpp/plugins/fstat/main.cpp:260-264,153-154`) — which always
/// exists in the reference. The port binds that layer to an attached L2
/// storage, so the *absence* of one surfaces as a typed error here (D11)
/// instead of a null dereference.
#[test]
fn storage_backed_members_require_an_attached_storage() {
    let _lock = vm_lock();
    let env = Env::without_storage();
    for expression in [
        "Storages.fstat('x')",
        "Storages.getTime('x')",
        "Storages.dirlist('x/')",
        "Storages.getTemporaryName()",
        "Storages.copyFile('a', 'b')",
    ] {
        let message = env.error(expression);
        assert!(
            message.contains("no storage") || message.contains("storage"),
            "{expression:?} should report the missing storage, got: {message}"
        );
    }
}

/// For an archive member `fstat` returns only the uncompressed `size` —
/// no timestamps (`reference/cpp/plugins/fstat/main.cpp:264-289`) — and
/// `getTime` is disk-only because `getLocalName` cannot resolve an archive
/// path (`main.cpp:153-154,222-231`). `exportFile` and
/// `getMD5HashString` read the archive stream itself (`main.cpp:351-361,845-860`).
#[test]
fn archive_entries_report_size_only() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.mount_xp3(&[("data/inside.ks", b"hello world", false)]);

    // fstat returns the uncompressed size and no timestamps for an archive.
    assert_eq!(
        env.int("Storages.fstat('data.xp3>data/inside.ks').size"),
        11
    );
    assert_eq!(
        env.eval_ok("typeof Storages.fstat('data.xp3>data/inside.ks').mtime"),
        TjsValue::String("undefined".into())
    );
    // getTime is disk-only, exactly like the reference: an archive member has
    // no local name, so `TVPGetLocalName` throws
    // (`reference/cpp/core/base/impl/StorageImpl.cpp:408-413`).
    assert!(
        env.error("Storages.getTime('data.xp3>data/inside.ks')")
            .contains("Cannot get local name")
    );
    // exportFile and getMD5HashString read archive members.
    env.run("Storages.exportFile('data.xp3>data/inside.ks', 'from_archive.bin');");
    let dir = env.dir.as_ref().expect("game dir");
    assert_eq!(read(dir, "from_archive.bin"), b"hello world");
    assert_eq!(
        env.string("Storages.getMD5HashString('data.xp3>data/inside.ks')"),
        "5eb63bbbe01eeed093cb22bb8f5acdc3"
    );
}

/// `setDirListInfo` writes `mtime`/`ctime`/`atime` as raw epoch-seconds
/// *integers* (`tTJSVariant vmtime = mtime;`,
/// `reference/cpp/plugins/fstat/main.cpp:577-597`) — unlike `fstat`, which
/// wraps its times in `Date` objects (`main.cpp:112-133`) — `size` only for
/// regular files (`main.cpp:565`), `attrib` as the permission bits
/// (`main.cpp:571`), and directory names carry no `/` suffix, unlike
/// `dirlist` (`fstat/manaul.tjs:64-75`).
#[test]
fn dirlist_ex_writes_raw_epoch_second_timestamps() {
    let _lock = vm_lock();
    let env = Env::new();
    let dir = env.dir.as_ref().expect("game dir");
    dir.write("data/rt.txt", b"abc");
    dir.mkdir("data/sub");
    set_mtime(&dir.path().join("data/rt.txt"), 1_600_000_000);

    env.run(
        "var entries = Storages.dirlistEx('data/'); \
         var file = null; var folderName = 'missing'; \
         for (var i = 0; i < entries.length; i++) { \
           if (entries[i].name == 'rt.txt') file = entries[i]; \
           if (entries[i].name == 'sub') folderName = entries[i].name; \
         }",
    );
    // The timestamps are plain integers in epoch seconds…
    assert_eq!(env.string("typeof file.mtime"), "Integer");
    assert_eq!(env.int("file.mtime"), 1_600_000_000);
    assert_eq!(env.string("typeof file.ctime"), "Integer");
    assert_eq!(env.string("typeof file.atime"), "Integer");
    assert_eq!(env.int("file.size"), 3);
    assert!(env.int("file.attrib") > 0);
    // …and directory names are not suffixed with '/'.
    assert_eq!(env.string("folderName"), "sub");
}

/// `setFileTime` hands `TVP_utime` the epoch whenever the dictionary's
/// `mtime` is not a usable `Date` (`hasC ? m : 0`,
/// `reference/cpp/plugins/fstat/main.cpp:215-218`; the gate is
/// `restoreDate` at `main.cpp:135-140`), and `storeDate` skips times `<= 0`
/// (`main.cpp:115`) — so afterwards no `mtime` member exists at all —
/// while `setTime` still reports `true` (`main.cpp:305-329`).
#[test]
fn set_time_without_a_date_clobbers_to_the_epoch() {
    let _lock = vm_lock();
    let env = Env::new();
    let dir = env.dir.as_ref().expect("game dir");
    dir.write("a.txt", b"hello");
    set_mtime(&dir.path().join("a.txt"), 1_600_000_000);

    assert_eq!(
        env.int("Storages.fstat('a.txt').mtime.getTime()"),
        1_600_000_000
    );

    // An empty dictionary: `mtime` is missing → the file goes to epoch.
    assert!(env.bool("Storages.setTime('a.txt', %[])"));
    assert_eq!(
        env.eval_ok("typeof Storages.fstat('a.txt').mtime"),
        TjsValue::String("undefined".into())
    );
    assert_eq!(
        env.eval_ok("typeof Storages.getTime('a.txt').mtime"),
        TjsValue::String("undefined".into())
    );
    // The file itself is untouched otherwise.
    assert_eq!(env.int("Storages.fstat('a.txt').size"), 5);

    // A non-Date `mtime` fails `Date.getTime()` → epoch as well.
    set_mtime(&dir.path().join("a.txt"), 1_600_000_000);
    assert!(env.bool("Storages.setTime('a.txt', %[mtime: 'nope'])"));
    assert_eq!(
        env.eval_ok("typeof Storages.fstat('a.txt').mtime"),
        TjsValue::String("undefined".into())
    );
}

/// `_dirlist` throws `"Directory not found."` for a missing path
/// (`reference/cpp/plugins/fstat/main.cpp:513-521`), while `_dirtree`
/// quietly returns an empty array for one (`main.cpp:457-460`).
#[test]
fn directory_listing_reports_missing_directories() {
    let _lock = vm_lock();
    let env = Env::new();

    assert!(
        env.error("Storages.dirlist('absent/')")
            .contains("Directory not found.")
    );
    assert!(
        env.error("Storages.dirlistEx('absent/')")
            .contains("Directory not found.")
    );
    assert_eq!(env.int("Storages.dirtree('absent/').length"), 0);
}

/// The `currentPath` setter forwards to `changeDirectory`, a warn-and-succeed
/// no-op on non-Windows hosts (`reference/cpp/plugins/fstat/main.cpp:683-691,915`);
/// the getter keeps returning the normalized game path (`main.cpp:909-913`).
#[test]
fn current_path_write_is_the_reference_no_op() {
    let _lock = vm_lock();
    let env = Env::new();

    let before = env.string("Storages.currentPath");
    env.run("Storages.currentPath = 'C:/elsewhere/';");
    assert_eq!(env.string("Storages.currentPath"), before);
}
