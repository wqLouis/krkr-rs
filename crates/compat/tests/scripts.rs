//! Integration tests for the `Scripts` native class.
//!
//! Raw `exec` / `eval`, `setCallMissing`, `getClassNames`, `getTraceString`,
//! `dump` and the `textEncoding` property are exercised against a bound VM.
//! The storage-backed members (`execStorage` / `evalStorage` /
//! `compileStorage`) are verified to require an attached L2 storage; their
//! end-to-end resolution is covered by the L2 `engine` tests.
//! `evalStorage` is also exercised against a mounted L2 storage: it
//! evaluates an expression-shaped storage and runs a script-shaped one.

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

    // The `script read` diagnostic names the script that was read, its byte
    // length and its encoding; it never carries the script's text.
    let read = logs
        .iter()
        .find(|line| line.contains("script read 'data/ok.tjs'"))
        .unwrap_or_else(|| panic!("no script read line in {logs:?}"));
    assert!(read.contains("bytes="), "{read}");
    assert!(read.contains("encoding="), "{read}");
    assert!(
        !read.contains("ok_value"),
        "the read diagnostic carries the script's text: {read}"
    );
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
// The bytecode round trip
// ---------------------------------------------------------------------------

/// The scripts that exercise the VM opcodes. Each one leaves its result in a
/// global the test reads back, so the result of the bytecode form can be
/// compared with the result of the source form.
///
/// At the top level of a script a `var` declares a member of `this` and not a
/// local variable: the local branch of the generator needs a namespace level
/// of at least one, and the top level is level zero
/// (`reference/cpp/core/tjs2/tjsInterCodeGen.cpp:2845-2883`). The opcodes of
/// the local variables are therefore reached from inside a function, where
/// the body is a block and the level is one.
///
/// The base opcode of an operator family is the compound assignment of a
/// local variable; `p_locals` reaches the base opcodes, and the entries
/// below reach the member variants.
///
/// The opcodes a script cannot reach are covered by the layout table itself:
/// it has an entry for every opcode, and the build fails when one is
/// missing. The unreachable ones are the class clear (`VM_CCL`) the generator
/// keeps disabled, the octet cast (`VM_OCTET`) the grammar has no operator
/// for, the hidden member set (`VM_SPDEH`) the generator does not emit and
/// the indirect set of a this-proxy (`VM_SPI`), which the grammar never
/// builds: a this-proxy is made for a plain name, and a plain name is a
/// direct access.
const OPCODE_SCRIPTS: &[(&str, &str)] = &[
    // VM_CONST, VM_SPDS, VM_GPD: a constant is stored in a member of `this`
    // and read back from it.
    ("p_const", "var a = 42; var p_const = a;"),
    // VM_GLOBAL, VM_SPDE, VM_CONST: the global object is named and a member
    // of it is written.
    ("p_global", "global.p_global = 7;"),
    // VM_ADDCI, VM_CL, VM_REGMEMBER: the class instance information.
    // VM_NEW: the instance is created. VM_GPD, VM_CALLD, VM_SRV, VM_RET: its
    // method is called.
    (
        "p_class",
        "class K { function m() { return 5; } } var k = new K(); var p_class = k.m();",
    ),
    // VM_NOP: the padding instruction at the end of the super class getter of
    // a class that extends another one.
    (
        "p_extends",
        "class PBase { } class PDerived extends PBase { } var p_extends = typeof PDerived;",
    ),
    // VM_CEQ, VM_SETF: the comparison and its result.
    ("p_ceq", "var p_ceq = (1 == 1);"),
    // VM_CDEQ, VM_SETF: the strict comparison and its result.
    ("p_cdeq", "var p_cdeq = (1 === 1);"),
    // VM_CLT, VM_SETF: the less-than comparison and its result.
    ("p_clt", "var p_clt = (1 < 2);"),
    // VM_CGT, VM_SETF: the greater-than comparison and its result.
    ("p_cgt", "var p_cgt = (2 > 1);"),
    // VM_CEQ, VM_SETNF: the inverted comparison result.
    ("p_setnf", "var p_setnf = (1 != 1);"),
    // VM_LNOT: the logical not.
    ("p_lnot", "var p_lnot = !0;"),
    // VM_JNF, VM_JMP, VM_CP: the conditional operator and the merge of its
    // two branches.
    ("p_cond", "var p_cond = (1 < 2) ? 3 : 4;"),
    // VM_TT, VM_JF, VM_NF, VM_SETF: the logical operator and the inverted
    // condition flag.
    ("p_nf", "var p_nf = (0 || (1 != 1));"),
    // VM_TF: the flag of a value that is not a constant of the script.
    ("p_tf", "var p_tf = (!0) ? 1 : 2;"),
    // VM_CP, VM_INC: the counter of the loop head is a local variable.
    // VM_CLT, VM_JNF, VM_JMP: the loop. VM_GPD, VM_ADD, VM_SPD: its body.
    (
        "p_loop",
        "var s = 0; for (var i = 0; i < 4; i++) { s = s + i; } var p_loop = s;",
    ),
    // VM_INCPD, VM_DECPD: the increment and the decrement of a member of
    // `this`.
    (
        "p_incdec",
        "var i = 1; i++; i--; ++i; --i; var p_incdec = i;",
    ),
    // VM_INCPD, VM_DECPD: the increment and the decrement of a member of an
    // object.
    (
        "p_incdec_pd",
        "var o = %[v: 1]; o.v++; o.v--; ++o.v; --o.v; var p_incdec_pd = o.v;",
    ),
    // VM_INCPI, VM_DECPI: the increment and the decrement of an indexed
    // member.
    (
        "p_incdec_pi",
        "var o = %[v: 1]; var k = 'v'; o[k]++; o[k]--; var p_incdec_pi = o.v;",
    ),
    // VM_CP, VM_CL, VM_INC, VM_DEC: a local variable is declared, counted up
    // and down and assigned. VM_ADD, VM_SUB, VM_MOD, VM_DIV, VM_IDIV, VM_MUL,
    // VM_BOR, VM_BXOR, VM_BAND, VM_SAR, VM_SAL, VM_SR, VM_LOR, VM_LAND: the
    // compound assignment of a local variable, which is the base opcode of
    // each operator family.
    (
        "p_locals",
        "function locals() { var a = 5; var b; a += 2; a -= 1; a *= 2; a %= 5; a /= 2; a \\= 2; a |= 1; a ^= 1; a &= 3; a >>= 1; a <<= 1; a >>>= 1; a ||= 4; a &&= 5; a++; a--; ++a; --a; b = a; return b; } var p_locals = locals();",
    ),
    // VM_ADDPD, VM_ADDPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_add",
        "var o = %[v: 4]; o.v += 2; var k = 'v'; o[k] += 1; var p_add = o.v;",
    ),
    // VM_SUBPD, VM_SUBPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_sub",
        "var o = %[v: 4]; o.v -= 2; var k = 'v'; o[k] -= 1; var p_sub = o.v;",
    ),
    // VM_MULPD, VM_MULPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_mul",
        "var o = %[v: 4]; o.v *= 2; var k = 'v'; o[k] *= 1; var p_mul = o.v;",
    ),
    // VM_DIVPD, VM_DIVPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_div",
        "var o = %[v: 4]; o.v /= 2; var k = 'v'; o[k] /= 1; var p_div = o.v;",
    ),
    // VM_IDIVPD, VM_IDIVPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_idiv",
        "var o = %[v: 4]; o.v \\= 2; var k = 'v'; o[k] \\= 1; var p_idiv = o.v;",
    ),
    // VM_MODPD, VM_MODPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_mod",
        "var o = %[v: 4]; o.v %= 3; var k = 'v'; o[k] %= 1; var p_mod = o.v;",
    ),
    // VM_LORPD, VM_LORPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_lor",
        "var o = %[v: 4]; o.v ||= 2; var k = 'v'; o[k] ||= 1; var p_lor = o.v;",
    ),
    // VM_LANDPD, VM_LANDPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_land",
        "var o = %[v: 4]; o.v &&= 2; var k = 'v'; o[k] &&= 1; var p_land = o.v;",
    ),
    // VM_BORPD, VM_BORPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_bor",
        "var o = %[v: 4]; o.v |= 2; var k = 'v'; o[k] |= 1; var p_bor = o.v;",
    ),
    // VM_BANDPD, VM_BANDPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_band",
        "var o = %[v: 4]; o.v &= 6; var k = 'v'; o[k] &= 1; var p_band = o.v;",
    ),
    // VM_BXORPD, VM_BXORPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_bxor",
        "var o = %[v: 4]; o.v ^= 2; var k = 'v'; o[k] ^= 1; var p_bxor = o.v;",
    ),
    // VM_SARPD, VM_SARPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_sar",
        "var o = %[v: 8]; o.v >>= 2; var k = 'v'; o[k] >>= 1; var p_sar = o.v;",
    ),
    // VM_SALPD, VM_SALPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_sal",
        "var o = %[v: 8]; o.v <<= 2; var k = 'v'; o[k] <<= 1; var p_sal = o.v;",
    ),
    // VM_SRPD, VM_SRPI: the compound assignment of a member and of an
    // indexed member.
    (
        "p_sr",
        "var o = %[v: 8]; o.v >>>= 2; var k = 'v'; o[k] >>>= 1; var p_sr = o.v;",
    ),
    // VM_BNOT: the bitwise not.
    ("p_bnot", "var p_bnot = ~5;"),
    // VM_ASC: the character code.
    ("p_asc", "var p_asc = #(\"A\");"),
    // VM_CHR: the character.
    ("p_chr", "var p_chr = $65;"),
    // VM_NUM: the numeric conversion.
    ("p_num", "var p_num = +\"12\";"),
    // VM_CHS: the sign change.
    ("p_chs", "var p_chs = -12;"),
    // VM_INV: the invalidation.
    ("p_inv", "var o = %[v: 1]; invalidate o; var p_inv = 1;"),
    // VM_CHKINV: the validity check.
    ("p_chkinv", "var o = %[v: 1]; var p_chkinv = isvalid o;"),
    // VM_TYPEOF: the type name of a value.
    ("p_typeof", "var p_typeof = typeof 1;"),
    // VM_TYPEOFD: the type name of a member.
    ("p_typeofd", "var o = %[v: 1]; var p_typeofd = typeof o.v;"),
    // VM_TYPEOFI: the type name of an indexed member.
    (
        "p_typeofi",
        "var o = %[v: 1]; var k = 'v'; var p_typeofi = typeof o[k];",
    ),
    // VM_EVAL: the evaluation of an expression.
    ("p_eval", "var p_eval = (\"1 + 1\")!;"),
    // VM_EEXP: the evaluation of an expression whose result is not used.
    ("p_eexp", "var p_eexp = 0; \"1 + 1\"!;"),
    // VM_INT: the integer cast.
    ("p_int", "var p_int = int \"12\";"),
    // VM_REAL: the real cast.
    ("p_real", "var p_real = real \"1.5\";"),
    // VM_STR: the string cast.
    ("p_str", "var p_str = string 12;"),
    // VM_CHKINS: the membership test.
    ("p_chkins", "var o = %[v: 1]; var p_chkins = (\"v\" in o);"),
    // VM_CALL: the call of a function value that a local variable names.
    (
        "p_call",
        "function f(a, b) { return a + b; } function call_it() { var g = f; return g(1, 2); } var p_call = call_it();",
    ),
    // VM_CALLD: the call of a member.
    (
        "p_calld",
        "var o = %[m: function(a) { return a + 1; }]; var p_calld = o.m(1);",
    ),
    // VM_CALLI: the call of an indexed member.
    (
        "p_calli",
        "var o = %[m: function(a) { return a + 1; }]; var k = 'm'; var p_calli = o[k](1);",
    ),
    // VM_CALLD: the call that forwards its arguments.
    (
        "p_call_omit",
        "function g(a, b) { return a + b; } function h() { return g(...); } var p_call_omit = h(1, 2);",
    ),
    // VM_GLOBAL, VM_NEW, VM_SPIS, VM_INC: the inline array.
    // VM_CALLD: the call that expands it into the arguments.
    (
        "p_call_expand",
        "function g(a, b) { return a + b; } var arr = [1, 2]; var p_call_expand = g(arr*);",
    ),
    // VM_GPD, VM_GPI: the property read. VM_SPDE, VM_SPIE: the property
    // write.
    (
        "p_prop",
        "var o = %[v: 1]; o.v = 2; o['v'] = 3; var p_prop = o.v + o['v'];",
    ),
    // VM_GPDS, VM_GPIS: the property access that ignores the property operation.
    (
        "p_ignoreprop",
        "var o = %[v: 1]; var d = &o.v; var e = &o['v']; var p_ignoreprop = 1;",
    ),
    // VM_GETP, VM_SETP: the property object access.
    (
        "p_propass",
        "var propvalue; property p1 { setter(v) { propvalue = v; } getter { return propvalue; } } p1 = 5; function proptest() { var pr = &p1; *pr = 30; return (*pr); } var p_propass = proptest();",
    ),
    // The property variants of the operator families: they are the base opcode
    // plus three, and only the property object access reaches them.
    // VM_ADDP, VM_INCP, VM_DECP and the other property variants.
    (
        "p_propvariant",
        "var propvalue; property p1 { setter(v) { propvalue = v; } getter { return propvalue; } } p1 = 5; function proptest() { var pr = &p1; *pr += 2; *pr -= 1; *pr *= 2; *pr %= 5; *pr /= 2; *pr \\= 2; *pr |= 1; *pr ^= 1; *pr &= 3; *pr >>= 1; *pr <<= 1; *pr >>>= 1; *pr ||= 4; *pr &&= 5; (*pr)++; (*pr)--; return (*pr); } var p_propvariant = proptest();",
    ),
    // VM_DELD: the deletion of a member.
    ("p_deld", "var o = %[v: 1]; delete o.v; var p_deld = 1;"),
    // VM_DELI: the deletion of an indexed member.
    (
        "p_deli",
        "var o = %[v: 1]; var k = 'v'; delete o[k]; var p_deli = 1;",
    ),
    // VM_ENTRY, VM_EXTRY: the try block.
    (
        "p_try",
        "var p_try = 0; try { p_try = 1; } catch (e) { p_try = 2; }",
    ),
    // VM_THROW: the throw.
    (
        "p_throw",
        "var p_throw = 0; try { throw 'x'; } catch (e) { p_throw = 1; }",
    ),
    // VM_CHGTHIS: the context change.
    (
        "p_chgthis",
        "var o = %[m: function() { return 1; }]; var p_chgthis = (o.m incontextof o)();",
    ),
    // VM_DEBUGGER: the debugger statement.
    ("p_debugger", "debugger; var p_debugger = 1;"),
];
/// Every script the opcode table names compiles to the reference's container
/// through `Scripts.compileStorage`, and `Scripts.execStorage` refuses that
/// container by name: the Rust executor has no bytecode loader.
///
/// The writer still has to accept every opcode the generator emits, so a
/// source form that fails to compile is a writer finding even though the
/// compiled form cannot run here.
#[test]
fn every_opcode_compiles_to_refused_bytecode() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    for (name, source) in OPCODE_SCRIPTS {
        let compiled = compile_fixture(
            &env,
            source,
            &format!("data/{name}_src.tjs"),
            &format!("data/{name}.tjsb"),
        );
        let bytes = std::fs::read(env.game_dir().unwrap().join(&compiled)).unwrap();
        assert_eq!(
            &bytes[..8],
            b"TJS2100\0",
            "{name}: the writer must emit the bytecode tag"
        );
        let error = env.error(&format!("Scripts.execStorage({compiled:?});"));
        assert!(error.contains("TJS2100"), "{name}: {error}");
        assert!(error.contains("unsupported"), "{name}: {error}");
    }
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
/// `Scripts.compileStorage` writes the reference's container, and
/// `Scripts.execStorage` refuses that container by name instead of decoding it
/// as text or running it through the vendored loader.
#[test]
fn exec_storage_refuses_compiled_bytecode() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let compiled = compile_fixture(
        &env,
        "var bytecode_ran = 42;",
        "data/bytecode_src.tjs",
        "data/bytecode.tjsb",
    );
    let bytes = std::fs::read(env.game_dir().unwrap().join(&compiled)).unwrap();
    // The writer emits the container's own tag.
    assert_eq!(&bytes[..8], b"TJS2100\0", "compiled bytecode tag");

    let error = env.error(&format!("Scripts.execStorage({compiled:?});"));
    assert!(
        error.contains(&compiled),
        "the refusal names the storage: {error}"
    );
    assert!(
        error.contains("TJS2100"),
        "the refusal names the tag: {error}"
    );
    assert!(
        error.contains("unsupported"),
        "the refusal names the decision: {error}"
    );
    assert_eq!(
        env.eval_ok("typeof global.bytecode_ran"),
        TjsValue::String("undefined".to_string())
    );
}

