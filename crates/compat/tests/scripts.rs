//! Integration tests for the `Scripts` native class.
//!
//! Raw `exec` / `eval`, `setCallMissing`, `getClassNames`, `getTraceString`,
//! `dump` and the `textEncoding` property are exercised against a bound VM.
//! The storage-backed members (`execStorage` / `evalStorage` /
//! `compileStorage`) are verified to require an attached L2 storage; their
//! end-to-end resolution is covered by the L2 `engine` tests.

mod common;

use common::vm_lock;
use compat::CompatHost;
use tjs::{Tjs2Engine, TjsValue};

thread_local! {
    static CAPTURED_LOGS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// A `log` sink that records every record on the calling thread.
struct CaptureLogger;

impl log::Log for CaptureLogger {
    fn enabled(&self, _metadata: &log::Metadata) -> bool {
        true
    }

    fn log(&self, record: &log::Record) {
        CAPTURED_LOGS.with(|logs| {
            logs.borrow_mut()
                .push(format!("{} {}", record.level(), record.args()));
        });
    }

    fn flush(&self) {}
}

/// Install the capturing sink once per test binary, clear this thread's
/// buffer, run `f`, and return its result plus the thread's log lines.
fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, Vec<String>) {
    static ONCE: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    ONCE.get_or_init(|| {
        let _ = log::set_boxed_logger(Box::new(CaptureLogger));
        log::set_max_level(log::LevelFilter::Debug);
    });
    CAPTURED_LOGS.with(|logs| logs.borrow_mut().clear());
    let result = f();
    let logs = CAPTURED_LOGS.with(|logs| logs.borrow().clone());
    (result, logs)
}

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
        common::link_all_builtins(&engine);
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

    // Reference: `Scripts.exec`/`Scripts.eval` compile and run the given text
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:1385-1438`).
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
fn eval_honours_the_callers_result_need() {
    let _lock = vm_lock();
    let env = Env::new();

    // A native method called as a bare statement receives a null `result`
    // (`reference/cpp/core/tjs2/tjsInterCodeExec.cpp:2124-2130` passes
    // `code[1] ? ... : nullptr`), so `Scripts.eval` compiles with
    // `MustReturnResult = false`
    // (`reference/cpp/core/tjs2/tjsScriptCache.cpp:163`). The `swap` operator
    // may then be used even though it produces no value
    // (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:1211-1216` raises only
    // when the result is required). The Conductor's tag dispatch reaches
    // `Scripts.eval` exactly this way, from `MainWindow.tjs`'s `eval` tag
    // handler.
    env.engine
        .exec_script(
            "var swap_left = 1; var swap_right = 2; \
             Scripts.eval('swap_left <-> swap_right');",
            "test",
        )
        .expect("a discarded Scripts.eval of a swap must not raise");
    assert_eq!(env.eval_ok("swap_left"), TjsValue::Integer(2));
    assert_eq!(env.eval_ok("swap_right"), TjsValue::Integer(1));

    // When the caller does need the value, the reference raises for a
    // resultless `swap`, and so does the port.
    let error = env.error("Scripts.eval('swap_left <-> swap_right')");
    assert!(
        error.contains("Cannot get the value of this expression"),
        "unexpected error: {error}"
    );
}

#[test]
fn eval_returns_a_callable_object() {
    let _lock = vm_lock();
    let env = Env::new();
    // The Action.tjs pattern: evaluate an action string and call the result
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:1412-1438`).
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

    // Reference: `Scripts.setCallMissing` sets the object's
    // `TJS_CII_SET_MISSING` info to the `missing` member
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:1474-1496`).
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
    // Reference: `Scripts.getClassNames` walks `TJS_CII_GET` class info into
    // an array, most-derived class first
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:1497-1535`).
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
    // Reference: `Scripts.getTraceString`/`Scripts.dump`
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:1440-1461`).
    assert_eq!(env.eval_ok("Scripts.dump()"), TjsValue::Void);
}

