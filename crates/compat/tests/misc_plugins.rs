//! Reference-behaviour tests for the plugin shims that have no dedicated
//! test file: `fft_graph`, `win32_dialog`, `dirlist`, `wutcwf` and
//! `simple_binder`. Every test carries the `reference/cpp/**:line` citation
//! for the behaviour it pins.
//!
//! * `fft_graph` (`reference/cpp/plugins/fftgraph.cpp:5-7`) executes
//!   `function drawFFTGraph(){}`, so the port must expose a replaceable,
//!   no-op global.
//! * `win32_dialog` (`reference/cpp/plugins/win32dialog.cpp:6-10`) defines
//!   `WIN32Dialog.messageBox(message, caption, type)` as
//!   `!System.inform(message, caption, 2)`; per D18 the port emulates it
//!   rather than raising "unsupported", looks `System.inform` up through the
//!   object on every call (script override wins) and forwards `message` /
//!   `caption` unchanged (`void` when omitted).
//! * `dirlist` (`reference/cpp/plugins/dirlist.cpp:13-57`) registers the
//!   global `getDirList(dir)` with the trailing-`/` rule and the
//!   file-and-directory listing of `TVPGetLocalFileListAt`.
//! * `wutcwf` (`reference/cpp/plugins/wutcwf.cpp:12-21`) registers a TCWF wave
//!   decoder; the port installs the decoder on the sound seam, so the test
//!   drives it end to end through `WaveSoundBuffer.open`.
//! * `simple_binder` (`reference/cpp/plugins/simplebinder/readme.txt:4-9`,
//!   `simplebinder.hpp:1-3`) is a header-only helper and registers no script
//!   surface at all.

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
            "krkr-compat-misc-plugins-{}-{}",
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

    fn write(&self, name: &str, bytes: &[u8]) {
        std::fs::write(self.0.join(name), bytes).expect("write fixture");
    }
}

impl Drop for GameDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A bound VM + host + attached storage.
///
/// The host is declared before the boxed engine/storage so its `Drop` removes
/// the process-wide binding before the VM and storage it points at are freed.
struct Env {
    _host: CompatHost,
    engine: Box<Tjs2Engine>,
    _storage: Box<Storage>,
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
        let engine = Box::new(Tjs2Engine::new().expect("create engine"));
        let mut host = CompatHost::new();
        // SAFETY: `storage` is boxed and outlives the binding, which `_host`
        // removes on drop.
        unsafe { host.services_mut().attach_storage(&mut *storage) };
        host.install(&engine).expect("install compat surface");
        Env {
            _host: host,
            engine,
            _storage: storage,
            dir,
        }
    }

    fn write(&self, name: &str, bytes: &[u8]) {
        self.dir.write(name, bytes);
    }

    /// Create a subdirectory of the game dir (for the `getDirList` fixture).
    fn mkdir(&self, name: &str) {
        std::fs::create_dir_all(self.dir.path().join(name)).expect("create fixture dir");
    }

    fn run(&self, script: &str) {
        self.engine
            .exec_script(script, "misc-plugins-test")
            .unwrap_or_else(|error| panic!("exec {script:?} failed: {error}"));
    }

    fn eval(&self, expression: &str) -> Result<TjsValue, String> {
        self.engine
            .eval(expression, "misc-plugins-test")
            .map_err(|error| error.to_string())
    }

    fn eval_ok(&self, expression: &str) -> TjsValue {
        self.eval(expression)
            .unwrap_or_else(|error| panic!("eval {expression:?} failed: {error}"))
    }

    fn eval_int(&self, expression: &str) -> i64 {
        match self.eval_ok(expression) {
            TjsValue::Integer(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected an integer"),
        }
    }

    fn eval_str(&self, expression: &str) -> String {
        match self.eval_ok(expression) {
            TjsValue::String(value) => value,
            other => panic!("{expression:?} returned {other:?}, expected a string"),
        }
    }

    fn error(&self, expression: &str) -> String {
        self.eval(expression)
            .expect_err(&format!("{expression:?} should have raised"))
    }
}

// ---------------------------------------------------------------------------
// `fft_graph`
// ---------------------------------------------------------------------------