/// A refused compiled script has no text source to reprint: the diagnostic
/// names the bytecode path instead of dumping decoded nonsense, and the
/// refusal happens before any decode, so there is no source window.
#[test]
fn refused_bytecode_logs_no_source_excerpt() {
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

/// A storage without the compiled-script tag runs as text through the Rust
/// executor.
#[test]
fn exec_storage_still_runs_plain_text() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let dir = env.game_dir().unwrap();
    std::fs::write(dir.join("plain.tjs"), b"var text_path_ran = 7;").unwrap();
    env.run("Scripts.execStorage('plain.tjs');");
    assert_eq!(env.eval_ok("text_path_ran"), TjsValue::Integer(7));
}

/// Corrupt `TJS2100` data is refused by name, never decoded as text: the
/// Rust executor has no bytecode loader, so it does not inspect the container
/// it cannot run.
#[test]
fn corrupt_bytecode_is_refused_by_name() {
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
    assert!(message.contains("TJS2100"), "{message}");
    assert!(message.contains("unsupported"), "{message}");
}

/// A truncated `TJS2100` blob is refused by name, not decoded as text: the
/// Rust executor refuses the tag before it reads any header field.
#[test]
fn truncated_bytecode_is_refused_by_name() {
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
    assert!(message.contains("TJS2100"), "{message}");
    assert!(message.contains("unsupported"), "{message}");
}