#[test]
fn text_encoding_property_round_trips_through_dispatch_state() {
    let _lock = vm_lock();
    let env = Env::new();

    // Reference: `Scripts.textEncoding` reads/writes
    // `TVPGetDefaultReadEncoding`/`TVPSetDefaultReadEncoding`
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:1537-1549`).
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
    // `TVPSetDefaultReadEncoding`,
    // `reference/cpp/core/base/TextStream.cpp:418-430`).
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
fn raw_exec_and_eval_honour_context_and_lineofs() {
    let _lock = vm_lock();
    let env = Env::new();
    env.engine.exec_script("var c = %[x: 5];", "t").unwrap();

    // While `outputErrorLogOnEval` is at its default `true`, `windowEx`'s
    // `Scripts.eval` override delegates to the core eval, so `context` (the
    // fourth argument) becomes the evaluation `this`
    // (`reference/cpp/plugins/windowEx.cpp:1542-1547`,
    // `reference/cpp/core/base/ScriptMgnIntf.cpp:1385-1438`).
    assert_eq!(
        env.eval_ok("Scripts.eval('this.x', '', 0, c)"),
        TjsValue::Integer(5)
    );
    assert_eq!(
        env.eval_ok("Scripts.exec('this.x = 9;', '', 0, c)"),
        TjsValue::Void
    );
    assert_eq!(env.eval_ok("c.x"), TjsValue::Integer(9));

    // `lineofs` (third argument) shifts the reported source line
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:1390-1396`).
    let line_of = |message: &str| -> i64 {
        let start = message.find("blk(").expect("the block name is reported") + 4;
        let end = message[start..]
            .find(')')
            .expect("the line number is closed")
            + start;
        message[start..end]
            .parse()
            .expect("the reported line is a number")
    };
    let base = env.error("Scripts.eval('1 +', 'blk', 0, null)");
    let shifted = env.error("Scripts.eval('1 +', 'blk', 10, null)");
    assert_eq!(line_of(&shifted) - line_of(&base), 10, "{shifted}");

    // A present, non-object context raises the variant conversion error
    // (`reference/cpp/core/tjs2/tjsVariant.h:694-699`).
    assert!(env.eval("Scripts.eval('1', null, null, 42)").is_err());
}

#[test]
fn stored_scripts_honour_context_and_use_the_storage_basename() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let dir = env.game_dir().expect("game dir");
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(dir.join("data/ctx.tjs"), b"this.x = 7;").unwrap();
    std::fs::write(dir.join("data/boom.tjs"), b"1 +").unwrap();

    // `execStorage`'s third argument is the script's `this`
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:1314-1354,704-811`).
    env.run("var ctx = %[]; Scripts.execStorage('data/ctx.tjs', '', ctx);");
    assert_eq!(env.eval_ok("ctx.x"), TjsValue::Integer(7));

    // The block name is `TVPExtractStorageName` of the placed path, not the
    // full storage name (`reference/cpp/core/base/ScriptMgnIntf.cpp:720,782`).
    let error = env.error("Scripts.execStorage('data/boom.tjs')");
    assert!(error.contains("boom.tjs("), "error: {error}");
    assert!(!error.contains("data/boom.tjs("), "error: {error}");
}

#[test]
fn failing_storage_script_logs_its_source_window() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let dir = env.game_dir().expect("game dir");
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(
        dir.join("data/fail.tjs"),
        b"// header\nvar x = 1;\nthrow \"boom\";\nvar y = 2;",
    )
    .unwrap();

    // Reference: a failing `TVPExecuteStorage` surfaces the script exception
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-811`); the debug
    // diagnostic reprints the decoded source around the reported line, with
    // line numbers, so the failure can be read from the log alone.
    let (result, logs) = capture_logs(|| env.error("Scripts.execStorage('data/fail.tjs')"));
    assert!(result.contains("fail.tjs("), "error: {result}");

    let failure = logs
        .iter()
        .find(|line| line.contains("script failure 'data/fail.tjs'"))
        .unwrap_or_else(|| panic!("no failure diagnostic in {logs:?}"));
    assert!(failure.contains("source lines 1-4 of 4:"), "{failure}");
    assert!(failure.contains("     1| // header"), "{failure}");
    assert!(failure.contains("     3| throw \"boom\";"), "{failure}");
}