/// Reference: the whole plugin is
/// `TVPExecuteScript("function drawFFTGraph(){}")`
/// (`reference/cpp/plugins/fftgraph.cpp:5-7`) — a *global* zero-parameter
/// function whose body is empty, so calling it (with or without arguments;
/// TJS accepts extra arguments) returns `void`, and because it is an ordinary
/// script member a game or `util_graph.dll` may reassign it afterwards.
/// `typeof` of any function value is `"Object"`
/// (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2614-2616`).
#[test]
fn fft_graph_defines_the_reference_noop_function() {
    let _lock = vm_lock();
    let env = Env::new();

    // A global function, not a missing member.
    assert_eq!(env.eval_str("typeof drawFFTGraph"), "Object");
    // The reference body is empty → void, and extra arguments are accepted.
    assert_eq!(env.eval_ok("drawFFTGraph()"), TjsValue::Void);
    assert_eq!(env.eval_ok("drawFFTGraph(1, 'x', null)"), TjsValue::Void);

    // The reference stub is meant to be replaced by `util_graph.dll`/the
    // game, so reassignment must keep working.
    env.run("drawFFTGraph = function(value) { return value * 2; };");
    assert_eq!(env.eval_int("drawFFTGraph(21)"), 42);
}

// ---------------------------------------------------------------------------
// `win32_dialog`
// ---------------------------------------------------------------------------

/// Reference: the script class body is
/// `function messageBox(message, caption, type) { return !System.inform(message, caption, 2); }`
/// (`reference/cpp/plugins/win32dialog.cpp:6-10`). The emulated
/// `System.inform` reports button `0` whenever a `buttons` argument is
/// present (`natives/system.rs`, mirroring
/// `reference/cpp/core/base/impl/SystemImpl.cpp:594-596`, which stores the
/// clicked-button index), so the negation is Integer `1` — `!` is the
/// reference `operator bool` (`tjsVariant.h:911-932`). `type` is ignored:
/// the body always passes the literal `2` (`win32dialog.cpp:9`).
#[test]
fn win32_dialog_message_box_emulates_system_inform() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run("var dialog = new WIN32Dialog();");

    // A function member: `typeof` reports `Object`
    // (tjsInterCodeExec.cpp:2614-2616).
    assert_eq!(env.eval_str("typeof dialog.messageBox"), "Object");

    // Reference body: `return !System.inform(message, caption, 2)`. The
    // emulated `System.inform` reports button 0, so the negation is `1`.
    assert_eq!(env.eval_int("dialog.messageBox('hello', 'caption', 0)"), 1);
    // The result is the reference script `!` → Integer, never Real/void.
    assert_eq!(
        env.eval_str("typeof dialog.messageBox('hello', 'caption', 0)"),
        "Integer"
    );
    // Missing arguments are still passed through as the reference does
    // (as `void` — pinned below through the override).
    assert_eq!(env.eval_int("dialog.messageBox()"), 1);
    // D18: the Win32 member emulates, it never raises "unsupported".
    assert!(
        env.eval("dialog.messageBox('x', 'y', 3)").is_ok(),
        "messageBox must emulate the dialog, not refuse it"
    );
    // `type` is ignored — the body always passes the literal 2
    // (win32dialog.cpp:9), so any `type` value gives the same result.
    assert_eq!(env.eval_int("dialog.messageBox('x', 'y', 9999)"), 1);
}

/// Reference: the body looks `System.inform` up **on the `System` object at
/// call time** (`win32dialog.cpp:8-9`) and passes `message`/`caption`
/// through unchanged — a call that omits them forwards the script
/// parameters' `void`, and the third argument is always the literal `2`,
/// never `type`. So a script override of `System.inform` wins, observes the
/// raw argument types, and its return value is negated by the reference `!`
/// (`tjsVariant.h:911-932`: non-zero Integer `7` → `0`, void → `1`).
#[test]
fn message_box_dispatches_through_system_and_forwards_raw_arguments() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var seen = ''; \
         System.inform = function(m, c, t) { \
             seen = typeof m + '|' + typeof c + '|' + t; return 7; }; \
         var dialog = new WIN32Dialog();",
    );

    // Override wins: the result is `!7` = Integer 0, not the emulated 1.
    assert_eq!(env.eval_int("dialog.messageBox('hello', 'cap', 5)"), 0);
    assert_eq!(
        env.eval_str("seen"),
        "String|String|2",
        "message/caption must pass through and `type` must be replaced by the literal 2"
    );

    // Omitted arguments arrive as `void`, exactly like the reference script
    // class' parameters (win32dialog.cpp:7-9); an override returning `void`
    // is negated to Integer 1 (`tjsVariant.h:913-914`: void is false).
    env.run(
        "System.inform = function(m, c, t) { \
             seen = typeof m + '|' + typeof c + '|' + t; };",
    );
    assert_eq!(env.eval_int("dialog.messageBox()"), 1);
    assert_eq!(env.eval_str("seen"), "void|void|2");
}

