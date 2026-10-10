//! End-to-end tests for the `xp3_filter` plugin.
//!
//! Each test writes a real `xp3filter.tjs` into a temporary game directory,
//! installs the full compat surface (which defers loading the script to the
//! first archive read), mounts a synthetic XP3 archive whose contents are
//! XOR-encrypted, and reads a member back through the `Storages` native. The
//! byte-for-byte comparison proves the game script's closure ran on the
//! archive bytes.
//!
//! The process-wide engine → context binding is single, so every test holds
//! the shared VM lock.

mod common;

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use common::vm_lock;
use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use fs::xp3::{SEGM_ENCODE_RAW, XP3_MAGIC};
use tjs::{Tjs2Engine, TjsValue};

// ---------------------------------------------------------------------------
// A minimal raw-index XP3 builder (compat does not depend on `testkit`).
// ---------------------------------------------------------------------------

/// Build a raw-index XP3 archive with one raw segment per `(name, stored)`
/// entry. The stored bytes are written verbatim, so callers pass ciphertext.
fn build_xp3(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut file = Vec::new();
    file.extend_from_slice(&XP3_MAGIC);
    file.extend_from_slice(&0u64.to_le_bytes());

    let mut file_chunks: Vec<Vec<u8>> = Vec::new();
    for (name, stored) in entries {
        let start = file.len() as u64;
        file.extend_from_slice(stored);

        let units: Vec<u16> = name.encode_utf16().collect();
        let mut info = Vec::new();
        info.extend_from_slice(b"info");
        info.extend_from_slice(&((22 + units.len() * 2) as u64).to_le_bytes());
        info.extend_from_slice(&0u32.to_le_bytes());
        info.extend_from_slice(&(stored.len() as i64).to_le_bytes());
        info.extend_from_slice(&(stored.len() as i64).to_le_bytes());
        info.extend_from_slice(&(units.len() as i16).to_le_bytes());
        for unit in &units {
            info.extend_from_slice(&unit.to_le_bytes());
        }

        let mut segm = Vec::new();
        segm.extend_from_slice(b"segm");
        segm.extend_from_slice(&28u64.to_le_bytes());
        segm.extend_from_slice(&SEGM_ENCODE_RAW.to_le_bytes());
        segm.extend_from_slice(&(start as i64).to_le_bytes());
        segm.extend_from_slice(&(stored.len() as i64).to_le_bytes());
        segm.extend_from_slice(&(stored.len() as i64).to_le_bytes());

        let mut adlr = Vec::new();
        adlr.extend_from_slice(b"adlr");
        adlr.extend_from_slice(&4u64.to_le_bytes());
        adlr.extend_from_slice(&0x1234_5678u32.to_le_bytes());

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
            "krkr-compat-xp3filter-{}-{}",
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
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bound VM with a mounted storage and the `xp3_filter` plugin active.
struct Env {
    // Drop order matters: the host removes the process-wide binding before the
    // boxed engine/storage it points at are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    storage: Box<Storage>,
    dir: GameDir,
}

impl Env {
    /// Create the game directory with `xp3filter.tjs` already written, then
    /// install the compat surface.
    fn new(script: &str) -> Env {
        let dir = GameDir::new();
        std::fs::write(dir.path().join("xp3filter.tjs"), script).expect("write filter script");
        let mut storage = Box::new(
            Storage::mount_archives(
                FileSystem::host(dir.path()),
                dir.path().to_path_buf(),
                std::iter::empty::<&str>(),
            )
            .expect("mount storage"),
        );
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        // SAFETY: `storage` is boxed and outlives the binding, which `_host`
        // removes on drop.
        unsafe {
            host.services_mut()
                .attach_storage(storage.as_mut() as *mut Storage);
        }
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            storage,
            dir,
        }
    }

    /// Write a fixture file into the game directory.
    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.dir.path().join(name), bytes).expect("write fixture");
    }

    /// Mount an archive from the game directory.
    fn mount(&mut self, name: &str) {
        self.storage
            .mount_archive(self.dir.path().join(name))
            .expect("mount archive");
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "xp3-filter-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn defined(&self, expression: &str) -> bool {
        !matches!(
            self.engine.eval(&format!("typeof {expression}"), "xp3-filter-test"),
            Ok(TjsValue::String(kind)) if kind == "undefined"
        )
    }

    /// Export a storage entry to a local file and return its bytes, reading
    /// through the `Storages` native so the plugin's filter path runs.
    fn export(&self, source: &str, dest: &str) -> Vec<u8> {
        self.run(&format!("Storages.exportFile({source:?}, {dest:?});\n"));
        std::fs::read(self.dir.path().join(dest)).expect("read exported file")
    }
}