#[test]
fn successful_storage_script_logs_no_source() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let dir = env.game_dir().expect("game dir");
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(dir.join("data/ok.tjs"), b"var ok_value = 7;").unwrap();

    // A successful `TVPExecuteStorage` is not a failure diagnostic: the debug
    // log keeps the `script read` line and prints no source
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-811`).
    let (_, logs) = capture_logs(|| env.run("Scripts.execStorage('data/ok.tjs');"));
    assert_eq!(env.eval_ok("ok_value"), TjsValue::Integer(7));
    assert!(
        !logs.iter().any(|line| line.contains("script failure")),
        "unexpected failure diagnostic: {logs:?}"
    );
    assert!(
        !logs.iter().any(|line| line.contains("source lines")),
        "unexpected source dump: {logs:?}"
    );

    // The `script read` diagnostic is unchanged: name, byte length, hex head,
    // encoding decision and NUL counts.
    let read = logs
        .iter()
        .find(|line| line.contains("script read 'data/ok.tjs'"))
        .unwrap_or_else(|| panic!("no script read line in {logs:?}"));
    assert!(read.contains("bytes="), "{read}");
    assert!(read.contains("head32=["), "{read}");
    assert!(read.contains("encoding="), "{read}");
    assert!(read.contains("nul_bytes=0"), "{read}");
    assert!(read.contains("decoded_has_nul=false"), "{read}");
}

#[test]
fn failure_source_excerpt_keeps_the_failing_line_when_capped() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let dir = env.game_dir().expect("game dir");
    // Lines far larger than the cap: an excerpt that fills from the start of a
    // window would cut before the failure, so the excerpt must centre on the
    // reported line instead.
    let padding = "// ".to_string() + &"x".repeat(1200);
    let mut script = String::new();
    for _ in 0..59 {
        script.push_str(&padding);
        script.push('\n');
    }
    script.push_str("throw \"boom\";\n");
    script.push_str(&padding);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::write(dir.join("data/big.tjs"), &script).unwrap();

    // A huge script cannot flood the log, but the cap must never hide the line
    // it is reporting: the excerpt centres on the failure, cuts the lines
    // before it, and states both the cutoff and the omitted count. The cap is
    // this diagnostic's own bound, so it asserts no reference behaviour; the
    // failure it reports still comes from `execStorage`
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-811`).
    let (result, logs) = capture_logs(|| env.error("Scripts.execStorage('data/big.tjs')"));
    assert!(result.contains("big.tjs("), "error: {result}");
    let failure = logs
        .iter()
        .find(|line| line.contains("script failure 'data/big.tjs'"))
        .unwrap_or_else(|| panic!("no failure diagnostic in {logs:?}"));
    // The failing line is present and identified, and the earliest lines were
    // the ones cut.
    assert!(failure.contains("    60| throw \"boom\";"), "{failure}");
    assert!(!failure.contains("     1| // "), "{failure}");
    assert!(failure.contains("source lines 55-61 of 61:"), "{failure}");
    assert!(
        failure.contains("excerpt truncated at 8192 bytes"),
        "{failure}"
    );
    assert!(failure.contains("more line(s) not shown"), "{failure}");
}

// ---------------------------------------------------------------------------
// Precompiled bytecode
// ---------------------------------------------------------------------------

/// Compile `source` (written at `source_name`) to `compiled_name` with the
/// VM's own `Scripts.compileStorage`, and return the compiled storage name.
fn compile_fixture(
    env: &common::Env,
    source: &str,
    source_name: &str,
    compiled_name: &str,
) -> String {
    let dir = env.game_dir().expect("game dir").clone();
    let source_path = dir.join(source_name);
    if let Some(parent) = source_path.parent() {
        std::fs::create_dir_all(parent).expect("create source dir");
    }
    std::fs::write(&source_path, source).expect("write source");
    let output_path = dir.join(compiled_name);
    env.run(&format!(
        "Scripts.compileStorage({source_name:?}, {:?});",
        output_path.to_string_lossy()
    ));
    assert!(
        output_path.is_file(),
        "compileStorage did not write {compiled_name}"
    );
    compiled_name.to_string()
}