/// A file whose header looks like bytecode is refused as bytecode and is not
/// run as text. The error names the tag and the missing loader, not the text
/// lexer, so the decision is observable.
#[test]
fn magic_prefixed_text_is_refused_as_bytecode_not_run_as_text() {
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
    assert!(message.contains("TJS2100"), "{message}");
    assert!(message.contains("unsupported"), "{message}");
}

/// A magic-prefixed blob shorter than the reference's fixed header is refused
/// by name, not read as a truncated container: the tag alone selects the
/// refusal.
#[test]
fn short_magic_prefixed_blob_is_refused_by_name() {
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
        assert!(message.contains("TJS2100"), "{name}: {message}");
        assert!(message.contains("unsupported"), "{name}: {message}");
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
fn eval_storage_evaluates_an_expression_shaped_storage() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let dir = env.game_dir().expect("the temp game directory");
    // The KAG's save data is a `(const) %[...]` literal and the KAG reads it
    // back through `Scripts.evalStorage`
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:1348-1350`).
    std::fs::write(dir.join("datasc.ksd"), "(const) %[\"chSpeed\" => 30]\n")
        .expect("write the storage");
    let value = env.eval_ok("Scripts.evalStorage('datasc.ksd')");
    assert!(
        matches!(value, TjsValue::Object),
        "the evaluated storage should be the dictionary, got {value:?}"
    );
}

/// `evalStorage` runs an expression once. A text that compiles as an
/// expression and then raises has already run, so its failure is the
/// storage's failure; the script mode is only for a text that fails to
/// compile as an expression.
///
/// Reference: `TVPExecuteStorage` calls `EvalExpression` once for an
/// expression-shaped storage (`reference/cpp/core/base/ScriptMgnIntf.cpp:803-807`).
#[test]
fn eval_storage_does_not_run_a_failing_expression_twice() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let dir = env.game_dir().expect("the temp game directory");
    std::fs::write(
        dir.join("once.tjs"),
        "(global.eval_storage_runs = global.eval_storage_runs + 1) \\ 0",
    )
    .expect("write the storage");
    env.run("global.eval_storage_runs = 0;");
    let error = env.error("Scripts.evalStorage('once.tjs');");
    assert!(!error.is_empty(), "the expression must raise");
    assert_eq!(
        env.eval_ok("global.eval_storage_runs"),
        TjsValue::Integer(1),
        "the expression must have run exactly once"
    );
}

#[test]
fn eval_storage_runs_a_script_shaped_storage() {
    let _lock = vm_lock();
    let env = common::Env::with_storage();
    let dir = env.game_dir().expect("the temp game directory");
    // A game loads its own class definitions through the same member
    // (`reference/cpp/core/base/ScriptMgnIntf.cpp:713-729`). The TJS2's
    // expression mode compiles the text as `return <text>;`
    // (`reference/cpp/core/tjs2/tjsLex.cpp:1676-1681`), so a script-shaped
    // storage — a class or a function definition — is a grammar error there
    // and has to run as a script.
    std::fs::write(
        dir.join("macro_SliderLayer.tjs"),
        "class SKN_Slider { var value; }\n",
    )
    .expect("write the storage");
    env.eval_ok("Scripts.evalStorage('macro_SliderLayer.tjs')");
    // The later script is a compatibility script, so it runs through the Rust
    // executor that shares the global store the declaration wrote.
    assert_eq!(
        env.eval_ok("Scripts.eval('typeof SKN_Slider')"),
        TjsValue::String("Object".into())
    );
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
