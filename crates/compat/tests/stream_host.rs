//! The stream host the Rust executor's save/load members use.
//!
//! The reference wires the four stream factories to the storage layer
//! (`reference/cpp/core/base/ScriptMgnIntf.cpp:464-469`), so a relative name
//! resolves through the game's project directory and an
//! `archive.xp3>member` name resolves through the mounted archive
//! (`reference/cpp/core/base/StorageIntf.cpp:1131-1148`). These checks run
//! the save/load members through the installed host and the real storage,
//! not through an in-memory table.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use compat::CompatHost;
use engine::Storage;
use fs::FileSystem;
use testkit::build_xp3;
use tjs::vm::runtime::bridge::GlobalMembers;
use tjs::vm::runtime::engine as runtime_engine;
use tjs::vm::runtime::stream;
use tjs::vm::runtime::string::TjsString;
use tjs::{Tjs2Engine, TjsValue};

/// The VM keeps the installed host; one check at a time.
static VM_LOCK: Mutex<()> = Mutex::new(());

fn vm_lock() -> MutexGuard<'static, ()> {
    VM_LOCK.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// A bound VM over a temporary game directory.
struct Env {
    // Drop order matters: the host removes the binding (and the stream host)
    // before the boxed storage is freed.
    _host: CompatHost,
    engine: Rc<Tjs2Engine>,
    globals: Rc<GlobalMembers>,
    storage: Box<Storage>,
    dir: PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = unique_dir("krkr-stream-host");
        std::fs::create_dir_all(&dir).expect("create game directory");
        let mut storage = Box::new(
            Storage::mount_archives(
                FileSystem::host(&dir),
                dir.clone(),
                std::iter::empty::<&str>(),
            )
            .expect("mount storage"),
        );
        let engine = Rc::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        // SAFETY: the boxed storage outlives the binding (it is declared after
        // the host and dropped after it).
        unsafe { host.services_mut().attach_storage(&mut *storage) };
        host.install(&engine).expect("install compat surface");
        let globals = Rc::new(GlobalMembers::new());
        Self {
            _host: host,
            engine,
            globals,
            storage,
            dir,
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.join(name)
    }

    fn mount_xp3(&mut self, name: &str, entries: &[(&str, &[u8], bool)]) {
        let path = self.path(name);
        std::fs::write(&path, build_xp3(entries)).expect("write archive");
        self.storage.mount_archive(path).expect("mount archive");
    }

    fn run(&self, script: &str) {
        runtime_engine::run_with_members(
            Rc::clone(&self.engine),
            script,
            "stream-host",
            Rc::clone(&self.globals),
        )
        .unwrap_or_else(|failure| panic!("script {script:?} failed: {failure:?}"));
    }

    fn failure(&self, script: &str) -> String {
        match runtime_engine::run_with_members(
            Rc::clone(&self.engine),
            script,
            "stream-host",
            Rc::clone(&self.globals),
        ) {
            Ok(_) => panic!("script {script:?} answered instead of raising"),
            Err(failure) => failure.message(),
        }
    }

    fn eval(&self, expression: &str) -> TjsValue {
        let answer = runtime_engine::eval_with_members(
            Rc::clone(&self.engine),
            expression,
            "stream-host",
            Rc::clone(&self.globals),
        )
        .unwrap_or_else(|failure| panic!("expression {expression:?} failed: {failure:?}"));
        runtime_engine::host_value(answer)
            .unwrap_or_else(|error| panic!("expression {expression:?} cannot answer: {error}"))
    }