/// Reference: the body passes `message` unchanged to `System.inform`
/// (`reference/cpp/plugins/win32dialog.cpp:8-9`), so an object argument
/// reaches `System.inform` as the very same object — the reference resolves
/// the parameter as a `tTJSVariant` (the object itself, not a copy or a
/// string). A port that forwarded a released handle would fail the identity
/// check below.
#[test]
fn message_box_forwards_object_arguments_by_identity() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "var arr = [1, 2]; \
         var seen = false; \
         System.inform = function(m, c, t) { seen = (m === arr); return 0; }; \
         var dialog = new WIN32Dialog();",
    );

    // `!0` = Integer 1, and the override saw the identical array object.
    assert_eq!(env.eval_int("dialog.messageBox(arr, 'cap', 0)"), 1);
    assert_eq!(env.eval_int("seen"), 1, "the object identity was lost");
}

/// Reference: an exception thrown inside `System.inform` propagates out of
/// the script body (`win32dialog.cpp:8-9` executes the `!`-expression on the
/// call stack), so `messageBox` reports a typed failure — never a fabricated
/// button result (D11: no silent zero).
#[test]
fn message_box_reports_an_override_that_throws_as_a_typed_error() {
    let _lock = vm_lock();
    let env = Env::new();
    env.run(
        "System.inform = function() { throw 'boom'; }; \
         var dialog = new WIN32Dialog();",
    );

    let message = env.error("dialog.messageBox('a', 'b', 0)");
    assert!(
        message.contains("boom") || message.contains("inform"),
        "unexpected message: {message}"
    );
}

/// Reference: the reference declares a *script* class
/// (`win32dialog.cpp:7`), so an instance-level assignment shadows
/// `messageBox` per object and the shadow wins on the next call. The port's
/// native class behaves identically because the override is found through
/// the object before the class member.
#[test]
fn instance_level_override_of_message_box_wins() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run(
        "var plain = new WIN32Dialog(); \
         var custom = new WIN32Dialog(); \
         custom.messageBox = function() { return 123; };",
    );
    assert_eq!(env.eval_int("custom.messageBox('x')"), 123);
    // The other instance keeps the reference body.
    assert_eq!(env.eval_int("plain.messageBox('x')"), 1);
}

/// Reference: `class WIN32Dialog { ... }` (`win32dialog.cpp:7`) is an
/// ordinary instantiable script class with no constructor — any arguments
/// are accepted and distinct instances stay distinct.
#[test]
fn win32_dialog_is_an_instance_class() {
    let _lock = vm_lock();
    let env = Env::new();

    // The reference declares a script class, so it is instantiable and the
    // constructor accepts any arguments.
    env.run("var first = new WIN32Dialog(); var second = new WIN32Dialog(1, 2, 3);");
    assert_eq!(env.eval_int("first !== second"), 1);
}

// ---------------------------------------------------------------------------
// `dirlist`
// ---------------------------------------------------------------------------

/// Join an array's elements with `|` so order-independent membership checks
/// can assert on a substring.
fn joined(env: &Env, array: &str) -> String {
    let expression = format!(
        "(function(){{ var a = {array}; var s = ''; \
         for (var i = 0; i < a.length; i++) s += a[i] + '|'; \
         return s; }})()"
    );
    env.eval_str(&expression)
}