/// A compiled script loads and executes. `Scripts.compileStorage` is the
/// VM's bytecode writer (`reference/cpp/core/base/ScriptMgnIntf.cpp:810-845`)
/// and `Scripts.execStorage` calls the same VM's `LoadByteCode` before it
/// opens the text stream
/// (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-727`).
///
/// This is a **self-consistency** round trip: both sides are this port's copy
/// of the same VM, so it proves the loader wiring, not that a `.tjsb`
/// produced by the original KiriKiri (or another compiler revision) loads.
/// That needs a device run with a real title's bytecode file (D36).
///
/// The fixture uses a plain top-level `var` (a `VM_GLOBAL` + `VM_SPDS` pair),
/// which the port's bytecode writer round-trips. Property-access compound
/// assignment (`+=`, `VM_ADDPD`) is a **writer** gap in this reconstruction:
/// the writer's export address table registers only the base opcode, so the
/// variant's operands stay byte offsets and the loader multiplies them again
/// (see `docs.old/audits/bytecode-scripts.md`). The assertion below still fails on
/// a wrong context: a top-level `var` needs the global as its context, and a
/// null/wrong one makes the `VM_SPDS` object-operand conversion raise.
#[test]
fn exec_storage_executes_compiled_bytecode() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let compiled = compile_fixture(
        &env,
        "var bytecode_ran = 42;",
        "data/bytecode_src.tjs",
        "data/bytecode.tjsb",
    );
    let bytes = std::fs::read(env.game_dir().unwrap().join(&compiled)).unwrap();
    // The writer emits the TJS2 bytecode tag `TJS2100\0`
    // (`TJS::tTJSByteCodeLoader::FILE_TAG_LE`/`VER_TAG_LE`), the signature
    // the loader checks (`reference/cpp/core/tjs2/tjsByteCodeLoader.cpp:40-47`).
    assert_eq!(&bytes[..8], b"TJS2100\0", "compiled bytecode tag");

    env.run(&format!("Scripts.execStorage({compiled:?});"));
    assert_eq!(env.eval_ok("bytecode_ran"), TjsValue::Integer(42));
}

/// A precompiled script that fails at run time has no text source to reprint:
/// the diagnostic names the bytecode path instead of dumping decoded
/// nonsense. `execStorage` offers the bytes to the loader
/// (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-727`), and the loader
/// executes them, so the failure travels back through the bytecode branch.
#[test]
fn failing_bytecode_script_logs_no_source_excerpt() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let compiled = compile_fixture(
        &env,
        "throw \"bytecode boom\";",
        "data/bytecode_boom_src.tjs",
        "data/bytecode_boom.tjsb",
    );
    let bytes = std::fs::read(env.game_dir().unwrap().join(&compiled)).unwrap();
    assert_eq!(&bytes[..8], b"TJS2100\0", "compiled bytecode tag");

    let (result, logs) = capture_logs(|| env.error(&format!("Scripts.execStorage({compiled:?})")));
    assert!(!result.is_empty(), "expected a bytecode failure");
    let failure = logs
        .iter()
        .find(|line| line.contains("script failure"))
        .unwrap_or_else(|| panic!("no failure diagnostic in {logs:?}"));
    assert!(failure.contains("precompiled bytecode"), "{failure}");
    assert!(failure.contains("no source to show"), "{failure}");
    assert!(!failure.contains("source lines"), "{failure}");
}

/// A non-bytecode storage script still takes the text path: the loader
/// reports "not bytecode" and `TVPExecuteStorage` falls through to the text
/// stream (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-811`).
#[test]
fn exec_storage_still_runs_plain_text() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let dir = env.game_dir().unwrap();
    std::fs::write(dir.join("plain.tjs"), b"var text_path_ran = 7;").unwrap();
    env.run("Scripts.execStorage('plain.tjs');");
    assert_eq!(env.eval_ok("text_path_ran"), TjsValue::Integer(7));
}

/// Corrupt recognized bytecode is a typed error, never a silent fall-through
/// to the text decoder: the reference's `LoadByteCode` raises
/// `TJSByteCodeBroken` when the loader rejects data whose tag it recognized
/// (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-727`,
/// `reference/cpp/core/tjs2/tjsScriptCache.cpp:199-212`).
#[test]
fn corrupt_bytecode_is_a_typed_error() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let compiled = compile_fixture(
        &env,
        "var corrupt_probe = 1;",
        "data/corrupt_src.tjs",
        "data/corrupt.tjsb",
    );
    let path = env.game_dir().unwrap().join(&compiled);
    let mut bytes = std::fs::read(&path).unwrap();
    // Flip the DATA tag at offset 12 while keeping the `TJS2100` header, so
    // the loader recognizes bytecode and then rejects it. The full length is
    // kept, so the loader's size check stays in bounds.
    // (`reference/cpp/core/tjs2/tjsByteCodeLoader.cpp:53-56`)
    bytes[12] ^= 0xFF;
    std::fs::write(&path, &bytes).unwrap();

    let message = env.error(&format!("Scripts.execStorage({compiled:?})"));
    assert!(
        message.contains("Bytecode read error"),
        "expected the bytecode loader's error, got: {message}"
    );
}