    fn string(&self, expression: &str) -> String {
        match self.eval(expression) {
            TjsValue::String(text) => text,
            other => panic!("{expression:?} answered {other:?}, expected a string"),
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// A unique directory no other check of this binary uses.
fn unique_dir(prefix: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
}

/// The bytes of a text payload as the shipped archive member is encoded:
/// UTF-16LE with a byte-order mark.
fn utf16le_with_bom(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xFF, 0xFE];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

/// reference/cpp/core/base/ScriptMgnIntf.cpp:464-469 wires the stream
/// factories to the storage, so a save writes below the project directory
/// and the load reads the same name back.
#[test]
fn a_save_and_load_round_trip_through_the_host() {
    let _lock = vm_lock();
    let env = Env::new();
    assert!(
        stream::host().is_some(),
        "the composition root must install the stream host"
    );

    env.run("var a = (const)['hello', 7, 3.5]; a.save('savedata/sub/out.txt', '');");
    let written = env.path("savedata/sub/out.txt");
    let bytes = std::fs::read(&written).expect("read the save");
    // reference/cpp/core/base/TextStream.cpp writes the plain text form as the
    // UTF-16LE byte-order mark followed by the code units.
    assert_eq!(
        &bytes[..2],
        &[0xFF, 0xFE],
        "the text save has no UTF-16 BOM"
    );

    env.run("var b = (const)[]; b.load('savedata/sub/out.txt', '');");
    assert_eq!(env.string("b[0] + '|' + b[1] + '|' + b[2]"), "hello|7|3.5");

    // reference/cpp/core/tjs2/tjsArray.cpp:440-489: the structured binary
    // form goes through the binary stream factory and reads back the same
    // values.
    env.run("var c = (const)['x', 7]; c.saveStruct('savedata/sub/out.bin', 'b');");
    env.run("var d = (const)[]; d.loadStruct('savedata/sub/out.bin', 'b');");
    assert_eq!(env.string("d[0] + '|' + d[1]"), "x|7");

    // reference/cpp/core/base/TextStream.cpp:82-105: a `c` mode writes the
    // simple cipher and the reader reverses it.
    env.run("var e = (const)['cipher']; e.save('cipher.txt', 'c');");
    env.run("var f = (const)[]; f.load('cipher.txt', '');");
    assert_eq!(env.string("f[0]"), "cipher");
    let cipher = std::fs::read(env.path("cipher.txt")).expect("read the cipher save");
    assert_eq!(&cipher[..5], &[0xFE, 0xFE, 0x01, 0xFF, 0xFE]);

    // reference/cpp/core/base/TextStream.cpp:104-131: a `z` mode writes the
    // compressed container and the reader inflates it.
    env.run("var g = (const)['compressed']; g.save('zip.txt', 'z');");
    env.run("var h = (const)[]; h.load('zip.txt', '');");
    assert_eq!(env.string("h[0]"), "compressed");
    let zipped = std::fs::read(env.path("zip.txt")).expect("read the zlib save");
    assert_eq!(&zipped[..5], &[0xFE, 0xFE, 0x02, 0xFF, 0xFE]);
}

/// reference/cpp/core/base/StorageIntf.cpp:1131-1148 resolves an explicit
/// `archive.xp3>member` through the mounted archive, and
/// reference/cpp/core/base/TextStream.cpp:47-57 decodes its text.
#[test]
fn an_archive_member_loads_through_the_host() {
    let _lock = vm_lock();
    let mut env = Env::new();
    let asd = ";! clipleft=0\r\n@clip left=0 top=0\r\n";
    let text = utf16le_with_bom(asd);
    env.mount_xp3("data.xp3", &[("clickglyph.asd", &text, true)]);

    env.run("var lines = (const)[]; lines.load('data.xp3>clickglyph.asd', '');");
    assert_eq!(env.string("lines[0]"), ";! clipleft=0");
    assert_eq!(env.string("lines[1]"), "@clip left=0 top=0");
}

/// reference/cpp/core/base/ScriptMgnIntf.cpp:464-469 also wires the binary
/// read factory to the storage, so a structured member inside an archive
/// loads through the same mounted namespace.
#[test]
fn an_archive_binary_member_loads_through_the_host() {
    let _lock = vm_lock();
    let mut env = Env::new();
    env.run("var src = (const)['archive', 9]; src.saveStruct('source.bin', 'b');");
    let bytes = std::fs::read(env.path("source.bin")).expect("read the binary save");
    env.mount_xp3("data.xp3", &[("member.bin", &bytes, false)]);

    env.run("var loaded = (const)[]; loaded.loadStruct('data.xp3>member.bin', 'b');");
    assert_eq!(env.string("loaded[0] + '|' + loaded[1]"), "archive|9");
}

/// reference/cpp/core/base/StorageIntf.cpp:1131-1148 raises the open error
/// when neither the storage nor the local file system places the name.
#[test]
fn a_missing_name_answers_the_open_error() {
    let _lock = vm_lock();
    let env = Env::new();
    let error = match stream::open_text_read(
        &TjsString::from("@tjs-stream-host-absent"),
        &TjsString::new(),
    ) {
        Ok(_) => panic!("the absent name opened"),
        Err(error) => error,
    };
    assert_eq!(
        error.to_string(),
        "Cannot open storage @tjs-stream-host-absent"
    );

    // The member reaches the same host failure; the dispatch surface reports
    // its own refusal text, which is why the host error above is the one the
    // message contract names.
    assert!(
        !env.failure("var a = (const)[]; a.load('@tjs-stream-host-absent', '');")
            .is_empty(),
        "a missing name must raise"
    );
}

/// The archive delimiter is not a local path: a storage miss for an archive
/// member never falls back to a literal file.
#[test]
fn an_archive_member_miss_does_not_fall_back_to_a_literal_file() {
    let _lock = vm_lock();
    let env = Env::new();
    std::fs::write(env.path("data.xp3>literal.asd"), b"not an archive member")
        .expect("write the literal fixture");
    let error =
        match stream::open_text_read(&TjsString::from("data.xp3>literal.asd"), &TjsString::new()) {
            Ok(_) => panic!("the literal archive-name file opened"),
            Err(error) => error,
        };
    assert_eq!(
        error.to_string(),
        "Cannot open storage data.xp3>literal.asd"
    );
}

/// reference/cpp/core/base/TextStream.cpp:75-80 and
/// reference/cpp/core/base/BinaryStream.cpp:27-49 seek to the mode's `oN`
/// offset before reading; an offset past the end is the seek error.
#[test]
fn the_mode_offset_positions_both_streams() {
    let _lock = vm_lock();
    let env = Env::new();
    // A two-byte prefix then the UTF-16LE form: the `oN` offset starts the
    // reader at the payload's own byte-order mark.
    let mut text = vec![0xCA, 0xFE];
    text.extend_from_slice(&utf16le_with_bom("abcdef\r\n"));
    std::fs::write(env.path("offset.txt"), text).expect("write the offset fixture");
    env.run("var b = (const)[]; b.load('offset.txt', 'o2');");
    assert_eq!(env.string("b[0]"), "abcdef");

    env.run("var c = (const)['x', 7]; c.saveStruct('offset.bin', 'b');");
    let size = std::fs::metadata(env.path("offset.bin"))
        .expect("struct metadata")
        .len();
    // At the end the stream is empty, so the structured load finds no header.
    let at_end = env.failure(&format!(
        "var d = (const)[]; d.loadStruct('offset.bin', 'o{size}');"
    ));
    assert!(!at_end.is_empty(), "an offset at the end must not load");

    let past = match stream::open_text_read(
        &TjsString::from("offset.txt"),
        &TjsString::from(format!("o{}", size + 1000)),
    ) {
        Ok(_) => panic!("an offset past the end opened"),
        Err(error) => error,
    };
    assert_eq!(past.to_string(), "Seek error");
}

/// The runtime's own array object, saved and loaded directly through the
/// installed host and the attached storage.
#[test]
fn the_array_object_round_trips_through_the_installed_host() {
    let _lock = vm_lock();
    let env = Env::new();
    let array = tjs::vm::runtime::array::ArrayObject::new();
    array
        .append(tjs::vm::runtime::variant::Variant::from("host"))
        .expect("append the text");
    array
        .append(tjs::vm::runtime::variant::Variant::from(5i64))
        .expect("append the integer");
    array
        .save(&TjsString::from("direct.txt"), &TjsString::new())
        .expect("the installed host saves the array");
    assert!(
        env.path("direct.txt").is_file(),
        "the host wrote the save below the storage's project directory"
    );

    let loaded = tjs::vm::runtime::array::ArrayObject::new();
    loaded
        .load(&TjsString::from("direct.txt"), &TjsString::new())
        .expect("the installed host loads the array");
    let first = loaded
        .prop_get_by_num(0, tjs::vm::runtime::object::MemberFlags::NONE)
        .expect("read the first element");
    let second = loaded
        .prop_get_by_num(1, tjs::vm::runtime::object::MemberFlags::NONE)
        .expect("read the second element");
    assert_eq!(first.to_string().expect("a text").to_string(), "host");
    assert_eq!(
        second.to_integer().expect("an integer"),
        5,
        "the integer round trip"
    );
}