/// Reference: `getDirList(dir)` returns an `Array` object
/// (`reference/cpp/plugins/dirlist.cpp:33,46`) filled from
/// `TVPGetLocalFileListAt`, keeping every entry whose mode is a regular file
/// **or a directory** (`dirlist.cpp:41-45`) — the immediate child names,
/// directories without a trailing `/`.
#[test]
fn get_dir_list_lists_immediate_children_as_an_array() {
    let _lock = vm_lock();
    let env = Env::new();
    env.mkdir("data");
    env.write("data/a.txt", b"a");
    env.write("data/b.txt", b"b");
    env.mkdir("data/sub");

    // The result is the Array class object (dirlist.cpp:33). `instanceof`
    // takes a class-name string; `instanceof Array` negates to false because
    // the native class object has no string conversion for the right operand
    // (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2685-2691`).
    assert!(
        env.eval_int("getDirList('data/') instanceof 'Array'") != 0,
        "getDirList must return an Array object"
    );
    assert_eq!(env.eval_int("getDirList('data/').length"), 3);
    let names = joined(&env, "getDirList('data/')");
    assert!(
        names.contains("a.txt|"),
        "getDirList missing a.txt: {names}"
    );
    assert!(
        names.contains("b.txt|"),
        "getDirList missing b.txt: {names}"
    );
    // Directories are listed without a trailing slash (dirlist.cpp:42-43).
    assert!(names.contains("sub|"), "getDirList missing sub: {names}");
}

/// Reference: `if(dir.GetLastChar() != TJS_W('/')) TVPThrowExceptionMessage(
/// "'/' must be specified at the end of given directory name.")`
/// (`reference/cpp/plugins/dirlist.cpp:25-27`) — a typed, catchable
/// exception, never a silent empty list.
#[test]
fn get_dir_list_requires_the_trailing_slash() {
    let _lock = vm_lock();
    let env = Env::new();
    env.mkdir("data");
    env.write("data/a.txt", b"a");

    let message = env.error("getDirList('data')");
    assert!(
        message.contains("'/' must be specified at the end of given directory name."),
        "unexpected message: {message}"
    );
}

/// Reference: `TVPGetLocalFileListAt` lowercases every listed name before the
/// callback sees it (`reference/cpp/core/base/impl/StorageImpl.cpp:234-241`),
/// and `getDirList` copies those names verbatim into the result
/// (`reference/cpp/plugins/dirlist.cpp:41-45`). The fstat member does not
/// lowercase (`reference/cpp/plugins/fstat/main.cpp:526`), so the wrapper
/// lowercases after the delegated call.
#[test]
fn get_dir_list_lowercases_mixed_case_names() {
    let _lock = vm_lock();
    let env = Env::new();
    env.mkdir("data");
    env.write("data/MixedCase.TXT", b"x");

    let names = joined(&env, "getDirList('data/')");
    assert!(
        names.contains("mixedcase.txt|"),
        "getDirList must lowercase names (StorageImpl.cpp:234-241): {names}"
    );
    assert!(
        !names.contains("MixedCase.TXT"),
        "getDirList returned the original case: {names}"
    );
}

/// Reference: `getDirList` requires at least one argument —
/// `if(numparams < 1) return TJS_E_BADPARAMCOUNT;`
/// (`reference/cpp/plugins/dirlist.cpp:20-21`). A call without the directory
/// raises instead of listing something arbitrary.
#[test]
fn get_dir_list_without_an_argument_is_a_typed_error() {
    let _lock = vm_lock();
    let env = Env::new();

    assert!(env.eval("getDirList()").is_err());
}

/// Reference: `dirlist.cpp` converts the argument through `TVPGetLocalName`
/// before listing (`dirlist.cpp:40`). An absolute POSIX path has no locally
/// accessible form, so `GetLocallyAccessibleName` yields an empty name and
/// `TVPGetLocalName` throws `TVPCannotGetLocalName`
/// (`reference/cpp/core/base/impl/StorageImpl.cpp:301-413`, message
/// `"Cannot get local name from %1"`,
/// `reference/cpp/core/base/MsgIntfInc.h:35`) — before
/// `TVPGetLocalFileListAt` can silently produce the empty array of
/// `dirlist.cpp:41-50`. The fstat twin's `_dirlist` has the same guard
/// (`reference/cpp/plugins/fstat/main.cpp:509-512`).
#[test]
fn get_dir_list_unlistable_directory_is_a_typed_error() {
    let _lock = vm_lock();
    let env = Env::new();

    let message = env.error("getDirList('/no-such-directory/')");
    assert!(
        message.contains("Cannot get local name"),
        "unexpected message: {message}"
    );
}