fn xor(data: &[u8], key: u8) -> Vec<u8> {
    data.iter().map(|byte| byte ^ key).collect()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

/// The game's extraction closure (using indexed buffer access) decrypts an
/// archive member end-to-end.
// The extraction filter receives the reference argument list
// `(file_hash, offset, buffer, buffer_size, file_name, ctx)`
// (`reference/cpp/plugins/xp3filter.cpp:501-510`) and rewrites the segment
// buffer in place through the marshalled buffer accessor.
#[test]
fn extraction_filter_decrypts_archive_member() {
    let _lock = vm_lock();
    let key = 0x5Au8;
    let plain = b"the secret archive contents";
    let cipher = xor(plain, key);
    let script = format!(
        "function xp3extract(file_hash, offset, buffer, buffer_size, file_name, ctx) {{\n\
         \x20\x20for (var i = 0; i < buffer_size; i++) buffer[i] = buffer[i] ^ {key};\n\
         }}\n\
         Storages.setXP3ArchiveExtractionFilter(xp3extract);\n"
    );

    let mut env = Env::new(&script);
    assert!(
        env.defined("Storages.setXP3ArchiveExtractionFilter"),
        "the extraction setter must be registered"
    );
    env.write("data.xp3", &build_xp3(&[("secret.txt", &cipher)]));
    env.mount("data.xp3");

    assert_eq!(env.export("secret.txt", "decrypted.bin"), plain);
}

/// A content filter establishes the per-entry `ctx` passed to the extraction
/// filter (`[ret, ctx]` handling from the reference).
// The content filter returns `[ret, ctx]` and the reference copies `ctx` out
// of the returned array before the extraction filter runs
// (`reference/cpp/plugins/xp3filter.cpp:484-489`).
#[test]
fn content_filter_context_reaches_extraction_filter() {
    let _lock = vm_lock();
    let key = 0x33u8;
    let plain = b"context keyed decryption";
    let cipher = xor(plain, key);
    let script = format!(
        "function xp3content(filepath, archivename, filesize) {{\n\
         \x20\x20return [0, {key}];\n\
         }}\n\
         function xp3extract(file_hash, offset, buffer, buffer_size, file_name, ctx) {{\n\
         \x20\x20for (var i = 0; i < buffer_size; i++) buffer[i] = buffer[i] ^ ctx;\n\
         }}\n\
         Storages.setXP3ArchiveContentFilter(xp3content);\n\
         Storages.setXP3ArchiveExtractionFilter(xp3extract);\n"
    );

    let mut env = Env::new(&script);
    assert!(
        env.defined("Storages.setXP3ArchiveContentFilter"),
        "the content setter must be registered"
    );
    env.write("data.xp3", &build_xp3(&[("secret.txt", &cipher)]));
    env.mount("data.xp3");

    assert_eq!(env.export("secret.txt", "decrypted.bin"), plain);
}

/// The reference `CBinaryAccessor::xor` helper is available on the marshalled
/// buffer and rewrites it in place.
// The reference `CBinaryAccessor::FuncXor` (`reference/cpp/plugins/xp3filter.cpp:266-297`)
// xors `buffer[offset .. offset+length]` with `value` in place; the port
// exposes the same `xor(offset, length, value)` on the marshalled buffer.
#[test]
fn buffer_xor_helper_decrypts_archive_member() {
    let _lock = vm_lock();
    let key = 0x77u8;
    let plain = b"xor helper decryption";
    let cipher = xor(plain, key);
    let script = format!(
        "function xp3extract(file_hash, offset, buffer, buffer_size, file_name, ctx) {{\n\
         \x20\x20buffer.xor(0, buffer_size, {key});\n\
         }}\n\
         Storages.setXP3ArchiveExtractionFilter(xp3extract);\n"
    );

    let mut env = Env::new(&script);
    env.write("data.xp3", &build_xp3(&[("secret.txt", &cipher)]));
    env.mount("data.xp3");

    assert_eq!(env.export("secret.txt", "decrypted.bin"), plain);
}

// The reference content-filter wrapper runs independently of the extraction
// filter: `TVPXP3ArchiveContentFilterWrapper` is registered and invoked on
// its own (`reference/cpp/plugins/xp3filter.cpp:465-491`) while the
// extraction wrapper only checks its own closure (`:493-530`). Installing
// *only* a content filter must therefore still invoke it — for its argument
// types (`FilePath`/`ArcName` strings, `FileSize` int64, `:475-478`) and its
// side effects — even though the archive bytes pass through untouched.
#[test]
fn content_filter_runs_without_an_extraction_filter() {
    let _lock = vm_lock();
    let cipher = xor(b"undecrypted entry", 0x5A);
    let script = "\
        global.__content_calls = 0;\n\
        global.__content_arg_types = \"\";\n\
        function xp3content(filepath, archivename, filesize) {\n\
        \x20\x20global.__content_calls = global.__content_calls + 1;\n\
        \x20\x20global.__content_arg_types = typeof filepath + \",\" + typeof archivename + \",\" + typeof filesize;\n\
        \x20\x20return [0, 1];\n\
        }\n\
        Storages.setXP3ArchiveContentFilter(xp3content);\n";

    let mut env = Env::new(script);
    env.write("data.xp3", &build_xp3(&[("entry.txt", &cipher)]));
    env.mount("data.xp3");

    // No extraction filter: the export copies the stored (cipher) bytes.
    let exported = env.export("entry.txt", "raw.bin");
    assert_eq!(exported, cipher);

    assert_eq!(
        env.engine
            .eval("global.__content_calls", "xp3-filter-test")
            .expect("content call counter"),
        tjs::TjsValue::Integer(1)
    );
    assert_eq!(
        env.engine
            .eval("global.__content_arg_types", "xp3-filter-test")
            .expect("content argument types"),
        tjs::TjsValue::String("String,String,Integer".into())
    );
}

// The reference executes `xp3filter.tjs` on a dedicated sub-engine whose
// global holds only `Debug`/`System`/`Storages`
// (`reference/cpp/plugins/xp3filter.cpp:394-421`), so its top-level
// declarations are invisible to the game script. The port wraps the source
// in a named local function for the same effect: a probe declared at the
// filter script's top level must not resolve in the main scope, while the
// setter it calls still installs the closure.
#[test]
fn filter_script_declarations_stay_local_to_the_filter_script() {
    let _lock = vm_lock();
    let script = "\
        function xp3filter_probe_function() { return 1; }\n\
        var xp3filter_probe_var = 1;\n\
        Storages.setXP3ArchiveContentFilter(function(filepath, archivename, filesize) {\n\
        \x20\x20return [0, 0];\n\
        });\n";

    let _env = Env::new(script);

    // A bare `typeof <name>` compiles to a must-exist member read and raises
    // `Member ... does not exist` for an absent global; `typeof global.<name>`
    // reports `"undefined"` instead (`tjsInterCodeExec.cpp:1130-1135`).
    let probe_fn = _env
        .engine
        .eval("typeof global.xp3filter_probe_function", "xp3-filter-test")
        .expect("typeof probe function");
    assert_eq!(probe_fn, tjs::TjsValue::String("undefined".into()));
    let probe_var = _env
        .engine
        .eval("typeof global.xp3filter_probe_var", "xp3-filter-test")
        .expect("typeof probe variable");
    assert_eq!(probe_var, tjs::TjsValue::String("undefined".into()));
}

// Both setters require at least one argument — `TJS_E_BADPARAMCOUNT`
// (`reference/cpp/plugins/xp3filter.cpp:356-357`, `:384-385`) — and a
// non-object argument raises the variant-convert error because the
// reference calls `param[0]->AsObjectClosure()`
// (`reference/cpp/core/tjs2/tjsVariant.h:719-726`).
#[test]
fn setters_reject_missing_and_non_object_arguments() {
    let _lock = vm_lock();
    let _env = Env::new("");

    for call in [
        "Storages.setXP3ArchiveExtractionFilter()".to_string(),
        "Storages.setXP3ArchiveContentFilter()".to_string(),
        "Storages.setXP3ArchiveExtractionFilter(5)".to_string(),
        "Storages.setXP3ArchiveContentFilter(5)".to_string(),
    ] {
        let error = _env
            .engine
            .exec_script(&format!("{call};"), "xp3-filter-test")
            .err()
            .unwrap_or_else(|| panic!("{call} should have raised"));
        assert!(!error.to_string().is_empty(), "{call}: empty error");
    }
}

// Passing `null` stores a null object closure, so the reference extraction
// wrapper skips the managed call entirely (`reference/cpp/plugins/xp3filter.cpp:360`,
// guard at `:500`) and the stored bytes pass through unchanged even though
// a filter had been installed earlier.
#[test]
fn clearing_the_extraction_filter_with_null_passes_bytes_through() {
    let _lock = vm_lock();
    let key = 0x5Au8;
    let plain = b"decrypted before clear";
    let cipher = xor(plain, key);
    let script = format!(
        "function xp3extract(file_hash, offset, buffer, buffer_size, file_name, ctx) {{\n\
         \x20\x20for (var i = 0; i < buffer_size; i++) buffer[i] = buffer[i] ^ {key};\n\
         }}\n\
         Storages.setXP3ArchiveExtractionFilter(xp3extract);\n"
    );

    let mut env = Env::new(&script);
    env.write("data.xp3", &build_xp3(&[("secret.txt", &cipher)]));
    env.mount("data.xp3");

    // Sanity: with the filter installed the export decrypts.
    assert_eq!(env.export("secret.txt", "step1.bin"), plain);
    // Clear the filter with `null` and export again: ciphertext now.
    env.run("Storages.setXP3ArchiveExtractionFilter(null);");
    assert_eq!(env.export("secret.txt", "step2.bin"), cipher);
}

// The reference `CBinaryAccessor::FuncXor` requires three parameters and
// returns without a result (`reference/cpp/plugins/xp3filter.cpp:266-297`):
// `xor(offset, length, value)` rewrites bytes in place and yields `void`,
// while a shorter call is a typed error. The port attaches the helper to the
// marshalled `Array` buffer (see the module docs), so the semantics are
// pinned on an array.
#[test]
fn xor_helper_is_void_and_requires_three_arguments() {
    let _lock = vm_lock();
    let env = Env::new("");

    env.run("var a = [1, 2, 3]; var r = a.xor(0, 2, 255);");
    let r = env.engine.eval("r", "xp3-filter-test").expect("xor result");
    assert_eq!(r, tjs::TjsValue::Void);
    let first = env.engine.eval("a[0]", "xp3-filter-test").expect("a[0]");
    assert_eq!(first, tjs::TjsValue::Integer(254));
    let third = env.engine.eval("a[2]", "xp3-filter-test").expect("a[2]");
    assert_eq!(third, tjs::TjsValue::Integer(3));

    let error = env
        .engine
        .exec_script("a.xor(0, 1);", "xp3-filter-test")
        .expect_err("xor with two arguments must raise");
    assert!(!error.to_string().is_empty());
}