/// A truncated bytecode blob is a typed error, not a fall-through: the
/// loader reads the declared size from the header and rejects a mismatch
/// (`reference/cpp/core/tjs2/tjsByteCodeLoader.cpp:49-51`).
#[test]
fn truncated_bytecode_is_a_typed_error() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let compiled = compile_fixture(
        &env,
        "var trunc_probe = 1;",
        "data/trunc_src.tjs",
        "data/trunc.tjsb",
    );
    let path = env.game_dir().unwrap().join(&compiled);
    let mut bytes = std::fs::read(&path).unwrap();
    // Keep the header (tag, version, declared size) and cut the payload: the
    // declared size no longer matches the buffer, which the loader rejects
    // before it touches the data area.
    bytes.truncate(bytes.len() / 2);
    std::fs::write(&path, &bytes).unwrap();

    let message = env.error(&format!("Scripts.execStorage({compiled:?})"));
    assert!(
        message.contains("Bytecode read error"),
        "expected the bytecode loader's error, got: {message}"
    );
}

/// The loader is consulted before the text decoder, so a file whose header
/// looks like bytecode is not run as text when the loader rejects it: the
/// reference branches on the `LoadByteCode` result
/// (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-727`). The error names the
/// bytecode loader, not the text lexer, so the order is observable.
#[test]
fn magic_prefixed_text_is_rejected_by_the_loader_not_run_as_text() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let dir = env.game_dir().unwrap();
    // `TJS2100\0` (the bytecode tag) followed by a bogus declared size and
    // text: the loader recognizes bytecode and then rejects it.
    let mut bytes = b"TJS2100\0".to_vec();
    bytes.extend_from_slice(&0x7fff_ffffu32.to_le_bytes());
    bytes.extend_from_slice(b"this is text, not bytecode, but it is long enough");
    std::fs::write(dir.join("fake.tjsb"), &bytes).unwrap();

    let message = env.error("Scripts.execStorage('fake.tjsb')");
    assert!(
        message.contains("Bytecode read error"),
        "the bytecode loader must run first; got: {message}"
    );
}

/// A magic-prefixed blob shorter than the loader's fixed header is a typed
/// error, not an out-of-bounds read. `ReadByteCode` reads the file size at
/// offset 8, the `DATA` tag at 12 and the data-area size at 16
/// (`reference/cpp/core/tjs2/tjsByteCodeLoader.cpp:40-56`), so a recognized
/// magic below 20 bytes must be rejected before the loader touches it.
#[test]
fn short_magic_prefixed_blob_is_a_typed_error() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let dir = env.game_dir().unwrap();
    for (name, bytes) in [
        ("head8.tjsb", b"TJS2100\0".to_vec()),
        ("head11.tjsb", b"TJS2100\0\x01\x02\x03".to_vec()),
        ("head19.tjsb", {
            let mut bytes = b"TJS2100\0".to_vec();
            bytes.extend_from_slice(&[0u8; 11]);
            bytes
        }),
    ] {
        std::fs::write(dir.join(name), &bytes).unwrap();
        let message = env.error(&format!("Scripts.execStorage({name:?})"));
        assert!(
            message.contains("Bytecode read error"),
            "{name} ({} bytes) should be a typed error; got: {message}",
            bytes.len()
        );
    }
}

#[test]
fn storage_backed_members_require_an_attached_storage() {
    let _lock = vm_lock();
    let env = Env::new();
    // Reference: the storage-backed members resolve through
    // `TVPExecuteStorage` and require a storage
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:1314-1383`).
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
    // Reference: each native returns `TJS_E_BADPARAMCOUNT` when a required
    // argument is missing (`reference/cpp/core/base/ScriptMgnIntf.cpp:1314-1535`).
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