/// Reference: `getDirList` is an ordinary member on the global object,
/// installed with `PropSet(TJS_MEMBERENSURE, "getDirList", ...)`
/// (`reference/cpp/plugins/dirlist.cpp:80-87`) — a script may reassign the
/// global afterwards, and the new binding wins.
#[test]
fn get_dir_list_is_an_ordinary_reassignable_global() {
    let _lock = vm_lock();
    let env = Env::new();

    env.run("getDirList = function(dir) { return 'replaced'; };");
    assert_eq!(env.eval_str("getDirList('data/')"), "replaced");
}

// ---------------------------------------------------------------------------
// `wutcwf`
// ---------------------------------------------------------------------------

/// Build a minimal mono TCWF stream: one block with the two ADPCM seed
/// samples and two zero nibbles. Decodes to `[100, 200, 200, 200]`.
fn tcwf_fixture() -> Vec<u8> {
    const MARK: [u8; 6] = *b"TCWF0\x1a";
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&MARK);
    bytes.push(1); // channels
    bytes.push(0); // reserved
    bytes.extend_from_slice(&22_050i32.to_le_bytes()); // frequency
    bytes.extend_from_slice(&1i32.to_le_bytes()); // numblocks
    bytes.extend_from_slice(&34i32.to_le_bytes()); // bytesperblock = 32 + 2 nibbles
    bytes.extend_from_slice(&4i32.to_le_bytes()); // samplesperblock
    // Block header.
    bytes.extend_from_slice(&100i16.to_le_bytes()); // ms_sample0
    bytes.extend_from_slice(&200i16.to_le_bytes()); // ms_sample1
    bytes.extend_from_slice(&16i16.to_le_bytes()); // ms_idelta
    bytes.push(0); // ms_bpred
    bytes.push(0); // ima_stepindex
    bytes.extend_from_slice(&[0u8; 24]); // six unexpected peaks, all zero
    bytes.extend_from_slice(&[0x00, 0x00]); // two nibble bytes
    bytes
}

#[test]
fn wutcwf_decodes_a_tcwf_stream_through_wave_sound_buffer() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write("tone.tcwf", &tcwf_fixture());

    env.run("var s = new WaveSoundBuffer(); s.open('tone.tcwf');");
    assert_eq!(env.eval_int("s.frequency"), 22_050);
    assert_eq!(env.eval_int("s.channels"), 1);
    assert_eq!(env.eval_int("s.bits"), 32);
    // The decoded source is loaded but not started, exactly like the
    // reference decoder after `open`.
    assert_eq!(env.eval_str("s.status"), "stop");
}

#[test]
fn wutcwf_reports_a_typed_error_for_a_non_tcwf_stream() {
    let _lock = vm_lock();
    let env = Env::new();
    env.write("plain.wav", b"RIFFnot-a-tcwf");
    env.run("var s = new WaveSoundBuffer();");

    let message = env.error("s.open('plain.wav')");
    assert!(
        message.contains("TCWF0"),
        "the decoder must name the missing signature, got: {message}"
    );
}

// ---------------------------------------------------------------------------
// `simple_binder` (and the absence of the helper-only globals)
// ---------------------------------------------------------------------------

#[test]
fn helper_plugins_register_no_script_surface() {
    let _lock = vm_lock();
    let env = Env::new();

    // `SimpleBinder` is a compile-time C++ helper and never registers a
    // global; `wutcwf` registers a decoder and no global class. Their Rust
    // ports must not invent script surfaces either.
    for name in ["SimpleBinder", "Wutcwf", "TCWFDecoder"] {
        assert_eq!(
            env.eval_str(&format!("typeof global.{name}")),
            "undefined",
            "{name} must not be registered"
        );
    }
}

/// Reference: `TVPGetLocalFileListAt` does nothing when `opendir`/the
/// directory probe fails (`reference/cpp/core/base/impl/StorageImpl.cpp:222-247`)
/// and `getDirList`'s `catch` rethrows only *other* exceptions
/// (`reference/cpp/plugins/dirlist.cpp:44-51`), so a missing directory yields
/// an empty array rather than the fstat member's `"Directory not found."`.
#[test]
fn get_dir_list_returns_empty_for_a_missing_directory() {
    let _lock = vm_lock();
    let env = Env::new();
    assert_eq!(env.eval_int("getDirList('missing/').length"), 0);
    assert_eq!(
        env.eval_str("typeof getDirList('missing/')"),
        "Object",
        "a missing directory still yields the Array object"
    );
}
