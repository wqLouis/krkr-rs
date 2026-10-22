//! Storage-backed TJS text and binary streams.
//!
//! The reference wires `TJSCreateTextStreamForRead` /
//! `TJSCreateBinaryStreamForRead` to the storage layer
//! (`reference/cpp/core/base/ScriptMgnIntf.cpp:464-469`), so `Array.load` and
//! `Array.loadStruct` resolve `archive.xp3>member` through the storage, and a
//! name the storage cannot place raises `TVPCannotOpenStorage`
//! (`reference/cpp/core/base/StorageIntf.cpp:1131-1148`,
//! `reference/cpp/core/tjs2/tjsArray.cpp:254-345`). The compat host installs
//! the storage read seam; these tests pin the observable outcomes: an archive
//! text member loads, a missing member raises, and a binary member reads.
//!
//! Every test serializes on the shared VM lock, since the process-wide
//! engine → context binding is single.

mod common;

use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::ptr;
use std::time::{SystemTime, UNIX_EPOCH};

use common::vm_lock;
use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use testkit::build_xp3;
use tjs::{Tjs2Engine, TjsValue};

// The binary stream factories live in cpp/streams.cpp; this helper reads a
// whole stream through the same wired factory the VM uses.
unsafe extern "C" {
    fn tjs2_test_read_binary_stream_all(
        name_utf8: *const c_char,
        mode_utf8: *const c_char,
        out_data: *mut *mut u8,
        out_len: *mut usize,
        out_error: *mut *mut c_char,
    ) -> c_int;
    fn tjs2_free_storage_bytes(data: *mut c_void);
    fn tjs2_free_string(s: *mut c_char);
}

/// A unique temporary game directory, removed on drop.
struct GameDir(PathBuf);

impl GameDir {
    fn new() -> Self {
        let mut path = std::env::temp_dir();
        let unique = format!(
            "krkr-compat-streams-{}-{}",
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

/// A bound VM with a mounted storage attached to the compat services.
struct Env {
    // Drop order matters: the host clears the process-wide binding (and the
    // stream storage seam) before the boxed engine/storage are freed.
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    storage: Box<Storage>,
    dir: GameDir,
}

impl Env {
    fn new() -> Env {
        let dir = GameDir::new();
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
            storage,
            dir,
        }
    }

    /// Build `data.xp3` from `entries` and mount it.
    fn mount_xp3(&mut self, entries: &[(&str, &[u8], bool)]) {
        let path = self.dir.path().join("data.xp3");
        std::fs::write(&path, build_xp3(entries)).expect("write archive");
        self.storage.mount_archive(path).expect("mount archive");
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "streams-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.engine
            .eval(expression, "streams-test")
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn string(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    fn int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    /// Read a whole binary stream through the wired read factory.
    fn try_read_binary(&self, name: &str, mode: &str) -> Result<Vec<u8>, String> {
        let name = CString::new(name).expect("name is NUL-free");
        let mode = CString::new(mode).expect("mode is NUL-free");
        let mut data: *mut u8 = ptr::null_mut();
        let mut len: usize = 0;
        let mut error: *mut c_char = ptr::null_mut();
        // SAFETY: the C strings live for the call; the helper writes a malloc'd
        // buffer into `data`/`len` and, on failure, a malloc'd message.
        let rc = unsafe {
            tjs2_test_read_binary_stream_all(
                name.as_ptr(),
                mode.as_ptr(),
                &mut data,
                &mut len,
                &mut error,
            )
        };
        if rc != 0 {
            let message = if error.is_null() {
                String::new()
            } else {
                // SAFETY: the helper stores a NUL-terminated malloc'd string.
                unsafe { CStr::from_ptr(error) }
                    .to_string_lossy()
                    .into_owned()
            };
            if !error.is_null() {
                // SAFETY: the helper allocated it with malloc.
                unsafe { tjs2_free_string(error) };
            }
            return Err(message);
        }
        // SAFETY: the helper returns `len` readable bytes in a malloc'd buffer.
        let bytes = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
        // SAFETY: the helper allocated the buffer with malloc.
        unsafe { tjs2_free_storage_bytes(data as *mut c_void) };
        Ok(bytes)
    }

    fn read_binary(&self, name: &str, mode: &str) -> Vec<u8> {
        self.try_read_binary(name, mode)
            .unwrap_or_else(|error| panic!("binary read of {name:?} failed: {error}"))
    }
}

/// A text payload encoded the way the shipped archive member is:
/// UTF-16LE with a byte-order mark (`data1080.xp3>clickglyph.asd`).
fn utf16le_with_bom(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

// reference/cpp/core/base/ScriptMgnIntf.cpp:464-469 wires the text read factory
// to the storage, and reference/cpp/core/base/StorageIntf.cpp:1131-1148
// resolves `arc.xp3>member`; the port must return the member's lines rather
// than an empty array (reference/cpp/core/tjs2/tjsArray.cpp:254-345).
#[test]
fn array_load_reads_a_storage_member() {
    let _lock = vm_lock();
    let mut env = Env::new();
    let asd = ";! clipleft=0 cliptop=0 clipwidth=60 clipheight=60\r\n\
               @clip left=0 top=0\r\n\
               @clip left=0 top=60\r\n";
    env.mount_xp3(&[("clickglyph.asd", &utf16le_with_bom(asd), false)]);

    env.run("var lines = []; lines.load('data.xp3>clickglyph.asd');");

    assert_eq!(env.int("lines.length"), 3);
    assert_eq!(
        env.string("lines[0]"),
        ";! clipleft=0 cliptop=0 clipwidth=60 clipheight=60"
    );
    assert_eq!(env.string("lines[2]"), "@clip left=0 top=60");
}

// reference/cpp/core/base/StorageIntf.cpp:1131-1148 raises
// `TVPCannotOpenStorage` when the storage cannot place the name, so the
// factory never returns an empty stream (reference/cpp/core/tjs2/tjsArray.cpp:254-345).
#[test]
fn array_load_of_a_missing_member_raises() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.mount_xp3(&[("present.asd", b";! present\r\n", false)]);

    let error = env
        .engine
        .exec_script(
            "var lines = []; lines.load('data.xp3>missing.asd');",
            "streams-test",
        )
        .expect_err("a missing storage member must raise");
    let message = error.to_string();
    assert!(message.contains("Cannot open storage"), "{message}");

    // A literal file whose name contains the archive delimiter is not a
    // storage name: the archive reference must not fall back to it.
    std::fs::write(
        env.dir.path().join("data.xp3>literal.asd"),
        b";! literal\r\n",
    )
    .expect("write literal fixture");
    let error = env
        .engine
        .exec_script(
            "var literal = []; literal.load('data.xp3>literal.asd');",
            "streams-test",
        )
        .expect_err("an archive reference must not read a literal `>` file");
    let message = error.to_string();
    assert!(message.contains("Cannot open storage"), "{message}");
}

// reference/cpp/core/base/ScriptMgnIntf.cpp:464-469 wires the binary read
// factory to the storage as well; the archive member's bytes must come back.
#[test]
fn binary_read_reads_a_storage_member() {
    let _lock = vm_lock();
    let mut env = Env::new();
    let payload: &[u8] = b"TJS\x00storage\xffpayload";
    // Compressed in the archive, so the read also exercises the storage's
    // extraction filter.
    env.mount_xp3(&[("blob.bin", payload, true)]);

    assert_eq!(env.read_binary("data.xp3>blob.bin", ""), payload);
    // The mode's `oN` offset applies to the in-memory stream
    // (reference/cpp/core/base/BinaryStream.cpp:57-76).
    assert_eq!(env.read_binary("data.xp3>blob.bin", "o4"), &payload[4..]);
    // A member the archive does not have raises through the same factory.
    assert!(env.try_read_binary("data.xp3>absent.bin", "").is_err());
}

// reference/cpp/core/base/StorageIntf.cpp:1150-1153: a write whose normalized
// name still carries the archive delimiter raises `TVPCannotWriteToArchive`,
// so no literal file named with `>` is created.
#[test]
fn writes_to_an_archive_member_raise() {
    let _lock = vm_lock();
    let env = Env::new();
    let archive = format!(
        "data-{}-{}.xp3",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    );
    let names = [format!("{archive}>out.txt"), format!("{archive}>out.bin")];
    let scripts = [
        format!("var a = ['x']; a.save('{}');", names[0]),
        format!("var a = ['x']; a.saveStruct('{}');", names[0]),
        format!("var a = ['x']; a.saveStruct('{}', 'b');", names[1]),
    ];
    for script in &scripts {
        let error = env
            .engine
            .exec_script(script, "streams-test")
            .expect_err("a write into an archive member must raise");
        assert!(
            error.to_string().contains("Cannot write to archive"),
            "{script}: {error}"
        );
    }
    for entry in std::fs::read_dir(env.dir.path()).expect("read game dir") {
        let name = entry.expect("dir entry").file_name();
        assert!(
            !name.to_string_lossy().contains('>'),
            "wrote a literal archive-name file {name:?}"
        );
    }
    let cwd = std::env::current_dir().expect("process working directory");
    for name in &names {
        assert!(
            !cwd.join(name).exists(),
            "an archive write must not create {name:?} in the working directory"
        );
    }
}

// The reference resolves a relative write through the storage's current path.
// The compat host points the stream factories at the attached storage's game
// directory, so the save lands under the game path, never the process CWD.
#[test]
fn relative_write_lands_in_the_game_dir() {
    let _lock = vm_lock();
    let env = Env::new();
    let name = format!(
        "relative-{}-{}.txt",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    );
    env.run(&format!("var a = ['hello', 'world']; a.save('{name}');"));

    let written = env.dir.path().join(&name);
    assert!(
        written.is_file(),
        "a relative save must land under the game directory"
    );
    let cwd = std::env::current_dir().expect("process working directory");
    assert!(
        !cwd.join(&name).exists(),
        "a relative save must not use the process working directory"
    );

    env.run(&format!("var b = []; b.load('{name}');"));
    assert_eq!(env.string("b[0] + '|' + b[1]"), "hello|world");
}

// reference/cpp/core/base/TextStream.cpp:75-80 seeks to the mode's `oN`
// offset before reading; `tTJSBinaryStream::SetPosition` raises the TJS seek
// error past the end and reads zero bytes exactly at the end.
#[test]
fn text_stream_offset_past_the_end_raises() {
    let _lock = vm_lock();
    let mut env = Env::new();
    let bytes = utf16le_with_bom(";! header\r\n");
    env.mount_xp3(&[("offset.asd", &bytes, false)]);

    env.run(&format!(
        "var at_end = []; at_end.load('data.xp3>offset.asd', 'o{}');",
        bytes.len()
    ));
    assert_eq!(env.int("at_end.length"), 0);

    let past = format!("o{}", bytes.len() + 1);
    let error = env
        .engine
        .exec_script(
            &format!("var past = []; past.load('data.xp3>offset.asd', '{past}');"),
            "streams-test",
        )
        .expect_err("an offset past the end must raise");
    assert!(error.to_string().contains("Seek error"), "{error}");
}

// reference/cpp/core/base/BinaryStream.cpp:27-49 applies the same `oN` seek
// through `SetPosition`; the binary read must not silently return a short or
// empty stream for an offset it cannot reach.
#[test]
fn binary_stream_offset_past_the_end_raises() {
    let _lock = vm_lock();
    let mut env = Env::new();
    let payload: &[u8] = b"0123456789";
    env.mount_xp3(&[("off.bin", payload, false)]);

    assert_eq!(
        env.read_binary("data.xp3>off.bin", &format!("o{}", payload.len())),
        Vec::<u8>::new()
    );
    let past = format!("o{}", payload.len() + 1);
    let error = env
        .try_read_binary("data.xp3>off.bin", &past)
        .expect_err("an offset past the end must raise");
    assert!(error.contains("Seek error"), "{error}");
}

// The reference creates the missing parent folders of a file it is about to
// create: `tTVPLocalFileStream` checks the target's parent directory and
// calls `TVPCreateFolders` (mkdir -p) before the file is created
// (`reference/cpp/core/base/impl/StorageImpl.cpp:618-641`). A write into a
// directory that does not exist yet therefore lands in the game directory
// instead of failing with "No such file or directory" — the failure that
// made a game's save report a failure while writing no file.
#[test]
fn a_write_into_a_new_subdirectory_creates_it() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run("var a = ['hello', 'world']; a.save('savedata/sub/out.txt');");

    let written = env.dir.path().join("savedata/sub/out.txt");
    assert!(
        written.is_file(),
        "the write must create the parent directories"
    );
    // The payload round-trips through the same stream factories.
    env.run("var b = []; b.load('savedata/sub/out.txt');");
    assert_eq!(env.string("b[0] + '|' + b[1]"), "hello|world");
}

// The built-in `Array.saveStruct` writes through the same text write factory
// (`reference/cpp/core/tjs2/tjsArray.cpp:440-489`), so it creates the parent
// folders of a target that does not exist yet as well
// (`reference/cpp/core/base/impl/StorageImpl.cpp:618-641`). This is the
// path a game's save data takes.
#[test]
fn save_struct_into_a_new_subdirectory_creates_it() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run("var a = ['x', 7]; a.saveStruct('savedata/sub/struct.txt');");

    let written = env.dir.path().join("savedata/sub/struct.txt");
    assert!(
        written.is_file(),
        "the save must create the parent directories"
    );
    // The binary struct form is written and read through the binary stream
    // factories, which create the parent folders the same way.
    env.run("var c = ['x', 7]; c.saveStruct('savedata/sub/struct.bin', 'b');");
    let binary = env.dir.path().join("savedata/sub/struct.bin");
    assert!(
        binary.is_file(),
        "the binary save must create the parent directories"
    );
    env.run("var d = []; d.loadStruct('savedata/sub/struct.bin');");
    assert_eq!(env.string("d[0]"), "x");
    assert_eq!(env.int("d[1]"), 7);
}

// A write target that cannot be opened raises the storage open error instead
// of producing a closed stream that silently drops the payload. The
// reference creates the missing parent folders of a file it is about to
// create (`tTVPLocalFileStream`,
// `reference/cpp/core/base/impl/StorageImpl.cpp:618-641`), so the failure is
// reached through a parent that is a regular file, where the folder tree
// cannot be created.
#[test]
fn write_open_failure_raises() {
    let _lock = vm_lock();
    let env = Env::new();
    std::fs::write(env.dir.path().join("blocker"), b"not a directory")
        .expect("write the blocking file");

    for (script, label) in [
        ("var a = ['x']; a.save('blocker/out.txt');", "text write"),
        (
            "var a = ['x']; a.saveStruct('blocker/out.bin', 'b');",
            "binary write",
        ),
    ] {
        let error = env
            .engine
            .exec_script(script, "streams-test")
            .expect_err(&format!("{label} below a regular file must raise"));
        assert!(
            error.to_string().contains("Cannot open storage"),
            "{label}: {error}"
        );
    }
}

// A short or empty struct stream leaves the 8-byte header partially unwritten;
// `Array/Dictionary.loadStruct` must treat it as non-binary input and return
// the typed invalid-parameter failure, never inspect the buffer
// (`tjsArray.cpp` / `tjsDictionary.cpp` loadStruct).
#[test]
fn load_struct_of_a_short_or_empty_stream_fails() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.mount_xp3(&[("short.bin", b"short", false)]);
    std::fs::write(env.dir.path().join("empty.bin"), b"").expect("empty fixture");
    for name in ["data.xp3>short.bin", "empty.bin"] {
        for script in [
            format!("var a = []; a.loadStruct('{name}');"),
            format!("Dictionary.loadStruct('{name}');"),
        ] {
            let error = env
                .engine
                .exec_script(&script, "streams-test")
                .expect_err("a short struct stream must not load");
            assert!(
                error.to_string().contains("Invalid argument"),
                "{script}: {error}"
            );
        }
    }

    // The full binary struct loads normally; reading it with an `oN` offset at
    // EOF has the full size but zero readable header bytes.
    env.run("var src = ['a', 'b']; src.saveStruct('struct.bin', 'b');");
    let size = std::fs::metadata(env.dir.path().join("struct.bin"))
        .expect("struct metadata")
        .len();
    env.run("var loaded = []; loaded.loadStruct('struct.bin');");
    assert_eq!(env.int("loaded.length"), 2);

    let error = env
        .engine
        .exec_script(
            &format!("var eof = []; eof.loadStruct('struct.bin', 'o{size}');"),
            "streams-test",
        )
        .expect_err("a struct read at EOF must fail");
    assert!(error.to_string().contains("Invalid argument"), "{error}");
}

// A BOM-less text stream decodes with the default read encoding (UTF-8); an
// even-length ASCII payload must not be mistaken for UTF-16LE.
#[test]
fn bomless_text_uses_the_default_utf8_encoding() {
    let _lock = vm_lock();
    let mut env = Env::new();
    let ascii = ";! header\r\n@clip top=1\r\n";
    assert_eq!(ascii.len() % 2, 0, "the ASCII fixture must be even-length");
    let japanese = ";! クリップ。\r\n";
    assert_eq!(
        japanese.len() % 2,
        0,
        "the UTF-8 fixture must be even-length"
    );
    env.mount_xp3(&[
        ("bomless-ascii.asd", ascii.as_bytes(), false),
        ("bomless-utf8.asd", japanese.as_bytes(), false),
    ]);

    env.run("var lines = []; lines.load('data.xp3>bomless-ascii.asd');");
    assert_eq!(env.int("lines.length"), 2);
    assert_eq!(env.string("lines[0]"), ";! header");
    assert_eq!(env.string("lines[1]"), "@clip top=1");

    env.run("var wide = []; wide.load('data.xp3>bomless-utf8.asd');");
    assert_eq!(env.string("wide[0]"), ";! クリップ。");
}

// The reference routes a media-prefixed write to that media's provider. The
// port has no write-capable media provider, so a media name is refused rather
// than written as a local file; `file:` stays the local media.
#[test]
fn writes_to_storage_media_names_raise() {
    let _lock = vm_lock();
    let env = Env::new();
    for script in [
        "var a = ['x']; a.save('psb://cache.psb/out.txt');",
        "var a = ['x']; a.saveStruct('psb://cache.psb/out.bin', 'b');",
        "var a = ['x']; a.save('var://store/out.txt');",
        "var a = ['x']; a.saveStruct('var://store/out.bin', 'b');",
    ] {
        let error = env
            .engine
            .exec_script(script, "streams-test")
            .expect_err("a storage-media write must raise");
        assert!(
            error.to_string().contains("Cannot open storage"),
            "{script}: {error}"
        );
    }
    assert!(!env.dir.path().join("psb:").exists());
    assert!(!env.dir.path().join("var:").exists());

    let name = format!(
        "file-{}-{}.txt",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after epoch")
            .as_nanos()
    );
    env.run(&format!("var a = ['local']; a.save('file://.{name}');"));
    assert!(
        env.dir.path().join(&name).is_file(),
        "the file media still writes under the game directory"
    );
}
